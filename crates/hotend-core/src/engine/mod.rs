//! The engine worker's contract as Rust types (PROTOCOL.md section 3).
//!
//! [`EngineRequest`] is what the core writes to `request.json`; [`EngineResult`] is what it
//! accepts from `result.json`. The result is hostile input: parsing is strict (unknown fields
//! are rejected) and [`EngineResult::sanitize`] bounds every name, string and count before
//! anything else in the core looks at it.

mod request;
mod result;

pub use request::{
    Anchor, BuildRequest, CalibrateRequest, CardRef, CheckOptions, CheckOrientation, DocsRequest,
    EngineContext, EngineLimits, EngineRequest, ExportFormat, ExportRequest, GaugeFeature,
    GaugeReading, GaugeRequest, Keepout, MAX_KEEPOUTS, MAX_SWEEP_SETTINGS, MaterialContext,
    MeasureRequest, NozzleContext, Outputs, PrepareRequest, PrepareStage, PrinterContext, Reach,
    Section, SectionPlane, SweepRequest, UpHint, VIEW_NAMES, ViewsRequest,
};
pub use result::{
    Artifacts, BBox, CalibrationChange, CheckResult, CheckSeverity, CheckStatus, ComponentUse,
    DocEntry, EngineError, EngineResult, EngineStatus, Gauge, GaugeClearance, GaugeDimension,
    InvalidResult, Orientation, ParamSpec, PartSummary, Plate, PlateItem, PlateLayout, Question,
    SweepFailure, SweepReport, SweepRequirement, SweepSample, Unplaced,
};

/// The protocol version this build speaks.
pub const PROTOCOL_VERSION: u32 = 0;
