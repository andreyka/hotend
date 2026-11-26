//! The shell: every command the window can call, as methods on one value.
//!
//! The Tauri commands (`ipc`) and the development bridge both call these methods and
//! nothing else, so the two transports cannot differ in what they allow. Each method is a
//! thin layer over `hotend_core::Core`; what the shell adds is listed here because it is
//! all security:
//!
//! - `print_confirm` and `print_project_confirm` go through [`crate::confirm`]: the native
//!   dialog, and the confirmation authority that never leaves that module. No other
//!   method delivers a print job or hands over an editable project.
//! - A path from the window (an export folder, a folder for a project, a slicer program,
//!   a new hand-off folder in the settings) is accepted only if the person picked it in a
//!   system dialog of this session ([`crate::grants`]). `snapshot_export` takes only a
//!   folder picked under the snapshot export picker's own title.
//! - A link is opened by id from a fixed table ([`crate::links`]).
//! - `printer_certificate_check` pins a printer's certificate only through
//!   [`crate::confirm`]'s native dialog, with the fingerprint of the first look the shell
//!   took itself; no method takes a fingerprint, and the core's `printer_certificate_confirm`
//!   is called from nowhere else.
//! - `research_consent_ask` records the agreement to web research only through
//!   [`crate::confirm`]'s native dialog, for the chosen source only; `settings_set` keeps the
//!   core's research settings whatever the window sends.
//! - No method returns a secret: `provider_key_set` and `search_key_set` take a key and
//!   return nothing, `provider_key_status` and `search_key_status` return a boolean;
//!   `printer_lan_key_set` takes the printer's access code and returns nothing, and
//!   `printer_lan_get` says only whether one is there.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use hotend_core::engine::Section;
use hotend_core::{
    ApiError, AuditEntry, BuildReport, Calibration, CalibrationUpdate, CardRef, Core,
    DeliveryChoice, DoctorReport, EditableProject, EngineResult, ExportFormat, ExportedFile,
    GaugeExport, GaugeReading, HandOffStatus, LanPrinterStatus, Preparation, PrepareOptions,
    PrintJob, PrintReview, PrinterInfo, PrinterLanSetup, Project, ProjectDestination,
    ProjectDetail, ProjectReview, ProviderKind, Question, RenderedView, SearchService,
    SearchSource, SearchSourceOption, Settings, SliceOutcome, SlicerInstallation, Snapshot,
    SnapshotExport, Version, VersionDetail,
};
use serde::Serialize;
use serde_json::Value;

use crate::confirm::{
    CertificateConfirmation, ConfirmationPrompt, PrintConfirmation, ProjectConfirmation,
    ResearchConsentAnswer, ReviewLock,
};
use crate::grants::{self, PathGrants, Picked, Pickers};
use crate::links::LinkOpener;
use crate::wire::LinkId;

/// What `message_send` answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MessageStarted {
    pub run_id: String,
}

/// What `printer_lan_get` answers: the core's setup, which says where the access code would
/// come from, and whether one is there at all (`access_code_present`). Never the code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LanSetup {
    #[serde(flatten)]
    pub setup: PrinterLanSetup,
    /// True when an access code is available (from the environment or the secret store).
    pub access_code_present: bool,
}

impl LanSetup {
    pub fn from_core(setup: PrinterLanSetup) -> Self {
        let access_code_present =
            matches!(setup.access_code.as_str(), "environment" | "secret_store");
        Self {
            setup,
            access_code_present,
        }
    }
}

/// The parts of the shell that touch the desktop. The application gives the native ones;
/// the bridge and the tests give stand-ins.
pub struct Desktop {
    pub prompt: Arc<dyn ConfirmationPrompt>,
    /// Whether `prompt` reaches a person (or, in a test, a scripted answer).
    pub can_confirm: bool,
    pub pickers: Arc<dyn Pickers>,
    pub links: Arc<dyn LinkOpener>,
}

/// The shell. `core` is an error when the core could not be opened; every command then
/// answers with that error, except the doctor report, which says why.
pub struct Shell {
    core: Result<Core, ApiError>,
    desktop: Desktop,
    grants: PathGrants,
    reviews: ReviewLock,
}

type Outcome<T> = Result<T, ApiError>;

impl Shell {
    pub fn new(core: Result<Core, ApiError>, desktop: Desktop) -> Self {
        Self {
            core,
            desktop,
            grants: PathGrants::default(),
            reviews: ReviewLock::default(),
        }
    }

