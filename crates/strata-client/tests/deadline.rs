use serde_json::json;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixListener;
use std::thread;
use std::time::{Duration, Instant};
use strata_client::{CallError, Kind, call_within};
use strata_types::Code;

fn socket() -> (tempfile::TempDir, std::path::PathBuf, UnixListener) {
    let dir = tempfile::Builder::new().prefix("deadline-").tempdir_in("/tmp").unwrap();
    let path = dir.path().join("s");
    let listener = UnixListener::bind(&path).unwrap();
    (dir, path, listener)
}

#[test]
fn silence_and_dribbled_bytes_share_one_total_reply_budget() {
    for dribble in [false, true] {
        for kind in [Kind::Read, Kind::Write] {
            let (_dir, path, listener) = socket();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = String::new();
                BufReader::new(&stream).read_line(&mut request).unwrap();
                if dribble {
                    for _ in 0..20 {
                        if stream.write_all(b" ").is_err() {
                            break;
                        }
                        thread::sleep(Duration::from_millis(20));
                    }
                } else {
                    thread::sleep(Duration::from_millis(250));
                }
            });
            let start = Instant::now();
            let error = call_within(&path, "sync", json!({}), kind, Duration::from_millis(150)).unwrap_err();
            assert!(start.elapsed() >= Duration::from_millis(140));
            assert!(start.elapsed() < Duration::from_millis(250));
            assert!(matches!(error, CallError::Timeout { phase: "reply", sent: true, .. }));
            let error = error.into_error();
            assert_eq!(error.code, if kind == Kind::Write { Code::Unknown } else { Code::Busy });
            assert_eq!(error.detail.as_ref().unwrap()["reason"], "client_timeout");
            if kind == Kind::Write {
                assert!(error.message.contains("may still run"));
            }
            server.join().unwrap();
        }
    }
}

#[test]
fn missing_and_stale_socket_are_down_immediately() {
    let (_dir, path, listener) = socket();
    drop(listener);
    for exists in [true, false] {
        if !exists {
            std::fs::remove_file(&path).unwrap();
        }
        let start = Instant::now();
        let error = call_within(&path, "sync", json!({}), Kind::Write, Duration::from_secs(30)).unwrap_err();
        assert!(start.elapsed() < Duration::from_millis(300));
        assert!(matches!(error, CallError::Down(_)));
        let error = error.into_error();
        assert_eq!(error.detail.unwrap()["socket_exists"], exists);
    }
}

#[test]
fn partial_send_without_newline_is_busy_and_not_dispatched() {
    let (_dir, path, listener) = socket();
    let (release, wait) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        // Prove a partial frame reached the peer, then stop draining until
        // the client has hit its deadline. No scheduler-dependent sleep.
        let mut first = [0u8; 1];
        stream.read_exact(&mut first).unwrap();
        wait.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut bytes = Vec::from(first);
        stream.read_to_end(&mut bytes).unwrap();
        assert!(!bytes.contains(&b'\n'));
    });
    let error = call_within(
        &path,
        "sync",
        json!({ "large": "x".repeat(1024 * 1024) }),
        Kind::Write,
        Duration::from_secs(1),
    )
    .unwrap_err();
    assert!(matches!(error, CallError::Timeout { phase: "send", sent: false, .. }));
    let error = error.into_error();
    assert_eq!(error.code, Code::Busy);
    assert!(error.detail.unwrap().get("outcome").is_none());
    release.send(()).unwrap();
    server.join().unwrap();
}

#[test]
fn unaccepted_connection_expires_in_reply_and_zero_budget_in_connect() {
    let (_dir, path, _listener) = socket();
    let error = call_within(&path, "sync", json!({}), Kind::Write, Duration::from_millis(100)).unwrap_err();
    assert!(matches!(error, CallError::Timeout { phase: "reply", sent: true, .. }));
    let error = call_within(&path, "sync", json!({}), Kind::Write, Duration::ZERO).unwrap_err();
    assert!(matches!(error, CallError::Timeout { phase: "connect", sent: false, .. }));
}
