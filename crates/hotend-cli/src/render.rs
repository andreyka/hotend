//! How agent events and core records are shown in a terminal.
//!
//! Everything printed here comes from the agent or from a model script, so it is treated
//! as untrusted text: control characters (which could move the cursor or recolour the
//! terminal) are removed before printing.

use std::io::Write;

use hotend_core::{PrintJob, Version, VersionStatus};
use serde_json::Value;

use crate::table;

/// Width the wrapped text aims for.
pub const WIDTH: usize = 96;

/// Removes control characters except newline and tab.
pub fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

/// The web tools, by the names their events carry: the provider's own, or the gateway's
/// when research goes through a search service. They are worded as what they are.
fn web_tool(event: &Value) -> Option<&'static str> {
    match event["name"].as_str() {
        Some("web_search") => Some("web search"),
        Some("web_fetch") => Some("web page"),
        _ => None,
    }
}

/// What a web call is about: the provider's tools carry a `note` (the query, or the page's
/// host); the gateway's carry their arguments, of which only the query or the host is shown.
fn web_note(event: &Value) -> String {
    if let Some(note) = event["note"].as_str().filter(|note| !note.is_empty()) {
        return note.to_owned();
    }
    if let Some(query) = event["args"]["query"].as_str() {
        return query.chars().take(200).collect();
    }
    event["args"]["url"]
        .as_str()
        .and_then(hotend_core::research::clean_url)
        .map(|url| hotend_core::research::display_host(&url.host))
        .unwrap_or_default()
}

/// One line for a `tool_start` event: the tool and the note the agent gave. For a web
/// search the note is the query, for a fetched page its host.
pub fn tool_start_line(event: &Value) -> String {
    if let Some(what) = web_tool(event) {
        let note = clean(&web_note(event)).replace('\n', " ");
        return if note.is_empty() {
            what.to_owned()
        } else {
            format!("{what}: {note}")
        };
    }
    let name = clean(event["name"].as_str().unwrap_or("?"));
    match event["note"].as_str().filter(|note| !note.is_empty()) {
        Some(note) => format!("tool {name}: {}", clean(note).replace('\n', " ")),
        None => format!("tool {name}"),
    }
}

/// One line for a `tool_end` event: the first line of the result the agent was given.
pub fn tool_end_line(event: &Value) -> String {
    let summary = clean(event["summary"].as_str().unwrap_or("")).replace('\n', " ");
    let mut line = if event["ok"].as_bool().unwrap_or(false) {
        format!("-> {summary}")
    } else if web_tool(event).is_some() {
        // Nothing refused it: the search or the page failed at the provider.
        format!("-> failed: {summary}")
    } else {
        format!("-> refused: {summary}")
    };
    match event["images"].as_u64().unwrap_or(0) {
        0 => {}
        1 => line.push_str(" (1 image shown to the agent)"),
        n => line.push_str(&format!(" ({n} images shown to the agent)")),
    }
    line
}

/// The run's totals from the last `usage` event. Input tokens the provider read from its
/// cache are named when there were any: they cost a fraction of the others.
pub fn usage_line(usage: &Value) -> String {
    let n = |key: &str| usage[key].as_u64().unwrap_or(0);
    let mut line = format!(
        "{} model requests, {} tool calls, {} input and {} output tokens",
        n("requests"),
        n("tool_calls"),
        n("input_tokens"),
        n("output_tokens")
    );
    match n("cache_read_tokens") {
        0 => {}
        cached => line.push_str(&format!(", {cached} read from the provider's cache")),
    }
    line
}

/// Prints agent events as they arrive: text as it streams, one line per tool call.
#[derive(Debug, Default)]
pub struct StreamPrinter {
    /// Whether the cursor is in the middle of a line of agent text.
    mid_line: bool,
    /// Whether the last thing printed was a tool line; the next text gets one blank line.
    after_tool: bool,
    /// The newest `usage` event.
    pub usage: Option<Value>,
    /// The core's `research` event: what the run looked up on the web.
    pub research: Option<Value>,
}

impl StreamPrinter {
    /// Handles one `agent.event` object.
    pub fn event(&mut self, event: &Value) {
        let mut out = std::io::stdout().lock();
        self.write_event(event, &mut out);
        let _ = out.flush();
    }

