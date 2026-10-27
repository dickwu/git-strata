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

pub mod client;
pub mod confirm_flow;
pub mod du;
pub mod fetch_after;
pub mod handlers;
pub mod locks;
pub mod plist;
pub mod resolve;
pub mod rpc;
pub mod startup;
pub mod track;
pub mod waiting;

/// The confirm seams a test daemon is built with (feature `test-seams`
/// only; a binary built with them refuses to start). Re-exported so the
/// cli's end-to-end tests need no direct strata-confirm dependency.
#[cfg(feature = "test-seams")]
pub mod seams {
    pub use strata_confirm::Confirmer;
    pub use strata_confirm::seams::{FakeClock, PromptLog, ScriptedAuth, Verdict, bypass_env_names};

    static SCENE_SYNC_FAILS: std::sync::Mutex<Vec<std::path::PathBuf>> = std::sync::Mutex::new(Vec::new());

    /// Review P1-13: make every fsync of a push scene record under `root`
    /// fail with EIO (`on`), or stop doing so. Keyed by root so parallel
    /// tests are unaffected.
    pub fn set_scene_sync_fails(root: &std::path::Path, on: bool) {
        let mut v = SCENE_SYNC_FAILS.lock().unwrap_or_else(|p| p.into_inner());
        v.retain(|r| r != root);
        if on {
            v.push(root.to_path_buf());
        }
    }

    pub(crate) fn scene_sync_fails(p: &std::path::Path) -> bool {
        SCENE_SYNC_FAILS.lock().unwrap_or_else(|p| p.into_inner()).iter().any(|r| p.starts_with(r))
    }

    type Hook = std::sync::Arc<dyn Fn() + Send + Sync>;
    static AFTER_CONFIRM: std::sync::Mutex<Vec<(std::path::PathBuf, Hook)>> = std::sync::Mutex::new(Vec::new());

    /// Review P1-15: run `hook` after every passed push confirmation of the
    /// daemon whose root is `root`, before its repo lock is taken (`None`
    /// removes it). Keyed by root so parallel tests are unaffected.
    pub fn set_after_confirm(root: &std::path::Path, hook: Option<Hook>) {
        let mut v = AFTER_CONFIRM.lock().unwrap_or_else(|p| p.into_inner());
        v.retain(|(r, _)| r != root);
        if let Some(h) = hook {
            v.push((root.to_path_buf(), h));
        }
    }

    pub(crate) fn after_confirm(root: &std::path::Path) {
        let h = AFTER_CONFIRM.lock().unwrap_or_else(|p| p.into_inner()).iter().find(|(r, _)| r == root).map(|(_, h)| h.clone());
        if let Some(h) = h {
            h();
        }
    }
}

/// Test seam point after a passed push confirmation (no-op in a normal
/// build); see `seams::set_after_confirm`.
pub(crate) fn after_confirm(_root: &Path) {
    #[cfg(feature = "test-seams")]
    seams::after_confirm(_root);
}

pub type Result<T> = std::result::Result<T, StrataError>;

/// D2: how long the first lock or queue take of a request waits before
/// anything visible was written (→ `Busy`).
pub const LOCK_WAIT: Duration = Duration::from_secs(30);
/// D7: the longest request line (bytes, excluding the newline).
pub const MAX_FRAME: usize = 16 * 1024 * 1024;
/// D11: how often the reconcile loop runs.
pub const RECONCILE_EVERY: Duration = Duration::from_secs(3600);
/// How often the reconcile thread refreshes seed copies whose main-checkout
/// source changed (`Sessions::refresh_seeds`; a few lstats per seed).
pub const SEED_REFRESH_EVERY: Duration = Duration::from_secs(2);
/// D12: `gc_repo` keep window.
pub const GC_KEEP: Duration = Duration::from_secs(14 * 24 * 3600);
/// D33: the store-wide gc's keep (removed sessions, decided proposals,
/// closed attempts). Longer than [`GC_KEEP`]: what it deletes (checkpoints
/// of removed sessions) cannot be rebuilt. Only `git-strata gc --store`
/// runs it.
pub const STORE_GC_KEEP: Duration = Duration::from_secs(30 * 24 * 3600);
/// D1: the system part of the PATH installs and checks run with (launchd
/// gives a bare PATH). [`install_path_for_home`] puts `~/.bun/bin` in front.
pub const DEFAULT_PATH: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin";

/// D1: the PATH installs and checks run with, and the agent's PATH:
/// `<home>/.bun/bin` (bun's own installer puts it there, not in Homebrew),
/// then [`DEFAULT_PATH`].
pub fn install_path_for_home(home: &Path) -> String {
    format!("{}:{DEFAULT_PATH}", home.join(".bun/bin").display())
}

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
    /// Review M6: a standalone (or restore) push whose outcome is unknown;
    /// its staging directory `scene` (with `push.json`) is kept.
    UnknownPush { repo: RepoName, commit: strata_types::CommitOid, scene: PathBuf },
    /// Review P1-5: a `refs/strata/**/*.lock` left by a killed ref
    /// transaction, removed at startup.
    StaleRefLock { repo: RepoName, path: PathBuf },
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
