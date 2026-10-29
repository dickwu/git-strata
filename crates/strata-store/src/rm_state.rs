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

// ---- rebase_pending writers (design §1.6) ---------------------------------

/// One replay retarget or enrollment, written as a planned target before
/// the replay's ref transaction (§1.6 "Replay retarget and enrollment").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayPlan {
    pub session: SessionId,
    pub planned_head: String,
    pub planned_tree: String,
}

fn wp(op: &'static str) -> impl Fn(crate::sql::Error) -> StrataError {
    move |e| sql(e, op, NOT_WRITTEN)
}

/// The workspace's current `(base tree, base deps)`; `None` when the pair
/// has no workspace.
fn ws_base(c: &Connection, session: &str, repo: &str) -> crate::sql::SqlResult<Option<(String, String)>> {
    use crate::sql::OptionalExtension;
    c.query_row(
        "SELECT g.tree, g.deps FROM workspaces w JOIN base_gens g ON g.repo = w.repo AND g.id = w.base_gen \
         WHERE w.session = ?1 AND w.repo = ?2",
        params![session, repo],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
    )
    .optional()
}

fn row_of(c: &Connection, session: &SessionId, repo: &RepoName) -> Result<Option<RebasePending>> {
    Ok(read_rebase_pending(c, Some(session))?.into_iter().find(|p| p.repo == *repo))
}

/// A row with no committed target, no planned replay target and no
/// planned publish carries nothing: it is deleted.
fn drop_if_empty(c: &Connection, session: &str, repo: &str, op: &'static str) -> Result<()> {
    c.execute(
        "DELETE FROM rebase_pending WHERE session = ?1 AND repo = ?2 \
         AND head IS NULL AND planned_head IS NULL AND planned_publish IS NULL",
        params![session, repo],
    )
    .map_err(wp(op))?;
    Ok(())
}

fn rev_i(rev: u64) -> Result<i64> {
    i64::try_from(rev).map_err(|_| err(Code::InvalidInput, "rev is too large", json!({ "reason": "rev" })))
}

/// Rule 1 of §1.6, inside `record_publish`'s transaction: every workspace
/// of the group gets `planned_publish = id` (the row is inserted
/// when absent; other targets are left as they are).
pub(crate) fn plan_publish(c: &Connection, op: &'static str, id: i64, pairs: &[(&str, &str)], at_ns: i64) -> Result<()> {
    if user_version(c)? < V3 {
        return Ok(());
    }
    for (session, repo) in pairs {
        let Some((tree, deps)) = ws_base(c, session, repo).map_err(wp(op))? else { continue };
        let n = c
            .execute("UPDATE rebase_pending SET planned_publish = ?3 WHERE session = ?1 AND repo = ?2", params![session, repo, id])
            .map_err(wp(op))?;
        if n == 0 {
            c.execute(
                "INSERT INTO rebase_pending (session, repo, rev, from_tree, from_deps, planned_publish, reason, \
                 first_at_ns, last_at_ns, attempts) VALUES (?1, ?2, 0, ?3, ?4, ?5, 'planned', ?6, ?6, 0)",
                params![session, repo, tree, deps, id, at_ns],
            )
            .map_err(wp(op))?;
        }
    }
    Ok(())
}

/// §1.6 rule 6, in `finish_move`'s transaction: the move to `(to_tree,
/// to_deps)` at `head` consumes the row's committed target when it was the
/// attempted `rev` (or, with no `rev`, when the move landed exactly on that
/// target); a newer target stays with its `from_*` rewritten to the new
/// base. Any other row is left as it is (it is stale and dropped later).
pub(crate) fn consume_on_move(
    c: &Connection,
    op: &'static str,
    session: &str,
    repo: &str,
    to: (&str, &str),
    head: &str,
    consume: Option<u64>,
) -> Result<()> {
    if user_version(c)? < V3 {
        return Ok(());
    }
    let (Ok(sid), Ok(rn)) = (SessionId::try_from(session.to_string()), RepoName::try_from(repo.to_string())) else {
        return Ok(());
    };
    let Some(row) = row_of(c, &sid, &rn)? else { return Ok(()) };
    let attempted = match consume {
        Some(r) => row.rev == r,
        None => row.head.as_deref() == Some(head),
    };
    let newer = consume.is_some() && !attempted;
    if attempted {
        c.execute(
            "UPDATE rebase_pending SET head = NULL, tree = NULL, from_tree = ?3, from_deps = ?4 WHERE session = ?1 AND repo = ?2",
            params![session, repo, to.0, to.1],
        )
        .map_err(wp(op))?;
    } else if newer {
        c.execute(
            "UPDATE rebase_pending SET from_tree = ?3, from_deps = ?4 WHERE session = ?1 AND repo = ?2",
            params![session, repo, to.0, to.1],
        )
        .map_err(wp(op))?;
    }
    drop_if_empty(c, session, repo, op)
}

impl Store {
    /// The row of one pair, if any (empty on a store older than v3).
    pub fn rebase_pending_of(&self, session: &SessionId, repo: &RepoName) -> Result<Option<RebasePending>> {
        self.read(|c| row_of(c, session, repo))
    }

