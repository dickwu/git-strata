//! Status bar, daemon side (status-bar design §3.2-§3.4, §11.4-§11.6):
//! `daemon_status` names the daemon (`version`, `pid`, `started`), keeps the
//! `Busy` replies requests got (`busy`), and reports the data volume
//! (`volume`) and the size scan's counters (`scan`). Temp-dir fixtures and an
//! in-process daemon only.
mod support;

use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};
use strata_daemon::handlers::{Server, Trackers};
use strata_daemon::startup::Daemon;
use support::close::{self, call, ok};
use support::*;

const WAIT: Duration = Duration::from_secs(30);

fn status(server: &Server<'_, '_>) -> Value {
    ok(call(server, "daemon_status", json!({ "verify": false })))
}

fn idle_busy() -> Value {
    json!({ "total": 0, "recent": [], "truncated": false })
}

/// Reply-side counting (§3.4): a held repo lock and session queue place,
/// probed by zero-wait background takes and read by requests, count
/// nothing; a client write whose first take waits past the lock wait counts
/// once, with the holder, and is still listed after the hold is gone.
#[test]
fn busy_counted_at_reply_only() {
    let fx = Dfx::new(); // lock wait 300 ms
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_only(&server, &["tool"]);
    let tool = repo_name("tool");
    assert_eq!(status(&server)["busy"], idle_busy());

    let refused = std::thread::scope(|sc| {
        let (held_tx, held_rx) = mpsc::channel();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let (core, tool, s) = (&core, &tool, &s);
        // Held as catch-up and reconcile hold them: zero-wait takes, no request.
        let holder = sc.spawn(move || {
            core.locks.try_repo(tool, "catch_up", || {
                core.locks
                    .try_session(s, "reconcile", || {
                        held_tx.send(()).unwrap();
                        let _ = go_rx.recv();
                        Ok(())
                    })
                    .expect("the queue place is free")
            })
        });
        held_rx.recv_timeout(WAIT).unwrap();
        // Background probes miss both takes: Busy to the prober, no reply.
        let probe = core
            .locks
            .try_repo(tool, "periodic_fetch", || Ok(()))
            .unwrap_err();
        assert_eq!(reason(&probe), Some("lock_wait"));
        assert!(core.locks.try_session(s, "reconcile", || Ok(())).is_none());
        // Reads are answered meanwhile.
        ok(call(
            &server,
            "status",
            json!({ "cwd": null, "session": null, "all": true }),
        ));
        assert_eq!(status(&server)["busy"], idle_busy());
        // A client write: its first take waits the lock wait, then Busy.
        let refused = call(
            &server,
            "checkpoint",
            json!({ "session": s, "repo": "tool", "reason": { "kind": "Manual", "message": "m" }, "paths": [] }),
        );
        go_tx.send(()).unwrap();
        holder.join().unwrap().unwrap();
        refused
    });
    assert_eq!(refused["error"]["code"], "Busy", "{refused}");
    assert_eq!(
        refused["error"]["detail"]["reason"], "lock_wait",
        "{refused}"
    );

    // The hold is gone; the event stays, as it was.
    assert!(core.locks.holds().is_empty());
    let busy = status(&server)["busy"].clone();
    assert_eq!(busy["total"], 1, "{busy}");
    assert_eq!(busy["truncated"], false, "{busy}");
    let recent = busy["recent"].as_array().unwrap();
    assert_eq!(recent.len(), 1, "{busy}");
    let e = &recent[0];
    assert_eq!(e["method"], "checkpoint", "{e}");
    assert_eq!(e["session"], s.as_str(), "{e}");
    assert_eq!(e["reason"], "lock_wait", "{e}");
    assert_eq!(e["scope"], json!({ "session": s.as_str() }), "{e}");
    assert_eq!(e["holder"], "reconcile", "{e}");
    assert_eq!(e["holder_session"], s.as_str(), "{e}");
    let at = strata_types::serde_rfc3339::parse(e["at"].as_str().unwrap()).unwrap();
    assert!(at <= SystemTime::now(), "{e}");
    // A later successful write adds nothing.
    ok(call(
        &server,
        "checkpoint",
        json!({ "session": s, "repo": "tool", "reason": { "kind": "Manual", "message": "m" }, "paths": [] }),
    ));
    assert_eq!(status(&server)["busy"]["total"], 1);
}

