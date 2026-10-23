//! Small OS helpers: real paths via `F_GETPATH`, lstat classification,
//! random session ids.

use std::ffi::CString;
use std::fs::Metadata;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::FileTypeExt;
use std::path::{Component, Path, PathBuf};

use strata_apfs::{IoSide, classify_io_error};
use strata_types::SessionId;

use crate::Result;

/// The volume's spelling of `p` with symlinks resolved: open with
/// `O_DIRECTORY` and ask `fcntl(F_GETPATH)`. Errors go through apfs's
/// classification with `IoSide::Read`.
pub(crate) fn real_dir(p: &Path) -> Result<PathBuf> {
    real_dir_io(p).map_err(|e| classify_io_error(&e, IoSide::Read, p))
}

pub(crate) fn real_dir_io(p: &Path) -> io::Result<PathBuf> {
    let c = CString::new(p.as_os_str().as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    // SAFETY: c is a valid NUL-terminated string; the fd is closed below.
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buf = vec![0u8; libc::PATH_MAX as usize + 1];
    // SAFETY: F_GETPATH writes at most MAXPATHLEN bytes into buf.
    let r = unsafe { libc::fcntl(fd, libc::F_GETPATH, buf.as_mut_ptr()) };
    let err = io::Error::last_os_error();
    // SAFETY: fd came from open above.
    unsafe { libc::close(fd) };
    if r < 0 {
        return Err(err);
    }
    let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    buf.truncate(n);
    Ok(PathBuf::from(std::ffi::OsString::from_vec(buf)))
}

/// `lstat`; `Ok(None)` when the path (or a parent) does not exist.
pub(crate) fn lstat(p: &Path) -> io::Result<Option<Metadata>> {
    match std::fs::symlink_metadata(p) {
        Ok(m) => Ok(Some(m)),
        Err(e) if e.raw_os_error() == Some(libc::ENOENT) || e.raw_os_error() == Some(libc::ENOTDIR) => {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// The `file_type` detail for a non-regular, non-directory entry.
pub(crate) fn special_kind(m: &Metadata) -> Option<&'static str> {
    let t = m.file_type();
    if t.is_fifo() {
        Some("fifo")
    } else if t.is_socket() {
        Some("socket")
    } else if t.is_char_device() {
        Some("char_device")
    } else if t.is_block_device() {
        Some("block_device")
    } else {
        None
    }
}

/// Is `inner` equal to or below `outer`, by path components; ASCII
/// case-insensitively when `fold`.
pub(crate) fn within(inner: &Path, outer: &Path, fold: bool) -> bool {
    let a: Vec<Component> = inner.components().collect();
    let b: Vec<Component> = outer.components().collect();
    if a.len() < b.len() {
        return false;
    }
    a.iter().zip(b.iter()).all(|(x, y)| {
        let (x, y) = (x.as_os_str().as_bytes(), y.as_os_str().as_bytes());
        if fold { x.eq_ignore_ascii_case(y) } else { x == y }
    })
}

/// A fresh session id: 40 random bits as `s-` + 8 lowercase base32 chars.
/// `2 * bytes` lowercase hex digits from arc4random.
pub(crate) fn random_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    // SAFETY: arc4random_buf fills exactly b.len() bytes.
    unsafe { libc::arc4random_buf(b.as_mut_ptr().cast(), b.len()) };
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A RepoPath from a path this crate composes itself.
pub(crate) fn rp(s: impl Into<String>) -> strata_types::RepoPath {
    let s = s.into();
    strata_types::RepoPath::try_from(s.clone()).unwrap_or_else(|e| panic!("composed path {s:?} is not a RepoPath: {e:?}"))
}

pub(crate) fn random_session_id() -> SessionId {
    let mut b = [0u8; 5];
    // SAFETY: arc4random_buf fills exactly b.len() bytes.
    unsafe { libc::arc4random_buf(b.as_mut_ptr().cast(), b.len()) };
    let mut v: u64 = 0;
    for x in b {
        v = (v << 8) | u64::from(x);
    }
    const ALPHA: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut s = String::from("s-");
    for i in (0..8).rev() {
        s.push(ALPHA[((v >> (i * 5)) & 31) as usize] as char);
    }
    SessionId::try_from(s).expect("generated ids have the session-id form")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_have_the_form() {
        for _ in 0..100 {
            let id = random_session_id();
            assert!(crate::ticket::looks_like_session_id(id.as_str()));
        }
    }

    #[test]
    fn within_by_components() {
        assert!(within(Path::new("/a/b/c"), Path::new("/a/b"), false));
        assert!(within(Path::new("/a/b"), Path::new("/a/b"), false));
        assert!(!within(Path::new("/a/bc"), Path::new("/a/b"), false));
        assert!(!within(Path::new("/A/b"), Path::new("/a/b"), false));
        assert!(within(Path::new("/A/b/x"), Path::new("/a/B"), true));
    }
}
