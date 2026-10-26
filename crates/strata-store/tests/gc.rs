//! store.md §7, Task 9: gc, gc_repo and last_gc_report (§5.11).
//! Run with `cargo test -p strata-store --features test-seams --test gc`
//! (the dev-dependency already enables `test-seams`).

mod support;

use std::collections::BTreeMap;
use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::macos::fs::MetadataExt as _;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use strata_store::*;
use strata_types::{
    BaseGen, BaseManifest, Batch, BlobId, Checkpoint, CheckpointId, CheckpointReason, Code, CommitOid, DepsSet, DepsSetId,
    EntryKind, OverlayEntry, ReleaseGroup, RepoName, RepoPath, SessionId, SessionManifest, StrataError, TreeOid, WorkspaceState,
};
use support::*;

const K0: Duration = Duration::ZERO;
/// A keep no test timestamp (all in 1970) is past.
const FOREVER: Duration = Duration::from_secs(100 * 365 * 86_400);
const UF_IMMUTABLE: u32 = 0x2;

fn repo(s: &str) -> RepoName {
    RepoName::try_from(s.to_string()).unwrap()
}
/// Letters outside hex map onto distinct digits so tests can keep mnemonic names.
fn set_id(c: char) -> DepsSetId {
    let c = match c {
        'x' => '1', 'y' => '2', 'z' => '3', 'w' => '4', 'r' => '5', 'q' => '6', 's' => '7',
        c => c,
    };
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
    UNIX_EPOCH + Duration::from_secs(secs)
}
fn g(tr: char, d: char) -> BaseGen {
    BaseGen { repo: repo("app"), tree: tree(tr), deps: set_id(d) }
}
fn mk(p: &Path) {
    fs::create_dir_all(p).unwrap();
    fs::write(p.join("f"), b"hello").unwrap();
}
fn chflags(p: &Path, flags: u32) {
    let c = CString::new(p.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::lchflags(c.as_ptr(), flags) }, 0, "chflags {}", p.display());
}
fn flags(p: &Path) -> u32 {
    fs::symlink_metadata(p).unwrap().st_flags()
}
fn chmod(p: &Path, m: u32) {
    fs::set_permissions(p, fs::Permissions::from_mode(m)).unwrap();
}
fn mode(p: &Path) -> u32 {
    fs::symlink_metadata(p).unwrap().mode() & 0o7777
}
/// mtime 2020 (ctime stays now).
fn old_mtime(p: &Path) {
    let _w = support::spawn_window();
    let st = Command::new("touch").args(["-h", "-t", "202001010000"]).arg(p).status().unwrap();
    assert!(st.success());
}
fn exists(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}
fn count(env: &Env, sql: &str) -> i64 {
    let (c, _) = raw(&env.db(), false);
    c.query_row(sql, (), |r| r.get(0)).unwrap()
}
fn trash(env: &Env) -> Vec<PathBuf> {
    match fs::read_dir(env.store_dir().join("trash")) {
        Ok(d) => d.map(|e| e.unwrap().path()).collect(),
        Err(_) => vec![],
    }
}
fn floor0(s: &Store) {
    s.arm(Seam::AgeFloor { secs: 0 });
}
fn deps(s: &Store, d: char) {
    let set = DepsSet {
        id: set_id(d),
        repo: repo("app"),
        inputs_hash: "0".repeat(64),
        platform: "darwin-arm64".into(),
        toolchain: "none".into(),
        dirs: vec![],
    };
    s.record_deps_set(&set, DepsOrigin::Empty, None, t(1)).unwrap();
}
/// Register `b` at `t(at)` and give it a directory with one file.
fn reg(s: &Store, b: &BaseGen, at: u64) {
    let m = BaseManifest { tree: b.tree.clone(), entries: BTreeMap::new() };
    s.record_base_gen(b, &m, t(at)).unwrap();
    mk(&s.base_dir(b));
}
fn has_gen(env: &Env, b: &BaseGen) -> bool {
    count(env, &format!("SELECT count(*) FROM base_gens WHERE tree = '{}' AND deps = '{}'", b.tree.as_str(), b.deps.as_str())) == 1
}
fn has_set(env: &Env, d: char) -> bool {
    count(env, &format!("SELECT count(*) FROM deps_sets WHERE id = '{}'", set_id(d).as_str())) == 1
}
/// Deps `a` and generation app/1/a (at t(1)).
fn setup() -> (Env, Store) {
    let env = env();
    let s = env.open();
    deps(&s, 'a');
    reg(&s, &g('1', 'a'), 1);
    (env, s)
}
fn session(env: &Env, s: &Store, name: &str, b: &BaseGen) -> SessionId {
    let id = sess(name);
    let root = env.root.join("sessions").join(id.as_str());
    let mut repos = BTreeMap::new();
    repos.insert(repo("app"), WorkspaceState { base: b.clone(), head: head('1'), path: root.join("app"), tracked: true });
    s.create_session(&SessionManifest { id: id.clone(), ticket: None, root, repos }, t(10)).unwrap();
    assert!(s.set_lifecycle(&id, Lifecycle::Creating, Lifecycle::Active, None, t(11)).unwrap());
    id
}
fn remove(s: &Store, id: &SessionId) {
    assert!(s.set_lifecycle(id, Lifecycle::Active, Lifecycle::Removing, None, t(12)).unwrap());
    assert!(s.set_lifecycle(id, Lifecycle::Removing, Lifecycle::Removed, None, t(13)).unwrap());
}
fn added(p: &str, b: &BlobId) -> OverlayEntry {
    OverlayEntry { path: rp(p), kind: EntryKind::Added { blob: b.clone() }, mode: 0o100644 }
}
fn cp(s: &Store, id: &SessionId, b: &BaseGen, reason: CheckpointReason, entries: Vec<OverlayEntry>) -> CheckpointId {
    s.record_checkpoint(Checkpoint {
        id: CheckpointId::try_from(999u64).unwrap(),
        session: id.clone(),
        repo: repo("app"),
        base: b.clone(),
        at: t(50),
        reason,
        entries,
    })
    .unwrap()
}
fn manual() -> CheckpointReason {
    CheckpointReason::Manual { message: "m".into() }
}
fn blob_path(env: &Env, id: &BlobId) -> PathBuf {
    env.store_dir().join("blobs").join(&id.as_str()[..2]).join(id.as_str())
}
/// Same length, different bytes.
fn corrupt(p: &Path) {
    let len = fs::metadata(p).unwrap().len() as usize;
    let m = mode(p);
    chmod(p, 0o644);
    fs::write(p, vec![b'#'; len]).unwrap();
    chmod(p, m);
}
fn rewrite(p: &Path, bytes: &[u8]) {
    let m = mode(p);
    chmod(p, 0o644);
    fs::write(p, bytes).unwrap();
    chmod(p, m);
}
fn outcomes(env: &Env, scope: Option<&str>) -> Vec<String> {
    let (c, _) = raw(&env.db(), false);
    let mut q = c.prepare("SELECT outcome FROM gc_runs WHERE scope IS ?1 ORDER BY id").unwrap();
    q.query_map([scope], |r| r.get::<_, String>(0)).unwrap().map(|r| r.unwrap()).collect()
}
fn interrupted() -> StrataError {
    StrataError { code: Code::Unknown, message: "x".into(), detail: Some(serde_json::json!({ "reason": "interrupted" })) }
}

