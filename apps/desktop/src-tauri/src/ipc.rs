//! The window's commands: one table, from which the Tauri commands, the TypeScript binding
//! and the development bridge's dispatcher are all generated.
//!
//! Guarantee: a command exists for the window if and only if it is a row of the table
//! below (plus `events_subscribe`, the event stream). Each row calls the method of the same
//! name on [`Shell`]; the Tauri command and the bridge's `dispatch` arm are the same row, so
//! the two transports expose the same commands with the same argument names. Command names
//! are those of PROTOCOL.md section 8; argument names are camelCase on the wire (Tauri's
//! convention: `project_id` is sent as `projectId`), which the generated binding hides.
//!
//! The capability file grants these commands, one permission each, to the main window and
//! nothing else; `tests/window.rs` keeps the three lists (this table,
//! `build/command_names.rs`, `capabilities/main.json`) equal.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hotend_core::engine::Section;
use hotend_core::{
    ApiError, AuditEntry, BuildReport, Calibration, CalibrationUpdate, CardRef, DeliveryChoice,
    DoctorReport, EditableProject, EngineResult, ExportFormat, ExportedFile, GaugeExport,
    GaugeReading, HandOffStatus, LanPrinterStatus, Preparation, PrepareOptions, PrintJob,
    PrintReview, PrinterInfo, Project, ProjectDestination, ProjectDetail, ProjectReview,
    ProviderKind, Question, RenderedView, SearchService, SearchSource, SearchSourceOption,
    Settings, SliceOutcome, SlicerInstallation, Snapshot, SnapshotExport, Version, VersionDetail,
};
use serde_json::Value;
use tauri::ipc::Channel;

use crate::confirm::{
    CertificateConfirmation, PrintConfirmation, ProjectConfirmation, ResearchConsentAnswer,
};
use crate::events::ShellEvent;
use crate::shell::{LanSetup, MessageStarted, Shell};
use crate::wire::{self as w, IntoPlain, LinkId, Wire};

/// A failed command as the window receives it.
pub type Failure = Wire<ApiError, w::ApiError>;

/// Why the bridge's dispatcher did not run a command.
#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error("`{0}` is not a command of the window")]
    UnknownCommand(String),
    #[error("the arguments of `{command}` are not valid: {reason}")]
    BadArguments { command: String, reason: String },
    /// The command ran and failed.
    #[error("{}", .0.message())]
    Failed(ApiError),
}

