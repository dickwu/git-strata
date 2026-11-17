//! Updater check and download interface.

use serde_json::Value;

use super::UpdateFuture;
use crate::error::GuiError;

/// Metadata returned by the update check. Manifest parsing uses `raw_json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateCandidate {
    pub version: String,
    pub notes: Option<String>,
    pub raw_json: Value,
}

/// A verified archive and the install operation from the same checked update.
pub struct VerifiedUpdate {
    pub bytes: Vec<u8>,
    /// Captures the updater that verified these bytes, including its signature and install destination.
    pub installer: Box<dyn FnOnce(&[u8]) -> tauri_plugin_updater::Result<()> + Send>,
}

pub trait UpdateSource: Send + Sync {
    fn check(&self) -> UpdateFuture<'_, Result<Option<UpdateCandidate>, GuiError>>;

    /// Download and verify the checked target, reporting chunk bytes and the optional total.
    fn download<'a>(
        &'a self,
        target: &'a str,
        on_progress: &'a mut (dyn FnMut(usize, Option<u64>) + Send),
    ) -> UpdateFuture<'a, Result<VerifiedUpdate, GuiError>>;
}
