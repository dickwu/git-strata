//! session.md §5.6 step 4.2 and step 5.3 (D57): work whose exact content is
//! in a publish of this session that origin already has does not make
//! `remove` Dirty, even while the session stays on its old base.

mod support;

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{Value, json};
use strata_overlay::{Base, Checkpointed, WorkspaceRef};
use strata_session::{BeforeCheckpoint, NewReport, Sessions};
use strata_store::{Lifecycle, PublishPhase};
use strata_types::{Batch, CheckpointId, CheckpointReason, Code, ReleaseGroup, SessionId, StrataError};
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

/// A checkpoint of the session's tool workspace, as a publish takes one.
fn checkpoint(fx: &Fx, rep: &NewReport) -> CheckpointId {
    let rn = repo_name("tool");
    let root = ws(rep);
    let id = id(rep);
    let w = WorkspaceRef { session: &id, repo: &rn, root: &root, config: &fx.config.repos[&rn], git: &fx.gits[&rn], store: fx.store() };
    strata_overlay::checkpoint(&w, &base_of(fx, rep), None, CheckpointReason::BeforePublish).unwrap().checkpoint.id
}

/// A commit on main with these writes and deletions, pushed to origin.
/// `fetch` also moves origin's tracking ref, as a publish's push does.
fn land(fx: &Fx, writes: &[(&str, &[u8])], deletes: &[&str], fetch: bool) -> String {
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
    fx.sb.git(&main, &["push", "-q", "origin", "HEAD:main"]);
    if fetch {
        fx.sb.git(&main, &["fetch", "-q", "origin", &format!("+refs/heads/main:{TRACK}")]);
    }
    c
}

/// A publish row of these sessions' batches, closed with `phase` and
/// `outcome`, the shape `push.rs` writes.
fn row(fx: &Fx, batches: &[(&SessionId, CheckpointId)], phase: PublishPhase, outcome: Value) -> u64 {
    let group = ReleaseGroup {
        id: 0,
        batches: batches
            .iter()
            .map(|(s, cp)| Batch { session: (*s).clone(), repo: repo_name("tool"), checkpoint: *cp, paths: rps(&["src/index.ts"]) })
            .collect(),
    };
    let n = fx.store().record_publish(&group, SystemTime::now()).unwrap();
    if phase != PublishPhase::Running {
        fx.store().set_publish_phase(n, PublishPhase::Running, phase, Some(&outcome), SystemTime::now()).unwrap();
    }
    n
}

fn outcome(commit: &str, session: &SessionId) -> Value {
    json!({
        "repo": "tool",
        "commit": commit,
        "paths": ["src/index.ts"],
        "push": { "kind": "Pushed" },
        "rebase": [{ "kind": "Waiting", "session": session.as_str(), "proposal": 21 }],
    })
}

/// The session publishes what its workspace holds now (`writes`/`deletes`
/// mirror it on main) and stays on its old base.
fn publish(fx: &Fx, rep: &NewReport, writes: &[(&str, &[u8])], deletes: &[&str]) -> (u64, String) {
    let cp = checkpoint(fx, rep);
    let c = land(fx, writes, deletes, true);
    let n = row(fx, &[(&id(rep), cp)], PublishPhase::Done, outcome(&c, &id(rep)));
    (n, c)
}

fn detail(e: &StrataError) -> &serde_json::Map<String, Value> {
    e.detail.as_ref().and_then(|d| d.as_object()).expect("detail is an object")
}

fn paths(e: &StrataError) -> Vec<String> {
    detail(e)["repos"]["tool"]["paths"].as_array().unwrap().iter().map(|p| p.as_str().unwrap().to_string()).collect()
}

fn lifecycle(fx: &Fx, id: &SessionId) -> Lifecycle {
    fx.store().session(id).unwrap().unwrap().lifecycle
}

fn removed(fx: &Fx, s: &Sessions<'_>, rep: &NewReport) {
    let r = s.remove(&id(rep), false, None);
    assert!(r.is_ok(), "{:?}", r.err());
    assert_eq!(lifecycle(fx, &id(rep)), Lifecycle::Removed);
}

fn dirty(s: &Sessions<'_>, rep: &NewReport) -> StrataError {
    let e = s.remove(&id(rep), false, None).unwrap_err();
    assert_eq!(e.code, Code::Dirty, "{e:?}");
    e
}

