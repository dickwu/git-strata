//! session.md §8 Task 6: `remove` (§5.6), with the parts of the §5.5 and
//! §5.7 rows that need it.

mod support;

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::symlink;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;
use strata_overlay::{Base, WorkspaceRef};
use strata_session::{MoveReport, NewReport, SessionSeam, SessionState, Sessions};
use strata_store::{Lifecycle, MoveState};
use strata_types::{
    Action, BaseGen, CheckpointReason, Code, CommitOid, ConfirmToken, OverlayEntry, RepoId, SessionId, StrataError,
};
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

fn repos_dir(fx: &Fx, id: &SessionId) -> PathBuf {
    fx.root().join("sessions/.repos").join(id.as_str())
}

fn shadow(fx: &Fx, id: &SessionId, repo: &str) -> PathBuf {
    repos_dir(fx, id).join(format!("{repo}.git"))
}

fn rid(fx: &Fx, repo: &str) -> RepoId {
    fx.gits[&fx.rn(repo)].repo_id()
}

fn trash(fx: &Fx) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(fx.root().join("store/trash"))
        .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

fn push(fx: &Fx, repo: &str, files: &[(&str, &[u8])], msg: &str) -> String {
    let main = fx.main(repo);
    let c = fx.sb.commit(&main, files, msg);
    fx.sb.git(&main, &["push", "-q", "origin", "HEAD:main"]);
    c
}

fn target(fx: &Fx, s: &Sessions<'_>, repo: &str, files: &[(&str, &[u8])]) -> (BaseGen, CommitOid) {
    let h = commit_oid(&push(fx, repo, files, "upstream"));
    (s.ensure_base(&fx.rn(repo), &h).unwrap(), h)
}

fn mv(fx: &Fx, s: &Sessions<'_>, id: &SessionId, to: &(BaseGen, CommitOid)) -> strata_session::Result<MoveReport> {
    s.move_workspace(id, &rid(fx, "tool"), to.0.clone(), to.1.clone())
}

fn new_all(fx: &Fx, s: &Sessions<'_>) -> NewReport {
    s.new(None, &fx.all()).unwrap()
}

fn lifecycle(fx: &Fx, id: &SessionId) -> Lifecycle {
    fx.store().session(id).unwrap().unwrap().lifecycle
}

fn tok(id: &SessionId) -> Option<ConfirmToken> {
    Some(ConfirmToken { action: Action::RemoveDiscard { session: id.clone(), kept_trees: vec![] }, issued: Instant::now(), nonce: 1 })
}

fn detail(e: &StrataError) -> &serde_json::Map<String, Value> {
    e.detail.as_ref().and_then(|d| d.as_object()).expect("detail is an object")
}

fn kinds(e: &StrataError) -> Vec<String> {
    detail(e)["unresolved"].as_array().unwrap().iter().map(|u| u["kind"].as_str().unwrap().to_string()).collect()
}

fn open_pids(e: &StrataError) -> Vec<i64> {
    detail(e)["open"].as_array().unwrap().iter().map(|o| o["pid"].as_i64().unwrap()).collect()
}

fn base_of(fx: &Fx, g: &BaseGen) -> Base {
    let store = fx.store();
    let manifest = store.manifest(&g.repo, &g.tree).unwrap().expect("base manifest");
    Base { r#gen: g.clone(), manifest: std::sync::Arc::new(manifest), dir: store.base_dir(g) }
}

fn overlay(fx: &Fx, id: &SessionId, root: &Path, g: &BaseGen) -> Vec<OverlayEntry> {
    let rn = fx.rn("tool");
    let w = WorkspaceRef {
        session: id,
        repo: &rn,
        root,
        config: &fx.config.repos[&rn],
        git: &fx.gits[&rn],
        store: fx.store(),
    };
    strata_overlay::reconcile_full(&w, &base_of(fx, g)).unwrap().entries
}

/// Everything a refused remove must leave as it was.
#[derive(Debug, PartialEq)]
struct Untouched {
    dirs: Vec<String>,
    root: Vec<Entry>,
    shadow: Vec<Entry>,
    pins: String,
    lifecycle: Lifecycle,
    intents: Vec<strata_store::MoveIntent>,
    cps: usize,
}

fn untouched(fx: &Fx, id: &SessionId) -> Untouched {
    Untouched {
        dirs: fx.session_dirs(),
        root: snapshot(&session_root(fx, id)),
        shadow: snapshot(&repos_dir(fx, id)),
        pins: fx.pins("tool"),
        lifecycle: lifecycle(fx, id),
        intents: fx.store().move_intents(id).unwrap(),
        cps: fx.store().checkpoints(id, None).unwrap().len(),
    }
}

fn assert_removed(fx: &Fx, id: &SessionId) {
    assert_eq!(lifecycle(fx, id), Lifecycle::Removed);
    assert!(!session_root(fx, id).exists(), "root retired");
    assert!(!repos_dir(fx, id).exists(), ".repos retired");
    assert!(!fx.pins("tool").contains(id.as_str()), "pin deleted");
}

// ---------------------------------------------------------------- holders

const WORKER_ENV: &str = "STRATA_SESSION_REMOVE_WORKER";

/// Worker mode: hold something, say READY, then wait on stdin (or, for
/// `pipe`, block writing into a pipe nobody reads).
#[test]
fn holder_worker() {
    let Ok(spec) = std::env::var(WORKER_ENV) else { return };
    let (kind, path) = spec.split_once(':').unwrap();
    let mut keep: Vec<fs::File> = Vec::new();
    match kind {
        "fd" | "pipe" => keep.push(fs::File::open(path).unwrap()),
        "mmap" => {
            let f = fs::File::open(path).unwrap();
            let len = f.metadata().unwrap().len() as usize;
            // SAFETY: read-only private mapping of an open file.
            let p = unsafe {
                libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_PRIVATE, f.as_raw_fd(), 0)
            };
            assert_ne!(p, libc::MAP_FAILED);
            drop(f);
        }
        "cwd" => {}
        _ => panic!("unknown worker {kind}"),
    }
    let mut out = std::io::stdout();
    writeln!(out, "READY").unwrap();
    out.flush().unwrap();
    if kind == "pipe" {
        let chunk = vec![b'x'; 65536];
        loop {
            if out.write_all(&chunk).is_err() {
                break;
            }
        }
    }
    let mut sink = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut sink);
    std::process::exit(0);
}

/// A holder process; killed on drop.
struct Worker {
    child: Child,
    _out: BufReader<std::process::ChildStdout>,
}

impl Worker {
    fn spawn(spec: String, cwd: Option<&Path>) -> Worker {
        let mut c = Command::new(std::env::current_exe().unwrap());
        c.args(["--exact", "holder_worker", "--nocapture", "--test-threads=1"])
            .env(WORKER_ENV, spec)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(d) = cwd {
            c.current_dir(d);
        }
        let mut child = c.spawn().unwrap();
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        loop {
            line.clear();
            assert!(out.read_line(&mut line).unwrap() > 0, "worker exited before READY");
            if line.contains("READY") {
                break;
            }
        }
        Worker { child, _out: out }
    }

