//! Material print cards: `data/print-cards.toml`.
//!
//! Guarantees:
//! - there is exactly one card per [`Family`], and every card answers for every plate type
//!   a person can choose (the SuperTack plate aside, which always blocks the plan);
//! - every number of a card lies inside the constant hard range of the field it feeds, a
//!   raised bed pair is never below the level 0 pair, the bed after the first layer is never
//!   above the first layer's, and first layer speeds never rise with the level. A card that
//!   breaks one of these fails to load, so no card can weaken what a higher level gives.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::bounds::{self, Kind};
use crate::explain::{DataError, check_revision};
use crate::model::{Basis, Family, Field, PlateType, Value, WarpClass};

const FILE: &str = "print-cards.toml";

/// Whether a material may be printed on a plate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlateStatus {
    /// It may.
    Ok,
    /// It may, with a warning.
    Warning,
    /// It must not: the plan is blocked.
    Refused,
}

/// What the plate needs under this material.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Glue {
    /// Glue stick is required.
    Required,
    /// No glue.
    No,
    /// The sources say nothing; the list says nothing either.
    Unstated,
}

/// Where the filament must come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feed {
    /// Any slot.
    Any,
    /// Only the outside holder of the main, direct-drive nozzle.
    ExternalMain,
}

/// A material on one plate type.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlateCard {
    /// Whether the material may be printed on the plate.
    pub status: PlateStatus,
    /// Glue or no glue. Absent for a refused plate.
    pub glue: Option<Glue>,
    /// Bed temperature `[first layer, other layers]` at level 0.
    pub bed: Option<[i64; 2]>,
    /// What `bed` rests on.
    pub bed_mark: Option<Basis>,
    /// The pair from level 1.
    pub bed_raised: Option<[i64; 2]>,
    /// What `bed_raised` rests on.
    pub bed_raised_mark: Option<Basis>,
    /// The range `[low, high]` a bed temperature may be moved in.
    pub window: Option<[i64; 2]>,
    /// What `window` rests on.
    pub window_mark: Option<Basis>,
    /// Where the numbers come from.
    pub source: String,
}

/// The bed values of a plate the material may be printed on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bed {
    /// `[first layer, other layers]` at level 0.
    pub normal: [i64; 2],
    /// What `normal` rests on.
    pub normal_mark: Basis,
    /// The pair from level 1.
    pub raised: [i64; 2],
    /// What `raised` rests on.
    pub raised_mark: Basis,
    /// The window `[low, high]`.
    pub window: [i64; 2],
    /// Glue or no glue.
    pub glue: Glue,
}

impl PlateCard {
    /// The bed values; `None` for a refused plate.
    pub fn bed_values(&self) -> Option<Bed> {
        Some(Bed {
            normal: self.bed?,
            normal_mark: self.bed_mark?,
            raised: self.bed_raised?,
            raised_mark: self.bed_raised_mark?,
            window: self.window?,
            glue: self.glue?,
        })
    }
}

/// First layer speeds for levels 0, 1 and 2.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FirstLayerSpeeds {
    /// Wall speed per level, in mm/s.
    pub walls_mm_s: [i64; 3],
    /// Infill speed per level, in mm/s.
    pub infill_mm_s: [i64; 3],
    /// What each level's pair rests on.
    pub marks: [Basis; 3],
}

/// The auxiliary fan maximum for levels 0, 1 and 2.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuxFanMax {
    /// The cap per level, in percent.
    pub pct: [i64; 3],
    /// What each level's cap rests on.
    pub marks: [Basis; 3],
}

/// A hardness grade of a flexible family.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grade {
    /// The Shore A hardness the material id names.
    pub shore_a: u8,
    /// The flow cap of the grade, in mm3/s.
    pub max_flow_cap_mm3_s: f64,
    /// Lines of the list this grade adds, by id.
    #[serde(default)]
    pub list: Vec<String>,
    /// What the cap rests on.
    pub mark: Basis,
    /// Where the number comes from.
    pub source: String,
}

