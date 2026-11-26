//! Web research through the whole core: a scripted agent sends a request with the web
//! markers through the egress to a stand-in provider that answers with a scripted search
//! turn, records a number through the gateway and builds a model with it; then the facade
//! shows what the person sees. No test reaches a provider.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::agent::{AgentLimits, RunEvent, StopReason};
use crate::core::{Core, CoreConfig, MessageRun};
use crate::egress::ProviderKind;
use crate::egress::tests::{KEY, Reply, Upstream, spawn_upstream};
use crate::research::{ProjectResearch, ResearchState};
use crate::secrets::{MemorySecretStore, Secret, SecretStore, provider_key_name};
use crate::settings::{ProviderSettings, Settings};
use crate::testutil::{fake_agent, sandboxed_python};

struct Fixture {
    core: Core,
    upstream: Upstream,
    project_id: String,
    tmp: tempfile::TempDir,
}

async fn fixture(test: &str, provider: ProviderKind) -> Option<Fixture> {
    let runtime = sandboxed_python(test).await?;
    let tmp = tempfile::tempdir().unwrap();
    let secrets = Arc::new(MemorySecretStore::default());
    secrets
        .set(&provider_key_name(provider), Secret::new(KEY))
        .unwrap();
    let upstream = spawn_upstream().await;
    let config = CoreConfig {
        data_dir: Some(tmp.path().to_path_buf()),
        agent: Some(fake_agent(&runtime)),
        runtime: Some(runtime),
        agent_limits: AgentLimits::default(),
        ..CoreConfig::default()
    };
    let core = Core::open_with_upstream_for_tests(config, secrets, &upstream.origin)
        .await
        .unwrap();
    core.settings_set(Settings {
        provider: Some(ProviderSettings {
            kind: provider,
            model: "test-model".into(),
        }),
        ..Settings::default()
    })
    .await
    .unwrap();
    let project_id = core.project_create("tamper stand").await.unwrap().id;
    Some(Fixture {
        core,
        upstream,
        project_id,
        tmp,
    })
}

impl Fixture {
    async fn run(&self, steps: Value) -> Vec<RunEvent> {
        let mut run: MessageRun = self
            .core
            .message_send(&self.project_id, &json!({"steps": steps}).to_string())
            .await
            .unwrap();
        let mut events = Vec::new();
        let all = async {
            while let Some(event) = run.next().await {
                events.push(event);
            }
        };
        tokio::time::timeout(Duration::from_secs(120), all)
            .await
            .expect("the run did not end");
        events
    }

    /// What the scripted agent logged in the last stored run.
    fn log(&self) -> Vec<Value> {
        let store = crate::store::Store::open(&self.tmp.path().join("hotend.db")).unwrap();
        let (_, history) = store.history(&self.project_id).unwrap().unwrap();
        history.last().unwrap()["log"].as_array().unwrap().clone()
    }

    fn audit(&self, kind: &str) -> Vec<crate::store::AuditEntry> {
        let store = crate::store::Store::open(&self.tmp.path().join("hotend.db")).unwrap();
        store
            .list_audit(Some(&self.project_id), 200)
            .unwrap()
            .into_iter()
            .filter(|entry| entry.kind == kind)
            .collect()
    }
}

fn transcript(name: &str) -> Reply {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../python/tests/agent/fixtures/research")
        .join(format!("{name}.sse"));
    let bytes = std::fs::read(path).unwrap();
    Reply {
        status: 200,
        content_type: "text/event-stream",
        chunks: bytes.chunks(700).map(<[u8]>::to_vec).collect(),
    }
}

/// A model request with the two markers and a cache marker, as the real agent sends it.
fn research_request() -> Value {
    json!({"llm": {"path": "/v1/messages", "body": {
        "model": "test-model", "max_tokens": 1024, "stream": true,
        "system": [{"type": "text", "text": "s", "cache_control": {"type": "ephemeral"}}],
        "messages": [{"role": "user", "content": "a stand for my Brenta 600 tamper"}],
        "tools": [{"type": "web_search_20260209", "name": "web_search"},
                  {"type": "web_fetch_20260209", "name": "web_fetch"}]}}})
}

const PAGE: &str = "https://maker.example/brenta-600/specs?ref=search";

fn record(value: f64, url: &str, quote: &str) -> Value {
    json!({"tool": "record_source", "args": {
        "key": "basket_diameter", "label": "Basket inner diameter", "value": value,
        "unit": "mm", "url": url, "quote": quote}})
}

