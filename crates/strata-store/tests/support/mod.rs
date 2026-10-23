//! Shared fixtures for the store tests (store.md §7).
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::Ordering;

#[allow(unused_imports)]
pub use strata_store::sql::{Connection, OptionalExtension, Value, params};
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

/// A raw connection on store.db (Turso, multi-process WAL like the store),
/// configured like the store's writer.
pub fn connect(db: &Path) -> Connection {
    strata_store::sql::open(db, strata_store::sql::Mode::Shared).expect("raw open")
}

/// The gc delete gate (`strata_gc_gate`, the stand-in for the design's
/// `strata_gc_active()` function). `store(true)` commits `active = 1`, which
/// lets every connection delete until it is stored back to false.
#[derive(Clone)]
pub struct Gate(PathBuf);

impl Gate {
    pub fn store(&self, on: bool, _: Ordering) {
        let c = connect(&self.0);
        c.execute("UPDATE strata_gc_gate SET active = ?1", [on as i64]).expect("set gc gate");
    }
}

/// A raw connection that, when it opened the gc gate, closes it on drop.
pub struct Raw {
    c: Option<Connection>,
    gate: Gate,
    opened: bool,
}

impl std::ops::Deref for Raw {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.c.as_ref().unwrap()
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        if let Some(c) = self.c.take() {
            if !c.is_autocommit() {
                let _ = c.execute_batch("ROLLBACK");
            }
            if self.opened {
                let _ = c.execute("UPDATE strata_gc_gate SET active = 0", ());
            }
        }
    }
}

/// A raw connection on store.db with foreign keys on; `gc = true` opens the
/// gc delete gate for its lifetime (the gate is in the database, so other
/// connections see it too).
pub fn raw(db: &Path, gc: bool) -> (Raw, Gate) {
    let c = connect(db);
    c.execute_batch("PRAGMA foreign_keys = 1;").unwrap();
    let gate = Gate(db.to_path_buf());
    if gc {
        c.execute("UPDATE strata_gc_gate SET active = 1", ()).expect("open gc gate");
    }
    (Raw { c: Some(c), gate: gate.clone(), opened: true }, gate)
}

/// A read-only raw connection (`PRAGMA query_only`; Turso ignores the
/// read-only open flag for files already open in the process).
pub fn ro(db: &Path) -> Connection {
    let c = connect(db);
    c.execute_batch("PRAGMA query_only = 1").unwrap();
    c
}

/// A raw connection with nothing configured (like a sqlite3 CLI).
pub fn bare(db: &Path) -> Connection {
    connect(db)
}

/// Read a raw connection's single value as text-ish debug form.
pub fn val(c: &Connection, sql: &str) -> Value {
    c.query_row(sql, (), |r| r.get(0)).unwrap()
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
