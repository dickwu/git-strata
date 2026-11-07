//! Auto-close §10 row 23 (r10): a stale deps draft writer that commits after
//! its origin left for removal is refused, one that commits before refuses
//! the close, and originless proposals are untouched. Temp-dir fixtures only.
mod support;

use std::fs;
use std::sync::mpsc;
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use strata_daemon::handlers::{Server, Trackers};
use strata_daemon::startup::{Core, Daemon};
use strata_session::SessionSeam;
use strata_store::{Lifecycle, NewProposal};
use strata_types::{DepsSetId, MarkByKind, SessionId};
use support::close::{self, call, mark_of, ok};
use support::*;

const WAIT: Duration = Duration::from_secs(30);

fn siblings() -> Dfx {
    let mut fx = close::opted_in(&["front", "tool"]);
    fx.catch_up_every = Duration::from_secs(86400);
    fx.reconcile_every = Duration::from_secs(86400);
    fx.lock_wait = Duration::from_secs(60);
    fx.write_config();
    fx
}

fn lifecycle(core: &Core, s: &SessionId) -> Lifecycle {
    core.store.session(s).unwrap().unwrap().lifecycle
}

fn pin(core: &Core, s: &SessionId, repo: &str) -> bool {
    core.gits[&repo_name(repo)].read_ref(&format!("refs/strata/pins/{s}")).unwrap().is_some()
}

fn own_rows(core: &Core, s: &SessionId) -> usize {
    core.store.proposals(None, false).unwrap().iter().filter(|p| p.origin.as_ref() == Some(s)).count()
}

fn listed(server: &Server<'_, '_>, id: &Value) -> Option<Value> {
    let l = ok(call(server, "deps_list", json!({ "repo": null })));
    l["proposals"].as_array().unwrap().iter().find(|q| q["id"] == *id).cloned()
}

fn entry<'a>(out: &'a Value, s: &SessionId) -> &'a Value {
    &out["auto_close"]["sessions"][s.as_str()]
}

struct Release<'a, 'b>(&'a strata_session::Sessions<'b>);
impl Drop for Release<'_, '_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// "The writer": arms `after_build` with a hook that signals `built` and then
/// waits on `go`, then runs Draft's `deps_propose` on this thread.
fn writer(server: &Server<'_, '_>, s: &SessionId, repo: &str, switch: bool, built: mpsc::Sender<()>, go: mpsc::Receiver<()>) -> Value {
    strata_deps::test_seams::after_build(Box::new(move || {
        built.send(()).unwrap();
        go.recv_timeout(Duration::from_secs(120)).unwrap();
    }));
    let out = call(server, "deps_propose", json!({ "session": s, "repo": repo, "switch": switch }));
    strata_deps::test_seams::reset();
    out
}

/// S edits `tool` and checkpoints; the publish group for it.
fn edit_and_group(core: &Core, s: &SessionId) -> strata_types::ReleaseGroup {
    fs::write(ws(core, s, "tool").join("t.txt"), "t\n").unwrap();
    let cp = checkpoint(core, s, "tool");
    group(vec![batch(s, "tool", cp.id, &["t.txt"])])
}

