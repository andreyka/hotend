//! The engine client against the real worker, inside the real sandbox: the Rust request and
//! result types match what `hotend_engine.worker` reads and writes, failures are typed, and
//! a hostile result never becomes a version.
//!
//! These tests need `python/.venv`; without it each prints why it is skipped.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use hotend_core::engine::{
    BuildRequest, CheckOptions, CheckStatus, DocsRequest, EngineContext, EngineLimits,
    EngineRequest, EngineStatus, ExportFormat, ExportRequest, MaterialContext, MeasureRequest,
    Outputs, Section, SectionPlane, ViewsRequest,
};
use hotend_core::jobs::{FinishedJob, JobConfig, JobError, JobFile, JobRunner, JobScope};
// Only the Unix-only stand-in for a taken-over engine drives the modeler here.
#[cfg(unix)]
use hotend_core::modeling::{Modeler, ModelingError};
use hotend_core::paths::DataDir;
use hotend_core::profiles::printer_profile;
use hotend_core::store::Store;
use serde_json::{Value, json};

struct Bench {
    data: DataDir,
    store: Arc<Store>,
    runner: JobRunner,
    _tmp: tempfile::TempDir,
}

async fn bench(test: &str) -> Option<Bench> {
    let runtime = common::sandboxed_python(test).await?;
    let tmp = tempfile::tempdir().unwrap();
    let data = DataDir::resolve(Some(tmp.path())).unwrap();
    let store = Arc::new(Store::open(&data.db_path()).unwrap());
    let runner = JobRunner::new(
        data.clone(),
        runtime,
        Arc::clone(&store),
        JobConfig::default(),
    );
    Some(Bench {
        data,
        store,
        runner,
        _tmp: tmp,
    })
}

impl Bench {
    fn jobs_left(&self) -> usize {
        std::fs::read_dir(self.data.jobs_dir()).unwrap().count()
    }
}

fn context() -> EngineContext {
    EngineContext {
        printer: Some(printer_profile("bambu-x2d").unwrap().engine_context()),
        material: Some(MaterialContext {
            id: "pla-basic".into(),
            family: "PLA".into(),
            density_g_cm3: Some(1.24),
            max_overhang_deg: None,
        }),
        calibration_path: None,
    }
}

fn build_request(params: &[(&str, f64)]) -> BuildRequest {
    BuildRequest {
        model_path: "model.py".into(),
        params: params.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect(),
        context: context(),
        ..BuildRequest::default()
    }
}

fn code(text: &str) -> Vec<(&'static str, JobFile)> {
    vec![("model.py", JobFile::Bytes(text.as_bytes().to_vec()))]
}

/// Asserts that the typed result holds everything the worker wrote: every key of the raw
/// `result.json` is either equal in the typed value or an empty default the types omit.
fn assert_nothing_lost(job: &FinishedJob) {
    let raw: Value =
        serde_json::from_slice(&job.read_artifact("result.json", 8 << 20).unwrap()).unwrap();
    let typed = serde_json::to_value(&job.result).unwrap();
    fn compare(path: &str, raw: &Value, typed: Option<&Value>) {
        let empty = |v: &Value| match v {
            Value::Null | Value::Bool(false) => true,
            Value::String(s) => s.is_empty(),
            Value::Array(a) => a.is_empty(),
            Value::Object(o) => o.is_empty(),
            Value::Number(_) | Value::Bool(true) => false,
        };
        match (raw, typed) {
            (raw, None) => assert!(empty(raw), "{path} was dropped by the Rust types: {raw}"),
            (Value::Object(raw), Some(Value::Object(typed))) => {
                for (key, value) in raw {
                    compare(&format!("{path}.{key}"), value, typed.get(key));
                }
            }
            (Value::Array(raw), Some(Value::Array(typed))) => {
                assert_eq!(raw.len(), typed.len(), "{path} has a different length");
                for (index, (a, b)) in raw.iter().zip(typed).enumerate() {
                    compare(&format!("{path}[{index}]"), a, Some(b));
                }
            }
            (Value::Number(a), Some(Value::Number(b))) => {
                assert_eq!(a.as_f64(), b.as_f64(), "{path} differs");
            }
            (raw, Some(typed)) => assert_eq!(raw, typed, "{path} differs"),
        }
    }
    compare("result", &raw, Some(&typed));
}

