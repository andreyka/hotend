//! Web research through the provider: the grant a run needs, the entries the egress writes
//! and the rules a request with research must keep.
//!
//! The agent process has no network and gets none here. The provider itself offers two
//! tools that run on its side, web search and web fetch. For a run that holds a
//! [`ResearchGrant`] the egress writes those two tool entries into the request, from the
//! grant and the provider table; the agent only marks where they go. Guarantees:
//! - Without a grant nothing changes: a web tool entry is refused like every other
//!   provider-side tool.
//! - With a grant only the two basic tool types of the table are written, pinned to direct
//!   calls, with the grant's caps. Nothing the agent wrote in its marker is sent, except an
//!   exact `cache_control`.
//! - Every other provider-side tool or option stays refused.
//! - The egress counts what a run used and holds a run to its grant's caps exactly,
//!   whatever the agent process sends: each request's `max_uses` lets the provider run what
//!   the run has left and no more ([`max_uses`]); within a provider turn the entries keep
//!   their bytes, and a turn that starts with nothing left of a kind is not offered it.
//! - What a project is charged is what was seen to run. An answer that could not be read
//!   to its end counts in full against the run's own ceiling only.
//! - A fetched PDF or an oversized result is never sent again.
//! - A web result block is looked through like every other block: nothing inside it may
//!   name an address or a stored file for the provider to load.

use std::sync::Arc;

use serde_json::{Map, Value, json};

use super::providers::{ResearchKind, ServerToolRule};

/// Most searches or fetches a grant may allow a run (one message).
pub const MAX_USES_PER_TURN: u32 = 10;
/// The range of the token cap on one fetched page.
pub const CONTENT_TOKENS: std::ops::RangeInclusive<u32> = 1000..=20_000;
/// The range of the size cap on one result block, in bytes of JSON.
pub const RESULT_BYTES: std::ops::RangeInclusive<usize> = 16 * 1024..=2 * 1024 * 1024;
/// Size cap on one result block when no grant sets one.
pub const DEFAULT_RESULT_BYTES: usize = 256 * 1024;
/// Most domain entries written into a tool entry.
pub const MAX_DOMAINS: usize = 20;

/// What a response with web research used, as the egress counted it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResearchUse {
    /// Searches counted against the caps (failed ones included).
    pub searches: u32,
    /// Fetches counted against the caps (failed ones included).
    pub fetches: u32,
    /// Searches the provider reported as billed.
    pub billed_searches: u32,
    /// Bytes of JSON of the result blocks: they stay in the conversation.
    pub result_bytes: u64,
}

/// Why the egress will not let a run's conversation be stored or sent again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResearchBlock {
    /// A fetched page was a PDF. The provider does not cap its size.
    Pdf,
    /// A result block was larger than the cap.
    TooLarge,
}

/// Where the egress reports what the provider's web tools did. The core implements it over
/// the project store; the egress itself keeps nothing.
///
/// Every string comes from the provider's response and so, in the end, from a web page:
/// an implementation must treat it as untrusted text.
pub trait ResearchSink: Send + Sync {
    /// A page the provider found or read: `url` and `title` as the result gave them.
    fn source(&self, kind: ResearchKind, url: &str, title: &str);
    /// Text Hotend can later compare a quote with: a fetched page's text, or the passage
    /// a search citation quoted. Never given back to the model.
    fn evidence(&self, url: &str, text: &str, citation: bool);
    /// One call of a web tool: the query or the URL, and `ok`, `N results` or an error code.
    fn call(&self, kind: ResearchKind, text: &str, outcome: &str);
    /// What one response used. Called once per response that carried web tools.
    fn used(&self, used: &ResearchUse, block: Option<ResearchBlock>);
    /// The provider refused the request because this tool is switched off for the account.
    fn refused(&self, kind: ResearchKind);
}

/// Permission for one run to use the provider's web search and web fetch, with its caps.
/// Built by the core from the stored settings, never from anything in the conversation.
#[derive(Clone)]
pub struct ResearchGrant {
    /// Searches the run may make in all, at least 1: the per-message cap, or what the
    /// project has left when that is less.
    pub searches_per_turn: u32,
    /// Fetches the run may make in all, the same way; 0 means the run has no fetch.
    pub fetches_per_turn: u32,
    /// Token cap on one fetched page (text only; the provider does not cap PDFs).
    pub max_content_tokens: u32,
    /// Host names the tools are limited to; empty for no limit.
    pub allowed_domains: Vec<String>,
    /// Host names the tools must avoid. Ignored when `allowed_domains` is not empty.
    pub blocked_domains: Vec<String>,
    /// Whether links found inside a fetched page may be fetched too.
    pub follow_links: bool,
    /// Whether a fetched PDF may stay in the conversation (within `max_result_bytes`).
    pub allow_pdf: bool,
    /// Size cap on one result block, in bytes of JSON.
    pub max_result_bytes: usize,
    pub sink: Arc<dyn ResearchSink>,
}

