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
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::sql::{self, Connection, Mode};
use serde_json::json;
use strata_apfs::{IoSide, classify_io_error, full_fsync, nearest_existing_ancestor, retry_eintr, same_volume};
use strata_types::{Code, StrataError};

use crate::errors::{self, NOT_WRITTEN, corrupt, err, sql};
use crate::schema::{APPLICATION_ID, BASE_VERSION, LATEST, ddl, pending};
use crate::seams::{self, Seam, Seams};
use crate::{Result, Store};

/// The names `open` accepts in `<root>`. `daemon` is the daemon's scratch
/// and kept push scenes (`<root>/daemon/{push,observe,approve}`): without
/// it, any daemon start after a standalone push or an approve was refused
/// `NotEmpty` (found with review M6) [实现时补的决定].
/// Entries a strata root may hold. `search` is the daemon's search index
/// (strata-search); incident 2026-10-27: a root holding it was refused.
pub(crate) const LAYOUT_NAMES: [&str; 6] = ["store", "bases", "deps", "sessions", "daemon", "search"];
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
    match retry_eintr(|| fs::symlink_metadata(p)) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

fn dir_has_entries(p: &Path) -> io::Result<bool> {
    match retry_eintr(|| fs::read_dir(p).map(|mut it| it.next().is_some())) {
        Ok(any) => Ok(any),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) if e.raw_os_error() == Some(libc::ENOTDIR) => Ok(true),
        Err(e) => Err(e),
    }
}

