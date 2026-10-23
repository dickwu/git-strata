//! store.md §7, Task 6: checkpoint records.

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use strata_store::*;
use strata_types::{
    BaseGen, BaseManifest, BlobId, Checkpoint, CheckpointId, CheckpointReason, Code, CommitOid, DepsSet, DepsSetId,
    EntryKind, OverlayEntry, RepoName, RepoPath, SessionId, SessionManifest, TreeOid, WorkspaceState,
};
use support::*;

fn repo(s: &str) -> RepoName {
    RepoName::try_from(s.to_string()).unwrap()
}
fn set_id(c: char) -> DepsSetId {
    DepsSetId::try_from(c.to_string().repeat(64)).unwrap()
}
fn tree(c: char) -> TreeOid {
    TreeOid::try_from(c.to_string().repeat(40)).unwrap()
}
fn head(c: char) -> CommitOid {
    CommitOid::try_from(c.to_string().repeat(40)).unwrap()
}
fn sess(s: &str) -> SessionId {
    SessionId::try_from(format!("s-{s}")).unwrap()
}
fn rp(s: &str) -> RepoPath {
    RepoPath::try_from(s.to_string()).unwrap()
}
fn t(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_nanos(secs * 1_000_000_000 + 7)
}
fn g(r: &str, tr: char) -> BaseGen {
    BaseGen { repo: repo(r), tree: tree(tr), deps: set_id('a') }
}
fn cid(n: u64) -> CheckpointId {
    CheckpointId::try_from(n).unwrap()
}

const REPOS: [&str; 3] = ["app", "lib", "web"];

/// Store with deps 'a' and generations <repo>/1/a and app/2/a.
fn setup() -> (Env, Store) {
    let env = env();
    let s = env.open();
    for r in REPOS {
        let set = DepsSet {
            id: set_id('a'),
            repo: repo(r),
            inputs_hash: "0".repeat(64),
            platform: "darwin-arm64".into(),
            toolchain: "none".into(),
            dirs: vec![],
        };
        s.record_deps_set(&set, DepsOrigin::Empty, None, t(1)).unwrap();
    }
    for b in [g("app", '1'), g("app", '2'), g("lib", '1'), g("web", '1')] {
        let m = BaseManifest { tree: b.tree.clone(), entries: BTreeMap::new() };
        s.record_base_gen(&b, &m, t(1)).unwrap();
    }
    (env, s)
}

/// An active session: app and lib tracked, web untracked.
fn active(env: &Env, s: &Store, id: &SessionId) {
    let root = env.root.join("sessions").join(id.as_str());
    let mut repos = BTreeMap::new();
    for (r, tracked) in [("app", true), ("lib", true), ("web", false)] {
        repos.insert(repo(r), WorkspaceState { base: g(r, '1'), head: head('1'), path: root.join(r), tracked });
    }
    s.create_session(&SessionManifest { id: id.clone(), ticket: None, root, repos }, t(10)).unwrap();
    assert!(s.set_lifecycle(id, Lifecycle::Creating, Lifecycle::Active, None, t(11)).unwrap());
}

fn cp(id: &SessionId, r: &str, reason: CheckpointReason, entries: Vec<OverlayEntry>) -> Checkpoint {
    Checkpoint { id: cid(999), session: id.clone(), repo: repo(r), base: g(r, '1'), at: t(50), reason, entries }
}
fn manual() -> CheckpointReason {
    CheckpointReason::Manual { message: "m".into() }
}
fn added(p: &str, b: &BlobId) -> OverlayEntry {
    OverlayEntry { path: rp(p), kind: EntryKind::Added { blob: b.clone() }, mode: 0o100644 }
}
fn deleted(p: &str) -> OverlayEntry {
    OverlayEntry { path: rp(p), kind: EntryKind::Deleted, mode: 0 }
}
fn sorted(mut c: Checkpoint, id: u64) -> Checkpoint {
    c.id = cid(id);
    c.entries.sort_by(|a, b| a.path.cmp(&b.path));
    c
}

