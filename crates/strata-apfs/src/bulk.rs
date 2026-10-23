//! The `getattrlistbulk` / `getattrlistat` attribute request and the record
//! parser (§4.2 D). Every record is parsed by its own returned bitmap, with
//! unaligned reads; `attrs_of` and `walk` share it.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::io::RawFd;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;
use strata_types::{Code, StrataError};

use crate::attrs::{Attrs, FileKind};
use crate::sys::{self, rd_i32, rd_i64, rd_u32, rd_u64};

/// Not in libc 0.2.191.
pub(crate) const ATTR_CMN_ERROR: u32 = 0x2000_0000;

pub(crate) const COMMON_POINT: u32 = libc::ATTR_CMN_RETURNED_ATTRS
    | libc::ATTR_CMN_OBJTYPE
    | libc::ATTR_CMN_MODTIME
    | libc::ATTR_CMN_CHGTIME
    | libc::ATTR_CMN_ACCESSMASK
    | libc::ATTR_CMN_GEN_COUNT
    | libc::ATTR_CMN_FILEID;
pub(crate) const COMMON_BULK: u32 = COMMON_POINT | libc::ATTR_CMN_NAME | ATTR_CMN_ERROR;

const VREG: u32 = 1;
const VDIR: u32 = 2;
const VBLK: u32 = 3;
const VCHR: u32 = 4;
const VLNK: u32 = 5;
const VSOCK: u32 = 6;
const VFIFO: u32 = 7;

/// The longest name: 255 UTF-16 units of BMP characters, 3 bytes each.
pub(crate) const MAX_NAME_BYTES: usize = 765;

const fn round_up(n: usize, a: usize) -> usize {
    n.div_ceil(a) * a
}

/// The longest record the bulk request can produce: length, returned
/// bitmap, error, name reference, OBJTYPE, two timespecs, ACCESSMASK,
/// GEN_COUNT, FILEID, DATALENGTH (8 ≥ MOUNTSTATUS's 4), then the name with
/// its NUL, the record padded to 8.
pub(crate) const MAX_RECORD_BYTES: usize =
    round_up(4 + 20 + 4 + 8 + 4 + 16 + 16 + 4 + 4 + 8 + 8 + round_up(MAX_NAME_BYTES + 1, 4), 8);
const _: () = assert!(MAX_RECORD_BYTES <= crate::MIN_BULK_BUFFER_BYTES);
const _: () = assert!(MAX_RECORD_BYTES >= 850 && MAX_RECORD_BYTES <= 880);

pub(crate) fn attrlist(bulk: bool) -> libc::attrlist {
    libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: if bulk { COMMON_BULK } else { COMMON_POINT },
        volattr: 0,
        dirattr: libc::ATTR_DIR_MOUNTSTATUS,
        fileattr: libc::ATTR_FILE_DATALENGTH,
        forkattr: 0,
    }
}

/// One parsed record. Fields absent from the returned bitmap are `None`
/// (gen count: 0).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct RawEntry {
    pub(crate) name: Option<String>,
    pub(crate) error: u32,
    pub(crate) objtype: Option<u32>,
    pub(crate) mtime: Option<(i64, i64)>,
    pub(crate) ctime: Option<(i64, i64)>,
    pub(crate) mode: Option<u32>,
    pub(crate) gen_count: u64,
    pub(crate) inode: Option<u64>,
    pub(crate) mount_point: bool,
    pub(crate) size: u64,
}

fn bad(what: &str) -> StrataError {
    StrataError::new(Code::Unknown, format!("bad attribute record: {what}"))
        .with_detail(json!({ "reason": "bad_record", "what": what }))
}

/// Parse `n` records from `buf` (the first `n` that `getattrlistbulk`
/// returned, or the one `getattrlistat` wrote). A record that cannot be
/// framed (length 0, past the buffer) ends the parse with an `Err`; a record
/// with a bad name is one `Err` and the parse goes on.
pub(crate) fn parse_records(buf: &[u8], n: usize) -> Vec<Result<RawEntry, StrataError>> {
    let mut out = Vec::with_capacity(n);
    let mut off = 0usize;
    for _ in 0..n {
        let Some(len) = rd_u32(buf, off) else {
            out.push(Err(bad("truncated buffer")));
            break;
        };
        let len = len as usize;
        if len < 24 || off + len > buf.len() {
            out.push(Err(bad("record length")));
            break;
        }
        out.push(parse_one(&buf[off..off + len]));
        off += len;
    }
    out
}

