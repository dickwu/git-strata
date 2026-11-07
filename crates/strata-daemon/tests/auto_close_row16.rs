//! Auto-close §10 row 16: enumeration is only a candidate snapshot; marks and
//! lifecycle are reread under the session queue. All repositories are Dfx temps.
mod support;

use std::fs;
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};
use strata_daemon::handlers::{Server, Trackers};
use strata_daemon::startup::{Core, Daemon};
use strata_session::SessionSeam;
use strata_store::Lifecycle;
use strata_types::{MarkByKind, MarkState, SessionId};
use support::close::{self, audit_lines, call, mark_of, ok};
use support::*;

fn fixture(push: bool) -> Dfx {
    let mut fx = close::opted_in(&["tool"]);
    fx.catch_up_every = Duration::from_secs(86400);
    let rc = fx.config.repos.get_mut(&repo_name("tool")).unwrap();
    rc.publish.push = push;
    // These races concern queue ownership, not dependency installation.
    rc.deps_dirs.clear();
    rc.seed_caches.clear();
    rc.install_inputs.clear();
    rc.install = None;
    fx.write_config();
    fx
}

fn entry<'a>(out: &'a Value, s: &SessionId) -> &'a Value {
    &out["auto_close"]["sessions"][s.as_str()]
}

fn audits(fx: &Dfx, s: &SessionId) -> Vec<Value> {
    audit_lines(fx)
        .into_iter()
        .filter(|l| l["event"] == "auto_close" && l["session"] == s.as_str())
        .collect()
}

fn assert_live(core: &Core, s: &SessionId) {
    let rec = core.store.session(s).unwrap().unwrap();
    assert_eq!(rec.lifecycle, Lifecycle::Active);
    assert!(rec.manifest.root.is_dir());
    assert!(
        core.gits[&repo_name("tool")]
            .read_ref(&format!("refs/strata/pins/{s}"))
            .unwrap()
            .is_some()
    );
}

fn publish_file(server: &Server<'_, '_>, core: &Core, s: &SessionId, name: &str) -> Value {
    fs::write(ws(core, s, "tool").join(name), format!("{name}\n")).unwrap();
    ok(close::publish(server, core, s, "tool", &[name]))
}

fn not_pushed(server: &Server<'_, '_>, core: &Core) -> SessionId {
    let s = close::new(server);
    let out = publish_file(server, core, &s, "s.txt");
    assert_eq!(out["closed"], false, "{out}");
    assert_eq!(entry(&out, &s)["reasons"], json!(["not_pushed"]), "{out}");
    let mark = mark_of(core, &s).expect("refusal must create a done mark");
    assert_eq!(mark.state, MarkState::Done);
    assert_eq!(mark.by.kind, MarkByKind::Daemon);
    s
}

/// The competing handler must finish *before* release: this proves the hook
/// holds no queue at enumeration/before_judge. Release even on a timeout so a
/// regression fails rather than stranding scoped threads behind the pause.
fn at_pause(
    d: &Daemon<'_>,
    phase: &str,
    hook: impl FnOnce() -> Value + Send,
    competing: impl FnOnce() -> Value + Send,
) -> (Value, Value) {
    d.sessions.arm(SessionSeam::PauseAt {
        phase: phase.into(),
    });
    std::thread::scope(|scope| {
        let hook = scope.spawn(hook);
        d.sessions.wait_paused();
        let (tx, rx) = mpsc::channel();
        let other = scope.spawn(move || tx.send(competing()).unwrap());
        let completed = rx.recv_timeout(Duration::from_secs(10));
        d.sessions.release();
        let out = hook.join().unwrap();
        other.join().unwrap();
        (
            ok(out),
            ok(completed.expect("competing handler must succeed while hook is paused")),
        )
    })
}

#[test]
fn push_rereads_the_done_mark_under_the_queue() {
    let fx = fixture(false);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = not_pushed(&server, &core);
    let before = audits(&fx, &s);

    let (out, _) = at_pause(
        &d,
        "auto_close.after_enumerate",
        || call(&server, "push", json!({"repo": "tool"})),
        || call(&server, "mark", json!({"session": s, "state": "active"})),
    );
    assert_eq!(out["push"]["kind"], "Pushed", "{out}");
    assert!(out.get("closed").is_none(), "{out}");
    assert!(out.get("auto_close").is_none(), "{out}");
    assert_live(&core, &s);
    assert!(
        mark_of(&core, &s).is_none(),
        "hook must not resurrect a cleared mark"
    );
    assert_eq!(audits(&fx, &s), before, "dropped candidate is not audited");
}

