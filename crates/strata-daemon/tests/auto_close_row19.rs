//! Auto-close §10 row 19 (r6): the hook's failure paths and the mark.
//! Temp-dir fixtures only.
mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use serde_json::{Value, json};
use strata_daemon::handlers::{Server, Trackers};
use strata_daemon::startup::{Core, Daemon};
use strata_session::SessionSeam;
use strata_store::Lifecycle;
use strata_types::{MarkByKind, SessionId};
use support::close::{self, audit_lines, call, mark_of, ok};
use support::*;

fn entry<'a>(out: &'a Value, s: &SessionId) -> &'a Value {
    &out["auto_close"]["sessions"][s.as_str()]
}

fn lifecycle(core: &Core, s: &SessionId) -> Lifecycle {
    core.store.session(s).unwrap().unwrap().lifecycle
}

fn audits_of(fx: &Dfx, s: &SessionId) -> Vec<Value> {
    audit_lines(fx)
        .into_iter()
        .filter(|l| l["event"] == "auto_close" && l["session"] == s.as_str())
        .collect()
}

fn tool_fixture() -> Dfx {
    let mut fx = close::opted_in(&["tool"]);
    fx.catch_up_every = Duration::from_secs(86400);
    fx.lock_wait = Duration::from_secs(60);
    fx.write_config();
    fx
}

fn assert_error_entry(out: &Value, s: &SessionId, code: &str) {
    assert_eq!(out["published"][0]["push"]["kind"], "Pushed", "{out}");
    assert_eq!(out["closed"], false, "{out}");
    let e = entry(out, s);
    assert_eq!(e["reasons"], json!(["error"]), "{out}");
    assert_eq!(e["detail"]["code"], code, "{out}");
    assert_eq!(e["mark"]["skipped"], "error", "{out}");
}

#[test]
fn hook_panic_leaves_the_publish_landed() {
    let fx = tool_fixture();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_only(&server, &["tool"]);
    fs::write(ws(&core, &s, "tool").join("t.txt"), "t\n").unwrap();
    d.sessions.arm(SessionSeam::PanicAt {
        phase: "auto_close.in_closure".into(),
    });
    let reply = close::publish(&server, &core, &s, "tool", &["t.txt"]);
    let out = ok(reply);
    assert_error_entry(&out, &s, "handler_panicked");
    let lines = audits_of(&fx, &s);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["reasons"], json!(["error"]), "{lines:?}");
    assert!(mark_of(&core, &s).is_none());
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);
    // The place the closure held was released: rm takes it and passes.
    ok(call(&server, "rm", json!({"session": s, "discard": false})));
    assert_eq!(lifecycle(&core, &s), Lifecycle::Removed);
}

#[test]
fn queue_take_failure_writes_no_mark() {
    let fx = tool_fixture();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_only(&server, &["tool"]);
    fs::write(ws(&core, &s, "tool").join("t.txt"), "t\n").unwrap();
    let cp = checkpoint(&core, &s, "tool");
    let g = group(vec![batch(&s, "tool", cp.id, &["t.txt"])]);
    d.sessions.arm(SessionSeam::PauseAt {
        phase: "auto_close.after_enumerate".into(),
    });
    let reply = std::thread::scope(|sc| {
        let h = sc.spawn(|| call(&server, "publish", json!({"group": g, "shown": {}})));
        d.sessions.wait_paused();
        core.locks.shutdown();
        d.sessions.release();
        h.join().unwrap()
    });
    let out = ok(reply);
    assert_error_entry(&out, &s, "ReadOnly");
    assert!(mark_of(&core, &s).is_none());
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);
}

