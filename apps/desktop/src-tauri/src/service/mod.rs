//! S0 (gui-daemon-setup design §6.1 `legacy_down`, §13 Q3): start the
//! existing `com.gwd.git-stratad` launch agent from the app, the same way
//! `git-strata daemon start` does, so a down daemon needs no terminal.
//!
//! Start only. The GUI never stops, boots out or signals the agent: the
//! trait has no such method and `launchctl.rs` has no such verb (a static
//! test below holds both). With no plist the GUI reports
//! `plist_missing` and writes nothing; installing a plist from the app is
//! S1 territory.
//!
//! The decision (`kickstart` when loaded, `bootstrap` of the plist when
//! not) and the wait for the socket live here, over [`ServiceManager`], so
//! the tests drive them with a fake and never run launchctl.
//!
//! cli.md D38 (scope.rs): only when HOME is the login home, and only an
//! agent launchd loaded from that home's plist; anything else is
//! `foreign_home` / `foreign_agent` before a kickstart or bootstrap.

pub mod launchctl;
pub mod scope;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};
use strata_types::Code;

use crate::error::GuiError;

/// The legacy agent's label (strata_daemon::AGENT_LABEL; the desktop may
/// not depend on strata-daemon, gui.md §2.3).
pub const AGENT_LABEL: &str = "com.gwd.git-stratad";

/// What to tell someone who would rather use a terminal.
pub const CLI_START: &str = "git-strata daemon start";

/// `~/Library/LaunchAgents/com.gwd.git-stratad.plist`
/// (strata_daemon::plist::agent_plist_path).
pub fn plist_for_home(home: &Path) -> PathBuf {
    home.join("Library")
        .join("LaunchAgents")
        .join(format!("{AGENT_LABEL}.plist"))
}

/// The only launchd operations the GUI has. No stop, no bootout, no kill.
pub trait ServiceManager: Send + Sync {
    /// launchd's dump of the job in `gui/<uid>` (`launchctl print`), or
    /// `None` when launchd does not have it.
    fn print(&self, label: &str) -> Result<Option<String>, GuiError>;
    /// Start a loaded job that is not running. Without `-k`: a running
    /// job is left alone.
    fn kickstart(&self, label: &str) -> Result<(), GuiError>;
    /// Load the job from this exact plist into `gui/<uid>`; `RunAtLoad`
    /// starts it.
    fn bootstrap(&self, plist: &Path) -> Result<(), GuiError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    /// launchd has the job; Start kickstarts it.
    Loaded,
    /// The plist is there but launchd does not have the job; Start
    /// bootstraps it.
    Installed,
    /// No plist and not loaded: setup is needed, Start is not offered.
    NotInstalled,
    /// D38: HOME is not the login home, or the loaded agent comes from
    /// another plist. Start is not offered; `refusal` says which.
    Foreign,
}

/// The `legacy_agent` reply: enough for the down notice to pick its variant.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LegacyAgent {
    pub state: AgentState,
    pub label: &'static str,
    pub plist: String,
    pub command: &'static str,
    /// `foreign_home` / `foreign_agent` when `state` is `Foreign`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<GuiError>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Kickstart,
    Bootstrap,
}

