//! A design around a bought device (`ctx.component("sample_phone")`) through the path the
//! window uses: the core starts the real worker in the real sandbox, reads its result with
//! the strict types, and stores, lays out, plans, exports and gauges the version.
//!
//! The engine's own scenario tests drive the worker directly and read `result.json` as
//! plain JSON. That hid a field the core refused (`malformed_result: unknown field
//! "components"`), so these tests go through the core to the end. They need `python/.venv`
//! and a usable sandbox; without them each fails, and only with `HOTEND_TEST_ALLOW_SKIP=1`
//! it prints why it is skipped instead. No test makes a model request.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;

use hotend_core::engine::{CheckStatus, ComponentUse, EngineResult, MaterialContext};
use hotend_core::gateway::RunSession;
use hotend_core::{Core, CoreConfig, EngineStatus, ExportFormat};
use serde_json::{Value, json};

const CASE: &str = "scenarios/sample-phone-case/model.py";

/// The material of the scenario: the flexible one a stretch fit needs.
fn tpu() -> MaterialContext {
    MaterialContext {
        id: "tpu-95a".into(),
        family: "TPU".into(),
        density_g_cm3: None,
        max_overhang_deg: None,
    }
}

async fn use_material(core: &Core, material: MaterialContext) {
    let mut settings = core.settings_get().await.unwrap();
    settings.material = material;
    settings.plate_type = Some("textured-pei".into());
    // A flexible filament is fed from the outside holder of the main nozzle only.
    settings.spool.slot = "external_main".into();
    core.settings_set(settings).await.unwrap();
}

/// The one device of a result, with what must hold for it whatever the model did.
fn the_phone(result: &EngineResult) -> &ComponentUse {
    let [phone] = result.components.as_slice() else {
        panic!("one component, got {:?}", result.components);
    };
    assert_eq!(phone.id, "sample_phone");
    assert_eq!(phone.name, "Sample phone (invented)");
    assert_eq!(phone.kind, "phone");
    assert_eq!(phone.size, [80.0, 130.0, 10.0]);
    assert_eq!(phone.provenance, "standard");
    assert!(phone.features.iter().all(|f| f["id"].is_string()));
    assert!(phone.sources["sample"]["url"].is_string());
    assert_eq!(phone.body["name"], "sample_phone");
    // What the component asks is also among the questions of the version.
    let asked = result.questions.as_deref().unwrap_or_default();
    assert!(phone.questions.iter().all(|q| asked.contains(q)));
    phone
}

