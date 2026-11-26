//! What a plan is computed from, besides the plate facts: the person's answers about the
//! printer, what the core derived from the printer data, the print history, the fit
//! calibration, and what was asked for.
//!
//! Every type here is closed: tokens, numbers inside stated bounds, and ids with a fixed
//! pattern. The crate reads no printer data itself; the core hands over what it derived.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::bounds::{self, Kind};
use crate::facts::PlateFacts;
use crate::model::{
    BrimKind, Colour, Day, Family, Field, Intent, MaterialId, PlateType, ProcessPreset, Source,
    SpoolSlot, Value,
};
use crate::propose::Changes;
use crate::signature::FitSignature;

/// An input that is not usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InputError {
    /// Two spools were declared for one slot.
    #[error("two spools are declared for the same slot; declare one spool per slot")]
    DuplicateSlot,
    /// More spools than a printer has slots.
    #[error("more spools are declared than the printer has slots; declare at most 18")]
    TooManySpools,
    /// A number of the printer data is outside what this crate accepts.
    #[error(
        "the printer data hold a value Hotend does not accept for `{0}`; correct the printer profile"
    )]
    Printer(&'static str),
    /// A level outside 0 to 3.
    #[error("an adhesion level must lie between 0 and 3")]
    Level,
    /// A token that is not in the list of its field.
    #[error("`{0}` has no such value")]
    Token(Field),
    /// A value of a proven baseline that Hotend does not accept.
    #[error("a proven baseline cannot hold this `{0}`: {1}")]
    Baseline(Field, &'static str),
}

/// A declared spool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Spool {
    /// Where it sits.
    pub slot: SpoolSlot,
    /// Its material.
    pub material_id: MaterialId,
    /// The material's family.
    pub family: Family,
    /// Its colour.
    pub colour: Colour,
    /// Whether it is a silk filament, which sticks less well.
    pub silk: bool,
}

/// The person's answers about the printer: which plate is on the bed and what is loaded.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct Setup {
    plate_type: Option<PlateType>,
    spools: Vec<Spool>,
}

impl Setup {
    /// The most spools: sixteen AMS slots and the two outside holders.
    pub const MAX_SPOOLS: usize = 18;

    /// The answers. One spool per slot.
    pub fn new(plate_type: Option<PlateType>, spools: Vec<Spool>) -> Result<Self, InputError> {
        if spools.len() > Self::MAX_SPOOLS {
            return Err(InputError::TooManySpools);
        }
        let slots: BTreeSet<SpoolSlot> = spools.iter().map(|spool| spool.slot).collect();
        if slots.len() != spools.len() {
            return Err(InputError::DuplicateSlot);
        }
        Ok(Self { plate_type, spools })
    }

    /// The plate on the bed, when the person has answered.
    pub fn plate_type(&self) -> Option<PlateType> {
        self.plate_type
    }

    /// The declared spools.
    pub fn spools(&self) -> &[Spool] {
        &self.spools
    }
}

/// Which nozzle a slot feeds, as the core derived it from the printer data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SlotFeed {
    /// The slot.
    pub slot: SpoolSlot,
    /// The device extruder id of the nozzle it feeds.
    pub nozzle_id: u8,
    /// Whether that is the main, direct-drive nozzle.
    pub main: bool,
}

/// The facts of the printer the rules need.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PrinterFacts {
    nozzle_diameter_mm: f64,
    max_layer_height_mm: f64,
    bed_limit_c: i64,
    feeds: Vec<SlotFeed>,
}

