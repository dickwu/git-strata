//! The real old-tree loop releases both locks on contention, retries Busy,
//! and does not requeue after shutdown. Everything lives in Dfx temp roots.
mod support;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use strata_daemon::{handlers, locks::RequestScope, startup::Daemon};
use strata_session::{
    SessionSeam,
    cwd::{MoveCwd, with_move_cwd},
};
use strata_types::{CommitOid, SessionId};
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
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            handlers::test_old_tree_loop(
                d,
                &|| stop.load(Ordering::SeqCst) || d.stopping(),
                Duration::from_millis(200),
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

#[test]
fn foreground_session_and_repo_waiters_yield_and_retry_then_shutdown_drops_the_item() {
    for repo_wait in [false, true] {
        let fx = Dfx::new();
        let core = fx.prepare().unwrap();
        let d = Daemon::start(&core).unwrap();
        let (id, old) = old_tree(&d, &fx);
        d.sessions.arm(SessionSeam::PauseAt {
            phase: "old_tree.after_scan".into(),
        });
        serving(&d, || {
            d.sessions.wait_paused();
            assert_eq!(core.locks.in_flight(&id).as_deref(), Some("old_tree"));
            let start = Instant::now();
            std::thread::scope(|scope| {
                let foreground = scope.spawn(|| {
                    let _request = RequestScope::begin("sync", Some(id.clone()));
                    if repo_wait {
                        core.locks.with_repo(&repo_name("tool"), || Ok(()))
                    } else {
                        core.locks
                            .with_sessions(std::slice::from_ref(&id), || Ok(()))
                    }
                });
                until(|| {
                    if repo_wait {
                        core.locks.repo_waiting(&repo_name("tool")) > 0
                    } else {
                        core.locks.queued(&id) > 0
                    }
                });
                d.sessions.release();
                foreground.join().unwrap().unwrap();
            });
            assert!(
                start.elapsed() < Duration::from_secs(1),
                "yield took {:?}",
                start.elapsed()
            );
            assert!(old.is_dir(), "yield is not a clean verdict or retirement");
            // It must return for another pass after the retry delay.
            d.sessions.arm(SessionSeam::PauseAt {
                phase: "old_tree.after_scan".into(),
            });
            d.sessions.wait_paused();
            assert!(
                start.elapsed() >= Duration::from_millis(200),
                "retry bypassed its backoff"
            );
            d.request_stop();
            std::thread::sleep(Duration::from_millis(60));
            d.sessions.release();
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
}

#[test]
fn busy_repo_releases_the_session_queue_and_retries_the_item() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let (id, old) = old_tree(&d, &fx);
    let (ready, wait_ready) = mpsc::channel();
    let (release, wait_release) = mpsc::channel();
    std::thread::scope(|scope| {
        let locks = &core.locks;
        let holder = scope.spawn(move || {
            locks.with_repo(&repo_name("tool"), || {
                ready.send(()).unwrap();
                wait_release.recv_timeout(Duration::from_secs(15)).unwrap();
                Ok(())
            })
        });
        wait_ready.recv().unwrap();
        d.sessions.arm(SessionSeam::PauseAt {
            phase: "old_tree.after_scan".into(),
        });
        serving(&d, || {
            // Allow the initial zero-wait repo attempt to fail and requeue.
            std::thread::sleep(Duration::from_millis(250));
            assert!(
                core.locks.in_flight(&id).is_none(),
                "background held queue while repo was busy"
            );
            assert_eq!(core.locks.repo_waiting(&repo_name("tool")), 0);
            release.send(()).unwrap();
            // A dropped Busy item would never reach this seam.
            d.sessions.wait_paused();
            assert!(old.is_dir());
            d.request_stop();
            std::thread::sleep(Duration::from_millis(60));
            d.sessions.release();
        });
        holder.join().unwrap().unwrap();
    });
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