/// (checkpoints, overlay_entries, blobs) row counts.
fn counts(env: &Env) -> (i64, i64, i64) {
    let (c, _) = raw(&env.db(), false);
    let n = |t: &str| c.query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get::<_, i64>(0)).unwrap();
    (n("checkpoints"), n("overlay_entries"), n("blobs"))
}
fn blob_file(env: &Env, b: &BlobId) -> PathBuf {
    env.store_dir().join("blobs").join(&b.as_str()[..2]).join(b.as_str())
}
fn blob_files(env: &Env) -> usize {
    let mut n = 0;
    for d in fs::read_dir(env.store_dir().join("blobs")).unwrap() {
        let d = d.unwrap().path();
        if d.is_dir() {
            n += fs::read_dir(d).unwrap().count();
        }
    }
    n
}
fn ids(s: &Store, id: &SessionId, r: Option<&str>) -> Vec<u64> {
    s.checkpoints(id, r.map(repo).as_ref()).unwrap().into_iter().map(|i| u64::from(i.id)).collect()
}

// ------------------------------------------------------------------ tests

#[test]
fn record_checkpoint_roundtrip_keeps_base() {
    let (env, s) = setup();
    let a = sess("aaaaaaaa");
    active(&env, &s, &a);
    let b: Vec<BlobId> = (0..4).map(|i| s.put_bytes(format!("blob {i}").as_bytes()).unwrap()).collect();
    let entries = vec![
        OverlayEntry { path: rp("z/added"), kind: EntryKind::Added { blob: b[0].clone() }, mode: 0o100755 },
        OverlayEntry { path: rp("m/modified"), kind: EntryKind::Modified { blob: b[1].clone() }, mode: 0o100644 },
        deleted("gone"),
        OverlayEntry { path: rp("new/name"), kind: EntryKind::Renamed { from: rp("old/name"), blob: b[2].clone() }, mode: 0o100644 },
        OverlayEntry { path: rp("a/untracked"), kind: EntryKind::Untracked { blob: b[3].clone() }, mode: 0o100644 },
    ];
    let mut c = cp(&a, "app", manual(), entries);
    c.base = g("app", '2');
    let id = s.record_checkpoint(c.clone()).unwrap();
    assert_eq!(id, cid(1), "input id is ignored");
    let want = sorted(c, 1);
    assert_eq!(s.checkpoint(&a, id).unwrap(), want);
    assert_eq!(s.checkpoint(&a, id).unwrap().base, g("app", '2'));
    assert_eq!(s.latest_checkpoint(&a, &repo("app")).unwrap(), Some(want.clone()));
    assert_eq!(s.latest_checkpoint(&a, &repo("lib")).unwrap(), None);
    let infos = s.checkpoints(&a, None).unwrap();
    assert_eq!(
        infos,
        vec![CheckpointInfo { id: cid(1), repo: repo("app"), base: g("app", '2'), at: t(50), reason: manual(), entries: 5 }]
    );
    assert_code(&s.checkpoint(&a, cid(2)).unwrap_err(), Code::NoSuchCheckpoint);
    assert_eq!(counts(&env), (1, 5, 4));
}

#[test]
fn checkpoint_ids_monotone_per_session() {
    let (env, s) = setup();
    let (a, b) = (sess("aaaaaaaa"), sess("bbbbbbbb"));
    active(&env, &s, &a);
    active(&env, &s, &b);
    let x = s.put_bytes(b"x").unwrap();
    let rec = |id: &SessionId, r: &str| u64::from(s.record_checkpoint(cp(id, r, manual(), vec![added("f", &x)])).unwrap());
    assert_eq!([rec(&a, "app"), rec(&a, "lib"), rec(&a, "app")], [1, 2, 3]);
    assert_eq!(rec(&b, "app"), 1);
    assert_eq!(rec(&a, "lib"), 4);
    assert_eq!(rec(&b, "lib"), 2);
    assert_eq!(ids(&s, &a, None), vec![1, 2, 3, 4]);
    assert_eq!(ids(&s, &a, Some("app")), vec![1, 3]);
    assert_eq!(ids(&s, &a, Some("lib")), vec![2, 4]);
    assert_eq!(u64::from(s.latest_checkpoint(&a, &repo("app")).unwrap().unwrap().id), 3);
    assert_eq!(u64::from(s.latest_checkpoint(&b, &repo("lib")).unwrap().unwrap().id), 2);
}

