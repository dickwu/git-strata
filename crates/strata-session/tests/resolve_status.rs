//! session.md §8 Task 3: resolve, select, list and status (§5.4, §5.5).

mod support;

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use strata_overlay::{Base, WorkspaceRef};
use strata_session::{
    NewReport, Scene, SceneOp, SessionState, Sessions, StatusWarning, WorkspaceHealth, WorkspaceStatus,
};
use strata_store::{Lifecycle, NewProposal};
use strata_types::{
    CheckpointReason, Code, CommitOid, DepsSetId, OverlayEntry, SessionId, StrataError, TreeOid,
};
use support::*;

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

/// Commit to main and publish it to origin.
fn push(fx: &Fx, repo: &str, files: &[(&str, &[u8])], msg: &str) -> String {
    let main = fx.main(repo);
    let c = fx.sb.commit(&main, files, msg);
    fx.sb.git(&main, &["push", "-q", "origin", "HEAD:main"]);
    c
}

fn ignore(fx: &Fx, repo: &str, extra: &str) {
    let main = fx.main(repo);
    let mut g = fs::read(main.join(".gitignore")).unwrap();
    g.extend_from_slice(extra.as_bytes());
    push(fx, repo, &[(".gitignore", &g)], "ignore");
}

fn wstat<'a>(st: &'a strata_session::SessionStatus, repo: &str) -> &'a WorkspaceStatus {
    &st.workspaces[&repo_name(repo)]
}

fn scene(id: &SessionId) -> serde_json::Value {
    serde_json::to_value(Scene {
        op: SceneOp::Remove,
        session: id.clone(),
        repo: None,
        phase: "test".into(),
        at: SystemTime::now(),
        kept: vec![],
        error: StrataError::new(Code::Unknown, "test scene"),
    })
    .unwrap()
}

fn make_unknown(fx: &Fx, id: &SessionId) {
    let st = fx.store();
    assert!(st.set_lifecycle(id, Lifecycle::Active, Lifecycle::Removing, None, SystemTime::now()).unwrap());
    assert!(st.set_lifecycle(id, Lifecycle::Removing, Lifecycle::Unknown, Some(&scene(id)), SystemTime::now()).unwrap());
}

fn make_removed(fx: &Fx, id: &SessionId) {
    let st = fx.store();
    assert!(st.set_lifecycle(id, Lifecycle::Active, Lifecycle::Removing, None, SystemTime::now()).unwrap());
    assert!(st.set_lifecycle(id, Lifecycle::Removing, Lifecycle::Removed, None, SystemTime::now()).unwrap());
}

/// A manual checkpoint of one workspace through overlay.
fn checkpoint(fx: &Fx, rep: &NewReport, repo: &str) -> Vec<OverlayEntry> {
    let rn = repo_name(repo);
    let st = &rep.manifest.repos[&rn];
    let store = fx.store();
    let manifest = store.manifest(&rn, &st.base.tree).unwrap().expect("base manifest");
    let base = Base { r#gen: st.base.clone(), manifest: Arc::new(manifest), dir: store.base_dir(&st.base) };
    let wref = WorkspaceRef {
        session: &rep.manifest.id,
        repo: &rn,
        root: &st.path,
        config: &fx.config.repos[&rn],
        git: &fx.gits[&rn],
        store,
    };
    let reason = CheckpointReason::Manual { message: "test".into() };
    strata_overlay::checkpoint(&wref, &base, None, reason).unwrap().checkpoint.entries
}

fn entry_paths(e: &[OverlayEntry]) -> BTreeSet<String> {
    e.iter().map(|x| x.path.as_str().to_string()).collect()
}

fn base_dirs(fx: &Fx, repo: &str) -> BTreeSet<String> {
    match fs::read_dir(fx.root().join("bases").join(repo)) {
        Ok(rd) => rd
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| !n.starts_with('.'))
            .collect(),
        Err(_) => BTreeSet::new(),
    }
}

fn has_warning(w: &WorkspaceStatus, f: impl Fn(&StatusWarning) -> bool) -> bool {
    w.warnings.iter().any(f)
}

fn mirror_items(w: &WorkspaceStatus) -> Option<Vec<serde_json::Value>> {
    w.warnings.iter().find_map(|x| match x {
        StatusWarning::MirrorDrift { items } => Some(items.iter().map(|i| serde_json::to_value(i).unwrap()).collect()),
        _ => None,
    })
}

