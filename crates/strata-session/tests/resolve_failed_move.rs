//! session.md §8 Task 7: `resolve_failed_move` (§5.9).

mod support;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;
use strata_session::{
    MoveReport, MoveResolution, NewReport, ResolutionReport, SessionSeam, Sessions, StatusWarning, WorkspaceHealth,
};
use strata_store::{MoveIntent, MoveState};
use strata_types::{Action, BaseGen, Code, CommitOid, ConfirmToken, RepoId, SessionId, StrataError};
use support::*;

use MoveResolution::{Abandon, Accept};

// ---------------------------------------------------------------- helpers

fn commit_oid(s: &str) -> CommitOid {
    CommitOid::try_from(s.to_string()).unwrap()
}

fn rid(fx: &Fx, repo: &str) -> RepoId {
    fx.gits[&fx.rn(repo)].repo_id()
}

fn shadow(fx: &Fx, id: &SessionId) -> PathBuf {
    fx.root().join("sessions/.repos").join(id.as_str()).join("tool.git")
}

fn admin(fx: &Fx, id: &SessionId, n: u64) -> PathBuf {
    shadow(fx, id).join("worktrees").join(n.to_string())
}

fn trash(fx: &Fx) -> usize {
    fs::read_dir(fx.root().join("store/trash")).map(|d| d.count()).unwrap_or(0)
}

fn push(fx: &Fx, files: &[(&str, &[u8])], msg: &str) -> String {
    let main = fx.main("tool");
    let c = fx.sb.commit(&main, files, msg);
    fx.sb.git(&main, &["push", "-q", "origin", "HEAD:main"]);
    c
}

fn target(fx: &Fx, s: &Sessions<'_>, files: &[(&str, &[u8])]) -> (BaseGen, CommitOid) {
    let h = commit_oid(&push(fx, files, "upstream"));
    (s.ensure_base(&fx.rn("tool"), &h).unwrap(), h)
}

fn mv(fx: &Fx, s: &Sessions<'_>, id: &SessionId, to: &(BaseGen, CommitOid)) -> strata_session::Result<MoveReport> {
    s.move_workspace(id, &rid(fx, "tool"), to.0.clone(), to.1.clone())
}

fn resolve(s: &Sessions<'_>, id: &SessionId, choice: MoveResolution) -> strata_session::Result<ResolutionReport> {
    s.resolve_failed_move(id, &repo_name("tool"), choice)
}

fn state(fx: &Fx, id: &SessionId) -> strata_types::WorkspaceState {
    fx.store().session(id).unwrap().unwrap().manifest.repos[&fx.rn("tool")].clone()
}

fn intents(fx: &Fx, id: &SessionId) -> Vec<MoveIntent> {
    fx.store().move_intents(id).unwrap()
}

fn last(fx: &Fx, id: &SessionId) -> MoveIntent {
    intents(fx, id).last().unwrap().clone()
}

fn warnings(s: &Sessions<'_>, id: &SessionId) -> Vec<StatusWarning> {
    s.status(id).unwrap().workspaces[&repo_name("tool")].warnings.clone()
}

fn health(s: &Sessions<'_>, id: &SessionId) -> WorkspaceHealth {
    s.status(id).unwrap().workspaces[&repo_name("tool")].health
}

fn detail(e: &StrataError) -> &serde_json::Map<String, Value> {
    e.detail.as_ref().and_then(|d| d.as_object()).expect("detail is an object")
}

fn pin(fx: &Fx, id: &SessionId) -> String {
    fx.sb.git(&fx.main("tool"), &["rev-parse", &format!("refs/strata/pins/{id}")]).trim().to_string()
}

fn tok(id: &SessionId) -> Option<ConfirmToken> {
    Some(ConfirmToken { action: Action::RemoveDiscard { session: id.clone() }, issued: Instant::now(), nonce: 1 })
}

