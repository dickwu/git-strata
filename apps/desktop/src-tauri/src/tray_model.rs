//! gui.md §5.8: what the menu-bar item shows, as plain data built from
//! the daemon's raw replies. Kept free of tauri so it is tested directly.
//!
//! G7: a "Waiting for you" item names only its kind, its number and the
//! repository. Tickets, commit titles, checkpoint messages, paths and
//! proposal reasons never reach the menu: the labels below are assembled
//! from those fields and the affected-session count only.

use std::time::{Duration, SystemTime};

use serde_json::Value;

pub const MAX_WAITING: usize = 5;
/// Older data than this is not shown as current (gui.md §5.8).
pub const STALE_AFTER: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Normal,
    /// Something waits for the human (a dot).
    Pending,
    /// The daemon is unreachable or the data is stale (a slash).
    Down,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WaitingKind {
    DepsProposal,
    Conflict,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitingItem {
    pub kind: WaitingKind,
    /// The proposal id, or the number of conflicted paths.
    pub number: u64,
    pub repo: String,
    pub sessions: usize,
}

impl WaitingItem {
    pub fn label(&self) -> String {
        let label = match self.kind {
            WaitingKind::DepsProposal => format!("Deps proposal {} in {}", self.number, self.repo),
            WaitingKind::Conflict if self.number == 1 => format!("1 conflict in {}", self.repo),
            WaitingKind::Conflict => format!("{} conflicts in {}", self.number, self.repo),
        };
        format!(
            "{label} · {} {}",
            self.sessions,
            if self.sessions == 1 {
                "session"
            } else {
                "sessions"
            }
        )
    }

    /// The menu item id; the front end gets the same string to open.
    pub fn id(&self) -> String {
        match self.kind {
            WaitingKind::DepsProposal => format!("waiting:deps:{}", self.number),
            WaitingKind::Conflict => format!("waiting:conflict:{}", self.repo),
        }
    }
}

/// The latest replies the GUI read, kept by the read commands.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    /// `None` until the first read; `Some(false)` when the last read found
    /// no daemon (or it did not answer).
    pub up: Option<bool>,
    /// The `status{all:true}` reply.
    pub sessions: Option<Value>,
    /// The `deps_list` reply.
    pub deps: Option<Value>,
    /// Latest daemon_status, used solely for additive capability gates.
    pub daemon_status: Option<Value>,
    /// When `up` was last set from a read.
    pub updated: Option<SystemTime>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayModel {
    pub icon: Icon,
    pub status_line: String,
    pub waiting: Vec<WaitingItem>,
    /// Items beyond `MAX_WAITING`.
    pub more: usize,
}

fn active_sessions(status: &Value) -> usize {
    status["sessions"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|s| {
                    s["status"]["state"]
                        .as_str()
                        .is_some_and(|st| st == "Active" || st == "Creating")
                })
                .count()
        })
        .unwrap_or(0)
}

/// Without a row mode, legacy callers retain confirm behavior.
pub fn waiting(sessions: Option<&Value>, deps: Option<&Value>) -> Vec<WaitingItem> {
    waiting_at(sessions, deps, false, SystemTime::now())
}

