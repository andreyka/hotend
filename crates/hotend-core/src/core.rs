//! The facade: one object with an async method per UI command of PROTOCOL.md section 8, so
//! the Tauri app and the CLI stay thin. Everything a caller needs is re-exported from the
//! crate root.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use futures_util::Stream;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::agent::{
    AgentCommand, AgentError, AgentLimits, AgentSupervisor, CancelHandle, RunEvent, RunRequest,
    StopReason, text_of,
};
use crate::egress::{Egress, EgressError, EgressLimits, ProviderKind};
use crate::engine::{CardRef, EngineResult, ExportFormat, GaugeReading, Question, Section};
use crate::gateway::{
    Gateway, GatewayPolicy, SliceBackend, SliceFailure, SliceOutcome, SlicerInstallation, ToolSpec,
};
use crate::jobs::{JobConfig, JobNotice, JobRunner, JobScope};
use crate::modeling::{
    BuildReport, Calibration, CalibrationUpdate, ExportedFile, GaugeExport, Modeler, ModelingError,
    Preparation, PrepareOptions, PrintPlanView, RenderedView, VersionDetail,
};
use crate::paths::{DataDir, PathError};
use crate::printing::{
    CoreEvent, DeliveryChoice, EditableProject, PrintError, PrintJob, PrintReview, PrintService,
    PrinterInfo, PrinterOptions, ProjectDestination, ProjectReview,
};
use crate::research::{KeyUnreadable, SearchKeyState};
use crate::research::{ProjectResearch, ResearchStatus, SearchSourceOption};
use crate::runtime::{PythonRuntime, RuntimeError};
use crate::search::{SearchClient, SearchService, SearchSource, search_key_name};
use crate::secrets::{
    EnvSecretStore, FileSecretStore, KeyringSecretStore, RevealToken, Secret, SecretError,
    SecretStore, SecretStoreKind, provider_key_name,
};
use crate::settings::{SETTINGS_KEY, Settings};
use crate::snapshot::SnapshotExport;
use crate::store::Snapshot;
use crate::store::{
    AuditEntry, NewAudit, Project, ResearchLogEntry, ResearchNote, ResearchSource, Store,
    StoreError, TranscriptEntry, Version,
};
use crate::util::{new_id, now_ms, truncate};

/// A store refusal of a value (a snapshot name taken or not one plain line) as the facade's
/// `invalid_input`; every other store error as it is.
fn invalid_input(error: StoreError) -> CoreError {
    match error {
        StoreError::Invalid(message) => CoreError::InvalidInput(message),
        other => CoreError::Store(other),
    }
}

/// Opens the data folder's database. A folder that already holds projects but no database
/// is refused: a new, empty database would show none of them, and the sweep of a core that
/// believes to be alone would clean folders another program may be using.
fn open_store(data: &DataDir) -> Result<Store, CoreError> {
    let database = data.db_path();
    if std::fs::symlink_metadata(&database).is_err() {
        let projects = data.root().join("projects");
        let mut entries = std::fs::read_dir(&projects).map_err(|source| PathError::Io {
            path: projects.clone(),
            source,
        })?;
        if entries.next().is_some() {
            return Err(CoreError::Setup(format!(
                "the data folder {} holds projects but no database {}. Nothing was opened or \
                 changed. Put the database back or name another data folder. A folder that was \
                 made by an earlier version must be converted first",
                data.root().display(),
                database.file_name().unwrap_or_default().display(),
            )));
        }
    }
    Ok(Store::open(&database)?)
}

/// Environment variable that selects the secret backend: `keyring` (default), `env`
/// (read-only) or `file` (one file of the person's own, see [`FileSecretStore`]).
pub const SECRETS_ENV: &str = "HOTEND_SECRETS";
/// Longest user message.
pub const MAX_MESSAGE_BYTES: usize = 32 * 1024;
const MAX_TRANSCRIPT_TEXT: usize = 200_000;
const EVENT_BUFFER: usize = 1024;

/// Where provider keys are kept.
#[derive(Debug, Clone, Default)]
pub enum SecretBackend {
    /// The operating system keychain.
    #[default]
    Keyring,
    /// Read-only, from the providers' usual environment variables. For headless and WSL use.
    Environment,
    /// One file of the person's own (`HOTEND_SECRETS_FILE`, else
    /// `~/.config/hotend/secrets.json`); a key in the environment still wins for reading.
    /// For headless and WSL use when keys must be stored from Hotend.
    File,
    /// A store supplied by the caller (tests use `MemorySecretStore`).
    Custom(Arc<dyn SecretStore>),
}

impl SecretBackend {
    /// The backend named by `HOTEND_SECRETS`: `env` selects the environment backend,
    /// `file` the file backend, anything else the keychain.
    pub fn from_env() -> Self {
        match std::env::var(SECRETS_ENV).as_deref() {
            Ok("env") => Self::Environment,
            Ok("file") => Self::File,
            _ => Self::Keyring,
        }
    }

    /// What kind of store this backend names (without opening a keychain).
    pub fn kind(&self) -> SecretStoreKind {
        match self {
            Self::Keyring => SecretStoreKind::Keychain,
            Self::Environment => SecretStoreKind::Environment,
            Self::File => SecretStoreKind::File,
            Self::Custom(store) => store.kind(),
        }
    }

    /// The store this backend names.
    pub fn store(&self) -> Arc<dyn SecretStore> {
        match self {
            Self::Keyring => Arc::new(KeyringSecretStore::default()),
            Self::Environment => Arc::new(EnvSecretStore::default()),
            Self::File => Arc::new(FileSecretStore::from_env()),
            Self::Custom(store) => Arc::clone(store),
        }
    }
}

/// How to open the core. `Default` is the product configuration.
#[derive(Debug, Clone, Default)]
pub struct CoreConfig {
    /// Data directory; `None` uses `HOTEND_DATA_DIR` or the platform default.
    pub data_dir: Option<PathBuf>,
    pub secrets: SecretBackend,
    /// Python runtime; `None` discovers it (`HOTEND_PYTHON`, `HOTEND_PYTHON_SRC`).
    pub runtime: Option<PythonRuntime>,
    /// Agent child; `None` runs `python -I -m hotend_agent` in the runtime.
    pub agent: Option<AgentCommand>,
    pub jobs: JobConfig,
    pub gateway: GatewayPolicy,
    pub egress: EgressLimits,
    pub agent_limits: AgentLimits,
    /// How the simulated printer behaves.
    pub printer: PrinterOptions,
    /// Hotend's configuration folder, where the person's own files for reading a real
    /// printer's status live (`bambu-ca.pem`); `None` uses `HOTEND_CONFIG_DIR` or the
    /// user's configuration folder plus `hotend` ([`crate::printer_lan::config_dir`]).
    pub config_dir: Option<PathBuf>,
}

/// Why a facade call failed. Messages say what the user can do next and never hold a secret.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Modeling(#[from] ModelingError),
    #[error(transparent)]
    Path(#[from] PathError),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error(transparent)]
    Secret(#[from] SecretError),
    #[error(transparent)]
    Egress(#[from] EgressError),
    #[error(transparent)]
    Agent(#[from] AgentError),
    #[error(transparent)]
    Slice(#[from] SliceFailure),
    #[error(transparent)]
    Print(#[from] PrintError),
    /// Something must be set up first; the message says what.
    #[error("{0}")]
    NotConfigured(String),
    /// The project already has a run in progress.
    #[error("this project already has a message in progress; wait for it or cancel it")]
    Busy,
    /// An argument is not acceptable.
    #[error("{0}")]
    InvalidInput(String),
    /// A component could not be set up.
    #[error("the core could not start: {0}")]
    Setup(String),
}

/// A project with its versions and conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectDetail {
    pub project: Project,
    /// Oldest first.
    pub versions: Vec<Version>,
    /// The conversation as the user saw it, oldest first.
    pub transcript: Vec<TranscriptEntry>,
}

/// The events of one `message_send`, ending with [`RunEvent::Finished`] or
/// [`RunEvent::Failed`].
#[derive(Debug)]
pub struct MessageRun {
    /// The id to pass to [`Core::run_cancel`].
    pub run_id: String,
    events: broadcast::Receiver<RunEvent>,
    done: bool,
}

impl MessageRun {
    /// The next event, or `None` after the final one. Dropping this value does not stop the
    /// run; cancel it with [`Core::run_cancel`].
    pub async fn next(&mut self) -> Option<RunEvent> {
        if self.done {
            return None;
        }
        loop {
            match self.events.recv().await {
                Ok(event) => {
                    self.done =
                        matches!(event, RunEvent::Finished { .. } | RunEvent::Failed { .. });
                    return Some(event);
                }
                // A slow reader loses old events, never the final one.
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => {
                    self.done = true;
                    return None;
                }
            }
        }
    }

    /// The same events as a `Stream`.
    pub fn into_stream(self) -> impl Stream<Item = RunEvent> {
        futures_util::stream::unfold(self, |mut run| async move {
            run.next().await.map(|event| (event, run))
        })
    }
}

struct ActiveRun {
    project_id: String,
    cancel: CancelHandle,
    task: JoinHandle<()>,
}

struct Inner {
    /// Held for as long as this core is open (see [`crate::sweep`]).
    _data_lock: Option<crate::sweep::DataLock>,
    data: DataDir,
    store: Arc<Store>,
    modeler: Arc<Modeler>,
    gateway: Arc<Gateway>,
    egress: Arc<Egress>,
    supervisor: AgentSupervisor,
    secrets: Arc<dyn SecretStore>,
    prints: PrintService,
    /// Reading a real printer's status over the local network (read-only).
    lan: Arc<crate::printer_lan::LanService>,
    /// Everything the core reports while it works: agent runs and print jobs.
    events: broadcast::Sender<CoreEvent>,
    /// What the job runner tells people (a restarted engine worker).
    job_notices: broadcast::Sender<JobNotice>,
    runs: Mutex<HashMap<String, ActiveRun>>,
    /// Projects whose next run is being started (between the busy check and the run being
    /// listed in `runs`). Taken under the `runs` lock, so two messages to one project can
    /// never both start a run: they would share a conversation and plan their web research
    /// from the same counters.
    starting: Mutex<std::collections::HashSet<String>>,
    /// The stand-in search service of a test.
    #[cfg(test)]
    search_base_for_tests: Mutex<Option<String>>,
}

impl Inner {
    fn runs(&self) -> std::sync::MutexGuard<'_, HashMap<String, ActiveRun>> {
        self.runs.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn starting(&self) -> std::sync::MutexGuard<'_, std::collections::HashSet<String>> {
        self.starting.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Reserves the start of a run for `project_id`, or says the project is busy: a run of
    /// it is in progress or being started. The reservation ends when the guard is dropped,
    /// on every path; by then a started run is listed in `runs`.
    fn reserve_run(&self, project_id: &str) -> Result<RunReservation<'_>, CoreError> {
        let runs = self.runs();
        let mut starting = self.starting();
        if starting.contains(project_id) || runs.values().any(|run| run.project_id == project_id) {
            return Err(CoreError::Busy);
        }
        starting.insert(project_id.to_owned());
        Ok(RunReservation {
            inner: self,
            project_id: project_id.to_owned(),
        })
    }
}

/// A project's run being started; see [`Inner::reserve_run`].
struct RunReservation<'a> {
    inner: &'a Inner,
    project_id: String,
}

impl Drop for RunReservation<'_> {
    fn drop(&mut self) {
        self.inner.starting().remove(&self.project_id);
    }
}

/// Hotend's core. Cloning is cheap and gives another handle to the same core; when the
/// last handle is dropped every running agent is stopped.
#[derive(Clone)]
pub struct Core {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Core {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Core")
            .field("data_dir", &self.inner.data.root())
            .finish_non_exhaustive()
    }
}

/// A handle that does not keep the core alive. For components the core itself owns and that
/// call back into it (a slicer backend that asks the core for a mesh export): holding a
/// [`Core`] there would be a reference cycle, and the agents would never be stopped.
#[derive(Clone)]
pub struct WeakCore {
    inner: Weak<Inner>,
}

impl WeakCore {
    /// The core, while a [`Core`] handle is still alive.
    pub fn upgrade(&self) -> Option<Core> {
        self.inner.upgrade().map(|inner| Core { inner })
    }
}

impl std::fmt::Debug for WeakCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WeakCore").finish_non_exhaustive()
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        for (_, run) in self.runs().drain() {
            // Aborting the bookkeeping task drops its agent handle, which stops the child.
            run.task.abort();
        }
    }
}

