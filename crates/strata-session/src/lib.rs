//! strata-session (session.md): sessions of sibling workspaces cloned from
//! pinned base generations, each with its own shadow repository; moves of a
//! workspace to a new base, removal and recovery after a restart.
//!
//! Layer 3 (types.md §8): depends on types, apfs, gitio, store, overlay and
//! deps only.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use strata_deps::{InstallEnv, RepoWriteLock};
use strata_gitio::Git;
use strata_overlay::{Base, Checkpointed, WorkspaceRef};
use strata_store::Store;
use strata_types::{CheckpointReason, Code, Config, RepoConfig, RepoName, SessionId, StrataError};

mod activity;
mod base;
mod carry;
pub mod catalog;
pub mod cwd;
mod dispatch;
mod failed_move;
#[cfg(test)]
mod lock_tests;
mod move_workspace;
mod new;
mod no_wait;
pub mod observe;
mod old_tree;
mod orphans;
mod outstanding;
mod recover;
mod remove;
mod resolve;
mod rm_note;
mod scene;
mod seams;
mod seed;
mod status;
mod summary;
mod sys;
mod ticket;

#[cfg(feature = "test-seams")]
pub use activity::walk_activity_during_scan;
pub use activity::{
    Activity, Idle, activity_scope, baseline_of, busy_reason, changed_since, holders_json,
    idle_activity, idle_activity_scoped, walk_activity, walk_activity_scoped,
};
pub use failed_move::{MoveResolution, ResolutionReport};
pub use move_workspace::{AUTO_DEFERRED, MoveReport, auto_deferral};
pub use new::NewReport;
pub use new::NewWarning;
pub use new::add::{ADD_STAGING_PREFIX, AddReport};
pub use new::{Layout, validate_base_rev};
pub use old_tree::{
    OLD_TREE_TTL, OldTreeClaim, OldTreeGuard, OldTreeKey, OldTreeLocks, OldTreeMode,
    OldTreeVerdict, Outcome as OldTreeOutcome,
};
pub use orphans::{OrphanRetired, Sweep};
pub use outstanding::{Ancestry, Outstanding, PendingFold, outstanding};
pub use recover::{IntentRef, Leftover, RecoverReport};
pub use remove::{Regenerable, RemoveReport};
pub use resolve::{resolve_in, select_in};
pub use rm_note::{
    KeptTree, LastRm, RebaseNote, RmInterrupted, RmNotes, leftover_line, local_hhmm, local_hhmmss,
    rebase_cause, rebase_stuck, removed_leftover_of, removed_leftovers, rm_notes,
    with_leftover_detail,
};
pub use scene::{Scene, SceneOp};
#[cfg(feature = "test-seams")]
pub use seams::SessionSeam;
pub use status::{
    DRIFT_NOTE, DriftStatus, MirrorDriftItem, SessionState, SessionStatus, StatusWarning,
    WorkspaceHealth, WorkspaceStatus,
};
pub use summary::{BehindStatus, SessionSummary, SummarySelect, WorkspaceSummary, summary};

/// The fetch window the daemon gives `new` and `add` (D53).
pub const NEW_FETCH_WINDOW: Duration = Duration::from_secs(30);
pub use seed::{
    SEED_MARK, SEED_STAGING_PREFIX, SeedRecord, SeedRefresh, SeedSource, seed_files, seed_plan,
};

pub type Result<T> = std::result::Result<T, StrataError>;

/// Prefix of a move's staging directory under the session root (§4.1).
pub const MOVE_STAGING_PREFIX: &str = ".strata-tmp-move-";
/// `bases/<repo>/.staging/`: where base generations are built (§5.2).
pub const BASE_STAGING_DIR: &str = ".staging";
/// How many dirty paths `remove`'s `Dirty` detail lists per repo (§5.6).
/// No longer applied: rm-followups R2 keeps the full lists in `detail`.
pub const DIRTY_PATHS_CAP: usize = 100;

/// The daemon's `BeforeRollout` / `BeforeRemove` checkpoint (§3): records
/// with the workspace's Tracker when it has one. Never enters or waits on
/// the session queue, never waits behind a repo lock.
pub trait BeforeCheckpoint: Sync {
    fn checkpoint(
        &self,
        ws: &WorkspaceRef<'_>,
        base: &Base,
        reason: CheckpointReason,
    ) -> Result<Checkpointed>;
}

