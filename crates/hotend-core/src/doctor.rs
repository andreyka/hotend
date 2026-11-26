//! The doctor report: what this machine has, what is missing and how to fix it, as data.
//!
//! [`report`] needs no open core, because its first job is to say why a core cannot open.
//! Hard requirements (`required` checks): a usable sandbox, the Python environment, an
//! engine self-test that really builds a part inside the sandbox, and an agent self-test
//! that runs the real agent process, sandboxed, through a written two-turn conversation
//! with one tool call: no model is asked and nothing is sent anywhere. Slicers and the
//! keychain are reported but optional.
//! No key is read or printed; only "is it set" is asked of the secret stores.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::agent::{AgentCommand, RunEvent, StopReason};
use crate::core::{Core, CoreConfig, SecretBackend};
use crate::egress::ProviderKind;
use crate::engine::EngineStatus;
use crate::gateway::{SliceBackend, SlicerInstallation};
use crate::runtime::PythonRuntime;
use crate::secrets::{FileSecretStore, MemorySecretStore, SecretStore, provider_key_name};
use crate::settings::{ProviderSettings, Settings};

const SELFTEST_MODEL: &str = include_str!("../assets/selftest_model.py");
const SELFTEST_SCRIPT: &str = include_str!("../assets/selftest_agent_script.json");
/// The self-test part is a 10 x 20 x 30 mm block.
const SELFTEST_VOLUME: f64 = 6000.0;
const SELFTEST_TIMEOUT: Duration = Duration::from_secs(240);

/// The outcome of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DoctorStatus {
    Ok,
    Warn,
    Fail,
    /// Not run, because something it needs failed.
    Skipped,
    Info,
}

/// One line of the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorCheck {
    pub id: String,
    pub label: String,
    pub status: DoctorStatus,
    /// A hard requirement: when it fails, Hotend cannot run a model.
    pub required: bool,
    pub detail: String,
    /// What the user can do, when something is wrong.
    pub fix: Option<String>,
}

impl DoctorCheck {
    pub fn new(id: &str, label: &str, status: DoctorStatus, detail: impl Into<String>) -> Self {
        Self {
            id: id.to_owned(),
            label: label.to_owned(),
            status,
            required: false,
            detail: detail.into(),
            fix: None,
        }
    }

    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    pub fn fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
}

/// The whole report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorReport {
    /// No hard requirement is missing.
    pub ok: bool,
    /// Hotend's version.
    pub hotend: String,
    pub debug_build: bool,
    pub checks: Vec<DoctorCheck>,
    /// What the sandbox probe found; `None` when the launcher could not be asked.
    pub sandbox: Option<hotend_sandbox::ProbeReport>,
    /// The slicer programs the given backend knows.
    pub slicers: Vec<SlicerInstallation>,
    /// The providers whose key the store in use reads from the environment, where it wins
    /// over a stored key (the environment and file stores only; never a key).
    #[serde(default)]
    pub keys_from_environment: Vec<ProviderKind>,
}

/// The providers whose key a store of `kind` takes from the environment: with the
/// environment and file stores, those whose usual variable holds a usable key.
pub fn keys_from_environment(kind: crate::secrets::SecretStoreKind) -> Vec<ProviderKind> {
    use crate::secrets::SecretStoreKind;
    if !matches!(kind, SecretStoreKind::Environment | SecretStoreKind::File) {
        return Vec::new();
    }
    ProviderKind::ALL
        .into_iter()
        .filter(|kind| crate::secrets::environment_has(&provider_key_name(*kind)))
        .collect()
}

/// What the report covers.
#[derive(Clone, Default)]
pub struct DoctorOptions {
    /// The slicer backend to ask for installed slicers; without one the slicer rows say so.
    pub slicers: Option<Arc<dyn SliceBackend>>,
    /// Skip the two self-tests (they build a part and run the agent, which takes a while).
    pub skip_self_tests: bool,
}

/// Text from outside this program, safe to show: no control characters, bounded.
fn clean(text: &str) -> String {
    let shown: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    crate::util::truncate(shown.trim(), 2000)
}