#[test]
fn publish_set_b_rereads_the_done_mark_under_the_queue() {
    let mut fx = fixture(false);
    let core = fx
        .prepare_with(fx.confirmer(vec![strata_confirm::seams::Verdict::pass()]).0)
        .unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = not_pushed(&server, &core);
    let before = audits(&fx, &s);
    fx.config
        .repos
        .get_mut(&repo_name("tool"))
        .unwrap()
        .publish
        .push = true;
    fx.write_config();
    assert_eq!(core.reload_for_test().outcome, "applied");
    // Push is a pinned safety option; reload alone cannot loosen it.
    core.config_relax(&repo_name("tool")).unwrap();
    // T starts on S's chain top, so its publish really pushes both commits.
    let t = close::new(&server);
    fs::write(ws(&core, &t, "tool").join("t.txt"), "t\n").unwrap();
    let cp = checkpoint(&core, &t, "tool");
    let g = group(vec![batch(&t, "tool", cp.id, &["t.txt"])]);
    let (out, _) = at_pause(
        &d,
        "auto_close.after_enumerate",
        || call(&server, "publish", json!({"group": g, "shown": {}})),
        || call(&server, "mark", json!({"session": s, "state": "active"})),
    );
    assert_eq!(out["published"][0]["push"]["kind"], "Pushed", "{out}");
    assert_eq!(out["auto_close"]["rejudged"], json!([]), "{out}");
    assert!(
        out["auto_close"]["sessions"].get(s.as_str()).is_none(),
        "{out}"
    );
    assert_eq!(out["closed"], true, "T alone controls group closed: {out}");
    assert_eq!(entry(&out, &t)["closed"], true, "{out}");
    assert_eq!(entry(&out, &t)["in_group"], true, "{out}");
    assert_eq!(
        core.store.session(&t).unwrap().unwrap().lifecycle,
        Lifecycle::Removed
    );
    assert_live(&core, &s);
    assert!(mark_of(&core, &s).is_none());
    assert_eq!(audits(&fx, &s), before);
}

#[test]
fn mark_done_after_the_enumeration_waits_for_the_next_trigger() {
    let fx = fixture(false);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    // A preexisting candidate makes push enter the enumeration seam. U is
    // clean and unmarked when that fixed candidate set is collected.
    let s = not_pushed(&server, &core);
    let u = close::new(&server);
    assert!(mark_of(&core, &u).is_none());
    let (out, _) = at_pause(
        &d,
        "auto_close.after_enumerate",
        || call(&server, "push", json!({"repo": "tool"})),
        || {
            call(
                &server,
                "mark",
                json!({"session": u, "state": "done", "agent": "codex"}),
            )
        },
    );
    assert_eq!(out["push"]["kind"], "Pushed", "{out}");
    assert_eq!(out["auto_close"]["rejudged"], json!([s]), "{out}");
    assert!(
        out["auto_close"]["sessions"].get(u.as_str()).is_none(),
        "{out}"
    );
    assert_live(&core, &u);
    let mark = mark_of(&core, &u).unwrap();
    assert_eq!(mark.state, MarkState::Done);
    assert_eq!(mark.by.kind, MarkByKind::Agent);
    assert!(audits(&fx, &u).is_empty());

    // An equal chain returns NoCommit before the hook. Supply a genuinely
    // new unpushed commit for the next trigger, leaving U outside its group.
    let v = close::new(&server);
    let staged = publish_file(&server, &core, &v, "v.txt");
    assert!(staged["published"][0]["push"].is_null(), "{staged}");
    ok(call(
        &server,
        "mark",
        json!({"session": v, "state": "active"}),
    ));
    assert!(
        audits(&fx, &u).is_empty(),
        "a non-pushing publish cannot rejudge U"
    );
    let next = ok(call(&server, "push", json!({"repo": "tool"})));
    assert_eq!(next["push"]["kind"], "Pushed", "{next}");
    assert_eq!(next["auto_close"]["rejudged"], json!([u]), "{next}");
    assert_eq!(entry(&next, &u)["closed"], true, "{next}");
    assert_eq!(entry(&next, &u)["in_group"], false, "{next}");
    assert_eq!(
        core.store.session(&u).unwrap().unwrap().lifecycle,
        Lifecycle::Removed
    );
    // §3: existing marks stay on Removed tombstones; close must not replace
    // the agent's mark with a daemon refusal mark.
    let retained = mark_of(&core, &u).expect("mark stays on Removed tombstone");
    assert_eq!(retained.at, mark.at);
    assert_eq!(retained.by.kind, MarkByKind::Agent);
    assert_eq!(retained.state, MarkState::Done);
    assert_eq!(audits(&fx, &u).len(), 1);
}

#[test]
fn own_session_removed_before_its_hook_reads_closed() {
    let fx = fixture(true);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let b = close::new(&server);
    let root = core.store.session(&b).unwrap().unwrap().manifest.root;
    fs::write(ws(&core, &b, "tool").join("b.txt"), "b\n").unwrap();
    let cp = checkpoint(&core, &b, "tool");
    let g = group(vec![batch(&b, "tool", cp.id, &["b.txt"])]);
    let (out, _) = at_pause(
        &d,
        "auto_close.before_judge",
        || call(&server, "publish", json!({"group": g, "shown": {}})),
        || call(&server, "rm", json!({"session": b, "discard": false})),
    );
    assert_eq!(out["closed"], true, "{out}");
    assert_eq!(
        entry(&out, &b),
        &json!({
            "closed": true, "in_group": true, "by": "other", "reasons": [],
            "detail": {}, "rm": null, "mark": {"written": false}
        }),
        "{out}"
    );
    assert!(out["upstream"].get(b.as_str()).is_none(), "{out}");
    assert_eq!(
        core.store.session(&b).unwrap().unwrap().lifecycle,
        Lifecycle::Removed
    );
    assert!(!root.exists());
    assert!(
        core.gits[&repo_name("tool")]
            .read_ref(&format!("refs/strata/pins/{b}"))
            .unwrap()
            .is_none()
    );
    assert!(mark_of(&core, &b).is_none());
    let lines = audits(&fx, &b);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["closed"], false);
    assert_eq!(lines[0]["reasons"], json!(["gone"]));
    assert_eq!(lines[0]["mark"], json!({"written": false}));
    assert!(lines[0]["rm"].is_null());
}