impl PrinterFacts {
    /// `bed_limit_c` is the lower of the printer's and the filament's highest bed
    /// temperature. `feeds` lists the slots the printer data know.
    pub fn new(
        nozzle_diameter_mm: f64,
        max_layer_height_mm: f64,
        bed_limit_c: i64,
        feeds: Vec<SlotFeed>,
    ) -> Result<Self, InputError> {
        if !(nozzle_diameter_mm.is_finite() && (0.2..=1.0).contains(&nozzle_diameter_mm)) {
            return Err(InputError::Printer("nozzle diameter"));
        }
        if !(max_layer_height_mm.is_finite() && (0.08..=0.8).contains(&max_layer_height_mm)) {
            return Err(InputError::Printer("maximum layer height"));
        }
        if !(bounds::BED_C.0..=bounds::BED_C.1).contains(&bed_limit_c) {
            return Err(InputError::Printer("bed temperature limit"));
        }
        let ids = bounds::NOZZLE_ID.0..=bounds::NOZZLE_ID.1;
        if feeds.len() > Setup::MAX_SPOOLS
            || feeds
                .iter()
                .any(|feed| !ids.contains(&i64::from(feed.nozzle_id)))
        {
            return Err(InputError::Printer("nozzle id"));
        }
        Ok(Self {
            nozzle_diameter_mm,
            max_layer_height_mm,
            bed_limit_c,
            feeds,
        })
    }

    /// The nozzle diameter in mm.
    pub fn nozzle_diameter_mm(&self) -> f64 {
        self.nozzle_diameter_mm
    }

    /// The nozzle's maximum layer height in mm.
    pub fn max_layer_height_mm(&self) -> f64 {
        self.max_layer_height_mm
    }

    /// The highest bed temperature the printer and the filament allow.
    pub fn bed_limit_c(&self) -> i64 {
        self.bed_limit_c
    }

    /// The nozzle a slot feeds, when the printer data know the slot.
    pub fn feed(&self, slot: SpoolSlot) -> Option<SlotFeed> {
        self.feeds.iter().copied().find(|feed| feed.slot == slot)
    }
}

/// What earlier prints on this printer, plate and material reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct History {
    min_level: u8,
    proven_level: Option<u8>,
}

impl History {
    /// `min_level` is a floor for the level; `proven_level` the highest level a print
    /// reported as stuck.
    pub fn new(min_level: u8, proven_level: Option<u8>) -> Result<Self, InputError> {
        if min_level > 3 || proven_level.is_some_and(|level| level > 3) {
            return Err(InputError::Level);
        }
        Ok(Self {
            min_level,
            proven_level,
        })
    }

    /// The floor for the level.
    pub fn min_level(&self) -> u8 {
        self.min_level
    }

    /// The highest level a print reported as stuck.
    pub fn proven_level(&self) -> Option<u8> {
        self.proven_level
    }
}

/// The sticking settings of a print the person reported as good, as plain values. Every
/// value is a Hotend field in its hard range; [`Baseline::new`] checks them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct BaselineValues {
    /// `adhesion.level`, 0 to 2 (level 3 comes from print history only).
    pub level: u8,
    /// `adhesion.brim_kind`: `none` or `outer`, the kinds a print has been proven with.
    pub brim_kind: BrimKind,
    /// `adhesion.brim_width_mm`: more than 0 exactly when the kind draws a brim.
    pub brim_width_mm: i64,
    /// `first_layer.wall_speed_mm_s`.
    pub first_layer_wall_speed_mm_s: i64,
    /// `first_layer.infill_speed_mm_s`.
    pub first_layer_infill_speed_mm_s: i64,
    /// `first_layer.bed_c`.
    pub first_layer_bed_c: i64,
    /// `layers.bed_c`, never above `first_layer.bed_c`.
    pub layers_bed_c: i64,
    /// `cooling.part_fan_off_layers`.
    pub part_fan_off_layers: i64,
    /// `cooling.aux_fan_first_layers_pct`.
    pub aux_fan_first_layers_pct: i64,
    /// `cooling.aux_fan_max_pct`.
    pub aux_fan_max_pct: i64,
    /// `shell.wall_loops`.
    pub wall_loops: i64,
}

/// A proven baseline: the sticking settings of a print on this printer, plate and material
/// that the person reported as good. It is the floor of the sticking group: the rules may
/// make a plan more careful than the baseline, never less careful. The core keeps one per
/// printer, plate and material and hands over the one of the plate's printer; this crate
/// checks that its plate and material are the plate's own and ignores it otherwise.
///
/// It holds no text: the person's label of the print stays with the core and is shown
/// beside the plan, never inside a sentence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Baseline {
    plate: PlateType,
    material: MaterialId,
    printed_on: Day,
    values: BaselineValues,
}

