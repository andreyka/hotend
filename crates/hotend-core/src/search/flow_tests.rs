//! Web research through a search service, through the whole core: a scripted agent calls
//! `web_search`, `web_fetch` and `record_source` through the gateway, the core calls a
//! stand-in search service on loopback, and the facade shows what the person sees. The
//! model provider is a local one (Ollama) to show that a model without built-in search
//! researches through Keenable. No test reaches a real service.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use super::tests::{Replier, Reply, Seen, stand_in};
use super::{SearchService, SearchSource};
use crate::agent::{AgentLimits, RunEvent, StopReason};
use crate::core::{Core, CoreConfig};
use crate::egress::ProviderKind;
use crate::egress::tests::spawn_upstream;
use crate::research::ResearchState;
use crate::secrets::MemorySecretStore;
use crate::settings::{ProviderSettings, Settings};
use crate::testutil::{fake_agent, sandboxed_python};

/// A made-up search key; no real service's key looks like it.
const SEARCH_KEY: &str = "test-search-key-fedcba9876543210";

fn service() -> Replier {
    Arc::new(|seen: &Seen| {
        if seen.path.ends_with("/search") || seen.path.starts_with("/v1/search") {
            let results = json!([
                {"title": "PLA settings", "url": "https://maker.example/pla",
                 "snippet": "Bed temperature for PLA: 60 C.", "content": "Bed temperature for PLA: 60 C."}
            ]);
            Reply::json(200, json!({"query": "q", "results": results}))
        } else if seen.path == "/extract" {
            Reply::json(
                200,
                json!({"results": [{"url": "https://maker.example/pla",
                "raw_content": "Nozzle 0.4 mm. Bed 60 C."}], "failed_results": []}),
            )
        } else {
            Reply::json(
                200,
                json!({"url": "https://maker.example/pla", "title": "PLA",
                "content": "Use a 4.0 mm hole for an M3 insert."}),
            )
        }
    })
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

async fn run(core: &Core, project: &str, steps: Value) -> Vec<RunEvent> {
    let mut run = core
        .message_send(project, &json!({"steps": steps}).to_string())
        .await
        .unwrap();
    let mut events = Vec::new();
    tokio::time::timeout(Duration::from_secs(120), async {
        while let Some(event) = run.next().await {
            events.push(event);
        }
    })
    .await
    .expect("the run did not end");
    events
}

fn log(tmp: &tempfile::TempDir, project: &str) -> Vec<Value> {
    let store = crate::store::Store::open(&tmp.path().join("hotend.db")).unwrap();
    let (_, history) = store.history(project).unwrap().unwrap();
    history.last().unwrap()["log"].as_array().unwrap().clone()
}

#[tokio::test]
async fn a_local_model_researches_through_keenable_and_then_tavily() {
    let name = "a_local_model_researches_through_keenable_and_then_tavily";
    let Some(runtime) = sandboxed_python(name).await else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let secrets = Arc::new(MemorySecretStore::default());
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
    let (base, seen) = stand_in(service()).await;
    core.set_search_base_for_tests(&base);
    core.settings_set(Settings {
        provider: Some(ProviderSettings {
            kind: ProviderKind::Ollama,
            model: "local-model".into(),
        }),
        ..Settings::default()
    })
    .await
    .unwrap();
    let project = core.project_create("insert test").await.unwrap().id;

    // The default source does not exist for a local model, and Keenable is suggested.
    let status = core.research_status(&project).await.unwrap();
    assert_eq!(status.state, ResearchState::UnsupportedProvider);
    let options = core.research_source_options().await.unwrap();
    assert!(!options[0].available && options[1].suggested);

    // Choosing Keenable needs the person's agreement for Keenable; until then nothing is
    // offered and nothing is sent.
    core.research_source_set(SearchSource::Keenable)
        .await
        .unwrap();
    let status = core.research_status(&project).await.unwrap();
    assert_eq!(status.state, ResearchState::NeedsConsent);
    assert!(status.message.contains("api.keenable.ai"));
    run(
        &core,
        &project,
        json!([{"params": ["research", "tools"]},
        {"tool": "web_search", "args": {"query": "PLA bed temperature"}}]),
    )
    .await;
    let entries = log(&tmp, &project);
    assert_eq!(entries[0]["params"]["research"]["search"], false);
    assert!(entries[0]["params"]["research"].get("service").is_none());
    assert_eq!(entries[0]["params"]["tools"].as_array().unwrap().len(), 14);
    assert!(tool_text(&entries[1]).starts_with("\"unknown_tool\""));
    assert!(
        seen.lock().unwrap().is_empty(),
        "a query left without consent"
    );

    // An agreement for another source than the chosen one is refused.
    assert!(
        core.research_consent_for(SearchSource::BuiltIn, true)
            .await
            .is_err()
    );
    core.research_consent_for(SearchSource::Keenable, true)
        .await
        .unwrap();
    let status = core.research_status(&project).await.unwrap();
    assert_eq!(status.state, ResearchState::On);
    assert!(status.keyless && !status.key_present);

    let events = run(
        &core,
        &project,
        json!([
            {"params": ["research", "tools"]},
            {"tool": "web_search", "args": {"query": "M3 heat set insert hole diameter"}},
            {"tool": "web_fetch", "args": {"url": "http://192.168.1.10/router"}},
            {"tool": "web_fetch", "args": {"url": "https://maker.example/pla"}},
            {"tool": "record_source", "args": {"key": "insert_hole", "label": "M3 insert hole",
                "value": 4.0, "unit": "mm", "url": "https://maker.example/pla",
                "quote": "Use a 4.0 mm hole for an M3 insert."}},
        ]),
    )
    .await;
    assert!(matches!(
        events.last(),
        Some(RunEvent::Finished {
            stop_reason: StopReason::Done,
            ..
        })
    ));
    let entries = log(&tmp, &project);
    let research = &entries[0]["params"]["research"];
    assert_eq!(research["search"], true);
    assert_eq!(research["service"], "keenable");
    let tools: Vec<&str> = entries[0]["params"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool.as_str().unwrap())
        .collect();
    assert_eq!(tools.len(), 17);
    assert_eq!(&tools[14..], ["record_source", "web_search", "web_fetch"]);
    let found = tool_text(&entries[1]);
    assert!(
        found.starts_with("web_search: 1 result from Keenable"),
        "{found}"
    );
    assert!(tool_text(&entries[2]).starts_with("\"url_not_allowed\""));
    let page = tool_text(&entries[3]);
    assert!(
        page.contains("Use a 4.0 mm hole for an M3 insert."),
        "{page}"
    );
    let recorded = tool_text(&entries[4]);
    assert!(recorded.starts_with("recorded as r1"), "{recorded}");
    assert!(
        recorded.contains("quote checked against the page"),
        "{recorded}"
    );

    // The stand-in got the keyless requests, naming the app, with the model's query only.
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].path, "/v1/search/public");
        assert_eq!(seen[1].path, "/v1/fetch/public");
        for request in seen.iter() {
            assert_eq!(request.headers["x-keenable-title"], "Hotend");
            assert!(!request.headers.contains_key("x-api-key"));
            assert!(
                !request.body.contains("insert test"),
                "the project name was sent"
            );
        }
        let body: Value = serde_json::from_str(&seen[0].body).unwrap();
        assert_eq!(body["query"], "M3 heat set insert hole diameter");
    }
    // The person sees the run's summary, the counts and the log.
    let summary: Vec<&Value> = events
        .iter()
        .filter_map(|event| match event {
            RunEvent::Agent { event } if event["type"] == "research" => Some(event),
            _ => None,
        })
        .collect();
    assert_eq!(summary.len(), 1);
    assert_eq!(summary[0]["source"], "keenable");
    assert_eq!(
        (
            summary[0]["searches"].as_u64(),
            summary[0]["fetches"].as_u64()
        ),
        (Some(1), Some(1))
    );
    assert_eq!(summary[0]["search_fee_micro_usd"], 0);
    let status = core.research_status(&project).await.unwrap();
    assert_eq!((status.searches_used, status.fetches_used), (1, 1));
    let notes = core.research_notes(&project).await.unwrap();
    assert_eq!(notes[0].state, "unconfirmed");

    // Tavily: a key first, then a new agreement; the old one was for Keenable.
    core.research_source_set(SearchSource::Tavily)
        .await
        .unwrap();
    assert_eq!(
        core.research_status(&project).await.unwrap().state,
        ResearchState::NeedsKey
    );
    core.search_key_set(SearchService::Tavily, SEARCH_KEY.to_owned())
        .await
        .unwrap();
    let status = core.research_status(&project).await.unwrap();
    assert_eq!(status.state, ResearchState::NeedsConsent);
    assert!(status.key_present);
    core.research_consent(true).await.unwrap();
    let mut settings = core.settings_get().await.unwrap();
    settings.research.searches_per_turn = 1;
    core.settings_set(settings).await.unwrap();
    let events = run(
        &core,
        &project,
        json!([
            {"tool": "web_search", "args": {"query": "PLA nozzle size"}},
            {"tool": "web_search", "args": {"query": "a second one"}},
            {"tool": "web_fetch", "args": {"url": "https://maker.example/pla"}},
        ]),
    )
    .await;
    let entries = log(&tmp, &project);
    assert!(tool_text(&entries[0]).starts_with("web_search: 1 result from Tavily"));
    assert!(tool_text(&entries[1]).starts_with("\"research_cap_reached\""));
    assert!(tool_text(&entries[2]).contains("Nozzle 0.4 mm. Bed 60 C."));
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 4);
        assert_eq!(seen[2].path, "/search");
        assert_eq!(
            seen[2].headers["authorization"],
            format!("Bearer {SEARCH_KEY}")
        );
        assert_eq!(seen[3].path, "/extract");
    }

    // The key is in nothing the core gives out: events, settings, status, the options,
    // the research log, the audit log, the stored conversation.
    let mut everything = String::new();
    for event in &events {
        everything.push_str(&serde_json::to_string(event).unwrap());
    }
    everything.push_str(&serde_json::to_string(&core.settings_get().await.unwrap()).unwrap());
    everything
        .push_str(&serde_json::to_string(&core.research_status(&project).await.unwrap()).unwrap());
    everything
        .push_str(&serde_json::to_string(&core.research_source_options().await.unwrap()).unwrap());
    everything
        .push_str(&serde_json::to_string(&core.research_log(&project).await.unwrap()).unwrap());
    everything
        .push_str(&serde_json::to_string(&core.audit_log(None, 1000).await.unwrap()).unwrap());
    everything.push_str(&serde_json::to_string(&entries).unwrap());
    assert!(!everything.contains(SEARCH_KEY), "the search key leaked");
    assert!(everything.contains("\"key_present\":true"));
    // Switching to `off` stops research; withdrawing clears the agreement.
    core.research_source_set(SearchSource::Off).await.unwrap();
    assert_eq!(
        core.research_status(&project).await.unwrap().state,
        ResearchState::Off
    );
    let settings = core.research_consent(false).await.unwrap();
    assert!(settings.research.acknowledged_at_ms.is_none());
    assert!(settings.research.acknowledged_source.is_none());
}

