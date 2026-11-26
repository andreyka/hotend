//! Tool results as compact text written for a language model: the status first, errors with
//! their line numbers, then the check table and the part summary. Everything is bounded.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde_json::Value;

use crate::engine::{
    CheckResult, CheckStatus, DocEntry, EngineResult, EngineStatus, ParamSpec, PartSummary,
    Question,
};
use crate::gateway::SliceOutcome;
use crate::modeling::BuildReport;
use crate::store::PendingPrintJob;
use crate::util::truncate;

/// Longest text of one tool result.
pub const MAX_RESULT_TEXT: usize = 12_000;

/// A number without trailing zeros, at most three decimals.
fn num(value: f64) -> String {
    let text = format!("{value:.3}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text.is_empty() || text == "-0" {
        "0".to_owned()
    } else {
        text.to_owned()
    }
}

// Writing to a String cannot fail, so the results of `write!` are ignored throughout.

fn params(out: &mut String, values: &BTreeMap<String, f64>, specs: &BTreeMap<String, ParamSpec>) {
    if values.is_empty() {
        return;
    }
    out.push_str("parameters:");
    for (name, value) in values {
        let _ = write!(out, " {name}={}", num(*value));
        if let Some(spec) = specs.get(name) {
            let _ = write!(out, " {}", spec.unit);
            if let (Some(min), Some(max)) = (spec.min, spec.max) {
                let _ = write!(out, " [{}..{}]", num(min), num(max));
            }
        }
        out.push(';');
    }
    out.push('\n');
}

fn parts(out: &mut String, parts: &[PartSummary]) {
    if parts.is_empty() {
        return;
    }
    let _ = writeln!(out, "parts ({}):", parts.len());
    for part in parts {
        let size: Vec<String> = (0..3)
            .map(|axis| num(part.bbox.max[axis] - part.bbox.min[axis]))
            .collect();
        let _ = write!(
            out,
            "- {}: {} mm, {} cm3",
            part.name,
            size.join(" x "),
            num(part.volume / 1000.0)
        );
        if let Some(mass) = part.mass_estimate_g {
            let _ = write!(out, ", {} g solid", num(mass));
        }
        let _ = write!(out, ", {} faces", part.faces);
        if part.solids != 1 {
            let _ = write!(out, ", {} SOLIDS", part.solids);
        }
        if !part.is_valid {
            out.push_str(", INVALID");
        }
        if !part.is_manifold {
            out.push_str(", NOT WATERTIGHT");
        }
        let z = (num(part.bbox.min[2]), num(part.bbox.max[2]));
        let _ = writeln!(out, ", z {}..{}", z.0, z.1);
    }
}

/// Whether a check result says it could not be evaluated at all (a contract requirement
/// Hotend cannot measure): `data.not_checkable` true, on a `skipped` row or on the `warn`
/// row of a requirement that also rests on a guess. A failure is always a failure.
pub fn not_checkable(check: &CheckResult) -> bool {
    matches!(check.status, CheckStatus::Skipped | CheckStatus::Warn)
        && check.data["not_checkable"] == Value::Bool(true)
}

/// `text` on one line: control characters (line breaks included) become spaces and runs of
/// white space one space, so text from a model script cannot start lines of its own in a
/// report the model reads.
fn one_line(text: &str) -> String {
    text.split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The check table: a count line, then every check that did not pass, failures first. A
/// requirement that cannot be checked has a line and a count of its own, so it is never
/// read as a check that had nothing to do.
pub fn check_table(out: &mut String, checks: &[CheckResult]) {
    if checks.is_empty() {
        out.push_str("checks: none ran\n");
        return;
    }
    // A requirement that cannot be checked counts as that, whatever its row's status.
    let count = |status| {
        checks
            .iter()
            .filter(|c| c.status == status && !not_checkable(c))
            .count()
    };
    let unknown = checks.iter().filter(|c| not_checkable(c)).count();
    let _ = writeln!(
        out,
        "checks: {} fail, {} warn, {} pass, {} skipped{}",
        count(CheckStatus::Fail),
        count(CheckStatus::Warn),
        count(CheckStatus::Pass),
        count(CheckStatus::Skipped),
        if unknown > 0 {
            format!(", {unknown} not checkable")
        } else {
            String::new()
        }
    );
    for (status, label) in [
        (CheckStatus::Fail, "FAIL"),
        (CheckStatus::Warn, "warn"),
        (CheckStatus::Skipped, "NOT CHECKABLE"),
        (CheckStatus::Skipped, "skipped"),
    ] {
        let wanted = |c: &&CheckResult| match label {
            "NOT CHECKABLE" => not_checkable(c),
            _ => c.status == status && !not_checkable(c),
        };
        for check in checks.iter().filter(wanted) {
            let part = check
                .part
                .as_deref()
                .map(|p| format!(" [{p}]"))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "- {label} {}{part}: {}",
                check.id,
                truncate(&check.message, 400)
            );
        }
    }
    let mut passed: Vec<&str> = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Pass)
        .map(|c| c.id.as_str())
        .collect();
    passed.dedup();
    if !passed.is_empty() {
        let _ = writeln!(out, "- pass: {}", passed.join(", "));
    }
}

fn stdout(out: &mut String, result: &EngineResult) {
    if !result.stdout.trim().is_empty() {
        let _ = writeln!(out, "stdout:\n{}", truncate(result.stdout.trim_end(), 2000));
    }
    for warning in &result.warnings {
        let _ = writeln!(out, "warning: {}", truncate(warning, 400));
    }
}

/// Most questions one result lists; the rest is counted in its last line.
const MAX_SHOWN_QUESTIONS: usize = 16;
/// Longest list of questions of one result, in bytes. About what twelve long questions took.
const MAX_QUESTIONS_TEXT: usize = 5_200;
/// The room a list of questions always gets, however long the checks before it are: the
/// check table keeps the rest of the result.
const MIN_QUESTIONS_TEXT: usize = 3_000;