/// Runs every check.
pub async fn report(options: DoctorOptions) -> DoctorReport {
    let mut checks: Vec<DoctorCheck> = Vec::new();
    let probe = sandbox(&mut checks).await;
    let sandbox_ok = probe.as_ref().is_some_and(|report| report.usable);
    let runtime = python(&mut checks).await;
    if let (true, Some(runtime)) = (sandbox_ok, &runtime) {
        read_only_folders(&mut checks, runtime).await;
    }
    if options.skip_self_tests {
        for (id, label) in [("engine", "engine self-test"), ("agent", "agent self-test")] {
            checks.push(DoctorCheck::new(
                id,
                label,
                DoctorStatus::Skipped,
                "not asked for",
            ));
        }
    } else {
        engine(&mut checks, sandbox_ok, runtime.clone()).await;
        agent(&mut checks, sandbox_ok, runtime).await;
    }
    let slicers = slicers(&mut checks, options.slicers).await;
    printer(&mut checks);
    secrets(&mut checks).await;
    let ok = !checks
        .iter()
        .any(|check| check.required && check.status == DoctorStatus::Fail);
    DoctorReport {
        ok,
        hotend: env!("CARGO_PKG_VERSION").to_owned(),
        debug_build: cfg!(debug_assertions),
        checks,
        sandbox: probe,
        slicers,
        keys_from_environment: keys_from_environment(SecretBackend::from_env().kind()),
    }
}

// ---- sandbox ---------------------------------------------------------------------------

async fn sandbox(checks: &mut Vec<DoctorCheck>) -> Option<hotend_sandbox::ProbeReport> {
    let probed = tokio::task::spawn_blocking(hotend_sandbox::probe).await;
    let report = match probed {
        Ok(Ok(report)) => report,
        Ok(Err(error)) => {
            checks.push(
                DoctorCheck::new("sandbox", "sandbox", DoctorStatus::Fail, error.to_string())
                    .required()
                    .fix(if cfg!(debug_assertions) {
                        "build the launcher with `cargo build --workspace` so that \
                         `hotend-sandbox` sits next to `hotend` (a debug build also takes \
                         its path from HOTEND_SANDBOX_BIN)"
                    } else {
                        "install `hotend-sandbox` next to `hotend`; a release build looks \
                         for it nowhere else"
                    }),
            );
            return None;
        }
        Err(error) => {
            checks.push(
                DoctorCheck::new("sandbox", "sandbox", DoctorStatus::Fail, error.to_string())
                    .required(),
            );
            return None;
        }
    };
    let mut detail = format!("{} on {}", report.backend, report.platform);
    if let Some(kernel) = &report.kernel {
        detail.push_str(&format!(", kernel {kernel}"));
    }
    match report.landlock_abi {
        Some(abi) => detail.push_str(&format!(", Landlock ABI {abi}")),
        None => detail.push_str(", no Landlock"),
    }
    detail.push_str(if report.seccomp {
        ", seccomp"
    } else {
        ", no seccomp"
    });
    match (&report.namespaces, &report.namespaces_detail) {
        (true, _) => detail.push_str(", namespaces"),
        (false, Some(why)) => detail.push_str(&format!(", no namespaces ({why})")),
        (false, None) => detail.push_str(", no namespaces"),
    }
    if report.root {
        detail.push_str("; running as root");
    }
    if report.usable {
        checks.push(DoctorCheck::new("sandbox", "sandbox", DoctorStatus::Ok, detail).required());
        for (guarantee, mechanisms) in &report.guarantees {
            checks.push(DoctorCheck::new(
                &format!("sandbox.{guarantee}"),
                &format!("  {guarantee}"),
                DoctorStatus::Ok,
                mechanisms.join("; "),
            ));
        }
    } else {
        let reason = report
            .reason
            .clone()
            .unwrap_or_else(|| "the sandbox cannot be applied on this machine".to_owned());
        checks.push(
            DoctorCheck::new("sandbox", "sandbox", DoctorStatus::Fail, detail)
                .required()
                .fix(reason),
        );
    }
    if std::env::var_os(hotend_sandbox::UNSAFE_ENV).is_some_and(|value| value == "1") {
        let (status, detail) = if cfg!(debug_assertions) {
            (
                DoctorStatus::Warn,
                "HOTEND_UNSAFE_NO_SANDBOX=1 is set: this debug build runs AI-written code \
                 WITHOUT a sandbox",
            )
        } else {
            (
                DoctorStatus::Info,
                "HOTEND_UNSAFE_NO_SANDBOX=1 is set and ignored: release builds keep the \
                 sandbox on",
            )
        };
        checks.push(
            DoctorCheck::new("sandbox.override", "  unsafe override", status, detail)
                .fix("unset HOTEND_UNSAFE_NO_SANDBOX"),
        );
    }
    if let Some(path) = std::env::var_os(hotend_sandbox::LAUNCHER_ENV).filter(|v| !v.is_empty()) {
        let path = clean(&path.to_string_lossy());
        let (status, detail) = if cfg!(debug_assertions) {
            (
                DoctorStatus::Warn,
                format!(
                    "HOTEND_SANDBOX_BIN is set: this debug build starts every job through \
                     {path} instead of the launcher next to `hotend`; whatever that program \
                     is decides how AI-written code is confined"
                ),
            )
        } else {
            (
                DoctorStatus::Info,
                "HOTEND_SANDBOX_BIN is set and ignored: release builds only use the \
                 launcher next to `hotend`"
                    .to_owned(),
            )
        };
        checks.push(
            DoctorCheck::new("sandbox.launcher", "  launcher override", status, detail)
                .fix("unset HOTEND_SANDBOX_BIN"),
        );
    }
    Some(report)
}

