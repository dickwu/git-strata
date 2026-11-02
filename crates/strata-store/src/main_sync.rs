//! Main-sync persistence. Readers return empty results before schema v4.
use crate::codec::{from_json, int, nonneg, ns, opt_parse, parse, time, to_json};
use crate::errors::{NOT_WRITTEN, bad_row, invalid, sql, sql_read};
use crate::rm_state::{V4, user_version};
use crate::sql::{Connection, OptionalExtension, Row, params};
use crate::txn::Tx;
use crate::{Result, Store};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::SystemTime;
use strata_types::{BlobId, CommitOid, ProposalId, RepoName, RepoPath, SessionId, StrataError};

fn w(op: &'static str) -> impl Fn(crate::sql::Error) -> StrataError {
    move |e| sql(e, op, NOT_WRITTEN)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DepsAutoState {
    Installing,
    Ready,
    InstallFailed,
    Applied,
}
impl DepsAutoState {
    fn text(self) -> &'static str {
        match self {
            Self::Installing => "installing",
            Self::Ready => "ready",
            Self::InstallFailed => "install_failed",
            Self::Applied => "applied",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DepsAuto {
    pub proposal: ProposalId,
    pub state: DepsAutoState,
    pub error: Option<Value>,
    pub at: SystemTime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoFetch {
    /// Time of the last attempt, including a failed one.
    pub at: SystemTime,
    pub ok: bool,
    pub error: Option<String>,
    /// Time of the last successful fetch; failures never clear it.
    pub ok_at: Option<SystemTime>,
}

/// A persisted conflict with its resolution binding. `ours_blob` is a store
/// BLAKE3 id, not a Git oid; None means our side was absent. Hunk numbers are
/// kept verbatim, without the display preview's length limits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncReportRow {
    pub target: CommitOid,
    pub generation: u64,
    pub path: RepoPath,
    pub kind: String,
    pub ours_blob: Option<BlobId>,
    pub hunks: Value,
}

fn auto_row(r: &Row) -> crate::sql::SqlResult<Result<DepsAuto>> {
    let (proposal, state, error, at): (i64, String, Option<String>, i64) =
        (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
    Ok((|| {
        Ok(DepsAuto {
            proposal: ProposalId::try_from(nonneg(proposal, "deps_auto.proposal")?)
                .map_err(|_| bad_row("deps_auto.proposal"))?,
            state: match state.as_str() {
                "installing" => DepsAutoState::Installing,
                "ready" => DepsAutoState::Ready,
                "install_failed" => DepsAutoState::InstallFailed,
                "applied" => DepsAutoState::Applied,
                _ => return Err(bad_row(format!("deps_auto.state {state:?}"))),
            },
            error: error
                .map(|e| from_json(&e, "deps_auto.error_json"))
                .transpose()?,
            at: time(at),
        })
    })())
}

pub(crate) fn delete_report(
    c: &Connection,
    session: &str,
    repo: &str,
    op: &'static str,
) -> Result<()> {
    c.execute(
        "DELETE FROM sync_report WHERE session = ?1 AND repo = ?2",
        params![session, repo],
    )
    .map_err(w(op))?;
    Ok(())
}

impl Store {
    pub fn deps_auto(&self, proposal: ProposalId) -> Result<Option<DepsAuto>> {
        let id = int(proposal.get(), "proposal")?;
        self.read(|c| {
            if user_version(c)? < V4 {
                return Ok(None);
            }
            c.query_row(
                "SELECT proposal, state, error_json, at_ns FROM deps_auto WHERE proposal = ?1",
                [id],
                auto_row,
            )
            .optional()
            .map_err(sql_read)?
            .transpose()
        })
    }

    pub fn deps_auto_all(&self, repo: Option<&RepoName>) -> Result<Vec<DepsAuto>> {
        self.read(|c| {
            if user_version(c)? < V4 { return Ok(Vec::new()); }
            let mut q = c.prepare("SELECT a.proposal, a.state, a.error_json, a.at_ns FROM deps_auto a \
                JOIN deps_proposals p ON p.id = a.proposal WHERE ?1 IS NULL OR p.repo = ?1 ORDER BY a.proposal").map_err(sql_read)?;
            let rows = q.query_map([repo.map(RepoName::as_str)], auto_row).map_err(sql_read)?
                .collect::<crate::sql::SqlResult<Vec<_>>>().map_err(sql_read)?;
            rows.into_iter().collect()
        })
    }

    pub fn set_deps_auto(
        &self,
        proposal: ProposalId,
        state: DepsAutoState,
        error: Option<&Value>,
        at: SystemTime,
    ) -> Result<()> {
        const OP: &str = "set_deps_auto";
        let (id, at) = (int(proposal.get(), "proposal")?, ns(at, "at")?);
        let error = error.map(|e| to_json(e, "error")).transpose()?;
        self.write(OP, |c| {
            c.execute("INSERT INTO deps_auto (proposal, state, error_json, at_ns) VALUES (?1, ?2, ?3, ?4) \
                ON CONFLICT (proposal) DO UPDATE SET state = excluded.state, error_json = excluded.error_json, at_ns = excluded.at_ns",
                params![id, state.text(), error, at]).map_err(w(OP))?;
            Ok(Tx::Commit(()))
        })
    }

    pub fn delete_deps_auto(&self, proposal: ProposalId) -> Result<bool> {
        const OP: &str = "delete_deps_auto";
        let id = int(proposal.get(), "proposal")?;
        self.write(OP, |c| {
            let n = c
                .execute("DELETE FROM deps_auto WHERE proposal = ?1", [id])
                .map_err(w(OP))?;
            Ok(if n == 1 {
                Tx::Commit(true)
            } else {
                Tx::Rollback(false)
            })
        })
    }

    /// Atomically apply a Pending proposal and record the automatic decision.
    /// Like `decide_proposal(Applied)`, missing ids error and non-Pending ids
    /// return false without changing either record.
    pub fn auto_applied(&self, proposal: ProposalId, at: SystemTime) -> Result<bool> {
        const OP: &str = "auto_applied";
        let (id, at) = (int(proposal.get(), "proposal")?, ns(at, "at")?);
        self.write(OP, |c| {
            let state: Option<String> = c.query_row("SELECT state FROM deps_proposals WHERE id = ?1", [id], |r| r.get(0)).optional().map_err(w(OP))?;
            if state.is_none() { return Err(crate::deps::no_such_proposal(proposal)); }
            if state.as_deref() != Some("pending") { return Ok(Tx::Rollback(false)); }
            c.execute("UPDATE deps_proposals SET state = 'applied', updated_ns = ?2 WHERE id = ?1 AND state = 'pending'", params![id, at]).map_err(w(OP))?;
            c.execute("INSERT INTO deps_auto (proposal, state, error_json, at_ns) VALUES (?1, 'applied', NULL, ?2) \
                ON CONFLICT (proposal) DO UPDATE SET state = 'applied', error_json = NULL, at_ns = excluded.at_ns", params![id, at]).map_err(w(OP))?;
            Ok(Tx::Commit(true))
        })
    }

    pub fn record_fetch(
        &self,
        repo: &RepoName,
        ok: bool,
        error: Option<&str>,
        at: SystemTime,
    ) -> Result<()> {
        const OP: &str = "record_fetch";
        let at = ns(at, "at")?;
        self.write(OP, |c| {
            c.execute("INSERT INTO repo_fetch (repo, at_ns, ok, error, ok_at_ns) VALUES (?1, ?2, ?3, ?4, CASE WHEN ?3 THEN ?2 ELSE NULL END) \
                ON CONFLICT (repo) DO UPDATE SET at_ns = excluded.at_ns, ok = excluded.ok, error = excluded.error, \
                ok_at_ns = CASE WHEN excluded.ok THEN excluded.at_ns ELSE repo_fetch.ok_at_ns END", params![repo.as_str(), at, ok, error]).map_err(w(OP))?;
            Ok(Tx::Commit(()))
        })
    }

    pub fn repo_fetch(&self, repo: &RepoName) -> Result<Option<RepoFetch>> {
        self.read(|c| {
            if user_version(c)? < V4 {
                return Ok(None);
            }
            c.query_row(
                "SELECT at_ns, ok, error, ok_at_ns FROM repo_fetch WHERE repo = ?1",
                [repo.as_str()],
                |r| {
                    let (at, ok, error, ok_at): (i64, i64, Option<String>, Option<i64>) =
                        (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
                    Ok(RepoFetch {
                        at: time(at),
                        ok: ok != 0,
                        error,
                        ok_at: ok_at.map(time),
                    })
                },
            )
            .optional()
            .map_err(sql_read)
        })
    }

    /// Replace a pair's complete report in one transaction. Each row must
    /// have the same target/generation binding as the report.
    pub fn put_sync_report(
        &self,
        session: &SessionId,
        repo: &RepoName,
        target: &CommitOid,
        generation: u64,
        rows: &[SyncReportRow],
    ) -> Result<()> {
        const OP: &str = "put_sync_report";
        if rows
            .iter()
            .any(|r| r.target != *target || r.generation != generation)
        {
            return Err(invalid(
                "report_binding",
                "sync report rows must match the target and generation",
            ));
        }
        let generation = int(generation, "generation")?;
        let bodies: Vec<String> = rows
            .iter()
            .map(|r| to_json(&r.hunks, "hunks"))
            .collect::<Result<_>>()?;
        self.writable()?;
        // Like record_checkpoint, pin the physical blobs under L3 before
        // registering references under L2. put_bytes alone creates no blob row.
        let _gate = self.gate_shared();
        let mut sizes = std::collections::BTreeMap::new();
        for blob in rows.iter().filter_map(|r| r.ours_blob.as_ref()) {
            if sizes.contains_key(blob.as_str()) {
                continue;
            }
            let path = self.blob_path(blob);
            let md = std::fs::symlink_metadata(&path).map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    crate::errors::err(
                        strata_types::Code::Corrupt,
                        "sync report blob is missing",
                        serde_json::json!({"reason":"blob_missing", "blob":blob}),
                    )
                } else {
                    crate::errors::io(&e, strata_apfs::IoSide::Read, &path, OP, NOT_WRITTEN)
                }
            })?;
            if !md.file_type().is_file() {
                return Err(crate::errors::corrupt(
                    "blob_type",
                    "sync report blob is not a regular file",
                ));
            }
            sizes.insert(blob.as_str(), int(md.len(), "blob_size")?);
        }
        let created = ns(SystemTime::now(), "now")?;
        self.write(OP, |c| {
            if crate::sessions::session_lifecycle(c, session.as_str()).map_err(w(OP))?.as_deref().is_none_or(|s| s == "removed") {
                return Err(crate::errors::no_such_session(session.as_str()));
            }
            for (blob, size) in &sizes {
                c.execute("INSERT INTO blobs (id, size, created_ns) VALUES (?1, ?2, ?3) ON CONFLICT (id) DO NOTHING",
                    params![blob, size, created]).map_err(w(OP))?;
                let registered: i64 = c.query_row("SELECT size FROM blobs WHERE id = ?1", [blob], |r| r.get(0)).map_err(w(OP))?;
                if registered != *size { return Err(crate::errors::corrupt("blob_size", "sync report blob size differs from its registration")); }
            }
            delete_report(c, session.as_str(), repo.as_str(), OP)?;
            for (r, hunks) in rows.iter().zip(&bodies) {
                c.execute("INSERT INTO sync_report (session, repo, target, generation, path, kind, ours_blob, hunks_json) \
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)", params![session.as_str(), repo.as_str(), target.as_str(), generation,
                    r.path.as_str(), r.kind, r.ours_blob.as_ref().map(BlobId::as_str), hunks]).map_err(w(OP))?;
            }
            Ok(Tx::Commit(()))
        })
    }

    pub fn sync_report(&self, session: &SessionId, repo: &RepoName) -> Result<Vec<SyncReportRow>> {
        self.read(|c| {
            if user_version(c)? < V4 {
                return Ok(Vec::new());
            }
            let mut q = c
                .prepare(
                    "SELECT target, generation, path, kind, ours_blob, hunks_json FROM sync_report \
                WHERE session = ?1 AND repo = ?2 ORDER BY path",
                )
                .map_err(sql_read)?;
            let rows = q
                .query_map(params![session.as_str(), repo.as_str()], |r| {
                    let (target, generation, path, kind, ours_blob, hunks): (
                        String,
                        i64,
                        String,
                        String,
                        Option<String>,
                        String,
                    ) = (
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    );
                    Ok((|| {
                        Ok(SyncReportRow {
                            target: parse(target, "sync_report.target")?,
                            generation: nonneg(generation, "sync_report.generation")?,
                            path: parse(path, "sync_report.path")?,
                            kind,
                            ours_blob: opt_parse(ours_blob, "sync_report.ours_blob")?,
                            hunks: from_json(&hunks, "sync_report.hunks_json")?,
                        })
                    })())
                })
                .map_err(sql_read)?
                .collect::<crate::sql::SqlResult<Vec<Result<SyncReportRow>>>>()
                .map_err(sql_read)?;
            rows.into_iter().collect()
        })
    }

    pub fn delete_sync_report(&self, session: &SessionId, repo: &RepoName) -> Result<()> {
        self.write("delete_sync_report", |c| {
            delete_report(c, session.as_str(), repo.as_str(), "delete_sync_report")?;
            Ok(Tx::Commit(()))
        })
    }
}
