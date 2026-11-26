//! Running one engine job inside the sandbox and reading its result as hostile input.
//!
//! Guarantees:
//! - The worker only ever runs through `hotend_sandbox::command` with the engine profile; if
//!   the sandbox cannot be applied the job fails with [`JobError::Sandbox`] or
//!   [`JobError::SandboxRefused`] and nothing runs.
//! - The wall-clock limit is enforced twice: by the launcher and by a timer here.
//! - Logs are kept to a bounded tail; `result.json` is size-capped, parsed strictly and
//!   sanitised before a [`FinishedJob`] exists.
//! - Every artefact is reached through [`FinishedJob::copy_artifact`] or
//!   [`FinishedJob::read_artifact`], which resolve the reported path inside the job folder,
//!   refuse symbolic links and enforce a size cap.
//! - Every job leaves one audit entry, whether it succeeded, failed, was cancelled with its
//!   run or was abandoned because its caller went away; the entry says whether the sandbox
//!   was on and which launcher applied it.
//! - The job folder is deleted when the job fails, when it is cancelled or abandoned (after
//!   the launcher has ended the worker's whole process tree) and when a [`FinishedJob`] is
//!   dropped. A launcher is never left running: [`JobGuard`] stops it with SIGTERM, so it
//!   takes its tree down itself, before any SIGKILL.
//! - A worker that crashes (it died without a result, by a signal or an exit code the
//!   protocol does not know) is started once more on a fresh copy of the same inputs; a
//!   second crash fails the job. Each crash is kept for study in `crashes/<job-id>/`, a
//!   folder the core creates new: `crash.json` (exit status and signal, the end of stderr, the
//!   operation, how long it ran, the peak memory seen and the limits) and the core's own
//!   inputs of the job (`request.json`, `restart.json`, the input files up to
//!   [`MAX_KEPT_INPUT_BYTES`] each and [`MAX_KEPT_CRASH_BYTES`] together), written from what
//!   the core holds. Nothing the worker wrote is kept, opened or followed: its job folder is
//!   removed, so a planted symbolic link or FIFO is never touched. The audit entry carries the
//!   same facts, and a [`JobNotice`] tells the run's person what happened. Only the newest
//!   [`MAX_KEPT_CRASHES`] crash folders are kept.
//! - A job the launcher killed at its process, memory or disk limit (its own exit code
//!   [`EXIT_LIMIT`]) is no crash: it is [`JobError::LimitReached`], not restarted and not
//!   kept, and the person is told which limit it was.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hotend_sandbox::{Limits, Policy, Profile, SandboxError};
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Child;
use tokio::sync::{Semaphore, broadcast, watch};

use crate::engine::{EngineRequest, EngineResult, EngineStatus};
use crate::import::{self, ImportError};
use crate::paths::{DataDir, PathError};
use crate::runtime::PythonRuntime;
use crate::store::{NewAudit, PendingAudit, Store};
use crate::util::{new_id, truncate};

/// Largest accepted `result.json`.
pub const MAX_RESULT_BYTES: u64 = 4 * 1024 * 1024;
/// Largest file the core writes into a job folder as input.
pub const MAX_INPUT_BYTES: u64 = 512 * 1024 * 1024;
/// Exit code of the launcher at its wall-clock limit (PROTOCOL.md section 7).
pub(crate) const EXIT_WALL_CLOCK: i32 = 124;
/// Exit code of the launcher when the sandbox could not be applied.
const EXIT_SANDBOX_FAILED: i32 = 125;
/// Exit code of the worker for a request it refuses.
const EXIT_BAD_REQUEST: i32 = 2;
/// Exit code of the launcher when it killed the job at its process, memory or disk limit.
pub(crate) const EXIT_LIMIT: i32 = hotend_sandbox::error::EXIT_LIMIT;
/// Crash folders kept under `crashes/`; older ones are removed when a new one is kept.
pub const MAX_KEPT_CRASHES: usize = 10;
/// Largest input file copied into a kept crash folder; a larger one is only named.
pub const MAX_KEPT_INPUT_BYTES: u64 = 1024 * 1024;
/// Largest total of the input files of one kept crash folder.
pub const MAX_KEPT_CRASH_BYTES: u64 = 4 * 1024 * 1024;
/// The file in a restarted job's folder that says it is a restart (and of what).
pub const RESTART_FILE: &str = "restart.json";
/// How often the memory of a running job's processes is sampled (Linux only).
const MEMORY_SAMPLE_INTERVAL: Duration = Duration::from_millis(200);

/// Something the job runner tells the person whose run a job belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobNotice {
    pub project_id: Option<String>,
    pub run_id: Option<String>,
    /// One plain sentence or two, for the person.
    pub text: String,
}

/// What is known about a worker that crashed: kept in `crash.json` and the audit entry.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CrashReport {
    pub job_id: String,
    pub op: String,
    /// 1 for the first run of the request, 2 for its one restart.
    pub attempt: u32,
    /// The launcher's exit status as the platform prints it.
    pub exit: String,
    pub exit_code: Option<i32>,
    /// The signal that ended the launcher itself, if one did.
    pub signal: Option<i32>,
    /// The worker's signal as the launcher passes it on (an exit code of 128 + N), with its
    /// name when it is a common one.
    pub worker_signal: Option<i32>,
    pub worker_signal_name: Option<&'static str>,
    pub core_dumped: bool,
    pub elapsed_ms: u64,
    /// The largest sum of the resident memory of the job's processes, sampled every 200 ms.
    pub peak_tree_rss_mb: Option<f64>,
    /// The largest high-water mark of resident memory of any one process of the job.
    pub peak_process_rss_mb: Option<f64>,
    pub memory_limit_mb: u64,
    pub wall_limit_s: u64,
    /// The end of the worker's stderr.
    pub stderr_tail: String,
    /// Where the crash was kept, when it could be.
    pub kept: Option<PathBuf>,
    /// Input files that were too large to keep with the crash.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub left_out: Vec<String>,
}