/// The read-only rule, tried for real: a sandboxed process attempts to create a file in
/// every folder the engine may only read. The folders hold Hotend's own code; a job that
/// could write there would run its own code in every later job.
async fn read_only_folders(checks: &mut Vec<DoctorCheck>, runtime: &PythonRuntime) {
    let label = "  read-only folders";
    let probe = runtime.clone();
    let tried = tokio::task::spawn_blocking(move || try_to_write(&probe)).await;
    let written = match tried {
        Ok(Ok(written)) => written,
        Ok(Err(problem)) => {
            checks.push(
                DoctorCheck::new("sandbox.read_only", label, DoctorStatus::Fail, problem)
                    .required(),
            );
            return;
        }
        Err(error) => {
            checks.push(
                DoctorCheck::new(
                    "sandbox.read_only",
                    label,
                    DoctorStatus::Fail,
                    error.to_string(),
                )
                .required(),
            );
            return;
        }
    };
    let show = |paths: &[&std::path::PathBuf]| {
        paths
            .iter()
            .map(|path| clean(&path.display().to_string()))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let on_9p: Vec<&std::path::PathBuf> = runtime
        .read_only
        .iter()
        .filter(|path| hotend_sandbox::on_9p(path))
        .collect();
    let check = if !written.is_empty() {
        DoctorCheck::new(
            "sandbox.read_only",
            label,
            DoctorStatus::Fail,
            format!(
                "a sandboxed process could write to {}",
                show(&written.iter().collect::<Vec<_>>())
            ),
        )
        .fix(
            "do not run models on this machine as it is; if the folder is on a Windows \
             drive (/mnt/c, ...), move the repository and the Python environment to the \
             Linux filesystem",
        )
    } else if !on_9p.is_empty() {
        DoctorCheck::new(
            "sandbox.read_only",
            label,
            DoctorStatus::Warn,
            format!(
                "a sandboxed process could not write to them, but {} is on a 9p filesystem \
                 (a Windows drive under WSL), where the rule depends on the launcher \
                 keeping the folder open and everything is slow",
                show(&on_9p)
            ),
        )
        .fix(
            "keep the repository and the Python environment on the Linux filesystem (for \
             example under your home folder)",
        )
    } else {
        DoctorCheck::new(
            "sandbox.read_only",
            label,
            DoctorStatus::Ok,
            format!(
                "a sandboxed process could not write to any of the {} folders the engine \
                 may only read",
                runtime.read_only.len()
            ),
        )
    };
    checks.push(check.required());
}

const WRITE_PROBE: &str = "import os, sys\n\
    for folder in sys.argv[1:]:\n    \
        target = os.path.join(folder, '.hotend-doctor-write-probe')\n    \
        try:\n        \
            open(target, 'x').close()\n    \
        except OSError:\n        \
            print('refused')\n    \
        else:\n        \
            print('written')\n";

/// Runs the write probe in the engine's sandbox. Returns the read-only folders it managed
/// to write to (and removes what it wrote).
fn try_to_write(runtime: &PythonRuntime) -> Result<Vec<std::path::PathBuf>, String> {
    let job = tempfile::tempdir().map_err(|error| format!("no temporary folder: {error}"))?;
    let mut policy = hotend_sandbox::Policy::new(hotend_sandbox::Profile::Engine, job.path());
    policy.read_only = runtime.read_only.clone();
    policy.limits.wall_seconds = 60;
    let mut args: Vec<std::ffi::OsString> = vec!["-I".into(), "-c".into(), WRITE_PROBE.into()];
    args.extend(
        runtime
            .read_only
            .iter()
            .map(|path| path.clone().into_os_string()),
    );
    let output = hotend_sandbox::command(&policy, &runtime.python, args)
        .map_err(|error| error.to_string())?
        .output()
        .map_err(|error| format!("the sandbox launcher did not start: {error}"))?;
    let answers: Vec<&str> = std::str::from_utf8(&output.stdout)
        .unwrap_or("")
        .lines()
        .collect();
    if !output.status.success() || answers.len() != runtime.read_only.len() {
        return Err(format!(
            "the write probe did not finish ({}): {}",
            output.status,
            clean(String::from_utf8_lossy(&output.stderr).trim())
        ));
    }
    let mut written = Vec::new();
    for (folder, answer) in runtime.read_only.iter().zip(answers) {
        if answer == "written" {
            let _ = std::fs::remove_file(folder.join(".hotend-doctor-write-probe"));
            written.push(folder.clone());
        }
    }
    Ok(written)
}

// ---- Python and the engine -------------------------------------------------------------

async fn python(checks: &mut Vec<DoctorCheck>) -> Option<PythonRuntime> {
    match PythonRuntime::discover().await {
        Ok(runtime) => {
            checks.push(
                DoctorCheck::new(
                    "python",
                    "python environment",
                    DoctorStatus::Ok,
                    tidy(&runtime.python).display().to_string(),
                )
                .required(),
            );
            Some(runtime)
        }
        Err(error) => {
            checks.push(
                DoctorCheck::new(
                    "python",
                    "python environment",
                    DoctorStatus::Fail,
                    error.to_string(),
                )
                .required()
                .fix(error.fix().unwrap_or(python_fix(cfg!(debug_assertions)))),
            );
            None
        }
    }
}

/// What to do about a Python environment that cannot be used. An installed Hotend brings
/// its own, so the person can only install it again; a build from a checkout is set up
/// with the repository's script.
fn python_fix(debug_build: bool) -> &'static str {
    if debug_build {
        "run `bash scripts/setup-wsl.sh` (or `cd python && uv sync --frozen`); if the \
         environment is somewhere else, set HOTEND_PYTHON to its python and \
         HOTEND_PYTHON_SRC to the repository's python/ folder"
    } else {
        "Hotend's own Python environment is missing or damaged: install Hotend again. \
         (If you built Hotend yourself, set HOTEND_PYTHON to the python of its \
         environment.)"
    }
}

