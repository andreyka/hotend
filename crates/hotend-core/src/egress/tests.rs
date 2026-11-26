//! Egress tests against a stand-in server on loopback inside the test process. No test
//! reaches a real provider.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::convert::Infallible;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap as AxumHeaders, Method, Response, Uri};

use super::*;
use crate::paths::DataDir;
use crate::secrets::MemorySecretStore;

pub(crate) const KEY: &str = "sk-live-7f3a9c2e41d8b6a0-hotend-test";

/// One request the stand-in server received.
#[derive(Debug, Clone)]
pub(crate) struct Seen {
    pub method: String,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

/// An answer the stand-in server gives instead of its usual one.
pub(crate) struct Reply {
    pub status: u16,
    pub content_type: &'static str,
    pub chunks: Vec<Vec<u8>>,
}

#[derive(Clone, Default)]
struct Shared {
    seen: Arc<Mutex<Vec<Seen>>>,
    replies: Arc<Mutex<std::collections::VecDeque<Reply>>>,
}

pub(crate) struct Upstream {
    pub origin: String,
    pub seen: Arc<Mutex<Vec<Seen>>>,
    replies: Arc<Mutex<std::collections::VecDeque<Reply>>>,
}

impl Upstream {
    pub(crate) fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    /// Makes the next request get `reply`, whatever its path.
    pub(crate) fn queue(&self, reply: Reply) {
        self.replies.lock().unwrap().push_back(reply);
    }
}

fn streamed(parts: Vec<Vec<u8>>) -> Body {
    let stream = futures_util::stream::unfold(parts.into_iter(), |mut parts| async move {
        let part = parts.next()?;
        tokio::time::sleep(Duration::from_millis(15)).await;
        Some((Ok::<_, Infallible>(Bytes::from(part)), parts))
    });
    Body::from_stream(stream)
}

async fn respond(
    State(shared): State<Shared>,
    method: Method,
    uri: Uri,
    headers: AxumHeaders,
    body: Bytes,
) -> Response<Body> {
    let headers: BTreeMap<String, String> = headers
        .iter()
        .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or("").to_owned()))
        .collect();
    let key = headers
        .get("x-api-key")
        .or_else(|| headers.get("authorization"))
        .cloned()
        .unwrap_or_default();
    shared.seen.lock().unwrap().push(Seen {
        method: method.to_string(),
        path: uri.to_string(),
        headers,
        body: body.to_vec(),
    });
    let response = Response::builder();
    if let Some(reply) = shared.replies.lock().unwrap().pop_front() {
        return response
            .status(reply.status)
            .header("content-type", reply.content_type)
            .body(streamed(reply.chunks))
            .unwrap();
    }
    match uri.path() {
        "/v1/messages" => response
            .header("content-type", "text/event-stream")
            .header("x-request-id", "req_123")
            .header("set-cookie", "session=upstream")
            .body(streamed(vec![
                b"data: one\n\n".to_vec(),
                b"data: two\n\n".to_vec(),
                b"data: three\n\n".to_vec(),
            ])),
        // Echoes the credential it received: split over two chunks and in a header.
        "/v1/messages/count_tokens" => {
            let (head, tail) = key.split_at(key.len() / 2);
            response
                .header("content-type", "application/json")
                .header("x-request-id", key.clone())
                .body(streamed(vec![
                    format!("{{\"you_sent\": \"{head}").into_bytes(),
                    format!("{tail}\", \"again\": \"{key}\"}}").into_bytes(),
                ]))
        }
        "/v1/chat/completions" => response
            .status(307)
            .header("location", "/elsewhere")
            .body(Body::from("moved")),
        "/v1/responses" => response.body(Body::from(vec![b'x'; 64 * 1024])),
        _ => response.status(404).body(Body::from("not found")),
    }
    .unwrap()
}

/// Starts the stand-in provider on a free loopback port.
pub(crate) async fn spawn_upstream() -> Upstream {
    let shared = Shared::default();
    let app = axum::Router::new()
        .fallback(respond)
        .with_state(shared.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Upstream {
        origin,
        seen: shared.seen,
        replies: shared.replies,
    }
}

pub(crate) struct Fixture {
    pub egress: Arc<Egress>,
    pub upstream: Upstream,
    pub store: Arc<Store>,
    pub secrets: Arc<MemorySecretStore>,
    _tmp: tempfile::TempDir,
}

pub(crate) async fn fixture(limits: EgressLimits) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let data = DataDir::resolve(Some(tmp.path())).unwrap();
    let store = Arc::new(Store::open(&data.db_path()).unwrap());
    let secrets = Arc::new(MemorySecretStore::default());
    for kind in [ProviderKind::Anthropic, ProviderKind::Openai] {
        secrets
            .set(&provider_key_name(kind), Secret::new(KEY))
            .unwrap();
    }
    let upstream = spawn_upstream().await;
    let egress = Arc::new(Egress::with_upstream_for_tests(
        secrets.clone(),
        Arc::clone(&store),
        limits,
        &upstream.origin,
    ));
    Fixture {
        egress,
        upstream,
        store,
        secrets,
        _tmp: tmp,
    }
}

pub(super) fn request(run: &str, token: &str, path: &str, body: &Value) -> LlmRequest {
    LlmRequest {
        run_id: run.to_owned(),
        request_id: "q1".to_owned(),
        token: token.to_owned(),
        method: "POST".to_owned(),
        path: path.to_owned(),
        headers: BTreeMap::from([("content-type".to_owned(), "application/json".to_owned())]),
        body_b64: BASE64.encode(body.to_string()),
    }
}

pub(super) fn small_body() -> Value {
    json!({"model": "m", "max_tokens": 1024, "messages": []})
}

