//! Main-window entry points for the desktop updater and daemon lifecycle.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::json;
use strata_types::Code;
use tauri::{Manager, WebviewWindow, ipc::Channel};

use crate::error::GuiError;
use crate::service::main_only;
use crate::update::state::UpdateState;
use crate::update::{cli_link, setup};

fn unavailable(reason: &str) -> GuiError {
    GuiError::new(
        Code::Unsupported,
        "Open the installed GIT STRATA app in Applications",
        json!({"reason":reason}),
    )
}

fn start_presentation(
    window: &WebviewWindow,
) -> Result<tauri::State<'_, crate::update::state::Presentation>, GuiError> {
    let view = window.state::<crate::update::state::Presentation>();
    if !view.begin() {
        return Err(GuiError::new(
            Code::Busy,
            "An update operation is already in progress",
            json!({"reason":"update_in_flight"}),
        ));
    }
    Ok(view)
}

#[tauri::command]
pub async fn update_state(window: WebviewWindow) -> Result<serde_json::Value, GuiError> {
    use crate::update::{
        Installed, cache, check, install::InstallCoordinator, move_daemon::MoveDaemon,
        state::Presentation,
    };
    use std::sync::Arc;

    main_only(window.label())?;
    let ready = check::checker().map_or(UpdateState::Idle, |checker| checker.state());
    let presentation = window.state::<Presentation>();
    let Some(installed) = window.try_state::<Arc<Installed>>() else {
        return Ok(presentation.snapshot(ready, None));
    };
    let document = match cache::read_document(&cache::path(&installed.home)) {
        Ok(document) => document,
        Err(error) => {
            return Ok(json!({
                "phase":"daemon_failed", "reason":"update_txn_io",
                "operation_error":GuiError::internal(format!("Cannot read the updater journal: {error}"))
            }));
        }
    };
    let saved = document.get("txn").filter(|txn| !txn.is_null());
    let txid = saved.and_then(|txn| txn["id"].as_str());

    let mut versions = None;
    let mut eligibility = None;
    let mut recovery = None;
    let mut latest_offer = None;
    if !presentation.active() {
        let move_pending = document.get("move").is_some_and(|record| !record.is_null());
        if saved.is_none() {
            versions = installed.versions(move_pending).await.ok();
        }
        if let Some(installer) = window.try_state::<InstallCoordinator>() {
            let pending = installer.pending();
            if let Ok(Some(txn)) = pending {
                if let Ok((config, root)) = installed.rollback_scope().await {
                    recovery = installer.recovery(&txn, &config, &root).await.ok();
                }
            } else if pending.is_ok()
                && let UpdateState::Available { target } = &ready
            {
                latest_offer = Some(target.clone());
                let mode = versions.as_ref();
                let version = &target.version;
                let offer = if mode.is_some_and(|v| v["move_mode"].is_string())
                    && window.try_state::<MoveDaemon>().is_some()
                {
                    json!({"version":version,"kind":"move"})
                } else if mode.is_some_and(|v| v["can_setup"] == true) {
                    json!({"version":version,"kind":"setup"})
                } else {
                    match installer.eligibility().await {
                        Ok(()) => json!({"version":version,"kind":"eligible"}),
                        Err(error) => {
                            json!({"version":version,"kind":"refused","message":error.message,"manual_download_url":error.detail["url"]})
                        }
                    }
                };
                eligibility = Some(offer);
            }
        }
    }
    let mut snapshot = presentation.snapshot(ready, txid);
    if !presentation.active() {
        if let Some(versions) = versions {
            snapshot["version_details"] = versions;
        }
        if let Some(offer) = latest_offer {
            snapshot["latest_offer"] = json!(offer);
        }
        if let Some(eligibility) = eligibility {
            snapshot["install_eligibility"] = eligibility;
        }
        if let Some(recovery) = recovery {
            snapshot["recovery"] = recovery;
        }
    }
    Ok(snapshot)
}

#[tauri::command]
pub async fn update_check(window: WebviewWindow) -> Result<UpdateState, GuiError> {
    main_only(window.label())?;
    let view = window.state::<crate::update::state::Presentation>();
    if !view.active()
        && window
            .try_state::<crate::update::install::InstallCoordinator>()
            .is_none_or(|installer| installer.pending().is_ok_and(|txn| txn.is_none()))
    {
        view.clear_completed();
    }
    crate::update::check::manual().await
}

