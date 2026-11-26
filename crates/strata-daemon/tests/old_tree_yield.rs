//! new-lock §3 over the real old-tree loop and the daemon's locks: phase 1
//! walks holding no lock and shows as activity, phase 2 waits a bounded
//! time for the repo lock, foreground guards stop a claim before taking
//! any lock (with or without an admission deadline), Busy is retried and
//! shutdown drops the item. Everything lives in Dfx temp roots.
mod support;
use serde_json::{Value, json};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use strata_daemon::handlers::{self, Server, Trackers};
use strata_daemon::{resolve, startup::Daemon};
use strata_session::{
    SessionSeam,
    cwd::{MoveCwd, with_move_cwd},
};
use strata_types::{Code, CommitOid, SessionId};
use support::close::{call, ok};
use support::*;

struct Holder(Child);
impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn old_tree(d: &Daemon<'_>, fx: &Dfx) -> (SessionId, std::path::PathBuf) {
    let _catalog = d.core.admit();
    let repo = repo_name("tool");
    let id = d
        .sessions
        .new(None, std::slice::from_ref(&repo))
        .unwrap()
        .manifest
        .id;
    let ws = ws(d.core, &id, "tool");
    let holder = Holder(
        Command::new("/bin/sleep")
            .arg("300")
            .current_dir(&ws)
            .stdin(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let main = fx.main("tool");
    let head = CommitOid::try_from(
        fx.sb
            .commit(&main, &[("new.txt", b"upstream\n")], "new head"),
    )
    .unwrap();
    fx.sb.git(&main, &["push", "-q", "origin", "main"]);
    d.core.gits[&repo].fetch("origin", "main").unwrap();
    let base = d.sessions.ensure_base(&repo, &head).unwrap();
    let report = d
        .core
        .locks
        .with_repo(&repo, || {
            with_move_cwd(
                MoveCwd {
                    allow_cwd_holders: true,
                    ..Default::default()
                },
                || {
                    d.sessions
                        .move_workspace(&id, &d.core.gits[&repo].repo_id(), base, head)
                },
            )
        })
        .unwrap();
    drop(holder);
    (id, report.old_tree.expect("held tree"))
}
fn serving(d: &Daemon<'_>, f: impl FnOnce()) {
    serving_with(d, Duration::from_millis(200), f);
}
/// The old-tree loop with retry delay `retry` while `f` runs.
fn serving_with(d: &Daemon<'_>, retry: Duration, f: impl FnOnce()) {
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            handlers::test_old_tree_loop(
                d,
                &|| stop.load(Ordering::SeqCst) || d.stopping(),
                retry,
                &|| {},
            )
        });
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        stop.store(true, Ordering::SeqCst);
        d.request_stop();
        d.sessions.release();
        worker.join().unwrap();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    });
}
fn until(test: impl Fn() -> bool) {
    let start = Instant::now();
    while !test() {
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn pause(d: &Daemon<'_>, phase: &str) {
    d.sessions.arm(SessionSeam::PauseAt {
        phase: phase.into(),
    });
}
/// Stop the loop while a claim is paused, then let the claim go: it ends
/// without a verdict, a retire or a retry.
fn stop_paused(d: &Daemon<'_>) {
    d.request_stop();
    std::thread::sleep(Duration::from_millis(60));
    d.sessions.release();
}

/// §3.1, §3.5: the walk holds neither the session's place nor the repo
/// lock, so foreground takes of both go through at once; it shows as an
/// `old_tree_check` activity in its walk, not as a hold. Shutdown during
/// the walk drops the item.
#[test]
fn phase1_holds_no_lock() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let (id, old) = old_tree(&d, &fx);
    pause(&d, "old_tree.after_scan");
    serving(&d, || {
        d.sessions.wait_paused();
        assert!(core.locks.in_flight(&id).is_none());
        let start = Instant::now();
        core.locks
            .with_sessions(std::slice::from_ref(&id), || Ok(()))
            .unwrap();
        core.locks.with_repo(&repo_name("tool"), || Ok(())).unwrap();
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "took {:?}",
            start.elapsed()
        );
        let st = ok(call(&server, "daemon_status", json!({})));
        let checks: Vec<&Value> = st["activity"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|a| a["kind"] == "old_tree_check")
            .collect();
        assert_eq!(checks.len(), 1, "{st}");
        assert_eq!(checks[0]["phase"], "walk", "{st}");
        assert_eq!(checks[0]["session"], id.as_str(), "{st}");
        assert_eq!(checks[0]["repo"], "tool", "{st}");
        assert_eq!(st["holds"], json!([]), "{st}");
        stop_paused(&d);
    });
    assert!(old.is_dir());
    // A mistaken requeue would only become visible after its 200 ms
    // delay; checking immediately would pass even with that bug.
    std::thread::sleep(Duration::from_millis(250));
    assert!(
        d.sessions.next_old_tree(Duration::ZERO).is_none(),
        "shutdown does not requeue"
    );
}