/// Runs one request and returns its result with every event it produced.
pub(super) async fn send(
    egress: &Egress,
    request: LlmRequest,
) -> (Result<(), EgressError>, Vec<LlmEvent>) {
    let (tx, mut rx) = mpsc::channel(4);
    let collector = tokio::spawn(async move {
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        events
    });
    let result = egress.handle(request, &tx).await;
    drop(tx);
    (result, collector.await.unwrap())
}

pub(super) fn body_of(events: &[LlmEvent]) -> Vec<u8> {
    events
        .iter()
        .filter_map(|e| match e {
            LlmEvent::Chunk(bytes) => Some(bytes.clone()),
            LlmEvent::Start { .. } => None,
        })
        .flatten()
        .collect()
}

#[tokio::test]
async fn the_key_is_injected_and_agent_supplied_credentials_are_dropped() {
    let f = fixture(EgressLimits::default()).await;
    let grant = f
        .egress
        .begin_run("run", None, ProviderKind::Anthropic, "m", None)
        .await
        .unwrap();
    assert_eq!(grant.base_url, "https://api.anthropic.com");
    let mut req = request("run", &grant.token, "/v1/messages", &small_body());
    for (name, value) in [
        ("X-Api-Key", "agent-made-this-up"),
        ("Authorization", "Bearer agent-made-this-up"),
        ("Cookie", "a=b"),
        ("Host", "evil.example"),
        ("X-Forwarded-For", "10.0.0.1"),
        ("Proxy-Authorization", "Basic abc"),
        ("anthropic-version", "2023-06-01"),
        ("Accept", "text/event-stream"),
    ] {
        req.headers.insert(name.to_owned(), value.to_owned());
    }
    let (result, events) = send(&f.egress, req).await;
    result.unwrap();

    let seen = f.upstream.seen();
    assert_eq!(seen.len(), 1);
    let got = &seen[0];
    assert_eq!(
        (got.method.as_str(), got.path.as_str()),
        ("POST", "/v1/messages")
    );
    assert_eq!(got.headers["x-api-key"], KEY);
    assert_eq!(got.headers["anthropic-version"], "2023-06-01");
    assert_eq!(got.headers["accept"], "text/event-stream");
    assert_eq!(got.headers["content-type"], "application/json");
    assert!(got.headers["host"].starts_with("127.0.0.1:"));
    for dropped in [
        "authorization",
        "cookie",
        "x-forwarded-for",
        "proxy-authorization",
    ] {
        assert!(
            !got.headers.contains_key(dropped),
            "{dropped} was forwarded"
        );
    }
    assert_eq!(
        serde_json::from_slice::<Value>(&got.body).unwrap(),
        small_body()
    );

    let LlmEvent::Start { status, headers } = &events[0] else {
        panic!("the first event must be the response start");
    };
    assert_eq!(*status, 200);
    assert_eq!(headers["content-type"], "text/event-stream");
    assert_eq!(headers["x-request-id"], "req_123");
    assert!(!headers.contains_key("set-cookie"));
    assert_eq!(
        body_of(&events),
        b"data: one\n\ndata: two\n\ndata: three\n\n"
    );
    assert!(
        events.len() >= 3,
        "the body must arrive in pieces, got {events:?}"
    );
}

#[tokio::test]
async fn bearer_providers_get_an_authorization_header_and_redirects_are_not_followed() {
    let f = fixture(EgressLimits::default()).await;
    let grant = f
        .egress
        .begin_run("run", None, ProviderKind::Openai, "m", None)
        .await
        .unwrap();
    let body = json!({"model": "m", "max_completion_tokens": 100});
    let (result, events) = send(
        &f.egress,
        request("run", &grant.token, "/v1/chat/completions", &body),
    )
    .await;
    result.unwrap();
    let seen = f.upstream.seen();
    assert_eq!(seen.len(), 1, "the redirect must not be followed: {seen:?}");
    assert_eq!(seen[0].headers["authorization"], format!("Bearer {KEY}"));
    assert!(!seen[0].headers.contains_key("x-api-key"));
    assert!(matches!(events[0], LlmEvent::Start { status: 307, .. }));
}

#[tokio::test]
async fn hosts_paths_and_methods_outside_the_table_are_refused() {
    let f = fixture(EgressLimits::default()).await;
    let grant = f
        .egress
        .begin_run("run", None, ProviderKind::Anthropic, "m", None)
        .await
        .unwrap();
    for path in [
        "/v1/files",
        "/v1/messages/batches",
        "/v1/messages/../models",
        "/v1/chat/completions",
        "https://evil.example/v1/messages",
        "//evil.example/v1/messages",
        "@evil.example/v1/messages",
        "/v1/messages#@evil.example",
        "/v1/messages?redirect=https://evil.example",
        "",
    ] {
        let (result, events) =
            send(&f.egress, request("run", &grant.token, path, &small_body())).await;
        assert!(
            matches!(result, Err(EgressError::Denied { .. })),
            "{path:?}: {result:?}"
        );
        assert!(events.is_empty());
    }
    for method in ["GET", "DELETE", "PUT", "CONNECT"] {
        let mut req = request("run", &grant.token, "/v1/messages", &small_body());
        req.method = method.to_owned();
        let (result, _) = send(&f.egress, req).await;
        assert!(
            matches!(result, Err(EgressError::Denied { .. })),
            "{method}"
        );
    }
    assert!(
        f.upstream.seen().is_empty(),
        "a refused request reached the network"
    );

    // The request format has no field for a host or a URL at all.
    let with_url = json!({"run_id": "run", "request_id": "q", "token": grant.token,
        "method": "POST", "path": "/v1/messages", "base_url": "https://evil.example"});
    assert!(serde_json::from_value::<LlmRequest>(with_url).is_err());
}

