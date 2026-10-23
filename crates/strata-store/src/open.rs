//! `open`, `open_read_only`, `close`, `quick_check` (store.md §5.1, §5.2),
//! plus the connection setup and the reader pool.

use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::ops::Deref;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use rusqlite::config::DbConfig;
use rusqlite::functions::FunctionFlags;
use rusqlite::{Connection, OpenFlags};
use serde_json::json;
use strata_apfs::{IoSide, classify_io_error, full_fsync, nearest_existing_ancestor, same_volume};
use strata_types::{Code, StrataError};

use crate::errors::{self, NOT_WRITTEN, corrupt, err, sql};
use crate::schema::{APPLICATION_ID, DDL, USER_VERSION};
use crate::seams::{self, Seam, Seams};
use crate::{Result, Store};

pub(crate) const LAYOUT_NAMES: [&str; 4] = ["store", "bases", "deps", "sessions"];
const MAGIC: &[u8; 16] = b"SQLite format 3\0";
const READER_BUSY_MS: u64 = 2000;

pub(crate) fn hex32() -> String {
    let mut b = [0u8; 16];
    unsafe { libc::arc4random_buf(b.as_mut_ptr().cast(), b.len()) };
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn ioe(e: &io::Error, side: IoSide, path: &Path, op: &str) -> StrataError {
    errors::io(e, side, path, op, NOT_WRITTEN)
}

/// F_FULLFSYNC a directory (or file) by path.
pub(crate) fn full_sync_path(p: &Path, op: &str, outcome: &str) -> Result<()> {
    let f = File::open(p).map_err(|e| errors::io(&e, IoSide::AfterWrite, p, op, outcome))?;
    full_fsync(&f).map_err(|e| errors::annotate(e, op, outcome))
}

/// `renamex_np(from, to, RENAME_EXCL)`.
pub(crate) fn rename_excl(from: &Path, to: &Path) -> io::Result<()> {
    let a = CString::new(from.as_os_str().as_bytes()).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let b = CString::new(to.as_os_str().as_bytes()).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let rc = unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_EXCL) };
    if rc == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

fn exists(p: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(p) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

fn dir_has_entries(p: &Path) -> io::Result<bool> {
    match fs::read_dir(p) {
        Ok(mut it) => Ok(it.next().is_some()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) if e.raw_os_error() == Some(libc::ENOTDIR) => Ok(true),
        Err(e) => Err(e),
    }
}

/// L1: `store/LOCK`, `O_CREAT | O_RDWR | O_CLOEXEC`, `flock(LOCK_EX | LOCK_NB)`.
fn take_lock(store_dir: &Path) -> Result<File> {
    let path = store_dir.join("LOCK");
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC)
        .open(&path)
        .map_err(|e| ioe(&e, IoSide::Write, &path, "open"))?;
    loop {
        let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(f);
        }
        let e = io::Error::last_os_error();
        match e.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EWOULDBLOCK) => {
                return Err(err(
                    Code::Busy,
                    "another process holds the store writer lock (is the daemon running?)",
                    json!({ "lock": "store-writer", "path": path.to_string_lossy() }),
                ));
            }
            _ => return Err(ioe(&e, IoSide::Write, &path, "open")),
        }
    }
}

/// Step 2 of §5.2: the file header, without SQLite.
fn check_header(db: &Path) -> Result<()> {
    let mut f = File::open(db).map_err(|e| ioe(&e, IoSide::Read, db, "open"))?;
    let mut buf = [0u8; 100];
    let mut got = 0;
    while got < buf.len() {
        match f.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(ioe(&e, IoSide::Read, db, "open")),
        }
    }
    let shown = db.to_string_lossy();
    if got < 100 || &buf[..16] != MAGIC {
        return Err(err(
            Code::Corrupt,
            format!("{shown} is not a strata store (no SQLite header)"),
            json!({ "reason": "application_id", "path": shown }),
        ));
    }
    let be = |o: usize| u32::from_be_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]);
    let app = be(68);
    if app != APPLICATION_ID as u32 {
        return Err(err(
            Code::Corrupt,
            format!("{shown} is not a strata store (application_id {app})"),
            json!({ "reason": "application_id", "path": shown, "expected": APPLICATION_ID, "actual": app }),
        ));
    }
    let uv = be(60);
    if uv != USER_VERSION as u32 {
        return Err(err(
            Code::Corrupt,
            format!("{shown} has schema version {uv}; this strata understands {USER_VERSION}"),
            json!({ "reason": "user_version", "path": shown, "expected": USER_VERSION, "actual": uv }),
        ));
    }
    if buf[18] != 2 || buf[19] != 2 {
        return Err(err(
            Code::Corrupt,
            format!("{shown} is not in WAL mode; strata never converts a store's journal mode"),
            json!({ "reason": "journal_mode", "path": shown, "read_version": buf[18], "write_version": buf[19] }),
        ));
    }
    Ok(())
}

