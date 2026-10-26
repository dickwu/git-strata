//! `usage_of` (apfs.md §5.11): CoW-aware disk usage of one tree.
//!
//! One `getattrlistbulk(2)` pass per directory (never following links or
//! crossing mount points), three figures per regular file:
//!
//! * `logical`: `ATTR_FILE_DATALENGTH`, what `ls -l` shows and what a plain
//!   copy of the tree would need;
//! * `allocated`: `ATTR_FILE_DATAALLOCSIZE`, the blocks the file maps. A
//!   clone maps the same blocks as its origin, so summing this over clones
//!   counts a shared extent once per file (what `du` reports);
//! * `private`: `ATTR_CMNEXT_PRIVATESIZE` (APFS), the bytes of extents that
//!   no other file shares. A fresh `clonefile` copy and its origin both have
//!   0; a write into either makes the rewritten blocks private.
//!
//! So `private` is a lower bound of what removing the tree frees (an extent
//! shared only between files inside the tree is in neither file's private
//! size) and `allocated` an upper bound of what the tree occupies; the
//! difference is `shared`. There is no public API that names the other
//! holders of a shared extent (`F_LOG2PHYS_EXT` maps one file offset to one
//! device offset at a time, so deduplicating extents that way costs one
//! call per extent per file and is not done). Hard links count once per
//! scan (by file id). Symlinks count their target length as logical and
//! whatever they allocate. Volumes without `PRIVATESIZE` report `allocated`
//! as private and say so (`private_exact = false`).

use std::collections::HashSet;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::Path;

use strata_types::Code;

use crate::Result;
use crate::bulk::ATTR_CMN_ERROR;
use crate::error::{IoSide, classify_errno, errno_error};
use crate::rooted::{DIR_FLAGS, Fd, openat};
use crate::sys::{self, rd_i32, rd_i64, rd_u32, rd_u64};

/// Not in libc 0.2.191's apple module under these names (sys/attr.h).
const ATTR_FILE_LINKCOUNT: u32 = 0x0000_0001;
const ATTR_FILE_DATALENGTH: u32 = 0x0000_0200;
const ATTR_FILE_DATAALLOCSIZE: u32 = 0x0000_0400;
const ATTR_CMNEXT_PRIVATESIZE: u32 = 0x0000_0008;
const FSOPT_ATTR_CMN_EXTENDED: u64 = 0x0000_0020;
const FSOPT_NOFOLLOW: u64 = 0x0000_0001;

const VREG: u32 = 1;
const VDIR: u32 = 2;
const VLNK: u32 = 5;

const COMMON: u32 = libc::ATTR_CMN_RETURNED_ATTRS | libc::ATTR_CMN_NAME | ATTR_CMN_ERROR | libc::ATTR_CMN_OBJTYPE | libc::ATTR_CMN_FILEID;
const FILE: u32 = ATTR_FILE_LINKCOUNT | ATTR_FILE_DATALENGTH | ATTR_FILE_DATAALLOCSIZE;
const BUFFER: usize = 256 * 1024;
const MAX_DEPTH: u32 = 512;

/// Totals of one tree (or of several, through [`Usage::add`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// Sum of file lengths.
    pub logical: u64,
    /// Sum of allocated sizes (shared extents counted once per file).
    pub allocated: u64,
    /// Sum of private sizes (shared extents not counted at all).
    pub private: u64,
    pub files: u64,
    pub dirs: u64,
    /// Symlinks, FIFOs, sockets, devices.
    pub other: u64,
    /// Entries that could not be read (counted, never fatal).
    pub errors: u64,
    /// Every regular file reported `PRIVATESIZE`; `false` when at least one
    /// did not and its allocated size stood in.
    pub private_exact: bool,
}

impl Usage {
    pub fn empty() -> Usage {
        Usage { private_exact: true, ..Usage::default() }
    }

    pub fn add(&mut self, o: &Usage) {
        self.logical += o.logical;
        self.allocated += o.allocated;
        self.private += o.private;
        self.files += o.files;
        self.dirs += o.dirs;
        self.other += o.other;
        self.errors += o.errors;
        self.private_exact &= o.private_exact;
    }

