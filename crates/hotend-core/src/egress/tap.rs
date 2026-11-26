//! The response tap: reads, without changing a byte, what the provider's web tools did in
//! one response, and reports it to the run's [`ResearchSink`].
//!
//! It sees the response after the credential redactor, exactly as the agent will. It
//! understands the provider's event stream (one JSON event per `data:` line) and a plain
//! JSON answer. What it cannot read it does not guess: it reports what it saw, the calls
//! that were still waiting, and that the response was not read to its end. The egress then
//! counts the request in full against the run's own ceiling, not against the project.
//!
//! A web call and its result can arrive in different responses: after a paused turn, and
//! when the model calls a web tool in one group with a gateway tool. The calls that wait
//! are kept by the run, handed to the tap of the next response and taken back at its end,
//! so a result is logged with its query or address.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use super::providers::ResearchKind;
use super::research::{ResearchBlock, ResearchGrant, ResearchSink, ResearchUse, is_pdf_result};

/// Longest line (one event) the tap parses. A longer one is skipped.
const MAX_LINE: usize = 4 * 1024 * 1024;
/// How much of a skipped line is kept to tell what it was.
const HEAD: usize = 512;
/// Longest tool input (a query or a URL as JSON) kept while it streams.
const MAX_INPUT: usize = 8 * 1024;
/// Most page text handed to the sink per fetched page.
const MAX_PAGE_TEXT: usize = 200 * 1024;
/// Most bytes of a plain JSON answer the tap keeps to parse it at the end.
const MAX_JSON: usize = 8 * 1024 * 1024;

/// Most web calls a run keeps waiting for their result. A response can hold at most the
/// per-turn cap of each kind; this only bounds what a broken provider could make us keep.
const MAX_PENDING: usize = 64;

/// Web calls that wait for their result, by call id: the kind and the query or address.
pub(super) type Pending = HashMap<String, (ResearchKind, String)>;

/// What a finished response amounted to.
#[derive(Debug, Default)]
pub(super) struct TapOutcome {
    /// What was seen to run. In a response that was not read to its end this includes the
    /// calls that were waiting, because nobody saw whether they ran.
    pub used: ResearchUse,
    /// The response was read and understood to its end, so `used` is all it used.
    pub read: bool,
    pub block: Option<ResearchBlock>,
    /// The calls that still wait for their result after a response that was read.
    pub pending: Pending,
    /// The calls whose result counted against the caps in this response, by id.
    pub counted: Vec<(ResearchKind, String)>,
    /// Every web call this response made, by id (whether or not its result came).
    pub calls: Vec<(ResearchKind, String)>,
    /// The calls the provider answered with `max_uses_exceeded`: they ran nothing.
    pub exceeded: Vec<String>,
    /// Result blocks counted per kind, those without an id included.
    pub results: [u32; 2],
    /// Calls still waiting when an answer broke off, per kind (counted, as nobody saw
    /// whether they ran).
    pub waiting: [u32; 2],
    /// The provider's own counts in this response (0 when it gave none).
    pub reported: [u32; 2],
    /// The response ended with `stop_reason: pause_turn`.
    pub paused: bool,
    /// The provider answered with status 200 and the answer was read to its end.
    pub answered: bool,
}

/// A web tool call whose input is still streaming.
struct Call {
    id: String,
    kind: ResearchKind,
    input: String,
}

pub(super) struct Tap {
    sink: Arc<dyn ResearchSink>,
    status: u16,
    streamed: bool,
    allow_pdf: bool,
    max_result_bytes: usize,
    /// The unfinished line of an event stream, or the whole body of a JSON answer.
    buffer: Vec<u8>,
    /// The current line is too long and is being skipped.
    skipping: bool,
    /// Calls by content block index, until their input is complete.
    open: HashMap<u64, Call>,
    /// The query or URL of calls that still wait for their result, by call id: the run's
    /// from earlier responses and this response's own.
    asked: Pending,
    /// The `max_uses` the request carried, per kind: the most one response can have used.
    caps: [u32; 2],
    /// Result blocks seen, per kind (not counting `max_uses_exceeded`).
    results: [u32; 2],
    /// Searches that came back with a result list: what the provider bills.
    found: u32,
    /// The provider's own counts, when it reported them.
    reported: Option<[u32; 2]>,
    result_bytes: u64,
    block: Option<ResearchBlock>,
    /// The calls whose result counted.
    counted: Vec<(ResearchKind, String)>,
    /// Every web call seen in this response.
    calls: Vec<(ResearchKind, String)>,
    /// Calls answered `max_uses_exceeded`.
    exceeded: Vec<String>,
    /// The message ended with `pause_turn`.
    paused: bool,
    /// The end of the message was seen.
    ended: bool,
    /// Something could not be read.
    unreadable: bool,
}

