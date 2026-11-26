//! Printing: from a proposed job to a delivered one, through `hotend-printer`.
//!
//! Guarantees:
//! - Nothing here is reachable from the tool gateway or the agent: the gateway holds no
//!   handle to this module, and a job leaves the state `pending` only in
//!   [`PrintService::confirm`], which needs the process's [`ConfirmationAuthority`]. Only
//!   the layer that shows the confirmation to a person can supply it.
//! - What the person confirms is read from the sliced file itself, after the G-code linter
//!   passed it; a file that fails the linter is never delivered, by any path.
//! - The file is checked for the job it was sliced as: the build plate the person chose
//!   in the settings and the nozzle sizes of the printer profile, both recorded with the
//!   slice. A slice made without a chosen plate, or for another plate than the settings
//!   name now, does not become a print.
//! - The confirmation carries the text the person was shown. It must contain the review
//!   text of the checked file ([`PrintReview::review_text`]) word for word, and the printer
//!   layer binds the confirmation to exactly that text; a job whose file changed since
//!   the review is refused.
//! - The only printer Hotend delivers to is the simulated one. No code here opens a network
//!   connection. A real printer's status, read by [`crate::printer_lan`], reaches a
//!   handed-off job only through [`Follower`], which ties a report to a job by the name of the
//!   file the hand-off placed and never changes the job's status.
//! - A hand-off to Bambu Connect places the checked bytes in the hand-off folder the
//!   review named (the person's choice in the settings; under WSL it must be a folder on
//!   a Windows drive), and only then gives the link, which only a confirmed hand-off can
//!   make, to the injected [`LinkOpener`]. The opener is a stand-in that opens nothing
//!   until the application sets the real one ([`crate::Core::set_link_opener`]), so tests
//!   and the command line never open anything.
//! - A started job is watched: a printer that accepts the start and stays idle is reported
//!   as `not_started`, never as printing.
//! - A job is claimed before anything is delivered: `confirm` moves it from `pending` to
//!   `starting` with one compare-and-set, and for the printer does so while it holds the
//!   printer, in the same step as the upload and the start. A cancel that comes first wins
//!   and nothing is sent; one that comes later finds a job it can stop. The record never
//!   says `cancelled` of a job that was started.
//! - A job whose version was laid out again since it was sliced is not reviewed: what
//!   would print is no longer what the preparation shows.
//! - An editable Bambu Studio project ([`project`]) is made only for a plate whose slice
//!   passes the G-code check, and handed over only on its own confirmation, which covers
//!   the project file's hash. Every text about it says that a project opened this way is
//!   sliced again by Bambu Studio, so it is outside Hotend's check.

mod followed;
mod project;

pub(crate) use followed::Follower;
pub use followed::{LanPrinterStatus, NozzleReading, TemperatureReading};
pub use project::{
    EditableProject, PROJECT_TO_BAMBU_STUDIO, PROJECT_TO_FOLDER, ProjectDestination, ProjectReview,
};

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant};

use hotend_printer::{
    ArchiveLimits, BambuConnectHandOff, Capabilities, CheckedJob, Command, ConfirmationAuthority,
    ConfirmationRequest, DeclaredFilament, DeclaredJob, Delivered, Delivery, Explanation,
    ExportToFolder, FilamentSource, Finding, HandOffFolder, HostKind, HumanConfirmation, JobError,
    JobEvent, JobOutcome, JobTracker, LinkOpener, OpenError, Phase, Printer, PrinterDelivery,
    PrinterStatus, Severity, SimConfig, SimulatedPrinter, StandInOpener, StartOptions, StartQuirk,
    StartToken, StartVerdict, StartWatchdog, TokenError, start_print,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::agent::RunEvent;
use crate::gateway::{MAX_SLICE_BYTES, SliceSummary};
use crate::import;
use crate::paths::DataDir;
use crate::settings::{PreviewsMode, SETTINGS_KEY, Settings};
use crate::store::{NewAudit, PendingPrintJob, Store, StoreError};
use crate::util::truncate;

/// The id of the simulated printer in [`PrinterInfo`].
pub const SIMULATED_PRINTER_ID: &str = "simulated-x2d";
/// The plate inside a sliced file: every prepared plate is sliced into a file of its own.
const ARCHIVE_PLATE: u32 = 1;

/// How the simulated printer behaves. `Default` is the product configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct PrinterOptions {
    /// Simulated seconds per real second.
    pub time_scale: f64,
    /// How long, in the printer's (simulated) time, a start may take before the job is
    /// reported as not started.
    pub start_deadline: Duration,
    /// How often the status is read while a job runs.
    pub poll_interval: Duration,
    /// The simulated printer accepts a start and stays idle, as an X2D does while its AMS
    /// dries filament. For exercising the start watchdog.
    pub stays_idle_after_start: bool,
}

impl Default for PrinterOptions {
    fn default() -> Self {
        Self {
            time_scale: 60.0,
            start_deadline: StartWatchdog::DEFAULT_DEADLINE,
            poll_interval: Duration::from_millis(250),
            stays_idle_after_start: false,
        }
    }
}

/// How a confirmed job leaves Hotend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeliveryChoice {
    /// Upload to the simulated printer and start it there.
    SimulatedPrinter,
    /// Place the checked file in the hand-off folder and give Bambu Connect's import link
    /// to the opener.
    BambuConnect,
    /// Write the file into a folder (or a USB drive).
    ExportToFolder { folder: PathBuf },
}

impl DeliveryChoice {
    /// The name stored with the job.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SimulatedPrinter => "simulated_printer",
            Self::BambuConnect => "bambu_connect",
            Self::ExportToFolder { .. } => "export_to_folder",
        }
    }
}

/// Whether a linter finding blocks delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LintSeverity {
    Error,
    Warning,
}

/// The code of the review's warning that the version was built for another printer,
/// material or calibration than it is printed for.
pub const BUILT_FOR_OTHER: &str = "built_for_other";

/// The code of the review's warning that the size of the fitted nozzle is assumed, not
/// known: the person did not set it (the slice record's `job.nozzle_size_set`).
pub const NOZZLE_SIZE_ASSUMED: &str = "nozzle_size_assumed";

/// One thing the G-code linter found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LintFinding {
    pub severity: LintSeverity,
    pub code: String,
    /// 1-based line of the G-code; 0 for a finding about the job as a whole.
    pub line: u64,
    pub message: String,
    pub excerpt: String,
}

impl From<&Finding> for LintFinding {
    fn from(finding: &Finding) -> Self {
        Self {
            severity: match finding.severity {
                Severity::Error => LintSeverity::Error,
                Severity::Warning => LintSeverity::Warning,
            },
            code: finding.code.to_owned(),
            line: finding.line,
            message: finding.message.clone(),
            excerpt: finding.excerpt.clone(),
        }
    }
}

/// A problem the printer reports, explained, with the raw code kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrinterProblem {
    pub message: String,
    pub raw_code: String,
    pub meaning: Option<String>,
    pub blocking: bool,
    pub help_url: Option<String>,
}

impl From<Explanation> for PrinterProblem {
    fn from(explanation: Explanation) -> Self {
        Self {
            message: explanation.message,
            raw_code: explanation.raw_code,
            meaning: explanation.meaning,
            blocking: explanation.blocking,
            help_url: explanation.help_url,
        }
    }
}

/// One filament as the sliced file describes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrintFilament {
    pub slot: u32,
    pub material: Option<String>,
    /// `#RRGGBBAA`.
    pub colour: Option<String>,
    pub grams: Option<f64>,
    pub metres: Option<f64>,
    /// The nozzle the file's own filament map assigns.
    pub nozzle_id: Option<i64>,
    pub selected_in_gcode: bool,
}

/// What a sliced file will make the printer do, read from the file itself after the linter
/// passed it. This is what the confirmation shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrintSummary {
    pub file_name: String,
    pub sha256: String,
    pub size_bytes: u64,
    /// The plate inside the sliced file, which is always 1: every plate of a layout is
    /// sliced into a file of its own. The plate of the layout is [`PrintReview::plate`].
    pub plate: u32,
    pub estimated_seconds: Option<u64>,
    pub layers: Option<u32>,
    pub filaments: Vec<PrintFilament>,
    pub max_nozzle_c: Option<f64>,
    pub max_bed_c: Option<f64>,
    pub max_chamber_c: Option<f64>,
    /// The box around everything the G-code prints: `[min, max]`, mm.
    pub extent: Option<[[f64; 3]; 2]>,
}

fn summary_of(job: &CheckedJob) -> PrintSummary {
    let summary = job.summary();
    PrintSummary {
        file_name: summary.file_name.clone(),
        sha256: summary.sha256.clone(),
        size_bytes: summary.size_bytes,
        plate: summary.plate,
        estimated_seconds: summary.estimated_seconds,
        layers: summary.layers,
        filaments: summary
            .filaments
            .iter()
            .map(|filament| PrintFilament {
                slot: filament.slot,
                material: filament.material.clone(),
                colour: filament.colour.as_ref().map(|c| format!("#{}", c.hex())),
                grams: filament.grams,
                metres: filament.metres,
                nozzle_id: filament.nozzle.map(i64::from),
                selected_in_gcode: filament.selected_in_gcode,
            })
            .collect(),
        max_nozzle_c: summary.max_nozzle_c,
        max_bed_c: summary.max_bed_c,
        max_chamber_c: summary.max_chamber_c,
        extent: summary.extent.map(|extent| [extent.min, extent.max]),
    }
}

