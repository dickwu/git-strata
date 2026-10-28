use serde_json::Value;
use tauri::{State, WebviewWindow};

use crate::daemon::Read;
use crate::error::GuiError;
use crate::service::main_only;
use crate::state::AppState;

#[tauri::command]
pub async fn deps_list(state: State<'_, AppState>) -> Result<Value, GuiError> {
    state.read(Read::DepsList).await
}

/// gui-approve D1 (Option A) / D2=b: send `deps_approve{proposal,
/// sessions}` to the daemon, which shows the macOS prompt naming the
/// proposal and moves nothing unless the human passes it. The reply (or
/// the daemon's error) is returned as is; the front end classifies it.
/// Main window only, like the other non-read commands.
#[tauri::command]
pub async fn deps_approve(
    window: WebviewWindow,
    state: State<'_, AppState>,
    proposal: u64,
    sessions: Option<Vec<String>>,
) -> Result<Value, GuiError> {
    main_only(window.label())?;
    state.deps_approve(proposal, sessions).await
}
