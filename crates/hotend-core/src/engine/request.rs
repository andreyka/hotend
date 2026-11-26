//! `request.json`: what the core asks the engine worker to do.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::PROTOCOL_VERSION;

/// The view names the engine can render.
pub const VIEW_NAMES: [&str; 7] = ["iso", "front", "back", "left", "right", "top", "bottom"];

/// One request to the engine worker. Serialises to the exact JSON of PROTOCOL.md section 3.2.
///
/// A build request is much larger than the others. One request exists per job, so the
/// variants are kept unboxed and callers match on plain structs.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "op", rename_all = "lowercase")]
#[allow(clippy::large_enum_variant)]
pub enum EngineRequest {
    Build(BuildRequest),
    Measure(MeasureRequest),
    Export(ExportRequest),
    Views(ViewsRequest),
    Docs(DocsRequest),
    Prepare(PrepareRequest),
    Gauge(GaugeRequest),
    Calibrate(CalibrateRequest),
    Sweep(SweepRequest),
}

impl EngineRequest {
    /// The op name, as it appears in the request and in the result.
    pub fn op(&self) -> &'static str {
        match self {
            Self::Build(_) => "build",
            Self::Measure(_) => "measure",
            Self::Export(_) => "export",
            Self::Views(_) => "views",
            Self::Docs(_) => "docs",
            Self::Prepare(_) => "prepare",
            Self::Gauge(_) => "gauge",
            Self::Calibrate(_) => "calibrate",
            Self::Sweep(_) => "sweep",
        }
    }

    /// The wall-clock limit this request asks for.
    pub fn wall_seconds(&self) -> u64 {
        match self {
            Self::Build(r) => r.limits.wall_seconds,
            Self::Measure(r) => r.limits.wall_seconds,
            Self::Export(r) => r.limits.wall_seconds,
            Self::Views(r) => r.limits.wall_seconds,
            Self::Docs(r) => r.limits.wall_seconds,
            Self::Prepare(r) => r.limits.wall_seconds,
            Self::Gauge(r) => r.limits.wall_seconds,
            Self::Calibrate(r) => r.limits.wall_seconds,
            Self::Sweep(r) => r.limits.wall_seconds,
        }
    }

    /// The request as the JSON document the worker reads, with the protocol version.
    pub fn to_json(&self) -> Result<Value, serde_json::Error> {
        let mut value = serde_json::to_value(self)?;
        if let Value::Object(map) = &mut value {
            map.insert("protocol".into(), PROTOCOL_VERSION.into());
        }
        Ok(value)
    }
}

/// Run a model script and produce geometry, checks and views.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct BuildRequest {
    /// The script, relative to the job folder.
    pub model_path: String,
    /// Values that override the script's defaults.
    pub params: BTreeMap<String, f64>,
    pub context: EngineContext,
    pub outputs: Outputs,
    pub checks: CheckOptions,
    pub limits: EngineLimits,
}

/// Printer, material and calibration the model is built for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct EngineContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub printer: Option<PrinterContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub material: Option<MaterialContext>,
    /// Calibration table, relative to the job folder.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calibration_path: Option<String>,
}

/// The printer as the engine's checks need it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrinterContext {
    pub id: String,
    pub nozzles: Vec<NozzleContext>,
}

/// One nozzle of the printer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NozzleContext {
    /// Device extruder id.
    pub id: i64,
    pub role: String,
    pub diameter: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reach: Option<Reach>,
}

/// The box a nozzle can reach, `[low, high]` per axis in millimetres.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reach {
    pub x: [f64; 2],
    pub y: [f64; 2],
    pub z: [f64; 2],
}

/// The material the part will be printed in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaterialContext {
    pub id: String,
    #[serde(default)]
    pub family: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub density_g_cm3: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_overhang_deg: Option<f64>,
}

/// Which artefacts a build writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Outputs {
    pub step: bool,
    pub brep: bool,
    pub mesh: bool,
    pub views: Vec<String>,
    pub stl: bool,
    pub threemf: bool,
    pub view_size: [u32; 2],
}

impl Default for Outputs {
    fn default() -> Self {
        Self {
            step: true,
            brep: true,
            mesh: true,
            views: Vec::new(),
            stl: false,
            threemf: false,
            view_size: [768, 768],
        }
    }
}

impl Outputs {
    /// No artefacts at all: a build that only runs the checks.
    pub fn none() -> Self {
        Self {
            step: false,
            brep: false,
            mesh: false,
            ..Self::default()
        }
    }
}

