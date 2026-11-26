//! Hotend's printer layer (DESIGN.md "Printer control", IMPLEMENTATION_BRIEF section 7).
//!
//! The path of a print job through this crate, each step producing the only value the next
//! step accepts:
//!
//! ```text
//! sliced .gcode.3mf ── check_job ──▶ CheckedJob ── Delivery::deliver ──▶ Delivered::OnPrinter(UploadedJob)
//!   (read once)                          │
//!                 ConfirmationRequest::start_print(job, printer)   (the dialog shows request.review_text())
//!                                        │  human click
//!                 ConfirmationAuthority::confirm(request, shown text) ──▶ HumanConfirmation
//!                                        │
//!                 StartToken::mint ──▶ StartToken
//!                                        │
//!                 start_print(printer, token, job, uploaded, sources, options) ──▶ StartWatchdog
//! ```
//!
//! | Module | What it guarantees |
//! | --- | --- |
//! | [`profile`] | Every printer fact is data in `profiles/bambu.toml` with a `verified` flag; nozzles are mapped by id only |
//! | [`archive`] | The `.gcode.3mf` is read as a hostile zip with one reading: entry count, size, ratio and path limits; entries tile the file and every header says what the directory says; nothing extracted |
//! | [`lint`] | G-code is delivered only if every line matches a row of the command table (command, section, parameter letters, a limit for every value); every move that lays plastic is within the reach of the nozzle that makes it and every other move within its travel box; nothing goes below the first layer; plastic is laid on the plate or on plastic, in amounts a nozzle lays down, through a nozzle commanded hot; every temperature is within the limits of the filament, the build plate and the machine; every heater is off when the file ends; and the file says it was sliced for the declared printer, build plate and nozzle size. Section markers choose rows and grant no trust |
//! | [`job`] | [`check_job`] is the single gate. The file is read once; the hash, the check, the text and every delivery are of those bytes. The archive holds the declared plate and nothing unread. What the file states (time, filament, materials, nozzles, MD5) is held to the commands and to the declared job, and [`JobSummary::review_text`] says which values were measured and which the file states |
//! | [`token`] | A print starts only with a single-use [`StartToken`] minted from a human confirmation of the review text, bound to the job (file hash and size, plate, declaration, review text, name on the printer), the printer's serial and an expiry |
//! | [`protocol`] | The Bambu LAN messages as typed structs; reports are parsed as hostile input; [`Command`] is a closed allowlist with no raw G-code |
//! | [`status`] | Reports merged into one [`PrinterStatus`], with AMS units, trays and the nozzle each unit feeds |
//! | [`printer`] | The [`Printer`] trait and [`start_print`], which starts only the file this job uploaded (its name on the printer carries the file's hash) and refuses a job that no longer matches what is loaded |
//! | [`delivery`] | Three delivery paths behind [`Delivery`], each sending the checked bytes and stating what it can and cannot do; the Bambu Connect link is a [`BambuConnectLink`] only a confirmed hand-off makes |
//! | [`handoff`] | The hand-off folder the person chose; under WSL a Windows drive folder, and the placed file's Windows path by a pure translation that cannot leave the folder |
//! | [`opener`] | [`LinkOpener`]: the one interface that opens the import link, a stand-in that opens nothing, and one real opener per platform that starts one program with one argument and no shell; none runs in a test build |
//! | [`watchdog`] | An accepted start is not a started print: [`StartWatchdog`] and [`JobTracker`] decide from reports |
//! | [`explain`] | Printer states and error codes in plain language, raw code kept |
//! | [`sim`] | [`SimulatedPrinter`], the printer used by the application and all tests |
//! | [`discovery`] | Parsing of discovery announcements; no socket |
//! | `lan` (feature `lan-status` or `lan`, both off by default) | `lan-status`: the read-only status listener (`StatusListener`), which connects to one private-range address, checks the certificate against the person's CA file or a pin, and has no code path that publishes; the application enables only this. `lan` adds the commanding transport: MQTT over TLS 1.2 and implicit FTPS with a pinned certificate, loopback only (`LanPrinter`), for this crate's tests |
//!
//! Nothing in a default build opens a network connection. See `README.md` in this crate for
//! every protocol assumption that still has to be confirmed on a real X2D.

pub mod archive;
pub mod delivery;
pub mod discovery;
pub mod explain;
pub mod handoff;
pub mod ident;
pub mod job;
#[cfg(feature = "lan-status")]
pub mod lan;
pub mod lint;
mod md5;
pub mod opener;
pub mod printer;
pub mod profile;
pub mod protocol;
pub mod secret;
pub mod sim;
pub mod status;
pub mod token;
pub mod watchdog;

pub use crate::archive::ArchiveLimits;
pub use crate::delivery::{
    BambuConnectHandOff, BambuConnectLink, Capabilities, Delivered, Delivery, DeliveryError,
    DeliveryKind, ExportToFolder, HandOff, PrinterDelivery, bambu_connect_entries,
    bambu_connect_missing, bambu_connect_missing_in,
};
pub use crate::explain::Explanation;
pub use crate::handoff::{HandOffFolder, HandOffPathError, HostKind, PlacedPath, PlacedProject};
pub use crate::ident::{PrinterIdentity, Serial};
pub use crate::job::{CheckedJob, JobError, JobSummary, SummaryFilament, check_job};
pub use crate::lint::{
    ConfigurationUse, DeclaredFilament, DeclaredJob, Finding, GcodeStats, LintReport, Severity,
};
pub use crate::opener::{LinkOpener, OpenError, StandInOpener, platform_opener};
pub use crate::printer::{Printer, PrinterError, StartError, UploadedJob, start_print};
pub use crate::profile::{ConfigKind, PrinterProfile, Profiles, Section};
pub use crate::protocol::command::{Command, FilamentSource, SpeedProfile, StartOptions};
pub use crate::protocol::hms::HmsCode;
pub use crate::secret::AccessCode;
pub use crate::sim::{SimConfig, SimFault, SimulatedPrinter, StartQuirk};
pub use crate::status::{AmsUnit, Phase, PrinterStatus, Tray};
pub use crate::token::{
    ConfirmationAuthority, ConfirmationRecord, ConfirmationRequest, HumanConfirmation,
    MAX_DESTINATION_BYTES, PROJECT_OUTSIDE_CHECK, StartToken, TokenError, is_hidden_format,
    is_plain_line,
};
pub use crate::watchdog::{
    JobEvent, JobOutcome, JobTracker, NotStartedReason, StartVerdict, StartWatchdog,
};
