//! rm-followups §1.11: the v2 -> v3 migration (`rm_attempts`,
//! `rebase_pending`, `move_activity`) on a real v2 store
//! (`tests/fixtures/store-v2.db`, written by the v2 code at main 47aae23:
//! the v1 fixture migrated, plus session `s-agentabc` with agent `codex`
//! and a dev-port lease), and the reader guards that let a new CLI read a
//! v2 store an old daemon still serves.

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;
use strata_store::{Lifecycle, RmAttempt, RmOutcome, Store};
use strata_types::{Code, SessionId};
use support::world::*;
use support::*;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/store-v2.db");
const NEW_TABLES: [&str; 3] = ["move_activity", "rebase_pending", "rm_attempts"];

fn header_u32(db: &Path, off: usize) -> u32 {
    let b = fs::read(db).unwrap();
    u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if e.file_type().unwrap().is_dir() {
            copy_tree(&src, &dst);
        } else {
            fs::copy(&src, &dst).unwrap();
        }
    }
}

/// A store root laid out by the current code whose store.db is the v2
/// fixture (made under another root and copied, as in `migrate.rs`).
fn v2_env() -> Env {
    let env = env();
    let scratch = support::env();
    scratch.open().close().unwrap();
    copy_tree(&scratch.root, &env.root);
    for side in ["store.db", "store.db-wal", "store.db-shm", "store.db-tshm"] {
        let _ = fs::remove_file(env.store_dir().join(side));
    }
    fs::copy(FIXTURE, env.db()).unwrap();
    env
}

fn backups(env: &Env) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = match fs::read_dir(env.store_dir().join("backups")) {
        Ok(rd) => rd.map(|e| e.unwrap().path()).collect(),
        Err(_) => vec![],
    };
    v.sort();
    v
}

