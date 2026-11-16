//! Status-bar §3.1, §11 and §12 (P-b slice A) over `Server::handle` and
//! `Dfx`: whose a repo lock hold is (`holds[].origin`, the session it works
//! on), the long work in flight (`daemon_status.activity`) and the
//! `LongHold` rule built on both. Every repo is a Dfx sandbox; work is
//! parked at session and publish seams, never slept out.
mod support;

use std::fs;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};
use strata_daemon::handlers::{Server, Trackers, catch_up_pass};
use strata_daemon::locks::BACKGROUND_HOLD_MAX;
use strata_daemon::startup::{Core, Daemon};
use strata_publish::seams::{PublishFault, pause};
use strata_session::SessionSeam;
use strata_types::CommitOid;
use support::close::{self, call, ok};
use support::*;

const WAIT: Duration = Duration::from_secs(10);

/// Releases a session seam's pause even when an assertion fails first, so
/// no scoped thread is left parked: a pause not reached yet is disarmed,
/// and one reached is let go.
struct ReleasePause<'a, 'core>(&'a strata_session::Sessions<'core>);

impl Drop for ReleasePause<'_, '_> {
    fn drop(&mut self) {
        self.0.arm(SessionSeam::PauseAt {
            phase: "disarmed".into(),
        });
        self.0.release();
        // A thread that took the seam just before the disarm parks after
        // that release.
        if self.0.wait_paused_timeout(Duration::from_millis(100)) {
            self.0.release();
        }
    }
}

/// Wait for the armed session pause. One that never came is disarmed, so a
/// late arrival cannot park a thread the test is about to join.
fn paused(d: &Daemon<'_>) -> bool {
    let paused = d.sessions.wait_paused_timeout(WAIT);
    if !paused {
        d.sessions.arm(SessionSeam::PauseAt {
            phase: "disarmed".into(),
        });
    }
    paused
}

fn status(server: &Server<'_, '_>) -> Value {
    ok(call(server, "daemon_status", json!({})))
}

/// `daemon_status` until `done` holds of it or [`WAIT`] passes: the last reply.
fn status_until(server: &Server<'_, '_>, done: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + WAIT;
    loop {
        let st = status(server);
        if done(&st) || Instant::now() >= deadline {
            return st;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn hold<'v>(st: &'v Value, repo: &str) -> Option<&'v Value> {
    st["holds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["repo"] == repo)
}

/// The hold of `repo` once it has lasted longer than `ms`.
fn held_over(st: &Value, repo: &str, ms: u128) -> bool {
    hold(st, repo).is_some_and(|h| h["elapsed_ms"].as_u64().is_some_and(|e| u128::from(e) > ms))
}

fn activity<'v>(st: &'v Value, kind: &str) -> Vec<&'v Value> {
    st["activity"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["kind"] == kind)
        .collect()
}