/// The orientation `overhangs` and `bed_contact` judge a part in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckOrientation {
    /// As the script drew it.
    AsModelled,
    /// As print preparation would turn it (PROTOCOL.md section 3.5).
    Prepared,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Which checks a build runs. The three options the engine has off by default are left out
/// of the request when they are off, so such a request is the same as before they existed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CheckOptions {
    pub enabled: bool,
    pub min_wall: f64,
    pub verify_determinism: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub only: Option<Vec<String>>,
    pub sample_ranges: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orientation: Option<CheckOrientation>,
    /// Measure the model's `CONTRACT` and report the questions it leaves open.
    #[serde(skip_serializing_if = "is_false")]
    pub contract: bool,
    /// Report joints made without a feature card.
    #[serde(skip_serializing_if = "is_false")]
    pub undeclared_joints: bool,
}

impl Default for CheckOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            min_wall: 0.8,
            verify_determinism: false,
            only: None,
            sample_ranges: false,
            orientation: None,
            contract: false,
            undeclared_joints: false,
        }
    }
}

impl CheckOptions {
    /// What the product asks of every build: parts judged as they will be printed, the
    /// contract measured, hand-made joints pointed out.
    pub fn product() -> Self {
        Self {
            orientation: Some(CheckOrientation::Prepared),
            contract: true,
            undeclared_joints: true,
            ..Self::default()
        }
    }
}

/// Limits the worker applies itself. The wall-clock limit is enforced by the supervisor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EngineLimits {
    pub wall_seconds: u64,
    pub max_triangles: u64,
}

impl Default for EngineLimits {
    fn default() -> Self {
        Self {
            wall_seconds: 120,
            max_triangles: 2_000_000,
        }
    }
}

/// The most settings one `sweep` request may carry (the engine's limit).
pub const MAX_SWEEP_SETTINGS: usize = 16;

/// Build a model at a few parameter settings, one after the other, and report how each
/// fares; nothing is stored (PROTOCOL.md section 3.2). The settings are built in the model
/// process by the range sampler, so what comes back is advice for the agent, never the
/// check outcome of a version.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SweepRequest {
    /// The script, relative to the job folder.
    pub model_path: String,
    /// The values every setting starts from: the current version's parameters.
    pub params: BTreeMap<String, f64>,
    /// Each setting overrides some parameters; 1 to [`MAX_SWEEP_SETTINGS`].
    pub settings: Vec<BTreeMap<String, f64>>,
    pub context: EngineContext,
    pub min_wall: f64,
    /// No further setting is started after this many seconds of building.
    pub budget_seconds: f64,
    pub limits: EngineLimits,
}

/// Exact measurements on a finished build (`model.brep` and `model.parts.json` in the job
/// folder). Queries are passed through as JSON; the engine validates them.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct MeasureRequest {
    pub queries: Vec<Value>,
    pub limits: EngineLimits,
}

/// A file format the engine can export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ExportFormat {
    #[serde(rename = "3mf")]
    ThreeMf,
    #[serde(rename = "stl")]
    Stl,
    #[serde(rename = "step")]
    Step,
}

/// Export a finished build to interchange formats.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExportRequest {
    pub formats: Vec<ExportFormat>,
    /// Part names; `None` exports every part.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parts: Option<Vec<String>>,
    pub limits: EngineLimits,
}

/// A principal plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SectionPlane {
    XY,
    XZ,
    YZ,
}

/// Cut the model open before rendering.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Section {
    pub plane: SectionPlane,
    #[serde(default)]
    pub offset: f64,
}

/// Render views of a finished build.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ViewsRequest {
    pub views: Vec<String>,
    pub size: [u32; 2],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<Section>,
    pub limits: EngineLimits,
}

/// Search the documentation of the installed CAD libraries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DocsRequest {
    pub query: String,
    pub limit: u32,
    pub limits: EngineLimits,
}

/// The direction of a part, as modelled, that points up on the bed: `"+x"` ... `"-z"` or a
/// vector, as in the model's `PRINT` declaration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpHint {
    pub up: Value,
}

