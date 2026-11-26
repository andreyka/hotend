//! LLM egress: the only way a model request leaves the machine.
//!
//! The agent process has no socket. It sends `llm.request` over its stdio channel and this
//! module decides whether the request may go out, adds the credential and streams the answer
//! back.
//!
//! Guarantees, per request:
//! - It belongs to a run that was opened with [`Egress::begin_run`] and presents that run's
//!   random token.
//! - Method, path and query match a row of the provider table ([`providers`]); the
//!   destination host comes from the table, never from the agent.
//! - Every request header outside a short allowlist is dropped, including any credential
//!   header the agent sets; of a beta-feature header only allowlisted features pass. The
//!   real credential is read from the secret store and attached here.
//! - The body is a JSON object that asks for the run's own model, for one answer, and for
//!   no tool that the provider would run on its side (code execution, computer use, remote
//!   MCP, file tools): the agent has no network, and the provider must not become one.
//!   What is sent upstream is the parsed body written out again, so the provider reads
//!   exactly the values that were checked.
//! - One exception, and only for a run that was opened with a [`ResearchGrant`]: the
//!   provider's web search and web fetch. The egress writes those two tool entries itself,
//!   in their basic versions, with the grant's caps ([`research`]); the agent can only mark
//!   where they go. Without a grant they are refused like every other provider-side tool.
//!   Through them the provider does reach the web for this run: search queries and page
//!   addresses leave the machine, and what comes back is untrusted text in the model's
//!   context. It cannot add a tool, a host or a cap.
//! - No content block of the conversation names a URL or a stored file for the provider
//!   to load.
//! - The run's caps hold: number of requests (refused ones included), request bytes,
//!   response bytes and a ceiling on the output tokens a request may ask for.
//! - Redirects are not followed.
//! - The credential never appears in an event, an error or the audit log, and it is
//!   scrubbed from the response body should an upstream echo it.

pub mod effort;
pub mod providers;
pub mod research;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use zeroize::Zeroize;

pub use effort::Effort;
pub use providers::{
    Auth, BodyRule, ModelPlace, PathRule, ProviderKind, ProviderRule, ResearchKind, ResearchRule,
    Route, ServerToolRule, ToolRule,
};
pub use research::{ResearchBlock, ResearchGrant, ResearchSink, ResearchUse};

use crate::secrets::{RevealToken, Secret, SecretError, SecretStore, provider_key_name};
use crate::store::{NewAudit, PendingAudit, Store};
use crate::util::{random_hex, truncate};

/// Caps of one agent run.
#[derive(Debug, Clone)]
pub struct EgressLimits {
    /// Requests per run.
    pub max_requests: u32,
    /// Body bytes of one request.
    pub max_request_bytes: usize,
    /// Body bytes of all requests of a run.
    pub max_total_request_bytes: u64,
    /// Response body bytes of a run.
    pub max_response_bytes: u64,
    /// Ceiling on `max_tokens` / `max_output_tokens` of a request.
    pub max_output_tokens: u64,
    /// Largest response chunk handed to the agent.
    pub chunk_bytes: usize,
    /// Time allowed for one request, including its streamed response.
    pub request_timeout: Duration,
}

impl Default for EgressLimits {
    fn default() -> Self {
        Self {
            max_requests: 100,
            max_request_bytes: 8 * 1024 * 1024,
            max_total_request_bytes: 128 * 1024 * 1024,
            max_response_bytes: 256 * 1024 * 1024,
            max_output_tokens: 32_000,
            chunk_bytes: 16 * 1024,
            request_timeout: Duration::from_secs(600),
        }
    }
}

/// Why a model request was not carried out. Never contains a credential or a body.
#[derive(Debug, thiserror::Error)]
pub enum EgressError {
    /// The run is not open or the token is wrong.
    #[error("the request does not belong to an open run")]
    NotAuthorised,
    /// A cap of the run is used up.
    #[error("the run's limit of {limit} {what} is used up")]
    CapExceeded { what: &'static str, limit: u64 },
    /// The request is not one the provider table allows.
    #[error("the request was refused: {reason}")]
    Denied { reason: String },
    /// The request itself is malformed.
    #[error("the request is malformed: {0}")]
    Invalid(&'static str),
    /// No API key is stored for the provider.
    #[error("no API key is set for {provider}; add one in the settings")]
    NoCredential { provider: &'static str },
    /// The secret store failed.
    #[error(transparent)]
    Secret(#[from] SecretError),
    /// The provider could not be reached or the transfer broke.
    #[error("the provider request failed: {0}")]
    Upstream(&'static str),
    /// The HTTP client or the random source could not be set up.
    #[error("the egress module cannot start: {0}")]
    Setup(String),
}

impl EgressError {
    /// Whether the request itself was turned away (as opposed to a failure of the
    /// provider, the secret store or the core). Refusals count against the run.
    pub fn is_refusal(&self) -> bool {
        matches!(
            self,
            Self::NotAuthorised | Self::CapExceeded { .. } | Self::Denied { .. } | Self::Invalid(_)
        )
    }

    /// A short stable code for audit entries and JSON-RPC error data.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotAuthorised => "not_authorised",
            Self::CapExceeded { .. } => "cap_exceeded",
            Self::Denied { .. } => "denied",
            Self::Invalid(_) => "invalid_request",
            Self::NoCredential { .. } => "no_credential",
            Self::Secret(_) => "secret_store_error",
            Self::Upstream(_) => "upstream_error",
            Self::Setup(_) => "setup_error",
        }
    }
}

/// The `llm.request` parameters (PROTOCOL.md section 5).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmRequest {
    pub run_id: String,
    /// Chosen by the agent; echoed on the response notifications.
    pub request_id: String,
    /// The run's egress token from `agent.run`.
    pub token: String,
    pub method: String,
    /// Path and optional query, as the provider SDK built it.
    pub path: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub body_b64: String,
}

/// What the agent receives while a request is answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmEvent {
    /// Status line and the allowlisted response headers.
    Start {
        status: u16,
        headers: BTreeMap<String, String>,
    },
    /// A piece of the response body.
    Chunk(Vec<u8>),
}

/// What a run needs to use the egress: handed to the agent in `agent.run`.
#[derive(Debug, Clone)]
pub struct RunGrant {
    /// Random per-run token the agent must present with every `llm.request`.
    pub token: String,
    /// The base URL to configure in the provider SDK.
    pub base_url: &'static str,
    /// The most output tokens one request of this run may ask for. The agent is told it, so
    /// that it asks for this much instead of a smaller default of its own: a model that
    /// thinks first can use up a small allowance before it writes a word.
    pub max_output_tokens: u64,
}

/// Totals of a finished run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunTotals {
    /// Requests sent to the provider.
    pub requests: u32,
    pub request_bytes: u64,
    pub response_bytes: u64,
    /// Requests that were turned away. They count against the run's request cap too.
    pub refused: u32,
    /// What the provider's web tools were used for in this run, as the egress counted it.
    pub research: ResearchUse,
    /// Set when a response held a web result that must not be stored or sent again.
    pub research_block: Option<ResearchBlock>,
}

