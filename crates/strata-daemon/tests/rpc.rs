//! daemon.md Task 1: the json-lines envelope. The `no_*` tests pin what the
//! envelope is not (no handshake, token, scope, binary framing or
//! encryption); each asserts the observable shape of requests and replies.

use std::io::{BufRead, BufReader, Cursor, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use strata_daemon::rpc::{self, Frame, Method, Params, Reply, parse_request};
use strata_daemon::{MAX_FRAME, RpcReject};
use strata_types::{Code, StrataError};

fn req(v: Value) -> Result<rpc::Request, Reply> {
    parse_request(&serde_json::to_vec(&v).unwrap())
}

fn rejected(v: Value) -> Value {
    req(v).expect_err("must be refused").0
}

fn rpc_of(reply: &Value) -> &str {
    reply["error"]["rpc"].as_str().unwrap_or("")
}

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Every source file of the crate, concatenated.
fn sources() -> String {
    let mut out = String::new();
    for e in std::fs::read_dir(src_dir()).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "rs") {
            out.push_str(&std::fs::read_to_string(p).unwrap());
        }
    }
    out
}

fn status_req(id: Value) -> Value {
    json!({ "id": id, "method": "status", "params": { "cwd": null, "session": null, "all": true } })
}

/// A socket in a short temp dir, served by `serve_connection` with an echo
/// handler that answers `{"method": name}`.
struct Echo {
    _dir: tempfile::TempDir,
    sock: PathBuf,
}

fn echo_server(limit: usize) -> Echo {
    let dir = tempfile::Builder::new().prefix("sd-").tempdir_in("/tmp").unwrap();
    let sock = dir.path().join("daemon.sock");
    let l = UnixListener::bind(&sock).unwrap();
    std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(s) = s else { return };
            std::thread::spawn(move || {
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut w = s;
                let _ = rpc::serve_connection(&mut r, &mut w, limit, &mut |q| {
                    Reply::ok(&q.id, json!({ "method": q.method.name() }))
                });
            });
        }
    });
    Echo { _dir: dir, sock }
}

fn roundtrip(sock: &Path, lines: &[&[u8]]) -> Vec<Value> {
    let s = UnixStream::connect(sock).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut w = s.try_clone().unwrap();
    for l in lines {
        w.write_all(l).unwrap();
    }
    w.flush().unwrap();
    let mut r = BufReader::new(s);
    let mut out = Vec::new();
    for _ in lines {
        let mut buf = String::new();
        r.read_line(&mut buf).unwrap();
        out.push(serde_json::from_str(&buf).unwrap());
    }
    out
}

#[test]
fn method_table_has_the_twenty_names() {
    let names: Vec<&str> = Method::ALL.iter().map(|m| m.name()).collect();
    assert_eq!(
        names,
        [
            "new", "status", "checkpoint", "restore", "grep", "files", "diff", "deps_propose", "deps_approve",
            "deps_list", "publish", "publish_plan", "push", "assign", "rm", "gc", "resolve_move", "chain_resolve", "config_relax",
            "daemon_status"
        ]
    );
    let ro: Vec<&str> = Method::ALL.iter().filter(|m| m.is_read_only()).map(|m| m.name()).collect();
    assert_eq!(ro, ["status", "grep", "files", "diff", "deps_list", "publish_plan", "daemon_status"]);
    for m in Method::ALL {
        assert_eq!(Method::from_name(m.name()), Some(*m));
    }
}

#[test]
fn unknown_method_is_rpc_unknown_method_without_code() {
    let r = rejected(json!({ "id": 7, "method": "ping", "params": {} }));
    assert_eq!(r["id"], 7);
    assert_eq!(r["ok"], false);
    assert_eq!(rpc_of(&r), "UnknownMethod");
    assert!(r["error"].get("code").is_none(), "{r}");
}