    fn pid(&self) -> i64 {
        self.child.id() as i64
    }

    fn alive(&mut self) -> bool {
        self.child.try_wait().unwrap().is_none()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------- §7 rows

#[test]
fn workspace_git_never_writes_main_checkout() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let before = fx.main_snap("tool");
    let rep = new_all(&fx, &s);
    let id = id(&rep);
    let w = ws(&rep, "tool");
    let pin = format!("refs/strata/pins/{id}");
    let strip = |refs: &str| refs.lines().filter(|l| !l.starts_with(&pin)).collect::<Vec<_>>().join("\n");

    let c = fx.sb.commit(&w, &[("src/ws.ts", b"ws\n")], "in the workspace");
    fs::write(w.join("src/index.ts"), b"stashed\n").unwrap();
    fx.sb.git(&w, &["stash", "-q"]);
    fx.sb.git(&w, &["branch", "topic"]);
    assert!(!fx.sb.git_output(&w, &["fetch", "origin"]).status.success(), "no remote in the workspace");

    let during = fx.main_snap("tool");
    assert_eq!(during.tree, before.tree);
    assert_eq!(during.git, before.git);
    assert_eq!(during.index, before.index);
    assert_eq!(strip(&during.refs), strip(&before.refs), "only the pin is new");
    assert!(during.refs.contains(&pin));
    let main = fx.main("tool");
    assert!(!fx.sb.git_output(&main, &["cat-file", "-e", &c]).status.success(), "the commit is not in main");
    assert!(fx.sb.git_output(&shadow(&fx, &id, "tool"), &["cat-file", "-e", &c]).status.success());

    s.remove(&id, true, tok(&id)).unwrap();
    assert_eq!(fx.main_snap("tool"), before, "the pin is gone and nothing else changed");
    assert_removed(&fx, &id);
}

#[test]
fn removed_session_id_never_resolves_or_reissued() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = new_all(&fx, &s);
    let id = id(&rep);
    let w = ws(&rep, "tool");
    s.remove(&id, false, None).unwrap();
    assert_removed(&fx, &id);
    assert_eq!(s.select(id.as_str()).unwrap_err().code, Code::NoSuchSession);
    assert_eq!(s.status(&id).unwrap_err().code, Code::NoSuchSession);
    fs::create_dir_all(&w).unwrap();
    assert_eq!(s.resolve(&w).unwrap(), None, "a recreated directory is no session");
    let dirs = fx.session_dirs();
    s.set_next_id(id.clone());
    let e = s.new(None, &fx.all()).unwrap_err();
    assert_eq!(e.code, Code::Exists, "{e:?}");
    assert_eq!(reason(&e), Some("store_row"));
    assert_eq!(fx.session_dirs(), dirs, "the directory is not reused");
    assert_eq!(lifecycle(&fx, &id), Lifecycle::Removed);
}

#[test]
fn remove_refuses_unpublished_without_discard() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = new_all(&fx, &s);
    let id = id(&rep);
    let w = ws(&rep, "tool");
    fx.sb.write(&w, "src/new.ts", b"new\n");
    fs::write(w.join("src/index.ts"), b"changed\n").unwrap();
    let a = untouched(&fx, &id);
    let calls = fx.before.calls();
    let e = s.remove(&id, false, None).unwrap_err();
    assert_eq!(e.code, Code::Dirty, "{e:?}");
    let paths: Vec<&str> =
        detail(&e)["repos"]["tool"]["paths"].as_array().unwrap().iter().map(|p| p.as_str().unwrap()).collect();
    assert!(paths.contains(&"src/new.ts") && paths.contains(&"src/index.ts"), "{paths:?}");
    assert_eq!(untouched(&fx, &id), a, "a refusal changes nothing");
    assert_eq!(fx.before.calls(), calls, "no checkpoint");

    let base = rep.manifest.repos[&fx.rn("tool")].base.clone();
    let want = overlay(&fx, &id, &w, &base);
    let r = s.remove(&id, true, tok(&id)).unwrap();
    assert!(r.discarded);
    let cp = fx.store().checkpoint(&id, r.checkpoints[&fx.rn("tool")]).unwrap();
    assert_eq!(cp.reason, CheckpointReason::BeforeRemove);
    assert_eq!(cp.entries, want, "the last checkpoint holds the discarded overlay");
    assert_removed(&fx, &id);

    // A changed old tree also needs discard.
    let rep = new_all(&fx, &s);
    let id2 = rep.manifest.id.clone();
    let w = ws(&rep, "tool");
    let holder = Worker::spawn("cwd:".into(), Some(&w.join("src")));
    let to = target(&fx, &s, "tool", &[("src/up.ts", b"up\n")]);
    let old = mv(&fx, &s, &id2, &to).unwrap().old_tree.expect("kept while in use");
    drop(holder);
    fs::write(old.join("src/late.ts"), b"late\n").unwrap();
    let a = untouched(&fx, &id2);
    let e = s.remove(&id2, false, None).unwrap_err();
    assert_eq!(e.code, Code::Dirty, "{e:?}");
    let ot = detail(&e)["old_trees"].as_array().unwrap();
    assert_eq!(ot.len(), 1, "{e:?}");
    assert_eq!(PathBuf::from(ot[0]["path"].as_str().unwrap()), old);
    assert!(detail(&e)["repos"].as_object().unwrap().is_empty(), "{e:?}");
    assert_eq!(untouched(&fx, &id2), a);
    s.remove(&id2, true, tok(&id2)).unwrap();
    assert_removed(&fx, &id2);
}

#[test]
fn remove_untouched_untracked_repo_needs_no_discard() {
    let fx = Fx::new();
    let s = fx.sessions();
    let rep = s.new(Some("T-6"), &[fx.rn("tool")]).unwrap();
    let id = id(&rep);
    // `front` came along untracked (every sibling joins; design.md:302) and nobody touched it.
    let r = s.remove(&id, false, None).unwrap();
    assert!(r.checkpoints.contains_key(&fx.rn("tool")) && !r.checkpoints.contains_key(&fx.rn("front")), "{r:?}");
    assert_eq!(lifecycle(&fx, &id), Lifecycle::Removed);
    assert!(!session_root(&fx, &id).exists() && !repos_dir(&fx, &id).exists());
}

