//! The thresholds of the rules and the intent table: `data/print-rules.toml` and
//! `data/intents.toml`.
//!
//! Both are data so that a measured print can move a number without a code change. They are
//! read strictly (an unknown key is an error), every table carries a mark and a source, and
//! every value that ends up in a field is checked against the field's constant hard range
//! when the file loads. A data file can therefore tune a rule; it cannot widen a range.

use serde::Deserialize;

use crate::bounds;
use crate::explain::{DataError, check_revision};
use crate::model::{Basis, Field, InfillPattern, Intent, PlateType, ProcessPreset, Seam, Value};

const RULES_FILE: &str = "print-rules.toml";
const INTENTS_FILE: &str = "intents.toml";

/// STK-1: the sticking values of the standard preset.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    /// The preset the values belong to.
    pub preset: ProcessPreset,
    /// Elephant foot compensation in mm.
    pub elephant_foot_mm: f64,
    /// First layer height in mm.
    pub first_layer_height_mm: f64,
    /// First layer line width in mm.
    pub first_layer_line_width_mm: f64,
    /// First layer acceleration in mm/s2.
    pub first_layer_accel_mm_s2: i64,
    /// What the values rest on.
    pub mark: Basis,
    /// Where they come from.
    pub source: String,
}

/// AD-1.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThinStrip {
    /// A strip narrower than this asks for a brim.
    pub below_mm: f64,
    /// The brim width.
    pub width_mm: i64,
    /// What the numbers rest on.
    pub mark: Basis,
    /// Where they come from.
    pub source: String,
}

/// AD-2.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SmallFoot {
    /// A contact area below this asks for `width_mm`.
    pub below_mm2: f64,
    /// The brim width for a small foot.
    pub width_mm: i64,
    /// A contact area below this asks for `tiny_width_mm`.
    pub tiny_below_mm2: f64,
    /// The brim width for a tiny foot.
    pub tiny_width_mm: i64,
    /// What the numbers rest on.
    pub mark: Basis,
    /// Where they come from.
    pub source: String,
}

/// AD-3.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Slender {
    /// A slenderness above this asks for `width_mm`.
    pub above: f64,
    /// The brim width for a slender piece.
    pub width_mm: i64,
    /// A slenderness above this asks for `very_width_mm`, level 2 and gentle motion.
    pub very_above: f64,
    /// The brim width for a very slender piece.
    pub very_width_mm: i64,
    /// A slenderness above this also gives the warning to turn or split the part.
    pub warn_above: f64,
    /// What the numbers rest on.
    pub mark: Basis,
    /// Where they come from.
    pub source: String,
}

/// AD-4.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LongPart {
    /// Warp class low: a contact span above this asks for `low_width_mm`.
    pub low_above_mm: f64,
    /// The brim width for warp class low.
    pub low_width_mm: i64,
    /// Warp class medium: a contact span above this asks for `medium_width_mm`.
    pub medium_above_mm: f64,
    /// The brim width for warp class medium.
    pub medium_width_mm: i64,
    /// What the numbers rest on.
    pub mark: Basis,
    /// Where they come from.
    pub source: String,
}

/// AD-5.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Warping {
    /// The brim width every plate of a warp class high material gets at least.
    pub width_mm: i64,
    /// What the number rests on.
    pub mark: Basis,
    /// Where it comes from.
    pub source: String,
}

/// AD-6.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlexibleBrim {
    /// The brim width of a flexible material.
    pub width_mm: i64,
    /// Its brim width when AD-1 or AD-3 fired.
    pub thin_or_tall_width_mm: i64,
    /// What the numbers rest on.
    pub mark: Basis,
    /// Where they come from.
    pub source: String,
}

/// When an inner brim is possible.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InnerBrim {
    /// An inner loop must be at least `2 x width + extra_mm` wide.
    pub extra_mm: f64,
    /// What the number rests on.
    pub mark: Basis,
    /// Where it comes from.
    pub source: String,
}

