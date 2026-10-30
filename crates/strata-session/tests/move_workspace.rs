//! session.md §8 Task 5: `move_workspace` (§5.7, §5.7.1), its leftovers in
//! `recover` (§5.8 step 2) and in `status` (§5.5 step 2.9).
//!
//! The parts of the §7 rows that need `remove` (Task 6) are asserted with
//! that task; `resolve_failed_move` (Task 7) parts are asserted here and in
//! tests/resolve_failed_move.rs.

mod support;

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use strata_overlay::{Base, WorkspaceRef};
use strata_session::{MoveReport, MoveResolution, NewReport, SessionSeam, Sessions, StatusWarning, WorkspaceHealth};
use strata_store::{MoveIntent, MoveState};
use strata_types::{BaseGen, CheckpointReason, Code, CommitOid, RepoId, SessionId, StrataError};
use support::*;

// ---------------------------------------------------------------- helpers

fn commit_oid(s: &str) -> CommitOid {
    CommitOid::try_from(s.to_string()).unwrap()
}

fn ws(rep: &NewReport, repo: &str) -> PathBuf {
    rep.manifest.repos[&repo_name(repo)].path.clone()
}

fn id(rep: &NewReport) -> SessionId {
    rep.manifest.id.clone()
}

fn session_root(fx: &Fx, id: &SessionId) -> PathBuf {
    fx.root().join("sessions").join(id.as_str())
}

fn shadow(fx: &Fx, id: &SessionId, repo: &str) -> PathBuf {
    fx.root().join("sessions/.repos").join(id.as_str()).join(format!("{repo}.git"))
}

fn rid(fx: &Fx, repo: &str) -> RepoId {
    fx.gits[&fx.rn(repo)].repo_id()
}

fn trash_count(fx: &Fx) -> usize {
    fs::read_dir(fx.root().join("store/trash")).map(|d| d.count()).unwrap_or(0)
}

/// Commit to main and publish it to origin.
fn push(fx: &Fx, repo: &str, files: &[(&str, &[u8])], msg: &str) -> String {
    let main = fx.main(repo);
    let c = fx.sb.commit(&main, files, msg);
    fx.sb.git(&main, &["push", "-q", "origin", "HEAD:main"]);
    c
}

/// Publish a commit and build its base generation.
fn target(fx: &Fx, s: &Sessions<'_>, repo: &str, files: &[(&str, &[u8])]) -> (BaseGen, CommitOid) {
    let h = commit_oid(&push(fx, repo, files, "upstream"));
    (s.ensure_base(&fx.rn(repo), &h).unwrap(), h)
}

fn ignore(fx: &Fx, repo: &str, extra: &str) {
    let main = fx.main(repo);
    let mut g = fs::read(main.join(".gitignore")).unwrap();
    g.extend_from_slice(extra.as_bytes());
    push(fx, repo, &[(".gitignore", &g)], "ignore");
}

fn new_tool(fx: &Fx, s: &Sessions<'_>) -> NewReport {
    s.new(Some("T-5"), &[fx.rn("tool")]).unwrap()
}

fn mv(fx: &Fx, s: &Sessions<'_>, id: &SessionId, to: &(BaseGen, CommitOid)) -> strata_session::Result<MoveReport> {
    s.move_workspace(id, &rid(fx, "tool"), to.0.clone(), to.1.clone())
}

fn state(fx: &Fx, id: &SessionId, repo: &str) -> strata_types::WorkspaceState {
    fx.store().session(id).unwrap().unwrap().manifest.repos[&fx.rn(repo)].clone()
}

fn generation(fx: &Fx, id: &SessionId, repo: &str) -> u64 {
    fx.store().session(id).unwrap().unwrap().workspaces[&fx.rn(repo)].generation
}

fn intents(fx: &Fx, id: &SessionId) -> Vec<MoveIntent> {
    fx.store().move_intents(id).unwrap()
}

fn intent(fx: &Fx, id: &SessionId, n: u64) -> MoveIntent {
    intents(fx, id).into_iter().find(|i| i.n == n).expect("intent")
}

/// Move stagings left in the session root.
fn stagings(fx: &Fx, id: &SessionId) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(session_root(fx, id))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with(strata_session::MOVE_STAGING_PREFIX))
        .collect();
    v.sort();
    v
}

fn admin(fx: &Fx, id: &SessionId, repo: &str, n: u64) -> PathBuf {
    shadow(fx, id, repo).join("worktrees").join(n.to_string())
}

fn gitfile(ws: &Path) -> String {
    fs::read_to_string(ws.join(".git")).unwrap()
}

fn warnings(s: &Sessions<'_>, id: &SessionId) -> Vec<StatusWarning> {
    s.status(id).unwrap().workspaces[&repo_name("tool")].warnings.clone()
}

fn health(s: &Sessions<'_>, id: &SessionId) -> WorkspaceHealth {
    s.status(id).unwrap().workspaces[&repo_name("tool")].health
}

fn detail(e: &StrataError) -> &serde_json::Map<String, serde_json::Value> {
    e.detail.as_ref().and_then(|d| d.as_object()).expect("detail is an object")
}

