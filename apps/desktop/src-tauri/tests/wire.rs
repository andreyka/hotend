//! The TypeScript types the window is given are generated from the mirrors in
//! `hotend_desktop::wire`, while the values it is sent are the core's own. This test
//! runs the commands on a real core (scripted agent, stand-in slicer, simulated printer)
//! and checks, for every answer and every event, that the mirror reads exactly what was
//! sent and writes it back unchanged: no field missing, none extra, none of another type.
//! Arguments are checked the other way round: they are written from the mirrors and the
//! core accepts them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a test stops at the first surprise"
)]

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hotend_core::{ApiError, CoreEvent};
use hotend_desktop::confirm::{Answer, ConfirmationPrompt, ConfirmationText};
use hotend_desktop::events::ShellEvent;
use hotend_desktop::grants::Pickers;
use hotend_desktop::ipc::{self, DispatchError};
use hotend_desktop::links::LinkOpener;
use hotend_desktop::shell::{Desktop, Shell};
use hotend_desktop::wire as w;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use url::Url;

struct Confirms;

impl ConfirmationPrompt for Confirms {
    fn ask(&self, _text: &ConfirmationText) -> Answer {
        Answer::Confirm
    }
}

struct PicksFolder(PathBuf);

impl Pickers for PicksFolder {
    fn pick_folder(&self, _title: &str) -> Option<PathBuf> {
        Some(self.0.clone())
    }

    fn pick_file(&self, _title: &str) -> Option<PathBuf> {
        None
    }
}

struct OpensNothing;

impl LinkOpener for OpensNothing {
    fn open(&self, _url: &Url) -> Result<(), String> {
        Ok(())
    }
}

/// Runs commands and keeps a list of the ones whose answer was checked against a mirror.
struct Checker<'a> {
    shell: &'a Shell,
    checked: Mutex<BTreeSet<String>>,
}

/// Where two JSON values first differ, as a path with both sides.
fn first_difference(sent: &Value, written: &Value, path: &str) -> Option<String> {
    match (sent, written) {
        (Value::Object(a), Value::Object(b)) => {
            for (key, value) in a {
                match b.get(key) {
                    None => {
                        return Some(format!(
                            "{path}/{key}: sent, but the mirror has no such field"
                        ));
                    }
                    Some(other) => {
                        if let Some(found) =
                            first_difference(value, other, &format!("{path}/{key}"))
                        {
                            return Some(found);
                        }
                    }
                }
            }
            b.keys()
                .find(|key| !a.contains_key(*key))
                .map(|key| format!("{path}/{key}: written by the mirror, but never sent"))
        }
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => a
            .iter()
            .zip(b)
            .enumerate()
            .find_map(|(index, (x, y))| first_difference(x, y, &format!("{path}/{index}"))),
        (a, b) if a == b => None,
        (a, b) => Some(format!("{path}: sent {a}, the mirror writes {b}")),
    }
}

/// `sent` read by the mirror `W` and written again must be `sent`.
fn same_through<W: Serialize + DeserializeOwned>(what: &str, sent: &Value) {
    let read: W = serde_json::from_value(sent.clone()).unwrap_or_else(|error| {
        panic!("{what}: the mirror cannot read what is sent: {error}\n{sent}")
    });
    let written = serde_json::to_value(&read).unwrap();
    if let Some(difference) = first_difference(sent, &written, "") {
        panic!("{what}: the mirror and the value that is sent differ at {difference}");
    }
}

impl Checker<'_> {
    /// Runs `command` and checks its answer against the mirror `W`.
    async fn ok<W: Serialize + DeserializeOwned>(&self, command: &str, args: Value) -> Value {
        let answer = match ipc::dispatch(self.shell, command, args).await {
            Ok(answer) => answer,
            Err(error) => panic!("{command} failed: {error:?}"),
        };
        same_through::<W>(command, &answer);
        self.checked.lock().unwrap().insert(command.to_owned());
        answer
    }

    /// Runs `command`, which must fail, and checks the error against its mirror.
    async fn refused(&self, command: &str, args: Value) -> ApiError {
        match ipc::dispatch(self.shell, command, args).await {
            Err(DispatchError::Failed(error)) => {
                same_through::<w::ApiError>(command, &serde_json::to_value(&error).unwrap());
                error
            }
            other => panic!("{command} should have failed in the shell, got {other:?}"),
        }
    }
}

