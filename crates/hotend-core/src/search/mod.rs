//! Web research through a search service (Keenable, Tavily) instead of the AI provider's
//! own web tools (docs/WEB_RESEARCH.md, section 16).
//!
//! One small interface, [`SearchClient`]: `search(query, limits)` and `read_page(url,
//! limits)`. Page reading goes through the service's own page fetch; the core never fetches
//! a web site itself.
//!
//! Guarantees:
//! - A client talks to one host only, the chosen service's API host (`api.keenable.ai`,
//!   `api.tavily.com`), over HTTPS. Its base address is fixed in a release build; only the
//!   crate's own tests can point it at a stand-in on loopback.
//! - No redirect is followed: an answer that redirects is refused, whatever host it names.
//! - Every answer is read under a size cap and the whole call under a time limit.
//! - The key is added here, to the outbound request, and appears nowhere else: not in an
//!   error, not in a log line, not in anything returned. Error texts are fixed sentences;
//!   nothing the service wrote is passed on except a validated short code, and every text
//!   that leaves this module is scrubbed of the key once more.
//! - A request carries the model's query or the page address and the service's own
//!   switches, nothing about the person, the project or the machine. The keyless path names
//!   the application by its product name, `Hotend`, as Keenable's documentation asks.
//! - The keyless path respects Keenable's published limits (10 requests a second and 1,000
//!   an hour per internet address): requests are spaced, a local hourly ceiling stays below
//!   the shared one, and after a 429 nothing is sent until the time the service named.
//!   Redistributors must check Keenable's terms.

#[cfg(test)]
mod example_tests;
#[cfg(test)]
mod flow_tests;
pub(crate) mod keenable;
pub(crate) mod run;
pub(crate) mod tavily;
#[cfg(test)]
mod tests;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use zeroize::Zeroizing;

use crate::secrets::Secret;

/// Where web research comes from (`Settings.research.source`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchSource {
    /// The AI provider's own web search and web fetch (Anthropic). The default.
    #[default]
    BuiltIn,
    /// Keenable's search and fetch API, with the person's key or keyless.
    Keenable,
    /// Tavily's search and extract API, with the person's key.
    Tavily,
    /// No web research at all.
    Off,
}

impl SearchSource {
    /// Every source, in the order a list shows them.
    pub const ALL: [Self; 4] = [Self::BuiltIn, Self::Keenable, Self::Tavily, Self::Off];

    /// The wire name: `built_in`, `keenable`, `tavily`, `off`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BuiltIn => "built_in",
            Self::Keenable => "keenable",
            Self::Tavily => "tavily",
            Self::Off => "off",
        }
    }

    /// The search service behind this source, if it is one.
    pub fn service(self) -> Option<SearchService> {
        match self {
            Self::Keenable => Some(SearchService::Keenable),
            Self::Tavily => Some(SearchService::Tavily),
            Self::BuiltIn | Self::Off => None,
        }
    }
}

/// A search service Hotend can call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchService {
    Keenable,
    Tavily,
}

impl SearchService {
    /// Every service.
    pub const ALL: [Self; 2] = [Self::Keenable, Self::Tavily];

    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keenable => "keenable",
            Self::Tavily => "tavily",
        }
    }

    /// The name shown to the person.
    pub fn label(self) -> &'static str {
        match self {
            Self::Keenable => "Keenable",
            Self::Tavily => "Tavily",
        }
    }

    /// The one host a client of this service talks to.
    pub fn host(self) -> &'static str {
        match self {
            Self::Keenable => "api.keenable.ai",
            Self::Tavily => "api.tavily.com",
        }
    }

    /// The environment variable the key is read from with `HOTEND_SECRETS=env`.
    pub fn env_var(self) -> &'static str {
        match self {
            Self::Keenable => "KEENABLE_API_KEY",
            Self::Tavily => "TAVILY_API_KEY",
        }
    }

    /// Whether the service can be used without a key (Keenable's keyless trial path).
    pub fn keyless(self) -> bool {
        matches!(self, Self::Keenable)
    }

    /// The source that names this service.
    pub fn source(self) -> SearchSource {
        match self {
            Self::Keenable => SearchSource::Keenable,
            Self::Tavily => SearchSource::Tavily,
        }
    }

    fn base(self) -> &'static str {
        match self {
            Self::Keenable => "https://api.keenable.ai",
            Self::Tavily => "https://api.tavily.com",
        }
    }
}

