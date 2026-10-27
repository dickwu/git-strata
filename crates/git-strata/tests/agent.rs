//! agent-id: `new` records the agent (detected or `--agent`), sends it only
//! to a daemon listing `new.agent`, and degrades to a warning against an
//! older one. Fake daemons for the compat cases, the in-process daemon end
//! to end.

mod support;

use serde_json::{Value, json};
use support::live::Live;
use support::{FakeDaemon, Home, Run};

fn data(req: &Value, d: Value) -> Value {
    json!({ "id": req["id"], "ok": true, "data": d })
}

fn rpc_failed(req: &Value, message: &str) -> Value {
    json!({ "id": req["id"], "ok": false, "error": { "rpc": "Failed", "message": message } })
}

fn created(req: &Value) -> Value {
    let mut m = json!({ "id": "s-aaaaaaaa", "root": "/nowhere", "repos": {} });
    if let Some(a) = req["params"].get("agent") {
        m["agent"] = a.clone();
    }
    data(req, json!({ "session": { "manifest": m, "warnings": [] }, "anchor_unknown": [] }))
}

const OLD_REFUSAL: &str = "unknown field `agent`, expected one of `ticket`, `repos`, `only`";

/// A daemon from before `new.agent`: no `features`, and `new` refuses the field.
fn old_daemon(req: &Value) -> Value {
    match req["method"].as_str() {
        Some("daemon_status") => data(req, json!({ "holds": [], "warnings": [] })),
        Some("new") if req["params"].get("agent").is_some() => rpc_failed(req, OLD_REFUSAL),
        Some("new") => created(req),
        _ => rpc_failed(req, "unexpected"),
    }
}

/// Lists `new.agent`, but `new` is answered by an older daemon (swapped in
/// between the two connections).
fn swapped_daemon(req: &Value) -> Value {
    match req["method"].as_str() {
        Some("daemon_status") => data(req, json!({ "holds": [], "warnings": [], "features": ["new.agent"] })),
        _ => old_daemon(req),
    }
}

fn new_daemon(req: &Value) -> Value {
    match req["method"].as_str() {
        Some("daemon_status") => data(req, json!({ "holds": [], "warnings": [], "features": ["new.agent"] })),
        Some("new") => created(req),
        _ => rpc_failed(req, "unexpected"),
    }
}

fn run_new(h: &Home, env: &[(&str, &str)], extra: &[&str]) -> Run {
    let mut args = vec!["new", "T-1", "--repo", "tool"];
    args.extend_from_slice(extra);
    let mut c = h.cmd(&args);
    for (k, v) in env {
        c.env(k, v);
    }
    Run::from(c.output().expect("spawn git-strata"))
}

fn methods(d: &FakeDaemon) -> Vec<(String, Option<Value>)> {
    d.requests().iter().map(|r| (r["method"].as_str().unwrap_or("").to_string(), r["params"].get("agent").cloned())).collect()
}

fn not_recorded(agent: &str) -> Value {
    json!({ "kind": "AgentNotRecorded", "agent": agent, "reason": "daemon_too_old" })
}

#[test]
fn new_daemon_gets_the_agent() {
    let h = Home::new();
    let d = FakeDaemon::start(&h.socket(), new_daemon);
    let r = run_new(&h, &[("STRATA_AGENT", "aider")], &[]);
    let out = r.ok();
    assert_eq!(out["manifest"]["agent"], "aider", "{r:?}");
    assert_eq!(out["warnings"], json!([]));
    assert_eq!(methods(&d), [("daemon_status".into(), None), ("new".into(), Some(json!("aider")))]);
}

#[test]
fn old_daemon_never_sees_the_field_and_the_session_says_so() {
    let h = Home::new();
    let d = FakeDaemon::start(&h.socket(), old_daemon);
    let r = run_new(&h, &[("CODEX_THREAD_ID", "t-1")], &[]);
    let out = r.ok();
    assert!(out["manifest"].get("agent").is_none(), "{r:?}");
    assert_eq!(out["warnings"], json!([not_recorded("codex")]), "{r:?}");
    assert!(r.stderr.contains("predates agent recording"), "{r:?}");
    assert_eq!(methods(&d), [("daemon_status".into(), None), ("new".into(), None)]);
}

#[test]
fn a_daemon_swapped_after_the_probe_gets_new_again_without_the_agent() {
    let h = Home::new();
    let d = FakeDaemon::start(&h.socket(), swapped_daemon);
    let r = run_new(&h, &[], &["--agent", "claude-code"]);
    let out = r.ok();
    assert_eq!(out["warnings"], json!([not_recorded("claude-code")]), "{r:?}");
    assert_eq!(
        methods(&d),
        [("daemon_status".into(), None), ("new".into(), Some(json!("claude-code"))), ("new".into(), None)]
    );
}

