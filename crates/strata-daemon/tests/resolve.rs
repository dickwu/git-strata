//! §8 Task 7: `resolve_move` and `chain_resolve` (daemon.md §5.5, D18, D19).
//! Every remote is a local bare repo in the sandbox.

mod support;

use std::cell::RefCell;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use strata_confirm::seams::Verdict;
use strata_daemon::ChainChoice;
use strata_daemon::handlers;
use strata_daemon::resolve::{self, ChainResolved};
use strata_daemon::startup::{self, Core, Daemon};
use strata_session::{MoveResolution, ResolutionReport};
use strata_types::{Code, CommitOid, SessionId, StrataError};
use support::*;

fn serving<R>(d: &Daemon<'_>, f: impl FnOnce() -> R) -> R {
    let listener = startup::bind(&d.core.opts.socket).unwrap();
    let ext = AtomicBool::new(false);
    std::thread::scope(|s| {
        let h = s.spawn(|| handlers::serve(d, &listener, &ext));
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        d.request_stop();
        h.join().unwrap().unwrap();
        r.unwrap_or_else(|p| std::panic::resume_unwind(p))
    })
}

fn call(socket: &Path, method: &str, params: Value) -> Value {
    let mut s = UnixStream::connect(socket).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(120))).unwrap();
    let line = json!({ "id": 3, "method": method, "params": params }).to_string();
    s.write_all(line.as_bytes()).unwrap();
    s.write_all(b"\n").unwrap();
    let mut out = String::new();
    BufReader::new(s).read_line(&mut out).unwrap();
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("reply {out:?}: {e}"))
}

fn ok(v: Value) -> Value {
    assert_eq!(v["ok"], json!(true), "{v:#}");
    v["data"].clone()
}

fn new_session(socket: &Path) -> SessionId {
    let d = ok(call(socket, "new", json!({ "ticket": null, "repos": ["tool"] })));
    serde_json::from_value(d["session"]["manifest"]["id"].clone()).unwrap()
}

fn src(name: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(name)).unwrap()
}

/// The text of `fn name` in `file` (up to the next top-level item).
fn fn_body(file: &str, name: &str) -> String {
    let s = src(file);
    let start = s.find(&format!("pub fn {name}")).unwrap_or_else(|| panic!("no fn {name}"));
    let rest = &s[start..];
    let end = rest[1..].find("\npub fn ").or_else(|| rest[1..].find("\n/// ")).map(|i| i + 1).unwrap_or(rest.len());
    rest[..end].to_string()
}

// ---- the chain fixture --------------------------------------------------

const CHAIN: &str = "refs/strata/publish/main";
const ANCHOR: &str = "refs/strata/anchor/main";

/// A, P, C, O of the chain pictures: P → A on origin, C on A on the chain.
struct Chain {
    main: PathBuf,
    bare: PathBuf,
    other: PathBuf,
    p: String,
    a: String,
    c: String,
}

impl Chain {
    /// origin main at A (parent P); chain C on A, anchor A.
    fn new(fx: &Dfx) -> Chain {
        let sb = &fx.sb;
        let main = fx.main("tool");
        let bare = sb.root.join("tool-origin.git");
        let p = sb.head(&main);
        let a = sb.commit(&main, &[("a.txt", b"a\n")], "A");
        sb.git(&main, &["push", "-q", "origin", "main"]);
        let c = sb.commit(&main, &[("c.txt", b"c\n")], "C");
        sb.git(&main, &["reset", "-q", "--hard", &a]);
        sb.git(&main, &["update-ref", CHAIN, &c]);
        sb.git(&main, &["update-ref", ANCHOR, &a]);
        let other = sb.root.join("other");
        sb.git(&sb.root, &["clone", "-q", bare.to_str().unwrap(), other.to_str().unwrap()]);
        Chain { main, bare, other, p, a, c }
    }

    /// origin gains O on A (a plain push from another clone): Diverged.
    fn advance_origin(&self, fx: &Dfx, files: &[(&str, &[u8])]) -> String {
        let o = fx.sb.commit(&self.other, files, "O");
        fx.sb.git(&self.other, &["push", "-q", "origin", "HEAD:main"]);
        o
    }

    /// origin's main set to `to` in the bare repo itself: Rewound.
    fn rewind_origin(&self, fx: &Dfx, to: &str) {
        fx.sb.git(&self.bare, &["update-ref", "refs/heads/main", to]);
    }

    /// origin gets Q on P on a side branch, then main is set to it.
    fn rewind_to_unrelated(&self, fx: &Dfx) -> String {
        fx.sb.git(&self.other, &["checkout", "-q", "-b", "q", &self.p]);
        let q = fx.sb.commit(&self.other, &[("q.txt", b"q\n")], "Q");
        fx.sb.git(&self.other, &["push", "-q", "origin", "q"]);
        self.rewind_origin(fx, &q);
        q
    }