/// What a run is allowed to ask of the model: the model itself and the effort value the
/// egress writes.
#[derive(Clone, Copy)]
struct RunModel<'a> {
    model: &'a str,
    effort: Option<&'static str>,
}

struct RunState {
    provider: ProviderKind,
    /// The only model the run may ask for.
    model: String,
    /// The value of `output_config.effort` written into each generating request, when
    /// the run's provider and model take one ([`effort::anthropic_effort`]).
    effort: Option<&'static str>,
    project_id: Option<String>,
    token: String,
    totals: RunTotals,
    /// The first request refused for the used-up request cap has been audited; the ones
    /// after it are only counted.
    cap_reported: bool,
    /// The run's permission to use the provider's web tools, already clamped.
    research: Option<ResearchGrant>,
    /// A request that carries web tools is being answered. One at a time.
    research_in_flight: bool,
    /// Web calls counted against the run's allowance (the grant's caps), per kind: what was
    /// seen to run and, for an answer that could not be read to its end, the whole
    /// allowance that request carried. Never charged to the project.
    research_ceiling: [u32; 2],
    /// The provider turn in progress: what was written at its start, the calls seen in it
    /// and what it was charged.
    research_turn: Turn,
    /// The run's last answer that was read (status 200) ended with `pause_turn`: only then
    /// does a request whose last message is the model's own go on with the turn. A failed
    /// or unread answer leaves it as it was, so a resent request still goes on with it.
    research_paused: bool,
    /// Web calls whose result has not arrived yet, by call id: the query or the address.
    /// The result opens a later response of the run (a paused turn, or a call made in one
    /// group with a gateway tool).
    research_pending: tap::Pending,
}