/// The spool that will feed one filament of the job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpoolChoice {
    pub slot: u32,
    pub material: String,
    pub nozzle_id: i64,
    /// The AMS unit as the printer reports it.
    pub ams_unit: u8,
    pub tray: u8,
    /// What is loaded there, for people.
    pub loaded: String,
}

/// A job as it is put before the person who confirms it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrintReview {
    pub job: PrintJob,
    pub delivery: DeliveryChoice,
    /// True when the job would go to the simulated printer: nothing is printed.
    pub simulated: bool,
    /// The printer that would print it; `None` for a hand-off or an export.
    pub printer: Option<PrinterRef>,
    /// The plate of the version's layout that was sliced, from 1 (the slice record).
    pub plate: u32,
    /// The pieces on that plate, by name.
    pub pieces: Vec<String>,
    pub summary: PrintSummary,
    /// The text a person confirms, written by the printer layer from the checked file
    /// alone: time, layers, filament, nozzle, temperatures, the build plate, the printed
    /// area, every accepted command that changes the printer's settings, and the file's
    /// SHA-256. Whoever asks for the confirmation shows this text as it is, in one piece,
    /// and hands what was shown to [`crate::Core::print_confirm`].
    pub review_text: String,
    /// What the linter found that does not block.
    pub warnings: Vec<LintFinding>,
    /// The spools the printer would use; empty for a hand-off or an export.
    pub spools: Vec<SpoolChoice>,
    /// Numbers of this version that were found on the web and that nobody has confirmed,
    /// with their page and quote (docs/WEB_RESEARCH.md). Show them above the confirmation.
    #[serde(default)]
    pub web_numbers: Vec<crate::research::WebNumber>,
    /// Where a hand-off to Bambu Connect would place the file, and what opens the link;
    /// `None` for the other deliveries.
    #[serde(default)]
    pub handoff: Option<HandOffTarget>,
}

/// The Bambu Connect hand-off as it is set up now, for the settings page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandOffStatus {
    /// `wsl`, `windows`, `macos` or `linux`: the host the hand-off path is made for.
    pub host: String,
    /// The folder the settings name, as the person chose it; `None` when none was chosen.
    pub chosen: Option<PathBuf>,
    /// The folder a hand-off would use now (canonical); `None` when there is none.
    pub folder: Option<PathBuf>,
    /// Its Windows form, under WSL and on Windows.
    pub windows_folder: Option<String>,
    /// Why no hand-off can be made now, as a sentence.
    pub problem: Option<String>,
    /// What opens the import link ([`LinkOpener::describe`]).
    pub opener: String,
    /// False when the opener opens nothing (the stand-in).
    pub opens: bool,
}

/// Where a hand-off places the file, and what then opens Bambu Connect's import link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandOffTarget {
    /// The hand-off folder as Hotend sees it.
    pub folder: PathBuf,
    /// Its Windows form, under WSL and on Windows.
    pub windows_folder: Option<String>,
    /// What opens the link ([`LinkOpener::describe`]).
    pub opener: String,
    /// False when the opener opens nothing (the stand-in): the file is placed and the link
    /// is kept, and Bambu Connect is not opened.
    pub opens: bool,
}

/// A printer by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrinterRef {
    pub id: String,
    pub name: String,
    pub model: String,
    pub serial: String,
    pub simulated: bool,
}

/// One loaded tray.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrayInfo {
    pub id: u8,
    pub material: Option<String>,
    pub product: Option<String>,
    pub colour: Option<String>,
    pub remaining_percent: Option<u8>,
}

/// One AMS unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AmsInfo {
    pub id: u8,
    pub high_temperature: bool,
    /// The nozzle it feeds.
    pub nozzle_id: Option<i64>,
    pub drying_minutes_left: Option<u32>,
    pub trays: Vec<TrayInfo>,
}

/// What a printer reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrinterState {
    /// `idle`, `preparing`, `heating`, `printing`, `paused`, `finished`, `failed`,
    /// `offline` or `unknown`.
    pub phase: String,
    /// One sentence for the interface.
    pub headline: String,
    pub progress_percent: Option<u8>,
    pub remaining_minutes: Option<u32>,
    pub layer: Option<u32>,
    pub total_layers: Option<u32>,
    pub job_name: Option<String>,
    pub bed_c: Option<f64>,
    pub problems: Vec<PrinterProblem>,
    pub ams: Vec<AmsInfo>,
}

/// One way a job can leave Hotend, with what it can and cannot do.
///
/// Only the ways this build can deliver are listed, each in its own words: a way that is
/// not built (direct control of a real printer) has no entry, and the simulated printer is
/// described as a simulation, not with a real printer's abilities and limits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryInfo {
    /// `simulated_printer`, `bambu_connect` or `export_to_folder`: what
    /// [`DeliveryChoice::as_str`] returns for the matching choice.
    pub kind: String,
    pub name: String,
    /// Whether it can be chosen. False for `bambu_connect` while the hand-off folder chosen
    /// in the settings cannot be used (on every host), and under WSL while none is chosen.
    pub enabled: bool,
    /// Why not, as a sentence, when it cannot.
    pub disabled_reason: Option<String>,
    pub sends_file: bool,
    pub starts_print: bool,
    pub controls_print: bool,
    pub live_status: bool,
    pub reports_result: bool,
    pub can: Vec<String>,
    pub cannot: Vec<String>,
    pub cost: String,
    pub needs: Vec<String>,
}

impl DeliveryInfo {
    fn new(kind: &str, capabilities: Capabilities) -> Self {
        let words = |list: Vec<&'static str>| list.into_iter().map(str::to_owned).collect();
        Self {
            kind: kind.to_owned(),
            name: capabilities.name.to_owned(),
            enabled: true,
            disabled_reason: None,
            sends_file: capabilities.sends_file,
            starts_print: capabilities.starts_print,
            controls_print: capabilities.controls_print,
            live_status: capabilities.live_status,
            reports_result: capabilities.reports_result,
            can: words(capabilities.can),
            cannot: words(capabilities.cannot),
            cost: capabilities.cost.to_owned(),
            needs: words(capabilities.needs),
        }
    }
}

/// A printer Hotend can deliver to, and the ways to deliver.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrinterInfo {
    #[serde(flatten)]
    pub printer: PrinterRef,
    pub state: PrinterState,
    pub deliveries: Vec<DeliveryInfo>,
}

/// The last known state of a print job, for display.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrintJobDetail {
    /// True when the job went to the simulated printer: nothing was printed.
    pub simulated: bool,
    pub phase: Option<String>,
    pub headline: Option<String>,
    pub progress_percent: Option<u8>,
    pub remaining_minutes: Option<u32>,
    /// Why the job is where it is, when that needs words.
    pub message: Option<String>,
    pub problems: Vec<PrinterProblem>,
    /// Linter findings of a rejected job.
    pub findings: Vec<LintFinding>,
    /// The Bambu Connect import link of a handed-off job, as it was given to the opener.
    pub link: Option<String>,
    /// Where an exported or handed-off file was written, as Hotend sees it.
    pub path: Option<PathBuf>,
    /// The Windows form of a handed-off file's path (under WSL and on Windows): the path
    /// the link names.
    pub windows_path: Option<String>,
    /// Whether Bambu Connect was asked to import a handed-off file: true when a real opener
    /// started, false for the stand-in or when the opener failed.
    pub opened: Option<bool>,
    /// What was asked to open the link ([`LinkOpener::describe`]).
    pub opener: Option<String>,
}

/// A print job: proposed by the agent or the user, then confirmed by a person or not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrintJob {
    pub id: String,
    pub project_id: String,
    pub version: u32,
    /// SHA-256 of the sliced file the job refers to.
    pub slice_sha256: String,
    pub note: String,
    /// `pending`, `rejected`, `exported`, `handed_off`, `starting`, `not_started`,
    /// `printing`, `finished`, `failed` or `cancelled`.
    pub status: String,
    /// `simulated_printer`, `bambu_connect` or `export_to_folder`, once confirmed.
    pub delivery: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub detail: PrintJobDetail,
}

impl From<PendingPrintJob> for PrintJob {
    fn from(job: PendingPrintJob) -> Self {
        Self {
            detail: serde_json::from_value(job.detail).unwrap_or_default(),
            id: job.id,
            project_id: job.project_id,
            version: job.version,
            slice_sha256: job.slice_sha256,
            note: job.note,
            status: job.status,
            delivery: job.delivery,
            created_at_ms: job.created_at_ms,
            updated_at_ms: job.updated_at_ms,
        }
    }
}

/// Something that happened to a print job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PrintEvent {
    /// The job moved to another state; the job as it is now.
    Status { job: Box<PrintJob> },
    /// The printer moved to another phase.
    Phase { phase: String, headline: String },
    Progress {
        percent: u8,
        remaining_minutes: Option<u32>,
    },
    /// The printer reports a problem it did not report before.
    Problem { problem: PrinterProblem },
}

/// Everything the core reports while it works, on one channel ([`crate::Core::events`]).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "channel", rename_all = "snake_case")]
pub enum CoreEvent {
    /// An event of an agent run.
    Run {
        run_id: String,
        project_id: String,
        event: RunEvent,
    },
    /// An event of a print job.
    Print {
        job_id: String,
        project_id: String,
        /// True for the simulated printer.
        simulated: bool,
        event: PrintEvent,
    },
    /// What a printer followed over the local network reported (read-only: Hotend never
    /// sends it anything). `status.job_id` names the handed-off job the report was matched
    /// to by its name; without one it is the printer's status, not claimed as a Hotend job.
    Printer { status: Box<LanPrinterStatus> },
}

