//! cli.md §8 Task 1: the envelope and stdout write failures.

mod support;

use std::fmt;
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::Stdio;

use git_strata::envelope::{Envelope, emit};
use serde_json::{Value, json};
use strata_types::{Code, StrataError};
use support::json::{J, parse};
use support::live::{Live, write};
use support::{Home, code_names};

/// The three keys, in order, compact (no whitespace outside strings).
fn check_shape(line: &str) -> J {
    assert!(line.ends_with('\n'), "{line:?}");
    let body = line.strip_suffix('\n').unwrap();
    assert!(!body.contains('\n'), "{line:?}");
    let j = parse(body).unwrap_or_else(|e| panic!("independent parser: {e}: {body}"));
    assert_eq!(j.keys(), vec!["ok", "data", "error"], "{body}");
    assert_ne!(j.get("data"), Some(&J::Null), "{body}");
    j
}

#[test]
fn every_command_emits_ok_envelope() {
    // Library level: a null result is {}.
    let e = Envelope::ok(Value::Null);
    let j = check_shape(&e.line());
    assert_eq!(j.get("ok"), Some(&J::Bool(true)));
    assert_eq!(j.get("data"), Some(&J::Obj(vec![])));
    assert_eq!(j.get("error"), Some(&J::Null));
    // The binary, with no daemon and no config.
    let h = Home::new();
    let r = h.run(&["daemon", "status"]);
    assert_eq!(r.code, 0, "{r:?}");
    let j = check_shape(&r.stdout);
    assert_eq!(j.get("ok"), Some(&J::Bool(true)));
    assert_eq!(j.get("error"), Some(&J::Null));
    assert!(matches!(j.get("data"), Some(J::Obj(_))));
    every_command_against_a_live_daemon();
}

