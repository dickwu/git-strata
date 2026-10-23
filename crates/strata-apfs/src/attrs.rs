//! `Attrs`, `FileKind`, the gen count contract and `attrs_of` (§3, §5.5).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use strata_types::{Code, RepoPath};

use crate::bulk;
use crate::error::{IoSide, classify_errno, errno_error};
use crate::hooks as seams;
use crate::rooted::{check_root, resolve};
use crate::{GEN_COUNT_UNAVAILABLE, Result};

/// Object type. A symlink carries its target's raw bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileKind {
    File,
    Dir,
    Symlink { target: PathBuf },
    Fifo,
    Socket,
    CharDevice,
    BlockDevice,
}

/// Attributes of one entry, lstat semantics (a symlink is the link itself).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attrs {
    pub kind: FileKind,
    /// ATTR_FILE_DATALENGTH; 0 for directories, FIFOs and sockets; a
    /// symlink's target length.
    pub size: u64,
    /// The low 12 bits of st_mode.
    pub mode: u32,
    /// ATTR_CMN_GEN_COUNT; GEN_COUNT_UNAVAILABLE (0) = unknown. Never compared
    /// alone: use `unchanged_since`.
    pub gen_count: u64,
    pub mtime: SystemTime,
    pub ctime: SystemTime,
    /// ATTR_CMN_FILEID, meaningful within one volume.
    pub inode: u64,
}

impl Attrs {
    /// "Not written since `earlier`" (§4.2 C, §5.5): same kind (symlink
    /// target included), inode, size and mode, and both gen counts known and
    /// equal. Unknown on either side is `false`. Times do not take part.
    pub fn unchanged_since(&self, earlier: &Attrs) -> bool {
        self.kind == earlier.kind
            && self.inode == earlier.inode
            && self.size == earlier.size
            && self.mode == earlier.mode
            && self.gen_count != GEN_COUNT_UNAVAILABLE
            && self.gen_count == earlier.gen_count
    }

    /// Whether `gen_count` is known.
    pub fn gen_count_known(&self) -> bool {
        self.gen_count != GEN_COUNT_UNAVAILABLE
    }

    /// The later of mtime and ctime (gc's age, addendum 4).
    pub fn last_changed(&self) -> SystemTime {
        self.mtime.max(self.ctime)
    }
}

fn point_error(errno: i32, shown: &Path) -> crate::StrataErrorAlias {
    match errno {
        libc::ENOENT => errno_error(Code::NoSuchPath, errno, shown, None),
        libc::ENAMETOOLONG => errno_error(Code::InvalidInput, errno, shown, Some("name_too_long")),
        _ => classify_errno(errno, IoSide::Read, shown),
    }
}

/// Attributes of `root/path`: parents resolved with O_NOFOLLOW one level at a
/// time, the last component with lstat semantics (§5.5).
pub fn attrs_of(root: &Path, path: &RepoPath) -> Result<Attrs> {
    check_root(root)?;
    let mut r = resolve(root, path, IoSide::Read, 0)?;
    let mut tries = 0;
    loop {
        tries += 1;
        seams::before_open(r.step, &r.name);
        r.step += 1;
        let buf = bulk::getattrlistat(r.dir.0, &r.name).map_err(|e| point_error(e, &r.shown))?;
        let raw = bulk::parse_records(&buf, 1).pop().expect("one record").map_err(|mut e| {
            crate::error::set_detail(&mut e, "path", r.shown.to_string_lossy().into());
            e
        })?;
        let Some(mut attrs) = bulk::into_attrs(&raw) else {
            return Err(errno_error(Code::Unknown, libc::EIO, &r.shown, Some("incomplete_attributes")));
        };
        if let FileKind::Symlink { target } = &mut attrs.kind {
            match bulk::readlink_at(r.dir.0, &r.name) {
                Ok(t) => *target = t,
                // Swapped between the two calls: read again once.
                Err(libc::ENOENT) | Err(libc::EINVAL) if tries < 2 => continue,
                Err(libc::ENOENT) | Err(libc::EINVAL) => {
                    return Err(errno_error(Code::NoSuchPath, libc::ENOENT, &r.shown, None));
                }
                Err(e) => return Err(classify_errno(e, IoSide::Read, &r.shown)),
            }
        }
        return Ok(attrs);
    }
}