/// Whether `url` names this computer: `localhost` or a loopback address.
fn is_loopback(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// The egress module. Share it behind an `Arc`.
pub struct Egress {
    secrets: Arc<dyn SecretStore>,
    store: Arc<Store>,
    /// For a provider elsewhere: honours `HTTPS_PROXY`, `HTTP_PROXY` and `ALL_PROXY`.
    client: reqwest::Client,
    /// For a provider on this computer (Ollama): never through a proxy.
    direct: reqwest::Client,
    limits: EgressLimits,
    runs: Mutex<HashMap<String, RunState>>,
    /// Replaces the table's origin. Only the test constructor can set it.
    origin_override: Option<String>,
}

impl std::fmt::Debug for Egress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Egress")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl Egress {
    /// An egress that sends to the providers' real hosts.
    pub fn new(
        secrets: Arc<dyn SecretStore>,
        store: Arc<Store>,
        limits: EgressLimits,
    ) -> Result<Self, EgressError> {
        Self::build(secrets, store, limits, None)
    }

    /// An egress whose requests go to a stand-in server instead of the provider. Exists only
    /// in test builds of this crate; nothing the agent sends can select a host.
    #[cfg(test)]
    pub(crate) fn with_upstream_for_tests(
        secrets: Arc<dyn SecretStore>,
        store: Arc<Store>,
        limits: EgressLimits,
        origin: &str,
    ) -> Self {
        #[allow(clippy::expect_used)]
        Self::build(secrets, store, limits, Some(origin.to_owned())).expect("test egress")
    }

    /// As [`Self::with_upstream_for_tests`], with every request that may use a proxy sent
    /// to `proxy`, as the proxy variables would send it.
    #[cfg(test)]
    pub(crate) fn with_proxy_for_tests(
        secrets: Arc<dyn SecretStore>,
        store: Arc<Store>,
        origin: &str,
        proxy: &str,
    ) -> Self {
        #[allow(clippy::expect_used)]
        let mut egress = Self::build(
            secrets,
            store,
            EgressLimits::default(),
            Some(origin.to_owned()),
        )
        .expect("test egress");
        #[allow(clippy::expect_used)]
        let proxied = Self::builder(&egress.limits)
            .proxy(reqwest::Proxy::all(proxy).expect("a proxy address"))
            .build()
            .expect("test client");
        egress.client = proxied;
        egress
    }

    fn builder(limits: &EgressLimits) -> reqwest::ClientBuilder {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .referer(false)
            .connect_timeout(Duration::from_secs(20))
            .timeout(limits.request_timeout)
            .user_agent(concat!("hotend/", env!("CARGO_PKG_VERSION")))
    }

    fn build(
        secrets: Arc<dyn SecretStore>,
        store: Arc<Store>,
        limits: EgressLimits,
        origin_override: Option<String>,
    ) -> Result<Self, EgressError> {
        let setup = |e: reqwest::Error| EgressError::Setup(truncate(&e.to_string(), 200));
        let client = Self::builder(&limits).build().map_err(setup)?;
        let direct = Self::builder(&limits).no_proxy().build().map_err(setup)?;
        Ok(Self {
            secrets,
            store,
            client,
            direct,
            limits,
            runs: Mutex::new(HashMap::new()),
            origin_override,
        })
    }

    fn runs(&self) -> std::sync::MutexGuard<'_, HashMap<String, RunState>> {
        self.runs.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Opens a run for one provider and one model and returns its token. Fails when the
    /// provider needs a key and none is stored, so the user is told before any model call
    /// is attempted. `research` lets the run use the provider's web search and web fetch
    /// within the grant's caps; `None` keeps them refused.
    pub async fn begin_run(
        &self,
        run_id: &str,
        project_id: Option<&str>,
        provider: ProviderKind,
        model: &str,
        research: Option<ResearchGrant>,
    ) -> Result<RunGrant, EgressError> {
        self.begin_run_with_effort(run_id, project_id, provider, model, research, None)
            .await
    }

    /// [`Egress::begin_run`] with the person's effort level. It reaches the provider only
    /// for an Anthropic run whose model is documented to accept that level; otherwise the
    /// request carries no effort field (one the agent wrote is removed) and the provider's
    /// default applies.
    pub async fn begin_run_with_effort(
        &self,
        run_id: &str,
        project_id: Option<&str>,
        provider: ProviderKind,
        model: &str,
        research: Option<ResearchGrant>,
        effort: Option<Effort>,
    ) -> Result<RunGrant, EgressError> {
        let effort = effort
            .filter(|_| provider == ProviderKind::Anthropic)
            .and_then(|effort| effort::anthropic_effort(model, effort));
        let rule = provider.rule();
        if rule.auth != Auth::None {
            self.credential(provider).await?;
        }
        // A grant for a provider without web research is dropped, not an error: the run
        // simply has none.
        let research = research
            .filter(|_| rule.body.research.is_some())
            .map(ResearchGrant::clamped);
        let token = random_hex(32).map_err(|e| EgressError::Setup(e.to_string()))?;
        self.runs().insert(
            run_id.to_owned(),
            RunState {
                provider,
                model: model.to_owned(),
                effort,
                project_id: project_id.map(str::to_owned),
                token: token.clone(),
                totals: RunTotals::default(),
                cap_reported: false,
                research,
                research_in_flight: false,
                research_ceiling: [0; 2],
                research_turn: Turn::default(),
                research_paused: false,
                research_pending: tap::Pending::new(),
            },
        );
        Ok(RunGrant {
            token,
            base_url: rule.sdk_base_url,
            max_output_tokens: self.limits.max_output_tokens,
        })
    }

    /// Closes a run; its token stops working. Returns what the run used. A web call that
    /// never got its result in this run is logged as `pending`.
    pub fn end_run(&self, run_id: &str) -> RunTotals {
        let Some(run) = self.runs().remove(run_id) else {
            return RunTotals::default();
        };
        if let Some(grant) = &run.research {
            let mut waiting: Vec<_> = run.research_pending.into_values().collect();
            waiting.sort_by(|a, b| a.1.cmp(&b.1));
            for (kind, text) in waiting {
                grant.sink.call(kind, &text, "pending");
            }
        }
        run.totals
    }

    async fn credential(&self, provider: ProviderKind) -> Result<Secret, EgressError> {
        let secrets = Arc::clone(&self.secrets);
        let name = provider_key_name(provider);
        // Keychains answer over blocking IPC.
        tokio::task::spawn_blocking(move || secrets.reveal(&name, &RevealToken::new()))
            .await
            .map_err(|_| EgressError::Setup("the secret store task was cancelled".to_owned()))??
            .ok_or(EgressError::NoCredential {
                provider: provider.as_str(),
            })
    }

    /// Checks one `llm.request`, sends it and streams the response into `events`.
    ///
    /// Returns when the response has been delivered completely. A refused request sends no
    /// events. Every call leaves one audit entry with sizes and the outcome, never a body
    /// or a header; a call whose future is dropped half-way (the run ended) is recorded as
    /// `abandoned`. The one exception: once a run's request cap is used up, only the first
    /// refusal for it is audited and the rest are counted in the run's totals.
    pub async fn handle(
        &self,
        request: LlmRequest,
        events: &mpsc::Sender<LlmEvent>,
    ) -> Result<(), EgressError> {
        let project_id = self
            .runs()
            .get(&request.run_id)
            .and_then(|run| run.project_id.clone());
        let mut record = Record {
            audit: PendingAudit::new(
                &self.store,
                NewAudit {
                    project_id,
                    run_id: Some(truncate(&request.run_id, 64)),
                    kind: "egress",
                    name: "unknown".to_owned(),
                    args_sha256: None,
                    outcome: String::new(),
                    detail: json!({
                        "method": truncate(&request.method, 16),
                        "path": truncate(request.path.split('?').next().unwrap_or(""), 200),
                    }),
                },
            ),
            facts: Facts::default(),
        };
        let outcome = self.handle_inner(&request, events, &mut record.facts).await;
        record
            .audit
            .finish(outcome.as_ref().map_or_else(|e| e.code(), |()| "ok"));
        if outcome.as_ref().is_err_and(EgressError::is_refusal)
            && let Some(run) = self.runs().get_mut(&request.run_id)
        {
            run.totals.refused = run.totals.refused.saturating_add(1);
            if record.facts.over_request_cap {
                if run.cap_reported {
                    record.audit.suppress();
                }
                run.cap_reported = true;
            }
        }
        outcome
    }

    async fn handle_inner(
        &self,
        request: &LlmRequest,
        events: &mpsc::Sender<LlmEvent>,
        facts: &mut Facts,
    ) -> Result<(), EgressError> {
        // 1. The caller: an open run and its token. Every request counts against the
        //    run's cap, also the ones that are refused further down.
        let (provider, model, effort, research, left) = {
            let runs = self.runs();
            let run = runs
                .get(&request.run_id)
                .ok_or(EgressError::NotAuthorised)?;
            if run.totals.requests.saturating_add(run.totals.refused) >= self.limits.max_requests {
                facts.over_request_cap = true;
                return Err(EgressError::CapExceeded {
                    what: "requests",
                    limit: u64::from(self.limits.max_requests),
                });
            }
            if !constant_time_eq(run.token.as_bytes(), request.token.as_bytes()) {
                return Err(EgressError::NotAuthorised);
            }
            let left = run
                .research
                .as_ref()
                .map_or(Allowance::default(), |grant| Allowance::of(run, grant));
            (
                run.provider,
                run.model.clone(),
                run.effort,
                run.research.clone(),
                left,
            )
        };
        let rule = provider.rule();
        facts.provider = Some(rule.name);

        // 2. The destination: method, path and query from the table.
        let method = request.method.to_ascii_uppercase();
        let (path, query) = match request.path.split_once('?') {
            Some((path, query)) => (path, Some(query)),
            None => (request.path.as_str(), None),
        };
        let route = rule
            .route(&method, path, query)
            .ok_or_else(|| EgressError::Denied {
                reason: format!(
                    "{} {} is not an allowed {} API call",
                    truncate(&method, 16),
                    truncate(path, 120),
                    rule.name
                ),
            })?;

        // 3. The body and the caps.
        if request.body_b64.len() > self.limits.max_request_bytes / 3 * 4 + 4 {
            return Err(EgressError::CapExceeded {
                what: "bytes per request",
                limit: self.limits.max_request_bytes as u64,
            });
        }
        let body = BASE64
            .decode(request.body_b64.as_bytes())
            .map_err(|_| EgressError::Invalid("body_b64 is not base64"))?;
        facts.request_bytes = body.len() as u64;
        let allowance = research.as_ref().map(|grant| (grant, &left));
        let run = RunModel {
            model: &model,
            effort,
        };
        let checked = self.check_body(rule, route, path, run, &body, allowance)?;
        let (body, max_tokens) = (checked.body, checked.max_tokens);
        facts.max_tokens = max_tokens;
        let mut charge = None;
        {
            let mut runs = self.runs();
            let run = runs
                .get_mut(&request.run_id)
                .ok_or(EgressError::NotAuthorised)?;
            if run.totals.request_bytes + body.len() as u64 > self.limits.max_total_request_bytes {
                return Err(EgressError::CapExceeded {
                    what: "request bytes",
                    limit: self.limits.max_total_request_bytes,
                });
            }
            if run.totals.response_bytes >= self.limits.max_response_bytes {
                return Err(EgressError::CapExceeded {
                    what: "response bytes",
                    limit: self.limits.max_response_bytes,
                });
            }
            let granted = research.is_some();
            if let Some(grant) = research.filter(|_| checked.web != [false; 2]) {
                admit_research(run, &grant, &left)?;
                charge = Some(ResearchCharge {
                    egress: self,
                    run_id: request.run_id.clone(),
                    allowed: [
                        if checked.web[0] { left.left[0] } else { 0 },
                        if checked.web[1] { left.left[1] } else { 0 },
                    ],
                    grant,
                    sent: false,
                    clean: false,
                    pending: Some(std::mem::take(&mut run.research_pending)),
                    tap: None,
                });
            }
            // A generating request that does not go on with the provider's turn starts a new
            // one: its count of web calls starts at nothing, and what it writes (what the run
            // has left now) is what every request of the turn writes.
            if granted && route.generates && !checked.continues {
                let written = run
                    .research
                    .as_ref()
                    .map_or([0; 2], |grant| research_left(run, grant));
                run.research_turn = Turn {
                    written,
                    epoch: run.research_turn.epoch.wrapping_add(1),
                    ..Turn::default()
                };
                run.research_paused = false;
            }
            run.totals.requests += 1;
            run.totals.request_bytes += body.len() as u64;
        }

        // 4. Headers: the allowlist, then the credential.
        let mut headers = HeaderMap::new();
        for (name, value) in &request.headers {
            let name = name.to_ascii_lowercase();
            let allowed = providers::GLOBAL_REQUEST_HEADERS.contains(&name.as_str())
                || rule.pass_headers.contains(&name.as_str());
            if !allowed || value.len() > 1024 {
                continue;
            }
            let features = rule
                .feature_headers
                .iter()
                .find(|(header, _)| *header == name)
                .map(|(_, prefixes)| *prefixes);
            let value = match features {
                Some(prefixes) => match allowed_features(value, prefixes) {
                    Some(value) => value,
                    None => continue,
                },
                None => value.clone(),
            };
            if let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_str(&value),
            ) {
                headers.insert(name, value);
            }
        }
        headers.insert(
            reqwest::header::ACCEPT_ENCODING,
            HeaderValue::from_static("identity"),
        );
        let secret = match rule.auth {
            Auth::None => None,
            _ => Some(self.credential(provider).await?),
        };
        if let Some(secret) = &secret {
            let (name, text) = match rule.auth {
                Auth::Header(name) => (HeaderName::from_static(name), secret.expose().to_owned()),
                _ => (
                    reqwest::header::AUTHORIZATION,
                    format!("Bearer {}", secret.expose()),
                ),
            };
            let mut value = HeaderValue::from_str(&text)
                .map_err(|_| EgressError::Invalid("the stored key is not a valid header value"))?;
            value.set_sensitive(true);
            headers.insert(name, value);
        }

        // 5. The URL: origin from the table (or the test stand-in), path as validated.
        let origin = self.origin_override.as_deref().unwrap_or(rule.origin);
        let url = reqwest::Url::parse(&format!("{origin}{}", request.path))
            .map_err(|_| EgressError::Invalid("the path does not form a URL"))?;
        let expected = reqwest::Url::parse(origin)
            .map_err(|_| EgressError::Setup("the provider origin is not a URL".to_owned()))?;
        if url.origin() != expected.origin() || url.path() != path || !url.username().is_empty() {
            return Err(EgressError::Denied {
                reason: "the path does not stay on the provider's host".to_owned(),
            });
        }
        let method = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| EgressError::Invalid("unknown method"))?;

        // 6. Send and stream back.
        if let Some(charge) = charge.as_mut() {
            charge.sent = true;
        }
        // A provider on this computer is reached directly: a proxy would carry the
        // conversation off it.
        let client = if is_loopback(&url) {
            &self.direct
        } else {
            &self.client
        };
        let sent = client
            .request(method, url)
            .headers(headers)
            .body(body)
            .send()
            .await;
        let mut response = match sent {
            Ok(response) => response,
            Err(error) => {
                // A request that never connected reached nobody: no web tool ran.
                if let Some(charge) = charge.as_mut().filter(|_| error.is_connect()) {
                    charge.sent = false;
                }
                return Err(upstream_error(error));
            }
        };
        let status = response.status().as_u16();
        facts.status = Some(status);
        if let Some(charge) = charge.as_mut() {
            let streamed = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|kind| kind.starts_with("text/event-stream"));
            charge.tap = Some(tap::Tap::new(
                status,
                streamed,
                &charge.grant,
                charge.allowed,
                charge.pending.take().unwrap_or_default(),
            ));
        }
        let mut redactor = Redactor::new(secret.as_ref().map(Secret::expose).unwrap_or(""));
        let mut passed = BTreeMap::new();
        for name in providers::RESPONSE_HEADERS {
            if let Some(value) = response.headers().get(name).and_then(|v| v.to_str().ok()) {
                let clean = redactor.scrub_text(value);
                passed.insert(name.to_owned(), truncate(&clean, 1024));
            }
        }
        let closed = |_| EgressError::Upstream("the agent stopped listening");
        events
            .send(LlmEvent::Start {
                status,
                headers: passed,
            })
            .await
            .map_err(closed)?;
        loop {
            let piece = response.chunk().await.map_err(upstream_error)?;
            let data = match &piece {
                Some(bytes) => redactor.push(bytes),
                None => redactor.finish(),
            };
            facts.response_bytes += data.len() as u64;
            // The tap reads what the agent will read: after the redactor, before the agent.
            if let Some(tap) = charge.as_mut().and_then(|charge| charge.tap.as_mut()) {
                tap.push(&data);
            }
            let over = {
                let mut runs = self.runs();
                match runs.get_mut(&request.run_id) {
                    Some(run) => {
                        run.totals.response_bytes += data.len() as u64;
                        run.totals.response_bytes > self.limits.max_response_bytes
                    }
                    // The run was closed while the response was streaming.
                    None => return Err(EgressError::NotAuthorised),
                }
            };
            if over {
                return Err(EgressError::CapExceeded {
                    what: "response bytes",
                    limit: self.limits.max_response_bytes,
                });
            }
            for part in data.chunks(self.limits.chunk_bytes.max(1)) {
                events
                    .send(LlmEvent::Chunk(part.to_vec()))
                    .await
                    .map_err(closed)?;
            }
            if piece.is_none() {
                if let Some(charge) = charge.as_mut() {
                    charge.clean = true;
                }
                return Ok(());
            }
        }
    }

    /// Checks the body of a request against the provider's [`BodyRule`] and, for a
    /// generating route, the output-token ceiling. Returns the body to send (the parsed
    /// JSON written out again, so a key that was given twice has one value, the one that
    /// was checked) and the output tokens it asks for.
    ///
    /// With a `research` grant (and what the run may still use of each kind), a marker entry
    /// for the provider's web search or web fetch is replaced by the entry the egress builds
    /// itself, whose `max_uses` lets the provider run exactly what the run has left
    /// ([`research::max_uses`]). Within a provider turn that value stays the same, so the
    /// entries keep their bytes; a kind with nothing to write is not offered at all. On a
    /// route that does not generate (token counting, which rejects such tools) the marker
    /// is removed. Without a grant the marker is refused like any other provider-side tool.
    fn check_body(
        &self,
        rule: &ProviderRule,
        route: &Route,
        path: &str,
        run: RunModel<'_>,
        body: &[u8],
        research: Option<(&ResearchGrant, &Allowance)>,
    ) -> Result<Checked, EgressError> {
        let RunModel { model, effort } = run;
        let none = Allowance::default();
        let allowance = research.map_or(&none, |(_, allowance)| allowance);
        let left = allowance.left;
        let research = research.map(|(grant, _)| grant);
        let mut json: Value = serde_json::from_slice(body)
            .map_err(|_| EgressError::Invalid("the request body is not JSON"))?;
        let Some(object) = json.as_object() else {
            return Err(EgressError::Invalid(
                "the request body is not a JSON object",
            ));
        };
        let denied = |reason: String| EgressError::Denied { reason };
        let at = |path: &[&str]| {
            path.iter()
                .try_fold(&json, |value, key| value.get(key))
                .filter(|value| !value.is_null())
        };

        let asked = match rule.body.model {
            ModelPlace::Body => object.get("model").and_then(Value::as_str),
            ModelPlace::Path => route.path.model_in(path),
        };
        if asked != Some(model) {
            return Err(denied(format!(
                "the request asks for the model `{}`; this run uses `{}`",
                truncate(asked.unwrap_or("(none)"), 80),
                truncate(model, 80)
            )));
        }
        for parent in rule.body.camel_case_only {
            let keys = at(parent).and_then(Value::as_object);
            if let Some(key) = keys.and_then(|keys| keys.keys().find(|key| key.contains('_'))) {
                return Err(denied(format!(
                    "the field `{}` is not in the spelling this provider's requests use",
                    truncate(key, 60)
                )));
            }
        }
        if let Some(field) = rule
            .body
            .denied_fields
            .iter()
            .find(|field| object.get(**field).is_some_and(|value| !value.is_null()))
        {
            return Err(denied(format!(
                "the field `{field}` is not allowed: model requests may not use provider-side \
                 tools, stored requests or other models"
            )));
        }
        if let Some(allowed) = rule.body.allowed_fields
            && let Some(field) = object
                .iter()
                .find(|(field, value)| !value.is_null() && !allowed.contains(&field.as_str()))
                .map(|(field, _)| field)
        {
            return Err(denied(format!(
                "the field `{}` is not allowed: a request to this provider may hold only {}; \
                 leave it out",
                truncate(field, 60),
                allowed.join(", ")
            )));
        }
        for field in rule.body.single_output_fields {
            if at(field).is_some_and(|value| value.as_u64() != Some(1)) {
                return Err(denied(format!(
                    "`{}` must be 1: a request may ask for one answer",
                    field.join(".")
                )));
            }
        }
        let server_tools: Vec<&str> = rule
            .body
            .research
            .iter()
            .flat_map(|research| research.tools.iter().map(|tool| tool.name))
            .collect();
        let limits = research::BlockLimits {
            denied_sources: rule.body.denied_block_sources,
            denied_types: rule.body.denied_block_types,
            server_tools: &server_tools,
            web_results: rule.body.research.is_some(),
            allow_pdf: research.is_some_and(|grant| grant.allow_pdf),
            max_result_bytes: research.map_or(research::DEFAULT_RESULT_BYTES, |grant| {
                grant.max_result_bytes
            }),
        };
        if (limits.web_results
            || !limits.denied_sources.is_empty()
            || !limits.denied_types.is_empty())
            && let Some(reason) = ["messages", "system"]
                .iter()
                .filter_map(|field| object.get(*field))
                .find_map(|value| research::check_blocks(value, &limits, 0))
        {
            return Err(denied(reason));
        }
        // Whether the request goes on with the provider's turn, and the web calls of that
        // turn the provider counts against `max_uses` (only looked at with a grant).
        let messages = object.get("messages");
        let continues = research.is_some() && research::continues_turn(messages, allowance.paused);
        // What each entry carries (see `research::max_uses`): a new turn writes what is left;
        // a request that goes on with the turn writes what the turn's first request wrote.
        // 0 means the entry is not written.
        let writes = if continues {
            let earlier = research::calls_in_turn(messages, &allowance.turn.calls);
            [0, 1].map(|slot| {
                research::max_uses(allowance.turn.written[slot], left[slot], earlier[slot])
            })
        } else {
            left
        };
        // Which web tool entries the request ends up with: [search, fetch].
        let mut web = [false; 2];
        let mut listed = [false; 2];
        let mut tools_out = None;
        if let Some(tools) = at(&["tools"]) {
            let entries = tools.as_array().ok_or(EgressError::Invalid(
                "`tools` in the request body is not a list",
            ))?;
            let marker = |tool: &Value| rule.body.research.and_then(|r| r.marker(tool));
            // The fetch entry may name the search entry only when that is written too.
            let with_search = writes[ResearchKind::Search as usize] > 0
                && entries
                    .iter()
                    .any(|tool| marker(tool).is_some_and(|m| m.kind == ResearchKind::Search));
            let mut written = Vec::with_capacity(entries.len());
            for tool in entries {
                if tool_allowed(&rule.body.tools, tool) {
                    written.push(tool.clone());
                    continue;
                }
                let (Some(server), Some(grant)) = (marker(tool), research) else {
                    let kind = tool
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("(untyped)");
                    return Err(denied(format!(
                        "the tool entry of type `{}` is not allowed: only tools that the core \
                         itself executes may be offered to the model",
                        truncate(kind, 60)
                    )));
                };
                let slot = server.kind as usize;
                if listed[slot] || grant.max_uses(server.kind) == 0 {
                    return Err(denied(format!(
                        "the tool `{}` is {}",
                        server.name,
                        if listed[slot] {
                            "listed twice"
                        } else {
                            "not part of this run's web research"
                        }
                    )));
                }
                listed[slot] = true;
                // Nothing to write: the run had nothing left of this kind when the turn
                // began. The tool is not offered at all, so the model cannot call it (the
                // request goes on without it). Within a turn an entry whose kind ran out
                // stays, carrying the calls already made: the provider answers a further
                // call with `max_uses_exceeded`, which runs and counts nothing.
                if writes[slot] == 0 {
                    continue;
                }
                web[slot] = true;
                if route.generates {
                    written.push(grant.entry(server, tool, writes[slot], with_search));
                }
            }
            tools_out = Some(written);
        }
        if route.generates && web != [false; 2] && !research::has_cache_marker(&json, 0) {
            return Err(denied(
                "a request with web research must use prompt caching (a `cache_control` \
                 marker): web results are read again by every later request"
                    .to_owned(),
            ));
        }
        if !route.generates {
            web = [false; 2];
        }
        if let (Some(written), Some(object)) = (tools_out, json.as_object_mut()) {
            object.insert("tools".to_owned(), Value::Array(written));
        }
        // The person's effort level: written by the egress, never taken from the agent,
        // and only into a request that generates. An `output_config.effort` the agent
        // wrote is removed from every request first, so a request for which Hotend
        // writes none (another route, another provider, a model not documented for the
        // level) goes without one and the provider's default applies. Other fields of
        // `output_config` stay; an `output_config` that held nothing else is removed
        // whole, so that no provider is sent an empty object it may not accept.
        if let Some(object) = json.as_object_mut() {
            let emptied = object
                .get_mut("output_config")
                .and_then(Value::as_object_mut)
                .is_some_and(|config| config.remove("effort").is_some() && config.is_empty());
            if emptied {
                object.remove("output_config");
            }
        }
        if route.generates
            && let Some(level) = effort
            && let Some(object) = json.as_object_mut()
        {
            let config = object
                .entry("output_config")
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
            if !config.is_object() {
                *config = Value::Object(serde_json::Map::new());
            }
            if let Some(config) = config.as_object_mut() {
                config.insert("effort".to_owned(), Value::String(level.to_owned()));
            }
        }

        let max_tokens = if route.generates {
            Some(self.check_token_ceiling(rule, &json)?)
        } else {
            None
        };
        let body = serde_json::to_vec(&json)
            .map_err(|_| EgressError::Invalid("the request body cannot be written as JSON"))?;
        Ok(Checked {
            body,
            max_tokens,
            web,
            continues,
        })
    }

    /// Enforces the ceiling on the output tokens a generating request may ask for and
    /// returns the value it asks for (0 when the provider needs none and none is given).
    fn check_token_ceiling(&self, rule: &ProviderRule, json: &Value) -> Result<u64, EgressError> {
        let ceiling = self.limits.max_output_tokens;
        let mut largest = None;
        for field in rule.token_fields {
            let mut value = Some(json);
            for key in *field {
                value = value.and_then(|v| v.get(key));
            }
            match value {
                None | Some(Value::Null) => {}
                Some(value) => {
                    let tokens = value.as_u64().ok_or(EgressError::Invalid(
                        "the output token limit in the body is not a whole number",
                    ))?;
                    if tokens > ceiling {
                        return Err(EgressError::Denied {
                            reason: format!(
                                "the request asks for {tokens} output tokens; the ceiling is {ceiling}"
                            ),
                        });
                    }
                    largest = Some(largest.unwrap_or(0).max(tokens));
                }
            }
        }
        match largest {
            Some(tokens) => Ok(tokens),
            None if rule.require_token_limit => Err(EgressError::Denied {
                reason: format!(
                    "the request sets no output token limit; set `{}` to at most {ceiling}",
                    rule.token_fields
                        .first()
                        .map(|f| f.join("."))
                        .unwrap_or_default()
                ),
            }),
            None => Ok(0),
        }
    }
}

