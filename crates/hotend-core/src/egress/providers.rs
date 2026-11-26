//! The provider table: for each model provider, exactly which requests may leave the
//! machine and where the credential goes. Data, not code: a request that matches no row is
//! refused.

use serde::{Deserialize, Serialize};

/// A model provider the agent can be pointed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Anthropic,
    Openai,
    Google,
    Openrouter,
    Ollama,
}

impl ProviderKind {
    /// Every provider, in a fixed order.
    pub const ALL: [Self; 5] = [
        Self::Anthropic,
        Self::Openai,
        Self::Google,
        Self::Openrouter,
        Self::Ollama,
    ];

    /// The wire name.
    pub fn as_str(self) -> &'static str {
        self.rule().name
    }

    /// The provider's row of the table.
    pub fn rule(self) -> &'static ProviderRule {
        match self {
            Self::Anthropic => &ANTHROPIC,
            Self::Openai => &OPENAI,
            Self::Google => &GOOGLE,
            Self::Openrouter => &OPENROUTER,
            Self::Ollama => &OLLAMA,
        }
    }
}

/// Where the credential is put on an outbound request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Auth {
    /// `<header>: <key>`.
    Header(&'static str),
    /// `Authorization: Bearer <key>`.
    Bearer,
    /// No credential (a model server on this machine).
    None,
}

/// How the path of a route is matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathRule {
    /// The path must equal this string.
    Exact(&'static str),
    /// The path must be this prefix, one model name (`[A-Za-z0-9._-]+`) and one of the
    /// listed action suffixes.
    ModelAction {
        prefix: &'static str,
        actions: &'static [&'static str],
    },
}

/// One allowed request shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    pub method: &'static str,
    pub path: PathRule,
    /// Query strings allowed on this route, compared exactly. No query is always allowed.
    pub queries: &'static [&'static str],
    /// Whether the route produces model output and is therefore subject to the token
    /// ceiling.
    pub generates: bool,
}

/// Where a request names the model it wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelPlace {
    /// The body's top-level `model` string.
    Body,
    /// The model segment of the path ([`PathRule::ModelAction`]).
    Path,
}

/// What one entry of the body's `tools` array may look like. Only tools the agent executes
/// itself, through the gateway, are allowed: a tool the provider runs on its own servers
/// (web search, web fetch, code execution, remote MCP) would give the agent a network
/// path the sandbox does not have, paid for with the user's key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRule {
    /// An object whose `type` is one of these; `untyped` also allows an entry without one.
    Typed {
        types: &'static [&'static str],
        untyped: bool,
    },
    /// An object with exactly these keys and no others.
    Keys(&'static [&'static str]),
}

/// The two provider-side web tools the egress can write into a request, and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResearchKind {
    /// The provider searches the web.
    Search,
    /// The provider reads one page.
    Fetch,
}

impl ResearchKind {
    /// The wire name of the kind (`search` or `fetch`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Fetch => "fetch",
        }
    }
}

/// One provider-side web tool: which marker entries of the agent stand for it and which
/// entry the egress writes in their place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerToolRule {
    pub kind: ResearchKind,
    /// The tool's fixed name in the request and in the response blocks.
    pub name: &'static str,
    /// Marker types the agent may declare. They are never sent.
    pub accepted_types: &'static [&'static str],
    /// The type the egress writes: the basic version, which has no code-execution path.
    pub written_type: &'static str,
    /// Whether `allowed_callers: ["direct"]` is written. Clear it for a kind only if the
    /// provider ever rejects the field on `written_type`; never raise the version instead.
    pub write_allowed_callers: bool,
}

/// The web research a provider offers: search and fetch run on the provider's side, only for
/// a run that holds a grant ([`super::ResearchGrant`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResearchRule {
    pub tools: &'static [ServerToolRule],
    /// What the provider charges per search, in millionths of a US dollar.
    pub search_price_micro_usd: u32,
}

