//! Auto-close design §10 row 6: `remove_strict` has no `rm.regenerable`
//! exemption and no historical (D57) discharge; only the projection onto a
//! publish on `origin/<default_branch>` lets entries go.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{Value, json};
use strata_overlay::{Base, Checkpointed, WorkspaceRef};
use strata_session::{BeforeCheckpoint, NewReport, Sessions};
use strata_store::{Lifecycle, PublishPhase};
use strata_types::{Batch, CheckpointId, CheckpointReason, Code, CommitOid, ReleaseGroup, SessionId, StrataError};
use support::*;

fn track(branch: &str) -> String {
    format!("refs/strata/remotes/origin/{branch}")
}

fn ws(rep: &NewReport) -> PathBuf {
    rep.manifest.repos[&repo_name("tool")].path.clone()
}

fn id(rep: &NewReport) -> SessionId {
    rep.manifest.id.clone()
}

fn base_of(fx: &Fx, rep: &NewReport) -> Base {
    let store = fx.store();
    let rec = store.session(&id(rep)).unwrap().unwrap();
    let g = &rec.manifest.repos[&repo_name("tool")].base;
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

/// A commit with these writes on the main checkout's current branch, pushed
/// to origin's `branch`; `fetch` moves origin's tracking ref of `branch`.
fn land_on(fx: &Fx, branch: &str, writes: &[(&str, &[u8])], deletes: &[&str], fetch: bool) -> String {
    let main = fx.main("tool");
    for d in deletes {
        fx.sb.git(&main, &["rm", "-rq", d]);
    }
    for (p, b) in writes {
        fx.sb.write(&main, p, b);
    }
    fx.sb.git(&main, &["add", "-A"]);
    fx.sb.git(&main, &["commit", "-q", "--allow-empty", "-m", "publish"]);
    let c = fx.sb.head(&main);
    fx.sb.git(&main, &["push", "-q", "origin", &format!("HEAD:{branch}")]);
    if fetch {
        fetch_branch(fx, branch);
    }
    c
}

fn fetch_branch(fx: &Fx, branch: &str) {
    fx.sb.git(&fx.main("tool"), &["fetch", "-q", "origin", &format!("+refs/heads/{branch}:{}", track(branch))]);
}

fn row(fx: &Fx, sid: &SessionId, cp: CheckpointId, commit: &str, paths: &[&str]) -> u64 {
    let group = ReleaseGroup {
        id: 0,
        batches: vec![Batch { session: sid.clone(), repo: repo_name("tool"), checkpoint: cp, paths: rps(paths) }],
    };
    let n = fx.store().record_publish(&group, SystemTime::now()).unwrap();
    let outcome = json!({
        "repo": "tool",
        "commit": commit,
        "paths": paths,
        "push": { "kind": "Pushed" },
        "rebase": [{ "kind": "Waiting", "session": sid.as_str(), "proposal": 21 }],
    });
    fx.store().set_publish_phase(n, PublishPhase::Running, PublishPhase::Done, Some(&outcome), SystemTime::now()).unwrap();
    n
}

/// A Done publish of the session on `branch` with these files.
#[allow(clippy::too_many_arguments)]
fn t3_publish_on(fx: &Fx, s: &Sessions<'_>, rep: &NewReport, branch: &str, writes: &[(&str, &[u8])], deletes: &[&str], fetch: bool, build: bool) -> (u64, String) {
    let cp = checkpoint(fx, rep);
    let c = land_on(fx, branch, writes, deletes, fetch);
    let paths: Vec<&str> = writes.iter().map(|(p, _)| *p).chain(deletes.iter().copied()).collect();
    let n = row(fx, &id(rep), cp, &c, &paths);
    if build {
        s.ensure_base(&repo_name("tool"), &oid(&c)).unwrap();
    }
    (n, c)
}

fn t3_publish(fx: &Fx, s: &Sessions<'_>, rep: &NewReport, writes: &[(&str, &[u8])], deletes: &[&str], fetch: bool, build: bool) -> (u64, String) {
    t3_publish_on(fx, s, rep, "main", writes, deletes, fetch, build)
}

fn oid(c: &str) -> CommitOid {
    CommitOid::try_from(c.to_string()).unwrap()
}

/// Move the session's tool workspace onto `c` (built here).
fn mv(fx: &Fx, s: &Sessions<'_>, rep: &NewReport, c: &str) {
    let g = s.ensure_base(&repo_name("tool"), &oid(c)).unwrap();
    s.move_workspace(&id(rep), &fx.gits[&repo_name("tool")].repo_id(), g, oid(c)).unwrap();
}

fn detail(e: &StrataError) -> &serde_json::Map<String, Value> {
    e.detail.as_ref().and_then(|d| d.as_object()).expect("detail is an object")
}

fn list(v: &Value) -> Vec<String> {
    let mut p: Vec<String> = v.as_array().unwrap().iter().map(|p| p.as_str().unwrap().to_owned()).collect();
    p.sort();
    p
}

fn paths(e: &StrataError) -> Vec<String> {
    list(&detail(e)["repos"]["tool"]["paths"])
}

fn lifecycle(fx: &Fx, id: &SessionId) -> Lifecycle {
    fx.store().session(id).unwrap().unwrap().lifecycle
}

fn strict_dirty(s: &Sessions<'_>, rep: &NewReport) -> StrataError {
    let e = s.remove_strict(&id(rep)).unwrap_err();
    assert_eq!(e.code, Code::Dirty, "{e:?}");
    e
}

fn before_removes(fx: &Fx) -> usize {
    fx.before.reasons.lock().unwrap().iter().filter(|r| **r == CheckpointReason::BeforeRemove).count()
}

fn agent_rm(fx: &Fx, s: &Sessions<'_>, rep: &NewReport) -> strata_session::RemoveReport {
    let r = s.remove(&id(rep), false, None);
    assert!(r.is_ok(), "{:?}", r.err());
    assert_eq!(lifecycle(fx, &id(rep)), Lifecycle::Removed);
    r.unwrap()
}

fn set(s: &Sessions<'_>, list: &[&str]) {
    s.set_regenerable(&repo_name("tool"), list.iter().map(|p| p.to_string()).collect());
}

/// A BeforeCheckpoint that first runs `f` on the workspace.
struct Meddling<F: Fn(&Path) + Sync>(F);

impl<F: Fn(&Path) + Sync> BeforeCheckpoint for Meddling<F> {
    fn checkpoint(&self, ws: &WorkspaceRef<'_>, base: &Base, reason: CheckpointReason) -> strata_session::Result<Checkpointed> {
        (self.0)(ws.root);
        strata_overlay::checkpoint(ws, base, None, reason)
    }
}

fn lines(edits: &[(usize, &str)]) -> Vec<u8> {
    let mut v: Vec<String> = (1..=40).map(|i| format!("line {i}")).collect();
    for (n, t) in edits {
        v[n - 1] = (*t).to_string();
    }
    (v.join("\n") + "\n").into_bytes()
}

/// A session (tool only) on a base with `src/big.ts` whose line 1 it edits.
fn t3_session(fx: &Fx, s: &Sessions<'_>) -> NewReport {
    land_on(fx, "main", &[("src/big.ts", &lines(&[]))], &[], true);
    let rep = s.new(None, &[repo_name("tool")]).unwrap();
    fs::write(ws(&rep).join("src/big.ts"), lines(&[(1, "mine")])).unwrap();
    rep
}

/// Split-branch repo: publishes land on `release`, the default branch stays
/// `main`.
fn split_fx() -> Fx {
    let mut fx = Fx::with(&["tool"]);
    let c = fx.cfg_mut("tool");
    c.publish.branch = "release".into();
    c.deps_dirs.clear();
    c.install = None;
    c.install_inputs.clear();
    fx
}

// ---------------------------------------------------------------- tests

#[test]
fn remove_strict_counts_regenerable_as_unpublished() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    set(&s, &["gen/**", "!gen/keep.json", "src/index.ts"]);
    let rep = s.new(None, &[repo_name("tool")]).unwrap();
    let w = ws(&rep);
    fx.sb.write(&w, "gen/a.json", b"{}\n");
    fs::write(w.join("src/index.ts"), b"regenerated\n").unwrap();
    let e = strict_dirty(&s, &rep);
    assert_eq!(paths(&e), ["gen/a.json", "src/index.ts"]);
    assert_eq!(list(&detail(&e)["regenerable"]["tool"]["paths"]), ["gen/a.json", "src/index.ts"]);
    assert_eq!(lifecycle(&fx, &id(&rep)), Lifecycle::Active);
    assert_eq!(before_removes(&fx), 0, "no BeforeRemove checkpoint");
    let r = agent_rm(&fx, &s, &rep);
    assert_eq!(r.regenerable[&repo_name("tool")].len(), 2);
}

