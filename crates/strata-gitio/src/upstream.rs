//! Bounded local-ref history reads for main-sync status. Never fetches.
use crate::command::Opts;
use crate::refs::parse_oid;
use crate::{Git, Result};
use strata_types::CommitOid;

#[derive(Clone, Debug)]
pub struct CommitSummary {
    pub oid: CommitOid,
    pub subject: String,
    pub author: String,
    /// Values of the publish message's `sessions:` trailer.
    pub sessions: Vec<String>,
}

impl Git {
    /// Count all commits in `base..tip`, stopping at `limit` (not first-parent).
    pub fn count_between(&self, base: &CommitOid, tip: &CommitOid, limit: u32) -> Result<u32> {
        let mut c = self.repo_command();
        c.args([
            "rev-list",
            "--count",
            &format!("--max-count={limit}"),
            tip.as_str(),
            "--not",
            base.as_str(),
            "--",
        ]);
        let out = self.run(c, Opts::c())?;
        if !out.ok() {
            return Err(self.missing_commit_or(&[base, tip], "rev-list", &out));
        }
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .map_err(|_| crate::command::failed("rev-list count", &out))
    }

    /// At most `limit` commits, including merge parents. NUL separators keep
    /// subjects/authors separate; Git parses only the final trailer block.
    pub fn history_between(
        &self,
        base: &CommitOid,
        tip: &CommitOid,
        limit: usize,
    ) -> Result<Vec<CommitSummary>> {
        let mut c = self.repo_command();
        c.args([
            "log",
            "-z",
            "--no-show-signature",
            "--format=%H%x00%s%x00%an%x00%(trailers:key=sessions,valueonly,separator=%x1f)",
            &format!("--max-count={limit}"),
            tip.as_str(),
            "--not",
            base.as_str(),
            "--",
        ]);
        let out = self.run(c, Opts::c())?;
        if !out.ok() {
            return Err(self.missing_commit_or(&[base, tip], "log", &out));
        }
        let mut fields = out.stdout.split(|b| *b == 0);
        let mut rows = Vec::new();
        while let Some(oid) = fields.next().filter(|b| !b.is_empty()) {
            let (Some(subject), Some(author), Some(sessions)) =
                (fields.next(), fields.next(), fields.next())
            else {
                return Err(crate::command::failed("log fields", &out));
            };
            rows.push(CommitSummary {
                oid: parse_oid(oid, "log")?,
                subject: String::from_utf8_lossy(subject).into_owned(),
                author: String::from_utf8_lossy(author).into_owned(),
                sessions: String::from_utf8_lossy(sessions)
                    .split([',', '\u{1f}', '\n'])
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect(),
            });
        }
        Ok(rows)
    }
}

/// Publish identity parsed by Git from the final trailer paragraph.
#[derive(Clone, Debug)]
pub struct PublishCommit {
    pub oid: CommitOid,
    pub subject: String,
    pub publishes: Vec<String>,
    /// Values of the publish message's `sessions:` trailer.
    pub sessions: Vec<String>,
}

/// How far back from the tip [`Git::landed_copy`] looks, first parent.
pub const LANDED_COPY_LIMIT: usize = 500;

impl Git {
    pub fn commits_between_trailers(&self, anchor: &CommitOid, tip: &CommitOid, limit: usize) -> Result<Vec<PublishCommit>> {
        let mut c = self.repo_command();
        c.args(["log", "-z", "--first-parent", "--no-show-signature",
            "--format=%H%x00%s%x00%(trailers:key=Strata-Publish,valueonly,separator=%x01)%x00%(trailers:key=sessions,valueonly,separator=%x1f)",
            &format!("--max-count={limit}"), tip.as_str(), "--not", anchor.as_str(), "--"]);
        let out = self.run(c, Opts::c())?;
        if !out.ok() { return Err(self.missing_commit_or(&[anchor, tip], "log", &out)); }
        let mut fields = out.stdout.split(|b| *b == 0);
        let mut rows = Vec::new();
        while let Some(oid) = fields.next().filter(|b| !b.is_empty()) {
            let (Some(subject), Some(trailers), Some(sessions)) = (fields.next(), fields.next(), fields.next()) else {
                return Err(crate::command::failed("log trailer fields", &out));
            };
            rows.push(PublishCommit {
                oid: parse_oid(oid, "log")?,
                subject: String::from_utf8_lossy(subject).into_owned(),
                publishes: String::from_utf8_lossy(trailers).split(['\u{1}', '\n']).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect(),
                sessions: String::from_utf8_lossy(sessions)
                    .split([',', '\u{1f}', '\n'])
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect(),
            });
        }
        Ok(rows)
    }

