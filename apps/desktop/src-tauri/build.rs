/// Every command in lib.rs `generate_handler!`. Declaring them makes the app
/// ACL apply: Tauri then refuses a command no capability grants, and
/// capabilities/default.json grants them to the main window only.
/// Service and update actions also check the window label in Rust.
const APP_COMMANDS: &[&str] = &[
    "daemon_status",
    "daemon_socket",
    "sessions_list",
    "session_status",
    "du",
    "deps_list",
    "deps_approve",
    "legacy_agent",
    "daemon_start",
    "update_state",
    "update_check",
    "update_install",
    "update_cancel",
    "daemon_restart",
    "daemon_move_into_app",
    "daemon_setup",
    "cli_link_install",
    "update_rollback",
];

fn main() {
    // The dev-connector capability lives in capabilities-dev/, outside the
    // default ./capabilities/**/* glob, and is only picked up by a debug
    // build with the dev-connector feature (gui.md G4).
    let dev_connector = std::env::var_os("CARGO_FEATURE_DEV_CONNECTOR").is_some()
        && std::env::var("PROFILE").as_deref() == Ok("debug");
    let mut attrs = tauri_build::Attributes::new()
        .app_manifest(tauri_build::AppManifest::new().commands(APP_COMMANDS));
    if dev_connector {
        attrs = attrs.capabilities_path_pattern("./capabilities*/**/*");
    }
    tauri_build::try_build(attrs).expect("tauri-build failed");
}