/// A request body that passed [`Egress::check_body`].
struct Checked {
    /// The body to send.
    body: Vec<u8>,
    /// The output tokens it asks for, on a generating route.
    max_tokens: Option<u64>,
    /// Whether it carries the web search and the web fetch entry.
    web: [bool; 2],
    /// Whether it goes on with the provider's turn (only worked out with a grant).
    continues: bool,
}

/// Most web calls a turn remembers by id; a provider that went past it is not followed.
const MAX_TURN_CALLS: usize = 64;

/// The provider turn a run is in: what its first request wrote, the calls seen in it and
/// what it was charged. Charged per call, once: a call the provider's usage count reported
/// in one response and whose result came in the next is one call.
#[derive(Debug, Clone, Default)]
struct Turn {
    /// The `max_uses` the turn's first request wrote, per kind (what the run had left
    /// then). Every later request of the turn writes the same, or less when the
    /// conversation does not hold the calls the run was charged for ([`research::max_uses`]).
    written: [u32; 2],
    /// The web calls the responses of this turn made, by id, but those answered
    /// `max_uses_exceeded`: what the provider counts against `max_uses`.
    calls: HashMap<String, ResearchKind>,
    /// The calls whose result counted, by id.
    counted: HashMap<String, ResearchKind>,
    /// Counted results without an id, and calls still waiting when an answer broke off.
    other: [u32; 2],
    /// The provider's own counts, summed over the turn's responses.
    reported: [u32; 2],
    /// What the turn was charged so far, per kind.
    charged: [u32; 2],
    /// Changes with every new turn.
    epoch: u64,
}

