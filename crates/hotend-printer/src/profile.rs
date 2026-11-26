//! Printer facts as data: `profiles/bambu.toml`.
//!
//! Guarantees:
//! - no protocol or machine constant of a printer model lives in code; everything the other
//!   modules need (nozzle ids, reach and travel, ports, AMS ids, the G-code table, filament
//!   temperature limits) is read from the profile file;
//! - every block of facts carries a `verified` flag, and [`PrinterProfile::unverified`] lists
//!   the blocks that were not observed in real output. Today nothing was observed on a
//!   printer; the G-code table, the section markers, the main nozzle's travel box and the
//!   slice file names were observed in real slices (the file's header says which);
//! - a G-code row cannot be loaded without a limit for every parameter it accepts, and a
//!   row of unknown meaning cannot be loaded as one that leaves the nozzle where it was
//!   unless it gives the evidence;
//! - the file is parsed strictly (unknown keys are errors) and checked for consistency before
//!   anything uses it.
//!
//! Nozzles are identified by number only: [`Nozzle::device_extruder`] is the id the printer
//! reports and accepts, [`Nozzle::slicer_extruder`] the slicer's 1-based number. On the X2D
//! the two numberings are inverted, so nothing here or elsewhere maps by "main" or "left".

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Deserialize;

/// The profile file shipped with the crate.
const BUILTIN: &str = include_str!("../profiles/bambu.toml");
/// The layout version this code reads.
const SCHEMA: u32 = 5;

/// Why the profile data could not be used.
#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    /// The file is not valid TOML of the expected layout.
    #[error("the printer profile file cannot be read: {0}")]
    Parse(String),
    /// The file is well-formed but inconsistent.
    #[error("the printer profile file is inconsistent: {0}")]
    Invalid(String),
    /// No printer with this id.
    #[error("no printer profile `{0}`; known printers: {1}")]
    UnknownPrinter(String, String),
}

/// A closed interval on each axis, in millimetres, in bed coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AxisBox {
    /// `[low, high]` on X.
    pub x: [f64; 2],
    /// `[low, high]` on Y.
    pub y: [f64; 2],
    /// `[low, high]` on Z.
    pub z: [f64; 2],
    /// Whether the box was measured: on a real printer, or (a travel box) in a real slice.
    pub verified: bool,
}

impl AxisBox {
    /// Rounding slack: slicers write three decimals.
    const EPS: f64 = 1e-6;

    /// Whether `value` on `axis` (0 = X, 1 = Y, 2 = Z) is inside the box.
    pub fn contains_axis(&self, axis: usize, value: f64) -> bool {
        let [low, high] = match axis {
            0 => self.x,
            1 => self.y,
            _ => self.z,
        };
        value.is_finite() && value >= low - Self::EPS && value <= high + Self::EPS
    }

    /// The largest distance the axis can travel.
    pub fn span(&self, axis: usize) -> f64 {
        let [low, high] = match axis {
            0 => self.x,
            1 => self.y,
            _ => self.z,
        };
        high - low
    }

    fn well_formed(&self) -> bool {
        [self.x, self.y, self.z]
            .iter()
            .all(|[low, high]| low.is_finite() && high.is_finite() && low <= high)
    }

    /// The box both `self` and `other` contain.
    pub fn intersection(&self, other: &AxisBox) -> AxisBox {
        let both = |a: [f64; 2], b: [f64; 2]| [a[0].max(b[0]), a[1].min(b[1])];
        AxisBox {
            x: both(self.x, other.x),
            y: both(self.y, other.y),
            z: both(self.z, other.z),
            verified: self.verified && other.verified,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct NozzleIds {
    device_extruder: u8,
    slicer_extruder: u8,
    verified: bool,
}

/// A speed limit of one nozzle.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaxSpeed {
    /// Millimetres per second.
    pub mm_s: f64,
    /// Whether the limit was confirmed on a real printer.
    pub verified: bool,
}

/// The external spool holder that feeds one nozzle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalTray {
    /// The holder's tray id in reports and in the filament mapping.
    pub id: u8,
    /// Whether confirmed on a real printer.
    pub verified: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNozzle {
    role: String,
    drive: String,
    ids: NozzleIds,
    reach: AxisBox,
    #[serde(default)]
    travel: Option<AxisBox>,
    #[serde(default)]
    max_speed: Option<MaxSpeed>,
    #[serde(default)]
    external_tray: Option<ExternalTray>,
    source: String,
}

/// One nozzle of a printer.
#[derive(Debug, Clone, PartialEq)]
pub struct Nozzle {
    /// The vendor's word for the nozzle ("main", "auxiliary"). For display only.
    pub role: String,
    /// "direct" or "bowden". For display only.
    pub drive: String,
    /// The extruder id the printer reports and accepts.
    pub device_extruder: u8,
    /// The slicer's 1-based number of the same nozzle.
    pub slicer_extruder: u8,
    /// Whether the two ids were confirmed on a real printer.
    pub ids_verified: bool,
    /// Where this nozzle can print.
    pub reach: AxisBox,
    /// Where this nozzle may be sent without printing. `None`: not measured, and every
    /// move is held to `reach`.
    pub travel: Option<AxisBox>,
    /// The nozzle's speed limit, if it has one.
    pub max_speed: Option<MaxSpeed>,
    /// The external spool holder that feeds this nozzle, when known.
    pub external_tray: Option<ExternalTray>,
    /// Where the facts come from.
    pub source: String,
}

/// How a printer names itself.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    /// Model codes the printer reports in discovery.
    pub model_codes: Vec<String>,
    /// Prefixes of the serial numbers of this model.
    pub serial_prefixes: Vec<String>,
    /// Whether confirmed on a real printer.
    pub verified: bool,
    /// Where the facts come from.
    pub source: String,
}

/// Temperatures the machine must never be asked to exceed, whatever the filament.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MachineLimits {
    /// Highest nozzle temperature, °C.
    pub nozzle_max_c: f64,
    /// Highest bed temperature, °C.
    pub bed_max_c: f64,
    /// Highest chamber temperature, °C.
    pub chamber_max_c: f64,
    /// Whether confirmed on a real printer.
    pub verified: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLimits {
    nozzle_max_c: f64,
    bed_max_c: f64,
    chamber_max_c: f64,
    verified: bool,
    #[allow(dead_code)]
    source: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPrinter {
    id: String,
    name: String,
    family: String,
    identity: Identity,
    slice_file: SliceFileNames,
    #[serde(default)]
    plate: Vec<BuildPlate>,
    nozzle: Vec<RawNozzle>,
    limits: RawLimits,
    #[serde(default)]
    keepout: Vec<Keepout>,
}

/// A part of the bed that pieces and their brims stay off, in bed coordinates (mm).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Keepout {
    /// `^[a-z][a-z0-9_]{0,31}$`, once per printer.
    pub id: String,
    /// From and to in X, mm.
    pub x: [f64; 2],
    /// From and to in Y, mm.
    pub y: [f64; 2],
    /// Why the bed must stay free there, in words.
    pub reason: String,
    /// Whether the box was confirmed on a real printer.
    pub verified: bool,
    /// Where the facts come from.
    pub source: String,
}

impl Keepout {
    /// The most boxes one printer may name (the engine's limit).
    pub const MAX: usize = 16;

    fn well_formed(&self) -> bool {
        let mut chars = self.id.chars();
        let id_ok = self.id.len() <= 32
            && chars.next().is_some_and(|c| c.is_ascii_lowercase())
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        let axis_ok = |[low, high]: [f64; 2]| {
            low.is_finite()
                && high.is_finite()
                && low < high
                && low.abs() <= 1000.0
                && high.abs() <= 1000.0
        };
        id_ok && axis_ok(self.x) && axis_ok(self.y)
    }
}

/// Connection facts of a protocol family.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transport {
    /// MQTT over TLS.
    pub mqtt_port: u16,
    /// Implicit FTPS.
    pub ftps_port: u16,
    /// The only TLS version Hotend offers: its choice.
    pub tls: String,
    /// The fixed user name; the password is the access code.
    pub username: String,
    /// Length of the access code.
    pub access_code_length: usize,
    /// Topic the printer publishes reports on; `{serial}` is replaced.
    pub report_topic: String,
    /// Topic the printer takes requests on; `{serial}` is replaced.
    pub request_topic: String,
    /// Whether the certificate's common name is the serial number.
    pub certificate_names_serial: bool,
    /// Whether confirmed on a real printer.
    pub verified: bool,
    /// Where the facts come from.
    pub source: String,
}

/// Where an uploaded job lands and how it is named in the start command.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Storage {
    /// Which storage FTPS reaches: "external" (USB drive) or "internal".
    pub ftps_reaches: String,
    /// Prefix of the `url` of an uploaded file in `project_file`.
    pub url_prefix: String,
    /// Whether confirmed on a real printer.
    pub verified: bool,
    /// Where the facts come from.
    pub source: String,
}

/// Discovery facts.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Discovery {
    /// UDP port the printer announces itself on.
    pub port: u16,
    /// The service type a Bambu printer announces.
    pub service: String,
    /// Whether confirmed on a real printer.
    pub verified: bool,
    /// Where the facts come from.
    pub source: String,
}

/// How the four calibration flags of `project_file` are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlagType {
    /// `true` / `false`.
    Boolean,
    /// `1` / `0`.
    Integer,
}

/// Model differences of the `project_file` command.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectFileRules {
    /// Type of `bed_leveling`, `flow_cali`, `vibration_cali`, `layer_inspect`.
    pub flag_type: FlagType,
    /// Spelling of the bed levelling key.
    pub bed_leveling_key: String,
    /// Whether `ams_mapping2` is sent beside `ams_mapping`.
    pub send_ams_mapping2: bool,
    /// Whether confirmed on a real printer.
    pub verified: bool,
    /// Where the facts come from.
    pub source: String,
}

