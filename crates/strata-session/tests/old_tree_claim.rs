//! new-lock §3 and §8 Item 2: one claim of the old-tree worker (phases 0,
//! 1 and 2, 2N and 2′), its token, the foreground guards and the queue
//! rules, driven through `claim_old_tree` and `run_old_tree_claim` with a
//! lock double. The daemon's own locks are covered in strata-daemon's
//! tests/old_tree_yield.rs.

mod support;

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use strata_session::{
    OldTreeClaim, OldTreeKey, OldTreeLocks, OldTreeMode, OldTreeOutcome, SessionSeam, Sessions,
};
use strata_types::{BaseGen, Code, CommitOid, RepoId, RepoName, SessionId, StrataError};
use support::*;

/// The worker's retry delay in these tests.
const RETRY: Duration = Duration::from_millis(50);

// ---------------------------------------------------------------- helpers

/// A child process that keeps a directory as its cwd; killed on drop.
struct Holder(Child);

impl Holder {
    fn cwd(dir: &Path) -> Holder {
        Holder(
            Command::new("/bin/sleep")
                .arg("300")
                .current_dir(dir)
                .stdin(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The locks of a claim: `try_session` call k (counted from 1) is refused
/// when `refuse` holds k, the repo lock while `repo_busy` is set, and
/// `before` runs ahead of every `try_session` call with its number.
struct Fake<'h> {
    calls: AtomicUsize,
    refuse: Vec<usize>,
    repo_busy: bool,
    before: &'h dyn Fn(usize),
}

fn nothing(_: usize) {}

impl<'h> Fake<'h> {
    fn new(refuse: &[usize], repo_busy: bool, before: &'h dyn Fn(usize)) -> Fake<'h> {
        Fake {
            calls: AtomicUsize::new(0),
            refuse: refuse.to_vec(),
            repo_busy,
            before,
        }
    }
}

impl Fake<'static> {
    fn free() -> Fake<'static> {
        Fake::new(&[], false, &nothing)
    }
}

impl OldTreeLocks for Fake<'_> {
    fn try_session(
        &self,
        _: &SessionId,
        _: bool,
        f: &mut dyn FnMut(),
    ) -> Option<strata_session::Result<()>> {
        let k = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        (self.before)(k);
        if self.refuse.contains(&k) {
            return None;
        }
        f();
        Some(Ok(()))
    }

    fn repo_bounded(
        &self,
        _: &RepoName,
        _: &dyn Fn() -> bool,
        f: &mut dyn FnMut(),
    ) -> strata_session::Result<()> {
        if self.repo_busy {
            return Err(StrataError::new(Code::Busy, "the repo lock is busy"));
        }
        f();
        Ok(())
    }
}

/// One done move's old tree.
struct Tree {
    id: SessionId,
    old: PathBuf,
    key: OldTreeKey,
    ino: u64,
}

fn ino(p: &Path) -> u64 {
    fs::symlink_metadata(p).unwrap().ino()
}

fn rid(fx: &Fx) -> RepoId {
    fx.gits[&fx.rn("tool")].repo_id()
}

fn tree(fx: &Fx, id: SessionId, old: PathBuf) -> Tree {
    let n = fx.store().move_intents(&id).unwrap().last().unwrap().n;
    Tree {
        key: OldTreeKey {
            session: id.clone(),
            repo: fx.rn("tool"),
            n,
        },
        ino: ino(&old),
        old,
        id,
    }
}

/// Publish a commit of tool and build its base generation.
fn target(fx: &Fx, s: &Sessions<'_>, files: &[(&str, &[u8])]) -> (BaseGen, CommitOid) {
    let main = fx.main("tool");
    let c = fx.sb.commit(&main, files, "upstream");
    fx.sb.git(&main, &["push", "-q", "origin", "HEAD:main"]);
    let h = CommitOid::try_from(c).unwrap();
    (s.ensure_base(&fx.rn("tool"), &h).unwrap(), h)
}

/// An `Explicit` move of tool whose old tree was kept because a process
/// sits in it; the process is returned (drop it to free the tree).
fn held_tree(fx: &Fx, s: &Sessions<'_>) -> (Tree, Holder) {
    let rep = s.new(Some("T-5"), &[fx.rn("tool")]).unwrap();
    let id = rep.manifest.id.clone();
    let w = rep.manifest.repos[&fx.rn("tool")].path.clone();
    let holder = Holder::cwd(&w.join("src"));
    std::thread::sleep(Duration::from_millis(100));
    let to = target(fx, s, &[("src/up.ts", b"up\n")]);
    let report = strata_session::cwd::with_move_cwd(
        strata_session::cwd::MoveCwd {
            allow_cwd_holders: true,
            ..Default::default()
        },
        || s.move_workspace(&id, &rid(fx), to.0.clone(), to.1.clone()),
    )
    .unwrap();
    (
        tree(fx, id, report.old_tree.expect("kept while in use")),
        holder,
    )
}

/// An `Explicit` move's old tree, free and unchanged.
fn explicit_tree(fx: &Fx, s: &Sessions<'_>) -> Tree {
    let (t, holder) = held_tree(fx, s);
    drop(holder);
    t
}

/// An `Auto` move of tool whose old tree was kept because a process sat in
/// the workspace at the swap; the process is gone, the row is clean.
fn auto_tree(fx: &Fx, s: &Sessions<'_>) -> Tree {
    let rep = s.new(Some("T-5"), &[fx.rn("tool")]).unwrap();
    let id = rep.manifest.id.clone();
    let w = rep.manifest.repos[&fx.rn("tool")].path.clone();
    let to = target(fx, s, &[("src/up.ts", b"up\n")]);
    let baseline = strata_session::baseline_of(&strata_session::walk_activity(&w).unwrap());
    s.arm(SessionSeam::PauseAt {
        phase: "move.swap".into(),
    });
    let report = std::thread::scope(|scope| {
        let mover = scope.spawn(|| {
            s.move_workspace_auto(&id, &rid(fx), to.0.clone(), to.1.clone(), None, &baseline)
        });
        s.wait_paused();
        let holder = Holder::cwd(&w);
        std::thread::sleep(Duration::from_millis(50));
        s.release();
        let report = mover.join().unwrap().unwrap();
        drop(holder);
        report
    });
    let t = tree(fx, id, report.old_tree.expect("kept while held"));
    let r = row(fx, &t);
    assert!(
        r.baseline.is_some() && r.late_writes.is_none() && r.passes.is_empty(),
        "{r:?}"
    );
    t
}

fn row(fx: &Fx, t: &Tree) -> strata_store::MoveActivity {
    fx.store()
        .move_activity(&t.id, &t.key.repo, t.key.n)
        .unwrap()
        .expect("an Auto move's row")
}

fn set_passes(fx: &Fx, t: &Tree, passes: &[i64]) {
    fx.store()
        .set_move_passes(&t.id, &t.key.repo, t.key.n, passes)
        .unwrap();
}

fn no_stop() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

/// Queue `t` as due and claim it (a retry of it comes due first).
fn claim(s: &Sessions<'_>, t: &Tree) -> OldTreeClaim {
    s.queue_old_tree_at(t.key.clone(), t.old.clone(), t.ino, Instant::now());
    let c = s
        .claim_old_tree(Duration::from_secs(5))
        .expect("a due claim");
    assert_eq!(c.key, t.key);
    c
}

fn run(s: &Sessions<'_>, c: OldTreeClaim, locks: &dyn OldTreeLocks) -> OldTreeOutcome {
    s.run_old_tree_claim(c, locks, &no_stop(), RETRY).unwrap()
}

fn unclaimed(s: &Sessions<'_>) -> bool {
    s.claim_old_tree(Duration::ZERO).is_none()
}

// ------------------------------------------------------------------ tests

/// §3.3: a foreground judgement (status Inline), an rm or restore
/// (`forget_old_trees`), a move of the repo (`forget_old_trees_of`) and a
/// real move's step 20 change the intent's token while a claim is between
/// its phases; its phase 2 then retires nothing. The next claim judges
/// under the new token.
#[test]
fn token_bumped_by_rm_step20_status() {
    for bump in ["status", "rm", "move", "step20"] {
        let fx = Fx::with(&["tool"]);
        let s = fx.sessions();
        let t = explicit_tree(&fx, &s);
        let c = claim(&s, &t);
        s.arm(SessionSeam::PauseAt {
            phase: "old_tree.between".into(),
        });
        let out = std::thread::scope(|scope| {
            let worker = scope.spawn(|| run(&s, c, &Fake::free()));
            s.wait_paused();
            match bump {
                "status" => {
                    s.set_old_tree_mode(OldTreeMode::Inline);
                    s.status(&t.id).unwrap();
                }
                "rm" => s.forget_old_trees(&t.id),
                "move" => s.forget_old_trees_of(&t.id, &t.key.repo),
                _ => {
                    let to = target(&fx, &s, &[("src/next.ts", b"next\n")]);
                    s.move_workspace(&t.id, &rid(&fx), to.0, to.1).unwrap();
                }
            }
            s.release();
            worker.join().unwrap()
        });
        assert_eq!(out, OldTreeOutcome::Skipped("invalidated"), "{bump}");
        assert!(s.old_tree_verdict(&t.key).is_none(), "{bump}");
        if bump == "step20" {
            assert!(!t.old.exists(), "step 20 judged and retired it inline");
            continue;
        }
        assert!(t.old.is_dir(), "{bump}: a voided judgement retires nothing");
        assert_eq!(
            run(&s, claim(&s, &t), &Fake::free()),
            OldTreeOutcome::Retired,
            "{bump}"
        );
        assert!(!t.old.exists());
    }
}

/// §3.1 r5 B3: a late root file seen by a completed phase 1 is recorded
/// (2N) although phase 2 cannot get the repo lock; the file goes away
/// before the retry, and the notice still stands: never retired, rm refuses.
#[test]
fn positive_finding_persisted_when_phase2_busy() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let t = auto_tree(&fx, &s);
    fs::write(t.old.join("late-root.txt"), b"late\n").unwrap();
    let busy = Fake::new(&[], true, &nothing);
    assert_eq!(
        run(&s, claim(&s, &t), &busy),
        OldTreeOutcome::Skipped("busy")
    );
    let notice = row(&fx, &t)
        .late_writes
        .expect("2N wrote the notice without the locks");
    assert_eq!(notice["reason"], "late_writes", "{notice}");
    let paths: Vec<&str> = notice["paths"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p.as_str())
        .collect();
    assert!(paths.contains(&"late-root.txt"), "{notice}");
    assert!(t.old.is_dir() && s.old_tree_verdict(&t.key).is_none());

