//! The model: the closed list of print settings, their values, and the plan that records how
//! each value was decided.
//!
//! Guarantees:
//! - the list of fields is closed ([`Field`]); there is no field with a free name;
//! - a value is a number, a flag, a colour, a spool slot or a token of a closed list written
//!   in this crate ([`Value`]). No type here can hold text from a person, a model or a file;
//! - a [`PrintSettings`] can only be built inside this crate, and only from values that pass
//!   the constant hard range of their field ([`crate::bounds`]). It is `Serialize` only: the
//!   stored plan is a record and is never read back;
//! - a [`PrintPlan`] records, for every field, the value, the rule that set it, the value the
//!   rules alone gave ([`Decision::rule_value`]) and the envelope a later change must stay in.

use std::fmt;

use serde::ser::{SerializeMap, SerializeStruct};
use serde::{Deserialize, Serialize, Serializer};

use crate::bounds::{self, RangeError};
use crate::facts::PlateFacts;
use crate::inputs::Asker;

/// The layout version of a serialized [`PrintSettings`].
pub const SCHEMA: u32 = 1;

/// Declares an enum whose values are fixed tokens. The tokens are the only strings a value
/// of the model can carry, so every one of them is written here, in code.
macro_rules! token_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident => $token:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $name {
            $($(#[$vmeta])* $variant),+
        }

        impl $name {
            /// Every value, in the fixed order.
            pub const ALL: &'static [$name] = &[$($name::$variant),+];
            /// The tokens of [`Self::ALL`], in the same order.
            pub const TOKENS: &'static [&'static str] = &[$($token),+];

            /// The token of this value.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $token),+
                }
            }

            /// The value with this exact token.
            pub fn from_token(token: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|value| value.as_str() == token)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let token = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
                // The error names the type only: the unknown text is not repeated.
                Self::from_token(&token)
                    .ok_or_else(|| serde::de::Error::custom(concat!("not a value of ", stringify!($name))))
            }
        }
    };
}

token_enum! {
    /// A print setting of Hotend, slicer-neutral. The order is the one of the table in
    /// docs/PRINT_SETTINGS.md, in which a plan is computed and written.
    Field {
        PlateType => "setup.plate_type",
        Spool => "filament.spool",
        NozzleId => "filament.nozzle_id",
        Colour => "filament.colour",
        Intent => "intent",
        Level => "adhesion.level",
        BrimKind => "adhesion.brim_kind",
        BrimWidth => "adhesion.brim_width_mm",
        BrimGap => "adhesion.brim_gap_mm",
        SkirtLoops => "adhesion.skirt_loops",
        ElephantFoot => "adhesion.elephant_foot_mm",
        FirstLayerHeight => "first_layer.height_mm",
        FirstLayerLineWidth => "first_layer.line_width_mm",
        FirstLayerWallSpeed => "first_layer.wall_speed_mm_s",
        FirstLayerInfillSpeed => "first_layer.infill_speed_mm_s",
        FirstLayerAccel => "first_layer.accel_mm_s2",
        FirstLayerBed => "first_layer.bed_c",
        LayersBed => "layers.bed_c",
        FirstLayerNozzle => "first_layer.nozzle_c",
        LayersNozzle => "layers.nozzle_c",
        PartFanOffLayers => "cooling.part_fan_off_layers",
        AuxFanFirstLayers => "cooling.aux_fan_first_layers_pct",
        AuxFanMax => "cooling.aux_fan_max_pct",
        Process => "shell.process",
        WallLoops => "shell.wall_loops",
        InfillPct => "shell.infill_pct",
        InfillPattern => "shell.infill_pattern",
        Seam => "shell.seam",
        SupportsEnabled => "supports.enabled",
        SupportsKind => "supports.kind",
        SupportsThreshold => "supports.threshold_deg",
        SupportsWhere => "supports.where",
        OuterWallSpeedCap => "motion.outer_wall_speed_cap_mm_s",
        SpeedCap => "motion.speed_cap_mm_s",
        AccelCap => "motion.accel_cap_mm_s2",
        OuterWallAccelCap => "motion.outer_wall_accel_cap_mm_s2",
        MaxFlowCap => "motion.max_flow_cap_mm3_s",
        JerkCap => "motion.jerk_cap_mm_s",
        PrimeTower => "prime_tower.enabled",
    }
}

