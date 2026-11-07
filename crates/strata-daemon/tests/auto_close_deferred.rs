//! Slice W: §10 named tests earlier slices deferred (row 1 leftovers, row 23
//! daemon/config gone+busy race). Temp-dir fixtures only.
mod support;

use std::fs;
use std::time::Duration;

use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use strata_daemon::handlers::{Server, Trackers};
use strata_daemon::startup::{Core, Daemon};
use strata_session::SessionSeam;
use strata_store::{Lifecycle, PublishPhase};
use strata_types::{MarkByKind, MarkState, SessionId};
use support::close::{self, audit_lines, call, mark_of, ok};
use support::*;

fn entry<'a>(out: &'a Value, s: &SessionId) -> &'a Value {
    &out["auto_close"]["sessions"][s.as_str()]
}

fn lifecycle(core: &Core, s: &SessionId) -> Lifecycle {
    core.store.session(s).unwrap().unwrap().lifecycle
}

fn audits(fx: &Dfx, s: &SessionId) -> Vec<Value> {
    audit_lines(fx)
        .into_iter()
        .filter(|l| l["event"] == "auto_close" && l["session"] == s.as_str())
        .collect()
}

fn tool_fixture(push: bool, quiet_secs: u64) -> Dfx {
    let mut fx = close::opted_in(&["tool"]);
    fx.catch_up_every = Duration::from_secs(86400);
    fx.lock_wait = Duration::from_secs(60);
    let rc = fx.config.repos.get_mut(&repo_name("tool")).unwrap();
    rc.publish.push = push;
    rc.rebase.quiet_secs = quiet_secs;
    rc.deps_dirs.clear();
    rc.seed_caches.clear();
    rc.install_inputs.clear();
    rc.install = None;
    fx.write_config();
    fx
}

/// (a) `pins_tightened` during the hook and (b) `du_cancel_timeout` from a
/// stuck du scan: both Busy, no mark, session Active, publish Done.
#[test]
fn publish_busy_by_code_marks_nothing() {
    busy_scene_pins_tightened();
    busy_scene_du_cancel_timeout();
}

fn assert_busy_no_mark(fx: &Dfx, core: &Core, s: &SessionId, out: &Value, reason: &str) {
    assert_eq!(out["closed"], false, "{out}");
    let e = entry(out, s);
    assert_eq!(e["reasons"], json!(["busy"]), "{out}");
    assert_eq!(e["detail"]["reason"], reason, "{out}");
    assert_eq!(e["mark"]["skipped"], "busy", "{out}");
    assert!(mark_of(core, s).is_none(), "Busy writes no mark");
    assert_eq!(lifecycle(core, s), Lifecycle::Active);
    let id = out["published"][0]["id"].as_u64().expect("publish id");
    assert_eq!(
        core.store.publish_record(id).unwrap().unwrap().phase,
        PublishPhase::Done
    );
    let lines = audits(fx, s);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["closed"], false, "{lines:?}");
    assert_eq!(lines[0]["reasons"], json!(["busy"]), "{lines:?}");
}

fn busy_scene_pins_tightened() {
    let mut fx = tool_fixture(true, 0);
    fx.config
        .repos
        .get_mut(&repo_name("tool"))
        .unwrap()
        .publish
        .confirm = false;
    fx.write_config();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);

    fs::write(ws(&core, &s, "tool").join("new.txt"), "mine\n").unwrap();
    let cp = checkpoint(&core, &s, "tool");
    let g = group(vec![batch(&s, "tool", cp.id, &["new.txt"])]);
    d.sessions.arm(SessionSeam::PauseAt {
        phase: "auto_close.before_judge".into(),
    });
    let out = std::thread::scope(|sc| {
        let h = sc.spawn(|| call(&server, "publish", json!({"group": g, "shown": {}})));
        assert!(
            d.sessions.wait_paused_timeout(Duration::from_secs(30)),
            "before_judge not reached"
        );
        fx.config
            .repos
            .get_mut(&repo_name("tool"))
            .unwrap()
            .publish
            .confirm = true;
        fx.write_config();
        let reloaded = core.reload_for_test();
        assert_eq!(reloaded.outcome, "applied", "{reloaded:?}");
        d.sessions.release();
        ok(h.join().unwrap())
    });

    assert_busy_no_mark(&fx, &core, &s, &out, "pins_tightened");
}

