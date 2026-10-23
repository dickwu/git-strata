//! store.md §7, Task 5: sessions, workspaces, seeds, move intents, retire_dir.

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;
use strata_store::*;
use strata_types::{
    BaseGen, BaseManifest, Code, CommitOid, DepsSet, DepsSetId, ProposalId, RepoName, RepoPath, SessionId, SessionManifest, StrataError, TreeOid,
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
fn g(r: &str, tr: char, d: char) -> BaseGen {
    BaseGen { repo: repo(r), tree: tree(tr), deps: set_id(d) }
}
fn register_deps(s: &Store, r: &str, d: char) {
    let set = DepsSet {
        id: set_id(d),
        repo: repo(r),
        inputs_hash: "0".repeat(64),
        platform: "darwin-arm64".into(),
        toolchain: "none".into(),
        dirs: vec![],
    };
    s.record_deps_set(&set, DepsOrigin::Empty, None, t(1)).unwrap();
}
fn register(s: &Store, b: &BaseGen) {
    let m = BaseManifest { tree: b.tree.clone(), entries: BTreeMap::new() };
    s.record_base_gen(b, &m, t(1)).unwrap();
}
fn interrupted() -> StrataError {
    StrataError { code: Code::Unknown, message: "x".into(), detail: Some(json!({ "reason": "interrupted" })) }
}

/// Store with deps 'a','b' for app and lib, and generations
/// app/1/a, app/2/a, app/3/a, app/3/b, app/4/a, app/5/a, lib/1/a.
fn setup() -> (Env, Store) {
    let env = env();
    let s = env.open();
    for r in ["app", "lib"] {
        register_deps(&s, r, 'a');
        register_deps(&s, r, 'b');
    }
    for b in [g("app", '1', 'a'), g("app", '2', 'a'), g("app", '3', 'b'), g("app", '4', 'a'), g("app", '3', 'a'), g("app", '5', 'a'), g("lib", '1', 'a')] {
        register(&s, &b);
    }
    (env, s)
}

fn sroot(env: &Env, id: &SessionId) -> PathBuf {
    env.root.join("sessions").join(id.as_str())
}
fn manifest(env: &Env, id: &SessionId) -> SessionManifest {
    let root = sroot(env, id);
    let mut repos = BTreeMap::new();
    repos.insert(repo("app"), WorkspaceState { base: g("app", '1', 'a'), head: head('1'), path: root.join("app"), tracked: true });
    repos.insert(repo("lib"), WorkspaceState { base: g("lib", '1', 'a'), head: head('2'), path: root.join("lib"), tracked: false });
    SessionManifest { id: id.clone(), ticket: Some("T-1".into()), root, repos }
}
fn active(env: &Env, s: &Store, id: &SessionId) {
    s.create_session(&manifest(env, id), t(10)).unwrap();
    assert!(s.set_lifecycle(id, Lifecycle::Creating, Lifecycle::Active, None, t(11)).unwrap());
}
fn stg(env: &Env, id: &SessionId, name: &str) -> PathBuf {
    sroot(env, id).join(name)
}
fn begin(env: &Env, s: &Store, id: &SessionId, to: char, name: &str) -> u64 {
    s.begin_move(id, &repo("app"), &g("app", to, 'a'), &head(to), &stg(env, id, name), t(20)).unwrap()
}
fn swapping(s: &Store, id: &SessionId, n: u64, c: &[RepoPath]) {
    assert!(s.set_move_state(id, &repo("app"), n, MoveState::Preparing, MoveState::Swapping, Some(c), t(21)).unwrap());
}
fn ws(s: &Store, id: &SessionId) -> (WorkspaceState, WorkspaceRecord) {
    let r = s.session(id).unwrap().unwrap();
    (r.manifest.repos[&repo("app")].clone(), r.workspaces[&repo("app")].clone())
}
fn intents(s: &Store, id: &SessionId) -> Vec<MoveIntent> {
    s.move_intents(id).unwrap()
}
fn last(s: &Store, id: &SessionId) -> MoveIntent {
    intents(s, id).into_iter().filter(|i| i.repo == repo("app")).last().unwrap()
}

/// Make a dir with one file, so we can check the trash copy is intact.
fn mk(p: &Path) {
    fs::create_dir_all(p).unwrap();
    fs::write(p.join("f"), b"hello").unwrap();
}
#[track_caller]
fn retire_ok(s: &Store, p: &Path) {
    let to = s.retire_dir(p).unwrap_or_else(|e| panic!("retire {} failed: {e:?}", p.display()));
    assert!(!p.exists(), "{} still there", p.display());
    assert!(to.starts_with(s.root().join("store/trash")), "{}", to.display());
    assert_eq!(fs::read(to.join("f")).unwrap(), b"hello");
}
#[track_caller]
fn retire_no(s: &Store, p: &Path, code: Code) -> StrataError {
    let e = s.retire_dir(p).unwrap_err();
    assert_code(&e, code);
    if code != Code::NoSuchPath {
        assert!(fs::symlink_metadata(p).is_ok(), "{} was moved", p.display());
    }
    e
}

// ------------------------------------------------------------------ tests

#[test]
fn session_manifest_roundtrip() {
    let (env, s) = setup();
    let (a, b) = (sess("aaaaaaaa"), sess("bbbbbbbb"));
    let m = manifest(&env, &a);
    s.create_session(&m, t(10)).unwrap();
    let r = s.session(&a).unwrap().unwrap();
    assert_eq!(r.manifest, m);
    assert_eq!((r.lifecycle, r.created, r.removed, r.phase.clone(), r.scene.clone()), (Lifecycle::Creating, t(10), None, None, None));
    for w in r.workspaces.values() {
        assert_eq!((w.generation, w.health, w.scene.clone(), w.conflicts.clone(), w.seeds.clone()), (1, Health::Ok, None, vec![], vec![]));
    }
    assert!(s.session(&sess("cccccccc")).unwrap().is_none());

    // listing by creation time
    s.create_session(&manifest(&env, &b), t(5)).unwrap();
    let ids: Vec<_> = s.sessions().unwrap().into_iter().map(|r| r.manifest.id).collect();
    assert_eq!(ids, vec![b.clone(), a.clone()]);

    // duplicates
    assert_code(&s.create_session(&m, t(12)).unwrap_err(), Code::Exists);

    // layout and base checks
    let c = sess("cccccccc");
    let mut bad = manifest(&env, &c);
    bad.root = env.root.join("elsewhere");
    assert_code(&s.create_session(&bad, t(12)).unwrap_err(), Code::InvalidInput);
    let mut bad = manifest(&env, &c);
    bad.repos.get_mut(&repo("app")).unwrap().path = sroot(&env, &c).join("other");
    assert_code(&s.create_session(&bad, t(12)).unwrap_err(), Code::InvalidInput);
    let mut bad = manifest(&env, &c);
    bad.repos.get_mut(&repo("app")).unwrap().base = g("app", '9', 'a');
    assert_code(&s.create_session(&bad, t(12)).unwrap_err(), Code::InvalidInput);
    let mut bad = manifest(&env, &c);
    bad.repos.get_mut(&repo("app")).unwrap().base = g("lib", '1', 'a');
    assert_code(&s.create_session(&bad, t(12)).unwrap_err(), Code::InvalidInput);
    let mut bad = manifest(&env, &c);
    bad.repos.clear();
    assert_code(&s.create_session(&bad, t(12)).unwrap_err(), Code::InvalidInput);
    assert!(s.session(&c).unwrap().is_none());
    assert_eq!(s.sessions().unwrap().len(), 2);
}

#[test]
fn lifecycle_moves_forward_by_compare_and_swap() {
    use Lifecycle::*;
    let (env, s) = setup();
    let id = sess("aaaaaaaa");
    s.create_session(&manifest(&env, &id), t(10)).unwrap();
    s.set_session_phase(&id, "cloning").unwrap();
    assert_eq!(s.session(&id).unwrap().unwrap().phase.as_deref(), Some("cloning"));

    // not the current state: false, nothing written
    assert!(!s.set_lifecycle(&id, Active, Removing, None, t(11)).unwrap());
    assert_eq!(s.session(&id).unwrap().unwrap().lifecycle, Creating);
    // forbidden edges and scene rules
    for (f, to) in [(Removed, Active), (Active, Unknown), (Unknown, Active), (Creating, Removing), (Active, Creating)] {
        let sc = (to == Unknown).then(|| json!({"x": 1}));
        assert_reason(&s.set_lifecycle(&id, f, to, sc.as_ref(), t(11)).unwrap_err(), Code::InvalidInput, "transition");
    }
    assert_code(&s.set_lifecycle(&id, Creating, Unknown, None, t(11)).unwrap_err(), Code::InvalidInput);
    assert_code(&s.set_lifecycle(&id, Creating, Active, Some(&json!(1)), t(11)).unwrap_err(), Code::InvalidInput);
    assert_code(&s.set_lifecycle(&sess("zzzzzzzz"), Creating, Active, None, t(11)).unwrap_err(), Code::NoSuchSession);

    assert!(s.set_lifecycle(&id, Creating, Active, None, t(11)).unwrap());
    assert_code(&s.set_session_phase(&id, "x").unwrap_err(), Code::InvalidInput);
    assert!(s.set_lifecycle(&id, Active, Removing, None, t(12)).unwrap());
    s.set_session_phase(&id, "removing-ws").unwrap();
    assert!(s.set_lifecycle(&id, Removing, Removed, None, t(13)).unwrap());
    let r = s.session(&id).unwrap().unwrap();
    assert_eq!((r.lifecycle, r.removed), (Removed, Some(t(13))));
    assert_code(&s.set_session_phase(&id, "x").unwrap_err(), Code::InvalidInput);
    assert_code(&s.set_session_phase(&sess("zzzzzzzz"), "x").unwrap_err(), Code::NoSuchSession);

    // the other three edges
    let b = sess("bbbbbbbb");
    s.create_session(&manifest(&env, &b), t(20)).unwrap();
    assert!(s.set_lifecycle(&b, Creating, Unknown, Some(&json!({"step": 1})), t(21)).unwrap());
    assert!(s.set_lifecycle(&b, Unknown, Removing, None, t(22)).unwrap());
    assert!(s.set_lifecycle(&b, Removing, Unknown, Some(&json!({"step": 2})), t(23)).unwrap());
    assert_eq!(s.session(&b).unwrap().unwrap().scene, Some(json!({"step": 2})));
}

#[test]
fn unknown_session_keeps_its_scene() {
    use Lifecycle::*;
    let (env, s) = setup();
    let id = sess("aaaaaaaa");
    s.create_session(&manifest(&env, &id), t(10)).unwrap();
    let scene = json!({"failed": "clone", "paths": ["a", "b"], "n": 3});
    assert!(s.set_lifecycle(&id, Creating, Unknown, Some(&scene), t(11)).unwrap());
    assert_eq!(s.session(&id).unwrap().unwrap().scene, Some(scene.clone()));
    drop(s);
    let s = env.open();
    let r = s.session(&id).unwrap().unwrap();
    assert_eq!((r.lifecycle, r.scene.clone()), (Unknown, Some(scene.clone())));
    assert!(s.set_lifecycle(&id, Unknown, Removing, None, t(12)).unwrap());
    assert!(s.set_lifecycle(&id, Removing, Removed, None, t(13)).unwrap());
    assert_eq!(s.session(&id).unwrap().unwrap().scene, Some(scene));
}

#[test]
fn seeds_are_per_path_rows_written_once() {
    let (env, s) = setup();
    let id = sess("aaaaaaaa");
    s.create_session(&manifest(&env, &id), t(10)).unwrap();
    let seed = |p: &str, n: u64| SeedRow {
        path: rp(p),
        source: env.checkout.join(p),
        link: (n == 2).then(|| PathBuf::from("../target")),
        size: n * 10,
        source_mtime: UNIX_EPOCH - Duration::from_nanos(1_500_000_001 * n),
        copied: t(n),
    };
    s.record_seeds(&id, &repo("app"), &[seed("z.env", 1), seed("a/.env", 2)]).unwrap();
    s.record_seeds(&id, &repo("app"), &[seed("m.env", 3)]).unwrap();
    let w = &s.session(&id).unwrap().unwrap().workspaces[&repo("app")];
    assert_eq!(w.seeds, vec![seed("a/.env", 2), seed("m.env", 3), seed("z.env", 1)]);

    let mut other = seed("m.env", 4);
    other.size = 999;
    let e = s.record_seeds(&id, &repo("app"), &[other]).unwrap_err();
    assert_reason(&e, Code::InvalidInput, "seed_exists");
    let e = s.record_seeds(&id, &repo("app"), &[seed("q", 1), seed("q", 1)]).unwrap_err();
    assert_reason(&e, Code::InvalidInput, "seed_exists");
    let w = &s.session(&id).unwrap().unwrap().workspaces[&repo("app")];
    assert_eq!(w.seeds, vec![seed("a/.env", 2), seed("m.env", 3), seed("z.env", 1)]);
    assert_code(&s.record_seeds(&id, &repo("nope"), &[seed("x", 1)]).unwrap_err(), Code::NoSuchRepo);
    assert_code(&s.record_seeds(&sess("zzzzzzzz"), &repo("app"), &[seed("x", 1)]).unwrap_err(), Code::NoSuchSession);

    assert!(s.set_lifecycle(&id, Lifecycle::Creating, Lifecycle::Active, None, t(11)).unwrap());
    assert_reason(&s.record_seeds(&id, &repo("app"), &[seed("x", 1)]).unwrap_err(), Code::InvalidInput, "lifecycle");
}

#[test]
fn move_intent_generation_never_reused() {
    let (env, s) = setup();
    let id = sess("aaaaaaaa");
    s.create_session(&manifest(&env, &id), t(10)).unwrap();
    // not active
    let e = s.begin_move(&id, &repo("app"), &g("app", '2', 'a'), &head('2'), &stg(&env, &id, ".m"), t(20)).unwrap_err();
    assert_reason(&e, Code::InvalidInput, "lifecycle");
    assert!(s.set_lifecycle(&id, Lifecycle::Creating, Lifecycle::Active, None, t(11)).unwrap());

    let n = begin(&env, &s, &id, '2', ".m2");
    assert_eq!(n, 2);
    let e = s.begin_move(&id, &repo("app"), &g("app", '3', 'b'), &head('3'), &stg(&env, &id, ".m3"), t(22)).unwrap_err();
    assert_code(&e, Code::Exists);
    assert_eq!(detail(&e, "n"), Some(&json!(2)));
    assert!(s.set_move_state(&id, &repo("app"), 2, MoveState::Preparing, MoveState::Refused, None, t(23)).unwrap());
    assert!(!s.set_move_state(&id, &repo("app"), 2, MoveState::Preparing, MoveState::Refused, None, t(23)).unwrap());
    assert_eq!(begin(&env, &s, &id, '3', ".m3"), 3);
    assert!(s.set_move_state(&id, &repo("app"), 3, MoveState::Preparing, MoveState::Abandoned, None, t(24)).unwrap());

    let before = intents(&s, &id);
    let bad = |to: BaseGen, r: &str, staging: PathBuf| s.begin_move(&id, &repo(r), &to, &head('4'), &staging, t(25)).unwrap_err();
    assert_reason(&bad(g("lib", '1', 'a'), "lib", stg(&env, &id, ".x")), Code::InvalidInput, "untracked");
    assert_code(&bad(g("app", '9', 'a'), "app", stg(&env, &id, ".x")), Code::InvalidInput);
    assert_code(&bad(g("lib", '1', 'a'), "app", stg(&env, &id, ".x")), Code::InvalidInput);
    assert_code(&bad(g("app", '4', 'a'), "app", env.root.join("store/x")), Code::InvalidInput);
    assert_code(&bad(g("app", '4', 'a'), "app", stg(&env, &id, "app")), Code::InvalidInput);
    assert_code(&bad(g("app", '4', 'a'), "app", stg(&env, &id, "a/b")), Code::InvalidInput);
    assert_code(&bad(g("app", '4', 'a'), "app", stg(&env, &sess("bbbbbbbb"), ".x")), Code::InvalidInput);
    assert_code(&bad(g("app", '4', 'a'), "nope", stg(&env, &id, ".x")), Code::NoSuchRepo);
    assert_code(
        &s.begin_move(&sess("zzzzzzzz"), &repo("app"), &g("app", '4', 'a'), &head('4'), &stg(&env, &id, ".x"), t(25)).unwrap_err(),
        Code::NoSuchSession,
    );
    // health unknown
    let n = begin(&env, &s, &id, '4', ".m4");
    assert_eq!(n, 4);
    swapping(&s, &id, n, &[]);
    s.fail_move(&id, &repo("app"), n, &json!({"swap": "half"}), t(26)).unwrap();
    assert_reason(&bad(g("app", '2', 'a'), "app", stg(&env, &id, ".x")), Code::InvalidInput, "health_unknown");
    assert_eq!(intents(&s, &id).len(), before.len() + 1);
    let ns: Vec<u64> = intents(&s, &id).iter().map(|i| i.n).collect();
    assert_eq!(ns, vec![2, 3, 4]);
}

#[test]
fn finish_move_swaps_base_head_and_generation_together() {
    let (env, s) = setup();
    let id = sess("aaaaaaaa");
    active(&env, &s, &id);
    let n = begin(&env, &s, &id, '3', ".m");
    // begin wrote target b deps? use app/3/b
    let n = {
        assert!(s.set_move_state(&id, &repo("app"), n, MoveState::Preparing, MoveState::Abandoned, None, t(20)).unwrap());
        s.begin_move(&id, &repo("app"), &g("app", '3', 'b'), &head('3'), &stg(&env, &id, ".m3"), t(20)).unwrap()
    };
    s.set_move_phase(&id, &repo("app"), n, "staged").unwrap();
    assert_code(&s.set_old_tree(&id, &repo("app"), n, &stg(&env, &id, ".old")).unwrap_err(), Code::InvalidInput);
    let c = vec![rp("src/a.rs"), rp("b")];
    swapping(&s, &id, n, &c);
    let (old_state, old_rec) = ws(&s, &id);

    s.arm(Seam::PauseBeforeCommit);
    std::thread::scope(|sc| {
        let h = sc.spawn(|| s.finish_move(&id, &repo("app"), n, &c, t(30)));
        s.wait_paused();
        let (st, rec) = ws(&s, &id);
        assert_eq!((st, rec.generation, rec.conflicts), (old_state.clone(), old_rec.generation, old_rec.conflicts.clone()));
        assert_eq!(last(&s, &id).state, MoveState::Swapping);
        s.release();
        h.join().unwrap().unwrap();
    });
    let (st, rec) = ws(&s, &id);
    assert_eq!((st.base, st.head, rec.generation, rec.conflicts), (g("app", '3', 'b'), head('3'), n, c.clone()));
    let i = last(&s, &id);
    assert_eq!((i.state, i.ended, i.phase.as_deref(), i.old_tree.clone()), (MoveState::Done, Some(t(30)), Some("staged"), None));
    assert_code(&s.set_move_phase(&id, &repo("app"), n, "x").unwrap_err(), Code::InvalidInput);

    // set_old_tree: only Done, only once, only staging-shaped paths
    assert_code(&s.set_old_tree(&id, &repo("app"), n, &stg(&env, &id, "app")).unwrap_err(), Code::InvalidInput);
    assert_code(&s.set_old_tree(&id, &repo("app"), n, &env.root.join("store/x")).unwrap_err(), Code::InvalidInput);
    s.set_old_tree(&id, &repo("app"), n, &stg(&env, &id, ".old")).unwrap();
    assert_eq!(last(&s, &id).old_tree, Some(stg(&env, &id, ".old")));
    assert_code(&s.set_old_tree(&id, &repo("app"), n, &stg(&env, &id, ".old2")).unwrap_err(), Code::InvalidInput);
    assert_eq!(last(&s, &id).old_tree, Some(stg(&env, &id, ".old")));
    assert_code(&s.set_old_tree(&id, &repo("app"), 99, &stg(&env, &id, ".old2")).unwrap_err(), Code::InvalidInput);
}

#[test]
fn failed_move_marks_workspace_unknown() {
    let (env, s) = setup();
    let id = sess("aaaaaaaa");
    active(&env, &s, &id);
    let n = begin(&env, &s, &id, '2', ".m2");
    assert_eq!(s.open_move_intents().unwrap().iter().map(|i| (i.n, i.state)).collect::<Vec<_>>(), vec![(n, MoveState::Preparing)]);
    swapping(&s, &id, n, &[rp("x")]);
    assert_eq!(s.open_move_intents().unwrap().iter().map(|i| (i.n, i.state)).collect::<Vec<_>>(), vec![(n, MoveState::Swapping)]);
    // fail only from Swapping
    let scene = json!({"left": ["a"]});
    s.fail_move(&id, &repo("app"), n, &scene, t(30)).unwrap();
    assert_code(&s.fail_move(&id, &repo("app"), n, &scene, t(30)).unwrap_err(), Code::InvalidInput);
    let i = last(&s, &id);
    assert_eq!((i.state, i.ended), (MoveState::Unknown, Some(t(30))));
    let (st, rec) = ws(&s, &id);
    assert_eq!((rec.health, rec.scene.clone(), rec.generation, st.base), (Health::Unknown, Some(scene), 1, g("app", '1', 'a')));
    assert!(s.open_move_intents().unwrap().is_empty());
    let e = s.begin_move(&id, &repo("app"), &g("app", '4', 'a'), &head('4'), &stg(&env, &id, ".m4"), t(31)).unwrap_err();
    assert_code(&e, Code::InvalidInput);

    // Preparing → Abandoned on another session
    let b = sess("bbbbbbbb");
    active(&env, &s, &b);
    let n = begin(&env, &s, &b, '2', ".m");
    assert!(s.set_move_state(&b, &repo("app"), n, MoveState::Preparing, MoveState::Abandoned, None, t(32)).unwrap());
    let i = last(&s, &b);
    assert_eq!((i.state, i.ended, i.conflicts.clone()), (MoveState::Abandoned, Some(t(32)), None));
    assert!(s.open_move_intents().unwrap().is_empty());
}

#[test]
fn move_conflicts_are_recorded_at_swapping() {
    let (env, s) = setup();
    let id = sess("aaaaaaaa");
    active(&env, &s, &id);
    let n = begin(&env, &s, &id, '2', ".m2");
    assert_eq!(last(&s, &id).conflicts, None);
    let before = intents(&s, &id);
    let c = vec![rp("b.rs"), rp("a.rs")];
    let st = |from, to, c: Option<&[RepoPath]>| s.set_move_state(&id, &repo("app"), n, from, to, c, t(21));
    assert_code(&st(MoveState::Preparing, MoveState::Swapping, None).unwrap_err(), Code::InvalidInput);
    assert_code(&st(MoveState::Preparing, MoveState::Refused, Some(&c)).unwrap_err(), Code::InvalidInput);
    assert_code(&st(MoveState::Preparing, MoveState::Abandoned, Some(&c)).unwrap_err(), Code::InvalidInput);
    assert_code(&st(MoveState::Swapping, MoveState::Done, None).unwrap_err(), Code::InvalidInput);
    assert_eq!(intents(&s, &id), before);
    swapping(&s, &id, n, &c);
    assert_eq!(last(&s, &id).conflicts, Some(c.clone()));

    // finish with another set: refused, nothing written
    let (ws0, rec0) = ws(&s, &id);
    let i0 = intents(&s, &id);
    for other in [vec![rp("b.rs")], vec![rp("a.rs"), rp("b.rs")], vec![]] {
        let e = s.finish_move(&id, &repo("app"), n, &other, t(30)).unwrap_err();
        assert_reason(&e, Code::InvalidInput, "conflicts_mismatch");
        assert_eq!(ws(&s, &id), (ws0.clone(), rec0.clone()));
        assert_eq!(intents(&s, &id), i0);
    }
    s.finish_move(&id, &repo("app"), n, &c, t(30)).unwrap();
    assert_eq!(ws(&s, &id).1.conflicts, c);

    // empty list reads back Some([]); refused intent reads None; fail keeps conflicts
    let n = begin(&env, &s, &id, '4', ".m4");
    assert!(s.set_move_state(&id, &repo("app"), n, MoveState::Preparing, MoveState::Refused, None, t(31)).unwrap());
    assert_eq!(last(&s, &id).conflicts, None);
    let n = begin(&env, &s, &id, '4', ".m5");
    swapping(&s, &id, n, &[]);
    assert_eq!(last(&s, &id).conflicts, Some(vec![]));
    let b = sess("bbbbbbbb");
    active(&env, &s, &b);
    let m = begin(&env, &s, &b, '2', ".m");
    swapping(&s, &b, m, &c);
    s.fail_move(&b, &repo("app"), m, &json!(1), t(40)).unwrap();
    assert_eq!(last(&s, &b).conflicts, Some(c));
}

fn failed(env: &Env, s: &Store, id: &SessionId, to: char, c: &[RepoPath]) -> u64 {
    active(env, s, id);
    let n = begin(env, s, id, to, ".m2");
    swapping(s, id, n, c);
    s.fail_move(id, &repo("app"), n, &json!({"half": true}), t(30)).unwrap();
    n
}

#[test]
fn resolve_move_abandon_returns_health_to_ok() {
    let (env, s) = setup();
    let id = sess("aaaaaaaa");
    let c = vec![rp("x")];
    let n = failed(&env, &s, &id, '2', &c);
    let (st0, rec0) = ws(&s, &id);
    s.resolve_move(&id, &repo("app"), n, false, t(40)).unwrap();
    let i = last(&s, &id);
    assert_eq!((i.state, i.ended, i.conflicts.clone()), (MoveState::Abandoned, Some(t(40)), Some(c)));
    let (st, rec) = ws(&s, &id);
    assert_eq!(st, st0);
    assert_eq!((rec.health, rec.generation, rec.conflicts, rec.scene), (Health::Ok, rec0.generation, rec0.conflicts, rec0.scene));
    let staging = stg(&env, &id, ".m2");
    mk(&staging);
    retire_ok(&s, &staging);
    let admin = env.root.join(format!("sessions/.repos/{}/app.git/worktrees/{n}", id.as_str()));
    mk(&admin);
    retire_ok(&s, &admin);
    assert_eq!(begin(&env, &s, &id, '4', ".m3"), n + 1);
}

#[test]
fn resolve_move_accept_keeps_the_conflicts_recorded_at_swapping() {
    let (env, s) = setup();
    let id = sess("aaaaaaaa");
    active(&env, &s, &id);
    let n = s.begin_move(&id, &repo("app"), &g("app", '3', 'b'), &head('3'), &stg(&env, &id, ".m"), t(20)).unwrap();
    let c = vec![rp("a"), rp("b")];
    swapping(&s, &id, n, &c);
    drop(s); // crash mid-swap: no close
    let s = env.open();
    let open = s.open_move_intents().unwrap();
    assert_eq!(open.iter().map(|i| (i.n, i.state, i.conflicts.clone())).collect::<Vec<_>>(), vec![(n, MoveState::Swapping, Some(c.clone()))]);
    s.fail_move(&id, &repo("app"), n, &json!({"recover": 1}), t(30)).unwrap();
    let (st0, rec0) = ws(&s, &id);

    s.arm(Seam::PauseBeforeCommit);
    std::thread::scope(|sc| {
        let h = sc.spawn(|| s.resolve_move(&id, &repo("app"), n, true, t(40)));
        s.wait_paused();
        assert_eq!(ws(&s, &id), (st0.clone(), rec0.clone()));
        assert_eq!(last(&s, &id).state, MoveState::Unknown);
        s.release();
        h.join().unwrap().unwrap();
    });
    let (st, rec) = ws(&s, &id);
    assert_eq!((st.base, st.head, rec.generation, rec.conflicts, rec.health), (g("app", '3', 'b'), head('3'), n, c, Health::Ok));
    assert_eq!(rec.scene, Some(json!({"recover": 1})));
    let i = last(&s, &id);
    assert_eq!((i.state, i.old_tree.clone(), i.ended), (MoveState::Done, Some(i.staging.clone()), Some(t(40))));
}

#[test]
fn resolve_move_guards_refuse_and_write_nothing() {
    let (env, s) = setup();
    let id = sess("aaaaaaaa");
    let n = failed(&env, &s, &id, '5', &[rp("c")]);
    let snap = |s: &Store| (s.sessions().unwrap(), s.move_intents(&id).unwrap());
    let before = snap(&s);
    let r = |sid: &SessionId, rn: &str, n: u64, acc: bool| s.resolve_move(sid, &repo(rn), n, acc, t(50)).unwrap_err();

    assert_code(&r(&sess("zzzzzzzz"), "app", n, true), Code::NoSuchSession);
    assert_code(&r(&id, "nope", n, true), Code::NoSuchRepo);
    assert_reason(&r(&id, "lib", n, true), Code::InvalidInput, "not_failed");
    assert_reason(&r(&id, "app", 99, true), Code::InvalidInput, "no_failed_move");
    assert_reason(&r(&id, "app", u64::MAX, false), Code::InvalidInput, "no_failed_move");
    assert_eq!(snap(&s), before);

    // an earlier Done intent is not the failed one
    let b = sess("bbbbbbbb");
    active(&env, &s, &b);
    let m = begin(&env, &s, &b, '2', ".a");
    swapping(&s, &b, m, &[]);
    s.finish_move(&b, &repo("app"), m, &[], t(31)).unwrap();
    let m2 = begin(&env, &s, &b, '4', ".b");
    swapping(&s, &b, m2, &[]);
    s.fail_move(&b, &repo("app"), m2, &json!(2), t(32)).unwrap();
    let before_b = s.move_intents(&b).unwrap();
    assert_reason(&r(&b, "app", m, true), Code::InvalidInput, "no_failed_move");
    assert_eq!(s.move_intents(&b).unwrap(), before_b);

    // abandon then again: not_failed
    s.resolve_move(&b, &repo("app"), m2, false, t(33)).unwrap();
    assert_reason(&r(&b, "app", m2, false), Code::InvalidInput, "not_failed");

    // target base row gone (gc-mode raw connection): Corrupt
    {
        let (c, _) = raw(&env.db(), true);
        c.execute("DELETE FROM base_gens WHERE repo = 'app' AND tree = ?1", [tree('5').as_str()]).unwrap();
    }
    let before = snap(&s);
    assert_reason(&r(&id, "app", n, true), Code::Corrupt, "base_gen_missing");
    assert_eq!(snap(&s), before);

    // Removing, then Removed
    assert!(s.set_lifecycle(&id, Lifecycle::Active, Lifecycle::Removing, None, t(60)).unwrap());
    assert_reason(&r(&id, "app", n, false), Code::InvalidInput, "lifecycle");
    assert!(s.set_lifecycle(&id, Lifecycle::Removing, Lifecycle::Removed, None, t(61)).unwrap());
    let before = snap(&s);
    assert_code(&r(&id, "app", n, false), Code::NoSuchSession);
    assert_eq!(snap(&s), before);
}

#[test]
fn open_proposals_skip_removed_sessions() {
    let (env, s) = setup();
    let (a, b, c) = (sess("aaaaaaaa"), sess("bbbbbbbb"), sess("cccccccc"));
    for id in [&a, &b] {
        active(&env, &s, id);
    }
    let prop = |r: &str, origin: Option<&SessionId>, to: char| NewProposal {
        repo: repo(r),
        origin: origin.cloned(),
        from: set_id('a'),
        from_inputs: "a".repeat(64),
        to: set_id(to),
        to_inputs: to.to_string().repeat(64),
        tree: origin.is_none().then(|| tree('5')),
        head: origin.is_none().then(|| head('5')),
    };
    let pa = s.record_proposal(&prop("app", Some(&a), 'c'), None, t(40)).unwrap().unwrap();
    let pb = s.record_proposal(&prop("lib", Some(&b), 'd'), None, t(41)).unwrap().unwrap();
    let pc = s.record_proposal(&prop("app", Some(&c), 'e'), None, t(42)).unwrap().unwrap();
    let pp = s.record_proposal(&prop("lib", None, 'f'), None, t(43)).unwrap().unwrap();
    assert!(s.set_lifecycle(&b, Lifecycle::Active, Lifecycle::Removing, None, t(50)).unwrap());
    assert!(s.set_lifecycle(&b, Lifecycle::Removing, Lifecycle::Removed, None, t(51)).unwrap());
    let ids = |open| -> Vec<ProposalId> {
        let mut v: Vec<_> = s.proposals(None, open).unwrap().into_iter().map(|p| p.id).collect();
        v.sort();
        v
    };
    assert_eq!(ids(true), vec![pa, pp]);
    let mut all = vec![pa, pb, pc, pp];
    all.sort();
    assert_eq!(ids(false), all);
}

// ------------------------------------------------------------------ retire_dir

#[test]
fn retire_dir_moves_only_dirs_without_live_rows() {
    let (env, s) = setup();
    let r = repo("app");
    // row 1
    let unreg = s.base_dir(&g("app", '7', 'a'));
    mk(&unreg);
    retire_ok(&s, &unreg);
    let reg = s.base_dir(&g("app", '1', 'a'));
    mk(&reg);
    retire_no(&s, &reg, Code::InvalidInput);
    // row 2
    let d = s.deps_dir(&r, &set_id('e'));
    mk(&d);
    retire_ok(&s, &d);
    let d = s.deps_dir(&r, &set_id('a'));
    mk(&d);
    retire_no(&s, &d, Code::InvalidInput);
    // row 3
    let run = s.begin_deps_attempt(&r, &set_id('c'), AttemptKind::Build, t(30)).unwrap();
    let p = s.deps_staging_dir(&r, run);
    mk(&p);
    retire_ok(&s, &p);
    s.end_deps_attempt(run, AttemptState::Refused, &interrupted(), t(31)).unwrap();
    mk(&p);
    retire_ok(&s, &p);
    let fail = s.begin_deps_attempt(&r, &set_id('c'), AttemptKind::Build, t(32)).unwrap();
    s.end_deps_attempt(fail, AttemptState::Failed, &interrupted(), t(33)).unwrap();
    let p = s.deps_staging_dir(&r, fail);
    mk(&p);
    retire_no(&s, &p, Code::InvalidInput);
    // other repo's attempt number
    let p = s.deps_staging_dir(&repo("lib"), run);
    mk(&p);
    retire_no(&s, &p, Code::InvalidInput);
    let sealed = s.begin_deps_attempt(&r, &set_id('c'), AttemptKind::Build, t(34)).unwrap();
    let set = DepsSet { id: set_id('c'), repo: r.clone(), inputs_hash: "0".repeat(64), platform: "darwin-arm64".into(), toolchain: "none".into(), dirs: vec![] };
    s.record_deps_set(&set, DepsOrigin::Build, Some(sealed), t(35)).unwrap();
    let p = s.deps_staging_dir(&r, sealed);
    mk(&p);
    retire_ok(&s, &p);
    // gc deleted the row
    {
        let (c, _) = raw(&env.db(), true);
        c.execute("DELETE FROM deps_attempts WHERE id = ?1", [run as i64]).unwrap();
    }
    let p = s.deps_staging_dir(&r, run);
    mk(&p);
    retire_ok(&s, &p);
    // never issued
    let p = s.deps_staging_dir(&r, 99);
    mk(&p);
    retire_no(&s, &p, Code::InvalidInput);

    // off the table, symlinks, dot-dot, files, missing
    for bad in ["store", "store/blobs", "bases/app", "x/y", "sessions/.repos", "bases/app/.staging"] {
        let p = env.root.join(bad);
        fs::create_dir_all(&p).unwrap();
        assert_reason(&retire_no(&s, &p, Code::InvalidInput), Code::InvalidInput, "path_shape");
    }
    assert_code(&s.retire_dir(&env.checkout).unwrap_err(), Code::InvalidInput);
    assert_code(&s.retire_dir(&env.root.join("bases/app/../app/x/y")).unwrap_err(), Code::InvalidInput);
    assert_code(&s.retire_dir(Path::new("bases/app/a/b")).unwrap_err(), Code::InvalidInput);
    let target = env.root.join("bases/app/real");
    mk(&target);
    let link_parent = env.root.join("bases/lnk");
    std::os::unix::fs::symlink(&target, &link_parent).unwrap();
    let via = link_parent.join(format!("{}/{}", tree('8').as_str(), set_id('a').as_str()));
    fs::create_dir_all(env.root.join("bases/app/real").join(tree('8').as_str()).join(set_id('a').as_str())).unwrap();
    assert_reason(&s.retire_dir(&via).unwrap_err(), Code::InvalidInput, "symlink");
    let lnk = s.base_dir(&g("app", '8', 'b'));
    fs::create_dir_all(lnk.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&target, &lnk).unwrap();
    assert_reason(&s.retire_dir(&lnk).unwrap_err(), Code::InvalidInput, "symlink");
    assert!(target.join("f").exists());
    let file = s.base_dir(&g("app", '6', 'a'));
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, b"x").unwrap();
    retire_no(&s, &file, Code::NotDirectory);
    retire_no(&s, &s.base_dir(&g("app", '9', 'a')), Code::NoSuchPath);
}

