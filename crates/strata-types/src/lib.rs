//! Shared identifiers, errors, and configuration types for git-strata.
//!
//! This crate does no I/O. Callers read the config file and pass the text to [`Config::parse`].

pub mod serde_rfc3339;

mod config;
mod error;
mod ids;
mod model;

pub use config::{Config, Paths, RepoConfig, StatusConfig};
pub use error::{Code, StrataError};
pub use ids::{
    BlobId, CheckpointId, CommitOid, DepsSetId, ProposalId, RepoId, RepoName, RepoPath, SessionId,
    TreeOid, ViewId,
};
pub use model::{
    Action, BaseGen, BaseManifest, Batch, Checkpoint, CheckpointReason, ConfirmToken, DepsSet,
    EntryKind, ManifestEntry, OverlayEntry, PublishTarget, PushOutcome, ReleaseGroup,
    SessionManifest, WorkspaceState, display_url,
};
