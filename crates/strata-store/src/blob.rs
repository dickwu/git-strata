//! Content-addressed blob store (store.md §5.3, §5.4). Blobs live as files
//! under `store/blobs/<id[..2]>/<id>`; put never touches SQLite.

use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::RwLockReadGuard;
use std::sync::atomic::Ordering;
use std::time::SystemTime;

use serde_json::json;
use strata_apfs::{CloneKind, FileKind, FileMeta, IoSide};
use strata_types::{BlobId, Code, RepoPath, StrataError};

use crate::errors::{NOT_WRITTEN, PARTIAL, annotate, err, io, sql_read, with};
use crate::open::hex32;
use crate::seams::Seam;
use crate::{Result, Store};

/// Attempts at a fresh temp name before giving up (§5.3 step 3).
const TMP_TRIES: usize = 8;
/// From this size on, hashing maps the file and uses every core.
const MMAP_MIN: u64 = 16 << 20;
/// Extended attributes kept on a blob (§4.6): kernel-managed ones.
const XATTR_KEEP: [&str; 3] = ["com.apple.provenance", "com.apple.decmpfs", "com.apple.ResourceFork"];

fn cpath(p: &Path) -> CString {
    CString::new(p.as_os_str().as_bytes()).expect("store paths have no NUL")
}

fn last_os() -> io::Error {
    io::Error::last_os_error()
}

fn unknown(op: &str, outcome: &str, path: &Path, msg: impl Into<String>, e: Option<&io::Error>) -> StrataError {
    let mut d = json!({ "op": op, "outcome": outcome, "path": path.to_string_lossy() });
    if let Some(n) = e.and_then(|e| e.raw_os_error()) {
        d["errno"] = json!(n);
    }
    err(Code::Unknown, msg, d)
}

/// Any failure after `dst` was created: `Unknown` with outcome `partial`.
fn partial(e: StrataError, dst: &Path) -> StrataError {
    let cause = format!("{:?}", e.code);
    let mut out = err(
        Code::Unknown,
        format!("{}: the file was created but could not be finished: {}", dst.display(), e.message),
        json!({ "op": "get", "outcome": PARTIAL, "path": dst.to_string_lossy(), "cause": cause }),
    );
    if let Some(d) = e.detail {
        out = with(out, "cause_detail", d);
    }
    out
}

fn open_nofollow(p: &Path, write: bool) -> io::Result<File> {
    let mut o = OpenOptions::new();
    if write {
        o.write(true);
    } else {
        o.read(true);
    }
    o.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(p)
}

fn barrier(fd: &impl AsRawFd) -> io::Result<()> {
    loop {
        let r = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_BARRIERFSYNC) };
        if r != -1 {
            return Ok(());
        }
        let e = last_os();
        if e.raw_os_error() != Some(libc::EINTR) {
            return Err(e);
        }
    }
}

/// BLAKE3 of a whole open file; holes read as zeros. Returns (hex, size).
fn hash_file(f: &File) -> io::Result<(String, u64)> {
    let len = f.metadata()?.len();
    let mut h = blake3::Hasher::new();
    if len >= MMAP_MIN {
        let m = unsafe { memmap2::Mmap::map(f)? };
        h.update_rayon(&m);
        return Ok((h.finalize().to_hex().to_string(), m.len() as u64));
    }
    let mut buf = vec![0u8; 256 * 1024];
    let mut off = 0u64;
    loop {
        let n = match f.read_at(&mut buf, off) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        h.update(&buf[..n]);
        off += n as u64;
    }
    Ok((h.finalize().to_hex().to_string(), off))
}

fn hash_bytes(b: &[u8]) -> String {
    let mut h = blake3::Hasher::new();
    if b.len() as u64 >= MMAP_MIN {
        h.update_rayon(b);
    } else {
        h.update(b);
    }
    h.finalize().to_hex().to_string()
}

fn blob_id(hex: String) -> BlobId {
    BlobId::try_from(hex).expect("BLAKE3 hex is a valid blob id")
}

