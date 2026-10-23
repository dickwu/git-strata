//! Deps sets, attempts, proposals and the main record (store.md §5.6, §5.10).

use std::time::SystemTime;

use crate::sql::{Connection, OptionalExtension, Row, params};
use serde_json::json;
use strata_types::{Code, CommitOid, DepsSet, DepsSetId, ProposalId, RepoName, RepoPath, StrataError, TreeOid};

use crate::codec::*;
use crate::errors::{NOT_WRITTEN, bad_row, err, invalid, invalid_field, sql, sql_read};
use crate::txn::{Tx, next_seq};
use crate::{
    AttemptKind, AttemptState, DepsAttempt, DepsMain, DepsOrigin, DepsSetRecord, NewProposal, ProposalDecision,
    ProposalRecord, ProposalStatus, Result, Store,
};

fn hex64(s: &str, field: &str) -> Result<()> {
    if s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        Ok(())
    } else {
        Err(invalid_field(field, format!("{field} must be 64 lowercase hex digits")))
    }
}

fn no_such_attempt(id: u64) -> StrataError {
    err(Code::InvalidInput, format!("no deps attempt {id}"), json!({ "reason": "no_such_attempt", "attempt": id }))
}

fn no_such_proposal(id: ProposalId) -> StrataError {
    err(Code::InvalidInput, format!("no proposal {id}"), json!({ "reason": "no_such_proposal", "proposal": id.get() }))
}

const ATTEMPT_COLS: &str = "id, repo, set_id, kind, state, staging, pid, started_ns, ended_ns, error";
const PROPOSAL_COLS: &str = "id, repo, origin_session, from_set, from_inputs, to_set, to_inputs, tree, head, state, \
                             superseded_by, created_ns, updated_ns";
const MAIN_COLS: &str = "repo, tree, head, inputs_hash, set_id, observed_ns";

fn attempt_row(r: &Row) -> crate::sql::SqlResult<Result<DepsAttempt>> {
    let id: i64 = r.get(0)?;
    let repo: String = r.get(1)?;
    let set: String = r.get(2)?;
    let kind: String = r.get(3)?;
    let state: String = r.get(4)?;
    let staging: String = r.get(5)?;
    let pid: Option<i64> = r.get(6)?;
    let started: i64 = r.get(7)?;
    let ended: Option<i64> = r.get(8)?;
    let error: Option<String> = r.get(9)?;
    Ok((|| {
        let pid = match pid {
            None => None,
            Some(p) => Some(i32::try_from(p).ok().filter(|p| *p > 0).ok_or_else(|| bad_row(format!("bad pid {p}")))?),
        };
        let state = attempt_state_of(&state)?;
        let error: Option<StrataError> = error.map(|e| from_json(&e, "deps_attempts.error")).transpose()?;
        if (state == AttemptState::Running) != ended.is_none()
            || matches!(state, AttemptState::Failed | AttemptState::Refused) != error.is_some()
        {
            return Err(bad_row(format!("deps attempt {id}: state and columns disagree")));
        }
        Ok(DepsAttempt {
            id: seq(id, "attempt id")?,
            repo: parse(repo, "repo")?,
            set: parse(set, "deps set id")?,
            kind: attempt_kind_of(&kind)?,
            state,
            staging: pathbuf(staging),
            pid,
            started: time(started),
            ended: opt_time(ended),
            error,
        })
    })())
}

