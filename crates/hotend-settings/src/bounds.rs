//! The hard range of every field, as constants.
//!
//! Guarantees:
//! - every range is written here, in code. None is read from a profile on disk, from a data
//!   file or from an input, so nothing outside this file can widen one;
//! - [`check`] is the single test "this value may exist for this field". The settings are
//!   built through it, the proposal schema is generated from the same table, and the slicer
//!   side judges every echoed value against it, written by Hotend or not.
//!
//! A plan narrows a range further (the material card's bed window, the envelope of a field);
//! it never widens one.

use crate::model::{
    BrimKind, Family, Field, InfillPattern, Intent, PlateType, ProcessPreset, Seam, SupportKind,
    SupportWhere, Unit, Value,
};

/// Bed temperature, in C. The lowest and the highest value any material card may hold; the
/// top is the printer's own limit (`bed_max_c` of `bambu.toml`), and a test holds every card
/// window inside.
pub const BED_C: (i64, i64) = (30, 120);
/// Nozzle temperature, in C. Never written in version 1; an echoed value is judged against
/// the family's range, which a test holds inside this one.
pub const NOZZLE_C: (i64, i64) = (170, 300);
/// The bed temperatures a material card of this family may hold, on any plate type: the
/// constant frame around the card's windows. A card whose window leaves it fails to load, so
/// an edited data file cannot cool the bed below what the family needs to stick, or heat it
/// past what softens it. The numbers are the outermost values of the vendor's material table
/// for the plates Hotend knows.
pub const fn bed_range_c(family: Family) -> (i64, i64) {
    match family {
        Family::Pla => (35, 65),
        Family::Petg => (60, 80),
        Family::Tpu => (30, 45),
        Family::Abs | Family::Asa => (90, 100),
    }
}

/// Constant limits of the rule data and the material cards.
///
/// The thresholds and widths of the rules are data, so that a measurement can replace a
/// judgment without a release. What guards adhesion and the fit must not rest on a data file
/// alone: a file that asks for less than these constants fails to load.
pub mod floors {
    /// FIT-2: the gap of a brim that a rule requires. It touches the part; no other value
    /// loads.
    pub const REQUIRED_BRIM_GAP_MM: f64 = 0.0;
    /// FIT-2: the least elephant foot compensation a touching brim needs to snap off clean.
    pub const TOUCHING_BRIM_ELEPHANT_FOOT_MIN_MM: f64 = 0.1;
    /// The narrowest brim a brim rule for a rigid material may draw, and the narrowest
    /// brim level 2 may ask for.
    pub const RULE_BRIM_MIN_MM: i64 = 3;
    /// The narrowest brim AD-6 may draw under a thin or tall flexible piece.
    pub const FLEXIBLE_BRIM_MIN_MM: i64 = 1;
    /// What level 2 adds to an existing brim, at least.
    pub const LEVEL_BRIM_EXTRA_MIN_MM: i64 = 1;
    /// AD-1 fires at least for strips narrower than this.
    pub const THIN_STRIP_BELOW_MIN_MM: f64 = 2.0;
    /// AD-2 fires at least for feet smaller than this: the pieces that came off the plate
    /// had 167 and 196 mm2.
    pub const SMALL_FOOT_BELOW_MIN_MM2: f64 = 200.0;
    /// AD-3 fires at the latest from this slenderness.
    pub const SLENDER_ABOVE_MAX: f64 = 3.0;
    /// AD-4 fires at the latest from this length, warp class low.
    pub const LONG_PART_LOW_ABOVE_MAX_MM: f64 = 250.0;
    /// AD-4 fires at the latest from this length, warp class medium.
    pub const LONG_PART_MEDIUM_ABOVE_MAX_MM: f64 = 180.0;
    /// The layers the part fan stays off for, on any card and at level 2.
    pub const FAN_OFF_LAYERS_MIN: i64 = 3;
    /// The first layer height level 2 may ask for, at least: the standard preset's.
    pub const LEVEL_FIRST_LAYER_HEIGHT_MIN_MM: f64 = 0.20;
    /// MAT-1: the wall loops a thin wall gets, at least.
    pub const THIN_WALL_LOOPS_MIN: i64 = 3;
}

