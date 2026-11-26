//! The whole loop through the facade: a scripted agent drives the gateway, the gateway
//! builds real models through the sandboxed engine, and the UI commands read the results.
//!
//! These tests need `python/.venv`; without it each prints why it is skipped. No test makes
//! a model request: the scripted agent only calls tools.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use futures_util::future::BoxFuture;
use hotend_core::gateway::{FilamentUse, RunSession, SlicerInfo};
use hotend_core::{
    ContentItem, CoreConfig, CoreError, EngineStatus, ExportFormat, GatewayPolicy, ProviderKind,
    RunEvent, SliceBackend, SliceError, SliceRequest, SliceSummary, StopReason, VersionStatus,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The answers the scripted agent got for its tool calls, from the stored history.
fn tool_log(history: &Value) -> Vec<Value> {
    history.as_array().unwrap().last().unwrap()["log"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["response"]["result"].clone())
        .collect()
}

fn text(result: &Value) -> String {
    result["content"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|i| i["type"] == "text")
                .map(|i| i["text"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn an_agent_designs_a_part_and_the_ui_commands_see_it() {
    let test = "an_agent_designs_a_part_and_the_ui_commands_see_it";
    let Some(t) = common::open_core(test, CoreConfig::default()).await else {
        return;
    };
    let core = &t.core;
    let project = core.project_create("Cable box").await.unwrap();
    let box_code = common::model("lidded_box.py");
    let steps = json!([
        {"say": "Designing the box. "},
        {"tool": "set_parameter", "args": {"name": "width", "value": 100}},
        {"tool": "write_model_code", "args": {"code": common::model("broken_exception.py"), "note": "first try"}},
        {"tool": "write_model_code", "args": {"code": box_code, "note": "lidded box"}},
        {"tool": "set_parameter", "args": {"name": "width", "value": 100}},
        {"tool": "set_parameter", "args": {"name": "width", "value": 9999}},
        {"tool": "run_checks", "args": {"checks": ["min_wall", "overhangs"]}},
        {"tool": "measure", "args": {"queries": [{"kind": "bbox", "part": "lid"}, {"kind": "volume"}]}},
        {"tool": "render_views", "args": {"views": ["iso", "front"], "section": {"plane": "XZ"}}},
        {"tool": "read_docs", "args": {"query": "fillet edges", "limit": 2}},
        {"tool": "list_cards", "args": {}},
        {"tool": "slice", "args": {"profile": "0.20mm Standard"}},
        {"tool": "propose_print", "args": {"note": "ready"}},
        {"tool": "write_model_code", "args": {"code": box_code, "extra": true}},
        {"say": "Done."}
    ]);
    let run = core
        .message_send(&project.id, &json!({"steps": steps}).to_string())
        .await
        .unwrap();
    let run_id = run.run_id.clone();
    let events = common::collect(run).await;
    assert_eq!(
        events.last().unwrap(),
        &RunEvent::Finished {
            stop_reason: StopReason::Done,
            error: None
        },
        "{events:?}"
    );

    // What the agent was told, call by call.
    let detail = core.project_get(&project.id).await.unwrap();
    assert_eq!(
        detail.transcript.last().unwrap().text,
        "Designing the box. Done."
    );
    let history: Value = {
        let store = hotend_core::store::Store::open(&t.tmp.path().join("hotend.db")).unwrap();
        json!(store.history(&project.id).unwrap().unwrap().1)
    };
    let log = tool_log(&history);
    let code_of = |i: usize| log[i]["error"]["code"].as_str().unwrap_or("").to_owned();

    assert_eq!(
        code_of(0),
        "no_current_version",
        "a parameter needs a model first"
    );

    let failed = text(&log[1]);
    assert_eq!(
        log[1]["ok"], true,
        "a model error is a result the model reads"
    );
    assert!(
        failed.starts_with(
            "status: model_error\nno version was created; the project still has no version\n"
        ),
        "{failed}"
    );
    assert!(failed.contains("error: ValueError at line 9:"), "{failed}");
    assert!(failed.contains("line 9, in rounded"), "{failed}");

    let built = text(&log[2]);
    assert!(
        built.starts_with("status: ok\nversion: 1 created and now current\n"),
        "{built}"
    );
    assert!(
        built.contains("parameters: depth=60 mm [30..220];"),
        "{built}"
    );
    assert!(
        built.contains("- box: 90 x 60 x 35 mm, 29.444 cm3, 36.51 g solid, 19 faces"),
        "{built}"
    );
    assert!(
        built.contains("checks: 0 fail, 1 warn, 17 pass, 2 skipped"),
        "{built}"
    );
    // The lid is judged the way it will be printed (turned over, no overhang); what is
    // left is the joint that was drawn by hand instead of with a feature card.
    assert!(!built.contains("warn overhangs"), "{built}");
    assert!(
        built.contains("- warn fit_pairs: 'box' and 'lid'"),
        "{built}"
    );
    assert!(built.contains("stdout:\nlid lip 85.4 x 55.4"), "{built}");

    let wider = text(&log[3]);
    assert!(
        wider.starts_with("status: ok\nversion: 2 created and now current\n"),
        "{wider}"
    );
    assert!(wider.contains("width=100 mm"), "{wider}");

    let refused = text(&log[4]);
    assert!(
        refused.starts_with(
            "status: model_error\nno version was created; the current version is still 2\n"
        ),
        "{refused}"
    );
    assert!(
        refused.contains("ParameterError") && refused.contains("9999"),
        "{refused}"
    );

    let checks = text(&log[5]);
    assert!(
        checks.starts_with("status: ok (version 2)\nchecks: 0 fail, 0 warn, 4 pass, 0 skipped\n"),
        "{checks}"
    );

    let measured = text(&log[6]);
    assert!(
        measured.contains(r#""size":[100.0,60.0,4.0]"#),
        "{measured}"
    );
    assert!(
        measured.lines().nth(1).unwrap().starts_with("2. {"),
        "{measured}"
    );

    let views = log[7]["content"].as_array().unwrap();
    assert_eq!(views.len(), 3);
    assert!(views[0]["text"].as_str().unwrap().contains("iso, front"));
    for (item, name) in views[1..].iter().zip(["iso", "front"]) {
        let item: ContentItem = serde_json::from_value(item.clone()).unwrap();
        let ContentItem::Image {
            path,
            media_type,
            data_b64,
        } = item
        else {
            panic!("expected an image");
        };
        assert_eq!(
            (path.as_str(), media_type.as_str()),
            (format!("views/{name}.png").as_str(), "image/png")
        );
        assert!(data_b64.starts_with("iVBORw0KGgo"), "not an inlined PNG");
        assert!(data_b64.len() > 1000);
    }

    let docs = text(&log[8]);
    assert!(
        docs.starts_with("installed: bd_warehouse 0.4.0; build123d 0.13.0;"),
        "{docs}"
    );
    assert!(docs.contains("## build123d.fillet (function)"), "{docs}");

    // The feature cards come from the installed hotend_fit package, or the call says that
    // it is missing; it never invents a list.
    if log[9]["ok"] == true {
        assert!(text(&log[9]).contains("hotend_fit"));
    } else {
        assert_eq!(code_of(9), "not_installed");
    }
    assert_eq!(code_of(10), "not_configured");
    assert_eq!(code_of(11), "not_configured");
    assert_eq!(code_of(12), "invalid_arguments");
    assert!(
        log[12]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("extra")
    );

    // The UI commands see the same state.
    assert_eq!(detail.project.current_version, Some(2));
    assert_eq!(detail.versions.len(), 2);
    assert_eq!(detail.versions[0].origin, "write_model_code");
    assert_eq!(detail.versions[0].note, "lidded box");
    assert_eq!(detail.versions[1].origin, "set_parameter");
    assert_eq!(detail.versions[1].parent, Some(1));
    assert_eq!(detail.versions[1].params["width"], 100.0);
    assert_eq!(detail.versions[1].status, VersionStatus::Ok);

    let version = core.version_get(&project.id, 2).await.unwrap();
    assert_eq!(version.result.status, EngineStatus::Ok);
    assert_eq!(version.result.parts.len(), 2);
    for file in [
        "model.py",
        "params.json",
        "result.json",
        "model.step",
        "model.brep",
        "model.parts.json",
        "mesh.json",
        "views/iso.png",
    ] {
        let path = version.dir.join(file);
        assert!(path.is_file(), "{file} is missing from the version folder");
        assert!(
            std::fs::metadata(&path).unwrap().permissions().readonly(),
            "{file} is writable"
        );
    }
    assert_eq!(
        std::fs::read_to_string(version.dir.join("model.py")).unwrap(),
        box_code
    );
    let params: Value =
        serde_json::from_slice(&std::fs::read(version.dir.join("params.json")).unwrap()).unwrap();
    assert_eq!(params["width"], 100.0);
    let mesh: Value =
        serde_json::from_slice(&core.version_mesh(&project.id, 2).await.unwrap()).unwrap();
    assert_eq!(mesh["hotend"]["parts"]["lid"], "/model/lid");
    assert!(mesh["instances"].is_array());

    // Audit: every tool call with the hash of its canonical arguments, and no job folders.
    let audit = core.audit_log(Some(&project.id), 200).await.unwrap();
    let tools: Vec<_> = audit.iter().rev().filter(|a| a.kind == "tool").collect();
    assert_eq!(tools.len(), 13);
    assert!(
        tools
            .iter()
            .all(|a| a.run_id.as_deref() == Some(run_id.as_str()))
    );
    let outcomes: Vec<&str> = tools.iter().map(|a| a.outcome.as_str()).collect();
    assert_eq!(&outcomes[..3], ["no_current_version", "ok", "ok"]);
    assert_eq!(outcomes[12], "invalid_arguments");
    let expected = hex(&Sha256::digest(br#"{"name":"width","value":100}"#));
    assert_eq!(tools[0].args_sha256.as_deref(), Some(expected.as_str()));
    assert_eq!(tools[2].detail["version"], 1);
    assert!(
        !serde_json::to_string(&audit).unwrap().contains("fillet("),
        "arguments are hashed, not stored"
    );
    assert!(
        audit
            .iter()
            .any(|a| a.kind == "version" && a.outcome == "ok")
    );
    assert_eq!(
        std::fs::read_dir(t.tmp.path().join("jobs"))
            .unwrap()
            .count(),
        0
    );

    // parameter_set, checks_run and export_run from the UI side.
    let report = core
        .parameter_set(&project.id, "height", 50.0)
        .await
        .unwrap();
    let third = report.version.expect("a valid value creates a version");
    assert_eq!((third.number, third.origin.as_str()), (3, "set_parameter"));
    let report = core
        .parameter_set(&project.id, "height", 1.0)
        .await
        .unwrap();
    assert!(report.version.is_none());
    assert_eq!(report.result.error.unwrap().kind, "ParameterError");
    assert!(matches!(
        core.parameter_set(&project.id, "Bad Name", 1.0).await,
        Err(CoreError::Modeling(_))
    ));

    let checks = core
        .checks_run(&project.id, Some(vec!["valid_solid".into()]), false)
        .await
        .unwrap();
    assert_eq!(checks.checks.len(), 2);
    assert!(
        checks.artifacts.step.is_none(),
        "running checks writes no artefacts"
    );
    assert!(matches!(
        core.checks_run(&project.id, Some(vec!["rm -rf".into()]), false)
            .await,
        Err(CoreError::InvalidInput(_))
    ));

    let out = tempfile::tempdir().unwrap();
    std::fs::write(out.path().join("model.step"), "an older export").unwrap();
    let files = core
        .export_run(
            &project.id,
            Some(1),
            vec![ExportFormat::Step, ExportFormat::Stl, ExportFormat::ThreeMf],
            Some(vec!["lid".into()]),
            out.path(),
        )
        .await
        .unwrap();
    let names: Vec<String> = files
        .iter()
        .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["model.step", "model.3mf", "lid.stl"]);
    let step = std::fs::read(out.path().join("model.step")).unwrap();
    assert!(step.starts_with(b"ISO-10303-21;"));
    assert_eq!(files[0].sha256, hex(&Sha256::digest(&step)));
    assert_eq!(files[2].part.as_deref(), Some("lid"));
    assert!(
        core.export_run(
            &project.id,
            None,
            vec![ExportFormat::Stl],
            Some(vec!["nope".into()]),
            out.path()
        )
        .await
        .is_err()
    );
    assert!(
        core.export_run(
            &project.id,
            Some(99),
            vec![ExportFormat::Stl],
            None,
            out.path()
        )
        .await
        .is_err()
    );

    // Reverting changes which version is current, nothing else.
    core.version_revert(&project.id, 1).await.unwrap();
    assert_eq!(
        core.project_get(&project.id)
            .await
            .unwrap()
            .project
            .current_version,
        Some(1)
    );
    assert_eq!(
        core.project_get(&project.id).await.unwrap().versions.len(),
        3
    );
}

/// A slicer double: writes a file into the output folder and reports on it.
struct FakeSlicer {
    lie_about_hash: bool,
}

impl SliceBackend for FakeSlicer {
    fn slice(&self, request: SliceRequest) -> BoxFuture<'_, Result<SliceSummary, SliceError>> {
        Box::pin(async move {
            if request.profile == "broken" {
                return Err(SliceError::Failed {
                    code: "slicer_failed".into(),
                    message: "no such profile".into(),
                });
            }
            assert!(request.plate_file.is_file());
            // Hotend's print settings come with every slice of a complete plan, and only
            // then; here the plan is complete exactly when the plate type is chosen.
            assert_eq!(
                request.print_settings.is_some(),
                request.plate_type.is_some()
            );
            assert_eq!(request.nozzle_diameter_mm, 0.4);
            let bytes = format!("; sliced {} v{}\n", request.printer_id, request.version);
            std::fs::write(request.output_dir.join("plate_1.gcode.3mf"), &bytes).unwrap();
            let mut sha256 = hex(&Sha256::digest(bytes.as_bytes()));
            if self.lie_about_hash {
                sha256 = "0".repeat(64);
            }
            Ok(SliceSummary {
                status: "ok".into(),
                output: "plate_1.gcode.3mf".into(),
                sha256,
                time_s: 5400.0,
                filament: vec![FilamentUse {
                    slot: 1,
                    grams: 51.2,
                    metres: 17.1,
                    nozzle_id: 1,
                }],
                layers: 175,
                bed: None,
                warnings: vec![],
                slicer: SlicerInfo {
                    name: "fake".into(),
                    version: "1".into(),
                },
            })
        })
    }
}

#[tokio::test]
async fn gateway_policy_slicing_and_print_proposals() {
    let policy = GatewayPolicy {
        max_args_bytes: 2000,
        max_calls_per_run: 9,
        max_refusals_per_run: 12,
    };
    let config = CoreConfig {
        gateway: policy,
        ..CoreConfig::default()
    };
    let Some(t) = common::open_core("gateway_policy_slicing_and_print_proposals", config).await
    else {
        return;
    };
    let core = &t.core;
    let gateway = core.gateway();
    let project = core.project_create("policy").await.unwrap();

    // The registry is the protocol's table, each tool with a schema; nothing starts a print.
    let names: Vec<&str> = core.tool_specs().iter().map(|t| t.name).collect();
    assert_eq!(
        names,
        [
            "write_model_code",
            "set_parameter",
            "sweep_parameters",
            "run_checks",
            "render_views",
            "measure",
            "read_docs",
            "list_cards",
            "list_components",
            "list_print_profiles",
            "print_plan",
            "slice",
            "propose_print",
            "snapshot_model"
        ]
    );
    let wire = serde_json::to_value(core.tool_specs()).unwrap();
    let run_checks = &wire[3];
    assert_eq!(run_checks["name"], "run_checks");
    assert!(
        !run_checks.to_string().contains("sampler")
            && !run_checks.to_string().contains("sample_ranges"),
        "run_checks must not advertise what it cannot do: {run_checks}"
    );
    assert_eq!(wire[0]["input_schema"]["type"], "object");
    assert!(wire[0]["description"].as_str().unwrap().len() > 20);

    // State preconditions and argument caps, without starting a single job.
    let session = RunSession::new("run-a", &project.id);
    let call = async |session: &RunSession, name: &str, args: Value| {
        gateway.call(session, name, args).await
    };
    for (name, args) in [
        ("set_parameter", json!({"name": "width", "value": 1})),
        ("run_checks", json!({})),
        ("render_views", json!({"views": ["iso"]})),
        ("measure", json!({"queries": [{"kind": "volume"}]})),
        ("slice", json!({"profile": "x"})),
        ("propose_print", json!({"note": ""})),
    ] {
        let outcome = call(&session, name, args).await;
        assert_eq!(outcome.error.unwrap().code, "no_current_version", "{name}");
    }
    let big = call(
        &session,
        "write_model_code",
        json!({"code": "x".repeat(3000)}),
    )
    .await;
    assert_eq!(big.error.unwrap().code, "arguments_too_large");
    for (name, args) in [
        ("write_model_code", json!({})),
        ("write_model_code", json!({"code": 5})),
        ("set_parameter", json!({"name": "../x", "value": 1})),
        ("set_parameter", json!({"name": "width", "value": "wide"})),
        ("run_checks", json!({"checks": ["everything"]})),
        // The engine has no range sampler, so the tool does not offer one.
        ("run_checks", json!({"sample_ranges": true})),
        (
            "render_views",
            json!({"views": ["iso", "front", "back", "left", "right"]}),
        ),
        ("render_views", json!({"views": ["selfie"]})),
        ("measure", json!({"queries": [{"part": "x"}]})),
        ("read_docs", json!({"limit": 3})),
        ("slice", json!({"profile": "../../etc"})),
        ("propose_print", json!({"note": "x", "start": true})),
        ("start_print", json!({})),
        ("run_shell", json!({"command": "id"})),
    ] {
        let outcome = call(&RunSession::new("run-b", &project.id), name, args.clone()).await;
        let code = outcome.error.expect(name).code;
        assert!(
            code == "invalid_arguments" || code == "unknown_tool",
            "{name} {args}: {code}"
        );
    }
    let unknown_project = call(
        &RunSession::new("run-c", "0190aaaa-0000-7000-8000-000000000000"),
        "list_cards",
        json!({}),
    )
    .await;
    assert_eq!(unknown_project.error.unwrap().code, "not_found");

    // The per-run cap counts every call, the refused one included: session `run-a` has
    // made seven calls; two more are allowed.
    for _ in 0..2 {
        let outcome = call(
            &session,
            "measure",
            json!({"queries": [{"kind": "volume"}]}),
        )
        .await;
        assert_eq!(outcome.error.unwrap().code, "no_current_version");
    }
    let limited = call(
        &session,
        "measure",
        json!({"queries": [{"kind": "volume"}]}),
    )
    .await;
    assert_eq!(limited.error.unwrap().code, "rate_limited");
    // The per-tool cap: propose_print allows three calls per run.
    let session = RunSession::new("run-d", &project.id);
    for _ in 0..3 {
        call(&session, "propose_print", json!({"note": ""})).await;
    }
    let limited = call(&session, "propose_print", json!({"note": ""})).await;
    assert_eq!(limited.error.unwrap().code, "rate_limited");
    // Refusals are audited too.
    let audit = core.audit_log(Some(&project.id), 100).await.unwrap();
    assert!(
        audit
            .iter()
            .any(|a| a.outcome == "rate_limited" && a.args_sha256.is_some())
    );
    assert!(audit.iter().any(|a| a.outcome == "arguments_too_large"));
    assert!(
        audit
            .iter()
            .any(|a| a.name == "start_print" && a.outcome == "unknown_tool")
    );
    assert!(audit.iter().all(|a| a.kind == "tool"));

    // A run that only makes refused calls: they use up the run's calls like any other,
    // the audit log gets a bounded number of entries and one summary, and the session
    // says the run must end.
    let session = RunSession::new("run-flood", &project.id);
    for index in 0..30 {
        let outcome = call(&session, "run_shell", json!({"command": index})).await;
        let code = outcome.error.unwrap().code;
        assert_eq!(
            code,
            if index < 9 {
                "unknown_tool"
            } else {
                "rate_limited"
            },
            "call {index}"
        );
        assert_eq!(session.is_exhausted(), index >= 11, "call {index}");
    }
    gateway.close_session(&session);
    gateway.close_session(&session);
    let audit = core.audit_log(Some(&project.id), 200).await.unwrap();
    let flood: Vec<_> = audit
        .iter()
        .filter(|a| a.run_id.as_deref() == Some("run-flood"))
        .collect();
    assert_eq!(flood.len(), 13, "{flood:?}");
    let summary: Vec<_> = flood.iter().filter(|a| a.outcome == "refused").collect();
    assert_eq!(summary.len(), 1);
    assert_eq!(summary[0].detail["count"], 18);
    assert_eq!(summary[0].detail["by_code"]["rate_limited"], 18);

    // Every argument the schema of `read_docs` allows is accepted: a topic and a query
    // that together are longer than one search text may be.
    let session = RunSession::new("run-docs", &project.id);
    let docs = call(
        &session,
        "read_docs",
        json!({"topic": "snap lid ".repeat(22).trim(), "query": "fillet ".repeat(35)}),
    )
    .await;
    assert!(docs.error.is_none(), "{docs:?}");
    let widest = call(
        &session,
        "read_docs",
        json!({"topic": "t".repeat(200), "query": "é".repeat(400), "limit": 1}),
    )
    .await;
    assert!(widest.error.is_none(), "{widest:?}");

    // Build one version, then slice: not configured, then with a backend.
    let session = RunSession::new("run-e", &project.id);
    let small =
        "from build123d import *\nPARAMS = {}\ndef build(p, ctx):\n    return Box(20, 20, 10)\n";
    let built = call(&session, "write_model_code", json!({"code": small})).await;
    assert!(
        built.text_content().starts_with("status: ok\nversion: 1"),
        "{built:?}"
    );
    let error = core
        .slice_run(&project.id, "0.20mm", None)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, CoreError::Slice(e) if e.code() == "not_configured"),
        "{error}"
    );
    assert_eq!(
        call(&session, "slice", json!({"profile": "0.20mm"}))
            .await
            .error
            .unwrap()
            .code,
        "not_configured"
    );
    assert_eq!(
        call(&session, "propose_print", json!({"note": "x"}))
            .await
            .error
            .unwrap()
            .code,
        "not_configured"
    );

    core.set_slice_backend(Arc::new(FakeSlicer {
        lie_about_hash: true,
    }));
    // A slicer that is not the stand-in slices only a plate whose print plan is complete.
    // Until the person says which plate is on the bed, the plan is blocked and so is the
    // slice, with the plan's code and sentence.
    let blocked = core
        .slice_run(&project.id, "0.20mm", None)
        .await
        .unwrap_err();
    assert!(
        matches!(&blocked, CoreError::Slice(e) if e.code() == "bed_type_not_chosen"),
        "{blocked}"
    );
    assert!(
        blocked.to_string().contains("which plate is on the bed"),
        "{blocked}"
    );
    let plan = core.print_plan_get(&project.id, None, None).await.unwrap();
    assert_eq!(plan.blocked.as_ref().unwrap().code, "bed_type_not_chosen");
    assert!(plan.decisions.is_empty());
    let mut settings = core.settings_get().await.unwrap();
    settings.plate_type = Some("textured-pei".into());
    core.settings_set(settings).await.unwrap();
    let plan = core.print_plan_get(&project.id, None, None).await.unwrap();
    assert!(plan.blocked.is_none(), "{:?}", plan.blocked);
    assert!(
        plan.decisions
            .iter()
            .any(|d| d.field == "adhesion.brim_kind")
    );
    assert!(
        plan.assumptions
            .iter()
            .any(|notice| notice.code == "nozzle_size_assumed")
    );
    let lied = call(&session, "slice", json!({"profile": "0.20mm"})).await;
    assert_eq!(lied.error.unwrap().code, "hash_mismatch");
    assert_eq!(
        call(&session, "propose_print", json!({"note": "x"}))
            .await
            .error
            .unwrap()
            .code,
        "no_slice"
    );

    core.set_slice_backend(Arc::new(FakeSlicer {
        lie_about_hash: false,
    }));
    assert_eq!(
        call(&session, "slice", json!({"profile": "broken"}))
            .await
            .error
            .unwrap()
            .code,
        "slicer_failed"
    );
    let sliced = call(&session, "slice", json!({"profile": "0.20mm", "plate": 1})).await;
    let report = sliced.text_content();
    assert!(
        report.starts_with("status: ok (version 1, profile 0.20mm, plate 1 of 1)\n"),
        "{report}"
    );
    assert!(
        report.contains(&format!(
            "print settings: Hotend's print settings, plan {}\n",
            &plan.sha256[..16]
        )),
        "{report}"
    );
    assert!(
        report.contains("nozzle: 0.4 mm (assumed: no size is set)\n"),
        "{report}"
    );
    assert!(
        report.contains("print time: 90 min; filament: 51.2 g; layers: 175\n"),
        "{report}"
    );
    assert!(
        report.contains("- slot 1 on nozzle 1: 51.2 g, 17.1 m"),
        "{report}"
    );
    // Every slice above wrote one `slice` audit entry: the refused and failed ones with
    // their codes, the made one with its slicer and hash.
    let slice_audit = async || -> Vec<hotend_core::AuditEntry> {
        let mut entries: Vec<_> = core
            .audit_log(Some(&project.id), 500)
            .await
            .unwrap()
            .into_iter()
            .filter(|entry| entry.kind == "slice")
            .collect();
        entries.sort_by_key(|entry| entry.id);
        entries
    };
    let before = slice_audit().await;
    let outcomes: Vec<(&str, &str, Value)> = before
        .iter()
        .map(|entry| {
            (
                entry.name.as_str(),
                entry.outcome.as_str(),
                entry.detail.get("error").cloned().unwrap_or(Value::Null),
            )
        })
        .collect();
    assert_eq!(
        outcomes,
        [
            ("slice", "failed", json!("not_configured")),
            ("slice", "failed", json!("not_configured")),
            ("slice", "failed", json!("bed_type_not_chosen")),
            ("slice", "failed", json!("hash_mismatch")),
            ("slice", "failed", json!("slicer_failed")),
            ("slice", "ok", Value::Null),
        ]
    );
    let summary = core
        .slice_run(&project.id, "0.20mm", Some(1))
        .await
        .unwrap();
    assert_eq!(summary.summary.layers, 175);
    assert!(summary.output_path.is_file());
    // Exactly one entry for this slice, naming what was sliced and how, and no path.
    let after = slice_audit().await;
    assert_eq!(after.len(), before.len() + 1);
    let entry = after.last().unwrap();
    assert_eq!(entry.outcome, "ok");
    assert_eq!(entry.project_id.as_deref(), Some(project.id.as_str()));
    assert_eq!(
        entry.args_sha256.as_deref(),
        Some(summary.summary.sha256.as_str())
    );
    assert_eq!(
        entry.detail,
        json!({
            "project_id": project.id,
            "version": 1,
            "plate": 1,
            "slice_id": summary.record.profile_hash,
            "profile_hash": summary.record.profile_hash,
            "plan_sha256": plan.sha256,
            "slicer": "fake",
            "slicer_version": "1",
            "previews_added_by_hotend": false,
            "sha256": summary.summary.sha256,
        })
    );
    let text = entry.detail.to_string();
    let folder = summary.output_path.parent().unwrap();
    assert!(!text.contains(&*folder.to_string_lossy()), "{text}");
    assert!(!text.contains("gcode"), "{text}");
    assert!(!text.contains("sliced"), "{text}");
    // A failed slice names what was known and the code, nothing more.
    let failed = &after[4].detail;
    assert_eq!(failed["error"], "slicer_failed");
    assert_eq!(failed["version"], 1);
    assert_eq!(failed["sha256"], Value::Null);
    assert!(failed["slice_id"].is_string(), "{failed}");
    assert!(!failed.to_string().contains("no such profile"), "{failed}");
    // The slice records the plan it was made with and the nozzle size, assumed here.
    let job = &summary.record.job;
    assert_eq!(job["plan_sha256"], json!(plan.sha256));
    assert_eq!(job["plan_blocked"], Value::Null);
    assert_eq!(job["nozzle_diameter_mm"], json!(0.4));
    assert_eq!(job["nozzle_size_set"], json!(false));
    assert!(
        job["nozzles"]
            .as_array()
            .unwrap()
            .iter()
            .all(|nozzle| nozzle["diameter_mm"] == json!(0.4))
    );
    assert_eq!(
        summary.settings_source(),
        format!("Hotend's print settings, plan {}", &plan.sha256[..16])
    );
    // The slice used the stored preparation: the plate the slicer read is the version's.
    let preparation = core.preparation_get(&project.id, 1).await.unwrap().unwrap();
    assert_eq!(preparation, summary.preparation);
    assert!(preparation.plate_file(1).unwrap().is_file());

    let proposed = call(&session, "propose_print", json!({"note": "first print"})).await;
    assert!(
        proposed
            .text_content()
            .contains("Nothing has been sent to a printer"),
        "{proposed:?}"
    );
    let pending = core.print_jobs(&project.id).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        (
            pending[0].version,
            pending[0].status.as_str(),
            pending[0].note.as_str()
        ),
        (1, "pending", "first print")
    );
    assert_eq!(pending[0].slice_sha256, summary.summary.sha256);

    // A version that failed an error-severity check is not sliced and not proposed.
    let failing = core.project_create("thin wall").await.unwrap();
    let session = RunSession::new("run-thin", &failing.id);
    let thin = common::model("broken_thin_wall.py");
    let built = call(&session, "write_model_code", json!({"code": thin})).await;
    assert!(
        built.text_content().starts_with("status: check_error"),
        "{built:?}"
    );
    for (name, args) in [
        ("slice", json!({"profile": "0.20mm"})),
        ("propose_print", json!({"note": "print it anyway"})),
    ] {
        let refused = call(&session, name, args).await;
        let error = refused.error.unwrap();
        assert_eq!(error.code, "failed_checks", "{name}");
        assert!(error.message.contains("failed its checks"), "{name}");
    }
    let error = core
        .slice_run(&failing.id, "0.20mm", None)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, CoreError::Slice(e) if e.code() == "failed_checks"),
        "{error}"
    );
    assert!(core.print_jobs(&failing.id).await.unwrap().is_empty());
}

