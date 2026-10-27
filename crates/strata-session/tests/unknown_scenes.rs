//! session.md §8 Task 4: Unknown scenes kept after `new` fails past its
//! first visible write, and leftovers collected by `recover` (§5.5, §5.8,
//! §6.1, §6.3).

mod support;

use std::fs;
use std::path::{Path, PathBuf};

use strata_session::{Leftover, SceneOp, SessionSeam, SessionState, Sessions};
use strata_store::Lifecycle;
use strata_types::{Code, CommitOid, SessionId, StrataError};
use support::*;

fn commit_oid(s: &str) -> CommitOid {
    CommitOid::try_from(s.to_string()).unwrap()
}

fn trash_count(fx: &Fx) -> usize {
    fs::read_dir(fx.root().join("store/trash")).map(|d| d.count()).unwrap_or(0)
}

fn staging_entries(fx: &Fx, repo: &str) -> Vec<PathBuf> {
    let p = fx.root().join("bases").join(repo).join(".staging");
    let mut v: Vec<PathBuf> = fs::read_dir(&p).map(|d| d.map(|e| e.unwrap().path()).collect()).unwrap_or_default();
    v.sort();
    v
}

fn session_root(fx: &Fx, id: &SessionId) -> PathBuf {
    fx.root().join("sessions").join(id.as_str())
}

fn shadow(fx: &Fx, id: &SessionId, repo: &str) -> PathBuf {
    fx.root().join("sessions/.repos").join(id.as_str()).join(format!("{repo}.git"))
}

/// The one session row's id (sessions are only created by the test).
fn only_session(fx: &Fx) -> SessionId {
    let rows = fx.store().sessions().unwrap();
    assert_eq!(rows.len(), 1, "one session row");
    rows[0].manifest.id.clone()
}

/// `new` failing with ENOSPC at the start of `workspace` for `tool`, the
/// second repo in RepoName order.
fn fail_new_at_second_workspace(fx: &Fx, s: &Sessions<'_>) -> (SessionId, StrataError) {
    s.arm(SessionSeam::FailKeepAt { phase: "new.workspace@tool".into(), errno: libc::ENOSPC });
    let e = s.new(Some("T-1"), &fx.all()).unwrap_err();
    (only_session(fx), e)
}

fn detail(e: &StrataError) -> &serde_json::Map<String, serde_json::Value> {
    e.detail.as_ref().and_then(|d| d.as_object()).expect("detail is an object")
}

/// The session directories and the main checkouts, byte for byte.
fn disk(fx: &Fx) -> Vec<Entry> {
    let mut v = snapshot(&fx.root().join("sessions"));
    for n in &fx.names {
        v.extend(snapshot(&fx.main(n)));
    }
    v
}

// ---------------------------------------------------------------- §6.1 rule 3

#[test]
fn new_failure_after_writing_began_is_unknown() {
    let fx = Fx::new();
    let s = fx.sessions();
    let (id, e) = fail_new_at_second_workspace(&fx, &s);

    assert_eq!(e.code, Code::Unknown, "{e:?}");
    assert_eq!(detail_str(&e, "repo"), Some("tool"));

    // The scene as it was: the root and the first repo's clone; nothing of
    // tool, no shadow repository yet. Nothing removed, retried or rolled back.
    let root = session_root(&fx, &id);
    assert!(root.is_dir());
    assert!(root.join("front/package.json").is_file(), "front was cloned");
    assert!(!root.join("tool").exists(), "tool never started");
    assert!(!shadow(&fx, &id, "front").exists());
    assert!(!shadow(&fx, &id, "tool").exists());
    assert_eq!(fx.pins("front"), "");

    // The store: Unknown, last phase workspace, the scene recorded.
    let rec = fx.store().session(&id).unwrap().expect("row");
    assert_eq!(rec.lifecycle, Lifecycle::Unknown);
    assert_eq!(rec.phase.as_deref(), Some("workspace"));

    // status reports the scene; resolve finds the session by a cloned path.
    let st = s.status(&id).unwrap();
    assert_eq!(st.state, SessionState::Unknown);
    let scene = st.scene.expect("scene");
    assert_eq!(scene.op, SceneOp::New);
    assert_eq!(scene.phase, "workspace");
    assert_eq!(scene.repo, Some(fx.rn("tool")));
    assert_eq!(scene.error, e);
    assert_eq!(s.resolve(&root.join("front")).unwrap(), Some((id.clone(), fx.rn("front"))));

    // The same ticket again: a new id; the old scene is untouched.
    let before = snapshot(&root);
    let again = s.new(Some("T-1"), &fx.all()).unwrap();
    assert_ne!(again.manifest.id, id);
    assert_eq!(snapshot(&root), before);
    let st2 = s.status(&id).unwrap();
    assert_eq!(st2.state, SessionState::Unknown);
    assert_eq!(st2.scene.unwrap().error, e);
}