/// Why a print command failed.
#[derive(Debug, thiserror::Error)]
pub enum PrintError {
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The job is not waiting for a confirmation any more.
    #[error("print job {id} is `{status}`; only a pending job can be reviewed or confirmed")]
    NotPending { id: String, status: String },
    /// The sliced file is gone or is not the one the job was proposed for.
    #[error(
        "the sliced file of this print job is missing or has changed since it was proposed; \
         slice the version again and propose a new print"
    )]
    SliceChanged,
    /// The version was prepared again after the job's file was sliced.
    #[error(
        "the version was laid out on the plates again after this print was sliced, so the \
         sliced file is not what the preparation shows now; slice it again and propose a \
         new print"
    )]
    LayoutChanged,
    /// The job's file was sliced with Hotend's print settings for its plate, and those
    /// are no longer the plate's plan (the settings, the spool, the nozzle or the proven
    /// baseline changed since).
    #[error(
        "the print settings of this plate changed after this print was sliced (the settings, \
         the spool, the nozzle or the proven baseline), so the sliced file is not what \
         Hotend would print now; slice it again and propose a new print"
    )]
    PlanChanged,
    /// The G-code linter refused the file.
    #[error("the sliced file did not pass the G-code check and was not delivered: {first}")]
    Rejected {
        first: String,
        findings: Vec<LintFinding>,
    },
    /// The job was not put before a person since the core started.
    #[error("review the print job first (`print_review`), then confirm what it shows")]
    NotReviewed,
    /// What the person was shown does not contain the review text of the checked file.
    #[error(
        "the confirmation was not given for the review text of this print job: what was \
         shown does not contain it word for word. Nothing was confirmed; review the job again"
    )]
    SummaryMismatch,
    /// The version was sliced without a build plate chosen in the settings.
    #[error(
        "no build plate was chosen when this version was sliced, so the sliced file cannot \
         be checked for the plate on the printer. Choose the plate in the settings \
         (`plate_type`: {known}), slice the version again and propose a new print"
    )]
    PlateNotChosen { known: String },
    /// The settings name another build plate now than the version was sliced for.
    #[error(
        "this version was sliced for the {sliced}, and the settings now name {now}. Slice \
         the version again for the plate that is on the printer and propose a new print"
    )]
    PlateChanged { sliced: String, now: String },
    /// This build cannot deliver such a job.
    #[error("{0}")]
    Unsupported(String),
    /// No loaded spool matches a filament of the job.
    #[error(
        "no spool of {material} is loaded for nozzle {nozzle_id} (filament {slot} of the job); \
         load one in an AMS that feeds that nozzle, or change the material in the settings \
         and slice again"
    )]
    NoSpool {
        slot: u32,
        material: String,
        nozzle_id: i64,
    },
    /// The printer or the delivery refused.
    #[error("{0}")]
    Delivery(String),
    /// The hand-off folder is not chosen or cannot be used.
    #[error("{0}")]
    HandOffFolder(String),
    /// The editable project was made from a slice, a layout or a print plan that is not
    /// the current one any more.
    #[error(
        "this editable project was made from a slice or a print plan that is no longer the \
         current one; slice the plate again and make the project again"
    )]
    ProjectStale,
    /// The project file, or its test slice, is not the file that was made and checked.
    #[error(
        "the editable project's file is missing or is not the file Hotend made and sliced; \
         nothing was handed over. Make the project again"
    )]
    ProjectChanged,
    /// The sliced file lacks entries Bambu Connect needs to import it (the plate preview,
    /// first of all): a hand-off would be accepted by its import dialog and do nothing.
    /// `reason` says why, so that the window can offer the way on ([`not_importable`]).
    #[error("{message}")]
    NotImportable {
        message: String,
        reason: NotImportableReason,
    },
}

/// Why a sliced file cannot be handed to Bambu Connect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotImportableReason {
    /// The plate preview is missing, Hotend-made previews are on now, and the slice was
    /// made without them (before they were switched on): slicing again adds them.
    SliceBeforePreviews,
    /// The plate preview is missing and Hotend-made previews are off.
    PreviewsOff,
    /// Something else is missing (or previews were asked for and still are not there).
    Other,
}

impl NotImportableReason {
    /// A short stable sub-code of `not_importable`, for the interface; `None` for
    /// [`Self::Other`].
    pub fn code(self) -> Option<&'static str> {
        match self {
            Self::SliceBeforePreviews => Some("slice_before_previews"),
            Self::PreviewsOff => Some("previews_off"),
            Self::Other => None,
        }
    }
}

/// The refusal of a Bambu Connect hand-off of a sliced file that lacks `missing`, in words
/// that fit the settings now: a slice made before Hotend-made previews were switched on
/// only needs slicing again; with previews off, the person switches them on first. The
/// other ways (the editable project, a folder) are the next step's short second line.
/// `sliced_with_previews` is whether the slice record says Hotend-made previews were
/// asked for when it was made.
pub(crate) fn not_importable(
    missing: Vec<String>,
    previews: PreviewsMode,
    sliced_with_previews: bool,
) -> PrintError {
    let preview = missing.iter().find(|name| name.ends_with(".png")).cloned();
    let (reason, message) = match (preview, previews, sliced_with_previews) {
        (Some(preview), PreviewsMode::HotendMade, false) => (
            NotImportableReason::SliceBeforePreviews,
            format!(
                "This slice was made before Hotend drew plate previews. Slice again, then \
                 hand it off. Bambu Connect cannot import it as it is: it lacks the plate \
                 preview `{preview}`. Nothing was saved or sent."
            ),
        ),
        (Some(preview), PreviewsMode::Slicer, _) => (
            NotImportableReason::PreviewsOff,
            format!(
                "Bambu Connect cannot import this slice: it lacks the plate preview \
                 `{preview}`, which Bambu Studio on Linux cannot draw without a display. \
                 Switch on Hotend-made plate previews in Settings and slice again, then \
                 hand it off. Nothing was saved or sent."
            ),
        ),
        _ => (
            NotImportableReason::Other,
            hotend_printer::DeliveryError::NotImportable(missing).to_string(),
        ),
    };
    PrintError::NotImportable { message, reason }
}

impl PrintError {
    /// A short stable code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Store(StoreError::NotFound { .. }) => "not_found",
            Self::Store(_) => "storage_error",
            Self::NotPending { .. } => "not_pending",
            Self::SliceChanged | Self::LayoutChanged => "slice_changed",
            Self::PlanChanged => "plan_changed",
            Self::Rejected { .. } => "gcode_rejected",
            Self::NotReviewed => "not_reviewed",
            Self::SummaryMismatch => "summary_mismatch",
            Self::PlateNotChosen { .. } => "bed_type_not_chosen",
            Self::PlateChanged { .. } => "plate_changed",
            Self::Unsupported(_) => "unsupported",
            Self::NoSpool { .. } => "no_spool",
            Self::Delivery(_) => "delivery_failed",
            Self::HandOffFolder(_) => "handoff_folder",
            Self::ProjectStale => "project_stale",
            Self::ProjectChanged => "project_changed",
            Self::NotImportable { .. } => "not_importable",
        }
    }
}

fn delivery_error(error: impl std::fmt::Display) -> PrintError {
    PrintError::Delivery(truncate(&error.to_string(), 1000))
}

fn phase_name(phase: Phase) -> String {
    serde_json::to_value(phase)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

/// The warning that the size of the fitted nozzle was assumed: the slice record's job does
/// not say that the person set it (`nozzle_size_set` is false, or the record is older than
/// that field). The job is declared and checked for the size it was sliced for, and a file
/// sliced for another size is refused; whether the printer really has that nozzle is for
/// the person to check, so the review says so.
fn nozzle_size_assumed(declared: &DeclaredJob, record_job: &Value) -> Option<LintFinding> {
    if record_job.get("nozzle_size_set").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let sizes: std::collections::BTreeSet<String> = declared
        .filaments
        .iter()
        .map(|filament| format!("{}", filament.nozzle_diameter_mm))
        .collect();
    if sizes.is_empty() {
        return None;
    }
    let size = sizes.into_iter().collect::<Vec<_>>().join(" mm and ");
    Some(LintFinding {
        severity: LintSeverity::Warning,
        code: NOZZLE_SIZE_ASSUMED.to_owned(),
        line: 0,
        message: format!(
            "Hotend assumes a {size} mm nozzle: the nozzle size was not set in the settings. Check that the printer has a {size} mm nozzle before you confirm"
        ),
        excerpt: String::new(),
    })
}

/// The notice that the sliced file's plate previews were made by Hotend, when its slice
/// says so: the person sees, before confirming, that the pictures Bambu Connect and the
/// printer show are not Bambu Studio's.
fn hotend_made_previews(summary: &SliceSummary) -> Option<LintFinding> {
    let warning = summary
        .warnings
        .iter()
        .find(|warning| warning.code == crate::gateway::HOTEND_MADE_PREVIEWS)?;
    Some(LintFinding {
        severity: LintSeverity::Warning,
        code: crate::gateway::HOTEND_MADE_PREVIEWS.to_owned(),
        line: 0,
        message: truncate(&warning.message, 600),
        excerpt: String::new(),
    })
}

/// The warnings of a print review, in the order they are shown. The notice that the plate
/// previews are Hotend's comes first: the native dialog and the command line show only
/// the first warnings, and the linter can report many.
fn review_warnings(
    previews: Option<LintFinding>,
    built_for_other: Option<LintFinding>,
    lint: impl Iterator<Item = LintFinding>,
    nozzle_size_assumed: Option<LintFinding>,
) -> Vec<LintFinding> {
    previews
        .into_iter()
        .chain(built_for_other)
        .chain(lint)
        .chain(nozzle_size_assumed)
        .collect()
}

/// What a slice was made for, as the G-code check holds its file to it.
struct SliceDeclaration {
    declared: DeclaredJob,
    /// The plate of the version's layout that was sliced, from 1.
    layout_plate: u32,
    /// The names of the pieces on it.
    pieces: Vec<String>,
}

/// The first error of a report the G-code check refused, as a sentence, and its findings.
fn rejection(report: &hotend_printer::LintReport) -> (String, Vec<LintFinding>) {
    let findings: Vec<LintFinding> = report.findings.iter().take(50).map(Into::into).collect();
    let first = findings
        .iter()
        .find(|finding| finding.severity == LintSeverity::Error)
        .map(|finding| format!("line {}: {}", finding.line, finding.message))
        .unwrap_or_else(|| "see the findings".to_owned());
    (first, findings)
}

struct Review {
    job: Arc<CheckedJob>,
    delivery: DeliveryChoice,
    sources: Vec<FilamentSource>,
    /// How long the slicer estimated the print takes, from the slice record. The simulated
    /// printer runs for this long: a real slice states the same time in its file, and the
    /// stand-in slicer's file states none (it prints nothing) while its estimate is in the
    /// record.
    estimate: Option<Duration>,
    /// The version's folder and the layout the reviewed file was sliced from.
    version_dir: PathBuf,
    layout: Option<i64>,
    /// For a hand-off: the folder the review named.
    handoff: Option<HandOffFolder>,
}

/// The part of `shown`, the text a dialog displayed, that is `review_text`: the review
/// text in one piece, word for word. `None` when the dialog did not display it.
fn shown_review<'a>(shown: &'a str, review_text: &str) -> Option<&'a str> {
    if review_text.is_empty() {
        return None;
    }
    shown
        .find(review_text)
        .map(|start| &shown[start..start + review_text.len()])
}