impl Field {
    /// The position of the field in [`Field::ALL`].
    pub fn index(self) -> usize {
        self as usize
    }

    /// The field's name for people, in plain words, for messages that name a setting.
    pub const fn plain_name(self) -> &'static str {
        match self {
            Field::PlateType => "plate type",
            Field::Spool => "spool",
            Field::NozzleId => "nozzle",
            Field::Colour => "filament colour",
            Field::Intent => "purpose of the print",
            Field::Level => "adhesion level",
            Field::BrimKind => "brim kind",
            Field::BrimWidth => "brim width",
            Field::BrimGap => "gap between brim and part",
            Field::SkirtLoops => "skirt loops",
            Field::ElephantFoot => "elephant foot compensation",
            Field::FirstLayerHeight => "first layer height",
            Field::FirstLayerLineWidth => "first layer line width",
            Field::FirstLayerWallSpeed => "first layer speed",
            Field::FirstLayerInfillSpeed => "first layer infill speed",
            Field::FirstLayerAccel => "first layer acceleration",
            Field::FirstLayerBed => "bed temperature of the first layer",
            Field::LayersBed => "bed temperature of the other layers",
            Field::FirstLayerNozzle => "nozzle temperature of the first layer",
            Field::LayersNozzle => "nozzle temperature of the other layers",
            Field::PartFanOffLayers => "layers without the part cooling fan",
            Field::AuxFanFirstLayers => "auxiliary fan on the first layers",
            Field::AuxFanMax => "auxiliary fan limit",
            Field::Process => "process preset",
            Field::WallLoops => "wall loops",
            Field::InfillPct => "infill density",
            Field::InfillPattern => "infill pattern",
            Field::Seam => "seam position",
            Field::SupportsEnabled => "supports",
            Field::SupportsKind => "support kind",
            Field::SupportsThreshold => "support angle",
            Field::SupportsWhere => "where supports may stand",
            Field::OuterWallSpeedCap => "outer wall speed limit",
            Field::SpeedCap => "speed limit",
            Field::AccelCap => "acceleration limit",
            Field::OuterWallAccelCap => "outer wall acceleration limit",
            Field::MaxFlowCap => "flow limit",
            Field::JerkCap => "jerk limit",
            Field::PrimeTower => "prime tower",
        }
    }
}

token_enum! {
    /// The build plate on the bed. `CoolSupertack` can be answered and always blocks the plan.
    PlateType {
        Cool => "cool",
        TexturedPei => "textured-pei",
        HighTemp => "high-temp",
        Engineering => "engineering",
        CoolSupertack => "cool-supertack",
    }
}

token_enum! {
    /// A material family, the same list as `python/hotend_fit/materials.py`.
    Family {
        Pla => "PLA",
        Petg => "PETG",
        Tpu => "TPU",
        Abs => "ABS",
        Asa => "ASA",
    }
}

token_enum! {
    /// What the print is for. It sets shell and motion values and never a sticking value.
    Intent {
        Balanced => "balanced",
        Strong => "strong",
        BestSurface => "best_surface",
        Fast => "fast",
        FitCritical => "fit_critical",
    }
}

token_enum! {
    /// Where the brim is drawn. There is no automatic kind, on purpose.
    BrimKind {
        None => "none",
        Outer => "outer",
        Inner => "inner",
        OuterAndInner => "outer_and_inner",
    }
}

token_enum! {
    /// A process preset, by Hotend's id. The preset gives the layer height.
    ProcessPreset {
        Standard020 => "standard-0.20mm",
        Fine012 => "fine-0.12mm",
        Draft024 => "draft-0.24mm",
    }
}

impl ProcessPreset {
    /// The layer height of the preset in hundredths of a millimetre.
    pub const fn layer_height_hundredths(self) -> i64 {
        match self {
            ProcessPreset::Standard020 => 20,
            ProcessPreset::Fine012 => 12,
            ProcessPreset::Draft024 => 24,
        }
    }
}

token_enum! {
    /// The sparse infill pattern.
    InfillPattern {
        Grid => "grid",
        Gyroid => "gyroid",
    }
}

token_enum! {
    /// Where the seam of the walls goes.
    Seam {
        Aligned => "aligned",
        Back => "back",
    }
}