/// D56 (incident 2026-10-27, s-kooeepw3): a `new` that fails at any step
/// rolls itself back. No directory, shadow or pin is left, the row is
/// Removed, and the error says so (`rolled_back`); the same ticket works
/// again.
#[test]
fn new_failure_rolls_back_every_step() {
    for phase in ["new.root", "new.workspace@tool", "new.seed_caches@tool", "new.git@tool", "new.pin@tool", "new.seed_files@tool", "new.finalize"] {
        let fx = Fx::new();
        let s = fx.sessions();
        s.arm(SessionSeam::FailAt { phase: phase.into(), errno: libc::ENOSPC });
        let e = s.new(Some("T-1"), &fx.all()).unwrap_err();
        let id = only_session(&fx);
        assert_eq!(detail(&e).get("rolled_back"), Some(&serde_json::json!(true)), "{phase}: {e:?}");
        assert_eq!(detail_str(&e, "op"), Some("new"), "{phase}");
        assert_eq!(detail_str(&e, "session"), Some(id.as_str()), "{phase}");
        assert!(detail(&e).contains_key("cause"), "{phase}: {e:?}");
        assert!(e.message.contains("rolled back"), "{phase}: {}", e.message);

        assert_eq!(fx.session_dirs(), Vec::<String>::new(), "{phase}");
        assert_eq!(fx.pins("front"), "", "{phase}");
        assert_eq!(fx.pins("tool"), "", "{phase}");
        let rec = fx.store().session(&id).unwrap().expect("row");
        assert_eq!(rec.lifecycle, Lifecycle::Removed, "{phase}");

        let again = s.new(Some("T-1"), &fx.all()).unwrap();
        assert!(again.manifest.root.is_dir(), "{phase}");
    }
}

/// D56: the seed caches are cloned and joined before any timed git step, so
/// a large cache clone cannot stall the shadow `git init`s: a cache failure
/// is seen before a shadow or pin exists.
#[test]
fn new_joins_seed_caches_before_git() {
    let fx = Fx::new();
    let s = fx.sessions();
    s.arm(SessionSeam::FailKeepAt { phase: "new.seed_caches@tool".into(), errno: libc::EIO });
    let e = s.new(Some("T-1"), &fx.all()).unwrap_err();
    let id = only_session(&fx);
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    let rec = fx.store().session(&id).unwrap().expect("row");
    assert_eq!(rec.lifecycle, Lifecycle::Unknown);
    assert_eq!(rec.phase.as_deref(), Some("seed_caches"));
    assert!(session_root(&fx, &id).join("tool/package.json").is_file());
    assert!(!shadow(&fx, &id, "front").exists());
    assert!(!shadow(&fx, &id, "tool").exists());
    assert_eq!(fx.pins("front"), "");
    assert_eq!(fx.pins("tool"), "");
}

/// Incident 2026-10-27: a shadow directory left by a `git init` that never
/// finished (only a `config`, no marker) made `status` fail outright. It is
/// reported (`ShadowInvalid`) and status answers.
#[test]
fn status_survives_a_half_built_shadow() {
    let fx = Fx::new();
    let s = fx.sessions();
    s.arm(SessionSeam::FailKeepAt { phase: "new.git@tool".into(), errno: libc::EIO });
    s.new(Some("T-1"), &fx.all()).unwrap_err();
    let id = only_session(&fx);
    let sh = shadow(&fx, &id, "tool");
    fs::create_dir_all(&sh).unwrap();
    fs::write(sh.join("config"), b"[core]\n\trepositoryformatversion = 0\n").unwrap();
    let st = s.status(&id).unwrap();
    assert_eq!(st.state, SessionState::Unknown);
    let w = format!("{:?}", st);
    assert!(w.contains("ShadowInvalid"), "{w}");
}

#[test]
fn new_unknown_detail_names_the_kept_scene() {
    let fx = Fx::new();
    let s = fx.sessions();
    let (id, e) = fail_new_at_second_workspace(&fx, &s);
    let root = session_root(&fx, &id);
    let d = detail(&e);

    let mut keys: Vec<&str> = d.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["cause", "kept", "op", "path", "phase", "repo", "session"]);
    assert_eq!(d["op"], "new");
    assert_eq!(d["session"], id.as_str());
    assert_eq!(d["phase"], "workspace");
    assert_eq!(d["repo"], "tool");
    assert_eq!(d["path"], root.join("tool").display().to_string());
    assert_eq!(d["cause"]["errno"], 28);
    let text = std::io::Error::from_raw_os_error(libc::ENOSPC).to_string();
    let cause_msg = d["cause"]["message"].as_str().unwrap();
    assert!(cause_msg.contains(&text), "{cause_msg}");
    assert_eq!(d["kept"], serde_json::json!([root.display().to_string()]));
    assert!(root.is_absolute());

    assert_eq!(
        e.message,
        format!(
            "new {id}: workspace failed after writing began; kept {}; nothing was rolled back, cleaned up or retried; see git-strata status {id}",
            root.display()
        )
    );

    // The stored scene serialises op the same way as detail.op.
    let scene = s.status(&id).unwrap().scene.unwrap();
    let v = serde_json::to_value(&scene).unwrap();
    assert_eq!(v["op"], "new");
    assert_eq!(v["op"], d["op"]);
    assert_eq!(scene.kept, vec![root]);
}