/// The print card of one material family.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Card {
    /// The family.
    pub family: Family,
    /// Whether the settings were ever printed. An unproven card says so in the list.
    pub proven: bool,
    /// Whether the material is flexible (AD-6, MAT-2, the longer list).
    pub flexible: bool,
    /// How easily the material lifts.
    pub warp_class: WarpClass,
    /// Where the filament must come from.
    pub feed: Feed,
    /// Layers the part fan stays off for.
    pub fan_off_layers: i64,
    /// What `fan_off_layers` rests on.
    pub fan_off_layers_mark: Basis,
    /// The auxiliary fan on the first layers, in percent.
    pub aux_fan_first_layers_pct: i64,
    /// What that rests on.
    pub aux_fan_first_layers_mark: Basis,
    /// The gap under a brim that no rule requires, in mm.
    pub brim_gap_mm: f64,
    /// What the gap rests on.
    pub brim_gap_mark: Basis,
    /// The bed temperature above which the vendor says to open the printer.
    pub vent_mark_c: Option<i64>,
    /// The nozzle temperature range `[low, high]` of the family. A constant: it is never
    /// read from a profile.
    pub nozzle_range_c: [i64; 2],
    /// What the range rests on.
    pub nozzle_range_mark: Basis,
    /// The steepest overhang the material prints without support, in degrees from vertical.
    /// The same number as `max_overhang_deg` of `python/hotend_fit/materials.py`; the
    /// support threshold may be moved up to 90 minus this value.
    pub max_overhang_deg: i64,
    /// What that rests on.
    pub max_overhang_mark: Basis,
    /// The outer wall speed cap of the family, in mm/s.
    pub outer_wall_cap_mm_s: Option<i64>,
    /// What the cap rests on.
    pub outer_wall_cap_mark: Option<Basis>,
    /// Lines of the list this family always adds, by id.
    pub list: Vec<String>,
    /// Where the numbers come from.
    pub source: String,
    /// First layer speeds per level.
    pub first_layer: FirstLayerSpeeds,
    /// The auxiliary fan maximum per level; absent means "as the profile has it".
    pub aux_fan_max: Option<AuxFanMax>,
    /// The card per plate type, by the plate's token.
    pub plate: BTreeMap<String, PlateCard>,
    /// Hardness grades with their own flow cap.
    #[serde(default)]
    pub grade: Vec<Grade>,
}

impl Card {
    /// The card for a plate type. `None` only for the SuperTack plate.
    pub fn on(&self, plate: PlateType) -> Option<&PlateCard> {
        self.plate.get(plate.as_str())
    }

