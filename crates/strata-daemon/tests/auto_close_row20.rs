//! Auto-close §10 row 20 (r7 blocker): an `add` of an opted-out repo during
//! the hook, and the handler-level nits that change behaviour. Temp-dir
//! fixtures only.
mod support;

use std::fs;
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use strata_daemon::handlers::{Server, Trackers};
use strata_daemon::startup::{Core, Daemon};
use strata_session::SessionSeam;
use strata_store::{Lifecycle, PublishPhase};
use strata_types::{MarkByKind, SessionId};
use support::close::{self, audit_lines, call, mark_of, ok};
use support::*;

/// `front` and `tool`, auto-close on globally, `front` opted out.
fn fixture(tool_push: bool, front_push: bool) -> Dfx {
    let mut fx = close::opted_in(&["front", "tool"]);
    fx.catch_up_every = Duration::from_secs(86400);
    fx.lock_wait = Duration::from_secs(60);
    let front = fx.config.repos.get_mut(&repo_name("front")).unwrap();
    front.publish.auto_close = Some(false);
    front.publish.push = front_push;
    fx.config.repos.get_mut(&repo_name("tool")).unwrap().publish.push = tool_push;
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
    core.gits[&repo_name(repo)]
        .read_ref(&format!("refs/strata/pins/{s}"))
        .unwrap()
        .is_some()
}

fn audit_of(fx: &Dfx, s: &SessionId) -> usize {
    audit_lines(fx)
        .iter()
        .filter(|l| l["event"] == "auto_close" && l["session"] == s.as_str())
        .count()
}

fn add_front(server: &Server<'_, '_>, s: &SessionId) -> Value {
    call(server, "add", json!({"session": s, "repos": ["front"]}))
}

fn assert_added(reply: &Value) {
    let data = ok(reply.clone());
    assert!(!data["report"]["added"]["front"].is_null(), "{data}");
}

fn assert_kept_both(core: &Core, s: &SessionId) {
    assert_eq!(lifecycle(core, s), Lifecycle::Active);
    for repo in ["front", "tool"] {
        assert!(ws(core, s, repo).exists(), "{repo} workspace");
        assert!(pin(core, s, repo), "{repo} pin");
    }
}

/// Publish `tool` on a thread, `add front` at `auto_close.before_judge`.
fn publish_with_add_at_judge(d: &Daemon<'_>, server: &Server<'_, '_>, core: &Core, s: &SessionId) -> Value {
    fs::write(ws(core, s, "tool").join("t.txt"), "t\n").unwrap();
    let cp = checkpoint(core, s, "tool");
    let g = group(vec![batch(s, "tool", cp.id, &["t.txt"])]);
    d.sessions.arm(SessionSeam::PauseAt { phase: "auto_close.before_judge".into() });
    std::thread::scope(|sc| {
        let h = sc.spawn(|| call(server, "publish", json!({"group": g, "shown": {}})));
        d.sessions.wait_paused();
        // The hook holds no queue: `add` lands at the pause.
        let added = add_front(server, s);
        d.sessions.release();
        assert_added(&added);
        h.join().unwrap()
    })
}

#[test]
fn add_of_opted_out_repo_during_the_hook_disables_the_close() {
    let fx = fixture(true, true);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_only(&server, &["tool"]);
    let out = ok(publish_with_add_at_judge(&d, &server, &core, &s));
    assert_eq!(out["published"][0]["push"]["kind"], "Pushed", "{out}");
    assert_eq!(out["closed"], false, "{out}");
    let e = entry(&out, &s);
    assert_eq!(e["reasons"], json!(["disabled"]), "{out}");
    assert_eq!(e["detail"]["repo"], "front", "{out}");
    assert_eq!(out["auto_close"]["rejudged"], json!([]), "{out}");
    assert_kept_both(&core, &s);
    assert!(mark_of(&core, &s).is_none());
    assert_eq!(audit_of(&fx, &s), 0);
}

#[test]
fn add_on_an_unpushed_chain_does_not_mark_an_opted_out_session() {
    let fx = fixture(true, false);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let u = close::new_only(&server, &["front"]);
    fs::write(ws(&core, &u, "front").join("u.txt"), "u\n").unwrap();
    let uo = ok(close::publish(&server, &core, &u, "front", &["u.txt"]));
    assert_eq!(entry(&uo, &u)["reasons"], json!(["disabled"]), "{uo}");
    let s = close::new_only(&server, &["tool"]);
    let out = ok(publish_with_add_at_judge(&d, &server, &core, &s));
    assert_eq!(out["closed"], false, "{out}");
    assert_eq!(entry(&out, &s)["reasons"], json!(["disabled"]), "{out}");
    assert_kept_both(&core, &s);
    assert!(mark_of(&core, &s).is_none());
    assert_eq!(audit_of(&fx, &s), 0);
}