impl Baseline {
    /// The fields a baseline holds, in the order of [`Field::ALL`].
    pub const FIELDS: &'static [Field] = &[
        Field::Level,
        Field::BrimKind,
        Field::BrimWidth,
        Field::FirstLayerWallSpeed,
        Field::FirstLayerInfillSpeed,
        Field::FirstLayerBed,
        Field::LayersBed,
        Field::PartFanOffLayers,
        Field::AuxFanFirstLayers,
        Field::AuxFanMax,
        Field::WallLoops,
    ];

    /// The highest level a baseline may hold: level 3 is "ask", which only print history
    /// gives.
    pub const MAX_LEVEL: u8 = 2;

    /// A baseline. Every value must lie in its field's hard range, the brim kind must be
    /// `none` or `outer` and agree with the width, and the bed after the first layer must
    /// not be hotter than the first layer.
    pub fn new(
        plate: PlateType,
        material: MaterialId,
        printed_on: Day,
        values: BaselineValues,
    ) -> Result<Self, InputError> {
        if values.level > Self::MAX_LEVEL {
            return Err(InputError::Baseline(
                Field::Level,
                "a proven level is 0, 1 or 2",
            ));
        }
        let baseline = Self {
            plate,
            material,
            printed_on,
            values,
        };
        for field in Self::FIELDS {
            let value = baseline.get(*field).ok_or(InputError::Baseline(
                *field,
                "the baseline does not hold it",
            ))?;
            bounds::check(*field, &value).map_err(|_| {
                InputError::Baseline(*field, "the value lies outside the field's hard range")
            })?;
        }
        match (values.brim_kind, values.brim_width_mm > 0) {
            (BrimKind::Outer, true) | (BrimKind::None, false) => {}
            (BrimKind::Outer | BrimKind::None, _) => {
                return Err(InputError::Baseline(
                    Field::BrimWidth,
                    "an outer brim has a width above 0 and no brim has width 0",
                ));
            }
            _ => {
                return Err(InputError::Baseline(
                    Field::BrimKind,
                    "only `none` and `outer` have been proven",
                ));
            }
        }
        if values.layers_bed_c > values.first_layer_bed_c {
            return Err(InputError::Baseline(
                Field::LayersBed,
                "the bed after the first layer is never hotter than during it",
            ));
        }
        Ok(baseline)
    }

    /// The plate it was proven on.
    pub fn plate(&self) -> PlateType {
        self.plate
    }

    /// The material it was proven with.
    pub fn material(&self) -> &MaterialId {
        &self.material
    }

    /// The day of the proven print.
    pub fn printed_on(&self) -> Day {
        self.printed_on
    }

    /// The values.
    pub fn values(&self) -> BaselineValues {
        self.values
    }

    /// Whether it applies to a plate of `material` on `plate`.
    pub fn applies(&self, plate: Option<PlateType>, material: &MaterialId) -> bool {
        plate == Some(self.plate) && *material == self.material
    }

    /// The baseline's value of `field`, `None` for a field it does not hold.
    pub fn get(&self, field: Field) -> Option<Value> {
        let v = &self.values;
        Some(match field {
            Field::Level => Value::Whole(i64::from(v.level)),
            Field::BrimKind => Value::Token(v.brim_kind.as_str()),
            Field::BrimWidth => Value::Whole(v.brim_width_mm),
            Field::FirstLayerWallSpeed => Value::Whole(v.first_layer_wall_speed_mm_s),
            Field::FirstLayerInfillSpeed => Value::Whole(v.first_layer_infill_speed_mm_s),
            Field::FirstLayerBed => Value::Whole(v.first_layer_bed_c),
            Field::LayersBed => Value::Whole(v.layers_bed_c),
            Field::PartFanOffLayers => Value::Whole(v.part_fan_off_layers),
            Field::AuxFanFirstLayers => Value::Whole(v.aux_fan_first_layers_pct),
            Field::AuxFanMax => Value::Whole(v.aux_fan_max_pct),
            Field::WallLoops => Value::Whole(v.wall_loops),
            _ => return None,
        })
    }
}

