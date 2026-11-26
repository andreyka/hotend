//! `hotend demo`: the whole loop on one product, offline.
//!
//! What is real: the agent process (sandboxed, talking to the core over its JSON-RPC
//! channel), the tool gateway, the sandboxed CAD engine, every check, the mesh, the views,
//! the STEP file, the print preparation, the G-code check, the store and the audit log.
//! What is simulated: the agent replays a written conversation instead of asking a model
//! (no key, no model request), the slicer is Hotend's stand-in, and the printer is a
//! model of one inside Hotend. The output says so; nothing here pretends otherwise.
//!
//! The print job runs on the simulated printer only with `--simulate-print` and after
//! `yes` was typed at a terminal; otherwise it stays pending.
//!
//! The demo never touches the user's projects: unless `--data-dir` is given it keeps its
//! own data directory inside the output folder.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use hotend_core::gateway::render as report;
use hotend_core::{
    AgentCommand, AuditEntry, Core, CoreConfig, DeliveryChoice, MemorySecretStore, PrintJob,
    PrinterOptions, ProviderKind, ProviderSettings, PythonRuntime, RunEvent, SecretBackend,
    Settings, StopReason,
};
use serde_json::{Value, json};

use crate::error::CliError;
use crate::{app, print, render};

/// The conversation the agent replays is the one `hotend_agent` ships as
/// `builtin:cable_box_demo`, all but its closing turn, then the demo's own turns: a slice, a
/// print proposal and a closing text. The shipped file is read when `hotend` is built, so
/// the demo never shows an older wording of the conversation than the window does.
const DESIGN_SCRIPT: &str =
    include_str!("../../../python/hotend_agent/scripts/cable_box_demo.json");
const PRINT_TURNS: &str = include_str!("../assets/cable_box_print_demo.json");
const SCRIPT_NAME: &str = "cable_box_print_demo.json";

/// The demo's script: [`DESIGN_SCRIPT`] without its closing turn, then [`PRINT_TURNS`].
fn script() -> Result<Value, serde_json::Error> {
    let design: Value = serde_json::from_str(DESIGN_SCRIPT)?;
    let mut script: Value = serde_json::from_str(PRINT_TURNS)?;
    let mut turns = design["turns"].as_array().cloned().unwrap_or_default();
    turns.pop();
    turns.extend(script["turns"].as_array().cloned().unwrap_or_default());
    script["turns"] = Value::Array(turns);
    Ok(script)
}
/// What the user says. The script answers this message and no other.
const USER_TEXT: &str = "A box that hides a power strip on my desk, with a lid that snaps on. \
    Show me a first version before I measure anything, then slice it and queue it for my review.";
/// The provider slot of the settings must name a provider. `ollama` is the one that takes
/// no key; the scripted agent never sends it (or anyone) a request.
const PLACEHOLDER_PROVIDER: ProviderKind = ProviderKind::Ollama;
const PLACEHOLDER_MODEL: &str = "scripted-demo";
/// The build plate of the demo's scenario, as the settings name it.
const DEMO_PLATE_TYPE: &str = "textured-pei";
const VIEWS: [&str; 4] = ["iso", "front", "top", "right"];
/// Simulated seconds per real second: a print of a few hours passes in a few seconds.
const SIMULATED_TIME_SCALE: f64 = 3600.0;

fn setup(error: impl std::fmt::Display) -> CliError {
    CliError::setup(error.to_string())
}