    fn rev(&self, fx: &Dfx, r: &str) -> String {
        let out = fx.sb.git_output(&self.main, &["rev-parse", "--verify", "-q", r]);
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn refs(&self, fx: &Dfx) -> (String, String) {
        (self.rev(fx, CHAIN), self.rev(fx, ANCHOR))
    }

    fn origin_main(&self, fx: &Dfx) -> String {
        fx.sb.git(&self.bare, &["rev-parse", "refs/heads/main"]).trim().to_string()
    }
}

/// Fetch so the read before the lock sees the fork, then forget the calls.
fn sync(core: &Core) {
    let g = &core.gits[&repo_name("tool")];
    g.fetch("origin", "main").unwrap();
    g.take_git_calls();
}

#[derive(Default, Debug)]
struct Calls {
    fetch: usize,
    push: usize,
    transactions: usize,
    replay: usize,
    push_args: Vec<String>,
}

fn calls(core: &Core) -> Calls {
    let mut out = Calls::default();
    for c in core.gits[&repo_name("tool")].take_git_calls() {
        let args: Vec<String> = c.args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        let has = |w: &str| args.iter().any(|a| a == w);
        if has("fetch") {
            out.fetch += 1;
        }
        if has("push") {
            out.push += 1;
            out.push_args = args.clone();
        }
        if has("update-ref") && has("--stdin") {
            out.transactions += 1;
        }
        if has("merge-tree") {
            out.replay += 1;
        }
    }
    out
}

/// `chain_resolve` as the cli sends it: `shown` is the chain top it read.
fn resolve(core: &Core, choice: ChainChoice) -> (strata_daemon::Result<ChainResolved>, usize) {
    let shown = core.gits[&repo_name("tool")].read_ref(CHAIN).unwrap();
    resolve_shown(core, choice, shown.as_ref())
}

fn resolve_shown(core: &Core, choice: ChainChoice, shown: Option<&CommitOid>) -> (strata_daemon::Result<ChainResolved>, usize) {
    let mut observed = 0;
    let r = resolve::chain_resolve(core, &repo_name("tool"), choice, shown, &mut |_| {
        observed += 1;
        Ok(())
    });
    (r, observed)
}

fn code_reason(r: &strata_daemon::Result<ChainResolved>) -> (Code, String) {
    let e: &StrataError = r.as_ref().expect_err("must fail");
    (e.code, reason(e).unwrap_or("").to_string())
}

fn set_confirm(fx: &mut Dfx) {
    let rc = fx.config.repos.get_mut(&repo_name("tool")).unwrap();
    rc.publish.confirm = true;
    fx.write_config();
}

// ---- resolve_move -------------------------------------------------------

fn report_for(core: &Core, s: &SessionId, choice: MoveResolution) -> ResolutionReport {
    let rec = core.store.session(s).unwrap().unwrap();
    let w = &rec.manifest.repos[&repo_name("tool")];
    ResolutionReport {
        session: s.clone(),
        repo: repo_name("tool"),
        choice,
        n: 1,
        base: w.base.clone(),
        head: w.head.clone(),
        old_tree: None,
        leftover: vec![],
    }
}

#[test]
fn resolve_move_exclusive_choice() {
    assert_eq!(resolve::move_choice(true, false), Some(MoveResolution::Abandon));
    assert_eq!(resolve::move_choice(false, true), Some(MoveResolution::Accept));
    assert_eq!(resolve::move_choice(true, true), None);
    assert_eq!(resolve::move_choice(false, false), None);

    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    serving(&d, || {
        let id = new_session(&sock);
        let before = serde_json::to_value(core.store.session(&id).unwrap().unwrap().manifest).unwrap();
        for (ab, ac) in [(true, true), (false, false)] {
            let r = call(&sock, "resolve_move", json!({ "session": id, "repo": "tool", "abandon": ab, "accept": ac }));
            assert_eq!(r["error"]["rpc"], json!("Failed"), "{ab} {ac}: {r:#}");
        }
        // resolve_failed_move was not called: the session is untouched.
        let after = serde_json::to_value(core.store.session(&id).unwrap().unwrap().manifest).unwrap();
        assert_eq!(before, after);
        // One choice reaches session (no failed move here → its refusal).
        let r = call(&sock, "resolve_move", json!({ "session": id, "repo": "tool", "abandon": true, "accept": false }));
        assert_eq!(r["ok"], json!(false), "{r:#}");
        assert!(r["error"]["rpc"].is_null(), "a domain error, not an RPC rejection: {r:#}");
    });
}

#[test]
fn resolve_move_skips_tty_and_confirm() {
    let mut fx = Dfx::new();
    set_confirm(&mut fx);
    // No scripted verdict: any prompt would fail the call.
    let (c, log, _) = fx.confirmer(vec![]);
    let core = fx.prepare_with(c).unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    serving(&d, || {
        let id = new_session(&sock);
        for (ab, ac) in [(true, false), (false, true)] {
            let r = call(&sock, "resolve_move", json!({ "session": id, "repo": "tool", "abandon": ab, "accept": ac }));
            let code = r["error"]["code"].as_str().unwrap_or("");
            assert!(code != "NotTty" && code != "NotConfirmed", "{r:#}");
        }
        // No credential in the parameters.
        let r = call(&sock, "resolve_move", json!({ "session": id, "repo": "tool", "abandon": true, "accept": false, "token": "x" }));
        assert_eq!(r["error"]["rpc"], json!("Failed"), "{r:#}");
    });
    assert!(log.shown().is_empty(), "{:?}", log.shown());
    assert_eq!(log.probes(), 0);
    let body = fn_body("resolve.rs", "resolve_move");
    for w in ["confirmer", "require_validated", "NotTty", "isatty", "is_terminal"] {
        assert!(!body.contains(w), "resolve_move mentions {w}");
    }
}

#[test]
fn resolve_move_queues_and_does_not_hold_repo_lock() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    serving(&d, || {
        let id = new_session(&sock);
        let repo = repo_name("tool");
        for choice in [MoveResolution::Abandon, MoveResolution::Accept] {
            let order = RefCell::new(Vec::new());
            let r = resolve::resolve_move(
                &core,
                &id,
                &repo,
                choice,
                &|s, r, c| {
                    assert!(core.locks.holds().is_empty(), "no repo lock around resolve_failed_move");
                    assert!(core.locks.in_flight(s).is_some(), "the session queue is held");
                    assert_eq!((s, r, c), (&id, &repo, choice));
                    order.borrow_mut().push("resolve");
                    Ok(report_for(&core, s, c))
                },
                &mut || {
                    assert!(core.locks.in_flight(&id).is_some(), "reset under the queue");
                    order.borrow_mut().push("reset+watch");
                },
            )
            .unwrap();
            assert_eq!(r.choice, choice);
            let want: Vec<&str> =
                if choice == MoveResolution::Accept { vec!["resolve", "reset+watch"] } else { vec!["resolve"] };
            assert_eq!(*order.borrow(), want, "{choice:?}");
        }

        // Queued behind another holder of the same session, not refused.
        let released = AtomicBool::new(false);
        std::thread::scope(|s| {
            let (tx, rx) = std::sync::mpsc::channel();
            let (core, id, released) = (&core, &id, &released);
            s.spawn(move || {
                core.locks
                    .with_sessions(std::slice::from_ref(id), || {
                        tx.send(()).unwrap();
                        std::thread::sleep(Duration::from_millis(150));
                        released.store(true, Ordering::SeqCst);
                        Ok(())
                    })
                    .unwrap();
            });
            rx.recv().unwrap();
            let t = Instant::now();
            resolve::resolve_move(
                core,
                id,
                &repo,
                MoveResolution::Abandon,
                &|s, _, c| {
                    assert!(released.load(Ordering::SeqCst), "ran while the queue was held");
                    Ok(report_for(core, s, c))
                },
                &mut || {},
            )
            .unwrap();
            assert!(t.elapsed() >= Duration::from_millis(100));
        });
    });
}

// ---- chain_resolve ------------------------------------------------------

#[test]
fn chain_resolve_accept_origin_does_not_push() {
    // Drop: chain and anchor both to o, one transaction, no push.
    {
        let fx = Dfx::new();
        let ch = Chain::new(&fx);
        let o = ch.advance_origin(&fx, &[("o.txt", b"o\n")]);
        let core = fx.prepare().unwrap();
        sync(&core);
        let (r, _) = resolve(&core, ChainChoice::AcceptOriginDrop);
        let r = r.unwrap();
        assert_eq!((r.chain.as_str(), r.anchor.as_str(), r.pushed), (o.as_str(), o.as_str(), false));
        assert_eq!(ch.refs(&fx), (o.clone(), o.clone()));
        let k = calls(&core);
        assert_eq!((k.push, k.transactions, k.replay, k.fetch), (0, 1, 0, 1), "{k:?}");
        assert_eq!(ch.origin_main(&fx), o, "origin untouched");
    }
    // Replay: the range is the advanced a..c, not the old anchor's.
    {
        let fx = Dfx::new();
        let ch = Chain::new(&fx);
        // The anchor lags at P; merge_base(o, c) = A is past it.
        fx.sb.git(&ch.main, &["update-ref", ANCHOR, &ch.p]);
        let o = ch.advance_origin(&fx, &[("o.txt", b"o\n")]);
        let core = fx.prepare().unwrap();
        sync(&core);
        let (r, _) = resolve(&core, ChainChoice::AcceptOriginReplay);
        let r = r.unwrap();
        assert_eq!(r.anchor, o);
        assert!(!r.pushed);
        let (chain, anchor) = ch.refs(&fx);
        assert_eq!((chain.as_str(), anchor.as_str()), (r.chain.as_str(), o.as_str()));
        let n = fx.sb.git(&ch.main, &["rev-list", "--count", &format!("{o}..{chain}")]);
        assert_eq!(n.trim(), "1", "only C is replayed (A is already on origin)");
        assert_eq!(fx.sb.git(&ch.main, &["rev-parse", &format!("{chain}^")]).trim(), o);
        let files = fx.sb.git(&ch.main, &["ls-tree", "--name-only", &chain]);
        for f in ["a.txt", "c.txt", "o.txt"] {
            assert!(files.lines().any(|l| l == f), "{f} in {files}");
        }
        let k = calls(&core);
        assert_eq!((k.push, k.transactions, k.replay), (0, 1, 1), "{k:?}");
        assert_eq!(ch.origin_main(&fx), o);
    }
    // Replay conflict: before any transaction, refs byte-identical.
    {
        let fx = Dfx::new();
        let ch = Chain::new(&fx);
        let c2 = fx.sb.commit(&ch.main, &[("a.txt", b"chain\n")], "C2");
        fx.sb.git(&ch.main, &["reset", "-q", "--hard", &ch.a]);
        fx.sb.git(&ch.main, &["update-ref", CHAIN, &c2]);
        ch.advance_origin(&fx, &[("a.txt", b"origin\n")]);
        let core = fx.prepare().unwrap();
        sync(&core);
        let before = ch.refs(&fx);
        let (r, _) = resolve(&core, ChainChoice::AcceptOriginReplay);
        assert_eq!(code_reason(&r).0, Code::Conflict, "{r:?}");
        assert_eq!(ch.refs(&fx), before);
        let k = calls(&core);
        assert_eq!((k.push, k.transactions), (0, 0), "{k:?}");
    }
    // Rewound without an anchor: no replay range, nothing written.
    {
        let fx = Dfx::new();
        let ch = Chain::new(&fx);
        fx.sb.git(&ch.main, &["update-ref", "-d", ANCHOR]);
        let core = fx.prepare().unwrap();
        sync(&core);
        let (r, _) = resolve(&core, ChainChoice::AcceptOriginReplay);
        assert_eq!(code_reason(&r), (Code::InvalidInput, "no_replay_range".into()));
        assert_eq!(ch.rev(&fx, ANCHOR), "");
        assert_eq!(ch.rev(&fx, CHAIN), ch.c);
        assert_eq!(calls(&core).transactions, 0);
    }
}

