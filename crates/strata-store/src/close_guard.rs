//! The transaction that commits `Removing` for a strict close (auto-close
//! §3.2): nothing a writer holding only the repo lock can change is left
//! unread between the close's proof and its first visible write.

use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

use serde_json::{Value, json};
use strata_types::{RepoName, SessionId};

use crate::codec::{from_json, lifecycle_text};
use crate::errors::{NOT_WRITTEN, sql};
use crate::rm_state::{RebasePending, read_rebase_pending};
use crate::sessions::session_lifecycle;
use crate::sql::{Connection, params};
use crate::txn::Tx;
use crate::{Lifecycle, Result, Store};

/// The part of a `rebase_pending` row a strict close proved: every change of
/// target moves one of these.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingKey {
    pub repo: RepoName,
    pub rev: u64,
    pub head: Option<String>,
    pub planned_head: Option<String>,
    pub planned_publish: Option<u64>,
}

impl PendingKey {
    pub fn of(p: &RebasePending) -> PendingKey {
        PendingKey {
            repo: p.repo.clone(),
            rev: p.rev,
            head: p.head.clone(),
            planned_head: p.planned_head.clone(),
            planned_publish: p.planned_publish,
        }
    }
}

/// What [`Store::set_removing_if_unchanged`] found. Anything but `Removing`
/// rolled back: the lifecycle and every row stay as they were.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IfUnchanged {
    Removing,
    LifecycleMoved,
    PendingMoved,
    /// Clause 6's items, each `{repo, code, …}` in clause 6's shape.
    Reopened(Vec<Value>),
}

fn w(e: crate::sql::Error) -> strata_types::StrataError {
    sql(e, "set_removing_if_unchanged", NOT_WRITTEN)
}

