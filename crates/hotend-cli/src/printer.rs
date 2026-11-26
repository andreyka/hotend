//! `hotend printer`: a real printer's status, read over the local network.
//!
//! Guarantees:
//! - Reading is read-only: the core's listener cannot publish, so nothing typed here
//!   reaches the printer, and Developer Mode stays off.
//! - The printer's certificate is trusted only by a person at a terminal who was shown its
//!   fingerprint and subject and then typed `yes` (or, without that, through the person's
//!   own file of Bambu Lab's certificate authorities). Anything typed before the question
//!   is discarded; no flag or environment variable stands in for the answer.
//! - The access code is never taken from the command line and never printed: it is read
//!   from BAMBU_LAN_ACCESS_CODE by the core, or stored here from standard input or a prompt
//!   without echo.

use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use clap::Subcommand;
use hotend_core::{
    Core, CoreEvent, LanPrinterStatus, PrinterCertificate, PrinterLanSetup, SecretBackend,
};
use zeroize::Zeroizing;

use crate::error::CliError;
use crate::{SecretsOpt, app, render};

#[derive(Debug, Subcommand)]
pub enum PrinterCommand {
    /// Show the printer's status: phase, layers, progress, time left, temperatures,
    /// problems and job name. Read-only. The first time, the printer's certificate is shown
    /// and you are asked to confirm it.
    Status {
        /// Keep following the printer and print each report until Ctrl-C.
        #[arg(long)]
        watch: bool,
        /// Seconds to wait for a report (Hotend never asks the printer for one; it sends
        /// one when something changes).
        #[arg(long, default_value_t = 15, value_parser = clap::value_parser!(u64).range(1..=120))]
        wait: u64,
        #[arg(long, conflicts_with = "watch")]
        json: bool,
    },
    /// Store, remove or check the printer's LAN access code (write-only: it is never shown).
    #[command(subcommand)]
    AccessCode(AccessCodeCommand),
    /// Forget the confirmed printer certificate; the next `status` asks again.
    ForgetCertificate,
}

#[derive(Debug, Subcommand)]
pub enum AccessCodeCommand {
    /// Store the access code shown on the printer's screen, read from standard input (a
    /// pipe) or typed at a prompt without echo; never from the command line.
    /// BAMBU_LAN_ACCESS_CODE in the environment takes precedence over a stored code.
    Set {
        #[command(flatten)]
        secrets: SecretsOpt,
        /// Anything after `set` is refused without being repeated: it may be the code.
        #[arg(hide = true, num_args = 0.., allow_hyphen_values = true)]
        rejected: Vec<String>,
    },
    /// Remove the stored access code.
    Delete {
        #[command(flatten)]
        secrets: SecretsOpt,
    },
    /// Say where the access code would come from (never the code).
    Status {
        #[command(flatten)]
        secrets: SecretsOpt,
    },
}

/// One line about the setup, for `settings show`.
pub fn setup_line(setup: &PrinterLanSetup) -> String {
    let Some(address) = &setup.address else {
        return "no printer address (`hotend settings set --printer-lan-ip <ip> \
                --printer-serial <serial>`)"
            .to_owned();
    };
    let trust = match setup.trust.as_str() {
        "ca_file" => "certificate checked against your Bambu Lab CA file".to_owned(),
        "pinned" => "certificate confirmed (pinned)".to_owned(),
        _ => "certificate not confirmed yet (`hotend printer status` asks)".to_owned(),
    };
    format!(
        "{} serial {}, read-only; {trust}; access code: {}",
        address.ip,
        address.serial,
        code_words(&setup.access_code)
    )
}

fn code_words(source: &str) -> &'static str {
    match source {
        "environment" => "from BAMBU_LAN_ACCESS_CODE",
        "secret_store" => "stored in the key store",
        "unreadable" => "the key store cannot be read",
        _ => "not set (BAMBU_LAN_ACCESS_CODE, or `hotend printer access-code set`)",
    }
}

fn temperature(current: Option<f64>, target: Option<f64>) -> String {
    match (current, target) {
        (Some(current), Some(target)) if target > 0.0 => format!("{current:.0}/{target:.0} °C"),
        (Some(current), _) => format!("{current:.0} °C"),
        _ => "not reported yet".to_owned(),
    }
}

