//! status-perf A1: judging a done move's old tree off the status path.
//!
//! One judgement, [`Sessions::collect_old_tree`], is shared by move step 20
//! and the daemon's background `old-tree` thread. Status in
//! [`OldTreeMode::Deferred`] only reads the verdict table this module keeps
//! and queues the intents it has no fresh verdict for. The table is a hint:
//! nothing is ever retired on a cached verdict, only on a fresh judgement
//! made under the session's queue place and the repo's write lock.

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use strata_deps::OpenFile;
use strata_store::{Lifecycle, MoveIntent, MoveState};
use strata_types::{RepoConfig, RepoName, SessionId};

use crate::move_workspace::prior_generation;
use crate::{Result, Sessions};

/// How long a verdict counts as fresh for status (design A1).
pub const OLD_TREE_TTL: Duration = Duration::from_secs(600);

/// Where status gets its old-tree judgement.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OldTreeMode {
    /// Judge in the request, as before status-perf (unit tests).
    #[default]
    Inline,
    /// Read the verdict table only; queue what is missing or stale.
    Deferred,
}

/// One old tree: a done move of `repo` in `session`, numbered `n`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct OldTreeKey {
    pub session: SessionId,
    pub repo: RepoName,
    pub n: u64,
}

/// A judgement of the tree at `path` (inode `ino`) made at `checked_at`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OldTreeVerdict {
    pub path: PathBuf,
    pub ino: u64,
    pub changed: bool,
    pub holders: Vec<OpenFile>,
    pub checked_at: SystemTime,
}

/// What [`Sessions::collect_old_tree`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Moved to the store trash (recoverable until gc), with its admin dir.
    Retired,
    /// Kept: held open, changed, retained, or the retire failed. `changed`
    /// is `None` when a held tree was not compared (step 20).
    Kept {
        holders: Vec<OpenFile>,
        changed: Option<bool>,
    },
    /// Not judged; no verdict is written.
    Skipped(&'static str),
}

/// The verdict table and the queue of the background thread.
#[derive(Default)]
pub(crate) struct OldTrees {
    mode: Mutex<OldTreeMode>,
    verdicts: Mutex<BTreeMap<OldTreeKey, OldTreeVerdict>>,
    /// Queued keys with the path and inode they were queued for.
    pending: Mutex<BTreeMap<OldTreeKey, (PathBuf, u64)>>,
    /// rm-followups §1.4 G4 item 4: keys queued with a not-before time
    /// (an `Auto` tree's next pass); moved to `pending` once due.
    delayed: Mutex<BTreeMap<OldTreeKey, (Instant, PathBuf, u64)>>,
    wake: Condvar,
}

/// rm-followups §1.4 G4 items 5–6: an old tree against its move's
/// activity baseline.
#[derive(Clone, Debug)]
pub(crate) enum AutoCheck {
    /// No `move_activity` row: an `Explicit` move (today's rules).
    Explicit,
    /// Changed since the baseline, a late-write notice already there, or
    /// no readable baseline: never retired.
    Late,
    /// Unchanged against the baseline; the row's clean passes.
    Clean(Vec<i64>),
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// The inode of `p` (not followed), `None` when it cannot be read.
pub(crate) fn ino_of(p: &Path) -> Option<u64> {
    std::fs::symlink_metadata(p).ok().map(|m| m.ino())
}

/// The tree a done move left: the retained path, else its staging.
pub(crate) fn old_path(i: &MoveIntent) -> PathBuf {
    i.old_tree.clone().unwrap_or_else(|| i.staging.clone())
}

fn key_of(i: &MoveIntent) -> OldTreeKey {
    OldTreeKey {
        session: i.session.clone(),
        repo: i.repo.clone(),
        n: i.n,
    }
}

impl<'a> Sessions<'a> {
    /// The daemon and the local fallback set [`OldTreeMode::Deferred`].
    pub fn set_old_tree_mode(&self, mode: OldTreeMode) {
        *lock(&self.old_trees.mode) = mode;
    }

    pub(crate) fn old_tree_mode(&self) -> OldTreeMode {
        *lock(&self.old_trees.mode)
    }

