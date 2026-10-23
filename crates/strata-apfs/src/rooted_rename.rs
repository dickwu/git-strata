//! `rename_rooted` (§5.4 "改名、删除与 `full_fsync`"): both parents pinned,
//! one `renameatx_np` with `RENAME_EXCL` or `RENAME_SWAP`.

use std::path::Path;

use serde_json::json;
use strata_types::{Code, RepoPath, StrataError};

use crate::Result;
use crate::error::{IoSide, classify_errno, errno_error, refusal, set_detail};
use crate::hooks;
use crate::meta::{BLOCKING, blocking_flags_str};
use crate::rooted::{Resolved, check_root, resolve};
use crate::sys;

/// Rename mode, mapped to the `renameatx_np` flags. There is no overwrite.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenameMode {
    /// `RENAME_EXCL`: `to` existing (anything, dangling symlinks included) → `Exists`.
    Excl,
    /// `RENAME_SWAP`: both must exist; atomically exchanged.
    Swap,
}

/// Rename `root/from` to `root/to` with one `renameatx_np`.
pub fn rename_rooted(root: &Path, from: &RepoPath, to: &RepoPath, mode: RenameMode) -> Result<()> {
    check_root(root)?;
    let from_shown = root.join(from.as_str());
    if from.as_str() == to.as_str() {
        return Err(refusal(Code::InvalidInput, &from_shown, "same_path", "from and to are the same path"));
    }
    if to.as_str().starts_with(&format!("{}/", from.as_str())) {
        return Err(refusal(Code::InvalidInput, &from_shown, "into_itself", "cannot move a directory into itself"));
    }
    let pf = resolve(root, from, IoSide::Write, 0)?;
    let pt = resolve(root, to, IoSide::Write, pf.step)?;
    let cf = sys::cos(&pf.name).map_err(|e| classify_errno(e, IoSide::Write, &pf.shown))?;
    let ct = sys::cos(&pt.name).map_err(|e| classify_errno(e, IoSide::Write, &pt.shown))?;
    let flags = match mode {
        RenameMode::Excl => libc::RENAME_EXCL,
        RenameMode::Swap => libc::RENAME_SWAP,
    };
    hooks::before_open(pt.step, &pt.name);
    match sys::retry(|| unsafe { libc::renameatx_np(pf.dir.0, cf.as_ptr(), pt.dir.0, ct.as_ptr(), flags) }) {
        Ok(_) => Ok(()),
        Err(e) => Err(rename_error(e, &pf, &pt, mode)),
    }
}

fn same_dir(a: &Resolved, b: &Resolved) -> bool {
    match (sys::fstat(a.dir.0), sys::fstat(b.dir.0)) {
        (Ok(x), Ok(y)) => x.st_dev == y.st_dev && x.st_ino == y.st_ino,
        _ => false,
    }
}

fn rename_error(e: i32, pf: &Resolved, pt: &Resolved, mode: RenameMode) -> StrataError {
    match e {
        libc::EEXIST => errno_error(Code::Exists, e, &pt.shown, None),
        libc::ENOENT => {
            if sys::fstatat_nofollow(pf.dir.0, &pf.name).err() == Some(libc::ENOENT) {
                errno_error(Code::NoSuchPath, e, &pf.shown, Some("from_missing"))
            } else {
                errno_error(Code::NoSuchPath, e, &pt.shown, Some("to_missing"))
            }
        }
        libc::ENOTDIR => errno_error(Code::NotDirectory, e, &pt.shown, None),
        libc::EXDEV => {
            let mut err = errno_error(Code::CrossVolume, e, &pt.shown, Some("cross_volume"));
            set_detail(&mut err, "src_dev", json!(sys::real_dev_fd(pf.dir.0).ok()));
            set_detail(&mut err, "dst_dev", json!(sys::real_dev_fd(pt.dir.0).ok()));
            err
        }
        libc::ENOTSUP | libc::EOPNOTSUPP => {
            let mut err = errno_error(Code::Unsupported, e, &pf.shown, None);
            if let Ok(c) = crate::volume_caps(&sys::fd_path(pf.dir.0)) {
                set_detail(&mut err, "fs_type", json!(c.fs_type));
            }
            err
        }
        libc::EINVAL => errno_error(Code::InvalidInput, e, &pt.shown, None),
        libc::ENAMETOOLONG => errno_error(Code::InvalidInput, e, &pt.shown, Some("name_too_long")),
        libc::EROFS => errno_error(Code::ReadOnly, e, &pf.shown, Some("read_only_volume")),
        libc::EACCES | libc::EPERM => {
            let mut stats = vec![sys::fstatat_nofollow(pf.dir.0, &pf.name)];
            if mode == RenameMode::Swap {
                stats.push(sys::fstatat_nofollow(pt.dir.0, &pt.name));
            }
            let stats: Vec<libc::stat> = stats.into_iter().flatten().collect();
            let reparent = !same_dir(pf, pt);
            let mut flags = 0u32;
            let mut reason = "no_permission";
            if reparent && stats.iter().any(|s| sys::is_dir(s) && s.st_mode & 0o200 == 0) {
                reason = "reparent_needs_write";
            } else if let Some(s) = stats.iter().find(|s| s.st_flags & BLOCKING != 0) {
                reason = "immutable";
                flags = s.st_flags;
            }
            let mut err = errno_error(Code::ReadOnly, e, &pf.shown, Some(reason));
            if flags != 0 {
                set_detail(&mut err, "flags", json!(blocking_flags_str(flags)));
            }
            err
        }
        libc::EBUSY => errno_error(Code::Unknown, e, &pf.shown, Some("busy")),
        _ => errno_error(Code::Unknown, e, &pf.shown, None),
    }
}
