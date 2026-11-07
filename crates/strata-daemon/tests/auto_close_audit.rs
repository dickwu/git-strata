//! Design §10 row 4: session audit records on real handler paths.
//! All repositories and sidecars belong to Dfx temporary directories.
mod support;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier, Mutex, mpsc};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use strata_daemon::handlers::{Server, Trackers};
use strata_daemon::seams::set_audit_after_rotate;
use strata_daemon::startup::Daemon;
use strata_session::SessionSeam;
use strata_store::Lifecycle;
use support::close::{self, audit_lines, mark_of, opted_in};
use support::*;

const ROTATE_BYTES: usize = 1024 * 1024;
const WAIT: Duration = Duration::from_secs(15);

fn pad_sessions_file(fx: &Dfx) -> PathBuf {
    let path = fx.config_dir.join("sessions-audit.jsonl");
    // A valid whitespace-only line: the next event must rotate, while readers
    // can still parse every nonblank line in the retained file.
    let mut padding = vec![b' '; ROTATE_BYTES - 101];
    padding.push(b'\n');
    fs::write(&path, padding).unwrap();
    path
}

fn retained_lines(path: &Path) -> Vec<Value> {
    let rotated = path.with_file_name("sessions-audit.jsonl.1");
    [rotated.as_path(), path]
        .into_iter()
        .flat_map(|p| {
            fs::read_to_string(p)
                .unwrap()
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| serde_json::from_str(line).unwrap())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Drop inside the thread scope so failed assertions cannot strand an append.
struct ReleaseAudit {
    path: PathBuf,
    go: mpsc::Sender<()>,
}
impl Drop for ReleaseAudit {
    fn drop(&mut self) {
        set_audit_after_rotate(&self.path, None);
        let _ = self.go.send(());
    }
}

fn park_rotation(path: &Path) -> (mpsc::Receiver<()>, ReleaseAudit) {
    let (parked_tx, parked_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel();
    let go_rx = Mutex::new(go_rx);
    set_audit_after_rotate(
        path,
        Some(Arc::new(move || {
            parked_tx.send(()).unwrap();
            go_rx.lock().unwrap().recv_timeout(WAIT).unwrap();
        })),
    );
    (
        parked_rx,
        ReleaseAudit {
            path: path.to_owned(),
            go: go_tx,
        },
    )
}

fn assert_no_note_text(value: &Value) {
    match value {
        Value::Object(fields) => {
            assert!(!fields.contains_key("note"), "note text leaked: {value}");
            for child in fields.values() {
                assert_no_note_text(child);
            }
        }
        Value::Array(items) => items.iter().for_each(assert_no_note_text),
        _ => {}
    }
}

#[test]
fn auto_close_line_shape() {
    let fx = opted_in(&["tool"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);
    let secret = "private agent note must never enter the audit";
    close::ok(close::call(
        &server,
        "mark",
        json!({
            "session": s, "state": "done", "agent": "codex", "note": secret,
        }),
    ));
    fs::write(ws(&core, &s, "tool").join("new.txt"), "mine\n").unwrap();
    let out = close::ok(close::publish(&server, &core, &s, "tool", &["new.txt"]));
    assert_eq!(out["closed"], true, "{out}");
    let lines = audit_lines(&fx);
    assert_eq!(lines.len(), 1, "{lines:?}");
    let line = &lines[0];
    let keys: BTreeSet<_> = line
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        BTreeSet::from([
            "at", "event", "session", "by", "auth", "version", "trigger", "closed", "reasons",
            "detail", "rm", "mark",
        ])
    );
    strata_types::serde_rfc3339::parse(line["at"].as_str().unwrap()).unwrap();
    assert_eq!(line["event"], "auto_close");
    assert_eq!(line["session"], s.as_str());
    assert_eq!(
        line["by"],
        json!({"kind": "daemon", "pid": std::process::id()})
    );
    assert_eq!(line["auth"], "none");
    assert_eq!(line["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(line["trigger"]["method"], "publish");
    assert!(line["trigger"]["publish"].is_u64(), "{line}");
    assert!(line["trigger"]["repo"].is_null(), "{line}");
    assert_eq!(line["trigger"]["in_group"], true);
    assert_eq!(line["closed"], true);
    assert_eq!(line["reasons"], json!([]));
    assert_eq!(line["detail"], json!({}));
    assert_eq!(line["rm"]["mode"], "strict");
    let entry = &out["auto_close"]["sessions"][s.as_str()];
    assert_eq!(line["rm"]["checkpoints"], entry["rm"]["checkpoints"]);
    assert!(line["rm"]["checkpoints"]["tool"].is_number());
    assert_eq!(line["rm"]["trash"], entry["rm"]["trash"]);
    let trash = line["rm"]["trash"].as_array().unwrap();
    assert!(!trash.is_empty());
    for destination in trash {
        assert!(Path::new(destination.as_str().unwrap()).exists(), "{line}");
    }
    assert_eq!(line["mark"], json!({"written": false}));
    assert_no_note_text(line);
    assert!(!line.to_string().contains(secret));
}

#[test]
fn busy_writes_an_audit_line_and_no_mark() {
    let fx = opted_in(&["tool"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);
    fs::write(ws(&core, &s, "tool").join("new.txt"), "mine\n").unwrap();
    let cp = checkpoint(&core, &s, "tool");
    let g = group(vec![batch(&s, "tool", cp.id, &["new.txt"])]);
    d.sessions.arm(SessionSeam::PauseAt {
        phase: "auto_close.before_judge".into(),
    });
    let out = std::thread::scope(|scope| {
        let publish =
            scope.spawn(|| close::call(&server, "publish", json!({"group": g, "shown": {}})));
        // Every wait is bounded; release the session seam even on a failed assertion.
        struct Release<'a>(&'a strata_session::Sessions<'a>);
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                self.0.release();
            }
        }
        let _release = Release(&d.sessions);
        assert!(d.sessions.wait_paused_timeout(WAIT));
        core.locks
            .with_sessions(std::slice::from_ref(&s), || {
                d.sessions.release();
                // Keep the queue occupied until the hook has exhausted its wait.
                Ok(close::ok(publish.join().unwrap()))
            })
            .unwrap()
    });
    assert_eq!(out["published"][0]["push"]["kind"], "Pushed", "{out}");
    let entry = &out["auto_close"]["sessions"][s.as_str()];
    assert_eq!(entry["reasons"], json!(["busy"]), "{out}");
    assert_eq!(entry["mark"], json!({"written": false, "skipped": "busy"}));
    assert!(mark_of(&core, &s).is_none());
    assert_eq!(
        core.store.session(&s).unwrap().unwrap().lifecycle,
        Lifecycle::Active
    );
    let lines = audit_lines(&fx);
    assert_eq!(lines.len(), 1, "{lines:?}");
    let line = &lines[0];
    assert_eq!(line["session"], s.as_str());
    assert_eq!(line["event"], "auto_close");
    assert_eq!(line["closed"], false);
    assert_eq!(line["reasons"], json!(["busy"]));
    assert_eq!(line["detail"]["reason"], "lock_wait");
    assert_eq!(line["mark"], entry["mark"]);
    assert!(line["rm"].is_null());
    assert_no_note_text(line);
}

#[test]
fn two_sessions_hooks_append_concurrently() {
    let fx = opted_in(&["tool", "front"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let a = close::new_only(&server, &["tool"]);
    let b = close::new_only(&server, &["front"]);
    fs::write(ws(&core, &a, "tool").join("a.txt"), "a\n").unwrap();
    fs::write(ws(&core, &b, "front").join("b.txt"), "b\n").unwrap();
    let ca = checkpoint(&core, &a, "tool");
    let cb = checkpoint(&core, &b, "front");
    let ga = group(vec![batch(&a, "tool", ca.id, &["a.txt"])]);
    let gb = group(vec![batch(&b, "front", cb.id, &["b.txt"])]);
    let path = pad_sessions_file(&fx);
    let (parked, release) = park_rotation(&path);
    let start = Barrier::new(2);
    let (out_a, out_b) = std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            start.wait();
            close::ok(close::call(
                &server,
                "publish",
                json!({"group": ga, "shown": {}}),
            ))
        });
        let second = scope.spawn(|| {
            start.wait();
            close::ok(close::call(
                &server,
                "publish",
                json!({"group": gb, "shown": {}}),
            ))
        });
        let release = release;
        parked
            .recv_timeout(WAIT)
            .expect("one hook rotates the sessions sidecar");
        // Both hooks finish teardown while the first append holds the audit
        // mutex. Thus this exercises overlapping hooks, not sequential appends.
        let deadline = Instant::now() + WAIT;
        while [&a, &b]
            .iter()
            .any(|s| core.store.session(s).unwrap().unwrap().lifecycle != Lifecycle::Removed)
        {
            assert!(
                Instant::now() < deadline,
                "both sessions must reach teardown"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !path.exists(),
            "the waiting writer cannot recreate the file before the rotator"
        );
        drop(release);
        (first.join().unwrap(), second.join().unwrap())
    });
    assert_eq!(out_a["closed"], true, "{out_a}");
    assert_eq!(out_b["closed"], true, "{out_b}");
    let lines = retained_lines(&path);
    assert_eq!(lines.len(), 2, "{lines:?}");
    for session in [&a, &b] {
        assert_eq!(
            lines
                .iter()
                .filter(|l| l["event"] == "auto_close" && l["session"] == session.as_str())
                .count(),
            1
        );
    }
    assert!(
        lines
            .iter()
            .all(|l| l["closed"] == true && l["trigger"]["in_group"] == true)
    );
}

#[test]
fn audit_append_holds_no_queue() {
    let fx = opted_in(&["tool"]);
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new(&server);
    fs::write(ws(&core, &s, "tool").join("published.txt"), "published\n").unwrap();
    fs::write(ws(&core, &s, "tool").join("remaining.txt"), "unpublished\n").unwrap();
    let cp = checkpoint(&core, &s, "tool");
    let g = group(vec![batch(&s, "tool", cp.id, &["published.txt"])]);
    let path = pad_sessions_file(&fx);
    let (parked, release) = park_rotation(&path);
    let out = std::thread::scope(|scope| {
        let publish =
            scope.spawn(|| close::call(&server, "publish", json!({"group": g, "shown": {}})));
        let release = release;
        parked
            .recv_timeout(WAIT)
            .expect("the hook parks inside audit append");
        assert!(
            mark_of(&core, &s).is_some(),
            "the refusal already wrote its done mark"
        );
        let marked = close::ok(close::call(
            &server,
            "mark",
            json!({"session": s, "state": "active"}),
        ));
        assert_eq!(marked["mark"], json!({"state": "active"}));
        assert!(
            mark_of(&core, &s).is_none(),
            "mark takes the same queue and must succeed before append resumes"
        );
        assert!(!path.exists(), "audit append is still parked");
        drop(release);
        close::ok(publish.join().unwrap())
    });
    assert_eq!(out["closed"], false, "{out}");
    assert_eq!(
        out["auto_close"]["sessions"][s.as_str()]["reasons"],
        json!(["unpublished"])
    );
    assert_eq!(
        core.store.session(&s).unwrap().unwrap().lifecycle,
        Lifecycle::Active
    );
    assert!(
        mark_of(&core, &s).is_none(),
        "no late rewrite of the cleared mark"
    );
    let lines = retained_lines(&path);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["session"], s.as_str());
    assert_eq!(lines[0]["reasons"], json!(["unpublished"]));
    assert_no_note_text(&lines[0]);
}
