//! Shared identifiers, errors, and configuration types for git-strata.
//!
//! This crate does no I/O. Callers read the config file and pass the text to [`Config::parse`].

pub mod serde_rfc3339;

mod config;
mod error;
mod ids;
mod mark;
mod model;
mod sync;
pub mod upstream;
pub use sync::SyncRequest;
pub use upstream::{RepoUpstream, Upstream, UpstreamAuto, UpstreamCommit, UpstreamDeps, UpstreamMain};

pub use config::{Config, DepsChanged, DevPorts, Paths, PublishDefaults, RebaseConfig, RepoConfig, RmConfig, StatusConfig, SyncConfig, SyncDeps, regenerable_problem};
pub use error::{Code, StrataError};
pub use mark::{MarkBy, MarkByKind, MarkState, SessionMark, validate_mark_note};
pub use ids::{
    AgentName, BlobId, CheckpointId, CommitOid, DepsSetId, ProposalId, RepoId, RepoName, RepoPath, SessionId,
    TreeOid, ViewId,
};
pub use model::{
    Action, BaseGen, BaseManifest, Batch, Checkpoint, CheckpointReason, ConfirmToken, DepsSet,
    EntryKind, KeptTreeRef, ManifestEntry, OverlayEntry, PublishTarget, PushOutcome, ReleaseGroup,
    SessionManifest, WorkspaceState, display_url,
};

/// Latest supported store schema. The author of a store migration bumps this
/// alongside `strata-store`'s `schema::LATEST`.
pub const STORE_SCHEMA: u32 = 7;

/// Oldest release that can read the persisted formats this release writes.
/// The release owner updates it when a change to the store, pins, or config
/// raises the minimum compatible version.
pub const DOWNGRADE_FLOOR: &str = "0.3.0";
