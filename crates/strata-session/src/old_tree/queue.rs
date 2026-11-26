//! new-lock §3.6: the old-tree worker's queue state behind one mutex: the
//! verdict table, the queue (`pending`, `delayed`, `retries`), the claims in
//! flight, the judgement tokens (§3.3), the completed-claim cooldown and the
//! foreground guards (§3.4).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use strata_deps::RepoWriteLock;
use strata_store::{MoveIntent, MoveState};
use strata_types::{RepoName, SessionId};

use super::{OLD_TREE_TTL, OldTreeKey, OldTreeMode, OldTreeVerdict, ino_of, key_of, old_path};
use crate::{Result, Sessions};

/// A judgement's identity besides path and inode (§3.3): how often the
/// intent was invalidated in this process, and its repo's config epoch.
pub(crate) type Token = (u64, u64);

/// How long a blocking guard sleeps between looks at the claims it waits
/// for; each look wakes their repo waits again.
const GUARD_POLL: Duration = Duration::from_millis(50);

#[derive(Default)]
pub(crate) struct OldTrees {
    pub(super) mode: Mutex<OldTreeMode>,
    pub(super) state: Mutex<QueueState>,
    /// Work was queued or a guard dropped: the worker looks again.
    pub(super) wake: Condvar,
    /// A claim exited: blocking guards look again.
    pub(super) exits: Condvar,
}

#[derive(Default)]
pub(crate) struct QueueState {
    pub(super) verdicts: BTreeMap<OldTreeKey, Judged>,
    /// Queued keys with the path and inode they were queued for.
    pub(super) pending: BTreeMap<OldTreeKey, (PathBuf, u64)>,
    /// rm-followups §1.4 G4 item 4: keys queued with a not-before time (an
    /// `Auto` tree's next pass); moved to `pending` once due.
    pub(super) delayed: BTreeMap<OldTreeKey, (Instant, PathBuf, u64)>,
    /// Loop retries take precedence over status and Q scheduling. Entries
    /// stay while a check runs so an interrupted retry retains its count.
    pub(super) retries: BTreeMap<OldTreeKey, Retry>,
    pub(super) in_flight: BTreeMap<OldTreeKey, Flight>,
    /// Invalidations per intent, per (session, repo) and per session; a
    /// token is their sum.
    tokens: BTreeMap<OldTreeKey, u64>,
    scopes: BTreeMap<(SessionId, RepoName), u64>,
    sessions: BTreeMap<SessionId, u64>,
    pub(super) completed: BTreeMap<OldTreeKey, Completed>,
    pub(super) foreground: BTreeMap<SessionId, Guards>,
}

/// A verdict with the token it was made under.
#[derive(Clone)]
pub(super) struct Judged {
    pub(super) verdict: OldTreeVerdict,
    pub(super) token: Token,
}

pub(super) struct Retry {
    pub(super) at: Option<Instant>,
    pub(super) path: PathBuf,
    pub(super) ino: u64,
    pub(super) attempts: u32,
}

/// Where a claim is (§3.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClaimState {
    Phase0,
    Phase1,
    Between,
    Revalidating,
    Committing,
}

/// A claim in flight.
pub(super) struct Flight {
    pub(super) path: PathBuf,
    pub(super) ino: u64,
    pub(super) token: Token,
    pub(super) state: ClaimState,
    /// Stops its walks (a guard, §3.4).
    pub(super) stop: Arc<AtomicBool>,
    /// A guard registered: no retiring phase 2, its repo wait gives up.
    pub(super) routed: Arc<AtomicBool>,
    /// A changed identity was hinted or invalidated meanwhile: queued as
    /// due when the claim completes.
    pub(super) recheck: Option<(PathBuf, u64)>,
}

/// The last claim of a key that ran a comparison, and when it completed.
pub(super) struct Completed {
    pub(super) path: PathBuf,
    pub(super) ino: u64,
    pub(super) token: Token,
    pub(super) at: Instant,
}

/// The foreground guards of one session: registered (claims of the session
/// still being stopped) and active.
#[derive(Default)]
pub(super) struct Guards {
    pending: usize,
    active: usize,
}