// ---------------------------------------------------------------- tests

#[test]
fn remove_after_publish_waiting_on_deps_needs_no_discard() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = s.new(None, &fx.all()).unwrap();
    let w = ws(&rep);
    fs::write(w.join("src/index.ts"), b"changed\n").unwrap();
    fx.sb.write(&w, "src/new.ts", b"new\n");
    publish(&fx, &rep, &[("src/index.ts", b"changed\n"), ("src/new.ts", b"new\n")], &[]);
    let r = s.remove(&id(&rep), false, None).unwrap();
    let cp = fx.store().checkpoint(&id(&rep), r.checkpoints[&repo_name("tool")]).unwrap();
    assert_eq!(cp.reason, CheckpointReason::BeforeRemove);
    let got: Vec<&str> = cp.entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(got, ["src/index.ts", "src/new.ts"], "the checkpoint keeps the full overlay");
}

#[test]
fn remove_landed_survives_later_main_changes() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = s.new(None, &fx.all()).unwrap();
    fs::write(ws(&rep).join("src/index.ts"), b"changed\n").unwrap();
    publish(&fx, &rep, &[("src/index.ts", b"changed\n")], &[]);
    land(&fx, &[("src/index.ts", b"later\n")], &[], true);
    removed(&fx, &s, &rep);
}

#[test]
fn remove_edit_after_publish_is_dirty_only_for_that_path() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = s.new(None, &fx.all()).unwrap();
    let w = ws(&rep);
    fs::write(w.join("src/index.ts"), b"changed\n").unwrap();
    fx.sb.write(&w, "src/new.ts", b"new\n");
    let (n, c) = publish(&fx, &rep, &[("src/index.ts", b"changed\n"), ("src/new.ts", b"new\n")], &[]);
    fs::write(w.join("src/new.ts"), b"edited again\n").unwrap();
    let e = dirty(&s, &rep);
    assert_eq!(paths(&e), ["src/new.ts"]);
    assert_eq!(detail(&e)["repos"]["tool"]["count"], 1);
    assert_eq!(detail(&e)["landed"]["tool"], json!({ "count": 1, "publishes": [n], "commits": [c], "proposals": [21] }));
    assert!(detail(&e).get("landed_errors").is_none());
    let want = format!("1 more are already published in publish {n}, which left the session waiting on deps proposal #21");
    assert!(e.message.contains(&want), "{}", e.message);
    assert!(e.message.contains("changes that are not published (1 path;"), "{}", e.message);
    assert_eq!(lifecycle(&fx, &id(&rep)), Lifecycle::Active);
}

#[test]
fn remove_landed_ignores_rows_that_do_not_count() {
    for case in ["unknown", "running", "local_chain", "not_ours", "null_commit", "no_tracking_ref", "other_repo"] {
        let fx = Fx::with(&["tool"]);
        let s = fx.sessions();
        let rep = s.new(None, &fx.all()).unwrap();
        let other = s.new(None, &fx.all()).unwrap();
        fs::write(ws(&rep).join("src/index.ts"), b"changed\n").unwrap();
        let cp = checkpoint(&fx, &rep);
        let main = fx.main("tool");
        let c = if case == "local_chain" {
            fx.sb.write(&main, "src/index.ts", b"changed\n");
            fx.sb.git(&main, &["commit", "-qam", "chain only"]);
            let c = fx.sb.head(&main);
            fx.sb.git(&main, &["update-ref", "refs/strata/publish/main", &c]);
            c
        } else {
            land(&fx, &[("src/index.ts", b"changed\n")], &[], case != "no_tracking_ref")
        };
        if case == "no_tracking_ref" {
            // Session creation fetched origin; take the tracking ref away.
            fx.sb.git(&main, &["update-ref", "-d", TRACK]);
            assert!(!fx.sb.git_output(&main, &["rev-parse", "--verify", "-q", TRACK]).status.success());
        }
        let mut o = outcome(&c, &id(&rep));
        let mut batches = vec![(id(&rep), cp)];
        let mut phase = PublishPhase::Done;
        match case {
            "unknown" => phase = PublishPhase::Unknown,
            "running" => phase = PublishPhase::Running,
            "not_ours" => batches = vec![(id(&other), checkpoint(&fx, &other))],
            "null_commit" => o["commit"] = Value::Null,
            "other_repo" => o["repo"] = json!("front"),
            _ => {}
        }
        let b: Vec<(&SessionId, CheckpointId)> = batches.iter().map(|(s, c)| (s, *c)).collect();
        row(&fx, &b, phase, o);
        let e = dirty(&s, &rep);
        assert_eq!(paths(&e), ["src/index.ts"], "{case}");
        assert_eq!(detail(&e)["landed"], json!({}), "{case}");
    }
}

