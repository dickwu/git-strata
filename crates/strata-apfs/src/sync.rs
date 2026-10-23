//! `full_fsync` (§5.4): `fcntl(F_FULLFSYNC)` with EINTR retry.

use std::fs::File;
use std::os::fd::AsRawFd;

use strata_types::Code;

use crate::Result;
use crate::error::errno_error;
use crate::sys;

/// Flush data and metadata all the way to the medium (`F_FULLFSYNC`).
/// Works on regular-file handles, directory handles from `open_rooted`, and
/// the root handle from `open_root`.
pub fn full_fsync(file: &File) -> Result<()> {
    let fd = file.as_raw_fd();
    match sys::retry(|| unsafe { libc::fcntl(fd, libc::F_FULLFSYNC) }) {
        Ok(_) => Ok(()),
        Err(e) => {
            let path = sys::fd_path(fd);
            let code = if e == libc::ENOTSUP || e == libc::EOPNOTSUPP { Code::Unsupported } else { Code::Unknown };
            Err(errno_error(code, e, &path, None))
        }
    }
}
