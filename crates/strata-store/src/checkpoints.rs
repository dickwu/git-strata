//! Checkpoint records (store.md §5.8).

use std::collections::{BTreeMap, BTreeSet};
use std::io;

use crate::sql::{Connection, OptionalExtension, params};
use serde_json::json;
use strata_apfs::IoSide;
use strata_types::{
    BaseGen, BlobId, Checkpoint, CheckpointId, CheckpointReason, Code, EntryKind, OverlayEntry, RepoName, RepoPath,
    SessionId, StrataError,
};

use crate::codec::*;
use crate::errors::{NOT_WRITTEN, bad_row, corrupt, err, errno, invalid, io as io_err, no_such_repo, no_such_session, sql, sql_read, too_large};
use crate::seams::Seam;
use crate::sessions::{base_gen_id, session_lifecycle, workspace};
use crate::txn::Tx;
use crate::{CheckpointInfo, Result, Store};

const OP: &str = "record_checkpoint";
const MODES: [u32; 3] = [0o100644, 0o100755, 0o120000];

fn w(e: crate::sql::Error) -> StrataError {
    sql(e, OP, NOT_WRITTEN)
}

fn reason_cols(r: &CheckpointReason) -> (&'static str, Option<&str>) {
    match r {
        CheckpointReason::ToolCall { id } => ("tool_call", Some(id)),
        CheckpointReason::Manual { message } => ("manual", Some(message)),
        CheckpointReason::BeforePublish => ("before_publish", None),
        CheckpointReason::BeforeRestore => ("before_restore", None),
        CheckpointReason::BeforeRollout => ("before_rollout", None),
        CheckpointReason::BeforeRemove => ("before_remove", None),
    }
}

fn reason_of(kind: &str, text: Option<String>) -> Result<CheckpointReason> {
    Ok(match (kind, text) {
        ("tool_call", Some(id)) => CheckpointReason::ToolCall { id },
        ("manual", Some(message)) => CheckpointReason::Manual { message },
        ("before_publish", None) => CheckpointReason::BeforePublish,
        ("before_restore", None) => CheckpointReason::BeforeRestore,
        ("before_rollout", None) => CheckpointReason::BeforeRollout,
        ("before_remove", None) => CheckpointReason::BeforeRemove,
        (k, _) => return Err(bad_row(format!("checkpoint reason {k:?} does not match its reason_text"))),
    })
}

fn kind_cols(k: &EntryKind) -> (&'static str, Option<&BlobId>, Option<&RepoPath>) {
    match k {
        EntryKind::Added { blob } => ("added", Some(blob), None),
        EntryKind::Modified { blob } => ("modified", Some(blob), None),
        EntryKind::Deleted => ("deleted", None, None),
        EntryKind::Renamed { from, blob } => ("renamed", Some(blob), Some(from)),
        EntryKind::Untracked { blob } => ("untracked", Some(blob), None),
    }
}

fn entry_invalid(path: &RepoPath, why: &str, msg: String) -> StrataError {
    err(Code::InvalidInput, msg, json!({ "reason": "entries", "why": why, "path": path.as_str() }))
}

/// Step 1: the entries must describe one coherent overlay.
fn validate(cp: &Checkpoint) -> Result<()> {
    let mut paths = BTreeSet::new();
    for e in &cp.entries {
        if !paths.insert(e.path.as_str()) {
            return Err(entry_invalid(&e.path, "duplicate_path", format!("two entries for {}", e.path.as_str())));
        }
    }
    let mut froms = BTreeSet::new();
    for e in &cp.entries {
        let (kind, blob, from) = kind_cols(&e.kind);
        if let Some(from) = from {
            if from == &e.path {
                return Err(entry_invalid(&e.path, "rename_to_self", format!("{} is renamed from itself", e.path.as_str())));
            }
            if paths.contains(from.as_str()) {
                return Err(entry_invalid(
                    &e.path,
                    "rename_source_has_entry",
                    format!("rename source {} also has its own entry", from.as_str()),
                ));
            }
            if !froms.insert(from.as_str()) {
                return Err(entry_invalid(&e.path, "duplicate_rename_source", format!("{} is renamed twice", from.as_str())));
            }
        }
        let ok = match blob {
            None => e.mode == 0,
            Some(_) => MODES.contains(&e.mode),
        };
        if !ok {
            return Err(entry_invalid(&e.path, "mode", format!("mode {:o} does not fit a {kind} entry", e.mode)));
        }
    }
    Ok(())
}