/// The session handle the daemon holds (one per process).
pub struct Sessions<'a> {
    pub(crate) config: &'a Config,
    catalog: Option<&'a dyn catalog::CatalogSource>,
    pub(crate) gits: &'a BTreeMap<RepoName, Git>,
    pub(crate) store: &'a Store,
    pub(crate) locks: no_wait::SessionLock<'a>,
    pub(crate) env: &'a InstallEnv,
    pub(crate) before: &'a dyn BeforeCheckpoint,
    /// Base staging directories being built by this process (§5.2 step 3).
    pub(crate) live_staging: Mutex<BTreeSet<PathBuf>>,
    /// Moves in progress in this process (§5.5 step 2.9).
    pub(crate) live_moves: Mutex<BTreeSet<(SessionId, RepoName)>>,
    pub(crate) seams: seams::Seams,
    /// When `new`/`add` last fetched each repo successfully (D53).
    pub(crate) fetched: Mutex<BTreeMap<RepoName, Instant>>,
    /// How long such a fetch counts as fresh; zero (the default) always
    /// fetches. The daemon sets `NEW_FETCH_WINDOW`.
    pub(crate) fetch_window: Mutex<Duration>,
    /// status-perf A4: the slots shared by every status read of this
    /// process, `[status] parallel` of them (read when opened).
    pub(crate) status_slots: status::Slots,
    /// status-perf A1: the old-tree mode, verdict table and queue.
    pub(crate) old_trees: old_tree::OldTrees,
    /// Compatibility overrides for static handles. Daemon handles ignore
    /// this map and always read their admission catalog.
    pub(crate) regenerable: Mutex<BTreeMap<RepoName, Vec<String>>>,
    /// rm-followups R1: `new`'s row-and-root enrollment and the orphan
    /// sweep's recheck-and-retire exclude each other. A leaf lock: nothing
    /// else is taken while it is held.
    pub(crate) enroll: Mutex<()>,
}