#[tokio::test]
async fn a_request_needs_an_open_run_and_its_token() {
    let f = fixture(EgressLimits::default()).await;
    let grant = f
        .egress
        .begin_run("run", None, ProviderKind::Anthropic, "m", None)
        .await
        .unwrap();
    let other = f
        .egress
        .begin_run("other", None, ProviderKind::Anthropic, "m", None)
        .await
        .unwrap();
    assert_ne!(grant.token, other.token);
    assert_eq!(grant.token.len(), 64);
    for (run, token) in [
        ("run", ""),
        ("run", "0000"),
        ("run", other.token.as_str()),
        ("nope", grant.token.as_str()),
    ] {
        let (result, _) = send(
            &f.egress,
            request(run, token, "/v1/messages", &small_body()),
        )
        .await;
        assert!(
            matches!(result, Err(EgressError::NotAuthorised)),
            "{run} {token}"
        );
    }
    assert_eq!(f.egress.end_run("run").requests, 0);
    let (result, _) = send(
        &f.egress,
        request("run", &grant.token, "/v1/messages", &small_body()),
    )
    .await;
    assert!(matches!(result, Err(EgressError::NotAuthorised)));
    assert!(f.upstream.seen().is_empty());
}

#[tokio::test]
async fn the_request_count_and_byte_caps_hold() {
    let limits = EgressLimits {
        max_requests: 4,
        max_request_bytes: 4096,
        max_total_request_bytes: 6000,
        ..EgressLimits::default()
    };
    let f = fixture(limits).await;
    let grant = f
        .egress
        .begin_run("run", Some("p1"), ProviderKind::Anthropic, "m", None)
        .await
        .unwrap();
    let sized = |bytes: usize| json!({"model": "m", "max_tokens": 10, "system": "p".repeat(bytes)});

    let (too_big, _) = send(
        &f.egress,
        request("run", &grant.token, "/v1/messages", &sized(5000)),
    )
    .await;
    assert!(matches!(
        too_big,
        Err(EgressError::CapExceeded {
            what: "bytes per request",
            ..
        })
    ));

    let (first, _) = send(
        &f.egress,
        request("run", &grant.token, "/v1/messages", &sized(3500)),
    )
    .await;
    first.unwrap();
    let (over_total, _) = send(
        &f.egress,
        request("run", &grant.token, "/v1/messages", &sized(3500)),
    )
    .await;
    assert!(matches!(
        over_total,
        Err(EgressError::CapExceeded {
            what: "request bytes",
            ..
        })
    ));
    let (second, _) = send(
        &f.egress,
        request("run", &grant.token, "/v1/messages", &sized(10)),
    )
    .await;
    second.unwrap();
    // Two requests were sent and two were refused: the cap of four is used up.
    for _ in 0..5 {
        let (over, _) = send(
            &f.egress,
            request("run", &grant.token, "/v1/messages", &sized(10)),
        )
        .await;
        assert!(matches!(
            over,
            Err(EgressError::CapExceeded {
                what: "requests",
                limit: 4
            })
        ));
    }

    assert_eq!(f.upstream.seen().len(), 2);
    let totals = f.egress.end_run("run");
    assert_eq!((totals.requests, totals.refused), (2, 7));
    assert!(totals.request_bytes > 3500 && totals.response_bytes > 0);
    // Requests over the cap are audited once, not once each.
    let audit = f.store.list_audit(Some("p1"), 100).unwrap();
    assert_eq!(audit.len(), 5, "{audit:?}");
}

/// A request body one of the pinned provider SDKs really sends, as recorded in
/// `tests/fixtures/sdk_requests.json`.
fn sdk_request(provider: &str) -> (String, String, BTreeMap<String, String>, Value) {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sdk_requests.json");
    let all: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let one = &all[provider];
    let headers = one["headers"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(name, value)| (name.clone(), value.as_str().unwrap().to_owned()))
        .collect();
    (
        one["model"].as_str().unwrap().to_owned(),
        one["path"].as_str().unwrap().to_owned(),
        headers,
        one["body"].clone(),
    )
}

#[tokio::test]
async fn requests_of_the_pinned_sdks_pass_the_body_rules_unchanged() {
    let f = fixture(EgressLimits::default()).await;
    f.secrets
        .set(&provider_key_name(ProviderKind::Google), Secret::new(KEY))
        .unwrap();
    f.secrets
        .set(
            &provider_key_name(ProviderKind::Openrouter),
            Secret::new(KEY),
        )
        .unwrap();
    for provider in ProviderKind::ALL {
        let (model, path, headers, body) = sdk_request(provider.as_str());
        let grant = f
            .egress
            .begin_run("run", None, provider, &model, None)
            .await
            .unwrap();
        let mut req = request("run", &grant.token, &path, &body);
        req.headers = headers;
        let before = f.upstream.seen().len();
        let (result, _) = send(&f.egress, req).await;
        result.unwrap_or_else(|e| panic!("{}: {e}", provider.as_str()));
        let seen = f.upstream.seen();
        assert_eq!(seen.len(), before + 1, "{}", provider.as_str());
        assert_eq!(
            serde_json::from_slice::<Value>(&seen[before].body).unwrap(),
            body,
            "{}: the provider gets what the SDK wrote",
            provider.as_str()
        );
        f.egress.end_run("run");
    }
}

