//! store.md §7, Task 8: busy, reader isolation and stale locks.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use rusqlite::{Connection, OpenFlags};
use strata_store::*;
use strata_types::Code;
use support::world::*;
use support::*;

fn envs(env: &Env) -> Vec<(&'static str, String)> {
    child_env(env)
}

fn spawn(test: &str, env: &Env) -> ChildProc {
    let e = envs(env);
    let refs: Vec<(&str, &str)> = e.iter().map(|(k, v)| (*k, v.as_str())).collect();
    spawn_child(test, &refs)
}

fn table_counts(env: &Env) -> (i64, i64, i64) {
    let c = Connection::open_with_flags(env.db(), OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let n = |t: &str| c.query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get::<_, i64>(0)).unwrap();
    (n("checkpoints"), n("overlay_entries"), n("blobs"))
}

/// Child: open a raw connection on store.db, hold `BEGIN IMMEDIATE` until
/// the parent says go.
#[test]
fn lock_child() {
    if !is_child() {
        return;
    }
    let root = env_path("STRATA_ROOT");
    let c = Connection::open_with_flags(root.join("store/store.db"), OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap();
    c.execute_batch("BEGIN IMMEDIATE").unwrap();
    say("LOCKED");
    wait_go();
    c.execute_batch("ROLLBACK").unwrap();
    say("RELEASED");
}

#[test]
fn external_writer_is_busy_without_side_effects() {
    let env = env();
    let s = env.open();
    world(&env, &s);
    let b = s.put_bytes(b"busy blob").unwrap();
    let cp = tool_cp("call-busy", vec![added("f", &b)]);
    let mut child = spawn("lock_child", &env);
    child.wait_for("LOCKED");
    let before = (table_counts(&env), snapshot(&env.root.join("sessions")));
    let t0 = Instant::now();
    let e = s.record_checkpoint(cp.clone()).unwrap_err();
    assert!(t0.elapsed() < Duration::from_secs(2), "Busy must not wait or retry: {:?}", t0.elapsed());
    assert_code(&e, Code::Busy);
    assert_eq!(detail(&e, "lock").and_then(|v| v.as_str()), Some("sqlite"));
    // other writes too
    assert_code(&s.set_session_phase(&sid(), "x").unwrap_err(), Code::Busy);
    assert_eq!((table_counts(&env), snapshot(&env.root.join("sessions"))), before);
    assert!(!s.is_poisoned(), "Busy is not an uncertain commit");
    child.go();
    child.wait_for("RELEASED");
    child.finish();
    // the same call now succeeds and gets the first id: nothing was consumed
    assert_eq!(u64::from(s.record_checkpoint(cp).unwrap()), 1);
    s.close().unwrap();
}

#[track_caller]
fn quick<T>(what: &str, f: impl FnOnce() -> T) -> T {
    let t0 = Instant::now();
    let v = f();
    let d = t0.elapsed();
    assert!(d < Duration::from_millis(100), "{what} took {d:?}");
    v
}

#[test]
fn readers_never_block_on_writer() {
    let env = env();
    let s = Arc::new(env.open());
    world(&env, &s);
    let b = s.put_bytes(b"one").unwrap();
    let first = s.record_checkpoint(tool_cp("c1", vec![added("f", &b)])).unwrap();
    // warm the reader pool (opening a connection is not what we time)
    s.checkpoints(&sid(), None).unwrap();
    let ro = Store::open_read_only(&env.root).unwrap();
    ro.checkpoints(&sid(), None).unwrap();

    // (1) an external process holds the SQLite write lock
    let mut child = spawn("lock_child", &env);
    child.wait_for("LOCKED");
    for st in [&*s, &ro] {
        let th = std::thread::scope(|sc| {
            sc.spawn(|| {
                quick("session", || st.session(&sid()).unwrap().unwrap());
                quick("checkpoint", || st.checkpoint(&sid(), first).unwrap());
                quick("checkpoints", || st.checkpoints(&sid(), None).unwrap().len())
            })
            .join()
            .unwrap()
        });
        assert_eq!(th, 1);
    }
    child.go();
    child.wait_for("RELEASED");
    child.finish();

    // (2) our own write transaction paused before COMMIT
    let b2 = s.put_bytes(b"two").unwrap();
    s.arm(Seam::PauseBeforeCommit);
    let w = {
        let s = s.clone();
        std::thread::spawn(move || s.record_checkpoint(tool_cp("c2", vec![added("g", &b2)])).unwrap())
    };
    s.wait_paused();
    for st in [&*s, &ro] {
        let (n, latest) = std::thread::scope(|sc| {
            sc.spawn(|| {
                quick("session", || st.session(&sid()).unwrap().unwrap());
                let n = quick("checkpoints", || st.checkpoints(&sid(), None).unwrap().len());
                let l = quick("latest", || st.latest_checkpoint(&sid(), &repo()).unwrap().unwrap());
                (n, l)
            })
            .join()
            .unwrap()
        });
        assert_eq!(n, 1, "readers see the state before the commit");
        assert_eq!(latest.id, first);
    }
    s.release();
    let second = w.join().unwrap();
    assert_eq!(u64::from(second), 2);
    assert_eq!(s.checkpoints(&sid(), None).unwrap().len(), 2);
    assert_eq!(ro.checkpoints(&sid(), None).unwrap().len(), 2);
    ro.close().unwrap();
    Arc::try_unwrap(s).ok().unwrap().close().unwrap();
}

/// Child: open the store as the writer, start a write that pauses before
/// COMMIT, report, and wait to be killed.
#[test]
fn holder_child() {
    if !is_child() {
        return;
    }
    let root = env_path("STRATA_ROOT");
    let checkout = env_path("STRATA_CHECKOUT");
    let s = Arc::new(Store::open(&root, &[&checkout]).unwrap());
    let b = s.put_bytes(b"from the killed child").unwrap();
    s.arm(Seam::PauseBeforeCommit);
    let w = s.clone();
    std::thread::spawn(move || {
        let _ = w.record_checkpoint(tool_cp("killed", vec![added("k", &b)]));
    });
    s.wait_paused();
    say("PAUSED");
    wait_go(); // never comes: the parent sends SIGKILL
    std::process::exit(3);
}

#[test]
fn killed_lock_holder_leaves_no_stale_lock() {
    let env = env();
    let s = env.open();
    world(&env, &s);
    s.close().unwrap();
    let mut child = spawn("holder_child", &env);
    child.wait_for("PAUSED");
    // while it lives, it is the writer
    assert_code(&env.try_open().err().unwrap(), Code::Busy);
    child.sigkill();
    let s = env.try_open().expect("open after the holder was killed");
    let b = s.put_bytes(b"parent").unwrap();
    let id = s.record_checkpoint(tool_cp("after", vec![added("p", &b)])).unwrap();
    assert_eq!(u64::from(id), 1, "the killed child's uncommitted checkpoint is gone");
    assert_eq!(s.checkpoints(&sid(), None).unwrap().len(), 1);
    s.close().unwrap();
}
