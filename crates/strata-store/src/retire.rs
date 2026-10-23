//! `retire_dir` (store.md §5.5): the only way a directory leaves a live path.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension};
use serde_json::json;
use strata_apfs::{IoSide, RenameMode};
use strata_types::{Code, DepsSetId, RepoName, RepoPath, SessionId, StrataError, TreeOid};

use crate::errors::{NOT_WRITTEN, err, errno, read_only_handle, sql_read};
use crate::open::hex32;
use crate::seams::Seam;
use crate::sessions::session_lifecycle;
use crate::{Result, Store};

const OP: &str = "retire_dir";

fn shape(path: &Path, why: &str) -> StrataError {
    err(
        Code::InvalidInput,
        format!("{} is not a retirable path: {why}", path.display()),
        json!({ "reason": "path_shape", "path": path.to_string_lossy(), "message": why }),
    )
}

fn refused(path: &Path, row: u32, check: &str) -> StrataError {
    err(
        Code::InvalidInput,
        format!("{} matches row {row} of the retire table, but {check}", path.display()),
        json!({ "reason": "not_retirable", "path": path.to_string_lossy(), "row": row, "check": check }),
    )
}

fn key<T: TryFrom<String>>(s: &str) -> Option<T> {
    T::try_from(s.to_string()).ok()
}

/// Decimal, positive, no leading zeros.
fn positive(s: &str) -> Option<i64> {
    if s.is_empty() || s.starts_with('0') || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<i64>().ok().filter(|n| *n > 0)
}

fn hex32_name(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

enum Row7<'a> {
    Gen(RepoName, TreeOid, DepsSetId),
    Deps(RepoName, DepsSetId),
    Attempt(RepoName, i64),
    SessionDir(SessionId),
    Child(SessionId, &'a str),
    Admin(SessionId, RepoName, i64),
    BaseStaging,
}

fn classify<'a>(parts: &[&'a str]) -> Option<Row7<'a>> {
    Some(match parts {
        ["bases", repo, ".staging", h] => {
            key::<RepoName>(repo)?;
            if !hex32_name(h) {
                return None;
            }
            Row7::BaseStaging
        }
        ["bases", repo, tree, deps] => Row7::Gen(key(repo)?, key(tree)?, key(deps)?),
        ["deps", repo, ".staging", a] => Row7::Attempt(key(repo)?, positive(a)?),
        ["deps", repo, deps] => Row7::Deps(key(repo)?, key(deps)?),
        ["sessions", ".repos", sid] => Row7::SessionDir(key(sid)?),
        ["sessions", sid] => Row7::SessionDir(key(sid)?),
        ["sessions", ".repos", sid, git, "worktrees", m] => {
            Row7::Admin(key(sid)?, key(git.strip_suffix(".git")?)?, positive(m)?)
        }
        ["sessions", sid, name] => Row7::Child(key(sid)?, name),
        _ => return None,
    })
}

fn exists(c: &Connection, q: &str, p: impl rusqlite::Params) -> Result<bool> {
    c.query_row(q, p, |r| r.get(0)).map_err(sql_read)
}

impl Store {
    pub fn retire_dir(&self, path: &Path) -> Result<PathBuf> {
        if self.read_only {
            return Err(read_only_handle());
        }
        // 1. shape of the text
        let text = path.to_str().ok_or_else(|| shape(path, "not UTF-8"))?;
        let root = self.root.to_str().ok_or_else(|| shape(path, "the root is not UTF-8"))?.trim_end_matches('/');
        let rel = text
            .strip_prefix(root)
            .and_then(|r| r.strip_prefix('/'))
            .ok_or_else(|| shape(path, "not under the strata root"))?;
        if rel.split('/').any(|c| c.is_empty() || c == "." || c == "..") {
            return Err(shape(path, "empty, '.' or '..' component"));
        }
        let rel: RepoPath = RepoPath::try_from(rel.to_string()).map_err(|_| shape(path, "not a valid relative path"))?;
        if rel.as_str() != &text[root.len() + 1..] {
            return Err(shape(path, "not in NFC form"));
        }
        let parts: Vec<&str> = rel.as_str().split('/').collect();
        let row = classify(&parts).ok_or_else(|| shape(path, "matches no row of the retire table"))?;

        // 2. the row's check, in one read snapshot
        self.read(|c| self.retire_check(c, path, text, &row))?;

        // 3. open and prepare through apfs
        let f = strata_apfs::open_rooted(&self.real_root, &rel).map_err(|e| prep_error(e, path))?;
        let md = f.metadata().map_err(|e| crate::errors::io(&e, IoSide::Read, path, OP, NOT_WRITTEN))?;
        if !md.is_dir() {
            return Err(err(
                Code::NotDirectory,
                format!("{} is not a directory", path.display()),
                json!({ "path": text, "op": OP, "mode": md.mode() }),
            ));
        }
        if let Some(n) = self.seams.take(|s| match s {
            Seam::RetirePrepFails { errno } => Some(*errno),
            _ => None,
        }) {
            return Err(errno(n, IoSide::Write, path, OP, NOT_WRITTEN));
        }
        strata_apfs::make_removable(&f).map_err(|e| crate::errors::annotate(e, OP, NOT_WRITTEN))?;
        drop(f);

        // 4. rename into trash, then F_FULLFSYNC the trash directory
        let ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let to = RepoPath::try_from(format!("store/trash/{ms}-{}", hex32())).expect("trash name");
        strata_apfs::rename_rooted(&self.real_root, &rel, &to, RenameMode::Excl)
            .map_err(|e| crate::errors::annotate(e, OP, NOT_WRITTEN))?;
        let trash = RepoPath::try_from("store/trash".to_string()).expect("trash");
        let t = strata_apfs::open_rooted(&self.real_root, &trash).map_err(|e| crate::errors::annotate(e, OP, "partial"))?;
        strata_apfs::full_fsync(&t).map_err(|e| crate::errors::annotate(e, OP, "partial"))?;
        self.seams.dir_barriers.fetch_add(1, Ordering::SeqCst);
        Ok(self.root.join(to.as_str()))
    }