fn base_of(fx: &Fx, g: &BaseGen) -> Base {
    let store = fx.store();
    let manifest = store.manifest(&g.repo, &g.tree).unwrap().expect("base manifest");
    Base { r#gen: g.clone(), manifest: Arc::new(manifest), dir: store.base_dir(g) }
}

/// A cold scan of `root` against `g`.
fn cold(fx: &Fx, id: &SessionId, root: &Path, g: &BaseGen) -> Vec<(String, strata_types::EntryKind)> {
    let rn = fx.rn("tool");
    let w = WorkspaceRef {
        session: id,
        repo: &rn,
        root,
        config: &fx.config.repos[&rn],
        git: &fx.gits[&rn],
        store: fx.store(),
    };
    let o = strata_overlay::reconcile_full(&w, &base_of(fx, g)).unwrap();
    let mut v: Vec<_> = o.entries.into_iter().map(|e| (e.path.as_str().to_string(), e.kind)).collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

/// A child process that keeps something open; killed on drop.
struct Holder(Child);

impl Holder {
    fn cwd(dir: &Path) -> Holder {
        Holder(Command::new("/bin/sleep").arg("300").current_dir(dir).stdin(Stdio::null()).spawn().unwrap())
    }

    fn file(f: &Path) -> Holder {
        Holder(
            Command::new("/bin/sleep").arg("300").current_dir("/").stdin(fs::File::open(f).unwrap()).spawn().unwrap(),
        )
    }

    fn pid(&self) -> i32 {
        self.0.id() as i32
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Byte-and-mode picture of a directory tree, with inodes of the top.
fn picture(dir: &Path) -> Vec<Entry> {
    snapshot(dir)
}

fn ino(p: &Path) -> u64 {
    fs::symlink_metadata(p).unwrap().ino()
}

/// What a refusal must leave as it was.
#[derive(Debug, PartialEq)]
struct Untouched {
    ws_ino: u64,
    ws: Vec<Entry>,
    state: strata_types::WorkspaceState,
    generation: u64,
    pins: String,
}

fn untouched(fx: &Fx, id: &SessionId) -> Untouched {
    let st = state(fx, id, "tool");
    Untouched {
        ws_ino: ino(&st.path),
        ws: picture(&st.path),
        generation: generation(fx, id, "tool"),
        state: st,
        pins: fx.pins("tool"),
    }
}

// ---------------------------------------------------------------- §5.7 Phase A

#[test]
fn move_workspace_validates_before_writing() {
    let fx = Fx::new();
    let s = fx.sessions();
    let rep = s.new(Some("T-5"), &[fx.rn("tool")]).unwrap();
    let id = id(&rep);
    let to = target(&fx, &s, "tool", &[("src/new.ts", b"export {}\n")]);
    let front_head = commit_oid(&fx.sb.head(&fx.main("front")));
    let front_base = s.ensure_base(&fx.rn("front"), &front_head).unwrap();

    struct Snap {
        calls: u32,
        intents: usize,
        stagings: usize,
        refs: String,
        trash: usize,
        cps: usize,
        u: Untouched,
    }
    let snap = |fx: &Fx| Snap {
        calls: fx.before.calls(),
        intents: intents(fx, &id).len(),
        stagings: stagings(fx, &id).len(),
        refs: fx.refs("tool"),
        trash: trash_count(fx),
        cps: fx.store().checkpoints(&id, None).unwrap().len(),
        u: untouched(fx, &id),
    };
    let same = |fx: &Fx, a: &Snap, what: &str| {
        let b = snap(fx);
        assert_eq!(a.calls, b.calls, "{what}: no checkpoint");
        assert_eq!(a.cps, b.cps, "{what}: no checkpoint row");
        assert_eq!(a.intents, b.intents, "{what}: no intent");
        assert_eq!(a.stagings, b.stagings, "{what}: no staging");
        assert_eq!(a.refs, b.refs, "{what}: refs");
        assert_eq!(a.trash, b.trash, "{what}: trash");
        assert_eq!(a.u, b.u, "{what}: workspace");
    };
    let expect = |e: StrataError, code: Code, why: Option<&str>, what: &str| {
        assert_eq!(e.code, code, "{what}: {e:?}");
        if let Some(r) = why {
            assert_eq!(reason(&e), Some(r), "{what}: {e:?}");
        }
    };

    let other = commit_oid(&push(&fx, "tool", &[("src/other.ts", b"x\n")], "other"));
    let main = fx.main("tool");
    fx.sb.git(&main, &["checkout", "-q", "--orphan", "lonely"]);
    let lonely = commit_oid(&fx.sb.commit(&main, &[("lonely", b"1\n")], "lonely"));
    fx.sb.git(&main, &["push", "-q", "origin", "lonely"]);
    fx.sb.git(&main, &["checkout", "-q", "-f", "main"]);
    let lonely_tree = strata_types::TreeOid::try_from(fx.sb.tree_of(&main, &lonely.to_string())).unwrap();
    let lg = s.ensure_base_for(&fx.rn("tool"), &lonely_tree, &to.0.deps).unwrap();

    let a = snap(&fx);
    // RepoId not configured.
    let e = s.move_workspace(&id, &RepoId::new(PathBuf::from("/nowhere/x")), to.0.clone(), to.1.clone()).unwrap_err();
    expect(e, Code::NoSuchRepo, None, "unknown repo id");
    same(&fx, &a, "unknown repo id");
    // new_base of another repo.
    let e = s.move_workspace(&id, &rid(&fx, "tool"), front_base.clone(), to.1.clone()).unwrap_err();
    expect(e, Code::InvalidInput, Some("repo_mismatch"), "repo mismatch");
    same(&fx, &a, "repo mismatch");
    // tracked = false.
    let e = s.move_workspace(&id, &rid(&fx, "front"), front_base.clone(), front_head.clone()).unwrap_err();
    expect(e, Code::InvalidInput, Some("untracked_repo"), "untracked");
    same(&fx, &a, "untracked");
    // head's tree is not the base's.
    let e = mv(&fx, &s, &id, &(to.0.clone(), other.clone())).unwrap_err();
    expect(e, Code::InvalidInput, Some("head_tree"), "head tree");
    same(&fx, &a, "head tree");
    // Unsealed deps set.
    let mut unsealed = to.0.clone();
    unsealed.deps = strata_types::DepsSetId::try_from("0".repeat(64)).unwrap();
    let e = mv(&fx, &s, &id, &(unsealed, to.1.clone())).unwrap_err();
    expect(e, Code::InvalidInput, Some("deps_unsealed"), "unsealed");
    same(&fx, &a, "unsealed");
    // Sealed set, unregistered generation: never built here.
    let other_tree = strata_types::TreeOid::try_from(fx.sb.tree_of(&fx.main("tool"), &other.to_string())).unwrap();
    let unbuilt = BaseGen { repo: fx.rn("tool"), tree: other_tree, deps: to.0.deps.clone() };
    let installs = fx.sb.install_count();
    let e = mv(&fx, &s, &id, &(unbuilt.clone(), other.clone())).unwrap_err();
    expect(e, Code::InvalidInput, Some("base_unbuilt"), "unbuilt");
    assert!(!fx.store().has_base_gen(&unbuilt).unwrap(), "no base built");
    assert_eq!(fx.sb.install_count(), installs);
    same(&fx, &a, "unbuilt");
    // No common ancestor.
    let e = mv(&fx, &s, &id, &(lg, lonely.clone())).unwrap_err();
    expect(e, Code::NoMergeBase, None, "no merge base");
    same(&fx, &a, "no merge base");
    // Workspace missing.
    let w = ws(&rep, "tool");
    let aside = w.with_file_name("tool-aside");
    fs::rename(&w, &aside).unwrap();
    let e = mv(&fx, &s, &id, &to).unwrap_err();
    assert_eq!(e.code, Code::NoSuchPath, "{e:?}");
    assert_eq!(intents(&fx, &id).len(), 0);
    assert_eq!(stagings(&fx, &id).len(), 0);
    fs::rename(&aside, &w).unwrap();
    // Shadow missing.
    let sh = shadow(&fx, &id, "tool");
    let sh_aside = sh.with_file_name("tool-aside.git");
    fs::rename(&sh, &sh_aside).unwrap();
    let e = mv(&fx, &s, &id, &to).unwrap_err();
    expect(e, Code::NoSuchPath, Some("shadow_missing"), "shadow missing");
    fs::rename(&sh_aside, &sh).unwrap();
    same(&fx, &a, "shadow missing");
    // A restore scene.
    let restore = session_root(&fx, &id).join(format!("{}tool", strata_overlay::RESTORE_TEMP_PREFIX));
    fs::create_dir_all(restore.join("x")).unwrap();
    let e = mv(&fx, &s, &id, &to).unwrap_err();
    expect(e, Code::Unknown, Some("restore_scene"), "restore scene");
    fs::remove_dir_all(&restore).unwrap();
    same(&fx, &a, "restore scene");
    // Creating / Removing / Unknown sessions.
    let st = fx.store();
    let now = std::time::SystemTime::now();
    assert!(
        st.set_lifecycle(&id, strata_store::Lifecycle::Active, strata_store::Lifecycle::Removing, None, now).unwrap()
    );
    let e = mv(&fx, &s, &id, &to).unwrap_err();
    expect(e, Code::InvalidInput, Some("lifecycle"), "removing");
    let scene = serde_json::json!({ "op": "remove", "phase": "test" });
    assert!(
        st.set_lifecycle(&id, strata_store::Lifecycle::Removing, strata_store::Lifecycle::Unknown, Some(&scene), now)
            .unwrap()
    );
    let e = mv(&fx, &s, &id, &to).unwrap_err();
    assert_eq!(detail(&e)["scene"], scene);
    expect(e, Code::Unknown, Some("session_unknown"), "unknown session");
    assert_eq!(intents(&fx, &id).len(), 0);
    assert_eq!(fx.before.calls(), a.calls);

    // A Creating session: a second session stopped in `new`.
    s.arm(SessionSeam::StopAt { phase: "new.workspace@tool".into() });
    let _ = s.new(Some("T-6"), &[fx.rn("tool")]).unwrap_err();
    let creating = fx
        .store()
        .sessions()
        .unwrap()
        .into_iter()
        .find(|r| r.lifecycle == strata_store::Lifecycle::Creating)
        .expect("a creating session");
    let e = s.move_workspace(&creating.manifest.id, &rid(&fx, "tool"), to.0.clone(), to.1.clone()).unwrap_err();
    expect(e, Code::InvalidInput, Some("lifecycle"), "creating");
}

#[test]
fn move_workspace_refuses_an_unknown_workspace() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = new_tool(&fx, &s);
    let id = id(&rep);
    let to = target(&fx, &s, "tool", &[("src/a.ts", b"a\n")]);
    // Leave the workspace Unknown through an interrupted swap.
    s.arm(SessionSeam::StopAt { phase: "move.pin".into() });
    let _ = mv(&fx, &s, &id, &to).unwrap_err();
    drop(s);
    let s = fx.sessions();
    s.recover().unwrap();
    assert_eq!(health(&s, &id), WorkspaceHealth::Unknown);
    let calls = fx.before.calls();
    let n = intents(&fx, &id).len();
    let to2 = target(&fx, &s, "tool", &[("src/b.ts", b"b\n")]);
    let e = mv(&fx, &s, &id, &to2).unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    assert_eq!(reason(&e), Some("workspace_unknown"));
    assert!(detail(&e)["scene"].is_object(), "the recorded scene: {e:?}");
    assert_eq!(fx.before.calls(), calls);
    assert_eq!(intents(&fx, &id).len(), n);
}

// ---------------------------------------------------------------- §5.7 the move

#[test]
fn move_workspace_swaps_base_and_deps_atomically() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = new_tool(&fx, &s);
    let id = id(&rep);
    let w = ws(&rep, "tool");
    let from = state(&fx, &id, "tool");
    fs::write(w.join("src/mine.ts"), b"mine\n").unwrap();

    // New source and a new dependency set together.
    fx.sb.set_install_script(&DEFAULT_INSTALL.replace("module.exports = 1", "module.exports = 2"));
    let to = target(&fx, &s, "tool", &[("src/up.ts", b"up\n"), ("bun.lock", b"lock 2\n")]);
    assert_ne!(to.0.deps, from.base.deps, "a new dependency set");
    let old_dir = fx.store().base_dir(&from.base);
    let old_pic = picture(&old_dir);
    let calls = fx.before.calls();

    let r = mv(&fx, &s, &id, &to).unwrap();
    assert_eq!(r.from, from.base);
    assert_eq!(r.to, to.0);
    assert_eq!(r.head, to.1);
    assert!(r.old_tree.is_none(), "an idle, unchanged old tree is retired: {r:?}");
    assert!(r.uncarried.is_empty(), "{r:?}");

    // The BeforeRollout checkpoint went through BeforeCheckpoint.
    assert_eq!(fx.before.calls(), calls + 1);
    assert_eq!(fx.before.reasons.lock().unwrap().last(), Some(&CheckpointReason::BeforeRollout));
    let cp = fx.store().latest_checkpoint(&id, &fx.rn("tool")).unwrap().expect("checkpoint");
    assert_eq!(Some(cp.id), r.checkpoint);

    // Source = new base + overlay; deps = the new set.
    assert_eq!(fs::read(w.join("src/up.ts")).unwrap(), b"up\n");
    assert_eq!(fs::read(w.join("src/mine.ts")).unwrap(), b"mine\n");
    assert_eq!(fs::read(w.join("bun.lock")).unwrap(), b"lock 2\n");
    assert_eq!(fs::read_to_string(w.join("node_modules/x/index.js")).unwrap(), "module.exports = 2\n");
    assert!(fs::symlink_metadata(w.join("node_modules")).unwrap().is_dir(), "a real directory");

    // Store, admin dir, HEAD and pin.
    let st = state(&fx, &id, "tool");
    assert_eq!(st.base, to.0);
    assert_eq!(st.head, to.1);
    let n = intents(&fx, &id).last().unwrap().n;
    assert_eq!(intent(&fx, &id, n).state, MoveState::Done);
    assert_eq!(generation(&fx, &id, "tool"), n);
    assert!(gitfile(&w).trim_end().ends_with(&format!("worktrees/{n}")), "{}", gitfile(&w));
    assert!(!admin(&fx, &id, "tool", 1).exists(), "old admin dir retired");
    assert_eq!(fx.sb.git(&w, &["rev-parse", "HEAD"]), to.1.to_string());
    assert!(fx.sb.git_output(&w, &["symbolic-ref", "-q", "HEAD"]).stdout.is_empty(), "detached");
    assert!(fx.pins("tool").contains(&to.1.to_string()), "{}", fx.pins("tool"));
    assert_eq!(fx.sb.git(&w, &["status", "--porcelain", "--", "src/up.ts", "bun.lock"]), "");
    assert!(stagings(&fx, &id).is_empty(), "{:?}", stagings(&fx, &id));
    // The old generation is untouched.
    assert_eq!(picture(&old_dir), old_pic);

    // Moving to where it is: nothing happens.
    let calls = fx.before.calls();
    let r = mv(&fx, &s, &id, &to).unwrap();
    assert!(r.checkpoint.is_none());
    assert_eq!(fx.before.calls(), calls);

    // The proposer's own swap: (old tree, new set) built outside the lock.
    fx.sb.set_install_script(&DEFAULT_INSTALL.replace("module.exports = 1", "module.exports = 3"));
    let h3 = commit_oid(&push(&fx, "tool", &[("bun.lock", b"lock 3\n")], "lock 3"));
    let g3 = s.ensure_base(&fx.rn("tool"), &h3).unwrap();
    let same_tree = s.ensure_base_for(&fx.rn("tool"), &to.0.tree, &g3.deps).unwrap();
    let r = mv(&fx, &s, &id, &(same_tree.clone(), to.1.clone())).unwrap();
    assert_eq!(r.to, same_tree);
    assert_eq!(state(&fx, &id, "tool").head, to.1);
    assert_eq!(fs::read(w.join("bun.lock")).unwrap(), b"lock 2\n", "source unchanged");
    assert_eq!(fs::read_to_string(w.join("node_modules/x/index.js")).unwrap(), "module.exports = 3\n");
}

/// Stopped at each phase, reopened and recovered: never new source with old
/// dependencies.
#[test]
fn move_workspace_stops_are_recovered() {
    for phase in ["move.prepare", "move.swap", "move.pin", "move.record"] {
        let mut fx = Fx::with(&["tool"]);
        let rep = {
            let s = fx.sessions();
            new_tool(&fx, &s)
        };
        let id = id(&rep);
        let w = ws(&rep, "tool");
        let before = state(&fx, &id, "tool");
        let to = {
            let s = fx.sessions();
            fx.sb.set_install_script(&DEFAULT_INSTALL.replace("module.exports = 1", "module.exports = 2"));
            let to = target(&fx, &s, "tool", &[("src/up.ts", b"up\n"), ("bun.lock", b"lock 2\n")]);
            s.arm(SessionSeam::StopAt { phase: phase.into() });
            let e = mv(&fx, &s, &id, &to).unwrap_err();
            assert_eq!(e.code, Code::Unknown, "{phase}: {e:?}");
            to
        };
        let n = intents(&fx, &id).last().unwrap().n;
        let staging = intent(&fx, &id, n).staging;
        fx.close_store();
        fx.reopen_store();
        let s = fx.sessions();
        let rr = s.recover().unwrap();
        // Never mixed: either both old or both new.
        let src_new = w.join("src/up.ts").exists();
        let deps_new = fs::read_to_string(w.join("node_modules/x/index.js")).unwrap() == "module.exports = 2\n";
        assert_eq!(src_new, deps_new, "{phase}: source and deps move together");
        if phase == "move.prepare" {
            assert_eq!(intent(&fx, &id, n).state, MoveState::Abandoned, "{phase}");
            assert_eq!(rr.abandoned.len(), 1, "{phase}: {rr:?}");
            assert!(!staging.exists(), "{phase}: staging retired");
            assert!(!src_new);
            assert_eq!(state(&fx, &id, "tool"), before);
            assert_eq!(health(&s, &id), WorkspaceHealth::Ok);
        } else {
            assert_eq!(rr.failed.len(), 1, "{phase}: {rr:?}");
            assert_eq!(intent(&fx, &id, n).state, MoveState::Unknown, "{phase}");
            assert_eq!(health(&s, &id), WorkspaceHealth::Unknown, "{phase}");
            assert!(w.is_dir() && staging.is_dir(), "{phase}: both trees kept");
            let _ = to;
        }
    }
}

#[test]
fn move_workspace_rechecks_open_files_before_swap() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = new_tool(&fx, &s);
    let id = id(&rep);
    let to = target(&fx, &s, "tool", &[("src/up.ts", b"up\n")]);
    let before = untouched(&fx, &id);
    let trash = trash_count(&fx);
    s.arm(SessionSeam::PauseAt { phase: "move.recheck".into() });
    let (e, holder_pid) = std::thread::scope(|sc| {
        let h = sc.spawn(|| mv(&fx, &s, &id, &to));
        s.wait_paused();
        let st = stagings(&fx, &id);
        assert_eq!(st.len(), 1);
        let holder = Holder::file(&st[0].join("node_modules/x/index.js"));
        let pid = holder.pid();
        // lsof must see it before the recheck runs.
        std::thread::sleep(Duration::from_millis(200));
        s.release();
        let e = h.join().unwrap().unwrap_err();
        drop(holder);
        (e, pid)
    });
    assert_eq!(e.code, Code::OpenFiles, "{e:?}");
    assert!(serde_json::to_string(&e.detail).unwrap().contains(&holder_pid.to_string()));
    assert_eq!(untouched(&fx, &id), before, "workspace untouched");
    assert!(stagings(&fx, &id).is_empty());
    let n = intents(&fx, &id)[0].n;
    assert_eq!(intent(&fx, &id, n).state, MoveState::Refused);
    assert!(!admin(&fx, &id, "tool", n).exists(), "new admin dir retired");
    assert!(trash_count(&fx) >= trash + 2, "staging and admin dir in the trash");
    // The checkpoint stays as history; the next move takes n + 1.
    assert_eq!(fx.store().checkpoints(&id, None).unwrap().len(), 1);
    let r = mv(&fx, &s, &id, &to).unwrap();
    assert_eq!(intents(&fx, &id).last().unwrap().n, n + 1);
    assert!(r.checkpoint.is_some());

    // The first check, before anything is written.
    let held = ws(&rep, "tool").join("node_modules/x/index.js");
    let holder = Holder::file(&held);
    std::thread::sleep(Duration::from_millis(200));
    let to2 = target(&fx, &s, "tool", &[("src/up2.ts", b"up\n")]);
    let n_before = intents(&fx, &id).len();
    let e = mv(&fx, &s, &id, &to2).unwrap_err();
    assert_eq!(e.code, Code::OpenFiles, "{e:?}");
    assert_eq!(intents(&fx, &id).len(), n_before, "no intent");
    drop(holder);
}

#[test]
fn move_workspace_projects_overlay_and_marks_conflicts() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let lines = "1\n2\n3\n4\n5\n6\n7\n8\n9\n";
    push(&fx, "tool", &[("a.txt", lines.as_bytes()), ("c.txt", b"c\n")], "texts");
    let rep = new_tool(&fx, &s);
    let id = id(&rep);
    let w = ws(&rep, "tool");
    fs::write(w.join("a.txt"), lines.replace("1\n", "1s\n")).unwrap();
    fs::write(w.join("c.txt"), b"session\n").unwrap();
    fs::write(w.join("pub.txt"), b"P\n").unwrap();
    fs::write(w.join("run.sh"), b"#!/bin/sh\n").unwrap();
    fs::set_permissions(w.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    symlink("target-bytes", w.join("ln")).unwrap();

    let to = target(
        &fx,
        &s,
        "tool",
        &[("a.txt", lines.replace("9\n", "9u\n").as_bytes()), ("c.txt", b"upstream\n"), ("pub.txt", b"P\n")],
    );
    let r = mv(&fx, &s, &id, &to).unwrap();
    assert_eq!(r.dropped, rps(&["pub.txt"]), "{r:?}");
    assert_eq!(r.conflicts, rps(&["c.txt"]), "{r:?}");
    assert_eq!(fs::read_to_string(w.join("a.txt")).unwrap(), lines.replace("1\n", "1s\n").replace("9\n", "9u\n"));
    assert_eq!(fs::read(w.join("c.txt")).unwrap(), b"session\n", "the session's version");
    assert_eq!(fs::metadata(w.join("run.sh")).unwrap().mode() & 0o111, 0o111);
    assert!(fs::symlink_metadata(w.join("ln")).unwrap().file_type().is_symlink());
    assert_eq!(fs::read_link(w.join("ln")).unwrap(), PathBuf::from("target-bytes"));
    let st = s.status(&id).unwrap();
    assert_eq!(st.workspaces[&fx.rn("tool")].conflicts, rps(&["c.txt"]));
    assert_eq!(intents(&fx, &id).last().unwrap().conflicts, Some(rps(&["c.txt"])));
    // A cold scan after the swap equals the rebase result.
    let scan: BTreeSet<String> = cold(&fx, &id, &w, &to.0).into_iter().map(|x| x.0).collect();
    let want: BTreeSet<String> = ["a.txt", "c.txt", "ln", "run.sh"].into_iter().map(String::from).collect();
    assert_eq!(scan, want);
}

fn blob_count(fx: &Fx) -> usize {
    fn count(p: &Path) -> usize {
        let Ok(rd) = fs::read_dir(p) else {
            return 0;
        };
        rd.flatten().map(|e| if e.file_type().unwrap().is_dir() { count(&e.path()) } else { 1 }).sum()
    }
    count(&fx.root().join("store/blobs"))
}

fn readlink_bytes(p: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    fs::read_link(p).unwrap().as_os_str().as_bytes().to_vec()
}

fn listing(dir: &Path) -> BTreeSet<String> {
    fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect()
}

const CARRY_IGNORES: &str =
    ".env\nlocal.txt\nfifo\nsecret\nsrc/gen-link\nconf/\ndist/\nempty/\nlink\na.out\nb.out\nflagged\n";

/// A fixture whose `tool` also seeds `node_modules/.cache`, with the
/// carry test's ignore rules published.
fn carry_fx() -> Fx {
    let mut fx = Fx::with(&["tool"]);
    fx.cfg_mut("tool").seed_caches.push(rp("node_modules/.cache"));
    ignore(&fx, "tool", CARRY_IGNORES);
    let main = fx.main("tool");
    fs::write(main.join(".env"), b"SECRET=1\n").unwrap();
    fs::create_dir_all(main.join(".cache")).unwrap();
    fs::write(main.join(".cache/c.bin"), b"cache\n").unwrap();
    fx
}

#[test]
fn move_workspace_carries_ignored_content() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let fx = carry_fx();
    let s = fx.sessions();
    let rep = new_tool(&fx, &s);
    let id = id(&rep);
    let w = ws(&rep, "tool");
    assert!(w.join(".env").is_file() && w.join(".cache/c.bin").is_file(), "seeded");
    let outside = fx.sb.path("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(
        w.join("dist/a.js")
            .parent()
            .map(|d| {
                fs::create_dir_all(d).unwrap();
                d.join("a.js")
            })
            .unwrap(),
        b"built\n",
    )
    .unwrap();
    fs::create_dir_all(w.join("empty")).unwrap();
    symlink("../somewhere", w.join("link")).unwrap();
    symlink(OsStr::from_bytes(b"gen-\xff-target"), w.join("src/gen-link")).unwrap();
    fs::write(w.join("local.txt"), b"mine\n").unwrap();
    fs::create_dir_all(w.join("conf")).unwrap();
    fs::write(w.join("conf/x"), b"conf\n").unwrap();
    mkfifo(&w.join("fifo"));
    fs::write(w.join("secret"), b"s\n").unwrap();
    fs::set_permissions(w.join("secret"), fs::Permissions::from_mode(0o000)).unwrap();
    fs::create_dir_all(w.join("node_modules/.cache")).unwrap();
    fs::write(w.join("node_modules/.cache/n.bin"), b"nested\n").unwrap();

    // The new base: a tracked local.txt, and conf as a tracked symlink out.
    let main = fx.main("tool");
    fs::write(main.join("local.txt"), b"tracked\n").unwrap();
    symlink(&outside, main.join("conf")).unwrap();
    fx.sb.git(&main, &["add", "-f", "local.txt", "conf"]);
    fx.sb.git(&main, &["commit", "-q", "-m", "tracked"]);
    fx.sb.git(&main, &["push", "-q", "origin", "HEAD:main"]);
    let h = commit_oid(&fx.sb.head(&main));
    let to = (s.ensure_base(&fx.rn("tool"), &h).unwrap(), h);

    let blobs = blob_count(&fx);
    let r = mv(&fx, &s, &id, &to).unwrap();
    assert_eq!(blob_count(&fx), blobs, "carried content never enters the blob store");
    let w2 = state(&fx, &id, "tool").path;
    assert_eq!(w2, w);
    assert_eq!(fs::read(w.join(".env")).unwrap(), b"SECRET=1\n");
    assert_eq!(fs::read(w.join(".cache/c.bin")).unwrap(), b"cache\n");
    assert_eq!(fs::read(w.join("node_modules/.cache/n.bin")).unwrap(), b"nested\n");
    assert_eq!(fs::read(w.join("dist/a.js")).unwrap(), b"built\n");
    assert!(w.join("empty").is_dir() && listing(&w.join("empty")).is_empty());
    assert_eq!(readlink_bytes(&w.join("link")), b"../somewhere");
    assert!(fs::symlink_metadata(w.join("src/gen-link")).unwrap().file_type().is_symlink());
    assert_eq!(readlink_bytes(&w.join("src/gen-link")), b"gen-\xff-target", "recreated byte for byte");
    assert_eq!(fs::read(w.join("local.txt")).unwrap(), b"tracked\n", "carry never overwrites");
    assert!(fs::symlink_metadata(w.join("conf")).unwrap().file_type().is_symlink());
    assert!(listing(&outside).is_empty(), "nothing written through the tracked symlink");
    assert!(!w.join("fifo").exists() && fs::symlink_metadata(w.join("secret")).is_err());
    let unc: Vec<&str> = r.uncarried.iter().map(|p| p.as_str()).collect();
    for p in ["fifo", "local.txt", "secret"] {
        assert!(unc.contains(&p), "{p} in {unc:?}");
    }
    assert!(unc.iter().any(|p| *p == "conf" || p.starts_with("conf/")), "{unc:?}");
    assert!(!listing(&w).iter().any(|n| n.starts_with(".strata-carry-")));
    let old = r.old_tree.clone().expect("kept: uncarried content");
    assert_eq!(fs::read(old.join("local.txt")).unwrap(), b"mine\n");
    let retained = warnings(&s, &id).into_iter().find_map(|x| match x {
        StatusWarning::OldTreeKept { retained, .. } => Some(retained),
        _ => None,
    });
    assert_eq!(retained, Some(true));
    let to2 = target(&fx, &s, "tool", &[("src/up.ts", b"up\n")]);
    mv(&fx, &s, &id, &to2).unwrap();
    assert!(old.is_dir(), "a retained old tree is never collected by a later move");
    fs::set_permissions(old.join("secret"), fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn move_workspace_carries_ignored_content_races() {
    let fx = carry_fx();
    let s = fx.sessions();
    let rep = new_tool(&fx, &s);
    let id = id(&rep);
    let w = ws(&rep, "tool");
    let outside = fx.sb.path("outside");
    fs::create_dir_all(outside.join("src")).unwrap();
    symlink("from-outside", outside.join("src/gen-link")).unwrap();
    fs::write(w.join("a.out"), b"a\n").unwrap();
    fs::write(w.join("b.out"), b"b\n").unwrap();
    symlink("gen-target", w.join("src/gen-link")).unwrap();
    let to = target(&fx, &s, "tool", &[("src/up.ts", b"up\n")]);
    s.arm(SessionSeam::PauseAt { phase: "move.carry".into() });
    let r = std::thread::scope(|sc| {
        let h = sc.spawn(|| mv(&fx, &s, &id, &to));
        s.wait_paused();
        fs::remove_file(w.join("a.out")).unwrap();
        fs::create_dir(w.join("a.out")).unwrap();
        fs::remove_file(w.join("b.out")).unwrap();
        fs::rename(w.join("src"), w.join("src-aside")).unwrap();
        symlink(outside.join("src"), w.join("src")).unwrap();
        s.release();
        h.join().unwrap().unwrap()
    });
    let unc: Vec<&str> = r.uncarried.iter().map(|p| p.as_str()).collect();
    assert!(unc.contains(&"a.out") && unc.contains(&"src/gen-link"), "{unc:?}");
    assert!(!unc.contains(&"b.out"), "{unc:?}");
    assert!(!w.join("a.out").exists() && !w.join("b.out").exists());
    assert!(fs::symlink_metadata(w.join("src/gen-link")).is_err(), "nothing from outside the session");
    assert!(!listing(&w).iter().any(|n| n.starts_with(".strata-carry-")));
    let old = r.old_tree.expect("kept");
    assert!(old.join("a.out").is_dir());
    assert_eq!(listing(&outside.join("src")), BTreeSet::from(["gen-link".to_string()]));
}

#[test]
fn move_workspace_refuses_a_uchg_ignored_file() {
    let fx = carry_fx();
    let s = fx.sessions();
    let rep = new_tool(&fx, &s);
    let id = id(&rep);
    let w = ws(&rep, "tool");
    fs::write(w.join("flagged"), b"f\n").unwrap();
    let st = Command::new("/usr/bin/chflags").arg("uchg").arg(w.join("flagged")).status().unwrap();
    assert!(st.success());
    let to = target(&fx, &s, "tool", &[("src/up.ts", b"up\n")]);
    let before = untouched(&fx, &id);
    let e = mv(&fx, &s, &id, &to).unwrap_err();
    let clear = || {
        let _ = Command::new("/usr/bin/chflags").arg("-R").arg("nouchg").arg(fx.root()).status();
    };
    clear();
    assert_eq!(e.code, Code::ReadOnly, "{e:?}");
    let path = detail(&e).get("path").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    assert!(path.ends_with("flagged"), "detail.path names the entry: {e:?}");
    let last = intents(&fx, &id).last().unwrap().clone();
    assert_eq!(last.state, MoveState::Refused);
    assert!(stagings(&fx, &id).is_empty(), "staging retired");
    assert_eq!(untouched(&fx, &id), before);
}

#[test]
fn move_workspace_keeps_old_tree_while_in_use() {
    for changed in [true, false] {
        let fx = Fx::with(&["tool"]);
        let s = fx.sessions();
        let rep = new_tool(&fx, &s);
        let id = id(&rep);
        let w = ws(&rep, "tool");
        let holder = Holder::cwd(&w.join("src"));
        std::thread::sleep(Duration::from_millis(100));
        let to = target(&fx, &s, "tool", &[("src/up.ts", b"up\n")]);
        let r = mv(&fx, &s, &id, &to).unwrap();
        let old = r.old_tree.clone().expect("kept while in use");
        assert!(old.file_name().unwrap().to_string_lossy().starts_with(".strata-tmp-move-tool-"), "{}", old.display());
        assert_eq!(old.parent().unwrap(), session_root(&fx, &id));
        assert!(old.join("src/index.ts").is_file() && !old.join("src/up.ts").exists(), "the old tree as it was");
        let n = intents(&fx, &id).last().unwrap().n;
        let kept = |s: &Sessions<'_>| {
            warnings(s, &id).into_iter().find_map(|x| match x {
                StatusWarning::OldTreeKept { path, n: k, holders, changed, retained, .. } => {
                    Some((path, k, holders, changed, retained))
                }
                _ => None,
            })
        };
        let (path, k, holders, ch, retained) = kept(&s).expect("OldTreeKept");
        assert_eq!((path.clone(), k, ch, retained), (old.clone(), n, Some(false), false));
        assert!(holders.iter().any(|h| h.pid == holder.pid()), "{holders:?}");

        if changed {
            fs::write(old.join("src/late.ts"), b"late\n").unwrap();
            let (_, _, _, ch, _) = kept(&s).unwrap();
            assert_eq!(ch, Some(true), "a write into the old tree reads as changed");
        }
        drop(holder);
        let to2 = target(&fx, &s, "tool", &[("src/up2.ts", b"up\n")]);
        let r2 = mv(&fx, &s, &id, &to2).unwrap();
        assert!(r2.old_tree.is_none(), "{r2:?}");
        if changed {
            assert!(old.is_dir(), "a changed old tree is never collected");
            assert!(kept(&s).is_some());
        } else {
            assert!(!old.exists(), "an idle unchanged old tree goes on the next move");
            assert!(!admin(&fx, &id, "tool", 1).exists(), "with its admin dir");
            assert!(kept(&s).is_none());
        }
    }
}

/// Review P1-14 (GS-01): an ignored file edited in the old tree after the
/// carry took it (before the swap) is not in the new tree; the old tree
/// must be kept, not retired. Both a carried file and a file inside a
/// directory carried whole; and, as the control, no edit → retired.
#[test]
fn move_workspace_keeps_old_tree_with_a_late_ignored_edit() {
    for late in [None, Some("late.env"), Some("build/out.txt")] {
        let fx = Fx::with(&["tool"]);
        let s = fx.sessions();
        ignore(&fx, "tool", "late.env\nbuild/\n");
        let rep = new_tool(&fx, &s);
        let id = id(&rep);
        let w = ws(&rep, "tool");
        fs::write(w.join("late.env"), b"v1\n").unwrap();
        fs::create_dir_all(w.join("build")).unwrap();
        fs::write(w.join("build/out.txt"), b"v1\n").unwrap();
        let to = target(&fx, &s, "tool", &[("src/up.ts", b"up\n")]);
        s.arm(SessionSeam::PauseAt { phase: "move.recheck".into() });
        let r = std::thread::scope(|sc| {
            let h = sc.spawn(|| mv(&fx, &s, &id, &to));
            s.wait_paused();
            if let Some(p) = late {
                fs::write(w.join(p), b"v2\n").unwrap();
            }
            s.release();
            h.join().unwrap().unwrap()
        });
        assert!(r.uncarried.is_empty(), "{r:?}");
        let Some(p) = late else {
            assert!(r.old_tree.is_none(), "nothing late: the old tree goes ({r:?})");
            assert_eq!(fs::read(w.join("late.env")).unwrap(), b"v1\n");
            continue;
        };
        let old = r.old_tree.clone().unwrap_or_else(|| panic!("{p}: the old tree is kept ({r:?})"));
        assert_eq!(fs::read(old.join(p)).unwrap(), b"v2\n", "{p}: the late edit survives in the old tree");
        assert_eq!(fs::read(w.join(p)).unwrap(), b"v1\n", "{p}: the new tree has what the carry took");
        let n = intents(&fx, &id).last().unwrap().n;
        assert_eq!(intent(&fx, &id, n).old_tree.as_deref(), Some(old.as_path()), "{p}: retained");
        let kept = warnings(&s, &id).into_iter().find_map(|x| match x {
            StatusWarning::OldTreeKept { changed, retained, .. } => Some((changed, retained)),
            _ => None,
        });
        assert_eq!(kept, Some((Some(true), true)), "{p}");
        // A later move does not collect it either.
        let to2 = target(&fx, &s, "tool", &[("src/up2.ts", b"up\n")]);
        mv(&fx, &s, &id, &to2).unwrap();
        assert!(old.join(p).is_file(), "{p}: still kept after the next move");
    }
}

#[test]
#[ignore = "timing on a full-size tree (§7 move_workspace_seconds_warm)"]
fn move_workspace_seconds_warm() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let main = fx.main("tool");
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for i in 0..4200 {
        files.push((format!("src/d{}/f{i}.ts", i % 60), format!("export const v{i} = {i};\n").into_bytes()));
    }
    let refs: Vec<(&str, &[u8])> = files.iter().map(|(p, b)| (p.as_str(), b.as_slice())).collect();
    push(&fx, "tool", &refs, "big");
    ignore(&fx, "tool", "target/\n");
    let rep = new_tool(&fx, &s);
    let w = ws(&rep, "tool");
    for d in 0..300 {
        let dir = w.join(format!("target/debug/d{d}"));
        fs::create_dir_all(&dir).unwrap();
        for f in 0..1000 {
            fs::write(dir.join(format!("o{f}")), b"o").unwrap();
        }
    }
    let to = target(&fx, &s, "tool", &[("src/up.ts", b"up\n")]);
    let _ = main;
    let t = Instant::now();
    mv(&fx, &s, &id(&rep), &to).unwrap();
    let el = t.elapsed();
    eprintln!("move_workspace: {el:?}");
    assert!(el < Duration::from_secs(5), "{el:?}");
}