/// Remove every extended attribute but the kernel-managed ones.
fn strip_xattrs(f: &File, path: &Path) -> Result<()> {
    let fd = f.as_raw_fd();
    let n = unsafe { libc::flistxattr(fd, std::ptr::null_mut(), 0, 0) };
    if n < 0 {
        let e = last_os();
        return Err(unknown("put", NOT_WRITTEN, path, format!("{}: listing extended attributes failed: {e}", path.display()), Some(&e)));
    }
    if n == 0 {
        return Ok(());
    }
    let mut buf = vec![0u8; n as usize];
    let n = unsafe { libc::flistxattr(fd, buf.as_mut_ptr().cast(), buf.len(), 0) };
    if n < 0 {
        let e = last_os();
        return Err(unknown("put", NOT_WRITTEN, path, format!("{}: listing extended attributes failed: {e}", path.display()), Some(&e)));
    }
    for name in buf[..n as usize].split(|b| *b == 0).filter(|s| !s.is_empty()) {
        if XATTR_KEEP.iter().any(|k| k.as_bytes() == name) {
            continue;
        }
        let c = CString::new(name).expect("xattr names have no NUL");
        if unsafe { libc::fremovexattr(fd, c.as_ptr(), 0) } != 0 {
            let e = last_os();
            if e.raw_os_error() == Some(libc::ENOATTR) {
                continue;
            }
            return Err(unknown(
                "put",
                NOT_WRITTEN,
                path,
                format!("{}: removing extended attribute {} failed: {e}", path.display(), String::from_utf8_lossy(name)),
                Some(&e),
            ));
        }
    }
    Ok(())
}

impl Store {
    pub(crate) fn blob_path(&self, id: &BlobId) -> PathBuf {
        let s = id.as_str();
        self.store_dir().join("blobs").join(&s[..2]).join(s)
    }

