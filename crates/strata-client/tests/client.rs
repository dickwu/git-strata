//! gui.md §7 `client::call_maps_errors_like_cli`: the extracted `call`
//! maps every reply shape exactly as the cli's rpc.rs did before P0.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::thread;

use serde_json::{Value, json};
use strata_client::{CallError, Kind, call, listening};
use strata_types::{Code, StrataError};

/// How the fake daemon answers the one request it reads.
enum Answer {
    /// Reply with this envelope; `"$id"` in the `id` slot is replaced by the request id.
    Line(Value),
    /// Write these raw bytes (no id substitution).
    Raw(&'static str),
    /// Close the connection without answering.
    Close,
}

/// Serve one connection on a fresh socket; returns the socket path, the
/// tempdir guard, and a handle yielding the request line received.
fn serve(answer: Answer) -> (PathBuf, tempfile::TempDir, thread::JoinHandle<Value>) {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("d.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let h = thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&s).read_line(&mut line).unwrap();
        let req: Value = serde_json::from_str(line.trim_end()).unwrap();
        match answer {
            Answer::Line(mut v) => {
                if v.get("id") == Some(&json!("$id")) {
                    v["id"] = req["id"].clone();
                }
                s.write_all(format!("{v}\n").as_bytes()).unwrap();
            }
            Answer::Raw(b) => s.write_all(b.as_bytes()).unwrap(),
            Answer::Close => {}
        }
        req
    });
    (sock, dir, h)
}

fn run(answer: Answer, kind: Kind) -> (Result<Value, CallError>, Value, PathBuf) {
    let (sock, _dir, h) = serve(answer);
    let r = call(&sock, "m_test", json!({ "x": 1 }), kind);
    let req = h.join().unwrap();
    (r, req, sock)
}

fn failed(r: Result<Value, CallError>) -> StrataError {
    match r {
        Err(CallError::Failed(e)) => e,
        other => panic!("expected Failed, got {other:?}"),
    }
}

fn reason(e: &StrataError) -> &str {
    e.detail
        .as_ref()
        .and_then(|d| d.get("reason"))
        .and_then(Value::as_str)
        .unwrap_or("")
}

fn daemon_down_shape(e: &StrataError, sock: &Path) {
    assert_eq!(e.code, Code::Busy);
    assert_eq!(reason(e), "daemon_down");
    assert_eq!(
        e.detail.as_ref().unwrap()["socket"],
        json!(sock.to_string_lossy())
    );
}

