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
        assert!(!fx.config.paths.root.join("daemon/push").read_dir().map(|mut d| d.next().is_some()).unwrap_or(false));
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