/// (b) A du scan held on the session with its cancel token ignored
/// (`du.rs` unit-test plant, exposed via `plant_stuck_scan_for_test`) →
/// `detail.reason == "du_cancel_timeout"`.
///
/// Plant at `auto_close.before_judge` (after the publish rebase): planting
/// earlier makes `PublishLocksDu` treat the hold timeout as a rebase Busy and
/// `sleep_busy` forever. Auto-close's `with_du_hold` does not retry.
fn busy_scene_du_cancel_timeout() {
    let mut fx = tool_fixture(true, 0);
    fx.config
        .repos
        .get_mut(&repo_name("tool"))
        .unwrap()
        .publish
        .confirm = false;
    fx.write_config();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);

    fs::write(ws(&core, &s, "tool").join("new.txt"), "mine\n").unwrap();
    let cp = checkpoint(&core, &s, "tool");
    let g = group(vec![batch(&s, "tool", cp.id, &["new.txt"])]);
    d.sessions.arm(SessionSeam::PauseAt {
        phase: "auto_close.before_judge".into(),
    });
    let (out, token) = std::thread::scope(|sc| {
        let h = sc.spawn(|| call(&server, "publish", json!({"group": g, "shown": {}})));
        assert!(
            d.sessions.wait_paused_timeout(Duration::from_secs(30)),
            "before_judge not reached"
        );
        // Stuck active entry: hold sets the cancel flag but nobody leaves active.
        let token = d.du.plant_stuck_scan_for_test(s.as_str());
        d.sessions.release();
        (ok(h.join().unwrap()), token)
    });
    assert!(
        token.load(std::sync::atomic::Ordering::SeqCst),
        "hold must cancel the planted token"
    );
    d.du.clear_stuck_scan_for_test(s.as_str());

    assert_busy_no_mark(&fx, &core, &s, &out, "du_cancel_timeout");
}

/// Note names `rm`, not publish, on why_none=no_built_base.
///
/// Design's Deferred+gc'd-dir shape: deleting the on-disk base while the store
/// row remains yields `error`/`judge_read`, and a Deferred publish still builds
/// the tip so a later push closes. The unbuilt-target path of row 19 is what
/// produces `no_built_base` today.
#[test]
fn publish_marks_done_no_built_base_names_rm() {
    let fx = tool_fixture(true, 3600);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);

    let hook = fx.sb.path("tool-origin.git/hooks/pre-receive");
    fs::write(&hook, "#!/bin/sh\necho 'denied by policy' >&2\nexit 1\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let s = close::new(&server);
    fs::write(ws(&core, &s, "tool").join("new.txt"), "mine\n").unwrap();
    let first = close::publish(&server, &core, &s, "tool", &["new.txt"]);
    assert_eq!(first["ok"], false, "{first}");
    assert!(mark_of(&core, &s).is_none(), "error path writes no mark");
    fs::remove_file(&hook).unwrap();

    let pushed = ok(call(&server, "push", json!({ "repo": "tool" })));
    assert_eq!(pushed["push"]["kind"], "Pushed", "{pushed}");

    // Do not create another session first: that builds the tip and the close
    // would succeed (row 19's control). Re-run S's publish while the tip is
    // still unbuilt for S's deps.
    let out = ok(close::publish(&server, &core, &s, "tool", &["new.txt"]));
    assert_eq!(
        out["already_published"][0]["push"]["kind"], "Pushed",
        "{out}"
    );
    let e = entry(&out, &s);
    assert_eq!(e["closed"], false, "{out}");
    assert_eq!(e["reasons"], json!(["unpublished"]), "{out}");
    assert_eq!(
        e["detail"]["landing"]["tool"]["why_none"], "no_built_base",
        "{out}"
    );
    let mark = mark_of(&core, &s).expect("daemon mark");
    assert_eq!(mark.state, MarkState::Done);
    assert_eq!(mark.by.kind, MarkByKind::Daemon);
    let note = mark.note.as_deref().unwrap_or("");
    assert!(
        note.contains(&format!("git-strata rm --session {s}")),
        "note names rm: {note}"
    );
    assert!(
        note.contains("not published against"),
        "note uses no_built_base wording: {note}"
    );
    assert!(
        !note.contains("an older publish has this content"),
        "no_built_base outranks historical: {note}"
    );
    assert!(
        !note.contains("publish it") && !note.contains("then publish"),
        "note must not advise publish: {note}"
    );
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);
}

