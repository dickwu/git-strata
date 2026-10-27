use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Instant, SystemTime};

use serde::{Deserialize, Serialize};

use crate::{
    BlobId, CheckpointId, CommitOid, DepsSetId, ProposalId, RepoId, RepoName, RepoPath, SessionId,
    TreeOid,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BaseGen {
    pub repo: RepoName,
    pub tree: TreeOid,
    pub deps: DepsSetId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepsSet {
    pub id: DepsSetId,
    pub repo: RepoName,
    pub inputs_hash: String,
    pub platform: String,
    pub toolchain: String,
    pub dirs: Vec<RepoPath>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseManifest {
    pub tree: TreeOid,
    pub entries: BTreeMap<RepoPath, ManifestEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub blob: BlobId,
    pub mode: u32,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlayEntry {
    pub path: RepoPath,
    #[serde(flatten)]
    pub kind: EntryKind,
    pub mode: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum EntryKind {
    Added { blob: BlobId },
    Modified { blob: BlobId },
    Deleted,
    Renamed { from: RepoPath, blob: BlobId },
    Untracked { blob: BlobId },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: CheckpointId,
    pub session: SessionId,
    pub repo: RepoName,
    pub base: BaseGen,
    #[serde(with = "crate::serde_rfc3339")]
    pub at: SystemTime,
    pub reason: CheckpointReason,
    pub entries: Vec<OverlayEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum CheckpointReason {
    ToolCall { id: String },
    Manual { message: String },
    BeforePublish,
    BeforeRestore,
    BeforeRollout,
    BeforeRemove,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Batch {
    pub session: SessionId,
    pub repo: RepoName,
    pub checkpoint: CheckpointId,
    pub paths: Vec<RepoPath>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseGroup {
    pub id: u64,
    pub batches: Vec<Batch>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishTarget {
    pub branch: String,
    pub push: bool,
    pub confirm: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "message")]
pub enum PushOutcome {
    Pushed,
    Refused(String),
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionManifest {
    pub id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<String>,
    pub root: PathBuf,
    pub repos: BTreeMap<RepoName, WorkspaceState>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceState {
    pub base: BaseGen,
    pub head: CommitOid,
    pub path: PathBuf,
    pub tracked: bool,
}

#[derive(Debug)]
pub struct ConfirmToken {
    pub action: Action,
    pub issued: Instant,
    pub nonce: u128,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Action {
    /// `url`: origin's effective push URL, read before the prompt and shown
    /// on it (userinfo redacted, [`display_url`]); the push goes to exactly
    /// it, and a different URL under the repo lock is `NotConfirmed`
    /// (`remote_changed`). Serialized redacted too (store records, error
    /// details never hold a token); empty only in a record written before
    /// review P1-15 (`serde(default)`). [实现时补的决定]
    PushDeploy {
        repo: RepoName,
        branch: String,
        #[serde(default, serialize_with = "ser_display_url")]
        url: String,
    },
    DepsApprove { proposal: ProposalId },
    RemoveDiscard { session: SessionId },
    ConfigRelax { repo: RepoId },
}

/// A push URL as shown to a person (prompt, plan, error detail): the
/// userinfo of a `scheme://userinfo@host/...` URL (which may hold a token)
/// becomes `***`; anything else is kept as is. The binding itself always
/// compares the full URL. [实现时补的决定] (review P1-15)
pub fn display_url(url: &str) -> String {
    let Some(i) = url.find("://") else { return url.to_string() };
    let rest = &url[i + 3..];
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    match rest[..end].rfind('@') {
        Some(at) => format!("{}***{}", &url[..i + 3], &rest[at..]),
        None => url.to_string(),
    }
}

fn ser_display_url<S: serde::Serializer>(url: &str, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&display_url(url))
}
