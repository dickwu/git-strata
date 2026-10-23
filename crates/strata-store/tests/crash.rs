//! store.md §7, Task 8: crash durability, commit fsync and poisoning.

mod support;

use std::fs;
use std::io::Write;

use rusqlite::{Connection, OpenFlags};
use strata_store::*;
use strata_types::{BlobId, Code, EntryKind};
use support::world::*;
use support::*;

fn spawn(test: &str, env: &Env) -> ChildProc {
    let e = child_env(env);
    let refs: Vec<(&str, &str)> = e.iter().map(|(k, v)| (*k, v.as_str())).collect();
    spawn_child(test, &refs)
}

fn blob_file(env: &Env, b: &BlobId) -> std::path::PathBuf {
    env.store_dir().join("blobs").join(&b.as_str()[..2]).join(b.as_str())
}

fn blob_row(env: &Env, b: &BlobId) -> bool {
    let c = Connection::open_with_flags(env.db(), OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    c.query_row("SELECT count(*) FROM blobs WHERE id = ?1", [b.as_str()], |r| r.get::<_, i64>(0)).unwrap() == 1
}

/// Child: put two blobs, record a checkpoint, acknowledge, wait for SIGKILL.
#[test]
fn crash_child() {
    if !is_child() {
        return;
    }
    let s = Store::open(&env_path("STRATA_ROOT"), &[&env_path("STRATA_CHECKOUT")]).unwrap();
    let a = s.put_bytes(b"crash blob a").unwrap();
    let b = s.put_bytes(b"crash blob b").unwrap();
    let id = s.record_checkpoint(tool_cp("crash", vec![added("a", &a), added("dir/b", &b)])).unwrap();
    say(&format!("ACK {} {} {}", u64::from(id), a.as_str(), b.as_str()));
    wait_go(); // never comes
    std::process::exit(3);
}

#[test]
fn checkpoint_write_is_durable_after_crash() {
    let env = env();
    let s = env.open();
    world(&env, &s);
    s.close().unwrap();
    let mut child = spawn("crash_child", &env);
    let ack = child.wait_for("ACK");
    child.sigkill();
    let parts: Vec<&str> = ack.split(' ').collect();
    let id: u64 = parts[1].parse().unwrap();
    let a = BlobId::try_from(parts[2].to_string()).unwrap();
    let b = BlobId::try_from(parts[3].to_string()).unwrap();
    assert!(!env.store_dir().join("clean-shutdown").exists());
    let s = env.open();
    let cp = s.checkpoint(&sid(), cid(id)).unwrap();
    assert_eq!(cp.entries.len(), 2);
    assert_eq!(cp.entries[0].kind, EntryKind::Added { blob: a.clone() });
    assert_eq!(cp.entries[1].kind, EntryKind::Added { blob: b.clone() });
    for (x, bytes) in [(&a, &b"crash blob a"[..]), (&b, &b"crash blob b"[..])] {
        assert!(blob_file(&env, x).exists());
        assert!(blob_row(&env, x));
        assert_eq!(s.read_blob(x, 100).unwrap(), bytes);
        s.verify_blob(x).unwrap();
    }
    // the replay of the acknowledged call returns the same id
    assert_eq!(u64::from(s.record_checkpoint(tool_cp("crash", vec![added("a", &a), added("dir/b", &b)])).unwrap()), id);
    let next = s.record_checkpoint(tool_cp("next", vec![deleted("x")])).unwrap();
    assert_eq!(u64::from(next), id + 1);
    s.close().unwrap();
}

#[test]
fn commit_issues_full_fsync() {
    let env = env();
    let s = env.open();
    world(&env, &s);
    let b = s.put_bytes(b"synced").unwrap();
    let mut last = s.sync_counts();
    for i in 0..3 {
        s.record_checkpoint(tool_cp(&format!("c{i}"), vec![added("f", &b)])).unwrap();
        let now = s.sync_counts();
        assert!(now.wal_full > last.wal_full, "commit {i} issued no SQLITE_SYNC_FULL xSync on the WAL: {now:?}");
        last = now;
    }
    assert_eq!(last.wal_normal, 0, "no WAL sync is ever NORMAL: {last:?}");
    s.close().unwrap();
}

#[test]
fn commit_io_error_is_unknown_and_poisons_writer() {
    let env = env();
    let s = env.open();
    world(&env, &s);
    let b = s.put_bytes(b"maybe").unwrap();
    let first = s.record_checkpoint(tool_cp("before", vec![added("f", &b)])).unwrap();
    s.arm(Seam::CommitSyncFails);
    let cp = tool_cp("uncertain", vec![added("g", &b)]);
    let e = s.record_checkpoint(cp.clone()).unwrap_err();
    assert_code(&e, Code::Unknown);
    assert_eq!(detail(&e, "outcome").and_then(|v| v.as_str()), Some("uncertain"), "{e:?}");
    assert!(s.is_poisoned());
    // every later write is refused as poisoned; nothing is retried
    assert_reason(&s.record_checkpoint(cp.clone()).unwrap_err(), Code::Unknown, "poisoned");
    assert_reason(&s.set_session_phase(&sid(), "x").unwrap_err(), Code::Unknown, "poisoned");
    assert_reason(&s.put_bytes(b"new").unwrap_err(), Code::Unknown, "poisoned");
    // reads work
    assert_eq!(s.checkpoint(&sid(), first).unwrap().entries.len(), 1);
    assert!(s.session(&sid()).unwrap().is_some());
    s.close().unwrap();
    assert!(!env.store_dir().join("clean-shutdown").exists(), "a poisoned writer leaves no clean-shutdown marker");
    let s = env.open();
    assert!(!s.is_poisoned());
    let before: Vec<u64> = s.checkpoints(&sid(), None).unwrap().iter().map(|c| u64::from(c.id)).collect();
    assert!(before == vec![1] || before == vec![1, 2], "{before:?}");
    // the replay either finds the commit or records it now; never twice
    let id = s.record_checkpoint(cp).unwrap();
    let after: Vec<u64> = s.checkpoints(&sid(), None).unwrap().iter().map(|c| u64::from(c.id)).collect();
    assert_eq!(after, vec![1, 2]);
    assert_eq!(u64::from(id), 2);
    s.close().unwrap();
}

#[test]
#[ignore = "needs an hdiutil disk image"]
fn disk_full_during_checkpoint_keeps_previous_state() {
    let img = Image::new("APFS");
    let root = img.mount.join("strata");
    let checkout = img.mount.join("checkout");
    fs::create_dir(&checkout).unwrap();
    let env = Env { dir: tempfile::tempdir().unwrap(), root: root.clone(), checkout: checkout.clone() };
    let s = env.open();
    world(&env, &s);
    let b = s.put_bytes(b"before the disk filled").unwrap();
    let first = s.record_checkpoint(tool_cp("first", vec![added("f", &b)])).unwrap();
    // fill the volume
    let filler = img.mount.join("filler");
    {
        let mut f = fs::File::create(&filler).unwrap();
        let big = vec![0x5au8; 1 << 20];
        while f.write_all(&big).is_ok() {}
        let small = vec![0x5au8; 4096];
        while f.write_all(&small).is_ok() {}
        let _ = f.sync_all();
    }
    let mut n = 0;
    while fs::write(img.mount.join(format!("pad{n}")), vec![1u8; 512]).is_ok() {
        n += 1;
    }
    // put fails before anything is published
    let big: Vec<u8> = (0..(4 << 20)).map(|i| (i % 251) as u8).collect();
    let e = s.put_bytes(&big).unwrap_err();
    assert_code(&e, Code::Unknown);
    assert_eq!(detail(&e, "outcome").and_then(|v| v.as_str()), Some("not_written"), "{e:?}");
    // a large checkpoint needs new pages
    let entries: Vec<_> = (0..4000).map(|i| deleted(&format!("some/long/directory/name/for/padding/file-{i:05}"))).collect();
    let e = s.record_checkpoint(tool_cp("full", entries)).unwrap_err();
    assert_code(&e, Code::Unknown);
    let outcome = detail(&e, "outcome").and_then(|v| v.as_str()).unwrap().to_string();
    assert!(outcome == "not_written" || outcome == "uncertain", "{e:?}");
    // the previous state reads back
    assert_eq!(s.checkpoint(&sid(), first).unwrap().entries.len(), 1);
    assert_eq!(s.read_blob(&b, 100).unwrap(), b"before the disk filled");
    // make room; a new checkpoint succeeds (on a fresh handle if poisoned)
    fs::remove_file(&filler).unwrap();
    let s = if s.is_poisoned() {
        let _ = s.close();
        env.open()
    } else {
        s
    };
    let b2 = s.put_bytes(b"after").unwrap();
    let id = s.record_checkpoint(tool_cp("after", vec![added("g", &b2)])).unwrap();
    assert!(u64::from(id) > u64::from(first));
    s.close().unwrap();
}