// ------------------------------------------------------------------ tests

#[test]
fn gc_keeps_referenced_generations() {
    let (env, s) = setup();
    let (g1, g2) = (g('1', 'a'), g('2', 'a'));
    let a = session(&env, &s, "aaaaaaaa", &g1);
    let x = s.put_bytes(b"referenced").unwrap();
    let c1 = cp(&s, &a, &g1, manual(), vec![added("f", &x)]);
    reg(&s, &g2, 2);
    floor0(&s);
    s.gc(K0).unwrap();
    s.gc_repo(&repo("app"), K0).unwrap();
    assert!(has_gen(&env, &g1) && s.base_dir(&g1).exists());
    assert_eq!(s.checkpoint(&a, c1).unwrap().entries.len(), 1);
    s.verify_blob(&x).unwrap();
    assert_eq!(count(&env, "SELECT count(*) FROM blobs"), 1);
    // a referenced blob row cannot be deleted even with the gate open
    {
        let (c, _gate) = raw(&env.db(), true);
        assert!(c.execute("DELETE FROM blobs WHERE id = ?1", [x.as_str()]).is_err());
    }
    remove(&s, &a);
    let r = s.gc(K0).unwrap();
    assert_eq!(r.sessions, 1);
    let r = s.gc_repo(&repo("app"), K0).unwrap();
    assert_eq!(r.base_gens, 1);
    assert!(!has_gen(&env, &g1) && !s.base_dir(&g1).exists());
    assert!(has_gen(&env, &g2) && s.base_dir(&g2).exists());
}

#[test]
fn gc_keeps_base_of_every_retained_checkpoint() {
    let (env, s) = setup();
    let (g1, g2) = (g('1', 'a'), g('2', 'a'));
    reg(&s, &g2, 2);
    reg(&s, &g('3', 'a'), 3); // newest, so G2 is not pinned by being newest
    let a = session(&env, &s, "aaaaaaaa", &g1);
    let x = s.put_bytes(b"on g1").unwrap();
    cp(&s, &a, &g1, manual(), vec![added("f", &x)]);
    let n = s.begin_move(&a, &repo("app"), &g2, &head('2'), &env.root.join("sessions/s-aaaaaaaa/stage"), t(20)).unwrap();
    assert!(s.set_move_state(&a, &repo("app"), n, MoveState::Preparing, MoveState::Swapping, Some(&[]), t(21)).unwrap());
    s.finish_move(&a, &repo("app"), n, &[], t(22)).unwrap();
    floor0(&s);
    s.gc_repo(&repo("app"), K0).unwrap();
    assert!(has_gen(&env, &g1) && s.base_dir(&g1).exists());
    remove(&s, &a);
    s.gc(FOREVER).unwrap();
    s.gc_repo(&repo("app"), K0).unwrap();
    assert!(has_gen(&env, &g1), "removed session within keep still pins G1 through its checkpoint");
    assert_eq!(s.gc(K0).unwrap().sessions, 1);
    s.gc_repo(&repo("app"), K0).unwrap();
    assert!(!has_gen(&env, &g1) && !s.base_dir(&g1).exists());
}

#[test]
fn gc_reclaims_removed_sessions_after_keep() {
    let (env, s) = setup();
    deps(&s, 'b');
    let g1 = g('1', 'a');
    let a = session(&env, &s, "aaaaaaaa", &g1);
    let x = s.put_bytes(b"only a").unwrap();
    cp(&s, &a, &g1, CheckpointReason::BeforeRemove, vec![added("f", &x), added("g", &x)]);
    let np = NewProposal {
        repo: repo("app"),
        origin: Some(a.clone()),
        from: set_id('a'),
        from_inputs: "1".repeat(64),
        to: set_id('b'),
        to_inputs: "2".repeat(64),
        tree: None,
        head: None,
    };
    s.record_proposal(&np, None, t(30)).unwrap().unwrap();
    remove(&s, &a);
    floor0(&s);
    s.gc(FOREVER).unwrap();
    assert!(s.session(&a).unwrap().is_some());
    assert_eq!(s.checkpoints(&a, None).unwrap().len(), 1);
    assert_eq!(count(&env, "SELECT count(*) FROM deps_proposals"), 1);
    let r = s.gc(K0).unwrap();
    assert!(s.session(&a).unwrap().is_none());
    for tbl in ["workspaces", "seeds", "move_intents", "checkpoints", "overlay_entries", "deps_proposals", "blobs"] {
        assert_eq!(count(&env, &format!("SELECT count(*) FROM {tbl}")), 0, "{tbl}");
    }
    assert_eq!((r.sessions, r.checkpoints, r.overlay_entries, r.proposals, r.blobs), (1, 1, 2, 1, 1));
    assert!(!blob_path(&env, &x).exists());
}