fn kind_of_tool(name: &str) -> Option<ResearchKind> {
    match name {
        "web_search" => Some(ResearchKind::Search),
        "web_fetch" => Some(ResearchKind::Fetch),
        _ => None,
    }
}

/// An error code as the provider gave it when it is a short lower-case word, else `unknown`.
fn error_code(value: Option<&Value>) -> String {
    match value.and_then(Value::as_str) {
        Some(code)
            if !code.is_empty()
                && code.len() <= 40
                && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') =>
        {
            code.to_owned()
        }
        _ => "unknown".to_owned(),
    }
}

impl Tap {
    /// A tap for the response to a request whose web entries carried `caps` as `max_uses`
    /// (0 for a kind it did not carry). `pending` are the run's calls that wait for a result.
    pub(super) fn new(
        status: u16,
        streamed: bool,
        grant: &ResearchGrant,
        caps: [u32; 2],
        pending: Pending,
    ) -> Self {
        Self {
            sink: Arc::clone(&grant.sink),
            status,
            streamed,
            allow_pdf: grant.allow_pdf,
            max_result_bytes: grant.max_result_bytes,
            buffer: Vec::new(),
            skipping: false,
            open: HashMap::new(),
            asked: pending,
            caps,
            results: [0; 2],
            found: 0,
            reported: None,
            result_bytes: 0,
            block: None,
            counted: Vec::new(),
            calls: Vec::new(),
            exceeded: Vec::new(),
            paused: false,
            ended: false,
            unreadable: false,
        }
    }

    /// Feeds the next piece of the response body.
    pub(super) fn push(&mut self, data: &[u8]) {
        if !self.streamed || self.status != 200 {
            if self.buffer.len() + data.len() <= MAX_JSON {
                self.buffer.extend_from_slice(data);
            } else {
                self.unreadable = true;
            }
            return;
        }
        let mut rest = data;
        while let Some(end) = rest.iter().position(|byte| *byte == b'\n') {
            self.extend_line(&rest[..end]);
            self.end_line();
            rest = &rest[end + 1..];
        }
        self.extend_line(rest);
    }

    fn extend_line(&mut self, piece: &[u8]) {
        if self.skipping {
            return;
        }
        if self.buffer.len() + piece.len() > MAX_LINE {
            // Keep the head: it says what kind of event is being skipped.
            let room = HEAD.saturating_sub(self.buffer.len());
            self.buffer
                .extend_from_slice(&piece[..room.min(piece.len())]);
            self.buffer.truncate(HEAD);
            self.skipping = true;
        } else {
            self.buffer.extend_from_slice(piece);
        }
    }

    fn end_line(&mut self) {
        let line = std::mem::take(&mut self.buffer);
        if std::mem::take(&mut self.skipping) {
            self.skipped(&line);
            return;
        }
        let Some(data) = line.strip_prefix(b"data:") else {
            return;
        };
        match serde_json::from_slice::<Value>(data) {
            Ok(event) => self.event(&event),
            // `data: [DONE]` and the like are not JSON and not ours.
            Err(_) if data.len() < 64 => {}
            Err(_) => self.unreadable = true,
        }
    }

    /// An event too long to parse. A web result that large must not be kept.
    fn skipped(&mut self, head: &[u8]) {
        let head = String::from_utf8_lossy(head);
        let kind = if head.contains("web_fetch_tool_result") {
            Some(ResearchKind::Fetch)
        } else if head.contains("web_search_tool_result") {
            Some(ResearchKind::Search)
        } else {
            None
        };
        match kind {
            Some(kind) => {
                self.results[kind as usize] += 1;
                self.result_bytes = self.result_bytes.saturating_add(MAX_LINE as u64);
                self.block = Some(ResearchBlock::TooLarge);
                self.sink.call(kind, "", "too_large");
            }
            None => self.unreadable = true,
        }
    }