#[test]
fn failed_record_consumes_no_id() {
    let (env, s) = setup();
    let a = sess("aaaaaaaa");
    active(&env, &s, &a);
    let x = s.put_bytes(b"x").unwrap();
    assert_eq!(s.record_checkpoint(cp(&a, "app", manual(), vec![added("f", &x)])).unwrap(), cid(1));
    let before = counts(&env);
    s.arm(Seam::AfterCheckpointRow);
    let y = s.put_bytes(b"y").unwrap();
    let e = s.record_checkpoint(cp(&a, "app", manual(), vec![added("f", &x), added("g", &y)])).unwrap_err();
    assert_code(&e, Code::Unknown);
    assert_eq!(detail(&e, "outcome").and_then(|v| v.as_str()), Some("not_written"));
    assert_eq!(counts(&env), before);
    assert_eq!(s.record_checkpoint(cp(&a, "app", manual(), vec![added("g", &y)])).unwrap(), cid(2));
    assert_eq!(s.record_checkpoint(cp(&a, "app", manual(), vec![added("h", &y)])).unwrap(), cid(3));
    assert_eq!(ids(&s, &a, None), vec![1, 2, 3]);
}

#[test]
fn checkpoint_id_refuses_past_i64_max() {
    let (env, s) = setup();
    let a = sess("aaaaaaaa");
    active(&env, &s, &a);
    {
        let (c, _) = raw(&env.db(), false);
        let base: i64 = c
            .query_row("SELECT id FROM base_gens WHERE repo = 'app' AND tree = ?1", [tree('1').as_str()], |r| r.get(0))
            .unwrap();
        c.execute(
            "INSERT INTO checkpoints (session, id, repo, base_gen, at_ns, reason, reason_text, entries) \
             VALUES (?1, ?2, 'app', ?3, 0, 'before_publish', NULL, 0)",
            params![a.as_str(), i64::MAX, base],
        )
        .unwrap();
    }
    let x = s.put_bytes(b"x").unwrap();
    let before = counts(&env);
    let e = s.record_checkpoint(cp(&a, "app", manual(), vec![added("f", &x)])).unwrap_err();
    assert_code(&e, Code::TooLarge);
    assert_eq!(detail(&e, "limit").and_then(|v| v.as_str()), Some("checkpoint_id"));
    assert_eq!(counts(&env), before);
    assert_eq!(ids(&s, &a, None), vec![i64::MAX as u64]);
}

#[test]
fn ids_survive_reopen() {
    let (env, s) = setup();
    let a = sess("aaaaaaaa");
    active(&env, &s, &a);
    let x = s.put_bytes(b"persist").unwrap();
    let c1 = cp(&a, "app", manual(), vec![added("f", &x)]);
    assert_eq!(s.record_checkpoint(c1.clone()).unwrap(), cid(1));
    let ino = fs::metadata(blob_file(&env, &x)).unwrap().ino();
    drop(s);
    let s = env.open();
    assert_eq!(s.checkpoint(&a, cid(1)).unwrap(), sorted(c1, 1));
    assert_eq!(s.read_blob(&x, 100).unwrap(), b"persist");
    assert_eq!(s.put_bytes(b"persist").unwrap(), x, "same content is reused");
    assert_eq!(fs::metadata(blob_file(&env, &x)).unwrap().ino(), ino);
    assert_eq!(blob_files(&env), 1);
    assert_eq!(s.record_checkpoint(cp(&a, "app", manual(), vec![added("g", &x)])).unwrap(), cid(2));
}

