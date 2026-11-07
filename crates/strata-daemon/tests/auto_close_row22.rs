//! Auto-close §10 row 22 (r9): the session's own deps draft on an untracked
//! sibling holds the close, a main proposal there does not, and a saved
//! `sync` conflict report holds the close — including the agent's `rm`
//! way out that deletes the report. Temp-dir fixtures only.
mod support;

use std::fs;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use strata_daemon::handlers::{Server, Trackers};
use strata_daemon::startup::{Core, Daemon};
use strata_session::SessionSeam;
use strata_store::{Lifecycle, NewProposal};
use strata_types::{CommitOid, DepsSetId, MarkByKind, ProposalId, SessionId, TreeOid};
use support::close::{self, call, mark_of, ok};
use support::*;

/// `front` and `tool`, auto-close on, written before `prepare`.
fn siblings() -> Dfx {
    let mut fx = close::opted_in(&["front", "tool"]);
    fx.catch_up_every = Duration::from_secs(86400);
    fx.lock_wait = Duration::from_secs(60);
    fx.write_config();
    fx
}

fn entry<'a>(out: &'a Value, s: &SessionId) -> &'a Value {
    &out["auto_close"]["sessions"][s.as_str()]
}

fn lifecycle(core: &Core, s: &SessionId) -> Lifecycle {
    core.store.session(s).unwrap().unwrap().lifecycle
}

fn pin(core: &Core, s: &SessionId, repo: &str) -> bool {
    core.gits[&repo_name(repo)].read_ref(&format!("refs/strata/pins/{s}")).unwrap().is_some()
}

fn assert_kept_both(core: &Core, s: &SessionId) {
    assert_eq!(lifecycle(core, s), Lifecycle::Active);
    for repo in ["front", "tool"] {
        assert!(ws(core, s, repo).exists(), "{repo} workspace");
        assert!(pin(core, s, repo), "{repo} pin");
    }
}

fn listed(server: &Server<'_, '_>, p: &Value) -> bool {
    let l = ok(call(server, "deps_list", json!({ "repo": null })));
    l["proposals"].as_array().unwrap().iter().any(|q| q["id"] == *p)
}

fn assert_daemon_mark_names(core: &Core, s: &SessionId, p: &Value) {
    let m = mark_of(core, s).expect("a daemon mark");
    assert_eq!(m.by.kind, MarkByKind::Daemon);
    let note = serde_json::to_string(&m).unwrap();
    assert!(note.contains(&p.to_string()), "note names {p}: {note}");
}

/// Draft: `NEW_DEP` in S's `front`, `deps_propose`; Restore: `PACKAGE_JSON` back.
fn draft_and_restore(server: &Server<'_, '_>, core: &Core, s: &SessionId) -> Value {
    let root = ws(core, s, "front");
    assert!(!core.store.session(s).unwrap().unwrap().manifest.repos[&repo_name("front")].tracked);
    fs::write(root.join("package.json"), NEW_DEP).unwrap();
    let p = ok(call(server, "deps_propose", json!({ "session": s, "repo": "front", "switch": false })));
    assert_eq!(p["proposal"]["state"], json!({ "kind": "AwaitingPublish" }), "{p}");
    fs::write(root.join("package.json"), PACKAGE_JSON).unwrap();
    p["proposal"]["id"].clone()
}

/// The session's own draft written straight into the store, as row 18 does.
fn own_record(core: &Core, s: &SessionId, repo: &str) -> ProposalId {
    let rn = repo_name(repo);
    let w = core.store.session(s).unwrap().unwrap().manifest.repos[&rn].clone();
    let set = core.store.deps_set(&rn, &w.base.deps).unwrap().unwrap().set;
    core.store
        .record_proposal(
            &NewProposal {
                repo: rn,
                origin: Some(s.clone()),
                from: w.base.deps.clone(),
                from_inputs: set.inputs_hash,
                from_head: Some(w.head.clone()),
                to: DepsSetId::try_from("b".repeat(64)).unwrap(),
                to_inputs: "b".repeat(64),
                tree: None,
                head: None,
            },
            None,
            SystemTime::now(),
        )
        .unwrap()
        .unwrap()
}

fn start(fx: &Dfx) -> Core {
    fx.prepare().unwrap()
}