/// What the flat `ams_mapping` array holds for an AMS HT unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum FlatHt {
    /// The unit id, 128-135.
    #[serde(rename = "unit_id")]
    UnitId,
    /// 16 plus the unit's index, 16-23.
    #[serde(rename = "slot_16")]
    Slot16,
}

/// AMS numbering.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AmsRules {
    /// Trays of a regular unit.
    pub trays_per_unit: u8,
    /// Id of the first AMS HT unit.
    pub ht_unit_id_base: u8,
    /// How many AMS HT ids exist.
    pub ht_unit_count: u8,
    /// What the flat mapping array holds for an AMS HT.
    pub flat_ht: FlatHt,
    /// Tray ids of the external spool holders.
    pub external_ids: Vec<u8>,
    /// Bit position of the nozzle id in a unit's `info` bitfield.
    pub nozzle_shift: u8,
    /// Mask of the nozzle id after shifting.
    pub nozzle_mask: u32,
    /// The value that means "not bound to one nozzle".
    pub nozzle_unbound: u32,
    /// Whether confirmed on a real printer.
    pub verified: bool,
    /// Where the facts come from.
    pub source: String,
}

impl AmsRules {
    /// Whether `unit` is the id of an AMS HT (a unit with a single tray).
    pub fn is_ht(&self, unit: u8) -> bool {
        unit >= self.ht_unit_id_base
            && u16::from(unit) < u16::from(self.ht_unit_id_base) + u16::from(self.ht_unit_count)
    }

    /// The device extruder id a unit feeds, from its `info` bitfield. `None` when the unit
    /// is not bound to one nozzle.
    pub fn nozzle_of_info(&self, info: u64) -> Option<u8> {
        let value = ((info >> self.nozzle_shift) as u32) & self.nozzle_mask;
        (value != self.nozzle_unbound).then_some(value as u8)
    }
}

/// The numbers of the four speed presets.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeedLevels {
    /// Quietest preset.
    pub silent: u8,
    /// Default preset.
    pub standard: u8,
    /// Faster preset.
    pub sport: u8,
    /// Fastest preset.
    pub ludicrous: u8,
    /// Whether confirmed on a real printer.
    pub verified: bool,
    /// Where the facts come from.
    pub source: String,
}

/// What an error number refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeKind {
    /// The number in `print.print_error`.
    PrintError,
    /// One entry of `print.hms` (`attr` and `code`).
    Hms,
}

/// An error code Hotend can explain or acts on.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnownCode {
    /// Which report field the code appears in.
    pub kind: CodeKind,
    /// For `hms`: the `attr` half.
    #[serde(default)]
    pub attr: u32,
    /// The code (for `hms`: the `code` half).
    pub code: u32,
    /// A stable word for the condition: `cancelled`, `filament_mapping`, `not_authorised`, ...
    pub meaning: String,
    /// Plain-language text for a non-expert.
    pub message: String,
    /// Whether confirmed on a real printer.
    pub verified: bool,
    /// Where the fact comes from.
    pub source: String,
}

/// A part of a sliced file that has its own rows of the G-code table.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, serde::Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Section {
    /// The machine start routine.
    Start,
    /// The print. A file without section markers is body from its first line to its last.
    Body,
    /// The machine end routine.
    End,
}

impl Section {
    /// All sections, in file order.
    pub const ALL: [Section; 3] = [Section::Start, Section::Body, Section::End];

    /// The section in a sentence.
    pub fn phrase(self) -> &'static str {
        match self {
            Section::Start => "the start routine",
            Section::Body => "the print itself",
            Section::End => "the end routine",
        }
    }
}

/// What the check does with a line that matches a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GcodeClass {
    /// Linear move.
    Move,
    /// Clockwise arc.
    ArcCw,
    /// Counter-clockwise arc.
    ArcCcw,
    /// A guarded move whose end is not known.
    Probe,
    /// Homing.
    Home,
    /// Absolute positioning.
    Absolute,
    /// Relative positioning.
    Relative,
    /// Absolute extrusion.
    EAbsolute,
    /// Relative extrusion.
    ERelative,
    /// `G92`.
    SetPosition,
    /// `T<n>`: selects a declared filament.
    Tool,
    /// A tool number that is not a filament.
    ServiceTool,
    /// `M73`.
    Progress,
    /// `G4`: waits; the time counts towards the time the moves take.
    Dwell,
    /// A probing area: `X Y` is its corner, `I J` its size.
    Area,
    /// A purge line: `X Y Z` is its start, `L` its length.
    Line,
    /// Starts lines that run only if a printer flag says so.
    BranchStart,
    /// Ends them.
    BranchEnd,
    /// Starts lines that switch to the filament or service tool named by `S` and run only
    /// if the printer decides so.
    SwitchStart,
    /// Ends them.
    SwitchEnd,
    /// One of the row's verbs and one value.
    Text,
    /// One base64 word.
    Mask,
    /// Nothing is followed; every value must match the row.
    Opaque,
}

impl GcodeClass {
    /// The parameter letters whose values the class itself reads, so the row needs no
    /// limit for them.
    fn own_letters(self) -> &'static str {
        match self {
            GcodeClass::Move => "XYZEF",
            GcodeClass::ArcCw | GcodeClass::ArcCcw => "XYZIJRPEF",
            GcodeClass::Home => "XYZ",
            GcodeClass::SetPosition => "XYZE",
            GcodeClass::Area => "XYIJ",
            GcodeClass::Line => "XYZL",
            _ => "",
        }
    }
}

/// The limit of one parameter value.
#[derive(Debug, Clone, PartialEq)]
pub enum ArgSpec {
    /// A number from `.0` to `.1`, both included.
    Range(f64, f64),
    /// One of these numbers.
    OneOf(Vec<f64>),
    /// A number above 0 and at most this.
    UpTo(f64),
    /// The letter alone, without a value.
    Flag,
    /// A nozzle temperature that is set.
    NozzleTemp,
    /// A bed temperature that is set.
    BedTemp,
    /// A chamber temperature that is set.
    ChamberTemp,
    /// A value that may be a nozzle temperature; held to the nozzle limits.
    NozzleTempRef,
    /// A device extruder id of the printer.
    Nozzle,
    /// A project filament number (counted from 0) followed by the letter `A`.
    FilamentA,
    /// One of the service tool numbers.
    ServiceTool,
    /// A Z trim: 0, or the trim of the build plate the job declares.
    PlateZTrim,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ArgWord {
    Flag,
    NozzleTemp,
    BedTemp,
    ChamberTemp,
    NozzleTempRef,
    Nozzle,
    FilamentA,
    ServiceTool,
    PlateZTrim,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum RawArg {
    Range([f64; 2]),
    OneOf { one_of: Vec<f64> },
    UpTo { up_to: f64 },
    Word(ArgWord),
}

/// What a configuration-changing line changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigKind {
    /// The printer's own travel limit check (soft end stops).
    TravelLimits,
    /// Settings the printer keeps after the print.
    StoredSettings,
    /// Acceleration, speed and jerk limits.
    MotionLimits,
    /// Motor current.
    MotorCurrent,
    /// A Z offset for the print.
    ZOffset,
}

impl ConfigKind {
    /// The kind in a sentence.
    pub fn phrase(self) -> &'static str {
        match self {
            ConfigKind::TravelLimits => "the printer's own travel limit check",
            ConfigKind::StoredSettings => "stored settings",
            ConfigKind::MotionLimits => "speed and acceleration limits",
            ConfigKind::MotorCurrent => "motor current",
            ConfigKind::ZOffset => "the Z trim",
        }
    }

    /// What a change of this kind means for the person who confirms the print.
    pub fn explain(self) -> &'static str {
        match self {
            ConfigKind::TravelLimits => {
                "The printer's own travel limit check is switched. While it is off, Hotend's check of every move in this file is the only thing that keeps the nozzle inside the printer."
            }
            ConfigKind::StoredSettings => "Settings may be stored in the printer.",
            ConfigKind::MotionLimits => "Speed and acceleration limits are set for this print.",
            ConfigKind::MotorCurrent => "The motor current is changed and put back.",
            ConfigKind::ZOffset => "A small Z trim is set for this print.",
        }
    }
}

/// Where the meaning of a row comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    /// The command means the same in common firmware.
    Standard,
    /// The slicer's own routine says what the line is for.
    Stated,
    /// Worked out from the values or the place in the file.
    Inferred,
    /// Not known.
    Unknown,
}