#[tokio::test]
async fn a_case_for_a_shipped_device_goes_through_the_core_to_plates_files_and_gauges() {
    let test = "a_case_for_a_shipped_device_goes_through_the_core_to_plates_files_and_gauges";
    let Some(t) = common::open_core(test, CoreConfig::default()).await else {
        return;
    };
    let core = &t.core;
    use_material(core, tpu()).await;
    let project = core.project_create("sample phone case").await.unwrap();
    let id = project.id.as_str();

    // ---- build: the agent's tool call, as in the window ----
    let session = RunSession::new("run-component", id);
    let code = json!({"code": common::model(CASE), "note": "case"});
    let outcome = core
        .gateway()
        .call(&session, "write_model_code", code)
        .await;
    assert!(outcome.error.is_none(), "{outcome:?}");
    let told = outcome.text_content();
    assert!(
        told.starts_with("status: ok\nversion: 1 created and now current\n"),
        "{told}"
    );
    assert!(told.contains("questions for the user ("), "{told}");
    assert!(told.contains("reference_fit: Not fully proven"), "{told}");

    // The stored version has the device, as the window reads it.
    let version = core.version_get(id, 1).await.unwrap();
    assert_eq!(version.result.status, EngineStatus::Ok);
    let phone = the_phone(&version.result);
    assert!(phone.used_features.contains(&"usb_c".to_owned()));
    assert!(!phone.proven && !phone.unverified.is_empty() && !phone.questions.is_empty());
    assert!(version.result.references.is_empty());
    // It is stored as compact JSON, and a file an older Hotend wrote (indented) reads the
    // same.
    let path = version.dir.join("result.json");
    let stored = std::fs::read_to_string(&path).unwrap();
    assert!(!stored.contains('\n'), "result.json is not compact");
    let value: Value = serde_json::from_str(&stored).unwrap();
    let pretty = serde_json::to_string_pretty(&value).unwrap();
    println!(
        "result.json: {} bytes, indented {}",
        stored.len(),
        pretty.len()
    );
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, pretty).unwrap();
    assert_eq!(
        core.version_get(id, 1).await.unwrap().result,
        version.result
    );
    let questions = core.questions_list(id, None).await.unwrap();
    let asked: Vec<&str> = questions.iter().map(|q| q.requirement.as_str()).collect();
    for question in ["side_wall", "phone_fit", "sample_phone_usb_c"] {
        assert!(asked.contains(&question), "{asked:?}");
    }
    let mesh: Value = serde_json::from_slice(&core.version_mesh(id, 1).await.unwrap()).unwrap();
    assert_eq!(mesh["hotend"]["references"][0]["name"], "sample_phone");
    let index = std::fs::read(version.dir.join("model.parts.json")).unwrap();
    let index: Value = serde_json::from_slice(&index).unwrap();
    assert_eq!(index["print"]["components"], json!(["sample_phone"]));

    // ---- what the window does on a version ----
    let checks = core.checks_run(id, None, false).await.unwrap();
    assert_eq!(checks.status, EngineStatus::Ok);
    the_phone(&checks);
    let few = core
        .checks_run(id, Some(vec!["contract".into()]), false)
        .await
        .unwrap();
    the_phone(&few);
    assert!(few.checks.iter().any(|c| c.id == "contract"));
    let measured = core
        .measure_run(id, vec![json!({"kind": "bbox", "part": "case"})])
        .await
        .unwrap();
    assert_eq!(measured[0]["ok"], true);
    let views = core
        .views_render(id, vec!["iso".into()], None, [320, 240])
        .await
        .unwrap();
    assert_eq!(views.len(), 1);

    // ---- layout, print facts and the plan made from them ----
    let prepared = core
        .prepare_run(id, None, Default::default())
        .await
        .unwrap();
    assert_eq!(prepared.status, EngineStatus::Ok, "{:?}", prepared.checks);
    assert_eq!(prepared.plates.len(), 1);
    assert!(prepared.unplaced.is_empty());
    assert!(prepared.has_print_facts());
    assert!(prepared.plate_file(1).unwrap().is_file());
    let nozzle = prepared
        .checks
        .iter()
        .find(|c| c.id == "material_nozzle")
        .expect("a flexible filament is held to its nozzle");
    assert_eq!(nozzle.status, CheckStatus::Pass);
    // The facts were read by the strict types and the rules decided something from them.
    let plan = core.print_plan_get(id, None, None).await.unwrap();
    assert!(plan.blocked.is_none(), "{:?}", plan.blocked);
    assert_eq!(plan.facts.as_ref().unwrap()["piece_count"], 1);
    assert!(!plan.decisions.is_empty());

    // ---- files ----
    let out = tempfile::tempdir().unwrap();
    let formats = vec![ExportFormat::Step, ExportFormat::ThreeMf, ExportFormat::Stl];
    let files = core
        .export_run(id, None, formats, None, out.path())
        .await
        .unwrap();
    assert_eq!(files.len(), 3);
    assert!(files.iter().all(|f| f.path.is_file() && f.bytes > 1000));

    // ---- the ring gauge of the device's pocket, from `print.components` of the version ----
    let gauges_dir = tempfile::tempdir().unwrap();
    let gauges = core
        .gauge_export(Some(id), None, None, gauges_dir.path())
        .await
        .unwrap();
    assert!(gauges.warnings.is_empty(), "{:?}", gauges.warnings);
    let [gauge] = gauges.gauges.as_slice() else {
        panic!("one gauge, got {:?}", gauges.gauges);
    };
    assert_eq!(
        (gauge.card.as_str(), gauge.name.as_str()),
        ("pocket", "phone")
    );
    assert_eq!(gauge.parts, ["phone.ring"]);
    assert_eq!(gauge.args["reference"], "sample_phone");
    assert!(gauges_dir.path().join("gauge-plate-1.3mf").is_file());
    // Its readings go into the calibration, like those of any gauge.
    let hole = &gauge.dimensions[0];
    let update = core
        .calibration_record(
            vec![hotend_core::GaugeReading {
                feature: hotend_core::GaugeFeature::Hole,
                drawn: hole.drawn,
                measured: hole.drawn - 0.1,
            }],
            None,
        )
        .await
        .unwrap();
    assert_eq!(update.changes[0].quantity, "hole_offset");

    // ---- another version from a slider, now with the calibrated printer ----
    let report = core.parameter_set(id, "wall", 1.6).await.unwrap();
    assert_eq!(report.version.expect("a version").number, 2);
    the_phone(&report.result);

    // ---- sweep: the settings are built and judged with the device ----
    let outcome = core
        .gateway()
        .call(
            &session,
            "sweep_parameters",
            json!({"params": [{"name": "wall", "values": [1.4]}]}),
        )
        .await;
    assert!(outcome.error.is_none(), "{outcome:?}");
    let told = outcome.text_content();
    assert!(told.contains("1. wall=1.4: builds and passes"), "{told}");
    assert!(told.contains("reference_fit"), "{told}");
}

/// The wrong material: the stretch fit of the case cannot work in PLA. A build that fails
/// its checks reports the device too, and still becomes a version.
#[tokio::test]
async fn a_device_design_that_fails_its_checks_is_read_too() {
    let test = "a_device_design_that_fails_its_checks_is_read_too";
    let Some(t) = common::open_core(test, CoreConfig::default()).await else {
        return;
    };
    let core = &t.core;
    let project = core.project_create("case in PLA").await.unwrap();
    let build = async |code: String| {
        core.model_build(&project.id, &code, BTreeMap::new(), "")
            .await
            .unwrap()
    };
    let report = build(common::model("phone_case.py")).await;
    assert_eq!(report.result.status, EngineStatus::CheckError);
    assert!(
        report
            .result
            .checks
            .iter()
            .any(|c| c.id == "reference_fit" && c.status == CheckStatus::Fail)
    );
    the_phone(&report.result);
    assert_eq!(report.version.expect("a version").number, 1);

    // A component that does not exist is a model error, without any device fields.
    let script = "from build123d import *\nPARAMS = {}\ndef build(p, ctx):\n    \
                  ctx.component('phone_99')\n    return Box(10, 10, 10)\n";
    let report = build(script.to_owned()).await;
    assert_eq!(report.result.status, EngineStatus::ModelError);
    assert!(report.version.is_none() && report.result.components.is_empty());

    // A design without a device answers as it always did: the keys are not there at all.
    let report = build(common::model("box.py")).await;
    assert_eq!(report.result.status, EngineStatus::Ok);
    let sent = serde_json::to_value(&report.result).unwrap();
    assert!(sent.get("components").is_none() && sent.get("references").is_none());
}
