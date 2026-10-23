//! git-strata deps (docs/modules/deps.md): install-input keys, sealed deps-set
//! generations, cold installs, drift, proposals and rollout plans.

use std::time::Duration;

use strata_types::{RepoPath, StrataError};

mod inputs;

pub use inputs::{inputs_hash, is_input, nested_seed_caches, platform, set_id, toolchain};

pub type Result<T> = std::result::Result<T, StrataError>;

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

#[doc(hidden)]
pub use inputs::empty_inputs_hash;