#[test]
fn retire_dir_takes_dirs_of_removed_sessions() {
    use Lifecycle::*;
    let (env, s) = setup();
    let dirs = |id: &SessionId| [sroot(&env, id), env.root.join("sessions/.repos").join(id.as_str())];
    let make = |id: &SessionId| {
        for d in dirs(id) {
            mk(&d);
            mk(&d.join("app.git"));
        }
    };
    let (a, b, c, d) = (sess("aaaaaaaa"), sess("bbbbbbbb"), sess("cccccccc"), sess("dddddddd"));
    // Creating, Active, Unknown: refused
    s.create_session(&manifest(&env, &a), t(10)).unwrap();
    make(&a);
    for p in dirs(&a) {
        retire_no(&s, &p, Code::InvalidInput);
    }
    assert!(s.set_lifecycle(&a, Creating, Active, None, t(11)).unwrap());
    for p in dirs(&a) {
        retire_no(&s, &p, Code::InvalidInput);
    }
    s.create_session(&manifest(&env, &b), t(10)).unwrap();
    assert!(s.set_lifecycle(&b, Creating, Unknown, Some(&json!(1)), t(11)).unwrap());
    make(&b);
    for p in dirs(&b) {
        retire_no(&s, &p, Code::InvalidInput);
    }
    // Removing
    assert!(s.set_lifecycle(&a, Active, Removing, None, t(12)).unwrap());
    for p in dirs(&a) {
        retire_ok(&s, &p);
    }
    // Removed
    assert!(s.set_lifecycle(&b, Unknown, Removing, None, t(12)).unwrap());
    assert!(s.set_lifecycle(&b, Removing, Removed, None, t(13)).unwrap());
    for p in dirs(&b) {
        retire_ok(&s, &p);
    }
    // row gone (gc)
    s.create_session(&manifest(&env, &c), t(10)).unwrap();
    {
        let (conn, _) = raw(&env.db(), true);
        conn.execute_batch(&format!(
            "DELETE FROM workspaces WHERE session = '{0}'; DELETE FROM sessions WHERE id = '{0}';",
            c.as_str()
        ))
        .unwrap();
    }
    make(&c);
    for p in dirs(&c) {
        retire_ok(&s, &p);
    }
    // never existed
    make(&d);
    for p in dirs(&d) {
        retire_ok(&s, &p);
    }
}