impl QueueState {
    /// The invalidation part of `key`'s token.
    pub(super) fn bumps(&self, key: &OldTreeKey) -> u64 {
        self.tokens.get(key).copied().unwrap_or(0)
            + self
                .scopes
                .get(&(key.session.clone(), key.repo.clone()))
                .copied()
                .unwrap_or(0)
            + self.sessions.get(&key.session).copied().unwrap_or(0)
    }

    /// Whether `session` has a foreground guard, pending or active.
    pub(super) fn guarded(&self, session: &SessionId) -> bool {
        self.foreground.contains_key(session)
    }

    /// A changed identity during a claim: queue the key again once the
    /// claim completes.
    fn recheck_in_flight(&mut self, pick: impl Fn(&OldTreeKey) -> bool) {
        for (k, f) in self.in_flight.iter_mut() {
            if pick(k) {
                f.recheck = Some((f.path.clone(), f.ino));
            }
        }
    }

    /// Schedule a retry of `key` after `delay` (ten times that from the
    /// third consecutive attempt on).
    pub(super) fn retry(
        &mut self,
        key: OldTreeKey,
        path: PathBuf,
        ino: u64,
        delay: Duration,
    ) -> Duration {
        let retry = self.retries.entry(key.clone()).or_insert(Retry {
            at: None,
            path,
            ino,
            attempts: 0,
        });
        retry.attempts = retry.attempts.saturating_add(1);
        let delay = if retry.attempts >= 3 {
            10 * delay
        } else {
            delay
        };
        retry.at = Some(Instant::now() + delay);
        self.pending.remove(&key);
        delay
    }

    /// The delayed keys and retries whose time has come, moved to the
    /// queue; a key in flight waits where it is (§3.6). A Q pass due while
    /// a retry of the key exists leaves it to the retry.
    fn promote_due(&mut self) {
        let now = Instant::now();
        let due: Vec<OldTreeKey> = self
            .delayed
            .iter()
            .filter(|(k, (at, ..))| *at <= now && !self.in_flight.contains_key(*k))
            .map(|(k, _)| k.clone())
            .collect();
        for k in due {
            if let Some((_, p, ino)) = self.delayed.remove(&k)
                && !self.retries.contains_key(&k)
            {
                self.pending.entry(k).or_insert((p, ino));
            }
        }
        for (key, retry) in self.retries.iter_mut() {
            if retry.at.is_some_and(|at| at <= now) && !self.in_flight.contains_key(key) {
                retry.at = None;
                self.pending
                    .entry(key.clone())
                    .or_insert((retry.path.clone(), retry.ino));
            }
        }
    }
}

/// A foreground guard (§3.4): while it lives, no claim of its sessions
/// starts and none enters `Committing`. Dropping it lets them claim again.
pub struct OldTreeGuard<'s> {
    trees: &'s OldTrees,
    sessions: Vec<SessionId>,
}

impl Drop for OldTreeGuard<'_> {
    fn drop(&mut self) {
        let mut st = lock(&self.trees.state);
        for s in &self.sessions {
            if let Some(g) = st.foreground.get_mut(s) {
                g.active = g.active.saturating_sub(1);
                if g.active == 0 && g.pending == 0 {
                    st.foreground.remove(s);
                }
            }
        }
        drop(st);
        self.trees.wake.notify_all();
    }
}

/// One claimed old tree (§3.6): in flight from [`Sessions::claim_old_tree`]
/// until it is run or released.
#[derive(Debug)]
pub struct OldTreeClaim {
    pub key: OldTreeKey,
    pub path: PathBuf,
    pub ino: u64,
    pub(super) stop: Arc<AtomicBool>,
    pub(super) routed: Arc<AtomicBool>,
    /// The repo's config epoch, read before the claimant admits a catalog.
    pub(super) epoch: u64,
}