    /// Status step 2.9 in Deferred mode: the verdict for the tree at
    /// `path` (inode `ino`), if one was made for this very tree; queues the
    /// intent when there is none or it is older than [`OLD_TREE_TTL`]. A
    /// stale verdict is still returned (its `checked_at` shows its age).
    pub(crate) fn old_tree_hint(
        &self,
        i: &MoveIntent,
        path: &Path,
        ino: u64,
    ) -> Option<OldTreeVerdict> {
        let key = key_of(i);
        let v = lock(&self.old_trees.verdicts)
            .get(&key)
            .filter(|v| v.path == path && v.ino == ino)
            .cloned();
        let fresh = v
            .as_ref()
            .is_some_and(|v| v.checked_at.elapsed().is_ok_and(|age| age < OLD_TREE_TTL));
        if !fresh {
            self.queue_old_tree(key, path.to_path_buf(), ino);
        }
        v
    }

    fn queue_old_tree(&self, key: OldTreeKey, path: PathBuf, ino: u64) {
        lock(&self.old_trees.pending).insert(key, (path, ino));
        self.old_trees.wake.notify_all();
    }

    /// The verdicts of `session` are void (after a move, restore or rm).
    pub fn forget_old_trees(&self, session: &SessionId) {
        lock(&self.old_trees.verdicts).retain(|k, _| &k.session != session);
        lock(&self.old_trees.pending).retain(|k, _| &k.session != session);
        lock(&self.old_trees.delayed).retain(|k, _| &k.session != session);
    }

    /// Queue `key` for a check no earlier than `at` (an earlier due time
    /// already queued stands).
    pub(crate) fn queue_old_tree_at(&self, key: OldTreeKey, path: PathBuf, ino: u64, at: Instant) {
        let mut d = lock(&self.old_trees.delayed);
        match d.get(&key) {
            Some((due, ..)) if *due <= at => {}
            _ => {
                d.insert(key, (at, path, ino));
            }
        }
    }

    /// The delayed keys whose time has come, moved to the queue.
    fn promote_due(&self) {
        let now = Instant::now();
        let due: Vec<(OldTreeKey, (Instant, PathBuf, u64))> = {
            let mut d = lock(&self.old_trees.delayed);
            let keys: Vec<OldTreeKey> = d.iter().filter(|(_, (at, ..))| *at <= now).map(|(k, _)| k.clone()).collect();
            keys.into_iter().filter_map(|k| d.remove(&k).map(|v| (k, v))).collect()
        };
        if due.is_empty() {
            return;
        }
        let mut q = lock(&self.old_trees.pending);
        for (k, (_, p, ino)) in due {
            q.entry(k).or_insert((p, ino));
        }
    }

    /// rm-followups §1.4 G4 item 4: queue the next pass of every done
    /// `Auto` move of `session` whose old tree is still there and may yet
    /// be retired (no late-write notice, not retained): at its last clean
    /// pass + Q, else now + Q. Phase F calls it after `forget_old_trees`.
    pub(crate) fn queue_auto_trees(&self, session: &SessionId) {
        let Ok(all) = self.store.move_intents(session) else { return };
        let Ok(rows) = self.store.move_activities(session) else { return };
        for r in rows {
            if r.late_writes.is_some() || r.baseline.is_none() {
                continue;
            }
            let Some(i) = all.iter().find(|i| i.repo == r.repo && i.n == r.n && i.state == MoveState::Done) else {
                continue;
            };
            if i.old_tree.is_some() {
                continue;
            }
            let path = old_path(i);
            let Some(ino) = ino_of(&path) else { continue };
            let q = self.quiet_of(&i.repo);
            self.queue_old_tree_at(key_of(i), path, ino, next_pass_at(&r.passes, q));
        }
    }

    fn quiet_of(&self, repo: &RepoName) -> Duration {
        let secs = self.repo_cfg(repo).map(|c| c.rebase.quiet_secs).unwrap_or_else(|_| strata_types::RebaseConfig::default_quiet_secs());
        Duration::from_secs(secs)
    }