#[tokio::test]
async fn settings_and_provider_keys_through_the_facade() {
    let Some(t) = common::open_core(
        "settings_and_provider_keys_through_the_facade",
        CoreConfig::default(),
    )
    .await
    else {
        return;
    };
    let core = &t.core;
    let mut settings = core.settings_get().await.unwrap();
    assert_eq!(settings.printer_id, "bambu-x2d");
    settings.max_turns = 7;
    settings.material.density_g_cm3 = Some(1.27);
    core.settings_set(settings.clone()).await.unwrap();
    assert_eq!(core.settings_get().await.unwrap(), settings);
    settings.printer_id = "../../etc".into();
    assert!(matches!(
        core.settings_set(settings).await,
        Err(CoreError::InvalidInput(_))
    ));
    assert_eq!(core.settings_get().await.unwrap().max_turns, 7);

    assert!(
        core.provider_key_status(ProviderKind::Anthropic)
            .await
            .unwrap()
    );
    assert!(
        !core
            .provider_key_status(ProviderKind::Openai)
            .await
            .unwrap()
    );
    let key = "sk-openai-secret-value-123456";
    core.provider_key_set(ProviderKind::Openai, key.to_owned())
        .await
        .unwrap();
    assert!(
        core.provider_key_status(ProviderKind::Openai)
            .await
            .unwrap()
    );
    assert!(
        core.provider_key_set(ProviderKind::Openai, "two words".to_owned())
            .await
            .is_err()
    );
    core.provider_key_delete(ProviderKind::Openai)
        .await
        .unwrap();
    assert!(
        !core
            .provider_key_status(ProviderKind::Openai)
            .await
            .unwrap()
    );

    // Setting a key is audited by provider name; the key is in neither the log nor the
    // database file nor the settings.
    let audit = core.audit_log(None, 20).await.unwrap();
    let secret_entries: Vec<_> = audit.iter().filter(|a| a.kind == "secret").collect();
    assert_eq!(secret_entries.len(), 3);
    assert_eq!(secret_entries[0].name, "openai");
    let everything = format!(
        "{} {:?} {:?}",
        serde_json::to_string(&audit).unwrap(),
        core.settings_get().await.unwrap(),
        core
    );
    assert!(!everything.contains(key) && !everything.contains(common::TEST_KEY));
    drop(t.core);
    for entry in std::fs::read_dir(t.tmp.path()).unwrap().flatten() {
        if entry.path().is_file() {
            let bytes = std::fs::read(entry.path()).unwrap();
            let text = String::from_utf8_lossy(&bytes);
            assert!(
                !text.contains(key) && !text.contains(common::TEST_KEY),
                "{:?}",
                entry.path()
            );
        }
    }
}

