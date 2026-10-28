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

mod base;
mod carry;
mod dispatch;
mod failed_move;
mod move_workspace;
mod new;
mod recover;
mod remove;
mod resolve;
mod scene;
mod seams;
mod seed;
mod status;
mod sys;
mod ticket;

pub use failed_move::{MoveResolution, ResolutionReport};
pub use move_workspace::MoveReport;
pub use remove::RemoveReport;
#[cfg(feature = "test-seams")]
pub use seams::SessionSeam;
pub use status::{
    MirrorDriftItem, SessionState, SessionStatus, StatusWarning, WorkspaceHealth, WorkspaceStatus,
};
pub use recover::{IntentRef, Leftover, RecoverReport};
pub use scene::{Scene, SceneOp};
pub use new::NewReport;
pub use new::NewWarning;
pub use new::Layout;
pub use new::add::{ADD_STAGING_PREFIX, AddReport};

/// The fetch window the daemon gives `new` and `add` (D53).
pub const NEW_FETCH_WINDOW: Duration = Duration::from_secs(30);
pub use seed::{SEED_MARK, SEED_STAGING_PREFIX, SeedRecord, SeedRefresh, SeedSource, seed_files, seed_plan};

pub type Result<T> = std::result::Result<T, StrataError>;

/// Prefix of a move's staging directory under the session root (§4.1).
pub const MOVE_STAGING_PREFIX: &str = ".strata-tmp-move-";
/// `bases/<repo>/.staging/`: where base generations are built (§5.2).
pub const BASE_STAGING_DIR: &str = ".staging";
/// How many dirty paths `remove`'s `Dirty` detail lists per repo (§5.6).
pub const DIRTY_PATHS_CAP: usize = 100;

/// The daemon's `BeforeRollout` / `BeforeRemove` checkpoint (§3): records
/// with the workspace's Tracker when it has one. Never enters or waits on
/// the session queue, never waits behind a repo lock.
pub trait BeforeCheckpoint: Sync {
    fn checkpoint(&self, ws: &WorkspaceRef<'_>, base: &Base, reason: CheckpointReason) -> Result<Checkpointed>;
}

/// The session handle the daemon holds (one per process).
pub struct Sessions<'a> {
    pub(crate) config: &'a Config,
    pub(crate) gits: &'a BTreeMap<RepoName, Git>,
    pub(crate) store: &'a Store,
    pub(crate) locks: &'a dyn RepoWriteLock,
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
            config,
            gits,
            store,
            locks,
            env,
            before,
            live_staging: Mutex::new(BTreeSet::new()),
            live_moves: Mutex::new(BTreeSet::new()),
            seams: seams::Seams::default(),
            fetched: Mutex::new(BTreeMap::new()),
            fetch_window: Mutex::new(Duration::ZERO),
        })
    }

    /// D53: a repo that `new` or `add` fetched successfully less than `w`
    /// ago is not fetched again by them (the chain top is still read under
    /// the repo lock). Zero turns the skip off.
    pub fn set_fetch_window(&self, w: Duration) {
        *self.fetch_window.lock().unwrap_or_else(|p| p.into_inner()) = w;
    }

    pub(crate) fn fetch_is_fresh(&self, repo: &RepoName) -> bool {
        let w = *self.fetch_window.lock().unwrap_or_else(|p| p.into_inner());
        if w.is_zero() {
            return false;
        }
        self.fetched.lock().unwrap_or_else(|p| p.into_inner()).get(repo).is_some_and(|t| t.elapsed() < w)
    }

    /// `repo`'s configuration and git handle, for callers that work on a
    /// repo outside any session (publish's check siblings, publish.md D21).
    pub fn repo_handles(&self, repo: &RepoName) -> Result<(&'a RepoConfig, &'a Git)> {
        Ok((self.repo_cfg(repo)?, self.git(repo)?))
    }

    pub(crate) fn repo_cfg(&self, repo: &RepoName) -> Result<&'a RepoConfig> {
        self.config.repos.get(repo).ok_or_else(|| no_such_repo(repo))
    }

    pub(crate) fn git(&self, repo: &RepoName) -> Result<&'a Git> {
        self.gits.get(repo).ok_or_else(|| no_such_repo(repo))
    }

    /// One critical section under `repo`'s write lock with the daemon's
    /// bounded wait: `Busy` passes through (a named refusal before the first
    /// visible write).
    pub(crate) fn with_lock<T>(&self, repo: &RepoName, mut f: impl FnMut() -> Result<T>) -> Result<T> {
        let mut out: Option<T> = None;
        let r = self.locks.with_write(repo, &mut || {
            out = Some(f()?);
            Ok(())
        })?;
        r?;
        Ok(out.expect("with_write ran the closure"))
    }

    /// The same after the call's first visible write: `Busy` from the outer
    /// wait is retried until the lock is had (§6.2; module-rulings "Waits
    /// after a call's first visible write are not bounded").
    pub(crate) fn with_lock_unbounded<T>(&self, repo: &RepoName, mut f: impl FnMut() -> Result<T>) -> Result<T> {
        loop {
            let mut out: Option<T> = None;
            match self.locks.with_write(repo, &mut || {
                out = Some(f()?);
                Ok(())
            }) {
                Err(e) if e.code == Code::Busy => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    continue;
                }
                Err(e) => return Err(e),
                Ok(r) => {
                    r?;
                    return Ok(out.expect("with_write ran the closure"));
                }
            }
        }
    }
}

pub(crate) fn no_such_repo(repo: &RepoName) -> StrataError {
    StrataError::new(Code::NoSuchRepo, format!("no repo named {repo} in the configuration"))
        .with_detail(serde_json::json!({ "repo": repo.as_str() }))
}