    fs::remove_file(t.old.join("late-root.txt")).unwrap();
    let out = run(&s, claim(&s, &t), &Fake::free());
    assert_eq!(
        out,
        OldTreeOutcome::Kept {
            holders: vec![],
            changed: Some(true)
        }
    );
    assert_eq!(
        row(&fx, &t).late_writes,
        Some(notice),
        "the first notice stands"
    );
    let e = s.remove(&t.id, false, None).unwrap_err();
    assert_eq!(e.code, Code::Dirty, "{e:?}");
    assert!(t.old.is_dir());
}

/// §3.1, §15.3: a guard that registers between the phases routes the claim
/// to its non-retiring exit. A clean phase 1 gets its suspended pass back
/// (2′); a late one writes its notice (2N) and gets no pass back.
#[test]
fn guard_in_between_restores_pass_and_routed_late_phase1_writes_notice_no_restore() {
    for late in [false, true] {
        let fx = Fx::with(&["tool"]);
        let s = fx.sessions();
        let t = auto_tree(&fx, &s);
        set_passes(&fx, &t, &[1]);
        if late {
            fs::write(t.old.join("src/late.ts"), b"late\n").unwrap();
        }
        let c = claim(&s, &t);
        s.arm(SessionSeam::PauseAt {
            phase: "old_tree.between".into(),
        });
        let out = std::thread::scope(|scope| {
            let worker = scope.spawn(|| run(&s, c, &Fake::free()));
            s.wait_paused();
            assert!(row(&fx, &t).passes.is_empty(), "suspended before the scan");
            // Registered, then withdrawn at its deadline: the claim stays routed.
            let deadline = Some(Instant::now() + Duration::from_millis(100));
            assert!(
                s.old_tree_guard(std::slice::from_ref(&t.id), deadline)
                    .is_none()
            );
            s.release();
            worker.join().unwrap()
        });
        assert_eq!(out, OldTreeOutcome::Skipped("yielded"), "late {late}");
        let r = row(&fx, &t);
        assert!(t.old.is_dir());
        if late {
            assert_eq!(
                r.late_writes.as_ref().map(|n| n["reason"].clone()),
                Some("late_writes".into()),
                "{r:?}"
            );
            assert!(r.passes.is_empty(), "no pass after a late phase 1: {r:?}");
        } else {
            assert!(r.late_writes.is_none(), "{r:?}");
            assert_eq!(r.passes, vec![1], "2′ restored the pass");
        }
        assert!(s.old_tree_verdict(&t.key).is_none());
        // A yielded claim is retried.
        let c = s.claim_old_tree(Duration::from_secs(5)).expect("retried");
        s.release_old_tree_claim(c, false);
    }
}