#[test]
fn remove_strict_projects_regenerable_entries() {
    for strict in [true, false] {
        let fx = Fx::with(&["tool"]);
        let s = fx.sessions();
        set(&s, &["src/big.ts"]);
        let rep = t3_session(&fx, &s);
        t3_publish(&fx, &s, &rep, &[("src/big.ts", &lines(&[(1, "mine"), (38, "origin")]))], &[], true, true);
        if strict {
            let r = s.remove_strict(&id(&rep));
            assert!(r.is_ok(), "{:?}", r.err());
            assert_eq!(lifecycle(&fx, &id(&rep)), Lifecycle::Removed);
        } else {
            agent_rm(&fx, &s, &rep);
        }
    }
}

#[test]
fn remove_strict_recheck_has_no_regenerable_exemption() {
    for strict in [true, false] {
        let mut fx = Fx::with(&["tool"]);
        fx.cfg_mut("tool").rm.regenerable = vec!["gen/**".into()];
        let s = fx.sessions();
        let rep = s.new(None, &[repo_name("tool")]).unwrap();
        let hook = Meddling(|w: &Path| {
            fs::create_dir_all(w.join("gen")).unwrap();
            fs::write(w.join("gen/late.json"), b"{}\n").unwrap();
        });
        let s2 = Sessions::open(&fx.config, &fx.gits, fx.store(), &fx.lock, &fx.env, &hook).unwrap();
        if strict {
            let e = s2.remove_strict(&id(&rep)).unwrap_err();
            assert_eq!(e.code, Code::Dirty, "{e:?}");
            assert_eq!(detail(&e)["reason"], "changed_during_remove");
            assert_eq!(detail(&e)["after"], "dirty_check");
            assert_eq!(paths(&e), ["gen/late.json"]);
            assert_eq!(lifecycle(&fx, &id(&rep)), Lifecycle::Active);
        } else {
            let r = s2.remove(&id(&rep), false, None);
            assert!(r.is_ok(), "{:?}", r.err());
            assert_eq!(lifecycle(&fx, &id(&rep)), Lifecycle::Removed);
        }
        drop(s);
    }
}

