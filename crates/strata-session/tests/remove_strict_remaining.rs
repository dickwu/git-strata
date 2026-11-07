//! Auto-close design §10 rows 20–22: remaining `remove_strict_*` named tests
//! (nested repos, equal-to-target discharge, historical replace, Unknown /
//! landed-read, Phase B projection / lost base). Helpers are copies of
//! remove_landed.rs / remove_strict_regen.rs; those files are not included
//! here because they carry tests.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{Value, json};
use strata_overlay::{Base, Checkpointed, WorkspaceRef};
use strata_session::{BeforeCheckpoint, NewReport, SessionSeam, Sessions};
use strata_store::{Lifecycle, PublishPhase};
use strata_types::{
    Batch, CheckpointId, CheckpointReason, Code, CommitOid, ReleaseGroup, SessionId, StrataError,
};
use support::*;

const TRACK: &str = "refs/strata/remotes/origin/main";

// ---------------------------------------------------------------- helpers

fn ws(rep: &NewReport) -> PathBuf {
    rep.manifest.repos[&repo_name("tool")].path.clone()
}

fn ws_named(rep: &NewReport, repo: &str) -> PathBuf {
    rep.manifest.repos[&repo_name(repo)].path.clone()
}

fn id(rep: &NewReport) -> SessionId {
    rep.manifest.id.clone()
}

fn base_of(fx: &Fx, rep: &NewReport) -> Base {
    let store = fx.store();
    let rec = store.session(&id(rep)).unwrap().unwrap();
    let g = &rec.manifest.repos[&repo_name("tool")].base;
    let manifest = store
        .manifest(&g.repo, &g.tree)
        .unwrap()
        .expect("base manifest");
    Base {
        r#gen: g.clone(),
        manifest: std::sync::Arc::new(manifest),
        dir: store.base_dir(g),
    }
}

fn checkpoint(fx: &Fx, rep: &NewReport) -> CheckpointId {
    let rn = repo_name("tool");
    let root = ws(rep);
    let sid = id(rep);
    let w = WorkspaceRef {
        session: &sid,
        repo: &rn,
        root: &root,
        config: &fx.config.repos[&rn],
        git: &fx.gits[&rn],
        store: fx.store(),
    };
    strata_overlay::checkpoint(&w, &base_of(fx, rep), None, CheckpointReason::BeforePublish)
        .unwrap()
        .checkpoint
        .id
}

/// A commit on main pushed to origin; `fetch` also moves the tracking ref L.
fn land(fx: &Fx, writes: &[(&str, &[u8])], deletes: &[&str], fetch: bool) -> String {
    let main = fx.main("tool");
    for d in deletes {
        fx.sb.git(&main, &["rm", "-rq", d]);
    }
    for (p, b) in writes {
        fx.sb.write(&main, p, b);
    }
    fx.sb.git(&main, &["add", "-A"]);
    fx.sb
        .git(&main, &["commit", "-q", "--allow-empty", "-m", "publish"]);
    let c = fx.sb.head(&main);
    fx.sb.git(&main, &["push", "-q", "origin", "HEAD:main"]);
    if fetch {
        fx.sb.git(
            &main,
            &[
                "fetch",
                "-q",
                "origin",
                &format!("+refs/heads/main:{TRACK}"),
            ],
        );
    }
    c
}

fn row(fx: &Fx, sid: &SessionId, cp: CheckpointId, commit: &str, paths: &[&str]) -> u64 {
    let group = ReleaseGroup {
        id: 0,
        batches: vec![Batch {
            session: sid.clone(),
            repo: repo_name("tool"),
            checkpoint: cp,
            paths: rps(paths),
        }],
    };
    let n = fx
        .store()
        .record_publish(&group, SystemTime::now())
        .unwrap();
    let outcome = json!({
        "repo": "tool",
        "commit": commit,
        "paths": paths,
        "push": { "kind": "Pushed" },
        "rebase": [{ "kind": "Waiting", "session": sid.as_str(), "proposal": 21 }],
    });
    fx.store()
        .set_publish_phase(
            n,
            PublishPhase::Running,
            PublishPhase::Done,
            Some(&outcome),
            SystemTime::now(),
        )
        .unwrap();
    n
}

fn oid(c: &str) -> CommitOid {
    CommitOid::try_from(c.to_string()).unwrap()
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
    land(fx, &[("src/big.ts", &lines(&[]))], &[], true);
    let rep = s.new(None, &[repo_name("tool")]).unwrap();
    fs::write(ws(&rep).join("src/big.ts"), lines(&[(1, "mine")])).unwrap();
    rep
}

