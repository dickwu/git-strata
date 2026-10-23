//! Resolution under a root without following symlinks (§5.4): `check_root`,
//! `resolve`, `open_root`, `open_rooted`, `open_rooted_file`.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use strata_types::{Code, RepoPath};

use crate::Result;
use crate::error::{IoSide, classify_errno, errno_error, refusal};
use crate::hooks as seams;
use crate::sys;

/// An owned descriptor; closed on drop.
#[derive(Debug)]
pub(crate) struct Fd(pub(crate) RawFd);

impl Fd {
    pub(crate) fn into_file(self) -> File {
        let fd = self.0;
        std::mem::forget(self);
        unsafe { File::from_raw_fd(fd) }
    }
}

impl Drop for Fd {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

impl AsRawFd for Fd {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

pub(crate) const DIR_FLAGS: libc::c_int = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
pub(crate) const FINAL_FLAGS: libc::c_int = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;

/// `openat` with EINTR retry; returns the errno on failure.
pub(crate) fn openat(dirfd: RawFd, name: &OsStr, flags: libc::c_int, mode: libc::c_uint) -> std::result::Result<Fd, i32> {
    let c = sys::cos(name)?;
    let fd = sys::retry(|| unsafe { libc::openat(dirfd, c.as_ptr(), flags, mode) })?;
    Ok(Fd(fd))
}

/// The canonical-root check (§5.4 step 1), on raw bytes. Every function that
/// takes a root calls this first.
pub(crate) fn check_root(root: &Path) -> Result<()> {
    let b = root.as_os_str().as_bytes();
    let bad = b.is_empty()
        || b[0] != b'/'
        || (b.len() > 1 && b.ends_with(b"/"))
        || b.split(|&c| c == b'/').skip(1).any(|c| c == b"." || c == b".." || (c.is_empty()));
    // `/` itself splits into ["", ""]; it is canonical.
    let bad = bad && b != b"/";
    if bad {
        return Err(refusal(
            Code::InvalidInput,
            root,
            "root_not_canonical",
            "root must be an absolute path without a trailing '/', '.' or '..' components",
        ));
    }
    Ok(())
}

/// Classify a failed `openat` of an intermediate directory (or the root).
fn dir_open_error(errno: i32, dirfd: Option<RawFd>, name: &OsStr, side: IoSide, shown: &Path) -> crate::StrataErrorAlias {
    match errno {
        libc::ENOENT => errno_error(Code::NoSuchPath, errno, shown, None),
        libc::ENOTDIR | libc::ELOOP => {
            let st = match dirfd {
                Some(d) => sys::fstatat_nofollow(d, name),
                None => sys::lstat(shown),
            };
            let reason = match st {
                Ok(st) if sys::is_lnk(&st) => "symlink",
                _ => "not_a_directory",
            };
            errno_error(Code::NotDirectory, errno, shown, Some(reason))
        }
        libc::ENAMETOOLONG => errno_error(Code::InvalidInput, errno, shown, Some("name_too_long")),
        _ => classify_errno(errno, side, shown),
    }
}

pub(crate) fn open_root_fd(root: &Path, side: IoSide) -> Result<Fd> {
    check_root(root)?;
    let c = sys::cpath(root).map_err(|e| classify_errno(e, side, root))?;
    match sys::retry(|| unsafe { libc::open(c.as_ptr(), DIR_FLAGS) }) {
        Ok(fd) => Ok(Fd(fd)),
        Err(e) => Err(dir_open_error(e, None, root.as_os_str(), side, root)),
    }
}

/// A resolved parent directory and the last name under it.
pub(crate) struct Resolved {
    /// Handle of the parent directory (the root for a one-component path).
    pub(crate) dir: Fd,
    /// The last component.
    pub(crate) name: OsString,
    /// The next `before_open` step number.
    pub(crate) step: usize,
    /// `root/path`, for messages and detail.
    pub(crate) shown: PathBuf,
}

pub(crate) fn components(path: &RepoPath) -> Vec<&OsStr> {
    path.as_str().split('/').map(OsStr::new).collect()
}

/// `resolve` (§5.4 steps 1–2): open the root, then each parent component with
/// `O_DIRECTORY | O_NOFOLLOW`, holding only the current level's handle.
/// `step` is where this call's seam numbering starts.
pub(crate) fn resolve(root: &Path, path: &RepoPath, side: IoSide, step: usize) -> Result<Resolved> {
    let mut dir = open_root_fd(root, side)?;
    let comps = components(path);
    let shown = root.join(path.as_str());
    let mut step = step;
    let mut walked = root.to_path_buf();
    for comp in &comps[..comps.len() - 1] {
        seams::before_open(step, comp);
        step += 1;
        walked.push(comp);
        match openat(dir.0, comp, DIR_FLAGS, 0) {
            Ok(next) => dir = next,
            Err(e) => return Err(dir_open_error(e, Some(dir.0), comp, side, &walked)),
        }
    }
    Ok(Resolved { dir, name: comps[comps.len() - 1].to_os_string(), step, shown })
}

/// Open the last component for reading (the `open_rooted` landing): shared by
/// `open_rooted`, `open_rooted_file` and `clone_from_rooted`. Errors are
/// returned raw so callers can map `ELOOP` / `EOPNOTSUPP` by their own rules.
pub(crate) fn open_final(r: &mut Resolved) -> std::result::Result<Fd, i32> {
    seams::before_open(r.step, &r.name);
    r.step += 1;
    openat(r.dir.0, &r.name, FINAL_FLAGS, 0)
}

fn final_error(errno: i32, shown: &Path) -> crate::StrataErrorAlias {
    match errno {
        libc::ELOOP => errno_error(Code::NotFile, errno, shown, Some("symlink")),
        libc::EOPNOTSUPP => errno_error(Code::NotFile, errno, shown, Some("socket")),
        libc::ENOENT => errno_error(Code::NoSuchPath, errno, shown, None),
        libc::ENAMETOOLONG => errno_error(Code::InvalidInput, errno, shown, Some("name_too_long")),
        _ => classify_errno(errno, IoSide::Read, shown),
    }
}

/// Open the root itself (§5.4 step 1).
pub fn open_root(root: &Path) -> Result<File> {
    Ok(open_root_fd(root, IoSide::Read)?.into_file())
}

fn open_checked(root: &Path, path: &RepoPath) -> Result<(Fd, libc::stat, PathBuf)> {
    let mut r = resolve(root, path, IoSide::Read, 0)?;
    let fd = open_final(&mut r).map_err(|e| final_error(e, &r.shown))?;
    let st = sys::fstat(fd.0).map_err(|e| classify_errno(e, IoSide::Read, &r.shown))?;
    let fmt = sys::fmt_of(&st);
    if fmt == libc::S_IFCHR || fmt == libc::S_IFBLK {
        return Err(refusal(Code::NotFile, &r.shown, "device", "is a device"));
    }
    Ok((fd, st, r.shown))
}

/// Open `root/path` read-only, never following a symlink at any level.
pub fn open_rooted(root: &Path, path: &RepoPath) -> Result<File> {
    Ok(open_checked(root, path)?.0.into_file())
}

/// Like `open_rooted`, but only a regular file is accepted.
pub fn open_rooted_file(root: &Path, path: &RepoPath) -> Result<File> {
    let (fd, st, shown) = open_checked(root, path)?;
    if sys::is_reg(&st) {
        return Ok(fd.into_file());
    }
    drop(fd);
    if sys::is_dir(&st) {
        return Err(refusal(Code::IsDirectory, &shown, "directory", "is a directory"));
    }
    Err(refusal(Code::NotFile, &shown, sys::special_reason(&st), "is not a regular file"))
}