#[test]
fn negative_id_read_back_is_corrupt() {
    let (env, s) = setup();
    let a = sess("aaaaaaaa");
    active(&env, &s, &a);
    {
        let (c, _) = raw(&env.db(), false);
        c.execute_batch("PRAGMA ignore_check_constraints = 1").unwrap();
        let base: i64 = c
            .query_row("SELECT id FROM base_gens WHERE repo = 'app' AND tree = ?1", [tree('1').as_str()], |r| r.get(0))
            .unwrap();
        c.execute(
            "INSERT INTO checkpoints (session, id, repo, base_gen, at_ns, reason, reason_text, entries) \
             VALUES (?1, -5, 'app', ?2, 0, 'before_publish', NULL, 0)",
            params![a.as_str(), base],
        )
        .unwrap();
    }
    assert_reason(&s.checkpoints(&a, None).unwrap_err(), Code::Corrupt, "bad_row");
    assert_reason(&s.checkpoints(&a, Some(&repo("app"))).unwrap_err(), Code::Corrupt, "bad_row");
    assert_reason(&s.checkpoint(&a, CheckpointId::from(-5i64)).unwrap_err(), Code::Corrupt, "bad_row");
    assert_reason(&s.latest_checkpoint(&a, &repo("app")).unwrap_err(), Code::Corrupt, "bad_row");
}

#[test]
fn checkpoint_read_with_missing_entries_is_corrupt() {
    let (env, s) = setup();
    let a = sess("aaaaaaaa");
    active(&env, &s, &a);
    let x = s.put_bytes(b"x").unwrap();
    let id = s.record_checkpoint(cp(&a, "app", manual(), vec![added("a", &x), added("b", &x), deleted("c")])).unwrap();
    {
        let (c, _) = raw(&env.db(), true);
        assert_eq!(c.execute("DELETE FROM overlay_entries WHERE path = 'b'", []).unwrap(), 1);
    }
    for e in [s.checkpoint(&a, id).unwrap_err(), s.latest_checkpoint(&a, &repo("app")).unwrap_err()] {
        assert_reason(&e, Code::Corrupt, "entries_mismatch");
        assert_eq!(detail(&e, "expected").and_then(|v| v.as_i64()), Some(3));
        assert_eq!(detail(&e, "actual").and_then(|v| v.as_i64()), Some(2));
    }
    let infos = s.checkpoints(&a, None).unwrap();
    assert_eq!((infos.len(), infos[0].entries), (1, 3));
}

#[test]
fn tool_call_checkpoint_recorded_once() {
    let (env, s) = setup();
    let a = sess("aaaaaaaa");
    active(&env, &s, &a);
    let x = s.put_bytes(b"x").unwrap();
    let tc = |id: &str| CheckpointReason::ToolCall { id: id.into() };
    assert_eq!(s.record_checkpoint(cp(&a, "app", tc("t1"), vec![added("f", &x)])).unwrap(), cid(1));
    let before = counts(&env);
    let y = s.put_bytes(b"y").unwrap();
    let before_y = counts(&env);
    assert_eq!(before, before_y);
    assert_eq!(s.record_checkpoint(cp(&a, "app", tc("t1"), vec![added("g", &y), deleted("h")])).unwrap(), cid(1));
    assert_eq!(counts(&env), before, "second record writes nothing");
    assert_eq!(s.checkpoint(&a, cid(1)).unwrap().entries, vec![added("f", &x)]);
    assert_eq!(s.record_checkpoint(cp(&a, "lib", tc("t1"), vec![added("f", &x)])).unwrap(), cid(2));
    assert_eq!(s.record_checkpoint(cp(&a, "app", tc("t2"), vec![added("f", &x)])).unwrap(), cid(3));
    // a Manual with the same text is not a tool call
    let m = CheckpointReason::Manual { message: "t1".into() };
    assert_eq!(s.record_checkpoint(cp(&a, "app", m, vec![added("f", &x)])).unwrap(), cid(4));
}