token_enum! {
    /// The kind of support structure.
    SupportKind {
        Tree => "tree",
        Normal => "normal",
    }
}

token_enum! {
    /// Where supports may stand.
    SupportWhere {
        Everywhere => "everywhere",
        BedOnly => "bed_only",
    }
}

token_enum! {
    /// The unit of a field's value.
    Unit {
        None => "",
        Step => "step",
        Enum => "enum",
        Flag => "flag",
        Count => "count",
        Layers => "layers",
        Millimetre => "mm",
        MillimetrePerSecond => "mm/s",
        MillimetrePerSecondSquared => "mm/s2",
        CubicMillimetrePerSecond => "mm3/s",
        Celsius => "C",
        Percent => "percent",
        Degrees => "degrees",
    }
}

token_enum! {
    /// Who decided a value.
    Source {
        Rule => "rule",
        Intent => "intent",
        Agent => "agent",
        Person => "person",
        History => "history",
        Calibration => "calibration",
        Profile => "profile",
        /// A print on this printer, plate and material that the person reported as good:
        /// the proven baseline (`inputs::Baseline`).
        Proven => "proven",
    }
}

token_enum! {
    /// What a number rests on.
    Basis {
        Measured => "measured",
        Sourced => "sourced",
        Judgment => "judgment",
        Stock => "stock",
    }
}

token_enum! {
    /// How easily a material family lifts from the bed.
    WarpClass {
        Low => "low",
        Medium => "medium",
        High => "high",
    }
}

token_enum! {
    /// The stable id of a rule. It appears in every decision.
    RuleId {
        /// The person's answers about the printer: plate and spools.
        Set1 => "SET-1",
        Ad0 => "AD-0",
        Ad1 => "AD-1",
        Ad2 => "AD-2",
        Ad3 => "AD-3",
        Ad4 => "AD-4",
        Ad5 => "AD-5",
        Ad6 => "AD-6",
        /// Level 0: no brim rule fired.
        Lvl0 => "LVL-0",
        /// Level 1: a brim rule fired.
        Lvl1 => "LVL-1",
        /// Level 2 and what it brings: wider brim, longer fan delay, thicker first layer.
        Lvl2 => "LVL-2",
        /// Level 3: history only.
        Lvl3 => "LVL-3",
        /// A silk spool raises the level by one.
        LvlSilk => "LVL-SILK",
        /// The stored minimum level of earlier prints.
        LvlHist => "LVL-HIST",
        /// A level asked for by the agent or the person.
        LvlAsk => "LVL-ASK",
        /// A value of the material card.
        Card => "CARD",
        Plt1 => "PLT-1",
        Plt2 => "PLT-2",
        Mat1 => "MAT-1",
        Mat2 => "MAT-2",
        Vnt1 => "VNT-1",
        Sup1 => "SUP-1",
        Sup2 => "SUP-2",
        Sup3 => "SUP-3",
        Sup4 => "SUP-4",
        Spd1 => "SPD-1",
        Spd2 => "SPD-2",
        Pt1 => "PT-1",
        Noz1 => "NOZ-1",
        /// A value of the intent table.
        Int1 => "INT-1",
        /// An intent value that is not available fell back to the balanced one.
        Int2 => "INT-2",
        Stk1 => "STK-1",
        Fit1 => "FIT-1",
        Fit2 => "FIT-2",
        Fit3 => "FIT-3",
        Fit4 => "FIT-4",
        Fit5 => "FIT-5",
        /// The fit signature of the calibration gauge.
        Sig1 => "SIG-1",
        /// The proven baseline: the sticking settings of a print the person reported as
        /// good on this printer, plate and material are the floor of the plan.
        Prv1 => "PRV-1",
        /// Fixed in version 1.
        Fix1 => "FIX-1",
        /// The cross-field rules, checked last.
        Xf1 => "XF-1",
        /// A limit of the field itself: the end of its hard range, or the widest value
        /// Hotend offers for it.
        Env1 => "ENV-1",
    }
}

token_enum! {
    /// How a change that somebody asked for was answered.
    Answer {
        /// The value is used as asked.
        Accepted => "accepted",
        /// The number lay outside the envelope and the nearest bound is used.
        Clamped => "clamped",
        /// The value is not in the allowed set of this plate; nothing changed.
        NotAllowed => "not_allowed_here",
        /// The field cannot be changed on this plate; nothing changed.
        Locked => "locked",
        /// The change broke a cross-field rule and was taken back; nothing changed.
        Conflict => "conflict",
    }
}