/// How early a question is listed, and whether a fit may depend on it ([`MAY_FIT`]).
///
/// 0: a fit depends on it. A question about a bought device (one of its own, or an id that
/// starts with the device's), about a reference file (`reference_<name>`), or of a contract
/// requirement of [`FIT_REQUIREMENTS`]; and, when the contract's rows are there, every
/// question without a row: the engine asks it itself, about a pocket.
/// 1 and 2: it may. The contract's rows are missing (a partial check run, a crashed check),
/// so its kind is not known. First those without a guessed parameter, as a pocket's has
/// none, then those with one (a size the thing must hold may rest on a guess).
/// 3: its row says that it is something else, such as a wall thickness.
fn question_rank(question: &Question, result: &EngineResult) -> u8 {
    let id = question.requirement.as_str();
    let of_device = id.starts_with("reference_")
        || result.components.iter().any(|component| {
            component
                .questions
                .iter()
                .any(|asked| asked.requirement == id)
                || id
                    .strip_prefix(component.id.as_str())
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('_'))
        });
    if of_device {
        return 0;
    }
    let mut rows = false;
    let mut kinds = Vec::new();
    for check in result.checks.iter().filter(|check| check.id == "contract") {
        let (Some(requirement), Some(kind)) = (
            check.data["requirement"].as_str(),
            check.data["requirement_kind"].as_str(),
        ) else {
            continue;
        };
        rows = true;
        if requirement == id {
            kinds.push(kind);
        }
    }
    match (rows, kinds.is_empty()) {
        (true, true) => 0,
        (true, false) if kinds.iter().any(|kind| FIT_REQUIREMENTS.contains(kind)) => 0,
        (false, _) if question.guessed_params.is_empty() => 1,
        (false, _) => MAY_FIT,
        _ => 3,
    }
}

/// The last rank of a question a fit may depend on.
const MAY_FIT: u8 = 2;

/// The kinds of requirement a fit or a given size depends on (`dimension` may be a mounting
/// pattern).
const FIT_REQUIREMENTS: [&str; 6] = [
    "fit",
    "reference_fit",
    "cavity_min",
    "outer_max",
    "standoff",
    "dimension",
];

/// What the engine wants the user asked (requirements that rest on a guess, what is not
/// verified about a device): one line per question in the order of [`question_rank`]. At
/// most [`MAX_SHOWN_QUESTIONS`] lines and `room` bytes; what does not fit is counted in a
/// last line, with how many of them a fit may depend on.
fn questions(result: &EngineResult, room: usize) -> String {
    let mut out = String::new();
    let Some(questions) = result.questions.as_ref().filter(|q| !q.is_empty()) else {
        return out;
    };
    let mut ordered: Vec<(u8, &Question)> = questions
        .iter()
        .map(|question| (question_rank(question, result), question))
        .collect();
    ordered.sort_by_key(|(rank, _)| *rank);
    let _ = writeln!(
        out,
        "questions for the user ({}; ask them, do not answer them yourself):",
        questions.len()
    );
    let mut shown = 0;
    for (_, question) in ordered.iter().take(MAX_SHOWN_QUESTIONS) {
        let line = format!(
            "- [{}] {}\n",
            truncate(&question.requirement, 64),
            truncate(&question.text, 400)
        );
        if out.len() + line.len() > room {
            break;
        }
        out.push_str(&line);
        shown += 1;
    }
    let hidden = &ordered[shown..];
    if !hidden.is_empty() {
        let fits = hidden.iter().filter(|(rank, _)| *rank <= MAY_FIT).count();
        let _ = writeln!(
            out,
            "- and {} more, not listed here ({}): the maker sees all of them in Hotend's list \
             of open questions",
            hidden.len(),
            if fits == 0 {
                "none of them about a fit".to_owned()
            } else {
                format!("a fit may depend on {fits} of them")
            }
        );
    }
    out
}

/// The end of a build's or check run's result: the questions stay whole, and what comes
/// before them (the check table, failures first) is cut when the text is too long. A long
/// body makes the list of questions shorter, to [`MIN_QUESTIONS_TEXT`] at least.
fn with_questions(body: &str, result: &EngineResult) -> String {
    let room = MAX_RESULT_TEXT
        .saturating_sub(body.len())
        .clamp(MIN_QUESTIONS_TEXT, MAX_QUESTIONS_TEXT);
    let questions = questions(result, room);
    let mut out = truncate(body, MAX_RESULT_TEXT.saturating_sub(questions.len()));
    if !questions.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&questions);
    out
}

/// The web numbers of the project as they stand in the version that was just built, for
/// the model: one short line for each that nobody has confirmed, and the parameters tagged
/// `web` without a note behind them. The long text, with the quote, is for the maker
/// (`questions_list`, the print review). Empty when there is nothing to say.
pub fn web_numbers(numbers: &[crate::research::WebNumber], problems: &[String]) -> String {
    let open: Vec<&crate::research::WebNumber> = numbers
        .iter()
        .filter(|number| number.state == "unconfirmed")
        .collect();
    let mut out = String::new();
    if !open.is_empty() {
        let _ = writeln!(out, "web numbers nobody has confirmed ({}):", open.len());
    }
    for number in open.iter().take(12) {
        let value = crate::research::number_text(number.value);
        let head = format!(
            "{} {} {value} {}",
            number.note,
            truncate(&number.key, 40),
            number.unit
        );
        let used = match (number.params.first(), number.built) {
            (None, _) => "no parameter names it".to_owned(),
            (Some(name), Some(built)) if (built - number.value).abs() > 1e-9 => format!(
                "{name} is built with {}",
                crate::research::number_text(built)
            ),
            (Some(_), _) => format!("used by {}", number.params.join(", ")),
        };
        let _ = if number.evidence == crate::research::UNSOURCED {
            writeln!(
                out,
                "{head} no page recorded, {used}: record its page with record_source, or \
                 tell the maker it is your own number"
            )
        } else {
            writeln!(out, "{head} {} unconfirmed, {used}", number.host)
        };
    }
    for problem in problems.iter().take(4) {
        let _ = writeln!(out, "- [web:source] {}", truncate(problem, 300));
    }
    out
}

/// The result of `write_model_code` and `set_parameter`.
pub fn build_report(report: &BuildReport, current_before: Option<u32>) -> String {
    let result = &report.result;
    let mut out = String::new();
    let _ = writeln!(out, "status: {}", result.status.as_str());
    match &report.version {
        Some(version) => {
            let _ = writeln!(out, "version: {} created and now current", version.number);
        }
        None => {
            let kept = current_before
                .map(|n| format!("the current version is still {n}"))
                .unwrap_or_else(|| "the project still has no version".to_owned());
            let _ = writeln!(out, "no version was created; {kept}");
        }
    }
    if let Some(error) = &result.error {
        let line = error
            .line
            .map(|l| format!(" at line {l}"))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "error: {}{line}: {}",
            error.kind,
            truncate(&error.message, 1500)
        );
        if !error.traceback.trim().is_empty() {
            let _ = writeln!(
                out,
                "traceback:\n{}",
                truncate(error.traceback.trim_end(), 3000)
            );
        }
    }
    if result.status == EngineStatus::CheckError {
        out.push_str("an error-severity check failed: fix it before this version is printed\n");
    }
    params(&mut out, &result.params, &result.param_specs);
    parts(&mut out, &result.parts);
    if result.status != EngineStatus::ModelError {
        check_table(&mut out, &result.checks);
    }
    stdout(&mut out, result);
    with_questions(&out, result)
}

