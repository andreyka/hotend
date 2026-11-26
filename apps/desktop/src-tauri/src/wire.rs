//! The shapes that cross to the window, as types the TypeScript binding is generated from.
//!
//! `hotend-core`'s facade types are plain serde data but carry no `specta::Type`, so each
//! has a mirror here with the same serde attributes. A mirror is never built at run time:
//! [`Wire<T, W>`] sends (or reads) the core's own value `T` and only *describes* itself to
//! the binding generator as the mirror `W`. What the window receives is therefore always
//! exactly what the core serialises; a mirror that has drifted makes the generated types
//! wrong, not the data, and `tests/wire.rs` fails when a mirror cannot reproduce the JSON
//! the core sends for a command's answer or an event.
//!
//! Integers wider than 32 bits (timestamps in milliseconds, byte counts, row ids) are
//! exported as TypeScript `number`: none of them comes near 2^53.

use std::collections::BTreeMap;
use std::marker::PhantomData;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use specta::Type;

/// A core value `T` that the binding generator sees as its mirror `W`.
pub struct Wire<T, W>(pub T, PhantomData<fn() -> W>);

impl<T, W> Wire<T, W> {
    pub fn new(value: T) -> Self {
        Self(value, PhantomData)
    }
}

impl<T, W> From<T> for Wire<T, W> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T: Clone, W> Clone for Wire<T, W> {
    fn clone(&self) -> Self {
        Self::new(self.0.clone())
    }
}

impl<T: std::fmt::Debug, W> std::fmt::Debug for Wire<T, W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl<T: Serialize, W> Serialize for Wire<T, W> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de, T: Deserialize<'de>, W> Deserialize<'de> for Wire<T, W> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        T::deserialize(deserializer).map(Self::new)
    }
}

impl<T, W: Type> Type for Wire<T, W> {
    fn definition(types: &mut specta::Types) -> specta::datatype::DataType {
        W::definition(types)
    }
}

/// Turns a command argument into the value the shell works with: a [`Wire`] gives its core
/// value, plain data is itself.
pub trait IntoPlain {
    type Plain;
    fn into_plain(self) -> Self::Plain;
}

impl<T, W> IntoPlain for Wire<T, W> {
    type Plain = T;
    fn into_plain(self) -> T {
        self.0
    }
}

macro_rules! plain {
    ($($ty:ty),* $(,)?) => {
        $(impl IntoPlain for $ty {
            type Plain = $ty;
            fn into_plain(self) -> $ty {
                self
            }
        })*
    };
}

plain!(
    String,
    bool,
    u32,
    [u32; 2],
    Option<String>,
    Option<u32>,
    Option<i64>,
    Vec<String>,
    Option<Vec<String>>,
    Vec<Value>,
    LinkId,
);

/// A number as the window sees it: TypeScript `number`.
///
/// specta exports `f64` as `number | null`, because JSON has no NaN or infinity and
/// serde_json writes those as `null`. Every number the core sends was read from JSON (the
/// engine's and the slicer's results) or computed from such numbers, so the mirrors promise
/// a finite number instead of making every reader handle a `null` that never comes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Num(pub f64);

impl Type for Num {
    fn definition(_types: &mut specta::Types) -> specta::datatype::DataType {
        specta_typescript::define("number").into()
    }
}

// ---- projects and versions -------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub current_version: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum VersionStatus {
    Ok,
    CheckError,
}

/// One immutable build of the model.
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct Version {
    pub project_id: String,
    pub number: u32,
    pub created_at_ms: i64,
    pub parent: Option<u32>,
    /// `write_model_code` for a written model, `set_parameter` for one changed size.
    pub origin: String,
    pub note: String,
    pub status: VersionStatus,
    pub code_sha256: String,
    pub params: BTreeMap<String, Num>,
}