/// Turn a built version into plates (PROTOCOL.md section 3.5). The job folder holds the
/// build's `model.brep`, `model.parts.json` and calibration file.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PrepareRequest {
    pub context: EngineContext,
    /// Part names; `None` prepares every part.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parts: Option<Vec<String>>,
    /// Copies per part (default 1).
    pub copies: BTreeMap<String, u32>,
    /// Device extruder id of the nozzle that prints a part (default: the main nozzle).
    pub nozzles: BTreeMap<String, i64>,
    /// The user's choice of orientation per part, used before the model's `PRINT`.
    pub orientations: BTreeMap<String, UpHint>,
    pub gap: f64,
    pub margin: f64,
    pub threemf: bool,
    pub views: Vec<String>,
    pub view_size: [u32; 2],
    pub limits: EngineLimits,
    /// Which part of the preparation runs (the print settings interface, PROTOCOL.md
    /// section 3.5). `None` asks in the old way: no stage, no facts in the result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<PrepareStage>,
    /// Room in mm kept free round every piece for its brim, 0 to 20.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clearance: Option<f64>,
    /// Where the group of pieces lies on the plate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
    /// Boxes on the bed that pieces and brims stay off, at most [`MAX_KEEPOUTS`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub keepouts: Vec<Keepout>,
}

impl Default for PrepareRequest {
    fn default() -> Self {
        Self {
            context: EngineContext::default(),
            parts: None,
            copies: BTreeMap::new(),
            nozzles: BTreeMap::new(),
            orientations: BTreeMap::new(),
            gap: 5.0,
            margin: 2.0,
            threemf: true,
            views: vec!["iso".to_owned(), "top".to_owned()],
            view_size: [768, 768],
            limits: EngineLimits::default(),
            stage: None,
            clearance: None,
            anchor: None,
            keepouts: Vec::new(),
        }
    }
}

/// The most keep-out boxes a request may carry (the engine's limit).
pub const MAX_KEEPOUTS: usize = 16;

/// Which part of a preparation runs (there are two passes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrepareStage {
    /// Orientation and layout in one call (the engine's default).
    All,
    /// Orientations, facts and checks; no plates and no files.
    Orient,
    /// Layout of orientations chosen before: every part's `up` is pinned.
    Plate,
}

impl PrepareStage {
    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Orient => "orient",
            Self::Plate => "plate",
        }
    }
}

/// Where the laid-out group of pieces lies on the plate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    /// From the front left corner (the engine's default).
    FrontLeft,
    /// The group's box centred in the nozzles' reach ("parts mid-bed").
    Centre,
}

/// A box on the bed that pieces and their brims stay off, in bed coordinates (mm).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Keepout {
    /// `^[a-z][a-z0-9_]{0,31}$`, used once per request.
    pub id: String,
    pub min: [f64; 2],
    pub max: [f64; 2],
}

/// A feature card named by kind and public arguments, for a gauge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CardRef {
    pub card: String,
    #[serde(default)]
    pub args: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Export the test gauges of feature cards as a plate of their own.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GaugeRequest {
    pub context: EngineContext,
    /// `None`: the gauges of every card the built model in the job folder uses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cards: Option<Vec<CardRef>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nozzle_id: Option<i64>,
    pub gap: f64,
    pub margin: f64,
    pub threemf: bool,
    pub views: Vec<String>,
    pub view_size: [u32; 2],
    pub limits: EngineLimits,
}

impl Default for GaugeRequest {
    fn default() -> Self {
        Self {
            context: EngineContext::default(),
            cards: None,
            nozzle_id: None,
            gap: 5.0,
            margin: 2.0,
            threemf: true,
            views: vec!["iso".to_owned(), "top".to_owned()],
            view_size: [768, 768],
            limits: EngineLimits::default(),
        }
    }
}

/// Which kind of size a gauge dimension is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GaugeFeature {
    /// An inside size.
    Hole,
    /// An outside size.
    Peg,
}

/// One caliper reading of a printed gauge.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GaugeReading {
    pub feature: GaugeFeature,
    /// The size as drawn, from the gauge's `dimensions`.
    pub drawn: f64,
    /// What the caliper read.
    pub measured: f64,
}

