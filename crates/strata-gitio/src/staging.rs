//! Publish staging trees (§5.6 `attach_staging`): a private git dir whose
//! `commondir` is the main checkout's common dir, so objects land in the
//! main store without touching `<common>/worktrees/` (invariant 2).

use std::fs::File;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use serde_json::json;
use strata_types::{Code, CommitOid, StrataError};

use crate::command::{ATTACH_TIMEOUT, Opts, failed};
use crate::err::{as_unknown, errd, path_str};
use crate::{Git, Result, fsx};

fn not_canonical(p: &Path, what: &str) -> StrataError {
    errd(
        Code::InvalidInput,
        format!("{what} must be a lexically canonical path: {}", p.display()),
        json!({ "path": path_str(p), "reason": "not-canonical" }),
    )
}

pub(crate) fn not_staging(staging: &Path, why: &str) -> StrataError {
    errd(
        Code::InvalidInput,
        format!("{} is not a publish staging tree of this repository: {why}", staging.display()),
        json!({ "path": path_str(staging), "reason": "not-staging" }),
    )
}

fn is_canonical(p: &Path) -> bool {
    let n = fsx::lexical_normalize(p);
    n.as_os_str() == p.as_os_str()
}

impl Git {
    /// Give `staging` (an export of `head`) a private git dir `gitdir`
    /// sharing this repository's object store and refs (§5.6).
    pub fn attach_staging(&self, staging: &Path, gitdir: &Path, head: &CommitOid) -> Result<()> {
        // Step 1: refusals (G9), nothing written yet.
        fsx::require_absolute(staging, "staging")?;
        fsx::require_absolute(gitdir, "gitdir")?;
        for (p, what) in [(staging, "staging"), (gitdir, "gitdir")] {
            if !is_canonical(p) {
                return Err(not_canonical(p, what));
            }
        }
        let tree = self.tree_of(head)?;
        for (p, what) in [(staging, "staging"), (gitdir, "gitdir")] {
            if self.inside_main(p) {
                return Err(errd(
                    Code::InvalidInput,
                    format!("{what} {} is inside the main checkout", p.display()),
                    json!({ "path": path_str(p), "reason": "inside-main" }),
                ));
            }
        }
        match fsx::lstat(staging)? {
            None => {
                return Err(errd(
                    Code::NoSuchPath,
                    format!("staging does not exist: {}", staging.display()),
                    json!({ "path": path_str(staging) }),
                ));
            }
            Some(m) if !m.is_dir() => {
                return Err(errd(
                    Code::NotDirectory,
                    format!("staging is not a directory: {}", staging.display()),
                    json!({ "path": path_str(staging) }),
                ));
            }
            Some(_) => {}
        }
        let dotgit = staging.join(".git");
        if fsx::lstat(&dotgit)?.is_some() {
            // A human's linked worktree (its gitfile points into the common
            // dir) is the caller's mistake, not an occupied slot.
            if let Ok(Some(target)) = self.worktree_gitdir(staging)
                && self.inside_main(&fsx::lexical_normalize(&target))
            {
                return Err(errd(
                    Code::InvalidInput,
                    format!("{} is a worktree of the main checkout", staging.display()),
                    json!({ "path": path_str(staging), "reason": "linked-worktree" }),
                ));
            }
            return Err(errd(Code::Exists, format!("{} already exists", dotgit.display()), json!({ "path": path_str(&dotgit) })));
        }
        if fsx::lstat(gitdir)?.is_some() {
            return Err(errd(Code::Exists, format!("{} already exists", gitdir.display()), json!({ "path": path_str(gitdir) })));
        }
        let (parent, name) = match (gitdir.parent(), gitdir.file_name()) {
            (Some(p), Some(n)) => (p, n),
            _ => return Err(not_canonical(gitdir, "gitdir")),
        };
        match fsx::lstat(parent)? {
            None => {
                return Err(errd(
                    Code::NoSuchPath,
                    format!("the parent of gitdir does not exist: {}", parent.display()),
                    json!({ "path": path_str(parent) }),
                ));
            }
            Some(m) if !m.is_dir() => {
                return Err(errd(
                    Code::NotDirectory,
                    format!("the parent of gitdir is not a directory: {}", parent.display()),
                    json!({ "path": path_str(parent) }),
                ));
            }
            Some(_) => {}
        }

        // Steps 2-4: every failure from here on is Unknown.
        let parent_real = std::fs::canonicalize(parent)
            .map_err(|e| as_unknown(fsx::io_err(&e, "realpath", parent), "resolving the gitdir parent"))?;
        let name = name.to_str().ok_or_else(|| not_canonical(gitdir, "gitdir"))?.to_string();
        let gd_real = parent_real.join(&name);
        let write = || -> Result<()> {
            strata_apfs::mkdir_rooted(&parent_real, &fsx::rp(&name)?, 0o755, false)?;
            let mut common = self.inner.common_dir.as_os_str().as_bytes().to_vec();
            common.push(b'\n');
            let mut back = dotgit.as_os_str().as_bytes().to_vec();
            back.push(b'\n');
            let files: [(&str, Vec<u8>); 3] =
                [("commondir", common), ("gitdir", back), ("HEAD", format!("{}\n", head.as_str()).into_bytes())];
            for (f, bytes) in files {
                fsx::write_new_rooted(&gd_real, f, &bytes, 0o644)?;
            }
            Ok(())
        };
        write().map_err(|e| as_unknown(e, "building the staging git dir"))?;

        let idx = gd_real.join("index");
        let index = || -> Result<()> {
            let mut cmd = self.tree_command(&gd_real, staging, Some(&idx));
            cmd.args(["-c", "core.splitIndex=false", "read-tree"]).arg(tree.as_str());
            let out = self.run(cmd, Opts::default().timeout(ATTACH_TIMEOUT))?;
            if !out.ok() {
                return Err(failed("read-tree", &out));
            }
            let mut cmd = self.tree_command(&gd_real, staging, Some(&idx));
            cmd.args(["-c", "core.splitIndex=false", "update-index", "-q", "--refresh"]);
            let out = self.run(cmd, Opts::default().timeout(ATTACH_TIMEOUT))?;
            if !out.ok() {
                return Err(failed("update-index --refresh", &out));
            }
            Ok(())
        };
        index().map_err(|e| as_unknown(e, "building the staging index"))?;

        let mut gitfile = b"gitdir: ".to_vec();
        gitfile.extend_from_slice(gitdir.as_os_str().as_bytes());
        gitfile.push(b'\n');
        fsx::write_new_rooted(staging, ".git", &gitfile, 0o644).map_err(|e| as_unknown(e, "writing the gitfile"))?;

        self.staging_sync(&parent_real, &name, staging).map_err(|e| as_unknown(e, "syncing the staging"))
    }