    /// The grade for a Shore A hardness: the row with the lowest hardness that is not
    /// softer than the material, so a row holds for its hardness and every softer one. A
    /// material harder than every row has no grade. A material whose id names no hardness
    /// gets the softest row: what is not known is printed as carefully as the softest
    /// filament the card knows.
    pub fn grade(&self, shore_a: Option<u8>) -> Option<&Grade> {
        self.grade
            .iter()
            .filter(|grade| shore_a.is_none_or(|hardness| grade.shore_a >= hardness))
            .min_by_key(|grade| grade.shore_a)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    schema: u32,
    revision: String,
    card: Vec<Card>,
}

/// The loaded cards.
#[derive(Debug, Clone)]
pub struct Cards {
    revision: String,
    cards: Vec<Card>,
}

/// The plate types a card must answer for.
pub const CARD_PLATES: [PlateType; 4] = [
    PlateType::Cool,
    PlateType::TexturedPei,
    PlateType::HighTemp,
    PlateType::Engineering,
];

fn bad(family: Family, problem: &str) -> DataError {
    DataError::new(FILE, format!("the {family} card: {problem}"))
}

/// A whole number inside the hard range of the field it feeds.
fn in_range(family: Family, field: Field, value: i64) -> Result<(), DataError> {
    bounds::check(field, &Value::Whole(value)).map_err(|error| bad(family, &error.to_string()))
}

fn check_card(card: &Card) -> Result<(), DataError> {
    let family = card.family;
    let speeds = &card.first_layer;
    for level in 0..3 {
        in_range(family, Field::FirstLayerWallSpeed, speeds.walls_mm_s[level])?;
        in_range(
            family,
            Field::FirstLayerInfillSpeed,
            speeds.infill_mm_s[level],
        )?;
        if level > 0
            && (speeds.walls_mm_s[level] > speeds.walls_mm_s[level - 1]
                || speeds.infill_mm_s[level] > speeds.infill_mm_s[level - 1])
        {
            return Err(bad(family, "a first layer speed rises with the level"));
        }
    }
    if let Some(aux) = &card.aux_fan_max {
        for level in 0..3 {
            in_range(family, Field::AuxFanMax, aux.pct[level])?;
            if level > 0 && aux.pct[level] > aux.pct[level - 1] {
                return Err(bad(family, "the auxiliary fan cap rises with the level"));
            }
        }
    }
    in_range(family, Field::PartFanOffLayers, card.fan_off_layers)?;
    if card.fan_off_layers < bounds::floors::FAN_OFF_LAYERS_MIN {
        return Err(bad(
            family,
            "the part fan starts earlier than the constant floor allows",
        ));
    }
    in_range(
        family,
        Field::AuxFanFirstLayers,
        card.aux_fan_first_layers_pct,
    )?;
    bounds::check(Field::BrimGap, &Value::decimal(card.brim_gap_mm))
        .map_err(|error| bad(family, &error.to_string()))?;
    let [low, high] = card.nozzle_range_c;
    if low > high || low < bounds::NOZZLE_C.0 || high > bounds::NOZZLE_C.1 {
        return Err(bad(family, "the nozzle range lies outside the hard range"));
    }
    // 90 minus the overhang is a support threshold, and must be one the field can hold.
    in_range(family, Field::SupportsThreshold, 90 - card.max_overhang_deg)?;
    match (card.outer_wall_cap_mm_s, card.outer_wall_cap_mark) {
        (Some(cap), Some(_)) => in_range(family, Field::OuterWallSpeedCap, cap)?,
        (None, None) => {}
        _ => {
            return Err(bad(
                family,
                "the outer wall cap needs its mark, and the mark its cap",
            ));
        }
    }
    for grade in &card.grade {
        bounds::check(Field::MaxFlowCap, &Value::decimal(grade.max_flow_cap_mm3_s))
            .map_err(|error| bad(family, &error.to_string()))?;
    }
    if !card.flexible && !card.grade.is_empty() {
        return Err(bad(family, "only a flexible family has hardness grades"));
    }

    if card.plate.len() != CARD_PLATES.len() {
        return Err(bad(
            family,
            "it must answer for exactly the four plate types",
        ));
    }
    for plate in CARD_PLATES {
        let on = card
            .on(plate)
            .ok_or_else(|| bad(family, "a plate type is missing"))?;
        let Some(bed) = on.bed_values() else {
            if on.status == PlateStatus::Refused {
                continue;
            }
            return Err(bad(
                family,
                "a plate that is not refused needs glue, bed, bed_raised and window",
            ));
        };
        let Kind::Whole { min, max } = bounds::spec(Field::FirstLayerBed).kind else {
            return Err(bad(family, "the bed temperature has no whole range"));
        };
        let [low, high] = bed.window;
        let (family_low, family_high) = bounds::bed_range_c(family);
        if low > high || low < min.max(family_low) || high > max.min(family_high) {
            return Err(bad(
                family,
                "a bed window lies outside the constant bed range of the family",
            ));
        }
        for pair in [bed.normal, bed.raised] {
            if pair[1] > pair[0] || pair.iter().any(|value| !(low..=high).contains(value)) {
                return Err(bad(
                    family,
                    "a bed pair lies outside its window, or rises after the first layer",
                ));
            }
        }
        if bed.raised[0] < bed.normal[0] || bed.raised[1] < bed.normal[1] {
            return Err(bad(family, "a raised bed pair is below the level 0 pair"));
        }
    }
    Ok(())
}

impl Cards {
    /// Reads and checks the cards.
    pub fn from_toml(text: &str) -> Result<Self, DataError> {
        let file: File =
            toml::from_str(text).map_err(|error| DataError::new(FILE, error.to_string()))?;
        check_revision(FILE, file.schema, &file.revision)?;
        for family in Family::ALL {
            if file
                .card
                .iter()
                .filter(|card| card.family == *family)
                .count()
                != 1
            {
                return Err(bad(*family, "there must be exactly one card per family"));
            }
        }
        for card in &file.card {
            check_card(card)?;
        }
        Ok(Self {
            revision: file.revision,
            cards: file.card,
        })
    }

    /// The revision of the file.
    pub fn revision(&self) -> &str {
        &self.revision
    }

    /// The card of a family.
    pub fn card(&self, family: Family) -> Option<&Card> {
        self.cards.iter().find(|card| card.family == family)
    }

    /// Every card, in the order of the file.
    pub fn all(&self) -> &[Card] {
        &self.cards
    }
}