/// C1 = p v1, C2 = p v2 (the workspace moved onto each), then p back to v1
/// and C3 = q.
fn revert_scene(fx: &Fx, s: &Sessions<'_>) -> (NewReport, String) {
    let rep = s.new(None, &[repo_name("tool")]).unwrap();
    let w = ws(&rep);
    fx.sb.write(&w, "src/p.ts", b"v1\n");
    let (_, c1) = t3_publish(fx, s, &rep, &[("src/p.ts", b"v1\n")], &[], true, true);
    mv(fx, s, &rep, &c1);
    fs::write(w.join("src/p.ts"), b"v2\n").unwrap();
    let (_, c2) = t3_publish(fx, s, &rep, &[("src/p.ts", b"v2\n")], &[], true, true);
    mv(fx, s, &rep, &c2);
    fs::write(w.join("src/p.ts"), b"v1\n").unwrap();
    fx.sb.write(&w, "src/q.ts", b"q\n");
    let (_, c3) = t3_publish(fx, s, &rep, &[("src/q.ts", b"q\n")], &[], true, true);
    (rep, c3)
}

#[test]
fn remove_strict_refuses_a_fresh_revert_a_historical_publish_matches() {
    for moved in [true, false] {
        let fx = Fx::with(&["tool"]);
        let s = fx.sessions();
        let (rep, c3) = revert_scene(&fx, &s);
        if moved {
            mv(&fx, &s, &rep, &c3);
        }
        let e = strict_dirty(&s, &rep);
        assert_eq!(paths(&e), ["src/p.ts"], "{e:?}");
        let l = &detail(&e)["landing"]["tool"];
        if moved {
            assert_eq!(l["why_none"], "no_newer_publish", "{l}");
            assert_eq!(list(&l["historical"]), ["src/p.ts"], "{l}");
        } else {
            assert_eq!(l["target"]["commit"], c3.as_str(), "{l}");
            assert_eq!(detail(&e)["landed"]["tool"]["rebased"], 1, "{e:?}");
        }
        assert_eq!(lifecycle(&fx, &id(&rep)), Lifecycle::Active);
        let r = agent_rm(&fx, &s, &rep);
        let cp = fx.store().checkpoint(&id(&rep), r.checkpoints[&repo_name("tool")]).unwrap();
        assert_eq!(cp.reason, CheckpointReason::BeforeRemove);
        assert!(cp.entries.iter().any(|e| e.path.as_str() == "src/p.ts"), "{:?}", cp.entries);
    }
}

