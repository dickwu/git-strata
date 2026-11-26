//! new-lock §3.1: one claim of the old-tree worker. Phase 0 revalidates
//! under the session's queue place (taken only if free) and suspends the
//! clean passes; phase 1 scans and walks holding no lock; phase 2 takes the
//! session place (zero wait) and the repo lock (bounded, abortable),
//! revalidates and retires. A completed phase 1 that is not followed by
//! phase 2 persists its positive finding (2N) and restores a clean pass
//! under the session place alone (2′).

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use strata_deps::OpenFile;
use strata_store::{Lifecycle, MoveActivity, MoveIntent, MoveState};
use strata_types::{Code, RepoConfig, RepoName, SessionId};

use super::queue::{ClaimState, Completed, Judged, OldTreeClaim, Token, lock};
use super::{OLD_TREE_TTL, OldTreeKey, OldTreeVerdict, Outcome, Probe, ino_of, old_path};
use crate::move_workspace::prior_generation;
use crate::observe::{self, Work};
use crate::{Result, Sessions};

/// The locks a claim takes: the daemon's, or none where the caller already
/// excludes all other work (tests, the local fallback).
pub trait OldTreeLocks {
    /// Run `f` holding `session`'s queue place only if it is free now;
    /// `None` when it is not (also when this thread holds it). `yielding`:
    /// a request that queues for the place meanwhile stops `f`'s walks
    /// (phase 0 only, §15.2).
    fn try_session(
        &self,
        session: &SessionId,
        yielding: bool,
        f: &mut dyn FnMut(),
    ) -> Option<Result<()>>;
    /// Run `f` holding `repo`'s write lock, waited for at most the lock
    /// wait and given up (`Busy`) once `abort` returns true.
    fn repo_bounded(
        &self,
        repo: &RepoName,
        abort: &dyn Fn() -> bool,
        f: &mut dyn FnMut(),
    ) -> Result<()>;
}

/// No locks: the caller excludes everything else.
struct NoLocks;

impl OldTreeLocks for NoLocks {
    fn try_session(&self, _: &SessionId, _: bool, f: &mut dyn FnMut()) -> Option<Result<()>> {
        f();
        Some(Ok(()))
    }
    fn repo_bounded(&self, _: &RepoName, _: &dyn Fn() -> bool, f: &mut dyn FnMut()) -> Result<()> {
        f();
        Ok(())
    }
}

/// What phase 0 read under the session place.
struct Facts {
    all: Vec<MoveIntent>,
    i: MoveIntent,
    cfg: RepoConfig,
    /// The `move_activity` row as phase 0 found it (before the suspension).
    row: Option<MoveActivity>,
    /// The clean passes phase 0 cleared.
    suspended: Vec<i64>,
    tok0: Token,
}

/// Phase 0's facts, or how the claim ends (with phase 0's token once the
/// identity checks passed).
type Phase0 = std::result::Result<Facts, (End, Option<Token>)>;

/// What phase 1 saw.
enum Seen {
    OpenFilesFailed,
    Seam {
        held: bool,
    },
    /// Stopped by a guard, a yield or shutdown; `late`: the activity
    /// probe had already found a change (no pass is put back then).
    Stopped {
        held: bool,
        late: bool,
    },
    Held {
        holders: Vec<OpenFile>,
        changed: bool,
        checked_at: SystemTime,
        compared: bool,
    },
    Free {
        probe: Probe,
        changed: bool,
        checked_at: SystemTime,
    },
}

enum Verdict {
    Keep,
    Remove,
    Set(OldTreeVerdict),
}

/// What a claim leaves behind.
struct End {
    out: Outcome,
    verdict: Verdict,
    /// A comparison ran: its identity enters the cooldown (§3.6).
    compared: bool,
    /// Busy, yielded, deferred or unreadable: retried (daemon only).
    retry: bool,
    /// Queue the session's `Auto` trees for their next pass.
    queue_auto: bool,
}

impl End {
    fn skip(why: &'static str, verdict: Verdict, retry: bool, queue_auto: bool) -> End {
        End {
            out: Outcome::Skipped(why),
            verdict,
            compared: false,
            retry,
            queue_auto,
        }
    }
    fn busy() -> End {
        End::skip("busy", Verdict::Keep, true, false)
    }
    fn yielded() -> End {
        End::skip("yielded", Verdict::Keep, true, false)
    }
}

