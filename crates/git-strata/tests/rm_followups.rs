//! rm-followups R2/R3 end to end (the real `git-strata` binary against the
//! in-process daemon, or the store alone): Ctrl-C during `rm`, the last
//! refused rm in `status`/`ls`/`--brief`, and removed sessions' leftover
//! directories.

mod support;

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use support::Run;
use support::live::{Live, write};

fn run_env(lv: &Live, args: &[&str], k: &str, v: &str) -> Run {
    Run::from(lv.home.cmd(args).env(k, v).output().expect("spawn git-strata"))
}

fn rows(store_root: &std::path::Path, s: &str) -> Option<strata_store::Lifecycle> {
    let store = strata_store::Store::open_read_only(store_root).unwrap();
    let id = strata_types::SessionId::try_from(s.to_string()).unwrap();
    store.session(&id).unwrap().map(|r| r.lifecycle)
}

#[test]
fn rm_cli_sigint_before_send_says_not_sent() {
    let lv = Live::new(&["tool"], &[("a.txt", b"alpha\n")]);
    lv.serve(|_| {
        let s = lv.new_session("T-1", "tool");
        lv.requests.lock().unwrap().clear();
        let r = run_env(&lv, &["rm", "--session", s.as_str()], "GIT_STRATA_TEST_INTERRUPT", "before_newline");
        assert_eq!(r.code, 130, "{r:?}");
        let e = r.env();
        assert_eq!(e["ok"], false, "{r:?}");
        assert_eq!(e["error"]["code"], "NotConfirmed", "{r:?}");
        assert_eq!(e["error"]["detail"]["reason"], "interrupted_not_sent");
        assert_eq!(e["error"]["detail"]["sent"], false);
        assert_eq!(e["error"]["detail"]["exit"], 130);
        assert!(r.stderr.contains(&format!("rm of {s} was not sent; nothing happened")), "{r:?}");
        // The partial line never reached a handler; the session stays.
        std::thread::sleep(Duration::from_millis(200));
        assert!(!lv.requests.lock().unwrap().iter().any(|m| m == "rm"), "{:?}", lv.requests.lock().unwrap());
        assert_eq!(rows(&lv.root, s.as_str()), Some(strata_store::Lifecycle::Active));
        lv.run(&["status", "--session", s.as_str()]).ok();
    });
}