impl ResearchRule {
    /// The rule of the tool an entry of `tools` is a marker for, if it is one.
    pub fn marker(&self, tool: &serde_json::Value) -> Option<&'static ServerToolRule> {
        let kind = tool.get("type")?.as_str()?;
        let name = tool.get("name")?.as_str()?;
        self.tools
            .iter()
            .find(|rule| rule.name == name && rule.accepted_types.contains(&kind))
    }
}

/// What the body of a request must look like beyond being a JSON object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyRule {
    pub model: ModelPlace,
    pub tools: ToolRule,
    /// Top-level fields that are refused: provider-side tools and anything that swaps in
    /// another model or a stored request.
    pub denied_fields: &'static [&'static str],
    /// When set, the only top-level fields a request may hold: the ones the agent library
    /// sends. Everything else is refused by name, so an option the provider adds later
    /// (a dearer mode, another model to fall back to, a stored conversation) is closed
    /// until this table names it.
    pub allowed_fields: Option<&'static [&'static str]>,
    /// Fields (as paths from the root) that multiply the output; allowed only as 1.
    pub single_output_fields: &'static [&'static [&'static str]],
    /// Objects (as paths from the root; the empty path is the body itself) whose keys must
    /// not contain an underscore. For an API that also accepts a snake_case spelling of
    /// every field, so that a limit cannot be stated twice with different values.
    pub camel_case_only: &'static [&'static [&'static str]],
    /// `source.type` values refused in any content block of `messages`: a block that names
    /// a URL or a stored file makes the provider load it, which is a network path.
    pub denied_block_sources: &'static [&'static str],
    /// Content block types refused anywhere in `messages`: calls and results of tools the
    /// provider runs for itself, other than the two web tools of `research`. A call block
    /// of the provider's own tools (`server_tool_use`) is refused too unless it names one
    /// of those two.
    pub denied_block_types: &'static [&'static str],
    /// The provider's web search and web fetch, when the egress can grant them.
    pub research: Option<&'static ResearchRule>,
}

/// Everything the egress module knows about one provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderRule {
    pub name: &'static str,
    /// Scheme and host (and port) every request goes to.
    pub origin: &'static str,
    /// The base URL to configure in the provider's SDK inside the agent, so that the paths
    /// the SDK builds match `routes`.
    pub sdk_base_url: &'static str,
    pub routes: &'static [Route],
    pub auth: Auth,
    /// Environment variables the opt-in environment secret store reads, in order.
    pub env_vars: &'static [&'static str],
    /// Request headers passed through in addition to the global allowlist.
    pub pass_headers: &'static [&'static str],
    /// Headers of `pass_headers` whose value is a comma-separated list of feature names.
    /// Only names that start with one of the listed prefixes are passed on.
    pub feature_headers: &'static [(&'static str, &'static [&'static str])],
    pub body: BodyRule,
    /// Body fields that cap the output length, as paths from the root of the JSON body.
    pub token_fields: &'static [&'static [&'static str]],
    /// Whether a generating request must carry one of `token_fields`.
    pub require_token_limit: bool,
}

const fn post(path: &'static str) -> Route {
    Route {
        method: "POST",
        path: PathRule::Exact(path),
        queries: &[],
        generates: true,
    }
}

/// The chat-completions and responses APIs: function tools only, one answer per request.
/// `web_search_options` turns on the provider's search; `prompt` names a stored prompt,
/// which can bring its own model and tools.
const OPENAI_STYLE_BODY: BodyRule = BodyRule {
    model: ModelPlace::Body,
    tools: ToolRule::Typed {
        types: &["function"],
        untyped: false,
    },
    denied_fields: &["web_search_options", "prompt"],
    allowed_fields: None,
    single_output_fields: &[&["n"]],
    camel_case_only: &[],
    denied_block_sources: &[],
    denied_block_types: &[],
    research: None,
};

