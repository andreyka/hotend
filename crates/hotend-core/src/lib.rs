//! Hotend's core library: everything between the UI (or the CLI) and the sandboxed child
//! processes.
//!
//! Start with [`Core`]: it has one async method per UI command of `docs/PROTOCOL.md`
//! section 8. Underneath it:
//!
//! | Module | What it guarantees |
//! | --- | --- |
//! | [`store`] | SQLite store with migrations; versions are immutable; nothing secret is stored |
//! | [`snapshot`] | A snapshot names one version; it changes no version or file, and its export carries the exact bytes of the version's files and of its print |
//! | [`jobs`] | The engine runs only inside the sandbox, with a wall-clock limit and bounded logs; its result is parsed as hostile input |
//! | [`import`] | A file reported by a child is used only if it is a plain file inside the job folder, within its size cap |
//! | [`engine`] | The engine's request and result as strict Rust types |
//! | [`modeling`] | A version exists only for a build that finished; artefacts are stored under names the core chooses |
//! | [`gateway`] | Every agent tool call is schema-validated, policy-checked, audited and executed here; there is no print-start tool |
//! | [`agent`] | The agent child is sandboxed, speaks JSON-RPC over stdio and is stopped when its run ends |
//! | [`egress`] | Model requests leave only for the chosen provider's API, with the key attached here and caps per run |
//! | [`secrets`] | Provider keys are write-only from outside this crate |
//! | [`sweep`] | Job and staging folders left by a core that was killed are removed at the next start, never next to a core that is still running |
//! | [`profiles`] | Printer facts from one source: the printer layer's profile for its printers, the core's file for the others, never both |
//! | [`printer_lan`] | A real printer's status is read over the local network only: from the one private-range address the person set, through a listener that cannot publish, with the certificate checked against the person's CA file or a pin they confirmed, and the access code never shown anywhere |
//! | [`printing`] | A job leaves `pending` only on a person's confirmation, after the G-code linter passed its file; the agent's tools cannot reach it; the only printer is simulated |
//! | [`research`] | A run searches the web only with the person's agreement and inside the project's allowance; what a page says is stored and shown as data, and a number from the web stays marked until the person confirms it |
//! | [`api`] | Facade errors as tagged data with a message and a next step |
//! | [`doctor`] | What this machine has and lacks, as data; the self-tests run the real engine and agent in the sandbox |
//!
//! The core is the only component that holds secrets or opens network connections.

pub mod agent;
pub mod api;
pub mod core;
pub mod doctor;
pub mod egress;
pub mod engine;
pub mod gateway;
pub mod import;
pub mod jobs;
pub mod modeling;
pub mod paths;
pub mod printer_lan;
pub mod printing;
pub mod profiles;
pub mod research;
#[cfg(test)]
mod research_flow_tests;
pub mod runtime;
pub mod search;
pub mod secrets;
pub mod settings;
pub mod snapshot;
pub mod store;
pub mod sweep;
#[cfg(test)]
mod testutil;
pub mod util;

/// The character classes a confirmation refuses or drops, for the desktop's dialog.
pub use hotend_printer::is_hidden_format;
pub use hotend_printer::{ConfirmationAuthority, PROJECT_OUTSIDE_CHECK};
/// The Bambu Connect hand-off: the opener interface, its stand-in, and the host kind.
pub use hotend_printer::{HostKind, LinkOpener, OpenError, StandInOpener, platform_opener};

pub use crate::agent::{AgentCommand, AgentError, AgentLimits, RunEvent, StopReason};
pub use crate::api::ApiError;
pub use crate::core::{
    Core, CoreConfig, CoreError, MessageRun, ProjectDetail, SecretBackend, WeakCore,
};
pub use crate::doctor::{DoctorCheck, DoctorOptions, DoctorReport, DoctorStatus, KeyState};
pub use crate::egress::{Effort, EgressLimits, ProviderKind};
pub use crate::engine::{
    CalibrationChange, CardRef, EngineResult, EngineStatus, ExportFormat, Gauge, GaugeFeature,
    GaugeReading, Orientation, Plate, Question, Unplaced,
};
pub use crate::gateway::{
    ContentItem, GatewayPolicy, HOTEND_MADE_PREVIEWS, PlacedPiece, PlateRenders,
    ProjectExportSummary, ProjectKeys, SliceBackend, SliceBed, SliceError, SliceOutcome,
    SliceRequest, SliceSummary, SlicerCatalogue, SlicerInstallation, ToolOutcome, ToolSpec,
};
pub use crate::jobs::JobConfig;
pub use crate::modeling::{
    BaselineFields, BaselineRemoval, BuildReport, Calibration, CalibrationUpdate, ExportedFile,
    GaugeExport, GaugeFile, InvalidBaseline, PlanBaseline, PlanChecklistItem, PlanDecision,
    PlanNotice, PlatePlanSummary, Preparation, PrepareOptions, PrintPlanView, ProvenBaseline,
    RenderedView, VersionDetail,
};
pub use crate::printer_lan::{
    CertificatePin, PrinterCertificate, PrinterLanAddress, PrinterLanSetup,
};
pub use crate::printing::{
    AmsInfo, CoreEvent, DeliveryChoice, DeliveryInfo, EditableProject, HandOffStatus,
    HandOffTarget, LanPrinterStatus, LintFinding, LintSeverity, NozzleReading, PrintError,
    PrintEvent, PrintFilament, PrintJob, PrintJobDetail, PrintReview, PrintSummary, PrinterInfo,
    PrinterOptions, PrinterProblem, PrinterRef, PrinterState, ProjectDestination, ProjectReview,
    SpoolChoice, TemperatureReading, TrayInfo,
};
pub use crate::research::{
    ProjectResearch, ResearchState, ResearchStatus, SearchSourceOption, WebNumber,
};
pub use crate::runtime::PythonRuntime;
pub use crate::search::{SearchService, SearchSource};
pub use crate::secrets::{
    FileSecretStore, MemorySecretStore, Secret, SecretStore, SecretStoreKind,
};
pub use crate::settings::SpoolSettings;
pub use crate::settings::{PreviewsMode, ProviderSettings, ResearchSettings, Settings};
pub use crate::snapshot::{MissingPrinted, PrintedFile, SnapshotExport, SnapshotFile};
pub use crate::store::{
    AuditEntry, PendingPrintJob, Project, ResearchLogEntry, ResearchNote, ResearchSource,
    SliceRecord, Snapshot, TranscriptEntry, Version, VersionStatus,
};
