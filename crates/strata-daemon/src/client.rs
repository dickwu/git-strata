//! The client side of the socket (one request per connection). The cli
//! crate does not exist yet; this is the piece of it the daemon's
//! contract needs: a write method with no daemon listening fails
//! `ReadOnly` (reason `daemon_down`, daemon.md §6) instead of writing
//! anything itself. Read-only methods are the cli's direct reads.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use serde_json::{Value, json};
use strata_types::{Code, StrataError};

use crate::Result;
use crate::rpc::Method;

/// Whether the cli must go through the daemon for `method` (every write
/// method); read-only ones can be answered by reading directly.
pub fn needs_daemon(method: Method) -> bool {
    !method.is_read_only()
}

fn daemon_down(socket: &Path, why: &str) -> StrataError {
    StrataError::new(Code::ReadOnly, format!("the daemon is not running ({why}); nothing was written"))
        .with_detail(json!({ "reason": "daemon_down", "socket": socket.to_string_lossy() }))
}

/// Send one request and read its reply. `Ok(data)` on `ok: true`; a
/// `code` error comes back as that `StrataError`; an RPC-level refusal is
/// `InvalidInput` with `detail.rpc`. A missing / refusing socket is
/// `ReadOnly` (`daemon_down`). A connection that ends without a reply is
/// `Unknown`: the request may have run, and it is not sent again. No read
/// timeout (rpc I3): it waits as long as the daemon keeps the connection.
pub fn call(socket: &Path, method: &str, params: Value) -> Result<Value> {
    let mut s = match UnixStream::connect(socket) {
        Ok(s) => s,
        Err(e) => return Err(daemon_down(socket, &e.to_string())),
    };
    let lost = |what: &str| {
        StrataError::new(Code::Unknown, format!("the reply to {method} was lost ({what}); it is not sent again"))
            .with_detail(json!({ "reason": "reply_lost", "method": method }))
    };
    let line = json!({ "id": 1, "method": method, "params": params }).to_string();
    s.write_all(line.as_bytes()).and_then(|_| s.write_all(b"\n")).map_err(|e| lost(&e.to_string()))?;
    let mut r = BufReader::new(s);
    let mut out = String::new();
    match r.read_line(&mut out) {
        Ok(0) => return Err(lost("end of stream")),
        Ok(_) => {}
        Err(e) => return Err(lost(&e.to_string())),
    }
    let v: Value = serde_json::from_str(&out).map_err(|e| lost(&e.to_string()))?;
    if v["ok"] == json!(true) {
        return Ok(v["data"].clone());
    }
    let err = &v["error"];
    if let Some(rpc) = err.get("rpc") {
        return Err(StrataError::new(Code::InvalidInput, err["message"].as_str().unwrap_or("rejected").to_string())
            .with_detail(json!({ "rpc": rpc })));
    }
    let code: Code = serde_json::from_value(err["code"].clone()).unwrap_or(Code::Unknown);
    let mut e = StrataError::new(code, err["message"].as_str().unwrap_or("").to_string());
    if let Some(d) = err.get("detail") {
        e = e.with_detail(d.clone());
    }
    Err(e)
}
