//! strata-store: SQLite metadata plus a content-addressed BLAKE3 blob store
//! (docs/modules/store.md). One writer process per `<root>` (the daemon);
//! read-only handles for the CLI and for doctor.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::SystemTime;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use strata_types::{
    Action, BaseGen, BlobId, CheckpointId, CheckpointReason, CommitOid, DepsSet, DepsSetId, ProposalId,
    ReleaseGroup, RepoName, RepoPath, SessionId, SessionManifest, StrataError, TreeOid,
};

mod blob;
mod codec;
mod deps;
mod errors;
mod gens;
mod open;
mod schema;
mod seams;
mod txn;

#[cfg(feature = "test-seams")]
pub use seams::{Seam, SyncCounts};

pub type Result<T> = std::result::Result<T, StrataError>;

/// The store handle. `Send + Sync`; share it behind an `Arc`.
pub struct Store {
    pub(crate) root: PathBuf,
    /// `realpath(root)`, used for every rooted apfs call.
    pub(crate) real_root: PathBuf,
    pub(crate) read_only: bool,
    /// The writer connection (read-only handles: a query-only connection).
    pub(crate) db: Mutex<Connection>,
    pub(crate) readers: Mutex<Vec<Connection>>,
    /// L1: the flock on `store/LOCK`, held for the life of the handle.
    #[allow(dead_code)]
    pub(crate) writer_lock: Option<File>,
    /// L3: `put` holds it shared from dedup check to row commit; gc's
    /// unlink pass holds it exclusive.
    pub(crate) blob_gate: RwLock<()>,
    pub(crate) gc_active: Arc<AtomicBool>,
    pub(crate) gc_running: Mutex<()>,
    pub(crate) poisoned: AtomicBool,
    pub(crate) seams: seams::Seams,
}

impl Store {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    pub fn is_poisoned(&self) -> bool {
        self.poisoned.load(Ordering::SeqCst)
    }

    pub(crate) fn store_dir(&self) -> PathBuf {
        self.real_root.join("store")
    }

    /// `bases/<repo>/<tree>/<deps>/`
    pub fn base_dir(&self, g: &BaseGen) -> PathBuf {
        self.root.join("bases").join(g.repo.as_str()).join(g.tree.as_str()).join(g.deps.as_str())
    }

    /// `deps/<repo>/<deps>/`
    pub fn deps_dir(&self, repo: &RepoName, id: &DepsSetId) -> PathBuf {
        self.root.join("deps").join(repo.as_str()).join(id.as_str())
    }

    /// `deps/<repo>/.staging/<attempt>/`
    pub fn deps_staging_dir(&self, repo: &RepoName, attempt: u64) -> PathBuf {
        self.root.join("deps").join(repo.as_str()).join(".staging").join(attempt.to_string())
    }
}

#[cfg(feature = "test-seams")]
impl Store {
    pub fn arm(&self, seam: Seam) {
        self.seams.arm(seam)
    }

    pub fn wait_paused(&self) {
        self.seams.wait_paused()
    }

    pub fn release(&self) {
        self.seams.release()
    }

    pub fn sync_counts(&self) -> SyncCounts {
        self.seams.counts()
    }

    /// Arm a seam for the next `Store::open` on this thread. Needed for
    /// `InitBeforeRename`, which fires while the store is being created and
    /// no `Store` exists yet to call `arm` on.
    pub fn arm_next_open(seam: Seam) {
        seams::arm_next_open(seam)
    }

    /// Read back one pragma (as text) from the writer connection
    /// (`writer = true`) or a pooled reader connection.
    pub fn debug_pragma(&self, writer: bool, name: &str) -> Result<String> {
        let q = |c: &Connection| -> Result<String> {
            c.query_row(&format!("PRAGMA {name}"), [], |r| {
                let v: rusqlite::types::Value = r.get(0)?;
                Ok(match v {
                    rusqlite::types::Value::Integer(i) => i.to_string(),
                    rusqlite::types::Value::Text(t) => t,
                    other => format!("{other:?}"),
                })
            })
            .map_err(errors::sql_read)
        };
        if writer {
            q(&self.db.lock().unwrap_or_else(|p| p.into_inner()))
        } else {
            let r = self.reader()?;
            q(&r)
        }
    }
}