fn parse_one(rec: &[u8]) -> Result<RawEntry, StrataError> {
    let need = |v: Option<u32>| v.ok_or_else(|| bad("field past the record"));
    let common = need(rd_u32(rec, 4))?;
    let (vol, dir, file, fork) = (need(rd_u32(rec, 8))?, need(rd_u32(rec, 12))?, need(rd_u32(rec, 16))?, need(rd_u32(rec, 20))?);
    if common & !COMMON_BULK != 0
        || vol != 0
        || dir & !libc::ATTR_DIR_MOUNTSTATUS != 0
        || file & !libc::ATTR_FILE_DATALENGTH != 0
        || fork != 0
    {
        return Err(bad("unexpected attribute in the returned bitmap"));
    }
    let mut e = RawEntry::default();
    let mut p = 24usize;
    let u32_at = |p: usize| rd_u32(rec, p).ok_or_else(|| bad("field past the record"));
    let u64_at = |p: usize| rd_u64(rec, p).ok_or_else(|| bad("field past the record"));
    let ts_at = |p: usize| -> Result<(i64, i64), StrataError> {
        Ok((
            rd_i64(rec, p).ok_or_else(|| bad("field past the record"))?,
            rd_i64(rec, p + 8).ok_or_else(|| bad("field past the record"))?,
        ))
    };
    if common & ATTR_CMN_ERROR != 0 {
        e.error = u32_at(p)?;
        p += 4;
    }
    if common & libc::ATTR_CMN_NAME != 0 {
        let rel = rd_i32(rec, p).ok_or_else(|| bad("field past the record"))?;
        let nlen = u32_at(p + 4)? as i64;
        let start = p as i64 + rel as i64;
        if nlen < 1 || start < 0 || start + nlen > rec.len() as i64 {
            return Err(bad("name reference out of the record"));
        }
        let bytes = &rec[start as usize..(start + nlen) as usize];
        let bytes = match bytes.iter().position(|&b| b == 0) {
            Some(z) => &bytes[..z],
            None => bytes,
        };
        let name = String::from_utf8(bytes.to_vec()).map_err(|_| {
            StrataError::new(Code::NotText, "a directory entry name is not UTF-8")
                .with_detail(json!({ "reason": "name_not_utf8", "bytes": bytes }))
        })?;
        e.name = Some(name);
        p += 8;
    }
    if common & libc::ATTR_CMN_OBJTYPE != 0 {
        e.objtype = Some(u32_at(p)?);
        p += 4;
    }
    if common & libc::ATTR_CMN_MODTIME != 0 {
        e.mtime = Some(ts_at(p)?);
        p += 16;
    }
    if common & libc::ATTR_CMN_CHGTIME != 0 {
        e.ctime = Some(ts_at(p)?);
        p += 16;
    }
    if common & libc::ATTR_CMN_ACCESSMASK != 0 {
        e.mode = Some(u32_at(p)?);
        p += 4;
    }
    if common & libc::ATTR_CMN_GEN_COUNT != 0 {
        e.gen_count = u32_at(p)? as u64;
        p += 4;
    }
    if common & libc::ATTR_CMN_FILEID != 0 {
        e.inode = Some(u64_at(p)?);
        p += 8;
    }
    if dir & libc::ATTR_DIR_MOUNTSTATUS != 0 {
        e.mount_point = u32_at(p)? & libc::DIR_MNTSTATUS_MNTPOINT != 0;
        p += 4;
    }
    if file & libc::ATTR_FILE_DATALENGTH != 0 {
        e.size = u64_at(p)?;
    }
    Ok(e)
}

pub(crate) fn ts(sec: i64, nsec: i64) -> SystemTime {
    let nsec = nsec.clamp(0, 999_999_999) as u64;
    if sec >= 0 {
        UNIX_EPOCH + Duration::new(sec as u64, nsec as u32)
    } else {
        UNIX_EPOCH - Duration::from_secs(sec.unsigned_abs()) + Duration::from_nanos(nsec)
    }
}

fn kind_of(objtype: u32) -> Option<FileKind> {
    Some(match objtype {
        VREG => FileKind::File,
        VDIR => FileKind::Dir,
        VBLK => FileKind::BlockDevice,
        VCHR => FileKind::CharDevice,
        VLNK => FileKind::Symlink { target: PathBuf::new() },
        VSOCK => FileKind::Socket,
        VFIFO => FileKind::Fifo,
        _ => return None,
    })
}

