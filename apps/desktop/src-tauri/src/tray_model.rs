//! Pure menu-bar policy. Only counts, repository names and fixed copy reach
//! the menu or tooltip; warning payloads, tickets, paths and reasons stay out.
pub use crate::tray_snapshot::{DownWhy, Input, Snapshot, Stamp};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, SystemTime};
pub const MAX_WAITING: usize = 5;
pub const STALE_AFTER: Duration = Duration::from_secs(60);
pub const BACKGROUND_HOLD_MAX: Duration = Duration::from_secs(30 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    Down,
    Alert,
    Checking,
    Waiting,
    Conflict,
    Syncing,
    Normal,
}
impl Icon {
    pub const ALL: [Self; 7] = [
        Self::Down,
        Self::Alert,
        Self::Checking,
        Self::Waiting,
        Self::Conflict,
        Self::Syncing,
        Self::Normal,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Down => "down",
            Self::Alert => "alert",
            Self::Checking => "checking",
            Self::Waiting => "waiting",
            Self::Conflict => "conflict",
            Self::Syncing => "syncing",
            Self::Normal => "normal",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WaitingKind {
    DepsProposal,
    Deferred,
    ConfigRelax,
    Conflict,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitingItem {
    pub kind: WaitingKind,
    pub number: u64,
    pub repo: String,
    pub sessions: usize,
    pub stale_age: Option<Duration>,
}
impl WaitingItem {
    pub fn label(&self) -> String {
        let sessions = plural(self.sessions, "session", "sessions");
        let stale = self
            .stale_age
            .map(|age| format!("as of {} ago", age_text(age)));
        if self.kind == WaitingKind::Deferred {
            return format!(
                "{}: newer changes not applied for over a day ({sessions}{})",
                self.repo,
                stale.map_or(String::new(), |s| format!(", {s}"))
            );
        }
        let label = match self.kind {
            WaitingKind::DepsProposal => format!(
                "Deps proposal {} in {} · {sessions}",
                self.number, self.repo
            ),
            WaitingKind::Conflict => format!(
                "{} in {} · {sessions}",
                plural(self.number as usize, "conflict", "conflicts"),
                self.repo
            ),
            WaitingKind::ConfigRelax => format!("{}: config relax waiting", self.repo),
            WaitingKind::Deferred => unreachable!(),
        };
        format!(
            "{label}{}",
            stale.map_or(String::new(), |s| format!(" ({s})"))
        )
    }
    pub fn id(&self) -> String {
        match self.kind {
            WaitingKind::DepsProposal => format!("waiting:deps:{}", self.number),
            WaitingKind::Deferred => format!("waiting:deferred:{}", self.repo),
            WaitingKind::ConfigRelax => format!("waiting:relax:{}", self.repo),
            WaitingKind::Conflict => format!("waiting:conflict:{}", self.repo),
        }
    }
}
fn item(kind: WaitingKind, number: u64, repo: &str, sessions: usize) -> WaitingItem {
    WaitingItem {
        kind,
        number,
        repo: repo.to_string(),
        sessions,
        stale_age: None,
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayModel {
    pub icon: Icon,
    pub status_line: String,
    pub tooltip: String,
    pub waiting: Vec<WaitingItem>,
    pub more: usize,
    pub stale: Option<String>,
    pub unavailable: usize,
}
fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}
fn age_text(age: Duration) -> String {
    let minutes = age.as_secs() / 60;
    if minutes < 60 {
        format!("{minutes} min")
    } else {
        format!("{} h", minutes / 60)
    }
}
fn entries(sessions: Option<&Value>) -> &[Value] {
    sessions
        .and_then(|s| s["sessions"].as_array())
        .map(Vec::as_slice)
        .unwrap_or_default()
}
fn active_sessions(status: Option<&Value>) -> usize {
    entries(status)
        .iter()
        .filter(|s| matches!(s["status"]["state"].as_str(), Some("Active" | "Creating")))
        .count()
}
fn upstream<'a>(s: &'a Value, repo: &str) -> &'a Value {
    if s["status"]["manifest"]["repos"][repo]["tracked"] == false
        || !s["status"]["upstream"].is_object()
    {
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

pub fn long_deferral(auto: &Value, now: SystemTime) -> bool {
    auto["state"] == "deferred"
        && auto["reason"] != "conflicts"
        && auto["since"]
            .as_str()
            .and_then(timestamp)
            .and_then(|at| now.duration_since(at).ok())
            .is_some_and(|age| age > Duration::from_secs(86400))
}
fn stuck(s: &Value, repo: &str, enabled: bool, now: SystemTime) -> bool {
    let ws = &s["status"]["workspaces"][repo];
    let u = if enabled {
        upstream(s, repo)
    } else {
        &Value::Null
    };
    matches!(
        u["deps"]["state"].as_str(),
        Some("install_failed" | "own_change" | "drift" | "waiting_human")
    ) || ws["health"].as_str().is_some_and(|h| h != "Ok")
        || !ws["scene"].is_null()
        || !s["status"]["scene"].is_null()
        || u["following"] == false
        || u["diverged"] == true
        || long_deferral(&u["auto"], now)
}
pub fn waiting(sessions: Option<&Value>, deps: Option<&Value>) -> Vec<WaitingItem> {
    waiting_at(sessions, deps, false, SystemTime::now())
}
fn waiting_at(
    sessions: Option<&Value>,
    deps: Option<&Value>,
    enabled: bool,
    now: SystemTime,
) -> Vec<WaitingItem> {
    let ss = entries(sessions);
    let mut out = Vec::new();
    let mut covered = BTreeSet::new();
    if let Some(ps) = deps.and_then(|d| d["proposals"].as_array()) {
        let mut ps: Vec<_> = ps.iter().collect();
        ps.sort_by_key(|p| p["id"].as_u64());
        for p in ps {
            if p["state"]["kind"] != "Pending" {
                continue;
            }
            let (Some(id), Some(repo)) = (p["id"].as_u64(), p["repo"].as_str()) else {
                continue;
            };
            let affected: Vec<_> = ss
                .iter()
                .enumerate()
                .filter(|(_, s)| affected(s, repo, id))
                .collect();
            let applies = |s: &Value, mode: &str| {
                let u = upstream(s, repo);
                u["deps"]["proposal"].as_u64() == Some(id) && u["deps"]["mode"] == mode
            };
            let applying = match p["mode"].as_str() {
                Some("apply") => true,
                Some("confirm") => false,
                _ => {
                    enabled
                        && affected.iter().any(|(_, s)| applies(s, "apply"))
                        && !affected.iter().any(|(_, s)| applies(s, "confirm"))
                }
            };
            if applying && !affected.iter().any(|(_, s)| stuck(s, repo, enabled, now)) {
                continue;
            }
            if applying {
                for (idx, _) in &affected {
                    covered.insert((*idx, repo.to_string()));
                }
            }
            out.push(item(WaitingKind::DepsProposal, id, repo, affected.len()));
        }
    }
    let mut deferred = BTreeMap::<String, usize>::new();
    let mut conflicts = Vec::new();
    for (idx, s) in ss.iter().enumerate() {
        let Some(wss) = s["status"]["workspaces"].as_object() else {
            continue;
        };
        for (repo, ws) in wss {
            let u = if enabled {
                upstream(s, repo)
            } else {
                &Value::Null
            };
            if long_deferral(&u["auto"], now) && !covered.contains(&(idx, repo.clone())) {
                *deferred.entry(repo.clone()).or_default() += 1;
            }
            let n = ws["conflicts"]
                .as_array()
                .map_or(0, Vec::len)
                .max(u["conflicts"].as_array().map_or(0, Vec::len));
            if n > 0 || u["auto"]["reason"] == "conflicts" {
                conflicts.push(item(WaitingKind::Conflict, n.max(1) as u64, repo, 1));
            }
        }
    }
    for (repo, count) in deferred {
        out.push(item(WaitingKind::Deferred, 0, &repo, count));
    }
    out.extend(conflicts);
    out
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WarningClass {
    Error,
    Waiting,
    Notice,
    Activity,
}
pub fn warning_name(w: &Value) -> Option<&str> {
    w.as_str().or_else(|| {
        w.as_object()
            .filter(|o| o.len() == 1)
            .and_then(|o| o.keys().next().map(String::as_str))
    })
}
fn hold_since(hold: &Value) -> Option<SystemTime> {
    let since = &hold["since"];
    let secs = since["secs_since_epoch"].as_u64()?;
    let nanos = u32::try_from(since["nanos_since_epoch"].as_u64()?).ok()?;
    if nanos >= 1_000_000_000 {
        return None;
    }
    SystemTime::UNIX_EPOCH.checked_add(Duration::new(secs, nanos))
}
fn background_hold(hold: &Value, s: &Snapshot, now: SystemTime) -> bool {
    let activity_capable = s
        .daemon_status
        .value
        .as_ref()
        .and_then(|d| d["features"].as_array())
        .is_some_and(|fs| fs.iter().any(|f| f == "daemon_status.activity"));
    hold.get("origin").is_none()
        && !activity_capable
        && matches!(
            hold["method"].as_str(),
            Some("catch_up" | "old_tree" | "periodic_fetch" | "daemon")
        )
        && hold_since(hold)
            .and_then(|since| now.duration_since(since).ok())
            .is_some_and(|age| age < BACKGROUND_HOLD_MAX)
}
pub fn warning_class(w: &Value, s: &Snapshot, now: SystemTime) -> WarningClass {
    match warning_name(w).unwrap_or_default() {
        "StorePoisoned" | "VerifyFailed" | "UnknownPublish" | "UnknownPush" | "Quarantine"
        | "AttachOutside" => WarningClass::Error,
        "ConfigRelaxPending" => WarningClass::Waiting,
        "LongHold" if background_hold(&w["LongHold"], s, now) => WarningClass::Activity,
        _ => WarningClass::Notice,
    }
}
fn syncing_description(s: &Snapshot, now: SystemTime) -> String {
    let oldest = s.daemon_status.value.as_ref().and_then(|d| {
        let holds = d["holds"].as_array().into_iter().flatten();
        let long_holds = d["warnings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|w| w.get("LongHold"));
        holds
            .chain(long_holds)
            .filter(|h| h["method"] == "catch_up")
            .filter_map(|h| {
                let repo = h["repo"].as_str()?;
                let age = now.duration_since(hold_since(h)?).ok()?;
                Some((repo, age))
            })
            .max_by_key(|(_, age)| *age)
    });
    oldest.map_or_else(
        || "Bringing in newer changes".to_string(),
        |(repo, age)| format!("Bringing in newer changes: {repo}, {}", age_text(age)),
    )
}
fn syncing(sessions: Option<&Value>, enabled: bool, now: SystemTime) -> bool {
    enabled
        && entries(sessions).iter().any(|s| {
            s["status"]["workspaces"].as_object().is_some_and(|wss| {
                wss.keys().any(|repo| {
                    let u = upstream(s, repo);
                    let auto = &u["auto"];
                    auto["state"] == "pending"
                        || (auto["state"] == "waiting"
                            && matches!(
                                auto["reason"].as_str(),
                                Some("installing" | "waiting_install")
                            ))
                        || (auto["state"] == "deferred"
                            && auto["reason"] != "conflicts"
                            && !long_deferral(auto, now))
                })
            })
        })
}
pub fn model(s: &Snapshot, now: SystemTime) -> TrayModel {
    model_at(
        s,
        Stamp {
            wall: now,
            ..Stamp::now()
        },
    )
}
pub fn model_at(s: &Snapshot, now: Stamp) -> TrayModel {
    let daemon_fresh = s.daemon_status.fresh(now, STALE_AFTER);
    let sessions_fresh = s.sessions.fresh(now, s.sessions_limit());
    let deps_fresh = s.deps.fresh(now, Duration::from_secs(90));
    let enabled = s.upstream_enabled();
    let mut all = waiting_at(
        s.sessions.value.as_ref(),
        s.deps.value.as_ref(),
        enabled,
        now.wall,
    );
    let mut alerts = 0;
    let mut notices = 0;
    let mut relax = BTreeSet::new();
    if let Some(ws) = s
        .daemon_status
        .value
        .as_ref()
        .and_then(|d| d["warnings"].as_array())
    {
        for w in ws {
            if warning_name(w).is_none() {
                continue;
            }
            match warning_class(w, s, now.wall) {
                WarningClass::Error if daemon_fresh => alerts += 1,
                WarningClass::Notice if daemon_fresh => notices += 1,
                WarningClass::Waiting => {
                    if let Some(repo) = w["ConfigRelaxPending"]["repo"].as_str() {
                        relax.insert(repo.to_string());
                    }
                }
                _ => {}
            }
        }
    }
    for repo in relax {
        all.push(item(WaitingKind::ConfigRelax, 0, &repo, 0));
    }
    // Human actions precede agent conflicts; all counts are taken before truncation.
    all.sort_by_key(|w| w.kind == WaitingKind::Conflict);
    for w in &mut all {
        let inputs: Vec<(&Input, bool)> = match w.kind {
            WaitingKind::DepsProposal => vec![(&s.sessions, sessions_fresh), (&s.deps, deps_fresh)],
            WaitingKind::ConfigRelax => vec![(&s.daemon_status, daemon_fresh)],
            _ => vec![(&s.sessions, sessions_fresh)],
        };
        if inputs.iter().any(|(_, fresh)| !fresh) {
            w.stale_age = inputs
                .iter()
                .filter_map(|(i, _)| {
                    i.at.map(|at| Duration::from_secs(at.age(now).as_secs() / 60 * 60))
                })
                .max();
        }
    }
    let waiting = all
        .iter()
        .filter(|w| w.kind != WaitingKind::Conflict)
        .count();
    let conflicts = all
        .iter()
        .filter(|w| w.kind == WaitingKind::Conflict)
        .map(|w| &w.repo)
        .collect::<BTreeSet<_>>()
        .len();
    let unavailable = if enabled {
        entries(s.sessions.value.as_ref())
            .iter()
            .filter(|s| !s["status"]["upstream"].is_object())
            .count()
    } else {
        0
    };
    let stale_inputs = [
        (&s.daemon_status, daemon_fresh),
        (&s.sessions, sessions_fresh),
        (&s.deps, deps_fresh),
    ];
    let stale = if stale_inputs.iter().any(|(_, fresh)| !fresh)
        || s.last_down
            .is_some_and(|(at, _)| at.age(now) >= STALE_AFTER)
    {
        let oldest = stale_inputs
            .iter()
            .filter(|(_, fresh)| !fresh)
            .filter_map(|(i, _)| {
                i.at.map(|at| Duration::from_secs(at.age(now).as_secs() / 60 * 60))
            })
            .max();
        Some(oldest.map_or_else(
            || "Checking the daemon…".to_string(),
            |age| format!("Not updated for {}", age_text(age)),
        ))
    } else {
        None
    };
    let n = active_sessions(s.sessions.value.as_ref());
    let running = format!("Daemon running, {}", plural(n, "session", "sessions"));
    let (icon, description) =
        if let Some((_, why)) = s.last_down.filter(|(at, _)| at.age(now) < STALE_AFTER) {
            (
                Icon::Down,
                match why {
                    DownWhy::NotRunning => "Daemon isn't running",
                    DownWhy::NotResponding => "Daemon isn't responding",
                }
                .to_string(),
            )
        } else if s.fd_at.is_some_and(|at| at.age(now) < STALE_AFTER)
            || (daemon_fresh && s.fd_pressure)
            || alerts > 0
        {
            let base = if s.fd_at.is_some_and(|at| at.age(now) < STALE_AFTER) {
                "Daemon is out of file handles".to_string()
            } else if daemon_fresh && s.fd_pressure {
                "Daemon is close to its file-handle limit".to_string()
            } else {
                format!(
                    "Daemon needs attention: {}",
                    plural(alerts, "alert", "alerts")
                )
            };
            let suffix = if sessions_fresh
                && deps_fresh
                && (daemon_fresh || !all.iter().any(|w| w.kind == WaitingKind::ConfigRelax))
            {
                if waiting > 0 {
                    format!(", {waiting} waiting")
                } else {
                    String::new()
                }
            } else {
                ", waiting items not current".to_string()
            };
            (Icon::Alert, format!("{base}{suffix}"))
        } else if let Some(stale) = &stale {
            (Icon::Checking, stale.clone())
        } else if waiting > 0 {
            (Icon::Waiting, format!("{waiting} waiting for you"))
        } else if conflicts > 0 {
            (
                Icon::Conflict,
                format!(
                    "{} need{} an agent",
                    plural(conflicts, "repo", "repos"),
                    if conflicts == 1 { "s" } else { "" }
                ),
            )
        } else if unavailable > 0 {
            (
                Icon::Checking,
                "Some session details are unavailable".to_string(),
            )
        } else if syncing(s.sessions.value.as_ref(), enabled, now.wall) {
            (Icon::Syncing, syncing_description(s, now.wall))
        } else {
            (
                Icon::Normal,
                format!("All clear, {}", plural(n, "session", "sessions")),
            )
        };
    let mut status_line = if matches!(icon, Icon::Down | Icon::Alert | Icon::Checking) {
        description.clone()
    } else {
        running
    };
    if notices > 0 {
        status_line.push_str(&format!(" · {}", plural(notices, "notice", "notices")));
    }
    let more = all.len().saturating_sub(MAX_WAITING);
    let config_failed = s.daemon_status.value.as_ref().is_some_and(|d| {
        d["features"]
            .as_array()
            .is_some_and(|features| features.iter().any(|f| f == "config.reload"))
            && d["warnings"].as_array().is_some_and(|warnings| {
                warnings.iter().any(|w| {
                    w.get("ConfigReloadFailed").is_some()
                        || w.get("ConfigRestartRequired").is_some()
                })
            })
    });
    if daemon_fresh && config_failed {
        match icon {
            Icon::Down => {}
            Icon::Alert => status_line.push_str(" · config.toml not applied — open status"),
            _ => status_line = "config.toml not applied — open status".to_string(),
        }
    }
    TrayModel {
        icon,
        status_line,
        tooltip: format!("{}: {description}", crate::APP_NAME),
        waiting: all.into_iter().take(MAX_WAITING).collect(),
        more,
        stale,
        unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn config_warning_status_line_requires_feature_and_fresh_daemon() {
        let now = Stamp::now();
        let mut s = Snapshot {
            daemon_status: Input {
                value: Some(
                    json!({"features":["config.reload"], "warnings":[{"ConfigReloadFailed":{"reason":"parse"}}]}),
                ),
                at: Some(now),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            model_at(&s, now).status_line,
            "config.toml not applied — open status"
        );
        let stale = Stamp {
            instant: now.instant + STALE_AFTER + Duration::from_secs(1),
            wall: now.wall + STALE_AFTER + Duration::from_secs(1),
        };
        assert_ne!(
            model_at(&s, stale).status_line,
            "config.toml not applied — open status"
        );
        s.last_down = Some((now, DownWhy::NotRunning));
        assert_eq!(model_at(&s, now).icon, Icon::Down);
        assert_ne!(
            model_at(&s, now).status_line,
            "config.toml not applied — open status"
        );
        s.last_down = None;
        s.fd_pressure = true;
        let alert = model_at(&s, now);
        assert_eq!(alert.icon, Icon::Alert);
        assert!(
            alert
                .status_line
                .starts_with("Daemon is close to its file-handle limit")
        );
        assert!(alert.status_line.contains("config.toml not applied"));
        s.fd_pressure = false;
        s.daemon_status.value.as_mut().unwrap()["features"] = json!([]);
        assert_ne!(
            model_at(&s, now).status_line,
            "config.toml not applied — open status"
        );
    }
    fn fixture() -> (Value, Value) {
        (
            json!({"sessions":[{"status":{"upstream":{"repos":{}},"manifest":{"repos":{"tool":{"tracked":true}}},"workspaces":{"tool":{"health":"Ok","pending":[{"id":75}],"upstream":{"behind":3,"deps":{"proposal":75,"mode":"apply","state":"ready"},"auto":{"state":"deferred","reason":"open_files","since":"2026-11-01T12:00:00Z"}}}}}}]}),
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
            deps: Input {
                value: Some(deps),
                at: Some(Stamp {
                    wall: now,
                    ..Stamp::now()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(model(&snapshot, now).icon, Icon::Checking);
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