/// The simple box of the engine's tests, with its width taken from research note `r1`.
fn model_with_a_web_number() -> String {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../python/tests/models/box.py");
    let code = std::fs::read_to_string(path).unwrap();
    let old = r#""width": {"default": 80.0, "min": 40.0, "max": 200.0, "unit": "mm",
              "label": "Width", "provenance": "style"},"#;
    assert!(code.contains(old), "the box model changed");
    code.replace(
        old,
        r#""width": {"default": 58.5, "min": 40.0, "max": 200.0, "unit": "mm",
              "label": "Width", "provenance": "web", "source": "r1"},"#,
    )
}

/// The same model with its depth tagged as a standard size: a number the maker never gave,
/// set in a run that read the web, with no note behind it. (The stand-in agent's script is
/// the maker's message here, so the number is written in a form no message would use.)
fn model_with_a_claimed_number() -> String {
    let code = model_with_a_web_number();
    let old = r#""depth": {"default": 60.0, "min": 30.0, "max": 200.0, "unit": "mm",
              "label": "Depth", "provenance": "style"},"#;
    assert!(code.contains(old), "the box model changed");
    code.replace(
        old,
        r#""depth": {"default": 6.25e1, "min": 30.0, "max": 200.0, "unit": "mm",
              "label": "Depth", "provenance": "standard"},"#,
    )
}

fn tool_text(entry: &Value) -> String {
    let result = &entry["response"]["result"];
    match result["content"].as_array() {
        Some(items) if !items.is_empty() => items[0]["text"].as_str().unwrap().to_owned(),
        _ => format!(
            "{}: {}",
            result["error"]["code"], result["error"]["message"]
        ),
    }
}