struct Head {
    id: i64,
    repo: String,
    tree: String,
    deps: String,
    at: i64,
    reason: String,
    reason_text: Option<String>,
    entries: i64,
}

const HEAD_SQL: &str = "SELECT c.id, c.repo, b.tree, b.deps, c.at_ns, c.reason, c.reason_text, c.entries \
     FROM checkpoints c JOIN base_gens b ON b.repo = c.repo AND b.id = c.base_gen";

fn head_of(r: &crate::sql::Row) -> crate::sql::SqlResult<Head> {
    Ok(Head {
        id: r.get(0)?,
        repo: r.get(1)?,
        tree: r.get(2)?,
        deps: r.get(3)?,
        at: r.get(4)?,
        reason: r.get(5)?,
        reason_text: r.get(6)?,
        entries: r.get(7)?,
    })
}

fn info(h: Head) -> Result<CheckpointInfo> {
    let id = seq(h.id, "checkpoint id")?;
    let repo: RepoName = parse(h.repo, "checkpoint repo")?;
    Ok(CheckpointInfo {
        id: CheckpointId::try_from(id).map_err(|_| bad_row(format!("checkpoint id {} out of range", h.id)))?,
        base: BaseGen { repo: repo.clone(), tree: parse(h.tree, "base tree")?, deps: parse(h.deps, "base deps")? },
        repo,
        at: time(h.at),
        reason: reason_of(&h.reason, h.reason_text)?,
        entries: nonneg(h.entries, "checkpoint entries")?,
    })
}

fn entries_of(c: &Connection, session: &str, id: i64) -> Result<Vec<OverlayEntry>> {
    let mut st = c
        .prepare("SELECT path, kind, blob, from_path, mode FROM overlay_entries WHERE session = ?1 AND checkpoint = ?2 ORDER BY path")
        .map_err(sql_read)?;
    let rows = st
        .query_map(params![session, id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, Option<String>>(3)?, r.get::<_, i64>(4)?))
        })
        .map_err(sql_read)?;
    let mut out = Vec::new();
    for row in rows {
        let (path, kind, blob, from, mode) = row.map_err(sql_read)?;
        let blob = opt_parse::<BlobId>(blob, "entry blob")?;
        let from = opt_parse::<RepoPath>(from, "entry from_path")?;
        let kind = match (kind.as_str(), blob, from) {
            ("added", Some(blob), None) => EntryKind::Added { blob },
            ("modified", Some(blob), None) => EntryKind::Modified { blob },
            ("deleted", None, None) => EntryKind::Deleted,
            ("renamed", Some(blob), Some(from)) => EntryKind::Renamed { from, blob },
            ("untracked", Some(blob), None) => EntryKind::Untracked { blob },
            (k, _, _) => return Err(bad_row(format!("overlay entry kind {k:?} does not match its columns"))),
        };
        let mode = u32::try_from(mode).map_err(|_| bad_row(format!("overlay entry mode {mode} out of range")))?;
        out.push(OverlayEntry { path: parse(path, "entry path")?, kind, mode });
    }
    Ok(out)
}

fn full(c: &Connection, session: &SessionId, h: Head) -> Result<Checkpoint> {
    let raw_id = h.id;
    let expected = h.entries;
    let i = info(h)?;
    let entries = entries_of(c, session.as_str(), raw_id)?;
    if entries.len() as i64 != expected {
        return Err(err(
            Code::Corrupt,
            format!("checkpoint {} of {} has {} entry rows, its row says {expected}", raw_id, session.as_str(), entries.len()),
            json!({ "reason": "entries_mismatch", "session": session.as_str(), "checkpoint": raw_id,
                    "expected": expected, "actual": entries.len() }),
        ));
    }
    Ok(Checkpoint { id: i.id, session: session.clone(), repo: i.repo, base: i.base, at: i.at, reason: i.reason, entries })
}