    /// The core, or why it could not be opened.
    pub fn core(&self) -> Outcome<&Core> {
        self.core.as_ref().map_err(Clone::clone)
    }

    // ---- projects and versions --------------------------------------------------------

    pub async fn projects_list(&self) -> Outcome<Vec<Project>> {
        Ok(self.core()?.projects_list().await?)
    }

    pub async fn project_create(&self, name: String) -> Outcome<Project> {
        Ok(self.core()?.project_create(&name).await?)
    }

    pub async fn project_get(&self, project_id: String) -> Outcome<ProjectDetail> {
        Ok(self.core()?.project_get(&project_id).await?)
    }

    pub async fn versions_list(&self, project_id: String) -> Outcome<Vec<Version>> {
        Ok(self.core()?.versions_list(&project_id).await?)
    }

    pub async fn version_get(&self, project_id: String, number: u32) -> Outcome<VersionDetail> {
        Ok(self.core()?.version_get(&project_id, number).await?)
    }

    /// The text of the version's `mesh.json`.
    pub async fn version_mesh(&self, project_id: String, number: u32) -> Outcome<String> {
        let bytes = self.core()?.version_mesh(&project_id, number).await?;
        String::from_utf8(bytes).map_err(|_| ApiError::Storage {
            message: format!("the mesh of version {number} is not text"),
            next_step: "Build the version again.".to_owned(),
        })
    }

    pub async fn version_revert(&self, project_id: String, number: u32) -> Outcome<Version> {
        Ok(self.core()?.version_revert(&project_id, number).await?)
    }

    pub async fn questions_list(
        &self,
        project_id: String,
        version: Option<u32>,
    ) -> Outcome<Vec<Question>> {
        Ok(self.core()?.questions_list(&project_id, version).await?)
    }

    pub async fn measure_run(
        &self,
        project_id: String,
        queries: Vec<Value>,
    ) -> Outcome<Vec<Value>> {
        Ok(self.core()?.measure_run(&project_id, queries).await?)
    }

    pub async fn model_build(
        &self,
        project_id: String,
        code: String,
        params: BTreeMap<String, f64>,
        note: String,
    ) -> Outcome<BuildReport> {
        Ok(self
            .core()?
            .model_build(&project_id, &code, params, &note)
            .await?)
    }

    pub async fn parameter_set(
        &self,
        project_id: String,
        name: String,
        value: f64,
    ) -> Outcome<BuildReport> {
        Ok(self
            .core()?
            .parameter_set(&project_id, &name, value)
            .await?)
    }

    pub async fn checks_run(
        &self,
        project_id: String,
        checks: Option<Vec<String>>,
        sample_ranges: bool,
    ) -> Outcome<EngineResult> {
        Ok(self
            .core()?
            .checks_run(&project_id, checks, sample_ranges)
            .await?)
    }

    pub async fn views_render(
        &self,
        project_id: String,
        views: Vec<String>,
        section: Option<Section>,
        size: [u32; 2],
    ) -> Outcome<Vec<RenderedView>> {
        Ok(self
            .core()?
            .views_render(&project_id, views, section, size)
            .await?)
    }

    /// Writes files of a version into a folder the person picked.
    pub async fn export_run(
        &self,
        project_id: String,
        version: Option<u32>,
        formats: Vec<ExportFormat>,
        parts: Option<Vec<String>>,
        destination: String,
    ) -> Outcome<Vec<ExportedFile>> {
        let core = self.core()?;
        let destination = PathBuf::from(destination);
        self.grants.check(Picked::Folder, &destination)?;
        Ok(core
            .export_run(&project_id, version, formats, parts, &destination)
            .await?)
    }

    // ---- snapshots ------------------------------------------------------------------------

    pub async fn snapshots_list(&self, project_id: String) -> Outcome<Vec<Snapshot>> {
        Ok(self.core()?.snapshots_list(&project_id).await?)
    }

    /// Names a version (the current one by default). Changes no version.
    pub async fn snapshot_create(
        &self,
        project_id: String,
        name: String,
        version: Option<u32>,
        note: String,
    ) -> Outcome<Snapshot> {
        Ok(self
            .core()?
            .snapshot_create(&project_id, &name, version, &note)
            .await?)
    }

    /// Removes the name only: the version and its files stay.
    pub async fn snapshot_delete(&self, project_id: String, name: String) -> Outcome<Snapshot> {
        Ok(self.core()?.snapshot_delete(&project_id, &name).await?)
    }