pub(super) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl<'a> Sessions<'a> {
    /// The daemon and the local fallback set [`OldTreeMode::Deferred`].
    pub fn set_old_tree_mode(&self, mode: OldTreeMode) {
        *lock(&self.old_trees.mode) = mode;
    }

    pub(crate) fn old_tree_mode(&self) -> OldTreeMode {
        *lock(&self.old_trees.mode)
    }

    /// `key`'s current token (§3.3).
    pub(super) fn token_locked(&self, st: &QueueState, key: &OldTreeKey) -> Token {
        (st.bumps(key), self.locks.config_epoch(&key.repo))
    }

    /// §3.3: a judgement or write of intent `key` that is not the worker's
    /// own claim (a foreground judgement, step 19's notice, step 20).
    pub(crate) fn bump_old_tree_token(&self, key: &OldTreeKey) {
        let mut st = lock(&self.old_trees.state);
        *st.tokens.entry(key.clone()).or_default() += 1;
        st.recheck_in_flight(|k| k == key);
    }

    /// Status step 2.9 in Deferred mode: the verdict for the tree at
    /// `path` (inode `ino`), if one was made for this very tree, and the
    /// queueing rules of §3.6: a claim of the same identity in flight, a
    /// fresh verdict, a completed claim younger than [`OLD_TREE_TTL`] or a
    /// scheduled retry of the same tree leave it alone; a claim of another
    /// identity in flight gets one recheck. A stale verdict is still
    /// returned (its `checked_at` shows its age).
    pub(crate) fn old_tree_hint(
        &self,
        i: &MoveIntent,
        path: &Path,
        ino: u64,
    ) -> Option<OldTreeVerdict> {
        let key = key_of(i);
        let mut st = lock(&self.old_trees.state);
        let token = self.token_locked(&st, &key);
        let shown = st
            .verdicts
            .get(&key)
            .filter(|j| j.verdict.path == path && j.verdict.ino == ino)
            .map(|j| j.verdict.clone());
        let same = |p: &Path, n: u64, t: Token| p == path && n == ino && t == token;
        if let Some(f) = st.in_flight.get_mut(&key) {
            if !same(&f.path, f.ino, f.token) {
                f.recheck = Some((path.to_path_buf(), ino));
            }
            return shown;
        }
        let fresh = st.verdicts.get(&key).is_some_and(|j| {
            same(&j.verdict.path, j.verdict.ino, j.token)
                && j.verdict
                    .checked_at
                    .elapsed()
                    .is_ok_and(|age| age < OLD_TREE_TTL)
        });
        let cooling = st
            .completed
            .get(&key)
            .is_some_and(|c| same(&c.path, c.ino, c.token) && c.at.elapsed() < OLD_TREE_TTL);
        // The token is not part of a retry's identity: an invalidation must
        // not bypass the backoff of the same tree (move_workspace.rs:1336).
        let retrying = st
            .retries
            .get(&key)
            .is_some_and(|r| r.path == path && r.ino == ino);
        if !(fresh || cooling || retrying) {
            st.pending.insert(key, (path.to_path_buf(), ino));
            drop(st);
            self.old_trees.wake.notify_all();
        }
        shown
    }

    fn queue_old_tree(&self, key: OldTreeKey, path: PathBuf, ino: u64) {
        let mut st = lock(&self.old_trees.state);
        if st.retries.contains_key(&key) {
            return;
        }
        if let Some(f) = st.in_flight.get_mut(&key) {
            if f.path != path || f.ino != ino {
                f.recheck = Some((path, ino));
            }
            return;
        }
        st.pending.insert(key, (path, ino));
        drop(st);
        self.old_trees.wake.notify_all();
    }

    /// The verdicts of `session` are void (after a restore or rm) and every
    /// intent's token changes; claims in flight are rechecked. Scheduled
    /// checks stay, including retries of other repos' Explicit trees. Each
    /// check revalidates the lifecycle, intent, path and inode.
    pub fn forget_old_trees(&self, session: &SessionId) {
        let mut st = lock(&self.old_trees.state);
        *st.sessions.entry(session.clone()).or_default() += 1;
        st.verdicts.retain(|k, _| &k.session != session);
        st.completed.retain(|k, _| &k.session != session);
        st.recheck_in_flight(|k| &k.session == session);
    }

    /// [`Sessions::forget_old_trees`] for the intents of one repo (a move
    /// of `repo` changes only its trees, §3.3).
    pub fn forget_old_trees_of(&self, session: &SessionId, repo: &RepoName) {
        let mut st = lock(&self.old_trees.state);
        *st.scopes
            .entry((session.clone(), repo.clone()))
            .or_default() += 1;
        let mine = |k: &OldTreeKey| &k.session == session && &k.repo == repo;
        st.verdicts.retain(|k, _| !mine(k));
        st.completed.retain(|k, _| !mine(k));
        st.recheck_in_flight(mine);
    }

    /// Schedule a Q-driven check at `at`, preserving an earlier schedule
    /// and any immediate status request. Loop retries have their own clock.
    pub fn queue_old_tree_at(&self, key: OldTreeKey, path: PathBuf, ino: u64, at: Instant) {
        let mut st = lock(&self.old_trees.state);
        match st.delayed.get(&key) {
            Some((due, ..)) if *due <= at => {}
            _ => {
                st.delayed.insert(key, (at, path, ino));
            }
        }
    }

    /// A busy, yielded or unreadable check backs off independently of Q.
    /// Only a completed judgement resets the consecutive retry count.
    pub fn retry_old_tree(
        &self,
        key: OldTreeKey,
        path: PathBuf,
        ino: u64,
        delay: Duration,
    ) -> Duration {
        lock(&self.old_trees.state).retry(key, path, ino, delay)
    }

    /// Completed or invalid work no longer needs its retry history.
    pub fn finish_old_tree_retry(&self, key: &OldTreeKey) {
        lock(&self.old_trees.state).retries.remove(key);
    }

    /// The current verdict for `key` (tests, diagnostics).
    pub fn old_tree_verdict(&self, key: &OldTreeKey) -> Option<OldTreeVerdict> {
        lock(&self.old_trees.state)
            .verdicts
            .get(key)
            .map(|j| j.verdict.clone())
    }

    /// Queue every done move's existing old tree of every live session
    /// (daemon startup, after recover).
    pub fn queue_all_old_trees(&self) -> Result<()> {
        for id in self.list()? {
            for i in self.store.move_intents(&id)? {
                if i.state != MoveState::Done {
                    continue;
                }
                let path = old_path(&i);
                if let Some(ino) = ino_of(&path) {
                    self.queue_old_tree(key_of(&i), path, ino);
                }
            }
        }
        Ok(())
    }

    /// The next queued old tree, waiting up to `wait` for one (tests; the
    /// daemon claims with [`Sessions::claim_old_tree`]).
    pub fn next_old_tree(&self, wait: Duration) -> Option<(OldTreeKey, PathBuf, u64)> {
        let mut st = lock(&self.old_trees.state);
        st.promote_due();
        if st.pending.is_empty() {
            st = self
                .old_trees
                .wake
                .wait_timeout(st, wait)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
        let next = st.pending.pop_first();
        if let Some((k, _)) = &next {
            st.delayed.remove(k);
        }
        next.map(|(k, (p, ino))| (k, p, ino))
    }

    /// Put `key` back at the end of the queue (its session was busy).
    pub fn requeue_old_tree(&self, key: OldTreeKey, path: PathBuf, ino: u64) {
        lock(&self.old_trees.state)
            .pending
            .entry(key)
            .or_insert((path, ino));
        self.old_trees.wake.notify_all();
    }

    /// §3.6: pop a due key and mark it in flight, atomically, waiting up to
    /// `wait` for one. A key of a session with a foreground guard is not
    /// popped; it stays queued until the guard drops.
    pub fn claim_old_tree(&self, wait: Duration) -> Option<OldTreeClaim> {
        let deadline = Instant::now() + wait;
        let mut st = lock(&self.old_trees.state);
        loop {
            st.promote_due();
            let next = st
                .pending
                .keys()
                .find(|k| !st.in_flight.contains_key(*k) && !st.guarded(&k.session))
                .cloned();
            if let Some(key) = next {
                let (path, ino) = st.pending.remove(&key).expect("a pending key");
                st.delayed.remove(&key);
                let claim = self.claim_locked(&mut st, key, path, ino);
                return Some(claim);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            // Retries and Q passes come due without a notification.
            st = self
                .old_trees
                .wake
                .wait_timeout(st, left.min(GUARD_POLL))
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }

    /// Mark `key` in flight for a caller that judges it in place.
    pub(super) fn claim_locked(
        &self,
        st: &mut QueueState,
        key: OldTreeKey,
        path: PathBuf,
        ino: u64,
    ) -> OldTreeClaim {
        let epoch = self.locks.config_epoch(&key.repo);
        let stop = Arc::new(AtomicBool::new(false));
        let routed = Arc::new(AtomicBool::new(false));
        st.in_flight.insert(
            key.clone(),
            Flight {
                path: path.clone(),
                ino,
                token: (st.bumps(&key), epoch),
                state: ClaimState::Phase0,
                stop: stop.clone(),
                routed: routed.clone(),
                recheck: None,
            },
        );
        OldTreeClaim {
            key,
            path,
            ino,
            stop,
            routed,
            epoch,
        }
    }

    /// Give a claim back unjudged (the drain refused the pass): it leaves
    /// the flight table and, with `requeue`, goes back to the queue.
    pub fn release_old_tree_claim(&self, claim: OldTreeClaim, requeue: bool) {
        let mut st = lock(&self.old_trees.state);
        let recheck = st.in_flight.remove(&claim.key).and_then(|f| f.recheck);
        if requeue {
            st.pending
                .entry(claim.key.clone())
                .or_insert((claim.path.clone(), claim.ino));
        }
        if let Some(r) = recheck {
            st.pending.entry(claim.key).or_insert(r);
        }
        drop(st);
        self.old_trees.exits.notify_all();
        self.old_trees.wake.notify_all();
    }

    /// Move claim `key` to `state` (activity and guard routing).
    pub(super) fn set_claim_state(&self, key: &OldTreeKey, state: ClaimState) {
        if let Some(f) = lock(&self.old_trees.state).in_flight.get_mut(key) {
            f.state = state;
        }
    }

    /// Whether a claim of `session` is in flight: the try-only guard of
    /// background callers (§3.4) defers then and registers nothing.
    pub fn old_tree_claim_in_flight(&self, session: &SessionId) -> bool {
        lock(&self.old_trees.state)
            .in_flight
            .keys()
            .any(|k| &k.session == session)
    }

    /// §3.4's blocking guard for `sessions`, taken before any session queue
    /// or repo lock: new claims of them stop at once, claims in flight are
    /// stopped or routed to their non-retiring exit, and this returns once
    /// none is in flight. With a `deadline` that passes first, the guard is
    /// withdrawn and `None` returned (the caller's deadline refusal).
    pub fn old_tree_guard(
        &self,
        sessions: &[SessionId],
        deadline: Option<Instant>,
    ) -> Option<OldTreeGuard<'_>> {
        let mut st = lock(&self.old_trees.state);
        for s in sessions {
            st.foreground.entry(s.clone()).or_default().pending += 1;
        }
        loop {
            let repos: Vec<RepoName> = st
                .in_flight
                .iter_mut()
                .filter(|(k, _)| sessions.contains(&k.session))
                .map(|(k, f)| {
                    f.stop.store(true, Ordering::SeqCst);
                    f.routed.store(true, Ordering::SeqCst);
                    k.repo.clone()
                })
                .collect();
            if repos.is_empty() {
                break;
            }
            let wait = match deadline {
                None => GUARD_POLL,
                Some(d) => {
                    let left = d.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        for s in sessions {
                            if let Some(g) = st.foreground.get_mut(s) {
                                g.pending = g.pending.saturating_sub(1);
                                if g.pending == 0 && g.active == 0 {
                                    st.foreground.remove(s);
                                }
                            }
                        }
                        drop(st);
                        self.old_trees.wake.notify_all();
                        return None;
                    }
                    left.min(GUARD_POLL)
                }
            };
            drop(st);
            // A claim in phase 2's repo wait looks at its abort again.
            for r in &repos {
                self.locks.wake(r);
            }
            st = lock(&self.old_trees.state);
            st = self
                .old_trees
                .exits
                .wait_timeout(st, wait)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
        for s in sessions {
            let g = st.foreground.entry(s.clone()).or_default();
            g.pending = g.pending.saturating_sub(1);
            g.active += 1;
        }
        Some(OldTreeGuard {
            trees: &self.old_trees,
            sessions: sessions.to_vec(),
        })
    }
}