/// The basic versions only: the later ones let model-written code run over the results on
/// the provider's side unless `allowed_callers` says otherwise.
static ANTHROPIC_RESEARCH: ResearchRule = ResearchRule {
    tools: &[
        ServerToolRule {
            kind: ResearchKind::Search,
            name: "web_search",
            accepted_types: &[
                "web_search_20250305",
                "web_search_20260209",
                "web_search_20260318",
            ],
            written_type: "web_search_20250305",
            write_allowed_callers: true,
        },
        ServerToolRule {
            kind: ResearchKind::Fetch,
            name: "web_fetch",
            accepted_types: &[
                "web_fetch_20250910",
                "web_fetch_20260209",
                "web_fetch_20260309",
                "web_fetch_20260318",
            ],
            written_type: "web_fetch_20250910",
            write_allowed_callers: true,
        },
    ],
    search_price_micro_usd: 10_000,
};

static ANTHROPIC: ProviderRule = ProviderRule {
    name: "anthropic",
    origin: "https://api.anthropic.com",
    sdk_base_url: "https://api.anthropic.com",
    routes: &[
        Route {
            method: "POST",
            path: PathRule::Exact("/v1/messages"),
            queries: &["beta=true"],
            generates: true,
        },
        Route {
            method: "POST",
            path: PathRule::Exact("/v1/messages/count_tokens"),
            queries: &["beta=true"],
            generates: false,
        },
    ],
    auth: Auth::Header("x-api-key"),
    env_vars: &["ANTHROPIC_API_KEY"],
    pass_headers: &["anthropic-version", "anthropic-beta"],
    // Beta features that change how the model answers. None of them runs a tool on the
    // provider's side or reaches a network (those are also refused in the body).
    feature_headers: &[(
        "anthropic-beta",
        &[
            "interleaved-thinking-",
            "fine-grained-tool-streaming-",
            "token-efficient-tools-",
            "prompt-caching-",
            "extended-cache-ttl-",
            "context-management-",
            "compact-",
            // What the API does with a replayed thinking block whose conversation prefix
            // changed: drop it instead of refusing the request. The agent's model library
            // sends it with `thinking.block_binding`, which the API refuses without it.
            "thinking-binding-controls-",
        ],
    )],
    body: BodyRule {
        model: ModelPlace::Body,
        // A plain tool has no type or the type `custom`; every server tool has a dated
        // type such as `web_search_20250305`.
        tools: ToolRule::Typed {
            types: &["custom"],
            untyped: true,
        },
        denied_fields: &["mcp_servers", "container"],
        // What the agent library sends, and nothing else. Not in the list, among others:
        // `speed` (a dearer mode), `fallbacks` (another model), `thread` (a stored
        // conversation), `service_tier`, `inference_geo`, `context_management`, `betas`.
        allowed_fields: Some(&[
            "model",
            "max_tokens",
            "messages",
            "system",
            "tools",
            "tool_choice",
            "stream",
            "thinking",
            // Its `effort` is never the agent's: the egress removes it from every request
            // and writes the person's level itself where it applies (`effort.rs`).
            "output_config",
            "metadata",
            "cache_control",
            "stop_sequences",
        ]),
        single_output_fields: &[],
        camel_case_only: &[],
        denied_block_sources: &["url", "file"],
        denied_block_types: &[
            "container_upload",
            "mcp_tool_use",
            "mcp_tool_result",
            "code_execution_tool_result",
            "bash_code_execution_tool_result",
            "text_editor_code_execution_tool_result",
        ],
        research: Some(&ANTHROPIC_RESEARCH),
    },
    token_fields: &[&["max_tokens"]],
    require_token_limit: true,
};

static OPENAI: ProviderRule = ProviderRule {
    name: "openai",
    origin: "https://api.openai.com",
    sdk_base_url: "https://api.openai.com/v1",
    routes: &[post("/v1/chat/completions"), post("/v1/responses")],
    auth: Auth::Bearer,
    env_vars: &["OPENAI_API_KEY"],
    pass_headers: &[],
    feature_headers: &[],
    body: OPENAI_STYLE_BODY,
    token_fields: &[
        &["max_output_tokens"],
        &["max_completion_tokens"],
        &["max_tokens"],
    ],
    require_token_limit: true,
};

