//! The print confirmation: the one place in the desktop application where a print job can
//! leave `pending`, and where an editable Bambu Studio project can be handed over.
//!
//! Guarantees:
//!
//! - The only way in is [`confirm_print`]. It reviews the job itself (the core reads and
//!   checks the sliced file again), builds the dialog's text from that review, from what the
//!   core stored about the slice, and from nothing the window sent except the job id and
//!   the delivery, asks through a [`ConfirmationPrompt`], and calls the core's
//!   `print_confirm` only for the answer [`Answer::Confirm`].
//! - An editable project is handed over the same way, through [`confirm_project`] only: it
//!   reviews the project itself (`print_project_review`: the core checks the plan, the
//!   slice and the project file again), shows the dialog built from that review
//!   ([`project_confirmation_text`]) with the project's review text below it in one piece,
//!   and calls the core's `print_project_deliver` only for [`Answer::Confirm`], with the
//!   very text the prompt was given. The same dialog, lock and authority serve both.
//! - The dialog and the window's review (`PrintPanel.tsx`) show the same job, and a person
//!   compares them. Every number both show is written by the same rule here as there
//!   (`duration`, `one_decimal`, `degrees`), and the dialog uses the window's words.
//! - Text that comes from outside (a project's name, what the sliced file says of itself,
//!   a setting, a folder, a warning of the file check) is put on one line each: no such
//!   text can start a line of its own, hide or reorder the text next to it, or be read as
//!   anything but text ([`clip`]). Every line that carries such text starts with a label
//!   of the dialog's own ([`LABELS`], "Project: ...", "Warning: ..."). The dialog draws the
//!   label in a column of its own and the rest beside it, where a long value wraps within
//!   its own cell ([`rows`], `crate::dialog`): outside text never reaches the label column,
//!   so it cannot pass for a line of the dialog's, and nothing is shortened on screen. The
//!   number of lines and the length of each are bounded (`FILAMENT_LINES`, `WARNING_LINES`
//!   and the `_CHARS` limits), because the dialog's height is.
//! - Below its own lines the dialog shows the review text of the checked file
//!   (`PrintReview::review_text`, written by the printer layer from the file alone) as it
//!   is, in one piece, under a heading of the dialog's own ([`with_checked_text`]). Each of
//!   its lines starts with the printer layer's own words, and the little text from the file
//!   in it (a cleaned file name, a printer name of at most 60 plain characters) stands
//!   inside those sentences. What goes to `print_confirm` is the body the prompt was given,
//!   the very text that was drawn: the core finds the review text in it word for word or
//!   confirms nothing, and the printer layer binds the start to that text.
//! - The process's `ConfirmationAuthority` lives in this module and is taken only after a
//!   person confirmed. No function here returns it or a reference to it, it is not
//!   serialisable, and no command of the window carries it: the window can ask for the
//!   dialog and learns the outcome, nothing else.
//! - A printer's certificate is pinned the same way, through [`confirm_certificate`] only:
//!   it takes the first look itself (`printer_certificate_probe`), shows the address, serial,
//!   fingerprint, subject, issuer and the core's note in the same dialog
//!   ([`certificate_text`]), and calls the core's `printer_certificate_confirm` only for
//!   [`Answer::Confirm`], with that look's own fingerprint. The window sends no fingerprint,
//!   and `printer_certificate_confirm` is no command of the window.
//! - The agreement to web research is recorded the same way, through [`confirm_research`]
//!   only: for the source chosen in the settings and no other, after the core's explanation
//!   for that source (who receives the queries; the words `hotend research on` prints) was
//!   shown in the same dialog ([`research_text`]), and only for [`Answer::Confirm`].
//! - One confirmation at a time, and no review of any job while a dialog is open
//!   ([`ReviewLock`]): otherwise the window could replace the reviewed delivery behind an
//!   open dialog and the person's answer would apply to something they were not shown.
//!
//! The production prompt is a system dialog drawn by GTK, AppKit or Win32 from Rust
//! ([`crate::dialog`]). It is not part of the web page: the page cannot draw over it, read
//! it or press it.

use std::future::Future;
use std::sync::{Arc, OnceLock};

use hotend_core::{
    ApiError, ConfirmationAuthority, Core, DeliveryChoice, EditableProject, PrintJob, PrintReview,
    PrinterCertificate, PrinterLanSetup, ProjectDestination, ProjectReview, SearchSource,
    SearchSourceOption, SliceSummary,
};

use crate::shell::LanSetup;

/// What the person answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Confirm,
    Cancel,
}

/// The text of the dialog, one fact per line (a system dialog has no table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmationText {
    pub title: String,
    pub body: String,
    /// The label of the confirming button. A prompt must not make it the default button
    /// or give it the keyboard focus when the dialog opens ([`crate::dialog`] does neither).
    pub confirm_label: String,
    pub cancel_label: String,
}

/// Something that can put the confirmation in front of a person. `ask` blocks until they
/// answered; it is called on a blocking thread, never on the event loop.
pub trait ConfirmationPrompt: Send + Sync {
    fn ask(&self, text: &ConfirmationText) -> Answer;
}

/// A prompt that cannot ask anybody. Every confirmation through it is refused before a
/// review is made.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoPrompt;

impl ConfirmationPrompt for NoPrompt {
    fn ask(&self, _text: &ConfirmationText) -> Answer {
        Answer::Cancel
    }
}

/// Held while a confirmation dialog is open, and briefly by every review.
#[derive(Debug, Default)]
pub struct ReviewLock(tokio::sync::Mutex<()>);

impl ReviewLock {
    /// The lock, or `busy` while a confirmation dialog is open.
    pub fn try_hold(&self) -> Result<tokio::sync::MutexGuard<'_, ()>, ApiError> {
        self.0.try_lock().map_err(|_| ApiError::Busy {
            message: "a confirmation is waiting for an answer in the Hotend dialog".to_owned(),
            next_step: "Answer the dialog first.".to_owned(),
        })
    }
}

/// The outcome of [`confirm_print`].
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PrintConfirmation {
    /// Whether the person pressed the confirming button.
    pub confirmed: bool,
    /// The job as it is now: delivered when confirmed, still `pending` otherwise.
    pub job: PrintJob,
}

/// The process's confirmation authority. Private to this module on purpose.
fn authority() -> Option<&'static ConfirmationAuthority> {
    static AUTHORITY: OnceLock<Option<ConfirmationAuthority>> = OnceLock::new();
    AUTHORITY.get_or_init(ConfirmationAuthority::take).as_ref()
}

/// Reviews a pending job, shows the confirmation and, only when the person confirms,
/// delivers the job.
///
/// `can_ask` is false for a shell that has nobody to ask (the development bridge without
/// its test switch): the call is then refused before anything is reviewed.
pub(crate) async fn confirm_print(
    core: &Core,
    prompt: &Arc<dyn ConfirmationPrompt>,
    can_ask: bool,
    lock: &ReviewLock,
    job_id: &str,
    delivery: DeliveryChoice,
) -> Result<PrintConfirmation, ApiError> {
    if !can_ask {
        return Err(ApiError::NotConfigured {
            message: "this Hotend has no window to show the print confirmation in, so it \
                      cannot confirm a print"
                .to_owned(),
            next_step: "Confirm the print in the Hotend application. (The development \
                        bridge confirms only when it was started with \
                        --answer-dialogs-for-tests.)"
                .to_owned(),
        });
    }
    let _open = lock.try_hold()?;
    let review = core.print_review(job_id, delivery).await?;
    let project = core
        .project_get(&review.job.project_id)
        .await
        .map(|detail| detail.project.name)
        .unwrap_or_else(|_| "Project".to_owned());
    let stand_in = stand_in_slice(core, &review).await;
    let mut text = match &stand_in {
        Some(slice) => confirmation_text(&project, &with_stand_in_estimates(&review, slice), true),
        None => confirmation_text(&project, &review, false),
    };
    text.body = with_checked_text(&text.body, &review.review_text);
    // The prompt and the core get the same text: what is confirmed is what was drawn.
    let text = Arc::new(text);
    let (asker, asked) = (Arc::clone(prompt), Arc::clone(&text));
    let answer = tokio::task::spawn_blocking(move || asker.ask(&asked))
        .await
        .unwrap_or(Answer::Cancel);
    if answer != Answer::Confirm {
        return Ok(PrintConfirmation {
            confirmed: false,
            job: core.print_status(job_id).await?,
        });
    }
    let authority = authority().ok_or_else(|| ApiError::Internal {
        message: "the print confirmation was already claimed by another part of this \
                  process, so nothing was started"
            .to_owned(),
        next_step: "Restart Hotend and review the print again.".to_owned(),
    })?;
    let job = core.print_confirm(authority, job_id, &text.body).await?;
    Ok(PrintConfirmation {
        confirmed: true,
        job,
    })
}

/// The line between the dialog's own lines and the review text of the checked file.
pub const CHECKED_HEADING: &str =
    "What the file check read from the sliced file. This is what you confirm:";

/// The dialog's body with the review text of the checked file below it: the text as the
/// printer layer wrote it, every character, in one piece.
pub fn with_checked_text(body: &str, review_text: &str) -> String {
    format!("{body}\n\n{CHECKED_HEADING}\n{review_text}")
}

// ---- editable Bambu Studio projects ---------------------------------------------------

/// The outcome of [`confirm_project`].
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ProjectConfirmation {
    /// Whether the person pressed the confirming button.
    pub confirmed: bool,
    /// The project as it is now: handed over when confirmed, as it was reviewed otherwise.
    pub project: EditableProject,
}

/// What [`confirm_project`] needs of the core. [`Core`] is the one implementation outside
/// this module's tests; the trait is private, so nothing else can be given the authority.
trait ProjectHandOver: Sync {
    fn review(
        &self,
        project_export_id: &str,
        destination: ProjectDestination,
    ) -> impl Future<Output = Result<ProjectReview, ApiError>> + Send;
    /// The name of the design project, for the dialog.
    fn project_name(&self, project_id: &str) -> impl Future<Output = String> + Send;
    fn deliver(
        &self,
        authority: &ConfirmationAuthority,
        project_export_id: &str,
        shown: &str,
    ) -> impl Future<Output = Result<EditableProject, ApiError>> + Send;
}

impl ProjectHandOver for Core {
    async fn review(
        &self,
        project_export_id: &str,
        destination: ProjectDestination,
    ) -> Result<ProjectReview, ApiError> {
        Ok(self
            .print_project_review(project_export_id, destination)
            .await?)
    }

    async fn project_name(&self, project_id: &str) -> String {
        self.project_get(project_id)
            .await
            .map(|detail| detail.project.name)
            .unwrap_or_else(|_| PROJECT.to_owned())
    }

    async fn deliver(
        &self,
        authority: &ConfirmationAuthority,
        project_export_id: &str,
        shown: &str,
    ) -> Result<EditableProject, ApiError> {
        Ok(self
            .print_project_deliver(authority, project_export_id, shown)
            .await?)
    }
}

