//! Shared test helpers: sparse disk images, special files, cleanup.
#![allow(dead_code)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use strata_types::{Code, RepoPath, StrataError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fs {
    Apfs,
    ApfsCaseSensitive,
    Hfs,
    MsDos,
}

impl Fs {
    fn arg(self) -> &'static str {
        match self {
            Fs::Apfs => "APFS",
            Fs::ApfsCaseSensitive => "Case-sensitive APFS",
            Fs::Hfs => "HFS+",
            Fs::MsDos => "MS-DOS",
        }
    }
}

/// A sparse image mounted under a temp dir (never under a
/// /Volumes mount: `hdiutil attach -mountpoint` there is refused). Drop detaches and deletes.
pub struct Image {
    pub dir: tempfile::TempDir,
    pub mount: PathBuf,
    image: PathBuf,
    attached: bool,
}

fn hdiutil(args: &[&str]) -> Result<String, String> {
    let out = Command::new("hdiutil").args(args).output().map_err(|e| format!("hdiutil: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "hdiutil {:?} failed: {}{}",
            args,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ))
    }
}

impl Image {
    pub fn create(fs: Fs, size_mb: u64) -> Image {
        let dir = tempfile::Builder::new().prefix("strata-img").tempdir_in(canon_tmp()).expect("tempdir");
        let base = fs::canonicalize(dir.path()).unwrap();
        let image_base = base.join("img");
        let size = format!("{size_mb}m");
        hdiutil(&[
            "create", "-quiet", "-size", &size, "-fs", fs.arg(), "-type", "SPARSE", "-volname", "STRATA",
            image_base.to_str().unwrap(),
        ])
        .unwrap_or_else(|e| panic!("cannot create {fs:?} image: {e}"));
        let image = base.join("img.sparseimage");
        let mount = base.join("mnt");
        fs::create_dir(&mount).unwrap();
        let mut img = Image { dir, mount: mount.clone(), image, attached: false };
        img.attach(&mount, false);
        img
    }

    fn attach(&mut self, at: &Path, ro: bool) {
        let mut args = vec!["attach", "-quiet", "-nobrowse", "-noautoopen", "-owners", "on", "-mountpoint"];
        args.push(at.to_str().unwrap());
        if ro {
            args.push("-readonly");
        }
        args.push(self.image.to_str().unwrap());
        hdiutil(&args).unwrap_or_else(|e| panic!("cannot attach image: {e}"));
        self.mount = at.to_path_buf();
        self.attached = true;
    }

    pub fn detach(&mut self) {
        if self.attached {
            let m = self.mount.to_str().unwrap().to_string();
            for _ in 0..5 {
                if hdiutil(&["detach", "-quiet", "-force", &m]).is_ok() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
            self.attached = false;
        }
    }

    /// Re-mount the image at `dir` (e.g. a nested mount inside a tree).
    pub fn attach_at(&mut self, dir: &Path) {
        self.detach();
        self.attach(&fs::canonicalize(dir).unwrap(), false);
    }

    pub fn reattach_read_only(&mut self) {
        let m = self.mount.clone();
        self.detach();
        self.attach(&m, true);
    }

    pub fn path(&self) -> &Path {
        &self.mount
    }

    pub fn free_bytes(&self) -> u64 {
        free_bytes(&self.mount)
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        let _ = Command::new("chflags").args(["-R", "nouchg,noschg,nouappnd"]).arg(&self.mount).output();
        self.detach();
        let _ = fs::remove_file(&self.image);
    }
}

pub fn free_bytes(p: &Path) -> u64 {
    let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).unwrap();
    let mut s: libc::statfs = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::statfs(c.as_ptr(), &mut s) }, 0);
    s.f_bavail * s.f_bsize as u64
}

/// The canonical temp dir (`/private/var/folders/...`).
pub fn canon_tmp() -> PathBuf {
    fs::canonicalize(std::env::temp_dir()).unwrap()
}

/// A temp dir with a canonical path (rooted functions require one).
pub struct Tmp {
    pub td: tempfile::TempDir,
    pub path: PathBuf,
}

impl Tmp {
    pub fn new() -> Tmp {
        let td = tempfile::Builder::new().prefix("strata-apfs").tempdir_in(canon_tmp()).unwrap();
        let path = fs::canonicalize(td.path()).unwrap();
        Tmp { td, path }
    }
    pub fn join(&self, p: &str) -> PathBuf {
        self.path.join(p)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        remove_protected_tree(&self.path);
    }
}

pub fn make_fifo(p: &Path) {
    let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0, "mkfifo {}", p.display());
}

/// Bind a unix socket at a short path, then rename it into place
/// (sockaddr_un has a 104-byte limit).
pub fn make_socket(p: &Path) {
    let short = PathBuf::from(format!("/tmp/sk{}-{}", std::process::id(), rand_suffix()));
    let l = std::os::unix::net::UnixListener::bind(&short).expect("bind socket");
    drop(l);
    fs::rename(&short, p).expect("rename socket");
}

fn rand_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos() as u64;
    t ^ (N.fetch_add(1, Ordering::Relaxed) << 32)
}

/// Write files under `dir` until the volume says ENOSPC.
pub fn fill_until_enospc(dir: &Path) -> PathBuf {
    let p = dir.join("filler");
    let mut f = fs::File::create(&p).unwrap();
    let chunk = vec![0xa5u8; 1 << 20];
    loop {
        match f.write_all(&chunk) {
            Ok(()) => {}
            Err(e) if e.raw_os_error() == Some(libc::ENOSPC) => break,
            Err(e) => panic!("fill: {e}"),
        }
    }
    let small = vec![1u8; 4096];
    while f.write_all(&small).is_ok() {}
    let _ = f.sync_all();
    p
}