    /// Makes the snapshot's version the current one again.
    pub async fn snapshot_restore(&self, project_id: String, name: String) -> Outcome<Version> {
        Ok(self.core()?.snapshot_restore(&project_id, &name).await?)
    }

    /// Writes a snapshot's files into a new folder below `folder`, which must be a folder
    /// the person picked under the snapshot export picker's own title
    /// ([`grants::SNAPSHOT_TITLE`]); a folder picked for anything else is refused.
    pub async fn snapshot_export(
        &self,
        project_id: String,
        name: String,
        folder: String,
    ) -> Outcome<SnapshotExport> {
        let core = self.core()?;
        let folder = PathBuf::from(folder);
        self.grants.check(Picked::SnapshotFolder, &folder)?;
        Ok(core.snapshot_export(&project_id, &name, &folder).await?)
    }

    // ---- preparation, gauges, calibration ---------------------------------------------

    pub async fn prepare_run(
        &self,
        project_id: String,
        version: Option<u32>,
        options: PrepareOptions,
    ) -> Outcome<Preparation> {
        Ok(self
            .core()?
            .prepare_run(&project_id, version, options)
            .await?)
    }

    pub async fn preparation_get(
        &self,
        project_id: String,
        version: u32,
    ) -> Outcome<Option<Preparation>> {
        Ok(self.core()?.preparation_get(&project_id, version).await?)
    }

    /// Writes gauge plates into a folder the person picked.
    pub async fn gauge_export(
        &self,
        project_id: Option<String>,
        cards: Option<Vec<CardRef>>,
        nozzle_id: Option<i64>,
        destination: String,
    ) -> Outcome<GaugeExport> {
        let core = self.core()?;
        let destination = PathBuf::from(destination);
        self.grants.check(Picked::Folder, &destination)?;
        Ok(core
            .gauge_export(project_id.as_deref(), cards, nozzle_id, &destination)
            .await?)
    }

    pub async fn calibration_get(&self) -> Outcome<Calibration> {
        Ok(self.core()?.calibration_get().await?)
    }

    pub async fn calibration_record(
        &self,
        readings: Vec<GaugeReading>,
        nozzle_id: Option<i64>,
    ) -> Outcome<CalibrationUpdate> {
        Ok(self.core()?.calibration_record(readings, nozzle_id).await?)
    }

    // ---- slicing ------------------------------------------------------------------------

    pub async fn slice_run(
        &self,
        project_id: String,
        profile: String,
        plate: Option<u32>,
    ) -> Outcome<SliceOutcome> {
        Ok(self.core()?.slice_run(&project_id, &profile, plate).await?)
    }

    pub async fn slice_get(
        &self,
        project_id: String,
        version: Option<u32>,
    ) -> Outcome<Option<SliceOutcome>> {
        Ok(self.core()?.slice_get(&project_id, version).await?)
    }

    pub async fn slicers_list(&self) -> Outcome<Vec<SlicerInstallation>> {
        Ok(self.core()?.slicers_list().await?)
    }

    /// Makes a slicer the one in use; `executable` must be a program the person picked.
    pub async fn slicer_choose(
        &self,
        kind: String,
        executable: Option<String>,
    ) -> Outcome<Settings> {
        let core = self.core()?;
        let executable = executable.map(PathBuf::from);
        if let Some(program) = &executable {
            self.grants.check(Picked::Program, program)?;
        }
        Ok(core.slicer_choose(&kind, executable).await?)
    }

    // ---- printing -----------------------------------------------------------------------

    pub async fn printers_list(&self) -> Outcome<Vec<PrinterInfo>> {
        Ok(self.core()?.printers_list().await?)
    }

    pub async fn print_propose(&self, project_id: String, note: String) -> Outcome<PrintJob> {
        Ok(self.core()?.print_propose(&project_id, &note).await?)
    }

    pub async fn print_jobs(&self, project_id: String) -> Outcome<Vec<PrintJob>> {
        Ok(self.core()?.print_jobs(&project_id).await?)
    }

    pub async fn print_status(&self, job_id: String) -> Outcome<PrintJob> {
        Ok(self.core()?.print_status(&job_id).await?)
    }

    /// The confirmation summary of a pending job. Refused while a confirmation dialog is
    /// open, and for a folder the person did not pick.
    pub async fn print_review(
        &self,
        job_id: String,
        delivery: DeliveryChoice,
    ) -> Outcome<PrintReview> {
        let core = self.core()?;
        self.check_delivery(&delivery)?;
        let _no_dialog_open = self.reviews.try_hold()?;
        Ok(core.print_review(&job_id, delivery).await?)
    }

