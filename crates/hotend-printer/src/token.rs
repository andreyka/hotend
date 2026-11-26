//! The start token: the only way a print can start.
//!
//! The chain, each step consuming the one before it:
//!
//! 1. [`ConfirmationRequest::start_print`] is built from a [`CheckedJob`] and a printer. It
//!    holds the job's [`crate::JobSummary`] and the job's identity: the SHA-256 of the
//!    file, its size, the plate, the name the file gets on the printer, the declaration
//!    the file was held to, and the SHA-256 of the review text. The dialog shows
//!    [`ConfirmationRequest::review_text`]. Where each value of that text comes from
//!    (measured from the commands, a claim of the file that was held to a measurement,
//!    or a name from the printer profile) is said in [`crate::job`] and in the text.
//! 2. [`ConfirmationAuthority::confirm`] turns the request into a [`HumanConfirmation`].
//!    It takes the text the dialog displayed and refuses any text but the request's
//!    own. That cannot prove what was on the screen; it makes the dialog code say what
//!    it drew, and a dialog that draws something else cannot produce a confirmation.
//!    [`HumanConfirmation::record`] is the record of what was confirmed. The authority
//!    can be taken once per process ([`ConfirmationAuthority::take`]); the application
//!    takes it at start-up and gives it to the code that draws the native confirmation
//!    dialog and to nothing else. Whoever does not hold it cannot produce a
//!    confirmation, so neither the agent's tool gateway nor the webview can.
//! 3. [`StartToken::mint`] binds the confirmation to that identity, the printer's serial
//!    and an expiry.
//! 4. [`crate::printer::start_print`] takes the token by value and refuses a job with
//!    another identity than the confirmed one (another file, another plate, another
//!    declaration, another review text, another name on the printer), a different
//!    printer, an expired token, and a job whose file on disk was changed since.
//!
//! An editable Bambu Studio project is handed over on a confirmation of its own
//! ([`ConfirmationRequest::project`]): the request is built from the checked test slice
//! that Bambu Studio's command line made from the project, and its text and identity also
//! carry the SHA-256 of the project file, where it goes (the destination line) and the
//! sentence [`PROJECT_OUTSIDE_CHECK`]. Such a confirmation cannot mint a start token and
//! cannot release a Bambu Connect link; it is redeemed only by
//! [`HumanConfirmation::redeem_for_project`] for that project's hash and that destination.
//!
//! What is sent to a printer is the job's own copy of the bytes that were checked
//! ([`crate::job`]), under a name that carries their hash, so the token, the check, the
//! text and the upload are about the same bytes.
//!
//! Misuse is unrepresentable rather than checked: `HumanConfirmation` and `StartToken`
//! have private fields, no public constructor, no `Clone` and no `Deserialize`, and both
//! are consumed by value, so a token cannot be forged, copied, stored as text or used twice.
//!
//! ```compile_fail
//! // A token cannot be cloned.
//! fn twice(token: hotend_printer::StartToken) {
//!     let _copy = token.clone();
//! }
//! ```
//!
//! ```compile_fail
//! // A token is moved into the start; using it again does not compile.
//! use hotend_printer::*;
//! fn twice(printer: &mut dyn Printer, token: StartToken, job: &CheckedJob, up: &UploadedJob) {
//!     let _ = start_print(printer, token, job, up, &[], StartOptions::default());
//!     let _ = start_print(printer, token, job, up, &[], StartOptions::default());
//! }
//! ```
//!
//! ```compile_fail
//! // A token cannot be read back from text.
//! let _: hotend_printer::StartToken = serde_json::from_str("{}").unwrap();
//! ```
//!
//! ```compile_fail
//! // A confirmation cannot be made without the authority.
//! let _ = hotend_printer::HumanConfirmation { printer: None };
//! ```

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use crate::ident::{PrinterIdentity, Serial};
use crate::job::{CheckedJob, JobSummary, sha256_hex};
use crate::lint::Finding;