/// The bodies of the `LongHold` warnings.
fn long_holds(st: &Value) -> Vec<&Value> {
    st["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w.get("LongHold"))
        .collect()
}

/// A commit on `repo`'s main, pushed and fetched: the tracking ref moves on.
fn advance(fx: &Dfx, core: &Core, repo: &str, file: &str) -> CommitOid {
    let main = fx.main(repo);
    let head = fx
        .sb
        .commit(&main, &[(file, b"upstream\n".as_slice())], "upstream");
    fx.sb.git(&main, &["push", "-q", "origin", "main"]);
    core.gits[&repo_name(repo)].fetch("origin", "main").unwrap();
    CommitOid::try_from(head).unwrap()
}

fn head_of(core: &Core, s: &strata_types::SessionId, repo: &str) -> CommitOid {
    core.store.session(s).unwrap().unwrap().manifest.repos[&repo_name(repo)]
        .head
        .clone()
}

/// A catch-up move: the hold of `tool` is background work named for the
/// session it moves, one `move` activity shows its cause and phase, and
/// `daemon_status` answers at once while the move holds the locks.
#[test]
fn catch_up_move_hold_has_session_and_activity() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);
    let target = advance(&fx, &core, "tool", "src/next.ts");
    d.sessions.arm(SessionSeam::PauseAt {
        phase: "move.prepare".into(),
    });
    let (seen, pass) = thread::scope(|scope| {
        let pass = scope.spawn(|| catch_up_pass(&d, SystemTime::now() + Duration::from_secs(3600)));
        let _release = ReleasePause(&d.sessions);
        let seen = paused(&d).then(|| {
            let t = Instant::now();
            (status(&server), t.elapsed())
        });
        d.sessions.release();
        (seen, pass.join().unwrap())
    });
    let (st, answered_in) =
        seen.unwrap_or_else(|| panic!("the pass never reached the move: {pass:?}"));
    // The move holds the lock until released, after this reply: a status
    // that waited for it would not have answered. The bound is loose for a
    // loaded machine.
    assert!(
        answered_in < Duration::from_secs(5),
        "daemon_status waited {answered_in:?}"
    );
    let h = hold(&st, "tool").unwrap_or_else(|| panic!("{st}"));
    assert_eq!(
        (h["method"].as_str(), h["origin"].as_str()),
        (Some("catch_up"), Some("background")),
        "{st}"
    );
    assert_eq!(h["session"], s.as_str(), "the session it moves: {st}");
    assert!(h["elapsed_ms"].is_u64(), "{st}");
    let moves = activity(&st, "move");
    assert_eq!(moves.len(), 1, "{st}");
    let m = moves[0];
    assert_eq!(
        (
            m["cause"].as_str(),
            m["phase"].as_str(),
            m["origin"].as_str()
        ),
        (Some("catch_up"), Some("prepare"), Some("background")),
        "{st}"
    );
    assert_eq!(
        (m["repo"].as_str(), m["session"].as_str()),
        (Some("tool"), Some(s.as_str())),
        "{st}"
    );
    assert_eq!(
        m["step_elapsed_ms"], m["elapsed_ms"],
        "a move is one step: {st}"
    );
    assert!(
        m["id"].is_u64() && m["started"].as_str().is_some_and(|t| !t.is_empty()),
        "{st}"
    );
    assert!(long_holds(&st).is_empty(), "{st}");
    assert_eq!(head_of(&core, &s, "tool"), target, "{pass:?}");
    let after = status(&server);
    assert_eq!(
        (&after["activity"], &after["holds"]),
        (&json!([]), &json!([])),
        "{after}"
    );
}

/// A `sync` move: `sync_one`'s move and the session layer's are one
/// entry; the request's hold raises no `LongHold` past the lock wait while
/// the move's step is younger than the background max, and does once the
/// step is older.
#[test]
fn sync_move_hold_is_exempt_until_its_step_passes_the_max() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);
    let target = advance(&fx, &core, "tool", "src/next.ts");
    d.sessions.arm(SessionSeam::PauseAt {
        phase: "move.prepare".into(),
    });
    let (under, over, synced) = thread::scope(|scope| {
        let syncing =
            scope.spawn(|| call(&server, "sync", json!({ "session": s, "fetch": false })));
        let _release = ReleasePause(&d.sessions);
        let seen = paused(&d).then(|| {
            let under = status_until(&server, |st| {
                held_over(st, "tool", fx.lock_wait.as_millis() + 100)
            });
            core.locks.set_background_hold_max(Duration::from_millis(1));
            let over = status_until(&server, |st| !long_holds(st).is_empty());
            core.locks.set_background_hold_max(BACKGROUND_HOLD_MAX);
            (under, over)
        });
        d.sessions.release();
        let synced = syncing.join().unwrap();
        let (under, over) = seen.unwrap_or_else(|| panic!("sync never reached the move: {synced}"));
        (under, over, synced)
    });
    assert!(
        held_over(&under, "tool", fx.lock_wait.as_millis()),
        "{under}"
    );
    let h = hold(&under, "tool").unwrap();
    assert_eq!(
        (
            h["method"].as_str(),
            h["origin"].as_str(),
            h["session"].as_str()
        ),
        (Some("sync"), Some("request"), Some(s.as_str())),
        "{under}"
    );
    let moves = activity(&under, "move");
    assert_eq!(moves.len(), 1, "one move, not one per layer: {under}");
    assert_eq!(
        (
            moves[0]["cause"].as_str(),
            moves[0]["origin"].as_str(),
            moves[0]["session"].as_str()
        ),
        (Some("sync"), Some("request"), Some(s.as_str())),
        "{under}"
    );
    assert!(
        long_holds(&under).is_empty(),
        "the move's step is young: {under}"
    );
    let raised = long_holds(&over);
    assert_eq!(raised.len(), 1, "{over}");
    assert_eq!(
        (
            raised[0]["repo"].as_str(),
            raised[0]["method"].as_str(),
            raised[0]["origin"].as_str()
        ),
        (Some("tool"), Some("sync"), Some("request")),
        "{over}"
    );
    assert_eq!(raised[0]["session"], s.as_str(), "{over}");
    assert_eq!(ok(synced)["repos"]["tool"]["result"], "moved");
    assert_eq!(head_of(&core, &s, "tool"), target);
    assert_eq!(status(&server)["activity"], json!([]));
}