impl Answer {
    /// Whether the change took effect, as asked or held to a bound.
    pub const fn applied(self) -> bool {
        matches!(self, Answer::Accepted | Answer::Clamped)
    }
}

/// A filament colour, `#RRGGBB`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Colour([u8; 3]);

impl Colour {
    /// Reads `#RRGGBB` (six hexadecimal digits, either case). Anything else is `None`.
    pub fn parse(text: &str) -> Option<Self> {
        let digits = text.strip_prefix('#')?;
        if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        let channel = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).ok();
        Some(Self([channel(0)?, channel(2)?, channel(4)?]))
    }

    /// Red, green and blue.
    pub const fn rgb(self) -> [u8; 3] {
        self.0
    }
}

impl fmt::Display for Colour {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [r, g, b] = self.0;
        write!(f, "#{r:02X}{g:02X}{b:02X}")
    }
}

impl Serialize for Colour {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

/// A calendar day, `YYYY-MM-DD`: the day a proven print was made. A closed type: three
/// numbers that form a real date between the years 2000 and 2199. It fills the place
/// `{date}` of a sentence as digits only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Day {
    year: u16,
    month: u8,
    day: u8,
}

impl Day {
    /// The day, when it exists on the calendar and lies in the years 2000 to 2199.
    pub fn new(year: u16, month: u8, day: u8) -> Option<Self> {
        if !(2000..=2199).contains(&year) || !(1..=12).contains(&month) || day == 0 {
            return None;
        }
        let leap =
            (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
        let days = match month {
            2 if leap => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        };
        (day <= days).then_some(Self { year, month, day })
    }

    /// Reads exactly `YYYY-MM-DD` (ten ASCII characters). Anything else is `None`.
    pub fn parse(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
            return None;
        }
        let digits = |range: std::ops::Range<usize>| -> Option<u16> {
            let part = text.get(range)?;
            if !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            part.parse().ok()
        };
        let year = digits(0..4)?;
        let month = u8::try_from(digits(5..7)?).ok()?;
        let day = u8::try_from(digits(8..10)?).ok()?;
        Self::new(year, month, day)
    }
}

impl fmt::Display for Day {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

impl Serialize for Day {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

/// Where a spool sits: an AMS slot `1` to `16`, or one of the two outside holders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SpoolSlot {
    /// An AMS slot, counted from 1.
    Ams(u8),
    /// The outside holder that feeds the main, direct-drive nozzle.
    ExternalMain,
    /// The outside holder that feeds the auxiliary nozzle.
    ExternalAuxiliary,
}

impl SpoolSlot {
    /// The highest AMS slot number.
    pub const MAX_AMS: u8 = 16;

    /// Reads `1` to `16`, `external_main` or `external_auxiliary`.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "external_main" => Some(Self::ExternalMain),
            "external_auxiliary" => Some(Self::ExternalAuxiliary),
            _ => {
                let plain = !text.is_empty()
                    && text.len() <= 2
                    && !text.starts_with('0')
                    && text.bytes().all(|byte| byte.is_ascii_digit());
                let number: u8 = if plain {
                    text.parse().ok()?
                } else {
                    return None;
                };
                Self::ams(number)
            }
        }
    }

    /// The AMS slot with this number, when it is `1` to `16`.
    pub fn ams(number: u8) -> Option<Self> {
        (1..=Self::MAX_AMS)
            .contains(&number)
            .then_some(Self::Ams(number))
    }

    /// Whether the spool sits in the AMS.
    pub const fn is_ams(self) -> bool {
        matches!(self, Self::Ams(_))
    }
}

impl fmt::Display for SpoolSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ams(number) => write!(f, "{number}"),
            Self::ExternalMain => f.write_str("external_main"),
            Self::ExternalAuxiliary => f.write_str("external_auxiliary"),
        }
    }
}

