//! Auto-close §10 row 21: assignment reads and the assignee's queue place.
//! All repositories and conflict records are created in Dfx's temp sandbox.
mod support;

use std::fs;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};
use strata_daemon::handlers::{Server, Trackers};
use strata_daemon::startup::{Core, Daemon};
use strata_session::SessionSeam;
use strata_store::{Lifecycle, PublishPhase};
use strata_types::{MarkByKind, SessionId};
use support::close::{self, call, ok};
use support::*;

const CONFLICT: &str = "src/a.ts";
const WAIT: Duration = Duration::from_secs(10);

// A failed assertion/receive must not leave a scoped publisher at the seam.
struct ReleasePause<'a, 'core>(&'a strata_session::Sessions<'core>);

impl Drop for ReleasePause<'_, '_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

fn fixture(names: &[&str]) -> Dfx {
    let mut fx = close::opted_in(names);
    fx.lock_wait = Duration::from_secs(60);
    fx.catch_up_every = Duration::from_secs(86400);
    fx.write_config();
    fx
}

fn publish_group(core: &Core, s: &SessionId, repo: &str, path: &str) -> Value {
    let cp = checkpoint(core, s, repo);
    json!({"group": group(vec![batch(s, repo, cp.id, &[path])]), "shown": {}})
}

// A really lands and closes; B really conflicts against it. Do not seed a
// synthetic assignment row: the handler must find the repo of the failed row.
fn conflict(server: &Server<'_, '_>, core: &Core, repo: &str) -> (u64, SessionId, Value) {
    let a = close::new_only(server, &[repo]);
    let b = close::new_only(server, &[repo]);
    fs::write(ws(core, &a, repo).join(CONFLICT), "export const a = 8;\n").unwrap();
    fs::write(ws(core, &b, repo).join(CONFLICT), "export const a = 9;\n").unwrap();
    let ga = publish_group(core, &a, repo, CONFLICT);
    let gb = publish_group(core, &b, repo, CONFLICT);
    let landed = ok(call(server, "publish", ga));
    assert_eq!(landed["closed"], true, "{landed}");
    assert_eq!(
        core.store.session(&a).unwrap().unwrap().lifecycle,
        Lifecycle::Removed
    );
    let failed = call(server, "publish", gb.clone());
    assert_eq!(failed["error"]["code"], "Conflict", "{failed}");
    let p = failed["error"]["detail"]["publish"]
        .as_u64()
        .expect("conflict publish id");
    let outcome = core
        .store
        .publish_record(p)
        .unwrap()
        .unwrap()
        .outcome
        .unwrap();
    assert!(
        outcome["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["path"] == CONFLICT),
        "{outcome}"
    );
    (p, b, gb)
}

fn assign(server: &Server<'_, '_>, p: u64, s: &SessionId) -> Value {
    call(
        server,
        "assign",
        json!({"group": p, "path": CONFLICT, "session": s}),
    )
}

fn outcome(core: &Core, p: u64) -> Value {
    core.store
        .publish_record(p)
        .unwrap()
        .unwrap()
        .outcome
        .unwrap()
}

fn unassigned(core: &Core, p: u64, s: &SessionId) {
    assert!(
        outcome(core, p).get("assigned").is_none(),
        "{}",
        outcome(core, p)
    );
    assert!(strata_publish::assigned_to(&core.store.publishes().unwrap(), s).is_empty());
}

fn entry<'a>(out: &'a Value, s: &SessionId) -> &'a Value {
    &out["auto_close"]["sessions"][s.as_str()]
}

fn assigned_hold(core: &Core, out: &Value, s: &SessionId, repo: &str) {
    let e = entry(out, s);
    assert_eq!(e["closed"], false, "{out}");
    assert_eq!(e["reasons"], json!(["assigned"]), "{out}");
    assert_eq!(e["detail"]["repo"], repo, "{out}");
    assert_eq!(e["detail"]["paths"], json!([CONFLICT]), "{out}");
    assert_eq!(e["detail"]["assigned"][repo], json!([CONFLICT]), "{out}");
    assert_eq!(
        core.store.session(s).unwrap().unwrap().lifecycle,
        Lifecycle::Active
    );
    assert_eq!(close::mark_of(core, s).unwrap().by.kind, MarkByKind::Daemon);
}

fn ready_to_publish(server: &Server<'_, '_>, core: &Core) -> (SessionId, Value) {
    let s = close::new_only(server, &["tool"]);
    fs::write(
        ws(core, &s, "tool").join("src/s.ts"),
        "export const s = 1;\n",
    )
    .unwrap();
    let g = publish_group(core, &s, "tool", "src/s.ts");
    (s, g)
}

