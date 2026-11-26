//! `hotend`: the headless driver for Hotend's core.
//!
//! Every command is a thin call into [`hotend_core::Core`]; the only logic of its own is
//! wiring (which slicer, which secret store), reading arguments and printing. Two commands
//! exist for the person setting a machine up: `doctor` reports what is present and what to
//! fix, and `demo` runs the whole loop offline with a scripted agent, the stand-in slicer
//! and the simulated printer.
//!
//! A print is confirmed only by a person typing `yes` at a terminal (`print confirm`,
//! `demo --simulate-print`); no argument or environment variable stands in for that. The
//! same holds for trusting a real printer's certificate before its status is read
//! (`printer status`); that reading is read-only and sends the printer nothing.
//!
//! Exit codes: 0 the command did what was asked; 1 it ran and the outcome is a failure (a
//! model that does not build, a failed check, a missing hard requirement in `doctor`);
//! 2 it could not run (bad arguments, nothing set up).

mod app;
mod baseline;
mod chat;
mod commands;
mod demo;
mod doctor;
mod error;
mod ops;
mod plan;
mod print;
mod printer;
mod render;
mod research;
mod snapshot;
mod table;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::error::CliError;

#[derive(Debug, Parser)]
#[command(
    name = "hotend",
    version,
    about = "Hotend: from a described product to a checked CAD model to a print job",
    long_about = "Hotend: from a described product to a checked CAD model to a print job.\n\n\
        Start with `hotend doctor` (what is installed, what to fix) and `hotend demo` (the \
        whole loop offline, with a scripted agent, a stand-in slicer and a simulated printer)."
)]
struct Cli {
    /// Where projects are kept (default: HOTEND_DATA_DIR, else the platform's data folder).
    #[arg(long, global = true, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Check this machine: sandbox, Python environment, engine and agent self-tests, slicers,
    /// key store.
    Doctor(DoctorArgs),
    /// Run the offline end-to-end demo: scripted agent, real engine and checks, stand-in
    /// slicer, simulated printer.
    Demo(DemoArgs),
    /// Create, list and show projects.
    #[command(subcommand)]
    Project(ProjectCommand),
    /// Build a model script in the sandbox and store it as the project's next version.
    Build(BuildArgs),
    /// Run the checks on the current version.
    Checks(ChecksArgs),
    /// Export a version as STEP, 3MF or STL.
    Export(ExportArgs),
    /// Change one parameter of the current version; the result is a new version.
    #[command(subcommand)]
    Param(ParamCommand),
    /// Measure the current version: sizes, distances, volumes.
    Measure(MeasureArgs),
    /// List a project's versions.
    Versions {
        #[command(flatten)]
        project: ProjectArg,
        #[arg(long)]
        json: bool,
    },
    /// Make an earlier version the current one. No version is changed or removed.
    Revert(RevertArgs),
    /// Name versions as snapshots, export a snapshot's files, go back to one.
    #[command(subcommand)]
    Snapshot(snapshot::SnapshotCommand),
    /// List what the current version's contract still asks the user.
    Questions {
        #[command(flatten)]
        project: ProjectArg,
        #[arg(long)]
        json: bool,
    },
    /// Lay a version out on the printer's plate: orientation, copies, plates.
    Prepare(PrepareArgs),
    /// Prepare (if needed) and slice the current version with an installed slicer.
    Slice(SliceArgs),
    /// Show Hotend's print settings for a plate (brim, first layer, bed, fans, walls) and
    /// the reason for each.
    #[command(subcommand)]
    Plan(PlanCommand),
    /// Export a gauge plate: small test pieces that measure how this printer prints a fit.
    Gauge(GaugeArgs),
    /// Record what was measured on a printed gauge, or show the printer's calibration.
    Calibrate(CalibrateArgs),
    /// Record, list or remove proven baselines: the sticking settings of a print you
    /// reported as good, the floor for later plans on the same printer, plate and material.
    #[command(subcommand)]
    Baseline(baseline::BaselineCommand),
    /// List the printers and the ways a confirmed job can be delivered.
    Printers {
        #[arg(long)]
        json: bool,
    },
    /// Propose, review, confirm and follow print jobs.
    #[command(subcommand)]
    Print(PrintCommand),
    /// Read a real printer's status over your network (read-only: Hotend never sends it
    /// anything), and set its LAN access code.
    #[command(subcommand)]
    Printer(printer::PrinterCommand),
    /// Show or change the settings: printer, material, provider, model, limits, slicer.
    #[command(subcommand)]
    Settings(SettingsCommand),
    /// Read the audit log: every tool call, build, slice and print step.
    Audit(AuditArgs),
    /// Render PNG views of the current version.
    Views(ViewsArgs),
    /// Send a message to the AI agent, using your own provider key.
    Chat(ChatArgs),
    /// Web research: let the agent look up real products, parts and standards through the
    /// AI provider's web search; see what it found and confirm or reject its numbers.
    #[command(subcommand)]
    Research(ResearchCommand),
    /// Store a provider API key, or show which keys are set (never the keys themselves).
    #[command(subcommand)]
    Key(KeyCommand),
}