#[test]
fn checkpoint_unit_is_one_transaction() {
    let (env, s) = setup();
    let a = sess("aaaaaaaa");
    active(&env, &s, &a);
    let old = s.put_bytes(b"old").unwrap();
    s.record_checkpoint(cp(&a, "app", manual(), vec![added("o", &old)])).unwrap();
    let before = counts(&env);
    let n1 = s.put_bytes(b"new1").unwrap();
    let n2 = s.put_bytes(b"new2").unwrap();
    let c2 = cp(&a, "app", manual(), vec![added("a", &n1), added("b", &n2), added("c", &old), deleted("d")]);
    s.arm(Seam::PauseBeforeCommit);
    std::thread::scope(|sc| {
        let h = sc.spawn(|| s.record_checkpoint(c2.clone()));
        s.wait_paused();
        assert_eq!(counts(&env), before);
        assert_eq!(ids(&s, &a, None), vec![1]);
        assert_code(&s.checkpoint(&a, cid(2)).unwrap_err(), Code::NoSuchCheckpoint);
        s.release();
        assert_eq!(h.join().unwrap().unwrap(), cid(2));
    });
    assert_eq!(counts(&env), (2, 5, 3));
    assert_eq!(s.checkpoint(&a, cid(2)).unwrap(), sorted(c2, 2));
    // the old blob's first registration wins
    let (c, _) = raw(&env.db(), false);
    let n: i64 = c.query_row("SELECT count(*) FROM blobs WHERE id = ?1", [old.as_str()], |r| r.get(0)).unwrap();
    assert_eq!(n, 1);
}

#[test]
fn record_checkpoint_failure_rolls_back_whole_unit() {
    let (env, s) = setup();
    let a = sess("aaaaaaaa");
    active(&env, &s, &a);
    let old = s.put_bytes(b"old").unwrap();
    let c1 = cp(&a, "app", manual(), vec![added("o", &old)]);
    s.record_checkpoint(c1.clone()).unwrap();
    let before = counts(&env);
    let fresh = s.put_bytes(b"fresh").unwrap();
    s.arm(Seam::AfterCheckpointRow);
    let e = s.record_checkpoint(cp(&a, "app", manual(), vec![added("n", &fresh), added("o", &old)])).unwrap_err();
    assert_code(&e, Code::Unknown);
    assert_eq!(counts(&env), before, "no checkpoint row, no entries, no new blob row");
    let (c, _) = raw(&env.db(), false);
    let n: i64 = c.query_row("SELECT count(*) FROM blobs WHERE id = ?1", [fresh.as_str()], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);
    assert_eq!(s.checkpoint(&a, cid(1)).unwrap(), sorted(c1, 1));
    assert_eq!(s.read_blob(&old, 10).unwrap(), b"old");
    // reported once: the seam is spent and the store did not retry by itself
    assert_eq!(ids(&s, &a, None), vec![1]);
    assert_eq!(s.record_checkpoint(cp(&a, "app", manual(), vec![added("n", &fresh)])).unwrap(), cid(2));
}

#[test]
fn late_source_failure_writes_no_checkpoint() {
    let (env, s) = setup();
    let a = sess("aaaaaaaa");
    active(&env, &s, &a);
    let src = env.checkout.join("src");
    fs::create_dir(&src).unwrap();
    let names = ["f1", "f2", "f3", "f4"];
    for (i, n) in names.iter().enumerate() {
        if i != 2 {
            fs::write(src.join(n), format!("content {n}")).unwrap();
        }
    }
    // a caller putting files in order, then recording
    let run = || -> Result<CheckpointId> {
        let mut entries = vec![];
        for n in names {
            let b = s.put_blob(&src.join(n))?;
            entries.push(added(n, &b));
        }
        s.record_checkpoint(cp(&a, "app", manual(), entries))
    };
    let e = run().unwrap_err();
    assert_code(&e, Code::NoSuchPath);
    assert_eq!(counts(&env).0, 0);
    assert_eq!(ids(&s, &a, None), Vec::<u64>::new());
    assert_eq!(blob_files(&env), 2, "the k-1 blobs already put stay in blobs/ for gc");
    assert_eq!(counts(&env).2, 0, "unregistered until a checkpoint references them");
}

