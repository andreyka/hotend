//! The printer interface and the one function that can start a print.
//!
//! [`Printer`] is what the application talks to: the simulated printer
//! ([`crate::sim::SimulatedPrinter`]) by default, the LAN transport behind the `lan`
//! feature. It can report status, take an upload of a checked job, and take a command from
//! the closed [`Command`] list.
//!
//! Guarantees of [`start_print`], the only place a
//! [`Command::StartPreparedJob`] is created:
//! - it consumes a [`StartToken`], which exists only after a human confirmation, and
//!   refuses a token for another job (another file, plate, declaration, review text or
//!   name on the printer) or another printer, an expired token, and a job whose file on
//!   disk was changed since it was checked;
//! - the job must have been uploaded to this printer, by this job: the upload sends the
//!   job's own copy of the checked bytes under a name that carries their hash, and the
//!   start command names exactly that file. Two different files never share a name, so
//!   an upload of another job cannot take the confirmed file's place. What the printer
//!   does with its storage afterwards is the printer's: the start command carries no
//!   checksum the printer is known to verify (see the README's checklist);
//! - the job must have been checked for this printer model;
//! - the printer must not be busy, and what is loaded must agree with what was sliced:
//!   every filament the job uses must come from a loaded tray of the declared material on
//!   an AMS or external spool holder that feeds the nozzle the job was sliced for. A job
//!   whose filament map no longer matches the printer is refused, not sent;
//! - on success it returns a [`StartWatchdog`], because an accepted start is not a started
//!   print.

use crate::delivery::Capabilities;
use crate::explain::Explanation;
use crate::ident::{PrinterIdentity, Serial};
use crate::job::CheckedJob;
use crate::profile::PrinterProfile;
use crate::protocol::command::{Command, FilamentSource, StartCommand, StartOptions};
use crate::status::{Phase, PrinterStatus, Tray};
use crate::token::{StartToken, TokenError};
use crate::watchdog::StartWatchdog;

/// Why a printer operation failed.
#[derive(Debug, thiserror::Error)]
pub enum PrinterError {
    /// The printer cannot be reached.
    #[error(
        "the printer cannot be reached: {0}. Check that it is switched on and on the same network"
    )]
    Unreachable(String),
    /// The printer refused the request.
    #[error("the printer refused: {0}")]
    Refused(String),
    /// This path cannot do what was asked.
    #[error("not supported: {0}")]
    NotSupported(String),
    /// The upload failed or did not arrive intact.
    #[error("the upload failed: {0}")]
    Upload(String),
    /// The job cannot be used.
    #[error(transparent)]
    Job(#[from] crate::job::JobError),
}

/// A checked job as it sits on one printer's storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadedJob {
    remote_name: String,
    sha256: String,
    size_bytes: u64,
    printer: Serial,
}

impl UploadedJob {
    /// Records that `job` was uploaded to `printer` under the job's name on a printer
    /// ([`CheckedJob::remote_name`], which carries the file's hash). Only the
    /// printers of this crate can say so, after they verified the bytes they stored.
    pub(crate) fn new(job: &CheckedJob, printer: &PrinterIdentity) -> Self {
        Self {
            remote_name: job.remote_name(),
            sha256: job.sha256().to_owned(),
            size_bytes: job.summary().size_bytes,
            printer: printer.serial.clone(),
        }
    }

    /// The file's name on the printer's storage.
    pub fn remote_name(&self) -> &str {
        &self.remote_name
    }

    /// SHA-256 of the uploaded file.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Size of the uploaded file.
    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    /// The printer it was uploaded to.
    pub fn printer(&self) -> &Serial {
        &self.printer
    }
}

/// A printer Hotend can talk to.
///
/// The trait is implemented in this crate only ([`crate::sim::SimulatedPrinter`] and the
/// LAN printer): an [`UploadedJob`] cannot be created elsewhere.
pub trait Printer: Send {
    /// Which printer this is.
    fn identity(&self) -> &PrinterIdentity;

    /// The facts of the printer's model.
    fn profile(&self) -> &PrinterProfile;

    /// What delivering a job to this printer can and cannot do, in sentences that are true
    /// of this printer: a simulated printer says that it is one.
    fn capabilities(&self) -> Capabilities;

    /// The latest state the printer reported.
    fn status(&mut self) -> Result<PrinterStatus, PrinterError>;

    /// Copies a checked job to the printer's storage under [`CheckedJob::remote_name`].
    /// The bytes sent are the job's own copy of what was checked; the implementation
    /// verifies that what it sent has the job's hash.
    fn upload(&mut self, job: &CheckedJob) -> Result<UploadedJob, PrinterError>;

    /// Sends one allowlisted command. A start command can only come from [`start_print`].
    fn send(&mut self, command: Command) -> Result<(), PrinterError>;
}

/// Why a print was not started.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// The token does not permit this start.
    #[error(transparent)]
    Token(#[from] TokenError),
    /// The job is not on this printer.
    #[error("the job has not been uploaded to this printer; upload it first")]
    NotUploaded,
    /// The job was checked for another printer model.
    #[error(
        "the job was checked for the printer model `{checked_for}`, not for this printer (`{printer}`); slice and check it for this printer"
    )]
    WrongModel {
        /// The model the job was checked for.
        checked_for: String,
        /// This printer's model.
        printer: String,
    },
    /// The printer is doing something else.
    #[error("the printer is busy ({0:?}); wait until it is idle")]
    Busy(Phase),
    /// What is loaded does not match what was sliced.
    #[error("the job no longer matches the printer: {0}. Load the right filament or slice again")]
    Mismatch(String),
    /// Talking to the printer failed.
    #[error(transparent)]
    Printer(#[from] PrinterError),
}

