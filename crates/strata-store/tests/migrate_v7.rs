//! The v7 migration adds only marking; checkpoint schema and rows stay intact.
mod support;

use strata_store::Store;
use strata_types::{CheckpointReason, MarkByKind, MarkState};
use support::world::*;
use support::*;

fn dump(c: &Connection, table: &str) -> Vec<String> {
    let mut q = c
        .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
        .unwrap();
    let columns = q.column_count().unwrap();
    q.query_map([], |r| {
        (0..columns)
            .map(|i| r.get::<_, Value>(i).map(|v| format!("{v:?}")))
            .collect::<Result<Vec<_>, _>>()
            .map(|v| v.join("|"))
    })
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

#[test]
fn v6_to_v7_preserves_checkpoints_and_adds_only_marks() {
    let env = env();
    let store = env.open();
    world(&env, &store);
    let blob = store.put_bytes(b"checkpoint content").unwrap();
    for reason in [
        CheckpointReason::ToolCall {
            id: "call-7".into(),
        },
        CheckpointReason::Manual {
            message: "before mark".into(),
        },
        CheckpointReason::BeforePublish,
        CheckpointReason::BeforeRestore,
        CheckpointReason::BeforeRollout,
        CheckpointReason::BeforeRemove,
    ] {
        let mut cp = tool_cp("unused", vec![added("file.txt", &blob)]);
        cp.reason = reason;
        store.record_checkpoint(cp).unwrap();
    }
    store.close().unwrap();
    // Strip only the v7 addition to obtain the exact v6 DDL with populated
    // checkpoint/overlay tables; no old migration is rewritten for the fixture.
    let before = {
        let c = bare(&env.db());
        c.execute_batch("PRAGMA foreign_keys=1;").unwrap();
        c.execute_batch("DROP TABLE session_marks; PRAGMA user_version = 6;")
            .unwrap();
        assert_eq!(
            c.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        let rows = (dump(&c, "checkpoints"), dump(&c, "overlay_entries"));
        assert_eq!(rows.0.len(), 6);
        assert_eq!(rows.1.len(), 6);
        rows
    };
    let ro = Store::open_read_only(&env.root).unwrap();
    assert!(ro.mark(&sid()).unwrap().is_none());
    drop(ro);
    let store = env.open();
    store
        .set_mark(
            &sid(),
            MarkState::Done,
            Some("ready"),
            MarkByKind::Agent,
            t(60),
        )
        .unwrap();
    store.close().unwrap();
    let c = bare(&env.db());
    c.execute_batch("PRAGMA foreign_keys=1;").unwrap();
    assert_eq!(
        c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        7
    );
    assert_eq!(
        c.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        (dump(&c, "checkpoints"), dump(&c, "overlay_entries")),
        before
    );
    assert_eq!(c.query_row("SELECT count(*) FROM sqlite_schema WHERE type='table' AND (name='session_exports' OR name='session_mark_events' OR name LIKE '%v7_copy%')", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM pragma_table_info('session_marks')",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        5
    );
    assert!(
        c.execute("UPDATE checkpoints SET reason='before_export'", [])
            .is_err()
    );
    assert!(c.execute("DELETE FROM checkpoints", []).is_err());
    assert!(
        c.execute("UPDATE session_marks SET state='archived'", [])
            .is_err()
    );
    assert!(
        c.execute("UPDATE session_marks SET note=?1", ["é".repeat(1001)])
            .is_err()
    );
    assert!(
        c.execute("UPDATE session_marks SET by_kind='daemon'", [])
            .is_err()
    );
    assert!(c.execute("INSERT INTO session_marks (session,state,by_kind,at_ns) VALUES ('s-missingx','done','cli',1)", []).is_err());
    assert!(
        c.query_row("PRAGMA foreign_key_check", [], |_| Ok(()))
            .optional()
            .unwrap()
            .is_none()
    );
    let backups: Vec<_> = std::fs::read_dir(env.store_dir().join("backups"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(
        bare(&backups[0])
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        6
    );
}