impl Turn {
    /// What the turn uses of each kind: every call counted once, by id or otherwise, or
    /// the provider's own count when that is more.
    fn used(&self) -> [u32; 2] {
        [ResearchKind::Search, ResearchKind::Fetch].map(|kind| {
            let by_id = self.counted.values().filter(|k| **k == kind).count() as u32;
            by_id
                .saturating_add(self.other[kind as usize])
                .max(self.reported[kind as usize])
        })
    }

    /// Takes in one response's calls and counts; returns what the turn is charged more.
    fn take(&mut self, outcome: &tap::TapOutcome) -> [u32; 2] {
        for (kind, id) in &outcome.calls {
            if self.calls.len() < MAX_TURN_CALLS {
                self.calls.insert(id.clone(), *kind);
            }
        }
        for id in &outcome.exceeded {
            self.calls.remove(id);
        }
        let mut by_id = [0u32; 2];
        for (kind, id) in &outcome.counted {
            by_id[*kind as usize] += 1;
            if self.counted.len() < MAX_TURN_CALLS {
                self.counted.insert(id.clone(), *kind);
            }
        }
        for (kind, counted) in by_id.into_iter().enumerate() {
            let without_id = outcome.results[kind].saturating_sub(counted);
            self.other[kind] = self.other[kind]
                .saturating_add(without_id)
                .saturating_add(outcome.waiting[kind]);
            self.reported[kind] = self.reported[kind].saturating_add(outcome.reported[kind]);
        }
        let now = self.used();
        let more = [0, 1].map(|kind| now[kind].saturating_sub(self.charged[kind]));
        self.charged = [0, 1].map(|kind| self.charged[kind].max(now[kind]));
        more
    }
}