#[test]
fn checkpoint_refuses_reference_to_missing_blob() {
    let (env, s) = setup();
    let a = sess("aaaaaaaa");
    active(&env, &s, &a);
    let x = s.put_bytes(b"x").unwrap();
    let ghost = BlobId::try_from("f".repeat(64)).unwrap();
    let before = counts(&env);
    let e = s.record_checkpoint(cp(&a, "app", manual(), vec![added("a", &x), added("b", &ghost)])).unwrap_err();
    assert_reason(&e, Code::Corrupt, "blob_missing");
    assert_eq!(counts(&env), before);
    // Renamed and Untracked references are derived and checked too
    let e = s
        .record_checkpoint(cp(&a, "app", manual(), vec![OverlayEntry {
            path: rp("n"),
            kind: EntryKind::Renamed { from: rp("o"), blob: ghost.clone() },
            mode: 0o100644,
        }]))
        .unwrap_err();
    assert_reason(&e, Code::Corrupt, "blob_missing");
    let e = s
        .record_checkpoint(cp(&a, "app", manual(), vec![OverlayEntry { path: rp("u"), kind: EntryKind::Untracked { blob: ghost.clone() }, mode: 0o100644 }]))
        .unwrap_err();
    assert_reason(&e, Code::Corrupt, "blob_missing");
    assert_eq!(counts(&env), before);
    // every committed checkpoint's blobs are on disk and registered
    let id = s.record_checkpoint(cp(&a, "app", manual(), vec![added("a", &x)])).unwrap();
    for e in s.checkpoint(&a, id).unwrap().entries {
        if let EntryKind::Added { blob } = e.kind {
            assert!(blob_file(&env, &blob).is_file());
            let (c, _) = raw(&env.db(), false);
            let n: i64 = c.query_row("SELECT count(*) FROM blobs WHERE id = ?1", [blob.as_str()], |r| r.get(0)).unwrap();
            assert_eq!(n, 1);
        }
    }
}

#[test]
fn interleaved_sessions_do_not_see_failed_checkpoints() {
    let (env, s) = setup();
    let ss = [sess("aaaaaaaa"), sess("bbbbbbbb"), sess("cccccccc")];
    for id in &ss {
        active(&env, &s, id);
    }
    let b0 = s.put_bytes(b"s0").unwrap();
    let b1 = s.put_bytes(b"s1").unwrap();
    let c0 = cp(&ss[0], "app", manual(), vec![added("f", &b0)]);
    let r0 = s.record_checkpoint(c0.clone()).unwrap();
    let b2 = s.put_bytes(b"s2").unwrap();
    s.arm(Seam::AfterCheckpointRow);
    let e = s.record_checkpoint(cp(&ss[1], "app", manual(), vec![added("f", &b1)])).unwrap_err();
    assert_code(&e, Code::Unknown);
    let c2 = cp(&ss[2], "app", manual(), vec![added("f", &b2)]);
    let r2 = s.record_checkpoint(c2.clone()).unwrap();
    assert_eq!((r0, r2), (cid(1), cid(1)));
    assert_eq!(s.checkpoint(&ss[0], r0).unwrap(), sorted(c0, 1));
    assert_eq!(s.checkpoint(&ss[2], r2).unwrap(), sorted(c2, 1));
    assert_eq!(ids(&s, &ss[1], None), Vec::<u64>::new());
    assert_eq!(s.latest_checkpoint(&ss[1], &repo("app")).unwrap(), None);
    assert_code(&s.checkpoint(&ss[1], cid(1)).unwrap_err(), Code::NoSuchCheckpoint);
    assert_eq!(counts(&env), (2, 2, 2));
}