/// The path without `..` detours, keeping the file name as given (the interpreter of a
/// virtual environment is a link that must not be resolved).
fn tidy(path: &std::path::Path) -> std::path::PathBuf {
    match (
        path.parent().and_then(|dir| dir.canonicalize().ok()),
        path.file_name(),
    ) {
        (Some(dir), Some(name)) => dir.join(name),
        _ => path.to_path_buf(),
    }
}

async fn engine(checks: &mut Vec<DoctorCheck>, sandbox_ok: bool, runtime: Option<PythonRuntime>) {
    let label = "engine self-test";
    let (true, Some(runtime)) = (sandbox_ok, runtime) else {
        checks.push(
            DoctorCheck::new(
                "engine",
                label,
                DoctorStatus::Skipped,
                "not run: it needs the sandbox and the Python environment",
            )
            .required(),
        );
        return;
    };
    let started = Instant::now();
    let outcome = tokio::time::timeout(SELFTEST_TIMEOUT, self_test(runtime)).await;
    let seconds = started.elapsed().as_secs_f64();
    let check = match outcome {
        Ok(Ok(summary)) => DoctorCheck::new(
            "engine",
            label,
            DoctorStatus::Ok,
            format!(
                "built a 10 x 20 x 30 mm block inside the sandbox in {seconds:.1} s: {summary}"
            ),
        ),
        Ok(Err(problem)) => DoctorCheck::new("engine", label, DoctorStatus::Fail, problem).fix(
            "run `cd python && uv sync --frozen` to restore the locked environment, then \
             `hotend build python/tests/models/box.py` to see the full error",
        ),
        Err(_) => DoctorCheck::new(
            "engine",
            label,
            DoctorStatus::Fail,
            format!("no result within {} s", SELFTEST_TIMEOUT.as_secs()),
        )
        .fix(
            "the first import of the CAD kernel can be slow on a Windows drive: keep the \
             Python environment on the Linux filesystem (see scripts/setup-wsl.sh) and try \
             again",
        ),
    };
    checks.push(check.required());
}

