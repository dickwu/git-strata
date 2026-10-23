//! Engine verification for the Turso metadata layer (docs/impl-progress/
//! store.md, "Engine: Turso"): the SQLite behaviours the design relies on,
//! checked on this Mac against the real engine. Crash durability and
//! cross-process writer Busy are in crash.rs / concurrency.rs; schema drift,
//! user_version and corrupt pages are in open.rs.

mod support;

use std::time::{Duration, Instant};

use strata_store::*;
use strata_types::Code;
use support::world::*;
use support::*;

fn spawn(test: &str, env: &Env) -> ChildProc {
    let e = child_env(env);
    let refs: Vec<(&str, &str)> = e.iter().map(|(k, v)| (*k, v.as_str())).collect();
    spawn_child(test, &refs)
}

fn count(c: &Connection, t: &str) -> i64 {
    c.query_row(&format!("SELECT count(*) FROM {t}"), (), |r| r.get(0)).unwrap()
}

#[test]
fn store_file_is_wal_sqlite_format() {
    let env = env();
    let s = env.open();
    let b = std::fs::read(env.db()).unwrap();
    assert_eq!(&b[..16], b"SQLite format 3\0");
    assert_eq!((b[18], b[19]), (2, 2), "WAL file format bytes");
    assert_eq!(s.debug_pragma(true, "journal_mode").unwrap(), "wal");
    assert_eq!(s.debug_pragma(false, "journal_mode").unwrap(), "wal");
    s.close().unwrap();
    // the clean close folds the WAL back into store.db
    let wal = env.store_dir().join("store.db-wal");
    assert!(!wal.exists() || std::fs::metadata(&wal).unwrap().len() == 0);
}

#[test]
fn integrity_check_is_clean_after_writes() {
    let env = env();
    let s = env.open();
    world(&env, &s);
    let b = s.put_bytes(b"integrity").unwrap();
    s.record_checkpoint(tool_cp("ic", vec![added("f", &b)])).unwrap();
    s.quick_check().unwrap();
    s.close().unwrap();
    let c = ro(&env.db());
    let r: String = c.query_row("PRAGMA integrity_check", (), |r| r.get(0)).unwrap();
    assert_eq!(r, "ok");
}

/// Child: a CLI-style read-only open while the daemon holds the writer;
/// prints what it sees, then (raw) inserts a row for the daemon to see.
#[test]
fn cli_child() {
    if !is_child() {
        return;
    }
    let root = env_path("STRATA_ROOT");
    let r = Store::open_read_only(&root).unwrap();
    let n = r.checkpoints(&sid(), None).unwrap().len();
    say(&format!("SEEN {n}"));
    wait_go();
    let n = r.checkpoints(&sid(), None).unwrap().len();
    say(&format!("SEEN2 {n}"));
    r.close().unwrap();
    // a raw writer from another process (the daemon holds no SQL lock
    // between transactions)
    let c = connect(&root.join("store/store.db"));
    c.execute("INSERT INTO blobs (id, size, created_ns) VALUES (?1, 1, 0)", [&"e".repeat(64)]).unwrap();
    say("WROTE");
}

#[test]
fn daemon_and_cli_processes_share_the_store() {
    let env = env();
    let s = env.open();
    world(&env, &s);
    let b = s.put_bytes(b"cross process").unwrap();
    s.record_checkpoint(tool_cp("x1", vec![added("f", &b)])).unwrap();
    let mut child = spawn("cli_child", &env);
    assert_eq!(child.wait_for("SEEN"), "SEEN 1");
    // a commit made while the other process has the file open is visible
    s.record_checkpoint(tool_cp("x2", vec![added("g", &b)])).unwrap();
    child.go();
    assert_eq!(child.wait_for("SEEN2"), "SEEN2 2");
    child.wait_for("WROTE");
    child.finish();
    // and the other process's commit is visible here
    let c = ro(&env.db());
    let n: i64 = c.query_row("SELECT count(*) FROM blobs WHERE id = ?1", [&"e".repeat(64)], |r| r.get(0)).unwrap();
    assert_eq!(n, 1);
    s.close().unwrap();
}

/// Child: hold BEGIN IMMEDIATE on a raw connection until told to go.
#[test]
fn immediate_child() {
    if !is_child() {
        return;
    }
    let c = connect(&env_path("STRATA_ROOT").join("store/store.db"));
    c.execute_batch("BEGIN IMMEDIATE").unwrap();
    c.execute("INSERT INTO blobs (id, size, created_ns) VALUES (?1, 1, 0)", [&"f".repeat(64)]).unwrap();
    say("LOCKED");
    wait_go();
    c.execute_batch("COMMIT").unwrap();
    say("DONE");
}

