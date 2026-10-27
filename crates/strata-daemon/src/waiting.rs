//! §5.1a: waiting for the store instead of exiting.
//!
//! When `prepare` fails with an I/O error (EINTR while macOS TCC decides on
//! a removable volume, EPERM/EACCES before consent, a volume not mounted
//! yet), exiting would let launchd's `KeepAlive` restart the daemon every
//! ten seconds, each start asking TCC again. Instead `main` keeps the
//! instance lock, binds the socket, answers every request with
//! `Busy` / `store_unavailable` (so `daemon status` says why at once), and
//! retries with backoff (1 s doubling to 60 s). The error is logged once,
//! again only when it changes, and as a reminder every ten minutes.

use std::io::{self, BufReader};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use strata_types::{Code, StrataError};

use crate::rpc::{self, Reply, Request};
use crate::{MAX_FRAME, Result};

/// The waiting knobs; [`Policy::PRODUCTION`] in `main`, short ones in tests.
#[derive(Clone, Copy, Debug)]
pub struct Policy {
    /// The first wait after a failure.
    pub first: Duration,
    /// The longest wait between attempts.
    pub cap: Duration,
    /// An unchanged error is logged again after this long.
    pub remind: Duration,
    /// How often a wait checks `stop`.
    pub tick: Duration,
}

impl Policy {
    pub const PRODUCTION: Policy = Policy {
        first: Duration::from_secs(1),
        cap: Duration::from_secs(60),
        remind: Duration::from_secs(600),
        tick: Duration::from_millis(100),
    };
}

/// Exponential backoff: `first`, doubling, capped at `cap`.
#[derive(Debug)]
pub struct Backoff {
    next: Duration,
    cap: Duration,
}

impl Backoff {
    pub fn new(first: Duration, cap: Duration) -> Backoff {
        Backoff { next: first.min(cap), cap }
    }

    /// The wait before the next attempt; the one after doubles (to `cap`).
    pub fn next_delay(&mut self) -> Duration {
        let d = self.next;
        self.next = (self.next * 2).min(self.cap);
        d
    }
}

/// Decides whether an error line is printed: the first one, a changed one,
/// or an unchanged one `remind` after it was last printed.
#[derive(Debug)]
pub struct ErrorLog {
    remind: Duration,
    last: Option<(String, Instant)>,
}

impl ErrorLog {
    pub fn new(remind: Duration) -> ErrorLog {
        ErrorLog { remind, last: None }
    }

    pub fn should_log(&mut self, key: &str, now: Instant) -> bool {
        let log = match &self.last {
            None => true,
            Some((k, at)) => k != key || now.duration_since(*at) >= self.remind,
        };
        if log {
            self.last = Some((key.to_owned(), now));
        }
        log
    }
}

/// Whether a startup error is worth waiting out: it came from the file
/// system (`errno`, `io_kind` or `io` in its detail) and is not "another
/// daemon is running" or a test-seam refusal.
pub fn retryable(e: &StrataError) -> bool {
    let Some(d) = e.detail.as_ref() else { return false };
    if matches!(d.get("reason").and_then(Value::as_str), Some("daemon_running" | "test_seams" | "no_instance_lock")) {
        return false;
    }
    d.get("errno").is_some() || d.get("io_kind").is_some() || d.get("io").is_some()
}

/// The key two errors are "the same" by: code, message and errno.
fn error_key(e: &StrataError) -> String {
    let errno = e.detail.as_ref().and_then(|d| d.get("errno")).cloned().unwrap_or(Value::Null);
    format!("{}|{}|{}", e.code, e.message, errno)
}