    fn event(&mut self, event: &Value) {
        let index = event.get("index").and_then(Value::as_u64).unwrap_or(0);
        match event.get("type").and_then(Value::as_str) {
            Some("content_block_start") => {
                if let Some(block) = event.get("content_block") {
                    self.block_start(index, block);
                }
            }
            Some("content_block_delta") => {
                let delta = event.get("delta");
                let field = |name: &str| delta.and_then(|d| d.get(name));
                match field("type").and_then(Value::as_str) {
                    Some("input_json_delta") => {
                        let part = field("partial_json").and_then(Value::as_str);
                        if let (Some(call), Some(part)) = (self.open.get_mut(&index), part)
                            && call.input.len() + part.len() <= MAX_INPUT
                        {
                            call.input.push_str(part);
                        }
                    }
                    Some("citations_delta") => {
                        if let Some(citation) = field("citation") {
                            self.citation(citation);
                        }
                    }
                    _ => {}
                }
            }
            Some("content_block_stop") => {
                if let Some(call) = self.open.remove(&index) {
                    self.asked(call);
                }
            }
            Some("message_start") => self.usage(event.pointer("/message/usage")),
            Some("message_delta") => {
                self.usage(event.get("usage"));
                self.paused |= event.pointer("/delta/stop_reason").and_then(Value::as_str)
                    == Some("pause_turn");
                self.ended = true;
            }
            _ => {}
        }
    }