#[test]
fn own_draft_on_an_untracked_sibling_holds_the_close() {
    let fx = siblings();
    let core = start(&fx);
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_in(&server, &["tool"]);
    let p = draft_and_restore(&server, &core, &s);
    fs::write(ws(&core, &s, "tool").join("t.txt"), "t\n").unwrap();
    let out = ok(close::publish(&server, &core, &s, "tool", &["t.txt"]));
    assert_eq!(out["published"][0]["push"]["kind"], "Pushed", "{out}");
    assert_eq!(out["closed"], false, "{out}");
    let e = entry(&out, &s);
    assert_eq!(e["reasons"], json!(["deps_pending"]), "{out}");
    assert_eq!(e["detail"]["repo"], "front", "{out}");
    assert_eq!(e["detail"]["proposal"], p, "{out}");
    assert_eq!(e["detail"]["state"], "awaiting_publish", "{out}");
    assert_eq!(e["detail"]["own"], true, "{out}");
    assert_eq!(e["detail"]["tracked"], false, "{out}");
    assert_daemon_mark_names(&core, &s, &p);
    assert_kept_both(&core, &s);
    assert!(listed(&server, &p));
    // The way out: the agent's rm, with the Agent rule unchanged.
    ok(call(&server, "rm", json!({ "session": s, "discard": false })));
    assert_eq!(lifecycle(&core, &s), Lifecycle::Removed);
    assert!(!listed(&server, &p));
}

#[test]
fn own_draft_on_an_untracked_sibling_holds_a_rejudge() {
    let fx = siblings();
    let core = start(&fx);
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_in(&server, &["tool"]);
    ok(call(&server, "mark", json!({ "session": s, "state": "done", "agent": "codex" })));
    let p = draft_and_restore(&server, &core, &s);
    let t = close::new_only(&server, &["tool"]);
    fs::write(ws(&core, &t, "tool").join("t.txt"), "t\n").unwrap();
    let out = ok(close::publish(&server, &core, &t, "tool", &["t.txt"]));
    assert_eq!(out["closed"], true, "{out}");
    assert_eq!(out["auto_close"]["rejudged"], json!([s]), "{out}");
    let e = entry(&out, &s);
    assert_eq!(e["in_group"], false, "{out}");
    assert_eq!(e["reasons"], json!(["deps_pending"]), "{out}");
    assert_eq!(e["detail"]["tracked"], false, "{out}");
    assert_eq!(e["detail"]["proposal"], p, "{out}");
    assert_kept_both(&core, &s);
    assert!(listed(&server, &p));
}

/// S edits and checkpoints `tool`; the publish pauses at
/// `remove.after_recheck`, where `during` writes a draft; then release.
fn close_with_draft_at_recheck(
    d: &Daemon<'_>,
    server: &Server<'_, '_>,
    core: &Core,
    s: &SessionId,
    draft_repo: &str,
) -> (Value, ProposalId) {
    fs::write(ws(core, s, "tool").join("t.txt"), "t\n").unwrap();
    let cp = checkpoint(core, s, "tool");
    let g = group(vec![batch(s, "tool", cp.id, &["t.txt"])]);
    d.sessions.arm(SessionSeam::PauseAt { phase: "remove.after_recheck".into() });
    std::thread::scope(|sc| {
        let h = sc.spawn(|| call(server, "publish", json!({ "group": g, "shown": {} })));
        d.sessions.wait_paused();
        let pid = own_record(core, s, draft_repo);
        d.sessions.release();
        (ok(h.join().unwrap()), pid)
    })
}

#[test]
fn own_draft_on_an_untracked_sibling_during_the_close_refuses() {
    let fx = siblings();
    let core = start(&fx);
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_in(&server, &["tool"]);
    let (out, pid) = close_with_draft_at_recheck(&d, &server, &core, &s, "front");
    assert_eq!(out["closed"], false, "{out}");
    let e = entry(&out, &s);
    assert_eq!(e["reasons"], json!(["deps_pending"]), "{out}");
    assert_eq!(e["detail"]["repo"], "front", "{out}");
    assert_eq!(e["detail"]["proposal"], json!(pid), "{out}");
    assert_eq!(e["detail"]["tracked"], false, "{out}");
    assert_eq!(e["detail"]["after"], "checkpoint", "{out}");
    assert_eq!(mark_of(&core, &s).expect("mark").by.kind, MarkByKind::Daemon);
    assert_kept_both(&core, &s);
}

