//! A print job never blocks design. While a job of version N is handed over or waiting for
//! its confirmation, the agent builds N+1 through a normal run: the run is not refused as
//! busy, N+1 becomes the current version, and the job, its file and version N stay exactly
//! as they were; a reviewed job of N still confirms afterwards.
//!
//! Through the core's facade with the stand-in slicer, the stand-in link opener and the
//! scripted agent of hotend-core's tests. Nothing here talks to a model, a real printer or
//! the network.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use common::*;
use hotend_core::{
    AgentCommand, ConfirmationAuthority, Core, CoreConfig, CoreEvent, DeliveryChoice,
    MemorySecretStore, PrintEvent, PrintJob, PrinterOptions, ProviderKind, ProviderSettings,
    RunEvent, Secret, SecretBackend, SecretStore, Settings, StopReason,
};
use hotend_slicer::Programs;
use serde_json::json;

const MODEL: &str = include_str!("../../../python/tests/models/cable_box.py");
const PLATE: &str = "textured-pei";
/// A key for the in-memory secret store; the scripted agent never sends a request.
const TEST_KEY: &str = "sk-test-integration-0123456789";

fn authority() -> &'static ConfirmationAuthority {
    static AUTHORITY: OnceLock<ConfirmationAuthority> = OnceLock::new();
    AUTHORITY.get_or_init(|| ConfirmationAuthority::take().expect("taken once"))
}

struct Fixture {
    tmp: tempfile::TempDir,
    core: Core,
    project: String,
}

impl Fixture {
    fn data(&self) -> PathBuf {
        self.tmp.path().join("data")
    }

    fn version_dir(&self, number: u32) -> PathBuf {
        self.data()
            .join("projects")
            .join(&self.project)
            .join("versions")
            .join(number.to_string())
    }
}

