//! P1 back end against a fake daemon on a temporary socket (never the
//! live one): the requests the GUI sends, the error shapes it returns,
//! the per-read deadline and single-flight guard, the snapshot the menu
//! bar is drawn from, and the window behaviour behind `lifecycle::Shell`.

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use strata_desktop_lib::daemon::{Daemon, Read};
use strata_desktop_lib::lifecycle::{self, Shell};
use strata_desktop_lib::socket::socket_for_home;
use strata_desktop_lib::state::AppState;
use strata_desktop_lib::tray_model::{self, Icon, MAX_WAITING, Snapshot};

fn fixture(name: &str) -> Value {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/p1")
        .join(format!("{name}.json"));
    serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap()
}

#[derive(Clone, Copy)]
enum Mode {
    /// Answer from the fixtures.
    Fixtures,
    /// Read the request, then hold the connection without answering
    /// until the test drops the fake.
    Silent,
    /// Answer every request with an `ok:false` error.
    Refuse,
}

/// A json-lines daemon on a socket under /tmp (short enough for
/// sun_path). Records every request it reads.
struct Fake {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    seen: Arc<Mutex<Vec<Value>>>,
    held: Arc<Mutex<Vec<UnixStream>>>,
}

impl Fake {
    fn start(mode: Mode) -> Fake {
        let dir = tempfile::Builder::new()
            .prefix("gsgui")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = dir.path().join("d.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let held = Arc::new(Mutex::new(Vec::new()));
        let (s2, h2) = (seen.clone(), held.clone());
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { return };
                let mut line = String::new();
                if BufReader::new(&conn).read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
                }
                let req: Value = serde_json::from_str(&line).unwrap();
                s2.lock().unwrap().push(req.clone());
                let reply = match mode {
                    Mode::Silent => {
                        h2.lock().unwrap().push(conn);
                        continue;
                    }
                    Mode::Refuse => json!({ "id": req["id"], "ok": false, "error": {
                        "code": "NoMatch", "message": "no such session", "detail": { "reason": "session_unknown" } } }),
                    Mode::Fixtures => {
                        json!({ "id": req["id"], "ok": true, "data": fixture(fixture_for(&req)) })
                    }
                };
                let _ = conn.write_all(format!("{reply}\n").as_bytes());
            }
        });
        Fake {
            _dir: dir,
            socket,
            seen,
            held,
        }
    }

    fn requests(&self) -> Vec<Value> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.held.lock().unwrap().clear();
    }
}

fn fixture_for(req: &Value) -> &'static str {
    match (
        req["method"].as_str().unwrap(),
        req["params"]["all"].as_bool(),
    ) {
        ("daemon_status", _) => "daemon_status",
        ("status", Some(true)) => "status_all",
        ("status", _) => "status_session",
        ("du", _) => "du",
        ("deps_list", _) => "deps_list",
        (m, _) => panic!("unexpected method {m}"),
    }
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn all_reads() -> Vec<(&'static str, Read)> {
    vec![
        ("daemon_status", Read::DaemonStatus),
        ("status_all", Read::Sessions),
        ("status_session", Read::Session("S1".into())),
        ("du", Read::Du),
        ("deps_list", Read::DepsList),
    ]
}

/// The params sent are exactly the ones crates/git-strata/tests/
/// gui_fixtures.rs sends to the real daemon, and each reply comes back
/// unchanged.
#[test]
fn gui_reads_send_the_checked_requests() {
    let fake = Fake::start(Mode::Fixtures);
    let d = Daemon::new(fake.socket.clone());
    let expected = fixture("requests");
    rt().block_on(async {
        for (name, r) in all_reads() {
            let got = d.read(r).await.unwrap();
            assert_eq!(got, fixture(name), "{name}");
        }
    });
    let reqs = fake.requests();
    assert_eq!(reqs.len(), 5);
    let expected = expected.as_array().unwrap();
    assert_eq!(expected.len(), 5);
    for (((name, _), req), want) in all_reads().iter().zip(&reqs).zip(expected) {
        assert_eq!(want["fixture"], *name);
        let mut want = want.clone();
        if want["params"]["session"] == "$S1" {
            want["params"]["session"] = json!("S1");
        }
        assert_eq!(req["method"], want["method"], "{name}");
        assert_eq!(req["params"], want["params"], "{name}");
    }
}

#[test]
fn gui_no_socket_is_daemon_down() {
    let dir = tempfile::Builder::new()
        .prefix("gsgui")
        .tempdir_in("/tmp")
        .unwrap();
    let d = Daemon::new(dir.path().join("absent.sock"));
    let e = rt().block_on(d.read(Read::DaemonStatus)).unwrap_err();
    assert_eq!(e.code, "Busy");
    assert_eq!(e.reason(), Some("daemon_down"));
    // The error the front end gets has the cli envelope's shape.
    let v = serde_json::to_value(&e).unwrap();
    assert!(
        v["code"].is_string() && v["message"].is_string() && v["detail"].is_object(),
        "{v}"
    );
}