fn proposal_row(r: &Row) -> crate::sql::SqlResult<Result<ProposalRecord>> {
    let id: i64 = r.get(0)?;
    let repo: String = r.get(1)?;
    let origin: Option<String> = r.get(2)?;
    let from: String = r.get(3)?;
    let from_inputs: String = r.get(4)?;
    let to: String = r.get(5)?;
    let to_inputs: String = r.get(6)?;
    let tree: Option<String> = r.get(7)?;
    let head: Option<String> = r.get(8)?;
    let state: String = r.get(9)?;
    let by: Option<i64> = r.get(10)?;
    let created: i64 = r.get(11)?;
    let updated: i64 = r.get(12)?;
    Ok((|| {
        let status = proposal_status_of(&state)?;
        let superseded_by = by.map(|b| seq(b, "superseded_by").map(|b| ProposalId::from(b as i64))).transpose()?;
        if (status == ProposalStatus::Superseded) != superseded_by.is_some() || tree.is_some() != head.is_some() {
            return Err(bad_row(format!("proposal {id}: state and columns disagree")));
        }
        let check = |s: &str, w: &str| hex64(s, w).map_err(|_| bad_row(format!("proposal {id}: bad {w}")));
        check(&from_inputs, "from_inputs")?;
        check(&to_inputs, "to_inputs")?;
        Ok(ProposalRecord {
            id: ProposalId::from(seq(id, "proposal id")? as i64),
            repo: parse(repo, "repo")?,
            origin: opt_parse(origin, "session id")?,
            from: parse(from, "deps set id")?,
            from_inputs,
            to: parse(to, "deps set id")?,
            to_inputs,
            tree: opt_parse(tree, "tree oid")?,
            head: opt_parse(head, "commit oid")?,
            status,
            superseded_by,
            created: time(created),
            updated: time(updated),
        })
    })())
}

fn main_row(r: &Row) -> crate::sql::SqlResult<Result<DepsMain>> {
    let repo: String = r.get(0)?;
    let tree: String = r.get(1)?;
    let head: String = r.get(2)?;
    let inputs_hash: String = r.get(3)?;
    let set: String = r.get(4)?;
    let observed: i64 = r.get(5)?;
    Ok((|| {
        hex64(&inputs_hash, "inputs_hash").map_err(|_| bad_row("deps_main: bad inputs_hash"))?;
        Ok(DepsMain {
            repo: parse(repo, "repo")?,
            tree: parse(tree, "tree oid")?,
            head: parse(head, "commit oid")?,
            inputs_hash,
            set: parse(set, "deps set id")?,
            observed: time(observed),
        })
    })())
}

fn collect<T>(
    c: &Connection,
    q: &str,
    p: impl crate::sql::Params,
    f: fn(&Row) -> crate::sql::SqlResult<Result<T>>,
) -> Result<Vec<T>> {
    let mut st = c.prepare(q).map_err(sql_read)?;
    let rows = st.query_map(p, f).map_err(sql_read)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(sql_read)??);
    }
    Ok(out)
}

fn one<T>(
    c: &Connection,
    q: &str,
    p: impl crate::sql::Params,
    f: fn(&Row) -> crate::sql::SqlResult<Result<T>>,
) -> Result<Option<T>> {
    c.query_row(q, p, f).optional().map_err(sql_read)?.transpose()
}