/// A Done publish of the session with these files; `fetch` moves L, `build`
/// builds its base. The session stays on its old base.
fn t3_publish(
    fx: &Fx,
    s: &Sessions<'_>,
    rep: &NewReport,
    writes: &[(&str, &[u8])],
    deletes: &[&str],
    fetch: bool,
    build: bool,
) -> (u64, String) {
    let cp = checkpoint(fx, rep);
    let c = land(fx, writes, deletes, fetch);
    let paths: Vec<&str> = writes
        .iter()
        .map(|(p, _)| *p)
        .chain(deletes.iter().copied())
        .collect();
    let n = row(fx, &id(rep), cp, &c, &paths);
    if build {
        s.ensure_base(&repo_name("tool"), &oid(&c)).unwrap();
    }
    (n, c)
}

fn detail(e: &StrataError) -> &serde_json::Map<String, Value> {
    e.detail
        .as_ref()
        .and_then(|d| d.as_object())
        .expect("detail is an object")
}

fn list(v: &Value) -> Vec<String> {
    let mut p: Vec<String> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap().to_owned())
        .collect();
    p.sort();
    p
}

fn paths(e: &StrataError) -> Vec<String> {
    list(&detail(e)["repos"]["tool"]["paths"])
}

fn lifecycle(fx: &Fx, sid: &SessionId) -> Lifecycle {
    fx.store().session(sid).unwrap().unwrap().lifecycle
}

fn agent_rm(fx: &Fx, s: &Sessions<'_>, rep: &NewReport) {
    let r = s.remove(&id(rep), false, None);
    assert!(r.is_ok(), "{:?}", r.err());
    assert_eq!(lifecycle(fx, &id(rep)), Lifecycle::Removed);
}

fn nest_repo(fx: &Fx, at: &Path) {
    fs::create_dir_all(at).unwrap();
    fx.sb.git(at, &["init", "-q"]);
}

/// Index path of an attached workspace: the admin dir named by its `.git` file.
fn workspace_index(root: &Path) -> PathBuf {
    let raw = fs::read_to_string(root.join(".git")).expect("gitfile");
    let line = raw.lines().next().expect("gitfile line");
    let gitdir = line.strip_prefix("gitdir: ").unwrap_or(line).trim();
    PathBuf::from(gitdir).join("index")
}

/// BeforeCheckpoint that runs the real checkpoint, then `f` on the workspace.
struct AfterCheckpoint<F: Fn(&Path) + Sync>(F);

impl<F: Fn(&Path) + Sync> BeforeCheckpoint for AfterCheckpoint<F> {
    fn checkpoint(
        &self,
        ws: &WorkspaceRef<'_>,
        base: &Base,
        reason: CheckpointReason,
    ) -> strata_session::Result<Checkpointed> {
        let cp = strata_overlay::checkpoint(ws, base, None, reason)?;
        (self.0)(ws.root);
        Ok(cp)
    }
}

// ---------------------------------------------------------------- row 20

#[test]
fn remove_strict_discharges_an_entry_equal_to_the_target() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = t3_session(&fx, &s);
    let v2 = b"v2\n";
    let v3 = b"v3\n";
    fs::write(ws(&rep).join("src/big.ts"), v3).unwrap();
    // Origin advances with a different content first; the publish then lands v3.
    land(&fx, &[("src/big.ts", v2)], &[], true);
    t3_publish(&fx, &s, &rep, &[("src/big.ts", v3)], &[], true, true);
    let r = s.remove_strict(&id(&rep));
    assert!(r.is_ok(), "{:?}", r.err());
    assert_eq!(lifecycle(&fx, &id(&rep)), Lifecycle::Removed);
}