/// The result of `run_checks`.
pub fn checks_report(result: &EngineResult, version: u32) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "status: {} (version {version})",
        result.status.as_str()
    );
    if let Some(error) = &result.error {
        let line = error
            .line
            .map(|l| format!(" at line {l}"))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "error: {}{line}: {}",
            error.kind,
            truncate(&error.message, 1500)
        );
    } else {
        check_table(&mut out, &result.checks);
    }
    stdout(&mut out, result);
    with_questions(&out, result)
}

/// The result of `measure`: one line of JSON per query, in order.
pub fn measure_report(results: &[Value]) -> String {
    let mut out = String::new();
    for (index, result) in results.iter().enumerate() {
        let _ = writeln!(
            out,
            "{}. {}",
            index + 1,
            truncate(&result.to_string(), 1500)
        );
    }
    out.push_str("lengths in mm, areas in mm2, volumes in mm3\n");
    truncate(&out, MAX_RESULT_TEXT)
}

/// The result of `read_docs` and `list_cards`.
pub fn docs_report(versions: &BTreeMap<String, Option<String>>, docs: &[DocEntry]) -> String {
    let mut out = String::from("installed:");
    for (package, version) in versions {
        let _ = write!(
            out,
            " {package} {};",
            version.as_deref().unwrap_or("not installed")
        );
    }
    out.push('\n');
    if docs.is_empty() {
        out.push_str("no matching documentation\n");
    }
    for doc in docs {
        let _ = writeln!(
            out,
            "\n## {} ({})\n{}{}\n{}",
            doc.name,
            doc.kind,
            doc.name.rsplit('.').next().unwrap_or(""),
            truncate(&doc.signature, 600),
            truncate(doc.doc.trim(), 1500)
        );
    }
    truncate(&out, MAX_RESULT_TEXT)
}

/// What `list_components` searches the docs index for: every entry of the catalogue module
/// has these words in its name.
pub const COMPONENTS_QUERY: &str = "shipped component";
/// The module of `hotend_fit` that writes the catalogue from its shipped component data:
/// `shipped_components` (the introduction) and one `shipped_component_<id>` per component.
const CATALOGUE_MODULE: &str = "hotend_fit.components.catalogue";
const CATALOGUE_INTRO: &str = "hotend_fit.components.catalogue.shipped_components";
const CATALOGUE_ENTRY: &str = "hotend_fit.components.catalogue.shipped_component_";