/// FIT-2.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequiredGap {
    /// The gap under a brim that a rule requires.
    pub required_mm: f64,
    /// The elephant foot compensation from which that gap is used.
    pub min_elephant_foot_mm: f64,
    /// What the numbers rest on.
    pub mark: Basis,
    /// Where they come from.
    pub source: String,
}

/// The widest brim a change may ask for.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrimLimit {
    /// For warp classes low and medium.
    pub max_mm: i64,
    /// For warp class high.
    pub max_warp_high_mm: i64,
    /// What the numbers rest on.
    pub mark: Basis,
    /// Where they come from.
    pub source: String,
}

/// The brim rules.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrimRules {
    /// AD-1.
    pub thin_strip: ThinStrip,
    /// AD-2.
    pub small_foot: SmallFoot,
    /// AD-3.
    pub slender: Slender,
    /// AD-4.
    pub long_part: LongPart,
    /// AD-5.
    pub warping: Warping,
    /// AD-6.
    pub flexible: FlexibleBrim,
    /// The inner brim.
    pub inner: InnerBrim,
    /// FIT-2.
    pub gap: RequiredGap,
    /// The envelope's top.
    pub limit: BrimLimit,
}

/// The level and what level 2 brings.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LevelRules {
    /// A silk spool raises the level by one, to this at most.
    pub silk_max: i64,
    /// The highest level the agent or the person may ask for.
    pub ask_max: i64,
    /// Level 2: the brim is at least this wide.
    pub brim_min_mm: i64,
    /// Level 2: and this much wider than the rules' width.
    pub brim_extra_mm: i64,
    /// Level 2: but at most this wide.
    pub brim_max_mm: i64,
    /// Level 2: layers the part fan stays off for.
    pub fan_off_layers: i64,
    /// Level 2: the first layer height on `first_layer_height_plate`.
    pub first_layer_height_mm: f64,
    /// The plate that gets the thicker first layer.
    pub first_layer_height_plate: PlateType,
    /// What the numbers rest on.
    pub mark: Basis,
    /// Where they come from.
    pub source: String,
}

/// MAT-1.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Walls {
    /// A strip narrower than this asks for `loops`.
    pub strip_below_mm: f64,
    /// A thinnest wall of at most this asks for `loops`.
    pub min_wall_at_most_mm: f64,
    /// The fewest wall loops for thin walls.
    pub loops: i64,
    /// What the numbers rest on.
    pub mark: Basis,
    /// Where they come from.
    pub source: String,
}

/// MAT-2.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Flexible {
    /// More flexible pieces than this on one plate give a warning.
    pub max_pieces: usize,
    /// A Shore A hardness of at most this is refused with `refused_nozzle_mm`.
    pub refused_shore_a: u8,
    /// The nozzle diameter up to which the soft grade is refused.
    pub refused_nozzle_mm: f64,
    /// What the numbers rest on.
    pub mark: Basis,
    /// Where they come from.
    pub source: String,
}

/// SUP-1 to SUP-4.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Supports {
    /// A bridge of at most this span does not count.
    pub bridge_max_mm: f64,
    /// The ceiling of a round hole of at most this diameter does not count.
    pub hole_max_mm: f64,
    /// A free overhang reaching at most this far does not count.
    pub reach_max_mm: f64,
    /// From this counted area supports may be switched on.
    pub offer_from_mm2: f64,
    /// From this counted area supports are on.
    pub on_from_mm2: f64,
    /// Above this counted area supports cannot be switched off.
    pub forced_above_mm2: f64,
    /// `on_from_mm2` for a flexible material.
    pub flexible_on_from_mm2: f64,
    /// `forced_above_mm2` for a flexible material.
    pub flexible_forced_above_mm2: f64,
    /// From this counted area above the part itself supports may stand on the part.
    pub over_part_from_mm2: f64,
    /// From this overhang on mating faces normal supports are used.
    pub fit_overhang_from_mm2: f64,
    /// The support threshold in degrees from horizontal.
    pub threshold_deg: i64,
    /// What the numbers rest on.
    pub mark: Basis,
    /// Where they come from.
    pub source: String,
}

