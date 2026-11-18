//! gui.md §8.1 P1: the read replies the desktop app is built against.
//!
//! The desktop crate may not depend on strata-daemon, so its tests run a
//! fake daemon fed from `apps/desktop/src-tauri/tests/fixtures/p1/`. This
//! test keeps those fixtures honest: it starts the real in-process daemon
//! (tests/support/live.rs, temp HOME and socket, never the live one),
//! sends each request in `requests.json` exactly as the desktop sends it,
//! and checks every field of the committed fixture is still in the reply
//! with the same JSON type, and that the fixture has every key of [`added`]
//! the reply has.
//!
//! Update the fixtures (after a deliberate reply change) with
//! `GUI_FIXTURES=write cargo test -p git-strata --test gui_fixtures`. A
//! committed fixture is not rewritten: the desktop's own tests read some of
//! its values as an older daemon's reply (`du` without `create_ms`,
//! `daemon_status` without `features`). `write` keeps every committed value
//! and adds each key of [`added`] that the reply has and the fixture lacks;
//! a fixture file that does not exist yet is written whole. Replies only
//! grow, so a reply change lists its new keys in [`added`].

mod support;

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use strata_client::Kind;
use support::live::{Live, write};

const NEW_DEP: &str = "{\n  \"name\": \"tool\",\n  \"version\": \"1.0.0\",\n  \"dependencies\": { \"x\": \"^1.0.0\", \"y\": \"^2.0.0\" }\n}\n";

#[test]
fn desktop_drain_client_round_trips_against_the_daemon_handler() {
    use std::time::Duration;
    let live = Live::with_deps(&["tool"], &[], &[]);
    live.serve(|_| {
        let socket = live.home.socket();
        let budget = Duration::from_secs(10);
        let status = || {
            strata_client::call(
                &socket,
                "daemon_status",
                json!({"verify":false}),
                Kind::Read,
            )
            .unwrap()
        };
        let before = status();
        strata_client::require_feature(&before, "daemon.drain").unwrap();
        let id = strata_client::drain_acquire(&socket, budget, |status| {
            status["pid"] == before["pid"] && status["started"] == before["started"]
        })
        .unwrap();
        assert_eq!(
            strata_client::drain_on(&socket, Some(&id), budget).unwrap(),
            id
        );
        assert_eq!(status()["draining"]["lease_id"], id);
        strata_client::drain_seal(&socket, &id, "hold", budget).unwrap();
        assert_eq!(status()["draining"]["then"], "hold");
        let wrong = strata_client::drain_cancel(&socket, "wrong", budget).unwrap_err();
        assert_eq!(wrong.detail.unwrap()["reason"], "lease_lost");
        strata_client::drain_cancel(&socket, &id, budget).unwrap();
        assert_eq!(status()["draining"], Value::Null);
    });
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/desktop/src-tauri/tests/fixtures/p1")
}

/// The version of the desktop app built against the fixtures.
fn app_version() -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/desktop/package.json");
    let package: Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
    package["version"]
        .as_str()
        .expect("package.json version")
        .to_owned()
}