/// The result of `list_components`: the catalogue's introduction, then one line per shipped
/// component in the order of their ids, or that the installation ships none.
pub fn components_report(docs: &[DocEntry]) -> String {
    let catalogue: Vec<&DocEntry> = docs
        .iter()
        .filter(|doc| doc.module == CATALOGUE_MODULE && !doc.doc.trim().is_empty())
        .collect();
    let mut entries: Vec<&DocEntry> = catalogue
        .iter()
        .copied()
        .filter(|doc| doc.name.starts_with(CATALOGUE_ENTRY))
        .collect();
    if entries.is_empty() {
        return "this installation ships no components: there is no bought device whose data \
                Hotend has; use web research if it is on, otherwise ask the maker\n"
            .to_owned();
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out = String::new();
    if let Some(intro) = catalogue.iter().find(|doc| doc.name == CATALOGUE_INTRO) {
        let _ = writeln!(out, "{}\n", intro.doc.trim());
    }
    for entry in entries {
        let _ = writeln!(out, "{}", entry.doc.trim());
    }
    truncate(&out, MAX_RESULT_TEXT)
}

/// The result of `slice`.
pub fn slice_report(outcome: &SliceOutcome) -> String {
    let (summary, preparation) = (&outcome.summary, &outcome.preparation);
    let plate = outcome
        .record
        .job
        .get("plate")
        .and_then(Value::as_u64)
        .unwrap_or(1);
    let mut out = String::new();
    let _ = writeln!(
        out,
        "status: {} (version {}, profile {}, plate {plate} of {})",
        truncate(&summary.status, 40),
        outcome.record.version,
        truncate(&outcome.record.profile, 80),
        preparation.plates.len()
    );
    let _ = writeln!(out, "print settings: {}", outcome.settings_source());
    if let Some(size) = outcome.record.job["nozzle_diameter_mm"].as_f64() {
        let set = outcome.record.job["nozzle_size_set"].as_bool() == Some(true);
        let _ = writeln!(
            out,
            "nozzle: {size} mm{}",
            if set {
                ""
            } else {
                " (assumed: no size is set)"
            }
        );
    }
    let minutes = summary.time_s / 60.0;
    let grams: f64 = summary.filament.iter().map(|f| f.grams).sum();
    let _ = writeln!(
        out,
        "print time: {} min; filament: {} g; layers: {}",
        num(minutes),
        num(grams),
        summary.layers
    );
    for filament in &summary.filament {
        let _ = writeln!(
            out,
            "- slot {} on nozzle {}: {} g, {} m",
            filament.slot,
            filament.nozzle_id,
            num(filament.grams),
            num(filament.metres)
        );
    }
    out.push_str("orientation on the plate:\n");
    for orientation in preparation.orientations.iter().take(40) {
        let copies = if orientation.copies > 1 {
            format!(" x{}", orientation.copies)
        } else {
            String::new()
        };
        let _ = writeln!(
            out,
            "- {}{copies}: {}. {}",
            truncate(&orientation.part, 60),
            truncate(&orientation.turned, 60),
            truncate(&orientation.explanation, 300)
        );
    }
    if preparation.unplaced.is_empty() {
        out.push_str("every piece fits a plate\n");
    } else {
        let _ = writeln!(
            out,
            "DID NOT FIT: {} piece(s) are on no plate and will not be printed:",
            preparation.unplaced.len()
        );
        for piece in preparation.unplaced.iter().take(20) {
            let _ = writeln!(
                out,
                "- {}: {}",
                truncate(&piece.name, 100),
                truncate(&piece.reason, 300)
            );
        }
    }
    for check in preparation
        .checks
        .iter()
        .filter(|check| matches!(check.status, CheckStatus::Warn | CheckStatus::Fail))
        .take(10)
    {
        let _ = writeln!(
            out,
            "plate check {} {}: {}",
            if check.status == CheckStatus::Fail {
                "FAIL"
            } else {
                "warn"
            },
            truncate(&check.id, 60),
            truncate(&check.message, 300)
        );
    }
    for warning in summary.warnings.iter().take(20) {
        let _ = writeln!(
            out,
            "warning {}: {}",
            truncate(&warning.code, 60),
            truncate(&warning.message, 400)
        );
    }
    let _ = writeln!(
        out,
        "slicer: {} {}",
        truncate(&summary.slicer.name, 60),
        truncate(&summary.slicer.version, 40)
    );
    truncate(&out, MAX_RESULT_TEXT)
}

/// One sweep setting as `name=value, name=value`.
fn setting_text(params: &BTreeMap<String, f64>) -> String {
    params
        .iter()
        .map(|(name, value)| format!("{name}={}", num(*value)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The result of `sweep_parameters`: one block per setting, then per parameter the values
/// that passed and failed. Says that nothing was stored and that the numbers are the range
/// sampler's advice.
pub fn sweep_report(result: &EngineResult, version: u32, base: &BTreeMap<String, f64>) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "sweep of version {version}: nothing was stored and no version was created; each \
         setting was built from scratch with the other parameters as the version has them"
    );
    if let Some(error) = &result.error {
        let line = error
            .line
            .map(|l| format!(" at line {l}"))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "status: {}\nerror: {}{line}: {}",
            result.status.as_str(),
            error.kind,
            truncate(&error.message, 1500)
        );
        return truncate(&out, MAX_RESULT_TEXT);
    }
    let Some(report) = &result.sweep else {
        out.push_str("the engine answered without a sweep report\n");
        return truncate(&out, MAX_RESULT_TEXT);
    };
    if !base.is_empty() {
        let _ = writeln!(out, "version {version}: {}", setting_text(base));
    }
    let _ = writeln!(
        out,
        "judged by: {} (error checks and the contract, as a build is)",
        report.checks.join(", ")
    );
    for (index, sample) in report.samples.iter().enumerate() {
        let _ = writeln!(
            out,
            "{}. {}: {}",
            index + 1,
            setting_text(&sample.params),
            match sample.status.as_str() {
                "ok" => "builds and passes the error checks",
                "check_error" => "builds, but an error check FAILS",
                "model_error" => "does NOT build",
                _ => "not built: the time for this call ran out",
            }
        );
        for failure in sample.failed.iter().take(6) {
            let _ = writeln!(
                out,
                "   - FAIL {}: {}",
                truncate(&one_line(&failure.check), 64),
                truncate(&one_line(&failure.message), 300)
            );
        }
        if !sample.requirements.is_empty() {
            let words: Vec<String> = sample
                .requirements
                .iter()
                .map(|requirement| {
                    let verdict = match (requirement.not_checkable, requirement.met) {
                        (true, _) | (false, None) => "not checkable",
                        (false, Some(true)) => "met",
                        (false, Some(false)) => "NOT met",
                    };
                    let guess = if requirement.status == CheckStatus::Warn {
                        " (rests on a guess)"
                    } else {
                        ""
                    };
                    format!(
                        "{} {verdict}{guess}",
                        truncate(&one_line(&requirement.id), 64)
                    )
                })
                .collect();
            let _ = writeln!(out, "   contract: {}", words.join("; "));
        }
        if !sample.parts.is_empty() {
            let sizes: Vec<String> = sample
                .parts
                .iter()
                .map(|(name, size)| {
                    format!(
                        "{name} {} x {} x {} mm",
                        num(size[0]),
                        num(size[1]),
                        num(size[2])
                    )
                })
                .collect();
            let _ = writeln!(out, "   parts: {}", sizes.join("; "));
        }
    }
    // Per parameter swept on its own: which of its values passed.
    let single: BTreeMap<&str, Vec<(f64, bool)>> = report
        .samples
        .iter()
        .filter(|sample| sample.params.len() == 1 && sample.status != "not_run")
        .fold(BTreeMap::new(), |mut by_name, sample| {
            if let Some((name, value)) = sample.params.iter().next() {
                by_name
                    .entry(name.as_str())
                    .or_insert_with(Vec::new)
                    .push((*value, sample.status == "ok"));
            }
            by_name
        });
    for (name, tried) in single {
        let list = |ok: bool| {
            let values: Vec<String> = tried
                .iter()
                .filter(|(_, passed)| *passed == ok)
                .map(|(value, _)| num(*value))
                .collect();
            if values.is_empty() {
                "none".to_owned()
            } else {
                values.join(", ")
            }
        };
        let _ = writeln!(
            out,
            "{name}: passes at {}; fails at {}",
            list(true),
            list(false)
        );
    }
    if report.truncated {
        let _ = writeln!(
            out,
            "partial report: the time for this call ran out after {} of {} settings; the \
             ones above marked not built were stopped or never started: try fewer settings",
            report.builds,
            report.samples.len()
        );
    }
    out.push_str(
        "this is advice from the range sampler, not a check of a version: set the value you \
         choose with set_parameter, which builds and checks it as a version\n",
    );
    truncate(&out, MAX_RESULT_TEXT)
}

/// The result of `list_print_profiles`.
pub fn catalogue_report(
    settings: &crate::settings::Settings,
    catalogue: Option<&crate::gateway::SlicerCatalogue>,
) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "settings now: printer {}; material {} ({}); build plate {}; nozzle {}",
        truncate(&settings.printer_id, 64),
        truncate(&settings.material.id, 64),
        truncate(&settings.material.family, 32),
        settings
            .plate_type
            .as_deref()
            .map(|plate| truncate(plate, 64))
            .unwrap_or_else(
                || "not chosen yet (only the maker knows which is on the printer)".to_owned()
            ),
        settings
            .nozzle_diameter_mm
            .map(|size| format!("{} mm", num(size)))
            .unwrap_or_else(|| "0.4 mm (assumed: not set)".to_owned())
    );
    let Some(catalogue) = catalogue else {
        out.push_str(
            "Hotend has no list of slicer profiles for this printer, or no slicer is set up \
             in this build\n",
        );
        return out;
    };
    let entry = |entry: &crate::gateway::CatalogueEntry| {
        let aliases = if entry.aliases.is_empty() {
            String::new()
        } else {
            format!(" (also {})", entry.aliases.join(", "))
        };
        format!(
            "- {}{aliases}: {}{}",
            truncate(&entry.id, 64),
            truncate(&entry.name, 120),
            if entry.verified {
                ""
            } else {
                " (name not yet seen on a real installation)"
            }
        )
    };
    for (title, list) in [
        (
            "process profiles (the `profile` of slice)",
            &catalogue.processes,
        ),
        ("materials", &catalogue.materials),
        ("build plates", &catalogue.plates),
    ] {
        let _ = writeln!(out, "{title}:");
        for item in list.iter().take(40) {
            let _ = writeln!(out, "{}", entry(item));
        }
    }
    out.push_str(
        "only the maker changes the material, the build plate or the nozzle, in Hotend's \
         settings; ask them if one is wrong\n",
    );
    truncate(&out, MAX_RESULT_TEXT)
}

