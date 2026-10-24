//! `replay_commits` (daemon.md D18): copy `exclusive_base..tip` onto
//! `onto`, oldest first, along first parents. Writes commit objects only
//! (into this checkout's object store); never a ref, never a push.

use serde_json::json;
use strata_types::{Code, CommitOid, TreeOid};

use crate::command::{Opts, failed};
use crate::err::errd;
use crate::refs::parse_oid;
use crate::{Git, Result};

impl Git {
    /// Read-only: the commits reachable from `tip` and not from
    /// `exclusive_base`, first-parent only, newest first, each with its
    /// subject line (what a push of `exclusive_base..tip` publishes; the cli
    /// lists them before a `PushDeploy` prompt, cli.md §5 step 2). At most
    /// `limit` entries; the bool is whether more were left out.
    pub fn commits_between(
        &self,
        exclusive_base: &CommitOid,
        tip: &CommitOid,
        limit: usize,
    ) -> Result<(Vec<(CommitOid, String)>, bool)> {
        let mut c = self.repo_command();
        let max = format!("--max-count={}", limit.saturating_add(1));
        c.args(["log", "--first-parent", "--no-show-signature", "--format=%H%x00%s", &max, tip.as_str(), "--not", exclusive_base.as_str(), "--"]);
        let out = self.run(c, Opts::c())?;
        if !out.ok() {
            return Err(self.missing_commit_or(&[exclusive_base, tip], "log", &out));
        }
        let mut v = Vec::new();
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let (oid, subject) = line.split_once('\0').unwrap_or((line, ""));
            v.push((parse_oid(oid.as_bytes(), "log")?, subject.to_string()));
        }
        let more = v.len() > limit;
        v.truncate(limit);
        Ok((v, more))
    }

    /// The commits reachable from `tip` and not from `exclusive_base`,
    /// first-parent only, oldest first, each re-applied (3-way against its
    /// first parent) on the previous new commit; the first goes on `onto`.
    /// A merge commit becomes a single-parent commit with its own tree
    /// relative to its first parent (no re-merge). A piece that does not
    /// apply → `Conflict` (no ref changed: this never writes refs).
    /// Identity from config only (`git-identity`). Returns the new tip,
    /// or `onto` when the range is empty.
    pub fn replay_commits(&self, exclusive_base: &CommitOid, tip: &CommitOid, onto: &CommitOid) -> Result<CommitOid> {
        let mut c = self.repo_command();
        c.args(["rev-list", "--reverse", "--first-parent", tip.as_str(), "--not", exclusive_base.as_str(), "--"]);
        let out = self.run(c, Opts::c())?;
        if !out.ok() {
            return Err(self.missing_commit_or(&[exclusive_base, tip], "rev-list", &out));
        }
        // `onto` must be a commit here (NoSuchCommit otherwise).
        self.is_ancestor(onto, onto)?;
        let range: Vec<CommitOid> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| parse_oid(l.as_bytes(), "rev-list"))
            .collect::<Result<_>>()?;
        let mut head = onto.clone();
        for commit in &range {
            let parent = self.first_parent(commit)?.ok_or_else(|| {
                errd(
                    Code::InvalidInput,
                    format!("commit {} has no parent to replay against", commit.as_str()),
                    json!({ "reason": "no_replay_range", "commit": commit.as_str() }),
                )
            })?;
            let mut m = self.repo_command();
            m.args([
                "merge-tree",
                "--write-tree",
                "--no-messages",
                &format!("--merge-base={}", parent.as_str()),
                head.as_str(),
                commit.as_str(),
            ]);
            let mo = self.run(m, Opts::c())?;
            let tree = match mo.status {
                Some(0) => {
                    let s = String::from_utf8_lossy(&mo.stdout);
                    let line = s.lines().next().unwrap_or("").trim().to_string();
                    TreeOid::try_from(line).map_err(|_| failed("merge-tree", &mo))?
                }
                Some(1) => {
                    return Err(errd(
                        Code::Conflict,
                        format!("commit {} does not apply onto {}", commit.as_str(), head.as_str()),
                        json!({ "commit": commit.as_str(), "onto": head.as_str() }),
                    ));
                }
                _ => return Err(failed("merge-tree", &mo)),
            };
            let message = self.commit_message(commit)?;
            head = self.commit_tree(&tree, &head, &message)?;
        }
        Ok(head)
    }

    fn first_parent(&self, commit: &CommitOid) -> Result<Option<CommitOid>> {
        let mut c = self.repo_command();
        c.args(["rev-parse", "--verify", "--quiet", &format!("{}^1", commit.as_str())]);
        let out = self.run(c, Opts::c())?;
        match out.status {
            Some(0) => Ok(Some(parse_oid(&out.stdout, "rev-parse")?)),
            Some(1) => Ok(None),
            _ => Err(failed("rev-parse", &out)),
        }
    }

    /// The raw message of `commit` (everything after the header block).
    fn commit_message(&self, commit: &CommitOid) -> Result<String> {
        let mut c = self.repo_command();
        c.args(["cat-file", "commit", commit.as_str()]);
        let out = self.run(c, Opts::c())?;
        if !out.ok() {
            return Err(failed("cat-file commit", &out));
        }
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        Ok(text.split_once("\n\n").map(|(_, m)| m.to_string()).unwrap_or_default())
    }
}