#[tokio::test]
async fn every_request_type_round_trips_through_the_real_worker() {
    let Some(bench) = bench("every_request_type_round_trips_through_the_real_worker").await else {
        return;
    };
    let scope = JobScope::default();
    let out = tempfile::tempdir().unwrap();

    // build, with every output switched on
    let mut request = build_request(&[("width", 100.0)]);
    request.outputs = Outputs {
        views: vec!["iso".into(), "top".into()],
        stl: true,
        threemf: true,
        view_size: [256, 256],
        ..Outputs::default()
    };
    request.checks.verify_determinism = true;
    let job = bench
        .runner
        .run(
            &scope,
            &EngineRequest::Build(request),
            code(&common::model("lidded_box.py")),
        )
        .await
        .unwrap();
    assert_nothing_lost(&job);
    let result = &job.result;
    assert_eq!(result.status, EngineStatus::Ok);
    assert_eq!(result.params["width"], 100.0);
    assert_eq!(result.param_specs["wall"].provenance, "guessed");
    // verify_determinism builds the model twice, so its print appears twice.
    assert_eq!(result.stdout, "lid lip 95.4 x 55.4\nlid lip 95.4 x 55.4\n");
    let names: Vec<&str> = result.parts.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["box", "lid"]);
    assert!(result.parts[0].is_valid && result.parts[0].is_manifold);
    assert!(result.parts[0].mass_estimate_g.unwrap() > 10.0);
    assert_eq!(result.parts[0].bbox.max, [50.0, 30.0, 35.0]);
    assert!(
        result
            .checks
            .iter()
            .any(|c| c.id == "fits_build_volume" && c.status == CheckStatus::Pass)
    );
    assert!(
        result
            .checks
            .iter()
            .any(|c| c.id == "fit_pairs" && c.status == CheckStatus::Skipped)
    );
    let artifacts = &result.artifacts;
    assert_eq!(artifacts.step.as_deref(), Some("model.step"));
    assert_eq!(artifacts.brep.as_deref(), Some("model.brep"));
    assert_eq!(artifacts.parts_index.as_deref(), Some("model.parts.json"));
    assert_eq!(artifacts.mesh.as_deref(), Some("mesh.json"));
    assert_eq!(artifacts.threemf.as_deref(), Some("export/model.3mf"));
    assert_eq!(artifacts.stl["lid"], "export/lid.stl");
    assert_eq!(artifacts.views["top"], "views/top.png");
    assert!(result.timings["total_s"] > 0.0);
    let step = job.read_artifact("model.step", 64 << 20).unwrap();
    assert!(step.starts_with(b"ISO-10303-21;"));
    let png = job.read_artifact("views/iso.png", 16 << 20).unwrap();
    assert!(png.starts_with(b"\x89PNG"));
    for name in ["model.brep", "model.parts.json"] {
        job.copy_artifact(name, &out.path().join(name), 64 << 20)
            .unwrap();
    }
    drop(job);
    assert_eq!(
        bench.jobs_left(),
        0,
        "a finished job's folder is deleted when it is dropped"
    );
    let geometry = || -> Vec<(&'static str, JobFile)> {
        vec![
            ("model.brep", JobFile::Copy(out.path().join("model.brep"))),
            (
                "model.parts.json",
                JobFile::Copy(out.path().join("model.parts.json")),
            ),
        ]
    };

    // measure
    let measure = EngineRequest::Measure(MeasureRequest {
        queries: vec![
            json!({"kind": "bbox", "part": "lid"}),
            json!({"kind": "volume"}),
            json!({"kind": "distance", "a": {"part": "box"}, "b": {"point": [0, 0, 100]}}),
            json!({"kind": "section", "plane": "XZ", "offset": 0}),
            json!({"kind": "bbox", "part": "nope"}),
        ],
        limits: EngineLimits::default(),
    });
    let job = bench
        .runner
        .run(&scope, &measure, geometry())
        .await
        .unwrap();
    assert_nothing_lost(&job);
    let results = &job.result.results;
    assert_eq!(results.len(), 5);
    assert_eq!(results[0]["size"], json!([100.0, 60.0, 4.0]));
    assert!(results[1]["volume"].as_f64().unwrap() > 40_000.0);
    assert_eq!(results[2]["ok"], true);
    assert_eq!(results[3]["parts"].as_array().unwrap().len(), 2);
    assert_eq!(results[4]["ok"], false);

    // export
    let export = EngineRequest::Export(ExportRequest {
        formats: vec![ExportFormat::ThreeMf, ExportFormat::Stl, ExportFormat::Step],
        parts: Some(vec!["lid".into()]),
        limits: EngineLimits::default(),
    });
    let job = bench.runner.run(&scope, &export, geometry()).await.unwrap();
    assert_nothing_lost(&job);
    assert_eq!(
        job.result.artifacts.step.as_deref(),
        Some("export/model.step")
    );
    assert_eq!(
        job.result.artifacts.threemf.as_deref(),
        Some("export/model.3mf")
    );
    assert_eq!(
        job.result.artifacts.stl,
        BTreeMap::from([("lid".to_owned(), "export/lid.stl".to_owned())])
    );
    assert!(job.read_artifact("export/lid.stl", 64 << 20).unwrap().len() > 100);

    // views, cut open
    let views = EngineRequest::Views(ViewsRequest {
        views: vec!["front".into()],
        size: [128, 128],
        section: Some(Section {
            plane: SectionPlane::XZ,
            offset: 0.0,
        }),
        limits: EngineLimits::default(),
    });
    let job = bench.runner.run(&scope, &views, geometry()).await.unwrap();
    assert_nothing_lost(&job);
    assert_eq!(job.result.artifacts.views["front"], "views/front.png");

    // docs
    let docs = EngineRequest::Docs(DocsRequest {
        query: "fillet edges".into(),
        limit: 3,
        limits: EngineLimits::default(),
    });
    let job = bench.runner.run(&scope, &docs, Vec::new()).await.unwrap();
    assert_nothing_lost(&job);
    assert_eq!(job.result.docs.len(), 3);
    assert_eq!(job.result.docs[0].name, "build123d.fillet");
    assert_eq!(job.result.versions["build123d"].as_deref(), Some("0.13.0"));
    drop(job);

    // Every job left one audit entry with its op and outcome.
    let audit = bench.store.list_audit(None, 20).unwrap();
    let ops: Vec<&str> = audit.iter().rev().map(|a| a.name.as_str()).collect();
    assert_eq!(ops, ["build", "measure", "export", "views", "docs"]);
    assert!(
        audit
            .iter()
            .all(|a| a.kind == "job" && a.outcome == "ok" && a.args_sha256.is_some())
    );
}