fn upstream<'a>(s: &'a Value, repo: &str) -> &'a Value {
    if s["status"]["manifest"]["repos"][repo]["tracked"] == false {
        return &Value::Null;
    }
    let ws = &s["status"]["workspaces"][repo]["upstream"];
    if !ws.is_null() {
        ws
    } else {
        &s["status"]["upstream"]["repos"][repo]
    }
}
fn affected(s: &Value, repo: &str, id: u64) -> bool {
    s["status"]["workspaces"][repo]["pending"]
        .as_array()
        .is_some_and(|ps| {
            ps.iter()
                .any(|p| p.as_u64() == Some(id) || p["id"].as_u64() == Some(id))
        })
}
fn timestamp(text: &str) -> Option<SystemTime> {
    if let Ok(t) = strata_types::serde_rfc3339::parse(text) {
        return Some(t);
    }
    let at = text.len().checked_sub(6)?;
    let offset = text.get(at..)?;
    if !matches!(offset.as_bytes()[0], b'+' | b'-') || offset.as_bytes()[3] != b':' {
        return None;
    }
    let hours: u64 = offset.get(1..3)?.parse().ok()?;
    let minutes: u64 = offset.get(4..6)?.parse().ok()?;
    if hours > 23 || minutes > 59 {
        return None;
    }
    let t = strata_types::serde_rfc3339::parse(&format!("{}Z", text.get(..at)?)).ok()?;
    let d = Duration::from_secs(hours * 3600 + minutes * 60);
    if offset.starts_with('+') {
        t.checked_sub(d)
    } else {
        t.checked_add(d)
    }
}
fn stuck(s: &Value, repo: &str, now: SystemTime) -> bool {
    let ws = &s["status"]["workspaces"][repo];
    let u = upstream(s, repo);
    matches!(
        u["deps"]["state"].as_str(),
        Some("install_failed" | "own_change" | "drift" | "waiting_human")
    ) || ws["health"].as_str().is_some_and(|h| h != "Ok")
        || !ws["scene"].is_null()
        || !s["status"]["scene"].is_null()
        || u["following"] == false
        || u["diverged"] == true
        || (u["auto"]["state"] == "deferred"
            && u["auto"]["since"]
                .as_str()
                .and_then(timestamp)
                .and_then(|t| now.duration_since(t).ok())
                .is_some_and(|d| d > Duration::from_secs(86400)))
}
fn waiting_at(
    sessions: Option<&Value>,
    deps: Option<&Value>,
    enabled: bool,
    now: SystemTime,
) -> Vec<WaitingItem> {
    let mut out = Vec::new();
    let ss = sessions
        .and_then(|s| s["sessions"].as_array())
        .map(Vec::as_slice)
        .unwrap_or_default();
    if let Some(ps) = deps.and_then(|d| d["proposals"].as_array()) {
        let mut ps: Vec<_> = ps.iter().collect();
        ps.sort_by_key(|p| p["id"].as_u64());
        for p in ps {
            if p["state"]["kind"].as_str() != Some("Pending") {
                continue;
            }
            if let (Some(id), Some(repo)) = (p["id"].as_u64(), p["repo"].as_str()) {
                let affected: Vec<_> = ss.iter().filter(|s| affected(s, repo, id)).collect();
                let applies = |s: &&Value, mode: &str| {
                    let u = upstream(s, repo);
                    u["deps"]["proposal"].as_u64() == Some(id) && u["deps"]["mode"] == mode
                };
                let applying = match p["mode"].as_str() {
                    Some("apply") => true,
                    Some("confirm") => false,
                    _ => {
                        enabled
                            && affected.iter().any(|s| applies(s, "apply"))
                            && !affected.iter().any(|s| applies(s, "confirm"))
                    }
                };
                if applying && !affected.iter().any(|s| stuck(s, repo, now)) {
                    continue;
                }
                out.push(WaitingItem {
                    kind: WaitingKind::DepsProposal,
                    number: id,
                    repo: repo.to_string(),
                    sessions: affected.len(),
                });
            }
        }
    }
    for s in ss {
        let Some(wss) = s["status"]["workspaces"].as_object() else {
            continue;
        };
        for (repo, ws) in wss {
            let u = if enabled {
                upstream(s, repo)
            } else {
                &Value::Null
            };
            let n = ws["conflicts"]
                .as_array()
                .map_or(0, Vec::len)
                .max(u["conflicts"].as_array().map_or(0, Vec::len));
            if n > 0 || u["auto"]["reason"] == "conflicts" {
                out.push(WaitingItem {
                    kind: WaitingKind::Conflict,
                    number: n.max(1) as u64,
                    repo: repo.clone(),
                    sessions: 1,
                });
            }
        }
    }
    out
}