fn stale_after_removing(repo: &str, switch: bool) {
    let fx = siblings();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_in(&server, &["tool"]);
    let tracked = core.store.session(&s).unwrap().unwrap().manifest.repos[&repo_name(repo)].tracked;
    assert_eq!(tracked, repo == "tool");
    let root = ws(&core, &s, repo);
    let head_before = fs::read_to_string(root.join("package.json")).unwrap();
    fs::write(root.join("package.json"), NEW_DEP).unwrap();
    let (built_tx, built_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel();
    let (draft, out) = std::thread::scope(|sc| {
        let w = sc.spawn(|| writer(&server, &s, repo, switch, built_tx, go_rx));
        built_rx.recv_timeout(WAIT).expect("the writer reached after_build");
        // Restore.
        fs::write(root.join("package.json"), PACKAGE_JSON).unwrap();
        let g = edit_and_group(&core, &s);
        d.sessions.arm(SessionSeam::PauseAt { phase: "remove.unpin".into() });
        let srv = &server;
        let p = sc.spawn(move || call(srv, "publish", json!({ "group": g, "shown": {} })));
        let _release = Release(&d.sessions);
        assert!(d.sessions.wait_paused_timeout(WAIT), "remove.unpin was not reached");
        assert_eq!(lifecycle(&core, &s), Lifecycle::Removing);
        go_tx.send(()).unwrap();
        let draft = w.join().unwrap();
        assert_eq!(own_rows(&core, &s), 0, "a draft row was recorded after Removing: {draft}");
        d.sessions.release();
        (draft, p.join().unwrap())
    });
    assert_eq!(draft["ok"], false, "{draft}");
    assert_eq!(draft["error"]["code"], "InvalidInput", "{draft}");
    assert_eq!(draft["error"]["detail"]["reason"], "lifecycle", "{draft}");
    assert_eq!(draft["error"]["detail"]["lifecycle"], "removing", "{draft}");
    let out = ok(out);
    assert_eq!(out["closed"], true, "{out}");
    assert_eq!(lifecycle(&core, &s), Lifecycle::Removed);
    assert_eq!(own_rows(&core, &s), 0);
    if switch {
        // No move ran: the session's tree is gone with the close, and the
        // main checkout's manifest is untouched.
        assert_eq!(fs::read_to_string(fx.main(repo).join("package.json")).unwrap(), head_before);
    }
}

#[test]
fn stale_draft_after_removing_is_refused() {
    stale_after_removing("front", false);
}

#[test]
fn stale_draft_after_removing_is_refused_tracked() {
    stale_after_removing("tool", false);
}

#[test]
fn stale_draft_after_removing_is_refused_with_switch() {
    stale_after_removing("front", true);
}

#[test]
fn stale_draft_after_removed_is_no_such_session() {
    let fx = siblings();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_in(&server, &["tool"]);
    let root = ws(&core, &s, "front");
    fs::write(root.join("package.json"), NEW_DEP).unwrap();
    let (built_tx, built_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel();
    let (draft, out) = std::thread::scope(|sc| {
        let w = sc.spawn(|| writer(&server, &s, "front", false, built_tx, go_rx));
        built_rx.recv_timeout(WAIT).expect("the writer reached after_build");
        fs::write(root.join("package.json"), PACKAGE_JSON).unwrap();
        let g = edit_and_group(&core, &s);
        let out = ok(call(&server, "publish", json!({ "group": g, "shown": {} })));
        assert_eq!(out["closed"], true, "{out}");
        assert_eq!(lifecycle(&core, &s), Lifecycle::Removed);
        go_tx.send(()).unwrap();
        (w.join().unwrap(), out)
    });
    assert_eq!(draft["ok"], false, "{draft}");
    assert_eq!(draft["error"]["code"], "NoSuchSession", "{draft} / {out}");
    assert_eq!(own_rows(&core, &s), 0);
}

#[test]
fn stale_draft_before_removing_refuses_the_close() {
    let fx = siblings();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_in(&server, &["tool"]);
    let root = ws(&core, &s, "front");
    fs::write(root.join("package.json"), NEW_DEP).unwrap();
    let (built_tx, built_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel();
    let (draft, out) = std::thread::scope(|sc| {
        let w = sc.spawn(|| writer(&server, &s, "front", false, built_tx, go_rx));
        built_rx.recv_timeout(WAIT).expect("the writer reached after_build");
        fs::write(root.join("package.json"), PACKAGE_JSON).unwrap();
        let g = edit_and_group(&core, &s);
        d.sessions.arm(SessionSeam::PauseAt { phase: "remove.after_recheck".into() });
        let srv = &server;
        let p = sc.spawn(move || call(srv, "publish", json!({ "group": g, "shown": {} })));
        let _release = Release(&d.sessions);
        assert!(d.sessions.wait_paused_timeout(WAIT), "remove.after_recheck was not reached");
        go_tx.send(()).unwrap();
        let draft = w.join().unwrap();
        d.sessions.release();
        (draft, p.join().unwrap())
    });
    let draft = ok(draft);
    assert_eq!(draft["proposal"]["state"], json!({ "kind": "AwaitingPublish" }), "{draft}");
    let id = draft["proposal"]["id"].clone();
    let out = ok(out);
    assert_eq!(out["closed"], false, "{out}");
    let e = entry(&out, &s);
    assert_eq!(e["reasons"], json!(["deps_pending"]), "{out}");
    assert_eq!(e["detail"]["repo"], "front", "{out}");
    assert_eq!(e["detail"]["own"], true, "{out}");
    assert_eq!(e["detail"]["tracked"], false, "{out}");
    assert_eq!(e["detail"]["state"], "awaiting_publish", "{out}");
    assert_eq!(e["detail"]["after"], "checkpoint", "{out}");
    assert_eq!(mark_of(&core, &s).expect("a daemon mark").by.kind, MarkByKind::Daemon);
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);
    for repo in ["front", "tool"] {
        assert!(ws(&core, &s, repo).exists(), "{repo} workspace");
        assert!(pin(&core, &s, repo), "{repo} pin");
    }
    assert!(listed(&server, &id).is_some(), "deps_list lists the draft");
}

#[test]
fn main_proposal_after_removing_stays_listed() {
    let fx = siblings();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_only(&server, &["tool"]);
    let rn = repo_name("tool");
    let w = core.store.session(&s).unwrap().unwrap().manifest.repos[&rn].clone();
    let set = core.store.deps_set(&rn, &w.base.deps).unwrap().unwrap().set;
    let main = fx.main("tool");
    let g = edit_and_group(&core, &s);
    let (pid, out) = std::thread::scope(|sc| {
        d.sessions.arm(SessionSeam::PauseAt { phase: "remove.unpin".into() });
        let srv = &server;
        let p = sc.spawn(move || call(srv, "publish", json!({ "group": g, "shown": {} })));
        let _release = Release(&d.sessions);
        assert!(d.sessions.wait_paused_timeout(WAIT), "remove.unpin was not reached");
        assert_eq!(lifecycle(&core, &s), Lifecycle::Removing);
        let pid = core
            .store
            .record_proposal(
                &NewProposal {
                    repo: rn.clone(),
                    origin: None,
                    from: w.base.deps.clone(),
                    from_inputs: set.inputs_hash.clone(),
                    from_head: Some(w.head.clone()),
                    to: DepsSetId::try_from("b".repeat(64)).unwrap(),
                    to_inputs: "b".repeat(64),
                    tree: Some(strata_types::TreeOid::try_from(fx.sb.tree_of(&main, "HEAD")).unwrap()),
                    head: Some(strata_types::CommitOid::try_from(fx.sb.head(&main)).unwrap()),
                },
                None,
                SystemTime::now(),
            )
            .unwrap();
        assert!(pid.is_some(), "an originless proposal is inserted while S is Removing");
        d.sessions.release();
        (pid.unwrap(), p.join().unwrap())
    });
    let out = ok(out);
    assert_eq!(out["closed"], true, "{out}");
    assert_eq!(lifecycle(&core, &s), Lifecycle::Removed);
    let row = listed(&server, &json!(pid)).expect("deps_list lists the main proposal");
    assert!(row["state"].to_string().contains("Pending"), "{row}");
}
