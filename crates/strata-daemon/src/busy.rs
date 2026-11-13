//! `daemon_status.busy` (status-bar design §3.4, §11.5): the `Busy` replies
//! requests got because a lock or a du scan held them off past their wait
//! (`detail.reason` `lock_wait` or `du_cancel_timeout`), counted once per
//! reply in `Server::handle`. A background probe that skips a held lock and
//! the session layer's own retries never reach a reply, so they are not
//! counted; neither are the `Busy` answers made outside `Server::handle`
//! (`store_unavailable`, `starting`, `too_many_open_files`).
//!
//! The last [`RING`] events are kept; a snapshot lists those younger than
//! [`WINDOW`], oldest first. The mutex is a leaf: held only to push or to
//! copy, never across other work or while taking another lock.

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Map, Value, json};
use strata_types::{Code, SessionId, StrataError};

/// Events kept.
pub const RING: usize = 50;
/// How far back a snapshot reaches.
pub const WINDOW: Duration = Duration::from_secs(600);
/// The `detail.reason`s counted: a request held off past its wait.
pub const REASONS: [&str; 2] = ["lock_wait", "du_cancel_timeout"];

struct Event {
    /// For the window (the wall clock may move).
    seen: Instant,
    at: SystemTime,
    method: String,
    session: Option<SessionId>,
    reason: &'static str,
    scope: Map<String, Value>,
    holder: Option<String>,
    holder_session: Option<String>,
}

#[derive(Default)]
struct Ring {
    total: u64,
    events: VecDeque<Event>,
    /// When the youngest event the ring pushed out was seen.
    evicted: Option<Instant>,
}

/// The `Busy` replies since the daemon started.
#[derive(Default)]
pub struct BusyLog {
    ring: Mutex<Ring>,
}

impl BusyLog {
    fn lock(&self) -> MutexGuard<'_, Ring> {
        self.ring.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// One reply's error: kept when it is a `Busy` with a counted reason.
    /// `method` and `session` are the request's.
    pub fn record(&self, method: &str, session: Option<&SessionId>, e: &StrataError) {
        self.record_at(Instant::now(), method, session, e);
    }

    fn record_at(&self, now: Instant, method: &str, session: Option<&SessionId>, e: &StrataError) {
        if e.code != Code::Busy {
            return;
        }
        let Some(detail) = e.detail.as_ref() else {
            return;
        };
        let Some(reason) = REASONS.into_iter().find(|r| detail["reason"] == *r) else {
            return;
        };
        let text = |v: &Value| v.as_str().map(str::to_owned);
        let event = Event {
            seen: now,
            at: SystemTime::now(),
            method: method.to_owned(),
            session: session.cloned(),
            reason,
            scope: scope_of(detail),
            holder: text(&detail["holder"]),
            holder_session: text(&detail["holder_session"]),
        };
        let mut ring = self.lock();
        ring.total += 1;
        if ring.events.len() >= RING
            && let Some(old) = ring.events.pop_front()
        {
            ring.evicted = Some(old.seen);
        }
        ring.events.push_back(event);
    }

    /// `{total, recent, truncated}`: `recent` holds the kept events younger
    /// than [`WINDOW`], oldest first; `truncated` says the ring pushed out
    /// an event that is still inside the window.
    pub fn json(&self) -> Value {
        self.json_at(Instant::now())
    }

    fn json_at(&self, now: Instant) -> Value {
        let mut ring = self.lock();
        let young = |t: Instant| now.saturating_duration_since(t) < WINDOW;
        while ring.events.front().is_some_and(|e| !young(e.seen)) {
            ring.events.pop_front();
        }
        let truncated = ring.evicted.is_some_and(young);
        let recent: Vec<Value> = ring.events.iter().map(Event::json).collect();
        json!({ "total": ring.total, "recent": recent, "truncated": truncated })
    }
}

impl Event {
    fn json(&self) -> Value {
        json!({
            "at": strata_types::serde_rfc3339::format(&self.at).ok(),
            "method": self.method,
            "session": self.session,
            "reason": self.reason,
            "scope": self.scope,
            "holder": self.holder,
            "holder_session": self.holder_session,
        })
    }
}