fn ago(d: Duration) -> String {
    let m = d.as_secs() / 60;
    match m {
        0 => "Updated just now".to_string(),
        1 => "Updated 1 min ago".to_string(),
        m if m < 60 => format!("Updated {m} min ago"),
        _ => format!("Updated {} h ago", m / 60),
    }
}

pub fn model(s: &Snapshot, now: SystemTime) -> TrayModel {
    let stale = s.updated.map(|t| now.duration_since(t).unwrap_or_default());
    let enabled = s
        .daemon_status
        .as_ref()
        .and_then(|d| d["features"].as_array())
        .is_some_and(|features| features.iter().any(|f| f == "status.upstream"));
    let all = waiting_at(s.sessions.as_ref(), s.deps.as_ref(), enabled, now);
    let more = all.len().saturating_sub(MAX_WAITING);
    let shown: Vec<_> = all.into_iter().take(MAX_WAITING).collect();
    let (icon, status_line) = match (s.up, stale) {
        (None, _) => (Icon::Down, "Checking the daemon…".to_string()),
        (Some(false), _) => (Icon::Down, "Daemon isn't running".to_string()),
        (Some(true), Some(age)) if age > STALE_AFTER => (Icon::Down, ago(age)),
        (Some(true), _) => {
            let line = match s.sessions.as_ref().map(active_sessions) {
                Some(1) => "Daemon running, 1 session".to_string(),
                Some(n) => format!("Daemon running, {n} sessions"),
                None => "Daemon running".to_string(),
            };
            (
                if shown.is_empty() {
                    Icon::Normal
                } else {
                    Icon::Pending
                },
                line,
            )
        }
    };
    TrayModel {
        icon,
        status_line,
        waiting: shown,
        more,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> (Value, Value) {
        (
            json!({"sessions":[{"status":{"manifest":{"repos":{"tool":{"tracked":true}}},"workspaces":{"tool":{"health":"Ok","pending":[{"id":75}],"upstream":{"behind":3,"deps":{"proposal":75,"mode":"apply","state":"ready"},"auto":{"state":"deferred","reason":"open_files","since":"2026-11-01T12:00:00Z"}}}}}}]}),
            json!({"proposals":[{"id":75,"repo":"tool","state":{"kind":"Pending"}}]}),
        )
    }
    #[test]
    fn tray_affected_count_and_apply_stuck_threshold() {
        let (sessions, deps) = fixture();
        let now = timestamp("2026-11-02T12:00:00Z").unwrap();
        let legacy = waiting_at(Some(&sessions), Some(&deps), false, now);
        assert_eq!(legacy[0].sessions, 1);
        assert_eq!(legacy[0].label(), "Deps proposal 75 in tool · 1 session");
        assert!(waiting_at(Some(&sessions), Some(&deps), true, now).is_empty());
        assert_eq!(
            waiting_at(
                Some(&sessions),
                Some(&deps),
                true,
                now + Duration::from_millis(1)
            )
            .len(),
            1
        );
        assert_eq!(timestamp("2026-11-02T06:00:00-06:00"), Some(now));
        assert_eq!(timestamp("2026-11-02T14:00:00+02:00"), Some(now));
    }
    #[test]
    fn tray_proposal_mode_before_sessions_and_capability_polls() {
        let (_, mut deps) = fixture();
        deps["proposals"][0]["mode"] = json!("apply");
        let now = timestamp("2026-11-02T11:00:00Z").unwrap();
        for enabled in [false, true] {
            assert!(waiting_at(None, Some(&deps), enabled, now).is_empty());
        }
        let snapshot = Snapshot {
            up: Some(true),
            deps: Some(deps),
            updated: Some(now),
            ..Default::default()
        };
        assert_eq!(model(&snapshot, now).icon, Icon::Normal);
        assert!(model(&snapshot, now).waiting.is_empty());
    }
    #[test]
    fn tray_proposal_mode_without_workspace_upstream_or_remaining_pending() {
        let (mut sessions, mut deps) = fixture();
        deps["proposals"][0]["mode"] = json!("apply");
        let ws = &mut sessions["sessions"][0]["status"]["workspaces"]["tool"];
        ws.as_object_mut().unwrap().remove("upstream");
        let now = timestamp("2026-11-02T11:00:00Z").unwrap();
        assert!(waiting_at(Some(&sessions), Some(&deps), true, now).is_empty());
        sessions["sessions"][0]["status"]["workspaces"]["tool"]["pending"] = json!([]);
        assert!(waiting_at(Some(&sessions), Some(&deps), true, now).is_empty());
    }
    #[test]
    fn tray_proposal_mode_overrides_workspace_mode_both_ways() {
        let (mut sessions, mut deps) = fixture();
        let now = timestamp("2026-11-02T11:00:00Z").unwrap();
        deps["proposals"][0]["mode"] = json!("confirm");
        assert_eq!(waiting_at(Some(&sessions), Some(&deps), true, now).len(), 1);
        deps["proposals"][0]["mode"] = json!("apply");
        sessions["sessions"][0]["status"]["workspaces"]["tool"]["upstream"]["deps"]["mode"] =
            json!("confirm");
        assert!(waiting_at(Some(&sessions), Some(&deps), true, now).is_empty());
    }
    #[test]
    fn tray_apply_mode_keeps_unhealthy_and_scene_workspaces_without_upstream() {
        let now = timestamp("2026-11-02T11:00:00Z").unwrap();
        for condition in ["unhealthy", "workspace scene", "session scene"] {
            let (mut sessions, mut deps) = fixture();
            deps["proposals"][0]["mode"] = json!("apply");
            let status = &mut sessions["sessions"][0]["status"];
            let ws = &mut status["workspaces"]["tool"];
            ws.as_object_mut().unwrap().remove("upstream");
            match condition {
                "unhealthy" => ws["health"] = json!("Missing"),
                "workspace scene" => ws["scene"] = json!({}),
                _ => status["scene"] = json!({}),
            }
            let waiting = waiting_at(Some(&sessions), Some(&deps), true, now);
            assert_eq!(waiting.len(), 1, "{condition}");
            assert_eq!(waiting[0].sessions, 1);
        }
    }
    #[test]
    fn tray_older_rows_fall_back_to_workspace_then_confirm() {
        let (mut sessions, deps) = fixture();
        let now = timestamp("2026-11-02T11:00:00Z").unwrap();
        assert!(waiting_at(Some(&sessions), Some(&deps), true, now).is_empty());
        assert_eq!(
            waiting_at(Some(&sessions), Some(&deps), false, now).len(),
            1
        );
        sessions["sessions"][0]["status"]["workspaces"]["tool"]
            .as_object_mut()
            .unwrap()
            .remove("upstream");
        assert_eq!(waiting_at(Some(&sessions), Some(&deps), true, now).len(), 1);
        assert_eq!(waiting_at(None, Some(&deps), true, now).len(), 1);
    }
    #[test]
    fn tray_surfaces_all_stuck_states_and_agent_conflicts() {
        let now = timestamp("2026-11-02T11:00:00Z").unwrap();
        for state in ["install_failed", "own_change", "drift", "waiting_human"] {
            let (mut s, d) = fixture();
            s["sessions"][0]["status"]["workspaces"]["tool"]["upstream"]["deps"]["state"] =
                json!(state);
            assert_eq!(
                waiting_at(Some(&s), Some(&d), true, now).len(),
                1,
                "{state}"
            );
        }
        let (mut s, d) = fixture();
        s["sessions"][0]["status"]["workspaces"]["tool"]["upstream"]["auto"]["reason"] =
            json!("conflicts");
        let waiting = waiting_at(Some(&s), Some(&d), true, now);
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].kind, WaitingKind::Conflict);
    }
}