/// The status as lines for the terminal. Text from the printer is cleaned first.
pub fn status_lines(status: &LanPrinterStatus) -> Vec<String> {
    let state = &status.state;
    let mut lines = Vec::new();
    if status.reports == 0 {
        lines.push(
            "no report yet: the printer sends one when something changes (Hotend never asks \
             for one)"
                .to_owned(),
        );
    }
    lines.push(format!(
        "phase:     {}: {}",
        state.phase,
        render::clean(&state.headline)
    ));
    let job = state
        .job_name
        .as_deref()
        .map_or_else(|| "none reported".to_owned(), render::clean);
    lines.push(match (&status.job_id, &status.matched_name) {
        (Some(id), Some(name)) => format!(
            "job:       {job} (the Hotend job {id} handed off to Bambu Connect, matched by the \
             name `{}`)",
            render::clean(name)
        ),
        _ => format!("job:       {job} (printer status; not matched to a Hotend job)"),
    });
    let mut progress = Vec::new();
    if let Some(percent) = state.progress_percent {
        progress.push(format!("{percent}%"));
    }
    match (state.layer, state.total_layers) {
        (Some(layer), Some(total)) => progress.push(format!("layer {layer} of {total}")),
        (Some(layer), None) => progress.push(format!("layer {layer}")),
        _ => {}
    }
    if let Some(minutes) = state.remaining_minutes {
        progress.push(format!("{minutes} min left"));
    }
    lines.push(format!(
        "progress:  {}",
        if progress.is_empty() {
            "not reported yet".to_owned()
        } else {
            progress.join(", ")
        }
    ));
    let nozzles: Vec<String> = status
        .nozzles
        .iter()
        .map(|n| format!("#{} {}", n.id, temperature(n.current_c, n.target_c)))
        .collect();
    lines.push(format!(
        "nozzles:   {}",
        if nozzles.is_empty() {
            "not reported yet".to_owned()
        } else {
            nozzles.join(", ")
        }
    ));
    lines.push(format!(
        "bed:       {}; chamber: {}",
        temperature(status.bed.current_c, status.bed.target_c),
        temperature(status.chamber.current_c, status.chamber.target_c)
    ));
    if state.problems.is_empty() {
        lines.push("problems:  none reported".to_owned());
    } else {
        lines.push("problems:".to_owned());
        for problem in &state.problems {
            lines.push(format!(
                "  - {} [{}]{}{}",
                render::clean(&problem.message),
                render::clean(&problem.raw_code),
                problem
                    .meaning
                    .as_deref()
                    .map(|m| format!(": {}", render::clean(m)))
                    .unwrap_or_default(),
                if problem.blocking { " (blocking)" } else { "" }
            ));
        }
    }
    if let Some(ended) = &status.ended {
        lines.push(format!(
            "offline:   the connection ended: {} (not reopened)",
            render::clean(ended)
        ));
    }
    lines
}

/// Asks the person at the terminal; true only for `yes` typed after the question.
fn ask_yes(question: &str) -> Result<bool, CliError> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(CliError::setup(
            "the printer's certificate is confirmed by a person at a terminal who has seen \
             it, and this is not a terminal: nothing was stored and nothing was connected to. \
             Run `hotend printer status` in a terminal",
        ));
    }
    std::io::stdout().flush()?;
    crate::print::discard_typed_ahead().map_err(|error| {
        CliError::setup(format!(
            "what was typed before the question could not be discarded ({error}); nothing \
             was stored"
        ))
    })?;
    print!("{question} Type `yes` to trust it, anything else to stop: ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(answer.trim() == "yes")
}

fn certificate_text(certificate: &PrinterCertificate) -> String {
    let fingerprint = certificate
        .sha256
        .as_bytes()
        .chunks(8)
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "The printer at {} (serial {}) presents this certificate:\n  \
         SHA-256: {fingerprint}\n  subject: {}\n  issuer:  {}\n\n{}\n",
        certificate.ip,
        certificate.serial,
        render::clean(&certificate.subject),
        render::clean(&certificate.issuer),
        certificate.note
    )
}

