//! The json-lines client (cli.md §3.4). The plain transport lives in
//! `strata-client` (gui.md §3.1 P0) and is re-exported here; the
//! interruptible call (rm-followups R2) uses signals and stays in the cli.

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use strata_types::{Code, StrataError};

pub use strata_client::*;

/// An interruptible call stopped by its flag (rm-followups R2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stopped {
    /// The request line's newline never went out: the daemon drops a
    /// partial line, so nothing happened.
    NotSent,
    /// The whole line went out: the daemon goes on without us.
    Sent,
}

/// Bytes per write of an interruptible request (the flag is checked
/// between writes).
const CHUNK: usize = 256;
/// How often an interruptible call looks at its flag while it waits.
const POLL: Duration = Duration::from_millis(100);

/// Debug-build test seam: raise SIGINT (or SIGTERM with a `:term`
/// suffix) at a named point of an interruptible call
/// (`GIT_STRATA_TEST_INTERRUPT=before_newline|newline_fails|after_newline|after_reply|after_check[:term]`).
/// True when it fired.
fn test_interrupt(point: &str) -> bool {
    if !cfg!(debug_assertions) {
        return false;
    }
    let Ok(v) = std::env::var("GIT_STRATA_TEST_INTERRUPT") else { return false };
    let (at, sig) = match v.split_once(':') {
        Some((at, "term")) => (at.to_string(), libc::SIGTERM),
        _ => (v.clone(), libc::SIGINT),
    };
    if at == point {
        // SAFETY: raise only delivers a signal to this process.
        unsafe {
            libc::raise(sig);
        }
        return true;
    }
    false
}

/// `call` that gives up when `stop` is set: before the request's final
/// newline is written (`NotSent`), or while waiting for the reply
/// (`Sent`). Writes in small chunks with the newline last, and reads with
/// a short timeout, so a signal handler's flag is seen within `POLL`.
pub fn call_interruptible(
    socket: &Path,
    method: &str,
    params: Value,
    kind: Kind,
    stop: &AtomicBool,
) -> Result<Result<Value, CallError>, Stopped> {
    let Ok(mut stream) = UnixStream::connect(socket) else { return Ok(Err(CallError::Down(daemon_down(socket)))) };
    if stream.set_write_timeout(Some(POLL)).is_err() || stream.set_read_timeout(Some(POLL)).is_err() {
        return Ok(Err(CallError::Down(daemon_down(socket))));
    }
    let id = next_id();
    let body = json!({ "id": id, "method": method, "params": params }).to_string();
    let broken = || match kind {
        Kind::Write => CallError::Failed(
            StrataError::new(Code::Unknown, "the connection to the daemon closed after the request was sent; see `git-strata status`")
                .with_detail(json!({ "reason": "socket_closed", "method": method })),
        ),
        Kind::Read => CallError::Failed(daemon_down(socket)),
    };
    // A write that times out (a full socket buffer) is retried after a
    // look at the flag; anything else is a broken connection.
    let mut send = |mut buf: &[u8]| -> Result<std::result::Result<(), ()>, Stopped> {
        while !buf.is_empty() {
            if stop.load(Ordering::SeqCst) {
                return Err(Stopped::NotSent);
            }
            match stream.write(buf) {
                Ok(0) => return Ok(Err(())),
                Ok(n) => buf = &buf[n..],
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => {}
                Err(_) => return Ok(Err(())),
            }
        }
        Ok(Ok(()))
    };
    // Every way out is checked against the flag: a signal caught while a
    // write or read was failing or finishing still stops the call (with
    // what was sent so far), so the exit code is always 128 + signal.
    let stopped = |sent: Stopped| stop.load(Ordering::SeqCst).then_some(sent);
    for chunk in body.as_bytes().chunks(CHUNK) {
        if send(chunk)?.is_err() {
            return match stopped(Stopped::NotSent) {
                Some(s) => Err(s),
                None => Ok(Err(CallError::Failed(daemon_down(socket)))),
            };
        }
    }
    test_interrupt("before_newline");
    let newline = if test_interrupt("newline_fails") { Ok(Err(())) } else { send(b"\n") };
    if newline?.is_err() {
        // The one-byte newline was not written: the daemon never dispatches
        // a line without it (`read_frame` drops a partial last line).
        return match stopped(Stopped::NotSent) {
            Some(s) => Err(s),
            None => Ok(Err(broken())),
        };
    }
    test_interrupt("after_newline");
    let r = read_reply(&stream, stop, &broken).and_then(|reply| parse_reply(method, &id, &reply));
    test_interrupt("after_reply");
    if let Some(s) = stopped(Stopped::Sent) {
        return Err(s);
    }
    // A signal from here on still sets the exit code (`exit_override`).
    test_interrupt("after_check");
    Ok(r)
}

/// The reply line. Gives up as soon as `stop` is set; the caller then
/// returns `Stopped::Sent` instead of this result.
fn read_reply(stream: &UnixStream, stop: &AtomicBool, broken: &dyn Fn() -> CallError) -> Result<String, CallError> {
    let mut r = BufReader::new(stream);
    let mut reply = Vec::new();
    loop {
        if stop.load(Ordering::SeqCst) {
            return Err(broken());
        }
        match r.read_until(b'\n', &mut reply) {
            Ok(0) => return Err(broken()),
            Ok(_) if reply.ends_with(b"\n") => break,
            Ok(_) => return Err(broken()),
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => {}
            Err(_) => return Err(broken()),
        }
    }
    Ok(String::from_utf8_lossy(&reply).into_owned())
}

