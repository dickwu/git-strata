//! rm-followups §1.4 G4 (r8): the durable activity baseline of an `Auto`
//! move, written in `begin_move`'s transaction and kept through Done. Its
//! late-write notice and its clean passes live in the same row. A row
//! exists only for `Auto` moves: `Explicit` moves keep today's rules.

use crate::sql::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use strata_types::{RepoName, SessionId, StrataError};

use crate::codec::to_json;
use crate::errors::{NOT_WRITTEN, sql, sql_read};
use crate::rm_state::{V3, user_version};
use crate::txn::Tx;
use crate::{Result, Store};

fn w(op: &'static str) -> impl Fn(crate::sql::Error) -> StrataError {
    move |e| sql(e, op, NOT_WRITTEN)
}

/// G2's snapshot (§1.4): the newest ctime `m0_ns` and the walk's start
/// `t0_ns` (ns since the epoch), the root's entry names and the entry count.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveBaseline {
    pub m0_ns: i64,
    pub t0_ns: i64,
    pub root_names: Vec<String>,
    pub entries: u64,
}

/// One `move_activity` row. `baseline` is `None` when the stored baseline
/// cannot be read (the caller fails closed: the tree is kept).
#[derive(Clone, Debug, PartialEq)]
pub struct MoveActivity {
    pub session: SessionId,
    pub repo: RepoName,
    pub n: u64,
    pub baseline: Option<MoveBaseline>,
    /// The late-write notice (`{moved_at, paths, count, reason}`), if any.
    pub late_writes: Option<Value>,
    /// Clean passes of the old-tree check (ns since the epoch, newest last).
    pub passes: Vec<i64>,
}

type Raw = (String, String, i64, i64, i64, String, i64, Option<String>, Option<String>);

fn of_raw(r: Raw) -> Result<MoveActivity> {
    let (s, repo, n, m0, t0, names, entries, late, passes) = r;
    let root_names: Option<Vec<String>> = serde_json::from_str(&names).ok();
    let baseline = match (root_names, u64::try_from(entries)) {
        (Some(root_names), Ok(entries)) => Some(MoveBaseline { m0_ns: m0, t0_ns: t0, root_names, entries }),
        _ => None,
    };
    let passes = match passes {
        None => Vec::new(),
        // An unreadable pass list counts as no pass (stricter).
        Some(p) => serde_json::from_str::<Vec<i64>>(&p).unwrap_or_default(),
    };
    Ok(MoveActivity {
        session: crate::codec::parse(s, "move_activity.session")?,
        repo: crate::codec::parse(repo, "move_activity.repo")?,
        n: crate::codec::nonneg(n, "move_activity.n")?,
        baseline,
        late_writes: late.and_then(|l| serde_json::from_str(&l).ok()),
        passes,
    })
}

const COLS: &str = "session, repo, n, m0_ns, t0_ns, root_names_json, entries, late_writes_json, passes_json";

fn raw(r: &crate::sql::Row) -> crate::sql::SqlResult<Raw> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?))
}

/// Inserts the row of move `n` (inside `begin_move`'s transaction).
pub(crate) fn insert(c: &crate::sql::Connection, id: &str, repo: &str, n: i64, b: &MoveBaseline, op: &'static str) -> Result<()> {
    let names = to_json(&b.root_names, "root_names")?;
    let entries = i64::try_from(b.entries).unwrap_or(i64::MAX);
    c.execute(
        "INSERT INTO move_activity (session, repo, n, policy, m0_ns, t0_ns, root_names_json, entries) \
         VALUES (?1, ?2, ?3, 'auto', ?4, ?5, ?6, ?7)",
        params![id, repo, n, b.m0_ns, b.t0_ns, names, entries],
    )
    .map_err(w(op))?;
    Ok(())
}