static GOOGLE: ProviderRule = ProviderRule {
    name: "google",
    origin: "https://generativelanguage.googleapis.com",
    sdk_base_url: "https://generativelanguage.googleapis.com",
    routes: &[Route {
        method: "POST",
        path: PathRule::ModelAction {
            prefix: "/v1beta/models/",
            actions: &[":generateContent", ":streamGenerateContent"],
        },
        queries: &["alt=sse"],
        generates: true,
    }],
    auth: Auth::Header("x-goog-api-key"),
    env_vars: &["GOOGLE_API_KEY", "GEMINI_API_KEY"],
    pass_headers: &[],
    feature_headers: &[],
    body: BodyRule {
        model: ModelPlace::Path,
        // `googleSearch`, `urlContext`, `codeExecution` and the like are other keys.
        tools: ToolRule::Keys(&["functionDeclarations"]),
        // Cached content is a stored request; it can carry tools of its own.
        denied_fields: &["cachedContent"],
        allowed_fields: None,
        single_output_fields: &[&["generationConfig", "candidateCount"]],
        camel_case_only: &[&[], &["generationConfig"]],
        denied_block_sources: &[],
        denied_block_types: &[],
        research: None,
    },
    token_fields: &[&["generationConfig", "maxOutputTokens"]],
    require_token_limit: true,
};

static OPENROUTER: ProviderRule = ProviderRule {
    name: "openrouter",
    origin: "https://openrouter.ai",
    sdk_base_url: "https://openrouter.ai/api/v1",
    routes: &[post("/api/v1/chat/completions")],
    auth: Auth::Bearer,
    env_vars: &["OPENROUTER_API_KEY"],
    pass_headers: &[],
    feature_headers: &[],
    body: BodyRule {
        // `models` and `route` name fallback models; `plugins` adds web search.
        denied_fields: &["web_search_options", "plugins", "models", "route"],
        ..OPENAI_STYLE_BODY
    },
    token_fields: &[&["max_completion_tokens"], &["max_tokens"]],
    require_token_limit: true,
};

static OLLAMA: ProviderRule = ProviderRule {
    name: "ollama",
    origin: "http://127.0.0.1:11434",
    sdk_base_url: "http://127.0.0.1:11434/v1",
    routes: &[post("/v1/chat/completions"), post("/api/chat")],
    auth: Auth::None,
    env_vars: &[],
    pass_headers: &[],
    feature_headers: &[],
    body: OPENAI_STYLE_BODY,
    token_fields: &[
        &["max_completion_tokens"],
        &["max_tokens"],
        &["options", "num_predict"],
    ],
    // A local model costs nothing per token; the request and byte caps still apply.
    require_token_limit: false,
};

/// Request headers the agent may set for any provider. Everything else is dropped.
pub const GLOBAL_REQUEST_HEADERS: [&str; 2] = ["accept", "content-type"];

/// Response headers passed back to the agent. Everything else is dropped.
pub const RESPONSE_HEADERS: [&str; 5] = [
    "content-type",
    "retry-after",
    "request-id",
    "x-request-id",
    "x-should-retry",
];

impl ProviderRule {
    /// The route a request matches, if any.
    pub fn route(&self, method: &str, path: &str, query: Option<&str>) -> Option<&'static Route> {
        self.routes.iter().find(|route| {
            route.method == method
                && route.path.matches(path)
                && query.is_none_or(|q| route.queries.contains(&q))
        })
    }
}

impl PathRule {
    fn matches(&self, path: &str) -> bool {
        match self {
            Self::Exact(expected) => path == *expected,
            Self::ModelAction { .. } => self.model_in(path).is_some(),
        }
    }

