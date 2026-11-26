//! Web research for a project: whether a run may use the provider's web search and web
//! fetch, what the project has used, and the numbers the assistant found on the web.
//!
//! The network side is in [`crate::egress`]: the provider runs the searches, the egress
//! writes the two tool entries and reads the responses. This module decides and remembers.
//! Guarantees:
//! - A run gets research only when the provider supports it, the project has it on, the
//!   person has agreed once that queries leave this computer, and the project has not used
//!   up its allowance. Otherwise nothing changes for the run, and the state says why.
//! - Everything that came from a web page (titles, addresses, page text, quotes) is stored
//!   and shown as data. None of it is ever executed, used as a path or given a capability.
//! - A number from the web is kept as a note with its page, the quoted sentence and how far
//!   Hotend could check that sentence. Until the person confirms or rejects it, it is
//!   listed among the questions for every version and in the print review.
//! - The key of the provider is not in reach of this module.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::egress::research::is_host_name;
use crate::egress::{
    ProviderKind, ResearchBlock, ResearchGrant, ResearchKind, ResearchSink, ResearchUse,
};
use crate::engine::{CheckStatus, EngineResult, Question};
use crate::search::run::{ServiceGrant, ServiceGrantSpec};
use crate::search::{SearchClient, SearchService, SearchSource};
use crate::secrets::SecretStoreKind;
use crate::settings::{MAX_RESEARCH_PER_PROJECT, ResearchSettings, Settings};
use crate::store::{NewResearchNote, ResearchNote, ResearchRow, Store, StoreError};
use crate::util::sha256_hex;

/// Units a note's value may have.
pub const NOTE_UNITS: [&str; 5] = ["mm", "deg", "count", "ratio", "g"];
/// Units of length a quoted value may be converted from.
pub const QUOTED_UNITS: [&str; 4] = ["mm", "cm", "m", "in"];
/// Notes a project keeps in use; past it the oldest unconfirmed one is retired.
pub const MAX_ACTIVE_NOTES: usize = 40;
/// Longest quote of a note, in characters.
pub const MAX_QUOTE_CHARS: usize = 200;
/// The evidence of a note Hotend made itself, for a number the model set while it was
/// reading the web without recording a page for it. Such a note has no address and no quote.
pub const UNSOURCED: &str = "unsourced";
/// Provenances a number from the web must not hide behind: a parameter tagged with one of
/// them in a run that read the web, with no note, is asked about.
const CLAIMED_PROVENANCES: [&str; 3] = ["measured", "standard", "calibrated"];
/// Most questions of that kind one build adds.
const MAX_UNSOURCED_PER_BUILD: usize = 8;
/// Sites named in the notes block for a page that was not kept.
const MAX_BLOCKED_HOSTS: usize = 5;
/// Room left for the block around a web result when the agent is told the size limit: the
/// agent measures the result's content, the egress the whole block.
pub(crate) const RESULT_WRAPPER_BYTES: usize = 256;
/// Shortest cited passage that counts as evidence, in characters.
const MIN_CITATION_CHARS: usize = 8;
/// Longest address Hotend keeps.
const MAX_URL_BYTES: usize = 2000;
/// Sources listed in the summary of one run.
const MAX_RUN_SOURCES: usize = 12;

/// What a project says about web research. Missing values follow the global settings.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ProjectResearch {
    /// On or off for this project; `None` follows `Settings.research.default_on`.
    pub enabled: Option<bool>,
    /// Host names the provider's tools are limited to. Empty for no limit.
    pub allowed_domains: Vec<String>,
    /// Host names the tools must avoid. Not together with `allowed_domains`.
    pub blocked_domains: Vec<String>,
    /// Whether links found inside a fetched page may be fetched too. Off by default: with
    /// it a page can make the provider request addresses of its choosing.
    pub follow_links: bool,
    /// Whether a fetched PDF may stay in the conversation. Off by default: the provider
    /// does not cap a PDF's size, and it is paid for again with every later request.
    pub allow_pdf: bool,
    /// This project's own cap on searches.
    pub searches_per_project: Option<u32>,
    /// This project's own cap on fetched pages.
    pub fetches_per_project: Option<u32>,
}

impl ProjectResearch {
    /// Checks the values a person or an interface can get wrong.
    pub fn validate(&self) -> Result<(), String> {
        for (name, list) in [
            ("allowed_domains", &self.allowed_domains),
            ("blocked_domains", &self.blocked_domains),
        ] {
            if list.len() > crate::egress::research::MAX_DOMAINS {
                return Err(format!(
                    "{name} can hold at most {} host names",
                    crate::egress::research::MAX_DOMAINS
                ));
            }
            if let Some(bad) = list.iter().find(|entry| !is_host_name(entry)) {
                return Err(format!(
                    "`{}` in {name} is not a host name; write it like `example.com`, in lower \
                     case, without `https://`, a path, a port or a `*`",
                    plain(bad, 80)
                ));
            }
        }
        if !self.allowed_domains.is_empty() && !self.blocked_domains.is_empty() {
            return Err(
                "use either allowed_domains or blocked_domains, not both: the provider accepts \
                 only one of them"
                    .to_owned(),
            );
        }
        let cap_ok = |cap: Option<u32>, min: u32| {
            cap.is_none_or(|cap| (min..=MAX_RESEARCH_PER_PROJECT).contains(&cap))
        };
        if !cap_ok(self.searches_per_project, 1) || !cap_ok(self.fetches_per_project, 0) {
            return Err(format!(
                "searches_per_project must be between 1 and {MAX_RESEARCH_PER_PROJECT} and \
                 fetches_per_project between 0 and {MAX_RESEARCH_PER_PROJECT}"
            ));
        }
        Ok(())
    }

    /// The settings stored with a project. Values that no longer parse count as unset.
    pub(crate) fn from_row(row: &ResearchRow) -> Self {
        serde_json::from_value(row.settings.clone()).unwrap_or_default()
    }
}

/// Where a project stands with web research.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResearchState {
    /// The next message may search.
    On,
    /// Switched off for the project (or no provider is chosen).
    Off,
    /// On, but the person has not yet agreed that queries leave this computer.
    NeedsConsent,
    /// The project's allowance is used up. The assistant is told to ask instead.
    BudgetUsed,
    /// Used well past the allowance: the next message gets no web tools at all.
    Stopped,
    /// The chosen provider has no web research in Hotend.
    UnsupportedProvider,
    /// The provider said web search is switched off for the account.
    ProviderRefused,
    /// The chosen search service needs an API key and none is stored (Tavily).
    NeedsKey,
}

/// A project's research state with the numbers behind it, for the window and the command
/// line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResearchStatus {
    pub state: ResearchState,
    /// The state in one plain sentence, with what to do about it.
    pub message: String,
    /// Whether the next message gets web search.
    pub search: bool,
    /// Whether the next message gets web fetch.
    pub fetch: bool,
    pub searches_used: u32,
    pub searches_cap: u32,
    pub fetches_used: u32,
    pub fetches_cap: u32,
    pub searches_per_turn: u32,
    pub fetches_per_turn: u32,
    /// Searches the provider reported as billed, and their fee in millionths of a dollar.
    pub billed_searches: u32,
    pub search_fee_micro_usd: u64,
    /// Bytes of web results in the project's conversation, their cap and a rough token
    /// count (they are read again by every later request, at the cached price).
    pub result_bytes: u64,
    pub result_bytes_cap: u64,
    pub result_tokens_estimate: u64,
    /// Whether the person has agreed that queries leave this computer, for the source that
    /// is chosen now (an agreement for another source does not count).
    pub acknowledged: bool,
    /// The project's own settings.
    pub project: ProjectResearch,
    /// Where research comes from (`Settings.research.source`).
    pub source: SearchSource,
    /// The host that receives the queries: the search service's API host, or empty for the
    /// provider's own search and for `off`.
    pub service_host: String,
    /// Whether a key for the chosen search service is stored. Always false for `built_in`
    /// and `off`. The key itself is never in any answer.
    pub key_present: bool,
    /// Whether the chosen service is used without a key (Keenable's keyless trial path).
    pub keyless: bool,
}

impl ResearchStatus {
    /// Whether a run gets a grant at all.
    pub fn granted(&self) -> bool {
        self.search
    }
}

/// The fee of `searches` billed searches with a provider, in millionths of a dollar.
fn search_fee(provider: Option<ProviderKind>, searches: u32) -> u64 {
    provider
        .and_then(|kind| kind.rule().body.research)
        .map_or(0, |rule| {
            u64::from(searches) * u64::from(rule.search_price_micro_usd)
        })
}

/// Whether the person's agreement covers `source`: an agreement is given for one source
/// (one receiver of the queries) and does not carry over to another. An agreement stored
/// before sources existed was given for the provider's own search.
pub fn consent_covers(global: &ResearchSettings, source: SearchSource) -> bool {
    global.acknowledged_at_ms.is_some()
        && global.acknowledged_source.unwrap_or(SearchSource::BuiltIn) == source
}

/// Whether the provider has web research of its own in Hotend (Anthropic so far).
pub fn provider_has_builtin(kind: ProviderKind) -> bool {
    kind.rule().body.research.is_some()
}

/// Why the provider's own search cannot be used with `kind`, for the person.
fn builtin_unavailable(kind: ProviderKind) -> String {
    format!(
        "The AI provider's own web search is not available with {}: Hotend has it with \
         Anthropic only. Choose a search service as the research source instead: Keenable \
         works without a key or an account. Everything else works as before.",
        kind.as_str()
    )
}

/// Works out the state of a project from the settings and its stored row. For a search
/// service as the source this assumes no key is stored; see [`status_with_key`].
pub fn status(settings: &Settings, row: &ResearchRow) -> ResearchStatus {
    status_with_key(settings, row, false)
}

/// What the key store says about the chosen search service's key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchKeyState {
    /// No key is stored (with `HOTEND_SECRETS=env`: the variable is not set).
    Absent,
    /// A key is stored.
    Present,
    /// The key store could not be asked. Whether a key is stored is unknown, so Keenable's
    /// keyless path is not used in its place.
    Unreadable,
}

/// Works out the state of a project. `key_present` says whether a key for the chosen search
/// service is stored; it is not looked at for `built_in` and `off`.
pub fn status_with_key(
    settings: &Settings,
    row: &ResearchRow,
    key_present: bool,
) -> ResearchStatus {
    status_with_key_state(
        settings,
        row,
        if key_present {
            SearchKeyState::Present
        } else {
            SearchKeyState::Absent
        },
    )
}

/// Works out the state of a project with what the key store said about the chosen search
/// service's key. A key store that could not be asked is `needs_key`, for Keenable too:
/// Hotend does not fall back to the keyless path while a key may be stored.
pub fn status_with_key_state(
    settings: &Settings,
    row: &ResearchRow,
    key: SearchKeyState,
) -> ResearchStatus {
    let global = &settings.research;
    let project = ProjectResearch::from_row(row);
    let provider = settings.provider.as_ref().map(|provider| provider.kind);
    let source = global.source;
    let service = source.service();
    let key_present = service.is_some() && key == SearchKeyState::Present;
    let key_unreadable = service.is_some() && key == SearchKeyState::Unreadable;
    let searches_cap = project
        .searches_per_project
        .unwrap_or(global.searches_per_project);
    let fetches_cap = project
        .fetches_per_project
        .unwrap_or(global.fetches_per_project);
    let bytes_cap = global.max_project_result_bytes;
    let enabled = project.enabled.unwrap_or(global.default_on);
    let supported = match source {
        SearchSource::BuiltIn => provider.is_some_and(provider_has_builtin),
        SearchSource::Keenable | SearchSource::Tavily => true,
        SearchSource::Off => false,
    };
    // A refusal by the provider concerns its own search only.
    let refused_search = source == SearchSource::BuiltIn && row.refused_search;
    let refused_fetch = source == SearchSource::BuiltIn && row.refused_fetch;

    let (state, message) = if provider.is_none() {
        (
            ResearchState::Off,
            "Web research needs an AI provider: choose one in the settings first.".to_owned(),
        )
    } else if source == SearchSource::Off {
        (
            ResearchState::Off,
            "Web research is switched off in the settings (research source: off). The \
             assistant works from what you tell it and from Hotend's own library."
                .to_owned(),
        )
    } else if !supported {
        (
            ResearchState::UnsupportedProvider,
            builtin_unavailable(provider.unwrap_or(ProviderKind::Ollama)),
        )
    } else if !enabled {
        (
            ResearchState::Off,
            "Web research is off for this project: the assistant works from what you tell it \
             and from Hotend's own library."
                .to_owned(),
        )
    } else if let Some(service) = service.filter(|_| key_unreadable) {
        (
            ResearchState::NeedsKey,
            format!(
                "Hotend could not read the key store, so it does not know whether a {label} \
                 key is stored{keyless}. Until the key store can be read the assistant does \
                 not search. Fix the system keychain, or start Hotend with \
                 HOTEND_SECRETS=env and the key (if any) in {var}.",
                label = service.label(),
                keyless = if service.keyless() {
                    "; it does not use the keyless path in its place"
                } else {
                    ""
                },
                var = service.env_var()
            ),
        )
    } else if let Some(service) = service.filter(|s| !s.keyless() && !key_present) {
        (
            ResearchState::NeedsKey,
            format!(
                "{} needs an API key: store one in the settings (or, with the environment \
                 store, in {}). Until then the assistant does not search.",
                service.label(),
                service.env_var()
            ),
        )
    } else if !consent_covers(global, source) {
        (
            ResearchState::NeedsConsent,
            consent_request(source, key_present),
        )
    } else if refused_search {
        (
            ResearchState::ProviderRefused,
            "The provider refused web search: it is switched off for your account there. \
             Switch it on in the provider's console, then switch research on again here."
                .to_owned(),
        )
    } else if row.searches >= searches_cap.saturating_add(global.searches_per_turn)
        || row.result_bytes >= bytes_cap.saturating_add(bytes_cap / 2)
    {
        (
            ResearchState::Stopped,
            format!(
                "Web research has stopped for this project: it has used {} searches (allowed: \
                 {searches_cap}) and {} kB of web results (allowed: {} kB). Raise the \
                 project's limit to let the assistant search again.",
                row.searches,
                row.result_bytes / 1024,
                bytes_cap / 1024
            ),
        )
    } else if row.searches >= searches_cap || row.result_bytes >= bytes_cap {
        (
            ResearchState::BudgetUsed,
            format!(
                "This project has used its web research allowance ({} of {searches_cap} \
                 searches, {} of {} kB of web results). The assistant does not search any \
                 more and asks you instead. Raise the project's limit to let it search again.",
                row.searches,
                row.result_bytes / 1024,
                bytes_cap / 1024
            ),
        )
    } else {
        (
            ResearchState::On,
            format!(
                "Web research is on{}: the assistant may look up real products, parts and \
                 standards ({} of {searches_cap} searches used in this project).",
                match service {
                    Some(service) if !key_present => format!(
                        " through {} without a key (a trial path: at most 1,000 requests an \
                         hour, shared by everyone on this internet address)",
                        service.label()
                    ),
                    Some(service) => format!(" through {}", service.label()),
                    None => String::new(),
                },
                row.searches
            ),
        )
    };
    // A used-up project gets no provider search at all: the provider's tools would let the
    // model search again. A search service still gets its tools; the core refuses every
    // call once the project's counters reached the allowance (section 16 of the doc).
    let search =
        state == ResearchState::On || (state == ResearchState::BudgetUsed && service.is_some());
    let fetch =
        search && global.fetches_per_turn > 0 && !refused_fetch && row.fetches < fetches_cap;
    ResearchStatus {
        state,
        message,
        search,
        fetch,
        searches_used: row.searches,
        searches_cap,
        fetches_used: row.fetches,
        fetches_cap,
        searches_per_turn: global.searches_per_turn,
        fetches_per_turn: global.fetches_per_turn,
        billed_searches: row.billed_searches,
        search_fee_micro_usd: search_fee(provider, row.billed_searches),
        result_bytes: row.result_bytes,
        result_bytes_cap: bytes_cap,
        result_tokens_estimate: row.result_bytes / 4,
        acknowledged: consent_covers(global, source),
        project,
        source,
        service_host: service.map_or_else(String::new, |s| s.host().to_owned()),
        key_present,
        keyless: service.is_some_and(SearchService::keyless) && key == SearchKeyState::Absent,
    }
}