/// The value a verb of a text command takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerbValue {
    /// A whole number from 0 to 999.
    Number,
    /// A name of letters, digits and `_`.
    Name,
    /// The material of a declared filament, or `UNKNOWN`.
    FilamentType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RawRequires {
    Relative,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCommand {
    codes: Vec<String>,
    #[serde(default)]
    sections: Option<Vec<Section>>,
    #[serde(default)]
    class: Option<GcodeClass>,
    #[serde(default)]
    forms: Option<Vec<String>>,
    #[serde(default)]
    letters: Option<String>,
    #[serde(default)]
    args: BTreeMap<String, RawArg>,
    #[serde(default)]
    moves: bool,
    #[serde(default)]
    stays_put: Option<String>,
    #[serde(default)]
    requires: Option<RawRequires>,
    #[serde(default)]
    config: Option<ConfigKind>,
    #[serde(default)]
    max_count: Option<u32>,
    #[serde(default)]
    verbs: BTreeMap<String, VerbValue>,
    #[serde(default)]
    mask_chars: Option<usize>,
    what: String,
    basis: Basis,
    verified: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDeny {
    category: String,
    codes: Vec<String>,
    why: String,
}

/// The marker comments around the machine routines and how long the routines may be.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SectionRules {
    /// Comment before the first command: the start routine follows.
    pub open: String,
    /// Comment that ends the start routine.
    pub start_end: String,
    /// Comment that starts the end routine.
    pub end_start: String,
    /// Comment after the last command.
    pub close: String,
    /// Most commands in the start routine.
    pub start_max_commands: u64,
    /// Most commands in the end routine.
    pub end_max_commands: u64,
    /// Whether the markers were seen in a real slice.
    pub verified: bool,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMaxMm {
    x: f64,
    y: f64,
    z: f64,
}

/// A signed distance range per axis: `[furthest in the negative direction, furthest in
/// the positive direction]`, in millimetres.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAxisRanges {
    x: [f64; 2],
    y: [f64; 2],
    z: [f64; 2],
}

impl RawAxisRanges {
    fn axes(self) -> [[f64; 2]; 3] {
        [self.x, self.y, self.z]
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawUnplaced {
    sections: Vec<Section>,
    plain: RawAxisRanges,
    guarded: RawAxisRanges,
    net: RawAxisRanges,
    total_mm: RawMaxMm,
    max_count: u32,
    verified: bool,
}

/// Moves written relative to a position only the printer knows. Nothing can check where
/// they end, so they are held to the direction and the distances of the stock routine.
/// Every range is `[furthest in the negative direction, furthest in the positive
/// direction]` per axis (X, Y, Z), in millimetres. A lower Z is a smaller gap between the
/// nozzle and the bed.
#[derive(Debug, Clone, PartialEq)]
pub struct UnplacedRules {
    /// The sections that may contain them.
    pub sections: Vec<Section>,
    /// What one plain move (`G0`, `G1`) may cover. It never lowers Z.
    pub plain: [[f64; 2]; 3],
    /// What one guarded move (class `probe`) may cover.
    pub guarded: [[f64; 2]; 3],
    /// Where the sum of such moves on an axis must stay, counted from where the printer
    /// itself last left that axis. For Z it never goes below 0.
    pub net: [[f64; 2]; 3],
    /// Longest distance all such moves of a file may cover per axis, both directions
    /// added up.
    pub total_mm: [f64; 3],
    /// Most such moves in a file.
    pub max_count: u32,
    /// Whether the numbers were measured in a real slice.
    pub verified: bool,
}

/// Checks the limits of unplaced moves. They are the only moves nothing else can check,
/// so a table that would let one of them close the gap to the bed is refused.
fn check_unplaced(raw: RawUnplaced) -> Result<UnplacedRules, ProfileError> {
    let invalid = |message: &str| {
        Err(ProfileError::Invalid(format!(
            "the limits of unplaced moves are unusable: {message}"
        )))
    };
    let (plain, guarded, net) = (raw.plain.axes(), raw.guarded.axes(), raw.net.axes());
    let total_mm = [raw.total_mm.x, raw.total_mm.y, raw.total_mm.z];
    let is_range =
        |[low, high]: [f64; 2]| low.is_finite() && high.is_finite() && low <= 0.0 && high >= 0.0;
    if ![plain, guarded, net]
        .iter()
        .flatten()
        .all(|range| is_range(*range))
    {
        return invalid("every range is [a distance of 0 or less, a distance of 0 or more]");
    }
    if total_mm
        .iter()
        .any(|total| !total.is_finite() || *total < 0.0)
    {
        return invalid("a total distance is 0 or more");
    }
    // Z is index 2. A plain move is not guarded, so it never lowers the nozzle; and the
    // sum of the moves never ends below where the printer left the nozzle.
    if plain[2][0] != 0.0 || net[2][0] != 0.0 {
        return invalid(
            "neither a plain move nor the sum of the moves may lower Z from a position that is not known",
        );
    }
    for axis in 0..3 {
        // One move stays within what the moves may add up to. Only a guarded move may go
        // back, and no further than a lift can have gone.
        let within = plain[axis][0] >= net[axis][0]
            && plain[axis][1] <= net[axis][1]
            && guarded[axis][1] <= net[axis][1]
            && guarded[axis][0] >= net[axis][0] - net[axis][1];
        if !within {
            return invalid("one move may go further than the moves may add up to");
        }
    }
    Ok(UnplacedRules {
        sections: raw.sections,
        plain,
        guarded,
        net,
        total_mm,
        max_count: raw.max_count,
        verified: raw.verified,
    })
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGcode {
    verified: bool,
    source: String,
    sections: SectionRules,
    unplaced: RawUnplaced,
    limits: MoveLimits,
    command: Vec<RawCommand>,
    deny: Vec<RawDeny>,
}

/// One row of the G-code table: a command word in one exact shape.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandRow {
    /// Position of the row in the table; the check counts uses by it.
    pub index: usize,
    /// The sections the row applies in.
    pub sections: Vec<Section>,
    /// What the check does with the line.
    pub class: GcodeClass,
    /// The accepted sets of parameter letters, each sorted. Empty when `letters` is set.
    pub forms: Vec<String>,
    /// For moves: any subset of these letters is accepted.
    pub letters: Option<String>,
    /// The limit of each parameter the class does not read itself.
    pub args: BTreeMap<u8, ArgSpec>,
    /// After the line the nozzle is somewhere the file does not say.
    pub moves: bool,
    /// The line is accepted only in relative positioning.
    pub requires_relative: bool,
    /// Set when the line changes limits or stored configuration.
    pub config: Option<ConfigKind>,
    /// Most uses of the row in one file.
    pub max_count: Option<u32>,
    /// For a text command: the accepted first words and the value each takes.
    pub verbs: BTreeMap<String, VerbValue>,
    /// For a mask command: the length of its one word.
    pub mask_chars: Option<usize>,
    /// What the line does, or "unknown".
    pub what: String,
    /// Where `what` comes from.
    pub basis: Basis,
    /// Whether the row was seen in a real slice, with these letters and values in its limits.
    pub verified: bool,
}

impl CommandRow {
    /// Whether the row applies in `section`.
    pub fn applies_in(&self, section: Section) -> bool {
        self.sections.contains(&section)
    }

    /// Whether the row accepts exactly this sorted set of parameter letters.
    pub fn accepts(&self, sorted_letters: &str) -> bool {
        match &self.letters {
            Some(letters) => sorted_letters
                .bytes()
                .all(|l| letters.as_bytes().contains(&l)),
            None => self.forms.iter().any(|form| form == sorted_letters),
        }
    }

    /// Whether `letter` is a parameter of the row in any of its forms.
    pub fn takes(&self, letter: u8) -> bool {
        match &self.letters {
            Some(letters) => letters.as_bytes().contains(&letter),
            None => self
                .forms
                .iter()
                .any(|form| form.as_bytes().contains(&letter)),
        }
    }
}

/// A refused G-code command.
#[derive(Debug, Clone, PartialEq)]
pub struct DeniedCommand {
    /// `eeprom`, `firmware`, `configuration`, `network`, `storage` or `pins`.
    pub category: String,
    /// Why it is refused.
    pub why: String,
}

/// The G-code a printer family may be sent.
#[derive(Debug, Clone, PartialEq)]
pub struct GcodeRules {
    /// The rows by command word (`"G29.1"`; `"T"` for a filament selection, `"T65279"` for
    /// one tool number).
    pub rows: BTreeMap<String, Vec<CommandRow>>,
    /// How many rows there are (the highest `index` plus one).
    pub row_count: usize,
    /// Commands refused with a reason, by upper-case code, wherever no row accepts them.
    pub deny: BTreeMap<String, DeniedCommand>,
    /// Tool numbers that are not filaments.
    pub service_tools: Vec<u32>,
    /// The section markers and limits.
    pub sections: SectionRules,
    /// The limits of moves from a position only the printer knows.
    pub unplaced: UnplacedRules,
    /// What the moves of a file may do beyond staying inside the nozzle's boxes.
    pub limits: MoveLimits,
    /// Whether the table was written from a real slice.
    pub verified: bool,
    /// Where the table comes from.
    pub source: String,
}

impl GcodeRules {
    /// The rows of a command word; empty when the table does not have it.
    pub fn rows_of(&self, code: &str) -> &[CommandRow] {
        self.rows.get(code).map_or(&[], Vec::as_slice)
    }

    /// Every row, in table order.
    pub fn all_rows(&self) -> impl Iterator<Item = (&str, &CommandRow)> {
        let mut rows: Vec<(&str, &CommandRow)> = self
            .rows
            .iter()
            .flat_map(|(code, rows)| rows.iter().map(move |row| (code.as_str(), row)))
            .collect();
        rows.sort_by_key(|(_, row)| row.index);
        rows.into_iter()
    }
}

/// What a file sliced for a printer says about itself, and what its archive may hold.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SliceFileNames {
    /// The values of `printer_model` a file for this printer carries.
    pub printer_models: Vec<String>,
    /// The values of `printer_settings_id` a file for this printer carries.
    pub printer_settings_ids: Vec<String>,
    /// The nozzle sizes the G-code table was written from, in millimetres.
    pub nozzle_diameters_mm: Vec<f64>,
    /// The names an archive entry may have; `{plate}` is the declared plate's number and
    /// `{n}` any number.
    pub entries: Vec<String>,
    /// The entries the check reads and therefore needs, in the same notation.
    pub required_entries: Vec<String>,
    /// Whether the names were seen in a real slice.
    pub verified: bool,
    /// Where the names come from.
    pub source: String,
}

/// Whether `name` is `pattern` with `{plate}` replaced by `plate` and `{n}` by a number
/// of one to six digits without a leading zero.
fn entry_matches(pattern: &str, name: &str, plate: u32) -> bool {
    let pattern = pattern.replace("{plate}", &plate.to_string());
    match pattern.split_once("{n}") {
        None => pattern == name,
        Some((before, after)) => name
            .strip_prefix(before)
            .and_then(|rest| rest.strip_suffix(after))
            .is_some_and(|digits| {
                (1..=6).contains(&digits.len())
                    && digits.bytes().all(|b| b.is_ascii_digit())
                    && !digits.starts_with('0')
            }),
    }
}

impl SliceFileNames {
    /// Whether an archive for `plate` may hold an entry of this name.
    pub fn allows_entry(&self, name: &str, plate: u32) -> bool {
        self.entries
            .iter()
            .any(|pattern| entry_matches(pattern, name, plate))
    }

    /// The names of the entries an archive for `plate` must hold.
    pub fn required_for(&self, plate: u32) -> Vec<String> {
        self.required_entries
            .iter()
            .map(|pattern| pattern.replace("{plate}", &plate.to_string()))
            .collect()
    }

    /// Whether the G-code table was written from this nozzle size.
    pub fn knows_nozzle(&self, diameter_mm: f64) -> bool {
        self.nozzle_diameters_mm
            .iter()
            .any(|known| (known - diameter_mm).abs() < 1e-6)
    }
}

/// A build plate a job can be sliced for.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildPlate {
    /// Hotend's own id of the plate (`cool`, `textured-pei`): what the settings store and
    /// what a slice is recorded with. It is the id of the same plate in the slicer
    /// layer's `system-profiles.toml` and the token of the print settings' plate type.
    pub id: String,
    /// The plate as `curr_bed_type` names it; also what a person is shown.
    pub name: String,
    /// The plate as `bed_type` in `Metadata/plate_<n>.json` names it, when known.
    #[serde(default)]
    pub json_name: Option<String>,
    /// Highest bed temperature a file for this plate may command, °C.
    pub bed_max_c: f64,
    /// The Z trim the stock start routine sets for this plate, mm, when it was seen in
    /// a real slice. A file for a plate without one cannot set a trim other than 0.
    #[serde(default)]
    pub z_trim_mm: Option<f64>,
    /// Whether the names were seen in a real slice.
    pub verified: bool,
    /// Where the facts come from.
    pub source: String,
}

/// What the moves of a file may do beyond staying inside the nozzle's boxes. Each number
/// was measured in a real slice; `profiles/bambu.toml` says what was seen and which
/// margin was chosen.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoveLimits {
    /// The lowest height any move may name, mm.
    pub min_z_mm: f64,
    /// How far above the highest plastic laid so far plastic may be laid, mm.
    pub max_layer_step_mm: f64,
    /// The highest feed rate a move may carry, mm/min.
    pub max_feed_mm_min: f64,
    /// Most filament pushed per millimetre of path.
    pub max_e_per_mm: f64,
    /// Most filament pulled back and not yet pushed forward again, mm.
    pub max_retracted_mm: f64,
    /// By how much a push in place may exceed what was pulled back, for the rounding of
    /// the written values, mm.
    pub in_place_slack_mm: f64,
    /// The lowest nozzle temperature that must be in force when filament is pushed, °C.
    pub min_extrude_c: f64,
    /// The filament's diameter, mm.
    pub filament_diameter_mm: f64,
    /// The densities a stated weight may imply for the measured length, g/cm³.
    pub density_g_cm3: [f64; 2],
    /// Fraction by which a stated filament length may differ from the measured one.
    pub length_tolerance: f64,
    /// Millimetres by which it may differ on top of that.
    pub length_slack_mm: f64,
    /// The stated time may be at most the moves' time times this, plus the overhead.
    pub time_max_factor: f64,
    /// Seconds a stated time may add for heating, levelling and the routines.
    pub time_overhead_s: f64,
    /// Most lines of G-code that are read.
    pub max_lines: u64,
    /// Whether the numbers were measured in a real slice.
    pub verified: bool,
}