/// What the request waited for: the lock a `lock_wait` names (`detail.lock`,
/// `{repo}` or `{session}`), else the error's own `repo` / `session`, else
/// the one session a `du_cancel_timeout` names. Empty when none is known.
fn scope_of(detail: &Value) -> Map<String, Value> {
    let from = if detail["lock"].is_object() {
        &detail["lock"]
    } else {
        detail
    };
    let mut scope = Map::new();
    for key in ["repo", "session"] {
        if let Some(name) = from[key].as_str() {
            scope.insert(key.to_owned(), json!(name));
        }
    }
    if scope.is_empty()
        && let Some([one]) = detail["sessions"].as_array().map(Vec::as_slice)
        && let Some(name) = one.as_str()
    {
        scope.insert("session".to_owned(), json!(name));
    }
    scope
}

#[cfg(test)]
mod tests {
    use super::*;

    fn busy(detail: Value) -> StrataError {
        StrataError::new(Code::Busy, "busy").with_detail(detail)
    }

    fn sid(s: &str) -> SessionId {
        SessionId::try_from(s.to_owned()).unwrap()
    }

    #[test]
    fn only_busy_with_a_counted_reason_is_kept() {
        let log = BusyLog::default();
        log.record("sync", None, &busy(json!({ "reason": "recent_writes" })));
        log.record(
            "sync",
            None,
            &busy(json!({ "reason": "changed_since_scan" })),
        );
        log.record("sync", None, &StrataError::new(Code::Busy, "no detail"));
        let not_busy =
            StrataError::new(Code::Conflict, "c").with_detail(json!({ "reason": "lock_wait" }));
        log.record("sync", None, &not_busy);
        assert_eq!(
            log.json(),
            json!({ "total": 0, "recent": [], "truncated": false })
        );

        let s = sid("s-aaaaaaaa");
        let lock_wait = busy(json!({
            "reason": "lock_wait", "lock": { "repo": "tool" },
            "holder": "catch_up", "holder_session": "s-bbbbbbbb",
        }));
        log.record("checkpoint", Some(&s), &lock_wait);
        let du = busy(json!({ "reason": "du_cancel_timeout", "sessions": ["s-aaaaaaaa"] }));
        log.record("rm", Some(&s), &du);
        let v = log.json();
        assert_eq!(v["total"], 2, "{v}");
        let recent = v["recent"].as_array().unwrap();
        assert_eq!(recent.len(), 2, "{v}");
        let first = &recent[0];
        assert_eq!(first["method"], "checkpoint");
        assert_eq!(first["session"], "s-aaaaaaaa");
        assert_eq!(first["reason"], "lock_wait");
        assert_eq!(first["scope"], json!({ "repo": "tool" }));
        assert_eq!(first["holder"], "catch_up");
        assert_eq!(first["holder_session"], "s-bbbbbbbb");
        assert!(
            strata_types::serde_rfc3339::parse(first["at"].as_str().unwrap()).is_ok(),
            "{first}"
        );
        let second = &recent[1];
        assert_eq!(second["reason"], "du_cancel_timeout");
        assert_eq!(second["scope"], json!({ "session": "s-aaaaaaaa" }));
        assert_eq!(
            (second["holder"].clone(), second["holder_session"].clone()),
            (Value::Null, Value::Null)
        );
    }

    #[test]
    fn scope_is_the_lock_then_the_error_then_unknown() {
        assert_eq!(
            scope_of(&json!({ "lock": { "session": "s-a" }, "repo": "web" })),
            json!({ "session": "s-a" }).as_object().unwrap().clone()
        );
        assert_eq!(
            scope_of(&json!({ "repo": "web" })),
            json!({ "repo": "web" }).as_object().unwrap().clone()
        );
        assert!(scope_of(&json!({ "sessions": ["s-a", "s-b"] })).is_empty());
        assert!(scope_of(&json!({ "reason": "lock_wait" })).is_empty());
    }

    #[test]
    fn the_window_drops_old_events_and_the_ring_says_when_it_truncated() {
        let log = BusyLog::default();
        let t0 = Instant::now();
        let e = busy(json!({ "reason": "lock_wait", "lock": { "repo": "tool" } }));
        for _ in 0..RING + 3 {
            log.record_at(t0, "sync", None, &e);
        }
        let v = log.json_at(t0);
        assert_eq!(v["total"], RING + 3);
        assert_eq!(v["recent"].as_array().unwrap().len(), RING);
        assert_eq!(v["truncated"], true, "pushed out inside the window");
        // Later: the old events leave the window, and so does the truncation.
        let later = t0 + WINDOW + Duration::from_secs(1);
        log.record_at(later, "add", None, &e);
        let v = log.json_at(later);
        assert_eq!(v["total"], RING + 4);
        assert_eq!(v["recent"].as_array().unwrap().len(), 1, "{v}");
        assert_eq!(v["recent"][0]["method"], "add");
        assert_eq!(v["truncated"], false);
    }
}