/// Why a token or a confirmation was refused.
#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    /// The confirmation was for something else than starting a print.
    #[error("this confirmation was not given for starting a print")]
    NotForAPrint,
    /// The confirmation was for a start, not for a hand-off.
    #[error("this confirmation was given for another action")]
    NotForThis,
    /// The token or confirmation is too old.
    #[error("the confirmation has expired; confirm the print again")]
    Expired,
    /// The token was minted for another printer.
    #[error("the confirmation was given for another printer; confirm the print again")]
    WrongPrinter,
    /// The token was minted for another sliced file.
    #[error("the confirmation was given for another sliced file; confirm the print again")]
    WrongFile,
    /// The text the dialog displayed is not the text of the request.
    #[error(
        "the confirmation dialog did not show the text of this print job, so nothing was confirmed"
    )]
    NotWhatWasShown,
    /// The token was minted for another job of the same file: another plate, another
    /// declaration, another name on the printer or another review text.
    #[error("the confirmation was given for another job of this file; check and confirm it again")]
    WrongJob,
    /// The file on disk is no longer the file that was confirmed.
    #[error("the sliced file changed after it was confirmed; check and confirm it again")]
    FileChanged,
    /// The file could not be read for the final check.
    #[error("the sliced file could not be read for the final check: {0}")]
    Unreadable(String),
}

/// What every text about an editable Bambu Studio project says, in these words: a project
/// is sliced again by Bambu Studio when it is opened, so Hotend's check does not cover what
/// Bambu Studio then makes of it.
pub const PROJECT_OUTSIDE_CHECK: &str =
    "A project opened this way is sliced again by Bambu Studio, so it is outside Hotend's check.";

/// The text a person confirms before an editable project is handed over: the project's
/// hash, where it goes (`destination`), [`PROJECT_OUTSIDE_CHECK`], and the review text of
/// the test slice, introduced as what it is.
fn project_review_text(summary: &JobSummary, project_sha256: &str, destination: &str) -> String {
    [
        format!("Editable Bambu Studio project. SHA-256 of the project file: {project_sha256}."),
        format!("Where it goes: {destination}"),
        PROJECT_OUTSIDE_CHECK.to_owned(),
        "Before handing it over, Hotend sliced this project once itself with Bambu Studio's \
         command line, as a test. The lines below are about that test file only; the slice \
         Bambu Studio makes when the project is opened can differ from it."
            .to_owned(),
        summary.review_text(),
    ]
    .join("\n")
}

/// A project file's SHA-256 as an identity keeps it: 64 lower-case hex digits, or `None`.
fn project_hash(text: &str) -> Option<String> {
    (text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| text.to_ascii_lowercase())
}

/// The longest text, in bytes, that names where a project goes in its confirmation.
pub const MAX_DESTINATION_BYTES: usize = 1000;

/// A project's destination as an identity keeps it: one plain line of at most
/// [`MAX_DESTINATION_BYTES`] ([`is_plain_line`]), not blank, or `None`.
fn project_destination(text: &str) -> Option<String> {
    (!text.trim().is_empty() && text.len() <= MAX_DESTINATION_BYTES && is_plain_line(text))
        .then(|| text.to_owned())
}

/// Whether `text` shows as what it says on one line: no control character, no white space
/// but the plain space (no tab, no line break, no U+2028 or U+2029, no other kind of space),
/// and no character that changes how the text around it is laid out without showing
/// itself ([`is_hidden_format`]). Text that names a folder in a confirmation must be such
/// a line, so that what the person reads is what the confirmation is bound to.
pub fn is_plain_line(text: &str) -> bool {
    text.chars()
        .all(|c| c == ' ' || !(c.is_control() || c.is_whitespace() || is_hidden_format(c)))
}

/// Whether a character is a format character or otherwise invisible: zero-width characters,
/// the bidirectional marks, embeddings, overrides and isolates, the word joiner and the
/// invisible operators, the byte order mark, variation selectors, tags, fillers, the
/// interlinear annotation characters and the other characters of Unicode's general category
/// Cf (format). The same classes as the desktop's confirmation dialog drops, and Cf beside.
pub fn is_hidden_format(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061C}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08E2}'
            | '\u{115F}'
            | '\u{1160}'
            | '\u{180B}'..='\u{180F}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{2800}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{110BD}'
            | '\u{110CD}'
            | '\u{13430}'..='\u{1343F}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0000}'..='\u{E0FFF}'
    )
}