    /// Shows the native confirmation for a pending job and delivers the job only when the
    /// person confirms there. See [`crate::confirm`].
    pub async fn print_confirm(
        &self,
        job_id: String,
        delivery: DeliveryChoice,
    ) -> Outcome<PrintConfirmation> {
        let core = self.core()?;
        self.check_delivery(&delivery)?;
        crate::confirm::confirm_print(
            core,
            &self.desktop.prompt,
            self.desktop.can_confirm,
            &self.reviews,
            &job_id,
            delivery,
        )
        .await
    }

    pub async fn print_cancel(&self, job_id: String) -> Outcome<PrintJob> {
        Ok(self.core()?.print_cancel(&job_id).await?)
    }

    fn check_delivery(&self, delivery: &DeliveryChoice) -> Outcome<()> {
        match delivery {
            DeliveryChoice::ExportToFolder { folder } => self.grants.check(Picked::Folder, folder),
            DeliveryChoice::SimulatedPrinter | DeliveryChoice::BambuConnect => Ok(()),
        }
    }

    // ---- a real printer's status (read-only) -------------------------------------------

    /// How reading the printer's status is set up. Never the access code.
    pub async fn printer_lan_get(&self) -> Outcome<LanSetup> {
        Ok(LanSetup::from_core(self.core()?.printer_lan_get().await?))
    }

    /// The printer's address (a private-range IPv4 address) and serial number.
    pub async fn printer_lan_set(&self, ip: String, serial: String) -> Outcome<LanSetup> {
        Ok(LanSetup::from_core(
            self.core()?.printer_lan_set(&ip, &serial).await?,
        ))
    }

    pub async fn printer_lan_clear(&self) -> Outcome<()> {
        Ok(self.core()?.printer_lan_clear().await?)
    }

    /// Stores the printer's LAN access code. Write-only: nothing returns it;
    /// `printer_lan_get` says only whether one is there.
    pub async fn printer_lan_key_set(&self, code: String) -> Outcome<()> {
        Ok(self.core()?.printer_lan_key_set(code).await?)
    }

    pub async fn printer_lan_key_delete(&self) -> Outcome<()> {
        Ok(self.core()?.printer_lan_key_delete().await?)
    }

    /// Takes a first look at the printer's certificate and shows it in the native dialog;
    /// it is pinned only when the person confirms there. See [`crate::confirm`].
    pub async fn printer_certificate_check(&self) -> Outcome<CertificateConfirmation> {
        let core = self.core()?;
        crate::confirm::confirm_certificate(
            core,
            &self.desktop.prompt,
            self.desktop.can_confirm,
            &self.reviews,
        )
        .await
    }

    pub async fn printer_certificate_forget(&self) -> Outcome<()> {
        Ok(self.core()?.printer_certificate_forget().await?)
    }

    /// Follows the printer with the core's read-only listener (it cannot publish); every
    /// report arrives on the event stream (channel `printer`).
    pub async fn printer_watch_start(&self) -> Outcome<()> {
        Ok(self.core()?.printer_watch_start().await?)
    }

    /// Ends the session; whether one was running.
    pub async fn printer_watch_stop(&self) -> Outcome<bool> {
        Ok(self.core()?.printer_watch_stop().await)
    }

    /// The latest status of the session, while one runs or after it ended.
    pub async fn printer_watch_latest(&self) -> Outcome<Option<LanPrinterStatus>> {
        Ok(self.core()?.printer_watch_latest())
    }

    // ---- editable Bambu Studio projects -------------------------------------------------

    /// The hand-off as it is set up now: the folder, its Windows form, what opens a file,
    /// and why no hand-off can be made, if none can.
    pub async fn handoff_status(&self) -> Outcome<HandOffStatus> {
        Ok(self.core()?.handoff_status().await?)
    }

    /// Makes the editable Bambu Studio project of one plate of the current version. Nothing
    /// is opened or handed over.
    pub async fn print_project_make(
        &self,
        project_id: String,
        plate: Option<u32>,
    ) -> Outcome<EditableProject> {
        Ok(self.core()?.print_project_make(&project_id, plate).await?)
    }

    pub async fn print_projects(&self, project_id: String) -> Outcome<Vec<EditableProject>> {
        Ok(self.core()?.print_projects(&project_id).await?)
    }