#[test]
fn chain_resolve_restore_pushes_only_fast_forward() {
    // Rewound to P: C is a fast-forward of origin.
    {
        let fx = Dfx::new();
        let ch = Chain::new(&fx);
        ch.rewind_origin(&fx, &ch.p);
        let core = fx.prepare().unwrap();
        sync(&core);
        let (r, _) = resolve(&core, ChainChoice::Restore);
        let r = r.unwrap();
        assert!(r.pushed);
        assert_eq!(ch.origin_main(&fx), ch.c);
        assert_eq!(ch.refs(&fx), (ch.c.clone(), ch.c.clone()));
        let k = calls(&core);
        assert_eq!((k.push, k.transactions), (1, 1), "{k:?}");
        for a in &k.push_args {
            assert!(!a.starts_with("--force") && !a.starts_with('+'), "{:?}", k.push_args);
        }
    }
    // Rewound to an unrelated Q, and Diverged: not a fast-forward.
    for diverge in [false, true] {
        let fx = Dfx::new();
        let ch = Chain::new(&fx);
        let o = if diverge { ch.advance_origin(&fx, &[("o.txt", b"o\n")]) } else { ch.rewind_to_unrelated(&fx) };
        let core = fx.prepare().unwrap();
        sync(&core);
        let before = ch.refs(&fx);
        let (r, _) = resolve(&core, ChainChoice::Restore);
        assert_eq!(code_reason(&r), (Code::InvalidInput, "not_fast_forward".into()));
        assert_eq!(ch.refs(&fx), before);
        assert_eq!(ch.origin_main(&fx), o);
        let k = calls(&core);
        assert_eq!((k.push, k.transactions), (0, 0), "{k:?}");
    }
}

/// Review-2 N4: a definite Restore push settles the kept push scenes of
/// the same repo, remote and branch whose commit it contains; a scene of
/// another branch and a record without remote and branch (written before
/// N4) stay.
///
/// Review-3 R4: origin's push URL points at another bare remote B here, so
/// the restore pushes to B. Only the scene recorded with this repo's
/// identity and B's URL settles; one recorded for origin's fetch URL (the
/// destination before the URL changed), for another repository identity,
/// or without `repo_id` / `push_url` (written before R4) stays.
#[test]
fn chain_resolve_restore_settles_only_matching_push_scenes() {
    let fx = Dfx::new();
    let ch = Chain::new(&fx);
    ch.rewind_origin(&fx, &ch.p);
    let tool = fx.config.repos[&repo_name("tool")].path.clone();
    let a_url = fx.sb.git(&tool, &["config", "remote.origin.url"]).trim().to_string();
    let b = fx.sb.path("tool-b.git");
    fx.sb.git(&fx.sb.root, &["clone", "-q", "--bare", &a_url, b.to_str().unwrap()]);
    let b_url = b.to_str().unwrap().to_string();
    fx.sb.git(&tool, &["config", "remote.origin.pushurl", &b_url]);
    let repo_id = fx.sb.open(&tool).repo_id().get().to_string_lossy().into_owned();
    let scenes = fx.config.paths.root.join("daemon/push");
    let scene = |name: &str, body: Value| {
        let d = scenes.join(name);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("push.json"), body.to_string()).unwrap();
        d
    };
    let full = |branch: &str, id: &str, url: &str| {
        json!({ "repo": "tool", "commit": ch.c, "remote": "origin", "branch": branch, "repo_id": id, "push_url": url })
    };
    let same = scene("1-1", full("main", &repo_id, &b_url));
    let other = scene("1-2", full("release", &repo_id, &b_url));
    let legacy = scene("1-3", json!({ "repo": "tool", "commit": ch.c }));
    let pre_r4 = scene("1-4", json!({ "repo": "tool", "commit": ch.c, "remote": "origin", "branch": "main" }));
    let to_a = scene("1-5", full("main", &repo_id, &a_url));
    let other_id = scene("1-6", full("main", "/elsewhere/.git", &b_url));
    let core = fx.prepare().unwrap();
    sync(&core);
    let (r, _) = resolve(&core, ChainChoice::Restore);
    assert!(r.unwrap().pushed);
    assert_eq!(fx.sb.git(&b, &["rev-parse", "refs/heads/main"]).trim(), ch.c, "the restore pushed to B");
    assert!(!same.exists(), "the matching scene is settled");
    assert!(other.exists(), "another branch's scene stays");
    assert!(legacy.exists(), "a record without remote/branch stays");
    assert!(pre_r4.exists(), "a record without repo_id/push_url stays");
    assert!(to_a.exists(), "a scene of another push URL stays");
    assert!(other_id.exists(), "a scene of another repository identity stays");
}

/// Review-4 F1: the restore pushes to exactly the push URL it recorded in
/// the scene. Origin's push URL flips A→B after the destination was read
/// (while staging) and back B→A before the push is recorded: the commit
/// lands at A (the recorded destination), B is untouched, and the kept
/// scene for A settles — it is true that A has the commit.
#[test]
fn chain_resolve_restore_pushes_to_the_recorded_url_across_a_b_a() {
    let fx = Dfx::new();
    let ch = Chain::new(&fx);
    ch.rewind_origin(&fx, &ch.p);
    let tool = fx.config.repos[&repo_name("tool")].path.clone();
    let a_url = fx.sb.git(&tool, &["config", "remote.origin.url"]).trim().to_string();
    let b = fx.sb.path("tool-b.git");
    fx.sb.git(&fx.sb.root, &["clone", "-q", "--bare", &a_url, b.to_str().unwrap()]);
    let b_url = b.to_str().unwrap().to_string();
    let repo_id = fx.sb.open(&tool).repo_id().get().to_string_lossy().into_owned();
    let old = fx.config.paths.root.join("daemon/push/1-1");
    fs::create_dir_all(&old).unwrap();
    let body = json!({ "repo": "tool", "commit": ch.c, "remote": "origin", "branch": "main", "repo_id": repo_id, "push_url": a_url });
    fs::write(old.join("push.json"), body.to_string()).unwrap();
    let core = fx.prepare().unwrap();
    sync(&core);
    let git = &core.gits[&repo_name("tool")];
    let staging = git.pause_export_before_rename();
    let record = git.pause_push_before_record();
    let wait = |p: &strata_gitio::Pause, what: &str| {
        let start = std::time::Instant::now();
        while !p.is_paused() && start.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(p.is_paused(), "restore reached the {what} pause");
    };
    std::thread::scope(|sc| {
        let t = sc.spawn(|| resolve(&core, ChainChoice::Restore).0);
        let start = std::time::Instant::now();
        while !staging.is_paused() && !t.is_finished() && start.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(10));
        }
        if t.is_finished() {
            panic!("restore ended before staging: {:?}", t.join().unwrap());
        }
        wait(&staging, "staging");
        let scene = fs::read_dir(fx.config.paths.root.join("daemon/push")).unwrap().flatten().map(|d| d.path()).find(|d| *d != old).unwrap();
        let rec: Value = serde_json::from_str(&fs::read_to_string(scene.join("push.json")).unwrap()).unwrap();
        assert_eq!(rec["push_url"], a_url, "the scene records A");
        fx.sb.git(&tool, &["config", "remote.origin.pushurl", &b_url]);
        staging.release();
        wait(&record, "record");
        fx.sb.git(&tool, &["config", "--unset", "remote.origin.pushurl"]);
        record.release();
        assert!(t.join().unwrap().unwrap().pushed);
    });
    assert_eq!(fx.sb.git(&fx.sb.root, &["--git-dir", &a_url, "rev-parse", "refs/heads/main"]).trim(), ch.c, "the push went to A, as recorded");
    assert_eq!(fx.sb.git(&b, &["rev-parse", "refs/heads/main"]).trim(), ch.p, "B untouched");
    assert!(!old.exists(), "the scene for A settles: A has the commit");
}