#[test]
fn retire_dir_takes_swapped_out_trees() {
    let (env, s) = setup();
    let (id, other) = (sess("aaaaaaaa"), sess("bbbbbbbb"));
    active(&env, &s, &id);
    active(&env, &s, &other);
    // Done: staging holds the swapped-out tree
    let n = begin(&env, &s, &id, '2', ".done");
    swapping(&s, &id, n, &[]);
    s.finish_move(&id, &repo("app"), n, &[], t(30)).unwrap();
    // Done with old tree kept elsewhere
    let n = begin(&env, &s, &id, '3', ".kept-staging");
    let n = {
        assert!(s.set_move_state(&id, &repo("app"), n, MoveState::Preparing, MoveState::Abandoned, None, t(31)).unwrap());
        s.begin_move(&id, &repo("app"), &g("app", '3', 'b'), &head('3'), &stg(&env, &id, ".kept"), t(31)).unwrap()
    };
    swapping(&s, &id, n, &[]);
    s.finish_move(&id, &repo("app"), n, &[], t(32)).unwrap();
    s.set_old_tree(&id, &repo("app"), n, &stg(&env, &id, ".old")).unwrap();
    let r = begin(&env, &s, &id, '4', ".refused");
    assert!(s.set_move_state(&id, &repo("app"), r, MoveState::Preparing, MoveState::Refused, None, t(33)).unwrap());
    let p = begin(&env, &s, &id, '4', ".preparing");

    for name in [".done", ".old", ".kept-staging", ".refused", ".preparing", ".kept"] {
        let d = stg(&env, &id, name);
        mk(&d);
        retire_ok(&s, &d);
    }
    for name in ["app", "lib", ".nobody", ".done/sub"] {
        let d = stg(&env, &id, name);
        mk(&d);
        retire_no(&s, &d, Code::InvalidInput);
    }
    // Swapping and Unknown
    swapping(&s, &id, p, &[]);
    let d = stg(&env, &id, ".preparing");
    mk(&d);
    retire_no(&s, &d, Code::InvalidInput);
    s.fail_move(&id, &repo("app"), p, &json!(1), t(34)).unwrap();
    retire_no(&s, &d, Code::InvalidInput);
    // another session's staging name
    let d = stg(&env, &other, ".done");
    mk(&d);
    retire_no(&s, &d, Code::InvalidInput);
    // Removing: row 5 no longer applies
    s.resolve_move(&id, &repo("app"), p, false, t(35)).unwrap();
    let d = stg(&env, &id, ".refused");
    mk(&d);
    assert!(s.set_lifecycle(&id, Lifecycle::Active, Lifecycle::Removing, None, t(36)).unwrap());
    retire_no(&s, &d, Code::InvalidInput);
    mk(&sroot(&env, &id));
    retire_ok(&s, &sroot(&env, &id));
}