#[test]
fn remove_strict_discharges_only_by_projection() {
    // Built: the projection onto C discharges the whole overlay.
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = s.new(None, &[repo_name("tool")]).unwrap();
    fx.sb.write(&ws(&rep), "src/p.ts", b"v1\n");
    t3_publish(&fx, &s, &rep, &[("src/p.ts", b"v1\n")], &[], true, true);
    let r = s.remove_strict(&id(&rep));
    assert!(r.is_ok(), "{:?}", r.err());
    let r = r.unwrap();
    assert!(r.checkpoints.contains_key(&repo_name("tool")));
    assert!(r.regenerable.is_empty());
    assert_eq!(lifecycle(&fx, &id(&rep)), Lifecycle::Removed);

    // Unbuilt: no projection, and D57's content match is not a discharge.
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = s.new(None, &[repo_name("tool")]).unwrap();
    fx.sb.write(&ws(&rep), "src/p.ts", b"v1\n");
    t3_publish(&fx, &s, &rep, &[("src/p.ts", b"v1\n")], &[], true, false);
    let e = strict_dirty(&s, &rep);
    assert_eq!(paths(&e), ["src/p.ts"]);
    assert_eq!(detail(&e)["landing"]["tool"]["why_none"], "no_built_base", "{e:?}");
    assert_eq!(lifecycle(&fx, &id(&rep)), Lifecycle::Active);
    agent_rm(&fx, &s, &rep);
}

#[test]
fn remove_strict_recheck_has_no_historical_discharge() {
    for strict in [true, false] {
        let fx = Fx::with(&["tool"]);
        let s = fx.sessions();
        let rep = s.new(None, &[repo_name("tool")]).unwrap();
        let w = ws(&rep);
        fx.sb.write(&w, "src/p.ts", b"v1\n");
        let (_, c1) = t3_publish(&fx, &s, &rep, &[("src/p.ts", b"v1\n")], &[], true, true);
        mv(&fx, &s, &rep, &c1);
        fs::write(w.join("src/p.ts"), b"v2\n").unwrap();
        let (_, c2) = t3_publish(&fx, &s, &rep, &[("src/p.ts", b"v2\n")], &[], true, true);
        mv(&fx, &s, &rep, &c2);
        let hook = Meddling(|w: &Path| fs::write(w.join("src/p.ts"), b"v1\n").unwrap());
        let s2 = Sessions::open(&fx.config, &fx.gits, fx.store(), &fx.lock, &fx.env, &hook).unwrap();
        if strict {
            let e = s2.remove_strict(&id(&rep)).unwrap_err();
            assert_eq!(e.code, Code::Dirty, "{e:?}");
            assert_eq!(detail(&e)["reason"], "changed_during_remove");
            assert_eq!(detail(&e)["after"], "dirty_check");
            assert_eq!(paths(&e), ["src/p.ts"]);
            assert_eq!(lifecycle(&fx, &id(&rep)), Lifecycle::Active);
        } else {
            let r = s2.remove(&id(&rep), false, None);
            assert!(r.is_ok(), "{:?}", r.err());
            assert_eq!(lifecycle(&fx, &id(&rep)), Lifecycle::Removed);
        }
    }
}

#[test]
fn remove_strict_anchors_candidates_on_the_default_branch() {
    // The publish lands on `release` only.
    let fx = split_fx();
    let s = fx.sessions();
    let rep = s.new(None, &[repo_name("tool")]).unwrap();
    fx.sb.write(&ws(&rep), "src/p.ts", b"v1\n");
    t3_publish_on(&fx, &s, &rep, "release", &[("src/p.ts", b"v1\n")], &[], true, true);
    let e = strict_dirty(&s, &rep);
    let l = &detail(&e)["landing"]["tool"];
    assert_eq!(l["why_none"], "no_landed_publish", "{l}");
    assert_eq!(l["anchor"], track("main"), "{l}");
    assert_eq!(paths(&e), ["src/p.ts"]);
    assert_eq!(lifecycle(&fx, &id(&rep)), Lifecycle::Active);
    assert_eq!(before_removes(&fx), 0, "no checkpoint written");
    // `release` merged into `main`: C now descends from B on the default branch.
    let main = fx.main("tool");
    fx.sb.git(&main, &["push", "-q", "origin", "HEAD:main"]);
    fetch_branch(&fx, "main");
    let r = s.remove_strict(&id(&rep));
    assert!(r.is_ok(), "{:?}", r.err());

    // Control: the agent's rm passes by D57 against origin/release.
    let fx = split_fx();
    let s = fx.sessions();
    let rep = s.new(None, &[repo_name("tool")]).unwrap();
    fx.sb.write(&ws(&rep), "src/p.ts", b"v1\n");
    t3_publish_on(&fx, &s, &rep, "release", &[("src/p.ts", b"v1\n")], &[], true, true);
    agent_rm(&fx, &s, &rep);

    // `release` branched before B: after the merge, C is on main but not
    // newer than the base, a documented false negative, never a false close.
    for strict in [true, false] {
        let fx = split_fx();
        let main = fx.main("tool");
        fx.sb.git(&main, &["branch", "-q", "old"]);
        land_on(&fx, "main", &[("src/b.ts", b"b\n")], &[], true);
        let s = fx.sessions();
        let rep = s.new(None, &[repo_name("tool")]).unwrap();
        fx.sb.write(&ws(&rep), "src/p.ts", b"v1\n");
        fx.sb.git(&main, &["checkout", "-q", "old"]);
        t3_publish_on(&fx, &s, &rep, "release", &[("src/p.ts", b"v1\n")], &[], true, true);
        fx.sb.git(&main, &["checkout", "-q", "main"]);
        fx.sb.git(&main, &["merge", "-q", "--no-edit", "old"]);
        fx.sb.git(&main, &["push", "-q", "origin", "HEAD:main"]);
        fetch_branch(&fx, "main");
        if strict {
            let e = strict_dirty(&s, &rep);
            assert_eq!(detail(&e)["landing"]["tool"]["why_none"], "no_newer_publish", "{e:?}");
            assert_eq!(paths(&e), ["src/p.ts"]);
        } else {
            agent_rm(&fx, &s, &rep);
        }
    }
}