#[test]
fn call_maps_errors_like_cli() {
    // ok → data; the request is one line with a decimal-string id, method, params.
    let (r, req, _) = run(
        Answer::Line(json!({ "id": "$id", "ok": true, "data": { "a": 1 } })),
        Kind::Read,
    );
    assert_eq!(r.unwrap(), json!({ "a": 1 }));
    assert_eq!(req["method"], json!("m_test"));
    assert_eq!(req["params"], json!({ "x": 1 }));
    let first: u64 = req["id"].as_str().unwrap().parse().unwrap();

    // ok without data → {}; ids keep counting up.
    let (r, req, _) = run(Answer::Line(json!({ "id": "$id", "ok": true })), Kind::Read);
    assert_eq!(r.unwrap(), json!({}));
    assert!(req["id"].as_str().unwrap().parse::<u64>().unwrap() > first);

    // A StrataError reply comes back as is (code, message, detail).
    let (r, _, _) = run(
        Answer::Line(json!({ "id": "$id", "ok": false,
            "error": { "code": "NoSuchSession", "message": "gone", "detail": { "session": "s1" } } })),
        Kind::Write,
    );
    let e = failed(r);
    assert_eq!((e.code, e.message.as_str()), (Code::NoSuchSession, "gone"));
    assert_eq!(e.detail, Some(json!({ "session": "s1" })));

    // A null detail is dropped.
    let (r, _, _) = run(
        Answer::Line(
            json!({ "id": "$id", "ok": false, "error": { "code": "Busy", "message": "b", "detail": null } }),
        ),
        Kind::Read,
    );
    let e = failed(r);
    assert_eq!((e.code, e.detail.clone()), (Code::Busy, None));

    // RPC-level UnknownMethod → InvalidInput unknown_method (the GUI's "old daemon" test).
    let (r, _, _) = run(
        Answer::Line(
            json!({ "id": "$id", "ok": false, "error": { "rpc": "UnknownMethod", "message": "no such method" } }),
        ),
        Kind::Read,
    );
    let e = failed(r);
    assert_eq!(e.code, Code::InvalidInput);
    assert_eq!(reason(&e), "unknown_method");
    assert_eq!(e.detail.as_ref().unwrap()["rpc"], json!("UnknownMethod"));
    assert_eq!(e.message, "no such method");

    // RPC-level Failed → InvalidInput rpc_failed.
    let (r, _, _) = run(
        Answer::Line(
            json!({ "id": "$id", "ok": false, "error": { "rpc": "Failed", "message": "bad params" } }),
        ),
        Kind::Read,
    );
    let e = failed(r);
    assert_eq!((e.code, reason(&e)), (Code::InvalidInput, "rpc_failed"));

    // An unknown code name → InvalidInput rpc_failed; a missing error → the default message.
    let (r, _, _) = run(
        Answer::Line(
            json!({ "id": "$id", "ok": false, "error": { "code": "NoSuchThing", "message": "x" } }),
        ),
        Kind::Read,
    );
    let e = failed(r);
    assert_eq!((e.code, reason(&e)), (Code::InvalidInput, "rpc_failed"));
    let (r, _, _) = run(
        Answer::Line(json!({ "id": "$id", "ok": false })),
        Kind::Read,
    );
    let e = failed(r);
    assert_eq!(
        (e.code, e.message.as_str()),
        (Code::InvalidInput, "the daemon refused the request")
    );

    // Another request id → InvalidInput rpc_id.
    let (r, _, _) = run(
        Answer::Line(json!({ "id": "999999", "ok": true, "data": {} })),
        Kind::Read,
    );
    let e = failed(r);
    assert_eq!((e.code, reason(&e)), (Code::InvalidInput, "rpc_id"));

    // Not JSON → InvalidInput rpc_failed.
    let (r, _, _) = run(Answer::Raw("not json\n"), Kind::Read);
    let e = failed(r);
    assert_eq!((e.code, reason(&e)), (Code::InvalidInput, "rpc_failed"));

    // Connection closed after sending: Write → Unknown socket_closed (no resend); Read → Busy daemon_down.
    let (r, _, _) = run(Answer::Close, Kind::Write);
    let e = failed(r);
    assert_eq!((e.code, reason(&e)), (Code::Unknown, "socket_closed"));
    assert_eq!(e.detail.as_ref().unwrap()["method"], json!("m_test"));
    let (r, _, sock) = run(Answer::Close, Kind::Read);
    daemon_down_shape(&failed(r), &sock);

    // A reply line cut before its newline counts as a broken connection.
    let (r, _, _) = run(Answer::Raw("{\"id\":\"1\",\"ok\":true}"), Kind::Write);
    assert_eq!(reason(&failed(r)), "socket_closed");

    // No socket: Down (nothing sent) with Busy daemon_down; listening is false.
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("none.sock");
    assert!(!listening(&missing));
    match call(&missing, "m_test", json!({}), Kind::Write) {
        Err(CallError::Down(e)) => daemon_down_shape(&e, &missing),
        other => panic!("expected Down, got {other:?}"),
    }
    // into_error / From give the inner error for both variants.
    let e: StrataError = CallError::Down(strata_client::daemon_down(&missing)).into();
    assert_eq!(e.code, Code::Busy);
}

#[test]
fn listening_probes_without_sending() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("l.sock");
    let _l = UnixListener::bind(&sock).unwrap();
    assert!(listening(&sock));
}