/// Run `attempt` until it succeeds, fails with an error `retryable` rejects
/// (returned as is), or `stop` is true (→ `Ok(None)`). `on_error` sees every
/// retryable failure with its attempt number and the wait that follows.
pub fn wait_until<T>(
    mut attempt: impl FnMut() -> Result<T>,
    policy: Policy,
    stop: &dyn Fn() -> bool,
    log: &dyn Fn(&str),
    on_error: &mut dyn FnMut(&StrataError, u32, Duration),
) -> Result<Option<T>> {
    let mut backoff = Backoff::new(policy.first, policy.cap);
    let mut errors = ErrorLog::new(policy.remind);
    let started = Instant::now();
    let mut n: u32 = 0;
    loop {
        if stop() {
            return Ok(None);
        }
        n += 1;
        match attempt() {
            Ok(v) => {
                if n > 1 {
                    log(&format!("store available after {n} attempts ({}s)", started.elapsed().as_secs()));
                }
                return Ok(Some(v));
            }
            Err(e) if retryable(&e) => {
                let wait = backoff.next_delay();
                if errors.should_log(&error_key(&e), Instant::now()) {
                    log(&format!(
                        "{} (waiting for the store: attempt {n}, retrying in {}s, not exiting)",
                        e.message,
                        wait.as_secs_f32()
                    ));
                }
                on_error(&e, n, wait);
                let until = Instant::now() + wait;
                while Instant::now() < until {
                    if stop() {
                        return Ok(None);
                    }
                    std::thread::sleep(policy.tick.min(until.saturating_duration_since(Instant::now())));
                }
            }
            Err(e) => return Err(e),
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// What the waiting socket answers with; shared with `main`'s loop.
#[derive(Clone)]
pub struct WaitState(Arc<Mutex<Value>>);

impl Default for WaitState {
    fn default() -> Self {
        WaitState(Arc::new(Mutex::new(json!({ "state": "starting", "since": unix_now(), "attempts": 0 }))))
    }
}

impl WaitState {
    /// A failed attempt: `store_unavailable` with the error and the next try.
    pub fn failed(&self, e: &StrataError, attempts: u32, wait: Duration) {
        let mut v = self.0.lock().unwrap_or_else(|p| p.into_inner());
        v["state"] = json!("store_unavailable");
        v["attempts"] = json!(attempts);
        v["last_error"] = serde_json::to_value(e).unwrap_or(Value::Null);
        v["last_error_at"] = json!(unix_now());
        v["next_retry_in_ms"] = json!(wait.as_millis() as u64);
    }

    /// The store opened; the rest of startup (recover, gc) is running.
    pub fn starting(&self) {
        let mut v = self.0.lock().unwrap_or_else(|p| p.into_inner());
        v["state"] = json!("starting");
        if let Some(o) = v.as_object_mut() {
            o.remove("next_retry_in_ms");
        }
    }

    pub fn snapshot(&self) -> Value {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// The error every request gets while waiting.
    pub fn error(&self) -> StrataError {
        let v = self.snapshot();
        let state = v["state"].as_str().unwrap_or("starting").to_owned();
        let message = match v["last_error"]["message"].as_str() {
            Some(m) if state == "store_unavailable" => format!("git-stratad is waiting for its store: {m}"),
            _ => "git-stratad is starting".to_owned(),
        };
        let mut detail = v;
        detail["reason"] = json!(state);
        StrataError::new(Code::Busy, message).with_detail(detail)
    }

    pub fn reply(&self, req: &Request) -> Reply {
        Reply::error(&req.id, &self.error())
    }
}

/// The socket while waiting: every request gets [`WaitState::error`].
/// [`Waiting::finish`] stops accepting and hands the listener back, so the
/// real accept loop takes over the same socket (queued connections included).
pub struct Waiting {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<UnixListener>>,
    pub state: WaitState,
}

impl Waiting {
    pub fn start(listener: UnixListener, state: WaitState) -> io::Result<Waiting> {
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let (st, s2) = (stop.clone(), state.clone());
        let thread = std::thread::Builder::new().name("strata-waiting".into()).spawn(move || {
            while !st.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let s3 = s2.clone();
                        let _ = std::thread::Builder::new().name("strata-waiting-conn".into()).spawn(move || {
                            let Ok(mut w) = stream.try_clone() else { return };
                            // Like the real server: no read timeout; the peer owns the connection.
                            let mut r = BufReader::new(stream);
                            let mut h = |req: Request| s3.reply(&req);
                            let _ = rpc::serve_connection(&mut r, &mut w, MAX_FRAME, &mut h);
                        });
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(20)),
                    Err(_) => std::thread::sleep(Duration::from_millis(20)),
                }
            }
            listener
        })?;
        Ok(Waiting { stop, thread: Some(thread), state })
    }

    /// Stop answering; the listener, back in blocking mode's hands of the caller.
    pub fn finish(mut self) -> Option<UnixListener> {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.take().and_then(|t| t.join().ok())
    }

    /// Stop and remove the socket file (shutdown while still waiting).
    pub fn abandon(self, socket: &Path) {
        drop(self.finish());
        let _ = std::fs::remove_file(socket);
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    fn io_err(errno: i32) -> StrataError {
        strata_apfs::classify_io_error(&io::Error::from_raw_os_error(errno), strata_apfs::IoSide::Read, Path::new("/Volumes/X/.strata"))
    }

    fn quick() -> Policy {
        Policy { first: Duration::from_millis(1), cap: Duration::from_millis(4), remind: Duration::from_secs(600), tick: Duration::from_millis(1) }
    }

    #[test]
    fn backoff_doubles_to_the_cap() {
        let mut b = Backoff::new(Duration::from_secs(1), Duration::from_secs(60));
        let got: Vec<u64> = (0..9).map(|_| b.next_delay().as_secs()).collect();
        assert_eq!(got, vec![1, 2, 4, 8, 16, 32, 60, 60, 60]);
    }

    #[test]
    fn error_log_prints_first_changed_and_reminders_only() {
        let mut l = ErrorLog::new(Duration::from_secs(600));
        let t = Instant::now();
        assert!(l.should_log("a", t));
        assert!(!l.should_log("a", t + Duration::from_secs(10)));
        assert!(!l.should_log("a", t + Duration::from_secs(599)));
        assert!(l.should_log("a", t + Duration::from_secs(600)));
        assert!(l.should_log("b", t + Duration::from_secs(601)));
        assert!(!l.should_log("b", t + Duration::from_secs(602)));
    }

    #[test]
    fn retryable_is_io_errors_but_not_another_daemon() {
        assert!(retryable(&io_err(libc::EINTR)));
        assert!(retryable(&io_err(libc::EPERM)));
        assert!(retryable(&io_err(libc::EACCES)));
        let running = StrataError::new(Code::Exists, "another git-stratad is running")
            .with_detail(json!({ "reason": "daemon_running", "errno": 35 }));
        assert!(!retryable(&running));
        assert!(!retryable(&StrataError::new(Code::InvalidInput, "bad config")));
        let missing = StrataError::new(Code::InvalidInput, "repo x: /a does not exist")
            .with_detail(json!({ "reason": "missing", "io": "Interrupted system call (os error 4)" }));
        assert!(retryable(&missing));
    }

    #[test]
    fn waits_out_eintr_logging_once_then_succeeds() {
        let lines = RefCell::new(Vec::<String>::new());
        let calls = Cell::new(0);
        let mut seen = Vec::new();
        let r = wait_until(
            || {
                calls.set(calls.get() + 1);
                if calls.get() < 4 { Err(io_err(libc::EINTR)) } else { Ok("core") }
            },
            quick(),
            &|| false,
            &|l| lines.borrow_mut().push(l.to_owned()),
            &mut |_, n, w| seen.push((n, w.as_millis())),
        )
        .unwrap();
        assert_eq!(r, Some("core"));
        assert_eq!(calls.get(), 4);
        assert_eq!(seen, vec![(1, 1), (2, 2), (3, 4)]);
        let lines = lines.into_inner();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].contains("Interrupted system call") && lines[0].contains("not exiting"), "{lines:?}");
        assert!(lines[1].contains("after 4 attempts"), "{lines:?}");
    }

    #[test]
    fn a_changed_error_is_logged_again() {
        let lines = RefCell::new(Vec::<String>::new());
        let calls = Cell::new(0);
        let _ = wait_until(
            || {
                calls.set(calls.get() + 1);
                match calls.get() {
                    1 | 2 => Err(io_err(libc::EINTR)),
                    3 => Err(io_err(libc::EPERM)),
                    _ => Ok(()),
                }
            },
            quick(),
            &|| false,
            &|l| lines.borrow_mut().push(l.to_owned()),
            &mut |_, _, _| {},
        );
        let lines = lines.into_inner();
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[1].contains("Operation not permitted"), "{lines:?}");
    }

    #[test]
    fn a_non_retryable_error_returns_at_once() {
        let calls = Cell::new(0);
        let r: Result<Option<()>> = wait_until(
            || {
                calls.set(calls.get() + 1);
                Err(StrataError::new(Code::InvalidInput, "bad config"))
            },
            quick(),
            &|| false,
            &|_| {},
            &mut |_, _, _| {},
        );
        assert_eq!(r.unwrap_err().message, "bad config");
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn stop_ends_the_wait() {
        let calls = Cell::new(0);
        let p = Policy { first: Duration::from_secs(3600), ..quick() };
        let r: Result<Option<()>> = wait_until(
            || {
                calls.set(calls.get() + 1);
                Err(io_err(libc::EINTR))
            },
            p,
            &|| calls.get() >= 1,
            &|_| {},
            &mut |_, _, _| {},
        );
        assert_eq!(r.unwrap(), None);
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn the_waiting_socket_answers_store_unavailable_and_hands_the_listener_back() {
        let dir = tempfile::Builder::new().prefix("sw").tempdir_in("/tmp").unwrap();
        let sock = dir.path().join("d.sock");
        let state = WaitState::default();
        state.failed(&io_err(libc::EINTR), 3, Duration::from_secs(4));
        let w = Waiting::start(UnixListener::bind(&sock).unwrap(), state.clone()).unwrap();
        let e = crate::client::call(&sock, "daemon_status", json!({ "verify": false })).unwrap_err();
        assert_eq!(e.code, Code::Busy, "{e:?}");
        let d = e.detail.unwrap();
        assert_eq!(d["reason"], "store_unavailable", "{d}");
        assert_eq!(d["attempts"], 3, "{d}");
        assert_eq!(d["last_error"]["detail"]["errno"], libc::EINTR, "{d}");
        assert!(e.message.contains("waiting for its store"), "{}", e.message);
        state.starting();
        let e = crate::client::call(&sock, "daemon_status", json!({ "verify": false })).unwrap_err();
        assert_eq!(e.detail.unwrap()["reason"], "starting");
        let l = w.finish().expect("listener back");
        assert!(std::os::unix::net::UnixStream::connect(&sock).is_ok());
        drop(l);
    }
}
