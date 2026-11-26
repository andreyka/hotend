//! Keenable's REST API (docs.keenable.ai, read 2026-11-20): `POST /v1/search` and
//! `GET /v1/fetch` with the key in `X-API-Key`; the keyless trial path `/v1/search/public`
//! and `/v1/fetch/public` with `X-Keenable-Title` naming the application.
//!
//! Only documented parameters are sent (Keenable answers 400 to an unknown one). Not sent:
//! `objective` (logged by the service), `session_id`, `prompt` (a model's extraction instead
//! of the page's text), `live`, `mode` (the default `pro`).

use serde_json::{Value, json};

use super::{
    APP_TITLE, Answer, CallSpec, Page, SNIPPET_CHARS, SearchError, SearchHit, safe_code,
    status_error, text_field,
};

const SERVICE: &str = "Keenable";

fn path(base: &str, keyed: bool) -> String {
    if keyed {
        base.to_owned()
    } else {
        format!("{base}/public")
    }
}

fn title_header(keyed: bool) -> Vec<(&'static str, String)> {
    if keyed {
        Vec::new()
    } else {
        vec![("X-Keenable-Title", APP_TITLE.to_owned())]
    }
}

/// `POST /v1/search` (or `/public`): the query, the number of results and the excerpt
/// length, nothing else.
pub(crate) fn search_call(query: &str, max_results: u32, keyed: bool) -> CallSpec {
    CallSpec {
        post: true,
        path_and_query: path("/v1/search", keyed),
        body: Some(json!({
            "query": query,
            "max_results": max_results,
            "snippet_max_length": SNIPPET_CHARS,
        })),
        headers: title_header(keyed),
    }
}

/// `GET /v1/fetch?url=..&max_chars=..` (or `/public`).
pub(crate) fn fetch_call(url: &str, max_chars: usize, keyed: bool) -> CallSpec {
    let mut query =
        reqwest::Url::parse("https://query.invalid/").unwrap_or_else(|_| unreachable!());
    query
        .query_pairs_mut()
        .append_pair("url", url)
        .append_pair("max_chars", &max_chars.max(1).to_string());
    CallSpec {
        post: false,
        path_and_query: format!(
            "{}?{}",
            path("/v1/fetch", keyed),
            query.query().unwrap_or("")
        ),
        body: None,
        headers: title_header(keyed),
    }
}

/// The error of an answer that is not 200, by Keenable's stable codes.
fn error(answer: &Answer) -> SearchError {
    let code = safe_code(answer.body.get("code"));
    match (answer.status, code.as_str()) {
        (403, "credentials_forbidden") => SearchError::KeyRejected {
            service: SERVICE,
            status: 403,
        },
        (403, "target_blocked" | "private_url_not_allowed") | (404 | 422, _) | (502, _) => {
            SearchError::PageUnavailable {
                service: SERVICE,
                reason: match (answer.status, code.as_str()) {
                    (403, _) => "the page may not be fetched".to_owned(),
                    (404, _) => "no such page".to_owned(),
                    (422, _) => "the page has no readable text, perhaps behind a login".to_owned(),
                    _ => "the site did not answer".to_owned(),
                },
            }
        }
        (429, "public_domain_requires_api_key") => SearchError::Refused {
            service: SERVICE,
            status: 429,
            code,
        },
        (529, _) => SearchError::Unavailable {
            service: SERVICE,
            status: 529,
        },
        _ => status_error(SERVICE, answer, code),
    }
}

/// `{query, mode, results: [{title, url, snippet?, ...}], usage?}`.
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
                snippet: text_field(result, "snippet"),
            })
        })
        .collect())
}

/// `{url, content, title?, truncated?, total_chars?, ...}`.
pub(crate) fn parse_fetch(answer: &Answer) -> Result<Page, SearchError> {
    if answer.status != 200 {
        return Err(error(answer));
    }
    let Some(content) = answer.body.get("content").and_then(Value::as_str) else {
        return Err(SearchError::BadResponse {
            service: SERVICE,
            detail: "no page text",
        });
    };
    let url = text_field(&answer.body, "url");
    Ok(Page {
        url: (!url.is_empty()).then_some(url),
        title: text_field(&answer.body, "title"),
        text: content.to_owned(),
        truncated: answer.body.get("truncated").and_then(Value::as_bool) == Some(true),
    })
}