    /// §1.6 rule 2/3: publish `publish` landed `target` (its commit and
    /// tree). Conditioned on the row's `rev` being `rev` (what the caller
    /// read under the repo lock): clears `planned_publish` when it is still
    /// `publish`, and when `set` sets the committed target (`rev + 1`,
    /// `reason`, attempts 0, `from_*` = the workspace's current base,
    /// `detail.publish`). `false` when the row is gone or moved on.
    #[allow(clippy::too_many_arguments)]
    pub fn promote_pending(
        &self,
        session: &SessionId,
        repo: &RepoName,
        rev: u64,
        publish: u64,
        target: Option<(&str, &str)>,
        reason: &str,
        at: SystemTime,
    ) -> Result<bool> {
        const OP: &str = "promote_pending";
        let at = ns(at, "at")?;
        let (r, p) = (rev_i(rev)?, rev_i(publish)?);
        self.write(OP, |c| {
            let Some(row) = row_of(c, session, repo)? else { return Ok(Tx::Rollback(false)) };
            if row.rev != rev {
                return Ok(Tx::Rollback(false));
            }
            let (s, rn) = (session.as_str(), repo.as_str());
            c.execute(
                "UPDATE rebase_pending SET planned_publish = NULL WHERE session = ?1 AND repo = ?2 AND planned_publish = ?3",
                params![s, rn, p],
            )
            .map_err(wp(OP))?;
            if let Some((head, tree)) = target {
                let Some((ft, fd)) = ws_base(c, s, rn).map_err(wp(OP))? else { return Ok(Tx::Rollback(false)) };
                let detail = to_json(&json!({ "publish": publish }), "detail")?;
                c.execute(
                    "UPDATE rebase_pending SET head = ?3, tree = ?4, rev = ?5 + 1, reason = ?6, detail_json = ?7, \
                     attempts = 0, last_at_ns = ?8, from_tree = ?9, from_deps = ?10 WHERE session = ?1 AND repo = ?2",
                    params![s, rn, head, tree, r, reason, detail, at, ft, fd],
                )
                .map_err(wp(OP))?;
            }
            drop_if_empty(c, s, rn, OP)?;
            Ok(Tx::Commit(true))
        })
    }

    /// §1.6 rule 3: a publish that is not live and has no reachable commit
    /// loses its plan; other targets are untouched. `false` when the row's
    /// `planned_publish` is not `publish`.
    pub fn clear_planned_publish(&self, session: &SessionId, repo: &RepoName, publish: u64) -> Result<bool> {
        const OP: &str = "clear_planned_publish";
        let p = rev_i(publish)?;
        self.write(OP, |c| {
            let (s, rn) = (session.as_str(), repo.as_str());
            if user_version(c)? < V3 {
                return Ok(Tx::Rollback(false));
            }
            let n = c
                .execute(
                    "UPDATE rebase_pending SET planned_publish = NULL WHERE session = ?1 AND repo = ?2 AND planned_publish = ?3",
                    params![s, rn, p],
                )
                .map_err(wp(OP))?;
            drop_if_empty(c, s, rn, OP)?;
            Ok(if n == 1 { Tx::Commit(true) } else { Tx::Rollback(false) })
        })
    }

    /// Drop the committed target of rev `rev` (stale, §1.6 "Staleness", or
    /// unreachable at a promotion): `rev + 1`, `reason`, `detail`; the row
    /// is deleted when nothing else is planned. `false` when `rev` moved.
    pub fn discard_pending_target(&self, session: &SessionId, repo: &RepoName, rev: u64, reason: &str, detail: &Value, at: SystemTime) -> Result<bool> {
        const OP: &str = "discard_pending_target";
        let at = ns(at, "at")?;
        let r = rev_i(rev)?;
        let detail = to_json(detail, "detail")?;
        self.write(OP, |c| {
            let (s, rn) = (session.as_str(), repo.as_str());
            if user_version(c)? < V3 {
                return Ok(Tx::Rollback(false));
            }
            let n = c
                .execute(
                    "UPDATE rebase_pending SET head = NULL, tree = NULL, rev = rev + 1, reason = ?4, detail_json = ?5, \
                     last_at_ns = ?6 WHERE session = ?1 AND repo = ?2 AND rev = ?3",
                    params![s, rn, r, reason, detail, at],
                )
                .map_err(wp(OP))?;
            drop_if_empty(c, s, rn, OP)?;
            Ok(if n == 1 { Tx::Commit(true) } else { Tx::Rollback(false) })
        })
    }

