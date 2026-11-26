//! Build script: the Tauri context, the permission that lists the window's commands, and
//! the guard that keeps the development bridge out of release builds.

include!("build/guard.rs");
include!("build/command_names.rs");

fn main() {
    let profile = std::env::var("PROFILE").unwrap_or_default();
    let bridge = std::env::var_os("CARGO_FEATURE_DEV_BRIDGE").is_some();
    if let Err(reason) = bridge_allowed(&profile, bridge) {
        panic!("{reason}");
    }
    // The application's own commands are denied to every window unless a capability grants
    // them: this manifest turns them into the one permission `allow-hotend-commands`.
    let attributes = tauri_build::Attributes::new()
        .app_manifest(tauri_build::AppManifest::new().commands(COMMAND_NAMES));
    tauri_build::try_build(attributes).unwrap_or_else(|error| panic!("tauri-build: {error:#}"));
}