#[test]
fn gc_keeps_newest_base_per_repo() {
    let (env, s) = setup();
    let (g1, g2, g3) = (g('1', 'a'), g('2', 'a'), g('3', 'a'));
    reg(&s, &g2, 2);
    reg(&s, &g3, 3);
    floor0(&s);
    let r = s.gc_repo(&repo("app"), K0).unwrap();
    assert_eq!(r.base_gens, 2);
    assert!(!has_gen(&env, &g1) && !has_gen(&env, &g2));
    assert!(!s.base_dir(&g1).exists() && !s.base_dir(&g2).exists());
    assert!(has_gen(&env, &g3) && s.base_dir(&g3).exists());
    // the newest stays even though nothing references it
    s.gc_repo(&repo("app"), K0).unwrap();
    assert!(has_gen(&env, &g3));
}

#[test]
fn gc_keeps_pending_proposal_deps() {
    let env = env();
    let s = env.open();
    for d in ['a', 'b', 'c', 'd', 'e', 'f'] {
        deps(&s, d);
        mk(&s.deps_dir(&repo("app"), &set_id(d)));
    }
    reg(&s, &g('1', 'f'), 1);
    let live = session(&env, &s, "livelive", &g('1', 'f'));
    let p = |from: char, to: char| NewProposal {
        repo: repo("app"),
        origin: None,
        from: set_id(from),
        from_inputs: "1".repeat(64),
        to: set_id(to),
        to_inputs: "2".repeat(64),
        tree: Some(tree('1')),
        head: Some(head('1')),
    };
    let pending = s.record_proposal(&p('a', 'b'), None, t(30)).unwrap().unwrap();
    let aw = NewProposal { origin: Some(live.clone()), tree: None, head: None, ..p('a', 'c') };
    s.record_proposal(&aw, None, t(30)).unwrap().unwrap();
    let main = DepsMain { repo: repo("app"), tree: tree('1'), head: head('1'), inputs_hash: "3".repeat(64), set: set_id('d'), observed: t(31) };
    assert!(s.set_deps_main(&main, None).unwrap().0);
    floor0(&s);
    s.gc(K0).unwrap();
    let r = s.gc_repo(&repo("app"), K0).unwrap();
    assert_eq!(r.deps_sets, 2, "{r:?}");
    for d in ['b', 'c', 'd', 'f'] {
        assert!(has_set(&env, d) && s.deps_dir(&repo("app"), &set_id(d)).exists(), "{d}");
    }
    for d in ['a', 'e'] {
        assert!(!has_set(&env, d) && !s.deps_dir(&repo("app"), &set_id(d)).exists(), "{d}");
    }
    // decided past keep: the proposal goes, then its to-set
    assert!(s.decide_proposal(pending, ProposalDecision::Applied, t(40)).unwrap());
    assert_eq!(s.gc(K0).unwrap().proposals, 1);
    s.gc_repo(&repo("app"), K0).unwrap();
    assert!(!has_set(&env, 'b'));
    assert!(has_set(&env, 'c') && has_set(&env, 'd'));
}

#[test]
fn gc_reclaims_closed_attempts_keeps_failed_scenes() {
    let env = env();
    let s = env.open();
    let r = repo("app");
    for d in ['a', 'b', 'c'] {
        deps(&s, d);
    }
    let sealed = s.begin_deps_attempt(&r, &set_id('x'), AttemptKind::Build, t(20)).unwrap();
    let set = DepsSet {
        id: set_id('x'),
        repo: r.clone(),
        inputs_hash: "0".repeat(64),
        platform: "darwin-arm64".into(),
        toolchain: "none".into(),
        dirs: vec![],
    };
    s.record_deps_set(&set, DepsOrigin::Build, Some(sealed), t(21)).unwrap();
    let refused = s.begin_deps_attempt(&r, &set_id('y'), AttemptKind::Build, t(22)).unwrap();
    assert!(s.end_deps_attempt(refused, AttemptState::Refused, &interrupted(), t(23)).unwrap());
    let failed = s.begin_deps_attempt(&r, &set_id('z'), AttemptKind::Build, t(24)).unwrap();
    let staging = s.deps_attempts(Some(&r)).unwrap().into_iter().find(|a| a.id == failed).unwrap().staging;
    mk(&staging);
    assert!(s.end_deps_attempt(failed, AttemptState::Failed, &interrupted(), t(25)).unwrap());
    floor0(&s);
    let rep = s.gc(K0).unwrap();
    assert_eq!(rep.attempts, 2);
    let left: Vec<u64> = s.deps_attempts(Some(&r)).unwrap().into_iter().map(|a| a.id).collect();
    assert_eq!(left, vec![failed]);
    assert!(staging.exists());
    fs::remove_dir_all(&staging).unwrap();
    assert_eq!(s.gc(K0).unwrap().attempts, 1);
    assert!(s.deps_attempts(Some(&r)).unwrap().is_empty());
    let next = s.begin_deps_attempt(&r, &set_id('w'), AttemptKind::Adopt, t(30)).unwrap();
    assert!(next > failed, "attempt ids are never reused");
}

#[test]
fn gc_never_deletes_young_blobs() {
    let (env, s) = setup();
    let x = s.put_bytes(b"young").unwrap();
    let bp = blob_path(&env, &x);
    assert_eq!(s.gc(K0).unwrap().blobs, 0);
    assert!(bp.exists());
    old_mtime(&bp);
    assert_eq!(s.gc(K0).unwrap().blobs, 0, "ctime is now");
    assert!(bp.exists());
    s.arm(Seam::AgeFloor { secs: 1 });
    thread::sleep(Duration::from_millis(1100));
    assert_eq!(s.gc(K0).unwrap().blobs, 1);
    assert!(!bp.exists());
    let a = session(&env, &s, "aaaaaaaa", &g('1', 'a'));
    let e = s
        .record_checkpoint(Checkpoint {
            id: CheckpointId::try_from(1u64).unwrap(),
            session: a.clone(),
            repo: repo("app"),
            base: g('1', 'a'),
            at: t(50),
            reason: manual(),
            entries: vec![added("f", &x)],
        })
        .unwrap_err();
    assert_code(&e, Code::Corrupt);
    assert!(s.checkpoints(&a, None).unwrap().is_empty());
}