#[test]
fn rm_cli_sigint_prints_hint_daemon_finishes() {
    let lv = Live::new(&["tool"], &[("a.txt", b"alpha\n")]);
    lv.serve(|_| {
        let s = lv.new_session("T-1", "tool");
        let r = run_env(&lv, &["rm", "--session", s.as_str()], "GIT_STRATA_TEST_INTERRUPT", "after_newline");
        assert_eq!(r.code, 130, "{r:?}");
        let e = r.env();
        assert_eq!(e["error"]["code"], "Unknown", "{r:?}");
        assert_eq!(e["error"]["detail"]["reason"], "interrupted_after_send");
        assert_eq!(e["error"]["detail"]["sent"], true);
        assert!(
            r.stderr.contains(&format!("rm of {s} continues in the daemon"))
                && r.stderr.contains(&format!("`git-strata status {s}`")),
            "{r:?}"
        );
        // The daemon goes on without the CLI and removes the session.
        let t = Instant::now();
        while rows(&lv.root, s.as_str()) != Some(strata_store::Lifecycle::Removed) {
            assert!(t.elapsed() < Duration::from_secs(20), "rm never finished: {:?}", rows(&lv.root, s.as_str()));
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(lv.run(&["status", "--session", s.as_str()]).err_code(), "NoSuchSession");
    });
}

#[test]
fn rm_cli_sigterm_after_send_exits_143() {
    let lv = Live::new(&["tool"], &[("a.txt", b"alpha\n")]);
    lv.serve(|_| {
        let s = lv.new_session("T-1", "tool");
        let r = run_env(&lv, &["rm", "--session", s.as_str()], "GIT_STRATA_TEST_INTERRUPT", "after_newline:term");
        assert_eq!(r.code, 143, "{r:?}");
        let e = r.env();
        assert_eq!(e["error"]["code"], "Unknown", "{r:?}");
        assert_eq!(e["error"]["detail"]["reason"], "interrupted_after_send");
        assert_eq!(e["error"]["detail"]["exit"], 143);
        let t = Instant::now();
        while rows(&lv.root, s.as_str()) != Some(strata_store::Lifecycle::Removed) {
            assert!(t.elapsed() < Duration::from_secs(20), "rm never finished");
            std::thread::sleep(Duration::from_millis(50));
        }
    });
}

#[test]
fn rm_cli_signal_after_reply_still_exits_by_signal() {
    // The reply is in, then the signal lands before the CLI returns: the
    // exit code still says the user stopped it, and the hint still points
    // at `status` (the rm itself finished).
    let lv = Live::new(&["tool"], &[("a.txt", b"alpha\n")]);
    lv.serve(|_| {
        let s = lv.new_session("T-1", "tool");
        let r = run_env(&lv, &["rm", "--session", s.as_str()], "GIT_STRATA_TEST_INTERRUPT", "after_reply");
        assert_eq!(r.code, 130, "{r:?}");
        assert_eq!(r.env()["error"]["detail"]["reason"], "interrupted_after_send", "{r:?}");
        assert!(r.stderr.contains(&format!("`git-strata status {s}`")), "{r:?}");
        assert_eq!(rows(&lv.root, s.as_str()), Some(strata_store::Lifecycle::Removed));
    });
}

#[test]
fn rm_cli_signal_while_newline_write_fails_says_not_sent() {
    // The newline never went out: not sent, whatever the signal.
    for (point, exit) in [("newline_fails", 130), ("newline_fails:term", 143)] {
        let lv = Live::new(&["tool"], &[("a.txt", b"alpha\n")]);
        lv.serve(|_| {
            let s = lv.new_session("T-1", "tool");
            lv.requests.lock().unwrap().clear();
            let r = run_env(&lv, &["rm", "--session", s.as_str()], "GIT_STRATA_TEST_INTERRUPT", point);
            assert_eq!(r.code, exit, "{point} {r:?}");
            let e = r.env();
            assert_eq!(e["error"]["code"], "NotConfirmed", "{point} {r:?}");
            assert_eq!(e["error"]["detail"]["sent"], false);
            assert_eq!(e["error"]["detail"]["exit"], exit);
            std::thread::sleep(Duration::from_millis(200));
            assert!(!lv.requests.lock().unwrap().iter().any(|m| m == "rm"));
            assert_eq!(rows(&lv.root, s.as_str()), Some(strata_store::Lifecycle::Active));
        });
    }
}

#[test]
fn rm_cli_signal_after_the_last_check_still_sets_the_exit_code() {
    // The reply was taken as the answer; a signal while it is parsed or
    // printed still exits 128 + signal, with the daemon's true envelope.
    let lv = Live::new(&["tool"], &[("a.txt", b"alpha\n")]);
    lv.serve(|_| {
        let s = lv.new_session("T-1", "tool");
        let r = run_env(&lv, &["rm", "--session", s.as_str()], "GIT_STRATA_TEST_INTERRUPT", "after_check:term");
        assert_eq!(r.code, 143, "{r:?}");
        assert_eq!(r.env()["ok"], true, "{r:?}");
        assert_eq!(rows(&lv.root, s.as_str()), Some(strata_store::Lifecycle::Removed));
    });
}

#[test]
fn rm_attempt_recorded_and_shown_in_status() {
    let lv = Live::new(&["tool"], &[("a.txt", b"alpha\n")]);
    lv.serve(|_| {
        let s = lv.new_session("T-1", "tool");
        let other = lv.new_session("T-2", "tool");
        write(&lv.ws(&s, "tool"), "dirty.txt", b"unsaved\n");
        let r = lv.run(&["rm", "--session", s.as_str()]);
        assert_eq!(r.err_code(), "Dirty", "{r:?}");
        let msg = r.env()["error"]["message"].as_str().unwrap().to_string();
        assert!(msg.contains("nothing was removed"), "{msg}");
        // status: the last rm, as data and as a note.
        let st = lv.run(&["status", "--session", s.as_str()]);
        let d = st.ok();
        assert_eq!(d["state"], "Active", "{d:#}");
        assert_eq!(d["last_rm"]["outcome"], "refused", "{d:#}");
        assert_eq!(d["last_rm"]["code"], "Dirty", "{d:#}");
        let notes: Vec<&str> = d["notes"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        assert!(notes.iter().any(|n| n.contains("Dirty")), "{notes:?}");
        assert!(st.stderr.contains(&format!("note: {s}: ")), "{st:?}");
        // Still listed everywhere, with the same note; the other has none.
        let l = lv.run(&["ls"]);
        let ld = l.ok();
        let mine = ld["sessions"].as_array().unwrap().iter().find(|x| x["id"] == json!(s.as_str())).expect("listed").clone();
        assert_eq!(mine["state"], "Active", "{ld:#}");
        assert_eq!(mine["last_rm"]["outcome"], "refused");
        let theirs = ld["sessions"].as_array().unwrap().iter().find(|x| x["id"] == json!(other.as_str())).unwrap().clone();
        assert!(theirs.get("last_rm").is_none() && theirs.get("notes").is_none(), "{theirs:#}");
        assert!(ld.get("leftovers").is_none(), "{ld:#}");
        assert!(l.stderr.contains(&format!("note: {s}: ")), "{l:?}");
        let all = lv.run(&["status", "--all"]).ok();
        assert!(all["sessions"].as_array().unwrap().iter().any(|x| x["manifest"]["id"] == json!(s.as_str()) && x["last_rm"]["outcome"] == "refused"));
        let br = lv.run(&["status", "--brief", "--all"]).ok();
        assert!(br["sessions"].as_array().unwrap().iter().any(|x| x["id"] == json!(s.as_str()) && x["last_rm"]["outcome"] == "refused"), "{br:#}");
        let t = lv.run(&["ls", "--table"]);
        assert_eq!(t.code, 0, "{t:?}");
        assert!(t.stdout.lines().any(|x| x.starts_with(&format!("note: {s}: "))), "{t:?}");
    });
    // The same from the store alone.
    let st = lv.run(&["ls"]).ok();
    assert_eq!(st["source"], "local");
    assert!(st["sessions"].as_array().unwrap().iter().any(|x| x["last_rm"]["outcome"] == "refused"), "{st:#}");
}

/// A removed session whose `sessions/<sid>` is still there.
fn removed_with_leftover(lv: &Live) -> (String, std::path::PathBuf) {
    let s = lv.new_session("T-9", "tool");
    lv.run(&["rm", "--session", s.as_str()]).ok();
    assert_eq!(rows(&lv.root, s.as_str()), Some(strata_store::Lifecycle::Removed));
    let dir = lv.root.join("sessions").join(s.as_str());
    std::fs::create_dir_all(dir.join("tool")).unwrap();
    (s.as_str().to_string(), dir)
}

fn assert_leftover_everywhere(lv: &Live, s: &str, dir: &std::path::Path) {
    let line = format!("leftover of removed session {s}: ");
    let has = |v: &Value| {
        v["leftovers"]
            .as_array()
            .is_some_and(|a| a.iter().any(|l| l["kind"] == "RemovedSession" && l["session"] == json!(s)))
    };
    let l = lv.run(&["ls"]);
    let d = l.ok();
    assert!(has(&d), "{d:#}");
    assert!(l.stderr.contains(&line) && l.stderr.contains("moved to trash by the next gc or daemon start"), "{l:?}");
    let b = lv.run(&["status", "--brief", "--all"]);
    assert!(has(&b.ok()), "{b:?}");
    assert!(b.stderr.contains(&line), "{b:?}");
    let st = lv.run(&["status", "--all"]);
    st.ok();
    assert!(st.stderr.contains(&line), "{st:?}");
    for args in [vec!["status", "--session", s], vec!["status", "--brief", "--session", s]] {
        let r = lv.run(&args);
        assert_eq!(r.err_code(), "NoSuchSession", "{args:?} {r:?}");
        let e = r.env()["error"].clone();
        assert_eq!(e["detail"]["session"], json!(s), "{args:?} {e:#}");
        let paths: Vec<&str> = e["detail"]["leftover"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        assert!(paths.iter().any(|p| p.ends_with(&format!("sessions/{s}"))), "{paths:?} {}", dir.display());
        assert!(e["message"].as_str().unwrap().contains(&line), "{args:?} {e:#}");
    }
}

#[test]
fn ls_and_brief_print_removed_leftovers() {
    let lv = Live::new(&["tool"], &[("a.txt", b"alpha\n")]);
    lv.serve(|_| {
        let (s, dir) = removed_with_leftover(&lv);
        assert_leftover_everywhere(&lv, &s, &dir);
    });
}

#[test]
fn brief_single_session_lists_unrelated_leftovers() {
    // `status --brief` for one live session (by id or by cwd) still says
    // there is a removed session's directory, like `ls` does.
    let lv = Live::new(&["tool"], &[("a.txt", b"alpha\n")]);
    let check = |s: &str, r: &Run| {
        let d = r.ok();
        assert!(
            d["leftovers"].as_array().is_some_and(|a| a.iter().any(|l| l["session"] == json!(s))),
            "{d:#}"
        );
        assert!(r.stderr.contains(&format!("leftover of removed session {s}: ")), "{r:?}");
    };
    let (gone, live) = lv.serve(|_| {
        let (gone, _) = removed_with_leftover(&lv);
        let live = lv.new_session("T-2", "tool");
        check(&gone, &lv.run(&["status", "--brief", "--session", live.as_str()]));
        check(&gone, &lv.run_in(&lv.ws(&live, "tool"), &["status", "--brief"]));
        (gone, live)
    });
    // Read only, without the daemon, by id and by cwd.
    for r in [lv.run(&["status", "--brief", "--session", live.as_str()]), lv.run_in(&lv.ws(&live, "tool"), &["status", "--brief"])] {
        assert_eq!(r.ok()["source"], "local", "{r:?}");
        check(&gone, &r);
    }
}

#[test]
fn status_leftovers_daemon_and_ro() {
    let lv = Live::new(&["tool"], &[("a.txt", b"alpha\n")]);
    let (s, dir) = lv.serve(|_| {
        let (s, dir) = removed_with_leftover(&lv);
        let st = lv.run(&["status", "--all"]);
        assert!(st.stderr.contains(&format!("leftover of removed session {s}: ")), "{st:?}");
        (s, dir)
    });
    // No daemon: the store and two read_dirs say the same.
    assert_leftover_everywhere(&lv, &s, &dir);
}

#[test]
fn status_sid_orphan_detail() {
    // A directory with no session row at all.
    let lv = Live::new(&["tool"], &[("a.txt", b"alpha\n")]);
    let s = "s-k2bm4yt4";
    let dir = lv.root.join("sessions/.repos").join(s);
    lv.serve(|_| {
        std::fs::create_dir_all(&dir).unwrap();
        let r = lv.run(&["status", "--session", s]);
        assert_eq!(r.err_code(), "NoSuchSession", "{r:?}");
        let e = r.env()["error"].clone();
        assert_eq!(e["detail"]["leftover"][0].as_str().map(|p| p.ends_with(&format!("sessions/.repos/{s}"))), Some(true), "{e:#}");
    });
    let r = lv.run(&["status", "--session", s]);
    assert_eq!(r.err_code(), "NoSuchSession", "{r:?}");
    assert_eq!(r.env()["error"]["detail"]["session"], json!(s));
}

#[test]
fn brief_sid_orphan_detail() {
    let lv = Live::new(&["tool"], &[("a.txt", b"alpha\n")]);
    let s = "s-k2bm4yt4";
    lv.serve(|_| {
        std::fs::create_dir_all(lv.root.join("sessions").join(s)).unwrap();
        let r = lv.run(&["status", "--brief", "--session", s]);
        assert_eq!(r.err_code(), "NoSuchSession", "{r:?}");
        assert_eq!(r.env()["error"]["detail"]["session"], json!(s));
    });
    let r = lv.run(&["status", "--brief", "--session", s]);
    assert_eq!(r.err_code(), "NoSuchSession", "{r:?}");
    assert!(r.env()["error"]["message"].as_str().unwrap().contains("leftover of removed session"), "{r:?}");
}