/// What a person reads before agreeing to web research through `source`: who receives the
/// queries, what it costs and how Hotend limits it. `hotend research on` prints it before
/// asking for `yes`, and the application's agreement dialog shows it word for word, so the
/// two never say different things.
pub fn consent_explanation(source: SearchSource, key_present: bool) -> String {
    let Some(service) = source.service() else {
        return "With web research on, the agent may look up real products, parts and standards \
                while it designs.\n\
                - Its search queries and the addresses of the pages it reads go to your AI \
                provider, which runs the searches. They can contain words from your messages.\n\
                - The provider bills searches to your account (Anthropic: $10 per 1,000 \
                searches), and what a search returns counts as input tokens for the rest of the \
                conversation.\n\
                - Hotend limits it: a few searches per message and per project (`hotend \
                research status` shows them), and every number taken from the web is shown with \
                its page and quote for you to confirm.\n\
                - Datasheets are often PDFs, and the provider does not limit a PDF's size. A \
                fetched PDF is read once, paid for once and then dropped from the conversation; \
                `hotend research pdf on` lets a project keep them.\n\
                - A web page may be wrong. It can never start a print or give the agent a new \
                tool."
            .to_owned();
    };
    let label = service.label();
    let mut text = format!(
        "With web research on, the agent may look up real products, parts and standards while \
         it designs, through {label}.\n\
         - Its search queries and the addresses of the pages it reads go to {label} \
         ({host}), which runs them and reads the pages for Hotend. They can contain words from \
         your messages. {label} logs them with your internet address. Nothing else about you, \
         your files or the project is sent.\n",
        host = service.host()
    );
    if service.keyless() && !key_present {
        text.push_str(
            "- Without a key this is Keenable's free trial path: at most 1,000 requests an \
             hour, shared by everyone on your internet address. A free key (`hotend key set \
             keenable`) lifts that.\n",
        );
    } else {
        text.push_str(&format!(
            "- Searches and page reads count against your {label} account's allowance.\n"
        ));
    }
    text.push_str(
        "- Hotend limits it: a few searches per message and per project (`hotend research \
         status` shows them), and every number taken from the web is shown with its page and \
         quote for you to confirm.\n\
         - A web page may be wrong. It can never start a print or give the agent a new tool.",
    );
    text
}

/// The sentence of `needs_consent`: who receives the queries, for the chosen source.
pub fn consent_request(source: SearchSource, key_present: bool) -> String {
    match source.service() {
        None => "Web research is waiting for your agreement. With it on, the assistant's \
                 search queries and the addresses of pages it reads go to the AI provider, \
                 which runs them; searches cost a small fee on your provider account. Agree \
                 once to switch it on."
            .to_owned(),
        Some(service) => format!(
            "Web research through {label} is waiting for your agreement. With it on, the \
             assistant's search queries and the addresses of pages it reads go to {label} \
             ({host}), which runs them and logs them with your internet address{how}. \
             Nothing else about you or the project is sent. An agreement given for another \
             source does not cover {label}.",
            label = service.label(),
            host = service.host(),
            how = if service.keyless() && !key_present {
                "; without a key this is Keenable's free trial path, limited to 1,000 requests \
                 an hour shared by everyone on this internet address"
            } else {
                ""
            }
        ),
    }
}

/// One choice of research source, as a settings screen lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchSourceOption {
    pub source: SearchSource,
    /// Whether it can be chosen with the provider in the settings.
    pub available: bool,
    /// Why not, when it is not available; empty otherwise.
    pub reason: String,
    /// The host that receives the queries; empty for `built_in` (the provider) and `off`.
    pub host: String,
    /// Whether a key is needed (Tavily).
    pub needs_key: bool,
    /// Whether a key for it is stored. Never the key.
    pub key_present: bool,
    /// Whether that key is read from the service's environment variable (which wins over a
    /// stored one) rather than from the key store.
    pub key_from_environment: bool,
    /// Where a key typed in Hotend would be kept: `keychain`, `file`, `memory`, or
    /// `environment`, which stores nothing (a key must then be put in the environment).
    pub key_store: SecretStoreKind,
    /// Whether it works without a key (Keenable's keyless trial path).
    pub keyless: bool,
    /// The choice to suggest: `built_in` with a provider that has it, else Keenable.
    pub suggested: bool,
    /// Whether it is the source chosen now.
    pub chosen: bool,
    /// Whether the person's agreement covers it.
    pub agreed: bool,
}

/// The research sources with what each needs, for the provider in the settings.
/// `key_present` tells for each service whether its key is stored.
pub fn source_options(
    settings: &Settings,
    key_present: impl Fn(SearchService) -> bool,
) -> Vec<SearchSourceOption> {
    let provider = settings.provider.as_ref().map(|provider| provider.kind);
    let builtin = provider.is_some_and(provider_has_builtin);
    SearchSource::ALL
        .into_iter()
        .map(|source| {
            let service = source.service();
            let present = service.is_some_and(&key_present);
            let (available, reason) = match source {
                SearchSource::BuiltIn if !builtin => (
                    false,
                    provider.map_or_else(
                        || "choose an AI provider first".to_owned(),
                        builtin_unavailable,
                    ),
                ),
                _ => (true, String::new()),
            };
            SearchSourceOption {
                source,
                available,
                reason,
                host: service.map_or_else(String::new, |s| s.host().to_owned()),
                needs_key: service.is_some_and(|s| !s.keyless()),
                key_present: present,
                key_from_environment: false,
                key_store: SecretStoreKind::Memory,
                keyless: service.is_some_and(SearchService::keyless),
                suggested: match source {
                    SearchSource::BuiltIn => builtin,
                    SearchSource::Keenable => !builtin,
                    _ => false,
                },
                chosen: settings.research.source == source,
                agreed: source != SearchSource::Off && consent_covers(&settings.research, source),
            }
        })
        .collect()
}

/// What one run gets: the egress grant (if any), the sink that reports to the store and
/// the `research` object of `agent.run`.
pub(crate) struct RunPlan {
    pub status: ResearchStatus,
    /// What the key store said about the service's key when the run was planned: the
    /// run's summary is worked out with it.
    pub key: SearchKeyState,
    pub grant: Option<ResearchGrant>,
    pub sink: Option<Arc<StoreSink>>,
    /// The run's research through a search service: the gateway's `web_search` and
    /// `web_fetch`. `None` for the provider's own search and without research.
    pub service: Option<Arc<ServiceGrant>>,
    /// The `research` parameter of `agent.run`.
    pub agent: Value,
    /// What this run shows the model and is held with, for [`remember_run`].
    pub shown: Shown,
}

/// What a run was planned with, remembered once its conversation is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Shown {
    /// The research state the notes block was made for.
    state: ResearchState,
    /// The notes the block was made from: id and state at that time.
    notes: BTreeMap<String, String>,
    /// The sites the block named as having served something that was not kept.
    blocked: Vec<String>,
    /// A digest of everything the egress writes into the web tool entries for this run;
    /// empty for a run without a grant.
    entries: String,
}

/// A digest of the grant fields that reach the tool entries of a request. The provider
/// binds earlier thinking to the tool list, so a run held with other entries must not get
/// that thinking back.
///
/// It is the digest of the entries actually written under which the stored thinking was
/// made: the egress writes a message's first turn from the grant (nothing is used yet, so
/// `max_uses` is the grant's caps), keeps those bytes on every request of that turn, and a
/// later turn of the same run (the look nudge, or the agent library's retry after an answer
/// without output) may carry less; from such a turn on the agent sends and keeps no
/// thinking (`runner.py`, `_entries_may_change`). So a run planned with the same digest
/// writes the same entries
/// as those the stored thinking was made under, and one with another digest is told
/// `"changed"` and leaves the thinking out.
fn entries_digest(grant: Option<&ResearchGrant>) -> String {
    let Some(grant) = grant else {
        return String::new();
    };
    let allowed: Vec<&String> = grant
        .allowed_domains
        .iter()
        .filter(|host| is_host_name(host))
        .collect();
    let blocked: Vec<&String> = if allowed.is_empty() {
        grant
            .blocked_domains
            .iter()
            .filter(|host| is_host_name(host))
            .collect()
    } else {
        Vec::new()
    };
    let fields = json!([
        grant.searches_per_turn,
        grant.fetches_per_turn,
        grant.max_content_tokens,
        allowed,
        blocked,
        grant.follow_links,
    ]);
    sha256_hex(fields.to_string().as_bytes())[..8].to_owned()
}

/// The searches and fetches one run with the provider's own search may make in all: the
/// per-message caps, or what is left of the project's caps when that is less; no fetch
/// when the status gives none.
pub(crate) fn run_allowance(status: &ResearchStatus) -> (u32, u32) {
    let searches = status
        .searches_per_turn
        .min(status.searches_cap.saturating_sub(status.searches_used));
    let fetches = if status.fetch {
        status
            .fetches_per_turn
            .min(status.fetches_cap.saturating_sub(status.fetches_used))
    } else {
        0
    };
    (searches, fetches)
}

/// The stored shape: the agent's own hash and, after a dot, the digest of the entries the
/// run was held with.
fn split_shape(stored: &str) -> (&str, &str) {
    stored.split_once('.').unwrap_or((stored, ""))
}

/// Decides what the next run of a project gets, for the provider's own search (the
/// default source). A search service as the source needs [`plan_with`].
#[cfg(test)]
pub(crate) fn plan(
    store: &Arc<Store>,
    settings: &Settings,
    project_id: &str,
    run_id: &str,
) -> Result<RunPlan, StoreError> {
    plan_with(store, settings, project_id, run_id, Ok(None), "")
}

/// The key store could not be asked for the search service's key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct KeyUnreadable;

/// Decides what the next run of a project gets. `search` is the client of the chosen search
/// service (with the key, when one is stored), `None` for the provider's own search and for
/// `off`, or [`KeyUnreadable`] when the key store could not be asked (the run then gets no
/// research through the service); `user_text` is the maker's message of the run (an
/// address in it may be fetched).
pub(crate) fn plan_with(
    store: &Arc<Store>,
    settings: &Settings,
    project_id: &str,
    run_id: &str,
    search: Result<Option<SearchClient>, KeyUnreadable>,
    user_text: &str,
) -> Result<RunPlan, StoreError> {
    let row = store.research_row(project_id)?;
    let (search, key) = match search {
        // A client of another service than the chosen one is not used.
        Ok(search) => {
            let search = search
                .filter(|client| Some(client.service()) == settings.research.source.service());
            let key = if search.as_ref().is_some_and(SearchClient::has_key) {
                SearchKeyState::Present
            } else {
                SearchKeyState::Absent
            };
            (search, key)
        }
        Err(KeyUnreadable) => (None, SearchKeyState::Unreadable),
    };
    let status = status_with_key_state(settings, &row, key);
    let notes = store.research_notes(project_id)?;
    let blocked = blocked_hosts(store, project_id)?;
    let block = notes_block(&notes, status.state, &blocked);
    // The block is given again only when it changed since the model last saw it.
    let fresh = notes_hash(&block) != row.notes_shown_hash;
    let global = &settings.research;
    let service_run = status.granted() && status.source.service().is_some();
    let mut service = None;
    let (grant, sink) = if service_run {
        // Through a search service: no provider tool entry; the gateway offers the two web
        // tools and the core makes the calls.
        let sink = Arc::new(StoreSink::new(Arc::clone(store), project_id, run_id));
        if let Some(client) = search {
            service = Some(Arc::new(ServiceGrant::new(
                client,
                Arc::clone(&sink),
                Arc::clone(store),
                project_id,
                ServiceGrantSpec {
                    searches_per_run: global.searches_per_turn,
                    fetches_per_run: if status.fetch {
                        global.fetches_per_turn
                    } else {
                        0
                    },
                    max_content_tokens: global.max_content_tokens,
                    max_result_bytes: global.max_result_bytes,
                    allowed_domains: status.project.allowed_domains.clone(),
                    blocked_domains: status.project.blocked_domains.clone(),
                    follow_links: status.project.follow_links,
                    user_text: user_text.to_owned(),
                    searches_cap: status.searches_cap,
                    fetches_cap: status.fetches_cap,
                    result_bytes_cap: status.result_bytes_cap,
                },
            )));
        }
        (None, service.is_some().then_some(sink))
    } else if status.granted() {
        let sink = Arc::new(StoreSink::new(Arc::clone(store), project_id, run_id));
        // What this run may use in all: the per-message cap, or what the project has left
        // when that is less. The egress holds the run to it exactly.
        let (searches, fetches) = run_allowance(&status);
        let grant = ResearchGrant {
            searches_per_turn: searches,
            fetches_per_turn: fetches,
            max_content_tokens: global.max_content_tokens,
            allowed_domains: status.project.allowed_domains.clone(),
            blocked_domains: status.project.blocked_domains.clone(),
            follow_links: status.project.follow_links,
            allow_pdf: status.project.allow_pdf,
            max_result_bytes: global.max_result_bytes,
            sink: Arc::clone(&sink) as Arc<dyn ResearchSink>,
        };
        (Some(grant), Some(sink))
    } else {
        (None, None)
    };
    let entries = entries_digest(grant.as_ref());
    // The shape the stored conversation was held with, as the agent reported it. Nothing
    // is stored for a project from before shapes were kept: the agent then assumes
    // nothing. A stored shape whose tool entries differ from this run's is stale whatever
    // the agent sees, so it gets a value that matches no shape.
    let (agent_shape, stored_entries) = split_shape(&row.shape_hash);
    let shape = if agent_shape.is_empty() {
        Value::Null
    } else if stored_entries == entries {
        json!(agent_shape)
    } else {
        json!("changed")
    };
    let searches = status.search && (!service_run || service.is_some());
    let mut agent = json!({
        "search": searches,
        "fetch": searches && status.fetch,
        "cache_ttl": global.cache_ttl,
        "notes": if fresh { block.as_str() } else { "" },
        "shape": shape,
        // What may stay in the conversation: the agent takes everything else out before
        // it sends or stores it, and the egress refuses what is left.
        "keep_pdf": status.project.allow_pdf,
        "max_result_bytes": global.max_result_bytes.saturating_sub(RESULT_WRAPPER_BYTES),
    });
    // Only a run through a search service names it: the agent then declares no provider
    // tool, and `web_search` / `web_fetch` come with the gateway's tools. The parameter of
    // a run with the provider's own search keeps its bytes.
    if let Some(service) = &service {
        agent["service"] = json!(service.service_name());
    }
    // A run without search is told why, so that the assistant's line about it is true and
    // the window offers the right thing (agreeing, the settings, or nothing for a used-up
    // allowance). A run with search keeps the parameter's bytes.
    if !searches {
        agent["state"] = json!(status.state);
        agent["source"] = json!(status.source.as_str());
    }
    Ok(RunPlan {
        key,
        grant,
        sink,
        service,
        agent,
        shown: Shown {
            state: status.state,
            notes: notes
                .iter()
                .map(|note| (note.id.clone(), note.state.clone()))
                .collect(),
            blocked,
            entries,
        },
        status,
    })
}