// ---------------------------------------------------------------- §5.8 step 1

#[test]
fn unknown_scene_survives_daemon_restart() {
    // (a) An Unknown from a failure: the same scene after a restart.
    let mut fx = Fx::new();
    let (id, scene) = {
        let s = fx.sessions();
        let (id, _) = fail_new_at_second_workspace(&fx, &s);
        (id.clone(), s.status(&id).unwrap().scene.unwrap())
    };
    let before = disk(&fx);
    fx.close_store();
    fx.reopen_store();
    {
        let s = fx.sessions();
        let rep = s.recover().unwrap();
        assert!(rep.errors.is_empty(), "{:?}", rep.errors);
        assert!(rep.unknown.is_empty(), "already Unknown");
        let st = s.status(&id).unwrap();
        assert_eq!(st.state, SessionState::Unknown);
        assert_eq!(st.scene.unwrap(), scene);
    }
    assert_eq!(disk(&fx), before, "no replay, no cleanup");

    // (b) A crash: StopAt returns in place; the row stays Creating.
    let mut fx = Fx::new();
    {
        let s = fx.sessions();
        s.arm(SessionSeam::StopAt { phase: "new.seed_files".into() });
        let e = s.new(None, &fx.all()).unwrap_err();
        assert_eq!(e.code, Code::Unknown);
    }
    let id = only_session(&fx);
    assert_eq!(fx.store().session(&id).unwrap().unwrap().lifecycle, Lifecycle::Creating);
    let before = disk(&fx);
    fx.close_store();
    fx.reopen_store();
    {
        let s = fx.sessions();
        let rep = s.recover().unwrap();
        assert!(rep.errors.is_empty(), "{:?}", rep.errors);
        assert_eq!(rep.unknown, vec![id.clone()]);
        let st = s.status(&id).unwrap();
        assert_eq!(st.state, SessionState::Unknown);
        let scene = st.scene.unwrap();
        assert_eq!(scene.op, SceneOp::New);
        assert_eq!(scene.phase, "seed_files");
        assert_eq!(scene.error.code, Code::Unknown);
        assert_eq!(detail_str(&scene.error, "reason"), Some("interrupted"));
        let root = session_root(&fx, &id);
        assert_eq!(scene.kept, vec![root, shadow(&fx, &id, "front"), shadow(&fx, &id, "tool")]);
        // A second start finds nothing more to do.
        let again = s.recover().unwrap();
        assert!(again.unknown.is_empty() && again.errors.is_empty());
        assert_eq!(s.status(&id).unwrap().scene.unwrap(), scene);
    }
    assert_eq!(disk(&fx), before, "no replay, no cleanup");
}

// ---------------------------------------------------------------- §5.2, §5.5, §5.8 step 3

fn base_staging(l: &Leftover) -> (&str, &Path, &[PathBuf]) {
    match l {
        Leftover::BaseStaging { repo, path, export_temps } => (repo.as_str(), path.as_path(), export_temps.as_slice()),
    }
}

