//! store.md §7, Task 7: publish records and ref audit.

mod support;

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;
use strata_store::*;
use strata_types::{
    Action, BaseGen, BaseManifest, Batch, BlobId, Checkpoint, CheckpointId, CheckpointReason, Code, CommitOid, DepsSet,
    DepsSetId, EntryKind, OverlayEntry, ReleaseGroup, RepoName, RepoPath, SessionId, SessionManifest, TreeOid,
    WorkspaceState,
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
#[allow(dead_code)]
fn deleted(p: &str) -> OverlayEntry {
    OverlayEntry { path: rp(p), kind: EntryKind::Deleted, mode: 0 }
}

fn counts(env: &Env) -> (i64, i64, i64) {
    let (c, _) = raw(&env.db(), false);
    let n = |t: &str| c.query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get::<_, i64>(0)).unwrap();
    (n("publishes"), n("publish_items"), n("refs_audit"))
}

/// Session a with checkpoints 1 (app) and 2 (lib); session r removed with checkpoint 1.
fn world() -> (Env, Store, SessionId, SessionId) {
    let (env, s) = setup();
    let (a, r) = (sess("aaaaaaaa"), sess("rrrrrrrr"));
    active(&env, &s, &a);
    active(&env, &s, &r);
    let x = s.put_bytes(b"x").unwrap();
    s.record_checkpoint(cp(&a, "app", manual(), vec![added("f", &x)])).unwrap();
    s.record_checkpoint(cp(&a, "lib", manual(), vec![added("f", &x)])).unwrap();
    s.record_checkpoint(cp(&r, "app", manual(), vec![added("f", &x)])).unwrap();
    assert!(s.set_lifecycle(&r, Lifecycle::Active, Lifecycle::Removing, None, t(12)).unwrap());
    assert!(s.set_lifecycle(&r, Lifecycle::Removing, Lifecycle::Removed, None, t(13)).unwrap());
    (env, s, a, r)
}
fn batch(id: &SessionId, r: &str, c: u64, paths: &[&str]) -> Batch {
    Batch { session: id.clone(), repo: repo(r), checkpoint: cid(c), paths: paths.iter().map(|p| rp(p)).collect() }
}
fn group(batches: Vec<Batch>) -> ReleaseGroup {
    ReleaseGroup { id: 77, batches }
}
fn change(op: RefOp, name: &str, outcome: RefOutcome, publish: Option<u64>) -> RefChange {
    RefChange {
        at: t(60),
        repo: repo("app"),
        op,
        ref_name: name.into(),
        old: Some(head('1')),
        new: head('2'),
        outcome,
        publish,
        confirmed: None,
        detail: None,
    }
}

// ------------------------------------------------------------------ tests

#[test]
fn publish_record_roundtrip_and_phase_machine() {
    let (env, s, a, r) = world();
    let gr = group(vec![batch(&a, "app", 1, &["z/last", "a/first", "old/renamed"]), batch(&a, "lib", 2, &["f"])]);
    let id = s.record_publish(&gr, t(70)).unwrap();
    assert_eq!(id, 1, "input id is ignored");
    let want = PublishRecord {
        group: ReleaseGroup { id: 1, batches: gr.batches.clone() },
        phase: PublishPhase::Running,
        started: t(70),
        ended: None,
        outcome: None,
    };
    assert_eq!(s.publish_record(1).unwrap(), Some(want.clone()));
    assert_eq!(s.publish_record(2).unwrap(), None);
    assert_eq!(s.record_publish(&group(vec![batch(&a, "app", 1, &["f"])]), t(71)).unwrap(), 2);

    // phase machine
    let out = json!({ "app": "pushed" });
    assert!(s.set_publish_phase(1, PublishPhase::Running, PublishPhase::Unknown, Some(&out), t(80)).unwrap());
    let got = s.publish_record(1).unwrap().unwrap();
    assert_eq!((got.phase, got.ended, got.outcome.clone()), (PublishPhase::Unknown, Some(t(80)), Some(out.clone())));
    assert!(!s.set_publish_phase(1, PublishPhase::Running, PublishPhase::Done, None, t(81)).unwrap(), "CAS on a stale phase");
    assert!(s.set_publish_phase(1, PublishPhase::Unknown, PublishPhase::Done, None, t(82)).unwrap());
    let got = s.publish_record(1).unwrap().unwrap();
    assert_eq!((got.phase, got.ended, got.outcome, got.group), (PublishPhase::Done, Some(t(82)), Some(out), want.group));
    for (from, to) in [
        (PublishPhase::Done, PublishPhase::Running),
        (PublishPhase::Done, PublishPhase::Unknown),
        (PublishPhase::Unknown, PublishPhase::Running),
        (PublishPhase::Running, PublishPhase::Running),
        (PublishPhase::Done, PublishPhase::Done),
    ] {
        assert_code(&s.set_publish_phase(1, from, to, None, t(83)).unwrap_err(), Code::InvalidInput);
    }
    assert!(s.set_publish_phase(2, PublishPhase::Running, PublishPhase::Done, None, t(84)).unwrap());
    // an unknown publish id is a caller bug, not a lost CAS
    assert_reason(&s.set_publish_phase(99, PublishPhase::Running, PublishPhase::Done, None, t(85)).unwrap_err(), Code::InvalidInput, "no_such_publish");

    // refusals write nothing
    let before = counts(&env);
    assert_code(&s.record_publish(&group(vec![batch(&a, "app", 9, &["f"])]), t(90)).unwrap_err(), Code::NoSuchCheckpoint);
    assert_code(
        &s.record_publish(&group(vec![batch(&a, "app", 1, &["f"]), batch(&a, "lib", 7, &["f"])]), t(90)).unwrap_err(),
        Code::NoSuchCheckpoint,
    );
    assert_code(&s.record_publish(&group(vec![batch(&r, "app", 1, &["f"])]), t(90)).unwrap_err(), Code::NoSuchSession);
    assert_code(&s.record_publish(&group(vec![batch(&sess("nnnnnnnn"), "app", 1, &["f"])]), t(90)).unwrap_err(), Code::NoSuchSession);
    assert_code(&s.record_publish(&group(vec![]), t(90)).unwrap_err(), Code::InvalidInput);
    assert_code(&s.record_publish(&group(vec![batch(&a, "app", 1, &["f"]), batch(&a, "lib", 2, &[])]), t(90)).unwrap_err(), Code::InvalidInput);
    assert_code(&s.record_publish(&group(vec![batch(&a, "lib", 1, &["f"])]), t(90)).unwrap_err(), Code::InvalidInput);
    assert_eq!(counts(&env), before);
}

