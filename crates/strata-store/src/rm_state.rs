//! Schema v3 (rm-followups §1.11, §2.2 R2): the last `rm` outcome of a
//! session and the deferred post-publish rebases. Every reader checks
//! `user_version >= 3` first: a new CLI on a store an old daemon still
//! serves (v2) sees no rows, not an error. Writers run on a migrated
//! (latest) handle only.

use std::time::SystemTime;

use crate::sql::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use strata_types::{Code, RepoName, SessionId, StrataError};

use crate::codec::{ns, nonneg, parse, time, to_json};
use crate::errors::{NOT_WRITTEN, bad_row, err, sql, sql_read};
use crate::sessions::session_lifecycle;
use crate::txn::Tx;
use crate::{Result, Store};

/// The first schema version with `rm_attempts`, `rebase_pending` and
/// `move_activity`.
pub(crate) const V3: i64 = 3;

fn w(op: &'static str) -> impl Fn(crate::sql::Error) -> StrataError {
    move |e| sql(e, op, NOT_WRITTEN)
}

pub(crate) fn user_version(c: &Connection) -> Result<i64> {
    c.query_row("PRAGMA user_version", [], |r| r.get(0)).map_err(sql_read)
}

/// How an `rm` request that did not remove its session ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RmOutcome {
    /// Refused before anything changed (Dirty, OpenFiles, changed while
    /// being removed, …): the session is as it was.
    Refused,
    /// Failed after teardown began: the session is Unknown with a scene.
    Failed,
}

/// The last `rm` of a session that did not end in `Removed`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RmAttempt {
    pub session: SessionId,
    #[serde(with = "strata_types::serde_rfc3339")]
    pub at: SystemTime,
    pub outcome: RmOutcome,
    /// `{code, message, detail}` of the error the caller got.
    pub detail: Value,
}

/// One deferred post-publish rebase (design §1.3). Written from phase C
/// on; read here so status shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RebasePending {
    pub session: SessionId,
    pub repo: RepoName,
    pub rev: u64,
    pub from_tree: String,
    pub from_deps: String,
    /// The committed (attemptable) target.
    pub head: Option<String>,
    pub tree: Option<String>,
    /// A replay's planned target, adopted or cleared under the repo lock.
    pub planned_head: Option<String>,
    pub planned_tree: Option<String>,
    /// A publish whose commit is not known yet.
    pub planned_publish: Option<u64>,
    pub reason: String,
    pub detail: Option<Value>,
    #[serde(with = "strata_types::serde_rfc3339")]
    pub first_at: SystemTime,
    #[serde(with = "strata_types::serde_rfc3339")]
    pub last_at: SystemTime,
    pub attempts: u64,
}

fn outcome_text(o: RmOutcome) -> &'static str {
    match o {
        RmOutcome::Refused => "refused",
        RmOutcome::Failed => "failed",
    }
}

fn outcome_of(s: &str) -> Result<RmOutcome> {
    match s {
        "refused" => Ok(RmOutcome::Refused),
        "failed" => Ok(RmOutcome::Failed),
        other => Err(bad_row(format!("rm_attempts.outcome {other:?}"))),
    }
}

impl Store {
    /// Record how the last `rm` of `session` ended (it did not remove the
    /// session). One row per session: a later attempt replaces it. The
    /// caller holds the session's queue place, so attempts are ordered.
    /// A session already Removed (or unknown to the store) records
    /// nothing: `Removed` deletes the row in its own transaction.
    pub fn record_rm_attempt(&self, a: &RmAttempt) -> Result<bool> {
        const OP: &str = "record_rm_attempt";
        self.writable()?;
        if !a.detail.is_object() {
            return Err(err(Code::InvalidInput, "an rm attempt's detail is an object", json!({ "reason": "detail" })));
        }
        let detail = to_json(&a.detail, "detail")?;
        let at = ns(a.at, "at")?;
        let id = a.session.as_str();
        self.write(OP, |c| {
            match session_lifecycle(c, id).map_err(w(OP))?.as_deref() {
                None | Some("removed") => return Ok(Tx::Rollback(false)),
                Some(_) => {}
            }
            c.execute(
                "INSERT INTO rm_attempts (session, at_ns, outcome, detail_json) VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT (session) DO UPDATE SET at_ns = excluded.at_ns, outcome = excluded.outcome, \
                 detail_json = excluded.detail_json",
                params![id, at, outcome_text(a.outcome), detail],
            )
            .map_err(w(OP))?;
            Ok(Tx::Commit(true))
        })
    }