// ---------------------------------------------------------------- §5.3 / §5.4

#[test]
fn new_same_ticket_creates_another_session() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let a = s.new(Some("9246"), &[fx.rn("tool")]).unwrap();
    let bases = base_dirs(&fx, "tool");
    let installs = fx.sb.install_count();
    let b = s.new(Some("9246"), &[fx.rn("tool")]).unwrap();
    assert_ne!(id(&a), id(&b));
    assert_ne!(session_root(&fx, &id(&a)), session_root(&fx, &id(&b)));
    assert_ne!(ws(&a, "tool"), ws(&b, "tool"));
    assert!(shadow(&fx, &id(&a), "tool").is_dir());
    assert!(shadow(&fx, &id(&b), "tool").is_dir());
    assert_ne!(shadow(&fx, &id(&a), "tool"), shadow(&fx, &id(&b), "tool"));
    // the base and deps set are reused, not rebuilt
    assert_eq!(a.manifest.repos[&fx.rn("tool")].base, b.manifest.repos[&fx.rn("tool")].base);
    assert_eq!(base_dirs(&fx, "tool"), bases);
    assert_eq!(fx.sb.install_count(), installs);
    let e = s.select("9246").unwrap_err();
    assert_eq!(e.code, Code::AmbiguousMatch, "{e:?}");
    let got: BTreeSet<String> =
        e.detail.as_ref().unwrap()["candidates"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    let want: BTreeSet<String> = [id(&a), id(&b)].iter().map(|i| i.as_str().to_string()).collect();
    assert_eq!(got, want);
}

#[test]
fn new_lost_reply_is_found_not_replayed() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let _ = s.new(Some("lost-1"), &[fx.rn("tool")]).unwrap();
    let list = s.list().unwrap();
    assert_eq!(list.len(), 1);
    let found = s.select("lost-1").unwrap();
    assert_eq!(found, list[0]);
    assert_eq!(s.status(&found).unwrap().state, SessionState::Active);
    // no replay: another `new` is another session
    let again = s.new(Some("lost-1"), &[fx.rn("tool")]).unwrap();
    assert_ne!(id(&again), found);
    assert_eq!(s.list().unwrap(), vec![found, id(&again)]);
}

#[test]
fn resolve_by_cwd_inside_workspace() {
    let mut fx = Fx::with(&["tool"]);
    let (sid, w) = {
        let s = fx.sessions();
        let rep = s.new(None, &[fx.rn("tool")]).unwrap();
        let sid = id(&rep);
        let w = ws(&rep, "tool");
        fs::create_dir_all(w.join("src/app")).unwrap();
        let want = Some((sid.clone(), fx.rn("tool")));
        assert_eq!(s.resolve(&w.join("src/app")).unwrap(), want);
        assert_eq!(s.resolve(&w).unwrap(), want);
        // another spelling of the same directory (case-insensitive volume)
        let upper = session_root(&fx, &sid).join("TOOL").join("SRC").join("App");
        assert_eq!(s.resolve(&upper).unwrap(), want);
        // through a symlink
        let link = fx.sb.path("ws-link");
        symlink(w.join("src"), &link).unwrap();
        assert_eq!(s.resolve(&link.join("app")).unwrap(), want);

        let root = session_root(&fx, &sid);
        fs::create_dir_all(root.join("node_modules")).unwrap();
        fs::create_dir_all(root.join(".strata-tmp-move-tool-2")).unwrap();
        fs::create_dir_all(root.join(".strata-tmp-restore-tool")).unwrap();
        for p in [
            root.clone(),
            root.join("node_modules"),
            root.join(".strata-tmp-move-tool-2"),
            root.join(".strata-tmp-restore-tool"),
            shadow(&fx, &sid, "tool"),
            fx.main("tool"),
            fx.main("tool").join("src"),
            w.join("src/nope"),
            fx.root(),
            fx.root().join("sessions"),
        ] {
            assert_eq!(s.resolve(&p).unwrap(), None, "{}", p.display());
        }
        fs::remove_dir(root.join(".strata-tmp-move-tool-2")).unwrap();
        fs::remove_dir(root.join(".strata-tmp-restore-tool")).unwrap();

        let unk = s.new(None, &[fx.rn("tool")]).unwrap();
        make_unknown(&fx, &id(&unk));
        assert_eq!(s.resolve(&ws(&unk, "tool")).unwrap(), Some((id(&unk), fx.rn("tool"))));
        let gone = s.new(None, &[fx.rn("tool")]).unwrap();
        make_removed(&fx, &id(&gone));
        assert!(ws(&gone, "tool").is_dir());
        assert_eq!(s.resolve(&ws(&gone, "tool")).unwrap(), None);
        (sid, w)
    };

    // The store cannot be read: Err, not None. A freshly opened store has no
    // pooled readers, so the next read opens store.db and is refused.
    fx.close_store();
    fx.reopen_store();
    let db = fx.root().join("store/store.db");
    let mode = fs::metadata(&db).unwrap().permissions().mode();
    fs::set_permissions(&db, fs::Permissions::from_mode(0o000)).unwrap();
    let r = fx.sessions().resolve(&w);
    fs::set_permissions(&db, fs::Permissions::from_mode(mode)).unwrap();
    assert!(r.is_err(), "unreadable store gave {r:?}");
    assert_eq!(fx.sessions().resolve(&w).unwrap(), Some((sid, fx.rn("tool"))));
}

