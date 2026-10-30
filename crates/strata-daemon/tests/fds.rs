//! Incident 2026-10-27: git-stratad ran out of descriptors (EMFILE) and its
//! accept loop retried in silence while clients hung. These tests change
//! process-wide state (RLIMIT_NOFILE, the descriptor table), so they live in
//! their own binary and run one at a time.

mod support;

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use strata_daemon::handlers;
use strata_daemon::startup::{self, Daemon};
use strata_types::SessionId;
use support::*;

static SERIAL: Mutex<()> = Mutex::new(());
fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

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
    s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    let line = json!({ "id": 7, "method": method, "params": params }).to_string();
    s.write_all(line.as_bytes()).unwrap();
    s.write_all(b"\n").unwrap();
    let mut r = BufReader::new(s);
    let mut out = String::new();
    r.read_line(&mut out).unwrap();
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("reply {out:?}: {e}"))
}

fn ok(v: Value) -> Value {
    assert_eq!(v["ok"], json!(true), "{v:#}");
    v["data"].clone()
}

fn fds() -> usize {
    startup::open_fd_count().unwrap()
}

/// Wait for background work (connection threads, reconcile, git children)
/// to settle, then the lowest count seen.
fn settled_fds() -> usize {
    let t0 = Instant::now();
    let mut low = fds();
    while t0.elapsed() < Duration::from_millis(1500) {
        std::thread::sleep(Duration::from_millis(50));
        low = low.min(fds());
    }
    low
}

fn rlimit() -> libc::rlimit {
    let mut r = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut r) }, 0);
    r
}

fn set_soft(n: u64) {
    let mut r = rlimit();
    r.rlim_cur = n;
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &r) }, 0, "{}", std::io::Error::last_os_error());
}

/// The descriptor count stays flat across many requests on a session with a
/// live watcher: status, files, grep, checkpoint and daemon_status.
#[test]
fn descriptor_count_is_stable_across_requests() {
    let _g = serial();
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    serving(&d, || {
        let s = ok(call(&sock, "new", json!({ "ticket": null, "repos": ["tool"] })));
        let id: SessionId = serde_json::from_value(s["session"]["manifest"]["id"].clone()).unwrap();
        let round = || {
            ok(call(&sock, "status", json!({ "cwd": null, "session": id.as_str(), "all": false })));
            ok(call(&sock, "files", json!({ "session": id, "repo": "tool", "glob": "*" })));
            ok(call(&sock, "grep", json!({ "pattern": "needle", "session": id, "all": false, "ignore_case": false })));
            ok(call(&sock, "daemon_status", json!({})));
        };
        for _ in 0..5 {
            round();
        }
        let before = settled_fds();
        for _ in 0..60 {
            round();
        }
        let after = settled_fds();
        assert!(after <= before + 2, "descriptors grew from {before} to {after} over 60 rounds");
        let st = ok(call(&sock, "daemon_status", json!({})));
        assert!(st["fds"]["open"].as_u64().unwrap() > 0, "{st:#}");
    });
    drop(d);
    core.close().unwrap();
}

/// Out of descriptors, a client is answered `Busy` (too_many_open_files)
/// and closed instead of hanging in the backlog; once descriptors free up
/// the daemon serves again.
#[test]
fn out_of_descriptors_the_daemon_sheds_with_busy_and_recovers() {
    let _g = serial();
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    let saved = rlimit();
    serving(&d, || {
        ok(call(&sock, "daemon_status", json!({})));
        set_soft((fds() + 64) as u64);
        let mut fill: Vec<File> = Vec::new();
        let fill_up = |fill: &mut Vec<File>| {
            while let Ok(f) = File::open("/dev/null") {
                fill.push(f);
            }
        };
        fill_up(&mut fill);
        // Free slots one at a time until the client socket gets one, then
        // take every other slot: the daemon's accept fails EMFILE. macOS
        // closes that client; it must not hang.
        let first = loop {
            fill.pop().expect("ran out of fillers");
            if let Ok(c) = UnixStream::connect(&sock) {
                break c;
            }
        };
        fill_up(&mut fill);
        let read = |c: &UnixStream| {
            c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            let mut out = String::new();
            BufReader::new(c).read_line(&mut out).expect("the client is answered or closed, not left hanging");
            out
        };
        let out = read(&first);
        drop(first);
        assert!(out.is_empty() || out.contains("too_many_open_files"), "{out}");
        // The daemon has let go of its reserve descriptor. A client that
        // gets in now is answered Busy while descriptors are still short.
        let mut busy = None;
        for _ in 0..5 {
            let c = loop {
                fill.pop().expect("ran out of fillers");
                if let Ok(c) = UnixStream::connect(&sock) {
                    break c;
                }
            };
            let out = read(&c);
            if let Ok(v) = serde_json::from_str::<Value>(&out) {
                busy = Some(v);
                break;
            }
        }
        drop(fill);
        unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &saved) };
        let v = busy.expect("no Busy reply in 5 attempts");
        assert_eq!(v["error"]["detail"]["reason"], "too_many_open_files", "{v:#}");
        assert_eq!(v["ok"], json!(false), "{v:#}");
        assert_eq!(v["error"]["code"], "Busy", "{v:#}");
        // Recovered.
        let st = ok(call(&sock, "daemon_status", json!({})));
        assert!(st["fds"]["limit"].as_u64().unwrap() >= 64, "{st:#}");
        assert!(st["fds"]["accept_errors"].as_u64().unwrap() >= 1, "{st:#}");
    });
    unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &saved) };
    drop(d);
    core.close().unwrap();
}

/// `raise_fd_limit` lifts launchd's 256 to min(FD_LIMIT, hard) and never lowers.
#[test]
fn raise_fd_limit_lifts_the_soft_limit() {
    let _g = serial();
    let saved = rlimit();
    set_soft(256);
    let (before, after) = startup::raise_fd_limit().unwrap();
    assert_eq!(before, 256);
    assert_eq!(after, startup::FD_LIMIT.min(saved.rlim_max));
    assert_eq!(rlimit().rlim_cur, after);
    let (b2, a2) = startup::raise_fd_limit().unwrap();
    assert_eq!((b2, a2), (after, after), "a second call changes nothing");
    unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &saved) };
}