#[tokio::test]
async fn a_run_may_only_ask_for_its_own_model() {
    let f = fixture(EgressLimits::default()).await;
    f.secrets
        .set(&provider_key_name(ProviderKind::Google), Secret::new(KEY))
        .unwrap();
    let grant = f
        .egress
        .begin_run("run", None, ProviderKind::Anthropic, "claude-small", None)
        .await
        .unwrap();
    for body in [
        json!({"model": "claude-largest", "max_tokens": 10}),
        json!({"max_tokens": 10}),
        json!({"model": ["claude-small"], "max_tokens": 10}),
        json!({"model": "claude-small ", "max_tokens": 10}),
    ] {
        for path in ["/v1/messages", "/v1/messages/count_tokens"] {
            let (result, _) = send(&f.egress, request("run", &grant.token, path, &body)).await;
            let error = result.unwrap_err();
            assert!(matches!(error, EgressError::Denied { .. }), "{body}");
            assert!(error.to_string().contains("this run uses `claude-small`"));
        }
    }
    let own = json!({"model": "claude-small", "max_tokens": 10});
    send(
        &f.egress,
        request("run", &grant.token, "/v1/messages", &own),
    )
    .await
    .0
    .unwrap();

    // Google names the model in the path.
    let grant = f
        .egress
        .begin_run("g", None, ProviderKind::Google, "gemini-small", None)
        .await
        .unwrap();
    let body = json!({"contents": [], "generationConfig": {"maxOutputTokens": 10}});
    let (result, _) = send(
        &f.egress,
        request(
            "g",
            &grant.token,
            "/v1beta/models/gemini-largest:generateContent",
            &body,
        ),
    )
    .await;
    assert!(matches!(result, Err(EgressError::Denied { .. })));
    send(
        &f.egress,
        request(
            "g",
            &grant.token,
            "/v1beta/models/gemini-small:generateContent",
            &body,
        ),
    )
    .await
    .0
    .unwrap();
    assert_eq!(f.upstream.seen().len(), 2);
}

#[tokio::test]
async fn provider_side_tools_stored_requests_and_output_multipliers_are_refused() {
    let f = fixture(EgressLimits::default()).await;
    for kind in [ProviderKind::Google, ProviderKind::Openrouter] {
        f.secrets
            .set(&provider_key_name(kind), Secret::new(KEY))
            .unwrap();
    }
    let function = json!({"type": "function", "function": {"name": "list_cards"}});
    let cases: Vec<(ProviderKind, &str, Value)> = vec![
        (
            ProviderKind::Anthropic,
            "/v1/messages",
            json!({"max_tokens": 10, "tools": [{"type": "web_search_20250305", "name": "web_search"}]}),
        ),
        (
            ProviderKind::Anthropic,
            "/v1/messages",
            json!({"max_tokens": 10, "tools": [{"name": "ok", "input_schema": {}},
                {"type": "code_execution_20250825", "name": "code_execution"}]}),
        ),
        (
            ProviderKind::Anthropic,
            "/v1/messages",
            json!({"max_tokens": 10, "tools": [{"type": "web_fetch_20250910", "name": "web_fetch"}]}),
        ),
        (
            ProviderKind::Anthropic,
            "/v1/messages",
            json!({"max_tokens": 10, "mcp_servers": [{"type": "url", "url": "https://evil.example"}]}),
        ),
        (
            ProviderKind::Anthropic,
            "/v1/messages",
            json!({"max_tokens": 10, "tools": "web_search"}),
        ),
        (
            ProviderKind::Anthropic,
            "/v1/messages",
            json!({"max_tokens": 10, "tools": ["web_search"]}),
        ),
        (
            ProviderKind::Openai,
            "/v1/responses",
            json!({"max_output_tokens": 10, "tools": [{"type": "web_search"}]}),
        ),
        (
            ProviderKind::Openai,
            "/v1/responses",
            json!({"max_output_tokens": 10,
                "tools": [{"type": "mcp", "server_url": "https://evil.example"}]}),
        ),
        (
            ProviderKind::Openai,
            "/v1/responses",
            json!({"max_output_tokens": 10, "tools": [{"type": "code_interpreter"}]}),
        ),
        (
            ProviderKind::Openai,
            "/v1/responses",
            json!({"max_output_tokens": 10, "prompt": {"id": "pmpt_1"}}),
        ),
        (
            ProviderKind::Openai,
            "/v1/chat/completions",
            json!({"max_completion_tokens": 10, "n": 128}),
        ),
        (
            ProviderKind::Openai,
            "/v1/chat/completions",
            json!({"max_completion_tokens": 10, "n": "128"}),
        ),
        (
            ProviderKind::Openai,
            "/v1/chat/completions",
            json!({"max_completion_tokens": 10, "web_search_options": {}}),
        ),
        (
            ProviderKind::Openai,
            "/v1/chat/completions",
            json!({"max_completion_tokens": 10, "tools": [function, {"name": "untyped"}]}),
        ),
        (
            ProviderKind::Openrouter,
            "/api/v1/chat/completions",
            json!({"max_tokens": 10, "plugins": [{"id": "web"}]}),
        ),
        (
            ProviderKind::Openrouter,
            "/api/v1/chat/completions",
            json!({"max_tokens": 10, "models": ["m", "the-dearest"]}),
        ),
        (
            ProviderKind::Google,
            "/v1beta/models/m:generateContent",
            json!({"generationConfig": {"maxOutputTokens": 10}, "tools": [{"googleSearch": {}}]}),
        ),
        (
            ProviderKind::Google,
            "/v1beta/models/m:generateContent",
            json!({"generationConfig": {"maxOutputTokens": 10},
                "tools": [{"functionDeclarations": [], "codeExecution": {}}]}),
        ),
        (
            ProviderKind::Google,
            "/v1beta/models/m:generateContent",
            json!({"generationConfig": {"maxOutputTokens": 10, "candidateCount": 8}}),
        ),
        // The snake_case spelling the API also accepts would state the limits a second time.
        (
            ProviderKind::Google,
            "/v1beta/models/m:generateContent",
            json!({"generationConfig": {"maxOutputTokens": 10},
                "generation_config": {"maxOutputTokens": 999_999}}),
        ),
        (
            ProviderKind::Google,
            "/v1beta/models/m:generateContent",
            json!({"generationConfig": {"maxOutputTokens": 10, "candidate_count": 8}}),
        ),
        (
            ProviderKind::Google,
            "/v1beta/models/m:generateContent",
            json!({"generationConfig": {"maxOutputTokens": 10}, "cachedContent": "cachedContents/1"}),
        ),
    ];
    let sink = Arc::new(super::research_tests::Recorder::default());
    for (index, (provider, path, mut body)) in cases.into_iter().enumerate() {
        if provider.rule().body.model == ModelPlace::Body {
            body["model"] = json!("m");
        }
        // Without web research and with it: a grant opens nothing in this list. (The two
        // bare web tool entries are refused with a grant as well, because a research
        // request must use the prompt cache; `research_tests.rs` shows the one form a
        // grant lets through, and that the entries sent are then written by the egress.)
        for research in [false, true] {
            let run = format!("run{index}{research}");
            let research = research.then(|| super::research_tests::grant(&sink));
            let grant = f
                .egress
                .begin_run(&run, None, provider, "m", research)
                .await
                .unwrap();
            let (result, events) = send(&f.egress, request(&run, &grant.token, path, &body)).await;
            assert!(
                matches!(
                    result,
                    Err(EgressError::Denied { .. } | EgressError::Invalid(_))
                ),
                "{} {body}: {result:?}",
                provider.as_str()
            );
            assert!(events.is_empty());
            assert_eq!(f.egress.end_run(&run).refused, 1);
        }
    }
    assert!(sink.lines().is_empty());
    assert!(
        f.upstream.seen().is_empty(),
        "a refused request reached the network"
    );

    // What the agent's own tools look like goes through.
    let grant = f
        .egress
        .begin_run("ok", None, ProviderKind::Openai, "m", None)
        .await
        .unwrap();
    let body = json!({"model": "m", "max_completion_tokens": 10, "n": 1, "tools": [function]});
    send(
        &f.egress,
        request("ok", &grant.token, "/v1/chat/completions", &body),
    )
    .await
    .0
    .unwrap();
}