    fn write_event(&mut self, event: &Value, out: &mut impl Write) {
        match event["type"].as_str() {
            Some("text_delta") => {
                let text = clean(event["text"].as_str().unwrap_or(""));
                // The agent separates its turns with blank lines; at the start of a line
                // they would only add empty space around the tool lines.
                let text = if self.mid_line {
                    text.as_str()
                } else {
                    text.trim_start_matches('\n')
                };
                if let Some(last) = text.chars().last() {
                    if self.after_tool {
                        let _ = writeln!(out);
                        self.after_tool = false;
                    }
                    let _ = write!(out, "{text}");
                    self.mid_line = last != '\n';
                }
            }
            Some(kind @ ("tool_start" | "tool_end")) => {
                self.break_line(out);
                let line = if kind == "tool_start" {
                    tool_start_line(event)
                } else {
                    tool_end_line(event)
                };
                let _ = writeln!(out, "  [{line}]");
                self.after_tool = true;
            }
            Some("usage") => self.usage = Some(event.clone()),
            Some("research") => self.research = Some(event.clone()),
            // Thinking text is not shown.
            _ => {}
        }
    }

    /// Ends the current line of agent text, if one is open.
    pub fn finish(&mut self) {
        let mut out = std::io::stdout().lock();
        self.break_line(&mut out);
    }

    fn break_line(&mut self, out: &mut impl Write) {
        if self.mid_line {
            let _ = writeln!(out);
            self.mid_line = false;
        }
    }
}

/// `text` wrapped to the terminal width, every line indented.
pub fn indented(text: &str, indent: &str) -> String {
    let width = WIDTH.saturating_sub(indent.chars().count());
    let wrapped = format!("{indent}{}", table::wrap(clean(text).trim(), width, indent));
    // An empty line of the text must not become a line of spaces.
    let lines: Vec<&str> = wrapped.lines().map(str::trim_end).collect();
    lines.join("\n")
}

pub fn status_word(status: VersionStatus) -> &'static str {
    match status {
        VersionStatus::Ok => "ok",
        VersionStatus::CheckError => "check_error",
    }
}

/// The versions of a project as a table; `current` is marked, and `from` is the version each
/// was made from, so a line that went back to an earlier version shows where it branched.
pub fn versions_table(versions: &[Version], current: Option<u32>) -> String {
    let rows: Vec<Vec<String>> = versions
        .iter()
        .map(|version| {
            let mark = if Some(version.number) == current {
                " *"
            } else {
                ""
            };
            let params: Vec<String> = version
                .params
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect();
            vec![
                format!("{}{mark}", version.number),
                version
                    .parent
                    .map_or_else(|| "-".to_owned(), |parent| parent.to_string()),
                status_word(version.status).to_owned(),
                clean(&version.origin),
                shorten(&params.join(" "), 48),
                shorten(&clean(&version.note).replace('\n', " "), 60),
            ]
        })
        .collect();
    table::render(
        &["version", "from", "status", "origin", "parameters", "note"],
        &rows,
    )
}

/// The print jobs of a project as a table.
pub fn pending_table(jobs: &[PrintJob]) -> String {
    let rows: Vec<Vec<String>> = jobs
        .iter()
        .map(|job| {
            vec![
                job.id.clone(),
                job.version.to_string(),
                clean(&job.status),
                shorten(&job.slice_sha256, 16),
                shorten(&clean(&job.note).replace('\n', " "), 60),
            ]
        })
        .collect();
    table::render(
        &[
            "print job",
            "version",
            "status",
            "sliced file sha256",
            "note",
        ],
        &rows,
    )
}