#[test]
fn bad_envelope_or_params_is_rpc_failed_without_code() {
    let cases = [
        json!({ "method": "status", "params": {} }),
        json!({ "id": "a", "params": {} }),
        json!({ "id": "a", "method": "status" }),
        json!({ "id": true, "method": "status", "params": {} }),
        json!({ "id": "a", "method": "status", "params": { "cwd": null, "session": null } }),
        json!({ "id": "a", "method": "status", "params": { "cwd": null, "session": null, "all": "yes" } }),
        json!({ "id": "a", "method": "status", "params": { "cwd": null, "session": null, "all": true, "x": 1 } }),
        json!({ "id": "a", "method": "status", "params": [] }),
        json!({ "id": "a", "method": "chain_resolve", "params": { "repo": "tool", "choice": "Force" } }),
        json!({ "id": "a", "method": "resolve_move", "params": { "session": "s-abcdefgh", "repo": "tool", "abandon": true, "accept": true } }),
        json!({ "id": "a", "method": "resolve_move", "params": { "session": "s-abcdefgh", "repo": "tool", "abandon": false, "accept": false } }),
        json!([1, 2]),
    ];
    for c in cases {
        let r = rejected(c.clone());
        assert_eq!(rpc_of(&r), "Failed", "{c} -> {r}");
        assert!(r["error"].get("code").is_none(), "{r}");
        assert!(r["error"]["message"].is_string());
    }
    let r = parse_request(b"{not json").unwrap_err().0;
    assert_eq!(rpc_of(&r), "Failed");
    assert_eq!(r["id"], Value::Null);
}

#[test]
fn id_is_echoed_as_string_or_number() {
    for id in [json!("abc"), json!(42), json!(1.5)] {
        let q = req(status_req(id.clone())).unwrap();
        assert_eq!(q.id, id);
        let r = Reply::ok(&q.id, Value::Null).0;
        assert_eq!(r, json!({ "id": id, "ok": true, "data": {} }));
    }
}

#[test]
fn upstream_error_carries_code_message_detail() {
    let e = StrataError::new(Code::Busy, "held").with_detail(json!({ "repo": "tool" }));
    let r = Reply::error(&json!(3), &e).0;
    assert_eq!(r, json!({ "id": 3, "ok": false, "error": { "code": "Busy", "message": "held", "detail": { "repo": "tool" } } }));
    let r = Reply::error(&json!(3), &StrataError::new(Code::Conflict, "x")).0;
    assert!(r["error"].get("detail").is_none());
    assert!(r["error"].get("rpc").is_none());
}

#[test]
fn every_method_parses_its_documented_params() {
    let sid = "s-abcdefgh";
    let cases = [
        ("new", json!({ "ticket": null, "repos": ["tool"] })),
        ("status", json!({ "cwd": "/x", "session": null, "all": false })),
        ("checkpoint", json!({ "session": sid, "repo": "tool", "reason": { "kind": "Manual", "message": "m" }, "paths": [] })),
        ("restore", json!({ "session": sid, "repo": "tool", "checkpoint": 3, "paths": null })),
        ("grep", json!({ "pattern": "x", "session": null, "all": true, "ignore_case": false })),
        ("files", json!({ "session": sid, "repo": "tool", "glob": "*.ts" })),
        ("diff", json!({ "a": sid, "b": "s-bcdefghi", "repo": "tool" })),
        ("deps_propose", json!({ "session": sid, "repo": "tool", "switch": true })),
        ("deps_approve", json!({ "proposal": 1, "sessions": null })),
        ("deps_list", json!({ "repo": null })),
        ("publish", json!({ "group": { "id": 0, "batches": [] } })),
        ("publish_plan", json!({ "group": { "id": 0, "batches": [] } })),
        ("push", json!({ "repo": "tool" })),
        ("assign", json!({ "group": 1, "path": "a.txt", "session": sid })),
        ("rm", json!({ "session": sid, "discard": false })),
        ("gc", json!({ "repo": "tool" })),
        ("resolve_move", json!({ "session": sid, "repo": "tool", "abandon": true, "accept": false })),
        ("chain_resolve", json!({ "repo": "tool", "choice": "AcceptOriginDrop" })),
        ("config_relax", json!({ "repo": "tool" })),
        ("daemon_status", json!({})),
    ];
    assert_eq!(cases.len(), Method::ALL.len());
    for (m, p) in cases {
        let q = req(json!({ "id": 1, "method": m, "params": p })).unwrap_or_else(|r| panic!("{m}: {:?}", r.0));
        assert_eq!(q.method.name(), m);
    }
    let q = req(json!({ "id": 1, "method": "daemon_status", "params": { "verify": true } })).unwrap();
    assert_eq!(q.params, Params::DaemonStatus(rpc::DaemonStatusParams { verify: true }));
}