    /// Bytes that are allocated but shared with at least one other file.
    pub fn shared(&self) -> u64 {
        self.allocated.saturating_sub(self.private)
    }
}

#[derive(Debug, Default)]
struct Rec {
    name: Option<OsString>,
    error: u32,
    objtype: u32,
    fileid: u64,
    links: u32,
    length: u64,
    alloc: u64,
    private: Option<u64>,
    mount_point: bool,
}

/// Parse one record by its returned bitmap (unaligned reads; every field is
/// 4-byte packed, groups in common, dir, file, extended-common order).
fn parse(rec: &[u8]) -> Option<Rec> {
    let common = rd_u32(rec, 4)?;
    let dir = rd_u32(rec, 12)?;
    let file = rd_u32(rec, 16)?;
    let ext = rd_u32(rec, 20)?;
    let mut r = Rec::default();
    let mut p = 24usize;
    if common & ATTR_CMN_ERROR != 0 {
        r.error = rd_u32(rec, p)?;
        p += 4;
    }
    if common & libc::ATTR_CMN_NAME != 0 {
        let rel = rd_i32(rec, p)? as i64;
        let len = rd_u32(rec, p + 4)? as i64;
        let start = p as i64 + rel;
        if len < 1 || start < 0 || start + len > rec.len() as i64 {
            return None;
        }
        let mut b = rec[start as usize..(start + len) as usize].to_vec();
        if let Some(z) = b.iter().position(|&c| c == 0) {
            b.truncate(z);
        }
        r.name = Some(OsString::from_vec(b));
        p += 8;
    }
    if common & libc::ATTR_CMN_OBJTYPE != 0 {
        r.objtype = rd_u32(rec, p)?;
        p += 4;
    }
    if common & libc::ATTR_CMN_FILEID != 0 {
        r.fileid = rd_u64(rec, p)?;
        p += 8;
    }
    if dir & libc::ATTR_DIR_MOUNTSTATUS != 0 {
        r.mount_point = rd_u32(rec, p)? & libc::DIR_MNTSTATUS_MNTPOINT != 0;
        p += 4;
    }
    if file & ATTR_FILE_LINKCOUNT != 0 {
        r.links = rd_u32(rec, p)?;
        p += 4;
    }
    if file & ATTR_FILE_DATALENGTH != 0 {
        r.length = rd_i64(rec, p)?.max(0) as u64;
        p += 8;
    }
    if file & ATTR_FILE_DATAALLOCSIZE != 0 {
        r.alloc = rd_i64(rec, p)?.max(0) as u64;
        p += 8;
    }
    if ext & ATTR_CMNEXT_PRIVATESIZE != 0 {
        r.private = Some(rd_i64(rec, p)?.max(0) as u64);
    }
    Some(r)
}

fn attrlist(bulk: bool) -> libc::attrlist {
    libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: if bulk { COMMON } else { COMMON & !(libc::ATTR_CMN_NAME | ATTR_CMN_ERROR) },
        volattr: 0,
        dirattr: libc::ATTR_DIR_MOUNTSTATUS,
        fileattr: FILE,
        forkattr: ATTR_CMNEXT_PRIVATESIZE,
    }
}

struct Scan<'s> {
    seen: HashSet<u64>,
    total: Usage,
    buf: Vec<u8>,
    stop: &'s dyn Fn() -> bool,
}