#[derive(Debug, Args)]
struct DoctorArgs {
    /// Print the report as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct DemoArgs {
    /// Folder for the demo's data and artefacts (default: a new folder in the temp directory).
    #[arg(long, value_name = "DIR")]
    out: Option<PathBuf>,
    /// After the proposal, offer to run the job on the simulated printer. The job runs only
    /// after `yes` is typed at a terminal; nothing is ever printed.
    #[arg(long)]
    simulate_print: bool,
}

#[derive(Debug, Subcommand)]
enum ProjectCommand {
    /// Create an empty project.
    New { name: String },
    /// List the projects, most recently changed first.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show a project's versions, pending print jobs and conversation.
    Show {
        /// Project id, the start of an id, or a name (default: the most recently changed).
        project: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

/// Which project a command acts on.
#[derive(Debug, Args)]
struct ProjectArg {
    /// Project id, the start of an id, or a name (default: the most recently changed).
    #[arg(long, short, value_name = "PROJECT")]
    project: Option<String>,
}

#[derive(Debug, Args)]
struct BuildArgs {
    /// The model script (PROTOCOL.md section 2).
    model: PathBuf,
    /// Project to add the version to (default: a project named after the file; created if new).
    #[arg(long, short, value_name = "PROJECT")]
    project: Option<String>,
    /// Set a parameter of the model, for example --param width=90.
    #[arg(long = "param", value_name = "NAME=VALUE")]
    params: Vec<String>,
    /// A note stored with the version.
    #[arg(long, default_value = "")]
    note: String,
}

#[derive(Debug, Args)]
struct ChecksArgs {
    #[command(flatten)]
    project: ProjectArg,
    /// Run only these checks (comma separated).
    #[arg(long, value_delimiter = ',', value_name = "CHECK")]
    only: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum FormatArg {
    Step,
    #[value(name = "3mf")]
    ThreeMf,
    Stl,
}

#[derive(Debug, Args)]
struct ExportArgs {
    #[command(flatten)]
    project: ProjectArg,
    /// File format; repeat for several.
    #[arg(long, value_enum, required = true)]
    format: Vec<FormatArg>,
    /// Version to export (default: the current one).
    #[arg(long)]
    version: Option<u32>,
    /// Export only this part; repeat for several (default: all).
    #[arg(long = "part", value_name = "NAME")]
    parts: Vec<String>,
    /// Folder to write into; it must exist.
    #[arg(long, default_value = ".", value_name = "DIR")]
    out: PathBuf,
}

#[derive(Debug, Args)]
struct SliceArgs {
    #[command(flatten)]
    project: ProjectArg,
    /// Process profile id.
    #[arg(long, default_value = app::DEMO_PROCESS_PROFILE)]
    profile: String,
    #[arg(long)]
    plate: Option<u32>,
    /// Use Hotend's stand-in slicer instead of an installed one. Its output is not printable.
    #[arg(long)]
    fake: bool,
    #[arg(long)]
    json: bool,
}

/// Which plate's plan a command shows.
#[derive(Debug, Args)]
struct PlanTarget {
    #[command(flatten)]
    project: ProjectArg,
    /// The plate, from 1 (default: 1).
    #[arg(long)]
    plate: Option<u32>,
    /// The version (default: the current one).
    #[arg(long)]
    version: Option<u32>,
}

#[derive(Debug, Subcommand)]
enum PlanCommand {
    /// Show the plan of a plate: its settings, what is still to answer, what is assumed and
    /// the before-you-print list. Prepares the version first when needed.
    Show {
        #[command(flatten)]
        target: PlanTarget,
        #[arg(long)]
        json: bool,
    },
    /// One line per decision: field, value, rule id and the sentence that explains it.
    Explain {
        #[command(flatten)]
        target: PlanTarget,
    },
}

#[derive(Debug, Subcommand)]
enum ParamCommand {
    /// Set a parameter of the current version's model and build the result as a new version.
    Set {
        name: String,
        #[arg(allow_negative_numbers = true)]
        value: f64,
        #[command(flatten)]
        project: ProjectArg,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
struct MeasureArgs {
    #[command(flatten)]
    project: ProjectArg,
    /// A measurement as a JSON object (PROTOCOL.md section 4); repeat for several.
    #[arg(long = "query", value_name = "JSON", required = true)]
    queries: Vec<String>,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct RevertArgs {
    /// The version to make current again.
    version: u32,
    #[command(flatten)]
    project: ProjectArg,
}

#[derive(Debug, Args)]
struct PrepareArgs {
    #[command(flatten)]
    project: ProjectArg,
    /// Version to prepare (default: the current one).
    #[arg(long)]
    version: Option<u32>,
    /// How many of a part to print, for example --copies lid=2.
    #[arg(long, value_name = "PART=COPIES")]
    copies: Vec<String>,
    /// Which side of a part faces up, instead of the side Hotend chooses: +x, -x, +y, -y,
    /// +z or -z, for example --up lid=-z.
    #[arg(long, value_name = "PART=DIRECTION")]
    up: Vec<String>,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct GaugeArgs {
    #[command(flatten)]
    project: ProjectArg,
    /// A joint card to make a gauge for, as JSON; repeat for several (default: the joints
    /// of the project's current version).
    #[arg(long = "card", value_name = "JSON")]
    cards: Vec<String>,
    /// The nozzle the gauge is printed with (default: the printer's main nozzle).
    #[arg(long)]
    nozzle: Option<i64>,
    /// Folder to write the gauge plate into; it must exist.
    #[arg(long, default_value = ".", value_name = "DIR")]
    out: PathBuf,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct CalibrateArgs {
    /// What a caliper read on a printed gauge: FEATURE:DRAWN=MEASURED in mm with hole or
    /// peg, for example --reading hole:6.49=6.31. Without one, the calibration is shown.
    #[arg(long = "reading", value_name = "FEATURE:DRAWN=MEASURED")]
    readings: Vec<String>,
    /// The nozzle the gauge was printed with (default: the printer's main nozzle).
    #[arg(long)]
    nozzle: Option<i64>,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum DeliveryArg {
    /// The simulated printer inside Hotend. Nothing is printed.
    Simulated,
    /// Place the file for Bambu Connect and print the link that opens it there.
    BambuConnect,
    /// Write the sliced file into a folder (`--out`).
    Folder,
}

#[derive(Debug, Args)]
struct PrintTarget {
    /// The print job's id.
    job: String,
    /// How the job is delivered.
    #[arg(long, value_enum, default_value = "simulated")]
    to: DeliveryArg,
    /// With `--to folder`: the folder to write the sliced file into.
    #[arg(long, value_name = "DIR")]
    out: Option<PathBuf>,
    /// How many simulated seconds pass per real second on the simulated printer.
    #[arg(long, default_value_t = 600.0, value_parser = time_scale, value_name = "FACTOR")]
    time_scale: f64,
}

fn time_scale(text: &str) -> Result<f64, String> {
    match text.parse::<f64>() {
        Ok(value) if (1.0..=100_000.0).contains(&value) => Ok(value),
        _ => Err("a number from 1 to 100000".to_owned()),
    }
}

#[derive(Debug, Subcommand)]
enum PrintCommand {
    /// Propose the current version's sliced file as a print job. The job waits for a person.
    Propose {
        #[command(flatten)]
        project: ProjectArg,
        /// A note stored with the job.
        #[arg(long, default_value = "")]
        note: String,
        #[arg(long)]
        json: bool,
    },
    /// Show one job, or the jobs of a project.
    Status {
        /// The print job's id (default: every job of the project).
        job: Option<String>,
        #[command(flatten)]
        project: ProjectArg,
        #[arg(long)]
        json: bool,
    },
    /// Check a pending job's G-code and show what would be printed, read from the file.
    /// Confirms and sends nothing.
    Review {
        #[command(flatten)]
        target: PrintTarget,
        #[arg(long)]
        json: bool,
    },
    /// Show the review, ask at the terminal, and deliver the job after a typed `yes`.
    /// Refuses when standard input is not a terminal.
    Confirm {
        #[command(flatten)]
        target: PrintTarget,
    },
    /// Cancel a job that has not finished.
    Cancel { job: String },
    /// Make the editable Bambu Studio project of a plate with Hotend's print settings,
    /// from a slice of the same plan that passed the G-code check, and say where it is.
    /// With `--out`, show what you confirm, ask at the terminal and save a copy into that
    /// folder after a typed `yes`. Opens nothing. A project opened in Bambu Studio is
    /// sliced again by Bambu Studio, so it is outside Hotend's check.
    Project {
        #[command(flatten)]
        project: ProjectArg,
        /// The plate of the layout (default: 1).
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=64))]
        plate: Option<u32>,
        /// The folder to save a copy of the project into, after your typed `yes`.
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,
        /// Ask Hotend's stand-in slicer, which makes no project and says so.
        #[arg(long)]
        fake: bool,
        #[arg(long, conflicts_with = "out")]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum SettingsCommand {
    /// Show the settings.
    Show {
        #[arg(long)]
        json: bool,
    },
    /// Change settings; what is not named stays as it is.
    Set(Box<SettingsSet>),
    /// List the slicers: found or not, version, whether Hotend will run it, its printers.
    Slicers {
        #[arg(long)]
        json: bool,
    },
}

/// What `settings set` changes.
#[derive(Debug, Args)]
struct SettingsSet {
    /// Printer profile id, for example bambu-x2d or prusa-mk4s.
    #[arg(long)]
    printer: Option<String>,
    /// Material id, for example pla-generic (the default: Generic PLA).
    #[arg(long)]
    material: Option<String>,
    /// Material family, for example PLA or PETG.
    #[arg(long)]
    family: Option<String>,
    /// Material density in g/cm3.
    #[arg(long)]
    density: Option<f64>,
    /// The build plate that is on the printer: cool, textured-pei, high-temp or
    /// engineering. A slice is made for this plate and a print job is checked
    /// against it; there is no default.
    #[arg(long, value_name = "PLATE")]
    plate_type: Option<String>,
    /// The diameter of the nozzle fitted to the printer, in mm: 0.2, 0.4, 0.6 or 0.8.
    /// Until it is set, 0.4 mm is assumed and shown as an assumption.
    #[arg(long, value_name = "MM")]
    nozzle: Option<f64>,
    /// The colour of the loaded spool, #rrggbb. Until it is set, grey is assumed.
    #[arg(long, value_name = "#RRGGBB")]
    spool_colour: Option<String>,
    /// Where the spool sits: an AMS slot 1 to 16, external-main or external-auxiliary.
    #[arg(long, value_name = "SLOT")]
    spool_slot: Option<String>,
    #[arg(long, value_enum, requires = "model")]
    provider: Option<ProviderArg>,
    #[arg(long, requires = "provider")]
    model: Option<String>,
    /// How much the model may think and spend on one answer. Sent only to Anthropic
    /// models documented to accept it; `high` by default.
    #[arg(long, value_enum)]
    effort: Option<EffortArg>,
    /// Most model turns one message may use.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=200))]
    max_turns: Option<u32>,
    /// The slicer to use, optionally with its program: bambu-studio or prusa-slicer,
    /// for example --slicer prusa-slicer=/usr/bin/prusa-slicer. Choosing a slicer
    /// switches to a printer that slicer has a profile for.
    #[arg(long, value_name = "SLICER[=PROGRAM]")]
    slicer: Option<String>,
    /// The folder a Bambu Connect hand-off (and an editable project for Bambu Studio) is
    /// placed in: an absolute path to an existing folder. Under WSL it must be on a
    /// Windows drive, for example /mnt/d/Models/Hotend; its Windows path must hold no
    /// comma.
    #[arg(long, value_name = "FOLDER", conflicts_with = "clear_handoff_folder")]
    handoff_folder: Option<PathBuf>,
    /// Forget the chosen hand-off folder: Hotend's own folder in the data folder is used
    /// again (under WSL a folder must then be chosen before a hand-off).
    #[arg(long)]
    clear_handoff_folder: bool,
    /// Where a sliced file's plate previews come from: `slicer` (only what the slicer
    /// draws; the Linux Bambu Studio draws none headless) or `hotend-made` (when the
    /// slicer drew none, Hotend's own renders of the plate are put into the file before
    /// it is checked, and the review says "plate previews made by Hotend"). Bambu
    /// Connect does not import a file without a plate preview.
    #[arg(long, value_enum, value_name = "SOURCE")]
    previews: Option<PreviewsArg>,
    /// The printer's IP address on your network, for reading its status (read-only):
    /// four numbers in a private range, such as 192.168.1.50. Names are not looked up.
    #[arg(
        long,
        value_name = "IP",
        requires = "printer_serial",
        conflicts_with = "clear_printer_lan"
    )]
    printer_lan_ip: Option<String>,
    /// The printer's serial number, as shown on its screen; its certificate must name it.
    #[arg(long, value_name = "SERIAL", requires = "printer_lan_ip")]
    printer_serial: Option<String>,
    /// Forget the printer's address and serial (Hotend then reads no printer status).
    #[arg(long)]
    clear_printer_lan: bool,
    #[arg(long)]
    json: bool,
}

/// The choices of `hotend settings set --previews`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum PreviewsArg {
    Slicer,
    HotendMade,
}

impl From<PreviewsArg> for hotend_core::PreviewsMode {
    fn from(arg: PreviewsArg) -> Self {
        match arg {
            PreviewsArg::Slicer => Self::Slicer,
            PreviewsArg::HotendMade => Self::HotendMade,
        }
    }
}

#[derive(Debug, Args)]
struct AuditArgs {
    #[command(flatten)]
    project: ProjectArg,
    /// Entries of every project and of none (calibration, gauges).
    #[arg(long, conflicts_with = "project")]
    all: bool,
    /// Most entries to show, newest first.
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=1000))]
    limit: u32,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ViewsArgs {
    #[command(flatten)]
    project: ProjectArg,
    /// View to render: iso, front, back, left, right, top, bottom. Repeat for several.
    #[arg(long = "view", value_name = "NAME", default_values_t = [String::from("iso")])]
    views: Vec<String>,
    /// Cut the model open first: a plane (XY, XZ or YZ), optionally with an offset in mm,
    /// for example XZ or XY@12.5.
    #[arg(long, value_name = "PLANE[@OFFSET]")]
    section: Option<String>,
    /// Image width and height in pixels.
    #[arg(long, default_value_t = 768, value_parser = clap::value_parser!(u32).range(64..=2048))]
    size: u32,
    /// Folder to write the PNG files into; it must exist.
    #[arg(long, default_value = ".", value_name = "DIR")]
    out: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum SecretsArg {
    /// The operating system keychain.
    Keyring,
    /// The provider's usual environment variable (read-only).
    Env,
    /// One file of your own (HOTEND_SECRETS_FILE, else ~/.config/hotend/secrets.json, mode
    /// 600); a key in the environment still wins. For machines without a keychain. Weaker
    /// than a keychain: programs running as you can read it.
    File,
}

/// The effort levels of `hotend settings set --effort`.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub(crate) enum EffortArg {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl From<EffortArg> for hotend_core::Effort {
    fn from(arg: EffortArg) -> Self {
        match arg {
            EffortArg::Low => Self::Low,
            EffortArg::Medium => Self::Medium,
            EffortArg::High => Self::High,
            EffortArg::Xhigh => Self::Xhigh,
            EffortArg::Max => Self::Max,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ProviderArg {
    Anthropic,
    Openai,
    Google,
    Openrouter,
    Ollama,
}

/// Whose key `hotend key set` stores: an AI provider's, or a search service's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum KeyArg {
    Anthropic,
    Openai,
    Google,
    Openrouter,
    Ollama,
    /// Keenable's search API (optional: Keenable also has a keyless trial path).
    Keenable,
    /// Tavily's search API.
    Tavily,
}

/// Where web research comes from (`hotend research source`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum SourceArg {
    /// The AI provider's own web search (Anthropic). The default.
    BuiltIn,
    /// Keenable's search API: with your key, or keyless as a trial.
    Keenable,
    /// Tavily's search API, with your key.
    Tavily,
    /// No web research.
    Off,
}

#[derive(Debug, Args)]
struct SecretsOpt {
    /// Where keys are kept (default: HOTEND_SECRETS=keyring|env|file, else the keychain).
    #[arg(long, value_enum)]
    secrets: Option<SecretsArg>,
}

#[derive(Debug, Args)]
struct ChatArgs {
    /// Project id, the start of an id, or a name; a new name creates the project.
    project: String,
    /// What to tell the agent.
    text: String,
    /// The AI provider. Saved in the settings; needed the first time.
    #[arg(long, value_enum, requires = "model")]
    provider: Option<ProviderArg>,
    /// The provider's model id. Saved in the settings; needed the first time.
    #[arg(long, requires = "provider")]
    model: Option<String>,
    #[command(flatten)]
    secrets: SecretsOpt,
    /// Most model turns this message may use.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=200))]
    max_turns: Option<u32>,
    /// Development only: replay a written conversation (`builtin:<name>` or a file) instead
    /// of asking a model. No key is used and nothing is sent.
    #[arg(long, hide = true, value_name = "SCRIPT", conflicts_with_all = ["provider", "model"])]
    script: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Switch {
    On,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CacheArg {
    /// Five minutes: cheaper to write, lost between messages that are further apart.
    #[value(name = "5m")]
    FiveMinutes,
    /// One hour: for a conversation with pauses.
    #[value(name = "1h")]
    OneHour,
}

#[derive(Debug, Subcommand)]
enum ResearchCommand {
    /// Show whether the agent may search the web for this project, what it has used and
    /// what that cost.
    Status {
        #[command(flatten)]
        project: ProjectArg,
        #[arg(long)]
        json: bool,
    },
    /// Switch web research on for a project. The first time for a source, this explains
    /// what leaves this computer and to whom, and asks for a typed `yes` at a terminal.
    On {
        #[command(flatten)]
        project: ProjectArg,
    },
    /// Choose where web research comes from, for every project: built-in (the AI
    /// provider's own search), keenable, tavily or off. A new source needs your agreement
    /// again (a typed `yes` at a terminal), because the queries go somewhere else.
    Source {
        #[arg(value_enum)]
        source: SourceArg,
    },
    /// Switch web research off for a project. Nothing found so far is removed.
    Off {
        #[command(flatten)]
        project: ProjectArg,
    },
    /// Limit searches and fetched pages to these sites (host names such as
    /// maker.example). Without a host, the limit is removed.
    Allow {
        hosts: Vec<String>,
        #[command(flatten)]
        project: ProjectArg,
    },
    /// Keep searches and fetched pages away from these sites. Without a host, the list is
    /// emptied. Not together with an `allow` list.
    Block {
        hosts: Vec<String>,
        #[command(flatten)]
        project: ProjectArg,
    },
    /// Whether a fetched PDF may stay in the conversation. Off by default: a PDF has no
    /// size limit at the provider and is paid for again with every later message.
    Pdf {
        #[arg(value_enum)]
        switch: Switch,
        #[command(flatten)]
        project: ProjectArg,
    },
    /// Whether links found inside a fetched page may be fetched too. Off by default: with
    /// it a page can make the provider request addresses of its choosing.
    Links {
        #[arg(value_enum)]
        switch: Switch,
        #[command(flatten)]
        project: ProjectArg,
    },
    /// Change how much one project may search and fetch.
    Limit {
        /// Searches this project may use in all.
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=200))]
        searches: Option<u32>,
        /// Pages this project may fetch in all (0: none).
        #[arg(long, value_parser = clap::value_parser!(u32).range(0..=200))]
        fetches: Option<u32>,
        #[command(flatten)]
        project: ProjectArg,
    },
    /// Change the settings every project starts from.
    Defaults {
        /// Whether a project has web research unless it says otherwise.
        #[arg(long, value_enum)]
        new_projects: Option<Switch>,
        /// Searches the agent may make for one message.
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=10))]
        searches_per_message: Option<u32>,
        /// Pages the agent may fetch for one message (0: search only).
        #[arg(long, value_parser = clap::value_parser!(u32).range(0..=10))]
        fetches_per_message: Option<u32>,
        /// Searches a project may use in all.
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=200))]
        searches: Option<u32>,
        /// Pages a project may fetch in all.
        #[arg(long, value_parser = clap::value_parser!(u32).range(0..=200))]
        fetches: Option<u32>,
        /// Most tokens of one fetched page that are kept.
        #[arg(long, value_parser = clap::value_parser!(u32).range(1000..=20000))]
        page_tokens: Option<u32>,
        /// How long the provider keeps the conversation cached between messages.
        #[arg(long, value_enum)]
        cache: Option<CacheArg>,
        #[arg(long)]
        json: bool,
    },
    /// Take back the agreement that search queries go to the AI provider. No project
    /// searches until `hotend research on` is agreed to again.
    Withdraw,
    /// List the numbers found on the web for a project, with their page and quote.
    Notes {
        #[command(flatten)]
        project: ProjectArg,
        #[arg(long)]
        json: bool,
    },
    /// List the searches and fetched pages of a project.
    Log {
        #[command(flatten)]
        project: ProjectArg,
        #[arg(long)]
        json: bool,
    },
    /// Say that a web number is right: you checked it against the real thing.
    Confirm {
        /// The note's id, for example r1 (see `hotend research notes`).
        note: String,
        #[command(flatten)]
        project: ProjectArg,
    },
    /// Say that a web number is wrong. The agent is told not to use it.
    Reject {
        /// The note's id, for example r1.
        note: String,
        #[command(flatten)]
        project: ProjectArg,
    },
    /// Take back a `confirm` or `reject`.
    Undecide {
        /// The note's id, for example r1.
        note: String,
        #[command(flatten)]
        project: ProjectArg,
    },
}