    fn retire_check(&self, c: &Connection, path: &Path, text: &str, row: &Row7<'_>) -> Result<()> {
        match row {
            Row7::Gen(repo, tree, deps) => {
                if exists(
                    c,
                    "SELECT EXISTS (SELECT 1 FROM base_gens WHERE repo = ?1 AND tree = ?2 AND deps = ?3)",
                    [repo.as_str(), tree.as_str(), deps.as_str()],
                )? {
                    return Err(refused(path, 1, "the base generation is registered"));
                }
            }
            Row7::Deps(repo, deps) => {
                if exists(c, "SELECT EXISTS (SELECT 1 FROM deps_sets WHERE repo = ?1 AND id = ?2)", [repo.as_str(), deps.as_str()])? {
                    return Err(refused(path, 2, "the deps set is registered"));
                }
            }
            Row7::Attempt(repo, a) => {
                let r: Option<(String, String)> = c
                    .query_row("SELECT repo, state FROM deps_attempts WHERE id = ?1", [a], |r| Ok((r.get(0)?, r.get(1)?)))
                    .optional()
                    .map_err(sql_read)?;
                match r {
                    Some((r, _)) if r != repo.as_str() => return Err(refused(path, 3, "the attempt belongs to another repo")),
                    Some((_, s)) if s == "failed" => return Err(refused(path, 3, "the attempt failed; its staging is the scene")),
                    Some(_) => {}
                    None => {
                        let issued: i64 = c
                            .query_row("SELECT COALESCE((SELECT seq FROM sqlite_sequence WHERE name = 'deps_attempts'), 0)", [], |r| r.get(0))
                            .map_err(sql_read)?;
                        if *a > issued {
                            return Err(refused(path, 3, "no attempt with this number was ever issued"));
                        }
                    }
                }
            }
            Row7::SessionDir(sid) => {
                if let Some(lc) = session_lifecycle(c, sid.as_str()).map_err(sql_read)? {
                    if lc != "removing" && lc != "removed" {
                        return Err(refused(path, 4, &format!("the session is {lc}")));
                    }
                }
            }
            Row7::Child(sid, name) => {
                let id = sid.as_str();
                let lc = session_lifecycle(c, id).map_err(sql_read)?;
                if lc.as_deref() != Some("active") {
                    return Err(refused(path, 5, "the session is not active"));
                }
                if exists(c, "SELECT EXISTS (SELECT 1 FROM workspaces WHERE session = ?1 AND repo = ?2)", [id, name])? {
                    return Err(refused(path, 5, "it is a workspace directory"));
                }
                if !exists(
                    c,
                    "SELECT EXISTS (SELECT 1 FROM move_intents WHERE session = ?1 AND \
                     ((staging = ?2 AND state IN ('preparing', 'refused', 'abandoned', 'done')) OR old_tree = ?2))",
                    [id, text],
                )? {
                    return Err(refused(path, 5, "no move intent of the session names it as retirable"));
                }
            }
            Row7::Admin(sid, repo, m) => {
                let id = sid.as_str();
                let lc = session_lifecycle(c, id).map_err(sql_read)?;
                if lc.as_deref() != Some("active") {
                    return Err(refused(path, 6, "the session is not active"));
                }
                let generation: Option<i64> = c
                    .query_row("SELECT generation FROM workspaces WHERE session = ?1 AND repo = ?2", [id, repo.as_str()], |r| r.get(0))
                    .optional()
                    .map_err(sql_read)?;
                let Some(generation) = generation else {
                    return Err(refused(path, 6, "the session has no such workspace"));
                };
                let ok = *m < generation
                    || exists(
                        c,
                        "SELECT EXISTS (SELECT 1 FROM move_intents WHERE session = ?1 AND repo = ?2 AND n = ?3 \
                         AND state IN ('preparing', 'refused', 'abandoned'))",
                        rusqlite::params![id, repo.as_str(), m],
                    )?;
                if !ok {
                    return Err(refused(path, 6, "it is the current, an in-flight or an unknown generation's admin dir"));
                }
            }
            Row7::BaseStaging => {}
        }
        Ok(())
    }
}

/// apfs open errors → the codes of §5.5 step 3.
fn prep_error(e: StrataError, path: &Path) -> StrataError {
    let reason = e.detail.as_ref().and_then(|d| d.get("reason")).and_then(|r| r.as_str()).map(str::to_string);
    match (e.code, reason.as_deref()) {
        (Code::NotDirectory | Code::NotFile, Some("symlink")) => err(
            Code::InvalidInput,
            format!("{} passes through or is a symlink", path.display()),
            json!({ "reason": "symlink", "path": path.to_string_lossy() }),
        ),
        (Code::NotFile, _) => err(
            Code::NotDirectory,
            format!("{} is not a directory", path.display()),
            json!({ "path": path.to_string_lossy(), "op": OP, "kind": reason }),
        ),
        _ => crate::errors::annotate(e, OP, NOT_WRITTEN),
    }
}
