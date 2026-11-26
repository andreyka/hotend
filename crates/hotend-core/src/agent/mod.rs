//! The agent supervisor: runs the agent child for one message and is the other end of its
//! JSON-RPC channel (PROTOCOL.md section 5).
//!
//! The agent is a sandboxed child (agent profile: no network, no files beyond its runtime)
//! that talks to the core over stdin and stdout, one JSON-RPC 2.0 message per line. The
//! supervisor:
//! - sends `agent.run` and waits for its result;
//! - answers `tool.call` through the [`Gateway`] and `llm.request` through the [`Egress`],
//!   several at a time, each bound to this run's id;
//! - forwards valid `agent.event` notifications to a broadcast channel;
//! - supports cancelling (`agent.cancel`, then a kill after a grace period) and a wall-clock
//!   limit;
//! - treats everything the child writes as hostile: lines are length-capped, parsed
//!   strictly, and a child that keeps breaking the protocol is stopped;
//! - ends a run whose calls keep being refused (the gateway's refusal limit);
//! - when the run ends, stops the engine jobs of calls still in flight and waits (bounded)
//!   for those calls to write their audit entries and remove their job folders;
//! - stops the child when the run ends, when the [`AgentRun`] is dropped, and when the
//!   runtime goes away, always with SIGTERM to the launcher first
//!   ([`crate::jobs::JobGuard`]), so the sandbox ends the whole process tree itself.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use hotend_sandbox::{Limits, Policy, Profile, SandboxError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::{JoinHandle, JoinSet};

use crate::egress::{Egress, LlmEvent, LlmRequest, ProviderKind, ResearchBlock};
use crate::gateway::{Gateway, RunSession};
use crate::jobs::{EXIT_WALL_CLOCK, JobGuard};
use crate::paths::{DataDir, PathError};
use crate::runtime::PythonRuntime;
use crate::util::truncate;

/// Event types the agent may emit.
pub const EVENT_TYPES: [&str; 5] = ["text_delta", "tool_start", "tool_end", "thinking", "usage"];
const MAX_EVENT_BYTES: usize = 64 * 1024;
const JSONRPC_INVALID_REQUEST: i64 = -32600;
const JSONRPC_METHOD_NOT_FOUND: i64 = -32601;
const JSONRPC_INVALID_PARAMS: i64 = -32602;
const JSONRPC_SERVER_ERROR: i64 = -32000;
const JSONRPC_BUSY: i64 = -32001;

/// How the agent child is started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Paths the child may read: its runtime.
    pub read_only: Vec<PathBuf>,
}

impl AgentCommand {
    /// The real agent: `python -I -m hotend_agent` in the given runtime.
    pub fn python_module(runtime: &PythonRuntime) -> Self {
        Self {
            program: runtime.python.clone(),
            args: vec!["-I".into(), "-m".into(), "hotend_agent".into()],
            read_only: runtime.read_only.clone(),
        }
    }
}

/// Limits of one agent run.
#[derive(Debug, Clone)]
pub struct AgentLimits {
    /// Wall-clock seconds for the whole run.
    pub wall_seconds: u64,
    /// How long the agent gets to stop by itself after a cancel.
    pub cancel_grace: Duration,
    /// Longest line the child may write.
    pub max_line_bytes: usize,
    /// Protocol violations tolerated before the child is stopped.
    pub max_protocol_errors: u32,
    /// Requests of the child handled at the same time.
    pub max_in_flight: usize,
    /// How long calls still in flight get, after the run has ended and their jobs were
    /// told to stop, to record their outcome and clean up before they are dropped.
    pub in_flight_grace: Duration,
}

impl Default for AgentLimits {
    fn default() -> Self {
        Self {
            wall_seconds: 1800,
            cancel_grace: Duration::from_secs(5),
            max_line_bytes: 48 * 1024 * 1024,
            max_protocol_errors: 8,
            max_in_flight: 8,
            in_flight_grace: Duration::from_secs(8),
        }
    }
}

