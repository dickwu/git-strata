//! Shared snapshots are recorded by blocking read flights, never by their waiters.
use crate::{
    daemon::{Daemon, Read},
    error::GuiError,
    tray_snapshot::{Snapshot, Stamp},
};
use serde_json::Value;
use std::sync::{Arc, Mutex};

type OnChange = Arc<dyn Fn() + Send + Sync>;
#[derive(Default)]
pub(crate) struct SnapshotStore {
    pub snapshot: Mutex<Snapshot>,
    on_change: Mutex<Option<OnChange>>,
}
impl SnapshotStore {
    pub fn record(
        &self,
        r: &Read,
        generation: Option<u64>,
        started: Stamp,
        completed: Stamp,
        result: &Result<Value, GuiError>,
    ) {
        self.snapshot
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .record(r, generation, started, completed, result);
    }
    pub fn attempt(&self, r: &Read, now: Stamp) {
        if let Some(input) = self
            .snapshot
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .input_mut(r)
        {
            input.last_attempt = Some(now);
        }
    }
    pub fn timeout(&self, r: &Read, now: Stamp) {
        self.snapshot
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .timeout(r, now);
        self.notify();
    }
    pub fn notify(&self) {
        let callback = self
            .on_change
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Some(callback) = callback {
            callback();
        }
    }
}

pub struct AppState {
    daemon: Option<Daemon>,
    store: Arc<SnapshotStore>,
}
impl AppState {
    pub fn new(daemon: Option<Daemon>) -> Self {
        let store = daemon.as_ref().map(|d| d.store.clone()).unwrap_or_default();
        Self { daemon, store }
    }
    pub fn set_on_change(&self, f: impl Fn() + Send + Sync + 'static) {
        *self
            .store
            .on_change
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(Arc::new(f));
    }
    pub fn socket(&self) -> Option<String> {
        self.daemon
            .as_ref()
            .map(|d| d.socket().to_string_lossy().into_owned())
    }
    pub fn snapshot(&self) -> Snapshot {
        self.store
            .snapshot
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
    pub async fn read(&self, r: Read) -> Result<Value, GuiError> {
        self.daemon
            .as_ref()
            .ok_or_else(GuiError::no_home)?
            .read(r)
            .await
    }
    pub async fn poll_read(&self, r: Read) -> Result<Value, GuiError> {
        self.daemon
            .as_ref()
            .ok_or_else(GuiError::no_home)?
            .poll_read(r)
            .await
    }
    /// A write is never a tray input. The window refetches after every outcome.
    pub async fn deps_approve(
        &self,
        proposal: u64,
        sessions: Option<Vec<String>>,
    ) -> Result<Value, GuiError> {
        self.daemon
            .as_ref()
            .ok_or_else(GuiError::no_home)?
            .deps_approve(proposal, sessions)
            .await
    }
}
