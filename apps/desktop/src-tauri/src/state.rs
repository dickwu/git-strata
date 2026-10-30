//! The state the commands share: the daemon handle and the last replies
//! (for the menu-bar item, which has no reads of its own in P1).

use std::sync::Mutex;
use std::time::SystemTime;

use serde_json::Value;

use crate::daemon::{Daemon, Read};
use crate::error::GuiError;
use crate::tray_model::Snapshot;

type OnChange = Box<dyn Fn(&Snapshot) + Send + Sync>;

pub struct AppState {
    daemon: Option<Daemon>,
    snapshot: Mutex<Snapshot>,
    on_change: Mutex<Option<OnChange>>,
}

impl AppState {
    /// `daemon` is `None` when HOME is unset: every read then fails with
    /// `no_home` instead of guessing a socket.
    pub fn new(daemon: Option<Daemon>) -> AppState {
        AppState {
            daemon,
            snapshot: Mutex::new(Snapshot::default()),
            on_change: Mutex::new(None),
        }
    }

    pub fn set_on_change(&self, f: impl Fn(&Snapshot) + Send + Sync + 'static) {
        *self.on_change.lock().unwrap_or_else(|p| p.into_inner()) = Some(Box::new(f));
    }

    pub fn socket(&self) -> Option<String> {
        self.daemon
            .as_ref()
            .map(|d| d.socket().to_string_lossy().into_owned())
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snapshot
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub async fn read(&self, r: Read) -> Result<Value, GuiError> {
        let d = self.daemon.as_ref().ok_or_else(GuiError::no_home)?;
        let res = d.read(r.clone()).await;
        self.record(&r, &res, SystemTime::now());
        res
    }

    /// `deps_approve` (daemon.rs). Not recorded in the snapshot: the front
    /// end refetches deps and sessions after every outcome, and those
    /// reads update the menu bar.
    pub async fn deps_approve(
        &self,
        proposal: u64,
        sessions: Option<Vec<String>>,
    ) -> Result<Value, GuiError> {
        let d = self.daemon.as_ref().ok_or_else(GuiError::no_home)?;
        d.deps_approve(proposal, sessions).await
    }

    /// A reply says the daemon is up; `daemon_down` and `daemon_timeout`
    /// say it is not. Other errors (a refused request, one already in
    /// flight) say nothing about liveness and leave the snapshot alone.
    pub fn record(&self, r: &Read, res: &Result<Value, GuiError>, now: SystemTime) {
        let snap = {
            let mut s = self.snapshot.lock().unwrap_or_else(|p| p.into_inner());
            match res {
                Ok(v) => {
                    s.up = Some(true);
                    s.updated = Some(now);
                    match r {
                        Read::Sessions => s.sessions = Some(v.clone()),
                        Read::DepsList => s.deps = Some(v.clone()),
                        _ => {}
                    }
                }
                Err(e) if matches!(e.reason(), Some("daemon_down" | "daemon_timeout")) => {
                    s.up = Some(false);
                    s.updated = Some(now);
                }
                Err(_) => return,
            }
            s.clone()
        };
        if let Some(f) = self
            .on_change
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            f(&snap);
        }
    }
}