fn dump_copy(db: &Path) -> (u32, String, BTreeMap<String, Vec<String>>) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("copy.db");
    fs::copy(db, &p).unwrap();
    let c = strata_store::sql::open(&p, strata_store::sql::Mode::Private).unwrap();
    let uv: i64 = c.query_row("PRAGMA user_version", (), |r| r.get(0)).unwrap();
    let ic: String = c.query_row("PRAGMA integrity_check", (), |r| r.get(0)).unwrap();
    let mut out = BTreeMap::new();
    let names: Vec<String> = {
        let mut st = c.prepare("SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap();
        st.query_map((), |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap()
    };
    for t in names {
        let mut st = c.prepare(&format!("SELECT * FROM {t} ORDER BY rowid")).unwrap();
        let n = st.column_count().unwrap();
        let rows = st
            .query_map((), |r| {
                let mut cols = Vec::new();
                for i in 0..n {
                    let v: Value = r.get(i)?;
                    cols.push(format!("{v:?}"));
                }
                Ok(cols.join("|"))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        out.insert(t, rows);
    }
    (uv as u32, ic, out)
}

fn agentabc() -> SessionId {
    SessionId::try_from("s-agentabc".to_string()).unwrap()
}

fn attempt(at: u64, what: &str) -> RmAttempt {
    RmAttempt {
        session: agentabc(),
        at: t(at),
        outcome: RmOutcome::Refused,
        detail: json!({ "code": "Dirty", "message": what, "detail": { "removed": false } }),
    }
}

#[test]
fn fixture_is_a_v2_store() {
    let (uv, ic, rows) = dump_copy(Path::new(FIXTURE));
    assert_eq!((uv, ic.as_str()), (2, "ok"));
    assert_eq!(header_u32(Path::new(FIXTURE), 60), 2);
    assert_eq!(rows["sessions"].len(), 3);
    assert_eq!(rows["dev_ports"].len(), 1);
    for t in NEW_TABLES {
        assert!(!rows.contains_key(t), "{t} is v3");
    }
}

#[test]
fn v2_to_v3_migration_forward_only() {
    let env = v2_env();
    let (_, _, before) = dump_copy(&env.db());
    let s = env.open();
    assert_eq!(s.sessions().unwrap().len(), 3);
    assert!(s.rm_attempts(None).unwrap().is_empty());
    assert!(s.rebase_pending(None).unwrap().is_empty());
    s.close().unwrap();

    assert_eq!(header_u32(&env.db(), 60), 3);
    let (uv, ic, after) = dump_copy(&env.db());
    assert_eq!((uv, ic.as_str()), (3, "ok"));
    let mut keys: Vec<&str> = before.keys().map(String::as_str).collect();
    keys.extend(NEW_TABLES);
    keys.sort();
    assert_eq!(keys, after.keys().map(String::as_str).collect::<Vec<_>>());
    for (t, rows) in &before {
        assert_eq!(rows, &after[t], "{t} unchanged");
    }
    for t in NEW_TABLES {
        assert!(after[t].is_empty(), "{t}");
    }
    // one standalone v2 backup, byte-identical to the fixture
    let b = backups(&env);
    assert_eq!(b.len(), 1, "{b:?}");
    let name = b[0].file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.starts_with("store-v2-") && name.ends_with(".db"), "{name}");
    assert_eq!(fs::read(&b[0]).unwrap(), fs::read(FIXTURE).unwrap());

    // forward only: reopening is a no-op, and nothing goes back to v2
    let s = env.open();
    s.close().unwrap();
    assert_eq!(backups(&env), b, "no second backup");
    assert_eq!(header_u32(&env.db(), 60), 3);
}

#[test]
fn new_and_v2_migrated_stores_have_the_same_schema() {
    let fresh = env();
    fresh.open().close().unwrap();
    let migrated = v2_env();
    migrated.open().close().unwrap();
    let q = "SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name";
    let schema = |c: &Connection| -> Vec<String> {
        let mut st = c.prepare(q).unwrap();
        st.query_map((), |r| {
            let (a, b, t, s): (String, String, String, Option<String>) = (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
            Ok(format!("{a}|{b}|{t}|{s:?}"))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    };
    assert_eq!(schema(&bare(&fresh.db())), schema(&bare(&migrated.db())));
}

#[test]
fn new_cli_reads_v2_store_without_new_tables() {
    let env = v2_env();
    let ro = Store::open_read_only(&env.root).unwrap();
    // the old daemon has not migrated: the new tables are absent, the
    // readers show nothing rather than failing
    assert!(ro.rm_attempts(None).unwrap().is_empty());
    assert!(ro.rm_attempts(Some(&agentabc())).unwrap().is_empty());
    assert!(ro.rebase_pending(None).unwrap().is_empty());
    assert_eq!(ro.sessions().unwrap().len(), 3);
    // then the (new) daemon migrates under the reader and records an rm
    let s = env.open();
    assert!(s.record_rm_attempt(&attempt(40, "refused")).unwrap());
    let got = ro.rm_attempts(None).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].outcome, RmOutcome::Refused);
    assert_eq!(got[0].at, t(40));
    s.close().unwrap();
}

#[test]
fn open_rejects_newer_store_with_upgrade_hint() {
    let env = v2_env();
    env.open().close().unwrap();
    {
        let c = bare(&env.db());
        c.execute_batch("PRAGMA user_version = 4").unwrap();
        c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", (), |_| Ok(())).unwrap();
    }
    for e in [env.try_open().err().expect("v4 refused"), Store::open_read_only(&env.root).err().expect("v4 refused ro")] {
        assert_reason(&e, Code::Corrupt, "user_version");
        assert!(
            e.message.contains("this git-strata is older than the store (v4); install the matching git-strata"),
            "{}",
            e.message
        );
    }
}

#[test]
fn rm_attempt_is_replaced_then_dropped_with_the_session() {
    let env = v2_env();
    let s = env.open();
    let id = agentabc();
    assert!(s.record_rm_attempt(&attempt(40, "first")).unwrap());
    let mut later = attempt(50, "second");
    later.outcome = RmOutcome::Failed;
    assert!(s.record_rm_attempt(&later).unwrap());
    let got = s.rm_attempts(Some(&id)).unwrap();
    assert_eq!(got.len(), 1, "one row per session");
    assert_eq!((got[0].at, got[0].outcome), (t(50), RmOutcome::Failed));
    assert_eq!(got[0].detail["message"], "second");
    // not an object: refused
    let mut bad = attempt(60, "x");
    bad.detail = json!("text");
    assert_code(&s.record_rm_attempt(&bad).unwrap_err(), Code::InvalidInput);
    // Removed drops the row in the same transaction
    let lc = s.session(&id).unwrap().unwrap().lifecycle;
    if lc == Lifecycle::Creating {
        assert!(s.set_lifecycle(&id, Lifecycle::Creating, Lifecycle::Active, None, t(61)).unwrap());
    }
    assert!(s.set_lifecycle(&id, Lifecycle::Active, Lifecycle::Removing, None, t(62)).unwrap());
    assert!(s.set_lifecycle(&id, Lifecycle::Removing, Lifecycle::Removed, None, t(63)).unwrap());
    assert!(s.rm_attempts(None).unwrap().is_empty());
    // and a Removed (or unknown) session records nothing
    assert!(!s.record_rm_attempt(&attempt(70, "late")).unwrap());
    let ghost = RmAttempt { session: SessionId::try_from("s-zzzzzzzz".to_string()).unwrap(), ..attempt(70, "ghost") };
    assert!(!s.record_rm_attempt(&ghost).unwrap());
    assert!(s.rm_attempts(None).unwrap().is_empty());
    s.close().unwrap();
}
