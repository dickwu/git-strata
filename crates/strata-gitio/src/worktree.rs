//! Workspace generations (§5.6): `attach_worktree` builds a fresh admin dir
//! `worktrees/<n>/` in the session's shadow repository for each generation;
//! `worktree_gitdir` reads a tree's gitfile without following anything.

use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde_json::json;
use strata_apfs::{FileKind, RemoveKind};
use strata_types::{Code, CommitOid, StrataError};

use crate::command::{ATTACH_TIMEOUT, Opts, failed, flag};
use crate::err::{as_unknown, errd, path_str};
use crate::seams::{ATTACH_BEFORE_INDEX, ATTACH_BEFORE_READ_TREE, ATTACH_BEFORE_READBACK};
use crate::shadow::Shadow;
use crate::{Git, Result, fsx};

/// Identity of the staging root taken right after 3.1.
#[derive(PartialEq, Eq, Clone, Copy)]
struct RootId {
    dev: u64,
    ino: u64,
}

fn ctime_of(m: &std::fs::Metadata) -> (i64, i64) {
    (m.ctime(), m.ctime_nsec())
}

/// Typed `NotDirectory` / `Exists` pass through; anything else is Unknown.
fn typed_or_unknown(e: StrataError, ctx: &str) -> StrataError {
    match e.code {
        Code::NotDirectory | Code::Exists => e,
        _ => as_unknown(e, ctx),
    }
}

fn staging_changed(tree_dir: &Path) -> StrataError {
    errd(
        Code::Unknown,
        format!("the staging tree {} changed during attach", tree_dir.display()),
        json!({ "path": path_str(tree_dir), "reason": "staging-changed" }),
    )
}