impl Store {
    /// The row of move `n` of `session`/`repo`, if it was an `Auto` move
    /// (none on a store older than v3).
    pub fn move_activity(&self, session: &SessionId, repo: &RepoName, n: u64) -> Result<Option<MoveActivity>> {
        let ni = i64::try_from(n).unwrap_or(i64::MAX);
        self.read(|c| {
            if user_version(c)? < V3 {
                return Ok(None);
            }
            let r = c
                .query_row(
                    &format!("SELECT {COLS} FROM move_activity WHERE session = ?1 AND repo = ?2 AND n = ?3"),
                    params![session.as_str(), repo.as_str(), ni],
                    raw,
                )
                .optional()
                .map_err(sql_read)?;
            r.map(of_raw).transpose()
        })
    }

    /// Every row of `session` (status: the late-write notices), by repo, n.
    pub fn move_activities(&self, session: &SessionId) -> Result<Vec<MoveActivity>> {
        self.read(|c| {
            if user_version(c)? < V3 {
                return Ok(Vec::new());
            }
            let mut st = c
                .prepare(&format!("SELECT {COLS} FROM move_activity WHERE session = ?1 ORDER BY repo, n"))
                .map_err(sql_read)?;
            let rows = st
                .query_map([session.as_str()], raw)
                .map_err(sql_read)?
                .collect::<crate::sql::SqlResult<Vec<_>>>()
                .map_err(sql_read)?;
            rows.into_iter().map(of_raw).collect()
        })
    }

    /// Sets the late-write notice of move `n` unless one is already there
    /// (the first notice stands). `false` when there is no row.
    pub fn set_move_late_writes(&self, session: &SessionId, repo: &RepoName, n: u64, notice: &Value) -> Result<bool> {
        const OP: &str = "set_move_late_writes";
        let body = to_json(notice, "late_writes")?;
        let ni = i64::try_from(n).unwrap_or(i64::MAX);
        self.write(OP, |c| {
            if user_version(c)? < V3 {
                return Ok(Tx::Rollback(false));
            }
            let k = c
                .execute(
                    "UPDATE move_activity SET late_writes_json = ?4 \
                     WHERE session = ?1 AND repo = ?2 AND n = ?3 AND late_writes_json IS NULL",
                    params![session.as_str(), repo.as_str(), ni, body],
                )
                .map_err(w(OP))?;
            Ok(if k == 1 { Tx::Commit(true) } else { Tx::Rollback(false) })
        })
    }

    /// Replaces the clean-pass list of move `n`. `false` when there is no row.
    pub fn set_move_passes(&self, session: &SessionId, repo: &RepoName, n: u64, passes: &[i64]) -> Result<bool> {
        const OP: &str = "set_move_passes";
        let body = to_json(&passes, "passes")?;
        let ni = i64::try_from(n).unwrap_or(i64::MAX);
        self.write(OP, |c| {
            if user_version(c)? < V3 {
                return Ok(Tx::Rollback(false));
            }
            let k = c
                .execute(
                    "UPDATE move_activity SET passes_json = ?4 WHERE session = ?1 AND repo = ?2 AND n = ?3",
                    params![session.as_str(), repo.as_str(), ni, body],
                )
                .map_err(w(OP))?;
            Ok(if k == 1 { Tx::Commit(true) } else { Tx::Rollback(false) })
        })
    }
}

#[cfg(feature = "test-seams")]
impl Store {
    /// Test only: damage move `n`'s persisted baseline (its root names stop
    /// parsing), as store damage would.
    pub fn damage_move_activity_for_test(&self, session: &SessionId, repo: &RepoName, n: u64) -> Result<bool> {
        const OP: &str = "damage_move_activity";
        let ni = i64::try_from(n).unwrap_or(i64::MAX);
        self.write(OP, |c| {
            let k = c
                .execute(
                    "UPDATE move_activity SET root_names_json = '[1, 2]' WHERE session = ?1 AND repo = ?2 AND n = ?3",
                    params![session.as_str(), repo.as_str(), ni],
                )
                .map_err(w(OP))?;
            Ok(if k == 1 { Tx::Commit(true) } else { Tx::Rollback(false) })
        })
    }
}
