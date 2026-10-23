//! Publish records and the ref audit (store.md §5.9).

use std::time::SystemTime;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use strata_types::{Action, Batch, Code, CommitOid, ReleaseGroup, StrataError};

use crate::codec::*;
use crate::errors::{NOT_WRITTEN, bad_row, err, invalid, no_such_session, sql, sql_read, too_large};
use crate::sessions::session_lifecycle;
use crate::txn::Tx;
use crate::{PublishPhase, PublishRecord, RefChange, RefOp, RefOutcome, Result, Store};

fn w(op: &'static str) -> impl Fn(rusqlite::Error) -> StrataError {
    move |e| sql(e, op, NOT_WRITTEN)
}

fn no_such_publish(id: u64) -> StrataError {
    err(Code::InvalidInput, format!("no publish {id}"), json!({ "reason": "no_such_publish", "publish": id }))
}

fn allowed(from: PublishPhase, to: PublishPhase) -> bool {
    matches!(
        (from, to),
        (PublishPhase::Running, PublishPhase::Done) | (PublishPhase::Running, PublishPhase::Unknown) | (PublishPhase::Unknown, PublishPhase::Done)
    )
}

fn publish_exists(c: &Connection, id: i64) -> rusqlite::Result<bool> {
    Ok(c.query_row("SELECT 1 FROM publishes WHERE id = ?1", [id], |_| Ok(())).optional()?.is_some())
}

fn load_publish(c: &Connection, id: i64, phase: String, started: i64, ended: Option<i64>, outcome: Option<String>) -> Result<PublishRecord> {
    let uid = seq(id, "publish id")?;
    let mut st = c
        .prepare("SELECT batch, item, session, repo, checkpoint, path FROM publish_items WHERE publish = ?1 ORDER BY batch, item")
        .map_err(sql_read)?;
    let rows = st
        .query_map([id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, i64>(4)?, r.get::<_, String>(5)?))
        })
        .map_err(sql_read)?;
    let mut batches: Vec<Batch> = Vec::new();
    for row in rows {
        let (b, i, session, repo, cp, path) = row.map_err(sql_read)?;
        let (b, i) = (nonneg(b, "publish batch")?, nonneg(i, "publish item")?);
        let cp = strata_types::CheckpointId::try_from(seq(cp, "publish checkpoint")?).map_err(|_| bad_row("publish checkpoint out of range"))?;
        if b as usize == batches.len() {
            if i != 0 {
                return Err(bad_row(format!("publish {id} batch {b} does not start at item 0")));
            }
            batches.push(Batch { session: parse(session, "publish session")?, repo: parse(repo, "publish repo")?, checkpoint: cp, paths: vec![] });
        } else if b as usize + 1 != batches.len() {
            return Err(bad_row(format!("publish {id} skips batch {}", batches.len())));
        } else {
            let last = batches.last().expect("batch");
            if last.session.as_str() != session || last.repo.as_str() != repo || last.checkpoint != cp {
                return Err(bad_row(format!("publish {id} batch {b} names two checkpoints")));
            }
            if i as usize != last.paths.len() {
                return Err(bad_row(format!("publish {id} batch {b} skips item {}", last.paths.len())));
            }
        }
        batches.last_mut().expect("batch").paths.push(parse(path, "publish path")?);
    }
    if batches.is_empty() {
        return Err(bad_row(format!("publish {id} has no items")));
    }
    let phase = publish_phase_of(&phase)?;
    if (phase == PublishPhase::Running) != ended.is_none() {
        return Err(bad_row(format!("publish {id} phase and end time disagree")));
    }
    Ok(PublishRecord {
        group: ReleaseGroup { id: uid, batches },
        phase,
        started: time(started),
        ended: opt_time(ended),
        outcome: outcome.map(|o| from_json::<Value>(&o, "publish outcome")).transpose()?,
    })
}

fn publishes_where(c: &Connection, cond: &str, arg: Option<i64>) -> Result<Vec<PublishRecord>> {
    let mut st = c
        .prepare(&format!("SELECT id, phase, started_ns, ended_ns, outcome FROM publishes WHERE {cond} ORDER BY id"))
        .map_err(sql_read)?;
    let map = |r: &rusqlite::Row<'_>| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, Option<i64>>(3)?, r.get::<_, Option<String>>(4)?))
    };
    let rows: Vec<_> = match arg {
        Some(a) => st.query_map([a], map).map_err(sql_read)?.collect::<rusqlite::Result<_>>(),
        None => st.query_map([], map).map_err(sql_read)?.collect::<rusqlite::Result<_>>(),
    }
    .map_err(sql_read)?;
    rows.into_iter().map(|(id, ph, s, e, o)| load_publish(c, id, ph, s, e, o)).collect()
}