#[test]
fn too_large_line_is_code_too_large() {
    // The real limit: one byte over MAX_FRAME, then a normal request.
    let mut big = vec![b'x'; MAX_FRAME + 1];
    big.push(b'\n');
    big.extend_from_slice(&serde_json::to_vec(&status_req(json!(1))).unwrap());
    big.push(b'\n');
    let mut r = Cursor::new(big);
    assert_eq!(rpc::read_frame(&mut r).unwrap(), Frame::TooLarge);
    assert!(matches!(rpc::read_frame(&mut r).unwrap(), Frame::Line(_)));
    assert_eq!(rpc::read_frame(&mut r).unwrap(), Frame::Eof);
    let reply = Reply::too_large(MAX_FRAME).0;
    assert_eq!(reply["error"]["code"], "TooLarge");
    assert!(reply["error"].get("rpc").is_none());
    // Exactly MAX_FRAME is accepted.
    let mut ok = vec![b'y'; MAX_FRAME];
    ok.push(b'\n');
    assert!(matches!(rpc::read_frame(&mut Cursor::new(ok)).unwrap(), Frame::Line(l) if l.len() == MAX_FRAME));
}

// ---- What the envelope is not.

#[test]
fn no_auth_facade_observation() {
    for extra in ["scope", "token", "observed", "auth"] {
        let mut v = status_req(json!(1));
        v[extra] = json!("x");
        assert_eq!(rpc_of(&rejected(v)), "Failed", "{extra}");
    }
    // Plain json lines from the same uid are the whole connection.
    let e = echo_server(MAX_FRAME);
    let out = roundtrip(&e.sock, &[format!("{}\n", status_req(json!(1))).as_bytes()]);
    assert_eq!(out[0]["data"]["method"], "status");
}

#[test]
fn no_auth_reply_token() {
    let r = Reply::ok(&json!(1), json!({ "x": 1 })).0;
    let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["data", "id", "ok"]);
}

#[test]
fn no_binary_frame() {
    // A short line is a frame; there is no 8192-byte fixed record.
    let mut r = Cursor::new(b"{}\n".to_vec());
    assert_eq!(rpc::read_frame(&mut r).unwrap(), Frame::Line(b"{}".to_vec()));
    let mut r = Cursor::new([vec![b'a'; 9000], b"\n".to_vec()].concat());
    assert!(matches!(rpc::read_frame(&mut r).unwrap(), Frame::Line(l) if l.len() == 9000));
}

#[test]
fn no_binary_reply_frame() {
    let line = Reply::ok(&json!(1), json!({})).to_line();
    assert_eq!(*line.last().unwrap(), b'\n');
    assert_eq!(line.iter().filter(|&&b| b == b'\n').count(), 1);
    assert!(serde_json::from_slice::<Value>(&line).is_ok());
}

#[test]
fn no_binary_tag_extension() {
    // Additions are serde fields; an unknown field is RPC Failed.
    let v = json!({ "id": 1, "method": "push", "params": { "repo": "tool", "tag": 1 } });
    assert_eq!(rpc_of(&rejected(v)), "Failed");
    assert_eq!(rpc_of(&parse_request(&[0x01, 0x02, 0x03]).unwrap_err().0), "Failed");
}