#[tokio::test]
async fn the_provider_gets_the_body_that_was_checked() {
    let f = fixture(EgressLimits::default()).await;
    let grant = f
        .egress
        .begin_run("run", None, ProviderKind::Anthropic, "m", None)
        .await
        .unwrap();
    // A key given twice: a parser that keeps the first value and one that keeps the last
    // disagree about what this body asks for.
    let twice = r#"{"model": "the-dearest", "max_tokens": 999999, "model": "m", "max_tokens": 10}"#;
    let mut req = request("run", &grant.token, "/v1/messages", &small_body());
    req.body_b64 = BASE64.encode(twice);
    send(&f.egress, req).await.0.unwrap();
    let sent = String::from_utf8(f.upstream.seen()[0].body.clone()).unwrap();
    assert!(!sent.contains("the-dearest"), "{sent}");
    assert!(!sent.contains("999999"), "{sent}");
    assert_eq!(
        serde_json::from_str::<Value>(&sent).unwrap(),
        json!({"model": "m", "max_tokens": 10})
    );
}

#[tokio::test]
async fn only_listed_beta_features_are_passed_on() {
    let f = fixture(EgressLimits::default()).await;
    let grant = f
        .egress
        .begin_run("run", None, ProviderKind::Anthropic, "m", None)
        .await
        .unwrap();
    let mut req = request("run", &grant.token, "/v1/messages", &small_body());
    req.headers.insert(
        "anthropic-beta".into(),
        "mcp-client-2025-04-04, interleaved-thinking-2025-05-14,code-execution-2025-08-25,         web-fetch-2025-09-10, files-api-2025-04-14, thinking-binding-controls-2026-08-01,         web-search-2025-03-05, computer-use-2025-01-24"
            .into(),
    );
    send(&f.egress, req).await.0.unwrap();
    let mut req = request("run", &grant.token, "/v1/messages", &small_body());
    req.headers
        .insert("Anthropic-Beta".into(), "mcp-client-2025-04-04".into());
    send(&f.egress, req).await.0.unwrap();
    let seen = f.upstream.seen();
    // Features that change how the model answers pass; nothing that names a tool does,
    // research or not: the web tools need no feature header.
    assert_eq!(
        seen[0].headers["anthropic-beta"],
        "interleaved-thinking-2025-05-14,thinking-binding-controls-2026-08-01"
    );
    assert!(!seen[1].headers.contains_key("anthropic-beta"));

    // OpenAI's beta header is not passed at all.
    let grant = f
        .egress
        .begin_run("oa", None, ProviderKind::Openai, "m", None)
        .await
        .unwrap();
    let body = json!({"model": "m", "max_output_tokens": 10});
    let mut req = request("oa", &grant.token, "/v1/responses", &body);
    req.headers
        .insert("openai-beta".into(), "assistants=v2".into());
    send(&f.egress, req).await.0.unwrap();
    assert!(!f.upstream.seen()[2].headers.contains_key("openai-beta"));
}

#[tokio::test]
async fn the_output_token_ceiling_holds() {
    let limits = EgressLimits {
        max_output_tokens: 2000,
        ..EgressLimits::default()
    };
    let f = fixture(limits).await;
    let grant = f
        .egress
        .begin_run("run", None, ProviderKind::Anthropic, "m", None)
        .await
        .unwrap();
    let cases = [
        (json!({"model": "m", "max_tokens": 2001}), "ceiling"),
        (json!({"model": "m", "max_tokens": 1e12}), "whole number"),
        (
            json!({"model": "m", "max_tokens": "all of them"}),
            "whole number",
        ),
        (json!({"model": "m", "max_tokens": -1}), "whole number"),
        (json!({"model": "m"}), "sets no output token limit"),
        (json!(["not", "an", "object"]), "JSON object"),
    ];
    for (body, expected) in cases {
        let (result, _) = send(
            &f.egress,
            request("run", &grant.token, "/v1/messages", &body),
        )
        .await;
        let message = result.unwrap_err().to_string();
        assert!(message.contains(expected), "{body}: {message}");
    }
    let mut not_json = request("run", &grant.token, "/v1/messages", &small_body());
    not_json.body_b64 = BASE64.encode("max_tokens=1");
    assert!(send(&f.egress, not_json).await.0.is_err());
    assert!(f.upstream.seen().is_empty());

    let at_ceiling = json!({"model": "m", "max_tokens": 2000});
    send(
        &f.egress,
        request("run", &grant.token, "/v1/messages", &at_ceiling),
    )
    .await
    .0
    .unwrap();

    // OpenAI: every known field is checked, not only the first one present.
    let grant = f
        .egress
        .begin_run("oa", None, ProviderKind::Openai, "m", None)
        .await
        .unwrap();
    let sneaky = json!({"model": "m", "max_tokens": 10, "max_output_tokens": 999_999});
    let (result, _) = send(
        &f.egress,
        request("oa", &grant.token, "/v1/responses", &sneaky),
    )
    .await;
    assert!(matches!(result, Err(EgressError::Denied { .. })));
    // Google: the limit is nested.
    let rule = ProviderKind::Google.rule();
    let nested = json!({"generationConfig": {"maxOutputTokens": 5000}});
    assert!(f.egress.check_token_ceiling(rule, &nested).is_err());
    let nested = json!({"generationConfig": {"maxOutputTokens": 500}});
    assert_eq!(f.egress.check_token_ceiling(rule, &nested).unwrap(), 500);
}

