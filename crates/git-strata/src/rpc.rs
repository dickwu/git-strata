//! The json-lines client (cli.md §3.4): one connection per call, one
//! request line, one reply line. Ids are decimal strings counting from 1.

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use serde_json::{Value, json};
use strata_types::{Code, StrataError};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Whether a call writes (a broken connection after sending is then
/// `Unknown socket_closed`) or only reads (`Busy daemon_down`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Read,
    Write,
}

/// A failed call. `Down` means nothing was sent: the socket did not accept
/// a connection (the local read-only path may take over).
#[derive(Debug)]
pub enum CallError {
    Down(StrataError),
    Failed(StrataError),
}

impl CallError {
    pub fn into_error(self) -> StrataError {
        match self {
            CallError::Down(e) | CallError::Failed(e) => e,
        }
    }
}

impl From<CallError> for StrataError {
    fn from(e: CallError) -> StrataError {
        e.into_error()
    }
}

pub fn daemon_down(socket: &Path) -> StrataError {
    StrataError::new(Code::Busy, "the daemon is not running; ask the human to run `git-strata daemon start`")
        .with_detail(json!({ "reason": "daemon_down", "socket": socket.to_string_lossy() }))
}

/// Whether something accepts connections on `socket` (the probe sends
/// nothing).
pub fn listening(socket: &Path) -> bool {
    UnixStream::connect(socket).is_ok()
}

/// Send `method` with `params`; the reply's `data`, or its error as is.
pub fn call(socket: &Path, method: &str, params: Value, kind: Kind) -> Result<Value, CallError> {
    call_inner(socket, method, params, kind, None)
}

/// `call`, giving up (as a broken connection) when sending or the reply
/// takes longer than `deadline` each. For probes that must not hang.
pub fn call_within(socket: &Path, method: &str, params: Value, kind: Kind, deadline: Duration) -> Result<Value, CallError> {
    call_inner(socket, method, params, kind, Some(deadline))
}

fn call_inner(socket: &Path, method: &str, params: Value, kind: Kind, deadline: Option<Duration>) -> Result<Value, CallError> {
    let mut stream = UnixStream::connect(socket).map_err(|_| CallError::Down(daemon_down(socket)))?;
    if deadline.is_some() && (stream.set_read_timeout(deadline).is_err() || stream.set_write_timeout(deadline).is_err()) {
        return Err(CallError::Down(daemon_down(socket)));
    }
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed).to_string();
    let line = format!("{}\n", json!({ "id": id, "method": method, "params": params }));
    let broken = || match kind {
        Kind::Write => CallError::Failed(
            StrataError::new(Code::Unknown, "the connection to the daemon closed after the request was sent; see `git-strata status`")
                .with_detail(json!({ "reason": "socket_closed", "method": method })),
        ),
        Kind::Read => CallError::Failed(daemon_down(socket)),
    };
    if stream.write_all(line.as_bytes()).and_then(|_| stream.flush()).is_err() {
        return Err(broken());
    }
    let mut reply = String::new();
    match BufReader::new(&stream).read_line(&mut reply) {
        Ok(0) | Err(_) => return Err(broken()),
        Ok(_) => {}
    }
    if !reply.ends_with('\n') {
        return Err(broken());
    }
    parse_reply(method, &id, &reply)
}

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
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed).to_string();
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

fn parse_reply(method: &str, id: &str, reply: &str) -> Result<Value, CallError> {
    let v: Value = serde_json::from_str(reply.trim_end()).map_err(|e| {
        CallError::Failed(
            StrataError::new(Code::InvalidInput, format!("the daemon's reply is not JSON: {e}"))
                .with_detail(json!({ "reason": "rpc_failed", "method": method })),
        )
    })?;
    if v.get("id") != Some(&Value::String(id.to_string())) {
        return Err(CallError::Failed(
            StrataError::new(Code::InvalidInput, "the daemon answered another request id")
                .with_detail(json!({ "reason": "rpc_id", "method": method, "sent": id, "got": v.get("id") })),
        ));
    }
    if v.get("ok") == Some(&Value::Bool(true)) {
        return Ok(v.get("data").cloned().unwrap_or_else(|| json!({})));
    }
    Err(CallError::Failed(reply_error(method, v.get("error"))))
}

/// The reply's error: a `StrataError` as is; the RPC-level names
/// (`UnknownMethod`, `Failed`) become `InvalidInput` (D7).
fn reply_error(method: &str, err: Option<&Value>) -> StrataError {
    let err = err.cloned().unwrap_or(Value::Null);
    let message = err.get("message").and_then(Value::as_str).unwrap_or("the daemon refused the request").to_string();
    if let Some(rpc) = err.get("rpc").and_then(Value::as_str) {
        let reason = if rpc == "UnknownMethod" { "unknown_method" } else { "rpc_failed" };
        return StrataError::new(Code::InvalidInput, message).with_detail(json!({ "reason": reason, "method": method, "rpc": rpc }));
    }
    let code = err.get("code").and_then(Value::as_str).and_then(|c| Code::ALL.iter().find(|k| k.as_str() == c).copied());
    match code {
        Some(code) => {
            let mut e = StrataError::new(code, message);
            e.detail = err.get("detail").cloned().filter(|d| !d.is_null());
            e
        }
        None => StrataError::new(Code::InvalidInput, message).with_detail(json!({ "reason": "rpc_failed", "method": method })),
    }
}