#[test]
fn gc_spares_in_flight_put_with_old_mtime() {
    let (env, s) = setup();
    let src = env.dir.path().join("src");
    fs::write(&src, b"in flight").unwrap();
    old_mtime(&src);
    s.arm(Seam::PauseInPut);
    thread::scope(|sc| {
        let put = sc.spawn(|| s.put_blob(&src));
        s.wait_paused();
        let (tx, rx) = mpsc::channel();
        let s = &s;
        let gc = sc.spawn(move || {
            let r = s.gc(K0);
            tx.send(()).unwrap();
            r
        });
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "gc must wait for the put's L3");
        s.release();
        let id = put.join().unwrap().unwrap();
        let r = gc.join().unwrap().unwrap();
        assert_eq!((r.tmp_removed, r.blobs), (0, 0));
        assert!(blob_path(&env, &id).exists());
    });
}

#[test]
fn gc_spares_freshly_renamed_generation_dir() {
    let (_env, s) = setup();
    let fresh = g('9', 'a');
    let built = s.root().join("built");
    mk(&built);
    old_mtime(&built);
    let to = s.base_dir(&fresh);
    fs::create_dir_all(to.parent().unwrap()).unwrap();
    fs::rename(&built, &to).unwrap();
    s.gc_repo(&repo("app"), K0).unwrap();
    assert!(to.exists(), "ctime is now: too young");
    s.arm(Seam::AgeFloor { secs: 1 });
    thread::sleep(Duration::from_millis(1100));
    let r = s.gc_repo(&repo("app"), K0).unwrap();
    assert!(!to.exists());
    assert!(r.dirs_retired >= 1);
}

#[test]
fn gc_skips_paths_of_open_intents_and_attempts() {
    let (env, s) = setup();
    reg(&s, &g('2', 'a'), 2);
    reg(&s, &g('3', 'a'), 3);
    let r = repo("app");
    let a = session(&env, &s, "aaaaaaaa", &g('1', 'a'));
    // begin_move only accepts staging under the session root, so an intent
    // can never own a path inside bases/; its staging is simply left alone.
    let odd = env.root.join("sessions/s-aaaaaaaa/.move-stage");
    mk(&odd);
    let n = s.begin_move(&a, &r, &g('2', 'a'), &head('2'), &odd, t(20)).unwrap();
    let running = s.begin_deps_attempt(&r, &set_id('r'), AttemptKind::Build, t(20)).unwrap();
    let failed = s.begin_deps_attempt(&r, &set_id('q'), AttemptKind::Build, t(20)).unwrap();
    let st = |id: u64| s.deps_attempts(Some(&r)).unwrap().into_iter().find(|x| x.id == id).unwrap().staging;
    let (rs, fs_) = (st(running), st(failed));
    mk(&rs);
    mk(&fs_);
    assert!(s.end_deps_attempt(failed, AttemptState::Failed, &interrupted(), t(21)).unwrap());
    floor0(&s);
    s.gc(K0).unwrap();
    s.gc_repo(&r, K0).unwrap();
    for p in [&odd, &rs, &fs_] {
        assert!(p.join("f").exists(), "{}", p.display());
    }
    assert!(has_gen(&env, &g('2', 'a')), "target of an open intent");
    assert!(s.set_move_state(&a, &r, n, MoveState::Preparing, MoveState::Refused, None, t(22)).unwrap());
    s.gc_repo(&r, K0).unwrap();
    assert!(rs.exists() && fs_.exists());
}

#[test]
fn retiring_a_0555_root_reparents_it() {
    let (env, s) = setup();
    deps(&s, 'z');
    let z = s.deps_dir(&repo("app"), &set_id('z'));
    mk(&z);
    chmod(&z, 0o555);
    floor0(&s);
    let r = s.gc_repo(&repo("app"), K0).unwrap();
    assert_eq!(r.deps_sets, 1);
    assert!(!has_set(&env, 'z') && !z.exists());
    assert_eq!(trash(&env).len(), 1);
    s.gc(K0).unwrap();
    assert!(trash(&env).is_empty());
    // an unregistered 0555 root: retire_dir moves it too
    let y = s.deps_dir(&repo("app"), &set_id('y'));
    mk(&y);
    chmod(&y, 0o555);
    let to = s.retire_dir(&y).unwrap();
    assert!(!y.exists() && to.join("f").exists());
}

#[test]
fn retiring_a_uchg_root_clears_the_flag_first() {
    let (env, s) = setup();
    let (g1, g2) = (g('1', 'a'), g('2', 'a'));
    reg(&s, &g2, 2);
    let d1 = s.base_dir(&g1);
    chmod(&d1, 0o555);
    chflags(&d1, UF_IMMUTABLE);
    floor0(&s);
    s.arm(Seam::RetirePrepFails { errno: libc::EPERM });
    let r = s.gc_repo(&repo("app"), K0).unwrap();
    assert!(r.error.is_none());
    assert_eq!(r.stuck.len(), 1, "{r:?}");
    assert!(r.stuck[0].ends_with(d1.strip_prefix(&env.root).unwrap()));
    assert!(has_gen(&env, &g1) && d1.exists());
    assert_ne!(flags(&d1) & UF_IMMUTABLE, 0);
    let r = s.gc_repo(&repo("app"), K0).unwrap();
    assert_eq!(r.base_gens, 1);
    assert!(!has_gen(&env, &g1) && !d1.exists());
    s.gc(K0).unwrap();
    assert!(trash(&env).is_empty());
    // retire_dir on an unregistered uchg root
    let u = s.base_dir(&g('8', 'a'));
    mk(&u);
    chflags(&u, UF_IMMUTABLE);
    s.arm(Seam::RetirePrepFails { errno: libc::EPERM });
    assert_code(&s.retire_dir(&u).unwrap_err(), Code::ReadOnly);
    assert!(u.join("f").exists());
    let to = s.retire_dir(&u).unwrap();
    assert!(!u.exists() && to.join("f").exists());
    assert_eq!(flags(&to) & UF_IMMUTABLE, 0);
}