impl std::fmt::Debug for ResearchGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResearchGrant")
            .field("searches_per_turn", &self.searches_per_turn)
            .field("fetches_per_turn", &self.fetches_per_turn)
            .field("max_content_tokens", &self.max_content_tokens)
            .finish_non_exhaustive()
    }
}

/// Whether `text` can stand in a domain list: a lower-case ASCII host name with at least
/// two labels, no scheme, port, path, wildcard or IP address.
pub fn is_host_name(text: &str) -> bool {
    let labels: Vec<&str> = text.split('.').collect();
    text.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
        && labels
            .last()
            .is_some_and(|tld| tld.bytes().any(|b| b.is_ascii_lowercase()))
}

impl ResearchGrant {
    /// The grant with every value forced into the range the egress accepts, whatever the
    /// caller passed.
    pub(super) fn clamped(mut self) -> Self {
        self.searches_per_turn = self.searches_per_turn.clamp(1, MAX_USES_PER_TURN);
        self.fetches_per_turn = self.fetches_per_turn.min(MAX_USES_PER_TURN);
        self.max_content_tokens = self
            .max_content_tokens
            .clamp(*CONTENT_TOKENS.start(), *CONTENT_TOKENS.end());
        self.max_result_bytes = self
            .max_result_bytes
            .clamp(*RESULT_BYTES.start(), *RESULT_BYTES.end());
        for list in [&mut self.allowed_domains, &mut self.blocked_domains] {
            list.retain(|entry| is_host_name(entry));
            list.truncate(MAX_DOMAINS);
        }
        if !self.allowed_domains.is_empty() {
            self.blocked_domains.clear();
        }
        self
    }

    /// What the run may use of one kind in all.
    pub fn max_uses(&self, kind: ResearchKind) -> u32 {
        match kind {
            ResearchKind::Search => self.searches_per_turn,
            ResearchKind::Fetch => self.fetches_per_turn,
        }
    }

    /// The tool entry the egress sends in place of the agent's `marker`, with `max_uses` as
    /// [`max_uses`] works it out. `with_search` says whether the request also carries the
    /// search entry, which the fetch entry refers to.
    pub(super) fn entry(
        &self,
        rule: &ServerToolRule,
        marker: &Value,
        max_uses: u32,
        with_search: bool,
    ) -> Value {
        let mut entry = Map::new();
        entry.insert("type".to_owned(), json!(rule.written_type));
        entry.insert("name".to_owned(), json!(rule.name));
        entry.insert("max_uses".to_owned(), json!(max_uses));
        if rule.write_allowed_callers {
            entry.insert("allowed_callers".to_owned(), json!(["direct"]));
        }
        if !self.allowed_domains.is_empty() {
            entry.insert("allowed_domains".to_owned(), json!(self.allowed_domains));
        } else if !self.blocked_domains.is_empty() {
            entry.insert("blocked_domains".to_owned(), json!(self.blocked_domains));
        }
        if rule.kind == ResearchKind::Fetch {
            entry.insert(
                "max_content_tokens".to_owned(),
                json!(self.max_content_tokens),
            );
            // No `citations`: the evidence for a fetched page is its text, which the tap
            // hands to the sink. Citations would only split the answer into more blocks.
            // Which links may be fetched. Never one from a gateway tool result (script
            // output, error text). From the provider's own results: search results only,
            // unless the project lets the model follow links inside fetched pages.
            let from_results = if !with_search {
                json!({"type": "none"})
            } else if self.follow_links {
                json!({"type": "all"})
            } else {
                json!({"type": "only", "tools": [{"type": "tool_reference", "name": "web_search"}]})
            };
            entry.insert(
                "url_sources".to_owned(),
                json!({"client_tool_results": {"type": "none"},
                       "server_tool_results": from_results}),
            );
        }
        if let Some(cache) = exact_cache_control(marker.get("cache_control")) {
            entry.insert("cache_control".to_owned(), cache);
        }
        Value::Object(entry)
    }
}

/// A marker's `cache_control`, copied only when it is exactly the ephemeral marker with an
/// optional lifetime of five minutes or one hour.
fn exact_cache_control(value: Option<&Value>) -> Option<Value> {
    let object = value?.as_object()?;
    if object.get("type").and_then(Value::as_str) != Some("ephemeral") {
        return None;
    }
    match (object.len(), object.get("ttl").and_then(Value::as_str)) {
        (1, None) => Some(json!({"type": "ephemeral"})),
        (2, Some(ttl @ ("5m" | "1h"))) => Some(json!({"type": "ephemeral", "ttl": ttl})),
        _ => None,
    }
}