#[test]
fn remove_edited_untracked_repo_requires_discard() {
    let fx = Fx::new();
    let s = fx.sessions();
    let rep = s.new(Some("T-6"), &[fx.rn("tool")]).unwrap();
    let id = id(&rep);
    fx.sb.write(&ws(&rep, "front"), "src/stray.ts", b"stray\n");
    let a = untouched(&fx, &id);
    let e = s.remove(&id, false, None).unwrap_err();
    assert_eq!(e.code, Code::Dirty, "{e:?}");
    assert_eq!(detail(&e)["untracked_repos"], serde_json::json!(["front"]));
    assert_eq!(detail(&e)["repos"]["front"]["paths"], serde_json::json!(["src/stray.ts"]), "{e:?}");
    assert!(detail(&e)["repos"].get("tool").is_none(), "{e:?}");
    assert_eq!(untouched(&fx, &id), a);
    let r = s.remove(&id, true, tok(&id)).unwrap();
    assert!(r.checkpoints.contains_key(&fx.rn("tool")) && !r.checkpoints.contains_key(&fx.rn("front")), "{r:?}");
    assert_eq!(lifecycle(&fx, &id), Lifecycle::Removed);
    assert!(!session_root(&fx, &id).exists() && !repos_dir(&fx, &id).exists());
}

#[test]
fn remove_discard_checks_the_token() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let a_id = id(&new_all(&fx, &s));
    let b_id = id(&new_all(&fx, &s));
    let before = (untouched(&fx, &a_id), untouched(&fx, &b_id));
    let expected = serde_json::to_value(Action::RemoveDiscard { session: a_id.clone(), kept_trees: vec![] }).unwrap();
    let push = ConfirmToken {
        action: Action::PushDeploy { repo: fx.rn("tool"), branch: "main".into(), url: String::new() },
        issued: Instant::now(),
        nonce: 2,
    };
    for (what, t, why) in [("none", None, "missing"), ("other session", tok(&b_id), "mismatch"), ("push", Some(push), "mismatch")]
    {
        let e = s.remove(&a_id, true, t).unwrap_err();
        assert_eq!(e.code, Code::NotConfirmed, "{what}: {e:?}");
        assert_eq!(reason(&e), Some(why), "{what}");
        assert_eq!(detail(&e)["expected"], expected, "{what}");
        assert_eq!((untouched(&fx, &a_id), untouched(&fx, &b_id)), before, "{what}: nothing deleted");
    }
    s.remove(&a_id, true, tok(&a_id)).unwrap();
    assert_eq!(lifecycle(&fx, &b_id), Lifecycle::Active);
}

#[test]
fn remove_after_mtime_only_change_needs_no_discard() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let rep = new_all(&fx, &s);
    let id = id(&rep);
    let w = ws(&rep, "tool");
    let prev = fx.store().latest_checkpoint(&id, &fx.rn("tool")).unwrap().map(|c| c.id);
    std::thread::sleep(Duration::from_millis(20));
    let st = Command::new("/usr/bin/touch").arg(w.join("src/index.ts")).status().unwrap();
    assert!(st.success());
    let body = fs::read(w.join("package.json")).unwrap();
    fs::write(w.join("package.json"), &body).unwrap();
    let r = s.remove(&id, false, None).unwrap();
    assert!(!r.discarded);
    let cp = r.checkpoints[&fx.rn("tool")];
    if let Some(p) = prev {
        assert!(cp > p, "{cp:?} after {p:?}");
    }
    assert!(fx.store().checkpoint(&id, cp).unwrap().entries.is_empty());
    assert_removed(&fx, &id);
}

#[test]
fn remove_refuses_when_a_process_holds_fd_cwd_or_mmap() {
    for how in ["fd", "cwd_ws", "cwd_shadow", "mmap", "pipe"] {
        let fx = Fx::with(&["tool"]);
        let s = fx.sessions();
        let rep = new_all(&fx, &s);
        let id = id(&rep);
        let w = ws(&rep, "tool");
        let dep = w.join("node_modules/x/index.js");
        assert!(dep.is_file());
        let mut h = match how {
            "fd" => Worker::spawn(format!("fd:{}", w.join("src/index.ts").display()), None),
            "cwd_ws" => Worker::spawn("cwd:".into(), Some(&w.join("src"))),
            "cwd_shadow" => Worker::spawn("cwd:".into(), Some(&shadow(&fx, &id, "tool"))),
            "mmap" => Worker::spawn(format!("mmap:{}", dep.display()), None),
            "pipe" => Worker::spawn(format!("pipe:{}", w.join("src/index.ts").display()), None),
            _ => unreachable!(),
        };
        std::thread::sleep(Duration::from_millis(100));
        let a = untouched(&fx, &id);
        for discard in [false, true] {
            let t = if discard { tok(&id) } else { None };
            let e = s.remove(&id, discard, t).unwrap_err();
            assert_eq!(e.code, Code::OpenFiles, "{how}/{discard}: {e:?}");
            assert!(open_pids(&e).contains(&h.pid()), "{how}: {e:?}");
            assert_eq!(untouched(&fx, &id), a, "{how}: nothing changed");
            assert!(h.alive(), "{how}: the holder is never killed");
        }
        drop(h);
        s.remove(&id, false, None).unwrap();
        assert_removed(&fx, &id);
    }
}

#[test]
fn remove_open_files_in_one_session_never_block_another() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let a = new_all(&fx, &s);
    let b = new_all(&fx, &s);
    let mut h = Worker::spawn("cwd:".into(), Some(&ws(&a, "tool").join("src")));
    std::thread::sleep(Duration::from_millis(100));
    s.remove(&id(&b), false, None).unwrap();
    assert_eq!(lifecycle(&fx, &id(&b)), Lifecycle::Removed);
    let e = s.remove(&id(&a), false, None).unwrap_err();
    assert_eq!(e.code, Code::OpenFiles, "{e:?}");
    assert!(h.alive());
    assert_eq!(lifecycle(&fx, &id(&a)), Lifecycle::Active);
}