/// The final look at the job's file on disk.
fn still_on_disk(job: &CheckedJob) -> Result<(), TokenError> {
    if let Err(error) = std::fs::symlink_metadata(job.path()) {
        return Err(TokenError::Unreadable(error.kind().to_string()));
    }
    job.verify_on_disk().map_err(|_| TokenError::FileChanged)
}

/// What a confirmation is about: one checked job, by everything that tells it from
/// another. Two jobs with the same identity send the same bytes under the same name for
/// the same plate and showed the same text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JobIdentity {
    sha256: String,
    size_bytes: u64,
    plate: u32,
    remote_name: String,
    /// SHA-256 of the declaration the file was held to.
    declaration: String,
    /// SHA-256 of the review text.
    review: String,
    /// For an editable project: the SHA-256 of the project file. `None` for every other
    /// request.
    project: Option<String>,
    /// For an editable project: where it goes, as the review text says it. `None` for
    /// every other request.
    destination: Option<String>,
}

impl JobIdentity {
    pub(crate) fn of(job: &CheckedJob) -> Self {
        Self {
            sha256: job.sha256().to_owned(),
            size_bytes: job.summary().size_bytes,
            plate: job.summary().plate,
            remote_name: job.remote_name(),
            declaration: sha256_hex(job.declaration_text().as_bytes()),
            review: sha256_hex(job.summary().review_text().as_bytes()),
            project: None,
            destination: None,
        }
    }

    /// The identity of handing over the project `project_sha256` (a hash as
    /// [`project_hash`] keeps it) to `destination` (as [`project_destination`] keeps it),
    /// made after the test slice `job`.
    fn of_project(job: &CheckedJob, project_sha256: &str, destination: &str) -> Self {
        Self {
            review: sha256_hex(
                project_review_text(job.summary(), project_sha256, destination).as_bytes(),
            ),
            project: Some(project_sha256.to_owned()),
            destination: Some(destination.to_owned()),
            ..Self::of(job)
        }
    }

    /// `Ok` when `job` is the job this identity was taken from.
    fn matches(&self, job: &CheckedJob) -> Result<(), TokenError> {
        let now = Self::of(job);
        if self.sha256 != now.sha256 || self.size_bytes != now.size_bytes {
            return Err(TokenError::WrongFile);
        }
        if *self != now {
            return Err(TokenError::WrongJob);
        }
        Ok(())
    }
}

static AUTHORITY_TAKEN: AtomicBool = AtomicBool::new(false);

/// The capability to say "a human confirmed this". One per process.
#[derive(Debug)]
pub struct ConfirmationAuthority {
    _private: (),
}

impl ConfirmationAuthority {
    /// Takes the process's only authority. `None` if it was taken before.
    ///
    /// Call this once at start-up and hand the value to the confirmation dialog. Never
    /// expose it to code that handles requests from the agent or the webview.
    pub fn take() -> Option<Self> {
        AUTHORITY_TAKEN
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| Self { _private: () })
    }

    /// Records that a human confirmed `request` after reading `shown`, the text the
    /// dialog displayed. `shown` must be [`ConfirmationRequest::review_text`], character
    /// for character: a dialog that displayed anything else did not ask this question,
    /// and gets [`TokenError::NotWhatWasShown`]. Keep
    /// [`HumanConfirmation::record`] in the application's log: it says what was
    /// confirmed.
    pub fn confirm(
        &self,
        request: ConfirmationRequest,
        shown: &str,
    ) -> Result<HumanConfirmation, TokenError> {
        if shown != request.review_text() {
            return Err(TokenError::NotWhatWasShown);
        }
        Ok(HumanConfirmation {
            job: request.job,
            printer: request.printer.map(|printer| printer.serial),
            given: Instant::now(),
            at: SystemTime::now(),
        })
    }
}

/// What one confirmation was given for, for the application's record of it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConfirmationRecord {
    /// SHA-256 of the sliced file.
    pub sha256: String,
    /// SHA-256 of the review text the person read.
    pub review_sha256: String,
    /// The plate.
    pub plate: u32,
    /// The name the file has on the printer or the drive.
    pub remote_name: String,
    /// The printer the start was confirmed for; `None` for a hand-off.
    pub printer: Option<Serial>,
    /// When the confirmation was given.
    pub confirmed_at: SystemTime,
}