/// After a run whose conversation was stored: remembers which notes block the model has now
/// seen and the shape of the requests it was held with.
///
/// What the model has seen is the block of the time the run was planned, and the notes it
/// recorded itself during the run. Everything else that changed meanwhile (the allowance
/// ran out, the maker confirmed or rejected a number) is news: the next block differs from
/// the remembered one, and the next run is told once.
pub(crate) fn remember_run(
    store: &Store,
    project_id: &str,
    shown: &Shown,
    shape: Option<&str>,
) -> Result<(), StoreError> {
    let seen: Vec<ResearchNote> = store
        .research_notes(project_id)?
        .into_iter()
        .map(|mut note| {
            match shown.notes.get(&note.id) {
                Some(state) => note.state.clone_from(state),
                // Recorded in this run: the model made it, unconfirmed.
                None if matches!(note.state.as_str(), "confirmed" | "rejected") => {
                    "unconfirmed".clone_into(&mut note.state);
                }
                None => {}
            }
            note
        })
        .collect();
    let block = notes_block(&seen, shown.state, &shown.blocked);
    let shape = shape
        .filter(|shape| is_shape(shape))
        .map(|shape| format!("{shape}.{}", shown.entries));
    store.set_research_hashes(project_id, Some(&notes_hash(&block)), shape.as_deref())
}

/// A request shape as the agent reports it: a short lower-case hex string.
fn is_shape(text: &str) -> bool {
    (8..=64).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn notes_hash(block: &str) -> String {
    sha256_hex(block.as_bytes())[..16].to_owned()
}

// ---- text from the web ----------------------------------------------------------------

/// True for characters that are invisible or change how text is laid out: controls and
/// format characters (zero-width, direction overrides, tags).
pub(crate) fn is_hidden(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{00AD}' | '\u{061C}' | '\u{180E}' | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{206F}' | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}' | '\u{E0000}'..='\u{E007F}')
}

/// `text` as one line of visible characters, at most `max` characters: hidden characters
/// and line breaks become spaces, runs of spaces one space.
pub fn plain(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut count = 0;
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() || is_hidden(c) {
            space = !out.is_empty();
            continue;
        }
        if count + usize::from(space) >= max {
            out.push('…');
            break;
        }
        if std::mem::take(&mut space) {
            out.push(' ');
            count += 1;
        }
        out.push(c);
        count += 1;
    }
    out
}

/// An address from the web, cleaned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanUrl {
    /// The whole address without its fragment: what sources are stored and matched under.
    pub url: String,
    /// The address without its query string: what logs and lists show.
    pub shown: String,
    /// The host, lower-case (international names in their ASCII form).
    pub host: String,
    /// Whether the address is `https`.
    pub encrypted: bool,
}

/// Cleans an address a web result or the assistant gave. `None` when it is not a plain
/// `http` or `https` address of a named host: no login part, no IP address, nothing hidden.
pub fn clean_url(raw: &str) -> Option<CleanUrl> {
    let raw = raw.trim();
    if raw.len() > MAX_URL_BYTES || raw.chars().any(|c| is_hidden(c) || c.is_whitespace()) {
        return None;
    }
    let mut url = reqwest::Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    // `domain` is `None` for an IP address.
    let host = url.domain()?.trim_end_matches('.').to_ascii_lowercase();
    if !is_host_name(&host) {
        return None;
    }
    url.set_fragment(None);
    let mut shown = url.clone();
    shown.set_query(None);
    Some(CleanUrl {
        encrypted: url.scheme() == "https",
        url: url.to_string(),
        shown: shown.to_string(),
        host,
    })
}

/// A host as lists show it: its last three labels, so a long made-up subdomain cannot push
/// the real site out of sight.
pub fn display_host(host: &str) -> String {
    let labels: Vec<&str> = host.split('.').collect();
    let start = labels.len().saturating_sub(3);
    labels[start..].join(".")
}

/// The host of a stored address, for display.
pub fn host_of(url: &str) -> String {
    clean_url(url).map_or_else(|| "unknown site".to_owned(), |url| display_host(&url.host))
}

// ---- the sink the egress reports to -----------------------------------------------------

/// What one run found, for its summary.
#[derive(Debug, Default)]
struct RunSeen {
    /// Pages of this run as (address without query, title), in the order found.
    sources: Vec<(String, String)>,
}

/// Writes what the egress reports into the project store.
///
/// Every text is cleaned and bounded here, once, before it is stored. A store error is not
/// reported to the provider's response stream (there is nobody to tell): the counters of
/// the run still hold in the egress.
pub(crate) struct StoreSink {
    store: Arc<Store>,
    project_id: String,
    run_id: String,
    seen: Mutex<RunSeen>,
}

impl StoreSink {
    pub(crate) fn new(store: Arc<Store>, project_id: &str, run_id: &str) -> Self {
        Self {
            store,
            project_id: project_id.to_owned(),
            run_id: run_id.to_owned(),
            seen: Mutex::new(RunSeen::default()),
        }
    }

    /// The pages this run found or read: address without query, and title.
    pub(crate) fn sources(&self) -> Vec<(String, String)> {
        self.seen
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .sources
            .clone()
    }
}

impl ResearchSink for StoreSink {
    fn source(&self, kind: ResearchKind, url: &str, title: &str) {
        let Some(url) = clean_url(url) else {
            return;
        };
        let title = plain(title, 160);
        let _ = self.store.put_research_source(
            &self.project_id,
            &self.run_id,
            &url.url,
            &title,
            kind.as_str(),
        );
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        if seen.sources.len() < MAX_RUN_SOURCES
            && !seen.sources.iter().any(|(known, _)| *known == url.shown)
        {
            seen.sources.push((url.shown, title));
        }
    }

    fn evidence(&self, url: &str, text: &str, citation: bool) {
        let Some(url) = clean_url(url) else {
            return;
        };
        let _ = if citation {
            let passage = plain(text, 400);
            // A passage without text, or too short to state anything, proves nothing.
            if passage.chars().count() < MIN_CITATION_CHARS {
                return;
            }
            self.store
                .add_research_citation(&self.project_id, &url.url, &passage)
        } else {
            self.store
                .set_research_page_text(&self.project_id, &url.url, text)
        };
    }

    fn call(&self, kind: ResearchKind, text: &str, outcome: &str) {
        let text = match kind {
            ResearchKind::Search => plain(text, 200),
            // The query string of an address can carry anything; the log shows none of it.
            ResearchKind::Fetch => clean_url(text)
                .map_or_else(|| "(not a plain web address)".to_owned(), |url| url.shown),
        };
        let _ = self.store.append_research_log(
            &self.project_id,
            &self.run_id,
            kind.as_str(),
            &text,
            &plain(outcome, 40),
        );
    }

    fn used(&self, used: &ResearchUse, _block: Option<ResearchBlock>) {
        if *used == ResearchUse::default() {
            return;
        }
        let _ = self.store.add_research_use(
            &self.project_id,
            used.searches,
            used.fetches,
            used.billed_searches,
            used.result_bytes,
        );
    }

    fn refused(&self, kind: ResearchKind) {
        let _ =
            self.store
                .set_research_refused(&self.project_id, kind == ResearchKind::Fetch, true);
    }
}

// ---- notes ------------------------------------------------------------------------------

/// Why `record_source` did not store a note: a code and a message with the next step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordError {
    pub code: &'static str,
    pub message: String,
}

fn record_error<T>(code: &'static str, message: impl Into<String>) -> Result<T, RecordError> {
    Err(RecordError {
        code,
        message: message.into(),
    })
}

impl From<StoreError> for RecordError {
    fn from(error: StoreError) -> Self {
        Self {
            code: "storage_error",
            message: error.to_string(),
        }
    }
}

/// Text folded for comparing a quote with a page: lower case, typographic quotes and
/// dashes as their plain forms, hidden characters and every run of white space as one
/// space.
fn fold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() || is_hidden(c) {
            space = !out.is_empty();
            continue;
        }
        if std::mem::take(&mut space) {
            out.push(' ');
        }
        match c {
            '\u{2018}' | '\u{2019}' | '\u{2032}' => out.push('\''),
            '\u{201C}' | '\u{201D}' | '\u{2033}' => out.push('"'),
            '\u{2010}'..='\u{2015}' | '\u{2212}' => out.push('-'),
            c => out.extend(c.to_lowercase()),
        }
    }
    out
}

/// The numbers written in `text`, each in the readings it can have, with the index of the
/// character after it. A comma followed by one or two digits is a decimal comma (`80,5` is
/// 80.5, never 805); followed by three it may also group thousands (`1,234` is 1234 or
/// 1.234).
fn numbers_at(text: &str) -> Vec<(f64, usize)> {
    let mut found = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        if !chars[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let start = index;
        while index < chars.len()
            && (chars[index].is_ascii_digit()
                || (matches!(chars[index], '.' | ',')
                    && chars.get(index + 1).is_some_and(char::is_ascii_digit)))
        {
            index += 1;
        }
        let token: String = chars[start..index].iter().collect();
        // Commas group thousands only when every one of them is followed by exactly three
        // digits.
        let groups = token.split(',').skip(1).all(|group| {
            group
                .split('.')
                .next()
                .is_some_and(|digits| digits.len() == 3)
        });
        let mut readings = vec![
            token.replace(',', "."),
            token.replace('.', "").replace(',', "."),
        ];
        if groups {
            readings.push(token.replace(',', ""));
        }
        for reading in readings {
            if let Ok(value) = reading.parse::<f64>()
                && value.is_finite()
            {
                found.push((value, index));
            }
        }
    }
    found
}

/// The numbers written in `text`, in every reading [`numbers_at`] gives them.
fn numbers_in(text: &str) -> Vec<f64> {
    numbers_at(text)
        .into_iter()
        .map(|(value, _)| value)
        .collect()
}

/// A unit of length other than millimetres that stands right after a number.
fn other_length_unit(tail: &str) -> Option<&'static str> {
    let tail = tail.trim_start_matches([' ', '-']);
    if tail.starts_with('"') {
        return Some("inches");
    }
    let word: String = tail
        .chars()
        .take_while(|c| c.is_alphabetic())
        .flat_map(char::to_lowercase)
        .collect();
    match word.as_str() {
        "in" | "inch" | "inches" => Some("inches"),
        "cm" => Some("centimetres"),
        "m" => Some("metres"),
        _ => None,
    }
}

/// When every place the quote states `value` gives it in a unit of length other than
/// millimetres: that unit. `None` when the quote states it at least once without one.
fn only_in_other_unit(quote: &str, value: f64) -> Option<&'static str> {
    let folded = fold(quote);
    let chars: Vec<char> = folded.chars().collect();
    let mut other = None;
    for (found, end) in numbers_at(&folded) {
        if !same_number(found, value) {
            continue;
        }
        let tail: String = chars[end..].iter().take(12).collect();
        other = Some(other_length_unit(&tail)?);
    }
    other
}

fn same_number(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
}