#[test]
fn remove_has_no_force_path() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    // Open files win over discard.
    let rep = new_all(&fx, &s);
    let a = id(&rep);
    let h = Worker::spawn("cwd:".into(), Some(&ws(&rep, "tool")));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(s.remove(&a, true, tok(&a)).unwrap_err().code, Code::OpenFiles);
    drop(h);

    // Step 5.1 takes the bounded lock: Busy, nothing changed.
    let rep = new_all(&fx, &s);
    let c = id(&rep);
    let to = target(&fx, &s, "tool", &[("src/up.ts", b"up\n")]);
    s.arm(SessionSeam::FailAt { phase: "move.prepare".into(), errno: libc::EIO });
    mv(&fx, &s, &c, &to).unwrap_err();
    assert_eq!(fx.store().move_intents(&c).unwrap().last().unwrap().state, MoveState::Preparing);
    fx.lock.set_wait(Duration::from_millis(200));
    let before = untouched(&fx, &c);
    {
        let _g = fx.lock.hold("tool");
        let e = s.remove(&c, false, None).unwrap_err();
        assert_eq!(e.code, Code::Busy, "{e:?}");
    }
    assert_eq!(untouched(&fx, &c), before, "Busy changes nothing");
    assert_eq!(lifecycle(&fx, &c), Lifecycle::Active);

    // Step 7 waits for the lock however long it is held.
    let d = id(&new_all(&fx, &s));
    let (tx, rx) = mpsc::channel();
    std::thread::scope(|sc| {
        let lock = &fx.lock;
        sc.spawn(move || {
            let g = lock.hold("tool");
            tx.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(1000));
            drop(g);
        });
        rx.recv().unwrap();
        let t = Instant::now();
        s.remove(&d, false, None).unwrap();
        assert!(t.elapsed() >= Duration::from_millis(700), "waited past the bounded wait: {:?}", t.elapsed());
    });
    assert_eq!(lifecycle(&fx, &d), Lifecycle::Removed);

    // `new` step 13 (git and pin) also waits past the bounded wait.
    s.arm(SessionSeam::PauseAt { phase: "new.git".into() });
    let r = std::thread::scope(|sc| {
        let t = sc.spawn(|| s.new(None, &fx.all()));
        s.wait_paused();
        let g = fx.lock.hold("tool");
        s.release();
        std::thread::sleep(Duration::from_millis(600));
        drop(g);
        t.join().unwrap()
    });
    let r = r.unwrap();
    assert_eq!(lifecycle(&fx, &id(&r)), Lifecycle::Active);
}

#[test]
fn remove_unresolved_scene_requires_discard_confirmation() {
    for how in ["ws_unknown", "missing", "restore", "new_unknown", "swapping"] {
        let fx = Fx::with(&["tool"]);
        let s = fx.sessions();
        let id = if how == "new_unknown" {
            s.arm(SessionSeam::FailKeepAt { phase: "new.workspace@tool".into(), errno: libc::ENOSPC });
            s.new(None, &fx.all()).unwrap_err();
            let rows = fx.store().sessions().unwrap();
            assert_eq!(rows.len(), 1);
            rows[0].manifest.id.clone()
        } else {
            id(&new_all(&fx, &s))
        };
        let w = session_root(&fx, &id).join("tool");
        let want = match how {
            "ws_unknown" => {
                let to = target(&fx, &s, "tool", &[("src/up.ts", b"up\n")]);
                s.arm(SessionSeam::FailAt { phase: "move.pin".into(), errno: libc::EIO });
                assert_eq!(mv(&fx, &s, &id, &to).unwrap_err().code, Code::Unknown);
                "workspace_unknown"
            }
            "missing" => {
                fs::remove_dir_all(&w).unwrap();
                "workspace_missing"
            }
            "restore" => {
                let d = session_root(&fx, &id).join(format!("{}tool", strata_overlay::RESTORE_TEMP_PREFIX));
                fs::create_dir_all(&d).unwrap();
                fs::write(d.join("plan.json"), b"{}").unwrap();
                "restore_scene"
            }
            "new_unknown" => "session_unknown",
            "swapping" => {
                let to = target(&fx, &s, "tool", &[("src/up.ts", b"up\n")]);
                s.arm(SessionSeam::StopAt { phase: "move.pin".into() });
                mv(&fx, &s, &id, &to).unwrap_err();
                assert_eq!(fx.store().move_intents(&id).unwrap().last().unwrap().state, MoveState::Swapping);
                "move_awaiting_recover"
            }
            _ => unreachable!(),
        };
        let a = untouched(&fx, &id);
        let e = s.remove(&id, false, None).unwrap_err();
        assert_eq!(e.code, Code::Dirty, "{how}: {e:?}");
        assert!(kinds(&e).iter().any(|k| k == want), "{how}: {e:?}");
        assert_eq!(untouched(&fx, &id), a, "{how}");
        if how == "swapping" {
            // Swapping is recover's; discard is not asked for here.
            continue;
        }
        let r = s.remove(&id, true, tok(&id)).unwrap();
        assert!(r.discarded, "{how}");
        assert_removed(&fx, &id);
    }

    // Only a MoveLeftover: no discard needed; the intent is closed.
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let id = id(&new_all(&fx, &s));
    let to = target(&fx, &s, "tool", &[("src/up.ts", b"up\n")]);
    s.arm(SessionSeam::FailAt { phase: "move.prepare".into(), errno: libc::EIO });
    mv(&fx, &s, &id, &to).unwrap_err();
    let n = fx.store().move_intents(&id).unwrap().last().unwrap().n;
    s.remove(&id, false, None).unwrap();
    let i = fx.store().move_intents(&id).unwrap().into_iter().find(|i| i.n == n).unwrap();
    assert_eq!(i.state, MoveState::Abandoned);
    assert_removed(&fx, &id);
}

#[test]
fn remove_failure_midway_is_unknown_and_keeps_scene() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let id = id(&new_all(&fx, &s));
    s.arm(SessionSeam::FailAt { phase: "remove.retire".into(), errno: libc::EIO });
    let e = s.remove(&id, false, None).unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    assert_eq!(detail_str(&e, "phase"), Some("retire"), "{e:?}");
    assert_ne!(fx.pins("tool"), "", "R4: the pin goes after the retire");
    assert!(session_root(&fx, &id).is_dir() && shadow(&fx, &id, "tool").is_dir(), "the scene is kept");
    assert_eq!(lifecycle(&fx, &id), Lifecycle::Unknown);
    let st = s.status(&id).unwrap();
    assert_eq!(st.state, SessionState::Unknown);
    let scene = st.scene.expect("scene");
    assert_eq!(scene.phase, "retire");
    let e = s.remove(&id, false, None).unwrap_err();
    assert_eq!(e.code, Code::Dirty, "{e:?}");
    assert!(kinds(&e).contains(&"session_unknown".to_string()));
    s.remove(&id, true, tok(&id)).unwrap();
    assert_removed(&fx, &id);
}