/// What a run may still use of the provider's web tools when a request is checked, and what
/// it knows of the provider's current turn.
#[derive(Debug, Clone, Default)]
struct Allowance {
    /// Calls of each kind the run may still make: its grant's cap less what it was charged.
    left: [u32; 2],
    /// [`RunState::research_turn`].
    turn: Turn,
    /// [`RunState::research_paused`].
    paused: bool,
}

impl Allowance {
    fn of(run: &RunState, grant: &ResearchGrant) -> Self {
        Self {
            left: research_left(run, grant),
            turn: run.research_turn.clone(),
            paused: run.research_paused,
        }
    }

    /// Whether `other` describes the same state of the run (the turn's calls change only
    /// with an answer, which also changes what was charged, or with a new turn).
    fn same(&self, other: &Self) -> bool {
        self.left == other.left
            && self.turn.epoch == other.turn.epoch
            && self.turn.calls.len() == other.turn.calls.len()
            && self.turn.charged == other.turn.charged
            && self.paused == other.paused
    }
}

/// What a run may still use of each kind: the grant's cap (the per-message cap, or less when
/// the project has less left) less what the run was charged. Never more, whatever the
/// agent process sends; at 0 the tool is not offered any more.
fn research_left(run: &RunState, grant: &ResearchGrant) -> [u32; 2] {
    [ResearchKind::Search, ResearchKind::Fetch].map(|kind| {
        grant
            .max_uses(kind)
            .saturating_sub(run.research_ceiling[kind as usize])
    })
}