fn pragma_text(c: &Connection, name: &str) -> rusqlite::Result<String> {
    c.query_row(&format!("PRAGMA {name}"), [], |r| {
        let v: rusqlite::types::Value = r.get(0)?;
        Ok(match v {
            rusqlite::types::Value::Integer(i) => i.to_string(),
            rusqlite::types::Value::Text(t) => t.to_lowercase(),
            other => format!("{other:?}"),
        })
    })
}

/// Step 3: re-check identity through SQLite.
fn check_identity(c: &Connection, op: &str) -> Result<()> {
    let s = |e| sql(e, op, NOT_WRITTEN);
    let app = pragma_text(c, "application_id").map_err(s)?;
    if app != APPLICATION_ID.to_string() {
        return Err(err(Code::Corrupt, "application_id mismatch", json!({ "reason": "application_id", "actual": app })));
    }
    let uv = pragma_text(c, "user_version").map_err(s)?;
    if uv != USER_VERSION.to_string() {
        return Err(err(Code::Corrupt, "user_version mismatch", json!({ "reason": "user_version", "actual": uv })));
    }
    let jm = pragma_text(c, "journal_mode").map_err(s)?;
    if jm != "wal" {
        return Err(err(Code::Corrupt, "journal_mode is not WAL", json!({ "reason": "journal_mode", "actual": jm })));
    }
    Ok(())
}

/// Set and read back one pragma; mismatch → `Unsupported`.
fn pin(c: &Connection, name: &str, set: &str, expect: &str, op: &str) -> Result<()> {
    c.execute_batch(&format!("PRAGMA {name} = {set}")).map_err(|e| sql(e, op, NOT_WRITTEN))?;
    let got = pragma_text(c, name).map_err(|e| sql(e, op, NOT_WRITTEN))?;
    if got != expect {
        return Err(err(
            Code::Unsupported,
            format!("SQLite would not keep {name} = {expect} (read back {got})"),
            json!({ "setting": name, "expected": expect, "actual": got }),
        ));
    }
    Ok(())
}

fn defensive(c: &Connection, op: &str) -> Result<()> {
    c.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true).map_err(|e| sql(e, op, NOT_WRITTEN))?;
    Ok(())
}

/// The §4.2 settings for a write connection (journal_mode is in the file).
fn pin_writer(c: &Connection, op: &str) -> Result<()> {
    defensive(c, op)?;
    c.busy_timeout(Duration::ZERO).map_err(|e| sql(e, op, NOT_WRITTEN))?;
    pin(c, "synchronous", "FULL", "2", op)?;
    pin(c, "fullfsync", "1", "1", op)?;
    pin(c, "checkpoint_fullfsync", "1", "1", op)?;
    pin(c, "foreign_keys", "1", "1", op)?;
    pin(c, "recursive_triggers", "1", "1", op)?;
    pin(c, "trusted_schema", "0", "0", op)?;
    let bt = pragma_text(c, "busy_timeout").map_err(|e| sql(e, op, NOT_WRITTEN))?;
    if bt != "0" {
        return Err(err(Code::Unsupported, "busy_timeout did not stick", json!({ "setting": "busy_timeout", "expected": "0", "actual": bt })));
    }
    Ok(())
}

fn pin_reader(c: &Connection, op: &str) -> Result<()> {
    defensive(c, op)?;
    c.busy_timeout(Duration::from_millis(READER_BUSY_MS)).map_err(|e| sql(e, op, NOT_WRITTEN))?;
    pin(c, "trusted_schema", "0", "0", op)?;
    pin(c, "query_only", "1", "1", op)?;
    Ok(())
}