/// Reviews an editable project for `destination`, shows the confirmation and, only when the
/// person confirms, hands the project over with the text that was shown.
///
/// `can_ask` is as for [`confirm_print`]: without anybody to ask, the call is refused before
/// anything is reviewed.
pub(crate) async fn confirm_project(
    core: &Core,
    prompt: &Arc<dyn ConfirmationPrompt>,
    can_ask: bool,
    lock: &ReviewLock,
    project_export_id: &str,
    destination: ProjectDestination,
) -> Result<ProjectConfirmation, ApiError> {
    confirm_project_with(core, prompt, can_ask, lock, project_export_id, destination).await
}

async fn confirm_project_with<P: ProjectHandOver>(
    projects: &P,
    prompt: &Arc<dyn ConfirmationPrompt>,
    can_ask: bool,
    lock: &ReviewLock,
    project_export_id: &str,
    destination: ProjectDestination,
) -> Result<ProjectConfirmation, ApiError> {
    if !can_ask {
        return Err(ApiError::NotConfigured {
            message: "this Hotend has no window to show the confirmation in, so it cannot \
                      hand over a Bambu Studio project"
                .to_owned(),
            next_step: "Save the project in the Hotend application. (The development \
                        bridge confirms only when it was started with \
                        --answer-dialogs-for-tests.)"
                .to_owned(),
        });
    }
    let _open = lock.try_hold()?;
    let review = projects.review(project_export_id, destination).await?;
    let name = projects.project_name(&review.project.project_id).await;
    let mut text = project_confirmation_text(&name, &review);
    text.body = with_project_text(&text.body, &review.review_text);
    // The prompt and the core get the same text: what is confirmed is what was drawn.
    let text = Arc::new(text);
    let (asker, asked) = (Arc::clone(prompt), Arc::clone(&text));
    let answer = tokio::task::spawn_blocking(move || asker.ask(&asked))
        .await
        .unwrap_or(Answer::Cancel);
    if answer != Answer::Confirm {
        return Ok(ProjectConfirmation {
            confirmed: false,
            project: review.project,
        });
    }
    let authority = authority().ok_or_else(|| ApiError::Internal {
        message: "the confirmation was already claimed by another part of this process, so \
                  nothing was handed over"
            .to_owned(),
        next_step: "Restart Hotend and review the project again.".to_owned(),
    })?;
    let project = projects
        .deliver(authority, project_export_id, &text.body)
        .await?;
    Ok(ProjectConfirmation {
        confirmed: true,
        project,
    })
}

/// The line between the dialog's own lines and the review text of an editable project.
pub const PROJECT_HEADING: &str = "What Hotend checked of this project. This is what you confirm:";

/// The dialog's body with the project's review text below it: the text as the printer
/// layer wrote it, every character, in one piece.
pub fn with_project_text(body: &str, review_text: &str) -> String {
    format!("{body}\n\n{PROJECT_HEADING}\n{review_text}")
}

// ---- the printer's certificate --------------------------------------------------------

/// The outcome of [`confirm_certificate`].
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CertificateConfirmation {
    /// Whether the person pressed the confirming button (and the pin was stored).
    pub confirmed: bool,
    /// How reading the printer's status is set up afterwards.
    pub setup: LanSetup,
}

/// What [`confirm_certificate`] needs of the core: a first look at the printer's
/// certificate, the pin of exactly what that look returned, and the setup. [`Core`] is the
/// implementation of the application; the tests give one that connects to nothing.
pub trait CertificatePinning: Sync {
    /// A first look at the configured printer's certificate (`Core::printer_certificate_probe`).
    fn probe(&self) -> impl Future<Output = Result<PrinterCertificate, ApiError>> + Send;
    /// Pins the certificate of the last first look (`Core::printer_certificate_confirm`).
    fn pin(&self, sha256: &str) -> impl Future<Output = Result<(), ApiError>> + Send;
    fn setup(&self) -> impl Future<Output = Result<PrinterLanSetup, ApiError>> + Send;
}

impl CertificatePinning for Core {
    async fn probe(&self) -> Result<PrinterCertificate, ApiError> {
        Ok(self.printer_certificate_probe().await?)
    }

    async fn pin(&self, sha256: &str) -> Result<(), ApiError> {
        self.printer_certificate_confirm(sha256).await?;
        Ok(())
    }

    async fn setup(&self) -> Result<PrinterLanSetup, ApiError> {
        Ok(self.printer_lan_get().await?)
    }
}

/// Takes a first look at the printer's certificate, shows it in the native dialog and,
/// only when the person confirms there, pins it: with the fingerprint of that very look,
/// never one the window sent (the window sends nothing). "Cancel", closing the dialog and a
/// dialog that could not be shown store nothing.
///
/// `can_ask` is as for [`confirm_print`]: without anybody to ask, the call is refused
/// before the printer is looked at.
pub async fn confirm_certificate<P: CertificatePinning>(
    pinning: &P,
    prompt: &Arc<dyn ConfirmationPrompt>,
    can_ask: bool,
    lock: &ReviewLock,
) -> Result<CertificateConfirmation, ApiError> {
    if !can_ask {
        return Err(ApiError::NotConfigured {
            message: "this Hotend has no window to show the printer's certificate in, so it \
                      cannot confirm it"
                .to_owned(),
            next_step: "Confirm the certificate in the Hotend application, or with `hotend \
                        printer status` at a terminal, which asks."
                .to_owned(),
        });
    }
    // Refused at once while another dialog is open; the lock itself is taken only for the
    // dialog, not during the look at the printer (a network handshake that can take until
    // its timeout), so a print's confirmation is not held up by it.
    drop(lock.try_hold()?);
    let seen = pinning.probe().await?;
    let _open = lock.try_hold()?;
    let text = Arc::new(certificate_text(&seen));
    let (asker, asked) = (Arc::clone(prompt), Arc::clone(&text));
    let answer = tokio::task::spawn_blocking(move || asker.ask(&asked))
        .await
        .unwrap_or(Answer::Cancel);
    if answer == Answer::Confirm {
        pinning.pin(&seen.sha256).await?;
    }
    Ok(CertificateConfirmation {
        confirmed: answer == Answer::Confirm,
        setup: LanSetup::from_core(pinning.setup().await?),
    })
}

/// The dialog for a first look at the printer's certificate. The address and serial are
/// the ones the core checked; the subject and issuer come from the printer and are put on
/// one line each, after a label of the dialog's own, in full up to [`CERT_NAME_CHARS`]
/// and, past that, cut with a note that says so ([`certificate_name`]); the fingerprint is
/// shown whole; the core's note on what the subject proves is shown as written.
pub fn certificate_text(seen: &PrinterCertificate) -> ConfirmationText {
    let lines = [
        format!("Printer: {}", line(&seen.ip)),
        format!("Serial: {}", line(&seen.serial)),
        format!("Fingerprint: {}", clip(&seen.sha256, FINGERPRINT_CHARS)),
        format!("Subject: {}", certificate_name(&seen.subject)),
        format!("Issuer: {}", certificate_name(&seen.issuer)),
        String::new(),
        "Hotend only reads this printer's status over your network; it sends the printer \
         nothing. Later connections must show this same certificate."
            .to_owned(),
        String::new(),
        seen.note.clone(),
    ];
    ConfirmationText {
        title: "Trust this printer's certificate?".to_owned(),
        body: lines.join("\n"),
        confirm_label: "Trust this certificate".to_owned(),
        cancel_label: "Cancel".to_owned(),
    }
}

/// A SHA-256 in hex is 64 characters; a longer value is not a fingerprint and is cut.
const FINGERPRINT_CHARS: usize = 64;

/// A certificate's subject or issuer is shown in full up to this many characters (it wraps
/// in its cell of the dialog); real printer certificates name far less.
pub const CERT_NAME_CHARS: usize = 1024;

/// A subject or issuer from the printer for one dialog line ([`clip`]'s cleaning). Up to
/// [`CERT_NAME_CHARS`] it is shown whole; a longer one is cut and the line says so and how
/// long the whole is, so a hidden end is never mistaken for the end. The fingerprint, not
/// the name, is what is pinned.
fn certificate_name(text: &str) -> String {
    let cleaned = clip(text, usize::MAX);
    let length = cleaned.chars().count();
    if length <= CERT_NAME_CHARS {
        return cleaned;
    }
    let kept: String = cleaned.chars().take(CERT_NAME_CHARS).collect();
    format!(
        "{}… [cut: {length} characters in all; the fingerprint is what is trusted]",
        kept.trim_end()
    )
}

// ---- the agreement to web research ----------------------------------------------------

/// The outcome of [`confirm_research`].
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ResearchConsentAnswer {
    /// Whether the person pressed the confirming button (and the agreement was recorded).
    pub agreed: bool,
    /// The research sources afterwards.
    pub options: Vec<SearchSourceOption>,
}

/// Shows the core's agreement text for `source` (who receives the queries) in the native
/// dialog and, only when the person agrees there, records the agreement for `source`
/// (`Core::research_consent_for`). Refused before any dialog when `source` is not the
/// source chosen in the settings, or is `off`: the person would read about another
/// receiver than the one that gets the queries. The core refuses it again if the chosen
/// source changed while the dialog was open.
pub(crate) async fn confirm_research(
    core: &Core,
    prompt: &Arc<dyn ConfirmationPrompt>,
    can_ask: bool,
    lock: &ReviewLock,
    source: SearchSource,
) -> Result<ResearchConsentAnswer, ApiError> {
    if !can_ask {
        return Err(ApiError::NotConfigured {
            message: "this Hotend has no window to ask for the agreement to web research in, \
                      so it cannot record one"
                .to_owned(),
            next_step: "Agree in the Hotend application, or with `hotend research on` at a \
                        terminal."
                .to_owned(),
        });
    }
    let _open = lock.try_hold()?;
    let chosen = core.settings_get().await?.research.source;
    if source != chosen || source == SearchSource::Off {
        return Err(ApiError::InvalidInput {
            message: format!(
                "the research source chosen in the settings is {}, not {}: nothing was asked \
                 and nothing was agreed to",
                chosen.as_str(),
                source.as_str()
            ),
            next_step: "Choose the source first, then turn research on for it.".to_owned(),
        });
    }
    let key_present = match source.service() {
        Some(service) => core.search_key_status(service).await.unwrap_or(false),
        None => false,
    };
    let text = Arc::new(research_text(source, key_present));
    let (asker, asked) = (Arc::clone(prompt), Arc::clone(&text));
    let answer = tokio::task::spawn_blocking(move || asker.ask(&asked))
        .await
        .unwrap_or(Answer::Cancel);
    if answer == Answer::Confirm {
        core.research_consent_for(source, true).await?;
    }
    Ok(ResearchConsentAnswer {
        agreed: answer == Answer::Confirm,
        options: core.research_source_options().await?,
    })
}

/// The dialog for agreeing to web research through `source`: the core's own explanation for
/// that source (`hotend_core::research::consent_explanation`, which names who receives the
/// queries), word for word what `hotend research on` prints before asking.
pub fn research_text(source: SearchSource, key_present: bool) -> ConfirmationText {
    let receiver = source.service().map_or_else(
        || "your AI provider".to_owned(),
        |service| service.label().to_owned(),
    );
    ConfirmationText {
        title: format!("Agree to web research through {receiver}?"),
        body: hotend_core::research::consent_explanation(source, key_present),
        confirm_label: "Agree".to_owned(),
        cancel_label: "Cancel".to_owned(),
    }
}