fn gitdir_of(tree: &Path) -> String {
    let s = fs::read_to_string(tree.join(".git")).unwrap();
    s.trim().strip_prefix("gitdir:").unwrap().trim().to_string()
}

/// Everything a refusal must leave as it was.
#[derive(Debug, PartialEq)]
struct Scene {
    session: String,
    intents: String,
    pins: String,
    ws: Vec<Entry>,
    staging: Vec<Entry>,
    worktrees: Vec<Entry>,
}

fn scene(fx: &Fx, id: &SessionId, w: &Path, staging: &Path) -> Scene {
    Scene {
        session: format!("{:?}", fx.store().session(id).unwrap()),
        intents: format!("{:?}", intents(fx, id)),
        pins: fx.pins("tool"),
        ws: snapshot(w),
        staging: snapshot(staging),
        worktrees: snapshot(&shadow(fx, id).join("worktrees")),
    }
}

struct Failed {
    id: SessionId,
    ws: PathBuf,
    staging: PathBuf,
    n: u64,
    g: u64,
    h0: CommitOid,
    to: (BaseGen, CommitOid),
}

fn failed_of(fx: &Fx, rep: &NewReport, to: (BaseGen, CommitOid), h0: CommitOid, g: u64) -> Failed {
    let id = rep.manifest.id.clone();
    let m = last(fx, &id);
    Failed { ws: rep.manifest.repos[&fx.rn("tool")].path.clone(), staging: m.staging.clone(), n: m.n, g, h0, to, id }
}

/// `FailAt move.pin`: swapped, pin still the old head, health Unknown.
fn swapped(fx: &Fx) -> Failed {
    let s = fx.sessions();
    let rep = s.new(Some("T-7"), &[fx.rn("tool")]).unwrap();
    let id = rep.manifest.id.clone();
    let h0 = state(fx, &id).head;
    let g = fx.store().session(&id).unwrap().unwrap().workspaces[&fx.rn("tool")].generation;
    let to = target(fx, &s, &[("src/up.ts", b"up\n")]);
    s.arm(SessionSeam::FailAt { phase: "move.pin".into(), errno: libc::EIO });
    let e = mv(fx, &s, &id, &to).unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    assert_eq!(detail(&e)["phase"], "pin");
    failed_of(fx, &rep, to, h0, g)
}

/// `StopAt move.swap`, reopened and recovered: never swapped.
fn never_swapped(fx: &mut Fx) -> Failed {
    let (rep, to, h0, g) = {
        let s = fx.sessions();
        let rep = s.new(Some("T-7"), &[fx.rn("tool")]).unwrap();
        let id = rep.manifest.id.clone();
        let h0 = state(fx, &id).head;
        let g = fx.store().session(&id).unwrap().unwrap().workspaces[&fx.rn("tool")].generation;
        let to = target(fx, &s, &[("src/up.ts", b"up\n")]);
        s.arm(SessionSeam::StopAt { phase: "move.swap".into() });
        let _ = mv(fx, &s, &id, &to).unwrap_err();
        (rep, to, h0, g)
    };
    fx.close_store();
    fx.reopen_store();
    fx.sessions().recover().unwrap();
    failed_of(fx, &rep, to, h0, g)
}

fn expect_invalid(r: strata_session::Result<ResolutionReport>, why: &str, what: &str) -> StrataError {
    let e = r.unwrap_err();
    assert_eq!(e.code, Code::InvalidInput, "{what}: {e:?}");
    assert_eq!(reason(&e), Some(why), "{what}: {e:?}");
    e
}

// ---------------------------------------------------------------- tests