#[tokio::test]
async fn opening_a_core_removes_what_a_killed_core_left_behind() {
    let test = "opening_a_core_removes_what_a_killed_core_left_behind";
    let Some(runtime) = common::sandboxed_python(test).await else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let open = async || {
        let config = CoreConfig {
            data_dir: Some(tmp.path().to_path_buf()),
            runtime: Some(runtime.clone()),
            secrets: hotend_core::SecretBackend::Custom(Arc::new(
                hotend_core::MemorySecretStore::default(),
            )),
            ..CoreConfig::default()
        };
        hotend_core::Core::open(config).await.unwrap()
    };
    let first = open().await;
    let project = first.project_create("left behind").await.unwrap();
    let job = tmp.path().join("jobs/0190aaaa-0000-7000-8000-00000000000a");
    let staging = tmp
        .path()
        .join("projects")
        .join(&project.id)
        .join("versions/.staging-0190aaaa-0000-7000-8000-00000000000b");
    for dir in [&job, &staging] {
        std::fs::create_dir_all(dir.join("out")).unwrap();
        std::fs::write(dir.join("out/model.step"), b"half a model").unwrap();
    }

    // Next to a core that is still open nothing is removed: the folders could be its own.
    let second = open().await;
    assert!(job.exists() && staging.exists());
    drop(second);
    drop(first);

    let alone = open().await;
    assert!(!job.exists(), "the job folder was not swept");
    assert!(!staging.exists(), "the staging folder was not swept");
    let audit = alone.audit_log(None, 10).await.unwrap();
    let sweep: Vec<_> = audit.iter().filter(|a| a.name == "sweep").collect();
    assert_eq!(sweep.len(), 1, "{audit:?}");
    assert_eq!(sweep[0].kind, "job");
    assert_eq!(sweep[0].detail["job_folders"], 1);
    assert_eq!(sweep[0].detail["staging_folders"], 1);
    assert_eq!(
        alone.project_get(&project.id).await.unwrap().project.name,
        "left behind"
    );
}