fn research_events(events: &[RunEvent]) -> Vec<&Value> {
    events
        .iter()
        .filter_map(|event| match event {
            RunEvent::Agent { event } if event["type"] == "research" => Some(event),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_run_searches_records_a_number_and_the_maker_is_asked_about_it() {
    let name = "a_run_searches_records_a_number_and_the_maker_is_asked_about_it";
    let Some(f) = fixture(name, ProviderKind::Anthropic).await else {
        return;
    };
    let core = &f.core;

    // Until the person agrees, nothing is different from a core without research.
    let status = core.research_status(&f.project_id).await.unwrap();
    assert_eq!(status.state, ResearchState::NeedsConsent);
    let events = f
        .run(json!([
            {"params": ["research", "tools"]},
            research_request(),
            record(58.5, PAGE, "an inner diameter of 58.5 mm"),
        ]))
        .await;
    assert!(matches!(events.last(), Some(RunEvent::Finished { .. })));
    assert!(research_events(&events).is_empty());
    let log = f.log();
    assert_eq!(log[0]["params"]["research"]["search"], false);
    assert_eq!(log[0]["params"]["tools"].as_array().unwrap().len(), 14);
    assert_eq!(log[1]["response"]["error"]["data"]["code"], "denied");
    assert!(
        tool_text(&log[2]).starts_with("\"unknown_tool\""),
        "{}",
        tool_text(&log[2])
    );
    assert!(
        f.upstream.seen().is_empty(),
        "a request left without consent"
    );

    // With the person's agreement the run gets the two web tools and `record_source`.
    core.research_consent(true).await.unwrap();
    assert_eq!(
        core.research_status(&f.project_id).await.unwrap().state,
        ResearchState::On
    );
    f.upstream.queue(transcript("search_turn"));
    let events = f
        .run(json!([
            {"params": ["research", "tools"]},
            research_request(),
            record(58.5, "https://evil.example/specs", "an inner diameter of 58.5 mm"),
            record(60.0, PAGE, "an inner diameter of 58.5 mm"),
            record(58.5, PAGE, "an inner diameter of 58.5 mm"),
            {"tool": "write_model_code", "args": {"code": model_with_a_web_number()}},
            {"tool": "set_parameter", "args": {"name": "height", "value": 41.0}},
            {"tool": "write_model_code", "args": {"code": model_with_a_claimed_number()}},
            {"shape": "0123456789abcdef"},
        ]))
        .await;
    assert!(
        matches!(
            events.last(),
            Some(RunEvent::Finished {
                stop_reason: StopReason::Done,
                ..
            })
        ),
        "{:?}",
        events.last()
    );
    let log = f.log();
    assert_eq!(
        log[0]["params"]["research"],
        // No shape is stored yet for this project: the agent is told nothing about it.
        json!({"search": true, "fetch": true, "cache_ttl": "1h", "notes": "", "shape": null,
               "keep_pdf": false, "max_result_bytes": 256 * 1024 - 256})
    );
    let tools = log[0]["params"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 15);
    assert_eq!(tools[14], "record_source");

    // The provider got entries written by the egress, with the key, and nothing else new.
    let seen = f.upstream.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].headers["x-api-key"], KEY);
    let sent: Value = serde_json::from_slice(&seen[0].body).unwrap();
    assert_eq!(sent["tools"][0]["type"], "web_search_20250305");
    assert_eq!(sent["tools"][0]["max_uses"], 3);
    assert_eq!(sent["tools"][1]["type"], "web_fetch_20250910");
    assert_eq!(sent["tools"][1]["max_content_tokens"], 6000);
    // The agent got the provider's answer, byte for byte.
    assert!(
        log[1]["body"]
            .as_str()
            .unwrap()
            .contains("web_search_tool_result")
    );

    // A number is stored only from a page the search found, with a quote that holds it.
    assert!(
        tool_text(&log[2]).starts_with("\"unknown_source\""),
        "{}",
        tool_text(&log[2])
    );
    assert!(tool_text(&log[3]).starts_with("\"value_not_in_quote\""));
    let recorded = tool_text(&log[4]);
    assert!(
        recorded.starts_with("recorded as r1: basket_diameter = 58.5 mm from maker.example"),
        "{recorded}"
    );
    assert!(recorded.contains("quote matches a passage the search cited"));
    // The build result names the web number in one short line: the long text, with the
    // quote, is for the maker.
    let built = tool_text(&log[5]);
    assert!(built.contains("version: 1 created"), "{built}");
    assert!(
        built.contains(
            "web numbers nobody has confirmed (1):\n\
             r1 basket_diameter 58.5 mm maker.example unconfirmed, used by width\n"
        ),
        "{built}"
    );
    assert!(!built.contains("The page says"), "{built}");
    // Trying a parameter does not repeat it, and neither would a build that changes
    // nothing about the web numbers.
    let tried = tool_text(&log[6]);
    assert!(tried.contains("version: 2 created"), "{tried}");
    assert!(!tried.contains("web numbers"), "{tried}");
    // A size tagged `standard` in a run that read the web, which the maker never gave and
    // no note holds: Hotend asks about it, and tells the model once.
    let claimed = tool_text(&log[7]);
    assert!(claimed.contains("version: 3 created"), "{claimed}");
    assert!(
        claimed.contains(
            "web numbers nobody has confirmed (2):\n\
             r1 basket_diameter 58.5 mm maker.example unconfirmed, used by width\n\
             r2 depth 62.5 mm no page recorded, used by depth: record its page with \
             record_source, or tell the maker it is your own number\n"
        ),
        "{claimed}"
    );

    // What the person sees: the summary of the run, the note, the question, the log.
    let summary = research_events(&events);
    assert_eq!(summary.len(), 1);
    assert_eq!(summary[0]["searches"], 1);
    assert_eq!(summary[0]["fetches"], 0);
    assert_eq!(summary[0]["search_fee_micro_usd"], 10_000);
    assert_eq!(
        summary[0]["sources"],
        json!([
            {"url": "https://maker.example/brenta-600/specs",
             "title": "Brenta 600 portafilter basket - dimensions"},
            {"url": "https://reviews.example/brenta-600", "title": "Brenta 600 review"}
        ])
    );
    assert_eq!(summary[0]["project"]["searches_used"], 1);
    let status = core.research_status(&f.project_id).await.unwrap();
    assert_eq!((status.searches_used, status.billed_searches), (1, 1));
    assert!(status.result_bytes > 300);
    let notes = core.research_notes(&f.project_id).await.unwrap();
    assert_eq!(notes.len(), 2);
    assert_eq!(
        (notes[0].evidence.as_str(), notes[0].state.as_str()),
        ("citation", "unconfirmed")
    );
    assert_eq!(
        (notes[1].key.as_str(), notes[1].evidence.as_str()),
        ("depth", "unsourced")
    );
    let questions = core.questions_list(&f.project_id, None).await.unwrap();
    let web: Vec<_> = questions
        .iter()
        .filter(|q| q.requirement == "web:r1")
        .collect();
    assert_eq!(web.len(), 1);
    assert_eq!(web[0].web.as_ref().unwrap().url, PAGE);
    assert_eq!(web[0].guessed_params, ["width"]);
    let log_lines = core.research_log(&f.project_id).await.unwrap();
    assert_eq!(
        (log_lines[0].text.as_str(), log_lines[0].outcome.as_str()),
        ("Brenta 600 basket inner diameter mm", "2 results")
    );
    assert_eq!(core.research_sources(&f.project_id).await.unwrap().len(), 2);
    let agent_audit = f.audit("agent");
    let detail = &agent_audit.first().unwrap().detail;
    assert_eq!(detail["web_searches"], 1);
    assert_eq!(detail["billed_web_searches"], 1);
    assert_eq!(detail["research"], "on");

    // The next message: the model saw the note when it made it, so it is not sent again;
    // the request shape of the stored conversation is handed back.
    f.run(json!([{"params": ["research"]}])).await;
    let research = f.log()[0]["params"]["research"].clone();
    assert_eq!(research["notes"], "");
    assert_eq!(research["shape"], "0123456789abcdef");

    // The person confirms the number. Only they can; the model is told with the next message.
    assert!(
        core.research_note_decide(&f.project_id, "r1", "approved")
            .await
            .is_err()
    );
    core.research_note_decide(&f.project_id, "r1", "confirmed")
        .await
        .unwrap();
    f.run(json!([{"params": ["research"]}])).await;
    let notes_block = f.log()[0]["params"]["research"]["notes"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        notes_block.starts_with("[Hotend research notes:"),
        "{notes_block}"
    );
    assert!(notes_block.contains("r1 basket_diameter = 58.5 mm | maker.example | "));
    assert!(notes_block.ends_with("| confirmed by the maker"));
    // What is left to ask is the size without a page; the person settles that one too.
    let questions = core.questions_list(&f.project_id, None).await.unwrap();
    let web: Vec<&str> = questions
        .iter()
        .filter(|q| q.requirement.starts_with("web:"))
        .map(|q| q.requirement.as_str())
        .collect();
    assert_eq!(web, ["web:r2"]);
    core.research_note_decide(&f.project_id, "r2", "confirmed")
        .await
        .unwrap();
    let questions = core.questions_list(&f.project_id, None).await.unwrap();
    assert!(questions.iter().all(|q| !q.requirement.starts_with("web:")));
    assert_eq!(
        f.audit("research").len(),
        2,
        "each decision is audited with the project"
    );

    // Switched off for the project: the next run is as if research did not exist.
    let status = core
        .research_set(
            &f.project_id,
            ProjectResearch {
                enabled: Some(false),
                ..ProjectResearch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(status.state, ResearchState::Off);
    f.run(json!([{"params": ["research", "tools"]}, research_request()]))
        .await;
    let log = f.log();
    assert_eq!(log[0]["params"]["research"]["search"], false);
    assert_eq!(log[0]["params"]["tools"].as_array().unwrap().len(), 14);
    assert_eq!(log[1]["response"]["error"]["data"]["code"], "denied");
    assert_eq!(f.upstream.seen().len(), 1);
    // Bad project settings are refused with what to write instead.
    let bad = ProjectResearch {
        allowed_domains: vec!["https://maker.example/specs".into()],
        ..ProjectResearch::default()
    };
    let error = core.research_set(&f.project_id, bad).await.unwrap_err();
    assert!(error.to_string().contains("is not a host name"), "{error}");
}

#[tokio::test]
async fn a_fetched_pdf_is_not_kept_and_a_refusing_provider_switches_research_off() {
    let name = "a_fetched_pdf_is_not_kept_and_a_refusing_provider_switches_research_off";
    let Some(f) = fixture(name, ProviderKind::Anthropic).await else {
        return;
    };
    let core = &f.core;
    core.research_consent(true).await.unwrap();

    let fetched = json!({"type": "web_fetch_result", "url": "https://maker.example/sheet.pdf?s=1",
        "content": {"type": "document", "source": {"type": "base64",
            "media_type": "application/pdf", "data": "JVBERi0x"}}});
    let pdf = [
        json!({"type": "content_block_start", "index": 0, "content_block": {
            "type": "server_tool_use", "id": "srvtoolu_1", "name": "web_fetch",
            "input": {"url": "https://maker.example/sheet.pdf?s=1"}}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "content_block_start", "index": 1, "content_block": {
            "type": "web_fetch_tool_result", "tool_use_id": "srvtoolu_1", "content": fetched}}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"},
            "usage": {"server_tool_use": {"web_search_requests": 0, "web_fetch_requests": 1}}}),
    ];
    let body: Vec<u8> = pdf
        .iter()
        .flat_map(|event| format!("data: {event}\n\n").into_bytes())
        .collect();
    let reply = || Reply {
        status: 200,
        content_type: "text/event-stream",
        chunks: vec![body.clone()],
    };
    let store = crate::store::Store::open(&f.tmp.path().join("hotend.db")).unwrap();

    // The agent is told what may stay in a conversation.
    f.upstream.queue(reply());
    let events = f
        .run(json!([{"params": ["research"]}, research_request(), {"say": "It is 3 mm."}]))
        .await;
    let research = f.log()[0]["params"]["research"].clone();
    assert_eq!(research["keep_pdf"], false);
    assert!(research["max_result_bytes"].as_u64().unwrap() > 16_000);
    // A PDF came back and the agent took it out of what it stores (the stand-in agent keeps
    // only the text it was sent): the run ends well and is remembered.
    assert!(
        matches!(events.last(), Some(RunEvent::Finished { .. })),
        "{:?}",
        events.last()
    );
    assert!(store.history(&f.project_id).unwrap().is_some());
    let status = core.research_status(&f.project_id).await.unwrap();
    assert_eq!((status.searches_used, status.fetches_used), (0, 1));
    assert_eq!(research_events(&events)[0]["fetches"], 1);
    let log = core.research_log(&f.project_id).await.unwrap();
    assert_eq!(
        (log[0].text.as_str(), log[0].outcome.as_str()),
        ("https://maker.example/sheet.pdf", "pdf_not_kept")
    );

    // The next run is told once which site served a PDF, by its host only: never an
    // address, which the fetch tool could read from the maker's message.
    f.run(json!([{"params": ["research"]}])).await;
    let notes = f.log()[0]["params"]["research"]["notes"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        notes.contains(
            "a page on maker.example was a PDF and was not kept: do not fetch it again, ask \
             the maker for the number"
        ),
        "{notes}"
    );
    assert!(
        !notes.contains("sheet.pdf") && !notes.contains("://"),
        "{notes}"
    );
    f.run(json!([{"params": ["research"]}])).await;
    assert_eq!(f.log()[0]["params"]["research"]["notes"], "");

    // An agent that hands the PDF back in its conversation: nothing is stored (the egress
    // would refuse to send it again), and the person is told which page and what to do.
    let before = store.history(&f.project_id).unwrap();
    f.upstream.queue(reply());
    let kept = json!({"kind": "response", "parts": [{"part_kind": "builtin-tool-return",
        "tool_name": "web_fetch", "tool_call_id": "srvtoolu_1", "content": fetched}]});
    let events = f
        .run(json!([research_request(), {"keep": kept}, {"say": "It is 3 mm."}]))
        .await;
    match events.last() {
        Some(RunEvent::Failed { code, message }) => {
            assert_eq!(code, "web_result_not_kept");
            assert!(message.contains("fetched a PDF"), "{message}");
            assert!(message.contains("maker.example/sheet.pdf"), "{message}");
            assert!(!message.contains("s=1"), "{message}");
            assert!(message.contains("without that page"), "{message}");
            assert!(message.contains("allow PDFs"), "{message}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(store.history(&f.project_id).unwrap(), before);
    assert_eq!(
        core.research_status(&f.project_id)
            .await
            .unwrap()
            .fetches_used,
        2
    );

    // A project that allows PDFs keeps it.
    let allowing = ProjectResearch {
        allow_pdf: true,
        ..ProjectResearch::default()
    };
    core.research_set(&f.project_id, allowing).await.unwrap();
    f.upstream.queue(reply());
    let events = f
        .run(json!([{"params": ["research"]}, research_request(), {"keep": kept}]))
        .await;
    assert!(matches!(events.last(), Some(RunEvent::Finished { .. })));
    assert_eq!(f.log()[0]["params"]["research"]["keep_pdf"], true);
    core.research_set(&f.project_id, ProjectResearch::default())
        .await
        .unwrap();

    // The provider says web search is switched off for the account.
    let refusal = json!({"type": "error", "error": {"type": "invalid_request_error",
        "message": "Web search is not enabled for this organization."}});
    f.upstream.queue(Reply {
        status: 400,
        content_type: "application/json",
        chunks: vec![refusal.to_string().into_bytes()],
    });
    f.run(json!([research_request()])).await;
    let status = core.research_status(&f.project_id).await.unwrap();
    assert_eq!(status.state, ResearchState::ProviderRefused);
    assert!(status.message.contains("provider's console"));
    // The next run has no web tools; it does not fail.
    let events = f.run(json!([{"params": ["research", "tools"]}])).await;
    assert!(matches!(events.last(), Some(RunEvent::Finished { .. })));
    assert_eq!(f.log()[0]["params"]["research"]["search"], false);
    // Switching research on again clears the refusal.
    let on = ProjectResearch {
        enabled: Some(true),
        ..ProjectResearch::default()
    };
    let status = core.research_set(&f.project_id, on).await.unwrap();
    assert_eq!(status.state, ResearchState::On);
}

#[tokio::test]
async fn a_provider_without_web_research_says_so_and_works_as_before() {
    let name = "a_provider_without_web_research_says_so_and_works_as_before";
    let Some(f) = fixture(name, ProviderKind::Openai).await else {
        return;
    };
    let core = &f.core;
    core.research_consent(true).await.unwrap();
    let status = core.research_status(&f.project_id).await.unwrap();
    assert_eq!(status.state, ResearchState::UnsupportedProvider);
    assert!(
        status.message.contains("not available with openai"),
        "{}",
        status.message
    );
    let events = f
        .run(json!([
            {"params": ["research", "tools"]},
            {"llm": {"path": "/v1/responses", "body": {"model": "test-model",
                "max_output_tokens": 64, "tools": [{"type": "web_search"}]}}},
            {"say": "done"},
        ]))
        .await;
    assert!(matches!(
        events.last(),
        Some(RunEvent::Finished {
            stop_reason: StopReason::Done,
            ..
        })
    ));
    let log = f.log();
    assert_eq!(log[0]["params"]["research"]["search"], false);
    assert_eq!(log[0]["params"]["tools"].as_array().unwrap().len(), 14);
    // The provider's own search tool stays refused.
    assert_eq!(log[1]["response"]["error"]["data"]["code"], "denied");
    assert!(f.upstream.seen().is_empty());
}

/// Regression: the busy check was made before the awaits of a run's start and not again
/// before the run was listed, so two messages to one project could both start a run, plan
/// their web research from the same counters and go past the project's cap. Now the second
/// is answered `Busy`, and the counters never pass the cap.
#[tokio::test]
async fn a_second_message_while_a_run_starts_is_busy_and_the_cap_holds() {
    let name = "a_second_message_while_a_run_starts_is_busy_and_the_cap_holds";
    let Some(f) = fixture(name, ProviderKind::Anthropic).await else {
        return;
    };
    let core = &f.core;
    core.research_consent(true).await.unwrap();
    let store = crate::store::Store::open(&f.tmp.path().join("hotend.db")).unwrap();
    // One search of the project's eight is left.
    store.add_research_use(&f.project_id, 7, 0, 7, 0).unwrap();
    f.upstream.queue(transcript("search_turn"));
    let steps = json!({"steps": [{"params": ["research"]}, research_request()]}).to_string();
    let (first, second) = tokio::join!(
        core.message_send(&f.project_id, &steps),
        core.message_send(&f.project_id, &steps),
    );
    let (mut run, refused) = match (first, second) {
        (Ok(run), Err(error)) | (Err(error), Ok(run)) => (run, error),
        (first, second) => panic!(
            "exactly one run must start: {:?} {:?}",
            first.map(|_| ()),
            second.map(|_| ())
        ),
    };
    assert!(
        matches!(refused, crate::core::CoreError::Busy),
        "{refused:?}"
    );
    let all = async { while run.next().await.is_some() {} };
    tokio::time::timeout(Duration::from_secs(120), all)
        .await
        .expect("the run did not end");
    // The started run had one search; the project is at its cap and no further.
    let row = store.research_row(&f.project_id).unwrap();
    assert_eq!(row.searches, 8);
    assert_eq!(
        core.research_status(&f.project_id).await.unwrap().state,
        ResearchState::BudgetUsed
    );
    // The reservation is gone with the run: the next message starts.
    let mut next = core.message_send(&f.project_id, &steps).await.unwrap();
    while next.next().await.is_some() {}
}