/// Why an agent run failed. The child is no longer running when one of these is returned.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// The sandboxed command could not be prepared.
    #[error("the sandbox is not available, so the agent was not started: {0}")]
    Sandbox(#[from] SandboxError),
    /// The launcher could not be started.
    #[error("cannot start the agent: {0}")]
    Spawn(std::io::Error),
    /// The scratch folder of the run could not be prepared.
    #[error("cannot prepare the agent's scratch folder: {0}")]
    Scratch(String),
    #[error(transparent)]
    Path(#[from] PathError),
    /// The child exited before it answered `agent.run`.
    #[error("the agent stopped unexpectedly ({exit}): {stderr_tail}")]
    Exited { exit: String, stderr_tail: String },
    /// The child broke the protocol too often or in a way that cannot be recovered.
    #[error("the agent broke the protocol: {0}")]
    Protocol(String),
    /// The agent answered `agent.run` with a JSON-RPC error.
    #[error("the agent reported an error: {0}")]
    Reported(String),
    /// The run hit its wall-clock limit.
    #[error("the agent did not finish within {seconds} seconds and was stopped")]
    Timeout { seconds: u64 },
    /// The handle was dropped while the run was going on.
    #[error("the run was abandoned and the agent was stopped")]
    Abandoned,
    /// The agent kept making calls that were refused.
    #[error(
        "the agent made {limit} calls that were refused (unknown tools, invalid arguments, \
         used-up limits or model requests that are not allowed) and was stopped"
    )]
    TooManyRefusals { limit: u32 },
    /// The run fetched something from the web that Hotend does not keep in a conversation,
    /// and the agent handed it back in the conversation all the same.
    #[error(
        "{}{}, so this answer was not remembered: it would be sent, and paid for, again with \
         every later message. Ask again without that page and name the number you need, or \
         give it yourself{}",
        match .block {
            ResearchBlock::Pdf => "the assistant fetched a PDF from the web",
            ResearchBlock::TooLarge => "a web result was larger than Hotend keeps",
        },
        .page.as_ref().map(|page| format!(" ({page})")).unwrap_or_default(),
        match .block {
            ResearchBlock::Pdf => "; a project can also allow PDFs in its research settings",
            ResearchBlock::TooLarge => "",
        }
    )]
    WebResultNotKept {
        block: ResearchBlock,
        /// The page, as host and path, when the research log names it.
        page: Option<String>,
    },
}

impl AgentError {
    /// A short stable code for audit entries.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Sandbox(_) => "sandbox_unavailable",
            Self::Spawn(_) | Self::Scratch(_) | Self::Path(_) => "spawn_failed",
            Self::Exited { .. } => "exited",
            Self::Protocol(_) => "protocol_error",
            Self::Reported(_) => "agent_error",
            Self::Timeout { .. } => "timeout",
            Self::Abandoned => "abandoned",
            Self::TooManyRefusals { .. } => "too_many_refusals",
            Self::WebResultNotKept { .. } => "web_result_not_kept",
        }
    }
}

/// What to run.
#[derive(Debug, Clone)]
pub struct RunRequest {
    pub run_id: String,
    pub project_id: String,
    pub provider: ProviderKind,
    pub model: String,
    /// From [`crate::egress::Egress::begin_run`].
    pub base_url: String,
    /// From [`crate::egress::Egress::begin_run`].
    pub egress_token: String,
    /// Provider-native history, opaque to the core.
    pub history: Vec<Value>,
    pub user_text: String,
    pub max_turns: u32,
    /// The `research` object of `agent.run` (docs/WEB_RESEARCH.md); `Null` leaves it out.
    pub research: Value,
    /// Whether the run has web research: it is then offered `record_source`.
    pub research_tools: bool,
    /// Web research through a search service: the gateway's `web_search` and `web_fetch`.
    pub(crate) web: Option<Arc<crate::search::run::ServiceGrant>>,
    /// From [`crate::egress::Egress::begin_run`]: the egress ceiling on output tokens.
    pub max_output_tokens: u64,
}

/// Why the agent stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StopReason {
    Done,
    Budget,
    Cancelled,
    Error,
}

/// The result of `agent.run`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunOutcome {
    /// The new provider-native history, to be stored and passed to the next run.
    pub history: Vec<Value>,
    pub stop_reason: StopReason,
    #[serde(default)]
    pub usage: Value,
    /// What went wrong, when `stop_reason` is `error`.
    #[serde(default)]
    pub error: Option<String>,
    /// A hash of the instructions and tools the run's requests were made with
    /// (docs/WEB_RESEARCH.md). The core stores it and hands it to the next run.
    #[serde(default)]
    pub shape: Option<String>,
}

