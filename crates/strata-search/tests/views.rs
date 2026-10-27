//! search.md §8 Task 1: the view table and the typed refusals of `attach`.

mod support;

use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

use strata_search::{GrepOpts, Search, Which};
use strata_session::{NewReport, Scene, SceneOp};
use strata_store::{Lifecycle, MoveState};
use strata_types::{Code, SessionId, SessionManifest, StrataError, ViewId};
use support::*;

fn id(rep: &NewReport) -> SessionId {
    rep.manifest.id.clone()
}

/// A front-only fixture with one session whose overlay is captured.
fn one() -> (Fx, NewReport, FakeOverlays) {
    let fx = Fx::with(&["front"]);
    let rep = fx.new_session(&["front"]);
    let ov = FakeOverlays::default();
    ov.set(&id(&rep), "front", fx.capture(&id(&rep), "front"));
    (fx, rep, ov)
}

fn code(r: Result<impl std::fmt::Debug, StrataError>) -> StrataError {
    r.expect_err("expected an error")
}

fn assert_no_view(s: &Search<'_>, v: &ViewId) {
    let e = code(s.grep(Which::One(v.clone()), "x", &GrepOpts::default()));
    assert_eq!(e.code, Code::NoSuchView, "grep {e:?}");
    let e = code(s.files(v.clone(), "*"));
    assert_eq!(e.code, Code::NoSuchView, "files {e:?}");
    let e = code(s.refresh(v.clone(), &[]));
    assert_eq!(e.code, Code::NoSuchView, "refresh {e:?}");
    let e = code(s.detach(v.clone()));
    assert_eq!(e.code, Code::NoSuchView, "detach {e:?}");
}

#[test]
fn unknown_view_is_no_such_view() {
    let (fx, rep, ov) = one();
    let ss = fx.sessions();
    let s = Search::open(&fx.config, &ss, &ov);
    assert_no_view(&s, &ViewId::new("v-0000000000000000".into()));

    let v = s.attach(&id(&rep), &fx.rn("front")).unwrap();
    let txt = v.get();
    assert!(txt.starts_with("v-") && txt.len() == 18, "{txt}");
    assert!(txt[2..].chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()), "{txt}");
    s.detach(v.clone()).unwrap();
    assert_no_view(&s, &v);

    // Re-attaching is a new id.
    let w = s.attach(&id(&rep), &fx.rn("front")).unwrap();
    assert_ne!(v, w);
    assert_eq!(s.files(w, "nothing-matches-this").unwrap(), Vec::new());
    // The shared generation outlives its views.
    let gens = generation_dirs(&fx.root().join("search/front"));
    assert!(!gens.is_empty(), "no generation under <root>/search/front");
}

/// Every `gen-*` directory below `dir` (any depth).
pub fn generation_dirs(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    fn go(d: &std::path::Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if e.file_name().to_string_lossy().starts_with("gen-") {
                out.push(p);
            } else if p.is_dir() {
                go(&p, out);
            }
        }
    }
    go(dir, &mut out);
    out
}

fn scene(id: &SessionId, op: SceneOp, repo: Option<&str>) -> serde_json::Value {
    serde_json::to_value(Scene {
        op,
        session: id.clone(),
        repo: repo.map(repo_name),
        phase: "test".into(),
        at: SystemTime::now(),
        kept: vec![],
        error: StrataError::new(Code::Unknown, "test scene"),
    })
    .unwrap()
}

