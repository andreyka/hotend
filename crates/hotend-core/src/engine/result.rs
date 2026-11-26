//! `result.json`: what the engine worker reports. Hostile input.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{PROTOCOL_VERSION, VIEW_NAMES};
use crate::util::{is_model_name, truncate};

const MAX_PARAMS: usize = 64;
const MAX_PARTS: usize = 64;
const MAX_CHECKS: usize = 2048;
const MAX_WARNINGS: usize = 64;
const MAX_RESULTS: usize = 64;
const MAX_DOCS: usize = 20;
const MAX_STDOUT: usize = 20_000;
const MAX_MESSAGE: usize = 4_000;
const MAX_TRACEBACK: usize = 8_000;
const MAX_DOC_TEXT: usize = 4_000;
const MAX_PATH: usize = 200;
/// What a build can ask at most: one question per requirement of a contract (the engine
/// allows 256), two per reference file and one per feature of a device.
const MAX_QUESTIONS: usize = 512;
/// Devices of one build. The engine restores at most as many.
const MAX_COMPONENTS: usize = 32;
/// Features of one device.
const MAX_FEATURES: usize = 64;
/// Numbers of one device that are listed as not verified.
const MAX_UNVERIFIED: usize = 1024;
/// Reference files of one build: the engine accepts no more in a request.
const MAX_REFERENCES: usize = 8;
const MAX_PLATES: usize = 64;
const MAX_PIECES: usize = 256;
const MAX_GAUGES: usize = 32;
/// Largest calibration table accepted from a `calibrate` result, as JSON text.
const MAX_TABLE_BYTES: usize = 256 * 1024;
/// Largest `components` list and largest `references` list, each as JSON text. A phone with
/// every feature used takes about 28 KiB.
const MAX_DEVICE_BYTES: usize = 512 * 1024;
const UNITS: [&str; 4] = ["mm", "deg", "count", "ratio"];
const PROVENANCES: [&str; 5] = ["measured", "standard", "calibrated", "guessed", "style"];
/// The provenance of a parameter whose number was found on the web: it names a research
/// note of the project in `source`. Only parameters can have it.
const WEB_PROVENANCE: &str = "web";

/// How an engine job ended, as the worker reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineStatus {
    Ok,
    /// The model script raised or broke a rule.
    ModelError,
    /// Built, but an error-severity check failed.
    CheckError,
    Timeout,
    InternalError,
}

impl EngineStatus {
    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::ModelError => "model_error",
            Self::CheckError => "check_error",
            Self::Timeout => "timeout",
            Self::InternalError => "internal_error",
        }
    }
}

/// The `error` object of a failed job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineError {
    #[serde(rename = "type")]
    pub kind: String,
    pub message: String,
    /// Line in the model script, when known.
    pub line: Option<u32>,
    /// Traceback trimmed to the model script's own frames.
    #[serde(default)]
    pub traceback: String,
}

/// One parameter's declaration, with `unit` and `label` filled in by the engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParamSpec {
    pub default: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    pub unit: String,
    pub label: String,
    /// `measured`, `standard`, `calibrated`, `guessed`, `style` or `web`.
    pub provenance: String,
    /// With the provenance `web`: the id of the research note the number is from (`r3`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// True for the id of a research note: `r` and one to six digits.
fn is_note_id(text: &str) -> bool {
    text.strip_prefix('r').is_some_and(|digits| {
        (1..=6).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())
    })
}

/// Axis-aligned bounding box.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BBox {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

/// One printed piece of the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartSummary {
    pub name: String,
    /// mm3.
    pub volume: f64,
    /// mm2.
    pub area: f64,
    /// Solid (100 % infill) mass, when the material's density is known.
    pub mass_estimate_g: Option<f64>,
    pub bbox: BBox,
    pub solids: u32,
    pub faces: u32,
    pub is_valid: bool,
    pub is_manifold: bool,
    /// `#rrggbb`, as used in mesh.json and the views.
    pub color: String,
}

/// Outcome of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
    Skipped,
}

/// Whether a failing check blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckSeverity {
    Error,
    Warning,
}

/// One row of the check table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckResult {
    pub id: String,
    pub status: CheckStatus,
    pub severity: CheckSeverity,
    pub message: String,
    pub part: Option<String>,
    /// The numbers behind the message.
    pub data: Value,
}

/// Paths of the files a job wrote, relative to the job folder. They are claims of the child:
/// the job importer resolves and checks every one before use.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifacts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brep: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parts_index: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub views: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threemf: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub stl: BTreeMap<String, String>,
}

/// One documentation excerpt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocEntry {
    pub name: String,
    pub kind: String,
    pub module: String,
    pub signature: String,
    pub doc: String,
    pub score: f64,
}

/// A question for the user that the model's contract leaves open (PROTOCOL.md section 4.1),
/// or that a number found on the web raises (docs/WEB_RESEARCH.md).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Question {
    /// Id of the contract requirement the question is about; `web:<note id>` for a question
    /// about a number found on the web.
    pub requirement: String,
    /// The question, in plain words.
    pub text: String,
    /// The model parameters whose guessed values it rests on.
    pub guessed_params: Vec<String>,
    /// The web number a `web:` question is about. Only the core sets it, from its stored
    /// research notes; in a result of the engine it is dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web: Option<crate::research::WebNumber>,
}

/// A bought device a build was designed around (PROTOCOL.md section 3.3, `components`).
///
/// The numbers come from the data files shipped with the engine, but the list is written by
/// the engine process and which devices and features it names is what the model's process
/// declared. It is kept as a description of the version and nothing is decided by it:
/// `proven` is the claim, the `reference_fit` check is the verdict.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentUse {
    pub id: String,
    pub name: String,
    /// What it is, in a word: `phone`.
    pub kind: String,
    /// Where its origin and axes are, in words.
    pub frame: String,
    /// Width, height and depth, mm.
    pub size: [f64; 3],
    /// Where its body's numbers are from; one of the provenances of a parameter.
    pub provenance: String,
    /// Title, address and kind of every source, by key. Text of the data file, for display.
    pub sources: Value,
    /// How many numbers its data holds.
    pub numbers: u32,
    /// One object per feature (button, port, camera), in the data's order.
    pub features: Vec<Value>,
    /// The features the model relied on.
    pub used_features: Vec<String>,
    /// The engine says that every number the model relied on is verified.
    pub proven: bool,
    /// The numbers the model relied on that are not verified.
    pub unverified: Vec<Value>,
    /// What the user should be asked about it; also part of the result's `questions`.
    pub questions: Vec<Question>,
    /// The measures of its body.
    pub body: Value,
}