#[test]
fn select_by_id_or_ticket() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let one = id(&s.new(Some("one"), &[fx.rn("tool")]).unwrap());
    let two_a = id(&s.new(Some("two"), &[fx.rn("tool")]).unwrap());
    let two_b = id(&s.new(Some("two"), &[fx.rn("tool")]).unwrap());
    let plain = id(&s.new(None, &[fx.rn("tool")]).unwrap());

    assert_eq!(s.select(one.as_str()).unwrap(), one);
    assert_eq!(s.select(plain.as_str()).unwrap(), plain);
    assert_eq!(s.select("one").unwrap(), one);
    let e = s.select("three").unwrap_err();
    assert_eq!(e.code, Code::NoSuchSession, "{e:?}");
    let e = s.select("s-zzzzzzzz").unwrap_err();
    assert_eq!(e.code, Code::NoSuchSession, "{e:?}");
    let e = s.select("two").unwrap_err();
    assert_eq!(e.code, Code::AmbiguousMatch, "{e:?}");
    let c: Vec<&str> = e.detail.as_ref().unwrap()["candidates"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(c, vec![two_a.as_str(), two_b.as_str()]);
    for bad in ["", "../x", "a/b", ".hidden", "S-ABCDEFGH", "has space"] {
        let e = s.select(bad).unwrap_err();
        assert_eq!(e.code, Code::InvalidInput, "{bad:?}: {e:?}");
    }

    // Unknown stays listed and selectable; Removed is gone everywhere.
    make_unknown(&fx, &two_b);
    assert_eq!(s.select(two_b.as_str()).unwrap(), two_b);
    let st = s.status(&two_b).unwrap();
    assert_eq!(st.state, SessionState::Unknown);
    let sc = st.scene.expect("scene");
    assert_eq!((sc.op, sc.phase.as_str()), (SceneOp::Remove, "test"));
    make_removed(&fx, &one);
    let e = s.select(one.as_str()).unwrap_err();
    assert_eq!(e.code, Code::NoSuchSession, "{e:?}");
    let e = s.select("one").unwrap_err();
    assert_eq!(e.code, Code::NoSuchSession, "{e:?}");
    let e = s.status(&one).unwrap_err();
    assert_eq!(e.code, Code::NoSuchSession, "{e:?}");
    assert_eq!(s.list().unwrap(), vec![two_a, two_b, plain]);
}

// ---------------------------------------------------------------- isolation

