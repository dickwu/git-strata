//! new-lock §4: `new` and `add` send a refusal again only when the daemon
//! certified it replay-safe, within `--timeout`, naming the holder on stderr.
mod support;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use support::{FakeDaemon, Home};

fn refusal(q: &Value, certified: bool) -> Value {
    let mut detail = json!({
        "reason": "lock_wait", "lock": {"repo": "tool"}, "holder": "sync",
        "holder_session": "s-uijvlo3l", "holder_origin": "request",
        "holder_since": "2026-11-25T10:00:00Z", "held_ms": 130_000,
    });
    if certified {
        detail["retry_safe"] = json!(true);
    }
    json!({"id": q["id"], "ok": false, "error": {
        "code": "Busy", "message": "waited 30000 ms before anything was written", "detail": detail,
    }})
}

fn status(q: &Value) -> Value {
    json!({"id": q["id"], "ok": true, "data": {"features": [], "holds": []}})
}

fn created(q: &Value) -> Value {
    json!({"id": q["id"], "ok": true, "data": {"session": {
        "manifest": {"id": "s-abcdefgh", "repos": {}},
        "warnings": [{"kind": "FetchSkipped", "repo": "tool", "holder": "sync",
            "holder_session": "s-uijvlo3l", "last_fetch_at": "2026-11-25T10:00:00Z"}],
    }}})
}

static CERTIFIED: AtomicUsize = AtomicUsize::new(0);

/// `cli_retries_only_pre_write_lock_wait`, end to end: two certified
/// refusals, then the same request is accepted.
#[test]
fn new_resends_the_identical_request_after_certified_refusals() {
    let h = Home::new();
    let d = FakeDaemon::start(&h.socket(), |q| {
        if q["method"] != "new" {
            return status(q);
        }
        if CERTIFIED.fetch_add(1, Ordering::SeqCst) < 2 {
            refusal(q, true)
        } else {
            created(q)
        }
    });
    let run = h.run(&[
        "new",
        "T-1",
        "--repo",
        "tool",
        "--progress",
        "--timeout",
        "30",
    ]);
    assert_eq!(run.ok()["manifest"]["id"], "s-abcdefgh");
    let sent: Vec<Value> = d
        .requests()
        .into_iter()
        .filter(|q| q["method"] == "new")
        .collect();
    assert_eq!(sent.len(), 3, "{run:?}");
    assert!(
        sent.iter().all(|q| q["params"] == sent[0]["params"]),
        "{sent:?}"
    );
    assert_eq!(
        run.stderr.matches("; retrying").count(),
        2,
        "{}",
        run.stderr
    );
    assert!(
        run.stderr
            .contains("new; repo tool held by sync (session s-uijvlo3l, request, 2m10s); retrying"),
        "{}",
        run.stderr
    );
    assert!(run.stderr.contains("tool: fetch skipped"), "{}", run.stderr);
}

/// An uncertified `lock_wait` (every legacy daemon's) comes back at once,
/// unchanged, still naming the holder.
#[test]
fn uncertified_lock_wait_is_returned_at_once() {
    let h = Home::new();
    let d = FakeDaemon::start(&h.socket(), |q| {
        if q["method"] == "new" {
            refusal(q, false)
        } else {
            status(q)
        }
    });
    let run = h.run(&["new", "T-2", "--repo", "tool", "--progress"]);
    assert_eq!(run.err_code(), "Busy");
    assert_eq!(run.reason(), "lock_wait");
    assert!(run.env()["error"]["detail"].get("retries").is_none());
    assert_eq!(
        d.requests().iter().filter(|q| q["method"] == "new").count(),
        1
    );
    assert!(run.stderr.contains("; not retried"), "{}", run.stderr);
}

/// `budget_checked_between_attempts_only`: the budget ends the retries and
/// the last refusal says what happened and what to do.
#[test]
fn spent_budget_returns_the_last_refusal_with_the_fix() {
    let h = Home::new();
    let d = FakeDaemon::start(&h.socket(), |q| {
        if q["method"] == "new" {
            refusal(q, true)
        } else {
            status(q)
        }
    });
    let start = Instant::now();
    let run = h.run(&["new", "T-3", "--repo", "tool", "--timeout", "1.5"]);
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "{:?}",
        start.elapsed()
    );
    assert_eq!(run.err_code(), "Busy");
    let detail = &run.env()["error"]["detail"];
    assert_eq!(detail["retry_safe"], true);
    // How many retries fit in 1.5 s depends on the wall clock (2 when the
    // first attempt is quick); at least the first 1 s back-off fits.
    let retries = detail["retries"].as_u64().unwrap_or(0) as usize;
    assert!(retries >= 1, "{detail}");
    assert!(detail["waited_ms"].as_u64().unwrap() >= 1_400, "{detail}");
    assert!(
        detail["fix"].as_str().unwrap().contains("--timeout 0"),
        "{detail}"
    );
    assert_eq!(
        d.requests().iter().filter(|q| q["method"] == "new").count(),
        retries + 1
    );
    assert!(
        run.stderr.is_empty(),
        "no TTY and no --progress: {}",
        run.stderr
    );
    let run: support::Run = h
        .cmd(&["new", "T-3", "--repo", "tool"])
        .env("GIT_STRATA_TIMEOUT", "NaN")
        .output()
        .unwrap()
        .into();
    assert_eq!(run.reason(), "timeout");
    assert_eq!(
        d.requests().iter().filter(|q| q["method"] == "new").count(),
        retries + 1,
        "an invalid budget is refused before anything is sent"
    );
}