// ---------------------------------------------------------------- recover and status

#[test]
fn recover_abandons_pre_visible_move_intents_after_a_failure() {
    // (a) An unclassified failure in prepare: Unknown, a leftover, collected
    // by the next move.
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = new_tool(&fx, &s);
    let id = id(&rep);
    let to = target(&fx, &s, "tool", &[("src/up.ts", b"up\n")]);
    s.arm(SessionSeam::FailAt { phase: "move.prepare".into(), errno: libc::EIO });
    let e = mv(&fx, &s, &id, &to).unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    assert_eq!(detail_str(&e, "phase"), Some("prepare"));
    let n = intents(&fx, &id)[0].n;
    let staging = intent(&fx, &id, n).staging;
    let kept: Vec<PathBuf> = serde_json::from_value(detail(&e)["kept"].clone()).unwrap();
    assert_eq!(kept, vec![staging.clone()]);
    assert_eq!(intent(&fx, &id, n).state, MoveState::Preparing);
    assert_eq!(health(&s, &id), WorkspaceHealth::Ok);
    assert!(
        warnings(&s, &id).contains(&StatusWarning::MoveLeftover { n, staging: staging.clone() }),
        "{:?}",
        warnings(&s, &id)
    );
    mv(&fx, &s, &id, &to).unwrap();
    assert_eq!(intent(&fx, &id, n).state, MoveState::Abandoned);
    assert_eq!(intents(&fx, &id).last().unwrap().n, n + 1);
    assert!(!warnings(&s, &id).iter().any(|w| matches!(w, StatusWarning::MoveLeftover { .. })));

    // (c) A named failure in prepare: refused on the spot.
    let to2 = target(&fx, &s, "tool", &[("src/up2.ts", b"up\n")]);
    s.arm(SessionSeam::FailAt { phase: "move.prepare".into(), errno: libc::ENOTDIR });
    let e = mv(&fx, &s, &id, &to2).unwrap_err();
    assert_eq!(e.code, Code::NotDirectory, "{e:?}");
    let last = intents(&fx, &id).last().unwrap().clone();
    assert_eq!(last.state, MoveState::Refused);
    assert!(!last.staging.exists());
    assert!(!warnings(&s, &id).iter().any(|w| matches!(w, StatusWarning::MoveLeftover { .. })));
}