#[test]
fn sessions_on_one_base_are_isolated() {
    let fx = Fx::with(&["tool"]);
    // main ignores scratch.log through info/exclude when the sessions are made
    let main_exclude = fx.main("tool").join(".git/info/exclude");
    let exclude_before = fs::read(&main_exclude).unwrap_or_default();
    let mut ex = exclude_before.clone();
    ex.extend_from_slice(b"scratch.log\n");
    fs::create_dir_all(main_exclude.parent().unwrap()).unwrap();
    fs::write(&main_exclude, &ex).unwrap();

    let s = fx.sessions();
    let a = s.new(None, &[fx.rn("tool")]).unwrap();
    let b = s.new(None, &[fx.rn("tool")]).unwrap();
    assert_eq!(a.manifest.repos[&fx.rn("tool")].base, b.manifest.repos[&fx.rn("tool")].base);
    let (wa, wb) = (ws(&a, "tool"), ws(&b, "tool"));
    let rec_b = fx.store().session(&id(&b)).unwrap().unwrap();
    let cp_b = fx.store().latest_checkpoint(&id(&b), &fx.rn("tool")).unwrap();
    let snap_b = snapshot(&wb);
    let shadow_b = snapshot(&shadow(&fx, &id(&b), "tool"));

    // write, delete, rename in A
    fx.sb.write(&wa, "src/new.ts", b"export const n = 1\n");
    fs::remove_file(wa.join("src/index.ts")).unwrap();
    fs::rename(wa.join("patches/a.patch"), wa.join("patches/b.patch")).unwrap();
    fx.sb.write(&wa, "scratch.log", b"scratch\n");
    let entries_a = checkpoint(&fx, &a, "tool");
    assert!(!entry_paths(&entries_a).contains("scratch.log"));
    assert_eq!(snapshot(&wb), snap_b);
    assert_eq!(fx.store().session(&id(&b)).unwrap().unwrap(), rec_b);
    assert_eq!(fx.store().latest_checkpoint(&id(&b), &fx.rn("tool")).unwrap(), cp_b);

    // stash and branches live in A's shadow
    fx.sb.git(&wa, &["stash", "-q"]);
    assert!(wa.join("src/index.ts").exists());
    assert_eq!(fx.sb.git(&wb, &["stash", "list"]), "");
    fx.sb.git(&wa, &["stash", "pop", "-q"]);
    assert!(!wa.join("src/index.ts").exists());
    fx.sb.git(&wa, &["branch", "x"]);
    assert_eq!(fx.sb.git(&wb, &["branch", "--list", "x"]), "");
    assert_eq!(fx.sb.git(&fx.main("tool"), &["branch", "--list", "x"]), "");
    assert_eq!(snapshot(&wb), snap_b);

    // main stops ignoring scratch.log; A's mirror is unchanged
    fs::write(&main_exclude, &exclude_before).unwrap();
    let o = fx.sb.git_output(&wa, &["check-ignore", "-q", "scratch.log"]);
    assert!(o.status.success(), "A no longer ignores scratch.log");
    assert_eq!(checkpoint(&fx, &a, "tool"), entries_a);
    let st = s.status(&id(&a)).unwrap();
    let items = mirror_items(wstat(&st, "tool")).expect("MirrorDrift");
    assert!(items.contains(&serde_json::json!({"reason": "info_exclude"})), "{items:?}");

    // a refused new changes neither
    let rec_a = fx.store().session(&id(&a)).unwrap().unwrap();
    let snap_a = snapshot(&wa);
    let e = s.new(None, &[repo_name("nope")]).unwrap_err();
    assert_eq!(e.code, Code::NoSuchRepo, "{e:?}");
    assert_eq!(fx.store().session(&id(&a)).unwrap().unwrap(), rec_a);
    assert_eq!(fx.store().session(&id(&b)).unwrap().unwrap(), rec_b);
    assert_eq!(snapshot(&wa), snap_a);
    assert_eq!(snapshot(&wb), snap_b);
    assert_eq!(snapshot(&shadow(&fx, &id(&b), "tool")), shadow_b);
}

// ---------------------------------------------------------------- §5.5 status