/// The secret name of a search service's key.
pub fn search_key_name(service: SearchService) -> String {
    format!("search-key.{}", service.as_str())
}

/// The name the keyless path identifies the application with: the product name only.
pub const APP_TITLE: &str = "Hotend";

/// The excerpt length asked of a service, and the longest passage the research store keeps
/// as a citation: the model sees exactly what Hotend can check a quote against.
pub const SNIPPET_CHARS: usize = 400;

/// Bounds of one call.
#[derive(Debug, Clone)]
pub struct SearchLimits {
    /// Results asked for, 1 to 10.
    pub max_results: u32,
    /// Most characters of a page's text asked for and kept.
    pub page_chars: usize,
    /// Largest answer read from the service, in bytes.
    pub max_response_bytes: usize,
    /// Time for the whole call, connection included.
    pub timeout: Duration,
}

impl Default for SearchLimits {
    fn default() -> Self {
        Self {
            max_results: 5,
            page_chars: 24_000,
            max_response_bytes: 1024 * 1024,
            timeout: Duration::from_secs(20),
        }
    }
}

/// One search result, as the service gave it (not yet cleaned).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// A page's text, read through the service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// The address the service says it read (after its own redirects), when it says.
    pub url: Option<String>,
    pub title: String,
    pub text: String,
    /// Whether the text was cut off, by the service or here.
    pub truncated: bool,
}

