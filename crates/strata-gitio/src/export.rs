//! `export_tree` (§5.5): a tree checked out like `git checkout` into a new
//! directory, made durable, then renamed into place with `RENAME_EXCL`.

use std::collections::BTreeSet;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use strata_apfs::{NAME_MAX_UTF16_UNITS, PATH_MAX_BYTES, RemoveKind, RenameMode};
use strata_types::{Code, RepoPath, StrataError, TreeOid};

use crate::command::{Opts, failed};
use crate::err::{errd, path_str};
use crate::fsx;
use crate::seams::EXPORT_BEFORE_RENAME;
use crate::tree::TreeTable;
use crate::{EXPORT_TEMP_PREFIX, Git, Result};

fn invalid_dst(dst: &Path, why: &str, reason: &str) -> StrataError {
    errd(
        Code::InvalidInput,
        format!("export destination {}: {why}", dst.display()),
        json!({ "path": path_str(dst), "reason": reason }),
    )
}

/// Every failure after the first write is `Unknown` with the tree and the
/// kept temporary directory in `detail` (G4).
fn after_write(e: StrataError, tree: &TreeOid, temp: &Path) -> StrataError {
    let mut detail = match e.detail {
        Some(Value::Object(m)) => m,
        Some(other) => {
            let mut m = serde_json::Map::new();
            m.insert("inner".into(), other);
            m
        }
        None => serde_json::Map::new(),
    };
    if e.code != Code::Unknown {
        detail.insert("code".into(), json!(e.code.as_str()));
    }
    detail.insert("tree".into(), json!(tree.as_str()));
    detail.insert("temp".into(), json!(path_str(temp)));
    errd(Code::Unknown, format!("export of {} failed: {}", tree.as_str(), e.message), Value::Object(detail))
}

fn fsync_file(f: &File, shown: &Path) -> Result<()> {
    if unsafe { libc::fsync(f.as_raw_fd()) } != 0 {
        let e = std::io::Error::last_os_error();
        return Err(strata_apfs::classify_io_error(&e, strata_apfs::IoSide::AfterWrite, shown));
    }
    Ok(())
}

impl Git {
    /// Export `tree` into the new directory `dst`, byte for byte what
    /// `git checkout` writes (§5.5).
    pub fn export_tree(&self, tree: &TreeOid, dst: &Path) -> Result<()> {
        // Step 1: every typed refusal, before anything is written (G9).
        fsx::require_absolute(dst, "export destination")?;
        let table = self.tree_table(tree)?;
        let name = match dst.file_name() {
            Some(n) => n,
            None => return Err(invalid_dst(dst, "has no final name", "bad-name")),
        };
        let parent = dst.parent().unwrap_or(Path::new("/")).to_path_buf();
        match fsx::lstat(&parent)? {
            None => {
                return Err(errd(
                    Code::NoSuchPath,
                    format!("parent of export destination does not exist: {}", parent.display()),
                    json!({ "path": path_str(&parent) }),
                ));
            }
            Some(m) if !m.is_dir() => {
                return Err(errd(
                    Code::NotDirectory,
                    format!("parent of export destination is not a directory: {}", parent.display()),
                    json!({ "path": path_str(&parent) }),
                ));
            }
            Some(_) => {}
        }
        if fsx::lstat(dst)?.is_some() {
            return Err(errd(
                Code::Exists,
                format!("export destination already exists: {}", dst.display()),
                json!({ "path": path_str(dst) }),
            ));
        }
        if self.inside_main(dst) {
            return Err(invalid_dst(dst, "is inside the main checkout", "inside-main"));
        }
        let name = match name.to_str() {
            Some(n) => n,
            None => return Err(invalid_dst(dst, "name is not UTF-8", "bad-name")),
        };
        let name_rp = match RepoPath::try_from(name.to_string()) {
            Ok(p) if !p.as_str().contains('/') => p,
            _ => return Err(invalid_dst(dst, "name is not a single path component", "bad-name")),
        };
        let tname = format!("{EXPORT_TEMP_PREFIX}{name}-{}", fsx::nonce());
        if tname.encode_utf16().count() > NAME_MAX_UTF16_UNITS {
            return Err(invalid_dst(dst, "temporary directory name would be too long", "name-too-long"));
        }
        // `P/T/tree/<path>` (and `P/T/index`) plus the terminating NUL must
        // fit in PATH_MAX.
        let rel_max = table.entries.iter().map(|e| "tree/".len() + e.path.as_str().len()).max().unwrap_or(0).max("index".len());
        let full = parent.as_os_str().len() + 1 + tname.len() + 1 + rel_max;
        if full + 1 > PATH_MAX_BYTES {
            return Err(invalid_dst(dst, "a path in the export would reach PATH_MAX", "path-too-long"));
        }
        let t_rp = fsx::rp(&tname)?;
        let temp = parent.join(&tname);

        // Step 2: the temporary directory and its work tree.
        if let Err(e) = strata_apfs::mkdir_rooted(&parent, &t_rp, 0o700, false) {
            return Err(match e.code {
                Code::NoSuchPath | Code::NotDirectory => e,
                _ => crate::err::as_unknown(e, "creating the export directory"),
            });
        }
        self.export_after_mkdir(tree, &table, &parent, &tname, &name_rp, dst)
            .map_err(|e| after_write(e, tree, &temp))
    }

