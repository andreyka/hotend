//! Web research against the real provider, through the real agent process, the real egress
//! and the provider's own web search.
//!
//! These are the only tests that make model requests, and they are paid: they are `ignore`d
//! by a plain `cargo test`, and run only with `-- --ignored`, a key in `ANTHROPIC_API_KEY` and
//! `HOTEND_REAL_PAID=1`; without all three they fail before anything is sent, so they can
//! not pass without running. The model is the one in `HOTEND_REAL_MODEL` (default below).
//!
//! - `one_small_message_searches_the_web_through_the_real_provider`: one model request with
//!   the agent's instructions and at most one web search. It proves what the fixtures
//!   cannot: the provider accepts the tool entries the egress writes (the basic tool versions
//!   with `allowed_callers`, the caps and `url_sources`), the prompt-cache markers and the
//!   request as a whole; and what the egress counts from the real stream agrees with what
//!   the provider reports.
//! - `a_named_part_is_looked_up_once_and_a_plain_box_is_not`: three messages, each a whole
//!   run of at most twelve model requests that builds a model. It checks the behaviour the
//!   instructions ask for, which no fixture can show: a named product is looked up and its
//!   number recorded with its page; a second message that needs the same number does not
//!   search again and reads the conversation from the provider's cache; a plain box with
//!   its sizes given is designed without a search. It also prints, request by request, how
//!   many input tokens were read from the cache and how many were written to it, and says
//!   whether a request that followed a response with web calls found the conversation in
//!   the cache (docs/WEB_RESEARCH.md section 12, the look-back of a cache marker).
//!
//! Run one with `HOTEND_REAL_PAID=1 cargo test -p hotend-core --test real_research <name>
//! -- --ignored --nocapture`.
//!
//! The key is read by the core's environment secret store and added to the outbound request
//! by the egress. This file never prints it; it reads the variable once, to check that the
//! value appears nowhere in what a run produced.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::Path;
use std::time::Duration;

use hotend_core::{
    Core, CoreConfig, ProviderKind, ProviderSettings, ResearchState, RunEvent, SecretBackend,
    Settings, StopReason,
};
use serde_json::Value;

const KEY_VARIABLE: &str = "ANTHROPIC_API_KEY";
/// A paid run is asked for by a person, not by a key that happens to be in the environment.
const PAID_VARIABLE: &str = "HOTEND_REAL_PAID";
const DEFAULT_MODEL: &str = "claude-fable-5-2";
const MESSAGE: &str = "Before anything else, search the web once for the outer diameter of a \
    608 ball bearing in millimetres. Then tell me the number and the site in one sentence. \
    Do not write a model and do not call any other tool.";

/// A real product with published sizes: the instructions say to look it up, not to ask.
const NAMED_PART: &str = "A drop-in desk tray for a Raspberry Pi 5 board, components facing \
    up, with a rim 3 mm high around the board.";
/// Needs the board's length and width a second time.
const FOLLOW_UP: &str = "Now add a flat lid that covers the whole board and rests on the rim.";
/// Every size is given and nothing is named: there is nothing to look up.
const PLAIN_BOX: &str = "A plain open box: 80 mm wide, 60 mm deep and 40 mm high on the \
    outside, with walls 2 mm thick.";

/// Every file below `dir`, as bytes, with its path.
fn files(dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files(&path, out);
        } else if let Ok(bytes) = std::fs::read(&path) {
            out.push((path.display().to_string(), bytes));
        }
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

/// The key. The tests that need it are `ignore`d; run without it they fail, they never pass.
fn required_key(test: &str, cost: &str) -> String {
    let paid = std::env::var(PAID_VARIABLE).is_ok_and(|value| value == "1");
    match std::env::var(KEY_VARIABLE) {
        Ok(key) if paid && !key.trim().is_empty() => key,
        _ => panic!(
            "{test} did not run: it makes {cost}, and needs {PAID_VARIABLE}=1 and {KEY_VARIABLE}"
        ),
    }
}

/// The key is in no event, no stored row and no file of the data directory.
async fn assert_no_key(core: Core, data_dir: &Path, events: &[Value], key: &str) {
    let events_text = serde_json::to_string(events).unwrap();
    assert!(!events_text.contains(key.trim()), "an event holds the key");
    let audit = serde_json::to_string(&core.audit_log(None, 1000).await.unwrap()).unwrap();
    assert!(!audit.contains(key.trim()), "the audit log holds the key");
    drop(core);
    let mut stored = Vec::new();
    files(data_dir, &mut stored);
    for (path, bytes) in &stored {
        assert!(
            !contains(bytes, key.trim().as_bytes()),
            "{path} holds the key"
        );
    }
}

