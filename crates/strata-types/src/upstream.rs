//! Shared main-sync status wire types (main-sync design §5).
use crate::{CommitOid, DepsSetId, ProposalId, RepoName, RepoPath, SyncDeps, TreeOid};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Upstream {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of: Option<String>,
    pub clean: bool,
    pub repos: BTreeMap<RepoName, RepoUpstream>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoUpstream {
    pub following: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behind: Option<u32>,
    pub capped: bool,
    pub unchecked: bool,
    pub diverged: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub base_only: Vec<CommitOid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main: Option<UpstreamMain>,
    pub base_head: CommitOid,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commits: Vec<UpstreamCommit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deps: Option<UpstreamDeps>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto: Option<UpstreamAuto>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<RepoPath>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unpublished: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamMain {
    pub head: CommitOid,
    pub tree: TreeOid,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamCommit {
    pub oid: CommitOid,
    pub subject: String,
    pub author: String,
    pub own: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamDeps {
    pub proposal: ProposalId,
    pub mode: SyncDeps,
    pub state: String,
    pub from: DepsSetId,
    pub to: DepsSetId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// `install_failed` only: the worker's error; `detail.staging` is the
    /// kept staging to inspect and remove before `deps approve` (O8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamAuto {
    pub source: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub since: String,
    pub attempts: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<UpstreamMain>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
}
