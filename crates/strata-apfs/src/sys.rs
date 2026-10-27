//! Small libc helpers shared by the modules.

use std::ffi::{CString, OsStr};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

pub(crate) fn errno() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// NUL-terminated copy of a path; interior NUL becomes EINVAL.
pub(crate) fn cpath(p: &Path) -> Result<CString, i32> {
    cos(p.as_os_str())
}

pub(crate) fn cos(s: &OsStr) -> Result<CString, i32> {
    CString::new(s.as_bytes()).map_err(|_| libc::EINVAL)
}

/// Calls [`retry`] makes in all before an `EINTR` is returned (review
/// P1-6). Signals interrupt a call a few times at most; a volume that keeps
/// interrupting (TCC on a removable volume) must reach the caller's own
/// bounded [`crate::retry_eintr`] / the daemon's startup wait, which can
/// report it and be cancelled.
pub(crate) const SYS_EINTR_ATTEMPTS: u32 = 64;

/// Retry a syscall returning -1 on EINTR, at most [`SYS_EINTR_ATTEMPTS`]
/// calls; then the `EINTR` is returned.
pub(crate) fn retry<T: Copy + PartialEq + From<i8>>(f: impl FnMut() -> T) -> Result<T, i32> {
    retry_n(SYS_EINTR_ATTEMPTS, f)
}

pub(crate) fn retry_n<T: Copy + PartialEq + From<i8>>(attempts: u32, mut f: impl FnMut() -> T) -> Result<T, i32> {
    let mut n = 1;
    loop {
        let r = f();
        if r == T::from(-1) {
            let e = errno();
            if e == libc::EINTR && n < attempts {
                n += 1;
                continue;
            }
            return Err(e);
        }
        return Ok(r);
    }
}

pub(crate) fn lstat(p: &Path) -> Result<libc::stat, i32> {
    let c = cpath(p)?;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    retry(|| unsafe { libc::lstat(c.as_ptr(), &mut st) })?;
    Ok(st)
}

pub(crate) fn fstat(fd: i32) -> Result<libc::stat, i32> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    retry(|| unsafe { libc::fstat(fd, &mut st) })?;
    Ok(st)
}

pub(crate) fn fstatat_nofollow(dirfd: i32, name: &OsStr) -> Result<libc::stat, i32> {
    let c = cos(name)?;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    retry(|| unsafe { libc::fstatat(dirfd, c.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) })?;
    Ok(st)
}

pub(crate) fn fmt_of(st: &libc::stat) -> libc::mode_t {
    st.st_mode & libc::S_IFMT
}

pub(crate) fn is_dir(st: &libc::stat) -> bool {
    fmt_of(st) == libc::S_IFDIR
}

pub(crate) fn is_lnk(st: &libc::stat) -> bool {
    fmt_of(st) == libc::S_IFLNK
}

pub(crate) fn is_reg(st: &libc::stat) -> bool {
    fmt_of(st) == libc::S_IFREG
}

/// Short reason name for a non-regular, non-directory object.
pub(crate) fn special_reason(st: &libc::stat) -> &'static str {
    match fmt_of(st) {
        libc::S_IFIFO => "fifo",
        libc::S_IFSOCK => "socket",
        libc::S_IFLNK => "symlink",
        libc::S_IFCHR | libc::S_IFBLK => "device",
        libc::S_IFDIR => "directory",
        _ => "not_a_file",
    }
}

/// Real device id of a path (follows links), `FSOPT_RETURN_REALDEV`.
pub(crate) fn real_dev_path(p: &Path) -> Result<i32, i32> {
    let c = cpath(p)?;
    let mut al = devid_attrlist();
    let mut buf = [0u8; 16];
    retry(|| unsafe {
        libc::getattrlist(
            c.as_ptr(),
            &mut al as *mut _ as *mut _,
            buf.as_mut_ptr() as *mut _,
            buf.len(),
            libc::FSOPT_RETURN_REALDEV,
        )
    })?;
    Ok(i32::from_ne_bytes(buf[4..8].try_into().unwrap()))
}

/// Real device id of an open descriptor.
pub(crate) fn real_dev_fd(fd: i32) -> Result<i32, i32> {
    let mut al = devid_attrlist();
    let mut buf = [0u8; 16];
    retry(|| unsafe {
        libc::fgetattrlist(
            fd,
            &mut al as *mut _ as *mut _,
            buf.as_mut_ptr() as *mut _,
            buf.len(),
            libc::FSOPT_RETURN_REALDEV,
        )
    })?;
    Ok(i32::from_ne_bytes(buf[4..8].try_into().unwrap()))
}

fn devid_attrlist() -> libc::attrlist {
    libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_DEVID,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    }
}

pub(crate) fn rd_u32(b: &[u8], off: usize) -> Option<u32> {
    b.get(off..off + 4).map(|s| u32::from_ne_bytes(s.try_into().unwrap()))
}

pub(crate) fn rd_i32(b: &[u8], off: usize) -> Option<i32> {
    b.get(off..off + 4).map(|s| i32::from_ne_bytes(s.try_into().unwrap()))
}

pub(crate) fn rd_u64(b: &[u8], off: usize) -> Option<u64> {
    b.get(off..off + 8).map(|s| u64::from_ne_bytes(s.try_into().unwrap()))
}

pub(crate) fn rd_i64(b: &[u8], off: usize) -> Option<i64> {
    b.get(off..off + 8).map(|s| i64::from_ne_bytes(s.try_into().unwrap()))
}

/// The path the kernel reports for a descriptor (`F_GETPATH`), for messages.
pub(crate) fn fd_path(fd: i32) -> std::path::PathBuf {
    let mut buf = vec![0u8; libc::PATH_MAX as usize + 1];
    let r = unsafe { libc::fcntl(fd, libc::F_GETPATH, buf.as_mut_ptr()) };
    if r == -1 {
        return std::path::PathBuf::from(format!("<fd {fd}>"));
    }
    let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    buf.truncate(n);
    std::path::PathBuf::from(<std::ffi::OsString as std::os::unix::ffi::OsStringExt>::from_vec(buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fail_with(e: i32) -> i32 {
        // SAFETY: errno is thread-local.
        unsafe { *libc::__error() = e };
        -1
    }

    /// Review P1-6: an endless EINTR ends with EINTR, not a hang.
    #[test]
    fn endless_eintr_is_bounded() {
        let mut calls = 0;
        let r = retry(|| {
            calls += 1;
            fail_with(libc::EINTR)
        });
        assert_eq!(r, Err(libc::EINTR));
        assert_eq!(calls, SYS_EINTR_ATTEMPTS);
    }

    #[test]
    fn eintr_then_success_and_other_errors() {
        let mut calls = 0;
        let r = retry(|| {
            calls += 1;
            if calls < 3 { fail_with(libc::EINTR) } else { 5 }
        });
        assert_eq!(r, Ok(5));
        assert_eq!(calls, 3);
        let mut calls = 0;
        let r = retry(|| {
            calls += 1;
            fail_with(libc::EACCES)
        });
        assert_eq!(r, Err(libc::EACCES));
        assert_eq!(calls, 1);
    }
}