/// Builds the self-test model through the real path (core, sandbox, engine worker) in a
/// throwaway data directory. Returns a one-line summary, or what went wrong.
async fn self_test(runtime: PythonRuntime) -> Result<String, String> {
    let scratch = tempfile::tempdir().map_err(|error| format!("no temporary folder: {error}"))?;
    let config = CoreConfig {
        data_dir: Some(scratch.path().to_path_buf()),
        secrets: SecretBackend::Custom(Arc::new(MemorySecretStore::default())),
        runtime: Some(runtime),
        ..CoreConfig::default()
    };
    let text = |error: crate::CoreError| clean(&error.to_string());
    let core = Core::open(config).await.map_err(text)?;
    let project = core
        .project_create("doctor self-test")
        .await
        .map_err(text)?;
    let report = core
        .model_build(&project.id, SELFTEST_MODEL, Default::default(), "self-test")
        .await
        .map_err(text)?;
    let result = &report.result;
    if let Some(error) = &result.error {
        return Err(clean(&format!("{}: {}", error.kind, error.message)));
    }
    let volume = result.parts.first().map_or(0.0, |part| part.volume);
    if (volume - SELFTEST_VOLUME).abs() > 1e-3 {
        return Err(format!(
            "the block has a volume of {volume} mm3 instead of {SELFTEST_VOLUME}"
        ));
    }
    let failed: Vec<&str> = result
        .checks
        .iter()
        .filter(|check| check.status == crate::engine::CheckStatus::Fail)
        .map(|check| check.id.as_str())
        .collect();
    if result.status != EngineStatus::Ok || report.version.is_none() {
        return Err(format!(
            "the build ended as {} (failed checks: {})",
            result.status.as_str(),
            failed.join(", ")
        ));
    }
    // The model prints the versions it runs on; they come from inside the sandbox.
    let versions = clean(result.stdout.lines().next().unwrap_or("").trim());
    Ok(format!(
        "volume {volume:.0} mm3, {} checks run, none failed; {versions}",
        result.checks.len()
    ))
}

// ---- the agent -------------------------------------------------------------------------

async fn agent(checks: &mut Vec<DoctorCheck>, sandbox_ok: bool, runtime: Option<PythonRuntime>) {
    let label = "agent self-test";
    let (true, Some(runtime)) = (sandbox_ok, runtime) else {
        checks.push(DoctorCheck::new(
            "agent",
            label,
            DoctorStatus::Skipped,
            "not run: it needs the sandbox and the Python environment",
        ));
        return;
    };
    let started = Instant::now();
    let outcome = tokio::time::timeout(SELFTEST_TIMEOUT, agent_self_test(runtime)).await;
    let seconds = started.elapsed().as_secs_f64();
    let fix = "run `cd python && uv sync --frozen` to restore the locked environment; the \
               agent needs the `hotend_agent` package and its pinned SDKs";
    let check = match outcome {
        Ok(Ok(summary)) => DoctorCheck::new(
            "agent",
            label,
            DoctorStatus::Ok,
            format!(
                "the sandboxed agent process replayed a written conversation in {seconds:.1} s \
                 (no model was asked, nothing was sent): {summary}"
            ),
        ),
        Ok(Err(problem)) => DoctorCheck::new("agent", label, DoctorStatus::Fail, problem).fix(fix),
        Err(_) => DoctorCheck::new(
            "agent",
            label,
            DoctorStatus::Fail,
            format!("no result within {} s", SELFTEST_TIMEOUT.as_secs()),
        )
        .fix(fix),
    };
    checks.push(check.required());
}

