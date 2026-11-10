//! A v7 store written by turso =0.9.0-pre.1 (the 0.2.1 engine) opens on the
//! current engine with its schema check passing and its rows intact.
//! The fixture comes from fixtures/generate_v7_fixture.rs.txt; see
//! fixtures/README.md.
mod support;
use strata_store::Store;
use strata_types::{MarkByKind, MarkState};
use support::world::*;
use support::*;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/store-v7-turso-pre1.db"
);

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
fn pre1_v7_store_opens_with_rows_and_schema_intact() {
    let env = fixture();
    assert_eq!(version(&env), 7);
    let fresh = support::env();
    fresh.open().close().unwrap();
    assert_eq!(schema(&env), schema(&fresh));

    let ro = Store::open_read_only(&env.root).unwrap();
    let mark = ro.mark(&sid()).unwrap().expect("the fixture's mark");
    assert_eq!(mark.state, MarkState::Done);
    assert_eq!(mark.note.as_deref(), Some("written by turso pre.1"));
    assert_eq!(ro.proposals(Some(&repo()), false).unwrap().len(), 1);
    drop(ro);

    let store = env.open();
    store
        .set_mark(
            &sid(),
            MarkState::Done,
            Some("rewritten by pre.5"),
            MarkByKind::Gui,
            t(30),
        )
        .unwrap();
    store.close().unwrap();
    assert_eq!(version(&env), 7);
    assert!(!env.store_dir().join("backups").exists());
    assert_eq!(schema(&env), schema(&fresh));
    let c = bare(&env.db());
    assert_eq!(
        c.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    drop(c);
    let ro = Store::open_read_only(&env.root).unwrap();
    let mark = ro.mark(&sid()).unwrap().expect("the rewritten mark");
    assert_eq!(mark.note.as_deref(), Some("rewritten by pre.5"));
    assert_eq!(mark.by.kind, MarkByKind::Gui);
}
