//! Helpers shared by the integration tests: the real Python runtime, the sandbox launcher
//! and the scripted agent.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use hotend_core::{
    AgentCommand, Core, CoreConfig, MemorySecretStore, MessageRun, ProviderKind, ProviderSettings,
    PythonRuntime, RunEvent, Secret, SecretBackend, SecretStore, Settings,
};

/// A key for the in-memory secret store. It is never sent anywhere: no integration test
/// makes a model request.
pub const TEST_KEY: &str = "sk-test-integration-0123456789";

/// Builds the `hotend-sandbox` launcher when it is not yet next to the test binaries:
/// `cargo test -p hotend-core` does not build another package's binary by itself.
pub fn ensure_launcher() {
    static BUILT: OnceLock<()> = OnceLock::new();
    BUILT.get_or_init(|| {
        if std::env::var_os("HOTEND_SANDBOX_BIN").is_some() {
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
        assert!(build.status().expect("cannot run cargo").success());
    });
}

/// The repository's Python runtime with a usable sandbox.
///
/// Without them these tests prove nothing, so a missing runtime or an unusable sandbox fails
/// the test. Only with `HOTEND_TEST_ALLOW_SKIP=1` it is skipped instead (returns `None`
/// after printing why).
pub async fn sandboxed_python(test: &str) -> Option<PythonRuntime> {
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

pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .canonicalize()
        .unwrap()
}

/// A model script from the engine's own test models.
pub fn model(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../python/tests/models")
        .join(name);
    std::fs::read_to_string(path).unwrap()
}

pub fn fake_agent(runtime: &PythonRuntime) -> AgentCommand {
    let mut read_only = runtime.read_only.clone();
    read_only.push(fixtures());
    AgentCommand {
        program: runtime.python.clone(),
        args: vec![
            "-I".into(),
            fixtures()
                .join("fake_agent.py")
                .to_string_lossy()
                .into_owned(),
        ],
        read_only,
    }
}

pub struct TestCore {
    pub core: Core,
    pub secrets: Arc<MemorySecretStore>,
    pub tmp: tempfile::TempDir,
}

/// A core on a temporary data directory, with the scripted agent, an in-memory secret store
/// and Anthropic selected as provider.
pub async fn open_core(test: &str, mut config: CoreConfig) -> Option<TestCore> {
    let runtime = sandboxed_python(test).await?;
    let tmp = tempfile::tempdir().unwrap();
    let secrets = Arc::new(MemorySecretStore::default());
    config.data_dir = Some(tmp.path().to_path_buf());
    config.agent = Some(fake_agent(&runtime));
    config.runtime = Some(runtime);
    config.secrets = SecretBackend::Custom(secrets.clone());
    let core = Core::open(config).await.unwrap();
    secrets
        .set("provider-key.anthropic", Secret::new(TEST_KEY))
        .unwrap();
    core.settings_set(Settings {
        provider: Some(ProviderSettings {
            kind: ProviderKind::Anthropic,
            model: "test-model".into(),
        }),
        ..Settings::default()
    })
    .await
    .unwrap();
    Some(TestCore { core, secrets, tmp })
}

/// Every event of a run, ending with the final one.
pub async fn collect(mut run: MessageRun) -> Vec<RunEvent> {
    let mut events = Vec::new();
    let all = async {
        while let Some(event) = run.next().await {
            events.push(event);
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(300), all)
        .await
        .expect("the run did not end");
    events
}
