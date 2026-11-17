//! Volume identity and capabilities (§5.1).

use std::ffi::CStr;
use std::io;
use std::os::unix::ffi::OsStrExt;
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
            Err(e)
                if e.kind() == io::ErrorKind::NotFound
                    || e.raw_os_error() == Some(libc::ENOTDIR) =>
            {
                match cur.parent() {
                    Some(p) => cur = p,
                    None => return Err(e),
                }
            }
            Err(e) => return Err(e),
        }
    }
}

/// Space of the file system holding a path, in bytes ([`volume_space`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VolumeSpace {
    /// `f_bavail × f_bsize`: what an unprivileged writer can still
    /// allocate.
    pub free: u64,
    /// `f_blocks × f_bsize`: the capacity.
    pub size: u64,
}

/// `statfs` of the file system holding `path` (symlinks followed). On APFS
/// both figures are the whole container's, which every volume in it
/// shares; `free` leaves out purgeable space (local snapshots, caches
/// macOS may evict), so it is below what Finder calls available, and
/// `size − free` is everything stored in the container, not what one
/// program uses. Errors carry the `statfs` errno.
pub fn volume_space(path: &Path) -> io::Result<VolumeSpace> {
    let c = sys::cpath(path).map_err(io::Error::from_raw_os_error)?;
    let mut sfs: libc::statfs = unsafe { std::mem::zeroed() };
    sys::retry(|| unsafe { libc::statfs(c.as_ptr(), &mut sfs) })
        .map_err(io::Error::from_raw_os_error)?;
    let block = u64::from(sfs.f_bsize);
    Ok(VolumeSpace {
        free: sfs.f_bavail.saturating_mul(block),
        size: sfs.f_blocks.saturating_mul(block),
    })
}

/// Which volume holds a path ([`volume_label`]): what a person would call it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeLabel {
    /// `statfs.f_mntonname`: where the volume is mounted (`/Volumes/GWD`,
    /// `/System/Volumes/Data` for the startup disk's data volume).
    pub mount: PathBuf,
    /// The volume's name (`ATTR_VOL_NAME`: `GWD`, `Macintosh HD - Data`);
    /// the mount point's last component when the volume gives none, `/`
    /// for the root.
    pub name: String,
}

/// `statfs` and `getattrlist(ATTR_VOL_NAME)` of the file system holding
/// `path` (symlinks and firmlinks followed, like [`volume_space`]). Errors
/// carry the errno of whichever call failed.
pub fn volume_label(path: &Path) -> io::Result<VolumeLabel> {
    let c = sys::cpath(path).map_err(io::Error::from_raw_os_error)?;
    let mut sfs: libc::statfs = unsafe { std::mem::zeroed() };
    sys::retry(|| unsafe { libc::statfs(c.as_ptr(), &mut sfs) })
        .map_err(io::Error::from_raw_os_error)?;
    // The mount point's own bytes, never a lossy copy. The name is read
    // there, on the volume statfs reported, so mount and name always
    // describe the same volume.
    let raw = unsafe { CStr::from_ptr(sfs.f_mntonname.as_ptr()) };
    let mount = PathBuf::from(std::ffi::OsStr::from_bytes(raw.to_bytes()));
    let mc = raw.to_owned();
    let mut al = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: libc::ATTR_VOL_INFO | libc::ATTR_VOL_NAME,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut buf = [0u8; 4 + 8 + 1024];
    sys::retry(|| unsafe {
        libc::getattrlist(
            mc.as_ptr(),
            &mut al as *mut _ as *mut _,
            buf.as_mut_ptr() as *mut _,
            buf.len(),
            0,
        )
    })
    .map_err(io::Error::from_raw_os_error)?;
    Ok(VolumeLabel {
        name: attr_name(&buf).unwrap_or_else(|| fallback_name(&mount)),
        mount,
    })
}

/// The `attrreference_t` name after the u32 length: offset (i32, from the
/// reference itself) and length (u32, with the NUL).
fn attr_name(buf: &[u8]) -> Option<String> {
    let off = sys::rd_i32(buf, 4)?;
    let len = sys::rd_u32(buf, 8)? as usize;
    let start = 4usize.checked_add(usize::try_from(off).ok()?)?;
    let bytes = buf.get(start..start.checked_add(len)?)?;
    let name = CStr::from_bytes_until_nul(bytes)
        .ok()?
        .to_string_lossy()
        .into_owned();
    (!name.is_empty()).then_some(name)
}

fn fallback_name(mount: &Path) -> String {
    mount
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| mount.to_string_lossy().into_owned())
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

#[cfg(test)]
mod label_tests {
    use super::*;

    /// A getattrlist reply: u32 length, attrreference (offset from itself,
    /// length with the NUL), then the name.
    fn reply(off: i32, len: u32, name: &[u8]) -> Vec<u8> {
        let mut b = vec![0u8; 12];
        b[4..8].copy_from_slice(&off.to_ne_bytes());
        b[8..12].copy_from_slice(&len.to_ne_bytes());
        b.extend_from_slice(name);
        b
    }

    #[test]
    fn attr_name_reads_the_reference_and_refuses_bad_ones() {
        assert_eq!(attr_name(&reply(8, 4, b"GWD\0")), Some("GWD".into()));
        assert_eq!(
            attr_name(&reply(8, 6, "Wörk\0".as_bytes())),
            Some("Wörk".into())
        );
        assert_eq!(attr_name(&reply(8, 1, b"\0")), None, "empty name");
        assert_eq!(attr_name(&reply(-4, 4, b"GWD\0")), None, "negative offset");
        assert_eq!(attr_name(&reply(8, 40, b"GWD\0")), None, "past the buffer");
        assert_eq!(attr_name(&reply(8, 3, b"GWD")), None, "no NUL");
        assert_eq!(attr_name(&[0u8; 6]), None, "truncated reference");
    }

    #[test]
    fn fallback_name_is_the_mount_points_last_component() {
        assert_eq!(fallback_name(Path::new("/Volumes/GWD")), "GWD");
        assert_eq!(fallback_name(Path::new("/")), "/");
    }
}