/// A plan value as people read it.
fn plan_value(value: &Value, unit: &str) -> String {
    let text = match value {
        Value::Null => return "as the installed profile has it".to_owned(),
        Value::String(text) => truncate(text, 80),
        Value::Bool(true) => "on".to_owned(),
        Value::Bool(false) => "off".to_owned(),
        other => truncate(&other.to_string(), 80),
    };
    match unit {
        "" | "enum" | "flag" | "step" | "count" => text,
        "percent" => format!("{text} %"),
        unit => format!("{text} {}", truncate(unit, 16)),
    }
}

/// The result of `print_plan` (the text of `hotend plan show`, or `explain`).
pub fn plan_report(view: &crate::modeling::PrintPlanView, explain: bool) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "version {}, plate {}: plan {}",
        view.version,
        view.plate,
        view.sha256.get(..16).unwrap_or(&view.sha256)
    );
    let _ = writeln!(out, "a slice of this plate uses {}", view.settings_source);
    let _ = writeln!(
        out,
        "nozzle: {} mm{}",
        num(view.nozzle_diameter_mm),
        if view.nozzle_size_set {
            ""
        } else {
            " (assumed)"
        }
    );
    if let Some(baseline) = &view.baseline {
        let _ = writeln!(
            out,
            "proven baseline: {} / {} / {}, printed {}; it set {}",
            truncate(&baseline.printer_id, 64),
            truncate(&baseline.plate_type, 64),
            truncate(&baseline.material_id, 64),
            truncate(&baseline.printed_on, 16),
            if baseline.fields.is_empty() {
                "nothing the rules did not already give".to_owned()
            } else {
                baseline.fields.join(", ")
            }
        );
    }
    if let Some(blocked) = &view.blocked {
        let _ = writeln!(
            out,
            "BLOCKED ({}): {}",
            truncate(&blocked.code, 64),
            truncate(&blocked.sentence, 600)
        );
    }
    for decision in view.decisions.iter().take(80) {
        if explain {
            let _ = writeln!(
                out,
                "- {} = {} ({}; rule {}, {}): {}",
                truncate(&decision.field, 64),
                plan_value(&decision.value, &decision.unit),
                truncate(&decision.origin, 120),
                truncate(&decision.rule, 16),
                truncate(&decision.basis, 16),
                truncate(&decision.sentence, 300)
            );
        } else {
            let _ = writeln!(
                out,
                "- {} = {} ({})",
                truncate(&decision.field, 64),
                plan_value(&decision.value, &decision.unit),
                truncate(&decision.origin, 120)
            );
        }
    }
    for (label, list) in [
        ("question for the maker", &view.questions),
        ("warning", &view.warnings),
        ("note", &view.notes),
        ("assumed", &view.assumptions),
    ] {
        for notice in list.iter().take(20) {
            let _ = writeln!(
                out,
                "{label} ({}): {}",
                truncate(&notice.code, 64),
                truncate(&notice.sentence, 400)
            );
        }
    }
    if !view.checklist.is_empty() {
        out.push_str("before you print:\n");
        for item in view.checklist.iter().take(20) {
            let _ = writeln!(out, "  - {}", truncate(&item.text, 300));
        }
    }
    out.push_str(
        "read-only: the plan follows from the plate and Hotend's settings; only the maker \
         changes the settings or confirms a print\n",
    );
    truncate(&out, MAX_RESULT_TEXT)
}

/// The result of `propose_print`.
pub fn proposal_report(job: &PendingPrintJob) -> String {
    format!(
        "pending print job {} created for version {}.\nIt is waiting for the user to review \
         and confirm it in Hotend. Nothing has been sent to a printer, and no tool can start \
         the print.\n",
        job.id, job.version
    )
}