#[test]
fn gc_rechecks_pins_written_after_its_snapshot() {
    let (env, s) = setup();
    let (g1, g2) = (g('1', 'a'), g('2', 'a'));
    reg(&s, &g2, 2);
    reg(&s, &g('3', 'a'), 3);
    deps(&s, 's');
    let sd = s.deps_dir(&repo("app"), &set_id('s'));
    mk(&sd);
    chmod(&s.base_dir(&g1), 0o555);
    floor0(&s);
    s.arm(Seam::GcPauseBeforeRows);
    let r = repo("app");
    let a = thread::scope(|sc| {
        let h = sc.spawn(|| s.gc_repo(&r, K0));
        s.wait_paused();
        let a = session(&env, &s, "aaaaaaaa", &g1);
        let n = s.begin_move(&a, &r, &g2, &head('2'), &env.root.join("sessions/s-aaaaaaaa/stage"), t(20)).unwrap();
        let main = DepsMain { repo: r.clone(), tree: tree('1'), head: head('1'), inputs_hash: "3".repeat(64), set: set_id('s'), observed: t(31) };
        assert!(s.set_deps_main(&main, None).unwrap().0);
        s.release();
        let rep = h.join().unwrap().unwrap();
        assert!(rep.error.is_none());
        assert_eq!((rep.base_gens, rep.deps_sets), (0, 0), "{rep:?}");
        (a, n, main)
    });
    assert_eq!(outcomes(&env, Some("app")), vec!["complete"]);
    for b in [&g1, &g2] {
        assert!(has_gen(&env, b) && s.base_dir(b).join("f").exists());
    }
    assert!(has_set(&env, 's') && sd.join("f").exists());
    assert!(trash(&env).is_empty());
    assert_ne!(mode(&s.base_dir(&g1)) & 0o200, 0, "prepared roots are not restored");
    let (a, n, main) = a;
    assert!(s.set_move_state(&a, &r, n, MoveState::Preparing, MoveState::Refused, None, t(22)).unwrap());
    let other = DepsMain { set: set_id('a'), ..main.clone() };
    assert!(s.set_deps_main(&other, Some(&main)).unwrap().0);
    let rep = s.gc_repo(&r, K0).unwrap();
    assert_eq!((rep.base_gens, rep.deps_sets), (1, 1), "{rep:?}");
    assert!(!has_gen(&env, &g2) && !has_set(&env, 's'));
    assert!(has_gen(&env, &g1));
}

#[test]
fn gc_refuses_publish_pins_between_batches() {
    let (env, s) = setup();
    let g1 = g('1', 'a');
    let a = session(&env, &s, "aaaaaaaa", &g1);
    let (x, y) = (s.put_bytes(b"x").unwrap(), s.put_bytes(b"y").unwrap());
    let c1 = cp(&s, &a, &g1, manual(), vec![added("f", &x), added("g", &y)]);
    remove(&s, &a);
    s.arm(Seam::GcBatchRows { rows: 2 });
    s.arm(Seam::GcPauseAfterBatch { n: 1 });
    thread::scope(|sc| {
        let h = sc.spawn(|| s.gc(K0));
        s.wait_paused();
        let group = ReleaseGroup { id: 1, batches: vec![Batch { session: a.clone(), repo: repo("app"), checkpoint: c1, paths: vec![rp("f")] }] };
        assert_code(&s.record_publish(&group, t(60)).unwrap_err(), Code::NoSuchSession);
        assert_eq!(count(&env, "SELECT count(*) FROM publishes"), 0);
        assert_eq!(count(&env, "SELECT count(*) FROM publish_items"), 0);
        let e = s.checkpoint(&a, c1).unwrap_err();
        assert_code(&e, Code::Corrupt);
        s.release();
        let r = h.join().unwrap().unwrap();
        assert!(r.error.is_none());
    });
    assert_eq!(outcomes(&env, None), vec!["complete"]);
    assert!(s.session(&a).unwrap().is_none());
    assert_eq!(count(&env, "SELECT count(*) FROM checkpoints"), 0);
    assert_eq!(count(&env, "SELECT count(*) FROM workspaces"), 0);
}

#[test]
fn gc_retires_stale_base_staging() {
    let (env, s) = setup();
    let stg = env.root.join("bases/app/.staging");
    let old = stg.join("0123456789abcdef0123456789abcdef");
    mk(&old.join("tree"));
    mk(&old.join(".export-tree-1234"));
    let bad = stg.join("0123456789abcdef0123456789abcde"); // 31
    mk(&bad);
    // APFS is case-insensitive: pick a name that differs from every lowercase one
    let upper = stg.join("ABCDEF0123456789ABCDEF0123456789");
    mk(&upper);
    let dstg = env.root.join("deps/app/.staging/0123456789abcdef0123456789abcdef");
    mk(&dstg);
    s.arm(Seam::AgeFloor { secs: 1 });
    thread::sleep(Duration::from_millis(1100));
    let fresh = stg.join("fedcba9876543210fedcba9876543210");
    mk(&fresh);
    let r = s.gc_repo(&repo("app"), K0).unwrap();
    assert_eq!(r.dirs_retired, 1, "{r:?}");
    assert!(!old.exists());
    for p in [&bad, &upper, &dstg, &fresh, &stg] {
        assert!(p.exists(), "{}", p.display());
    }
    // the builder's registration rename now fails: nothing appears
    let e = fs::rename(old.join("tree"), env.root.join("bases/app").join(tree('5').as_str())).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
    assert!(!env.root.join("bases/app").join(tree('5').as_str()).exists());
}