/// Ends a claim that unwinds: out of the flight table, a recheck queued.
struct Exit<'s, 'a> {
    s: &'s Sessions<'a>,
    key: OldTreeKey,
    retries: bool,
    done: bool,
}

impl Drop for Exit<'_, '_> {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let mut st = lock(&self.s.old_trees.state);
        let recheck = st.in_flight.remove(&self.key).and_then(|f| f.recheck);
        if self.retries {
            st.retries.remove(&self.key);
        }
        if let Some(r) = recheck {
            st.pending.entry(self.key.clone()).or_insert(r);
        }
        drop(st);
        self.s.old_trees.exits.notify_all();
        self.s.old_trees.wake.notify_all();
    }
}

impl<'a> Sessions<'a> {
    /// One check of a queued old tree judged in place without locks (tests
    /// and callers that exclude all other work): the session must be
    /// Active and the intent still Done at the same path and inode, else
    /// it is dropped (`Skipped`). `stop` gives up (`Skipped("stopped")`, no
    /// verdict). A retained tree gets a verdict but is never retired. No
    /// retry is scheduled.
    pub fn check_old_tree(
        &self,
        key: &OldTreeKey,
        path: &Path,
        ino: u64,
        stop: &Arc<AtomicBool>,
    ) -> Result<Outcome> {
        let claim = {
            let mut st = lock(&self.old_trees.state);
            if st.in_flight.contains_key(key) {
                return Ok(Outcome::Skipped("in_flight"));
            }
            self.claim_locked(&mut st, key.clone(), path.to_path_buf(), ino)
        };
        self.judge(claim, &NoLocks, stop, None)
    }

    /// The worker's run of `claim` (new-lock §3.1) with `locks`; `shutdown`
    /// stops it (`Skipped("stopped")`, nothing retried). A busy, yielded,
    /// deferred or unreadable claim is retried after `retry` (ten times
    /// that from the third attempt on); any other end clears the retry.
    pub fn run_old_tree_claim(
        &self,
        claim: OldTreeClaim,
        locks: &dyn OldTreeLocks,
        shutdown: &Arc<AtomicBool>,
        retry: Duration,
    ) -> Result<Outcome> {
        self.judge(claim, locks, shutdown, Some(retry))
    }

    fn judge(
        &self,
        claim: OldTreeClaim,
        locks: &dyn OldTreeLocks,
        stop: &Arc<AtomicBool>,
        retry: Option<Duration>,
    ) -> Result<Outcome> {
        let mut exit = Exit {
            s: self,
            key: claim.key.clone(),
            retries: retry.is_some(),
            done: false,
        };
        let observed = observe::begin(Work::OldTreeCheck {
            repo: claim.key.repo.clone(),
            session: claim.key.session.clone(),
            n: claim.key.n,
        });
        let r = strata_apfs::with_walk_stop(stop.clone(), || self.phases(&claim, locks, &observed));
        let stopped = stop.load(Ordering::SeqCst);
        let (end, tok0, error) = match r {
            Ok((end, tok0)) => (end, tok0, None),
            Err(e) => {
                let busy = e.code == Code::Busy;
                (
                    End::skip("error", Verdict::Keep, busy, false),
                    None,
                    Some(e),
                )
            }
        };
        // A stop after the identity checks writes nothing (as before).
        let end = match end.out {
            Outcome::Retired => end,
            _ if stopped && tok0.is_some() => End::skip("stopped", Verdict::Keep, false, false),
            _ => end,
        };
        self.complete(&claim, &end, tok0, retry, stopped);
        exit.done = true;
        match error {
            Some(e) => Err(e),
            None => Ok(end.out),
        }
    }