pub async fn run(
    out: Option<PathBuf>,
    simulate_print: bool,
    data_dir: Option<PathBuf>,
) -> Result<(), CliError> {
    let started = Instant::now();
    if simulate_print {
        // Before anything runs: the simulated print needs a person to say yes.
        print::require_terminal()?;
    }
    let out = output_dir(out)?;
    let data = match data_dir {
        Some(dir) => dir,
        None => {
            let dir = out.join("data");
            if dir.join("hotend.db").exists() {
                return Err(CliError::setup(format!(
                    "{} already holds a demo run; choose another --out folder or delete it",
                    out.display()
                )));
            }
            dir
        }
    };
    std::fs::create_dir_all(&data)
        .map_err(|e| CliError::setup(format!("cannot create {}: {e}", data.display())))?;
    let data = data.canonicalize().map_err(setup)?;

    // Fail before anything is printed when the machine cannot run a sandboxed job.
    let probe = tokio::task::spawn_blocking(hotend_sandbox::probe)
        .await
        .map_err(setup)?
        .map_err(|e| CliError::setup(format!("{e}. Run `hotend doctor` for the fix")))?;
    if !probe.usable {
        return Err(CliError::setup(format!(
            "the sandbox is not usable on this machine, so no model code can run: {}. Run \
             `hotend doctor` for details",
            probe
                .reason
                .unwrap_or_else(|| "no reason was given".to_owned())
        )));
    }
    let runtime = PythonRuntime::discover()
        .await
        .map_err(|e| CliError::setup(format!("{e}. Run `hotend doctor` for the fix")))?;

    // The script is a file the sandboxed agent reads; its folder joins the agent's
    // read-only view.
    let script_dir = data.join("demo");
    std::fs::create_dir_all(&script_dir).map_err(setup)?;
    let script_path = script_dir.join(SCRIPT_NAME);
    let script = script().map_err(setup)?;
    std::fs::write(&script_path, script.to_string()).map_err(setup)?;
    let mut agent = AgentCommand::python_module(&runtime);
    agent
        .args
        .extend(["--script".to_owned(), script_path.display().to_string()]);
    agent.read_only.push(script_dir);

    let core = Core::open(CoreConfig {
        data_dir: Some(data.clone()),
        // An empty in-memory store: the demo holds no key and cannot reach a keychain.
        secrets: SecretBackend::Custom(Arc::new(MemorySecretStore::default())),
        runtime: Some(runtime),
        agent: Some(agent),
        printer: PrinterOptions {
            time_scale: SIMULATED_TIME_SCALE,
            ..PrinterOptions::default()
        },
        ..CoreConfig::default()
    })
    .await?;
    core.settings_set(Settings {
        provider: Some(ProviderSettings {
            kind: PLACEHOLDER_PROVIDER,
            model: PLACEHOLDER_MODEL.to_owned(),
        }),
        // The demo's scenario has the textured plate on its (simulated) printer. A slice
        // without a chosen plate cannot become a print job.
        plate_type: Some(DEMO_PLATE_TYPE.to_owned()),
        ..Settings::default()
    })
    .await?;
    app::install_slicer(&core, true)?;
    let project = core.project_create("Cable box (demo)").await?;

    print_intro();
    let mut show = Transcript::new(&core, &project.id);
    show.step("Describe");
    println!("{}", said("You:   ", USER_TEXT));

    let mut run = core.message_send(&project.id, USER_TEXT).await?;
    let end = loop {
        match run.next().await {
            Some(RunEvent::Agent { event }) => show.event(&event).await?,
            Some(end) => break end,
            None => {
                break RunEvent::Failed {
                    code: "closed".to_owned(),
                    message: "the event stream closed before the run ended".to_owned(),
                };
            }
        }
    };
    match end {
        RunEvent::Finished {
            stop_reason: StopReason::Done,
            ..
        } => show.finish(),
        RunEvent::Finished { stop_reason, error } => {
            show.finish();
            return Err(CliError::failed(format!(
                "the demo run stopped early ({stop_reason:?}): {}",
                render::clean(&error.unwrap_or_else(|| "no detail was given".to_owned()))
            )));
        }
        RunEvent::Failed { code, message } => {
            show.finish();
            return Err(CliError::failed(format!(
                "the demo run failed ({code}): {}",
                render::clean(&message)
            )));
        }
        RunEvent::Agent { .. } => {}
    }

    let ended = show.print_steps(simulate_print, &data).await?;
    let artefacts = collect(&core, &project.id, &out, ended.is_some()).await?;
    print_artefacts(&out, &data, &artefacts);
    println!(
        "\nDone in {:.0} s. Look around with:\n  hotend --data-dir {} project show",
        started.elapsed().as_secs_f64(),
        data.display()
    );
    match ended {
        Some(job) if job.status != "finished" => Err(CliError::failed(format!(
            "the simulated print ended as `{}`, not `finished`",
            job.status
        ))),
        _ => Ok(()),
    }
}