/// One message and what its run did.
struct Sent {
    events: Vec<Value>,
    /// Web searches charged to the project by this run.
    searches: u32,
    /// `record_source` calls the gateway accepted.
    recorded: usize,
    /// The run's last `usage` event.
    usage: Value,
}

/// What one model request of a run used, from the difference of two `usage` events.
struct Request {
    input: u64,
    cache_read: u64,
    cache_write: u64,
    /// Whether the response before this request held a web call.
    after_web: bool,
}

/// The run's model requests in order. The agent's `usage` events hold running totals.
fn requests(events: &[Value]) -> Vec<Request> {
    let n = |event: &Value, key: &str| event[key].as_u64().unwrap_or(0);
    let mut out: Vec<Request> = Vec::new();
    let mut last = Value::Null;
    let mut web_before = false;
    let mut web_now = false;
    for event in events {
        let web = ["web_search", "web_fetch"].contains(&event["name"].as_str().unwrap_or(""));
        if event["type"] == "tool_start" && web {
            web_now = true;
        }
        if event["type"] != "usage" {
            continue;
        }
        let request = Request {
            input: n(event, "input_tokens") - n(&last, "input_tokens"),
            cache_read: n(event, "cache_read_tokens") - n(&last, "cache_read_tokens"),
            cache_write: n(event, "cache_write_tokens") - n(&last, "cache_write_tokens"),
            after_web: web_before,
        };
        if n(event, "requests") > n(&last, "requests") {
            out.push(request);
            web_before = web_now;
            web_now = false;
        } else if let Some(open) = out.last_mut() {
            // The same request, counted further (a tool call ended): keep the larger numbers.
            open.input += request.input;
            open.cache_read += request.cache_read;
            open.cache_write += request.cache_write;
        }
        last = event.clone();
    }
    out
}