/// What the human is asked to confirm. Built only from a checked job.
#[derive(Debug, Clone)]
pub struct ConfirmationRequest {
    job: JobIdentity,
    summary: JobSummary,
    warnings: Vec<Finding>,
    printer: Option<PrinterIdentity>,
}

impl ConfirmationRequest {
    /// A request to start `job` on `printer`.
    pub fn start_print(job: &CheckedJob, printer: &PrinterIdentity) -> Self {
        Self {
            job: JobIdentity::of(job),
            summary: job.summary().clone(),
            warnings: job.lint().findings.clone(),
            printer: Some(printer.clone()),
        }
    }

    /// A request to hand `job` to Bambu Connect.
    pub fn hand_off(job: &CheckedJob) -> Self {
        Self {
            job: JobIdentity::of(job),
            summary: job.summary().clone(),
            warnings: job.lint().findings.clone(),
            printer: None,
        }
    }

    /// A request to hand over the editable Bambu Studio project whose file has the SHA-256
    /// `project_sha256` (64 hex digits) to `destination`, where `job` is the checked test
    /// slice that Bambu Studio's command line made from that very project file.
    /// `destination` says, in one line, where the project goes (the folder it is placed in
    /// and whether anything opens it). Its [`Self::review_text`] names the project's hash
    /// and the destination, says [`PROJECT_OUTSIDE_CHECK`] and then gives the test slice's
    /// review text; the confirmation is bound to that text, to the test slice's identity,
    /// to the project's hash and to the destination. `None` when `project_sha256` is not a
    /// SHA-256, or `destination` is blank, longer than 1000 bytes or not one plain line
    /// ([`is_plain_line`]: a control character, a tab, a line or paragraph separator, any
    /// white space but the plain space, or an invisible or bidirectional format character
    /// is refused).
    pub fn project(job: &CheckedJob, project_sha256: &str, destination: &str) -> Option<Self> {
        let project = project_hash(project_sha256)?;
        let destination = project_destination(destination)?;
        Some(Self {
            job: JobIdentity::of_project(job, &project, &destination),
            summary: job.summary().clone(),
            warnings: job.lint().findings.clone(),
            printer: None,
        })
    }

    /// The job's summary, for a dialog that lays the values out itself. Where each value
    /// comes from is documented on [`JobSummary`].
    pub fn summary(&self) -> &JobSummary {
        &self.summary
    }

    /// The text the dialog must show: time, layers, filament, nozzle, temperatures,
    /// build plate, the printed area and every accepted command that changes limits or
    /// stored configuration. The confirmation is bound to the hash of this text.
    pub fn review_text(&self) -> String {
        match (&self.job.project, &self.job.destination) {
            (Some(project), Some(destination)) => {
                project_review_text(&self.summary, project, destination)
            }
            _ => self.summary.review_text(),
        }
    }

    /// The SHA-256 of the project file of a project request; `None` for any other.
    pub fn project_sha256(&self) -> Option<&str> {
        self.job.project.as_deref()
    }

    /// SHA-256 of [`Self::review_text`], lower-case hex, for the application's record
    /// of what was confirmed.
    pub fn review_sha256(&self) -> &str {
        &self.job.review
    }

    /// The linter's warnings for the job, to show beside the summary.
    pub fn warnings(&self) -> &[Finding] {
        &self.warnings
    }

    /// The printer the print would start on; `None` for a hand-off.
    pub fn printer(&self) -> Option<&PrinterIdentity> {
        self.printer.as_ref()
    }
}

/// Proof that a human confirmed one request. Opaque; consumed by its use.
#[derive(Debug)]
pub struct HumanConfirmation {
    job: JobIdentity,
    printer: Option<Serial>,
    given: Instant,
    at: SystemTime,
}

impl HumanConfirmation {
    /// How long a confirmation stays usable.
    pub const MAX_AGE: Duration = Duration::from_secs(120);

    /// What was confirmed, for the application's record.
    pub fn record(&self) -> ConfirmationRecord {
        ConfirmationRecord {
            sha256: self.job.sha256.clone(),
            review_sha256: self.job.review.clone(),
            plate: self.job.plate,
            remote_name: self.job.remote_name.clone(),
            printer: self.printer.clone(),
            confirmed_at: self.at,
        }
    }

