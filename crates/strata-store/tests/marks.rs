mod support;

use std::time::Duration;

use strata_store::{Lifecycle, Seam};
use strata_types::{Code, MarkByKind, MarkState, SessionId};
use support::world::*;
use support::*;

#[test]
fn done_round_trips_replaces_metadata_and_active_clears_it() {
    let env = env();
    let store = env.open();
    world(&env, &store);
    let before = store.session(&sid()).unwrap().unwrap();
    assert!(store.mark(&sid()).unwrap().is_none());
    let mark = store
        .set_mark(
            &sid(),
            MarkState::Done,
            Some("landed\tall\nready"),
            MarkByKind::Agent,
            t(20),
        )
        .unwrap()
        .unwrap();
    assert_eq!(mark, store.mark(&sid()).unwrap().unwrap());
    assert_eq!(mark.note.as_deref(), Some("landed\tall\nready"));
    assert_eq!(mark.by.kind, MarkByKind::Agent);
    assert_eq!(mark.at, t(20));
    let after = store.session(&sid()).unwrap().unwrap();
    assert_eq!(after.manifest, before.manifest);
    assert_eq!(after.lifecycle, Lifecycle::Active);
    store.close().unwrap();

    let store = env.open();
    assert_eq!(store.mark(&sid()).unwrap(), Some(mark));
    let mark = store
        .set_mark(&sid(), MarkState::Done, None, MarkByKind::Cli, t(21))
        .unwrap()
        .unwrap();
    assert!(mark.note.is_none());
    assert_eq!(mark.by.kind, MarkByKind::Cli);
    assert_eq!(mark.at, t(21));
    for _ in 0..2 {
        assert!(
            store
                .set_mark(&sid(), MarkState::Active, None, MarkByKind::Agent, t(22))
                .unwrap()
                .is_none()
        );
        assert!(store.mark(&sid()).unwrap().is_none());
    }
    store.close().unwrap();
}

#[test]
fn a_daemon_mark_round_trips_and_is_replaced_like_any_other() {
    let env = env();
    let store = env.open();
    world(&env, &store);
    let mark = store
        .set_mark(
            &sid(),
            MarkState::Done,
            Some("auto-close did not close this session: unpublished"),
            MarkByKind::Daemon,
            t(20),
        )
        .unwrap()
        .unwrap();
    assert_eq!(mark.by.kind, MarkByKind::Daemon);
    assert_eq!(store.mark(&sid()).unwrap(), Some(mark));
    store.close().unwrap();

    let store = env.open();
    assert_eq!(
        store.mark(&sid()).unwrap().unwrap().by.kind,
        MarkByKind::Daemon
    );
    assert!(
        store
            .set_mark(&sid(), MarkState::Active, None, MarkByKind::Daemon, t(21))
            .unwrap()
            .is_none()
    );
    assert!(store.mark(&sid()).unwrap().is_none());
    store.close().unwrap();
}

#[test]
fn note_validation_uses_bytes_and_allows_only_tab_and_newline_controls() {
    let env = env();
    let store = env.open();
    world(&env, &store);
    for note in ["a".repeat(2000), "é".repeat(1000), "\t\n".into()] {
        store
            .set_mark(&sid(), MarkState::Done, Some(&note), MarkByKind::Cli, t(20))
            .unwrap();
    }
    let before = store.mark(&sid()).unwrap();
    let mut invalid = vec![String::new(), "a".repeat(2001), "é".repeat(1001)];
    for b in 0u8..0x20 {
        if b != b'\t' && b != b'\n' {
            invalid.push(format!("before{}after", char::from(b)));
        }
    }
    for note in invalid {
        let e = store
            .set_mark(&sid(), MarkState::Done, Some(&note), MarkByKind::Cli, t(21))
            .unwrap_err();
        assert_reason(&e, Code::InvalidInput, "note");
        assert_eq!(store.mark(&sid()).unwrap(), before);
    }
    let e = store
        .set_mark(
            &sid(),
            MarkState::Active,
            Some("note"),
            MarkByKind::Cli,
            t(21),
        )
        .unwrap_err();
    assert_reason(&e, Code::InvalidInput, "note");
    store.close().unwrap();
}