impl MoveLimits {
    fn well_formed(&self) -> bool {
        let positive = [
            self.min_z_mm,
            self.max_layer_step_mm,
            self.max_feed_mm_min,
            self.max_e_per_mm,
            self.max_retracted_mm,
            self.in_place_slack_mm,
            self.min_extrude_c,
            self.filament_diameter_mm,
            self.density_g_cm3[0],
            self.length_slack_mm,
            self.time_max_factor,
            self.time_overhead_s,
        ];
        positive.iter().all(|n| n.is_finite() && *n > 0.0)
            && self.density_g_cm3[1].is_finite()
            && self.density_g_cm3[0] <= self.density_g_cm3[1]
            && (0.0..1.0).contains(&self.length_tolerance)
            && self.time_max_factor >= 1.0
            && self.max_lines > 0
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFamily {
    id: String,
    name: String,
    transport: Transport,
    storage: Storage,
    discovery: Discovery,
    project_file: ProjectFileRules,
    ams: AmsRules,
    speed_levels: SpeedLevels,
    #[serde(default)]
    known_code: Vec<KnownCode>,
    gcode: RawGcode,
}

/// A protocol generation shared by several printer models.
#[derive(Debug, Clone, PartialEq)]
pub struct Family {
    /// Stable id, for example `bambu-h2`.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Connection facts.
    pub transport: Transport,
    /// Storage facts.
    pub storage: Storage,
    /// Discovery facts.
    pub discovery: Discovery,
    /// `project_file` differences.
    pub project_file: ProjectFileRules,
    /// AMS numbering.
    pub ams: AmsRules,
    /// Speed presets.
    pub speed_levels: SpeedLevels,
    /// Error codes with a known meaning.
    pub known_codes: Vec<KnownCode>,
    /// The G-code table: every line of a sliced file must match one of its rows.
    pub gcode: GcodeRules,
}

/// Temperature limits of a filament family.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilamentLimits {
    /// Family name, for example `PLA` or `PLA-CF`.
    pub family: String,
    /// Other names that mean this family, for example product names.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Highest nozzle temperature, °C.
    pub nozzle_max_c: f64,
    /// Highest bed temperature, °C.
    pub bed_max_c: f64,
    /// Highest chamber temperature, °C.
    pub chamber_max_c: f64,
    /// Whether confirmed.
    pub verified: bool,
    /// Where the numbers come from.
    pub source: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    schema: u32,
    revision: String,
    printer: Vec<RawPrinter>,
    family: Vec<RawFamily>,
    filament: Vec<FilamentLimits>,
}

/// Everything known about one printer model: the model's own facts, its protocol family and
/// the filament limits. Cheap to clone.
#[derive(Debug, Clone)]
pub struct PrinterProfile {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    id: String,
    name: String,
    revision: String,
    identity: Identity,
    slice_file: SliceFileNames,
    plates: Vec<BuildPlate>,
    nozzles: Vec<Nozzle>,
    limits: MachineLimits,
    family: Family,
    filaments: Vec<FilamentLimits>,
    keepouts: Vec<Keepout>,
}

/// The parsed profile file.
#[derive(Debug, Clone)]
pub struct Profiles {
    revision: String,
    printers: Vec<PrinterProfile>,
}

/// Normal form of a material family: upper case without spaces, dashes or underscores.
fn normal_family(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, ' ' | '-' | '_'))
        .flat_map(char::to_uppercase)
        .collect()
}

impl Profiles {
    /// The profiles shipped with this crate.
    pub fn builtin() -> Result<Self, ProfileError> {
        Self::from_toml(BUILTIN)
    }