impl Serialize for SpoolSlot {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

/// The id of a material, such as `pla-basic` or `tpu-90a`: lower-case letters, digits and
/// dashes, 40 characters at most. It is the only identifier of the model that is not a token
/// of this crate, so its characters are fixed here.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MaterialId(String);

impl MaterialId {
    /// Reads an id. `None` when a character or the length is not allowed.
    pub fn parse(text: &str) -> Option<Self> {
        let mut bytes = text.bytes();
        let first = bytes.next()?;
        let head = first.is_ascii_lowercase() || first.is_ascii_digit();
        let tail =
            bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        (head && tail && text.len() <= 40).then(|| Self(text.to_owned()))
    }

    /// The id.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The Shore A hardness the id names (`tpu-90a` gives 90), if any: the first two digits
    /// that are followed by `a` and then by the end of the id or a hyphen. This is the rule
    /// of `python/hotend_fit/materials.py`, so both sides read the same hardness from every
    /// id (`tests/data/shore-table.json`); digits before the two do not hide them, so
    /// `tpu-085a` is 85A here as it is there.
    pub fn shore_a(&self) -> Option<u8> {
        let bytes = self.0.as_bytes();
        bytes.windows(3).enumerate().find_map(|(at, window)| {
            let digits =
                window[0].is_ascii_digit() && window[1].is_ascii_digit() && window[2] == b'a';
            let ends = bytes
                .get(at + 3)
                .is_none_or(|next| !next.is_ascii_alphanumeric());
            (digits && ends).then(|| (window[0] - b'0') * 10 + (window[1] - b'0'))
        })
    }
}

impl fmt::Display for MaterialId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for MaterialId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

/// The value of a field.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value {
    /// "As the installed profile has it": nothing is written for the field.
    Unset,
    /// A whole number: a count, a step, whole millimetres, degrees, percent or a speed.
    Whole(i64),
    /// A decimal with two places, in the field's unit.
    Decimal(f64),
    /// A token of the field's closed list.
    Token(&'static str),
    /// On or off.
    Flag(bool),
    /// A colour.
    Colour(Colour),
    /// A spool slot.
    Slot(SpoolSlot),
}

impl Value {
    /// Whether the field is left to the installed profile.
    pub const fn is_unset(&self) -> bool {
        matches!(self, Value::Unset)
    }

    /// The number of a `Whole` or `Decimal` value.
    pub fn number(&self) -> Option<f64> {
        match self {
            Value::Whole(whole) => Some(*whole as f64),
            Value::Decimal(decimal) => Some(*decimal),
            _ => None,
        }
    }

    /// The whole number of a `Whole` value.
    pub const fn whole(&self) -> Option<i64> {
        match self {
            Value::Whole(whole) => Some(*whole),
            _ => None,
        }
    }

    /// The token of a `Token` value.
    pub const fn token(&self) -> Option<&'static str> {
        match self {
            Value::Token(token) => Some(token),
            _ => None,
        }
    }

    /// The state of a `Flag` value.
    pub const fn flag(&self) -> Option<bool> {
        match self {
            Value::Flag(flag) => Some(*flag),
            _ => None,
        }
    }

    /// A decimal rounded to two places, the precision every decimal field is written with.
    pub fn decimal(value: f64) -> Self {
        Value::Decimal((value * 100.0).round() / 100.0)
    }
}

impl Serialize for Value {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Value::Unset => serializer.serialize_none(),
            Value::Whole(whole) => serializer.serialize_i64(*whole),
            Value::Decimal(decimal) => serializer.serialize_f64(*decimal),
            Value::Token(token) => serializer.serialize_str(token),
            Value::Flag(flag) => serializer.serialize_bool(*flag),
            Value::Colour(colour) => colour.serialize(serializer),
            Value::Slot(slot) => slot.serialize(serializer),
        }
    }
}

/// The values of a field Hotend is willing to deliver for this plate.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Envelope {
    /// Every number from `min` to `max` in steps of `step`.
    Range {
        /// The lowest value.
        min: f64,
        /// The highest value.
        max: f64,
        /// The distance between two values.
        step: f64,
    },
    /// The listed values.
    Set {
        /// The values that may be chosen, the rule's value among them.
        allowed: Vec<Value>,
    },
    /// The value cannot be changed; `rule` says why.
    Locked {
        /// The rule that owns the value.
        rule: RuleId,
    },
}

/// What a decision applies to. Version 1 has one set of settings per plate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// The whole plate.
    Plate,
}