#[test]
fn remove_deletes_only_its_own_session() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let a = new_all(&fx, &s);
    let b = new_all(&fx, &s);
    let (a_id, b_id) = (id(&a), id(&b));
    let outside = fx.sb.path("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("keep.txt"), b"keep\n").unwrap();
    fs::create_dir_all(ws(&a, "tool").join("node_modules")).unwrap();
    symlink(&outside, ws(&a, "tool").join("node_modules/tailwindcss")).unwrap();
    let outside_before = snapshot(&outside);
    let b_before = untouched(&fx, &b_id);
    let g = &a.manifest.repos[&fx.rn("tool")].base;
    let bases = fx.store().base_dir(g).ancestors().nth(3).unwrap().to_path_buf();
    assert!(bases.ends_with("bases"), "{}", bases.display());
    let bases_before = snapshot(&bases);
    let main_before = fx.main_snap("tool");
    let trash_before = trash(&fx);
    let a_pin = format!("refs/strata/pins/{a_id}");

    s.remove(&a_id, true, tok(&a_id)).unwrap();
    assert_removed(&fx, &a_id);
    let new_trash: Vec<String> = trash(&fx).into_iter().filter(|t| !trash_before.contains(t)).collect();
    assert_eq!(new_trash.len(), 2, "root and .repos go to trash: {new_trash:?}");
    assert_eq!(untouched(&fx, &b_id).root, b_before.root);
    assert_eq!(untouched(&fx, &b_id).shadow, b_before.shadow);
    assert_eq!(lifecycle(&fx, &b_id), Lifecycle::Active);
    assert!(fx.pins("tool").contains(b_id.as_str()));
    assert_eq!(snapshot(&bases), bases_before);
    let main_after = fx.main_snap("tool");
    assert_eq!((main_after.tree, main_after.git, main_after.index), (main_before.tree, main_before.git, main_before.index));
    let strip = |r: &str| r.lines().filter(|l| !l.starts_with(&a_pin)).collect::<Vec<_>>().join("\n");
    assert_eq!(main_after.refs, strip(&main_before.refs));

    // Emptying trash never follows the symlink out.
    for t in new_trash {
        let r = strata_apfs::remove_tree_rooted(&fx.root(), &rp(&format!("store/trash/{t}"))).unwrap();
        assert!(r.is_complete());
    }
    assert_eq!(snapshot(&outside), outside_before);
    assert_eq!(fs::read(outside.join("keep.txt")).unwrap(), b"keep\n");
}

// ------------------------------------------------- rm-followups R2/R3

#[test]
fn rm_dirty_refusal_says_nothing_removed_and_active() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let id = id(&new_all(&fx, &s));
    let w = session_root(&fx, &id).join("tool");
    for i in 0..7 {
        fx.sb.write(&w, &format!("src/n{i}.ts"), b"new\n");
    }
    let e = s.remove(&id, false, None).unwrap_err();
    assert_eq!(e.code, Code::Dirty, "{e:?}");
    let m = &e.message;
    assert!(
        m.starts_with(&format!(
            "rm refused: nothing was removed; session {id} is still Active and listed (`git-strata ls`). \
             tool: 7 paths not published: src/n0.ts, src/n1.ts, src/n2.ts, src/n3.ts, src/n4.ts (+2 more)."
        )),
        "{m}"
    );
    assert!(m.ends_with(&format!(
        "Publish them, or run `git-strata rm --discard {id}` (Touch ID) to throw them away; the BeforeRemove \
         checkpoint then keeps them until gc."
    )));
    assert_eq!(detail(&e)["removed"], false);
    assert_eq!(detail(&e)["lifecycle"], "Active");
    assert_eq!(detail(&e)["repos"]["tool"]["paths"].as_array().unwrap().len(), 7, "detail keeps them all");
    assert_eq!(lifecycle(&fx, &id), Lifecycle::Active);
    assert!(s.status(&id).is_ok(), "still listed");
}

#[test]
fn rm_open_files_refusal_says_nothing_removed() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let id = id(&new_all(&fx, &s));
    let holder = Worker::spawn("cwd:".into(), Some(&session_root(&fx, &id).join("tool/src")));
    let e = s.remove(&id, false, None).unwrap_err();
    drop(holder);
    assert_eq!(e.code, Code::OpenFiles, "{e:?}");
    assert!(e.message.starts_with(&format!("rm refused: nothing was removed; session {id} is still Active")), "{e:?}");
    assert_eq!(detail(&e)["removed"], false);
    assert_eq!(detail(&e)["lifecycle"], "Active");
}

#[test]
fn rm_interrupted_hint_names_phase_and_remaining_dirs() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let id = id(&new_all(&fx, &s));
    s.arm(SessionSeam::FailAt { phase: "remove.retire".into(), errno: libc::EIO });
    assert_eq!(s.remove(&id, false, None).unwrap_err().code, Code::Unknown);
    let st = s.status(&id).unwrap();
    let i = st.rm.rm_interrupted.clone().expect("rm_interrupted");
    assert_eq!(i.phase, "retire");
    assert_eq!(i.remaining, vec![session_root(&fx, &id), repos_dir(&fx, &id)]);
    assert!(!i.root_absent);
    let want = format!(
        "rm interrupted at phase retire; remaining: sessions/{id}, sessions/.repos/{id}; needs `git-strata rm --discard {id}`"
    );
    assert!(st.rm.notes.contains(&want), "{:?}", st.rm.notes);

    // The root gone, .repos still there.
    fs::rename(session_root(&fx, &id), fx.sb.path("moved-away")).unwrap();
    let st = s.status(&id).unwrap();
    let want = format!(
        "rm interrupted at phase retire; remaining: sessions/.repos/{id} (root absent); needs `git-strata rm --discard {id}`"
    );
    assert!(st.rm.notes.contains(&want), "{:?}", st.rm.notes);

    let e = s.remove(&id, false, None).unwrap_err();
    assert_eq!(e.code, Code::Dirty, "{e:?}");
    assert!(
        e.message.starts_with(&format!("rm refused: nothing was removed; session {id} is still Unknown ({want}).")),
        "{e:?}"
    );
    assert!(e.message.ends_with(&format!("Run `git-strata rm --discard {id}` (Touch ID) to finish removing it.")), "{e:?}");
    assert_eq!(detail(&e)["lifecycle"], "Unknown");
}