#[test]
fn creating_sessions_and_raw_sql_cannot_bypass_lifecycle_guards() {
    let env = env();
    let store = env.open();
    world(&env, &store);
    let mut manifest = store.session(&sid()).unwrap().unwrap().manifest;
    manifest.id = SessionId::try_from("s-creating".to_string()).unwrap();
    manifest.root = env.root.join("sessions").join(manifest.id.as_str());
    for (repo, ws) in &mut manifest.repos {
        ws.path = manifest.root.join(repo.as_str());
    }
    store.create_session(&manifest, t(12)).unwrap();
    for state in [MarkState::Done, MarkState::Active] {
        assert_reason(
            &store
                .set_mark(&manifest.id, state, None, MarkByKind::Cli, t(20))
                .unwrap_err(),
            Code::InvalidInput,
            "lifecycle",
        );
    }
    store
        .set_mark(&sid(), MarkState::Done, None, MarkByKind::Cli, t(20))
        .unwrap();
    let (c, _) = raw(&env.db(), false);
    assert!(
        c.execute(
            "INSERT INTO session_marks (session,state,by_kind,at_ns) VALUES (?1,'done','cli',1)",
            [manifest.id.as_str()],
        )
        .is_err()
    );
    assert!(
        c.execute(
            "UPDATE session_marks SET session = ?1 WHERE session = ?2",
            params![manifest.id.as_str(), sid().as_str()],
        )
        .is_err()
    );
    store
        .set_lifecycle(&sid(), Lifecycle::Active, Lifecycle::Removing, None, t(21))
        .unwrap();
    assert!(
        c.execute(
            "UPDATE session_marks SET note='changed' WHERE session=?1",
            [sid().as_str()]
        )
        .is_err()
    );
    assert!(
        c.execute(
            "DELETE FROM session_marks WHERE session=?1",
            [sid().as_str()]
        )
        .is_err()
    );
    drop(c);
    store.close().unwrap();
}

#[test]
fn lifecycle_guards_apply_to_mark_and_undo_and_gc_cleans_marks() {
    let env = env();
    let store = env.open();
    world(&env, &store);
    let missing = SessionId::try_from("s-missingx".to_string()).unwrap();
    assert_eq!(
        store
            .set_mark(&missing, MarkState::Done, None, MarkByKind::Cli, t(20))
            .unwrap_err()
            .code,
        Code::NoSuchSession
    );
    store
        .set_mark(&sid(), MarkState::Done, None, MarkByKind::Cli, t(20))
        .unwrap();
    store
        .set_lifecycle(&sid(), Lifecycle::Active, Lifecycle::Removing, None, t(21))
        .unwrap();
    for state in [MarkState::Done, MarkState::Active] {
        assert_reason(
            &store
                .set_mark(&sid(), state, None, MarkByKind::Cli, t(22))
                .unwrap_err(),
            Code::InvalidInput,
            "lifecycle",
        );
    }
    store
        .set_lifecycle(
            &sid(),
            Lifecycle::Removing,
            Lifecycle::Unknown,
            Some(&serde_json::json!({"test":true})),
            t(23),
        )
        .unwrap();
    store
        .set_mark(&sid(), MarkState::Active, None, MarkByKind::Cli, t(24))
        .unwrap();
    store
        .set_mark(&sid(), MarkState::Done, None, MarkByKind::Gui, t(25))
        .unwrap();
    store
        .set_lifecycle(&sid(), Lifecycle::Unknown, Lifecycle::Removing, None, t(26))
        .unwrap();
    store
        .set_lifecycle(&sid(), Lifecycle::Removing, Lifecycle::Removed, None, t(27))
        .unwrap();
    for state in [MarkState::Done, MarkState::Active] {
        assert_reason(
            &store
                .set_mark(&sid(), state, None, MarkByKind::Cli, t(28))
                .unwrap_err(),
            Code::InvalidInput,
            "lifecycle",
        );
    }
    // Frozen removal records remain readable until the session's normal GC.
    assert!(store.mark(&sid()).unwrap().is_some());
    store.arm(Seam::AgeFloor { secs: 0 });
    let result = store.gc(Duration::ZERO).unwrap();
    assert_eq!(result.sessions, 1);
    assert!(store.mark(&sid()).unwrap().is_none());
    store.close().unwrap();
}