/// L1: `store/LOCK`, `O_CREAT | O_RDWR | O_CLOEXEC`, `flock(LOCK_EX | LOCK_NB)`.
fn take_lock(store_dir: &Path) -> Result<File> {
    let path = store_dir.join("LOCK");
    let mut opts = OpenOptions::new();
    opts.read(true).write(true).create(true).truncate(false).mode(0o600).custom_flags(libc::O_CLOEXEC);
    let f = retry_eintr(|| opts.open(&path)).map_err(|e| ioe(&e, IoSide::Write, &path, "open"))?;
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
    let mut f = retry_eintr(|| File::open(db)).map_err(|e| ioe(&e, IoSide::Read, db, "open"))?;
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
    // The header may lag the WAL by one migration; PRAGMA user_version
    // (step 3) decides, the header only gates the range.
    let uv = be(60);
    if !(BASE_VERSION..=LATEST).contains(&uv) {
        return Err(bad_version(&shown, uv));
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

fn pragma_text(c: &Connection, name: &str) -> sql::SqlResult<String> {
    c.query_row(&format!("PRAGMA {name}"), [], |r| {
        let v: sql::Value = r.get(0)?;
        Ok(match v {
            sql::Value::Integer(i) => i.to_string(),
            sql::Value::Text(t) => t.to_lowercase(),
            other => format!("{other:?}"),
        })
    })
}

fn bad_version(shown: &str, uv: u32) -> StrataError {
    // rm-followups §1.11: an older CLI meeting a store its daemon migrated
    // says what to do, not only that the version is out of range.
    let why = if uv > LATEST {
        format!("; this git-strata is older than the store (v{uv}); install the matching git-strata")
    } else {
        String::new()
    };
    err(
        Code::Corrupt,
        format!("{shown} has schema version {uv}; this strata understands {BASE_VERSION}..={LATEST}{why}"),
        json!({ "reason": "user_version", "path": shown, "expected": LATEST, "actual": uv }),
    )
}

/// `Database schema changed`: what a connection that lived through
/// another process's migration reports on the first statement of a
/// transaction that touches a table (`PRAGMA user_version` does not), and
/// keeps reporting in every explicit transaction until an autocommit
/// statement has reloaded its schema.
fn schema_changed(e: &StrataError) -> bool {
    const MSG: &str = "Database schema changed";
    e.message.contains(MSG) || e.detail.as_ref().and_then(|d| d.get("message")).and_then(|m| m.as_str()).is_some_and(|m| m.contains(MSG))
}

/// Run `f` in one read transaction on `c`; if the store's schema changed
/// under this connection (a migration by another process), run it again,
/// once, in a fresh transaction. Reads have no side effects to repeat.
pub(crate) fn read_txn<T>(c: &Connection, op: &str, f: impl Fn(&Connection) -> Result<T>) -> Result<T> {
    let mut retried = false;
    loop {
        c.execute_batch("BEGIN DEFERRED").map_err(|e| sql(e, op, NOT_WRITTEN))?;
        let out = f(c);
        let end = c.execute_batch(if out.is_ok() { "COMMIT" } else { "ROLLBACK" });
        match out {
            Err(e) if !retried && schema_changed(&e) => {
                // Inside BEGIN the connection keeps its stale schema; one
                // autocommit statement makes it reload (Turso 0.9.0-pre.1).
                c.query_row("SELECT count(*) FROM sqlite_schema", (), |r| r.get::<_, i64>(0)).map_err(|e| sql(e, op, NOT_WRITTEN))?;
                retried = true;
            }
            Err(e) => return Err(e),
            Ok(v) => {
                end.map_err(|e| sql(e, op, NOT_WRITTEN))?;
                return Ok(v);
            }
        }
    }
}

/// Step 3: re-check identity through SQLite; the schema version.
fn check_identity(c: &Connection, op: &str) -> Result<u32> {
    let s = |e| sql(e, op, NOT_WRITTEN);
    let app = pragma_text(c, "application_id").map_err(s)?;
    if app != APPLICATION_ID.to_string() {
        return Err(err(Code::Corrupt, "application_id mismatch", json!({ "reason": "application_id", "actual": app })));
    }
    let uv = pragma_text(c, "user_version").map_err(s)?;
    let v: u32 = match uv.parse() {
        Ok(v) if (BASE_VERSION..=LATEST).contains(&v) => v,
        _ => return Err(err(Code::Corrupt, "user_version mismatch", json!({ "reason": "user_version", "actual": uv }))),
    };
    let jm = pragma_text(c, "journal_mode").map_err(s)?;
    if jm != "wal" {
        return Err(err(Code::Corrupt, "journal_mode is not WAL", json!({ "reason": "journal_mode", "actual": jm })));
    }
    Ok(v)
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

/// The §4.2 settings for a write connection (journal_mode is in the file).
/// `SQLITE_DBCONFIG_DEFENSIVE` has no Turso equivalent; Turso refuses
/// writes to `sqlite_schema` unconditionally, which is the part we need.
fn pin_writer(c: &Connection, op: &str) -> Result<()> {
    c.busy_timeout(Duration::ZERO).map_err(|e| sql(e, op, NOT_WRITTEN))?;
    pin(c, "synchronous", "FULL", "2", op)?;
    pin(c, "fullfsync", "1", "1", op)?;
    // No `checkpoint_fullfsync` in Turso (unknown pragma, silently ignored):
    // its pager uses the one `fullfsync` sync type for commits and
    // checkpoints alike, so pinning `fullfsync` covers both.
    pin(c, "foreign_keys", "1", "1", op)?;
    // `recursive_triggers` and `trusted_schema` are unknown to Turso (0.8.3,
    // 0.9.0-pre.1: accepted, ignored): there are no app-defined SQL functions or virtual
    // tables for trusted_schema to guard, and REPLACE's implicit delete is
    // covered by tests/guards.rs against the engine's actual behaviour.
    let bt = pragma_text(c, "busy_timeout").map_err(|e| sql(e, op, NOT_WRITTEN))?;
    if bt != "0" {
        return Err(err(Code::Unsupported, "busy_timeout did not stick", json!({ "setting": "busy_timeout", "expected": "0", "actual": bt })));
    }
    Ok(())
}

fn pin_reader(c: &Connection, op: &str) -> Result<()> {
    c.busy_timeout(Duration::from_millis(READER_BUSY_MS)).map_err(|e| sql(e, op, NOT_WRITTEN))?;
    pin(c, "foreign_keys", "1", "1", op)?;
    pin(c, "query_only", "1", "1", op)?;
    Ok(())
}

type SchemaRow = (String, String, String, Option<String>);

fn schema_rows(c: &Connection) -> sql::SqlResult<Vec<SchemaRow>> {
    let mut st = c.prepare("SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name")?;
    let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
    rows.collect()
}

/// Step 5: the schema must equal the pinned DDL of version `v`, row for row.
fn check_schema(c: &Connection, op: &str, v: u32) -> Result<()> {
    let mem = sql::open_in_memory().map_err(|e| sql(e, op, NOT_WRITTEN))?;
    mem.execute_batch(&ddl(v)).map_err(|e| sql(e, op, NOT_WRITTEN))?;
    let want = schema_rows(&mem).map_err(|e| sql(e, op, NOT_WRITTEN))?;
    let got = schema_rows(c).map_err(|e| sql(e, op, NOT_WRITTEN))?;
    let show = |r: Option<&SchemaRow>| r.map(|(t, n, tb, s)| json!({ "type": t, "name": n, "tbl_name": tb, "sql": s }));
    let n = want.len().max(got.len());
    for i in 0..n {
        let (w, g) = (want.get(i), got.get(i));
        if w != g {
            return Err(err(
                Code::Corrupt,
                "the store's schema differs from this strata's schema for its version; refusing to open (no repair)",
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

/// Open one store connection (Turso, multi-process WAL). Under
/// `test-seams` it goes through the instrumented I/O (vfs.rs) so tests can
/// count and fail syncs.
pub(crate) fn open_conn(path: &Path) -> sql::SqlResult<Connection> {
    sql::open(path, Mode::Shared)
}

fn open_reader_conn(db: &Path) -> Result<Connection> {
    let c = open_conn(db).map_err(|e| sql(e, "open", NOT_WRITTEN))?;
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
        if let Some(c) = self.conn.take()
            && c.is_autocommit()
        {
            self.store.readers.lock().unwrap_or_else(|p| p.into_inner()).push(c);
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
            None => open_reader_conn(&self.db_path())?,
        };
        Ok(Reader { store: self, conn: Some(c_or(conn)) })
    }

    /// Run `f` inside one read transaction on a pooled reader.
    pub(crate) fn read<T>(&self, f: impl Fn(&Connection) -> Result<T>) -> Result<T> {
        let r = self.reader()?;
        read_txn(&r, "read", f)
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
        let anc = retry_eintr(|| nearest_existing_ancestor(root)).map_err(|e| classify_io_error(&e, IoSide::Read, root))?;
        for c in checkouts {
            match retry_eintr(|| fs::metadata(c)) {
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
            let same = retry_eintr(|| same_volume(&anc, c)).map_err(|e| classify_io_error(&e, IoSide::Read, c))?;
            if !same {
                return Err(err(
                    Code::CrossVolume,
                    format!("store root {} is not on the same volume as checkout {}", root.display(), c.display()),
                    json!({ "root": root.to_string_lossy(), "checkout": c.to_string_lossy() }),
                ));
            }
        }
        // Incident 2026-10-27: an existing store opens whatever else its
        // root holds; the foreign-entry check below guards only creating
        // a store in a directory that is not one. (An error here is left
        // to steps 3 and 4 to classify.)
        if let Ok(true) = exists(&root.join("store").join("store.db")) {
            return open_existing(root, None);
        }
        // 3. look at <root>
        match retry_eintr(|| fs::metadata(root)) {
            Ok(m) if m.is_dir() => {
                let mut foreign = Vec::new();
                let names = retry_eintr(|| fs::read_dir(root)?.map(|e| e.map(|e| e.file_name())).collect::<io::Result<Vec<_>>>())
                    .map_err(|e| ioe(&e, IoSide::Read, root, "open"))?;
                for name in names {
                    let name = name.to_string_lossy().into_owned();
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
        let real_root = retry_eintr(|| fs::canonicalize(root)).map_err(|e| ioe(&e, IoSide::Read, root, "open"))?;
        let rdb = real_root.join("store").join("store.db");
        let c = open_reader_conn(&rdb)?;
        // Version and schema from one snapshot: a migration in the daemon
        // may land between any two statements otherwise.
        read_txn(&c, "open", |c| check_identity(c, "open").and_then(|v| check_schema(c, "open", v)))?;
        Ok(new_store(root, real_root, true, c, None))
    }

    pub fn close(self) -> Result<()> {
        let Store { real_root, read_only, db, readers, writer_lock, poisoned, .. } = self;
        let readers = readers.into_inner().unwrap_or_else(|p| p.into_inner());
        for r in readers {
            let _ = r.close();
        }
        let conn = db.into_inner().unwrap_or_else(|p| p.into_inner());
        if !read_only && !poisoned.load(Ordering::SeqCst) {
            // Best effort, like SQLite's last-connection checkpoint: fold the
            // WAL into store.db so a cold open replays nothing. A reader in
            // another process may keep it from truncating; that is harmless.
            let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", (), |_| Ok(()));
        }
        conn.close().map_err(|e| sql(e, "close", NOT_WRITTEN))?;
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

fn new_store(root: &Path, real_root: PathBuf, read_only: bool, db: Connection, lock: Option<File>) -> Store {
    Store {
        root: root.to_path_buf(),
        real_root,
        read_only,
        db: Mutex::new(db),
        readers: Mutex::new(Vec::new()),
        writer_lock: lock,
        blob_gate: RwLock::new(()),
        gc_running: Mutex::new(()),
        poisoned: AtomicBool::new(false),
        seams: Seams::default(),
        dev_ports: Mutex::new(None),
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
        // A private file: nobody else knows its name, so no multi-process
        // coordination (and no `-tshm` file) is needed for it.
        let c = sql::open(&tdb, Mode::Private).map_err(|e| sql(e, "init", NOT_WRITTEN))?;
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
        c.execute_batch(&format!("BEGIN IMMEDIATE;\n{}\nCOMMIT;", ddl(LATEST))).map_err(|e| sql(e, "init", NOT_WRITTEN))?;
        let ck: Vec<i64> = c
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| Ok(vec![r.get(0)?, r.get(1)?, r.get(2)?]))
            .map_err(|e| sql(e, "init", NOT_WRITTEN))?;
        if ck[0] != 0 {
            return Err(err(
                Code::Unknown,
                "the new store's WAL could not be checkpointed",
                json!({ "op": "init", "outcome": NOT_WRITTEN, "checkpoint": ck }),
            ));
        }
        c.close().map_err(|e| sql(e, "init", NOT_WRITTEN))?;
    }
    // Everything is in the main file now; the empty sidecars of the temp
    // name must not outlive it.
    for suffix in ["-wal", "-shm", "-tshm"] {
        let side = PathBuf::from(format!("{}{suffix}", tdb.display()));
        match fs::remove_file(&side) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(ioe(&e, IoSide::Write, &side, "init")),
        }
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
    let real_root = retry_eintr(|| fs::canonicalize(root)).map_err(|e| ioe(&e, IoSide::Read, root, "open"))?;
    let rstore = real_root.join("store");
    let rdb = rstore.join("store.db");
    let c = open_conn(&rdb).map_err(|e| sql(e, "open", NOT_WRITTEN))?;
    c.busy_timeout(Duration::ZERO).map_err(|e| sql(e, "open", NOT_WRITTEN))?;
    let v = check_identity(&c, "open")?;
    pin_writer(&c, "open")?;
    check_schema(&c, "open", v)?;
    // The marker goes first: a migration that fails or dies below must
    // never leave a stale clean-shutdown claim behind.
    let marker = rstore.join("clean-shutdown");
    let mut checked = false;
    if exists(&marker).map_err(|e| ioe(&e, IoSide::Read, &marker, "open"))? {
        fs::remove_file(&marker).map_err(|e| {
            let e = errors::io(&e, IoSide::Write, &marker, "open", NOT_WRITTEN);
            StrataError { code: Code::Unknown, ..e }
        })?;
        full_sync_path(&rstore, "open", errors::UNCERTAIN)?;
    } else {
        quick_check_on(&c)?;
        checked = true;
    }
    if v < LATEST {
        if !checked {
            quick_check_on(&c)?;
        }
        migrate(&c, &rstore, &rdb, v)?;
    }
    Ok(new_store(root, real_root, false, c, Some(lock)))
}

const MIGRATE: &str = "migrate";
const CHECKPOINT_WAIT: Duration = Duration::from_secs(5);

/// store.md §4.4: take a store at `from` to `LATEST` in one transaction,
/// after a durable, verified backup of the `from` file. L1 is held, so no
/// other writer (or checkpointer) exists; read-only connections may.
fn migrate(c: &Connection, rstore: &Path, rdb: &Path, from: u32) -> Result<()> {
    let backup = backup(c, rstore, rdb, from)?;
    c.execute_batch("BEGIN IMMEDIATE").map_err(|e| sql(e, MIGRATE, NOT_WRITTEN))?;
    let applied = (|| -> Result<()> {
        for m in pending(from) {
            c.execute_batch(m.sql).map_err(|e| errors::with(sql(e, MIGRATE, NOT_WRITTEN), "migration", m.name))?;
        }
        // Validate inside the transaction (it sees its own schema), so
        // nothing that can fail as NOT_WRITTEN runs after COMMIT.
        let uv = pragma_text(c, "user_version").map_err(|e| sql(e, MIGRATE, NOT_WRITTEN))?;
        if uv != LATEST.to_string() {
            return Err(err(
                Code::Corrupt,
                format!("migration left user_version {uv}, not {LATEST}"),
                json!({ "reason": "user_version", "op": MIGRATE, "outcome": NOT_WRITTEN, "expected": LATEST, "actual": uv }),
            ));
        }
        check_schema(c, MIGRATE, LATEST)?;
        if seams::take_next_open(&Seam::MigrateFailsBeforeCommit) {
            return Err(err(
                Code::Unknown,
                "migration interrupted before COMMIT (test seam)",
                json!({ "op": MIGRATE, "outcome": NOT_WRITTEN, "seam": "migrate_fails_before_commit" }),
            ));
        }
        Ok(())
    })();
    if let Err(e) = applied {
        let _ = c.execute_batch("ROLLBACK");
        return Err(errors::with_default(e, "outcome", NOT_WRITTEN));
    }
    #[cfg(feature = "test-seams")]
    let fail = seams::take_next_open(&Seam::MigrateCommitSyncFails).then(|| {
        let v = crate::vfs::counters(c.path().unwrap_or_default());
        v.fail_next.store(true, Ordering::SeqCst);
        v
    });
    let res = c.execute_batch("COMMIT");
    #[cfg(feature = "test-seams")]
    if let Some(v) = fail {
        v.fail_next.store(false, Ordering::SeqCst);
    }
    if let Err(e) = res {
        let definite = matches!(&e, turso::Error::Constraint(_) | turso::Error::Busy(_) | turso::Error::BusySnapshot(_));
        let _ = c.execute_batch("ROLLBACK");
        if definite {
            return Err(sql(e, MIGRATE, NOT_WRITTEN));
        }
        // Like txn.rs commit_failed: whether the new schema reached the
        // disk is unknown. Only a fresh process re-runs WAL recovery
        // (store.md §5.2), so the caller must exit; its next open reads
        // user_version and migrates again or proceeds — both whole.
        let msg = e.to_string();
        return Err(err(
            Code::Unknown,
            format!("COMMIT of the schema migration {from} -> {LATEST} failed; whether it reached the disk is unknown (restart the process to recover): {msg}"),
            json!({ "op": MIGRATE, "outcome": errors::UNCERTAIN, "reason": "migration_commit", "from": from, "to": LATEST, "backup": backup.to_string_lossy(), "message": msg }),
        ));
    }
    if seams::take_next_open(&Seam::MigrateAbortAfterCommit) {
        std::process::abort();
    }
    // Committed: from here on the store is LATEST whatever happens; the
    // user_version written inside the transaction is what every open
    // trusts (check_identity reads the PRAGMA). Folding the WAL is best
    // effort and only tidies the file: if it fails, the header still says
    // `from` (an accepted value) until a later checkpoint.
    let _ = c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", (), |_| Ok(()));
    eprintln!("git-strata: store schema migrated {from} -> {LATEST}; a copy of the v{from} store is at {}", backup.display());
    Ok(())
}

/// Fold the WAL into store.db and clone it to `store/backups/`, durably,
/// then check the copy stands alone. Any failure: nothing migrated.
fn backup(c: &Connection, rstore: &Path, rdb: &Path, from: u32) -> Result<PathBuf> {
    const OP: &str = "migrate_backup";
    let wal = PathBuf::from(format!("{}-wal", rdb.display()));
    let deadline = Instant::now() + CHECKPOINT_WAIT;
    loop {
        let ck: Vec<sql::Value> = c
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| Ok(vec![r.get(0)?, r.get(1)?, r.get(2)?]))
            .map_err(|e| sql(e, OP, NOT_WRITTEN))?;
        let wal_len = match fs::metadata(&wal) {
            Ok(m) => m.len(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => 0,
            Err(e) => return Err(ioe(&e, IoSide::Read, &wal, OP)),
        };
        if ck.first() == Some(&sql::Value::Integer(0)) && wal_len == 0 {
            break;
        }
        if Instant::now() >= deadline {
            return Err(err(
                Code::Busy,
                "cannot fold the WAL before migrating (a reader holds it); nothing migrated, the store is unchanged — retry when git-strata readers finish",
                json!({ "reason": "migration_backup", "op": OP, "outcome": NOT_WRITTEN, "checkpoint": format!("{ck:?}"), "wal_bytes": wal_len }),
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let dir = rstore.join("backups");
    let made = match fs::create_dir(&dir) {
        Ok(()) => true,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => false,
        Err(e) => return Err(ioe(&e, IoSide::Write, &dir, OP)),
    };
    let ns = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let dst = dir.join(format!("store-v{from}-{ns}.db"));
    let done = (|| -> Result<()> {
        strata_apfs::clone_file(rdb, &dst).map_err(|e| errors::annotate(e, OP, NOT_WRITTEN))?;
        full_sync_path(&dst, OP, NOT_WRITTEN)?;
        full_sync_path(&dir, OP, NOT_WRITTEN)?;
        if made {
            full_sync_path(rstore, OP, NOT_WRITTEN)?;
        }
        verify_backup(&dst, from)
    })();
    if let Err(e) = done {
        let _ = fs::remove_file(&dst);
        return Err(e);
    }
    Ok(dst)
}

/// The copy, read without SQLite (so no sidecar is created next to it):
/// a strata header at `from`, and exactly `page_count` pages long.
fn verify_backup(p: &Path, from: u32) -> Result<()> {
    let bytes = fs::read(p).map_err(|e| ioe(&e, IoSide::Read, p, "migrate_backup"))?;
    let shown = p.to_string_lossy();
    let bad = |what: String| {
        err(
            Code::Unknown,
            format!("the pre-migration backup {shown} is not a whole v{from} store ({what}); nothing migrated"),
            json!({ "reason": "migration_backup", "op": "migrate_backup", "outcome": NOT_WRITTEN, "path": shown }),
        )
    };
    if bytes.len() < 100 || &bytes[..16] != MAGIC {
        return Err(bad("no SQLite header".into()));
    }
    let be = |o: usize| u32::from_be_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    if be(68) != APPLICATION_ID as u32 || be(60) != from {
        return Err(bad(format!("application_id {} user_version {}", be(68), be(60))));
    }
    let ps = match u16::from_be_bytes([bytes[16], bytes[17]]) {
        1 => 65536u64,
        n => n as u64,
    };
    let pages = be(28) as u64;
    if pages == 0 || ps * pages != bytes.len() as u64 {
        return Err(bad(format!("{pages} pages of {ps} bytes but {} bytes long", bytes.len())));
    }
    Ok(())
}

#[allow(dead_code)]
pub(crate) fn _corrupt_unused() -> StrataError {
    corrupt("x", "y")
}