/// Device extruder ids of a two-nozzle printer.
pub const NOZZLE_ID: (i64, i64) = (0, 1);
/// Adhesion level.
pub const LEVEL: (i64, i64) = (0, 3);
/// Brim width, whole millimetres.
pub const BRIM_WIDTH_MM: (i64, i64) = (0, 15);
/// Gap between brim and part, in mm.
pub const BRIM_GAP_MM: (f64, f64) = (0.0, 0.3);
/// Skirt loops: none, always.
pub const SKIRT_LOOPS: (i64, i64) = (0, 0);
/// Elephant foot compensation, in mm.
pub const ELEPHANT_FOOT_MM: (f64, f64) = (0.0, 0.3);
/// First layer height, in mm. The top is the 0.4 mm nozzle's maximum layer height.
pub const FIRST_LAYER_HEIGHT_MM: (f64, f64) = (0.16, 0.28);
/// First layer line width, in mm.
pub const FIRST_LAYER_LINE_WIDTH_MM: (f64, f64) = (0.42, 0.60);
/// First layer wall speed, in mm/s.
pub const FIRST_LAYER_WALL_SPEED: (i64, i64) = (10, 50);
/// First layer infill speed, in mm/s.
pub const FIRST_LAYER_INFILL_SPEED: (i64, i64) = (15, 105);
/// First layer acceleration cap, in mm/s2.
pub const FIRST_LAYER_ACCEL: (i64, i64) = (250, 500);
/// Layers the part fan stays off for.
pub const PART_FAN_OFF_LAYERS: (i64, i64) = (1, 10);
/// Auxiliary fan on the first layers, percent.
pub const AUX_FAN_FIRST_LAYERS_PCT: (i64, i64) = (0, 40);
/// Auxiliary fan maximum, percent.
pub const AUX_FAN_MAX_PCT: (i64, i64) = (0, 100);
/// Wall loops.
pub const WALL_LOOPS: (i64, i64) = (2, 6);
/// Sparse infill density, percent.
pub const INFILL_PCT: (i64, i64) = (5, 50);
/// Support threshold, degrees from horizontal.
pub const SUPPORT_THRESHOLD_DEG: (i64, i64) = (20, 45);
/// Outer wall speed cap, in mm/s.
pub const OUTER_WALL_SPEED_CAP: (i64, i64) = (20, 200);
/// General speed cap, in mm/s.
pub const SPEED_CAP: (i64, i64) = (20, 300);
/// Acceleration cap, in mm/s2.
pub const ACCEL_CAP: (i64, i64) = (500, 10_000);
/// Outer wall acceleration cap, in mm/s2.
pub const OUTER_WALL_ACCEL_CAP: (i64, i64) = (500, 5000);
/// Volumetric flow cap, in mm3/s.
pub const MAX_FLOW_CAP: (f64, f64) = (1.0, 25.0);
/// Jerk cap, in mm/s. Reserved.
pub const JERK_CAP: (i64, i64) = (1, 9);

/// The type and hard range of a field's value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    /// A whole number from `min` to `max`.
    Whole {
        /// The lowest value.
        min: i64,
        /// The highest value.
        max: i64,
    },
    /// A decimal with two places from `min` to `max`.
    Decimal {
        /// The lowest value.
        min: f64,
        /// The highest value.
        max: f64,
    },
    /// One of these tokens.
    Tokens(&'static [&'static str]),
    /// On or off.
    Flag,
    /// `#RRGGBB`.
    Colour,
    /// A spool slot.
    Slot,
}

/// The table a field belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// Setup and plate.
    Setup,
    /// Adhesion, first layer and early cooling. No choice may weaken a field of this group.
    Sticking,
    /// Shell.
    Shell,
    /// Supports.
    Supports,
    /// Motion caps.
    Motion,
    /// The prime tower.
    PrimeTower,
}

/// Who may decide a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Who {
    /// The person is asked.
    pub person: bool,
    /// A rule decides.
    pub rule: bool,
    /// The agent or the person may move the value inside its envelope.
    pub adjustable: bool,
    /// Print history moves the rule's result.
    pub history: bool,
    /// Locked to the values of the fit calibration, when one exists.
    pub calibration: bool,
    /// Fixed in version 1.
    pub fixed: bool,
}

const fn who(letters: &str) -> Who {
    let bytes = letters.as_bytes();
    let mut who = Who {
        person: false,
        rule: false,
        adjustable: false,
        history: false,
        calibration: false,
        fixed: false,
    };
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'P' => who.person = true,
            b'R' => who.rule = true,
            b'A' => who.adjustable = true,
            b'H' => who.history = true,
            b'C' => who.calibration = true,
            b'F' => who.fixed = true,
            _ => {}
        }
        at += 1;
    }
    who
}

/// Everything constant about a field.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FieldSpec {
    /// The field.
    pub field: Field,
    /// The unit of its value.
    pub unit: Unit,
    /// The type and hard range of its value.
    pub kind: Kind,
    /// Whether the value may be `Unset`: "as the installed profile has it".
    pub optional: bool,
    /// The table it belongs to.
    pub group: Group,
    /// Who may decide it.
    pub who: Who,
}

const fn whole(range: (i64, i64)) -> Kind {
    Kind::Whole {
        min: range.0,
        max: range.1,
    }
}