/// Runs the real agent child in its sandbox through a written conversation with one tool
/// call, in a throwaway data directory. Returns a one-line summary, or what went wrong.
async fn agent_self_test(runtime: PythonRuntime) -> Result<String, String> {
    let scratch = tempfile::tempdir().map_err(|error| format!("no temporary folder: {error}"))?;
    let script_dir = scratch.path().join("script");
    let script = script_dir.join("selftest.json");
    std::fs::create_dir_all(&script_dir)
        .and_then(|()| std::fs::write(&script, SELFTEST_SCRIPT))
        .map_err(|error| format!("cannot write the self-test script: {error}"))?;
    let mut agent = AgentCommand::python_module(&runtime);
    agent
        .args
        .extend(["--script".to_owned(), script.display().to_string()]);
    agent.read_only.push(script_dir);
    let text = |error: crate::CoreError| clean(&error.to_string());
    let core = Core::open(CoreConfig {
        data_dir: Some(scratch.path().join("data")),
        secrets: SecretBackend::Custom(Arc::new(MemorySecretStore::default())),
        runtime: Some(runtime),
        agent: Some(agent),
        ..CoreConfig::default()
    })
    .await
    .map_err(text)?;
    // The settings must name a provider; `ollama` takes no key, and the scripted agent
    // sends no request to it or to anyone.
    core.settings_set(Settings {
        provider: Some(ProviderSettings {
            kind: ProviderKind::Ollama,
            model: "scripted-self-test".to_owned(),
        }),
        ..Settings::default()
    })
    .await
    .map_err(text)?;
    let project = core
        .project_create("doctor self-test")
        .await
        .map_err(text)?;
    let mut run = core
        .message_send(&project.id, "Which feature cards are there?")
        .await
        .map_err(text)?;
    let (mut tools, mut tools_ok) = (0u32, 0u32);
    loop {
        match run.next().await {
            Some(RunEvent::Agent { event }) => {
                if event.get("type").and_then(serde_json::Value::as_str) == Some("tool_end") {
                    tools += 1;
                    if event.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
                        tools_ok += 1;
                    }
                }
            }
            Some(RunEvent::Finished {
                stop_reason: StopReason::Done,
                ..
            }) => break,
            Some(RunEvent::Finished { stop_reason, error }) => {
                return Err(clean(&format!(
                    "the run stopped early ({stop_reason:?}): {}",
                    error.unwrap_or_default()
                )));
            }
            Some(RunEvent::Failed { code, message }) => {
                return Err(clean(&format!("the run failed ({code}): {message}")));
            }
            None => return Err("the event stream closed before the run ended".to_owned()),
        }
    }
    if tools != 1 || tools_ok != 1 {
        return Err(format!(
            "the agent made {tools} tool call(s), {tools_ok} succeeded; expected one that \
             succeeds"
        ));
    }
    Ok("1 tool call through the gateway, answered by the engine".to_owned())
}

// ---- slicers and printers ----------------------------------------------------------------

async fn slicers(
    checks: &mut Vec<DoctorCheck>,
    backend: Option<Arc<dyn SliceBackend>>,
) -> Vec<SlicerInstallation> {
    let Some(backend) = backend else {
        checks.push(DoctorCheck::new(
            "slicer",
            "slicers",
            DoctorStatus::Info,
            "not looked for: this caller installed no slicer backend",
        ));
        return Vec::new();
    };
    let rows = tokio::task::spawn_blocking(move || backend.installations(&Default::default()))
        .await
        .unwrap_or_default();
    for row in &rows {
        let label = format!("slicer: {}", row.name);
        let id = format!("slicer.{}", row.kind);
        let Some(path) = &row.path else {
            let mut check = DoctorCheck::new(&id, &label, DoctorStatus::Warn, "not found");
            if let Some(fix) = &row.fix {
                check = check.fix(clean(fix));
            }
            checks.push(check);
            continue;
        };
        let version = row.version.as_deref().unwrap_or("unknown version");
        let mut detail = format!(
            "{version} at {} ({}); policy: {}",
            clean(&path.display().to_string()),
            row.found.as_deref().unwrap_or("found"),
            row.verdict
        );
        if let Some(reason) = &row.reason {
            detail.push_str(&format!(" - {}", clean(reason)));
        }
        let status = match (row.stand_in, row.verdict.as_str()) {
            (true, _) => DoctorStatus::Info,
            (false, "allow") => DoctorStatus::Ok,
            _ => DoctorStatus::Warn,
        };
        let mut check = DoctorCheck::new(&id, &label, status, detail);
        if let Some(fix) = &row.fix {
            check = check.fix(clean(fix));
        }
        checks.push(check);
    }
    if !rows.iter().any(|row| row.usable && !row.stand_in) {
        checks.push(DoctorCheck::new(
            "slicer.usable",
            "slicer: usable",
            DoctorStatus::Warn,
            "none: no real slicer can be run on this machine",
        ));
    }
    rows
}

fn printer(checks: &mut Vec<DoctorCheck>) {
    checks.push(DoctorCheck::new(
        "printer",
        "printer",
        DoctorStatus::Info,
        "prints run on the simulated printer only: this build sends no print and no command to \
         a real printer and connects to no printer maker's cloud; a confirmed job runs on a \
         printer simulated inside Hotend, or is exported or handed to Bambu Connect; a real \
         printer's status is read only when you set that up",
    ));
}

// ---- secret stores ---------------------------------------------------------------------

/// Whether a provider's key is set in one secret store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "detail")]
pub enum KeyState {
    Set,
    NotSet,
    /// The provider runs locally and takes no key.
    NotNeeded,
    /// The store could not be asked; the text says why and what to do.
    Unavailable(String),
}

impl KeyState {
    pub fn word(&self) -> &str {
        match self {
            Self::Set => "set",
            Self::NotSet => "not set",
            Self::NotNeeded => "not needed (local)",
            Self::Unavailable(_) => "unavailable",
        }
    }
}