    /// The phases; the token is phase 0's when its checks passed.
    fn phases(
        &self,
        c: &OldTreeClaim,
        locks: &dyn OldTreeLocks,
        observed: &observe::Observed,
    ) -> Result<(End, Option<Token>)> {
        observed.step("revalidate");
        let mut p0 = None;
        match locks.try_session(&c.key.session, true, &mut || p0 = Some(self.phase0(c))) {
            None => return Ok((End::busy(), None)),
            Some(Err(e)) => return Err(e),
            Some(Ok(())) => {}
        }
        let f = match p0.unwrap_or_else(|| Ok(Err((End::busy(), None))))? {
            Ok(f) => f,
            Err((end, tok0)) => return Ok((end, tok0)),
        };
        self.set_claim_state(&c.key, ClaimState::Phase1);
        observed.step("walk");
        let seen = strata_apfs::with_walk_stop(c.stop.clone(), || self.phase1(c, &f));
        self.set_claim_state(&c.key, ClaimState::Between);
        let end = match seen {
            Seen::OpenFilesFailed => {
                self.restore_after(c, &f, locks);
                End::skip("open_files_failed", Verdict::Remove, false, true)
            }
            Seen::Seam { held } => {
                if !held {
                    self.restore_after(c, &f, locks);
                }
                End::skip("seam", Verdict::Remove, false, false)
            }
            Seen::Stopped { held, late } => {
                observed.step("restore");
                if !held && !late {
                    self.restore_after(c, &f, locks);
                }
                End::yielded()
            }
            Seen::Held {
                holders,
                changed,
                checked_at,
                compared,
            } => End {
                out: Outcome::Kept {
                    holders: holders.clone(),
                    changed: Some(changed),
                },
                verdict: Verdict::Set(self.verdict_of(c, changed, holders, checked_at)),
                compared,
                retry: false,
                queue_auto: true,
            },
            Seen::Free {
                probe,
                changed,
                checked_at,
            } => {
                // Between the phases (tests register guards here).
                let _ = self.seams.hit("old_tree.between", Some(&c.key.repo));
                if c.routed.load(Ordering::SeqCst) {
                    observed.step("restore");
                    self.persist_finding(c, &f, &probe);
                    if !probe.late() {
                        self.restore_after(c, &f, locks);
                    }
                    End::yielded()
                } else {
                    observed.step("commit");
                    self.phase2(c, &f, locks, &probe, changed, checked_at)
                }
            }
        };
        Ok((end, Some(f.tok0)))
    }

    /// Phase 0, under the session place: the identity checks, the stop
    /// check, the row and the suspension of its clean passes. A skip after
    /// the identity checks carries the token (a stop then ends `stopped`).
    fn phase0(&self, c: &OldTreeClaim) -> Result<Phase0> {
        let tok0 = (lock(&self.old_trees.state).bumps(&c.key), c.epoch);
        let skip = |why| Ok(Err((End::skip(why, Verdict::Keep, false, false), None)));
        let Some(rec) = self.store.session(&c.key.session)? else {
            return skip("no_session");
        };
        if rec.lifecycle != Lifecycle::Active || !rec.manifest.repos.contains_key(&c.key.repo) {
            return skip("not_active");
        }
        let all = self.store.move_intents(&c.key.session)?;
        let Some(i) = all
            .iter()
            .find(|i| i.repo == c.key.repo && i.n == c.key.n)
            .cloned()
        else {
            return skip("no_intent");
        };
        if i.state != MoveState::Done || old_path(&i) != c.path || ino_of(&c.path) != Some(c.ino) {
            return skip("intent_changed");
        }
        let cfg = self.repo_cfg(&c.key.repo)?.clone();
        if strata_apfs::walk_stopped() {
            return Ok(Err((End::yielded(), Some(tok0))));
        }
        // Crash-safe reset before the first scan (G4 item 5).
        let row = match self
            .seams
            .hit("old_tree.read_passes", Some(&i.repo))
            .and_then(|()| self.store.move_activity(&i.session, &i.repo, i.n))
        {
            Ok(row) => row,
            Err(_) => {
                let end = End::skip("passes_read_failed", Verdict::Remove, true, true);
                return Ok(Err((end, Some(tok0))));
            }
        };
        let suspended = row.as_ref().map(|r| r.passes.clone()).unwrap_or_default();
        if !suspended.is_empty() && !self.suspend_passes(&i) {
            let end = End::skip("passes_write_failed", Verdict::Remove, false, true);
            return Ok(Err((end, Some(tok0))));
        }
        Ok(Ok(Facts {
            all,
            i,
            cfg,
            row,
            suspended,
            tok0,
        }))
    }

