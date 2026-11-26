//! Example searches and source labels through the whole product: the real agent process
//! under the real supervisor, its requests through the egress to a stand-in model on
//! loopback, its tools through the gateway, searches to a stand-in search service on
//! loopback, models built by the real engine. No test reaches a model or the web.
//!
//! The stand-in model is a small program. Which request is open-ended, has sizes or names a
//! device is written into it; whether it searches for examples and when it skips that, it
//! decides by looking for the rule's sentence in the instructions it is sent: without the
//! section about examples it makes no example search, and without a rule to skip one it
//! makes one where these tests want none. So the tests show that the sentences, the tools,
//! the limits, the notes and the labels arrive and hold together. They show nothing of how
//! a real model reads the sentences, tells an open-ended request or words an answer: that
//! is `evals --research`, which needs a real model and has not been run.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use super::SearchSource;
use super::tests::{Replier, Reply, Seen, stand_in};
use crate::agent::{AgentCommand, RunEvent, StopReason};
use crate::core::{Core, CoreConfig};
use crate::egress::ProviderKind;
use crate::secrets::MemorySecretStore;
use crate::settings::{ProviderSettings, Settings};
use crate::testutil::sandboxed_python;

const OPEN_ENDED: &str = "A holder for my headphones.";
const AGAIN: &str = "Another headphone holder, for the other side of the desk.";
const WITH_SIZES: &str = "A box 100x60x40.";
const COMPONENT: &str = "sample phone case using PLA";
const FACT: &str = "A holder for a Raspberry Pi 5, look up its size.";
const GREEDY: &str = "Search for everything you can think of.";
const SHARED_MODEL: &str = "A holder like the one at https://www.printables.com/model/1-holder";

const LOOKED: &str = "looked at: the box is there, the right way up, nothing floats.";

/// What the stand-in model was sent: the instructions, the tools offered, and the results
/// of its tool calls since the maker's message.
struct Request {
    system: String,
    tools: Vec<String>,
    maker: &'static str,
    /// The text of everything before the maker's message.
    before: String,
    results: Vec<(String, String)>,
    bytes: usize,
}