#[test]
fn status_shows_last_rm_and_rebase_pending_from_the_store() {
    use strata_store::{RmAttempt, RmOutcome};
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let id = id(&new_all(&fx, &s));
    assert!(s.status(&id).unwrap().rm.is_empty());
    fx.sb.write(&session_root(&fx, &id).join("tool"), "src/n.ts", b"new\n");
    let e = s.remove(&id, false, None).unwrap_err();
    let a = RmAttempt {
        session: id.clone(),
        at: std::time::SystemTime::now(),
        outcome: RmOutcome::Refused,
        detail: serde_json::json!({ "code": "Dirty", "message": e.message, "detail": e.detail }),
    };
    assert!(fx.store().record_rm_attempt(&a).unwrap());
    let st = s.status(&id).unwrap();
    let last = st.rm.last_rm.clone().expect("last_rm");
    assert_eq!(last.summary, "Dirty (1 path in tool)");
    assert_eq!(last.code.as_deref(), Some("Dirty"));
    let want = format!("last rm {} refused: Dirty (1 path in tool); nothing removed", strata_session::local_hhmm(a.at));
    assert_eq!(st.rm.notes, vec![want]);
    let v = serde_json::to_value(&st).unwrap();
    assert_eq!(v["last_rm"]["outcome"], "refused", "{v}");
    let sum = strata_session::summary(
        fx.store(),
        &fx.root(),
        strata_session::SummarySelect::Session(id.as_str()),
        false,
    )
    .unwrap();
    assert_eq!(sum[0].rm, st.rm, "ls and --brief carry the same notes");

    // rebase_pending: a replay plan, then its adopted and deferred target
    let tool = strata_types::RepoName::try_from("tool".to_string()).unwrap();
    let (h, now) = ("c".repeat(40), std::time::SystemTime::now());
    let plan = strata_store::ReplayPlan { session: id.clone(), planned_head: h.clone(), planned_tree: h.clone() };
    fx.store().plan_replay(&tool, &[plan], now).unwrap();
    let st = s.status(&id).unwrap();
    assert_eq!(st.rm.notes.last().map(String::as_str), Some("waiting for the replay of tool"), "{:?}", st.rm.notes);
    assert!(fx.store().settle_planned(&id, &tool, &h, true, now).unwrap());
    let st = s.status(&id).unwrap();
    let since = strata_session::local_hhmm(now);
    assert_eq!(st.rm.notes.last().unwrap(), &format!("behind origin of tool: auto-rebase pending (pending, since {since})"));
    assert!(fx.store().defer_pending(&id, &tool, 1, "busy", &serde_json::json!({ "publish": 5 }), now).unwrap());
    let st = s.status(&id).unwrap();
    assert_eq!(st.rm.notes.last().unwrap(), &format!("behind publish 5 of tool: auto-rebase deferred (busy, since {since}, attempts 1)"));
    let note = st.rm.rebase_pending.last().unwrap();
    assert_eq!((note.reason.as_str(), note.publish, note.attempts), ("busy", Some(5), 1));
}

/// R4: a holder that appears after Phase B, just before Removing, is seen
/// by the batched recheck; the session stays Active.
#[test]
fn rm_refuses_open_files_found_just_before_removing() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let id = id(&new_all(&fx, &s));
    s.arm(SessionSeam::PauseAt { phase: "remove.before_removing".into() });
    let mut holder = None;
    let e = std::thread::scope(|sc| {
        let h = sc.spawn(|| s.remove(&id, false, None));
        s.wait_paused();
        holder = Some(Worker::spawn("cwd:".into(), Some(&shadow(&fx, &id, "tool"))));
        s.release();
        h.join().unwrap()
    })
    .unwrap_err();
    drop(holder);
    assert_eq!(e.code, Code::OpenFiles, "{e:?}");
    assert!(e.message.starts_with(&format!("rm refused: nothing was removed; session {id} is still Active")), "{e:?}");
    assert_eq!(lifecycle(&fx, &id), Lifecycle::Active);
    assert!(session_root(&fx, &id).is_dir() && shadow(&fx, &id, "tool").is_dir());
    s.remove(&id, false, None).unwrap();
    assert_removed(&fx, &id);
}

/// R4: teardown retires before it unpins; a failure between the two is an
/// Unknown scene at `unpin` that `rm --discard` finishes.
#[test]
fn teardown_retires_before_unpin_and_discard_finishes() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let id = id(&new_all(&fx, &s));
    s.arm(SessionSeam::FailAt { phase: "remove.unpin".into(), errno: libc::EIO });
    let e = s.remove(&id, false, None).unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    assert_eq!(detail_str(&e, "phase"), Some("unpin"), "{e:?}");
    assert!(!session_root(&fx, &id).exists() && !repos_dir(&fx, &id).exists(), "retired first");
    assert_ne!(fx.pins("tool"), "", "the pin is still there");
    assert_eq!(lifecycle(&fx, &id), Lifecycle::Unknown);
    s.remove(&id, true, tok(&id)).unwrap();
    assert_eq!(fx.pins("tool"), "");
    assert_removed(&fx, &id);
}

/// R4 crash coverage: a crash between retire and unpin, then a restart:
/// recover makes it Unknown (phase unpin) and `rm --discard` finishes.
#[test]
fn teardown_crash_after_retire_recovers_and_discard_finishes() {
    let mut fx = Fx::with(&["tool"]);
    let id = {
        let s = fx.sessions();
        let id = id(&new_all(&fx, &s));
        s.arm(SessionSeam::StopAt { phase: "remove.unpin".into() });
        s.remove(&id, false, None).unwrap_err();
        id
    };
    assert_eq!(lifecycle(&fx, &id), Lifecycle::Removing);
    assert!(!session_root(&fx, &id).exists() && !repos_dir(&fx, &id).exists(), "retired first");
    assert_ne!(fx.pins("tool"), "", "the pin is still there");
    fx.close_store();
    fx.reopen_store();
    let s = fx.sessions();
    let rep = s.recover().unwrap();
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    assert_eq!(lifecycle(&fx, &id), Lifecycle::Unknown);
    let e = s.remove(&id, false, None).unwrap_err();
    assert!(e.message.contains("interrupted at phase unpin"), "{e:?}");
    s.remove(&id, true, tok(&id)).unwrap();
    assert_eq!(fx.pins("tool"), "");
    assert_removed(&fx, &id);
}

// ---------------------------------------------------------------- R1 orphan sweep

/// A no-row orphan `sessions/<sid>` with a file in it.
fn orphan(fx: &Fx, name: &str) -> PathBuf {
    let d = fx.root().join("sessions").join(name);
    fs::create_dir_all(d.join("a")).unwrap();
    fs::write(d.join("a/f"), b"x").unwrap();
    d
}

fn removed_with_recreated_root(fx: &Fx, s: &Sessions<'_>) -> SessionId {
    let id = id(&new_all(fx, s));
    s.remove(&id, false, None).unwrap();
    fs::create_dir_all(session_root(fx, &id).join("a/b")).unwrap();
    id
}

#[test]
fn sweep_retires_dir_of_removed_row() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let id = removed_with_recreated_root(&fx, &s);
    let before = trash(&fx).len();
    let sw = s.sweep_orphans(None);
    assert!(sw.warnings.is_empty(), "{:?}", sw.warnings);
    assert_eq!(sw.retired.len(), 1, "{sw:?}");
    assert_eq!(sw.retired[0].session, id);
    assert_eq!(sw.retired[0].path, session_root(&fx, &id));
    assert!(sw.retired[0].trash.join("a/b").is_dir(), "moved to trash, not deleted");
    assert!(!session_root(&fx, &id).exists());
    assert_eq!(trash(&fx).len(), before + 1);
}

#[test]
fn sweep_retires_dir_without_row() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let d = orphan(&fx, "s-k3q7xm2a");
    let r = fx.root().join("sessions/.repos/s-k3q7xm2a");
    fs::create_dir_all(&r).unwrap();
    let sw = s.sweep_orphans(None);
    assert!(sw.warnings.is_empty(), "{:?}", sw.warnings);
    let mut got: Vec<PathBuf> = sw.retired.iter().map(|o| o.path.clone()).collect();
    got.sort();
    let mut want = vec![d.clone(), r.clone()];
    want.sort();
    assert_eq!(got, want);
    assert!(!d.exists() && !r.exists());
}