/// How print preparation turns one part (PROTOCOL.md section 3.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Orientation {
    pub part: String,
    /// `as_modelled`, `face` or `preferred`.
    pub source: String,
    /// The direction of the part as modelled that points up on the bed.
    pub up: [f64; 3],
    /// In words: `as modelled`, `upside down`, `onto its -X side`, ...
    pub turned: String,
    pub score: f64,
    /// The terms the score is the sum of.
    pub terms: BTreeMap<String, f64>,
    /// What was measured in the chosen orientation.
    pub measures: Value,
    /// The same measures for the part as modelled.
    pub as_modelled: Value,
    /// `the request`, `the model` or nothing.
    pub preferred_by: Option<String>,
    /// Why an orientation that was asked for was not used.
    pub overridden: Option<String>,
    /// The best candidates with their measures.
    pub candidates: Vec<Value>,
    /// One plain sentence.
    pub explanation: String,
    pub copies: u32,
    /// Device extruder id of the nozzle that prints the part.
    pub nozzle_id: i64,
    /// The print facts of the piece in the chosen orientation (the piece object of
    /// `hotend_settings::facts`), when the request asked in the print settings way. Read
    /// strictly only where a rule reads them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facts: Option<Value>,
}

/// One piece on a plate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlateItem {
    /// The part's name, or `<part>.<n>` when it has copies.
    pub name: String,
    pub part: String,
    pub copy: u32,
    pub nozzle_id: i64,
    pub quarter_turn: bool,
    /// 12 numbers, a 3 x 4 matrix row by row: plate = R * model + t.
    pub transform: Vec<f64>,
    /// The piece's box in plate coordinates.
    pub bbox: BBox,
}

/// One plate of a preparation or of a gauge export.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plate {
    /// From 1.
    pub index: u32,
    /// X and Y size of the bed area the nozzles reach.
    pub size: [f64; 2],
    pub items: Vec<PlateItem>,
    /// `plates/<index>/plate.3mf`, relative to the folder that holds the plates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threemf: Option<String>,
    /// `plates/<index>/views/<view>.png` by view name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub views: BTreeMap<String, String>,
    /// The print facts of the pieces of this plate, in the order of `items`: the object the
    /// rules read for the plate (`hotend_settings::facts::PlateFacts`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facts: Option<Value>,
    /// How the plate was laid out, when the request asked in the print settings way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<PlateLayout>,
}

/// How a plate was laid out (PROTOCOL.md section 3.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlateLayout {
    /// Room kept free round every piece, mm.
    pub clearance: f64,
    /// Distance of every grown footprint from the edge of the reach, mm.
    pub margin: f64,
    /// `front_left` or `centre`.
    pub anchor: String,
    /// How far the group was moved from the front left, mm.
    pub offset: [f64; 2],
}

/// A piece that fits no plate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unplaced {
    pub name: String,
    pub part: String,
    pub copy: u32,
    pub reason: String,
    /// One word beside the reason: `clearance`, `keepout`, `too_large` or `no_reach`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
}

/// What to measure on a printed gauge with a caliper.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GaugeDimension {
    pub id: String,
    pub half: String,
    /// The object on the plate that carries the dimension.
    pub object: String,
    /// `hole` (an inside size) or `peg` (an outside size).
    pub feature: String,
    /// The size as drawn, in millimetres.
    pub drawn: f64,
    /// Where to put the caliper, in words.
    pub what: String,
}

/// One clearance a gauge was drawn with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GaugeClearance {
    pub pair: String,
    pub fit_class: String,
    pub gap: f64,
    pub provenance: String,
}

/// The test gauge of one feature card.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gauge {
    pub card: String,
    pub name: String,
    pub args: BTreeMap<String, Value>,
    /// The names of its two objects on the plate.
    pub parts: Vec<String>,
    pub size: [f64; 3],
    pub dimensions: Vec<GaugeDimension>,
    pub clearances: Vec<GaugeClearance>,
    /// The print facts of its pieces, by piece name, when the request asked for them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facts: Option<BTreeMap<String, Value>>,
}

/// One quantity of the calibration table that readings changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalibrationChange {
    pub quantity: String,
    pub before: f64,
    pub after: f64,
    pub sigma: f64,
    pub readings: u32,
}

/// The worker's result for any op. Fields an op does not produce keep their defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineResult {
    pub protocol: u32,
    pub op: Option<String>,
    pub status: EngineStatus,
    pub error: Option<EngineError>,
    #[serde(default)]
    pub timings: BTreeMap<String, f64>,

    // build
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stdout: String,
    #[serde(default)]
    pub stdout_truncated: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, f64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub param_specs: BTreeMap<String, ParamSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<PartSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<CheckResult>,
    // build, export, views
    #[serde(default)]
    pub artifacts: Artifacts,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,

    // measure
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub results: Vec<Value>,

    // docs
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub versions: BTreeMap<String, Option<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub docs: Vec<DocEntry>,

    // build with `checks.contract`, or that used a device or a reference file
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub questions: Option<Vec<Question>>,

    // build
    /// The bought devices the model used.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<ComponentUse>,
    /// One object per reference file of the request. The core's requests list none today;
    /// kept as bounded JSON that nothing reads.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<Value>,

    // prepare
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub orientations: Vec<Orientation>,
    // prepare, gauge in the print settings way
    /// The stage that ran: `all`, `orient` or `plate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    /// The print facts of every piece of the request, copies included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facts: Option<Value>,
    // prepare, gauge
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plates: Vec<Plate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unplaced: Vec<Unplaced>,

    // gauge
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gauges: Vec<Gauge>,

    // calibrate
    /// The whole new calibration table (PROTOCOL.md section 4.2); the core stores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<CalibrationChange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_provenance: Option<String>,

    // sweep
    /// The range sampler's report of a `sweep` request: what the model process claims,
    /// advice for the agent and never a check outcome of a version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sweep: Option<SweepReport>,
}

/// The answer to a `sweep` request (PROTOCOL.md section 3.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SweepReport {
    pub protocol: u32,
    /// Settings that were built.
    pub builds: u32,
    /// Settings were left unbuilt when the time ran out (`not_run`).
    pub truncated: bool,
    /// The checks every setting was judged by.
    pub checks: Vec<String>,
    pub samples: Vec<SweepSample>,
}

