fn main() {
    // The dev-connector capability lives in capabilities-dev/, outside the
    // default ./capabilities/**/* glob, and is only picked up by a debug
    // build with the dev-connector feature (gui.md G4).
    let dev_connector = std::env::var_os("CARGO_FEATURE_DEV_CONNECTOR").is_some()
        && std::env::var("PROFILE").as_deref() == Ok("debug");
    let mut attrs = tauri_build::Attributes::new();
    if dev_connector {
        attrs = attrs.capabilities_path_pattern("./capabilities*/**/*");
    }
    tauri_build::try_build(attrs).expect("tauri-build failed");
}
