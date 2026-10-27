use serde_json::Value;
use tauri::State;

use crate::daemon::Read;
use crate::error::GuiError;
use crate::state::AppState;

#[tauri::command]
pub async fn deps_list(state: State<'_, AppState>) -> Result<Value, GuiError> {
    state.read(Read::DepsList).await
}
