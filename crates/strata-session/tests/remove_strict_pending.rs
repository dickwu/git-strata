//! p1-autoclose.md §10 row 17: `remove_strict`'s §2.1 proof at step 5.5 and
//! the `set_removing_if_unchanged` guard of the `rebase_pending` keys.
//! Helpers are copies of remove_landed.rs's (`land`, `row`, `t3_session`,
//! `t3_publish`); that file is not included here because it carries tests.
//! The store half (`set_removing_if_unchanged_compares_every_key`) lives in
//! crates/strata-store/tests/rebase_pending.rs.

mod support;

use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

use serde_json::{Value, json};
use strata_overlay::{Base, WorkspaceRef};
use strata_session::{NewReport, SessionSeam, Sessions};
use strata_store::{Lifecycle, PublishPhase};
use strata_types::{
    Batch, CheckpointId, CheckpointReason, Code, CommitOid, ReleaseGroup, SessionId, StrataError, TreeOid,
};
use support::*;

const TRACK: &str = "refs/strata/remotes/origin/main";

// ---------------------------------------------------------------- helpers

fn ws(rep: &NewReport) -> PathBuf {
    rep.manifest.repos[&repo_name("tool")].path.clone()
}

fn id(rep: &NewReport) -> SessionId {
    rep.manifest.id.clone()
}

fn base_of(fx: &Fx, rep: &NewReport) -> Base {
    let g = &rep.manifest.repos[&repo_name("tool")].base;
    let store = fx.store();
    let manifest = store.manifest(&g.repo, &g.tree).unwrap().expect("base manifest");
    Base { r#gen: g.clone(), manifest: std::sync::Arc::new(manifest), dir: store.base_dir(g) }
}

fn checkpoint(fx: &Fx, rep: &NewReport) -> CheckpointId {
    let rn = repo_name("tool");
    let root = ws(rep);
    let id = id(rep);
    let w = WorkspaceRef { session: &id, repo: &rn, root: &root, config: &fx.config.repos[&rn], git: &fx.gits[&rn], store: fx.store() };
    strata_overlay::checkpoint(&w, &base_of(fx, rep), None, CheckpointReason::BeforePublish).unwrap().checkpoint.id
}

/// A commit on main pushed to origin; `fetch` also moves the tracking ref L.
fn land(fx: &Fx, writes: &[(&str, &[u8])], fetch: bool) -> String {
    let main = fx.main("tool");
    for (p, b) in writes {
        fx.sb.write(&main, p, b);
    }
    fx.sb.git(&main, &["add", "-A"]);
    fx.sb.git(&main, &["commit", "-q", "--allow-empty", "-m", "publish"]);
    let c = fx.sb.head(&main);
    fx.sb.git(&main, &["push", "-q", "origin", "HEAD:main"]);
    if fetch {
        fx.sb.git(&main, &["fetch", "-q", "origin", &format!("+refs/heads/main:{TRACK}")]);
    }
    c
}

fn row(fx: &Fx, s: &SessionId, cp: CheckpointId, commit: &str) -> u64 {
    let group = ReleaseGroup {
        id: 0,
        batches: vec![Batch { session: s.clone(), repo: repo_name("tool"), checkpoint: cp, paths: rps(&["src/big.ts"]) }],
    };
    let n = fx.store().record_publish(&group, SystemTime::now()).unwrap();
    let outcome = json!({
        "repo": "tool",
        "commit": commit,
        "paths": ["src/big.ts"],
        "push": { "kind": "Pushed" },
        "rebase": [],
    });
    fx.store().set_publish_phase(n, PublishPhase::Running, PublishPhase::Done, Some(&outcome), SystemTime::now()).unwrap();
    n
}

fn lines(edits: &[(usize, &str)]) -> Vec<u8> {
    let mut v: Vec<String> = (1..=40).map(|i| format!("line {i}")).collect();
    for (n, t) in edits {
        v[n - 1] = (*t).to_string();
    }
    (v.join("\n") + "\n").into_bytes()
}

fn t3_session(fx: &Fx, s: &Sessions<'_>) -> NewReport {
    land(fx, &[("src/big.ts", &lines(&[]))], true);
    let rep = s.new(None, &fx.all()).unwrap();
    fs::write(ws(&rep).join("src/big.ts"), lines(&[(1, "mine")])).unwrap();
    rep
}

fn t3_publish(fx: &Fx, s: &Sessions<'_>, rep: &NewReport, writes: &[(&str, &[u8])], fetch: bool, build: bool) -> (u64, String) {
    let cp = checkpoint(fx, rep);
    let c = land(fx, writes, fetch);
    let n = row(fx, &id(rep), cp, &c);
    if build {
        s.ensure_base(&repo_name("tool"), &CommitOid::try_from(c.clone()).unwrap()).unwrap();
    }
    (n, c)
}

fn detail(e: &StrataError) -> &serde_json::Map<String, Value> {
    e.detail.as_ref().and_then(|d| d.as_object()).expect("detail is an object")
}

fn lifecycle(fx: &Fx, id: &SessionId) -> Lifecycle {
    fx.store().session(id).unwrap().unwrap().lifecycle
}

/// A clean session (nothing in its overlay) and a commit on L whose head
/// and tree a main-follow row names.
fn clean_session_and_main_target(fx: &Fx, s: &Sessions<'_>) -> (NewReport, CommitOid, TreeOid) {
    land(fx, &[("src/big.ts", &lines(&[]))], true);
    let rep = s.new(None, &fx.all()).unwrap();
    let c = land(fx, &[("src/main.ts", b"main\n")], true);
    let main = fx.main("tool");
    let tree = fx.sb.git(&main, &["rev-parse", &format!("{c}^{{tree}}")]);
    (rep, CommitOid::try_from(c).unwrap(), TreeOid::try_from(tree.trim().to_string()).unwrap())
}

fn enroll(fx: &Fx, rep: &NewReport, head: &CommitOid, tree: &TreeOid) {
    let from = &rep.manifest.repos[&repo_name("tool")].base;
    assert!(fx.store().enroll_main(&id(rep), &repo_name("tool"), from, head, tree, None, SystemTime::now()).unwrap());
}

// ---------------------------------------------------------------- tests

#[test]
fn remove_strict_refuses_an_outstanding_target_at_the_recheck() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = t3_session(&fx, &s);
    // A publish whose commit origin has but L does not: off L.
    t3_publish(&fx, &s, &rep, &[("src/big.ts", &lines(&[(1, "mine")]))], false, true);
    // The overlay is restored to empty, so only the proof can refuse.
    fs::write(ws(&rep).join("src/big.ts"), lines(&[])).unwrap();
    let e = s.remove_strict(&id(&rep)).unwrap_err();
    assert_eq!(e.code, Code::Dirty, "{e:?}");
    let d = detail(&e);
    assert_eq!(d["reason"], "changed_during_remove", "{e:?}");
    assert_eq!(d["after"], "checkpoint", "{e:?}");
    assert_eq!(d["outstanding"][0]["what"], "publish", "{e:?}");
    assert_eq!(lifecycle(&fx, &id(&rep)), Lifecycle::Active);
    // The control (§11 item 27): a plain rm does not prove targets.
    let r = s.remove(&id(&rep), false, None);
    assert!(r.is_ok(), "{:?}", r.err());
    assert_eq!(lifecycle(&fx, &id(&rep)), Lifecycle::Removed);
}

