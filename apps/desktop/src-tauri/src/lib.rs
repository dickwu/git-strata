//! Git Strata desktop (docs/modules/gui.md). P1: the shell, the Daemon
//! screen and the session list / detail over existing read methods, the
//! menu-bar item, single instance.

pub mod daemon;
pub mod error;
pub mod lifecycle;
pub mod socket;
pub mod state;
pub mod tray_model;

mod commands;
mod tray;

use std::time::Duration;

use tauri::{AppHandle, Manager, RunEvent, Runtime, WindowEvent};

use crate::daemon::Daemon;

/// The display name (tray, menus, window title). tauri.conf.json carries the
/// same string as productName and the window title, which names the .app
/// bundle and Info.plist CFBundleName / CFBundleDisplayName; the test
/// `app_name_matches_tauri_conf` keeps them together. The bundle identifier,
/// LaunchAgent label and binary names stay `git-strata`.
pub const APP_NAME: &str = "GIT STRATA";
use crate::state::AppState;

/// `lifecycle::Shell` over the real window and activation policy.
pub(crate) struct TauriShell<R: Runtime>(pub AppHandle<R>);

impl<R: Runtime> lifecycle::Shell for TauriShell<R> {
    fn show_window(&self) {
        if let Some(w) = self.0.get_webview_window("main") {
            let _ = w.show();
        }
    }
    fn unminimize_window(&self) {
        if let Some(w) = self.0.get_webview_window("main") {
            let _ = w.unminimize();
        }
    }
    fn focus_window(&self) {
        if let Some(w) = self.0.get_webview_window("main") {
            let _ = w.set_focus();
        }
    }
    fn hide_window(&self) {
        if let Some(w) = self.0.get_webview_window("main") {
            let _ = w.hide();
        }
    }
    fn set_regular(&self, regular: bool) {
        #[cfg(target_os = "macos")]
        {
            let p = if regular {
                tauri::ActivationPolicy::Regular
            } else {
                tauri::ActivationPolicy::Accessory
            };
            let _ = self.0.set_activation_policy(p);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = regular;
    }
}

pub fn run() {
    let builder = tauri::Builder::default()
        // First, so a second launch exits before it builds anything else.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            lifecycle::bring_to_front(&TauriShell(app.clone()))
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_os::init());
    // UI automation for development only (gui.md G4): needs both the
    // dev-connector feature (`bun run tauri:dev`) and a debug build.
    #[cfg(all(feature = "dev-connector", debug_assertions))]
    let builder = builder.plugin(tauri_plugin_connector::init());
    let app = builder
        .manage(AppState::new(socket::default_socket().map(Daemon::new)))
        .setup(|app| {
            let handle = app.handle().clone();
            tray::install(&handle)?;
            let h = handle.clone();
            app.state::<AppState>().set_on_change(move |snap| {
                let _ = tray::refresh(&h, snap);
            });
            // "Updated N min ago" ages without new reads.
            let h = handle.clone();
            tauri::async_runtime::spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_secs(30));
                loop {
                    tick.tick().await;
                    let snap = h.state::<AppState>().snapshot();
                    let _ = tray::refresh(&h, &snap);
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                lifecycle::hide_to_tray(&TauriShell(window.app_handle().clone()));
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::health::daemon_status,
            commands::health::daemon_socket,
            commands::sessions::sessions_list,
            commands::sessions::session_status,
            commands::storage::du,
            commands::deps::deps_list,
        ])
        .build(tauri::generate_context!())
        .unwrap_or_else(|e| panic!("error while building {APP_NAME}: {e}"));
    app.run(|app, event| {
        #[cfg(target_os = "macos")]
        if let RunEvent::Reopen { .. } = event {
            lifecycle::bring_to_front(&TauriShell(app.clone()));
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (app, event);
    });
}

#[cfg(test)]
mod tests {
    use super::APP_NAME;

    #[test]
    fn app_name_matches_tauri_conf() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(APP_NAME, "GIT STRATA");
        assert_eq!(conf["productName"], APP_NAME);
        assert_eq!(conf["app"]["windows"][0]["title"], APP_NAME);
        // The rename is display-only.
        assert_eq!(conf["identifier"], "io.github.dickwu.git-strata");
    }
}