/// Asks one secret store which provider keys are set. Only "is it set" is asked; no key is
/// read into this program. A keychain that does not answer within a few seconds counts as
/// unavailable.
pub async fn key_states(backend: &SecretBackend) -> Vec<(ProviderKind, KeyState)> {
    let store: Arc<dyn SecretStore> = backend.store();
    let mut states = Vec::new();
    let mut unavailable: Option<String> = None;
    for kind in ProviderKind::ALL {
        if kind.rule().env_vars.is_empty() {
            states.push((kind, KeyState::NotNeeded));
            continue;
        }
        // One failure is enough: do not wait for the same broken keychain four times.
        if let Some(reason) = &unavailable {
            states.push((kind, KeyState::Unavailable(reason.clone())));
            continue;
        }
        let asked = Arc::clone(&store);
        let task = tokio::task::spawn_blocking(move || asked.is_set(&provider_key_name(kind)));
        let state = match tokio::time::timeout(Duration::from_secs(5), task).await {
            Ok(Ok(Ok(true))) => KeyState::Set,
            Ok(Ok(Ok(false))) => KeyState::NotSet,
            Ok(Ok(Err(error))) => KeyState::Unavailable(error.to_string()),
            Ok(Err(error)) => KeyState::Unavailable(error.to_string()),
            Err(_) => KeyState::Unavailable(
                "the system keychain did not answer within 5 seconds; use the environment \
                 store (HOTEND_SECRETS=env) and the provider's environment variable"
                    .to_owned(),
            ),
        };
        if let KeyState::Unavailable(reason) = &state {
            unavailable = Some(reason.clone());
        }
        states.push((kind, state));
    }
    states
}

/// Asks one secret store which search service keys are set (Keenable, Tavily), the same
/// way as [`key_states`]. Keenable's key is optional (it has a keyless trial path); a
/// missing one is still reported as not set.
pub async fn search_key_states(
    backend: &SecretBackend,
) -> Vec<(crate::search::SearchService, KeyState)> {
    let store: Arc<dyn SecretStore> = backend.store();
    let mut states = Vec::new();
    let mut unavailable: Option<String> = None;
    for service in crate::search::SearchService::ALL {
        if let Some(reason) = &unavailable {
            states.push((service, KeyState::Unavailable(reason.clone())));
            continue;
        }
        let asked = Arc::clone(&store);
        let task = tokio::task::spawn_blocking(move || {
            asked.is_set(&crate::search::search_key_name(service))
        });
        let state = match tokio::time::timeout(Duration::from_secs(5), task).await {
            Ok(Ok(Ok(true))) => KeyState::Set,
            Ok(Ok(Ok(false))) => KeyState::NotSet,
            Ok(Ok(Err(error))) => KeyState::Unavailable(error.to_string()),
            Ok(Err(error)) => KeyState::Unavailable(error.to_string()),
            Err(_) => KeyState::Unavailable(
                "the system keychain did not answer within 5 seconds; use the environment \
                 store (HOTEND_SECRETS=env) and the service's environment variable"
                    .to_owned(),
            ),
        };
        if let KeyState::Unavailable(reason) = &state {
            unavailable = Some(reason.clone());
        }
        states.push((service, state));
    }
    states
}

fn providers_with(states: &[(ProviderKind, KeyState)], wanted: &KeyState) -> String {
    let names: Vec<&str> = states
        .iter()
        .filter(|(_, state)| state == wanted)
        .map(|(kind, _)| kind.as_str())
        .collect();
    if names.is_empty() {
        "none".to_owned()
    } else {
        names.join(", ")
    }
}