#[tokio::test]
async fn the_response_byte_cap_holds_and_chunks_are_bounded() {
    let limits = EgressLimits {
        max_response_bytes: 100_000,
        chunk_bytes: 1000,
        ..EgressLimits::default()
    };
    let f = fixture(limits).await;
    let grant = f
        .egress
        .begin_run("run", None, ProviderKind::Openai, "m", None)
        .await
        .unwrap();
    let body = json!({"model": "m", "max_output_tokens": 10});
    let (first, events) = send(
        &f.egress,
        request("run", &grant.token, "/v1/responses", &body),
    )
    .await;
    first.unwrap();
    assert_eq!(body_of(&events).len(), 64 * 1024);
    for event in &events {
        if let LlmEvent::Chunk(bytes) = event {
            assert!(bytes.len() <= 1000);
        }
    }
    let (second, _) = send(
        &f.egress,
        request("run", &grant.token, "/v1/responses", &body),
    )
    .await;
    assert!(matches!(
        second,
        Err(EgressError::CapExceeded {
            what: "response bytes",
            ..
        })
    ));
    let (third, _) = send(
        &f.egress,
        request("run", &grant.token, "/v1/responses", &body),
    )
    .await;
    assert!(matches!(
        third,
        Err(EgressError::CapExceeded {
            what: "response bytes",
            ..
        })
    ));
    assert_eq!(f.upstream.seen().len(), 2);
}

#[tokio::test]
async fn the_key_never_reaches_events_errors_or_the_audit_log() {
    let f = fixture(EgressLimits::default()).await;
    let grant = f
        .egress
        .begin_run("run", Some("p1"), ProviderKind::Anthropic, "m", None)
        .await
        .unwrap();
    let mut everything = String::new();

    // An upstream that echoes the credential in a header and across two body chunks.
    let (result, events) = send(
        &f.egress,
        request(
            "run",
            &grant.token,
            "/v1/messages/count_tokens",
            &json!({"model": "m", "messages": []}),
        ),
    )
    .await;
    result.unwrap();
    assert_eq!(f.upstream.seen()[0].headers["x-api-key"], KEY);
    let body = String::from_utf8(body_of(&events)).unwrap();
    assert_eq!(
        body, "{\"you_sent\": \"[redacted]\", \"again\": \"[redacted]\"}",
        "the echoed key must be scrubbed, also across a chunk boundary"
    );
    everything.push_str(&format!("{events:?}"));

    // Failures of every kind.
    let mut bad_header = request("run", &grant.token, "/v1/messages", &small_body());
    bad_header
        .headers
        .insert("x-api-key".into(), "agent".into());
    let failures = [
        request("run", "wrong-token", "/v1/messages", &small_body()),
        request("run", &grant.token, "/v1/secrets", &small_body()),
        request(
            "run",
            &grant.token,
            "/v1/messages",
            &json!({"model": "m", "max_tokens": 99_999_999}),
        ),
    ];
    for failing in failures {
        let (result, events) = send(&f.egress, failing).await;
        everything.push_str(&format!(
            "{:?} {} {events:?}",
            result,
            result.as_ref().unwrap_err()
        ));
    }
    // A provider that cannot be reached.
    let dead = Egress::with_upstream_for_tests(
        f.secrets.clone(),
        Arc::clone(&f.store),
        EgressLimits::default(),
        "http://127.0.0.1:9",
    );
    let dead_grant = dead
        .begin_run("dead", Some("p1"), ProviderKind::Anthropic, "m", None)
        .await
        .unwrap();
    let (result, _) = send(
        &dead,
        request("dead", &dead_grant.token, "/v1/messages", &small_body()),
    )
    .await;
    let error = result.unwrap_err();
    assert!(matches!(error, EgressError::Upstream(_)), "{error:?}");
    everything.push_str(&format!("{error:?} {error}"));

    everything.push_str(&format!("{:?} {:?}", f.egress, f.secrets));
    let audit = f.store.list_audit(None, 100).unwrap();
    assert!(audit.len() >= 5, "every request must be audited: {audit:?}");
    assert!(audit.iter().all(|entry| entry.kind == "egress"));
    assert!(
        audit
            .iter()
            .any(|e| e.outcome == "ok" && e.detail["status"] == 200)
    );
    assert!(audit.iter().any(|e| e.outcome == "denied"));
    assert!(audit.iter().any(|e| e.outcome == "not_authorised"));
    assert!(audit.iter().any(|e| e.outcome == "upstream_error"));
    everything.push_str(&serde_json::to_string(&audit).unwrap());

    assert!(!everything.contains(KEY), "the key leaked: {everything}");
    assert!(!everything.contains(&KEY[..12]), "part of the key leaked");
    assert!(
        !everything.contains(&grant.token),
        "the run token leaked into logs"
    );
}