/// §15.2: a yield raised while phase 0 holds the session place lets the
/// waiter in after phase 0 and does not carry into phase 1: the walk runs
/// to its end and the claim retires without a retry (the retry delay here
/// is far longer than the test).
#[test]
fn phase0_yield_does_not_carry_into_phase1() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let (id, old) = old_tree(&d, &fx);
    pause(&d, "old_tree.read_passes");
    serving_with(&d, Duration::from_secs(60), || {
        d.sessions.wait_paused();
        assert_eq!(core.locks.in_flight(&id).as_deref(), Some("old_tree"));
        pause(&d, "old_tree.between");
        std::thread::scope(|scope| {
            let foreground = scope.spawn(|| {
                core.locks
                    .with_sessions(std::slice::from_ref(&id), || Ok(()))
            });
            until(|| core.locks.queued(&id) > 0);
            d.sessions.release();
            foreground.join().unwrap().unwrap();
        });
        // A stopped phase 1 would never get between the phases.
        d.sessions.wait_paused();
        d.sessions.release();
        until(|| !old.exists());
    });
}

/// §3.1, opus r6 NB9: phase 2 waits for a repo lock held for less than the
/// lock wait and keeps its walk: the judgement completes and retires.
#[test]
fn phase2_bounded_repo_wait_keeps_walk() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let (_, old) = old_tree(&d, &fx);
    let tool = repo_name("tool");
    pause(&d, "old_tree.between");
    serving(&d, || {
        d.sessions.wait_paused();
        let (taken, wait_taken) = mpsc::channel();
        let (seen, wait_seen) = mpsc::channel::<()>();
        let (core, tool, taken) = (&core, &tool, &taken);
        std::thread::scope(|scope| {
            let holder = scope.spawn(move || {
                core.locks.with_repo(tool, || {
                    taken.send(()).unwrap();
                    let _ = wait_seen.recv_timeout(Duration::from_secs(10));
                    // Released well within the claim's lock wait.
                    std::thread::sleep(core.locks.wait() / 2);
                    Ok(())
                })
            });
            wait_taken.recv().unwrap();
            d.sessions.release();
            until(|| core.locks.repo_waiting(tool) > 0);
            seen.send(()).unwrap();
            holder.join().unwrap().unwrap();
        });
        until(|| !old.exists());
    });
}

/// §3.1: phase 2 gives up the repo lock after the lock wait, releases the
/// session place and discards the judgement (nothing retired); the key is
/// retried with a new walk, which retires once the lock is free.
#[test]
fn phase2_busy_discards_and_retries() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let (id, old) = old_tree(&d, &fx);
    let tool = repo_name("tool");
    pause(&d, "old_tree.between");
    serving(&d, || {
        d.sessions.wait_paused();
        pause(&d, "old_tree.after_scan");
        let (taken, wait_taken) = mpsc::channel();
        let (done, wait_done) = mpsc::channel::<()>();
        let (locks, tool, taken) = (&core.locks, &tool, &taken);
        std::thread::scope(|scope| {
            let holder = scope.spawn(move || {
                locks.with_repo(tool, || {
                    taken.send(()).unwrap();
                    let _ = wait_done.recv_timeout(Duration::from_secs(10));
                    Ok(())
                })
            });
            wait_taken.recv().unwrap();
            let start = Instant::now();
            d.sessions.release();
            until(|| locks.repo_waiting(tool) > 0);
            until(|| locks.repo_waiting(tool) == 0);
            assert!(
                start.elapsed() >= core.locks.wait() / 2,
                "gave up after {:?}",
                start.elapsed()
            );
            // 2N and 2′ run under the place, then it is released.
            until(|| core.locks.in_flight(&id).is_none());
            assert!(old.is_dir(), "a discarded judgement retires nothing");
            // The retry walks again.
            d.sessions.wait_paused();
            done.send(()).unwrap();
            holder.join().unwrap().unwrap();
            d.sessions.release();
        });
        until(|| !old.exists());
    });
}