#[tokio::test]
async fn model_failures_are_results_with_line_numbers() {
    let Some(bench) = bench("model_failures_are_results_with_line_numbers").await else {
        return;
    };
    let scope = JobScope::default();
    let run = async |name: &str, params: &[(&str, f64)]| {
        let request = EngineRequest::Build(build_request(params));
        let job = bench
            .runner
            .run(&scope, &request, code(&common::model(name)))
            .await
            .unwrap();
        assert_nothing_lost(&job);
        job.result.clone()
    };

    let syntax = run("broken_syntax.py", &[]).await;
    assert_eq!(syntax.status, EngineStatus::ModelError);
    let error = syntax.error.unwrap();
    assert_eq!(error.kind, "SyntaxError");
    assert!(error.line.is_some());

    let raised = run("broken_exception.py", &[]).await;
    let error = raised.error.unwrap();
    assert_eq!((error.kind.as_str(), error.line), ("ValueError", Some(9)));
    assert!(error.traceback.contains("line 9, in rounded"));
    assert_eq!(raised.stdout, "about to fillet\n");
    assert!(raised.parts.is_empty() && raised.artifacts.step.is_none());

    let thin = run("broken_thin_wall.py", &[]).await;
    assert_eq!(thin.status, EngineStatus::CheckError);
    assert!(thin.error.is_none());
    assert!(
        thin.checks
            .iter()
            .any(|c| c.id == "min_wall" && c.status == CheckStatus::Fail)
    );
    assert_eq!(thin.artifacts.step.as_deref(), Some("model.step"));

    let out_of_range = run("lidded_box.py", &[("width", 9999.0)]).await;
    assert_eq!(out_of_range.error.unwrap().kind, "ParameterError");
    let unknown = run("lidded_box.py", &[("nonsense", 1.0)]).await;
    assert!(unknown.error.unwrap().message.contains("nonsense"));
}