#[test]
fn remove_landed_reconciled_row() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = s.new(None, &fx.all()).unwrap();
    fs::write(ws(&rep).join("src/index.ts"), b"changed\n").unwrap();
    let cp = checkpoint(&fx, &rep);
    let c = land(&fx, &[("src/index.ts", b"changed\n")], &[], true);
    let o = json!({ "repo": "tool", "commit": c, "paths": ["src/index.ts"], "push": "Pushed", "reconciled": true });
    let n = row(&fx, &[(&id(&rep), cp)], PublishPhase::Unknown, o.clone());
    fx.store().set_publish_phase(n, PublishPhase::Unknown, PublishPhase::Done, Some(&o), SystemTime::now()).unwrap();
    removed(&fx, &s, &rep);
}

/// One session, `edit` applied to its workspace, `main` to the published commit.
fn kind_case(edit: impl Fn(&Path), writes: &[(&str, &[u8])], deletes: &[&str], ok: bool, what: &str) {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = s.new(None, &fx.all()).unwrap();
    edit(&ws(&rep));
    let cp = checkpoint(&fx, &rep);
    let c = land(&fx, writes, deletes, true);
    row(&fx, &[(&id(&rep), cp)], PublishPhase::Done, outcome(&c, &id(&rep)));
    let r = s.remove(&id(&rep), false, None);
    match (ok, r) {
        (true, Ok(_)) => {}
        (false, Err(e)) => assert_eq!(e.code, Code::Dirty, "{what}: {e:?}"),
        (ok, r) => panic!("{what}: want ok={ok}, got {r:?}"),
    }
}

