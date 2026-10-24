//! deps_propose / deps_approve over the socket (deps.md §5.12, wired for
//! the cli): refusals come before any prompt; a proposal is recorded only
//! on drift; `switch` moves the proposer itself without a confirmation.

mod support;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use serde_json::{Value, json};
use strata_daemon::handlers;
use strata_daemon::startup::{self, Daemon};
use strata_types::SessionId;
use support::*;

fn serving<R>(d: &Daemon<'_>, f: impl FnOnce() -> R) -> R {
    let listener = startup::bind(&d.core.opts.socket).unwrap();
    let ext = AtomicBool::new(false);
    std::thread::scope(|s| {
        let h = s.spawn(|| handlers::serve(d, &listener, &ext));
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        d.request_stop();
        h.join().unwrap().unwrap();
        r.unwrap_or_else(|p| std::panic::resume_unwind(p))
    })
}

fn call(socket: &Path, method: &str, params: Value) -> Value {
    let mut s = UnixStream::connect(socket).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(120))).unwrap();
    let line = json!({ "id": "1", "method": method, "params": params }).to_string();
    s.write_all(line.as_bytes()).unwrap();
    s.write_all(b"\n").unwrap();
    let mut out = String::new();
    BufReader::new(s).read_line(&mut out).unwrap();
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("reply {out:?}: {e}"))
}

fn ok(v: Value) -> Value {
    assert_eq!(v["ok"], json!(true), "{v:#}");
    v["data"].clone()
}

fn new_session(socket: &Path) -> SessionId {
    let d = ok(call(socket, "new", json!({ "ticket": null, "repos": ["tool"] })));
    serde_json::from_value(d["session"]["manifest"]["id"].clone()).unwrap()
}

const NEW_DEP: &str = r#"{
  "name": "tool",
  "version": "1.0.87",
  "dependencies": {
    "x": "^1.0.0",
    "y": "^2.0.0"
  }
}
"#;

#[test]
fn approve_unknown_proposal_is_refused_without_prompt() {
    let fx = Dfx::new();
    let (c, log, _) = fx.confirmer(vec![]);
    let core = fx.prepare_with(c).unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    serving(&d, || {
        let r = call(&sock, "deps_approve", json!({ "proposal": 4242, "sessions": null }));
        assert_eq!(r["ok"], json!(false), "{r:#}");
        assert_eq!(r["error"]["code"], json!("NoSuchProposal"), "{r:#}");
    });
    assert!(log.shown().is_empty());
}

#[test]
fn propose_without_drift_records_nothing() {
    let fx = Dfx::new();
    let core = fx.prepare().unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    serving(&d, || {
        let id = new_session(&sock);
        let p = ok(call(&sock, "deps_propose", json!({ "session": id, "repo": "tool", "switch": false })));
        assert_eq!(p["proposal"], Value::Null, "{p:#}");
        let l = ok(call(&sock, "deps_list", json!({ "repo": null })));
        assert_eq!(l["proposals"], json!([]), "{l:#}");
    });
}

#[test]
fn propose_on_drift_awaits_publish_and_approve_refuses_it() {
    let fx = Dfx::new();
    let (c, log, _) = fx.confirmer(vec![]);
    let core = fx.prepare_with(c).unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    serving(&d, || {
        let id = new_session(&sock);
        let root = ws(&core, &id, "tool");
        std::fs::write(root.join("package.json"), NEW_DEP).unwrap();
        let p = ok(call(&sock, "deps_propose", json!({ "session": id, "repo": "tool", "switch": false })));
        assert_eq!(p["proposal"]["state"], json!({ "kind": "AwaitingPublish" }), "{p:#}");
        assert!(p["drift"].is_object(), "{p:#}");
        assert_eq!(p["switched"], Value::Null);
        let pid = p["proposal"]["id"].clone();
        // Not Pending: refused before the confirmation, state in detail.
        let r = call(&sock, "deps_approve", json!({ "proposal": pid, "sessions": null }));
        assert_eq!(r["error"]["code"], json!("NoSuchProposal"), "{r:#}");
        assert_eq!(r["error"]["detail"]["state"], json!({ "kind": "AwaitingPublish" }), "{r:#}");
    });
    assert!(log.shown().is_empty());
}

#[test]
fn propose_switch_moves_the_proposer_without_prompt() {
    let fx = Dfx::new();
    let (c, log, _) = fx.confirmer(vec![]);
    let core = fx.prepare_with(c).unwrap();
    let d = Daemon::start(&core).unwrap();
    let sock = fx.socket();
    serving(&d, || {
        let id = new_session(&sock);
        let root = ws(&core, &id, "tool");
        let before = core.store.session(&id).unwrap().unwrap().manifest.repos[&repo_name("tool")].base.clone();
        std::fs::write(root.join("package.json"), NEW_DEP).unwrap();
        let p = ok(call(&sock, "deps_propose", json!({ "session": id, "repo": "tool", "switch": true })));
        let sw = p["switched"].as_array().unwrap_or_else(|| panic!("{p:#}"));
        assert_eq!(sw.len(), 1, "{p:#}");
        assert_eq!(sw[0]["kind"], json!("moved"), "{p:#}");
        let after = core.store.session(&id).unwrap().unwrap().manifest.repos[&repo_name("tool")].base.clone();
        assert_eq!(after.tree, before.tree);
        assert_ne!(after.deps, before.deps);
        assert_eq!(after.deps.as_str(), p["proposal"]["to"].as_str().unwrap());
    });
    assert!(log.shown().is_empty());
}