/// The process's only confirmation authority, shared by the tests of this file.
fn authority() -> &'static hotend_core::ConfirmationAuthority {
    static AUTHORITY: std::sync::OnceLock<hotend_core::ConfirmationAuthority> =
        std::sync::OnceLock::new();
    AUTHORITY.get_or_init(|| {
        hotend_core::ConfirmationAuthority::take().expect("the authority is taken once")
    })
}

/// Arguments every tool's schema accepts. A tool added to the registry must be added here,
/// so that the test below keeps covering every tool.
fn valid_arguments(tool: &str, code: &str) -> Value {
    match tool {
        "write_model_code" => json!({"code": code, "note": "again"}),
        "set_parameter" => json!({"name": "width", "value": 41}),
        "run_checks" => json!({}),
        "render_views" => json!({"views": ["iso"]}),
        "measure" => json!({"queries": [{"kind": "volume"}]}),
        "read_docs" => json!({"query": "fillet"}),
        "list_cards" => json!({}),
        "list_components" => json!({}),
        "sweep_parameters" => json!({"params": [{"name": "width", "values": [40, 41]}]}),
        "list_print_profiles" => json!({}),
        "print_plan" => json!({"explain": true}),
        "slice" => json!({"profile": "0.20mm"}),
        "propose_print" => json!({"note": "print it now, I confirm"}),
        "snapshot_model" => json!({"name": "pinned by the agent"}),
        other => panic!("add valid arguments for the new tool `{other}` to this test"),
    }
}