#[test]
fn removed_group_session_under_a_tightening_reload_reads_gone() {
    let mut fx = tool_fixture(true, 0);
    fx.config
        .repos
        .get_mut(&repo_name("tool"))
        .unwrap()
        .publish
        .confirm = false;
    fx.write_config();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let x = close::new(&server);
    let y = close::new(&server);
    let (a, s) = if x.as_str() < y.as_str() {
        (x, y)
    } else {
        (y, x)
    };

    fs::write(ws(&core, &a, "tool").join("a.txt"), "a\n").unwrap();
    fs::write(ws(&core, &s, "tool").join("s.txt"), "s\n").unwrap();
    let acp = checkpoint(&core, &a, "tool");
    let scp = checkpoint(&core, &s, "tool");
    let g = group(vec![
        batch(&a, "tool", acp.id, &["a.txt"]),
        batch(&s, "tool", scp.id, &["s.txt"]),
    ]);
    d.sessions.arm(SessionSeam::PauseAt {
        phase: "auto_close.before_judge".into(),
    });
    let out = std::thread::scope(|sc| {
        let h = sc.spawn(|| call(&server, "publish", json!({"group": g, "shown": {}})));
        assert!(
            d.sessions.wait_paused_timeout(Duration::from_secs(30)),
            "before_judge not reached"
        );
        let removed = call(&server, "rm", json!({"session": s, "discard": false}));
        assert_eq!(removed["ok"], true, "{removed}");
        fx.config
            .repos
            .get_mut(&repo_name("tool"))
            .unwrap()
            .publish
            .confirm = true;
        fx.write_config();
        assert_eq!(core.reload_for_test().outcome, "applied");
        d.sessions.release();
        ok(h.join().unwrap())
    });

    let ea = entry(&out, &a);
    assert_eq!(ea["reasons"], json!(["busy"]), "{out}");
    assert_eq!(ea["detail"]["reason"], "pins_tightened", "{out}");
    assert_eq!(lifecycle(&core, &a), Lifecycle::Active);
    assert!(mark_of(&core, &a).is_none());

    let es = entry(&out, &s);
    assert_eq!(
        es,
        &json!({
            "closed": true, "in_group": true, "by": "other", "reasons": [],
            "detail": {}, "rm": null, "mark": {"written": false}
        }),
        "{out}"
    );
    assert_eq!(lifecycle(&core, &s), Lifecycle::Removed);
    let s_lines = audits(&fx, &s);
    assert_eq!(s_lines.len(), 1, "{s_lines:?}");
    assert_eq!(s_lines[0]["closed"], false, "{s_lines:?}");
    assert_eq!(s_lines[0]["reasons"], json!(["gone"]), "{s_lines:?}");
}

/// Variant (b): a done-marked session already Removed before step 1 is absent
/// from `sessions`, `rejudged`, and new audit lines.
#[test]
fn removed_at_step1_is_absent_from_sessions_rejudged_and_audit() {
    let fx = tool_fixture(false, 0);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let live = close::new(&server);
    let gone = close::new(&server);

    fs::write(ws(&core, &live, "tool").join("live.txt"), "live\n").unwrap();
    let live_out = ok(close::publish(&server, &core, &live, "tool", &["live.txt"]));
    assert_eq!(
        entry(&live_out, &live)["reasons"],
        json!(["not_pushed"]),
        "{live_out}"
    );

    fs::write(ws(&core, &gone, "tool").join("gone.txt"), "gone\n").unwrap();
    let gone_out = ok(close::publish(&server, &core, &gone, "tool", &["gone.txt"]));
    assert_eq!(
        entry(&gone_out, &gone)["reasons"],
        json!(["not_pushed"]),
        "{gone_out}"
    );
    ok(call(
        &server,
        "mark",
        json!({"session": gone, "state": "active"}),
    ));
    ok(call(
        &server,
        "mark",
        json!({"session": gone, "state": "done", "agent": "codex"}),
    ));
    ok(call(
        &server,
        "rm",
        json!({"session": gone, "discard": false}),
    ));
    assert_eq!(lifecycle(&core, &gone), Lifecycle::Removed);
    let audits_before = audit_lines(&fx).len();

    let pushed = ok(call(&server, "push", json!({"repo": "tool"})));
    assert_eq!(pushed["push"]["kind"], "Pushed", "{pushed}");
    assert!(
        pushed["auto_close"]["sessions"]
            .get(gone.as_str())
            .is_none(),
        "Removed at step 1 absent from sessions: {pushed}"
    );
    let rejudged = pushed["auto_close"]["rejudged"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        rejudged.iter().all(|v| v.as_str() != Some(gone.as_str())),
        "{pushed}"
    );
    let after = audit_lines(&fx);
    assert!(
        after[audits_before..]
            .iter()
            .all(|l| l["session"] != gone.as_str()),
        "no new audit for gone: {after:?}"
    );
    assert_eq!(lifecycle(&core, &live), Lifecycle::Removed);
}
