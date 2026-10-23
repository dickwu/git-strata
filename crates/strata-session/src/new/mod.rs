//! `new` (§5.1): validate (phase A), resolve heads and bases (phase B),
//! register (phase C), build the layout (phase D), observe main (phase E).

use serde::{Deserialize, Serialize};
use serde_json::json;
use strata_types::{Code, CommitOid, RepoName, SessionManifest, StrataError, TreeOid};

use crate::{Result, Sessions, dispatch};

pub(crate) mod validate;

/// `new`'s result: the manifest, plus warnings about observing main after
/// the session was built (§5.1 step 15) [D15].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewReport {
    pub manifest: SessionManifest,
    /// Empty means no warnings.
    pub warnings: Vec<NewWarning>,
}

/// Serialised with a `kind` tag carrying the variant name (types.md §2.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum NewWarning {
    /// `deps::observe_main` failed for this repo; `detail` is deps' error
    /// as returned. The session is Active all the same.
    ObserveFailed { repo: RepoName, detail: StrataError },
}

impl Sessions<'_> {
    /// Create a session of `repos` (§5.1).
    #[allow(clippy::new_ret_no_self)] // `new` is the spec's operation name, not a constructor
    pub fn new(&self, ticket: Option<&str>, repos: &[RepoName]) -> Result<NewReport> {
        let v = self.validate_new(ticket, repos)?;
        let _heads = self.resolve_heads(&v.tracked)?;
        Err(StrataError::new(Code::Unsupported, "new: building the session layout is not implemented yet")
            .with_detail(json!({ "reason": "not_implemented", "session": v.id.as_str() })))
    }

    /// §5.1 step 8 for every configured repo: fetch the default branch and
    /// read the chain top in one critical section, then the tree.
    pub(crate) fn resolve_heads(&self, _tracked: &[RepoName]) -> Result<Vec<(RepoName, (CommitOid, TreeOid))>> {
        let all: Vec<RepoName> = self.config.repos.keys().cloned().collect();
        dispatch::each_repo(&all, &|r| self.seams.hit("head", Some(r)), &|r| self.head_of(r))
    }

    fn head_of(&self, repo: &RepoName) -> Result<(CommitOid, TreeOid)> {
        let cfg = self.repo_cfg(repo)?;
        let git = self.git(repo)?;
        let head = self.with_lock(repo, || {
            git.fetch("origin", &cfg.default_branch)?;
            strata_deps::main_tip(cfg, git)
        })?;
        let Some(head) = head else {
            return Err(StrataError::new(
                Code::NoSuchBranch,
                format!("repo {repo} has no main tip for origin/{}", cfg.default_branch),
            )
            .with_detail(json!({ "repo": repo.as_str(), "remote": "origin", "branch": cfg.default_branch })));
        };
        let tree = git.tree_of(&head)?;
        Ok((head, tree))
    }
}