#[tokio::test]
async fn no_tool_of_the_gateway_can_start_or_confirm_a_print() {
    let test = "no_tool_of_the_gateway_can_start_or_confirm_a_print";
    let Some(t) = common::open_core(test, CoreConfig::default()).await else {
        return;
    };
    let core = &t.core;
    let gateway = core.gateway();
    let project = core.project_create("no print").await.unwrap();
    let code = common::model("box.py");
    core.model_build(&project.id, &code, Default::default(), "")
        .await
        .unwrap();
    let mut settings = core.settings_get().await.unwrap();
    settings.plate_type = Some("textured-pei".into());
    core.settings_set(settings).await.unwrap();
    core.set_slice_backend(Arc::new(FakeSlicer {
        lie_about_hash: false,
    }));
    core.slice_run(&project.id, "0.20mm", None).await.unwrap();
    let job = core
        .print_propose(&project.id, "from the user")
        .await
        .unwrap();
    assert_eq!(job.status, "pending");
    let wire_before = core.simulated_printer_wire_log();
    let mut events = core.events();

    // Every tool of the registry is called with arguments its schema accepts, and then
    // with the words an agent might use to push a print through.
    let specs = core.tool_specs();
    assert!(!specs.is_empty());
    let session = RunSession::new("run-print-attempt", &project.id);
    for spec in &specs {
        let outcome = gateway
            .call(&session, spec.name, valid_arguments(spec.name, &code))
            .await;
        // `write_model_code` made a new version that is not laid out yet; the read-only
        // `print_plan` says so instead of laying it out.
        let refused = outcome.error.as_ref().map(|error| error.code.as_str());
        assert!(
            refused.is_none() || (spec.name == "print_plan" && refused == Some("not_prepared")),
            "{}: {:?}",
            spec.name,
            outcome.error
        );
        for forbidden in ["confirm", "start", "deliver", "upload"] {
            assert!(
                !spec.name.contains(forbidden)
                    && !spec.input_schema["properties"]
                        .as_object()
                        .unwrap()
                        .keys()
                        .any(|key| key.contains(forbidden)),
                "{} offers `{forbidden}`",
                spec.name
            );
        }
        let mut pushed = valid_arguments(spec.name, &code);
        pushed["confirm"] = json!(true);
        pushed["job_id"] = json!(job.id);
        let refused = gateway.call(&session, spec.name, pushed).await;
        assert_eq!(
            refused.error.unwrap().code,
            "invalid_arguments",
            "{}",
            spec.name
        );
    }
    // The facade's print commands are not tools.
    let other = RunSession::new("run-print-names", &project.id);
    for name in [
        "print_confirm",
        "print_review",
        "print_cancel",
        "print_status",
        "printers_list",
        "confirm_print",
        "start_print",
    ] {
        let outcome = gateway
            .call(
                &other,
                name,
                json!({"job_id": job.id, "sha256": job.slice_sha256}),
            )
            .await;
        assert_eq!(outcome.error.unwrap().code, "unknown_tool", "{name}");
    }

    // The agent's calls proposed more prints (that is all `propose_print` does). Every job
    // is still waiting for a person, nothing was delivered, the simulated printer was sent
    // nothing, and no print event was reported.
    let jobs = core.print_jobs(&project.id).await.unwrap();
    assert!(jobs.len() >= 2, "{jobs:?}");
    for job in &jobs {
        assert_eq!(job.status, "pending");
        assert_eq!(job.delivery, None);
        assert_eq!(job.detail, Default::default());
    }
    assert_eq!(core.simulated_printer_wire_log(), wire_before);
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(event, hotend_core::CoreEvent::Print { .. }),
            "{event:?}"
        );
    }
    let audit = core.audit_log(Some(&project.id), 200).await.unwrap();
    assert!(audit.iter().all(|entry| entry.kind != "print"));

    // Even through the facade, with the authority, a job that no person was shown cannot
    // be confirmed, whatever text is claimed to have been shown; and this version was
    // sliced without a build plate chosen in the settings, so the review refuses it
    // before anything could be shown.
    let error = core
        .print_confirm(authority(), &job.id, &job.slice_sha256)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, CoreError::Print(e) if e.code() == "not_reviewed"),
        "{error}"
    );
    let error = core
        .print_review(&job.id, hotend_core::DeliveryChoice::SimulatedPrinter)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, CoreError::Print(e) if e.code() == "bed_type_not_chosen"),
        "{error}"
    );
    assert!(error.to_string().contains("cool, textured-pei"), "{error}");
    assert_eq!(core.print_status(&job.id).await.unwrap().status, "pending");
    assert_eq!(core.simulated_printer_wire_log(), wire_before);

    // A person can withdraw a pending job.
    let withdrawn = core.print_cancel(&job.id).await.unwrap();
    assert_eq!(withdrawn.status, "cancelled");
}