// ---- through the core -------------------------------------------------------------------

use crate::research::ProjectResearch;
use crate::secrets::{RevealToken, Secret, SecretError, SecretStore};

/// A key store that cannot be read for search keys; provider keys work as in memory.
#[derive(Debug, Default)]
struct BrokenSearchKeys {
    inner: MemorySecretStore,
}

impl BrokenSearchKeys {
    fn broken(name: &str) -> Result<(), SecretError> {
        if name.starts_with("search-key.") {
            Err(SecretError::Backend("the keychain is locked".to_owned()))
        } else {
            Ok(())
        }
    }
}

impl SecretStore for BrokenSearchKeys {
    fn set(&self, name: &str, secret: Secret) -> Result<(), SecretError> {
        Self::broken(name)?;
        self.inner.set(name, secret)
    }
    fn delete(&self, name: &str) -> Result<(), SecretError> {
        Self::broken(name)?;
        self.inner.delete(name)
    }
    fn is_set(&self, name: &str) -> Result<bool, SecretError> {
        Self::broken(name)?;
        self.inner.is_set(name)
    }
    fn reveal(&self, name: &str, token: &RevealToken) -> Result<Option<Secret>, SecretError> {
        Self::broken(name)?;
        self.inner.reveal(name, token)
    }
}