    /// Phase 1, holding no lock: the open-files scan, then a held tree's
    /// comparison (or its fresh cached verdict) or a free tree's activity
    /// probe and comparison. Reads only.
    fn phase1(&self, c: &OldTreeClaim, f: &Facts) -> Seen {
        let i = &f.i;
        let holders = match strata_deps::open_files(&c.path) {
            Ok(h) => h,
            Err(_) => return Seen::OpenFilesFailed,
        };
        let held = !holders.is_empty();
        if self
            .seams
            .hit("old_tree.after_scan", Some(&i.repo))
            .is_err()
        {
            return Seen::Seam { held };
        }
        if strata_apfs::walk_stopped() {
            return Seen::Stopped { held, late: false };
        }
        let started = SystemTime::now();
        if held {
            let cached = lock(&self.old_trees.state)
                .verdicts
                .get(&c.key)
                .filter(|j| {
                    j.verdict.path == c.path
                        && j.verdict.ino == c.ino
                        && j.token == f.tok0
                        && j.verdict
                            .checked_at
                            .elapsed()
                            .is_ok_and(|age| age < OLD_TREE_TTL)
                })
                .map(|j| j.verdict.clone());
            let (changed, checked_at, compared) = match cached {
                Some(v) => (v.changed, v.checked_at, false),
                None => {
                    if self
                        .seams
                        .hit("old_tree.before_compare", Some(&i.repo))
                        .is_err()
                    {
                        return Seen::Seam { held };
                    }
                    (
                        !self.old_tree_matches(i, &f.cfg).unwrap_or(false),
                        started,
                        true,
                    )
                }
            };
            if strata_apfs::walk_stopped() {
                return Seen::Stopped { held, late: false };
            }
            return Seen::Held {
                holders,
                changed,
                checked_at,
                compared,
            };
        }
        let probe = self.auto_probe(i, &c.path, f.row.as_ref());
        if matches!(probe, Probe::Stopped) || strata_apfs::walk_stopped() {
            return Seen::Stopped {
                held: false,
                late: probe.late(),
            };
        }
        let changed = probe.late() || !self.old_tree_matches(i, &f.cfg).unwrap_or(false);
        if strata_apfs::walk_stopped() {
            return Seen::Stopped {
                held: false,
                late: probe.late(),
            };
        }
        Seen::Free {
            probe,
            changed,
            checked_at: started,
        }
    }

    /// Phase 2: the session place without waiting, then the repo lock
    /// within the lock wait (given up when a guard registers). Busy ⇒ the
    /// judgement is discarded after 2N and 2′, and retried.
    fn phase2(
        &self,
        c: &OldTreeClaim,
        f: &Facts,
        locks: &dyn OldTreeLocks,
        probe: &Probe,
        changed: bool,
        checked_at: SystemTime,
    ) -> End {
        let mut end = None;
        let r = locks.try_session(&c.key.session, false, &mut || {
            if c.routed.load(Ordering::SeqCst) {
                // Routed by a guard: 2N, then 2′ under the place held here.
                self.persist_finding(c, f, probe);
                if !probe.late() {
                    self.restore_checked(c, f);
                }
                end = Some(End::yielded());
                return;
            }
            self.set_claim_state(&c.key, ClaimState::Revalidating);
            let routed = c.routed.clone();
            let mut locked = None;
            let taken =
                locks.repo_bounded(&c.key.repo, &|| routed.load(Ordering::SeqCst), &mut || {
                    locked = Some(strata_apfs::with_walk_stop(c.stop.clone(), || {
                        self.phase2_locked(c, f, probe, changed, checked_at)
                    }));
                });
            end = Some(match (taken, locked) {
                (Ok(()), Some(e)) => e,
                _ => {
                    self.persist_finding(c, f, probe);
                    if !probe.late() {
                        self.restore_checked(c, f);
                    }
                    if c.routed.load(Ordering::SeqCst) {
                        End::yielded()
                    } else {
                        End::busy()
                    }
                }
            });
        });
        match (r, end) {
            (Some(Ok(())), Some(end)) => end,
            _ => {
                self.persist_finding(c, f, probe);
                if !probe.late() {
                    self.restore_after(c, f, locks);
                }
                End::busy()
            }
        }
    }