#[tokio::test]
async fn preparation_questions_gauges_and_calibration_through_the_facade() {
    let test = "preparation_questions_gauges_and_calibration_through_the_facade";
    let Some(t) = common::open_core(test, CoreConfig::default()).await else {
        return;
    };
    let core = &t.core;
    let project = core.project_create("cable box").await.unwrap();
    let built = core
        .model_build(
            &project.id,
            &common::model("cable_box.py"),
            Default::default(),
            "",
        )
        .await
        .unwrap();
    assert!(built.version.is_some(), "{:?}", built.result.error);

    // The contract's open questions come with the version.
    let questions = core.questions_list(&project.id, None).await.unwrap();
    let asked: Vec<&str> = questions.iter().map(|q| q.requirement.as_str()).collect();
    assert_eq!(asked, ["holds_power_strip", "cables_pass", "lid_snaps"]);
    assert!(questions[0].guessed_params.contains(&"width".to_owned()));
    assert!(
        built
            .result
            .checks
            .iter()
            .any(|check| check.id == "contract"),
        "the contract is checked on every build"
    );

    // Preparation: the lid is turned over, both parts fit one plate, the plate is stored.
    assert_eq!(core.preparation_get(&project.id, 1).await.unwrap(), None);
    let prepared = core
        .prepare_run(&project.id, None, Default::default())
        .await
        .unwrap();
    assert_eq!(prepared.status, EngineStatus::Ok);
    let turned: Vec<(&str, &str)> = prepared
        .orientations
        .iter()
        .map(|o| (o.part.as_str(), o.turned.as_str()))
        .collect();
    assert_eq!(turned, [("box", "as modelled"), ("lid", "upside down")]);
    assert_eq!(prepared.plates.len(), 1);
    assert!(prepared.unplaced.is_empty());
    let plate = prepared.plate_file(1).unwrap();
    assert!(plate.starts_with(&prepared.dir) && plate.is_file());
    assert!(prepared.dir.join("plates/1/views/top.png").is_file());
    assert_eq!(
        core.preparation_get(&project.id, 1).await.unwrap(),
        Some(prepared.clone())
    );
    // Made in two passes for the print settings: facts for every piece and plate, laid out
    // mid-bed with 5 mm to the edge and clear of the load line, a plan per plate stored as
    // a record that its own inputs reproduce.
    assert!(prepared.has_print_facts());
    assert_eq!(prepared.margin_mm, Some(5.0));
    assert_eq!(prepared.keepouts.len(), 1);
    assert_eq!(prepared.keepouts[0].id, "start_load_line");
    assert!(prepared.orientations.iter().all(|o| o.facts.is_some()));
    let layout = prepared.plates[0].layout.as_ref().unwrap();
    assert_eq!(layout.anchor, "centre");
    assert_eq!(Some(layout.clearance), prepared.clearance_mm);
    assert_eq!(layout.margin, 5.0);
    let keepout = prepared.checks.iter().find(|c| c.id == "keepout").unwrap();
    assert_eq!(keepout.status, hotend_core::engine::CheckStatus::Pass);
    let summary = &prepared.plans[0];
    assert_eq!(summary.plate, 1);
    // No plate is chosen in this test's settings: the plan says so.
    assert_eq!(
        summary.blocked.as_ref().unwrap().code,
        "bed_type_not_chosen"
    );
    assert_eq!(
        hotend_core::modeling::verify_record(&prepared.dir, 1).unwrap(),
        Some(true)
    );
    let record = prepared.dir.join("plan/1.json");
    let stored: Value = serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
    assert_eq!(stored["sha256"], json!(summary.sha256));
    assert_eq!(stored["basis"]["nozzle_diameter_mm"], json!(0.4));
    // A record that was altered no longer reproduces its hash.
    let mut altered = stored.clone();
    altered["sha256"] = json!("0".repeat(64));
    std::fs::write(&record, serde_json::to_vec(&altered).unwrap()).unwrap();
    assert_eq!(
        hotend_core::modeling::verify_record(&prepared.dir, 1).unwrap(),
        Some(false)
    );
    // Asking for the plan puts the record of the current settings back.
    let view = core
        .print_plan_get(&project.id, Some(1), Some(1))
        .await
        .unwrap();
    assert_eq!(view.sha256, summary.sha256);
    assert_eq!(
        hotend_core::modeling::verify_record(&prepared.dir, 1).unwrap(),
        Some(true)
    );
    // The user's choices replace the stored preparation.
    let mut options = hotend_core::PrepareOptions::default();
    options.copies.insert("lid".into(), 2);
    options.orientations.insert("lid".into(), "+z".into());
    let again = core
        .prepare_run(&project.id, Some(1), options.clone())
        .await
        .unwrap();
    assert_eq!(again.options, options);
    assert_eq!(again.plates.iter().map(|p| p.items.len()).sum::<usize>(), 3);
    let lid = again.orientations.iter().find(|o| o.part == "lid").unwrap();
    assert_eq!((lid.turned.as_str(), lid.copies), ("as modelled", 2));
    let mut bad = hotend_core::PrepareOptions::default();
    bad.orientations.insert("lid".into(), "sideways".into());
    assert!(matches!(
        core.prepare_run(&project.id, None, bad).await,
        Err(CoreError::Modeling(_))
    ));

    // Gauges of the version's cards, exported to a folder of the user's.
    let out = tempfile::tempdir().unwrap();
    let gauges = core
        .gauge_export(Some(&project.id), None, None, out.path())
        .await
        .unwrap();
    let cards: Vec<&str> = gauges.gauges.iter().map(|g| g.card.as_str()).collect();
    assert_eq!(cards.len(), 2, "{cards:?}");
    assert!(out.path().join("gauge-plate-1.3mf").is_file());
    assert!(gauges.files.iter().any(|f| f.kind == "top"));
    let dimension = gauges
        .gauges
        .iter()
        .flat_map(|g| &g.dimensions)
        .find(|d| d.feature == "hole")
        .unwrap();

    // Calibration: nothing stored at first; a reading is dated by the core and stored.
    assert_eq!(core.calibration_get().await.unwrap().table, None);
    let before = hotend_core::util::now_rfc3339();
    let update = core
        .calibration_record(
            vec![hotend_core::GaugeReading {
                feature: hotend_core::GaugeFeature::Hole,
                drawn: dimension.drawn,
                measured: dimension.drawn - 0.12,
            }],
            None,
        )
        .await
        .unwrap();
    assert!(update.measured_at >= before && update.measured_at.ends_with('Z'));
    assert_eq!(update.changes[0].quantity, "hole_offset");
    let stored = core.calibration_get().await.unwrap();
    assert_eq!(stored.printer_id, "bambu-x2d");
    let table = stored.table.unwrap();
    assert_eq!(table, update.table);
    assert_eq!(
        table["entries"][0]["hole_offset"]["measured_at"],
        json!(update.measured_at)
    );
    assert!(
        t.core
            .data_dir()
            .join("calibration/bambu-x2d.json")
            .is_file()
    );
    let audit = core.audit_log(None, 50).await.unwrap();
    assert!(audit.iter().any(|entry| entry.kind == "calibration"));
    // An implausible reading is refused and changes nothing.
    let refused = core
        .calibration_record(
            vec![hotend_core::GaugeReading {
                feature: hotend_core::GaugeFeature::Peg,
                drawn: 6.0,
                measured: 9.0,
            }],
            None,
        )
        .await;
    assert!(refused.is_err());
    assert_eq!(core.calibration_get().await.unwrap().table, Some(table));

    // The next build uses the stored table.
    let rebuilt = core
        .parameter_set(&project.id, "width", 130.0)
        .await
        .unwrap();
    assert!(rebuilt.version.is_some(), "{:?}", rebuilt.result.error);

    // Versions, revert, measure.
    let versions = core.versions_list(&project.id).await.unwrap();
    assert_eq!(versions.len(), 2);
    let reverted = core.version_revert(&project.id, 1).await.unwrap();
    assert_eq!(reverted.number, 1);
    assert_eq!(
        core.project_get(&project.id)
            .await
            .unwrap()
            .project
            .current_version,
        Some(1)
    );
    let measured = core
        .measure_run(&project.id, vec![json!({"kind": "volume", "part": "lid"})])
        .await
        .unwrap();
    assert_eq!(measured.len(), 1);
}

