//! `clone_tree` (§5.2) and `clone_file` (§5.3). No copy fallback: if
//! `clonefile(2)` refuses, the error is returned (invariant A2).

use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use serde_json::json;
use strata_types::{Code, StrataError};

use crate::error::{IoSide, classify_errno, errno_error, refusal, set_detail, with};
use crate::sys;
use crate::{NAME_MAX_UTF16_UNITS, PATH_MAX_BYTES, Result};

pub(crate) const CLONE_NOFOLLOW: u32 = 0x1;
pub(crate) const CLONE_NOOWNERCOPY: u32 = 0x2;
pub(crate) const CLONE_FLAGS: u32 = CLONE_NOFOLLOW | CLONE_NOOWNERCOPY;

/// Clone a directory tree atomically with `clonefile(2)`.
pub fn clone_tree(src: &Path, dst: &Path) -> Result<()> {
    // 1. src must be a real directory.
    let st = sys::lstat(src).map_err(|e| classify_errno(e, IoSide::Read, src))?;
    if sys::is_lnk(&st) {
        return Err(refusal(Code::NotDirectory, src, "symlink", "source is a symlink, not a directory"));
    }
    if !sys::is_dir(&st) {
        return Err(refusal(Code::NotDirectory, src, sys::special_reason(&st), "source is not a directory"));
    }
    // 2-4. dst shape, absence, parent.
    let parent = check_dst(dst)?;
    // 5. dst inside src.
    if let (Ok(rs), Ok(rp)) = (std::fs::canonicalize(src), std::fs::canonicalize(&parent))
        && rp.starts_with(&rs)
    {
        return Err(refusal(Code::InvalidInput, dst, "dst_inside_src", "destination is inside the source"));
    }
    // 6-7.
    check_volume(src, &parent, dst, sys::real_dev_path(src))?;
    // 8-9.
    do_clone(src, dst)
}

/// Clone one file or symlink (the link itself, never its target).
pub fn clone_file(src: &Path, dst: &Path) -> Result<()> {
    let st = sys::lstat(src).map_err(|e| classify_errno(e, IoSide::Read, src))?;
    if sys::is_dir(&st) {
        return Err(refusal(Code::IsDirectory, src, "directory", "source is a directory"));
    }
    if !sys::is_reg(&st) && !sys::is_lnk(&st) {
        return Err(refusal(Code::NotFile, src, sys::special_reason(&st), "source is not a regular file or symlink"));
    }
    let parent = check_dst(dst)?;
    // A (possibly dangling) symlink lives on its parent's volume.
    let src_dev = if sys::is_lnk(&st) {
        sys::real_dev_path(src.parent().unwrap_or(Path::new("/")))
    } else {
        sys::real_dev_path(src)
    };
    check_volume(src, &parent, dst, src_dev)?;
    do_clone(src, dst)
}

fn do_clone(src: &Path, dst: &Path) -> Result<()> {
    let cs = sys::cpath(src).map_err(|e| classify_errno(e, IoSide::Write, src))?;
    let cd = sys::cpath(dst).map_err(|e| classify_errno(e, IoSide::Write, dst))?;
    match sys::retry(|| unsafe { libc::clonefile(cs.as_ptr(), cd.as_ptr(), CLONE_FLAGS) }) {
        Ok(_) => Ok(()),
        Err(e) => Err(fill_dst_exists(clone_errno(e, dst), dst)),
    }
}

/// Map a clonefile errno (write side) with a clearer message for EACCES.
pub(crate) fn clone_errno(e: i32, dst: &Path) -> StrataError {
    let mut err = classify_errno(e, IoSide::Write, dst);
    if e == libc::EACCES {
        err.message = format!(
            "{}: permission denied: either the destination's parent is not writable or an entry in the source tree is unreadable",
            dst.display()
        );
    }
    err
}

/// Steps 2-4 of §5.2: shape, absence (lstat), parent. Returns the parent.
pub(crate) fn check_dst(dst: &Path) -> Result<PathBuf> {
    check_dst_shape(dst)?;
    match sys::lstat(dst) {
        Ok(_) => {
            return Err(with(
                errno_error(Code::Exists, libc::EEXIST, dst, None),
                "dst_exists",
                json!(true),
            ));
        }
        Err(libc::ENOENT) => {}
        Err(e) => return Err(classify_errno(e, IoSide::Read, dst)),
    }
    let parent = match dst.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    match std::fs::metadata(&parent) {
        Ok(m) if m.is_dir() => Ok(parent),
        Ok(_) => Err(errno_error(Code::NotDirectory, libc::ENOTDIR, &parent, Some("parent_not_a_directory"))),
        Err(e) => {
            let errno = e.raw_os_error().unwrap_or(libc::EIO);
            Err(classify_errno(errno, IoSide::Read, &parent))
        }
    }
}

pub(crate) fn check_dst_shape(dst: &Path) -> Result<()> {
    let bytes = dst.as_os_str().as_bytes();
    if bytes.ends_with(b"/") {
        return Err(refusal(Code::InvalidInput, dst, "trailing_slash", "destination ends with '/'"));
    }
    if let Some(name) = dst.file_name()
        && utf16_units(name.as_bytes()) > NAME_MAX_UTF16_UNITS
    {
        return Err(errno_error(Code::InvalidInput, libc::ENAMETOOLONG, dst, Some("name_too_long")));
    }
    if bytes.len() >= PATH_MAX_BYTES {
        return Err(errno_error(Code::InvalidInput, libc::ENAMETOOLONG, dst, Some("path_too_long")));
    }
    Ok(())
}

pub(crate) fn utf16_units(name: &[u8]) -> usize {
    String::from_utf8_lossy(name).encode_utf16().count()
}

/// Steps 6-7: same volume, then clone capability.
fn check_volume(src: &Path, parent: &Path, dst: &Path, src_dev: std::result::Result<i32, i32>) -> Result<()> {
    let sd = src_dev.map_err(|e| classify_errno(e, IoSide::Read, src))?;
    let dd = sys::real_dev_path(parent).map_err(|e| classify_errno(e, IoSide::Read, parent))?;
    check_devs(sd, dd, src, dst)
}

pub(crate) fn check_devs(sd: i32, dd: i32, src: &Path, dst: &Path) -> Result<()> {
    if sd != dd {
        let e = errno_error(Code::CrossVolume, libc::EXDEV, dst, Some("cross_volume"));
        let e = with(e, "src_dev", json!(sd));
        let e = with(e, "dst_dev", json!(dd));
        return Err(with(e, "dst_exists", json!(false)));
    }
    let probe = if sys::lstat(src).map(|s| sys::is_lnk(&s)).unwrap_or(false) {
        src.parent().unwrap_or(Path::new("/"))
    } else {
        src
    };
    let caps = crate::volume_caps(probe)?;
    if caps.clone == Some(false) {
        let mut e = refusal(Code::Unsupported, dst, "clone_unsupported", "the volume does not support clonefile");
        set_detail(&mut e, "fs_type", json!(caps.fs_type));
        set_detail(&mut e, "dst_exists", json!(false));
        return Err(e);
    }
    Ok(())
}

/// Step 9: record whether dst exists after a failure.
pub(crate) fn fill_dst_exists(mut e: StrataError, dst: &Path) -> StrataError {
    let exists = sys::lstat(dst).is_ok();
    set_detail(&mut e, "dst_exists", json!(exists));
    if e.code == Code::Unsupported
        && let Ok(c) = crate::volume_caps(dst.parent().unwrap_or(Path::new("/")))
    {
        set_detail(&mut e, "fs_type", json!(c.fs_type));
    }
    e
}
