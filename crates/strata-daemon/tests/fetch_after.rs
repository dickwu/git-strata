//! Task 5: §5.7 anchor after fetch, observe retry, §5.10 alerts, and the
//! ToolCall hint/flush thread rule.

mod support;

use std::cell::Cell;
use std::fs;
use std::path::Path;
use std::sync::mpsc;

use strata_daemon::DaemonWarning;
use strata_daemon::fetch_after::{
    AnchorStep, alert_attach, attach_outside_reason, observe_if_moved, report_guard_disabled, shadow_dir,
};
use strata_daemon::startup::Daemon;
use strata_daemon::track;
use strata_deps::ChainState;
use strata_overlay::{DEFAULT_DIRTY_CAPACITY, Tracker};
use strata_session::{NewWarning, SessionSeam};
use strata_types::{Code, CommitOid, StrataError};
use support::*;

/// gitio's tracking ref (`remote_ref("origin", "main")`).
const ORIGIN: &str = "refs/strata/remotes/origin/main";
const CHAIN: &str = "refs/strata/publish/main";
const ANCHOR: &str = "refs/strata/anchor/main";

fn commit_on(fx: &Dfx, main: &Path, parent: &str, msg: &str) -> String {
    let tree = fx.sb.tree_of(main, parent);
    fx.sb.git(main, &["commit-tree", &tree, "-p", parent, "-m", msg])
}

fn set(fx: &Dfx, main: &Path, name: &str, v: Option<&str>) {
    match v {
        Some(o) => fx.sb.git(main, &["update-ref", name, o]),
        None => fx.sb.git(main, &["update-ref", "-d", name]),
    };
}

