//! store.md §4.4: forward-only schema migrations, v1 -> v2
//! (`sessions.agent`) -> v3 (rm-followups: `rm_attempts`, `rebase_pending`,
//! `move_activity`), run by the writer open on a real v1 store
//! (`tests/fixtures/store-v1.db`, written by the v1 code at main 31769e3).
//! The v2 -> v3 step from a real v2 store is in `migrate_v3.rs`.

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use strata_store::{DevPortsPolicy, Seam, Store};
use strata_types::{AgentName, Code, CommitOid, SessionId, SessionManifest, WorkspaceState};
use support::world::*;
use support::*;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/store-v1.db");
const ROOT_ENV: &str = "STRATA_MIGRATE_TEST_ROOT";

fn header_u32(db: &Path, off: usize) -> u32 {
    let b = fs::read(db).unwrap();
    u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

/// A store root laid out by the current code (dirs, LOCK, marker) whose
/// store.db is the v1 fixture. The layout is made under another root and
/// copied, so this process never had a Turso database at the fixture's path.
fn v1_env() -> Env {
    let env = env();
    let scratch = support::env();
    scratch.open().close().unwrap();
    copy_tree(&scratch.root, &env.root);
    for side in ["store.db", "store.db-wal", "store.db-shm", "store.db-tshm"] {
        let _ = fs::remove_file(env.store_dir().join(side));
    }
    fs::copy(FIXTURE, env.db()).unwrap();
    assert!(env.store_dir().join("clean-shutdown").exists());
    env
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

fn backups(env: &Env) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = match fs::read_dir(env.store_dir().join("backups")) {
        Ok(rd) => rd.map(|e| e.unwrap().path()).collect(),
        Err(_) => vec![],
    };
    v.sort();
    v
}

/// Every table's rows (as debug text), from a private copy of `db`, so the
/// original never gains sidecars.
fn dump_copy(db: &Path) -> (u32, String, BTreeMap<String, Vec<String>>) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("copy.db");
    fs::copy(db, &p).unwrap();
    let c = strata_store::sql::open(&p, strata_store::sql::Mode::Private).unwrap();
    let uv: i64 = c.query_row("PRAGMA user_version", (), |r| r.get(0)).unwrap();
    let ic: String = c.query_row("PRAGMA integrity_check", (), |r| r.get(0)).unwrap();
    (uv as u32, ic, dump_all(&c))
}