/// §3.1: a stop raised during phase 2's final scan retires nothing; the
/// suspended pass comes back only when that scan saw no holder.
#[test]
fn stop_during_final_scan_no_retire() {
    for held in [false, true] {
        let fx = Fx::with(&["tool"]);
        let s = fx.sessions();
        let t = auto_tree(&fx, &s);
        set_passes(&fx, &t, &[1]);
        let c = claim(&s, &t);
        let stop = no_stop();
        s.arm(SessionSeam::PauseAt {
            phase: "old_tree.final_scan".into(),
        });
        let mut holder = None;
        let out = std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                s.run_old_tree_claim(c, &Fake::free(), &stop, RETRY)
                    .unwrap()
            });
            s.wait_paused();
            if held {
                holder = Some(Holder::cwd(&t.old));
            }
            stop.store(true, Ordering::SeqCst);
            s.release();
            worker.join().unwrap()
        });
        assert_eq!(out, OldTreeOutcome::Skipped("stopped"), "held {held}");
        assert!(t.old.is_dir(), "held {held}");
        assert_eq!(
            row(&fx, &t).passes,
            if held { vec![] } else { vec![1] },
            "held {held}"
        );
        assert!(s.old_tree_verdict(&t.key).is_none());
        assert!(unclaimed(&s), "a stopped claim is not retried");
        drop(holder);
    }
}