    /// Parses and checks a profile file.
    pub fn from_toml(text: &str) -> Result<Self, ProfileError> {
        let raw: RawFile = toml::from_str(text).map_err(|e| ProfileError::Parse(e.to_string()))?;
        let invalid = |message: String| Err(ProfileError::Invalid(message));
        if raw.schema != SCHEMA {
            return invalid(format!(
                "schema {} is not the schema this version reads ({SCHEMA})",
                raw.schema
            ));
        }
        for limits in &raw.filament {
            let numbers = [limits.nozzle_max_c, limits.bed_max_c, limits.chamber_max_c];
            if limits.family.trim().is_empty()
                || numbers.iter().any(|n| !n.is_finite() || *n <= 0.0)
            {
                return invalid(format!("filament `{}` has unusable limits", limits.family));
            }
        }
        // A material must name one family, so no two names may be the same.
        let mut names: Vec<String> = raw
            .filament
            .iter()
            .flat_map(|limits| std::iter::once(&limits.family).chain(&limits.aliases))
            .map(|name| normal_family(name))
            .collect();
        names.sort();
        if names.iter().any(String::is_empty) || names.windows(2).any(|pair| pair[0] == pair[1]) {
            return invalid("two filament families or aliases have the same name".to_owned());
        }
        let mut families = Vec::new();
        for family in raw.family {
            families.push(check_family(family)?);
        }
        let mut printers: Vec<PrinterProfile> = Vec::new();
        for printer in raw.printer {
            if printers.iter().any(|known| known.id() == printer.id) {
                return invalid(format!("printer `{}` is listed twice", printer.id));
            }
            let Some(family) = families.iter().find(|f| f.id == printer.family) else {
                return invalid(format!(
                    "printer `{}` names the unknown family `{}`",
                    printer.id, printer.family
                ));
            };
            if printer.nozzle.is_empty() {
                return invalid(format!("printer `{}` has no nozzle", printer.id));
            }
            let plain_name = |name: &str| {
                !name.is_empty()
                    && name.len() <= 40
                    && name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
            };
            let plain_id = |id: &str| {
                !id.is_empty()
                    && id.len() <= 40
                    && id
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            };
            for (at, plate) in printer.plate.iter().enumerate() {
                let names_ok = plain_name(&plate.name)
                    && plain_id(&plate.id)
                    && plate.json_name.as_deref().is_none_or(plain_name)
                    && !printer.plate[..at].iter().any(|known| {
                        known.name == plate.name
                            || known.id == plate.id
                            || (known.json_name.is_some() && known.json_name == plate.json_name)
                    });
                // A trim is added to every height of the file: never below zero, and never
                // more than a quarter of a 0.2 mm layer.
                let trim_ok = plate
                    .z_trim_mm
                    .is_none_or(|trim| trim.is_finite() && (0.0..=0.05).contains(&trim));
                if !names_ok || !trim_ok || !plate.bed_max_c.is_finite() || plate.bed_max_c <= 0.0 {
                    return invalid(format!(
                        "build plate `{}` of `{}` has an unusable name or limit",
                        plate.name, printer.id
                    ));
                }
            }
            let files = &printer.slice_file;
            let names_ok = files
                .printer_models
                .iter()
                .chain(&files.printer_settings_ids)
                .all(|name| plain_name(name));
            let sizes_ok = !files.nozzle_diameters_mm.is_empty()
                && files
                    .nozzle_diameters_mm
                    .iter()
                    .all(|size| size.is_finite() && *size > 0.0 && *size <= 2.0);
            let entries_ok = !files.entries.is_empty()
                && files
                    .required_entries
                    .iter()
                    .all(|required| files.entries.contains(required))
                && files.entries.iter().all(|entry| {
                    entry.is_ascii() && !entry.is_empty() && entry.matches("{n}").count() <= 1
                });
            if !names_ok || !sizes_ok || !entries_ok {
                return invalid(format!(
                    "printer `{}` has unusable slice file facts",
                    printer.id
                ));
            }
            let mut nozzles: Vec<Nozzle> = Vec::new();
            for nozzle in printer.nozzle {
                let boxes_ok = nozzle.reach.well_formed()
                    && nozzle.travel.as_ref().is_none_or(AxisBox::well_formed);
                if !boxes_ok || nozzle.ids.slicer_extruder == 0 {
                    return invalid(format!(
                        "nozzle `{}` of `{}` has an unusable reach or id",
                        nozzle.role, printer.id
                    ));
                }
                if nozzles.iter().any(|known| {
                    known.device_extruder == nozzle.ids.device_extruder
                        || known.slicer_extruder == nozzle.ids.slicer_extruder
                }) {
                    return invalid(format!("printer `{}` repeats a nozzle id", printer.id));
                }
                nozzles.push(Nozzle {
                    role: nozzle.role,
                    drive: nozzle.drive,
                    device_extruder: nozzle.ids.device_extruder,
                    slicer_extruder: nozzle.ids.slicer_extruder,
                    ids_verified: nozzle.ids.verified,
                    reach: nozzle.reach,
                    travel: nozzle.travel,
                    max_speed: nozzle.max_speed,
                    external_tray: nozzle.external_tray,
                    source: nozzle.source,
                });
            }
            let limits = MachineLimits {
                nozzle_max_c: printer.limits.nozzle_max_c,
                bed_max_c: printer.limits.bed_max_c,
                chamber_max_c: printer.limits.chamber_max_c,
                verified: printer.limits.verified,
            };
            if [limits.nozzle_max_c, limits.bed_max_c, limits.chamber_max_c]
                .iter()
                .any(|n| !n.is_finite() || *n <= 0.0)
            {
                return invalid(format!("printer `{}` has unusable limits", printer.id));
            }
            let keepouts_ok = printer.keepout.len() <= Keepout::MAX
                && printer.keepout.iter().enumerate().all(|(at, zone)| {
                    zone.well_formed()
                        && !printer.keepout[..at]
                            .iter()
                            .any(|known| known.id == zone.id)
                });
            if !keepouts_ok {
                return invalid(format!(
                    "printer `{}` has an unusable keep-out zone",
                    printer.id
                ));
            }
            printers.push(PrinterProfile {
                inner: Arc::new(Inner {
                    id: printer.id,
                    name: printer.name,
                    revision: raw.revision.clone(),
                    identity: printer.identity,
                    slice_file: printer.slice_file,
                    plates: printer.plate,
                    nozzles,
                    limits,
                    family: family.clone(),
                    filaments: raw.filament.clone(),
                    keepouts: printer.keepout,
                }),
            });
        }
        if printers.is_empty() {
            return invalid("no printer is defined".to_owned());
        }
        Ok(Self {
            revision: raw.revision,
            printers,
        })
    }

    /// The file's revision string.
    pub fn revision(&self) -> &str {
        &self.revision
    }

    /// All printer models.
    pub fn printers(&self) -> &[PrinterProfile] {
        &self.printers
    }

    /// The profile of the printer model `id` (for example `bambu-x2d`).
    pub fn printer(&self, id: &str) -> Result<PrinterProfile, ProfileError> {
        self.printers
            .iter()
            .find(|printer| printer.id() == id)
            .cloned()
            .ok_or_else(|| {
                let known: Vec<&str> = self.printers.iter().map(PrinterProfile::id).collect();
                ProfileError::UnknownPrinter(id.chars().take(60).collect(), known.join(", "))
            })
    }

    /// The model whose identity lists `model_code` (as reported in discovery).
    pub fn printer_for_model_code(&self, model_code: &str) -> Option<PrinterProfile> {
        self.printers
            .iter()
            .find(|printer| {
                printer
                    .identity()
                    .model_codes
                    .iter()
                    .any(|code| code.eq_ignore_ascii_case(model_code))
            })
            .cloned()
    }
}

/// A sorted, duplicate-free set of upper-case letters, or `None`.
fn letter_set(text: &str) -> Option<String> {
    let mut letters: Vec<u8> = text.bytes().collect();
    letters.sort_unstable();
    let distinct = letters.windows(2).all(|pair| pair[0] != pair[1]);
    (distinct && letters.iter().all(u8::is_ascii_uppercase))
        .then(|| letters.into_iter().map(char::from).collect())
}

fn check_gcode(raw: RawGcode) -> Result<GcodeRules, ProfileError> {
    let invalid = |message: String| Err(ProfileError::Invalid(message));
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    let well_formed_code = |code: &str| {
        matches!(code.as_bytes().first(), Some(b'G' | b'M'))
            && code.len() <= 12
            && code[1..].split('.').count() <= 2
            && code[1..].split('.').all(digits)
    };

    let mut rows: BTreeMap<String, Vec<CommandRow>> = BTreeMap::new();
    let mut service_tools = Vec::new();
    let mut index = 0;
    for entry in raw.command {
        let named = format!("G-code row {:?}", entry.codes);
        let class = entry.class.unwrap_or(GcodeClass::Opaque);
        let sections = entry.sections.unwrap_or_else(|| Section::ALL.to_vec());
        if sections.is_empty() || entry.codes.is_empty() {
            return invalid(format!("{named} has no code or no section"));
        }
        // Either free letters (moves) or exact forms; never a `G` parameter.
        let free = matches!(
            class,
            GcodeClass::Move | GcodeClass::ArcCw | GcodeClass::ArcCcw | GcodeClass::SetPosition
        );
        let (forms, letters) = match (entry.forms, entry.letters) {
            (None, Some(letters)) if free => match letter_set(&letters) {
                Some(letters) => (Vec::new(), Some(letters)),
                None => return invalid(format!("{named}: `letters` is not a set of letters")),
            },
            (forms, None) if !free => {
                let forms = forms.unwrap_or_else(|| vec![String::new()]);
                let sorted: Option<Vec<String>> = forms.iter().map(|f| letter_set(f)).collect();
                match sorted {
                    Some(sorted) if !sorted.is_empty() => (sorted, None),
                    _ => return invalid(format!("{named}: a form is not a set of letters")),
                }
            }
            _ => {
                return invalid(format!(
                    "{named}: moves take `letters`, every other class takes `forms`"
                ));
            }
        };
        let all_letters: String = letters.clone().unwrap_or_else(|| forms.concat());
        if all_letters.contains('G') {
            return invalid(format!("{named}: G is never a parameter letter"));
        }
        let mut args = BTreeMap::new();
        for (letter, spec) in entry.args {
            let &[letter] = letter.as_bytes() else {
                return invalid(format!("{named}: `{letter}` is not one parameter letter"));
            };
            let spec = match spec {
                RawArg::Range([low, high])
                    if low.is_finite() && high.is_finite() && low <= high =>
                {
                    ArgSpec::Range(low, high)
                }
                RawArg::OneOf { one_of }
                    if !one_of.is_empty() && one_of.iter().all(|n| n.is_finite()) =>
                {
                    ArgSpec::OneOf(one_of)
                }
                RawArg::UpTo { up_to } if up_to.is_finite() && up_to > 0.0 => ArgSpec::UpTo(up_to),
                RawArg::Word(word) => match word {
                    ArgWord::Flag => ArgSpec::Flag,
                    ArgWord::NozzleTemp => ArgSpec::NozzleTemp,
                    ArgWord::BedTemp => ArgSpec::BedTemp,
                    ArgWord::ChamberTemp => ArgSpec::ChamberTemp,
                    ArgWord::NozzleTempRef => ArgSpec::NozzleTempRef,
                    ArgWord::Nozzle => ArgSpec::Nozzle,
                    ArgWord::FilamentA => ArgSpec::FilamentA,
                    ArgWord::ServiceTool => ArgSpec::ServiceTool,
                    ArgWord::PlateZTrim => ArgSpec::PlateZTrim,
                },
                _ => {
                    return invalid(format!(
                        "{named}: the limit of {} is unusable",
                        letter as char
                    ));
                }
            };
            if !all_letters.as_bytes().contains(&letter) {
                return invalid(format!(
                    "{named}: a limit is given for {}, which no form has",
                    letter as char
                ));
            }
            args.insert(letter, spec);
        }
        // Every value is either read by the class or limited by the row.
        let own = class.own_letters().as_bytes();
        if let Some(letter) = all_letters
            .bytes()
            .find(|letter| !own.contains(letter) && !args.contains_key(letter))
        {
            return invalid(format!(
                "{named}: parameter {} has no limit",
                letter as char
            ));
        }
        let is_text = class == GcodeClass::Text;
        let is_mask = class == GcodeClass::Mask;
        let mask_ok = entry
            .mask_chars
            .is_some_and(|chars| (4..=64).contains(&chars) && chars % 4 == 0);
        if is_text == entry.verbs.is_empty()
            || is_mask != entry.mask_chars.is_some()
            || (is_mask && !mask_ok)
            || ((is_text || is_mask) && forms != [String::new()])
        {
            return invalid(format!(
                "{named}: `verbs` or `mask_chars` do not fit the class"
            ));
        }
        let unknown = entry.basis == Basis::Unknown;
        if unknown != entry.what.starts_with("unknown") {
            return invalid(format!(
                "{named}: a row of unknown meaning says \"unknown\", and no other row does"
            ));
        }
        let evidence = entry
            .stays_put
            .as_deref()
            .is_some_and(|why| !why.trim().is_empty());
        if (unknown && !entry.moves && !evidence) || (entry.moves && entry.stays_put.is_some()) {
            return invalid(format!(
                "{named}: a command of unknown meaning may move the nozzle (`moves = true`) unless `stays_put` says why it does not"
            ));
        }
        if entry.config.is_some() && entry.max_count.is_none() {
            return invalid(format!(
                "{named}: a configuration command needs `max_count`"
            ));
        }
        for code in entry.codes {
            let tool_number = code.strip_prefix('T').filter(|rest| digits(rest));
            let code_ok = match class {
                GcodeClass::Tool => code == "T",
                GcodeClass::ServiceTool => tool_number.is_some(),
                _ => well_formed_code(&code),
            };
            if !code_ok {
                return invalid(format!("`{code}` is not a command name for its class"));
            }
            if let (GcodeClass::ServiceTool, Some(number)) = (class, tool_number) {
                match number.parse::<u32>() {
                    Ok(number) => service_tools.push(number),
                    Err(_) => return invalid(format!("`{code}` is not a tool number")),
                }
            }
            let existing = rows.entry(code.clone()).or_default();
            // A line must match at most one row, and a text or mask command has one shape.
            let clash = existing.iter().any(|known: &CommandRow| {
                let shared = sections.iter().any(|section| known.applies_in(*section));
                let special = |class| matches!(class, GcodeClass::Text | GcodeClass::Mask);
                shared
                    && (special(known.class)
                        || special(class)
                        || known.letters.is_some()
                        || letters.is_some()
                        || forms.iter().any(|form| known.forms.contains(form)))
            });
            if clash {
                return invalid(format!(
                    "`{code}` has two rows for the same section and form"
                ));
            }
            existing.push(CommandRow {
                index,
                sections: sections.clone(),
                class,
                forms: forms.clone(),
                letters: letters.clone(),
                args: args.clone(),
                moves: entry.moves,
                requires_relative: entry.requires == Some(RawRequires::Relative),
                config: entry.config,
                max_count: entry.max_count,
                verbs: entry.verbs.clone(),
                mask_chars: entry.mask_chars,
                what: entry.what.clone(),
                basis: entry.basis,
                verified: entry.verified,
            });
            index += 1;
        }
    }
    let mut deny = BTreeMap::new();
    for entry in raw.deny {
        for code in entry.codes {
            if !well_formed_code(&code) {
                return invalid(format!("`{code}` is not a command name"));
            }
            deny.insert(
                code,
                DeniedCommand {
                    category: entry.category.clone(),
                    why: entry.why.clone(),
                },
            );
        }
    }
    let sections = raw.sections;
    let markers = [
        &sections.open,
        &sections.start_end,
        &sections.end_start,
        &sections.close,
    ];
    let marker_ok = |marker: &String| {
        !marker.is_empty()
            && marker.len() <= 64
            && marker
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    };
    let distinct = (0..markers.len()).all(|a| (0..a).all(|b| markers[a] != markers[b]));
    if !markers.iter().all(|marker| marker_ok(marker)) || !distinct {
        return invalid("the section markers must be four different upper-case words".to_owned());
    }
    let unplaced = check_unplaced(raw.unplaced)?;
    if !raw.limits.well_formed() {
        return invalid("the move limits must be positive numbers".to_owned());
    }
    Ok(GcodeRules {
        rows,
        row_count: index,
        deny,
        service_tools,
        sections,
        unplaced,
        limits: raw.limits,
        verified: raw.verified,
        source: raw.source,
    })
}