#[test]
fn gc_sweeps_stale_temp_entries() {
    let (env, s) = setup();
    let tmp = env.store_dir().join("tmp");
    let clone = tmp.join("put-0123456789abcdef0123456789abcdef");
    fs::write(&clone, b"dead put").unwrap();
    chflags(&clone, UF_IMMUTABLE);
    let initdb = tmp.join("init-0123.db");
    fs::write(&initdb, b"SQLite format 3\0").unwrap();
    let foreign = tmp.join("foreign");
    mk(&foreign);
    let linked = tmp.join("linked");
    fs::write(&linked, b"two names").unwrap();
    let outside = env.dir.path().join("outside-link");
    fs::hard_link(&linked, &outside).unwrap();
    chflags(&linked, UF_IMMUTABLE);
    s.arm(Seam::AgeFloor { secs: 1 });
    thread::sleep(Duration::from_millis(1100));
    let young = tmp.join("put-fedcba9876543210fedcba9876543210");
    fs::write(&young, b"young").unwrap();
    let r = s.gc(K0).unwrap();
    assert_eq!(r.tmp_removed, 3, "{r:?}");
    assert!(!exists(&clone) && !exists(&initdb) && !exists(&foreign));
    assert!(young.exists() && linked.exists());
    assert_eq!(r.stuck.len(), 1);
    assert!(r.stuck[0].ends_with("store/tmp/linked"));
    assert_ne!(flags(&linked) & UF_IMMUTABLE, 0);
    assert_ne!(flags(&outside) & UF_IMMUTABLE, 0);
    chflags(&linked, 0);
}

#[test]
fn gc_deletes_supersede_chains_across_batches() {
    let env = env();
    let s = env.open();
    for d in ['a', 'b', 'c', 'd', 'f'] {
        deps(&s, d);
    }
    reg(&s, &g('1', 'f'), 1);
    let live = session(&env, &s, "livelive", &g('1', 'f'));
    let np = |to: char, origin: Option<SessionId>| NewProposal {
        repo: repo("app"),
        origin: origin.clone(),
        from: set_id('a'),
        from_inputs: "1".repeat(64),
        to: set_id(to),
        to_inputs: "2".repeat(64),
        tree: origin.is_none().then(|| tree('1')),
        head: origin.is_none().then(|| head('1')),
    };
    let p1 = s.record_proposal(&np('b', Some(live.clone())), None, t(30)).unwrap().unwrap();
    let p2 = s.record_proposal(&np('c', None), None, t(31)).unwrap().unwrap();
    // p2 (newer) superseded by p1 (older)
    assert!(s.promote_proposal(p1, &tree('1'), &head('1'), t(32)).unwrap());
    // p1 superseded by p3 (newer)
    let p3 = s.record_proposal(&np('d', None), Some(p1), t(33)).unwrap().unwrap();
    assert!(s.decide_proposal(p3, ProposalDecision::Applied, t(34)).unwrap());
    let all = s.proposals(None, false).unwrap();
    let by: BTreeMap<_, _> = all.iter().map(|p| (p.id, p.superseded_by)).collect();
    assert_eq!(by[&p2], Some(p1));
    assert_eq!(by[&p1], Some(p3));
    s.arm(Seam::GcBatchRows { rows: 1 });
    let r = s.gc(K0).unwrap();
    assert!(r.error.is_none());
    assert_eq!(r.proposals, 3);
    assert_eq!(count(&env, "SELECT count(*) FROM deps_proposals"), 0);
    // a kept (pending) row stays; the decided row pointing at it may go,
    // since nothing kept references it through superseded_by
    let q1 = s.record_proposal(&np('b', None), None, t(40)).unwrap().unwrap();
    let q2 = s.record_proposal(&np('c', None), Some(q1), t(41)).unwrap().unwrap();
    s.gc(K0).unwrap();
    let left: Vec<_> = s.proposals(None, false).unwrap().into_iter().map(|p| p.id).collect();
    assert_eq!(left, vec![q2], "q1 is superseded and past keep; q2 is pending");
    // with a keep no row is past, the superseded row stays as well
    let q3 = s.record_proposal(&np('d', None), Some(q2), t(42)).unwrap().unwrap();
    s.gc(FOREVER).unwrap();
    let left: Vec<_> = s.proposals(None, false).unwrap().into_iter().map(|p| p.id).collect();
    assert_eq!(left, vec![q2, q3]);
}

#[test]
fn symlink_put_never_retakes_the_blob_gate() {
    let (env, s) = setup();
    let link = env.dir.path().join("link");
    symlink("target text", &link).unwrap();
    s.arm(Seam::PauseInPut);
    thread::scope(|sc| {
        let (tx, rx) = mpsc::channel();
        let (s, link) = (&s, &link);
        sc.spawn(move || tx.send(s_put(s, link)).unwrap());
        s.wait_paused();
        let gc = sc.spawn(|| s.gc(FOREVER));
        thread::sleep(Duration::from_millis(200)); // gc queues on L3 exclusive
        s.release();
        let started = Instant::now();
        let id = rx.recv_timeout(Duration::from_secs(1)).expect("put returned within 1 s").unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(id.as_str(), blake3::hash(b"target text").to_hex().as_str());
        gc.join().unwrap().unwrap();
    });
}
fn s_put(s: &Store, p: &Path) -> Result<BlobId> {
    s.put_blob(p)
}