    /// Uses the confirmation for a hand-off of `job`.
    pub(crate) fn redeem_for_hand_off(self, job: &CheckedJob) -> Result<(), TokenError> {
        if self.printer.is_some() || self.job.project.is_some() {
            return Err(TokenError::NotForThis);
        }
        if self.given.elapsed() > Self::MAX_AGE {
            return Err(TokenError::Expired);
        }
        self.job.matches(job)?;
        still_on_disk(job)
    }

    /// Uses the confirmation for handing over the editable project whose file has the
    /// SHA-256 `project_sha256` to `destination`, made after the test slice `job`. Refused
    /// for any other confirmation (a start, a Bambu Connect hand-off or an export), for
    /// another project, another destination or another test slice, after
    /// [`Self::MAX_AGE`], and when the test slice's file changed on disk. The caller then
    /// writes the project's bytes, which it must hold to this hash, to that destination.
    pub fn redeem_for_project(
        self,
        job: &CheckedJob,
        project_sha256: &str,
        destination: &str,
    ) -> Result<(), TokenError> {
        if self.printer.is_some() || self.job.project.is_none() {
            return Err(TokenError::NotForThis);
        }
        if self.given.elapsed() > Self::MAX_AGE {
            return Err(TokenError::Expired);
        }
        let project = project_hash(project_sha256).ok_or(TokenError::WrongFile)?;
        if self.job.project.as_deref() != Some(project.as_str()) {
            return Err(TokenError::WrongFile);
        }
        if self.job.destination.as_deref() != Some(destination) {
            return Err(TokenError::WrongJob);
        }
        let now = JobIdentity::of_project(job, &project, destination);
        if self.job.sha256 != now.sha256 || self.job.size_bytes != now.size_bytes {
            return Err(TokenError::WrongFile);
        }
        if self.job != now {
            return Err(TokenError::WrongJob);
        }
        still_on_disk(job)
    }
}

/// The one-time permission to start one sliced file on one printer.
#[derive(Debug)]
pub struct StartToken {
    job: JobIdentity,
    printer: Serial,
    expires: Instant,
}

impl StartToken {
    /// The longest a token can live.
    pub const MAX_TTL: Duration = Duration::from_secs(300);
    /// A sensible lifetime: long enough for an upload, short enough to stay tied to the click.
    pub const DEFAULT_TTL: Duration = Duration::from_secs(120);

    /// Mints the token for a confirmed print. `ttl` is cut to [`Self::MAX_TTL`].
    pub fn mint(confirmation: HumanConfirmation, ttl: Duration) -> Result<Self, TokenError> {
        let Some(printer) = confirmation.printer else {
            return Err(TokenError::NotForAPrint);
        };
        if confirmation.given.elapsed() > HumanConfirmation::MAX_AGE {
            return Err(TokenError::Expired);
        }
        Ok(Self {
            job: confirmation.job,
            printer,
            expires: Instant::now() + ttl.min(Self::MAX_TTL),
        })
    }

    /// The serial of the printer the token is for.
    pub fn printer(&self) -> &Serial {
        &self.printer
    }

    /// The hash of the file the token is for.
    pub fn sha256(&self) -> &str {
        &self.job.sha256
    }

    /// Time left before the token expires.
    pub fn expires_in(&self) -> Duration {
        self.expires.saturating_duration_since(Instant::now())
    }

    /// Spends the token on starting `job` on `printer`. The job must be the confirmed
    /// one in every part of its identity, and its file on disk must still be the file
    /// that was checked.
    pub(crate) fn redeem(self, job: &CheckedJob, printer: &Serial) -> Result<Redeemed, TokenError> {
        if Instant::now() >= self.expires {
            return Err(TokenError::Expired);
        }
        if &self.printer != printer {
            return Err(TokenError::WrongPrinter);
        }
        self.job.matches(job)?;
        still_on_disk(job)?;
        Ok(Redeemed { _private: () })
    }
}

/// A spent token: what [`crate::printer::start_print`] needs to build the start command.
#[derive(Debug)]
pub(crate) struct Redeemed {
    _private: (),
}