impl CrashReport {
    /// The exit in a few words: "signal 6 (SIGABRT)", "exit code 7".
    pub fn summary(&self) -> String {
        match (self.signal, self.worker_signal, self.exit_code) {
            (Some(signal), _, _) => format!(
                "signal {signal}{}",
                signal_name(signal)
                    .map(|name| format!(" ({name})"))
                    .unwrap_or_default()
            ),
            (None, Some(signal), Some(code)) => format!(
                "exit code {code}, signal {signal}{} in the worker",
                self.worker_signal_name
                    .map(|name| format!(" ({name})"))
                    .unwrap_or_default()
            ),
            (None, None, Some(code)) => format!("exit code {code}"),
            _ => self.exit.clone(),
        }
    }
}

fn signal_name(signal: i32) -> Option<&'static str> {
    Some(match signal {
        4 => "SIGILL",
        6 => "SIGABRT",
        7 => "SIGBUS",
        8 => "SIGFPE",
        9 => "SIGKILL",
        11 => "SIGSEGV",
        13 => "SIGPIPE",
        15 => "SIGTERM",
        24 => "SIGXCPU",
        25 => "SIGXFSZ",
        _ => return None,
    })
}

/// What one run of the worker left behind besides its result, for a crash report.
#[derive(Debug, Default)]
struct RunFacts {
    status: Option<std::process::ExitStatus>,
    peak_tree_rss_kb: Option<u64>,
    peak_process_rss_kb: Option<u64>,
    wall_seconds: u64,
}

/// Why an engine job produced no usable result. None of these ever becomes a version.
#[derive(Debug, thiserror::Error)]
pub enum JobError {
    /// The sandboxed command could not be prepared (no launcher, unsupported platform).
    #[error("the sandbox is not available, so the job was not started: {0}")]
    Sandbox(#[from] SandboxError),
    /// The launcher refused to start the worker.
    #[error(
        "the sandbox could not be applied on this machine, so the job was not started: {detail}"
    )]
    SandboxRefused { detail: String },
    /// The launcher process could not be started.
    #[error("cannot start the engine worker: {0}")]
    Spawn(std::io::Error),
    /// The job ran past its wall-clock limit and was killed.
    #[error("the job did not finish within {seconds} seconds and was stopped")]
    Timeout { seconds: u64 },
    /// The worker died without a result (for example a geometry kernel crash).
    #[error("the engine worker crashed ({exit}): {stderr_tail}")]
    Crashed { exit: String, stderr_tail: String },
    /// The launcher stopped the job at a sandbox limit (processes, memory or the job folder's
    /// size). Not a crash: the same request would hit it again, so it is not restarted.
    #[error(
        "the engine job was stopped at its sandbox limit ({reason}); it was not restarted. \
         Make the model lighter (fewer processes, less memory or smaller files)"
    )]
    LimitReached { reason: String },
    /// The worker refused the request; the message says why.
    #[error("the engine refused the request: {message}")]
    BadRequest { message: String },
    /// The worker reported an internal error.
    #[error("the engine failed internally ({kind}): {message}")]
    EngineInternal { kind: String, message: String },
    /// `result.json` is missing, too large, not valid or breaks the protocol.
    #[error("the engine's result is malformed: {reason}")]
    MalformedResult { reason: String },
    /// A reported artefact is not a plain file inside the job folder.
    #[error("the engine reported an unusable file: {0}")]
    Artifact(#[from] ImportError),
    /// The job folder could not be prepared.
    #[error("cannot prepare the job folder {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// A data path could not be built.
    #[error(transparent)]
    Path(#[from] PathError),
    /// The run the job belonged to ended first; the worker was stopped.
    #[error("the job was stopped because the run it belonged to ended")]
    Cancelled,
}

impl JobError {
    /// A short stable code for audit entries and tool errors.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Sandbox(_) | Self::SandboxRefused { .. } => "sandbox_unavailable",
            Self::Spawn(_) => "spawn_failed",
            Self::Timeout { .. } => "timeout",
            Self::Crashed { .. } => "crashed",
            Self::LimitReached { .. } => "limit_reached",
            Self::BadRequest { .. } => "bad_request",
            Self::EngineInternal { .. } => "engine_internal_error",
            Self::MalformedResult { .. } => "malformed_result",
            Self::Artifact(_) => "unsafe_artifact",
            Self::Io { .. } | Self::Path(_) => "io_error",
            Self::Cancelled => "cancelled",
        }
    }
}

/// A file the core places in the job folder before the worker starts.
#[derive(Debug, Clone)]
pub enum JobFile {
    /// Written from memory.
    Bytes(Vec<u8>),
    /// Copied from a file the core owns (a version folder or the calibration folder).
    Copy(PathBuf),
}

/// Who a job runs for; copied into its audit entry.
#[derive(Debug, Clone, Default)]
pub struct JobScope {
    pub project_id: Option<String>,
    pub run_id: Option<String>,
    /// Becomes true when the run the job belongs to has ended; the job then stops with
    /// [`JobError::Cancelled`]. `None` for jobs that belong to no run.
    pub cancel: Option<watch::Receiver<bool>>,
}

impl JobScope {
    /// Resolves once the scope is cancelled; never for a scope that cannot be.
    async fn cancelled(&self) {
        if let Some(mut cancel) = self.cancel.clone()
            && cancel.wait_for(|stop| *stop).await.is_ok()
        {
            return;
        }
        std::future::pending().await
    }
}

/// How long a launcher gets to end its process tree after SIGTERM before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(5);

/// A sandbox launcher and the job folder its program works in.
///
/// Dropping the guard ends the job in an orderly way, on any thread and with or without an
/// async runtime: a launcher that is still running gets SIGTERM, which makes it kill its
/// whole process tree and exit; only if it has not gone after [`STOP_GRACE`] is it killed
/// (its watchdog then ends the tree). The folder is removed after the launcher has gone,
/// so nothing writes into a folder that is being deleted. A launcher is never killed
/// first: without a PID namespace that would orphan the program's descendants.
#[derive(Debug)]
pub(crate) struct JobGuard {
    child: Option<Child>,
    dir: Option<PathBuf>,
}

impl JobGuard {
    /// A guard for a job folder whose launcher has not been started yet.
    pub fn new(dir: PathBuf) -> Self {
        Self {
            child: None,
            dir: Some(dir),
        }
    }