fn until(mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        if predicate() {
            return true;
        }
        thread::sleep(Duration::from_millis(5));
    }
    false
}

#[test]
fn assign_before_the_judge_holds_the_close() {
    let fx = fixture(&["tool"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let (p, b, gb) = conflict(&server, &core, "tool");
    let (s, g) = ready_to_publish(&server, &core);
    d.sessions.arm(SessionSeam::PauseAt {
        phase: "auto_close.before_judge".into(),
    });
    let (paused, assignment, out) = thread::scope(|scope| {
        let publisher = scope.spawn(|| call(&server, "publish", g));
        let _release_on_unwind = ReleasePause(&d.sessions);
        let paused = d.sessions.wait_paused_timeout(WAIT);
        let assignment = if paused {
            Some(assign(&server, p, &s))
        } else {
            None
        };
        d.sessions.release();
        (paused, assignment, publisher.join().unwrap())
    });
    assert!(paused);
    assert_eq!(ok(assignment.unwrap()), json!({}));
    assigned_hold(&core, &ok(out), &s, "tool");
    let retry = call(&server, "publish", gb);
    assert_eq!(retry["error"]["code"], "Conflict", "{retry}");
    assert_eq!(retry["error"]["detail"]["reason"], "assigned", "{retry}");
    assert_eq!(
        retry["error"]["detail"]["conflicts"][0]["assigned"],
        s.as_str(),
        "{retry}"
    );
    assert_eq!(
        retry["error"]["detail"]["conflicts"][0]["session"],
        b.as_str(),
        "{retry}"
    );
}

fn during_close(short_wait: bool) {
    let mut fx = fixture(&["tool"]);
    if short_wait {
        fx.lock_wait = Duration::from_millis(300);
    }
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let (p, _, gb) = conflict(&server, &core, "tool");
    let (s, g) = ready_to_publish(&server, &core);
    let before = outcome(&core, p);
    d.sessions.arm(SessionSeam::PauseAt {
        phase: "remove.after_recheck".into(),
    });
    let (paused, queued, pending, unchanged, published, assignment) = thread::scope(|scope| {
        let publisher = scope.spawn(|| call(&server, "publish", g));
        let _release_on_unwind = ReleasePause(&d.sessions);
        let paused = d.sessions.wait_paused_timeout(WAIT);
        let (tx, rx) = mpsc::channel();
        let server_ref = &server;
        let session_ref = &s;
        let assignee = scope.spawn(move || {
            tx.send(assign(server_ref, p, session_ref)).unwrap();
        });
        let queued = until(|| core.locks.queued(&s) > 0);
        let early = if short_wait {
            Some(rx.recv_timeout(WAIT).unwrap())
        } else {
            None
        };
        let pending = if short_wait {
            true
        } else {
            matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty))
        };
        let unchanged = outcome(&core, p) == before;
        d.sessions.release();
        let published = publisher.join().unwrap();
        let assignment = early.unwrap_or_else(|| rx.recv_timeout(WAIT).unwrap());
        assignee.join().unwrap();
        (paused, queued, pending, unchanged, published, assignment)
    });
    assert!(
        paused && queued && pending && unchanged,
        "pause={paused}, queued={queued}, pending={pending}, unchanged={unchanged}"
    );
    let out = ok(published);
    assert_eq!(entry(&out, &s)["closed"], true, "{out}");
    assert_eq!(
        core.store.session(&s).unwrap().unwrap().lifecycle,
        Lifecycle::Removed
    );
    assert_eq!(
        assignment["error"]["code"],
        if short_wait { "Busy" } else { "NoSuchSession" },
        "{assignment}"
    );
    if short_wait {
        assert_eq!(
            assignment["error"]["detail"]["reason"], "lock_wait",
            "{assignment}"
        );
    }
    assert_eq!(outcome(&core, p), before);
    unassigned(&core, p, &s);
    let retry = call(&server, "publish", gb);
    assert_ne!(retry["error"]["detail"]["reason"], "assigned", "{retry}");
}

#[test]
fn assign_during_the_close_waits_and_writes_nothing() {
    during_close(false);
}

#[test]
fn assign_during_the_close_can_time_out_without_writing() {
    during_close(true);
}