macro_rules! shell_commands {
    ($(
        $(#[$meta:meta])*
        fn $name:ident ( $($arg:ident : $ty:ty),* $(,)? ) -> $ret:ty;
    )*) => {
        $(
            $(#[$meta])*
            #[tauri::command]
            #[specta::specta]
            pub async fn $name(
                shell: tauri::State<'_, Arc<Shell>>,
                $($arg: $ty),*
            ) -> Result<$ret, Failure> {
                shell
                    .$name($($arg.into_plain()),*)
                    .await
                    .map(<$ret>::from)
                    .map_err(Failure::from)
            }
        )*

        /// The names of the commands in the table, in its order.
        pub const SHELL_COMMANDS: &[&str] = &[$(stringify!($name)),*];

        /// The commands for the Tauri invoke handler and the binding generator.
        pub fn collect<R: tauri::Runtime>() -> tauri_specta::Commands<R> {
            tauri_specta::collect_commands![$($name,)* events_subscribe]
        }

        /// Runs a command of the table by name with JSON arguments: what the Tauri invoke
        /// handler does for the window, for the development bridge and the tests.
        pub async fn dispatch(
            shell: &Shell,
            command: &str,
            args: Value,
        ) -> Result<Value, DispatchError> {
            // Arguments are named, as Tauri sends them: an object, never a list. A command
            // without arguments may be called without one.
            let args = match args {
                Value::Null => Value::Object(Default::default()),
                Value::Object(_) => args,
                _ => {
                    return Err(DispatchError::BadArguments {
                        command: command.chars().take(64).collect(),
                        reason: "the arguments must be an object of named values".to_owned(),
                    });
                }
            };
            match command {
                $(stringify!($name) => {
                    #[derive(serde::Deserialize)]
                    #[serde(deny_unknown_fields, rename_all = "camelCase")]
                    struct Args {
                        $($arg: $ty,)*
                    }
                    let Args { $($arg,)* } = serde_json::from_value(args).map_err(|error| {
                        DispatchError::BadArguments {
                            command: command.to_owned(),
                            reason: error.to_string(),
                        }
                    })?;
                    let answer: $ret = shell
                        .$name($($arg.into_plain()),*)
                        .await
                        .map(<$ret>::from)
                        .map_err(DispatchError::Failed)?;
                    serde_json::to_value(answer).map_err(|error| {
                        DispatchError::Failed(ApiError::Internal {
                            message: format!("the answer of `{command}` could not be written: {error}"),
                            next_step: "Try again.".to_owned(),
                        })
                    })
                })*
                other => Err(DispatchError::UnknownCommand(other.chars().take(64).collect())),
            }
        }
    };
}

shell_commands! {
    // ---- projects and versions ----
    /// Every project, most recently changed first.
    fn projects_list() -> Wire<Vec<Project>, Vec<w::Project>>;
    /// A new, empty project.
    fn project_create(name: String) -> Wire<Project, w::Project>;
    /// A project with its versions and conversation.
    fn project_get(project_id: String) -> Wire<ProjectDetail, w::ProjectDetail>;
    /// The versions of a project, oldest first.
    fn versions_list(project_id: String) -> Wire<Vec<Version>, Vec<w::Version>>;
    /// A version with the engine's result: parameters, parts, checks.
    fn version_get(project_id: String, number: u32) -> Wire<VersionDetail, w::VersionDetail>;
    /// The text of a version's `mesh.json`, which three-cad-viewer loads unchanged.
    fn version_mesh(project_id: String, number: u32) -> String;
    /// Makes an earlier version the current one again. Nothing is changed or removed.
    fn version_revert(project_id: String, number: u32) -> Wire<Version, w::Version>;
    /// What the contract of a version (the current one by default) still asks the maker.
    fn questions_list(project_id: String, version: Option<u32>) -> Wire<Vec<Question>, Vec<w::Question>>;
    /// Exact measurements on the current version, one answer per query.
    fn measure_run(project_id: String, queries: Vec<Value>) -> Vec<Value>;
    /// Builds a model script and stores it as the next version if it built.
    fn model_build(project_id: String, code: String, params: Wire<BTreeMap<String, f64>, BTreeMap<String, w::Num>>, note: String) -> Wire<BuildReport, w::BuildReport>;
    /// Rebuilds the current version with one size changed. A refused value is a report
    /// without a version.
    fn parameter_set(project_id: String, name: String, value: Wire<f64, w::Num>) -> Wire<BuildReport, w::BuildReport>;
    /// Runs the checks (all, or the named ones) on the current version.
    fn checks_run(project_id: String, checks: Option<Vec<String>>, sample_ranges: bool) -> Wire<EngineResult, w::EngineResult>;
    /// Renders PNG views of the current version, optionally cut open.
    fn views_render(project_id: String, views: Vec<String>, section: Wire<Option<Section>, Option<w::Section>>, size: [u32; 2]) -> Wire<Vec<RenderedView>, Vec<w::RenderedView>>;
    /// Writes STEP, 3MF or STL files of a version into a folder picked with `folder_pick`.
    fn export_run(project_id: String, version: Option<u32>, formats: Wire<Vec<ExportFormat>, Vec<w::ExportFormat>>, parts: Option<Vec<String>>, destination: String) -> Wire<Vec<ExportedFile>, Vec<w::ExportedFile>>;

    // ---- snapshots ----
    /// The snapshots of a project, oldest first.
    fn snapshots_list(project_id: String) -> Wire<Vec<Snapshot>, Vec<w::Snapshot>>;
    /// Names a version (the current one when `version` is null). Changes no version.
    fn snapshot_create(project_id: String, name: String, version: Option<u32>, note: String) -> Wire<Snapshot, w::Snapshot>;
    /// Removes a snapshot's name; the version and its files stay.
    fn snapshot_delete(project_id: String, name: String) -> Wire<Snapshot, w::Snapshot>;
    /// Makes the snapshot's version the current one again. Nothing is removed.
    fn snapshot_restore(project_id: String, name: String) -> Wire<Version, w::Version>;
    /// Writes a snapshot's files into a new folder below a folder picked with `folder_pick`
    /// under the title "Save snapshot files" (and under no other title).
    fn snapshot_export(project_id: String, name: String, folder: String) -> Wire<SnapshotExport, w::SnapshotExport>;

    // ---- preparation, gauges, calibration ----
    /// Lays a version out for printing: orientation of each part, copies, plates.
    fn prepare_run(project_id: String, version: Option<u32>, options: Wire<PrepareOptions, w::PrepareOptions>) -> Wire<Preparation, w::Preparation>;
    /// The stored layout of a version, if it was prepared.
    fn preparation_get(project_id: String, version: u32) -> Wire<Option<Preparation>, Option<w::Preparation>>;
    /// Writes gauge plates into a folder picked with `folder_pick`.
    fn gauge_export(project_id: Option<String>, cards: Wire<Option<Vec<CardRef>>, Option<Vec<w::CardRef>>>, nozzle_id: Option<i64>, destination: String) -> Wire<GaugeExport, w::GaugeExport>;
    /// The calibration table of the printer in the settings.
    fn calibration_get() -> Wire<Calibration, w::Calibration>;
    /// Records caliper readings of a printed gauge. The core dates them.
    fn calibration_record(readings: Wire<Vec<GaugeReading>, Vec<w::GaugeReading>>, nozzle_id: Option<i64>) -> Wire<CalibrationUpdate, w::CalibrationUpdate>;

    // ---- slicing ----
    /// Prepares the current version if needed and slices one plate.
    fn slice_run(project_id: String, profile: String, plate: Option<u32>) -> Wire<SliceOutcome, w::SliceOutcome>;
    /// The newest slice of a version as it is laid out now.
    fn slice_get(project_id: String, version: Option<u32>) -> Wire<Option<SliceOutcome>, Option<w::SliceOutcome>>;
    /// Every slicer kind of the build, installed or not.
    fn slicers_list() -> Wire<Vec<SlicerInstallation>, Vec<w::SlicerInstallation>>;
    /// Makes a slicer the one in use. `executable` must come from `file_pick`.
    fn slicer_choose(kind: String, executable: Option<String>) -> Wire<Settings, w::Settings>;

    // ---- printing ----
    /// The printers (one, simulated) and the ways a confirmed job can be delivered.
    fn printers_list() -> Wire<Vec<PrinterInfo>, Vec<w::PrinterInfo>>;
    /// Proposes the sliced file of the current version as a pending print job.
    fn print_propose(project_id: String, note: String) -> Wire<PrintJob, w::PrintJob>;
    /// The print jobs of a project, newest first.
    fn print_jobs(project_id: String) -> Wire<Vec<PrintJob>, Vec<w::PrintJob>>;
    /// One print job as it is now.
    fn print_status(job_id: String) -> Wire<PrintJob, w::PrintJob>;
    /// Checks a pending job's sliced file and returns the confirmation summary read from it.
    fn print_review(job_id: String, delivery: Wire<DeliveryChoice, w::DeliveryChoice>) -> Wire<PrintReview, w::PrintReview>;
    /// Opens Hotend's own confirmation dialog for a pending job, outside the page. The job
    /// is delivered only if the person confirms there; the answer says whether they did.
    fn print_confirm(job_id: String, delivery: Wire<DeliveryChoice, w::DeliveryChoice>) -> Wire<PrintConfirmation, w::PrintConfirmation>;
    /// Withdraws a pending job, or stops one on the simulated printer.
    fn print_cancel(job_id: String) -> Wire<PrintJob, w::PrintJob>;
    /// The hand-off as it is set up now: the folder, its Windows form, what opens a file.
    fn handoff_status() -> Wire<HandOffStatus, w::HandOffStatus>;
    /// Makes the editable Bambu Studio project of one plate of the current version (plate 1
    /// by default). Nothing is opened or handed over.
    fn print_project_make(project_id: String, plate: Option<u32>) -> Wire<EditableProject, w::EditableProject>;
    /// The editable projects of a project, newest first.
    fn print_projects(project_id: String) -> Wire<Vec<EditableProject>, Vec<w::EditableProject>>;
    /// Checks an editable project again and returns what a person must see before it is
    /// handed over to the destination. A folder must come from `folder_pick`.
    fn print_project_review(project_export_id: String, destination: Wire<ProjectDestination, w::ProjectDestination>) -> Wire<ProjectReview, w::ProjectReview>;
    /// Opens Hotend's own confirmation dialog for handing a project over, outside the page.
    /// The project is handed over only if the person confirms there; the answer says
    /// whether they did.
    fn print_project_confirm(project_export_id: String, destination: Wire<ProjectDestination, w::ProjectDestination>) -> Wire<ProjectConfirmation, w::ProjectConfirmation>;

    // ---- a real printer's status over the local network (read-only) ----
    /// How reading the printer's status is set up, and whether an access code is there.
    /// Never the code.
    fn printer_lan_get() -> Wire<LanSetup, w::PrinterLanSetup>;
    /// The printer's address (a private-range IPv4 address) and serial number.
    fn printer_lan_set(ip: String, serial: String) -> Wire<LanSetup, w::PrinterLanSetup>;
    /// Forgets the printer's address and serial.
    fn printer_lan_clear() -> ();
    /// Stores the printer's LAN access code. Write-only: no command returns it.
    fn printer_lan_key_set(code: String) -> ();
    /// Removes the stored access code.
    fn printer_lan_key_delete() -> ();
    /// Takes a first look at the printer's certificate and shows it in Hotend's own
    /// dialog, outside the page; it is pinned only if the person confirms there. The answer
    /// says whether they did. The window sends no fingerprint.
    fn printer_certificate_check() -> Wire<CertificateConfirmation, w::CertificateConfirmation>;
    /// Forgets the confirmed certificate.
    fn printer_certificate_forget() -> ();
    /// Follows the printer (read-only) until `printer_watch_stop`; its reports arrive on
    /// the event stream (channel `printer`).
    fn printer_watch_start() -> ();
    /// Stops following the printer. False when nothing was followed.
    fn printer_watch_stop() -> bool;
    /// The latest report of the session; null before the first.
    fn printer_watch_latest() -> Wire<Option<LanPrinterStatus>, Option<w::LanPrinterStatus>>;

    // ---- settings, keys, audit, doctor ----
    /// The newest audit entries of one project, or of everything.
    fn audit_log(project_id: Option<String>, limit: u32) -> Wire<Vec<AuditEntry>, Vec<w::AuditEntry>>;
    /// The saved settings, or the defaults.
    fn settings_get() -> Wire<Settings, w::Settings>;
    /// Validates and saves the settings.
    fn settings_set(settings: Wire<Settings, w::Settings>) -> ();
    /// Stores a provider's API key. Write-only: no command returns a key.
    fn provider_key_set(kind: Wire<ProviderKind, w::ProviderKind>, key: String) -> ();
    /// Removes a provider's API key.
    fn provider_key_delete(kind: Wire<ProviderKind, w::ProviderKind>) -> ();
    /// Whether a provider's API key is stored.
    fn provider_key_status(kind: Wire<ProviderKind, w::ProviderKind>) -> bool;

    // ---- where web research comes from ----
    /// The research sources: available or why not, who receives the queries, whether a key
    /// is stored, which is chosen and which the person's agreement covers.
    fn research_source_options() -> Wire<Vec<SearchSourceOption>, Vec<w::SearchSourceOption>>;
    /// Chooses where web research comes from; an agreement for another source does not
    /// cover it.
    fn research_source_set(source: Wire<SearchSource, w::SearchSource>) -> Wire<Settings, w::Settings>;
    /// Shows Hotend's own dialog, outside the page, with who receives the queries of
    /// `source`; the agreement is recorded only if the person agrees there, and only for the
    /// chosen source. The answer says whether they did.
    fn research_consent_ask(source: Wire<SearchSource, w::SearchSource>) -> Wire<ResearchConsentAnswer, w::ResearchConsentAnswer>;
    /// Withdraws the agreement to web research.
    fn research_consent_withdraw() -> ();
    /// Stores a search service's API key. Write-only: no command returns a key.
    fn search_key_set(service: Wire<SearchService, w::SearchService>, key: String) -> ();
    /// Removes a search service's API key.
    fn search_key_delete(service: Wire<SearchService, w::SearchService>) -> ();
    /// Whether a search service's API key is stored.
    fn search_key_status(service: Wire<SearchService, w::SearchService>) -> bool;
    /// What this machine has and lacks. Works even when the core could not start.
    fn doctor_report(skip_self_tests: bool) -> Wire<DoctorReport, w::DoctorReport>;

    // ---- the agent ----
    /// Sends a message to the agent. The run's events arrive on the event stream.
    fn message_send(project_id: String, text: String) -> Wire<MessageStarted, w::MessageStarted>;
    /// Asks a run to stop. False when no such run is in progress.
    fn run_cancel(run_id: String) -> bool;

    // ---- the desktop ----
    /// The system's folder picker. Null when the person cancelled.
    fn folder_pick(title: String) -> Option<String>;
    /// The system's file picker, for choosing a slicer program. Null when cancelled.
    fn file_pick(title: String) -> Option<String>;
    /// Opens one of Hotend's fixed links in the system browser.
    fn open_link(link: LinkId) -> ();
}

/// The one event subscription of the window. A new one (after a reload) ends the old one.
#[derive(Default)]
pub struct Subscription(Mutex<Option<tauri::async_runtime::JoinHandle<()>>>);

/// Streams the core's events (agent runs and print jobs) to the window through a channel,
/// from now on.
#[tauri::command]
#[specta::specta]
pub async fn events_subscribe(
    shell: tauri::State<'_, Arc<Shell>>,
    subscription: tauri::State<'_, Subscription>,
    on_event: Channel<Wire<ShellEvent, w::ShellEvent>>,
) -> Result<(), Failure> {
    let events = shell.core().map_err(Failure::from)?.events();
    let task = tauri::async_runtime::spawn(crate::events::forward(events, move |event| {
        on_event.send(Wire::new(event)).is_ok()
    }));
    let previous = subscription
        .0
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .replace(task);
    if let Some(previous) = previous {
        previous.abort();
    }
    Ok(())
}

/// Every command the window may call, by name.
pub fn command_names() -> Vec<&'static str> {
    let mut names = SHELL_COMMANDS.to_vec();
    names.push("events_subscribe");
    names
}

/// The tauri-specta builder: the invoke handler and the TypeScript binding come from it.
pub fn specta_builder<R: tauri::Runtime>() -> tauri_specta::Builder<R> {
    tauri_specta::Builder::<R>::new()
        .commands(collect::<R>())
        .function_casing(tauri_specta::Casing::SnakeCase)
        // Every type crosses in one direction or is written back whole, so one TypeScript
        // type per Rust type is enough.
        .disable_serde_phases()
        // Milliseconds, byte counts and row ids: all far below 2^53.
        .dangerously_cast_bigints_to_number()
}

/// Writes the TypeScript binding to `path`.
pub fn export_bindings(path: &std::path::Path) -> Result<(), String> {
    specta_builder::<tauri::Wry>()
        .export(
            specta_typescript::Typescript::default()
                .header("// Generated by tauri-specta from apps/desktop/src-tauri/src/ipc.rs. Do not edit:\n// run `pnpm bindings` in apps/desktop.\n"),
            path,
        )
        .map_err(|error| error.to_string())
}
