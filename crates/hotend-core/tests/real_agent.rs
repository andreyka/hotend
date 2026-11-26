//! The real agent process under the real supervisor: `python -I -m hotend_agent` in the
//! agent sandbox, driven by `Core::message_send`, calling the real gateway, which builds
//! real models in the sandboxed engine.
//!
//! No test makes a model request: the agent runs in scripted mode (`--script`), which
//! replays the cable box demo conversation shipped in `python/hotend_agent/scripts/`.
//! Like the other integration tests these need `python/.venv` and a usable sandbox.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use hotend_core::{
    AgentCommand, Core, CoreConfig, MemorySecretStore, ProviderKind, ProviderSettings,
    PythonRuntime, RunEvent, Secret, SecretBackend, SecretStore, Settings, StopReason,
    VersionStatus,
};
use serde_json::Value;

const DEMO_SCRIPT: &str = "builtin:cable_box_demo";

/// What the agent says in each turn of the shipped script (`hotend_agent/scripts`). The
/// wording is the script's to choose; the tests here hold the transcript to it.
fn demo_turns_said() -> Vec<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../python/hotend_agent/scripts/cable_box_demo.json");
    let script: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    script["turns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|turn| turn["say"].as_str().unwrap().trim().to_owned())
        .collect()
}

/// The real agent in scripted mode. It needs nothing readable beyond the Python runtime.
fn scripted_agent(runtime: &PythonRuntime) -> AgentCommand {
    let mut command = AgentCommand::python_module(runtime);
    command
        .args
        .extend(["--script".to_owned(), DEMO_SCRIPT.to_owned()]);
    command
}

struct Fixture {
    core: Core,
    project_id: String,
    tmp: tempfile::TempDir,
}

async fn fixture(test: &str, max_turns: u32) -> Option<Fixture> {
    let runtime = common::sandboxed_python(test).await?;
    let tmp = tempfile::tempdir().unwrap();
    let secrets = Arc::new(MemorySecretStore::default());
    let config = CoreConfig {
        data_dir: Some(tmp.path().to_path_buf()),
        agent: Some(scripted_agent(&runtime)),
        runtime: Some(runtime),
        secrets: SecretBackend::Custom(secrets.clone()),
        ..CoreConfig::default()
    };
    let core = Core::open(config).await.unwrap();
    // A run cannot start without a key, although the scripted agent never asks for a model.
    secrets
        .set("provider-key.anthropic", Secret::new(common::TEST_KEY))
        .unwrap();
    core.settings_set(Settings {
        provider: Some(ProviderSettings {
            kind: ProviderKind::Anthropic,
            model: "scripted".into(),
        }),
        max_turns,
        ..Settings::default()
    })
    .await
    .unwrap();
    let project_id = core.project_create("Cable box").await.unwrap().id;
    Some(Fixture {
        core,
        project_id,
        tmp,
    })
}

fn agent_events(events: &[RunEvent]) -> Vec<&Value> {
    events
        .iter()
        .filter_map(|event| match event {
            RunEvent::Agent { event } => Some(event),
            _ => None,
        })
        .collect()
}

fn of_type<'a>(events: &[&'a Value], kind: &str) -> Vec<&'a Value> {
    events
        .iter()
        .copied()
        .filter(|event| event["type"] == kind)
        .collect()
}