/// §10 row 21: agent's `rm {discard: false}` at the same `remove.after_recheck`
/// pause as `assign_during_the_close_waits_and_writes_nothing` — waits, writes
/// nothing, then answers `NoSuchSession` (or `Busy`/`lock_wait` under a short
/// `lock_wait`) after the close finishes.
fn during_close_agent_rm(short_wait: bool) {
    let mut fx = fixture(&["tool"]);
    if short_wait {
        fx.lock_wait = Duration::from_millis(300);
    }
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let (s, g) = ready_to_publish(&server, &core);
    let attempts_before = core.store.rm_attempts(Some(&s)).unwrap().len();
    d.sessions.arm(SessionSeam::PauseAt {
        phase: "remove.after_recheck".into(),
    });
    let (paused, queued, pending, still_active, published, removed) = thread::scope(|scope| {
        let publisher = scope.spawn(|| call(&server, "publish", g));
        let _release_on_unwind = ReleasePause(&d.sessions);
        let paused = d.sessions.wait_paused_timeout(WAIT);
        let (tx, rx) = mpsc::channel();
        let server_ref = &server;
        let session_ref = &s;
        let remover = scope.spawn(move || {
            tx.send(call(
                server_ref,
                "rm",
                json!({"session": session_ref, "discard": false}),
            ))
            .unwrap();
        });
        let queued = until(|| core.locks.queued(&s) > 0);
        let early = if short_wait {
            Some(rx.recv_timeout(WAIT).unwrap())
        } else {
            None
        };
        let pending = if short_wait {
            true
        } else {
            matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty))
        };
        let still_active = core.store.session(&s).unwrap().unwrap().lifecycle == Lifecycle::Active;
        d.sessions.release();
        let published = publisher.join().unwrap();
        let removed = early.unwrap_or_else(|| rx.recv_timeout(WAIT).unwrap());
        remover.join().unwrap();
        (paused, queued, pending, still_active, published, removed)
    });
    assert!(
        paused && queued && pending && still_active,
        "pause={paused}, queued={queued}, pending={pending}, still_active={still_active}"
    );
    let out = ok(published);
    assert_eq!(entry(&out, &s)["closed"], true, "{out}");
    assert_eq!(
        core.store.session(&s).unwrap().unwrap().lifecycle,
        Lifecycle::Removed
    );
    assert_eq!(
        removed["error"]["code"],
        if short_wait { "Busy" } else { "NoSuchSession" },
        "{removed}"
    );
    if short_wait {
        assert_eq!(
            removed["error"]["detail"]["reason"], "lock_wait",
            "{removed}"
        );
    }
    // Agent's rm wrote nothing: no attempt row, and the close (not the agent)
    // is what Removed the session.
    assert_eq!(
        core.store.rm_attempts(Some(&s)).unwrap().len(),
        attempts_before,
        "agent rm must not record an attempt"
    );
}

#[test]
fn agent_rm_during_the_close_waits_and_writes_nothing() {
    during_close_agent_rm(false);
}

#[test]
fn agent_rm_during_the_close_can_time_out_without_writing() {
    during_close_agent_rm(true);
}

#[test]
fn assign_waits_then_lands_when_the_close_refuses() {
    let fx = fixture(&["tool"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let (p, _, _) = conflict(&server, &core, "tool");
    let (s, g) = ready_to_publish(&server, &core);
    fs::write(
        ws(&core, &s, "tool").join("src/remaining.ts"),
        "export const remaining = 1;\n",
    )
    .unwrap();
    d.sessions.arm(SessionSeam::PauseAt {
        phase: "auto_close.after_reread".into(),
    });
    let (paused, queued, pending, unchanged, out, assignment) = thread::scope(|scope| {
        let publisher = scope.spawn(|| call(&server, "publish", g));
        let _release_on_unwind = ReleasePause(&d.sessions);
        let paused = d.sessions.wait_paused_timeout(WAIT);
        let (tx, rx) = mpsc::channel();
        let server_ref = &server;
        let session_ref = &s;
        let assignee = scope.spawn(move || {
            tx.send(assign(server_ref, p, session_ref)).unwrap();
        });
        let queued = until(|| core.locks.queued(&s) > 0);
        let pending = matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty));
        let unchanged = outcome(&core, p).get("assigned").is_none();
        d.sessions.release();
        let out = publisher.join().unwrap();
        let assignment = rx.recv_timeout(WAIT).unwrap();
        assignee.join().unwrap();
        (paused, queued, pending, unchanged, out, assignment)
    });
    assert!(paused && queued && pending && unchanged);
    let out = ok(out);
    assert_eq!(entry(&out, &s)["reasons"], json!(["unpublished"]), "{out}");
    assert_eq!(
        core.store.session(&s).unwrap().unwrap().lifecycle,
        Lifecycle::Active
    );
    assert_eq!(ok(assignment), json!({}));
    // Remove only the deliberately unpublished edit so the next judge can
    // isolate assignment as the remaining reason, not unpublished+assigned.
    fs::remove_file(ws(&core, &s, "tool").join("src/remaining.ts")).unwrap();
    let t = close::new_only(&server, &["tool"]);
    fs::write(
        ws(&core, &t, "tool").join("src/t.ts"),
        "export const t = 1;\n",
    )
    .unwrap();
    let next = ok(close::publish(&server, &core, &t, "tool", &["src/t.ts"]));
    assert!(
        next["auto_close"]["rejudged"]
            .as_array()
            .unwrap()
            .contains(&json!(s)),
        "{next}"
    );
    assigned_hold(&core, &next, &s, "tool");
}

