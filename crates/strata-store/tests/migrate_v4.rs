//! v3 fixture written with the actual pre-v4 APIs; see fixtures/README.md.
mod support;
use strata_store::{PendingSource, Store};
use strata_types::ProposalId;
use support::world::*;
use support::*;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/store-v3.db");

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
fn v3_to_v4_migration_keeps_rows() {
    let env = fixture();
    assert_eq!(version(&env), 3);
    let ro = Store::open_read_only(&env.root).unwrap();
    let sessions = ro.sessions().unwrap();
    let proposals = ro.proposals(None, false).unwrap();
    let pending = ro.rebase_pending(None).unwrap();
    let activity = ro.move_activities(&sid()).unwrap();
    assert_eq!(
        (
            sessions.len(),
            proposals.len(),
            pending.len(),
            activity.len()
        ),
        (1, 1, 1, 1)
    );
    drop(ro);
    let s = env.open();
    assert_eq!(s.sessions().unwrap(), sessions);
    assert_eq!(s.proposals(None, false).unwrap(), proposals);
    assert_eq!(s.rebase_pending(None).unwrap(), pending);
    assert_eq!(s.move_activities(&sid()).unwrap(), activity);
    s.close().unwrap();
    assert_eq!(version(&env), 4);
    let fresh = support::env();
    fresh.open().close().unwrap();
    assert_eq!(
        schema(&env),
        schema(&fresh),
        "fresh/migrated DDL must be byte-identical"
    );
    let c = bare(&env.db());
    let integrity: String = c
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
}

#[test]
fn v4_readers_on_unmigrated_v3_store() {
    let env = fixture();
    let ro = Store::open_read_only(&env.root).unwrap();
    assert!(
        ro.deps_auto(ProposalId::try_from(1u64).unwrap())
            .unwrap()
            .is_none()
    );
    assert!(ro.deps_auto_all(None).unwrap().is_empty());
    assert!(ro.deps_auto_all(Some(&repo())).unwrap().is_empty());
    assert!(ro.repo_fetch(&repo()).unwrap().is_none());
    assert!(ro.sync_report(&sid(), &repo()).unwrap().is_empty());
    assert!(ro.proposals(None, false).unwrap()[0].from_head.is_none());
    assert!(
        ro.proposal(ProposalId::try_from(1u64).unwrap())
            .unwrap()
            .unwrap()
            .from_head
            .is_none()
    );
    let p = ro.rebase_pending_of(&sid(), &repo()).unwrap().unwrap();
    assert_eq!(p.source, PendingSource::Publish);
    assert!(p.to_deps.is_none());
    assert_eq!(version(&env), 3, "readers never migrate");
    let s = env.open();
    s.record_fetch(&repo(), true, None, t(100)).unwrap();
    assert_eq!(ro.repo_fetch(&repo()).unwrap().unwrap().ok_at, Some(t(100)));
    s.close().unwrap();
}

#[test]
fn rebase_pending_source_defaults_publish() {
    let env = fixture();
    let s = env.open();
    let row = s.rebase_pending_of(&sid(), &repo()).unwrap().unwrap();
    assert_eq!(row.source, PendingSource::Publish);
    assert!(row.to_deps.is_none());
    s.close().unwrap();
    let c = bare(&env.db());
    c.execute("DELETE FROM rebase_pending", []).unwrap();
    c.execute("INSERT INTO rebase_pending (session, repo, rev, from_tree, from_deps, reason, first_at_ns, last_at_ns, attempts) VALUES (?1, ?2, 0, ?3, ?4, 'planned', 1, 1, 0)",
        params![sid().as_str(), repo().as_str(), gen1().tree.as_str(), gen1().deps.as_str()]).unwrap();
    let source: String = c
        .query_row("SELECT source FROM rebase_pending", [], |r| r.get(0))
        .unwrap();
    assert_eq!(source, "publish");
}