/// One setting of a sweep and how it fared.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SweepSample {
    /// The parameters the setting changed; the others kept the version's values.
    pub params: BTreeMap<String, f64>,
    /// `ok`, `check_error`, `model_error` or `not_run`.
    pub status: String,
    /// The error-severity checks that failed (or `build:<error type>`), with the message.
    #[serde(default)]
    pub failed: Vec<SweepFailure>,
    /// Every requirement of the contract as measured.
    #[serde(default)]
    pub requirements: Vec<SweepRequirement>,
    /// The size (x, y, z) of every part, mm.
    #[serde(default)]
    pub parts: BTreeMap<String, [f64; 3]>,
}

/// A check of a sweep setting that failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SweepFailure {
    pub check: String,
    pub message: String,
}

/// One requirement of the contract as a sweep setting measured it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SweepRequirement {
    pub id: String,
    pub status: CheckStatus,
    pub met: Option<bool>,
    #[serde(default)]
    pub not_checkable: bool,
    pub message: String,
}

/// The statuses a sweep setting can have.
pub const SWEEP_STATUSES: [&str; 4] = ["ok", "check_error", "model_error", "not_run"];

/// Why a parsed result was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InvalidResult(pub String);

fn invalid<T>(reason: impl Into<String>) -> Result<T, InvalidResult> {
    Err(InvalidResult(reason.into()))
}

impl EngineResult {
    /// Checks the structural rules of the protocol and bounds every free-text field.
    ///
    /// Names, counts, enumerations and artefact path syntax that break the protocol make the
    /// whole result invalid. Free text the model script controls (messages, tracebacks,
    /// stdout, labels) is cut to a fixed length instead, because a long exception message is
    /// a legitimate outcome of a broken script.
    pub fn sanitize(mut self, expected_op: &str) -> Result<Self, InvalidResult> {
        if self.protocol != PROTOCOL_VERSION {
            return invalid(format!("protocol {} is not supported", self.protocol));
        }
        match self.op.as_deref() {
            Some(op) if op == expected_op => {}
            // A request the worker could not parse has no op yet.
            None if self.status == EngineStatus::InternalError => {}
            other => {
                return invalid(format!(
                    "the result is for op {other:?}, expected {expected_op:?}"
                ));
            }
        }
        let failed = matches!(
            self.status,
            EngineStatus::ModelError | EngineStatus::InternalError
        );
        if failed != self.error.is_some() && self.status != EngineStatus::Timeout {
            return invalid("status and error do not agree");
        }
        if let Some(error) = &mut self.error {
            error.kind = truncate(&error.kind, 120);
            error.message = truncate(&error.message, MAX_MESSAGE);
            error.traceback = truncate(&error.traceback, MAX_TRACEBACK);
        }
        if self.timings.len() > 32 || self.timings.keys().any(|k| k.len() > 32) {
            return invalid("timings are out of bounds");
        }
        self.stdout = truncate(&self.stdout, MAX_STDOUT);

        if self.params.len() > MAX_PARAMS || self.param_specs.len() > MAX_PARAMS {
            return invalid("too many parameters");
        }
        if let Some(name) = self
            .params
            .keys()
            .chain(self.param_specs.keys())
            .find(|name| !is_model_name(name))
        {
            return invalid(format!(
                "parameter name {:?} is not allowed",
                truncate(name, 40)
            ));
        }
        for spec in self.param_specs.values_mut() {
            let web = spec.provenance == WEB_PROVENANCE;
            if !UNITS.contains(&spec.unit.as_str())
                || !(web || PROVENANCES.contains(&spec.provenance.as_str()))
            {
                return invalid("a parameter has an unknown unit or provenance");
            }
            // A web number names its note, and only a web number does.
            if spec.source.as_deref().is_some_and(is_note_id) != web
                || (!web && spec.source.is_some())
            {
                return invalid("a parameter's `source` and its provenance do not go together");
            }
            if spec.label.chars().count() > 80 {
                spec.label = spec.label.chars().take(80).collect();
            }
        }

        if self.parts.len() > MAX_PARTS {
            return invalid("too many parts");
        }
        let mut names = BTreeSet::new();
        for part in &self.parts {
            if !is_model_name(&part.name) || !names.insert(part.name.as_str()) {
                return invalid(format!(
                    "part name {:?} is not allowed or repeated",
                    truncate(&part.name, 40)
                ));
            }
            if !is_color(&part.color) {
                return invalid("a part colour is not #rrggbb");
            }
        }

        if self.checks.len() > MAX_CHECKS {
            return invalid("too many check results");
        }
        for check in &mut self.checks {
            let id_ok = !check.id.is_empty()
                && check.id.len() <= 64
                && check.id.chars().all(|c| c.is_ascii_lowercase() || c == '_');
            if !id_ok || check.part.as_deref().is_some_and(|p| !is_model_name(p)) {
                return invalid("a check has an invalid id or part name");
            }
            check.message = truncate(&check.message, MAX_MESSAGE);
        }

        if self.warnings.len() > MAX_WARNINGS {
            return invalid("too many warnings");
        }
        for warning in &mut self.warnings {
            *warning = truncate(warning, MAX_MESSAGE);
        }

        let artifacts = &self.artifacts;
        let single = [
            &artifacts.step,
            &artifacts.brep,
            &artifacts.parts_index,
            &artifacts.mesh,
            &artifacts.threemf,
        ];
        let paths = single
            .into_iter()
            .flatten()
            .chain(artifacts.views.values())
            .chain(artifacts.stl.values());
        for path in paths {
            if path.is_empty() || path.len() > MAX_PATH {
                return invalid("an artefact path is empty or too long");
            }
        }
        if artifacts.views.len() > VIEW_NAMES.len()
            || artifacts
                .views
                .keys()
                .any(|v| !VIEW_NAMES.contains(&v.as_str()))
        {
            return invalid("an unknown view name was reported");
        }
        if artifacts.stl.len() > MAX_PARTS || artifacts.stl.keys().any(|p| !is_model_name(p)) {
            return invalid("an STL artefact names an invalid part");
        }

        if self.results.len() > MAX_RESULTS || self.results.iter().any(|r| !r.is_object()) {
            return invalid("measure results are out of bounds");
        }
        if self.docs.len() > MAX_DOCS || self.versions.len() > 8 {
            return invalid("docs results are out of bounds");
        }
        for doc in &mut self.docs {
            doc.name = truncate(&doc.name, 200);
            doc.kind = truncate(&doc.kind, 40);
            doc.module = truncate(&doc.module, 200);
            doc.signature = truncate(&doc.signature, MAX_DOC_TEXT);
            doc.doc = truncate(&doc.doc, MAX_DOC_TEXT);
        }
        self.sanitize_sweep()?;
        self.sanitize_device_fields()?;
        self.sanitize_plate_fields()?;
        Ok(self)
    }

