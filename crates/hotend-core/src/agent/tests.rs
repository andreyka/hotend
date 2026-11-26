//! The supervisor driven by the scripted agent of `tests/fixtures/fake_agent.py`, with model
//! requests going to the stand-in server of the egress tests. The engine is not involved.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::*;
use crate::core::{Core, CoreConfig, CoreError, MessageRun};
use crate::egress::tests::{KEY, Upstream, spawn_upstream};
use crate::secrets::{MemorySecretStore, Secret, SecretStore, provider_key_name};
use crate::settings::{ProviderSettings, Settings};
use crate::testutil::{fake_agent, sandboxed_python};

struct Fixture {
    core: Core,
    upstream: Upstream,
    project_id: String,
    tmp: tempfile::TempDir,
}

async fn fixture(test: &str, limits: AgentLimits) -> Option<Fixture> {
    let runtime = sandboxed_python(test).await?;
    let tmp = tempfile::tempdir().unwrap();
    let secrets = Arc::new(MemorySecretStore::default());
    secrets
        .set(
            &provider_key_name(ProviderKind::Anthropic),
            Secret::new(KEY),
        )
        .unwrap();
    let upstream = spawn_upstream().await;
    let config = CoreConfig {
        data_dir: Some(tmp.path().to_path_buf()),
        agent: Some(fake_agent(&runtime)),
        runtime: Some(runtime),
        agent_limits: limits,
        ..CoreConfig::default()
    };
    let core = Core::open_with_upstream_for_tests(config, secrets, &upstream.origin)
        .await
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
    let project_id = core.project_create("test").await.unwrap().id;
    Some(Fixture {
        core,
        upstream,
        project_id,
        tmp,
    })
}

impl Fixture {
    async fn send(&self, steps: Value) -> MessageRun {
        self.core
            .message_send(&self.project_id, &json!({"steps": steps}).to_string())
            .await
            .unwrap()
    }

    fn history(&self) -> Option<Vec<Value>> {
        let store = crate::store::Store::open(&self.tmp.path().join("hotend.db")).unwrap();
        store
            .history(&self.project_id)
            .unwrap()
            .map(|(_, history)| history)
    }

    fn jobs_left(&self) -> usize {
        std::fs::read_dir(self.tmp.path().join("jobs"))
            .unwrap()
            .count()
    }
}

async fn collect(mut run: MessageRun) -> Vec<RunEvent> {
    let mut events = Vec::new();
    let all = async {
        while let Some(event) = run.next().await {
            events.push(event);
        }
    };
    tokio::time::timeout(Duration::from_secs(60), all)
        .await
        .expect("the run did not end");
    events
}

