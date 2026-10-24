//! `push(staging, remote, src_ref, dst_branch, timeout)` (§5.11): a
//! definite `Pushed` / `Refused` only from this ref's porcelain status
//! line; everything else is `Unknown`. Never forces, never retries.

use std::path::Path;
use std::time::Duration;

use serde_json::json;
use strata_types::{Code, CommitOid, PushOutcome};

use crate::command::{Opts, failed};
use crate::err::errd;
use crate::publish_io::RefUpdate;
use crate::refs::check_strata_ref;
use crate::seams::{PUSH_BEFORE_RECORD, PUSH_BEFORE_SEND};
use crate::remote::{check_remote, remote_ref};
use crate::{Git, Result};

/// Last `n` bytes of `b`, lossy, for the log.
fn tail(b: &[u8], n: usize) -> String {
    String::from_utf8_lossy(&b[b.len().saturating_sub(n)..]).into_owned()
}

/// Classify `git push --porcelain` stdout for the status line of `spec`
/// (`<src>:<dst>`); `None` when there is no such line.
pub(crate) fn status_of(stdout: &[u8], spec: &str) -> Option<(char, String)> {
    let text = String::from_utf8_lossy(stdout);
    for line in text.lines() {
        let mut it = line.splitn(3, '\t');
        let (Some(flag), Some(refs), summary) = (it.next(), it.next(), it.next()) else { continue };
        let mut chars = flag.chars();
        let (Some(f), None) = (chars.next(), chars.next()) else { continue };
        if refs == spec {
            return Some((f, summary.unwrap_or("").to_string()));
        }
    }
    None
}

impl Git {
    /// The URL a push to `remote` goes to (`git remote get-url --push`:
    /// `pushurl` if set, else `url`, after `insteadOf` / `pushInsteadOf`
    /// rewriting). Local only: reads the repository's config, contacts no
    /// remote. `remote` is checked as for `push`; an unknown remote is
    /// `InvalidInput` `no-such-remote`. [实现时补的决定] (review-3 R4: the
    /// daemon records it with a kept push scene and settles the scene only
    /// on a push to the same destination.)
    pub fn push_url(&self, remote: &str) -> Result<String> {
        check_remote(remote)?;
        self.push_url_checked(remote)
    }

    fn push_url_checked(&self, remote: &str) -> Result<String> {
        let mut c = self.repo_command();
        c.args(["remote", "get-url", "--push", remote]);
        let out = self.run(c, Opts::c())?;
        if !out.ok() {
            if out.stderr_str().contains("No such remote") {
                return Err(errd(
                    Code::InvalidInput,
                    format!("no remote {remote}"),
                    json!({ "remote": remote, "reason": "no-such-remote" }),
                ));
            }
            return Err(failed("remote get-url", &out));
        }
        let url = String::from_utf8_lossy(&out.stdout).trim_end_matches(['\n', '\r']).to_string();
        if url.is_empty() {
            return Err(failed("remote get-url", &out));
        }
        Ok(url)
    }

    /// Steps 1–4 of `push`: refusals, the push URL, the push, and this
    /// ref's status line.
    fn push_send(
        &self,
        staging: &Path,
        remote: &str,
        src_ref: &str,
        dst_branch: &str,
        timeout: Duration,
    ) -> Result<Sent> {
        // Step 1: refusals; nothing has been sent.
        let gitfile = staging.join(".git");
        self.staging_gitdir(staging)?;
        check_strata_ref(src_ref)?;
        check_remote(remote)?;
        self.check_branch(dst_branch)?;
        let Some(pushed) = self.read_ref(src_ref)? else {
            return Err(errd(
                Code::NoSuchCommit,
                format!("{src_ref} does not exist"),
                json!({ "ref": src_ref }),
            ));
        };
        let tracking = remote_ref(remote, dst_branch);
        let before = self.read_ref(&tracking)?;

        // Step 2: the push URL.
        let url = self.push_url_checked(remote)?;

        // Step 3: the push itself, from the staging root, hooks live.
        self.inner.seams.hit(PUSH_BEFORE_SEND);
        let spec = format!("{src_ref}:refs/heads/{dst_branch}");
        let mut c = self.tree_command(&gitfile, staging, None);
        c.env("STRATA_PUBLISH", "1");
        c.args(["push", "--porcelain", "--no-follow-tags", "--end-of-options", &url, &spec]);
        let out = match self.run(c, Opts::default().timeout(timeout)) {
            Ok(o) => o,
            Err(e) if e.code == Code::Unknown && e.detail.as_ref().and_then(|d| d.get("reason")).and_then(|r| r.as_str()) == Some("timeout") => {
                tracing::warn!(remote, src_ref, dst_branch, ?timeout, "push timed out; outcome unknown");
                return Ok(Sent { outcome: PushOutcome::Unknown, tracking, before, pushed });
            }
            Err(e) => return Err(e),
        };
        tracing::info!(
            remote,
            src_ref,
            dst_branch,
            exit = ?out.status,
            stdout = %tail(&out.stdout, 4096),
            stderr = %tail(&out.stderr, 4096),
            "git push"
        );

        // Step 4: only this ref's status line decides.
        let outcome = match status_of(&out.stdout, &spec) {
            Some((' ' | '*' | '=', _)) => PushOutcome::Pushed,
            Some(('!', summary)) => {
                let mut msg = summary;
                for l in out.stderr_str().lines().filter(|l| l.starts_with("remote:")) {
                    msg.push('\n');
                    msg.push_str(l);
                }
                PushOutcome::Refused(msg)
            }
            _ => PushOutcome::Unknown,
        };

        Ok(Sent { outcome, tracking, before, pushed })
    }