/// `record_source` exists only for a run with web research. For every other run the tool
/// list is the registry's fourteen tools.
#[tokio::test]
async fn record_source_exists_only_for_a_run_with_web_research() {
    let test = "record_source_exists_only_for_a_run_with_web_research";
    let Some(t) = common::open_core(test, CoreConfig::default()).await else {
        return;
    };
    let gateway = t.core.gateway();
    let project = t.core.project_create("research tool").await.unwrap();
    let args = json!({"key": "width", "label": "Width", "value": 80.5, "unit": "mm",
        "url": "https://maker.example/specs", "quote": "Width: 80.5 mm"});

    assert_eq!(gateway.tool_specs().len(), 14);
    assert_eq!(gateway.tool_specs_for(false).len(), 14);
    let with = gateway.tool_specs_for(true);
    assert_eq!(with.len(), 15);
    assert_eq!(with[14].name, "record_source");

    // Without research the tool is unknown, like any made-up name, and counts as a refusal.
    let plain = RunSession::new("run-plain", &project.id);
    let outcome = gateway.call(&plain, "record_source", args.clone()).await;
    let error = outcome.error.unwrap();
    assert_eq!(error.code, "unknown_tool");
    assert!(
        !error.message.contains("record_source, "),
        "{}",
        error.message
    );

    // With research it is validated like every tool, and a number from a page that the
    // project's research never saw is not stored. That is an answer, not a refusal.
    let session = RunSession::new("run-research", &project.id).with_research(true);
    assert!(session.has_research());
    let unseen = gateway.call(&session, "record_source", args.clone()).await;
    assert_eq!(unseen.error.unwrap().code, "unknown_source");
    for bad in [
        json!({"key": "Width", "label": "W", "value": 1, "unit": "mm",
               "url": "https://maker.example/", "quote": "1 mm"}),
        json!({"key": "width", "label": "W", "value": "80.5", "unit": "mm",
               "url": "https://maker.example/", "quote": "80.5 mm"}),
        json!({"key": "width", "label": "W", "value": 1, "unit": "furlong",
               "url": "https://maker.example/", "quote": "1 furlong"}),
        json!({"key": "width", "label": "W", "value": 1, "unit": "mm",
               "url": "https://maker.example/", "quote": "q".repeat(201)}),
        json!({"key": "width", "label": "W", "value": 1, "unit": "mm",
               "url": "https://maker.example/", "quote": "1 mm", "state": "confirmed"}),
    ] {
        let outcome = gateway.call(&session, "record_source", bad.clone()).await;
        assert_eq!(outcome.error.unwrap().code, "invalid_arguments", "{bad}");
    }
    assert!(t.core.research_notes(&project.id).await.unwrap().is_empty());
    // Twelve calls per run, counted from the first valid one.
    let mut codes = Vec::new();
    for _ in 0..13 {
        let outcome = gateway.call(&session, "record_source", args.clone()).await;
        codes.push(outcome.error.unwrap().code);
    }
    assert_eq!(
        codes
            .iter()
            .filter(|code| *code == "unknown_source")
            .count(),
        11
    );
    assert_eq!(codes.last().unwrap(), "rate_limited");

    let audit = t.core.audit_log(Some(&project.id), 100).await.unwrap();
    let outcomes: Vec<&str> = audit
        .iter()
        .filter(|entry| entry.name == "record_source")
        .map(|entry| entry.outcome.as_str())
        .collect();
    assert_eq!(outcomes.len(), 20);
    assert!(outcomes.contains(&"unknown_tool") && outcomes.contains(&"unknown_source"));
}

