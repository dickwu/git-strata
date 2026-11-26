//! status-perf A1 and new-lock §3: judging a done move's old tree off the
//! status path.
//!
//! The daemon's background worker claims queued old trees and judges each
//! in phases that walk without any lock ([`claim`]); move step 20 judges
//! inline under sync's locks ([`Sessions::collect_old_tree`]). Status in
//! [`OldTreeMode::Deferred`] only reads the verdict table ([`queue`]) and
//! queues the intents it has no fresh verdict for. The table is a hint:
//! nothing is ever retired on a cached verdict, only on a fresh judgement
//! whose retirement runs under the session's queue place and the repo's
//! write lock.

mod claim;
mod queue;

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use strata_deps::OpenFile;
use strata_store::{Lifecycle, MoveActivity, MoveIntent, MoveState};
use strata_types::{RepoConfig, RepoName, SessionId};

pub use claim::OldTreeLocks;
pub(crate) use queue::OldTrees;
pub use queue::{OldTreeClaim, OldTreeGuard};

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

/// What a check of an old tree did.
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

/// rm-followups §1.4 G4 items 5–6: an old tree against its move's
/// activity baseline.
#[derive(Clone, Debug)]
pub(crate) enum AutoCheck {
    /// No `move_activity` row: an `Explicit` move (today's rules).
    Explicit,
    /// Interrupted: no judgement and no permanent late-write notice.
    Skipped,
    /// Changed since the baseline, a late-write notice already there, or
    /// no readable baseline: never retired.
    Late,
    /// Unchanged against the baseline; the row's clean passes.
    Clean(Vec<i64>),
}

/// The activity probe of an old tree against its row, without any write
/// (new-lock §3.1: only phase 2, 2N and foreground judges write notices).
#[derive(Clone, Debug)]
pub(crate) enum Probe {
    /// No row: an `Explicit` move.
    Explicit,
    /// A late-write notice is already there.
    Noticed,
    /// The row's baseline cannot be read.
    BaselineMissing,
    /// Changed since the baseline: the paths.
    LateWrites(Vec<String>),
    /// The walk failed (not stopped): the message.
    WalkError(String),
    /// The walk was stopped.
    Stopped,
    /// Unchanged against the baseline.
    Clean,
}

impl Probe {
    /// Never retired.
    pub(crate) fn late(&self) -> bool {
        matches!(
            self,
            Probe::Noticed | Probe::BaselineMissing | Probe::LateWrites(_) | Probe::WalkError(_)
        )
    }
}

/// The inode of `p` (not followed), `None` when it cannot be read.
pub(crate) fn ino_of(p: &Path) -> Option<u64> {
    std::fs::symlink_metadata(p).ok().map(|m| m.ino())
}

/// The tree a done move left: the retained path, else its staging.
pub(crate) fn old_path(i: &MoveIntent) -> PathBuf {
    i.old_tree.clone().unwrap_or_else(|| i.staging.clone())
}

pub(crate) fn key_of(i: &MoveIntent) -> OldTreeKey {
    OldTreeKey {
        session: i.session.clone(),
        repo: i.repo.clone(),
        n: i.n,
    }
}