    /// The report of a `sweep`: every count, name and status within the protocol's
    /// bounds, every message cut to 300 characters.
    fn sanitize_sweep(&mut self) -> Result<(), InvalidResult> {
        let Some(report) = &mut self.sweep else {
            return Ok(());
        };
        let short_id = |id: &str| {
            !id.is_empty()
                && id.len() <= 64
                && id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
        };
        if report.protocol != PROTOCOL_VERSION
            || report.samples.is_empty()
            || report.samples.len() > super::request::MAX_SWEEP_SETTINGS
            || report.builds as usize > report.samples.len()
            || report.checks.len() > 16
            || !report.checks.iter().all(|id| short_id(id))
        {
            return invalid("the sweep report is out of bounds");
        }
        for sample in &mut report.samples {
            let names_ok = sample.params.len() <= 8
                && sample.params.keys().all(|name| is_model_name(name))
                && sample.params.values().all(|value| value.is_finite());
            let parts_ok = sample.parts.len() <= MAX_PARTS
                && sample.parts.keys().all(|name| is_model_name(name))
                && sample.parts.values().flatten().all(|size| size.is_finite());
            if !names_ok
                || !parts_ok
                || !SWEEP_STATUSES.contains(&sample.status.as_str())
                || sample.failed.len() > 16
                || sample.requirements.len() > 32
                || !sample.failed.iter().all(|failure| short_id(&failure.check))
                || !sample
                    .requirements
                    .iter()
                    .all(|requirement| short_id(&requirement.id))
            {
                return invalid("a sweep setting is out of bounds");
            }
            for failure in &mut sample.failed {
                failure.message = truncate(&failure.message, 300);
            }
            for requirement in &mut sample.requirements {
                requirement.message = truncate(&requirement.message, 300);
            }
        }
        Ok(())
    }

    /// The devices and reference files of a build. Counts, names, enumerations and the size
    /// of the lists as JSON text refuse the result when they are out of bounds; the short
    /// texts are cut. The nested lists stay JSON: they are shown, never acted on.
    fn sanitize_device_fields(&mut self) -> Result<(), InvalidResult> {
        if self.components.len() > MAX_COMPONENTS || self.references.len() > MAX_REFERENCES {
            return invalid("too many components or references");
        }
        let bytes = |text: Result<String, serde_json::Error>| text.map_or(usize::MAX, |t| t.len());
        if bytes(serde_json::to_string(&self.components)) > MAX_DEVICE_BYTES
            || bytes(serde_json::to_string(&self.references)) > MAX_DEVICE_BYTES
        {
            return invalid("the components or references are too large");
        }
        if !self.references.iter().all(Value::is_object) {
            return invalid("a reference is not an object");
        }
        let mut ids = BTreeSet::new();
        for component in &mut self.components {
            if !is_model_name(&component.id) || !ids.insert(component.id.clone()) {
                return invalid("a component's id is not allowed or repeated");
            }
            if !PROVENANCES.contains(&component.provenance.as_str())
                || !component.size.iter().all(|v| v.is_finite() && *v > 0.0)
            {
                return invalid("a component has an invalid provenance or size");
            }
            if component.features.len() > MAX_FEATURES
                || component.used_features.len() > MAX_FEATURES
                || component.unverified.len() > MAX_UNVERIFIED
                || component.used_features.iter().any(|f| !is_model_name(f))
            {
                return invalid("the features of a component are out of bounds");
            }
            if !component.sources.is_object()
                || !component.body.is_object()
                || !component.features.iter().all(Value::is_object)
                || !component.unverified.iter().all(Value::is_object)
            {
                return invalid("a component is not described by objects");
            }
            component.name = truncate(&component.name, 200);
            component.kind = truncate(&component.kind, 40);
            component.frame = truncate(&component.frame, MAX_MESSAGE);
            sanitize_questions(&mut component.questions)?;
        }
        Ok(())
    }

    /// The fields of the contract, `prepare`, `gauge` and `calibrate` results.
    fn sanitize_plate_fields(&mut self) -> Result<(), InvalidResult> {
        if let Some(questions) = &mut self.questions {
            sanitize_questions(questions)?;
        }

        if self.orientations.len() > MAX_PARTS {
            return invalid("too many orientations");
        }
        for orientation in &mut self.orientations {
            if !is_model_name(&orientation.part)
                || !["as_modelled", "face", "preferred"].contains(&orientation.source.as_str())
                || orientation.terms.len() > 16
                || orientation.terms.keys().any(|key| key.len() > 32)
                || orientation.candidates.len() > 16
                || !(1..=MAX_PIECES as u32).contains(&orientation.copies)
            {
                return invalid("an orientation is out of bounds");
            }
            orientation.turned = truncate(&orientation.turned, 200);
            orientation.explanation = truncate(&orientation.explanation, MAX_MESSAGE);
            for text in [&mut orientation.preferred_by, &mut orientation.overridden]
                .into_iter()
                .flatten()
            {
                *text = truncate(text, MAX_MESSAGE);
            }
        }

        if self.plates.len() > MAX_PLATES || self.unplaced.len() > MAX_PIECES {
            return invalid("too many plates or unplaced pieces");
        }
        let mut pieces = 0usize;
        for (position, plate) in self.plates.iter().enumerate() {
            // The core builds every plate path itself from the index, so the index must be
            // the plate's position and the reported paths must be the fixed ones.
            if plate.index as usize != position + 1 {
                return invalid("plates are not numbered from 1 in order");
            }
            pieces += plate.items.len();
            if pieces > MAX_PIECES {
                return invalid("too many pieces on the plates");
            }
            for item in &plate.items {
                if !is_piece_name(&item.name)
                    || !is_piece_name(&item.part)
                    || item.transform.len() != 12
                    || item.transform.iter().any(|v| !v.is_finite())
                {
                    return invalid("a plate item is out of bounds");
                }
            }
            let expected = format!("plates/{}/plate.3mf", plate.index);
            if plate.threemf.as_ref().is_some_and(|path| *path != expected) {
                return invalid("a plate file was not reported at its fixed path");
            }
            if plate.views.len() > VIEW_NAMES.len() {
                return invalid("a plate reports too many views");
            }
            for (view, path) in &plate.views {
                if !VIEW_NAMES.contains(&view.as_str())
                    || *path != format!("plates/{}/views/{view}.png", plate.index)
                {
                    return invalid("a plate view was not reported at its fixed path");
                }
            }
        }
        for piece in &mut self.unplaced {
            if !is_piece_name(&piece.name) || !is_piece_name(&piece.part) {
                return invalid("an unplaced piece has an invalid name");
            }
            piece.reason = truncate(&piece.reason, MAX_MESSAGE);
        }

        if self.gauges.len() > MAX_GAUGES {
            return invalid("too many gauges");
        }
        for gauge in &mut self.gauges {
            if !is_model_name(&gauge.card)
                || !is_piece_name(&gauge.name)
                || gauge.parts.len() > 8
                || gauge.parts.iter().any(|part| !is_piece_name(part))
                || gauge.args.len() > 32
                || gauge.dimensions.len() > 16
                || gauge.clearances.len() > 64
            {
                return invalid("a gauge is out of bounds");
            }
            for dimension in &mut gauge.dimensions {
                if !is_model_name(&dimension.id)
                    || !is_model_name(&dimension.half)
                    || !is_piece_name(&dimension.object)
                    || !["hole", "peg"].contains(&dimension.feature.as_str())
                    || !dimension.drawn.is_finite()
                {
                    return invalid("a gauge dimension is out of bounds");
                }
                dimension.what = truncate(&dimension.what, 400);
            }
            for clearance in &mut gauge.clearances {
                clearance.pair = truncate(&clearance.pair, 64);
                clearance.fit_class = truncate(&clearance.fit_class, 32);
                clearance.provenance = truncate(&clearance.provenance, 32);
            }
        }

        self.sanitize_print_settings_fields()?;

        if let Some(table) = &self.table
            && (!table.is_object() || table.to_string().len() > MAX_TABLE_BYTES)
        {
            return invalid("the calibration table is not an object or is too large");
        }
        if self.changes.len() > 16 {
            return invalid("too many calibration changes");
        }
        for change in &mut self.changes {
            change.quantity = truncate(&change.quantity, 40);
        }
        if let Some(provenance) = &self.entry_provenance
            && !PROVENANCES.contains(&provenance.as_str())
        {
            return invalid("the calibration entry has an unknown provenance");
        }
        Ok(())
    }
}

