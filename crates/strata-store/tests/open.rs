//! store.md §7, Task 1: open, layout, pinned schema.

mod support;

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use strata_store::{AttemptKind, AttemptState, DepsMain, DepsOrigin, NewProposal, ProposalDecision, Seam, Store};
use strata_types::Code;
use support::*;

fn header_u32(db: &std::path::Path, off: usize) -> u32 {
    let b = fs::read(db).unwrap();
    u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn without_volatile(s: std::collections::BTreeMap<PathBuf, Snap>) -> std::collections::BTreeMap<PathBuf, Snap> {
    s.into_iter()
        .filter(|(p, _)| {
            let n = p.to_string_lossy();
            !n.ends_with("store.db-wal") && !n.ends_with("store.db-shm") && !n.ends_with("clean-shutdown")
        })
        .collect()
}

#[test]
fn open_creates_layout_on_empty_root() {
    for pre_create in [false, true] {
        let env = env();
        if pre_create {
            fs::create_dir(&env.root).unwrap();
        }
        let s = env.open();
        let blobs = env.store_dir().join("blobs");
        let fan: Vec<_> = fs::read_dir(&blobs).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(fan.len(), 256);
        for i in 0..=255u8 {
            assert!(blobs.join(format!("{i:02x}")).is_dir());
        }
        for d in ["store/tmp", "store/trash", "bases", "deps", "sessions"] {
            assert!(env.root.join(d).is_dir(), "{d}");
        }
        assert!(env.store_dir().join("LOCK").is_file());
        let db = env.db();
        assert_eq!(header_u32(&db, 68), 1398035009);
        assert_eq!(header_u32(&db, 60), 1);
        let b = fs::read(&db).unwrap();
        assert_eq!((b[18], b[19]), (2, 2));
        assert!(!s.is_read_only());
        s.close().unwrap();
        assert!(env.store_dir().join("clean-shutdown").exists());
        let before = snapshot(&env.root);
        let s = env.open();
        assert!(!env.store_dir().join("clean-shutdown").exists());
        let after = snapshot(&env.root);
        assert_eq!(without_volatile(before), without_volatile(after));
        s.close().unwrap();
    }
}

#[test]
fn interrupted_creation_is_resumed() {
    let env = env();
    Store::arm_next_open(Seam::InitBeforeRename);
    let e = env.try_open().err().expect("seam aborts init");
    assert_code(&e, Code::Unknown);
    // only empty dirs plus the temp db in store/tmp
    assert!(!env.db().exists());
    let tmp: Vec<String> = fs::read_dir(env.store_dir().join("tmp"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(tmp.iter().any(|n| n.starts_with("init-") && n.ends_with(".db")), "{tmp:?}");
    for (p, k) in snapshot(&env.root) {
        let n = p.to_string_lossy();
        if n.starts_with("store/tmp/") || n == "store/LOCK" {
            continue;
        }
        assert_eq!(k, Snap::Dir, "{n} should be an empty dir");
    }
    let s = env.open();
    assert!(env.db().exists());
    s.close().unwrap();
}

#[test]
fn open_refuses_nonempty_foreign_root() {
    let env = env();
    fs::create_dir(&env.root).unwrap();
    fs::write(env.root.join("notes.txt"), b"mine").unwrap();
    let before = snapshot(&env.root);
    let e = env.try_open().err().unwrap();
    assert_code(&e, Code::NotEmpty);
    let entries = detail(&e, "entries").unwrap().as_array().unwrap();
    assert!(entries.iter().any(|v| v == "notes.txt"));
    assert_eq!(snapshot(&env.root), before);
}

#[test]
fn open_refuses_lost_store_db() {
    let env = env();
    env.open().close().unwrap();
    fs::create_dir_all(env.root.join("sessions/s-abcdefgh/repo")).unwrap();
    fs::remove_file(env.db()).unwrap();
    let e = env.try_open().err().unwrap();
    assert_reason(&e, Code::Corrupt, "store_db_missing");
    assert!(!env.db().exists());
}

#[test]
#[ignore = "needs an hdiutil disk image"]
fn open_refuses_cross_volume_root() {
    let img = Image::new("APFS");
    let env = env();
    let root = img.mount.join("strata");
    let before_img = snapshot(&img.mount);
    let before_tmp = snapshot(env.dir.path());
    let e = Store::open(&root, &[&env.checkout]).err().unwrap();
    assert_code(&e, Code::CrossVolume);
    assert_eq!(snapshot(&img.mount), before_img);
    assert_eq!(snapshot(env.dir.path()), before_tmp);
}

#[test]
#[ignore = "needs an hdiutil disk image"]
fn open_refuses_volume_without_clonefile() {
    let img = Image::new("MS-DOS FAT32");
    let checkout = img.mount.join("checkout");
    fs::create_dir(&checkout).unwrap();
    let root = img.mount.join("strata");
    let before = listing(&img.mount);
    let e = Store::open(&root, &[&checkout]).err().unwrap();
    assert_code(&e, Code::Unsupported);
    assert_eq!(listing(&img.mount), before);
    assert!(!root.exists());
}

#[test]
fn open_pins_durable_pragmas() {
    let env = env();
    let s = env.open();
    for (name, want) in [
        ("journal_mode", "wal"),
        ("synchronous", "2"),
        ("fullfsync", "1"),
        ("checkpoint_fullfsync", "1"),
        ("foreign_keys", "1"),
        ("recursive_triggers", "1"),
        ("trusted_schema", "0"),
        ("busy_timeout", "0"),
    ] {
        assert_eq!(s.debug_pragma(true, name).unwrap().to_lowercase(), want, "writer {name}");
    }
    for (name, want) in [("query_only", "1"), ("trusted_schema", "0")] {
        assert_eq!(s.debug_pragma(false, name).unwrap(), want, "reader {name}");
    }
    s.close().unwrap();
    let r = Store::open_read_only(&env.root).unwrap();
    assert_eq!(r.debug_pragma(false, "query_only").unwrap(), "1");
    assert_eq!(r.debug_pragma(false, "trusted_schema").unwrap(), "0");
    r.close().unwrap();
}

#[test]
fn open_refuses_non_wal_store() {
    let env = env();
    env.open().close().unwrap();
    {
        let c = bare(&env.db());
        let m: String = c.query_row("PRAGMA journal_mode = delete", [], |r| r.get(0)).unwrap();
        assert_eq!(m, "delete");
    }
    let before = fs::read(env.db()).unwrap();
    let e = env.try_open().err().unwrap();
    assert_reason(&e, Code::Corrupt, "journal_mode");
    assert_eq!(fs::read(env.db()).unwrap(), before);
    let c = bare(&env.db());
    let m: String = c.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
    assert_eq!(m, "delete");
}

#[test]
fn open_refuses_schema_drift() {
    let edits: [(&str, &str); 4] = [
        ("drop table", "DROP TABLE gc_runs;"),
        (
            "drop check",
            "PRAGMA writable_schema = ON; UPDATE sqlite_schema SET sql = replace(sql, 'CHECK (size >= 0),', ',') WHERE name = 'blobs'; PRAGMA writable_schema = OFF;",
        ),
        (
            "change type",
            "PRAGMA writable_schema = ON; UPDATE sqlite_schema SET sql = replace(sql, 'ticket     TEXT,', 'ticket     ANY,') WHERE name = 'sessions'; PRAGMA writable_schema = OFF;",
        ),
        ("drop trigger", "DROP TRIGGER blobs_no_update;"),
    ];
    for (what, edit) in edits {
        let env = env();
        env.open().close().unwrap();
        {
            let (c, _) = raw(&env.db(), true);
            c.execute_batch(edit).unwrap_or_else(|e| panic!("{what}: {e}"));
            let changed: i64 = c.query_row("SELECT count(*) FROM sqlite_schema", [], |r| r.get(0)).unwrap();
            assert!(changed > 0);
        }
        let before = fs::read(env.db()).unwrap();
        let e = env.try_open().err().unwrap_or_else(|| panic!("{what}: open succeeded"));
        assert_reason(&e, Code::Corrupt, "schema");
        assert_eq!(fs::read(env.db()).unwrap(), before, "{what}: bytes changed");
        let e = Store::open_read_only(&env.root).err().unwrap();
        assert_reason(&e, Code::Corrupt, "schema");
    }
}

#[test]
fn open_refuses_unknown_user_version() {
    for v in [0, 2] {
        let env = env();
        env.open().close().unwrap();
        {
            let c = bare(&env.db());
            c.execute_batch(&format!("PRAGMA user_version = {v}")).unwrap();
        }
        let before = fs::read(env.db()).unwrap();
        let e = env.try_open().err().unwrap();
        assert_reason(&e, Code::Corrupt, "user_version");
        assert_eq!(fs::read(env.db()).unwrap(), before);
        assert_eq!(header_u32(&env.db(), 60), v);
    }
}

#[test]
fn open_leaves_unrelated_database_untouched() {
    let env = env();
    fs::create_dir_all(env.store_dir()).unwrap();
    {
        let c = rusqlite::Connection::open(env.db()).unwrap();
        c.execute_batch("CREATE TABLE t (x INTEGER); INSERT INTO t VALUES (1);").unwrap();
    }
    let before = fs::read(env.db()).unwrap();
    let e = env.try_open().err().unwrap();
    assert_reason(&e, Code::Corrupt, "application_id");
    assert_eq!(fs::read(env.db()).unwrap(), before);
    assert!(!env.store_dir().join("store.db-wal").exists());
    assert!(!env.store_dir().join("store.db-shm").exists());
    let e = Store::open_read_only(&env.root).err().unwrap();
    assert_reason(&e, Code::Corrupt, "application_id");
    assert!(!env.store_dir().join("store.db-wal").exists());
    let c = bare(&env.db());
    let m: String = c.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
    assert_eq!(m, "delete");
}

#[test]
fn second_writer_open_is_busy() {
    let env = env();
    let s = env.open();
    let before = snapshot(&env.store_dir());
    let e = env.try_open().err().unwrap();
    assert_code(&e, Code::Busy);
    assert_eq!(detail(&e, "lock").and_then(|v| v.as_str()), Some("store-writer"));
    assert_eq!(snapshot(&env.store_dir()), before);
    let out = run_child(
        "second_writer_child",
        &[("STRATA_ROOT", env.root.to_str().unwrap()), ("STRATA_CHECKOUT", env.checkout.to_str().unwrap())],
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "child failed: {text}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(text.contains("CHILD_BUSY_OK"), "{text}");
    let r = Store::open_read_only(&env.root).unwrap();
    assert!(r.is_read_only());
    r.close().unwrap();
    s.close().unwrap();
}

#[test]
fn second_writer_child() {
    if !is_child() {
        return;
    }
    let root = PathBuf::from(std::env::var("STRATA_ROOT").unwrap());
    let checkout = PathBuf::from(std::env::var("STRATA_CHECKOUT").unwrap());
    let e = Store::open(&root, &[&checkout]).err().expect("second writer must fail");
    assert_code(&e, Code::Busy);
    assert_eq!(detail(&e, "lock").and_then(|v| v.as_str()), Some("store-writer"));
    let r = Store::open_read_only(&root).expect("read-only open");
    r.close().unwrap();
    println!("CHILD_BUSY_OK");
}

#[test]
fn read_only_open_refuses_every_write() {
    let env = env();
    let w = env.open();
    let id = w.put_bytes(b"readable").unwrap();
    let repo = strata_types::RepoName::try_from("app".to_string()).unwrap();
    let set = |c: char| strata_types::DepsSetId::try_from(c.to_string().repeat(64)).unwrap();
    let tree = strata_types::TreeOid::try_from("1".repeat(40)).unwrap();
    let head = strata_types::CommitOid::try_from("2".repeat(40)).unwrap();
    let at = std::time::SystemTime::now();
    let attempt = w.begin_deps_attempt(&repo, &set('a'), AttemptKind::Build, at).unwrap();
    let proposal = NewProposal {
        repo: repo.clone(),
        origin: None,
        from: set('a'),
        from_inputs: "a".repeat(64),
        to: set('b'),
        to_inputs: "b".repeat(64),
        tree: Some(tree.clone()),
        head: Some(head.clone()),
    };
    let pid = w.record_proposal(&proposal, None, at).unwrap().unwrap();
    let main = DepsMain { repo: repo.clone(), tree: tree.clone(), head: head.clone(), inputs_hash: "a".repeat(64), set: set('a'), observed: at };
    w.set_deps_main(&main, None).unwrap();
    w.close().unwrap();
    let src = env.dir.path().join("ro-src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("f"), b"would be new").unwrap();
    let r = Store::open_read_only(&env.root).unwrap();
    let before = snapshot(&env.root);
    assert!(r.is_read_only());
    // read functions work
    r.quick_check().unwrap();
    assert_eq!(r.read_blob(&id, 100).unwrap(), b"readable");
    r.verify_blob(&id).unwrap();
    r.get_blob(&id, &src.join("out")).unwrap();
    assert_eq!(r.deps_attempts(None).unwrap().len(), 1);
    assert_eq!(r.proposals(Some(&repo), true).unwrap().len(), 1);
    assert!(r.proposal(pid).unwrap().is_some());
    assert_eq!(r.deps_main(&repo).unwrap(), Some(main.clone()));
    assert!(r.deps_set(&repo, &set('a')).unwrap().is_none());
    // write functions are refused, one block per task
    let f = strata_types::RepoPath::try_from("f".to_string()).unwrap();
    for e in [
        r.put_blob(&src.join("f")).err().unwrap(),
        r.put_blob_rooted(&src, &f).err().unwrap(),
        r.put_bytes(b"would be new").err().unwrap(),
        // Task 3
        r.begin_deps_attempt(&repo, &set('c'), AttemptKind::Adopt, at).err().unwrap(),
        r.set_deps_attempt_pid(attempt, 42).err().unwrap(),
        r.set_deps_attempt_pid(attempt, 0).err().unwrap(),
        r.end_deps_attempt(attempt, AttemptState::Failed, &strata_types::StrataError { code: Code::Unknown, message: "x".into(), detail: None }, at)
            .err()
            .unwrap(),
        r.record_deps_set(
            &strata_types::DepsSet {
                id: set('a'),
                repo: repo.clone(),
                inputs_hash: "a".repeat(64),
                platform: "p".into(),
                toolchain: "t".into(),
                dirs: vec![],
            },
            DepsOrigin::Empty,
            None,
            at,
        )
        .err()
        .unwrap(),
        r.record_proposal(&proposal, Some(pid), at).err().unwrap(),
        r.promote_proposal(pid, &tree, &head, at).err().unwrap(),
        r.decide_proposal(pid, ProposalDecision::Applied, at).err().unwrap(),
        r.set_deps_main(&main, Some(&main)).err().unwrap(),
    ] {
        assert_reason(&e, Code::ReadOnly, "read_only_handle");
    }
    assert_eq!(snapshot(&env.root), before);
    r.close().unwrap();
    // read-only close leaves the clean-shutdown marker alone
    assert!(env.store_dir().join("clean-shutdown").exists());
}

#[test]
fn quick_check_runs_only_after_unclean_exit() {
    let env = env();
    env.open().close().unwrap();
    {
        let (c, _) = raw(&env.db(), false);
        c.execute_batch("BEGIN").unwrap();
        for i in 0..2000 {
            c.execute(
                "INSERT INTO blobs (id, size, created_ns) VALUES (?1, ?2, 0)",
                rusqlite::params![format!("{:064x}", i), i],
            )
            .unwrap();
        }
        c.execute_batch("COMMIT").unwrap();
    }
    // the store's own close checkpoints everything into the main file
    env.open().close().unwrap();
    let root_page: i64 = {
        let c = bare(&env.db());
        c.query_row("SELECT rootpage FROM sqlite_schema WHERE name = 'blobs'", [], |r| r.get(0)).unwrap()
    };
    assert!(!env.store_dir().join("store.db-wal").exists() || fs::metadata(env.store_dir().join("store.db-wal")).unwrap().len() == 0);
    let mut bytes = fs::read(env.db()).unwrap();
    let page = 4096usize;
    let off = (root_page as usize - 1) * page;
    bytes[off] = 0x00; // invalid b-tree page type
    bytes[off + 1] = 0xff;
    fs::write(env.db(), &bytes).unwrap();

    // marker present: open succeeds without quick_check and removes it
    assert!(env.store_dir().join("clean-shutdown").exists());
    // read-only open neither reads nor touches the marker
    let ro = Store::open_read_only(&env.root).unwrap();
    ro.close().unwrap();
    assert!(env.store_dir().join("clean-shutdown").exists());
    let s = env.open();
    assert!(!env.store_dir().join("clean-shutdown").exists());
    let e = s.quick_check().err().expect("quick_check finds the damage");
    assert_reason(&e, Code::Corrupt, "sqlite");
    drop(s); // no close: no marker
    assert!(!env.store_dir().join("clean-shutdown").exists());
    let e = env.try_open().err().expect("unclean reopen runs quick_check");
    assert_reason(&e, Code::Corrupt, "sqlite");
}

#[test]
fn lock_is_not_inherited_by_child_processes() {
    let env = env();
    let s = env.open();
    let mut child = Command::new("sleep").arg("60").spawn().unwrap();
    drop(s);
    let s2 = env.try_open();
    let alive = child.try_wait().unwrap().is_none();
    let _ = child.kill();
    let _ = child.wait();
    assert!(alive, "the sleeper should still be running");
    s2.expect("open after the writer is dropped").close().unwrap();
}