#[test]
fn remove_strict_refuses_a_nested_repository() {
    // Tracked repo.
    {
        let fx = Fx::with(&["tool"]);
        let s = fx.sessions();
        let rep = s.new(None, &[repo_name("tool")]).unwrap();
        nest_repo(&fx, &ws(&rep).join("vendor/x"));
        let e = s.remove_strict(&id(&rep)).unwrap_err();
        assert_eq!(e.code, Code::Dirty, "{e:?}");
        assert_eq!(
            list(&detail(&e)["embedded_repos"]["tool"]["paths"]),
            ["vendor/x"]
        );
        assert_eq!(lifecycle(&fx, &id(&rep)), Lifecycle::Active);
        assert!(ws(&rep).join("vendor/x/.git").exists());
        agent_rm(&fx, &s, &rep);
    }
    // Untracked sibling: nested there also refuses under strict; agent passes.
    {
        let fx = Fx::new();
        let s = fx.sessions();
        let rep = s.new(None, &[repo_name("tool")]).unwrap();
        nest_repo(&fx, &ws_named(&rep, "front").join("vendor/x"));
        let e = s.remove_strict(&id(&rep)).unwrap_err();
        assert_eq!(e.code, Code::Dirty, "{e:?}");
        assert_eq!(
            list(&detail(&e)["embedded_repos"]["front"]["paths"]),
            ["vendor/x"]
        );
        assert_eq!(lifecycle(&fx, &id(&rep)), Lifecycle::Active);
        agent_rm(&fx, &s, &rep);
    }
}

#[test]
fn remove_strict_refuses_a_nested_repository_at_the_recheck() {
    for strict in [true, false] {
        let fx = Fx::with(&["tool"]);
        let s = fx.sessions();
        let rep = s.new(None, &[repo_name("tool")]).unwrap();
        let sid = id(&rep);
        let nested = ws(&rep).join("vendor/x");
        s.arm(SessionSeam::PauseAt {
            phase: "remove.before_removing".into(),
        });
        let result = std::thread::scope(|sc| {
            let h = sc.spawn(|| {
                if strict {
                    s.remove_strict(&sid)
                } else {
                    s.remove(&sid, false, None)
                }
            });
            s.wait_paused();
            nest_repo(&fx, &nested);
            s.release();
            h.join().unwrap()
        });
        if strict {
            let e = result.unwrap_err();
            assert_eq!(e.code, Code::Dirty, "{e:?}");
            assert_eq!(detail(&e)["reason"], "changed_during_remove");
            assert_eq!(detail(&e)["after"], "checkpoint");
            assert_eq!(paths(&e), ["vendor/x"]);
            assert_eq!(lifecycle(&fx, &sid), Lifecycle::Active);
            assert!(nested.join(".git").exists());
            agent_rm(&fx, &s, &rep);
        } else {
            assert!(result.is_ok(), "{:?}", result.err());
            assert_eq!(lifecycle(&fx, &sid), Lifecycle::Removed);
        }
    }
}

#[test]
fn remove_strict_refuses_own_content_a_later_publish_replaced() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = s.new(None, &[repo_name("tool")]).unwrap();
    let w = ws(&rep);
    fx.sb.write(&w, "src/p.ts", b"v1\n");
    fx.sb.write(&w, "src/q.ts", b"q\n");
    let (_, _c1) = t3_publish(&fx, &s, &rep, &[("src/p.ts", b"v1\n")], &[], true, true);
    land(&fx, &[("src/p.ts", b"v2\n")], &[], true);
    let (_, c3) = t3_publish(&fx, &s, &rep, &[("src/q.ts", b"q\n")], &[], true, true);
    let e = s.remove_strict(&id(&rep)).unwrap_err();
    assert_eq!(e.code, Code::Dirty, "{e:?}");
    assert_eq!(paths(&e), ["src/p.ts"]);
    let l = &detail(&e)["landing"]["tool"];
    assert_eq!(l["target"]["commit"], c3.as_str(), "{l}");
    assert_eq!(list(&l["historical"]), ["src/p.ts"], "{l}");
    assert_eq!(lifecycle(&fx, &id(&rep)), Lifecycle::Active);
    agent_rm(&fx, &s, &rep);
}

// ---------------------------------------------------------------- row 21

#[test]
fn remove_strict_refuses_an_unknown_session() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = s.new(None, &[repo_name("tool")]).unwrap();
    let sid = id(&rep);
    s.arm(SessionSeam::FailAt {
        phase: "remove.retire".into(),
        errno: libc::EIO,
    });
    let e = s.remove(&sid, false, None).unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    assert_eq!(lifecycle(&fx, &sid), Lifecycle::Unknown);

    let e = s.remove_strict(&sid).unwrap_err();
    assert_eq!(e.code, Code::InvalidInput, "{e:?}");
    assert_eq!(detail(&e)["reason"], "lifecycle");
    assert_eq!(lifecycle(&fx, &sid), Lifecycle::Unknown);

    let e = s.remove(&sid, false, None).unwrap_err();
    assert_eq!(e.code, Code::Dirty, "{e:?}");
    let kinds: Vec<&str> = detail(&e)["unresolved"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"session_unknown"), "{e:?}");
    assert!(e.message.contains("--discard"), "{e:?}");
}