/// Review-5 N1: a URL rewrite rule added while the restore stages (here a
/// prefix `insteadOf` that would send A to alt/<name>) does not move the
/// push: gitio pins the recorded URL, so the commit lands at A and alt is
/// untouched. The post-push check sees origin's destination changed and
/// settles nothing: the kept scene for A stays.
#[test]
fn chain_resolve_restore_is_pinned_against_url_rewriting() {
    let fx = Dfx::new();
    let ch = Chain::new(&fx);
    ch.rewind_origin(&fx, &ch.p);
    let tool = fx.config.repos[&repo_name("tool")].path.clone();
    let a_url = fx.sb.git(&tool, &["config", "remote.origin.url"]).trim().to_string();
    let a = Path::new(&a_url);
    let alt = fx.sb.path("alt");
    fs::create_dir_all(&alt).unwrap();
    let alt_repo = alt.join(a.file_name().unwrap());
    fx.sb.git(&fx.sb.root, &["clone", "-q", "--bare", &a_url, alt_repo.to_str().unwrap()]);
    let repo_id = fx.sb.open(&tool).repo_id().get().to_string_lossy().into_owned();
    let old = fx.config.paths.root.join("daemon/push/1-1");
    fs::create_dir_all(&old).unwrap();
    let body = json!({ "repo": "tool", "commit": ch.c, "remote": "origin", "branch": "main", "repo_id": repo_id, "push_url": a_url });
    fs::write(old.join("push.json"), body.to_string()).unwrap();
    let core = fx.prepare().unwrap();
    sync(&core);
    let git = &core.gits[&repo_name("tool")];
    let staging = git.pause_export_before_rename();
    std::thread::scope(|sc| {
        let t = sc.spawn(|| resolve(&core, ChainChoice::Restore).0);
        let start = std::time::Instant::now();
        while !staging.is_paused() && !t.is_finished() && start.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(10));
        }
        if t.is_finished() {
            panic!("restore ended before staging: {:?}", t.join().unwrap());
        }
        assert!(staging.is_paused(), "restore reached the staging pause");
        let rule = format!("url.{}/.insteadOf", alt.display());
        fx.sb.git(&tool, &["config", &rule, &format!("{}/", a.parent().unwrap().display())]);
        staging.release();
        assert!(t.join().unwrap().unwrap().pushed);
    });
    assert_eq!(fx.sb.git(&fx.sb.root, &["--git-dir", &a_url, "rev-parse", "refs/heads/main"]).trim(), ch.c, "the push went to A, as recorded");
    assert_eq!(fx.sb.git(&fx.sb.root, &["--git-dir", alt_repo.to_str().unwrap(), "rev-parse", "refs/heads/main"]).trim(), ch.p, "not to the rewritten URL");
    assert!(old.exists(), "origin's destination changed around the push: nothing settles");
}

/// Review-6 hole 2: a rule added after gitio's pre-send check and before
/// `git push` reads the config (an exact `pushInsteadOf` sending A to B),
/// and removed again right after the push (before the daemon's post-push
/// check), cannot redirect the restore: the push process checks its own
/// destination (D25), so nothing reaches B or A, the restore fails, and the
/// kept scene for A stays.
#[test]
fn chain_resolve_restore_rule_added_between_check_and_push_is_refused() {
    let fx = Dfx::new();
    let ch = Chain::new(&fx);
    ch.rewind_origin(&fx, &ch.p);
    let tool = fx.config.repos[&repo_name("tool")].path.clone();
    let a_url = fx.sb.git(&tool, &["config", "remote.origin.url"]).trim().to_string();
    let b = fx.sb.path("other.git");
    fx.sb.git(&fx.sb.root, &["clone", "-q", "--bare", &a_url, b.to_str().unwrap()]);
    let repo_id = fx.sb.open(&tool).repo_id().get().to_string_lossy().into_owned();
    let old = fx.config.paths.root.join("daemon/push/1-1");
    fs::create_dir_all(&old).unwrap();
    let body = json!({ "repo": "tool", "commit": ch.c, "remote": "origin", "branch": "main", "repo_id": repo_id, "push_url": a_url });
    fs::write(old.join("push.json"), body.to_string()).unwrap();
    let core = fx.prepare().unwrap();
    sync(&core);
    let git = &core.gits[&repo_name("tool")];
    let rule = format!("url.{}.pushInsteadOf", b.display());
    let checked = git.pause_push_after_check();
    let record = git.pause_push_before_record();
    let wait = |p: &strata_gitio::Pause, t: &std::thread::ScopedJoinHandle<'_, _>| {
        let start = std::time::Instant::now();
        while !p.is_paused() && !t.is_finished() && start.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    let r = std::thread::scope(|sc| {
        let t = sc.spawn(|| resolve(&core, ChainChoice::Restore).0);
        wait(&checked, &t);
        assert!(checked.is_paused(), "restore reached the push");
        fx.sb.git(&tool, &["config", &rule, &a_url]);
        checked.release();
        wait(&record, &t);
        // Gone again before anything after the push looks.
        fx.sb.git(&tool, &["config", "--unset", &rule]);
        record.release();
        t.join().unwrap()
    });
    let at = |u: &str| fx.sb.git(&fx.sb.root, &["--git-dir", u, "rev-parse", "refs/heads/main"]).trim().to_string();
    assert_eq!(at(b.to_str().unwrap()), ch.p, "nothing sent to B: {r:?}");
    assert_eq!(at(&a_url), ch.p, "nothing sent to A: {r:?}");
    assert!(r.is_err(), "the restore did not push: {r:?}");
    assert!(old.exists(), "the kept scene for A stays");
}

/// Review M7: Restore records the push in one transaction with the chain
/// check (tracking ref and anchor together). When the tracking ref moved
/// between the remote's acceptance and the recording, nothing local moves
/// and the outcome is `Unknown` (the scene is kept, M6) — never `Pushed`
/// with the anchor advanced alone.
#[test]
fn chain_resolve_restore_records_tracking_and_anchor_together() {
    let fx = Dfx::new();
    let ch = Chain::new(&fx);
    ch.rewind_origin(&fx, &ch.p);
    let core = fx.prepare().unwrap();
    sync(&core);
    let git = &core.gits[&repo_name("tool")];
    let tracking = "refs/strata/remotes/origin/main";
    let gap = git.pause_push_before_record();
    std::thread::scope(|sc| {
        let t = sc.spawn(|| resolve(&core, ChainChoice::Restore).0);
        let start = std::time::Instant::now();
        while !gap.is_paused() && !t.is_finished() && start.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(gap.is_paused(), "restore records through the recording push: {:?}", if t.is_finished() { Some(t.join().unwrap()) } else { None });
        assert_eq!(ch.rev(&fx, tracking), ch.p, "fetched the rewound origin");
        fx.sb.git(&ch.main, &["update-ref", tracking, &ch.a]);
        gap.release();
        let r = t.join().unwrap();
        let e = r.expect_err("unrecorded push is not Pushed");
        assert_eq!(e.code, Code::Unknown, "{e:?}");
        assert!(e.detail.as_ref().and_then(|d| d.get("scene")).is_some(), "scene kept: {e:?}");
    });
    assert_eq!(ch.refs(&fx), (ch.c.clone(), ch.a.clone()), "anchor not advanced alone");
    assert_eq!(ch.rev(&fx, tracking), ch.a);
}