#[test]
fn push_rejudge_after_add_of_opted_out_repo_is_disabled() {
    let fx = fixture(false, true);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_only(&server, &["tool"]);
    fs::write(ws(&core, &s, "tool").join("t.txt"), "t\n").unwrap();
    let first = ok(close::publish(&server, &core, &s, "tool", &["t.txt"]));
    assert_eq!(entry(&first, &s)["reasons"], json!(["not_pushed"]), "{first}");
    ok(call(&server, "mark", json!({"session": s, "state": "active"})));
    ok(call(&server, "mark", json!({"session": s, "state": "done", "agent": "codex"})));
    let at0 = mark_of(&core, &s).unwrap();
    assert_eq!(at0.by.kind, MarkByKind::Agent);
    let audits = audit_of(&fx, &s);
    d.sessions.arm(SessionSeam::PauseAt { phase: "auto_close.before_judge".into() });
    let pushed = std::thread::scope(|sc| {
        let h = sc.spawn(|| call(&server, "push", json!({"repo": "tool"})));
        d.sessions.wait_paused();
        let added = add_front(&server, &s);
        d.sessions.release();
        assert_added(&added);
        ok(h.join().unwrap())
    });
    assert_eq!(pushed["push"]["kind"], "Pushed", "{pushed}");
    assert!(pushed.get("closed").is_none(), "{pushed}");
    assert_eq!(pushed["auto_close"]["closed"], false, "{pushed}");
    assert_eq!(entry(&pushed, &s)["reasons"], json!(["disabled"]), "{pushed}");
    assert_eq!(pushed["auto_close"]["rejudged"], json!([]), "{pushed}");
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);
    assert_eq!(mark_of(&core, &s).unwrap().at, at0.at);
    assert_eq!(audit_of(&fx, &s), audits);
}

#[test]
fn add_after_the_reread_waits_and_finds_the_session_closed() {
    let fx = fixture(true, true);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_only(&server, &["tool"]);
    fs::write(ws(&core, &s, "tool").join("t.txt"), "t\n").unwrap();
    let cp = checkpoint(&core, &s, "tool");
    let g = group(vec![batch(&s, "tool", cp.id, &["t.txt"])]);
    d.sessions.arm(SessionSeam::PauseAt { phase: "auto_close.after_reread".into() });
    let (out, added) = std::thread::scope(|sc| {
        let h = sc.spawn(|| call(&server, "publish", json!({"group": g, "shown": {}})));
        d.sessions.wait_paused();
        let a = sc.spawn(|| add_front(&server, &s));
        std::thread::sleep(Duration::from_millis(500));
        assert!(!a.is_finished(), "add must wait for the place");
        d.sessions.release();
        (h.join().unwrap(), a.join().unwrap())
    });
    let out = ok(out);
    assert_eq!(out["closed"], true, "{out}");
    assert_eq!(added["ok"], false, "{added}");
    let code = added["error"]["code"].as_str().unwrap();
    assert!(["NoSuchSession", "InvalidInput", "Busy"].contains(&code), "{added}");
    assert_eq!(lifecycle(&core, &s), Lifecycle::Removed);
    assert!(!pin(&core, &s, "front"));
}

fn tool_only() -> (Dfx,) {
    let mut fx = close::opted_in(&["tool"]);
    fx.catch_up_every = Duration::from_secs(86400);
    fx.write_config();
    (fx,)
}

#[test]
fn failed_holder_scan_is_error_not_unknown() {
    let (fx,) = tool_only();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);
    fs::write(ws(&core, &s, "tool").join("t.txt"), "t\n").unwrap();
    let dir = fx.config.paths.root.join("sessions/.repos").join(s.as_str());
    strata_deps::test_seams::fail_open_files_under(
        &dir,
        strata_types::StrataError::new(strata_types::Code::Unknown, "scan failed"),
    );
    let out = ok(close::publish(&server, &core, &s, "tool", &["t.txt"]));
    let e = entry(&out, &s);
    assert_eq!(e["reasons"], json!(["error"]), "{out}");
    assert_eq!(e["detail"]["code"], "Unknown", "{out}");
    assert_eq!(e["mark"]["skipped"], "error", "{out}");
    assert!(mark_of(&core, &s).is_none());
    assert_eq!(audit_of(&fx, &s), 1);
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);
}