    pub(crate) fn gate_shared(&self) -> RwLockReadGuard<'_, ()> {
        self.blob_gate.read().unwrap_or_else(|p| p.into_inner())
    }

    /// Run `make` on fresh `store/tmp/put-<hex32>` names until one is free
    /// (§5.3 step 3). `make` reports a taken name as `Exists`.
    fn fresh_tmp<T>(&self, mut make: impl FnMut(&Path) -> Result<T>) -> Result<(PathBuf, T)> {
        let dir = self.store_dir().join("tmp");
        for i in 0..TMP_TRIES {
            let tmp = dir.join(format!("put-{}", hex32()));
            if i == 0 && self.seams.hit(&Seam::TempNameTaken) {
                // Make the collision real: the name now holds a stranger's file.
                let _ = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp).and_then(|mut f| f.write_all(b"taken"));
            }
            match make(&tmp) {
                Ok(v) => return Ok((tmp, v)),
                Err(e) if e.code == Code::Exists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(unknown("put", NOT_WRITTEN, &dir, format!("no free temp name in {} after {TMP_TRIES} tries", dir.display()), None))
    }

    /// Store `src`'s current content (§5.3).
    pub fn put_blob(&self, src: &Path) -> Result<BlobId> {
        self.writable()?;
        let _gate = self.gate_shared();
        let (tmp, ()) = self.fresh_tmp(|tmp| strata_apfs::clone_file(src, tmp))?;
        self.seams.pause_if(&Seam::PauseInPut);
        let md = fs::symlink_metadata(&tmp).map_err(|e| io(&e, IoSide::Read, &tmp, "put", NOT_WRITTEN))?;
        if md.file_type().is_symlink() {
            let target = fs::read_link(&tmp).map_err(|e| io(&e, IoSide::Read, &tmp, "put", NOT_WRITTEN))?;
            let _ = fs::remove_file(&tmp);
            return self.put_bytes_locked(target.as_os_str().as_bytes());
        }
        self.finish_put(&tmp)
    }

    /// Capture `src_root/src` without following a symlink at any level
    /// (§5.3, addendum 8).
    pub fn put_blob_rooted(&self, src_root: &Path, src: &RepoPath) -> Result<BlobId> {
        self.writable()?;
        let _gate = self.gate_shared();
        let attrs = strata_apfs::attrs_of(src_root, src)?;
        let shown = src_root.join(src.as_str());
        match attrs.kind {
            FileKind::File => {}
            FileKind::Dir => {
                return Err(err(
                    Code::IsDirectory,
                    format!("{}: is a directory", shown.display()),
                    json!({ "path": shown.to_string_lossy(), "reason": "directory" }),
                ));
            }
            FileKind::Symlink { target } => return self.put_bytes_locked(target.as_os_str().as_bytes()),
            other => {
                let reason = match other {
                    FileKind::Fifo => "fifo",
                    FileKind::Socket => "socket",
                    _ => "device",
                };
                return Err(err(
                    Code::NotFile,
                    format!("{}: is not a regular file", shown.display()),
                    json!({ "path": shown.to_string_lossy(), "reason": reason }),
                ));
            }
        }
        let (tmp, ()) = self.fresh_tmp(|tmp| strata_apfs::clone_from_rooted(src_root, src, tmp, CloneKind::File))?;
        self.seams.pause_if(&Seam::PauseInPut);
        self.finish_put(&tmp)
    }

    /// Store `bytes` (§5.3 step 10).
    pub fn put_bytes(&self, bytes: &[u8]) -> Result<BlobId> {
        self.writable()?;
        let _gate = self.gate_shared();
        self.put_bytes_locked(bytes)
    }

    /// `put_bytes` with L3 already held (never retaken: L3 is not reentrant).
    fn put_bytes_locked(&self, bytes: &[u8]) -> Result<BlobId> {
        let (tmp, mut f) = self.fresh_tmp(|tmp| {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(tmp)
                .map_err(|e| io(&e, IoSide::Write, tmp, "put", NOT_WRITTEN))
        })?;
        f.write_all(bytes).map_err(|e| io(&e, IoSide::Write, &tmp, "put", NOT_WRITTEN))?;
        let id = blob_id(hash_bytes(bytes));
        self.store_tmp(&tmp, &f, id, bytes.len() as u64)
    }

    /// §5.3 steps 4–9 on a regular-file clone at `tmp`.
    fn finish_put(&self, tmp: &Path) -> Result<BlobId> {
        let f = open_nofollow(tmp, false).map_err(|e| io(&e, IoSide::Read, tmp, "put", NOT_WRITTEN))?;
        strata_apfs::set_file_meta(&f, &FileMeta { clear_flags: true, mode: Some(0o600), mtime: None })
            .map_err(|e| annotate(e, "put", NOT_WRITTEN))?;
        strip_xattrs(&f, tmp)?;
        let (hex, size) = hash_file(&f).map_err(|e| io(&e, IoSide::Read, tmp, "put", NOT_WRITTEN))?;
        self.store_tmp(tmp, &f, blob_id(hex), size)
    }

    /// The stored copy, if any, is reused (step 7); else `tmp` is sealed,
    /// ordered and renamed in (steps 8–9).
    fn store_tmp(&self, tmp: &Path, f: &File, id: BlobId, size: u64) -> Result<BlobId> {
        let dst = self.blob_path(&id);
        if self.reuse_existing(&dst, tmp, size)? {
            return Ok(id);
        }
        strata_apfs::set_file_meta(f, &FileMeta { clear_flags: false, mode: Some(0o444), mtime: Some(SystemTime::now()) })
            .map_err(|e| annotate(e, "put", NOT_WRITTEN))?;
        barrier(f).map_err(|e| unknown("put", NOT_WRITTEN, tmp, format!("{}: F_BARRIERFSYNC failed: {e}", tmp.display()), Some(&e)))?;
        self.seams.blob_barriers.fetch_add(1, Ordering::SeqCst);
        let renamed = match self.seams.take(|s| match s {
            Seam::BlobRenameFails { errno } => Some(*errno),
            _ => None,
        }) {
            Some(n) => Err(io::Error::from_raw_os_error(n)),
            None => crate::open::rename_excl(tmp, &dst),
        };
        match renamed {
            Ok(()) => {
                let fan = dst.parent().expect("blob path has a fan-out dir");
                let d = File::open(fan)
                    .and_then(|d| barrier(&d))
                    .map_err(|e| unknown("put", crate::errors::UNCERTAIN, fan, format!("{}: F_BARRIERFSYNC failed: {e}", fan.display()), Some(&e)))?;
                let _ = d;
                self.seams.dir_barriers.fetch_add(1, Ordering::SeqCst);
                Ok(id)
            }
            Err(e) if e.raw_os_error() == Some(libc::EEXIST) => {
                if self.reuse_existing(&dst, tmp, size)? {
                    Ok(id)
                } else {
                    Err(unknown("put", NOT_WRITTEN, tmp, format!("{}: rename reported EEXIST but the blob is gone", dst.display()), Some(&e)))
                }
            }
            Err(e) => Err(unknown(
                "put",
                NOT_WRITTEN,
                tmp,
                format!("renaming {} to {} failed: {e}", tmp.display(), dst.display()),
                Some(&e),
            )),
        }
    }

    /// §5.3 step 7. `Ok(true)`: the stored copy is reused (times refreshed,
    /// `tmp` removed, no barrier); `Ok(false)`: nothing stored yet.
    fn reuse_existing(&self, dst: &Path, tmp: &Path, size: u64) -> Result<bool> {
        let md = match fs::symlink_metadata(dst) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(io(&e, IoSide::Read, dst, "put", NOT_WRITTEN)),
        };
        if !md.file_type().is_file() {
            return Err(err(
                Code::Corrupt,
                format!("{} is not a regular file; the correct copy is kept at {}", dst.display(), tmp.display()),
                json!({ "reason": "blob_type", "path": tmp.to_string_lossy(), "blob": dst.to_string_lossy() }),
            ));
        }
        if md.len() != size {
            return Err(err(
                Code::Corrupt,
                format!(
                    "{} has {} bytes, expected {size}; the correct copy is kept at {}",
                    dst.display(),
                    md.len(),
                    tmp.display()
                ),
                json!({ "reason": "blob_size", "path": tmp.to_string_lossy(), "blob": dst.to_string_lossy(), "expected": size, "actual": md.len() }),
            ));
        }
        let c = cpath(dst);
        let ts = [
            libc::timespec { tv_sec: 0, tv_nsec: libc::UTIME_NOW },
            libc::timespec { tv_sec: 0, tv_nsec: libc::UTIME_NOW },
        ];
        if unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), ts.as_ptr(), libc::AT_SYMLINK_NOFOLLOW) } != 0 {
            let e = last_os();
            return Err(unknown("put", NOT_WRITTEN, dst, format!("{}: refreshing times failed: {e}", dst.display()), Some(&e)));
        }
        let _ = fs::remove_file(tmp);
        Ok(true)
    }

    /// §5.4 steps 1–2: the blob file and, when registered, its length.
    fn locate(&self, id: &BlobId) -> Result<(PathBuf, u64)> {
        let p = self.blob_path(id);
        let md = match fs::symlink_metadata(&p) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(err(
                    Code::Corrupt,
                    format!("blob {id} is missing: {}", p.display()),
                    json!({ "reason": "blob_missing", "path": p.to_string_lossy(), "id": id.as_str() }),
                ));
            }
            Err(e) => return Err(io(&e, IoSide::Read, &p, "read", NOT_WRITTEN)),
        };
        if !md.file_type().is_file() {
            return Err(err(
                Code::Corrupt,
                format!("blob {id} is not a regular file: {}", p.display()),
                json!({ "reason": "blob_type", "path": p.to_string_lossy(), "id": id.as_str() }),
            ));
        }
        let r = self.reader()?;
        let row: Option<i64> = {
            use rusqlite::OptionalExtension;
            r.query_row("SELECT size FROM blobs WHERE id = ?1", [id.as_str()], |r| r.get(0)).optional().map_err(sql_read)?
        };
        drop(r);
        if let Some(s) = row {
            if s < 0 || s as u64 != md.size() {
                return Err(err(
                    Code::Corrupt,
                    format!("blob {id} has {} bytes, its row says {s}", md.size()),
                    json!({ "reason": "blob_size", "path": p.to_string_lossy(), "id": id.as_str(), "expected": s, "actual": md.size() }),
                ));
            }
        }
        Ok((p, md.size()))
    }

    /// Clone blob `id` to `dst`, shaped like a git checkout (§5.4).
    pub fn get_blob(&self, id: &BlobId, dst: &Path) -> Result<()> {
        let _gate = self.gate_shared();
        let (p, _) = self.locate(id)?;
        strata_apfs::clone_file(&p, dst)?;
        let f = open_nofollow(dst, false).map_err(|e| partial(io(&e, IoSide::Read, dst, "get", PARTIAL), dst))?;
        normalize_checkout(&f).map_err(|e| partial(e, dst))
    }

    /// Like `get_blob`, but `dst` is a `RepoPath` under `root`, reached
    /// without following a symlink at any level (§5.4, addendum 8).
    pub fn get_blob_rooted(&self, id: &BlobId, root: &Path, dst: &RepoPath) -> Result<()> {
        let _gate = self.gate_shared();
        let (p, _) = self.locate(id)?;
        strata_apfs::clone_file_rooted(&p, root, dst, None)?;
        let shown = root.join(dst.as_str());
        let f = strata_apfs::open_rooted_file(root, dst).map_err(|e| partial(e, &shown))?;
        normalize_checkout(&f).map_err(|e| partial(e, &shown))
    }

    /// All of blob `id`'s bytes, if at most `max` (trusted read, §5.4).
    pub fn read_blob(&self, id: &BlobId, max: u64) -> Result<Vec<u8>> {
        let _gate = self.gate_shared();
        let (p, size) = self.locate(id)?;
        if size > max {
            return Err(err(
                Code::TooLarge,
                format!("blob {id} has {size} bytes, more than the {max} allowed"),
                json!({ "limit": "read_max", "max": max, "size": size }),
            ));
        }
        let f = open_nofollow(&p, false).map_err(|e| io(&e, IoSide::Read, &p, "read", NOT_WRITTEN))?;
        let mut out = vec![0u8; size as usize];
        f.read_exact_at(&mut out, 0).map_err(|e| io(&e, IoSide::Read, &p, "read", NOT_WRITTEN))?;
        Ok(out)
    }

    /// Recompute blob `id`'s BLAKE3 (§5.4). Never modifies anything.
    pub fn verify_blob(&self, id: &BlobId) -> Result<()> {
        let _gate = self.gate_shared();
        let (p, _) = self.locate(id)?;
        let f = open_nofollow(&p, false).map_err(|e| io(&e, IoSide::Read, &p, "verify", NOT_WRITTEN))?;
        let (hex, _) = hash_file(&f).map_err(|e| io(&e, IoSide::Read, &p, "verify", NOT_WRITTEN))?;
        if hex != id.as_str() {
            return Err(err(
                Code::Corrupt,
                format!("blob {id} hashes to {hex}"),
                json!({ "reason": "blob_mismatch", "path": p.to_string_lossy(), "expected": id.as_str(), "actual": hex }),
            ));
        }
        Ok(())
    }
}

/// Flags cleared, 0644, mtime now (§5.4 step 4).
fn normalize_checkout(f: &File) -> Result<()> {
    strata_apfs::set_file_meta(f, &FileMeta { clear_flags: true, mode: Some(0o644), mtime: Some(SystemTime::now()) })
}