#[test]
fn own_draft_during_phase_b_refuses_the_close() {
    let fx = siblings();
    let core = start(&fx);
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_only(&server, &["tool"]);
    let (out, pid) = close_with_draft_at_recheck(&d, &server, &core, &s, "tool");
    assert_eq!(out["closed"], false, "{out}");
    let e = entry(&out, &s);
    assert_eq!(e["reasons"], json!(["deps_pending"]), "{out}");
    assert_eq!(e["detail"]["repo"], "tool", "{out}");
    assert_eq!(e["detail"]["proposal"], json!(pid), "{out}");
    assert_eq!(e["detail"]["own"], true, "{out}");
    assert_eq!(e["detail"]["tracked"], true, "{out}");
    assert_eq!(e["detail"]["after"], "checkpoint", "{out}");
    assert_eq!(mark_of(&core, &s).expect("mark").by.kind, MarkByKind::Daemon);
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);
    assert!(ws(&core, &s, "tool").exists());
    assert!(pin(&core, &s, "tool"));
}

#[test]
fn main_proposal_on_an_untracked_sibling_does_not_hold_the_close() {
    let fx = siblings();
    let core = start(&fx);
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_in(&server, &["tool"]);
    let rn = repo_name("front");
    let w = core.store.session(&s).unwrap().unwrap().manifest.repos[&rn].clone();
    let set = core.store.deps_set(&rn, &w.base.deps).unwrap().unwrap().set;
    let main = fx.main("front");
    let pid = core
        .store
        .record_proposal(
            &NewProposal {
                repo: rn,
                origin: None,
                from: w.base.deps.clone(),
                from_inputs: set.inputs_hash,
                from_head: Some(w.head.clone()),
                to: DepsSetId::try_from("b".repeat(64)).unwrap(),
                to_inputs: "b".repeat(64),
                tree: Some(TreeOid::try_from(fx.sb.tree_of(&main, "HEAD")).unwrap()),
                head: Some(CommitOid::try_from(fx.sb.head(&main)).unwrap()),
            },
            None,
            SystemTime::now(),
        )
        .unwrap()
        .unwrap();
    fs::write(ws(&core, &s, "tool").join("t.txt"), "t\n").unwrap();
    let out = ok(close::publish(&server, &core, &s, "tool", &["t.txt"]));
    assert_eq!(out["closed"], true, "{out}");
    assert_eq!(lifecycle(&core, &s), Lifecycle::Removed);
    assert!(listed(&server, &json!(pid)));
}

fn holder(dir: &std::path::Path) -> Child {
    let c = Command::new("/bin/sleep").arg("300").current_dir(dir).stdin(Stdio::null()).spawn().unwrap();
    std::thread::sleep(Duration::from_millis(200));
    c
}

fn publish_file(server: &Server<'_, '_>, core: &Core, s: &SessionId, path: &str, body: &str) -> Value {
    let p = ws(core, s, "tool").join(path);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
    ok(close::publish(server, core, s, "tool", &[path]))
}