#[tokio::test]
async fn the_real_agent_plays_the_cable_box_demo_through_the_gateway() {
    let test = "the_real_agent_plays_the_cable_box_demo_through_the_gateway";
    let Some(f) = fixture(test, 24).await else {
        return;
    };
    let run = f
        .core
        .message_send(
            &f.project_id,
            "A box that hides a power strip on my desk, with a lid that snaps on.",
        )
        .await
        .unwrap();
    let events = common::collect(run).await;
    assert_eq!(
        events.last().unwrap(),
        &RunEvent::Finished {
            stop_reason: StopReason::Done,
            error: None
        },
        "{events:?}"
    );

    // The tool calls the UI saw: a failing version, the fix, then the views.
    let agent = agent_events(&events);
    let ends = of_type(&agent, "tool_end");
    let calls: Vec<(&str, bool, &str)> = ends
        .iter()
        .map(|e| {
            (
                e["name"].as_str().unwrap(),
                e["ok"].as_bool().unwrap(),
                e["summary"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        calls,
        [
            ("list_cards", true, calls[0].2),
            ("write_model_code", true, "status: check_error"),
            ("write_model_code", true, "status: ok"),
            ("render_views", true, "views of version 2: iso"),
            (
                "render_views",
                true,
                "views of version 2: front (section XZ at 0 mm)"
            ),
        ],
        "{agent:?}"
    );
    assert_eq!(of_type(&agent, "tool_start").len(), 5);
    assert_eq!(
        ends[3]["images"], 1,
        "the view reached the agent as an image"
    );
    assert_eq!(ends[4]["images"], 1);
    assert!(!of_type(&agent, "usage").is_empty());

    // The engine really built both versions: the planted thin wall, then the fix.
    let detail = f.core.project_get(&f.project_id).await.unwrap();
    let statuses: Vec<VersionStatus> = detail.versions.iter().map(|v| v.status).collect();
    assert_eq!(
        statuses,
        [VersionStatus::CheckError, VersionStatus::Ok],
        "{:?}",
        detail.versions
    );
    assert_eq!(detail.versions[0].params["wall"], 1.0);
    assert_eq!(detail.versions[1].params["wall"], 2.0);
    assert_eq!(detail.project.current_version, Some(2));

    // The transcript is the agent's text, with the turns kept apart: every turn of the
    // shipped script, whatever it says, in order and whole.
    let said = &detail.transcript.last().unwrap().text;
    let turns = demo_turns_said();
    assert!(turns.len() >= 2, "{turns:?}");
    let mut at = 0;
    for (number, turn) in turns.iter().enumerate() {
        match said[at..].find(turn.as_str()) {
            Some(found) => {
                if number > 0 {
                    assert!(
                        said[at..at + found].contains("\n\n"),
                        "turn {number} runs into the one before it: {said}"
                    );
                }
                at += found + turn.len();
            }
            None => panic!("turn {number} is missing or out of order in: {said}"),
        }
    }
    assert_eq!(said[at..].trim_end(), "", "{said}");

    // The stored history is the agent's own format and holds the pictures as they were
    // sent, so the next message only appends to what the provider cached.
    let store = hotend_core::store::Store::open(&f.tmp.path().join("hotend.db")).unwrap();
    let (kind, history) = store.history(&f.project_id).unwrap().unwrap();
    assert_eq!(kind, "anthropic");
    let text = serde_json::to_string(&history).unwrap();
    assert!(
        text.contains("FAIL min_wall [box]"),
        "the failed check was fed back"
    );
    assert!(
        text.contains("\"identifier\":\"views/iso.png\"") && text.contains("\"kind\":\"binary\""),
        "images are kept as they were sent"
    );
    assert!(!text.contains("shown to the model when it was made"));
    assert!(text.len() < 2_000_000, "{} bytes of history", text.len());

    // A second message continues the same conversation in a new agent process.
    let run = f.core.message_send(&f.project_id, "Thanks!").await.unwrap();
    let events = common::collect(run).await;
    assert_eq!(
        events.last().unwrap(),
        &RunEvent::Finished {
            stop_reason: StopReason::Done,
            error: None
        },
        "{events:?}"
    );
    let (_, longer) = store.history(&f.project_id).unwrap().unwrap();
    assert!(longer.len() > history.len());
    assert_eq!(longer[..history.len()], history[..]);
}

#[tokio::test]
async fn the_real_agent_stops_at_the_turn_budget() {
    let test = "the_real_agent_stops_at_the_turn_budget";
    let Some(f) = fixture(test, 2).await else {
        return;
    };
    let run = f
        .core
        .message_send(&f.project_id, "A cable box, please.")
        .await
        .unwrap();
    let events = common::collect(run).await;
    assert_eq!(
        events.last().unwrap(),
        &RunEvent::Finished {
            stop_reason: StopReason::Budget,
            error: None
        },
        "{events:?}"
    );
    // Two model turns: list the cards, write the first version. The work done is kept.
    let detail = f.core.project_get(&f.project_id).await.unwrap();
    assert_eq!(detail.versions.len(), 1);
    let store = hotend_core::store::Store::open(&f.tmp.path().join("hotend.db")).unwrap();
    assert!(store.history(&f.project_id).unwrap().is_some());
}

/// `python/tests/agent` checks the agent against a copy of the tool list; this keeps the copy
/// equal to the registry.
#[test]
fn the_tool_specs_fixture_of_the_agent_tests_matches_the_registry() {
    let registry = serde_json::to_value(hotend_core::gateway::tools::tool_specs()).unwrap();
    if std::env::var_os("HOTEND_PRINT_TOOL_SPECS").is_some() {
        println!("{}", serde_json::to_string_pretty(&registry).unwrap());
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../python/tests/agent/fixtures/tool_specs.json");
    let fixture: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(
        registry, fixture,
        "update python/tests/agent/fixtures/tool_specs.json (run this test with \
         HOTEND_PRINT_TOOL_SPECS=1 and --nocapture to print the registry)"
    );
}

/// The same for the one tool a run with web research gets in addition. The registry of
/// every other run stays the fourteen tools above.
#[test]
fn the_research_tool_fixture_of_the_agent_tests_matches_the_gateway() {
    let spec = serde_json::to_value(hotend_core::gateway::tools::research_tool_spec()).unwrap();
    if std::env::var_os("HOTEND_PRINT_TOOL_SPECS").is_some() {
        println!("{}", serde_json::to_string_pretty(&spec).unwrap());
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../python/tests/agent/fixtures/research_tool_spec.json");
    let fixture: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(
        spec, fixture,
        "update python/tests/agent/fixtures/research_tool_spec.json (run this test with \
         HOTEND_PRINT_TOOL_SPECS=1 and --nocapture to print the tool)"
    );
    // One plain object schema: no combinator at the top, which some providers refuse.
    let schema = spec["input_schema"].as_object().unwrap();
    for combinator in ["oneOf", "allOf", "anyOf"] {
        assert!(!schema.contains_key(combinator), "{combinator}");
    }
    assert_eq!(schema["additionalProperties"], false);
    let names: Vec<String> = hotend_core::gateway::tools::tool_specs()
        .iter()
        .map(|tool| tool.name.to_owned())
        .collect();
    assert_eq!(names.len(), 14);
    assert!(!names.iter().any(|name| name == "record_source"));
}