    fn staging_sync(&self, parent: &Path, name: &str, staging: &Path) -> Result<()> {
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
        for f in ["commondir", "gitdir", "HEAD", "index"] {
            sync(parent, &format!("{name}/{f}"))?;
        }
        sync(parent, name)?;
        sync(parent, "")?;
        sync(staging, ".git")?;
        sync(staging, "")?;
        let r = strata_apfs::open_root(staging)?;
        strata_apfs::full_fsync(&r)?;
        self.inner.seams.full_fsync(staging);
        Ok(())
    }

    /// The private git dir of a staging tree attached to this repository
    /// (§5.9 step 1), or `InvalidInput`. Reads go through rooted opens (G11).
    pub(crate) fn staging_gitdir(&self, staging: &Path) -> Result<PathBuf> {
        fsx::require_absolute(staging, "staging")?;
        if !is_canonical(staging) {
            return Err(not_canonical(staging, "staging"));
        }
        let f = strata_apfs::open_rooted_file(staging, &fsx::rp(".git")?)
            .map_err(|e| not_staging(staging, &format!(".git: {}", e.message)))?;
        let gd = fsx::read_gitfile(&f, staging).map_err(|e| not_staging(staging, &e.message))?;
        let gd = fsx::lexical_normalize(&gd);
        let gd_real = fsx::resolve_lenient(&gd);
        if gd_real.starts_with(&self.inner.common_dir) || self.inside_main(&gd) {
            return Err(not_staging(staging, "its git dir is inside the main checkout"));
        }
        let (bytes, _) = fsx::read_rooted_limited(&gd_real, "commondir", Some(strata_apfs::PATH_MAX_BYTES as u64 + 2))
            .map_err(|e| not_staging(staging, &format!("commondir: {}", e.message)))?
            .ok_or_else(|| not_staging(staging, "no commondir"))?;
        let raw = fsx::strip_eol(&bytes);
        if raw.is_empty() || raw.contains(&0) {
            return Err(not_staging(staging, "bad commondir"));
        }
        let p = Path::new(std::ffi::OsStr::from_bytes(raw));
        let p = if p.is_absolute() { p.to_path_buf() } else { gd_real.join(p) };
        match std::fs::canonicalize(&p) {
            Ok(r) if r == self.inner.common_dir => Ok(gd_real),
            _ => Err(not_staging(staging, "its commondir is not this repository's common dir")),
        }
    }

}
