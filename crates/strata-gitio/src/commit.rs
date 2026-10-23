//! `commit_tree` (§5.10): one commit object, identity from config only.

use serde_json::json;
use strata_types::{Code, CommitOid, TreeOid};

use crate::command::Opts;
use crate::err::errd;
use crate::refs::parse_oid;
use crate::{Git, Result};

impl Git {
    /// `git -c user.useConfigOnly=true commit-tree <tree> -p <parent> -F -`;
    /// the message goes in verbatim. Moves no ref.
    pub fn commit_tree(&self, tree: &TreeOid, parent: &CommitOid, message: &str) -> Result<CommitOid> {
        let mut cmd = self.repo_command();
        cmd.args(["-c", "user.useConfigOnly=true", "commit-tree", tree.as_str(), "-p", parent.as_str(), "-F", "-"]);
        let out = self.run(cmd, Opts::c().stdin(message.as_bytes().to_vec()))?;
        if out.ok() {
            return parse_oid(&out.stdout, "commit-tree");
        }
        let stderr = out.stderr_str();
        if stderr.contains("auto-detection is disabled") {
            return Err(errd(
                Code::InvalidInput,
                format!("git identity is not configured: {}", stderr.trim()),
                json!({ "reason": "git-identity" }),
            ));
        }
        if !self.object_is(tree.as_str(), "tree")? {
            return Err(errd(
                Code::NoSuchCommit,
                format!("tree {} is not in the repository", tree.as_str()),
                json!({ "tree": tree.as_str() }),
            ));
        }
        Err(self.missing_commit_or(&[parent], "commit-tree", &out))
    }

    /// Whether `oid` names an object of type `kind` here.
    fn object_is(&self, oid: &str, kind: &str) -> Result<bool> {
        let mut cmd = self.repo_command();
        cmd.args(["cat-file", "-t", "--end-of-options", oid]);
        let out = self.run(cmd, Opts::c())?;
        Ok(out.ok() && String::from_utf8_lossy(&out.stdout).trim() == kind)
    }
}