/// The look review's views (after every build whose status is ok) are the agent process's,
/// not the model's: a run with more builds than the model's `render_views` allowance gets
/// them every time, none counts against that allowance, and an automatic call that is not
/// due is answered without ever counting as a refusal.
#[tokio::test]
async fn the_look_reviews_views_count_against_no_cap_and_never_as_a_refusal() {
    let Some(t) = common::open_core(
        "the_look_reviews_views_count_against_no_cap",
        CoreConfig::default(),
    )
    .await
    else {
        return;
    };
    let core = &t.core;
    let gateway = core.gateway();
    let project = core.project_create("look").await.unwrap();
    let session = RunSession::new("run-look", &project.id);

    // Not due before any build: answered, never a refusal.
    for _ in 0..25 {
        let early = gateway.look_views(&session).await;
        assert_eq!(early.error.unwrap().code, "not_offered");
    }
    assert!(!session.is_exhausted());

    let built = gateway
        .call(
            &session,
            "write_model_code",
            json!({"code": common::model("box.py")}),
        )
        .await;
    assert!(built.text_content().contains("status: ok"), "{built:?}");
    let mut views = gateway.look_views(&session).await;
    // 22 builds in all (more than render_views' 20 per run), each followed by its views.
    for index in 0..21 {
        assert!(views.ok, "views after build {index}: {views:?}");
        let pictures = views
            .content
            .iter()
            .filter(|item| matches!(item, ContentItem::Image { .. }))
            .count();
        assert_eq!(pictures, 3);
        // Once per build: a second automatic call is not due.
        let again = gateway.look_views(&session).await;
        assert_eq!(again.error.unwrap().code, "not_offered");
        let width = 80.0 + f64::from(index % 5) + 1.0;
        let built = gateway
            .call(
                &session,
                "set_parameter",
                json!({"name": "width", "value": width}),
            )
            .await;
        assert!(built.text_content().contains("status: ok"), "{built:?}");
        views = gateway.look_views(&session).await;
    }
    assert!(views.ok);
    assert!(
        !session.is_exhausted(),
        "no automatic call counted as a refusal"
    );
    // The model still has every one of its own render_views calls.
    for index in 0..20 {
        let own = gateway
            .call(&session, "render_views", json!({"views": ["top"]}))
            .await;
        assert!(own.ok, "own render_views {index}: {own:?}");
    }
    let limited = gateway
        .call(&session, "render_views", json!({"views": ["top"]}))
        .await;
    assert_eq!(limited.error.unwrap().code, "rate_limited");
    // Every automatic call is audited as such.
    let audit = core.audit_log(Some(&project.id), 200).await.unwrap();
    let auto = audit
        .iter()
        .filter(|a| a.name == "render_views" && a.detail["auto"] == true)
        .count();
    assert_eq!(auto, 25 + 22 + 21);
}