#[test]
fn retire_dir_takes_superseded_admin_dirs() {
    let (env, s) = setup();
    let id = sess("aaaaaaaa");
    active(&env, &s, &id);
    for (to, name) in [('2', ".a"), ('4', ".b")] {
        let n = begin(&env, &s, &id, to, name);
        swapping(&s, &id, n, &[]);
        s.finish_move(&id, &repo("app"), n, &[], t(30)).unwrap();
    }
    assert_eq!(ws(&s, &id).1.generation, 3);
    let n4 = begin(&env, &s, &id, '2', ".c");
    assert!(s.set_move_state(&id, &repo("app"), n4, MoveState::Preparing, MoveState::Refused, None, t(31)).unwrap());
    let n5 = begin(&env, &s, &id, '2', ".d");
    swapping(&s, &id, n5, &[]);
    assert_eq!((n4, n5), (4, 5));
    let wt = |m: &str| env.root.join(format!("sessions/.repos/{}/app.git/worktrees/{m}", id.as_str()));
    for m in ["1", "2", "4"] {
        mk(&wt(m));
        retire_ok(&s, &wt(m));
    }
    for m in ["3", "5", "9", "02", "0"] {
        mk(&wt(m));
        retire_no(&s, &wt(m), Code::InvalidInput);
    }
    let other = env.root.join(format!("sessions/.repos/{}/lib.git/worktrees/1", id.as_str()));
    mk(&other);
    retire_no(&s, &other, Code::InvalidInput);
    let nows = env.root.join(format!("sessions/.repos/{}/nope.git/worktrees/1", id.as_str()));
    mk(&nows);
    retire_no(&s, &nows, Code::InvalidInput);
    // not active: never
    s.fail_move(&id, &repo("app"), n5, &json!(1), t(32)).unwrap();
    assert!(s.set_lifecycle(&id, Lifecycle::Active, Lifecycle::Removing, None, t(33)).unwrap());
    mk(&wt("1"));
    retire_no(&s, &wt("1"), Code::InvalidInput);
}