impl Scan<'_> {
    fn count(&mut self, r: &Rec) {
        match r.objtype {
            VREG => {
                if r.links > 1 && !self.seen.insert(r.fileid) {
                    return;
                }
                self.total.files += 1;
                self.total.logical += r.length;
                self.total.allocated += r.alloc;
                match r.private {
                    Some(p) => self.total.private += p,
                    None => {
                        self.total.private += r.alloc;
                        self.total.private_exact = false;
                    }
                }
            }
            VDIR => self.total.dirs += 1,
            VLNK => {
                self.total.other += 1;
                self.total.logical += r.length;
                self.total.allocated += r.alloc;
                self.total.private += r.private.unwrap_or(r.alloc);
            }
            _ => self.total.other += 1,
        }
    }

    /// Count the entries of the directory open at `fd`, then descend.
    fn dir(&mut self, fd: &Fd, depth: u32) -> std::result::Result<(), i32> {
        if (self.stop)() {
            return Err(libc::ECANCELED);
        }
        let mut al = attrlist(true);
        let mut subdirs: Vec<OsString> = Vec::new();
        loop {
            let n = unsafe {
                libc::getattrlistbulk(
                    fd.0,
                    (&mut al as *mut libc::attrlist).cast(),
                    self.buf.as_mut_ptr().cast(),
                    self.buf.len(),
                    FSOPT_ATTR_CMN_EXTENDED,
                )
            };
            if n < 0 {
                match sys::errno() {
                    libc::EINTR => continue,
                    libc::ENOENT => break,
                    e => return Err(e),
                }
            }
            if n == 0 {
                break;
            }
            let mut off = 0usize;
            for _ in 0..n {
                let Some(len) = rd_u32(&self.buf, off) else { break };
                let len = len as usize;
                if len < 24 || off + len > self.buf.len() {
                    self.total.errors += 1;
                    break;
                }
                let rec = parse(&self.buf[off..off + len]);
                off += len;
                match rec {
                    Some(r) if r.error == 0 => {
                        if r.objtype == VDIR {
                            self.total.dirs += 1;
                            if !r.mount_point
                                && let Some(name) = r.name
                            {
                                subdirs.push(name);
                            }
                        } else {
                            self.count(&r);
                        }
                    }
                    _ => self.total.errors += 1,
                }
            }
        }
        if depth >= MAX_DEPTH {
            self.total.errors += subdirs.len() as u64;
            return Ok(());
        }
        for name in subdirs {
            match openat(fd.0, &name, DIR_FLAGS, 0) {
                Ok(child) => match self.dir(&child, depth + 1) {
                    Err(libc::ECANCELED) => return Err(libc::ECANCELED),
                    Err(_) => self.total.errors += 1,
                    Ok(()) => {}
                },
                Err(libc::ENOENT) => {}
                Err(_) => self.total.errors += 1,
            }
        }
        Ok(())
    }
}

/// Usage of the tree at `path` (an absolute path; the last component is
/// not followed if it is a symlink). A regular file is a tree of one.
pub fn usage_of(path: &Path) -> Result<Usage> {
    usage_with(path, &|| false)
}

/// [`usage_of`], giving up with `Code::Busy` (reason
/// `cancelled`) as soon as `stop` returns true (checked per directory).
pub fn usage_with(path: &Path, stop: &dyn Fn() -> bool) -> Result<Usage> {
    let c = sys::cpath(path).map_err(|e| errno_error(Code::InvalidInput, e, path, None))?;
    let mut al = attrlist(false);
    let mut one = vec![0u8; 1024];
    sys::retry(|| unsafe {
        libc::getattrlist(
            c.as_ptr(),
            (&mut al as *mut libc::attrlist).cast(),
            one.as_mut_ptr().cast(),
            one.len(),
            (FSOPT_ATTR_CMN_EXTENDED | FSOPT_NOFOLLOW) as _,
        )
    })
    .map_err(|e| classify_errno(e, IoSide::Read, path))?;
    let top = parse(&one).ok_or_else(|| errno_error(Code::Unknown, libc::EIO, path, Some("bad_record")))?;
    let mut scan = Scan { seen: HashSet::new(), total: Usage::empty(), buf: vec![0u8; BUFFER], stop };
    if top.objtype != VDIR {
        scan.count(&top);
        return Ok(scan.total);
    }
    scan.total.dirs += 1;
    let fd = openat(libc::AT_FDCWD, path.as_os_str(), DIR_FLAGS, 0).map_err(|e| classify_errno(e, IoSide::Read, path))?;
    match scan.dir(&fd, 0) {
        Ok(()) => Ok(scan.total),
        Err(libc::ECANCELED) => Err(errno_error(Code::Busy, libc::ECANCELED, path, Some("cancelled"))),
        Err(e) => Err(classify_errno(e, IoSide::Read, path)),
    }
}