#[derive(Debug, Subcommand)]
enum KeyCommand {
    /// Store an AI provider's or a search service's API key in the keychain (or, with
    /// `--secrets file` / HOTEND_SECRETS=file, in your secrets file). The key is read from
    /// standard input (a pipe) or typed at a prompt without echo; it is never taken from the
    /// command line.
    Set {
        #[arg(value_enum)]
        provider: KeyArg,
        #[command(flatten)]
        secrets: SecretsOpt,
        /// Anything after the provider is refused without being repeated: a key typed here
        /// would already be in the shell history, and an error must not print it again.
        #[arg(hide = true, num_args = 0.., allow_hyphen_values = true)]
        rejected: Vec<String>,
    },
    /// Remove a stored API key (from the keychain or the secrets file). A key in the
    /// environment is not touched.
    Delete {
        #[arg(value_enum)]
        provider: KeyArg,
        #[command(flatten)]
        secrets: SecretsOpt,
    },
    /// Show for each provider and search service whether a key is set, and where.
    Status {
        #[command(flatten)]
        secrets: SecretsOpt,
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("hotend: cannot start the async runtime: {error}");
            return ExitCode::from(2);
        }
    };
    let outcome = runtime.block_on(run(cli));
    // Blocking helpers (a keychain call that hangs, say) must not keep the process alive.
    runtime.shutdown_timeout(std::time::Duration::from_secs(2));
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if let Some(message) = error.message() {
                eprintln!("hotend: {message}");
            }
            ExitCode::from(error.exit_code())
        }
    }
}

