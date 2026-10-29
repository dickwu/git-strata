//! rm-followups §1.6 T2 (sub-step 2.4): the catch-up pass. Backoff,
//! publish_unfinished only once pushed, staleness drops, the no-wait locks
//! and the re-checks under them, and the move consuming the row.

mod support;

use std::fs;
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, SystemTime};

use strata_publish::pending::{CatchUp, backoff, catch_up};
use strata_publish::seams::{self, FailPoint};
use strata_publish::{Ctx, MovePolicy, PublishLocks, Published, RebaseReport, publish, rebase_session_with};
use strata_store::RebasePending;
use strata_types::{BaseGen, Code, ReleaseGroup, RepoConfig, RepoName, SessionId};
use support::*;

fn fx(quiet: u64) -> Fx {
    let mut fx = Fx::with(&["tool"]);
    fx.config.repos.get_mut(&repo_name("tool")).unwrap().rebase.quiet_secs = quiet;
    fx.git_of("tool").fetch("origin", "main").unwrap();
    fx
}

/// A child that keeps `f` open; killed on drop.
struct Holder(Child);

impl Holder {
    fn file(f: &Path) -> Holder {
        let h = Holder(
            Command::new("/bin/sleep").arg("300").current_dir("/").stdin(fs::File::open(f).unwrap()).spawn().unwrap(),
        );
        std::thread::sleep(Duration::from_millis(200));
        h
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The fixture's locks, flagged as T2's no-wait form.
struct NoWait<'a>(&'a Fx);

impl PublishLocks for NoWait<'_> {
    fn with_sessions(
        &self,
        sessions: &[SessionId],
        f: &mut dyn FnMut() -> strata_publish::Result<()>,
    ) -> strata_publish::Result<strata_publish::Result<()>> {
        self.0.with_sessions(sessions, f)
    }

    fn with_repo(
        &self,
        repo: &RepoName,
        f: &mut dyn FnMut() -> strata_publish::Result<()>,
    ) -> strata_publish::Result<strata_publish::Result<()>> {
        self.0.with_repo(repo, f)
    }

    fn waits(&self) -> bool {
        false
    }
}

fn session_with(fx: &Fx, files: &[(&str, &[u8])]) -> (SessionId, ReleaseGroup) {
    let s = fx.new_session(&["tool"]);
    fx.edit(&s, "tool", files);
    let c = fx.checkpoint(&s, "tool");
    let paths: Vec<&str> = files.iter().map(|(p, _)| *p).collect();
    (s.clone(), group(vec![batch(&s, "tool", c.id, &paths)]))
}

fn run(fx: &Fx, g: &ReleaseGroup) -> strata_publish::Result<Published> {
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    publish(&fx.ctx(&d, &ss), g, None)
}

/// One pass with `cfg` (the fixture's config, or a changed copy) at `now`.
fn pass(fx: &Fx, cfg: &RepoConfig, now: SystemTime) -> Vec<(SessionId, CatchUp)> {
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    let nw = NoWait(fx);
    let ctx = Ctx { cfg, locks: &nw, ..fx.ctx(&d, &ss) };
    catch_up(&ctx, now).unwrap().into_iter().map(|(s, r)| (s, r.unwrap())).collect()
}

fn of(out: &[(SessionId, CatchUp)], s: &SessionId) -> CatchUp {
    out.iter().find(|(x, _)| x == s).map(|(_, c)| c.clone()).unwrap_or_else(|| panic!("{s} not in {out:?}"))
}

fn read(fx: &Fx, s: &SessionId, p: &str) -> Option<Vec<u8>> {
    fs::read(fx.ws(s, "tool").join(p)).ok()
}

fn pending(fx: &Fx, s: &SessionId) -> Option<RebasePending> {
    fx.store().rebase_pending_of(s, &repo_name("tool")).unwrap()
}

fn cfg(fx: &Fx, quiet: u64) -> RepoConfig {
    let mut c = fx.config.repos[&repo_name("tool")].clone();
    c.rebase.quiet_secs = quiet;
    c
}

