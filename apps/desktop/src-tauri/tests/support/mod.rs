//! Shared by the integration tests: a real core in a temporary folder (stand-in slicer,
//! simulated printer, no model provider) and the helper programs it starts.

#![allow(
    dead_code,
    reason = "each test binary uses its own part of this module"
)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};

use hotend_core::printing::PrinterOptions;
use hotend_core::{Core, CoreConfig, MemorySecretStore, SecretBackend};

/// The folder cargo puts this build's programs in (`target/<profile>`).
pub fn programs_dir() -> PathBuf {
    let exe = std::env::current_exe().expect("the test has a path");
    let deps = exe.parent().expect("the test is in a folder");
    deps.parent()
        .expect("deps is inside the profile folder")
        .to_path_buf()
}

/// The core starts `hotend-sandbox` and the stand-in slicer as programs. `cargo test
/// --workspace` builds them; a test of this package alone builds them here.
pub fn ensure_tools() {
    static BUILT: OnceLock<()> = OnceLock::new();
    BUILT.get_or_init(|| {
        let dir = programs_dir();
        let present = ["hotend-sandbox", "hotend-fake-slicer"].iter().all(|name| {
            dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
                .is_file()
        });
        if present {
            return;
        }
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../Cargo.toml");
        let mut build = Command::new(cargo);
        build
            .args([
                "build",
                "--locked",
                "-p",
                "hotend-sandbox",
                "-p",
                "hotend-slicer",
                "--bins",
            ])
            .arg("--manifest-path")
            .arg(manifest);
        if !cfg!(debug_assertions) {
            build.arg("--release");
        }
        let status = build.status().expect("cargo runs");
        assert!(status.success(), "the helper programs build");
    });
}

/// The build plate the test cores are set up with, by its id in the settings.
pub const PLATE: &str = "textured-pei";

/// A core in `data_dir` with the stand-in slicer and a fast simulated printer. Keys live
/// in an empty in-memory store.
pub async fn open_core(data_dir: &Path) -> Core {
    ensure_tools();
    let core = Core::open(CoreConfig {
        data_dir: Some(data_dir.to_path_buf()),
        secrets: SecretBackend::Custom(Arc::new(MemorySecretStore::default())),
        printer: PrinterOptions {
            time_scale: 3600.0,
            ..PrinterOptions::default()
        },
        ..CoreConfig::default()
    })
    .await
    .expect("the core opens");
    core.settings_set(hotend_core::Settings {
        // The plate a print is checked for is the one the settings name; none is assumed.
        plate_type: Some(PLATE.to_owned()),
        ..hotend_core::Settings::default()
    })
    .await
    .expect("the settings are stored");
    hotend_slicer::install(&core, hotend_slicer::Programs::StandIn).expect("the stand-in slicer");
    core
}

/// A core like [`open_core`] whose configuration folder is `config_dir` (where the printer
/// layer looks for Bambu Lab's certificate authorities), not the user's.
pub async fn open_core_with_config(data_dir: &Path, config_dir: &Path) -> Core {
    ensure_tools();
    Core::open(CoreConfig {
        data_dir: Some(data_dir.to_path_buf()),
        config_dir: Some(config_dir.to_path_buf()),
        secrets: SecretBackend::Custom(Arc::new(MemorySecretStore::default())),
        ..CoreConfig::default()
    })
    .await
    .expect("the core opens")
}

/// A core like [`open_core_with_config`] whose keys live in `secrets` (a file store in a
/// temporary folder, say).
pub async fn open_core_with_secrets(
    data_dir: &Path,
    config_dir: &Path,
    secrets: Arc<dyn hotend_core::SecretStore>,
) -> Core {
    ensure_tools();
    Core::open(CoreConfig {
        data_dir: Some(data_dir.to_path_buf()),
        config_dir: Some(config_dir.to_path_buf()),
        secrets: SecretBackend::Custom(secrets),
        ..CoreConfig::default()
    })
    .await
    .expect("the core opens")
}

/// The cable box of the engine's own tests.
pub fn cable_box_code() -> String {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../python/tests/models/cable_box.py");
    std::fs::read_to_string(path).expect("the cable box model is in the repository")
}

/// A core like [`open_core`] whose agent replays a written conversation (`builtin:<name>`)
/// instead of asking a model. No key is read and no request leaves the computer.
pub async fn open_scripted_core(data_dir: &Path, script: &str) -> Core {
    use hotend_core::{AgentCommand, ProviderKind, ProviderSettings, PythonRuntime, Settings};

    ensure_tools();
    let runtime = PythonRuntime::discover().await.expect("the Python runtime");
    let mut agent = AgentCommand::python_module(&runtime);
    agent
        .args
        .extend(["--script".to_owned(), script.to_owned()]);
    let core = Core::open(CoreConfig {
        data_dir: Some(data_dir.to_path_buf()),
        secrets: SecretBackend::Custom(Arc::new(MemorySecretStore::default())),
        runtime: Some(runtime),
        agent: Some(agent),
        printer: PrinterOptions {
            time_scale: 3600.0,
            ..PrinterOptions::default()
        },
        ..CoreConfig::default()
    })
    .await
    .expect("the core opens");
    core.settings_set(Settings {
        // The one provider kind that takes no key. Nothing is ever sent to it.
        provider: Some(ProviderSettings {
            kind: ProviderKind::Ollama,
            model: "scripted".to_owned(),
        }),
        plate_type: Some(PLATE.to_owned()),
        ..Settings::default()
    })
    .await
    .expect("the settings are stored");
    hotend_slicer::install(&core, hotend_slicer::Programs::StandIn).expect("the stand-in slicer");
    core
}
