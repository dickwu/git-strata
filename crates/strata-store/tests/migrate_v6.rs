//! v3 fixture written with the actual pre-v4 APIs; see fixtures/README.md.
mod support;
use strata_store::Store;
use support::world::*;
use support::*;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/store-v5.db");

fn copy_tree(src: &std::path::Path, dst: &std::path::Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap() {
        let e = e.unwrap();
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &dst.join(e.file_name()));
        } else {
            std::fs::copy(e.path(), dst.join(e.file_name())).unwrap();
        }
    }
}
fn fixture() -> Env {
    let dst = env();
    let src = env();
    src.open().close().unwrap();
    copy_tree(&src.root, &dst.root);
    for name in ["store.db", "store.db-wal", "store.db-shm", "store.db-tshm"] {
        let p = dst.store_dir().join(name);
        if p.exists() {
            std::fs::remove_file(p).unwrap();
        }
    }
    std::fs::copy(FIXTURE, dst.db()).unwrap();
    dst
}
fn version(env: &Env) -> i64 {
    bare(&env.db())
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap()
}
fn schema(env: &Env) -> Vec<(String, String, Option<String>)> {
    let c = bare(&env.db());
    let mut q = c
        .prepare("SELECT type, name, sql FROM sqlite_schema ORDER BY type, name")
        .unwrap();
    q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn v5_migrates_with_backup_and_exact_schema_and_v5_readers_work() {
    let env = fixture();
    assert_eq!(version(&env), 5);
    let ro = Store::open_read_only(&env.root).unwrap();
    assert!(ro.publishes().unwrap().is_empty());
    assert!(ro.move_activities(&sid()).unwrap().is_empty());
    drop(ro);
    env.open().close().unwrap();
    assert_eq!(version(&env), 7);
    let fresh = support::env();
    fresh.open().close().unwrap();
    assert_eq!(schema(&env), schema(&fresh));
    let backups: Vec<_> = std::fs::read_dir(env.store_dir().join("backups"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(backups.len(), 1);
    let c = bare(&backups[0]);
    assert_eq!(
        c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        5
    );
    assert_eq!(
        c.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
}