/// B publishes b.txt; its own post-publish move defers: B was written
/// inside the 120 s window.
fn deferred_b(fx: &Fx) -> (SessionId, Published) {
    let (b, gb) = session_with(fx, &[("b.txt", b"bravo\n")]);
    fx.edit(&b, "tool", &[("later.txt", b"later\n")]);
    let p = run(fx, &gb).unwrap();
    let r = pending(fx, &b).expect("B keeps a target");
    assert_eq!((r.head.as_deref(), r.reason.as_str(), r.attempts), (Some(p.commit.as_str()), "recent_writes", 1), "{r:?}");
    (b, p)
}

#[test]
fn backoff_doubles_from_quiet_and_caps_at_thirty_minutes() {
    assert_eq!(backoff(120, 0), Duration::ZERO, "a fresh target waits nothing");
    assert_eq!(backoff(120, 1), Duration::from_secs(120));
    assert_eq!(backoff(120, 2), Duration::from_secs(240));
    assert_eq!(backoff(120, 4), Duration::from_secs(960));
    assert_eq!(backoff(120, 5), Duration::from_secs(1800));
    assert_eq!(backoff(120, 60), Duration::from_secs(1800));
    assert_eq!(backoff(86_400, u64::MAX), Duration::from_secs(1800));
}

#[test]
fn catch_up_waits_out_the_backoff_then_moves_and_consumes() {
    let fx = fx(120);
    let (b, p) = deferred_b(&fx);
    let row = pending(&fx, &b).unwrap();
    let c120 = cfg(&fx, 120);
    assert_eq!(of(&pass(&fx, &c120, row.last_at + Duration::from_secs(60)), &b), CatchUp::NotDue);
    assert_eq!(pending(&fx, &b).unwrap().attempts, 1, "a pass before the backoff does nothing");
    // Past the backoff, still inside the (real) quiet window: deferred again.
    match of(&pass(&fx, &c120, row.last_at + Duration::from_secs(121)), &b) {
        CatchUp::Attempted(RebaseReport::Deferred { reason, .. }) => assert_eq!(reason, "recent_writes"),
        c => panic!("{c:?}"),
    }
    assert_eq!(pending(&fx, &b).unwrap().attempts, 2);
    // The window over (Q = 0 here): moved, the row consumed.
    match of(&pass(&fx, &cfg(&fx, 0), SystemTime::now()), &b) {
        CatchUp::Attempted(RebaseReport::Moved { .. }) => {}
        c => panic!("{c:?}"),
    }
    assert_eq!(fx.base_of(&b, "tool").tree.as_str(), fx.git_of("tool").tree_of(&p.commit).unwrap().as_str());
    assert_eq!(read(&fx, &b, "b.txt").unwrap(), b"bravo\n");
    assert_eq!(read(&fx, &b, "later.txt").unwrap(), b"later\n");
    assert!(pending(&fx, &b).is_none(), "the move consumed the row");
}

#[test]
fn catch_up_busy_queue_defers_without_waiting() {
    let fx = fx(0);
    let (b, _p) = deferred_b_quiet0(&fx);
    *fx.queues.wait.lock().unwrap() = Duration::from_secs(60);
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
    std::thread::scope(|sc| {
        let fxr = &fx;
        let bs = b.clone();
        sc.spawn(move || {
            fxr.with_sessions(&[bs], &mut || {
                held_tx.send(()).unwrap();
                rx.recv().ok();
                Ok(())
            })
            .unwrap()
            .unwrap();
        });
        held_rx.recv().unwrap();
        // The queue's wait is 60 s; T2 must not use it.
        *fx.queues.wait.lock().unwrap() = Duration::ZERO;
        let t0 = std::time::Instant::now();
        let out = pass(&fx, &cfg(&fx, 0), SystemTime::now());
        assert!(t0.elapsed() < Duration::from_secs(30), "the pass did not wait");
        match of(&out, &b) {
            CatchUp::Attempted(RebaseReport::Deferred { reason, .. }) => assert_eq!(reason, "busy"),
            c => panic!("{c:?}"),
        }
        tx.send(()).unwrap();
    });
    let r = pending(&fx, &b).unwrap();
    assert_eq!((r.reason.as_str(), r.attempts), ("busy", 2));
}

/// B's own open files keep its post-publish move deferred even with Q = 0.
fn deferred_b_quiet0(fx: &Fx) -> (SessionId, Published) {
    let (b, gb) = session_with(fx, &[("b.txt", b"bravo\n")]);
    fx.edit(&b, "tool", &[("later.txt", b"later\n")]);
    let h = Holder::file(&fx.ws(&b, "tool").join("later.txt"));
    let p = run(fx, &gb).unwrap();
    drop(h);
    let r = pending(fx, &b).expect("B keeps a target");
    assert_eq!((r.reason.as_str(), r.attempts), ("open_files", 1), "{r:?}");
    (b, p)
}