#[test]
fn chain_resolve_restore_checks_push_deploy() {
    // Refused at the prompt: no fetch, no push, refs unchanged.
    {
        let mut fx = Dfx::new();
        set_confirm(&mut fx);
        let ch = Chain::new(&fx);
        ch.rewind_origin(&fx, &ch.p);
        let (c, log, _) = fx.confirmer(vec![Verdict::la(-2)]);
        let core = fx.prepare_with(c).unwrap();
        sync(&core);
        let before = ch.refs(&fx);
        let (r, _) = resolve(&core, ChainChoice::Restore);
        assert_eq!(code_reason(&r).0, Code::NotConfirmed, "{r:?}");
        assert_eq!(log.shown().len(), 1);
        assert!(log.shown()[0].contains("tool"), "{:?}", log.shown());
        assert_eq!(ch.refs(&fx), before);
        assert_eq!(ch.origin_main(&fx), ch.p);
        let k = calls(&core);
        assert_eq!((k.fetch, k.push, k.transactions), (0, 0, 0), "{k:?}");
    }
    // Approved: one prompt, one push.
    {
        let mut fx = Dfx::new();
        set_confirm(&mut fx);
        let ch = Chain::new(&fx);
        ch.rewind_origin(&fx, &ch.p);
        let (c, log, _) = fx.confirmer(vec![Verdict::pass()]);
        let core = fx.prepare_with(c).unwrap();
        sync(&core);
        let (r, _) = resolve(&core, ChainChoice::Restore);
        assert!(r.unwrap().pushed);
        assert_eq!(log.shown().len(), 1);
        assert_eq!(ch.origin_main(&fx), ch.c);
        assert_eq!(calls(&core).push, 1);
    }
    // Accepting origin never prompts, even when confirm is pinned.
    for choice in [ChainChoice::AcceptOriginDrop, ChainChoice::AcceptOriginReplay] {
        let mut fx = Dfx::new();
        set_confirm(&mut fx);
        let ch = Chain::new(&fx);
        ch.advance_origin(&fx, &[("o.txt", b"o\n")]);
        let (c, log, _) = fx.confirmer(vec![]);
        let core = fx.prepare_with(c).unwrap();
        sync(&core);
        let (r, _) = resolve(&core, choice);
        r.unwrap();
        assert!(log.shown().is_empty(), "{choice:?}");
        assert_eq!(log.probes(), 0);
        assert_eq!(calls(&core).push, 0);
    }
}

#[test]
fn chain_resolve_refs_under_repo_lock() {
    // Not a fork before the lock: refused with no fetch and no write.
    {
        let fx = Dfx::new();
        let ch = Chain::new(&fx); // chain leads origin
        let core = fx.prepare().unwrap();
        sync(&core);
        for choice in [ChainChoice::AcceptOriginDrop, ChainChoice::AcceptOriginReplay, ChainChoice::Restore] {
            let (r, _) = resolve(&core, choice);
            assert_eq!(code_reason(&r), (Code::InvalidInput, "not_rewound_or_diverged".into()));
            let k = calls(&core);
            assert_eq!((k.fetch, k.push, k.transactions), (0, 0, 0), "{k:?}");
        }
        assert_eq!(ch.refs(&fx), (ch.c.clone(), ch.a.clone()));
    }
    // A fork that the fetch inside the lock shows healed: classified again
    // under the lock, refused before any compare-and-swap.
    {
        let fx = Dfx::new();
        let ch = Chain::new(&fx);
        ch.rewind_origin(&fx, &ch.p);
        let core = fx.prepare().unwrap();
        sync(&core);
        ch.rewind_origin(&fx, &ch.a); // origin back at A: the chain leads again
        let (r, _) = resolve(&core, ChainChoice::AcceptOriginDrop);
        assert_eq!(code_reason(&r), (Code::InvalidInput, "not_rewound_or_diverged".into()));
        let k = calls(&core);
        assert_eq!((k.fetch, k.push, k.transactions), (1, 0, 0), "{k:?}");
        assert_eq!(ch.refs(&fx), (ch.c.clone(), ch.a.clone()));
    }
    // The repo lock held elsewhere: Busy, and nothing ran outside it.
    {
        let fx = Dfx::new();
        let ch = Chain::new(&fx);
        ch.advance_origin(&fx, &[("o.txt", b"o\n")]);
        let core = fx.prepare().unwrap();
        sync(&core);
        let before = ch.refs(&fx);
        let repo = repo_name("tool");
        std::thread::scope(|s| {
            let (tx, rx) = std::sync::mpsc::channel();
            let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
            let (core_r, repo_r) = (&core, &repo);
            s.spawn(move || {
                core_r.locks
                    .with_repo(repo_r, || {
                        tx.send(()).unwrap();
                        let _ = done_rx.recv_timeout(Duration::from_secs(10));
                        Ok(())
                    })
                    .unwrap();
            });
            rx.recv().unwrap();
            for choice in [ChainChoice::AcceptOriginDrop, ChainChoice::AcceptOriginReplay] {
                let (r, _) = resolve(&core, choice);
                assert_eq!(code_reason(&r), (Code::Busy, "lock_wait".into()));
            }
            done_tx.send(()).unwrap();
        });
        let k = calls(&core);
        assert_eq!((k.fetch, k.push, k.transactions, k.replay), (0, 0, 0, 0), "{k:?}");
        assert_eq!(ch.refs(&fx), before);
    }
    // Through the socket: the same handler, refs under the lock.
    {
        let fx = Dfx::new();
        let ch = Chain::new(&fx);
        let o = ch.advance_origin(&fx, &[("o.txt", b"o\n")]);
        let core = fx.prepare().unwrap();
        sync(&core);
        let d = Daemon::start(&core).unwrap();
        let sock = fx.socket();
        serving(&d, || {
            let r = call(&sock, "chain_resolve", json!({ "repo": "tool", "choice": "Bogus" }));
            assert_eq!(r["error"]["rpc"], json!("Failed"), "{r:#}");
            let v = ok(call(&sock, "chain_resolve", json!({ "repo": "tool", "choice": "AcceptOriginDrop" })));
            assert_eq!(v["chain"], json!(o));
            assert_eq!(v["anchor"], json!(o));
        });
        assert_eq!(ch.refs(&fx), (o.clone(), o));
    }
    let body = src("resolve.rs");
    assert!(!body.contains("update_ref("), "chain and anchor only through the one ref transaction");
}

#[test]
fn publish_calls_publish_not_stage() {
    let h = src("handlers.rs");
    assert!(h.contains("strata_publish::publish_bound("), "the publish handler calls publish::publish (bound to the shown plan)");
    for s in ["strata_publish::stage", "stage(", "Staged"] {
        assert!(!h.contains(s), "handlers.rs mentions {s}");
    }
    for f in ["resolve.rs", "main.rs", "client.rs", "lib.rs"] {
        assert!(!src(f).contains("strata_publish::stage"), "{f}");
    }
    let _ = CommitOid::try_from("0".repeat(40)).ok();
}