const fn decimal(range: (f64, f64)) -> Kind {
    Kind::Decimal {
        min: range.0,
        max: range.1,
    }
}

/// The constants of a field.
pub const fn spec(field: Field) -> FieldSpec {
    use Group::{Motion, PrimeTower, Setup, Shell, Sticking, Supports};
    let (unit, kind, optional, group, letters) = match field {
        Field::PlateType => (
            Unit::Enum,
            Kind::Tokens(PlateType::TOKENS),
            false,
            Setup,
            "P",
        ),
        Field::Spool => (Unit::None, Kind::Slot, false, Setup, "RP"),
        Field::NozzleId => (Unit::None, whole(NOZZLE_ID), false, Setup, "R"),
        Field::Colour => (Unit::None, Kind::Colour, false, Setup, "P"),
        Field::Intent => (Unit::Enum, Kind::Tokens(Intent::TOKENS), false, Setup, "A"),
        Field::Level => (Unit::Step, whole(LEVEL), false, Sticking, "RHA"),
        Field::BrimKind => (
            Unit::Enum,
            Kind::Tokens(BrimKind::TOKENS),
            false,
            Sticking,
            "RA",
        ),
        Field::BrimWidth => (
            Unit::Millimetre,
            whole(BRIM_WIDTH_MM),
            false,
            Sticking,
            "RHA",
        ),
        Field::BrimGap => (Unit::Millimetre, decimal(BRIM_GAP_MM), false, Sticking, "R"),
        Field::SkirtLoops => (Unit::Count, whole(SKIRT_LOOPS), false, Sticking, "F"),
        Field::ElephantFoot => (
            Unit::Millimetre,
            decimal(ELEPHANT_FOOT_MM),
            true,
            Sticking,
            "C",
        ),
        Field::FirstLayerHeight => (
            Unit::Millimetre,
            decimal(FIRST_LAYER_HEIGHT_MM),
            true,
            Sticking,
            "CR",
        ),
        Field::FirstLayerLineWidth => (
            Unit::Millimetre,
            decimal(FIRST_LAYER_LINE_WIDTH_MM),
            true,
            Sticking,
            "C",
        ),
        Field::FirstLayerWallSpeed => (
            Unit::MillimetrePerSecond,
            whole(FIRST_LAYER_WALL_SPEED),
            false,
            Sticking,
            "RA",
        ),
        Field::FirstLayerInfillSpeed => (
            Unit::MillimetrePerSecond,
            whole(FIRST_LAYER_INFILL_SPEED),
            false,
            Sticking,
            "RA",
        ),
        Field::FirstLayerAccel => (
            Unit::MillimetrePerSecondSquared,
            whole(FIRST_LAYER_ACCEL),
            true,
            Sticking,
            "A",
        ),
        Field::FirstLayerBed => (Unit::Celsius, whole(BED_C), false, Sticking, "RHA"),
        Field::LayersBed => (Unit::Celsius, whole(BED_C), false, Sticking, "R"),
        Field::FirstLayerNozzle => (Unit::Celsius, whole(NOZZLE_C), true, Sticking, "F"),
        Field::LayersNozzle => (Unit::Celsius, whole(NOZZLE_C), true, Sticking, "F"),
        Field::PartFanOffLayers => (
            Unit::Layers,
            whole(PART_FAN_OFF_LAYERS),
            false,
            Sticking,
            "RA",
        ),
        Field::AuxFanFirstLayers => (
            Unit::Percent,
            whole(AUX_FAN_FIRST_LAYERS_PCT),
            false,
            Sticking,
            "R",
        ),
        Field::AuxFanMax => (Unit::Percent, whole(AUX_FAN_MAX_PCT), true, Sticking, "RA"),
        // Set by the intent only: neither a rule nor a change names a preset.
        Field::Process => (
            Unit::Enum,
            Kind::Tokens(ProcessPreset::TOKENS),
            false,
            Shell,
            "",
        ),
        Field::WallLoops => (Unit::Count, whole(WALL_LOOPS), false, Shell, "A"),
        Field::InfillPct => (Unit::Percent, whole(INFILL_PCT), false, Shell, "A"),
        Field::InfillPattern => (
            Unit::Enum,
            Kind::Tokens(InfillPattern::TOKENS),
            false,
            Shell,
            "A",
        ),
        Field::Seam => (Unit::Enum, Kind::Tokens(Seam::TOKENS), false, Shell, "A"),
        Field::SupportsEnabled => (Unit::Flag, Kind::Flag, false, Supports, "RA"),
        Field::SupportsKind => (
            Unit::Enum,
            Kind::Tokens(SupportKind::TOKENS),
            false,
            Supports,
            "RA",
        ),
        Field::SupportsThreshold => (
            Unit::Degrees,
            whole(SUPPORT_THRESHOLD_DEG),
            false,
            Supports,
            "A",
        ),
        Field::SupportsWhere => (
            Unit::Enum,
            Kind::Tokens(SupportWhere::TOKENS),
            false,
            Supports,
            "RA",
        ),
        Field::OuterWallSpeedCap => (
            Unit::MillimetrePerSecond,
            whole(OUTER_WALL_SPEED_CAP),
            true,
            Motion,
            "A",
        ),
        Field::SpeedCap => (
            Unit::MillimetrePerSecond,
            whole(SPEED_CAP),
            true,
            Motion,
            "A",
        ),
        Field::AccelCap => (
            Unit::MillimetrePerSecondSquared,
            whole(ACCEL_CAP),
            true,
            Motion,
            "A",
        ),
        Field::OuterWallAccelCap => (
            Unit::MillimetrePerSecondSquared,
            whole(OUTER_WALL_ACCEL_CAP),
            true,
            Motion,
            "A",
        ),
        Field::MaxFlowCap => (
            Unit::CubicMillimetrePerSecond,
            decimal(MAX_FLOW_CAP),
            true,
            Motion,
            "A",
        ),
        Field::JerkCap => (
            Unit::MillimetrePerSecond,
            whole(JERK_CAP),
            true,
            Motion,
            "F",
        ),
        Field::PrimeTower => (Unit::Flag, Kind::Flag, false, PrimeTower, "R"),
    };
    FieldSpec {
        field,
        unit,
        kind,
        optional,
        group,
        who: who(letters),
    }
}

