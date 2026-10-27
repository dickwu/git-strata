//! S0: the legacy agent's state and the Start button (gui-daemon-setup
//! §6.1 `legacy_down`). Both run launchctl, so both answer the main window
//! only: the app ACL (build.rs) grants them to `main` in
//! capabilities/default.json, and `main_only` checks the label again.

use tauri::ipc::Channel;
use tauri::{State, WebviewWindow};

use crate::daemon::Read;
use crate::error::GuiError;
use crate::service::{LegacyAgent, Progress, Started, Starter, main_only};
use crate::state::AppState;

#[tauri::command]
pub async fn legacy_agent(
    window: WebviewWindow,
    starter: State<'_, Starter>,
) -> Result<LegacyAgent, GuiError> {
    main_only(window.label())?;
    starter.agent().await
}

/// `kickstart` (loaded) or `bootstrap` (plist, not loaded), then poll
/// `daemon_status` until it answers. Never stops anything.
#[tauri::command]
pub async fn daemon_start(
    window: WebviewWindow,
    starter: State<'_, Starter>,
    state: State<'_, AppState>,
    on_progress: Channel<Progress>,
) -> Result<Started, GuiError> {
    main_only(window.label())?;
    let mut send = move |p: Progress| {
        let _ = on_progress.send(p);
    };
    let state = state.inner();
    starter
        .start(|| state.read(Read::DaemonStatus), &mut send)
        .await
}