async fn run(cli: Cli) -> Result<(), CliError> {
    let data_dir = cli.data_dir;
    match cli.command {
        Command::Doctor(args) => doctor::run(args.json).await,
        Command::Demo(args) => demo::run(args.out, args.simulate_print, data_dir).await,
        Command::Project(command) => commands::project(command, data_dir).await,
        Command::Build(args) => commands::build(args, data_dir).await,
        Command::Checks(args) => commands::checks(args, data_dir).await,
        Command::Export(args) => commands::export(args, data_dir).await,
        Command::Param(command) => ops::param(command, data_dir).await,
        Command::Measure(args) => ops::measure(args, data_dir).await,
        Command::Versions { project, json } => ops::versions(project, json, data_dir).await,
        Command::Revert(args) => ops::revert(args, data_dir).await,
        Command::Snapshot(command) => snapshot::run(command, data_dir).await,
        Command::Questions { project, json } => ops::questions(project, json, data_dir).await,
        Command::Prepare(args) => ops::prepare(args, data_dir).await,
        Command::Slice(args) => commands::slice(args, data_dir).await,
        Command::Plan(command) => plan::run(command, data_dir).await,
        Command::Gauge(args) => ops::gauge(args, data_dir).await,
        Command::Calibrate(args) => ops::calibrate(args, data_dir).await,
        Command::Baseline(command) => baseline::run(command, data_dir).await,
        Command::Printers { json } => print::printers(json, data_dir).await,
        Command::Print(command) => print::print(command, data_dir).await,
        Command::Printer(command) => printer::run(command, data_dir).await,
        Command::Settings(command) => ops::settings(command, data_dir).await,
        Command::Audit(args) => ops::audit(args, data_dir).await,
        Command::Views(args) => commands::views(args, data_dir).await,
        Command::Chat(args) => chat::run(args, data_dir).await,
        Command::Research(command) => research::run(command, data_dir).await,
        Command::Key(command) => commands::key(command, data_dir).await,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_command_line_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn a_key_on_the_command_line_is_caught_and_not_treated_as_an_option() {
        // `key set` takes the provider and nothing else. What follows is collected so the
        // command can refuse it in its own words, without echoing it.
        for extra in ["sk-whatever", "--sk-whatever"] {
            let cli = Cli::try_parse_from(["hotend", "key", "set", "anthropic", extra]).unwrap();
            let Command::Key(KeyCommand::Set { rejected, .. }) = cli.command else {
                panic!("not parsed as `key set`");
            };
            assert_eq!(rejected, [extra]);
        }
    }

    #[test]
    fn the_research_source_and_the_search_keys_are_named_on_the_command_line() {
        for (word, source) in [
            ("built-in", SourceArg::BuiltIn),
            ("keenable", SourceArg::Keenable),
            ("tavily", SourceArg::Tavily),
            ("off", SourceArg::Off),
        ] {
            let cli = Cli::try_parse_from(["hotend", "research", "source", word]).unwrap();
            let Command::Research(ResearchCommand::Source { source: parsed }) = cli.command else {
                panic!("not parsed as `research source`");
            };
            assert_eq!(parsed, source);
        }
        // No argument stands in for the agreement a new source needs.
        assert!(
            Cli::try_parse_from(["hotend", "research", "source", "keenable", "--yes"]).is_err()
        );
        assert!(Cli::try_parse_from(["hotend", "research", "source", "google"]).is_err());
        for service in ["keenable", "tavily"] {
            let cli = Cli::try_parse_from(["hotend", "key", "set", service]).unwrap();
            assert!(matches!(cli.command, Command::Key(KeyCommand::Set { .. })));
        }
    }

    #[test]
    fn no_argument_of_any_command_can_confirm_a_print() {
        // The only way to a confirmation is the terminal prompt. An option that sounds like
        // a pre-approval must not exist anywhere in the command tree. (No argument can come
        // from an environment variable either: clap's `env` feature is not enabled.)
        fn walk(command: &clap::Command, found: &mut Vec<String>) {
            for argument in command.get_arguments() {
                let names: Vec<String> = argument
                    .get_long_and_visible_aliases()
                    .unwrap_or_default()
                    .into_iter()
                    .map(str::to_owned)
                    .chain(argument.get_short().map(|c| c.to_string()))
                    .collect();
                for name in names {
                    let forbidden = ["yes", "y", "confirm", "approve", "force", "assume-yes"];
                    if forbidden.contains(&name.as_str()) {
                        found.push(format!("{} --{name}", command.get_name()));
                    }
                }
            }
            for sub in command.get_subcommands() {
                walk(sub, found);
            }
        }
        let mut found = Vec::new();
        walk(&Cli::command(), &mut found);
        assert!(found.is_empty(), "{found:?}");
        for extra in ["--yes", "-y", "--confirm", "--force"] {
            assert!(
                Cli::try_parse_from(["hotend", "print", "confirm", "job", extra]).is_err(),
                "{extra}"
            );
            assert!(
                Cli::try_parse_from(["hotend", "demo", "--simulate-print", extra]).is_err(),
                "{extra}"
            );
        }
    }

    #[test]
    fn web_research_is_agreed_to_at_the_terminal_and_by_no_argument() {
        // `research on` takes a project and nothing that could stand in for the typed `yes`.
        for extra in ["--yes", "-y", "--agree", "--force", "--acknowledge"] {
            assert!(
                Cli::try_parse_from(["hotend", "research", "on", extra]).is_err(),
                "{extra}"
            );
        }
        let cli = Cli::try_parse_from(["hotend", "research", "on", "-p", "holder"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Research(ResearchCommand::On { .. })
        ));
        // The limits a person can set stay inside the ranges the core accepts.
        for bad in [
            ["limit", "--searches", "0"],
            ["limit", "--searches", "201"],
            ["defaults", "--searches-per-message", "11"],
            ["defaults", "--page-tokens", "999"],
            ["defaults", "--cache", "2h"],
        ] {
            let mut args = vec!["hotend", "research"];
            args.extend(bad);
            assert!(Cli::try_parse_from(args).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn chat_needs_provider_and_model_together() {
        assert!(
            Cli::try_parse_from(["hotend", "chat", "p", "hi", "--provider", "openai"]).is_err()
        );
        let cli = Cli::try_parse_from([
            "hotend",
            "chat",
            "p",
            "hi",
            "--provider",
            "anthropic",
            "--model",
            "m",
            "--secrets",
            "env",
        ]);
        assert!(cli.is_ok(), "{cli:?}");
    }
}