/// Why a call did not give a result. The texts are fixed: never the key, never text the
/// service wrote (only a validated short code).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SearchError {
    #[error("{service} needs an API key, and none is stored")]
    NeedsKey { service: &'static str },
    #[error(
        "{service} did not accept the API key (HTTP {status}); store a valid key in the settings"
    )]
    KeyRejected { service: &'static str, status: u16 },
    #[error("{}", rate_message(service, *retry_after_secs, *keyless_hourly))]
    RateLimited {
        service: &'static str,
        retry_after_secs: u64,
        keyless_hourly: bool,
        /// Whether the request was sent (a 429), or refused here before sending.
        sent: bool,
    },
    #[error("{service} says the account has no credits or reached its plan limit (HTTP {status})")]
    OutOfCredits { service: &'static str, status: u16 },
    #[error("{service} could not read that page ({reason})")]
    PageUnavailable {
        service: &'static str,
        reason: String,
    },
    #[error("{service} refused the request (HTTP {status}{})", code_suffix(code))]
    Refused {
        service: &'static str,
        status: u16,
        code: String,
    },
    #[error("{service} could not be reached or had an error{}", status_suffix(*status))]
    Unavailable { service: &'static str, status: u16 },
    #[error("{service} did not answer within {seconds} seconds")]
    Timeout { service: &'static str, seconds: u64 },
    #[error("{service}'s answer was larger than Hotend reads ({limit} bytes)")]
    TooLarge { service: &'static str, limit: usize },
    #[error("{service} answered with a redirect, which Hotend does not follow")]
    Redirect { service: &'static str },
    #[error("{service}'s answer could not be read ({detail})")]
    BadResponse {
        service: &'static str,
        detail: &'static str,
    },
    #[error("the request to {service} could not be sent ({detail})")]
    Setup {
        service: &'static str,
        detail: &'static str,
    },
}

fn code_suffix(code: &str) -> String {
    if code.is_empty() {
        String::new()
    } else {
        format!(", {code}")
    }
}

fn status_suffix(status: u16) -> String {
    if status == 0 {
        String::new()
    } else {
        format!(" (HTTP {status})")
    }
}

fn rate_message(service: &str, retry_after: u64, keyless_hourly: bool) -> String {
    let wait = if retry_after >= 120 {
        format!("about {} minutes", retry_after.div_ceil(60))
    } else {
        format!("{retry_after} seconds")
    };
    if keyless_hourly {
        format!(
            "{service}'s free keyless limit (1,000 requests an hour, shared by everyone on this \
             internet address) is used up; it resets in {wait}. A free {service} key lifts it: \
             add one in Hotend's settings"
        )
    } else {
        format!("{service} asks Hotend to slow down; try again in {wait}")
    }
}

impl SearchError {
    /// The stable code a tool result and the research log carry.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NeedsKey { .. } => "search_key_missing",
            Self::KeyRejected { .. } => "search_key_rejected",
            Self::RateLimited { .. } => "search_rate_limited",
            Self::OutOfCredits { .. } => "search_out_of_credits",
            Self::PageUnavailable { .. } => "page_unavailable",
            Self::Refused { .. } => "search_refused",
            Self::Unavailable { .. } | Self::Setup { .. } => "search_unavailable",
            Self::Timeout { .. } => "search_timeout",
            Self::TooLarge { .. } => "search_response_too_large",
            Self::Redirect { .. } => "search_redirect_refused",
            Self::BadResponse { .. } => "search_bad_response",
        }
    }

    /// Whether a request left this computer (and so counts against the caps).
    pub fn was_sent(&self) -> bool {
        match self {
            Self::NeedsKey { .. } | Self::Setup { .. } => false,
            Self::RateLimited { sent, .. } => *sent,
            _ => true,
        }
    }
}

/// A machine code from a service's error body: lower-case letters and underscores, short.
pub(crate) fn safe_code(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .filter(|code| {
            !code.is_empty()
                && code.len() <= 48
                && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        })
        .unwrap_or("")
        .to_owned()
}

/// A text field of a service's answer; missing or not text is empty.
pub(crate) fn text_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

/// The first `max` characters of `text`, and whether it was longer.
pub(crate) fn first_chars(text: &str, max: usize) -> (String, bool) {
    match text.char_indices().nth(max) {
        Some((at, _)) => (text[..at].to_owned(), true),
        None => (text.to_owned(), false),
    }
}

/// The parts of an HTTP request an adapter builds. The key is added by [`SearchClient`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CallSpec {
    pub post: bool,
    /// Path below the base, with its query string.
    pub path_and_query: String,
    pub body: Option<Value>,
    /// Headers the adapter adds itself (never the key).
    pub headers: Vec<(&'static str, String)>,
}

/// What came back, read under the size cap.
#[derive(Debug, Clone)]
pub(crate) struct Answer {
    pub status: u16,
    /// `Retry-After` in seconds, when the service sent it as a number.
    pub retry_after: Option<u64>,
    /// The JSON body; `Null` when it was not JSON.
    pub body: Value,
}

/// How long to wait after a 429: the header, else the body's `retry_after_seconds`, else a
/// minute; never more than an hour.
pub(crate) fn retry_after(answer: &Answer) -> u64 {
    answer
        .retry_after
        .or_else(|| {
            answer
                .body
                .get("retry_after_seconds")
                .and_then(Value::as_u64)
        })
        .unwrap_or(60)
        .clamp(1, 3600)
}

// ---- the keyless rate gate ----------------------------------------------------------------

/// Spacing between keyless requests: under Keenable's 10 a second per address.
const KEYLESS_MIN_GAP: Duration = Duration::from_millis(150);
/// Keyless requests this process sends in an hour: below the 1,000 the address shares.
pub(crate) const KEYLESS_PER_HOUR: usize = 900;
const HOUR: Duration = Duration::from_secs(3600);

/// When keyless requests may be sent. One per process for the real service.
#[derive(Debug, Default)]
pub(crate) struct Gate {
    /// Nothing is sent before this, after a 429.
    blocked_until: Option<Instant>,
    blocked_hourly: bool,
    /// When the requests of the last hour were (or will be) sent.
    sent: VecDeque<Instant>,
}

fn keyless_gate() -> Arc<Mutex<Gate>> {
    static GATE: OnceLock<Arc<Mutex<Gate>>> = OnceLock::new();
    Arc::clone(GATE.get_or_init(|| Arc::new(Mutex::new(Gate::default()))))
}

impl Gate {
    /// Reserves a slot: how long to wait before sending, or why not to send at all.
    pub(crate) fn reserve(
        &mut self,
        now: Instant,
        service: &'static str,
    ) -> Result<Duration, SearchError> {
        let local = |seconds: u64, hourly: bool| SearchError::RateLimited {
            service,
            retry_after_secs: seconds.max(1),
            keyless_hourly: hourly,
            sent: false,
        };
        if let Some(until) = self.blocked_until {
            if until > now {
                return Err(local((until - now).as_secs(), self.blocked_hourly));
            }
            self.blocked_until = None;
            self.blocked_hourly = false;
        }
        while self
            .sent
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= HOUR)
        {
            self.sent.pop_front();
        }
        if self.sent.len() >= KEYLESS_PER_HOUR {
            let oldest = self.sent.front().copied().unwrap_or(now);
            let wait = HOUR.saturating_sub(now.saturating_duration_since(oldest));
            return Err(local(wait.as_secs(), true));
        }
        let at = self
            .sent
            .back()
            .map_or(now, |last| (*last + KEYLESS_MIN_GAP).max(now));
        self.sent.push_back(at);
        Ok(at - now)
    }

    /// After a 429: nothing more until the service's time has passed.
    pub(crate) fn block(&mut self, now: Instant, seconds: u64, hourly: bool) {
        self.blocked_until = Some(now + Duration::from_secs(seconds));
        self.blocked_hourly = hourly;
    }
}

// ---- the client -------------------------------------------------------------------------

/// The one interface the core uses to search and read pages through a service.
pub struct SearchClient {
    service: SearchService,
    key: Option<Secret>,
    base: reqwest::Url,
    /// The host every request must go to.
    host: String,
    gate: Arc<Mutex<Gate>>,
}

impl std::fmt::Debug for SearchClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchClient")
            .field("service", &self.service)
            .field("key", &self.key.as_ref().map(|_| "<redacted>"))
            .field("host", &self.host)
            .finish_non_exhaustive()
    }
}