#[test]
fn remove_landed_kinds() {
    let rm = |p: &'static str| move |w: &Path| fs::remove_file(w.join(p)).unwrap();
    kind_case(rm("patches/a.patch"), &[], &["patches/a.patch"], true, "landed deletion");
    kind_case(rm("patches/a.patch"), &[("src/index.ts", b"x\n")], &[], false, "deletion, file still there");
    kind_case(
        |w| fs::set_permissions(w.join("src/index.ts"), fs::Permissions::from_mode(0o755)).unwrap(),
        &[("src/index.ts", b"export {}\n")],
        &[],
        false,
        "mode only, commit keeps 644",
    );
    kind_case(
        |w| {
            fs::write(w.join("src/run.sh"), b"#!/bin/sh\n").unwrap();
            fs::set_permissions(w.join("src/run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        },
        &[("src/run.sh", b"#!/bin/sh\n")],
        &[],
        false,
        "same bytes, 755 vs 644",
    );
    kind_case(|w| fs::write(w.join("notes.md"), b"n\n").unwrap(), &[("notes.md", b"n\n")], &[], true, "new file");
    kind_case(|w| fs::write(w.join("notes.md"), b"n\n").unwrap(), &[("notes.md", b"other\n")], &[], false, "new file, other bytes");
    kind_case(
        |w| {
            fs::remove_file(w.join("patches/a.patch")).unwrap();
            fs::create_dir(w.join("patches/a.patch")).unwrap();
            fs::write(w.join("patches/a.patch/x"), b"x\n").unwrap();
        },
        &[("patches/a.patch/x", b"x\n")],
        &["patches/a.patch"],
        true,
        "file to directory",
    );
    kind_case(
        |w| {
            fs::remove_dir_all(w.join("patches")).unwrap();
            fs::write(w.join("patches"), b"now a file\n").unwrap();
        },
        &[("patches", b"now a file\n")],
        &["patches"],
        true,
        "directory to file",
    );
    kind_case(
        |w| symlink("index.ts", w.join("src/link")).unwrap(),
        &[("src/link", b"index.ts")],
        &[],
        false,
        "symlink vs a file with the target's bytes",
    );
    let fx_link = |w: &Path| symlink("index.ts", w.join("src/link")).unwrap();
    {
        // The commit holds `src/link` as a 120000 blob.
        let fx = Fx::with(&["tool"]);
        let s = fx.sessions();
        let rep = s.new(None, &fx.all()).unwrap();
        fx_link(&ws(&rep));
        let cp = checkpoint(&fx, &rep);
        let main = fx.main("tool");
        symlink("index.ts", main.join("src/link")).unwrap();
        let c = land(&fx, &[], &[], true);
        row(&fx, &[(&id(&rep), cp)], PublishPhase::Done, outcome(&c, &id(&rep)));
        removed(&fx, &s, &rep);
    }
}

#[test]
fn remove_landed_rename() {
    let mv = |w: &Path| fs::rename(w.join("patches/a.patch"), w.join("patches/b.patch")).unwrap();
    kind_case(mv, &[("patches/b.patch", b"patch a\n")], &["patches/a.patch"], true, "rename landed");
    kind_case(mv, &[("patches/b.patch", b"patch a\n")], &[], false, "rename, commit still has from");
}

#[test]
fn remove_landed_in_another_sessions_batch_of_the_same_group() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let a = s.new(None, &fx.all()).unwrap();
    let b = s.new(None, &fx.all()).unwrap();
    fs::write(ws(&a).join("src/index.ts"), b"same\n").unwrap();
    fs::write(ws(&b).join("src/index.ts"), b"same\n").unwrap();
    let (ca, cb) = (checkpoint(&fx, &a), checkpoint(&fx, &b));
    let c = land(&fx, &[("src/index.ts", b"same\n")], &[], true);
    row(&fx, &[(&id(&a), ca), (&id(&b), cb)], PublishPhase::Done, outcome(&c, &id(&b)));
    removed(&fx, &s, &a);
}

#[test]
fn remove_landed_git_error_is_conservative() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = s.new(None, &fx.all()).unwrap();
    let w = ws(&rep);
    fs::write(w.join("src/index.ts"), b"changed\n").unwrap();
    fx.sb.write(&w, "src/new.ts", b"new\n");
    let cp = checkpoint(&fx, &rep);
    // An older good row lands one path; a newer one names a commit the
    // object db lacks, so is_ancestor fails.
    let c = land(&fx, &[("src/index.ts", b"changed\n")], &[], true);
    row(&fx, &[(&id(&rep), cp)], PublishPhase::Done, outcome(&c, &id(&rep)));
    row(&fx, &[(&id(&rep), cp)], PublishPhase::Done, outcome(&"1".repeat(40), &id(&rep)));
    let e = dirty(&s, &rep);
    assert_eq!(paths(&e), ["src/index.ts", "src/new.ts"], "the partial match is dropped");
    assert_eq!(detail(&e)["landed"], json!({}));
    assert!(detail(&e)["landed_errors"]["tool"].is_string(), "{:?}", detail(&e));
}

/// A BeforeCheckpoint that first runs `f` on the workspace.
struct Meddling<F: Fn(&Path) + Sync>(F);

impl<F: Fn(&Path) + Sync> BeforeCheckpoint for Meddling<F> {
    fn checkpoint(&self, ws: &WorkspaceRef<'_>, base: &Base, reason: CheckpointReason) -> strata_session::Result<Checkpointed> {
        (self.0)(ws.root);
        strata_overlay::checkpoint(ws, base, None, reason)
    }
}

#[test]
fn remove_rechecks_the_before_remove_checkpoint() {
    for published in [true, false] {
        let fx = Fx::with(&["tool"]);
        let s = fx.sessions();
        let rep = s.new(None, &fx.all()).unwrap();
        if published {
            fs::write(ws(&rep).join("src/index.ts"), b"changed\n").unwrap();
            publish(&fx, &rep, &[("src/index.ts", b"changed\n")], &[]);
        }
        let hook = Meddling(move |w: &Path| {
            if published {
                fs::write(w.join("src/index.ts"), b"unpublished\n").unwrap();
            } else {
                fs::write(w.join("src/late.ts"), b"late\n").unwrap();
            }
        });
        let s2 = Sessions::open(&fx.config, &fx.gits, fx.store(), &fx.lock, &fx.env, &hook).unwrap();
        let e = s2.remove(&id(&rep), false, None).unwrap_err();
        assert_eq!(e.code, Code::Dirty, "{e:?}");
        assert_eq!(detail(&e)["reason"], "changed_during_remove");
        let want = if published { "src/index.ts" } else { "src/late.ts" };
        assert_eq!(paths(&e), [want]);
        assert_eq!(lifecycle(&fx, &id(&rep)), Lifecycle::Active);
        assert!(ws(&rep).join(want).exists(), "the workspace is still there");
    }
}

/// A BeforeCheckpoint whose checkpoint also reports `path` Unstable, as a
/// path that changed while it was read and sampled as the base would.
struct Unsteady(&'static str);

impl BeforeCheckpoint for Unsteady {
    fn checkpoint(&self, ws: &WorkspaceRef<'_>, base: &Base, reason: CheckpointReason) -> strata_session::Result<Checkpointed> {
        let mut cp = strata_overlay::checkpoint(ws, base, None, reason)?;
        cp.warnings.push(strata_overlay::OverlayWarning::Unstable { path: rp(self.0) });
        Ok(cp)
    }
}

#[test]
fn remove_recheck_refuses_unstable_paths_without_entries() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = s.new(None, &fx.all()).unwrap();
    fs::write(ws(&rep).join("src/index.ts"), b"changed\n").unwrap();
    publish(&fx, &rep, &[("src/index.ts", b"changed\n")], &[]);
    let hook = Unsteady("src/quiet.ts");
    let s2 = Sessions::open(&fx.config, &fx.gits, fx.store(), &fx.lock, &fx.env, &hook).unwrap();
    let e = s2.remove(&id(&rep), false, None).unwrap_err();
    assert_eq!(e.code, Code::Dirty, "{e:?}");
    assert_eq!(detail(&e)["reason"], "changed_during_remove");
    assert_eq!(paths(&e), ["src/quiet.ts"], "the warning counts though no entry has its path");
    assert_eq!(lifecycle(&fx, &id(&rep)), Lifecycle::Active);
}

#[test]
fn remove_recheck_refuses_a_workspace_lost_before_its_checkpoint() {
    let fx = Fx::with(&["front", "tool"]);
    let s = fx.sessions();
    let rep = s.new(None, &fx.all()).unwrap();
    let tool = ws(&rep);
    fs::write(tool.join("src/index.ts"), b"changed\n").unwrap();
    publish(&fx, &rep, &[("src/index.ts", b"changed\n")], &[]);
    // front's checkpoint comes first; meanwhile tool gets unpublished work
    // and loses its .git, so tool has no BeforeRemove checkpoint.
    let other = tool.clone();
    let hook = Meddling(move |w: &Path| {
        if w == other.as_path() {
            return;
        }
        fs::write(other.join("src/index.ts"), b"unpublished\n").unwrap();
        let g = other.join(".git");
        if g.is_dir() { fs::remove_dir_all(&g).unwrap() } else { fs::remove_file(&g).unwrap() }
    });
    let s2 = Sessions::open(&fx.config, &fx.gits, fx.store(), &fx.lock, &fx.env, &hook).unwrap();
    let e = s2.remove(&id(&rep), false, None).unwrap_err();
    assert_eq!(e.code, Code::Dirty, "{e:?}");
    assert_eq!(detail(&e)["reason"], "changed_during_remove");
    assert_eq!(detail(&e)["repos"]["tool"]["workspace"], "missing", "{:?}", detail(&e));
    assert_eq!(lifecycle(&fx, &id(&rep)), Lifecycle::Active);
    assert_eq!(fs::read(tool.join("src/index.ts")).unwrap(), b"unpublished\n");
}

#[test]
fn remove_landed_gitlink_in_the_commit_is_conservative() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = s.new(None, &fx.all()).unwrap();
    fs::write(ws(&rep).join("src/index.ts"), b"changed\n").unwrap();
    let cp = checkpoint(&fx, &rep);
    let main = fx.main("tool");
    fx.sb.write(&main, "src/index.ts", b"changed\n");
    fx.sb.git(&main, &["add", "-A"]);
    let head = fx.sb.head(&main);
    fx.sb.git(&main, &["update-index", "--add", "--cacheinfo", &format!("160000,{head},vendor/sub")]);
    fx.sb.git(&main, &["commit", "-q", "-m", "publish with a gitlink"]);
    let c = fx.sb.head(&main);
    fx.sb.git(&main, &["push", "-q", "origin", "HEAD:main"]);
    fx.sb.git(&main, &["fetch", "-q", "origin", &format!("+refs/heads/main:{TRACK}")]);
    row(&fx, &[(&id(&rep), cp)], PublishPhase::Done, outcome(&c, &id(&rep)));
    let e = dirty(&s, &rep);
    assert_eq!(paths(&e), ["src/index.ts"]);
    assert_eq!(detail(&e)["landed"], json!({}));
    assert!(detail(&e)["landed_errors"]["tool"].is_string(), "{:?}", detail(&e));
}

