//! The json-lines client (cli.md §3.4): one connection per call, one
//! request line, one reply line. Ids are decimal strings counting from 1.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;
use std::sync::atomic::{AtomicU64, Ordering};

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
    StrataError::new(Code::Busy, "the daemon is not running; run `git-strata daemon start`")
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
    let v: Value = serde_json::from_str(reply.trim_end()).map_err(|e| {
        CallError::Failed(
            StrataError::new(Code::InvalidInput, format!("the daemon's reply is not JSON: {e}"))
                .with_detail(json!({ "reason": "rpc_failed", "method": method })),
        )
    })?;
    if v.get("id") != Some(&Value::String(id.clone())) {
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
