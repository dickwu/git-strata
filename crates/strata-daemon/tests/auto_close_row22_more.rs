//! Auto-close §10 row 22 (r9 nits 7 and 9): push rejudge with only an
//! extended session, and close reuses clause 2's upstream read. Temp-dir
//! fixtures only.
mod support;

use std::fs;
use std::time::Duration;

use serde_json::{Value, json};
use strata_daemon::handlers::{Server, Trackers};
use strata_daemon::startup::{Core, Daemon};
use strata_session::SessionSeam;
use strata_store::Lifecycle;
use strata_types::{MarkByKind, SessionId};
use support::close::{self, call, mark_of, ok};
use support::*;

fn entry<'a>(out: &'a Value, s: &SessionId) -> &'a Value {
    &out["auto_close"]["sessions"][s.as_str()]
}

fn lifecycle(core: &Core, s: &SessionId) -> Lifecycle {
    core.store.session(s).unwrap().unwrap().lifecycle
}

/// `front` and `tool`, auto-close on for both (front not opted out).
fn both_on(tool_push: bool, front_push: bool) -> Dfx {
    let mut fx = close::opted_in(&["front", "tool"]);
    fx.catch_up_every = Duration::from_secs(86400);
    fx.lock_wait = Duration::from_secs(60);
    fx.config
        .repos
        .get_mut(&repo_name("front"))
        .unwrap()
        .publish
        .push = front_push;
    fx.config
        .repos
        .get_mut(&repo_name("tool"))
        .unwrap()
        .publish
        .push = tool_push;
    fx.write_config();
    fx
}

fn add_front(server: &Server<'_, '_>, s: &SessionId) -> Value {
    call(server, "add", json!({"session": s, "repos": ["front"]}))
}

fn assert_added(reply: &Value) {
    let data = ok(reply.clone());
    assert!(!data["report"]["added"]["front"].is_null(), "{data}");
}

fn tool_only() -> Dfx {
    let mut fx = close::opted_in(&["tool"]);
    fx.catch_up_every = Duration::from_secs(86400);
    fx.lock_wait = Duration::from_secs(60);
    fx.write_config();
    fx
}

#[test]
fn push_with_only_an_extended_session_has_auto_close() {
    // §10 row 22 nit 7: row 20's push_rejudge_after_add_of_opted_out_repo_is_disabled
    // with front not opted out → extended, not disabled.
    let fx = both_on(false, true);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_only(&server, &["tool"]);
    fs::write(ws(&core, &s, "tool").join("t.txt"), "t\n").unwrap();
    let first = ok(close::publish(&server, &core, &s, "tool", &["t.txt"]));
    assert_eq!(
        entry(&first, &s)["reasons"],
        json!(["not_pushed"]),
        "{first}"
    );
    ok(call(
        &server,
        "mark",
        json!({"session": s, "state": "active"}),
    ));
    ok(call(
        &server,
        "mark",
        json!({"session": s, "state": "done", "agent": "codex"}),
    ));
    let at0 = mark_of(&core, &s).unwrap();
    assert_eq!(at0.by.kind, MarkByKind::Agent);
    d.sessions.arm(SessionSeam::PauseAt {
        phase: "auto_close.before_judge".into(),
    });
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
    assert_eq!(
        entry(&pushed, &s)["reasons"],
        json!(["extended"]),
        "{pushed}"
    );
    assert_eq!(pushed["auto_close"]["rejudged"], json!([]), "{pushed}");
    assert_eq!(lifecycle(&core, &s), Lifecycle::Active);
    assert_eq!(mark_of(&core, &s).unwrap().at, at0.at);
}

#[test]
fn close_uses_clause_2s_upstream_read() {
    // §10 row 22 nit 9: fail upstream only after after_clauses; the close still
    // carries clause 2's read (as_of_rm before, unpublished false).
    let fx = tool_only();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);
    fs::write(ws(&core, &s, "tool").join("t.txt"), "t\n").unwrap();
    let cp = checkpoint(&core, &s, "tool");
    let g = group(vec![batch(&s, "tool", cp.id, &["t.txt"])]);
    d.sessions.arm(SessionSeam::PauseAt {
        phase: "auto_close.after_clauses".into(),
    });
    let out = std::thread::scope(|sc| {
        let h = sc.spawn(|| call(&server, "publish", json!({"group": g, "shown": {}})));
        d.sessions.wait_paused();
        core.upstream.fail_for_test(true);
        d.sessions.release();
        let reply = h.join().unwrap();
        core.upstream.fail_for_test(false);
        ok(reply)
    });
    assert_eq!(out["closed"], true, "{out}");
    assert_eq!(out["upstream"][s.as_str()]["as_of_rm"], "before", "{out}");
    assert_eq!(
        out["upstream"][s.as_str()]["repos"]["tool"]["unpublished"],
        false,
        "{out}"
    );
    assert_eq!(lifecycle(&core, &s), Lifecycle::Removed);
}