#[test]
fn retire_dir_takes_base_staging() {
    let (env, s) = setup();
    let st = env.root.join("bases/app/.staging");
    let h = "0123456789abcdef0123456789abcdef";
    let d = st.join(h);
    mk(&d);
    mk(&d.join("tree/src"));
    mk(&d.join(".export-tree-abc123"));
    retire_ok(&s, &d);
    // a 0555 root is still taken
    let d = st.join("fedcba9876543210fedcba9876543210");
    mk(&d);
    mk(&d.join("tree"));
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&d, fs::Permissions::from_mode(0o555)).unwrap();
    }
    retire_ok(&s, &d);
    for bad in [&h[..31], "0123456789ABCDEF0123456789abcdef", "0123456789abcdef0123456789abcdeg"] {
        let p = st.join(bad);
        mk(&p);
        retire_no(&s, &p, Code::InvalidInput);
    }
    retire_no(&s, &st, Code::InvalidInput);
    let d = st.join("11111111111111111111111111111111");
    mk(&d.join("tree"));
    retire_no(&s, &d.join("tree"), Code::InvalidInput);
    let f = st.join("22222222222222222222222222222222");
    fs::write(&f, b"x").unwrap();
    retire_no(&s, &f, Code::NotDirectory);
    retire_no(&s, &st.join("33333333333333333333333333333333"), Code::NoSuchPath);
}