/// `Attrs` from a record (a symlink's target left empty for the caller to
/// fill). `None` when a field the record should carry is missing.
pub(crate) fn into_attrs(raw: &RawEntry) -> Option<Attrs> {
    let kind = kind_of(raw.objtype?)?;
    let (ms, mn) = raw.mtime?;
    let (cs, cn) = raw.ctime?;
    let size = match kind {
        FileKind::File | FileKind::Symlink { .. } => raw.size,
        _ => 0,
    };
    Some(Attrs {
        kind,
        size,
        mode: raw.mode? & 0o7777,
        gen_count: raw.gen_count,
        mtime: ts(ms, mn),
        ctime: ts(cs, cn),
        inode: raw.inode?,
    })
}

/// `readlinkat`, raw target bytes.
pub(crate) fn readlink_at(dirfd: RawFd, name: &OsStr) -> Result<PathBuf, i32> {
    let c = sys::cos(name)?;
    let mut buf = vec![0u8; libc::PATH_MAX as usize + 1];
    let n = sys::retry(|| unsafe { libc::readlinkat(dirfd, c.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) })?;
    buf.truncate(n as usize);
    Ok(PathBuf::from(std::ffi::OsString::from_vec(buf)))
}

/// The `ERROR`-record fallback: `Attrs` from `fstatat(AT_SYMLINK_NOFOLLOW)`,
/// plus the entry's `st_dev`. No gen count can be read this way, so
/// `gen_count = 0` (review M10).
pub(crate) fn stat_attrs(dirfd: RawFd, name: &OsStr) -> Result<(Attrs, libc::dev_t), i32> {
    let st = sys::fstatat_nofollow(dirfd, name)?;
    let kind = match sys::fmt_of(&st) {
        libc::S_IFREG => FileKind::File,
        libc::S_IFDIR => FileKind::Dir,
        libc::S_IFLNK => FileKind::Symlink { target: readlink_at(dirfd, name)? },
        libc::S_IFIFO => FileKind::Fifo,
        libc::S_IFSOCK => FileKind::Socket,
        libc::S_IFCHR => FileKind::CharDevice,
        libc::S_IFBLK => FileKind::BlockDevice,
        _ => return Err(libc::EIO),
    };
    let size = match &kind {
        FileKind::File => st.st_size as u64,
        FileKind::Symlink { target } => target.as_os_str().len() as u64,
        _ => 0,
    };
    let attrs = Attrs {
        kind,
        size,
        mode: st.st_mode as u32 & 0o7777,
        gen_count: crate::GEN_COUNT_UNAVAILABLE,
        mtime: ts(st.st_mtime, st.st_mtime_nsec),
        ctime: ts(st.st_ctime, st.st_ctime_nsec),
        inode: st.st_ino,
    };
    Ok((attrs, st.st_dev))
}

/// One finished directory entry: on-disk name, attributes, mount point.
pub(crate) struct Finished {
    pub(crate) name: String,
    pub(crate) attrs: Attrs,
    pub(crate) mount_point: bool,
}

/// Turn one bulk record under `dirfd` (on device `parent_dev`) into a
/// [`Finished`] (§5.6 read_dir steps 2 and 4). `None` = the entry vanished
/// (skip it); `Err(errno)` = the fallback or `readlinkat` failed otherwise.
pub(crate) fn finish_entry(dirfd: RawFd, parent_dev: libc::dev_t, raw: &RawEntry) -> Option<Result<Finished, i32>> {
    let name = raw.name.clone()?;
    let os = OsStr::new(&name);
    let (attrs, mount_point) = match (raw.error, into_attrs(raw)) {
        (0, Some(mut a)) => {
            if let FileKind::Symlink { target } = &mut a.kind {
                match readlink_at(dirfd, os) {
                    Ok(t) => *target = t,
                    Err(libc::ENOENT) | Err(libc::EINVAL) => return None,
                    Err(e) => return Some(Err(e)),
                }
            }
            (a, raw.mount_point)
        }
        _ => match stat_attrs(dirfd, os) {
            Ok((a, dev)) => {
                let mp = a.kind == FileKind::Dir && dev != parent_dev;
                (a, mp)
            }
            Err(libc::ENOENT) => return None,
            Err(e) => return Some(Err(e)),
        },
    };
    Some(Ok(Finished { name, attrs, mount_point }))
}