#[test]
fn recover_abandons_pre_visible_move_intents_after_a_stop() {
    // (b)
    let mut fx = Fx::with(&["tool"]);
    let (id, to) = {
        let s = fx.sessions();
        let rep = new_tool(&fx, &s);
        let to = target(&fx, &s, "tool", &[("src/up.ts", b"up\n")]);
        s.arm(SessionSeam::StopAt { phase: "move.carry".into() });
        let e = mv(&fx, &s, &id(&rep), &to).unwrap_err();
        assert_eq!(e.code, Code::Unknown);
        (id(&rep), to)
    };
    let before = state(&fx, &id, "tool");
    let n = intents(&fx, &id)[0].n;
    let staging = intent(&fx, &id, n).staging;
    assert!(staging.is_dir(), "a stop leaves the staging");
    let trash = trash_count(&fx);
    fx.close_store();
    fx.reopen_store();
    let s = fx.sessions();
    // Not running here any more: status reports the leftover.
    assert!(warnings(&s, &id).contains(&StatusWarning::MoveLeftover { n, staging: staging.clone() }));
    let rr = s.recover().unwrap();
    assert_eq!(rr.abandoned.len(), 1, "{rr:?}");
    assert_eq!(rr.abandoned[0].n, n);
    assert_eq!(intent(&fx, &id, n).state, MoveState::Abandoned);
    assert!(!staging.exists());
    assert!(trash_count(&fx) > trash);
    assert_eq!(state(&fx, &id, "tool"), before);
    assert!(warnings(&s, &id).is_empty(), "{:?}", warnings(&s, &id));
    mv(&fx, &s, &id, &to).unwrap();
}