/// The answer to a change of one field, as the plan records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct Answered {
    /// Who asked.
    pub by: Asker,
    /// What became of the change.
    pub answer: Answer,
    /// The rule that owns the envelope, the bound or the tie that decided the answer.
    pub rule: RuleId,
    /// The id of the catalogue sentence that says so (`[answer]` in `sentences.toml`).
    pub detail: &'static str,
}

/// How the value of one field was decided.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Decision {
    /// The field.
    pub field: Field,
    /// The value in the plan.
    pub value: Value,
    /// The unit of `value`.
    pub unit: Unit,
    /// Who decided `value`.
    pub source: Source,
    /// The rule that set `value`, or that owns the bound it was held to.
    pub rule: RuleId,
    /// What the number rests on.
    pub basis: Basis,
    /// The id of the catalogue sentence that explains the decision.
    pub sentence_id: &'static str,
    /// That sentence with the plan's numbers filled in.
    pub sentence: String,
    /// The values a change to this field must stay in.
    pub envelope: Envelope,
    /// What the agent or the person asked for, when that differs from `value`.
    pub asked: Option<Value>,
    /// What the decision applies to.
    pub scope: Scope,
    /// The value the rules alone gave, before any accepted change. Equal to `value` in a
    /// plan without changes. A change may never move a sticking field to the weak side of it.
    pub rule_value: Value,
    /// Who decided `rule_value`.
    pub rule_source: Source,
    /// The rule that set `rule_value`.
    pub rule_rule: RuleId,
    /// How the change that was asked for this field was answered; `None` when nobody asked.
    pub answer: Option<Answered>,
}

/// Why a value is what it is: who decided it, by which rule, and what the number rests on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Why {
    pub(crate) source: Source,
    pub(crate) rule: RuleId,
    pub(crate) basis: Basis,
}

impl Decision {
    /// A decision of the rule plan: nobody has asked for anything else.
    pub(crate) fn of_rules(
        field: Field,
        value: Value,
        why: Why,
        sentence_id: &'static str,
        sentence: String,
        envelope: Envelope,
    ) -> Self {
        Self {
            field,
            value,
            unit: bounds::spec(field).unit,
            source: why.source,
            rule: why.rule,
            basis: why.basis,
            sentence_id,
            sentence,
            envelope,
            asked: None,
            scope: Scope::Plate,
            rule_value: value,
            rule_source: why.source,
            rule_rule: why.rule,
            answer: None,
        }
    }
}

/// The filament of a plate. Version 1 has exactly one.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct FilamentSetting {
    /// The declared spool the filament comes from.
    pub spool: SpoolSlot,
    /// The device extruder id of the nozzle that spool feeds.
    pub nozzle_id: u8,
    /// The spool's colour.
    pub colour: Colour,
    /// The material.
    pub material_id: MaterialId,
    /// The material's family.
    pub family: Family,
}

/// The settings of one plate: a value for every field of [`Field::ALL`], each inside the
/// constant hard range of its field.
///
/// There is no public constructor and no `Deserialize`: the only way to obtain one is a plan
/// computed by this crate.
#[derive(Debug, Clone, PartialEq)]
pub struct PrintSettings {
    values: Vec<Value>,
    filaments: Vec<FilamentSetting>,
}

impl PrintSettings {
    /// Builds the settings from one value per field, in the order of [`Field::ALL`]. Every
    /// value is checked against the field's type and hard range, so a rule or a data file
    /// cannot produce a value outside them.
    pub(crate) fn from_values(
        values: Vec<Value>,
        filament: FilamentSetting,
    ) -> Result<Self, RangeError> {
        if values.len() != Field::ALL.len() {
            return Err(RangeError::Incomplete);
        }
        for (field, value) in Field::ALL.iter().zip(&values) {
            bounds::check(*field, value)?;
        }
        Ok(Self {
            values,
            filaments: vec![filament],
        })
    }

    /// The value of a field.
    pub fn get(&self, field: Field) -> Value {
        // `from_values` made the list as long as `Field::ALL`.
        self.values
            .get(field.index())
            .copied()
            .unwrap_or(Value::Unset)
    }