/// `text` cut to `max` characters, with `...` when something was removed.
pub fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let kept: String = text.chars().take(max.saturating_sub(3)).collect();
    format!("{kept}...")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    #[test]
    fn control_characters_from_the_agent_never_reach_the_terminal() {
        assert_eq!(clean("a\u{1b}[31mred\u{7}\r\nb\tc"), "a[31mred\nb\tc");
        let line = tool_start_line(&json!({"name": "slice\u{1b}[2J", "note": "one\ntwo"}));
        assert_eq!(line, "tool slice[2J: one two");
    }

    #[test]
    fn tool_lines_say_what_happened() {
        let end = json!({"name": "render_views", "ok": true, "summary": "views of version 2: iso",
            "images": 1});
        assert_eq!(
            tool_end_line(&end),
            "-> views of version 2: iso (1 image shown to the agent)"
        );
        let refused = json!({"ok": false, "summary": "error (rate_limited): too many calls"});
        assert_eq!(
            tool_end_line(&refused),
            "-> refused: error (rate_limited): too many calls"
        );
        let usage = json!({"requests": 5, "tool_calls": 7, "input_tokens": 10, "output_tokens": 3});
        assert_eq!(
            usage_line(&usage),
            "5 model requests, 7 tool calls, 10 input and 3 output tokens"
        );
        let cached = json!({"requests": 2, "tool_calls": 1, "input_tokens": 9000,
            "output_tokens": 40, "cache_read_tokens": 8200, "cache_write_tokens": 300});
        assert_eq!(
            usage_line(&cached),
            "2 model requests, 1 tool calls, 9000 input and 40 output tokens, 8200 read from \
             the provider's cache"
        );
    }

    #[test]
    fn web_calls_read_as_searches_and_pages() {
        let mut printer = StreamPrinter::default();
        let mut out = Vec::new();
        for event in [
            json!({"type": "tool_start", "name": "web_search", "note": "Brenta 600\ndiameter"}),
            json!({"type": "tool_end", "name": "web_search", "ok": true, "summary": "2 results",
                "images": 0}),
            json!({"type": "tool_start", "name": "web_fetch", "note": "maker.example"}),
            json!({"type": "tool_end", "name": "web_fetch", "ok": false,
                "summary": "url_not_accessible", "images": 0}),
            json!({"type": "research", "searches": 1}),
        ] {
            printer.write_event(&event, &mut out);
        }
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "  [web search: Brenta 600 diameter]\n  [-> 2 results]\n  \
             [web page: maker.example]\n  [-> failed: url_not_accessible]\n"
        );
        assert_eq!(printer.research.unwrap()["searches"], 1);
    }

    #[test]
    fn a_web_call_through_a_search_service_shows_its_query_or_host_only() {
        let search = json!({"type": "tool_start", "name": "web_search",
            "args": {"query": "M3 insert\nhole"}});
        assert_eq!(tool_start_line(&search), "web search: M3 insert hole");
        let fetch = json!({"type": "tool_start", "name": "web_fetch",
            "args": {"url": "https://a.b.maker.example/specs?token=x"}});
        assert_eq!(tool_start_line(&fetch), "web page: b.maker.example");
        let odd = json!({"type": "tool_start", "name": "web_fetch",
            "args": {"url": "javascript:alert(1)"}});
        assert_eq!(tool_start_line(&odd), "web page");
    }

    #[test]
    fn a_stream_reads_as_paragraphs_with_tool_lines_between() {
        let mut printer = StreamPrinter::default();
        let mut out = Vec::new();
        for event in [
            json!({"type": "text_delta", "text": "Writing "}),
            json!({"type": "text_delta", "text": "the model."}),
            json!({"type": "tool_start", "name": "write_model_code", "note": "v1"}),
            json!({"type": "tool_end", "name": "write_model_code", "ok": true, "summary": "status: ok"}),
            json!({"type": "text_delta", "text": "\n\n"}),
            json!({"type": "thinking", "text": "hidden"}),
            json!({"type": "text_delta", "text": "Done.\n"}),
            json!({"type": "usage", "requests": 2}),
        ] {
            printer.write_event(&event, &mut out);
        }
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "Writing the model.\n  [tool write_model_code: v1]\n  [-> status: ok]\n\nDone.\n"
        );
        assert_eq!(printer.usage.unwrap()["requests"], 2);
    }

    #[test]
    fn indented_text_has_no_trailing_spaces() {
        assert_eq!(indented("one\n\ntwo", "  "), "  one\n\n  two");
    }

    #[test]
    fn long_cells_are_shortened() {
        assert_eq!(shorten("abcdef", 6), "abcdef");
        assert_eq!(shorten("abcdefg", 6), "abc...");
    }
}