/// Review H2: a confirmed restore or push is bound to the chain top the
/// cli showed. A chain that moved between the display and the request
/// (C shown, C2 now) is `NotConfirmed` (`chain_moved`) even though it did
/// not move during the daemon's own prompt; nothing is pushed.
#[test]
fn confirmed_push_is_bound_to_the_shown_chain_top() {
    // chain_resolve Restore.
    {
        let mut fx = Dfx::new();
        set_confirm(&mut fx);
        let ch = Chain::new(&fx);
        ch.rewind_origin(&fx, &ch.p);
        let c2 = advance_chain(&fx, &ch);
        let (c, log, _) = fx.confirmer(vec![Verdict::pass(), Verdict::pass()]);
        let core = fx.prepare_with(c).unwrap();
        sync(&core);
        let before = ch.refs(&fx);
        let shown = CommitOid::try_from(ch.c.clone()).unwrap();
        let (r, _) = resolve_shown(&core, ChainChoice::Restore, Some(&shown));
        assert_eq!(code_reason(&r), (Code::NotConfirmed, "chain_moved".into()), "{r:?}");
        assert_eq!(ch.refs(&fx), before);
        assert_eq!(ch.origin_main(&fx), ch.p);
        assert_eq!(calls(&core).push, 0);
        // Nothing shown at all is not a plan either.
        let (r, _) = resolve_shown(&core, ChainChoice::Restore, None);
        assert_eq!(code_reason(&r), (Code::NotConfirmed, "chain_moved".into()), "{r:?}");
        assert_eq!(calls(&core).push, 0);
        assert_eq!(log.shown().len(), 2);
        let _ = c2;
    }
    // push over the socket.
    {
        let mut fx = Dfx::new();
        set_confirm(&mut fx);
        let ch = Chain::new(&fx);
        let c2 = advance_chain(&fx, &ch);
        let (c, _log, _) = fx.confirmer(vec![Verdict::pass(), Verdict::pass(), Verdict::pass()]);
        let core = fx.prepare_with(c).unwrap();
        sync(&core);
        let d = Daemon::start(&core).unwrap();
        let sock = fx.socket();
        serving(&d, || {
            for shown in [json!(ch.c), Value::Null] {
                let r = call(&sock, "push", json!({ "repo": "tool", "shown": shown }));
                assert_eq!(r["error"]["code"], "NotConfirmed", "{r:#}");
                assert_eq!(r["error"]["detail"]["reason"], "chain_moved", "{r:#}");
                assert_eq!(r["error"]["detail"]["now"], json!(c2), "{r:#}");
                assert_eq!(ch.origin_main(&fx), ch.a, "nothing pushed");
            }
            let r = ok(call(&sock, "push", json!({ "repo": "tool", "shown": c2 })));
            assert_eq!(r["commit"], json!(c2), "{r:#}");
            assert_eq!(ch.origin_main(&fx), c2);
        });
        drop(d);
        core.close().unwrap();
    }
}

/// Sets origin's push URL of `main` to `other` once a confirmation passed
/// (between the prompt and the push), through the daemon's test seam.
fn redirect_after_confirm(root: &Path, main: &Path, other: &Path) {
    let (main, other) = (main.to_path_buf(), other.to_path_buf());
    strata_daemon::seams::set_after_confirm(
        root,
        Some(std::sync::Arc::new(move || {
            let st = std::process::Command::new("git")
                .arg("-C")
                .arg(&main)
                .args(["config", "remote.origin.pushurl"])
                .arg(&other)
                .status()
                .unwrap();
            assert!(st.success());
        })),
    );
}

fn unredirect(root: &Path, fx: &Dfx, main: &Path) {
    strata_daemon::seams::set_after_confirm(root, None);
    fx.sb.git(main, &["config", "--unset", "remote.origin.pushurl"]);
}

/// Review P1-15: a confirmed restore or push is bound to origin's push URL
/// read before the prompt (the prompt names it). A push URL changed after
/// the confirmation is `NotConfirmed` (`remote_changed`): no ref moves and
/// nothing reaches either remote. With the URL unchanged it goes through.
#[test]
fn confirmed_push_is_bound_to_the_approved_url() {
    // chain_resolve Restore.
    {
        let mut fx = Dfx::new();
        set_confirm(&mut fx);
        let ch = Chain::new(&fx);
        ch.rewind_origin(&fx, &ch.p);
        let elsewhere = fx.sb.root.join("elsewhere.git");
        fx.sb.git(&fx.sb.root, &["init", "-q", "--bare", elsewhere.to_str().unwrap()]);
        let (c, log, _) = fx.confirmer(vec![Verdict::pass(), Verdict::pass()]);
        let core = fx.prepare_with(c).unwrap();
        sync(&core);
        let root = core.config.paths.root.clone();
        redirect_after_confirm(&root, &ch.main, &elsewhere);
        let before = ch.refs(&fx);
        let shown = CommitOid::try_from(ch.c.clone()).unwrap();
        let (r, _) = resolve_shown(&core, ChainChoice::Restore, Some(&shown));
        assert_eq!(code_reason(&r), (Code::NotConfirmed, "remote_changed".into()), "{r:?}");
        assert!(log.shown()[0].contains("tool-origin.git"), "the prompt names the URL: {:?}", log.shown());
        assert_eq!(ch.refs(&fx), before);
        assert_eq!(ch.origin_main(&fx), ch.p);
        assert_eq!(calls(&core).push, 0);
        assert_eq!(fx.sb.git(&fx.sb.root, &["--git-dir", elsewhere.to_str().unwrap(), "for-each-ref"]), "");
        unredirect(&root, &fx, &ch.main);
        let (r, _) = resolve_shown(&core, ChainChoice::Restore, Some(&shown));
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(ch.origin_main(&fx), ch.c);
        core.close().unwrap();
    }
    // push over the socket.
    {
        let mut fx = Dfx::new();
        set_confirm(&mut fx);
        let ch = Chain::new(&fx);
        let elsewhere = fx.sb.root.join("elsewhere.git");
        fx.sb.git(&fx.sb.root, &["init", "-q", "--bare", elsewhere.to_str().unwrap()]);
        let (c, _log, _) = fx.confirmer(vec![Verdict::pass(), Verdict::pass()]);
        let core = fx.prepare_with(c).unwrap();
        sync(&core);
        let root = core.config.paths.root.clone();
        redirect_after_confirm(&root, &ch.main, &elsewhere);
        let d = Daemon::start(&core).unwrap();
        let sock = fx.socket();
        serving(&d, || {
            let r = call(&sock, "push", json!({ "repo": "tool", "shown": ch.c }));
            assert_eq!(r["error"]["code"], "NotConfirmed", "{r:#}");
            assert_eq!(r["error"]["detail"]["reason"], "remote_changed", "{r:#}");
            assert_eq!(r["error"]["detail"]["now"], json!(elsewhere.to_str().unwrap()), "{r:#}");
            assert_eq!(ch.origin_main(&fx), ch.a, "nothing pushed");
            assert_eq!(fx.sb.git(&fx.sb.root, &["--git-dir", elsewhere.to_str().unwrap(), "for-each-ref"]), "");
            unredirect(&root, &fx, &ch.main);
            let r = ok(call(&sock, "push", json!({ "repo": "tool", "shown": ch.c })));
            assert_eq!(r["commit"], json!(ch.c), "{r:#}");
            assert_eq!(ch.origin_main(&fx), ch.c);
        });
        drop(d);
        core.close().unwrap();
    }
}

/// C2 on C on the chain (a local publish that landed after the display).
fn advance_chain(fx: &Dfx, ch: &Chain) -> String {
    let sb = &fx.sb;
    let a = sb.head(&ch.main);
    sb.git(&ch.main, &["reset", "-q", "--hard", &ch.c]);
    let c2 = sb.commit(&ch.main, &[("c2.txt", b"c2\n")], "C2");
    sb.git(&ch.main, &["reset", "-q", "--hard", &a]);
    sb.git(&ch.main, &["update-ref", CHAIN, &c2]);
    c2
}

// ---- push: the headless replay of a Diverged chain (publish.md D2a) ----

fn no_force(k: &Calls) {
    for a in &k.push_args {
        assert!(!a.starts_with("--force") && !a.starts_with('+'), "{:?}", k.push_args);
    }
}