#[test]
fn remove_strict_reports_a_failed_landed_read() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = s.new(None, &fx.all()).unwrap();
    let w = ws(&rep);
    fs::write(w.join("src/index.ts"), b"changed\n").unwrap();
    fx.sb.write(&w, "src/new.ts", b"new\n");
    let cp = checkpoint(&fx, &rep);
    // An older good row lands one path; a newer one names a commit the
    // object db lacks, so is_ancestor fails under the strict landed read.
    let c = land(&fx, &[("src/index.ts", b"changed\n")], &[], true);
    row(&fx, &id(&rep), cp, &c, &["src/index.ts"]);
    row(&fx, &id(&rep), cp, &"1".repeat(40), &["src/index.ts"]);
    let e = s.remove_strict(&id(&rep)).unwrap_err();
    assert_eq!(e.code, Code::Dirty, "{e:?}");
    assert!(detail(&e)["landed_errors"]["tool"].is_string(), "{e:?}");
    assert_eq!(lifecycle(&fx, &id(&rep)), Lifecycle::Active);
}

// ---------------------------------------------------------------- row 22

#[test]
fn remove_strict_phase_b_projection_failure_is_err() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = t3_session(&fx, &s);
    t3_publish(
        &fx,
        &s,
        &rep,
        &[("src/big.ts", &lines(&[(1, "mine"), (38, "origin")]))],
        &[],
        true,
        true,
    );
    let hook = AfterCheckpoint(|w: &Path| {
        let idx = workspace_index(w);
        fs::write(&idx, b"not-a-dirc-index").unwrap();
    });
    let s2 = Sessions::open(&fx.config, &fx.gits, fx.store(), &fx.lock, &fx.env, &hook).unwrap();
    let e = s2.remove_strict(&id(&rep)).unwrap_err();
    assert_ne!(
        e.code,
        Code::Dirty,
        "projection failure is Err, not Dirty: {e:?}"
    );
    assert_eq!(lifecycle(&fx, &id(&rep)), Lifecycle::Active);
    assert!(rep.manifest.root.is_dir(), "session root stays");
}

#[test]
fn remove_strict_phase_b_lost_base_refuses() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = t3_session(&fx, &s);
    let (_, c) = t3_publish(
        &fx,
        &s,
        &rep,
        &[("src/big.ts", &lines(&[(1, "mine"), (38, "origin")]))],
        &[],
        true,
        true,
    );
    // The tip base Phase A projected onto; dropping its store row (via the
    // gc gate, as strata-store's own raw tests do) makes `built_base` → None
    // so `project` answers `Ok(None)` → Dirty changed_during_remove.
    let tip = s.ensure_base(&repo_name("tool"), &oid(&c)).unwrap();
    let hook = AfterCheckpoint({
        let fx_root = fx.root();
        let g = tip.clone();
        move |_w: &Path| {
            let db = fx_root.join("store/store.db");
            let c = strata_store::sql::open(&db, strata_store::sql::Mode::Shared)
                .expect("raw store open");
            c.execute("UPDATE strata_gc_gate SET active = 1", ())
                .expect("open gc gate");
            let n = c
                .execute(
                    "DELETE FROM base_gens WHERE repo = ?1 AND tree = ?2 AND deps = ?3",
                    strata_store::sql::params![g.repo.as_str(), g.tree.as_str(), g.deps.as_str()],
                )
                .expect("unregister tip base");
            c.execute("UPDATE strata_gc_gate SET active = 0", ())
                .expect("close gc gate");
            assert_eq!(n, 1, "tip base_gen row");
            let dir = fx_root
                .join("bases")
                .join(g.repo.as_str())
                .join(g.tree.as_str())
                .join(g.deps.as_str());
            if dir.is_dir() {
                fs::remove_dir_all(&dir).unwrap();
            }
        }
    });
    let s2 = Sessions::open(&fx.config, &fx.gits, fx.store(), &fx.lock, &fx.env, &hook).unwrap();
    let e = s2.remove_strict(&id(&rep)).unwrap_err();
    assert_eq!(e.code, Code::Dirty, "{e:?}");
    assert_eq!(detail(&e)["reason"], "changed_during_remove", "{e:?}");
    assert_eq!(lifecycle(&fx, &id(&rep)), Lifecycle::Active);
}