    /// Hands the started launcher to the guard.
    pub fn attach(&mut self, child: Child) -> &mut Child {
        self.child.insert(child)
    }

    /// The launcher, once attached.
    pub fn child(&mut self) -> Option<&mut Child> {
        self.child.as_mut()
    }

    /// Stops the launcher now and waits for it: SIGTERM, then a kill after the grace period.
    pub async fn stop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            stop(child).await;
        }
    }

    /// Gives the folder to the caller, who removes it later. The launcher must have exited.
    pub fn keep_dir(mut self) -> Option<PathBuf> {
        self.dir.take()
    }
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        let dir = self.dir.take();
        let remove = move || {
            if let Some(dir) = dir {
                let _ = std::fs::remove_dir_all(dir);
            }
        };
        let Some(mut child) = self.child.take() else {
            return remove();
        };
        if !matches!(child.try_wait(), Ok(None)) {
            return remove();
        }
        terminate(&child);
        // Drop cannot wait, so a thread sees the launcher out and then removes the folder.
        let reaper = std::thread::Builder::new()
            .name("hotend-job-reaper".to_owned())
            .spawn(move || {
                // Where there is no SIGTERM the launcher is killed at once.
                let grace = if cfg!(unix) {
                    STOP_GRACE
                } else {
                    Duration::ZERO
                };
                let deadline = Instant::now() + grace;
                let mut killed = false;
                loop {
                    match child.try_wait() {
                        Ok(None) if Instant::now() < deadline => {}
                        Ok(None) if !killed => {
                            let _ = child.start_kill();
                            killed = true;
                        }
                        Ok(None) if Instant::now() < deadline + STOP_GRACE => {}
                        _ => break,
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                remove();
            });
        // Without a thread the folder is left for the sweep at the next start; the launcher
        // has its SIGTERM either way.
        drop(reaper);
    }
}