// ---------------------------------------------------------------- types

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lifecycle {
    Creating,
    Active,
    Removing,
    Removed,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Health {
    Ok,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRecord {
    pub manifest: SessionManifest,
    pub lifecycle: Lifecycle,
    pub created: SystemTime,
    pub removed: Option<SystemTime>,
    pub phase: Option<String>,
    pub scene: Option<serde_json::Value>,
    pub workspaces: BTreeMap<RepoName, WorkspaceRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceRecord {
    pub generation: u64,
    pub health: Health,
    pub scene: Option<serde_json::Value>,
    pub conflicts: Vec<RepoPath>,
    pub seeds: Vec<SeedRow>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeedRow {
    pub path: RepoPath,
    pub source: PathBuf,
    pub link: Option<PathBuf>,
    pub size: u64,
    pub source_mtime: SystemTime,
    pub copied: SystemTime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoveIntent {
    pub session: SessionId,
    pub repo: RepoName,
    pub n: u64,
    pub to: BaseGen,
    pub head: CommitOid,
    pub staging: PathBuf,
    pub phase: Option<String>,
    pub state: MoveState,
    pub conflicts: Option<Vec<RepoPath>>,
    pub old_tree: Option<PathBuf>,
    pub started: SystemTime,
    pub ended: Option<SystemTime>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MoveState {
    Preparing,
    Swapping,
    Done,
    Refused,
    Abandoned,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AttemptKind {
    Adopt,
    Build,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AttemptState {
    Running,
    Sealed,
    Failed,
    Refused,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DepsAttempt {
    pub id: u64,
    pub repo: RepoName,
    pub set: DepsSetId,
    pub kind: AttemptKind,
    pub state: AttemptState,
    pub staging: PathBuf,
    pub pid: Option<i32>,
    pub started: SystemTime,
    pub ended: Option<SystemTime>,
    pub error: Option<StrataError>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DepsOrigin {
    Adopt,
    Build,
    Empty,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DepsSetRecord {
    pub set: DepsSet,
    pub origin: DepsOrigin,
    pub sealed: SystemTime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewProposal {
    pub repo: RepoName,
    pub origin: Option<SessionId>,
    pub from: DepsSetId,
    pub from_inputs: String,
    pub to: DepsSetId,
    pub to_inputs: String,
    pub tree: Option<TreeOid>,
    pub head: Option<CommitOid>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposalRecord {
    pub id: ProposalId,
    pub repo: RepoName,
    pub origin: Option<SessionId>,
    pub from: DepsSetId,
    pub from_inputs: String,
    pub to: DepsSetId,
    pub to_inputs: String,
    pub tree: Option<TreeOid>,
    pub head: Option<CommitOid>,
    pub status: ProposalStatus,
    pub superseded_by: Option<ProposalId>,
    pub created: SystemTime,
    pub updated: SystemTime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProposalStatus {
    AwaitingPublish,
    Pending,
    Applied,
    Superseded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProposalDecision {
    Applied,
    Superseded { by: ProposalId },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DepsMain {
    pub repo: RepoName,
    pub tree: TreeOid,
    pub head: CommitOid,
    pub inputs_hash: String,
    pub set: DepsSetId,
    pub observed: SystemTime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointInfo {
    pub id: CheckpointId,
    pub repo: RepoName,
    pub base: BaseGen,
    pub at: SystemTime,
    pub reason: CheckpointReason,
    pub entries: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishRecord {
    pub group: ReleaseGroup,
    pub phase: PublishPhase,
    pub started: SystemTime,
    pub ended: Option<SystemTime>,
    pub outcome: Option<serde_json::Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PublishPhase {
    Running,
    Done,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefChange {
    pub at: SystemTime,
    pub repo: RepoName,
    pub op: RefOp,
    pub ref_name: String,
    pub old: Option<CommitOid>,
    pub new: CommitOid,
    pub outcome: RefOutcome,
    pub publish: Option<u64>,
    pub confirmed: Option<Action>,
    pub detail: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RefOp {
    UpdateRef,
    Push,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RefOutcome {
    Applied,
    HeadMoved,
    Pushed,
    Refused,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GcReport {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<RepoName>,
    #[serde(with = "strata_types::serde_rfc3339")]
    pub started: SystemTime,
    #[serde(with = "strata_types::serde_rfc3339")]
    pub ended: SystemTime,
    pub keep_secs: u64,
    pub sessions: u64,
    pub checkpoints: u64,
    pub overlay_entries: u64,
    pub proposals: u64,
    pub attempts: u64,
    pub base_gens: u64,
    pub base_trees: u64,
    pub deps_sets: u64,
    pub blobs: u64,
    pub blob_bytes: u64,
    pub dirs_retired: u64,
    pub trash_removed: u64,
    pub tmp_removed: u64,
    pub stuck: Vec<PathBuf>,
    pub verified: u64,
    pub corrupt: Vec<BlobId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<StrataError>,
}