fn agent_events(events: &[RunEvent]) -> Vec<&Value> {
    events
        .iter()
        .filter_map(|e| match e {
            RunEvent::Agent { event } => Some(event),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_run_reaches_the_gateway_and_the_egress_and_its_history_is_stored() {
    let name = "a_run_reaches_the_gateway_and_the_egress_and_its_history_is_stored";
    let Some(f) = fixture(name, AgentLimits::default()).await else {
        return;
    };
    let run = f
        .send(json!([
            {"say": "hello "},
            {"tool": "measure", "args": {"queries": [{"kind": "volume"}]}},
            {"tool": "start_print", "args": {}},
            {"tool": "measure", "args": {"queries": []}},
            {"tool": "measure", "args": {"queries": [{"kind": "volume"}]}, "run_id": "another-run"},
            {"llm": {"path": "/v1/messages", "body": {"model": "test-model", "max_tokens": 64},
                     "headers": {"content-type": "application/json",
                                 "x-api-key": "made-up-by-the-agent",
                                 "authorization": "Bearer made-up"}}},
            {"llm": {"path": "/v1/messages", "body": {"max_tokens": 64}, "token": "wrong"}},
            {"llm": {"path": "/v1/files", "body": {"max_tokens": 64}}},
            {"request": {"method": "fs.read", "params": {"path": "/etc/passwd"}}},
            {"event": {"type": "usage", "input_tokens": 3}},
            {"event": {"type": "shell", "command": "rm -rf /"}}
        ]))
        .await;
    let run_id = run.run_id.clone();
    let events = collect(run).await;

    // Events: the valid ones in order, the invalid one dropped, the final one last.
    assert_eq!(
        events.last().unwrap(),
        &RunEvent::Finished {
            stop_reason: StopReason::Done,
            error: None
        }
    );
    let from_agent = agent_events(&events);
    let types: Vec<&str> = from_agent
        .iter()
        .map(|e| e["type"].as_str().unwrap())
        .collect();
    assert_eq!(types[0], "text_delta");
    assert!(types.contains(&"tool_start") && types.contains(&"tool_end"));
    assert!(types.contains(&"usage") && !types.contains(&"shell"));
    let said: String = events.iter().filter_map(text_of).collect();
    assert_eq!(said, "hello data: one\n\ndata: two\n\ndata: three\n\n");

    // What the agent got back, as it recorded it in the history.
    let history = f.history().expect("the history must be stored");
    assert_eq!(history[0]["role"], "user");
    let log = history[1]["log"].as_array().unwrap();
    let tool_error = |i: usize| {
        log[i]["response"]["result"]["error"]["code"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert_eq!(log[0]["response"]["result"]["ok"], false);
    assert_eq!(tool_error(0), "no_current_version");
    assert_eq!(tool_error(1), "unknown_tool");
    assert_eq!(tool_error(2), "invalid_arguments");
    assert_eq!(
        log[3]["response"]["error"]["code"], -32602,
        "a foreign run id must be refused"
    );
    assert_eq!(log[4]["response"]["result"], json!({"ok": true}));
    assert_eq!(log[4]["start"][0]["status"], 200);
    assert_eq!(
        log[4]["start"][0]["headers"]["content-type"],
        "text/event-stream"
    );
    assert!(
        log[4]["chunks"].as_u64().unwrap() >= 2,
        "the response must be streamed"
    );
    assert_eq!(
        log[5]["response"]["error"]["data"]["code"],
        "not_authorised"
    );
    assert_eq!(log[6]["response"]["error"]["data"]["code"], "denied");
    assert_eq!(log[7]["response"]["error"]["code"], -32601);

    // The provider saw exactly one request, with the real key and nothing from the agent.
    let seen = f.upstream.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].headers["x-api-key"], KEY);
    assert!(!seen[0].headers.contains_key("authorization"));

    // Transcript and audit.
    let detail = f.core.project_get(&f.project_id).await.unwrap();
    let roles: Vec<&str> = detail.transcript.iter().map(|t| t.role.as_str()).collect();
    assert_eq!(roles, ["user", "assistant"]);
    assert_eq!(detail.transcript[1].text, said.trim());
    let audit = f.core.audit_log(Some(&f.project_id), 100).await.unwrap();
    let of_kind = |kind: &str| audit.iter().filter(|a| a.kind == kind).count();
    assert_eq!(of_kind("tool"), 3);
    assert_eq!(of_kind("egress"), 3);
    assert_eq!(of_kind("agent"), 1);
    assert!(
        audit
            .iter()
            .all(|a| a.run_id.as_deref() == Some(run_id.as_str()))
    );
    let agent_entry = audit.iter().find(|a| a.kind == "agent").unwrap();
    assert_eq!(agent_entry.outcome, "done");
    assert_eq!(agent_entry.detail["llm_requests"], 1);

    // The key is nowhere the agent, the UI or the log can see it.
    let visible = format!(
        "{} {events:?} {}",
        serde_json::to_string(&history).unwrap(),
        serde_json::to_string(&audit).unwrap()
    );
    assert!(!visible.contains(KEY) && !visible.contains(&KEY[..12]));
    assert_eq!(
        f.jobs_left(),
        0,
        "the agent's scratch folder must be removed"
    );
    assert!(
        !f.core.run_cancel(&run_id).await,
        "a finished run cannot be cancelled"
    );
}

#[tokio::test]
async fn a_cancelled_run_stops_and_keeps_its_history() {
    let Some(f) = fixture(
        "a_cancelled_run_stops_and_keeps_its_history",
        AgentLimits::default(),
    )
    .await
    else {
        return;
    };
    let mut run = f.send(json!([{"say": "working"}, {"sleep": 120}])).await;
    assert!(matches!(run.next().await, Some(RunEvent::Agent { .. })));
    let started = Instant::now();
    assert!(f.core.run_cancel(&run.run_id).await);
    let events = collect(run).await;
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "the agent obeys a cancel at once"
    );
    assert_eq!(
        events.last().unwrap(),
        &RunEvent::Finished {
            stop_reason: StopReason::Cancelled,
            error: None
        }
    );
    assert_eq!(f.history().unwrap().len(), 2);
    assert_eq!(f.jobs_left(), 0);
}

#[tokio::test]
async fn an_agent_that_ignores_a_cancel_is_killed_after_the_grace_period() {
    let limits = AgentLimits {
        cancel_grace: Duration::from_millis(400),
        ..AgentLimits::default()
    };
    let Some(f) = fixture("an_agent_that_ignores_a_cancel_is_killed", limits).await else {
        return;
    };
    let mut run = f
        .send(json!([{"ignore_cancel": true}, {"say": "busy"}, {"sleep": 120}]))
        .await;
    assert!(run.next().await.is_some());
    let started = Instant::now();
    f.core.run_cancel(&run.run_id).await;
    let events = collect(run).await;
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(matches!(
        events.last().unwrap(),
        RunEvent::Finished {
            stop_reason: StopReason::Cancelled,
            ..
        }
    ));
    // The agent never answered, so the stored history is the one the run started with.
    assert_eq!(f.history(), Some(vec![]));
}

#[tokio::test]
async fn a_crashed_agent_is_a_typed_failure_and_stores_nothing() {
    let Some(f) = fixture("a_crashed_agent_is_a_typed_failure", AgentLimits::default()).await
    else {
        return;
    };
    let events = collect(f.send(json!([{"say": "about to die"}, {"exit": 3}])).await).await;
    let RunEvent::Failed { code, message } = events.last().unwrap() else {
        panic!("expected a failure, got {events:?}");
    };
    assert_eq!(code, "exited");
    assert!(message.contains("exiting on request"), "{message}");
    assert_eq!(f.history(), None);
    let audit = f.core.audit_log(Some(&f.project_id), 10).await.unwrap();
    assert!(
        audit
            .iter()
            .any(|a| a.kind == "agent" && a.outcome == "exited")
    );

    // A run the agent itself reports as failed keeps the earlier history.
    let events = collect(f.send(json!([{"stop_reason": "error"}])).await).await;
    assert_eq!(
        events.last().unwrap(),
        &RunEvent::Finished {
            stop_reason: StopReason::Error,
            error: Some("scripted failure".into())
        }
    );
    assert_eq!(f.history(), None);

    // The transcript keeps what the user saw of both runs, marked as not remembered: the
    // history the next run starts with holds neither.
    let failed = collect(
        f.send(json!([{"say": "half an answer"}, {"stop_reason": "error"}]))
            .await,
    )
    .await;
    assert!(matches!(
        failed.last().unwrap(),
        RunEvent::Finished {
            stop_reason: StopReason::Error,
            ..
        }
    ));
    let detail = f.core.project_get(&f.project_id).await.unwrap();
    let lines: Vec<(&str, &str, bool)> = detail
        .transcript
        .iter()
        .map(|t| (t.role.as_str(), t.text.as_str(), t.remembered))
        .collect();
    assert_eq!(lines.len(), 5, "{lines:?}");
    assert!(lines.iter().all(|line| !line.2), "{lines:?}");
    assert_eq!(lines[1], ("assistant", "about to die", false));
    assert_eq!(lines[4], ("assistant", "half an answer", false));
    assert_eq!(f.history(), None);

    // A run that works is remembered, and its history has nothing of the failed ones.
    collect(f.core.message_send(&f.project_id, "hi").await.unwrap()).await;
    let detail = f.core.project_get(&f.project_id).await.unwrap();
    let last: Vec<bool> = detail.transcript[5..]
        .iter()
        .map(|t| t.remembered)
        .collect();
    assert_eq!(last, [true, true]);
    // The answers of the failed runs are not in it; the maker's new message carries the
    // core's note that names the messages they answered (which quote the scripts).
    let history = f.history().unwrap();
    let message = history[0]["content"].as_str().unwrap();
    assert!(message.starts_with("hi\n\n[Hotend note:"), "{message}");
    let answers = serde_json::to_string(&history[1..]).unwrap();
    assert!(!answers.contains("half an answer") && !answers.contains("about to die"));
}

const BOX_MODEL: &str =
    "from build123d import *\nPARAMS = {}\ndef build(p, ctx):\n    return Box(20, 20, 10)\n";

#[tokio::test]
async fn after_a_failed_run_the_next_message_is_told_what_it_left_and_goes_on() {
    let Some(f) = fixture(
        "after_a_failed_run_the_next_message_is_told",
        AgentLimits::default(),
    )
    .await
    else {
        return;
    };
    // The run builds a version, then the agent process dies before it answers.
    let crashed = f
        .send(json!([
            {"say": "building"},
            {"tool": "write_model_code", "args": {"code": BOX_MODEL}},
            {"exit": 3}
        ]))
        .await;
    let events = collect(crashed).await;
    assert!(
        matches!(events.last().unwrap(), RunEvent::Failed { code, .. } if code == "exited"),
        "{events:?}"
    );
    // Nothing half-done: the conversation is as it was, the version is whole, no job
    // folder and no run are left.
    assert_eq!(f.history(), None);
    let detail = f.core.project_get(&f.project_id).await.unwrap();
    assert_eq!(
        detail.versions.iter().map(|v| v.number).collect::<Vec<_>>(),
        [1]
    );
    assert_eq!(detail.project.current_version, Some(1));
    assert_eq!(f.jobs_left(), 0);
    assert!(detail.transcript.iter().all(|line| !line.remembered));
    let versions = f
        .tmp
        .path()
        .join("projects")
        .join(&f.project_id)
        .join("versions");
    let staged: Vec<_> = std::fs::read_dir(&versions)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
        .collect();
    assert!(staged.is_empty(), "no half-written version folder");

    // A run the agent reports as failed (a provider error) adds to what is told.
    let reported = collect(f.send(json!([{"stop_reason": "error"}])).await).await;
    assert!(matches!(
        reported.last().unwrap(),
        RunEvent::Finished {
            stop_reason: StopReason::Error,
            ..
        }
    ));

    // The next message carries a note for the model about both, after the maker's text;
    // the transcript keeps the maker's text alone.
    let events = collect(
        f.core
            .message_send(&f.project_id, "make it taller")
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        events.last().unwrap(),
        &RunEvent::Finished {
            stop_reason: StopReason::Done,
            error: None
        }
    );
    let said: String = events.iter().filter_map(text_of).collect();
    assert!(
        said.starts_with("echo: make it taller\n\n[Hotend note: the answer to the following"),
        "{said}"
    );
    assert!(said.contains(": version 1\n"), "{said}");
    assert!(said.contains(": no new version\n"), "{said}");
    assert!(said.contains("The current version is 1."), "{said}");
    assert!(
        said.contains("{\"steps\""),
        "the failed message is quoted: {said}"
    );
    let detail = f.core.project_get(&f.project_id).await.unwrap();
    let last: Vec<(&str, &str, bool)> = detail.transcript[detail.transcript.len() - 2..]
        .iter()
        .map(|t| (t.role.as_str(), t.text.as_str(), t.remembered))
        .collect();
    assert_eq!(last[0], ("user", "make it taller", true));
    assert!(last[1].2);
    assert!(f.history().is_some());

    // Told once: the message after that is the maker's alone.
    let events = collect(f.core.message_send(&f.project_id, "thanks").await.unwrap()).await;
    assert_eq!(
        events.iter().filter_map(text_of).collect::<String>(),
        "echo: thanks"
    );
}

/// A model whose worker dies with SIGABRT unless its job folder marks it as the restart.
const ABORT_ONCE: &str = "import os\nfrom build123d import *\nPARAMS = {}\n\
    def build(p, ctx):\n    if not os.path.exists('restart.json'):\n        os.abort()\n\
    \x20   return Box(20, 20, 10)\n";

#[tokio::test]
async fn a_crashed_engine_worker_is_restarted_once_and_the_person_is_told() {
    let Some(f) = fixture(
        "a_crashed_engine_worker_is_restarted_once",
        AgentLimits::default(),
    )
    .await
    else {
        return;
    };
    let events = collect(
        f.send(json!([
            {"tool": "write_model_code", "args": {"code": ABORT_ONCE}},
            {"say": "built"}
        ]))
        .await,
    )
    .await;
    assert_eq!(
        events.last().unwrap(),
        &RunEvent::Finished {
            stop_reason: StopReason::Done,
            error: None
        }
    );
    let said: String = events.iter().filter_map(text_of).collect();
    assert!(
        said.contains("[Hotend: the modelling engine crashed while running `build`")
            && said.contains("restarted once")
            && said.ends_with("built"),
        "{said}"
    );
    // The build of the restart is the version; the tool call saw no error.
    let history = f.history().unwrap();
    assert_eq!(history[1]["log"][0]["response"]["result"]["ok"], true);
    let detail = f.core.project_get(&f.project_id).await.unwrap();
    assert_eq!(detail.versions.len(), 1);
    assert!(
        detail.transcript[1]
            .text
            .contains("modelling engine crashed"),
        "the notice is kept in the transcript"
    );
    let kept = std::fs::read_dir(f.tmp.path().join("crashes"))
        .unwrap()
        .count();
    assert_eq!(kept, 1);
    assert_eq!(f.jobs_left(), 0);
}

/// A model whose build does not end on its own.
const SLOW_MODEL: &str = "import time\nfrom build123d import *\nPARAMS = {}\n\
    def build(p, ctx):\n    time.sleep(600)\n    return Box(20, 20, 10)\n";

fn slow_build() -> Value {
    json!({"tool": "write_model_code", "args": {"code": SLOW_MODEL}, "wait": false})
}

impl Fixture {
    /// Waits until the engine job of a tool call exists next to the agent's own folder.
    async fn wait_for_the_engine_job(&self) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while self.jobs_left() < 2 {
            assert!(Instant::now() < deadline, "the engine job never started");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// After a run that ended with a build in flight: nothing of the build is left behind
    /// and both the tool call and its job are in the audit log.
    async fn assert_the_build_was_stopped_and_recorded(&self, run_id: &str) {
        assert_eq!(self.jobs_left(), 0, "a job folder outlived the run");
        #[cfg(target_os = "linux")]
        assert_eq!(
            launchers_for(&self.tmp.path().canonicalize().unwrap().join("jobs")),
            0,
            "a job outlived the run"
        );
        let audit = self
            .core
            .audit_log(Some(&self.project_id), 100)
            .await
            .unwrap();
        let of_run: Vec<_> = audit
            .iter()
            .filter(|a| a.run_id.as_deref() == Some(run_id))
            .collect();
        let tool = of_run
            .iter()
            .find(|a| a.kind == "tool")
            .unwrap_or_else(|| panic!("the tool call left no audit entry: {of_run:?}"));
        assert_eq!(
            (tool.name.as_str(), tool.outcome.as_str()),
            ("write_model_code", "cancelled")
        );
        assert!(tool.args_sha256.is_some());
        let job = of_run
            .iter()
            .find(|a| a.kind == "job")
            .unwrap_or_else(|| panic!("the engine job left no audit entry: {of_run:?}"));
        assert_eq!(
            (job.name.as_str(), job.outcome.as_str()),
            ("build", "cancelled")
        );
        assert!(job.detail["job_id"].is_string());
        assert_eq!(job.detail["sandbox"]["mode"], "on");
    }
}

#[tokio::test]
async fn cancelling_a_run_stops_and_records_a_build_in_flight() {
    let Some(f) = fixture(
        "cancelling_a_run_stops_and_records_a_build_in_flight",
        AgentLimits::default(),
    )
    .await
    else {
        return;
    };
    let run = f.send(json!([slow_build(), {"sleep": 120}])).await;
    let run_id = run.run_id.clone();
    f.wait_for_the_engine_job().await;
    let started = Instant::now();
    assert!(f.core.run_cancel(&run_id).await);
    let events = collect(run).await;
    assert!(started.elapsed() < Duration::from_secs(8));
    assert_eq!(
        events.last().unwrap(),
        &RunEvent::Finished {
            stop_reason: StopReason::Cancelled,
            error: None
        }
    );
    f.assert_the_build_was_stopped_and_recorded(&run_id).await;
}

#[tokio::test]
async fn an_agent_that_dies_or_finishes_with_a_build_in_flight_leaves_nothing_behind() {
    let Some(f) = fixture(
        "an_agent_that_dies_with_a_build_in_flight",
        AgentLimits::default(),
    )
    .await
    else {
        return;
    };
    // The agent process dies.
    let run = f
        .send(json!([slow_build(), {"sleep": 1.5}, {"exit": 3}]))
        .await;
    let run_id = run.run_id.clone();
    f.wait_for_the_engine_job().await;
    let events = collect(run).await;
    assert!(
        matches!(events.last().unwrap(), RunEvent::Failed { code, .. } if code == "exited"),
        "{events:?}"
    );
    f.assert_the_build_was_stopped_and_recorded(&run_id).await;

    // The agent answers `agent.run` without waiting for its tool call.
    let run = f.send(json!([slow_build(), {"sleep": 1.5}])).await;
    let run_id = run.run_id.clone();
    f.wait_for_the_engine_job().await;
    let events = collect(run).await;
    assert_eq!(
        events.last().unwrap(),
        &RunEvent::Finished {
            stop_reason: StopReason::Done,
            error: None
        }
    );
    f.assert_the_build_was_stopped_and_recorded(&run_id).await;
    assert!(
        f.core
            .project_get(&f.project_id)
            .await
            .unwrap()
            .versions
            .is_empty(),
        "a stopped build must not become a version"
    );
}

#[tokio::test]
async fn a_run_that_keeps_being_refused_is_ended_and_its_refusals_are_bounded() {
    let Some(f) = fixture(
        "a_run_that_keeps_being_refused_is_ended",
        AgentLimits::default(),
    )
    .await
    else {
        return;
    };
    let limit = crate::gateway::GatewayPolicy::default().max_refusals_per_run as usize;
    let floods = [
        ("tool", json!({"tool": "start_print", "args": {}})),
        (
            "egress",
            json!({"llm": {"path": "/v1/files", "body": {"max_tokens": 1}}}),
        ),
    ];
    for (kind, step) in floods {
        let steps: Vec<Value> = std::iter::repeat_n(step, 200).collect();
        let run = f.send(Value::Array(steps)).await;
        let run_id = run.run_id.clone();
        let events = collect(run).await;
        let RunEvent::Failed { code, message } = events.last().unwrap() else {
            panic!("{kind}: expected a failure, got {:?}", events.last());
        };
        assert_eq!(code, "too_many_refusals", "{kind}: {message}");
        let audit = f.core.audit_log(Some(&f.project_id), 1000).await.unwrap();
        let rows: Vec<_> = audit
            .iter()
            .filter(|a| a.run_id.as_deref() == Some(run_id.as_str()) && a.kind == kind)
            .collect();
        assert!(
            (limit..=limit + 2).contains(&rows.len()),
            "{kind}: {} audit entries for 200 refused calls",
            rows.len()
        );
        let agent = audit
            .iter()
            .find(|a| a.run_id.as_deref() == Some(run_id.as_str()) && a.kind == "agent")
            .unwrap();
        assert_eq!(agent.outcome, "too_many_refusals");
    }
    assert!(f.upstream.seen().is_empty());
    assert_eq!(f.jobs_left(), 0);
}

#[tokio::test]
async fn an_agent_that_breaks_the_protocol_is_stopped() {
    let limits = AgentLimits {
        max_protocol_errors: 2,
        max_line_bytes: 4096,
        ..AgentLimits::default()
    };
    let Some(f) = fixture("an_agent_that_breaks_the_protocol_is_stopped", limits).await else {
        return;
    };
    let garbage = json!([
        {"raw": "this is not json"},
        {"raw": "{\"jsonrpc\": \"1.0\", \"method\": \"agent.event\"}"},
        {"raw": "[1, 2, 3]"},
        {"sleep": 120}
    ]);
    let events = collect(f.send(garbage).await).await;
    assert!(
        matches!(events.last().unwrap(), RunEvent::Failed { code, .. } if code == "protocol_error"),
        "{events:?}"
    );

    let long_line = json!([{"raw": "x".repeat(10_000)}, {"sleep": 120}]);
    let events = collect(f.send(long_line).await).await;
    let RunEvent::Failed { code, message } = events.last().unwrap() else {
        panic!("expected a failure, got {events:?}");
    };
    assert_eq!(code, "protocol_error");
    assert!(message.contains("line limit"));
    assert_eq!(f.jobs_left(), 0);
}

#[tokio::test]
async fn a_run_past_its_wall_clock_limit_is_stopped() {
    let limits = AgentLimits {
        wall_seconds: 1,
        ..AgentLimits::default()
    };
    let Some(f) = fixture("a_run_past_its_wall_clock_limit_is_stopped", limits).await else {
        return;
    };
    let started = Instant::now();
    let events = collect(
        f.send(json!([{"ignore_cancel": true}, {"sleep": 120}]))
            .await,
    )
    .await;
    assert!(
        matches!(events.last().unwrap(), RunEvent::Failed { code, .. } if code == "timeout"),
        "{events:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(10));
}

/// The launcher's wall clock and the run's deadline are the same number of seconds; when the
/// launcher's kill closes the output first the run must still end as a timeout. Seen on a
/// loaded 32-CPU WSL machine, where the test above reported "the agent stopped unexpectedly
/// (exit status: 124)".
#[cfg(unix)]
#[test]
fn the_launchers_wall_clock_exit_is_a_timeout() {
    use std::os::unix::process::ExitStatusExt;
    let exited_with = |code: i32| Some(std::process::ExitStatus::from_raw(code << 8));
    assert!(matches!(
        closed_error(exited_with(EXIT_WALL_CLOCK), 7),
        AgentError::Timeout { seconds: 7 }
    ));
    for status in [exited_with(0), exited_with(1), exited_with(125), None] {
        let error = closed_error(status, 7);
        assert_eq!(error.code(), "exited", "{status:?}");
    }
}

/// Launcher processes whose command line names a job folder below `jobs`.
#[cfg(target_os = "linux")]
fn launchers_for(jobs: &std::path::Path) -> usize {
    let needle = jobs.to_string_lossy().into_owned();
    std::fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .filter(|entry| {
            std::fs::read(entry.path().join("cmdline"))
                .map(|cmdline| String::from_utf8_lossy(&cmdline).contains(&needle))
                .unwrap_or(false)
        })
        .count()
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn one_run_per_project_and_dropping_the_core_stops_the_agent() {
    let Some(f) = fixture("dropping_the_core_stops_the_agent", AgentLimits::default()).await else {
        return;
    };
    let jobs = f.tmp.path().canonicalize().unwrap().join("jobs");
    let mut run = f
        .send(json!([{"say": "x"}, {"ignore_cancel": true}, {"sleep": 120}]))
        .await;
    assert!(run.next().await.is_some());
    assert!(launchers_for(&jobs) >= 1, "the agent should be running");
    let second = f.core.message_send(&f.project_id, "again").await;
    assert!(matches!(second, Err(CoreError::Busy)));

    let Fixture { core, tmp, .. } = f;
    drop(core);
    let deadline = Instant::now() + Duration::from_secs(10);
    while launchers_for(&jobs) > 0 {
        assert!(Instant::now() < deadline, "the agent outlived the core");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    drop(tmp);
}

#[tokio::test]
async fn a_message_needs_a_provider_a_key_and_text() {
    let Some(f) = fixture(
        "a_message_needs_a_provider_a_key_and_text",
        AgentLimits::default(),
    )
    .await
    else {
        return;
    };
    assert!(matches!(
        f.core.message_send(&f.project_id, "  ").await,
        Err(CoreError::InvalidInput(_))
    ));
    assert!(f.core.message_send("not-a-project", "hi").await.is_err());

    // A plain message is echoed by the fake agent.
    let events = collect(f.core.message_send(&f.project_id, "hi").await.unwrap()).await;
    assert_eq!(
        events.iter().filter_map(text_of).collect::<String>(),
        "echo: hi"
    );

    // Another provider cannot continue this conversation, and has no key anyway.
    let mut settings = f.core.settings_get().await.unwrap();
    settings.provider = Some(ProviderSettings {
        kind: ProviderKind::Openai,
        model: "m".into(),
    });
    f.core.settings_set(settings.clone()).await.unwrap();
    let error = f.core.message_send(&f.project_id, "hi").await.unwrap_err();
    assert!(error.to_string().contains("anthropic"), "{error}");
    let fresh = f.core.project_create("fresh").await.unwrap();
    let error = f.core.message_send(&fresh.id, "hi").await.unwrap_err();
    assert!(matches!(
        error,
        CoreError::Egress(crate::egress::EgressError::NoCredential { .. })
    ));

    settings.provider = None;
    f.core.settings_set(settings).await.unwrap();
    assert!(matches!(
        f.core.message_send(&fresh.id, "hi").await,
        Err(CoreError::NotConfigured(_))
    ));
    assert_eq!(f.jobs_left(), 0);
}
