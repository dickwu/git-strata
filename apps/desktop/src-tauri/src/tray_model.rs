//! gui.md §5.8: what the menu-bar item shows, as plain data built from
//! the daemon's raw replies. Kept free of tauri so it is tested directly.
//!
//! G7: a "Waiting for you" item names only its kind, its number and the
//! repository. Tickets, commit titles, checkpoint messages, paths and
//! proposal reasons never reach the menu: the labels below are assembled
//! from those three fields and nothing else.

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
}

impl WaitingItem {
    pub fn label(&self) -> String {
        match self.kind {
            WaitingKind::DepsProposal => format!("Deps proposal {} · {}", self.number, self.repo),
            WaitingKind::Conflict if self.number == 1 => format!("1 conflict · {}", self.repo),
            WaitingKind::Conflict => format!("{} conflicts · {}", self.number, self.repo),
        }
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

/// Pending deps proposals (they wait for a human to approve), then
/// workspaces with conflicted paths, in reply order.
pub fn waiting(sessions: Option<&Value>, deps: Option<&Value>) -> Vec<WaitingItem> {
    let mut out = Vec::new();
    if let Some(ps) = deps.and_then(|d| d["proposals"].as_array()) {
        for p in ps {
            if p["state"]["kind"].as_str() != Some("Pending") {
                continue;
            }
            if let (Some(id), Some(repo)) = (p["id"].as_u64(), p["repo"].as_str()) {
                out.push(WaitingItem {
                    kind: WaitingKind::DepsProposal,
                    number: id,
                    repo: repo.to_string(),
                });
            }
        }
    }
    if let Some(ss) = sessions.and_then(|s| s["sessions"].as_array()) {
        for s in ss {
            let Some(wss) = s["status"]["workspaces"].as_object() else {
                continue;
            };
            for (repo, ws) in wss {
                let n = ws["conflicts"].as_array().map_or(0, Vec::len);
                if n > 0 {
                    out.push(WaitingItem {
                        kind: WaitingKind::Conflict,
                        number: n as u64,
                        repo: repo.clone(),
                    });
                }
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
    let all = waiting(s.sessions.as_ref(), s.deps.as_ref());
    let more = all.len().saturating_sub(MAX_WAITING);
    let shown: Vec<_> = all.into_iter().take(MAX_WAITING).collect();
    let (icon, status_line) = match (s.up, stale) {
        (None, _) => (Icon::Down, "Checking the daemon…".to_string()),
        (Some(false), _) => (Icon::Down, "Daemon isn't running".to_string()),
        (Some(true), Some(age)) if age > STALE_AFTER => (Icon::Down, ago(age)),
        (Some(true), _) => {
            let line = match s.sessions.as_ref().map(active_sessions) {
                Some(1) => "Daemon running · 1 session".to_string(),
                Some(n) => format!("Daemon running · {n} sessions"),
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