#[test]
fn catch_up_skips_the_repo_when_its_lock_is_busy() {
    let fx = fx(0);
    let (b, _p) = deferred_b_quiet0(&fx);
    fx.lock.set_busy(1);
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    let nw = NoWait(&fx);
    let ctx = Ctx { locks: &nw, ..fx.ctx(&d, &ss) };
    let e = catch_up(&ctx, SystemTime::now()).unwrap_err();
    assert_eq!(e.code, Code::Busy, "{e:?}");
    assert_eq!(pending(&fx, &b).unwrap().attempts, 1, "nothing attempted");
}

#[test]
fn catch_up_defers_a_session_with_a_running_publish() {
    let fx = fx(0);
    let (b, _p) = deferred_b_quiet0(&fx);
    let c = fx.checkpoint(&b, "tool");
    let gb = group(vec![batch(&b, "tool", c.id, &["later.txt"])]);
    fx.store().record_publish(&gb, SystemTime::now()).unwrap();
    let before = fx.base_of(&b, "tool");
    match of(&pass(&fx, &cfg(&fx, 0), SystemTime::now()), &b) {
        CatchUp::Attempted(RebaseReport::Deferred { reason, detail, .. }) => {
            assert_eq!(reason, "busy");
            assert_eq!(detail["publishing"], true, "{detail}");
        }
        c => panic!("{c:?}"),
    }
    assert_eq!(fx.base_of(&b, "tool"), before);
}

#[test]
fn catch_up_drops_a_target_the_session_already_left() {
    let fx = fx(120);
    let (b, _p) = deferred_b(&fx);
    // A later publish of A; B is moved onto it explicitly (not the row's
    // target, so the row is left on B's old base).
    let (_a, ga) = session_with(&fx, &[("a.txt", b"alpha\n")]);
    let p = run(&fx, &ga).unwrap();
    let d = fx.deps_ctx("tool");
    let ss = fx.sessions();
    let ctx = fx.ctx(&d, &ss);
    let old = fx.base_of(&b, "tool");
    let ng = BaseGen { repo: repo_name("tool"), tree: fx.git_of("tool").tree_of(&p.commit).unwrap(), deps: old.deps };
    let r = rebase_session_with(&ctx, b.clone(), &repo_name("tool"), ng, p.commit.clone(), MovePolicy::Explicit).unwrap();
    assert!(matches!(r, RebaseReport::Moved { .. }), "{r:?}");
    assert!(pending(&fx, &b).is_some());
    assert_eq!(of(&pass(&fx, &cfg(&fx, 0), SystemTime::now()), &b), CatchUp::Dropped("stale_base".into()));
    assert!(pending(&fx, &b).is_none());
}

#[test]
fn publish_unfinished_waits_for_the_tracking_ref() {
    let fx = fx(0);
    let (s, g) = session_with(&fx, &[("a.txt", b"alpha\n")]);
    seams::fail_once(fx.git_of("tool"), FailPoint::BeforePush);
    let e = run(&fx, &g).unwrap_err();
    let c = e.detail.as_ref().and_then(|d| d["commit"].as_str()).map(str::to_string);
    let r = pending(&fx, &s).unwrap_or_else(|| panic!("a target after {e:?}"));
    assert_eq!(r.reason, "publish_unfinished", "{r:?}");
    let head = r.head.clone().unwrap();
    if let Some(c) = c {
        assert_eq!(c, head);
    }
    assert_eq!(of(&pass(&fx, &cfg(&fx, 0), SystemTime::now()), &s), CatchUp::Unpushed);
    // Pushed by hand, then fetched: attempted (and moved).
    let main = fx.main("tool");
    fx.sb.git(&main, &["push", "origin", &format!("{head}:refs/heads/main")]);
    fx.git_of("tool").fetch("origin", "main").unwrap();
    match of(&pass(&fx, &cfg(&fx, 0), SystemTime::now()), &s) {
        CatchUp::Attempted(RebaseReport::Moved { .. }) => {}
        c => panic!("{c:?}"),
    }
    assert!(pending(&fx, &s).is_none());
}
