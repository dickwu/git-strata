//! Volume identity and capabilities (§5.1).

use std::ffi::CStr;
use std::io;
use std::path::{Path, PathBuf};


use crate::error::{IoSide, classify_errno};
use crate::sys;

/// Whether two existing paths are on the same volume. Follows symlinks and
/// firmlinks; compares the *real* device (`FSOPT_RETURN_REALDEV`), so the
/// sealed system volume and the data volume differ even though `st_dev`
/// agrees across firmlinks.
pub fn same_volume(a: &Path, b: &Path) -> io::Result<bool> {
    let da = sys::real_dev_path(a).map_err(io::Error::from_raw_os_error)?;
    let db = sys::real_dev_path(b).map_err(io::Error::from_raw_os_error)?;
    Ok(da == db)
}

/// The first path, walking up from `path`, that exists (`stat`, following
/// links). `ENOENT` and `ENOTDIR` climb; any other error is returned as is.
/// Relative paths are `InvalidInput`. No realpath is applied.
pub fn nearest_existing_ancestor(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{}: path must be absolute", path.display()),
        ));
    }
    let mut cur: &Path = path;
    loop {
        match std::fs::metadata(cur) {
            Ok(_) => return Ok(cur.to_path_buf()),
            Err(e) if e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(libc::ENOTDIR) => {
                match cur.parent() {
                    Some(p) => cur = p,
                    None => return Err(e),
                }
            }
            Err(e) => return Err(e),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeCaps {
    /// `statfs.f_fstypename`: `apfs`, `hfs`, `msdos`, ...
    pub fs_type: String,
    /// `Some(true)` clonefile supported, `Some(false)` explicitly not,
    /// `None` the volume does not say (valid bit unset, e.g. HFS+).
    pub clone: Option<bool>,
    pub case_sensitive: bool,
    pub write_gen_count: bool,
}

pub fn volume_caps(path: &Path) -> crate::Result<VolumeCaps> {
    let c = sys::cpath(path).map_err(|e| classify_errno(e, IoSide::Read, path))?;
    let mut sfs: libc::statfs = unsafe { std::mem::zeroed() };
    sys::retry(|| unsafe { libc::statfs(c.as_ptr(), &mut sfs) })
        .map_err(|e| classify_errno(e, IoSide::Read, path))?;
    let fs_type = unsafe { CStr::from_ptr(sfs.f_fstypename.as_ptr()) }
        .to_string_lossy()
        .into_owned();

    let mut al = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: libc::ATTR_VOL_INFO | libc::ATTR_VOL_CAPABILITIES,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut buf = [0u8; 64];
    sys::retry(|| unsafe {
        libc::getattrlist(
            c.as_ptr(),
            &mut al as *mut _ as *mut _,
            buf.as_mut_ptr() as *mut _,
            buf.len(),
            0,
        )
    })
    .map_err(|e| classify_errno(e, IoSide::Read, path))?;
    // u32 length, then vol_capabilities_attr_t: capabilities[4], valid[4].
    let cap = |i: usize| sys::rd_u32(&buf, 4 + 4 * i).unwrap_or(0);
    let valid = |i: usize| sys::rd_u32(&buf, 20 + 4 * i).unwrap_or(0);
    let fmt = libc::VOL_CAPABILITIES_FORMAT;
    let int = libc::VOL_CAPABILITIES_INTERFACES;
    let flag = |idx: usize, bit: u32| valid(idx) & bit != 0 && cap(idx) & bit != 0;
    let clone = if valid(int) & libc::VOL_CAP_INT_CLONE != 0 {
        Some(cap(int) & libc::VOL_CAP_INT_CLONE != 0)
    } else {
        None
    };
    Ok(VolumeCaps {
        fs_type,
        clone,
        case_sensitive: flag(fmt, libc::VOL_CAP_FMT_CASE_SENSITIVE),
        write_gen_count: flag(fmt, libc::VOL_CAP_FMT_WRITE_GENERATION_COUNT),
    })
}