const RUNAWAY: &str =
    "from build123d import *\nPARAMS = {}\ndef build(p, ctx):\n    while True:\n        pass\n";
const SELF_KILL: &str =
    "import os\nfrom build123d import *\nPARAMS = {}\ndef build(p, ctx):\n    os._exit(7)\n";

#[tokio::test]
async fn timeouts_crashes_and_refused_requests_are_typed_errors() {
    let Some(bench) = bench("timeouts_crashes_and_refused_requests_are_typed_errors").await else {
        return;
    };
    let scope = JobScope {
        run_id: Some("run-1".into()),
        ..JobScope::default()
    };

    let mut request = build_request(&[]);
    request.limits.wall_seconds = 4;
    let started = std::time::Instant::now();
    let error = bench
        .runner
        .run(&scope, &EngineRequest::Build(request), code(RUNAWAY))
        .await
        .unwrap_err();
    assert!(
        matches!(error, JobError::Timeout { seconds: 4 }),
        "{error:?}"
    );
    assert!(started.elapsed().as_secs() < 20);

    let error = bench
        .runner
        .run(
            &scope,
            &EngineRequest::Build(build_request(&[])),
            code(SELF_KILL),
        )
        .await
        .unwrap_err();
    // A crash is tried once more, and crashes again (test below: the restart in detail).
    assert!(matches!(error, JobError::Crashed { .. }), "{error:?}");
    assert!(error.to_string().contains('7'), "{error}");

    // A request the worker refuses: measuring without a model in the job folder.
    let measure = EngineRequest::Measure(MeasureRequest {
        queries: vec![json!({"kind": "volume"})],
        limits: EngineLimits::default(),
    });
    let error = bench
        .runner
        .run(&scope, &measure, Vec::new())
        .await
        .unwrap_err();
    let JobError::BadRequest { message } = &error else {
        panic!("expected a refused request, got {error:?}");
    };
    assert!(message.contains("model.brep"), "{message}");

    // A check id the engine does not know is refused the same way.
    let mut request = build_request(&[]);
    request.checks = CheckOptions {
        only: Some(vec!["not_a_check".into()]),
        ..CheckOptions::default()
    };
    let error = bench
        .runner
        .run(
            &scope,
            &EngineRequest::Build(request),
            code(&common::model("box.py")),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), "bad_request");

    assert_eq!(bench.jobs_left(), 0, "failed jobs leave no folder behind");
    let audit = bench.store.list_audit(None, 10).unwrap();
    let outcomes: Vec<&str> = audit.iter().rev().map(|a| a.outcome.as_str()).collect();
    assert_eq!(
        outcomes,
        [
            "timeout",
            "crashed",
            "crashed",
            "bad_request",
            "bad_request"
        ]
    );
    assert!(audit.iter().all(|a| a.run_id.as_deref() == Some("run-1")));
}

/// A model whose worker dies with SIGABRT (as in the one unexplained crash of `hotend demo`:
/// exit 134, "free(): invalid size") unless its job folder says it is the restart.
const ABORT_ONCE: &str = "import os\nfrom build123d import *\nPARAMS = {}\n\
    def build(p, ctx):\n    if not os.path.exists('restart.json'):\n        os.abort()\n\
    \x20   return Box(10, 10, 10)\n";
const ABORT_ALWAYS: &str =
    "import os\nfrom build123d import *\nPARAMS = {}\ndef build(p, ctx):\n    os.abort()\n";

