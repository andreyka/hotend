//! The tool gateway: the only place an agent's tool call turns into an action.
//!
//! For every call the gateway, in this order:
//! 1. counts it against the run's total, whatever becomes of it;
//! 2. looks the tool up in the registry ([`tools::tool_specs`]); there is no other tool,
//!    and none that starts a print, runs a command or reads a file. A run with web research
//!    has one more, [`tools::research_tool_spec`], which stores a checked note and nothing
//!    else; for every other run it does not exist;
//! 3. caps the size of the arguments and validates them against the tool's JSON Schema;
//! 4. applies the tool's own cap per run;
//! 5. checks state preconditions (the project exists, there is a current version);
//! 6. executes the tool through [`crate::modeling::Modeler`] or the injected
//!    [`SliceBackend`];
//! 7. writes one audit entry with the SHA-256 of the canonical arguments and the outcome,
//!    whether the call was executed, refused, cancelled with its run or abandoned.
//!
//! The look review's views ([`Gateway::look_views`]) take another path: after a build whose
//! status is `ok` the agent process may ask once for Hotend's standard views of the new
//! version. That call is not the model's: it counts against no cap, can never be a refusal
//! and is audited with `auto: true`.
//!
//! A run may be refused only so often ([`GatewayPolicy::max_refusals_per_run`]). Beyond
//! that the session is exhausted: the agent supervisor ends the run, and until it has,
//! further refusals are only counted and reported in one summary entry
//! ([`Gateway::close_session`]), so a hostile agent cannot grow the audit log without
//! bound.
//!
//! The result is a [`ToolOutcome`]: compact text for the model, images as separate content
//! items. A failed build is a normal (`ok: true`) result that says what to fix; `ok: false`
//! means the call itself was refused or could not run.

pub mod render;
pub mod slice;
pub mod tools;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::watch;

pub use slice::{
    CatalogueEntry, FilamentUse, HOTEND_MADE_PREVIEWS, PlacedPiece, PlateRenders,
    ProjectExportSummary, ProjectKeys, SliceBackend, SliceBed, SliceError, SliceRequest,
    SliceSummary, SliceWarning, SlicerCatalogue, SlicerInfo, SlicerInstallation,
};
pub use tools::{CHECK_IDS, RECORD_SOURCE, ToolSpec, WEB_FETCH, WEB_SEARCH};

use crate::search::run::ServiceGrant;

use crate::engine::Section;
use crate::import;
use crate::jobs::JobScope;
use crate::modeling::{BuildReport, Modeler, ModelingError, Preparation};
use crate::settings::PreviewsMode;
use crate::store::{NewAudit, PendingAudit, PendingPrintJob, SliceRecord, VersionStatus};
use crate::util::{canonical_json, json_hash, now_ms, sha256_hex, truncate};

/// Largest sliced file the gateway will hash and record.
pub(crate) const MAX_SLICE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Longest search text the engine's `docs` op accepts (`docs.query`), in characters.
pub const MAX_DOCS_QUERY_CHARS: usize = 400;

/// The first `max` characters of `text`.
fn truncate_chars(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((end, _)) => &text[..end],
        None => text,
    }
}

/// Limits that apply to every tool.
#[derive(Debug, Clone)]
pub struct GatewayPolicy {
    /// Largest canonical JSON of one call's arguments.
    pub max_args_bytes: usize,
    /// Tool calls per agent run, over all tools, refused ones included.
    pub max_calls_per_run: u32,
    /// Refused calls (tool calls the gateway turned away and model requests the egress
    /// turned away) a run may make before it is ended.
    pub max_refusals_per_run: u32,
}

impl Default for GatewayPolicy {
    fn default() -> Self {
        Self {
            max_args_bytes: 256 * 1024,
            max_calls_per_run: 200,
            max_refusals_per_run: 20,
        }
    }
}

/// The look review's views, shown after every build whose status is `ok`.
pub const LOOK_VIEWS: [&str; 3] = ["iso", "front", "top"];
/// Their size in pixels. Three images of 512 x 512 are about 1 050 input tokens (a provider
/// counts about width x height / 750 per image), against about 2 360 at 768 x 768.
pub const LOOK_VIEW_PX: u32 = 512;
/// The size of the views the model asks for itself.
pub const VIEW_PX: u32 = 768;

/// The codes of calls the gateway itself turned away before anything ran.
const REFUSAL_CODES: [&str; 4] = [
    "unknown_tool",
    "arguments_too_large",
    "invalid_arguments",
    "rate_limited",
];

/// One item of a tool result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ContentItem {
    /// Text for the model.
    Text { text: String },
    /// An image, already inlined: the agent process never reads files.
    Image {
        /// Where the image belongs in the version folder (informational).
        path: String,
        media_type: String,
        data_b64: String,
    },
}

/// Why a call was refused or could not run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolFailure {
    /// Stable code: `unknown_tool`, `invalid_arguments`, `arguments_too_large`,
    /// `rate_limited`, `no_current_version`, `not_found`, `not_configured`, `no_slice`,
    /// `timeout`, `crashed`, `sandbox_unavailable`, ... (job and storage codes).
    pub code: String,
    /// What happened and what to do next, for the model.
    pub message: String,
}

/// The answer to a `tool.call`, in the wire shape of PROTOCOL.md section 5.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutcome {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content: Vec<ContentItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ToolFailure>,
}

impl ToolOutcome {
    fn text(text: String) -> Self {
        Self {
            ok: true,
            content: vec![ContentItem::Text { text }],
            error: None,
        }
    }