#[test]
fn assign_refuses_a_session_leaving_for_removal() {
    let fx = fixture(&["tool"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let (p, _, _) = conflict(&server, &core, "tool");
    let s = close::new_only(&server, &["tool"]);
    let before = outcome(&core, p);
    assert!(
        core.store
            .set_lifecycle(
                &s,
                Lifecycle::Active,
                Lifecycle::Removing,
                None,
                SystemTime::now()
            )
            .unwrap()
    );
    let refused = assign(&server, p, &s);
    assert_eq!(refused["error"]["code"], "InvalidInput", "{refused}");
    assert_eq!(
        refused["error"]["detail"]["reason"], "lifecycle",
        "{refused}"
    );
    assert_eq!(refused["error"]["detail"]["state"], "removing", "{refused}");
    assert_eq!(outcome(&core, p), before);
    assert!(
        core.store
            .set_lifecycle(
                &s,
                Lifecycle::Removing,
                Lifecycle::Removed,
                None,
                SystemTime::now()
            )
            .unwrap()
    );
    let gone = assign(&server, p, &s);
    assert_eq!(gone["error"]["code"], "NoSuchSession", "{gone}");
    assert_eq!(outcome(&core, p), before);
    unassigned(&core, p, &s);
}

#[test]
fn assign_holds_the_queue_while_it_waits_for_the_repo() {
    let fx = fixture(&["tool"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let (p, _, _) = conflict(&server, &core, "tool");
    let s = close::new_only(&server, &["tool"]);
    let (held_queue, waited_repo, unchanged, assignment) = thread::scope(|scope| {
        let (held_tx, held_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let core_ref = &core;
        let holder = scope.spawn(move || {
            core_ref
                .locks
                .with_repo(&repo_name("tool"), || {
                    held_tx.send(()).unwrap();
                    release_rx.recv_timeout(WAIT).unwrap();
                    Ok(())
                })
                .unwrap()
        });
        held_rx.recv_timeout(WAIT).unwrap();
        let assignee = scope.spawn(|| assign(&server, p, &s));
        let held_queue = until(|| {
            core.locks
                .try_session(&s, "row21-probe", || Ok(()))
                .is_none()
        });
        let waited_repo = until(|| core.locks.repo_waiting(&repo_name("tool")) > 0);
        let unchanged = outcome(&core, p).get("assigned").is_none();
        release_tx.send(()).unwrap();
        holder.join().unwrap();
        (held_queue, waited_repo, unchanged, assignee.join().unwrap())
    });
    assert!(held_queue && waited_repo && unchanged);
    assert_eq!(ok(assignment), json!({}));
    assert_eq!(outcome(&core, p)["assigned"][0]["session"], s.as_str());
}

#[test]
fn assigned_to_reads_every_repo_of_the_rows() {
    let fx = fixture(&["front", "tool"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let (p, other, _) = conflict(&server, &core, "front");
    let s = close::new_only(&server, &["tool"]);
    let row = core.store.publish_record(p).unwrap().unwrap();
    assert!(strata_publish::assigned_to(std::slice::from_ref(&row), &s).is_empty());
    ok(assign(&server, p, &s));
    let row = core.store.publish_record(p).unwrap().unwrap();
    assert_eq!(
        strata_publish::assigned_to(std::slice::from_ref(&row), &s),
        [(repo_name("front"), vec![CONFLICT.to_string()])].into()
    );
    assert!(strata_publish::assigned_to(std::slice::from_ref(&row), &other).is_empty());
    // Pure read over a second outcome repo: neither manifest membership nor
    // the group batches may narrow which outcome repos are read.
    let mut tool = row.clone();
    tool.group.id += 1;
    tool.outcome.as_mut().unwrap()["repo"] = json!("tool");
    assert_eq!(
        strata_publish::assigned_to(&[row.clone(), tool], &s).len(),
        2
    );
    let mut landed = row.clone();
    landed.group.id += 2;
    landed.phase = PublishPhase::Done;
    landed.ended = Some(SystemTime::now() + Duration::from_secs(1));
    landed.outcome = Some(json!({"repo": "front", "paths": [CONFLICT], "commit": "landed"}));
    assert!(strata_publish::assigned_to(&[row.clone(), landed], &s).is_empty());
    let mut reassigned = row.clone();
    reassigned.outcome.as_mut().unwrap()["assigned"][0]["session"] = json!(other);
    assert!(strata_publish::assigned_to(&[row, reassigned], &s).is_empty());
}

#[test]
fn assignment_in_a_repo_the_session_does_not_hold_holds_the_close() {
    let fx = fixture(&["front", "tool"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let (p, _, _) = conflict(&server, &core, "front");
    let (s, g) = ready_to_publish(&server, &core);
    assert!(
        !core
            .store
            .session(&s)
            .unwrap()
            .unwrap()
            .manifest
            .repos
            .contains_key(&repo_name("front"))
    );
    ok(assign(&server, p, &s));
    let out = ok(call(&server, "publish", g));
    assigned_hold(&core, &out, &s, "front");
}

#[test]
fn assignment_in_front_and_tool_lists_every_repo_in_detail() {
    // §10 row 22 nit 6: row 21's one-repo daemon scene plus a second
    // assignment to S in tool. The formatter unit alone is not enough.
    let fx = fixture(&["front", "tool"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let (p_front, _, _) = conflict(&server, &core, "front");
    let (p_tool, _, _) = conflict(&server, &core, "tool");
    let (s, g) = ready_to_publish(&server, &core);
    assert!(
        !core
            .store
            .session(&s)
            .unwrap()
            .unwrap()
            .manifest
            .repos
            .contains_key(&repo_name("front"))
    );
    ok(assign(&server, p_front, &s));
    ok(assign(&server, p_tool, &s));
    let out = ok(call(&server, "publish", g));
    let e = entry(&out, &s);
    assert_eq!(e["closed"], false, "{out}");
    assert_eq!(e["reasons"], json!(["assigned"]), "{out}");
    assert_eq!(e["detail"]["repo"], "front", "{out}");
    assert_eq!(e["detail"]["paths"], json!([CONFLICT]), "{out}");
    assert_eq!(
        e["detail"]["assigned"]["front"],
        json!([CONFLICT]),
        "{out}"
    );
    assert_eq!(
        e["detail"]["assigned"]["tool"],
        json!([CONFLICT]),
        "{out}"
    );
    assert_eq!(
        core.store.session(&s).unwrap().unwrap().lifecycle,
        Lifecycle::Active
    );
    let mark = close::mark_of(&core, &s).unwrap();
    assert_eq!(mark.by.kind, MarkByKind::Daemon);
    let note = mark.note.as_deref().unwrap_or("");
    assert!(
        note.contains(&format!("front: {CONFLICT} is assigned to this session")),
        "{note}"
    );
    assert!(
        note.contains(&format!("tool: {CONFLICT} is assigned to this session")),
        "{note}"
    );
}

fn disable_during_publish(per_repo: bool) {
    let mut fx = fixture(&["tool"]);
    fx.config_poll_every = Duration::ZERO;
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let (s, g) = ready_to_publish(&server, &core);
    d.sessions.arm(SessionSeam::PauseAt {
        phase: "auto_close.before_judge".into(),
    });
    let (paused, reload, out) = thread::scope(|scope| {
        let publisher = scope.spawn(|| call(&server, "publish", g));
        let _release_on_unwind = ReleasePause(&d.sessions);
        let paused = d.sessions.wait_paused_timeout(WAIT);
        if per_repo {
            fx.config
                .repos
                .get_mut(&repo_name("tool"))
                .unwrap()
                .publish
                .auto_close = Some(false);
        } else {
            fx.config.publish.auto_close = false;
        }
        fx.write_config();
        // These in-process fixtures have no serve loop to process a
        // config_reload ticket; use the same application path directly.
        let reload = core.reload_for_test();
        d.sessions.release();
        (paused, reload, publisher.join().unwrap())
    });
    assert!(paused);
    assert_eq!(reload.outcome, "applied");
    let out = ok(out);
    assert_eq!(entry(&out, &s)["closed"], false, "{out}");
    assert_eq!(entry(&out, &s)["reasons"], json!(["disabled"]), "{out}");
    assert!(close::mark_of(&core, &s).is_none());
    assert!(close::audit_lines(&fx).is_empty());
    assert_eq!(
        core.store.session(&s).unwrap().unwrap().lifecycle,
        Lifecycle::Active
    );
}

#[test]
fn false_set_during_the_publish_disables_the_close() {
    disable_during_publish(false);
}

#[test]
fn repo_false_set_during_the_publish_disables_the_close() {
    disable_during_publish(true);
}

#[test]
fn true_set_during_the_publish_waits_for_the_next_request() {
    let mut fx = fixture(&["tool"]);
    fx.config.publish.auto_close = false;
    fx.config_poll_every = Duration::ZERO;
    fx.write_config();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let (s, g) = ready_to_publish(&server, &core);
    d.sessions.arm(SessionSeam::PauseAt {
        phase: "auto_close.after_enumerate".into(),
    });
    let (paused, reload, out) = thread::scope(|scope| {
        let publisher = scope.spawn(|| call(&server, "publish", g));
        let _release_on_unwind = ReleasePause(&d.sessions);
        let paused = d.sessions.wait_paused_timeout(WAIT);
        fx.config.publish.auto_close = true;
        fx.write_config();
        let reload = core.reload_for_test();
        d.sessions.release();
        (paused, reload, publisher.join().unwrap())
    });
    assert!(paused);
    assert_eq!(reload.outcome, "applied");
    let out = ok(out);
    assert_eq!(entry(&out, &s)["reasons"], json!(["disabled"]), "{out}");
    assert!(close::mark_of(&core, &s).is_none());
    assert!(close::audit_lines(&fx).is_empty());
    assert_eq!(
        core.store.session(&s).unwrap().unwrap().lifecycle,
        Lifecycle::Active
    );
    fs::write(
        ws(&core, &s, "tool").join("src/next.ts"),
        "export const next = 1;\n",
    )
    .unwrap();
    let next = ok(close::publish(&server, &core, &s, "tool", &["src/next.ts"]));
    assert_eq!(entry(&next, &s)["closed"], true, "{next}");
    assert_eq!(
        core.store.session(&s).unwrap().unwrap().lifecycle,
        Lifecycle::Removed
    );
}

#[test]
fn add_of_an_opted_in_repo_during_the_hook_is_extended() {
    let fx = fixture(&["front", "tool"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let (s, g) = ready_to_publish(&server, &core);
    d.sessions.arm(SessionSeam::PauseAt {
        phase: "auto_close.before_judge".into(),
    });
    let (paused, added, out) = thread::scope(|scope| {
        let publisher = scope.spawn(|| call(&server, "publish", g));
        let _release_on_unwind = ReleasePause(&d.sessions);
        let paused = d.sessions.wait_paused_timeout(WAIT);
        let added = call(&server, "add", json!({"session": s, "repos": ["front"]}));
        d.sessions.release();
        (paused, added, publisher.join().unwrap())
    });
    assert!(paused);
    ok(added);
    let out = ok(out);
    let e = entry(&out, &s);
    assert_eq!(e["closed"], false, "{out}");
    assert_eq!(e["reasons"], json!(["extended"]), "{out}");
    assert_eq!(e["detail"]["repos"], json!(["front"]), "{out}");
    assert_eq!(e["mark"]["skipped"], "extended", "{out}");
    assert!(close::mark_of(&core, &s).is_none());
    assert_eq!(close::audit_lines(&fx).len(), 1);
    assert_eq!(
        core.store.session(&s).unwrap().unwrap().lifecycle,
        Lifecycle::Active
    );
    for repo in ["front", "tool"] {
        assert!(ws(&core, &s, repo).is_dir());
        assert!(
            core.gits[&repo_name(repo)]
                .read_ref(&format!("refs/strata/pins/{s}"))
                .unwrap()
                .is_some()
        );
    }
    fs::write(
        ws(&core, &s, "tool").join("src/next.ts"),
        "export const next = 1;\n",
    )
    .unwrap();
    let next = ok(close::publish(&server, &core, &s, "tool", &["src/next.ts"]));
    assert_eq!(entry(&next, &s)["closed"], true, "{next}");
    assert_eq!(
        core.store.session(&s).unwrap().unwrap().lifecycle,
        Lifecycle::Removed
    );
}