#[test]
fn attach_refuses_inactive_session() {
    let (fx, rep, ov) = one();
    let st = fx.store();

    // Creating: a second session row that never got past creation.
    let other = sid("s-aaaaaaaa");
    let root = fx.root().join("sessions").join(other.as_str());
    let mut repos = rep.manifest.repos.clone();
    for (r, w) in repos.iter_mut() {
        w.path = root.join(r.as_str());
    }
    st.create_session(&SessionManifest { id: other.clone(), ticket: None, agent: None, root, repos }, SystemTime::now())
        .unwrap();
    let first = ov.map.lock().unwrap()[&(id(&rep), fx.rn("front"))].clone();
    ov.set(&other, "front", first);

    // Workspace Unknown: a move that failed while swapping.
    let third = fx.new_session(&["front"]);
    let tid = id(&third);
    ov.set(&tid, "front", fx.capture(&tid, "front"));
    let w = fx.ws(&third, "front");
    let n = st
        .begin_move(&tid, &fx.rn("front"), &w.base, &w.head, &fx.root().join("sessions").join(tid.as_str()).join(".staging-front"), SystemTime::now())
        .unwrap();
    assert!(
        st.set_move_state(&tid, &fx.rn("front"), n, MoveState::Preparing, MoveState::Swapping, Some(&[]), SystemTime::now())
            .unwrap()
    );
    st.fail_move(&tid, &fx.rn("front"), n, &scene(&tid, SceneOp::Move, Some("front")), SystemTime::now()).unwrap();

    // Removed.
    let gone = id(&rep);
    assert!(st.set_lifecycle(&gone, Lifecycle::Active, Lifecycle::Removing, None, SystemTime::now()).unwrap());
    assert!(st.set_lifecycle(&gone, Lifecycle::Removing, Lifecycle::Removed, None, SystemTime::now()).unwrap());

    let ss = fx.sessions();
    let s = Search::open(&fx.config, &ss, &ov);
    let e = code(s.attach(&other, &fx.rn("front")));
    assert_eq!((e.code, reason(&e)), (Code::Busy, Some("not_active")), "{e:?}");
    let e = code(s.attach(&gone, &fx.rn("front")));
    assert_eq!(e.code, Code::NoSuchSession, "{e:?}");
    let e = code(s.attach(&tid, &fx.rn("front")));
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    assert!(e.detail.as_ref().and_then(|d| d.get("scene")).is_some_and(|s| !s.is_null()), "{e:?}");
    // No view was made: All has nothing to answer with.
    assert_eq!(s.grep(Which::All, ".", &GrepOpts::default()).unwrap(), Vec::new());
    assert!(generation_dirs(&fx.root().join("search")).is_empty());
}

#[test]
fn attach_missing_workspace_is_no_such_path() {
    let (fx, rep, ov) = one();
    let ws = fx.ws(&rep, "front").path;
    let aside = ws.with_file_name("front-aside");
    fs::rename(&ws, &aside).unwrap();
    {
        let ss = fx.sessions();
        let s = Search::open(&fx.config, &ss, &ov);
        let e = code(s.attach(&id(&rep), &fx.rn("front")));
        assert_eq!(e.code, Code::NoSuchPath, "{e:?}");
        assert_eq!(s.grep(Which::All, ".", &GrepOpts::default()).unwrap(), Vec::new());
    }
    fs::write(&ws, b"not a directory\n").unwrap();
    {
        let ss = fx.sessions();
        let s = Search::open(&fx.config, &ss, &ov);
        let e = code(s.attach(&id(&rep), &fx.rn("front")));
        assert_eq!(e.code, Code::NotDirectory, "{e:?}");
        assert_eq!(s.grep(Which::All, ".", &GrepOpts::default()).unwrap(), Vec::new());
    }
    fs::remove_file(&ws).unwrap();
    fs::rename(&aside, &ws).unwrap();
}

#[test]
fn invalid_pattern_is_invalid_input() {
    let (fx, rep, ov) = one();
    let ss = fx.sessions();
    let s = Search::open(&fx.config, &ss, &ov);
    let v = s.attach(&id(&rep), &fx.rn("front")).unwrap();
    for p in ["", "("] {
        for w in [Which::One(v.clone()), Which::All] {
            let e = code(s.grep(w, p, &GrepOpts::default()));
            assert_eq!((e.code, reason(&e)), (Code::InvalidInput, Some("pattern")), "{p:?} {e:?}");
        }
    }
    // The pattern is checked before the view.
    let e = code(s.grep(Which::One(ViewId::new("v-ffffffffffffffff".into())), "(", &GrepOpts::default()));
    assert_eq!(e.code, Code::InvalidInput);
}