/// The scripted agent of hotend-core's integration tests.
fn scripted_agent(runtime: &hotend_core::PythonRuntime) -> AgentCommand {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../hotend-core/tests/fixtures")
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

/// A core with the stand-in slicer, the scripted agent and the cable box built and sliced
/// as version 1. `None` (after printing why) only when skipping is allowed.
async fn sliced(test: &str) -> Option<Fixture> {
    if !sandbox_ready(test) {
        return None;
    }
    let runtime = match hotend_core::PythonRuntime::discover().await {
        Ok(runtime) => runtime,
        Err(error) => {
            let problem = format!("no Python environment ({error})");
            if std::env::var_os("HOTEND_TEST_ALLOW_SKIP").is_some_and(|v| v == "1") {
                println!("SKIPPED {test}: {problem}");
                return None;
            }
            panic!("{test} cannot run: {problem} (set HOTEND_TEST_ALLOW_SKIP=1 to skip instead)");
        }
    };
    let tmp = tempfile::tempdir().unwrap();
    let secrets = Arc::new(MemorySecretStore::default());
    let core = Core::open(CoreConfig {
        data_dir: Some(tmp.path().join("data")),
        secrets: SecretBackend::Custom(secrets.clone()),
        agent: Some(scripted_agent(&runtime)),
        runtime: Some(runtime),
        printer: PrinterOptions {
            time_scale: 20_000.0,
            poll_interval: Duration::from_millis(10),
            ..PrinterOptions::default()
        },
        ..CoreConfig::default()
    })
    .await
    .unwrap();
    hotend_slicer::install(&core, Programs::StandIn).unwrap();
    secrets
        .set("provider-key.anthropic", Secret::new(TEST_KEY))
        .unwrap();
    core.settings_set(Settings {
        provider: Some(ProviderSettings {
            kind: ProviderKind::Anthropic,
            model: "test-model".into(),
        }),
        plate_type: Some(PLATE.to_owned()),
        // The stand-in slicer draws no plate preview; a Bambu Connect hand-off needs one.
        previews: hotend_core::PreviewsMode::HotendMade,
        ..Settings::default()
    })
    .await
    .unwrap();
    let project = core.project_create("Cable box").await.unwrap();
    let built = core
        .model_build(&project.id, MODEL, Default::default(), "")
        .await
        .unwrap();
    assert_eq!(
        built.version.as_ref().map(|v| v.number),
        Some(1),
        "{:?}",
        built.result.error
    );
    core.slice_run(&project.id, "demo-0.20mm", None)
        .await
        .unwrap();
    Some(Fixture {
        tmp,
        core,
        project: project.id,
    })
}

/// Every file and folder under `dir` with the SHA-256 of each file's bytes.
fn tree(dir: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(Result::unwrap)
            .collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let name = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if entry.file_type().unwrap().is_dir() {
                out.insert(format!("{name}/"), String::new());
                walk(root, &path, out);
            } else {
                out.insert(name, sha256(&path));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// The agent builds the next version in an ordinary run (the scripted agent calls
/// `write_model_code`); the run must start (not `Busy`) and finish.
async fn agent_builds_the_next_version(core: &Core, project: &str) {
    let code = format!(
        "{MODEL}\n# The next version: built while a print job of the last one is listed.\n"
    );
    let steps = json!([
        {"tool": "write_model_code", "args": {"code": code, "note": "next version"}},
        {"say": "Built the next version."}
    ]);
    let mut run = core
        .message_send(project, &json!({"steps": steps}).to_string())
        .await
        .expect("a listed print job does not make the project busy");
    let mut events = Vec::new();
    let all = async {
        while let Some(event) = run.next().await {
            events.push(event);
        }
    };
    tokio::time::timeout(Duration::from_secs(300), all)
        .await
        .expect("the run did not end");
    assert_eq!(
        events.last().unwrap(),
        &RunEvent::Finished {
            stop_reason: StopReason::Done,
            error: None
        },
        "{events:?}"
    );
}

#[tokio::test]
async fn a_handed_off_job_never_blocks_design_and_nothing_of_it_changes() {
    let test = "a_handed_off_job_never_blocks_design_and_nothing_of_it_changes";
    let Some(fixture) = sliced(test).await else {
        return;
    };
    let core = &fixture.core;
    let project = fixture.project.as_str();

    // Version 1 is handed over (the stand-in opener opens nothing).
    let job = core.print_propose(project, "hand off v1").await.unwrap();
    let review = core
        .print_review(&job.id, DeliveryChoice::BambuConnect)
        .await
        .unwrap();
    let handed = core
        .print_confirm(authority(), &job.id, &review.review_text)
        .await
        .unwrap();
    assert_eq!(handed.status, "handed_off");
    let file = handed.detail.path.clone().unwrap();
    assert_eq!(sha256(&file), job.slice_sha256);
    let file_bytes = std::fs::read(&file).unwrap();
    let version_1 = tree(&fixture.version_dir(1));
    assert!(
        core.print_jobs(project)
            .await
            .unwrap()
            .iter()
            .any(|listed| listed.id == job.id),
        "the job is listed while the next version is built"
    );

    agent_builds_the_next_version(core, project).await;

    let detail = core.project_get(project).await.unwrap();
    assert_eq!(detail.project.current_version, Some(2));
    assert_eq!(detail.versions.len(), 2);
    let after: PrintJob = core.print_status(&job.id).await.unwrap();
    assert_eq!(
        (
            after.version,
            after.status.as_str(),
            after.slice_sha256.as_str()
        ),
        (1, "handed_off", job.slice_sha256.as_str())
    );
    assert_eq!(after.detail.path.as_ref(), Some(&file));
    assert_eq!(after.detail.link, handed.detail.link);
    assert_eq!(
        std::fs::read(&file).unwrap(),
        file_bytes,
        "the handed-over file changed"
    );
    assert_eq!(
        tree(&fixture.version_dir(1)),
        version_1,
        "version 1's folder changed"
    );
    assert!(core.simulated_printer_wire_log().is_empty());
}

#[tokio::test]
async fn a_reviewed_job_of_the_last_version_still_confirms_after_the_next_is_built() {
    let test = "a_reviewed_job_of_the_last_version_still_confirms_after_the_next_is_built";
    let Some(fixture) = sliced(test).await else {
        return;
    };
    let core = &fixture.core;
    let project = fixture.project.as_str();

    let job = core.print_propose(project, "print v1").await.unwrap();
    let review = core
        .print_review(&job.id, DeliveryChoice::SimulatedPrinter)
        .await
        .unwrap();
    let version_1 = tree(&fixture.version_dir(1));

    agent_builds_the_next_version(core, project).await;
    assert_eq!(
        core.project_get(project)
            .await
            .unwrap()
            .project
            .current_version,
        Some(2)
    );
    assert_eq!(core.print_status(&job.id).await.unwrap().status, "pending");

    // The review shown before v2 was built still confirms the job of v1, on its own file.
    let mut events = core.events();
    let confirmed = core
        .print_confirm(authority(), &job.id, &review.review_text)
        .await
        .expect("building the next version does not invalidate a reviewed job of the last");
    assert_eq!(
        (confirmed.version, confirmed.slice_sha256.as_str()),
        (1, job.slice_sha256.as_str())
    );
    assert!(
        ["starting", "printing", "finished"].contains(&confirmed.status.as_str()),
        "{}",
        confirmed.status
    );
    let finished = tokio::time::timeout(Duration::from_secs(60), async {
        if confirmed.status == "finished" {
            return confirmed.clone();
        }
        loop {
            if let Ok(CoreEvent::Print {
                job_id,
                event: PrintEvent::Status { job },
                ..
            }) = events.recv().await
                && job_id == confirmed.id
                && !["starting", "printing"].contains(&job.status.as_str())
            {
                return *job;
            }
        }
    })
    .await
    .expect("the simulated print ended");
    assert_eq!(
        (finished.version, finished.status.as_str()),
        (1, "finished")
    );
    assert_eq!(
        tree(&fixture.version_dir(1)),
        version_1,
        "version 1's folder changed"
    );
}