/// Lets one request with web tools start, or says why not: one such request at a time.
/// `allowance` is what the request's entries were written with; it must still hold.
fn admit_research(
    run: &mut RunState,
    grant: &ResearchGrant,
    allowance: &Allowance,
) -> Result<(), EgressError> {
    if run.research_in_flight || !allowance.same(&Allowance::of(run, grant)) {
        return Err(EgressError::Denied {
            reason: "another request with web research is still being answered; send one at \
                     a time"
                .to_owned(),
        });
    }
    run.research_in_flight = true;
    Ok(())
}

/// The bookkeeping of one request that carries web tools. Whatever ends the request, the
/// drop releases the run's slot and charges what was used.
///
/// Two counts are kept apart. The project (the run's totals and the sink) is charged what
/// was seen: results, the provider's own counts and, in an answer that broke off, the calls
/// that were waiting. The run's allowance is charged the same when the answer was read to
/// its end, and all the request could have used when it was not, because nobody can say
/// what a broken answer used. A request that never connected is charged nothing.
struct ResearchCharge<'a> {
    egress: &'a Egress,
    run_id: String,
    /// What the request let the provider run, per kind (what the run had left); 0 for a
    /// kind it does not carry.
    allowed: [u32; 2],
    grant: ResearchGrant,
    /// The request went out (or may have).
    sent: bool,
    /// The response was read to its end.
    clean: bool,
    /// The run's waiting web calls, until the tap takes them.
    pending: Option<tap::Pending>,
    tap: Option<tap::Tap>,
}