/// The keys `write` adds (fixture, path, value), all from the status-bar
/// work (feature `daemon_status.activity`; the `volume.*` names from
/// `daemon_status.volume_name`). The values are fixed so an
/// update is the same on every machine and run: machine-specific ones
/// (`pid`, `started`, `volume`; `version` is the app's, so the pair reads
/// as matched) and timing-specific ones (work in flight, Busy replies, scan
/// progress), the latter as a quiet daemon's in `daemon_status` and as the
/// committed `du` reply's running pass.
fn added() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        ("daemon_status", "version", json!(app_version())),
        ("daemon_status", "pid", json!(4242)),
        ("daemon_status", "started", json!("2026-01-01T00:00:00Z")),
        ("daemon_status", "activity", json!([])),
        ("daemon_status", "draining", Value::Null),
        ("daemon_status", "inflight", json!([])),
        (
            "daemon_status",
            "store_schema",
            json!(strata_types::STORE_SCHEMA),
        ),
        (
            "daemon_status",
            "busy",
            json!({ "total": 0, "recent": [], "truncated": false }),
        ),
        (
            "daemon_status",
            "volume",
            json!({ "free": 1_200_000_000_000u64, "size": 2_000_000_000_000u64, "as_of": "2026-01-01T00:00:00Z" }),
        ),
        (
            "daemon_status",
            "volume.path",
            json!("/Volumes/Work/.strata"),
        ),
        ("daemon_status", "volume.mount", json!("/Volumes/Work")),
        ("daemon_status", "volume.name", json!("Work")),
        (
            "daemon_status",
            "scan",
            json!({ "running": false, "active": 0, "queued": 0, "done": 0, "full": false, "unresolved_n": 0 }),
        ),
        ("du", "scan.active", json!(4)),
        ("du", "scan.unresolved", json!([])),
    ]
}

/// The value at a dotted `path` of objects.
fn at<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(v, |v, k| v.get(k))
}

/// The fixture `write` leaves for `name`: the committed one plus the keys
/// of [`added`] it lacks and `reply` has, or the whole reply (with those
/// keys' fixed values) when nothing is committed yet.
fn updated(name: &str, committed: Option<Value>, reply: &Value) -> Value {
    let fresh = committed.is_none();
    let mut out = committed.unwrap_or_else(|| reply.clone());
    for (_, path, value) in added().into_iter().filter(|(n, ..)| *n == name) {
        if at(reply, path).is_some() && (fresh || at(&out, path).is_none()) {
            let keys: Vec<&str> = path.split('.').collect();
            let (last, parents) = keys.split_last().expect("a non-empty path");
            let parent = parents.iter().fold(&mut out, |v, k| &mut v[*k]);
            parent[*last] = value;
        }
    }
    out
}

/// Every field the fixture has, the reply has too, with the same JSON
/// type (null on either side matches anything: optional fields). Array
/// elements compare index by index. Fields only the reply has are fine:
/// the desktop ignores fields it does not read.
fn same_shape(path: &str, want: &Value, got: &Value, errs: &mut Vec<String>) {
    match (want, got) {
        (Value::Null, _) | (_, Value::Null) => {}
        (Value::Object(w), Value::Object(g)) => {
            for (k, wv) in w {
                match g.get(k) {
                    Some(gv) => same_shape(&format!("{path}.{k}"), wv, gv, errs),
                    None if wv.is_null() => {}
                    None => errs.push(format!("{path}: reply has no `{k}`")),
                }
            }
        }
        (Value::Array(w), Value::Array(g)) => {
            for (i, (wv, gv)) in w.iter().zip(g).enumerate() {
                same_shape(&format!("{path}[{i}]"), wv, gv, errs);
            }
        }
        (Value::Number(_), Value::Number(_))
        | (Value::String(_), Value::String(_))
        | (Value::Bool(_), Value::Bool(_)) => {}
        _ => errs.push(format!("{path}: fixture {want} vs reply {got}")),
    }
}

/// The keys of [`added`] the reply has and the committed fixture lacks,
/// which `write` would add: the fixture is older than the reply.
fn missing_added(name: &str, want: &Value, got: &Value, errs: &mut Vec<String>) {
    for (_, path, _) in added().into_iter().filter(|(n, ..)| *n == name) {
        if at(got, path).is_some() && at(want, path).is_none() {
            errs.push(format!("{name}: fixture has no `{path}`, the reply has"));
        }
    }
}

/// Machine paths and per-run ids replaced by stable placeholders.
fn redact(v: &Value, subs: &[(String, String)]) -> Value {
    let mut s = serde_json::to_string_pretty(v).unwrap();
    for (from, to) in subs {
        s = s.replace(from.as_str(), to.as_str());
    }
    serde_json::from_str(&s).unwrap()
}