#[test]
fn no_branch_page_frame() {
    // One request, one reply line, whatever its size.
    let e = echo_server(MAX_FRAME);
    let l1 = format!("{}\n", status_req(json!(1)));
    let l2 = format!("{}\n", status_req(json!(2)));
    let out = roundtrip(&e.sock, &[l1.as_bytes(), l2.as_bytes()]);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0]["id"], 1);
    assert_eq!(out[1]["id"], 2);
}

#[test]
fn no_tcp_nodelay_auth() {
    let src = sources();
    for banned in ["TcpListener", "TcpStream", "set_nodelay", "TCP_NODELAY"] {
        assert!(!src.contains(banned), "{banned} in the daemon");
    }
}

#[test]
fn no_noise_window() {
    let src = sources().to_lowercase();
    for banned in ["noise", "ciphertext", "chacha", "x25519"] {
        assert!(!src.contains(banned), "{banned}");
    }
}

#[test]
fn no_nonce_record_limit() {
    // `FnOnce` is a closure bound, not a nonce.
    assert!(!sources().replace("FnOnce", "").to_lowercase().contains("nonce"));
    assert!(!format!("{:?}", Code::TooLarge).contains("Record"));
    let reply = Reply::too_large(MAX_FRAME).0;
    assert_eq!(reply["error"]["code"], "TooLarge");
}

#[test]
fn no_encrypted_record() {
    // Requests and replies are readable json text in both directions.
    let e = echo_server(MAX_FRAME);
    let out = roundtrip(&e.sock, &[format!("{}\n", status_req(json!("q"))).as_bytes()]);
    assert_eq!(out[0]["ok"], true);
    assert!(!sources().to_lowercase().contains("encrypt"));
}

#[test]
fn no_tamper_replay_frame() {
    // The same line twice is two independent requests; no MAC, no replay
    // window.
    let a = req(status_req(json!(1))).unwrap();
    let b = req(status_req(json!(1))).unwrap();
    assert_eq!(a, b);
    assert!(!sources().to_lowercase().contains("hmac"));
}

#[test]
fn no_duplex_eof_fence() {
    // End of stream ends the connection; a trailing partial line is dropped.
    let mut r = Cursor::new(b"{\"id\":1".to_vec());
    assert_eq!(rpc::read_frame(&mut r).unwrap(), Frame::Eof);
    let mut out = Vec::new();
    let mut n = 0;
    rpc::serve_connection(&mut Cursor::new(Vec::new()), &mut out, MAX_FRAME, &mut |q| {
        n += 1;
        Reply::ok(&q.id, json!({}))
    })
    .unwrap();
    assert_eq!(n, 0);
    assert!(out.is_empty());
}

#[test]
fn no_duplex_trailing_byte() {
    // Bytes after the last full line are not a protocol state; the full
    // lines before them are answered.
    let input = format!("{}\nxyz", status_req(json!(1)));
    let mut out = Vec::new();
    rpc::serve_connection(&mut Cursor::new(input.into_bytes()), &mut out, MAX_FRAME, &mut |q| {
        Reply::ok(&q.id, json!({}))
    })
    .unwrap();
    assert_eq!(out.iter().filter(|&&b| b == b'\n').count(), 1);
}

#[test]
fn no_duplex_read_timeout() {
    // The server waits for the next line as long as the peer keeps the
    // connection open: no read timeout stage.
    let e = echo_server(MAX_FRAME);
    let s = UnixStream::connect(&e.sock).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let mut w = s.try_clone().unwrap();
    w.write_all(format!("{}\n", status_req(json!(9))).as_bytes()).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["id"], 9);
    assert!(!sources().contains("set_read_timeout"));
}

#[test]
fn no_observed_prefix() {
    let mut line = b"observed:".to_vec();
    line.extend(serde_json::to_vec(&status_req(json!(1))).unwrap());
    assert_eq!(rpc_of(&parse_request(&line).unwrap_err().0), "Failed");
}

#[test]
fn no_observed_wrapping() {
    let v = json!({ "id": 1, "method": "status", "params": { "observed": status_req(json!(1)) } });
    assert_eq!(rpc_of(&rejected(v)), "Failed");
}