#[test]
fn sweep_skips_open_files() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let d = orphan(&fx, "s-k3q7xm2a");
    let w = Worker::spawn(format!("fd:{}", d.join("a/f").display()), None);
    let sw = s.sweep_orphans(None);
    assert!(sw.retired.is_empty(), "{sw:?}");
    assert_eq!(sw.warnings.len(), 1, "{sw:?}");
    assert_eq!(sw.warnings[0].code, Code::OpenFiles);
    assert_eq!(reason(&sw.warnings[0]), Some("orphan_open_files"));
    assert!(d.join("a/f").exists());
    drop(w);
    let sw = s.sweep_orphans(None);
    assert_eq!(sw.retired.len(), 1, "{sw:?}");
    assert!(!d.exists());
}

#[test]
fn sweep_ignores_non_session_names_tmp_symlinks() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let sessions = fx.root().join("sessions");
    fs::create_dir_all(sessions.join(".repos")).unwrap();
    let keep = [sessions.join("notes"), sessions.join(".strata-tmp-1234"), sessions.join(".repos/not-an-id")];
    for k in &keep {
        fs::create_dir_all(k).unwrap();
    }
    let target = fx.root().join("elsewhere");
    fs::create_dir_all(target.join("x")).unwrap();
    symlink(&target, sessions.join("s-k3q7xm2a")).unwrap();
    fs::write(sessions.join("s-m4n5p6q7"), b"a file").unwrap();
    let sw = s.sweep_orphans(None);
    assert!(sw.retired.is_empty() && sw.warnings.is_empty(), "{sw:?}");
    for k in &keep {
        assert!(k.is_dir(), "{}", k.display());
    }
    assert!(fs::symlink_metadata(sessions.join("s-k3q7xm2a")).unwrap().file_type().is_symlink());
    assert!(target.join("x").is_dir());
    assert!(sessions.join("s-m4n5p6q7").is_file());
}

#[test]
fn sweep_never_touches_creating_active_removing_unknown() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    // Active
    let active = id(&new_all(&fx, &s));
    // Creating: a crash in new.
    s.arm(SessionSeam::StopAt { phase: "new.seed_files".into() });
    s.set_next_id(sid("s-c2c2c2c2"));
    s.new(None, &fx.all()).unwrap_err();
    let creating = sid("s-c2c2c2c2");
    // Removing: a crash in teardown.
    let removing = id(&new_all(&fx, &s));
    s.arm(SessionSeam::StopAt { phase: "remove.unpin".into() });
    s.remove(&removing, false, None).unwrap_err();
    fs::create_dir_all(session_root(&fx, &removing).join("again")).unwrap();
    // Unknown: a failure in teardown, root recreated.
    let unknown = id(&new_all(&fx, &s));
    s.arm(SessionSeam::FailAt { phase: "remove.unpin".into(), errno: libc::EIO });
    s.remove(&unknown, false, None).unwrap_err();
    fs::create_dir_all(session_root(&fx, &unknown).join("again")).unwrap();
    for (i, lc) in [(&active, Lifecycle::Active), (&creating, Lifecycle::Creating), (&removing, Lifecycle::Removing), (&unknown, Lifecycle::Unknown)] {
        assert_eq!(lifecycle(&fx, i), lc);
    }
    let sw = s.sweep_orphans(None);
    assert!(sw.retired.is_empty() && sw.warnings.is_empty(), "{sw:?}");
    for i in [&active, &creating, &removing, &unknown] {
        let sw = s.sweep_orphans(Some(i));
        assert!(sw.retired.is_empty() && sw.warnings.is_empty(), "{i}: {sw:?}");
        assert!(session_root(&fx, i).is_dir(), "{i}");
    }
    assert!(repos_dir(&fx, &active).is_dir() && repos_dir(&fx, &creating).is_dir());
}

/// A sweep of the id `new` is enrolling waits for `enroll`, then sees the
/// row and skips.
#[test]
fn sweep_races_new() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let id = sid("s-k3q7xm2a");
    s.set_next_id(id.clone());
    s.arm(SessionSeam::PauseAt { phase: "new.enrolled".into() });
    let (rep, sw, waited) = std::thread::scope(|sc| {
        let n = sc.spawn(|| s.new(None, &fx.all()));
        s.wait_paused();
        assert!(session_root(&fx, &id).is_dir() && repos_dir(&fx, &id).is_dir());
        assert!(s.sweep_orphans(None).retired.is_empty(), "a Creating row is no orphan");
        let w = sc.spawn(|| s.sweep_orphans(Some(&id)));
        std::thread::sleep(Duration::from_millis(300));
        let waited = !w.is_finished();
        s.release();
        (n.join().unwrap(), w.join().unwrap(), waited)
    });
    assert!(waited, "the sweep waited for enroll");
    assert!(sw.retired.is_empty() && sw.warnings.is_empty(), "{sw:?}");
    rep.unwrap();
    assert_eq!(lifecycle(&fx, &id), Lifecycle::Active);
    assert!(session_root(&fx, &id).is_dir() && repos_dir(&fx, &id).is_dir());
}

#[test]
fn sweep_inode_changed_skips() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let d = orphan(&fx, "s-k3q7xm2a");
    s.arm(SessionSeam::PauseAt { phase: "orphan.enroll".into() });
    let sw = std::thread::scope(|sc| {
        let h = sc.spawn(|| s.sweep_orphans(None));
        s.wait_paused();
        fs::rename(&d, fx.root().join("moved-away")).unwrap();
        fs::create_dir_all(d.join("new")).unwrap();
        s.release();
        h.join().unwrap()
    });
    assert!(sw.retired.is_empty() && sw.warnings.is_empty(), "{sw:?}");
    assert!(d.join("new").is_dir(), "the replacement stays");
    assert_eq!(s.sweep_orphans(None).retired.len(), 1, "the next sweep takes it");
}

/// A leftover `.repos/<id>` that appears before enrollment makes `new`
/// fail with Exists under `enroll`, before any row.
#[test]
fn new_fails_on_leftover_repos_dir() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let id = sid("s-k3q7xm2a");
    s.set_next_id(id.clone());
    s.arm(SessionSeam::PauseAt { phase: "new.enroll".into() });
    let e = std::thread::scope(|sc| {
        let h = sc.spawn(|| s.new(None, &fx.all()));
        s.wait_paused();
        fs::create_dir_all(repos_dir(&fx, &id).join("keep")).unwrap();
        s.release();
        h.join().unwrap()
    })
    .unwrap_err();
    assert_eq!(e.code, Code::Exists, "{e:?}");
    assert_eq!(reason(&e), Some("directory"));
    assert_eq!(fx.session_rows(), 0);
    assert!(repos_dir(&fx, &id).join("keep").is_dir());
    assert!(!session_root(&fx, &id).exists());
}