/// SPD-2.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gentle {
    /// A slenderness above this turns gentle motion on.
    pub above: f64,
    /// The acceleration cap.
    pub accel_cap_mm_s2: i64,
    /// The outer wall acceleration cap.
    pub outer_wall_accel_cap_mm_s2: i64,
    /// The outer wall speed cap.
    pub outer_wall_speed_cap_mm_s: i64,
    /// What the numbers rest on.
    pub mark: Basis,
    /// Where they come from.
    pub source: String,
}

/// VNT-1.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Venting {
    /// The venting line applies to prints longer than this.
    pub longer_than_minutes: u32,
    /// What the number rests on.
    pub mark: Basis,
    /// Where it comes from.
    pub source: String,
}

/// The cross-field rules for the first layer.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cross {
    /// The first layer is at most this many nozzle diameters high.
    pub first_layer_height_max_factor: f64,
    /// The first layer line is at least this many nozzle diameters wide.
    pub line_width_min_factor: f64,
    /// And at most this many.
    pub line_width_max_factor: f64,
    /// What the numbers rest on.
    pub mark: Basis,
    /// Where they come from.
    pub source: String,
}

/// The fit signature comparison.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignatureRules {
    /// A bed temperature this far from the gauge's still counts as the same.
    pub bed_tolerance_c: i64,
    /// What the number rests on.
    pub mark: Basis,
    /// Where it comes from.
    pub source: String,
}

/// `print-rules.toml`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleData {
    schema: u32,
    /// The revision of the file.
    pub revision: String,
    /// STK-1.
    pub baseline: Baseline,
    /// AD-1 to AD-6, FIT-2.
    pub brim: BrimRules,
    /// The level.
    pub level: LevelRules,
    /// MAT-1.
    pub walls: Walls,
    /// MAT-2.
    pub flexible: Flexible,
    /// SUP-1 to SUP-4.
    pub supports: Supports,
    /// SPD-2.
    pub gentle: Gentle,
    /// VNT-1.
    pub venting: Venting,
    /// The cross-field rules.
    pub cross: Cross,
    /// The fit signature.
    pub signature: SignatureRules,
}

fn rules_error(error: impl ToString) -> DataError {
    DataError::new(RULES_FILE, error.to_string())
}

impl RuleData {
    /// Reads and checks the rule thresholds.
    pub fn from_toml(text: &str) -> Result<Self, DataError> {
        let data: Self = toml::from_str(text).map_err(rules_error)?;
        check_revision(RULES_FILE, data.schema, &data.revision)?;
        data.check()?;
        Ok(data)
    }