enum Answer {
    Say(String),
    Call(&'static str, Value),
}

/// The text of a chat message, whether a string or a list of parts.
fn text_of(message: &Value) -> String {
    match &message["content"] {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn read(body: &[u8]) -> Request {
    let value: Value = serde_json::from_slice(body).unwrap();
    let messages = value["messages"].as_array().unwrap();
    let requests = [
        OPEN_ENDED,
        AGAIN,
        WITH_SIZES,
        COMPONENT,
        FACT,
        GREEDY,
        SHARED_MODEL,
    ];
    let (at, maker) = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| message["role"] == "user")
        .filter_map(|(at, message)| {
            let text = text_of(message);
            requests
                .iter()
                .find(|request| text.starts_with(**request))
                .map(|request| (at, *request))
        })
        .next_back()
        .expect("no maker's message");
    let mut names = std::collections::BTreeMap::new();
    let mut results = Vec::new();
    for message in &messages[at..] {
        for call in message["tool_calls"].as_array().into_iter().flatten() {
            names.insert(
                call["id"].as_str().unwrap().to_owned(),
                call["function"]["name"].as_str().unwrap().to_owned(),
            );
        }
        if message["role"] == "tool" {
            let id = message["tool_call_id"].as_str().unwrap();
            results.push((names[id].clone(), text_of(message)));
        }
    }
    Request {
        system: messages
            .iter()
            .filter(|message| message["role"] == "system")
            .map(text_of)
            .collect(),
        tools: value["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["function"]["name"].as_str().unwrap().to_owned())
            .collect(),
        maker,
        before: messages[..at]
            .iter()
            .filter(|message| message["role"] != "system")
            .map(text_of)
            .collect(),
        results,
        bytes: body.len(),
    }
}

/// A box that uses web numbers: `params` is the body of its `PARAMS`.
fn model(params: &str) -> Value {
    json!({"note": "first", "code": format!(
        "from build123d import *\n\nPARAMS = {{{params}}}\n\n\ndef build(p, ctx):\n    \
         return {{\"box\": Pos(0, 0, 20) * Box(100, 60, 40)}}\n"
    )})
}

fn web(name: &str, value: f64, note: &str) -> String {
    format!(
        "\"{name}\": {{\"default\": {value}, \"provenance\": \"web\", \"source\": \"{note}\"}},"
    )
}

fn record(key: &str, label: &str, value: f64, url: &str, quote: &str) -> Answer {
    Answer::Call(
        "record_source",
        json!({"key": key, "label": label, "value": value, "unit": "mm", "url": url,
               "quote": quote}),
    )
}

/// What a model that follows its instructions does next.
fn decide(request: &Request) -> Answer {
    let done = |name: &str| request.results.iter().filter(|(n, _)| n == name).count();
    let last = |name: &str| {
        request
            .results
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, text)| text.clone())
            .unwrap_or_default()
    };
    let offered = |name: &str| request.tools.iter().any(|tool| tool == name);
    let instructed = |rule: &str| {
        request
            .system
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .contains(rule)
    };
    // The rules of the section about examples, as this run was told them.
    let examples = offered("web_search")
        && instructed("- Examples. When the request is open-ended")
        && instructed("make ONE short search before you model");
    // What the instructions say to skip it for, in this conversation.
    let known = (request.before.contains("\"example") || request.before.contains(": example"))
        && instructed("when the notes hold a key starting with \"example\"");
    let sized = instructed("Skip it when the maker gave the sizes")
        && instructed("a plain part is asked for (a box 100x60x40)");
    let settled = instructed("when a component settles the matter");
    let look = || {
        Answer::Call(
            "web_search",
            json!({"query": "headphone holder desk design common complaints PLA wall thickness"}),
        )
    };
    match request.maker {
        OPEN_ENDED | AGAIN => {
            if examples && !known && done("web_search") == 0 {
                return look();
            }
            if done("web_search") == 1 && done("record_source") == 0 {
                return record(
                    "example_wall",
                    "Usual wall of a printed headphone holder (secondary source)",
                    3.0,
                    "https://blog.example/holders",
                    "Walls of 3 mm in PLA do not creep",
                );
            }
            if done("write_model_code") == 0 {
                return Answer::Call("write_model_code", model(""));
            }
            if done("web_search") == 0 {
                return Answer::Say(format!("A second holder like the first.\n{LOOKED}"));
            }
            assert!(last("record_source").starts_with("recorded as r1"));
            Answer::Say(format!(
                "A holder with a wide rounded saddle.\n{LOOKED}\n\nFrom examples: a saddle of \
                 30 mm or more spares the headband (blog.example), people complain about arms \
                 that creep (forum.example), and PLA with 3 mm walls holds (blog.example, a \
                 secondary page, read only as a search excerpt, unconfirmed).\n\nClamp or \
                 screws? I suggest screws."
            ))
        }
        WITH_SIZES => {
            if examples && !sized && done("web_search") == 0 {
                return look();
            }
            if done("write_model_code") == 0 {
                return Answer::Call("write_model_code", model(""));
            }
            Answer::Say(format!("The box is 100 x 60 x 40 mm.\n{LOOKED}"))
        }
        COMPONENT => {
            if done("list_components") == 0 {
                return Answer::Call("list_components", json!({}));
            }
            if examples && !settled && done("web_search") == 0 {
                return look();
            }
            assert!(last("list_components").contains("- sample_phone: Sample phone (invented)"));
            Answer::Say("I build it around Hotend's sample phone (invented numbers).".into())
        }
        FACT => {
            let page = "https://news.example/pi5";
            match request.results.len() {
                0 => Answer::Call("web_search", json!({"query": "Raspberry Pi 5 dimensions"})),
                1 => Answer::Call("web_fetch", json!({"url": page})),
                2 => record(
                    "board_length",
                    "Raspberry Pi 5 board length (secondary source)",
                    85.0,
                    page,
                    "The board is approximately 85 mm long",
                ),
                // Only a search excerpt of the maker's own page was read.
                3 => record(
                    "hole_spacing",
                    "Raspberry Pi 5 hole spacing",
                    58.0,
                    "https://maker.example/pi5.pdf",
                    "Mounting holes are 58 mm apart",
                ),
                4 => {
                    let notes: Vec<&str> = request
                        .results
                        .iter()
                        .filter(|(name, _)| name == "record_source")
                        .map(|(_, text)| text.as_str())
                        .collect();
                    assert!(
                        notes[0].contains("say so if that page is not the product maker's own:"),
                        "{notes:?}"
                    );
                    assert!(
                        notes[1].contains("or that only a search excerpt was read"),
                        "{notes:?}"
                    );
                    let params = web("board_length", 85.0, "r1") + &web("hole_spacing", 58.0, "r2");
                    Answer::Call("write_model_code", model(&params))
                }
                _ => Answer::Say(format!(
                    "A holder for the board.\n{LOOKED}\nBoard 85 mm long, from news.example, a \
                     secondary page, not the manufacturer's: unconfirmed. Holes 58 mm apart, \
                     from a search excerpt of maker.example; the drawing itself was not read: \
                     unconfirmed."
                )),
            }
        }
        GREEDY => match done("web_search") {
            n @ 0..4 => Answer::Call("web_search", json!({"query": format!("query {n}")})),
            _ => {
                assert!(
                    last("web_search").contains("research_cap_reached"),
                    "{}",
                    last("web_search")
                );
                Answer::Say("I used this message's searches.".into())
            }
        },
        SHARED_MODEL => {
            if done("web_fetch") == 0 {
                // The maker wrote this address; it is a model-sharing site all the same.
                return Answer::Call(
                    "web_fetch",
                    json!({"url": "https://www.printables.com/model/1-holder"}),
                );
            }
            let refused = last("web_fetch");
            assert!(refused.contains("url_not_allowed"), "{refused}");
            assert!(refused.contains("is a model-sharing site"), "{refused}");
            Answer::Say("I do not open model-sharing pages. What do you like about it?".into())
        }
        other => panic!("no answer for {other:?}"),
    }
}

/// One answer of the chat completions API as the provider streams it.
fn streamed(answer: Answer, serial: usize) -> String {
    let chunk = |delta: Value, finish: Value| {
        json!({"id": format!("chatcmpl-{serial}"), "object": "chat.completion.chunk",
               "created": 1, "model": "test",
               "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
    };
    let mut events = vec![chunk(
        json!({"role": "assistant", "content": ""}),
        Value::Null,
    )];
    let finish = match answer {
        Answer::Say(text) => {
            events.push(chunk(json!({"content": text}), Value::Null));
            "stop"
        }
        Answer::Call(name, args) => {
            events.push(chunk(
                json!({"tool_calls": [{"index": 0, "id": format!("call_{serial}"),
                       "type": "function",
                       "function": {"name": name, "arguments": args.to_string()}}]}),
                Value::Null,
            ));
            "tool_calls"
        }
    };
    events.push(chunk(json!({}), json!(finish)));
    events.push(
        json!({"id": format!("chatcmpl-{serial}"), "object": "chat.completion.chunk",
        "created": 1, "model": "test", "choices": [],
        "usage": {"prompt_tokens": 100, "completion_tokens": 40, "total_tokens": 140}}),
    );
    let mut out = String::new();
    for event in events {
        out.push_str(&format!("data: {event}\n\n"));
    }
    out + "data: [DONE]\n\n"
}

/// The stand-in model. Returns its origin and the size of every request it was sent.
async fn stand_in_model() -> (String, Arc<Mutex<Vec<usize>>>) {
    let sizes = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&sizes);
    let app = axum::Router::new().fallback(move |body: axum::body::Bytes| {
        let request = read(&body);
        let serial = {
            let mut seen = seen.lock().unwrap();
            seen.push(request.bytes);
            seen.len()
        };
        let text = streamed(decide(&request), serial);
        async move {
            axum::response::Response::builder()
                .header("content-type", "text/event-stream")
                .body(axum::body::Body::from(text))
                .unwrap()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        // The largest body the egress sends is 8 MB; axum's own limit is lower.
        let app = app.layer(axum::extract::DefaultBodyLimit::max(8 << 20));
        axum::serve(listener, app).await.unwrap();
    });
    (origin, sizes)
}

/// The stand-in search service: the longest excerpts a search gives, five results.
fn service() -> Replier {
    Arc::new(|seen: &Seen| {
        if seen.path.starts_with("/v1/search") {
            let pad = "People say the same in other words. ".repeat(10);
            let hit = |title: &str, url: &str, start: &str| json!({"title": title, "url": url, "snippet": format!("{start}. {pad}")});
            Reply::json(
                200,
                json!({"query": "q", "results": [
                    hit("Holders", "https://blog.example/holders",
                        "Walls of 3 mm in PLA do not creep"),
                    hit("Complaints", "https://forum.example/t/1", "The arm creeps in summer"),
                    hit("Pi 5", "https://news.example/pi5", "All about the new board"),
                    hit("Pi 5 drawing", "https://maker.example/pi5.pdf",
                        "Mounting holes are 58 mm apart"),
                    hit("Holder STL", "https://www.printables.com/model/1-holder", "Download"),
                ]}),
            )
        } else {
            Reply::json(
                200,
                json!({"url": "https://news.example/pi5", "title": "Pi 5",
                       "content": "The board is approximately 85 mm long and wide enough."}),
            )
        }
    })
}

struct Played {
    _tmp: tempfile::TempDir,
    core: Core,
    searches: Arc<Mutex<Vec<Seen>>>,
    sizes: Arc<Mutex<Vec<usize>>>,
}

async fn open(name: &str) -> Option<Played> {
    let runtime = sandboxed_python(name).await?;
    let tmp = tempfile::tempdir().unwrap();
    let (origin, sizes) = stand_in_model().await;
    let config = CoreConfig {
        data_dir: Some(tmp.path().to_path_buf()),
        agent: Some(AgentCommand::python_module(&runtime)),
        runtime: Some(runtime),
        ..CoreConfig::default()
    };
    let secrets = Arc::new(MemorySecretStore::default());
    let core = Core::open_with_upstream_for_tests(config, secrets, &origin)
        .await
        .unwrap();
    let (base, searches) = stand_in(service()).await;
    core.set_search_base_for_tests(&base);
    core.settings_set(Settings {
        provider: Some(ProviderSettings {
            kind: ProviderKind::Ollama,
            model: "stand-in".into(),
        }),
        ..Settings::default()
    })
    .await
    .unwrap();
    core.research_source_set(SearchSource::Keenable)
        .await
        .unwrap();
    core.research_consent_for(SearchSource::Keenable, true)
        .await
        .unwrap();
    Some(Played {
        _tmp: tmp,
        core,
        searches,
        sizes,
    })
}

/// What a run called, oldest first, and what it said.
struct Ran {
    calls: Vec<(String, bool)>,
    said: String,
}

impl Ran {
    fn names(&self) -> Vec<&str> {
        self.calls.iter().map(|(name, _)| name.as_str()).collect()
    }
}

impl Played {
    async fn project(&self, name: &str) -> String {
        self.core.project_create(name).await.unwrap().id
    }

    async fn say(&self, project: &str, message: &str) -> Ran {
        let mut run = self.core.message_send(project, message).await.unwrap();
        let mut ran = Ran {
            calls: Vec::new(),
            said: String::new(),
        };
        let mut last = None;
        tokio::time::timeout(Duration::from_secs(240), async {
            while let Some(event) = run.next().await {
                if let RunEvent::Agent { event } = &event {
                    match event["type"].as_str() {
                        Some("tool_end") if event["auto"] != true => ran.calls.push((
                            event["name"].as_str().unwrap().to_owned(),
                            event["ok"].as_bool().unwrap(),
                        )),
                        Some("text_delta") => {
                            ran.said.push_str(event["text"].as_str().unwrap_or(""));
                        }
                        _ => {}
                    }
                }
                last = Some(event);
            }
        })
        .await
        .expect("the run did not end");
        assert_eq!(
            last,
            Some(RunEvent::Finished {
                stop_reason: StopReason::Done,
                error: None
            }),
            "{message}: {}",
            ran.said
        );
        ran
    }

    /// The queries and pages the search service was asked for since the last call.
    fn asked(&self) -> (Vec<String>, usize) {
        let seen: Vec<Seen> = self.searches.lock().unwrap().drain(..).collect();
        let queries: Vec<String> = seen
            .iter()
            .filter(|s| s.path.starts_with("/v1/search"))
            .map(|s| {
                let body: Value = serde_json::from_str(&s.body).unwrap();
                body["query"].as_str().unwrap().to_owned()
            })
            .collect();
        let pages = seen.len() - queries.len();
        (queries, pages)
    }
}

#[tokio::test]
async fn an_open_ended_request_makes_one_example_search_and_a_plain_one_makes_none() {
    let name = "an_open_ended_request_makes_one_example_search_and_a_plain_one_makes_none";
    let Some(t) = open(name).await else {
        return;
    };
    // Open-ended: one search, before the model, no page; the answer names its sites and
    // asks one thing.
    let holder = t.project("holder").await;
    let ran = t.say(&holder, OPEN_ENDED).await;
    assert_eq!(
        ran.names(),
        ["web_search", "record_source", "write_model_code"]
    );
    assert!(ran.calls.iter().all(|(_, ok)| *ok), "{:?}", ran.calls);
    let (queries, pages) = t.asked();
    assert_eq!((queries.len(), pages), (1, 0), "{queries:?}");
    for site in ["blog.example", "forum.example"] {
        assert!(ran.said.contains(site), "{}", ran.said);
    }
    assert_eq!(ran.said.matches('?').count(), 1);
    let status = t.core.research_status(&holder).await.unwrap();
    assert_eq!((status.searches_used, status.fetches_used), (1, 0));
    let notes = t.core.research_notes(&holder).await.unwrap();
    assert_eq!(
        (notes[0].key.as_str(), notes[0].evidence.as_str()),
        ("example_wall", "citation")
    );
    // What the example search cost the conversation, for the estimate in WEB_RESEARCH.md.
    let sizes = t.sizes.lock().unwrap().clone();
    println!("request bytes of the open-ended message: {sizes:?}");
    assert!(
        sizes[1] - sizes[0] < 8_000,
        "a search of five results added {} bytes",
        sizes[1] - sizes[0]
    );

    // A later message of the project: the example is in the conversation, no new search.
    let ran = t.say(&holder, AGAIN).await;
    assert_eq!(ran.names(), ["write_model_code"]);
    assert_eq!(t.asked(), (vec![], 0));

    // Sizes given, and a device Hotend has: nothing is looked up.
    let plain = t.project("box").await;
    let ran = t.say(&plain, WITH_SIZES).await;
    assert_eq!(ran.names(), ["write_model_code"]);
    let phone = t.project("case").await;
    let ran = t.say(&phone, COMPONENT).await;
    assert_eq!(ran.names(), ["list_components"]);
    assert_eq!(t.asked(), (vec![], 0));
    for project in [&plain, &phone] {
        let status = t.core.research_status(project).await.unwrap();
        assert_eq!((status.searches_used, status.fetches_used), (0, 0));
    }

    // Research off for the project: the run has no web tool and is not told of examples,
    // so it searches nothing and says nothing about research being off.
    let off = t.project("off").await;
    t.core
        .research_set(
            &off,
            crate::research::ProjectResearch {
                enabled: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let ran = t.say(&off, OPEN_ENDED).await;
    assert_eq!(ran.names(), ["write_model_code"]);
    assert!(!ran.said.contains("web research is off"), "{}", ran.said);
    assert_eq!(t.asked(), (vec![], 0));
}

#[tokio::test]
async fn the_limits_hold_and_a_secondary_source_is_labelled_in_the_answer_and_stored() {
    let name = "the_limits_hold_and_a_secondary_source_is_labelled_in_the_answer_and_stored";
    let Some(t) = open(name).await else {
        return;
    };
    // The fourth search of a message is refused by the core and never sent.
    let greedy = t.project("greedy").await;
    let ran = t.say(&greedy, GREEDY).await;
    assert_eq!(ran.names(), ["web_search"; 4]);
    let oks: Vec<bool> = ran.calls.iter().map(|(_, ok)| *ok).collect();
    assert_eq!(oks, [true, true, true, false]);
    let (queries, _) = t.asked();
    assert_eq!(queries, ["query 0", "query 1", "query 2"]);

    // A page of a model-sharing site is refused before anything is sent, although the maker
    // wrote its address.
    let shared = t.project("shared").await;
    let ran = t.say(&shared, SHARED_MODEL).await;
    assert_eq!(ran.calls, [("web_fetch".to_owned(), false)]);
    assert_eq!(t.asked(), (vec![], 0));

    // A number from a secondary page, and one from an excerpt of the maker's own drawing.
    let pi = t.project("pi").await;
    let ran = t.say(&pi, FACT).await;
    assert!(ran.calls.iter().all(|(_, ok)| *ok), "{:?}", ran.calls);
    assert!(
        ran.said
            .contains("85 mm long, from news.example, a secondary page"),
        "{}",
        ran.said
    );
    assert!(ran.said.contains("from a search excerpt of maker.example"));
    let notes = t.core.research_notes(&pi).await.unwrap();
    let stored: Vec<(&str, &str, &str)> = notes
        .iter()
        .map(|n| (n.label.as_str(), n.evidence.as_str(), n.state.as_str()))
        .collect();
    assert_eq!(
        stored,
        [
            (
                "Raspberry Pi 5 board length (secondary source)",
                "page_text",
                "unconfirmed"
            ),
            ("Raspberry Pi 5 hole spacing", "citation", "unconfirmed"),
        ]
    );
    // What the maker is asked: the label and the site, and for an excerpt what Hotend
    // itself knows about it.
    let questions = t.core.questions_list(&pi, None).await.unwrap();
    let text = |note: &str| {
        questions
            .iter()
            .find(|q| q.requirement == note)
            .map(|q| q.text.clone())
            .unwrap()
    };
    let secondary = text("web:r1");
    assert!(
        secondary.contains("(secondary source)") && secondary.contains("(news.example)"),
        "{secondary}"
    );
    assert!(secondary.contains("Nobody has confirmed it"), "{secondary}");
    let excerpt = text("web:r2");
    assert!(
        excerpt.contains("has not been checked against the page itself"),
        "{excerpt}"
    );
    assert!(!secondary.contains("search excerpt"), "{secondary}");
}