impl Core {
    /// Opens the data directory, the store and the Python runtime and wires the components.
    pub async fn open(config: CoreConfig) -> Result<Self, CoreError> {
        let secrets = config.secrets.store();
        let data = DataDir::resolve(config.data_dir.as_deref())?;
        let store = Arc::new(open_store(&data)?);
        let egress = Arc::new(Egress::new(
            Arc::clone(&secrets),
            Arc::clone(&store),
            config.egress.clone(),
        )?);
        Self::assemble(config, data, store, secrets, egress).await
    }

    /// Like [`Core::open`], with model requests sent to a stand-in server. Test builds of
    /// this crate only.
    #[cfg(test)]
    pub(crate) async fn open_with_upstream_for_tests(
        config: CoreConfig,
        secrets: Arc<dyn SecretStore>,
        origin: &str,
    ) -> Result<Self, CoreError> {
        let data = DataDir::resolve(config.data_dir.as_deref())?;
        let store = Arc::new(open_store(&data)?);
        let egress = Arc::new(Egress::with_upstream_for_tests(
            Arc::clone(&secrets),
            Arc::clone(&store),
            config.egress.clone(),
            origin,
        ));
        Self::assemble(config, data, store, secrets, egress).await
    }

    async fn assemble(
        config: CoreConfig,
        data: DataDir,
        store: Arc<Store>,
        secrets: Arc<dyn SecretStore>,
        egress: Arc<Egress>,
    ) -> Result<Self, CoreError> {
        let data_lock = {
            let (data, store) = (data.clone(), Arc::clone(&store));
            tokio::task::spawn_blocking(move || crate::sweep::lock_and_sweep(&data, &store))
                .await
                .ok()
                .flatten()
        };
        let runtime = match config.runtime {
            Some(runtime) => runtime,
            None => PythonRuntime::discover().await?,
        };
        let agent = config
            .agent
            .unwrap_or_else(|| AgentCommand::python_module(&runtime));
        let jobs = JobRunner::new(data.clone(), runtime, Arc::clone(&store), config.jobs);
        let job_notices = jobs.notices();
        let modeler = Arc::new(Modeler::new(data.clone(), Arc::clone(&store), jobs));
        let gateway =
            Arc::new(Gateway::new(Arc::clone(&modeler), config.gateway).map_err(CoreError::Setup)?);
        let supervisor = AgentSupervisor::new(
            agent,
            data.clone(),
            Arc::clone(&gateway),
            Arc::clone(&egress),
            config.agent_limits,
        );
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let prints = PrintService::new(
            data.clone(),
            Arc::clone(&store),
            config.printer,
            events.clone(),
        )
        .map_err(CoreError::Setup)?;
        let lan = Arc::new(crate::printer_lan::LanService::new(
            Arc::clone(&store),
            Arc::clone(&secrets),
            crate::printer_lan::config_dir(config.config_dir.as_deref()),
        ));
        Ok(Self {
            inner: Arc::new(Inner {
                _data_lock: data_lock,
                prints,
                lan,
                events,
                job_notices,
                data,
                store,
                modeler,
                gateway,
                egress,
                supervisor,
                secrets,
                runs: Mutex::new(HashMap::new()),
                starting: Mutex::new(std::collections::HashSet::new()),
                #[cfg(test)]
                search_base_for_tests: Mutex::new(None),
            }),
        })
    }

    /// Points the search services at a stand-in on loopback. Test builds only: a release
    /// build has no such switch, and its clients reach the services' own hosts alone.
    #[cfg(test)]
    pub(crate) fn set_search_base_for_tests(&self, base: &str) {
        *self
            .inner
            .search_base_for_tests
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(base.to_owned());
    }

    /// A handle that does not keep this core alive (see [`WeakCore`]).
    pub fn downgrade(&self) -> WeakCore {
        WeakCore {
            inner: Arc::downgrade(&self.inner),
        }
    }

    /// The data directory in use.
    pub fn data_dir(&self) -> &Path {
        self.inner.data.root()
    }

    /// The tool gateway, for callers that drive tools directly (the CLI, tests).
    pub fn gateway(&self) -> &Arc<Gateway> {
        &self.inner.gateway
    }