#[test]
fn resolve_failed_move_abandons_when_never_swapped() {
    let mut fx = Fx::with(&["tool"]);
    let f = never_swapped(&mut fx);
    let s = fx.sessions();
    let id = &f.id;
    let m = last(&fx, id);
    assert_eq!(m.state, MoveState::Unknown);
    assert_eq!(m.phase.as_deref(), Some("swap"));
    assert_eq!(health(&s, id), WorkspaceHealth::Unknown);
    assert!(!f.ws.join("src/up.ts").exists(), "the workspace is the old tree");
    assert!(f.staging.join("src/up.ts").exists(), "S is the new tree");
    assert_eq!(pin(&fx, id), f.h0.as_str());
    let before = state(&fx, id);
    let conflicts = s.status(id).unwrap().workspaces[&fx.rn("tool")].conflicts.clone();
    let ws_pic = snapshot(&f.ws);
    let t0 = trash(&fx);

    let r = resolve(&s, id, Abandon).unwrap();
    assert_eq!((r.choice, r.n), (Abandon, f.n));
    assert_eq!((&r.base, &r.head), (&before.base, &before.head));
    assert_eq!(r.old_tree, None);
    assert!(r.leftover.is_empty(), "{r:?}");
    assert_eq!(last(&fx, id).state, MoveState::Abandoned);
    assert_eq!(health(&s, id), WorkspaceHealth::Ok);
    assert_eq!(state(&fx, id), before);
    let st = s.status(id).unwrap();
    let w = &st.workspaces[&fx.rn("tool")];
    assert_eq!(w.generation, f.g);
    assert_eq!(w.conflicts, conflicts);
    assert_eq!(snapshot(&f.ws), ws_pic, "the workspace is byte-for-byte unchanged");
    assert!(!f.staging.exists() && !admin(&fx, id, f.n).exists());
    assert_eq!(trash(&fx), t0 + 2, "S and worktrees/<n> are in the trash");
    assert!(!warnings(&s, id).iter().any(|x| matches!(x, StatusWarning::MoveLeftover { .. })));
    assert_eq!(pin(&fx, id), f.h0.as_str());

    // A retry returns the same report and writes nothing.
    let sc = scene(&fx, id, &f.ws, &f.staging);
    assert_eq!(resolve(&s, id, Abandon).unwrap(), r);
    assert_eq!(scene(&fx, id, &f.ws, &f.staging), sc);
    expect_invalid(resolve(&s, id, Accept), "not_failed", "accept after abandon");
    assert_eq!(scene(&fx, id, &f.ws, &f.staging), sc);

    // The next move uses n + 1.
    let to2 = target(&fx, &s, &[("src/up2.ts", b"up\n")]);
    mv(&fx, &s, id, &to2).unwrap();
    assert_eq!(last(&fx, id).n, f.n + 1);
    assert_eq!(last(&fx, id).state, MoveState::Done);
    drop(s);

    // S/.git a directory (attach stopped at its step 3): Abandon still fits.
    let mut fx = Fx::with(&["tool"]);
    let f = never_swapped(&mut fx);
    fs::remove_file(f.staging.join(".git")).unwrap();
    fs::create_dir(f.staging.join(".git")).unwrap();
    let s = fx.sessions();
    let r = resolve(&s, &f.id, Abandon).unwrap();
    assert!(r.leftover.is_empty(), "{r:?}");
    assert_eq!(health(&s, &f.id), WorkspaceHealth::Ok);
    drop(s);

    // A failed retire is no error: the leftover is reported and collected.
    let mut fx = Fx::with(&["tool"]);
    let f = never_swapped(&mut fx);
    let s = fx.sessions();
    s.arm(SessionSeam::FailAt { phase: "resolve.retire".into(), errno: libc::EIO });
    let r = resolve(&s, &f.id, Abandon).unwrap();
    assert!(r.leftover.contains(&f.staging), "{r:?}");
    assert_eq!(last(&fx, &f.id).state, MoveState::Abandoned);
    assert_eq!(health(&s, &f.id), WorkspaceHealth::Ok);
    assert!(warnings(&s, &f.id).contains(&StatusWarning::MoveLeftover { n: f.n, staging: f.staging.clone() }));
    // A retry reports the same leftovers.
    assert_eq!(resolve(&s, &f.id, Abandon).unwrap(), r);
    let to2 = target(&fx, &s, &[("src/up2.ts", b"up\n")]);
    mv(&fx, &s, &f.id, &to2).unwrap();
    assert!(!f.staging.exists(), "the next move collected the leftover");
    assert!(!admin(&fx, &f.id, f.n).exists());
    assert!(!warnings(&s, &f.id).iter().any(|x| matches!(x, StatusWarning::MoveLeftover { .. })));
}