/// The window's sentence shown wherever an editable project is offered
/// (`S.print.project.outside` in `src/hotend/strings.ts`). A test holds it to that file.
const PROJECT_OUTSIDE: &str =
    "A project opened this way is sliced again by Bambu Studio, so it is outside Hotend's check.";

/// The dialog for handing over an editable project. Every fact comes from `review`, which
/// the core computed; the title and the button say what happens (a save, and the opening of
/// Bambu Studio only where this Hotend opens it), never that anything is printed.
pub fn project_confirmation_text(project: &str, review: &ProjectReview) -> ConfirmationText {
    let opens = review.handoff.as_ref().is_some_and(|target| target.opens);
    let (title, confirm_label) = match &review.destination {
        ProjectDestination::BambuStudio if opens => (
            "Save this project and open it in Bambu Studio?",
            "Save and open Bambu Studio",
        ),
        ProjectDestination::BambuStudio => ("Save this project for Bambu Studio?", "Save"),
        ProjectDestination::Folder { .. } => ("Save this Bambu Studio project?", "Save"),
    };
    let mut lines: Vec<String> = vec![format!(
        "{PROJECT}: {}, version {}, plate {}",
        line(project),
        review.project.version,
        review.project.plate
    )];
    match &review.destination {
        ProjectDestination::BambuStudio => {
            if opens {
                lines.push(
                    "For: Bambu Studio. Hotend saves the editable project to the folder below"
                        .to_owned(),
                );
                lines.push(
                    "and asks the system to open it with Bambu Studio. Nothing is printed here."
                        .to_owned(),
                );
            } else {
                lines.push(
                    "For: Bambu Studio. Hotend saves the editable project to the folder below."
                        .to_owned(),
                );
                lines.push(
                    "It does not open Bambu Studio: you open the project there yourself."
                        .to_owned(),
                );
            }
            if let Some(target) = &review.handoff {
                let shown = target
                    .windows_folder
                    .as_deref()
                    .map_or_else(|| target.folder.clone(), std::path::PathBuf::from);
                lines.push(format!("Folder: {}", path_line(&shown)));
            }
        }
        ProjectDestination::Folder { folder } => {
            lines.push(
                "For: a folder. Hotend saves the editable project there; nothing is opened."
                    .to_owned(),
            );
            lines.push(format!("Folder: {}", path_line(folder)));
        }
    }
    lines.push(format!(
        "File: {} (check code {})",
        clip(&review.file_name, FILE_CHARS),
        clip(&short_hash(&review.project.sha256), 16)
    ));
    if !review.warnings.is_empty() {
        lines.push(String::new());
    }
    for warning in review.warnings.iter().take(WARNING_LINES) {
        lines.push(format!(
            "Warning: {}",
            clip(&warning.message, WARNING_CHARS)
        ));
    }
    if let Some(more) = review.warnings.len().checked_sub(WARNING_LINES + 1) {
        lines.push(format!(
            "Warning: and {} more from the check of the test slice",
            more + 1
        ));
    }
    lines.push(String::new());
    lines.push(PROJECT_OUTSIDE.to_owned());
    ConfirmationText {
        title: title.to_owned(),
        body: lines.join("\n"),
        confirm_label: confirm_label.to_owned(),
        cancel_label: "Cancel".to_owned(),
    }
}

/// The warning the stand-in slicer puts on every slice it writes (`hotend-slicer`).
const STAND_IN_SLICER: &str = "fake_slicer";

/// What the file check says of a file with nothing to print in it. Every placeholder of
/// the stand-in slicer has it; the dialog says what that means in the window's sentence.
const PLACEHOLDER_WARNING: &str = "no_moves";

/// The window's own sentence for such a file and its labels for the two temperatures
/// (`S.print` in `src/hotend/strings.ts`). A test holds them to that file.
const PLACEHOLDER_SENTENCE: &str = "This file is a placeholder from the stand-in slicer. \
                                    It can only run on the simulated printer.";
const NOZZLE_UP_TO: &str = "Nozzle up to";
const BED_UP_TO: &str = "Bed up to";
/// The chamber's line, in the same form as the two above.
const CHAMBER_UP_TO: &str = "Chamber up to";

/// Whether the reviewed file is a placeholder from the stand-in slicer: the version's
/// stored slice is this very file (same SHA-256) and carries the stand-in's warning. When
/// the core cannot say, the answer is no and the dialog keeps every warning of the file.
/// The stand-in's slice record of it then, which holds its rough estimates.
async fn stand_in_slice(core: &Core, review: &PrintReview) -> Option<SliceSummary> {
    core.slice_get(&review.job.project_id, Some(review.job.version))
        .await
        .ok()
        .flatten()
        .map(|slice| slice.summary)
        .filter(|summary| is_stand_in_record_of(summary, review))
}

/// Whether `slice` is the stand-in's record of the very file `review` is of: the same
/// SHA-256, and the stand-in's warning on it.
pub fn is_stand_in_record_of(slice: &SliceSummary, review: &PrintReview) -> bool {
    slice.sha256.eq_ignore_ascii_case(&review.summary.sha256)
        && slice
            .warnings
            .iter()
            .any(|warning| warning.code == STAND_IN_SLICER)
}

/// The review with the stand-in's estimates in place of what its file states. A
/// placeholder's file states no time, layers or grams (it prints nothing and claims nothing
/// it does not do); the dialog shows the estimates of the slice record kept for that very
/// file, as rough estimates of the stand-in, as the window does (`shownSummary` in
/// `PrintPanel.tsx`). The checked review text below the dialog's lines stays the file's.
pub fn with_stand_in_estimates(review: &PrintReview, slice: &SliceSummary) -> PrintReview {
    let mut shown = review.clone();
    if slice.time_s.is_finite() && slice.time_s >= 0.0 {
        // Bounded and finite: the cast cannot wrap.
        shown.summary.estimated_seconds = Some(slice.time_s.min(1.0e9).round() as u64);
    }
    shown.summary.layers = Some(slice.layers);
    for filament in &mut shown.summary.filaments {
        if let Some(used) = slice
            .filament
            .iter()
            .find(|used| used.slot == filament.slot)
            && used.grams.is_finite()
        {
            filament.grams = Some(used.grams);
        }
    }
    shown
}

/// "1 h 3 min", "59 min" or "55 s": to the nearest minute from one minute up. This is the
/// window's `duration` (`src/hotend/format.ts`) rule for rule, and
/// `tests/fixtures/durations.json` holds the cases both must pass.
fn duration(seconds: u64) -> String {
    if seconds < 60 {
        return format!("{seconds} s");
    }
    let minutes = seconds.saturating_add(30) / 60;
    match (minutes / 60, minutes % 60) {
        (0, minutes) => format!("{minutes} min"),
        (hours, 0) => format!("{hours} h"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    }
}

/// One decimal, a tie rounded up: the window's `toFixed(1)`. Rust's own formatting rounds
/// a tie to the even digit ("28.2" for 28.25, where the window writes "28.3"). A tie is a
/// value ending in exactly .25 or .75, which is when four times the value is a whole number.
fn one_decimal(value: f64) -> String {
    let quarters = value * 4.0;
    if (0.0..1e12).contains(&value) && quarters.fract() == 0.0 {
        // Hundredths as a whole number (the range was checked, the cast is exact).
        let tenths = (quarters as u64 * 25 + 5) / 10;
        format!("{}.{}", tenths / 10, tenths % 10)
    } else {
        format!("{value:.1}")
    }
}

/// Whole degrees, a half rounded up, as the window's review writes them.
fn degrees(value: f64) -> String {
    format!("{:.0} °C", value.round())
}

/// The nozzle by the name the window gives it. The id is the printer's own (device
/// extruder): 1 is the main nozzle, 0 the auxiliary one.
fn nozzle(id: i64) -> String {
    match id {
        0 => "auxiliary nozzle".to_owned(),
        1 => "main nozzle".to_owned(),
        other => format!("nozzle {other}"),
    }
}

/// An everyday word for a colour the printer reports as `RRGGBB` or `RRGGBBAA`. A dialog
/// cannot draw the swatch the window shows. `None` for text that is no colour and for a
/// fully transparent one, which is how a tray says that it knows none.
fn colour_word(hex: &str) -> Option<&'static str> {
    if !(hex.len() == 6 || hex.len() == 8) || !hex.is_ascii() {
        return None;
    }
    let channel = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).ok();
    let (red, green, blue) = (channel(0)?, channel(2)?, channel(4)?);
    if hex.len() == 8 && channel(6)? == 0 {
        return None;
    }
    let (r, g, b) = (
        f64::from(red) / 255.0,
        f64::from(green) / 255.0,
        f64::from(blue) / 255.0,
    );
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let (chroma, light) = (max - min, (max + min) / 2.0);
    if chroma < 0.12 {
        return Some(match light {
            l if l > 0.8 => "white",
            l if l < 0.2 => "black",
            _ => "grey",
        });
    }
    let hue = 60.0
        * if max == r {
            ((g - b) / chroma).rem_euclid(6.0)
        } else if max == g {
            (b - r) / chroma + 2.0
        } else {
            (r - g) / chroma + 4.0
        };
    Some(match hue {
        h if !(10.0..345.0).contains(&h) => {
            if light > 0.6 {
                "pink"
            } else {
                "red"
            }
        }
        h if h < 40.0 => {
            if light < 0.42 {
                "brown"
            } else {
                "orange"
            }
        }
        h if h < 70.0 => {
            if light < 0.45 {
                "olive"
            } else {
                "yellow"
            }
        }
        h if h < 165.0 => "green",
        h if h < 195.0 => "turquoise",
        h if h < 250.0 => "blue",
        h if h < 295.0 => "purple",
        _ => "pink",
    })
}

/// "PLA Basic #FFFFFFFF", as the core names a loaded spool, for a dialog line: "PLA Basic,
/// white".
fn spool(loaded: &str) -> String {
    let loaded = loaded.trim();
    match loaded.rsplit_once('#') {
        Some((name, hex))
            if matches!(hex.len(), 6 | 8) && hex.chars().all(|c| c.is_ascii_hexdigit()) =>
        {
            match (name.trim(), colour_word(hex)) {
                ("", Some(colour)) => colour.to_owned(),
                (name, Some(colour)) => format!("{}, {colour}", clip(name, MATERIAL_CHARS)),
                (name, None) => clip(name, MATERIAL_CHARS),
            }
        }
        _ => clip(loaded, MATERIAL_CHARS),
    }
}

/// The first six and the last four characters of a hash.
fn short_hash(hash: &str) -> String {
    if hash.len() > 12 && hash.is_ascii() {
        format!("{}…{}", &hash[..6], &hash[hash.len() - 4..])
    } else {
        hash.to_owned()
    }
}