/// (a) The tracked workspaces: their conflicts, and the repo's `pending`
/// proposal when it is not the workspace's deps set.
fn tracked_items(c: &Connection, id: &str, items: &mut Vec<Value>, seen: &mut BTreeSet<i64>) -> Result<()> {
    let mut st = c
        .prepare(
            "SELECT w.repo, g.deps, w.conflicts FROM workspaces w \
             JOIN base_gens g ON g.repo = w.repo AND g.id = w.base_gen \
             WHERE w.session = ?1 AND w.tracked = 1 ORDER BY w.repo",
        )
        .map_err(w)?;
    let rows: Vec<(String, String, String)> = st
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map_err(w)?
        .collect::<crate::sql::SqlResult<_>>()
        .map_err(w)?;
    for (repo, deps, conflicts) in rows {
        let paths: Vec<String> = from_json(&conflicts, "workspaces.conflicts")?;
        if !paths.is_empty() {
            items.push(json!({ "repo": repo, "code": "conflicts", "conflicts": paths }));
        }
        let mut q = c
            .prepare("SELECT id, to_set, state, origin_session FROM deps_proposals WHERE repo = ?1 AND state = 'pending'")
            .map_err(w)?;
        let open: Vec<(i64, String, String, Option<String>)> = q
            .query_map([repo.as_str()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .map_err(w)?
            .collect::<crate::sql::SqlResult<_>>()
            .map_err(w)?;
        for (pid, to_set, state, origin) in open {
            if to_set != deps && seen.insert(pid) {
                items.push(json!({
                    "repo": repo,
                    "code": "deps_pending",
                    "proposal": pid,
                    "state": state,
                    "own": origin.as_deref() == Some(id),
                    "tracked": true,
                }));
            }
        }
    }
    Ok(())
}

/// (b) The session's own proposals, joined to no required workspace: a draft
/// on an untracked sibling vanishes with the session like any other.
fn own_items(c: &Connection, id: &str, items: &mut Vec<Value>, seen: &mut BTreeSet<i64>) -> Result<()> {
    let mut st = c
        .prepare(
            "SELECT p.id, p.repo, p.state, w.tracked FROM deps_proposals p \
             LEFT JOIN workspaces w ON w.session = p.origin_session AND w.repo = p.repo \
             WHERE p.origin_session = ?1 AND p.state IN ('pending', 'awaiting_publish') ORDER BY p.id",
        )
        .map_err(w)?;
    let rows: Vec<(i64, String, String, Option<i64>)> = st
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .map_err(w)?
        .collect::<crate::sql::SqlResult<_>>()
        .map_err(w)?;
    for (pid, repo, state, tracked) in rows {
        if seen.insert(pid) {
            items.push(json!({
                "repo": repo,
                "code": "deps_pending",
                "proposal": pid,
                "state": state,
                "own": true,
                "tracked": tracked == Some(1),
            }));
        }
    }
    Ok(())
}

/// (d) A saved `sync` conflict report, one item per repo.
fn report_items(c: &Connection, id: &str, items: &mut Vec<Value>) -> Result<()> {
    let mut st = c
        .prepare("SELECT repo, path FROM sync_report WHERE session = ?1 ORDER BY repo, path")
        .map_err(w)?;
    let rows: Vec<(String, String)> = st
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(w)?
        .collect::<crate::sql::SqlResult<_>>()
        .map_err(w)?;
    let mut by_repo: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (repo, path) in rows {
        by_repo.entry(repo).or_default().push(path);
    }
    for (repo, paths) in by_repo {
        items.push(json!({ "repo": repo, "code": "conflicts", "conflicts": paths, "report": true }));
    }
    Ok(())
}

impl Store {
    /// `cur → Removing` in one transaction, only when (1) the lifecycle is
    /// still `cur`, (2) the session's `rebase_pending` rows are the ones the
    /// close proved and (3) clause 6's proposals and conflicts are still
    /// clear. Writers holding only the repo lock are ordered before or after
    /// this transaction: before, it refuses; after, each checks the lifecycle
    /// in its own transaction, so `Removed` deletes only what was proved.
    pub fn set_removing_if_unchanged(
        &self,
        id: &SessionId,
        cur: Lifecycle,
        keys: &[PendingKey],
        _at: SystemTime,
    ) -> Result<IfUnchanged> {
        const OP: &str = "set_removing_if_unchanged";
        self.writable()?;
        let sid = id.as_str();
        self.write(OP, |c| {
            // 1.
            let lifecycle = session_lifecycle(c, sid).map_err(w)?;
            if lifecycle.as_deref() != Some(lifecycle_text(cur)) {
                return Ok(Tx::Rollback(IfUnchanged::LifecycleMoved));
            }
            // 2.
            let rows = read_rebase_pending(c, Some(id))?;
            if rows.iter().map(PendingKey::of).collect::<Vec<_>>() != keys {
                return Ok(Tx::Rollback(IfUnchanged::PendingMoved));
            }
            // 3.
            let mut items = Vec::new();
            let mut seen = BTreeSet::new();
            tracked_items(c, sid, &mut items, &mut seen)?;
            own_items(c, sid, &mut items, &mut seen)?;
            for row in rows.iter().filter(|r| r.reason == "conflicts") {
                items.push(json!({
                    "repo": row.repo,
                    "code": "conflicts",
                    "conflicts": [],
                    "auto_reason": "conflicts",
                }));
            }
            report_items(c, sid, &mut items)?;
            if !items.is_empty() {
                return Ok(Tx::Rollback(IfUnchanged::Reopened(items)));
            }
            // 4. The `UPDATE` of `set_lifecycle`.
            let n = c
                .execute(
                    "UPDATE sessions SET lifecycle = ?3 WHERE id = ?1 AND lifecycle = ?2",
                    params![sid, lifecycle_text(cur), lifecycle_text(Lifecycle::Removing)],
                )
                .map_err(w)?;
            Ok(if n == 1 {
                Tx::Commit(IfUnchanged::Removing)
            } else {
                Tx::Rollback(IfUnchanged::LifecycleMoved)
            })
        })
    }
}