#[test]
fn gui_daemon_error_passes_through() {
    let fake = Fake::start(Mode::Refuse);
    let d = Daemon::new(fake.socket.clone());
    let e = rt()
        .block_on(d.read(Read::Session("S9".into())))
        .unwrap_err();
    assert_eq!(e.code, "NoMatch");
    assert_eq!(e.reason(), Some("session_unknown"));
}

/// A daemon that accepts but never answers: the read gives up at its
/// deadline, the same kind is refused while the first connection still
/// waits (no second connection), other kinds still go out, and the slot
/// frees once the daemon closes the connection.
#[test]
fn gui_timeout_and_single_flight() {
    let fake = Fake::start(Mode::Silent);
    let d = Daemon::new(fake.socket.clone()).with_deadline(Duration::from_millis(300));
    let rt = rt();
    let e = rt.block_on(d.read(Read::DaemonStatus)).unwrap_err();
    assert_eq!(e.reason(), Some("daemon_timeout"));
    assert_eq!(e.detail["seconds"], 5, "reports the real deadline");

    let e = rt.block_on(d.read(Read::DaemonStatus)).unwrap_err();
    assert_eq!(e.reason(), Some("request_in_flight"));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        fake.requests().len(),
        1,
        "no second connection for the same read"
    );

    // Sessions and one session are separate slots.
    let e = rt.block_on(d.read(Read::Sessions)).unwrap_err();
    assert_eq!(e.reason(), Some("daemon_timeout"));
    let e = rt.block_on(d.read(Read::Session("S1".into()))).unwrap_err();
    assert_eq!(e.reason(), Some("daemon_timeout"));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(fake.requests().len(), 3);

    // The daemon closes the stuck connections: the slot frees.
    fake.held.lock().unwrap().clear();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match rt.block_on(d.read(Read::DaemonStatus)) {
            Err(e)
                if e.reason() == Some("request_in_flight")
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(20))
            }
            Err(e) => {
                assert_eq!(
                    e.reason(),
                    Some("daemon_timeout"),
                    "a fresh request went out"
                );
                break;
            }
            Ok(v) => panic!("silent daemon answered {v}"),
        }
    }
    assert_eq!(fake.requests().len(), 4);
    // The fourth read's thread still waits on the silent daemon; a
    // runtime drop would wait for it.
    rt.shutdown_background();
}

#[test]
fn gui_socket_path_from_home() {
    assert_eq!(
        socket_for_home(Path::new("/Users/someone")),
        PathBuf::from("/Users/someone/Library/Application Support/git-strata/daemon.sock")
    );
}

#[test]
fn gui_state_records_liveness_and_lists() {
    let fake = Fake::start(Mode::Fixtures);
    let state = AppState::new(Some(Daemon::new(fake.socket.clone())));
    let changes = Arc::new(Mutex::new(0));
    let c2 = changes.clone();
    state.set_on_change(move |_| *c2.lock().unwrap() += 1);
    assert_eq!(state.snapshot().up, None);
    let rt = rt();
    rt.block_on(state.read(Read::Sessions)).unwrap();
    rt.block_on(state.read(Read::DepsList)).unwrap();
    let s = state.snapshot();
    assert_eq!(s.up, Some(true));
    assert_eq!(s.sessions, Some(fixture("status_all")));
    assert_eq!(s.deps, Some(fixture("deps_list")));
    assert_eq!(*changes.lock().unwrap(), 2);

    // A refused request says nothing about liveness.
    let refused = Err(strata_desktop_lib::error::GuiError::in_flight("status"));
    state.record(&Read::Sessions, &refused, SystemTime::now());
    assert_eq!(state.snapshot().up, Some(true));
    assert_eq!(*changes.lock().unwrap(), 2);

    drop(fake);
    let dir = tempfile::Builder::new()
        .prefix("gsgui")
        .tempdir_in("/tmp")
        .unwrap();
    let down = AppState::new(Some(Daemon::new(dir.path().join("absent.sock"))));
    rt.block_on(down.read(Read::DaemonStatus)).unwrap_err();
    assert_eq!(down.snapshot().up, Some(false));

    let no_home = AppState::new(None);
    let e = rt.block_on(no_home.read(Read::DaemonStatus)).unwrap_err();
    assert_eq!(e.reason(), Some("no_home"));
}

fn snap(sessions: Value, deps: Value, age: Duration, now: SystemTime) -> Snapshot {
    Snapshot {
        up: Some(true),
        sessions: Some(sessions),
        deps: Some(deps),
        updated: Some(now - age),
    }
}