    /// Every field with its value, in the order of [`Field::ALL`].
    pub fn iter(&self) -> impl Iterator<Item = (Field, Value)> + '_ {
        Field::ALL.iter().copied().zip(self.values.iter().copied())
    }

    /// The filaments of the plate: one in version 1.
    pub fn filaments(&self) -> &[FilamentSetting] {
        &self.filaments
    }

    /// The plate type, when the token is one of [`PlateType`].
    pub fn plate_type(&self) -> Option<PlateType> {
        self.get(Field::PlateType)
            .token()
            .and_then(PlateType::from_token)
    }

    /// The intent.
    pub fn intent(&self) -> Option<Intent> {
        self.get(Field::Intent).token().and_then(Intent::from_token)
    }

    /// The process preset.
    pub fn process(&self) -> Option<ProcessPreset> {
        self.get(Field::Process)
            .token()
            .and_then(ProcessPreset::from_token)
    }

    /// The adhesion level, 0 to 3.
    pub fn level(&self) -> i64 {
        self.get(Field::Level).whole().unwrap_or(0)
    }
}

impl Serialize for PrintSettings {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        struct Fields<'a>(&'a PrintSettings);
        impl Serialize for Fields<'_> {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let mut map = serializer.serialize_map(Some(self.0.values.len()))?;
                for (field, value) in self.0.iter() {
                    map.serialize_entry(field.as_str(), &value)?;
                }
                map.end()
            }
        }
        let mut record = serializer.serialize_struct("PrintSettings", 3)?;
        record.serialize_field("schema", &SCHEMA)?;
        record.serialize_field("filaments", &self.filaments)?;
        record.serialize_field("fields", &Fields(self))?;
        record.end()
    }
}

/// A code with its catalogue sentence: a refusal, a warning, a note or a question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct Notice {
    /// The stable code.
    pub code: &'static str,
    /// The sentence, from the catalogue.
    pub sentence: String,
}

/// When a line of the list applies. The export resolves the condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Condition {
    /// Only when the print takes longer than this many minutes.
    PrintLongerThanMinutes(u32),
    /// Only when the editable project is handed over with the sliced file.
    ProjectDelivered,
}

/// One line of the before-you-print list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct ChecklistItem {
    /// The id of the catalogue sentence.
    pub id: &'static str,
    /// The priority class: 1 plate, 2 feed and loading, 3 which file to open, 4 send window,
    /// 5 what to watch, 6 afterwards.
    pub class: u8,
    /// The sentence.
    pub text: String,
    /// When the line applies; `None` means always.
    pub condition: Option<Condition>,
}

/// The before-you-print list of a plan, before its length limit is applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct Checklist {
    /// Every line the plan asks for, by class and then in catalogue order.
    pub items: Vec<ChecklistItem>,
    /// The most lines the person is shown: eight, or ten for a flexible material.
    pub limit: usize,
}

/// What the rules worked out on the way to the values. A later change is judged against it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Derived {
    /// The brim rules that fired, in order.
    pub fired: Vec<RuleId>,
    /// The level the rules, the spool and the history give, before anyone asked for more.
    pub rule_level: i64,
    /// The widest brim the brim rules ask for on this plate, before level 2 widens it.
    pub asked_brim_width_mm: i64,
    /// Whether a rule requires the brim: `none` is then not an allowed kind.
    pub brim_required: bool,
    /// The widest brim a change may ask for.
    pub brim_width_max_mm: i64,
    /// The warp class of the material.
    pub warp_class: WarpClass,
    /// Whether a declared mating face has an edge on the bed.
    pub fit_plate: bool,
    /// The bed temperature window of the material on this plate: the card's window, with its
    /// top cut at the printer's and the filament's limit. An echoed bed temperature is judged
    /// against it ([`PrintPlan::check_echo`]).
    pub bed_window_c: [i64; 2],
    /// The nozzle temperature range of the material family, a constant of its card. Hotend
    /// writes no nozzle temperature; an echoed one is judged against this range.
    pub nozzle_range_c: [i64; 2],
    /// The fewest wall loops the rules allow.
    pub wall_loops_floor: i64,
    /// The overhang area that counts towards supports.
    pub support_mm2: f64,
    /// How many overhang regions count, when the engine classified them all.
    pub support_regions: Option<usize>,
    /// The largest counted region.
    pub support_largest_mm2: f64,
    /// Whether gentle motion (SPD-2) is on.
    pub gentle_motion: bool,
    /// Whether a fit calibration set the three calibrated fields.
    pub calibrated: bool,
    /// The elephant foot compensation the plan counts on, written or not.
    pub elephant_foot_mm: f64,
    /// The day of the proven print whose settings are the floor of this plan; absent when
    /// no proven baseline applies (and then left out of the serialized plan, so that a plan
    /// without a baseline is the same, byte for byte, as before baselines existed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proven_on: Option<Day>,
    /// The fields whose value the proven baseline set, because the rules alone gave a less
    /// careful one. Left out when empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub proven_fields: Vec<Field>,
}

