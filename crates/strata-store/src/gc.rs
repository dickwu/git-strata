//! `gc`, `gc_repo` and `last_gc_report` (store.md §5.11).
//!
//! Guards for both: (a) paths named by open move intents and by running or
//! failed deps attempts are never touched, nor their ancestors and
//! descendants; (b) filesystem entries younger than the age floor (the later
//! of mtime and ctime, 1 hour by default) are never touched; (c) blob files
//! and `store/tmp/` are swept only under L3 exclusive.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::sql::{Connection, OptionalExtension, ToSql, params};
use serde_json::json;
use strata_apfs::{Attrs, FileKind, IoSide, RemoveKind, RenameMode};
use strata_types::{BlobId, Code, DepsSetId, RepoName, RepoPath, StrataError, TreeOid};

use crate::codec::{from_json, now, ns};
use crate::errors::{NOT_WRITTEN, PARTIAL, annotate, err, errno, sql, sql_read};
use crate::open::hex32;
use crate::seams::Seam;
use crate::txn::Tx;
use crate::{GcPreview, GcReport, Result, Store, TrashReport};

const OP_GC: &str = "gc";
const OP_REPO: &str = "gc_repo";
const DEFAULT_FLOOR_SECS: u64 = 3600;
const DEFAULT_SCRUB_BYTES: u64 = 2 << 30;
const DEFAULT_BATCH_ROWS: u64 = 10_000;

/// A removed session past keep that no running/unknown publish names.
const SESSION_GONE: &str = "s.lifecycle = 'removed' AND s.removed_ns IS NOT NULL AND s.removed_ns < ?2 \
     AND NOT EXISTS (SELECT 1 FROM publish_items pi JOIN publishes p ON p.id = pi.publish \
                     WHERE pi.session = s.id AND p.phase IN ('running', 'unknown'))";

/// A base generation some row still needs (§5.11 gc_repo step 1).
const GEN_PINNED: &str = "SELECT EXISTS (SELECT 1 FROM base_gens g WHERE g.id = ?1 AND ( \
       EXISTS (SELECT 1 FROM workspaces w WHERE w.repo = g.repo AND w.base_gen = g.id) \
    OR EXISTS (SELECT 1 FROM checkpoints k WHERE k.repo = g.repo AND k.base_gen = g.id) \
    OR EXISTS (SELECT 1 FROM move_intents m WHERE m.repo = g.repo AND m.to_tree = g.tree AND m.to_deps = g.deps \
               AND m.state IN ('preparing', 'swapping', 'unknown')) \
    OR g.id = (SELECT g2.id FROM base_gens g2 WHERE g2.repo = g.repo ORDER BY g2.created_ns DESC, g2.id DESC LIMIT 1)))";

/// A deps set pinned by a proposal or by deps_main (not by generations).
const DEPS_PINNED_OTHER: &str = "SELECT \
       EXISTS (SELECT 1 FROM deps_proposals p WHERE p.repo = ?1 AND p.to_set = ?2 AND (p.state = 'pending' \
               OR (p.state = 'awaiting_publish' AND EXISTS (SELECT 1 FROM sessions s WHERE s.id = p.origin_session)))) \
    OR EXISTS (SELECT 1 FROM deps_main m WHERE m.repo = ?1 AND m.set_id = ?2)";

fn rp(s: impl Into<String>) -> RepoPath {
    RepoPath::try_from(s.into()).expect("internal store path")
}

fn last_name(p: &RepoPath) -> &str {
    p.as_str().rsplit('/').next().unwrap_or("")
}

fn hex32_name(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

fn sqle(op: &'static str) -> impl Fn(crate::sql::Error) -> StrataError {
    move |e| sql(e, op, NOT_WRITTEN)
}

/// `Unknown` with `outcome = partial`: rows committed so far stay valid.
fn partial_error(op: &str, e: StrataError) -> StrataError {
    if e.code == Code::Unknown {
        let mut e = annotate(e, op, PARTIAL);
        if let Some(d) = e.detail.as_mut().and_then(|d| d.as_object_mut()) {
            d.insert("outcome".into(), json!(PARTIAL));
        }
        return e;
    }
    let cause = serde_json::to_value(&e).unwrap_or(serde_json::Value::Null);
    err(Code::Unknown, format!("{op} stopped part way: {}", e.message), json!({ "op": op, "outcome": PARTIAL, "cause": cause }))
}

enum Prep {
    /// The entry is gone: only the row goes.
    RowOnly,
    /// Rename this entry into trash.
    Entry,
}

struct Run<'a> {
    st: &'a Store,
    op: &'static str,
    cutoff: i64,
    floor: Duration,
    batch: u64,
    batches: u64,
    report: GcReport,
    /// What names a trash entry (store.md §5.11a); empty for `gc_repo`.
    refs: Vec<PathBuf>,
}

