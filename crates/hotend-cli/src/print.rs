//! The printer commands: list printers, propose a job, review it, confirm it, follow it.
//!
//! Guarantee: a job is confirmed only by a person at a terminal who was shown the review.
//! [`ask`] refuses when standard input or standard output is not a terminal, discards
//! whatever was typed before it wrote its question, and the process's confirmation
//! authority is taken only after the person then typed `yes`. No flag and no environment
//! variable confirms a print; the commands that run headless (`propose`, `review`,
//! `status`, `cancel`, and `project` without `--out`) cannot deliver anything. Saving a copy
//! of an editable project (`project --out`) is asked the same way, and nothing here opens a
//! project or a link.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::time::Duration;

use hotend_core::{
    ConfirmationAuthority, Core, CoreEvent, DeliveryChoice, EditableProject, HostKind, PrintEvent,
    PrintJob, PrintReview, PrinterOptions, ProjectDestination, ProjectReview,
};

use crate::error::CliError;
use crate::{DeliveryArg, PrintCommand, PrintTarget, app, render, research, table};

fn print_json(value: &impl serde::Serialize) -> Result<(), CliError> {
    let text = serde_json::to_string_pretty(value).map_err(|e| CliError::failed(e.to_string()))?;
    println!("{text}");
    Ok(())
}

/// Refuses when nobody can be asked or nobody would see what is asked: a confirmation
/// needs a person at a terminal who reads the review on that terminal.
pub fn require_terminal() -> Result<(), CliError> {
    terminals(
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
    )
}

fn terminals(input: bool, output: bool) -> Result<(), CliError> {
    if !input {
        return Err(CliError::setup(
            "a print is confirmed by a person at a terminal, and standard input is not one: \
             nothing was confirmed and nothing was sent. No flag or environment variable can \
             confirm a print; run this command in a terminal and type `yes`",
        ));
    }
    if !output {
        return Err(CliError::setup(
            "a print is confirmed by a person at a terminal who has read what will be \
             printed, and standard output is not a terminal: the review would not be seen. \
             Nothing was confirmed and nothing was sent; run this command without \
             redirecting its output",
        ));
    }
    Ok(())
}