impl Git {
    /// Give generation `generation` of a workspace its own admin dir and
    /// index in `shadow`, and put a gitfile at `tree_dir/.git` pointing at
    /// it (§5.6). `workspace` is where the tree will live once the session
    /// moves it there. Returns the admin dir `<shadow>/worktrees/<n>`.
    pub fn attach_worktree(
        &self,
        shadow: &Shadow,
        generation: u64,
        head: &CommitOid,
        tree_dir: &Path,
        workspace: &Path,
    ) -> Result<PathBuf> {
        // Step 1: refusals, before any write.
        self.require_owned(shadow)?;
        if generation == 0 {
            return Err(errd(
                Code::InvalidInput,
                "generation must be at least 1",
                json!({ "generation": 0, "reason": "generation-zero" }),
            ));
        }
        let tree = self.tree_of(head)?;
        fsx::require_absolute(tree_dir, "tree_dir")?;
        fsx::require_absolute(workspace, "workspace")?;
        match fsx::lstat(tree_dir)? {
            None => {
                return Err(errd(
                    Code::NoSuchPath,
                    format!("tree_dir does not exist: {}", tree_dir.display()),
                    json!({ "path": path_str(tree_dir) }),
                ));
            }
            Some(m) if !m.is_dir() => {
                return Err(errd(
                    Code::NotDirectory,
                    format!("tree_dir is not a directory: {}", tree_dir.display()),
                    json!({ "path": path_str(tree_dir) }),
                ));
            }
            Some(_) => {}
        }
        for (p, what) in [(tree_dir, "tree_dir"), (workspace, "workspace")] {
            if self.inside_main(p) {
                return Err(errd(
                    Code::InvalidInput,
                    format!("{what} {} is inside the main checkout", p.display()),
                    json!({ "path": path_str(p), "reason": "inside-main" }),
                ));
            }
        }
        let dotgit = tree_dir.join(".git");
        if fsx::lstat(&dotgit)?.is_some() {
            return Err(errd(
                Code::Exists,
                format!("{} already exists", dotgit.display()),
                json!({ "path": path_str(&dotgit) }),
            ));
        }
        let sdir = shadow.dir();
        let wts = fsx::rp("worktrees")?;
        match strata_apfs::attrs_of(sdir, &wts) {
            Ok(a) if a.kind == FileKind::Dir => {}
            Ok(_) => {
                return Err(errd(
                    Code::NotDirectory,
                    format!("{} is not a directory", sdir.join("worktrees").display()),
                    json!({ "path": path_str(&sdir.join("worktrees")) }),
                ));
            }
            Err(e) if e.code == Code::NoSuchPath => {}
            Err(e) => return Err(typed_or_unknown(e, "inspecting worktrees")),
        }
        let n = generation.to_string();
        let admin_rel = format!("worktrees/{n}");
        let admin = sdir.join(&admin_rel);
        match strata_apfs::attrs_of(sdir, &fsx::rp(&admin_rel)?) {
            Ok(_) => {
                return Err(errd(
                    Code::Exists,
                    format!("{} already exists", admin.display()),
                    json!({ "path": path_str(&admin) }),
                ));
            }
            Err(e) if e.code == Code::NoSuchPath => {}
            Err(e) => return Err(typed_or_unknown(e, "inspecting the admin dir")),
        }

        // Step 2: the admin dir, rooted at the shadow repository.
        let step2 = || -> Result<()> {
            match strata_apfs::mkdir_rooted(sdir, &wts, 0o755, false) {
                Ok(()) => {}
                Err(e) if e.code == Code::Exists => {
                    let a = strata_apfs::attrs_of(sdir, &wts)?;
                    if a.kind != FileKind::Dir {
                        return Err(errd(
                            Code::NotDirectory,
                            format!("{} is not a directory", sdir.join("worktrees").display()),
                            json!({ "path": path_str(&sdir.join("worktrees")) }),
                        ));
                    }
                }
                Err(e) => return Err(e),
            }
            strata_apfs::mkdir_rooted(sdir, &fsx::rp(&admin_rel)?, 0o755, false)?;
            let mut gitdir = workspace.join(".git").into_os_string().as_bytes().to_vec();
            gitdir.push(b'\n');
            let files: [(&str, Vec<u8>); 4] = [
                ("commondir", b"../..\n".to_vec()),
                ("gitdir", gitdir),
                ("locked", b"git-strata".to_vec()),
                ("HEAD", format!("{}\n", head.as_str()).into_bytes()),
            ];
            for (name, bytes) in files {
                fsx::write_new_rooted(sdir, &format!("{admin_rel}/{name}"), &bytes, 0o644)?;
            }
            Ok(())
        };
        step2().map_err(|e| typed_or_unknown(e, "building the admin dir"))?;
        self.inner.seams.hit(ATTACH_BEFORE_INDEX);

        // 3.1: the temporary index's directory, and the staging root's identity.
        strata_apfs::mkdir_rooted(tree_dir, &fsx::rp(".git")?, 0o700, false)
            .map_err(|e| typed_or_unknown(e, "creating the temporary index dir"))?;
        let root = strata_apfs::open_root(tree_dir).map_err(|e| as_unknown(e, "opening tree_dir"))?;
        let m0 = root.metadata().map_err(|e| fsx::io_err(&e, "fstat", tree_dir)).map_err(|e| as_unknown(e, "fstat"))?;
        let id0 = RootId { dev: m0.dev(), ino: m0.ino() };
        let c0 = ctime_of(&m0);
        self.inner.seams.hit(ATTACH_BEFORE_READ_TREE);

        // 3.2: git builds the index under tree_dir/.git.
        let idx = dotgit.join("index");
        let built = self.build_index(sdir, tree_dir, &idx, tree.as_str());
        self.inner.seams.hit(ATTACH_BEFORE_READBACK);

        // 3.3: verify the staging, then copy the index in, rooted.
        let same = (|| {
            let m1 = root.metadata().ok()?;
            if ctime_of(&m1) != c0 {
                return Some(false);
            }
            let again = strata_apfs::open_root(tree_dir).ok()?;
            let m2 = again.metadata().ok()?;
            Some(RootId { dev: m2.dev(), ino: m2.ino() } == id0)
        })()
        .unwrap_or(false);
        if !same {
            return Err(staging_changed(tree_dir));
        }
        built.map_err(|e| as_unknown(e, "building the index"))?;
        let (bytes, mtime) = (|| -> Result<(Vec<u8>, std::time::SystemTime)> {
            let f = strata_apfs::open_rooted_file(tree_dir, &fsx::rp(".git/index")?)?;
            let m = f.metadata().map_err(|e| fsx::io_err(&e, "fstat", &idx))?;
            let mut b = Vec::with_capacity(m.len() as usize);
            (&f).read_to_end(&mut b)
                .map_err(|e| strata_apfs::classify_io_error(&e, strata_apfs::IoSide::Read, &idx))?;
            let mt = m.modified().map_err(|e| fsx::io_err(&e, "fstat", &idx))?;
            Ok((b, mt))
        })()
        .map_err(|e| {
            errd(
                Code::Unknown,
                format!("reading back {} refused: {}", idx.display(), e.message),
                json!({ "path": path_str(&idx), "reason": "index-read-back-refused", "code": e.code.as_str() }),
            )
        })?;
        let copy_rel = format!("{admin_rel}/index");
        let mut f = strata_apfs::create_file_rooted(sdir, &fsx::rp(&copy_rel)?, 0o644)
            .map_err(|e| typed_or_unknown(e, "creating the index copy"))?;
        let shown = sdir.join(&copy_rel);
        f.write_all(&bytes)
            .map_err(|e| as_unknown(strata_apfs::classify_io_error(&e, strata_apfs::IoSide::AfterWrite, &shown), "writing the index copy"))?;
        f.set_modified(mtime).map_err(|e| as_unknown(fsx::io_err(&e, "set mtime", &shown), "index copy mtime"))?;
        drop(f);

        // 3.4: the temporary index and its directory go.
        strata_apfs::remove_rooted(tree_dir, &fsx::rp(".git/index")?, RemoveKind::File)
            .and_then(|_| strata_apfs::remove_rooted(tree_dir, &fsx::rp(".git")?, RemoveKind::EmptyDir))
            .map_err(|e| as_unknown(e, "removing the temporary index"))?;

        // Step 4: the gitfile.
        let mut gitfile = b"gitdir: ".to_vec();
        gitfile.extend_from_slice(admin.as_os_str().as_bytes());
        gitfile.push(b'\n');
        fsx::write_new_rooted(tree_dir, ".git", &gitfile, 0o644).map_err(|e| as_unknown(e, "writing the gitfile"))?;

        // Step 5: durability.
        self.attach_sync(sdir, &admin_rel, tree_dir).map_err(|e| as_unknown(e, "syncing the attach"))?;
        Ok(admin)
    }