async fn secrets(checks: &mut Vec<DoctorCheck>) {
    let chosen = SecretBackend::from_env();
    let keyring = key_states(&SecretBackend::Keyring).await;
    let unavailable = keyring.iter().find_map(|(_, state)| match state {
        KeyState::Unavailable(reason) => Some(reason.clone()),
        _ => None,
    });
    match unavailable {
        // Not used: the person chose the file store, so a missing keychain is no warning.
        Some(reason) if matches!(chosen, SecretBackend::File) => checks.push(DoctorCheck::new(
            "secrets.keyring",
            "key store: keychain",
            DoctorStatus::Info,
            format!(
                "not available ({}); not used: HOTEND_SECRETS=file",
                first_sentence(&reason)
            ),
        )),
        None => checks.push(DoctorCheck::new(
            "secrets.keyring",
            "key store: keychain",
            DoctorStatus::Ok,
            format!(
                "available; keys stored for: {}",
                providers_with(&keyring, &KeyState::Set)
            ),
        )),
        Some(reason) => checks.push(
            DoctorCheck::new(
                "secrets.keyring",
                "key store: keychain",
                DoctorStatus::Warn,
                format!("not available: {}", first_sentence(&reason)),
            )
            .fix(
                "on a headless machine or in WSL, put the key in the provider's environment \
                 variable (for example ANTHROPIC_API_KEY) and use `--secrets env` or \
                 HOTEND_SECRETS=env",
            ),
        ),
    }
    if matches!(chosen, SecretBackend::File) {
        checks.push(file_store_check(
            &FileSecretStore::from_env(),
            &key_states(&chosen).await,
        ));
    }
    let environment = key_states(&SecretBackend::Environment).await;
    let default = match chosen {
        SecretBackend::Environment => "environment (HOTEND_SECRETS=env)",
        SecretBackend::File => "file (HOTEND_SECRETS=file)",
        _ => "keychain",
    };
    checks.push(DoctorCheck::new(
        "secrets.env",
        "key store: environment",
        DoctorStatus::Info,
        format!(
            "read-only; keys present for: {}. Default store now: {default}",
            providers_with(&environment, &KeyState::Set)
        ),
    ));
}

/// The file store's row: where the file is, whether it can be used, and for which
/// providers a key is there (in the environment or the file). Never a key.
fn file_store_check(store: &FileSecretStore, states: &[(ProviderKind, KeyState)]) -> DoctorCheck {
    let place = store
        .path()
        .map_or_else(|| "no file".to_owned(), |path| path.display().to_string());
    let problem = states.iter().find_map(|(_, state)| match state {
        KeyState::Unavailable(reason) => Some(reason.clone()),
        _ => None,
    });
    match problem {
        None => DoctorCheck::new(
            "secrets.file",
            "key store: file",
            DoctorStatus::Ok,
            format!(
                "{place}; keys there or in the environment for: {}. Weaker than a keychain: \
                 programs running as you can read it",
                providers_with(states, &KeyState::Set)
            ),
        ),
        Some(reason) => DoctorCheck::new(
            "secrets.file",
            "key store: file",
            DoctorStatus::Warn,
            format!("{place} cannot be used: {}", first_sentence(&reason)),
        )
        .fix(reason),
    }
}

/// The text up to the first full stop that ends a sentence.
fn first_sentence(text: &str) -> String {
    match text.find(". ") {
        Some(end) => text[..end].to_owned(),
        None => text.trim_end_matches('.').to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn only_stores_that_read_the_environment_report_keys_from_it() {
        use crate::secrets::SecretStoreKind;
        // The keychain and a supplied store never take a key from the environment, whatever
        // this run's environment holds.
        assert!(keys_from_environment(SecretStoreKind::Keychain).is_empty());
        assert!(keys_from_environment(SecretStoreKind::Memory).is_empty());
        let file = keys_from_environment(SecretStoreKind::File);
        assert_eq!(file, keys_from_environment(SecretStoreKind::Environment));
        for kind in file {
            assert!(crate::secrets::environment_has(&provider_key_name(kind)));
        }
    }

    #[test]
    fn an_installed_hotend_is_not_told_to_run_a_script_of_the_repository() {
        let installed = python_fix(false);
        assert!(installed.contains("install Hotend again"), "{installed}");
        assert!(!installed.contains("setup-wsl") && !installed.contains("uv sync"));
        assert!(python_fix(true).contains("scripts/setup-wsl.sh"));
    }

    #[test]
    fn the_self_test_model_states_the_volume_the_check_expects() {
        assert!(SELFTEST_MODEL.contains("Box(10, 20, 30)"));
        assert_eq!(SELFTEST_VOLUME, 10.0 * 20.0 * 30.0);
    }

    #[test]
    fn the_self_test_script_is_a_two_turn_conversation_with_one_tool_call() {
        let script: serde_json::Value = serde_json::from_str(SELFTEST_SCRIPT).unwrap();
        let turns = script["turns"].as_array().unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0]["calls"][0]["tool"], "list_cards");
        assert!(turns[1].get("calls").is_none());
    }

    #[test]
    fn a_reason_is_shortened_to_its_first_sentence() {
        assert_eq!(first_sentence("No bus. Try again."), "No bus");
        assert_eq!(first_sentence("No bus."), "No bus");
    }

    #[test]
    fn shown_text_has_no_control_characters() {
        assert_eq!(clean("a\x1b[31mb\n"), "a [31mb");
    }
}