/// Asks a launcher to end its process tree and exit.
fn terminate(child: &Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id().and_then(|pid| libc::pid_t::try_from(pid).ok()) {
        // SAFETY: `kill` has no memory-safety preconditions. The pid belongs to a child that
        // has not been waited for yet, so it cannot have been reused.
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
    #[cfg(not(unix))]
    let _ = child;
}

/// What the audit log says about how a job was confined.
pub(crate) fn sandbox_detail() -> serde_json::Value {
    match hotend_sandbox::confinement() {
        Ok(hotend_sandbox::Confinement::Sandboxed {
            launcher,
            launcher_overridden,
        }) => json!({
            "mode": "on",
            "launcher": launcher.to_string_lossy(),
            "launcher_overridden": launcher_overridden,
        }),
        Ok(hotend_sandbox::Confinement::Unconfined) => json!({"mode": "off"}),
        Err(_) => json!({"mode": "unavailable"}),
    }
}

/// Tunables of the job runner.
#[derive(Debug, Clone)]
pub struct JobConfig {
    /// Memory cap of the worker in megabytes (address space per process and resident memory
    /// of the tree). A build with four views peaks near 1.1 GB.
    pub memory_mb: u64,
    /// Seconds granted beyond the request's wall-clock limit before the core kills the
    /// launcher itself.
    pub grace_seconds: u64,
    /// Bytes kept of the end of stdout and of stderr.
    pub max_log_bytes: usize,
    /// Engine jobs allowed to run at the same time.
    pub max_concurrent: usize,
}

impl Default for JobConfig {
    fn default() -> Self {
        Self {
            memory_mb: 4096,
            grace_seconds: 10,
            max_log_bytes: 32 * 1024,
            max_concurrent: 2,
        }
    }
}

/// A job whose worker exited normally and whose result passed validation.
///
/// The result's `status` may still be `model_error` or `check_error`. Dropping the value
/// deletes the job folder.
#[derive(Debug)]
pub struct FinishedJob {
    /// The job id (also the folder name).
    pub id: String,
    /// The validated result.
    pub result: EngineResult,
    /// How long the worker ran.
    pub duration: Duration,
    dir: PathBuf,
}

impl FinishedJob {
    /// Copies a reported artefact out of the job folder to `destination`. Returns the size
    /// and SHA-256 of the copy.
    pub fn copy_artifact(
        &self,
        relative: &str,
        destination: &Path,
        max_bytes: u64,
    ) -> Result<(u64, String), JobError> {
        Ok(import::copy_capped(
            &self.dir,
            relative,
            destination,
            max_bytes,
        )?)
    }

    /// Reads a reported artefact into memory.
    pub fn read_artifact(&self, relative: &str, max_bytes: u64) -> Result<Vec<u8>, JobError> {
        Ok(import::read_capped(&self.dir, relative, max_bytes)?)
    }
}

impl Drop for FinishedJob {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Runs engine jobs. Cheap to share behind an `Arc`.
#[derive(Debug)]
pub struct JobRunner {
    data: DataDir,
    runtime: PythonRuntime,
    store: Arc<Store>,
    config: JobConfig,
    slots: Semaphore,
    notices: broadcast::Sender<JobNotice>,
}

impl JobRunner {
    /// A runner that keeps job folders under `data` and audits to `store`.
    pub fn new(
        data: DataDir,
        runtime: PythonRuntime,
        store: Arc<Store>,
        config: JobConfig,
    ) -> Self {
        let slots = Semaphore::new(config.max_concurrent.max(1));
        Self {
            data,
            runtime,
            store,
            config,
            slots,
            notices: broadcast::channel(64).0,
        }
    }

    /// The Python runtime jobs run in.
    pub fn runtime(&self) -> &PythonRuntime {
        &self.runtime
    }

    /// Where notices for people go (a restarted worker); subscribe to read them.
    pub fn notices(&self) -> broadcast::Sender<JobNotice> {
        self.notices.clone()
    }

    /// Runs one request to completion.
    ///
    /// `files` are written into the new job folder first; their names must be plain file
    /// names. The returned job owns the folder until it is dropped.
    ///
    /// A worker that crashes is started once more, in a new job folder with the same inputs
    /// and a [`RESTART_FILE`]; the person is told through a [`JobNotice`] either way. A
    /// second crash is the job's error.
    pub async fn run(
        &self,
        scope: &JobScope,
        request: &EngineRequest,
        files: Vec<(&str, JobFile)>,
    ) -> Result<FinishedJob, JobError> {
        let (error, crash) = match self.attempt(scope, request, &files, None).await {
            Ok(job) => return Ok(job),
            Err((error, None)) => {
                self.notify_limit(scope, request, &error);
                return Err(error);
            }
            Err((error, Some(crash))) => (error, crash),
        };
        let kept = crash
            .kept
            .as_deref()
            .and_then(Path::file_name)
            .map(|name| {
                format!(
                    " Its details were kept in crashes/{}.",
                    name.to_string_lossy()
                )
            })
            .unwrap_or_default();
        self.notify(
            scope,
            format!(
                "[Hotend: the modelling engine crashed while running `{}` ({}) and was \
                 restarted once.{kept}]",
                crash.op,
                crash.summary()
            ),
        );
        drop(error);
        match self.attempt(scope, request, &files, Some(&crash)).await {
            Ok(job) => Ok(job),
            Err((error, Some(again))) => {
                self.notify(
                    scope,
                    format!(
                        "[Hotend: the modelling engine crashed again on the restart ({}), \
                         so this step failed. Trying it once more later may work; if it keeps \
                         happening, the kept crash folders say why.]",
                        again.summary()
                    ),
                );
                Err(error)
            }
            Err((error, None)) => {
                self.notify_limit(scope, request, &error);
                Err(error)
            }
        }
    }

    /// Tells the person that a job was stopped at a sandbox limit (and not restarted).
    fn notify_limit(&self, scope: &JobScope, request: &EngineRequest, error: &JobError) {
        if let JobError::LimitReached { reason } = error {
            self.notify(
                scope,
                format!(
                    "[Hotend: the modelling engine was stopped at its sandbox limit while \
                     running `{}` ({reason}); it was not restarted.]",
                    request.op()
                ),
            );
        }
    }

    fn notify(&self, scope: &JobScope, text: String) {
        // Nobody listening is not an error: the audit entry has the facts.
        let _ = self.notices.send(JobNotice {
            project_id: scope.project_id.clone(),
            run_id: scope.run_id.clone(),
            text,
        });
    }

    /// One run of the worker. A crash comes back with its report; its folder is kept.
    async fn attempt(
        &self,
        scope: &JobScope,
        request: &EngineRequest,
        files: &[(&str, JobFile)],
        restart_of: Option<&CrashReport>,
    ) -> Result<FinishedJob, (JobError, Option<CrashReport>)> {
        let id = new_id();
        let dir = self
            .data
            .job_dir(&id)
            .map_err(|e| (JobError::from(e), None))?;
        let started = Instant::now();
        // Declared before the guard, so it is dropped after it: when this future is dropped
        // half-way, the launcher gets its SIGTERM first and the audit entry is written
        // next, with the outcome `abandoned`.
        let mut audit = PendingAudit::new(
            &self.store,
            NewAudit {
                project_id: scope.project_id.clone(),
                run_id: scope.run_id.clone(),
                kind: "job",
                name: request.op().to_owned(),
                args_sha256: request.to_json().ok().map(|v| crate::util::json_hash(&v)),
                outcome: String::new(),
                detail: json!({"job_id": id, "sandbox": sandbox_detail()}),
            },
        );
        let mut guard = JobGuard::new(dir.clone());
        let mut facts = RunFacts::default();
        let outcome = self
            .run_in(&mut guard, scope, request, files, restart_of, &mut facts)
            .await;
        let duration = started.elapsed();
        let crash = match &outcome {
            Err(JobError::Crashed { exit, stderr_tail }) => {
                let mut report = self.crash_report(
                    &id,
                    request.op(),
                    if restart_of.is_some() { 2 } else { 1 },
                    exit,
                    stderr_tail,
                    &facts,
                    duration,
                );
                // The worker has exited. Its folder is removed; what the core gave it is
                // kept for study.
                if let Some(dir) = guard.dir.take() {
                    let crashes = self.data.root().join("crashes");
                    keep_crash_of(&dir, &crashes, &mut report, request, files, restart_of);
                }
                Some(report)
            }
            _ => None,
        };
        if let Some(previous) = restart_of {
            audit.detail()["restart_of"] = json!(previous.job_id);
        }
        match &outcome {
            Ok(result) => {
                audit.detail()["status"] = json!(result.status.as_str());
                audit.detail()["timings"] = json!(result.timings);
                audit.finish("ok");
            }
            Err(error) => {
                audit.detail()["error"] = json!(truncate(&error.to_string(), 2000));
                if let Some(report) = &crash {
                    audit.detail()["crash"] = json!(report);
                }
                audit.finish(error.code());
            }
        }
        match outcome {
            Ok(result) => {
                // The worker has exited; the folder now belongs to the finished job.
                guard.keep_dir();
                Ok(FinishedJob {
                    id,
                    result,
                    duration,
                    dir,
                })
            }
            // Dropping the guard removes the folder (unless a crash kept it).
            Err(error) => Err((error, crash)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn crash_report(
        &self,
        id: &str,
        op: &str,
        attempt: u32,
        exit: &str,
        stderr_tail: &str,
        facts: &RunFacts,
        duration: Duration,
    ) -> CrashReport {
        let code = facts.status.and_then(|status| status.code());
        #[cfg(unix)]
        let (signal, core_dumped) = {
            use std::os::unix::process::ExitStatusExt;
            (
                facts.status.and_then(|status| status.signal()),
                facts.status.is_some_and(|status| status.core_dumped()),
            )
        };
        #[cfg(not(unix))]
        let (signal, core_dumped) = (None, false);
        // The launcher and the worker pass on a signal that ended their child as 128 + N.
        let worker_signal = code
            .filter(|code| (129..=128 + 64).contains(code))
            .map(|c| c - 128);
        let mb = |kb: Option<u64>| kb.map(|kb| (kb as f64 / 1024.0 * 10.0).round() / 10.0);
        CrashReport {
            job_id: id.to_owned(),
            op: op.to_owned(),
            attempt,
            exit: exit.to_owned(),
            exit_code: code,
            signal,
            worker_signal,
            worker_signal_name: worker_signal.and_then(signal_name),
            core_dumped,
            elapsed_ms: u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
            peak_tree_rss_mb: mb(facts.peak_tree_rss_kb),
            peak_process_rss_mb: mb(facts.peak_process_rss_kb),
            memory_limit_mb: self.config.memory_mb,
            wall_limit_s: facts.wall_seconds,
            stderr_tail: stderr_tail.to_owned(),
            kept: None,
            left_out: Vec::new(),
        }
    }

    async fn run_in(
        &self,
        guard: &mut JobGuard,
        scope: &JobScope,
        request: &EngineRequest,
        files: &[(&str, JobFile)],
        restart_of: Option<&CrashReport>,
        facts: &mut RunFacts,
    ) -> Result<EngineResult, JobError> {
        let dir = guard.dir.clone().unwrap_or_default();
        let dir = dir.as_path();
        let io = |path: &Path| {
            let path = path.to_path_buf();
            move |source| JobError::Io { path, source }
        };
        std::fs::create_dir(dir).map_err(io(dir))?;
        for &(name, ref file) in files {
            let plain = Path::new(name).file_name().is_some_and(|f| f == name);
            if !plain || name == RESTART_FILE {
                return Err(JobError::Io {
                    path: dir.join(name),
                    source: std::io::Error::other("job input names must be plain file names"),
                });
            }
            let target = dir.join(name);
            match file {
                JobFile::Bytes(bytes) => std::fs::write(&target, bytes).map_err(io(&target))?,
                JobFile::Copy(source) => {
                    let size = std::fs::metadata(source).map_err(io(source))?.len();
                    if size > MAX_INPUT_BYTES {
                        return Err(JobError::Io {
                            path: source.clone(),
                            source: std::io::Error::other("job input file is too large"),
                        });
                    }
                    std::fs::copy(source, &target).map_err(io(&target))?;
                }
            }
        }
        if let Some(previous) = restart_of {
            // Tells the worker (and whoever studies the folder) that this is the restart.
            let target = dir.join(RESTART_FILE);
            let note = json!({"attempt": 2, "previous_job_id": previous.job_id,
                              "previous_exit": previous.summary()});
            std::fs::write(&target, note.to_string()).map_err(io(&target))?;
        }
        let request_json = request.to_json().map_err(|e| JobError::Io {
            path: dir.join("request.json"),
            source: std::io::Error::other(e),
        })?;
        let request_path = dir.join("request.json");
        std::fs::write(&request_path, request_json.to_string()).map_err(io(&request_path))?;

        let slot = tokio::select! {
            slot = self.slots.acquire() => slot,
            () = scope.cancelled() => return Err(JobError::Cancelled),
        };
        let _slot = slot.map_err(|_| JobError::Io {
            path: dir.to_path_buf(),
            source: std::io::Error::other("the job runner is shut down"),
        })?;
        let wall_seconds = request.wall_seconds().clamp(1, 3600);
        facts.wall_seconds = wall_seconds;
        let mut policy = Policy::new(Profile::Engine, dir);
        policy.limits = Limits {
            wall_seconds,
            memory_mb: self.config.memory_mb,
            ..Profile::Engine.default_limits()
        };
        for path in &self.runtime.read_only {
            policy = policy.read_only(path);
        }
        let mut command = tokio::process::Command::from(hotend_sandbox::command(
            &policy,
            &self.runtime.python,
            [
                "-I".as_ref(),
                "-m".as_ref(),
                "hotend_engine.worker".as_ref(),
                dir.as_os_str(),
            ],
        )?);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = guard.attach(command.spawn().map_err(JobError::Spawn)?);
        let cap = self.config.max_log_bytes;
        let stdout = tokio::spawn(read_tail(child.stdout.take(), cap));
        let stderr = tokio::spawn(read_tail(child.stderr.take(), cap));
        let memory = MemoryPeak::watch(child.id());

        let limit = Duration::from_secs(wall_seconds + self.config.grace_seconds);
        let waited = tokio::select! {
            waited = tokio::time::timeout(limit, child.wait()) => Some(waited),
            () = scope.cancelled() => None,
        };
        (facts.peak_tree_rss_kb, facts.peak_process_rss_kb) = memory.finish();
        if let Some(Ok(Ok(status))) = &waited {
            facts.status = Some(*status);
        }
        let status = match waited {
            Some(Ok(status)) => status.map_err(JobError::Spawn)?,
            Some(Err(_)) => {
                guard.stop().await;
                return Err(JobError::Timeout {
                    seconds: wall_seconds,
                });
            }
            None => {
                guard.stop().await;
                return Err(JobError::Cancelled);
            }
        };
        // The launcher has killed every process of the job before it exits, so the pipes
        // close; the timeout only guards against a descriptor that leaked elsewhere.
        let logs = async {
            (
                stdout.await.unwrap_or_default(),
                stderr.await.unwrap_or_default(),
            )
        };
        let (_stdout_tail, stderr_tail) = tokio::time::timeout(Duration::from_secs(5), logs)
            .await
            .unwrap_or_default();
        let stderr_tail = truncate(stderr_tail.trim(), 2000);

        match status.code() {
            Some(0) => {}
            Some(EXIT_BAD_REQUEST) => {
                // The worker explains a refused request in result.json when it can.
                let message = match read_result(dir, request.op()) {
                    Ok(result) => result.error.map(|e| e.message).unwrap_or_default(),
                    Err(_) => stderr_tail,
                };
                return Err(JobError::BadRequest { message });
            }
            Some(EXIT_WALL_CLOCK) => {
                return Err(JobError::Timeout {
                    seconds: wall_seconds,
                });
            }
            Some(EXIT_SANDBOX_FAILED) => {
                return Err(JobError::SandboxRefused {
                    detail: stderr_tail,
                });
            }
            Some(EXIT_LIMIT) => {
                return Err(JobError::LimitReached {
                    reason: limit_reason(&stderr_tail),
                });
            }
            _ => {
                return Err(JobError::Crashed {
                    exit: status.to_string(),
                    stderr_tail,
                });
            }
        }
        let result = read_result(dir, request.op())?;
        match result.status {
            EngineStatus::Ok | EngineStatus::ModelError | EngineStatus::CheckError => Ok(result),
            EngineStatus::Timeout => Err(JobError::Timeout {
                seconds: wall_seconds,
            }),
            EngineStatus::InternalError => {
                let error = result.error.unwrap_or(crate::engine::EngineError {
                    kind: "unknown".into(),
                    message: String::new(),
                    line: None,
                    traceback: String::new(),
                });
                Err(JobError::EngineInternal {
                    kind: error.kind,
                    message: error.message,
                })
            }
        }
    }
}

/// The limit the launcher names on stderr (`hotend-sandbox: memory limit of 300 MB reached
/// ...; the process tree was killed`), cleaned to one short line. The worker can write to
/// stderr too, so only the launcher's last line counts and nothing else is taken from it.
fn limit_reason(stderr_tail: &str) -> String {
    stderr_tail
        .lines()
        .rev()
        .filter_map(|line| line.trim().strip_prefix("hotend-sandbox: "))
        .find(|line| line.contains("limit"))
        .map(|line| {
            let line = line
                .split("; the process tree was killed")
                .next()
                .unwrap_or(line);
            let clean: String = line
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            truncate(clean.trim(), 160)
        })
        .filter(|line| !line.is_empty())
        .unwrap_or_else(|| "a process, memory or disk limit".to_owned())
}

/// Removes a crashed job's folder (nothing in it is opened or followed: the script may have
/// left links and FIFOs there) and keeps the crash with [`keep_crash`].
fn keep_crash_of(
    dir: &Path,
    crashes: &Path,
    report: &mut CrashReport,
    request: &EngineRequest,
    files: &[(&str, JobFile)],
    restart_of: Option<&CrashReport>,
) {
    let _ = std::fs::remove_dir_all(dir);
    keep_crash(crashes, report, request, files, restart_of);
}

/// Keeps a crash for study in `crashes/<job-id>/`, a folder created new: the core's own
/// inputs of the job (from what it holds, never from the job folder) and `crash.json`, every
/// file created new. Then removes the oldest crash folders beyond [`MAX_KEPT_CRASHES`].
/// Sets `report.kept` (and `left_out`); leaves `kept` unset, and nothing behind, when the
/// crash could not be kept.
fn keep_crash(
    crashes: &Path,
    report: &mut CrashReport,
    request: &EngineRequest,
    files: &[(&str, JobFile)],
    restart_of: Option<&CrashReport>,
) {
    let target = crashes.join(&report.job_id);
    let create = |name: &str, bytes: &[u8]| -> std::io::Result<()> {
        use std::io::Write;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target.join(name))?
            .write_all(bytes)
    };
    let mut left_out = Vec::new();
    let kept = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(crashes)?;
        std::fs::create_dir(&target)?;
        let request_json = request.to_json().map_err(std::io::Error::other)?;
        create("request.json", request_json.to_string().as_bytes())?;
        if let Some(previous) = restart_of {
            let note = json!({"attempt": 2, "previous_job_id": previous.job_id,
                              "previous_exit": previous.summary()});
            create(RESTART_FILE, note.to_string().as_bytes())?;
        }
        let mut total = 0u64;
        for &(name, ref file) in files {
            let plain = Path::new(name).file_name().is_some_and(|f| f == name);
            if !plain || matches!(name, "request.json" | "crash.json" | RESTART_FILE) {
                continue;
            }
            let bytes = match file {
                JobFile::Bytes(bytes) => Some(bytes.clone()),
                // Files the core owns; read only when small enough to keep.
                JobFile::Copy(source) => std::fs::metadata(source)
                    .ok()
                    .filter(|meta| meta.is_file() && meta.len() <= MAX_KEPT_INPUT_BYTES)
                    .and_then(|_| std::fs::read(source).ok()),
            };
            match bytes {
                Some(bytes)
                    if bytes.len() as u64 <= MAX_KEPT_INPUT_BYTES
                        && total + bytes.len() as u64 <= MAX_KEPT_CRASH_BYTES =>
                {
                    total += bytes.len() as u64;
                    create(name, &bytes)?;
                }
                _ => left_out.push(name.to_owned()),
            }
        }
        let mut written = report.clone();
        written.kept = Some(target.clone());
        written.left_out = left_out.clone();
        let text = serde_json::to_string_pretty(&written).map_err(std::io::Error::other)?;
        create("crash.json", text.as_bytes())
    })();
    if kept.is_err() {
        let _ = std::fs::remove_dir_all(&target);
        return;
    }
    report.kept = Some(target);
    report.left_out = left_out;
    prune_crashes(crashes, MAX_KEPT_CRASHES);
}

/// Removes the oldest entries of `crashes` beyond `keep`. Job ids sort by time, so the
/// oldest names come first. Nothing is followed: a link is removed, not its target.
fn prune_crashes(crashes: &Path, keep: usize) {
    if let Ok(entries) = std::fs::read_dir(crashes) {
        let mut names: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        names.sort();
        let surplus = names.len().saturating_sub(keep);
        for old in names.into_iter().take(surplus) {
            match std::fs::symlink_metadata(&old) {
                Ok(meta) if meta.is_dir() => {
                    let _ = std::fs::remove_dir_all(&old);
                }
                _ => {
                    let _ = std::fs::remove_file(&old);
                }
            }
        }
    }
}

/// Reads, parses and sanitises `result.json` of a job folder.
fn read_result(dir: &Path, op: &str) -> Result<EngineResult, JobError> {
    let malformed = |reason: String| JobError::MalformedResult { reason };
    let bytes = import::read_capped(dir, "result.json", MAX_RESULT_BYTES)
        .map_err(|e| malformed(e.to_string()))?;
    let result: EngineResult =
        serde_json::from_slice(&bytes).map_err(|e| malformed(truncate(&e.to_string(), 300)))?;
    result.sanitize(op).map_err(|e| malformed(e.0))
}

/// Drains a pipe to its end and returns the last `cap` bytes as text, so a chatty child
/// can neither block on a full pipe nor grow the core's memory.
async fn read_tail(pipe: Option<impl AsyncRead + Unpin>, cap: usize) -> String {
    let Some(mut pipe) = pipe else {
        return String::new();
    };
    let mut tail: Vec<u8> = Vec::new();
    let mut buffer = vec![0u8; 8192];
    loop {
        match pipe.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                tail.extend_from_slice(&buffer[..read]);
                if tail.len() > cap * 2 {
                    let cut = tail.len() - cap;
                    tail.drain(..cut);
                }
            }
        }
    }
    if tail.len() > cap {
        let cut = tail.len() - cap;
        tail.drain(..cut);
    }
    String::from_utf8_lossy(&tail).into_owned()
}