    /// §1.6 rule 6: an attempt of rev `rev` was deferred. Only an update of
    /// that rev (never an insert): `reason`, `detail`, `last_at`,
    /// `attempts + 1`. `false` when the row moved on meanwhile.
    pub fn defer_pending(&self, session: &SessionId, repo: &RepoName, rev: u64, reason: &str, detail: &Value, at: SystemTime) -> Result<bool> {
        const OP: &str = "defer_pending";
        let at = ns(at, "at")?;
        let r = rev_i(rev)?;
        let detail = to_json(detail, "detail")?;
        self.write(OP, |c| {
            if user_version(c)? < V3 {
                return Ok(Tx::Rollback(false));
            }
            let n = c
                .execute(
                    "UPDATE rebase_pending SET reason = ?4, detail_json = ?5, last_at_ns = ?6, attempts = attempts + 1 \
                     WHERE session = ?1 AND repo = ?2 AND rev = ?3 AND head IS NOT NULL",
                    params![session.as_str(), repo.as_str(), r, reason, detail, at],
                )
                .map_err(wp(OP))?;
            Ok(if n == 1 { Tx::Commit(true) } else { Tx::Rollback(false) })
        })
    }

    /// The committed target of rev `rev` keeps its place but is marked
    /// `reason` (rule 5: `publish_unfinished`). `false` when `rev` moved.
    pub fn set_pending_reason(&self, session: &SessionId, repo: &RepoName, rev: u64, reason: &str) -> Result<bool> {
        const OP: &str = "set_pending_reason";
        let r = rev_i(rev)?;
        self.write(OP, |c| {
            if user_version(c)? < V3 {
                return Ok(Tx::Rollback(false));
            }
            let n = c
                .execute(
                    "UPDATE rebase_pending SET reason = ?4 WHERE session = ?1 AND repo = ?2 AND rev = ?3 AND head IS NOT NULL",
                    params![session.as_str(), repo.as_str(), r, reason],
                )
                .map_err(wp(OP))?;
            Ok(if n == 1 { Tx::Commit(true) } else { Tx::Rollback(false) })
        })
    }

    /// §1.6 replay, before the ref transaction: every plan in one
    /// transaction (`planned_head/tree`; a pair with no row is inserted,
    /// `from_*` = its current base). Pairs with no workspace are skipped.
    pub fn plan_replay(&self, repo: &RepoName, plans: &[ReplayPlan], at: SystemTime) -> Result<()> {
        const OP: &str = "plan_replay";
        let at = ns(at, "at")?;
        if plans.is_empty() {
            return Ok(());
        }
        self.write(OP, |c| {
            if user_version(c)? < V3 {
                return Ok(Tx::Rollback(()));
            }
            let rn = repo.as_str();
            for p in plans {
                let s = p.session.as_str();
                let Some((ft, fd)) = ws_base(c, s, rn).map_err(wp(OP))? else { continue };
                let n = c
                    .execute(
                        "UPDATE rebase_pending SET planned_head = ?3, planned_tree = ?4 WHERE session = ?1 AND repo = ?2",
                        params![s, rn, p.planned_head, p.planned_tree],
                    )
                    .map_err(wp(OP))?;
                if n == 0 {
                    c.execute(
                        "INSERT INTO rebase_pending (session, repo, rev, from_tree, from_deps, planned_head, planned_tree, \
                         reason, first_at_ns, last_at_ns, attempts) VALUES (?1, ?2, 0, ?3, ?4, ?5, ?6, 'replay', ?7, ?7, 0)",
                        params![s, rn, ft, fd, p.planned_head, p.planned_tree, at],
                    )
                    .map_err(wp(OP))?;
                }
            }
            Ok(Tx::Commit(()))
        })
    }

    /// The planned replay target `planned_head` becomes the committed
    /// target (`rev + 1`, reason `pending`, attempts 0, `from_*` = the
    /// current base), or, with `adopt = false`, is cleared. Conditioned on
    /// the row still planning `planned_head`.
    pub fn settle_planned(&self, session: &SessionId, repo: &RepoName, planned_head: &str, adopt: bool, at: SystemTime) -> Result<bool> {
        const OP: &str = "settle_planned";
        let at = ns(at, "at")?;
        self.write(OP, |c| {
            let (s, rn) = (session.as_str(), repo.as_str());
            if user_version(c)? < V3 {
                return Ok(Tx::Rollback(false));
            }
            let n = if adopt {
                let Some((ft, fd)) = ws_base(c, s, rn).map_err(wp(OP))? else { return Ok(Tx::Rollback(false)) };
                c.execute(
                    "UPDATE rebase_pending SET head = planned_head, tree = planned_tree, planned_head = NULL, planned_tree = NULL, \
                     rev = rev + 1, reason = 'pending', attempts = 0, last_at_ns = ?4, from_tree = ?5, from_deps = ?6 \
                     WHERE session = ?1 AND repo = ?2 AND planned_head = ?3",
                    params![s, rn, planned_head, at, ft, fd],
                )
                .map_err(wp(OP))?
            } else {
                c.execute(
                    "UPDATE rebase_pending SET planned_head = NULL, planned_tree = NULL WHERE session = ?1 AND repo = ?2 AND planned_head = ?3",
                    params![s, rn, planned_head],
                )
                .map_err(wp(OP))?
            };
            drop_if_empty(c, s, rn, OP)?;
            Ok(if n == 1 { Tx::Commit(true) } else { Tx::Rollback(false) })
        })
    }
}