/// §3.1 phase 0: the passes are cleared in the store before the first
/// scan, so a crash between the phases loses the pass (fail closed); the
/// dead claim leaves nothing in flight, and the next claim starts over.
#[test]
fn crash_between_phases_passes_cleared() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let t = auto_tree(&fx, &s);
    set_passes(&fx, &t, &[1]);
    let c = claim(&s, &t);
    s.arm(SessionSeam::PanicAt {
        phase: "old_tree.between".into(),
    });
    let crashed =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&s, c, &Fake::free())));
    assert!(crashed.is_err());
    assert!(
        row(&fx, &t).passes.is_empty(),
        "the reset was persisted before the scan"
    );
    assert!(
        !s.old_tree_claim_in_flight(&t.id),
        "released by the drop guard"
    );
    let guard = s.old_tree_guard(std::slice::from_ref(&t.id), Some(Instant::now()));
    assert!(guard.is_some(), "nothing in flight to wait for");
    drop(guard);
    // A first clean pass again: retirement waits another Q.
    let out = run(&s, claim(&s, &t), &Fake::free());
    assert_eq!(
        out,
        OldTreeOutcome::Kept {
            holders: vec![],
            changed: Some(false)
        }
    );
    assert_eq!(row(&fx, &t).passes.len(), 1);
    assert!(t.old.is_dir());
}

/// §3.1: phase 2 busy (the repo lock, the session place, or both, also for
/// 2′) discards the judgement and retries the key; 2′ puts the pass back
/// only under the session place.
#[test]
fn phase2_busy_discards() {
    let cases: [(&[usize], bool, bool); 3] = [
        (&[], true, true),
        (&[2], false, true),
        (&[2, 3], false, false),
    ];
    for (refuse, repo_busy, restored) in cases {
        let fx = Fx::with(&["tool"]);
        let s = fx.sessions();
        let t = auto_tree(&fx, &s);
        set_passes(&fx, &t, &[1]);
        let locks = Fake::new(refuse, repo_busy, &nothing);
        let retry = Duration::from_millis(300);
        let out = s
            .run_old_tree_claim(claim(&s, &t), &locks, &no_stop(), retry)
            .unwrap();
        let case = format!("refuse {refuse:?}, repo busy {repo_busy}");
        assert_eq!(out, OldTreeOutcome::Skipped("busy"), "{case}");
        assert!(
            t.old.is_dir() && s.old_tree_verdict(&t.key).is_none(),
            "{case}"
        );
        let r = row(&fx, &t);
        assert_eq!(r.passes, if restored { vec![1] } else { vec![] }, "{case}");
        assert!(r.late_writes.is_none(), "{case}");
        assert!(unclaimed(&s), "{case}: the retry waits for its delay");
        let c = s.claim_old_tree(Duration::from_secs(5)).expect("retried");
        s.release_old_tree_claim(c, false);
    }
}