async fn local_core(
    name: &str,
    secrets: Arc<dyn SecretStore>,
) -> Option<(tempfile::TempDir, Core, String)> {
    let runtime = sandboxed_python(name).await?;
    let tmp = tempfile::tempdir().unwrap();
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
            kind: ProviderKind::Ollama,
            model: "local-model".into(),
        }),
        ..Settings::default()
    })
    .await
    .unwrap();
    let project = core.project_create("key test").await.unwrap().id;
    Some((tmp, core, project))
}

fn summary(events: &[RunEvent]) -> Value {
    events
        .iter()
        .find_map(|event| match event {
            RunEvent::Agent { event } if event["type"] == "research" => Some(event.clone()),
            _ => None,
        })
        .expect("no research summary")
}

#[tokio::test]
async fn the_run_summary_says_the_key_the_run_used() {
    let name = "the_run_summary_says_the_key_the_run_used";
    let secrets = Arc::new(MemorySecretStore::default());
    let Some((_tmp, core, project)) = local_core(name, secrets).await else {
        return;
    };
    let (base, seen) = stand_in(service()).await;
    core.set_search_base_for_tests(&base);
    for (source, service, path) in [
        (
            SearchSource::Keenable,
            SearchService::Keenable,
            "/v1/search",
        ),
        (SearchSource::Tavily, SearchService::Tavily, "/search"),
    ] {
        core.research_source_set(source).await.unwrap();
        core.search_key_set(service, SEARCH_KEY.to_owned())
            .await
            .unwrap();
        core.research_consent(true).await.unwrap();
        let events = run(
            &core,
            &project,
            json!([{"tool": "web_search", "args": {"query": "PLA bed temperature"}}]),
        )
        .await;
        let summary = summary(&events);
        assert_eq!(summary["source"], source.as_str());
        let state = &summary["project"];
        assert_eq!(state["state"], "on", "{source:?}: {state}");
        assert_eq!(state["key_present"], true, "{source:?}: {state}");
        assert_eq!(state["keyless"], false, "{source:?}: {state}");
        assert_eq!(seen.lock().unwrap().last().unwrap().path, path);
        assert!(!summary.to_string().contains(SEARCH_KEY));
    }
}

