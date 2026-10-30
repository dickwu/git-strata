use serde_json::Value;
use tauri::State;

use crate::daemon::Read;
use crate::error::GuiError;
use crate::state::AppState;

#[tauri::command]
pub async fn daemon_status(state: State<'_, AppState>) -> Result<Value, GuiError> {
    state.read(Read::DaemonStatus).await
}

/// The socket the GUI talks to, for the Daemon screen.
#[tauri::command]
pub fn daemon_socket(state: State<'_, AppState>) -> Result<String, GuiError> {
    state.socket().ok_or_else(GuiError::no_home)
}