    /// The commit on `tip`'s first-parent line past `merge_base(commit, tip)`
    /// whose trailers name both `<id> (<repo>)` and `session`: `commit`'s
    /// replayed copy. Publish ids are per store and `tip` holds other
    /// developers' commits, so the id alone proves nothing, and a subject-only
    /// commit names no session. A pruned `commit` has no merge base, so no copy
    /// is provable: `None`, not an error.
    pub fn landed_copy(
        &self,
        id: u64,
        repo: &strata_types::RepoName,
        session: &strata_types::SessionId,
        commit: &CommitOid,
        tip: &CommitOid,
    ) -> Result<Option<CommitOid>> {
        let anchor = match self.merge_base(commit, tip) {
            Ok(Some(m)) => m,
            Ok(None) => return Ok(None),
            Err(e) if e.code == strata_types::Code::NoSuchCommit => return Ok(None),
            Err(e) => return Err(e),
        };
        let wanted = format!("{id} ({repo})");
        let commits = self.commits_between_trailers(&anchor, tip, LANDED_COPY_LIMIT)?;
        Ok(commits
            .into_iter()
            .find(|c| {
                c.publishes.iter().any(|p| *p == wanted)
                    && c.sessions.iter().any(|s| s == session.as_str())
            })
            .map(|c| c.oid))
    }

    /// Final changed paths, after clean filters; tree objects are enough.
    pub fn changed_path_count(&self, base: &CommitOid, tree: &strata_types::TreeOid) -> Result<usize> {
        let mut c = self.repo_command();
        c.args(["diff-tree", "-r", "--no-renames", "--no-commit-id", "--name-only", "-z", base.as_str(), tree.as_str(), "--"]);
        let out = self.run(c, Opts::c())?;
        if !out.ok() { return Err(crate::command::failed("diff-tree", &out)); }
        Ok(out.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()).count())
    }

    /// All commits on head that neither accepted publish baseline contains.
    pub fn base_only(&self, head: &CommitOid, main: &CommitOid, b: &CommitOid, limit: usize) -> Result<(Vec<(CommitOid, String)>, bool)> {
        let mut c = self.repo_command();
        c.args(["log", "-z", "--no-show-signature", "--format=%H%x00%s", &format!("--max-count={}", limit.saturating_add(1)), head.as_str(), "--not", main.as_str(), b.as_str(), "--"]);
        let out = self.run(c, Opts::c())?;
        if !out.ok() { return Err(self.missing_commit_or(&[head, main, b], "log", &out)); }
        let mut fields = out.stdout.split(|b| *b == 0);
        let mut rows = Vec::new();
        while let Some(oid) = fields.next().filter(|b| !b.is_empty()) {
            let Some(subject) = fields.next() else { return Err(crate::command::failed("log fields", &out)); };
            rows.push((parse_oid(oid, "log")?, String::from_utf8_lossy(subject).into_owned()));
        }
        let more = rows.len() > limit;
        rows.truncate(limit);
        Ok((rows, more))
    }
}

impl Git {
    /// Paths tracked by this workspace's commit, independent of the main index.
    pub fn tracked_paths(&self, head: &CommitOid) -> Result<Vec<strata_types::RepoPath>> {
        let mut c = self.repo_command();
        c.args(["ls-tree", "-r", "-z", "--name-only", head.as_str(), "--"]);
        let out = self.run(c, Opts::c())?;
        if !out.ok() { return Err(self.missing_commit_or(&[head], "ls-tree", &out)); }
        out.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()).map(|p| {
            let p = std::str::from_utf8(p).map_err(|_| crate::command::failed("ls-tree path", &out))?;
            strata_types::RepoPath::try_from(p.to_owned())
        }).collect()
    }
}