    /// 3.2: `read-tree` then `update-index --refresh`, the index at `idx`.
    fn build_index(&self, sdir: &Path, tree_dir: &Path, idx: &Path, tree: &str) -> Result<()> {
        let mut cmd = self.tree_command(sdir, tree_dir, Some(idx));
        cmd.args(["-c", "core.splitIndex=false", "read-tree"]).arg(flag("--index-output=", idx)).arg(tree);
        let out = self.run(cmd, Opts::default().timeout(ATTACH_TIMEOUT))?;
        if !out.ok() {
            return Err(failed("read-tree", &out));
        }
        let mut cmd = self.tree_command(sdir, tree_dir, Some(idx));
        cmd.args(["-c", "core.splitIndex=false", "update-index", "-q", "--refresh"]);
        let out = self.run(cmd, Opts::default().timeout(ATTACH_TIMEOUT))?;
        if !out.ok() {
            return Err(failed("update-index --refresh", &out));
        }
        Ok(())
    }

    fn attach_sync(&self, sdir: &Path, admin_rel: &str, tree_dir: &Path) -> Result<()> {
        let sync = |root: &Path, rel: &str| -> Result<()> {
            let f: File = if rel.is_empty() { strata_apfs::open_root(root)? } else { strata_apfs::open_rooted(root, &fsx::rp(rel)?)? };
            let shown = if rel.is_empty() { root.to_path_buf() } else { root.join(rel) };
            if unsafe { libc::fsync(std::os::fd::AsRawFd::as_raw_fd(&f)) } != 0 {
                let e = std::io::Error::last_os_error();
                return Err(strata_apfs::classify_io_error(&e, strata_apfs::IoSide::AfterWrite, &shown));
            }
            self.inner.seams.fsync(&shown);
            Ok(())
        };
        for name in ["commondir", "gitdir", "locked", "HEAD", "index"] {
            sync(sdir, &format!("{admin_rel}/{name}"))?;
        }
        sync(sdir, admin_rel)?;
        sync(sdir, "worktrees")?;
        sync(sdir, "")?;
        sync(tree_dir, ".git")?;
        sync(tree_dir, "")?;
        let r = strata_apfs::open_root(sdir)?;
        strata_apfs::full_fsync(&r)?;
        self.inner.seams.full_fsync(sdir);
        Ok(())
    }

    /// Where `tree_dir/.git` (a gitfile) points, read-only and without
    /// following anything (§5.6). `None` when there is no `.git`.
    pub fn worktree_gitdir(&self, tree_dir: &Path) -> Result<Option<PathBuf>> {
        fsx::require_absolute(tree_dir, "tree_dir")?;
        let f = match strata_apfs::open_rooted_file(tree_dir, &fsx::rp(".git")?) {
            Ok(f) => f,
            Err(e) if e.code == Code::NoSuchPath => return Ok(None),
            Err(e) => return Err(e),
        };
        fsx::read_gitfile(&f, tree_dir).map(Some)
    }
}