/// Samples the resident memory of a job's process tree while it runs, for crash reports:
/// the largest sum over the tree, and the largest high-water mark of one process. Reads
/// `/proc` and so works on Linux only; elsewhere both stay unknown.
struct MemoryPeak {
    peaks: Arc<std::sync::Mutex<(Option<u64>, Option<u64>)>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl MemoryPeak {
    fn watch(pid: Option<u32>) -> Self {
        let peaks = Arc::new(std::sync::Mutex::new((None, None)));
        let task = pid.filter(|_| cfg!(target_os = "linux")).map(|pid| {
            let peaks = Arc::clone(&peaks);
            tokio::spawn(async move {
                loop {
                    if let Some((tree, process)) = sample_tree(pid) {
                        let mut peaks = peaks.lock().unwrap_or_else(|p| p.into_inner());
                        peaks.0 = Some(peaks.0.unwrap_or(0).max(tree));
                        peaks.1 = Some(peaks.1.unwrap_or(0).max(process));
                    }
                    tokio::time::sleep(MEMORY_SAMPLE_INTERVAL).await;
                }
            })
        });
        Self { peaks, task }
    }

    /// Stops sampling; (largest tree sum, largest process high-water mark) in kB.
    fn finish(mut self) -> (Option<u64>, Option<u64>) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        *self.peaks.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl Drop for MemoryPeak {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// (sum of VmRSS, largest VmHWM) in kB over `pid` and its descendants, from `/proc`.
fn sample_tree(pid: u32) -> Option<(u64, u64)> {
    let field = |status: &str, name: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|kb| kb.parse::<u64>().ok())
    };
    let mut pending = vec![pid];
    let mut seen = 0usize;
    let (mut tree, mut process) = (0u64, 0u64);
    let mut any = false;
    while let Some(pid) = pending.pop() {
        seen += 1;
        if seen > 4096 {
            break;
        }
        let proc_dir = PathBuf::from(format!("/proc/{pid}"));
        if let Ok(status) = std::fs::read_to_string(proc_dir.join("status")) {
            any = true;
            tree += field(&status, "VmRSS:").unwrap_or(0);
            process = process.max(field(&status, "VmHWM:").unwrap_or(0));
        }
        let Ok(tasks) = std::fs::read_dir(proc_dir.join("task")) else {
            continue;
        };
        for task in tasks.flatten() {
            if let Ok(children) = std::fs::read_to_string(task.path().join("children")) {
                pending.extend(
                    children
                        .split_whitespace()
                        .filter_map(|c| c.parse::<u32>().ok()),
                );
            }
        }
    }
    any.then_some((tree, process))
}

/// Stops a launcher: SIGTERM first, so it kills the whole process tree of the job, then a
/// hard kill if it has not gone within the grace period.
pub(crate) async fn stop(child: &mut Child) {
    if cfg!(unix) && child.id().is_some() {
        terminate(child);
        if tokio::time::timeout(STOP_GRACE, child.wait()).await.is_ok() {
            return;
        }
    }
    let _ = child.kill().await;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[tokio::test]
    async fn only_the_end_of_a_long_log_is_kept() {
        let data: Vec<u8> = (0..100_000u32)
            .flat_map(|i| format!("{i}\n").into_bytes())
            .collect();
        let tail = read_tail(Some(&data[..]), 1000).await;
        assert!(tail.len() <= 1000);
        assert!(tail.ends_with("99999\n"));
        assert_eq!(read_tail(None::<&[u8]>, 10).await, "");
    }

    #[test]
    fn a_missing_oversized_or_malformed_result_is_a_typed_error() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = read_result(tmp.path(), "build").unwrap_err();
        assert!(
            matches!(missing, JobError::MalformedResult { .. }),
            "{missing}"
        );

        let path = tmp.path().join("result.json");
        for bad in [
            "not json".to_owned(),
            "{}".to_owned(),
            r#"{"protocol":0,"op":"build","status":"ok","error":null,"extra":1}"#.to_owned(),
            r#"{"protocol":0,"op":"docs","status":"ok","error":null}"#.to_owned(),
            format!(
                r#"{{"protocol":0,"op":"build","status":"ok","error":null,"stdout":"{}"}}"#,
                "x".repeat(MAX_RESULT_BYTES as usize)
            ),
        ] {
            std::fs::write(&path, bad).unwrap();
            let error = read_result(tmp.path(), "build").unwrap_err();
            assert_eq!(error.code(), "malformed_result");
            assert!(error.to_string().len() < 600, "error text must stay short");
        }
        std::fs::write(
            &path,
            r#"{"protocol":0,"op":"build","status":"ok","error":null}"#,
        )
        .unwrap();
        assert!(read_result(tmp.path(), "build").is_ok());
    }