    fn export_after_mkdir(
        &self,
        tree: &TreeOid,
        table: &TreeTable,
        parent: &Path,
        tname: &str,
        name_rp: &RepoPath,
        dst: &Path,
    ) -> Result<()> {
        let temp = parent.join(tname);
        let work = temp.join("tree");
        let index = temp.join("index");
        strata_apfs::mkdir_rooted(parent, &fsx::rp(&format!("{tname}/tree"))?, 0o755, false)?;

        // Step 3: the private index.
        let mut c = self.tree_command(&self.inner.common_dir, &work, Some(&index));
        c.arg("read-tree").arg(tree.as_str());
        let out = self.run(c, Opts::default())?;
        if !out.ok() {
            return Err(failed("read-tree", &out));
        }

        // Step 4: checkout-index applies every checkout-direction conversion.
        let mut c = self.tree_command(&self.inner.common_dir, &work, Some(&index));
        c.args(["checkout-index", "--all", "--force"]);
        let out = self.run(c, Opts::default())?;
        if !out.ok() {
            return Err(failed("checkout-index", &out));
        }

        // Step 5: fsync every file and directory, then one F_FULLFSYNC.
        let mut dirs: BTreeSet<String> = BTreeSet::new();
        for e in &table.entries {
            let p = e.path.as_str();
            let mut i = 0;
            while let Some(j) = p[i..].find('/') {
                dirs.insert(p[..i + j].to_string());
                i += j + 1;
            }
            if e.mode == 0o100644 || e.mode == 0o100755 {
                self.sync_one(parent, &format!("{tname}/tree/{p}"))?;
            }
        }
        let mut dirs: Vec<String> = dirs.into_iter().collect();
        dirs.sort_by_key(|d| std::cmp::Reverse(d.matches('/').count()));
        for d in &dirs {
            self.sync_one(parent, &format!("{tname}/tree/{d}"))?;
        }
        let root = self.sync_one(parent, &format!("{tname}/tree"))?;
        strata_apfs::full_fsync(&root)?;
        self.inner.seams.full_fsync(&work);
        drop(root);

        self.inner.seams.hit(EXPORT_BEFORE_RENAME);

        // Step 6: the atomic replacement point.
        let from = fsx::rp(&format!("{tname}/tree"))?;
        self.inner.seams.rename_excl(&work, dst);
        strata_apfs::rename_rooted(parent, &from, name_rp, RenameMode::Excl)?;
        let p = strata_apfs::open_root(parent)?;
        strata_apfs::full_fsync(&p)?;
        self.inner.seams.full_fsync(parent);

        // Step 7: only the private index is left in the temporary directory.
        strata_apfs::remove_rooted(parent, &fsx::rp(&format!("{tname}/index"))?, RemoveKind::File)?;
        strata_apfs::remove_rooted(parent, &fsx::rp(tname)?, RemoveKind::EmptyDir)?;
        Ok(())
    }

    fn sync_one(&self, parent: &Path, rel: &str) -> Result<File> {
        let shown: PathBuf = parent.join(rel);
        let f = strata_apfs::open_rooted(parent, &fsx::rp(rel)?)?;
        fsync_file(&f, &shown)?;
        self.inner.seams.fsync(&shown);
        Ok(f)
    }
}