impl SearchClient {
    /// A client of the real service, with the person's key or (Keenable) none.
    pub(crate) fn new(service: SearchService, key: Option<Secret>) -> Self {
        Self {
            service,
            key,
            // A fixed, valid address.
            base: reqwest::Url::parse(service.base()).unwrap_or_else(|_| unreachable!()),
            host: service.host().to_owned(),
            gate: keyless_gate(),
        }
    }

    /// A client of a stand-in service on loopback. Test builds only: a release build has no
    /// way to send a key or a query anywhere but the service's own host.
    #[cfg(test)]
    pub(crate) fn with_base(service: SearchService, key: Option<Secret>, base: &str) -> Self {
        let base = reqwest::Url::parse(base).unwrap_or_else(|_| unreachable!());
        Self {
            service,
            key,
            host: base.host_str().unwrap_or("").to_owned(),
            base,
            gate: Arc::new(Mutex::new(Gate::default())),
        }
    }

    /// The service this client calls.
    pub fn service(&self) -> SearchService {
        self.service
    }

    /// Whether the person's key is used (else the keyless path).
    pub fn has_key(&self) -> bool {
        self.key.is_some()
    }

    /// Searches the web. `query` is the model's text, sent as it is.
    pub async fn search(
        &self,
        query: &str,
        limits: &SearchLimits,
    ) -> Result<Vec<SearchHit>, SearchError> {
        let max_results = limits.max_results.clamp(1, 10);
        let spec = match self.service {
            SearchService::Keenable => keenable::search_call(query, max_results, self.has_key()),
            SearchService::Tavily => tavily::search_call(query, max_results),
        };
        let answer = self.send(spec, limits).await?;
        self.after_answer(match self.service {
            SearchService::Keenable => keenable::parse_search(&answer),
            SearchService::Tavily => tavily::parse_search(&answer),
        })
    }

    /// Reads a page through the service's own fetch. `url` is checked by the caller.
    pub async fn read_page(&self, url: &str, limits: &SearchLimits) -> Result<Page, SearchError> {
        let spec = match self.service {
            SearchService::Keenable => keenable::fetch_call(url, limits.page_chars, self.has_key()),
            SearchService::Tavily => tavily::extract_call(url),
        };
        let answer = self.send(spec, limits).await?;
        let mut page = self.after_answer(match self.service {
            SearchService::Keenable => keenable::parse_fetch(&answer),
            SearchService::Tavily => tavily::parse_extract(&answer, url),
        })?;
        let (text, cut) = first_chars(&page.text, limits.page_chars);
        page.text = text;
        page.truncated |= cut;
        Ok(page)
    }

    /// After an answer was read: a keyless rate limit the service reported (a 429 the
    /// adapter read as a rate limit, per second or per hour) stops further keyless requests
    /// until the time it named. A 429 with another meaning (Keenable's
    /// `public_domain_requires_api_key`, a refusal of one site) does not.
    fn after_answer<T>(&self, result: Result<T, SearchError>) -> Result<T, SearchError> {
        if let Err(SearchError::RateLimited {
            retry_after_secs,
            keyless_hourly,
            sent: true,
            ..
        }) = &result
            && self.key.is_none()
        {
            let mut gate = self.gate.lock().unwrap_or_else(|p| p.into_inner());
            gate.block(Instant::now(), *retry_after_secs, *keyless_hourly);
        }
        result
    }

    /// Scrubs the key out of a text, should it ever be in one.
    pub(crate) fn redact(&self, text: &str) -> String {
        match &self.key {
            Some(key) if !key.expose().is_empty() => text.replace(key.expose(), "<redacted>"),
            _ => text.to_owned(),
        }
    }