impl Store {
    pub fn record_checkpoint(&self, cp: Checkpoint) -> Result<CheckpointId> {
        // 1. pure validation
        validate(&cp)?;
        let at = ns(cp.at, "at")?;
        // 2.
        self.writable()?;
        // 3. L3 shared, then lstat each distinct blob outside L2.
        let _gate = self.gate_shared();
        let mut sizes: BTreeMap<&str, i64> = BTreeMap::new();
        for e in &cp.entries {
            let Some(blob) = kind_cols(&e.kind).1 else { continue };
            if sizes.contains_key(blob.as_str()) {
                continue;
            }
            let p = self.blob_path(blob);
            let md = match std::fs::symlink_metadata(&p) {
                Ok(md) => md,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    return Err(err(
                        Code::Corrupt,
                        format!("blob {} referenced by {} is not in the store", blob.as_str(), e_path(&e, &cp, blob)),
                        json!({ "reason": "blob_missing", "blob": blob.as_str() }),
                    ));
                }
                Err(e) => return Err(io_err(&e, IoSide::Read, &p, OP, NOT_WRITTEN)),
            };
            if !md.file_type().is_file() {
                return Err(err(
                    Code::Corrupt,
                    format!("blob file {} is not a regular file", p.display()),
                    json!({ "reason": "blob_type", "blob": blob.as_str() }),
                ));
            }
            let size = i64::try_from(md.len()).map_err(|_| corrupt("blob_size", "blob larger than i64::MAX"))?;
            sizes.insert(blob.as_str(), size);
        }
        let session = cp.session.as_str();
        let repo = cp.repo.as_str();
        // 4..9
        self.write(OP, |c| {
            // 5.
            match session_lifecycle(c, session).map_err(w)? {
                None => return Err(no_such_session(session)),
                Some(lc) if lc == "removed" => return Err(no_such_session(session)),
                Some(_) => {}
            }
            let ws = workspace(c, session, repo).map_err(w)?.ok_or_else(|| no_such_repo(session, repo))?;
            if !ws.tracked {
                return Err(invalid("untracked", format!("workspace {session}/{repo} is not tracked")));
            }
            if cp.base.repo != cp.repo {
                return Err(err(
                    Code::InvalidInput,
                    format!("base generation belongs to {}, not {repo}", cp.base.repo.as_str()),
                    json!({ "reason": "base_repo", "repo": repo, "base_repo": cp.base.repo.as_str() }),
                ));
            }
            let base = base_gen_id(c, repo, cp.base.tree.as_str(), cp.base.deps.as_str()).map_err(w)?.ok_or_else(|| {
                err(
                    Code::InvalidInput,
                    format!("base generation {repo}/{}/{} is not registered", cp.base.tree.as_str(), cp.base.deps.as_str()),
                    json!({ "reason": "base_unregistered", "repo": repo }),
                )
            })?;
            let (reason, reason_text) = reason_cols(&cp.reason);
            if reason == "tool_call" {
                let prior: Option<i64> = c
                    .query_row(
                        "SELECT id FROM checkpoints WHERE session = ?1 AND repo = ?2 AND reason = 'tool_call' AND reason_text = ?3",
                        params![session, repo, reason_text],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(w)?;
                if let Some(id) = prior {
                    let id = seq(id, "checkpoint id")?;
                    return Ok(Tx::Rollback(CheckpointId::try_from(id).map_err(|_| bad_row("checkpoint id out of range"))?));
                }
            }
            // 6. blobs, first registration wins
            let created = ns(now(), "now")?;
            for (blob, size) in &sizes {
                c.execute(
                    "INSERT INTO blobs (id, size, created_ns) VALUES (?1, ?2, ?3) ON CONFLICT (id) DO NOTHING",
                    params![blob, size, created],
                )
                .map_err(w)?;
                let have: i64 = c.query_row("SELECT size FROM blobs WHERE id = ?1", [blob], |r| r.get(0)).map_err(w)?;
                if have != *size {
                    return Err(err(
                        Code::Corrupt,
                        format!("blob {blob} is {size} bytes on disk but registered as {have}"),
                        json!({ "reason": "blob_size", "blob": blob, "registered": have, "on_disk": size }),
                    ));
                }
            }
            // 7.
            let top: i64 = c
                .query_row("SELECT COALESCE(MAX(id), 0) FROM checkpoints WHERE session = ?1", [session], |r| r.get(0))
                .map_err(w)?;
            let id = top.max(0).checked_add(1).ok_or_else(|| too_large("checkpoint_id"))?;
            // 8.
            c.execute(
                "INSERT INTO checkpoints (session, id, repo, base_gen, at_ns, reason, reason_text, entries) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![session, id, repo, base, at, reason, reason_text, cp.entries.len() as i64],
            )
            .map_err(w)?;
            if self.seams.hit(&Seam::AfterCheckpointRow) {
                return Err(errno(5, IoSide::Write, &self.db_path(), OP, NOT_WRITTEN));
            }
            let mut ins = c
                .prepare(
                    "INSERT INTO overlay_entries (session, checkpoint, path, kind, blob, from_path, mode) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )
                .map_err(w)?;
            for e in &cp.entries {
                let (kind, blob, from) = kind_cols(&e.kind);
                ins.execute(params![
                    session,
                    id,
                    e.path.as_str(),
                    kind,
                    blob.map(BlobId::as_str),
                    from.map(RepoPath::as_str),
                    e.mode as i64
                ])
                .map_err(w)?;
            }
            Ok(Tx::Commit(CheckpointId::try_from(id as u64).map_err(|_| too_large("checkpoint_id"))?))
        })
    }

    pub fn checkpoint(&self, session: &SessionId, id: CheckpointId) -> Result<Checkpoint> {
        let raw = u64::from(id) as i64;
        self.read(|c| {
            let h = c
                .query_row(&format!("{HEAD_SQL} WHERE c.session = ?1 AND c.id = ?2"), params![session.as_str(), raw], head_of)
                .optional()
                .map_err(sql_read)?
                .ok_or_else(|| {
                    err(
                        Code::NoSuchCheckpoint,
                        format!("no checkpoint {id} in session {}", session.as_str()),
                        json!({ "session": session.as_str(), "checkpoint": u64::from(id) }),
                    )
                })?;
            full(c, session, h)
        })
    }

    pub fn latest_checkpoint(&self, session: &SessionId, repo: &RepoName) -> Result<Option<Checkpoint>> {
        self.read(|c| {
            let h = c
                .query_row(
                    &format!("{HEAD_SQL} WHERE c.session = ?1 AND c.repo = ?2 ORDER BY c.id DESC LIMIT 1"),
                    params![session.as_str(), repo.as_str()],
                    head_of,
                )
                .optional()
                .map_err(sql_read)?;
            h.map(|h| full(c, session, h)).transpose()
        })
    }

    pub fn checkpoints(&self, session: &SessionId, repo: Option<&RepoName>) -> Result<Vec<CheckpointInfo>> {
        self.read(|c| {
            let mut st = c
                .prepare(&format!("{HEAD_SQL} WHERE c.session = ?1 AND (?2 IS NULL OR c.repo = ?2) ORDER BY c.id"))
                .map_err(sql_read)?;
            let rows = st.query_map(params![session.as_str(), repo.map(RepoName::as_str)], head_of).map_err(sql_read)?;
            let mut out = Vec::new();
            for h in rows {
                out.push(info(h.map_err(sql_read)?)?);
            }
            Ok(out)
        })
    }
}

fn e_path(_e: &io::Error, cp: &Checkpoint, blob: &BlobId) -> String {
    cp.entries
        .iter()
        .find(|e| kind_cols(&e.kind).1 == Some(blob))
        .map(|e| e.path.as_str().to_owned())
        .unwrap_or_default()
}