#[test]
fn failure_after_removing_is_unknown() {
    let (fx,) = tool_only();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);
    fs::write(ws(&core, &s, "tool").join("t.txt"), "t\n").unwrap();
    d.sessions.arm(SessionSeam::FailAt { phase: "remove.unpin".into(), errno: libc::EIO });
    let out = ok(close::publish(&server, &core, &s, "tool", &["t.txt"]));
    assert_eq!(out["closed"], false, "{out}");
    let e = entry(&out, &s);
    assert_eq!(e["reasons"], json!(["unknown"]), "{out}");
    assert_eq!(e["detail"]["state"], "unknown", "{out}");
    assert!(mark_of(&core, &s).is_none());
    assert_eq!(lifecycle(&core, &s), Lifecycle::Unknown);
}

#[test]
fn upstream_unavailable_writes_no_mark() {
    let (fx,) = tool_only();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);
    fs::write(ws(&core, &s, "tool").join("t.txt"), "t\n").unwrap();
    core.upstream.fail_for_test(true);
    let out = ok(close::publish(&server, &core, &s, "tool", &["t.txt"]));
    core.upstream.fail_for_test(false);
    let e = entry(&out, &s);
    assert_eq!(e["reasons"], json!(["upstream_unavailable"]), "{out}");
    assert_eq!(e["mark"]["skipped"], "error", "{out}");
    assert!(mark_of(&core, &s).is_none());
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);
}

fn plant_assignment(core: &Core, s: &SessionId, path: &str) {
    let row = core.store.publishes().unwrap().into_iter().rev()
        .find(|r| r.phase == PublishPhase::Done).expect("a Done row");
    let mut outcome = row.outcome.clone().unwrap_or_else(|| json!({}));
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
    outcome["repo"] = json!("tool");
    outcome["assigned"] = json!([{"path": path, "session": s, "at": now}]);
    assert!(core.store.set_publish_outcome(row.group.id, PublishPhase::Done, &outcome).unwrap());
}

fn assignment(other: bool) {
    let (fx,) = tool_only();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    // A Done row of `tool` to carry the assignment.
    let p = close::new(&server);
    fs::write(ws(&core, &p, "tool").join("p.txt"), "p\n").unwrap();
    ok(close::publish(&server, &core, &p, "tool", &["p.txt"]));
    let s = close::new(&server);
    let o = close::new(&server);
    plant_assignment(&core, if other { &o } else { &s }, "src/a.ts");
    fs::create_dir_all(ws(&core, &s, "tool").join("src")).unwrap();
    fs::write(ws(&core, &s, "tool").join("src/b.ts"), "b\n").unwrap();
    if !other {
        fs::write(ws(&core, &s, "tool").join("src/a.ts"), "a\n").unwrap();
    }
    let out = ok(close::publish(&server, &core, &s, "tool", &["src/b.ts"]));
    if other {
        // The row names another session: S closes on the first publish.
        assert_eq!(out["closed"], true, "{out}");
        assert_eq!(lifecycle(&core, &s), Lifecycle::Removed);
        return;
    }
    let e = entry(&out, &s);
    assert_eq!(e["reasons"], json!(["assigned"]), "{out}");
    assert_eq!(e["detail"]["repo"], "tool", "{out}");
    assert_eq!(e["detail"]["paths"], json!(["src/a.ts"]), "{out}");
    assert_eq!(mark_of(&core, &s).unwrap().by.kind, MarkByKind::Daemon);
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);
    let out = ok(close::publish(&server, &core, &s, "tool", &["src/a.ts"]));
    assert_eq!(out["closed"], true, "{out}");
    assert_eq!(lifecycle(&core, &s), Lifecycle::Removed);
}

#[test]
fn assignment_to_the_session_holds_the_close() { assignment(false); }

#[test]
fn assignment_to_another_session_does_not_hold_the_close() { assignment(true); }

#[test]
fn publish_marks_done_for_a_nested_repository() {
    let (fx,) = tool_only();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);
    let nested = ws(&core, &s, "tool").join("vendor/x");
    fs::create_dir_all(&nested).unwrap();
    fx.sb.git(&nested, &["init", "-q"]);
    fs::write(nested.join("f"), "f\n").unwrap();
    fx.sb.git(&nested, &["add", "f"]);
    fx.sb.git(&nested, &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "x"]);
    fs::write(ws(&core, &s, "tool").join("t.txt"), "t\n").unwrap();
    let out = ok(close::publish(&server, &core, &s, "tool", &["t.txt"]));
    let e = entry(&out, &s);
    assert_eq!(e["reasons"], json!(["embedded_repo"]), "{out}");
    assert_eq!(e["detail"]["embedded_repos"]["tool"]["paths"], json!(["vendor/x"]), "{out}");
    assert_eq!(mark_of(&core, &s).unwrap().by.kind, MarkByKind::Daemon);
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);
    assert!(nested.join(".git").exists());
    ok(call(&server, "rm", json!({"session": s, "discard": false})));
    assert_eq!(lifecycle(&core, &s), Lifecycle::Removed);
}