#[test]
fn cross_process_begin_immediate_is_busy_and_isolated() {
    let env = env();
    env.open().close().unwrap();
    let mut child = spawn("immediate_child", &env);
    child.wait_for("LOCKED");
    let c = connect(&env.db());
    c.busy_timeout(Duration::ZERO).unwrap();
    let t0 = Instant::now();
    let e = c.execute_batch("BEGIN IMMEDIATE").unwrap_err();
    assert!(matches!(e, strata_store::sql::Error::Busy(_) | strata_store::sql::Error::BusySnapshot(_)), "{e:?}");
    assert!(t0.elapsed() < Duration::from_secs(1));
    assert!(c.is_autocommit());
    // readers are not blocked and do not see the uncommitted row
    assert_eq!(count(&c, "blobs"), 0);
    child.go();
    child.wait_for("DONE");
    child.finish();
    assert_eq!(count(&c, "blobs"), 1);
    c.execute_batch("BEGIN IMMEDIATE; ROLLBACK").unwrap();
}

/// txn.rs treats a COMMIT that fails on a constraint as definite: the
/// engine must report it as Constraint and keep the transaction open.
#[test]
fn deferred_foreign_key_fails_at_commit_and_keeps_tx() {
    let dir = tempfile::tempdir().unwrap();
    let c = strata_store::sql::open(&dir.path().join("fk.db"), strata_store::sql::Mode::Private).unwrap();
    c.execute_batch(
        "PRAGMA foreign_keys = 1;
         CREATE TABLE p (id INTEGER PRIMARY KEY) STRICT;
         CREATE TABLE ch (id INTEGER PRIMARY KEY, p INTEGER REFERENCES p (id) DEFERRABLE INITIALLY DEFERRED) STRICT;",
    )
    .unwrap();
    c.execute_batch("BEGIN IMMEDIATE; INSERT INTO ch VALUES (1, 7);").unwrap();
    let e = c.execute_batch("COMMIT").unwrap_err();
    assert!(matches!(e, strata_store::sql::Error::Constraint(_)), "{e:?}");
    assert!(!c.is_autocommit(), "the failed COMMIT must leave the tx open");
    c.execute_batch("INSERT INTO p VALUES (7); COMMIT;").unwrap();
    assert!(c.is_autocommit());
    assert_eq!(count(&c, "ch"), 1);
}

#[test]
fn gc_gate_controls_deletes_and_is_itself_guarded() {
    let env = env();
    let s = env.open();
    s.close().unwrap();
    let (c, gate) = raw(&env.db(), false);
    c.execute("INSERT INTO blobs (id, size, created_ns) VALUES (?1, 1, 0)", [&"d".repeat(64)]).unwrap();
    assert_eq!(count(&c, "blobs"), 1);
    let del = "DELETE FROM blobs";
    let e = c.execute_batch(del).unwrap_err().to_string();
    assert!(e.contains("delete outside gc: blobs"), "{e}");
    for (q, m) in [
        ("INSERT INTO strata_gc_gate VALUES (2, 1)", "illegal insert: strata_gc_gate"),
        ("DELETE FROM strata_gc_gate", "append-only: strata_gc_gate"),
    ] {
        let e = c.execute_batch(q).unwrap_err().to_string();
        assert!(e.contains(m), "{q}: {e}");
    }
    assert!(c.execute_batch("UPDATE strata_gc_gate SET active = 2").is_err());
    // the gate is a transaction-scoped switch for the gc's own tx
    c.execute_batch("BEGIN IMMEDIATE").unwrap();
    c.execute_batch("UPDATE strata_gc_gate SET active = 1").unwrap();
    c.execute_batch(del).unwrap();
    c.execute_batch("UPDATE strata_gc_gate SET active = 0").unwrap();
    c.execute_batch("ROLLBACK").unwrap();
    assert_eq!(count(&c, "blobs"), 1);
    let active: i64 = c.query_row("SELECT active FROM strata_gc_gate", (), |r| r.get(0)).unwrap();
    assert_eq!(active, 0);
    gate.store(true, std::sync::atomic::Ordering::SeqCst);
    c.execute_batch(del).unwrap();
    gate.store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(count(&c, "blobs"), 0);
}

#[test]
fn reader_connections_refuse_writes() {
    let env = env();
    let s = env.open();
    s.close().unwrap();
    let r = Store::open_read_only(&env.root).unwrap();
    assert_eq!(r.debug_pragma(false, "query_only").unwrap(), "1");
    r.close().unwrap();
    let c = ro(&env.db());
    let e = c.execute_batch("INSERT INTO blobs (id, size, created_ns) VALUES ('a', 1, 0)").unwrap_err();
    assert!(!matches!(e, strata_store::sql::Error::Busy(_)), "{e:?}");
    assert_eq!(count(&c, "blobs"), 0);
}

#[test]
fn store_rejects_writes_through_read_only_handle() {
    let env = env();
    env.open().close().unwrap();
    let r = Store::open_read_only(&env.root).unwrap();
    let e = r.put_bytes(b"nope").unwrap_err();
    assert_code(&e, Code::ReadOnly);
    r.close().unwrap();
}