/// Why a value may not exist for a field.
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
pub enum RangeError {
    /// The value has another type than the field.
    #[error("`{0}` takes another kind of value; see the field table in docs/PRINT_SETTINGS.md")]
    WrongKind(Field),
    /// The number is not finite.
    #[error("`{0}` must be a finite number")]
    NotFinite(Field),
    /// The number lies outside the hard range.
    #[error("`{field}` must lie between {min} and {max}")]
    OutOfRange {
        /// The field.
        field: Field,
        /// The lowest allowed value.
        min: f64,
        /// The highest allowed value.
        max: f64,
    },
    /// The token is not in the field's list.
    #[error("`{0}` does not have this value; see the field table in docs/PRINT_SETTINGS.md")]
    UnknownToken(Field),
    /// The field must have a value.
    #[error("`{0}` cannot be left to the installed profile; give it a value")]
    Required(Field),
    /// Not every field has a value.
    #[error("the settings must hold one value per field")]
    Incomplete,
}

/// Rounding slack for decimals with two places.
const EPS: f64 = 1e-9;

/// Whether `value` may exist for `field`: the right type, finite, and inside the hard range.
pub fn check(field: Field, value: &Value) -> Result<(), RangeError> {
    let spec = spec(field);
    match (spec.kind, value) {
        (_, Value::Unset) if spec.optional => Ok(()),
        (_, Value::Unset) => Err(RangeError::Required(field)),
        (Kind::Whole { min, max }, Value::Whole(whole)) => {
            if (min..=max).contains(whole) {
                Ok(())
            } else {
                Err(RangeError::OutOfRange {
                    field,
                    min: min as f64,
                    max: max as f64,
                })
            }
        }
        (Kind::Decimal { min, max }, Value::Decimal(decimal)) => {
            if !decimal.is_finite() {
                Err(RangeError::NotFinite(field))
            } else if *decimal < min - EPS || *decimal > max + EPS {
                Err(RangeError::OutOfRange { field, min, max })
            } else {
                Ok(())
            }
        }
        (Kind::Tokens(tokens), Value::Token(token)) => {
            if tokens.contains(token) {
                Ok(())
            } else {
                Err(RangeError::UnknownToken(field))
            }
        }
        (Kind::Flag, Value::Flag(_))
        | (Kind::Colour, Value::Colour(_))
        | (Kind::Slot, Value::Slot(_)) => Ok(()),
        _ => Err(RangeError::WrongKind(field)),
    }
}

/// The hard range of a numeric field as `(min, max)`; `None` for the other kinds.
pub fn numeric_range(field: Field) -> Option<(f64, f64)> {
    match spec(field).kind {
        Kind::Whole { min, max } => Some((min as f64, max as f64)),
        Kind::Decimal { min, max } => Some((min, max)),
        _ => None,
    }
}

/// The fields the agent or the person may propose a change to, in the order of the tables.
pub fn adjustable_fields() -> impl Iterator<Item = Field> {
    Field::ALL
        .iter()
        .copied()
        .filter(|field| spec(*field).who.adjustable)
}

/// The fields of the sticking group, in the order of the tables.
pub fn sticking_fields() -> impl Iterator<Item = Field> {
    Field::ALL
        .iter()
        .copied()
        .filter(|field| spec(*field).group == Group::Sticking)
}
