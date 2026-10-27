//! Dev-only: bring the main window to the front at startup so the
//! tauri-connector bridge answers. A background WKWebView is throttled and
//! the connector's IPC goes through the webview, so with the window behind
//! another app every eval / wait / screenshot times out (0.17.3 has no way
//! to focus a background window). Compiled only with the dev-connector
//! feature in a debug build (see the `mod dev_focus` gate in lib.rs and the
//! `dev_focus_is_gated` test); release builds never contain it.

use tauri::{App, Manager};

pub(crate) fn bring_main_to_front(app: &App) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
    activate_app();
}

/// `set_focus` alone does not activate a background app on recent macOS;
/// ask AppKit directly. `setup` runs on the main thread.
#[cfg(target_os = "macos")]
fn activate_app() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    if let Some(mtm) = MainThreadMarker::new() {
        let ns = NSApplication::sharedApplication(mtm);
        #[allow(deprecated)]
        ns.activateIgnoringOtherApps(true);
    }
}

#[cfg(not(target_os = "macos"))]
fn activate_app() {}