#[test]
fn remove_strict_refuses_when_pending_changes_after_the_proof() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let (rep, head, tree) = clean_session_and_main_target(&fx, &s);
    let sid = id(&rep);
    s.arm(SessionSeam::PauseAt { phase: "remove.after_recheck".into() });
    let e = std::thread::scope(|sc| {
        let h = sc.spawn(|| s.remove_strict(&sid));
        s.wait_paused();
        // A target landed on L: the proof would pass it, but it came after.
        enroll(&fx, &rep, &head, &tree);
        s.release();
        h.join().unwrap()
    })
    .unwrap_err();
    assert_eq!(e.code, Code::Dirty, "{e:?}");
    let d = detail(&e);
    assert_eq!(d["reason"], "changed_during_remove", "{e:?}");
    assert_eq!(d["pending_changed"], true, "the guard, not the proof: {e:?}");
    assert_eq!(lifecycle(&fx, &sid), Lifecycle::Active);
    assert!(rep.manifest.root.is_dir(), "the session root is still at its path");
    assert!(fx.store().rebase_pending_of(&sid, &repo_name("tool")).unwrap().is_some(), "the row is present");
}

#[test]
fn remove_strict_passes_a_deferral_during_the_close() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let (rep, head, tree) = clean_session_and_main_target(&fx, &s);
    let sid = id(&rep);
    enroll(&fx, &rep, &head, &tree);
    let rev = fx.store().rebase_pending_of(&sid, &repo_name("tool")).unwrap().unwrap().rev;
    s.arm(SessionSeam::PauseAt { phase: "remove.after_recheck".into() });
    let r = std::thread::scope(|sc| {
        let h = sc.spawn(|| s.remove_strict(&sid));
        s.wait_paused();
        assert!(fx.store().defer_pending(&sid, &repo_name("tool"), rev, "busy", &json!({}), SystemTime::now()).unwrap());
        s.release();
        h.join().unwrap()
    });
    assert!(r.is_ok(), "{:?}", r.err());
    assert_eq!(lifecycle(&fx, &sid), Lifecycle::Removed);
    assert!(fx.store().rebase_pending_of(&sid, &repo_name("tool")).unwrap().is_none(), "the row is gone with Removed");
}