pub(crate) fn register_gc_fn(c: &Connection, flag: Arc<AtomicBool>) -> Result<()> {
    c.create_scalar_function(
        "strata_gc_active",
        0,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_INNOCUOUS,
        move |_| Ok(flag.load(Ordering::SeqCst) as i64),
    )
    .map_err(|e| sql(e, "open", NOT_WRITTEN))
}

type SchemaRow = (String, String, String, Option<String>);

fn schema_rows(c: &Connection) -> rusqlite::Result<Vec<SchemaRow>> {
    let mut st = c.prepare("SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name")?;
    let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
    rows.collect()
}

/// Step 5: the schema must equal the pinned DDL, row for row.
fn check_schema(c: &Connection, op: &str) -> Result<()> {
    let mem = Connection::open_in_memory().map_err(|e| sql(e, op, NOT_WRITTEN))?;
    register_gc_fn(&mem, Arc::new(AtomicBool::new(false)))?;
    mem.execute_batch(DDL).map_err(|e| sql(e, op, NOT_WRITTEN))?;
    let want = schema_rows(&mem).map_err(|e| sql(e, op, NOT_WRITTEN))?;
    let got = schema_rows(c).map_err(|e| sql(e, op, NOT_WRITTEN))?;
    let show = |r: Option<&SchemaRow>| r.map(|(t, n, tb, s)| json!({ "type": t, "name": n, "tbl_name": tb, "sql": s }));
    let n = want.len().max(got.len());
    for i in 0..n {
        let (w, g) = (want.get(i), got.get(i));
        if w != g {
            return Err(err(
                Code::Corrupt,
                "the store's schema differs from this strata's schema; refusing to open (no repair, no migration)",
                json!({ "reason": "schema", "index": i, "expected": show(w), "actual": show(g) }),
            ));
        }
    }
    Ok(())
}

fn quick_check_on(c: &Connection) -> Result<()> {
    let mut st = c.prepare("PRAGMA quick_check").map_err(|e| sql(e, "quick_check", NOT_WRITTEN))?;
    let rows: Vec<String> = st
        .query_map([], |r| r.get::<_, String>(0))
        .and_then(|it| it.collect())
        .map_err(|e| sql(e, "quick_check", NOT_WRITTEN))?;
    if rows.len() == 1 && rows[0] == "ok" {
        return Ok(());
    }
    let first: Vec<&String> = rows.iter().take(10).collect();
    Err(err(Code::Corrupt, "SQLite quick_check found damage", json!({ "reason": "sqlite", "messages": first })))
}

/// Open one store connection. Under `test-seams` every connection goes
/// through the instrumented VFS (vfs.rs) so tests can count and fail syncs.
pub(crate) fn open_conn(path: &Path, flags: OpenFlags) -> rusqlite::Result<Connection> {
    #[cfg(feature = "test-seams")]
    {
        crate::vfs::ensure_registered();
        Connection::open_with_flags_and_vfs(path, flags, crate::vfs::NAME)
    }
    #[cfg(not(feature = "test-seams"))]
    {
        Connection::open_with_flags(path, flags)
    }
}

fn open_reader_conn(db: &Path) -> Result<Connection> {
    let c = open_conn(db, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|e| sql(e, "open", NOT_WRITTEN))?;
    pin_reader(&c, "open")?;
    Ok(c)
}

/// A pooled read-only connection; goes back to the pool on drop.
pub(crate) struct Reader<'a> {
    store: &'a Store,
    conn: Option<Connection>,
}

impl Deref for Reader<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn.as_ref().expect("reader connection")
    }
}

impl Drop for Reader<'_> {
    fn drop(&mut self) {
        if let Some(c) = self.conn.take() {
            if c.is_autocommit() {
                self.store.readers.lock().unwrap_or_else(|p| p.into_inner()).push(c);
            }
        }
    }
}

impl Store {
    pub(crate) fn db_path(&self) -> PathBuf {
        self.store_dir().join("store.db")
    }