/// What subscribers of a run receive.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunEvent {
    /// An `agent.event` from the agent: an object with a `type` out of [`EVENT_TYPES`].
    /// `text_delta` and `thinking` carry `text`; the other fields are the agent's.
    /// One event in this place is the core's own: `type` `research`, sent just before the
    /// last event of a run that used the web (docs/WEB_RESEARCH.md).
    Agent { event: Value },
    /// The run ended and its history is stored. Sent last.
    Finished {
        stop_reason: StopReason,
        error: Option<String>,
    },
    /// The run failed; nothing was stored. Sent last.
    Failed { code: String, message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Signal {
    None,
    Cancel,
    Kill,
}

/// A running agent. Dropping it stops the child.
#[derive(Debug)]
pub struct AgentRun {
    signal: watch::Sender<Signal>,
    task: Option<JoinHandle<Result<RunOutcome, AgentError>>>,
}

/// Cancels a run from another task.
#[derive(Debug, Clone)]
pub struct CancelHandle(watch::Sender<Signal>);

impl CancelHandle {
    /// Asks the agent to stop; it is killed if it has not answered after the grace period.
    pub fn cancel(&self) {
        self.0.send_if_modified(|signal| {
            let change = *signal == Signal::None;
            if change {
                *signal = Signal::Cancel;
            }
            change
        });
    }
}

impl AgentRun {
    /// A handle that can cancel this run.
    pub fn cancel_handle(&self) -> CancelHandle {
        CancelHandle(self.signal.clone())
    }

    /// Waits for the run to end. The child has exited when this returns.
    pub async fn wait(mut self) -> Result<RunOutcome, AgentError> {
        let Some(task) = self.task.as_mut() else {
            return Err(AgentError::Abandoned);
        };
        // The handle stays in `self` while waiting, so dropping this future stops the child.
        let outcome = task.await.unwrap_or(Err(AgentError::Abandoned));
        self.task = None;
        outcome
    }
}

impl Drop for AgentRun {
    fn drop(&mut self) {
        if self.task.is_some() {
            let _ = self.signal.send(Signal::Kill);
        }
    }
}

/// Starts agent runs. Share it behind an `Arc`.
#[derive(Debug)]
pub struct AgentSupervisor {
    command: AgentCommand,
    data: DataDir,
    gateway: Arc<Gateway>,
    egress: Arc<Egress>,
    limits: AgentLimits,
}

impl AgentSupervisor {
    /// A supervisor that starts `command` in the agent sandbox for each run, answers its
    /// tool calls through `gateway` and its model requests through `egress`, and keeps the
    /// run's scratch folder under the data directory's `jobs/`.
    pub fn new(
        command: AgentCommand,
        data: DataDir,
        gateway: Arc<Gateway>,
        egress: Arc<Egress>,
        limits: AgentLimits,
    ) -> Self {
        Self {
            command,
            data,
            gateway,
            egress,
            limits,
        }
    }

    /// Spawns the agent child for one run and returns at once.
    ///
    /// `events` receives every valid `agent.event` of the run. The caller must have opened
    /// the run with [`Egress::begin_run`] and put the grant into `request`.
    pub fn start(
        &self,
        request: RunRequest,
        events: broadcast::Sender<RunEvent>,
    ) -> Result<AgentRun, AgentError> {
        let scratch = self.data.job_dir(&request.run_id)?;
        std::fs::create_dir(&scratch).map_err(|e| AgentError::Scratch(e.to_string()))?;
        // From here on the guard removes the scratch folder, after the launcher has gone.
        let mut guard = JobGuard::new(scratch.clone());
        guard.attach(self.spawn(&scratch)?);
        let (signal, watcher) = watch::channel(Signal::None);
        let context = RunContext {
            session: Arc::new(
                RunSession::new(&request.run_id, &request.project_id)
                    .with_research(request.research_tools)
                    .with_web(request.web.clone()),
            ),
            gateway: Arc::clone(&self.gateway),
            egress: Arc::clone(&self.egress),
            limits: self.limits.clone(),
            events,
        };
        let task = tokio::spawn(run_loop(guard, request, context, watcher));
        Ok(AgentRun {
            signal,
            task: Some(task),
        })
    }

    fn spawn(&self, scratch: &std::path::Path) -> Result<tokio::process::Child, AgentError> {
        let mut policy = Policy::new(Profile::Agent, scratch);
        policy.limits = Limits {
            wall_seconds: self.limits.wall_seconds.clamp(1, 24 * 3600),
            ..Profile::Agent.default_limits()
        };
        for path in &self.command.read_only {
            policy = policy.read_only(path);
        }
        let mut command = tokio::process::Command::from(hotend_sandbox::command(
            &policy,
            &self.command.program,
            &self.command.args,
        )?);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.spawn().map_err(AgentError::Spawn)
    }
}

struct RunContext {
    session: Arc<RunSession>,
    gateway: Arc<Gateway>,
    egress: Arc<Egress>,
    limits: AgentLimits,
    events: broadcast::Sender<RunEvent>,
}

/// One line read from the child.
enum Line {
    Message(Vec<u8>),
    TooLong,
    Closed,
}

/// Reads one line of at most `cap` bytes. A longer line is discarded up to its end.
async fn read_line_capped(reader: &mut (impl AsyncBufReadExt + Unpin), cap: usize) -> Line {
    let mut line = Vec::new();
    let mut too_long = false;
    loop {
        let available = match reader.fill_buf().await {
            Ok(bytes) if !bytes.is_empty() => bytes,
            _ => {
                return if line.is_empty() && !too_long {
                    Line::Closed
                } else if too_long {
                    Line::TooLong
                } else {
                    Line::Message(line)
                };
            }
        };
        let (piece, done) = match available.iter().position(|b| *b == b'\n') {
            Some(index) => (&available[..index], Some(index + 1)),
            None => (available, None),
        };
        if !too_long {
            if line.len() + piece.len() > cap {
                too_long = true;
                line = Vec::new();
            } else {
                line.extend_from_slice(piece);
            }
        }
        let consumed = done.unwrap_or(available.len());
        reader.consume(consumed);
        if done.is_some() {
            return if too_long {
                Line::TooLong
            } else {
                Line::Message(line)
            };
        }
    }
}

async fn read_lines(stdout: impl AsyncRead + Unpin, cap: usize, lines: mpsc::Sender<Line>) {
    let mut reader = BufReader::with_capacity(64 * 1024, stdout);
    loop {
        let line = read_line_capped(&mut reader, cap).await;
        let closed = matches!(line, Line::Closed);
        if lines.send(line).await.is_err() || closed {
            return;
        }
    }
}

async fn write_lines(mut stdin: tokio::process::ChildStdin, mut lines: mpsc::Receiver<String>) {
    while let Some(mut line) = lines.recv().await {
        line.push('\n');
        if stdin.write_all(line.as_bytes()).await.is_err() || stdin.flush().await.is_err() {
            return;
        }
    }
}

fn response(id: &Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

fn error_response(id: &Value, code: i64, message: &str, data: Option<Value>) -> String {
    let mut error = json!({"code": code, "message": truncate(message, 2000)});
    if let Some(data) = data {
        error["data"] = data;
    }
    json!({"jsonrpc": "2.0", "id": id, "error": error}).to_string()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolCallParams {
    run_id: String,
    name: String,
    #[serde(default)]
    args: Value,
    /// The look review's own views after a build (`Gateway::look_views`), not the model's
    /// call: only `render_views` may carry it.
    #[serde(default)]
    auto: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventParams {
    run_id: String,
    event: Value,
}

/// Why a run ended when the agent's output closed, from the launcher's exit status.
///
/// The launcher enforces the same wall-clock limit as the run's own deadline and may reach it
/// first; its kill closes the output, so that exit is a timeout, not an agent that stopped.
fn closed_error(status: Option<std::process::ExitStatus>, wall_seconds: u64) -> AgentError {
    match status {
        Some(status) if status.code() == Some(EXIT_WALL_CLOCK) => AgentError::Timeout {
            seconds: wall_seconds,
        },
        Some(status) => AgentError::Exited {
            exit: status.to_string(),
            stderr_tail: String::new(),
        },
        None => AgentError::Exited {
            exit: "its output closed".to_owned(),
            stderr_tail: String::new(),
        },
    }
}

async fn run_loop(
    mut guard: JobGuard,
    request: RunRequest,
    context: RunContext,
    mut signal: watch::Receiver<Signal>,
) -> Result<RunOutcome, AgentError> {
    let no_pipes = || AgentError::Protocol("the agent has no stdio pipes".to_owned());
    let Some(child) = guard.child() else {
        return Err(no_pipes());
    };
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        guard.stop().await;
        return Err(no_pipes());
    };
    let stderr = tokio::spawn(read_stderr_tail(child.stderr.take()));
    let (to_child, outbox) = mpsc::channel::<String>(256);
    let writer = tokio::spawn(write_lines(stdin, outbox));
    let (line_tx, mut lines) = mpsc::channel::<Line>(32);
    let reader = tokio::spawn(read_lines(stdout, context.limits.max_line_bytes, line_tx));

    let mut run_message = json!({
        "jsonrpc": "2.0", "id": 1, "method": "agent.run",
        "params": {
            "run_id": request.run_id,
            "project_id": request.project_id,
            "provider": {"kind": request.provider, "model": request.model,
                         "base_url": request.base_url},
            "egress_token": request.egress_token,
            "history": request.history,
            "user_message": {"text": request.user_text, "attachments": []},
            "tools": context.gateway.tool_specs_for_session(&context.session),
            "budget": {"max_turns": request.max_turns,
                       "max_output_tokens": request.max_output_tokens},
        }
    });
    if !request.research.is_null() {
        run_message["params"]["research"] = request.research.clone();
    }
    let run_message = run_message.to_string();
    let _ = to_child.send(run_message).await;

    let deadline = tokio::time::sleep(Duration::from_secs(context.limits.wall_seconds));
    tokio::pin!(deadline);
    let mut cancel_deadline: Option<std::pin::Pin<Box<tokio::time::Sleep>>> = None;
    let mut in_flight: JoinSet<()> = JoinSet::new();
    let mut protocol_errors = 0u32;

    let outcome: Result<RunOutcome, AgentError> = loop {
        let cancel_timer = async {
            match cancel_deadline.as_mut() {
                Some(timer) => timer.await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            line = lines.recv() => {
                let bytes = match line {
                    Some(Line::Message(bytes)) => bytes,
                    Some(Line::TooLong) => break Err(AgentError::Protocol(
                        "a message was longer than the line limit".to_owned())),
                    Some(Line::Closed) | None => {
                        let status = match guard.child() {
                            Some(child) => tokio::time::timeout(Duration::from_secs(5), child.wait())
                                .await
                                .ok()
                                .and_then(Result::ok),
                            None => None,
                        };
                        break Err(closed_error(status, context.limits.wall_seconds));
                    }
                };
                if bytes.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                match handle_message(&bytes, &request, &context, &to_child, &mut in_flight).await {
                    Handled::Continue => {}
                    Handled::Finished(outcome) => break outcome,
                    Handled::Violation(reason) => {
                        protocol_errors += 1;
                        if protocol_errors > context.limits.max_protocol_errors {
                            break Err(AgentError::Protocol(reason));
                        }
                    }
                }
            }
            Some(_) = in_flight.join_next(), if !in_flight.is_empty() => {}
            () = context.session.exhausted() => break Err(AgentError::TooManyRefusals {
                limit: context.gateway.policy().max_refusals_per_run,
            }),
            _ = &mut deadline => break Err(AgentError::Timeout { seconds: context.limits.wall_seconds }),
            _ = cancel_timer => break Ok(RunOutcome {
                history: request.history.clone(),
                stop_reason: StopReason::Cancelled,
                usage: Value::Null,
                error: None,
                shape: None,
            }),
            changed = signal.changed() => {
                let now = if changed.is_ok() { *signal.borrow() } else { Signal::Kill };
                match now {
                    Signal::Kill => break Err(AgentError::Abandoned),
                    Signal::Cancel if cancel_deadline.is_none() => {
                        let cancel = json!({"jsonrpc": "2.0", "id": 2, "method": "agent.cancel",
                                            "params": {"run_id": request.run_id}});
                        let _ = to_child.try_send(cancel.to_string());
                        // Work in flight is not wanted any more: its engine jobs stop and
                        // the calls answer `cancelled`, so the agent can finish at once.
                        context.session.stop();
                        cancel_deadline = Some(Box::pin(tokio::time::sleep(context.limits.cancel_grace)));
                    }
                    _ => {}
                }
            }
        }
    };

    // The run is over. Calls still in flight are not dropped where they stand: that would
    // lose their audit entries and leave their job folders behind. Their engine jobs are
    // told to stop, their model requests end, and each call then finishes its own
    // bookkeeping. Only a call that does not come back in time is dropped; its guards
    // still record it as abandoned and remove its folder.
    context.session.stop();
    let drained = async { while in_flight.join_next().await.is_some() {} };
    if tokio::time::timeout(context.limits.in_flight_grace, drained)
        .await
        .is_err()
    {
        in_flight.shutdown().await;
    }
    context.gateway.close_session(&context.session);

    // Whatever happened, the child must be gone before this returns.
    drop(to_child);
    let _ = tokio::time::timeout(Duration::from_secs(1), writer).await;
    if let Some(child) = guard.child()
        && tokio::time::timeout(Duration::from_secs(2), child.wait())
            .await
            .is_err()
    {
        guard.stop().await;
    }
    // The launcher has exited, so this removes the scratch folder at once.
    drop(guard);
    reader.abort();
    let stderr_tail = tokio::time::timeout(Duration::from_secs(2), stderr)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    match outcome {
        Err(AgentError::Exited { exit, .. }) => Err(AgentError::Exited { exit, stderr_tail }),
        other => other,
    }
}

async fn read_stderr_tail(pipe: Option<tokio::process::ChildStderr>) -> String {
    let Some(pipe) = pipe else {
        return String::new();
    };
    let mut reader = BufReader::new(pipe);
    let mut tail: Vec<String> = Vec::new();
    loop {
        match read_line_capped(&mut reader, 4096).await {
            Line::Message(bytes) => {
                tail.push(String::from_utf8_lossy(&bytes).into_owned());
                if tail.len() > 40 {
                    tail.remove(0);
                }
            }
            Line::TooLong => {}
            Line::Closed => break,
        }
    }
    truncate(&tail.join("\n"), 4000)
}

enum Handled {
    Continue,
    Finished(Result<RunOutcome, AgentError>),
    Violation(String),
}

async fn handle_message(
    bytes: &[u8],
    request: &RunRequest,
    context: &RunContext,
    to_child: &mpsc::Sender<String>,
    in_flight: &mut JoinSet<()>,
) -> Handled {
    let Ok(Value::Object(mut message)) = serde_json::from_slice::<Value>(bytes) else {
        return Handled::Violation("a line was not a JSON object".to_owned());
    };
    if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Handled::Violation("a message was not JSON-RPC 2.0".to_owned());
    }
    let id = message.remove("id");
    let params = message.remove("params").unwrap_or(Value::Null);
    let Some(method) = message
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        // A response. Only `agent.run` (id 1) carries a result the core uses.
        return match id.as_ref().and_then(Value::as_u64) {
            Some(1) => {
                if let Some(error) = message.get("error") {
                    let text = error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error");
                    return Handled::Finished(Err(AgentError::Reported(truncate(text, 2000))));
                }
                let result = message.remove("result").unwrap_or(Value::Null);
                Handled::Finished(serde_json::from_value::<RunOutcome>(result).map_err(|e| {
                    AgentError::Protocol(format!(
                        "the result of agent.run is malformed: {}",
                        truncate(&e.to_string(), 200)
                    ))
                }))
            }
            Some(2) => Handled::Continue,
            _ => Handled::Violation("a response had an unknown id".to_owned()),
        };
    };

    let Some(id) = id else {
        // A notification.
        if method != "agent.event" {
            return Handled::Violation(format!("unknown notification `{}`", truncate(&method, 40)));
        }
        let valid = serde_json::from_value::<EventParams>(params)
            .ok()
            .filter(|p| {
                bytes.len() <= MAX_EVENT_BYTES
                    && p.run_id == request.run_id
                    && p.event.is_object()
                    && p.event
                        .get("type")
                        .and_then(Value::as_str)
                        .is_some_and(|t| EVENT_TYPES.contains(&t))
            });
        return match valid {
            Some(params) => {
                // No subscriber is not an error: events are best effort.
                let _ = context.events.send(RunEvent::Agent {
                    event: params.event,
                });
                Handled::Continue
            }
            None => Handled::Violation("an agent.event was malformed".to_owned()),
        };
    };
    if !(id.is_u64() || id.is_string()) || id.to_string().len() > 128 {
        return Handled::Violation("a request id was not a short number or string".to_owned());
    }

    // Replies sent from the supervisor's own loop must never block it: a child that stops
    // reading its input while flooding its output would otherwise stall the run.
    let reply = |line: String| to_child.try_send(line).is_ok();
    if in_flight.len() >= context.limits.max_in_flight {
        if !reply(error_response(
            &id,
            JSONRPC_BUSY,
            "too many requests in flight",
            None,
        )) {
            return Handled::Violation("the agent does not read its input".to_owned());
        }
        return Handled::Continue;
    }
    match method.as_str() {
        "tool.call" => {
            let params = match serde_json::from_value::<ToolCallParams>(params) {
                Ok(params)
                    if params.run_id == request.run_id
                        && (!params.auto || params.name == "render_views") =>
                {
                    params
                }
                _ => {
                    reply(error_response(
                        &id,
                        JSONRPC_INVALID_PARAMS,
                        "tool.call needs {run_id, name, args} for the current run (and `auto` \
                         only for render_views)",
                        None,
                    ));
                    return Handled::Violation("a tool.call had invalid params".to_owned());
                }
            };
            let gateway = Arc::clone(&context.gateway);
            let session = Arc::clone(&context.session);
            let to_child = to_child.clone();
            in_flight.spawn(async move {
                let outcome = if params.auto {
                    gateway.look_views(&session).await
                } else {
                    gateway.call(&session, &params.name, params.args).await
                };
                let _ = to_child.send(response(&id, json!(outcome))).await;
            });
        }
        "llm.request" => {
            let params = match serde_json::from_value::<LlmRequest>(params) {
                Ok(params) if params.run_id == request.run_id && params.request_id.len() <= 128 => {
                    params
                }
                _ => {
                    reply(error_response(
                        &id,
                        JSONRPC_INVALID_PARAMS,
                        "llm.request needs {run_id, request_id, token, method, path, headers, \
                         body_b64} for the current run",
                        None,
                    ));
                    return Handled::Violation("an llm.request had invalid params".to_owned());
                }
            };
            let egress = Arc::clone(&context.egress);
            let gateway = Arc::clone(&context.gateway);
            let session = Arc::clone(&context.session);
            let to_child = to_child.clone();
            in_flight.spawn(async move {
                let request_id = params.request_id.clone();
                let (events, mut stream) = mpsc::channel::<LlmEvent>(16);
                let forward = {
                    let to_child = to_child.clone();
                    async move {
                        while let Some(event) = stream.recv().await {
                            let (method, mut params) = match event {
                                LlmEvent::Start { status, headers } => (
                                    "llm.response_start",
                                    json!({"status": status, "headers": headers}),
                                ),
                                LlmEvent::Chunk(bytes) => (
                                    "llm.response_chunk",
                                    json!({"data_b64": BASE64.encode(bytes)}),
                                ),
                            };
                            params["request_id"] = json!(request_id);
                            let line =
                                json!({"jsonrpc": "2.0", "method": method, "params": params});
                            if to_child.send(line.to_string()).await.is_err() {
                                break;
                            }
                        }
                    }
                };
                let send = async {
                    let result = egress.handle(params, &events).await;
                    drop(events);
                    result
                };
                // When the run ends, the request is dropped where it stands; the egress
                // records it as abandoned.
                let result = tokio::select! {
                    (result, ()) = async { tokio::join!(send, forward) } => result,
                    () = session.stopped() => return,
                };
                let line = match result {
                    Ok(()) => response(&id, json!({"ok": true})),
                    Err(error) => {
                        if error.is_refusal() {
                            gateway.note_refusal(&session, error.code());
                        }
                        error_response(
                            &id,
                            JSONRPC_SERVER_ERROR,
                            &error.to_string(),
                            Some(json!({"code": error.code()})),
                        )
                    }
                };
                let _ = to_child.send(line).await;
            });
        }
        other => {
            let known = other.len() <= 64;
            reply(error_response(
                &id,
                if known {
                    JSONRPC_METHOD_NOT_FOUND
                } else {
                    JSONRPC_INVALID_REQUEST
                },
                "unknown method; the core answers tool.call and llm.request",
                None,
            ));
            return Handled::Violation("an unknown method was called".to_owned());
        }
    }
    Handled::Continue
}

/// The readable text a run produced: the concatenation of its `text_delta` events.
pub fn text_of(event: &RunEvent) -> Option<&str> {
    match event {
        RunEvent::Agent { event }
            if event.get("type").and_then(Value::as_str) == Some("text_delta") =>
        {
            event.get("text").and_then(Value::as_str)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests;