/// The folder the artefacts go into: the one given, or a new one in the temp directory.
fn output_dir(out: Option<PathBuf>) -> Result<PathBuf, CliError> {
    let dir = match out {
        Some(dir) => dir,
        None => {
            let seconds = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            std::env::temp_dir().join(format!("hotend-demo-{seconds}"))
        }
    };
    std::fs::create_dir_all(&dir)
        .map_err(|e| CliError::setup(format!("cannot create {}: {e}", dir.display())))?;
    dir.canonicalize().map_err(setup)
}

fn print_intro() {
    println!("Hotend demo: one product through the whole loop, offline.\n");
    println!("  Real:       the sandbox, the CAD engine (build123d), every check, the mesh, the");
    println!("              views, the STEP file, the tool gateway, the store and the audit log.");
    println!("              Also real: the print preparation (orientation, plate) and the G-code");
    println!("              check that every job passes before it may be delivered.");
    println!("  Simulated:  the AI agent replays a written conversation: no model is asked, no");
    println!("              key is used, nothing is sent anywhere. The slicer is Hotend's");
    println!("              stand-in: its time and filament are rough estimates from the mesh");
    println!("              volume and its output file is not printable. The printer is a model");
    println!("              of a Bambu Lab X2D inside Hotend: nothing is heated, moved or");
    println!("              printed, and no printer or cloud is contacted.");
    println!("              The run is recorded under the provider `ollama` only because the");
    println!("              settings must name one; it takes no key and receives no request.");
    println!("  You decide: the agent can only propose a print. The job runs (on the simulated");
    println!("              printer) only with --simulate-print and after you typed yes.");
}

/// `text` as a wrapped paragraph that starts with `who`.
fn said(who: &str, text: &str) -> String {
    let indent = " ".repeat(who.chars().count() + 2);
    render::indented(text, &indent).replacen(&indent, &format!("  {who}"), 1)
}

/// Prints the run as numbered steps. Agent text is held until the tool call that follows
/// it, so each step shows what the agent said and then what it did.
struct Transcript<'a> {
    core: &'a Core,
    project_id: &'a str,
    step: u32,
    text: String,
    writes: u32,
    views: u32,
}

impl<'a> Transcript<'a> {
    fn new(core: &'a Core, project_id: &'a str) -> Self {
        Self {
            core,
            project_id,
            step: 0,
            text: String::new(),
            writes: 0,
            views: 0,
        }
    }

    fn step(&mut self, title: &str) {
        self.step += 1;
        println!("\n[{}] {title}", self.step);
    }

    fn flush_text(&mut self) {
        let text = std::mem::take(&mut self.text);
        if !text.trim().is_empty() {
            println!("{}", said("Agent: ", &text));
        }
    }

    fn detail(&self, text: &str) {
        for line in render::clean(text).lines() {
            println!("{}", render::indented(line, "      "));
        }
    }

