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