/// (d) The shadow's `worktrees` replaced by a symlink or a file.
#[test]
fn recover_abandons_pre_visible_move_intents_quarantines_worktrees() {
    for as_symlink in [true, false] {
        let fx = Fx::with(&["tool"]);
        let s = fx.sessions();
        let rep = new_tool(&fx, &s);
        let id = id(&rep);
        let w = ws(&rep, "tool");
        let main_git = fx.main("tool").join(".git");
        fs::create_dir_all(main_git.join("worktrees")).unwrap();
        fs::write(main_git.join("worktrees/keep"), b"k").unwrap();
        let wt = shadow(&fx, &id, "tool").join("worktrees");
        let aside = wt.with_file_name("worktrees-real");
        let to = target(&fx, &s, "tool", &[("src/up.ts", b"up\n")]);
        fs::create_dir_all(main_git.join("worktrees")).unwrap();
        fs::write(main_git.join("worktrees/keep"), b"k").unwrap();
        let main_pic = picture(&main_git.join("worktrees"));
        let before = untouched(&fx, &id);
        // Replaced during the move, after the workspace's own index was
        // read (before the move, the workspace's gitfile would dangle and
        // the move would refuse earlier): attach meets it.
        s.arm(SessionSeam::PauseAt { phase: "move.carry".into() });
        let e = std::thread::scope(|sc| {
            let h = sc.spawn(|| mv(&fx, &s, &id, &to));
            s.wait_paused();
            fs::rename(&wt, &aside).unwrap();
            if as_symlink {
                symlink(main_git.join("worktrees"), &wt).unwrap();
            } else {
                fs::write(&wt, b"not a dir").unwrap();
            }
            s.release();
            h.join().unwrap().unwrap_err()
        });
        assert_eq!(e.code, Code::NotDirectory, "{e:?}");
        let last = intents(&fx, &id).last().unwrap().clone();
        assert_eq!(last.state, MoveState::Refused);
        assert!(!last.staging.exists());
        assert_eq!(picture(&main_git.join("worktrees")), main_pic, "never followed");
        assert_eq!(untouched(&fx, &id), before);
        assert!(fs::symlink_metadata(&wt).is_err(), "not left in place");
        let q: Vec<String> = fs::read_dir(shadow(&fx, &id, "tool").join("strata-quarantine"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(q.len(), 1);
        assert!(q[0].starts_with("worktrees-"), "{q:?}");
        let qd = detail(&e).get("quarantined").and_then(|v| v.as_str()).map(PathBuf::from);
        assert!(qd.is_some_and(|p| p.ends_with(&q[0])), "{e:?}");
        // The workspace's own admin dir is gone with the real `worktrees`;
        // put it back, the next move rebuilds `worktrees/` and succeeds.
        fs::rename(&aside, &wt).unwrap();
        mv(&fx, &s, &id, &to).unwrap();
        assert!(fx.sb.git(&w, &["rev-parse", "HEAD"]) == to.1.to_string());
    }
}

/// (e) attach paused before its index; `worktrees/<n>` tampered with.
#[test]
fn recover_abandons_pre_visible_move_intents_attach_exists_and_quarantine() {
    for how in ["index", "symlink", "file"] {
        let fx = Fx::with(&["tool"]);
        let s = fx.sessions();
        let rep = new_tool(&fx, &s);
        let id = id(&rep);
        let main_git = fx.main("tool").join(".git");
        let to = target(&fx, &s, "tool", &[("src/up.ts", b"up\n")]);
        let main_pic = picture(&main_git);
        let before = untouched(&fx, &id);
        let git = &fx.gits[&fx.rn("tool")];
        let pause = git.pause_attach_before_index();
        let e = std::thread::scope(|sc| {
            let h = sc.spawn(|| mv(&fx, &s, &id, &to));
            pause.wait_until_paused();
            let n = intents(&fx, &id).last().unwrap().n;
            let a = admin(&fx, &id, "tool", n);
            match how {
                "index" => fs::write(a.join("index"), b"x").unwrap(),
                "symlink" => {
                    fs::rename(&a, a.with_file_name("moved")).unwrap();
                    symlink(&main_git, &a).unwrap();
                }
                _ => {
                    fs::rename(&a, a.with_file_name("moved")).unwrap();
                    fs::write(&a, b"file").unwrap();
                }
            }
            pause.release();
            h.join().unwrap().unwrap_err()
        });
        let last = intents(&fx, &id).last().unwrap().clone();
        let n = last.n;
        let want = if how == "index" { Code::Exists } else { Code::NotDirectory };
        assert_eq!(e.code, want, "{how}: {e:?}");
        assert_eq!(last.state, MoveState::Refused, "{how}");
        assert!(!last.staging.exists(), "{how}: staging retired");
        assert!(fs::symlink_metadata(admin(&fx, &id, "tool", n)).is_err(), "{how}: not left in worktrees/");
        if how != "index" {
            let q: Vec<String> = fs::read_dir(shadow(&fx, &id, "tool").join("strata-quarantine"))
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            assert_eq!(q, vec![format!("{n}-{}", &q[0][format!("{n}-").len()..])], "{how}");
        }
        assert_eq!(picture(&main_git), main_pic, "{how}: main .git unchanged");
        assert_eq!(untouched(&fx, &id), before, "{how}");
        assert!(!warnings(&s, &id).iter().any(|w| matches!(w, StatusWarning::MoveLeftover { .. })), "{how}");
        let _ = fs::remove_dir_all(admin(&fx, &id, "tool", 0).with_file_name("moved"));
        mv(&fx, &s, &id, &to).unwrap();
        assert_eq!(intents(&fx, &id).last().unwrap().n, n + 1, "{how}");
    }
}

/// (f) attach paused before its readback; S moved away and back.
#[test]
fn recover_abandons_pre_visible_move_intents_staging_changed() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = new_tool(&fx, &s);
    let id = id(&rep);
    let to = target(&fx, &s, "tool", &[("src/up.ts", b"up\n")]);
    let git = &fx.gits[&fx.rn("tool")];
    let pause = git.pause_attach_before_readback();
    let e = std::thread::scope(|sc| {
        let h = sc.spawn(|| mv(&fx, &s, &id, &to));
        pause.wait_until_paused();
        let st = stagings(&fx, &id)[0].clone();
        let away = st.with_file_name("away");
        fs::rename(&st, &away).unwrap();
        fs::rename(&away, &st).unwrap();
        pause.release();
        h.join().unwrap().unwrap_err()
    });
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    assert_eq!(detail_str(&e, "phase"), Some("prepare"));
    let cause = &detail(&e)["cause"];
    assert_eq!(cause["detail"]["reason"], "staging-changed", "{e:?}");
    assert_eq!(health(&s, &id), WorkspaceHealth::Ok);
    let n = intents(&fx, &id)[0].n;
    assert!(warnings(&s, &id).iter().any(|w| matches!(w, StatusWarning::MoveLeftover { n: k, .. } if *k == n)));
    mv(&fx, &s, &id, &to).unwrap();
    assert_eq!(intent(&fx, &id, n).state, MoveState::Abandoned);
}

#[test]
fn status_tells_running_moves_from_interrupted_ones() {
    let mut fx = Fx::with(&["tool"]);
    let (id, to) = {
        let s = fx.sessions();
        let rep = new_tool(&fx, &s);
        let id = id(&rep);
        let to = target(&fx, &s, "tool", &[("src/up.ts", b"up\n")]);
        let to2 = target(&fx, &s, "tool", &[("src/up2.ts", b"up\n")]);
        for (phase, to) in [("move.prepare", &to), ("move.swap", &to2)] {
            s.arm(SessionSeam::PauseAt { phase: phase.into() });
            std::thread::scope(|sc| {
                let h = sc.spawn(|| mv(&fx, &s, &id, to));
                s.wait_paused();
                let st = s.status(&id).unwrap();
                let w = &st.workspaces[&fx.rn("tool")];
                assert_eq!(w.health, WorkspaceHealth::Ok, "{phase}");
                assert!(
                    !w.warnings.iter().any(|x| matches!(
                        x,
                        StatusWarning::MoveLeftover { .. } | StatusWarning::MoveAwaitingRecover { .. }
                    )),
                    "{phase}: a running move is not a leftover: {:?}",
                    w.warnings
                );
                s.release();
                h.join().unwrap().unwrap();
            });
        }
        // Stopped in its swap, not reopened: awaiting recover.
        let to3 = target(&fx, &s, "tool", &[("src/up3.ts", b"up\n")]);
        s.arm(SessionSeam::StopAt { phase: "move.pin".into() });
        let _ = mv(&fx, &s, &id, &to3).unwrap_err();
        let last = intents(&fx, &id).last().unwrap().clone();
        assert_eq!(last.state, MoveState::Swapping);
        assert!(
            warnings(&s, &id)
                .contains(&StatusWarning::MoveAwaitingRecover { n: last.n, staging: last.staging.clone() })
        );
        assert_eq!(health(&s, &id), WorkspaceHealth::Ok);
        for c in [MoveResolution::Abandon, MoveResolution::Accept] {
            let e = s.resolve_failed_move(&id, &fx.rn("tool"), c).unwrap_err();
            assert_eq!(e.code, Code::InvalidInput, "{c:?}: {e:?}");
            assert_eq!(reason(&e), Some("not_failed"), "{c:?}");
        }
        let calls = fx.before.calls();
        let n = intents(&fx, &id).len();
        let to4 = target(&fx, &s, "tool", &[("src/up4.ts", b"up\n")]);
        let e = mv(&fx, &s, &id, &to4).unwrap_err();
        assert_eq!(e.code, Code::InvalidInput);
        assert_eq!(reason(&e), Some("move_awaiting_recover"));
        assert_eq!(fx.before.calls(), calls, "no checkpoint");
        assert_eq!(intents(&fx, &id).len(), n, "nothing written");
        (id, to)
    };
    let _ = to;
    fx.close_store();
    fx.reopen_store();
    let s = fx.sessions();
    s.recover().unwrap();
    assert_eq!(intents(&fx, &id).last().unwrap().state, MoveState::Unknown);
    assert_eq!(health(&s, &id), WorkspaceHealth::Unknown);
    assert!(!warnings(&s, &id).iter().any(|w| matches!(w, StatusWarning::MoveAwaitingRecover { .. })));
    let r = s.resolve_failed_move(&id, &fx.rn("tool"), MoveResolution::Accept).unwrap();
    assert_eq!(r.choice, MoveResolution::Accept);
    assert_eq!(intents(&fx, &id).last().unwrap().state, MoveState::Done);
    assert_eq!(health(&s, &id), WorkspaceHealth::Ok);
}

#[test]
fn base_generation_never_modified_in_place() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = new_tool(&fx, &s);
    let id = id(&rep);
    let w = ws(&rep, "tool");
    let g0 = state(&fx, &id, "tool").base;
    let dir0 = fx.store().base_dir(&g0);
    let set0 = strata_deps::find_set(fx.store(), &fx.rn("tool"), &g0.deps).unwrap().unwrap();
    let pic0 = picture(&dir0);
    let set_pics: Vec<_> = set0.dirs.iter().map(|d| picture(&dir0.join(d.as_str()))).collect();

    fs::write(w.join("src/index.ts"), b"changed\n").unwrap();
    fs::remove_file(w.join("package.json")).unwrap();
    fs::set_permissions(w.join("bun.lock"), fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(w.join("node_modules/x/index.js"), b"poked\n").unwrap();
    fs::set_permissions(w.join("node_modules/x/package.json"), fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(picture(&dir0), pic0);

    let to = target(&fx, &s, "tool", &[("src/up.ts", b"up\n")]);
    mv(&fx, &s, &id, &to).unwrap();
    fs::write(w.join("src/up.ts"), b"poke\n").unwrap();
    assert_eq!(picture(&dir0), pic0, "the old generation is unchanged after the move");
    let set_after: Vec<_> = set0.dirs.iter().map(|d| picture(&dir0.join(d.as_str()))).collect();
    assert_eq!(set_after, set_pics);
    assert_eq!(picture(&fx.store().base_dir(&to.0)).iter().find(|e| e.0 == "src/up.ts").unwrap().5, b"up\n");
}

// ------------------------------------------- status-perf A1: old-tree verdicts

fn old_kept(s: &Sessions<'_>, id: &SessionId) -> Option<StatusWarning> {
    warnings(s, id).into_iter().find(|x| matches!(x, StatusWarning::OldTreeKept { .. }))
}

fn old_key(id: &SessionId, n: u64) -> strata_session::OldTreeKey {
    strata_session::OldTreeKey { session: id.clone(), repo: repo_name("tool"), n }
}

fn no_stop() -> Arc<std::sync::atomic::AtomicBool> {
    Arc::new(std::sync::atomic::AtomicBool::new(false))
}

/// Deferred status never judges: no verdict → `changed: null`,
/// `unchecked: true`, no holders, and the intent is queued; the background
/// check then retires the idle unchanged tree with its admin dir, and the
/// table is only a hint (status after it reports nothing).
#[test]
fn deferred_status_queues_and_the_background_check_retires() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = new_tool(&fx, &s);
    let id = id(&rep);
    let w = ws(&rep, "tool");
    let holder = Holder::cwd(&w.join("src"));
    std::thread::sleep(Duration::from_millis(100));
    let to = target(&fx, &s, "tool", &[("src/up.ts", b"up\n")]);
    let old = mv(&fx, &s, &id, &to).unwrap().old_tree.expect("kept while in use");
    let n = intents(&fx, &id).last().unwrap().n;
    s.set_old_tree_mode(strata_session::OldTreeMode::Deferred);
    let w0 = old_kept(&s, &id).expect("OldTreeKept");
    let StatusWarning::OldTreeKept { holders, changed, unchecked, checked_at, .. } = &w0 else { unreachable!() };
    assert!(holders.is_empty() && changed.is_none() && *unchecked && checked_at.is_none(), "{w0:?}");
    let v = serde_json::to_value(&w0).unwrap();
    assert_eq!(v["changed"], serde_json::Value::Null, "{v}");
    assert_eq!(v["unchecked"], true, "{v}");
    let (k, p, i) = s.next_old_tree(Duration::ZERO).expect("queued");
    assert_eq!((k.clone(), p.clone()), (old_key(&id, n), old.clone()));
    assert!(s.next_old_tree(Duration::ZERO).is_none(), "queued once");

    // Held: a verdict with the holders, not retired.
    let out = s.check_old_tree(&k, &p, i, &no_stop()).unwrap();
    assert!(
        matches!(&out, strata_session::OldTreeOutcome::Kept { holders, changed: Some(false) } if !holders.is_empty()),
        "{out:?}"
    );
    assert!(old.is_dir());
    let w1 = old_kept(&s, &id).expect("still kept");
    let StatusWarning::OldTreeKept { holders, changed, unchecked, checked_at, .. } = &w1 else { unreachable!() };
    assert!(holders.iter().any(|h| h.pid == holder.pid()), "{w1:?}");
    assert_eq!((*changed, *unchecked, checked_at.is_some()), (Some(false), false, true), "{w1:?}");
    assert!(serde_json::to_value(&w1).unwrap()["checked_at"].is_string());
    assert!(s.next_old_tree(Duration::ZERO).is_none(), "a fresh verdict is not queued again");

    // Idle: the cached `changed:false` is not trusted, the judgement is
    // made again, then retired.
    drop(holder);
    let out = s.check_old_tree(&k, &p, i, &no_stop()).unwrap();
    assert_eq!(out, strata_session::OldTreeOutcome::Retired);
    assert!(!old.exists(), "retired");
    assert!(!admin(&fx, &id, "tool", 1).exists(), "with its admin dir");
    assert!(s.old_tree_verdict(&k).is_none());
    assert!(old_kept(&s, &id).is_none());
}

/// A changed tree gets `changed: true` and is never retired; a stop gives
/// up without a verdict; an intent that no longer matches is dropped.
#[test]
fn background_check_keeps_changed_trees_and_honours_stop() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = new_tool(&fx, &s);
    let id = id(&rep);
    let w = ws(&rep, "tool");
    let holder = Holder::cwd(&w.join("src"));
    std::thread::sleep(Duration::from_millis(100));
    let to = target(&fx, &s, "tool", &[("src/up.ts", b"up\n")]);
    let old = mv(&fx, &s, &id, &to).unwrap().old_tree.expect("kept while in use");
    drop(holder);
    let n = intents(&fx, &id).last().unwrap().n;
    let k = old_key(&id, n);
    let i = ino(&old);

    let stop = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let out = s.check_old_tree(&k, &old, i, &stop).unwrap();
    assert_eq!(out, strata_session::OldTreeOutcome::Skipped("stopped"));
    assert!(old.is_dir() && s.old_tree_verdict(&k).is_none(), "a stop writes nothing, retires nothing");

    let out = s.check_old_tree(&k, &old, i + 1, &no_stop()).unwrap();
    assert_eq!(out, strata_session::OldTreeOutcome::Skipped("intent_changed"));
    let out = s.check_old_tree(&old_key(&id, n + 7), &old, i, &no_stop()).unwrap();
    assert_eq!(out, strata_session::OldTreeOutcome::Skipped("no_intent"));

    fs::write(old.join("src/late.ts"), b"late\n").unwrap();
    let out = s.check_old_tree(&k, &old, i, &no_stop()).unwrap();
    assert_eq!(out, strata_session::OldTreeOutcome::Kept { holders: vec![], changed: Some(true) });
    assert!(old.is_dir(), "a changed tree is never retired");
    assert_eq!(s.old_tree_verdict(&k).map(|v| v.changed), Some(true));
    s.set_old_tree_mode(strata_session::OldTreeMode::Deferred);
    let StatusWarning::OldTreeKept { changed, unchecked, .. } = old_kept(&s, &id).unwrap() else { unreachable!() };
    assert_eq!((changed, unchecked), (Some(true), false));
    s.forget_old_trees(&id);
    assert!(s.old_tree_verdict(&k).is_none(), "forgotten after move/restore/rm");
}

