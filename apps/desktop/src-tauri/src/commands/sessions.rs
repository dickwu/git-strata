use serde_json::Value;
use tauri::State;

use crate::daemon::Read;
use crate::error::GuiError;
use crate::state::AppState;

/// Every session, from `status{all:true}`. Slow on a busy store: the
/// front end polls it rarely. P2 replaces it with `sessions_summary`.
#[tauri::command]
pub async fn sessions_list(state: State<'_, AppState>) -> Result<Value, GuiError> {
    state.read(Read::Sessions).await
}

#[tauri::command]
pub async fn session_status(
    state: State<'_, AppState>,
    session: String,
) -> Result<Value, GuiError> {
    state.read(Read::Session(session)).await
}