    /// The tree at `path` of done move `i` against its move's activity
    /// baseline. Records the late-write notice the first time it finds a
    /// change (or no readable baseline); a store read error fails closed
    /// (`Late`, nothing recorded).
    pub(crate) fn auto_check(&self, i: &MoveIntent, path: &Path) -> AutoCheck {
        let row = match self.store.move_activity(&i.session, &i.repo, i.n) {
            Ok(None) => return AutoCheck::Explicit,
            Ok(Some(r)) => r,
            Err(_) => return AutoCheck::Late,
        };
        if row.late_writes.is_some() {
            return AutoCheck::Late;
        }
        let Some(b) = &row.baseline else {
            self.late_notice(i, &[], "baseline_missing");
            return AutoCheck::Late;
        };
        match crate::activity::changed_since(path, b) {
            Ok(v) if v.is_empty() => AutoCheck::Clean(row.passes),
            Ok(v) => {
                self.late_notice(i, &v, "late_writes");
                AutoCheck::Late
            }
            Err(e) => {
                self.late_notice(i, &[format!("(walk error: {})", e.message)], "walk_error");
                AutoCheck::Late
            }
        }
    }

    /// rm-followups §1.4 G4 item 5, crash-safe: before either open-files
    /// scan of an `Auto` tree with recorded clean passes, the passes are
    /// cleared in the store, so a hold seen by a scan is already persisted
    /// as a reset and a restart at any point loses passes, never keeps a
    /// stale one. A scan that sees no hold puts them back
    /// ([`Self::restore_passes`]). False when the write failed: the tree
    /// is then not scanned.
    fn suspend_passes(&self, i: &MoveIntent) -> bool {
        if self.seams.hit("old_tree.suspend_passes", Some(&i.repo)).is_err() {
            return false;
        }
        self.store.set_move_passes(&i.session, &i.repo, i.n, &[]).is_ok()
    }

    /// Puts suspended clean passes back (no hold was seen). A failed write
    /// only loses them (fail closed: two fresh passes are needed).
    fn restore_passes(&self, i: &MoveIntent, passes: &[i64]) {
        if !passes.is_empty() {
            let _ = self.store.set_move_passes(&i.session, &i.repo, i.n, passes);
        }
    }

    /// Writes the late-write notice of move `i` (the first one stands).
    pub(crate) fn late_notice(&self, i: &MoveIntent, paths: &[String], reason: &str) {
        let moved_at = i.ended.unwrap_or(i.started);
        let at = strata_types::serde_rfc3339::format(&moved_at).unwrap_or_default();
        let shown: Vec<&String> = paths.iter().take(20).collect();
        let notice = serde_json::json!({ "moved_at": at, "paths": shown, "count": paths.len(), "reason": reason });
        let _ = self.store.set_move_late_writes(&i.session, &i.repo, i.n, &notice);
    }