    fn check(&self) -> Result<(), DataError> {
        let whole = |field: Field, value: i64| {
            bounds::check(field, &Value::Whole(value)).map_err(rules_error)
        };
        let decimal = |field: Field, value: f64| {
            bounds::check(field, &Value::Decimal(value)).map_err(rules_error)
        };
        let brim = &self.brim;
        for width in [
            brim.thin_strip.width_mm,
            brim.small_foot.width_mm,
            brim.small_foot.tiny_width_mm,
            brim.slender.width_mm,
            brim.slender.very_width_mm,
            brim.long_part.low_width_mm,
            brim.long_part.medium_width_mm,
            brim.warping.width_mm,
            brim.flexible.width_mm,
            brim.flexible.thin_or_tall_width_mm,
            brim.limit.max_mm,
            brim.limit.max_warp_high_mm,
            self.level.brim_min_mm,
            self.level.brim_max_mm,
        ] {
            whole(Field::BrimWidth, width)?;
        }
        decimal(Field::BrimGap, brim.gap.required_mm)?;
        decimal(Field::ElephantFoot, self.baseline.elephant_foot_mm)?;
        decimal(Field::FirstLayerHeight, self.baseline.first_layer_height_mm)?;
        decimal(
            Field::FirstLayerLineWidth,
            self.baseline.first_layer_line_width_mm,
        )?;
        whole(
            Field::FirstLayerAccel,
            self.baseline.first_layer_accel_mm_s2,
        )?;
        decimal(Field::FirstLayerHeight, self.level.first_layer_height_mm)?;
        whole(Field::PartFanOffLayers, self.level.fan_off_layers)?;
        whole(Field::Level, self.level.silk_max)?;
        whole(Field::Level, self.level.ask_max)?;
        whole(Field::WallLoops, self.walls.loops)?;
        whole(Field::SupportsThreshold, self.supports.threshold_deg)?;
        whole(Field::AccelCap, self.gentle.accel_cap_mm_s2)?;
        whole(
            Field::OuterWallAccelCap,
            self.gentle.outer_wall_accel_cap_mm_s2,
        )?;
        whole(
            Field::OuterWallSpeedCap,
            self.gentle.outer_wall_speed_cap_mm_s,
        )?;

        let thresholds = [
            brim.thin_strip.below_mm,
            brim.small_foot.below_mm2,
            brim.small_foot.tiny_below_mm2,
            brim.slender.above,
            brim.slender.very_above,
            brim.slender.warn_above,
            brim.long_part.low_above_mm,
            brim.long_part.medium_above_mm,
            brim.inner.extra_mm,
            brim.gap.min_elephant_foot_mm,
            self.walls.strip_below_mm,
            self.walls.min_wall_at_most_mm,
            self.flexible.refused_nozzle_mm,
            self.supports.bridge_max_mm,
            self.supports.hole_max_mm,
            self.supports.reach_max_mm,
            self.supports.offer_from_mm2,
            self.supports.on_from_mm2,
            self.supports.forced_above_mm2,
            self.supports.flexible_on_from_mm2,
            self.supports.flexible_forced_above_mm2,
            self.supports.over_part_from_mm2,
            self.supports.fit_overhang_from_mm2,
            self.gentle.above,
            self.cross.first_layer_height_max_factor,
            self.cross.line_width_min_factor,
            self.cross.line_width_max_factor,
        ];
        if thresholds
            .iter()
            .any(|value| !value.is_finite() || *value < 0.0)
        {
            return Err(rules_error(
                "a threshold is negative or not a finite number",
            ));
        }
        let ordered = brim.small_foot.tiny_below_mm2 <= brim.small_foot.below_mm2
            && brim.small_foot.tiny_width_mm >= brim.small_foot.width_mm
            && brim.slender.above <= brim.slender.very_above
            && brim.slender.very_above <= brim.slender.warn_above
            && brim.slender.very_width_mm >= brim.slender.width_mm
            && self.supports.offer_from_mm2 <= self.supports.on_from_mm2
            && self.supports.on_from_mm2 <= self.supports.forced_above_mm2
            && self.supports.flexible_on_from_mm2 <= self.supports.flexible_forced_above_mm2
            && self.level.brim_min_mm <= self.level.brim_max_mm
            && self.level.brim_max_mm <= brim.limit.max_mm
            && brim.limit.max_mm <= brim.limit.max_warp_high_mm
            && self.signature.bed_tolerance_c >= 0;
        if !ordered {
            return Err(rules_error(
                "two thresholds of one rule are in the wrong order",
            ));
        }
        self.check_floors()
    }

