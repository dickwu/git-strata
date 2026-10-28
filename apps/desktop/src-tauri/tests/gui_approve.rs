//! gui-approve (D1 Option A, D2=b): the `deps_approve` write against a
//! fake daemon on a temporary socket (never the live one). The requests
//! sent, the `[]` refusal before any I/O, the single-flight slot, no
//! deadline, and the broken-connection error. No real proposal is touched.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use strata_desktop_lib::daemon::{Daemon, Read, approve_params};

/// What the fake does with a request after reading it.
#[derive(Clone, Copy)]
enum Mode {
    /// Answer with a one-session `moved` reply.
    Moved,
    /// Hold the connection until the test sends on `release`, then answer.
    HoldThenMoved,
    /// Close the connection without answering.
    Close,
}

struct Fake {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    seen: Arc<Mutex<Vec<Value>>>,
    connects: Arc<Mutex<usize>>,
    release: mpsc::Sender<()>,
}

fn moved_reply(id: &Value, proposal: &Value) -> Value {
    json!({ "id": id, "ok": true, "data": {
        "results": [{ "session": "S1", "kind": "moved", "report": {
            "session": "S1", "repo": "web",
            "from": { "repo": "web", "tree": "t1", "deps": "d1" }, "from_head": "a1",
            "to": { "repo": "web", "tree": "t1", "deps": "d2" }, "head": "b2",
            "dropped": [], "conflicts": [], "lww": [], "uncarried": [] } }],
        "proposal": { "id": proposal, "state": { "kind": "Applied" } } } })
}

impl Fake {
    fn start(mode: Mode) -> Fake {
        let dir = tempfile::Builder::new()
            .prefix("gsapv")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = dir.path().join("d.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let connects = Arc::new(Mutex::new(0));
        let (release, wait) = mpsc::channel::<()>();
        let wait = Arc::new(Mutex::new(wait));
        let (s2, c2) = (seen.clone(), connects.clone());
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { return };
                *c2.lock().unwrap() += 1;
                let mut line = String::new();
                if BufReader::new(&conn).read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
                }
                let req: Value = serde_json::from_str(&line).unwrap();
                s2.lock().unwrap().push(req.clone());
                let mut conn: UnixStream = conn;
                if req["method"] != "deps_approve" {
                    // A read while an approve is held: answer at once.
                    let r = json!({ "id": req["id"], "ok": true, "data": { "proposals": [] } });
                    let _ = conn.write_all(format!("{r}\n").as_bytes());
                    continue;
                }
                match mode {
                    Mode::Close => drop(conn),
                    Mode::Moved => {
                        let r = moved_reply(&req["id"], &req["params"]["proposal"]);
                        let _ = conn.write_all(format!("{r}\n").as_bytes());
                    }
                    Mode::HoldThenMoved => {
                        let wait = wait.clone();
                        std::thread::spawn(move || {
                            let _ = wait.lock().unwrap().recv();
                            let r = moved_reply(&req["id"], &req["params"]["proposal"]);
                            let _ = conn.write_all(format!("{r}\n").as_bytes());
                        });
                    }
                }
            }
        });
        Fake {
            _dir: dir,
            socket,
            seen,
            connects,
            release,
        }
    }

    fn requests(&self) -> Vec<Value> {
        self.seen.lock().unwrap().clone()
    }
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// D2=b: exactly `{proposal, sessions: null}`; no `planned`, nothing that
/// could stand for the human gate. The reply comes back unchanged.
#[test]
fn approve_sends_the_proposal_and_null_sessions_only() {
    let fake = Fake::start(Mode::Moved);
    let d = Daemon::new(fake.socket.clone());
    let got = rt().block_on(d.deps_approve(22, None)).unwrap();
    assert_eq!(got["results"][0]["kind"], "moved");
    assert_eq!(got["proposal"]["id"], 22);
    let reqs = fake.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0]["method"], "deps_approve");
    assert_eq!(
        reqs[0]["params"],
        json!({ "proposal": 22, "sessions": null })
    );
    let keys: Vec<&String> = reqs[0]["params"].as_object().unwrap().keys().collect();
    assert_eq!(keys.len(), 2, "{keys:?}");
}

/// A non-empty list is sent as is (S2 will use it; D4).
#[test]
fn approve_sends_a_named_list_as_is() {
    let fake = Fake::start(Mode::Moved);
    let d = Daemon::new(fake.socket.clone());
    rt().block_on(d.deps_approve(5, Some(vec!["S1".into(), "S2".into()])))
        .unwrap();
    assert_eq!(
        fake.requests()[0]["params"],
        json!({ "proposal": 5, "sessions": ["S1", "S2"] })
    );
}

/// gui.md §5.4 item 3: `[]` is refused before any socket I/O; so are id 0
/// and an empty session id.
#[test]
fn approve_refuses_bad_input_before_any_io() {
    let fake = Fake::start(Mode::Moved);
    let d = Daemon::new(fake.socket.clone());
    let rt = rt();
    for (proposal, sessions, reason) in [
        (3, Some(vec![]), "empty_sessions"),
        (0, None, "bad_proposal"),
        (3, Some(vec!["".to_string()]), "bad_session"),
    ] {
        let e = rt.block_on(d.deps_approve(proposal, sessions)).unwrap_err();
        assert_eq!(e.code, "InvalidInput");
        assert_eq!(e.reason(), Some(reason));
    }
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(*fake.connects.lock().unwrap(), 0);
    assert!(fake.requests().is_empty());
    assert!(approve_params(1, Some(vec![])).is_err());
}

/// No deadline on the write (the prompt alone may wait 60 s), and one
/// approve at a time: a second is refused while the first is unanswered;
/// reads keep their own slots.
#[test]
fn approve_has_no_deadline_and_one_flight() {
    let fake = Fake::start(Mode::HoldThenMoved);
    let d = Arc::new(Daemon::new(fake.socket.clone()).with_deadline(Duration::from_millis(50)));
    let rt = rt();
    let first = {
        let d = d.clone();
        rt.spawn(async move { d.deps_approve(7, None).await })
    };
    // Wait for the request to reach the fake.
    for _ in 0..200 {
        if !fake.requests().is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(fake.requests().len(), 1);
    let e = rt.block_on(d.deps_approve(7, None)).unwrap_err();
    assert_eq!(e.code, "Busy");
    assert_eq!(e.reason(), Some("request_in_flight"));
    // Well past the read deadline, the approve is still waiting.
    std::thread::sleep(Duration::from_millis(300));
    assert!(!first.is_finished());
    // A read is not blocked by the held approve.
    let r = rt.block_on(d.read(Read::DepsList)).unwrap();
    assert_eq!(r, json!({ "proposals": [] }));
    fake.release.send(()).unwrap();
    let got = rt.block_on(first).unwrap().unwrap();
    assert_eq!(got["results"][0]["session"], "S1");
    // The slot is free again.
    fake.release.send(()).unwrap();
    let again = rt.block_on(d.deps_approve(8, None)).unwrap();
    assert_eq!(again["proposal"]["id"], 8);
}

/// The connection broke after the request was written: the daemon may
/// have acted, so the error is `Unknown`, never a plain failure.
#[test]
fn approve_broken_connection_is_unknown() {
    let fake = Fake::start(Mode::Close);
    let d = Daemon::new(fake.socket.clone());
    let e = rt().block_on(d.deps_approve(9, None)).unwrap_err();
    assert_eq!(e.code, "Unknown", "{e:?}");
    assert_eq!(fake.requests().len(), 1);
}
