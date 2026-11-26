//! Tavily's REST API (docs.tavily.com, read 2026-11-20): `POST /search` and
//! `POST /extract`, the key as `Authorization: Bearer`. Tavily has no keyless path here.
//!
//! What is sent keeps the cost and the content to the plain case: `search_depth` `basic`
//! (one credit), no model-written answer (`include_answer` false), no raw page content in
//! a search, and `auto_parameters` false (it could switch to the dearer `advanced`). An
//! extract asks for one address, `basic`, as markdown, without a `query` (which would
//! rerank the page into chunks instead of giving its text).

use serde_json::{Value, json};

use super::{Answer, CallSpec, Page, SearchError, SearchHit, status_error, text_field};

const SERVICE: &str = "Tavily";

/// `POST /search`.
pub(crate) fn search_call(query: &str, max_results: u32) -> CallSpec {
    CallSpec {
        post: true,
        path_and_query: "/search".to_owned(),
        body: Some(json!({
            "query": query,
            "max_results": max_results,
            "search_depth": "basic",
            "include_answer": false,
            "include_raw_content": false,
            "include_images": false,
            "auto_parameters": false,
        })),
        headers: Vec::new(),
    }
}

/// `POST /extract` for one address.
pub(crate) fn extract_call(url: &str) -> CallSpec {
    CallSpec {
        post: true,
        path_and_query: "/extract".to_owned(),
        body: Some(json!({
            "urls": [url],
            "extract_depth": "basic",
            "format": "markdown",
            "include_images": false,
        })),
        headers: Vec::new(),
    }
}

fn error(answer: &Answer) -> SearchError {
    // Tavily's error bodies are `{"detail": {"error": "<text>"}}`: free text, never passed
    // on. The status says enough.
    match answer.status {
        403 => SearchError::KeyRejected {
            service: SERVICE,
            status: 403,
        },
        434 => SearchError::Unavailable {
            service: SERVICE,
            status: 434,
        },
        _ => status_error(SERVICE, answer, String::new()),
    }
}

/// `{query, results: [{title, url, content, score, ...}], ...}`. `content` is the passage
/// Tavily picked from the page.
pub(crate) fn parse_search(answer: &Answer) -> Result<Vec<SearchHit>, SearchError> {
    if answer.status != 200 {
        return Err(error(answer));
    }
    let results =
        answer
            .body
            .get("results")
            .and_then(Value::as_array)
            .ok_or(SearchError::BadResponse {
                service: SERVICE,
                detail: "no result list",
            })?;
    Ok(results
        .iter()
        .filter_map(|result| {
            let url = text_field(result, "url");
            (!url.is_empty()).then(|| SearchHit {
                title: text_field(result, "title"),
                url,
                snippet: text_field(result, "content"),
            })
        })
        .collect())
}

/// Whether a result's address is the one asked for: the same cleaned address (no fragment),
/// a final slash aside.
fn same_address(result: &str, asked: &str) -> bool {
    let key = |raw: &str| {
        crate::research::clean_url(raw).map(|url| url.url.trim_end_matches('/').to_owned())
    };
    matches!((key(result), key(asked)), (Some(a), Some(b)) if a == b)
}

/// `{results: [{url, raw_content}], failed_results: [{url, error}], ...}`: both lists are
/// looked at, also with HTTP 200. Only the result for the address that was asked for is
/// taken: a result for another address is not that page.
pub(crate) fn parse_extract(answer: &Answer, asked: &str) -> Result<Page, SearchError> {
    if answer.status != 200 {
        return Err(error(answer));
    }
    let results = answer.body.get("results").and_then(Value::as_array);
    let page = results.into_iter().flatten().find(|result| {
        result.get("raw_content").and_then(Value::as_str).is_some()
            && same_address(&text_field(result, "url"), asked)
    });
    if let Some(page) = page {
        let url = text_field(page, "url");
        return Ok(Page {
            url: (!url.is_empty()).then_some(url),
            title: String::new(),
            text: text_field(page, "raw_content"),
            truncated: false,
        });
    }
    let failed = answer
        .body
        .get("failed_results")
        .and_then(Value::as_array)
        .is_some_and(|failed| !failed.is_empty());
    if failed || results.is_some() {
        return Err(SearchError::PageUnavailable {
            service: SERVICE,
            reason: if failed {
                "the site could not be read".to_owned()
            } else {
                format!("no text came back for {}", crate::research::host_of(asked))
            },
        });
    }
    Err(SearchError::BadResponse {
        service: SERVICE,
        detail: "no result list",
    })
}