#[tauri::command]
pub async fn update_install(
    window: WebviewWindow,
    target: String,
    schema_acknowledgement: Option<u32>,
    on_progress: Channel<serde_json::Value>,
) -> Result<(), GuiError> {
    use crate::update::install::{InstallCoordinator, InstallOutcome};
    main_only(window.label())?;
    let installer = window
        .try_state::<InstallCoordinator>()
        .ok_or_else(|| unavailable("update_disabled"))?;
    let view = start_presentation(&window)?;
    let result = installer
        .install(&target, schema_acknowledgement, &mut |state| {
            let _ = on_progress.send(view.wire_progress(state));
        })
        .await;
    view.finish(result.as_ref().err().cloned());
    if result.is_err() && installer.pending().is_ok_and(|txn| txn.is_none()) {
        view.clear_completed();
    }
    let outcome = result?;
    if outcome == InstallOutcome::Relaunch {
        let app = window.app_handle().clone();
        window
            .run_on_main_thread(move || app.restart())
            .map_err(|error| GuiError::internal(format!("Cannot relaunch GIT STRATA: {error}")))?;
    }
    Ok(())
}

#[tauri::command]
pub async fn update_cancel(window: WebviewWindow) -> Result<(), GuiError> {
    use crate::update::{install::InstallCoordinator, restart::RestartController};

    main_only(window.label())?;
    if window
        .try_state::<RestartController>()
        .is_some_and(|controller| controller.request_cancel())
    {
        return Ok(());
    }
    window
        .try_state::<InstallCoordinator>()
        .ok_or_else(|| unavailable("update_disabled"))?
        .cancel()
}

#[tauri::command]
pub async fn daemon_restart(
    window: WebviewWindow,
    schema_acknowledgement: Option<u32>,
    on_progress: Channel<serde_json::Value>,
) -> Result<(), GuiError> {
    main_only(window.label())?;
    let _guard = crate::update::lock_lifecycle(&window)?;
    if let Some(installer) = window.try_state::<crate::update::install::InstallCoordinator>() {
        installer.require_idle()?;
    }
    let view = start_presentation(&window)?;
    let result =
        crate::update::restart::command(&window, schema_acknowledgement, on_progress).await;
    view.finish(result.as_ref().err().cloned());
    result
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
    let _guard = crate::update::lock_lifecycle(&window)?;
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
    let _guard = crate::update::lock_lifecycle(&window)?;
    if let Some(installer) = window.try_state::<crate::update::install::InstallCoordinator>() {
        installer.require_idle()?;
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
    let _guard = crate::update::lock_lifecycle(&window)?;
    if let Some(installer) = window.try_state::<crate::update::install::InstallCoordinator>() {
        installer.require_idle()?;
    }
    cli_link::install(&window).await
}

#[tauri::command]
pub async fn update_rollback(
    window: WebviewWindow,
    start_only: Option<bool>,
    on_progress: Channel<serde_json::Value>,
) -> Result<(), GuiError> {
    main_only(window.label())?;
    use crate::update::{Installed, install::InstallCoordinator};
    let installed = window
        .try_state::<std::sync::Arc<Installed>>()
        .ok_or_else(|| unavailable("update_disabled"))?;
    let installer = window.state::<InstallCoordinator>();
    let (config, root) = installed.rollback_scope().await?;
    let view = start_presentation(&window)?;
    if let Ok(doc) =
        crate::update::cache::read_document(&crate::update::cache::path(&installed.home))
        && let Some(id) = doc.get("txn").and_then(|txn| txn["id"].as_str())
    {
        view.restore_id(id);
    }
    let mut progress = |state: UpdateState| {
        let _ = on_progress.send(view.wire_progress(state));
    };
    let result = if start_only == Some(true) {
        installer
            .start_recovery(&config, &root, &mut progress)
            .await
            .map(|()| false)
    } else {
        installer
            .rollback_to_previous(&config, &root, &mut progress)
            .await
    };
    view.finish(result.as_ref().err().cloned());
    if result? {
        let app = window.app_handle().clone();
        window
            .run_on_main_thread(move || app.restart())
            .map_err(|e| GuiError::internal(format!("Cannot relaunch GIT STRATA: {e}")))?;
    }
    Ok(())
}