    fn block_start(&mut self, index: u64, block: &Value) {
        let text = |name: &str| block.get(name).and_then(Value::as_str).unwrap_or("");
        match text("type") {
            "server_tool_use" => {
                if let Some(kind) = kind_of_tool(text("name")) {
                    let input = match block.get("input") {
                        Some(Value::Object(object)) if !object.is_empty() => {
                            Value::Object(object.clone()).to_string()
                        }
                        _ => String::new(),
                    };
                    self.open.insert(
                        index,
                        Call {
                            id: text("id").to_owned(),
                            kind,
                            input,
                        },
                    );
                }
            }
            "web_search_tool_result" => self.result(ResearchKind::Search, block),
            "web_fetch_tool_result" => self.result(ResearchKind::Fetch, block),
            "text" => {
                for citation in block
                    .get("citations")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    self.citation(citation);
                }
            }
            _ => {}
        }
    }

    /// A call's input is complete: remember what was asked until its result arrives.
    fn asked(&mut self, call: Call) {
        let input: Value = serde_json::from_str(&call.input).unwrap_or(Value::Null);
        let key = match call.kind {
            ResearchKind::Search => "query",
            ResearchKind::Fetch => "url",
        };
        let text = input.get(key).and_then(Value::as_str).unwrap_or("");
        if !call.id.is_empty() && self.calls.len() < MAX_PENDING {
            self.calls.push((call.kind, call.id.clone()));
        }
        if self.asked.len() < MAX_PENDING {
            self.asked.insert(call.id, (call.kind, text.to_owned()));
        }
    }

    fn citation(&mut self, citation: &Value) {
        let text = |name: &str| citation.get(name).and_then(Value::as_str);
        if text("type") == Some("web_search_result_location")
            && let (Some(url), Some(cited)) = (text("url"), text("cited_text"))
        {
            self.sink.evidence(url, cited, true);
        }
    }

    fn usage(&mut self, usage: Option<&Value>) {
        let Some(counts) = usage.and_then(|usage| usage.get("server_tool_use")) else {
            return;
        };
        // A count that does not fit, or that no response to this request can mean, is not
        // taken at its word: one response adds at most what its request allowed.
        let count = |name: &str, cap: u32| {
            let value = counts.get(name).and_then(Value::as_u64).unwrap_or(0);
            u32::try_from(value).unwrap_or(u32::MAX).min(cap)
        };
        let now = [
            count("web_search_requests", self.caps[0]),
            count("web_fetch_requests", self.caps[1]),
        ];
        let before = self.reported.unwrap_or([0; 2]);
        self.reported = Some([before[0].max(now[0]), before[1].max(now[1])]);
    }

    /// A result block: complete in one event.
    fn result(&mut self, kind: ResearchKind, block: &Value) {
        let id = block
            .get("tool_use_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        let asked = self
            .asked
            .remove(id)
            .map(|(_, text)| text)
            .unwrap_or_default();
        let bytes = block.to_string().len();
        self.result_bytes = self.result_bytes.saturating_add(bytes as u64);
        let content = block.get("content");
        let mut counts = true;
        let mut outcome = match (kind, content) {
            (ResearchKind::Search, Some(Value::Array(results))) => {
                for result in results {
                    let text = |name: &str| result.get(name).and_then(Value::as_str).unwrap_or("");
                    if !text("url").is_empty() {
                        self.sink.source(kind, text("url"), text("title"));
                    }
                }
                self.found += 1;
                format!("{} results", results.len())
            }
            (ResearchKind::Fetch, Some(content))
                if content.get("type").and_then(Value::as_str) == Some("web_fetch_result") =>
            {
                let url = content.get("url").and_then(Value::as_str).unwrap_or("");
                let document = content.get("content");
                let title = document
                    .and_then(|d| d.get("title"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                // The page was read either way: it is a source of this run.
                if !url.is_empty() {
                    self.sink.source(kind, url, title);
                }
                if is_pdf_result(block) && !self.allow_pdf {
                    self.block = Some(ResearchBlock::Pdf);
                    "pdf_not_kept".to_owned()
                } else {
                    let page = document
                        .and_then(|d| d.pointer("/source/data"))
                        .and_then(Value::as_str)
                        .filter(|_| !is_pdf_result(block));
                    if let Some(page) = page {
                        let mut end = page.len().min(MAX_PAGE_TEXT);
                        while !page.is_char_boundary(end) {
                            end -= 1;
                        }
                        self.sink.evidence(url, &page[..end], false);
                    }
                    "ok".to_owned()
                }
            }
            (_, content) => {
                let code = error_code(content.and_then(|c| c.get("error_code")));
                // The provider ran nothing for a call past its own limit.
                counts = code != "max_uses_exceeded";
                if !counts && !id.is_empty() && self.exceeded.len() < MAX_PENDING {
                    self.exceeded.push(id.to_owned());
                }
                code
            }
        };
        if bytes > self.max_result_bytes && self.block.is_none() {
            self.block = Some(ResearchBlock::TooLarge);
            outcome = "too_large".to_owned();
        }
        if counts {
            self.results[kind as usize] += 1;
            if !id.is_empty() && self.counted.len() < MAX_PENDING {
                self.counted.push((kind, id.to_owned()));
            }
        }
        self.sink.call(kind, &asked, &outcome);
    }

    /// A plain JSON answer, read once at the end: the message's content blocks, or the
    /// error that says a web tool is switched off for the account.
    fn whole_body(&mut self) {
        let body = std::mem::take(&mut self.buffer);
        let Ok(json) = serde_json::from_slice::<Value>(&body) else {
            self.unreadable = self.unreadable || self.status == 200;
            return;
        };
        if self.status != 200 {
            let message = json
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            for (kind, name) in [
                (ResearchKind::Search, "web search"),
                (ResearchKind::Fetch, "web fetch"),
            ] {
                if self.status == 400 && message.contains(name) && message.contains("not enabled") {
                    self.sink.refused(kind);
                }
            }
            return;
        }
        for (index, block) in json
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            self.block_start(index as u64, block);
            if let Some(call) = self.open.remove(&(index as u64)) {
                self.asked(call);
            }
        }
        self.usage(json.get("usage"));
        self.paused = json.get("stop_reason").and_then(Value::as_str) == Some("pause_turn");
        self.ended = json.get("stop_reason").is_some();
    }

    /// Ends the response and says what it used. `clean` says whether the transport
    /// delivered the response to its end.
    pub(super) fn finish(mut self, clean: bool) -> TapOutcome {
        if !self.streamed || self.status != 200 {
            self.whole_body();
        } else if !self.buffer.is_empty() || self.skipping {
            // The stream stopped in the middle of a line.
            self.unreadable = true;
        }
        let read = self.status != 200 || (clean && self.ended && !self.unreadable);
        let mut waiting = [0u32; 2];
        if !read {
            // Nobody saw whether the calls that were waiting ran: they count, are logged
            // once and are not waited for any longer.
            let open = std::mem::take(&mut self.open);
            for call in open.into_values() {
                self.asked(call);
            }
            let mut calls: Vec<_> = std::mem::take(&mut self.asked).into_values().collect();
            calls.sort_by(|a, b| a.1.cmp(&b.1));
            for (kind, text) in calls {
                waiting[kind as usize] += 1;
                self.sink.call(kind, &text, "interrupted");
            }
        }
        let reported = self.reported.unwrap_or([0; 2]);
        let seen = |kind: usize| {
            self.results[kind]
                .saturating_add(waiting[kind])
                .max(reported[kind])
        };
        TapOutcome {
            used: ResearchUse {
                searches: seen(0),
                fetches: seen(1),
                // The provider's own count is what it bills. Without a final count (the
                // stream was cut), the searches that returned results stand in for it.
                billed_searches: match self.reported {
                    Some(counts) if read => counts[0],
                    _ => self.found.max(reported[0]),
                },
                result_bytes: self.result_bytes,
            },
            read,
            block: self.block,
            counted: self.counted,
            calls: self.calls,
            exceeded: self.exceeded,
            results: self.results,
            waiting,
            reported,
            paused: self.paused,
            answered: read && self.status == 200,
            // Calls that got no result in this response: the turn was paused, or the call
            // waits for a gateway tool of the same group.
            pending: self.asked,
        }
    }
}