#[test]
fn gui_p1_replies_match_fixtures() {
    let dir = fixtures_dir();
    let requests: Value = serde_json::from_str(
        &fs::read_to_string(dir.join("requests.json")).expect("requests.json"),
    )
    .unwrap();
    let writing = std::env::var("GUI_FIXTURES").as_deref() == Ok("write");

    let lv = Live::with_deps(
        &["tool", "web"],
        &[("a.txt", b"one\ntwo\n"), ("src/lib.txt", b"fn x\n")],
        &["tool"],
    );
    let (replies, subs) = lv.serve(|_| {
        let s1 = lv.new_session("T-101", "tool,web");
        let s2 = lv.new_session("T-102", "tool");
        let w1 = lv.ws(&s1, "tool");
        write(&w1, "a.txt", b"one\ntwo\nthree\n");
        write(&w1, "notes.txt", b"new\n");
        lv.run_in(&w1, &["checkpoint", "-m", "first edits"]).ok();
        write(&w1, "a.txt", b"one\n2\nthree\n");
        let w2 = lv.ws(&s2, "tool");
        write(&w2, "package.json", NEW_DEP.as_bytes());
        lv.run_in(&w2, &["deps", "propose"]).ok();

        let socket = lv.home.socket();
        let mut out = Vec::new();
        for r in requests.as_array().unwrap() {
            let name = r["fixture"].as_str().unwrap().to_string();
            let mut params = r["params"].clone();
            if params.get("session").and_then(Value::as_str) == Some("$S1") {
                params["session"] = json!(s1.as_str());
            }
            let method = r["method"].as_str().unwrap();
            // The desktop asks `status{session}` for `chains` (status-perf
            // D2). A daemon older than status-perf refuses the field; the
            // desktop then asks again without it (daemon.rs `read`), and so
            // does this test, so it runs on both sides of that change.
            let reply = match strata_client::call(&socket, method, params.clone(), Kind::Read) {
                Err(strata_client::CallError::Failed(e))
                    if params.get("chains").is_some()
                        && e.message.contains("unknown field `chains`") =>
                {
                    let mut params = params;
                    params.as_object_mut().unwrap().remove("chains");
                    strata_client::call(&socket, method, params, Kind::Read)
                }
                other => other,
            }
            .unwrap_or_else(|e| panic!("{name}: {:?}", e.into_error()));
            out.push((name, reply));
        }
        let subs = vec![
            (lv.sb.to_string_lossy().into_owned(), "/sandbox".to_string()),
            (
                lv.home.canon().to_string_lossy().into_owned(),
                "/home".to_string(),
            ),
            (
                lv.home.path().to_string_lossy().into_owned(),
                "/home".to_string(),
            ),
            (s1.as_str().to_string(), "S1".to_string()),
            (s2.as_str().to_string(), "S2".to_string()),
        ];
        (out, subs)
    });

    let mut errs = Vec::new();
    for (name, reply) in &replies {
        let got = redact(reply, &subs);
        let file = dir.join(format!("{name}.json"));
        if writing {
            let committed: Option<Value> = fs::read_to_string(&file)
                .ok()
                .map(|t| serde_json::from_str(&t).unwrap());
            let fixture = updated(name, committed.clone(), &got);
            if committed.as_ref() != Some(&fixture) {
                fs::write(
                    &file,
                    serde_json::to_string_pretty(&fixture).unwrap() + "\n",
                )
                .unwrap();
            }
            continue;
        }
        let want: Value = serde_json::from_str(
            &fs::read_to_string(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display())),
        )
        .unwrap();
        same_shape(name, &want, &got, &mut errs);
        missing_added(name, &want, &got, &mut errs);
    }
    assert!(
        errs.is_empty(),
        "replies drifted from the desktop fixtures (regenerate with GUI_FIXTURES=write):\n{}",
        errs.join("\n")
    );
}