impl EngineResult {
    /// The fields of the print settings interface (PROTOCOL.md section 3.5): the stage, the
    /// print facts, the layout of each plate, the cause of an unplaced piece and the
    /// `keepout` check. Facts are only bounded here (objects, at most
    /// [`hotend_settings::facts::MAX_FACTS_BYTES`] of text in all); they are read strictly,
    /// fact by fact, where a rule reads them, and refused there with `facts_invalid`.
    fn sanitize_print_settings_fields(&mut self) -> Result<(), InvalidResult> {
        if let Some(stage) = &self.stage
            && !STAGES.contains(&stage.as_str())
        {
            return invalid("the result names an unknown stage");
        }
        let mut budget = FactsBudget(0);
        if let Some(facts) = &self.facts {
            budget.take(facts)?;
        }
        for orientation in &self.orientations {
            if let Some(facts) = &orientation.facts {
                budget.take(facts)?;
            }
        }
        for plate in &self.plates {
            if let Some(facts) = &plate.facts {
                budget.take(facts)?;
            }
            if let Some(layout) = &plate.layout {
                let length = |v: f64, max: f64| v.is_finite() && (0.0..=max).contains(&v);
                if !length(layout.clearance, MAX_CLEARANCE_MM)
                    || !length(layout.margin, MAX_LAYOUT_MM)
                    || !ANCHORS.contains(&layout.anchor.as_str())
                    || layout
                        .offset
                        .iter()
                        .any(|v| !v.is_finite() || v.abs() > MAX_LAYOUT_MM)
                {
                    return invalid("a plate layout is out of bounds");
                }
            }
        }
        for gauge in &self.gauges {
            if let Some(facts) = &gauge.facts {
                if facts.len() > 8 || facts.keys().any(|name| !is_piece_name(name)) {
                    return invalid("the facts of a gauge name an invalid piece");
                }
                for value in facts.values() {
                    budget.take(value)?;
                }
            }
        }
        if self
            .unplaced
            .iter()
            .filter_map(|piece| piece.cause.as_deref())
            .any(|cause| !CAUSES.contains(&cause))
        {
            return invalid("an unplaced piece names an unknown cause");
        }
        for check in &self.checks {
            if check.id == "keepout" && !keepout_data_ok(check) {
                return invalid("the keepout check is out of bounds");
            }
        }
        Ok(())
    }
}

/// The questions of a result or of one of its components.
fn sanitize_questions(questions: &mut [Question]) -> Result<(), InvalidResult> {
    if questions.len() > MAX_QUESTIONS {
        return invalid("too many questions");
    }
    for question in questions {
        if !is_question_id(&question.requirement)
            || question.guessed_params.len() > MAX_PARAMS
            || question.guessed_params.iter().any(|p| !is_model_name(p))
        {
            return invalid("a question names an invalid requirement or parameter");
        }
        question.text = truncate(&question.text, MAX_MESSAGE);
        // Only the core says what a web number is.
        question.web = None;
    }
    Ok(())
}