fn get(fx: &Dfx, main: &Path, name: &str) -> Option<String> {
    let o = fx.sb.git_output(main, &["rev-parse", "-q", "--verify", name]);
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn oid(s: &str) -> CommitOid {
    CommitOid::try_from(s.to_string()).unwrap()
}

/// update-ref calls since the last take: (with --stdin, without).
fn update_ref_calls(core: &strata_daemon::startup::Core) -> (usize, usize) {
    let mut n = (0, 0);
    for c in core.gits[&repo_name("tool")].take_git_calls() {
        if c.args.iter().any(|a| a == "update-ref") {
            if c.args.iter().any(|a| a == "--stdin") {
                n.0 += 1;
            } else {
                n.1 += 1;
            }
        }
    }
    n
}

#[test]
fn anchor_advances_after_fetch() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let main = fx.main("tool");
    let rn = repo_name("tool");
    let a = fx.sb.head(&main);
    let b = commit_on(&fx, &main, &a, "b");
    let c = commit_on(&fx, &main, &b, "c");
    let d = commit_on(&fx, &main, &b, "d");
    // A real gitio fetch from the local bare origin moves the tracking ref to c.
    fx.sb.git(&main, &["push", "-q", "origin", &format!("{c}:refs/heads/main")]);
    assert_eq!(core.gits[&rn].fetch("origin", "main").unwrap(), oid(&c));
    assert_eq!(get(&fx, &main, ORIGIN).as_deref(), Some(c.as_str()));

    type Case<'a> = (&'a str, Option<&'a str>, Option<&'a str>, &'a str, AnchorStep, &'a str);
    let cases: Vec<Case> = vec![
        // (name, origin, chain, anchor before, step, anchor after)
        ("equal", Some(&c), Some(&c), &a, AnchorStep::Advanced(oid(&c)), &c),
        ("leads", Some(&b), Some(&c), &a, AnchorStep::Advanced(oid(&b)), &b),
        ("behind", Some(&c), Some(&b), &a, AnchorStep::Advanced(oid(&b)), &b),
        ("diverged", Some(&c), Some(&d), &a, AnchorStep::Advanced(oid(&b)), &b),
        ("absent", Some(&c), None, &a, AnchorStep::NotCalled(Some(ChainState::Absent)), &a),
        ("rewound", Some(&c), Some(&d), &d, AnchorStep::NotCalled(Some(ChainState::Rewound)), &d),
        ("not past the anchor", Some(&c), Some(&c), &c, AnchorStep::Kept, &c),
        ("never fetched", None, Some(&c), &a, AnchorStep::NotCalled(None), &a),
    ];
    for (name, origin, chain, before, step, after) in cases {
        set(&fx, &main, ORIGIN, origin);
        set(&fx, &main, CHAIN, chain);
        set(&fx, &main, ANCHOR, Some(before));
        update_ref_calls(&core);
        let got = core.advance_anchor(&rn).unwrap();
        assert_eq!(got, step, "{name}");
        assert_eq!(get(&fx, &main, ANCHOR).as_deref(), Some(after), "{name}");
        // The chain never moves; the anchor write is one transaction.
        assert_eq!(get(&fx, &main, CHAIN).as_deref(), chain, "{name}");
        let calls = update_ref_calls(&core);
        let want = if matches!(step, AnchorStep::Advanced(_)) { (1, 0) } else { (0, 0) };
        assert_eq!(calls, want, "{name}: (transactions, single update-refs)");
    }

    // Under the repo lock: while another holder has it, nothing moves (Busy).
    set(&fx, &main, ORIGIN, Some(&c));
    set(&fx, &main, CHAIN, Some(&c));
    set(&fx, &main, ANCHOR, Some(&a));
    let (held_tx, held_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    std::thread::scope(|s| {
        let locks = &core.locks;
        let rn2 = rn.clone();
        s.spawn(move || {
            locks
                .with_repo(&rn2, || {
                    held_tx.send(()).unwrap();
                    go_rx.recv().unwrap();
                    Ok(())
                })
                .unwrap();
        });
        held_rx.recv().unwrap();
        let e = core.advance_anchor(&rn).unwrap_err();
        assert_eq!(e.code, Code::Busy, "{e:?}");
        assert_eq!(get(&fx, &main, ANCHOR).as_deref(), Some(a.as_str()));
        go_tx.send(()).unwrap();
    });

    // A transaction that was called and failed: Unknown, AnchorNotAdvanced,
    // not retried. The next successful advance clears the warning.
    let lock = main.join(".git/refs/strata/anchor/main.lock");
    fs::write(&lock, b"").unwrap();
    update_ref_calls(&core);
    let e = core.advance_anchor(&rn).unwrap_err();
    assert_eq!(e.code, Code::Unknown, "{e:?}");
    assert_eq!(reason(&e), Some("anchor_transaction"));
    assert_eq!(update_ref_calls(&core), (1, 0), "called once, not retried");
    assert!(core.warnings.snapshot().contains(&DaemonWarning::AnchorNotAdvanced { repo: rn.clone() }));
    fs::remove_file(&lock).unwrap();
    assert_eq!(core.advance_anchor(&rn).unwrap(), AnchorStep::Advanced(oid(&c)));
    assert!(!core.warnings.snapshot().iter().any(|w| matches!(w, DaemonWarning::AnchorNotAdvanced { .. })));
    core.close().unwrap();
}