/// Progress for the Start button, sent over a channel while it runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum Progress {
    /// Asking launchd (`kickstart` or `bootstrap`).
    Launching {
        action: Action,
    },
    /// launchd took it; the socket does not answer yet.
    Waiting,
    /// The daemon answers `Busy` while it waits for its store (waiting.rs).
    /// `tcc` when the error looks like macOS still deciding on the
    /// removable volume (EPERM / EACCES / EINTR).
    WaitingStore {
        tcc: bool,
    },
    /// The store opened; recovery is running.
    Recovering,
    Ready,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Started {
    pub action: Action,
    pub waited_ms: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct WaitPolicy {
    pub interval: Duration,
    pub timeout: Duration,
}

impl WaitPolicy {
    /// The daemon binds its socket before it opens the store, so a healthy
    /// start answers within a second or two; 30 s covers a slow login.
    pub const PRODUCTION: WaitPolicy = WaitPolicy {
        interval: Duration::from_millis(500),
        timeout: Duration::from_secs(30),
    };
}

/// errno values the daemon's store wait reports while TCC has not decided
/// (waiting.rs: EINTR while the prompt is up, EPERM / EACCES before consent).
const TCC_ERRNOS: [i64; 3] = [1, 4, 13];

/// How one `daemon_status` probe reads during a start.
#[derive(Clone, Debug, PartialEq)]
enum Seen {
    Ready,
    Down,
    Store { tcc: bool, message: String },
    Recovering,
}

fn classify(res: &Result<Value, GuiError>) -> Seen {
    let e = match res {
        Ok(_) => return Seen::Ready,
        Err(e) => e,
    };
    match e.reason() {
        Some("store_unavailable") => {
            let last = e.detail.get("last_error");
            let errno = last
                .and_then(|l| l.get("detail"))
                .and_then(|d| d.get("errno"))
                .and_then(Value::as_i64);
            let message = last
                .and_then(|l| l.get("message"))
                .and_then(Value::as_str)
                .unwrap_or(&e.message)
                .to_string();
            Seen::Store {
                tcc: errno.is_some_and(|n| TCC_ERRNOS.contains(&n)),
                message,
            }
        }
        Some("starting") => Seen::Recovering,
        // daemon_down, daemon_timeout, request_in_flight (the window's own
        // poll got there first) and anything else: keep waiting.
        _ => Seen::Down,
    }
}

impl GuiError {
    pub fn plist_missing(plist: &Path) -> GuiError {
        GuiError::new(
            Code::InvalidInput,
            format!("no launch agent at {}", plist.display()),
            json!({ "reason": "plist_missing", "plist": plist.to_string_lossy(), "label": AGENT_LABEL }),
        )
    }

    pub fn launchctl_failed(step: &str, status: Option<i32>, stderr: &str) -> GuiError {
        let text = stderr.trim();
        GuiError::new(
            Code::Unknown,
            if text.is_empty() {
                format!("launchctl {step} failed")
            } else {
                format!("launchctl {step} failed: {text}")
            },
            json!({ "reason": "launchctl_failed", "step": step, "status": status, "stderr": text }),
        )
    }

    fn start_waited(seen: &Seen, secs: u64) -> GuiError {
        match seen {
            Seen::Store { tcc: true, message } => GuiError::new(
                Code::Busy,
                format!("the daemon is waiting for macOS to allow access to its volume: {message}"),
                json!({ "reason": "tcc_pending", "seconds": secs, "store_error": message }),
            ),
            Seen::Store {
                tcc: false,
                message,
            } => GuiError::new(
                Code::Busy,
                format!("the daemon started but cannot open its store: {message}"),
                json!({ "reason": "store_unavailable", "seconds": secs, "store_error": message }),
            ),
            _ => GuiError::new(
                Code::Busy,
                format!("launchd took the start, but the daemon did not answer within {secs} s"),
                json!({ "reason": "start_timeout", "seconds": secs }),
            ),
        }
    }

    pub fn start_in_flight() -> GuiError {
        GuiError::new(
            Code::Busy,
            "a start is already in progress",
            json!({ "reason": "start_in_flight" }),
        )
    }

    pub fn not_main_window(label: &str) -> GuiError {
        GuiError::new(
            Code::InvalidInput,
            format!("only the main window may start the daemon (called from `{label}`)"),
            json!({ "reason": "not_main_window", "window": label }),
        )
    }
}

/// Commands that run launchctl answer only the main window (the
/// capability already scopes them to it; this is the second lock).
pub fn main_only(label: &str) -> Result<(), GuiError> {
    if label == "main" {
        Ok(())
    } else {
        Err(GuiError::not_main_window(label))
    }
}

/// `plist` is this (login) home's plist; a loaded agent from any other
/// plist is `foreign_agent` (D38).
pub fn agent_state(sm: &dyn ServiceManager, plist: &Path) -> Result<AgentState, GuiError> {
    if let Some(dump) = sm.print(AGENT_LABEL)? {
        scope::owned(&dump, plist)?;
        Ok(AgentState::Loaded)
    } else if plist.is_file() {
        Ok(AgentState::Installed)
    } else {
        Ok(AgentState::NotInstalled)
    }
}

/// `kickstart` when loaded, `bootstrap` of the plist when installed,
/// `plist_missing` otherwise.
pub fn choose(state: AgentState, plist: &Path) -> Result<Action, GuiError> {
    match state {
        AgentState::Loaded => Ok(Action::Kickstart),
        AgentState::Installed => Ok(Action::Bootstrap),
        AgentState::NotInstalled => Err(GuiError::plist_missing(plist)),
        // agent_state returns the refusal itself; never a state to act on.
        AgentState::Foreign => Err(GuiError::internal("refusing a foreign launch agent")),
    }
}

/// The one launchctl verb a start runs after `print`.
pub fn run(sm: &dyn ServiceManager, action: Action, plist: &Path) -> Result<(), GuiError> {
    match action {
        Action::Kickstart => sm.kickstart(AGENT_LABEL),
        Action::Bootstrap => sm.bootstrap(plist),
    }
}

/// Probe until the daemon answers or `policy.timeout` passes. Progress is
/// sent only when it changes.
pub async fn wait_ready<F, Fut>(
    mut probe: F,
    policy: WaitPolicy,
    progress: &mut (dyn FnMut(Progress) + Send),
) -> Result<Duration, GuiError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Value, GuiError>>,
{
    let started = Instant::now();
    let mut last = Seen::Down;
    let mut sent: Option<Progress> = None;
    let mut send = |p: Progress, progress: &mut (dyn FnMut(Progress) + Send)| {
        if sent.as_ref() != Some(&p) {
            sent = Some(p.clone());
            progress(p);
        }
    };
    send(Progress::Waiting, progress);
    loop {
        let seen = classify(&probe().await);
        match &seen {
            Seen::Ready => {
                send(Progress::Ready, progress);
                return Ok(started.elapsed());
            }
            Seen::Down => {}
            Seen::Store { tcc, .. } => send(Progress::WaitingStore { tcc: *tcc }, progress),
            Seen::Recovering => send(Progress::Recovering, progress),
        }
        // A daemon that has answered once and then drops (a respawn) keeps
        // the more telling earlier reading.
        if seen != Seen::Down {
            last = seen;
        }
        if started.elapsed() + policy.interval > policy.timeout {
            return Err(GuiError::start_waited(&last, policy.timeout.as_secs()));
        }
        tokio::time::sleep(policy.interval).await;
    }
}

/// The managed state behind `legacy_agent` / `daemon_start`.
pub struct Starter {
    home: Option<PathBuf>,
    /// The login home (D38); `None` when it cannot be resolved, which
    /// refuses every start.
    login: Option<PathBuf>,
    sm: Arc<dyn ServiceManager>,
    policy: WaitPolicy,
    busy: AtomicBool,
}

struct Slot<'a>(&'a AtomicBool);
impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Starter {
    pub fn new(
        home: Option<PathBuf>,
        login: Option<PathBuf>,
        sm: Arc<dyn ServiceManager>,
        policy: WaitPolicy,
    ) -> Starter {
        Starter {
            home,
            login,
            sm,
            policy,
            busy: AtomicBool::new(false),
        }
    }

    /// HOME from the environment (the socket uses the same), the login
    /// home from the password database, the real launchctl, the
    /// production wait.
    pub fn from_env() -> Starter {
        let home = std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from);
        Starter::new(
            home,
            scope::login_home(),
            Arc::new(launchctl::Launchctl::new()),
            WaitPolicy::PRODUCTION,
        )
    }

    fn home(&self) -> Result<&Path, GuiError> {
        self.home.as_deref().ok_or_else(GuiError::no_home)
    }

    /// D38: the plist of the canonical login home, or `foreign_home`
    /// before any launchctl.
    fn plist(&self) -> Result<PathBuf, GuiError> {
        let home = scope::lifecycle_scope(self.home()?, self.login.as_deref())?;
        Ok(plist_for_home(&home))
    }

    pub async fn agent(&self) -> Result<LegacyAgent, GuiError> {
        let home = self.home()?;
        let foreign = |plist: &Path, e: GuiError| LegacyAgent {
            state: AgentState::Foreign,
            label: AGENT_LABEL,
            plist: plist.to_string_lossy().into_owned(),
            command: CLI_START,
            refusal: Some(e),
        };
        let plist = match self.plist() {
            Ok(p) => p,
            Err(e) if e.reason() == Some("foreign_home") => {
                return Ok(foreign(&plist_for_home(home), e));
            }
            Err(e) => return Err(e),
        };
        let sm = self.sm.clone();
        let p = plist.clone();
        let state = match tokio::task::spawn_blocking(move || agent_state(sm.as_ref(), &p))
            .await
            .map_err(|e| GuiError::internal(format!("agent check panicked: {e}")))?
        {
            Ok(s) => s,
            Err(e) if e.reason() == Some("foreign_agent") => return Ok(foreign(&plist, e)),
            Err(e) => return Err(e),
        };
        Ok(LegacyAgent {
            state,
            label: AGENT_LABEL,
            plist: plist.to_string_lossy().into_owned(),
            command: CLI_START,
            refusal: None,
        })
    }

    /// One start at a time; launchctl on a blocking thread, then the wait.
    pub async fn start<F, Fut>(
        &self,
        probe: F,
        progress: &mut (dyn FnMut(Progress) + Send),
    ) -> Result<Started, GuiError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<Value, GuiError>>,
    {
        let plist = self.plist()?;
        if self.busy.swap(true, Ordering::AcqRel) {
            return Err(GuiError::start_in_flight());
        }
        let _slot = Slot(&self.busy);
        let sm = self.sm.clone();
        let state = {
            let sm = sm.clone();
            let p = plist.clone();
            tokio::task::spawn_blocking(move || agent_state(sm.as_ref(), &p))
                .await
                .map_err(|e| GuiError::internal(format!("agent check panicked: {e}")))??
        };
        let action = choose(state, &plist)?;
        progress(Progress::Launching { action });
        let p = plist.clone();
        tokio::task::spawn_blocking(move || run(sm.as_ref(), action, &p))
            .await
            .map_err(|e| GuiError::internal(format!("launchctl call panicked: {e}")))??;
        let waited = wait_ready(probe, self.policy, progress).await?;
        Ok(Started {
            action,
            waited_ms: waited.as_millis() as u64,
        })
    }
}

#[cfg(test)]
mod tests;