/// The `max_uses` to write into a web tool entry of a request that goes on with the
/// provider's turn: `written`, what the turn's first request wrote (what the run had left
/// then), unless that could let the provider run more than the run has left now. 0 means
/// the entry is not written. (A request that starts a new turn writes what is left.)
///
/// The provider counts `max_uses` per turn, and a turn can span several requests: a request
/// whose last message holds only tool results, or that resumes a paused turn, goes on with
/// it, and the calls of the turn's earlier requests count (docs/WEB_RESEARCH.md, section 3).
/// So writing `written` again lets the provider run `written` less the turn's calls it
/// sees. Of those, `verified` are the ones the egress itself saw this turn's responses
/// make ([`calls_in_turn`]); the provider counts at least these. As long as `left +
/// verified` is not less than `written`, the value depends on the turn's start alone and
/// the tool entries keep their bytes from request to request of a tool loop: after a
/// failed call, after a kind ran out (the provider then answers `max_uses_exceeded`, which
/// runs and counts nothing), and however the calls were charged, since each call is charged
/// once. Only a conversation that leaves out calls the run was charged for (or an answer
/// that could not be read, whose calls nobody can name) gets the smaller `left +
/// verified`, so an agent process that hides, copies or makes up calls gets no more.
pub(super) fn max_uses(written: u32, left: u32, verified: u32) -> u32 {
    written.min(left.saturating_add(verified))
}

/// Whether one message holds tool results and nothing else.
fn only_tool_results(message: &Value) -> bool {
    message
        .get("content")
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            !blocks.is_empty()
                && blocks
                    .iter()
                    .all(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        })
}

/// Whether a request with these `messages` goes on with the provider's turn: its last
/// message holds only tool results, or is the model's own and `paused` says the run's last
/// answer really ended with `pause_turn`. Anything else starts a new turn.
pub(super) fn continues_turn(messages: Option<&Value>, paused: bool) -> bool {
    let Some(last) = messages.and_then(Value::as_array).and_then(|m| m.last()) else {
        return false;
    };
    match last.get("role").and_then(Value::as_str) {
        Some("assistant") => paused,
        Some("user") => only_tool_results(last),
        _ => false,
    }
}

/// The calls of each web tool in the provider turn the last message belongs to (the
/// messages after the last user message that is not only tool results) that the egress saw
/// this turn's responses make (`seen`, by id), whether their result came (a failed call
/// counts, one still waiting too) or not. One answered `max_uses_exceeded` ran nothing and
/// does not count. Each id counts once, however often it is written.
pub(super) fn calls_in_turn(
    messages: Option<&Value>,
    seen: &std::collections::HashMap<String, ResearchKind>,
) -> [u32; 2] {
    let Some(messages) = messages.and_then(Value::as_array) else {
        return [0; 2];
    };
    let start = messages
        .iter()
        .rposition(|message| {
            message.get("role").and_then(Value::as_str) == Some("user")
                && !only_tool_results(message)
        })
        .map_or(0, |index| index + 1);
    let blocks = || {
        messages[start..]
            .iter()
            .filter(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
            .filter_map(|message| message.get("content").and_then(Value::as_array))
            .flatten()
    };
    let exceeded: std::collections::HashSet<&str> = blocks()
        .filter(|block| {
            matches!(
                block.get("type").and_then(Value::as_str),
                Some("web_search_tool_result" | "web_fetch_tool_result")
            ) && block.pointer("/content/error_code").and_then(Value::as_str)
                == Some("max_uses_exceeded")
        })
        .filter_map(|block| block.get("tool_use_id").and_then(Value::as_str))
        .collect();
    let mut once = std::collections::HashSet::new();
    let mut calls = [0u32; 2];
    for block in blocks() {
        if block.get("type").and_then(Value::as_str) != Some("server_tool_use") {
            continue;
        }
        let kind = match block.get("name").and_then(Value::as_str) {
            Some("web_search") => ResearchKind::Search,
            Some("web_fetch") => ResearchKind::Fetch,
            _ => continue,
        };
        let Some(id) = block.get("id").and_then(Value::as_str) else {
            continue;
        };
        if seen.get(id) == Some(&kind) && !exceeded.contains(id) && once.insert(id) {
            calls[kind as usize] = calls[kind as usize].saturating_add(1);
        }
    }
    calls
}

/// Whether a JSON value holds the key `cache_control` anywhere (a prompt-cache marker).
pub(super) fn has_cache_marker(value: &Value, depth: u32) -> bool {
    if depth > 12 {
        return false;
    }
    match value {
        Value::Object(object) => object.iter().any(|(key, inner)| {
            (key == "cache_control" && !inner.is_null()) || has_cache_marker(inner, depth + 1)
        }),
        Value::Array(items) => items.iter().any(|item| has_cache_marker(item, depth + 1)),
        _ => false,
    }
}

/// What the conversation of a request must not hold.
pub(super) struct BlockLimits<'a> {
    /// `source.type` values that make the provider load something.
    pub denied_sources: &'a [&'a str],
    /// Block types of provider-side tools other than web research.
    pub denied_types: &'a [&'a str],
    /// The names a `server_tool_use` block may carry: the web tools. Empty for a provider
    /// without them, where such blocks are not looked at.
    pub server_tools: &'a [&'a str],
    /// Whether web result blocks are looked at (the provider has research at all).
    pub web_results: bool,
    pub allow_pdf: bool,
    pub max_result_bytes: usize,
}