/// §10 row 22 nit 1 through U's publish: S Active, held by a saved sync
/// conflict report (`reasons == ["conflicts"]`, `detail.report == true`).
/// Returns `(S, Conflict detail)` so the resolve path can bind the original
/// target, and so the agent-rm variant can leave from the refusal.
fn hold_with_saved_sync_conflict(
    server: &Server<'_, '_>,
    core: &Core,
) -> (SessionId, Value) {
    let s = close::new(server);
    let mut h = holder(&ws(core, &s, "tool"));
    let out = publish_file(server, core, &s, "src/p.ts", "v1\n");
    assert_eq!(out["published"][0]["push"]["kind"], "Pushed", "{out}");
    let rep = &out["published"][0]["rebases"][0]["report"];
    assert_eq!(rep["kind"], "Deferred", "{out}");
    assert_eq!(rep["reason"], "open_files", "{out}");
    assert_eq!(entry(&out, &s)["reasons"], json!(["open_files"]), "{out}");
    assert_eq!(mark_of(core, &s).unwrap().by.kind, MarkByKind::Daemon);

    let t = close::new(server);
    let out = publish_file(server, core, &t, "src/p.ts", "v2\n");
    assert_eq!(out["closed"], true, "{out}");
    let _ = h.kill();
    let _ = h.wait();

    let r = call(server, "sync", json!({ "session": s, "repos": ["tool"], "fetch": false }));
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!(r["error"]["code"], "Conflict", "{r}");
    let det = r["error"]["detail"].clone();
    assert_eq!(det["reason"], "sync_conflict", "{r}");
    let tool = repo_name("tool");
    assert!(!core.store.sync_report(&s, &tool).unwrap().is_empty());
    assert!(core.store.session(&s).unwrap().unwrap().workspaces[&tool].conflicts.is_empty());

    let u = close::new_only(server, &["tool"]);
    let out = publish_file(server, core, &u, "src/u.ts", "u\n");
    let e = entry(&out, &s);
    assert_eq!(e["closed"], false, "{out}");
    assert_eq!(e["reasons"], json!(["conflicts"]), "{out}");
    assert_eq!(e["detail"]["repo"], "tool", "{out}");
    assert_eq!(e["detail"]["report"], true, "{out}");
    assert_eq!(e["detail"]["conflicts"], json!(["src/p.ts"]), "{out}");
    assert_eq!(lifecycle(core, &s), Lifecycle::Active);
    assert!(!core.store.sync_report(&s, &tool).unwrap().is_empty());
    (s, det)
}

fn sync_conflict_fixture() -> (Dfx, Core) {
    let mut fx = close::opted_in(&["tool"]);
    fx.catch_up_every = Duration::from_secs(86400);
    fx.write_config();
    let core = start(&fx);
    (fx, core)
}

#[test]
fn saved_sync_conflict_holds_the_close() {
    let (_fx, core) = sync_conflict_fixture();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let (s, det) = hold_with_saved_sync_conflict(&server, &core);
    let tool = repo_name("tool");

    fs::write(ws(&core, &s, "tool").join("src/p.ts"), "v2\n").unwrap();
    let resolve = |det: &Value| {
        call(
            &server,
            "sync",
            json!({ "session": s, "repos": ["tool"], "fetch": false, "resolved": ["src/p.ts"],
                    "target": det["target"]["head"], "generation": det["generation"] }),
        )
    };
    // U's publish moved main past that Conflict's target, so the binding
    // answers HeadMoved; against the new target S's v2 no longer conflicts
    // (`conflicts == []`, the saved report replaced by none) and a plain
    // `sync` moves. (Slice R fixture note; do not "fix" production.)
    let mut r = resolve(&det);
    if r["error"]["code"] == "HeadMoved" {
        assert_eq!(r["error"]["detail"]["conflicts"], json!([]), "{r}");
        r = call(&server, "sync", json!({ "session": s, "repos": ["tool"], "fetch": false }));
    }
    let synced = ok(r);
    assert_eq!(synced["repos"]["tool"]["result"], "moved", "{synced}");
    assert!(core.store.sync_report(&s, &tool).unwrap().is_empty());

    let v = close::new_only(&server, &["tool"]);
    let out = publish_file(&server, &core, &v, "src/v.ts", "v\n");
    assert_eq!(entry(&out, &s)["closed"], true, "{out}");
    assert_eq!(lifecycle(&core, &s), Lifecycle::Removed);
}

/// §10 row 22 nit 1 variant from the refusal: the agent's `rm {discard: false}`
/// passes and `sessions.rs:683` deletes the saved sync conflict report.
#[test]
fn saved_sync_conflict_agent_rm_passes_and_deletes_the_report() {
    let (_fx, core) = sync_conflict_fixture();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let (s, _det) = hold_with_saved_sync_conflict(&server, &core);
    let tool = repo_name("tool");
    assert!(!core.store.sync_report(&s, &tool).unwrap().is_empty());

    ok(call(&server, "rm", json!({ "session": s, "discard": false })));
    assert_eq!(lifecycle(&core, &s), Lifecycle::Removed);
    assert!(core.store.sync_report(&s, &tool).unwrap().is_empty());
}