    /// Phase 2 under both locks: revalidate, then today's rules in today's
    /// order with the stop check after the final scan.
    fn phase2_locked(
        &self,
        c: &OldTreeClaim,
        f: &Facts,
        probe: &Probe,
        changed: bool,
        checked_at: SystemTime,
    ) -> End {
        let i = &f.i;
        if !self.unchanged(c, f) {
            // Passes stay cleared, no notice, the earlier verdict stays.
            return End::skip("invalidated", Verdict::Keep, false, true);
        }
        match probe {
            Probe::LateWrites(v) => self.write_late_notice(i, v, "late_writes"),
            Probe::BaselineMissing => self.write_late_notice(i, &[], "baseline_missing"),
            Probe::WalkError(m) => {
                self.write_late_notice(i, &[format!("(walk error: {m})")], "walk_error")
            }
            _ => {}
        }
        let kept = |holders: Vec<OpenFile>, changed: bool| End {
            out: Outcome::Kept {
                holders: holders.clone(),
                changed: Some(changed),
            },
            verdict: Verdict::Set(self.verdict_of(c, changed, holders, checked_at)),
            compared: true,
            retry: false,
            queue_auto: true,
        };
        if changed || i.old_tree.is_some() {
            // A late notice clears its clean history for good.
            if !probe.late() {
                self.restore_passes(i, &f.suspended);
            }
            return kept(Vec::new(), changed);
        }
        // Two consecutive clean passes at least Q apart before an `Auto`
        // tree is retired; this one is recorded (a restart keeps it).
        if matches!(probe, Probe::Clean) {
            let now = crate::activity::now_ns();
            let q = Duration::from_secs(f.cfg.rebase.quiet_secs).as_nanos() as i64;
            match f.suspended.last() {
                Some(last) if now.saturating_sub(*last) >= q => {}
                Some(_) => {
                    self.restore_passes(i, &f.suspended);
                    return kept(Vec::new(), false);
                }
                None => {
                    let _ = self.store.set_move_passes(&i.session, &i.repo, i.n, &[now]);
                    return kept(Vec::new(), false);
                }
            }
        }
        if self
            .seams
            .hit("old_tree.final_scan", Some(&i.repo))
            .is_err()
        {
            self.restore_passes(i, &f.suspended);
            return End::skip("seam", Verdict::Remove, false, false);
        }
        match strata_deps::open_files(&c.path) {
            Ok(h) if h.is_empty() && ino_of(&c.path) == Some(c.ino) => {}
            Ok(h) => {
                // A hold seen by the final scan leaves the passes cleared;
                // an inode change alone puts them back.
                if h.is_empty() {
                    self.restore_passes(i, &f.suspended);
                }
                return kept(h, false);
            }
            Err(_) => {
                self.restore_passes(i, &f.suspended);
                return End::skip("open_files_failed", Verdict::Remove, false, true);
            }
        }
        if strata_apfs::walk_stopped() {
            self.restore_passes(i, &f.suspended);
            return End::yielded();
        }
        if !self.enter_committing(c, f) {
            self.restore_passes(i, &f.suspended);
            return End::skip("deferred", Verdict::Keep, true, false);
        }
        let admin = self
            .shadow_dir(&i.session, &i.repo)
            .join("worktrees")
            .join(prior_generation(&f.all, i).to_string());
        if self.store.retire_dir(&i.staging).is_err() {
            self.restore_passes(i, &f.suspended);
            return kept(Vec::new(), false);
        }
        let _ = self.retire_present(&admin);
        End {
            out: Outcome::Retired,
            verdict: Verdict::Remove,
            compared: false,
            retry: false,
            queue_auto: false,
        }
    }

    /// Phase 0's facts still hold: the session, the intent at the same
    /// path and inode, the token, and the row (notice, baseline, passes
    /// still cleared).
    fn unchanged(&self, c: &OldTreeClaim, f: &Facts) -> bool {
        let Ok(Some(rec)) = self.store.session(&c.key.session) else {
            return false;
        };
        if rec.lifecycle != Lifecycle::Active || !rec.manifest.repos.contains_key(&c.key.repo) {
            return false;
        }
        let Ok(all) = self.store.move_intents(&c.key.session) else {
            return false;
        };
        let Some(i) = all.iter().find(|i| i.repo == c.key.repo && i.n == c.key.n) else {
            return false;
        };
        if i.state != MoveState::Done
            || i.old_tree != f.i.old_tree
            || i.staging != f.i.staging
            || ino_of(&c.path) != Some(c.ino)
        {
            return false;
        }
        if self.token_locked(&lock(&self.old_trees.state), &c.key) != f.tok0 {
            return false;
        }
        match (
            &f.row,
            self.store
                .move_activity(&c.key.session, &c.key.repo, c.key.n),
        ) {
            (None, Ok(None)) => true,
            (Some(a), Ok(Some(b))) => {
                a.late_writes == b.late_writes && a.baseline == b.baseline && b.passes.is_empty()
            }
            _ => false,
        }
    }