    /// The constants of [`bounds::floors`]: what guards adhesion and the fit cannot be
    /// weakened by editing this file.
    fn check_floors(&self) -> Result<(), DataError> {
        use bounds::floors;
        let brim = &self.brim;
        let rigid_widths = [
            brim.thin_strip.width_mm,
            brim.small_foot.width_mm,
            brim.small_foot.tiny_width_mm,
            brim.slender.width_mm,
            brim.slender.very_width_mm,
            brim.long_part.low_width_mm,
            brim.long_part.medium_width_mm,
            brim.warping.width_mm,
            self.level.brim_min_mm,
        ];
        let holds = brim.gap.required_mm == floors::REQUIRED_BRIM_GAP_MM
            && brim.gap.min_elephant_foot_mm >= floors::TOUCHING_BRIM_ELEPHANT_FOOT_MIN_MM
            && rigid_widths
                .iter()
                .all(|width| *width >= floors::RULE_BRIM_MIN_MM)
            && brim.flexible.thin_or_tall_width_mm >= floors::FLEXIBLE_BRIM_MIN_MM
            && self.level.brim_extra_mm >= floors::LEVEL_BRIM_EXTRA_MIN_MM
            && brim.thin_strip.below_mm >= floors::THIN_STRIP_BELOW_MIN_MM
            && brim.small_foot.below_mm2 >= floors::SMALL_FOOT_BELOW_MIN_MM2
            && brim.slender.above <= floors::SLENDER_ABOVE_MAX
            && brim.long_part.low_above_mm <= floors::LONG_PART_LOW_ABOVE_MAX_MM
            && brim.long_part.medium_above_mm <= floors::LONG_PART_MEDIUM_ABOVE_MAX_MM
            && self.level.fan_off_layers >= floors::FAN_OFF_LAYERS_MIN
            && self.level.first_layer_height_mm >= floors::LEVEL_FIRST_LAYER_HEIGHT_MIN_MM
            && self.walls.loops >= floors::THIN_WALL_LOOPS_MIN;
        if !holds {
            return Err(rules_error(
                "a number asks for less adhesion or a looser fit than the constant floors of \
                 Hotend allow (src/bounds.rs, `floors`); correct the number",
            ));
        }
        Ok(())
    }
}

/// What one intent sets.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentRow {
    /// The intent.
    pub id: Intent,
    /// The process preset.
    pub process: ProcessPreset,
    /// Wall loops.
    pub wall_loops: i64,
    /// Sparse infill density in percent.
    pub infill_pct: i64,
    /// The infill pattern.
    pub infill_pattern: InfillPattern,
    /// The seam position.
    pub seam: Seam,
    /// The outer wall speed cap in mm/s, when the intent has one.
    pub outer_wall_speed_cap_mm_s: Option<i64>,
    /// The outer wall acceleration cap in mm/s2, when the intent has one.
    pub outer_wall_accel_cap_mm_s2: Option<i64>,
    /// What the values rest on.
    pub mark: Basis,
    /// Where they come from.
    pub source: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IntentsFile {
    schema: u32,
    revision: String,
    intent: Vec<IntentRow>,
}

/// `intents.toml`.
#[derive(Debug, Clone, PartialEq)]
pub struct Intents {
    revision: String,
    rows: Vec<IntentRow>,
}

impl Intents {
    /// Reads and checks the intent table: one row per intent, every value in its hard range.
    pub fn from_toml(text: &str) -> Result<Self, DataError> {
        let bad = |error: &dyn std::fmt::Display| DataError::new(INTENTS_FILE, error.to_string());
        let file: IntentsFile = toml::from_str(text).map_err(|error| bad(&error))?;
        check_revision(INTENTS_FILE, file.schema, &file.revision)?;
        for intent in Intent::ALL {
            if file.intent.iter().filter(|row| row.id == *intent).count() != 1 {
                return Err(bad(&format!(
                    "there must be exactly one row for `{intent}`"
                )));
            }
        }
        for row in &file.intent {
            bounds::check(Field::WallLoops, &Value::Whole(row.wall_loops))
                .map_err(|error| bad(&error))?;
            bounds::check(Field::InfillPct, &Value::Whole(row.infill_pct))
                .map_err(|error| bad(&error))?;
            if let Some(cap) = row.outer_wall_speed_cap_mm_s {
                bounds::check(Field::OuterWallSpeedCap, &Value::Whole(cap))
                    .map_err(|error| bad(&error))?;
            }
            if let Some(cap) = row.outer_wall_accel_cap_mm_s2 {
                bounds::check(Field::OuterWallAccelCap, &Value::Whole(cap))
                    .map_err(|error| bad(&error))?;
            }
        }
        Ok(Self {
            revision: file.revision,
            rows: file.intent,
        })
    }

    /// The revision of the file.
    pub fn revision(&self) -> &str {
        &self.revision
    }

    /// The row of an intent.
    pub fn row(&self, intent: Intent) -> Option<&IntentRow> {
        self.rows.iter().find(|row| row.id == intent)
    }
}