    /// The tools the agent is offered.
    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        self.inner
            .gateway
            .tool_specs()
            .into_iter()
            .cloned()
            .collect()
    }

    /// Installs the slicer used by `slice_run` and by the agent's `slice` tool.
    pub fn set_slice_backend(&self, backend: Arc<dyn SliceBackend>) {
        self.inner.gateway.set_slice_backend(backend);
    }

    /// Sets what opens Bambu Connect's import link after a confirmed hand-off. Until it is
    /// set the stand-in opener is used, which opens nothing: tests and the command line
    /// never open anything.
    pub fn set_link_opener(&self, opener: Arc<dyn hotend_printer::LinkOpener>) {
        self.inner.prints.set_link_opener(opener);
    }

    /// Sets the host the hand-off path is made for (WSL: a Windows drive folder and the
    /// Windows path in the link). Until it is set the local path is used.
    pub fn set_handoff_host(&self, host: hotend_printer::HostKind) {
        self.inner.prints.set_handoff_host(host);
    }

    /// The Bambu Connect hand-off as it is set up now: the host, the folder (and its
    /// Windows form), what is wrong with it, and what opens the link.
    pub async fn handoff_status(&self) -> Result<crate::printing::HandOffStatus, CoreError> {
        Ok(self.inner.prints.handoff_status()?)
    }

    /// Checks `folder` as the hand-off folder would be checked once the settings name it,
    /// for the host set with [`Core::set_handoff_host`] (`handoff_folder` with the reason
    /// when it cannot be used). Nothing is saved: check a candidate before writing the
    /// settings.
    pub async fn handoff_folder_check(&self, folder: &std::path::Path) -> Result<(), CoreError> {
        Ok(self.inner.prints.handoff_folder_check(folder)?)
    }

    /// Everything the core reports while it works, on one channel: the events of every
    /// agent run and of every print job. A slow subscriber loses old events
    /// (`RecvError::Lagged`), never the core its progress.
    pub fn events(&self) -> broadcast::Receiver<CoreEvent> {
        self.inner.events.subscribe()
    }

    fn scope(project_id: &str) -> JobScope {
        JobScope {
            project_id: Some(project_id.to_owned()),
            ..JobScope::default()
        }
    }

    // ---- projects and versions --------------------------------------------------------

    /// `projects_list`: every project, most recently changed first.
    pub async fn projects_list(&self) -> Result<Vec<Project>, CoreError> {
        Ok(self.inner.store.list_projects()?)
    }

    /// `project_create`: a new, empty project.
    pub async fn project_create(&self, name: &str) -> Result<Project, CoreError> {
        Ok(self.inner.store.create_project(name)?)
    }

    /// `project_get`: a project with its versions and conversation.
    pub async fn project_get(&self, project_id: &str) -> Result<ProjectDetail, CoreError> {
        let store = &self.inner.store;
        Ok(ProjectDetail {
            project: store.project(project_id)?,
            versions: store.list_versions(project_id)?,
            transcript: store.transcript(project_id)?,
        })
    }

    /// `version_get`: a version with its parameters, parts, checks and artefact paths.
    pub async fn version_get(
        &self,
        project_id: &str,
        number: u32,
    ) -> Result<VersionDetail, CoreError> {
        Ok(self.inner.modeler.version_detail(project_id, number)?)
    }

    /// `version_mesh`: the bytes of the version's `mesh.json`, as three-cad-viewer loads it.
    pub async fn version_mesh(&self, project_id: &str, number: u32) -> Result<Vec<u8>, CoreError> {
        let detail = self.inner.modeler.version_detail(project_id, number)?;
        let Some(mesh) = detail.result.artifacts.mesh else {
            return Err(CoreError::InvalidInput(format!(
                "version {number} has no mesh (the build reported: {})",
                detail.result.warnings.join("; ")
            )));
        };
        let path = detail.dir.join(mesh);
        tokio::fs::read(&path)
            .await
            .map_err(|source| CoreError::Modeling(ModelingError::Io { path, source }))
    }

    /// `versions_list`: the versions of a project, oldest first.
    pub async fn versions_list(&self, project_id: &str) -> Result<Vec<Version>, CoreError> {
        self.inner.store.project(project_id)?;
        Ok(self.inner.store.list_versions(project_id)?)
    }

    /// `version_revert`: makes an earlier version the current one again. No version is
    /// changed or removed; the next build continues from the reverted one.
    pub async fn version_revert(
        &self,
        project_id: &str,
        number: u32,
    ) -> Result<Version, CoreError> {
        self.inner.store.set_current_version(project_id, number)?;
        let _ = self.inner.store.audit(NewAudit {
            project_id: Some(project_id.to_owned()),
            kind: "version",
            name: "revert".to_owned(),
            outcome: "ok".to_owned(),
            detail: json!({"version": number}),
            ..NewAudit::default()
        });
        Ok(self.inner.store.version(project_id, number)?)
    }

    /// `questions_list`: what the user should be asked about a version (the current one when
    /// `version` is `None`): requirements of the contract that rest on a guess, then every
    /// number found on the web that nobody has confirmed (`requirement` starts with `web:`,
    /// and `web` carries the page and the quote).
    pub async fn questions_list(
        &self,
        project_id: &str,
        version: Option<u32>,
    ) -> Result<Vec<Question>, CoreError> {
        let number = match version {
            Some(number) => number,
            None => self.inner.modeler.current_version(project_id)?.number,
        };
        let detail = self.inner.modeler.version_detail(project_id, number)?;
        let notes = self.inner.store.research_notes(project_id)?;
        let mut questions = detail.result.questions.clone().unwrap_or_default();
        questions.extend(crate::research::web_questions(&notes, &detail.result));
        Ok(questions)
    }

    /// `measure_run`: exact measurements on the current version; one result per query
    /// (PROTOCOL.md section 3.2, `measure`).
    pub async fn measure_run(
        &self,
        project_id: &str,
        queries: Vec<serde_json::Value>,
    ) -> Result<Vec<serde_json::Value>, CoreError> {
        if queries.is_empty() || queries.len() > 32 {
            return Err(CoreError::InvalidInput(
                "give between 1 and 32 measurements".to_owned(),
            ));
        }
        Ok(self
            .inner
            .modeler
            .measure(&Self::scope(project_id), project_id, queries)
            .await?)
    }

    /// Builds a model script the user supplied (the CLI's `hotend build`) and, if it built,
    /// stores it as the project's next version. `params` are merged over the script's
    /// defaults. The report has no version when the script failed.
    pub async fn model_build(
        &self,
        project_id: &str,
        code: &str,
        params: BTreeMap<String, f64>,
        note: &str,
    ) -> Result<BuildReport, CoreError> {
        Ok(self
            .inner
            .modeler
            .build_version(
                &Self::scope(project_id),
                project_id,
                code,
                params,
                "write_model_code",
                note,
            )
            .await?)
    }

    /// Renders PNG views of the current version, optionally cut open by a section plane.
    pub async fn views_render(
        &self,
        project_id: &str,
        views: Vec<String>,
        section: Option<Section>,
        size: [u32; 2],
    ) -> Result<Vec<RenderedView>, CoreError> {
        Ok(self
            .inner
            .modeler
            .render_views(&Self::scope(project_id), project_id, views, section, size)
            .await?)
    }

    /// `parameter_set`: rebuilds the current version with one parameter changed. The report
    /// has no version when the model refused the value; the reason is in its result.
    pub async fn parameter_set(
        &self,
        project_id: &str,
        name: &str,
        value: f64,
    ) -> Result<BuildReport, CoreError> {
        Ok(self
            .inner
            .modeler
            .set_parameter(&Self::scope(project_id), project_id, name, value)
            .await?)
    }

    /// `checks_run`: runs the checks (all, or the named ones) on the current version.
    pub async fn checks_run(
        &self,
        project_id: &str,
        checks: Option<Vec<String>>,
        sample_ranges: bool,
    ) -> Result<EngineResult, CoreError> {
        if let Some(unknown) = checks
            .iter()
            .flatten()
            .find(|id| !crate::gateway::CHECK_IDS.contains(&id.as_str()))
        {
            return Err(CoreError::InvalidInput(format!(
                "`{}` is not a check; the checks are: {}",
                truncate(unknown, 40),
                crate::gateway::CHECK_IDS.join(", ")
            )));
        }
        Ok(self
            .inner
            .modeler
            .run_checks(&Self::scope(project_id), project_id, checks, sample_ranges)
            .await?)
    }

    /// `export_run`: writes STEP, 3MF or STL files of a version (the current one when
    /// `version` is `None`) into `destination`, an existing folder.
    pub async fn export_run(
        &self,
        project_id: &str,
        version: Option<u32>,
        formats: Vec<ExportFormat>,
        parts: Option<Vec<String>>,
        destination: &Path,
    ) -> Result<Vec<ExportedFile>, CoreError> {
        Ok(self
            .inner
            .modeler
            .export(
                &Self::scope(project_id),
                project_id,
                version,
                formats,
                parts,
                destination,
            )
            .await?)
    }

    // ---- snapshots ----------------------------------------------------------------------

    /// `snapshot_create`: names a version (the current one when `version` is `None`) so it
    /// can be exported and gone back to later. Changes no version, no geometry and not the
    /// current version. When a print job of that version has left Hotend, the newest one
    /// is recorded with its file's SHA-256 and that exact sliced file is kept with the
    /// snapshot (see [`crate::snapshot`]).
    pub async fn snapshot_create(
        &self,
        project_id: &str,
        name: &str,
        version: Option<u32>,
        note: &str,
    ) -> Result<Snapshot, CoreError> {
        let store = &self.inner.store;
        let number = match version {
            Some(number) => number,
            None => store
                .project(project_id)?
                .current_version
                .ok_or(ModelingError::NoCurrentVersion)?,
        };
        let snapshot = store
            .create_snapshot(project_id, name, number, note)
            .map_err(invalid_input)?;
        let kept = {
            let (data, store, kept) =
                (self.inner.data.clone(), Arc::clone(store), snapshot.clone());
            tokio::task::spawn_blocking(move || crate::snapshot::keep_printed(&data, &store, &kept))
                .await
                .unwrap_or_else(|_| json!("not found"))
        };
        store.audit_snapshot(
            &snapshot,
            "create",
            None,
            "ok",
            json!({"by": "person", "printed_file": kept}),
        );
        Ok(snapshot)
    }

    /// `snapshots_list`: the snapshots of a project, oldest first.
    pub async fn snapshots_list(&self, project_id: &str) -> Result<Vec<Snapshot>, CoreError> {
        self.inner.store.project(project_id)?;
        Ok(self.inner.store.snapshots(project_id)?)
    }

    /// `snapshot_delete`: removes the name, and nothing else: the version, its files and a
    /// kept printed file stay.
    pub async fn snapshot_delete(
        &self,
        project_id: &str,
        name: &str,
    ) -> Result<Snapshot, CoreError> {
        let snapshot = self.inner.store.delete_snapshot(project_id, name)?;
        self.inner
            .store
            .audit_snapshot(&snapshot, "delete", None, "ok", json!({}));
        Ok(snapshot)
    }

    /// `snapshot_restore`: makes the snapshot's version the current one again
    /// ([`Core::version_revert`]). Nothing is lost: the later versions stay and can be
    /// returned to by number or by their own snapshots; the next build has this version as
    /// its parent.
    pub async fn snapshot_restore(
        &self,
        project_id: &str,
        name: &str,
    ) -> Result<Version, CoreError> {
        let snapshot = self.inner.store.snapshot(project_id, name)?;
        let before = self.inner.store.project(project_id)?.current_version;
        let version = self.version_revert(project_id, snapshot.version).await?;
        self.inner.store.audit_snapshot(
            &snapshot,
            "restore",
            None,
            "ok",
            json!({"was_current": before}),
        );
        Ok(version)
    }

    /// `snapshot_export`: writes the snapshot's files into a new folder below `folder` (an
    /// existing folder the person chose, as an absolute path): STEP and 3MF of its version
    /// (the version's own stored file, copied byte for byte, where it stores that format;
    /// `export_run` for one it does not), the exact sliced file of the print recorded when
    /// the snapshot was made (the same bytes, checked by the recorded SHA-256), and
    /// `snapshot.json` with the name, version, times and every file's hash. Refused before
    /// anything is written when the snapshot's folder already exists there. When the
    /// recorded printed file is found nowhere, the rest is exported and `printed_missing`
    /// (and the manifest) say so.
    pub async fn snapshot_export(
        &self,
        project_id: &str,
        name: &str,
        folder: &Path,
    ) -> Result<SnapshotExport, CoreError> {
        let store = &self.inner.store;
        let snapshot = store.snapshot(project_id, name)?;
        let outcome = self.snapshot_export_files(&snapshot, folder).await;
        let detail = match &outcome {
            Ok(export) => json!({
                "files": export.files.iter().map(|file| json!({
                    "kind": file.kind, "bytes": file.bytes, "sha256": file.sha256,
                })).collect::<Vec<_>>(),
                "printed_job": export.printed.as_ref().map(|printed| printed.job_id.clone()),
                "printed_missing": export.printed_missing.as_ref().map(|missing| json!({
                    "job_id": missing.job_id, "sha256": missing.sha256,
                })),
            }),
            Err(error) => json!({"error": truncate(&error.to_string(), 500)}),
        };
        let code = match &outcome {
            Ok(_) => "ok".to_owned(),
            Err(error) => {
                // The facade error's own code when it has one, else its kind.
                let value =
                    serde_json::to_value(crate::api::ApiError::from(error)).unwrap_or_default();
                value
                    .get("code")
                    .or_else(|| value.get("kind"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("error")
                    .to_owned()
            }
        };
        store.audit_snapshot(&snapshot, "export", None, &code, detail);
        outcome
    }

    async fn snapshot_export_files(
        &self,
        snapshot: &Snapshot,
        folder: &Path,
    ) -> Result<SnapshotExport, CoreError> {
        use crate::snapshot::{MANIFEST, Printed, SnapshotFile};
        if !folder.is_absolute() || !folder.is_dir() {
            return Err(CoreError::InvalidInput(format!(
                "the export folder must be an existing folder given by its full path; {} is \
                 not one",
                folder.display()
            )));
        }
        let project = self.inner.store.project(&snapshot.project_id)?;
        let printed = {
            let (data, store, kept) = (
                self.inner.data.clone(),
                Arc::clone(&self.inner.store),
                snapshot.clone(),
            );
            tokio::task::spawn_blocking(move || crate::snapshot::printed(&data, &store, &kept))
                .await
                .map_err(|error| CoreError::Setup(error.to_string()))??
        };
        let (printed, printed_missing) = match printed {
            Printed::Never => (None, None),
            Printed::Kept { path, file } => (Some((path, file)), None),
            Printed::Missing(missing) => (None, Some(missing)),
        };
        let folder_name = crate::snapshot::folder_name(&project.name, snapshot);
        let target = folder.join(&folder_name);
        let exists = |existing: &str| {
            CoreError::InvalidInput(format!(
                "{} already exists in {} (an earlier export of this snapshot, or of one whose \
                 name makes the same folder name); nothing was written. Choose another folder, \
                 or remove that one first",
                existing,
                folder.display()
            ))
        };
        // A name that differs only in letter case is the same folder on many file systems.
        let lower = folder_name.to_lowercase();
        let entries = std::fs::read_dir(folder).map_err(|source| {
            CoreError::Modeling(ModelingError::Io {
                path: folder.to_path_buf(),
                source,
            })
        })?;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.to_lowercase() == lower {
                return Err(exists(&name));
            }
        }
        // Made new: fails when it appeared in between.
        std::fs::create_dir(&target).map_err(|source| {
            if source.kind() == std::io::ErrorKind::AlreadyExists {
                exists(&folder_name)
            } else {
                CoreError::Modeling(ModelingError::Io {
                    path: target.clone(),
                    source,
                })
            }
        })?;
        let io = |path: &Path| {
            let path = path.to_path_buf();
            move |source| CoreError::Modeling(ModelingError::Io { path, source })
        };
        // The version's own stored files are copied byte for byte (hash checked); only a
        // format the version does not store is made by `export_run`.
        let version_dir = self
            .inner
            .data
            .version_dir(&snapshot.project_id, snapshot.version)?;
        let mut files: Vec<SnapshotFile> = Vec::new();
        let mut missing: Vec<ExportFormat> = Vec::new();
        for (format, kind, name) in [
            (ExportFormat::Step, "step", "model.step"),
            (ExportFormat::ThreeMf, "3mf", "model.3mf"),
        ] {
            let stored = version_dir.join(name);
            let plain = std::fs::symlink_metadata(&stored).is_ok_and(|meta| meta.is_file());
            if !plain {
                missing.push(format);
                continue;
            }
            let expected = crate::snapshot::hash_regular(&stored, u64::MAX).map_err(io(&stored))?;
            let path = target.join(name);
            let bytes = crate::snapshot::copy_new(&stored, &path).map_err(io(&path))?;
            let sha256 = crate::snapshot::hash_regular(&path, u64::MAX).map_err(io(&path))?;
            if sha256 != expected {
                let _ = std::fs::remove_file(&path);
                return Err(CoreError::InvalidInput(format!(
                    "the copy of version {}'s {name} does not have the stored file's SHA-256; \
                     it was not exported",
                    snapshot.version
                )));
            }
            files.push(SnapshotFile {
                kind: kind.to_owned(),
                source: "copied".to_owned(),
                path,
                bytes,
                sha256,
            });
        }
        if !missing.is_empty() {
            let exported = self
                .export_run(
                    &snapshot.project_id,
                    Some(snapshot.version),
                    missing,
                    None,
                    &target,
                )
                .await?;
            files.extend(exported.into_iter().map(|file| {
                SnapshotFile {
                    kind: match file.format {
                        ExportFormat::Step => "step",
                        ExportFormat::ThreeMf => "3mf",
                        ExportFormat::Stl => "stl",
                    }
                    .to_owned(),
                    source: "exported".to_owned(),
                    path: file.path,
                    bytes: file.bytes,
                    sha256: file.sha256,
                }
            }));
        }
        let printed_file = match printed {
            Some((source, file)) => {
                let path = target.join(&file.file_name);
                let bytes = crate::snapshot::copy_new(&source, &path).map_err(io(&path))?;
                let sha256 = crate::snapshot::hash_regular(&path, u64::MAX).map_err(io(&path))?;
                if !sha256.eq_ignore_ascii_case(&file.sha256) {
                    let _ = std::fs::remove_file(&path);
                    return Err(CoreError::InvalidInput(format!(
                        "the printed file changed while it was copied (SHA-256 {sha256}, \
                         expected {}); it was not exported",
                        file.sha256
                    )));
                }
                files.push(SnapshotFile {
                    kind: "printed".to_owned(),
                    source: "copied".to_owned(),
                    path,
                    bytes,
                    sha256,
                });
                Some(file)
            }
            None => None,
        };
        let manifest = crate::snapshot::manifest(
            &project.name,
            snapshot,
            &files,
            printed_file.as_ref(),
            printed_missing.as_ref(),
        );
        let path = target.join(MANIFEST);
        crate::snapshot::write_new(&path, &manifest).map_err(io(&path))?;
        files.push(SnapshotFile {
            kind: "manifest".to_owned(),
            source: "written".to_owned(),
            path,
            bytes: manifest.len() as u64,
            sha256: crate::util::sha256_hex(&manifest),
        });
        Ok(SnapshotExport {
            snapshot: snapshot.clone(),
            folder: target,
            files,
            printed: printed_file,
            printed_missing,
        })
    }

    // ---- print preparation, gauges and calibration ----------------------------------------

    /// `prepare_run`: lays a version (the current one when `version` is `None`) out on
    /// plates, each part turned the way it prints best, and stores the result with the
    /// version.
    pub async fn prepare_run(
        &self,
        project_id: &str,
        version: Option<u32>,
        options: PrepareOptions,
    ) -> Result<Preparation, CoreError> {
        Ok(self
            .inner
            .modeler
            .prepare(&Self::scope(project_id), project_id, version, options)
            .await?)
    }

    /// `preparation_get`: the stored preparation of a version, if it was prepared.
    pub async fn preparation_get(
        &self,
        project_id: &str,
        version: u32,
    ) -> Result<Option<Preparation>, CoreError> {
        Ok(self.inner.modeler.preparation(project_id, version)?)
    }

    /// `print_plan_get`: Hotend's print settings for one plate of a version (the current
    /// one when `version` is `None`, plate 1 when `plate` is `None`): every decision with
    /// its field, value, unit, source, rule and sentence, the questions, warnings and
    /// assumptions, the checklist, and why the plan is blocked when it is. Prepares the
    /// version first when it is not laid out for the settings.
    pub async fn print_plan_get(
        &self,
        project_id: &str,
        version: Option<u32>,
        plate: Option<u32>,
    ) -> Result<PrintPlanView, CoreError> {
        let (preparation, plate_plan) = self
            .inner
            .modeler
            .print_plan(&Self::scope(project_id), project_id, version, plate)
            .await?;
        Ok(plate_plan.view(&preparation))
    }

    /// `gauge_export`: writes the test gauges of feature cards as plates (3MF and
    /// pictures) into `destination`, an existing folder. Without `cards`, the gauges of
    /// every card the project's current version uses.
    pub async fn gauge_export(
        &self,
        project_id: Option<&str>,
        cards: Option<Vec<CardRef>>,
        nozzle_id: Option<i64>,
        destination: &Path,
    ) -> Result<GaugeExport, CoreError> {
        let scope = project_id.map(Self::scope).unwrap_or_default();
        Ok(self
            .inner
            .modeler
            .gauge(&scope, project_id, cards, nozzle_id, destination)
            .await?)
    }

    /// `calibration_get`: the calibration table of the printer in the settings.
    pub async fn calibration_get(&self) -> Result<Calibration, CoreError> {
        Ok(self.inner.modeler.calibration()?)
    }

    /// `calibration_record`: records caliper readings of a printed gauge in the calibration
    /// table of the printer and material in the settings. The core dates them now.
    pub async fn calibration_record(
        &self,
        readings: Vec<GaugeReading>,
        nozzle_id: Option<i64>,
    ) -> Result<CalibrationUpdate, CoreError> {
        Ok(self
            .inner
            .modeler
            .calibrate(&JobScope::default(), readings, nozzle_id)
            .await?)
    }

    /// `baseline_list`: the usable proven baselines of this data folder, one per printer,
    /// plate type and material. Records that cannot be used are left out here and named by
    /// [`Core::baseline_invalid`].
    pub async fn baseline_list(&self) -> Result<Vec<crate::modeling::ProvenBaseline>, CoreError> {
        Ok(self.inner.modeler.baselines().list()?)
    }

    /// `baseline_invalid`: the records of the proven-baselines file that cannot be used
    /// (and why). Each blocks the plans of its own printer, plate and material until it is
    /// corrected or removed with [`Core::baseline_remove`] or
    /// [`Core::baseline_remove_invalid`]; one that does not name all three blocks none.
    pub async fn baseline_invalid(
        &self,
    ) -> Result<Vec<crate::modeling::InvalidBaseline>, CoreError> {
        Ok(self.inner.modeler.baselines().invalid()?)
    }

    /// `baseline_set`: records the sticking settings of a print the person reported as good
    /// as the proven baseline of its printer, plate and material (replacing an earlier one).
    /// Every value is checked against its field's hard range. Returns the record replaced.
    pub async fn baseline_set(
        &self,
        entry: crate::modeling::ProvenBaseline,
    ) -> Result<Option<crate::modeling::ProvenBaseline>, CoreError> {
        Ok(self.inner.modeler.baseline_set(entry)?)
    }

    /// `baseline_remove`: removes the proven baseline of a printer, plate and material, and
    /// any record of the three that cannot be used. Returns what was removed (empty when
    /// there was nothing).
    pub async fn baseline_remove(
        &self,
        printer_id: &str,
        plate_type: &str,
        material_id: &str,
    ) -> Result<crate::modeling::BaselineRemoval, CoreError> {
        Ok(self
            .inner
            .modeler
            .baseline_remove(printer_id, plate_type, material_id)?)
    }

    /// `baseline_remove_invalid`: removes every record of the proven-baselines file that
    /// cannot be used on its own, and only those; usable records, and the copies of a
    /// printer, plate and material recorded twice (settled by key), are kept as they are.
    /// Returns what was removed (empty when there was nothing).
    pub async fn baseline_remove_invalid(
        &self,
    ) -> Result<Vec<crate::modeling::InvalidBaseline>, CoreError> {
        Ok(self.inner.modeler.baseline_remove_invalid()?)
    }

    // ---- slicing and printing ---------------------------------------------------------------

    /// `slicers_list`: the slicer programs the installed backend knows, found or not.
    /// Empty until a backend is installed with [`Core::set_slice_backend`].
    pub async fn slicers_list(&self) -> Result<Vec<SlicerInstallation>, CoreError> {
        let Some(backend) = self.inner.gateway.slice_backend_handle() else {
            return Ok(Vec::new());
        };
        let executables = self.inner.modeler.settings()?.slicer_executables;
        tokio::task::spawn_blocking(move || backend.installations(&executables))
            .await
            .map_err(|e| CoreError::Setup(e.to_string()))
    }

    /// `slicer_choose`: makes a slicer the one in use. Which slicer a slice runs follows
    /// from the printer, so this selects a printer that is sliced with `kind` (keeping the
    /// current one when it already is) and, with `executable`, the program to run for it.
    /// Returns the settings as saved.
    pub async fn slicer_choose(
        &self,
        kind: &str,
        executable: Option<PathBuf>,
    ) -> Result<Settings, CoreError> {
        let known = self.slicers_list().await?;
        let Some(slicer) = known.iter().find(|slicer| slicer.kind == kind) else {
            return Err(CoreError::InvalidInput(format!(
                "`{}` is not a slicer of this build; the slicers are: {}",
                truncate(kind, 40),
                known
                    .iter()
                    .map(|slicer| slicer.kind.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        };
        let mut settings = self.inner.modeler.settings()?;
        if !slicer.printers.contains(&settings.printer_id) {
            let printer = slicer
                .printers
                .iter()
                .find(|id| crate::profiles::printer_profile(id).is_some())
                .ok_or_else(|| {
                    CoreError::InvalidInput(format!(
                        "no printer of this build is sliced with {}",
                        slicer.name
                    ))
                })?;
            settings.printer_id = printer.clone();
        }
        if let Some(executable) = executable {
            settings
                .slicer_executables
                .insert(kind.to_owned(), executable);
        }
        self.settings_set(settings.clone()).await?;
        Ok(settings)
    }

    /// `slice_run`: prepares the current version if it is not (plates for the printer and
    /// material in the settings) and slices one plate. Fails with the code `not_configured`
    /// until a slicer backend is installed with [`Core::set_slice_backend`].
    pub async fn slice_run(
        &self,
        project_id: &str,
        profile: &str,
        plate: Option<u32>,
    ) -> Result<SliceOutcome, CoreError> {
        Ok(self
            .inner
            .gateway
            .slice_current(&Self::scope(project_id), project_id, profile, plate)
            .await?)
    }

    /// `slice_get`: the newest slice of a version (the current one when `version` is
    /// `None`), if it was sliced; for showing what the agent's `slice` tool produced.
    pub async fn slice_get(
        &self,
        project_id: &str,
        version: Option<u32>,
    ) -> Result<Option<SliceOutcome>, CoreError> {
        let number = match version {
            Some(number) => number,
            None => self.inner.modeler.current_version(project_id)?.number,
        };
        Ok(self.inner.gateway.stored_slice(project_id, number)?)
    }

    /// `printers_list`: the printers this build can deliver to (the simulated one) with
    /// their state, and every delivery path with what it can and cannot do.
    pub async fn printers_list(&self) -> Result<Vec<PrinterInfo>, CoreError> {
        Ok(self.inner.prints.printers()?)
    }

    /// `print_propose`: creates a pending print job from the newest slice of the current
    /// version. It only writes a record; a person confirms it with [`Core::print_confirm`].
    pub async fn print_propose(&self, project_id: &str, note: &str) -> Result<PrintJob, CoreError> {
        if note.len() > 500 {
            return Err(CoreError::InvalidInput(
                "the note is longer than 500 bytes".to_owned(),
            ));
        }
        Ok(self.inner.gateway.propose_print(project_id, note)?.into())
    }

    /// `print_jobs`: the print jobs of a project, newest first, pending or not.
    pub async fn print_jobs(&self, project_id: &str) -> Result<Vec<PrintJob>, CoreError> {
        self.inner.store.project(project_id)?;
        Ok(self.inner.prints.jobs(project_id)?)
    }

    /// `print_status`: one print job as it is now.
    pub async fn print_status(&self, job_id: &str) -> Result<PrintJob, CoreError> {
        Ok(self.inner.prints.job(job_id)?)
    }

    /// `print_review`: runs the G-code linter over a pending job's sliced file and returns
    /// the confirmation summary, read from that file, for the chosen delivery. A file the
    /// linter refuses marks the job `rejected` and is an error.
    pub async fn print_review(
        &self,
        job_id: &str,
        delivery: DeliveryChoice,
    ) -> Result<PrintReview, CoreError> {
        let mut review = self.inner.prints.review(job_id, delivery).await?;
        // A job sliced with Hotend's print settings is reviewed only while they are still
        // the plate's plan (the review's own checks, such as the plate and the layout,
        // speak first); a refused review is not kept.
        if let Err(error) = self.job_plan_is_current(job_id) {
            self.inner.prints.forget_review(job_id);
            return Err(error);
        }
        // The numbers of this version that came from the web and that nobody has confirmed:
        // the person sees them before they say yes.
        let (project_id, version) = (&review.job.project_id, review.job.version);
        if let (Ok(detail), Ok(notes)) = (
            self.inner.modeler.version_detail(project_id, version),
            self.inner.store.research_notes(project_id),
        ) {
            review.web_numbers = crate::research::unconfirmed_in_use(&notes, &detail.result);
        }
        Ok(review)
    }

    /// `print_confirm`: delivers a reviewed job on a person's confirmation.
    ///
    /// `authority` is the process's only [`hotend_printer::ConfirmationAuthority`]: the
    /// layer that shows the summary to a person (the native dialog, the terminal prompt)
    /// takes it at start-up and passes it here after the person said yes. It must never be
    /// reachable from a webview command or from anything the agent can call.
    /// `shown` is the text the person was shown before they said yes: everything the
    /// dialog or the terminal displayed for the job. It must contain
    /// [`PrintReview::review_text`] of the job's last review in one piece, word for word
    /// (`summary_mismatch` otherwise): the confirmation is given for that text and the
    /// printer layer binds the start to it. Pass the text that was displayed, never a
    /// copy made for this call.
    pub async fn print_confirm(
        &self,
        authority: &hotend_printer::ConfirmationAuthority,
        job_id: &str,
        shown: &str,
    ) -> Result<PrintJob, CoreError> {
        // The plan may have changed while the review was on the screen (another baseline,
        // spool, nozzle or setting): then the reviewed file is not what Hotend would
        // print now, and the review is used up. The confirmation's own reasons (no
        // review, another plate, another layout) are named first when they hold.
        if let Err(error) = self.job_plan_is_current(job_id) {
            let error = self
                .inner
                .prints
                .held_review_problem(job_id)
                .map_or(error, CoreError::from);
            self.inner.prints.forget_review(job_id);
            return Err(error);
        }
        Ok(self.inner.prints.confirm(authority, job_id, shown).await?)
    }

    /// `Ok` unless the print job's file was sliced with Hotend's print settings for its
    /// plate and those are no longer the plate's plan (`plan_changed`). A job whose slice
    /// is gone is left to the review, which refuses it on its own.
    fn job_plan_is_current(&self, job_id: &str) -> Result<(), CoreError> {
        let job = self.inner.store.print_job(job_id)?;
        let Some(record) =
            self.inner
                .store
                .slice(&job.project_id, job.version, &job.profile_hash)?
        else {
            return Ok(());
        };
        let Some(plan_sha256) = record
            .job
            .get("plan_sha256")
            .and_then(serde_json::Value::as_str)
        else {
            return Ok(());
        };
        let plate = record
            .job
            .get("plate")
            .and_then(serde_json::Value::as_u64)
            .and_then(|plate| u32::try_from(plate).ok())
            .unwrap_or(1);
        if self
            .inner
            .gateway
            .plan_is_current(&job.project_id, job.version, plate, plan_sha256)?
        {
            Ok(())
        } else {
            Err(PrintError::PlanChanged.into())
        }
    }

    /// `print_cancel`: withdraws a pending job, or stops one that is starting or printing
    /// on the simulated printer.
    pub async fn print_cancel(&self, job_id: &str) -> Result<PrintJob, CoreError> {
        Ok(self.inner.prints.cancel(job_id)?)
    }

    /// `print_project_make`: makes the editable Bambu Studio project of one plate of the
    /// current version (plate 1 when `plate` is `None`), for its current layout and print
    /// plan, into `versions/<n>/slicer-project/<plate>-<first 16 hex digits of the plan's
    /// hash>/`, and records it with its SHA-256. Only with the real Bambu Studio (the
    /// stand-in is refused with `stand_in_slicer`), only for a plan that is not blocked,
    /// and only when a slice of the same plan and layout exists whose file passes the
    /// G-code check now (the check runs again; `gcode_rejected` otherwise). The project's
    /// own test slice must pass the proof of the settings and the G-code check too. A
    /// project opened in Bambu Studio is sliced again by Bambu Studio, so it is outside
    /// Hotend's check; nothing here opens or sends anything.
    pub async fn print_project_make(
        &self,
        project_id: &str,
        plate: Option<u32>,
    ) -> Result<EditableProject, CoreError> {
        let plan = self.inner.gateway.project_plan(project_id, plate).await?;
        // A plan whose sliced file the check refuses never becomes a project.
        self.inner.prints.check_plan_slice(&plan.slice).await?;
        let export = self.inner.gateway.export_project(&plan).await?;
        Ok(self.inner.prints.record_project(&plan, export).await?)
    }

    /// `print_projects`: the editable projects of a project, newest first.
    pub async fn print_projects(
        &self,
        project_id: &str,
    ) -> Result<Vec<EditableProject>, CoreError> {
        self.inner.store.project(project_id)?;
        Ok(self.inner.prints.projects(project_id)?)
    }

    /// `print_project_review`: checks an editable project again (its plan is still the
    /// plate's plan, the slice of that plan passes the G-code check, the project file and
    /// its test slice are the files that were made) and returns what a person must see
    /// before it is handed over to `destination`: the printer layer's review text, which
    /// names the project's SHA-256 and says that the project is outside Hotend's check.
    pub async fn print_project_review(
        &self,
        project_export_id: &str,
        destination: ProjectDestination,
    ) -> Result<ProjectReview, CoreError> {
        // A refused review also drops the last one.
        if let Err(error) = self.project_plan_is_current(project_export_id) {
            self.inner.prints.forget_project_review(project_export_id);
            return Err(error);
        }
        Ok(self
            .inner
            .prints
            .project_review(project_export_id, destination)
            .await?)
    }

    /// `print_project_deliver`: hands a reviewed editable project over on a person's
    /// confirmation: places the very bytes that were sliced and proven, under the job's
    /// label and the project's hash, in the hand-off folder (and asks the core's opener to
    /// open it, which is the stand-in until the application sets the real one) or in the
    /// folder the review named. `authority` and `shown` are as for [`Core::print_confirm`];
    /// `shown` must contain [`ProjectReview::review_text`] of the last review in one piece.
    pub async fn print_project_deliver(
        &self,
        authority: &hotend_printer::ConfirmationAuthority,
        project_export_id: &str,
        shown: &str,
    ) -> Result<EditableProject, CoreError> {
        // The plan may have changed since the review (another baseline, spool, nozzle or
        // setting): then the project no longer carries the plate's plan, and the review is
        // used up.
        // (A project that does not exist is left to the hand-over, which refuses it as not
        // reviewed.)
        if self.inner.prints.project(project_export_id).is_ok()
            && let Err(error) = self.project_plan_is_current(project_export_id)
        {
            self.inner.prints.forget_project_review(project_export_id);
            return Err(error);
        }
        Ok(self
            .inner
            .prints
            .project_deliver(authority, project_export_id, shown)
            .await?)
    }

    /// `Ok` while the editable project's plan is the plate's plan now (`project_stale`
    /// otherwise).
    fn project_plan_is_current(&self, project_export_id: &str) -> Result<(), CoreError> {
        let project = self.inner.prints.project(project_export_id)?;
        if self.inner.gateway.plan_is_current(
            &project.project_id,
            project.version,
            project.plate,
            &project.plan_sha256,
        )? {
            Ok(())
        } else {
            Err(PrintError::ProjectStale.into())
        }
    }

    /// What the simulated printer was sent so far, oldest first. A diagnostic.
    pub fn simulated_printer_wire_log(&self) -> Vec<String> {
        self.inner.prints.wire_log()
    }

    /// `doctor_report`: what this machine has and what is missing, with the slicers of the
    /// installed backend. [`crate::doctor::report`] gives the same without an open core.
    pub async fn doctor_report(&self, skip_self_tests: bool) -> crate::doctor::DoctorReport {
        let mut report = crate::doctor::report(crate::doctor::DoctorOptions {
            slicers: self.inner.gateway.slice_backend_handle(),
            skip_self_tests,
        })
        .await;
        // As this core's own store reads keys, not as `HOTEND_SECRETS` alone would.
        report.keys_from_environment =
            crate::doctor::keys_from_environment(self.inner.secrets.kind());
        report
    }

    /// `audit_log`: the newest audit entries, of one project or of everything.
    pub async fn audit_log(
        &self,
        project_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<AuditEntry>, CoreError> {
        Ok(self.inner.store.list_audit(project_id, limit.min(1000))?)
    }

    // ---- settings and keys ------------------------------------------------------------

    /// `settings_get`: the saved settings, or the defaults.
    pub async fn settings_get(&self) -> Result<Settings, CoreError> {
        Ok(self.inner.modeler.settings()?)
    }

    /// `settings_set`: validates and saves the settings.
    ///
    /// The person's agreement to web research (`research.acknowledged_at_ms`,
    /// `research.acknowledged_source`) is not taken from `settings`: the stored one is
    /// kept, and only `research_consent` / `research_consent_for` change it. The research
    /// source may change here; an agreement covers one source, so a new source then waits
    /// for the person's agreement.
    ///
    /// A material (or printer) that changes is checked against the slicer backend's table
    /// of profiles for the printer: an id the slicer has no profile for is refused, with the
    /// known ids. Settings saved before keep their material until it is changed.
    pub async fn settings_set(&self, mut settings: Settings) -> Result<(), CoreError> {
        let current = self.inner.modeler.settings()?;
        settings.research.acknowledged_at_ms = current.research.acknowledged_at_ms;
        settings.research.acknowledged_source = current.research.acknowledged_source;
        if settings.material.id != current.material.id || settings.printer_id != current.printer_id
        {
            let catalogue = self
                .inner
                .gateway
                .slice_backend_handle()
                .and_then(|backend| backend.catalogue(&settings.printer_id));
            settings
                .validate_material(catalogue.as_ref())
                .map_err(CoreError::InvalidInput)?;
        }
        self.save_settings(&settings)
    }

    /// Validates and saves the settings as they are, the agreement included.
    fn save_settings(&self, settings: &Settings) -> Result<(), CoreError> {
        settings.validate().map_err(CoreError::InvalidInput)?;
        Ok(self
            .inner
            .store
            .set_setting(SETTINGS_KEY, &json!(settings))?)
    }

    // ---- a real printer's status over the local network (read-only) -------------------

    /// `printer_lan_get`: how reading the printer's status is set up (address, serial, how
    /// its certificate is checked, where the access code would come from). Never the code.
    pub async fn printer_lan_get(&self) -> Result<crate::printer_lan::PrinterLanSetup, CoreError> {
        let lan = Arc::clone(&self.inner.lan);
        crate::printer_lan::blocking(move || lan.setup()).await
    }

    /// `printer_lan_set`: the printer's address on the local network (a private-range IPv4
    /// address, never a name) and its serial number. Not secrets.
    pub async fn printer_lan_set(
        &self,
        ip: &str,
        serial: &str,
    ) -> Result<crate::printer_lan::PrinterLanSetup, CoreError> {
        self.inner.lan.set_address(ip, serial)?;
        self.printer_lan_get().await
    }

    /// Forgets the printer's address and serial (a pin stays until it is forgotten).
    pub async fn printer_lan_clear(&self) -> Result<(), CoreError> {
        self.printer_watch_stop().await;
        self.inner.lan.clear_address()
    }

    /// `printer_lan_key_set`: stores the printer's LAN access code. Write-only: no call
    /// returns it. `BAMBU_LAN_ACCESS_CODE` in the environment takes precedence.
    pub async fn printer_lan_key_set(&self, code: String) -> Result<(), CoreError> {
        let lan = Arc::clone(&self.inner.lan);
        crate::printer_lan::blocking(move || lan.set_access_code(code)).await
    }

    /// Removes the stored access code.
    pub async fn printer_lan_key_delete(&self) -> Result<(), CoreError> {
        let lan = Arc::clone(&self.inner.lan);
        crate::printer_lan::blocking(move || lan.delete_access_code()).await
    }

    /// `printer_lan_key_status`: where the access code would come from: `environment`,
    /// `secret_store`, `missing` or `unreadable`. Never the code.
    pub async fn printer_lan_key_status(&self) -> Result<String, CoreError> {
        let lan = Arc::clone(&self.inner.lan);
        crate::printer_lan::blocking(move || Ok(lan.access_code_source().to_owned())).await
    }

    /// `printer_certificate_probe`: a first look at the configured printer's certificate (a
    /// TLS handshake only; nothing is sent), for the person to check and confirm.
    pub async fn printer_certificate_probe(
        &self,
    ) -> Result<crate::printer_lan::PrinterCertificate, CoreError> {
        let lan = Arc::clone(&self.inner.lan);
        let profile = self.inner.prints.profile();
        crate::printer_lan::blocking(move || lan.probe(&profile)).await
    }

    /// `printer_certificate_confirm`: pins the certificate of the last first look, given
    /// its fingerprint as the person confirmed it (in a native dialog, or `yes` typed at a
    /// terminal). The only call that stores a pin.
    pub async fn printer_certificate_confirm(
        &self,
        sha256: &str,
    ) -> Result<crate::printer_lan::CertificatePin, CoreError> {
        self.inner.lan.confirm(sha256)
    }

    /// Forgets the confirmed certificate.
    pub async fn printer_certificate_forget(&self) -> Result<(), CoreError> {
        self.printer_watch_stop().await;
        self.inner.lan.forget_pin()
    }

    /// `printer_status_read`: the printer's status now. Connects (read-only), waits up to
    /// `wait` for a report (Hotend never asks for one), and ends the session; while the
    /// core follows the printer, its latest status. The report is matched to a handed-off
    /// job by name when one fits.
    pub async fn printer_status_read(
        &self,
        wait: std::time::Duration,
    ) -> Result<crate::printing::LanPrinterStatus, CoreError> {
        let lan = Arc::clone(&self.inner.lan);
        let profile = self.inner.prints.profile();
        let follower = self.inner.prints.follower();
        crate::printer_lan::blocking(move || lan.read(&profile, follower, wait)).await
    }

    /// `printer_watch_start`: follows the printer until `printer_watch_stop`; every report
    /// is sent as `CoreEvent::Printer` (and, for a matched handed-off job, as its
    /// `CoreEvent::Print` events).
    pub async fn printer_watch_start(&self) -> Result<(), CoreError> {
        let lan = Arc::clone(&self.inner.lan);
        let profile = self.inner.prints.profile();
        let follower = self.inner.prints.follower();
        crate::printer_lan::blocking(move || lan.start(&profile, follower)).await
    }

    /// `printer_watch_stop`: ends the session; returns whether one was running.
    pub async fn printer_watch_stop(&self) -> bool {
        let lan = Arc::clone(&self.inner.lan);
        tokio::task::spawn_blocking(move || lan.stop())
            .await
            .unwrap_or(false)
    }

    /// The latest status of the session, while one runs or after it ended.
    pub fn printer_watch_latest(&self) -> Option<crate::printing::LanPrinterStatus> {
        self.inner.lan.latest()
    }

    /// The store. Test builds only.
    #[cfg(test)]
    pub(crate) fn store_for_tests(&self) -> &Arc<Store> {
        &self.inner.store
    }

    /// The printer-status service and a follower of this core's jobs. Test builds only.
    #[cfg(test)]
    pub(crate) fn printer_watch_parts_for_tests(
        &self,
    ) -> (
        Arc<crate::printer_lan::LanService>,
        crate::printing::Follower,
    ) {
        (Arc::clone(&self.inner.lan), self.inner.prints.follower())
    }

    /// Follows a scripted status source as if it were the printer. Test builds only.
    #[cfg(test)]
    pub(crate) fn printer_watch_source_for_tests(
        &self,
        source: Box<dyn crate::printer_lan::StatusSource>,
    ) -> Result<(), CoreError> {
        // The previous scripted session may still be finishing (it ended, its thread has
        // not returned yet): wait for it, so that the new one is not refused as running.
        self.inner.lan.stop();
        self.inner
            .lan
            .start_with(source, self.inner.prints.follower())
    }

    // ---- web research ---------------------------------------------------------------

    /// `research_status`: whether the next message of a project may search the web, why
    /// or why not, and what the project has used.
    pub async fn research_status(&self, project_id: &str) -> Result<ResearchStatus, CoreError> {
        let settings = self.inner.modeler.settings()?;
        let row = self.inner.store.research_row(project_id)?;
        // The key store is asked only when a search service is the source: the default
        // path (the provider's own search) does not wait for a keychain.
        // A key store that cannot be asked is said as such (`needs_key` with what to do),
        // never taken for "no key": Keenable would otherwise go keyless while a key may be
        // stored.
        let key = match settings.research.source.service() {
            Some(service) => match self.search_key_status(service).await {
                Ok(true) => SearchKeyState::Present,
                Ok(false) => SearchKeyState::Absent,
                Err(_) => SearchKeyState::Unreadable,
            },
            None => SearchKeyState::Absent,
        };
        Ok(crate::research::status_with_key_state(&settings, &row, key))
    }

    /// `research_source_options`: the research sources with what each needs for the
    /// provider in the settings: available or why not, the host that receives the queries,
    /// whether a key is needed and stored (never the key), which one to suggest, which is
    /// chosen and which the person's agreement covers.
    pub async fn research_source_options(&self) -> Result<Vec<SearchSourceOption>, CoreError> {
        let settings = self.inner.modeler.settings()?;
        let mut present = Vec::new();
        for service in SearchService::ALL {
            if self.search_key_status(service).await.unwrap_or(false) {
                present.push(service);
            }
        }
        let kind = self.inner.secrets.kind();
        let mut options =
            crate::research::source_options(&settings, |service| present.contains(&service));
        for option in &mut options {
            option.key_store = kind;
            // Only these stores read the environment; a supplied store never does.
            option.key_from_environment =
                matches!(kind, SecretStoreKind::Environment | SecretStoreKind::File)
                    && option.source.service().is_some_and(|service| {
                        crate::secrets::environment_has(&search_key_name(service))
                    });
        }
        Ok(options)
    }

    /// What kind of store keeps the keys (`environment` stores nothing).
    pub fn secret_store_kind(&self) -> SecretStoreKind {
        self.inner.secrets.kind()
    }

    /// `research_source_set`: chooses where web research comes from. An agreement given for
    /// another source does not cover the new one: until the person agrees again
    /// (`research_consent_for`), research is `needs_consent`. Only a person's own action may
    /// call this.
    pub async fn research_source_set(&self, source: SearchSource) -> Result<Settings, CoreError> {
        let mut settings = self.inner.modeler.settings()?;
        let before = settings.research.source;
        settings.research.source = source;
        self.settings_set(settings.clone()).await?;
        let _ = self.inner.store.audit(NewAudit {
            kind: "research",
            name: "source".to_owned(),
            outcome: source.as_str().to_owned(),
            detail: json!({"before": before.as_str()}),
            ..NewAudit::default()
        });
        Ok(settings)
    }

    /// `research_set`: replaces a project's research settings (on or off, its domains, its
    /// caps). Switching research on also clears a refusal the provider reported earlier,
    /// so the next message tries again.
    pub async fn research_set(
        &self,
        project_id: &str,
        project: ProjectResearch,
    ) -> Result<ResearchStatus, CoreError> {
        project.validate().map_err(CoreError::InvalidInput)?;
        let store = &self.inner.store;
        store.set_research_settings(project_id, &json!(project))?;
        if project.enabled == Some(true) {
            store.set_research_refused(project_id, false, false)?;
            store.set_research_refused(project_id, true, false)?;
        }
        let _ = store.audit(NewAudit {
            project_id: Some(project_id.to_owned()),
            kind: "research",
            name: "settings".to_owned(),
            outcome: "ok".to_owned(),
            detail: json!(project),
            ..NewAudit::default()
        });
        self.research_status(project_id).await
    }

    /// `research_consent`: records that the person agrees (or no longer agrees) that search
    /// queries and page addresses are sent to the AI provider. Without it no run searches.
    /// Only a person's own action may call this.
    ///
    /// The agreement is recorded for the source chosen now; see
    /// [`Self::research_consent_for`].
    pub async fn research_consent(&self, agreed: bool) -> Result<Settings, CoreError> {
        let source = self.inner.modeler.settings()?.research.source;
        self.research_consent_for(source, agreed).await
    }

    /// `research_consent_for`: records that the person agrees that search queries and page
    /// addresses go to `source` (the provider, Keenable or Tavily), or withdraws any
    /// agreement (`agreed: false`). An agreement covers that one source. Agreeing for a
    /// source that is not the chosen one is refused: the person read a dialog about another
    /// receiver than the one that would get the queries. Only a person's own action may call
    /// this.
    pub async fn research_consent_for(
        &self,
        source: SearchSource,
        agreed: bool,
    ) -> Result<Settings, CoreError> {
        let mut settings = self.inner.modeler.settings()?;
        if agreed && source != settings.research.source {
            return Err(CoreError::InvalidInput(format!(
                "the research source is {}, not {}: nothing was agreed to. Ask again for the \
                 source that is chosen",
                settings.research.source.as_str(),
                source.as_str()
            )));
        }
        if agreed && source == SearchSource::Off {
            return Err(CoreError::InvalidInput(
                "research is switched off in the settings (source `off`): there is nothing to \
                 agree to"
                    .to_owned(),
            ));
        }
        settings.research.acknowledged_at_ms = agreed.then(now_ms);
        settings.research.acknowledged_source = agreed.then_some(source);
        self.save_settings(&settings)?;
        let _ = self.inner.store.audit(NewAudit {
            kind: "research",
            name: "consent".to_owned(),
            outcome: if agreed { "agreed" } else { "withdrawn" }.to_owned(),
            detail: json!({"source": source.as_str()}),
            ..NewAudit::default()
        });
        Ok(settings)
    }

    /// `research_notes`: the numbers found on the web for a project, oldest first, in every
    /// state.
    pub async fn research_notes(&self, project_id: &str) -> Result<Vec<ResearchNote>, CoreError> {
        Ok(self.inner.store.research_notes(project_id)?)
    }

    /// `research_note_decide`: the person confirms a web number, rejects it, or takes the
    /// decision back (`confirmed`, `rejected`, `unconfirmed`). The assistant has no way to
    /// call this; it is told the decision with its next message.
    pub async fn research_note_decide(
        &self,
        project_id: &str,
        note_id: &str,
        state: &str,
    ) -> Result<ResearchNote, CoreError> {
        if !["confirmed", "rejected", "unconfirmed"].contains(&state) {
            return Err(CoreError::InvalidInput(
                "a web number can be `confirmed`, `rejected` or `unconfirmed`".to_owned(),
            ));
        }
        let store = &self.inner.store;
        let known = store.research_notes(project_id)?;
        match known.iter().find(|note| note.id == note_id) {
            Some(note) if matches!(note.state.as_str(), "superseded" | "expired") => {
                return Err(CoreError::InvalidInput(format!(
                    "note {note_id} is {}: a newer value replaced it; decide on that one",
                    note.state
                )));
            }
            _ => {}
        }
        let note = store.set_research_note_state(project_id, note_id, state)?;
        let _ = store.audit(NewAudit {
            project_id: Some(project_id.to_owned()),
            kind: "research",
            name: "note".to_owned(),
            outcome: state.to_owned(),
            detail: json!({"note": note.id, "key": note.key}),
            ..NewAudit::default()
        });
        Ok(note)
    }

    /// `research_sources`: the pages the provider found or read for a project, newest first.
    pub async fn research_sources(
        &self,
        project_id: &str,
    ) -> Result<Vec<ResearchSource>, CoreError> {
        Ok(self.inner.store.research_sources(project_id)?)
    }

    /// `research_log`: the searches and fetches of a project, oldest first.
    pub async fn research_log(&self, project_id: &str) -> Result<Vec<ResearchLogEntry>, CoreError> {
        Ok(self.inner.store.research_log(project_id)?)
    }

    async fn with_secrets<T: Send + 'static>(
        &self,
        action: impl FnOnce(&dyn SecretStore) -> Result<T, SecretError> + Send + 'static,
    ) -> Result<T, CoreError> {
        let secrets = Arc::clone(&self.inner.secrets);
        // Keychains answer over blocking IPC.
        tokio::task::spawn_blocking(move || action(secrets.as_ref()))
            .await
            .map_err(|e| CoreError::Setup(e.to_string()))?
            .map_err(CoreError::from)
    }

    fn audit_secret(&self, kind: ProviderKind, action: &str, outcome: &Result<(), CoreError>) {
        let _ = self.inner.store.audit(NewAudit {
            kind: "secret",
            name: kind.as_str().to_owned(),
            outcome: if outcome.is_ok() { "ok" } else { "failed" }.to_owned(),
            detail: json!({"action": action}),
            ..NewAudit::default()
        });
    }

    /// `provider_key_set`: stores a provider's API key. Write-only: no call returns it.
    pub async fn provider_key_set(&self, kind: ProviderKind, key: String) -> Result<(), CoreError> {
        let secret = Secret::new(key);
        let outcome = self
            .with_secrets(move |store| store.set(&provider_key_name(kind), secret))
            .await;
        self.audit_secret(kind, "set", &outcome);
        outcome
    }

    /// `provider_key_delete`: removes a provider's API key.
    pub async fn provider_key_delete(&self, kind: ProviderKind) -> Result<(), CoreError> {
        let outcome = self
            .with_secrets(move |store| store.delete(&provider_key_name(kind)))
            .await;
        self.audit_secret(kind, "delete", &outcome);
        outcome
    }

    /// `provider_key_status`: whether a provider's API key is stored. The key itself is
    /// never returned.
    pub async fn provider_key_status(&self, kind: ProviderKind) -> Result<bool, CoreError> {
        self.with_secrets(move |store| store.is_set(&provider_key_name(kind)))
            .await
    }

    fn audit_search_secret(
        &self,
        service: SearchService,
        action: &str,
        outcome: &Result<(), CoreError>,
    ) {
        let _ = self.inner.store.audit(NewAudit {
            kind: "secret",
            name: service.as_str().to_owned(),
            outcome: if outcome.is_ok() { "ok" } else { "failed" }.to_owned(),
            detail: json!({"action": action}),
            ..NewAudit::default()
        });
    }

    /// `search_key_set`: stores a search service's API key (Keenable, Tavily), in the same
    /// store as the model key. Write-only: no call returns it.
    pub async fn search_key_set(
        &self,
        service: SearchService,
        key: String,
    ) -> Result<(), CoreError> {
        let secret = Secret::new(key);
        let outcome = self
            .with_secrets(move |store| store.set(&search_key_name(service), secret))
            .await;
        self.audit_search_secret(service, "set", &outcome);
        outcome
    }

    /// `search_key_delete`: removes a search service's API key.
    pub async fn search_key_delete(&self, service: SearchService) -> Result<(), CoreError> {
        let outcome = self
            .with_secrets(move |store| store.delete(&search_key_name(service)))
            .await;
        self.audit_search_secret(service, "delete", &outcome);
        outcome
    }

    /// `search_key_status`: whether a search service's key is stored. Never the key.
    pub async fn search_key_status(&self, service: SearchService) -> Result<bool, CoreError> {
        self.with_secrets(move |store| store.is_set(&search_key_name(service)))
            .await
    }

    /// The client of the chosen search service for a run, with the key when one is stored.
    /// `None` for the provider's own search and for `off`: the key store is not asked.
    ///
    /// A key store that cannot be asked is [`KeyUnreadable`], for every service: the run
    /// then has no research through the service (its status is `needs_key`), and the rest
    /// of the message goes on. Keenable's keyless path is used only when no key is stored
    /// (with `HOTEND_SECRETS=env`: when `KEENABLE_API_KEY` is not set), never in place of
    /// a key that could not be read.
    async fn search_client(
        &self,
        source: SearchSource,
    ) -> Result<Option<SearchClient>, KeyUnreadable> {
        let Some(service) = source.service() else {
            return Ok(None);
        };
        let key = self
            .with_secrets(move |store| store.reveal(&search_key_name(service), &RevealToken::new()))
            .await
            .map_err(|_| KeyUnreadable)?;
        #[cfg(test)]
        if let Some(base) = self
            .inner
            .search_base_for_tests
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
        {
            return Ok(Some(SearchClient::with_base(service, key, &base)));
        }
        Ok(Some(SearchClient::new(service, key)))
    }

    // ---- the agent ----------------------------------------------------------------------

    /// `message_send`: sends a user message to the agent and returns the run's event stream.
    ///
    /// The call returns as soon as the agent child is started. The conversation history is
    /// stored when the run ends, just before the final [`RunEvent::Finished`]; a failed run
    /// stores nothing and ends with [`RunEvent::Failed`].
    pub async fn message_send(
        &self,
        project_id: &str,
        text: &str,
    ) -> Result<MessageRun, CoreError> {
        let inner = &self.inner;
        let text = text.trim();
        if text.is_empty() || text.len() > MAX_MESSAGE_BYTES {
            return Err(CoreError::InvalidInput(format!(
                "a message must have between 1 and {MAX_MESSAGE_BYTES} bytes"
            )));
        }
        let project = inner.store.project(project_id)?;
        let settings = inner.modeler.settings()?;
        let provider = settings.provider.clone().ok_or_else(|| {
            CoreError::NotConfigured(
                "choose an AI provider and a model in the settings first".to_owned(),
            )
        })?;
        let history = match inner.store.history(project_id)? {
            Some((kind, history)) if kind == provider.kind.as_str() => history,
            Some((kind, history)) if !history.is_empty() => {
                return Err(CoreError::InvalidInput(format!(
                    "this project's conversation was held with {kind}; switch the provider \
                     back or start a new project"
                )));
            }
            _ => Vec::new(),
        };
        // Held until the run is listed in `runs` (or the start failed): a second message to
        // this project meanwhile is answered `Busy`.
        let reservation = inner.reserve_run(&project.id)?;

        let run_id = new_id();
        // Whether this run may search the web, and what it is told about earlier finds. A
        // search service as the source needs its client (with the key, when one is stored);
        // the provider's own search needs nothing more.
        let search = self.search_client(settings.research.source).await;
        let research =
            crate::research::plan_with(&inner.store, &settings, project_id, &run_id, search, text)?;
        let grant = inner
            .egress
            .begin_run_with_effort(
                &run_id,
                Some(project_id),
                provider.kind,
                &provider.model,
                research.grant.clone(),
                Some(settings.effort),
            )
            .await?;
        let (sender, receiver) = broadcast::channel(EVENT_BUFFER);
        let mut own_events = sender.subscribe();
        let mut notices = inner.job_notices.subscribe();
        let started_with = history.clone();
        // Runs since the last one whose conversation was kept ended without it (an error, a
        // crash, a timeout): the model is told on this message what they left behind, so it
        // goes on from the project as it is. The note travels in the maker's new message, so
        // nothing sent before changes.
        let user_text = match failed_runs_note(&inner.store, &project)? {
            Some(note) => format!("{text}\n\n{note}"),
            None => text.to_owned(),
        };
        let request = RunRequest {
            run_id: run_id.clone(),
            project_id: project.id.clone(),
            provider: provider.kind,
            model: provider.model.clone(),
            base_url: grant.base_url.to_owned(),
            egress_token: grant.token,
            history,
            user_text,
            max_turns: settings.max_turns,
            research_tools: research.grant.is_some() || research.service.is_some(),
            web: research.service.clone(),
            research: research.agent,
            max_output_tokens: grant.max_output_tokens,
        };
        let run = match inner.supervisor.start(request, sender.clone()) {
            Ok(run) => run,
            Err(error) => {
                inner.egress.end_run(&run_id);
                return Err(error.into());
            }
        };
        inner
            .store
            .append_transcript(project_id, &run_id, "user", text)?;
        let cancel = run.cancel_handle();

        let research_sink = research.sink;
        let research_service = research.service;
        let research_shown = research.shown;
        let research_keeps_pdf = research.status.project.allow_pdf;
        let research_state = research.status.state;
        let research_key = research.key;
        let weak = Arc::downgrade(inner);
        let all_events = inner.events.clone();
        let (store, egress) = (Arc::clone(&inner.store), Arc::clone(&inner.egress));
        let (project_id, id) = (project.id.clone(), run_id.clone());
        // Hold the map lock across the spawn so the task cannot finish before it is listed.
        let mut runs = inner.runs();
        let task = tokio::spawn(async move {
            let mut said = String::new();
            let forward = |event: RunEvent| {
                let _ = all_events.send(CoreEvent::Run {
                    run_id: id.clone(),
                    project_id: project_id.clone(),
                    event,
                });
            };
            let mut note = |event: &RunEvent| {
                if let Some(text) = text_of(event)
                    && said.len() + text.len() <= MAX_TRANSCRIPT_TEXT
                {
                    said.push_str(text);
                }
                forward(event.clone());
            };
            // A notice of the job runner about this run (a restarted engine worker) is shown
            // to the person like the agent's text and kept in the transcript with it.
            let notice_event = |notice: JobNotice| {
                (notice.run_id.as_deref() == Some(id.as_str())).then(|| RunEvent::Agent {
                    event: json!({"type": "text_delta", "text": format!("\n\n{}\n\n", notice.text)}),
                })
            };
            let mut notices_open = true;
            let wait = run.wait();
            tokio::pin!(wait);
            let outcome = loop {
                tokio::select! {
                    outcome = &mut wait => break outcome,
                    event = own_events.recv() => if let Ok(event) = event { note(&event) },
                    notice = notices.recv(), if notices_open => match notice {
                        Ok(notice) => if let Some(event) = notice_event(notice) {
                            let _ = sender.send(event);
                        },
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => notices_open = false,
                    },
                }
            };
            while let Ok(notice) = notices.try_recv() {
                if let Some(event) = notice_event(notice) {
                    let _ = sender.send(event);
                }
            }
            while let Ok(event) = own_events.try_recv() {
                note(&event);
            }
            let totals = egress.end_run(&id);
            let mut web = totals.research;
            // Through a search service the core made the calls itself and counted them.
            if let Some(service) = &research_service {
                let used = service.used();
                web.searches += used.searches;
                web.fetches += used.fetches;
                web.result_bytes += used.result_bytes;
            }
            let usage = json!({"llm_requests": totals.requests,
                "refused_requests": totals.refused,
                "request_bytes": totals.request_bytes, "response_bytes": totals.response_bytes,
                "web_searches": web.searches, "web_fetches": web.fetches,
                "billed_web_searches": web.billed_searches,
                "web_result_bytes": web.result_bytes,
                "research": research_state,
                "sandbox": crate::jobs::sandbox_detail()});
            // A response held a web result Hotend does not keep, and the agent is to take
            // it out of the conversation. When it is still there, the conversation of this
            // run is not stored: the egress would refuse to send it again.
            let outcome = outcome.and_then(|outcome| {
                let left = totals.research_block.and_then(|_| {
                    crate::research::unkept_in_history(
                        &outcome.history,
                        research_keeps_pdf,
                        settings.research.max_result_bytes,
                    )
                });
                match left {
                    Some(block) => Err(AgentError::WebResultNotKept {
                        block,
                        page: crate::research::unkept_page(&store, &project_id, &id),
                    }),
                    None => Ok(outcome),
                }
            });
            // The transcript is what the user saw; the history is what the agent will be
            // given next time. A run whose conversation does not reach the history (it
            // failed, reported an error or was stopped before it answered) keeps its
            // lines in the transcript, marked as not remembered.
            let remembered = outcome.as_ref().is_ok_and(|outcome| {
                outcome.stop_reason != StopReason::Error && outcome.history != started_with
            });
            let stored = outcome
                .map_err(|e| (e.code().to_owned(), e.to_string()))
                .and_then(|outcome| {
                    if outcome.stop_reason != StopReason::Error {
                        store
                            .set_history(&project_id, provider.kind.as_str(), &outcome.history)
                            .map_err(|e| ("storage_error".to_owned(), e.to_string()))?;
                        // The model has now seen the notes it was given, and the next run
                        // learns which instructions and tools this history was made with.
                        let _ = crate::research::remember_run(
                            &store,
                            &project_id,
                            &research_shown,
                            outcome.shape.as_deref(),
                        );
                    }
                    Ok(outcome)
                });
            // What the run looked up, for the person: counts, the fee and the pages.
            if let Some(sink) = &research_sink {
                let sources = sink.sources();
                if web.searches + web.fetches > 0 || !sources.is_empty() {
                    let event = RunEvent::Agent {
                        event: research_event(
                            &store,
                            &settings,
                            research_key,
                            &project_id,
                            &web,
                            &sources,
                        ),
                    };
                    forward(event.clone());
                    let _ = sender.send(event);
                }
            }
            if !said.trim().is_empty() {
                let _ = store.append_transcript(&project_id, &id, "assistant", said.trim());
            }
            if !remembered || stored.is_err() {
                let _ = store.mark_run_not_remembered(&project_id, &id);
            }
            let (code, event) = match stored {
                Ok(outcome) => (
                    format!("{:?}", outcome.stop_reason).to_lowercase(),
                    RunEvent::Finished {
                        stop_reason: outcome.stop_reason,
                        error: outcome.error.map(|e| truncate(&e, 2000)),
                    },
                ),
                Err((code, message)) => (
                    code.clone(),
                    RunEvent::Failed {
                        code,
                        message: truncate(&message, 2000),
                    },
                ),
            };
            forward(event.clone());
            let _ = store.audit(NewAudit {
                project_id: Some(project_id.clone()),
                run_id: Some(id.clone()),
                kind: "agent",
                name: provider.kind.as_str().to_owned(),
                args_sha256: None,
                outcome: code,
                detail: usage,
            });
            if let Some(inner) = weak.upgrade() {
                inner.runs().remove(&id);
            }
            let _ = sender.send(event);
        });
        runs.insert(
            run_id.clone(),
            ActiveRun {
                project_id: project.id,
                cancel,
                task,
            },
        );
        drop(runs);
        drop(reservation);
        Ok(MessageRun {
            run_id,
            events: receiver,
            done: false,
        })
    }

    /// `run_cancel`: asks a run to stop. Returns false when no such run is in progress.
    /// The run's stream still ends with its final event.
    pub async fn run_cancel(&self, run_id: &str) -> bool {
        match self.inner.runs().get(run_id) {
            Some(run) => {
                run.cancel.cancel();
                true
            }
            None => false,
        }
    }
}

/// Longest part of an earlier message quoted in [`failed_runs_note`].
const QUOTED_MESSAGE_CHARS: usize = 200;

/// The note for the agent about the runs since the last one whose conversation was kept:
/// which of the maker's messages they answered and which versions they made, and the
/// project's current version. `None` when the last run was kept (or there was none).
///
/// Such a run left the stored conversation as it was (its answer is in the transcript,
/// marked as not remembered), but the versions it built are real and the current version
/// may be one of them; without this the model would go on from a version it never saw.
fn failed_runs_note(store: &Store, project: &Project) -> Result<Option<String>, CoreError> {
    let transcript = store.transcript(&project.id)?;
    // The runs at the end of the transcript none of whose lines were remembered, oldest
    // first.
    let mut runs: Vec<(String, String)> = Vec::new();
    let mut index = transcript.len();
    while index > 0 {
        let run_id = transcript[index - 1].run_id.clone();
        let start = transcript[..index]
            .iter()
            .rposition(|entry| entry.run_id != run_id)
            .map_or(0, |at| at + 1);
        let lines = &transcript[start..index];
        if lines.iter().any(|entry| entry.remembered) {
            break;
        }
        let asked = lines
            .iter()
            .find(|entry| entry.role == "user")
            .map(|entry| entry.text.clone())
            .unwrap_or_default();
        runs.insert(0, (run_id, asked));
        index = start;
    }
    if runs.is_empty() {
        return Ok(None);
    }
    // Only the newest runs are named; the versions of the others are in the project.
    let omitted = runs.len().saturating_sub(NOTED_FAILED_RUNS);
    let mut named = Vec::new();
    for (run_id, asked) in &runs[omitted..] {
        let versions = store.versions_made_in_run(&project.id, run_id)?;
        named.push((asked.clone(), versions));
    }
    Ok(Some(failed_runs_text(
        &named,
        omitted,
        project.current_version,
    )))
}

/// The most failed runs [`failed_runs_note`] names (the newest), and the longest the note
/// may be, in characters: it goes into every run until a run is kept again.
const NOTED_FAILED_RUNS: usize = 3;
const MAX_FAILED_RUNS_NOTE_CHARS: usize = 1500;
/// The most version numbers named for one run.
const NOTED_VERSIONS: usize = 8;

/// The text of [`failed_runs_note`]: `runs` (oldest first) with the maker's message and the
/// versions each made, `omitted` older runs only counted.
fn failed_runs_text(runs: &[(String, Vec<u32>)], omitted: usize, current: Option<u32>) -> String {
    let head = "[Hotend note: the answer to the following message(s) ended with an error before \
                its conversation was kept, so it is not in this conversation. The project kept \
                what they built:";
    let earlier = (omitted > 0).then(|| {
        format!(
            "- and {omitted} more earlier message(s) whose answers ended the same way \
             (their versions are in the project's history)"
        )
    });
    let tail = match current {
        Some(number) => format!(
            "The current version is {number}. Look at it before you change it, and tell the \
             maker in one sentence that the earlier answer was cut off.]"
        ),
        None => "The project has no version yet. Tell the maker in one sentence that the \
                 earlier answer was cut off.]"
            .to_owned(),
    };
    let fixed = head.chars().count()
        + earlier.as_ref().map_or(0, |line| line.chars().count() + 1)
        + tail.chars().count()
        + 1;
    // What is left is shared by the named runs: a line each, its quote cut to fit.
    let per_run = MAX_FAILED_RUNS_NOTE_CHARS
        .saturating_sub(fixed)
        .checked_div(runs.len())
        .unwrap_or(0);
    let mut lines = vec![head.to_owned()];
    lines.extend(earlier);
    for (asked, versions) in runs {
        let made = match versions.as_slice() {
            [] => "no new version".to_owned(),
            [one] => format!("version {one}"),
            many => {
                let shown = &many[many.len().saturating_sub(NOTED_VERSIONS)..];
                let listed = shown
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                match many.len() - shown.len() {
                    0 => format!("versions {listed}"),
                    more => format!("versions {listed} and {more} before them"),
                }
            }
        };
        // `- "` + quote + `...": ` + made + the line break.
        let room = per_run
            .saturating_sub(made.chars().count() + 9)
            .min(QUOTED_MESSAGE_CHARS);
        let flat = asked.replace('\n', " ");
        let quoted: String = flat.chars().take(room).collect();
        let more = if flat.chars().count() > room {
            "..."
        } else {
            ""
        };
        lines.push(format!("- \"{quoted}{more}\": {made}"));
    }
    lines.push(tail);
    lines.join("\n")
}

/// The summary of what one run looked up on the web, sent as an event of type `research`
/// just before the run's last event (docs/WEB_RESEARCH.md). It comes from the core's own
/// counters and stored pages, never from the agent. `key` is what the key store said when
/// the run was planned: the project's state is the one the run had, a stored key included.
fn research_event(
    store: &Store,
    settings: &Settings,
    key: SearchKeyState,
    project_id: &str,
    used: &crate::egress::ResearchUse,
    sources: &[(String, String)],
) -> serde_json::Value {
    let provider = settings.provider.as_ref().map(|provider| provider.kind);
    let price = provider
        .and_then(|kind| kind.rule().body.research)
        .map_or(0, |rule| u64::from(rule.search_price_micro_usd));
    let project = store
        .research_row(project_id)
        .ok()
        .map(|row| crate::research::status_with_key_state(settings, &row, key));
    json!({
        "type": "research",
        "source": settings.research.source.as_str(),
        "searches": used.searches,
        "fetches": used.fetches,
        "billed_searches": used.billed_searches,
        "search_fee_micro_usd": u64::from(used.billed_searches) * price,
        "result_bytes": used.result_bytes,
        "result_tokens_estimate": used.result_bytes / 4,
        "sources": sources.iter().map(|(url, title)| json!({"url": url, "title": title}))
            .collect::<Vec<_>>(),
        "project": project,
    })
}

#[cfg(test)]
mod failed_runs_note_tests {
    use super::*;

    #[test]
    fn the_note_names_the_newest_runs_only_and_stays_short() {
        let runs: Vec<(String, Vec<u32>)> = (1..=10)
            .map(|n| {
                (
                    format!("message {n}: {}", "a long request ".repeat(40)),
                    (n * 100..n * 100 + 30).collect(),
                )
            })
            .collect();
        let omitted = runs.len() - NOTED_FAILED_RUNS;
        let note = failed_runs_text(&runs[omitted..], omitted, Some(1030));
        assert!(note.chars().count() <= MAX_FAILED_RUNS_NOTE_CHARS, "{note}");
        assert!(note.contains("and 7 more"), "{note}");
        for n in 8..=10 {
            assert!(note.contains(&format!("message {n}:")), "{note}");
        }
        assert!(!note.contains("message 7:"), "{note}");
        // The newest version a run made is named; the many before it are counted.
        assert!(
            note.contains("1029") && note.contains("before them"),
            "{note}"
        );
        assert!(note.ends_with("cut off.]"), "{note}");

        // A single short run is quoted in full, as before.
        let one = failed_runs_text(&[("a box".to_owned(), vec![3])], 0, Some(3));
        assert!(one.contains("- \"a box\": version 3"), "{one}");
        assert!(!one.contains("more"), "{one}");
    }
}

#[cfg(test)]
mod open_store_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn a_folder_with_projects_and_no_database_is_refused_and_left_as_it_is() {
        let tmp = tempfile::tempdir().unwrap();
        let data = DataDir::resolve(Some(tmp.path())).unwrap();
        let version = data.version_dir(&new_id(), 1).unwrap();
        std::fs::create_dir_all(&version).unwrap();
        let error = open_store(&data).unwrap_err().to_string();
        assert!(error.contains("holds projects but no database"), "{error}");
        assert!(!data.db_path().exists());
        assert!(!data.root().join(crate::sweep::LOCK_FILE).exists());
        // An empty folder is a new one, and one with its database opens as before.
        std::fs::remove_dir_all(data.root().join("projects")).unwrap();
        std::fs::create_dir_all(data.root().join("projects")).unwrap();
        drop(open_store(&data).unwrap());
        std::fs::create_dir_all(&version).unwrap();
        drop(open_store(&data).unwrap());
    }
}