/// A value as text without a trailing `.0`.
pub fn number_text(value: f64) -> String {
    let text = format!("{value:.4}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

fn evidence_words(evidence: &str) -> &'static str {
    match evidence {
        "page_text" => "quote checked against the page",
        "citation" => {
            "quote matches a passage the search cited, a search excerpt; it has not been \
             checked against the page itself"
        }
        _ => "quote not checked: Hotend does not hold the page's text",
    }
}

/// How the label of a note ends whose page is not the product maker's own (the assistant's
/// words, asked for by the tool's description).
const SECONDARY_LABEL: &str = "(secondary source)";

/// `label` ending with [`SECONDARY_LABEL`], within the length of a label.
fn secondary_label(label: &str) -> String {
    if label.ends_with(SECONDARY_LABEL) {
        return label.to_owned();
    }
    let room = 80 - SECONDARY_LABEL.len() - 1;
    let head: String = label.chars().take(room).collect();
    format!("{} {SECONDARY_LABEL}", head.trim_end())
}

/// The source a note takes when its number is recorded again as `new`, or `None` when it
/// keeps its own. Nothing is taken that would lose the product maker's page, a warning or
/// what the maker confirmed:
/// - the same page: a quote that was checked further replaces the old one, and a label that
///   called the page secondary still does. Of a confirmed number only the evidence changes,
///   and only when the quote that was confirmed itself stands on the page;
/// - another page: never for a confirmed number; never a secondary page (by its label) in
///   place of one that is not, however far its quote was checked; a page that is not
///   secondary in place of one that is, when its quote was checked as far; otherwise only
///   when its quote was checked further.
fn better_source(
    note: &ResearchNote,
    new: NewResearchNote,
    page: Option<&str>,
) -> Option<NewResearchNote> {
    let strength = |evidence: &str| match evidence {
        "page_text" => 2,
        "citation" => 1,
        _ => 0,
    };
    let confirmed = note.state == "confirmed";
    let further = strength(&new.evidence) > strength(&note.evidence);
    let was_secondary = note.label.ends_with(SECONDARY_LABEL);
    let is_secondary = new.label.ends_with(SECONDARY_LABEL);
    if note.url == new.url {
        if !further {
            return None;
        }
        if confirmed {
            let stands = new.evidence == "page_text"
                && page.is_some_and(|page| fold(page).contains(&fold(&note.quote)));
            return stands.then(|| NewResearchNote {
                label: note.label.clone(),
                quote: note.quote.clone(),
                title: note.title.clone(),
                locator: note.locator.clone(),
                ..new
            });
        }
        let label = if was_secondary {
            secondary_label(&new.label)
        } else {
            new.label.clone()
        };
        return Some(NewResearchNote { label, ..new });
    }
    let taken = match (was_secondary, is_secondary) {
        _ if confirmed => false,
        (false, true) => false,
        (true, false) => strength(&new.evidence) >= strength(&note.evidence),
        _ => further,
    };
    taken.then_some(new)
}

/// The `record_source` tool: checks a number the assistant read on the web against what
/// Hotend saw of the page, and stores it as a note. Returns the text for the model.
///
/// `args` has passed the tool's schema. The page must be one this project's research
/// found; the number must stand in the quote; and when Hotend holds the page's text, the
/// quote must stand in it.
pub(crate) fn record(store: &Store, project_id: &str, args: &Value) -> Result<String, RecordError> {
    let text = |name: &str| args.get(name).and_then(Value::as_str).unwrap_or("");
    let number = |name: &str| args.get(name).and_then(Value::as_f64);
    let key = text("key");
    let unit = text("unit");
    let Some(value) = number("value").filter(|value| value.is_finite()) else {
        return record_error("invalid_arguments", "`value` must be a finite number");
    };
    let quote = plain(text("quote"), MAX_QUOTE_CHARS);

    let Some(url) = clean_url(text("url")) else {
        return record_error(
            "unknown_source",
            "`url` is not a plain http(s) address of a named host. Give the address exactly \
             as the search result or the fetched page gave it.",
        );
    };
    let source = match store.research_source(project_id, &url.url)? {
        Some(source) => Some(source),
        // The same page under an address that differs only in its query or a final slash.
        None => store
            .research_sources(project_id)?
            .into_iter()
            .find(|source| {
                clean_url(&source.url).is_some_and(|known| {
                    known.shown.trim_end_matches('/') == url.shown.trim_end_matches('/')
                })
            })
            .map(|source| store.research_source(project_id, &source.url))
            .transpose()?
            .flatten(),
    };
    let Some(source) = source else {
        return record_error(
            "unknown_source",
            format!(
                "{} is not a page this project's web research found or read. Record only \
                 numbers from pages that web_search returned or web_fetch read, with the \
                 address as it was given; if the number is from your memory, tag the \
                 parameter `guessed` instead.",
                url.shown
            ),
        );
    };

    // The number must be in the quote: as it is, or in the unit the page uses.
    let quoted = numbers_in(&quote);
    // The number as the page states it.
    let stated = number("quoted_value").unwrap_or(value);
    match (number("quoted_value"), text("quoted_unit")) {
        (Some(quoted_value), quoted_unit) => {
            let factor = match quoted_unit {
                "mm" => 1.0,
                "cm" => 10.0,
                "m" => 1000.0,
                "in" => 25.4,
                _ => {
                    return record_error(
                        "invalid_arguments",
                        "`quoted_value` needs `quoted_unit`: mm, cm, m or in",
                    );
                }
            };
            if unit != "mm" {
                return record_error(
                    "invalid_arguments",
                    "`quoted_value` converts a length: `unit` must be mm",
                );
            }
            if !quoted.iter().any(|found| same_number(*found, quoted_value)) {
                return record_error(
                    "value_not_in_quote",
                    format!(
                        "the quote does not contain {}. Quote the sentence of the page that \
                         states the number, word for word.",
                        number_text(quoted_value)
                    ),
                );
            }
            let converted = quoted_value * factor;
            if (converted - value).abs() > (0.005 * value.abs()).max(0.01) {
                return record_error(
                    "value_not_in_quote",
                    format!(
                        "{} {quoted_unit} is {} mm, not {} mm. Record the converted value.",
                        number_text(quoted_value),
                        number_text(converted),
                        number_text(value)
                    ),
                );
            }
        }
        (None, _) => {
            if let Some(other) = Some(unit)
                .filter(|unit| *unit == "mm")
                .and_then(|_| only_in_other_unit(&quote, value))
            {
                return record_error(
                    "value_not_in_quote",
                    format!(
                        "the quote gives {} in {other}, not in mm. Pass that number as \
                         `quoted_value` with its `quoted_unit` and record the converted value.",
                        number_text(value)
                    ),
                );
            }
            if !quoted.iter().any(|found| same_number(*found, value)) {
                return record_error(
                    "value_not_in_quote",
                    format!(
                        "the quote does not contain {}. Quote the sentence of the page that \
                         states the number, word for word; if the page gives it in another \
                         unit, pass that number as `quoted_value` with its `quoted_unit`.",
                        number_text(value)
                    ),
                );
            }
        }
    }

    // The quote must be on the page, as far as Hotend can tell.
    let folded = fold(&quote);
    let evidence = if let Some(page) = &source.page_text {
        if !fold(page).contains(&folded) {
            return record_error(
                "quote_not_found",
                format!(
                    "Hotend holds the text of {} and this quote is not in it. Quote the \
                     page word for word (one sentence or table row is enough), or ask the \
                     maker for the number.",
                    url.shown
                ),
            );
        }
        "page_text"
    } else if source.citations.iter().any(|citation| {
        // The cited passage must hold the whole quote and the number itself. A quote that
        // only contains the passage proves nothing about what was added to it.
        let citation = fold(citation);
        citation.chars().count() >= MIN_CITATION_CHARS
            && citation.contains(&folded)
            && numbers_in(&citation)
                .iter()
                .any(|found| same_number(*found, stated))
    }) {
        "citation"
    } else {
        "none"
    };

    let notes = store.research_notes(project_id)?;
    // What the maker rejected does not come back: under the same key, from the same page
    // or, when Hotend cannot check the quote, from anywhere at all.
    if let Some(rejected) = notes.iter().find(|note| {
        note.state == "rejected"
            && same_number(note.value, value)
            && (note.key == key
                || note.url == source.url
                || (evidence == "none" && note.unit == unit))
    }) {
        return record_error(
            "rejected_by_maker",
            format!(
                "the maker rejected this value before (note {}). Do not use it; ask the \
                 maker for the right number.",
                rejected.id
            ),
        );
    }
    let active = |note: &&ResearchNote| matches!(note.state.as_str(), "unconfirmed" | "confirmed");
    if let Some(same) = notes
        .iter()
        .filter(active)
        .filter(|note| note.evidence != UNSOURCED)
        .find(|note| note.key == key && same_number(note.value, value) && note.unit == unit)
    {
        let again = NewResearchNote {
            key: same.key.clone(),
            label: plain(text("label"), 80),
            value: same.value,
            unit: same.unit.clone(),
            url: source.url.clone(),
            title: source.title.clone(),
            quote,
            locator: plain(text("locator"), 80),
            evidence: evidence.to_owned(),
        };
        if let Some(better) = better_source(same, again, source.page_text.as_deref()) {
            let note = store.set_research_note_source(project_id, &same.id, better)?;
            return Ok(format!(
                "{} now rests on {}: {} = {} {} ({}; {}). Use it with \
                 \"provenance\": \"web\", \"source\": \"{}\".",
                note.id,
                host_of(&note.url),
                note.key,
                number_text(note.value),
                note.unit,
                evidence_words(&note.evidence),
                note.state,
                note.id
            ));
        }
        let kept = if same.url == source.url {
            String::new()
        } else {
            format!(", and it keeps its source, {}", host_of(&same.url))
        };
        return Ok(format!(
            "already recorded as {}: {} = {} {} ({}{kept}). Use it with \
             \"provenance\": \"web\", \"source\": \"{}\".",
            same.id,
            same.key,
            number_text(same.value),
            same.unit,
            same.state,
            same.id
        ));
    }
    // A new value under a key replaces the old note; a confirmed one stays in the list.
    for old in notes.iter().filter(active).filter(|note| note.key == key) {
        store.set_research_note_state(project_id, &old.id, "superseded")?;
    }
    let unconfirmed: Vec<&ResearchNote> = notes
        .iter()
        .filter(|note| note.state == "unconfirmed" && note.key != key)
        .collect();
    let in_use = notes.iter().filter(active).filter(|n| n.key != key).count();
    if in_use >= MAX_ACTIVE_NOTES {
        match unconfirmed.first() {
            Some(oldest) => {
                store.set_research_note_state(project_id, &oldest.id, "expired")?;
            }
            None => {
                return record_error(
                    "too_many_notes",
                    format!(
                        "this project already has {MAX_ACTIVE_NOTES} confirmed web values; \
                         tell the maker, who can remove some"
                    ),
                );
            }
        }
    }
    let note = store.add_research_note(
        project_id,
        NewResearchNote {
            key: key.to_owned(),
            label: plain(text("label"), 80),
            value,
            unit: unit.to_owned(),
            url: source.url.clone(),
            title: source.title.clone(),
            quote,
            locator: plain(text("locator"), 80),
            evidence: evidence.to_owned(),
        },
    )?;
    let mut answer = format!(
        "recorded as {id}: {key} = {value} {unit} from {host} ({evidence}). In PARAMS write \
         {{\"default\": {value}, \"provenance\": \"web\", \"source\": \"{id}\"}}. Tell the \
         maker the value and the page it is from, and say so if that page is not the product \
         maker's own{excerpt}: it stays unconfirmed until the maker confirms it.",
        id = note.id,
        key = note.key,
        value = number_text(note.value),
        unit = note.unit,
        host = display_host(&url.host),
        evidence = evidence_words(evidence),
        excerpt = if evidence == "citation" {
            " or that only a search excerpt was read"
        } else {
            ""
        },
    );
    if evidence == "none" {
        answer.push_str(
            " If a fit depends on this number, read the page once with web_fetch and record \
             it again so the quote can be checked, or ask the maker to measure.",
        );
    }
    Ok(answer)
}

/// Days since 1970-01-01 as a calendar date (`2026-11-18`).
fn date_text(at_ms: i64) -> String {
    let days = at_ms.div_euclid(86_400_000);
    // Civil-from-days, after Howard Hinnant.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// The block that gives the notes back to the model on a later turn, so it does not search
/// for the same fact again. Empty when there is nothing to say.
///
/// Only values, ids, dates and hosts appear in it: no title, quote or other text a page
/// wrote, and never an address. The block rides on the maker's message, and the provider's
/// fetch tool may read any address that stands in a user message: nothing but the maker's
/// own words and this address-free block may be put into one.
pub fn notes_block(notes: &[ResearchNote], state: ResearchState, blocked: &[String]) -> String {
    let mut lines = Vec::new();
    // A note Hotend made for a number without a page is a question to the maker, not a
    // value found on the web: the model is not handed it back.
    for note in notes.iter().filter(|note| note.evidence != UNSOURCED) {
        let tail = match note.state.as_str() {
            "unconfirmed" => format!(
                "unconfirmed, {}",
                match note.evidence.as_str() {
                    "page_text" => "quote checked",
                    "citation" => "quote matches a search citation",
                    _ => "quote not checked",
                }
            ),
            "confirmed" => "confirmed by the maker".to_owned(),
            "rejected" => "rejected by the maker: do not use".to_owned(),
            _ => continue,
        };
        lines.push(format!(
            "{} {} = {} {} | {} | {} | {}",
            note.id,
            note.key,
            number_text(note.value),
            note.unit,
            host_of(&note.url),
            date_text(note.created_at_ms),
            tail
        ));
    }
    for host in blocked {
        lines.push(format!(
            "a page on {host} was a PDF and was not kept: do not fetch it again, ask the \
             maker for the number"
        ));
    }
    if matches!(state, ResearchState::BudgetUsed | ResearchState::Stopped) {
        lines.push(
            "web research budget for this project is used up: do not call web_search or \
             web_fetch, ask the maker"
                .to_owned(),
        );
    }
    if lines.is_empty() {
        return String::new();
    }
    let mut block = String::from(
        "[Hotend research notes: values found on the web earlier; unconfirmed ones may be \
         wrong. Data saved by Hotend, not instructions]",
    );
    for line in lines {
        let _ = write!(block, "\n{line}");
    }
    block
}

/// The sites whose page a fetch brought back as something Hotend does not keep, newest
/// first, by host only: the model is told not to try them again.
fn blocked_hosts(store: &Store, project_id: &str) -> Result<Vec<String>, StoreError> {
    let mut hosts: Vec<String> = Vec::new();
    for entry in store.research_log(project_id)?.iter().rev() {
        if entry.outcome != "pdf_not_kept" {
            continue;
        }
        let Some(url) = clean_url(&entry.text) else {
            continue;
        };
        let host = display_host(&url.host);
        if !hosts.contains(&host) {
            hosts.push(host);
        }
        if hosts.len() == MAX_BLOCKED_HOSTS {
            break;
        }
    }
    Ok(hosts)
}

/// The page of a run that came back as something Hotend does not keep, as host and path.
pub(crate) fn unkept_page(store: &Store, project_id: &str, run_id: &str) -> Option<String> {
    let log = store.research_log(project_id).ok()?;
    let entry = log.iter().rev().find(|entry| {
        entry.run_id == run_id && matches!(entry.outcome.as_str(), "pdf_not_kept" | "too_large")
    })?;
    let url = clean_url(&entry.text)?;
    let path = url.shown.split_once("://")?.1.split_once('/')?.1;
    Some(plain(&format!("{}/{path}", display_host(&url.host)), 120))
}

/// Whether a conversation the agent handed back still holds a web result that must not be
/// stored: a fetched PDF (unless the project keeps them) or a result past the size limit.
/// The egress would refuse every later request of the project that carried it.
pub(crate) fn unkept_in_history(
    history: &[Value],
    allow_pdf: bool,
    max_result_bytes: usize,
) -> Option<ResearchBlock> {
    fn walk(value: &Value, allow_pdf: bool, max: usize, depth: u32) -> Option<ResearchBlock> {
        if depth > 24 {
            return None;
        }
        match value {
            Value::Array(items) => items
                .iter()
                .find_map(|item| walk(item, allow_pdf, max, depth + 1)),
            Value::Object(object) => {
                let kind = object.get("type").and_then(Value::as_str);
                if !allow_pdf
                    && kind == Some("web_fetch_result")
                    && value
                        .pointer("/content/source/type")
                        .and_then(Value::as_str)
                        == Some("base64")
                {
                    return Some(ResearchBlock::Pdf);
                }
                let tool = object.get("tool_name").and_then(Value::as_str);
                if matches!(tool, Some("web_search" | "web_fetch"))
                    && object
                        .get("content")
                        .is_some_and(|content| content.to_string().len() > max)
                {
                    return Some(ResearchBlock::TooLarge);
                }
                object
                    .values()
                    .find_map(|inner| walk(inner, allow_pdf, max, depth + 1))
            }
            _ => None,
        }
    }
    history
        .iter()
        .find_map(|message| walk(message, allow_pdf, max_result_bytes, 0))
}

/// A number from the web as it stands in one version of the model: the note, where the
/// version uses it, and what to tell the person about it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebNumber {
    /// The note's id (`r3`).
    pub note: String,
    pub key: String,
    pub label: String,
    /// The value found on the web.
    pub value: f64,
    pub unit: String,
    /// The page. Show it as text; open it only when the person asks.
    pub url: String,
    /// The page's host, shortened for display.
    pub host: String,
    pub title: String,
    /// The sentence the assistant quoted for the value.
    pub quote: String,
    pub locator: String,
    /// `page_text` (the quote is on the page Hotend read), `citation` (it matches a passage
    /// the search cited) or `none` (Hotend could not check it).
    pub evidence: String,
    /// `unconfirmed` or `confirmed`.
    pub state: String,
    /// The model parameters built from this note.
    pub params: Vec<String>,
    /// The value the version was built with, when a parameter uses the note.
    pub built: Option<f64>,
    /// What to tell the person, in plain words.
    pub text: String,
}

/// The web numbers of a version: every note still in use, with the parameters that carry
/// it. Parameters tagged `web` whose note is missing, rejected or replaced are reported
/// through `problems`.
pub fn web_numbers(notes: &[ResearchNote], result: &EngineResult) -> (Vec<WebNumber>, Vec<String>) {
    let has_fit = result.questions.as_ref().is_some_and(|q| !q.is_empty())
        || result
            .checks
            .iter()
            .any(|check| check.id == "fit_pairs" && check.status != CheckStatus::Skipped);
    let mut numbers = Vec::new();
    let mut claimed = BTreeSet::new();
    for note in notes
        .iter()
        .filter(|note| matches!(note.state.as_str(), "unconfirmed" | "confirmed"))
    {
        let unsourced = note.evidence == UNSOURCED;
        // Parameters that name the note. A note without a page belongs to the parameter
        // it was made for, while that parameter still has its value.
        let tagged: Vec<&String> = result
            .param_specs
            .iter()
            .filter(|(name, spec)| {
                if unsourced {
                    **name == note.key
                        && spec.source.is_none()
                        && spec.unit == note.unit
                        && result
                            .params
                            .get(*name)
                            .is_some_and(|built| same_number(*built, note.value))
                } else {
                    spec.source.as_deref() == Some(note.id.as_str())
                }
            })
            .map(|(name, _)| name)
            .collect();
        if unsourced && tagged.is_empty() {
            // The number is gone from this version: nothing to ask.
            continue;
        }
        // Parameters built with the note's value as a guess, without saying where it is
        // from. What the maker measured, a standard size or a look is not second-guessed
        // for being equal to a number on a page.
        let untagged: Vec<&String> = result
            .param_specs
            .iter()
            .filter(|(name, spec)| {
                tagged.is_empty()
                    && spec.provenance == "guessed"
                    && spec.unit == note.unit
                    && result
                        .params
                        .get(*name)
                        .is_some_and(|built| same_number(*built, note.value))
            })
            .map(|(name, _)| name)
            .collect();
        let params: Vec<String> = tagged
            .iter()
            .chain(&untagged)
            .map(|n| (*n).clone())
            .collect();
        if !unsourced {
            claimed.extend(tagged.iter().map(|name| (*name).clone()));
        }
        let built = params
            .first()
            .and_then(|name| result.params.get(name))
            .copied();
        let host = host_of(&note.url);
        let what = if note.label.is_empty() {
            note.key.clone()
        } else {
            note.label.clone()
        };
        if unsourced {
            let provenance = result
                .param_specs
                .get(&params[0])
                .map_or("", |spec| spec.provenance.as_str());
            let mut text = format!(
                "{what} (`{}`) is {} {} and tagged `{provenance}`. It was set while the \
                 assistant was reading the web, and there is no page recorded for it.",
                params[0],
                number_text(note.value),
                note.unit
            );
            text.push_str(if note.state == "unconfirmed" {
                " If the number is yours or you have checked it against the real thing, \
                 confirm it; if not, reject it and give the assistant the right one."
            } else {
                " You confirmed it."
            });
            numbers.push(WebNumber {
                note: note.id.clone(),
                key: note.key.clone(),
                label: note.label.clone(),
                value: note.value,
                unit: note.unit.clone(),
                url: String::new(),
                host: String::new(),
                title: String::new(),
                quote: String::new(),
                locator: String::new(),
                evidence: note.evidence.clone(),
                state: note.state.clone(),
                params,
                built,
                text,
            });
            continue;
        }
        let checked = match note.evidence.as_str() {
            "page_text" => "Hotend found this sentence on the page.",
            "citation" => {
                "The sentence matches a passage the search cited, a search excerpt; it has not \
                 been checked against the page itself."
            }
            _ => {
                "Hotend could not check this sentence against the page: it is the \
                 assistant's wording."
            }
        };
        let mut text = match (built, tagged.is_empty(), params.is_empty()) {
            (_, _, true) => format!(
                "{what}: {} {} was found on the web ({host}). No parameter of this version \
                 names it, so Hotend cannot tell whether the model uses it as a fixed size \
                 in its code.",
                number_text(note.value),
                note.unit
            ),
            (Some(built), false, _) if !same_number(built, note.value) => format!(
                "{what} (`{}`) was changed from the web value {} {} ({host}) to {} {}.",
                params[0],
                number_text(note.value),
                note.unit,
                number_text(built),
                note.unit
            ),
            (_, true, _) => format!(
                "{what} (`{}`) is {} {}, the value found on the web ({host}), although the \
                 model does not say so.",
                params[0],
                number_text(note.value),
                note.unit
            ),
            _ => format!(
                "{what} (`{}`) is {} {}, taken from the web ({host}).",
                params[0],
                number_text(note.value),
                note.unit
            ),
        };
        let _ = write!(text, " The page says: \"{}\". {checked}", note.quote);
        if note.state == "unconfirmed" {
            text.push_str(
                " Nobody has confirmed it: check it against the real thing, then confirm or \
                 reject it.",
            );
            if has_fit && !params.is_empty() {
                text.push_str(" If a fit depends on it, print the fit gauge before the part.");
            }
        } else {
            text.push_str(" You confirmed it.");
        }
        numbers.push(WebNumber {
            note: note.id.clone(),
            key: note.key.clone(),
            label: note.label.clone(),
            value: note.value,
            unit: note.unit.clone(),
            url: note.url.clone(),
            host,
            title: note.title.clone(),
            quote: note.quote.clone(),
            locator: note.locator.clone(),
            evidence: note.evidence.clone(),
            state: note.state.clone(),
            params,
            built,
            text,
        });
    }
    let mut problems = Vec::new();
    for (name, spec) in &result.param_specs {
        if spec.provenance != "web" || claimed.contains(name) {
            continue;
        }
        let source = spec.source.as_deref().unwrap_or("");
        let state = notes
            .iter()
            .find(|note| note.id == source)
            .map(|n| n.state.as_str());
        problems.push(match state {
            Some("rejected") => format!(
                "Parameter `{name}` still uses a web value you rejected (note {source}). Give \
                 the assistant the right number."
            ),
            Some(state) => format!(
                "Parameter `{name}` is tagged as a web value, but its note {source} is \
                 {state}: its number has no current source."
            ),
            None => format!(
                "Parameter `{name}` is tagged as a web value, but there is no research note \
                 `{}` in this project: nobody can see where its number is from.",
                plain(source, 16)
            ),
        });
    }
    (numbers, problems)
}

/// The web numbers of a project that nobody has confirmed, as they stand in a version: what
/// the print review puts before the person. A number no parameter names is listed too: the
/// model may use it as a fixed size in its code, which Hotend cannot see.
pub fn unconfirmed_in_use(notes: &[ResearchNote], result: &EngineResult) -> Vec<WebNumber> {
    web_numbers(notes, result)
        .0
        .into_iter()
        .filter(|number| number.state == "unconfirmed")
        .collect()
}

/// What a newly built version means for the project's notes. Called by the gateway after
/// `write_model_code` made a version; `before` is the version that was current until then.
///
/// - A parameter that carried a web number and is now `measured` has the maker's own
///   value: its note is no longer in use (unless another parameter still names it).
/// - In a run that used the web, a new or changed parameter tagged `measured`, `standard`
///   or `calibrated` whose number the maker never wrote and no note holds gets a note
///   without a page, so that the person is asked where the number is from.
pub(crate) fn after_build(
    store: &Store,
    project_id: &str,
    run_id: &str,
    before: Option<&EngineResult>,
    result: &EngineResult,
) -> Result<(), StoreError> {
    let notes = store.research_notes(project_id)?;
    let active = |note: &&ResearchNote| matches!(note.state.as_str(), "unconfirmed" | "confirmed");
    if let Some(before) = before {
        for (name, spec) in &result.param_specs {
            let replaced = before
                .param_specs
                .get(name)
                .filter(|old| old.provenance == "web" && spec.provenance == "measured")
                .and_then(|old| old.source.as_deref());
            let Some(id) = replaced else {
                continue;
            };
            let still_named = result
                .param_specs
                .values()
                .any(|spec| spec.source.as_deref() == Some(id));
            if !still_named && notes.iter().filter(active).any(|note| note.id == id) {
                store.set_research_note_state(project_id, id, "superseded")?;
            }
        }
    }

    let searched = store
        .research_log(project_id)?
        .iter()
        .any(|entry| entry.run_id == run_id);
    if !searched {
        return Ok(());
    }
    // Numbers the maker wrote in a message are theirs.
    let typed: Vec<f64> = store
        .transcript(project_id)?
        .iter()
        .filter(|line| line.role == "user")
        .flat_map(|line| numbers_in(&line.text))
        .collect();
    let mut in_use = notes.iter().filter(active).count();
    let mut added = 0;
    for (name, spec) in &result.param_specs {
        if added == MAX_UNSOURCED_PER_BUILD || in_use >= MAX_ACTIVE_NOTES {
            break;
        }
        if !CLAIMED_PROVENANCES.contains(&spec.provenance.as_str())
            || !NOTE_UNITS.contains(&spec.unit.as_str())
        {
            continue;
        }
        let value = result.params.get(name).copied().unwrap_or(spec.default);
        let unchanged = before
            .and_then(|before| {
                let old = before.param_specs.get(name)?;
                let built = before.params.get(name).copied().unwrap_or(old.default);
                Some(old.provenance == spec.provenance && same_number(built, value))
            })
            .unwrap_or(false);
        if unchanged
            || typed.iter().any(|number| same_number(*number, value))
            || notes
                .iter()
                .filter(active)
                .any(|note| same_number(note.value, value) && note.unit == spec.unit)
        {
            continue;
        }
        for old in notes
            .iter()
            .filter(active)
            .filter(|note| note.evidence == UNSOURCED && note.key == *name)
        {
            store.set_research_note_state(project_id, &old.id, "superseded")?;
        }
        store.add_research_note(
            project_id,
            NewResearchNote {
                key: name.clone(),
                label: plain(&spec.label, 80),
                value,
                unit: spec.unit.clone(),
                url: String::new(),
                title: String::new(),
                quote: String::new(),
                locator: String::new(),
                evidence: UNSOURCED.to_owned(),
            },
        )?;
        added += 1;
        in_use += 1;
    }
    Ok(())
}

/// The questions web research adds to a version's list: one for every unconfirmed web
/// number and one for every parameter whose web source cannot be shown. They are made here,
/// from stored notes, not by the process that runs the model's script.
pub fn web_questions(notes: &[ResearchNote], result: &EngineResult) -> Vec<Question> {
    let (numbers, problems) = web_numbers(notes, result);
    let mut questions: Vec<Question> = numbers
        .into_iter()
        .filter(|number| number.state == "unconfirmed")
        .map(|number| Question {
            requirement: format!("web:{}", number.note),
            text: number.text.clone(),
            guessed_params: number.params.clone(),
            web: Some(number),
        })
        .collect();
    questions.extend(problems.into_iter().map(|text| Question {
        requirement: "web:source".to_owned(),
        text,
        guessed_params: Vec::new(),
        web: None,
    }));
    questions
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::paths::DataDir;
    use crate::settings::ProviderSettings;

    fn store() -> (tempfile::TempDir, Arc<Store>, String) {
        let tmp = tempfile::tempdir().unwrap();
        let data = DataDir::resolve(Some(tmp.path())).unwrap();
        let store = Arc::new(Store::open(&data.db_path()).unwrap());
        let project = store.create_project("stand").unwrap().id;
        (tmp, store, project)
    }

    fn settings(kind: ProviderKind) -> Settings {
        let mut settings = Settings {
            provider: Some(ProviderSettings {
                kind,
                model: "m".to_owned(),
            }),
            ..Settings::default()
        };
        settings.research.acknowledged_at_ms = Some(1);
        settings
    }

    const PAGE: &str = "https://maker.example/brenta-600/specs";

    /// A project whose research read one page and found another.
    fn researched() -> (tempfile::TempDir, Arc<Store>, String) {
        let (tmp, store, project) = store();
        let sink = StoreSink::new(Arc::clone(&store), &project, "run1");
        sink.source(ResearchKind::Fetch, PAGE, "Brenta 600 - technical data");
        sink.evidence(
            PAGE,
            "Brenta 600 - technical data\n\nBasket inner diameter: 58.5\u{a0}mm\nRim \
             diameter: 70 mm\nWidth 3.35 in\nLip height: 3.35 mm\nDepth: 80,5 mm\n\
             Released 2025. Thickness 0.40 inches.\n",
            false,
        );
        sink.source(
            ResearchKind::Search,
            "https://shop.example/brenta?utm=1#top",
            "Brenta \u{202e}600\n shop",
        );
        sink.evidence(
            "https://shop.example/brenta?utm=1",
            "The Brenta 600 weighs 412 g.",
            true,
        );
        (tmp, store, project)
    }

    fn args(key: &str, value: f64, unit: &str, url: &str, quote: &str) -> Value {
        json!({"key": key, "label": "Basket inner diameter", "value": value, "unit": unit,
               "url": url, "quote": quote})
    }

    #[test]
    fn the_state_follows_provider_project_consent_and_budget() {
        let (_tmp, store, project) = store();
        let row = || store.research_row(&project).unwrap();
        let state = |settings: &Settings| status(settings, &row()).state;

        let on = settings(ProviderKind::Anthropic);
        assert_eq!(state(&on), ResearchState::On);
        let status_on = status(&on, &row());
        assert!(status_on.search && status_on.fetch && status_on.granted());
        assert_eq!((status_on.searches_cap, status_on.fetches_cap), (8, 6));

        // Not chosen, not supported, switched off, not agreed to: no grant, and a sentence.
        let mut none = on.clone();
        none.provider = None;
        assert_eq!(state(&none), ResearchState::Off);
        for kind in [ProviderKind::Openai, ProviderKind::Ollama] {
            let other = status(&settings(kind), &row());
            assert_eq!(other.state, ResearchState::UnsupportedProvider);
            assert!(!other.granted() && other.message.contains("not available"));
        }
        let mut unasked = on.clone();
        unasked.research.acknowledged_at_ms = None;
        let waiting = status(&unasked, &row());
        assert_eq!(waiting.state, ResearchState::NeedsConsent);
        assert!(!waiting.granted() && waiting.message.contains("go to the AI provider"));
        let mut off_by_default = on.clone();
        off_by_default.research.default_on = false;
        assert_eq!(state(&off_by_default), ResearchState::Off);
        store
            .set_research_settings(&project, &json!({"enabled": true}))
            .unwrap();
        assert_eq!(state(&off_by_default), ResearchState::On);
        store
            .set_research_settings(&project, &json!({"enabled": false}))
            .unwrap();
        assert_eq!(state(&on), ResearchState::Off);
        store.set_research_settings(&project, &json!({})).unwrap();

        // The allowance: a used-up project gets no provider search any more (before, it
        // kept its entries and was only told not to search).
        store.add_research_use(&project, 7, 5, 7, 1000).unwrap();
        assert_eq!(state(&on), ResearchState::On);
        // What is left is less than a message's caps: the run gets what is left.
        assert_eq!(run_allowance(&status(&on, &row())), (1, 1));
        store.add_research_use(&project, 1, 1, 1, 0).unwrap();
        let used = status(&on, &row());
        assert_eq!(used.state, ResearchState::BudgetUsed);
        assert!(!used.search && !used.fetch && !used.granted());
        // A search service keeps its tools: the core refuses each call itself.
        let mut service = on.clone();
        service.research.source = SearchSource::Keenable;
        service.research.acknowledged_source = Some(SearchSource::Keenable);
        let through_service = status(&service, &row());
        assert_eq!(through_service.state, ResearchState::BudgetUsed);
        assert!(through_service.search);
        assert_eq!(used.search_fee_micro_usd, 80_000);
        store.add_research_use(&project, 3, 0, 3, 0).unwrap();
        let stopped = status(&on, &row());
        assert_eq!(stopped.state, ResearchState::Stopped);
        assert!(!stopped.granted());
        // The project's own cap lifts it again.
        store
            .set_research_settings(&project, &json!({"searches_per_project": 30}))
            .unwrap();
        assert_eq!(state(&on), ResearchState::On);
        // Result bytes count the same way.
        store
            .add_research_use(&project, 0, 0, 0, 1536 * 1024)
            .unwrap();
        assert_eq!(state(&on), ResearchState::BudgetUsed);
        store
            .add_research_use(&project, 0, 0, 0, 800 * 1024)
            .unwrap();
        assert_eq!(state(&on), ResearchState::Stopped);
    }

    #[test]
    fn the_provider_refusing_search_switches_research_off_and_fetch_alone_drops_fetch() {
        let (_tmp, store, project) = store();
        let on = settings(ProviderKind::Anthropic);
        let sink = StoreSink::new(Arc::clone(&store), &project, "run1");
        sink.refused(ResearchKind::Fetch);
        let status_now = status(&on, &store.research_row(&project).unwrap());
        assert!(status_now.search && !status_now.fetch);
        sink.refused(ResearchKind::Search);
        let status_now = status(&on, &store.research_row(&project).unwrap());
        assert_eq!(status_now.state, ResearchState::ProviderRefused);
        assert!(!status_now.granted());
    }

    #[test]
    fn a_run_plan_carries_the_caps_and_the_notes_only_when_they_changed() {
        let (_tmp, store, project) = researched();
        let on = settings(ProviderKind::Anthropic);
        let planned = plan(&store, &on, &project, "run2").unwrap();
        let grant = planned.grant.unwrap();
        assert_eq!((grant.searches_per_turn, grant.fetches_per_turn), (3, 2));
        assert_eq!(
            planned.agent,
            json!({"search": true, "fetch": true, "cache_ttl": "1h", "notes": "", "shape": null,
                   "keep_pdf": false, "max_result_bytes": 256 * 1024 - 256})
        );

        record(
            &store,
            &project,
            &args(
                "basket_diameter",
                58.5,
                "mm",
                PAGE,
                "Basket inner diameter: 58.5 mm",
            ),
        )
        .unwrap();
        let planned = plan(&store, &on, &project, "run3").unwrap();
        let block = planned.agent["notes"].as_str().unwrap().to_owned();
        assert!(
            block.contains("r1 basket_diameter = 58.5 mm | maker.example | "),
            "{block}"
        );
        assert!(block.ends_with("| unconfirmed, quote checked"), "{block}");
        // Once a stored run has shown it, it is not sent again until it changes.
        remember_run(&store, &project, &planned.shown, Some("0123456789abcdef")).unwrap();
        let planned = plan(&store, &on, &project, "run4").unwrap();
        assert_eq!(planned.agent["notes"], "");
        assert_eq!(planned.agent["shape"], "0123456789abcdef");
        // A shape that is not a hash is not stored.
        remember_run(&store, &project, &planned.shown, Some("<script>")).unwrap();
        assert_eq!(
            plan(&store, &on, &project, "run4").unwrap().agent["shape"],
            "0123456789abcdef"
        );
        store
            .set_research_note_state(&project, "r1", "rejected")
            .unwrap();
        let planned = plan(&store, &on, &project, "run5").unwrap();
        assert!(
            planned.agent["notes"]
                .as_str()
                .unwrap()
                .ends_with("rejected by the maker: do not use")
        );

        // Close to the project's caps the run gets what is left, not a whole message's caps.
        store.add_research_use(&project, 7, 5, 0, 0).unwrap();
        let planned = plan(&store, &on, &project, "run6").unwrap();
        let grant = planned.grant.unwrap();
        assert_eq!((grant.searches_per_turn, grant.fetches_per_turn), (1, 1));
        // With the allowance used up the run gets no web tools and the model is told not to
        // search (before, the run kept its entries and could search on).
        store.add_research_use(&project, 2, 0, 0, 0).unwrap();
        let planned = plan(&store, &on, &project, "run6").unwrap();
        assert_eq!(planned.status.state, ResearchState::BudgetUsed);
        assert!(planned.grant.is_none() && planned.sink.is_none());
        assert_eq!(planned.agent["search"], false);
        assert!(
            planned.agent["notes"]
                .as_str()
                .unwrap()
                .contains("budget for this project is used up")
        );
        // No grant without research; the notes are still given.
        let off = plan(&store, &settings(ProviderKind::Openai), &project, "run7").unwrap();
        assert!(off.grant.is_none() && off.sink.is_none());
        assert_eq!(off.agent["search"], false);
        assert!(
            off.agent["notes"]
                .as_str()
                .unwrap()
                .contains("r1 basket_diameter")
        );
    }

    #[test]
    fn addresses_and_texts_from_the_web_are_cleaned_before_they_are_kept() {
        let clean = clean_url("HTTPS://Maker.Example:443/a/b?x=1&k=secret#frag").unwrap();
        assert_eq!(clean.url, "https://maker.example/a/b?x=1&k=secret");
        assert_eq!(clean.shown, "https://maker.example/a/b");
        assert_eq!(clean.host, "maker.example");
        assert!(clean.encrypted);
        assert!(!clean_url("http://maker.example/").unwrap().encrypted);
        // International names are kept in their ASCII form: look-alikes show as what they are.
        assert_eq!(
            clean_url("https://аррӏе.com/").unwrap().host,
            "xn--80ak6aa92e.com"
        );
        for bad in [
            "https://user:pw@maker.example/",
            "https://maker.example@evil.example/",
            "https://10.0.0.1/",
            "https://[::1]/",
            "https://localhost/",
            "ftp://maker.example/",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "https://maker.example/\u{202e}fdp.exe",
            "https://maker.example/a b",
            "not an address",
            "",
        ] {
            assert_eq!(clean_url(bad), None, "{bad}");
        }
        assert_eq!(display_host("a.b.c.d.maker.example"), "d.maker.example");
        assert_eq!(display_host("maker.example"), "maker.example");
        assert_eq!(plain("  a\u{200b}b\n\tc \u{202e}d  ", 80), "a b c d");
        assert_eq!(plain("abcdef", 3), "abc…");

        let (_tmp, store, project) = researched();
        let sources = store.research_sources(&project).unwrap();
        assert_eq!(sources[0].url, "https://shop.example/brenta?utm=1");
        assert_eq!(sources[0].title, "Brenta 600 shop");
        assert_eq!(sources[0].citations, ["The Brenta 600 weighs 412 g."]);
        // What cannot be cleaned is not stored at all.
        let sink = StoreSink::new(Arc::clone(&store), &project, "run2");
        sink.source(ResearchKind::Search, "https://user:pw@evil.example/", "x");
        sink.source(ResearchKind::Search, "https://192.168.1.1/admin", "router");
        assert_eq!(store.research_sources(&project).unwrap().len(), 2);
        // The log shows a query as typed (one line) and an address without its query string.
        sink.call(
            ResearchKind::Search,
            "brenta\n600 \u{202e}size",
            "2 results",
        );
        sink.call(
            ResearchKind::Fetch,
            "https://maker.example/p?session=abc123",
            "ok",
        );
        sink.call(ResearchKind::Fetch, "file:///etc/passwd", "url_not_allowed");
        let log = store.research_log(&project).unwrap();
        let lines: Vec<(&str, &str)> = log
            .iter()
            .map(|e| (e.text.as_str(), e.outcome.as_str()))
            .collect();
        assert_eq!(
            lines,
            [
                ("brenta 600 size", "2 results"),
                ("https://maker.example/p", "ok"),
                ("(not a plain web address)", "url_not_allowed"),
            ]
        );
        assert!(sink.sources().is_empty());
    }

    #[test]
    fn a_number_is_recorded_only_from_a_page_that_was_seen_and_a_quote_that_holds_it() {
        let (_tmp, store, project) = researched();
        let code = |args: Value| record(&store, &project, &args).unwrap_err().code;
        let quote = "Basket inner diameter: 58.5 mm";

        // A page nobody saw, an address that is not one, a number that is not in the quote,
        // a quote that is not on the page.
        assert_eq!(
            code(args("d", 58.5, "mm", "https://evil.example/specs", quote)),
            "unknown_source"
        );
        assert_eq!(
            code(args("d", 58.5, "mm", "file:///etc/passwd", quote)),
            "unknown_source"
        );
        assert_eq!(
            code(args("d", 58.0, "mm", PAGE, quote)),
            "value_not_in_quote"
        );
        assert_eq!(
            code(args(
                "d",
                58.5,
                "mm",
                PAGE,
                "The basket is 58.5 mm wide, trust me"
            )),
            "quote_not_found"
        );
        assert!(store.research_notes(&project).unwrap().is_empty());

        // The real thing: case, line breaks and the non-breaking space do not matter.
        let text = record(
            &store,
            &project,
            &args(
                "basket_diameter",
                58.5,
                "mm",
                PAGE,
                "basket INNER diameter:  58.5 mm",
            ),
        )
        .unwrap();
        assert!(text.starts_with("recorded as r1: basket_diameter = 58.5 mm from maker.example"));
        assert!(text.contains("\"provenance\": \"web\", \"source\": \"r1\""));
        let note = &store.research_notes(&project).unwrap()[0];
        assert_eq!(
            (note.evidence.as_str(), note.state.as_str()),
            ("page_text", "unconfirmed")
        );
        assert_eq!(note.title, "Brenta 600 - technical data");
        assert_eq!(note.url, PAGE);

        // A length in another unit: the quoted number and its conversion are both checked.
        let mut inches = args("width", 85.09, "mm", PAGE, "Width 3.35 in");
        assert_eq!(code(inches.clone()), "value_not_in_quote");
        inches["quoted_value"] = json!(3.35);
        inches["quoted_unit"] = json!("in");
        record(&store, &project, &inches).unwrap();
        inches["value"] = json!(80.0);
        inches["key"] = json!("width2");
        assert_eq!(code(inches), "value_not_in_quote");

        // A search result without page text: a quote that matches a cited passage counts as
        // such; any other quote is stored as unchecked.
        let shop = "https://shop.example/brenta";
        record(
            &store,
            &project,
            &args("mass", 412.0, "g", shop, "weighs 412 g"),
        )
        .unwrap();
        record(
            &store,
            &project,
            &args("rim", 70.0, "mm", shop, "Rim 70 mm"),
        )
        .unwrap();
        let notes = store.research_notes(&project).unwrap();
        assert_eq!(notes[2].evidence, "citation");
        assert_eq!(notes[3].evidence, "none");
        assert_eq!(notes[2].url, "https://shop.example/brenta?utm=1");

        // The same value again is not a second note; a new value replaces the old one.
        let again = record(
            &store,
            &project,
            &args("mass", 412.0, "g", shop, "weighs 412 g"),
        )
        .unwrap();
        assert!(again.starts_with("already recorded as r3"), "{again}");
        record(
            &store,
            &project,
            &args("basket_diameter", 70.0, "mm", PAGE, "Rim diameter: 70 mm"),
        )
        .unwrap();
        let notes = store.research_notes(&project).unwrap();
        assert_eq!(notes[0].state, "superseded");
        assert_eq!(notes.last().unwrap().id, "r5");

        // What the maker rejected cannot come back, under the same key or from the same page.
        store
            .set_research_note_state(&project, "r5", "rejected")
            .unwrap();
        assert_eq!(
            code(args(
                "basket_diameter",
                70.0,
                "mm",
                PAGE,
                "Rim diameter: 70 mm"
            )),
            "rejected_by_maker"
        );
        assert_eq!(
            code(args("other_name", 70.0, "mm", PAGE, "Rim diameter: 70 mm")),
            "rejected_by_maker"
        );
    }

    #[test]
    fn every_unconfirmed_web_number_is_asked_about_until_the_maker_decides() {
        let (_tmp, store, project) = researched();
        for (key, value, quote) in [
            ("basket_diameter", 58.5, "Basket inner diameter: 58.5 mm"),
            ("rim_diameter", 70.0, "Rim diameter: 70 mm"),
            ("width", 3.35, "Lip height: 3.35 mm"),
        ] {
            record(&store, &project, &args(key, value, "mm", PAGE, quote)).unwrap();
        }
        record(
            &store,
            &project,
            &args(
                "mass",
                412.0,
                "g",
                "https://shop.example/brenta",
                "about 412 g",
            ),
        )
        .unwrap();
        let result: EngineResult = serde_json::from_value(json!({
            "protocol": 0, "op": "build", "status": "ok", "error": null,
            "params": {"bore": 58.5, "rim": 71.0, "wall": 3.35, "lip": 3.0, "cup": 9.0},
            "param_specs": {
                "bore": {"default": 58.5, "unit": "mm", "label": "Bore", "provenance": "web",
                         "source": "r1"},
                "rim": {"default": 70.0, "unit": "mm", "label": "Rim", "provenance": "web",
                        "source": "r2"},
                "wall": {"default": 3.35, "unit": "mm", "label": "Wall", "provenance": "guessed"},
                "lip": {"default": 3.0, "unit": "mm", "label": "Lip", "provenance": "web",
                        "source": "r9"},
                "cup": {"default": 9.0, "unit": "mm", "label": "Cup", "provenance": "guessed"}
            },
            "checks": [{"id": "fit_pairs", "status": "pass", "severity": "error",
                        "message": "", "part": null, "data": {}}]
        }))
        .unwrap();
        let notes = store.research_notes(&project).unwrap();
        let questions = web_questions(&notes, &result);
        let about: Vec<&str> = questions.iter().map(|q| q.requirement.as_str()).collect();
        assert_eq!(
            about,
            ["web:r1", "web:r2", "web:r3", "web:r4", "web:source"]
        );

        // Used as found: the value, the site, the quote, how far it was checked, the gauge.
        let bore = &questions[0];
        assert_eq!(bore.guessed_params, ["bore"]);
        assert!(bore.text.starts_with(
            "Basket inner diameter (`bore`) is 58.5 mm, taken from the web (maker.example). \
             The page says: \"Basket inner diameter: 58.5 mm\". Hotend found this sentence \
             on the page."
        ));
        assert!(
            bore.text.contains("Nobody has confirmed it"),
            "{}",
            bore.text
        );
        assert!(bore.text.ends_with("print the fit gauge before the part."));
        let number = bore.web.as_ref().unwrap();
        assert_eq!((number.built, number.value), (Some(58.5), 58.5));
        assert_eq!(number.url, PAGE);

        // Built with another value than the page gave (a slider, an override).
        assert!(
            questions[1]
                .text
                .contains("was changed from the web value 70 mm")
        );
        assert!(
            questions[1].text.contains("to 71 mm"),
            "{}",
            questions[1].text
        );
        // A parameter that carries the value without saying where it is from.
        assert_eq!(questions[2].guessed_params, ["wall"]);
        assert!(
            questions[2]
                .text
                .contains("although the model does not say so")
        );
        // A note no parameter names, with a quote Hotend could not check. Hotend cannot
        // see a fixed size in the code, and says so instead of claiming it is unused.
        assert!(
            questions[3]
                .text
                .contains("No parameter of this version names it")
        );
        assert!(questions[3].text.contains("fixed size in its code"));
        assert!(questions[3].text.contains("could not check this sentence"));
        assert!(!questions[3].text.contains("gauge"));
        // A parameter tagged `web` without a note behind it.
        assert!(questions[4].text.contains("`lip`") && questions[4].text.contains("`r9`"));

        // Confirmed and rejected notes are no longer asked about; a rejected one that a
        // parameter still uses is.
        store
            .set_research_note_state(&project, "r1", "confirmed")
            .unwrap();
        store
            .set_research_note_state(&project, "r2", "rejected")
            .unwrap();
        let notes = store.research_notes(&project).unwrap();
        let questions = web_questions(&notes, &result);
        let about: Vec<&str> = questions.iter().map(|q| q.requirement.as_str()).collect();
        assert_eq!(about, ["web:r3", "web:r4", "web:source", "web:source"]);
        assert!(
            questions[3]
                .text
                .contains("`rim` still uses a web value you rejected")
        );
        let (numbers, _) = web_numbers(&notes, &result);
        assert_eq!(numbers[0].state, "confirmed");
        assert!(numbers[0].text.ends_with("You confirmed it."));
        // The print review lists every web number nobody confirmed: the ones a parameter
        // carries and the ones the model may use as a fixed size.
        let in_use: Vec<String> = unconfirmed_in_use(&notes, &result)
            .into_iter()
            .map(|number| number.note)
            .collect();
        assert_eq!(in_use, ["r3", "r4"]);
    }

    #[test]
    fn numbers_are_read_in_the_ways_pages_write_them() {
        assert!(numbers_in("80,5 mm").contains(&80.5));
        assert!(numbers_in("1,234.5 g").contains(&1234.5));
        assert!(numbers_in("1.234,5 g").contains(&1234.5));
        assert!(numbers_in("M3x0.5, 4.0mm hole.").contains(&4.0));
        assert!(numbers_in("ø58.5mm").contains(&58.5));
        assert!(numbers_in("no digits").is_empty());
        assert_eq!(number_text(80.50), "80.5");
        assert_eq!(number_text(8.0), "8");
        assert_eq!(date_text(1_795_000_000_000), "2026-11-18");
        assert_eq!(date_text(0), "1970-01-01");
        assert_eq!(date_text(951_782_400_000), "2000-02-29");
    }

    /// Regression: a quote that merely contains a cited passage, or any quote next to an empty
    /// citation, was labelled as matching the citation.
    #[test]
    fn a_quote_counts_as_cited_only_when_the_citation_holds_it() {
        let (_tmp, store, project) = researched();
        let shop = "https://shop.example/brenta";
        let evidence_of = |key: &str| {
            let notes = store.research_notes(&project).unwrap();
            notes
                .iter()
                .find(|note| note.key == key)
                .unwrap()
                .evidence
                .clone()
        };
        // The cited passage with another number appended: the citation does not hold it.
        record(
            &store,
            &project,
            &args(
                "mass_boxed",
                500.0,
                "g",
                shop,
                "The Brenta 600 weighs 412 g. Boxed 500 g",
            ),
        )
        .unwrap();
        assert_eq!(evidence_of("mass_boxed"), "none");
        // A part of the cited passage that holds the number does count.
        record(
            &store,
            &project,
            &args("mass", 412.0, "g", shop, "Brenta 600 weighs 412 g"),
        )
        .unwrap();
        assert_eq!(evidence_of("mass"), "citation");

        // A citation without text, or too short to mean anything, is not kept and matches
        // nothing.
        let sink = StoreSink::new(Arc::clone(&store), &project, "run2");
        let other = "https://other.example/page";
        sink.source(ResearchKind::Search, other, "Other");
        sink.evidence(other, " \u{200b} \n", true);
        sink.evidence(other, "123 mm", true);
        assert!(
            store
                .research_source(&project, other)
                .unwrap()
                .unwrap()
                .citations
                .is_empty()
        );
        record(
            &store,
            &project,
            &args("made_up", 123.0, "mm", other, "Totally made up 123 mm"),
        )
        .unwrap();
        assert_eq!(evidence_of("made_up"), "none");
        // A citation stored before this rule (empty) still matches nothing.
        store.add_research_citation(&project, other, "").unwrap();
        record(
            &store,
            &project,
            &args("made_up_2", 124.0, "mm", other, "Totally made up 124 mm"),
        )
        .unwrap();
        assert_eq!(evidence_of("made_up_2"), "none");
    }

    /// Regression: a rejected value came back under a new key from a page that was only listed.
    #[test]
    fn a_rejected_value_does_not_come_back_through_an_unchecked_quote() {
        let (_tmp, store, project) = researched();
        let shop = "https://shop.example/brenta";
        record(
            &store,
            &project,
            &args("w", 805.0, "mm", shop, "Official width: 805 mm"),
        )
        .unwrap();
        store
            .set_research_note_state(&project, "r1", "rejected")
            .unwrap();
        let sink = StoreSink::new(Arc::clone(&store), &project, "run2");
        let listed = "https://listed.example/page";
        sink.source(ResearchKind::Search, listed, "Listed");
        let again = record(
            &store,
            &project,
            &args("w_again", 805.0, "mm", listed, "Width 805 mm"),
        )
        .unwrap_err();
        assert_eq!(again.code, "rejected_by_maker");
        assert!(again.message.contains("r1"), "{}", again.message);
        // Another unit is another value; a value the page's own text shows is still taken
        // (the quote is checked), and says so.
        record(
            &store,
            &project,
            &args("turns", 805.0, "count", listed, "805 turns"),
        )
        .unwrap();
        // An unchecked quote is stored, and the model is told how to get it checked.
        let text = record(
            &store,
            &project,
            &args("w_other", 99.0, "mm", listed, "Official width: 99 mm"),
        )
        .unwrap();
        assert!(text.contains("quote not checked"), "{text}");
        assert!(text.contains("web_fetch"), "{text}");
    }

    /// Regression: `80,5 mm` was accepted as 805, and inches as millimetres.
    #[test]
    fn a_number_is_read_the_way_the_quote_writes_it() {
        assert!(!numbers_in("80,5 mm").contains(&805.0));
        assert!(!numbers_in("12,55").contains(&1255.0));
        assert!(numbers_in("1,234 g").contains(&1234.0));
        assert!(numbers_in("1,234 g").contains(&1.234));
        assert!(numbers_in("12,345,678").contains(&12_345_678.0));
        assert!(!numbers_in("1.234,5 g").contains(&12345.0));

        let (_tmp, store, project) = researched();
        let error = |args: Value| record(&store, &project, &args).unwrap_err();
        assert_eq!(
            error(args("depth", 805.0, "mm", PAGE, "Depth: 80,5 mm")).code,
            "value_not_in_quote"
        );
        record(
            &store,
            &project,
            &args("depth", 80.5, "mm", PAGE, "Depth: 80,5 mm"),
        )
        .unwrap();
        // A length the quote gives in another unit needs the conversion spelled out.
        for (value, quote) in [(0.40, "Thickness 0.40 inches."), (3.35, "Width 3.35 in")] {
            let refused = error(args("t", value, "mm", PAGE, quote));
            assert_eq!(refused.code, "value_not_in_quote", "{quote}");
            assert!(
                refused.message.contains("quoted_value"),
                "{}",
                refused.message
            );
        }
        let mut inches = args("t", 10.16, "mm", PAGE, "Thickness 0.40 inches.");
        inches["quoted_value"] = json!(0.40);
        inches["quoted_unit"] = json!("in");
        record(&store, &project, &inches).unwrap();
        // The same number once in millimetres is fine.
        record(
            &store,
            &project,
            &args("lip", 3.35, "mm", PAGE, "Lip height: 3.35 mm"),
        )
        .unwrap();
    }

    /// Regression: the budget line was recorded as shown by the run that used the budget up, so
    /// the next run was never told.
    #[test]
    fn the_run_after_the_budget_was_used_up_is_told_once() {
        let (_tmp, store, project) = store();
        let on = settings(ProviderKind::Anthropic);
        // The real order: plan, the run uses the allowance, the run is remembered, plan.
        let first = plan(&store, &on, &project, "run1").unwrap();
        assert_eq!(first.agent["notes"], "");
        store.add_research_use(&project, 8, 0, 8, 0).unwrap();
        remember_run(&store, &project, &first.shown, Some("0123456789abcdef")).unwrap();
        let second = plan(&store, &on, &project, "run2").unwrap();
        assert_eq!(second.status.state, ResearchState::BudgetUsed);
        let notes = second.agent["notes"].as_str().unwrap();
        assert!(
            notes.contains("budget for this project is used up: do not call web_search"),
            "{notes}"
        );
        // Told once: the run after it gets nothing new.
        remember_run(&store, &project, &second.shown, Some("0123456789abcdef")).unwrap();
        let third = plan(&store, &on, &project, "run3").unwrap();
        assert_eq!(third.agent["notes"], "");
        // A run whose conversation was not stored has shown nothing: the block comes again.
        store
            .set_research_settings(&project, &json!({"searches_per_project": 30}))
            .unwrap();
        let lifted = plan(&store, &on, &project, "run3b").unwrap();
        assert_eq!(lifted.status.state, ResearchState::On);
        remember_run(&store, &project, &lifted.shown, None).unwrap();
        store.add_research_use(&project, 30, 0, 0, 0).unwrap();
        let fourth = plan(&store, &on, &project, "run4").unwrap();
        assert!(fourth.agent["notes"].as_str().unwrap().contains("used up"));
        let fifth = plan(&store, &on, &project, "run5").unwrap();
        assert!(fifth.agent["notes"].as_str().unwrap().contains("used up"));
        // A note the model recorded during a run is not news to it. What the maker decided
        // while the run went on is.
        let (_tmp, store, project) = researched();
        let before = plan(&store, &on, &project, "run1").unwrap();
        record(
            &store,
            &project,
            &args("rim", 70.0, "mm", PAGE, "Rim diameter: 70 mm"),
        )
        .unwrap();
        remember_run(&store, &project, &before.shown, None).unwrap();
        let after = plan(&store, &on, &project, "run2").unwrap();
        assert_eq!(after.agent["notes"], "");
        store
            .set_research_note_state(&project, "r1", "rejected")
            .unwrap();
        remember_run(&store, &project, &after.shown, None).unwrap();
        let told = plan(&store, &on, &project, "run3").unwrap();
        let block = told.agent["notes"].as_str().unwrap();
        assert!(
            block.contains("r1 rim = 70 mm | maker.example | "),
            "{block}"
        );
        assert!(
            block.ends_with("rejected by the maker: do not use"),
            "{block}"
        );
    }

    /// Regression: what the egress writes into the tool entries was not part of the stored
    /// shape, so a changed cap or domain list replayed thinking the provider then refuses.
    #[test]
    fn a_changed_tool_entry_makes_the_stored_shape_stale() {
        let (_tmp, store, project) = store();
        let on = settings(ProviderKind::Anthropic);
        // A project from before shapes were stored: the agent is told nothing.
        let first = plan(&store, &on, &project, "run1").unwrap();
        assert_eq!(first.agent["shape"], Value::Null);
        remember_run(&store, &project, &first.shown, Some("0123456789abcdef")).unwrap();
        let same = plan(&store, &on, &project, "run2").unwrap();
        assert_eq!(same.agent["shape"], "0123456789abcdef");

        // Each of these reaches a tool entry the egress writes.
        type Change = Box<dyn Fn(&mut Settings)>;
        let changes: [(&str, Value, Change); 5] = [
            (
                "domains",
                json!({"allowed_domains": ["maker.example"]}),
                Box::new(|_| {}),
            ),
            (
                "blocked",
                json!({"blocked_domains": ["spam.example"]}),
                Box::new(|_| {}),
            ),
            ("links", json!({"follow_links": true}), Box::new(|_| {})),
            (
                "caps",
                json!({}),
                Box::new(|s| s.research.searches_per_turn = 2),
            ),
            (
                "tokens",
                json!({}),
                Box::new(|s| s.research.max_content_tokens = 3000),
            ),
        ];
        for (what, project_settings, change) in changes {
            store
                .set_research_settings(&project, &project_settings)
                .unwrap();
            let mut settings = on.clone();
            change(&mut settings);
            let changed = plan(&store, &settings, &project, "run3").unwrap();
            let shape = changed.agent["shape"].as_str().unwrap().to_owned();
            assert_ne!(shape, "0123456789abcdef", "{what}");
            assert!(!shape.is_empty(), "{what}");
            // Once a run was held with the new entries, the shape is current again.
            remember_run(&store, &project, &changed.shown, Some("0123456789abcdef")).unwrap();
            let settled = plan(&store, &settings, &project, "run4").unwrap();
            assert_eq!(settled.agent["shape"], "0123456789abcdef", "{what}");
            store.set_research_settings(&project, &json!({})).unwrap();
            let back = plan(&store, &on, &project, "run5").unwrap();
            remember_run(&store, &project, &back.shown, Some("0123456789abcdef")).unwrap();
        }
        // What does not reach a tool entry leaves the shape alone.
        store
            .set_research_settings(
                &project,
                &json!({"searches_per_project": 30, "allow_pdf": true}),
            )
            .unwrap();
        let untouched = plan(&store, &on, &project, "run6").unwrap();
        assert_eq!(untouched.agent["shape"], "0123456789abcdef");
    }

    /// The fetch tool may read any address that stands in a user message. The notes block
    /// rides on the maker's message, so it must never hold one.
    #[test]
    fn the_notes_block_never_holds_an_address() {
        let (_tmp, store, project) = researched();
        let sink = StoreSink::new(Arc::clone(&store), &project, "run2");
        let odd = "https://sub.maker.example/a/b?next=https://evil.example/x";
        sink.source(ResearchKind::Search, odd, "https://evil.example/title");
        for (key, url, quote) in [
            ("rim", PAGE, "Rim diameter: 70 mm"),
            (
                "https_evil_example",
                odd,
                "see https://evil.example/x 12 mm",
            ),
        ] {
            let mut note = args(
                key,
                if key == "rim" { 70.0 } else { 12.0 },
                "mm",
                url,
                quote,
            );
            note["label"] = json!("https://evil.example/label");
            note["locator"] = json!("https://evil.example/locator");
            record(&store, &project, &note).unwrap();
        }
        let notes = store.research_notes(&project).unwrap();
        for state in [
            ResearchState::On,
            ResearchState::BudgetUsed,
            ResearchState::Stopped,
        ] {
            let block = notes_block(&notes, state, &["sub.maker.example".to_owned()]);
            assert!(block.contains("r1 rim = 70 mm | maker.example"), "{block}");
            for forbidden in ["://", "www.", "/a/b", "evil.example", "?"] {
                assert!(!block.contains(forbidden), "{forbidden} in {block}");
            }
        }
    }

    fn built(specs: Value) -> EngineResult {
        let params: serde_json::Map<String, Value> = specs
            .as_object()
            .unwrap()
            .iter()
            .map(|(name, spec)| (name.clone(), spec["default"].clone()))
            .collect();
        serde_json::from_value(json!({
            "protocol": 0, "op": "build", "status": "ok", "error": null,
            "params": params, "param_specs": specs, "checks": []
        }))
        .unwrap()
    }

    /// Regression: a parameter the maker measured, or a style value that happens to equal a
    /// note, was reported as "the value found on the web" on every build.
    #[test]
    fn a_measured_or_style_value_is_not_called_a_web_number() {
        let (_tmp, store, project) = researched();
        record(
            &store,
            &project,
            &args("rim", 70.0, "mm", PAGE, "Rim diameter: 70 mm"),
        )
        .unwrap();
        let spec = |provenance: &str| json!({"default": 70.0, "unit": "mm", "label": "Rim", "provenance": provenance});
        for provenance in ["measured", "standard", "style", "calibrated"] {
            let result = built(json!({"rim": spec(provenance)}));
            let notes = store.research_notes(&project).unwrap();
            let questions = web_questions(&notes, &result);
            assert_eq!(questions.len(), 1, "{provenance}");
            assert!(questions[0].guessed_params.is_empty(), "{provenance}");
            assert!(
                !questions[0]
                    .text
                    .contains("although the model does not say so"),
                "{provenance}: {}",
                questions[0].text
            );
        }
        // A guess that equals the note is still pointed out.
        let notes = store.research_notes(&project).unwrap();
        let guessed = web_questions(&notes, &built(json!({"rim": spec("guessed")})));
        assert_eq!(guessed[0].guessed_params, ["rim"]);

        // The maker gave the number: the parameter that carried the web value is now
        // `measured`, and the web note is no longer in use.
        let web = built(
            json!({"rim": {"default": 70.0, "unit": "mm", "label": "Rim",
            "provenance": "web", "source": "r1"}}),
        );
        let measured = built(
            json!({"rim": {"default": 70.4, "unit": "mm", "label": "Rim",
            "provenance": "measured"}}),
        );
        after_build(&store, &project, "run9", Some(&web), &measured).unwrap();
        let notes = store.research_notes(&project).unwrap();
        assert_eq!(notes[0].state, "superseded");
        assert!(web_questions(&notes, &measured).is_empty());
        // A note another parameter still names stays.
        record(
            &store,
            &project,
            &args("bore", 58.5, "mm", PAGE, "Basket inner diameter: 58.5 mm"),
        )
        .unwrap();
        let two = built(json!({
            "bore": {"default": 58.5, "unit": "mm", "label": "Bore", "provenance": "web", "source": "r2"},
            "seat": {"default": 58.5, "unit": "mm", "label": "Seat", "provenance": "web", "source": "r2"}}));
        let one = built(json!({
            "bore": {"default": 58.0, "unit": "mm", "label": "Bore", "provenance": "measured"},
            "seat": {"default": 58.5, "unit": "mm", "label": "Seat", "provenance": "web", "source": "r2"}}));
        after_build(&store, &project, "run9", Some(&two), &one).unwrap();
        assert_eq!(
            store.research_notes(&project).unwrap()[1].state,
            "unconfirmed"
        );
    }

    /// Regression: a number the model took from a page and tagged `measured` or `standard`
    /// without recording it showed nowhere as a web number.
    #[test]
    fn a_number_set_while_the_web_was_read_is_asked_about() {
        let (_tmp, store, project) = store();
        store
            .append_transcript(
                &project,
                "run1",
                "user",
                "a holder, 40 mm deep, for a 608 bearing",
            )
            .unwrap();
        let version = built(json!({
            "depth": {"default": 40.0, "unit": "mm", "label": "Depth", "provenance": "measured"},
            "bore": {"default": 22.0, "unit": "mm", "label": "Bearing bore", "provenance": "standard"},
            "wall": {"default": 2.0, "unit": "mm", "label": "Wall", "provenance": "style"},
            "gap": {"default": 0.3, "unit": "mm", "label": "Gap", "provenance": "guessed"}}));

        // A run that did not use the web changes nothing.
        after_build(&store, &project, "run1", None, &version).unwrap();
        assert!(store.research_notes(&project).unwrap().is_empty());

        // A run that searched: the number the maker did not give is asked about; the one
        // they typed is theirs.
        let sink = StoreSink::new(Arc::clone(&store), &project, "run2");
        sink.call(ResearchKind::Search, "608 bearing dimensions", "3 results");
        after_build(&store, &project, "run2", None, &version).unwrap();
        let notes = store.research_notes(&project).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(
            (
                notes[0].key.as_str(),
                notes[0].value,
                notes[0].evidence.as_str()
            ),
            ("bore", 22.0, "unsourced")
        );
        assert_eq!((notes[0].url.as_str(), notes[0].quote.as_str()), ("", ""));
        let questions = web_questions(&notes, &version);
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].requirement, "web:r1");
        assert_eq!(questions[0].guessed_params, ["bore"]);
        let text = &questions[0].text;
        assert!(
            text.starts_with("Bearing bore (`bore`) is 22 mm and tagged `standard`."),
            "{text}"
        );
        assert!(
            text.contains("while the assistant was reading the web"),
            "{text}"
        );
        assert!(text.contains("no page"), "{text}");
        assert_eq!(unconfirmed_in_use(&notes, &version).len(), 1);
        // The model is not handed it back as a value found on the web.
        assert_eq!(notes_block(&notes, ResearchState::On, &[]), "");

        // The same version again, or a later one that leaves the number alone: no second note.
        after_build(&store, &project, "run2", Some(&version), &version).unwrap();
        assert_eq!(store.research_notes(&project).unwrap().len(), 1);
        // The maker confirms it: nothing is asked any more.
        store
            .set_research_note_state(&project, "r1", "confirmed")
            .unwrap();
        let notes = store.research_notes(&project).unwrap();
        assert!(web_questions(&notes, &version).is_empty());
        // A version that no longer has the number does not ask about it either.
        store
            .set_research_note_state(&project, "r1", "unconfirmed")
            .unwrap();
        let notes = store.research_notes(&project).unwrap();
        let without = built(json!({"depth": version.param_specs["depth"]}));
        assert!(web_questions(&notes, &without).is_empty());
        // Recording the number with its page replaces the question by a real note.
        sink.source(ResearchKind::Search, PAGE, "608");
        record(
            &store,
            &project,
            &args("bore", 22.0, "mm", PAGE, "Bore 22 mm"),
        )
        .unwrap();
        let notes = store.research_notes(&project).unwrap();
        assert_eq!(notes[0].state, "superseded");
        assert_eq!(notes[1].evidence, "none");
    }

    #[test]
    fn the_oldest_unconfirmed_note_makes_room() {
        let (_tmp, store, project) = researched();
        for index in 0..MAX_ACTIVE_NOTES + 2 {
            record(
                &store,
                &project,
                &args(
                    &format!("k{index}"),
                    70.0,
                    "mm",
                    PAGE,
                    "Rim diameter: 70 mm",
                ),
            )
            .unwrap();
            if index == 0 {
                store
                    .set_research_note_state(&project, "r1", "confirmed")
                    .unwrap();
            }
        }
        let notes = store.research_notes(&project).unwrap();
        let states: Vec<&str> = notes
            .iter()
            .take(4)
            .map(|note| note.state.as_str())
            .collect();
        assert_eq!(states, ["confirmed", "expired", "expired", "unconfirmed"]);
        let block = notes_block(&notes, ResearchState::On, &[]);
        assert_eq!(block.lines().count(), 1 + MAX_ACTIVE_NOTES);
        // Nothing a page wrote is in the block: ids, keys, values, hosts, dates, states.
        assert!(!block.contains("Rim diameter") && !block.contains("technical data"));
        assert_eq!(notes_block(&[], ResearchState::On, &[]), "");
        assert!(notes_block(&[], ResearchState::BudgetUsed, &[]).contains("used up"));
    }

    /// However web research comes to be off, a run gets no grant (the egress then refuses
    /// both web tools) and is offered neither of them, and the agent is still told the
    /// lifetime of the prompt cache: caching does not depend on research. The parameter is
    /// the one in `python/tests/agent/fixtures/research/off_params.json`; the agent's tests
    /// show that with it the agent sends the request it sent before web research existed,
    /// plus the cache markers.
    #[test]
    fn with_research_off_a_run_gets_no_grant_and_still_names_the_cache_lifetime() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../python/tests/agent/fixtures/research/off_params.json");
        let off_params: Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(off_params["search"], false);
        assert_eq!(off_params["cache_ttl"], "1h");

        let on = settings(ProviderKind::Anthropic);
        type Change = Box<dyn Fn(&mut Settings, &Store, &str)>;
        let ways_off: [(&str, ResearchState, Change); 8] = [
            (
                "not agreed to",
                ResearchState::NeedsConsent,
                Box::new(|s, _, _| s.research.acknowledged_at_ms = None),
            ),
            (
                "no source chosen",
                ResearchState::Off,
                Box::new(|s, _, _| s.research.source = SearchSource::Off),
            ),
            (
                "off by default",
                ResearchState::Off,
                Box::new(|s, _, _| s.research.default_on = false),
            ),
            (
                "off for the project",
                ResearchState::Off,
                Box::new(|_, store, project| {
                    store
                        .set_research_settings(project, &json!({"enabled": false}))
                        .unwrap();
                }),
            ),
            (
                "no provider chosen",
                ResearchState::Off,
                Box::new(|s, _, _| s.provider = None),
            ),
            (
                "a provider without web research",
                ResearchState::UnsupportedProvider,
                Box::new(|s, _, _| *s = settings(ProviderKind::Openai)),
            ),
            (
                "refused by the provider",
                ResearchState::ProviderRefused,
                Box::new(|_, store, project| {
                    store.set_research_refused(project, false, true).unwrap();
                }),
            ),
            (
                "stopped at the project's limit",
                ResearchState::Stopped,
                Box::new(|_, store, project| {
                    store.add_research_use(project, 11, 0, 11, 4000).unwrap();
                }),
            ),
        ];
        for (what, state, change) in ways_off {
            let (_tmp, store, project) = store();
            let mut off = on.clone();
            change(&mut off, &store, &project);
            let planned = plan(&store, &off, &project, "run1").unwrap();
            assert_eq!(planned.status.state, state, "{what}");
            assert!(planned.grant.is_none() && planned.sink.is_none(), "{what}");
            assert!(!planned.status.granted(), "{what}");
            // What only a run without search adds: why, and the source chosen in the
            // settings, so that the assistant and the window say the true reason.
            let source = planned.status.source.as_str();
            let mut first = off_params.clone();
            first["state"] = json!(state);
            first["source"] = json!(source);
            if state == ResearchState::Stopped {
                // The one thing a project that used the web adds: its notes block, on the
                // maker's message, once. Here it holds the line on the allowance.
                let notes = planned.agent["notes"].as_str().unwrap();
                assert_eq!(notes.lines().count(), 2, "{notes}");
                assert!(notes.ends_with("ask the maker"), "{notes}");
                first["notes"] = json!(notes);
            }
            assert_eq!(planned.agent, first, "{what}");
            match what {
                "not agreed to" => assert_eq!(first["state"], "needs_consent"),
                "no source chosen" => assert_eq!((&first["state"], source), (&json!("off"), "off")),
                _ => assert_ne!(source, "off", "{what}"),
            }
            // The same for the next message, which is given the shape the run reported.
            remember_run(&store, &project, &planned.shown, Some("0123456789abcdef")).unwrap();
            let again = plan(&store, &off, &project, "run2").unwrap();
            let mut expected = off_params.clone();
            expected["shape"] = json!("0123456789abcdef");
            expected["state"] = json!(state);
            expected["source"] = json!(source);
            assert_eq!(again.agent, expected, "{what}");
        }

        // With research on the lifetime is the same one: switching research changes the
        // tools and the instructions, not whether the cache is used.
        let (_tmp, store, project) = store();
        let planned = plan(&store, &on, &project, "run1").unwrap();
        assert!(planned.grant.is_some());
        assert_eq!(planned.agent["cache_ttl"], off_params["cache_ttl"]);
    }
}
