//! The agent's read-only tools change nothing: called with arguments their schemas accept,
//! they leave every file of the project (its versions, layouts, plan records and slices) and
//! what the store says about it byte for byte as it was.
//!
//! These tests need `python/.venv`, like the other integration tests. No test makes a model
//! request.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use hotend_core::gateway::RunSession;
use hotend_core::{Core, CoreConfig};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// The tools of the registry that change something. Every other tool of the registry is
/// read-only and is called by the test below; a new tool lands in one list or the other on
/// purpose. (`web_search` and `web_fetch` are read-only too, but exist only in a run with
/// web research through a search service, and store nothing.)
const CHANGING: [&str; 5] = [
    "write_model_code",
    "set_parameter",
    "slice",
    "propose_print",
    // Writes a snapshot's name and nothing else: no version, no file (tested below).
    "snapshot_model",
];
const READ_ONLY: [&str; 9] = [
    "sweep_parameters",
    "run_checks",
    "render_views",
    "measure",
    "read_docs",
    "list_cards",
    "list_components",
    "list_print_profiles",
    "print_plan",
];

fn arguments(tool: &str) -> Value {
    match tool {
        "sweep_parameters" => json!({"params": [{"name": "width", "values": [70, 90]}]}),
        "run_checks" => json!({}),
        "render_views" => json!({"views": ["iso", "top"]}),
        "measure" => json!({"queries": [{"kind": "volume"}]}),
        "read_docs" => json!({"query": "fillet"}),
        "list_cards" => json!({}),
        "list_components" => json!({}),
        "list_print_profiles" => json!({}),
        "print_plan" => json!({"explain": true}),
        other => panic!("add arguments for the read-only tool `{other}` to this test"),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Every file under `dir` (by its path relative to `dir`) with the SHA-256 of its bytes;
/// folders are listed too, so an added empty folder counts as a change.
fn tree(dir: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let name = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                out.insert(format!("{name}/"), String::new());
                walk(root, &path, out);
            } else {
                let bytes = std::fs::read(&path).unwrap();
                out.insert(name, hex(&Sha256::digest(&bytes)));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// What the project is, on disk and in the store: its folder, its versions and current
/// version, its stored layouts, slices and print jobs.
async fn state(core: &Core, data: &Path, project_id: &str) -> (BTreeMap<String, String>, Value) {
    let files = tree(&data.join("projects").join(project_id));
    let detail = core.project_get(project_id).await.unwrap();
    let mut layouts = Vec::new();
    for version in &detail.versions {
        layouts.push(json!({
            "preparation": serde_json::to_value(core.preparation_get(project_id, version.number).await.unwrap()).unwrap(),
            "slice": serde_json::to_value(core.slice_get(project_id, Some(version.number)).await.unwrap()).unwrap(),
        }));
    }
    let store = json!({
        "project": serde_json::to_value(&detail.project).unwrap(),
        "versions": serde_json::to_value(&detail.versions).unwrap(),
        "layouts": layouts,
        "jobs": serde_json::to_value(core.print_jobs(project_id).await.unwrap()).unwrap(),
    });
    (files, store)
}

/// Calls every read-only tool once and checks that the project is byte for byte unchanged.
/// Returns the outcomes' error codes by tool.
async fn call_every_read_only_tool(
    core: &Core,
    data: &Path,
    project_id: &str,
    run: &str,
) -> BTreeMap<&'static str, Option<String>> {
    let before = state(core, data, project_id).await;
    let gateway = core.gateway();
    let session = RunSession::new(run, project_id);
    let mut codes = BTreeMap::new();
    for tool in READ_ONLY {
        let outcome = gateway.call(&session, tool, arguments(tool)).await;
        codes.insert(tool, outcome.error.map(|error| error.code));
        let after = state(core, data, project_id).await;
        let added: Vec<_> = after
            .0
            .keys()
            .filter(|k| !before.0.contains_key(*k))
            .collect();
        let changed: Vec<_> = after
            .0
            .iter()
            .filter(|(k, v)| before.0.get(*k).is_some_and(|old| old != *v))
            .map(|(k, _)| k)
            .collect();
        let removed: Vec<_> = before
            .0
            .keys()
            .filter(|k| !after.0.contains_key(*k))
            .collect();
        assert!(
            added.is_empty() && changed.is_empty() && removed.is_empty(),
            "`{tool}` changed the project's files: added {added:?}, changed {changed:?}, removed {removed:?}"
        );
        assert_eq!(
            after.1, before.1,
            "`{tool}` changed what the store says about the project"
        );
    }
    codes
}

#[tokio::test]
async fn the_read_only_tools_leave_the_project_byte_for_byte_unchanged() {
    let test = "the_read_only_tools_leave_the_project_byte_for_byte_unchanged";
    let Some(t) = common::open_core(test, CoreConfig::default()).await else {
        return;
    };
    let core = &t.core;
    let data = t.tmp.path();

    // The registry is exactly the changing tools and the read-only ones.
    let mut registry: Vec<&str> = core.tool_specs().iter().map(|spec| spec.name).collect();
    registry.sort_unstable();
    let mut classified: Vec<&str> = CHANGING.iter().chain(READ_ONLY.iter()).copied().collect();
    classified.sort_unstable();
    assert_eq!(registry, classified, "classify the new tool in this test");

    let project = core.project_create("read only").await.unwrap();
    let built = core
        .model_build(
            &project.id,
            &common::model("box.py"),
            Default::default(),
            "",
        )
        .await
        .unwrap();
    assert!(built.version.is_some(), "{:?}", built.result.error);
    let mut settings = core.settings_get().await.unwrap();
    settings.plate_type = Some("textured-pei".into());
    core.settings_set(settings).await.unwrap();

    // Not laid out yet: `print_plan` says so instead of laying the version out.
    let codes = call_every_read_only_tool(core, data, &project.id, "run-unprepared").await;
    for (tool, code) in &codes {
        let expected = (*tool == "print_plan").then(|| "not_prepared".to_owned());
        assert_eq!(code, &expected, "{tool}");
    }
    assert_eq!(core.preparation_get(&project.id, 1).await.unwrap(), None);

    // Laid out, with a stored plan record; then the settings change so that the record is
    // not the plan of the settings any more. `print_plan` shows the new plan and leaves the
    // record (which a pending job's review compares with) as it was.
    core.prepare_run(&project.id, None, Default::default())
        .await
        .unwrap();
    let stored = core.print_plan_get(&project.id, None, None).await.unwrap();
    assert!(stored.blocked.is_none(), "{:?}", stored.blocked);
    let mut settings = core.settings_get().await.unwrap();
    settings.plate_type = None;
    core.settings_set(settings).await.unwrap();
    let codes = call_every_read_only_tool(core, data, &project.id, "run-prepared").await;
    assert!(codes.values().all(Option::is_none), "{codes:?}");
    let session = RunSession::new("run-plan", &project.id);
    let plan = core.gateway().call(&session, "print_plan", json!({})).await;
    assert!(plan.error.is_none(), "{plan:?}");
    assert!(
        plan.text_content().contains("bed_type_not_chosen")
            || plan.text_content().contains("which plate is on the bed"),
        "the plan of the settings as they are now: {}",
        plan.text_content()
    );
}

/// `snapshot_model`, called by the agent in a scripted conversation, names the current
/// version and changes nothing else: every file of the project, its versions and its current
/// version stay exactly as they were. It has no path, version or folder argument, refuses a
/// name the project already uses, and the label is on the audit log as the agent's.
#[tokio::test]
async fn the_agents_snapshot_writes_only_the_label() {
    let test = "the_agents_snapshot_writes_only_the_label";
    let Some(t) = common::open_core(test, CoreConfig::default()).await else {
        return;
    };
    let core = &t.core;
    let data = t.tmp.path();
    let project = core.project_create("snapshot by the agent").await.unwrap();
    for width in [None, Some(70.0)] {
        let params = width
            .map(|w| BTreeMap::from([("width".to_owned(), w)]))
            .unwrap_or_default();
        let built = core
            .model_build(&project.id, &common::model("box.py"), params, "")
            .await
            .unwrap();
        assert!(built.version.is_some(), "{:?}", built.result.error);
    }
    // The person names version 1; the agent then names the current one, version 2.
    core.snapshot_create(&project.id, "first", Some(1), "")
        .await
        .unwrap();
    let before = state(core, data, &project.id).await;

    let steps = json!([
        {"tool": "snapshot_model", "args": {"name": "Lid fits", "note": "the maker's words"}},
        {"tool": "snapshot_model", "args": {"name": "lid FITS"}},
        {"tool": "snapshot_model", "args": {"name": "elsewhere", "folder": "/tmp/out"}},
        {"tool": "snapshot_model", "args": {"name": "two\nlines"}},
        {"say": "Saved this state as a snapshot."}
    ]);
    let run = core
        .message_send(&project.id, &json!({"steps": steps}).to_string())
        .await
        .unwrap();
    let events = common::collect(run).await;
    assert!(
        matches!(
            events.last().unwrap(),
            hotend_core::RunEvent::Finished { error: None, .. }
        ),
        "{events:?}"
    );

    let after = state(core, data, &project.id).await;
    assert_eq!(after.0, before.0, "the agent's snapshot changed a file");
    assert_eq!(after.1, before.1, "the agent's snapshot changed the store");
    let snapshots = core.snapshots_list(&project.id).await.unwrap();
    let named: Vec<(&str, u32, &str)> = snapshots
        .iter()
        .map(|s| (s.name.as_str(), s.version, s.note.as_str()))
        .collect();
    assert_eq!(
        named,
        [("first", 1, ""), ("Lid fits", 2, "the maker's words")]
    );

    // What the agent was told, call by call.
    let tools: Vec<_> = core
        .audit_log(Some(&project.id), 100)
        .await
        .unwrap()
        .into_iter()
        .rev()
        .filter(|entry| entry.kind == "tool" && entry.name == "snapshot_model")
        .map(|entry| entry.outcome)
        .collect();
    assert_eq!(
        tools,
        ["ok", "name_taken", "invalid_arguments", "invalid_name"]
    );
    let labels: Vec<_> = core
        .audit_log(Some(&project.id), 100)
        .await
        .unwrap()
        .into_iter()
        .filter(|entry| entry.kind == "snapshot")
        .collect();
    assert_eq!(labels.len(), 2, "{labels:?}");
    assert_eq!(labels[0].name, "create");
    assert_eq!(labels[0].detail["by"], "agent");
    assert_eq!(labels[0].detail["snapshot"], "Lid fits");
    assert!(labels[0].run_id.is_some());
    assert_eq!(labels[1].detail["by"], "person");
}