/// The result of `snapshot_model`.
pub fn snapshot_report(snapshot: &crate::store::Snapshot) -> String {
    format!(
        "snapshot \"{}\" now names version {}.\nNothing else changed: the version stays as it \
         is and the design can go on. The maker can export the snapshot's STEP and 3MF files \
         or go back to it in Hotend; no tool can do either.\n",
        snapshot.name, snapshot.version
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    fn result(value: Value) -> EngineResult {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn numbers_are_short() {
        assert_eq!(num(90.0), "90");
        assert_eq!(num(29.443645), "29.444");
        assert_eq!(num(0.3), "0.3");
        assert_eq!(num(-0.0001), "0");
    }

    #[test]
    fn the_component_list_is_the_catalogue_or_says_there_is_none() {
        let doc = |name: &str, module: &str, text: &str| DocEntry {
            name: name.to_owned(),
            kind: "function".to_owned(),
            module: module.to_owned(),
            signature: "() -> dict".to_owned(),
            doc: text.to_owned(),
            score: 1.0,
        };
        let module = "hotend_fit.components.catalogue";
        let found = [
            // Ranked by the search, not by id; another module's hit is left out.
            doc(
                &format!("{module}.shipped_component_zz_board"),
                module,
                "- zz_board: A board",
            ),
            doc(
                "hotend_fit.capabilities.bought_components",
                "hotend_fit.capabilities",
                "recipe",
            ),
            doc(
                &format!("{module}.shipped_components"),
                module,
                "The bought devices: a, b (2).",
            ),
            doc(
                &format!("{module}.shipped_component_sample_phone"),
                module,
                "- sample_phone: Sample phone",
            ),
        ];
        let text = components_report(&found);
        assert_eq!(
            text,
            "The bought devices: a, b (2).\n\n- sample_phone: Sample phone\n- zz_board: A board\n"
        );
        assert!(
            !text.contains("shipped_component"),
            "no Python names: {text}"
        );
        for none in [vec![], vec![found[1].clone(), found[2].clone()]] {
            let text = components_report(&none);
            assert!(text.contains("ships no components"), "{text}");
            assert!(text.contains("otherwise ask the maker"), "{text}");
        }
    }

    #[test]
    fn a_model_error_leads_with_the_type_line_and_traceback() {
        let report = BuildReport {
            version: None,
            result: result(
                json!({"protocol": 0, "op": "build", "status": "model_error",
                "error": {"type": "ValueError", "message": "radius too big", "line": 9,
                          "traceback": "  File \"model.py\", line 9, in build"},
                "stdout": "about to fillet\n"}),
            ),
        };
        let text = build_report(&report, Some(2));
        assert!(text.starts_with(
            "status: model_error\nno version was created; the current version is still 2\n"
        ));
        assert!(text.contains("error: ValueError at line 9: radius too big"));
        assert!(text.contains("line 9, in build"));
        assert!(text.contains("stdout:\nabout to fillet"));
        assert!(!text.contains("checks:"));
    }

    #[test]
    fn the_check_table_lists_failures_first_and_passes_once() {
        let check = |id: &str, status: &str, part: Value| {
            json!({"id": id, "status": status, "severity": "error", "message": format!("{id} msg"),
                   "part": part, "data": {}})
        };
        let result = result(
            json!({"protocol": 0, "op": "build", "status": "check_error",
            "error": null, "checks": [
                check("valid_solid", "pass", json!("box")),
                check("valid_solid", "pass", json!("lid")),
                check("overhangs", "warn", json!("lid")),
                check("min_wall", "fail", json!("box")),
                check("fit_pairs", "skipped", Value::Null)]}),
        );
        let text = checks_report(&result, 4);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "status: check_error (version 4)");
        assert_eq!(lines[1], "checks: 1 fail, 1 warn, 2 pass, 1 skipped");
        assert_eq!(lines[2], "- FAIL min_wall [box]: min_wall msg");
        assert_eq!(lines[3], "- warn overhangs [lid]: overhangs msg");
        assert_eq!(lines[4], "- skipped fit_pairs: fit_pairs msg");
        assert_eq!(lines[5], "- pass: valid_solid");
    }

    /// A requirement Hotend cannot measure (here `opening` and `lid_seat`)
    /// gets its own count and line with the reason, apart from checks that were skipped.
    #[test]
    fn a_requirement_that_cannot_be_checked_is_named_as_such() {
        let row = |status: &str, message: &str, data: Value| {
            json!({"id": "contract", "status": status, "severity": "error",
                   "message": message, "part": "basket", "data": data})
        };
        let result = result(json!({"protocol": 0, "op": "build", "status": "ok",
            "error": null, "checks": [
                row("pass", "met", json!({"requirement": "fits"})),
                row("skipped", "Not checkable: requirement 'opening' (measured) cannot be \
                     checked: a dimension named in words ('inside diameter') has no place",
                    json!({"requirement": "opening", "not_checkable": true,
                           "reason": "no place"})),
                {"id": "parameter_ranges", "status": "skipped", "severity": "warning",
                 "message": "Skipped: range sampling was not requested.", "part": null,
                 "data": {}}]}));
        let text = checks_report(&result, 19);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[1],
            "checks: 0 fail, 0 warn, 1 pass, 1 skipped, 1 not checkable"
        );
        assert!(
            lines[2].starts_with(
                "- NOT CHECKABLE contract [basket]: Not checkable: requirement 'opening'"
            ),
            "{text}"
        );
        assert_eq!(
            lines[3],
            "- skipped parameter_ranges: Skipped: range sampling was not requested."
        );
    }

    #[test]
    fn a_sweep_says_how_each_setting_fared_and_that_nothing_was_stored() {
        let sample = |wall: f64, status: &str, failed: Value, met: bool| {
            json!({"params": {"wall": wall}, "status": status, "failed": failed,
                   "requirements": [
                       {"id": "walls_print", "status": if met {"pass"} else {"fail"},
                        "met": met, "not_checkable": false, "message": "m"},
                       {"id": "cables_pass", "status": "warn", "met": null,
                        "not_checkable": true, "message": "m"}],
                   "parts": {"box": [124.0, 64.0, 40.0]}})
        };
        let result = result(json!({"protocol": 0, "op": "sweep", "status": "ok",
            "error": null, "sweep": {"protocol": 0, "builds": 2, "truncated": true,
            "checks": ["min_wall", "contract"], "samples": [
                sample(1.0, "check_error",
                       json!([{"check": "min_wall", "message": "only 0.60 mm"}]), false),
                sample(2.0, "ok", json!([]), true),
                {"params": {"wall": 3.0}, "status": "not_run"}]}}))
        .sanitize("sweep")
        .unwrap();
        let base = BTreeMap::from([("wall".to_owned(), 2.0), ("width".to_owned(), 120.0)]);
        let text = sweep_report(&result, 2, &base);
        assert!(
            text.starts_with("sweep of version 2: nothing was stored"),
            "{text}"
        );
        assert!(text.contains("version 2: wall=2, width=120\n"), "{text}");
        assert!(
            text.contains("1. wall=1: builds, but an error check FAILS\n   - FAIL min_wall: only 0.60 mm\n   contract: walls_print NOT met; cables_pass not checkable (rests on a guess)\n   parts: box 124 x 64 x 40 mm\n"),
            "{text}"
        );
        assert!(text.contains("2. wall=2: builds and passes the error checks\n"));
        assert!(text.contains("3. wall=3: not built: the time for this call ran out\n"));
        assert!(text.contains("wall: passes at 2; fails at 1\n"), "{text}");
        assert!(text.contains("try fewer settings"));
        assert!(text.contains("partial report: the time for this call ran out after 2 of 3"));
        assert!(text.contains("not a check of a version"));
        // A report out of the protocol's bounds is refused before anything reads it.
        let bad = result_value_with_status("passed");
        assert!(
            serde_json::from_value::<EngineResult>(bad)
                .unwrap()
                .sanitize("sweep")
                .is_err()
        );
    }

    /// A failure message from a model script (an exception text) cannot start lines of its
    /// own in the report: line breaks and control characters are flattened.
    #[test]
    fn sweep_failure_messages_stay_on_their_line() {
        let result = result(json!({"protocol": 0, "op": "sweep", "status": "ok",
            "error": null, "sweep": {"protocol": 0, "builds": 1, "truncated": false,
            "checks": ["min_wall"], "samples": [
                {"params": {"wall": 1.0}, "status": "model_error",
                 "failed": [{"check": "build:ValueError",
                             "message": "bad\n2. wall=9: builds and passes\r\n\u{1b}[2J\tend"}]}]}}))
        .sanitize("sweep")
        .unwrap();
        let text = sweep_report(&result, 1, &BTreeMap::new());
        assert!(
            text.contains("   - FAIL build:ValueError: bad 2. wall=9: builds and passes [2J end\n"),
            "{text}"
        );
        assert!(!text.contains('\u{1b}') && !text.contains('\r') && !text.contains('\t'));
        assert!(
            !text.lines().any(|line| line.starts_with("2. wall=9")),
            "{text}"
        );
    }

    /// A requirement that cannot be checked and rests on a guess comes as a `warn` row with
    /// `data.not_checkable`; it counts as not checkable, not as a warning.
    #[test]
    fn a_guessed_requirement_that_cannot_be_checked_counts_as_not_checkable() {
        let result = result(json!({"protocol": 0, "op": "build", "status": "ok",
            "error": null, "checks": [
                {"id": "contract", "status": "warn", "severity": "error",
                 "message": "Not checkable: Requirement 'cables_pass' (guessed) is not checkable: \
                             no place. It rests on a guess: how thick are the cables?",
                 "part": "box", "data": {"requirement": "cables_pass", "not_checkable": true,
                                         "reason": "no place"}},
                {"id": "overhangs", "status": "warn", "severity": "warning",
                 "message": "overhangs msg", "part": null, "data": {}}]}));
        let text = checks_report(&result, 3);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[1],
            "checks: 0 fail, 1 warn, 0 pass, 0 skipped, 1 not checkable"
        );
        assert_eq!(lines[2], "- warn overhangs: overhangs msg");
        assert!(
            lines[3].starts_with("- NOT CHECKABLE contract [box]: Not checkable:"),
            "{text}"
        );
    }

    fn result_value_with_status(status: &str) -> Value {
        json!({"protocol": 0, "op": "sweep", "status": "ok", "error": null,
               "sweep": {"protocol": 0, "builds": 1, "truncated": false, "checks": [],
                         "samples": [{"params": {"wall": 1.0}, "status": status}]}})
    }

    #[test]
    fn the_profile_list_names_the_settings_and_what_the_slicer_knows() {
        use crate::gateway::{CatalogueEntry, SlicerCatalogue};
        let settings = crate::settings::Settings::default();
        let none = catalogue_report(&settings, None);
        assert!(none.contains("material pla-generic (PLA)"), "{none}");
        assert!(none.contains("build plate not chosen yet"), "{none}");
        assert!(none.contains("0.4 mm (assumed: not set)"), "{none}");
        assert!(none.contains("no list of slicer profiles"), "{none}");
        let catalogue = SlicerCatalogue {
            printer_id: "bambu-x2d".into(),
            processes: vec![CatalogueEntry {
                id: "standard-0.20mm".into(),
                aliases: vec!["demo-0.20mm".into()],
                name: "0.20mm Standard @BBL X2D".into(),
                verified: true,
            }],
            materials: vec![CatalogueEntry {
                id: "petg-basic".into(),
                aliases: Vec::new(),
                name: "Bambu PETG Basic @BBL X2D 0.4 nozzle".into(),
                verified: false,
            }],
            plates: Vec::new(),
        };
        let text = catalogue_report(&settings, Some(&catalogue));
        assert!(
            text.contains("- standard-0.20mm (also demo-0.20mm): 0.20mm Standard @BBL X2D\n"),
            "{text}"
        );
        assert!(text.contains("- petg-basic: Bambu PETG Basic @BBL X2D 0.4 nozzle (name not yet seen on a real installation)"));
        assert!(text.contains("only the maker changes the material"));
    }

    /// A long list of questions: what a fit depends on first, a bounded number, and a line
    /// that says how many are missing. A long build text never cuts them off.
    #[test]
    fn the_questions_a_fit_depends_on_are_listed_first_and_none_is_dropped_silently() {
        let ask =
            |id: &str, text: &str| json!({"requirement": id, "text": text, "guessed_params": []});
        let row = |id: &str, kind: &str| {
            json!({"id": "contract", "status": "warn", "severity": "error", "message": "m",
                   "part": "case", "data": {"kind": "requirement", "requirement": id,
                                            "requirement_kind": kind}})
        };
        let device = json!({"id": "sample_phone", "name": "Phone", "kind": "phone", "frame": "f",
            "size": [1.0, 2.0, 3.0], "provenance": "standard", "sources": {}, "numbers": 1,
            "features": [], "used_features": [], "proven": false, "unverified": [],
            "questions": [], "body": {}});
        let looks: Vec<Value> = (0..20)
            .map(|n| row(&format!("look_{n}"), "wall_min"))
            .collect();
        let mut asked: Vec<Value> = (0..20)
            .map(|n| ask(&format!("look_{n}"), "a style question"))
            .collect();
        asked.push(ask("sample_phone_usb_c", "how wide is the port?"));
        asked.push(ask("holds_it", "how wide is the thing?"));
        asked.push(ask("wall_note", "a wall?"));
        // The longest id the core accepts is shown whole.
        let long_id = format!("reference_{}", "a".repeat(54));
        asked.push(ask(&long_id, "is this the thing?"));
        asked.push(ask("phone_pocket", "print the ring gauge"));
        let value = |asked: &[Value], stdout: String| {
            result(
                json!({"protocol": 0, "op": "build", "status": "ok", "error": null,
                "checks": looks.iter().cloned().chain([row("holds_it", "cavity_min"),
                                               row("wall_note", "wall_min")]).collect::<Vec<_>>(),
                "components": [device], "questions": asked, "stdout": stdout}),
            )
        };
        let text = checks_report(&value(&asked, String::new()), 1);
        let lines: Vec<&str> = text
            .lines()
            .skip_while(|l| !l.starts_with("questions for the user"))
            .collect();
        assert_eq!(
            lines[0],
            "questions for the user (25; ask them, do not answer them yourself):"
        );
        assert_eq!(lines[1], "- [sample_phone_usb_c] how wide is the port?");
        assert_eq!(lines[2], "- [holds_it] how wide is the thing?");
        assert_eq!(lines[3], format!("- [{long_id}] is this the thing?"));
        // A pocket's question about its ring gauge has no requirement row.
        assert_eq!(lines[4], "- [phone_pocket] print the ring gauge");
        assert_eq!(lines[5], "- [look_0] a style question");
        assert_eq!(lines.len(), 2 + MAX_SHOWN_QUESTIONS);
        assert_eq!(
            lines.last().unwrap(),
            &"- and 9 more, not listed here (none of them about a fit): the maker sees all of \
              them in Hotend's list of open questions"
        );
        // Every question fits: no such line.
        let few = checks_report(&value(&asked[..MAX_SHOWN_QUESTIONS], String::new()), 1);
        assert!(!few.contains("more, not listed here"), "{few}");
        assert!(few.contains("- [look_15] "));

        // Without the contract's rows (a partial check run, a crashed contract check) the
        // device's questions, a file's and a pocket's still come before forty walls; a
        // pocket's is known by its having no guessed parameter.
        let wall = |n: usize| {
            json!({"requirement": format!("wall_{n}"), "text": "how thick?",
                   "guessed_params": [format!("wall_{n}")]})
        };
        let mut many: Vec<Value> = (0..40).map(wall).collect();
        many.extend([
            ask("phone_pocket", "print the ring gauge"),
            ask("sample_phone_usb_c", "how wide is the port?"),
            ask("sample_phone", "its size is not verified"),
            ask(&long_id, "is this the thing?"),
            ask("holes", "where are the mounting holes?"),
        ]);
        let crashed = json!({"id": "contract", "status": "fail", "severity": "error",
                             "message": "crashed", "part": null, "data": {}});
        let rows: Vec<Value> = (0..40)
            .map(|n| row(&format!("wall_{n}"), "wall_min"))
            .chain([row("holes", "dimension")])
            .collect();
        // Without rows the kinds of the hidden ones are not known, so it is not said that
        // none is about a fit.
        for (checks, said) in [
            (json!([]), "a fit may depend on 29 of them"),
            (json!([crashed]), "a fit may depend on 29 of them"),
            (json!(rows), "none of them about a fit"),
        ] {
            let text = checks_report(
                &result(
                    json!({"protocol": 0, "op": "build", "status": "ok", "error": null,
                    "checks": checks, "components": [device], "questions": many}),
                ),
                1,
            );
            let mut top: Vec<&str> = text
                .lines()
                .skip_while(|l| !l.starts_with("questions for the user"))
                .skip(1)
                .take(5)
                .map(|l| l.split(']').next().unwrap().trim_start_matches("- ["))
                .collect();
            top.sort_unstable();
            assert_eq!(
                top,
                [
                    "holes",
                    "phone_pocket",
                    &long_id,
                    "sample_phone",
                    "sample_phone_usb_c"
                ],
                "{text}"
            );
            assert!(
                text.contains(&format!("- and 29 more, not listed here ({said})")),
                "{text}"
            );
        }
        // A size that rests on a guessed parameter, behind twenty others and without rows:
        // hidden, and counted as one a fit may depend on.
        let guessed: Vec<Value> = (0..20)
            .map(wall)
            .chain([json!({"requirement": "holds_it", "text": "how wide?",
                           "guessed_params": ["width"]})])
            .collect();
        let said = |checks: Value| {
            checks_report(
                &result(
                    json!({"protocol": 0, "op": "build", "status": "ok", "error": null,
                    "checks": checks, "questions": guessed}),
                ),
                1,
            )
        };
        let text = said(json!([]));
        assert!(
            !text.contains("[holds_it]") && !text.contains("none of them"),
            "{text}"
        );
        assert!(text.contains("- and 5 more, not listed here (a fit may depend on 5 of them)"));
        // With its row it is known, and first.
        for kind in [
            "cavity_min",
            "fit",
            "dimension",
            "outer_max",
            "standoff",
            "reference_fit",
        ] {
            let mut rows: Vec<Value> = (0..20)
                .map(|n| row(&format!("wall_{n}"), "wall_min"))
                .collect();
            rows.push(row("holds_it", kind));
            let text = said(json!(rows));
            assert!(text.contains("yourself):\n- [holds_it] "), "{kind}: {text}");
            assert!(text.contains("- and 5 more, not listed here (none of them about a fit)"));
        }
        // More questions about a fit than lines: the last line says how many are hidden.
        let ports: Vec<Value> = (0..20)
            .map(|n| ask(&format!("sample_phone_port_{n}"), "how wide?"))
            .chain((0..40).map(wall))
            .collect();
        let text = checks_report(
            &result(
                json!({"protocol": 0, "op": "build", "status": "ok", "error": null,
                "components": [device], "questions": ports}),
            ),
            1,
        );
        assert!(
            text.contains("- and 44 more, not listed here (a fit may depend on 44 of them)"),
            "{text}"
        );

        // Long questions: fewer lines, the same bound, and the count says what is missing.
        let long: Vec<Value> = (0..40)
            .map(|n| ask(&format!("look_{n}"), &"word ".repeat(300)))
            .collect();
        let text = checks_report(&value(&long, "x\n".repeat(9000)), 1);
        let from = text.find("questions for the user").unwrap();
        assert!(
            text.len() - from < MAX_QUESTIONS_TEXT + 200,
            "{}",
            text.len() - from
        );
        let listed = text[from..].matches("\n- [").count();
        assert!((8..MAX_SHOWN_QUESTIONS).contains(&listed), "{listed}");
        assert!(text.trim_end().ends_with("open questions"), "{text}");
        assert!(text.contains(&format!("- and {} more", 40 - listed)));
        // A huge script output and a huge check table do not push the questions out.
        let checks: Vec<Value> = (0..200)
            .map(|_| {
                json!({"id": "overhangs", "status": "warn", "severity": "warning",
                       "message": "m".repeat(400), "part": null, "data": {}})
            })
            .collect();
        let flooded = result(
            json!({"protocol": 0, "op": "build", "status": "ok", "error": null,
            "checks": checks, "questions": [ask("holds_it", "how wide?")]}),
        );
        let text = checks_report(&flooded, 1);
        assert!(text.ends_with(
            "questions for the user (1; ask them, do not answer them yourself):\n\
             - [holds_it] how wide?\n"
        ));
        assert!(text.contains("[truncated]") && text.len() <= MAX_RESULT_TEXT + 40);
        // Both long: the check table keeps most of the text, the failure at its head too.
        let mut checks = checks;
        checks.push(
            json!({"id": "min_wall", "status": "fail", "severity": "error",
                           "message": "too thin", "part": null, "data": {}}),
        );
        let both = result(
            json!({"protocol": 0, "op": "build", "status": "ok", "error": null,
            "checks": checks, "questions": long}),
        );
        let text = checks_report(&both, 1);
        let from = text.find("questions for the user").unwrap();
        assert!(text.contains("- FAIL min_wall: too thin"), "{text}");
        assert!(from > 8_500, "the check table got {from} bytes");
        assert!((5..16).contains(&text[from..].matches("\n- [").count()));
        assert!(text.len() <= MAX_RESULT_TEXT + 40 && text.contains(" more, not listed here ("));
    }

    #[test]
    fn long_output_is_bounded() {
        let results: Vec<Value> = (0..64).map(|_| json!({"blob": "x".repeat(5000)})).collect();
        assert!(measure_report(&results).len() <= MAX_RESULT_TEXT + 20);
    }
}
