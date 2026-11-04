//! Agent sync RPC input. `check` is handled by the upstream read-only lane.
use crate::{CommitOid, RepoName, RepoPath, SessionId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncRequest {
    pub session: SessionId,
    #[serde(default)]
    pub caller_cwd: Option<std::path::PathBuf>,
    #[serde(default)]
    pub caller_pids: Vec<i32>,
    #[serde(default)]
    pub allow_cwd: bool,
    #[serde(default)]
    pub allow_cwd_holders: bool,
    #[serde(default)]
    pub repos: Option<Vec<RepoName>>,
    #[serde(default)]
    pub check: bool,
    #[serde(default = "fetch_default")]
    pub fetch: bool,
    #[serde(default)]
    pub resolved: Vec<RepoPath>,
    #[serde(default)]
    pub keep: Vec<RepoPath>,
    #[serde(default)]
    pub target: Option<CommitOid>,
    #[serde(default)]
    pub generation: Option<u64>,
    #[serde(default)]
    pub accept_diverged: bool,
}
fn fetch_default() -> bool {
    true
}