#[test]
fn base_staging_leftover_reported_then_retired_by_recover() {
    let mut fx = Fx::with(&["tool"]);
    let tool = fx.rn("tool");
    let main = fx.main("tool");

    // A registered generation first (and the deps set sealed).
    let (g0, b) = {
        let s = fx.sessions();
        let h0 = commit_oid(&fx.sb.head(&main));
        let g0 = s.ensure_base(&tool, &h0).unwrap();
        assert!(staging_entries(&fx, "tool").is_empty());

        // The process stops before registering.
        let h1 = commit_oid(&fx.sb.commit(&main, &[("a.txt", b"a\n")], "a"));
        s.arm(SessionSeam::StopAt { phase: "base.register".into() });
        let e = s.ensure_base(&tool, &h1).unwrap_err();
        assert_eq!(e.code, Code::Unknown);
        let st = staging_entries(&fx, "tool");
        assert_eq!(st.len(), 1, "B stays behind");
        (g0, st[0].clone())
    };
    fx.close_store();
    fx.reopen_store();
    {
        let s = fx.sessions();
        let left = s.leftovers().unwrap();
        assert_eq!(left.len(), 1, "{left:?}");
        let (repo, path, _) = base_staging(&left[0]);
        assert_eq!(repo, "tool");
        assert_eq!(path, b.as_path());

        let t0 = trash_count(&fx);
        let rep = s.recover().unwrap();
        assert!(rep.errors.is_empty(), "{:?}", rep.errors);
        assert_eq!(rep.retired, vec![b.clone()]);
        assert!(!b.exists());
        assert_eq!(trash_count(&fx), t0 + 1, "B is in the trash");
        assert!(s.leftovers().unwrap().is_empty());
        assert!(fx.store().has_base_gen(&g0).unwrap(), "registered generation untouched");
        assert!(fx.store().base_dir(&g0).is_dir());

        // A staging being built right now is not a leftover.
        let h2 = commit_oid(&fx.sb.commit(&main, &[("b.txt", b"b\n")], "b"));
        std::thread::scope(|sc| {
            s.arm(SessionSeam::PauseAt { phase: "base.deps".into() });
            let h = sc.spawn(|| s.ensure_base(&tool, &h2));
            s.wait_paused();
            assert_eq!(staging_entries(&fx, "tool").len(), 1, "the live staging exists");
            assert!(s.leftovers().unwrap().is_empty(), "live staging is not a leftover");
            s.release();
            h.join().unwrap().unwrap();
        });

        // A half-done write: Unknown with kept empty; B is already retired.
        let h3 = commit_oid(&fx.sb.commit(&main, &[("c.txt", b"c\n")], "c"));
        let t0 = trash_count(&fx);
        s.arm(SessionSeam::FailAt { phase: "base.register".into(), errno: libc::EIO });
        let e = s.ensure_base(&tool, &h3).unwrap_err();
        assert_eq!(e.code, Code::Unknown, "{e:?}");
        let d = detail(&e);
        assert_eq!(d["op"], "base");
        assert_eq!(d["kept"], serde_json::json!([]));
        let retired = d["retired"].as_array().unwrap();
        assert_eq!(retired.len(), 1);
        assert_eq!(retired[0], d["path"]);
        assert!(!Path::new(d["path"].as_str().unwrap()).exists());
        assert_eq!(trash_count(&fx), t0 + 1);
        assert!(e.message.ends_with("the staging was retired; retry when the cause is fixed"), "{}", e.message);
        assert!(s.leftovers().unwrap().is_empty());

        // A named failure passes through; the staging is retired on the spot.
        s.arm(SessionSeam::FailAt { phase: "base.export".into(), errno: libc::ENOTDIR });
        let e = s.ensure_base(&tool, &h3).unwrap_err();
        assert_eq!(e.code, Code::NotDirectory, "{e:?}");
        assert!(staging_entries(&fx, "tool").is_empty());
        assert!(s.leftovers().unwrap().is_empty());

        // gc_repo takes the staging before registration: rebuilt once.
        let tree = fx.gits[&tool].tree_of(&h3).unwrap();
        std::thread::scope(|sc| {
            s.arm(SessionSeam::PauseAt { phase: "base.register".into() });
            let h = sc.spawn(|| s.ensure_base(&tool, &h3));
            s.wait_paused();
            let st = staging_entries(&fx, "tool");
            assert_eq!(st.len(), 1);
            fx.store().retire_dir(&st[0]).unwrap();
            s.release();
            let g = h.join().unwrap().unwrap();
            assert_eq!(g.tree, tree);
            assert!(fx.store().has_base_gen(&g).unwrap());
        });
        assert!(staging_entries(&fx, "tool").is_empty());

        // Taken twice: Busy, nothing registered, no generation directory.
        let h4 = commit_oid(&fx.sb.commit(&main, &[("d.txt", b"d\n")], "d"));
        let tree4 = fx.gits[&tool].tree_of(&h4).unwrap();
        std::thread::scope(|sc| {
            s.arm(SessionSeam::PauseAt { phase: "base.register".into() });
            let h = sc.spawn(|| s.ensure_base(&tool, &h4));
            s.wait_paused();
            let st = staging_entries(&fx, "tool");
            fx.store().retire_dir(&st[0]).unwrap();
            s.arm(SessionSeam::PauseAt { phase: "base.register".into() });
            s.release();
            s.wait_paused();
            let st = staging_entries(&fx, "tool");
            assert_eq!(st.len(), 1, "the rebuild's staging");
            fx.store().retire_dir(&st[0]).unwrap();
            s.release();
            let e = h.join().unwrap().unwrap_err();
            assert_eq!(e.code, Code::Busy, "{e:?}");
            assert_eq!(reason(&e), Some("base_staging_retired"));
        });
        assert!(!fx.root().join("bases/tool").join(tree4.as_str()).exists());
        assert!(s.leftovers().unwrap().is_empty());
    }
}
