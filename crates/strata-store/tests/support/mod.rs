//! Shared fixtures for the store tests (store.md §7).
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::functions::FunctionFlags;
use rusqlite::{Connection, OpenFlags};
use strata_store::Store;
use strata_types::{Code, StrataError};
use tempfile::TempDir;

pub mod world;

pub const CHILD_ENV: &str = "STRATA_STORE_TEST_CHILD";

/// A temp dir on the system (APFS) volume with a not-yet-existing store
/// root and an existing checkout directory.
pub struct Env {
    pub dir: TempDir,
    pub root: PathBuf,
    pub checkout: PathBuf,
}

pub fn env() -> Env {
    let dir = tempfile::Builder::new().prefix("strata-store-").tempdir().expect("tempdir");
    let base = fs::canonicalize(dir.path()).unwrap();
    let root = base.join("strata");
    let checkout = base.join("checkout");
    fs::create_dir(&checkout).unwrap();
    Env { dir, root, checkout }
}

impl Env {
    pub fn open(&self) -> Store {
        Store::open(&self.root, &[&self.checkout]).expect("open store")
    }

    pub fn try_open(&self) -> Result<Store, StrataError> {
        Store::open(&self.root, &[&self.checkout])
    }

    pub fn db(&self) -> PathBuf {
        self.root.join("store/store.db")
    }

    pub fn store_dir(&self) -> PathBuf {
        self.root.join("store")
    }
}

/// A raw connection on store.db, configured like the store's writer, with
/// `strata_gc_active` registered (returning the flag's value).
pub fn raw(db: &Path, gc: bool) -> (Connection, Arc<AtomicBool>) {
    let c = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_WRITE).expect("raw open");
    c.execute_batch("PRAGMA foreign_keys = 1; PRAGMA recursive_triggers = 1; PRAGMA trusted_schema = 0;")
        .unwrap();
    let flag = Arc::new(AtomicBool::new(gc));
    let f = flag.clone();
    c.create_scalar_function(
        "strata_gc_active",
        0,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_INNOCUOUS,
        move |_| Ok(f.load(Ordering::SeqCst) as i64),
    )
    .unwrap();
    (c, flag)
}

/// A raw connection without the function registered (like the sqlite3 CLI).
pub fn bare(db: &Path) -> Connection {
    Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_WRITE).expect("bare open")
}

pub fn detail<'a>(e: &'a StrataError, key: &str) -> Option<&'a serde_json::Value> {
    e.detail.as_ref()?.get(key)
}

pub fn reason(e: &StrataError) -> Option<&str> {
    detail(e, "reason")?.as_str()
}

#[track_caller]
pub fn assert_code(e: &StrataError, code: Code) {
    assert_eq!(e.code, code, "unexpected error: {e:?}");
}

#[track_caller]
pub fn assert_reason(e: &StrataError, code: Code, r: &str) {
    assert_eq!(e.code, code, "unexpected error: {e:?}");
    assert_eq!(reason(e), Some(r), "unexpected reason: {e:?}");
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Snap {
    Dir,
    File(Vec<u8>),
    Link(PathBuf),
}

/// Every path under `dir` with its kind and bytes.
pub fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Snap> {
    let mut out = BTreeMap::new();
    fn walk(base: &Path, p: &Path, out: &mut BTreeMap<PathBuf, Snap>) {
        let rd = match fs::read_dir(p) {
            Ok(r) => r,
            Err(_) => return,
        };
        for e in rd {
            let e = e.unwrap();
            let path = e.path();
            let rel = path.strip_prefix(base).unwrap().to_path_buf();
            let m = fs::symlink_metadata(&path).unwrap();
            if m.file_type().is_symlink() {
                out.insert(rel, Snap::Link(fs::read_link(&path).unwrap()));
            } else if m.is_dir() {
                out.insert(rel, Snap::Dir);
                walk(base, &path, out);
            } else {
                out.insert(rel, Snap::File(fs::read(&path).unwrap_or_default()));
            }
        }
    }
    walk(dir, dir, &mut out);
    out
}

/// Names under `dir` (relative), sorted.
pub fn listing(dir: &Path) -> Vec<PathBuf> {
    snapshot(dir).into_keys().collect()
}

/// Re-run one test of this binary as a child process with `CHILD_ENV` set.
pub fn run_child(test: &str, envs: &[(&str, &str)]) -> std::process::Output {
    let exe = std::env::current_exe().unwrap();
    let mut cmd = Command::new(exe);
    cmd.args(["--exact", test, "--nocapture", "--test-threads=1", "--include-ignored"]).env(CHILD_ENV, "1");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().expect("run child")
}

pub fn is_child() -> bool {
    std::env::var_os(CHILD_ENV).is_some()
}

/// A disk image from `hdiutil`, attached under a temp mount point.
pub struct Image {
    pub mount: PathBuf,
    _dir: TempDir,
}

impl Image {
    /// `fs` is e.g. "APFS" or "MS-DOS FAT32".
    pub fn new(fs_name: &str) -> Image {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("img.dmg");
        let mount = dir.path().join("mnt");
        fs::create_dir(&mount).unwrap();
        let st = Command::new("hdiutil")
            .args(["create", "-size", "64m", "-fs", fs_name, "-volname", "STRATATEST", "-type", "UDIF"])
            .arg(&img)
            .status()
            .unwrap();
        assert!(st.success(), "hdiutil create failed");
        let st = Command::new("hdiutil")
            .args(["attach", "-nobrowse", "-owners", "on", "-mountpoint"])
            .arg(&mount)
            .arg(&img)
            .status()
            .unwrap();
        assert!(st.success(), "hdiutil attach failed");
        let mount = fs::canonicalize(&mount).unwrap();
        Image { mount, _dir: dir }
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        let _ = Command::new("hdiutil").args(["detach", "-force"]).arg(&self.mount).status();
    }
}
