/// Every command in lib.rs `generate_handler!`. Declaring them makes the app
/// ACL apply: Tauri then refuses a command no capability grants, and
/// capabilities/default.json grants them to the main window only. S0's
/// `daemon_start` / `legacy_agent` run launchctl, so this is what keeps
/// them off any other window (the command checks the label as well).
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
