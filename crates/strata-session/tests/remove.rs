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
    Some(ConfirmToken { action: Action::RemoveDiscard { session: id.clone() }, issued: Instant::now(), nonce: 1 })
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
    let expected = serde_json::to_value(Action::RemoveDiscard { session: a_id.clone() }).unwrap();
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
    assert_eq!(fx.pins("tool"), "", "the pin went in step 7");
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