    fn report(job_id: &str) -> CrashReport {
        CrashReport {
            job_id: job_id.to_owned(),
            op: "build".into(),
            attempt: 1,
            exit: "exit status: 134".into(),
            exit_code: Some(134),
            signal: None,
            worker_signal: Some(6),
            worker_signal_name: Some("SIGABRT"),
            core_dumped: false,
            elapsed_ms: 1,
            peak_tree_rss_mb: None,
            peak_process_rss_mb: None,
            memory_limit_mb: 4096,
            wall_limit_s: 120,
            stderr_tail: "boom".into(),
            kept: None,
            left_out: Vec::new(),
        }
    }

    fn build() -> EngineRequest {
        EngineRequest::Build(crate::engine::BuildRequest {
            model_path: "model.py".into(),
            ..Default::default()
        })
    }

    #[test]
    fn eleven_crashes_keep_the_newest_ten_and_only_small_core_inputs() {
        let tmp = tempfile::tempdir().unwrap();
        let crashes = tmp.path().join("crashes");
        let big = vec![b'x'; (MAX_KEPT_INPUT_BYTES + 1) as usize];
        let mut ids = Vec::new();
        for _ in 0..11 {
            let mut crash = report(&new_id());
            let files = vec![
                ("model.py", JobFile::Bytes(b"print(1)".to_vec())),
                ("big.step", JobFile::Bytes(big.clone())),
            ];
            keep_crash(&crashes, &mut crash, &build(), &files, None);
            let kept = crash.kept.clone().expect("kept");
            assert_eq!(crash.left_out, ["big.step"]);
            assert!(kept.join("model.py").is_file() && kept.join("request.json").is_file());
            assert!(!kept.join("big.step").exists(), "large inputs are not kept");
            let written: serde_json::Value =
                serde_json::from_slice(&std::fs::read(kept.join("crash.json")).unwrap()).unwrap();
            assert_eq!(written["left_out"][0], "big.step");
            ids.push(crash.job_id);
        }
        let mut left: Vec<String> = std::fs::read_dir(&crashes)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left.len(), MAX_KEPT_CRASHES);
        assert_eq!(left, ids[1..], "the oldest crash went");
        let total: u64 = walk_size(&crashes);
        assert!(
            total <= MAX_KEPT_CRASHES as u64 * (MAX_KEPT_CRASH_BYTES + 64 * 1024),
            "{total}"
        );
    }

    fn walk_size(dir: &Path) -> u64 {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                let meta = e.metadata().unwrap();
                if meta.is_dir() {
                    walk_size(&e.path())
                } else {
                    meta.len()
                }
            })
            .sum()
    }

    /// The second layer under the sandbox (which refuses links and FIFOs in a job folder on
    /// Linux): a crashed folder holding a link named `crash.json` to a file outside, and a
    /// FIFO named like a kept file, is neither written through nor opened.
    #[cfg(unix)]
    #[test]
    fn keeping_a_crash_never_follows_links_or_opens_fifos_left_in_the_job_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let crashes = tmp.path().join("crashes");
        let outside = tmp.path().join("outside.txt");
        std::fs::write(&outside, "the person's file").unwrap();
        for planted in ["crash.json", "request.json", "model.py", "restart.json"] {
            let job = tmp.path().join(format!("job-{planted}"));
            std::fs::create_dir(&job).unwrap();
            std::os::unix::fs::symlink(&outside, job.join(planted)).unwrap();
            let fifo = job.join("fifo");
            let name = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
            // SAFETY: a valid C string; mkfifo only creates the node.
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            let mut crash = report(&new_id());
            let files = vec![("model.py", JobFile::Bytes(b"print(1)".to_vec()))];
            let previous = report(&new_id());
            // A thread, so a FIFO that was opened would show as a hang, not block the test.
            let (done, finished) = std::sync::mpsc::channel();
            let (job2, crashes2) = (job.clone(), crashes.clone());
            std::thread::spawn(move || {
                keep_crash_of(
                    &job2,
                    &crashes2,
                    &mut crash,
                    &build(),
                    &files,
                    Some(&previous),
                );
                let _ = done.send(crash);
            });
            let crash = finished
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("keeping a crash must not hang on a FIFO");
            assert_eq!(
                std::fs::read_to_string(&outside).unwrap(),
                "the person's file"
            );
            let kept = crash.kept.expect("kept");
            for entry in std::fs::read_dir(&kept).unwrap() {
                let meta = std::fs::symlink_metadata(entry.unwrap().path()).unwrap();
                assert!(meta.is_file(), "only plain files the core made");
            }
            assert!(kept.join("crash.json").is_file() && kept.join("restart.json").is_file());
            assert!(!job.exists(), "the crashed job folder is removed");
        }
    }

    #[test]
    fn the_inputs_of_a_crash_are_kept_within_the_total_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let crashes = tmp.path().join("crashes");
        let part = vec![b'x'; MAX_KEPT_INPUT_BYTES as usize];
        let names = ["a", "b", "c", "d", "e", "f"];
        let files: Vec<(&str, JobFile)> = names
            .iter()
            .map(|name| (*name, JobFile::Bytes(part.clone())))
            .collect();
        let mut crash = report(&new_id());
        keep_crash(&crashes, &mut crash, &build(), &files, None);
        assert_eq!(crash.left_out, ["e", "f"]);
        assert!(walk_size(&crashes) <= MAX_KEPT_CRASH_BYTES + 64 * 1024);
    }

    #[test]
    fn only_the_launchers_limit_line_names_the_limit() {
        assert_eq!(
            limit_reason(
                "worker said: memory limit of 1 MB\nhotend-sandbox: process limit of 512 \
                 reached (512 tasks); the process tree was killed\n"
            ),
            "process limit of 512 reached (512 tasks)"
        );
        // The worker cannot put anything but a launcher-looking line in, and that is cut
        // to one short clean line.
        let spoof = format!("hotend-sandbox: disk limit \u{1b}[2J{}", "y".repeat(500));
        let reason = limit_reason(&spoof);
        assert!(
            reason.chars().count() <= 200 && !reason.contains('\u{1b}'),
            "{reason}"
        );
        assert_eq!(limit_reason("nothing"), "a process, memory or disk limit");
        let error = JobError::LimitReached {
            reason: "memory limit of 300 MB reached by the process tree".into(),
        };
        assert_eq!(error.code(), "limit_reached");
        assert!(error.to_string().contains("not restarted"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn a_result_that_is_a_symlink_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("elsewhere.json");
        std::fs::write(
            &real,
            r#"{"protocol":0,"op":"build","status":"ok","error":null}"#,
        )
        .unwrap();
        let job = tmp.path().join("job");
        std::fs::create_dir(&job).unwrap();
        std::os::unix::fs::symlink(&real, job.join("result.json")).unwrap();
        let error = read_result(&job, "build").unwrap_err();
        assert!(error.to_string().contains("symbolic link"), "{error}");
    }
}