/// Turn readings of a printed gauge into the updated calibration table.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CalibrateRequest {
    pub context: EngineContext,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nozzle_id: Option<i64>,
    /// When the gauge was measured (RFC 3339). The engine reads no clock.
    pub measured_at: String,
    pub readings: Vec<GaugeReading>,
    pub limits: EngineLimits,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    #[test]
    fn a_build_request_serialises_to_the_protocol_document() {
        let request = EngineRequest::Build(BuildRequest {
            model_path: "model.py".into(),
            params: BTreeMap::from([("width".to_owned(), 90.0)]),
            context: EngineContext {
                material: Some(MaterialContext {
                    id: "pla-basic".into(),
                    family: "PLA".into(),
                    density_g_cm3: None,
                    max_overhang_deg: None,
                }),
                ..EngineContext::default()
            },
            outputs: Outputs {
                views: vec!["iso".into()],
                ..Outputs::default()
            },
            ..BuildRequest::default()
        });
        assert_eq!(
            request.to_json().unwrap(),
            json!({
                "protocol": 0, "op": "build", "model_path": "model.py",
                "params": {"width": 90.0},
                "context": {"material": {"id": "pla-basic", "family": "PLA"}},
                "outputs": {"step": true, "brep": true, "mesh": true, "views": ["iso"],
                            "stl": false, "threemf": false, "view_size": [768, 768]},
                "checks": {"enabled": true, "min_wall": 0.8, "verify_determinism": false,
                           "sample_ranges": false},
                "limits": {"wall_seconds": 120, "max_triangles": 2000000}
            })
        );
    }

    #[test]
    fn the_other_ops_serialise_with_their_tag() {
        let export = EngineRequest::Export(ExportRequest {
            formats: vec![ExportFormat::ThreeMf, ExportFormat::Stl, ExportFormat::Step],
            parts: Some(vec!["lid".into()]),
            limits: EngineLimits::default(),
        });
        let value = export.to_json().unwrap();
        assert_eq!(value["op"], "export");
        assert_eq!(value["formats"], json!(["3mf", "stl", "step"]));
        let views = EngineRequest::Views(ViewsRequest {
            views: vec!["top".into()],
            size: [256, 256],
            section: Some(Section {
                plane: SectionPlane::XZ,
                offset: 1.5,
            }),
            limits: EngineLimits::default(),
        });
        assert_eq!(
            views.to_json().unwrap()["section"],
            json!({"plane": "XZ", "offset": 1.5})
        );
        assert_eq!(views.op(), "views");
    }

    #[test]
    fn the_product_options_and_the_plate_ops_serialise_as_the_protocol_says() {
        let checks = serde_json::to_value(CheckOptions::product()).unwrap();
        assert_eq!(checks["orientation"], "prepared");
        assert_eq!(checks["contract"], true);
        assert_eq!(checks["undeclared_joints"], true);
        let prepare = EngineRequest::Prepare(PrepareRequest {
            copies: BTreeMap::from([("lid".to_owned(), 2)]),
            orientations: BTreeMap::from([("lid".to_owned(), UpHint { up: json!("-z") })]),
            ..PrepareRequest::default()
        });
        let value = prepare.to_json().unwrap();
        assert_eq!(value["op"], "prepare");
        assert_eq!(value["copies"], json!({"lid": 2}));
        assert_eq!(value["orientations"], json!({"lid": {"up": "-z"}}));
        assert_eq!(value["views"], json!(["iso", "top"]));
        assert!(value.get("parts").is_none());
        // A request from before the print settings names none of the four fields.
        for key in ["stage", "clearance", "anchor", "keepouts"] {
            assert!(value.get(key).is_none(), "{key}");
        }
        let plate = EngineRequest::Prepare(PrepareRequest {
            stage: Some(PrepareStage::Plate),
            clearance: Some(6.0),
            anchor: Some(Anchor::Centre),
            margin: 5.0,
            keepouts: vec![Keepout {
                id: "start_load_line".into(),
                min: [55.0, 0.0],
                max: [145.0, 8.0],
            }],
            ..PrepareRequest::default()
        });
        let value = plate.to_json().unwrap();
        assert_eq!(value["stage"], "plate");
        assert_eq!(value["clearance"], 6.0);
        assert_eq!(value["anchor"], "centre");
        assert_eq!(value["margin"], 5.0);
        assert_eq!(
            value["keepouts"],
            json!([{"id": "start_load_line", "min": [55.0, 0.0], "max": [145.0, 8.0]}])
        );
        let calibrate = EngineRequest::Calibrate(CalibrateRequest {
            context: EngineContext::default(),
            nozzle_id: None,
            measured_at: "2026-11-17T09:00:00Z".into(),
            readings: vec![GaugeReading {
                feature: GaugeFeature::Hole,
                drawn: 6.49,
                measured: 6.31,
            }],
            limits: EngineLimits::default(),
        });
        let value = calibrate.to_json().unwrap();
        assert_eq!(
            value["readings"],
            json!([{"feature": "hole", "drawn": 6.49, "measured": 6.31}])
        );
        assert_eq!(EngineRequest::Gauge(GaugeRequest::default()).op(), "gauge");
    }
}