#[tokio::test]
async fn a_crashed_worker_is_restarted_once_and_each_crash_is_kept_for_study() {
    let Some(bench) = bench("a_crashed_worker_is_restarted_once").await else {
        return;
    };
    let scope = JobScope {
        project_id: Some("project-1".into()),
        run_id: Some("run-1".into()),
        ..JobScope::default()
    };
    let mut notices = bench.runner.notices().subscribe();
    let crashes = bench.data.root().join("crashes");
    let kept = || -> Vec<std::path::PathBuf> {
        let mut found: Vec<_> = std::fs::read_dir(&crashes)
            .map(|entries| entries.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        found.sort();
        found
    };

    // The first run crashes, the restart builds: the call succeeds and the person is told.
    let job = bench
        .runner
        .run(
            &scope,
            &EngineRequest::Build(build_request(&[])),
            code(ABORT_ONCE),
        )
        .await
        .expect("the restart builds");
    assert_eq!(job.result.status, EngineStatus::Ok);
    let notice = notices.try_recv().expect("the restart is told");
    assert_eq!(notice.run_id.as_deref(), Some("run-1"));
    assert!(
        notice.text.contains("crashed while running `build`")
            && notice.text.contains("restarted once")
            && notice.text.contains("SIGABRT"),
        "{}",
        notice.text
    );
    assert!(notices.try_recv().is_err(), "one notice for one restart");

    // The crash is kept with what is needed to study it.
    let folders = kept();
    assert_eq!(folders.len(), 1);
    let crash: Value =
        serde_json::from_slice(&std::fs::read(folders[0].join("crash.json")).unwrap()).unwrap();
    assert_eq!(crash["op"], "build");
    assert_eq!(crash["attempt"], 1);
    assert!(
        crash["worker_signal"] == 6 || crash["signal"] == 6,
        "{crash:#}"
    );
    assert!(crash["exit"].as_str().unwrap().contains("134") || crash["signal"] == 6);
    assert!(
        !crash["stderr_tail"].as_str().unwrap().is_empty(),
        "{crash:#}"
    );
    assert!(crash["elapsed_ms"].as_u64().unwrap() > 0);
    assert_eq!(crash["memory_limit_mb"], 4096);
    #[cfg(target_os = "linux")]
    assert!(
        crash["peak_process_rss_mb"].as_f64().unwrap() > 10.0,
        "the worker's memory was sampled: {crash:#}"
    );
    assert!(folders[0].join("model.py").is_file() && folders[0].join("request.json").is_file());
    assert!(
        notice
            .text
            .contains(&*folders[0].file_name().unwrap().to_string_lossy())
    );

    // The audit log has both runs: the crash with its facts, the restart pointing at it.
    let audit = bench.store.list_audit(None, 10).unwrap();
    let (restart, first) = (&audit[0], &audit[1]);
    assert_eq!(
        (first.outcome.as_str(), restart.outcome.as_str()),
        ("crashed", "ok")
    );
    assert_eq!(first.detail["crash"]["attempt"], 1);
    assert_eq!(restart.detail["restart_of"], first.detail["job_id"]);
    drop(job);

    // Crashing again on the restart fails the call, and both crashes are kept.
    let error = bench
        .runner
        .run(
            &scope,
            &EngineRequest::Build(build_request(&[])),
            code(ABORT_ALWAYS),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), "crashed");
    let told: Vec<String> = std::iter::from_fn(|| notices.try_recv().ok())
        .map(|n| n.text)
        .collect();
    assert_eq!(told.len(), 2, "{told:?}");
    assert!(told[0].contains("restarted once") && told[1].contains("crashed again"));
    let folders = kept();
    assert_eq!(folders.len(), 3);
    let second: Value =
        serde_json::from_slice(&std::fs::read(folders[2].join("crash.json")).unwrap()).unwrap();
    assert_eq!(second["attempt"], 2);
    assert!(folders[2].join("restart.json").is_file());
    assert_eq!(bench.jobs_left(), 0, "nothing is left in the jobs folder");
}