#[test]
fn gc_keeps_open_publish_scenes() {
    let (env, s) = setup();
    let g1 = g('1', 'a');
    let a = session(&env, &s, "aaaaaaaa", &g1);
    let x = s.put_bytes(b"x").unwrap();
    let c1 = cp(&s, &a, &g1, manual(), vec![added("f", &x)]);
    let group = ReleaseGroup { id: 1, batches: vec![Batch { session: a.clone(), repo: repo("app"), checkpoint: c1, paths: vec![rp("f")] }] };
    let p = s.record_publish(&group, t(60)).unwrap();
    remove(&s, &a);
    assert_eq!(s.gc(K0).unwrap().sessions, 0);
    assert!(s.session(&a).unwrap().is_some());
    assert!(s.set_publish_phase(p, PublishPhase::Running, PublishPhase::Unknown, None, t(61)).unwrap());
    assert_eq!(s.gc(K0).unwrap().sessions, 0);
    assert_eq!(s.checkpoint(&a, c1).unwrap().entries.len(), 1);
    assert!(s.set_publish_phase(p, PublishPhase::Unknown, PublishPhase::Done, Some(&serde_json::json!({})), t(62)).unwrap());
    assert_eq!(s.gc(K0).unwrap().sessions, 1);
    assert!(s.session(&a).unwrap().is_none());
    assert_eq!(count(&env, "SELECT count(*) FROM publish_items"), 1, "publish rows are never deleted");
}

#[test]
fn gc_retires_dirs_through_trash_and_resumes() {
    let (env, s) = setup();
    let (g1, g2) = (g('1', 'a'), g('2', 'a'));
    reg(&s, &g2, 2);
    reg(&s, &g('3', 'a'), 3);
    let d1 = s.base_dir(&g1);
    let d2 = s.base_dir(&g2);
    // hard-to-delete contents: a uchg file and a 0555 dir
    fs::write(d1.join("locked"), b"l").unwrap();
    chflags(&d1.join("locked"), UF_IMMUTABLE);
    mk(&d2.join("ro"));
    chmod(&d2.join("ro"), 0o555);
    s.arm(Seam::GcStopAfterRetire { n: 1 });
    // trash only goes after the floor
    s.arm(Seam::AgeFloor { secs: 1 });
    thread::sleep(Duration::from_millis(1100));
    let e = s.gc_repo(&repo("app"), K0).unwrap_err();
    assert_code(&e, Code::Unknown);
    assert_eq!(e.detail.as_ref().unwrap()["outcome"], "partial");
    assert!(!has_gen(&env, &g1) && !has_gen(&env, &g2), "committed batches stay");
    assert_eq!([d1.exists(), d2.exists()].iter().filter(|x| **x).count(), 1);
    let r = s.gc_repo(&repo("app"), K0).unwrap();
    assert_eq!(r.dirs_retired, 1);
    assert!(!d1.exists() && !d2.exists());
    assert_eq!(trash(&env).len(), 2);
    // a symlink in trash pointing outside the root: only the link goes
    let outside = env.dir.path().join("outside");
    mk(&outside);
    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis();
    symlink(&outside, env.store_dir().join(format!("trash/{now_ms}-link"))).unwrap();
    let r = s.gc(K0).unwrap();
    assert_eq!(r.trash_removed, 0, "younger than the floor");
    s.arm(Seam::AgeFloor { secs: 0 });
    s.arm(Seam::GcUnlinkFails { errno: libc::EIO });
    let r = s.gc(K0).unwrap();
    assert!(r.error.is_none());
    assert_eq!(r.stuck.len(), 1, "{r:?}");
    assert_eq!(r.trash_removed, 2);
    let r = s.gc(K0).unwrap();
    assert_eq!(r.trash_removed, 1);
    assert!(trash(&env).is_empty());
    assert!(outside.join("f").exists());
}

#[test]
fn gc_touches_nothing_outside_its_areas() {
    let (env, s) = setup();
    let keep = [
        env.root.join("sessions/s-whatever/app/file"),
        env.root.join("bases/app/.tmp-export/x"),
        env.root.join("bases/app/not-a-key/x"),
        env.root.join("bases/app").join(tree('1').as_str()).join("not-deps/x"),
    ];
    for p in &keep {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"k").unwrap();
    }
    let outside = env.dir.path().join("outside");
    let inner = outside.join(set_id('a').as_str());
    mk(&inner);
    let l1 = env.root.join("bases/app").join(tree('4').as_str());
    symlink(&outside, &l1).unwrap();
    // bases/lib itself is a symlink out of the root
    let outside2 = env.dir.path().join("outside2");
    let inner2 = outside2.join(tree('1').as_str()).join(set_id('a').as_str());
    mk(&inner2);
    symlink(&outside2, env.root.join("bases/lib")).unwrap();
    floor0(&s);
    s.gc(K0).unwrap();
    s.gc_repo(&repo("app"), K0).unwrap();
    s.gc_repo(&repo("lib"), K0).unwrap();
    for p in &keep {
        assert!(p.exists(), "{}", p.display());
    }
    assert!(exists(&l1) && inner.join("f").exists());
    assert!(inner2.join("f").exists());
    assert!(trash(&env).is_empty());
}

#[test]
fn gc_never_reclaims_live_session_with_missing_dirs() {
    let (env, s) = setup();
    let g1 = g('1', 'a');
    let a = session(&env, &s, "aaaaaaaa", &g1);
    let x = s.put_bytes(b"x").unwrap();
    let c1 = cp(&s, &a, &g1, manual(), vec![added("f", &x)]);
    let ws = env.root.join("sessions/s-aaaaaaaa/app");
    mk(&ws);
    fs::remove_dir_all(env.root.join("sessions/s-aaaaaaaa")).unwrap();
    floor0(&s);
    let r = s.gc(K0).unwrap();
    assert_eq!((r.sessions, r.checkpoints, r.blobs), (0, 0, 0));
    s.gc_repo(&repo("app"), K0).unwrap();
    assert!(s.session(&a).unwrap().is_some());
    assert_eq!(s.checkpoint(&a, c1).unwrap().entries.len(), 1);
    assert!(has_gen(&env, &g1));
}

