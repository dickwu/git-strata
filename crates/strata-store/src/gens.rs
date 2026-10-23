//! Base generations with their manifests (store.md §5.5).

use std::collections::BTreeMap;
use std::time::SystemTime;

use crate::sql::{Connection, OptionalExtension, params};
use serde_json::json;
use strata_types::{BaseGen, BaseManifest, Code, ManifestEntry, RepoName, RepoPath, TreeOid};

use crate::codec::*;
use crate::errors::{NOT_WRITTEN, bad_row, err, invalid_field, sql, sql_read};
use crate::txn::Tx;
use crate::{Result, Store};

const MODES: [u32; 3] = [0o100644, 0o100755, 0o120000];

/// Manifest rows of (repo, tree) in path order, or None when no `base_trees` row.
fn load_manifest(c: &Connection, repo: &str, tree: &str) -> Result<Option<Vec<(String, String, i64, i64)>>> {
    let files: Option<i64> = c
        .query_row("SELECT files FROM base_trees WHERE repo = ?1 AND tree = ?2", params![repo, tree], |r| r.get(0))
        .optional()
        .map_err(sql_read)?;
    let Some(files) = files else { return Ok(None) };
    let mut st = c
        .prepare("SELECT path, blob, mode, size FROM manifest_entries WHERE repo = ?1 AND tree = ?2 ORDER BY path")
        .map_err(sql_read)?;
    let rows = st
        .query_map(params![repo, tree], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .map_err(sql_read)?
        .collect::<crate::sql::SqlResult<Vec<_>>>()
        .map_err(sql_read)?;
    if rows.len() as i64 != files {
        return Err(bad_row(format!("base_trees {repo}/{tree}: files = {files} but {} entries", rows.len())));
    }
    Ok(Some(rows))
}

impl Store {
    pub fn record_base_gen(&self, g: &BaseGen, manifest: &BaseManifest, at: SystemTime) -> Result<()> {
        const OP: &str = "record_base_gen";
        self.writable()?;
        if manifest.tree != g.tree {
            return Err(invalid_field("tree", "manifest.tree differs from g.tree"));
        }
        let at = ns(at, "at")?;
        let mut want = Vec::with_capacity(manifest.entries.len());
        for (path, e) in &manifest.entries {
            if !MODES.contains(&e.mode) {
                return Err(err(
                    Code::InvalidInput,
                    format!("{path}: mode {:o} is not 100644, 100755 or 120000", e.mode),
                    json!({ "field": "mode", "path": path.as_str() }),
                ));
            }
            want.push((path.as_str(), e.blob.as_str(), e.mode as i64, int(e.size, "size")?));
        }
        let files = want.len() as i64;
        let (repo, tree, deps) = (g.repo.as_str(), g.tree.as_str(), g.deps.as_str());
        self.write(OP, |c| {
            let registered: bool = c
                .query_row("SELECT EXISTS (SELECT 1 FROM deps_sets WHERE repo = ?1 AND id = ?2)", params![repo, deps], |r| r.get(0))
                .map_err(|e| sql(e, OP, NOT_WRITTEN))?;
            if !registered {
                return Err(err(
                    Code::InvalidInput,
                    format!("deps set {deps} of {repo} is not registered"),
                    json!({ "reason": "deps_unregistered", "repo": repo, "deps": deps }),
                ));
            }
            let mut wrote = false;
            match load_manifest(c, repo, tree).map_err(|e| crate::errors::annotate(e, OP, NOT_WRITTEN))? {
                Some(have) => {
                    let first_diff = have
                        .iter()
                        .zip(&want)
                        .find(|(h, w)| (h.0.as_str(), h.1.as_str(), h.2, h.3) != **w)
                        .map(|(h, w)| h.0.as_str().min(w.0))
                        .or_else(|| match have.len().cmp(&want.len()) {
                            std::cmp::Ordering::Greater => Some(have[want.len()].0.as_str()),
                            std::cmp::Ordering::Less => Some(want[have.len()].0),
                            std::cmp::Ordering::Equal => None,
                        });
                    if let Some(path) = first_diff {
                        return Err(err(
                            Code::InvalidInput,
                            format!("tree {tree} of {repo} is registered with a different manifest (first difference at {path})"),
                            json!({ "reason": "manifest_mismatch", "path": path, "repo": repo, "tree": tree }),
                        ));
                    }
                }
                None => {
                    c.execute(
                        "INSERT INTO base_trees (repo, tree, files, created_ns) VALUES (?1, ?2, ?3, ?4)",
                        params![repo, tree, files, at],
                    )
                    .map_err(|e| sql(e, OP, NOT_WRITTEN))?;
                    let mut st = c
                        .prepare("INSERT INTO manifest_entries (repo, tree, path, blob, mode, size) VALUES (?1, ?2, ?3, ?4, ?5, ?6)")
                        .map_err(|e| sql(e, OP, NOT_WRITTEN))?;
                    for (p, b, m, s) in &want {
                        st.execute(params![repo, tree, p, b, m, s]).map_err(|e| sql(e, OP, NOT_WRITTEN))?;
                    }
                    wrote = true;
                }
            }
            let n = c
                .execute(
                    "INSERT INTO base_gens (repo, tree, deps, created_ns) VALUES (?1, ?2, ?3, ?4) \
                     ON CONFLICT (repo, tree, deps) DO NOTHING",
                    params![repo, tree, deps, at],
                )
                .map_err(|e| sql(e, OP, NOT_WRITTEN))?;
            Ok(if wrote || n == 1 { Tx::Commit(()) } else { Tx::Rollback(()) })
        })
    }

    pub fn has_base_gen(&self, g: &BaseGen) -> Result<bool> {
        self.read(|c| {
            c.query_row(
                "SELECT EXISTS (SELECT 1 FROM base_gens WHERE repo = ?1 AND tree = ?2 AND deps = ?3)",
                params![g.repo.as_str(), g.tree.as_str(), g.deps.as_str()],
                |r| r.get(0),
            )
            .map_err(sql_read)
        })
    }

    pub fn manifest(&self, repo: &RepoName, tree: &TreeOid) -> Result<Option<BaseManifest>> {
        self.read(|c| {
            let Some(rows) = load_manifest(c, repo.as_str(), tree.as_str())? else { return Ok(None) };
            let mut entries = BTreeMap::new();
            for (path, blob, mode, size) in rows {
                let mode = u32::try_from(mode).ok().filter(|m| MODES.contains(m)).ok_or_else(|| bad_row(format!("bad mode {mode}")))?;
                let path: RepoPath = parse(path, "repo path")?;
                entries.insert(path, ManifestEntry { blob: parse(blob, "blob id")?, mode, size: nonneg(size, "size")? });
            }
            Ok(Some(BaseManifest { tree: tree.clone(), entries }))
        })
    }
}