#[tokio::test]
async fn a_missing_key_is_reported_before_any_request_and_ollama_needs_none() {
    let f = fixture(EgressLimits::default()).await;
    let error = f
        .egress
        .begin_run("run", None, ProviderKind::Google, "m", None)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        EgressError::NoCredential { provider: "google" }
    ));
    assert!(error.to_string().contains("settings"));

    // A key deleted in the middle of a run stops the next request.
    let grant = f
        .egress
        .begin_run("run", None, ProviderKind::Anthropic, "m", None)
        .await
        .unwrap();
    f.secrets
        .delete(&provider_key_name(ProviderKind::Anthropic))
        .unwrap();
    let (result, _) = send(
        &f.egress,
        request("run", &grant.token, "/v1/messages", &small_body()),
    )
    .await;
    assert!(matches!(result, Err(EgressError::NoCredential { .. })));
    assert!(f.upstream.seen().is_empty());

    let grant = f
        .egress
        .begin_run("local", None, ProviderKind::Ollama, "m", None)
        .await
        .unwrap();
    let mut req = request("local", &grant.token, "/api/chat", &json!({"model": "m"}));
    req.headers
        .insert("authorization".into(), "Bearer agent".into());
    let (result, events) = send(&f.egress, req).await;
    result.unwrap();
    assert!(matches!(events[0], LlmEvent::Start { status: 404, .. }));
    let seen = f.upstream.seen();
    assert!(!seen[0].headers.contains_key("authorization"));
    assert!(!seen[0].headers.contains_key("x-api-key"));
}

/// A stand-in proxy: records the first line of every request it is sent and answers 502.
async fn spawn_proxy() -> (String, Arc<Mutex<Vec<String>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let lines = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&lines);
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut head = vec![0u8; 4096];
            let read = socket.read(&mut head).await.unwrap_or(0);
            let text = String::from_utf8_lossy(&head[..read]).into_owned();
            seen.lock()
                .unwrap()
                .push(text.lines().next().unwrap_or("").to_owned());
            let _ = socket
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\n\r\n")
                .await;
        }
    });
    (address, lines)
}

/// A provider on this computer (Ollama) is reached directly whatever proxy is set; a
/// provider elsewhere goes through the proxy.
#[tokio::test]
async fn only_a_provider_elsewhere_goes_through_a_proxy() {
    let f = fixture(EgressLimits::default()).await;
    let (proxy, through) = spawn_proxy().await;
    let local = Egress::with_proxy_for_tests(
        f.secrets.clone(),
        Arc::clone(&f.store),
        &f.upstream.origin,
        &proxy,
    );
    let grant = local
        .begin_run("local", None, ProviderKind::Ollama, "m", None)
        .await
        .unwrap();
    let req = request("local", &grant.token, "/api/chat", &json!({"model": "m"}));
    let (result, _) = send(&local, req).await;
    result.unwrap();
    assert_eq!(f.upstream.seen().len(), 1);
    assert!(through.lock().unwrap().is_empty());

    let elsewhere = Egress::with_proxy_for_tests(
        f.secrets.clone(),
        Arc::clone(&f.store),
        "http://provider.invalid",
        &proxy,
    );
    let grant = elsewhere
        .begin_run("far", None, ProviderKind::Ollama, "m", None)
        .await
        .unwrap();
    let req = request("far", &grant.token, "/api/chat", &json!({"model": "m"}));
    let _ = send(&elsewhere, req).await;
    assert_eq!(
        through.lock().unwrap().as_slice(),
        ["POST http://provider.invalid/api/chat HTTP/1.1"]
    );
    assert_eq!(f.upstream.seen().len(), 1);
}

#[test]
fn loopback_is_localhost_and_the_loopback_addresses() {
    for (address, local) in [
        ("http://127.0.0.1:11434/v1", true),
        ("http://127.8.0.1/", true),
        ("http://localhost:1/", true),
        ("http://[::1]:1/", true),
        ("https://api.anthropic.com/v1", false),
        ("http://10.0.0.1/", false),
        ("http://localhost.example/", false),
    ] {
        assert_eq!(
            is_loopback(&reqwest::Url::parse(address).unwrap()),
            local,
            "{address}"
        );
    }
}

#[test]
fn the_redactor_finds_the_key_at_every_split_point() {
    let text = format!("start {KEY} middle {KEY}{KEY} end sk-live-7f3a");
    let expected = "start [redacted] middle [redacted][redacted] end sk-live-7f3a";
    for split in 0..=text.len() {
        let mut redactor = Redactor::new(KEY);
        let mut out = redactor.push(&text.as_bytes()[..split]);
        out.extend(redactor.push(&text.as_bytes()[split..]));
        out.extend(redactor.finish());
        assert_eq!(
            String::from_utf8(out).unwrap(),
            expected,
            "split at {split}"
        );
    }
    // One byte at a time.
    let mut redactor = Redactor::new(KEY);
    let mut out = Vec::new();
    for byte in text.as_bytes() {
        out.extend(redactor.push(&[*byte]));
    }
    out.extend(redactor.finish());
    assert_eq!(String::from_utf8(out).unwrap(), expected);
    // Nothing is held back when the data cannot be the start of the key.
    let mut redactor = Redactor::new(KEY);
    assert_eq!(redactor.push(b"data: hello\n\n"), b"data: hello\n\n");
    assert_eq!(Redactor::new("").push(b"abc"), b"abc");
}

#[test]
fn token_comparison_is_exact() {
    assert!(constant_time_eq(b"abc", b"abc"));
    assert!(!constant_time_eq(b"abc", b"abd"));
    assert!(!constant_time_eq(b"abc", b"abcd"));
    assert!(!constant_time_eq(b"", b"a"));
}

/// The body the upstream saw for each request, as JSON.
fn seen_bodies(f: &Fixture) -> Vec<Value> {
    f.upstream
        .seen()
        .iter()
        .map(|seen| serde_json::from_slice(&seen.body).unwrap())
        .collect()
}