/// The printer layer's confirmation of `request`, given for `shown`: the review text as
/// the dialog displayed it. The printer layer compares it with the request's own text
/// again and binds the confirmation to it.
fn confirmation(
    authority: &ConfirmationAuthority,
    request: ConfirmationRequest,
    shown: &str,
) -> Result<HumanConfirmation, PrintError> {
    authority
        .confirm(request, shown)
        .map_err(|error| match error {
            TokenError::NotWhatWasShown => PrintError::SummaryMismatch,
            other => delivery_error(other),
        })
}

struct Shared {
    store: Arc<Store>,
    options: PrinterOptions,
    profile: hotend_printer::PrinterProfile,
    printer: Mutex<SimulatedPrinter>,
    events: broadcast::Sender<CoreEvent>,
}

impl Shared {
    fn printer(&self) -> MutexGuard<'_, SimulatedPrinter> {
        self.printer.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn emit(&self, job: &PendingPrintJob, event: PrintEvent) {
        let _ = self.events.send(CoreEvent::Print {
            job_id: job.id.clone(),
            project_id: job.project_id.clone(),
            simulated: job.delivery.as_deref() == Some("simulated_printer"),
            event,
        });
    }

    /// Claims a pending job for a delivery: `pending` to `starting`, with one
    /// compare-and-set in the store. Fails with [`PrintError::NotPending`] when the job was
    /// cancelled or claimed first. Nothing is reported yet; the delivery reports its outcome.
    fn claim(&self, id: &str, delivery: &str) -> Result<(), PrintError> {
        let detail = PrintJobDetail {
            simulated: delivery == DeliveryChoice::SimulatedPrinter.as_str(),
            message: Some("confirmed; being delivered".to_owned()),
            ..PrintJobDetail::default()
        };
        let (job, changed) = self.store.update_print_job(
            id,
            &["pending"],
            "starting",
            Some(delivery),
            &json!(detail),
        )?;
        if changed {
            Ok(())
        } else {
            Err(PrintError::NotPending {
                id: job.id,
                status: job.status,
            })
        }
    }

    /// Gives a claimed job back after a delivery that delivered nothing: it is `pending`
    /// again, as it was.
    fn release(&self, id: &str) {
        let _ = self.store.release_print_job(id);
    }

    /// Moves a job to `status` (when it is in one of `from`), audits and reports it.
    fn transition(
        &self,
        id: &str,
        from: &[&str],
        status: &str,
        delivery: Option<&str>,
        detail: &PrintJobDetail,
    ) -> Result<PrintJob, PrintError> {
        self.transition_noting(id, from, status, delivery, detail, None)
    }

    /// [`Shared::transition`], with fields of its own added to the audit entry's detail.
    fn transition_noting(
        &self,
        id: &str,
        from: &[&str],
        status: &str,
        delivery: Option<&str>,
        detail: &PrintJobDetail,
        audit_extra: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Result<PrintJob, PrintError> {
        let (job, changed) =
            self.store
                .update_print_job(id, from, status, delivery, &json!(detail))?;
        if !changed {
            return Err(PrintError::NotPending {
                id: job.id,
                status: job.status,
            });
        }
        if job.status != "printing" || !from.contains(&"printing") {
            let mut audited = json!({
                "job": job.id,
                "delivery": job.delivery,
                "simulated": detail.simulated,
                "message": detail.message,
            });
            if let (Some(extra), Some(fields)) = (audit_extra, audited.as_object_mut()) {
                fields.extend(extra);
            }
            let _ = self.store.audit(NewAudit {
                project_id: Some(job.project_id.clone()),
                run_id: None,
                kind: "print",
                name: job.status.clone(),
                args_sha256: Some(job.slice_sha256.clone()),
                outcome: "ok".to_owned(),
                detail: audited,
            });
        }
        let shown = PrintJob::from(job.clone());
        self.emit(
            &job,
            PrintEvent::Status {
                job: Box::new(shown.clone()),
            },
        );
        Ok(shown)
    }
}

/// The audit text of an opener whose task did not finish.
const OPENER_TASK_FAILED: &str = "the opener's task failed";

/// A short fixed audit text for why an opener did not start: never the link, a path or a
/// system message, which can carry the project's name.
fn open_error_text(error: &OpenError) -> &'static str {
    match error {
        OpenError::Refused(_) => "real openers are switched off in this process",
        OpenError::Unsupported => "there is nothing to open it with on this system",
        OpenError::Spawn { .. } => "the opener's program could not be started",
        OpenError::Record(_) => "the stand-in opener could not write its record",
    }
}