    /// What a person must see before a project is handed over. Refused while a
    /// confirmation dialog is open, and for a folder the person did not pick.
    pub async fn print_project_review(
        &self,
        project_export_id: String,
        destination: ProjectDestination,
    ) -> Outcome<ProjectReview> {
        let core = self.core()?;
        self.check_destination(&destination)?;
        let _no_dialog_open = self.reviews.try_hold()?;
        Ok(core
            .print_project_review(&project_export_id, destination)
            .await?)
    }

    /// Shows the native confirmation for handing a project over and hands it over only
    /// when the person confirms there. See [`crate::confirm`].
    pub async fn print_project_confirm(
        &self,
        project_export_id: String,
        destination: ProjectDestination,
    ) -> Outcome<ProjectConfirmation> {
        let core = self.core()?;
        self.check_destination(&destination)?;
        crate::confirm::confirm_project(
            core,
            &self.desktop.prompt,
            self.desktop.can_confirm,
            &self.reviews,
            &project_export_id,
            destination,
        )
        .await
    }

    fn check_destination(&self, destination: &ProjectDestination) -> Outcome<()> {
        match destination {
            ProjectDestination::Folder { folder } => self.grants.check(Picked::Folder, folder),
            ProjectDestination::BambuStudio => Ok(()),
        }
    }

    // ---- settings, keys, audit, doctor --------------------------------------------------

    pub async fn audit_log(
        &self,
        project_id: Option<String>,
        limit: u32,
    ) -> Outcome<Vec<AuditEntry>> {
        Ok(self.core()?.audit_log(project_id.as_deref(), limit).await?)
    }

    pub async fn settings_get(&self) -> Outcome<Settings> {
        Ok(self.core()?.settings_get().await?)
    }

    /// Saves the settings. A slicer program that is not already in the settings must be
    /// one the person picked, and so must a hand-off folder that is not the one in the
    /// settings now (clearing it is allowed).
    pub async fn settings_set(&self, mut settings: Settings) -> Outcome<()> {
        let core = self.core()?;
        let current = core.settings_get().await?;
        // Web research is not this command's to change. Its settings hold the person's
        // consent that queries leave this computer (`acknowledged_at_ms`), which only a
        // person gives, at a terminal (`hotend research on`) or in the shell's own dialog
        // (`research_consent_ask`); a page that could write it would switch research on by
        // itself. The core's current research settings are kept whatever the window sends.
        settings.research = current.research.clone();
        for (kind, program) in &settings.slicer_executables {
            if current.slicer_executables.get(kind) != Some(program) {
                self.grants.check(Picked::Program, program)?;
            }
        }
        // The hand-off folder is where Hotend writes the checked file and the editable
        // project: a page that could name it would choose where Hotend writes. It must be
        // a folder picked under the hand-off picker's own title; a folder picked for an
        // export is not taken for it.
        if let Some(folder) = &settings.handoff_folder
            && current.handoff_folder.as_ref() != Some(folder)
        {
            self.grants.check(Picked::HandOffFolder, folder)?;
        }
        Ok(core.settings_set(settings).await?)
    }

    /// Stores a provider key. Write-only: nothing returns it.
    pub async fn provider_key_set(&self, kind: ProviderKind, key: String) -> Outcome<()> {
        Ok(self.core()?.provider_key_set(kind, key).await?)
    }

    pub async fn provider_key_delete(&self, kind: ProviderKind) -> Outcome<()> {
        Ok(self.core()?.provider_key_delete(kind).await?)
    }

    pub async fn provider_key_status(&self, kind: ProviderKind) -> Outcome<bool> {
        Ok(self.core()?.provider_key_status(kind).await?)
    }

    // ---- where web research comes from ---------------------------------------------------

    /// The research sources: available or why not, who receives the queries, whether a key
    /// is stored (never the key), which is chosen and which the agreement covers.
    pub async fn research_source_options(&self) -> Outcome<Vec<SearchSourceOption>> {
        Ok(self.core()?.research_source_options().await?)
    }

    /// Chooses where web research comes from. An agreement given for another source does
    /// not cover the new one.
    pub async fn research_source_set(&self, source: SearchSource) -> Outcome<Settings> {
        Ok(self.core()?.research_source_set(source).await?)
    }