/// session.md §5.6 step 5.3 against the real scan: a landed deletion whose
/// path comes back as a file, which the BeforeRemove scan walks and then
/// finds a directory holding unpublished work. The scan never walks the
/// child; its Unstable warning still refuses.
#[test]
fn remove_recheck_refuses_a_file_turned_directory_during_the_scan() {
    use strata_overlay::{OverlayFault, inject_fault, release_fault, wait_fault};
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = s.new(None, &fx.all()).unwrap();
    fs::remove_file(ws(&rep).join("src/index.ts")).unwrap();
    publish(&fx, &rep, &[], &["src/index.ts"]);
    static SWAPPED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let hook = Meddling(move |w: &Path| {
        let p = w.join("src/index.ts");
        fs::write(&p, b"back\n").unwrap();
        let pause = OverlayFault::PauseBeforeRead { path: rp("src/index.ts") };
        inject_fault(w, pause.clone());
        let root = w.to_path_buf();
        // Detached: it swaps the file for a directory while the checkpoint
        // that runs next is paused before reading it.
        std::thread::spawn(move || {
            assert_eq!(wait_fault(&root), Some(pause));
            fs::remove_file(&p).unwrap();
            fs::create_dir(&p).unwrap();
            fs::write(p.join("new.ts"), b"unpublished\n").unwrap();
            SWAPPED.store(true, std::sync::atomic::Ordering::SeqCst);
            release_fault(&root);
        });
    });
    let s2 = Sessions::open(&fx.config, &fx.gits, fx.store(), &fx.lock, &fx.env, &hook).unwrap();
    let e = s2.remove(&id(&rep), false, None).unwrap_err();
    release_fault(&ws(&rep));
    assert!(SWAPPED.load(std::sync::atomic::Ordering::SeqCst), "the scan paused before reading src/index.ts");
    assert_eq!(e.code, Code::Dirty, "{e:?}");
    assert_eq!(detail(&e)["reason"], "changed_during_remove");
    assert_eq!(paths(&e), ["src/index.ts"]);
    assert_eq!(lifecycle(&fx, &id(&rep)), Lifecycle::Active);
    assert_eq!(fs::read(ws(&rep).join("src/index.ts/new.ts")).unwrap(), b"unpublished\n");
}