    /// The current verdict for `key` (tests, diagnostics).
    pub fn old_tree_verdict(&self, key: &OldTreeKey) -> Option<OldTreeVerdict> {
        lock(&self.old_trees.verdicts).get(key).cloned()
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

    /// The next queued old tree, waiting up to `wait` for one.
    pub fn next_old_tree(&self, wait: Duration) -> Option<(OldTreeKey, PathBuf, u64)> {
        self.promote_due();
        let mut q = lock(&self.old_trees.pending);
        if q.is_empty() {
            q = self
                .old_trees
                .wake
                .wait_timeout(q, wait)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
        q.pop_first().map(|(k, (p, ino))| (k, p, ino))
    }

    /// Put `key` back at the end of the queue (its session was busy).
    pub fn requeue_old_tree(&self, key: OldTreeKey, path: PathBuf, ino: u64) {
        lock(&self.old_trees.pending)
            .entry(key)
            .or_insert((path, ino));
    }

    /// The background check of one queued old tree. The caller holds the
    /// session's queue place and the repo's write lock. Everything is read
    /// again under them: the session must be Active and the intent still
    /// Done at the same path and inode, else the item is dropped
    /// (`Skipped`). Walks give up once `stop` is set (`Skipped("stopped")`,
    /// no verdict). A retained tree gets a verdict but is never retired.
    pub fn check_old_tree(
        &self,
        key: &OldTreeKey,
        path: &Path,
        ino: u64,
        stop: &Arc<AtomicBool>,
    ) -> Result<Outcome> {
        let Some(rec) = self.store.session(&key.session)? else {
            return Ok(Outcome::Skipped("no_session"));
        };
        if rec.lifecycle != Lifecycle::Active || !rec.manifest.repos.contains_key(&key.repo) {
            return Ok(Outcome::Skipped("not_active"));
        }
        let all = self.store.move_intents(&key.session)?;
        let Some(i) = all.iter().find(|i| i.repo == key.repo && i.n == key.n) else {
            return Ok(Outcome::Skipped("no_intent"));
        };
        if i.state != MoveState::Done || old_path(i) != path || ino_of(path) != Some(ino) {
            return Ok(Outcome::Skipped("intent_changed"));
        }
        let cfg = self.repo_cfg(&key.repo)?;
        let out =
            strata_apfs::with_walk_stop(stop.clone(), || self.collect_old_tree(&all, i, cfg, true));
        if stop.load(Ordering::SeqCst) {
            return Ok(Outcome::Skipped("stopped"));
        }
        // rm-followups §1.4 G4 item 4: a kept `Auto` tree that may yet be
        // retired is checked again Q later; so is one not judged because a
        // store write or the open-files scan failed (impl-p2 r3 note).
        if matches!(
            out,
            Outcome::Kept { .. } | Outcome::Skipped("passes_write_failed" | "open_files_failed")
        ) {
            self.queue_auto_trees(&key.session);
        }
        match &out {
            Outcome::Kept {
                holders,
                changed: Some(changed),
            } => {
                let v = OldTreeVerdict {
                    path: path.to_path_buf(),
                    ino,
                    changed: *changed,
                    holders: holders.clone(),
                    checked_at: SystemTime::now(),
                };
                lock(&self.old_trees.verdicts).insert(key.clone(), v);
            }
            _ => {
                lock(&self.old_trees.verdicts).remove(key);
            }
        }
        Ok(out)
    }

    /// Move step 20 and the background thread (design A1): retire the old
    /// tree of done move `i` (`all` = the session's intents) only on a
    /// fresh judgement: no open files, unchanged against its BeforeRollout
    /// checkpoint, and again no open files at the same inode right before
    /// the retire. A retained tree (`old_tree` set) is judged, never
    /// retired. `judge_held`: also compare a held tree (for its verdict).
    /// An unreadable tree or `open_files` failure is never retired.
    pub(crate) fn collect_old_tree(
        &self,
        all: &[MoveIntent],
        i: &MoveIntent,
        cfg: &RepoConfig,
        judge_held: bool,
    ) -> Outcome {
        let path = old_path(i);
        let Some(ino) = ino_of(&path) else {
            return Outcome::Skipped("gone");
        };
        // rm-followups §1.4 G4 items 4–5: an `Auto` move's tree is held to
        // its persisted baseline; its clean passes are suspended (cleared
        // in the store) before the scans and put back only when no scan
        // saw a hold.
        let auto = self.auto_check(i, &path);
        let suspended: Vec<i64> = match &auto {
            AutoCheck::Clean(p) if !p.is_empty() => {
                if !self.suspend_passes(i) {
                    return Outcome::Skipped("passes_write_failed");
                }
                p.clone()
            }
            _ => Vec::new(),
        };
        let holders = match strata_deps::open_files(&path) {
            Ok(h) => h,
            Err(_) => {
                self.restore_passes(i, &suspended);
                return Outcome::Skipped("open_files_failed");
            }
        };
        // A held tree keeps its passes cleared; every other exit below
        // that does not retire puts them back.
        if !holders.is_empty() && !judge_held {
            return Outcome::Kept {
                holders,
                changed: None,
            };
        }
        // A tree that cannot be compared reads as changed.
        let changed = matches!(auto, AutoCheck::Late) || !self.old_tree_matches(i, cfg).unwrap_or(false);
        if changed || !holders.is_empty() || i.old_tree.is_some() {
            if holders.is_empty() {
                self.restore_passes(i, &suspended);
            }
            return Outcome::Kept {
                holders,
                changed: Some(changed),
            };
        }
        // Two consecutive clean passes at least Q apart before an `Auto`
        // tree is retired; this one is recorded (a restart keeps it).
        if let AutoCheck::Clean(passes) = &auto {
            let now = crate::activity::now_ns();
            let q = self.quiet_of(&i.repo).as_nanos() as i64;
            match passes.last() {
                Some(last) if now.saturating_sub(*last) >= q => {}
                Some(_) => {
                    self.restore_passes(i, &suspended);
                    return Outcome::Kept {
                        holders: Vec::new(),
                        changed: Some(false),
                    };
                }
                None => {
                    let _ = self.store.set_move_passes(&i.session, &i.repo, i.n, &[now]);
                    return Outcome::Kept {
                        holders: Vec::new(),
                        changed: Some(false),
                    };
                }
            }
        }
        if self.seams.hit("old_tree.final_scan", Some(&i.repo)).is_err() {
            self.restore_passes(i, &suspended);
            return Outcome::Skipped("seam");
        }
        match strata_deps::open_files(&path) {
            Ok(h) if h.is_empty() && ino_of(&path) == Some(ino) => {}
            Ok(h) => {
                // A hold seen by the final scan leaves the passes cleared
                // (already persisted); an inode change alone puts them back.
                if h.is_empty() {
                    self.restore_passes(i, &suspended);
                }
                return Outcome::Kept {
                    holders: h,
                    changed: Some(false),
                };
            }
            Err(_) => {
                self.restore_passes(i, &suspended);
                return Outcome::Skipped("open_files_failed");
            }
        }
        let admin = self
            .shadow_dir(&i.session, &i.repo)
            .join("worktrees")
            .join(prior_generation(all, i).to_string());
        if self.store.retire_dir(&i.staging).is_err() {
            self.restore_passes(i, &suspended);
            return Outcome::Kept {
                holders: Vec::new(),
                changed: Some(false),
            };
        }
        let _ = self.retire_present(&admin);
        Outcome::Retired
    }

    /// Design A1 half-retired sweep, under the session's queue place and
    /// `repo`'s write lock: for each done move of `repo` whose old tree is
    /// gone, retire the admin dir of the generation it replaced if it is
    /// still there and is not the workspace's current one (by the row's
    /// generation and by the workspace's `.git`; an unreadable `.git`
    /// refuses). Returns the retired paths.
    pub fn sweep_half_retired(&self, session: &SessionId, repo: &RepoName) -> Result<Vec<PathBuf>> {
        let Some(rec) = self.store.session(session)? else {
            return Ok(Vec::new());
        };
        if rec.lifecycle != Lifecycle::Active {
            return Ok(Vec::new());
        }
        let (Some(ws), Some(row)) = (rec.manifest.repos.get(repo), rec.workspaces.get(repo)) else {
            return Ok(Vec::new());
        };
        let Some(current) = gitdir_of(&ws.path) else {
            return Ok(Vec::new());
        };
        let shadow = self.shadow_dir(session, repo);
        let all = self.store.move_intents(session)?;
        let mut out = Vec::new();
        for i in all
            .iter()
            .filter(|i| &i.repo == repo && i.state == MoveState::Done)
        {
            if ino_of(&old_path(i)).is_some() || ino_of(&i.staging).is_some() {
                continue;
            }
            let g = prior_generation(&all, i);
            if g == row.generation {
                continue;
            }
            let admin = shadow.join("worktrees").join(g.to_string());
            if ino_of(&admin).is_none() || same_dir(&admin, &current) {
                continue;
            }
            if self.retire_present(&admin).is_ok() {
                out.push(admin);
            }
        }
        Ok(out)
    }
}

/// When the next pass of an `Auto` tree is due: Q after its last clean
/// pass, else Q from now.
fn next_pass_at(passes: &[i64], q: Duration) -> Instant {
    let now = Instant::now();
    match passes.last() {
        Some(last) => {
            let age = crate::activity::now_ns().saturating_sub(*last).max(0) as u128;
            let left = q.as_nanos().saturating_sub(age);
            now + Duration::from_nanos(left.min(u64::MAX as u128) as u64)
        }
        None => now + q,
    }
}

/// The admin dir a worktree's `.git` file points at, `None` when it cannot
/// be read.
fn gitdir_of(ws: &Path) -> Option<PathBuf> {
    let body = std::fs::read_to_string(ws.join(".git")).ok()?;
    let p = body.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
    let p = Path::new(p);
    Some(if p.is_absolute() {
        p.to_path_buf()
    } else {
        ws.join(p)
    })
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        // Cannot tell: treat as the current one (never touched).
        _ => true,
    }
}