    /// Push `src_ref` from the staging tree to `refs/heads/<dst_branch>` at
    /// `remote`'s push URL (§5.11).
    pub fn push(
        &self,
        staging: &Path,
        remote: &str,
        src_ref: &str,
        dst_branch: &str,
        timeout: Duration,
    ) -> Result<PushOutcome> {
        let Sent { outcome, tracking, before, pushed } = self.push_send(staging, remote, src_ref, dst_branch, timeout)?;

        // Step 5: strata's tracking ref, compare-and-swap against the value
        // read before the push; failure or skip changes nothing.
        if outcome == PushOutcome::Pushed
            && before.as_ref() != Some(&pushed)
            && let Err(e) = self.update_ref(&tracking, &pushed, before.as_ref())
        {
            tracing::info!(tracking, error = %e.message, "tracking ref left alone after push");
        }
        Ok(outcome)
    }

    /// `push` for publish (publish.md §4.6, §5.6): `Pushed` only after one
    /// ref transaction has moved both the tracking ref and `anchor_ref`
    /// (under `refs/strata/`) to the pushed commit, each compare-and-swap
    /// against its value read before the push. When that transaction does
    /// not commit, the result is `Unknown` (nothing local moved).
    pub fn push_published(
        &self,
        staging: &Path,
        remote: &str,
        src_ref: &str,
        dst_branch: &str,
        anchor_ref: &str,
        timeout: Duration,
    ) -> Result<PushOutcome> {
        self.push_recorded(staging, remote, src_ref, dst_branch, anchor_ref, &[], timeout)
    }

    /// `push_published` whose recording transaction also carries `verify`
    /// (extra compare-only updates, e.g. the publish chain still at the
    /// pushed commit): chain `--restore` records tracking and anchor in
    /// the same transaction that checks the chain (review M7). Any update
    /// that does not hold → `Unknown`, nothing local moved.
    #[allow(clippy::too_many_arguments)]
    pub fn push_recorded(
        &self,
        staging: &Path,
        remote: &str,
        src_ref: &str,
        dst_branch: &str,
        anchor_ref: &str,
        verify: &[RefUpdate],
        timeout: Duration,
    ) -> Result<PushOutcome> {
        check_strata_ref(anchor_ref)?;
        let anchor_before = self.read_ref(anchor_ref)?;
        let Sent { outcome, tracking, before, pushed } = self.push_send(staging, remote, src_ref, dst_branch, timeout)?;
        if outcome != PushOutcome::Pushed {
            return Ok(outcome);
        }
        self.inner.seams.hit(PUSH_BEFORE_RECORD);
        let mut updates = Vec::new();
        for (name, old) in [(tracking.as_str(), before), (anchor_ref, anchor_before)] {
            if old.as_ref() == Some(&pushed) {
                updates.push(RefUpdate::verify(name, old));
            } else {
                updates.push(RefUpdate::set(name, pushed.clone(), old));
            }
        }
        updates.extend(verify.iter().cloned());
        match self.ref_transaction(&updates) {
            Ok(()) => Ok(PushOutcome::Pushed),
            Err(e) => {
                tracing::warn!(tracking, anchor_ref, error = %e.message, "push sent but not recorded; outcome unknown");
                Ok(PushOutcome::Unknown)
            }
        }
    }
}

/// What steps 1–4 of a push saw.
struct Sent {
    outcome: PushOutcome,
    tracking: String,
    before: Option<CommitOid>,
    pushed: CommitOid,
}
