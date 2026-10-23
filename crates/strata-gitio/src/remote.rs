//! `fetch` into `refs/strata/remotes/` (§5.3).

use serde_json::json;
use strata_types::{Code, CommitOid};

use crate::command::{Opts, failed};
use crate::err::errd;
use crate::{Git, Result};

/// strata's remote-tracking ref: `refs/strata/remotes/<remote>/<branch>`.
pub fn remote_ref(remote: &str, branch: &str) -> String {
    format!("refs/strata/remotes/{remote}/{branch}")
}

/// gitio's own remote-name rule: `[A-Za-z0-9._-]`, not starting with `-` or `.`.
pub(crate) fn check_remote(remote: &str) -> Result<()> {
    let ok = !remote.is_empty()
        && !remote.starts_with('-')
        && !remote.starts_with('.')
        && remote.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(errd(Code::InvalidInput, format!("bad remote name {remote:?}"), json!({ "remote": remote })))
    }
}

impl Git {
    /// Branch names: no leading `-`, and `git check-ref-format refs/heads/<b>`.
    pub(crate) fn check_branch(&self, branch: &str) -> Result<()> {
        if branch.is_empty() || branch.starts_with('-') {
            return Err(errd(Code::InvalidInput, format!("bad branch name {branch:?}"), json!({ "branch": branch })));
        }
        let mut c = self.command();
        c.current_dir(&self.inner.common_dir).args(["check-ref-format"]).arg(format!("refs/heads/{branch}"));
        let out = self.run(c, Opts::default())?;
        match out.status {
            Some(0) => Ok(()),
            Some(1) => Err(errd(Code::InvalidInput, format!("bad branch name {branch:?}"), json!({ "branch": branch }))),
            _ => Err(failed("check-ref-format", &out)),
        }
    }

    /// Fetch `<remote>`'s `refs/heads/<branch>` into `remote_ref(remote,
    /// branch)` and return the tip. Writes nothing else.
    pub fn fetch(&self, remote: &str, branch: &str) -> Result<CommitOid> {
        check_remote(remote)?;
        self.check_branch(branch)?;
        let dst = remote_ref(remote, branch);
        let mut c = self.repo_command();
        c.args([
            "fetch",
            "--no-tags",
            "--no-write-fetch-head",
            "--no-auto-maintenance",
            "--no-write-commit-graph",
            "--no-prune",
            "--no-recurse-submodules",
            "--refmap=",
            "--end-of-options",
            remote,
        ])
        .arg(format!("+refs/heads/{branch}:{dst}"));
        let timeout = *self.inner.fetch_timeout.lock().unwrap();
        let out = self.run(c, Opts::c().timeout(timeout))?;
        if !out.ok() {
            let stderr = out.stderr_str();
            if stderr.contains("couldn't find remote ref") {
                return Err(errd(
                    Code::NoSuchBranch,
                    format!("{remote} has no branch {branch}"),
                    json!({ "remote": remote, "branch": branch, "stderr": stderr }),
                ));
            }
            if stderr.contains("does not appear to be a git repository") || stderr.contains("No such remote") {
                return Err(errd(
                    Code::InvalidInput,
                    format!("no remote {remote}"),
                    json!({ "remote": remote, "reason": "no-such-remote", "stderr": stderr }),
                ));
            }
            return Err(failed("fetch", &out));
        }
        match self.resolve_commit(&dst)? {
            Some(c) => Ok(c),
            None => Err(errd(Code::Unknown, format!("{dst} missing after fetch"), json!({ "ref": dst }))),
        }
    }
}