fn pairing_ok(op: RefOp, outcome: RefOutcome) -> bool {
    match op {
        RefOp::UpdateRef => matches!(outcome, RefOutcome::Applied | RefOutcome::HeadMoved),
        RefOp::Push => matches!(outcome, RefOutcome::Pushed | RefOutcome::Refused | RefOutcome::Unknown),
    }
}

impl Store {
    pub fn record_publish(&self, group: &ReleaseGroup, at: SystemTime) -> Result<u64> {
        const OP: &str = "record_publish";
        if group.batches.is_empty() {
            return Err(invalid("batches", "a publish needs at least one batch"));
        }
        for (i, b) in group.batches.iter().enumerate() {
            if b.paths.is_empty() {
                return Err(err(Code::InvalidInput, format!("batch {i} has no paths"), json!({ "reason": "paths", "batch": i })));
            }
        }
        let started = ns(at, "at")?;
        self.write(OP, |c| {
            for (i, b) in group.batches.iter().enumerate() {
                let sid = b.session.as_str();
                match session_lifecycle(c, sid).map_err(w(OP))? {
                    None => return Err(no_such_session(sid)),
                    Some(lc) if lc == "removed" => return Err(no_such_session(sid)),
                    Some(_) => {}
                }
                let cp = int(u64::from(b.checkpoint), "checkpoint")?;
                let repo: Option<String> = c
                    .query_row("SELECT repo FROM checkpoints WHERE session = ?1 AND id = ?2", params![sid, cp], |r| r.get(0))
                    .optional()
                    .map_err(w(OP))?;
                match repo {
                    None => {
                        return Err(err(
                            Code::NoSuchCheckpoint,
                            format!("no checkpoint {} in session {sid}", b.checkpoint),
                            json!({ "session": sid, "checkpoint": u64::from(b.checkpoint), "batch": i }),
                        ));
                    }
                    Some(r) if r != b.repo.as_str() => {
                        return Err(err(
                            Code::InvalidInput,
                            format!("checkpoint {} of {sid} is for {r}, batch {i} says {}", b.checkpoint, b.repo.as_str()),
                            json!({ "reason": "checkpoint_repo", "batch": i, "repo": b.repo.as_str(), "checkpoint_repo": r }),
                        ));
                    }
                    Some(_) => {}
                }
            }
            let top: i64 = c.query_row("SELECT COALESCE(MAX(id), 0) FROM publishes", [], |r| r.get(0)).map_err(w(OP))?;
            let id = top.max(0).checked_add(1).ok_or_else(|| too_large("release_group_id"))?;
            c.execute("INSERT INTO publishes (id, phase, started_ns) VALUES (?1, 'running', ?2)", params![id, started]).map_err(w(OP))?;
            let mut ins = c
                .prepare("INSERT INTO publish_items (publish, batch, item, session, repo, checkpoint, path) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)")
                .map_err(w(OP))?;
            for (bi, b) in group.batches.iter().enumerate() {
                let cp = int(u64::from(b.checkpoint), "checkpoint")?;
                for (pi, p) in b.paths.iter().enumerate() {
                    ins.execute(params![id, bi as i64, pi as i64, b.session.as_str(), b.repo.as_str(), cp, p.as_str()]).map_err(w(OP))?;
                }
            }
            Ok(Tx::Commit(id as u64))
        })
    }

    pub fn set_publish_phase(&self, id: u64, from: PublishPhase, to: PublishPhase, outcome: Option<&Value>, at: SystemTime) -> Result<bool> {
        const OP: &str = "set_publish_phase";
        if !allowed(from, to) {
            return Err(err(
                Code::InvalidInput,
                format!("publish phase {} → {} is not allowed", publish_phase_text(from), publish_phase_text(to)),
                json!({ "reason": "transition", "from": publish_phase_text(from), "to": publish_phase_text(to) }),
            ));
        }
        let ended = ns(at, "at")?;
        let outcome = outcome.map(|o| to_json(o, "outcome")).transpose()?;
        let rid = int(id, "publish")?;
        self.write(OP, |c| {
            if !publish_exists(c, rid).map_err(w(OP))? {
                return Err(no_such_publish(id));
            }
            let n = c
                .execute(
                    "UPDATE publishes SET phase = ?3, ended_ns = ?4, outcome = COALESCE(?5, outcome) WHERE id = ?1 AND phase = ?2",
                    params![rid, publish_phase_text(from), publish_phase_text(to), ended, outcome],
                )
                .map_err(w(OP))?;
            Ok(if n == 1 { Tx::Commit(true) } else { Tx::Rollback(false) })
        })
    }