#[test]
fn gui_tray_status_lines() {
    let now = SystemTime::now();
    let m = tray_model::model(&Snapshot::default(), now);
    assert_eq!(
        (m.icon, m.status_line.as_str()),
        (Icon::Down, "Checking the daemon…")
    );

    let down = Snapshot {
        up: Some(false),
        updated: Some(now),
        ..Default::default()
    };
    let m = tray_model::model(&down, now);
    assert_eq!(
        (m.icon, m.status_line.as_str()),
        (Icon::Down, "Daemon isn't running")
    );

    // The fixtures: two active sessions, a proposal still AwaitingPublish
    // (the session has not published yet: nothing waits for the human).
    let fresh = snap(
        fixture("status_all"),
        fixture("deps_list"),
        Duration::from_secs(5),
        now,
    );
    let m = tray_model::model(&fresh, now);
    assert_eq!(
        (m.icon, m.status_line.as_str()),
        (Icon::Normal, "Daemon running · 2 sessions")
    );
    assert!(m.waiting.is_empty());

    let mut one = fixture("status_all");
    one["sessions"].as_array_mut().unwrap().truncate(1);
    let m = tray_model::model(
        &snap(one, json!({ "proposals": [] }), Duration::ZERO, now),
        now,
    );
    assert_eq!(m.status_line, "Daemon running · 1 session");

    let stale = snap(
        fixture("status_all"),
        fixture("deps_list"),
        Duration::from_secs(180),
        now,
    );
    let m = tray_model::model(&stale, now);
    assert_eq!(
        (m.icon, m.status_line.as_str()),
        (Icon::Down, "Updated 3 min ago")
    );
}

const SECRETS: [&str; 6] = [
    "SECRET-TICKET",
    "SECRET-MESSAGE",
    "SECRET-PATH",
    "SECRET-REASON",
    "SECRET-SESSION",
    "SECRET-CONFLICT",
];

/// G7: the menu shows kind, number and repo only, however much agent
/// text the replies carry.
#[test]
fn gui_tray_no_agent_text() {
    let mut sessions = fixture("status_all");
    for s in sessions["sessions"].as_array_mut().unwrap() {
        s["status"]["manifest"]["ticket"] = json!("SECRET-TICKET");
        s["status"]["manifest"]["id"] = json!("SECRET-SESSION");
        s["status"]["manifest"]["root"] = json!("/SECRET-PATH");
        for c in s["checkpoints"].as_array_mut().unwrap() {
            c["reason"]["message"] = json!("SECRET-MESSAGE");
        }
        for ws in s["status"]["workspaces"]
            .as_object_mut()
            .unwrap()
            .values_mut()
        {
            ws["conflicts"] = json!([{ "path": "SECRET-CONFLICT/a.rs", "message": "SECRET-MESSAGE" }, "SECRET-PATH/b.rs"]);
        }
    }
    let mut deps = fixture("deps_list");
    let mut extra = Vec::new();
    for i in 0..4 {
        let mut p = deps["proposals"][0].clone();
        p["id"] = json!(10 + i);
        p["state"] = json!({ "kind": "Pending" });
        p["reason"] = json!("SECRET-REASON");
        p["origin"]["session"] = json!("SECRET-SESSION");
        extra.push(p);
    }
    deps["proposals"].as_array_mut().unwrap().extend(extra);

    let now = SystemTime::now();
    let m = tray_model::model(&snap(sessions, deps, Duration::ZERO, now), now);
    assert_eq!(m.icon, Icon::Pending);
    // 4 pending proposals + 4 workspaces with conflicts; the
    // AwaitingPublish one is not waiting for the human.
    assert_eq!(m.waiting.len(), MAX_WAITING);
    assert_eq!(m.more, 3);
    let labels: Vec<_> = m.waiting.iter().map(|w| w.label()).collect();
    assert_eq!(labels[0], "Deps proposal 10 · tool");
    assert_eq!(labels[4], "2 conflicts · tool");
    let mut text = vec![m.status_line.clone()];
    text.extend(labels);
    text.extend(m.waiting.iter().map(|w| w.id()));
    for t in &text {
        for s in SECRETS {
            assert!(!t.contains(s), "{t:?} leaks {s}");
        }
        assert!(!t.contains('/'), "{t:?} has a path");
    }
}

#[derive(Default)]
struct FakeShell(RefCell<Vec<&'static str>>);

impl Shell for FakeShell {
    fn show_window(&self) {
        self.0.borrow_mut().push("show");
    }
    fn unminimize_window(&self) {
        self.0.borrow_mut().push("unminimize");
    }
    fn focus_window(&self) {
        self.0.borrow_mut().push("focus");
    }
    fn hide_window(&self) {
        self.0.borrow_mut().push("hide");
    }
    fn set_regular(&self, regular: bool) {
        self.0
            .borrow_mut()
            .push(if regular { "regular" } else { "accessory" });
    }
}

/// A second launch only brings the one window back (never a new one),
/// with its Dock icon; closing hides to the menu bar.
#[test]
fn gui_single_instance() {
    let s = FakeShell::default();
    lifecycle::bring_to_front(&s);
    assert_eq!(*s.0.borrow(), ["regular", "show", "unminimize", "focus"]);
    s.0.borrow_mut().clear();
    lifecycle::hide_to_tray(&s);
    assert_eq!(*s.0.borrow(), ["hide", "accessory"]);
}