/// `getattrlistat(FSOPT_NOFOLLOW | FSOPT_ATTR_CMN_EXTENDED)` for one name;
/// returns the buffer holding one record.
pub(crate) fn getattrlistat(dirfd: RawFd, name: &OsStr) -> Result<Vec<u8>, i32> {
    let c = sys::cos(name)?;
    let mut al = attrlist(false);
    let mut buf = vec![0u8; 1024];
    sys::retry(|| unsafe {
        libc::getattrlistat(
            dirfd,
            c.as_ptr(),
            (&mut al as *mut libc::attrlist).cast(),
            buf.as_mut_ptr().cast(),
            buf.len(),
            (libc::FSOPT_NOFOLLOW | libc::FSOPT_ATTR_CMN_EXTENDED) as u64,
        )
    })?;
    Ok(buf)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{MetadataExt, symlink};

    /// A synthetic record; `None` fields are left out of the returned bitmap.
    #[derive(Default, Clone)]
    pub(crate) struct Rec {
        pub error: Option<u32>,
        pub name: Option<Vec<u8>>,
        pub objtype: Option<u32>,
        pub mtime: Option<(i64, i64)>,
        pub ctime: Option<(i64, i64)>,
        pub mode: Option<u32>,
        pub gen_c: Option<u32>,
        pub fileid: Option<u64>,
        pub mount: Option<u32>,
        pub len: Option<u64>,
        pub extra_common: u32,
    }

    pub(crate) fn build(r: &Rec) -> Vec<u8> {
        let mut common = libc::ATTR_CMN_RETURNED_ATTRS | r.extra_common;
        let mut b: Vec<u8> = vec![0; 24];
        if let Some(e) = r.error {
            common |= ATTR_CMN_ERROR;
            b.extend(e.to_ne_bytes());
        }
        let name_ref = r.name.as_ref().map(|_| {
            common |= libc::ATTR_CMN_NAME;
            b.extend([0u8; 8]);
            b.len() - 8
        });
        let mut put = |bit: u32, bytes: Vec<u8>, b: &mut Vec<u8>| {
            common |= bit;
            b.extend(bytes);
        };
        if let Some(v) = r.objtype {
            put(libc::ATTR_CMN_OBJTYPE, v.to_ne_bytes().to_vec(), &mut b);
        }
        for (bit, t) in [(libc::ATTR_CMN_MODTIME, r.mtime), (libc::ATTR_CMN_CHGTIME, r.ctime)] {
            if let Some((s, n)) = t {
                put(bit, [s.to_ne_bytes(), n.to_ne_bytes()].concat(), &mut b);
            }
        }
        if let Some(v) = r.mode {
            put(libc::ATTR_CMN_ACCESSMASK, v.to_ne_bytes().to_vec(), &mut b);
        }
        if let Some(v) = r.gen_c {
            put(libc::ATTR_CMN_GEN_COUNT, v.to_ne_bytes().to_vec(), &mut b);
        }
        if let Some(v) = r.fileid {
            put(libc::ATTR_CMN_FILEID, v.to_ne_bytes().to_vec(), &mut b);
        }
        let mut dir = 0;
        let mut file = 0;
        if let Some(v) = r.mount {
            dir = libc::ATTR_DIR_MOUNTSTATUS;
            b.extend(v.to_ne_bytes());
        }
        if let Some(v) = r.len {
            file = libc::ATTR_FILE_DATALENGTH;
            b.extend(v.to_ne_bytes());
        }
        if let (Some(at), Some(n)) = (name_ref, &r.name) {
            let start = b.len();
            b.extend(n);
            b.push(0);
            while b.len() % 4 != 0 {
                b.push(0);
            }
            b[at..at + 4].copy_from_slice(&((start - at) as i32).to_ne_bytes());
            b[at + 4..at + 8].copy_from_slice(&((n.len() + 1) as u32).to_ne_bytes());
        }
        while b.len() % 8 != 0 {
            b.push(0);
        }
        let len = b.len() as u32;
        b[0..4].copy_from_slice(&len.to_ne_bytes());
        b[4..8].copy_from_slice(&common.to_ne_bytes());
        b[12..16].copy_from_slice(&dir.to_ne_bytes());
        b[16..20].copy_from_slice(&file.to_ne_bytes());
        b
    }

    fn file_rec(name: &str) -> Rec {
        Rec {
            name: Some(name.as_bytes().to_vec()),
            objtype: Some(VREG),
            mtime: Some((1_600_000_000, 123)),
            ctime: Some((1_600_000_001, 456)),
            mode: Some(0o100644),
            gen_c: Some(7),
            fileid: Some(0x0102_0304_0506_0708),
            len: Some(42),
            ..Default::default()
        }
    }

    #[test]
    fn bulk_record_parser_follows_the_returned_bitmap() {
        // A: no GEN_COUNT (MSDOS): FILEID still lands right.
        let a = Rec { gen_c: None, ..file_rec("a") };
        // B: a directory, MOUNTSTATUS tail, mount point set.
        let b = Rec { objtype: Some(VDIR), len: None, mount: Some(1), name: Some(b"dir".to_vec()), ..file_rec("") };
        // C: an ERROR record.
        let c = Rec { error: Some(13), ..file_rec("c") };
        // D: a plain file, gen count present.
        let d = file_rec("ddd");
        let buf = [build(&a), build(&b), build(&c), build(&d)].concat();
        let got: Vec<_> = parse_records(&buf, 4).into_iter().map(|r| r.unwrap()).collect();
        assert_eq!(got[0].name.as_deref(), Some("a"));
        assert_eq!(got[0].gen_count, 0);
        assert_eq!(got[0].inode, Some(0x0102_0304_0506_0708));
        assert_eq!(got[0].size, 42);
        assert_eq!(got[0].error, 0);
        assert_eq!(into_attrs(&got[0]).unwrap().gen_count, 0);
        assert_eq!(got[1].name.as_deref(), Some("dir"));
        assert!(got[1].mount_point);
        assert_eq!(got[1].size, 0);
        assert_eq!(got[1].gen_count, 7);
        assert_eq!(into_attrs(&got[1]).unwrap().kind, FileKind::Dir);
        assert_eq!(got[2].error, 13);
        assert_eq!(got[2].name.as_deref(), Some("c"));
        assert_eq!(got[2].inode, Some(0x0102_0304_0506_0708));
        assert_eq!(got[3].error, 0);
        assert_eq!(got[3].gen_count, 7);
        let at = into_attrs(&got[3]).unwrap();
        assert_eq!(at.mode, 0o644);
        assert_eq!(at.mtime, UNIX_EPOCH + Duration::new(1_600_000_000, 123));
        assert_eq!(at.ctime, UNIX_EPOCH + Duration::new(1_600_000_001, 456));
        // Fields at 4-aligned, not 8-aligned offsets: the FILEID after the
        // u32 GEN_COUNT sits at an offset ≡ 4 (mod 8) in record D.
        let fileid_off = 24 + 8 + 4 + 16 + 16 + 4 + 4;
        assert_eq!(fileid_off % 8, 4);
        // Negative seconds.
        assert_eq!(ts(-2, 500_000_000), UNIX_EPOCH - Duration::from_millis(1500));
        // `n` limits the records read.
        assert_eq!(parse_records(&buf, 2).len(), 2);
    }

    #[test]
    fn bulk_record_parser_rejects_bad_records() {
        // A name that is not UTF-8: NotText, no U+FFFD; the next record still parses.
        let bad_name = Rec { name: Some(vec![b'a', 0xff, 0xfe]), ..file_rec("") };
        let buf = [build(&bad_name), build(&file_rec("ok"))].concat();
        let got = parse_records(&buf, 2);
        assert_eq!(got[0].as_ref().unwrap_err().code, Code::NotText);
        assert_eq!(got[1].as_ref().unwrap().name.as_deref(), Some("ok"));
        // Length 0.
        let mut z = build(&file_rec("z"));
        z[0..4].copy_from_slice(&0u32.to_ne_bytes());
        let got = parse_records(&z, 3);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].as_ref().unwrap_err().code, Code::Unknown);
        // Length past the buffer.
        let mut long = build(&file_rec("l"));
        let l = long.len() as u32 + 8;
        long[0..4].copy_from_slice(&l.to_ne_bytes());
        assert_eq!(parse_records(&long, 1)[0].as_ref().unwrap_err().code, Code::Unknown);
        // n larger than the buffer holds.
        let one = build(&file_rec("x"));
        let got = parse_records(&one, 2);
        assert!(got[0].is_ok());
        assert_eq!(got[1].as_ref().unwrap_err().code, Code::Unknown);
        // Name offset out of the record, both directions.
        for rel in [4096i32, -4096] {
            let mut r = build(&file_rec("n"));
            r[24..28].copy_from_slice(&rel.to_ne_bytes());
            assert_eq!(parse_records(&r, 1)[0].as_ref().unwrap_err().code, Code::Unknown, "{rel}");
        }
        // Name length 0.
        let mut r = build(&file_rec("n"));
        r[28..32].copy_from_slice(&0u32.to_ne_bytes());
        assert_eq!(parse_records(&r, 1)[0].as_ref().unwrap_err().code, Code::Unknown);
        // A record cut short of its fixed fields.
        let mut short = build(&Rec { name: None, ..file_rec("") });
        short.truncate(32);
        short[0..4].copy_from_slice(&32u32.to_ne_bytes());
        assert_eq!(parse_records(&short, 1)[0].as_ref().unwrap_err().code, Code::Unknown);
        // An attribute we did not ask for.
        let odd = Rec { extra_common: 0x0000_0002, ..file_rec("o") };
        assert_eq!(parse_records(&build(&odd), 1)[0].as_ref().unwrap_err().code, Code::Unknown);
        // Random garbage never panics.
        let mut seed = 0x1234_5678u32;
        for _ in 0..2000 {
            let mut g = build(&file_rec("garbage"));
            for _ in 0..4 {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                let i = (seed as usize >> 8) % g.len();
                g[i] = (seed >> 3) as u8;
            }
            let _ = parse_records(&g, 3);
        }
        // An ERROR record goes to the fstatat fallback (here: the name is gone → skipped).
        let td = tempfile::tempdir().unwrap();
        let dir = fs::File::open(td.path()).unwrap();
        use std::os::unix::io::AsRawFd;
        let raw = parse_records(&build(&Rec { error: Some(5), ..file_rec("gone") }), 1).pop().unwrap().unwrap();
        assert!(finish_entry(dir.as_raw_fd(), 0, &raw).is_none());
    }

    #[test]
    fn bulk_error_record_fallback_has_no_gen_count() {
        use std::os::unix::io::AsRawFd;
        let td = tempfile::tempdir().unwrap();
        let p = td.path();
        fs::write(p.join("real"), "twelve bytes").unwrap();
        fs::set_permissions(p.join("real"), std::os::unix::fs::PermissionsExt::from_mode(0o640)).unwrap();
        symlink("some/target", p.join("lnk")).unwrap();
        fs::create_dir(p.join("d")).unwrap();
        let dir = fs::File::open(p).unwrap();
        for name in ["real", "lnk", "d"] {
            // The record's own fields are deliberately wrong; the fallback must not use them.
            let rec = Rec { error: Some(libc::EACCES as u32), gen_c: Some(99), ..file_rec(name) };
            let raw = parse_records(&build(&rec), 1).pop().unwrap().unwrap();
            let fin = finish_entry(dir.as_raw_fd(), 0, &raw).unwrap().unwrap();
            let (n, a) = (fin.name, fin.attrs);
            assert_eq!(n, name);
            let m = fs::symlink_metadata(p.join(name)).unwrap();
            let want_kind = match name {
                "real" => FileKind::File,
                "lnk" => FileKind::Symlink { target: "some/target".into() },
                _ => FileKind::Dir,
            };
            assert_eq!(a.kind, want_kind);
            let want_size = match name {
                "real" => 12,
                "lnk" => "some/target".len() as u64,
                _ => 0,
            };
            assert_eq!(a.size, want_size, "{name}");
            assert_eq!(a.mode, m.mode() & 0o7777);
            assert_eq!(a.inode, m.ino());
            assert_eq!(a.mtime, ts(m.mtime(), m.mtime_nsec()));
            assert_eq!(a.ctime, ts(m.ctime(), m.ctime_nsec()));
            assert_eq!(a.gen_count, 0);
            assert!(!a.gen_count_known());
            assert!(!a.unchanged_since(&a.clone()));
        }
        // Gone by the time of the fallback: skipped, no error.
        fs::remove_file(p.join("real")).unwrap();
        let raw = parse_records(&build(&Rec { error: Some(1), ..file_rec("real") }), 1).pop().unwrap().unwrap();
        assert!(finish_entry(dir.as_raw_fd(), 0, &raw).is_none());
    }

    #[test]
    fn bulk_record_maximum_fits_the_minimum_buffer() {
        let name = "\u{3042}".repeat(255);
        assert_eq!(name.len(), MAX_NAME_BYTES);
        let rec = Rec { error: Some(1), len: Some(1), ..file_rec(&name) };
        let b = build(&rec);
        assert!((850..=880).contains(&b.len()), "{}", b.len());
        assert!(b.len() <= crate::MIN_BULK_BUFFER_BYTES);
        assert_eq!(b.len(), MAX_RECORD_BYTES);
        let raw = parse_records(&b, 1).pop().unwrap().unwrap();
        assert_eq!(raw.name.as_deref(), Some(name.as_str()));
    }
}