/// Throws away what was typed into the terminal before now, so that only an answer typed
/// after the question counts.
#[cfg(unix)]
pub(crate) fn discard_typed_ahead() -> std::io::Result<()> {
    // SAFETY: `tcflush` takes a file descriptor and a queue selector and touches no
    // memory; standard input stays open for the life of the process.
    if unsafe { libc::tcflush(libc::STDIN_FILENO, libc::TCIFLUSH) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(windows)]
pub(crate) fn discard_typed_ahead() -> std::io::Result<()> {
    use windows_sys::Win32::System::Console::{
        FlushConsoleInputBuffer, GetStdHandle, STD_INPUT_HANDLE,
    };
    // SAFETY: both calls take plain values; the handle is the process's own console
    // input and is not closed here.
    let flushed = unsafe { FlushConsoleInputBuffer(GetStdHandle(STD_INPUT_HANDLE)) };
    if flushed != 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn discard_typed_ahead() -> std::io::Result<()> {
    Err(std::io::Error::other(
        "this platform has no way to discard typed-ahead input",
    ))
}

/// Asks the person at the terminal whether to go ahead. The authority exists only after a
/// `yes` typed on a terminal after the question was written there; any other answer is
/// `None`, and so is anything typed before the question.
pub fn ask(question: &str) -> Result<Option<ConfirmationAuthority>, CliError> {
    ask_or(question, "anything else to leave the job pending")
}

/// [`ask`] with `otherwise` saying what any answer but `yes` does.
fn ask_or(question: &str, otherwise: &str) -> Result<Option<ConfirmationAuthority>, CliError> {
    require_terminal()?;
    // The review above this question must be on the terminal before input counts.
    std::io::stdout().flush()?;
    discard_typed_ahead().map_err(|error| {
        CliError::setup(format!(
            "what was typed before the question could not be discarded ({error}), so an \
             answer could not be told from it: nothing was confirmed and nothing was sent"
        ))
    })?;
    print!("{question} Type `yes` to confirm, {otherwise}: ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    if answer.trim() != "yes" {
        return Ok(None);
    }
    ConfirmationAuthority::take().map(Some).ok_or_else(|| {
        CliError::failed("this process already used its confirmation; run the command again")
    })
}

/// The line that opens the checked review text in what the terminal shows.
const CHECKED_HEADING: &str =
    "What the G-code check read from the sliced file (this is what you confirm):";

/// Everything the terminal shows about a job before it asks: where the job would go, the
/// spools, the review text of the checked file, and the check's warnings.
///
/// The review text ([`PrintReview::review_text`]) is the printer layer's, written from the
/// checked file alone. It is shown as it is, in one piece and flush left, because the
/// confirmation is given for that text: [`confirm_reviewed`] hands the text returned
/// here to the core, which looks for the review text in it word for word and refuses the
/// confirmation when it is not there.
pub fn shown_text(review: &PrintReview) -> String {
    let summary = &review.summary;
    let mut out = String::new();
    let mut line = |text: String| {
        out.push_str(&render::clean(&text));
        out.push('\n');
    };
    line(format!(
        "print job {} (version {})",
        review.job.id, review.job.version
    ));
    match (&review.delivery, &review.printer) {
        (DeliveryChoice::SimulatedPrinter, Some(printer)) => line(format!(
            "to: {} (serial {}) - SIMULATED: nothing will be printed",
            printer.name, printer.serial
        )),
        (DeliveryChoice::BambuConnect, _) => {
            line(
                "to: Bambu Connect (the checked file is saved to the hand-off folder and the \
                 import link is printed; the command line opens nothing. You choose the \
                 printer and press Print in Bambu Connect)"
                    .to_owned(),
            );
            if let Some(target) = &review.handoff {
                line(format!("hand-off folder: {}", target.folder.display()));
                if let Some(windows) = &target.windows_folder {
                    line(format!("as Windows sees it: {windows}"));
                }
            }
        }
        (DeliveryChoice::ExportToFolder { folder }, _) => {
            line(format!("to: the folder {}", folder.display()));
        }
        _ => {}
    }
    line(format!(
        "file: {} ({} bytes)",
        summary.file_name, summary.size_bytes
    ));
    // The plate of the layout the file was sliced from, and the pieces on it.
    line(format!(
        "plate {}{}",
        review.plate,
        if review.pieces.is_empty() {
            String::new()
        } else {
            format!(" ({})", review.pieces.join(", "))
        },
    ));
    for spool in &review.spools {
        line(format!(
            "filament {} ({}, nozzle {}) <- AMS {} tray {} ({})",
            spool.slot, spool.material, spool.nozzle_id, spool.ams_unit, spool.tray, spool.loaded
        ));
    }
    line(String::new());
    line(CHECKED_HEADING.to_owned());
    // Not cleaned and not indented: the text as the printer layer wrote it.
    out.push_str(&review.review_text);
    out.push('\n');
    let mut line = |text: String| {
        out.push_str(&render::clean(&text));
        out.push('\n');
    };
    line(String::new());
    line(format!(
        "G-code check: passed, {} warning(s)",
        review.warnings.len()
    ));
    for warning in review.warnings.iter().take(12) {
        line(format!(
            "- {} (line {}): {}",
            warning.code, warning.line, warning.message
        ));
    }
    // Sizes of this version that come from a web page nobody has confirmed. They do not
    // stop the print; the person decides knowing them.
    if !review.web_numbers.is_empty() {
        line(format!(
            "numbers from the web that are not confirmed ({}): check them against the real \
             thing before you print",
            review.web_numbers.len()
        ));
    }
    for number in &review.web_numbers {
        line(format!("- {}", number.text.replace('\n', " ")));
        for detail in research::web_number_lines(number, "  ").lines() {
            line(detail.to_owned());
        }
    }
    out
}

/// Everything the terminal shows about an editable project before it asks: where the copy
/// would go, its name, and the printer layer's review text word for word, which names the
/// project's hash, says that the project is outside Hotend's check, and then reads the
/// test slice Bambu Studio's command line made of it.
pub fn project_shown_text(review: &ProjectReview) -> String {
    let mut out = String::new();
    let mut line = |text: String| {
        out.push_str(&render::clean(&text));
        out.push('\n');
    };
    line(format!(
        "editable Bambu Studio project for plate {} of version {}",
        review.project.plate, review.project.version
    ));
    if let ProjectDestination::Folder { folder } = &review.destination {
        line(format!(
            "to: the folder {} (the command line opens nothing)",
            folder.display()
        ));
    }
    line(format!("file: {}", review.file_name));
    line(String::new());
    line(PROJECT_HEADING.to_owned());
    // Not cleaned and not indented: the text as the printer layer wrote it.
    out.push_str(&review.review_text);
    out.push('\n');
    let mut line = |text: String| {
        out.push_str(&render::clean(&text));
        out.push('\n');
    };
    line(String::new());
    line(format!(
        "G-code check of that test slice: passed, {} warning(s)",
        review.warnings.len()
    ));
    for warning in review.warnings.iter().take(12) {
        line(format!(
            "- {} (line {}): {}",
            warning.code, warning.line, warning.message
        ));
    }
    line(review.notice.clone());
    out
}

/// The line that opens the project's review text in what the terminal shows.
const PROJECT_HEADING: &str = "What you confirm:";

/// What the terminal says about a project that was made.
fn project_lines(project: &EditableProject) -> String {
    let mut out = String::new();
    let mut line = |text: String| {
        out.push_str(&render::clean(&text));
        out.push('\n');
    };
    line(format!(
        "editable Bambu Studio project for plate {} of version {}: {}",
        project.plate,
        project.version,
        project.path.display()
    ));
    line(format!("SHA-256: {}", project.sha256));
    line(format!(
        "settings changed from Bambu Studio's presets: process {}; filament {}",
        list_or_none(&project.keys.process),
        list_or_none(&project.keys.filament)
    ));
    let slice = &project.test_slice;
    line(format!(
        "Bambu Studio's command line sliced the project once on its own: {} layers, {:.1} g, \
         about {} min; the proof of Hotend's print settings passed on that test slice",
        slice.layers,
        slice.filament.iter().map(|use_| use_.grams).sum::<f64>(),
        (slice.time_s / 60.0).round()
    ));
    line(project.notice.clone());
    out
}

fn list_or_none(keys: &[String]) -> String {
    if keys.is_empty() {
        "none".to_owned()
    } else {
        keys.join(", ")
    }
}

fn job_line(job: &PrintJob) -> String {
    let mut text = format!("print job {}: {}", job.id, job.status);
    if let Some(delivery) = &job.delivery {
        text.push_str(&format!(" ({delivery})"));
    }
    if job.detail.simulated {
        text.push_str(" [simulated printer]");
    }
    if let Some(message) = &job.detail.message {
        text.push_str(&format!(" - {}", render::clean(message)));
    }
    text
}

/// Prints a job's events until it leaves `starting` and `printing`, and returns it.
pub async fn follow(
    core: &Core,
    events: &mut tokio::sync::broadcast::Receiver<CoreEvent>,
    job_id: &str,
) -> Result<PrintJob, CliError> {
    let mut last_percent = None;
    // The caller printed the job as it was confirmed.
    let mut shown = "starting".to_owned();
    loop {
        let event = match tokio::time::timeout(Duration::from_secs(600), events.recv()).await {
            Ok(Ok(event)) => event,
            // A slow reader loses old events; the stored job is always current.
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(_)) | Err(_) => return Ok(core.print_status(job_id).await?),
        };
        let CoreEvent::Print {
            job_id: id, event, ..
        } = event
        else {
            continue;
        };
        if id != job_id {
            continue;
        }
        match event {
            PrintEvent::Status { job } => {
                if job.status != shown {
                    println!("  [simulated] {}", job_line(&job));
                    shown.clone_from(&job.status);
                }
                if !["starting", "printing"].contains(&job.status.as_str()) {
                    return Ok(*job);
                }
            }
            PrintEvent::Phase { headline, .. } => {
                println!("  [simulated] {}", render::clean(&headline));
            }
            PrintEvent::Progress {
                percent,
                remaining_minutes,
            } => {
                // One line per ten percent keeps the transcript short.
                if last_percent.is_none_or(|last: u8| percent / 10 != last / 10) || percent == 100 {
                    let left = remaining_minutes
                        .map(|minutes| format!(", {minutes} min left on the simulated clock"))
                        .unwrap_or_default();
                    println!("  [simulated] {percent} %{left}");
                }
                last_percent = Some(percent);
            }
            PrintEvent::Problem { problem } => println!(
                "  [simulated] problem {}: {}",
                render::clean(&problem.raw_code),
                render::clean(&problem.message)
            ),
        }
    }
}

fn delivery(to: DeliveryArg, out: Option<PathBuf>) -> Result<DeliveryChoice, CliError> {
    match (to, out) {
        (DeliveryArg::Simulated, None) => Ok(DeliveryChoice::SimulatedPrinter),
        (DeliveryArg::BambuConnect, None) => Ok(DeliveryChoice::BambuConnect),
        (DeliveryArg::Folder, Some(folder)) => Ok(DeliveryChoice::ExportToFolder { folder }),
        (DeliveryArg::Folder, None) => Err(CliError::setup(
            "`--to folder` needs `--out <DIR>`, the folder to write the sliced file into",
        )),
        (_, Some(_)) => Err(CliError::setup("`--out` goes with `--to folder` only")),
    }
}

/// Asks the person about a job whose review was just shown, delivers it after a `yes`
/// and, on the simulated printer, follows it to its end. Returns the job as it ended, or
/// `None` when the person did not confirm: the job is then still pending.
///
/// `shown` is the text that was written to the terminal for this job ([`shown_text`]),
/// the very string that was printed. The core confirms only when it contains the review
/// text of the checked file.
pub async fn confirm_reviewed(
    core: &Core,
    review: &PrintReview,
    shown: &str,
) -> Result<Option<PrintJob>, CliError> {
    let question = if review.simulated {
        "Run this job on the SIMULATED printer? Nothing will be printed."
    } else {
        "Deliver this job?"
    };
    let Some(authority) = ask(question)? else {
        return Ok(None);
    };
    let job_id = review.job.id.as_str();
    let mut events = core.events();
    let job = core.print_confirm(&authority, job_id, shown).await?;
    println!("{}", job_line(&job));
    if let Some(link) = &job.detail.link {
        println!(
            "Bambu Connect import link (the command line does not open it: open it yourself, \
             or import the file in Bambu Connect):\n  {link}"
        );
    }
    if let Some(path) = &job.detail.path {
        println!("file: {}", path.display());
    }
    if let Some(windows) = &job.detail.windows_path {
        println!("file as Windows sees it: {windows}");
    }
    if job.status == "starting" {
        return follow(core, &mut events, job_id).await.map(Some);
    }
    Ok(Some(job))
}

pub async fn printers(json: bool, data_dir: Option<PathBuf>) -> Result<(), CliError> {
    let core = app::open_core(data_dir, hotend_core::SecretBackend::from_env()).await?;
    core.set_handoff_host(HostKind::detect());
    let printers = core.printers_list().await?;
    if json {
        return print_json(&printers);
    }
    for printer in &printers {
        println!(
            "{}  {} ({}), serial {}{}",
            printer.printer.id,
            render::clean(&printer.printer.name),
            printer.printer.model,
            printer.printer.serial,
            if printer.printer.simulated {
                "  [SIMULATED: a model of a printer inside Hotend, nothing is printed]"
            } else {
                ""
            }
        );
        println!("  state: {}", render::clean(&printer.state.headline));
        for unit in &printer.state.ams {
            let trays: Vec<String> = unit
                .trays
                .iter()
                .map(|tray| {
                    format!(
                        "{}: {}",
                        tray.id,
                        tray.product
                            .as_deref()
                            .or(tray.material.as_deref())
                            .unwrap_or("empty")
                    )
                })
                .collect();
            println!(
                "  AMS {}{} on nozzle {}: {}",
                unit.id,
                if unit.high_temperature { " (HT)" } else { "" },
                unit.nozzle_id
                    .map_or_else(|| "?".to_owned(), |id| id.to_string()),
                render::clean(&trays.join(", "))
            );
        }
        println!("\n  ways to deliver a confirmed job:");
        let rows: Vec<Vec<String>> = printer
            .deliveries
            .iter()
            .map(|d| {
                vec![
                    d.kind.clone(),
                    if d.enabled { "yes" } else { "no" }.to_owned(),
                    d.disabled_reason.clone().unwrap_or_else(|| d.cost.clone()),
                ]
            })
            .collect();
        print!(
            "{}",
            table::render_wrapped(&["delivery", "enabled", "what it means"], &rows, 100)
        );
    }
    Ok(())
}

pub async fn print(command: PrintCommand, data_dir: Option<PathBuf>) -> Result<(), CliError> {
    match command {
        PrintCommand::Propose {
            project,
            note,
            json,
        } => {
            let core = app::open_core(data_dir, hotend_core::SecretBackend::from_env()).await?;
            let project = app::find_project(&core, project.project.as_deref()).await?;
            // The core proposes only where slicing is set up. No slicer runs here: the job
            // refers to the file a slice already made.
            app::install_slicer(&core, false)?;
            let job = core.print_propose(&project.id, &note).await?;
            if json {
                return print_json(&job);
            }
            println!("{}", job_line(&job));
            println!(
                "It waits for your review: `hotend print review {0}` shows what it would \
                 print, `hotend print confirm {0}` delivers it after you typed yes.",
                job.id
            );
            Ok(())
        }
        PrintCommand::Status { job, project, json } => {
            let core = app::open_core(data_dir, hotend_core::SecretBackend::from_env()).await?;
            let jobs = match job {
                Some(id) => vec![core.print_status(&id).await?],
                None => {
                    let project = app::find_project(&core, project.project.as_deref()).await?;
                    core.print_jobs(&project.id).await?
                }
            };
            if json {
                return print_json(&jobs);
            }
            if jobs.is_empty() {
                println!("no print jobs");
            }
            for job in &jobs {
                println!("{}", job_line(job));
            }
            Ok(())
        }
        PrintCommand::Review { target, json } => {
            let PrintTarget { job, to, out, .. } = target;
            let core = app::open_core(data_dir, hotend_core::SecretBackend::from_env()).await?;
            core.set_handoff_host(HostKind::detect());
            let review = core.print_review(&job, delivery(to, out)?).await?;
            if json {
                return print_json(&review);
            }
            print!("{}", shown_text(&review));
            println!("Nothing was confirmed or sent. `hotend print confirm {job}` asks you.");
            Ok(())
        }
        PrintCommand::Confirm { target } => {
            let PrintTarget {
                job,
                to,
                out,
                time_scale,
            } = target;
            let choice = delivery(to, out)?;
            let printer = PrinterOptions {
                time_scale,
                ..PrinterOptions::default()
            };
            // Asked before anything is opened or read: without a terminal there is nobody
            // to show the review to.
            require_terminal()?;
            let core = app::open_core_with_printer(data_dir, printer).await?;
            // The hand-off path is made for this host; the opener stays the core's stand-in,
            // so nothing is opened from the command line.
            core.set_handoff_host(HostKind::detect());
            let review = core.print_review(&job, choice).await?;
            let shown = shown_text(&review);
            print!("{shown}");
            let Some(ended) = confirm_reviewed(&core, &review, &shown).await? else {
                return Err(CliError::failed(
                    "not confirmed; the job is still pending and nothing was sent",
                ));
            };
            match ended.status.as_str() {
                "finished" | "exported" | "handed_off" => Ok(()),
                _ => Err(CliError::Reported),
            }
        }
        PrintCommand::Cancel { job } => {
            let core = app::open_core(data_dir, hotend_core::SecretBackend::from_env()).await?;
            println!("{}", job_line(&core.print_cancel(&job).await?));
            Ok(())
        }
        PrintCommand::Project {
            project,
            plate,
            out,
            fake,
            json,
        } => {
            // Saving a copy is confirmed by a person at a terminal: asked before anything is
            // opened, made or read.
            if out.is_some() {
                require_terminal()?;
            }
            let core = app::open_core(data_dir, hotend_core::SecretBackend::from_env()).await?;
            let project = app::find_project(&core, project.project.as_deref()).await?;
            app::install_slicer(&core, fake)?;
            let made = core.print_project_make(&project.id, plate).await?;
            let Some(folder) = out else {
                if json {
                    return print_json(&made);
                }
                print!("{}", project_lines(&made));
                println!(
                    "Nothing was opened or sent. `hotend print project --out <DIR>` saves a copy \
                     into a folder after you typed yes."
                );
                return Ok(());
            };
            let review = core
                .print_project_review(&made.id, ProjectDestination::Folder { folder })
                .await?;
            let shown = project_shown_text(&review);
            print!("{shown}");
            let Some(authority) = ask_or(
                "Save this project into the folder?",
                "anything else to save nothing",
            )?
            else {
                return Err(CliError::failed(
                    "not confirmed; nothing was saved and nothing was opened",
                ));
            };
            let saved = core
                .print_project_deliver(&authority, &made.id, &shown)
                .await?;
            if let Some(path) = &saved.detail.path {
                println!("saved: {}", path.display());
            }
            if let Some(message) = &saved.detail.message {
                println!("{}", render::clean(message));
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn review(review_text: &str) -> PrintReview {
        serde_json::from_value(serde_json::json!({
            "job": {
                "id": "job-1", "project_id": "p", "version": 2,
                "slice_sha256": "ab", "note": "", "status": "pending", "delivery": null,
                "created_at_ms": 1, "updated_at_ms": 1, "detail": {}
            },
            "delivery": {"kind": "simulated_printer"},
            "simulated": true,
            "plate": 2,
            "pieces": ["box", "lid"],
            "printer": {"id": "simulated-x2d", "name": "Simulated X2D (no real printer)",
                        "model": "X2D", "serial": "20P0SIM00000001", "simulated": true},
            "summary": {
                "file_name": "plate_1.gcode.3mf", "sha256": "ab", "size_bytes": 2084,
                "plate": 1, "estimated_seconds": 0, "layers": null, "filaments": [],
                "max_nozzle_c": null, "max_bed_c": null, "max_chamber_c": null, "extent": null
            },
            "review_text": review_text,
            "warnings": [{"severity": "warning", "code": "no_moves", "line": 0,
                          "message": "the G-code contains no moves", "excerpt": ""}],
            "spools": [{"slot": 1, "material": "PLA", "nozzle_id": 1, "ams_unit": 0, "tray": 0,
                        "loaded": "PLA Basic #FFFFFFFF"}]
        }))
        .unwrap()
    }

    /// The terminal shows the printer layer's review text as it is: every character, in
    /// one piece, each line flush left, between the lines the CLI adds.
    #[test]
    fn the_review_text_is_shown_word_for_word_in_one_piece() {
        let checked = "Print file \"plate_1.gcode.3mf\", plate 1, checked for the Bambu Lab X2D.\n\
                       Layers: the commands lay no plastic.\n\
                       \u{20} indented and odd: \u{e9} \t.\n\
                       SHA-256 of the file: ab.";
        let shown = shown_text(&review(checked));
        assert_eq!(shown.matches(checked).count(), 1, "{shown}");
        let (before, after) = shown.split_once(checked).unwrap();
        assert!(
            before.ends_with(&format!("{CHECKED_HEADING}\n")),
            "{before}"
        );
        assert!(
            after.starts_with("\n\nG-code check: passed, 1 warning(s)\n- no_moves (line 0): "),
            "{after}"
        );
        assert!(before.starts_with("print job job-1 (version 2)\n"));
        assert!(before.contains("SIMULATED: nothing will be printed"));
        // The plate of the layout and its pieces, not the plate inside the file.
        assert!(
            before.contains(
                "
plate 2 (box, lid)
"
            ),
            "{before}"
        );
        assert!(
            before.contains("filament 1 (PLA, nozzle 1) <- AMS 0 tray 0 (PLA Basic #FFFFFFFF)")
        );
    }

    /// A project's review text is shown word for word and in one piece, after a heading
    /// that does not call the project checked, and the terminal repeats that the project is
    /// outside Hotend's check.
    #[test]
    fn a_project_review_is_shown_word_for_word_and_says_it_is_outside_the_check() {
        let text = format!(
            "Editable Bambu Studio project. SHA-256 of the project file: {}.\n{}\n\
             Print file \"sliced.gcode.3mf\", plate 1, checked for the Bambu Lab X2D.",
            "ab".repeat(32),
            hotend_core::PROJECT_OUTSIDE_CHECK
        );
        let review: ProjectReview = serde_json::from_value(serde_json::json!({
            "project": {
                "id": "x", "project_id": "p", "version": 3, "plate": 1,
                "plan_sha256": "a", "slice_sha256": "b", "path": "/data/project.3mf",
                "sha256": "ab", "keys": {"process": [], "filament": [], "printer": []},
                "test_slice": {
                    "status": "ok", "output": "proof/output/sliced.gcode.3mf", "sha256": "c",
                    "time_s": 60.0, "filament": [], "layers": 3, "bed": null, "warnings": [],
                    "slicer": {"name": "Bambu Studio", "version": "02.09.01.70"}
                },
                "proof": "passed", "notice": hotend_core::PROJECT_OUTSIDE_CHECK,
                "delivery": null, "detail": {}, "created_at_ms": 1, "updated_at_ms": 1
            },
            "destination": {"kind": "folder", "folder": "/media/usb"},
            "file_name": "Holder-v3-abab.3mf",
            "handoff": null,
            "review_text": text,
            "warnings": [],
            "notice": hotend_core::PROJECT_OUTSIDE_CHECK
        }))
        .unwrap();
        let shown = project_shown_text(&review);
        assert_eq!(shown.matches(&text).count(), 1, "{shown}");
        let (before, after) = shown.split_once(&text).unwrap();
        assert!(
            before.ends_with(&format!("{PROJECT_HEADING}\n")),
            "{before}"
        );
        assert!(
            before.contains("the command line opens nothing"),
            "{before}"
        );
        assert!(!before.contains("checked"), "{before}");
        assert!(
            after.ends_with(&format!("{}\n", hotend_core::PROJECT_OUTSIDE_CHECK)),
            "{after}"
        );
    }

    /// A hand-off review says where the file goes, in both forms, and that the command
    /// line opens nothing.
    #[test]
    fn a_hand_off_review_names_the_folder_and_opens_nothing() {
        let mut handed = review("Print file.");
        handed.delivery = DeliveryChoice::BambuConnect;
        handed.simulated = false;
        handed.printer = None;
        handed.handoff = Some(hotend_core::HandOffTarget {
            folder: "/mnt/d/Models/Hotend".into(),
            windows_folder: Some(r"D:\Models\Hotend".to_owned()),
            opener: "stand-in opener: nothing was opened".to_owned(),
            opens: false,
        });
        let shown = shown_text(&handed);
        assert!(shown.contains("the command line opens nothing"), "{shown}");
        assert!(
            shown.contains("\nhand-off folder: /mnt/d/Models/Hotend\n"),
            "{shown}"
        );
        assert!(
            shown.contains("\nas Windows sees it: D:\\Models\\Hotend\n"),
            "{shown}"
        );
        assert!(!shown.contains("SIMULATED"), "{shown}");
    }
}