#[test]
fn open_publishes_listed_for_restart() {
    let (env, s, a, _) = world();
    for i in 0..4 {
        assert_eq!(s.record_publish(&group(vec![batch(&a, "app", 1, &["f"])]), t(70 + i)).unwrap(), i + 1);
    }
    // 1 Running, 2 Unknown, 3 Done, 4 Unknown→Done
    assert!(s.set_publish_phase(2, PublishPhase::Running, PublishPhase::Unknown, None, t(80)).unwrap());
    assert!(s.set_publish_phase(3, PublishPhase::Running, PublishPhase::Done, None, t(80)).unwrap());
    assert!(s.set_publish_phase(4, PublishPhase::Running, PublishPhase::Unknown, None, t(80)).unwrap());
    assert!(s.set_publish_phase(4, PublishPhase::Unknown, PublishPhase::Done, None, t(81)).unwrap());
    let open = |s: &Store| s.open_publishes().unwrap().into_iter().map(|p| (p.group.id, p.phase)).collect::<Vec<_>>();
    let want = vec![(1, PublishPhase::Running), (2, PublishPhase::Unknown)];
    assert_eq!(open(&s), want);
    let full = s.open_publishes().unwrap();
    assert_eq!(full[0], s.publish_record(1).unwrap().unwrap());
    drop(s);
    let s = env.open();
    assert_eq!(open(&s), want);
    assert_eq!(s.open_publishes().unwrap(), full);
}

#[test]
fn ref_audit_only_strata_refs_for_update_ref() {
    let (env, s, a, _) = world();
    let p = s.record_publish(&group(vec![batch(&a, "app", 1, &["f"])]), t(70)).unwrap();
    let before = counts(&env);
    for c in [
        change(RefOp::UpdateRef, "refs/heads/main", RefOutcome::Applied, Some(p)),
        change(RefOp::UpdateRef, "refs/strataX/main", RefOutcome::Applied, Some(p)),
        change(RefOp::UpdateRef, "refs/strata/publish/main", RefOutcome::Pushed, Some(p)),
        change(RefOp::UpdateRef, "refs/strata/publish/main", RefOutcome::Unknown, Some(p)),
        change(RefOp::Push, "refs/heads/main", RefOutcome::HeadMoved, Some(p)),
        change(RefOp::Push, "refs/heads/main", RefOutcome::Applied, Some(p)),
        change(RefOp::UpdateRef, "refs/strata/publish/main", RefOutcome::Applied, Some(99)),
    ] {
        assert_code(&s.record_ref_change(&c).unwrap_err(), Code::InvalidInput);
    }
    assert_eq!(counts(&env), before);

    let c1 = change(RefOp::UpdateRef, "refs/strata/publish/main", RefOutcome::Applied, Some(p));
    let mut c2 = change(RefOp::UpdateRef, "refs/strata/publish/dev", RefOutcome::HeadMoved, Some(p));
    c2.old = None;
    let mut c3 = change(RefOp::Push, "refs/heads/main", RefOutcome::Pushed, Some(p));
    c3.at = t(61);
    c3.confirmed = Some(Action::PushDeploy { repo: repo("app"), branch: "main".into() });
    c3.detail = Some("remote: ok".into());
    let mut c4 = change(RefOp::Push, "refs/heads/main", RefOutcome::Refused, None);
    c4.at = t(62);
    for c in [&c1, &c2, &c3, &c4] {
        s.record_ref_change(c).unwrap();
    }
    assert_eq!(s.ref_changes(p).unwrap(), vec![c1, c2, c3.clone()]);
    assert_eq!(s.ref_changes(99).unwrap(), vec![]);
    // confirmed is stored as the Action's JSON, nothing else
    let (c, _) = raw(&env.db(), false);
    let stored: String = c.query_row("SELECT confirmed FROM refs_audit WHERE outcome = 'pushed'", [], |r| r.get(0)).unwrap();
    assert_eq!(serde_json::from_str::<serde_json::Value>(&stored).unwrap(), serde_json::to_value(c3.confirmed.unwrap()).unwrap());
}
