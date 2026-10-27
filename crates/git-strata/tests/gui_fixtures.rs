//! gui.md §8.1 P1: the read replies the desktop app is built against.
//!
//! The desktop crate may not depend on strata-daemon, so its tests run a
//! fake daemon fed from `apps/desktop/src-tauri/tests/fixtures/p1/`. This
//! test keeps those fixtures honest: it starts the real in-process daemon
//! (tests/support/live.rs, temp HOME and socket, never the live one),
//! sends each request in `requests.json` exactly as the desktop sends it,
//! and checks every field of the committed fixture is still in the reply
//! with the same JSON type.
//!
//! Regenerate the fixtures (after a deliberate reply change) with
//! `GUI_FIXTURES=write cargo test -p git-strata --test gui_fixtures`.

mod support;

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use strata_client::Kind;
use support::live::{Live, write};

const NEW_DEP: &str = "{\n  \"name\": \"tool\",\n  \"version\": \"1.0.0\",\n  \"dependencies\": { \"x\": \"^1.0.0\", \"y\": \"^2.0.0\" }\n}\n";

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/desktop/src-tauri/tests/fixtures/p1")
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
        (Value::Number(_), Value::Number(_)) | (Value::String(_), Value::String(_)) | (Value::Bool(_), Value::Bool(_)) => {}
        _ => errs.push(format!("{path}: fixture {want} vs reply {got}")),
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
    let requests: Value = serde_json::from_str(&fs::read_to_string(dir.join("requests.json")).expect("requests.json")).unwrap();
    let writing = std::env::var("GUI_FIXTURES").as_deref() == Ok("write");

    let lv = Live::with_deps(&["tool", "web"], &[("a.txt", b"one\ntwo\n"), ("src/lib.txt", b"fn x\n")], &["tool"]);
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
            let reply = strata_client::call(&socket, r["method"].as_str().unwrap(), params, Kind::Read)
                .unwrap_or_else(|e| panic!("{name}: {:?}", e.into_error()));
            out.push((name, reply));
        }
        let subs = vec![
            (lv.sb.to_string_lossy().into_owned(), "/sandbox".to_string()),
            (lv.home.canon().to_string_lossy().into_owned(), "/home".to_string()),
            (lv.home.path().to_string_lossy().into_owned(), "/home".to_string()),
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
            fs::write(&file, serde_json::to_string_pretty(&got).unwrap() + "\n").unwrap();
            continue;
        }
        let want: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display()))).unwrap();
        same_shape(name, &want, &got, &mut errs);
    }
    assert!(errs.is_empty(), "replies drifted from the desktop fixtures (regenerate with GUI_FIXTURES=write):\n{}", errs.join("\n"));
}