#[tokio::test]
async fn the_effort_level_is_written_only_for_a_model_documented_to_take_it() {
    let f = fixture(EgressLimits::default()).await;
    let body = |model: &str| json!({"model": model, "max_tokens": 1024, "messages": []});

    // The default model of a new installation with the default level.
    let grant = f
        .egress
        .begin_run_with_effort(
            "fable",
            None,
            ProviderKind::Anthropic,
            "claude-fable-5-2",
            None,
            Some(crate::egress::Effort::High),
        )
        .await
        .unwrap();
    let req = request(
        "fable",
        &grant.token,
        "/v1/messages",
        &body("claude-fable-5-2"),
    );
    send(&f.egress, req).await.0.unwrap();

    // Another field of output_config survives, and the agent cannot choose the level.
    let mut with_config = body("claude-fable-5-2");
    with_config["output_config"] =
        json!({"effort": "low", "format": {"type": "json_schema", "schema": {}}});
    let req = request("fable", &grant.token, "/v1/messages", &with_config);
    send(&f.egress, req).await.0.unwrap();

    // Token counting does not generate: no effort field is added, and one the agent wrote
    // is removed.
    let req = request(
        "fable",
        &grant.token,
        "/v1/messages/count_tokens",
        &body("claude-fable-5-2"),
    );
    send(&f.egress, req).await.0.unwrap();
    let agent_effort = |model: &str| {
        let mut body = body(model);
        body["output_config"] =
            json!({"effort": "max", "format": {"type": "json_schema", "schema": {}}});
        body
    };
    let req = request(
        "fable",
        &grant.token,
        "/v1/messages/count_tokens",
        &agent_effort("claude-fable-5-2"),
    );
    send(&f.egress, req).await.0.unwrap();

    // A model name the person typed that is not in the documented list.
    let grant = f
        .egress
        .begin_run_with_effort(
            "unknown",
            None,
            ProviderKind::Anthropic,
            "my-own-model",
            None,
            Some(crate::egress::Effort::High),
        )
        .await
        .unwrap();
    let req = request(
        "unknown",
        &grant.token,
        "/v1/messages",
        &body("my-own-model"),
    );
    send(&f.egress, req).await.0.unwrap();
    let req = request(
        "unknown",
        &grant.token,
        "/v1/messages",
        &agent_effort("my-own-model"),
    );
    send(&f.egress, req).await.0.unwrap();

    // A documented model with a level it does not take.
    let grant = f
        .egress
        .begin_run_with_effort(
            "old",
            None,
            ProviderKind::Anthropic,
            "claude-opus-4-6",
            None,
            Some(crate::egress::Effort::Xhigh),
        )
        .await
        .unwrap();
    let req = request(
        "old",
        &grant.token,
        "/v1/messages",
        &body("claude-opus-4-6"),
    );
    send(&f.egress, req).await.0.unwrap();
    let req = request(
        "old",
        &grant.token,
        "/v1/messages",
        &agent_effort("claude-opus-4-6"),
    );
    send(&f.egress, req).await.0.unwrap();

    // A run without a level of Hotend's: the agent's level is removed too.
    let grant = f
        .egress
        .begin_run_with_effort(
            "none",
            None,
            ProviderKind::Anthropic,
            "claude-fable-5-2",
            None,
            None,
        )
        .await
        .unwrap();
    let req = request(
        "none",
        &grant.token,
        "/v1/messages",
        &agent_effort("claude-fable-5-2"),
    );
    send(&f.egress, req).await.0.unwrap();

    let seen = seen_bodies(&f);
    assert_eq!(seen.len(), 9);
    assert_eq!(seen[0]["output_config"], json!({"effort": "high"}));
    assert_eq!(
        seen[1]["output_config"],
        json!({"effort": "high", "format": {"type": "json_schema", "schema": {}}})
    );
    assert!(seen[2].get("output_config").is_none(), "{}", seen[2]);
    assert!(seen[4].get("output_config").is_none(), "{}", seen[4]);
    assert!(seen[6].get("output_config").is_none(), "{}", seen[6]);
    // Where Hotend writes no level, the agent's is gone and the rest of output_config
    // stays.
    for index in [3, 5, 7, 8] {
        assert_eq!(
            seen[index]["output_config"],
            json!({"format": {"type": "json_schema", "schema": {}}}),
            "request {index}"
        );
    }
}

#[tokio::test]
async fn other_providers_get_no_effort_field() {
    let f = fixture(EgressLimits::default()).await;
    let grant = f
        .egress
        .begin_run_with_effort(
            "openai",
            None,
            ProviderKind::Openai,
            "claude-fable-5-2",
            None,
            Some(crate::egress::Effort::Max),
        )
        .await
        .unwrap();
    let body = json!({"model": "claude-fable-5-2", "max_tokens": 1024, "messages": []});
    let req = request("openai", &grant.token, "/v1/chat/completions", &body);
    send(&f.egress, req).await.0.unwrap();
    // An `output_config.effort` the agent wrote does not pass either.
    let mut with_effort = body.clone();
    with_effort["output_config"] = json!({"effort": "max"});
    let req = request("openai", &grant.token, "/v1/chat/completions", &with_effort);
    send(&f.egress, req).await.0.unwrap();
    let seen = seen_bodies(&f);
    assert_eq!(seen.len(), 2);
    assert!(seen[0].get("output_config").is_none(), "{}", seen[0]);
    assert!(seen[0].get("reasoning_effort").is_none(), "{}", seen[0]);
    // With the agent's level gone nothing was left in it, so no `output_config` is sent
    // at all (not an empty object).
    assert!(seen[1].get("output_config").is_none(), "{}", seen[1]);
    assert!(seen[1].get("reasoning_effort").is_none(), "{}", seen[1]);
}