/// Sends one message, waits for the run and requires a normal end.
async fn send(core: &Core, project_id: &str, label: &str, text: &str) -> Sent {
    let before = core.research_status(project_id).await.unwrap();
    let mut run = core.message_send(project_id, text).await.unwrap();
    let mut events = Vec::new();
    let end = tokio::time::timeout(Duration::from_secs(900), async {
        loop {
            match run.next().await {
                Some(RunEvent::Agent { event }) => events.push(event),
                Some(end) => break end,
                None => panic!("{label}: the event stream closed before the run ended"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{label}: the run did not end within fifteen minutes"));
    match &end {
        RunEvent::Finished {
            stop_reason: StopReason::Done | StopReason::Budget,
            error: None,
        } => {}
        other => panic!("{label}: the run did not end normally: {other:?}"),
    }
    let after = core.research_status(project_id).await.unwrap();
    let usage = events
        .iter()
        .rev()
        .find(|event| event["type"] == "usage")
        .cloned()
        .unwrap_or(Value::Null);
    let recorded = events
        .iter()
        .filter(|event| {
            event["type"] == "tool_end" && event["name"] == "record_source" && event["ok"] == true
        })
        .count();
    let sent = Sent {
        searches: after.searches_used - before.searches_used,
        recorded,
        usage,
        events,
    };
    println!(
        "{label}: {} model request(s), {} web search(es), {} number(s) recorded with a page, \
         {} input tokens of which {} read from the cache and {} written to it",
        sent.usage["requests"],
        sent.searches,
        sent.recorded,
        sent.usage["input_tokens"],
        sent.usage["cache_read_tokens"],
        sent.usage["cache_write_tokens"],
    );
    for (index, request) in requests(&sent.events).iter().enumerate() {
        println!(
            "{label}:   request {}: {} input, {} from the cache, {} to the cache{}",
            index + 1,
            request.input,
            request.cache_read,
            request.cache_write,
            if request.after_web {
                " (follows a response with web calls)"
            } else {
                ""
            },
        );
    }
    sent
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "makes paid requests with ANTHROPIC_API_KEY; run by a person, with `-- --ignored`"]
async fn a_named_part_is_looked_up_once_and_a_plain_box_is_not() {
    let test = "a_named_part_is_looked_up_once_and_a_plain_box_is_not";
    let key = required_key(test, "three real runs of up to twelve model requests each");
    let runtime = common::sandboxed_python(test)
        .await
        .unwrap_or_else(|| panic!("{test} did not run: the sandbox is not usable here"));
    let model = std::env::var("HOTEND_REAL_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_owned());
    let tmp = tempfile::tempdir().unwrap();
    let core = Core::open(CoreConfig {
        data_dir: Some(tmp.path().to_path_buf()),
        runtime: Some(runtime),
        secrets: SecretBackend::Environment,
        ..CoreConfig::default()
    })
    .await
    .unwrap();
    core.settings_set(Settings {
        provider: Some(ProviderSettings {
            kind: ProviderKind::Anthropic,
            model,
        }),
        max_turns: 12,
        ..Settings::default()
    })
    .await
    .unwrap();
    // The person's agreement, which this test gives by being run with a key on purpose.
    core.research_consent(true).await.unwrap();

    let tray = core.project_create("Board tray").await.unwrap();
    let status = core.research_status(&tray.id).await.unwrap();
    assert_eq!(status.state, ResearchState::On, "{}", status.message);

    // 1. A named product: looked up, and the number recorded with its page.
    let first = send(&core, &tray.id, "named part", NAMED_PART).await;
    let notes = core.research_notes(&tray.id).await.unwrap();
    assert!(
        first.searches >= 1,
        "the request names a real product and the agent did not search for its size"
    );
    assert!(
        first.recorded >= 1 && !notes.is_empty(),
        "the agent searched and recorded no number with its page ({} accepted call(s), {} \
         note(s))",
        first.recorded,
        notes.len()
    );
    assert!(
        first.searches <= status.searches_per_turn * 2,
        "{} searches for one product",
        first.searches
    );

    // 2. The same number is needed again: it is in the notes, so nothing is searched, and
    //    the conversation so far is read from the provider's cache.
    let second = send(&core, &tray.id, "follow-up", FOLLOW_UP).await;
    assert_eq!(
        second.searches, 0,
        "the number was in the research notes and the agent searched again"
    );
    assert!(
        second.usage["cache_read_tokens"].as_u64().unwrap_or(0) > 0,
        "the second message read nothing from the provider's cache: {}",
        second.usage
    );

    // 3. A plain box with its sizes given, in a project of its own with research on.
    let plain = core.project_create("Plain box").await.unwrap();
    assert_eq!(
        core.research_status(&plain.id).await.unwrap().state,
        ResearchState::On
    );
    let third = send(&core, &plain.id, "plain box", PLAIN_BOX).await;
    assert_eq!(
        third.searches, 0,
        "every size was given and the agent searched the web"
    );
    assert!(
        core.research_notes(&plain.id).await.unwrap().is_empty(),
        "a number of a plain box was recorded as found on the web"
    );

    // The look-back of a cache marker (docs/WEB_RESEARCH.md section 12). The first request
    // of the first run writes the instructions and tools to the cache; a later request
    // that reads no more than that has not found the conversation there.
    let runs = [requests(&first.events), requests(&second.events)];
    let prefix = runs[0]
        .first()
        .map_or(0, |request| request.cache_read + request.cache_write);
    let after_web: Vec<&Request> = runs
        .iter()
        .flatten()
        .filter(|request| request.after_web)
        .collect();
    if after_web.is_empty() {
        println!(
            "{test}: no request followed a response with web calls; the look-back was not measured"
        );
    }
    for request in after_web {
        if request.cache_read <= prefix {
            println!(
                "{test}: LOOK-BACK MISSED: a request after a response with web calls read {} \
                 tokens from the cache, no more than the instructions ({prefix}), and wrote {} \
                 again. See docs/WEB_RESEARCH.md section 12.",
                request.cache_read, request.cache_write
            );
        } else {
            println!(
                "{test}: a request after a response with web calls read {} tokens from the \
                 cache (instructions: {prefix}): the conversation was found there",
                request.cache_read
            );
        }
    }

    let mut events = first.events;
    events.extend(second.events);
    events.extend(third.events);
    assert_no_key(core, tmp.path(), &events, &key).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "makes paid requests with ANTHROPIC_API_KEY; run by a person, with `-- --ignored`"]
async fn one_small_message_searches_the_web_through_the_real_provider() {
    let test = "one_small_message_searches_the_web_through_the_real_provider";
    let key = required_key(test, "one real request");
    let runtime = common::sandboxed_python(test)
        .await
        .unwrap_or_else(|| panic!("{test} did not run: the sandbox is not usable here"));
    let model = std::env::var("HOTEND_REAL_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_owned());
    let tmp = tempfile::tempdir().unwrap();
    let core = Core::open(CoreConfig {
        data_dir: Some(tmp.path().to_path_buf()),
        runtime: Some(runtime),
        // The key stays where it is: the core reads it when it sends the request.
        secrets: SecretBackend::Environment,
        ..CoreConfig::default()
    })
    .await
    .unwrap();
    let mut settings = Settings {
        provider: Some(ProviderSettings {
            kind: ProviderKind::Anthropic,
            model,
        }),
        // One model turn: the search runs at the provider inside it.
        max_turns: 1,
        ..Settings::default()
    };
    settings.research.searches_per_turn = 1;
    settings.research.fetches_per_turn = 1;
    settings.research.max_content_tokens = 1000;
    core.settings_set(settings).await.unwrap();
    // The person's agreement, which this test gives by being run with a key on purpose.
    core.research_consent(true).await.unwrap();
    let project = core.project_create("Real research").await.unwrap();
    let before = core.research_status(&project.id).await.unwrap();
    assert_eq!(before.state, ResearchState::On, "{}", before.message);
    assert!(before.search && before.fetch);

    let mut run = core.message_send(&project.id, MESSAGE).await.unwrap();
    let mut events = Vec::new();
    let end = tokio::time::timeout(Duration::from_secs(300), async {
        loop {
            match run.next().await {
                Some(RunEvent::Agent { event }) => events.push(event),
                Some(end) => break end,
                None => panic!("the event stream closed before the run ended"),
            }
        }
    })
    .await
    .expect("the run did not end within five minutes");

    // The request was accepted and answered. (A refusal by the egress or a 400 from the
    // provider ends the run as `Failed` or with an error; the message is the provider's own
    // words and holds no key.)
    match &end {
        RunEvent::Finished {
            stop_reason: StopReason::Done | StopReason::Budget,
            error: None,
        } => {}
        other => panic!("the run did not end normally: {other:?}"),
    }
    let said: String = events
        .iter()
        .filter(|event| event["type"] == "text_delta")
        .filter_map(|event| event["text"].as_str())
        .collect();
    assert!(!said.trim().is_empty(), "the model said nothing");

    // What was used: the agent's count (the provider's own number), the core's count from
    // the stream, and the project's total agree.
    let usage = events
        .iter()
        .rev()
        .find(|event| event["type"] == "usage")
        .cloned()
        .unwrap_or(Value::Null);
    let research = events.iter().find(|event| event["type"] == "research");
    let status = core.research_status(&project.id).await.unwrap();
    let reported = usage["web_searches"].as_u64().unwrap_or(0);
    println!(
        "{test}: {} model request(s), {} input and {} output tokens, {} cached; provider \
         reports {reported} web search(es); Hotend counted {} search(es), {} fetch(es), {} \
         billed, {} bytes of web results",
        usage["requests"],
        usage["input_tokens"],
        usage["output_tokens"],
        usage["cache_read_tokens"],
        status.searches_used,
        status.fetches_used,
        status.billed_searches,
        status.result_bytes,
    );
    assert!(
        status.searches_used <= 2,
        "more searches than twice the cap of 1"
    );
    assert!(
        status.fetches_used <= 2,
        "more fetches than twice the cap of 1"
    );
    assert_eq!(
        u64::from(status.billed_searches),
        reported,
        "the searches Hotend read from the stream are not the ones the provider reported"
    );
    if status.searches_used > 0 {
        let research = research.expect("a run that searched sends the `research` event");
        assert_eq!(research["searches"], status.searches_used);
        assert!(status.result_bytes > 0);
        let calls: Vec<&Value> = events
            .iter()
            .filter(|event| event["type"] == "tool_start" && event["name"] == "web_search")
            .collect();
        assert!(!calls.is_empty(), "the search was not shown as it happened");
        let log = core.research_log(&project.id).await.unwrap();
        assert!(log.iter().any(|entry| entry.kind == "search"), "{log:?}");
        let sources = core.research_sources(&project.id).await.unwrap();
        assert!(
            !sources.is_empty(),
            "a search with results stores its pages"
        );
    } else {
        println!(
            "{test}: the model answered without searching; only the request shape was checked"
        );
    }

    assert_no_key(core, tmp.path(), &events, &key).await;
}

/// No request is made here: the table of requests is read from the agent's running totals.
#[test]
fn the_request_table_is_read_from_running_totals() {
    let usage = |requests: u64, input: u64, read: u64, write: u64| {
        serde_json::json!({"type": "usage", "requests": requests, "input_tokens": input,
            "cache_read_tokens": read, "cache_write_tokens": write})
    };
    let events = vec![
        serde_json::json!({"type": "tool_start", "name": "web_search"}),
        usage(1, 9000, 0, 8000),
        serde_json::json!({"type": "tool_start", "name": "record_source"}),
        // The same request, counted again after the tool call ended.
        usage(1, 9000, 0, 8000),
        usage(2, 21000, 8000, 11500),
        usage(3, 34000, 19500, 12000),
    ];
    let table = requests(&events);
    let rows: Vec<(u64, u64, u64, bool)> = table
        .iter()
        .map(|r| (r.input, r.cache_read, r.cache_write, r.after_web))
        .collect();
    assert_eq!(
        rows,
        [
            (9000, 0, 8000, false),
            (12000, 8000, 3500, true),
            (13000, 11500, 500, false),
        ]
    );
}