/// §3.1: a clean judgement authorizes a retire only in the phase 2 right
/// after its own walk. A busy phase 2 discards it; an edit before the
/// retry is seen by the retry's own walk.
#[test]
fn edit_between_busy_and_retry_seen() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let t = explicit_tree(&fx, &s);
    let busy = Fake::new(&[], true, &nothing);
    assert_eq!(
        run(&s, claim(&s, &t), &busy),
        OldTreeOutcome::Skipped("busy")
    );
    assert!(t.old.is_dir() && s.old_tree_verdict(&t.key).is_none());
    fs::write(t.old.join("src/late.ts"), b"late\n").unwrap();
    let out = run(&s, claim(&s, &t), &Fake::free());
    assert_eq!(
        out,
        OldTreeOutcome::Kept {
            holders: vec![],
            changed: Some(true)
        }
    );
    assert!(t.old.is_dir());
    assert_eq!(s.old_tree_verdict(&t.key).map(|v| v.changed), Some(true));
}

/// §3.1: phase 1 returns a walk error without recording it; phase 2
/// records it only if the intent's token is unchanged, so an rm between
/// the phases leaves no `walk_error` notice.
#[test]
fn walk_error_after_rm_not_recorded() {
    for rm in [false, true] {
        let fx = Fx::with(&["tool"]);
        let s = fx.sessions();
        let t = auto_tree(&fx, &s);
        s.arm(SessionSeam::FailAt {
            phase: "old_tree.activity_directory".into(),
            errno: libc::EIO,
        });
        // The second try_session is phase 2's.
        let forget = |k: usize| {
            if rm && k == 2 {
                s.forget_old_trees(&t.id);
            }
        };
        let out = run(&s, claim(&s, &t), &Fake::new(&[], false, &forget));
        let notice = row(&fx, &t).late_writes;
        if rm {
            assert_eq!(out, OldTreeOutcome::Skipped("invalidated"));
            assert!(notice.is_none(), "{notice:?}");
        } else {
            assert_eq!(
                out,
                OldTreeOutcome::Kept {
                    holders: vec![],
                    changed: Some(true)
                }
            );
            assert_eq!(
                notice.map(|n| n["reason"].clone()),
                Some("walk_error".into())
            );
        }
        assert!(t.old.is_dir());
    }
}

/// §3.6: a status poll (Deferred) leaves a key in flight with the same
/// identity alone, gives one with another identity one recheck once the
/// claim ends, never queues over a fresh verdict, and leaves a scheduled
/// retry of the same tree on its schedule.
#[test]
fn hint_inflight_and_cooldown_rules() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let (t, _holder) = held_tree(&fx, &s);
    s.set_old_tree_mode(OldTreeMode::Deferred);

    let c = claim(&s, &t);
    s.status(&t.id).unwrap();
    s.release_old_tree_claim(c, false);
    assert!(
        unclaimed(&s),
        "same identity in flight: the poll queued nothing"
    );

    s.queue_old_tree_at(t.key.clone(), t.old.clone(), t.ino + 1, Instant::now());
    let c = s
        .claim_old_tree(Duration::ZERO)
        .expect("the other identity");
    assert_eq!(c.ino, t.ino + 1);
    s.status(&t.id).unwrap();
    s.status(&t.id).unwrap();
    assert!(unclaimed(&s), "no second claim while one is in flight");
    s.release_old_tree_claim(c, false);
    let c = s
        .claim_old_tree(Duration::ZERO)
        .expect("one recheck after the claim");
    assert_eq!((c.path.clone(), c.ino), (t.old.clone(), t.ino));
    assert!(unclaimed(&s), "only one");

    let out = run(&s, c, &Fake::free());
    assert!(
        matches!(&out, OldTreeOutcome::Kept { holders, changed: Some(false) } if !holders.is_empty()),
        "{out:?}"
    );
    s.status(&t.id).unwrap();
    assert!(
        unclaimed(&s),
        "a fresh verdict of the same identity is not queued again"
    );

    s.forget_old_trees(&t.id);
    s.retry_old_tree(t.key.clone(), t.old.clone(), t.ino, Duration::from_secs(60));
    s.status(&t.id).unwrap();
    assert!(
        unclaimed(&s),
        "the retry of the same tree keeps its schedule"
    );
    s.finish_old_tree_retry(&t.key);
    s.status(&t.id).unwrap();
    let c = s
        .claim_old_tree(Duration::ZERO)
        .expect("queued once nothing covers it");
    s.release_old_tree_claim(c, false);
}