impl Drop for ResearchCharge<'_> {
    fn drop(&mut self) {
        let mut outcome = match self.tap.take() {
            Some(tap) => tap.finish(self.clean),
            None => tap::TapOutcome {
                pending: self.pending.take().unwrap_or_default(),
                ..tap::TapOutcome::default()
            },
        };
        let mut used = outcome.used;
        if let Some(run) = self.egress.runs().get_mut(&self.run_id) {
            run.research_in_flight = false;
            run.research_pending = std::mem::take(&mut outcome.pending);
            if self.sent {
                // Charged per call of the turn, once (see `Turn`): the project gets what the
                // turn uses more than before.
                let more = run.research_turn.take(&outcome);
                used.searches = more[0];
                used.fetches = more[1];
                for (kind, total) in run.research_ceiling.iter_mut().enumerate() {
                    // An answer nobody could read may have used all the request allowed:
                    // with what it was charged (`more`, per call of the turn) it costs
                    // that much in all. A result in it of a call charged before is no
                    // new use, so the raw count of the answer is not what is subtracted.
                    let unread = if outcome.read {
                        0
                    } else {
                        self.allowed[kind].saturating_sub(more[kind])
                    };
                    *total = total.saturating_add(more[kind]).saturating_add(unread);
                }
                // Only an answer that was read tells whether the turn paused: after a failed
                // or broken one a resent request still goes on with the turn.
                if outcome.answered {
                    run.research_paused = outcome.paused;
                }
            } else {
                used = ResearchUse::default();
            }
            let total = &mut run.totals.research;
            total.searches = total.searches.saturating_add(used.searches);
            total.fetches = total.fetches.saturating_add(used.fetches);
            total.billed_searches = total.billed_searches.saturating_add(used.billed_searches);
            total.result_bytes = total.result_bytes.saturating_add(used.result_bytes);
            if outcome.block.is_some() {
                run.totals.research_block = outcome.block;
            }
        }
        if self.sent {
            self.grant.sink.used(&used, outcome.block);
        }
    }
}

/// Whether one entry of a body's `tools` list is a tool the core executes.
fn tool_allowed(rule: &ToolRule, tool: &Value) -> bool {
    let Some(tool) = tool.as_object() else {
        return false;
    };
    match rule {
        ToolRule::Typed { types, untyped } => match tool.get("type") {
            None => *untyped,
            Some(kind) => kind.as_str().is_some_and(|kind| types.contains(&kind)),
        },
        ToolRule::Keys(keys) => {
            !tool.is_empty() && tool.keys().all(|key| keys.contains(&key.as_str()))
        }
    }
}

/// The features of a comma-separated feature header that start with an allowed prefix,
/// joined again; `None` when none is left.
fn allowed_features(value: &str, prefixes: &[&str]) -> Option<String> {
    let kept: Vec<&str> = value
        .split(',')
        .map(str::trim)
        .filter(|feature| prefixes.iter().any(|prefix| feature.starts_with(prefix)))
        .collect();
    (!kept.is_empty()).then(|| kept.join(","))
}

/// The audit entry of one request: written when the value is dropped, with whatever is
/// known by then.
struct Record<'a> {
    audit: PendingAudit<'a>,
    facts: Facts,
}

impl Drop for Record<'_> {
    fn drop(&mut self) {
        let facts = &self.facts;
        if let Some(provider) = facts.provider {
            self.audit.name(provider);
        }
        let detail = self.audit.detail();
        detail["status"] = json!(facts.status);
        detail["request_bytes"] = json!(facts.request_bytes);
        detail["response_bytes"] = json!(facts.response_bytes);
        detail["max_tokens"] = json!(facts.max_tokens);
    }
}

/// Facts about one request for its audit entry.
#[derive(Default)]
struct Facts {
    provider: Option<&'static str>,
    status: Option<u16>,
    request_bytes: u64,
    response_bytes: u64,
    max_tokens: Option<u64>,
    /// Refused because the run's request cap is used up.
    over_request_cap: bool,
}

/// Maps a transport error to a fixed phrase: the library's own text can hold a URL.
fn upstream_error(error: reqwest::Error) -> EgressError {
    EgressError::Upstream(if error.is_timeout() {
        "timed out"
    } else if error.is_connect() {
        "could not connect to the provider"
    } else if error.is_body() || error.is_decode() {
        "the response was cut off"
    } else {
        "the request could not be sent"
    })
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Replaces every occurrence of the credential in a byte stream, also when it is split
/// across chunks. Bytes are held back only while they could be the start of the credential.
struct Redactor {
    needle: Vec<u8>,
    carry: Vec<u8>,
}

const REDACTED: &[u8] = b"[redacted]";

impl Redactor {
    fn new(secret: &str) -> Self {
        Self {
            needle: secret.as_bytes().to_vec(),
            carry: Vec::new(),
        }
    }

    fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        if self.needle.is_empty() {
            return chunk.to_vec();
        }
        let mut data = std::mem::take(&mut self.carry);
        data.extend_from_slice(chunk);
        let mut out = Vec::with_capacity(data.len());
        let mut at = 0;
        while at < data.len() {
            let rest = &data[at..];
            if rest.starts_with(&self.needle) {
                out.extend_from_slice(REDACTED);
                at += self.needle.len();
            } else if rest.len() < self.needle.len() && self.needle.starts_with(rest) {
                break;
            } else {
                out.push(data[at]);
                at += 1;
            }
        }
        self.carry = data[at..].to_vec();
        data.zeroize();
        out
    }

    fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.carry)
    }

    fn scrub_text(&self, text: &str) -> String {
        match std::str::from_utf8(&self.needle) {
            Ok(needle) if !needle.is_empty() => text.replace(needle, "[redacted]"),
            _ => text.to_owned(),
        }
    }
}

impl Drop for Redactor {
    fn drop(&mut self) {
        self.needle.zeroize();
        self.carry.zeroize();
    }
}

mod tap;

#[cfg(test)]
mod research_tests;
#[cfg(test)]
pub(crate) mod tests;
