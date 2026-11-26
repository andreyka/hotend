//! Helpers for the tests inside this crate that start sandboxed children.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::agent::AgentCommand;
use crate::runtime::PythonRuntime;

/// Makes sure the `hotend-sandbox` launcher exists where `hotend_sandbox::command` looks
/// for it (next to the test binaries). `cargo test -p hotend-core` does not build another
/// package's binary by itself, so it is built here on first use.
pub(crate) fn ensure_launcher() {
    static BUILT: OnceLock<()> = OnceLock::new();
    BUILT.get_or_init(|| {
        if std::env::var_os(hotend_sandbox::LAUNCHER_ENV).is_some() {
            return;
        }
        let exe = std::env::current_exe().unwrap();
        let profile_dir = exe.parent().and_then(Path::parent).unwrap();
        let name = format!("hotend-sandbox{}", std::env::consts::EXE_SUFFIX);
        if profile_dir.join(&name).is_file() {
            return;
        }
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
        let mut build = std::process::Command::new(cargo);
        build
            .args(["build", "-p", "hotend-sandbox", "--manifest-path"])
            .arg(manifest);
        if !cfg!(debug_assertions) {
            build.arg("--release");
        }
        let status = build
            .status()
            .expect("cannot run cargo to build the sandbox launcher");
        assert!(status.success(), "building the sandbox launcher failed");
    });
}

/// The repository's Python runtime with a usable sandbox.
///
/// Without them these tests prove nothing, so a missing runtime or an unusable sandbox fails
/// the test. Only with `HOTEND_TEST_ALLOW_SKIP=1` it is skipped instead (returns `None`
/// after printing why).
pub(crate) async fn sandboxed_python(test: &str) -> Option<PythonRuntime> {
    let problem = match PythonRuntime::discover().await {
        Ok(runtime) => {
            ensure_launcher();
            match hotend_sandbox::probe() {
                Ok(report) if report.usable => return Some(runtime),
                Ok(report) => format!("the sandbox is not usable here: {:?}", report.reason),
                Err(error) => error.to_string(),
            }
        }
        Err(error) => error.to_string(),
    };
    if std::env::var_os("HOTEND_TEST_ALLOW_SKIP").is_some_and(|v| v == "1") {
        println!("SKIPPED {test}: {problem}");
        return None;
    }
    panic!("{test} cannot run: {problem} (set HOTEND_TEST_ALLOW_SKIP=1 to skip instead)");
}

/// The scripted agent of `tests/fixtures/fake_agent.py`.
pub(crate) fn fake_agent(runtime: &PythonRuntime) -> AgentCommand {
    let fixtures: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .canonicalize()
        .unwrap();
    let mut read_only = runtime.read_only.clone();
    read_only.push(fixtures.clone());
    AgentCommand {
        program: runtime.python.clone(),
        args: vec![
            "-I".into(),
            fixtures
                .join("fake_agent.py")
                .to_string_lossy()
                .into_owned(),
        ],
        read_only,
    }
}