    /// The model a path names, for a rule that has the model in the path.
    pub fn model_in<'p>(&self, path: &'p str) -> Option<&'p str> {
        let Self::ModelAction { prefix, actions } = self else {
            return None;
        };
        let rest = path.strip_prefix(prefix)?;
        actions
            .iter()
            .filter_map(|action| rest.strip_suffix(action))
            .find(|model| {
                !model.is_empty()
                    && model.len() <= 128
                    && !model.starts_with('.')
                    && model
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn wire_names_match_the_protocol() {
        let names: Vec<&str> = ProviderKind::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(
            names,
            ["anthropic", "openai", "google", "openrouter", "ollama"]
        );
        for kind in ProviderKind::ALL {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
        }
    }

    #[test]
    fn every_remote_provider_uses_https_and_a_credential() {
        for kind in ProviderKind::ALL {
            let rule = kind.rule();
            assert!(rule.sdk_base_url.starts_with(rule.origin));
            if kind == ProviderKind::Ollama {
                assert!(rule.origin.starts_with("http://127.0.0.1:"));
                assert_eq!(rule.auth, Auth::None);
            } else {
                assert!(rule.origin.starts_with("https://"));
                assert_ne!(rule.auth, Auth::None);
                assert!(rule.require_token_limit && !rule.env_vars.is_empty());
            }
        }
    }

    #[test]
    fn only_listed_methods_paths_and_queries_match() {
        let anthropic = ProviderKind::Anthropic.rule();
        assert!(anthropic.route("POST", "/v1/messages", None).is_some());
        assert!(
            anthropic
                .route("POST", "/v1/messages", Some("beta=true"))
                .is_some()
        );
        for (method, path, query) in [
            ("GET", "/v1/messages", None),
            ("DELETE", "/v1/messages", None),
            ("POST", "/v1/messages/", None),
            ("POST", "/v1/messages/../files", None),
            ("POST", "/v1/files", None),
            ("POST", "/v1/messages/batches", None),
            ("POST", "/v1/messages", Some("beta=true&x=1")),
            ("POST", "/v1/organizations/api_keys", None),
            ("POST", "v1/messages", None),
        ] {
            assert!(
                anthropic.route(method, path, query).is_none(),
                "{method} {path} {query:?}"
            );
        }
        assert!(
            !anthropic
                .route("POST", "/v1/messages/count_tokens", None)
                .unwrap()
                .generates
        );

        let google = ProviderKind::Google.rule();
        let ok = "/v1beta/models/gemini-2.5-pro:streamGenerateContent";
        assert!(google.route("POST", ok, Some("alt=sse")).is_some());
        for path in [
            "/v1beta/models/:generateContent",
            "/v1beta/models/a/b:generateContent",
            "/v1beta/models/..:generateContent",
            "/v1beta/models/x:deleteEverything",
            "/v1beta/models/x%2f..:generateContent",
            "/v1beta/tunedModels/x:generateContent",
        ] {
            assert!(google.route("POST", path, None).is_none(), "{path}");
        }
        assert!(google.route("POST", ok, Some("key=abc")).is_none());
    }

    /// The versions the egress writes have no code-execution path. Should a later version
    /// ever be written, it must be pinned to direct calls.
    #[test]
    fn only_basic_web_tool_versions_are_written_and_only_one_provider_has_them() {
        const BASIC: [&str; 2] = ["web_search_20250305", "web_fetch_20250910"];
        for kind in ProviderKind::ALL {
            let research = kind.rule().body.research;
            assert_eq!(research.is_some(), kind == ProviderKind::Anthropic);
            for tool in research.map(|r| r.tools).unwrap_or_default() {
                assert!(
                    BASIC.contains(&tool.written_type) || tool.write_allowed_callers,
                    "{} must be pinned to direct calls",
                    tool.written_type
                );
                assert!(BASIC.contains(&tool.written_type), "{}", tool.written_type);
                assert!(tool.accepted_types.contains(&tool.written_type));
                assert!(tool.accepted_types.iter().all(|t| t.starts_with(tool.name)));
            }
        }
        let rule = ProviderKind::Anthropic.rule().body.research.unwrap();
        let marker = |kind: &str, name: &str| {
            rule.marker(&serde_json::json!({"type": kind, "name": name}))
                .map(|tool| tool.kind)
        };
        assert_eq!(
            marker("web_search_20260209", "web_search"),
            Some(ResearchKind::Search)
        );
        assert_eq!(
            marker("web_fetch_20260318", "web_fetch"),
            Some(ResearchKind::Fetch)
        );
        assert_eq!(marker("web_search_20260209", "web_fetch"), None);
        assert_eq!(marker("code_execution_20250825", "code_execution"), None);
        assert_eq!(marker("web_search_20991231", "web_search"), None);
    }
}