#[test]
fn other_refusals_are_not_replayed() {
    fn other_failed(req: &Value) -> Value {
        match req["method"].as_str() {
            Some("new") => rpc_failed(req, "unknown field `x`"),
            _ => new_daemon(req),
        }
    }
    fn handler_error(req: &Value) -> Value {
        match req["method"].as_str() {
            Some("new") => json!({ "id": req["id"], "ok": false, "error": {
                "code": "InvalidInput", "message": "unknown field `agent`", "detail": { "reason": "agent" } } }),
            _ => new_daemon(req),
        }
    }
    fn closes(req: &Value) -> Value {
        match req["method"].as_str() {
            Some("new") => Value::Null,
            _ => new_daemon(req),
        }
    }
    let cases: [ReplyCase; 3] =
        [(other_failed, "InvalidInput", "rpc_failed"), (handler_error, "InvalidInput", "agent"), (closes, "Unknown", "socket_closed")];
    for (reply, code, reason) in cases {
        let h = Home::new();
        let d = FakeDaemon::start(&h.socket(), reply);
        let r = run_new(&h, &[("STRATA_AGENT", "codex")], &[]);
        assert_eq!((r.err_code().as_str(), r.reason().as_str()), (code, reason), "{r:?}");
        let news = methods(&d).iter().filter(|(m, _)| m == "new").count();
        assert_eq!(news, 1, "sent once: {:?}", d.requests());
    }
}

#[test]
fn no_agent_means_no_probe_and_no_field() {
    let h = Home::new();
    let d = FakeDaemon::start(&h.socket(), new_daemon);
    let r = run_new(&h, &[], &[]);
    assert!(r.ok()["manifest"].get("agent").is_none(), "{r:?}");
    assert_eq!(methods(&d), [("new".into(), None)]);
}

#[test]
fn a_bad_explicit_agent_sends_nothing() {
    let h = Home::new();
    let d = FakeDaemon::start(&h.socket(), new_daemon);
    for (env, extra) in [(vec![("STRATA_AGENT", "bad name")], vec![]), (vec![("CLAUDECODE", "1")], vec!["--agent", ""])] {
        let r = run_new(&h, &env, &extra);
        assert_eq!((r.err_code().as_str(), r.reason().as_str()), ("InvalidInput", "agent"), "{r:?}");
    }
    assert_eq!(d.connections(), 0);
}

/// The probe gives up after its deadline: a socket that accepts and never
/// answers still gets the session (without the agent) instead of hanging.
#[test]
fn the_probe_is_bounded() {
    let h = Home::new();
    let l = std::os::unix::net::UnixListener::bind(h.socket()).unwrap();
    let t0 = std::time::Instant::now();
    assert!(!git_strata::agent::daemon_takes_agent(&h.socket()));
    let took = t0.elapsed();
    assert!(took >= git_strata::agent::PROBE_DEADLINE && took < git_strata::agent::PROBE_DEADLINE * 3, "{took:?}");
    drop(l);
}

/// End to end: the in-process daemon stores the agent; `status` shows it
/// as `manifest.agent` (what the GUI's `agentOf` reads); `--agent` beats
/// the environment; markers are detected.
/// (environment, extra args, expected manifest.agent)
type LiveCase<'a> = (&'a [(&'a str, &'a str)], &'a [&'a str], Option<&'a str>);
/// (fake daemon reply, expected code, expected detail.reason)
type ReplyCase = (fn(&Value) -> Value, &'static str, &'static str);

#[test]
fn live_new_records_the_agent() {
    let lv = Live::new(&["tool"], &[("a.txt", b"one\n")]);
    lv.serve(|_| {
        let cases: [LiveCase; 4] = [
            (&[("STRATA_AGENT", "codex")], &[], Some("codex")),
            (&[("STRATA_AGENT", "codex"), ("CLAUDECODE", "1")], &["--agent", "mine.v2"], Some("mine.v2")),
            (&[("CLAUDE_CODE_ENTRYPOINT", "cli")], &[], Some("claude-code")),
            (&[], &[], None),
        ];
        for (i, (env, extra, want)) in cases.into_iter().enumerate() {
            let ticket = format!("T-{i}");
            let mut args = vec!["new", ticket.as_str(), "--repo", "tool"];
            args.extend_from_slice(extra);
            let mut c = lv.home.cmd(&args);
            for (k, v) in env {
                c.env(k, v);
            }
            let r = Run::from(c.output().unwrap());
            let out = r.ok();
            assert_eq!(out["manifest"]["agent"].as_str(), want, "{r:?}");
            assert_eq!(out["warnings"], json!([]), "{r:?}");
            let id = out["manifest"]["id"].as_str().unwrap();
            let st = lv.run(&["status", "--session", id]).ok();
            assert_eq!(st["manifest"]["agent"].as_str(), want, "{st}");
        }
        let all = lv.run(&["status", "--all"]).ok();
        let mut agents: Vec<Option<String>> = all["sessions"]
            .as_array()
            .unwrap_or_else(|| panic!("{all}"))
            .iter()
            .map(|s| s.get("status").unwrap_or(s)["manifest"]["agent"].as_str().map(str::to_string))
            .collect();
        agents.sort();
        assert_eq!(agents, [None, Some("claude-code".into()), Some("codex".into()), Some("mine.v2".into())], "{all}");
        let d = lv.run(&["daemon", "status"]).ok();
        assert!(d["features"].as_array().unwrap().contains(&json!("new.agent")), "{d}");
    });
}