/// A crashing model that tries to plant a symbolic link named `crash.json` (pointing at a
/// file outside its job folder) and FIFOs, then aborts. The sandbox refuses both on Linux;
/// keeping the crash must be safe either way (the unit test in `jobs` plants them itself).
#[cfg(unix)]
fn plant_and_abort(outside: &std::path::Path) -> String {
    format!(
        "import os\nfrom build123d import *\nPARAMS = {{}}\ndef build(p, ctx):\n\
         \x20   for name in ('crash.json', 'model.py.lnk'):\n\
         \x20       try:\n\
         \x20           os.symlink({outside:?}, name)\n\
         \x20       except OSError as e:\n\
         \x20           print('symlink', e)\n\
         \x20   for name in ('fifo', 'restart.fifo'):\n\
         \x20       try:\n\
         \x20           os.mkfifo(name)\n\
         \x20       except OSError as e:\n\
         \x20           print('fifo', e)\n\
         \x20   os.abort()\n"
    )
}

#[cfg(unix)]
#[tokio::test]
async fn a_crash_never_writes_through_links_or_opens_fifos_the_script_planted() {
    let Some(bench) = bench("a_crash_never_writes_through_links_or_opens_fifos").await else {
        return;
    };
    let outside = bench.data.root().join("outside.txt");
    std::fs::write(&outside, "the person's file").unwrap();
    let scope = JobScope::default();
    let request = EngineRequest::Build(build_request(&[]));
    let script = plant_and_abort(&outside);
    let run = bench.runner.run(&scope, &request, code(&script));
    let error = tokio::time::timeout(std::time::Duration::from_secs(120), run)
        .await
        .expect("a planted FIFO must not make keeping the crash hang")
        .unwrap_err();
    assert_eq!(error.code(), "crashed");
    assert_eq!(
        std::fs::read_to_string(&outside).unwrap(),
        "the person's file",
        "nothing was written through the planted link"
    );
    let crashes = bench.data.root().join("crashes");
    let kept: Vec<_> = std::fs::read_dir(&crashes)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(kept.len(), 2, "both crashes are kept");
    for folder in kept {
        let crash_json = std::fs::symlink_metadata(folder.join("crash.json")).unwrap();
        assert!(
            crash_json.is_file(),
            "crash.json is a plain file the core made"
        );
        let crash: Value =
            serde_json::from_slice(&std::fs::read(folder.join("crash.json")).unwrap()).unwrap();
        assert_eq!(crash["op"], "build");
        for entry in std::fs::read_dir(&folder).unwrap() {
            let entry = entry.unwrap();
            let meta = std::fs::symlink_metadata(entry.path()).unwrap();
            assert!(
                meta.is_file(),
                "only plain files the core wrote are kept: {:?}",
                entry.file_name()
            );
        }
        assert!(!folder.join("fifo").exists() && !folder.join("model.py.lnk").exists());
    }
    assert_eq!(bench.jobs_left(), 0);
}

/// A model that starts processes until a sandbox limit stops it: the process limit, or the
/// tree's resident memory (each copy of the worker counts in full), whichever comes first.
const FORK_UNTIL_THE_LIMIT: &str = "import os, time\nfrom build123d import *\nPARAMS = {}\n\
    def build(p, ctx):\n\
    \x20   for _ in range(2000):\n\
    \x20       try:\n\
    \x20           if os.fork() == 0:\n\
    \x20               time.sleep(60)\n\
    \x20               os._exit(0)\n\
    \x20       except OSError:\n\
    \x20           time.sleep(0.01)\n\
    \x20   time.sleep(60)\n";

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_job_stopped_at_a_sandbox_limit_is_not_a_crash_and_is_not_restarted() {
    let Some(bench) = bench("a_job_stopped_at_a_sandbox_limit_is_not_a_crash").await else {
        return;
    };
    let scope = JobScope {
        run_id: Some("run-1".into()),
        ..JobScope::default()
    };
    let mut notices = bench.runner.notices().subscribe();
    let mut request = build_request(&[]);
    request.limits.wall_seconds = 60;
    let error = bench
        .runner
        .run(
            &scope,
            &EngineRequest::Build(request),
            code(FORK_UNTIL_THE_LIMIT),
        )
        .await
        .unwrap_err();
    let JobError::LimitReached { reason } = &error else {
        panic!("expected a limit, got {error:?}");
    };
    assert!(
        reason.contains("process limit of") || reason.contains("memory limit of"),
        "{reason}"
    );
    assert_eq!(error.code(), "limit_reached");
    // Run once, not restarted, nothing kept, and the person is told which limit.
    let audit = bench.store.list_audit(None, 10).unwrap();
    let jobs: Vec<&str> = audit
        .iter()
        .filter(|a| a.kind == "job")
        .map(|a| a.outcome.as_str())
        .collect();
    assert_eq!(jobs, ["limit_reached"]);
    assert!(!bench.data.root().join("crashes").exists());
    let notice = notices.try_recv().expect("the person is told");
    assert!(
        notice.text.contains("sandbox limit") && notice.text.contains(reason.as_str()),
        "{}",
        notice.text
    );
    assert!(notices.try_recv().is_err());
    assert_eq!(bench.jobs_left(), 0);
}