/// The audit fields of an opener that was asked to open a hand-off: what it is, whether it
/// started anything (never for the stand-in) and, when it failed, a fixed short text. No
/// link or path: they can carry the project's name.
fn opener_audit(
    described: &str,
    stand_in: bool,
    asked: &Result<(), (String, &'static str)>,
) -> serde_json::Map<String, Value> {
    let mut fields = serde_json::Map::new();
    fields.insert("opener".to_owned(), json!(described));
    fields.insert(
        "opener_started".to_owned(),
        json!(asked.is_ok() && !stand_in),
    );
    if let Err((_, text)) = asked {
        fields.insert("opener_error".to_owned(), json!(text));
    }
    fields
}

/// The print commands of the facade.
pub(crate) struct PrintService {
    data: DataDir,
    shared: Arc<Shared>,
    reviews: Mutex<HashMap<String, Review>>,
    /// The reviewed editable projects, kept for their confirmation.
    project_reviews: Mutex<project::ProjectReviews>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    /// What opens Bambu Connect's import link: the stand-in until the application sets
    /// the real one.
    opener: RwLock<Arc<dyn LinkOpener>>,
    /// The host the hand-off path is made for: the local path until the application or
    /// the command line says otherwise.
    host: RwLock<HostKind>,
}

impl Drop for PrintService {
    fn drop(&mut self) {
        for task in self
            .tasks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain(..)
        {
            task.abort();
        }
    }
}

impl PrintService {
    pub(crate) fn new(
        data: DataDir,
        store: Arc<Store>,
        options: PrinterOptions,
        events: broadcast::Sender<CoreEvent>,
    ) -> Result<Self, String> {
        let profile = crate::profiles::printer_layer_profile(crate::profiles::DEFAULT_PRINTER_ID)
            .ok_or("the printer layer has no profile for the default printer")?;
        let mut config = SimConfig::x2d(&profile);
        config.time_scale = options.time_scale.clamp(1.0, 100_000.0);
        config.identity.name = "Simulated X2D (no real printer)".to_owned();
        if options.stays_idle_after_start {
            config.start_quirk = StartQuirk::AcceptedButStaysIdle;
        }
        let printer = SimulatedPrinter::new(profile.clone(), config);
        Ok(Self {
            data,
            shared: Arc::new(Shared {
                store,
                options,
                profile,
                printer: Mutex::new(printer),
                events,
            }),
            reviews: Mutex::new(HashMap::new()),
            project_reviews: Mutex::new(HashMap::new()),
            tasks: Mutex::new(Vec::new()),
            opener: RwLock::new(Arc::new(StandInOpener::new())),
            host: RwLock::new(HostKind::Linux),
        })
    }

    /// Sets what opens Bambu Connect's import link after a confirmed hand-off.
    pub(crate) fn set_link_opener(&self, opener: Arc<dyn LinkOpener>) {
        *self.opener.write().unwrap_or_else(|p| p.into_inner()) = opener;
    }

    fn opener(&self) -> Arc<dyn LinkOpener> {
        Arc::clone(&self.opener.read().unwrap_or_else(|p| p.into_inner()))
    }

    /// Sets the host the hand-off path is made for.
    pub(crate) fn set_handoff_host(&self, host: HostKind) {
        *self.host.write().unwrap_or_else(|p| p.into_inner()) = host;
    }

    pub(crate) fn handoff_host(&self) -> HostKind {
        *self.host.read().unwrap_or_else(|p| p.into_inner())
    }

    /// The hand-off as it is set up now.
    pub(crate) fn handoff_status(&self) -> Result<HandOffStatus, PrintError> {
        let opener = self.opener();
        let chosen = self.settings()?.handoff_folder;
        let (folder, windows_folder, problem) = match self.handoff_folder() {
            Ok(folder) => (
                Some(folder.path().to_path_buf()),
                folder.windows_path().map(str::to_owned),
                None,
            ),
            Err(error) => (None, None, Some(error.to_string())),
        };
        Ok(HandOffStatus {
            host: self.handoff_host().as_str().to_owned(),
            chosen,
            folder,
            windows_folder,
            problem,
            opener: opener.describe().to_owned(),
            opens: !opener.is_stand_in(),
        })
    }

    /// `Ok` when `folder` could be used as the hand-off folder on this host, as
    /// [`Self::handoff_folder`] would check it once it is in the settings. Nothing is
    /// saved, made or written.
    pub(crate) fn handoff_folder_check(&self, folder: &std::path::Path) -> Result<(), PrintError> {
        HandOffFolder::new(folder, self.handoff_host())
            .map(|_| ())
            .map_err(|error| {
                PrintError::HandOffFolder(format!(
                    "the hand-off folder for Bambu Connect cannot be used: {error}"
                ))
            })
    }

    /// The hand-off folder: the one the settings name, else, except under WSL,
    /// `<data-root>/handoff` (made when missing). Under WSL a folder must be chosen, on a
    /// Windows drive.
    pub(crate) fn handoff_folder(&self) -> Result<HandOffFolder, PrintError> {
        let host = self.handoff_host();
        let unusable = |error: hotend_printer::HandOffPathError| {
            PrintError::HandOffFolder(format!(
                "the hand-off folder for Bambu Connect cannot be used: {error}"
            ))
        };
        match self.settings()?.handoff_folder {
            Some(folder) => HandOffFolder::new(&folder, host).map_err(unusable),
            None if host == HostKind::Wsl => Err(PrintError::HandOffFolder(
                "Bambu Connect runs on Windows, and Hotend runs in WSL: choose a folder on a \
                 Windows drive (for example D:\\Models\\Hotend) as the Bambu Connect hand-off \
                 folder in Settings, so that Bambu Connect can read the file"
                    .to_owned(),
            )),
            None => {
                let folder = self.handoff_dir();
                std::fs::create_dir_all(&folder).map_err(delivery_error)?;
                HandOffFolder::new(&folder, host).map_err(unusable)
            }
        }
    }

    fn reviews(&self) -> MutexGuard<'_, HashMap<String, Review>> {
        self.reviews.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Drops the last review of a print job: what it showed may no longer be confirmed.
    pub(crate) fn forget_review(&self, id: &str) {
        self.reviews().remove(id);
    }

    /// Why the held review of a print job could not be confirmed now, as the confirmation
    /// would say it, when the reason is one of its own: no review, another plate in the
    /// settings, another layout. `None` otherwise. Nothing is consumed.
    pub(crate) fn held_review_problem(&self, id: &str) -> Option<PrintError> {
        let (plate, version_dir, layout) = {
            let reviews = self.reviews();
            let Some(review) = reviews.get(id) else {
                return Some(PrintError::NotReviewed);
            };
            (
                review.job.declared().plate_type.clone(),
                review.version_dir.clone(),
                review.layout,
            )
        };
        if let Err(error) = self.plate_still_chosen(&plate) {
            return Some(error);
        }
        (layout_time(&version_dir) != layout).then_some(PrintError::LayoutChanged)
    }

    /// Drops the last review of an editable project.
    pub(crate) fn forget_project_review(&self, id: &str) {
        self.project_reviews
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(id);
    }

    fn printer_ref(&self) -> PrinterRef {
        let printer = self.shared.printer();
        let identity = printer.identity();
        PrinterRef {
            id: SIMULATED_PRINTER_ID.to_owned(),
            name: identity.name.clone(),
            model: identity.model.clone(),
            serial: identity.serial.as_str().to_owned(),
            simulated: true,
        }
    }

    /// The user's settings as they are now.
    fn settings(&self) -> Result<Settings, PrintError> {
        match self.shared.store.setting(SETTINGS_KEY)? {
            Some(value) => serde_json::from_value(value).map_err(|source| {
                PrintError::Store(StoreError::Corrupt {
                    what: "the settings",
                    source,
                })
            }),
            None => Ok(Settings::default()),
        }
    }

    /// `Ok` when the settings name, now, the build plate `plate_name` (the printer
    /// layer's name of it): the plate a version was sliced for is still the plate the
    /// person says is on the printer.
    fn plate_still_chosen(&self, plate_name: &str) -> Result<(), PrintError> {
        let chosen_now = self.settings()?.plate_type;
        let same = chosen_now
            .as_deref()
            .and_then(|id| self.shared.profile.plate_by_id(id))
            .is_some_and(|plate| plate.name == plate_name);
        if same {
            return Ok(());
        }
        Err(PrintError::PlateChanged {
            sliced: plate_name.to_owned(),
            now: chosen_now.map_or_else(
                || "no plate".to_owned(),
                |id| format!("`{}`", truncate(&id, 64)),
            ),
        })
    }

    /// The profile of the printer the core plans and checks for.
    pub(crate) fn profile(&self) -> hotend_printer::PrinterProfile {
        self.shared.profile.clone()
    }

    /// A follower of a real printer's status reports, which matches them to a handed-off
    /// job when the job's name says so ([`Follower`]).
    pub(crate) fn follower(&self) -> Follower {
        Follower::new(Arc::clone(&self.shared))
    }

    fn state(&self) -> Result<PrinterState, PrintError> {
        let status = self.shared.printer().status().map_err(delivery_error)?;
        Ok(state_of(&status, &self.shared.profile))
    }

    /// The printers of this build (the simulated one) and the ways it can deliver. What
    /// each way can and cannot do is what its own [`Delivery::capabilities`] says.
    pub(crate) fn printers(&self) -> Result<Vec<PrinterInfo>, PrintError> {
        let simulated = PrinterDelivery::new(&mut *self.shared.printer()).capabilities();
        let handoff = BambuConnectHandOff::description().capabilities();
        let export = ExportToFolder::new(self.data.root()).capabilities();
        let mut connect = DeliveryInfo::new("bambu_connect", handoff);
        // A hand-off folder chosen in the settings that cannot be used (missing, not a
        // folder, not on a Windows drive under WSL, a comma in its Windows path) makes the
        // way unavailable on every host. Under WSL a folder on a Windows drive must be
        // chosen first, which only the person can do: until then the way is listed, and
        // says why it cannot be chosen.
        let host = self.handoff_host();
        match self.settings()?.handoff_folder {
            Some(folder) => {
                if let Err(error) = HandOffFolder::new(&folder, host) {
                    connect.enabled = false;
                    connect.disabled_reason = Some(format!(
                        "The hand-off folder chosen in Settings cannot be used: {error}."
                    ));
                }
            }
            None if host == HostKind::Wsl => {
                connect.enabled = false;
                connect.disabled_reason = Some(
                    "Choose a folder on a Windows drive for the Bambu Connect hand-off in \
                     Settings first: Bambu Connect runs on Windows and reads the file from \
                     there."
                        .to_owned(),
                );
            }
            None => {}
        }
        Ok(vec![PrinterInfo {
            printer: self.printer_ref(),
            state: self.state()?,
            deliveries: vec![
                DeliveryInfo::new("simulated_printer", simulated),
                connect,
                DeliveryInfo::new("export_to_folder", export),
            ],
        }])
    }

    fn handoff_dir(&self) -> PathBuf {
        self.data.root().join("handoff")
    }

    /// What the simulated printer was sent, oldest first: a diagnostic for tests and the
    /// doctor report.
    pub(crate) fn wire_log(&self) -> Vec<String> {
        self.shared.printer().wire_log().to_vec()
    }

    pub(crate) fn jobs(&self, project_id: &str) -> Result<Vec<PrintJob>, PrintError> {
        Ok(self
            .shared
            .store
            .list_pending_prints(project_id)?
            .into_iter()
            .map(PrintJob::from)
            .collect())
    }

    pub(crate) fn job(&self, id: &str) -> Result<PrintJob, PrintError> {
        Ok(self.shared.store.print_job(id)?.into())
    }

    /// Runs the linter over the job's sliced file and returns what a person must see
    /// before confirming. A file the linter refuses marks the job `rejected`.
    pub(crate) async fn review(
        &self,
        id: &str,
        delivery: DeliveryChoice,
    ) -> Result<PrintReview, PrintError> {
        // A new review replaces the last one, also when it fails: what was shown before
        // is not what would be shown now.
        self.reviews().remove(id);
        let store = &self.shared.store;
        let job = store.print_job(id)?;
        if job.status != "pending" {
            return Err(PrintError::NotPending {
                id: job.id,
                status: job.status,
            });
        }
        let record = store
            .slice(&job.project_id, job.version, &job.profile_hash)?
            .ok_or(PrintError::SliceChanged)?;
        let summary: SliceSummary =
            serde_json::from_value(record.summary.clone()).map_err(|_| PrintError::SliceChanged)?;
        // The rule of `slice_get` and `print_propose`: a slice counts only for the layout
        // it was made from.
        let version_dir = self
            .data
            .version_dir(&job.project_id, job.version)
            .map_err(|_| PrintError::SliceChanged)?;
        let sliced_layout = record.job.get("prepared_at_ms").and_then(Value::as_i64);
        if sliced_layout.is_none() || sliced_layout != layout_time(&version_dir) {
            return Err(PrintError::LayoutChanged);
        }
        let SliceDeclaration {
            declared,
            layout_plate,
            pieces,
        } = self.declaration(&record, &summary)?;
        let output_dir = version_dir.join("slice").join(&job.profile_hash);
        let project = store.project(&job.project_id)?;
        let name = job_name(&project.name, job.version);
        let checked = self
            .check_file(output_dir, summary.output.clone(), name, declared)
            .await?;
        let checked = match checked {
            Ok(checked) => checked,
            Err(report) => {
                let (first, findings) = rejection(&report);
                let detail = PrintJobDetail {
                    message: Some(format!("the G-code check refused the file: {first}")),
                    findings: findings.clone(),
                    ..PrintJobDetail::default()
                };
                self.shared
                    .transition(id, &["pending"], "rejected", None, &detail)?;
                return Err(PrintError::Rejected { first, findings });
            }
        };
        if !checked.sha256().eq_ignore_ascii_case(&job.slice_sha256) {
            return Err(PrintError::SliceChanged);
        }
        let (mut sources, mut spools) = (Vec::new(), Vec::new());
        let simulated = delivery == DeliveryChoice::SimulatedPrinter;
        // Bambu Connect accepts a file without the plate preview in its import dialog and
        // then imports nothing: such a file is not offered to it at all.
        if delivery == DeliveryChoice::BambuConnect {
            let missing = hotend_printer::bambu_connect_missing(&checked);
            if !missing.is_empty() {
                let sliced_with_previews =
                    record.job.get("previews").and_then(Value::as_str) == Some("hotend_made");
                return Err(not_importable(
                    missing,
                    self.settings()?.previews,
                    sliced_with_previews,
                ));
            }
        }
        // A hand-off names its folder before anything is confirmed: the dialog says where
        // the file goes, and the confirmation places it there and nowhere else.
        let handoff = match delivery {
            DeliveryChoice::BambuConnect => Some(self.handoff_folder()?),
            _ => None,
        };
        let opener = self.opener();
        let handoff_target = handoff.as_ref().map(|folder| HandOffTarget {
            folder: folder.path().to_path_buf(),
            windows_folder: folder.windows_path().map(str::to_owned),
            opener: opener.describe().to_owned(),
            opens: !opener.is_stand_in(),
        });
        if simulated {
            let status = self.shared.printer().status().map_err(delivery_error)?;
            for filament in &checked.declared().filaments {
                let spool = find_spool(&status, filament).ok_or_else(|| PrintError::NoSpool {
                    slot: filament.slot,
                    material: filament.material.clone(),
                    nozzle_id: i64::from(filament.nozzle),
                })?;
                sources.push(FilamentSource::Ams {
                    unit: spool.ams_unit,
                    tray: spool.tray,
                });
                spools.push(spool);
            }
        }
        // A version built for another printer, material or calibration than it was sliced
        // and is printed for: the person sees it before confirming.
        let built_for_other = self.built_for_other(&record, &version_dir, job.version);
        let review = PrintReview {
            job: job.into(),
            simulated,
            printer: simulated.then(|| self.printer_ref()),
            plate: layout_plate,
            pieces,
            summary: summary_of(&checked),
            review_text: checked.summary().review_text(),
            warnings: review_warnings(
                hotend_made_previews(&summary),
                built_for_other,
                checked.lint().findings.iter().map(Into::into),
                nozzle_size_assumed(checked.declared(), &record.job),
            ),
            spools,
            delivery: delivery.clone(),
            web_numbers: Vec::new(),
            handoff: handoff_target,
        };
        self.reviews().insert(
            id.to_owned(),
            Review {
                job: Arc::new(checked),
                delivery,
                sources,
                estimate: Some(summary.time_s)
                    .filter(|seconds| seconds.is_finite() && *seconds >= 1.0)
                    .map(|seconds| Duration::from_secs_f64(seconds.min(1.0e7))),
                version_dir,
                layout: sliced_layout,
                handoff,
            },
        );
        Ok(review)
    }

    /// The warning that the version was built for another printer, material or
    /// calibration than the slice `record` was made for, if it was.
    fn built_for_other(
        &self,
        record: &crate::store::SliceRecord,
        version_dir: &std::path::Path,
        version: u32,
    ) -> Option<LintFinding> {
        let material_id = record
            .job
            .pointer("/material/id")
            .and_then(Value::as_str)
            .unwrap_or("");
        crate::modeling::BuiltFor::print_warning(
            version_dir,
            version,
            self.shared.profile.id(),
            material_id,
            &self
                .data
                .root()
                .join("calibration")
                .join(format!("{}.json", self.shared.profile.id())),
        )
        .map(|message| LintFinding {
            severity: LintSeverity::Warning,
            code: BUILT_FOR_OTHER.to_owned(),
            line: 0,
            message,
            excerpt: String::new(),
        })
    }

    /// Runs the G-code check over `output` (relative to `dir`) as the job `name` held to
    /// `declared`. `Ok(Err(report))` is a file the check refused.
    async fn check_file(
        &self,
        dir: PathBuf,
        output: String,
        name: String,
        declared: DeclaredJob,
    ) -> Result<Result<CheckedJob, Box<hotend_printer::LintReport>>, PrintError> {
        let profile = self.shared.profile.clone();
        tokio::task::spawn_blocking(move || {
            let path = import::resolve_inside(&dir, &output, MAX_SLICE_BYTES)
                .map_err(|_| PrintError::SliceChanged)?;
            match hotend_printer::check_job(
                &path,
                &name,
                &declared,
                &profile,
                &ArchiveLimits::default(),
            ) {
                Ok(job) => Ok(Ok(job)),
                Err(JobError::Rejected(report)) => Ok(Err(report)),
                Err(error) => Err(delivery_error(error)),
            }
        })
        .await
        .map_err(delivery_error)
        .and_then(|checked| checked)
    }

    /// The job the G-code check holds the file of the slice `record` to: what the slice
    /// was made for (printer, build plate, material, nozzles), never what the file says.
    /// Refused for another printer than this build's, for a slice without a chosen build
    /// plate, for a plate the check has no facts about, and when the settings name another
    /// plate now.
    fn declaration(
        &self,
        record: &crate::store::SliceRecord,
        summary: &SliceSummary,
    ) -> Result<SliceDeclaration, PrintError> {
        let layout_plate = record
            .job
            .get("plate")
            .and_then(Value::as_u64)
            .and_then(|plate| u32::try_from(plate).ok())
            .unwrap_or(ARCHIVE_PLATE);
        let pieces: Vec<String> = record
            .job
            .get("pieces")
            .and_then(Value::as_array)
            .map(|pieces| {
                pieces
                    .iter()
                    .filter_map(Value::as_str)
                    .take(200)
                    .map(|name| truncate(name, 80))
                    .collect()
            })
            .unwrap_or_default();
        let printer_id = record.job.get("printer_id").and_then(Value::as_str);
        if printer_id != Some(self.shared.profile.id()) {
            return Err(PrintError::Unsupported(format!(
                "this build checks and delivers print jobs for {} only; this version was \
                 sliced for `{}`. The sliced file is in the version's slice folder if you want \
                 to print it yourself",
                self.shared.profile.name(),
                truncate(printer_id.unwrap_or("an unknown printer"), 80),
            )));
        }
        let family = record
            .job
            .pointer("/material/family")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        // The build plate: the one the person chose in the settings when the version was
        // sliced, by the name the printer layer has for that id. It is never taken from
        // the sliced file, which is what is being checked against it.
        let known_plates = || {
            let ids: Vec<&str> = self
                .shared
                .profile
                .plates()
                .iter()
                .map(|plate| plate.id.as_str())
                .collect();
            ids.join(", ")
        };
        let plate_id = record
            .job
            .get("plate_type")
            .and_then(Value::as_str)
            .ok_or_else(|| PrintError::PlateNotChosen {
                known: known_plates(),
            })?;
        let plate = self
            .shared
            .profile
            .plate_by_id(plate_id)
            .ok_or_else(|| {
                PrintError::Unsupported(format!(
                    "this version was sliced for the build plate `{}`, and the G-code check \
                     has no facts about that plate (it knows: {}), so the file cannot be \
                     checked or delivered by Hotend. The sliced file is in the version's \
                     slice folder if you want to print it yourself",
                    truncate(plate_id, 64),
                    known_plates(),
                ))
            })?
            .clone();
        self.plate_still_chosen(&plate.name)?;
        let mut declared = DeclaredJob {
            plate: ARCHIVE_PLATE,
            plate_type: plate.name.clone(),
            filaments: Vec::new(),
        };
        for filament in &summary.filament {
            // The nozzle size the slice was made for: the printer profile's size of the
            // filament's nozzle, as the slice recorded it.
            let nozzle_diameter_mm = record
                .job
                .get("nozzles")
                .and_then(Value::as_array)
                .and_then(|nozzles| {
                    nozzles
                        .iter()
                        .find(|nozzle| nozzle["id"].as_i64() == Some(filament.nozzle_id))
                })
                .and_then(|nozzle| nozzle["diameter_mm"].as_f64())
                .ok_or(PrintError::SliceChanged)?;
            declared.filaments.push(DeclaredFilament {
                slot: filament.slot,
                material: family.clone(),
                nozzle: u8::try_from(filament.nozzle_id).map_err(|_| PrintError::SliceChanged)?,
                nozzle_diameter_mm,
            });
        }
        Ok(SliceDeclaration {
            declared,
            layout_plate,
            pieces,
        })
    }

    /// Delivers a reviewed job, on a person's confirmation. `shown` is the text the
    /// person was shown before they confirmed: everything the dialog or the terminal
    /// displayed for the job. It must contain the review text of the reviewed file in one
    /// piece, word for word ([`PrintReview::review_text`]); that piece is what the printer
    /// layer's confirmation is given for and bound to.
    pub(crate) async fn confirm(
        &self,
        authority: &ConfirmationAuthority,
        id: &str,
        shown: &str,
    ) -> Result<PrintJob, PrintError> {
        let review = self.reviews().remove(id).ok_or(PrintError::NotReviewed)?;
        let job = self.shared.store.print_job(id)?;
        if job.status != "pending" {
            return Err(PrintError::NotPending {
                id: job.id,
                status: job.status,
            });
        }
        if !review.job.sha256().eq_ignore_ascii_case(&job.slice_sha256) {
            return Err(PrintError::SliceChanged);
        }
        // The plate on the printer may have been changed in the settings since the review.
        self.plate_still_chosen(&review.job.declared().plate_type)?;
        // The version may have been laid out again while the review was on the screen.
        if layout_time(&review.version_dir) != review.layout {
            return Err(PrintError::LayoutChanged);
        }
        let checked = Arc::clone(&review.job);
        let review_text = checked.summary().review_text();
        let shown = shown_review(shown, &review_text).ok_or(PrintError::SummaryMismatch)?;
        let kind = review.delivery.as_str();
        match review.delivery {
            DeliveryChoice::SimulatedPrinter => {
                let identity = self.shared.printer().identity().clone();
                let confirmation = confirmation(
                    authority,
                    ConfirmationRequest::start_print(&checked, &identity),
                    shown,
                )?;
                let shared = Arc::clone(&self.shared);
                let sources = review.sources;
                let job_file = Arc::clone(&checked);
                let estimate = review.estimate;
                let job_id = id.to_owned();
                let watchdog = tokio::task::spawn_blocking(move || {
                    // The claim, the upload and the start are one step under the printer's
                    // lock. `cancel` stops a claimed job under the same lock, so it acts
                    // either before the claim (nothing is sent) or after the start.
                    let mut printer = shared.printer();
                    shared.claim(&job_id, kind)?;
                    // The simulation lasts as long as the slicer estimated; without an
                    // estimate the simulated printer goes by the file's own statement.
                    printer.config_mut().print = estimate;
                    let started = (|| {
                        let uploaded = match PrinterDelivery::new(&mut *printer)
                            .deliver(&job_file)
                            .map_err(delivery_error)?
                        {
                            Delivered::OnPrinter(uploaded) => uploaded,
                            _ => return Err(delivery_error("the printer did not take the file")),
                        };
                        let token = StartToken::mint(confirmation, StartToken::DEFAULT_TTL)
                            .map_err(delivery_error)?;
                        start_print(
                            &mut *printer,
                            token,
                            &job_file,
                            &uploaded,
                            &sources,
                            StartOptions::default(),
                        )
                        .map_err(delivery_error)
                    })();
                    if started.is_err() {
                        // Nothing was started: the job waits for a confirmation again.
                        shared.release(&job_id);
                    }
                    started
                })
                .await
                .map_err(delivery_error)??
                .with_deadline(self.shared.options.start_deadline);
                // From here on the printer is running the job: it is followed whatever
                // else happens.
                let task = tokio::spawn(follow(Arc::clone(&self.shared), id.to_owned(), watchdog));
                {
                    let mut tasks = self.tasks.lock().unwrap_or_else(|p| p.into_inner());
                    tasks.retain(|task| !task.is_finished());
                    tasks.push(task);
                }
                let detail = PrintJobDetail {
                    simulated: true,
                    message: Some(
                        "sent to the simulated printer; waiting for it to begin".to_owned(),
                    ),
                    ..PrintJobDetail::default()
                };
                // The follower may already have moved the job on.
                let reported =
                    self.shared
                        .transition(id, &["starting"], "starting", Some(kind), &detail);
                match reported {
                    Ok(started) => Ok(started),
                    Err(PrintError::NotPending { .. }) => self.job(id),
                    Err(error) => {
                        // The record cannot say that the job runs, so it must not run.
                        let _ = self.shared.printer().send(Command::Stop);
                        Err(error)
                    }
                }
            }
            DeliveryChoice::BambuConnect => {
                let confirmation =
                    confirmation(authority, ConfirmationRequest::hand_off(&checked), shown)?;
                let folder = review.handoff.ok_or(PrintError::NotReviewed)?;
                self.shared.claim(id, kind)?;
                // The checked bytes are placed, read again, and only then is the link made:
                // the link names the file that passed the check and was shown.
                let placed = tokio::task::spawn_blocking(move || {
                    match BambuConnectHandOff::new(folder)
                        .deliver(&checked)
                        .map_err(delivery_error)?
                    {
                        Delivered::HandOff(placed) => {
                            let link = placed
                                .link(&checked, confirmation)
                                .map_err(delivery_error)?;
                            Ok((
                                link,
                                placed.path().to_path_buf(),
                                placed.windows_path().map(str::to_owned),
                            ))
                        }
                        _ => Err(delivery_error("the hand-off did not place the file")),
                    }
                })
                .await
                .map_err(delivery_error)
                .and_then(|placed| placed);
                let (link, path, windows_path) = match placed {
                    Ok(placed) => placed,
                    Err(error) => {
                        self.shared.release(id);
                        return Err(error);
                    }
                };
                let opener = self.opener();
                let described = opener.describe().to_owned();
                let stand_in = opener.is_stand_in();
                let asked = {
                    let link = link.clone();
                    tokio::task::spawn_blocking(move || opener.open(&link))
                        .await
                        .map_err(|error| (error.to_string(), OPENER_TASK_FAILED))
                        .and_then(|opened| {
                            opened.map_err(|error| (error.to_string(), open_error_text(&error)))
                        })
                };
                let audit = opener_audit(&described, stand_in, &asked);
                let asked = asked.map_err(|(error, _)| error);
                let after = "Hotend will not learn how the print goes";
                let (opened, message) = match asked {
                    Ok(()) if !stand_in => (
                        true,
                        format!(
                            "saved to the hand-off folder, and Bambu Connect was asked to import \
                             it. Choose the printer and press Print in Bambu Connect. {after}"
                        ),
                    ),
                    Ok(()) => (
                        false,
                        format!(
                            "saved to the hand-off folder; Bambu Connect was not opened \
                             ({described}). Import the file in Bambu Connect, choose the \
                             printer and press Print there. {after}"
                        ),
                    ),
                    Err(error) => (
                        false,
                        format!(
                            "saved to the hand-off folder, but Bambu Connect could not be \
                             opened: {}. Import the file in Bambu Connect, choose the printer \
                             and press Print there. {after}",
                            truncate(&error, 300)
                        ),
                    ),
                };
                let detail = PrintJobDetail {
                    message: Some(message),
                    link: Some(link.as_str().to_owned()),
                    path: Some(path),
                    windows_path,
                    opened: Some(opened),
                    opener: Some(described),
                    ..PrintJobDetail::default()
                };
                self.shared.transition_noting(
                    id,
                    &["starting"],
                    "handed_off",
                    Some(kind),
                    &detail,
                    Some(audit),
                )
            }
            DeliveryChoice::ExportToFolder { folder } => {
                // An export starts nothing, but it leaves Hotend on a person's word too.
                let _confirmed =
                    confirmation(authority, ConfirmationRequest::hand_off(&checked), shown)?;
                self.shared.claim(id, kind)?;
                let written = tokio::task::spawn_blocking(move || {
                    match ExportToFolder::new(folder)
                        .deliver(&checked)
                        .map_err(delivery_error)?
                    {
                        Delivered::Exported { path } => Ok(path),
                        _ => Err(delivery_error("the export did not write the file")),
                    }
                })
                .await
                .map_err(delivery_error)
                .and_then(|written| written);
                let path = match written {
                    Ok(path) => path,
                    Err(error) => {
                        self.shared.release(id);
                        return Err(error);
                    }
                };
                let detail = PrintJobDetail {
                    message: Some(
                        "written to the folder; nothing was sent to a printer".to_owned(),
                    ),
                    path: Some(path),
                    ..PrintJobDetail::default()
                };
                self.shared
                    .transition(id, &["starting"], "exported", Some(kind), &detail)
            }
        }
    }

    /// Stops a job that is starting or printing on the simulated printer, or withdraws a
    /// pending one.
    pub(crate) fn cancel(&self, id: &str) -> Result<PrintJob, PrintError> {
        let mut job = self.shared.store.print_job(id)?;
        if job.status == "pending" {
            self.reviews().remove(id);
            let detail = PrintJobDetail {
                message: Some("withdrawn before it was confirmed".to_owned()),
                ..PrintJobDetail::default()
            };
            match self
                .shared
                .transition(id, &["pending"], "cancelled", None, &detail)
            {
                // A confirmation claimed the job in between: it is stopped below.
                Err(PrintError::NotPending { .. }) => job = self.shared.store.print_job(id)?,
                outcome => return outcome,
            }
        }
        let on_printer = job.delivery.as_deref() == Some(DeliveryChoice::SimulatedPrinter.as_str());
        match job.status.as_str() {
            "starting" | "printing" if on_printer => {
                // Taking the printer waits for a delivery that is under way, so the stop
                // reaches a job that was started and is not lost before the start.
                let mut printer = self.shared.printer();
                let now = self.shared.store.print_job(id)?;
                if !matches!(now.status.as_str(), "starting" | "printing") {
                    // The delivery failed in the meantime and gave the job back.
                    drop(printer);
                    return self.cancel(id);
                }
                printer.send(Command::Stop).map_err(delivery_error)?;
                // The follower reports the end when the printer does.
                Ok(now.into())
            }
            _ => Err(PrintError::NotPending {
                id: job.id,
                status: job.status,
            }),
        }
    }
}

/// When the stored layout of a version was made (`created_at_ms` of its `prepare.json`);
/// `None` when the version has no readable preparation.
fn layout_time(version_dir: &std::path::Path) -> Option<i64> {
    const MAX_PREPARE_BYTES: u64 = 8 * 1024 * 1024;
    let path = version_dir.join("prepare").join("prepare.json");
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_PREPARE_BYTES {
        return None;
    }
    let preparation: Value = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
    preparation.get("created_at_ms").and_then(Value::as_i64)
}

/// A file name for the printer from the project's name: ASCII letters, digits, `-` and `_`.
fn job_name(project: &str, version: u32) -> String {
    let mut slug: String = project
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .take(40)
        .collect();
    if slug.trim_matches('_').is_empty() {
        slug = "hotend".to_owned();
    }
    format!("{slug}-v{version}")
}

/// The first loaded tray of an AMS that feeds the filament's nozzle and holds its material.
fn find_spool(status: &PrinterStatus, filament: &DeclaredFilament) -> Option<SpoolChoice> {
    for unit in &status.ams {
        if unit.nozzle != Some(filament.nozzle) || unit.is_drying() {
            continue;
        }
        for tray in &unit.trays {
            let Some(material) = tray.material.as_deref().filter(|_| tray.is_loaded()) else {
                continue;
            };
            let family = material.split([' ', '-']).next().unwrap_or(material);
            if family.eq_ignore_ascii_case(&filament.material) {
                return Some(SpoolChoice {
                    slot: filament.slot,
                    material: filament.material.clone(),
                    nozzle_id: i64::from(filament.nozzle),
                    ams_unit: unit.id,
                    tray: tray.id,
                    loaded: format!(
                        "{}{}",
                        tray.product.as_deref().unwrap_or(material),
                        tray.colour
                            .as_ref()
                            .map(|c| format!(" #{}", c.hex()))
                            .unwrap_or_default()
                    ),
                });
            }
        }
    }
    None
}

fn state_of(status: &PrinterStatus, profile: &hotend_printer::PrinterProfile) -> PrinterState {
    PrinterState {
        phase: phase_name(status.phase()),
        headline: hotend_printer::explain::headline(status),
        progress_percent: status.progress_percent,
        remaining_minutes: status.remaining_minutes,
        layer: status.layer,
        total_layers: status.total_layers,
        job_name: status.job_name.clone(),
        bed_c: status.bed.current,
        problems: hotend_printer::explain::problems(status, profile)
            .into_iter()
            .map(Into::into)
            .collect(),
        ams: status
            .ams
            .iter()
            .map(|unit| AmsInfo {
                id: unit.id,
                high_temperature: unit.high_temperature,
                nozzle_id: unit.nozzle.map(i64::from),
                drying_minutes_left: unit.drying_minutes_left,
                trays: unit
                    .trays
                    .iter()
                    .map(|tray| TrayInfo {
                        id: tray.id,
                        material: tray.material.clone(),
                        product: tray.product.clone(),
                        colour: tray.colour.as_ref().map(|c| format!("#{}", c.hex())),
                        remaining_percent: tray.remaining_percent,
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// Follows a started job on the simulated printer: the start watchdog first, then the
/// phases, the progress and the end. Every step is stored with the job and reported.
async fn follow(shared: Arc<Shared>, id: String, mut watchdog: StartWatchdog) {
    let begun = Instant::now();
    let scale = shared.options.time_scale.clamp(1.0, 100_000.0);
    let mut tracker = JobTracker::new();
    let mut started = false;
    let mut detail = PrintJobDetail {
        simulated: true,
        ..PrintJobDetail::default()
    };
    let end = |status: &str, detail: &PrintJobDetail| {
        let _ = shared.transition(&id, &["starting", "printing"], status, None, detail);
    };
    loop {
        tokio::time::sleep(shared.options.poll_interval).await;
        let status = match shared.printer().status() {
            Ok(status) => status,
            Err(error) => {
                detail.message = Some(truncate(&error.to_string(), 500));
                return end("failed", &detail);
            }
        };
        detail.phase = Some(phase_name(status.phase()));
        detail.headline = Some(hotend_printer::explain::headline(&status));
        detail.progress_percent = status.progress_percent;
        detail.remaining_minutes = status.remaining_minutes;
        if !started {
            // The printer's clock is simulated, so the deadline is counted on that clock.
            let waited = begun.elapsed().mul_f64(scale);
            match watchdog.observe(&status, waited, &shared.profile) {
                StartVerdict::Waiting => continue,
                StartVerdict::Started { .. } => {
                    started = true;
                    detail.message = None;
                    if shared
                        .transition(&id, &["starting"], "printing", None, &detail)
                        .is_err()
                    {
                        return;
                    }
                }
                StartVerdict::NotStarted { message, .. } => {
                    detail.message = Some(message);
                    return end("not_started", &detail);
                }
                StartVerdict::Failed { problems } => {
                    detail.problems = problems.into_iter().map(Into::into).collect();
                    detail.message =
                        Some("the printer reported a failure instead of starting".into());
                    return end("failed", &detail);
                }
            }
        }
        let Ok(job) = shared.store.print_job(&id) else {
            return;
        };
        for event in tracker.observe(&status, &shared.profile) {
            match event {
                JobEvent::Phase { phase, headline } => shared.emit(
                    &job,
                    PrintEvent::Phase {
                        phase: phase_name(phase),
                        headline,
                    },
                ),
                JobEvent::Progress {
                    percent,
                    remaining_minutes,
                } => shared.emit(
                    &job,
                    PrintEvent::Progress {
                        percent,
                        remaining_minutes,
                    },
                ),
                JobEvent::Problem(explanation) => {
                    let problem = PrinterProblem::from(explanation);
                    detail.problems.push(problem.clone());
                    shared.emit(&job, PrintEvent::Problem { problem });
                }
                JobEvent::Ended(outcome) => {
                    let status = match outcome {
                        JobOutcome::Finished => "finished",
                        JobOutcome::Cancelled => "cancelled",
                        JobOutcome::Failed { problems } => {
                            detail.problems = problems.into_iter().map(Into::into).collect();
                            "failed"
                        }
                    };
                    detail.message =
                        Some(format!("the simulated print {status}; nothing was printed"));
                    return end(status, &detail);
                }
            }
        }
        // Keep the stored job current for callers that poll instead of listening.
        let _ = shared
            .store
            .update_print_job(&id, &["printing"], "printing", None, &json!(detail));
    }
}

#[cfg(test)]
mod review_order_tests {
    #![allow(clippy::expect_used)]
    use super::*;

    fn finding(code: &str) -> LintFinding {
        LintFinding {
            severity: LintSeverity::Warning,
            code: code.to_owned(),
            line: 1,
            message: format!("{code} message"),
            excerpt: String::new(),
        }
    }

    #[test]
    fn the_hotend_made_previews_notice_comes_before_many_lint_findings() {
        let summary: SliceSummary = serde_json::from_value(serde_json::json!({
            "status": "ok", "output": "output/plate_1.gcode.3mf", "sha256": "ab",
            "time_s": 60.0, "filament": [], "layers": 1,
            "warnings": [
                {"code": "print_settings_proven", "message": "proven"},
                {"code": "hotend_made_previews",
                 "message": "plate previews made by Hotend: the slicer drew no plate preview"}
            ],
            "slicer": {"name": "BambuStudio", "version": "02.09.01.70"}
        }))
        .expect("a slice summary");
        let lint: Vec<LintFinding> = (0..8).map(|i| finding(&format!("lint_{i}"))).collect();
        let warnings = review_warnings(
            hotend_made_previews(&summary),
            Some(finding("built_for_other")),
            lint.into_iter(),
            Some(finding("nozzle_size_assumed")),
        );
        assert_eq!(warnings.len(), 11);
        assert_eq!(warnings[0].code, "hotend_made_previews");
        assert!(
            warnings[0]
                .message
                .starts_with("plate previews made by Hotend")
        );
        assert_eq!(warnings[1].code, "built_for_other");
        assert_eq!(warnings[10].code, "nozzle_size_assumed");
        // A slice without Hotend-made previews has no such notice.
        let plain: SliceSummary = serde_json::from_value(serde_json::json!({
            "status": "ok", "output": "o", "sha256": "ab", "time_s": 1.0,
            "filament": [], "layers": 1, "warnings": [],
            "slicer": {"name": "BambuStudio", "version": "02.09.01.70"}
        }))
        .expect("a slice summary");
        assert!(hotend_made_previews(&plain).is_none());
    }
}
