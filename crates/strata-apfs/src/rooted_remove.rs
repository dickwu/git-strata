//! `remove_rooted` (§5.4): one `unlinkat` on a pinned parent.

use std::path::Path;

use serde_json::json;
use strata_types::{Code, RepoPath, StrataError};

use crate::Result;
use crate::error::{IoSide, classify_errno, errno_error, set_detail};
use crate::hooks;
use crate::meta::{BLOCKING, blocking_flags_str};
use crate::rooted::resolve;
use crate::sys;

/// What `remove_rooted` removes. The wrong kind is an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoveKind {
    /// `unlinkat(fd, name, 0)`: anything but a directory (symlinks themselves).
    File,
    /// `unlinkat(fd, name, AT_REMOVEDIR)`: an empty directory.
    EmptyDir,
}

/// Remove one entry under the root with a single `unlinkat`. Never chmods,
/// never clears flags (A10).
pub fn remove_rooted(root: &Path, path: &RepoPath, kind: RemoveKind) -> Result<()> {
    let mut r = resolve(root, path, IoSide::Write, 0)?;
    let cn = sys::cos(&r.name).map_err(|e| classify_errno(e, IoSide::Write, &r.shown))?;
    let shown = r.shown.clone();
    let pdev = || sys::fstat(r.dir.0).map(|s| s.st_dev).ok();
    let mount_point = |dst_dev: Option<i32>, errno: i32| {
        let mut e = errno_error(Code::CrossVolume, errno, &shown, Some("mount_point"));
        set_detail(&mut e, "src_dev", json!(pdev()));
        set_detail(&mut e, "dst_dev", json!(dst_dev));
        e
    };
    if kind == RemoveKind::EmptyDir {
        let st = match sys::fstatat_nofollow(r.dir.0, &r.name) {
            Ok(st) => st,
            Err(e) if e == libc::ENOENT => return Err(errno_error(Code::NoSuchPath, e, &shown, None)),
            Err(e) => return Err(classify_errno(e, IoSide::Write, &shown)),
        };
        if sys::is_lnk(&st) {
            return Err(errno_error(Code::NotDirectory, libc::ENOTDIR, &shown, Some("symlink")));
        }
        if !sys::is_dir(&st) {
            return Err(errno_error(Code::NotDirectory, libc::ENOTDIR, &shown, Some("not_a_directory")));
        }
        if Some(st.st_dev) != pdev() {
            return Err(mount_point(Some(st.st_dev), 0));
        }
    }
    hooks::before_open(r.step, &r.name);
    r.step += 1;
    let flag = if kind == RemoveKind::EmptyDir { libc::AT_REMOVEDIR } else { 0 };
    let e = match sys::retry(|| unsafe { libc::unlinkat(r.dir.0, cn.as_ptr(), flag) }) {
        Ok(_) => return Ok(()),
        Err(e) => e,
    };
    let flags_reason = |e: i32| -> StrataError {
        let st = sys::fstatat_nofollow(r.dir.0, &r.name).ok();
        match st.map(|s| s.st_flags & BLOCKING).unwrap_or(0) {
            0 => errno_error(Code::ReadOnly, e, &shown, Some("no_permission")),
            f => {
                let mut err = errno_error(Code::ReadOnly, e, &shown, Some("immutable"));
                set_detail(&mut err, "flags", json!(blocking_flags_str(f)));
                err
            }
        }
    };
    Err(match (kind, e) {
        (_, libc::ENOENT) => errno_error(Code::NoSuchPath, e, &shown, None),
        (_, libc::EROFS) => errno_error(Code::ReadOnly, e, &shown, Some("read_only_volume")),
        (RemoveKind::File, libc::EPERM) => match sys::fstatat_nofollow(r.dir.0, &r.name) {
            Ok(st) if sys::is_dir(&st) => errno_error(Code::IsDirectory, e, &shown, Some("directory")),
            _ => flags_reason(e),
        },
        (RemoveKind::File, libc::EACCES) => errno_error(Code::ReadOnly, e, &shown, Some("no_permission")),
        (RemoveKind::File, libc::EBUSY) => errno_error(Code::Unknown, e, &shown, Some("busy")),
        (RemoveKind::EmptyDir, libc::ENOTDIR) => errno_error(Code::NotDirectory, e, &shown, None),
        (RemoveKind::EmptyDir, libc::ENOTEMPTY) => errno_error(Code::NotEmpty, e, &shown, None),
        (RemoveKind::EmptyDir, libc::EBUSY) => {
            let dd = sys::fstatat_nofollow(r.dir.0, &r.name).map(|s| s.st_dev).ok();
            mount_point(dd, e)
        }
        (RemoveKind::EmptyDir, libc::EPERM | libc::EACCES) => flags_reason(e),
        _ => errno_error(Code::Unknown, e, &shown, None),
    })
}