#[test]
fn observe_failed_does_not_fail_new_or_publish() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let rn = repo_name("tool");
    {
        let d = Daemon::start(&core).unwrap();

        // new's own observe fails: new succeeds with ObserveFailed; the
        // daemon observes once more; still failing stays a warning.
        d.sessions.arm(SessionSeam::FailAt { phase: "new.observe".into(), errno: libc::EIO });
        let mut rep = d.sessions.new(None, std::slice::from_ref(&rn)).unwrap();
        assert!(matches!(&rep.warnings[..], [NewWarning::ObserveFailed { repo, .. }] if *repo == rn), "{:?}", rep.warnings);
        let calls = Cell::new(0);
        let out = core.after_new(&mut rep, &|_, _| {
            calls.set(calls.get() + 1);
            Err(StrataError::new(Code::Unknown, "observe again failed"))
        });
        assert_eq!(calls.get(), 1);
        assert_eq!(out.reobserved, vec![rn.clone()]);
        match &rep.warnings[..] {
            [NewWarning::ObserveFailed { repo, detail }] => {
                assert_eq!(*repo, rn);
                assert_eq!(detail.message, "observe again failed");
            }
            w => panic!("{w:?}"),
        }
        let kind = serde_json::to_value(&rep.warnings[0]).unwrap();
        assert_eq!(kind["kind"], "ObserveFailed");
        assert!(core.store.session(&rep.manifest.id).unwrap().is_some(), "the session exists");

        // The retry with the real observe succeeds: the warning goes.
        d.sessions.arm(SessionSeam::FailAt { phase: "new.observe".into(), errno: libc::EIO });
        let mut rep2 = d.sessions.new(None, std::slice::from_ref(&rn)).unwrap();
        assert_eq!(rep2.warnings.len(), 1);
        core.after_new(&mut rep2, &|r, repo| core.observe_new(r, repo));
        assert!(rep2.warnings.is_empty(), "{:?}", rep2.warnings);

        // No ObserveFailed: the daemon does not observe again.
        let mut rep3 = d.sessions.new(None, std::slice::from_ref(&rn)).unwrap();
        let out = core.after_new(&mut rep3, &|_, _| panic!("not observed again"));
        assert!(out.reobserved.is_empty());
    }

    // Outside new/publish: observe only when main_tip moved; one retry;
    // the second failure is returned as the warning, never an error.
    let (x, y) = (oid(&"1".repeat(40)), oid(&"2".repeat(40)));
    let n = Cell::new(0);
    let mut never = || -> strata_daemon::Result<()> {
        n.set(n.get() + 1);
        Ok(())
    };
    assert!(observe_if_moved(Some(&x), Some(&x), &mut never).is_none());
    assert_eq!(n.get(), 0);
    let mut flaky = || -> strata_daemon::Result<()> {
        n.set(n.get() + 1);
        if n.get() == 1 { Err(StrataError::new(Code::Unknown, "first")) } else { Ok(()) }
    };
    assert!(observe_if_moved(Some(&x), Some(&y), &mut flaky).is_none());
    assert_eq!(n.get(), 2);
    n.set(0);
    let mut broken = || -> strata_daemon::Result<()> {
        n.set(n.get() + 1);
        Err(StrataError::new(Code::Unknown, format!("try {}", n.get())))
    };
    let w = observe_if_moved(None, Some(&y), &mut broken).expect("ObserveFailed");
    assert_eq!((n.get(), w.message.as_str()), (2, "try 2"));
    core.close().unwrap();
}

#[test]
fn hint_and_flush_off_callback_thread() {
    let fx = Dfx::new();
    let root = fx.sb.path("tracked");
    fs::create_dir_all(root.join("src")).unwrap();
    let mut t = Tracker::new(&root, DEFAULT_DIRTY_CAPACITY);
    t.watch().unwrap();
    fs::write(root.join("src/a.ts"), b"a\n").unwrap();
    // On a request thread: hint then flush.
    std::thread::scope(|s| {
        s.spawn(|| track::prepare_tool_call(&t, &rps(&["src/a.ts"]))).join().unwrap();
    });
    let v = t.inbox().pending();
    assert!(v.paths.contains(&rp("src/a.ts")) || v.full, "{v:?}");
    track::prepare_tool_call(&t, &[]);
    assert!(t.inbox().pending().full, "no path list → full scan");
    // Dropped on this (request/shutdown) thread, not in the callback.
    track::drop_tracker(t);

    // The daemon never builds a watcher callback of its own and calls hint,
    // flush and require_full only from track.rs (request thread).
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for e in fs::read_dir(&src).unwrap() {
        let p = e.unwrap().path();
        let text = fs::read_to_string(&p).unwrap();
        let code: String = text.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
        for needle in ["watch_with", "Watcher::", "dispatch_sync", "FlushSync"] {
            assert!(!code.contains(needle), "{needle} in {}", p.display());
        }
        if p.file_name().unwrap() != "track.rs" {
            for needle in [".hint(", "racker.flush()", ".require_full()", "inbox()"] {
                assert!(!code.contains(needle), "{needle} outside track.rs: {}", p.display());
            }
        }
    }
    // overlay's callback (the one the daemon gets via Tracker::watch) only notes.
    let overlay = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../strata-overlay/src/tracker.rs")).unwrap();
    assert!(overlay.contains("move |ev| inbox.note(&ev)"), "Tracker::watch callback changed");
}