    fn label(&self) -> &'static str {
        self.service.label()
    }

    async fn send(&self, spec: CallSpec, limits: &SearchLimits) -> Result<Answer, SearchError> {
        let service = self.label();
        let setup = |detail| SearchError::Setup { service, detail };
        if self.key.is_none() && !self.service.keyless() {
            return Err(SearchError::NeedsKey { service });
        }
        let url = self
            .base
            .join(&spec.path_and_query)
            .map_err(|_| setup("bad address"))?;
        // The allow list: one host per adapter, whatever an adapter built.
        if url.host_str() != Some(self.host.as_str()) || url.scheme() != self.base.scheme() {
            return Err(setup("not the service's host"));
        }
        let keyless = self.key.is_none();
        if keyless {
            let wait = {
                let mut gate = self.gate.lock().unwrap_or_else(|p| p.into_inner());
                gate.reserve(Instant::now(), service)?
            };
            if !wait.is_zero() {
                tokio::time::sleep(wait).await;
            }
        }

        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(limits.timeout.min(Duration::from_secs(10)))
            .timeout(limits.timeout)
            .referer(false)
            .user_agent(APP_TITLE);
        if cfg!(test) && url.scheme() == "http" {
            // The stand-in is on loopback; never through a proxy.
            builder = builder.no_proxy();
        } else {
            builder = builder.https_only(true);
        }
        let http = builder.build().map_err(|_| setup("no HTTP client"))?;
        let mut request = if spec.post {
            http.post(url)
        } else {
            http.get(url)
        };
        for (name, value) in &spec.headers {
            request = request.header(*name, value);
        }
        if let Some(key) = &self.key {
            // The one copy made here is wiped when it is dropped; the header value the
            // transport holds is marked sensitive (reqwest keeps it out of its debug output).
            let (name, value) = match self.service {
                SearchService::Keenable => ("X-API-Key", Zeroizing::new(key.expose().to_owned())),
                SearchService::Tavily => (
                    "Authorization",
                    Zeroizing::new(format!("Bearer {}", key.expose())),
                ),
            };
            let mut value = reqwest::header::HeaderValue::from_str(value.as_str())
                .map_err(|_| setup("bad key"))?;
            value.set_sensitive(true);
            request = request.header(name, value);
        }
        if let Some(body) = &spec.body {
            request = request
                .header("Content-Type", "application/json")
                .body(body.to_string());
        }
        let seconds = limits.timeout.as_secs();
        let fail = |error: reqwest::Error| {
            // The error is never formatted: it could name the address.
            if error.is_timeout() {
                SearchError::Timeout { service, seconds }
            } else if error.is_redirect() {
                SearchError::Redirect { service }
            } else {
                SearchError::Unavailable { service, status: 0 }
            }
        };
        let mut response = request.send().await.map_err(fail)?;
        let status = response.status().as_u16();
        if (300..400).contains(&status) || response.url().host_str() != Some(self.host.as_str()) {
            return Err(SearchError::Redirect { service });
        }
        let retry = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok());
        if response
            .content_length()
            .is_some_and(|length| length > limits.max_response_bytes as u64)
        {
            return Err(SearchError::TooLarge {
                service,
                limit: limits.max_response_bytes,
            });
        }
        let mut bytes: Vec<u8> = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(fail)? {
            if bytes.len() + chunk.len() > limits.max_response_bytes {
                return Err(SearchError::TooLarge {
                    service,
                    limit: limits.max_response_bytes,
                });
            }
            bytes.extend_from_slice(&chunk);
        }
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        // A 429 blocks the keyless gate once the adapter has read it (`after_answer`).
        Ok(Answer {
            status,
            retry_after: retry,
            body,
        })
    }
}

/// The error for an answer that is not a success, shared by the adapters' common cases.
pub(crate) fn status_error(service: &'static str, answer: &Answer, code: String) -> SearchError {
    let status = answer.status;
    match status {
        401 => SearchError::KeyRejected { service, status },
        402 | 432 | 433 => SearchError::OutOfCredits { service, status },
        429 => SearchError::RateLimited {
            service,
            retry_after_secs: retry_after(answer),
            keyless_hourly: code == "public_rate_limit_hourly",
            sent: true,
        },
        500..=599 => SearchError::Unavailable { service, status },
        _ => SearchError::Refused {
            service,
            status,
            code,
        },
    }
}
