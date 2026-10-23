//! git-strata deps (docs/modules/deps.md): install-input keys, sealed deps-set
//! generations, cold installs, drift, proposals and rollout plans.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use strata_gitio::Git;
use strata_store::Store;
use strata_types::{DepsSetId, Paths, RepoConfig, RepoName, RepoPath, StrataError};

mod adopt;
mod attempts;
mod build;
mod drift;
mod ensure;
mod inputs;
mod layout;
mod open_files;
mod proposal;
mod rollout;
mod seal;
mod seams;

pub use adopt::adopt;
pub use attempts::attempts;
pub use build::{build, clone_into, find_set, key_of};
pub use drift::{Drift, drift};
pub use ensure::ensure;
pub use inputs::{inputs_hash, is_input, nested_seed_caches, platform, set_id, toolchain};
pub use open_files::{Hold, OpenFile, open_files};
pub use proposal::{
    ChainState, Proposal, ProposalOrigin, ProposalState, ProposeArgs, PublishChain, get_proposal, main_tip, observe_main,
    open_proposals, propose, publish_chain,
};
pub use rollout::{RolloutPlan, RolloutStep, SkipReason, Target, ensure_target, plan_rollout, plan_switch, record_rollout};
#[cfg(feature = "test-seams")]
pub use seams::test_seams;

pub type Result<T> = std::result::Result<T, StrataError>;

/// The context of one deps operation. The daemon owns every field.
pub struct Ctx<'a> {
    pub store: &'a Store,
    pub paths: &'a Paths,
    pub repo: &'a RepoName,
    /// `repo`'s configuration (config.md `[repo.<name>]`).
    pub cfg: &'a RepoConfig,
    /// `repo`'s git, opened on the main checkout.
    pub git: &'a Git,
    pub env: &'a InstallEnv,
    /// The daemon's per-repo write lock; deps takes it to seal (§5.3), in
    /// `observe_main` (§5.9) and to register in `ensure_target` (§5.10).
    pub locks: &'a dyn RepoWriteLock,
}

/// The daemon's per-repo write lock (design §8).
pub trait RepoWriteLock: Sync {
    /// Run `f` once while holding `repo`'s write lock, releasing it when `f`
    /// returns. The outer `Err` is only `Busy` (the bounded wait expired and
    /// `f` did not run); `Ok` carries `f`'s result.
    fn with_write(&self, repo: &RepoName, f: &mut dyn FnMut() -> Result<()>) -> Result<Result<()>>;
}

/// The environment `install` and `<program> --version` run with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallEnv {
    /// The explicit PATH (launchd has no user PATH).
    pub path: String,
    /// One `install`'s time limit; the build lock wait is bounded by it too.
    pub timeout: Duration,
}

impl InstallEnv {
    /// Default install time limit: 60 minutes.
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60 * 60);
}

/// The deps configuration that enters the set id (§4.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DepsConfigKey {
    /// `deps_dirs`, byte order.
    pub deps_dirs: Vec<RepoPath>,
    /// `nested_seed_caches(cfg)`, byte order.
    pub nested_seed_caches: Vec<RepoPath>,
    /// `install` split per config.md §2.4; empty without `install`.
    pub install: Vec<String>,
}

/// One adoption or cold install, mirroring a store attempt record (§4.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildAttempt {
    /// The store's attempt number; the staging dir is `.staging/<id>/`.
    pub id: u64,
    pub repo: RepoName,
    /// The deps set this attempt was to seal.
    pub set: DepsSetId,
    pub kind: BuildKind,
    pub state: BuildState,
    /// Absolute path of the staging directory.
    pub staging: PathBuf,
    /// The install process (a process group leader) of a cold install.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<i32>,
    #[serde(with = "strata_types::serde_rfc3339")]
    pub started: SystemTime,
    /// Why it failed (detail shapes in §6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<StrataError>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuildKind {
    Adopt,
    Build,
}

/// `attempts` reports only these; sealed, refused and attempts whose staging
/// is gone are not reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuildState {
    /// The build lock is held: building.
    Running,
    /// Still `running` in the store but nobody holds the build lock: the
    /// holder died. Derived on read, never written back.
    Interrupted,
    /// Install failed or timed out, validation failed, sealing failed, or
    /// closed after its holder died. The scene is kept.
    Failed,
}

#[doc(hidden)]
pub use inputs::empty_inputs_hash;