fn same_family(profile: &PrinterProfile, a: &str, b: &str) -> bool {
    match (profile.filament_limits(a), profile.filament_limits(b)) {
        (Some(a), Some(b)) => a.family == b.family,
        _ => false,
    }
}

fn check_tray(
    profile: &PrinterProfile,
    slot: u32,
    wanted: &str,
    tray: Option<&Tray>,
    place: &str,
) -> Result<(), StartError> {
    let Some(tray) = tray.filter(|tray| tray.is_loaded()) else {
        return Err(StartError::Mismatch(format!(
            "filament {slot} should come from {place}, which is empty"
        )));
    };
    let loaded = tray.material.as_deref().unwrap_or_default();
    if !same_family(profile, wanted, loaded) {
        return Err(StartError::Mismatch(format!(
            "filament {slot} was sliced as {wanted}, but {place} holds {loaded}"
        )));
    }
    Ok(())
}

/// Checks that `sources` supply what `job` was sliced for, given what the printer reports.
fn check_sources(
    job: &CheckedJob,
    sources: &[FilamentSource],
    status: &PrinterStatus,
    profile: &PrinterProfile,
) -> Result<(), StartError> {
    let declared = &job.declared().filaments;
    if sources.len() != declared.len() {
        return Err(StartError::Mismatch(format!(
            "the job has {} filament(s) but {} source(s) were given",
            declared.len(),
            sources.len()
        )));
    }
    let rules = &profile.family().ams;
    for (filament, source) in declared.iter().zip(sources) {
        let slot = filament.slot;
        match *source {
            FilamentSource::Unused => {
                if job.lint().stats.slots_used.contains(&slot) || declared.len() == 1 {
                    return Err(StartError::Mismatch(format!(
                        "the job prints with filament {slot}, but no spool is assigned to it"
                    )));
                }
            }
            FilamentSource::External(id) => {
                if !rules.external_ids.contains(&id) {
                    return Err(StartError::Mismatch(format!(
                        "{id} is not an external spool holder of this printer"
                    )));
                }
                if profile.nozzles().len() > 1 {
                    let feeds = profile
                        .nozzles()
                        .iter()
                        .find(|nozzle| nozzle.external_tray.is_some_and(|tray| tray.id == id))
                        .map(|nozzle| nozzle.device_extruder);
                    if feeds != Some(filament.nozzle) {
                        return Err(StartError::Mismatch(match feeds {
                            Some(feeds) => format!(
                                "filament {slot} was sliced for nozzle id {}, but external spool holder {id} feeds nozzle id {feeds}",
                                filament.nozzle
                            ),
                            None => format!(
                                "it is not known which nozzle external spool holder {id} feeds, so filament {slot} cannot be routed safely"
                            ),
                        }));
                    }
                }
                // The holder has no presence sensor; what it reports is all there is.
                let tray = status.external.iter().find(|tray| tray.id == id);
                check_tray(
                    profile,
                    slot,
                    &filament.material,
                    tray,
                    "the external spool holder",
                )?;
            }
            FilamentSource::Ams { unit, tray } => {
                let place = format!("AMS {unit} tray {tray}");
                let Some(ams) = status.ams_unit(unit) else {
                    return Err(StartError::Mismatch(format!(
                        "filament {slot} should come from {place}, but the printer reports no such AMS"
                    )));
                };
                if profile.nozzles().len() > 1 && ams.nozzle != Some(filament.nozzle) {
                    return Err(StartError::Mismatch(match ams.nozzle {
                        Some(feeds) => format!(
                            "filament {slot} was sliced for nozzle id {}, but AMS {unit} feeds nozzle id {feeds}",
                            filament.nozzle
                        ),
                        None => format!(
                            "the printer does not say which nozzle AMS {unit} feeds, so filament {slot} cannot be routed safely"
                        ),
                    }));
                }
                check_tray(profile, slot, &filament.material, ams.tray(tray), &place)?;
            }
        }
    }
    Ok(())
}

/// Starts an uploaded job. See the module documentation for what is checked.
///
/// `sources` says, for each filament of the job in slot order, which loaded spool supplies
/// it. The token is consumed whether or not the start succeeds.
pub fn start_print(
    printer: &mut dyn Printer,
    token: StartToken,
    job: &CheckedJob,
    uploaded: &UploadedJob,
    sources: &[FilamentSource],
    options: StartOptions,
) -> Result<StartWatchdog, StartError> {
    let identity = printer.identity().clone();
    let _spent = token.redeem(job, &identity.serial)?;
    if uploaded.sha256() != job.sha256()
        || uploaded.size_bytes() != job.summary().size_bytes
        || uploaded.remote_name() != job.remote_name()
        || uploaded.printer() != &identity.serial
    {
        return Err(StartError::NotUploaded);
    }
    let profile = printer.profile().clone();
    if job.printer_model() != profile.id() {
        return Err(StartError::WrongModel {
            checked_for: job.printer_model().to_owned(),
            printer: profile.id().to_owned(),
        });
    }
    let status = printer.status()?;
    let phase = status.phase();
    if !matches!(phase, Phase::Idle | Phase::Finished | Phase::Failed) {
        return Err(StartError::Busy(phase));
    }
    check_sources(job, sources, &status, &profile)?;
    let command = StartCommand::new(
        job.summary().plate,
        uploaded.remote_name().to_owned(),
        job.name().to_owned(),
        sources.to_vec(),
        options,
    );
    printer.send(Command::StartPreparedJob(command))?;
    Ok(StartWatchdog::new(job.name(), &status))
}

/// What a non-expert should read about the printer's state right now.
pub fn describe(printer: &mut dyn Printer) -> Result<Vec<Explanation>, PrinterError> {
    let status = printer.status()?;
    Ok(crate::explain::problems(&status, printer.profile()))
}