impl<'a> Sessions<'a> {
    /// `gits` must have exactly the keys of `config.repos`. No I/O.
    pub fn open(
        config: &'a Config,
        gits: &'a BTreeMap<RepoName, Git>,
        store: &'a Store,
        locks: &'a dyn RepoWriteLock,
        env: &'a InstallEnv,
        before: &'a dyn BeforeCheckpoint,
    ) -> Result<Sessions<'a>> {
        let want: Vec<&RepoName> = config.repos.keys().collect();
        let have: Vec<&RepoName> = gits.keys().collect();
        if want != have {
            return Err(StrataError::new(
                Code::InvalidInput,
                "Sessions::open: the Git handles must cover exactly the configured repos",
            )
            .with_detail(serde_json::json!({
                "field": "gits",
                "configured": want.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
                "given": have.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
            })));
        }
        Ok(Sessions {
            catalog: None,
            config,
            gits,
            store,
            locks: no_wait::SessionLock(locks),
            env,
            before,
            live_staging: Mutex::new(BTreeSet::new()),
            live_moves: Mutex::new(BTreeSet::new()),
            seams: seams::Seams::default(),
            fetched: Mutex::new(BTreeMap::new()),
            fetch_window: Mutex::new(Duration::ZERO),
            status_slots: status::Slots::new(config.status.parallel as usize),
            old_trees: old_tree::OldTrees::default(),
            regenerable: Mutex::new(BTreeMap::new()),
            enroll: Mutex::new(()),
        })
    }

    /// Use one admission snapshot for every dynamic config read.
    pub fn with_catalog(mut self, catalog: &'a dyn catalog::CatalogSource) -> Self {
        self.catalog = Some(catalog);
        self
    }

    pub fn configuration(&self) -> Result<&'a Config> {
        match self.catalog {
            Some(c) => c.config(),
            None => Ok(self.config),
        }
    }

    fn dev_ports_policy(&self) -> Result<Option<&'a strata_store::DevPortsPolicy>> {
        match self.catalog {
            Some(c) => c.dev_ports(),
            None => Ok(None),
        }
    }

    /// D53: a repo that `new` or `add` fetched successfully less than `w`
    /// ago is not fetched again by them (the main tip then comes from the
    /// repo's release-point snapshot, or from the bounded locked read when
    /// that snapshot is not valid). Zero turns the skip off.
    pub fn set_fetch_window(&self, w: Duration) {
        *self.fetch_window.lock().unwrap_or_else(|p| p.into_inner()) = w;
    }

    /// Override a static handle's regenerable rules. Catalog-backed daemon
    /// handles use their immutable admission snapshot instead.
    pub fn set_regenerable(&self, repo: &RepoName, patterns: Vec<String>) {
        self.regenerable
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(repo.clone(), patterns);
    }

    /// The effective `rm.regenerable` of `repo`.
    pub fn regenerable(&self, repo: &RepoName) -> Result<Vec<String>> {
        if self.catalog.is_some() {
            return Ok(self
                .configuration()?
                .repos
                .get(repo)
                .map(|rc| rc.rm.regenerable.clone())
                .unwrap_or_default());
        }
        if let Some(v) = self
            .regenerable
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(repo)
        {
            return Ok(v.clone());
        }
        Ok(self
            .config
            .repos
            .get(repo)
            .map(|rc| rc.rm.regenerable.clone())
            .unwrap_or_default())
    }

    pub(crate) fn fetch_is_fresh(&self, repo: &RepoName) -> bool {
        let w = *self.fetch_window.lock().unwrap_or_else(|p| p.into_inner());
        if w.is_zero() {
            return false;
        }
        self.fetched
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(repo)
            .is_some_and(|t| t.elapsed() < w)
    }

    /// `repo`'s configuration and git handle, for callers that work on a
    /// repo outside any session (publish's check siblings, publish.md D21).
    pub fn repo_handles(&self, repo: &RepoName) -> Result<(&'a RepoConfig, &'a Git)> {
        Ok((self.repo_cfg(repo)?, self.git(repo)?))
    }

    pub(crate) fn repo_cfg(&self, repo: &RepoName) -> Result<&'a RepoConfig> {
        self.configuration()?
            .repos
            .get(repo)
            .ok_or_else(|| no_such_repo(repo))
    }

    pub(crate) fn git(&self, repo: &RepoName) -> Result<&'a Git> {
        let gits = match self.catalog {
            Some(c) => c.gits()?,
            None => self.gits,
        };
        gits.get(repo).ok_or_else(|| no_such_repo(repo))
    }

    /// One critical section under `repo`'s write lock with the daemon's
    /// bounded wait: `Busy` passes through (a named refusal before the first
    /// visible write).
    pub(crate) fn with_lock<T>(
        &self,
        repo: &RepoName,
        mut f: impl FnMut() -> Result<T>,
    ) -> Result<T> {
        let mut out: Option<T> = None;
        let r = self.locks.with_write(repo, &mut || {
            out = Some(f()?);
            Ok(())
        })?;
        r?;
        Ok(out.expect("with_write ran the closure"))
    }

    /// new-lock §2.4a: where a repo-lock take was removed, shutdown still
    /// refuses exactly as the closed lock did (`ReadOnly` `shutting_down`).
    pub(crate) fn refuse_if_shut_down(&self) -> Result<()> {
        if self.locks.is_shut_down() {
            return Err(
                StrataError::new(Code::ReadOnly, "the daemon is shutting down")
                    .with_detail(serde_json::json!({ "reason": "shutting_down" })),
            );
        }
        Ok(())
    }

    /// new-lock §2.7: delete session `id`'s pin in `repo` without the repo
    /// lock (gitio.md §5's caller rule, amended). The pin is read once and
    /// that value is the expected one; an absent pin is done. Only proven
    /// lock-file contention is retried, for at most [`PIN_LOCK_RETRY`] and
    /// with the same expected value; a moved pin (`HeadMoved`) and any other
    /// error are returned for the caller's failure path. Refused at shutdown
    /// like the closed lock; no policy fence applies (cleanup).
    pub(crate) fn unpin(&self, repo: &RepoName, id: &SessionId) -> Result<()> {
        self.refuse_if_shut_down()?;
        let git = self.git(repo)?;
        let pin = new::pin_ref(id);
        let Some(expected) = git.read_ref(&pin)? else {
            return Ok(());
        };
        let deadline = Instant::now() + PIN_LOCK_RETRY;
        loop {
            match git.delete_ref(&pin, &expected) {
                Err(e) if ref_lock_busy(&e) && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                r => return r,
            }
        }
    }
}

/// How long [`Sessions::unpin`] retries git's ref lock-file contention.
pub(crate) const PIN_LOCK_RETRY: Duration = Duration::from_secs(10);

/// git's lock-file contention (`Unable to create '<path>.lock': File
/// exists`, also for `packed-refs.lock`), not the generic `cannot lock ref`
/// git prints for a compare-and-swap mismatch too (gitio `cas_failure`).
pub fn ref_lock_busy(e: &StrataError) -> bool {
    let stderr = e
        .detail
        .as_ref()
        .and_then(|d| d.get("stderr"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    e.code == Code::Unknown
        && stderr
            .lines()
            .any(|l| l.contains("Unable to create '") && l.contains(".lock': File exists"))
}

pub(crate) fn no_such_repo(repo: &RepoName) -> StrataError {
    StrataError::new(
        Code::NoSuchRepo,
        format!("no repo named {repo} in the configuration"),
    )
    .with_detail(serde_json::json!({ "repo": repo.as_str() }))
}