#[test]
fn rejudge_with_the_same_reasons_keeps_mark_at() {
    let mut fx = close::opted_in(&["front", "tool"]);
    fx.catch_up_every = Duration::from_secs(86400);
    fx.config
        .repos
        .get_mut(&repo_name("front"))
        .unwrap()
        .publish
        .push = false;
    fx.write_config();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_in(&server, &["front"]);
    fs::write(ws(&core, &s, "front").join("a.txt"), "a\n").unwrap();
    let first = ok(close::publish(&server, &core, &s, "front", &["a.txt"]));
    let c1 = first["published"][0]["commit"].as_str().unwrap().to_owned();
    assert_eq!(
        entry(&first, &s)["reasons"],
        json!(["not_pushed"]),
        "{first}"
    );
    let m0 = mark_of(&core, &s).expect("daemon mark");
    assert_eq!(m0.by.kind, MarkByKind::Daemon);
    let note0 = m0.note.clone().unwrap();
    let id0 = first["published"][0]["id"].as_u64().unwrap();
    assert!(
        !note0.contains(&format!("publish {id0}")) && !note0.contains(&format!("#{id0}")),
        "{note0}"
    );
    for name in ["t1.txt", "t2.txt"] {
        let t = close::new_only(&server, &["tool"]);
        fs::write(ws(&core, &t, "tool").join(name), "t\n").unwrap();
        let out = ok(close::publish(&server, &core, &t, "tool", &[name]));
        assert_eq!(entry(&out, &t)["closed"], true, "{out}");
        let e = entry(&out, &s);
        assert_eq!(e["reasons"], json!(["not_pushed"]), "{out}");
        assert_eq!(e["mark"]["skipped"], "unchanged", "{out}");
        let m = mark_of(&core, &s).unwrap();
        assert_eq!(m.at, m0.at);
        assert_eq!(m.note.as_deref(), Some(note0.as_str()));
    }
    fs::write(ws(&core, &s, "front").join("b.txt"), "b\n").unwrap();
    let second = ok(close::publish(&server, &core, &s, "front", &["b.txt"]));
    let c2 = second["published"][0]["commit"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(entry(&second, &s)["mark"]["written"], true, "{second}");
    let m = mark_of(&core, &s).unwrap();
    let note = m.note.unwrap();
    assert!(
        note.contains(&format!("publish {id0} (commit {}", &c1[..10])),
        "{note}"
    );
    assert!(note.contains(&format!("base {}", &c2[..10])), "{note}");
    assert!(m.at > m0.at);
}

fn refusing(fx: &Dfx) -> std::path::PathBuf {
    let hook = fx.sb.path("tool-origin.git/hooks/pre-receive");
    fs::write(&hook, "#!/bin/sh\necho 'denied by policy' >&2\nexit 1\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    hook
}

/// A's refused publish, then a human's `push tool` with the hook removed.
fn refused_then_pushed(fx: &Dfx, server: &Server<'_, '_>, core: &Core) -> SessionId {
    let hook = refusing(fx);
    let a = close::new_only(server, &["tool"]);
    fs::write(ws(core, &a, "tool").join("new.txt"), "mine").unwrap();
    let first = close::publish(server, core, &a, "tool", &["new.txt"]);
    assert_eq!(first["ok"], false, "{first}");
    assert!(mark_of(core, &a).is_none(), "error path writes no mark");
    fs::remove_file(hook).unwrap();
    let pushed = ok(call(server, "push", json!({"repo": "tool"})));
    assert_eq!(pushed["push"]["kind"], "Pushed", "{pushed}");
    assert!(
        entry(&pushed, &a).is_null(),
        "the trigger skips A: {pushed}"
    );
    assert!(mark_of(core, &a).is_none());
    a
}

#[test]
fn refused_publish_pushed_by_a_human_reads_pushed_on_the_rerun() {
    let fx = tool_fixture();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let a = refused_then_pushed(&fx, &server, &core);
    let out = ok(close::publish(&server, &core, &a, "tool", &["new.txt"]));
    let already = &out["already_published"][0];
    assert_eq!(already["push"]["kind"], "Pushed", "{out}");
    assert_eq!(already["landed_later"], true, "{out}");
    let e = entry(&out, &a);
    assert_eq!(e["reasons"], json!(["unpublished"]), "{out}");
    assert_eq!(
        e["detail"]["landing"]["tool"]["why_none"], "no_built_base",
        "{out}"
    );
    assert_eq!(out["closed"], false, "{out}");
    assert_eq!(lifecycle(&core, &a), Lifecycle::Active);
}

#[test]
fn refused_publish_pushed_by_a_human_closes_once_a_new_built_the_base() {
    let fx = tool_fixture();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let a = refused_then_pushed(&fx, &server, &core);
    let _b = close::new_only(&server, &["tool"]);
    let out = ok(close::publish(&server, &core, &a, "tool", &["new.txt"]));
    assert_eq!(
        out["already_published"][0]["push"]["kind"], "Pushed",
        "{out}"
    );
    assert_eq!(out["closed"], true, "{out}");
    assert_eq!(lifecycle(&core, &a), Lifecycle::Removed);
}