#[test]
fn checkpoint_refused_for_untracked_or_removed() {
    let (env, s) = setup();
    let (a, r) = (sess("aaaaaaaa"), sess("rrrrrrrr"));
    active(&env, &s, &a);
    active(&env, &s, &r);
    assert!(s.set_lifecycle(&r, Lifecycle::Active, Lifecycle::Removing, None, t(12)).unwrap());
    assert!(s.set_lifecycle(&r, Lifecycle::Removing, Lifecycle::Removed, None, t(13)).unwrap());
    let x = s.put_bytes(b"x").unwrap();
    let e = || vec![added("f", &x)];
    let before = counts(&env);

    assert_reason(&s.record_checkpoint(cp(&a, "web", manual(), e())).unwrap_err(), Code::InvalidInput, "untracked");
    let mut c = cp(&a, "app", manual(), e());
    c.base = g("app", '9');
    assert_reason(&s.record_checkpoint(c).unwrap_err(), Code::InvalidInput, "base_unregistered");
    let mut c = cp(&a, "app", manual(), e());
    c.base = g("lib", '1');
    assert_code(&s.record_checkpoint(c).unwrap_err(), Code::InvalidInput);
    assert_code(&s.record_checkpoint(cp(&r, "app", manual(), e())).unwrap_err(), Code::NoSuchSession);
    assert_code(&s.record_checkpoint(cp(&sess("nnnnnnnn"), "app", manual(), e())).unwrap_err(), Code::NoSuchSession);
    let mut c = cp(&a, "app", manual(), e());
    c.repo = repo("other");
    c.base.repo = repo("other");
    assert_code(&s.record_checkpoint(c).unwrap_err(), Code::NoSuchRepo);
    assert_eq!(counts(&env), before);
}

#[test]
fn same_path_type_change_is_one_entry() {
    let (env, s) = setup();
    let a = sess("aaaaaaaa");
    active(&env, &s, &a);
    let x = s.put_bytes(b"target").unwrap();
    let before = counts(&env);
    let bad = |entries: Vec<OverlayEntry>| {
        let e = s.record_checkpoint(cp(&a, "app", manual(), entries)).unwrap_err();
        assert_code(&e, Code::InvalidInput);
    };
    bad(vec![deleted("p"), added("p", &x)]);
    // other incoherent overlays
    let ren = |to: &str, from: &str| OverlayEntry { path: rp(to), kind: EntryKind::Renamed { from: rp(from), blob: x.clone() }, mode: 0o100644 };
    bad(vec![ren("p", "p")]);
    bad(vec![ren("p", "q"), deleted("q")]);
    bad(vec![ren("p", "q"), ren("r", "q")]);
    bad(vec![OverlayEntry { path: rp("p"), kind: EntryKind::Deleted, mode: 0o100644 }]);
    bad(vec![OverlayEntry { path: rp("p"), kind: EntryKind::Added { blob: x.clone() }, mode: 0o040000 }]);
    bad(vec![OverlayEntry { path: rp("p"), kind: EntryKind::Added { blob: x.clone() }, mode: 0 }]);
    assert_eq!(counts(&env), before);

    // file → symlink is one Modified entry with mode 120000
    let link = OverlayEntry { path: rp("p"), kind: EntryKind::Modified { blob: x.clone() }, mode: 0o120000 };
    let c = cp(&a, "app", manual(), vec![link.clone()]);
    let id = s.record_checkpoint(c.clone()).unwrap();
    assert_eq!(s.checkpoint(&a, id).unwrap(), sorted(c, 1));
    assert_eq!(s.checkpoint(&a, id).unwrap().entries, vec![link]);
}
