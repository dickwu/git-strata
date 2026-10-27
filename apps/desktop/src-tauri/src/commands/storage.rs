use serde_json::Value;
use tauri::State;

use crate::daemon::Read;
use crate::error::GuiError;
use crate::state::AppState;

#[tauri::command]
pub async fn du(state: State<'_, AppState>) -> Result<Value, GuiError> {
    state.read(Read::Du).await
}