#[tokio::test]
async fn model_code_cannot_read_the_project_data_or_reach_the_network() {
    let Some(bench) = bench("model_code_cannot_read_the_project_data_or_reach_the_network").await
    else {
        return;
    };
    std::fs::write(
        bench.data.root().join("calibration").join("private.json"),
        "{}",
    )
    .unwrap();
    let script = r#"
import os, socket
from build123d import *
PARAMS = {}

def attempt(name, action):
    try:
        action()
        print(name, "ALLOWED")
    except OSError as error:
        print(name, "DENIED")

def build(p, ctx):
    attempt("database", lambda: open("../../hotend.db", "rb").read(16))
    attempt("calibration", lambda: os.listdir("../../calibration"))
    attempt("write-outside", lambda: open("../escape.txt", "w").write("x"))
    attempt("socket", lambda: socket.socket(socket.AF_INET, socket.SOCK_STREAM))
    attempt("job-folder", lambda: open("scratch.txt", "w").write("x"))
    print("key", os.environ.get("ANTHROPIC_API_KEY"))
    return Box(10, 10, 10)
"#;
    let job = bench
        .runner
        .run(
            &JobScope::default(),
            &EngineRequest::Build(build_request(&[])),
            code(script),
        )
        .await
        .unwrap();
    assert_eq!(
        job.result.status,
        EngineStatus::Ok,
        "{:?}",
        job.result.error
    );
    assert_eq!(
        job.result.stdout,
        "database DENIED\ncalibration DENIED\nwrite-outside DENIED\nsocket DENIED\n\
         job-folder ALLOWED\nkey None\n"
    );
    assert!(!bench.data.jobs_dir().join("escape.txt").exists());
}

#[cfg(unix)]
const BOX_MODEL: &str =
    "from build123d import *\nPARAMS = {}\n\ndef build(p, ctx):\n    return Box(20, 20, 10)\n";

/// An engine that has been taken over: an interpreter that runs the real worker and then
/// rewrites the `result.json` it left, with Python statements acting on the parsed result
/// `r` (or setting `text` to replace the file as a whole). What the engine does to keep a
/// model away from its result is the engine's business; the core must not depend on it.
#[cfg(unix)]
struct TakenOverEngine {
    dir: std::path::PathBuf,
    _tmp: tempfile::TempDir,
}

#[cfg(unix)]
impl TakenOverEngine {
    /// Wraps the interpreter of `runtime` and returns the runtime to use instead.
    fn wrap(runtime: hotend_core::PythonRuntime) -> (Self, hotend_core::PythonRuntime) {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let python = dir.join("python");
        let script = format!(
            "#!/bin/sh\n\"{real}\" \"$@\"\nstatus=$?\n\"{real}\" -I \"{tamper}\" 2>/dev/null\nexit $status\n",
            real = runtime.python.display(),
            tamper = dir.join("tamper.py").display(),
        );
        std::fs::write(&python, script).unwrap();
        std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut read_only = runtime.read_only;
        read_only.push(dir.clone());
        let engine = Self { dir, _tmp: tmp };
        engine.tamper_with("    pass");
        (engine, hotend_core::PythonRuntime { python, read_only })
    }