/// §3.4: a blocking guard registered while the claim walks stops the walk
/// and returns once the claim has exited, before the request takes any
/// lock; while it lives the retry is not claimed.
#[test]
fn foreground_guard_blocks_claim_and_stops_walk_before_locks() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let (id, old) = old_tree(&d, &fx);
    pause(&d, "old_tree.after_scan");
    serving(&d, || {
        d.sessions.wait_paused();
        std::thread::scope(|scope| {
            let foreground = scope.spawn(|| {
                let guard = d
                    .sessions
                    .old_tree_guard(std::slice::from_ref(&id), None)
                    .expect("no deadline");
                assert!(!d.sessions.old_tree_claim_in_flight(&id));
                core.locks
                    .with_sessions(std::slice::from_ref(&id), || Ok(()))
                    .unwrap();
                core.locks.with_repo(&repo_name("tool"), || Ok(())).unwrap();
                guard
            });
            std::thread::sleep(Duration::from_millis(100));
            assert!(!foreground.is_finished(), "the guard waits for the claim");
            d.sessions.release();
            let guard = foreground.join().unwrap();
            assert!(old.is_dir(), "a stopped walk retires nothing");
            pause(&d, "old_tree.after_scan");
            assert!(
                !d.sessions.wait_paused_timeout(Duration::from_millis(500)),
                "claimed under a guard"
            );
            drop(guard);
            d.sessions.wait_paused();
            stop_paused(&d);
        });
    });
    assert!(old.is_dir());
}

/// §3.4 (r5 B8, opus r6 B1): `resolve`'s guard counts against its
/// admission deadline; on expiry the guard is withdrawn and the caller
/// gets the deadline refusal, and nothing is left blocking the session.
#[test]
fn resolve_deadline_covers_guard_wait() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let (id, _) = old_tree(&d, &fx);
    pause(&d, "old_tree.after_scan");
    serving(&d, || {
        d.sessions.wait_paused();
        let start = Instant::now();
        let e = resolve::with_queue_guarded(&core, &d.sessions, &id, || Ok(())).unwrap_err();
        let waited = start.elapsed();
        assert_eq!(e.code, Code::Busy, "{e:?}");
        let detail = e.detail.clone().unwrap();
        assert_eq!(detail["reason"], "lock_wait", "{detail}");
        assert_eq!(detail["deadline"], true, "{detail}");
        assert!(
            waited >= core.locks.wait() && waited < Duration::from_secs(3),
            "{waited:?}"
        );
        d.sessions.release();
        resolve::with_queue_guarded(&core, &d.sessions, &id, || Ok(())).unwrap();
    });
}

/// §3.4: rm's guard covers its preflight and confirmation, so it has no
/// admission deadline: a walk longer than the lock wait delays rm, never
/// refuses it.
#[test]
fn rm_slow_preflight_beyond_lock_wait_succeeds() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let (id, old) = old_tree(&d, &fx);
    pause(&d, "old_tree.after_scan");
    serving(&d, || {
        d.sessions.wait_paused();
        let start = Instant::now();
        std::thread::scope(|scope| {
            let rm = scope.spawn(|| {
                call(
                    &server,
                    "rm",
                    json!({ "session": id.as_str(), "discard": false }),
                )
            });
            std::thread::sleep(core.locks.wait() * 2);
            assert!(!rm.is_finished(), "rm waits for the walk to stop");
            d.sessions.release();
            ok(rm.join().unwrap());
        });
        assert!(start.elapsed() >= core.locks.wait() * 2);
    });
    assert!(!old.exists());
}

/// §3.4: rm's guard comes before its discard preflight. While a claim of
/// the session's old tree is in flight, `rm --discard` runs neither the
/// preflight nor the prompt; once the stopped claim has left, the session
/// goes, its old tree with it.
#[test]
fn discard_preflight_vs_worker() {
    use strata_confirm::seams::Verdict;
    let fx = Dfx::new();
    let (c, log, _) = fx.confirmer(vec![Verdict::pass()]);
    let core = fx.prepare_with(c).unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let (id, old) = old_tree(&d, &fx);
    pause(&d, "old_tree.after_scan");
    serving(&d, || {
        d.sessions.wait_paused();
        std::thread::scope(|scope| {
            let rm = scope.spawn(|| {
                call(
                    &server,
                    "rm",
                    json!({ "session": id.as_str(), "discard": true }),
                )
            });
            std::thread::sleep(core.locks.wait() * 2);
            assert!(!rm.is_finished(), "rm waits for the claim");
            assert!(log.shown().is_empty(), "no prompt while the claim runs");
            assert!(d.sessions.old_tree_claim_in_flight(&id));
            d.sessions.release();
            ok(rm.join().unwrap());
        });
    });
    assert_eq!(log.shown().len(), 1, "one prompt");
    assert!(!old.exists());
}