/// The first look, the question and the pin.
async fn confirm_certificate(core: &Core) -> Result<(), CliError> {
    println!("Taking a first look at the printer's certificate (nothing is sent to it)...");
    let certificate = core.printer_certificate_probe().await?;
    println!("{}", certificate_text(&certificate));
    if !ask_yes("Trust this certificate for reading the printer's status?")? {
        return Err(CliError::setup(
            "the certificate was not confirmed: nothing was stored and the printer's status \
             was not read",
        ));
    }
    core.printer_certificate_confirm(&certificate.sha256)
        .await?;
    println!("Confirmed. Later connections must present this same certificate.\n");
    Ok(())
}

async fn status(core: &Core, watch: bool, wait: u64, json: bool) -> Result<(), CliError> {
    let setup = core.printer_lan_get().await?;
    let Some(address) = setup.address.clone() else {
        return Err(CliError::setup(
            "no printer address is set: `hotend settings set --printer-lan-ip <ip> \
             --printer-serial <serial>` (the serial is on the printer's screen)",
        ));
    };
    // Without an access code there is nothing to sign in with: nothing is looked at yet.
    if !matches!(setup.access_code.as_str(), "environment" | "secret_store") {
        return Err(CliError::setup(format!(
            "the printer's LAN access code: {}; nothing was connected to",
            code_words(&setup.access_code)
        )));
    }
    if setup.trust == "unconfirmed" {
        confirm_certificate(core).await?;
    }
    if !watch {
        let status = core.printer_status_read(Duration::from_secs(wait)).await?;
        if json {
            let text = serde_json::to_string_pretty(&status)
                .map_err(|error| CliError::failed(error.to_string()))?;
            println!("{text}");
            return Ok(());
        }
        println!(
            "printer {} (serial {}), read-only, {} report(s):",
            address.ip, address.serial, status.reports
        );
        for line in status_lines(&status) {
            println!("{line}");
        }
        return Ok(());
    }
    let mut events = core.events();
    core.printer_watch_start().await?;
    println!(
        "following the printer {} (serial {}), read-only; Ctrl-C to stop",
        address.ip, address.serial
    );
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                core.printer_watch_stop().await;
                println!("stopped");
                return Ok(());
            }
            event = events.recv() => match event {
                Ok(CoreEvent::Printer { status }) => {
                    println!("--- report {}", status.reports);
                    for line in status_lines(&status) {
                        println!("{line}");
                    }
                    if status.ended.is_some() {
                        return Err(CliError::failed("the printer status connection ended"));
                    }
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
            }
        }
    }
}

/// The most standard input may hold: the code is at most 32 characters.
const MAX_CODE_INPUT: u64 = 256;

/// Reads the code from standard input: the first line of a pipe, or a prompt without echo.
/// Every buffer that held the text is wiped when it is dropped; the piped input is read
/// into a buffer large enough that it is never reallocated (which would leave a copy).
fn read_code() -> Result<Zeroizing<String>, CliError> {
    let raw = if std::io::stdin().is_terminal() {
        Zeroizing::new(
            rpassword::prompt_password("Printer LAN access code (typing is hidden, then Enter): ")
                .map_err(|error| CliError::setup(format!("cannot read the code: {error}")))?,
        )
    } else {
        let mut text = Zeroizing::new(String::with_capacity(2 * MAX_CODE_INPUT as usize));
        std::io::stdin()
            .take(MAX_CODE_INPUT)
            .read_to_string(&mut text)
            .map_err(|_| CliError::setup("cannot read the code from standard input"))?;
        text
    };
    first_line_code(&raw).ok_or_else(|| CliError::setup("no code was given; nothing was stored"))
}

/// The first line of `raw`, trimmed, in a buffer that is wiped on drop; `None` if empty.
fn first_line_code(raw: &str) -> Option<Zeroizing<String>> {
    let code = raw.lines().next().unwrap_or("").trim();
    (!code.is_empty()).then(|| Zeroizing::new(code.to_owned()))
}