impl<'a> Sessions<'a> {
    /// rm-followups §1.4 G4 item 4: queue the next pass of every done
    /// `Auto` move of `session` whose old tree is still there and may yet
    /// be retired (no late-write notice, not retained): at its last clean
    /// pass + Q, else now + Q. Phase F calls it after `forget_old_trees`.
    pub(crate) fn queue_auto_trees(&self, session: &SessionId) {
        let Ok(all) = self.store.move_intents(session) else {
            return;
        };
        let Ok(rows) = self.store.move_activities(session) else {
            return;
        };
        for r in rows {
            if r.late_writes.is_some() || r.baseline.is_none() {
                continue;
            }
            let Some(i) = all
                .iter()
                .find(|i| i.repo == r.repo && i.n == r.n && i.state == MoveState::Done)
            else {
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
        let secs = self
            .repo_cfg(repo)
            .map(|c| c.rebase.quiet_secs)
            .unwrap_or_else(|_| strata_types::RebaseConfig::default_quiet_secs());
        Duration::from_secs(secs)
    }

    /// The tree at `path` of done move `i` against `row`, its activity
    /// baseline. Writes nothing.
    pub(crate) fn auto_probe(
        &self,
        i: &MoveIntent,
        path: &Path,
        row: Option<&MoveActivity>,
    ) -> Probe {
        let Some(row) = row else {
            return Probe::Explicit;
        };
        if row.late_writes.is_some() {
            return Probe::Noticed;
        }
        let Some(b) = &row.baseline else {
            return Probe::BaselineMissing;
        };
        match crate::activity::changed_since_with(path, b, || {
            self.seams.hit("old_tree.activity_directory", Some(&i.repo))
        }) {
            Ok(v) if v.is_empty() => Probe::Clean,
            Ok(v) => Probe::LateWrites(v),
            Err(e) if crate::activity::stopped(&e) => Probe::Stopped,
            Err(e) => Probe::WalkError(e.message),
        }
    }

    /// A foreground judgement (rm, status Inline, a move's step 20) of
    /// done move `i`'s tree at `path` against its activity baseline: bumps
    /// the intent's token (new-lock §3.3) and records the late-write notice
    /// the first time it finds a change (or no readable baseline); a store
    /// read error fails closed (`Late`, nothing recorded).
    pub(crate) fn auto_check(&self, i: &MoveIntent, path: &Path) -> AutoCheck {
        self.bump_old_tree_token(&key_of(i));
        let row = match self.store.move_activity(&i.session, &i.repo, i.n) {
            Ok(r) => r,
            Err(_) => return AutoCheck::Late,
        };
        match self.auto_probe(i, path, row.as_ref()) {
            Probe::Explicit => AutoCheck::Explicit,
            Probe::Noticed => AutoCheck::Late,
            Probe::BaselineMissing => {
                self.write_late_notice(i, &[], "baseline_missing");
                AutoCheck::Late
            }
            Probe::LateWrites(v) => {
                self.write_late_notice(i, &v, "late_writes");
                AutoCheck::Late
            }
            Probe::WalkError(m) => {
                self.write_late_notice(i, &[format!("(walk error: {m})")], "walk_error");
                AutoCheck::Late
            }
            Probe::Stopped => AutoCheck::Skipped,
            Probe::Clean => AutoCheck::Clean(row.map(|r| r.passes).unwrap_or_default()),
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
        if self
            .seams
            .hit("old_tree.suspend_passes", Some(&i.repo))
            .is_err()
        {
            return false;
        }
        self.store
            .set_move_passes(&i.session, &i.repo, i.n, &[])
            .is_ok()
    }

    /// Puts suspended clean passes back (no hold was seen). A failed write
    /// only loses them (fail closed: two fresh passes are needed).
    fn restore_passes(&self, i: &MoveIntent, passes: &[i64]) {
        if !passes.is_empty() {
            let _ = self.store.set_move_passes(&i.session, &i.repo, i.n, passes);
        }
    }

    /// Writes the late-write notice of move `i` (the first one stands) for
    /// a writer other than the worker's own claim, so the intent's token
    /// changes (new-lock §3.3).
    pub(crate) fn late_notice(&self, i: &MoveIntent, paths: &[String], reason: &str) {
        self.bump_old_tree_token(&key_of(i));
        self.write_late_notice(i, paths, reason);
    }

    /// The notice write itself (the first one stands).
    fn write_late_notice(&self, i: &MoveIntent, paths: &[String], reason: &str) {
        let notice = self.notice_json(i, paths, reason);
        let _ = self
            .store
            .set_move_late_writes(&i.session, &i.repo, i.n, &notice);
    }

    /// `{moved_at, paths (the first 20), count, reason}`.
    fn notice_json(&self, i: &MoveIntent, paths: &[String], reason: &str) -> serde_json::Value {
        let moved_at = i.ended.unwrap_or(i.started);
        let at = strata_types::serde_rfc3339::format(&moved_at).unwrap_or_default();
        let shown: Vec<&String> = paths.iter().take(20).collect();
        serde_json::json!({ "moved_at": at, "paths": shown, "count": paths.len(), "reason": reason })
    }

    /// Move step 20 (design A1, new-lock §15.5): judge the old tree of done
    /// move `i` (`all` = the session's intents) inline under the caller's
    /// queue place and repo lock, and retire it only on a fresh judgement:
    /// no open files, unchanged against its BeforeRollout checkpoint, and
    /// again no open files at the same inode right before the retire. A
    /// held tree is kept without a comparison; a retained tree (`old_tree`
    /// set) is judged, never retired. An unreadable tree or `open_files`
    /// failure is never retired. A foreground judgement: it bumps the
    /// intent's token.
    pub(crate) fn collect_old_tree(
        &self,
        all: &[MoveIntent],
        i: &MoveIntent,
        cfg: &RepoConfig,
    ) -> Outcome {
        self.bump_old_tree_token(&key_of(i));
        let path = old_path(i);
        let Some(ino) = ino_of(&path) else {
            return Outcome::Skipped("gone");
        };
        if strata_apfs::walk_stopped() {
            return Outcome::Skipped("yielded");
        }
        // Crash-safe reset before the first scan. A later interruption only
        // restores these passes if no holder was observed in this pass.
        let suspended = match self
            .seams
            .hit("old_tree.read_passes", Some(&i.repo))
            .and_then(|()| self.store.move_activity(&i.session, &i.repo, i.n))
        {
            Ok(row) => row.map(|r| r.passes).unwrap_or_default(),
            Err(_) => return Outcome::Skipped("passes_read_failed"),
        };
        if !suspended.is_empty() && !self.suspend_passes(i) {
            return Outcome::Skipped("passes_write_failed");
        }
        let holders = match strata_deps::open_files(&path) {
            Ok(h) => h,
            Err(_) => {
                self.restore_passes(i, &suspended);
                return Outcome::Skipped("open_files_failed");
            }
        };
        if self
            .seams
            .hit("old_tree.after_scan", Some(&i.repo))
            .is_err()
        {
            if holders.is_empty() {
                self.restore_passes(i, &suspended);
            }
            return Outcome::Skipped("seam");
        }
        if strata_apfs::walk_stopped() {
            if holders.is_empty() {
                self.restore_passes(i, &suspended);
            }
            return Outcome::Skipped("yielded");
        }
        if !holders.is_empty() {
            return Outcome::Kept {
                holders,
                changed: None,
            };
        }
        let auto = match self.auto_check(i, &path) {
            AutoCheck::Clean(_) => AutoCheck::Clean(suspended.clone()),
            other => other,
        };
        if matches!(auto, AutoCheck::Skipped) || strata_apfs::walk_stopped() {
            self.restore_passes(i, &suspended);
            return Outcome::Skipped("yielded");
        }
        let changed =
            matches!(auto, AutoCheck::Late) || !self.old_tree_matches(i, cfg).unwrap_or(false);
        if strata_apfs::walk_stopped() {
            self.restore_passes(i, &suspended);
            return Outcome::Skipped("yielded");
        }
        if changed || i.old_tree.is_some() {
            // A late notice clears its clean history permanently. Do not
            // resurrect passes that were suspended before that judgement.
            if !matches!(auto, AutoCheck::Late) {
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
        if self
            .seams
            .hit("old_tree.final_scan", Some(&i.repo))
            .is_err()
        {
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
        if strata_apfs::walk_stopped() {
            self.restore_passes(i, &suspended);
            return Outcome::Skipped("yielded");
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
            if strata_apfs::walk_stopped() {
                break;
            }
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
