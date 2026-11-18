//! IPC scaffolding for the desktop update and daemon setup.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::json;
use strata_types::Code;
use tauri::{WebviewWindow, ipc::Channel};

use crate::error::GuiError;
use crate::service::main_only;
use crate::update::state::UpdateState;
use crate::update::{cli_link, setup};

fn unavailable<T>() -> Result<T, GuiError> {
    Err(GuiError::new(
        Code::Unsupported,
        "desktop update command is unavailable",
        json!({ "reason": "not_implemented" }),
    ))
}

#[tauri::command]
pub fn update_state() -> Result<UpdateState, GuiError> {
    unavailable()
}

#[tauri::command]
pub async fn update_check() -> Result<UpdateState, GuiError> {
    unavailable()
}

#[tauri::command]
pub async fn update_install(
    window: WebviewWindow,
    target: String,
    schema_acknowledgement: Option<u32>,
    on_progress: Channel<UpdateState>,
) -> Result<(), GuiError> {
    main_only(window.label())?;
    let _ = (target, schema_acknowledgement, on_progress);
    unavailable()
}

#[tauri::command]
pub async fn update_cancel(window: WebviewWindow) -> Result<(), GuiError> {
    main_only(window.label())?;
    unavailable()
}

#[tauri::command]
pub async fn daemon_restart(
    window: WebviewWindow,
    schema_acknowledgement: Option<u32>,
    on_progress: Channel<UpdateState>,
) -> Result<(), GuiError> {
    main_only(window.label())?;
    let _ = (schema_acknowledgement, on_progress);
    unavailable()
}

#[tauri::command]
pub async fn daemon_move_into_app(
    window: WebviewWindow,
    schema_acknowledgement: Option<u32>,
    on_progress: Channel<UpdateState>,
) -> Result<(), GuiError> {
    main_only(window.label())?;
    let _ = (schema_acknowledgement, on_progress);
    unavailable()
}

/// Setup with a retained store requires acknowledgement of its current review (§6.3).
#[tauri::command]
pub async fn daemon_setup(
    window: WebviewWindow,
    data_root: Option<PathBuf>,
    repos: Option<BTreeMap<String, PathBuf>>,
    schema_acknowledgement: Option<u32>,
    setup_review_id: Option<String>,
    on_progress: Channel<UpdateState>,
) -> Result<(), GuiError> {
    main_only(window.label())?;
    if data_root.is_some() || repos.is_some() {
        return Err(GuiError::new(
            Code::InvalidInput,
            "Choose setup folders in the macOS folder picker.",
            json!({ "reason": "setup_native_picker_required" }),
        ));
    }
    setup::run(
        &window,
        schema_acknowledgement,
        setup_review_id.as_deref(),
        on_progress,
    )
    .await
}

#[tauri::command]
pub async fn cli_link_install(window: WebviewWindow) -> Result<(), GuiError> {
    main_only(window.label())?;
    cli_link::install(&window).await
}

#[tauri::command]
pub async fn update_rollback(
    window: WebviewWindow,
    on_progress: Channel<UpdateState>,
) -> Result<(), GuiError> {
    main_only(window.label())?;
    let _ = on_progress;
    unavailable()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn update_stubs_report_unsupported() {
        for error in [
            update_state().unwrap_err(),
            update_check().await.unwrap_err(),
            unavailable::<()>().unwrap_err(),
        ] {
            assert_eq!(error.code, Code::Unsupported.as_str());
            assert_eq!(error.reason(), Some("not_implemented"));
        }
    }
}
