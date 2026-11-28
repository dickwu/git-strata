// Prevents an extra console window on Windows in release; harmless on macOS.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

strata_types::mark_release_binary!();

fn main() {
    strata_desktop_lib::run()
}