    /// Only under the mutex and only without a guard (§3.4): the claim may
    /// rename the tree away.
    fn enter_committing(&self, c: &OldTreeClaim, f: &Facts) -> bool {
        let mut st = lock(&self.old_trees.state);
        if self.token_locked(&st, &c.key) != f.tok0
            || st.guarded(&c.key.session)
            || c.routed.load(Ordering::SeqCst)
        {
            return false;
        }
        if let Some(fl) = st.in_flight.get_mut(&c.key) {
            fl.state = ClaimState::Committing;
        }
        true
    }

    /// 2N (r5 B3, §15.3): a late-write or missing-baseline finding of a
    /// completed phase 1 is written even without the locks, in one store
    /// transaction conditioned on the intent and row phase 0 saw, after a
    /// token check under the queue mutex. The first notice stands.
    fn persist_finding(&self, c: &OldTreeClaim, f: &Facts, probe: &Probe) {
        let (paths, reason): (&[String], &str) = match probe {
            Probe::LateWrites(v) => (v, "late_writes"),
            Probe::BaselineMissing => (&[], "baseline_missing"),
            _ => return,
        };
        if ino_of(&c.path) != Some(c.ino) {
            return;
        }
        let notice = self.notice_json(&f.i, paths, reason);
        if self.token_locked(&lock(&self.old_trees.state), &c.key) != f.tok0 {
            return;
        }
        let baseline = f.row.as_ref().and_then(|r| r.baseline.as_ref());
        let _ = self.store.set_move_late_writes_if(
            &f.i.session,
            &f.i.repo,
            f.i.n,
            &notice,
            &f.i.staging,
            f.i.old_tree.as_deref(),
            baseline,
        );
    }

    /// 2′: put the suspended clean passes back under the session place,
    /// taken only if free, if phase 0's facts still hold; otherwise (or
    /// busy) they stay cleared, failing closed.
    fn restore_after(&self, c: &OldTreeClaim, f: &Facts, locks: &dyn OldTreeLocks) {
        if f.suspended.is_empty() {
            return;
        }
        let _ = locks.try_session(&c.key.session, false, &mut || self.restore_checked(c, f));
    }

    /// 2′'s body, under the session place.
    fn restore_checked(&self, c: &OldTreeClaim, f: &Facts) {
        if !f.suspended.is_empty() && self.unchanged(c, f) {
            self.restore_passes(&f.i, &f.suspended);
        }
    }

    fn verdict_of(
        &self,
        c: &OldTreeClaim,
        changed: bool,
        holders: Vec<OpenFile>,
        checked_at: SystemTime,
    ) -> OldTreeVerdict {
        OldTreeVerdict {
            path: c.path.clone(),
            ino: c.ino,
            changed,
            holders,
            checked_at,
        }
    }

    /// Complete the claim under the mutex (§3.6): the verdict (set only
    /// under the token it was made with), the cooldown, the retry
    /// bookkeeping (daemon only), a recheck; then wake guards and worker.
    fn complete(
        &self,
        c: &OldTreeClaim,
        end: &End,
        tok0: Option<Token>,
        retry: Option<Duration>,
        stopped: bool,
    ) {
        let mut st = lock(&self.old_trees.state);
        let recheck = st.in_flight.remove(&c.key).and_then(|f| f.recheck);
        let current = self.token_locked(&st, &c.key);
        let valid = tok0 == Some(current);
        match &end.verdict {
            Verdict::Keep => {}
            Verdict::Remove => {
                st.verdicts.remove(&c.key);
            }
            Verdict::Set(v) => {
                if valid {
                    st.verdicts.insert(
                        c.key.clone(),
                        Judged {
                            verdict: v.clone(),
                            token: current,
                        },
                    );
                }
            }
        }
        if end.compared && valid {
            st.completed.insert(
                c.key.clone(),
                Completed {
                    path: c.path.clone(),
                    ino: c.ino,
                    token: current,
                    at: Instant::now(),
                },
            );
        }
        if let Some(delay) = retry {
            if end.retry && !stopped {
                st.retry(c.key.clone(), c.path.clone(), c.ino, delay);
            } else {
                st.retries.remove(&c.key);
            }
        }
        if let Some(r) = recheck {
            st.pending.entry(c.key.clone()).or_insert(r);
        }
        drop(st);
        self.old_trees.exits.notify_all();
        self.old_trees.wake.notify_all();
        if end.queue_auto {
            self.queue_auto_trees(&c.key.session);
        }
    }
}