/// Q16: a publish's hold is never exempt. Past the lock wait it is
/// `LongHold`, named for the session it publishes, with the `publish`
/// activity in flight; its own post-publish move is cause `publish`.
#[test]
fn publish_hold_past_the_lock_wait_is_long_hold() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);
    fs::write(ws(&core, &s, "tool").join("a.txt"), "a\n").unwrap();
    let gap = pause(
        &core.gits[&repo_name("tool")],
        PublishFault::PauseBeforeCommit,
    );
    let (long, moving, out) = thread::scope(|scope| {
        let gap = gap;
        let publisher = scope.spawn(|| close::publish(&server, &core, &s, "tool", &["a.txt"]));
        let long = status_until(&server, |st| !long_holds(st).is_empty());
        // Armed while the publish is parked before its commit, so only the
        // move after the commit can reach it.
        d.sessions.arm(SessionSeam::PauseAt {
            phase: "move.prepare".into(),
        });
        let _release = ReleasePause(&d.sessions);
        drop(gap);
        let moving = paused(&d).then(|| status(&server));
        d.sessions.release();
        (long, moving, publisher.join().unwrap())
    });
    let raised = long_holds(&long);
    assert_eq!(raised.len(), 1, "{long}\n{out}");
    assert_eq!(
        (
            raised[0]["repo"].as_str(),
            raised[0]["method"].as_str(),
            raised[0]["origin"].as_str()
        ),
        (Some("tool"), Some("publish"), Some("request")),
        "{long}"
    );
    assert_eq!(raised[0]["session"], s.as_str(), "{long}");
    let h = hold(&long, "tool").unwrap();
    assert_eq!(
        (h["origin"].as_str(), h["session"].as_str()),
        (Some("request"), Some(s.as_str())),
        "{long}"
    );
    let publishing = activity(&long, "publish");
    assert_eq!(publishing.len(), 1, "{long}");
    let p = publishing[0];
    assert_eq!(
        (
            p["origin"].as_str(),
            p["repo"].as_str(),
            p["session"].as_str()
        ),
        (Some("request"), Some("tool"), Some(s.as_str())),
        "{long}"
    );
    assert_eq!(p["step_elapsed_ms"], p["elapsed_ms"], "{long}");
    let moving = moving.unwrap_or_else(|| panic!("the publish never moved its session: {out}"));
    let moves = activity(&moving, "move");
    assert_eq!(moves.len(), 1, "{moving}");
    assert_eq!(
        (
            moves[0]["cause"].as_str(),
            moves[0]["origin"].as_str(),
            moves[0]["session"].as_str()
        ),
        (Some("publish"), Some("request"), Some(s.as_str())),
        "{moving}"
    );
    assert!(
        activity(&moving, "publish").is_empty(),
        "its own step ended: {moving}"
    );
    ok(out);
    assert_eq!(status(&server)["activity"], json!([]));
}

/// The `LongHold` of `repo`, if raised.
fn long_hold<'v>(st: &'v Value, repo: &str) -> Option<&'v Value> {
    long_holds(st).into_iter().find(|w| w["repo"] == repo)
}

