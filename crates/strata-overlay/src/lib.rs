//! git-strata overlay (docs/modules/overlay.md): the whole-file differences
//! of a workspace against its base generation, checkpoints that store only
//! what changed, restore, rebase and projection.
//!
//! Layer 2: depends on strata-types, strata-apfs, strata-gitio and
//! strata-store only.

// Interim: helpers the scan and restore (tasks 4-10) use.
mod checkpoint;
mod classify;
mod compute;
#[allow(dead_code)]
mod context;
#[allow(dead_code)]
mod fault;
#[allow(dead_code)]
mod hash;
mod inbox;
mod lww;
mod manifest;
mod overlay;
mod project;
mod rebase;
mod rename;
mod restore;
mod scan;
mod tracker;

use serde::{Deserialize, Serialize};
use strata_types::{RepoPath, StrataError};

pub use checkpoint::{Checkpointed, checkpoint};
pub use compute::{Scan, compute, reconcile_full};
pub use context::{Base, WorkspaceRef};
#[cfg(feature = "test-seams")]
pub use fault::{OverlayFault, inject_fault, release_fault, wait_fault};
#[cfg(feature = "test-seams")]
pub use hash::{ReadProbe, read_candidate};
pub use inbox::Inbox;
#[cfg(feature = "test-seams")]
pub use inbox::InboxView;
pub use lww::{LwwRules, lww_matches};
pub use manifest::build_manifest;
pub use overlay::*;
pub use project::project;
pub use rebase::{RebaseOptions, Rebased, rebase};
pub use restore::{RestoreRequest, Restored, restore};
pub use tracker::Tracker;

/// types.md §8 `Result<T>`.
pub type Result<T> = std::result::Result<T, StrataError>;

/// Default bound of the dirty set (§5.4); above it the next scan is full.
pub const DEFAULT_DIRTY_CAPACITY: usize = 65_536;
/// Content limit of a three-way merge in rebase (§9): larger files conflict.
pub const MERGE_MAX_BYTES: u64 = 64 * 1024 * 1024;
/// The `.lww` rules file inside the repository (design §4).
pub const LWW_FILE: &str = ".strata/lww";
/// Prefix of restore's private staging directory under the session root (§4.2).
pub const RESTORE_TEMP_PREFIX: &str = ".strata-tmp-restore-";

/// Facts status reports (session.md §5.5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum OverlayWarning {
    /// setuid, setgid or sticky on a file; `mode` is the octal permission text ("4755").
    SpecialModeBits { path: RepoPath, mode: String },
    /// The file changed while it was read, or its gen count is unavailable.
    Unstable { path: RepoPath },
    /// A FIFO, socket or device file: never opened, never an entry.
    SpecialFile { path: RepoPath },
    /// A nested repository (a directory holding `.git`), entered as restricted.
    EmbeddedRepo { path: RepoPath },
}

/// Work counters of one scan (§7 asserts workload with them, never timing).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanStats {
    pub dirs_listed: u64,
    pub entries_seen: u64,
    pub files_hashed: u64,
    pub bytes_hashed: u64,
    pub vouched: u64,
    /// The whole tree was walked.
    pub full: bool,
    /// Cold: the attribute cache was empty, every candidate was read.
    pub cold: bool,
}