/// Absence is judged on the validated tree's identity, not on `ls-tree`'s
/// literal spelling: a commit that still holds the file under its
/// decomposed name has not landed the deletion of its NFC path.
#[test]
fn remove_landed_deletion_sees_a_decomposed_name() {
    let fx = Fx::with(&["tool"]);
    let main = fx.main("tool");
    let nfd = "e\u{301}.txt";
    // Keep git from precomposing the name when main's worktree is read.
    fx.sb.git(&main, &["config", "core.precomposeunicode", "false"]);
    fx.sb.write(&main, "accent.tmp", b"accent\n");
    let blob = fx.sb.git(&main, &["hash-object", "-w", "accent.tmp"]);
    fs::remove_file(main.join("accent.tmp")).unwrap();
    fx.sb.git(&main, &["update-index", "--add", "--cacheinfo", &format!("100644,{blob},{nfd}")]);
    fx.sb.git(&main, &["commit", "-q", "-m", "decomposed name"]);
    let tree = fx.sb.git(&main, &["ls-tree", "-z", "--name-only", "HEAD"]);
    assert!(tree.contains(nfd), "the commit spells the name decomposed: {tree:?}");
    fx.sb.git(&main, &["push", "-q", "origin", "HEAD:main"]);
    fx.sb.git(&main, &["reset", "-q", "--hard"]);
    let s = fx.sessions();
    let rep = s.new(None, &fx.all()).unwrap();
    let w = ws(&rep);
    fs::remove_file(w.join("\u{e9}.txt")).unwrap();
    fs::write(w.join("src/index.ts"), b"changed\n").unwrap();
    let cp = checkpoint(&fx, &rep);
    // The publish lands src/index.ts only; the accented file stays.
    let c = land(&fx, &[("src/index.ts", b"changed\n")], &[], true);
    let held = fx.sb.git(&main, &["ls-tree", "-z", "--name-only", &c]);
    assert!(held.contains(nfd), "{held:?}");
    row(&fx, &[(&id(&rep), cp)], PublishPhase::Done, outcome(&c, &id(&rep)));
    let e = dirty(&s, &rep);
    assert_eq!(paths(&e), ["\u{e9}.txt"]);
    assert_eq!(detail(&e)["landed"]["tool"]["count"], 1);
    assert_eq!(lifecycle(&fx, &id(&rep)), Lifecycle::Active);
}