    fn failure(code: &str, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            content: Vec::new(),
            error: Some(ToolFailure {
                code: code.to_owned(),
                message: truncate(&message.into(), 2000),
            }),
        }
    }

    /// All text items joined; handy for tests and the CLI.
    pub fn text_content(&self) -> String {
        self.content
            .iter()
            .filter_map(|item| match item {
                ContentItem::Text { text } => Some(text.as_str()),
                ContentItem::Image { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl From<ModelingError> for ToolOutcome {
    fn from(error: ModelingError) -> Self {
        Self::failure(error.code(), error.to_string())
    }
}

#[derive(Debug, Default)]
struct Counts {
    /// Calls per tool that passed the checks.
    per_tool: HashMap<&'static str, u32>,
    /// Every call received.
    total: u32,
    /// Refused calls and refused model requests.
    refused: u32,
    /// Refusals past the limit, by code: counted, not audited one by one.
    unrecorded: BTreeMap<String, u32>,
    /// The last build made a version whose status is `ok` and its views were not shown yet.
    look_pending: bool,
}

/// The calls made by one agent run on one project: its rate counters, its refusal count
/// and the switch that stops its jobs when the run ends.
#[derive(Debug)]
pub struct RunSession {
    run_id: String,
    project_id: String,
    counts: Mutex<Counts>,
    stop: watch::Sender<bool>,
    exhausted: watch::Sender<bool>,
    /// The run has web research: it is offered `record_source`.
    research: bool,
    /// The run's web research through a search service: it is offered `web_search` (and
    /// `web_fetch` when the grant has page reads), and the core makes the calls.
    web: Option<Arc<ServiceGrant>>,
    /// The lines about web numbers last given to the model in this run. They are given
    /// again only when they changed.
    web_given: Mutex<String>,
}

impl RunSession {
    /// A session for one run. Create one per `agent.run`.
    pub fn new(run_id: impl Into<String>, project_id: impl Into<String>) -> Self {
        Self {
            run_id: run_id.into(),
            project_id: project_id.into(),
            counts: Mutex::new(Counts::default()),
            stop: watch::channel(false).0,
            exhausted: watch::channel(false).0,
            research: false,
            web: None,
            web_given: Mutex::new(String::new()),
        }
    }

    /// Says whether the run has web research. Only then the gateway knows the tool
    /// `record_source`.
    pub fn with_research(mut self, research: bool) -> Self {
        self.research = research;
        self
    }

    /// Gives the run web research through a search service: the gateway then knows
    /// `web_search` and `web_fetch`, and `record_source`.
    pub(crate) fn with_web(mut self, web: Option<Arc<ServiceGrant>>) -> Self {
        if web.is_some() {
            self.research = true;
        }
        self.web = web;
        self
    }

    /// Whether the run has web research.
    pub fn has_research(&self) -> bool {
        self.research
    }

    /// The run this session belongs to.
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// The project the run works on.
    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    /// Ends the session: engine jobs of calls still in flight are stopped and those calls
    /// return `cancelled`. Call it when the run is over.
    pub fn stop(&self) {
        self.stop.send_replace(true);
    }

    /// Resolves when [`Self::stop`] has been called.
    pub async fn stopped(&self) {
        let mut stop = self.stop.subscribe();
        let _ = stop.wait_for(|stop| *stop).await;
    }

    /// Resolves when the run has used up its refusals and must be ended.
    pub async fn exhausted(&self) {
        let mut exhausted = self.exhausted.subscribe();
        let _ = exhausted.wait_for(|exhausted| *exhausted).await;
    }

    /// Whether the run has used up its refusals.
    pub fn is_exhausted(&self) -> bool {
        *self.exhausted.borrow()
    }

    fn counts(&self) -> std::sync::MutexGuard<'_, Counts> {
        self.counts.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Counts one refusal. Returns whether it should still get its own audit entry.
    fn note_refusal(&self, code: &str, limit: u32) -> bool {
        let mut counts = self.counts();
        counts.refused = counts.refused.saturating_add(1);
        if counts.refused >= limit {
            self.exhausted.send_replace(true);
        }
        if counts.refused <= limit {
            return true;
        }
        let key = truncate(code, 64);
        *counts.unrecorded.entry(key).or_insert(0) += 1;
        false
    }

    fn scope(&self) -> JobScope {
        JobScope {
            project_id: Some(self.project_id.clone()),
            run_id: Some(self.run_id.clone()),
            cancel: Some(self.stop.subscribe()),
        }
    }
}

/// Whether a build earns the look review: it made a version whose status is `ok`.
fn looks_for(report: &BuildReport) -> bool {
    report.version.is_some() && report.result.status.as_str() == "ok"
}

/// The result of a views call: one line naming them, then the pictures.
fn views_outcome(
    current: Option<u32>,
    rendered: &[crate::modeling::RenderedView],
    section: Option<Section>,
) -> ToolOutcome {
    let names: Vec<&str> = rendered.iter().map(|v| v.name.as_str()).collect();
    let mut content = vec![ContentItem::Text {
        text: format!(
            "views of version {}: {}{}",
            current.unwrap_or(0),
            names.join(", "),
            section
                .map(|s| format!(" (section {:?} at {} mm)", s.plane, s.offset))
                .unwrap_or_default()
        ),
    }];
    content.extend(rendered.iter().map(|view| ContentItem::Image {
        path: format!("views/{}.png", view.name),
        media_type: "image/png".to_owned(),
        data_b64: BASE64.encode(&view.png),
    }));
    ToolOutcome {
        ok: true,
        content,
        error: None,
    }
}

struct Tool {
    spec: ToolSpec,
    validator: jsonschema::Validator,
}

/// The tool gateway. Share it behind an `Arc`.
pub struct Gateway {
    modeler: Arc<Modeler>,
    tools: Vec<Tool>,
    policy: GatewayPolicy,
    slicer: RwLock<Option<Arc<dyn SliceBackend>>>,
}

impl std::fmt::Debug for Gateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gateway")
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl Gateway {
    /// Builds the gateway and compiles every tool schema.
    pub fn new(modeler: Arc<Modeler>, policy: GatewayPolicy) -> Result<Self, String> {
        let tools = tools::tool_specs()
            .into_iter()
            .chain([tools::research_tool_spec()])
            .chain(tools::web_tool_specs())
            .map(|spec| match jsonschema::validator_for(&spec.input_schema) {
                Ok(validator) => Ok(Tool { spec, validator }),
                Err(e) => Err(format!("the schema of tool {} is invalid: {e}", spec.name)),
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            modeler,
            tools,
            policy,
            slicer: RwLock::new(None),
        })
    }

    /// The registry: the tools of every run.
    pub fn tool_specs(&self) -> Vec<&ToolSpec> {
        self.tool_specs_for(false)
    }

    /// The `tools` list of `agent.run`: the registry, and `record_source` for a run with
    /// web research.
    pub fn tool_specs_for(&self, research: bool) -> Vec<&ToolSpec> {
        self.offered(research, false, false)
    }

    /// The `tools` list of `agent.run` for a session: the registry, `record_source` for a
    /// run with web research, and `web_search` / `web_fetch` for one whose research goes
    /// through a search service.
    pub fn tool_specs_for_session(&self, session: &RunSession) -> Vec<&ToolSpec> {
        let web = session.web.as_ref();
        self.offered(
            session.research,
            web.is_some(),
            web.is_some_and(|web| web.offers_fetch()),
        )
    }

    fn offered(&self, research: bool, search: bool, fetch: bool) -> Vec<&ToolSpec> {
        self.tools
            .iter()
            .filter(|tool| match tool.spec.name {
                RECORD_SOURCE => research,
                WEB_SEARCH => search,
                WEB_FETCH => fetch,
                _ => true,
            })
            .map(|tool| &tool.spec)
            .collect()
    }

    /// Installs the slicer that `slice` and `propose_print` use.
    pub fn set_slice_backend(&self, backend: Arc<dyn SliceBackend>) {
        *self.slicer.write().unwrap_or_else(|p| p.into_inner()) = Some(backend);
    }

    fn slice_backend(&self) -> Result<Arc<dyn SliceBackend>, SliceError> {
        self.slicer
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .ok_or(SliceError::NotConfigured)
    }

    /// The limits this gateway applies.
    pub fn policy(&self) -> &GatewayPolicy {
        &self.policy
    }

    /// Counts a refusal that happened outside the gateway (a model request the egress
    /// turned away) against the run's refusal limit.
    pub fn note_refusal(&self, session: &RunSession, code: &str) {
        session.note_refusal(code, self.policy.max_refusals_per_run);
    }

    /// Writes the summary entry for the refusals of a session that were not audited one by
    /// one. Call it once, when the run is over.
    pub fn close_session(&self, session: &RunSession) {
        let unrecorded = std::mem::take(&mut session.counts().unrecorded);
        if unrecorded.is_empty() {
            return;
        }
        let count: u32 = unrecorded.values().sum();
        let _ = self.modeler.store().audit(NewAudit {
            project_id: Some(session.project_id.clone()),
            run_id: Some(session.run_id.clone()),
            kind: "tool",
            name: "(refused calls)".to_owned(),
            args_sha256: None,
            outcome: "refused".to_owned(),
            detail: json!({"count": count, "by_code": unrecorded,
                "note": "refusals past the run's limit, counted instead of listed"}),
        });
    }

    /// The look review's views: Hotend's standard views ([`LOOK_VIEWS`], [`LOOK_VIEW_PX`])
    /// of the current version, once after each build whose status is `ok`. Asked for by the
    /// agent process, not the model, so it counts against no cap and is never a refusal;
    /// any other time it answers `not_offered`. Audited as `render_views` with `auto: true`.
    pub async fn look_views(&self, session: &RunSession) -> ToolOutcome {
        let mut audit = PendingAudit::new(
            self.modeler.store(),
            NewAudit {
                project_id: Some(session.project_id.clone()),
                run_id: Some(session.run_id.clone()),
                kind: "tool",
                name: "render_views".to_owned(),
                args_sha256: None,
                outcome: String::new(),
                detail: json!({"auto": true}),
            },
        );
        let pending = std::mem::take(&mut session.counts().look_pending);
        let outcome = if !pending {
            ToolOutcome::failure(
                "not_offered",
                "Hotend's own views come only once after a build whose status is ok",
            )
        } else {
            match self.look_views_now(session).await {
                Ok(outcome) => outcome,
                Err(error) => error.into(),
            }
        };
        audit.finish(outcome.error.as_ref().map_or("ok", |e| e.code.as_str()));
        outcome
    }

    async fn look_views_now(&self, session: &RunSession) -> Result<ToolOutcome, ModelingError> {
        let project_id = session.project_id.as_str();
        let current = self.modeler.store().project(project_id)?.current_version;
        if current.is_none() {
            return Err(ModelingError::NoCurrentVersion);
        }
        let views = LOOK_VIEWS.iter().map(|v| (*v).to_owned()).collect();
        let rendered = self
            .modeler
            .render_views(
                &session.scope(),
                project_id,
                views,
                None,
                [LOOK_VIEW_PX, LOOK_VIEW_PX],
            )
            .await?;
        Ok(views_outcome(current, &rendered, None))
    }

    /// Handles one tool call of a run. Never panics and never returns an error: every
    /// refusal is a [`ToolOutcome`] with `ok: false` and is audited like a success. If the
    /// returned future is dropped before it finishes, the entry says `abandoned`.
    pub async fn call(&self, session: &RunSession, name: &str, args: Value) -> ToolOutcome {
        let canonical = canonical_json(&args);
        let args_hash = sha256_hex(canonical.as_bytes());
        let mut audit = PendingAudit::new(
            self.modeler.store(),
            NewAudit {
                project_id: Some(session.project_id.clone()),
                run_id: Some(session.run_id.clone()),
                kind: "tool",
                name: truncate(name, 64),
                args_sha256: Some(args_hash),
                outcome: String::new(),
                detail: json!({"args_bytes": canonical.len()}),
            },
        );
        let outcome = self
            .checked_call(session, name, args, canonical.len(), audit.detail())
            .await;
        let code = outcome.error.as_ref().map(|e| e.code.as_str());
        audit.finish(code.unwrap_or("ok"));
        if let Some(code) = code.filter(|code| REFUSAL_CODES.contains(code))
            && !session.note_refusal(code, self.policy.max_refusals_per_run)
        {
            // Counted in the session's summary entry instead.
            audit.suppress();
        }
        outcome
    }

    async fn checked_call(
        &self,
        session: &RunSession,
        name: &str,
        args: Value,
        args_bytes: usize,
        detail: &mut Value,
    ) -> ToolOutcome {
        let rate_limited = |tool_cap: Option<u32>| {
            ToolOutcome::failure(
                "rate_limited",
                format!(
                    "this run has used up its calls{} ({} calls in total, refused ones \
                     included); finish with what you have and report to the user",
                    tool_cap
                        .map(|cap| format!(" of `{}` ({cap} of this tool)", truncate(name, 64)))
                        .unwrap_or_default(),
                    self.policy.max_calls_per_run
                ),
            )
        };
        {
            // Every call counts, before anything else is looked at.
            let mut counts = session.counts();
            counts.total = counts.total.saturating_add(1);
            if counts.total > self.policy.max_calls_per_run {
                return rate_limited(None);
            }
        }
        let offered = self.tool_specs_for_session(session);
        let tool = self
            .tools
            .iter()
            .find(|tool| tool.spec.name == name && offered.iter().any(|spec| spec.name == name));
        let Some(tool) = tool else {
            let known: Vec<&str> = offered.iter().map(|spec| spec.name).collect();
            return ToolOutcome::failure(
                "unknown_tool",
                format!(
                    "there is no tool `{}`; the tools are: {}",
                    truncate(name, 64),
                    known.join(", ")
                ),
            );
        };
        if args_bytes > self.policy.max_args_bytes {
            return ToolOutcome::failure(
                "arguments_too_large",
                format!(
                    "the arguments are {args_bytes} bytes; the limit is {}",
                    self.policy.max_args_bytes
                ),
            );
        }
        let problems: Vec<String> = tool
            .validator
            .iter_errors(&args)
            .take(3)
            .map(|error| {
                let path = error.instance_path().to_string();
                let place = if path.is_empty() {
                    "arguments".to_owned()
                } else {
                    path
                };
                format!("{place}: {}", truncate(&error.to_string(), 200))
            })
            .collect();
        if !problems.is_empty() {
            return ToolOutcome::failure("invalid_arguments", problems.join("; "));
        }
        let any_of = tool.spec.requires_any_of;
        if !any_of.is_empty() && !any_of.iter().any(|key| args.get(*key).is_some()) {
            return ToolOutcome::failure(
                "invalid_arguments",
                format!("arguments: give at least one of {}", any_of.join(", ")),
            );
        }
        {
            let mut counts = session.counts();
            let used = counts.per_tool.entry(tool.spec.name).or_insert(0);
            if *used >= tool.spec.max_calls_per_run {
                return rate_limited(Some(tool.spec.max_calls_per_run));
            }
            *used += 1;
        }
        let project = match self.modeler.store().project(&session.project_id) {
            Ok(project) => project,
            Err(error) => return ModelingError::from(error).into(),
        };
        if tool.spec.needs_current_version && project.current_version.is_none() {
            return ModelingError::NoCurrentVersion.into();
        }
        match self
            .execute(
                session,
                tool.spec.name,
                args,
                project.current_version,
                detail,
            )
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => error.into(),
        }
    }

    async fn execute(
        &self,
        session: &RunSession,
        name: &str,
        args: Value,
        current: Option<u32>,
        detail: &mut Value,
    ) -> Result<ToolOutcome, ModelingError> {
        let scope = session.scope();
        let project_id = session.project_id.as_str();
        let text = |key: &str| {
            args.get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        match name {
            "write_model_code" => {
                // The version that is replaced: what its parameters were says which numbers
                // are new.
                let before =
                    current.and_then(|number| self.modeler.version_detail(project_id, number).ok());
                let report = self
                    .modeler
                    .build_version(
                        &scope,
                        project_id,
                        &text("code"),
                        Default::default(),
                        "write_model_code",
                        &text("note"),
                    )
                    .await?;
                detail["status"] = json!(report.result.status.as_str());
                detail["version"] = json!(report.version.as_ref().map(|v| v.number));
                session.counts().look_pending = looks_for(&report);
                let mut text = render::build_report(&report, current);
                if report.version.is_some() {
                    // A store error here must not fail the build that succeeded.
                    let _ = crate::research::after_build(
                        self.modeler.store(),
                        project_id,
                        &session.run_id,
                        before.as_ref().map(|detail| &detail.result),
                        &report.result,
                    );
                    text.push_str(&self.web_lines(session, &report));
                }
                Ok(ToolOutcome::text(text))
            }
            "set_parameter" => {
                let value = args
                    .get("value")
                    .and_then(Value::as_f64)
                    .unwrap_or(f64::NAN);
                let report = self
                    .modeler
                    .set_parameter(&scope, project_id, &text("name"), value)
                    .await?;
                detail["status"] = json!(report.result.status.as_str());
                detail["version"] = json!(report.version.as_ref().map(|v| v.number));
                session.counts().look_pending = looks_for(&report);
                // Trying a parameter changes nothing about where a number is from: the web
                // numbers are not repeated here.
                Ok(ToolOutcome::text(render::build_report(&report, current)))
            }
            "run_checks" => {
                let only = args.get("checks").and_then(Value::as_array).map(|ids| {
                    ids.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                });
                let result = self
                    .modeler
                    .run_checks(&scope, project_id, only, false)
                    .await?;
                detail["status"] = json!(result.status.as_str());
                Ok(ToolOutcome::text(render::checks_report(
                    &result,
                    current.unwrap_or(0),
                )))
            }
            "render_views" => {
                let views: Vec<String> = args
                    .get("views")
                    .and_then(Value::as_array)
                    .map(|v| {
                        v.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                let section: Option<Section> = match args.get("section") {
                    Some(value) => Some(
                        serde_json::from_value(value.clone())
                            .map_err(|e| ModelingError::Invalid(format!("section: {e}")))?,
                    ),
                    None => None,
                };
                let rendered = self
                    .modeler
                    .render_views(&scope, project_id, views, section, [VIEW_PX, VIEW_PX])
                    .await?;
                Ok(views_outcome(current, &rendered, section))
            }
            "measure" => {
                let queries = args
                    .get("queries")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let results = self.modeler.measure(&scope, project_id, queries).await?;
                Ok(ToolOutcome::text(render::measure_report(&results)))
            }
            "read_docs" => {
                // The schema allows a topic and a query that together are longer than
                // the engine's limit for one search text; the search needs no more.
                let query = [text("topic"), text("query")].join(" ");
                let query = truncate_chars(query.trim(), MAX_DOCS_QUERY_CHARS);
                let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(5) as u32;
                let (versions, docs) = self.modeler.docs(&scope, query.trim(), limit).await?;
                Ok(ToolOutcome::text(render::docs_report(&versions, &docs)))
            }
            "list_cards" => {
                // The cards are whatever the installed hotend_fit package defines, read
                // through the engine's docs index so the list always matches the runtime.
                let (versions, docs) = self
                    .modeler
                    .docs(
                        &scope,
                        "hotend_fit cards snap_lid peg_hole screw_boss gauge port",
                        20,
                    )
                    .await?;
                if versions.get("hotend_fit").is_none_or(Option::is_none) {
                    return Ok(ToolOutcome::failure(
                        "not_installed",
                        "the feature-card library (hotend_fit) is not installed in this engine \
                         runtime, so no cards are available; model the fit features directly",
                    ));
                }
                let cards: Vec<_> = docs
                    .into_iter()
                    .filter(|doc| doc.module.starts_with("hotend_fit.cards"))
                    .collect();
                Ok(ToolOutcome::text(render::docs_report(&versions, &cards)))
            }
            tools::LIST_COMPONENTS => {
                // The catalogue is the docstrings the installed hotend_fit writes from its
                // own component data (an introduction, then one entry per component), read
                // through the engine's docs index like the cards.
                let (versions, docs) = self
                    .modeler
                    .docs(&scope, render::COMPONENTS_QUERY, 20)
                    .await?;
                if versions.get("hotend_fit").is_none_or(Option::is_none) {
                    return Ok(ToolOutcome::failure(
                        "not_installed",
                        "the feature-card library (hotend_fit) is not installed in this engine \
                         runtime, so no components are available",
                    ));
                }
                Ok(ToolOutcome::text(render::components_report(&docs)))
            }
            "slice" => {
                let plate = args.get("plate").and_then(Value::as_u64).map(|p| p as u32);
                match self
                    .slice_current(&scope, project_id, &text("profile"), plate)
                    .await
                {
                    Ok(outcome) => Ok(ToolOutcome::text(render::slice_report(&outcome))),
                    Err(error) => Ok(error.into()),
                }
            }
            "propose_print" => match self.propose_print(project_id, &text("note")) {
                Ok(job) => {
                    detail["pending_print_job"] = json!(job.id);
                    Ok(ToolOutcome::text(render::proposal_report(&job)))
                }
                Err(error) => Ok(error.into()),
            },
            tools::SWEEP_PARAMETERS => {
                let number = current.unwrap_or(0);
                let version = self.modeler.version_detail(project_id, number)?;
                let settings = match sweep_settings(&args, &version.result.param_specs) {
                    Ok(settings) => settings,
                    Err(message) => return Ok(ToolOutcome::failure("invalid_sweep", message)),
                };
                detail["settings"] = json!(settings.len());
                let result = self.modeler.sweep(&scope, project_id, settings).await?;
                detail["status"] = json!(result.status.as_str());
                Ok(ToolOutcome::text(render::sweep_report(
                    &result,
                    number,
                    &version.result.params,
                )))
            }
            tools::LIST_PRINT_PROFILES => {
                let settings = self.modeler.settings().map_err(ModelingError::from)?;
                let catalogue = self
                    .slice_backend()
                    .ok()
                    .and_then(|backend| backend.catalogue(&settings.printer_id));
                Ok(ToolOutcome::text(render::catalogue_report(
                    &settings,
                    catalogue.as_ref(),
                )))
            }
            tools::PRINT_PLAN => {
                let plate = args.get("plate").and_then(Value::as_u64).map(|p| p as u32);
                let explain = args.get("explain").and_then(Value::as_bool) == Some(true);
                // Read-only: computed from the stored layout, never stored and never laid
                // out again, so a pending print job's plan cannot change under it.
                let Some(stands) = self.modeler.plan_as_it_stands(project_id, plate)? else {
                    return Ok(ToolOutcome::failure(
                        "not_prepared",
                        "the current version is not laid out on plates for the printer and \
                         material in the settings, and print_plan changes nothing; `slice` lays \
                         it out, then print_plan shows the plan",
                    ));
                };
                let view = stands.plate_plan.view(&stands.preparation);
                let mut report = render::plan_report(&view, explain);
                if stands.lays_out_again {
                    report.push_str(
                        "\nNote: this plan's brim needs more room between the pieces than the \
                         layout keeps, so the next `slice` lays the version out again and the \
                         plan may change.",
                    );
                }
                Ok(ToolOutcome::text(report))
            }
            tools::SNAPSHOT_MODEL => {
                // Writes the name and nothing else: no version, no export. Like the person's
                // `snapshot_create`, it keeps the exact file of the version's print (if it
                // was printed) with the snapshot, where nothing else writes.
                let number = current.unwrap_or(0);
                let store = self.modeler.store();
                let name = text("name");
                if let Ok(taken) = store.snapshot(project_id, &name) {
                    return Ok(ToolOutcome::failure(
                        "name_taken",
                        format!(
                            "this project already has a snapshot named \"{}\" (version {}); \
                             choose another name, or ask the maker",
                            taken.name, taken.version
                        ),
                    ));
                }
                match store.create_snapshot(project_id, &name, number, &text("note")) {
                    Ok(snapshot) => {
                        detail["version"] = json!(number);
                        let kept = {
                            let (data, store, kept) = (
                                self.modeler.data().clone(),
                                Arc::clone(store),
                                snapshot.clone(),
                            );
                            tokio::task::spawn_blocking(move || {
                                crate::snapshot::keep_printed(&data, &store, &kept)
                            })
                            .await
                            .unwrap_or_else(|_| json!("not found"))
                        };
                        store.audit_snapshot(
                            &snapshot,
                            "create",
                            Some(&session.run_id),
                            "ok",
                            json!({"by": "agent", "printed_file": kept}),
                        );
                        Ok(ToolOutcome::text(render::snapshot_report(&snapshot)))
                    }
                    Err(crate::store::StoreError::Invalid(message)) => {
                        Ok(ToolOutcome::failure("invalid_name", message))
                    }
                    Err(error) => Err(error.into()),
                }
            }
            WEB_SEARCH | WEB_FETCH => {
                let Some(web) = session.web.as_ref() else {
                    return Ok(ToolOutcome::failure(
                        "unknown_tool",
                        "this run has no web research through a search service",
                    ));
                };
                let outcome = if name == WEB_SEARCH {
                    web.search(&text("query")).await
                } else {
                    web.fetch(&text("url")).await
                };
                detail["service"] = json!(web.service_name());
                Ok(match outcome {
                    // Text from the web: data for the model, framed as such by the grant.
                    Ok(text) => ToolOutcome::text(text),
                    Err(failure) => ToolOutcome::failure(failure.code, failure.message),
                })
            }
            RECORD_SOURCE => {
                Ok(
                    match crate::research::record(self.modeler.store(), project_id, &args) {
                        Ok(text) => ToolOutcome::text(text),
                        // The number was not stored; the message says what to do instead.
                        Err(error) => ToolOutcome::failure(error.code, error.message),
                    },
                )
            }
            _ => Ok(ToolOutcome::failure(
                "unknown_tool",
                "this tool has no executor",
            )),
        }
    }

    /// What the project's web numbers mean for the version that was just built, for the
    /// model: a few short lines, and only when they differ from what this run was given
    /// last. They are made here from the stored notes; the process that ran the model's
    /// script has no say in them.
    fn web_lines(&self, session: &RunSession, report: &BuildReport) -> String {
        let Ok(notes) = self.modeler.store().research_notes(&session.project_id) else {
            return String::new();
        };
        let (numbers, problems) = crate::research::web_numbers(&notes, &report.result);
        let lines = render::web_numbers(&numbers, &problems);
        let mut given = session.web_given.lock().unwrap_or_else(|p| p.into_inner());
        if *given == lines {
            return String::new();
        }
        given.clone_from(&lines);
        lines
    }

    /// The injected slicer backend, if there is one.
    pub fn slice_backend_handle(&self) -> Option<Arc<dyn SliceBackend>> {
        self.slice_backend().ok()
    }

    /// Slices one plate of the current version with the injected backend and records the
    /// result. The version is laid out on plates first (print preparation) unless it already
    /// is for the printer and material in the settings; the slicer reads the plate's 3MF.
    /// The sliced file is hashed here; a backend that reports a different hash or a file
    /// outside its output folder is refused.
    ///
    /// The plate's print plan is computed (or its stored record checked) first. A plan that
    /// is not blocked gives the slicer Hotend's print settings and its hash goes into the
    /// slice's folder name and record. A blocked plan is refused with its code and sentence,
    /// except by the stand-in slicer, which slices with its own profiles and whose file can
    /// never become a print.
    pub async fn slice_current(
        &self,
        scope: &JobScope,
        project_id: &str,
        profile: &str,
        plate: Option<u32>,
    ) -> Result<SliceOutcome, SliceFailure> {
        let mut known = SliceAuditFacts::default();
        let result = self
            .slice_plate(scope, project_id, profile, plate, false, &mut known)
            .await;
        self.audit_slice(project_id, plate.unwrap_or(1), &known, &result);
        result
    }

    /// Writes the one `slice` audit entry of a slice: identifiers, hashes and the slicer,
    /// never a path, a file's contents or the slicer's words.
    fn audit_slice(
        &self,
        project_id: &str,
        plate: u32,
        known: &SliceAuditFacts,
        result: &Result<SliceOutcome, SliceFailure>,
    ) {
        let mut detail = json!({
            "project_id": project_id,
            "version": known.version,
            "plate": plate,
            "slice_id": known.slice_id,
            "profile_hash": known.slice_id,
            "plan_sha256": known.plan_sha256,
            "slicer": null,
            "slicer_version": null,
            "previews_added_by_hotend": null,
            "sha256": null,
        });
        let (outcome, args_sha256) = match result {
            Ok(done) => {
                detail["version"] = json!(done.record.version);
                detail["slice_id"] = json!(done.record.profile_hash);
                detail["profile_hash"] = json!(done.record.profile_hash);
                detail["slicer"] = json!(truncate(&done.summary.slicer.name, 64));
                detail["slicer_version"] = json!(truncate(&done.summary.slicer.version, 64));
                detail["previews_added_by_hotend"] = json!(
                    done.summary
                        .warnings
                        .iter()
                        .any(|warning| warning.code == HOTEND_MADE_PREVIEWS)
                );
                detail["sha256"] = json!(done.summary.sha256.to_ascii_lowercase());
                ("ok", Some(done.summary.sha256.to_ascii_lowercase()))
            }
            Err(error) => {
                detail["error"] = json!(truncate(error.code(), 64));
                ("failed", None)
            }
        };
        let _ = self.modeler.store().audit(NewAudit {
            project_id: Some(project_id.to_owned()),
            run_id: None,
            kind: "slice",
            name: "slice".to_owned(),
            args_sha256,
            outcome: outcome.to_owned(),
            detail,
        });
    }

    /// [`Gateway::slice_current`]; `relaid` after the version was laid out again for a
    /// wider brim. `known` collects what the audit entry names, as it becomes known.
    async fn slice_plate(
        &self,
        scope: &JobScope,
        project_id: &str,
        profile: &str,
        plate: Option<u32>,
        relaid: bool,
        known: &mut SliceAuditFacts,
    ) -> Result<SliceOutcome, SliceFailure> {
        let backend = self.slice_backend()?;
        let version = self.modeler.current_version(project_id)?;
        known.version = Some(version.number);
        printable(&version)?;
        let settings = self.modeler.settings().map_err(ModelingError::from)?;
        let preparation = self
            .modeler
            .prepared_for_print(scope, project_id, version.number)
            .await?;
        let plate = plate.unwrap_or(1);
        let no_plate = || SliceFailure::NoPlate {
            plate,
            plates: preparation.plates.len(),
            why: match preparation.unplaced.first() {
                Some(piece) if preparation.plates.is_empty() => format!(
                    "nothing could be placed: `{}` does not fit ({})",
                    truncate(&piece.name, 80),
                    truncate(&piece.reason, 400)
                ),
                _ => "pick a plate of the preparation".to_owned(),
            },
        };
        let layout = preparation
            .plates
            .iter()
            .find(|candidate| candidate.index == plate)
            .ok_or_else(no_plate)?;
        let plate_file = preparation.plate_file(plate).ok_or_else(no_plate)?;
        // The plate's print settings. A plan whose brim needs more room than the layout
        // kept lays the version out again first, once.
        let plate_plan = self.modeler.plate_plan(&preparation, plate)?;
        let room = preparation.clearance_mm.unwrap_or(0.0);
        if crate::modeling::required_clearance(&plate_plan.computed.plan) > room + 1e-9 {
            if relaid {
                return Err(SliceError::Failed {
                    code: "brim_does_not_fit".to_owned(),
                    message: format!(
                        "the brim of plate {plate} needs more room round its pieces than the \
                         layout could keep; make the pieces smaller or print fewer per plate"
                    ),
                }
                .into());
            }
            let options = preparation.options.clone();
            self.modeler
                .prepare(scope, project_id, Some(version.number), options)
                .await?;
            return Box::pin(self.slice_plate(
                scope,
                project_id,
                profile,
                Some(plate),
                true,
                known,
            ))
            .await;
        }
        let blocked = plate_plan.blocked();
        // A blocked plan goes on only to the stand-in, or to a backend that has no slicer
        // to run at all (it then fails with its own, more basic reason); a real slicer is
        // refused here with the plan's code.
        if let Some(notice) = &blocked
            && self.slicers_in_use(&backend, &settings).await == SlicersInUse::Real
        {
            return Err(SliceError::Failed {
                code: notice.code.clone(),
                message: format!(
                    "Hotend has no print settings for plate {plate} yet: {}",
                    notice.sentence
                ),
            }
            .into());
        }
        let plan_sha256 = blocked
            .is_none()
            .then(|| plate_plan.computed.sha256.clone());
        let nozzle_diameter_mm = plate_plan.basis.nozzle_diameter_mm;
        let nozzle_size_set = plate_plan.basis.nozzle_size_set;
        let pieces: Vec<PlacedPiece> = layout
            .items
            .iter()
            .map(|item| PlacedPiece {
                name: item.name.clone(),
                nozzle_id: item.nozzle_id,
                min: item.bbox.min,
                max: item.bbox.max,
            })
            .collect();
        // Hotend-made plate previews, when the person asked for them and the layout drew
        // both renders. Without them the slicer's own previews (if any) are all.
        let plate_previews = match settings.previews {
            PreviewsMode::Slicer => None,
            PreviewsMode::HotendMade => preparation
                .plate_view(plate, "iso")
                .zip(preparation.plate_view(plate, "top"))
                .map(|(iso, top)| PlateRenders { iso, top }),
        };
        // A changed plan, or another nozzle, is another slice in a folder of its own; so is
        // a slice with Hotend-made previews (the hash of a slice without them is unchanged).
        let mut identity = json!({
            "profile": profile,
            "plate": plate,
            "plan_sha256": plan_sha256,
            "nozzle_diameter_mm": nozzle_diameter_mm,
        });
        if plate_previews.is_some() {
            identity["previews"] = json!("hotend_made");
        }
        let profile_hash = json_hash(&identity)[..16].to_owned();
        known.slice_id = Some(profile_hash.clone());
        known.plan_sha256.clone_from(&plan_sha256);
        let version_dir = self
            .modeler
            .data()
            .version_dir(project_id, version.number)
            .map_err(ModelingError::from)?;
        let output_dir = version_dir.join("slice").join(&profile_hash);
        std::fs::create_dir_all(&output_dir).map_err(|source| ModelingError::Io {
            path: output_dir.clone(),
            source,
        })?;
        // The nozzle sizes the slice is made for, by nozzle id: the fitted size of the
        // settings (0.4 mm while it is assumed), one size for every nozzle of the printer in
        // this version. A print job is checked against these, not against a later value.
        let nozzles: Vec<Value> = crate::profiles::printer_profile(&settings.printer_id)
            .map(|printer| printer.nozzles)
            .unwrap_or_default()
            .iter()
            .map(|nozzle| json!({"id": nozzle.id, "diameter_mm": nozzle_diameter_mm}))
            .collect();
        let plate_type = settings.plate_type.clone();
        let mut job = json!({
            "plate": plate,
            "printer_id": settings.printer_id,
            "material": {"id": settings.material.id, "family": settings.material.family},
            // The build plate the person chose, once the slicer has confirmed it (below);
            // `null` for a slice made without a choice, which cannot become a print.
            "plate_type": null,
            "nozzles": nozzles,
            "pieces": pieces.iter().map(|piece| piece.name.as_str()).collect::<Vec<_>>(),
            // Which layout was sliced: a version laid out again needs a new slice.
            "prepared_at_ms": preparation.created_at_ms,
            // Which print settings: the plan's hash, or `null` when the plan was blocked
            // and the stand-in sliced with its own profiles. A changed plan needs a new
            // slice.
            "plan_sha256": plan_sha256,
            "plan_blocked": blocked.as_ref().map(|notice| notice.code.clone()),
            // The fitted nozzle size, and whether the person set it or it was assumed.
            "nozzle_diameter_mm": nozzle_diameter_mm,
            "nozzle_size_set": nozzle_size_set,
            // Where the plate previews were asked to come from: a slice made before
            // Hotend-made previews were switched on is told apart from one made after.
            "previews": settings.previews,
        });
        let summary = backend
            .slice(SliceRequest {
                project_id: project_id.to_owned(),
                version: version.number,
                plate_file,
                pieces,
                output_dir: output_dir.clone(),
                profile: profile.to_owned(),
                plate,
                printer_id: settings.printer_id,
                material: settings.material,
                plate_type: plate_type.clone(),
                slicer_executables: settings.slicer_executables,
                print_settings: plate_plan.computed.plan.settings.clone(),
                nozzle_diameter_mm,
                plate_previews,
            })
            .await?;
        // A blocked plan reaches a slicer only when it is the stand-in. A backend that turns
        // out to be a real slicer has made a file without Hotend's settings: not used.
        if let Some(notice) = &blocked
            && !summary
                .warnings
                .iter()
                .any(|warning| warning.code == STAND_IN_WARNING)
        {
            return Err(SliceError::Failed {
                code: notice.code.clone(),
                message: format!(
                    "Hotend has no print settings for plate {plate} yet, and the slicer is \
                     not the stand-in; the file is not used: {}",
                    notice.sentence
                ),
            }
            .into());
        }
        // The plate. A backend that sliced for another plate than the chosen one did not
        // do what was asked; its file would heat the bed for a plate that is not there.
        if let (Some(chosen), Some(bed)) = (&plate_type, &summary.bed) {
            if bed.id != *chosen {
                return Err(SliceError::Failed {
                    code: "bed_type_not_applied".to_owned(),
                    message: format!(
                        "the settings name the build plate `{}`, but the slicer reports a \
                         file sliced for `{}`; the file is not used",
                        truncate(chosen, 64),
                        truncate(&bed.id, 64)
                    ),
                }
                .into());
            }
            job["plate_type"] = json!(chosen);
        }
        let output = summary.output.clone();
        let (output_path, hashed) = tokio::task::spawn_blocking(move || {
            let path = import::resolve_inside(&output_dir, &output, MAX_SLICE_BYTES)
                .map_err(|e| e.to_string())?;
            let hash = import::sha256_file(&path).map_err(|e| e.to_string())?;
            Ok::<_, String>((path, hash))
        })
        .await
        .map_err(|e| e.to_string())
        .and_then(|result| result)
        .map_err(|message| SliceError::Failed {
            code: "unusable_output".to_owned(),
            message,
        })?;
        if !hashed.eq_ignore_ascii_case(&summary.sha256) {
            return Err(SliceError::Failed {
                code: "hash_mismatch".to_owned(),
                message: "the sliced file does not have the hash the slicer reported".to_owned(),
            }
            .into());
        }
        let record = SliceRecord {
            project_id: project_id.to_owned(),
            version: version.number,
            profile_hash,
            profile: profile.to_owned(),
            summary: json!(summary),
            job,
            created_at_ms: now_ms(),
        };
        self.modeler
            .store()
            .put_slice(&record)
            .map_err(ModelingError::from)?;
        Ok(SliceOutcome {
            record,
            summary,
            preparation,
            output_path,
        })
    }

    /// Which programs the injected backend would run: the stand-in only, at least one real
    /// slicer, or none it can use. Asked only for a plate whose plan is blocked.
    async fn slicers_in_use(
        &self,
        backend: &Arc<dyn SliceBackend>,
        settings: &crate::settings::Settings,
    ) -> SlicersInUse {
        let (backend, executables) = (Arc::clone(backend), settings.slicer_executables.clone());
        let rows = tokio::task::spawn_blocking(move || backend.installations(&executables))
            .await
            .unwrap_or_default();
        let usable: Vec<&SlicerInstallation> = rows.iter().filter(|row| row.usable).collect();
        if usable.is_empty() {
            SlicersInUse::None
        } else if usable.iter().all(|row| row.stand_in) {
            SlicersInUse::StandIn
        } else {
            SlicersInUse::Real
        }
    }

    /// The newest stored slice of a version, with the preparation it was made from; `None`
    /// when the version was not sliced or its files are gone.
    pub fn stored_slice(
        &self,
        project_id: &str,
        version: u32,
    ) -> Result<Option<SliceOutcome>, ModelingError> {
        let Some((record, preparation)) = self.slice_of_the_layout(project_id, version)? else {
            return Ok(None);
        };
        let Ok(summary) = serde_json::from_value::<SliceSummary>(record.summary.clone()) else {
            return Ok(None);
        };
        let output_dir = self
            .modeler
            .data()
            .version_dir(project_id, version)?
            .join("slice")
            .join(&record.profile_hash);
        let Ok(output_path) = import::resolve_inside(&output_dir, &summary.output, MAX_SLICE_BYTES)
        else {
            return Ok(None);
        };
        Ok(Some(SliceOutcome {
            record,
            summary,
            preparation,
            output_path,
        }))
    }

    /// The newest slice of a version together with the layout it was sliced from. A slice
    /// of an earlier layout (the version was prepared again since) does not count: what
    /// would be printed is no longer what the preparation shows.
    fn slice_of_the_layout(
        &self,
        project_id: &str,
        version: u32,
    ) -> Result<Option<(SliceRecord, Preparation)>, ModelingError> {
        let Some(record) = self.modeler.store().latest_slice(project_id, version)? else {
            return Ok(None);
        };
        let Some(preparation) = self.modeler.preparation(project_id, version)? else {
            return Ok(None);
        };
        if record.job["prepared_at_ms"].as_i64() != Some(preparation.created_at_ms) {
            return Ok(None);
        }
        // A slice made with Hotend's print settings counts only while they are still the
        // plate's plan: settings changed since give another plan, and a new slice.
        if let Some(sliced_with) = record.job["plan_sha256"].as_str() {
            let plate = record.job["plate"]
                .as_u64()
                .and_then(|plate| u32::try_from(plate).ok())
                .unwrap_or(1);
            let current = self.modeler.current_plate_plan(&preparation, plate)?;
            if current.blocked().is_some() || current.computed.sha256 != sliced_with {
                return Ok(None);
            }
        }
        Ok(Some((record, preparation)))
    }

    /// Creates a pending print job from the newest slice of the current version. This only
    /// writes a record for the user to review.
    pub fn propose_print(
        &self,
        project_id: &str,
        note: &str,
    ) -> Result<PendingPrintJob, SliceFailure> {
        self.slice_backend()?;
        let version = self.modeler.current_version(project_id)?;
        printable(&version)?;
        let store = self.modeler.store();
        let (record, _) = self
            .slice_of_the_layout(project_id, version.number)?
            .ok_or(SliceFailure::NoSlice {
                version: version.number,
            })?;
        let sha256 = record
            .summary
            .get("sha256")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        Ok(store
            .create_pending_print(&record, &sha256, note)
            .map_err(ModelingError::from)?)
    }

    /// What the editable Bambu Studio project of one plate of the current version is made
    /// from: the backend's request, for the current layout and the plate's current print
    /// plan, and the slice of that plan by a real slicer. Refused with the stand-in slicer
    /// or no slicer (`stand_in_slicer`, `slicer_not_installed`), for a blocked plan (the
    /// plan's code), and while there is no slice of the same plan of the same layout made
    /// by a real slicer (`no_slice`): the caller checks that slice's file with the G-code
    /// check before anything is made, so a plan whose file the check refuses never becomes
    /// a project. The request's folder is `versions/<n>/slicer-project/<plate>-<first 16
    /// hex digits of the plan's hash>/`.
    pub(crate) async fn project_plan(
        &self,
        project_id: &str,
        plate: Option<u32>,
    ) -> Result<ProjectPlan, SliceFailure> {
        let backend = self.slice_backend()?;
        let version = self.modeler.current_version(project_id)?;
        printable(&version)?;
        let settings = self.modeler.settings().map_err(ModelingError::from)?;
        let plate = plate.unwrap_or(1);
        let failed = |code: &str, message: String| {
            SliceFailure::Slice(SliceError::Failed {
                code: code.to_owned(),
                message,
            })
        };
        match self.slicers_in_use(&backend, &settings).await {
            SlicersInUse::Real => {}
            SlicersInUse::StandIn => {
                return Err(failed(
                    "stand_in_slicer",
                    "an editable Bambu Studio project is made by the real Bambu Studio only, \
                     and the slicer in use is Hotend's stand-in, whose files cannot be printed. \
                     Install Bambu Studio (or choose its program in the settings), slice the \
                     plate with it and make the project again"
                        .to_owned(),
                ));
            }
            SlicersInUse::None => {
                return Err(failed(
                    "slicer_not_installed",
                    "an editable Bambu Studio project is made by the real Bambu Studio, and no \
                     slicer Hotend can run was found. Install Bambu Studio or choose its \
                     program in the settings"
                        .to_owned(),
                ));
            }
        }
        let no_slice = || {
            failed(
                "no_slice",
                format!(
                    "plate {plate} of version {} has not been sliced by the real slicer with \
                     its current print plan and layout. Slice it first and let the G-code \
                     check pass; the editable project is made only from a plan whose sliced \
                     file passed the check",
                    version.number
                ),
            )
        };
        let preparation = self
            .modeler
            .preparation(project_id, version.number)?
            .ok_or_else(no_slice)?;
        let no_plate = || SliceFailure::NoPlate {
            plate,
            plates: preparation.plates.len(),
            why: "pick a plate of the preparation".to_owned(),
        };
        let layout = preparation
            .plates
            .iter()
            .find(|candidate| candidate.index == plate)
            .ok_or_else(no_plate)?;
        let plate_file = preparation.plate_file(plate).ok_or_else(no_plate)?;
        let plate_plan = self.modeler.plate_plan(&preparation, plate)?;
        if let Some(notice) = plate_plan.blocked() {
            return Err(failed(
                &notice.code,
                format!(
                    "Hotend has no print settings for plate {plate} yet, so there is no \
                     editable project to make: {}",
                    notice.sentence
                ),
            ));
        }
        let print_settings = plate_plan.computed.plan.settings.clone().ok_or_else(|| {
            failed(
                "print_settings_missing",
                format!("the print plan of plate {plate} holds no print settings"),
            )
        })?;
        let plan_sha256 = plate_plan.computed.sha256.clone();
        let slice = self
            .modeler
            .store()
            .slices_of(project_id, version.number)
            .map_err(ModelingError::from)?
            .into_iter()
            .find(|record| {
                let stand_in = record
                    .summary
                    .get("warnings")
                    .and_then(Value::as_array)
                    .is_some_and(|warnings| {
                        warnings
                            .iter()
                            .any(|warning| warning["code"].as_str() == Some(STAND_IN_WARNING))
                    });
                record.job["plate"].as_u64() == Some(u64::from(plate))
                    && record.job["plan_sha256"].as_str() == Some(plan_sha256.as_str())
                    && record.job["prepared_at_ms"].as_i64() == Some(preparation.created_at_ms)
                    && !stand_in
            })
            .ok_or_else(no_slice)?;
        let version_dir = self
            .modeler
            .data()
            .version_dir(project_id, version.number)
            .map_err(ModelingError::from)?;
        let output_dir = version_dir
            .join(PROJECT_DIR)
            .join(format!("{plate}-{}", &plan_sha256[..16]));
        std::fs::create_dir_all(&output_dir).map_err(|source| ModelingError::Io {
            path: output_dir.clone(),
            source,
        })?;
        let pieces = layout
            .items
            .iter()
            .map(|item| PlacedPiece {
                name: item.name.clone(),
                nozzle_id: item.nozzle_id,
                min: item.bbox.min,
                max: item.bbox.max,
            })
            .collect();
        let request = SliceRequest {
            project_id: project_id.to_owned(),
            version: version.number,
            plate_file,
            pieces,
            output_dir,
            profile: slice.profile.clone(),
            plate,
            printer_id: settings.printer_id,
            material: settings.material,
            plate_type: settings.plate_type,
            slicer_executables: settings.slicer_executables,
            print_settings: Some(print_settings),
            nozzle_diameter_mm: plate_plan.basis.nozzle_diameter_mm,
            // An editable project is sliced again by Bambu Studio, which draws its own.
            plate_previews: None,
        };
        Ok(ProjectPlan {
            request,
            slice,
            plan_sha256,
        })
    }

    /// Has the backend make the editable project of `plan` and checks what it answers: not
    /// a stand-in's, proven, for the chosen plate, and both files (the project and its test
    /// slice) regular files inside the request's folder with the hashes the backend
    /// reported.
    pub(crate) async fn export_project(
        &self,
        plan: &ProjectPlan,
    ) -> Result<ProjectExportSummary, SliceFailure> {
        let backend = self.slice_backend()?;
        let output_dir = plan.request.output_dir.clone();
        let summary = backend.export_project(plan.request.clone()).await?;
        let failed = |code: &str, message: &str| {
            SliceFailure::Slice(SliceError::Failed {
                code: code.to_owned(),
                message: message.to_owned(),
            })
        };
        if summary.stand_in
            || summary
                .slice
                .warnings
                .iter()
                .any(|warning| warning.code == STAND_IN_WARNING)
        {
            return Err(failed(
                "stand_in_slicer",
                "the slicer's answer is a stand-in's project, which is never handed over; an \
                 editable project is made by the real Bambu Studio only",
            ));
        }
        if summary.proof != "passed" {
            return Err(failed(
                "proof_failed",
                "the project, sliced on its own, did not pass the proof of Hotend's print \
                 settings; it is not used",
            ));
        }
        if summary.slice.bed.as_ref().map(|bed| bed.id.as_str())
            != plan.request.plate_type.as_deref()
        {
            return Err(failed(
                "bed_type_not_applied",
                "the project's test slice is not for the build plate chosen in the settings; \
                 it is not used",
            ));
        }
        let (project, test_slice) = (summary.project.clone(), summary.slice.output.clone());
        let (project_hash, slice_hash) = tokio::task::spawn_blocking(move || {
            let name = project.to_ascii_lowercase();
            if !name.ends_with(".3mf") || name.ends_with(".gcode.3mf") {
                return Err("the project is not a .3mf project file".to_owned());
            }
            let hash = |relative: &str| {
                let path = import::resolve_inside(&output_dir, relative, MAX_SLICE_BYTES)
                    .map_err(|e| e.to_string())?;
                import::sha256_file(&path).map_err(|e| e.to_string())
            };
            Ok((hash(&project)?, hash(&test_slice)?))
        })
        .await
        .map_err(|e| e.to_string())
        .and_then(|hashed| hashed)
        .map_err(|message| failed("unusable_output", &message))?;
        if !project_hash.eq_ignore_ascii_case(&summary.sha256)
            || !slice_hash.eq_ignore_ascii_case(&summary.slice.sha256)
        {
            return Err(failed(
                "hash_mismatch",
                "a file of the editable project does not have the hash the slicer reported",
            ));
        }
        Ok(summary)
    }

    /// Whether `plan_sha256` is, now, the plan of plate `plate` of the stored layout of a
    /// version under the current settings and proven baseline, and the plan is not blocked.
    /// The review and the confirmation of a print job, and of an editable project, are
    /// refused when it is not.
    pub fn plan_is_current(
        &self,
        project_id: &str,
        version: u32,
        plate: u32,
        plan_sha256: &str,
    ) -> Result<bool, ModelingError> {
        let Some(preparation) = self.modeler.preparation(project_id, version)? else {
            return Ok(false);
        };
        // With the proven baseline, as every other plan of the plate is computed.
        let current = self.modeler.current_plate_plan(&preparation, plate)?;
        Ok(current.blocked().is_none() && current.computed.sha256 == plan_sha256)
    }
}

/// The settings of a `sweep_parameters` call, checked against the current version's
/// parameters: every name declared, every value finite, inside the declared range and whole
/// for a count. One parameter at a time (the others as the version has them), or every
/// combination with `combine`; settings that repeat are tried once. Err is the sentence for
/// the model.
pub(crate) fn sweep_settings(
    args: &Value,
    specs: &BTreeMap<String, crate::engine::ParamSpec>,
) -> Result<Vec<BTreeMap<String, f64>>, String> {
    let max = crate::engine::MAX_SWEEP_SETTINGS;
    let items = args
        .get("params")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut swept: Vec<(String, Vec<f64>)> = Vec::new();
    for item in &items {
        let name = item.get("name").and_then(Value::as_str).unwrap_or("");
        let Some(spec) = specs.get(name) else {
            return Err(format!(
                "the current version has no parameter `{}`; its parameters are: {}",
                truncate(name, 40),
                specs.keys().cloned().collect::<Vec<_>>().join(", ")
            ));
        };
        if swept.iter().any(|(seen, _)| seen == name) {
            return Err(format!("`{name}` is named twice; name each parameter once"));
        }
        let number = |key: &str| item.get(key).and_then(Value::as_f64);
        let values: Vec<f64> = match (item.get("values"), number("from"), number("to")) {
            (Some(values), None, None) if item.get("steps").is_none() => values
                .as_array()
                .map(|values| values.iter().filter_map(Value::as_f64).collect())
                .unwrap_or_default(),
            (None, Some(from), Some(to)) => {
                let steps = item
                    .get("steps")
                    .and_then(Value::as_u64)
                    .unwrap_or(5)
                    .max(2);
                (0..steps)
                    .map(|i| from + (to - from) * i as f64 / (steps - 1) as f64)
                    .map(|value| (value * 1e6).round() / 1e6)
                    .collect()
            }
            _ => {
                return Err(format!(
                    "give `{name}` either `values` or `from` and `to` (with `steps`, 2 to 9), \
                     not both"
                ));
            }
        };
        let (low, high) = (spec.min, spec.max);
        if low.is_none() && high.is_none() {
            return Err(format!(
                "`{name}` declares no range, so it cannot be varied; give it a min and a max \
                 in PARAMS first"
            ));
        }
        for value in &values {
            let outside = !value.is_finite()
                || low.is_some_and(|low| *value < low - 1e-9)
                || high.is_some_and(|high| *value > high + 1e-9);
            if outside {
                return Err(format!(
                    "{name} = {value} is outside its declared range [{}, {}]; widen the range in \
                     PARAMS (write_model_code) to try it",
                    low.map_or("-inf".to_owned(), |v| v.to_string()),
                    high.map_or("inf".to_owned(), |v| v.to_string())
                ));
            }
            if spec.unit == "count" && value.fract() != 0.0 {
                return Err(format!("{name} is a count: {value} is not a whole number"));
            }
        }
        swept.push((name.to_owned(), values));
    }
    let mut settings: Vec<BTreeMap<String, f64>> = Vec::new();
    let mut push = |setting: BTreeMap<String, f64>| {
        if !settings.contains(&setting) {
            settings.push(setting);
        }
    };
    if args.get("combine").and_then(Value::as_bool) == Some(true) {
        let total: usize = swept.iter().map(|(_, values)| values.len()).product();
        if total > max {
            return Err(format!(
                "every combination is {total} settings; at most {max} are built in one call"
            ));
        }
        let mut combos = vec![BTreeMap::new()];
        for (name, values) in &swept {
            combos = combos
                .into_iter()
                .flat_map(|combo: BTreeMap<String, f64>| {
                    values.iter().map(move |value| {
                        let mut next = combo.clone();
                        next.insert(name.clone(), *value);
                        next
                    })
                })
                .collect();
        }
        combos.into_iter().for_each(&mut push);
    } else {
        let total: usize = swept.iter().map(|(_, values)| values.len()).sum();
        if total > max {
            return Err(format!(
                "that is {total} settings; at most {max} are built in one call"
            ));
        }
        for (name, values) in &swept {
            for value in values {
                push(BTreeMap::from([(name.clone(), *value)]));
            }
        }
    }
    if settings.is_empty() {
        return Err("name at least one value to try".to_owned());
    }
    Ok(settings)
}

/// The folder below a version's folder that holds its editable slicer projects.
pub const PROJECT_DIR: &str = "slicer-project";

/// What the editable project of one plate is made from ([`Gateway::project_plan`]).
#[derive(Debug, Clone)]
pub(crate) struct ProjectPlan {
    /// The backend's request; its `output_dir` is the project's folder.
    pub request: SliceRequest,
    /// The slice of the same plan and layout, by a real slicer.
    pub slice: SliceRecord,
    /// The plate's print plan, by its hash.
    pub plan_sha256: String,
}

/// The programs a slice backend would run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlicersInUse {
    /// Hotend's stand-in only.
    StandIn,
    /// At least one real slicer.
    Real,
    /// None it can use: a slice fails with the backend's own reason.
    None,
}

/// The warning code by which the stand-in slicer labels every result.
pub const STAND_IN_WARNING: &str = "fake_slicer";

impl SliceOutcome {
    /// Whether the stand-in slicer made this file (estimates only, not printable).
    pub fn is_stand_in(&self) -> bool {
        self.summary
            .warnings
            .iter()
            .any(|warning| warning.code == STAND_IN_WARNING)
    }

    /// Which settings the slice was made with, in words: "Hotend's print settings, plan
    /// <first 16 hex digits of its hash>", or the slicer's own profiles.
    pub fn settings_source(&self) -> String {
        let job = &self.record.job;
        let words = crate::modeling::settings_source(
            job["plan_sha256"].as_str(),
            job["plan_blocked"].as_str(),
        );
        if self.is_stand_in() {
            format!("{words} (sliced by Hotend's stand-in slicer: not printable)")
        } else {
            words
        }
    }
}

/// A version goes towards the printer only if every error-severity check passed.
fn printable(version: &crate::store::Version) -> Result<(), SliceFailure> {
    match version.status {
        VersionStatus::Ok => Ok(()),
        VersionStatus::CheckError => Err(SliceFailure::FailedChecks {
            version: version.number,
        }),
    }
}

/// What a slice's audit entry names, collected while the slice runs: a slice that fails
/// part-way records what was known by then.
#[derive(Debug, Default)]
struct SliceAuditFacts {
    version: Option<u32>,
    /// The slice's folder name (`slice/<id>`), the hash of its identity.
    slice_id: Option<String>,
    plan_sha256: Option<String>,
}

/// A finished slice: the stored record, the slicer's summary and the preparation it used.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SliceOutcome {
    pub record: SliceRecord,
    pub summary: SliceSummary,
    pub preparation: Preparation,
    /// The sliced file.
    pub output_path: PathBuf,
}

/// Why a slice or a print proposal failed.
#[derive(Debug, thiserror::Error)]
pub enum SliceFailure {
    #[error(transparent)]
    Slice(#[from] SliceError),
    #[error(transparent)]
    Modeling(#[from] ModelingError),
    /// The current version has not been sliced.
    #[error(
        "version {version} has not been sliced as it is now laid out on the plate; slice it \
         first"
    )]
    NoSlice { version: u32 },
    /// The preparation has no such plate.
    #[error("there is no plate {plate}: the version was laid out on {plates} plate(s); {why}")]
    NoPlate {
        plate: u32,
        plates: usize,
        /// Why not, in words: the first piece that fits no plate, when nothing was placed.
        why: String,
    },
    /// The current version failed a check of severity `error`.
    #[error(
        "version {version} failed its checks and cannot be sliced or printed; run the checks, \
         fix what they report and build a new version first"
    )]
    FailedChecks { version: u32 },
}

impl SliceFailure {
    /// A short stable code.
    pub fn code(&self) -> &str {
        match self {
            Self::Slice(SliceError::NotConfigured) => "not_configured",
            Self::Slice(SliceError::Failed { code, .. }) => code,
            Self::Modeling(error) => error.code(),
            Self::NoSlice { .. } => "no_slice",
            Self::NoPlate { .. } => "no_plate",
            Self::FailedChecks { .. } => "failed_checks",
        }
    }
}

impl From<SliceFailure> for ToolOutcome {
    fn from(error: SliceFailure) -> Self {
        Self::failure(&truncate(error.code(), 64), error.to_string())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::engine::ParamSpec;

    fn specs() -> BTreeMap<String, ParamSpec> {
        let spec = |min: Option<f64>, max: Option<f64>, unit: &str| ParamSpec {
            default: 2.0,
            min,
            max,
            unit: unit.to_owned(),
            label: "x".to_owned(),
            provenance: "style".to_owned(),
            source: None,
        };
        BTreeMap::from([
            ("wall".to_owned(), spec(Some(0.8), Some(3.0), "mm")),
            ("depth".to_owned(), spec(Some(30.0), Some(80.0), "mm")),
            ("holes".to_owned(), spec(Some(1.0), Some(6.0), "count")),
            ("cable".to_owned(), spec(None, None, "mm")),
        ])
    }

    fn one(name: &str, value: f64) -> BTreeMap<String, f64> {
        BTreeMap::from([(name.to_owned(), value)])
    }

    #[test]
    fn a_sweep_tries_one_parameter_at_a_time_or_every_combination() {
        let settings = sweep_settings(
            &json!({"params": [{"name": "wall", "from": 1.0, "to": 3.0, "steps": 3},
                               {"name": "depth", "values": [30, 60, 60]}]}),
            &specs(),
        )
        .unwrap();
        assert_eq!(
            settings,
            [
                one("wall", 1.0),
                one("wall", 2.0),
                one("wall", 3.0),
                one("depth", 30.0),
                one("depth", 60.0)
            ]
        );
        let combined = sweep_settings(
            &json!({"combine": true, "params": [{"name": "wall", "values": [1, 2]},
                                                {"name": "holes", "values": [1, 2, 3]}]}),
            &specs(),
        )
        .unwrap();
        assert_eq!(combined.len(), 6);
        assert!(combined.iter().all(|setting| setting.len() == 2));
    }

    #[test]
    fn a_sweep_outside_the_declared_ranges_or_too_large_is_refused_with_the_reason() {
        let refused = |args: Value| sweep_settings(&args, &specs()).unwrap_err();
        assert!(
            refused(json!({"params": [{"name": "colour", "values": [1]}]}))
                .contains("no parameter `colour`")
        );
        assert!(
            refused(json!({"params": [{"name": "wall", "values": [0.5]}]}))
                .contains("outside its declared range [0.8, 3]")
        );
        assert!(
            refused(json!({"params": [{"name": "holes", "values": [2.5]}]}))
                .contains("not a whole number")
        );
        assert!(
            refused(json!({"params": [{"name": "cable", "values": [10]}]}))
                .contains("declares no range")
        );
        assert!(
            refused(json!({"params": [{"name": "wall", "values": [1], "from": 1}]}))
                .contains("either `values` or `from` and `to`")
        );
        assert!(
            refused(json!({"params": [{"name": "wall", "values": [1]},
                                          {"name": "wall", "values": [2]}]}))
            .contains("named twice")
        );
        let many = json!({"combine": true, "params": [
            {"name": "wall", "from": 1, "to": 3, "steps": 5},
            {"name": "depth", "from": 30, "to": 80, "steps": 5}]});
        assert!(refused(many).contains("25 settings; at most 16"));
        let sum = json!({"params": [
            {"name": "wall", "from": 1, "to": 3, "steps": 9},
            {"name": "depth", "from": 30, "to": 80, "steps": 9}]});
        assert!(refused(sum).contains("18 settings; at most 16"));
    }
}