/// True for what a question is about: the characters of a model name, at most 64 of them,
/// because the id of a question about a reference file is `reference_` and the file's name.
/// The core's own ids (`web:<note>`) are outside the pattern.
fn is_question_id(id: &str) -> bool {
    id.len() <= 64
        && id.starts_with(|c: char| c.is_ascii_lowercase())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Stages a result may name.
const STAGES: [&str; 3] = ["all", "orient", "plate"];
/// Anchors a plate layout may name.
const ANCHORS: [&str; 2] = ["front_left", "centre"];
/// Causes an unplaced piece may name.
const CAUSES: [&str; 4] = ["clearance", "keepout", "too_large", "no_reach"];
/// The largest clearance a request may ask for, mm.
const MAX_CLEARANCE_MM: f64 = 20.0;
/// The largest margin or offset a layout may state, mm.
const MAX_LAYOUT_MM: f64 = 1000.0;

/// Counts the text of the facts of one result.
struct FactsBudget(usize);

impl FactsBudget {
    fn take(&mut self, facts: &Value) -> Result<(), InvalidResult> {
        if !facts.is_object() {
            return invalid("print facts are not an object");
        }
        self.0 += facts.to_string().len();
        if self.0 > hotend_settings::facts::MAX_FACTS_BYTES {
            return invalid("the print facts are too large");
        }
        Ok(())
    }
}

/// True for the id of a keep-out zone: `^[a-z][a-z0-9_]{0,31}$`.
pub(crate) fn is_keepout_id(id: &str) -> bool {
    let mut chars = id.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && id.len() <= 32
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// The `keepout` check: severity `error`, no part, and `data` of
/// `{clearance, zones: [{id, min, max}], collisions: [{piece, plate, zone}]}` in bounds.
fn keepout_data_ok(check: &CheckResult) -> bool {
    let data = &check.data;
    let point = |value: &Value| {
        value.as_array().is_some_and(|xy| {
            xy.len() == 2
                && xy.iter().all(|v| {
                    v.as_f64()
                        .is_some_and(|v| v.is_finite() && v.abs() <= MAX_LAYOUT_MM)
                })
        })
    };
    let zones_ok = data["zones"].as_array().is_some_and(|zones| {
        zones.len() <= super::MAX_KEEPOUTS
            && zones.iter().all(|zone| {
                zone.as_object().is_some_and(|object| object.len() == 3)
                    && zone["id"].as_str().is_some_and(is_keepout_id)
                    && point(&zone["min"])
                    && point(&zone["max"])
            })
    });
    let collisions_ok = data["collisions"].as_array().is_some_and(|collisions| {
        collisions.len() <= MAX_PIECES * super::MAX_KEEPOUTS
            && collisions.iter().all(|collision| {
                collision
                    .as_object()
                    .is_some_and(|object| object.len() == 3)
                    && collision["piece"].as_str().is_some_and(is_piece_name)
                    && collision["plate"]
                        .as_u64()
                        .is_some_and(|plate| (1..=MAX_PLATES as u64).contains(&plate))
                    && collision["zone"].as_str().is_some_and(is_keepout_id)
            })
    });
    let clearance_ok = data["clearance"]
        .as_f64()
        .is_some_and(|v| v.is_finite() && (0.0..=MAX_CLEARANCE_MM).contains(&v));
    check.severity == CheckSeverity::Error
        && check.part.is_none()
        && data.as_object().is_some_and(|object| object.len() == 3)
        && clearance_ok
        && zones_ok
        && collisions_ok
}

/// True for the name of a piece on a plate: a part name, a copy `<part>.<n>` or a gauge
/// half `<card name>.<half>`.
fn is_piece_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && name.split('.').count() <= 3
        && name.split('.').all(|piece| {
            !piece.is_empty()
                && piece
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        })
}

fn is_color(text: &str) -> bool {
    text.len() == 7 && text.starts_with('#') && text[1..].chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    fn minimal() -> Value {
        json!({"protocol": 0, "op": "build", "status": "ok", "error": null})
    }

    fn parse(value: Value) -> Result<EngineResult, String> {
        serde_json::from_value::<EngineResult>(value)
            .map_err(|e| e.to_string())?
            .sanitize("build")
            .map_err(|e| e.0)
    }

    fn with(key: &str, value: Value) -> Value {
        let mut base = minimal();
        base[key] = value;
        base
    }

    #[test]
    fn a_minimal_result_is_accepted() {
        let result = parse(minimal()).unwrap();
        assert_eq!(result.status, EngineStatus::Ok);
        assert!(result.parts.is_empty());
    }

    #[test]
    fn unknown_fields_are_rejected_at_every_level() {
        assert!(
            parse(with("surprise", json!(1)))
                .unwrap_err()
                .contains("unknown field")
        );
        assert!(parse(with("artifacts", json!({"exe": "x"}))).is_err());
        let part = json!({"name": "a", "volume": 1.0, "area": 1.0, "mass_estimate_g": null,
            "bbox": {"min": [0, 0, 0], "max": [1, 1, 1]}, "solids": 1, "faces": 6,
            "is_valid": true, "is_manifold": true, "color": "#4c78a8"});
        assert!(parse(with("parts", json!([part.clone()]))).is_ok());
        let mut extra = part;
        extra["script"] = json!("x");
        assert!(parse(with("parts", json!([extra]))).is_err());
    }

    #[test]
    fn protocol_op_and_status_must_be_coherent() {
        assert!(parse(with("protocol", json!(1))).is_err());
        assert!(parse(with("op", json!("measure"))).is_err());
        assert!(parse(with("status", json!("great"))).is_err());
        assert!(
            parse(with("status", json!("model_error")))
                .unwrap_err()
                .contains("agree")
        );
        let error = json!({"type": "ValueError", "message": "m", "line": 3, "traceback": "t"});
        assert!(parse(with("error", error.clone())).is_err());
        let mut failed = with("error", error);
        failed["status"] = json!("model_error");
        assert_eq!(parse(failed).unwrap().error.unwrap().line, Some(3));
    }

    #[test]
    fn names_are_validated_and_free_text_is_bounded() {
        assert!(parse(with("params", json!({"../x": 1.0}))).is_err());
        assert!(parse(with("params", json!({"width": "wide"}))).is_err());
        let part = |name: &str, color: &str| {
            json!({"name": name, "volume": 1.0, "area": 1.0, "mass_estimate_g": 1.0,
            "bbox": {"min": [0, 0, 0], "max": [1, 1, 1]}, "solids": 1, "faces": 6,
            "is_valid": true, "is_manifold": true, "color": color})
        };
        assert!(parse(with("parts", json!([part("<img>", "#000000")]))).is_err());
        assert!(parse(with("parts", json!([part("a", "red")]))).is_err());
        assert!(
            parse(with(
                "parts",
                json!([part("a", "#000000"), part("a", "#000000")])
            ))
            .is_err()
        );
        assert!(parse(with("artifacts", json!({"views": {"selfie": "x.png"}}))).is_err());
        assert!(parse(with("artifacts", json!({"stl": {"../a": "x.stl"}}))).is_err());

        let mut noisy = with(
            "error",
            json!({"type": "E", "message": "m".repeat(100_000), "line": null,
                   "traceback": "t".repeat(100_000)}),
        );
        noisy["status"] = json!("model_error");
        noisy["stdout"] = json!("s".repeat(100_000));
        let result = parse(noisy).unwrap();
        let error = result.error.unwrap();
        assert!(error.message.len() < MAX_MESSAGE + 40);
        assert!(error.traceback.len() < MAX_TRACEBACK + 40);
        assert!(result.stdout.len() < MAX_STDOUT + 40);
    }

    #[test]
    fn plate_paths_are_fixed_and_piece_names_are_checked() {
        let item = |name: &str| {
            json!({"name": name, "part": "lid", "copy": 1, "nozzle_id": 1, "quarter_turn": false,
                "transform": [1, 0, 0, 2, 0, 1, 0, 2, 0, 0, 1, 0],
                "bbox": {"min": [2, 2, 0], "max": [12, 12, 3]}})
        };
        let plate = |index: u32, path: &str, name: &str| {
            json!({"index": index, "size": [256, 256], "items": [item(name)], "threemf": path,
                "views": {"top": format!("plates/{index}/views/top.png")}})
        };
        let good = with("plates", json!([plate(1, "plates/1/plate.3mf", "lid.2")]));
        assert_eq!(parse(good).unwrap().plates[0].items[0].name, "lid.2");
        for bad in [
            plate(1, "../plate.3mf", "lid"),
            plate(1, "plates/2/plate.3mf", "lid"),
            plate(2, "plates/2/plate.3mf", "lid"),
            plate(1, "plates/1/plate.3mf", "../lid"),
            plate(1, "plates/1/plate.3mf", "Lid"),
        ] {
            assert!(
                parse(with("plates", json!([bad.clone()]))).is_err(),
                "{bad}"
            );
        }
        let mut wrong_view = plate(1, "plates/1/plate.3mf", "lid");
        wrong_view["views"] = json!({"top": "plates/1/views/iso.png"});
        assert!(parse(with("plates", json!([wrong_view]))).is_err());

        let questions = json!([{"requirement": "holds_strip", "text": "t".repeat(100_000),
            "guessed_params": ["width"]}]);
        let parsed = parse(with("questions", questions)).unwrap();
        assert!(parsed.questions.unwrap()[0].text.len() < MAX_MESSAGE + 40);
        let bad = json!([{"requirement": "../x", "text": "t", "guessed_params": []}]);
        assert!(parse(with("questions", bad)).is_err());
        assert!(parse(with("table", json!("not a table"))).is_err());
        assert!(parse(with("entry_provenance", json!("made up"))).is_err());
    }

    #[test]
    fn the_print_settings_fields_are_accepted_in_bounds_and_refused_outside() {
        let item = json!({"name": "ring", "part": "ring", "copy": 1, "nozzle_id": 1,
            "quarter_turn": false, "transform": [1, 0, 0, 2, 0, 1, 0, 2, 0, 0, 1, 0],
            "bbox": {"min": [2, 2, 0], "max": [12, 12, 3]}});
        let facts = json!({"family": "PLA", "piece_count": 1, "total_contact_mm2": 1.0,
            "pieces": [{"contact_mm2": 1.0}]});
        let layout = json!({"clearance": 6.0, "margin": 5.0, "anchor": "centre",
            "offset": [80.5, 90.0]});
        let keepout = |collisions: Value| {
            json!({"id": "keepout", "status": "pass", "severity": "error", "message": "m",
                "part": null, "data": {"clearance": 6.0,
                    "zones": [{"id": "start_load_line", "min": [55.0, 0.0], "max": [145.0, 8.0]}],
                    "collisions": collisions}})
        };
        let result = |plate: Value, check: Value, stage: &str| {
            let mut value = minimal();
            value["op"] = json!("prepare");
            value["stage"] = json!(stage);
            value["facts"] = facts.clone();
            value["plates"] = json!([plate]);
            value["checks"] = json!([check]);
            value["unplaced"] = json!([{"name": "big", "part": "big", "copy": 1,
                "reason": "r", "cause": "clearance"}]);
            serde_json::from_value::<EngineResult>(value)
                .map_err(|e| e.to_string())?
                .sanitize("prepare")
                .map_err(|e| e.0)
        };
        let plate = json!({"index": 1, "size": [256, 256], "items": [item],
            "facts": facts, "layout": layout});
        let good = result(plate.clone(), keepout(json!([])), "plate").unwrap();
        assert_eq!(good.stage.as_deref(), Some("plate"));
        assert_eq!(good.plates[0].layout.as_ref().unwrap().anchor, "centre");
        assert_eq!(good.unplaced[0].cause.as_deref(), Some("clearance"));
        let collision = json!([{"piece": "ring", "plate": 1, "zone": "start_load_line"}]);
        assert!(result(plate.clone(), keepout(collision), "plate").is_ok());
        assert!(result(plate.clone(), keepout(json!([])), "later").is_err());
        let mut bad_layout = plate.clone();
        bad_layout["layout"]["clearance"] = json!(25.0);
        assert!(result(bad_layout, keepout(json!([])), "plate").is_err());
        let mut bad_anchor = plate.clone();
        bad_anchor["layout"]["anchor"] = json!("middle");
        assert!(result(bad_anchor, keepout(json!([])), "plate").is_err());
        let mut extra = plate.clone();
        extra["layout"]["rotate"] = json!(true);
        assert!(result(extra, keepout(json!([])), "plate").is_err());
        let mut not_object = plate.clone();
        not_object["facts"] = json!([1, 2]);
        assert!(result(not_object, keepout(json!([])), "plate").is_err());
        let mut huge = plate.clone();
        huge["facts"]["padding"] = json!("x".repeat(hotend_settings::facts::MAX_FACTS_BYTES));
        assert!(result(huge, keepout(json!([])), "plate").is_err());
        for bad in [
            json!([{"piece": "../x", "plate": 1, "zone": "start_load_line"}]),
            json!([{"piece": "ring", "plate": 0, "zone": "start_load_line"}]),
            json!([{"piece": "ring", "plate": 1, "zone": "Load Line"}]),
            json!([{"piece": "ring", "plate": 1, "zone": "z", "note": "x"}]),
        ] {
            assert!(
                result(plate.clone(), keepout(bad.clone()), "plate").is_err(),
                "{bad}"
            );
        }
        let mut warning = keepout(json!([]));
        warning["severity"] = json!("warning");
        assert!(result(plate.clone(), warning, "plate").is_err());
        let mut value = minimal();
        value["op"] = json!("prepare");
        value["unplaced"] = json!([{"name": "big", "part": "big", "copy": 1, "reason": "r",
            "cause": "bad luck"}]);
        let parsed = serde_json::from_value::<EngineResult>(value).unwrap();
        assert!(parsed.sanitize("prepare").is_err());
    }

    #[test]
    fn a_web_number_names_its_note_and_only_a_parameter_can_be_one() {
        let spec = |provenance: &str, source: Option<&str>| {
            let mut spec = json!({"default": 3, "unit": "mm", "label": "W",
                                  "provenance": provenance});
            if let Some(source) = source {
                spec["source"] = json!(source);
            }
            with("param_specs", json!({"w": spec}))
        };
        let parsed = parse(spec("web", Some("r12"))).unwrap();
        assert_eq!(parsed.param_specs["w"].source.as_deref(), Some("r12"));
        for (provenance, source) in [
            ("web", None),
            ("web", Some("")),
            ("web", Some("https://evil.example/")),
            ("web", Some("r")),
            ("web", Some("r1234567")),
            ("measured", Some("r1")),
            ("guessed", Some("anything")),
        ] {
            assert!(
                parse(spec(provenance, source)).is_err(),
                "{provenance} {source:?}"
            );
        }
        // A calibration entry is never a web number.
        assert!(parse(with("entry_provenance", json!("web"))).is_err());
        // What a web question is about comes from the core's notes, never from a result.
        let question = json!([{"requirement": "holds", "text": "t", "guessed_params": [],
            "web": {"note": "r1", "key": "k", "label": "", "value": 1.0, "unit": "mm",
                    "url": "https://evil.example/", "host": "evil.example", "title": "",
                    "quote": "", "locator": "", "evidence": "page_text", "state": "confirmed",
                    "params": [], "built": null, "text": ""}}]);
        let parsed = parse(with("questions", question)).unwrap();
        assert_eq!(parsed.questions.unwrap()[0].web, None);
        let named_like_the_core = json!([{"requirement": "web:r1", "text": "t",
                                          "guessed_params": []}]);
        assert!(parse(with("questions", named_like_the_core)).is_err());
    }

    #[test]
    fn a_device_of_a_build_is_bounded_and_decides_nothing() {
        let question = |id: &str| json!({"requirement": id, "text": "t", "guessed_params": []});
        let phone = json!({"id": "sample_phone", "name": "Sample phone (invented)",
            "kind": "phone", "frame": "f".repeat(100_000), "size": [80.0, 130.0, 10.0],
            "provenance": "standard", "sources": {"a": {"title": "t", "url": "u", "kind": "k"}},
            "numbers": 191, "features": [{"id": "usb_c"}], "used_features": ["usb_c"],
            "proven": false, "unverified": [{"path": "features.usb_c"}],
            "questions": [question("sample_phone_usb_c")], "body": {"name": "sample_phone"}});
        let build = |components: Value| parse(with("components", components));
        let parsed = build(json!([phone])).unwrap();
        assert_eq!(parsed.status, EngineStatus::Ok);
        assert!(parsed.components[0].frame.len() < MAX_MESSAGE + 40);
        assert_eq!(
            parsed.questions, None,
            "the engine lists the questions itself"
        );
        assert!(build(json!([])).unwrap().components.is_empty());

        let many = |id: fn(usize) -> String, count: usize| {
            Value::Array(
                (0..count)
                    .map(|n| {
                        let mut one = phone.clone();
                        one["id"] = json!(id(n));
                        one["frame"] = json!("f");
                        one
                    })
                    .collect(),
            )
        };
        assert!(build(many(|n| format!("c{n}"), MAX_COMPONENTS)).is_ok());
        assert!(build(many(|n| format!("c{n}"), MAX_COMPONENTS + 1)).is_err());
        assert!(build(many(|_| "twice".into(), 2)).is_err());
        let over = |what: &str, value: Value| {
            let mut bad = phone.clone();
            bad[what] = value;
            assert!(build(json!([bad])).is_err(), "{what}");
        };
        over("id", json!("../phone"));
        over("id", json!("a".repeat(33)));
        over("provenance", json!("web"));
        over("size", json!([80.0, 130.0, 0.0]));
        over("size", json!([80.0, 130.0]));
        over("sources", json!("https://example.com"));
        over("features", json!(["usb_c"]));
        over("features", json!(vec![json!({}); MAX_FEATURES + 1]));
        over("unverified", json!(vec![json!({}); MAX_UNVERIFIED + 1]));
        over("unverified", json!([1]));
        over("used_features", json!(["<img src=x>"]));
        over("used_features", json!(vec!["a"; MAX_FEATURES + 1]));
        over("questions", json!([question("web:r1")]));
        over("body", json!(null));
        over("body", json!({"padding": "x".repeat(MAX_DEVICE_BYTES)}));
        over("script", json!("x"));
        // Too large in all, though each one is within its bounds.
        let mut large = phone.clone();
        large["body"] = json!({"padding": "x".repeat(MAX_DEVICE_BYTES / 2)});
        let mut other = large.clone();
        other["id"] = json!("other");
        assert!(build(json!([large])).is_ok());
        assert!(build(json!([large, other])).is_err());

        let references = |value: Value| parse(with("references", value));
        assert!(references(json!([{"name": "sample", "used": true}])).is_ok());
        assert!(references(json!(["sample"])).is_err());
        assert!(references(json!(vec![json!({}); MAX_REFERENCES + 1])).is_err());
        assert!(references(json!([{"p": "x".repeat(MAX_DEVICE_BYTES)}])).is_err());
    }

    #[test]
    fn a_question_is_about_a_short_plain_id() {
        let ask = |id: &str, count: usize| {
            let one = json!({"requirement": id, "text": "t", "guessed_params": []});
            parse(with("questions", json!(vec![one; count])))
        };
        // The longest the engine writes: `reference_` and the name of a reference file.
        assert!(ask(&format!("reference_{}", "a".repeat(32)), 1).is_ok());
        assert!(ask(&"a".repeat(64), 1).is_ok());
        for bad in [
            "",
            "Reference",
            "1a",
            "_a",
            "a-b",
            "web:r1",
            &"a".repeat(65),
        ] {
            assert!(ask(bad, 1).is_err(), "{bad}");
        }
        // A contract may have 256 requirements, and each may be a question.
        assert!(ask("wall", 256).is_ok());
        assert!(ask("wall", MAX_QUESTIONS + 1).is_err());
    }

    #[test]
    fn a_result_survives_a_serde_round_trip() {
        let value = json!({"protocol": 0, "op": "build", "status": "check_error", "error": null,
            "stdout": "hi", "stdout_truncated": false, "params": {"n": 3},
            "param_specs": {"n": {"default": 3, "unit": "count", "label": "N",
                                  "provenance": "style"}},
            "checks": [{"id": "min_wall", "status": "fail", "severity": "error",
                        "message": "thin", "part": null, "data": {"thinnest": 0.2}}],
            "artifacts": {"step": "model.step", "views": {"iso": "views/iso.png"}},
            "warnings": ["w"], "timings": {"total_s": 1.5}});
        let result = parse(value).unwrap();
        let again: EngineResult =
            serde_json::from_str(&serde_json::to_string(&result).unwrap()).unwrap();
        assert_eq!(result, again);
        assert_eq!(result.params["n"], 3.0);
    }
}
