//! Shared identifiers, errors, and configuration types for git-strata.
//!
//! This crate does no I/O. Callers read the config file and pass the text to [`Config::parse`].

/// True if any linked Strata crate enabled its test seams.
pub const TEST_SEAMS: bool = cfg!(feature = "test-seams");

/// Give each shippable Mach-O a retained build marker, including after stripping.
#[macro_export]
macro_rules! mark_release_binary {
    () => {
        #[used]
        #[unsafe(link_section = "__TEXT,__strata_build")]
        static STRATA_BUILD: [u8; 24] = if $crate::TEST_SEAMS {
            *b"GIT-STRATA:TEST-SEAMS:V1"
        } else {
            *b"GIT-STRATA:PRODUCTION:V1"
        };
    };
}

pub mod serde_rfc3339;

mod config;
mod error;
mod ids;
mod mark;
mod model;
mod sync;
pub mod upstream;
pub use sync::SyncRequest;
pub use upstream::{
    RepoUpstream, Upstream, UpstreamAuto, UpstreamCommit, UpstreamDeps, UpstreamMain,
};

pub use config::{
    Config, DepsChanged, DevPorts, Paths, PublishDefaults, RebaseConfig, RepoConfig, RmConfig,
    StatusConfig, SyncConfig, SyncDeps, regenerable_problem,
};
pub use error::{Code, StrataError};
pub use ids::{
    AgentName, BlobId, CheckpointId, CommitOid, DepsSetId, ProposalId, RepoId, RepoName, RepoPath,
    SessionId, TreeOid, ViewId,
};
pub use mark::{MarkBy, MarkByKind, MarkState, SessionMark, validate_mark_note};
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

/// Wire compatibility floors while both the store schema and config format match.
pub const MIN_CLIENT: &str = "0.3.0";
pub const MIN_DAEMON: &str = "0.3.0";

/// A SemVer comparison key for a **stable** compatibility floor. Build
/// metadata does not affect the floor; prereleases of X.Y.Z precede X.Y.Z.
/// Two prereleases need not be ordered against one another on this wire.
pub fn release_version(version: &str) -> Option<([u64; 3], bool)> {
    fn valid_ids(suffix: &str, prerelease: bool) -> bool {
        suffix.split('.').all(|id| {
            !id.is_empty()
                && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && !(prerelease
                    && id.len() > 1
                    && id.starts_with('0')
                    && id.bytes().all(|b| b.is_ascii_digit()))
        })
    }
    let version = match version.split_once('+') {
        Some((core, build)) if valid_ids(build, false) => core,
        Some(_) => return None,
        None => version,
    };
    let (core, stable) = match version.split_once('-') {
        Some((core, pre)) if valid_ids(pre, true) => (core, false),
        Some(_) => return None,
        None => (version, true),
    };
    let mut parts = core.split('.');
    let mut parsed = [0; 3];
    for part in &mut parsed {
        let digits = parts.next()?;
        if !digits.bytes().all(|b| b.is_ascii_digit())
            || (digits.len() > 1 && digits.starts_with('0'))
        {
            return None;
        }
        *part = digits.parse().ok()?;
    }
    parts.next().is_none().then_some((parsed, stable))
}