    /// Sets what the engine does to its result from now on.
    fn tamper_with(&self, mutation: &str) {
        let code = format!(
            "import json, os\n\ndef tamper():\n    with open('result.json') as f:\n        \
             r = json.load(f)\n    text = None\n{mutation}\n    with open('result.json', 'w') as f:\n        \
             f.write(text if text is not None else json.dumps(r))\n\ntamper()\n"
        );
        std::fs::write(self.dir.join("tamper.py"), code).unwrap();
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_hostile_result_never_becomes_a_version() {
    let test = "a_hostile_result_never_becomes_a_version";
    let Some(runtime) = common::sandboxed_python(test).await else {
        return;
    };
    let (engine, runtime) = TakenOverEngine::wrap(runtime);
    let tmp = tempfile::tempdir().unwrap();
    let data = DataDir::resolve(Some(tmp.path())).unwrap();
    let store = Arc::new(Store::open(&data.db_path()).unwrap());
    let runner = JobRunner::new(
        data.clone(),
        runtime,
        Arc::clone(&store),
        JobConfig::default(),
    );
    let modeler = Modeler::new(data.clone(), Arc::clone(&store), runner);
    let project = store.create_project("hostile").unwrap();
    let scope = JobScope {
        project_id: Some(project.id.clone()),
        ..JobScope::default()
    };
    let cases: [(&str, &str, &str); 9] = [
        (
            "not JSON",
            "    text = 'this is not json'",
            "malformed_result",
        ),
        (
            "an unknown field",
            "    r['run_this'] = 'rm -rf ~'",
            "malformed_result",
        ),
        (
            "an unknown status",
            "    r['status'] = 'trust_me'",
            "malformed_result",
        ),
        (
            "a path outside the job folder",
            "    r['artifacts']['step'] = '../../../../../etc/hostname'",
            "malformed_result",
        ),
        (
            "an absolute path",
            "    r['artifacts']['mesh'] = '/etc/passwd'",
            "malformed_result",
        ),
        (
            "a missing file",
            "    os.remove('model.step')",
            "unsafe_artifact",
        ),
        (
            "a part name with markup",
            "    r['parts'][0]['name'] = '<img src=x onerror=alert(1)>'",
            "malformed_result",
        ),
        (
            "a view that is not an image",
            "    open('views/iso.png', 'w').write('<svg onload=alert(1)>')",
            "malformed_result",
        ),
        (
            "an oversized result",
            "    r['stdout'] = 'x' * (5 * 1024 * 1024)",
            "malformed_result",
        ),
    ];
    for (what, mutation, expected) in cases {
        engine.tamper_with(mutation);
        let error = modeler
            .build_version(&scope, &project.id, BOX_MODEL, BTreeMap::new(), "test", "")
            .await
            .expect_err(what);
        let ModelingError::Job(error) = error else {
            panic!("{what}: expected a job error, got {error:?}");
        };
        assert_eq!(error.code(), expected, "{what}: {error}");
        assert!(
            error.to_string().len() < 1000,
            "{what}: the error text must stay short"
        );
    }

    // Nothing was stored: no version row, no version or staging folder, no job folder.
    assert_eq!(store.project(&project.id).unwrap().current_version, None);
    assert!(store.list_versions(&project.id).unwrap().is_empty());
    let versions_dir = data.versions_dir(&project.id).unwrap();
    let leftovers = std::fs::read_dir(&versions_dir)
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(
        leftovers,
        0,
        "staging folders were left in {}",
        versions_dir.display()
    );
    assert_eq!(std::fs::read_dir(data.jobs_dir()).unwrap().count(), 0);
    // Each failure is in the audit log: five results were refused when they were read,
    // four when their files were imported.
    let audit = store.list_audit(Some(&project.id), 50).unwrap();
    let count = |kind: &str, outcome: &str| {
        audit
            .iter()
            .filter(|a| a.kind == kind && a.outcome == outcome)
            .count()
    };
    assert_eq!(count("job", "malformed_result"), 5);
    assert_eq!(count("version", "malformed_result"), 3);
    assert_eq!(count("version", "unsafe_artifact"), 1);
    assert_eq!(count("version", "ok"), 0);

    // The same script without tampering does become a version.
    engine.tamper_with("    pass");
    let report = modeler
        .build_version(&scope, &project.id, BOX_MODEL, BTreeMap::new(), "test", "")
        .await
        .unwrap();
    assert_eq!(report.version.unwrap().number, 1);
}