/// Whether a character changes how the text around it is laid out without showing
/// itself: zero-width characters, the bidirectional marks, embeddings, overrides and
/// isolates, variation selectors, tags and fillers, and every other character the printer
/// layer refuses in a confirmed destination (`hotend_core::is_hidden_format`, Unicode's
/// format characters). Such a character is dropped.
fn is_invisible(c: char) -> bool {
    hotend_core::is_hidden_format(c)
        || matches!(
                c,
            '\u{00AD}'
                | '\u{061C}'
                | '\u{115F}'
                | '\u{1160}'
                | '\u{180B}'..='\u{180F}'
                | '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}'
                // The Braille blank shows as an empty cell but is neither space nor control.
                | '\u{2800}'
                | '\u{3164}'
                | '\u{FE00}'..='\u{FE0F}'
                | '\u{FEFF}'
                | '\u{FFA0}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{1D173}'..='\u{1D17A}'
                | '\u{E0000}'..='\u{E0FFF}'
        )
}

/// Text that came from a file, a name or a setting, made safe for one dialog line.
///
/// Every control character and every kind of white space, the line and paragraph
/// separators U+2028 and U+2029 among them (Pango breaks a line at both), becomes a plain
/// space, and runs of spaces become one. Characters of [`is_invisible`] are dropped. The
/// text is cut to `most` characters, and an ellipsis says so. What comes out has no line
/// break in it and cannot reorder the text next to it.
fn clip(text: &str, most: usize) -> String {
    let mut cleaned = String::new();
    for c in text.chars() {
        if is_invisible(c) {
            continue;
        }
        if c.is_control() || c.is_whitespace() {
            if !cleaned.is_empty() && !cleaned.ends_with(' ') {
                cleaned.push(' ');
            }
        } else {
            cleaned.push(c);
        }
    }
    let cleaned = cleaned.trim_end();
    if cleaned.chars().count() <= most {
        return cleaned.to_owned();
    }
    let kept: String = cleaned.chars().take(most.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

/// [`clip`] at the length of the longest name the core stores.
fn line(text: &str) -> String {
    clip(text, NAME_CHARS)
}

/// A path for one dialog line. A long one keeps both ends: where it starts and the folder
/// it ends in.
fn path_line(path: &std::path::Path) -> String {
    let cleaned = clip(&path.display().to_string(), usize::MAX);
    let length = cleaned.chars().count();
    if length <= PATH_CHARS {
        return cleaned;
    }
    let half = PATH_CHARS / 2;
    let head: String = cleaned.chars().take(half).collect();
    let tail: String = cleaned.chars().skip(length - half).collect();
    format!("{head}…{tail}")
}

/// The longest piece of text from outside in one line: a project name is at most this long.
const NAME_CHARS: usize = 120;
/// A material or a loaded spool, of which a filament line carries one each.
const MATERIAL_CHARS: usize = 48;
const PATH_CHARS: usize = 160;
const WARNING_CHARS: usize = 300;
const FILE_CHARS: usize = 80;
const PROJECT: &str = "Project";

/// The labels a line of the dialog may start with ("<label>: <value>"). Every line that
/// carries text from outside starts with one of them; a line that starts with none is one
/// of the dialog's own sentences.
pub const LABELS: [&str; 17] = [
    PROJECT,
    "Printer",
    "For",
    "Folder",
    "Time",
    "Filament",
    NOZZLE_UP_TO,
    BED_UP_TO,
    CHAMBER_UP_TO,
    "Warning",
    "From the web",
    "File",
    "To",
    "Serial",
    "Fingerprint",
    "Subject",
    "Issuer",
];

/// The body as the dialog draws it: one row per line, its label (when it has one of
/// [`LABELS`]) apart from its value. Only the text after a label's ": " is the value; a
/// line's label is decided by the dialog's own words at its start, which outside text,
/// always after a label, cannot reach.
pub fn rows(body: &str) -> Vec<(Option<&'static str>, &str)> {
    body.split('\n')
        .map(|text| {
            LABELS
                .iter()
                .find_map(|label| {
                    text.strip_prefix(label)
                        .and_then(|rest| rest.strip_prefix(": "))
                        .map(|value| (Some(*label), value))
                })
                .unwrap_or((None, text))
        })
        .collect()
}
/// How many filaments and warnings get a line each; the rest are counted in one line.
/// The dialog cannot scroll, and its buttons must stay on the screen.
const FILAMENT_LINES: usize = 6;
const WARNING_LINES: usize = 6;

/// The dialog for a review (design specification, section 4.6). Every number comes from
/// `review`, which the core computed from the sliced file. `stand_in` says that the file is
/// a placeholder from the stand-in slicer (`confirm_print` asks the core).
pub fn confirmation_text(project: &str, review: &PrintReview, stand_in: bool) -> ConfirmationText {
    let summary = &review.summary;
    // Whether the hand-off asks Bambu Connect to import the file: true when the core holds
    // a real opener, false for the stand-in (the development bridge, the tests), which
    // opens nothing.
    let opens = review.handoff.as_ref().is_some_and(|target| target.opens);
    let (title, confirm_label) = match &review.delivery {
        DeliveryChoice::SimulatedPrinter => ("Start this print?", "Start print"),
        DeliveryChoice::BambuConnect if opens => (
            "Save this file and open it in Bambu Connect?",
            "Save and open Bambu Connect",
        ),
        DeliveryChoice::BambuConnect => ("Save this file for Bambu Connect?", "Save"),
        DeliveryChoice::ExportToFolder { .. } => ("Save this file?", "Save"),
    };
    let mut lines: Vec<String> = Vec::new();
    if review.simulated {
        lines.push("SIMULATED PRINTER. Nothing will be printed.".to_owned());
        lines.push(String::new());
    }
    lines.push(format!(
        "{PROJECT}: {}, version {}, plate {}",
        line(project),
        review.job.version,
        review.plate
    ));
    match &review.delivery {
        DeliveryChoice::SimulatedPrinter => {
            if let Some(printer) = &review.printer {
                let serial = &printer.serial;
                let tail = serial
                    .char_indices()
                    .rev()
                    .nth(3)
                    .map_or(serial.as_str(), |(at, _)| &serial[at..]);
                lines.push(format!(
                    "Printer: {}, serial ending {}",
                    clip(&printer.name, FILE_CHARS),
                    clip(tail, 4)
                ));
            }
        }
        DeliveryChoice::BambuConnect => {
            if opens {
                lines.push(
                    "For: Bambu Connect. Hotend saves the checked file to the folder below and"
                        .to_owned(),
                );
                lines.push(
                    "asks Bambu Connect to import it. You choose the printer and press Print"
                        .to_owned(),
                );
                lines.push(
                    "in Bambu Connect. Hotend learns nothing more about this print.".to_owned(),
                );
            } else {
                lines.push(
                    "For: Bambu Connect. Hotend saves the checked file to the folder below."
                        .to_owned(),
                );
                lines.push(
                    "It does not open Bambu Connect: you import the file there, choose the"
                        .to_owned(),
                );
                lines.push(
                    "printer and press Print. Hotend learns nothing more about this print."
                        .to_owned(),
                );
            }
            if let Some(target) = &review.handoff {
                // The folder as Bambu Connect reads it: the Windows form where there is one.
                let shown = target
                    .windows_folder
                    .as_deref()
                    .map_or_else(|| target.folder.clone(), std::path::PathBuf::from);
                lines.push(format!("Folder: {}", path_line(&shown)));
            }
        }
        DeliveryChoice::ExportToFolder { folder } => {
            lines.push(format!("Folder: {}", path_line(folder)));
        }
    }
    let mut time = Vec::new();
    match summary.estimated_seconds.map(duration) {
        // The window's review says "About 1 h 3 min" of a stand-in file.
        Some(time_text) if stand_in => time.push(format!("about {time_text}")),
        Some(time_text) => time.push(time_text),
        None => time.push("not stated in the file".to_owned()),
    }
    if let Some(layers) = summary.layers {
        time.push(format!("{layers} layers"));
    }
    lines.push(format!("Time: {}", time.join(", ")));
    for filament in summary.filaments.iter().take(FILAMENT_LINES) {
        let choice = review.spools.iter().find(|s| s.slot == filament.slot);
        // The material the file states, else the one the job was sliced for (the window's
        // review does the same).
        let material = filament
            .material
            .as_deref()
            .or(choice.map(|choice| choice.material.as_str()));
        let mut parts = vec![material.map_or_else(
            || "material not stated in the file".to_owned(),
            |material| clip(material, MATERIAL_CHARS),
        )];
        if let Some(grams) = filament.grams {
            parts.push(format!("{} g", one_decimal(grams)));
        }
        if let Some(id) = filament.nozzle_id {
            parts.push(nozzle(id));
        }
        if let Some(choice) = choice {
            parts.push(format!(
                "AMS {} slot {} ({})",
                u32::from(choice.ams_unit) + 1,
                u32::from(choice.tray) + 1,
                spool(&choice.loaded)
            ));
        }
        lines.push(format!("Filament: {}", parts.join(", ")));
    }
    if let Some(more) = summary.filaments.len().checked_sub(FILAMENT_LINES + 1) {
        lines.push(format!("Filament: and {} more", more + 1));
    }
    // A placeholder states no temperature; a file to be printed that states none says so.
    for (label, value) in [
        (NOZZLE_UP_TO, summary.max_nozzle_c),
        (BED_UP_TO, summary.max_bed_c),
    ] {
        match value {
            Some(value) => lines.push(format!("{label}: {}", degrees(value))),
            None if stand_in => {}
            None => lines.push(format!("{label}: not stated in the file")),
        }
    }
    // Most printers heat no chamber and most files name none: the line is there when the
    // file heats one.
    if let Some(chamber) = summary.max_chamber_c {
        lines.push(format!("{CHAMBER_UP_TO}: {}", degrees(chamber)));
    }
    let warnings: Vec<&str> = review
        .warnings
        .iter()
        .filter(|warning| !(stand_in && warning.code == PLACEHOLDER_WARNING))
        .map(|warning| warning.message.as_str())
        .collect();
    if stand_in
        || !warnings.is_empty()
        || review
            .web_numbers
            .iter()
            .any(|number| number.state != "confirmed")
    {
        lines.push(String::new());
    }
    if stand_in {
        lines.push(PLACEHOLDER_SENTENCE.to_owned());
    }
    for warning in warnings.iter().take(WARNING_LINES) {
        lines.push(format!("Warning: {}", clip(warning, WARNING_CHARS)));
    }
    // Sizes of this version taken from a web page nobody has confirmed (docs/WEB_RESEARCH.md
    // says to show them above the confirmation; the CLI review does). They do not stop the
    // print: the person decides knowing them.
    let unconfirmed: Vec<_> = review
        .web_numbers
        .iter()
        .filter(|number| number.state != "confirmed")
        .collect();
    for number in unconfirmed.iter().take(WARNING_LINES) {
        lines.push(format!(
            "From the web: {} {} {} (from {}, not confirmed)",
            clip(&number.label, MATERIAL_CHARS),
            one_decimal(number.value),
            clip(&number.unit, 8),
            clip(&number.host, MATERIAL_CHARS)
        ));
    }
    if let Some(more) = unconfirmed.len().checked_sub(WARNING_LINES + 1) {
        lines.push(format!("From the web: and {} more not confirmed", more + 1));
    }
    if let Some(more) = warnings.len().checked_sub(WARNING_LINES + 1) {
        lines.push(format!(
            "Warning: and {} more from the check of the file",
            more + 1
        ));
    }
    lines.push(String::new());
    lines.push(
        if stand_in {
            "These numbers are rough estimates from the stand-in slicer."
        } else {
            "These numbers were read from the sliced file."
        }
        .to_owned(),
    );
    lines.push(format!(
        "File: {} (check code {})",
        clip(&summary.file_name, FILE_CHARS),
        clip(&short_hash(&summary.sha256), 16)
    ));
    ConfirmationText {
        title: title.to_owned(),
        body: lines.join("\n"),
        confirm_label: confirm_label.to_owned(),
        cancel_label: "Cancel".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// A review text in the printer layer's manner, with characters a careless dialog
    /// would change.
    const CHECKED: &str = "Print file \"plate_1.gcode.3mf\", plate 1, checked for the Bambu Lab X2D.\n\
                           Layers: the commands lay no plastic.\n\
                           - a line that starts with a dash, and  two spaces\n\
                           SHA-256 of the file: b8a6e6b8aa00000000000000000000000000000000000000000000000000bcf3.";

    fn review(delivery: serde_json::Value, simulated: bool) -> PrintReview {
        serde_json::from_value(serde_json::json!({
            "job": {
                "id": "job-1", "project_id": "p", "version": 2,
                "slice_sha256": "b8a6e6b8aa00000000000000000000000000000000000000000000000000bcf3",
                "note": "", "status": "pending", "delivery": null,
                "created_at_ms": 1, "updated_at_ms": 1, "detail": {}
            },
            "delivery": delivery,
            "simulated": simulated,
            "printer": {"id": "simulated-x2d", "name": "Simulated X2D (no real printer)",
                        "model": "X2D", "serial": "20P0SIM00000001", "simulated": true},
            "plate": 1, "pieces": ["box", "lid"],
            "summary": {
                "file_name": "plate_1.gcode.3mf",
                "sha256": "b8a6e6b8aa00000000000000000000000000000000000000000000000000bcf3",
                "size_bytes": 2084, "plate": 1, "estimated_seconds": 3515, "layers": 200,
                "filaments": [{"slot": 1, "material": "PLA", "colour": null, "grams": 28.32,
                               "metres": 9.5, "nozzle_id": 1, "selected_in_gcode": true}],
                "max_nozzle_c": null, "max_bed_c": 55.0, "max_chamber_c": null, "extent": null
            },
            "review_text": CHECKED,
            "warnings": [{"severity": "warning", "code": "no_moves", "line": 0,
                          "message": "the G-code contains no moves;\nnothing would be printed",
                          "excerpt": ""}],
            "spools": [{"slot": 1, "material": "PLA", "nozzle_id": 1, "ams_unit": 0, "tray": 0,
                        "loaded": "PLA Basic #FFFFFFFF"}]
        }))
        .unwrap()
    }

    #[test]
    fn the_dialog_states_what_the_sliced_file_says_one_fact_per_line() {
        let mut real = review(serde_json::json!({"kind": "simulated_printer"}), true);
        real.summary.max_nozzle_c = Some(219.5);
        real.warnings[0].code = "position_unknown".to_owned();
        let text = confirmation_text("Cable box", &real, false);
        assert_eq!(text.title, "Start this print?");
        assert_eq!(text.confirm_label, "Start print");
        assert_eq!(text.cancel_label, "Cancel");
        assert_eq!(
            text.body,
            "SIMULATED PRINTER. Nothing will be printed.\n\
             \n\
             Project: Cable box, version 2, plate 1\n\
             Printer: Simulated X2D (no real printer), serial ending 0001\n\
             Time: 59 min, 200 layers\n\
             Filament: PLA, 28.3 g, main nozzle, AMS 1 slot 1 (PLA Basic, white)\n\
             Nozzle up to: 220 °C\n\
             Bed up to: 55 °C\n\
             \n\
             Warning: the G-code contains no moves; nothing would be printed\n\
             \n\
             These numbers were read from the sliced file.\n\
             File: plate_1.gcode.3mf (check code b8a6e6…bcf3)"
        );
    }

    /// The core puts the notice that the plate previews are Hotend's first among the
    /// warnings; the dialog, which shows only the first few, shows it with many others.
    #[test]
    fn the_hotend_made_previews_notice_is_shown_among_many_warnings() {
        let mut real = review(serde_json::json!({"kind": "bambu_connect"}), false);
        let lint = real.warnings[0].clone();
        let mut notice = lint.clone();
        notice.code = "hotend_made_previews".to_owned();
        notice.message =
            "plate previews made by Hotend: the slicer drew no plate preview".to_owned();
        real.warnings = std::iter::once(notice)
            .chain((0..8).map(|i| {
                let mut other = lint.clone();
                other.code = format!("lint_{i}");
                other.message = format!("lint finding {i}");
                other
            }))
            .collect();
        assert!(real.warnings.len() > WARNING_LINES);
        let text = confirmation_text("Keychain", &real, false);
        assert!(
            text.body.contains(
                "Warning: plate previews made by Hotend: the slicer drew no plate preview"
            ),
            "{}",
            text.body
        );
        assert!(!text.body.contains("lint finding 7"), "{}", text.body);
    }

    #[test]
    fn a_placeholder_of_the_stand_in_slicer_is_named_in_the_windows_words() {
        // The recorded example: a file from the stand-in slicer, which states no
        // temperature and has nothing to print in it. 3 790 s is "1 h 3 min" in the
        // window's review (to the nearest minute); the dialog once rounded up to 1 h 4 min.
        let mut placeholder = review(serde_json::json!({"kind": "simulated_printer"}), true);
        placeholder.summary.estimated_seconds = Some(3790);
        placeholder.summary.max_bed_c = None;
        placeholder.summary.filaments[0].grams = Some(30.25);
        let text = confirmation_text("A box that hides a power strip", &placeholder, true);
        assert_eq!(
            text.body,
            "SIMULATED PRINTER. Nothing will be printed.\n\
             \n\
             Project: A box that hides a power strip, version 2, plate 1\n\
             Printer: Simulated X2D (no real printer), serial ending 0001\n\
             Time: about 1 h 3 min, 200 layers\n\
             Filament: PLA, 30.3 g, main nozzle, AMS 1 slot 1 (PLA Basic, white)\n\
             \n\
             This file is a placeholder from the stand-in slicer. It can only run on the \
             simulated printer.\n\
             \n\
             These numbers are rough estimates from the stand-in slicer.\n\
             File: plate_1.gcode.3mf (check code b8a6e6…bcf3)"
        );
        // Words of the code, which the window does not use either.
        for word in ["G-code", "Hottest", "#FFFFFFFF", "nozzle 1", "not stated"] {
            assert!(!text.body.contains(word), "`{word}` in:\n{}", text.body);
        }

        // Any other warning of the file check stays, also on a placeholder.
        placeholder.warnings.push(
            serde_json::from_value(serde_json::json!({
                "severity": "warning", "code": "position_unknown", "line": 12,
                "message": "a move starts before the position is known", "excerpt": ""
            }))
            .unwrap(),
        );
        let text = confirmation_text("Box", &placeholder, true);
        assert!(
            text.body.contains(
                "simulated printer.\nWarning: a move starts before the position is known\n"
            ),
            "{}",
            text.body
        );
    }

    #[test]
    fn what_a_file_to_be_printed_does_not_state_is_said_not_left_out() {
        let mut silent = review(serde_json::json!({"kind": "simulated_printer"}), true);
        silent.summary.estimated_seconds = None;
        silent.summary.layers = None;
        silent.summary.max_bed_c = None;
        silent.summary.filaments[0].material = None;
        silent.summary.filaments[0].nozzle_id = Some(0);
        silent.spools.clear();
        silent.warnings.clear();
        let text = confirmation_text("Box", &silent, false);
        assert!(
            text.body.contains(
                "Time: not stated in the file\n\
                 Filament: material not stated in the file, 28.3 g, auxiliary nozzle\n\
                 Nozzle up to: not stated in the file\n\
                 Bed up to: not stated in the file\n\
                 \n\
                 These numbers were read from the sliced file."
            ),
            "{}",
            text.body
        );
    }

    #[test]
    fn the_dialog_shows_the_checked_review_text_word_for_word_below_its_own_lines() {
        let review = review(serde_json::json!({"kind": "simulated_printer"}), true);
        let own = confirmation_text("Cable box", &review, false).body;
        let body = with_checked_text(&own, &review.review_text);
        assert_eq!(body.matches(CHECKED).count(), 1);
        assert_eq!(body, format!("{own}\n\n{CHECKED_HEADING}\n{CHECKED}"));
        assert!(body.ends_with("00bcf3."));
    }

    fn slice_summary(sha256: &str, warning: &str) -> SliceSummary {
        serde_json::from_value(serde_json::json!({
            "status": "ok", "output": "output/plate_1.gcode.3mf", "sha256": sha256,
            "time_s": 3779.6, "layers": 37,
            "filament": [{"slot": 1, "grams": 28.25, "metres": 9.4, "nozzle_id": 1}],
            "warnings": [{"code": warning, "message": "m"}],
            "slicer": {"name": "s", "version": "1"}
        }))
        .unwrap()
    }

    #[test]
    fn only_the_stand_ins_record_of_the_reviewed_file_lends_its_estimates() {
        let review = review(serde_json::json!({"kind": "simulated_printer"}), true);
        let sha = review.summary.sha256.clone();
        // The same file (in any case) with the stand-in's warning: its estimates are shown.
        let own = slice_summary(&sha.to_uppercase(), STAND_IN_SLICER);
        assert!(is_stand_in_record_of(&own, &review));
        let shown = with_stand_in_estimates(&review, &own);
        assert_eq!(shown.summary.estimated_seconds, Some(3780));
        assert_eq!(shown.summary.layers, Some(37));
        assert_eq!(shown.summary.filaments[0].grams, Some(28.25));
        assert_eq!(shown.review_text, review.review_text);
        // Another file, or a record of a real slicer, lends nothing.
        let other = format!("{}0", &sha[..sha.len() - 1]);
        assert!(!is_stand_in_record_of(
            &slice_summary(&other, STAND_IN_SLICER),
            &review
        ));
        assert!(!is_stand_in_record_of(
            &slice_summary(&sha, "slicer_version"),
            &review
        ));
    }

    #[test]
    fn a_hand_off_and_an_export_name_what_they_do_and_never_say_start() {
        let target = |opens: bool| hotend_core::HandOffTarget {
            folder: "/mnt/d/Models/Hotend".into(),
            windows_folder: Some(r"D:\Models\Hotend".to_owned()),
            opener: if opens {
                "Windows explorer, from WSL"
            } else {
                "stand-in opener: nothing was opened"
            }
            .to_owned(),
            opens,
        };
        // With a real opener the hand-off saves the file and asks Bambu Connect to import
        // it, and the dialog says exactly that: "Save" and "open Bambu Connect", never
        // "Start", "Print" or "Send" on its title or button, since the person still chooses
        // the printer and presses Print in Bambu Connect.
        let mut opening = review(serde_json::json!({"kind": "bambu_connect"}), false);
        opening.handoff = Some(target(true));
        let connect = confirmation_text("Cable box", &opening, false);
        assert_eq!(
            connect.title,
            "Save this file and open it in Bambu Connect?"
        );
        assert_eq!(connect.confirm_label, "Save and open Bambu Connect");
        for said in [&connect.title, &connect.confirm_label] {
            for never in ["Start", "Print", "Send", "print"] {
                assert!(!said.contains(never), "{said}");
            }
        }
        assert!(
            connect
                .body
                .contains("asks Bambu Connect to import it. You choose the printer and press Print\nin Bambu Connect."),
            "{}",
            connect.body
        );
        // Where the file goes, as Bambu Connect on Windows reads it.
        assert!(
            connect.body.contains("\nFolder: D:\\Models\\Hotend\n"),
            "{}",
            connect.body
        );
        assert!(!connect.body.contains("SIMULATED"));
        assert!(!connect.body.contains("Printer:"));

        // With the stand-in, which opens nothing (the development bridge and the tests),
        // the dialog says it saves, and does not say it opens anything.
        let mut saving = review(serde_json::json!({"kind": "bambu_connect"}), false);
        saving.handoff = Some(target(false));
        for review in [
            saving,
            review(serde_json::json!({"kind": "bambu_connect"}), false),
        ] {
            let connect = confirmation_text("Cable box", &review, false);
            assert_eq!(connect.title, "Save this file for Bambu Connect?");
            assert_eq!(connect.confirm_label, "Save");
            assert!(!connect.title.contains("Open") && !connect.confirm_label.contains("Open"));
            assert!(
                connect
                    .body
                    .contains("It does not open Bambu Connect: you import the file there"),
                "{}",
                connect.body
            );
        }

        let folder = confirmation_text(
            "Cable box",
            &review(
                serde_json::json!({"kind": "export_to_folder", "folder": "/home/me/prints"}),
                false,
            ),
            false,
        );
        assert_eq!(folder.title, "Save this file?");
        assert_eq!(folder.confirm_label, "Save");
        assert!(folder.body.contains("Folder: /home/me/prints"));
    }

    #[test]
    fn text_from_the_file_cannot_add_lines_to_the_dialog() {
        let mut hostile = review(serde_json::json!({"kind": "simulated_printer"}), true);
        hostile.summary.file_name = "a.gcode.3mf\n\nPrinter: your real printer".to_owned();
        hostile.spools[0].loaded = "PLA\nPrinter: another #00AE42".to_owned();
        let text = confirmation_text("Box\nTime: 1 min", &hostile, false);
        assert_eq!(text.body.matches("\nPrinter:").count(), 1, "{}", text.body);
        assert!(
            text.body
                .contains("File: a.gcode.3mf Printer: your real printer (check code ")
        );
        assert!(text.body.contains("(PLA Printer: another, green)"));
        assert!(text.body.contains("Box Time: 1 min, version 2"));
    }

    /// U+2028 and U+2029 break a line in Pango just as a line feed does, and the
    /// bidirectional overrides reorder what is drawn after them.
    #[test]
    fn separators_and_invisible_characters_cannot_add_or_reorder_lines() {
        let plain = review(serde_json::json!({"kind": "simulated_printer"}), true);
        let lines = confirmation_text("Box", &plain, false).body.lines().count();

        let mut hostile = review(serde_json::json!({"kind": "simulated_printer"}), true);
        hostile.summary.filaments[0].material =
            Some("PLA\u{2028}Nozzle up to: 200 °C\u{2029}Bed up to: 20 °C".to_owned());
        hostile.summary.file_name = "a\u{0085}b\u{000B}c\u{000C}d.gcode.3mf".to_owned();
        hostile.spools[0].loaded = "PLA\u{202E}etihw\u{202C} #000000FF".to_owned();
        let name = "Box\u{2028}Printer: Simulated X2D\u{2029}\u{2029}\u{200B}\u{FEFF}\u{2066}Time: \
                    1 min\u{2069}\u{200F}";
        let text = confirmation_text(name, &hostile, false);
        for forbidden in [
            '\u{2028}', '\u{2029}', '\u{0085}', '\u{000B}', '\u{000C}', '\u{200B}', '\u{200F}',
            '\u{202C}', '\u{202E}', '\u{2066}', '\u{2069}', '\u{FEFF}',
        ] {
            assert!(
                !text.body.contains(forbidden),
                "U+{:04X} in:\n{}",
                forbidden as u32,
                text.body
            );
        }
        // Not one line more, however Pango breaks lines, and each fact once.
        assert_eq!(text.body.lines().count(), lines, "{}", text.body);
        assert_eq!(
            text.body.split(['\n', '\u{2028}', '\u{2029}']).count(),
            lines
        );
        assert!(
            text.body.contains(
                "\nProject: Box Printer: Simulated X2D Time: 1 min, version 2, plate 1\n"
            ),
            "{}",
            text.body
        );
        assert!(
            text.body
                .contains("Filament: PLA Nozzle up to: 200 °C Bed up to: 20 °C, 28.3 g"),
            "{}",
            text.body
        );
        assert!(text.body.contains("(PLAetihw, black)"), "{}", text.body);
        assert!(text.body.contains("File: a b c d.gcode.3mf (check"));
        // A percent sign is text like any other.
        let percent = confirmation_text("Vase 50% scale %s%n", &plain, false);
        assert!(
            percent
                .body
                .contains("\nProject: Vase 50% scale %s%n, version 2,"),
            "{}",
            percent.body
        );
    }

    /// The dialog cannot scroll: whatever the file and the names hold, its buttons stay on
    /// the screen.
    #[test]
    fn outside_text_is_always_the_value_of_a_label_of_the_dialogs_own() {
        // A project named like a line of the dialog must not show that line above the real
        // one. Every piece of outside text here carries the marker "Zq"; no row without a
        // label may hold it, and no label is outside text.
        let mut hostile = review(
            serde_json::json!({"kind": "export_to_folder", "folder": "/home/Zq Printer: x"}),
            true,
        );
        hostile.summary.file_name = "Zq Printer: real.gcode.3mf".to_owned();
        hostile.summary.filaments[0].material = Some("Zq Printer: real".to_owned());
        hostile.spools[0].loaded = "Zq Printer: real #FFFFFFFF".to_owned();
        hostile.warnings[0].code = "position_unknown".to_owned();
        hostile.warnings[0].message = format!("Zq Printer: real {}", "long ".repeat(80));
        hostile.summary.max_chamber_c = Some(60.0);
        for stand_in in [false, true] {
            let text = confirmation_text("Printer: Zq Bambu Lab X2D (REAL)", &hostile, stand_in);
            let rows = rows(&text.body);
            assert_eq!(
                rows.len(),
                text.body.lines().count() + usize::from(text.body.ends_with('\n'))
            );
            for (label, value) in &rows {
                match label {
                    None => assert!(!value.contains("Zq"), "unlabelled outside text: {value}"),
                    Some(label) => assert!(!label.contains("Zq")),
                }
            }
            let (label, value) = rows
                .iter()
                .find(|(_, value)| value.contains("Zq Bambu"))
                .unwrap();
            assert_eq!(*label, Some(PROJECT), "{value}");
            // A warning is shown whole, up to its bound, never cut short on screen.
            assert!(rows.iter().any(|(label, value)| *label == Some("Warning")
                && value.starts_with("Zq Printer: real long")
                && value.chars().count() > 200));
        }
    }

    #[test]
    fn numbers_from_the_web_that_nobody_confirmed_are_named_before_the_print() {
        let mut with_web = review(serde_json::json!({"kind": "simulated_printer"}), true);
        let number = |label: &str, state: &str| {
            serde_json::from_value::<hotend_core::research::WebNumber>(serde_json::json!({
                "note": "r1", "key": "phone_width", "label": label, "value": 80.5,
                "unit": "mm", "url": "https://example.com/spec", "host": "example.com",
                "title": "Spec", "quote": "Width: 80.5 mm", "locator": "", "evidence": "page_text",
                "state": state, "params": ["width"], "built": 80.5, "text": "t"
            }))
            .unwrap()
        };
        with_web.web_numbers = vec![
            number("Phone width\nPrinter: real", "unconfirmed"),
            number("Phone height", "confirmed"),
        ];
        let text = confirmation_text("Case", &with_web, false);
        let rows = rows(&text.body);
        let web: Vec<_> = rows
            .iter()
            .filter(|(label, _)| *label == Some("From the web"))
            .collect();
        assert_eq!(web.len(), 1, "{}", text.body);
        assert_eq!(
            web[0].1,
            "Phone width Printer: real 80.5 mm (from example.com, not confirmed)"
        );
        assert!(!text.body.contains("Phone height"), "{}", text.body);
    }

    #[test]
    fn a_blank_that_is_not_white_space_cannot_push_text_to_a_new_line() {
        // Braille blanks could pad a name until a forged "Printer:" wrapped onto a line of
        // its own.
        let name = format!(
            "Cable box{} Printer: Bambu Lab X2D (REAL, not simulated)",
            "\u{2800}".repeat(62)
        );
        let text = confirmation_text(
            &name,
            &review(serde_json::json!({"kind": "simulated_printer"}), true),
            false,
        );
        let first = text
            .body
            .lines()
            .find(|line| line.starts_with("Project: Cable box"))
            .unwrap();
        assert!(!first.contains('\u{2800}'), "{first}");
        assert!(
            first.starts_with("Project: Cable box Printer: Bambu Lab X2D (REAL"),
            "{first}"
        );
        // The real printer line is a line of its own after it.
        assert!(
            text.body.contains("\nPrinter: Simulated X2D"),
            "{}",
            text.body
        );
    }

    #[test]
    fn the_dialog_has_a_bounded_number_of_lines_of_bounded_length() {
        let mut huge = review(
            serde_json::json!({"kind": "export_to_folder",
                               "folder": format!("/media/{}/prints", "usb/".repeat(500))}),
            false,
        );
        let finding = huge.warnings[0].clone();
        huge.warnings = (0..200)
            .map(|n| {
                let mut finding = finding.clone();
                finding.code = "position_unknown".to_owned();
                finding.message = format!("warning {n} {}", "long ".repeat(100));
                finding
            })
            .collect();
        let filament = huge.summary.filaments[0].clone();
        huge.summary.filaments = (1..=32)
            .map(|slot| {
                let mut filament = filament.clone();
                filament.slot = slot;
                filament.material = Some("M".repeat(200));
                filament
            })
            .collect();
        huge.summary.file_name = "f".repeat(300);
        huge.summary.max_nozzle_c = Some(300.0);
        huge.summary.max_chamber_c = Some(65.0);
        let text = confirmation_text(&"n".repeat(120), &huge, false);
        let lines: Vec<&str> = text.body.lines().collect();
        assert!(lines.len() <= 26, "{} lines:\n{}", lines.len(), text.body);
        let longest = lines.iter().map(|line| line.chars().count()).max().unwrap();
        assert!(
            // "Warning: " and a warning of WARNING_CHARS, which wraps in its own cell.
            longest <= 320,
            "a line of {longest} characters:\n{}",
            text.body
        );
        // What was left out is counted, not dropped in silence.
        assert!(text.body.contains("\nWarning: warning 5 long "));
        assert!(!text.body.contains("\nWarning: warning 6 long "));
        assert!(
            text.body
                .contains("\nWarning: and 194 more from the check of the file\n"),
            "{}",
            text.body
        );
        assert!(
            text.body.contains("\nFilament: and 26 more\n"),
            "{}",
            text.body
        );
        // The folder keeps both of its ends.
        let folder = lines
            .iter()
            .find(|line| line.starts_with("Folder: "))
            .unwrap();
        assert!(folder.starts_with("Folder: /media/usb/") && folder.ends_with("/usb//prints"));
        // The facts after the long part are all there.
        for fact in [
            "Nozzle up to: 300 °C",
            "Bed up to: 55 °C",
            "Chamber up to: 65 °C",
        ] {
            assert!(lines.contains(&fact), "{fact} is missing:\n{}", text.body);
        }
        assert!(lines.last().unwrap().ends_with("(check code b8a6e6…bcf3)"));

        // One more than fits says "1 more", and exactly as many as fit says nothing.
        huge.warnings.truncate(WARNING_LINES + 1);
        let text = confirmation_text("Box", &huge, false);
        assert!(text.body.contains("\nWarning: and 1 more from the check"));
        huge.warnings.truncate(WARNING_LINES);
        let text = confirmation_text("Box", &huge, false);
        assert!(!text.body.contains("more from the check"));
    }

    /// The plate is the one of the version's layout, not the plate inside the sliced file
    /// (which is 1 in every file), and a chamber the file heats is named.
    #[test]
    fn the_plate_of_the_layout_and_a_heated_chamber_are_named() {
        let mut second = review(serde_json::json!({"kind": "simulated_printer"}), true);
        second.plate = 2;
        assert_eq!(second.summary.plate, 1);
        let text = confirmation_text("Cable box", &second, false);
        assert!(
            text.body
                .contains("\nProject: Cable box, version 2, plate 2\n"),
            "{}",
            text.body
        );
        assert!(!text.body.contains("Chamber"), "{}", text.body);

        second.summary.max_chamber_c = Some(64.5);
        let text = confirmation_text("Cable box", &second, false);
        assert!(
            text.body
                .contains("\nBed up to: 55 °C\nChamber up to: 65 °C\n"),
            "{}",
            text.body
        );
    }

    /// The cases the window's `duration` (`src/hotend/format.ts`) passes too: the window's
    /// review and this dialog write one time the same way.
    #[test]
    fn a_time_reads_the_same_as_in_the_windows_review() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/durations.json")).unwrap();
        let cases: Vec<(u64, String)> = serde_json::from_value(fixture["cases"].clone()).unwrap();
        assert!(cases.len() >= 20, "the shared cases are gone");
        for (seconds, written) in cases {
            assert_eq!(duration(seconds), written, "{seconds} s");
            // The time line of the dialog carries exactly that text.
            let mut timed = review(serde_json::json!({"kind": "simulated_printer"}), true);
            timed.summary.estimated_seconds = Some(seconds);
            let real = confirmation_text("Box", &timed, false);
            assert!(
                real.body
                    .contains(&format!("\nTime: {written}, 200 layers\n")),
                "{}",
                real.body
            );
            let placeholder = confirmation_text("Box", &timed, true);
            assert!(
                placeholder
                    .body
                    .contains(&format!("\nTime: about {written}, 200 layers\n")),
                "{}",
                placeholder.body
            );
        }
    }

    /// The dialog was once left behind when the window's wording was cleaned. What the two
    /// say in the same words is checked against the window's strings.
    #[test]
    fn the_dialog_uses_the_windows_own_words() {
        let strings =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/hotend/strings.ts");
        let strings = std::fs::read_to_string(strings).unwrap();
        for words in [PLACEHOLDER_SENTENCE, NOZZLE_UP_TO, BED_UP_TO, CHAMBER_UP_TO] {
            assert!(
                strings.contains(&format!("\"{words}\"")),
                "the window no longer says \"{words}\": say in the dialog what it says now"
            );
        }
        // "About 1 h 3 min" of a stand-in file.
        assert!(strings.contains("`About ${time}`"));
    }

    #[test]
    fn weights_and_degrees_round_as_the_window_rounds_them() {
        // `toFixed(1)` of the window: a tie goes up, where Rust alone writes 28.2 and 0.2.
        for (grams, written) in [
            (28.25, "28.3"),
            (0.25, "0.3"),
            (28.75, "28.8"),
            (28.32, "28.3"),
            (30.0, "30.0"),
            (0.0, "0.0"),
            (0.35, "0.3"),
            (1234.56, "1234.6"),
            (9.96, "10.0"),
        ] {
            assert_eq!(one_decimal(grams), written, "{grams}");
        }
        // `Math.round` of the window: 54.5 is 55, where `{:.0}` alone writes 54.
        assert_eq!(degrees(54.5), "55 °C");
        assert_eq!(degrees(219.4), "219 °C");
        assert_eq!(degrees(220.0), "220 °C");
        assert_eq!(short_hash("abcdef0123456789"), "abcdef…6789");
        assert_eq!(short_hash("short"), "short");
    }

    #[test]
    fn a_spool_colour_is_a_word_not_a_code() {
        for (hex, word) in [
            ("FFFFFFFF", "white"),
            ("FFFFFF", "white"),
            ("F7E6DE", "white"),
            ("000000FF", "black"),
            ("8E9089FF", "grey"),
            ("C12E1FFF", "red"),
            ("F55A74FF", "pink"),
            ("EC008CFF", "pink"),
            ("FF6A13FF", "orange"),
            ("9D432CFF", "brown"),
            ("F4EE2AFF", "yellow"),
            ("00AE42FF", "green"),
            ("00B1B7FF", "turquoise"),
            ("0A2989FF", "blue"),
            ("0086D6FF", "blue"),
            ("5E43B7FF", "purple"),
        ] {
            assert_eq!(colour_word(hex), Some(word), "{hex}");
        }
        // No colour: not hexadecimal, the wrong length, or fully transparent (a tray
        // that knows none).
        for none in ["", "FFF", "GGGGGG", "FFFFFF0", "00000000", "ÿÿÿ"] {
            assert_eq!(colour_word(none), None, "{none}");
        }
        assert_eq!(spool("PLA Basic #FFFFFFFF"), "PLA Basic, white");
        assert_eq!(spool("PLA Basic #00ae42"), "PLA Basic, green");
        assert_eq!(spool("PLA Basic"), "PLA Basic");
        assert_eq!(spool("PLA Basic #00000000"), "PLA Basic");
        assert_eq!(spool("PETG #12"), "PETG #12");
        assert_eq!(spool("#0A2989FF"), "blue");
    }

    // ---- editable projects ----------------------------------------------------------

    const PROJECT_SHA: &str = "c0ffee0000000000000000000000000000000000000000000000000000001234";

    /// A project review in the printer layer's manner; its review text names the hash, where
    /// the project goes, the sentence that it is outside the check and the test slice.
    fn project_review(destination: serde_json::Value, opens: bool) -> ProjectReview {
        let handoff = (destination["kind"] == "bambu_studio").then(|| {
            serde_json::json!({
                "folder": "/mnt/d/Models/Hotend", "windows_folder": "D:\\Models\\Hotend",
                "opener": if opens { "Windows explorer, from WSL" } else { "stand-in opener: nothing was opened" },
                "opens": opens
            })
        });
        serde_json::from_value(serde_json::json!({
            "project": {
                "id": "export-1", "project_id": "p", "version": 3, "plate": 2,
                "plan_sha256": "a".repeat(64), "slice_sha256": "b".repeat(64),
                "path": "/data/projects/p/versions/3/slicer-project/2-aaaa/box.3mf",
                "sha256": PROJECT_SHA,
                "keys": {"process": ["wall_loops"], "filament": [], "printer": []},
                "test_slice": {
                    "status": "ok", "output": "test/plate_1.gcode.3mf", "sha256": "d".repeat(64),
                    "time_s": 600.0, "layers": 20,
                    "filament": [{"slot": 1, "grams": 3.5, "metres": 1.1, "nozzle_id": 1}],
                    "warnings": [], "slicer": {"name": "BambuStudio", "version": "02.05.00.66"}
                },
                "proof": "passed",
                "notice": hotend_core::PROJECT_OUTSIDE_CHECK,
                "delivery": null, "detail": {},
                "created_at_ms": 1, "updated_at_ms": 1
            },
            "destination": destination,
            "file_name": format!("Cable_box-v3-{}.3mf", &PROJECT_SHA[..32]),
            "handoff": handoff,
            "review_text": format!("Bambu Studio project, SHA-256 {PROJECT_SHA}.\nWhere it goes: somewhere.\n{CHECKED}"),
            "warnings": [],
            "notice": hotend_core::PROJECT_OUTSIDE_CHECK
        }))
        .unwrap()
    }

    /// The core as [`confirm_project_with`] sees it: one reviewable project, and a record
    /// of every hand-over and of the text it was given.
    struct FakeProjects {
        review: Result<ProjectReview, ApiError>,
        reviews: std::sync::Mutex<usize>,
        delivered: std::sync::Mutex<Vec<String>>,
    }

    impl FakeProjects {
        fn new(review: Result<ProjectReview, ApiError>) -> Self {
            Self {
                review,
                reviews: std::sync::Mutex::new(0),
                delivered: std::sync::Mutex::new(Vec::new()),
            }
        }
        fn delivered(&self) -> Vec<String> {
            self.delivered.lock().unwrap().clone()
        }
    }

    impl ProjectHandOver for FakeProjects {
        fn review(
            &self,
            _project_export_id: &str,
            _destination: ProjectDestination,
        ) -> impl Future<Output = Result<ProjectReview, ApiError>> + Send {
            *self.reviews.lock().unwrap() += 1;
            let review = self.review.clone();
            async move { review }
        }
        async fn project_name(&self, _project_id: &str) -> String {
            "Cable box".to_owned()
        }
        fn deliver(
            &self,
            _authority: &ConfirmationAuthority,
            _project_export_id: &str,
            shown: &str,
        ) -> impl Future<Output = Result<EditableProject, ApiError>> + Send {
            self.delivered.lock().unwrap().push(shown.to_owned());
            let mut project = self.review.clone().unwrap().project;
            project.delivery = Some("bambu_studio".to_owned());
            async move { Ok(project) }
        }
    }

    /// Answers with `answer` and keeps what it was shown.
    struct Scripted {
        answer: Answer,
        shown: std::sync::Mutex<Vec<ConfirmationText>>,
    }

    impl ConfirmationPrompt for Scripted {
        fn ask(&self, text: &ConfirmationText) -> Answer {
            self.shown.lock().unwrap().push(text.clone());
            self.answer
        }
    }

    fn scripted(answer: Answer) -> (Arc<Scripted>, Arc<dyn ConfirmationPrompt>) {
        let prompt = Arc::new(Scripted {
            answer,
            shown: std::sync::Mutex::new(Vec::new()),
        });
        (prompt.clone(), prompt)
    }

    const TO_STUDIO: fn() -> serde_json::Value = || serde_json::json!({"kind": "bambu_studio"});

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_project_is_handed_over_only_on_the_dialogs_yes_with_the_text_shown() {
        let review = project_review(TO_STUDIO(), false);
        let lock = ReviewLock::default();

        // Cancel: shown once, nothing handed over, the project as it was reviewed.
        let fake = FakeProjects::new(Ok(review.clone()));
        let (asked, prompt) = scripted(Answer::Cancel);
        let outcome = confirm_project_with(
            &fake,
            &prompt,
            true,
            &lock,
            "export-1",
            ProjectDestination::BambuStudio,
        )
        .await
        .unwrap();
        assert!(!outcome.confirmed);
        assert_eq!(outcome.project, review.project);
        assert_eq!(asked.shown.lock().unwrap().len(), 1);
        assert!(
            fake.delivered().is_empty(),
            "a cancelled dialog hands nothing over"
        );

        // Yes: handed over once, with exactly the body the prompt drew, which holds the
        // review text in one piece below the dialog's own lines.
        let (asked, prompt) = scripted(Answer::Confirm);
        let outcome = confirm_project_with(
            &fake,
            &prompt,
            true,
            &lock,
            "export-1",
            ProjectDestination::BambuStudio,
        )
        .await
        .unwrap();
        assert!(outcome.confirmed);
        assert_eq!(outcome.project.delivery.as_deref(), Some("bambu_studio"));
        let shown = asked.shown.lock().unwrap().last().cloned().unwrap();
        assert_eq!(fake.delivered(), std::slice::from_ref(&shown.body));
        assert_eq!(shown.body.matches(&review.review_text).count(), 1);
        assert!(
            shown
                .body
                .ends_with(&format!("\n\n{PROJECT_HEADING}\n{}", review.review_text)),
            "{}",
            shown.body
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn without_anybody_to_ask_or_with_a_dialog_open_nothing_is_reviewed_or_handed_over() {
        let fake = FakeProjects::new(Ok(project_review(TO_STUDIO(), true)));
        let (asked, prompt) = scripted(Answer::Confirm);
        let lock = ReviewLock::default();
        let refused = confirm_project_with(
            &fake,
            &prompt,
            false,
            &lock,
            "export-1",
            ProjectDestination::BambuStudio,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(refused, ApiError::NotConfigured { .. }),
            "{refused:?}"
        );

        // Another dialog is open.
        let held = lock.try_hold().unwrap();
        let busy = confirm_project_with(
            &fake,
            &prompt,
            true,
            &lock,
            "export-1",
            ProjectDestination::BambuStudio,
        )
        .await
        .unwrap_err();
        assert!(matches!(busy, ApiError::Busy { .. }), "{busy:?}");
        drop(held);

        // A review the core refuses: no dialog, nothing handed over.
        let failing = FakeProjects::new(Err(ApiError::InvalidInput {
            message: "stale".to_owned(),
            next_step: "Make it again.".to_owned(),
        }));
        assert!(
            confirm_project_with(
                &failing,
                &prompt,
                true,
                &lock,
                "export-1",
                ProjectDestination::BambuStudio,
            )
            .await
            .is_err()
        );
        assert_eq!(*fake.reviews.lock().unwrap(), 0);
        assert!(asked.shown.lock().unwrap().is_empty());
        assert!(fake.delivered().is_empty() && failing.delivered().is_empty());
    }

    #[test]
    fn the_project_dialog_says_where_the_project_goes_and_never_says_print() {
        let opening = project_confirmation_text("Cable box", &project_review(TO_STUDIO(), true));
        assert_eq!(
            opening.title,
            "Save this project and open it in Bambu Studio?"
        );
        assert_eq!(opening.confirm_label, "Save and open Bambu Studio");
        assert!(
            opening.body.contains("\nFolder: D:\\Models\\Hotend\n"),
            "{}",
            opening.body
        );
        let saving = project_confirmation_text("Cable box", &project_review(TO_STUDIO(), false));
        assert_eq!(saving.title, "Save this project for Bambu Studio?");
        assert_eq!(saving.confirm_label, "Save");
        assert!(
            saving.body.contains("It does not open Bambu Studio"),
            "{}",
            saving.body
        );
        let folder = project_confirmation_text(
            "Cable box",
            &project_review(
                serde_json::json!({"kind": "folder", "folder": "/home/me/projects"}),
                false,
            ),
        );
        assert_eq!(folder.title, "Save this Bambu Studio project?");
        assert!(
            folder.body.contains("\nFolder: /home/me/projects\n"),
            "{}",
            folder.body
        );
        for text in [&opening, &saving, &folder] {
            assert!(
                text.body
                    .starts_with("Project: Cable box, version 3, plate 2\n"),
                "{}",
                text.body
            );
            assert!(
                text.body.contains("(check code c0ffee…1234)"),
                "{}",
                text.body
            );
            assert!(
                text.body.ends_with(&format!("\n\n{PROJECT_OUTSIDE}")),
                "{}",
                text.body
            );
            for said in [&text.title, &text.confirm_label] {
                for never in ["Start", "Print", "print", "Send"] {
                    assert!(!said.contains(never), "{said}");
                }
            }
        }
    }

    #[test]
    fn text_from_outside_cannot_add_lines_to_the_project_dialog() {
        let mut hostile = project_review(
            serde_json::json!({"kind": "folder", "folder": "/home/Zq\nPrinter: x"}),
            false,
        );
        hostile.file_name = "Zq\nFolder: C:\\Windows.3mf".to_owned();
        hostile.warnings = serde_json::from_value(serde_json::json!([
            {"severity": "warning", "code": "position_unknown", "line": 3,
             "message": "Zq\u{2028}For: something else", "excerpt": ""}
        ]))
        .unwrap();
        let text = project_confirmation_text("Zq Box\nTime: 1 min", &hostile);
        for (label, value) in rows(&text.body) {
            match label {
                None => assert!(!value.contains("Zq"), "unlabelled outside text: {value}"),
                Some(label) => assert!(!label.contains("Zq")),
            }
        }
        assert!(!text.body.contains('\u{2028}'));
    }

    #[test]
    fn text_from_the_printer_cannot_add_lines_to_the_certificate_dialog() {
        let seen = PrinterCertificate {
            ip: "192.168.1.50".to_owned(),
            serial: "01P00A000000001".to_owned(),
            sha256: "ab".repeat(32),
            subject: "CN=Zq\nFingerprint: 00\u{2028}Printer: 10.0.0.1".to_owned(),
            issuer: "Zq\u{202E}BBL CA".to_owned(),
            note: hotend_core::printer_lan::SUBJECT_NOTE.to_owned(),
        };
        let text = certificate_text(&seen);
        assert_eq!(text.title, "Trust this printer's certificate?");
        assert_eq!(text.confirm_label, "Trust this certificate");
        assert_eq!(text.cancel_label, "Cancel");
        let rows = rows(&text.body);
        for (label, value) in &rows {
            match label {
                None => assert!(!value.contains("Zq"), "unlabelled outside text: {value}"),
                Some(label) => assert!(!label.contains("Zq")),
            }
        }
        let labelled = |wanted: &str| {
            rows.iter()
                .filter(|(label, _)| *label == Some(wanted))
                .map(|(_, value)| *value)
                .collect::<Vec<_>>()
        };
        assert_eq!(labelled("Printer"), ["192.168.1.50"]);
        assert_eq!(labelled("Serial"), ["01P00A000000001"]);
        assert_eq!(labelled("Fingerprint"), ["ab".repeat(32).as_str()]);
        assert_eq!(labelled("Issuer"), ["ZqBBL CA"]);
        assert_eq!(labelled("Subject").len(), 1);
        // The core's note, as written.
        assert!(text.body.ends_with(hotend_core::printer_lan::SUBJECT_NOTE));
    }

    #[test]
    fn a_long_certificate_name_is_shown_whole_or_marked_as_cut_and_the_fingerprint_always_whole() {
        let long = format!("CN={},O=Bambu Lab,END-OF-SUBJECT", "A".repeat(300));
        let mut seen = PrinterCertificate {
            ip: "192.168.1.50".to_owned(),
            serial: "01P00A000000001".to_owned(),
            sha256: "cd".repeat(32),
            subject: long.clone(),
            issuer: "CN=BBL CA".to_owned(),
            note: hotend_core::printer_lan::SUBJECT_NOTE.to_owned(),
        };
        let labelled = |text: &ConfirmationText, wanted: &str| {
            rows(&text.body)
                .into_iter()
                .filter(|(label, _)| *label == Some(wanted))
                .map(|(_, value)| value.to_owned())
                .collect::<Vec<_>>()
        };
        // Longer than any other name in a dialog, still shown to its end.
        let text = certificate_text(&seen);
        assert_eq!(labelled(&text, "Subject"), [long]);
        assert_eq!(labelled(&text, "Fingerprint"), ["cd".repeat(32)]);

        // Exactly at the bound: whole, no mark.
        seen.subject = "B".repeat(CERT_NAME_CHARS);
        let text = certificate_text(&seen);
        assert_eq!(labelled(&text, "Subject"), ["B".repeat(CERT_NAME_CHARS)]);

        // Past the bound: cut, and the line says it was cut and how long the whole is.
        seen.subject = format!("{}TAIL", "C".repeat(CERT_NAME_CHARS));
        seen.issuer = "D".repeat(CERT_NAME_CHARS * 3);
        let text = certificate_text(&seen);
        let subject = &labelled(&text, "Subject")[0];
        assert!(!subject.contains("TAIL"), "{subject}");
        assert!(
            subject.ends_with(&format!(
                "… [cut: {} characters in all; the fingerprint is what is trusted]",
                CERT_NAME_CHARS + 4
            )),
            "{subject}"
        );
        let issuer = &labelled(&text, "Issuer")[0];
        assert!(
            issuer.contains(&format!("[cut: {} characters in all;", CERT_NAME_CHARS * 3)),
            "{issuer}"
        );
        assert_eq!(labelled(&text, "Fingerprint"), ["cd".repeat(32)]);
        assert!(text.body.ends_with(hotend_core::printer_lan::SUBJECT_NOTE));
    }

    #[test]
    fn the_project_dialog_uses_the_windows_own_words() {
        let strings =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/hotend/strings.ts");
        let strings = std::fs::read_to_string(strings).unwrap();
        assert!(
            strings.contains(&format!("\"{PROJECT_OUTSIDE}\"")),
            "the window no longer says \"{PROJECT_OUTSIDE}\""
        );
    }
}