/// Status-bar §11.1: `new` and `add` take repo locks on their own thread
/// and on dispatch workers carrying only the request tag; either way the
/// hold is the request's (method `new`/`add`, the request's session), and
/// `session_create` shows the session being made with its step. The
/// main-follow pass after each is parked holding the repo lock: the session
/// creation has ended, so that hold is a request's like any other and
/// raises `LongHold` past the lock wait. (The creation's own observe step
/// also holds the lock, briefly; the waits below skip it by waiting for the
/// creation to end.)
#[test]
fn new_and_add_holds_are_their_requests() {
    let fx = Dfx::with(&["tool", "front"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    close::new_only(&server, &["tool"]);
    let adding = close::new_only(&server, &["tool"]);
    close::new_only(&server, &["front"]);
    advance(&fx, &core, "tool", "src/next.ts");
    advance(&fx, &core, "front", "src/next.ts");

    // `new`: parked at its enroll step, then in main-follow after it.
    let gap = pause(
        &core.gits[&repo_name("tool")],
        PublishFault::PauseBeforeMainTarget,
    );
    d.sessions.arm(SessionSeam::PauseAt {
        phase: "new.enroll".into(),
    });
    let (creating, held, made) = thread::scope(|scope| {
        let gap = gap;
        let maker = scope.spawn(|| {
            call(
                &server,
                "new",
                json!({ "ticket": null, "repos": ["tool"], "only": true }),
            )
        });
        let _release = ReleasePause(&d.sessions);
        let creating = paused(&d).then(|| status(&server));
        d.sessions.release();
        let held = status_until(&server, |st| {
            activity(st, "session_create").is_empty()
                && held_over(st, "tool", fx.lock_wait.as_millis() + 100)
        });
        drop(gap);
        (creating, held, maker.join().unwrap())
    });
    let made = ok(made);
    let id = made["session"]["manifest"]["id"].as_str().unwrap();
    let creating = creating.unwrap_or_else(|| panic!("new never reached its enroll step: {made}"));
    let c = activity(&creating, "session_create");
    assert_eq!(c.len(), 1, "{creating}");
    assert_eq!(
        (
            c[0]["origin"].as_str(),
            c[0]["step"].as_str(),
            c[0]["session"].as_str()
        ),
        (Some("request"), Some("enroll"), Some(id)),
        "{creating}"
    );
    assert_eq!(
        (&c[0]["repos"], &c[0]["repo"]),
        (&json!(["tool"]), &Value::Null),
        "{creating}"
    );
    let h = hold(&held, "tool").unwrap_or_else(|| panic!("{held}"));
    assert_eq!(
        (h["method"].as_str(), h["origin"].as_str()),
        (Some("new"), Some("request")),
        "{held}"
    );
    assert!(
        h["session"].is_null(),
        "`new` has no session parameter: {held}"
    );
    assert!(held_over(&held, "tool", fx.lock_wait.as_millis()), "{held}");
    assert!(activity(&held, "session_create").is_empty(), "{held}");
    let w = long_hold(&held, "tool").unwrap_or_else(|| panic!("{held}"));
    assert_eq!(
        (w["method"].as_str(), w["origin"].as_str()),
        (Some("new"), Some("request")),
        "{held}"
    );

    // `add`: parked at its build step, then in main-follow after it.
    let gap = pause(
        &core.gits[&repo_name("front")],
        PublishFault::PauseBeforeMainTarget,
    );
    d.sessions.arm(SessionSeam::PauseAt {
        phase: "add.build".into(),
    });
    let (creating, held, added) = thread::scope(|scope| {
        let gap = gap;
        let adder = scope.spawn(|| {
            call(
                &server,
                "add",
                json!({ "session": adding, "repos": ["front"] }),
            )
        });
        let _release = ReleasePause(&d.sessions);
        let creating = paused(&d).then(|| status(&server));
        d.sessions.release();
        let held = status_until(&server, |st| {
            activity(st, "session_create").is_empty()
                && held_over(st, "front", fx.lock_wait.as_millis() + 100)
        });
        drop(gap);
        (creating, held, adder.join().unwrap())
    });
    let added = ok(added);
    let creating = creating.unwrap_or_else(|| panic!("add never reached its build step: {added}"));
    let c = activity(&creating, "session_create");
    assert_eq!(c.len(), 1, "{creating}");
    assert_eq!(
        (
            c[0]["origin"].as_str(),
            c[0]["step"].as_str(),
            c[0]["session"].as_str()
        ),
        (Some("request"), Some("build"), Some(adding.as_str())),
        "{creating}"
    );
    assert_eq!(c[0]["repos"], json!(["front"]), "{creating}");
    let h = hold(&held, "front").unwrap_or_else(|| panic!("{held}"));
    assert_eq!(
        (
            h["method"].as_str(),
            h["origin"].as_str(),
            h["session"].as_str()
        ),
        (Some("add"), Some("request"), Some(adding.as_str())),
        "{held}"
    );
    assert!(
        held_over(&held, "front", fx.lock_wait.as_millis()),
        "{held}"
    );
    assert!(activity(&held, "session_create").is_empty(), "{held}");
    let w = long_hold(&held, "front").unwrap_or_else(|| panic!("{held}"));
    assert_eq!(
        (
            w["method"].as_str(),
            w["origin"].as_str(),
            w["session"].as_str()
        ),
        (Some("add"), Some("request"), Some(adding.as_str())),
        "{held}"
    );
    assert_eq!(status(&server)["activity"], json!([]));
}

/// Status-bar §11.2, §12.1: a hold `add` takes inside its own session
/// creation (its observe step records main's deps under the repo lock,
/// parked here at the record's compare-and-swap) is exempt: no `LongHold`
/// past the lock wait while the creation's step is younger than the
/// background max, and `LongHold` once it is older.
#[test]
fn add_hold_inside_its_session_creation_is_exempt() {
    let fx = Dfx::with(&["tool", "front"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let adding = close::new_only(&server, &["tool"]);
    let (parked_tx, parked_rx) = mpsc::channel::<()>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let (seen, added) = thread::scope(|scope| {
        let go_tx = go_tx;
        let adder = scope.spawn(|| {
            // This thread's first compare-and-swap of main's record only:
            // the main-follow after `add` runs on it too.
            let mut first = Some((parked_tx, go_rx));
            strata_deps::test_seams::before_main_cas(Box::new(move || {
                if let Some((parked, go)) = first.take() {
                    let _ = parked.send(());
                    let _ = go.recv_timeout(WAIT);
                }
            }));
            let out = call(
                &server,
                "add",
                json!({ "session": adding, "repos": ["front"] }),
            );
            strata_deps::test_seams::reset();
            out
        });
        let seen = parked_rx.recv_timeout(WAIT).is_ok().then(|| {
            let under = status_until(&server, |st| {
                held_over(st, "front", fx.lock_wait.as_millis() + 100)
            });
            core.locks.set_background_hold_max(Duration::from_millis(1));
            let over = status_until(&server, |st| !long_holds(st).is_empty());
            core.locks.set_background_hold_max(BACKGROUND_HOLD_MAX);
            (under, over)
        });
        drop(go_tx);
        (seen, adder.join().unwrap())
    });
    let (under, over) = seen.unwrap_or_else(|| panic!("add never reached main's record: {added}"));
    let h = hold(&under, "front").unwrap_or_else(|| panic!("{under}"));
    assert_eq!(
        (
            h["method"].as_str(),
            h["origin"].as_str(),
            h["session"].as_str()
        ),
        (Some("add"), Some("request"), Some(adding.as_str())),
        "{under}"
    );
    assert!(
        held_over(&under, "front", fx.lock_wait.as_millis()),
        "{under}"
    );
    let c = activity(&under, "session_create");
    assert_eq!(c.len(), 1, "{under}");
    assert_eq!(
        (c[0]["step"].as_str(), &c[0]["repos"]),
        (Some("observe"), &json!(["front"])),
        "{under}"
    );
    assert!(
        long_holds(&under).is_empty(),
        "the creation's step is young: {under}"
    );
    let w = long_hold(&over, "front").unwrap_or_else(|| panic!("{over}"));
    assert_eq!(
        (
            w["method"].as_str(),
            w["origin"].as_str(),
            w["session"].as_str()
        ),
        (Some("add"), Some("request"), Some(adding.as_str())),
        "{over}"
    );
    ok(added);
    assert_eq!(status(&server)["activity"], json!([]));
}

/// Status-bar §11.2: main-sync's dependency install shows as `deps_install`
/// (background, its proposal as a string) while it runs, holding no lock,
/// and is gone once it returns.
#[test]
fn deps_install_shows_while_it_runs() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    close::new(&server);
    let main = fx.main("tool");
    fx.sb
        .commit(&main, &[("bun.lock", b"lock v2\n".as_slice())], "deps");
    fx.sb.git(&main, &["push", "-q", "origin", "main"]);
    core.gits[&repo_name("tool")]
        .fetch("origin", "main")
        .unwrap();
    strata_daemon::main_sync::reconcile(&core, &repo_name("tool")).unwrap();
    let mut seen = None;
    strata_daemon::main_sync::deps_auto_pass_with(
        &core,
        &repo_name("tool"),
        &mut |ctx, id, dir| {
            seen = Some((id, status(&server)));
            strata_deps::ensure_target(ctx, id, dir)
        },
    )
    .unwrap();
    let (id, st) = seen.expect("the pass installed nothing");
    let installs = activity(&st, "deps_install");
    assert_eq!(installs.len(), 1, "{st}");
    let i = installs[0];
    assert_eq!(
        (
            i["origin"].as_str(),
            i["repo"].as_str(),
            i["proposal"].as_str()
        ),
        (
            Some("background"),
            Some("tool"),
            Some(id.get().to_string().as_str())
        ),
        "{st}"
    );
    assert!(i["session"].is_null(), "{st}");
    assert_eq!(st["holds"], json!([]), "an install holds no lock: {st}");
    assert_eq!(status(&server)["activity"], json!([]));
}

/// Status-bar §12.1: `deps approve` holds the repo lock for its rollout;
/// `deps_rollout` counts its sessions, names the one it moves, and keeps
/// the request's hold out of `LongHold` past the lock wait.
#[test]
fn deps_rollout_parents_its_moves() {
    let fx = Dfx::new();
    let (confirmer, _, _) = fx.confirmer(vec![strata_confirm::seams::Verdict::pass()]);
    let core = fx.prepare_with(confirmer).unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);
    let rn = repo_name("tool");
    let main = fx.main("tool");
    let head = CommitOid::try_from(fx.sb.commit(
        &main,
        &[("package.json", NEW_DEP.as_bytes())],
        "new deps",
    ))
    .unwrap();
    fx.sb.git(&main, &["push", "-q", "origin", "main"]);
    let git = &core.gits[&rn];
    git.fetch("origin", "main").unwrap();
    let ctx = strata_deps::Ctx {
        store: &core.store,
        paths: &core.config.paths,
        repo: &rn,
        cfg: &core.config.repos[&rn],
        git,
        env: &core.env,
        locks: &core.locks,
    };
    let proposal = strata_deps::observe_main(&ctx, &git.tree_of(&head).unwrap(), &head, &main)
        .unwrap()
        .unwrap();
    d.sessions.arm(SessionSeam::PauseAt {
        phase: "move.prepare".into(),
    });
    let (st, out) = thread::scope(|scope| {
        let approving = scope.spawn(|| {
            call(
                &server,
                "deps_approve",
                json!({ "proposal": proposal.id, "sessions": [s] }),
            )
        });
        let _release = ReleasePause(&d.sessions);
        let st = paused(&d).then(|| {
            status_until(&server, |st| {
                held_over(st, "tool", fx.lock_wait.as_millis() + 100)
            })
        });
        d.sessions.release();
        (st, approving.join().unwrap())
    });
    let st = st.unwrap_or_else(|| panic!("the rollout never moved: {out}"));
    let r = activity(&st, "deps_rollout");
    assert_eq!(r.len(), 1, "{st}");
    assert_eq!(
        (
            r[0]["sessions"].as_u64(),
            r[0]["done"].as_u64(),
            r[0]["session"].as_str()
        ),
        (Some(1), Some(0), Some(s.as_str())),
        "{st}"
    );
    assert_eq!(
        (r[0]["origin"].as_str(), r[0]["repo"].as_str()),
        (Some("request"), Some("tool")),
        "{st}"
    );
    assert!(
        r[0]["step_elapsed_ms"].as_u64() <= r[0]["elapsed_ms"].as_u64(),
        "{st}"
    );
    let moves = activity(&st, "move");
    assert_eq!(moves.len(), 1, "{st}");
    assert_eq!(
        (
            moves[0]["cause"].as_str(),
            moves[0]["origin"].as_str(),
            moves[0]["session"].as_str()
        ),
        (Some("deps"), Some("request"), Some(s.as_str())),
        "{st}"
    );
    let h = hold(&st, "tool").unwrap();
    assert_eq!(
        (h["method"].as_str(), h["origin"].as_str()),
        (Some("deps_approve"), Some("request")),
        "{st}"
    );
    assert!(held_over(&st, "tool", fx.lock_wait.as_millis()), "{st}");
    assert!(
        long_holds(&st).is_empty(),
        "the rollout's step is young: {st}"
    );
    assert_eq!(ok(out)["results"][0]["kind"], "moved");
    assert_eq!(status(&server)["activity"], json!([]));
}

/// A panic in a move (a bug) unwinds through the activity guards: no entry
/// stays, no lock stays held, and `daemon_status` still answers.
#[test]
fn activity_entry_removed_on_panic() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);
    advance(&fx, &core, "tool", "src/next.ts");
    d.sessions.arm(SessionSeam::PanicAt {
        phase: "move.prepare".into(),
    });
    let r = call(&server, "sync", json!({ "session": s, "fetch": false }));
    assert_eq!(r["ok"], false, "{r}");
    assert!(r.to_string().contains("sync panicked"), "{r}");
    let st = status(&server);
    assert_eq!(
        (&st["activity"], &st["holds"]),
        (&json!([]), &json!([])),
        "{st}"
    );
}
