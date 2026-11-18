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
    match crate::update::check::checker() {
        Some(checker) => Ok(checker.state()),
        None => unavailable(),
    }
}

#[tauri::command]
pub async fn update_check() -> Result<UpdateState, GuiError> {
    match crate::update::check::checker() {
        Some(checker) => checker.manual().await,
        None => unavailable(),
    }
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
    use tauri::Listener;

    #[derive(Clone, Copy, PartialEq, Eq, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Action {
        Continue,
        Cancel,
        CheckRepair,
    }

    main_only(window.label())?;
    let coordinator = tauri::Manager::try_state::<crate::update::move_daemon::MoveDaemon>(&window)
        .ok_or_else(|| {
            GuiError::new(
                Code::Unsupported,
                "Moving the daemon into the app is unavailable in this build.",
                json!({ "reason": "move_unavailable" }),
            )
        })?;
    // Keep the T00 command signature. The progress channel identifies this
    // invocation's action listener on the main window. Checking is sent
    // only after that listener is ready, before the coordinator can mutate.
    let (actions, mut requested) = tokio::sync::watch::channel(None);
    let listener = window.listen(
        format!("git-strata-move-action-{}", on_progress.id()),
        move |event| {
            if let Ok(action) = serde_json::from_str::<Action>(event.payload()) {
                actions.send_if_modified(|current| {
                    if current.is_none()
                        || (*current == Some(Action::Continue) && action == Action::Cancel)
                    {
                        *current = Some(action);
                        true
                    } else {
                        false
                    }
                });
            }
        },
    );
    let result = async {
        on_progress
            .send(UpdateState::Checking)
            .map_err(|e| GuiError::internal(format!("The Move panel is unavailable: {e}")))?;
        let action = *tokio::time::timeout(
            std::time::Duration::from_secs(10),
            requested.wait_for(Option::is_some),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .ok_or_else(|| {
            GuiError::new(
                Code::Busy,
                "The Move panel did not confirm the action. Try again.",
                json!({ "reason": "move_action_timeout" }),
            )
        })?;
        let mut progress = |state| {
            let _ = on_progress.send(state);
        };
        match action {
            Some(Action::Continue) => {
                coordinator
                    .run_cancellable(
                        schema_acknowledgement,
                        None,
                        &|| *requested.borrow() == Some(Action::Cancel),
                        &mut progress,
                    )
                    .await
            }
            Some(Action::Cancel) => coordinator.cancel_pending(&mut progress).await,
            Some(Action::CheckRepair) => coordinator.check_repair(&mut progress).await,
            None => unreachable!("Move action was received"),
        }
    }
    .await;
    window.unlisten(listener);
    result
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
