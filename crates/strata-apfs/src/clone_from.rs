//! `clone_from_rooted` (§5.4, addendum 8): capture content from under a root.
//! The source is opened once through the no-follow chain, its kind checked on
//! the pinned handle, and the clone made from that handle (`fclonefileat`).

use std::path::Path;

use strata_types::{Code, RepoPath};

use crate::Result;
use crate::clone::{CLONE_FLAGS, check_dst, clone_errno, fill_dst_exists};
use crate::error::{IoSide, classify_errno, errno_error, refusal, set_detail, with};
use crate::hooks as seams;
use crate::rooted::{check_root, final_error, open_final, resolve};
use crate::sys;
use serde_json::json;

/// What the caller expects `clone_from_rooted`'s source to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloneKind {
    /// A regular file: a directory → IsDirectory; FIFO, socket, device,
    /// symlink → NotFile.
    File,
    /// A directory: anything else → NotDirectory.
    Dir,
}

/// Clone `src_root/src` to `dst`, cloning the object that was opened (never a
/// symlink's target, at any level).
pub fn clone_from_rooted(src_root: &Path, src: &RepoPath, dst: &Path, kind: CloneKind) -> Result<()> {
    check_root(src_root)?;
    // 1. dst first; the source is not touched on a dst refusal.
    let parent = check_dst(dst)?;
    // 2. Pin the source's parent.
    let mut r = resolve(src_root, src, IoSide::Read, 0)?;
    // 3. Open the last component.
    let fd = match open_final(&mut r) {
        Ok(fd) => fd,
        Err(e) => {
            return Err(match (kind, e) {
                (CloneKind::Dir, libc::ELOOP) => errno_error(Code::NotDirectory, e, &r.shown, Some("symlink")),
                (CloneKind::Dir, libc::EOPNOTSUPP) => {
                    errno_error(Code::NotDirectory, e, &r.shown, Some("not_a_directory"))
                }
                _ => final_error(e, &r.shown),
            });
        }
    };
    // 4. Kind check on the handle, before the volume check and the clone.
    let st = sys::fstat(fd.0).map_err(|e| classify_errno(e, IoSide::Read, &r.shown))?;
    match kind {
        CloneKind::File => {
            if sys::is_dir(&st) {
                return Err(refusal(Code::IsDirectory, &r.shown, "directory", "source is a directory"));
            }
            if !sys::is_reg(&st) {
                return Err(refusal(Code::NotFile, &r.shown, sys::special_reason(&st), "source is not a regular file"));
            }
        }
        CloneKind::Dir => {
            if !sys::is_dir(&st) {
                return Err(refusal(Code::NotDirectory, &r.shown, "not_a_directory", "source is not a directory"));
            }
        }
    }
    // 5. Same volume (the handle's real device), then clone support.
    let sd = sys::real_dev_fd(fd.0).map_err(|e| classify_errno(e, IoSide::Read, &r.shown))?;
    let dd = sys::real_dev_path(&parent).map_err(|e| classify_errno(e, IoSide::Read, &parent))?;
    if sd != dd {
        let e = errno_error(Code::CrossVolume, libc::EXDEV, dst, Some("cross_volume"));
        let e = with(e, "src_dev", json!(sd));
        let e = with(e, "dst_dev", json!(dd));
        return Err(with(e, "dst_exists", json!(false)));
    }
    let caps = crate::volume_caps(&parent)?;
    if caps.clone == Some(false) {
        let mut e = refusal(Code::Unsupported, dst, "clone_unsupported", "the volume does not support clonefile");
        set_detail(&mut e, "fs_type", json!(caps.fs_type));
        set_detail(&mut e, "dst_exists", json!(false));
        return Err(e);
    }
    // 6. Seam, then clone from the handle.
    let last = dst.file_name().unwrap_or(dst.as_os_str());
    seams::before_open(r.step, last);
    let cd = sys::cpath(dst).map_err(|e| classify_errno(e, IoSide::Write, dst))?;
    match sys::retry(|| unsafe { libc::fclonefileat(fd.0, libc::AT_FDCWD, cd.as_ptr(), CLONE_FLAGS) }) {
        Ok(_) => Ok(()),
        Err(e) => Err(fill_dst_exists(clone_errno(e, dst), dst)),
    }
}