#[test]
fn seed_files_listed_in_status_and_never_checkpointed() {
    let fx = Fx::with(&["tool"]);
    ignore(&fx, "tool", ".env\n");
    let main = fx.main("tool");
    fs::write(main.join(".env"), b"A=1\n").unwrap();
    let s = fx.sessions();
    let rep = s.new(None, &[fx.rn("tool")]).unwrap();
    let w = ws(&rep, "tool");

    let st = s.status(&id(&rep)).unwrap();
    let t = wstat(&st, "tool");
    assert_eq!(t.seeds.len(), 1, "{:?}", t.seeds);
    let seed = &t.seeds[0];
    assert_eq!(seed.path.as_str(), ".env");
    assert_eq!(seed.source, main.join(".env"));
    assert_eq!(seed.size, 4);
    let v = serde_json::to_value(seed).unwrap();
    let keys: BTreeSet<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, BTreeSet::from(["path", "source", "size", "source_mtime", "copied"]));
    assert!(t.warnings.is_empty(), "{:?}", t.warnings);

    fs::write(w.join(".env"), b"A=2\n").unwrap();
    assert!(!entry_paths(&checkpoint(&fx, &rep, "tool")).contains(".env"));

    // main's .env changes: stale
    std::thread::sleep(Duration::from_millis(20));
    fs::write(main.join(".env"), b"A=3\n").unwrap();
    let st = s.status(&id(&rep)).unwrap();
    let t = wstat(&st, "tool");
    assert!(has_warning(t, |x| matches!(x, StatusWarning::SeedStale { path } if path.as_str() == ".env")), "{:?}", t.warnings);

    // the session stops ignoring .env: not ignored, still never checkpointed
    let g = fs::read_to_string(w.join(".gitignore")).unwrap().replace(".env\n", "");
    fs::write(w.join(".gitignore"), g).unwrap();
    let st = s.status(&id(&rep)).unwrap();
    let t = wstat(&st, "tool");
    assert!(
        has_warning(t, |x| matches!(x, StatusWarning::SeedNotIgnored { path } if path.as_str() == ".env")),
        "{:?}",
        t.warnings
    );
    let paths = entry_paths(&checkpoint(&fx, &rep, "tool"));
    assert!(paths.contains(".gitignore"), "{paths:?}");
    assert!(!paths.contains(".env"), "{paths:?}");
    let porcelain = fx.sb.git(&w, &["status", "--porcelain"]);
    assert!(porcelain.lines().any(|l| l == "?? .env"), "{porcelain}");
}

#[test]
fn status_warns_untracked_repo_edits_not_checkpointed() {
    let fx = Fx::new();
    let s = fx.sessions();
    let a = s.new(None, &[fx.rn("tool")]).unwrap();
    // a pending proposal for front that a tracked front would list
    push(&fx, "front", &[("bun.lock", b"lock v2\n")], "bump");
    let b = s.new(None, &[fx.rn("front")]).unwrap();
    let d1 = a.manifest.repos[&fx.rn("front")].base.deps.clone();
    let d2 = b.manifest.repos[&fx.rn("front")].base.deps.clone();
    assert_ne!(d1, d2);
    record_pending(&fx, "front", &d1, &d2);

    checkpoint(&fx, &a, "tool");
    fx.sb.write(&ws(&a, "front"), "src/edit.ts", b"x\n");
    let st = s.status(&id(&a)).unwrap();
    let f = wstat(&st, "front");
    assert!(f.warnings.contains(&StatusWarning::NotCheckpointed), "{:?}", f.warnings);
    assert_eq!(f.checkpoint, None);
    assert!(f.pending.is_empty(), "{:?}", f.pending);
    let t = wstat(&st, "tool");
    assert!(!t.warnings.contains(&StatusWarning::NotCheckpointed), "{:?}", t.warnings);
    assert!(t.checkpoint.is_some());
    // b tracks front but is already on the proposal's target
    let st = s.status(&id(&b)).unwrap();
    assert!(wstat(&st, "front").pending.is_empty());
    assert!(wstat(&st, "tool").warnings.contains(&StatusWarning::NotCheckpointed));
}

fn deps_inputs(fx: &Fx, repo: &str, d: &DepsSetId) -> String {
    strata_deps::find_set(fx.store(), &repo_name(repo), d).unwrap().expect("deps set").inputs_hash
}

fn record_pending(fx: &Fx, repo: &str, from: &DepsSetId, to: &DepsSetId) -> strata_types::ProposalId {
    let main = fx.main(repo);
    let head = fx.sb.head(&main);
    let tree = fx.sb.tree_of(&main, "HEAD");
    // `new` may already have proposed the main tip's set; replace it.
    let occupant = strata_deps::open_proposals(fx.store(), Some(&repo_name(repo)))
        .unwrap()
        .into_iter()
        .find(|p| matches!(p.state, strata_deps::ProposalState::Pending))
        .map(|p| p.id);
    fx.store()
        .record_proposal(
            &NewProposal {
                repo: repo_name(repo),
                origin: None,
                from: from.clone(),
                from_inputs: deps_inputs(fx, repo, from),
                to: to.clone(),
                to_inputs: deps_inputs(fx, repo, to),
                tree: Some(TreeOid::try_from(tree).unwrap()),
                head: Some(CommitOid::try_from(head).unwrap()),
            },
            occupant,
            SystemTime::now(),
        )
        .unwrap()
        .expect("proposal recorded")
}