/// rm -rf that first clears user flags, modes and ACLs.
pub fn remove_protected_tree(p: &Path) {
    if fs::symlink_metadata(p).is_err() {
        return;
    }
    let _ = Command::new("chflags").args(["-R", "nouchg,nouappnd"]).arg(p).output();
    let _ = Command::new("chmod").args(["-R", "u+rwx"]).arg(p).output();
    let _ = Command::new("chmod").args(["-R", "-N"]).arg(p).output();
    let _ = fs::remove_dir_all(p);
}

pub fn rp(s: &str) -> RepoPath {
    RepoPath::try_from(s.to_string()).unwrap()
}

pub fn set_mode(p: &Path, m: u32) {
    fs::set_permissions(p, fs::Permissions::from_mode(m)).unwrap();
}

pub fn chflags(p: &Path, flags: &str) {
    let o = Command::new("chflags").args(["-h", flags]).arg(p).output().unwrap();
    assert!(o.status.success(), "chflags {flags} {}: {}", p.display(), String::from_utf8_lossy(&o.stderr));
}

pub fn detail_str(e: &StrataError, k: &str) -> Option<String> {
    e.detail.as_ref()?.get(k)?.as_str().map(str::to_string)
}

pub fn detail_i64(e: &StrataError, k: &str) -> Option<i64> {
    e.detail.as_ref()?.get(k)?.as_i64()
}

pub fn detail_bool(e: &StrataError, k: &str) -> Option<bool> {
    e.detail.as_ref()?.get(k)?.as_bool()
}

#[track_caller]
pub fn assert_code<T: std::fmt::Debug>(r: Result<T, StrataError>, code: Code) -> StrataError {
    match r {
        Ok(v) => panic!("expected {code:?}, got Ok({v:?})"),
        Err(e) => {
            assert_eq!(e.code, code, "wrong code: {e:?}");
            e
        }
    }
}

#[track_caller]
pub fn assert_reason<T: std::fmt::Debug>(r: Result<T, StrataError>, code: Code, reason: &str) -> StrataError {
    let e = assert_code(r, code);
    assert_eq!(detail_str(&e, "reason").as_deref(), Some(reason), "wrong reason: {e:?}");
    e
}

/// Byte-for-byte snapshot of a tree (names, kinds, contents, link targets).
pub fn snapshot(p: &Path) -> Vec<(PathBuf, String)> {
    let mut out = vec![];
    fn go(base: &Path, p: &Path, out: &mut Vec<(PathBuf, String)>) {
        let m = fs::symlink_metadata(p).unwrap();
        let rel = p.strip_prefix(base).unwrap().to_path_buf();
        let ft = m.file_type();
        if ft.is_symlink() {
            out.push((rel, format!("l:{:?}", fs::read_link(p).unwrap())));
        } else if ft.is_dir() {
            out.push((rel, format!("d:{:o}", m.permissions().mode())));
            let mut names: Vec<_> = fs::read_dir(p).unwrap().map(|e| e.unwrap().path()).collect();
            names.sort();
            for n in names {
                go(base, &n, out);
            }
        } else if ft.is_file() {
            let h = fs::read(p).map(|b| format!("{}:{}", b.len(), simple_hash(&b))).unwrap_or_else(|_| "unreadable".into());
            out.push((rel, format!("f:{:o}:{h}", m.permissions().mode())));
        } else {
            out.push((rel, "other".into()));
        }
    }
    go(p, p, &mut out);
    out
}

pub fn simple_hash(b: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for x in b {
        h ^= *x as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// `lstat` as the raw struct (flags, nanosecond times).
pub fn st(p: &Path) -> libc::stat {
    let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).unwrap();
    let mut s: libc::stat = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::lstat(c.as_ptr(), &mut s) }, 0, "lstat {}", p.display());
    s
}

pub fn ctime_ns(p: &Path) -> i128 {
    let s = st(p);
    s.st_ctime as i128 * 1_000_000_000 + s.st_ctime_nsec as i128
}

pub fn mtime_ns(p: &Path) -> i128 {
    let s = st(p);
    s.st_mtime as i128 * 1_000_000_000 + s.st_mtime_nsec as i128
}

pub fn atime_ns(p: &Path) -> i128 {
    let s = st(p);
    s.st_atime as i128 * 1_000_000_000 + s.st_atime_nsec as i128
}

/// Set mtime (and leave atime) without following a symlink.
pub fn set_mtime(p: &Path, secs: i64, nsec: i64) {
    let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).unwrap();
    let ts = [
        libc::timespec { tv_sec: 0, tv_nsec: libc::UTIME_OMIT },
        libc::timespec { tv_sec: secs, tv_nsec: nsec },
    ];
    assert_eq!(unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), ts.as_ptr(), libc::AT_SYMLINK_NOFOLLOW) }, 0);
}

pub fn mode_of(p: &Path) -> u32 {
    st(p).st_mode as u32 & 0o7777
}

pub fn flags_of(p: &Path) -> u32 {
    st(p).st_flags
}

pub const UF_IMMUTABLE: u32 = 0x2;
pub const UF_APPEND: u32 = 0x4;
pub const UF_COMPRESSED: u32 = 0x20;
pub const UF_HIDDEN: u32 = 0x8000;

/// Wait so a later ctime would differ from an earlier one.
pub fn tick() {
    std::thread::sleep(std::time::Duration::from_millis(20));
}