/// Deepest nesting of a request's conversation that is looked through. Past it the request
/// is refused: what cannot be checked is not sent.
pub(super) const MAX_BLOCK_DEPTH: u32 = 32;

/// `source.type` values a fetched page may have inside a `web_fetch_tool_result`: the page's
/// text, or the bytes of a PDF. Anything else would make the provider load something.
const FETCHED_SOURCES: [&str; 2] = ["text", "base64"];

/// Walks the conversation part of a request body and returns why it may not be sent, if a
/// block breaks `limits`. Web result blocks are looked through like every other block: a
/// source the provider would load is refused inside them too.
pub(super) fn check_blocks(value: &Value, limits: &BlockLimits<'_>, depth: u32) -> Option<String> {
    if depth > MAX_BLOCK_DEPTH {
        return Some(format!(
            "the conversation is nested more than {MAX_BLOCK_DEPTH} levels deep, which Hotend \
             does not look through; send flatter content blocks"
        ));
    }
    match value {
        Value::Array(items) => items
            .iter()
            .find_map(|item| check_blocks(item, limits, depth + 1)),
        Value::Object(object) => {
            let source = object
                .get("source")
                .and_then(|source| source.get("type"))
                .and_then(Value::as_str);
            if let Some(kind) = source.filter(|kind| limits.denied_sources.contains(kind)) {
                return Some(format!(
                    "a content block with a `{kind}` source is not allowed: the provider \
                     would load it from the network; send the content itself"
                ));
            }
            let kind = object.get("type").and_then(Value::as_str);
            if let Some(kind) = kind.filter(|kind| limits.denied_types.contains(kind)) {
                return Some(format!(
                    "a content block of type `{kind}` is not allowed: it belongs to a tool \
                     the provider runs for itself, which Hotend does not use"
                ));
            }
            if kind == Some("server_tool_use") && !limits.server_tools.is_empty() {
                let name = object.get("name").and_then(Value::as_str);
                if !name.is_some_and(|name| limits.server_tools.contains(&name)) {
                    return Some(
                        "the conversation holds a call of a provider-side tool other than \
                         web search and web fetch, which Hotend does not use; leave it out"
                            .to_owned(),
                    );
                }
            }
            if limits.web_results
                && let Some(kind @ ("web_search_tool_result" | "web_fetch_tool_result")) = kind
            {
                if kind == "web_fetch_tool_result" {
                    let fetched = value.pointer("/content/content/source");
                    let fetched_kind = fetched
                        .and_then(|source| source.get("type"))
                        .and_then(Value::as_str);
                    if fetched.is_some_and(|source| !source.is_null())
                        && !fetched_kind.is_some_and(|kind| FETCHED_SOURCES.contains(&kind))
                    {
                        return Some(
                            "a fetched page in the conversation names a source other than its \
                             own text or bytes, which the provider would load; send the web \
                             result back exactly as the provider gave it"
                                .to_owned(),
                        );
                    }
                    if !limits.allow_pdf && is_pdf_result(value) {
                        return Some(
                            "the conversation holds a fetched PDF, which Hotend does not send \
                             again: it would be paid for with every later request. Ask the \
                             maker for the number, or allow PDFs for this project \
                             (`hotend research pdf on`)"
                                .to_owned(),
                        );
                    }
                }
                let bytes = value.to_string().len();
                if bytes > limits.max_result_bytes {
                    return Some(format!(
                        "the conversation holds a web result of {bytes} bytes; the limit is \
                         {}. It would be paid for with every later request: ask the maker \
                         for the number instead",
                        limits.max_result_bytes
                    ));
                }
            }
            object
                .values()
                .find_map(|inner| check_blocks(inner, limits, depth + 1))
        }
        _ => None,
    }
}

/// Whether a `web_fetch_tool_result` block carries a document as base64 (a PDF).
pub(super) fn is_pdf_result(block: &Value) -> bool {
    block
        .pointer("/content/content/source/type")
        .and_then(Value::as_str)
        == Some("base64")
}