impl<'a> Run<'a> {
    fn new(st: &'a Store, op: &'static str, scope: Option<RepoName>, keep: Duration) -> Result<Run<'a>> {
        let started = now();
        let keep_ns = i64::try_from(keep.as_nanos()).unwrap_or(i64::MAX);
        let cutoff = ns(started, "now")?.saturating_sub(keep_ns);
        let floor = Duration::from_secs(st.seams.age_floor().unwrap_or(DEFAULT_FLOOR_SECS));
        let batch = st.seams.batch_rows().unwrap_or(DEFAULT_BATCH_ROWS).max(1);
        let report = GcReport {
            scope,
            started,
            ended: started,
            keep_secs: keep.as_secs(),
            sessions: 0,
            checkpoints: 0,
            overlay_entries: 0,
            proposals: 0,
            attempts: 0,
            base_gens: 0,
            base_trees: 0,
            deps_sets: 0,
            blobs: 0,
            blob_bytes: 0,
            dirs_retired: 0,
            trash_removed: 0,
            tmp_removed: 0,
            stuck: Vec::new(),
            verified: 0,
            corrupt: Vec::new(),
            trash_refused: Vec::new(),
            error: None,
        };
        Ok(Run { st, op, cutoff, floor, batch, batches: 0, report, refs: Vec::new() })
    }

    /// Guard (b): old enough to touch.
    fn old(&self, a: &Attrs) -> bool {
        match SystemTime::now().duration_since(a.last_changed()) {
            Ok(age) => age >= self.floor,
            Err(_) => self.floor.is_zero(),
        }
    }

    fn shown(&self, rel: &RepoPath) -> PathBuf {
        self.st.root.join(rel.as_str())
    }

    /// One batch: its own write transaction with deletes enabled. The
    /// `strata_gc_gate` row (Turso has no app-defined functions, so it
    /// stands in for `strata_gc_active()`) is set inside the transaction
    /// and cleared again before COMMIT, so no other transaction ever sees
    /// it on; a rollback restores it too.
    fn batch<T>(&self, f: impl FnOnce(&Connection) -> Result<Tx<T>>) -> Result<T> {
        let op = self.op;
        self.st.write(op, |c| {
            c.execute("UPDATE strata_gc_gate SET active = 1", ()).map_err(sqle(op))?;
            let out = f(c)?;
            if let Tx::Commit(_) = &out {
                c.execute("UPDATE strata_gc_gate SET active = 0", ()).map_err(sqle(op))?;
            }
            Ok(out)
        })
    }

    /// Count one committed batch; `GcPauseAfterBatch` stops here.
    fn committed(&mut self) {
        self.batches += 1;
        let n = self.batches;
        if self.st.seams.take(|s| matches!(s, Seam::GcPauseAfterBatch { n: k } if *k == n).then_some(())).is_some() {
            self.st.seams.pause();
        }
    }

    fn list(&self, dir: &RepoPath) -> Result<Vec<(RepoPath, Attrs)>> {
        strata_apfs::list_dir(&self.st.real_root, Some(dir)).collect()
    }

    /// `dir` exists and is a real directory (never a symlink).
    fn real_dir(&self, dir: &RepoPath) -> Result<bool> {
        match strata_apfs::attrs_of(&self.st.real_root, dir) {
            Ok(a) => Ok(matches!(a.kind, FileKind::Dir)),
            Err(e) if e.code == Code::NoSuchPath => Ok(false),
            Err(e) => Err(e),
        }
    }

    // ------------------------------------------------------------- finish

    fn finish(mut self, res: Result<()>, cursor: Option<String>) -> Result<GcReport> {
        self.report.ended = now();
        match res {
            Ok(()) => {
                self.append("complete", cursor.as_deref()).map_err(|e| partial_error(self.op, e))?;
                Ok(self.report)
            }
            Err(e) => {
                let e = if e.code == Code::Busy { e } else { partial_error(self.op, e) };
                self.report.error = Some(e.clone());
                let _ = self.append("partial", cursor.as_deref());
                Err(e)
            }
        }
    }

    fn append(&self, outcome: &str, cursor: Option<&str>) -> Result<()> {
        let op = self.op;
        let text = serde_json::to_string(&self.report).map_err(|e| err(Code::Unknown, format!("gc report: {e}"), json!({ "op": op })))?;
        let started = ns(self.report.started, "started")?;
        let ended = ns(self.report.ended, "ended")?;
        let scope = self.report.scope.as_ref().map(|r| r.as_str().to_owned());
        let cursor = if scope.is_some() { None } else { cursor.map(str::to_owned) };
        self.st.write(op, |c| {
            let id: i64 = c.query_row("SELECT COALESCE(MAX(id), 0) + 1 FROM gc_runs", (), |r| r.get(0)).map_err(sqle(op))?;
            c.execute(
                "INSERT INTO gc_runs (id, scope, started_ns, ended_ns, outcome, report, scrub_after) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![id, scope, started, ended, outcome, text, cursor],
            )
            .map_err(sqle(op))?;
            Ok(Tx::Commit(()))
        })
    }

    // ------------------------------------------------------------------ gc

    fn gc_body(&mut self, cursor: &mut Option<String>) -> Result<()> {
        let st = self.st;
        let cutoff = self.cutoff;
        // 1. the retention set, from one snapshot
        let (r, d, attempts, runs) = st.read(|c| retention(c, cutoff))?;
        st.seams.pause_if(&Seam::GcPauseBeforeRows);

        // 2. rows, in reference order
        self.delete_sessions(&r)?;
        self.delete_proposals(&d)?;
        self.report.attempts += self.delete_ids(
            &attempts,
            "DELETE FROM deps_attempts WHERE id = ?1 AND state IN ('sealed', 'refused', 'failed') AND ended_ns < ?2",
        )?;
        self.delete_ids(
            &runs,
            "DELETE FROM gc_runs WHERE id = ?1 AND ended_ns < ?2 AND id NOT IN (SELECT MAX(id) FROM gc_runs GROUP BY scope)",
        )?;

        // 3–4. blobs and tmp under L3 exclusive
        {
            let _x = st.blob_gate.write().unwrap_or_else(|p| p.into_inner());
            self.sweep_blobs()?;
            self.sweep_tmp()?;
        }
        // 5. trash
        self.sweep_trash()?;
        // 6. integrity scan under L3 shared
        let _s = st.gate_shared();
        let next = self.scrub(cursor.clone())?;
        *cursor = next;
        Ok(())
    }

    fn delete_ids(&mut self, ids: &[i64], q: &str) -> Result<u64> {
        let mut total = 0;
        let cutoff = self.cutoff;
        let op = self.op;
        for chunk in ids.chunks(self.batch as usize) {
            let k = self.batch(|c| {
                let mut k = 0u64;
                for id in chunk {
                    k += c.execute(q, params![id, cutoff]).map_err(sqle(op))? as u64;
                }
                Ok(if k == 0 { Tx::Rollback(0) } else { Tx::Commit(k) })
            })?;
            if k > 0 {
                total += k;
                self.committed();
            }
        }
        Ok(total)
    }

    fn delete_sessions(&mut self, r: &[String]) -> Result<()> {
        const TABLES: [(&str, &str); 7] = [
            ("overlay_entries", "session"),
            ("checkpoints", "session"),
            ("seeds", "session"),
            ("move_intents", "session"),
            ("dev_ports", "session"),
            ("workspaces", "session"),
            ("sessions", "id"),
        ];
        let (cutoff, batch, op) = (self.cutoff, self.batch as i64, self.op);
        let mut skipped: BTreeSet<&str> = BTreeSet::new();
        let check = format!("SELECT EXISTS (SELECT 1 FROM sessions s WHERE s.id = ?1 AND {SESSION_GONE})");
        for (table, col) in TABLES {
            let del = format!("DELETE FROM {table} WHERE rowid IN (SELECT rowid FROM {table} WHERE {col} = ?1 LIMIT ?2)");
            for sid in r {
                if skipped.contains(sid.as_str()) {
                    continue;
                }
                loop {
                    let k = self.batch(|c| {
                        let ok: bool = c.query_row(&check, params![sid, cutoff], |r| r.get(0)).map_err(sqle(op))?;
                        if !ok {
                            return Ok(Tx::Rollback(None));
                        }
                        let k = c.execute(&del, params![sid, batch]).map_err(sqle(op))? as u64;
                        Ok(if k == 0 { Tx::Rollback(Some(0)) } else { Tx::Commit(Some(k)) })
                    })?;
                    let Some(k) = k else {
                        skipped.insert(sid.as_str());
                        break;
                    };
                    if k == 0 {
                        break;
                    }
                    match table {
                        "overlay_entries" => self.report.overlay_entries += k,
                        "checkpoints" => self.report.checkpoints += k,
                        "sessions" => self.report.sessions += k,
                        _ => {}
                    }
                    self.committed();
                    if (k as i64) < batch {
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    /// Delete D from the superseded end first, so no batch's COMMIT trips
    /// the deferred `superseded_by` foreign key.
    fn delete_proposals(&mut self, d: &BTreeSet<i64>) -> Result<()> {
        let (cutoff, batch, op) = (self.cutoff, self.batch as usize, self.op);
        loop {
            let k = self.batch(|c| {
                let (all, now_d) = proposal_set(c, cutoff)?;
                let cand: BTreeSet<i64> = now_d.intersection(d).copied().collect();
                if cand.is_empty() {
                    return Ok(Tx::Rollback(0u64));
                }
                let mut referrers: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
                for (y, by) in &all {
                    if let Some(x) = by {
                        referrers.entry(*x).or_default().push(*y);
                    }
                }
                let mut picked: BTreeSet<i64> = BTreeSet::new();
                loop {
                    let mut progress = false;
                    for &x in &cand {
                        if picked.len() >= batch {
                            break;
                        }
                        if picked.contains(&x) {
                            continue;
                        }
                        if referrers.get(&x).is_none_or(|ys| ys.iter().all(|y| picked.contains(y))) {
                            picked.insert(x);
                            progress = true;
                        }
                    }
                    if !progress || picked.len() >= batch {
                        break;
                    }
                }
                if picked.is_empty() {
                    // a cycle: by the closure every referrer is in `cand`
                    picked = cand;
                }
                for x in &picked {
                    c.execute("DELETE FROM deps_proposals WHERE id = ?1", [x]).map_err(sqle(op))?;
                }
                Ok(Tx::Commit(picked.len() as u64))
            })?;
            if k == 0 {
                return Ok(());
            }
            self.report.proposals += k;
            self.committed();
        }
    }

    fn unlink(&mut self, rel: &RepoPath) -> bool {
        match strata_apfs::remove_rooted(&self.st.real_root, rel, RemoveKind::File) {
            Ok(()) => true,
            Err(e) if e.code == Code::NoSuchPath => true,
            Err(_) => {
                let p = self.shown(rel);
                self.report.stuck.push(p);
                false
            }
        }
    }

    fn sweep_blobs(&mut self) -> Result<()> {
        let st = self.st;
        let op = self.op;
        // (i) rows nothing references
        let rows: Vec<(String, i64)> = st.read(|c| {
            let mut q = c
                .prepare(
                    "SELECT id, size FROM blobs b WHERE NOT EXISTS (SELECT 1 FROM overlay_entries o WHERE o.blob = b.id) ORDER BY id",
                )
                .map_err(sql_read)?;
            q.query_map((), |r| Ok((r.get(0)?, r.get(1)?))).map_err(sql_read)?.collect::<crate::sql::SqlResult<Vec<_>>>().map_err(sql_read)
        })?;
        let mut doomed = Vec::new();
        for (id, size) in rows {
            let rel = rp(format!("store/blobs/{}/{id}", &id[..2]));
            match strata_apfs::attrs_of(&st.real_root, &rel) {
                Ok(a) if self.old(&a) => doomed.push((id, size, Some(rel))),
                Ok(_) => {}
                Err(e) if e.code == Code::NoSuchPath => doomed.push((id, size, None)),
                Err(e) => return Err(e),
            }
        }
        for chunk in doomed.chunks(self.batch as usize) {
            let gone = self.batch(|c| {
                let mut out = Vec::new();
                for (id, size, rel) in chunk {
                    let k = c
                        .execute("DELETE FROM blobs WHERE id = ?1 AND NOT EXISTS (SELECT 1 FROM overlay_entries WHERE blob = ?1)", [id])
                        .map_err(sqle(op))?;
                    if k == 1 {
                        out.push((*size, rel.clone()));
                    }
                }
                Ok(if out.is_empty() { Tx::Rollback(out) } else { Tx::Commit(out) })
            })?;
            if gone.is_empty() {
                continue;
            }
            self.committed();
            for (size, rel) in gone {
                self.report.blobs += 1;
                self.report.blob_bytes += size.max(0) as u64;
                if let Some(rel) = rel {
                    self.unlink(&rel);
                }
            }
        }
        // (ii) files without a row
        let blobs = rp("store/blobs");
        if !self.real_dir(&blobs)? {
            return Ok(());
        }
        for (fan, fa) in self.list(&blobs)? {
            let fname = last_name(&fan).to_owned();
            if !matches!(fa.kind, FileKind::Dir) || fname.len() != 2 {
                continue;
            }
            for (p, a) in self.list(&fan)? {
                let name = last_name(&p).to_owned();
                if !matches!(a.kind, FileKind::File) || !name.starts_with(&fname) || BlobId::try_from(name.clone()).is_err() {
                    continue;
                }
                if !self.old(&a) {
                    continue;
                }
                let has_row: bool =
                    st.read(|c| c.query_row("SELECT EXISTS (SELECT 1 FROM blobs WHERE id = ?1)", [&name], |r| r.get(0)).map_err(sql_read))?;
                if has_row {
                    continue;
                }
                if self.unlink(&p) {
                    self.report.blobs += 1;
                    self.report.blob_bytes += a.size;
                }
            }
        }
        Ok(())
    }

    fn sweep_tmp(&mut self) -> Result<()> {
        let tmp = rp("store/tmp");
        if !self.real_dir(&tmp)? {
            return Ok(());
        }
        for (p, a) in self.list(&tmp)? {
            if !self.old(&a) {
                continue;
            }
            match strata_apfs::remove_tree_rooted(&self.st.real_root, &p) {
                Ok(r) if r.is_complete() => self.report.tmp_removed += 1,
                _ => {
                    let s = self.shown(&p);
                    self.report.stuck.push(s);
                }
            }
        }
        Ok(())
    }

    fn sweep_trash(&mut self) -> Result<()> {
        let trash = rp("store/trash");
        if !self.real_dir(&trash)? {
            return Ok(());
        }
        for (p, a) in self.list(&trash)? {
            let named = last_name(&p).split_once('-').and_then(|(ms, _)| ms.parse::<u128>().ok());
            let old = match named {
                Some(ms) => now_ms().saturating_sub(ms) >= self.floor.as_millis(),
                None => self.old(&a),
            };
            if !old {
                continue;
            }
            // The same protection as `gc_trash` (review GS-09): an entry
            // a row or a git link still names stays.
            if self.st.trash_named(&self.refs, &p) {
                let s = self.shown(&p);
                self.report.trash_refused.push(s);
                continue;
            }
            let fails = self.st.seams.take(|s| match s {
                Seam::GcUnlinkFails { errno } => Some(*errno),
                _ => None,
            });
            let complete = fails.is_none()
                && matches!(strata_apfs::remove_tree_rooted(&self.st.real_root, &p), Ok(r) if r.is_complete());
            if complete {
                self.report.trash_removed += 1;
            } else {
                let s = self.shown(&p);
                self.report.stuck.push(s);
            }
        }
        Ok(())
    }

    /// Verify blobs after `after` until the budget is spent; the next
    /// cursor, `None` once the end is reached (the next run wraps).
    fn scrub(&mut self, after: Option<String>) -> Result<Option<String>> {
        let st = self.st;
        let budget = st.seams.scrub_budget().unwrap_or(DEFAULT_SCRUB_BYTES);
        let from = after.unwrap_or_default();
        let rows: Vec<(String, i64)> = st.read(|c| {
            let mut q = c.prepare("SELECT id, size FROM blobs WHERE id > ?1 ORDER BY id").map_err(sql_read)?;
            q.query_map([&from], |r| Ok((r.get(0)?, r.get(1)?))).map_err(sql_read)?.collect::<crate::sql::SqlResult<Vec<_>>>().map_err(sql_read)
        })?;
        let mut used = 0u64;
        for (i, (id, size)) in rows.iter().enumerate() {
            let Ok(bid) = BlobId::try_from(id.clone()) else { continue };
            match st.verify_locked(&bid) {
                Ok(()) => {}
                Err(e) if e.code == Code::Corrupt => self.report.corrupt.push(bid),
                Err(e) => return Err(e),
            }
            self.report.verified += 1;
            used = used.saturating_add((*size).max(0) as u64);
            if used >= budget && i + 1 < rows.len() {
                return Ok(Some(id.clone()));
            }
        }
        Ok(None)
    }

    // ------------------------------------------------------------- gc_repo

    /// Prepare `rel` for its rename: clear flags and add owner write on a
    /// directory root. `None` = preparation failed (the path is `stuck`).
    fn prepare(&mut self, rel: &RepoPath) -> Option<Prep> {
        let st = self.st;
        let a = match strata_apfs::attrs_of(&st.real_root, rel) {
            Ok(a) => a,
            Err(e) if e.code == Code::NoSuchPath => return Some(Prep::RowOnly),
            Err(_) => {
                let s = self.shown(rel);
                self.report.stuck.push(s);
                return None;
            }
        };
        if !matches!(a.kind, FileKind::Dir) {
            return Some(Prep::Entry);
        }
        let ok = (|| -> Result<()> {
            let f = strata_apfs::open_rooted(&st.real_root, rel)?;
            if let Some(n) = st.seams.take(|s| match s {
                Seam::RetirePrepFails { errno } => Some(*errno),
                _ => None,
            }) {
                return Err(errno(n, IoSide::Write, &self.shown(rel), self.op, NOT_WRITTEN));
            }
            strata_apfs::make_removable(&f)
        })();
        match ok {
            Ok(()) => Some(Prep::Entry),
            Err(e) if e.code == Code::NoSuchPath => Some(Prep::RowOnly),
            Err(_) => {
                let s = self.shown(rel);
                self.report.stuck.push(s);
                None
            }
        }
    }

    /// Rename `rel` into `store/trash/<ms>-<rand>`, then F_FULLFSYNC trash.
    fn retire(&mut self, rel: &RepoPath) -> Result<()> {
        let st = self.st;
        let to = rp(format!("store/trash/{}-{}", now_ms(), hex32()));
        match strata_apfs::rename_rooted(&st.real_root, rel, &to, RenameMode::Excl) {
            Ok(()) => {}
            Err(e) if e.code == Code::NoSuchPath => return Ok(()),
            Err(e) => return Err(annotate(e, self.op, PARTIAL)),
        }
        let t = strata_apfs::open_rooted(&st.real_root, &rp("store/trash")).map_err(|e| annotate(e, self.op, PARTIAL))?;
        strata_apfs::full_fsync(&t).map_err(|e| annotate(e, self.op, PARTIAL))?;
        st.seams.dir_barriers.fetch_add(1, Ordering::SeqCst);
        self.report.dirs_retired += 1;
        let n = self.report.dirs_retired;
        if st.seams.take(|s| matches!(s, Seam::GcStopAfterRetire { n: k } if *k == n).then_some(())).is_some() {
            return Err(errno(libc::EIO, IoSide::Write, &self.shown(rel), self.op, PARTIAL));
        }
        Ok(())
    }

    fn prep_and_retire(&mut self, rel: &RepoPath) -> Result<()> {
        match self.prepare(rel) {
            Some(Prep::Entry) => self.retire(rel),
            _ => Ok(()),
        }
    }

    fn repo_body(&mut self, repo: &RepoName) -> Result<()> {
        let st = self.st;
        let (r, cutoff, op) = (repo.as_str(), self.cutoff, self.op);
        // 1. pins and candidates, from one snapshot
        type Gen = (i64, String, String);
        let (guard, gens, cands, deps_cands) = st.read(|c| {
            let guard = st.owned_paths(c)?;
            let mut q = c.prepare("SELECT id, tree, deps, created_ns FROM base_gens WHERE repo = ?1 ORDER BY id").map_err(sql_read)?;
            let all = q
                .query_map([r], |x| Ok((x.get::<_, i64>(0)?, x.get::<_, String>(1)?, x.get::<_, String>(2)?, x.get::<_, i64>(3)?)))
                .map_err(sql_read)?
                .collect::<crate::sql::SqlResult<Vec<_>>>()
                .map_err(sql_read)?;
            let mut gens: Vec<Gen> = Vec::new();
            let mut cands: Vec<Gen> = Vec::new();
            for (id, tree, deps, created) in all {
                let pinned: bool = c.query_row(GEN_PINNED, [id], |x| x.get(0)).map_err(sql_read)?;
                if !pinned && created < cutoff {
                    cands.push((id, tree.clone(), deps.clone()));
                }
                gens.push((id, tree, deps));
            }
            let mut q = c.prepare("SELECT id FROM deps_sets WHERE repo = ?1 AND sealed_ns < ?2 ORDER BY id").map_err(sql_read)?;
            let mut deps_cands = Vec::new();
            for id in q.query_map(params![r, cutoff], |x| x.get::<_, String>(0)).map_err(sql_read)? {
                let id = id.map_err(sql_read)?;
                let other: bool = c.query_row(DEPS_PINNED_OTHER, params![r, id], |x| x.get(0)).map_err(sql_read)?;
                if !other {
                    deps_cands.push(id);
                }
            }
            Ok((guard, gens, cands, deps_cands))
        })?;
        let guarded = |rel: &RepoPath| guarded(&guard, rel);

        // 2. prepare generation roots
        let mut prepared: Vec<(i64, Option<RepoPath>)> = Vec::new();
        for (id, tree, deps) in &cands {
            let rel = rp(format!("bases/{r}/{tree}/{deps}"));
            if guarded(&rel) {
                continue;
            }
            match self.prepare(&rel) {
                Some(Prep::RowOnly) => prepared.push((*id, None)),
                Some(Prep::Entry) => prepared.push((*id, Some(rel))),
                None => {}
            }
        }
        let going: BTreeSet<i64> = prepared.iter().map(|(id, _)| *id).collect();
        let kept_deps: BTreeSet<&str> = gens.iter().filter(|(id, _, _)| !going.contains(id)).map(|(_, _, d)| d.as_str()).collect();
        let mut prepared_deps: Vec<(String, Option<RepoPath>)> = Vec::new();
        for id in &deps_cands {
            if kept_deps.contains(id.as_str()) {
                continue;
            }
            let rel = rp(format!("deps/{r}/{id}"));
            if guarded(&rel) {
                continue;
            }
            match self.prepare(&rel) {
                Some(Prep::RowOnly) => prepared_deps.push((id.clone(), None)),
                Some(Prep::Entry) => prepared_deps.push((id.clone(), Some(rel))),
                None => {}
            }
        }
        st.seams.pause_if(&Seam::GcPauseBeforeRows);

        // 3. rows, re-checking the pins in each batch
        let mut retire: Vec<RepoPath> = Vec::new();
        for chunk in prepared.chunks(self.batch as usize) {
            let gone = self.batch(|c| {
                let mut out = Vec::new();
                for (id, rel) in chunk {
                    let pinned: bool = c.query_row(GEN_PINNED, [id], |x| x.get(0)).map_err(sqle(op))?;
                    if pinned {
                        continue;
                    }
                    if c.execute("DELETE FROM base_gens WHERE id = ?1", [id]).map_err(sqle(op))? == 1 {
                        out.push(rel.clone());
                    }
                }
                Ok(if out.is_empty() { Tx::Rollback(out) } else { Tx::Commit(out) })
            })?;
            if gone.is_empty() {
                continue;
            }
            self.report.base_gens += gone.len() as u64;
            self.committed();
            retire.extend(gone.into_iter().flatten());
        }
        let trees: Vec<String> = st.read(|c| {
            let mut q = c
                .prepare(
                    "SELECT tree FROM base_trees t WHERE repo = ?1 AND created_ns < ?2 \
                     AND NOT EXISTS (SELECT 1 FROM base_gens g WHERE g.repo = t.repo AND g.tree = t.tree) ORDER BY tree",
                )
                .map_err(sql_read)?;
            q.query_map(params![r, cutoff], |x| x.get(0)).map_err(sql_read)?.collect::<crate::sql::SqlResult<Vec<_>>>().map_err(sql_read)
        })?;
        let batch = self.batch as i64;
        for tree in &trees {
            loop {
                let step = self.batch(|c| {
                    let used: bool = c
                        .query_row("SELECT EXISTS (SELECT 1 FROM base_gens WHERE repo = ?1 AND tree = ?2)", params![r, tree], |x| x.get(0))
                        .map_err(sqle(op))?;
                    if used {
                        return Ok(Tx::Rollback(None));
                    }
                    let k = c
                        .execute(
                            "DELETE FROM manifest_entries WHERE rowid IN \
                             (SELECT rowid FROM manifest_entries WHERE repo = ?1 AND tree = ?2 LIMIT ?3)",
                            params![r, tree, batch],
                        )
                        .map_err(sqle(op))?;
                    if k > 0 {
                        return Ok(Tx::Commit(Some(false)));
                    }
                    let k = c.execute("DELETE FROM base_trees WHERE repo = ?1 AND tree = ?2", params![r, tree]).map_err(sqle(op))?;
                    Ok(if k == 1 { Tx::Commit(Some(true)) } else { Tx::Rollback(None) })
                })?;
                match step {
                    None => break,
                    Some(done) => {
                        self.committed();
                        if done {
                            self.report.base_trees += 1;
                            break;
                        }
                    }
                }
            }
        }
        for chunk in prepared_deps.chunks(self.batch as usize) {
            let gone = self.batch(|c| {
                let mut out = Vec::new();
                for (id, rel) in chunk {
                    let other: bool = c.query_row(DEPS_PINNED_OTHER, params![r, id], |x| x.get(0)).map_err(sqle(op))?;
                    let used: bool = c
                        .query_row("SELECT EXISTS (SELECT 1 FROM base_gens WHERE repo = ?1 AND deps = ?2)", params![r, id], |x| x.get(0))
                        .map_err(sqle(op))?;
                    if other || used {
                        continue;
                    }
                    if c.execute("DELETE FROM deps_sets WHERE repo = ?1 AND id = ?2", params![r, id]).map_err(sqle(op))? == 1 {
                        out.push(rel.clone());
                    }
                }
                Ok(if out.is_empty() { Tx::Rollback(out) } else { Tx::Commit(out) })
            })?;
            if gone.is_empty() {
                continue;
            }
            self.report.deps_sets += gone.len() as u64;
            self.committed();
            retire.extend(gone.into_iter().flatten());
        }

        // 4. rename into trash
        for rel in &retire {
            self.retire(rel)?;
        }

        // 5. sweep unregistered directories on live paths
        self.sweep_repo(repo, &guard)?;
        Ok(())
    }

    fn sweep_repo(&mut self, repo: &RepoName, guard: &[PathBuf]) -> Result<()> {
        let st = self.st;
        let r = repo.as_str();
        let registered = |q: &str, p: &[&str]| -> Result<bool> {
            st.read(|c| c.query_row(q, &p.iter().map(|x| x as &dyn ToSql).collect::<Vec<_>>()[..], |x| x.get(0)).map_err(sql_read))
        };
        let bases = rp(format!("bases/{r}"));
        if self.real_dir(&bases)? {
            for (p, a) in self.list(&bases)? {
                let name = last_name(&p).to_owned();
                if !matches!(a.kind, FileKind::Dir) {
                    continue;
                }
                if name == ".staging" {
                    for (sp, sa) in self.list(&p)? {
                        if matches!(sa.kind, FileKind::Dir) && hex32_name(last_name(&sp)) && self.old(&sa) && !guarded(guard, &sp) {
                            self.prep_and_retire(&sp)?;
                        }
                    }
                    continue;
                }
                if name.starts_with('.') || TreeOid::try_from(name.clone()).is_err() {
                    continue;
                }
                for (dp, da) in self.list(&p)? {
                    let dn = last_name(&dp).to_owned();
                    if !matches!(da.kind, FileKind::Dir) || DepsSetId::try_from(dn.clone()).is_err() {
                        continue;
                    }
                    if !self.old(&da) || guarded(guard, &dp) {
                        continue;
                    }
                    if registered(
                        "SELECT EXISTS (SELECT 1 FROM base_gens WHERE repo = ?1 AND tree = ?2 AND deps = ?3)",
                        &[r, name.as_str(), dn.as_str()],
                    )? {
                        continue;
                    }
                    self.prep_and_retire(&dp)?;
                }
                if !guarded(guard, &p) && self.list(&p)?.is_empty() {
                    let _ = strata_apfs::remove_rooted(&st.real_root, &p, RemoveKind::EmptyDir);
                }
            }
        }
        let deps = rp(format!("deps/{r}"));
        if self.real_dir(&deps)? {
            for (p, a) in self.list(&deps)? {
                let name = last_name(&p).to_owned();
                if !matches!(a.kind, FileKind::Dir) || DepsSetId::try_from(name.clone()).is_err() {
                    continue;
                }
                if !self.old(&a) || guarded(guard, &p) {
                    continue;
                }
                if registered("SELECT EXISTS (SELECT 1 FROM deps_sets WHERE repo = ?1 AND id = ?2)", &[r, name.as_str()])? {
                    continue;
                }
                self.prep_and_retire(&p)?;
            }
        }
        Ok(())
    }
}

/// Guard (a): `rel` is, contains, or lies inside a path an open intent or a
/// running/failed attempt names.
fn guarded(guard: &[PathBuf], rel: &RepoPath) -> bool {
    let p = Path::new(rel.as_str());
    guard.iter().any(|g| g.starts_with(p) || p.starts_with(g))
}

fn staging_gone(p: &Path) -> bool {
    matches!(std::fs::symlink_metadata(p), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
}

/// Step 1 of `gc` (§5.11): removable sessions R, proposals D, closed
/// attempts and old gc records past the cutoff.
#[allow(clippy::type_complexity)]
fn retention(c: &Connection, cutoff: i64) -> Result<(Vec<String>, BTreeSet<i64>, Vec<i64>, Vec<i64>)> {
    let r = removable_sessions(c, cutoff)?;
    let (_, d) = proposal_set(c, cutoff)?;
    let mut a = c
        .prepare(
            "SELECT id, state, staging FROM deps_attempts \
             WHERE state IN ('sealed', 'refused', 'failed') AND ended_ns < ?1 ORDER BY id",
        )
        .map_err(sql_read)?;
    let attempts = a
        .query_map([cutoff], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
        .map_err(sql_read)?
        .collect::<crate::sql::SqlResult<Vec<_>>>()
        .map_err(sql_read)?;
    let mut g = c
        .prepare(
            "SELECT id FROM gc_runs WHERE ended_ns < ?1 \
             AND id NOT IN (SELECT MAX(id) FROM gc_runs GROUP BY scope) ORDER BY id",
        )
        .map_err(sql_read)?;
    let runs = g.query_map([cutoff], |r| r.get::<_, i64>(0)).map_err(sql_read)?.collect::<crate::sql::SqlResult<Vec<_>>>().map_err(sql_read)?;
    let attempts: Vec<i64> = attempts
        .into_iter()
        .filter(|(_, state, staging)| state != "failed" || staging_gone(Path::new(staging)))
        .map(|(id, _, _)| id)
        .collect();
    Ok((r, d, attempts, runs))
}

fn removable_sessions(c: &Connection, cutoff: i64) -> Result<Vec<String>> {
    let q = format!("SELECT s.id FROM sessions s WHERE ?1 IS NULL AND {SESSION_GONE} ORDER BY s.id");
    let mut st = c.prepare(&q).map_err(sql_read)?;
    st.query_map(params![Option::<i64>::None, cutoff], |r| r.get(0))
        .map_err(sql_read)?
        .collect::<crate::sql::SqlResult<Vec<String>>>()
        .map_err(sql_read)
}

/// All proposals (id → superseded_by) and D: decided past keep, or
/// AwaitingPublish whose proposer is gone or reclaimable; minus anything a
/// kept row reaches through `superseded_by`.
#[allow(clippy::type_complexity)]
fn proposal_set(c: &Connection, cutoff: i64) -> Result<(BTreeMap<i64, Option<i64>>, BTreeSet<i64>)> {
    let r: BTreeSet<String> = removable_sessions(c, cutoff)?.into_iter().collect();
    let mut q = c.prepare("SELECT id FROM sessions").map_err(sql_read)?;
    let live: BTreeSet<String> =
        q.query_map((), |x| x.get(0)).map_err(sql_read)?.collect::<crate::sql::SqlResult<BTreeSet<String>>>().map_err(sql_read)?;
    let mut q = c.prepare("SELECT id, state, origin_session, superseded_by, updated_ns FROM deps_proposals").map_err(sql_read)?;
    let rows = q
        .query_map((), |x| {
            Ok((x.get::<_, i64>(0)?, x.get::<_, String>(1)?, x.get::<_, Option<String>>(2)?, x.get::<_, Option<i64>>(3)?, x.get::<_, i64>(4)?))
        })
        .map_err(sql_read)?
        .collect::<crate::sql::SqlResult<Vec<_>>>()
        .map_err(sql_read)?;
    let mut all = BTreeMap::new();
    let mut d = BTreeSet::new();
    for (id, state, origin, by, updated) in rows {
        all.insert(id, by);
        let decided = matches!(state.as_str(), "applied" | "superseded") && updated < cutoff;
        let orphaned = state == "awaiting_publish"
            && origin.as_ref().is_some_and(|o| r.contains(o) || !live.contains(o));
        if decided || orphaned {
            d.insert(id);
        }
    }
    loop {
        let drop: Vec<i64> = all
            .iter()
            .filter(|(y, _)| !d.contains(*y))
            .filter_map(|(_, by)| *by)
            .filter(|x| d.contains(x))
            .collect();
        if drop.is_empty() {
            break;
        }
        for x in drop {
            d.remove(&x);
        }
    }
    Ok((all, d))
}

impl Store {
    /// Paths open intents and running/failed attempts name, relative to the
    /// root (guard (a)).
    fn owned_paths(&self, c: &Connection) -> Result<Vec<PathBuf>> {
        let mut q = c
            .prepare(
                "SELECT staging FROM move_intents WHERE state IN ('preparing', 'swapping', 'unknown') \
                 UNION ALL SELECT staging FROM deps_attempts WHERE state IN ('running', 'failed')",
            )
            .map_err(sql_read)?;
        let rows = q.query_map((), |x| x.get::<_, String>(0)).map_err(sql_read)?.collect::<crate::sql::SqlResult<Vec<_>>>().map_err(sql_read)?;
        Ok(rows
            .into_iter()
            .filter_map(|s| {
                let p = PathBuf::from(s);
                p.strip_prefix(&self.root).or_else(|_| p.strip_prefix(&self.real_root)).ok().map(Path::to_path_buf)
            })
            .collect())
    }

    /// Reclaim what nothing references any more (§5.11): removed sessions
    /// past `keep`, decided proposals, closed attempts, old gc records,
    /// unreferenced blobs, stale temp entries and trash; then a budgeted
    /// integrity scan of the blobs.
    pub fn gc(&self, keep: Duration) -> Result<GcReport> {
        self.gc_with(keep, &[])
    }

    /// [`Store::gc`] whose trash step also spares what `extra` names (the
    /// git links the daemon knows; store.md §5.11a). Trash entries a row
    /// or `extra` names are listed in the report's `trash_refused`.
    pub fn gc_with(&self, keep: Duration, extra: &[PathBuf]) -> Result<GcReport> {
        self.writable()?;
        let _one = self.gc_running.lock().unwrap_or_else(|p| p.into_inner());
        let mut run = Run::new(self, OP_GC, None, keep)?;
        run.refs = self.trash_refs(extra)?;
        let mut cursor: Option<String> = None;
        let res = self
            .read(|c| {
                c.query_row("SELECT scrub_after FROM gc_runs WHERE scope IS NULL ORDER BY id DESC LIMIT 1", (), |r| r.get(0))
                    .optional()
                    .map_err(sql_read)
            })
            .map(|v: Option<Option<String>>| cursor = v.flatten())
            .and_then(|()| run.gc_body(&mut cursor));
        run.finish(res, cursor)
    }

    /// What [`Store::gc_with`] would reclaim now, writing nothing (the
    /// `gc --store --dry-run` preview; store.md §5.11b). Counts are read
    /// from one snapshot and are an upper bound: each batch re-checks.
    /// Blobs count those no kept session's entry references whose file is
    /// missing or past the age floor; trash follows `gc_trash`'s rules.
    pub fn gc_preview(&self, keep: Duration, extra: &[PathBuf]) -> Result<GcPreview> {
        let run = Run::new(self, OP_GC, None, keep)?;
        let cutoff = run.cutoff;
        let (r, d, attempts, runs) = self.read(|c| retention(c, cutoff))?;
        let rows: Vec<(String, i64)> = self.read(|c| {
            let q = format!(
                "SELECT b.id, b.size FROM blobs b WHERE NOT EXISTS (SELECT 1 FROM overlay_entries o WHERE o.blob = b.id \
                 AND o.session NOT IN (SELECT s.id FROM sessions s WHERE ?1 IS NULL AND {SESSION_GONE})) ORDER BY b.id"
            );
            let mut q = c.prepare(&q).map_err(sql_read)?;
            q.query_map(params![Option::<i64>::None, cutoff], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(sql_read)?
                .collect::<crate::sql::SqlResult<Vec<_>>>()
                .map_err(sql_read)
        })?;
        let mut out = GcPreview {
            keep_secs: keep.as_secs(),
            floor_secs: run.floor.as_secs(),
            sessions: r,
            proposals: d.len() as u64,
            attempts: attempts.len() as u64,
            gc_runs: runs.len() as u64,
            ..GcPreview::default()
        };
        for (id, size) in rows {
            let rel = rp(format!("store/blobs/{}/{id}", &id[..2]));
            let gone = match strata_apfs::attrs_of(&self.real_root, &rel) {
                Ok(a) => run.old(&a),
                Err(e) if e.code == Code::NoSuchPath => true,
                Err(e) => return Err(e),
            };
            if gone {
                out.blobs += 1;
                out.blob_bytes += size.max(0) as u64;
            }
        }
        let refs = self.trash_refs(extra)?;
        let trash = rp("store/trash");
        if run.real_dir(&trash)? {
            for (p, a) in run.list(&trash)? {
                let named = last_name(&p).split_once('-').and_then(|(ms, _)| ms.parse::<u128>().ok());
                let old = match named {
                    Some(ms) => now_ms().saturating_sub(ms) >= run.floor.as_millis(),
                    None => run.old(&a),
                };
                if !old {
                    out.trash_young += 1;
                } else if self.trash_named(&refs, &p) {
                    out.trash_refused.push(self.root.join(p.as_str()));
                } else {
                    out.trash_eligible += 1;
                }
            }
        }
        Ok(out)
    }

    /// Reclaim one repo's base generations and deps sets past `keep` that
    /// nothing pins, through `store/trash/` (§5.11).
    pub fn gc_repo(&self, repo: &RepoName, keep: Duration) -> Result<GcReport> {
        self.writable()?;
        let _one = self.gc_running.lock().unwrap_or_else(|p| p.into_inner());
        let mut run = Run::new(self, OP_REPO, Some(repo.clone()), keep)?;
        let res = run.repo_body(repo);
        run.finish(res, None)
    }

    /// Empty `store/trash/` and nothing else (store.md §5.11a): every entry
    /// older than the age floor that no row and no `extra` path names. A
    /// path names an entry when it, or its canonical form, lies inside the
    /// entry; the rows read are every `sessions.root`, `workspaces.path`,
    /// `deps_attempts.staging` and `move_intents.staging`, whatever their
    /// state. `extra` carries what the caller knows and the store does not
    /// (alternates and gitdir links of live shadow repos). No gc_runs row:
    /// the global `gc`'s scrub cursor stays where it was.
    pub fn gc_trash(&self, extra: &[PathBuf]) -> Result<TrashReport> {
        self.writable()?;
        let _one = self.gc_running.lock().unwrap_or_else(|p| p.into_inner());
        let run = Run::new(self, "gc_trash", None, Duration::ZERO)?;
        let refs = self.trash_refs(extra)?;
        let mut report = TrashReport::default();
        let trash = rp("store/trash");
        if !run.real_dir(&trash)? {
            return Ok(report);
        }
        for (p, a) in run.list(&trash)? {
            let named = last_name(&p).split_once('-').and_then(|(ms, _)| ms.parse::<u128>().ok());
            let old = match named {
                Some(ms) => now_ms().saturating_sub(ms) >= run.floor.as_millis(),
                None => run.old(&a),
            };
            if !old {
                report.young += 1;
                continue;
            }
            let shown = self.root.join(p.as_str());
            if self.trash_named(&refs, &p) {
                report.refused.push(shown);
                continue;
            }
            let fails = self.seams.take(|s| match s {
                Seam::GcUnlinkFails { errno } => Some(*errno),
                _ => None,
            });
            let complete =
                fails.is_none() && matches!(strata_apfs::remove_tree_rooted(&self.real_root, &p), Ok(r) if r.is_complete());
            if complete {
                report.removed += 1;
            } else {
                report.stuck.push(shown);
            }
        }
        Ok(report)
    }

    /// The trash reference set (§5.11a): every `sessions.root`,
    /// `workspaces.path`, `deps_attempts.staging` and `move_intents.staging`,
    /// plus `extra`, each also in its canonical form.
    fn trash_refs(&self, extra: &[PathBuf]) -> Result<Vec<PathBuf>> {
        let mut refs: Vec<PathBuf> = self.read(|c| {
            let mut q = c
                .prepare(
                    "SELECT root FROM sessions UNION SELECT path FROM workspaces \
                     UNION SELECT staging FROM deps_attempts UNION SELECT staging FROM move_intents",
                )
                .map_err(sql_read)?;
            q.query_map((), |r| r.get::<_, String>(0))
                .map_err(sql_read)?
                .collect::<crate::sql::SqlResult<Vec<_>>>()
                .map_err(sql_read)
        })?
        .into_iter()
        .map(PathBuf::from)
        .collect();
        refs.extend(extra.iter().cloned());
        let canon: Vec<PathBuf> = refs.iter().filter_map(|p| std::fs::canonicalize(p).ok()).collect();
        refs.extend(canon);
        Ok(refs)
    }

    /// Some reference lies inside the trash entry `rel` (or is it).
    fn trash_named(&self, refs: &[PathBuf], rel: &RepoPath) -> bool {
        let shown = self.root.join(rel.as_str());
        let real = self.real_root.join(rel.as_str());
        refs.iter().any(|r| r.starts_with(&shown) || r.starts_with(&real))
    }

    /// The newest gc report of `scope` (`None` = the global `gc`).
    pub fn last_gc_report(&self, scope: Option<&RepoName>) -> Result<Option<GcReport>> {
        let s = scope.map(|r| r.as_str().to_owned());
        let text: Option<String> = self.read(|c| {
            c.query_row("SELECT report FROM gc_runs WHERE scope IS ?1 ORDER BY id DESC LIMIT 1", [s.clone()], |r| r.get(0))
                .optional()
                .map_err(sql_read)
        })?;
        text.map(|t| from_json(&t, "gc_runs.report")).transpose()
    }
}