/// Each subcommand that a non-TTY can complete, against an in-process
/// daemon. (The confirm commands are NotTty here: tests/confirm_gate.rs;
/// `assign` needs a publish conflict; `resolve`/`chain` need a TTY.)
fn every_command_against_a_live_daemon() {
    let lv = Live::new(&["tool"], &[("a.txt", b"alpha\n")]);
    let ok = |r: support::Run| -> J {
        assert_eq!(r.code, 0, "{r:?}");
        let j = check_shape(&r.stdout);
        assert_eq!(j.get("ok"), Some(&J::Bool(true)), "{r:?}");
        assert_eq!(j.get("error"), Some(&J::Null));
        j.get("data").unwrap().clone()
    };
    lv.serve(|_| {
        let d = ok(lv.run(&["new", "--ticket", "T-1", "--repos", "tool"]));
        let s = d.get("manifest").and_then(|m| m.get("id")).and_then(J::str).unwrap().to_string();
        let ws = lv.ws(&strata_types::SessionId::try_from(s.clone()).unwrap(), "tool");
        write(&ws, "b.txt", b"beta\n");
        ok(lv.run_in(&ws, &["status"]));
        ok(lv.run(&["status", "--all"]));
        let cp = ok(lv.run_in(&ws, &["checkpoint", "-m", "m"]));
        let id = match cp.get("checkpoints") {
            Some(J::Arr(v)) => match v[0].get("id") {
                Some(J::Num(n)) => n.clone(),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        };
        ok(lv.run_in(&ws, &["grep", "beta"]));
        ok(lv.run_in(&ws, &["files"]));
        ok(lv.run_in(&ws, &["diff"]));
        ok(lv.run(&["diff", "--between", &s, &s, "--repo", "tool"]));
        assert_eq!(ok(lv.run_in(&ws, &["deps", "propose"])), J::Obj(vec![]));
        ok(lv.run(&["deps", "list"]));
        ok(lv.run(&["daemon", "status"]));
        ok(lv.run_in(&ws, &["restore", &id]));
        let p = ok(lv.run(&["publish", "--from", &format!("{s}:b.txt")]));
        assert!(p.get("commits").is_some(), "{p:?}");
        assert_eq!(ok(lv.run(&["gc"])), J::Obj(vec![]));
        ok(lv.run(&["rm", "--session", &s]));
    });
}

#[test]
fn envelope_keys_always_present() {
    let e = Envelope::err(StrataError::new(Code::Busy, "x"));
    let j = check_shape(&e.line());
    assert_eq!(j.get("ok"), Some(&J::Bool(false)));
    assert!(matches!(j.get("error"), Some(J::Obj(_))));
    assert_eq!(j.get("data"), Some(&J::Obj(vec![])));
    // A failure with a result keeps it (a push timeout after commit_tree).
    let data = json!({ "commits": { "tool": "abc" }, "pushes": [{ "repo": "tool", "outcome": { "kind": "Unknown" } }] });
    let e = Envelope::err_with(StrataError::new(Code::Unknown, "push timed out"), data.clone());
    let v: Value = serde_json::from_str(e.line().trim_end()).unwrap();
    assert_eq!(v["data"], data);
    assert_eq!(v["error"]["code"], "Unknown");
    assert!(v["data"]["pushes"].is_array());
    // The binary's failure envelope.
    let h = Home::new();
    let r = h.run(&["no-such-command"]);
    assert_eq!(r.code, 1, "{r:?}");
    let j = check_shape(&r.stdout);
    assert_eq!(j.get("ok"), Some(&J::Bool(false)));
    let err = j.get("error").unwrap();
    assert_eq!(err.get("code").and_then(J::str), Some("InvalidInput"));
    assert!(err.get("message").is_some());
}

#[test]
fn json_escaping_round_trips() {
    let nasty = "q\"uo\\te\ttab\nnl\r\u{1}\u{1f}\u{7f} é 漢字 😀 \u{2028}";
    let e = Envelope::ok(json!({ "path": nasty, "paths": [nasty, "plain"] }));
    let j = check_shape(&e.line());
    let d = j.get("data").unwrap();
    assert_eq!(d.get("path").and_then(J::str), Some(nasty));
    assert_eq!(d.get("paths"), Some(&J::Arr(vec![J::Str(nasty.into()), J::Str("plain".into())])));
    let e = Envelope::err(StrataError::new(Code::InvalidInput, nasty).with_detail(json!({ "path": nasty })));
    let j = check_shape(&e.line());
    let err = j.get("error").unwrap();
    assert_eq!(err.get("message").and_then(J::str), Some(nasty));
    assert_eq!(err.get("detail").and_then(|d| d.get("path")).and_then(J::str), Some(nasty));
}

fn closed_pipe_writer() -> OwnedFd {
    let mut fds = [0; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    unsafe { libc::close(fds[0]) };
    unsafe { OwnedFd::from_raw_fd(fds[1]) }
}

#[test]
fn stdout_write_failure_is_nonzero() {
    let h = Home::new();
    let out = h.cmd(&["daemon", "status"]).stdout(Stdio::from(closed_pipe_writer())).stderr(Stdio::piped()).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("stdout_write_failed"), "{err}");
    assert!(!err.contains("panicked"), "{err}");
}

struct FailFmt;
impl Write for FailFmt {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Ok(0)
    }
    fn write_fmt(&mut self, _: fmt::Arguments<'_>) -> io::Result<()> {
        Err(io::Error::other("format write failed"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn stdout_format_write_failure_is_nonzero() {
    let e = Envelope::ok(json!({}));
    let r = emit(&e, &mut FailFmt);
    assert!(r.is_err());
    assert_eq!(e.exit_code(&r), 1);
    // The binary with a stdout whose writes fail (EFBIG: a regular file
    // under RLIMIT_FSIZE = 0, SIGXFSZ ignored). Rust's stdout treats EBADF
    // as success, so a read-only fd would not exercise this path.
    let h = Home::new();
    let file = File::create(h.path().join("out")).unwrap();
    let mut cmd = h.cmd(&["daemon", "status"]);
    unsafe {
        cmd.pre_exec(|| {
            libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
            let lim = libc::rlimit { rlim_cur: 0, rlim_max: libc::RLIM_INFINITY };
            if libc::setrlimit(libc::RLIMIT_FSIZE, &lim) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let out = cmd.stdout(Stdio::from(file)).stderr(Stdio::piped()).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("stdout_write_failed"), "{err}");
    assert!(!err.contains("panicked"), "{err}");
}

struct FailFlush(Vec<u8>);
impl Write for FailFlush {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("flush failed"))
    }
}

#[test]
fn stdout_flush_failure_is_nonzero() {
    let e = Envelope::ok(json!({ "a": 1 }));
    let mut w = FailFlush(Vec::new());
    let r = emit(&e, &mut w);
    assert!(r.is_err(), "the explicit flush failure must surface");
    assert_eq!(e.exit_code(&r), 1);
    assert_eq!(String::from_utf8(w.0).unwrap(), e.line());
}

#[test]
fn error_codes_match_types_md() {
    let names = code_names();
    assert_eq!(names.len(), 39);
    assert!(!names.iter().any(|n| n == "UnknownMethod" || n == "Failed"));
    for c in Code::ALL {
        let e = Envelope::err(StrataError::new(*c, "m"));
        let v: Value = serde_json::from_str(e.line().trim_end()).unwrap();
        assert_eq!(v["error"]["code"], c.as_str());
    }
    let h = Home::new();
    for args in [vec!["no-such-command"], vec!["deps"], vec!["status"], vec!["checkpoint", "--bogus"]] {
        let r = h.run(&args);
        let code = r.err_code();
        assert!(names.contains(&code), "{args:?}: {code} is not a types.md Code");
    }
}