    pub fn publish_record(&self, id: u64) -> Result<Option<PublishRecord>> {
        let Ok(rid) = i64::try_from(id) else { return Ok(None) };
        self.read(|c| Ok(publishes_where(c, "id = ?1", Some(rid))?.pop()))
    }

    pub fn open_publishes(&self) -> Result<Vec<PublishRecord>> {
        self.read(|c| publishes_where(c, "phase IN ('running', 'unknown')", None))
    }

    pub fn record_ref_change(&self, change: &RefChange) -> Result<()> {
        const OP: &str = "record_ref_change";
        if !pairing_ok(change.op, change.outcome) {
            return Err(err(
                Code::InvalidInput,
                format!("{} cannot have outcome {}", ref_op_text(change.op), ref_outcome_text(change.outcome)),
                json!({ "reason": "outcome", "op": ref_op_text(change.op), "outcome": ref_outcome_text(change.outcome) }),
            ));
        }
        if change.op == RefOp::UpdateRef && !change.ref_name.starts_with("refs/strata/") {
            return Err(err(
                Code::InvalidInput,
                format!("update_ref only touches refs/strata/, not {}", change.ref_name),
                json!({ "reason": "ref_name", "ref_name": change.ref_name }),
            ));
        }
        if change.ref_name.is_empty() {
            return Err(invalid("ref_name", "ref name is empty"));
        }
        let at = ns(change.at, "at")?;
        let publish = change.publish.map(|p| int(p, "publish")).transpose()?;
        let confirmed = change.confirmed.as_ref().map(|a| to_json(a, "confirmed")).transpose()?;
        self.write(OP, |c| {
            if let Some(p) = publish {
                if !publish_exists(c, p).map_err(w(OP))? {
                    return Err(no_such_publish(p as u64));
                }
            }
            let top: i64 = c.query_row("SELECT COALESCE(MAX(id), 0) FROM refs_audit", [], |r| r.get(0)).map_err(w(OP))?;
            let id = top.max(0).checked_add(1).ok_or_else(|| too_large("refs_audit_id"))?;
            c.execute(
                "INSERT INTO refs_audit (id, at_ns, repo, op, ref_name, old_oid, new_oid, outcome, publish, confirmed, detail) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    id,
                    at,
                    change.repo.as_str(),
                    ref_op_text(change.op),
                    change.ref_name,
                    change.old.as_ref().map(CommitOid::as_str),
                    change.new.as_str(),
                    ref_outcome_text(change.outcome),
                    publish,
                    confirmed,
                    change.detail
                ],
            )
            .map_err(w(OP))?;
            Ok(Tx::Commit(()))
        })
    }

    pub fn ref_changes(&self, publish: u64) -> Result<Vec<RefChange>> {
        let Ok(p) = i64::try_from(publish) else { return Ok(vec![]) };
        self.read(|c| {
            let mut st = c
                .prepare(
                    "SELECT id, at_ns, repo, op, ref_name, old_oid, new_oid, outcome, publish, confirmed, detail \
                     FROM refs_audit WHERE publish = ?1 ORDER BY at_ns, id",
                )
                .map_err(sql_read)?;
            let rows = st
                .query_map([p], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, String>(7)?,
                        r.get::<_, Option<i64>>(8)?,
                        r.get::<_, Option<String>>(9)?,
                        r.get::<_, Option<String>>(10)?,
                    ))
                })
                .map_err(sql_read)?;
            let mut out = Vec::new();
            for row in rows {
                let (id, at, repo, op, ref_name, old, new, outcome, publish, confirmed, detail) = row.map_err(sql_read)?;
                seq(id, "refs_audit id")?;
                out.push(RefChange {
                    at: time(at),
                    repo: parse(repo, "refs_audit repo")?,
                    op: ref_op_of(&op)?,
                    ref_name,
                    old: opt_parse(old, "refs_audit old_oid")?,
                    new: parse(new, "refs_audit new_oid")?,
                    outcome: ref_outcome_of(&outcome)?,
                    publish: publish.map(|p| seq(p, "refs_audit publish")).transpose()?,
                    confirmed: confirmed.map(|s| from_json::<Action>(&s, "refs_audit confirmed")).transpose()?,
                    detail,
                });
            }
            Ok(out)
        })
    }
}