/// An argument written from its mirror, as the generated TypeScript type would write it.
fn arg<W: Serialize>(mirror: W) -> Value {
    serde_json::to_value(mirror).unwrap()
}

/// A result with every field the engine may write (generated and kept current by the core's
/// `engine_result_contract` test), so a field the core accepts and the mirror does not know
/// is noticed without a build that produces it. The report of a `sweep` is the one part
/// that never reaches the window: it is the answer to a tool of the assistant.
#[test]
fn the_mirror_of_the_engine_s_result_has_every_field_of_it() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../crates/hotend-core/tests/fixtures/engine_result_example.json");
    let mut example: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(example.as_object_mut().unwrap().remove("sweep").is_some());
    let typed: hotend_core::EngineResult = serde_json::from_value(example).unwrap();
    same_through::<w::EngineResult>("the example", &serde_json::to_value(typed).unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_typescript_types_describe_what_the_core_sends() {
    let data = tempfile::tempdir().unwrap();
    let exports = tempfile::tempdir().unwrap();
    let core = support::open_scripted_core(data.path(), "builtin:cable_box_demo").await;
    // The stand-in slicer draws no plate preview, and a Bambu Connect review of a file
    // without one is refused: the slice below carries Hotend-made previews.
    let mut core_settings = core.settings_get().await.unwrap();
    core_settings.previews = hotend_core::PreviewsMode::HotendMade;
    core.settings_set(core_settings).await.unwrap();
    let shell = Shell::new(
        Ok(core.clone()),
        Desktop {
            prompt: Arc::new(Confirms),
            can_confirm: true,
            pickers: Arc::new(PicksFolder(exports.path().to_path_buf())),
            links: Arc::new(OpensNothing),
        },
    );
    let c = Checker {
        shell: &shell,
        checked: Mutex::default(),
    };

    // Every event the core reports from here on, as the shell forwards it.
    let events = Arc::new(Mutex::new(Vec::<CoreEvent>::new()));
    let collector = {
        let events = events.clone();
        let mut stream = core.events();
        tokio::spawn(async move {
            while let Ok(event) = stream.recv().await {
                events.lock().unwrap().push(event);
            }
        })
    };

    // ---- before anything is built ----
    c.ok::<Vec<w::Project>>("projects_list", json!({})).await;
    let project = c
        .ok::<w::Project>("project_create", json!({"name": "Cable box"}))
        .await;
    let id = project["id"].as_str().unwrap();
    let settings = c.ok::<w::Settings>("settings_get", json!({})).await;
    let slicers = c
        .ok::<Vec<w::SlicerInstallation>>("slicers_list", json!({}))
        .await;
    c.ok::<Vec<w::PrinterInfo>>("printers_list", json!({}))
        .await;
    c.ok::<w::Calibration>("calibration_get", json!({})).await;
    c.ok::<w::DoctorReport>("doctor_report", json!({"skipSelfTests": true}))
        .await;
    c.refused("project_get", json!({"projectId": "no-such-project"}))
        .await;
    // The printer's status setup. No address yet: the certificate check and the watch are
    // refused before anything is looked at or connected to (and no check is made later,
    // once an address is set: that would reach the network).
    c.ok::<w::PrinterLanSetup>("printer_lan_get", json!({}))
        .await;
    c.refused("printer_certificate_check", json!({})).await;
    c.refused("printer_watch_start", json!({})).await;
    c.ok::<Option<w::LanPrinterStatus>>("printer_watch_latest", json!({}))
        .await;
    c_plain(&shell, "printer_lan_key_set", json!({"code": "Zq12345678"})).await;
    c.ok::<w::PrinterLanSetup>(
        "printer_lan_set",
        json!({"ip": "192.168.1.50", "serial": "01P00A000000001"}),
    )
    .await;
    c.ok::<w::PrinterLanSetup>("printer_lan_get", json!({}))
        .await;
    c.refused(
        "slice_run",
        json!({"projectId": id, "profile": "demo-0.20mm", "plate": null}),
    )
    .await;

    // ---- one run of the scripted agent: two versions, the second passes its checks ----
    let started = c
        .ok::<w::MessageStarted>(
            "message_send",
            json!({"projectId": id, "text": "A cable box, please."}),
        )
        .await;
    let run_id = started["run_id"].as_str().unwrap().to_owned();
    let finished = |events: &[CoreEvent]| {
        events.iter().any(|event| {
            let json = serde_json::to_value(event).unwrap();
            json["run_id"] == run_id.as_str() && json["event"]["kind"] == "finished"
        })
    };
    for _ in 0..1200 {
        if finished(&events.lock().unwrap()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(finished(&events.lock().unwrap()), "the scripted run ended");

    let detail = c
        .ok::<w::ProjectDetail>("project_get", json!({"projectId": id}))
        .await;
    assert!(detail["transcript"].as_array().unwrap().len() >= 2);
    let current = detail["project"]["current_version"].as_u64().unwrap();
    assert!(current >= 1, "the scripted run built a version");
    c.ok::<Vec<w::Version>>("versions_list", json!({"projectId": id}))
        .await;
    // The first version and the current one (in the shipped script the first fails a
    // check and a later one passes: both shapes of a result).
    c.ok::<w::VersionDetail>("version_get", json!({"projectId": id, "number": 1}))
        .await;
    let version = c
        .ok::<w::VersionDetail>("version_get", json!({"projectId": id, "number": current}))
        .await;
    c.ok::<Vec<w::Question>>("questions_list", json!({"projectId": id, "version": null}))
        .await;
    c.ok::<w::EngineResult>(
        "checks_run",
        json!({"projectId": id, "checks": null, "sampleRanges": false}),
    )
    .await;
    c.ok::<Vec<w::RenderedView>>(
        "views_render",
        json!({
            "projectId": id, "views": ["iso"], "size": [320, 240],
            "section": arg(w::Section { plane: w::SectionPlane::XZ, offset: w::Num(0.0) }),
        }),
    )
    .await;

    // A size changed inside its range, and a model that does not build.
    let (name, spec) = version["result"]["param_specs"]
        .as_object()
        .and_then(|specs| {
            specs
                .iter()
                .find(|(_, spec)| spec["min"].is_number() && spec["max"].is_number())
        })
        .expect("the cable box has a size with a range");
    let middle = (spec["min"].as_f64().unwrap() + spec["max"].as_f64().unwrap()) / 2.0;
    let rebuilt = c
        .ok::<w::BuildReport>(
            "parameter_set",
            json!({"projectId": id, "name": name, "value": arg(w::Num(middle))}),
        )
        .await;
    assert!(rebuilt["version"].is_object(), "{rebuilt}");
    let broken = c
        .ok::<w::BuildReport>(
            "model_build",
            json!({
                "projectId": id, "code": "raise RuntimeError('not a model')", "note": "broken",
                "params": arg(BTreeMap::from([("wall".to_owned(), w::Num(2.0))])),
            }),
        )
        .await;
    assert!(broken["version"].is_null(), "{broken}");

    // ---- layout, files, gauges, calibration ----
    let folder = c_plain(&shell, "folder_pick", json!({"title": "Export to"})).await;
    let folder = folder.as_str().unwrap();
    c.ok::<w::Preparation>(
        "prepare_run",
        json!({
            "projectId": id, "version": null,
            "options": arg(w::PrepareOptions {
                copies: BTreeMap::from([("lid".to_owned(), 2)]),
                orientations: BTreeMap::new(),
            }),
        }),
    )
    .await;
    let latest = rebuilt["version"]["number"].as_u64().unwrap();
    let stored = c
        .ok::<Option<w::Preparation>>(
            "preparation_get",
            json!({"projectId": id, "version": latest}),
        )
        .await;
    assert!(stored.is_object());
    c.ok::<Option<w::Preparation>>("preparation_get", json!({"projectId": id, "version": 1}))
        .await;
    let files = c
        .ok::<Vec<w::ExportedFile>>(
            "export_run",
            json!({
                "projectId": id, "version": null, "parts": null, "destination": folder,
                "formats": arg(vec![w::ExportFormat::Step, w::ExportFormat::ThreeMf, w::ExportFormat::Stl]),
            }),
        )
        .await;
    assert!(files.as_array().unwrap().len() >= 3);
    // A snapshot of a version that was never printed: no printed file.
    let snapshot_folder = c_plain(
        &shell,
        "folder_pick",
        json!({"title": "Save snapshot files"}),
    )
    .await;
    c.ok::<w::Snapshot>(
        "snapshot_create",
        json!({"projectId": id, "name": "First box", "version": 1, "note": "the first"}),
    )
    .await;
    c.ok::<Vec<w::Snapshot>>("snapshots_list", json!({"projectId": id}))
        .await;
    let exported = c
        .ok::<w::SnapshotExport>(
            "snapshot_export",
            json!({"projectId": id, "name": "First box", "folder": snapshot_folder}),
        )
        .await;
    assert!(exported["printed"].is_null(), "{exported}");
    c.ok::<w::GaugeExport>(
        "gauge_export",
        json!({"projectId": id, "cards": null, "nozzleId": null, "destination": folder}),
    )
    .await;
    c.ok::<w::CalibrationUpdate>(
        "calibration_record",
        json!({
            "nozzleId": null,
            "readings": arg(vec![
                w::GaugeReading { feature: w::GaugeFeature::Hole, drawn: w::Num(5.0), measured: w::Num(4.85) },
                w::GaugeReading { feature: w::GaugeFeature::Peg, drawn: w::Num(5.0), measured: w::Num(5.05) },
            ]),
        }),
    )
    .await;
    c.ok::<w::Calibration>("calibration_get", json!({})).await;

    // ---- slicing ----
    c.ok::<Option<w::SliceOutcome>>("slice_get", json!({"projectId": id, "version": null}))
        .await;
    c.ok::<w::SliceOutcome>(
        "slice_run",
        json!({"projectId": id, "profile": "demo-0.20mm", "plate": null}),
    )
    .await;
    let slice = c
        .ok::<Option<w::SliceOutcome>>("slice_get", json!({"projectId": id, "version": null}))
        .await;
    assert!(slice.is_object());
    let kind = slicers[0]["kind"].as_str().unwrap();
    match ipc::dispatch(
        &shell,
        "slicer_choose",
        json!({"kind": kind, "executable": null}),
    )
    .await
    {
        Ok(answer) => same_through::<w::Settings>("slicer_choose", &answer),
        Err(DispatchError::Failed(error)) => {
            same_through::<w::ApiError>("slicer_choose", &serde_json::to_value(&error).unwrap());
        }
        Err(other) => panic!("slicer_choose: {other:?}"),
    }
    c.checked.lock().unwrap().insert("slicer_choose".to_owned());
    // What settings_get answers can be written back as it is.
    assert_eq!(
        c_plain(&shell, "settings_set", json!({"settings": settings})).await,
        Value::Null
    );

    // ---- printing: one job cancelled, one confirmed and run on the simulated printer ----
    let first = c
        .ok::<w::PrintJob>("print_propose", json!({"projectId": id, "note": "first"}))
        .await;
    c.ok::<w::PrintJob>("print_cancel", json!({"jobId": first["id"]}))
        .await;
    let job = c
        .ok::<w::PrintJob>("print_propose", json!({"projectId": id, "note": "second"}))
        .await;
    let job_id = job["id"].as_str().unwrap();
    for delivery in [
        w::DeliveryChoice::BambuConnect,
        w::DeliveryChoice::ExportToFolder {
            folder: folder.to_owned(),
        },
        w::DeliveryChoice::SimulatedPrinter,
    ] {
        c.ok::<w::PrintReview>(
            "print_review",
            json!({"jobId": job_id, "delivery": arg(delivery)}),
        )
        .await;
    }
    let confirmed = c
        .ok::<w::PrintConfirmation>(
            "print_confirm",
            json!({"jobId": job_id, "delivery": arg(w::DeliveryChoice::SimulatedPrinter)}),
        )
        .await;
    assert_eq!(confirmed["confirmed"], true);
    let mut last = String::new();
    for _ in 0..600 {
        let status = c
            .ok::<w::PrintJob>("print_status", json!({"jobId": job_id}))
            .await;
        c.ok::<Vec<w::PrinterInfo>>("printers_list", json!({}))
            .await;
        last = status["status"].as_str().unwrap().to_owned();
        if last == "finished" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(last, "finished", "the simulated print ends");
    c.ok::<Vec<w::PrintJob>>("print_jobs", json!({"projectId": id}))
        .await;
    // A snapshot of the printed version keeps the printed file, and its export has it.
    let printed = c
        .ok::<w::Snapshot>(
            "snapshot_create",
            json!({"projectId": id, "name": "Printed box", "version": null, "note": ""}),
        )
        .await;
    assert!(printed["printed_job_id"].is_string(), "{printed}");
    let exported = c
        .ok::<w::SnapshotExport>(
            "snapshot_export",
            json!({"projectId": id, "name": "Printed box", "folder": snapshot_folder}),
        )
        .await;
    assert!(
        exported["printed"].is_object() || exported["printed_missing"].is_object(),
        "{exported}"
    );
    c.ok::<w::Version>(
        "snapshot_restore",
        json!({"projectId": id, "name": "First box"}),
    )
    .await;
    c.ok::<w::Snapshot>(
        "snapshot_delete",
        json!({"projectId": id, "name": "Printed box"}),
    )
    .await;

    // ---- the hand-off and editable projects ----
    c.ok::<w::HandOffStatus>("handoff_status", json!({})).await;
    c.ok::<Vec<w::EditableProject>>("print_projects", json!({"projectId": id}))
        .await;
    // The stand-in slicer makes no project, so these three are refused here; their answers
    // are checked on the core's own values in the test below.
    c.refused(
        "print_project_make",
        json!({"projectId": id, "plate": null}),
    )
    .await;
    for command in ["print_project_review", "print_project_confirm"] {
        for destination in [
            w::ProjectDestination::BambuStudio,
            w::ProjectDestination::Folder {
                folder: folder.to_owned(),
            },
        ] {
            c.refused(
                command,
                json!({"projectExportId": "no-such-project", "destination": arg(destination)}),
            )
            .await;
        }
    }
    // On the heap: the whole run in one future is too large for the test thread's stack.
    Box::pin(research_commands(&c)).await;
    c.ok::<Vec<w::AuditEntry>>("audit_log", json!({"projectId": null, "limit": 500}))
        .await;
    c.ok::<Vec<w::AuditEntry>>("audit_log", json!({"projectId": id, "limit": 5}))
        .await;
    c.ok::<w::Version>("version_revert", json!({"projectId": id, "number": 2}))
        .await;
    c.ok::<w::ProjectDetail>("project_get", json!({"projectId": id}))
        .await;
    c.ok::<Vec<w::Project>>("projects_list", json!({})).await;

    // ---- the events of all of that ----
    collector.abort();
    let events = events.lock().unwrap().clone();
    let mut seen = BTreeSet::new();
    for event in events {
        let sent = serde_json::to_value(ShellEvent::from_core(event)).unwrap();
        same_through::<w::ShellEvent>("an event", &sent);
        let inner = &sent["event"];
        let detail = inner["event"]["type"]
            .as_str()
            .or(inner["type"].as_str())
            .unwrap_or("");
        seen.insert(format!(
            "{}/{}/{detail}",
            sent["channel"].as_str().unwrap(),
            inner["kind"].as_str().unwrap_or("")
        ));
    }
    for expected in [
        "run/agent/text_delta",
        "run/agent/tool_start",
        "run/agent/tool_end",
        "run/agent/usage",
        "run/finished/",
        "print//status",
        "print//progress",
    ] {
        assert!(
            seen.contains(expected),
            "no `{expected}` event among {seen:?}"
        );
    }

    // ---- every command with a typed answer was checked ----
    let plain = [
        "version_mesh",               // text
        "measure_run",                // the engine's own JSON, passed through
        "settings_set",               // nothing
        "provider_key_set",           // nothing
        "provider_key_delete",        // nothing
        "provider_key_status",        // a boolean
        "run_cancel",                 // a boolean
        "folder_pick",                // a path or nothing
        "file_pick",                  // a path or nothing
        "open_link",                  // nothing
        "events_subscribe",           // the stream itself, checked above
        "printer_lan_clear",          // nothing
        "printer_lan_key_set",        // nothing
        "printer_lan_key_delete",     // nothing
        "printer_certificate_forget", // nothing
        "printer_watch_start",        // nothing (refused here: no address)
        "printer_watch_stop",         // a boolean
        "research_consent_withdraw",  // nothing
        "search_key_set",             // nothing
        "search_key_delete",          // nothing
        "search_key_status",          // a boolean
        // Needs a printer to look at; checked on the core's values below
        // (`the_printer_mirrors_describe_what_the_core_sends`).
        "printer_certificate_check",
        // Refused with the stand-in slicer; checked on the core's values below
        // (`the_project_mirrors_describe_what_the_core_sends`).
        "print_project_make",
        "print_project_review",
        "print_project_confirm",
    ];
    let mut covered: BTreeSet<String> = c.checked.lock().unwrap().clone();
    covered.extend(plain.iter().map(|name| (*name).to_owned()));
    let all: BTreeSet<String> = ipc::command_names()
        .into_iter()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        covered, all,
        "a command was added without a check of its answer here"
    );
}

/// The answers of the project commands, which need the real Bambu Studio to be made: the
/// core's own values, with every optional field filled and empty, through the mirrors; and
/// a destination written from its mirror, read by the core.
#[test]
fn the_project_mirrors_describe_what_the_core_sends() {
    use hotend_core::{
        EditableProject, HandOffTarget, PrintJobDetail, ProjectDestination, ProjectKeys,
        ProjectReview,
    };
    use hotend_desktop::confirm::ProjectConfirmation;

    let sha = "c0ffee0000000000000000000000000000000000000000000000000000001234";
    let test_slice: hotend_core::SliceSummary = serde_json::from_value(json!({
        "status": "ok", "output": "test/plate_1.gcode.3mf", "sha256": "d".repeat(64),
        "time_s": 600.5, "layers": 20,
        "filament": [{"slot": 1, "grams": 3.5, "metres": 1.1, "nozzle_id": 1}],
        "bed": {"id": "textured-pei", "plate_type": "Textured PEI Plate", "temperature_c": 65.0},
        "warnings": [{"code": "w", "message": "m"}],
        "slicer": {"name": "BambuStudio", "version": "02.05.00.66"}
    }))
    .unwrap();
    let made = EditableProject {
        id: "export-1".to_owned(),
        project_id: "p".to_owned(),
        version: 3,
        plate: 2,
        plan_sha256: "a".repeat(64),
        slice_sha256: "b".repeat(64),
        path: PathBuf::from("/data/projects/p/versions/3/slicer-project/2-aaaa/box.3mf"),
        sha256: sha.to_owned(),
        keys: ProjectKeys {
            process: vec!["wall_loops".to_owned()],
            filament: vec![],
            printer: vec!["nozzle_diameter".to_owned()],
        },
        test_slice,
        proof: "passed".to_owned(),
        notice: hotend_core::PROJECT_OUTSIDE_CHECK.to_owned(),
        delivery: None,
        detail: PrintJobDetail::default(),
        created_at_ms: 1_700_000_000_000,
        updated_at_ms: 1_700_000_000_001,
    };
    let handed = EditableProject {
        delivery: Some("bambu_studio".to_owned()),
        detail: PrintJobDetail {
            message: Some("Saved.".to_owned()),
            path: Some(PathBuf::from("/mnt/d/Models/Hotend/box.3mf")),
            windows_path: Some(r"D:\Models\Hotend\box.3mf".to_owned()),
            opened: Some(true),
            opener: Some("Windows explorer, from WSL".to_owned()),
            link: Some("bambu-connect://import-file?path=x".to_owned()),
            ..PrintJobDetail::default()
        },
        ..made.clone()
    };
    fn value<T: Serialize>(of: &T) -> Value {
        serde_json::to_value(of).unwrap()
    }
    for project in [&made, &handed] {
        same_through::<w::EditableProject>("an editable project", &value(project));
    }
    same_through::<Vec<w::EditableProject>>(
        "print_projects",
        &serde_json::to_value([&made, &handed]).unwrap(),
    );

    let finding: hotend_core::LintFinding = serde_json::from_value(json!({
        "severity": "warning", "code": "position_unknown", "line": 3,
        "message": "a move starts before the position is known", "excerpt": "G1 X1"
    }))
    .unwrap();
    for (destination, handoff) in [
        (
            ProjectDestination::BambuStudio,
            Some(HandOffTarget {
                folder: PathBuf::from("/mnt/d/Models/Hotend"),
                windows_folder: Some(r"D:\Models\Hotend".to_owned()),
                opener: "Windows explorer, from WSL".to_owned(),
                opens: true,
            }),
        ),
        (
            ProjectDestination::Folder {
                folder: PathBuf::from("/home/me/projects"),
            },
            None,
        ),
    ] {
        let review = ProjectReview {
            project: made.clone(),
            destination,
            file_name: format!("Cable_box-v3-{}.3mf", &sha[..32]),
            handoff,
            review_text: "Bambu Studio project.\nWhere it goes: there.".to_owned(),
            warnings: vec![finding.clone()],
            notice: hotend_core::PROJECT_OUTSIDE_CHECK.to_owned(),
        };
        same_through::<w::ProjectReview>("print_project_review", &value(&review));
    }
    for (confirmed, project) in [(false, &made), (true, &handed)] {
        let answer = ProjectConfirmation {
            confirmed,
            project: project.clone(),
        };
        same_through::<w::ProjectConfirmation>("print_project_confirm", &value(&answer));
    }

    // The destination as the window writes it is the core's.
    for (mirror, core) in [
        (
            w::ProjectDestination::BambuStudio,
            ProjectDestination::BambuStudio,
        ),
        (
            w::ProjectDestination::Folder {
                folder: "/home/me/projects".to_owned(),
            },
            ProjectDestination::Folder {
                folder: PathBuf::from("/home/me/projects"),
            },
        ),
    ] {
        let read: ProjectDestination = serde_json::from_value(arg(mirror)).unwrap();
        assert_eq!(read, core);
    }
}

/// A printer's report, as a command's answer and as an event, and the certificate dialog's
/// answer with every optional field filled and empty, on the core's own values.
#[test]
fn the_printer_mirrors_describe_what_the_core_sends() {
    use hotend_core::{CertificatePin, LanPrinterStatus, PrinterLanAddress, PrinterLanSetup};
    use hotend_desktop::confirm::CertificateConfirmation;
    use hotend_desktop::shell::LanSetup;

    let status: LanPrinterStatus = serde_json::from_value(json!({
        "state": {
            "phase": "printing", "headline": "Printing", "progress_percent": 40,
            "remaining_minutes": 12, "layer": 8, "total_layers": 20, "job_name": "box",
            "bed_c": 60.0, "problems": [], "ams": []
        },
        "nozzles": [
            {"id": 0, "current_c": 219.5, "target_c": 220.0},
            {"id": 1, "current_c": null, "target_c": null}
        ],
        "bed": {"current_c": 60.0, "target_c": 60.0},
        "chamber": {"current_c": null, "target_c": null},
        "reports": 3, "job_id": "job-1", "matched_name": "box", "connected": true, "ended": null
    }))
    .unwrap();
    let ended = LanPrinterStatus {
        job_id: None,
        matched_name: None,
        connected: false,
        ended: Some("the printer closed the connection".to_owned()),
        ..status.clone()
    };
    for status in [&status, &ended] {
        same_through::<Option<w::LanPrinterStatus>>(
            "printer_watch_latest",
            &serde_json::to_value(Some(status)).unwrap(),
        );
        let event = ShellEvent::from_core(CoreEvent::Printer {
            status: Box::new(status.clone()),
        });
        same_through::<w::ShellEvent>("a printer event", &serde_json::to_value(event).unwrap());
    }

    let empty = PrinterLanSetup {
        address: None,
        trust: "unconfirmed".to_owned(),
        ca_file: None,
        pin: None,
        access_code: "missing".to_owned(),
        access_code_store: hotend_core::SecretStoreKind::Environment,
        watching: false,
    };
    let full = PrinterLanSetup {
        address: Some(PrinterLanAddress {
            ip: "192.168.1.50".to_owned(),
            serial: "01P00A000000001".to_owned(),
        }),
        trust: "pinned".to_owned(),
        ca_file: Some(PathBuf::from("/home/me/.config/hotend/bambu-ca.pem")),
        pin: Some(CertificatePin {
            ip: "192.168.1.50".to_owned(),
            serial: "01P00A000000001".to_owned(),
            sha256: "ab".repeat(32),
            subject: "CN=01P00A000000001".to_owned(),
            confirmed_at_ms: 1_700_000_000_000,
        }),
        access_code: "secret_store".to_owned(),
        access_code_store: hotend_core::SecretStoreKind::File,
        watching: true,
    };
    for (confirmed, setup) in [(false, empty), (true, full)] {
        let setup = LanSetup::from_core(setup);
        same_through::<w::PrinterLanSetup>(
            "printer_lan_get",
            &serde_json::to_value(&setup).unwrap(),
        );
        let answer = CertificateConfirmation { confirmed, setup };
        same_through::<w::CertificateConfirmation>(
            "printer_certificate_check",
            &serde_json::to_value(&answer).unwrap(),
        );
    }
}

/// A snapshot export whose recorded printed file was found nowhere, on the core's own
/// values: the one shape the run above does not reach.
#[test]
fn the_snapshot_mirrors_describe_what_the_core_sends() {
    use hotend_core::{MissingPrinted, Snapshot, SnapshotExport, SnapshotFile};

    let snapshot = Snapshot {
        id: "s1".to_owned(),
        project_id: "p".to_owned(),
        name: "Printed box".to_owned(),
        version: 3,
        note: String::new(),
        created_at_ms: 1_700_000_000_000,
        printed_job_id: Some("job-1".to_owned()),
        printed_sha256: Some("e".repeat(64)),
    };
    let export = SnapshotExport {
        snapshot: snapshot.clone(),
        folder: PathBuf::from("/home/me/snapshots/Printed box"),
        files: vec![SnapshotFile {
            kind: "manifest".to_owned(),
            source: "written".to_owned(),
            path: PathBuf::from("/home/me/snapshots/Printed box/snapshot.json"),
            bytes: 420,
            sha256: "f".repeat(64),
        }],
        printed: None,
        printed_missing: Some(MissingPrinted {
            job_id: "job-1".to_owned(),
            sha256: "e".repeat(64),
            message: "printed file missing: no copy with that hash".to_owned(),
        }),
    };
    same_through::<w::Snapshot>("a snapshot", &serde_json::to_value(&snapshot).unwrap());
    same_through::<w::SnapshotExport>("snapshot_export", &serde_json::to_value(&export).unwrap());
}

/// Where web research comes from: the answers of its commands through their mirrors.
async fn research_commands(c: &Checker<'_>) {
    c.ok::<Vec<w::SearchSourceOption>>("research_source_options", json!({}))
        .await;
    c.refused(
        "research_consent_ask",
        json!({"source": arg(w::SearchSource::Tavily)}),
    )
    .await;
    c.ok::<w::Settings>(
        "research_source_set",
        json!({"source": arg(w::SearchSource::Keenable)}),
    )
    .await;
    c_plain(
        c.shell,
        "search_key_set",
        json!({"service": arg(w::SearchService::Keenable), "key": "kn-test-key"}),
    )
    .await;
    let answer = c
        .ok::<w::ResearchConsentAnswer>(
            "research_consent_ask",
            json!({"source": arg(w::SearchSource::Keenable)}),
        )
        .await;
    assert_eq!(answer["agreed"], true, "{answer}");
    c.ok::<Vec<w::SearchSourceOption>>("research_source_options", json!({}))
        .await;
    c_plain(c.shell, "research_consent_withdraw", json!({})).await;
    c_plain(
        c.shell,
        "search_key_status",
        json!({"service": arg(w::SearchService::Keenable)}),
    )
    .await;
    c_plain(
        c.shell,
        "search_key_delete",
        json!({"service": arg(w::SearchService::Keenable)}),
    )
    .await;
}

async fn c_plain(shell: &Shell, command: &str, args: Value) -> Value {
    match ipc::dispatch(shell, command, args).await {
        Ok(answer) => answer,
        Err(error) => panic!("{command} failed: {error:?}"),
    }
}