async fn access_code(
    command: AccessCodeCommand,
    data_dir: Option<PathBuf>,
) -> Result<(), CliError> {
    match command {
        AccessCodeCommand::Set { secrets, rejected } => {
            if !rejected.is_empty() {
                return Err(CliError::setup(
                    "`hotend printer access-code set` takes nothing after `set`. A code is \
                     never read from the command line (it would stay in your shell history): \
                     run the command and type the code at the prompt, or pipe it in",
                ));
            }
            let backend = app::secret_backend(secrets.secrets);
            if matches!(backend, SecretBackend::Environment) {
                return Err(CliError::setup(
                    "with `--secrets env` Hotend only reads secrets: set \
                     BAMBU_LAN_ACCESS_CODE in its environment instead (keep it in a file of \
                     your own with mode 600 and load it in your shell before starting \
                     Hotend), or use `--secrets file` \
                     (HOTEND_SECRETS=file) to store it in a file of your own",
                ));
            }
            let core = app::open_core(data_dir, backend).await?;
            let mut code = read_code()?;
            // The text moves into the core's secret, which wipes it; the empty buffer left
            // here is wiped too.
            core.printer_lan_key_set(std::mem::take(&mut *code)).await?;
            println!("stored the printer's access code in the key store");
        }
        AccessCodeCommand::Delete { secrets } => {
            let core = app::open_core(data_dir, app::secret_backend(secrets.secrets)).await?;
            core.printer_lan_key_delete().await?;
            println!("removed the stored access code");
        }
        AccessCodeCommand::Status { secrets } => {
            let core = app::open_core(data_dir, app::secret_backend(secrets.secrets)).await?;
            let source = core.printer_lan_key_status().await?;
            println!("access code: {}", code_words(&source));
        }
    }
    Ok(())
}

pub async fn run(command: PrinterCommand, data_dir: Option<PathBuf>) -> Result<(), CliError> {
    match command {
        PrinterCommand::Status { watch, wait, json } => {
            let core = app::open_core(data_dir, SecretBackend::from_env()).await?;
            status(&core, watch, wait, json).await
        }
        PrinterCommand::AccessCode(command) => access_code(command, data_dir).await,
        PrinterCommand::ForgetCertificate => {
            let core = app::open_core(data_dir, SecretBackend::from_env()).await?;
            core.printer_certificate_forget().await?;
            println!(
                "forgot the printer's certificate; the next `hotend printer status` asks again"
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hotend_core::{NozzleReading, PrinterProblem, PrinterState, TemperatureReading};

    #[test]
    fn the_code_is_the_first_line_trimmed_in_a_wiped_buffer() {
        let code: Option<Zeroizing<String>> = first_line_code("  abc123XY \nsecond line\n");
        assert_eq!(code.as_deref().map(String::as_str), Some("abc123XY"));
        assert!(first_line_code("").is_none());
        assert!(first_line_code("   \nabc123XY").is_none());
    }

    fn status(job_id: Option<&str>) -> LanPrinterStatus {
        LanPrinterStatus {
            state: PrinterState {
                phase: "printing".into(),
                headline: "Printing \u{1b}[31mlayer 10".into(),
                progress_percent: Some(40),
                remaining_minutes: Some(25),
                layer: Some(10),
                total_layers: Some(50),
                job_name: Some("box-v3".into()),
                bed_c: Some(55.0),
                problems: vec![PrinterProblem {
                    message: "The nozzle is clogged".into(),
                    raw_code: "HMS_0300_0100_0001_0007".into(),
                    meaning: None,
                    blocking: true,
                    help_url: None,
                }],
                ams: Vec::new(),
            },
            nozzles: vec![NozzleReading {
                id: 0,
                current_c: Some(219.6),
                target_c: Some(220.0),
            }],
            bed: TemperatureReading {
                current_c: Some(55.0),
                target_c: Some(55.0),
            },
            chamber: TemperatureReading::default(),
            reports: 3,
            job_id: job_id.map(str::to_owned),
            matched_name: job_id.map(|_| "box-v3".to_owned()),
            connected: true,
            ended: None,
        }
    }

    #[test]
    fn the_status_says_whether_it_is_a_hotend_job_and_cleans_printer_text() {
        let ours = status_lines(&status(Some("0192-job"))).join("\n");
        assert!(
            ours.contains("the Hotend job 0192-job handed off to Bambu Connect"),
            "{ours}"
        );
        assert!(ours.contains("40%, layer 10 of 50, 25 min left"), "{ours}");
        assert!(ours.contains("#0 220/220 °C"), "{ours}");
        assert!(ours.contains("HMS_0300_0100_0001_0007") && ours.contains("(blocking)"));
        assert!(!ours.contains('\u{1b}'));
        let other = status_lines(&status(None)).join("\n");
        assert!(
            other.contains("printer status; not matched to a Hotend job"),
            "{other}"
        );
        assert!(other.contains("chamber: not reported yet"), "{other}");
    }
}