#[test]
fn gc_scrub_reports_corrupt_and_resumes_from_cursor() {
    let (env, s) = setup();
    let g1 = g('1', 'a');
    let a = session(&env, &s, "aaaaaaaa", &g1);
    let mut ids: Vec<BlobId> = (0..4u8).map(|i| s.put_bytes(&vec![b'a' + i; 3000]).unwrap()).collect();
    let entries = ids.iter().enumerate().map(|(i, b)| added(&format!("f{i}"), b)).collect();
    cp(&s, &a, &g1, manual(), entries);
    ids.sort_by(|x, y| x.as_str().cmp(y.as_str()));
    let bad = ids[2].clone();
    corrupt(&blob_path(&env, &bad));
    s.arm(Seam::ScrubBudget { bytes: 4096 });
    let r1 = s.gc(FOREVER).unwrap();
    let r2 = s.gc(FOREVER).unwrap();
    let r3 = s.gc(FOREVER).unwrap();
    assert_eq!((r1.verified, r2.verified, r3.verified), (2, 2, 2));
    assert!(r1.corrupt.is_empty());
    assert_eq!(r2.corrupt, vec![bad.clone()], "found in the run that reached it");
    assert!(r3.corrupt.is_empty(), "wrapped to the start");
    let r4 = s.gc(FOREVER).unwrap();
    assert_eq!(r4.corrupt, vec![bad]);
}

#[test]
fn corrupt_blob_is_reported_never_overwritten() {
    let (env, s) = setup();
    let g1 = g('1', 'a');
    let a = session(&env, &s, "aaaaaaaa", &g1);
    let x = s.put_bytes(b"precious bytes").unwrap();
    cp(&s, &a, &g1, manual(), vec![added("f", &x)]);
    let bp = blob_path(&env, &x);
    corrupt(&bp);
    let broken = fs::read(&bp).unwrap();
    let r = s.gc(FOREVER).unwrap();
    assert_eq!(r.corrupt, vec![x.clone()]);
    assert_eq!(fs::read(&bp).unwrap(), broken);
    assert_eq!(s.put_bytes(b"precious bytes").unwrap(), x);
    assert_eq!(fs::read(&bp).unwrap(), broken, "put never overwrites a stored copy");
    rewrite(&bp, b"precious bytes");
    s.verify_blob(&x).unwrap();
    assert!(s.gc(FOREVER).unwrap().corrupt.is_empty());
}

#[test]
fn gc_report_persisted_for_status() {
    let (env, s) = setup();
    let r1 = s.gc(K0).unwrap();
    assert_eq!(s.last_gc_report(None).unwrap(), Some(r1.clone()));
    let r2 = s.gc_repo(&repo("app"), K0).unwrap();
    assert_eq!(s.last_gc_report(Some(&repo("app"))).unwrap(), Some(r2.clone()));
    assert_eq!(s.last_gc_report(Some(&repo("lib"))).unwrap(), None);
    drop(s);
    let s = env.open();
    assert_eq!(s.last_gc_report(None).unwrap(), Some(r1));
    assert_eq!(s.last_gc_report(Some(&repo("app"))).unwrap(), Some(r2));
    // a run that stops part way is recorded
    reg(&s, &g('2', 'a'), 2);
    floor0(&s);
    s.arm(Seam::GcStopAfterRetire { n: 1 });
    let e = s.gc_repo(&repo("app"), K0).unwrap_err();
    let last = s.last_gc_report(Some(&repo("app"))).unwrap().unwrap();
    assert_eq!(last.error, Some(e));
    assert_eq!(last.base_gens, 1);
    assert_eq!(outcomes(&env, Some("app")), vec!["complete", "partial"]);
    // old records past keep go; the newest per scope stays
    s.gc(K0).unwrap();
    assert_eq!(outcomes(&env, Some("app")), vec!["partial"]);
    assert_eq!(outcomes(&env, None), vec!["complete", "complete"]);
    s.gc(K0).unwrap();
    assert_eq!(outcomes(&env, None), vec!["complete", "complete"]);
}


#[test]
fn gc_trash_removes_only_old_unreferenced_trash() {
    let (env, s) = setup();
    let t_dir = env.store_dir().join("trash");
    fs::create_dir_all(&t_dir).unwrap();
    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis();
    let old = now_ms - 2 * 3600 * 1000;
    let free = t_dir.join(format!("{old}-{}", "1".repeat(32)));
    let young = t_dir.join(format!("{now_ms}-{}", "2".repeat(32)));
    let by_row = t_dir.join(format!("{old}-{}", "3".repeat(32)));
    let by_extra = t_dir.join(format!("{old}-{}", "4".repeat(32)));
    for d in [&free, &young, &by_row, &by_extra] {
        mk(d);
    }
    fs::write(free.join("locked"), b"l").unwrap();
    chflags(&free.join("locked"), UF_IMMUTABLE);
    // a live workspace whose path resolves into a trash entry
    let id = session(&env, &s, "livelive", &g('1', 'a'));
    let ws = env.root.join("sessions").join(id.as_str());
    fs::create_dir_all(&ws).unwrap();
    symlink(&by_row, ws.join("app")).unwrap();
    // a caller path (an alternates target) inside another one
    let extra = vec![by_extra.join("objects")];
    let before = count(&env, "SELECT count(*) FROM gc_runs");
    let r = s.gc_trash(&extra).unwrap();
    assert_eq!(r.removed, 1, "{r:?}");
    assert_eq!(r.young, 1);
    assert!(r.stuck.is_empty(), "{r:?}");
    assert_eq!(r.refused.len(), 2, "{r:?}");
    assert!(!exists(&free));
    assert!(young.exists() && by_row.exists() && by_extra.exists());
    assert_eq!(count(&env, "SELECT count(*) FROM gc_runs"), before, "no gc_runs row");
    // nothing outside trash was touched
    assert!(s.base_dir(&g('1', 'a')).join("f").exists());
    // once nothing names them, they go (the floor still applies to `young`)
    fs::remove_file(ws.join("app")).unwrap();
    let r = s.gc_trash(&[]).unwrap();
    assert_eq!((r.removed, r.young, r.refused.len()), (2, 1, 0), "{r:?}");
    floor0(&s);
    let r = s.gc_trash(&[]).unwrap();
    assert_eq!(r.removed, 1);
    assert!(trash(&env).is_empty());
}