/// The revisions of the data files a plan was computed with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct Revisions {
    /// `print-rules.toml`.
    pub print_rules: String,
    /// `print-cards.toml`.
    pub print_cards: String,
    /// `intents.toml`.
    pub intents: String,
    /// `sentences.toml`.
    pub sentences: String,
}

/// The settings of one plate with the reasons for them.
///
/// A blocked plan has no settings and no decisions: `blocked` says what the person must do.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct PrintPlan {
    /// The settings; `None` when the plan is blocked.
    pub settings: Option<PrintSettings>,
    /// One decision per field, in the order of [`Field::ALL`].
    pub decisions: Vec<Decision>,
    /// The plate facts the plan was computed from; `None` when they were refused.
    pub facts: Option<PlateFacts>,
    /// What the rules worked out on the way.
    pub derived: Option<Derived>,
    /// What only the person can answer.
    pub questions: Vec<Notice>,
    /// What the person should know and may ignore.
    pub warnings: Vec<Notice>,
    /// Remarks that are part of the plan: known limits and why supports are on.
    pub notes: Vec<Notice>,
    /// Why there is no plan, with what to do about it.
    pub blocked: Option<Notice>,
    /// The before-you-print list.
    pub checklist: Checklist,
    /// The revisions of the data files.
    pub revisions: Revisions,
}

impl PrintPlan {
    /// The decision for a field; `None` in a blocked plan.
    pub fn decision(&self, field: Field) -> Option<&Decision> {
        self.decisions
            .iter()
            .find(|decision| decision.field == field)
    }

    /// The value of a field; `None` in a blocked plan.
    pub fn value(&self, field: Field) -> Option<Value> {
        self.settings.as_ref().map(|settings| settings.get(field))
    }

    /// The range a value that a sliced file states for `field` must lie in, for this plan:
    /// the constant hard range of the field, narrowed to the card's bed window for the two
    /// bed temperatures and to the family's nozzle range for the two nozzle temperatures.
    /// `None` for a field without a numeric range. A blocked plan narrows nothing.
    pub fn echo_range(&self, field: Field) -> Option<(f64, f64)> {
        let (hard_min, hard_max) = bounds::numeric_range(field)?;
        let narrowed = self.derived.as_ref().and_then(|derived| match field {
            Field::FirstLayerBed | Field::LayersBed => Some(derived.bed_window_c),
            Field::FirstLayerNozzle | Field::LayersNozzle => Some(derived.nozzle_range_c),
            _ => None,
        });
        Some(match narrowed {
            Some([low, high]) => ((low as f64).max(hard_min), (high as f64).min(hard_max)),
            None => (hard_min, hard_max),
        })
    }

    /// Whether a value that a sliced file states for `field` may stand in a print of this
    /// plan, whether Hotend wrote the field or not: [`bounds::check`], and for the bed and
    /// the nozzle temperatures the narrower range of [`PrintPlan::echo_range`]. An echo
    /// always states a value, so `Unset` is refused.
    pub fn check_echo(&self, field: Field, value: &Value) -> Result<(), RangeError> {
        if value.is_unset() {
            return Err(RangeError::Required(field));
        }
        bounds::check(field, value)?;
        if let (Some((min, max)), Some(number)) = (self.echo_range(field), value.number())
            && (number < min || number > max)
        {
            return Err(RangeError::OutOfRange { field, min, max });
        }
        Ok(())
    }

    /// How many values of the plan differ from the rules' because this asker asked. A change
    /// that was held to the rule's own value, or asked for exactly that value, does not
    /// count: nothing changed.
    pub fn changed_by(&self, asker: Asker) -> usize {
        self.decisions
            .iter()
            .filter(|decision| {
                decision.value != decision.rule_value
                    && decision
                        .answer
                        .is_some_and(|answered| answered.by == asker && answered.answer.applied())
            })
            .count()
    }
}