    pub(crate) fn reader(&self) -> Result<Reader<'_>> {
        let pooled = self.readers.lock().unwrap_or_else(|p| p.into_inner()).pop();
        let conn = match pooled {
            Some(c) => c,
            None => {
                let c = open_reader_conn(&self.db_path())?;
                register_gc_fn(&c, self.gc_active.clone())?;
                c
            }
        };
        Ok(Reader { store: self, conn: Some(c_or(conn)) })
    }

    /// Run `f` inside one read transaction on a pooled reader.
    pub(crate) fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let r = self.reader()?;
        r.execute_batch("BEGIN DEFERRED").map_err(errors::sql_read)?;
        let out = f(&r);
        let end = r.execute_batch(if out.is_ok() { "COMMIT" } else { "ROLLBACK" });
        let v = out?;
        end.map_err(errors::sql_read)?;
        Ok(v)
    }

    pub fn open(root: &Path, checkouts: &[&Path]) -> Result<Store> {
        if !root.is_absolute() {
            return Err(err(
                Code::InvalidInput,
                format!("store root must be absolute: {}", root.display()),
                json!({ "reason": "not_absolute", "path": root.to_string_lossy() }),
            ));
        }
        // 2. same volume, nothing written
        let anc = nearest_existing_ancestor(root).map_err(|e| classify_io_error(&e, IoSide::Read, root))?;
        for c in checkouts {
            match fs::metadata(c) {
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    return Err(err(
                        Code::NoSuchPath,
                        format!("checkout does not exist: {}", c.display()),
                        json!({ "path": c.to_string_lossy() }),
                    ));
                }
                Err(e) => return Err(classify_io_error(&e, IoSide::Read, c)),
            }
            let same = same_volume(&anc, c).map_err(|e| classify_io_error(&e, IoSide::Read, c))?;
            if !same {
                return Err(err(
                    Code::CrossVolume,
                    format!("store root {} is not on the same volume as checkout {}", root.display(), c.display()),
                    json!({ "root": root.to_string_lossy(), "checkout": c.to_string_lossy() }),
                ));
            }
        }
        // 3. look at <root>
        match fs::metadata(root) {
            Ok(m) if m.is_dir() => {
                let mut foreign = Vec::new();
                for ent in fs::read_dir(root).map_err(|e| ioe(&e, IoSide::Read, root, "open"))? {
                    let ent = ent.map_err(|e| ioe(&e, IoSide::Read, root, "open"))?;
                    let name = ent.file_name().to_string_lossy().into_owned();
                    if !LAYOUT_NAMES.contains(&name.as_str()) {
                        foreign.push(name);
                    }
                }
                if !foreign.is_empty() {
                    foreign.sort();
                    foreign.truncate(5);
                    return Err(err(
                        Code::NotEmpty,
                        format!("{} is not empty and is not a strata store", root.display()),
                        json!({ "path": root.to_string_lossy(), "entries": foreign }),
                    ));
                }
            }
            Ok(_) => {
                return Err(err(
                    Code::NotDirectory,
                    format!("store root is not a directory: {}", root.display()),
                    json!({ "path": root.to_string_lossy() }),
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(ioe(&e, IoSide::Read, root, "open")),
        }
        let store_dir = root.join("store");
        let db = store_dir.join("store.db");
        // 4.
        if exists(&db).map_err(|e| ioe(&e, IoSide::Read, &db, "open"))? {
            return open_existing(root, None);
        }
        for d in ["bases", "deps", "sessions"] {
            let p = root.join(d);
            if dir_has_entries(&p).map_err(|e| ioe(&e, IoSide::Read, &p, "open"))? {
                return Err(err(
                    Code::Corrupt,
                    format!("{} has data but store/store.db is missing; refusing to create an empty store over it", root.display()),
                    json!({ "reason": "store_db_missing", "path": db.to_string_lossy() }),
                ));
            }
        }
        create(root, &anc)
    }

    pub fn open_read_only(root: &Path) -> Result<Store> {
        let store_dir = root.join("store");
        let db = store_dir.join("store.db");
        if !exists(&db).map_err(|e| ioe(&e, IoSide::Read, &db, "open"))? {
            return Err(err(
                Code::NoSuchPath,
                format!("no store at {}", root.display()),
                json!({ "path": db.to_string_lossy() }),
            ));
        }
        check_header(&db)?;
        let real_root = fs::canonicalize(root).map_err(|e| ioe(&e, IoSide::Read, root, "open"))?;
        let rdb = real_root.join("store").join("store.db");
        let c = open_reader_conn(&rdb)?;
        check_identity(&c, "open")?;
        check_schema(&c, "open")?;
        let gc_active = Arc::new(AtomicBool::new(false));
        register_gc_fn(&c, gc_active.clone())?;
        Ok(new_store(root, real_root, true, c, None, gc_active))
    }

    pub fn close(self) -> Result<()> {
        let Store { real_root, read_only, db, readers, writer_lock, poisoned, .. } = self;
        let readers = readers.into_inner().unwrap_or_else(|p| p.into_inner());
        for r in readers {
            let _ = r.close();
        }
        let conn = db.into_inner().unwrap_or_else(|p| p.into_inner());
        conn.close().map_err(|(_, e)| sql(e, "close", NOT_WRITTEN))?;
        if read_only {
            return Ok(());
        }
        if !poisoned.load(Ordering::SeqCst) {
            let store_dir = real_root.join("store");
            let marker = store_dir.join("clean-shutdown");
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_CLOEXEC)
                .open(&marker)
            {
                Ok(f) => {
                    full_fsync(&f).map_err(|e| errors::annotate(e, "close", errors::UNCERTAIN))?;
                    full_sync_path(&store_dir, "close", errors::UNCERTAIN)?;
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => {
                    let e = errors::io(&e, IoSide::Write, &marker, "close", NOT_WRITTEN);
                    return Err(StrataError { code: Code::Unknown, ..e });
                }
            }
        }
        drop(writer_lock);
        Ok(())
    }

    pub fn quick_check(&self) -> Result<()> {
        let r = self.reader()?;
        quick_check_on(&r)
    }
}

fn c_or(c: Connection) -> Connection {
    c
}

fn new_store(root: &Path, real_root: PathBuf, read_only: bool, db: Connection, lock: Option<File>, gc_active: Arc<AtomicBool>) -> Store {
    Store {
        root: root.to_path_buf(),
        real_root,
        read_only,
        db: Mutex::new(db),
        readers: Mutex::new(Vec::new()),
        writer_lock: lock,
        blob_gate: RwLock::new(()),
        gc_active,
        gc_running: Mutex::new(()),
        poisoned: AtomicBool::new(false),
        seams: Seams::default(),
    }
}

/// `mkdir` one level, remembering it when this call created it.
fn mkdir_track(p: &Path, created: &mut Vec<PathBuf>) -> Result<()> {
    match fs::create_dir(p) {
        Ok(()) => {
            created.push(p.to_path_buf());
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(ioe(&e, IoSide::Write, p, "init")),
    }
}

fn mkdir_ok(p: &Path) -> Result<()> {
    match fs::create_dir(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(ioe(&e, IoSide::Write, p, "init")),
    }
}

/// §5.1 steps 5–10.
fn create(root: &Path, anc: &Path) -> Result<Store> {
    let mut created = Vec::new();
    // <root> and any missing parents below the nearest existing ancestor
    let mut chain = Vec::new();
    let mut p = root;
    while p != anc {
        chain.push(p.to_path_buf());
        match p.parent() {
            Some(q) => p = q,
            None => break,
        }
    }
    for d in chain.iter().rev() {
        mkdir_track(d, &mut created)?;
    }
    let store_dir = root.join("store");
    let tmp = store_dir.join("tmp");
    mkdir_track(&store_dir, &mut created)?;
    mkdir_track(&tmp, &mut created)?;
    let lock_path = store_dir.join("LOCK");
    let lock_existed = exists(&lock_path).map_err(|e| ioe(&e, IoSide::Read, &lock_path, "init"))?;
    let lock = take_lock(&store_dir)?;
    let db = store_dir.join("store.db");
    if exists(&db).map_err(|e| ioe(&e, IoSide::Read, &db, "init"))? {
        return open_existing(root, Some(lock));
    }
    // 6. probe clonefile
    let probe = tmp.join(format!("probe-{}", hex32()));
    let probe2 = tmp.join(format!("probe-{}", hex32()));
    let probed = (|| -> Result<()> {
        fs::write(&probe, b"strata clone probe\n").map_err(|e| ioe(&e, IoSide::Write, &probe, "init"))?;
        strata_apfs::clone_file(&probe, &probe2)
    })();
    let _ = fs::remove_file(&probe2);
    let _ = fs::remove_file(&probe);
    if let Err(e) = probed {
        if e.code == Code::Unsupported {
            if !lock_existed {
                let _ = fs::remove_file(&lock_path);
            }
            for d in created.iter().rev() {
                let _ = fs::remove_dir(d);
            }
            drop(lock);
        }
        return Err(e);
    }
    // 7. layout
    let blobs = store_dir.join("blobs");
    mkdir_ok(&blobs)?;
    for i in 0..=255u8 {
        mkdir_ok(&blobs.join(format!("{i:02x}")))?;
    }
    mkdir_ok(&store_dir.join("trash"))?;
    for d in ["bases", "deps", "sessions"] {
        mkdir_ok(&root.join(d))?;
    }
    // 8. temp db
    let tdb = tmp.join(format!("init-{}.db", hex32()));
    {
        let c = open_conn(
            &tdb,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| sql(e, "init", NOT_WRITTEN))?;
        let jm: String = c
            .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
            .map_err(|e| sql(e, "init", NOT_WRITTEN))?;
        if jm.to_lowercase() != "wal" {
            return Err(err(
                Code::Unsupported,
                "SQLite would not put the new store in WAL mode",
                json!({ "setting": "journal_mode", "expected": "wal", "actual": jm }),
            ));
        }
        pin_writer(&c, "init")?;
        register_gc_fn(&c, Arc::new(AtomicBool::new(false)))?;
        c.execute_batch(&format!("BEGIN IMMEDIATE;\n{DDL}\nCOMMIT;")).map_err(|e| sql(e, "init", NOT_WRITTEN))?;
        c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(())).map_err(|e| sql(e, "init", NOT_WRITTEN))?;
        c.close().map_err(|(_, e)| sql(e, "init", NOT_WRITTEN))?;
    }
    full_sync_path(&tdb, "init", NOT_WRITTEN)?;
    if seams::take_next_open(&Seam::InitBeforeRename) {
        return Err(err(
            Code::Unknown,
            "init interrupted before the rename (test seam)",
            json!({ "op": "init", "outcome": NOT_WRITTEN, "seam": "init_before_rename" }),
        ));
    }
    // 9. publish atomically
    rename_excl(&tdb, &db).map_err(|e| ioe(&e, IoSide::Write, &db, "init"))?;
    full_sync_path(&store_dir, "init", errors::UNCERTAIN)?;
    open_existing(root, Some(lock))
}

/// §5.2, the writer path (from step 1, or step 2 with L1 already held).
fn open_existing(root: &Path, lock: Option<File>) -> Result<Store> {
    let store_dir = root.join("store");
    let lock = match lock {
        Some(l) => l,
        None => take_lock(&store_dir)?,
    };
    let db = store_dir.join("store.db");
    check_header(&db)?;
    let real_root = fs::canonicalize(root).map_err(|e| ioe(&e, IoSide::Read, root, "open"))?;
    let rstore = real_root.join("store");
    let rdb = rstore.join("store.db");
    let c = open_conn(&rdb, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|e| sql(e, "open", NOT_WRITTEN))?;
    defensive(&c, "open")?;
    c.busy_timeout(Duration::ZERO).map_err(|e| sql(e, "open", NOT_WRITTEN))?;
    check_identity(&c, "open")?;
    pin_writer(&c, "open")?;
    check_schema(&c, "open")?;
    let marker = rstore.join("clean-shutdown");
    if exists(&marker).map_err(|e| ioe(&e, IoSide::Read, &marker, "open"))? {
        fs::remove_file(&marker).map_err(|e| {
            let e = errors::io(&e, IoSide::Write, &marker, "open", NOT_WRITTEN);
            StrataError { code: Code::Unknown, ..e }
        })?;
        full_sync_path(&rstore, "open", errors::UNCERTAIN)?;
    } else {
        quick_check_on(&c)?;
    }
    let gc_active = Arc::new(AtomicBool::new(false));
    register_gc_fn(&c, gc_active.clone())?;
    Ok(new_store(root, real_root, false, c, Some(lock), gc_active))
}

#[allow(dead_code)]
pub(crate) fn _corrupt_unused() -> StrataError {
    corrupt("x", "y")
}