fn tables(c: &Connection) -> Vec<String> {
    let mut st = c.prepare("SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap();
    st.query_map((), |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap()
}

fn dump_all(c: &Connection) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    for t in tables(c) {
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
    out
}

fn schema(c: &Connection) -> Vec<String> {
    let mut st = c.prepare("SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name").unwrap();
    st.query_map((), |r| {
        let (a, b, t, s): (String, String, String, Option<String>) = (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
        Ok(format!("{a}|{b}|{t}|{s:?}"))
    })
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

/// A new session on the fixture's repo, recorded by `agent`.
fn manifest(root: &Path, agent: &str) -> SessionManifest {
    let id = SessionId::try_from("s-agentabc".to_string()).unwrap();
    let sroot = root.join("sessions/s-agentabc");
    let mut repos = BTreeMap::new();
    repos.insert(repo(), WorkspaceState { base: gen1(), head: CommitOid::try_from("3".repeat(40)).unwrap(), path: sroot.join("app"), tracked: false });
    SessionManifest { id, ticket: None, agent: Some(AgentName::try_from(agent.to_string()).unwrap()), root: sroot, repos }
}

fn ticketed() -> SessionId {
    SessionId::try_from("s-ticketed".to_string()).unwrap()
}

#[test]
fn fixture_is_a_v1_store() {
    let (uv, ic, rows) = dump_copy(Path::new(FIXTURE));
    assert_eq!((uv, ic.as_str()), (1, "ok"));
    assert_eq!(header_u32(Path::new(FIXTURE), 60), 1);
    assert_eq!(rows["sessions"].len(), 2);
    assert!(rows["sessions"].iter().all(|r| r.split('|').count() == 8), "v1 sessions have 8 columns");
}

/// The tables v2 and v3 add, all empty after migrating a v1 store.
const NEW_TABLES: [&str; 4] = ["dev_ports", "move_activity", "rebase_pending", "rm_attempts"];

#[test]
fn writer_open_migrates_v1_and_keeps_every_row() {
    let env = v1_env();
    let (_, _, before) = dump_copy(&env.db());
    let s = env.open();
    // the store API reads the new column; v1 sessions have no agent
    let all = s.sessions().unwrap();
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|r| r.manifest.agent.is_none()));
    let t = s.session(&ticketed()).unwrap().unwrap();
    assert_eq!(t.manifest.ticket.as_deref(), Some("GWD-42"));
    s.close().unwrap();

    assert_eq!(header_u32(&env.db(), 60), 3);
    let (uv, ic, after) = dump_copy(&env.db());
    assert_eq!((uv, ic.as_str()), (3, "ok"));
    // v2 adds `dev_ports` (v1 sessions get leases lazily), v3 the three
    // rm-followups tables: all empty
    let mut keys: Vec<&str> = before.keys().map(String::as_str).collect();
    keys.extend(NEW_TABLES);
    keys.sort();
    assert_eq!(keys, after.keys().map(String::as_str).collect::<Vec<_>>());
    for t in NEW_TABLES {
        assert!(after[t].is_empty(), "{t}");
    }
    for (t, rows) in &before {
        if t == "sessions" {
            let widened: Vec<String> = rows.iter().map(|r| format!("{r}|Null")).collect();
            assert_eq!(&widened, &after[t], "sessions rows gain agent = NULL and nothing else");
        } else {
            assert_eq!(rows, &after[t], "{t} unchanged");
        }
    }

    // one durable, standalone v1 backup, byte-identical to the fixture
    let b = backups(&env);
    assert_eq!(b.len(), 1, "{b:?}");
    let name = b[0].file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.starts_with("store-v1-") && name.ends_with(".db"), "{name}");
    assert_eq!(fs::read(&b[0]).unwrap(), fs::read(FIXTURE).unwrap());
    let (buv, bic, brows) = dump_copy(&b[0]);
    assert_eq!((buv, bic.as_str()), (1, "ok"));
    assert_eq!(brows, before);
    assert_eq!(listing(&env.store_dir().join("backups")).len(), 1, "no sidecars beside the backup");
}

#[test]
fn reopen_after_migration_is_a_no_op() {
    let env = v1_env();
    env.open().close().unwrap();
    let first = backups(&env);
    let (_, _, rows) = dump_copy(&env.db());
    for _ in 0..2 {
        let s = env.open();
        assert_eq!(s.sessions().unwrap().len(), 2);
        s.close().unwrap();
    }
    assert_eq!(backups(&env), first, "no second backup");
    assert_eq!(dump_copy(&env.db()).2, rows);
}

#[test]
fn new_and_migrated_stores_have_the_same_schema() {
    let fresh = env();
    fresh.open().close().unwrap();
    let migrated = v1_env();
    migrated.open().close().unwrap();
    let (a, b) = (bare(&fresh.db()), bare(&migrated.db()));
    assert_eq!(schema(&a), schema(&b));
    let uv: i64 = a.query_row("PRAGMA user_version", (), |r| r.get(0)).unwrap();
    assert_eq!(uv, 3);
}

#[test]
fn agent_round_trips_after_migration() {
    let env = v1_env();
    let s = env.open();
    let m = manifest(&env.root, "codex");
    let id = m.id.clone();
    s.create_session(&m, t(30)).unwrap();
    assert_eq!(s.session(&id).unwrap().unwrap().manifest.agent.as_ref().map(|a| a.as_str()), Some("codex"));
    s.close().unwrap();
    let s = env.open();
    assert_eq!(s.session(&id).unwrap().unwrap().manifest.agent.as_ref().map(|a| a.as_str()), Some("codex"));
    s.close().unwrap();
}

#[test]
fn newer_schema_is_refused_untouched() {
    let env = v1_env();
    env.open().close().unwrap();
    {
        let c = bare(&env.db());
        c.execute_batch("PRAGMA user_version = 4").unwrap();
        c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", (), |_| Ok(())).unwrap();
    }
    let before = snapshot(&env.root);
    let e = env.try_open().err().expect("v4 refused");
    assert_reason(&e, Code::Corrupt, "user_version");
    assert!(e.message.contains("older than the store (v4)"), "{}", e.message);
    let e = Store::open_read_only(&env.root).err().expect("v4 refused read-only");
    assert_reason(&e, Code::Corrupt, "user_version");
    let mut after = snapshot(&env.root);
    let mut before = before;
    for m in [&mut before, &mut after] {
        m.retain(|p, _| !p.to_string_lossy().ends_with("-shm") && !p.to_string_lossy().ends_with("-tshm"));
    }
    assert_eq!(before, after);
    assert_eq!(backups(&env).len(), 1, "the v1 one only");
}

#[test]
fn failure_before_commit_leaves_v1_and_the_next_open_migrates() {
    let env = v1_env();
    let (_, _, rows) = dump_copy(&env.db());
    Store::arm_next_open(Seam::MigrateFailsBeforeCommit);
    let e = env.try_open().err().expect("seam fails the migration");
    assert_eq!(detail(&e, "outcome").and_then(|v| v.as_str()), Some("not_written"), "{e:?}");
    assert_eq!(header_u32(&env.db(), 60), 1);
    let (uv, ic, now) = dump_copy(&env.db());
    assert_eq!((uv, ic.as_str()), (1, "ok"));
    assert_eq!(now, rows);
    let s = env.open();
    assert_eq!(s.sessions().unwrap().len(), 2);
    s.close().unwrap();
    assert_eq!(header_u32(&env.db(), 60), 3);
}

#[test]
fn reader_holding_the_wal_makes_the_migration_wait_then_refuse() {
    let env = v1_env();
    // put frames in the WAL, then pin them with an open read transaction
    let w = bare(&env.db());
    w.execute_batch("UPDATE strata_gc_gate SET active = 0").unwrap();
    let frames = fs::metadata(env.store_dir().join("store.db-wal")).map(|m| m.len()).unwrap_or(0);
    assert!(frames > 0, "the WAL has frames to fold");
    let r = bare(&env.db());
    r.execute_batch("BEGIN").unwrap();
    let _: i64 = r.query_row("SELECT count(*) FROM sessions", (), |x| x.get(0)).unwrap();
    let e = env.try_open().err().expect("checkpoint cannot finish");
    assert_reason(&e, Code::Busy, "migration_backup");
    assert!(backups(&env).is_empty(), "{:?}", backups(&env));
    let uv: i64 = w.query_row("PRAGMA user_version", (), |x| x.get(0)).unwrap();
    assert_eq!(uv, 1);
    r.execute_batch("ROLLBACK").unwrap();
    drop((r, w));
    let s = env.open();
    assert_eq!(s.session(&ticketed()).unwrap().unwrap().manifest.ticket.as_deref(), Some("GWD-42"));
    s.close().unwrap();
    assert_eq!(backups(&env).len(), 1);
}

#[test]
fn read_only_handle_survives_an_in_process_migration() {
    let env = v1_env();
    let ro = Store::open_read_only(&env.root).unwrap();
    assert!(ro.sessions().unwrap().iter().all(|r| r.manifest.agent.is_none()));
    // v1 has no dev_ports table: every workspace reads as unleased, not Corrupt
    assert_eq!(ro.dev_port_leases(None).unwrap(), None);
    let s = env.open(); // migrates under the reader
    s.set_dev_ports_policy(Some(DevPortsPolicy { blocks: vec![20000], port_ok: std::sync::Arc::new(|_| true) }));
    let m = manifest(&env.root, "claude-code");
    let id = m.id.clone();
    s.create_session(&m, t(30)).unwrap();
    let got = ro.session(&id).unwrap().unwrap();
    assert_eq!(got.manifest.agent.map(String::from).as_deref(), Some("claude-code"));
    assert_eq!(ro.sessions().unwrap().len(), 3);
    let leases = ro.dev_port_leases(None).unwrap().unwrap();
    assert_eq!(leases.iter().map(|l| (l.session.as_str(), l.base)).collect::<Vec<_>>(), vec![(id.as_str(), 20000)]);
    s.close().unwrap();
}

// ------------------------------------------------- separate-process cases

fn child_root() -> Option<PathBuf> {
    is_child().then(|| PathBuf::from(std::env::var(ROOT_ENV).unwrap()))
}

#[test]
fn child_migrates_and_records_an_agent() {
    let Some(root) = child_root() else { return };
    let s = Store::open(&root, &[]).unwrap();
    let m = manifest(&root, "cursor-agent");
    s.create_session(&m, t(30)).unwrap();
    s.close().unwrap();
    println!("CHILD-OK");
}

#[test]
fn read_only_handle_survives_another_process_migrating() {
    let env = v1_env();
    let ro = Store::open_read_only(&env.root).unwrap();
    assert_eq!(ro.sessions().unwrap().len(), 2);
    let out = run_child("child_migrates_and_records_an_agent", &[(ROOT_ENV, env.root.to_str().unwrap())]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("CHILD-OK"), "{out:?}");
    let all = ro.sessions().unwrap();
    assert_eq!(all.len(), 3);
    let a: Vec<_> = all.iter().filter_map(|r| r.manifest.agent.as_ref().map(|a| a.as_str().to_string())).collect();
    assert_eq!(a, vec!["cursor-agent".to_string()]);
    // and a brand-new read-only open after the migration
    let ro2 = Store::open_read_only(&env.root).unwrap();
    assert_eq!(ro2.sessions().unwrap().len(), 3);
}

#[test]
fn child_migration_commit_sync_fails() {
    let Some(root) = child_root() else { return };
    Store::arm_next_open(Seam::MigrateCommitSyncFails);
    let e = Store::open(&root, &[]).err().expect("COMMIT fails");
    println!("CODE={:?} REASON={:?} OUTCOME={:?}", e.code, reason(&e), detail(&e, "outcome").and_then(|v| v.as_str()));
}

#[test]
fn uncertain_commit_is_resolved_by_a_fresh_process() {
    let env = v1_env();
    let (_, _, rows) = dump_copy(&env.db());
    let out = run_child("child_migration_commit_sync_fails", &[(ROOT_ENV, env.root.to_str().unwrap())]);
    let so = String::from_utf8_lossy(&out.stdout);
    assert!(so.contains(r#"CODE=Unknown REASON=Some("migration_commit") OUTCOME=Some("uncertain")"#), "{so}\n{}", String::from_utf8_lossy(&out.stderr));
    // this process is the "fresh process": whatever reached the disk, it opens whole at v3
    let s = env.open();
    assert_eq!(s.sessions().unwrap().len(), 2);
    s.close().unwrap();
    let (uv, ic, now) = dump_copy(&env.db());
    assert_eq!((uv, ic.as_str()), (3, "ok"));
    for (t, r) in &rows {
        if t != "sessions" {
            assert_eq!(r, &now[t], "{t}");
        }
    }
    assert!(!backups(&env).is_empty());
}

#[test]
fn child_aborts_after_migration_commit() {
    let Some(root) = child_root() else { return };
    Store::arm_next_open(Seam::MigrateAbortAfterCommit);
    let _ = Store::open(&root, &[]);
    unreachable!("the seam aborts the process");
}

#[test]
fn crash_right_after_commit_keeps_v3() {
    let env = v1_env();
    let out = run_child("child_aborts_after_migration_commit", &[(ROOT_ENV, env.root.to_str().unwrap())]);
    assert!(!out.status.success());
    let s = env.open();
    assert_eq!(s.sessions().unwrap().len(), 2);
    s.close().unwrap();
    let (uv, ic, _) = dump_copy(&env.db());
    assert_eq!((uv, ic.as_str()), (3, "ok"));
    assert_eq!(backups(&env).len(), 1, "committed; no second migration");
}