/// new's rollback retires only what it created; a `.repos/<id>` swapped in
/// is named in `not_owned` and left for the sweep.
#[test]
fn new_rollback_leaves_dirs_it_did_not_create() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let id = sid("s-k3q7xm2a");
    s.set_next_id(id.clone());
    s.arm(SessionSeam::PauseAt { phase: "new.pin".into() });
    let e = std::thread::scope(|sc| {
        let h = sc.spawn(|| s.new(None, &fx.all()));
        s.wait_paused();
        fs::rename(repos_dir(&fx, &id), fx.root().join("moved-away")).unwrap();
        fs::create_dir_all(repos_dir(&fx, &id).join("theirs")).unwrap();
        s.arm(SessionSeam::FailAt { phase: "new.seed_files".into(), errno: libc::EIO });
        s.release();
        h.join().unwrap()
    })
    .unwrap_err();
    let d = e.detail.as_ref().unwrap();
    assert_eq!(d["rolled_back"], Value::Bool(true), "{e:?}");
    assert_eq!(d["not_owned"], serde_json::json!([repos_dir(&fx, &id)]), "{e:?}");
    assert!(e.message.contains("left in place"), "{e:?}");
    assert_eq!(lifecycle(&fx, &id), Lifecycle::Removed);
    assert!(!session_root(&fx, &id).exists(), "its own root is retired");
    assert!(repos_dir(&fx, &id).join("theirs").is_dir(), "not ours: left");
    let sw = s.sweep_orphans(None);
    assert_eq!(sw.retired.iter().map(|o| o.path.clone()).collect::<Vec<_>>(), vec![repos_dir(&fx, &id)]);
}

#[test]
fn rm_end_sweep_retires_recreated_root() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let id = id(&new_all(&fx, &s));
    s.arm(SessionSeam::PauseAt { phase: "remove.unpin".into() });
    let rep = std::thread::scope(|sc| {
        let h = sc.spawn(|| s.remove(&id, false, None));
        s.wait_paused();
        assert!(!session_root(&fx, &id).exists(), "retired");
        fs::create_dir_all(session_root(&fx, &id).join("a/b")).unwrap();
        s.release();
        h.join().unwrap()
    })
    .unwrap();
    assert!(rep.warnings.is_empty(), "{:?}", rep.warnings);
    assert_eq!(rep.orphans.len(), 1, "{rep:?}");
    assert_eq!(rep.orphans[0].path, session_root(&fx, &id));
    assert!(rep.orphans[0].trash.join("a/b").is_dir());
    assert_removed(&fx, &id);
}

#[test]
fn rm_end_sweep_failure_is_warning() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let id = id(&new_all(&fx, &s));
    s.arm(SessionSeam::PauseAt { phase: "remove.unpin".into() });
    let rep = std::thread::scope(|sc| {
        let h = sc.spawn(|| s.remove(&id, false, None));
        s.wait_paused();
        fs::create_dir_all(session_root(&fx, &id).join("a/b")).unwrap();
        s.arm(SessionSeam::FailAt { phase: "orphan.retire".into(), errno: libc::EIO });
        s.release();
        h.join().unwrap()
    })
    .unwrap();
    assert!(rep.orphans.is_empty(), "{rep:?}");
    assert_eq!(rep.warnings.len(), 1, "{rep:?}");
    assert_eq!(lifecycle(&fx, &id), Lifecycle::Removed, "the committed Removed stands");
    assert!(session_root(&fx, &id).join("a/b").is_dir());
    let v = serde_json::to_value(&rep).unwrap();
    assert!(v["warnings"].is_array() && v.get("orphans").is_none(), "{v}");
    assert_eq!(s.sweep_orphans(None).retired.len(), 1);
}

/// `gc --dry-run`'s list (`Sessions::orphans`) writes nothing.
#[test]
fn gc_dry_run_reports_orphans_without_writes() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let id = removed_with_recreated_root(&fx, &s);
    let d = orphan(&fx, "s-k3q7xm2a");
    let before = trash(&fx);
    let mut got = s.orphans().unwrap();
    got.sort();
    let mut want = vec![(id.clone(), session_root(&fx, &id)), (sid("s-k3q7xm2a"), d.clone())];
    want.sort();
    assert_eq!(got, want);
    assert_eq!(trash(&fx), before);
    assert!(d.join("a/f").exists() && session_root(&fx, &id).join("a/b").is_dir());
}

/// recover step 4 runs after step 1: a Creating row became Unknown and is
/// kept; a no-row orphan is retired.
#[test]
fn recover_sweeps_after_unknown_rewrite() {
    let mut fx = Fx::with(&["tool"]);
    let creating = sid("s-c2c2c2c2");
    {
        let s = fx.sessions();
        s.arm(SessionSeam::StopAt { phase: "new.seed_files".into() });
        s.set_next_id(creating.clone());
        s.new(None, &fx.all()).unwrap_err();
    }
    let d = orphan(&fx, "s-k3q7xm2a");
    fx.close_store();
    fx.reopen_store();
    let s = fx.sessions();
    let rep = s.recover().unwrap();
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    assert_eq!(rep.unknown, vec![creating.clone()]);
    assert_eq!(rep.orphans_retired, vec![d.clone()]);
    assert!(!d.exists());
    assert_eq!(lifecycle(&fx, &creating), Lifecycle::Unknown);
    assert!(session_root(&fx, &creating).is_dir() && repos_dir(&fx, &creating).is_dir());
}

/// impl-p3 review: a `.repos/<id>` created by someone else between the
/// parent mkdir and the leaf mkdir fails `new` (exclusive leaf) and is
/// never retired by its rollback: it is named in `not_owned`.
#[test]
fn new_rollback_never_claims_a_repos_dir_made_before_its_mkdir() {
    let fx = Fx::with(&["tool"]);
    let s = fx.sessions();
    let id = sid("s-k3q7xm2a");
    s.set_next_id(id.clone());
    s.arm(SessionSeam::PauseAt { phase: "new.repos_dir".into() });
    let e = std::thread::scope(|sc| {
        let h = sc.spawn(|| s.new(None, &fx.all()));
        s.wait_paused();
        fs::create_dir_all(repos_dir(&fx, &id).join("theirs")).unwrap();
        s.release();
        h.join().unwrap()
    })
    .unwrap_err();
    let d = e.detail.as_ref().unwrap();
    assert_eq!(d["rolled_back"], Value::Bool(true), "{e:?}");
    assert_eq!(d["not_owned"], serde_json::json!([repos_dir(&fx, &id)]), "{e:?}");
    assert_eq!(lifecycle(&fx, &id), Lifecycle::Removed);
    assert!(!session_root(&fx, &id).exists(), "its own root is retired");
    assert!(repos_dir(&fx, &id).join("theirs").is_dir(), "not ours: left");
}