#[test]
fn status_reads_workspace_presence_from_filesystem() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = s.new(None, &[fx.rn("tool")]).unwrap();
    let sid = id(&rep);
    let w = ws(&rep, "tool");
    let rec = fx.store().session(&sid).unwrap().unwrap();
    let health = |s: &Sessions| wstat(&s.status(&sid).unwrap(), "tool").health;
    assert_eq!(health(&s), WorkspaceHealth::Ok);

    let away = fx.sb.path("away");
    fs::rename(&w, &away).unwrap();
    assert_eq!(health(&s), WorkspaceHealth::Missing);
    assert_eq!(fx.store().session(&sid).unwrap().unwrap(), rec);
    fs::rename(&away, &w).unwrap();
    assert_eq!(health(&s), WorkspaceHealth::Ok);

    fs::rename(w.join(".git"), fx.sb.path("dotgit")).unwrap();
    assert_eq!(health(&s), WorkspaceHealth::Missing);
    assert_eq!(fx.store().session(&sid).unwrap().unwrap(), rec);
    fs::rename(fx.sb.path("dotgit"), w.join(".git")).unwrap();
    assert_eq!(health(&s), WorkspaceHealth::Ok);

    let scene_dir = session_root(&fx, &sid).join(".strata-tmp-restore-tool");
    fs::create_dir_all(&scene_dir).unwrap();
    let restore = |s: &Sessions| {
        wstat(&s.status(&sid).unwrap(), "tool")
            .warnings
            .iter()
            .any(|x| matches!(x, StatusWarning::RestoreScene { path } if path == &scene_dir))
    };
    assert!(!restore(&s), "an empty restore dir is no scene");
    fs::write(scene_dir.join("plan.json"), b"{}").unwrap();
    assert!(restore(&s));
    fs::remove_dir_all(&scene_dir).unwrap();
    assert!(!restore(&s));
    assert_eq!(fx.store().session(&sid).unwrap().unwrap(), rec);
}