/// The other counted reason: a du scan of the session that does not stop
/// within the hold wait answers `rm` with `Busy du_cancel_timeout`; the
/// event's scope is that session. A refusal with another code adds nothing.
#[test]
fn busy_counts_du_cancel_timeout_and_only_busy() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let s = close::new_only(&server, &["tool"]);
    // `new` queued the session's units; a real scan would end the planted one.
    assert!(d.du.wait_idle(WAIT));
    let other = call(
        &server,
        "rm",
        json!({ "session": "s-zzzzzzzz", "discard": false }),
    );
    assert_eq!(other["ok"], false, "{other}");
    assert_ne!(other["error"]["code"], "Busy", "{other}");
    assert_eq!(status(&server)["busy"], idle_busy());

    let token = d.du.plant_stuck_scan_for_test(s.as_str());
    let refused = call(&server, "rm", json!({ "session": s, "discard": false }));
    d.du.clear_stuck_scan_for_test(s.as_str());
    assert!(
        token.load(std::sync::atomic::Ordering::SeqCst),
        "the hold cancels the scan"
    );
    assert_eq!(
        refused["error"]["detail"]["reason"], "du_cancel_timeout",
        "{refused}"
    );
    let busy = status(&server)["busy"].clone();
    assert_eq!(busy["total"], 1, "{busy}");
    let e = &busy["recent"][0];
    assert_eq!(
        (e["method"].clone(), e["reason"].clone()),
        (json!("rm"), json!("du_cancel_timeout")),
        "{e}"
    );
    assert_eq!(e["session"], s.as_str(), "{e}");
    assert_eq!(e["scope"], json!({ "session": s.as_str() }), "{e}");
    assert_eq!(
        (e["holder"].clone(), e["holder_session"].clone()),
        (Value::Null, Value::Null),
        "{e}"
    );
}

/// Identity: the crate version, this process (the daemon is in-process) and
/// the start time; FEATURES announces the fields.
#[test]
fn daemon_status_names_the_daemon() {
    let before = SystemTime::now();
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let v = status(&server);
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"), "{v}");
    assert_eq!(v["pid"], std::process::id(), "{v}");
    let started = strata_types::serde_rfc3339::parse(v["started"].as_str().unwrap()).unwrap();
    // RFC 3339 keeps nanoseconds: no rounding below `before`.
    assert!(before <= started && started <= SystemTime::now(), "{v}");
    assert!(
        v["features"]
            .as_array()
            .unwrap()
            .contains(&json!("daemon_status.activity")),
        "{v}"
    );
    assert!(v.get("api").is_none(), "no `api` (design Q11): {v}");
}

/// §3.2 / §11.6: the data root's volume, from the sampler's cache; a
/// sampler whose `statfs` fails answers `null`.
#[test]
fn volume_reported_and_null_on_error() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let end = Instant::now() + WAIT;
    let mut v = status(&server)["volume"].clone();
    while v.is_null() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
        v = status(&server)["volume"].clone();
    }
    let (free, size) = (v["free"].as_u64().unwrap(), v["size"].as_u64().unwrap());
    assert!(size > 0 && free <= size, "{v}");
    let want = strata_apfs::volume_space(&fx.config.paths.root).unwrap();
    assert_eq!(size, want.size, "{v}");
    let as_of = strata_types::serde_rfc3339::parse(v["as_of"].as_str().unwrap()).unwrap();
    assert!(as_of <= SystemTime::now(), "{v}");

    // A volume that cannot be read: null, never an error or a stale figure.
    let gone = strata_daemon::volume::Volume::start_with(
        fx.config.paths.root.join("no-such-dir"),
        Duration::from_millis(5),
        Arc::new(|p: &Path| strata_apfs::volume_space(p)),
    );
    std::thread::sleep(Duration::from_millis(50));
    assert!(gone.json().is_null());
}

/// §11.4: `daemon_status.scan` is the size scan's counters (no lock taken),
/// idle before any `du`, then following the pass `du` starts.
#[test]
fn daemon_status_scan_follows_the_du_pass() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let trackers = Trackers::new(&core);
    let server = Server::new(&d, &trackers);
    let idle = json!({ "running": false, "active": 0, "queued": 0, "done": 0, "full": false, "unresolved_n": 0 });
    assert_eq!(status(&server)["scan"], idle);
    let du = ok(call(&server, "du", json!({ "refresh": true })));
    assert!(du["scan"]["active"].is_u64(), "{du}");
    assert!(
        du["scan"]["unresolved"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "{du}"
    );
    assert!(d.du.wait_idle(WAIT));
    let scan = status(&server)["scan"].clone();
    assert_eq!(scan["running"], false, "{scan}");
    assert_eq!(
        (scan["active"].clone(), scan["queued"].clone()),
        (json!(0), json!(0)),
        "{scan}"
    );
    assert!(scan["done"].as_u64().unwrap() > 0, "{scan}");
    assert_eq!(scan["full"], false, "the full pass ended: {scan}");
    assert_eq!(scan["unresolved_n"], 0, "{scan}");
    let du = ok(call(&server, "du", json!({ "refresh": false })));
    assert!(du["scan"]["last_full"]["ended"].is_string(), "{du}");
}