#[tokio::test]
async fn a_key_store_that_cannot_be_read_affects_research_only() {
    let name = "a_key_store_that_cannot_be_read_affects_research_only";
    let secrets = Arc::new(BrokenSearchKeys::default());
    let Some((tmp, core, project)) = local_core(name, secrets).await else {
        return;
    };
    let (base, seen) = stand_in(service()).await;
    core.set_search_base_for_tests(&base);
    let done = |events: &[RunEvent]| {
        matches!(
            events.last(),
            Some(RunEvent::Finished {
                stop_reason: StopReason::Done,
                ..
            })
        )
    };

    // Tavily chosen, research off for this project: the message goes through.
    core.research_source_set(SearchSource::Tavily)
        .await
        .unwrap();
    core.research_set(
        &project,
        ProjectResearch {
            enabled: Some(false),
            ..ProjectResearch::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        core.research_status(&project).await.unwrap().state,
        ResearchState::Off
    );
    let events = run(&core, &project, json!([{"params": ["research"]}])).await;
    assert!(done(&events), "{events:?}");

    // Research on, not agreed and agreed: still only the research part is affected.
    core.research_set(
        &project,
        ProjectResearch {
            enabled: Some(true),
            ..ProjectResearch::default()
        },
    )
    .await
    .unwrap();
    for agree in [false, true] {
        if agree {
            core.research_consent(true).await.unwrap();
        }
        let status = core.research_status(&project).await.unwrap();
        assert_eq!(status.state, ResearchState::NeedsKey);
        assert!(status.message.contains("could not read the key store"));
        let events = run(&core, &project, json!([{"params": ["research"]}])).await;
        assert!(done(&events), "{events:?}");
        let entries = log(&tmp, &project);
        assert_eq!(entries[0]["params"]["research"]["search"], false);
    }

    // Keenable: a key store error is not "no key"; the keyless path is not taken.
    core.research_source_set(SearchSource::Keenable)
        .await
        .unwrap();
    core.research_consent(true).await.unwrap();
    let status = core.research_status(&project).await.unwrap();
    assert_eq!(status.state, ResearchState::NeedsKey);
    assert!(!status.keyless && !status.key_present);
    assert!(
        status.message.contains("does not use the keyless path"),
        "{}",
        status.message
    );
    let events = run(
        &core,
        &project,
        json!([{"params": ["research"]},
               {"tool": "web_search", "args": {"query": "PLA bed temperature"}}]),
    )
    .await;
    assert!(done(&events), "{events:?}");
    let entries = log(&tmp, &project);
    assert_eq!(entries[0]["params"]["research"]["search"], false);
    assert!(tool_text(&entries[1]).starts_with("\"unknown_tool\""));
    assert!(seen.lock().unwrap().is_empty(), "a request went keyless");
}

#[tokio::test]
async fn saving_the_settings_never_writes_the_agreement() {
    let name = "saving_the_settings_never_writes_the_agreement";
    let secrets = Arc::new(MemorySecretStore::default());
    let Some((_tmp, core, project)) = local_core(name, secrets).await else {
        return;
    };
    // Settings that claim an agreement do not give one.
    let mut forged = core.settings_get().await.unwrap();
    forged.research.source = SearchSource::Keenable;
    forged.research.acknowledged_at_ms = Some(1_700_000_000_000);
    forged.research.acknowledged_source = Some(SearchSource::Keenable);
    core.settings_set(forged).await.unwrap();
    let saved = core.settings_get().await.unwrap();
    assert_eq!(saved.research.source, SearchSource::Keenable);
    assert_eq!(saved.research.acknowledged_at_ms, None);
    assert_eq!(saved.research.acknowledged_source, None);
    assert_eq!(
        core.research_status(&project).await.unwrap().state,
        ResearchState::NeedsConsent
    );
    // An agreement given by the person stays when other settings are saved, also from a
    // copy that does not have it ...
    core.research_consent(true).await.unwrap();
    let mut other = core.settings_get().await.unwrap();
    other.research.searches_per_turn = 2;
    other.research.acknowledged_at_ms = None;
    other.research.acknowledged_source = None;
    core.settings_set(other).await.unwrap();
    let saved = core.settings_get().await.unwrap();
    assert_eq!(saved.research.searches_per_turn, 2);
    assert_eq!(
        saved.research.acknowledged_source,
        Some(SearchSource::Keenable)
    );
    assert_eq!(
        core.research_status(&project).await.unwrap().state,
        ResearchState::On
    );
    // ... and a source changed through the settings is not covered by it.
    let mut moved = core.settings_get().await.unwrap();
    moved.research.source = SearchSource::Tavily;
    moved.research.acknowledged_source = Some(SearchSource::Tavily);
    core.settings_set(moved).await.unwrap();
    let saved = core.settings_get().await.unwrap();
    assert_eq!(saved.research.source, SearchSource::Tavily);
    assert_eq!(
        saved.research.acknowledged_source,
        Some(SearchSource::Keenable)
    );
    let status = core.research_status(&project).await.unwrap();
    assert!(!status.acknowledged);
}