#[test]
fn resolve_failed_move_accepts_when_swapped() {
    let fx = Fx::with(&["tool"]);
    let f = swapped(&fx);
    let s = fx.sessions();
    let id = &f.id;
    assert!(f.ws.join("src/up.ts").exists(), "the workspace is the new tree");
    assert!(!f.staging.join("src/up.ts").exists(), "S is the old tree");
    assert_eq!(pin(&fx, id), f.h0.as_str());
    assert_eq!(health(&s, id), WorkspaceHealth::Unknown);
    let sc = scene(&fx, id, &f.ws, &f.staging);

    // Two failures that write nothing: the root sync, and the lock.
    s.arm(SessionSeam::FailAt { phase: "resolve.pin".into(), errno: libc::EIO });
    let e = resolve(&s, id, Accept).unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    assert!(e.detail.as_ref().and_then(|d| d.get("op")).is_none(), "an apfs Unknown, not a scene: {e:?}");
    assert_eq!(scene(&fx, id, &f.ws, &f.staging), sc);

    fx.lock.set_wait(Duration::from_millis(200));
    let (held_tx, held_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    std::thread::scope(|sc_| {
        let lock = &fx.lock;
        sc_.spawn(move || {
            let _g = lock.hold("tool");
            held_tx.send(()).unwrap();
            let _ = go_rx.recv();
        });
        held_rx.recv().unwrap();
        let e = resolve(&s, id, Accept).unwrap_err();
        assert_eq!(e.code, Code::Busy, "{e:?}");
        go_tx.send(()).unwrap();
    });
    fx.lock.set_wait(Duration::from_secs(60));
    assert_eq!(scene(&fx, id, &f.ws, &f.staging), sc);

    // (a) the record fails after the pin moved.
    s.arm(SessionSeam::FailAt { phase: "resolve.record".into(), errno: libc::EIO });
    let e = resolve(&s, id, Accept).unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    assert_eq!(detail(&e)["op"], "resolve");
    assert_eq!(detail(&e)["phase"], "record");
    assert!(e.message.starts_with(&format!("resolve {id}/tool: record failed after writing began")), "{e:?}");
    assert_eq!(pin(&fx, id), f.to.1.as_str());
    assert_eq!(health(&s, id), WorkspaceHealth::Unknown);
    assert_eq!(last(&fx, id).state, MoveState::Unknown);
    assert_eq!(snapshot(&f.ws), sc.ws);
    assert_eq!(snapshot(&f.staging), sc.staging);

    // (b) a trailing `/` in the workspace's gitfile still reads as swapped.
    let gd = gitdir_of(&f.ws);
    assert_eq!(PathBuf::from(&gd), admin(&fx, id, f.n).canonicalize().unwrap());
    fs::write(f.ws.join(".git"), format!("gitdir: {gd}/\n")).unwrap();
    let holds = fx.lock.holds_of("tool");
    let m = last(&fx, id);
    let r = resolve(&s, id, Accept).unwrap();
    assert_eq!(fx.lock.holds_of("tool"), holds, "the pin was not moved again");
    assert_eq!((r.choice, r.n), (Accept, f.n));
    assert_eq!((&r.base, &r.head), (&f.to.0, &f.to.1));
    assert_eq!(r.old_tree.as_deref(), Some(f.staging.as_path()));
    assert!(r.leftover.is_empty());
    let st = state(&fx, id);
    assert_eq!((&st.base, &st.head), (&f.to.0, &f.to.1));
    let ws_st = s.status(id).unwrap().workspaces[&fx.rn("tool")].clone();
    assert_eq!(ws_st.health, WorkspaceHealth::Ok);
    assert_eq!(ws_st.generation, f.n);
    assert_eq!(Some(ws_st.conflicts.clone()), m.conflicts);
    assert!(
        ws_st.warnings.iter().any(|x| matches!(
            x,
            StatusWarning::OldTreeKept { path, retained: true, .. } if path == &f.staging
        )),
        "{:?}",
        ws_st.warnings
    );
    let done = last(&fx, id);
    assert_eq!(done.state, MoveState::Done);
    assert_eq!(done.old_tree.as_deref(), Some(f.staging.as_path()));
    assert_eq!(fx.sb.git(&f.ws, &["rev-parse", "HEAD"]).trim(), f.to.1.as_str());
    assert_eq!(pin(&fx, id), f.to.1.as_str());

    // A retry writes nothing and returns the same report.
    let sc = scene(&fx, id, &f.ws, &f.staging);
    assert_eq!(resolve(&s, id, Accept).unwrap(), r);
    expect_invalid(resolve(&s, id, Abandon), "not_failed", "abandon after accept");
    assert_eq!(scene(&fx, id, &f.ws, &f.staging), sc);

    // The next move uses n + 1; the old tree goes with remove.
    let to2 = target(&fx, &s, &[("src/up2.ts", b"up\n")]);
    mv(&fx, &s, id, &to2).unwrap();
    assert_eq!(last(&fx, id).n, f.n + 1);
    assert!(f.staging.exists(), "a retained old tree is not collected by a move");
    let _ = f.g;
    let _ = f.h0;
    s.remove(id, false, None).unwrap();
    assert!(!f.staging.exists(), "the old tree is retired with remove");
}

#[test]
fn resolve_failed_move_refuses_a_disagreeing_scene() {
    // (a) swapped, Abandon chosen.
    let fx = Fx::with(&["tool"]);
    let f = swapped(&fx);
    let s = fx.sessions();
    let id = &f.id;
    let sc = scene(&fx, id, &f.ws, &f.staging);
    let e = expect_invalid(resolve(&s, id, Abandon), "scene_mismatch", "(a)");
    assert_eq!(detail(&e)["matches"], "accept");
    assert_eq!(detail(&e)["choice"], "abandon");
    assert_eq!(detail(&e)["n"], f.n);
    assert_eq!(detail(&e)["phase"], "pin");
    assert_eq!(detail(&e)["found"]["pin"], f.h0.as_str());
    assert_eq!(scene(&fx, id, &f.ws, &f.staging), sc);

    // (d) the pin moved elsewhere: Accept no longer fits.
    let other = push(&fx, &[("src/other.ts", b"o\n")], "other");
    let pref = format!("refs/strata/pins/{id}");
    fx.sb.git(&fx.main("tool"), &["update-ref", &pref, &other]);
    let sc_d = scene(&fx, id, &f.ws, &f.staging);
    let e = expect_invalid(resolve(&s, id, Accept), "scene_mismatch", "(d)");
    assert!(detail(&e)["matches"].is_null(), "{e:?}");
    assert_eq!(scene(&fx, id, &f.ws, &f.staging), sc_d);
    fx.sb.git(&fx.main("tool"), &["update-ref", &pref, f.h0.as_str()]);
    assert_eq!(scene(&fx, id, &f.ws, &f.staging), sc);

    // (f) the workspace's .git a symlink to a valid gitfile.
    let copy = fx.root().join("gitfile-copy");
    fs::copy(f.ws.join(".git"), &copy).unwrap();
    fs::rename(f.ws.join(".git"), fx.root().join("gitfile-orig")).unwrap();
    symlink(&copy, f.ws.join(".git")).unwrap();
    let sc_f = scene(&fx, id, &f.ws, &f.staging);
    for c in [Abandon, Accept] {
        let e = expect_invalid(resolve(&s, id, c), "scene_mismatch", "(f)");
        assert!(detail(&e)["matches"].is_null(), "{e:?}");
        assert_eq!(detail(&e)["found"]["workspace_refusal"], "NotFile", "{e:?}");
        assert!(detail(&e)["found"]["workspace_gitdir"].is_null());
        assert_eq!(scene(&fx, id, &f.ws, &f.staging), sc_f);
    }
    fs::remove_file(f.ws.join(".git")).unwrap();
    fs::rename(fx.root().join("gitfile-orig"), f.ws.join(".git")).unwrap();
    let s_git = fs::read(f.staging.join(".git")).unwrap();
    fs::write(f.staging.join(".git"), b"gitdir:x").unwrap();
    let sc_f2 = scene(&fx, id, &f.ws, &f.staging);
    for c in [Abandon, Accept] {
        let e = expect_invalid(resolve(&s, id, c), "scene_mismatch", "(f) staging");
        assert_eq!(detail(&e)["found"]["staging_refusal"], "InvalidInput", "{e:?}");
        assert!(detail(&e)["found"]["staging_gitdir"].is_null());
        assert!(detail(&e)["matches"].is_null());
        assert_eq!(scene(&fx, id, &f.ws, &f.staging), sc_f2);
    }
    fs::write(f.staging.join(".git"), s_git).unwrap();
    assert_eq!(scene(&fx, id, &f.ws, &f.staging), sc);

    // (e) a healthy workspace, and one with only a MoveLeftover.
    let rep = s.new(Some("T-7b"), &[fx.rn("tool")]).unwrap();
    let id2 = rep.manifest.id.clone();
    for c in [Abandon, Accept] {
        expect_invalid(resolve(&s, &id2, c), "not_failed", "healthy");
    }
    let to = target(&fx, &s, &[("src/l.ts", b"l\n")]);
    s.arm(SessionSeam::FailAt { phase: "move.prepare".into(), errno: libc::EIO });
    let _ = mv(&fx, &s, &id2, &to).unwrap_err();
    assert!(warnings(&s, &id2).iter().any(|x| matches!(x, StatusWarning::MoveLeftover { .. })));
    let m2 = last(&fx, &id2);
    let sc_e = scene(&fx, &id2, &rep.manifest.repos[&fx.rn("tool")].path, &m2.staging);
    for c in [Abandon, Accept] {
        expect_invalid(resolve(&s, &id2, c), "not_failed", "leftover only");
    }
    assert_eq!(scene(&fx, &id2, &rep.manifest.repos[&fx.rn("tool")].path, &m2.staging), sc_e);
    drop(s);

    // (b) never swapped, Accept chosen.
    let mut fx = Fx::with(&["tool"]);
    let f = never_swapped(&mut fx);
    let s = fx.sessions();
    let id = &f.id;
    let sc = scene(&fx, id, &f.ws, &f.staging);
    let e = expect_invalid(resolve(&s, id, Accept), "scene_mismatch", "(b)");
    assert_eq!(detail(&e)["matches"], "abandon");
    assert_eq!(detail(&e)["phase"], "swap");
    assert_eq!(scene(&fx, id, &f.ws, &f.staging), sc);

    // (c) S moved away: neither fits; rm --discard still removes the session.
    let away = f.staging.with_file_name("moved-away");
    fs::rename(&f.staging, &away).unwrap();
    let sc_c = scene(&fx, id, &f.ws, &away);
    for c in [Abandon, Accept] {
        let e = expect_invalid(resolve(&s, id, c), "scene_mismatch", "(c)");
        assert!(detail(&e)["matches"].is_null(), "{e:?}");
        assert!(detail(&e)["found"]["staging_gitdir"].is_null(), "{e:?}");
        assert_eq!(scene(&fx, id, &f.ws, &away), sc_c);
    }
    s.remove(id, true, tok(id)).unwrap();
    let e = resolve(&s, id, Abandon).unwrap_err();
    assert_eq!(e.code, Code::NoSuchSession, "{e:?}");
}