#[test]
fn no_zero_scope() {
    // An all-zero value is just an id.
    let q = req(status_req(json!(0))).unwrap();
    assert_eq!(q.id, 0);
    let q = req(status_req(json!("00000000000000000000000000000000"))).unwrap();
    assert_eq!(q.method, Method::Status);
}

#[test]
fn no_cleanup_tags() {
    let src = sources();
    for banned in ["Live", "Held", "Queued", "Gone"] {
        assert!(!src.contains(&format!("{banned} =")), "{banned} state byte");
    }
    assert!(Method::from_name("cleanup").is_none());
}

#[test]
fn no_traced_prefix_tag() {
    assert_eq!(rpc_of(&parse_request(b"\x00traced{}").unwrap_err().0), "Failed");
}

#[test]
fn no_telemetry_beside_reply() {
    let e = echo_server(MAX_FRAME);
    let out = roundtrip(&e.sock, &[format!("{}\n", status_req(json!(1))).as_bytes()]);
    let keys: Vec<&String> = out[0].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["data", "id", "ok"]);
}

#[test]
fn no_auth_observation_identity() {
    assert!(Method::from_name("observe").is_none());
    assert!(Method::from_name("worker").is_none());
    assert!(Method::from_name("status").is_some());
}

#[test]
fn no_auth_diagnostic_sink() {
    assert!(Method::from_name("diagnostics").is_none());
    assert!(!sources().contains("DiagnosticSink"));
}

#[test]
fn no_auth_traced_diagnostic() {
    // The connection is a plain unix stream.
    let e = echo_server(MAX_FRAME);
    let meta = std::fs::symlink_metadata(&e.sock).unwrap();
    use std::os::unix::fs::FileTypeExt;
    assert!(meta.file_type().is_socket());
}

#[test]
fn no_auth_traced_scope() {
    let r = Reply::error(&json!(1), &StrataError::new(Code::Busy, "b")).0;
    let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["error", "id", "ok"]);
    let ek: Vec<&String> = r["error"].as_object().unwrap().keys().collect();
    assert_eq!(ek, ["code", "message"]);
}

#[test]
fn confirm_is_not_channel_auth() {
    // A credential belongs to the action, not the connection: nothing in
    // the envelope or the socket carries one.
    let v = json!({ "id": 1, "method": "push", "params": { "repo": "tool" }, "confirm": "t" });
    assert_eq!(rpc_of(&rejected(v)), "Failed");
    assert!(!sources().contains("peer_cred"));
}

#[test]
fn same_machine_unix_socket() {
    let home = Path::new("/Users/someone");
    let dir = strata_daemon::config_dir_for_home(home);
    assert_eq!(dir, Path::new("/Users/someone/Library/Application Support/git-strata"));
    assert_eq!(strata_daemon::socket_path(&dir), dir.join("daemon.sock"));
    assert!(!sources().contains("daemonHost"));
}

#[test]
fn no_container_daemon_host() {
    let src = sources();
    assert!(!src.contains("LAYERFS_DAEMON_HOST"));
    assert!(!src.contains("127.0.0.1"));
    assert!(!src.contains("localhost"));
}

#[test]
fn readiness_is_socket_connect() {
    let dir = tempfile::Builder::new().prefix("sd-").tempdir_in("/tmp").unwrap();
    let sock = dir.path().join("daemon.sock");
    assert!(!rpc::ready(&sock, Duration::from_millis(50)));
    let _l = UnixListener::bind(&sock).unwrap();
    assert!(rpc::ready(&sock, Duration::from_secs(1)));
}

#[test]
fn no_container_archive_of_the_binary() {
    let src = sources();
    for banned in ["tar::", "Archive", "docker", "container"] {
        assert!(!src.contains(banned), "{banned}");
    }
}

#[test]
fn rpc_reject_variants() {
    assert_eq!(rpc_of(&Reply::rejected(&json!(1), RpcReject::Failed, "m").0), "Failed");
    assert_eq!(rpc_of(&Reply::rejected(&json!(1), RpcReject::UnknownMethod, "m").0), "UnknownMethod");
}