fn check_family(raw: RawFamily) -> Result<Family, ProfileError> {
    let invalid = |message: String| Err(ProfileError::Invalid(message));
    if raw.transport.tls != "1.2" {
        return invalid(format!(
            "family `{}` asks for TLS {}, which this version cannot pin",
            raw.id, raw.transport.tls
        ));
    }
    for topic in [&raw.transport.report_topic, &raw.transport.request_topic] {
        if !topic.contains("{serial}") {
            return invalid(format!("topic `{topic}` has no {{serial}} placeholder"));
        }
    }
    if raw.ams.trays_per_unit == 0 || raw.ams.nozzle_shift > 56 {
        return invalid(format!("family `{}` has unusable AMS rules", raw.id));
    }
    Ok(Family {
        id: raw.id,
        name: raw.name,
        transport: raw.transport,
        storage: raw.storage,
        discovery: raw.discovery,
        project_file: raw.project_file,
        ams: raw.ams,
        speed_levels: raw.speed_levels,
        known_codes: raw.known_code,
        gcode: check_gcode(raw.gcode)?,
    })
}

impl PrinterProfile {
    /// The model's id, for example `bambu-x2d`.
    pub fn id(&self) -> &str {
        &self.inner.id
    }

    /// The model's display name.
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// Revision of the profile file the facts were read from.
    pub fn revision(&self) -> &str {
        &self.inner.revision
    }

    /// How the model names itself.
    pub fn identity(&self) -> &Identity {
        &self.inner.identity
    }

    /// What a file sliced for this model says about itself.
    pub fn slice_file(&self) -> &SliceFileNames {
        &self.inner.slice_file
    }

    /// The nozzles.
    pub fn nozzles(&self) -> &[Nozzle] {
        &self.inner.nozzles
    }

    /// The nozzle with this device extruder id.
    pub fn nozzle_by_device_id(&self, device_extruder: u8) -> Option<&Nozzle> {
        self.inner
            .nozzles
            .iter()
            .find(|nozzle| nozzle.device_extruder == device_extruder)
    }

    /// The nozzle with this slicer extruder number.
    pub fn nozzle_by_slicer_id(&self, slicer_extruder: u8) -> Option<&Nozzle> {
        self.inner
            .nozzles
            .iter()
            .find(|nozzle| nozzle.slicer_extruder == slicer_extruder)
    }

    /// The machine's own temperature limits.
    pub fn limits(&self) -> &MachineLimits {
        &self.inner.limits
    }

    /// The parts of the bed that pieces and their brims stay off.
    pub fn keepouts(&self) -> &[Keepout] {
        &self.inner.keepouts
    }

    /// The protocol family.
    pub fn family(&self) -> &Family {
        &self.inner.family
    }

    /// Limits of the filament family `material` names: the family itself or one of its
    /// aliases, compared without case, spaces, dashes and underscores. Nothing matches by
    /// prefix (`PCTG` is not `PC`). `None` when no family matches.
    pub fn filament_limits(&self, material: &str) -> Option<&FilamentLimits> {
        let wanted = normal_family(material);
        if wanted.is_empty() {
            return None;
        }
        self.inner.filaments.iter().find(|limits| {
            normal_family(&limits.family) == wanted
                || limits
                    .aliases
                    .iter()
                    .any(|alias| normal_family(alias) == wanted)
        })
    }

    /// The build plates a job for this printer can be sliced for.
    pub fn plates(&self) -> &[BuildPlate] {
        &self.inner.plates
    }

    /// The build plate `curr_bed_type` names as `name`.
    pub fn plate(&self, name: &str) -> Option<&BuildPlate> {
        self.inner.plates.iter().find(|plate| plate.name == name)
    }

    /// The build plate with Hotend's id `id` (`cool`, `textured-pei`): how an
    /// application that stores the person's choice of plate finds the name to declare.
    pub fn plate_by_id(&self, id: &str) -> Option<&BuildPlate> {
        self.inner.plates.iter().find(|plate| plate.id == id)
    }

    /// The meaning of a `print_error` number, if it has one.
    pub fn known_print_error(&self, code: u32) -> Option<&KnownCode> {
        self.inner
            .family
            .known_codes
            .iter()
            .find(|known| known.kind == CodeKind::PrintError && known.code == code)
    }

    /// The meaning of an HMS entry, if it has one.
    pub fn known_hms(&self, attr: u32, code: u32) -> Option<&KnownCode> {
        self.inner
            .family
            .known_codes
            .iter()
            .find(|known| known.kind == CodeKind::Hms && known.attr == attr && known.code == code)
    }