#[test]
fn remove_strict_recheck_anchors_on_the_default_branch() {
    for strict in [true, false] {
        let fx = split_fx();
        let s = fx.sessions();
        let rep = s.new(None, &[repo_name("tool")]).unwrap();
        let w = ws(&rep);
        fx.sb.write(&w, "src/p.ts", b"v1\n");
        t3_publish_on(&fx, &s, &rep, "release", &[("src/p.ts", b"v1\n")], &[], true, true);
        // The workspace is restored to its base before the close.
        fs::remove_file(w.join("src/p.ts")).unwrap();
        let hook = Meddling(|w: &Path| fs::write(w.join("src/p.ts"), b"v1\n").unwrap());
        let s2 = Sessions::open(&fx.config, &fx.gits, fx.store(), &fx.lock, &fx.env, &hook).unwrap();
        if strict {
            let e = s2.remove_strict(&id(&rep)).unwrap_err();
            assert_eq!(e.code, Code::Dirty, "{e:?}");
            assert_eq!(detail(&e)["reason"], "changed_during_remove");
            assert_eq!(paths(&e), ["src/p.ts"]);
            assert_eq!(lifecycle(&fx, &id(&rep)), Lifecycle::Active);
        } else {
            let r = s2.remove(&id(&rep), false, None);
            assert!(r.is_ok(), "{:?}", r.err());
            assert_eq!(lifecycle(&fx, &id(&rep)), Lifecycle::Removed);
        }
    }
}

#[test]
fn remove_strict_historical_covers_remaining_entries_only() {
    // a.ts is discharged by the projection onto C2; b.ts = v1 is only in C1.
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = s.new(None, &[repo_name("tool")]).unwrap();
    let w = ws(&rep);
    fx.sb.write(&w, "src/b.ts", b"v1\n");
    t3_publish(&fx, &s, &rep, &[("src/b.ts", b"v1\n")], &[], true, true);
    fx.sb.write(&w, "src/a.ts", b"a\n");
    t3_publish(&fx, &s, &rep, &[("src/a.ts", b"a\n"), ("src/b.ts", b"v2\n")], &[], true, true);
    let e = strict_dirty(&s, &rep);
    assert_eq!(paths(&e), ["src/b.ts"], "{e:?}");
    assert_eq!(list(&detail(&e)["landing"]["tool"]["historical"]), ["src/b.ts"], "{e:?}");

    // Nothing remains after the projection; another repo refuses: no key.
    let fx = Fx::new();
    let s = fx.sessions();
    let rep = s.new(None, &[repo_name("tool")]).unwrap();
    fx.sb.write(&ws(&rep), "src/a.ts", b"a\n");
    t3_publish(&fx, &s, &rep, &[("src/a.ts", b"a\n")], &[], true, true);
    let front = rep.manifest.repos[&repo_name("front")].path.clone();
    fx.sb.write(&front, "src/stray.ts", b"stray\n");
    let e = strict_dirty(&s, &rep);
    assert!(detail(&e)["repos"].get("tool").is_none(), "{e:?}");
    assert!(detail(&e)["landing"]["tool"].get("historical").is_none(), "{e:?}");
}