/// The half-retired sweep: an old tree retired without its admin dir (a
/// crash between the two) loses the admin dir too; the current
/// generation's admin dir is never touched.
#[test]
fn half_retired_sweep_takes_only_a_stale_admin_dir() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = new_tool(&fx, &s);
    let id = id(&rep);
    let w = ws(&rep, "tool");
    let holder = Holder::cwd(&w.join("src"));
    std::thread::sleep(Duration::from_millis(100));
    let to = target(&fx, &s, "tool", &[("src/up.ts", b"up\n")]);
    let old = mv(&fx, &s, &id, &to).unwrap().old_tree.expect("kept while in use");
    drop(holder);
    let stale = admin(&fx, &id, "tool", 1);
    assert!(stale.is_dir());
    let gen_now = generation(&fx, &id, "tool");
    let current = admin(&fx, &id, "tool", gen_now);
    assert!(current.is_dir());
    assert!(s.sweep_half_retired(&id, &repo_name("tool")).unwrap().is_empty(), "the old tree is still there");
    fx.store().retire_dir(&old).unwrap();
    let done = s.sweep_half_retired(&id, &repo_name("tool")).unwrap();
    assert_eq!(done, vec![stale.clone()]);
    assert!(!stale.exists() && current.is_dir(), "only the stale admin dir");
    assert!(s.sweep_half_retired(&id, &repo_name("tool")).unwrap().is_empty(), "idempotent");
    assert_eq!(health(&s, &id), WorkspaceHealth::Ok);
}