    /// The recorded last `rm` outcomes (of `session`, if given), by session.
    /// Empty on a store older than v3.
    pub fn rm_attempts(&self, session: Option<&SessionId>) -> Result<Vec<RmAttempt>> {
        self.read(|c| {
            if user_version(c)? < V3 {
                return Ok(Vec::new());
            }
            let mut st = c
                .prepare(
                    "SELECT session, at_ns, outcome, detail_json FROM rm_attempts \
                     WHERE ?1 IS NULL OR session = ?1 ORDER BY session",
                )
                .map_err(sql_read)?;
            let rows = st
                .query_map([session.map(SessionId::as_str)], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?))
                })
                .map_err(sql_read)?
                .collect::<crate::sql::SqlResult<Vec<_>>>()
                .map_err(sql_read)?;
            let mut out = Vec::with_capacity(rows.len());
            for (s, at, o, d) in rows {
                out.push(RmAttempt {
                    session: parse(s, "rm_attempts.session")?,
                    at: time(at),
                    outcome: outcome_of(&o)?,
                    detail: serde_json::from_str(&d).map_err(|e| bad_row(format!("rm_attempts.detail_json: {e}")))?,
                });
            }
            Ok(out)
        })
    }

    /// The deferred rebases (of `session`, if given), by session then repo.
    /// Empty on a store older than v3.
    pub fn rebase_pending(&self, session: Option<&SessionId>) -> Result<Vec<RebasePending>> {
        self.read(|c| read_rebase_pending(c, session))
    }
}

type PendingRow = (
    String,
    String,
    i64,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
    String,
    Option<String>,
    i64,
    i64,
    i64,
);

pub(crate) fn read_rebase_pending(c: &Connection, session: Option<&SessionId>) -> Result<Vec<RebasePending>> {
    if user_version(c)? < V3 {
        return Ok(Vec::new());
    }
    let mut st = c
        .prepare(
            "SELECT session, repo, rev, from_tree, from_deps, head, tree, planned_head, planned_tree, planned_publish, \
                    reason, detail_json, first_at_ns, last_at_ns, attempts \
             FROM rebase_pending WHERE ?1 IS NULL OR session = ?1 ORDER BY session, repo",
        )
        .map_err(sql_read)?;
    let rows: Vec<PendingRow> = st
        .query_map([session.map(SessionId::as_str)], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
                r.get(10)?,
                r.get(11)?,
                r.get(12)?,
                r.get(13)?,
                r.get(14)?,
            ))
        })
        .map_err(sql_read)?
        .collect::<crate::sql::SqlResult<Vec<_>>>()
        .map_err(sql_read)?;
    let mut out = Vec::with_capacity(rows.len());
    for (s, repo, rev, ft, fd, head, tree, ph, pt, pp, reason, detail, first, last, attempts) in rows {
        out.push(RebasePending {
            session: parse(s, "rebase_pending.session")?,
            repo: parse(repo, "rebase_pending.repo")?,
            rev: nonneg(rev, "rebase_pending.rev")?,
            from_tree: ft,
            from_deps: fd,
            head,
            tree,
            planned_head: ph,
            planned_tree: pt,
            planned_publish: pp.map(|p| nonneg(p, "rebase_pending.planned_publish")).transpose()?,
            reason,
            detail: detail
                .map(|d| serde_json::from_str(&d).map_err(|e| bad_row(format!("rebase_pending.detail_json: {e}"))))
                .transpose()?,
            first_at: time(first),
            last_at: time(last),
            attempts: nonneg(attempts, "rebase_pending.attempts")?,
        });
    }
    Ok(out)
}