#[test]
fn status_reports_drift_and_pending_proposals() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let a = s.new(None, &[fx.rn("tool")]).unwrap();
    let w = ws(&a, "tool");
    let d1 = a.manifest.repos[&fx.rn("tool")].base.deps.clone();
    let drift = |s: &Sessions| wstat(&s.status(&id(&a)).unwrap(), "tool").drift.clone();
    assert_eq!(drift(&s), None);

    fx.sb.write(&w, "package.json", PACKAGE_JSON.replace(r#""x": "^1.0.0""#, r#""x": "^1.0.0", "y": "^2.0.0""#).as_bytes());
    let d = drift(&s).expect("drift");
    assert_eq!(d.pinned, d1);
    assert_eq!(d.pinned_inputs, deps_inputs(&fx, "tool", &d1));
    assert_ne!(d.found_inputs, d.pinned_inputs);
    fx.sb.write(&w, "package.json", PACKAGE_JSON.replace("1.0.87", "1.0.88").as_bytes());
    assert_eq!(drift(&s), None, "a version bump alone is no drift");
    fx.sb.write(&w, "package.json", PACKAGE_JSON.as_bytes());

    push(&fx, "tool", &[("bun.lock", b"lock v2\n")], "bump");
    let b = s.new(None, &[fx.rn("tool")]).unwrap();
    let d2 = b.manifest.repos[&fx.rn("tool")].base.deps.clone();
    assert_ne!(d1, d2);
    let pid = record_pending(&fx, "tool", &d1, &d2);
    let pa = wstat(&s.status(&id(&a)).unwrap(), "tool").pending.clone();
    assert_eq!(pa.iter().map(|p| p.id).collect::<Vec<_>>(), vec![pid]);
    assert_eq!(pa[0].to, d2);
    assert!(wstat(&s.status(&id(&b)).unwrap(), "tool").pending.is_empty(), "b is already on the target");
}

#[test]
fn status_reports_mirror_drift_and_vanished_shadow() {
    let fx = Fx::new();
    let s = fx.sessions();
    let rep = s.new(None, &[fx.rn("tool"), fx.rn("front")]).unwrap();
    let sid = id(&rep);
    let wa = ws(&rep, "tool");
    let sh = shadow(&fx, &sid, "tool");
    let shadow_info = snapshot(&sh.join("info"));
    let shadow_config = fs::read(sh.join("config")).unwrap();
    let st = s.status(&sid).unwrap();
    assert!(wstat(&st, "tool").warnings.is_empty(), "{:?}", wstat(&st, "tool").warnings);
    assert!(wstat(&st, "front").warnings.is_empty());

    let main = fx.main("tool");
    let ex = main.join(".git/info/exclude");
    let mut bytes = fs::read(&ex).unwrap_or_default();
    bytes.extend_from_slice(b"*.tmp\n");
    fs::write(&ex, bytes).unwrap();
    let global = fx.sb.path("global-ignore");
    fs::write(&global, b"*.bak\n").unwrap();
    fx.sb.git(&main, &["config", "core.excludesFile", global.to_str().unwrap()]);
    let st = s.status(&sid).unwrap();
    let items = mirror_items(wstat(&st, "tool")).expect("MirrorDrift");
    assert_eq!(
        items,
        vec![serde_json::json!({"reason": "info_exclude"}), serde_json::json!({"reason": "config", "key": "core.excludesfile"})]
    );
    assert_eq!(wstat(&st, "tool").warnings.len(), 1);
    assert!(wstat(&st, "front").warnings.is_empty());
    assert_eq!(snapshot(&sh.join("info")), shadow_info);
    assert_eq!(fs::read(sh.join("config")).unwrap(), shadow_config);

    // husky's install step
    fx.sb.git(&wa, &["config", "core.hooksPath", ".husky/_"]);
    let config_after = fs::read(sh.join("config")).unwrap();
    let st = s.status(&sid).unwrap();
    let items = mirror_items(wstat(&st, "tool")).expect("MirrorDrift");
    assert_eq!(items.len(), 3, "{items:?}");
    let guard = &items[2];
    assert_eq!(guard["reason"], "pre_push_guard_disabled");
    assert_eq!(guard["hook_replaced"], false);
    let hp = PathBuf::from(guard["hooks_path"].as_str().unwrap());
    assert!(hp.is_absolute() && hp.ends_with(".husky/_"), "{hp:?}");
    let ws_real = fs::canonicalize(&wa).unwrap();
    assert!(hp == wa.join(".husky/_") || hp == ws_real.join(".husky/_"), "{hp:?}");
    assert_eq!(fs::read(sh.join("config")).unwrap(), config_after, "status does not pin the config back");
    assert!(wstat(&st, "front").warnings.is_empty());

    // a hooks lookup git refuses
    fx.sb.git(&wa, &["config", "extensions.worktreeConfig", "true"]);
    fx.sb.git(&wa, &["config", "--worktree", "core.hooksPath", ""]);
    let st = s.status(&sid).unwrap();
    let t = wstat(&st, "tool");
    let unknown: Vec<&StrataError> = t
        .warnings
        .iter()
        .filter_map(|x| match x {
            StatusWarning::PrePushGuardUnknown { detail } => Some(detail),
            _ => None,
        })
        .collect();
    assert_eq!(unknown.len(), 1, "{:?}", t.warnings);
    assert_eq!(unknown[0].code, Code::Unknown);
    assert_eq!(unknown[0].detail.as_ref().unwrap()["reason"], "hooks-lookup");
    assert!(mirror_items(t).is_none(), "{:?}", t.warnings);
    assert!(wstat(&st, "front").warnings.is_empty());

    // the shadow vanishes: Missing, ShadowMissing, never rebuilt
    fs::remove_dir_all(&sh).unwrap();
    let st = s.status(&sid).unwrap();
    let t = wstat(&st, "tool");
    assert_eq!(t.health, WorkspaceHealth::Missing);
    assert!(t.warnings.contains(&StatusWarning::ShadowMissing { path: sh.clone() }), "{:?}", t.warnings);
    assert_eq!(wstat(&st, "front").health, WorkspaceHealth::Ok);
    assert!(!sh.exists());
}