/// Proposal state and repo, in a write transaction.
fn proposal_state(c: &Connection, id: i64, op: &str) -> Result<Option<(String, String)>> {
    c.query_row("SELECT state, repo FROM deps_proposals WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
        .map_err(|e| sql(e, op, NOT_WRITTEN))
}

fn main_eq(a: &DepsMain, b: &DepsMain) -> Result<bool> {
    Ok(a.repo == b.repo
        && a.tree == b.tree
        && a.head == b.head
        && a.inputs_hash == b.inputs_hash
        && a.set == b.set
        && ns(a.observed, "observed")? == ns(b.observed, "observed")?)
}

impl Store {
    pub fn begin_deps_attempt(&self, repo: &RepoName, set: &DepsSetId, kind: AttemptKind, at: SystemTime) -> Result<u64> {
        const OP: &str = "begin_deps_attempt";
        self.writable()?;
        let at = ns(at, "at")?;
        self.write(OP, |c| {
            let running: Option<i64> = c
                .query_row(
                    "SELECT id FROM deps_attempts WHERE repo = ?1 AND set_id = ?2 AND state = 'running'",
                    params![repo.as_str(), set.as_str()],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| sql(e, OP, NOT_WRITTEN))?;
            if let Some(a) = running {
                return Err(err(
                    Code::Exists,
                    format!("deps set {set} of {repo} already has running attempt {a}"),
                    json!({ "attempt": a, "repo": repo.as_str(), "set": set.as_str() }),
                ));
            }
            let n = next_seq(c, "deps_attempts", "attempt_id", OP)?;
            let staging = self.deps_staging_dir(repo, n as u64);
            let staging = path_text(&staging, "staging")?.to_owned();
            c.execute(
                "INSERT INTO deps_attempts (id, repo, set_id, kind, state, staging, started_ns) \
                 VALUES (?1, ?2, ?3, ?4, 'running', ?5, ?6)",
                params![n, repo.as_str(), set.as_str(), attempt_kind_text(kind), staging, at],
            )
            .map_err(|e| sql(e, OP, NOT_WRITTEN))?;
            Ok(Tx::Commit(n as u64))
        })
    }

    pub fn set_deps_attempt_pid(&self, attempt: u64, pid: i32) -> Result<bool> {
        const OP: &str = "set_deps_attempt_pid";
        self.writable()?;
        if pid <= 0 {
            return Err(invalid_field("pid", format!("pid {pid} is not positive")));
        }
        let id = int(attempt, "attempt")?;
        self.write(OP, |c| {
            let n = c
                .execute(
                    "UPDATE deps_attempts SET pid = ?1 WHERE id = ?2 AND state = 'running' AND pid IS NULL",
                    params![pid, id],
                )
                .map_err(|e| sql(e, OP, NOT_WRITTEN))?;
            if n == 1 {
                return Ok(Tx::Commit(true));
            }
            let row: Option<(String, Option<i64>)> = c
                .query_row("SELECT state, pid FROM deps_attempts WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()
                .map_err(|e| sql(e, OP, NOT_WRITTEN))?;
            match row {
                None => Err(no_such_attempt(attempt)),
                Some((state, _)) if state != "running" => Ok(Tx::Rollback(false)),
                Some((_, p)) => Err(err(
                    Code::InvalidInput,
                    format!("deps attempt {attempt} already has pid {}", p.unwrap_or_default()),
                    json!({ "reason": "pid_already_set", "attempt": attempt, "pid": p }),
                )),
            }
        })
    }

    pub fn end_deps_attempt(&self, attempt: u64, to: AttemptState, error: &StrataError, at: SystemTime) -> Result<bool> {
        const OP: &str = "end_deps_attempt";
        self.writable()?;
        if !matches!(to, AttemptState::Failed | AttemptState::Refused) {
            return Err(invalid_field("to", "an attempt can only end as Failed or Refused"));
        }
        let id = int(attempt, "attempt")?;
        let at = ns(at, "at")?;
        let error = to_json(error, "error")?;
        self.write(OP, |c| {
            let n = c
                .execute(
                    "UPDATE deps_attempts SET state = ?1, ended_ns = ?2, error = ?3 WHERE id = ?4 AND state = 'running'",
                    params![attempt_state_text(to), at, error, id],
                )
                .map_err(|e| sql(e, OP, NOT_WRITTEN))?;
            if n == 1 {
                return Ok(Tx::Commit(true));
            }
            let exists: bool = c
                .query_row("SELECT EXISTS (SELECT 1 FROM deps_attempts WHERE id = ?1)", [id], |r| r.get(0))
                .map_err(|e| sql(e, OP, NOT_WRITTEN))?;
            if exists { Ok(Tx::Rollback(false)) } else { Err(no_such_attempt(attempt)) }
        })
    }

    pub fn record_deps_set(&self, set: &DepsSet, origin: DepsOrigin, attempt: Option<u64>, at: SystemTime) -> Result<()> {
        const OP: &str = "record_deps_set";
        self.writable()?;
        hex64(&set.inputs_hash, "inputs_hash")?;
        let at = ns(at, "at")?;
        let kind = match origin {
            DepsOrigin::Empty => {
                if attempt.is_some() {
                    return Err(invalid("origin_mismatch", "an Empty deps set has no attempt"));
                }
                if !set.dirs.is_empty() {
                    return Err(invalid("origin_mismatch", "an Empty deps set has no dirs"));
                }
                None
            }
            DepsOrigin::Adopt => Some(AttemptKind::Adopt),
            DepsOrigin::Build => Some(AttemptKind::Build),
        };
        let attempt_id = match (kind, attempt) {
            (Some(_), None) => return Err(invalid("origin_mismatch", "an Adopt or Build deps set needs its attempt")),
            (_, a) => a.map(|a| int(a, "attempt")).transpose()?,
        };
        let dirs = to_json(&set.dirs.iter().map(RepoPath::as_str).collect::<Vec<_>>(), "dirs")?;
        self.write(OP, |c| {
            let exists: bool = c
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM deps_sets WHERE repo = ?1 AND id = ?2)",
                    params![set.repo.as_str(), set.id.as_str()],
                    |r| r.get(0),
                )
                .map_err(|e| sql(e, OP, NOT_WRITTEN))?;
            if exists {
                return Err(err(
                    Code::Exists,
                    format!("deps set {} of {} is already recorded", set.id, set.repo),
                    json!({ "repo": set.repo.as_str(), "set": set.id.as_str() }),
                ));
            }
            if let (Some(kind), Some(aid)) = (kind, attempt_id) {
                let row: Option<(String, String, String, String)> = c
                    .query_row("SELECT repo, set_id, kind, state FROM deps_attempts WHERE id = ?1", [aid], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                    })
                    .optional()
                    .map_err(|e| sql(e, OP, NOT_WRITTEN))?;
                let ok = matches!(&row, Some((r, s, k, st))
                    if r == set.repo.as_str() && s == set.id.as_str() && k == attempt_kind_text(kind) && st == "running");
                if !ok {
                    return Err(err(
                        Code::InvalidInput,
                        format!("attempt {aid} is not a running {} attempt for this deps set", attempt_kind_text(kind)),
                        json!({ "reason": "origin_mismatch", "attempt": aid }),
                    ));
                }
            }
            c.execute(
                "INSERT INTO deps_sets (repo, id, inputs_hash, platform, toolchain, dirs, origin, sealed_ns) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    set.repo.as_str(),
                    set.id.as_str(),
                    set.inputs_hash,
                    set.platform,
                    set.toolchain,
                    dirs,
                    origin_text(origin),
                    at
                ],
            )
            .map_err(|e| sql(e, OP, NOT_WRITTEN))?;
            if let Some(aid) = attempt_id {
                c.execute("UPDATE deps_attempts SET state = 'sealed', ended_ns = ?1 WHERE id = ?2", params![at, aid])
                    .map_err(|e| sql(e, OP, NOT_WRITTEN))?;
            }
            Ok(Tx::Commit(()))
        })
    }

    pub fn record_proposal(&self, new: &NewProposal, replaces: Option<ProposalId>, at: SystemTime) -> Result<Option<ProposalId>> {
        const OP: &str = "record_proposal";
        self.writable()?;
        if new.from == new.to {
            return Err(invalid("from_equals_to", "a proposal's from and to are the same deps set"));
        }
        if new.tree.is_some() != new.head.is_some() {
            return Err(invalid("tree_head_mismatch", "tree and head must be given together"));
        }
        let target = new.tree.is_some();
        if !target && new.origin.is_none() {
            return Err(invalid("no_origin", "a proposal without a target needs its proposing session"));
        }
        hex64(&new.from_inputs, "from_inputs")?;
        hex64(&new.to_inputs, "to_inputs")?;
        let at = ns(at, "at")?;
        let origin = new.origin.as_ref().map(|s| s.as_str().to_owned());
        self.write(OP, |c| {
            let occupant: Option<i64> = if target {
                c.query_row(
                    "SELECT id FROM deps_proposals WHERE repo = ?1 AND state = 'pending'",
                    [new.repo.as_str()],
                    |r| r.get(0),
                )
            } else {
                c.query_row(
                    "SELECT id FROM deps_proposals WHERE repo = ?1 AND origin_session = ?2 AND state = 'awaiting_publish'",
                    params![new.repo.as_str(), origin],
                    |r| r.get(0),
                )
            }
            .optional()
            .map_err(|e| sql(e, OP, NOT_WRITTEN))?;
            if occupant != replaces.map(|p| p.get() as i64) {
                return Ok(Tx::Rollback(None));
            }
            let n = next_seq(c, "deps_proposals", "proposal_id", OP)?;
            if let Some(old) = occupant {
                c.execute(
                    "UPDATE deps_proposals SET state = 'superseded', superseded_by = ?1, updated_ns = ?2 WHERE id = ?3",
                    params![n, at, old],
                )
                .map_err(|e| sql(e, OP, NOT_WRITTEN))?;
            }
            c.execute(
                "INSERT INTO deps_proposals (id, repo, origin_session, from_set, from_inputs, to_set, to_inputs, tree, head, \
                 state, created_ns, updated_ns) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
                params![
                    n,
                    new.repo.as_str(),
                    origin,
                    new.from.as_str(),
                    new.from_inputs,
                    new.to.as_str(),
                    new.to_inputs,
                    new.tree.as_ref().map(TreeOid::as_str),
                    new.head.as_ref().map(CommitOid::as_str),
                    if target { "pending" } else { "awaiting_publish" },
                    at
                ],
            )
            .map_err(|e| sql(e, OP, NOT_WRITTEN))?;
            Ok(Tx::Commit(Some(ProposalId::from(n))))
        })
    }

    pub fn promote_proposal(&self, id: ProposalId, tree: &TreeOid, head: &CommitOid, at: SystemTime) -> Result<bool> {
        const OP: &str = "promote_proposal";
        self.writable()?;
        let pid = int(id.get(), "id")?;
        let at = ns(at, "at")?;
        self.write(OP, |c| {
            let Some((state, repo)) = proposal_state(c, pid, OP)? else {
                return Err(no_such_proposal(id));
            };
            if state != "awaiting_publish" {
                return Ok(Tx::Rollback(false));
            }
            c.execute(
                "UPDATE deps_proposals SET state = 'superseded', superseded_by = ?1, updated_ns = ?2 \
                 WHERE repo = ?3 AND id <> ?1 AND (state = 'pending' OR (state = 'awaiting_publish' \
                 AND to_set = (SELECT to_set FROM deps_proposals WHERE id = ?1)))",
                params![pid, at, repo],
            )
            .map_err(|e| sql(e, OP, NOT_WRITTEN))?;
            c.execute(
                "UPDATE deps_proposals SET state = 'pending', tree = ?1, head = ?2, updated_ns = ?3 WHERE id = ?4",
                params![tree.as_str(), head.as_str(), at, pid],
            )
            .map_err(|e| sql(e, OP, NOT_WRITTEN))?;
            Ok(Tx::Commit(true))
        })
    }

    pub fn decide_proposal(&self, id: ProposalId, to: ProposalDecision, at: SystemTime) -> Result<bool> {
        const OP: &str = "decide_proposal";
        self.writable()?;
        let pid = int(id.get(), "id")?;
        let at = ns(at, "at")?;
        let by = match to {
            ProposalDecision::Applied => None,
            ProposalDecision::Superseded { by } => {
                if by == id {
                    return Err(invalid("self_supersede", "a proposal cannot supersede itself"));
                }
                Some(int(by.get(), "by")?)
            }
        };
        self.write(OP, |c| {
            if proposal_state(c, pid, OP)?.is_none() {
                return Err(no_such_proposal(id));
            }
            if let (Some(b), ProposalDecision::Superseded { by }) = (by, to)
                && proposal_state(c, b, OP)?.is_none()
            {
                return Err(err(
                    Code::InvalidInput,
                    format!("superseding proposal {by} does not exist"),
                    json!({ "reason": "no_such_proposal", "field": "by", "proposal": by.get() }),
                ));
            }
            let n = c
                .execute(
                    "UPDATE deps_proposals SET state = ?1, superseded_by = ?2, updated_ns = ?3 WHERE id = ?4 AND state = 'pending'",
                    params![if by.is_some() { "superseded" } else { "applied" }, by, at, pid],
                )
                .map_err(|e| sql(e, OP, NOT_WRITTEN))?;
            Ok(if n == 1 { Tx::Commit(true) } else { Tx::Rollback(false) })
        })
    }

    pub fn set_deps_main(&self, row: &DepsMain, expected: Option<&DepsMain>) -> Result<(bool, Option<DepsMain>)> {
        const OP: &str = "set_deps_main";
        self.writable()?;
        if let Some(e) = expected {
            if e.repo != row.repo {
                return Err(invalid("repo_mismatch", "row and expected name different repos"));
            }
            ns(e.observed, "expected.observed")?;
        }
        hex64(&row.inputs_hash, "inputs_hash")?;
        let observed = ns(row.observed, "observed")?;
        self.write(OP, |c| {
            let current = one(c, &format!("SELECT {MAIN_COLS} FROM deps_main WHERE repo = ?1"), [row.repo.as_str()], main_row)
                .map_err(|e| crate::errors::annotate(e, OP, NOT_WRITTEN))?;
            let equal = match (&current, expected) {
                (None, None) => true,
                (Some(a), Some(b)) => main_eq(a, b)?,
                _ => false,
            };
            if !equal {
                return Ok(Tx::Rollback((false, current)));
            }
            c.execute(
                "INSERT INTO deps_main (repo, tree, head, inputs_hash, set_id, observed_ns) VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT (repo) DO UPDATE SET tree = excluded.tree, head = excluded.head, \
                 inputs_hash = excluded.inputs_hash, set_id = excluded.set_id, observed_ns = excluded.observed_ns",
                params![row.repo.as_str(), row.tree.as_str(), row.head.as_str(), row.inputs_hash, row.set.as_str(), observed],
            )
            .map_err(|e| sql(e, OP, NOT_WRITTEN))?;
            Ok(Tx::Commit((true, current)))
        })
    }

    // ---- reads (§5.10) ----

    #[allow(clippy::type_complexity)]
    pub fn deps_set(&self, repo: &RepoName, id: &DepsSetId) -> Result<Option<DepsSetRecord>> {
        self.read(|c| {
            let row: Option<(String, String, String, String, String, String, String, i64)> = c
                .query_row(
                    "SELECT repo, id, inputs_hash, platform, toolchain, dirs, origin, sealed_ns FROM deps_sets \
                     WHERE repo = ?1 AND id = ?2",
                    params![repo.as_str(), id.as_str()],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)),
                )
                .optional()
                .map_err(sql_read)?;
            let Some((repo, id, inputs_hash, platform, toolchain, dirs, origin, sealed)) = row else {
                return Ok(None);
            };
            hex64(&inputs_hash, "inputs_hash").map_err(|_| bad_row("deps_sets: bad inputs_hash"))?;
            let dirs: Vec<String> = from_json(&dirs, "deps_sets.dirs")?;
            let dirs = dirs.into_iter().map(|d| parse(d, "repo path")).collect::<Result<Vec<RepoPath>>>()?;
            let origin = origin_of(&origin)?;
            if origin == DepsOrigin::Empty && !dirs.is_empty() {
                return Err(bad_row("deps_sets: an Empty set has dirs"));
            }
            Ok(Some(DepsSetRecord {
                set: DepsSet { id: parse(id, "deps set id")?, repo: parse(repo, "repo")?, inputs_hash, platform, toolchain, dirs },
                origin,
                sealed: time(sealed),
            }))
        })
    }

    pub fn deps_attempts(&self, repo: Option<&RepoName>) -> Result<Vec<DepsAttempt>> {
        self.read(|c| match repo {
            Some(r) => collect(
                c,
                &format!("SELECT {ATTEMPT_COLS} FROM deps_attempts WHERE repo = ?1 ORDER BY id"),
                [r.as_str()],
                attempt_row,
            ),
            None => collect(c, &format!("SELECT {ATTEMPT_COLS} FROM deps_attempts ORDER BY id"), [], attempt_row),
        })
    }

    pub fn proposal(&self, id: ProposalId) -> Result<Option<ProposalRecord>> {
        let Ok(pid) = i64::try_from(id.get()) else { return Ok(None) };
        self.read(|c| one(c, &format!("SELECT {PROPOSAL_COLS} FROM deps_proposals WHERE id = ?1"), [pid], proposal_row))
    }

    pub fn proposals(&self, repo: Option<&RepoName>, open_only: bool) -> Result<Vec<ProposalRecord>> {
        let open = if open_only {
            "AND (state = 'pending' OR (state = 'awaiting_publish' AND origin_session IN \
             (SELECT id FROM sessions WHERE lifecycle <> 'removed')))"
        } else {
            ""
        };
        let q = format!("SELECT {PROPOSAL_COLS} FROM deps_proposals WHERE (?1 IS NULL OR repo = ?1) {open} ORDER BY id");
        self.read(|c| collect(c, &q, [repo.map(RepoName::as_str)], proposal_row))
    }

    pub fn deps_main(&self, repo: &RepoName) -> Result<Option<DepsMain>> {
        self.read(|c| one(c, &format!("SELECT {MAIN_COLS} FROM deps_main WHERE repo = ?1"), [repo.as_str()], main_row))
    }
}
