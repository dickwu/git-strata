//! git-stratad: the per-user daemon (daemon.md). One process per user,
//! json-lines RPC over `<config dir>/daemon.sock`, the repo write locks and
//! session queues, the confirm order, pinned flags and the launchd plist.
//!
//! The daemon owns no domain logic: every method calls one lower-layer
//! function (session, overlay, search, deps, publish, confirm).

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use strata_types::{RepoName, SessionId, StrataError};

pub mod locks;
pub mod rpc;

pub type Result<T> = std::result::Result<T, StrataError>;

/// D2: how long the first lock or queue take of a request waits before
/// anything visible was written (→ `Busy`).
pub const LOCK_WAIT: Duration = Duration::from_secs(30);
/// D7: the longest request line (bytes, excluding the newline).
pub const MAX_FRAME: usize = 16 * 1024 * 1024;
/// D11: how often the reconcile loop runs.
pub const RECONCILE_EVERY: Duration = Duration::from_secs(3600);
/// D12: `gc_repo` keep window.
pub const GC_KEEP: Duration = Duration::from_secs(14 * 24 * 3600);
/// D1: the PATH installs and checks run with (launchd gives a bare PATH).
pub const DEFAULT_PATH: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin";

/// The launchd label of the agent.
pub const AGENT_LABEL: &str = "com.gwd.git-stratad";

/// `~/Library/Application Support/git-strata` for `home`.
pub fn config_dir_for_home(home: &Path) -> PathBuf {
    home.join("Library/Application Support/git-strata")
}

/// `<config dir>/daemon.sock`: a same-machine unix socket, nothing else.
pub fn socket_path(config_dir: &Path) -> PathBuf {
    config_dir.join("daemon.sock")
}

/// `<config dir>/daemon.lock`: the single-instance flock.
pub fn lock_path(config_dir: &Path) -> PathBuf {
    config_dir.join("daemon.lock")
}

/// `<config dir>/pins.json`.
pub fn pins_path(config_dir: &Path) -> PathBuf {
    config_dir.join("pins.json")
}

/// `<config dir>/daemon.err.log`.
pub fn err_log_path(config_dir: &Path) -> PathBuf {
    config_dir.join("daemon.err.log")
}

/// The RPC-level refusals (no `code`; daemon.md §4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RpcReject {
    UnknownMethod,
    Failed,
}

/// The pinned safety flags of one repo (pins.json).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinnedFlags {
    pub deploy_on_push: bool,
    pub confirm: bool,
    pub push: bool,
}

/// What `daemon_status` and the stderr alert report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DaemonWarning {
    Quarantine { path: PathBuf },
    AttachOutside { reason: String, index: PathBuf },
    LongHold { repo: RepoName, method: String, since: SystemTime },
    UnknownPublish { id: u64 },
    PublishRunning { id: u64 },
    ConfigRelaxPending { repo: RepoName },
    StorePoisoned,
    VerifyFailed { message: String },
    AnchorNotAdvanced { repo: RepoName },
}

/// One held repo write lock (status / daemon_status).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockHold {
    pub repo: RepoName,
    pub method: String,
    pub session: Option<SessionId>,
    pub since: SystemTime,
}

/// `chain_resolve`'s choice; serialized as the variant name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChainChoice {
    AcceptOriginReplay,
    AcceptOriginDrop,
    Restore,
}