/// A repo without confirmation: origin moved under the unpushed chain C.
/// `push` fetches, replays C onto O, runs the checks on the new tip (with
/// the sealed deps set cloned in), moves chain and anchor in one
/// transaction, pushes once as a fast-forward and rebases the session
/// that sat on C onto its copy.
#[test]
fn headless_push_replays_a_diverged_chain_then_pushes() {
    let mut fx = Dfx::new();
    {
        let rc = fx.config.repos.get_mut(&repo_name("tool")).unwrap();
        rc.checks = vec!["test -f o.txt".into(), "test -d node_modules".into()];
    }
    fx.write_config();
    let ch = Chain::new(&fx);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    serving(&d, || {
        let s = new_session(&sock);
        let head = |s: &SessionId| core.store.session(s).unwrap().unwrap().manifest.repos[&repo_name("tool")].head.as_str().to_string();
        assert_eq!(head(&s), ch.c, "the session is pinned on the unpushed chain top");
        let o = ch.advance_origin(&fx, &[("o.txt", b"o\n")]);
        core.gits[&repo_name("tool")].take_git_calls();
        let r = ok(call(&sock, "push", json!({ "repo": "tool" })));
        let new = r["commit"].as_str().unwrap().to_string();
        assert_ne!(new, ch.c);
        assert_eq!(r["push"]["kind"], json!("Pushed"), "{r:#}");
        assert_eq!(r["warnings"], json!([]), "{r:#}");
        assert_eq!(ch.origin_main(&fx), new, "origin fast-forwarded to the replayed tip");
        assert_eq!(fx.sb.git(&ch.main, &["rev-parse", &format!("{new}^")]).trim(), o);
        assert_eq!(ch.refs(&fx), (new.clone(), new.clone()));
        assert_eq!(r["replayed"]["from"], json!(ch.c), "{r:#}");
        assert_eq!(r["replayed"]["onto"], json!(o), "{r:#}");
        assert_eq!(r["replayed"]["commits"], json!(1), "{r:#}");
        assert_eq!(r["replayed"]["checks_ran"], json!(2), "{r:#}");
        let files = fx.sb.git(&ch.main, &["ls-tree", "--name-only", &new]);
        for f in ["a.txt", "c.txt", "o.txt"] {
            assert!(files.lines().any(|l| l == f), "{f} in {files}");
        }
        assert!(!files.lines().any(|l| l == "node_modules"), "deps never enter the tree: {files}");
        let k = calls(&core);
        assert_eq!((k.fetch >= 1, k.push, k.transactions >= 1, k.replay), (true, 1, true, 1), "{k:?}");
        no_force(&k);
        assert_eq!(r["rebases"][0]["session"], json!(s.as_str()), "{r:#}");
        assert_eq!(r["rebases"][0]["report"]["kind"], json!("Moved"), "{r:#}");
        assert_eq!(head(&s), new, "the session follows its replayed commit");
        // A second push has nothing left to do.
        let r = call(&sock, "push", json!({ "repo": "tool" }));
        assert_eq!(r["error"]["code"], "NoCommit", "{r:#}");
    });
    drop(d);
    core.close().unwrap();
}

/// The unpushed chain does not apply onto the new origin: `Conflict`
/// (`replay_conflict`) with the commit that failed; chain, anchor and
/// origin unchanged, no transaction, no push. A failing check after a
/// clean replay is `CheckFailed`, likewise with nothing written. Rewound
/// stays the human's (`chain_rewound`).
#[test]
fn headless_push_conflict_or_failed_check_writes_and_pushes_nothing() {
    // Conflict.
    {
        let fx = Dfx::new();
        let ch = Chain::new(&fx);
        let c2 = fx.sb.commit(&ch.main, &[("a.txt", b"chain\n")], "C2");
        fx.sb.git(&ch.main, &["reset", "-q", "--hard", &ch.a]);
        fx.sb.git(&ch.main, &["update-ref", CHAIN, &c2]);
        let o = ch.advance_origin(&fx, &[("a.txt", b"origin\n")]);
        let core = fx.prepare().unwrap();
        let d = Daemon::start(&core).unwrap();
        let sock = fx.socket();
        serving(&d, || {
            let before = ch.refs(&fx);
            core.gits[&repo_name("tool")].take_git_calls();
            let r = call(&sock, "push", json!({ "repo": "tool" }));
            assert_eq!(r["error"]["code"], "Conflict", "{r:#}");
            let det = &r["error"]["detail"];
            assert_eq!(det["reason"], "replay_conflict", "{r:#}");
            assert_eq!(det["commit"], json!(c2), "{r:#}");
            assert_eq!(det["chain"], json!(c2), "{r:#}");
            assert_eq!(det["origin"], json!(o), "{r:#}");
            assert_eq!(det["pushed"], json!(false), "{r:#}");
            assert_eq!(ch.refs(&fx), before);
            assert_eq!(ch.origin_main(&fx), o);
            let k = calls(&core);
            assert_eq!((k.push, k.transactions), (0, 0), "{k:?}");
        });
        drop(d);
        core.close().unwrap();
    }
    // A failing check.
    {
        let mut fx = Dfx::new();
        fx.config.repos.get_mut(&repo_name("tool")).unwrap().checks = vec!["false".into()];
        fx.write_config();
        let ch = Chain::new(&fx);
        let core = fx.prepare().unwrap();
        let d = Daemon::start(&core).unwrap();
        let sock = fx.socket();
        serving(&d, || {
            // Unsealed deps: the checks cannot run; refused, nothing written.
            let o = ch.advance_origin(&fx, &[("o.txt", b"o\n")]);
            let before = ch.refs(&fx);
            let r = call(&sock, "push", json!({ "repo": "tool" }));
            assert_eq!(r["error"]["detail"]["reason"], "replay_deps_unsealed", "{r:#}");
            assert_eq!(ch.refs(&fx), before);
            new_session(&sock); // seals the deps set of these inputs
            assert_eq!(ch.refs(&fx), before);
            core.gits[&repo_name("tool")].take_git_calls();
            let r = call(&sock, "push", json!({ "repo": "tool" }));
            assert_eq!(r["error"]["code"], "CheckFailed", "{r:#}");
            assert_eq!(ch.refs(&fx), before);
            assert_eq!(ch.origin_main(&fx), o);
            let k = calls(&core);
            assert_eq!((k.push, k.transactions), (0, 0), "{k:?}");
        });
        drop(d);
        core.close().unwrap();
    }
    // Rewound.
    {
        let fx = Dfx::new();
        let ch = Chain::new(&fx);
        let q = ch.rewind_to_unrelated(&fx);
        let core = fx.prepare().unwrap();
        let d = Daemon::start(&core).unwrap();
        let sock = fx.socket();
        serving(&d, || {
            let before = ch.refs(&fx);
            core.gits[&repo_name("tool")].take_git_calls();
            let r = call(&sock, "push", json!({ "repo": "tool" }));
            assert_eq!(r["error"]["code"], "InvalidInput", "{r:#}");
            assert_eq!(r["error"]["detail"]["reason"], "chain_rewound", "{r:#}");
            assert_eq!(ch.refs(&fx), before);
            assert_eq!(ch.origin_main(&fx), q);
            let k = calls(&core);
            assert_eq!((k.push, k.transactions, k.replay), (0, 0, 0), "{k:?}");
        });
        drop(d);
        core.close().unwrap();
    }
}

/// A repo that needs confirmation keeps the gated path: without a
/// confirmation nothing is fetched, replayed or pushed; with one, a
/// Diverged chain is still not replayed (`NoCommit`, the human's `chain`).
#[test]
fn confirmed_repo_push_never_replays() {
    let mut fx = Dfx::new();
    set_confirm(&mut fx);
    let ch = Chain::new(&fx);
    let o = ch.advance_origin(&fx, &[("o.txt", b"o\n")]);
    let (c, _log, _) = fx.confirmer(vec![Verdict::la(-2), Verdict::pass()]);
    let core = fx.prepare_with(c).unwrap();
    sync(&core);
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    serving(&d, || {
        let before = ch.refs(&fx);
        let r = call(&sock, "push", json!({ "repo": "tool", "shown": ch.c }));
        assert_ne!(r["ok"], json!(true), "{r:#}");
        let k = calls(&core);
        assert_eq!((k.fetch, k.push, k.transactions, k.replay), (0, 0, 0, 0), "{k:?}");
        let r = call(&sock, "push", json!({ "repo": "tool", "shown": ch.c }));
        assert_eq!(r["error"]["code"], "NoCommit", "{r:#}");
        let k = calls(&core);
        assert_eq!((k.push, k.transactions, k.replay), (0, 0, 0), "{k:?}");
        assert_eq!(ch.refs(&fx), before);
        assert_eq!(ch.origin_main(&fx), o);
    });
    drop(d);
    core.close().unwrap();
}

// ---- push: the check siblings of a headless replay (publish.md D21) ----