    async fn event(&mut self, event: &Value) -> Result<(), CliError> {
        let name = event["name"].as_str().unwrap_or("");
        match event["type"].as_str() {
            Some("text_delta") => self.text.push_str(event["text"].as_str().unwrap_or("")),
            Some("tool_start") => {
                let title = match name {
                    "list_cards" => {
                        Some("Contract: what will be made, and where each number comes from")
                    }
                    "write_model_code" if self.writes == 0 => Some("Code: the first version"),
                    "write_model_code" => {
                        Some("Fix: the agent reads the failed check and corrects the model")
                    }
                    "render_views" if self.views == 0 => {
                        Some("Views: the agent looks at the result")
                    }
                    "render_views" => None,
                    "slice" => {
                        self.mesh_step().await?;
                        Some("Prepare and slice: the parts are laid on the plate, then sliced")
                    }
                    "propose_print" => Some("Print job: proposed for your review"),
                    _ => Some("Tool call"),
                };
                if let Some(title) = title {
                    self.step(title);
                }
                self.flush_text();
                println!(
                    "{}",
                    render::indented(&render::tool_start_line(event), "    ")
                );
            }
            Some("tool_end") => {
                println!("      {}", render::tool_end_line(event));
                match name {
                    "write_model_code" => {
                        self.writes += 1;
                        self.after_build().await?;
                    }
                    "render_views" => self.views += 1,
                    "slice" => self.after_slice().await?,
                    "propose_print" => self.after_proposal().await?,
                    _ => {}
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// After a build: the failed checks of a version that did not pass, or the full check
    /// summary as its own step for one that did.
    async fn after_build(&mut self) -> Result<(), CliError> {
        let detail = self.core.project_get(self.project_id).await?;
        let Some(version) = detail.versions.last() else {
            self.detail("no version was created");
            return Ok(());
        };
        let result = self
            .core
            .version_get(self.project_id, version.number)
            .await?
            .result;
        let mut table = String::new();
        report::check_table(&mut table, &result.checks);
        // A requirement of the contract that is not met is a warning when it rests on a guess,
        // and still a fact the person must see: the window counts it as a problem, and so
        // does this summary.
        let unmet: Vec<&str> = result
            .checks
            .iter()
            .filter(|check| {
                check.id == "contract" && check.data.get("met") == Some(&Value::Bool(false))
            })
            .map(|check| check.message.as_str())
            .collect();
        if version.status == hotend_core::VersionStatus::Ok && unmet.is_empty() {
            self.step(&format!("Checks: version {} passes", version.number));
            self.detail(&table);
        } else if version.status == hotend_core::VersionStatus::Ok {
            self.step(&format!(
                "Checks: version {} passes its error checks, but {} requirement(s) of the \
                 product are not met",
                version.number,
                unmet.len()
            ));
            self.detail(&format!("not met:\n- {}", unmet.join("\n- ")));
            self.detail(&table);
        } else {
            let failed: String = table
                .lines()
                .filter(|line| line.starts_with("- FAIL"))
                .collect::<Vec<_>>()
                .join("\n");
            self.detail(&format!(
                "version {} was stored with status {}:\n{failed}",
                version.number,
                render::status_word(version.status)
            ));
        }
        Ok(())
    }

    async fn mesh_step(&mut self) -> Result<(), CliError> {
        let Some(number) = self
            .core
            .project_get(self.project_id)
            .await?
            .project
            .current_version
        else {
            return Ok(());
        };
        let bytes = self.core.version_mesh(self.project_id, number).await?;
        let (parts, triangles) = mesh_stats(&bytes);
        self.step("Mesh: the tessellation the 3D viewer loads");
        self.detail(&format!(
            "mesh.json of version {number}: {parts} parts, {triangles} triangles, {} kB",
            bytes.len() / 1024
        ));
        Ok(())
    }

    /// After the slice tool: the plate the core prepared, then what the slicer said.
    async fn after_slice(&self) -> Result<(), CliError> {
        let Some(outcome) = self.core.slice_get(self.project_id, None).await? else {
            return Ok(());
        };
        let plate = outcome.record.job["plate"]
            .as_u64()
            .and_then(|plate| u32::try_from(plate).ok())
            .and_then(|plate| outcome.preparation.plate_file(plate));
        if let Some(plate) = plate {
            self.detail(&format!("prepared plate (real): {}", plate.display()));
        }
        // The first line repeats the tool summary above.
        let text = report::slice_report(&outcome);
        let rest: Vec<&str> = text.lines().skip(1).collect();
        self.detail(&rest.join("\n"));
        Ok(())
    }

    async fn after_proposal(&self) -> Result<(), CliError> {
        if let Some(job) = self.core.print_jobs(self.project_id).await?.first() {
            self.detail(&format!(
                "status: {}; bound to the sliced file's sha256 {}...\n\
                 Nothing was sent anywhere. The agent has no tool that confirms or starts a \
                 print.",
                job.status,
                job.slice_sha256.chars().take(16).collect::<String>()
            ));
        }
        Ok(())
    }

    /// The steps after the agent is done, taken by the demo in the user's place: the
    /// G-code check with the confirmation summary, then, when asked for and confirmed at
    /// the terminal, the job on the simulated printer. Returns the job as the simulated
    /// print ended, or `None` when it did not run.
    async fn print_steps(
        &mut self,
        simulate_print: bool,
        data: &Path,
    ) -> Result<Option<PrintJob>, CliError> {
        let jobs = self.core.print_jobs(self.project_id).await?;
        let Some(job) = jobs.iter().find(|job| job.status == "pending") else {
            return Err(CliError::failed(
                "the demo run ended without a pending print job",
            ));
        };
        self.step("G-code check: what you would confirm, read from the sliced file");
        let review = self
            .core
            .print_review(&job.id, DeliveryChoice::SimulatedPrinter)
            .await?;
        // Flush left and as it is: the review text in it is what a `yes` confirms.
        let shown = print::shown_text(&review);
        print!("{shown}");
        if review.warnings.iter().any(|w| w.code == "no_moves") {
            self.detail(
                "(The stand-in slicer writes a placeholder without toolpaths, so no \
                 temperature and no printed extent can be read from it. A real slicer's file \
                 shows both here, and the check reads every move.)",
            );
        }

        self.step("Print: SIMULATED printer, nothing is printed");
        if !simulate_print {
            self.detail(&format!(
                "Not run: the job stays pending. `hotend demo --simulate-print` asks you at \
                 the terminal and then runs it on the simulated printer; so does\n  \
                 hotend --data-dir {} print confirm {}",
                data.display(),
                job.id
            ));
            return Ok(None);
        }
        match print::confirm_reviewed(self.core, &review, &shown).await? {
            Some(ended) => {
                self.detail(&format!(
                    "The simulated printer reports the job as `{}`. No object exists: this \
                     was a model of a printer running on a clock {SIMULATED_TIME_SCALE:.0} \
                     times faster than real time.",
                    ended.status
                ));
                Ok(Some(ended))
            }
            None => {
                self.detail("Not confirmed: the job stays pending and nothing was sent.");
                Ok(None)
            }
        }
    }

    fn finish(&mut self) {
        if !self.text.trim().is_empty() {
            self.step("Summary");
            self.flush_text();
        }
    }
}

/// Parts and triangles of a `mesh.json` (PROTOCOL.md section 3.4), read from the buffer
/// shapes without decoding them.
fn mesh_stats(bytes: &[u8]) -> (usize, u64) {
    let Ok(mesh) = serde_json::from_slice::<Value>(bytes) else {
        return (0, 0);
    };
    let parts = mesh["hotend"]["parts"]
        .as_object()
        .map_or(0, |parts| parts.len());
    let triangles = mesh["instances"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|instance| instance["triangles"]["shape"][0].as_u64())
        .map(|indices| indices / 3)
        .sum();
    (parts, triangles)
}

/// What the demo left behind, for the closing list.
struct Artefacts {
    version: u32,
    files: Vec<(PathBuf, String)>,
    sliced_file: Option<PathBuf>,
    tool_calls: Vec<String>,
    jobs: usize,
    model_requests: usize,
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), CliError> {
    std::fs::write(path, bytes)
        .map_err(|e| CliError::failed(format!("cannot write {}: {e}", path.display())))
}

/// Copies the current version's artefacts into the output folder, renders the views and
/// writes the slice result and the audit log next to them.
async fn collect(
    core: &Core,
    project_id: &str,
    out: &Path,
    printed: bool,
) -> Result<Artefacts, CliError> {
    let detail = core.project_get(project_id).await?;
    let number = detail
        .project
        .current_version
        .ok_or_else(|| CliError::failed("the demo ended without a version"))?;
    let version = core.version_get(project_id, number).await?;
    let mut files: Vec<(PathBuf, String)> = Vec::new();

    for (name, what) in [
        ("model.py", "the model script the agent wrote"),
        (
            "model.step",
            "exact geometry (STEP), opens in any CAD program",
        ),
        ("mesh.json", "tessellation for the 3D viewer"),
    ] {
        let source = version.dir.join(name);
        let bytes = std::fs::read(&source)
            .map_err(|e| CliError::failed(format!("cannot read {}: {e}", source.display())))?;
        write(&out.join(name), &bytes)?;
        files.push((PathBuf::from(name), what.to_owned()));
    }

    let views_dir = out.join("views");
    std::fs::create_dir_all(&views_dir)?;
    let views = VIEWS.iter().map(|name| (*name).to_owned()).collect();
    for view in core
        .views_render(project_id, views, None, [768, 768])
        .await?
    {
        let name = PathBuf::from("views").join(format!("{}.png", view.name));
        write(&out.join(&name), &view.png)?;
        files.push((name, "render of the finished version".to_owned()));
    }

    let jobs = core.print_jobs(project_id).await?;
    let mut sliced_file = None;
    if let Some(outcome) = core.slice_get(project_id, None).await? {
        let record = json!({
            "simulated": true,
            "note": "Produced by Hotend's stand-in slicer: time, filament and layers are rough \
                     estimates from the mesh volume, and the sliced file is not printable. The \
                     preparation (orientation, plate) is real.",
            "project_id": project_id,
            "version": number,
            "sliced_file": outcome.output_path,
            "result": outcome.summary,
            "preparation": outcome.preparation,
            "print_jobs": jobs,
        });
        let text =
            serde_json::to_vec_pretty(&record).map_err(|e| CliError::failed(e.to_string()))?;
        write(&out.join("slice-result.json"), &text)?;
        files.push((
            PathBuf::from("slice-result.json"),
            "the plate, the slice summary (time, filament, warnings) and the print job".to_owned(),
        ));
        sliced_file = Some(outcome.output_path);
    }
    if printed {
        let log = core.simulated_printer_wire_log();
        let text = serde_json::to_vec_pretty(&log).map_err(|e| CliError::failed(e.to_string()))?;
        write(&out.join("simulated-printer-log.json"), &text)?;
        files.push((
            PathBuf::from("simulated-printer-log.json"),
            "every message the simulated printer received, in order".to_owned(),
        ));
    }

    let mut audit: Vec<AuditEntry> = core.audit_log(Some(project_id), 1000).await?;
    audit.reverse();
    let text = serde_json::to_vec_pretty(&audit).map_err(|e| CliError::failed(e.to_string()))?;
    write(&out.join("audit-log.json"), &text)?;
    files.push((
        PathBuf::from("audit-log.json"),
        "every tool call, sandboxed job and version of this run, oldest first".to_owned(),
    ));
    let of_kind = |kind: &str| audit.iter().filter(|entry| entry.kind == kind).count();
    Ok(Artefacts {
        version: number,
        files,
        sliced_file,
        tool_calls: audit
            .iter()
            .filter(|entry| entry.kind == "tool")
            .map(|entry| format!("{} ({})", entry.name, entry.outcome))
            .collect(),
        jobs: of_kind("job"),
        model_requests: of_kind("egress"),
    })
}

fn print_artefacts(out: &Path, data: &Path, artefacts: &Artefacts) {
    println!(
        "\nArtefacts of version {} in {}:",
        artefacts.version,
        out.display()
    );
    let width = artefacts
        .files
        .iter()
        .map(|(path, _)| path.display().to_string().len())
        .max()
        .unwrap_or(0);
    for (path, what) in &artefacts.files {
        println!("  {:width$}  {what}", path.display().to_string());
    }
    if let Some(path) = &artefacts.sliced_file {
        println!(
            "\nSliced file (stand-in slicer, not printable):\n  {}",
            path.display()
        );
    }
    println!(
        "The demo's own data directory (store, versions 1 and 2):\n  {}",
        data.display()
    );
    println!(
        "\nAudit log: {} tool calls, {} sandboxed engine jobs, {} model requests sent.",
        artefacts.tool_calls.len(),
        artefacts.jobs,
        artefacts.model_requests
    );
    println!(
        "{}",
        render::indented(&artefacts.tool_calls.join(", "), "  ")
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn turns(script: &Value) -> &Vec<Value> {
        script["turns"].as_array().unwrap()
    }

    /// The demo's script is the shipped cable box conversation up to the views, whatever
    /// its wording; only the slice and print turns are the demo's own.
    #[test]
    fn the_script_is_the_agents_cable_box_demo_plus_slice_and_print() {
        let ours = script().unwrap();
        let shipped: Value = serde_json::from_str(DESIGN_SCRIPT).unwrap();
        let (ours, shipped) = (turns(&ours), turns(&shipped));
        // The shipped turn that is left out only talks: no tool call is lost with it.
        let closing = shipped.last().unwrap();
        assert!(closing.get("calls").is_none(), "{closing}");
        let shared = shipped.len() - 1;
        assert!(
            shared >= 4,
            "the shipped conversation lost its design turns"
        );
        assert_eq!(ours.len(), shared + 3);
        assert_eq!(ours[..shared], shipped[..shared]);
        // The demo's first turn answers what the last shared turn asked for: the views.
        let asked: Vec<&str> = ours[shared - 1]["calls"]
            .as_array()
            .unwrap()
            .iter()
            .map(|call| call["tool"].as_str().unwrap())
            .collect();
        assert!(
            asked.iter().all(|tool| *tool == "render_views"),
            "{asked:?}"
        );
        assert_eq!(ours[shared]["expect"], json!(["views of version"]));
        let calls: Vec<&str> = ours[shared..]
            .iter()
            .flat_map(|turn| turn["calls"].as_array().into_iter().flatten())
            .map(|call| call["tool"].as_str().unwrap())
            .collect();
        assert_eq!(calls, ["slice", "propose_print"]);
        assert_eq!(
            ours[shared]["calls"][0]["args"]["profile"],
            app::DEMO_PROCESS_PROFILE
        );
        // The closing text is the demo's own: it reports what the plate and the contract
        // say (the shipped one ends before either exists) and the honest note on the slice.
        let last = ours.last().unwrap()["say"].as_str().unwrap();
        for said in [
            "upside down",
            "measure",
            "not calibrated",
            "stand-in slicer",
        ] {
            assert!(last.contains(said), "{said}");
        }
    }

    #[test]
    fn mesh_statistics_come_from_the_buffer_shapes() {
        let mesh = json!({
            "instances": [{"triangles": {"shape": [36]}}, {"triangles": {"shape": [300]}}],
            "hotend": {"parts": {"box": "/model/box", "lid": "/model/lid"}}
        });
        assert_eq!(mesh_stats(mesh.to_string().as_bytes()), (2, 112));
        assert_eq!(mesh_stats(b"not json"), (0, 0));
    }

    #[test]
    fn the_placeholder_provider_takes_no_key() {
        assert!(PLACEHOLDER_PROVIDER.rule().env_vars.is_empty());
    }
}