/// §3.4 (r5 B8, opus r6 B1): rm's guard covers its Touch ID prompt and has
/// no admission deadline, so a confirmation that passes after more than the
/// lock wait (held here in the `after_confirm` seam, between the passed
/// prompt and the queue take) delays `rm --discard`, never refuses it.
/// Meanwhile the worker claims none of the session's old trees.
#[test]
fn rm_discard_slow_confirmer_succeeds() {
    use strata_confirm::seams::Verdict;
    let fx = Dfx::new();
    let (c, log, _) = fx.confirmer(vec![Verdict::pass()]);
    let core = fx.prepare_with(c).unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let (id, old) = old_tree(&d, &fx);
    let root = core.config.paths.root.clone();
    let (entered, wait_entered) = mpsc::channel::<()>();
    let (go, wait_go) = mpsc::channel::<()>();
    let wait_go = std::sync::Mutex::new(wait_go);
    strata_daemon::seams::set_after_confirm(
        &root,
        Some(std::sync::Arc::new(move || {
            let _ = entered.send(());
            let _ = wait_go
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .recv_timeout(Duration::from_secs(30));
        })),
    );
    // A claim of the old tree is in flight when rm arrives.
    pause(&d, "old_tree.after_scan");
    serving(&d, || {
        d.sessions.wait_paused();
        std::thread::scope(|scope| {
            let rm = scope.spawn(|| {
                call(
                    &server,
                    "rm",
                    json!({ "session": id.as_str(), "discard": true }),
                )
            });
            std::thread::sleep(core.locks.wait() * 2);
            assert!(!rm.is_finished(), "rm waits for the claim");
            // rm's guard stops the walk; the claim leaves, its retry queued.
            d.sessions.release();
            wait_entered
                .recv_timeout(Duration::from_secs(10))
                .expect("the confirmation passed");
            assert!(old.is_dir(), "the stopped claim retired nothing");
            let slow = Instant::now();
            pause(&d, "old_tree.after_scan");
            assert!(
                !d.sessions.wait_paused_timeout(core.locks.wait() * 2),
                "claimed under rm's guard"
            );
            assert!(!rm.is_finished(), "rm waits for the slow confirmation");
            go.send(()).unwrap();
            ok(rm.join().unwrap());
            assert!(slow.elapsed() >= core.locks.wait() * 2);
        });
    });
    strata_daemon::seams::set_after_confirm(&root, None);
    assert_eq!(log.shown().len(), 1, "one prompt");
    assert!(!old.exists());
}

#[test]
fn passes_read_failure_retries_before_recording_a_verdict() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let (id, old) = old_tree(&d, &fx);
    let _holder = Holder(
        Command::new("/bin/sleep")
            .arg("300")
            .current_dir(&old)
            .stdin(Stdio::null())
            .spawn()
            .unwrap(),
    );
    d.sessions.arm(SessionSeam::FailAt {
        phase: "old_tree.read_passes".into(),
        errno: libc::EIO,
    });
    let n = core.store.move_intents(&id).unwrap().last().unwrap().n;
    let key = strata_session::OldTreeKey {
        session: id,
        repo: repo_name("tool"),
        n,
    };
    assert!(d.sessions.old_tree_verdict(&key).is_none());
    let start = Instant::now();
    serving(&d, || {
        // The first pass fails before scanning. With no status calls or
        // other background workers here, only a retry can create a verdict.
        until(|| d.sessions.old_tree_verdict(&key).is_some());
        assert!(start.elapsed() >= Duration::from_millis(200));
        assert!(old.is_dir());
        assert!(!d.sessions.old_tree_verdict(&key).unwrap().changed);
    });
    assert!(old.is_dir());
}

#[test]
fn held_old_tree_keeps_its_verdict_without_short_retry_polling() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let (id, old) = old_tree(&d, &fx);
    let _holder = Holder(
        Command::new("/bin/sleep")
            .arg("300")
            .current_dir(&old)
            .stdin(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let n = core.store.move_intents(&id).unwrap().last().unwrap().n;
    let key = strata_session::OldTreeKey {
        session: id,
        repo: repo_name("tool"),
        n,
    };
    serving(&d, || {
        until(|| d.sessions.old_tree_verdict(&key).is_some());
        let verdict = d.sessions.old_tree_verdict(&key).unwrap();
        assert!(!verdict.holders.is_empty());
        // Any short retry would hit this seam and invalidate the verdict.
        d.sessions.arm(SessionSeam::StopAt {
            phase: "old_tree.after_scan".into(),
        });
        std::thread::sleep(Duration::from_millis(450));
        assert_eq!(d.sessions.old_tree_verdict(&key), Some(verdict));
    });
    assert!(old.is_dir());
}