    /// Shows the core's agreement text for `source` in the native dialog and records the
    /// agreement only when the person agrees there, and only for the chosen source. See
    /// [`crate::confirm`].
    pub async fn research_consent_ask(
        &self,
        source: SearchSource,
    ) -> Outcome<ResearchConsentAnswer> {
        let core = self.core()?;
        crate::confirm::confirm_research(
            core,
            &self.desktop.prompt,
            self.desktop.can_confirm,
            &self.reviews,
            source,
        )
        .await
    }

    /// Withdraws the agreement (for the chosen source; any agreement ends with it).
    pub async fn research_consent_withdraw(&self) -> Outcome<()> {
        let core = self.core()?;
        let chosen = core.settings_get().await?.research.source;
        core.research_consent_for(chosen, false).await?;
        Ok(())
    }

    /// Stores a search service's key. Write-only: nothing returns it.
    pub async fn search_key_set(&self, service: SearchService, key: String) -> Outcome<()> {
        Ok(self.core()?.search_key_set(service, key).await?)
    }

    pub async fn search_key_delete(&self, service: SearchService) -> Outcome<()> {
        Ok(self.core()?.search_key_delete(service).await?)
    }

    /// Whether a search service's key is stored. Never the key.
    pub async fn search_key_status(&self, service: SearchService) -> Outcome<bool> {
        Ok(self.core()?.search_key_status(service).await?)
    }

    /// What this machine has and lacks. Works without an open core, and then says why the
    /// core did not open.
    pub async fn doctor_report(&self, skip_self_tests: bool) -> Outcome<DoctorReport> {
        match &self.core {
            Ok(core) => Ok(core.doctor_report(skip_self_tests).await),
            Err(_) => Ok(hotend_core::doctor::report(hotend_core::DoctorOptions {
                slicers: None,
                skip_self_tests,
            })
            .await),
        }
    }

    // ---- the agent ----------------------------------------------------------------------

    /// Sends a message to the agent. The run's events arrive on the event stream.
    pub async fn message_send(&self, project_id: String, text: String) -> Outcome<MessageStarted> {
        // Dropping the run's own stream does not stop the run.
        let run = self.core()?.message_send(&project_id, &text).await?;
        Ok(MessageStarted { run_id: run.run_id })
    }

    pub async fn run_cancel(&self, run_id: String) -> Outcome<bool> {
        Ok(self.core()?.run_cancel(&run_id).await)
    }

    // ---- the desktop --------------------------------------------------------------------

    /// Asks the person for a folder in the system's picker. `None` when they cancelled.
    /// `title` is shown only if it is one of the interface's own
    /// ([`grants::folder_title`]). The folder is granted as the hand-off folder when
    /// `title` is exactly the hand-off picker's, as the snapshot export folder when it is
    /// exactly the snapshot export picker's, and as a folder to write files into otherwise
    /// ([`grants::folder_kind`]).
    pub async fn folder_pick(&self, title: String) -> Outcome<Option<String>> {
        let pickers = Arc::clone(&self.desktop.pickers);
        let kind = grants::folder_kind(&title);
        let title = grants::folder_title(&title);
        self.picked(kind, move || pickers.pick_folder(title)).await
    }

    /// Asks the person for a file (a slicer program) in the system's picker. `title` is
    /// shown only if it is one of the interface's own ([`grants::program_title`]).
    pub async fn file_pick(&self, title: String) -> Outcome<Option<String>> {
        let pickers = Arc::clone(&self.desktop.pickers);
        let title = grants::program_title(&title);
        self.picked(Picked::Program, move || pickers.pick_file(&title))
            .await
    }

    async fn picked(
        &self,
        picked: Picked,
        ask: impl FnOnce() -> Option<PathBuf> + Send + 'static,
    ) -> Outcome<Option<String>> {
        let choice = tokio::task::spawn_blocking(ask).await.unwrap_or(None);
        let Some(path) = choice else {
            return Ok(None);
        };
        let Some(text) = path.to_str().map(str::to_owned) else {
            return Err(ApiError::InvalidInput {
                message: "the name of that folder or file is not valid text".to_owned(),
                next_step: "Choose another one.".to_owned(),
            });
        };
        self.grants.grant(picked, &path);
        Ok(Some(text))
    }

    /// Opens one of Hotend's fixed links in the system browser.
    pub async fn open_link(&self, link: LinkId) -> Outcome<()> {
        let url = crate::links::resolve(link)?;
        self.desktop
            .links
            .open(&url)
            .map_err(|reason| ApiError::Internal {
                message: format!("the system browser could not be opened: {reason}"),
                next_step: format!("Open {url} in your browser."),
            })
    }
}