/// What the slicer side can do, in Hotend's own words: the process presets that are
/// installed, and per enum field the values whose profile token was verified by a real run.
/// This crate never sees a key or a token of a slicer.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct Capabilities {
    presets: BTreeSet<ProcessPreset>,
    values: BTreeMap<Field, BTreeSet<&'static str>>,
}

impl Capabilities {
    /// Nothing installed and nothing verified.
    pub fn none() -> Self {
        Self::default()
    }

    /// Adds an installed process preset.
    pub fn with_preset(mut self, preset: ProcessPreset) -> Self {
        self.presets.insert(preset);
        self
    }

    /// Adds a verified value of an enum field. The value must be one of the field's tokens.
    pub fn with_value(mut self, field: Field, value: &str) -> Result<Self, InputError> {
        let Kind::Tokens(tokens) = bounds::spec(field).kind else {
            return Err(InputError::Token(field));
        };
        let token = tokens
            .iter()
            .find(|token| **token == value)
            .ok_or(InputError::Token(field))?;
        self.values.entry(field).or_default().insert(token);
        Ok(self)
    }

    /// Whether the preset is installed.
    pub fn has_preset(&self, preset: ProcessPreset) -> bool {
        self.presets.contains(&preset)
    }

    /// Whether the value of the field is verified.
    pub fn verified(&self, field: Field, value: &str) -> bool {
        self.values
            .get(&field)
            .is_some_and(|values| values.contains(value))
    }
}

/// Who asked for a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Asker {
    /// The assistant.
    Agent,
    /// The person.
    Person,
}

impl Asker {
    /// The decision source of this asker.
    pub fn source(self) -> Source {
        match self {
            Asker::Agent => Source::Agent,
            Asker::Person => Source::Person,
        }
    }
}

/// A value somebody asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Asked<T> {
    /// The value.
    pub value: T,
    /// Who asked.
    pub by: Asker,
}

/// Everything a plan is a function of.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlanInputs {
    /// The checked plate facts.
    pub facts: PlateFacts,
    /// The material of the plate. Its family is the family of the facts.
    pub material: MaterialId,
    /// The person's answers about the printer.
    pub setup: Setup,
    /// The person's choice when more than one spool holds the material.
    pub spool_choice: Option<SpoolSlot>,
    /// What the core derived from the printer data.
    pub printer: PrinterFacts,
    /// The print history for this printer, plate and material.
    pub history: History,
    /// The fit signature of the newest gauge for this printer, material and nozzle.
    pub calibration: Option<FitSignature>,
    /// Whether the plate is a fit gauge. A gauge is always `fit_critical`.
    pub gauge: bool,
    /// The intent that was asked for. The two fields below come first when a plan is
    /// recomputed with accepted changes, because other fields take their defaults from them.
    pub intent: Option<Asked<Intent>>,
    /// The level that was asked for. It can only raise the rules' level, to 2 at most.
    pub level: Option<Asked<u8>>,
    /// The accepted changes to the other fields, the newest per field. Each is judged
    /// against the envelope of its field whenever the plan is computed.
    pub changes: Changes,
    /// What the slicer side can do.
    pub capabilities: Capabilities,
    /// The proven baseline of this printer, plate and material, when the person recorded
    /// one. It is the floor of the sticking group and of the wall loops; it is ignored when
    /// its plate or material is not the plate's own. `None`: the rules decide alone.
    pub baseline: Option<Baseline>,
}

impl PlanInputs {
    /// The baseline when it applies to this plate.
    pub fn applicable_baseline(&self) -> Option<&Baseline> {
        self.baseline
            .as_ref()
            .filter(|baseline| baseline.applies(self.setup.plate_type(), &self.material))
    }

    /// The same plate with nothing asked for: no level, no intent, no change. Its plan is
    /// the rule plan.
    pub fn reset(&self) -> Self {
        let mut inputs = self.clone();
        inputs.level = None;
        inputs.intent = None;
        inputs.changes = Changes::none();
        inputs
    }
}