    /// Names of the fact blocks that are not confirmed on a real printer.
    pub fn unverified(&self) -> Vec<String> {
        let inner = &self.inner;
        let family = &inner.family;
        let mut out = Vec::new();
        let mut note = |verified: bool, what: String| {
            if !verified {
                out.push(what);
            }
        };
        note(inner.identity.verified, "identity".to_owned());
        for nozzle in &inner.nozzles {
            note(nozzle.ids_verified, format!("nozzle {} ids", nozzle.role));
            note(
                nozzle.reach.verified,
                format!("nozzle {} reach", nozzle.role),
            );
            if let Some(travel) = &nozzle.travel {
                note(travel.verified, format!("nozzle {} travel", nozzle.role));
            }
            if let Some(speed) = &nozzle.max_speed {
                note(speed.verified, format!("nozzle {} max speed", nozzle.role));
            }
            if let Some(tray) = &nozzle.external_tray {
                note(
                    tray.verified,
                    format!("nozzle {} external spool holder", nozzle.role),
                );
            }
        }
        note(inner.limits.verified, "machine limits".to_owned());
        note(family.transport.verified, "transport".to_owned());
        note(family.storage.verified, "storage".to_owned());
        note(family.discovery.verified, "discovery".to_owned());
        note(family.project_file.verified, "project_file".to_owned());
        note(family.ams.verified, "ams".to_owned());
        note(family.speed_levels.verified, "speed levels".to_owned());
        note(inner.slice_file.verified, "slice file names".to_owned());
        for plate in &inner.plates {
            note(plate.verified, format!("build plate {}", plate.name));
        }
        for zone in &inner.keepouts {
            note(zone.verified, format!("keep-out {}", zone.id));
        }
        note(family.gcode.limits.verified, "gcode move limits".to_owned());
        note(family.gcode.verified, "gcode table".to_owned());
        note(family.gcode.sections.verified, "gcode sections".to_owned());
        note(
            family.gcode.unplaced.verified,
            "gcode unplaced moves".to_owned(),
        );
        for (code, row) in family.gcode.all_rows() {
            let form = match &row.letters {
                Some(letters) => letters.clone(),
                None => row.forms.join("|"),
            };
            note(row.verified, format!("gcode {code} [{form}]"));
        }
        for known in &family.known_codes {
            note(known.verified, format!("code {}", known.meaning));
        }
        for limits in &inner.filaments {
            note(limits.verified, format!("filament {}", limits.family));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn x2d() -> PrinterProfile {
        Profiles::builtin().unwrap().printer("bambu-x2d").unwrap()
    }

    #[test]
    fn the_x2d_nozzle_numbering_is_inverted_and_mapped_by_id() {
        let x2d = x2d();
        let main = x2d.nozzle_by_slicer_id(1).unwrap();
        assert_eq!((main.device_extruder, main.role.as_str()), (1, "main"));
        let auxiliary = x2d.nozzle_by_slicer_id(2).unwrap();
        assert_eq!(auxiliary.device_extruder, 0);
        assert_eq!(x2d.nozzle_by_device_id(0).unwrap().slicer_extruder, 2);
        assert_eq!(auxiliary.reach.x, [20.5, 256.0]);
        assert_eq!(main.reach.z, [0.0, 260.0]);
        assert!(x2d.nozzle_by_device_id(2).is_none());
        assert_eq!(x2d.family().id, "bambu-h2");
        assert_eq!(x2d.family().project_file.flag_type, FlagType::Boolean);
    }

    #[test]
    fn the_x2d_keeps_its_load_line_free_and_a_bad_box_is_refused() {
        let x2d = x2d();
        let zones = x2d.keepouts();
        assert_eq!(zones.len(), 1);
        let line = &zones[0];
        assert_eq!(line.id, "start_load_line");
        assert_eq!((line.x, line.y), ([55.0, 145.0], [0.0, 8.0]));
        assert!(!line.verified);
        assert!(
            x2d.unverified()
                .iter()
                .any(|name| name == "keep-out start_load_line")
        );
        let text = include_str!("../profiles/bambu.toml");
        for (good, bad) in [
            ("id = \"start_load_line\"", "id = \"Start Load Line\""),
            ("x = [55.0, 145.0]", "x = [145.0, 55.0]"),
            ("y = [0.0, 8.0]", "y = [0.0, 8000.0]"),
        ] {
            assert!(text.contains(good), "{good}");
            let broken = text.replacen(good, bad, 1);
            assert!(Profiles::from_toml(&broken).is_err(), "{bad}");
        }
    }

    #[test]
    fn every_fact_is_flagged_and_only_slice_facts_are_verified() {
        let x2d = x2d();
        let unverified = x2d.unverified();
        for expected in [
            "identity",
            "nozzle main ids",
            "nozzle main reach",
            "nozzle auxiliary reach",
            "machine limits",
            "transport",
            "project_file",
            "ams",
            "gcode T []",
            "filament PLA",
        ] {
            assert!(
                unverified.iter().any(|name| name == expected),
                "{expected} missing in {unverified:?}"
            );
        }
        // What the real slice showed is verified: the table, the markers, the travel box of
        // the main nozzle and the printer name.
        for observed in [
            "gcode table",
            "gcode sections",
            "gcode unplaced moves",
            "slice file names",
            "nozzle main travel",
            "gcode G1 [EFXYZ]",
            "gcode T [H]",
            "gcode M211 [XYZ]",
        ] {
            assert!(
                !unverified.iter().any(|name| name == observed),
                "{observed} is listed as unverified"
            );
        }
        // Nothing but slice facts is verified: no block outside the G-code table, the slice
        // file names, the build plate the slice was made for and the travel box says
        // `verified = true`.
        let text = include_str!("../profiles/bambu.toml");
        let table = text.find("[family.gcode]").unwrap();
        let after_table = text[table..].find("\n[[filament]]").unwrap() + table;
        let outside = format!("{}{}", &text[..table], &text[after_table..]);
        let verified_outside: Vec<&str> = outside
            .lines()
            .filter(|line| !line.trim_start().starts_with('#') && line.contains("verified = true"))
            .collect();
        assert_eq!(verified_outside.len(), 4, "{verified_outside:?}");
        assert!(verified_outside[0].starts_with("verified = true"));
        assert!(verified_outside[1].starts_with("verified = true"));
        assert!(verified_outside[2].starts_with("verified = true"));
        assert!(verified_outside[3].starts_with("travel = "));
        let plates = x2d.plates();
        assert_eq!(
            plates
                .iter()
                .filter(|plate| plate.verified)
                .map(|plate| plate.name.as_str())
                .collect::<Vec<_>>(),
            ["Cool Plate", "Textured PEI Plate"],
            "only the plates of the real slices are verified"
        );
    }

    #[test]
    fn the_gcode_table_names_where_and_how_each_command_was_seen() {
        let x2d = x2d();
        let gcode = &x2d.family().gcode;
        assert_eq!(gcode.all_rows().count(), gcode.row_count);
        // One command, two rows: the stock start form and the stock end form.
        let m211 = gcode.rows_of("M211");
        assert_eq!(m211.len(), 2);
        assert!(m211[0].applies_in(Section::Start) && !m211[0].applies_in(Section::Body));
        assert!(m211[0].accepts("XYZ") && !m211[0].accepts("S") && !m211[0].accepts(""));
        assert_eq!(m211[0].config, Some(ConfigKind::TravelLimits));
        assert_eq!(m211[0].max_count, Some(2));
        assert!(m211[1].applies_in(Section::End) && m211[1].accepts("Z"));
        // No row of a configuration command applies in the print itself, and each is counted.
        for (code, row) in gcode.all_rows() {
            if row.config.is_some() {
                assert!(!row.applies_in(Section::Body), "{code}");
                assert!(row.max_count.is_some(), "{code}");
            }
            // A row of unknown meaning says so.
            assert_eq!(
                row.basis == Basis::Unknown,
                row.what.starts_with("unknown"),
                "{code}"
            );
        }
        // Moves take any subset of their letters; nothing else does.
        let g1 = &gcode.rows_of("G1")[0];
        assert!(g1.accepts("") && g1.accepts("EX") && !g1.accepts("AX"));
        assert!(gcode.rows_of("M999").is_empty());
        assert_eq!(gcode.service_tools, [65279, 65535]);
        assert_eq!(gcode.deny["M502"].category, "eeprom");
    }

    #[test]
    fn ams_ids_and_nozzle_bits_come_from_the_data() {
        let profile = x2d();
        let ams = &profile.family().ams;
        assert!(ams.is_ht(128) && ams.is_ht(135));
        assert!(!ams.is_ht(136) && !ams.is_ht(3));
        assert_eq!(ams.nozzle_of_info(0x0000_0103), Some(1));
        assert_eq!(ams.nozzle_of_info(0x0000_0004), Some(0));
        assert_eq!(ams.nozzle_of_info(0x0000_0E03), None);
    }

    #[test]
    fn filament_families_match_by_name_or_alias_and_never_by_prefix() {
        let x2d = x2d();
        assert_eq!(x2d.filament_limits("PLA").unwrap().nozzle_max_c, 240.0);
        assert_eq!(x2d.filament_limits("pla basic").unwrap().family, "PLA");
        assert_eq!(x2d.filament_limits("PLA-CF").unwrap().family, "PLA-CF");
        assert_eq!(x2d.filament_limits("pla_cf").unwrap().family, "PLA-CF");
        assert_eq!(x2d.filament_limits("PETG HF").unwrap().family, "PETG");
        assert!(x2d.filament_limits("unobtainium").is_none());
        assert!(x2d.filament_limits("").is_none());
        // A name that only starts like a family is another material: PCTG prints far
        // cooler than PC, and "PLA, a 2 minute test" is not a material at all.
        assert!(x2d.filament_limits("PCTG").is_none());
        assert!(x2d.filament_limits("PLA, a 2 minute test").is_none());
        assert!(x2d.filament_limits("PLAX").is_none());
        let twice = include_str!("../profiles/bambu.toml").replacen(
            "aliases = [\"PLA Basic\", \"PLA Matte\"]",
            "aliases = [\"PLA Basic\", \"PETG\"]",
            1,
        );
        assert!(Profiles::from_toml(&twice).is_err());
    }

    #[test]
    fn archive_entry_names_and_plates_come_from_the_data() {
        let x2d = x2d();
        let files = x2d.slice_file();
        assert!(files.allows_entry("Metadata/plate_1.gcode", 1));
        assert!(files.allows_entry("3D/Objects/object_12.model", 1));
        assert!(files.allows_entry("Metadata/plate_no_light_3.png", 3));
        for name in [
            "Metadata/plate_2.gcode",
            "metadata/plate_1.gcode",
            "Metadata/PLATE_1.GCODE",
            "Metadata/plate_1.gcode/",
            "Metadata/plate_1.gcode ",
            "3D/Objects/object_01.model",
            "3D/Objects/object_.model",
            "Metadata/notes.txt",
        ] {
            assert!(!files.allows_entry(name, 1), "{name}");
        }
        assert_eq!(files.required_for(2)[0], "Metadata/plate_2.gcode");
        assert!(files.knows_nozzle(0.4) && !files.knows_nozzle(0.8));
        let cool = x2d.plate("Cool Plate").unwrap();
        assert_eq!(
            (cool.json_name.as_deref(), cool.bed_max_c),
            (Some("cool_plate"), 45.0)
        );
        assert_eq!(cool.z_trim_mm, Some(0.022));
        let textured = x2d.plate("Textured PEI Plate").unwrap();
        assert_eq!(
            (textured.json_name.as_deref(), textured.z_trim_mm),
            (Some("textured_plate"), Some(0.002))
        );
        assert!(x2d.plate("cool plate").is_none());
        assert_eq!(x2d.plate_by_id("cool"), Some(cool));
        assert_eq!(
            x2d.plate_by_id("textured-pei")
                .map(|plate| plate.name.as_str()),
            Some("Textured PEI Plate")
        );
        assert!(x2d.plate_by_id("Cool Plate").is_none());
        let limits = &x2d.family().gcode.limits;
        assert_eq!((limits.min_z_mm, limits.max_feed_mm_min), (0.2, 60_000.0));
    }

    #[test]
    fn the_file_is_parsed_strictly() {
        let text = include_str!("../profiles/bambu.toml");
        let unknown_key = text.replacen("schema = 5", "schema = 5\nsurprise = true", 1);
        assert!(matches!(
            Profiles::from_toml(&unknown_key),
            Err(ProfileError::Parse(_))
        ));
        let wrong_schema = text.replacen("schema = 5", "schema = 1", 1);
        assert!(matches!(
            Profiles::from_toml(&wrong_schema),
            Err(ProfileError::Invalid(_))
        ));
        let bad_reach = text.replacen("x = [0.0, 256.0]", "x = [300.0, 256.0]", 1);
        assert!(Profiles::from_toml(&bad_reach).is_err());
        assert!(Profiles::builtin().unwrap().printer("prusa-xl").is_err());
    }

    /// The table cannot be loosened by accident: each of these edits is refused when the
    /// file is loaded.
    #[test]
    fn a_loose_gcode_row_is_refused() {
        let text = include_str!("../profiles/bambu.toml");
        let edit = |from: &str, to: &str| {
            assert!(text.contains(from), "the profile no longer has `{from}`");
            Profiles::from_toml(&text.replacen(from, to, 1))
        };
        let refused = |from: &str, to: &str, part: &str| match edit(from, to) {
            Err(ProfileError::Invalid(message)) => {
                assert!(message.contains(part), "{message}");
            }
            other => panic!("`{to}` was not refused: {:?}", other.map(|_| ())),
        };
        // A parameter without a limit.
        refused(
            "args = { X = [0, 0], Y = [0, 0], Z = [0, 0] }",
            "args = { X = [0, 0], Y = [0, 0] }",
            "has no limit",
        );
        // A limit for a letter no form has.
        refused(
            "args = { H = [-1, -1] }",
            "args = { H = [-1, -1], Q = [0, 1] }",
            "which no form has",
        );
        // Two rows for the same command, section and form.
        refused(
            "codes = [\"G0\"]\nclass = \"move\"",
            "codes = [\"G0\", \"G1\"]\nclass = \"move\"",
            "two rows",
        );
        // A row of unknown meaning that is taken to leave the nozzle where it is.
        refused(
            "basis = \"unknown\"\nmoves = true\nverified = true",
            "basis = \"unknown\"\nverified = true",
            "unknown meaning",
        );
        // A configuration command that is not counted.
        refused(
            "config = \"stored_settings\"\nmax_count = 1",
            "config = \"stored_settings\"",
            "max_count",
        );
        // `G` as a parameter letter, which would let a second command hide in a line.
        refused("letters = \"XYZF\"", "letters = \"GXYZF\"", "G is never");
        // Free letters on a command that is not a move.
        refused(
            "forms = [\"S\", \"SA\", \"ST\"]",
            "letters = \"SAT\"",
            "every other class takes `forms`",
        );
        // An unusable limit, a section marker that is not a word, and a command name that
        // is not one.
        refused("F = { up_to = 1200 }", "F = { up_to = -1 }", "unusable");
        refused(
            "open = \"EXECUTABLE_BLOCK_START\"",
            "open = \"executable block\"",
            "section markers",
        );
        refused(
            "codes = [\"M625\"]",
            "codes = [\"M625 S1\"]",
            "command name",
        );
        // An unknown key in a row is a parse error, not something to ignore.
        assert!(matches!(
            edit("codes = [\"M625\"]", "codes = [\"M625\"]\nallow_any = true"),
            Err(ProfileError::Parse(_))
        ));
    }

    /// Moves from a position only the printer knows are the ones nothing else can check.
    /// The table cannot be written so that one of them closes the gap to the bed.
    #[test]
    fn unplaced_moves_cannot_be_allowed_towards_the_bed() {
        let text = include_str!("../profiles/bambu.toml");
        let edit = |from: &str, to: &str| {
            assert!(text.contains(from), "the profile no longer has `{from}`");
            Profiles::from_toml(&text.replacen(from, to, 1))
        };
        let refused = |from: &str, to: &str, part: &str| match edit(from, to) {
            Err(ProfileError::Invalid(message)) => {
                assert!(message.contains("unplaced moves"), "{message}");
                assert!(message.contains(part), "{message}");
            }
            other => panic!("`{to}` was not refused: {:?}", other.map(|_| ())),
        };
        let rules = Profiles::builtin()
            .unwrap()
            .printer("bambu-x2d")
            .unwrap()
            .family()
            .gcode
            .unplaced
            .clone();
        // What the real slice showed: five moves, Y 16 mm and Z 22 + 12 + 5 + 6 mm.
        assert_eq!(rules.sections, [Section::Start]);
        assert_eq!(rules.plain, [[0.0, 0.0], [-16.0, 0.0], [0.0, 6.0]]);
        assert_eq!(rules.guarded, [[0.0, 0.0], [0.0, 0.0], [-12.0, 22.0]]);
        assert_eq!(rules.net, [[0.0, 0.0], [-16.0, 0.0], [0.0, 22.0]]);
        assert_eq!(rules.total_mm, [0.0, 16.0, 45.0]);
        assert_eq!(rules.max_count, 5);

        // A plain move that lowers Z, and a sum that may end below where it began.
        refused(
            "plain = { x = [0.0, 0.0], y = [-16.0, 0.0], z = [0.0, 6.0] }",
            "plain = { x = [0.0, 0.0], y = [-16.0, 0.0], z = [-6.0, 6.0] }",
            "lower Z",
        );
        refused(
            "net = { x = [0.0, 0.0], y = [-16.0, 0.0], z = [0.0, 22.0] }",
            "net = { x = [0.0, 0.0], y = [-16.0, 0.0], z = [-12.0, 22.0] }",
            "lower Z",
        );
        // One move that goes further than the moves may add up to: a guarded lowering
        // longer than any lift, a longer plain move.
        refused(
            "guarded = { x = [0.0, 0.0], y = [0.0, 0.0], z = [-12.0, 22.0] }",
            "guarded = { x = [0.0, 0.0], y = [0.0, 0.0], z = [-30.0, 22.0] }",
            "further than",
        );
        refused(
            "plain = { x = [0.0, 0.0], y = [-16.0, 0.0], z = [0.0, 6.0] }",
            "plain = { x = [0.0, 0.0], y = [-20.0, 0.0], z = [0.0, 6.0] }",
            "further than",
        );
        // Ranges that are not ranges around 0, and a total that is not a distance.
        refused(
            "guarded = { x = [0.0, 0.0], y = [0.0, 0.0], z = [-12.0, 22.0] }",
            "guarded = { x = [0.0, 0.0], y = [0.0, 0.0], z = [22.0, -12.0] }",
            "every range",
        );
        refused(
            "total_mm = { x = 0.0, y = 16.0, z = 45.0 }",
            "total_mm = { x = 0.0, y = 16.0, z = -45.0 }",
            "total distance",
        );
        // The earlier layout, one symmetric length per axis, is not read any more.
        assert!(matches!(
            edit(
                "total_mm = { x = 0.0, y = 16.0, z = 45.0 }",
                "total_mm = { x = 0.0, y = 16.0, z = 45.0 }\nmax_mm = { x = 0.0, y = 16.0, z = 22.0 }"
            ),
            Err(ProfileError::Parse(_))
        ));
    }

    #[test]
    fn known_codes_and_model_codes_are_found() {
        let profiles = Profiles::builtin().unwrap();
        let x2d = profiles.printer_for_model_code("n6").unwrap();
        assert_eq!(x2d.id(), "bambu-x2d");
        assert!(profiles.printer_for_model_code("C11").is_none());
        assert_eq!(
            x2d.known_print_error(50_348_044).unwrap().meaning,
            "cancelled"
        );
        assert_eq!(
            x2d.known_hms(0x0300_0100, 0x0001_0007).unwrap().meaning,
            "heatbed_sensor"
        );
        assert!(x2d.known_print_error(1).is_none());
    }

    /// The slicer crate keeps its own copy of the nozzle ids and reach; the two files must
    /// say the same.
    #[test]
    fn nozzle_facts_agree_with_the_slicer_crate() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../hotend-slicer/profiles/printers.toml");
        let text = std::fs::read_to_string(path).unwrap();
        let slicer: toml::Value = toml::from_str(&text).unwrap();
        let printers = slicer["printer"].as_array().unwrap();
        let theirs = printers
            .iter()
            .find(|printer| printer["id"].as_str() == Some("bambu-x2d"))
            .unwrap();
        let x2d = x2d();
        let nozzles = theirs["nozzle"].as_array().unwrap();
        assert_eq!(nozzles.len(), x2d.nozzles().len());
        for nozzle in nozzles {
            let device = nozzle["ids"]["device_extruder"].as_integer().unwrap() as u8;
            let ours = x2d.nozzle_by_device_id(device).unwrap();
            assert_eq!(
                i64::from(ours.slicer_extruder),
                nozzle["ids"]["slicer_extruder"].as_integer().unwrap()
            );
            for (axis, ours_axis) in [
                ("x", ours.reach.x),
                ("y", ours.reach.y),
                ("z", ours.reach.z),
            ] {
                let pair = nozzle["reach"][axis].as_array().unwrap();
                assert_eq!(pair[0].as_float().unwrap(), ours_axis[0], "{axis}");
                assert_eq!(pair[1].as_float().unwrap(), ours_axis[1], "{axis}");
            }
        }
    }
}