/// tool's checks need `../admin` (check_siblings). A headless replay clones
/// admin's main-tip base next to the replayed tree, the checks see it, and
/// the scene (sibling included) is gone afterwards.
#[test]
fn headless_replay_supplies_the_check_siblings() {
    let mut fx = Dfx::with(&["tool", "admin"]);
    {
        let rc = fx.config.repos.get_mut(&repo_name("tool")).unwrap();
        rc.checks = vec!["test -f o.txt".into(), "test -f ../admin/package.json".into()];
        rc.check_siblings = vec![repo_name("admin")];
    }
    fx.write_config();
    let ch = Chain::new(&fx);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    serving(&d, || {
        new_session(&sock); // seals the deps set the replay needs
        let o = ch.advance_origin(&fx, &[("o.txt", b"o\n")]);
        let r = ok(call(&sock, "push", json!({ "repo": "tool" })));
        assert_eq!(r["push"]["kind"], json!("Pushed"), "{r:#}");
        assert_eq!(r["replayed"]["onto"], json!(o), "{r:#}");
        assert_eq!(r["replayed"]["checks_ran"], json!(2), "{r:#}");
        let scenes = fx.sb.path("strata").join("daemon").join("push");
        let left: Vec<_> = fs::read_dir(&scenes).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
        assert!(left.is_empty(), "the scene and its sibling are removed: {left:?}");
    });
    drop(d);
    core.close().unwrap();
}

/// A sibling that cannot be supplied refuses a replay up front:
/// `siblings_missing` with the push retry as the fix; nothing written or
/// pushed. A push that needs no replay is not refused for it.
#[test]
fn headless_replay_with_a_missing_sibling_is_siblings_missing() {
    let mut fx = Dfx::with(&["tool", "admin"]);
    {
        let rc = fx.config.repos.get_mut(&repo_name("tool")).unwrap();
        rc.checks = vec!["true".into()];
        rc.check_siblings = vec![repo_name("admin")];
        let ac = fx.config.repos.get_mut(&repo_name("admin")).unwrap();
        ac.default_branch = "no-such-branch".into();
        ac.publish.branch = "no-such-branch".into();
    }
    fx.write_config();
    let ch = Chain::new(&fx);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    serving(&d, || {
        let o = ch.advance_origin(&fx, &[("o.txt", b"o\n")]);
        let before = ch.refs(&fx);
        core.gits[&repo_name("tool")].take_git_calls();
        let r = call(&sock, "push", json!({ "repo": "tool" }));
        assert_eq!(r["error"]["code"], "InvalidInput", "{r:#}");
        let det = &r["error"]["detail"];
        assert_eq!(det["reason"], "siblings_missing", "{r:#}");
        assert_eq!(det["repo"], "tool", "{r:#}");
        assert_eq!(det["session"], Value::Null, "{r:#}");
        assert_eq!(det["missing"][0]["repo"], "admin", "{r:#}");
        assert_eq!(det["missing"][0]["why"], "main_tip_unavailable", "{r:#}");
        assert_eq!(det["fix"], "retry `git-strata push tool`", "{r:#}");
        assert_eq!(ch.refs(&fx), before);
        assert_eq!(ch.origin_main(&fx), o);
        // The in-memory replay may have run; no ref was written, nothing pushed.
        let k = calls(&core);
        assert_eq!((k.push, k.transactions), (0, 0), "{k:?}");
        let scenes = fx.sb.path("strata").join("daemon").join("push");
        let left: Vec<_> = fs::read_dir(&scenes).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
        assert!(left.is_empty(), "no scene was made: {left:?}");
    });
    drop(d);
    core.close().unwrap();
}

/// A push that needs no replay never needs the siblings: the Leads chain is
/// pushed though admin cannot be supplied.
#[test]
fn a_leads_push_is_not_refused_for_a_missing_sibling() {
    let mut fx = Dfx::with(&["tool", "admin"]);
    {
        let rc = fx.config.repos.get_mut(&repo_name("tool")).unwrap();
        rc.checks = vec!["true".into()];
        rc.check_siblings = vec![repo_name("admin")];
        let ac = fx.config.repos.get_mut(&repo_name("admin")).unwrap();
        ac.default_branch = "no-such-branch".into();
        ac.publish.branch = "no-such-branch".into();
    }
    fx.write_config();
    let ch = Chain::new(&fx);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    serving(&d, || {
        let r = ok(call(&sock, "push", json!({ "repo": "tool" })));
        assert_eq!(r["push"]["kind"], json!("Pushed"), "{r:#}");
        assert_eq!(r["replayed"], Value::Null, "{r:#}");
        assert_eq!(ch.origin_main(&fx), ch.refs(&fx).0, "{r:#}");
    });
    drop(d);
    core.close().unwrap();
}

/// Sibling preparation that is `Busy` or `Unknown` stops every push before
/// the repo lock, a Leads one included: nothing is pushed or written; once
/// the cause is gone the same push goes through.
#[test]
fn a_busy_or_unknown_sibling_prep_stops_even_a_leads_push() {
    let mut fx = Dfx::with(&["tool", "admin"]);
    {
        let rc = fx.config.repos.get_mut(&repo_name("tool")).unwrap();
        rc.checks = vec!["true".into()];
        rc.check_siblings = vec![repo_name("admin")];
    }
    fx.write_config();
    let ch = Chain::new(&fx);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    let (tool, admin) = (repo_name("tool"), repo_name("admin"));
    serving(&d, || {
        let origin = ch.origin_main(&fx);
        let before = ch.refs(&fx);
        core.gits[&tool].take_git_calls();
        // admin's lock held elsewhere: Busy naming the sibling.
        std::thread::scope(|s| {
            let (tx, rx) = std::sync::mpsc::channel();
            let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
            let (core_r, admin_r) = (&core, &admin);
            s.spawn(move || {
                core_r
                    .locks
                    .with_repo(admin_r, || {
                        tx.send(()).unwrap();
                        let _ = done_rx.recv_timeout(Duration::from_secs(30));
                        Ok(())
                    })
                    .unwrap();
            });
            rx.recv().unwrap();
            let r = call(&sock, "push", json!({ "repo": "tool" }));
            done_tx.send(()).unwrap();
            assert_eq!(r["error"]["code"], "Busy", "{r:#}");
            assert_eq!(r["error"]["detail"]["sibling"], "admin", "{r:#}");
        });
        // admin's anchor cannot be advanced after its fetch: Unknown.
        strata_publish::seams::fail_once(&core.gits[&admin], strata_publish::seams::FailPoint::SiblingAnchor);
        let r = call(&sock, "push", json!({ "repo": "tool" }));
        assert_eq!(r["error"]["code"], "Unknown", "{r:#}");
        assert_eq!(r["error"]["detail"]["reason"], "anchor_transaction", "{r:#}");
        assert_eq!(r["error"]["detail"]["sibling"], "admin", "{r:#}");
        let k = calls(&core);
        assert_eq!((k.fetch, k.push, k.transactions), (0, 0, 0), "tool untouched: {k:?}");
        assert_eq!(ch.refs(&fx), before);
        assert_eq!(ch.origin_main(&fx), origin, "nothing pushed");
        let r = ok(call(&sock, "push", json!({ "repo": "tool" })));
        assert_eq!(r["push"]["kind"], json!("Pushed"), "{r:#}");
        assert_eq!(ch.origin_main(&fx), ch.refs(&fx).0);
    });
    drop(d);
    core.close().unwrap();
}

/// The push scene is removed rooted and checked (D21): a failed removal
/// is never dropped silently.
#[test]
fn push_scene_cleanup_is_rooted_and_checked() {
    let body = src("resolve.rs");
    let start = body.find("pub(crate) fn with_staging").expect("with_staging");
    let rest = &body[start..];
    let end = rest.find("\nfn scene_rel").expect("scene_rel follows");
    // After the uncertain branch (the scene is kept for an Unknown push).
    let f = &rest[rest.find("if uncertain(&r)").expect("uncertain")..end];
    assert!(f.contains("strata_publish::remove_layout"), "{f}");
    assert!(f.contains("scene_outcome(&dir, r, removed)"), "the removal decides the result: {f}");
    assert!(!f.contains("remove_dir_all"), "{f}");
}