/// §3.6: a Q pass or a retry that comes due while the key is in flight
/// waits for the claim to end instead of being lost or claimed twice.
#[test]
fn delayed_q_pass_not_lost() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let t = explicit_tree(&fx, &s);
    let c = claim(&s, &t);
    s.queue_old_tree_at(
        t.key.clone(),
        t.old.clone(),
        t.ino,
        Instant::now() + Duration::from_millis(30),
    );
    std::thread::sleep(Duration::from_millis(80));
    assert!(unclaimed(&s), "an in-flight key is not claimed twice");
    s.release_old_tree_claim(c, false);
    let c = s
        .claim_old_tree(Duration::ZERO)
        .expect("the Q pass waited for the claim");
    s.retry_old_tree(
        t.key.clone(),
        t.old.clone(),
        t.ino,
        Duration::from_millis(30),
    );
    std::thread::sleep(Duration::from_millis(80));
    assert!(unclaimed(&s));
    s.release_old_tree_claim(c, false);
    let c = s
        .claim_old_tree(Duration::ZERO)
        .expect("the retry waited for the claim");
    s.release_old_tree_claim(c, false);
}

/// §3.6: a pass the drain refuses releases its claim atomically and puts
/// the key back; released without requeue, it is gone.
#[test]
fn drain_requeue_releases_claim() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let t = explicit_tree(&fx, &s);
    let c = claim(&s, &t);
    assert!(s.old_tree_claim_in_flight(&t.id));
    s.release_old_tree_claim(c, true);
    assert!(!s.old_tree_claim_in_flight(&t.id));
    let c = s.claim_old_tree(Duration::ZERO).expect("requeued");
    assert_eq!(c.key, t.key);
    s.release_old_tree_claim(c, false);
    assert!(unclaimed(&s), "released without requeue");
    assert!(t.old.is_dir());
}

/// §3.4: an active guard keeps the session's due keys unclaimed until it
/// drops; a guard that registers during phase 2 stops the claim before
/// its retire, and the withdrawn guard blocks nothing afterwards.
#[test]
fn foreground_guard_blocks_claims_and_the_retire() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let t = explicit_tree(&fx, &s);
    s.queue_old_tree_at(t.key.clone(), t.old.clone(), t.ino, Instant::now());
    let guard = s
        .old_tree_guard(std::slice::from_ref(&t.id), None)
        .expect("nothing in flight");
    assert!(
        s.claim_old_tree(Duration::from_millis(150)).is_none(),
        "claimed under a guard"
    );
    drop(guard);
    let c = s
        .claim_old_tree(Duration::ZERO)
        .expect("claimable once the guard drops");
    s.arm(SessionSeam::PauseAt {
        phase: "old_tree.final_scan".into(),
    });
    let out = std::thread::scope(|scope| {
        let worker = scope.spawn(|| run(&s, c, &Fake::free()));
        s.wait_paused();
        let started = Instant::now();
        let deadline = Some(started + Duration::from_millis(100));
        assert!(
            s.old_tree_guard(std::slice::from_ref(&t.id), deadline)
                .is_none(),
            "the claim outlived the deadline"
        );
        assert!(started.elapsed() >= Duration::from_millis(100));
        s.release();
        worker.join().unwrap()
    });
    assert_eq!(out, OldTreeOutcome::Skipped("yielded"));
    assert!(t.old.is_dir());
    let c = s.claim_old_tree(Duration::from_secs(5)).expect("retried");
    assert_eq!(run(&s, c, &Fake::free()), OldTreeOutcome::Retired);
    assert!(!t.old.exists());
}