/// One line of the stored conversation.
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct TranscriptEntry {
    pub id: i64,
    pub run_id: String,
    /// `user` or `assistant`.
    pub role: String,
    pub text: String,
    pub at_ms: i64,
    pub remembered: bool,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct ProjectDetail {
    pub project: Project,
    pub versions: Vec<Version>,
    pub transcript: Vec<TranscriptEntry>,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct VersionDetail {
    pub version: Version,
    pub result: EngineResult,
    pub dir: String,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct BuildReport {
    /// Absent when the model did not build or refused the value.
    pub version: Option<Version>,
    pub result: EngineResult,
}

// ---- the engine's result (PROTOCOL.md section 3.3) ---------------------------------------

#[derive(Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum EngineStatus {
    Ok,
    ModelError,
    CheckError,
    Timeout,
    InternalError,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct EngineError {
    #[serde(rename = "type")]
    pub kind: String,
    pub message: String,
    pub line: Option<u32>,
    pub traceback: String,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct ParamSpec {
    pub default: Num,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<Num>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<Num>,
    pub unit: String,
    pub label: String,
    /// `measured`, `standard`, `calibrated`, `guessed`, `style` or `web`.
    pub provenance: String,
    /// With the provenance `web`: the id of the research note the number is from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct BBox {
    pub min: [Num; 3],
    pub max: [Num; 3],
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct PartSummary {
    pub name: String,
    pub volume: Num,
    pub area: Num,
    pub mass_estimate_g: Option<Num>,
    pub bbox: BBox,
    pub solids: u32,
    pub faces: u32,
    pub is_valid: bool,
    pub is_manifold: bool,
    pub color: String,
}

#[derive(Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
    Skipped,
}

#[derive(Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum CheckSeverity {
    Error,
    Warning,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct CheckResult {
    pub id: String,
    pub status: CheckStatus,
    pub severity: CheckSeverity,
    pub message: String,
    pub part: Option<String>,
    pub data: Value,
}

#[derive(Debug, Serialize, Deserialize, Type)]
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

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct DocEntry {
    pub name: String,
    pub kind: String,
    pub module: String,
    pub signature: String,
    pub doc: String,
    pub score: Num,
}

/// What the contract still asks the maker, because a requirement rests on a guess.
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct Question {
    pub requirement: String,
    pub text: String,
    pub guessed_params: Vec<String>,
    /// The web number a question with a `web:` requirement is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web: Option<WebNumber>,
}

/// A bought device the version was designed around. Shown as text; nothing is decided by it.
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct ComponentUse {
    pub id: String,
    pub name: String,
    /// What it is, in a word: `phone`.
    pub kind: String,
    pub frame: String,
    /// Width, height and depth, mm.
    pub size: [Num; 3],
    pub provenance: String,
    pub sources: Value,
    pub numbers: u32,
    pub features: Vec<Value>,
    pub used_features: Vec<String>,
    /// The engine's claim; the `reference_fit` check is the verdict.
    pub proven: bool,
    pub unverified: Vec<Value>,
    pub questions: Vec<Question>,
    pub body: Value,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct Orientation {
    pub part: String,
    pub source: String,
    pub up: [Num; 3],
    pub turned: String,
    pub score: Num,
    pub terms: BTreeMap<String, Num>,
    pub measures: Value,
    pub as_modelled: Value,
    pub preferred_by: Option<String>,
    pub overridden: Option<String>,
    pub candidates: Vec<Value>,
    pub explanation: String,
    pub copies: u32,
    pub nozzle_id: i64,
    /// The print facts of the part in the chosen orientation (docs/PRINT_SETTINGS.md).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facts: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct PlateItem {
    pub name: String,
    pub part: String,
    pub copy: u32,
    pub nozzle_id: i64,
    pub quarter_turn: bool,
    /// 12 numbers, a 3 x 4 matrix row by row: plate = R * model + t.
    pub transform: Vec<Num>,
    pub bbox: BBox,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct Plate {
    pub index: u32,
    pub size: [Num; 2],
    pub items: Vec<PlateItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threemf: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub views: BTreeMap<String, String>,
    /// The print facts of the plate's pieces, in the order of `items`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facts: Option<Value>,
    /// How the plate was laid out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<PlateLayout>,
}

/// How a plate was laid out: the room kept round every piece for its brim, the distance
/// to the edge, where the group lies (`front_left` or `centre`) and how far it was moved.
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct PlateLayout {
    pub clearance: Num,
    pub margin: Num,
    pub anchor: String,
    pub offset: [Num; 2],
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct Unplaced {
    pub name: String,
    pub part: String,
    pub copy: u32,
    pub reason: String,
    /// `clearance`, `keepout`, `too_large` or `no_reach`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct GaugeDimension {
    pub id: String,
    pub half: String,
    pub object: String,
    pub feature: String,
    pub drawn: Num,
    pub what: String,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct GaugeClearance {
    pub pair: String,
    pub fit_class: String,
    pub gap: Num,
    pub provenance: String,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct Gauge {
    pub card: String,
    pub name: String,
    pub args: BTreeMap<String, Value>,
    pub parts: Vec<String>,
    pub size: [Num; 3],
    pub dimensions: Vec<GaugeDimension>,
    pub clearances: Vec<GaugeClearance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facts: Option<BTreeMap<String, Value>>,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct CalibrationChange {
    pub quantity: String,
    pub before: Num,
    pub after: Num,
    pub sigma: Num,
    pub readings: u32,
}

/// The engine's result for any operation. Fields an operation does not produce are absent.
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct EngineResult {
    pub protocol: u32,
    pub op: Option<String>,
    pub status: EngineStatus,
    pub error: Option<EngineError>,
    pub timings: BTreeMap<String, Num>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stdout: String,
    pub stdout_truncated: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, Num>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub param_specs: BTreeMap<String, ParamSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<PartSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<CheckResult>,
    pub artifacts: Artifacts,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub results: Vec<Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub versions: BTreeMap<String, Option<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub docs: Vec<DocEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub questions: Option<Vec<Question>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<ComponentUse>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub orientations: Vec<Orientation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facts: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plates: Vec<Plate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unplaced: Vec<Unplaced>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gauges: Vec<Gauge>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<CalibrationChange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_provenance: Option<String>,
}

// ---- views, export, preparation, gauges, calibration -------------------------------------

#[derive(Debug, Serialize, Deserialize, Type)]
pub enum SectionPlane {
    XY,
    XZ,
    YZ,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct Section {
    pub plane: SectionPlane,
    pub offset: Num,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct RenderedView {
    pub name: String,
    /// The PNG, base64.
    pub png: String,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub enum ExportFormat {
    #[serde(rename = "3mf")]
    ThreeMf,
    #[serde(rename = "stl")]
    Stl,
    #[serde(rename = "step")]
    Step,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct ExportedFile {
    pub format: ExportFormat,
    pub part: Option<String>,
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

// ---- snapshots ---------------------------------------------------------------------------

/// A name given to one version of a project (`hotend_core::Snapshot`).
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct Snapshot {
    pub id: String,
    pub project_id: String,
    /// One plain line, unique in the project.
    pub name: String,
    /// The version it pins.
    pub version: u32,
    /// Optional words about it; empty when none were given.
    pub note: String,
    pub created_at_ms: i64,
    /// The print job whose sliced file the snapshot keeps; `null` when the version had not
    /// been printed when the snapshot was made.
    #[serde(default)]
    pub printed_job_id: Option<String>,
    /// SHA-256 of that job's sliced file.
    #[serde(default)]
    pub printed_sha256: Option<String>,
}

/// One file of a snapshot export (`hotend_core::SnapshotFile`).
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct SnapshotFile {
    /// `step`, `3mf`, `printed` (the sliced file of the print) or `manifest`
    /// (`snapshot.json`).
    pub kind: String,
    /// `copied`, `exported` (made by `export_run`) or `written` (the manifest).
    pub source: String,
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

/// The sliced file of the print of a snapshot's version (`hotend_core::PrintedFile`).
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct PrintedFile {
    pub job_id: String,
    pub delivery: Option<String>,
    pub status: String,
    pub file_name: String,
    pub sha256: String,
}

/// The recorded printed file, when no copy with its hash was found
/// (`hotend_core::MissingPrinted`).
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct MissingPrinted {
    pub job_id: String,
    pub sha256: String,
    /// Starts with "printed file missing".
    pub message: String,
}

/// What `snapshot_export` wrote (`hotend_core::SnapshotExport`).
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct SnapshotExport {
    pub snapshot: Snapshot,
    /// The new folder the files are in, below the folder the person chose.
    pub folder: String,
    pub files: Vec<SnapshotFile>,
    pub printed: Option<PrintedFile>,
    #[serde(default)]
    pub printed_missing: Option<MissingPrinted>,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct PrepareOptions {
    /// Copies per part name.
    pub copies: BTreeMap<String, u32>,
    /// Which side of a part points up: `+x`, `-x`, `+y`, `-y`, `+z` or `-z`, per part name.
    pub orientations: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct Preparation {
    pub project_id: String,
    pub version: u32,
    pub printer_id: String,
    pub material_id: String,
    pub options: PrepareOptions,
    pub status: EngineStatus,
    pub orientations: Vec<Orientation>,
    pub plates: Vec<Plate>,
    pub unplaced: Vec<Unplaced>,
    pub checks: Vec<CheckResult>,
    pub warnings: Vec<String>,
    pub created_at_ms: i64,
    /// The room kept round every piece for its brim, mm; `null` for a preparation made
    /// before the print settings.
    pub clearance_mm: Option<Num>,
    /// The distance of every piece and its brim from the edge of the reach, mm.
    pub margin_mm: Option<Num>,
    /// The parts of the bed the layout kept free.
    pub keepouts: Vec<Keepout>,
    /// The plan of each plate when the version was prepared.
    pub plans: Vec<PlatePlanSummary>,
    pub dir: String,
}

/// A part of the bed that pieces and their brims stay off, in bed coordinates (mm).
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct Keepout {
    pub id: String,
    pub min: [Num; 2],
    pub max: [Num; 2],
}

/// A code with its sentence.
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct PlanNotice {
    pub code: String,
    pub sentence: String,
}

/// The print plan of one plate, in short.
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct PlatePlanSummary {
    pub plate: u32,
    pub sha256: String,
    /// Why there are no print settings for the plate yet, when there are none.
    pub blocked: Option<PlanNotice>,
    /// Which settings a slice of the plate would use, in words.
    pub settings_source: String,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct CardRef {
    pub card: String,
    pub args: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct GaugeFile {
    pub plate: u32,
    /// `3mf` or `view`.
    pub kind: String,
    pub path: String,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct GaugeExport {
    pub gauges: Vec<Gauge>,
    pub plates: Vec<Plate>,
    pub unplaced: Vec<Unplaced>,
    pub warnings: Vec<String>,
    pub files: Vec<GaugeFile>,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct Calibration {
    pub printer_id: String,
    pub table: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum GaugeFeature {
    Hole,
    Peg,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct GaugeReading {
    pub feature: GaugeFeature,
    pub drawn: Num,
    pub measured: Num,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct CalibrationUpdate {
    pub printer_id: String,
    pub material_id: String,
    pub nozzle_id: Option<i64>,
    pub measured_at: String,
    pub changes: Vec<CalibrationChange>,
    pub entry_provenance: Option<String>,
    pub table: Value,
}

// ---- slicing -----------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct SlicerInstallation {
    pub kind: String,
    pub name: String,
    pub path: Option<String>,
    pub found: Option<String>,
    pub version: Option<String>,
    pub stand_in: bool,
    pub verdict: String,
    pub reason: Option<String>,
    pub fix: Option<String>,
    pub usable: bool,
    pub printers: Vec<String>,
}

/// The material a slice was made for.
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct SliceJobMaterial {
    pub id: String,
    pub family: String,
}

/// What a slice was made for. `plate` is the plate of the version's layout, from 1: the
/// one a review and the confirmation dialog name.
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct SliceJob {
    pub plate: u32,
    pub printer_id: String,
    pub material: SliceJobMaterial,
    /// The pieces on that plate, by name.
    pub pieces: Vec<String>,
    /// `created_at_ms` of the layout that was sliced.
    pub prepared_at_ms: i64,
    /// The build plate the slice was made for, by its id, once the slicer confirmed it;
    /// `null` for a slice made without a chosen plate, which cannot become a print.
    pub plate_type: Option<String>,
    /// The nozzle sizes the slice was made for (the fitted size of the settings); a print
    /// job is checked against these.
    pub nozzles: Vec<SliceJobNozzle>,
    /// The hash of the print plan the slice was made with; `null` when the plan was
    /// blocked and the stand-in sliced with its own profiles, and in slices made before.
    #[serde(default)]
    pub plan_sha256: Option<String>,
    /// The code of the blocked plan, when the stand-in sliced without settings.
    #[serde(default)]
    pub plan_blocked: Option<String>,
    /// The fitted nozzle size the slice was made for, mm.
    #[serde(default)]
    pub nozzle_diameter_mm: Option<Num>,
    /// Whether the person set that size; `false` when 0.4 mm was assumed.
    #[serde(default)]
    pub nozzle_size_set: Option<bool>,
    /// The settings' `previews` when the slice was made; `null` in slices made before
    /// this was recorded.
    #[serde(default)]
    pub previews: Option<PreviewsMode>,
}

/// One nozzle of the printer a slice was made for.
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct SliceJobNozzle {
    /// The printer's own id of the nozzle (device extruder).
    pub id: i64,
    pub diameter_mm: Num,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct SliceRecord {
    pub project_id: String,
    pub version: u32,
    pub profile_hash: String,
    pub profile: String,
    pub summary: Value,
    pub job: SliceJob,
    pub created_at_ms: i64,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct FilamentUse {
    pub slot: u32,
    pub grams: Num,
    pub metres: Num,
    pub nozzle_id: i64,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct SliceWarning {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct SlicerInfo {
    pub name: String,
    pub version: String,
}

/// The build plate a file was sliced for.
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct SliceBed {
    /// The plate's id, as the settings name it.
    pub id: String,
    /// The slicer's own name for the plate.
    pub plate_type: String,
    pub temperature_c: Option<Num>,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct SliceSummary {
    pub status: String,
    pub output: String,
    pub sha256: String,
    pub time_s: Num,
    pub filament: Vec<FilamentUse>,
    pub layers: u32,
    /// The plate the file was sliced for, when the slicer has plate types.
    pub bed: Option<SliceBed>,
    pub warnings: Vec<SliceWarning>,
    pub slicer: SlicerInfo,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct SliceOutcome {
    pub record: SliceRecord,
    pub summary: SliceSummary,
    pub preparation: Preparation,
    pub output_path: String,
}

// ---- printing ----------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeliveryChoice {
    SimulatedPrinter,
    BambuConnect,
    ExportToFolder { folder: String },
}

#[derive(Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum LintSeverity {
    Error,
    Warning,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct LintFinding {
    pub severity: LintSeverity,
    pub code: String,
    pub line: u64,
    pub message: String,
    pub excerpt: String,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct PrinterProblem {
    pub message: String,
    pub raw_code: String,
    pub meaning: Option<String>,
    pub blocking: bool,
    pub help_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct PrintFilament {
    pub slot: u32,
    pub material: Option<String>,
    pub colour: Option<String>,
    pub grams: Option<Num>,
    pub metres: Option<Num>,
    pub nozzle_id: Option<i64>,
    pub selected_in_gcode: bool,
}

/// The confirmation summary, computed from the sliced file only.
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct PrintSummary {
    pub file_name: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub plate: u32,
    pub estimated_seconds: Option<u64>,
    pub layers: Option<u32>,
    pub filaments: Vec<PrintFilament>,
    pub max_nozzle_c: Option<Num>,
    pub max_bed_c: Option<Num>,
    pub max_chamber_c: Option<Num>,
    pub extent: Option<[[Num; 3]; 2]>,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct SpoolChoice {
    pub slot: u32,
    pub material: String,
    pub nozzle_id: i64,
    pub ams_unit: u8,
    pub tray: u8,
    pub loaded: String,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct PrinterRef {
    pub id: String,
    pub name: String,
    pub model: String,
    pub serial: String,
    pub simulated: bool,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct TrayInfo {
    pub id: u8,
    pub material: Option<String>,
    pub product: Option<String>,
    pub colour: Option<String>,
    pub remaining_percent: Option<u8>,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct AmsInfo {
    pub id: u8,
    pub high_temperature: bool,
    pub nozzle_id: Option<i64>,
    pub drying_minutes_left: Option<u32>,
    pub trays: Vec<TrayInfo>,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct PrinterState {
    pub phase: String,
    pub headline: String,
    pub progress_percent: Option<u8>,
    pub remaining_minutes: Option<u32>,
    pub layer: Option<u32>,
    pub total_layers: Option<u32>,
    pub job_name: Option<String>,
    pub bed_c: Option<Num>,
    pub problems: Vec<PrinterProblem>,
    pub ams: Vec<AmsInfo>,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct DeliveryInfo {
    pub kind: String,
    pub name: String,
    pub enabled: bool,
    pub disabled_reason: Option<String>,
    pub sends_file: bool,
    pub starts_print: bool,
    pub controls_print: bool,
    pub live_status: bool,
    pub reports_result: bool,
    pub can: Vec<String>,
    pub cannot: Vec<String>,
    pub cost: String,
    pub needs: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct PrinterInfo {
    pub id: String,
    pub name: String,
    pub model: String,
    pub serial: String,
    pub simulated: bool,
    pub state: PrinterState,
    pub deliveries: Vec<DeliveryInfo>,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct PrintJobDetail {
    pub simulated: bool,
    pub phase: Option<String>,
    pub headline: Option<String>,
    pub progress_percent: Option<u8>,
    pub remaining_minutes: Option<u32>,
    pub message: Option<String>,
    pub problems: Vec<PrinterProblem>,
    pub findings: Vec<LintFinding>,
    pub link: Option<String>,
    pub path: Option<String>,
    /// The Windows form of a handed-off file's path (under WSL and on Windows).
    pub windows_path: Option<String>,
    /// Whether the program was asked to open a handed-off file: true only when a real
    /// opener started; false for the stand-in or when the opener failed.
    pub opened: Option<bool>,
    /// What was asked to open it.
    pub opener: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct PrintJob {
    pub id: String,
    pub project_id: String,
    pub version: u32,
    pub slice_sha256: String,
    pub note: String,
    /// `pending`, `starting`, `printing`, `finished`, `failed`, `cancelled`, `rejected`,
    /// `not_started`, `handed_off` or `exported`.
    pub status: String,
    pub delivery: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub detail: PrintJobDetail,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct PrintReview {
    pub job: PrintJob,
    pub delivery: DeliveryChoice,
    pub simulated: bool,
    pub printer: Option<PrinterRef>,
    /// The plate of the version's layout that was sliced, from 1. `summary.plate` is the
    /// plate inside the sliced file, which is 1 in every file.
    pub plate: u32,
    /// The pieces on that plate, by name.
    pub pieces: Vec<String>,
    pub summary: PrintSummary,
    /// The text the confirmation is given for, written from the checked file. The native
    /// dialog shows it as it is; the window may show it too, and cannot confirm with it.
    pub review_text: String,
    pub warnings: Vec<LintFinding>,
    pub spools: Vec<SpoolChoice>,
    /// Numbers of this version found on the web that nobody has confirmed, with their page
    /// and quote (docs/WEB_RESEARCH.md).
    #[serde(default)]
    pub web_numbers: Vec<WebNumber>,
    /// Where a hand-off to Bambu Connect would place the file, and what opens the link;
    /// `null` for the other deliveries.
    #[serde(default)]
    pub handoff: Option<HandOffTarget>,
}

/// Where a hand-off places the file, and what then opens it
/// (`hotend_core::HandOffTarget`).
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct HandOffTarget {
    /// The hand-off folder as Hotend sees it.
    pub folder: String,
    /// Its Windows form, under WSL and on Windows.
    pub windows_folder: Option<String>,
    /// What opens the link or the file.
    pub opener: String,
    /// False when the opener opens nothing (the stand-in): the file is placed and nothing
    /// is opened.
    pub opens: bool,
}

/// The hand-off as it is set up now, for the settings page (`hotend_core::HandOffStatus`).
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct HandOffStatus {
    /// `wsl`, `windows`, `macos` or `linux`: the host the hand-off path is made for.
    pub host: String,
    /// The folder the settings name; `null` when none was chosen.
    pub chosen: Option<String>,
    /// The folder a hand-off would use now; `null` when there is none.
    pub folder: Option<String>,
    /// Its Windows form, under WSL and on Windows.
    pub windows_folder: Option<String>,
    /// Why no hand-off can be made now, as a sentence.
    pub problem: Option<String>,
    /// What opens the import link.
    pub opener: String,
    /// False when the opener opens nothing (the stand-in).
    pub opens: bool,
}

// ---- a real printer's status over the local network (read-only) --------------------------

/// The printer on the local network whose status Hotend reads
/// (`hotend_core::PrinterLanAddress`). Not a secret.
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct PrinterLanAddress {
    /// A private-range IPv4 address, as four numbers.
    pub ip: String,
    /// The printer's serial number, upper case.
    pub serial: String,
}

/// The certificate the person confirmed (`hotend_core::CertificatePin`).
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct CertificatePin {
    pub ip: String,
    pub serial: String,
    /// SHA-256 of the certificate, lower-case hex.
    pub sha256: String,
    pub subject: String,
    pub confirmed_at_ms: i64,
}

/// How the printer's certificate is checked.
#[derive(Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum LanTrust {
    /// Against Bambu Lab's certificate authorities in the configuration folder.
    CaFile,
    /// Against the certificate the person confirmed.
    Pinned,
    /// Not yet: nothing is connected to until the certificate is confirmed.
    Unconfirmed,
}

/// Where the printer's access code would come from. Never the code.
#[derive(Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AccessCodeSource {
    Environment,
    SecretStore,
    Missing,
    Unreadable,
}

/// Where a secret typed in the window would be kept (`hotend_core::SecretStoreKind`).
/// `environment` stores nothing: Hotend was started with `HOTEND_SECRETS=env`, and a key
/// must be put in the environment (or Hotend started with `HOTEND_SECRETS=file`).
#[derive(Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SecretStoreKind {
    Keychain,
    File,
    Environment,
    Memory,
}

/// How reading the printer's status is set up (`hotend_core::PrinterLanSetup`), with
/// whether an access code is there (`access_code_present`, the shell's). Never the code.
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct PrinterLanSetup {
    pub address: Option<PrinterLanAddress>,
    pub trust: LanTrust,
    /// Where Hotend looks for Bambu Lab's certificate authorities.
    pub ca_file: Option<String>,
    pub pin: Option<CertificatePin>,
    pub access_code: AccessCodeSource,
    /// Where a code typed in the window would be kept; `environment` keeps none.
    pub access_code_store: SecretStoreKind,
    /// Whether the core is following the printer now.
    pub watching: bool,
    /// True when an access code is available (environment or secret store).
    pub access_code_present: bool,
}

/// What `printer_certificate_check` answers: whether the person confirmed the certificate
/// in the native dialog, and the setup afterwards.
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct CertificateConfirmation {
    pub confirmed: bool,
    pub setup: PrinterLanSetup,
}

/// A temperature and its target, °C; a target of 0 means the heater is off.
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct TemperatureReading {
    pub current_c: Option<Num>,
    pub target_c: Option<Num>,
}

/// One nozzle's temperature, by the printer's extruder id.
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct NozzleReading {
    pub id: u8,
    pub current_c: Option<Num>,
    pub target_c: Option<Num>,
}

/// A real printer's status as read over the local network
/// (`hotend_core::LanPrinterStatus`).
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct LanPrinterStatus {
    pub state: PrinterState,
    pub nozzles: Vec<NozzleReading>,
    pub bed: TemperatureReading,
    pub chamber: TemperatureReading,
    /// How many reports were merged.
    pub reports: u64,
    /// The handed-off job these reports were matched to; `null`: the printer's status only.
    pub job_id: Option<String>,
    pub matched_name: Option<String>,
    pub connected: bool,
    /// Why the session ended, once it did.
    pub ended: Option<String>,
}

// ---- editable Bambu Studio projects ------------------------------------------------------

/// Where an editable project goes when it is handed over
/// (`hotend_core::ProjectDestination`).
#[derive(Debug, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProjectDestination {
    /// Placed in the hand-off folder; the system is then asked to open it with the program
    /// for `.3mf` files (Bambu Studio).
    BambuStudio,
    /// Written into a folder picked with `folder_pick`; nothing is opened.
    Folder { folder: String },
}

/// The keys an editable project lists as changed from the system presets, per preset.
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct ProjectKeys {
    pub process: Vec<String>,
    pub filament: Vec<String>,
    pub printer: Vec<String>,
}

/// An editable Bambu Studio project Hotend made for one plate
/// (`hotend_core::EditableProject`).
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct EditableProject {
    pub id: String,
    pub project_id: String,
    pub version: u32,
    /// The plate of the version's layout, from 1.
    pub plate: u32,
    /// The plate's print plan the project carries, by its hash.
    pub plan_sha256: String,
    /// SHA-256 of the sliced file of the same plan that passed the G-code check.
    pub slice_sha256: String,
    /// The project file where Hotend keeps it.
    pub path: String,
    /// SHA-256 of the project file.
    pub sha256: String,
    pub keys: ProjectKeys,
    /// The project sliced once on its own by Bambu Studio's command line (the test slice).
    pub test_slice: SliceSummary,
    /// The proof's verdict on the test slice (`passed`).
    pub proof: String,
    /// The sentence every interface shows with a project: it is outside Hotend's check.
    pub notice: String,
    /// `bambu_studio` or `folder` once it was handed over.
    pub delivery: Option<String>,
    /// The last hand-over: `message`, `path`, `windows_path`, `opened` and `opener`.
    pub detail: PrintJobDetail,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// An editable project as it is put before the person who confirms handing it over
/// (`hotend_core::ProjectReview`).
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct ProjectReview {
    pub project: EditableProject,
    /// Where it goes; a folder as the core resolved it.
    pub destination: ProjectDestination,
    /// The name the file gets where it is placed.
    pub file_name: String,
    /// For a hand-off to Bambu Studio: the hand-off folder and what opens the file.
    pub handoff: Option<HandOffTarget>,
    /// The text the confirmation is given for, written by the printer layer. The native
    /// dialog shows it as it is; the window may show it too, and cannot confirm with it.
    pub review_text: String,
    /// What the G-code check found in the test slice that does not block.
    pub warnings: Vec<LintFinding>,
    /// The sentence that the project is outside Hotend's check.
    pub notice: String,
}

/// What `print_project_confirm` answers: whether the person confirmed in the native
/// dialog, and the project as it is afterwards.
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct ProjectConfirmation {
    pub confirmed: bool,
    pub project: EditableProject,
}

/// A number of a version that came from a web page (`hotend_core::research::WebNumber`).
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct WebNumber {
    pub note: String,
    pub key: String,
    pub label: String,
    pub value: Num,
    pub unit: String,
    /// The page. Shown as text; opened only when the person asks.
    pub url: String,
    pub host: String,
    pub title: String,
    pub quote: String,
    pub locator: String,
    /// `page_text`, `citation` or `none`.
    pub evidence: String,
    /// `unconfirmed` or `confirmed`.
    pub state: String,
    pub params: Vec<String>,
    pub built: Option<Num>,
    pub text: String,
}

/// What `print_confirm` answers: whether the person confirmed in the native dialog, and
/// the job as it is afterwards.
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct PrintConfirmation {
    pub confirmed: bool,
    pub job: PrintJob,
}

// ---- settings, audit, doctor -------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Anthropic,
    Openai,
    Google,
    Openrouter,
    Ollama,
}

/// How much the model may think and spend on one answer (`effort` of the settings).
#[derive(Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct ProviderSettings {
    pub kind: ProviderKind,
    pub model: String,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct MaterialContext {
    pub id: String,
    pub family: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub density_g_cm3: Option<Num>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_overhang_deg: Option<Num>,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct Settings {
    pub provider: Option<ProviderSettings>,
    pub printer_id: String,
    pub material: MaterialContext,
    /// The build plate on the printer (`cool`, `textured-pei`, `high-temp`,
    /// `engineering`); `null` until the person chooses one.
    pub plate_type: Option<String>,
    /// Sent only to Anthropic models documented to accept it; `high` by default.
    #[serde(default = "default_effort")]
    pub effort: Effort,
    pub max_turns: u32,
    /// Slicer program per slicer kind.
    pub slicer_executables: BTreeMap<String, String>,
    /// Web research limits and the person's consent (docs/WEB_RESEARCH.md). Read only for
    /// the window: `settings_set` keeps the core's, whatever is sent here.
    #[serde(default)]
    pub research: Value,
    /// The diameter of the nozzle fitted to the printer: 0.2, 0.4, 0.6 or 0.8 mm; `null`
    /// until the person says, and 0.4 mm is then assumed.
    pub nozzle_diameter_mm: Option<Num>,
    /// The spool the plates are printed from.
    pub spool: SpoolSettings,
    /// The hand-off folder the person chose (Bambu Connect, Bambu Studio projects); `null`
    /// for none. `settings_set` takes a new one only if it was picked with `folder_pick`
    /// under the hand-off picker's own title ("Choose the Bambu Connect hand-off folder");
    /// settings sent without it clear it.
    #[serde(default)]
    pub handoff_folder: Option<String>,
    /// Where a sliced file's plate previews come from: `slicer` (the default) or
    /// `hotend_made`. Settings sent without it go back to `slicer`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previews: Option<PreviewsMode>,
}

/// The settings' `previews`.
#[derive(Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum PreviewsMode {
    Slicer,
    HotendMade,
}

/// The one spool the print settings plan with; its material is the settings' material.
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct SpoolSettings {
    /// An AMS slot `1` to `16`, `external_main` or `external_auxiliary`.
    pub slot: String,
    /// `#rrggbb`; `null` until the person says, and grey (#898989) is then assumed.
    pub colour: Option<String>,
    pub silk: bool,
}

fn default_effort() -> Effort {
    Effort::High
}

// ---- where web research comes from -------------------------------------------------------

/// Where web research comes from (`hotend_core::SearchSource`, the settings'
/// `research.source`).
#[derive(Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SearchSource {
    /// The AI provider's own web search.
    BuiltIn,
    Keenable,
    Tavily,
    /// No web research at all.
    Off,
}

/// A search service with a key of its own (`hotend_core::SearchService`).
#[derive(Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SearchService {
    Keenable,
    Tavily,
}

/// One choice of research source (`hotend_core::SearchSourceOption`).
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct SearchSourceOption {
    pub source: SearchSource,
    /// Whether it can be chosen with the provider in the settings.
    pub available: bool,
    /// Why not, when it is not available; empty otherwise.
    pub reason: String,
    /// The host that receives the queries; empty for `built_in` (the provider) and `off`.
    pub host: String,
    /// Whether a key is needed (Tavily).
    pub needs_key: bool,
    /// Whether a key for it is stored. Never the key.
    pub key_present: bool,
    /// Whether that key comes from the service's environment variable (which wins over a
    /// stored one).
    pub key_from_environment: bool,
    /// Where a key typed in the window would be kept; `environment` keeps none.
    pub key_store: SecretStoreKind,
    /// Whether it works without a key (Keenable's keyless trial path).
    pub keyless: bool,
    /// The choice to suggest.
    pub suggested: bool,
    /// Whether it is the source chosen now.
    pub chosen: bool,
    /// Whether the person's agreement covers it.
    pub agreed: bool,
}

/// What `research_consent_ask` answers: whether the person agreed in the native dialog,
/// and the sources afterwards.
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct ResearchConsentAnswer {
    pub agreed: bool,
    pub options: Vec<SearchSourceOption>,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct AuditEntry {
    pub id: i64,
    pub at_ms: i64,
    pub project_id: Option<String>,
    pub run_id: Option<String>,
    pub kind: String,
    pub name: String,
    pub args_sha256: Option<String>,
    pub outcome: String,
    pub detail: Value,
}

#[derive(Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum DoctorStatus {
    Ok,
    Warn,
    Fail,
    Skipped,
    Info,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct DoctorCheck {
    pub id: String,
    pub label: String,
    pub status: DoctorStatus,
    pub required: bool,
    pub detail: String,
    pub fix: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct SandboxProbe {
    pub platform: String,
    pub backend: String,
    pub usable: bool,
    pub reason: Option<String>,
    pub kernel: Option<String>,
    pub landlock_abi: Option<u32>,
    pub seccomp: bool,
    pub namespaces: bool,
    pub namespaces_detail: Option<String>,
    pub root: bool,
    pub process_cap: ProcessCapReport,
    pub guarantees: BTreeMap<String, Vec<String>>,
    pub limitations: Vec<String>,
}

/// Which kernel mechanism caps the number of processes of a sandboxed job.
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct ProcessCapReport {
    pub default: ProcessCapMode,
    pub without_namespaces: ProcessCapMode,
    pub rlimit_enforced: bool,
    pub rlimit_detail: Option<String>,
    pub cgroup: Option<String>,
    pub cgroup_parent: Option<String>,
    pub cgroup_detail: Option<String>,
}

/// `mechanism` is `rlimit-in-user-namespace`, `cgroup-pids`, `rlimit-on-user-total` or
/// `none`; `enforcement` is `kernel-exact`, `kernel-approximate` or `refused`.
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct ProcessCapMode {
    pub mechanism: String,
    pub enforcement: String,
    pub bound: String,
}

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct DoctorReport {
    pub ok: bool,
    pub hotend: String,
    pub debug_build: bool,
    pub checks: Vec<DoctorCheck>,
    pub sandbox: Option<SandboxProbe>,
    pub slicers: Vec<SlicerInstallation>,
    /// The providers whose key the store in use reads from the environment, where it wins
    /// over a stored key. Never a key.
    pub keys_from_environment: Vec<ProviderKind>,
}

// ---- errors --------------------------------------------------------------------------------

/// Why a command failed: what happened (`message`) and what the maker can do (`next_step`).
#[derive(Debug, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApiError {
    NotFound {
        message: String,
        next_step: String,
    },
    InvalidInput {
        message: String,
        next_step: String,
    },
    NotConfigured {
        message: String,
        next_step: String,
    },
    Busy {
        message: String,
        next_step: String,
    },
    NoModel {
        message: String,
        next_step: String,
    },
    Engine {
        code: String,
        message: String,
        next_step: String,
    },
    Slicer {
        code: String,
        message: String,
        next_step: String,
    },
    /// `reason` is a stable sub-code of some codes: for `not_importable`,
    /// `slice_before_previews` (slicing again is all it takes) or `previews_off`.
    Print {
        code: String,
        message: String,
        next_step: String,
        findings: Vec<LintFinding>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    Agent {
        code: String,
        message: String,
        next_step: String,
    },
    Secrets {
        message: String,
        next_step: String,
    },
    Storage {
        message: String,
        next_step: String,
    },
    Internal {
        message: String,
        next_step: String,
    },
}

// ---- runs and events -----------------------------------------------------------------------

/// What `message_send` answers. The run's events arrive on the event stream.
#[derive(Debug, Serialize, Deserialize, Type)]
pub struct MessageStarted {
    pub run_id: String,
}

#[derive(Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum StopReason {
    Done,
    Budget,
    Cancelled,
    Error,
}

/// One event of an agent run. `Agent.event` is the agent's own event (PROTOCOL.md section
/// 5.2): an object with `type` one of `text_delta`, `thinking`, `tool_start`, `tool_end`,
/// `usage`.
#[derive(Debug, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunEvent {
    Agent {
        event: Value,
    },
    Finished {
        stop_reason: StopReason,
        error: Option<String>,
    },
    Failed {
        code: String,
        message: String,
    },
}

#[derive(Debug, Serialize, Deserialize, Type)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(
    clippy::large_enum_variant,
    reason = "a description of the core's JSON, never stored"
)]
pub enum PrintEvent {
    Status {
        job: PrintJob,
    },
    Phase {
        phase: String,
        headline: String,
    },
    Progress {
        percent: u8,
        remaining_minutes: Option<u32>,
    },
    Problem {
        problem: PrinterProblem,
    },
}

/// What the shell streams to the window: the core's events, and `lagged` when the window
/// read too slowly and `missed` events were dropped (read the current state again).
#[derive(Debug, Serialize, Deserialize, Type)]
#[serde(tag = "channel", rename_all = "snake_case")]
#[allow(
    clippy::large_enum_variant,
    reason = "a description of the core's JSON, never stored"
)]
pub enum ShellEvent {
    Run {
        run_id: String,
        project_id: String,
        event: RunEvent,
    },
    Print {
        job_id: String,
        project_id: String,
        simulated: bool,
        event: PrintEvent,
    },
    /// What a printer followed over the local network reported (read-only).
    Printer {
        status: LanPrinterStatus,
    },
    Lagged {
        missed: u64,
    },
}

/// A page outside the window that `open_link` may open in the system browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum LinkId {
    AnthropicKeys,
    OpenaiKeys,
    GoogleKeys,
    OpenrouterKeys,
    OllamaDownload,
}