#[test]
fn attach_outside_alerts_without_touching_the_index() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let main = fx.main("tool");
    let index = main.join(".git/index");
    let before = (fs::read(&index).unwrap(), fs::metadata(&index).unwrap().modified().unwrap());

    let direct = StrataError::new(Code::Unknown, "reading back refused")
        .with_detail(serde_json::json!({ "reason": "index-read-back-refused", "path": "/x/index" }));
    let gitio = StrataError::new(Code::Unknown, "staging changed")
        .with_detail(serde_json::json!({ "reason": "staging-changed", "path": "/x/tree" }));
    let wrapped = StrataError::new(Code::Unknown, "move failed after writing began")
        .with_detail(serde_json::json!({ "phase": "prepare", "cause": serde_json::to_value(&gitio).unwrap() }));
    assert_eq!(attach_outside_reason(&direct), Some("index-read-back-refused"));
    assert_eq!(attach_outside_reason(&wrapped), Some("staging-changed"));
    for e in [&direct, &wrapped] {
        assert!(alert_attach(&core.warnings, &main, e));
        assert!(alert_attach(&core.warnings, &main, e), "still recognised");
    }
    // Other reasons, or not Unknown: no alert.
    let other = StrataError::new(Code::Unknown, "x").with_detail(serde_json::json!({ "reason": "staging_changed" }));
    let typed = StrataError::new(Code::Exists, "x").with_detail(serde_json::json!({ "reason": "staging-changed" }));
    assert!(!alert_attach(&core.warnings, &main, &other));
    assert!(!alert_attach(&core.warnings, &main, &typed));

    let w = core.warnings.snapshot();
    for r in ["index-read-back-refused", "staging-changed"] {
        assert!(w.contains(&DaemonWarning::AttachOutside { reason: r.into(), index: index.clone() }), "{w:?}");
    }
    let alerts: Vec<String> = fx.alerts().into_iter().filter(|l| l.contains("outside its staging")).collect();
    assert_eq!(alerts.len(), 2, "one line per reason, not repeated: {alerts:?}");
    for l in &alerts {
        assert!(l.contains(&index.display().to_string()) && l.contains("git status") && l.contains("git diff --cached"), "{l}");
    }
    let after = (fs::read(&index).unwrap(), fs::metadata(&index).unwrap().modified().unwrap());
    assert_eq!(before, after, "the main checkout's index is not touched");
    core.close().unwrap();
}

#[test]
fn pre_push_guard_disabled_is_reported_not_repinned() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let rn = repo_name("tool");
    {
        let d = Daemon::start(&core).unwrap();
        let id = d.sessions.new(None, std::slice::from_ref(&rn)).unwrap().manifest.id;
        let shadow = shadow_dir(&fx.config.paths.root, &id, &rn);
        let hook = shadow.join("hooks/pre-push");
        assert!(hook.is_file(), "{}", hook.display());
        fs::write(&hook, b"#!/bin/sh\nexit 0\n").unwrap();
        let config_before = fs::read(shadow.join("config")).unwrap();

        let st = d.sessions.status(&id).unwrap();
        let ws = &st.workspaces[&rn];
        assert!(
            ws.warnings.iter().any(|w| matches!(w, strata_session::StatusWarning::MirrorDrift { items } if items.iter().any(|i| i.reason == "pre_push_guard_disabled"))),
            "{:?}",
            ws.warnings
        );
        core.gits[&rn].take_git_calls();
        assert_eq!(report_guard_disabled(&core.warnings, &st, &fx.config.paths.root), vec![shadow.clone()]);
        let st = d.sessions.status(&id).unwrap();
        assert!(report_guard_disabled(&core.warnings, &st, &fx.config.paths.root).is_empty(), "first time only");
        let lines: Vec<String> = fx.alerts().into_iter().filter(|l| l.contains("pre-push guard disabled")).collect();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains(&shadow.display().to_string()));

        // Reported only: the shadow config and the replaced hook stay.
        assert_eq!(fs::read(shadow.join("config")).unwrap(), config_before);
        assert_eq!(fs::read(&hook).unwrap(), b"#!/bin/sh\nexit 0\n");
        for c in core.gits[&rn].take_git_calls() {
            let writes_config = c.args.iter().any(|a| a == "config")
                && !c.args.iter().any(|a| a == "--get" || a == "--get-all" || a == "--list" || a == "-l");
            assert!(!writes_config, "{:?}", c.args);
        }
    }
    core.close().unwrap();
}