/// An error after an earlier candidate already matched drops that match
/// too: the newer commit lands `src/index.ts`, the older one has a gitlink,
/// so `tree_of` fails while `src/other.ts` is still pending.
#[test]
fn remove_landed_error_after_a_match_drops_the_match() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = s.new(None, &fx.all()).unwrap();
    let w = ws(&rep);
    fs::write(w.join("src/index.ts"), b"changed\n").unwrap();
    fx.sb.write(&w, "src/other.ts", b"never published\n");
    let cp = checkpoint(&fx, &rep);
    let main = fx.main("tool");
    let head = fx.sb.head(&main);
    fx.sb.git(&main, &["update-index", "--add", "--cacheinfo", &format!("160000,{head},vendor/sub")]);
    fx.sb.git(&main, &["commit", "-q", "-m", "a gitlink"]);
    let old = fx.sb.head(&main);
    fx.sb.git(&main, &["rm", "-q", "--cached", "vendor/sub"]);
    let new = land(&fx, &[("src/index.ts", b"changed\n")], &[], true);
    row(&fx, &[(&id(&rep), cp)], PublishPhase::Done, outcome(&old, &id(&rep)));
    row(&fx, &[(&id(&rep), cp)], PublishPhase::Done, outcome(&new, &id(&rep)));
    let e = dirty(&s, &rep);
    assert_eq!(paths(&e), ["src/index.ts", "src/other.ts"], "the newer commit's match is dropped");
    assert_eq!(detail(&e)["landed"], json!({}));
    assert!(detail(&e)["landed_errors"]["tool"].is_string(), "{:?}", detail(&e));
}

/// At most 32 distinct commits are tried, newest first; rows naming the
/// same commit count once.
#[test]
fn remove_landed_tries_32_distinct_commits() {
    // (newer rows, each with its own commit?, removable?)
    for (rows, distinct, ok) in [(32, true, false), (31, true, true), (32, false, true)] {
        let fx = Fx::with(&["tool"]);
        let s = fx.sessions();
        let rep = s.new(None, &fx.all()).unwrap();
        fs::write(ws(&rep).join("src/index.ts"), b"changed\n").unwrap();
        let cp = checkpoint(&fx, &rep);
        let landed = land(&fx, &[("src/index.ts", b"changed\n")], &[], false);
        row(&fx, &[(&id(&rep), cp)], PublishPhase::Done, outcome(&landed, &id(&rep)));
        // Newer rows whose commits hold other bytes at src/index.ts.
        let mut c = land(&fx, &[("src/index.ts", b"later 0\n")], &[], false);
        for i in 0..rows {
            if distinct && i > 0 {
                c = land(&fx, &[("src/index.ts", format!("later {i}\n").as_bytes())], &[], false);
            }
            row(&fx, &[(&id(&rep), cp)], PublishPhase::Done, outcome(&c, &id(&rep)));
        }
        land(&fx, &[], &[], true);
        let r = s.remove(&id(&rep), false, None);
        match (ok, r) {
            (true, Ok(_)) => {}
            (false, Err(e)) => {
                assert_eq!(e.code, Code::Dirty, "{e:?}");
                assert_eq!(paths(&e), ["src/index.ts"]);
            }
            (ok, r) => panic!("rows={rows} distinct={distinct}: want ok={ok}, got {r:?}"),
        }
    }
}
