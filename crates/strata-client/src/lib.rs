//! The json-lines client (cli.md §3.4): one connection per call, one
//! request line, one reply line. Ids are decimal strings counting from 1.

use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use strata_types::{Code, StrataError};

mod deadline;
pub mod deps;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// The next request id (one counter per process, shared with the cli's
/// interruptible calls).
pub fn next_id() -> String {
    NEXT_ID.fetch_add(1, Ordering::Relaxed).to_string()
}

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
    Timeout {
        method: String,
        waited: Duration,
        phase: &'static str,
        kind: Kind,
        sent: bool,
    },
}

impl CallError {
    pub fn into_error(self) -> StrataError {
        match self {
            CallError::Down(e) | CallError::Failed(e) => e,
            CallError::Timeout {
                method,
                waited,
                phase,
                kind,
                sent,
            } => {
                let in_progress = kind == Kind::Write && sent;
                let message = if in_progress {
                    format!(
                        "{method} may still run in the daemon after {:.3}s; it was not cancelled",
                        waited.as_secs_f64()
                    )
                } else {
                    format!(
                        "{method} timed out after {:.3}s; nothing was changed by this request",
                        waited.as_secs_f64()
                    )
                };
                let mut detail = json!({ "reason": "client_timeout", "method": method,
                    "waited_secs": waited.as_secs_f64(), "phase": phase, "request_sent": sent,
                    "fix": if in_progress { "run git-strata status to inspect the result; do not retry, gc, or rm" }
                        else { "back off before retrying" } });
                if in_progress {
                    detail["outcome"] = json!("in_progress");
                }
                StrataError::new(
                    if in_progress {
                        Code::Unknown
                    } else {
                        Code::Busy
                    },
                    message,
                )
                .with_detail(detail)
            }
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
        .with_detail(json!({ "reason": "daemon_down", "socket": socket.to_string_lossy(), "socket_exists": socket.symlink_metadata().is_ok() }))
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

/// `call` with one total wall-clock budget, including connect, send and reply.
pub fn call_within(
    socket: &Path,
    method: &str,
    params: Value,
    kind: Kind,
    budget: Duration,
) -> Result<Value, CallError> {
    call_inner(socket, method, params, kind, Some(budget))
}

fn call_inner(
    socket: &Path,
    method: &str,
    params: Value,
    kind: Kind,
    budget: Option<Duration>,
) -> Result<Value, CallError> {
    let started = Instant::now();
    let until = budget
        .map(|b| {
            started.checked_add(b).ok_or_else(|| {
                CallError::Failed(
                    StrataError::new(
                        Code::InvalidInput,
                        "timeout exceeds the monotonic clock range",
                    )
                    .with_detail(json!({"reason":"timeout_budget"})),
                )
            })
        })
        .transpose()?;
    let timeout = |phase, sent| CallError::Timeout {
        method: method.into(),
        waited: started.elapsed(),
        phase,
        kind,
        sent,
    };
    let mut stream = match until {
        Some(until) => deadline::connect(socket, until),
        None => UnixStream::connect(socket),
    }
    .map_err(|e| {
        if e.kind() == ErrorKind::TimedOut {
            timeout("connect", false)
        } else {
            CallError::Down(daemon_down(socket))
        }
    })?;
    call_on_stream(socket, method, params, kind, started, until, &mut stream)
}

// Separate the connected stream so the shed EPIPE race can be tested with a
// half-closed UnixStream pair, without scheduling assumptions or a live daemon.
// Carry the original clock and deadline through so connecting uses the same budget.
fn call_on_stream(
    socket: &Path,
    method: &str,
    params: Value,
    kind: Kind,
    started: Instant,
    until: Option<Instant>,
    stream: &mut UnixStream,
) -> Result<Value, CallError> {
    let timeout = |phase, sent| CallError::Timeout {
        method: method.into(),
        waited: started.elapsed(),
        phase,
        kind,
        sent,
    };
    let id = next_id();
    let line = format!(
        "{}\n",
        json!({ "id": id, "method": method, "params": params })
    );
    let broken = |sent| {
        match kind {
        Kind::Write if sent => CallError::Failed(
            StrataError::new(
                Code::Unknown,
                "the connection to the daemon closed after the request was sent; see `git-strata status`",
            )
            .with_detail(json!({ "reason": "socket_closed", "method": method })),
        ),
        _ => CallError::Failed(daemon_down(socket)),
    }
    };
    let mut bytes = line.as_bytes();
    while !bytes.is_empty() {
        if let Some(until) = until {
            let left = deadline::remaining(until).map_err(|_| timeout("send", false))?;
            if stream.set_write_timeout(Some(left)).is_err() {
                break;
            }
        }
        match stream.write(bytes) {
            Ok(0) => break,
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e)
                if until.is_some()
                    && matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
            {
                if kind == Kind::Read && until.is_some_and(|end| deadline::remaining(end).is_ok()) {
                    break;
                }
                return Err(timeout("send", false));
            }
            Err(_) => break,
        }
    }
    // Only the final newline makes this a dispatchable request.
    let sent = bytes.is_empty();
    if !sent && kind == Kind::Write {
        return Err(broken(false));
    }
    // The shed path replies before reading, then closes. A read may hit EPIPE
    // with a reply already queued; consume that line without resending.
    let mut reply = Vec::new();
    let mut buf = [0u8; 8192];
    let mut nonblocking = false;
    loop {
        if let Some(until) = until {
            let left = deadline::remaining(until).map_err(|_| timeout("reply", sent))?;
            if !nonblocking && stream.set_read_timeout(Some(left)).is_err() {
                // macOS can reject SO_RCVTIMEO after EPIPE while queued bytes
                // remain readable. Never wait under an old, longer timeout.
                if sent || stream.set_nonblocking(true).is_err() {
                    return Err(broken(sent));
                }
                nonblocking = true;
            }
        }
        match stream.read(&mut buf) {
            Ok(0) => return Err(broken(sent)),
            Ok(n) => {
                if let Some(end) = buf[..n].iter().position(|b| *b == b'\n') {
                    reply.extend_from_slice(&buf[..=end]);
                    break;
                }
                reply.extend_from_slice(&buf[..n]);
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e)
                if until.is_some()
                    && !nonblocking
                    && matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
            {
                return Err(timeout("reply", sent));
            }
            Err(_) => return Err(broken(sent)),
        }
    }
    let reply = String::from_utf8(reply).map_err(|_| broken(sent))?;
    parse_reply(method, &id, &reply)
}

/// One reply line (without or with its newline) checked against the
/// request id: its `data`, or its error.
pub fn parse_reply(method: &str, id: &str, reply: &str) -> Result<Value, CallError> {
    let v: Value = serde_json::from_str(reply.trim_end()).map_err(|e| {
        CallError::Failed(
            StrataError::new(
                Code::InvalidInput,
                format!("the daemon's reply is not JSON: {e}"),
            )
            .with_detail(json!({ "reason": "rpc_failed", "method": method })),
        )
    })?;
    // The fd-reserve shed envelope is the only null-id exception.
    let shed = v.get("id") == Some(&Value::Null)
        && v.get("ok") == Some(&Value::Bool(false))
        && v["error"]["code"] == "Busy"
        && v["error"]["detail"]["reason"] == "too_many_open_files";
    if !shed && v.get("id") != Some(&Value::String(id.to_string())) {
        return Err(CallError::Failed(
            StrataError::new(Code::InvalidInput, "the daemon answered another request id")
                .with_detail(
                    json!({ "reason": "rpc_id", "method": method, "sent": id, "got": v.get("id") }),
                ),
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
    let message = err
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("the daemon refused the request")
        .to_string();
    if let Some(rpc) = err.get("rpc").and_then(Value::as_str) {
        let reason = if rpc == "UnknownMethod" {
            "unknown_method"
        } else {
            "rpc_failed"
        };
        return StrataError::new(Code::InvalidInput, message)
            .with_detail(json!({ "reason": reason, "method": method, "rpc": rpc }));
    }
    let code = err
        .get("code")
        .and_then(Value::as_str)
        .and_then(|c| Code::ALL.iter().find(|k| k.as_str() == c).copied());
    match code {
        Some(code) => {
            let mut e = StrataError::new(code, message);
            e.detail = err.get("detail").cloned().filter(|d| !d.is_null());
            e
        }
        None => StrataError::new(Code::InvalidInput, message)
            .with_detail(json!({ "reason": "rpc_failed", "method": method })),
    }
}

#[cfg(test)]
mod shed_tests {
    use super::*;

    const SHED: &[u8] = b"{\"id\":null,\"ok\":false,\"error\":{\"code\":\"Busy\",\"detail\":{\"reason\":\"too_many_open_files\"}}}\n";

    #[test]
    fn failed_send_consumes_queued_shed_for_reads_but_unsent_writes_are_down() {
        for budget in [None, Some(Duration::from_secs(1))] {
            for kind in [Kind::Read, Kind::Write] {
                let (mut client, mut server) = UnixStream::pair().unwrap();
                client.shutdown(std::net::Shutdown::Write).unwrap();
                client
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                server.write_all(SHED).unwrap();
                server.shutdown(std::net::Shutdown::Write).unwrap();
                let started = Instant::now();
                let error = call_on_stream(
                    Path::new("fixture.sock"),
                    "daemon_status",
                    json!({}),
                    kind,
                    started,
                    budget.map(|b| started + b),
                    &mut client,
                )
                .unwrap_err()
                .into_error();
                assert_eq!(error.code, Code::Busy);
                assert_eq!(
                    error.detail.unwrap()["reason"],
                    if kind == Kind::Read {
                        "too_many_open_files"
                    } else {
                        "daemon_down"
                    }
                );
                if kind == Kind::Write {
                    let mut queued = Vec::new();
                    client.read_to_end(&mut queued).unwrap();
                    assert_eq!(queued, SHED, "an unsent write must not consume the reply");
                }
            }
        }
    }

    #[test]
    fn failed_read_send_consumes_queued_shed_when_budget_remains() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        client.set_nonblocking(true).unwrap();
        server.write_all(SHED).unwrap();
        let started = Instant::now();
        // Force WouldBlock before the newline while a shed reply is queued.
        let error = call_on_stream(
            Path::new("fixture.sock"),
            "daemon_status",
            json!({ "large": "x".repeat(1024 * 1024) }),
            Kind::Read,
            started,
            Some(started + Duration::from_secs(1)),
            &mut client,
        )
        .unwrap_err()
        .into_error();
        assert_eq!(error.code, Code::Busy);
        assert_eq!(error.detail.unwrap()["reason"], "too_many_open_files");
    }

    #[test]
    fn failed_send_with_an_incomplete_reply_never_uses_the_old_timeout() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        server.write_all(&SHED[..SHED.len() - 1]).unwrap();
        // Keep the peer open: EOF must not hide an accidentally reused timeout.
        let started = Instant::now();
        let error = call_on_stream(
            Path::new("fixture.sock"),
            "daemon_status",
            json!({}),
            Kind::Read,
            started,
            Some(started + Duration::from_millis(100)),
            &mut client,
        )
        .unwrap_err();
        assert!(started.elapsed() < Duration::from_millis(600));
        match error {
            CallError::Failed(e) => {
                // Nonblocking fallback when SO_RCVTIMEO fails after EPIPE.
                assert_eq!(e.code, Code::Busy);
                assert_eq!(e.detail.unwrap()["reason"], "daemon_down");
            }
            CallError::Timeout {
                phase: "reply",
                sent: false,
                ..
            } => {}
            other => panic!("unexpected incomplete shed reply: {other:?}"),
        }
    }

    #[test]
    fn connected_stream_keeps_the_original_deadline_and_elapsed_time() {
        let (mut client, _server) = UnixStream::pair().unwrap();
        let now = Instant::now();
        let started = now - Duration::from_millis(800);
        let error = call_on_stream(
            Path::new("fixture.sock"),
            "daemon_status",
            json!({}),
            Kind::Read,
            started,
            Some(started + Duration::from_secs(1)),
            &mut client,
        )
        .unwrap_err();
        assert!(now.elapsed() < Duration::from_millis(600));
        match error {
            CallError::Timeout {
                phase: "reply",
                sent: true,
                waited,
                ..
            } => {
                assert!(waited >= Duration::from_millis(950));
            }
            other => panic!("unexpected deadline result: {other:?}"),
        }
    }
}
