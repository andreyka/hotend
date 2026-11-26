//! Hotend's print settings for a prepared plate: the plan of `hotend-settings`, computed
//! from the plate's measured facts and the person's settings (docs/PRINT_SETTINGS.md,
//! "What the core does with it").
//!
//! Guarantees:
//! - A plan is a pure function of what [`PlanBasis`] holds: the plate facts the engine
//!   measured, the material, the plate type, the one spool, the fitted nozzle size, the
//!   printer data and the proven baseline of this printer, plate and material when the
//!   person recorded one. Nothing else (no clock, no model output) goes into it.
//! - The stored plan (`versions/<n>/prepare/plan/<plate>.json`) is a record. It is never
//!   read back as settings: the core computes the plan again from the basis and compares
//!   the SHA-256; a record that the current settings no longer give is replaced.
//! - Facts are read strictly by `PlateFacts::from_value`; facts that are missing, out of
//!   bounds or do not match the plate's pieces give the blocked plan `facts_invalid`.
//! - A blocked plan carries no settings. It is shown with its code and sentence, and a real
//!   slice of that plate is refused with the same code.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use hotend_settings::facts::PlateFacts;
use hotend_settings::inputs::{
    Capabilities, History, PlanInputs, PrinterFacts, Setup, SlotFeed, Spool,
};
use hotend_settings::model::{Colour, Field, MaterialId, PlateType, SpoolSlot, Value as Setting};
use hotend_settings::{Changes, PrintPlan, RuleBook};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::baseline::{BaselineStore, ProvenBaseline};
use super::{Modeler, ModelingError, Preparation};
use crate::engine::Keepout;
use crate::jobs::JobScope;
use crate::profiles::DEFAULT_NOZZLE_DIAMETER;
use crate::settings::Settings;

/// The margin to the edge of the reach for a plate that is printed.
pub const PRINT_MARGIN_MM: f64 = 5.0;
/// The folder below `prepare/` that holds the plan records.
pub(crate) const PLAN_DIR: &str = "plan";
/// The layout version of a [`PlanRecord`].
pub const PLAN_RECORD_SCHEMA: u32 = 1;
const MAX_PLAN_RECORD_BYTES: u64 = 16 * 1024 * 1024;
/// The bed limit used for a printer whose profile states none (a printer Hotend slices
/// for but does not drive). A judgment: below every machine limit seen.
const UNKNOWN_BED_LIMIT_C: f64 = 110.0;
/// The largest layer height of a nozzle, as a share of its diameter. Measured for the
/// 0.4 mm nozzle (0.28 mm); the same share for the other sizes is a judgment.
const MAX_LAYER_SHARE: f64 = 0.7;

/// Why a basis gives no inputs.
enum BasisProblem {
    /// The facts are refused: the plan is `facts_invalid`.
    Facts,
    /// A value of the settings or the printer data is not usable.
    Input(String),
}

/// One slot the printer data know, and the nozzle it feeds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedRecord {
    pub slot: String,
    pub nozzle_id: u8,
    pub main: bool,
}

/// Everything a plan is computed from, as stored with it. Print history, the fit
/// calibration, an intent and accepted changes are not inputs yet: none exist in this
/// version (the plan says so in its assumptions). The proven baseline is one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanBasis {
    /// The plate facts as the engine sent them (`plates[].facts`).
    pub facts: Value,
    /// How many pieces the plate holds; the facts must name exactly as many.
    pub pieces: usize,
    pub material_id: String,
    /// Hotend's id of the plate on the bed; `None` until the person says.
    pub plate_type: Option<String>,
    pub spool_slot: String,
    pub spool_colour: String,
    /// Whether the person set the colour (otherwise it is assumed).
    pub spool_colour_set: bool,
    pub spool_silk: bool,
    pub nozzle_diameter_mm: f64,
    /// Whether the person set the nozzle size (otherwise 0.4 mm is assumed).
    pub nozzle_size_set: bool,
    pub printer_id: String,
    pub max_layer_height_mm: f64,
    pub bed_limit_c: i64,
    pub feeds: Vec<FeedRecord>,
    /// The proven baseline of this printer, plate and material; absent when none is
    /// recorded (and then left out of the record, as before baselines existed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<ProvenBaseline>,
}

impl PlanBasis {
    /// The basis of a plate's facts under the current settings.
    pub fn new(settings: &Settings, facts: Value, pieces: usize) -> Self {
        let nozzle_diameter_mm = settings
            .nozzle_diameter_mm
            .unwrap_or(DEFAULT_NOZZLE_DIAMETER);
        let (bed_limit_c, feeds) = printer_data(settings);
        Self {
            facts,
            pieces,
            material_id: settings.material.id.clone(),
            plate_type: settings.plate_type.clone(),
            spool_slot: settings.spool.slot.clone(),
            spool_colour: settings.spool.colour_or_assumed().to_owned(),
            spool_colour_set: settings.spool.colour.is_some(),
            spool_silk: settings.spool.silk,
            nozzle_diameter_mm,
            nozzle_size_set: settings.nozzle_diameter_mm.is_some(),
            printer_id: settings.printer_id.clone(),
            max_layer_height_mm: ((nozzle_diameter_mm * MAX_LAYER_SHARE) * 100.0).round() / 100.0,
            bed_limit_c,
            feeds,
            baseline: None,
        }
    }

    /// The same basis with the proven baseline of its printer, plate and material.
    pub fn with_baseline(mut self, baseline: Option<ProvenBaseline>) -> Self {
        self.baseline = baseline;
        self
    }

    /// The inputs of the rules. The facts are read strictly here.
    fn inputs(&self, plate_type: Option<PlateType>) -> Result<PlanInputs, BasisProblem> {
        let facts = PlateFacts::from_value(self.facts.clone()).map_err(|_| BasisProblem::Facts)?;
        if facts.pieces().len() != self.pieces {
            return Err(BasisProblem::Facts);
        }
        let input = |what: &str| BasisProblem::Input(format!("the {what} cannot be planned with"));
        let material = MaterialId::parse(&self.material_id).ok_or_else(|| input("material id"))?;
        let slot = SpoolSlot::parse(&self.spool_slot).ok_or_else(|| input("spool slot"))?;
        let colour = Colour::parse(&self.spool_colour).ok_or_else(|| input("spool colour"))?;
        let spool = Spool {
            slot,
            material_id: material.clone(),
            family: facts.family(),
            colour,
            silk: self.spool_silk,
        };
        let setup = Setup::new(plate_type, vec![spool])
            .map_err(|error| BasisProblem::Input(error.to_string()))?;
        let mut feeds = Vec::new();
        for feed in &self.feeds {
            feeds.push(SlotFeed {
                slot: SpoolSlot::parse(&feed.slot).ok_or_else(|| input("printer's slot list"))?,
                nozzle_id: feed.nozzle_id,
                main: feed.main,
            });
        }
        let printer = PrinterFacts::new(
            self.nozzle_diameter_mm,
            self.max_layer_height_mm,
            self.bed_limit_c,
            feeds,
        )
        .map_err(|error| BasisProblem::Input(error.to_string()))?;
        let baseline = match &self.baseline {
            Some(entry) => Some(
                entry
                    .to_input()
                    .map_err(|error| BasisProblem::Input(error.to_string()))?,
            ),
            None => None,
        };
        Ok(PlanInputs {
            facts,
            material,
            setup,
            spool_choice: None,
            printer,
            history: History::default(),
            calibration: None,
            gauge: false,
            intent: None,
            level: None,
            changes: Changes::none(),
            // The core knows no slicer: no optional token is offered as verified. The rules
            // then keep the verified defaults (outer brim, grid, aligned seam, tree support).
            capabilities: Capabilities::none(),
            baseline,
        })
    }

    /// The plate type of the settings, when it is one of the plan's tokens. Another id is
    /// treated as not chosen: the plan then asks which plate is on the bed.
    fn plate_type(&self) -> Option<PlateType> {
        self.plate_type.as_deref().and_then(PlateType::from_token)
    }

    /// Computes the plan and its hash.
    pub fn plan(&self) -> Result<ComputedPlan, ModelingError> {
        self.plan_with(self.plate_type())
    }

    fn plan_with(&self, plate_type: Option<PlateType>) -> Result<ComputedPlan, ModelingError> {
        let book = RuleBook::builtin().map_err(defect)?;
        let plan = match self.inputs(plate_type) {
            Ok(inputs) => book.plan(&inputs).map_err(defect)?,
            Err(BasisProblem::Facts) => book.refused_facts().map_err(defect)?,
            Err(BasisProblem::Input(message)) => return Err(ModelingError::Invalid(message)),
        };
        let sha256 = plan.sha256().map_err(defect)?;
        Ok(ComputedPlan { plan, sha256 })
    }

    /// The room every piece needs round it for its brim: the widest brim the rules give the
    /// pieces of these facts, plus 1 mm; 0 without a brim. While the plan is blocked by a
    /// question the brim rules (which read only the geometry and the material) are run with
    /// each plate type in turn; `None` when none gives a plan.
    pub fn clearance_mm(&self) -> Result<Option<f64>, ModelingError> {
        let mut tried = vec![self.plate_type()];
        tried.extend(
            [
                PlateType::TexturedPei,
                PlateType::HighTemp,
                PlateType::Cool,
                PlateType::Engineering,
            ]
            .map(Some),
        );
        for plate_type in tried {
            let computed = self.plan_with(plate_type)?;
            if computed.plan.blocked.is_none() {
                return Ok(Some(required_clearance(&computed.plan)));
            }
            if computed
                .plan
                .blocked
                .as_ref()
                .is_some_and(|blocked| blocked.code == "facts_invalid")
            {
                return Ok(None);
            }
        }
        Ok(None)
    }
}

fn defect(error: impl std::fmt::Display) -> ModelingError {
    ModelingError::Invalid(format!(
        "Hotend's print settings could not be worked out ({error}); this is a fault of \
         Hotend, not of the model"
    ))
}

/// The room a plan's brim needs round every piece: its width plus 1 mm, 0 without one.
pub fn required_clearance(plan: &PrintPlan) -> f64 {
    let kind = plan.value(Field::BrimKind).and_then(|value| value.token());
    let width = plan
        .value(Field::BrimWidth)
        .and_then(|value| value.whole())
        .unwrap_or(0);
    if kind.is_none_or(|kind| kind == "none") || width <= 0 {
        0.0
    } else {
        width as f64 + 1.0
    }
}

/// The printer data the plan needs: the bed limit (the lowest of the plate's, the
/// machine's and the filament family's) and the slots with the nozzle each feeds. The AMS
/// slots feed the main nozzle in version 1 (rule NOZ-1); the outside holders feed the
/// nozzle the printer data name for them.
fn printer_data(settings: &Settings) -> (i64, Vec<FeedRecord>) {
    let mut feeds = Vec::new();
    let Some(layer) = crate::profiles::printer_layer_profile(&settings.printer_id) else {
        if let Some(profile) = crate::profiles::printer_profile(&settings.printer_id)
            && let Some(main) = profile.main_nozzle()
            && let Ok(id) = u8::try_from(main.id)
        {
            feeds.push(FeedRecord {
                slot: SpoolSlot::ExternalMain.to_string(),
                nozzle_id: id,
                main: true,
            });
        }
        return (UNKNOWN_BED_LIMIT_C as i64, feeds);
    };
    let mut limit = layer.limits().bed_max_c;
    if let Some(plate) = settings
        .plate_type
        .as_deref()
        .and_then(|id| layer.plate_by_id(id))
    {
        limit = limit.min(plate.bed_max_c);
    }
    if let Some(filament) = layer.filament_limits(&settings.material.family) {
        limit = limit.min(filament.bed_max_c);
    }
    let main = layer
        .nozzles()
        .iter()
        .find(|nozzle| nozzle.role == "main")
        .or(layer.nozzles().first());
    if let Some(main) = main {
        for number in 1..=SpoolSlot::MAX_AMS {
            feeds.push(FeedRecord {
                slot: number.to_string(),
                nozzle_id: main.device_extruder,
                main: true,
            });
        }
    }
    for nozzle in layer.nozzles() {
        if nozzle.external_tray.is_none() {
            continue;
        }
        let slot = match nozzle.role.as_str() {
            "main" => SpoolSlot::ExternalMain,
            "auxiliary" => SpoolSlot::ExternalAuxiliary,
            _ => continue,
        };
        feeds.push(FeedRecord {
            slot: slot.to_string(),
            nozzle_id: nozzle.device_extruder,
            main: nozzle.role == "main",
        });
    }
    (limit.floor() as i64, feeds)
}

/// The keep-out zones of the printer in the settings, as the engine takes them.
pub fn keepouts(printer_id: &str) -> Vec<Keepout> {
    crate::profiles::printer_layer_profile(printer_id)
        .map(|profile| {
            profile
                .keepouts()
                .iter()
                .map(|zone| Keepout {
                    id: zone.id.clone(),
                    min: [zone.x[0], zone.y[0]],
                    max: [zone.x[1], zone.y[1]],
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A plan with its hash.
#[derive(Debug, Clone)]
pub struct ComputedPlan {
    pub plan: PrintPlan,
    pub sha256: String,
}

/// The stored plan of one plate: `versions/<n>/prepare/plan/<plate>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanRecord {
    pub schema: u32,
    pub project_id: String,
    pub version: u32,
    pub plate: u32,
    /// SHA-256 of the plan's canonical JSON.
    pub sha256: String,
    pub basis: PlanBasis,
    /// The plan as `hotend-settings` serializes it: settings values, decisions with their
    /// sentences, the checklist and the data revisions. A record only.
    pub plan: Value,
    pub created_at_ms: i64,
}

impl PlanRecord {
    fn new(
        project_id: &str,
        version: u32,
        plate: u32,
        basis: PlanBasis,
        computed: &ComputedPlan,
    ) -> Self {
        Self {
            schema: PLAN_RECORD_SCHEMA,
            project_id: project_id.to_owned(),
            version,
            plate,
            sha256: computed.sha256.clone(),
            basis,
            plan: serde_json::to_value(&computed.plan).unwrap_or(Value::Null),
            created_at_ms: crate::util::now_ms(),
        }
    }
}

/// The path of a plate's plan record inside a preparation folder.
pub(crate) fn record_path(prepare_dir: &Path, plate: u32) -> PathBuf {
    prepare_dir.join(PLAN_DIR).join(format!("{plate}.json"))
}

/// Writes a record next to its final place and renames it there.
pub(crate) fn write_record(path: &Path, record: &PlanRecord) -> Result<(), ModelingError> {
    let io = |source: std::io::Error| ModelingError::Io {
        path: path.to_path_buf(),
        source,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io)?;
    }
    let bytes = serde_json::to_vec_pretty(record).map_err(|e| io(std::io::Error::other(e)))?;
    let temporary = path.with_extension("json.new");
    std::fs::write(&temporary, bytes).map_err(io)?;
    std::fs::rename(&temporary, path).map_err(io)
}

/// The stored record, when it is a plain file of a sane size that parses.
fn read_record(path: &Path) -> Option<PlanRecord> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_PLAN_RECORD_BYTES {
        return None;
    }
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// The plan of one plate of a preparation under the current settings, with what it was
/// computed from.
#[derive(Debug, Clone)]
pub struct PlatePlan {
    pub plate: u32,
    pub computed: ComputedPlan,
    pub basis: PlanBasis,
}

impl PlatePlan {
    /// The plan of `plate` of `preparation` under `settings`, with the proven baseline of
    /// the settings' printer, plate and material (see [`Modeler::baseline_for`]). The facts
    /// are the ones the engine stated for that plate; a preparation without them (made
    /// before the print settings) gives `facts_invalid`.
    pub fn of(
        preparation: &Preparation,
        plate: u32,
        settings: &Settings,
        baseline: Option<ProvenBaseline>,
    ) -> Result<Self, ModelingError> {
        let layout = preparation
            .plates
            .iter()
            .find(|candidate| candidate.index == plate);
        let (facts, pieces) = layout.map_or((Value::Null, 0), |layout| {
            (
                layout.facts.clone().unwrap_or(Value::Null),
                layout.items.len(),
            )
        });
        let basis = PlanBasis::new(settings, facts, pieces).with_baseline(baseline);
        let computed = basis.plan()?;
        Ok(Self {
            plate,
            computed,
            basis,
        })
    }

    /// The plan's code and sentence when it is blocked.
    pub fn blocked(&self) -> Option<PlanNotice> {
        self.computed
            .plan
            .blocked
            .as_ref()
            .map(|notice| PlanNotice {
                code: notice.code.to_owned(),
                sentence: notice.sentence.clone(),
            })
    }

    /// Makes sure the stored record is the plan of the current settings: a record that
    /// another basis gave, or that its own basis no longer reproduces (other rule data), is
    /// replaced. Returns whether the record was written.
    pub fn store(&self, preparation: &Preparation) -> Result<bool, ModelingError> {
        let path = record_path(&preparation.dir, self.plate);
        if let Some(record) = read_record(&path)
            && record.sha256 == self.computed.sha256
            && record.basis == self.basis
            && record.project_id == preparation.project_id
            && record.version == preparation.version
            && record.plate == self.plate
        {
            return Ok(false);
        }
        let record = PlanRecord::new(
            &preparation.project_id,
            preparation.version,
            self.plate,
            self.basis.clone(),
            &self.computed,
        );
        write_record(&path, &record)?;
        Ok(true)
    }

    /// The plan as the window and the command line show it.
    pub fn view(&self, preparation: &Preparation) -> PrintPlanView {
        PrintPlanView::new(preparation, self)
    }
}

/// Recomputes a stored record from its own basis and says whether it gives the hash it
/// states: `Some(true)` when it does, `Some(false)` when not (the rule data changed, or the
/// file was altered), `None` when there is no readable record.
pub fn verify_record(prepare_dir: &Path, plate: u32) -> Result<Option<bool>, ModelingError> {
    let Some(record) = read_record(&record_path(prepare_dir, plate)) else {
        return Ok(None);
    };
    Ok(Some(record.basis.plan()?.sha256 == record.sha256))
}

impl Modeler {
    /// The preparation of a version that a plan and a slice can be made from: the stored
    /// one when it was made for the printer, the material and the keep-out zones in the
    /// settings, in two passes; otherwise the version is laid out again with the options
    /// of the earlier preparation.
    pub async fn prepared_for_print(
        &self,
        scope: &JobScope,
        project_id: &str,
        number: u32,
    ) -> Result<Preparation, ModelingError> {
        let settings = self.settings()?;
        match self.preparation(project_id, number)? {
            Some(found) if found.is_current_for(&settings) => Ok(found),
            stale => {
                let options = stale.map(|found| found.options).unwrap_or_default();
                self.prepare(scope, project_id, Some(number), options).await
            }
        }
    }

    /// The proven baselines of this data folder.
    pub fn baselines(&self) -> BaselineStore {
        BaselineStore::new(self.data.root())
    }

    /// The proven baseline of the printer, plate and material in `settings`; `None` while no
    /// plate is chosen or none is recorded. A baseline file that cannot be checked is an
    /// error: a plan is not made less careful because the file broke.
    pub fn baseline_for(
        &self,
        settings: &Settings,
    ) -> Result<Option<ProvenBaseline>, ModelingError> {
        let Some(plate) = settings.plate_type.as_deref() else {
            return Ok(None);
        };
        self.baselines()
            .find(&settings.printer_id, plate, &settings.material.id)
    }

    /// The plan of one plate of a preparation under the current settings and proven
    /// baseline, without touching the stored record.
    pub fn current_plate_plan(
        &self,
        preparation: &Preparation,
        plate: u32,
    ) -> Result<PlatePlan, ModelingError> {
        let settings = self.settings()?;
        let baseline = self.baseline_for(&settings)?;
        PlatePlan::of(preparation, plate, &settings, baseline)
    }

    /// The plan of one plate of a preparation under the current settings. The stored
    /// record is replaced when the settings, the proven baseline or the rule data give
    /// another plan.
    pub fn plate_plan(
        &self,
        preparation: &Preparation,
        plate: u32,
    ) -> Result<PlatePlan, ModelingError> {
        let plate_plan = self.current_plate_plan(preparation, plate)?;
        plate_plan.store(preparation)?;
        Ok(plate_plan)
    }

    /// The plan of a plate of a version (the current one when `version` is `None`; plate 1
    /// when `plate` is `None`), preparing the version first when needed. A plan whose brim
    /// needs more room than the layout kept lays the version out again, once.
    pub async fn print_plan(
        &self,
        scope: &JobScope,
        project_id: &str,
        version: Option<u32>,
        plate: Option<u32>,
    ) -> Result<(Preparation, PlatePlan), ModelingError> {
        let number = match version {
            Some(number) => self.store.version(project_id, number)?.number,
            None => self.current_version(project_id)?.number,
        };
        let plate = plate.unwrap_or(1);
        let mut preparation = self.prepared_for_print(scope, project_id, number).await?;
        let mut laid_out_again = false;
        loop {
            if !preparation
                .plates
                .iter()
                .any(|layout| layout.index == plate)
            {
                return Err(ModelingError::Invalid(format!(
                    "there is no plate {plate}: version {number} is laid out on {} plate(s)",
                    preparation.plates.len()
                )));
            }
            let plate_plan = self.plate_plan(&preparation, plate)?;
            let room = preparation.clearance_mm.unwrap_or(0.0);
            if laid_out_again || required_clearance(&plate_plan.computed.plan) <= room + 1e-9 {
                return Ok((preparation, plate_plan));
            }
            let options = preparation.options.clone();
            preparation = self
                .prepare(scope, project_id, Some(number), options)
                .await?;
            laid_out_again = true;
        }
    }

    /// The plan of a plate of the current version as it stands, for the agent's read-only
    /// `print_plan`: computed in memory from the stored preparation. Unlike
    /// [`Self::print_plan`] it writes no plan record and never lays the version out (either
    /// could change the plan a pending print job was made with, so that the job no longer
    /// passes review: `plan_changed`). `Ok(None)` when the version has no preparation made
    /// for the printer, material and keep-out zones of the settings.
    pub fn plan_as_it_stands(
        &self,
        project_id: &str,
        plate: Option<u32>,
    ) -> Result<Option<PlanAsItStands>, ModelingError> {
        let number = self.current_version(project_id)?.number;
        let plate = plate.unwrap_or(1);
        let settings = self.settings()?;
        let preparation = match self.preparation(project_id, number)? {
            Some(found) if found.is_current_for(&settings) => found,
            _ => return Ok(None),
        };
        if !preparation
            .plates
            .iter()
            .any(|layout| layout.index == plate)
        {
            return Err(ModelingError::Invalid(format!(
                "there is no plate {plate}: version {number} is laid out on {} plate(s)",
                preparation.plates.len()
            )));
        }
        let baseline = self.baseline_for(&settings)?;
        let plate_plan = PlatePlan::of(&preparation, plate, &settings, baseline)?;
        let room = preparation.clearance_mm.unwrap_or(0.0);
        let needed = required_clearance(&plate_plan.computed.plan);
        Ok(Some(PlanAsItStands {
            version: number,
            lays_out_again: needed > room + 1e-9,
            preparation,
            plate_plan,
        }))
    }
}

/// A plan computed without storing anything ([`Modeler::plan_as_it_stands`]).
#[derive(Debug)]
pub struct PlanAsItStands {
    pub version: u32,
    pub preparation: Preparation,
    pub plate_plan: PlatePlan,
    /// The plan's brim needs more room between pieces than the layout kept: the next
    /// preparation (a slice) lays the version out again, which can change this plan.
    pub lays_out_again: bool,
}

/// A code with its sentence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanNotice {
    pub code: String,
    pub sentence: String,
}

/// One decided field, for people.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanDecision {
    /// Hotend's field name, such as `adhesion.brim_width_mm`; never a slicer key.
    pub field: String,
    /// The value; `null` means "as the installed profile has it" (nothing is written).
    pub value: Value,
    pub unit: String,
    /// Who decided it: `rule`, `intent`, `agent`, `person`, `history`, `calibration`,
    /// `profile`.
    pub source: String,
    /// The stable rule id, such as `AD-1`.
    pub rule: String,
    /// What the number rests on: `measured`, `sourced`, `judgment`, `stock`.
    pub basis: String,
    pub sentence: String,
    /// The values a change must stay in.
    pub envelope: Value,
    /// What was asked for, when that differs from the value.
    pub asked: Option<Value>,
    /// Where the value came from, in words: "proven on your printer (<day>, <the person's
    /// label>)", "rule <id>", "your settings", "your choice", the assistant's choice, the intent, the print
    /// history, the fit calibration or the installed profile. A baseline value never reads
    /// as a choice of the person.
    #[serde(default)]
    pub origin: String,
}

/// The proven baseline a plan used, as the plan view shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanBaseline {
    pub printer_id: String,
    pub plate_type: String,
    pub material_id: String,
    /// The day of the proven print, `YYYY-MM-DD`.
    pub printed_on: String,
    /// The person's label of the print; shown, never interpreted.
    pub source: String,
    /// The fields whose value the baseline set (where the rules alone were less careful).
    pub fields: Vec<String>,
}

/// One line of the before-you-print list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanChecklistItem {
    pub id: String,
    /// The priority class, 1 (the plate) to 6 (afterwards).
    pub class: u8,
    pub text: String,
    /// When the line applies; `null` means always.
    pub condition: Option<Value>,
}

/// The plan of one plate as the window and the command line show it (`print_plan_get`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrintPlanView {
    pub project_id: String,
    pub version: u32,
    pub plate: u32,
    /// SHA-256 of the plan; a slice made with it records the same hash.
    pub sha256: String,
    /// Which settings a slice of this plate would use, in words: "Hotend's print settings,
    /// plan <first 16 hex digits>", or the installed profiles while the plan is blocked.
    pub settings_source: String,
    /// Why there are no settings, with what to do; `None` for a plan that can be sliced.
    pub blocked: Option<PlanNotice>,
    /// One decision per field, in the order of the fields; empty when blocked.
    pub decisions: Vec<PlanDecision>,
    /// What only the person can answer.
    pub questions: Vec<PlanNotice>,
    pub warnings: Vec<PlanNotice>,
    /// Remarks that are part of the plan: known limits, why supports are on.
    pub notes: Vec<PlanNotice>,
    /// What the plan takes for granted until the person says otherwise.
    pub assumptions: Vec<PlanNotice>,
    /// The before-you-print list, at most as many lines as the plan allows.
    pub checklist: Vec<PlanChecklistItem>,
    /// The plate facts the plan was computed from (`null` when they were refused).
    pub facts: Option<Value>,
    /// The room the preparation kept round every piece for its brim, mm.
    pub clearance_mm: Option<f64>,
    /// The nozzle size planned with, and whether the person set it.
    pub nozzle_diameter_mm: f64,
    pub nozzle_size_set: bool,
    /// The revisions of the rule data files.
    pub revisions: BTreeMap<String, String>,
    /// The proven baseline that is the floor of this plan; `None` when none applies.
    #[serde(default)]
    pub baseline: Option<PlanBaseline>,
}

/// The words for the settings source of a plan hash.
pub fn settings_source(sha256: Option<&str>, blocked: Option<&str>) -> String {
    match (sha256, blocked) {
        (Some(sha), None) => format!(
            "Hotend's print settings, plan {}",
            sha.get(..16).unwrap_or(sha)
        ),
        (_, Some(code)) => {
            format!("the installed profiles as they are: the print plan is blocked ({code})")
        }
        (None, None) => "the installed profiles as they are".to_owned(),
    }
}

impl PrintPlanView {
    fn new(preparation: &Preparation, plate: &PlatePlan) -> Self {
        let plan = &plate.computed.plan;
        let notice = |notice: &hotend_settings::model::Notice| PlanNotice {
            code: notice.code.to_owned(),
            sentence: notice.sentence.clone(),
        };
        let baseline = plate.basis.baseline.as_ref().filter(|_| {
            plan.derived
                .as_ref()
                .is_some_and(|derived| derived.proven_on.is_some())
        });
        let decisions = plan
            .decisions
            .iter()
            .map(|decision| PlanDecision {
                origin: origin(decision, baseline),
                field: decision.field.as_str().to_owned(),
                value: setting_json(&decision.value),
                unit: decision.unit.as_str().to_owned(),
                source: decision.source.as_str().to_owned(),
                rule: decision.rule.as_str().to_owned(),
                basis: decision.basis.as_str().to_owned(),
                sentence: decision.sentence.clone(),
                envelope: serde_json::to_value(&decision.envelope).unwrap_or(Value::Null),
                asked: decision.asked.as_ref().map(setting_json),
            })
            .collect();
        let checklist = plan
            .checklist
            .items
            .iter()
            .take(plan.checklist.limit)
            .map(|item| PlanChecklistItem {
                id: item.id.to_owned(),
                class: item.class,
                text: item.text.clone(),
                condition: item
                    .condition
                    .as_ref()
                    .and_then(|condition| serde_json::to_value(condition).ok()),
            })
            .collect();
        let blocked = plate.blocked();
        let revisions = serde_json::to_value(&plan.revisions)
            .ok()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default();
        Self {
            project_id: preparation.project_id.clone(),
            version: preparation.version,
            plate: plate.plate,
            sha256: plate.computed.sha256.clone(),
            settings_source: settings_source(
                Some(&plate.computed.sha256),
                blocked.as_ref().map(|notice| notice.code.as_str()),
            ),
            blocked,
            decisions,
            questions: plan.questions.iter().map(notice).collect(),
            warnings: plan.warnings.iter().map(notice).collect(),
            notes: plan.notes.iter().map(notice).collect(),
            assumptions: assumptions(&plate.basis),
            checklist,
            facts: plan
                .facts
                .as_ref()
                .and_then(|facts| serde_json::to_value(facts).ok()),
            clearance_mm: preparation.clearance_mm,
            nozzle_diameter_mm: plate.basis.nozzle_diameter_mm,
            nozzle_size_set: plate.basis.nozzle_size_set,
            revisions,
            baseline: baseline.map(|entry| PlanBaseline {
                printer_id: entry.printer_id.clone(),
                plate_type: entry.plate_type.clone(),
                material_id: entry.material_id.clone(),
                printed_on: entry.printed_on.clone(),
                source: entry.source.clone(),
                fields: plan
                    .derived
                    .as_ref()
                    .map(|derived| {
                        derived
                            .proven_fields
                            .iter()
                            .map(|field| field.as_str().to_owned())
                            .collect()
                    })
                    .unwrap_or_default(),
            }),
        }
    }
}

/// Where a decided value came from, in words. A value of the proven baseline names the day
/// and the person's label; the rules name their id; a change names who asked.
fn origin(
    decision: &hotend_settings::model::Decision,
    baseline: Option<&ProvenBaseline>,
) -> String {
    use hotend_settings::model::Source;
    match decision.source {
        Source::Proven => match baseline {
            Some(entry) => format!(
                "proven on your printer ({}, {})",
                entry.printed_on, entry.source
            ),
            None => "proven on your printer".to_owned(),
        },
        // The plate and the spool are what the person told Hotend in the settings.
        Source::Person if decision.rule == hotend_settings::model::RuleId::Set1 => {
            "your settings".to_owned()
        }
        Source::Person => "your choice".to_owned(),
        Source::Agent => "the assistant's choice".to_owned(),
        Source::Intent => format!("the intent ({})", decision.rule.as_str()),
        Source::History => format!("print history ({})", decision.rule.as_str()),
        Source::Calibration => format!("the fit calibration ({})", decision.rule.as_str()),
        Source::Profile => "the installed profile".to_owned(),
        Source::Rule => format!("rule {}", decision.rule.as_str()),
    }
}

/// The JSON of a setting's value.
fn setting_json(value: &Setting) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// What a plan takes for granted, in the core's own fixed words.
fn assumptions(basis: &PlanBasis) -> Vec<PlanNotice> {
    let mut out = Vec::new();
    let mut add = |code: &str, sentence: String| {
        out.push(PlanNotice {
            code: code.to_owned(),
            sentence,
        });
    };
    if !basis.nozzle_size_set {
        add(
            "nozzle_size_assumed",
            format!(
                "The fitted nozzle is taken to be {} mm because no size was set; set the size \
                 of the nozzle on the printer in the settings.",
                basis.nozzle_diameter_mm
            ),
        );
    }
    if !basis.spool_colour_set {
        add(
            "spool_colour_assumed",
            format!(
                "The spool's colour is taken to be grey ({}) because none was set; it names \
                 the filament and changes no other setting.",
                basis.spool_colour
            ),
        );
    }
    add(
        "intent_default",
        "The intent is balanced, the default; nothing else was asked for.".to_owned(),
    );
    if let Some(entry) = &basis.baseline {
        add(
            "proven_baseline",
            format!(
                "The sticking settings proven on this printer, plate and material ({}) are the \
                 floor of this plan; the rules may make it more careful, never less.",
                entry.printed_on
            ),
        );
    }
    add(
        "no_print_history",
        "No print history yet: no print on this printer, plate and material has been \
         reported, so nothing raises the adhesion level beyond the rules."
            .to_owned(),
    );
    add(
        "no_calibration",
        "No fit calibration is used yet: the first layer's height, line width and elephant \
         foot stay as the installed profile has them."
            .to_owned(),
    );
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    /// A ring gauge, as the engine states it.
    fn ring_facts() -> Value {
        serde_json::from_str(include_str!(
            "../../../../python/tests/engine/data/ring-gauge-facts.json"
        ))
        .unwrap()
    }

    fn settings() -> Settings {
        Settings::default()
    }

    fn pieces(facts: &Value) -> usize {
        facts["pieces"].as_array().unwrap().len()
    }

    #[test]
    fn without_a_plate_type_the_plan_is_blocked_with_its_question() {
        let facts = ring_facts();
        let basis = PlanBasis::new(&settings(), facts.clone(), pieces(&facts));
        let computed = basis.plan().unwrap();
        let blocked = computed.plan.blocked.as_ref().unwrap();
        assert_eq!(blocked.code, "bed_type_not_chosen");
        assert!(computed.plan.settings.is_none());
        assert_eq!(computed.sha256.len(), 64);
        // The brim rules read geometry only: the room for the brim is known all the same.
        assert_eq!(basis.clearance_mm().unwrap(), Some(6.0));
    }

    #[test]
    fn the_ring_gauge_on_textured_pei_gets_its_brim_and_a_stable_hash() {
        let facts = ring_facts();
        let mut settings = settings();
        settings.plate_type = Some("textured-pei".into());
        let basis = PlanBasis::new(&settings, facts.clone(), pieces(&facts));
        let first = basis.plan().unwrap();
        assert!(first.plan.blocked.is_none(), "{:?}", first.plan.blocked);
        let value = |field: Field| first.plan.value(field).unwrap();
        assert_eq!(value(Field::BrimKind).token(), Some("outer"));
        assert_eq!(value(Field::BrimWidth).whole(), Some(5));
        assert_eq!(required_clearance(&first.plan), 6.0);
        // Equal inputs, equal hash; a changed input, another hash.
        assert_eq!(basis.plan().unwrap().sha256, first.sha256);
        settings.spool.colour = Some("#ff0000".into());
        let other = PlanBasis::new(&settings, facts.clone(), pieces(&facts))
            .plan()
            .unwrap();
        assert_ne!(other.sha256, first.sha256);
        // The nozzle size reaches the printer facts; 0.4 mm is assumed until set.
        assert!(!basis.nozzle_size_set);
        assert_eq!(basis.nozzle_diameter_mm, 0.4);
        assert_eq!(basis.max_layer_height_mm, 0.28);
        settings.nozzle_diameter_mm = Some(0.6);
        let wide = PlanBasis::new(&settings, facts.clone(), pieces(&facts));
        assert!(wide.nozzle_size_set);
        assert_eq!(wide.max_layer_height_mm, 0.42);
    }

    #[test]
    fn facts_that_are_refused_or_do_not_match_the_plate_block_the_plan() {
        let facts = ring_facts();
        let mut settings = settings();
        settings.plate_type = Some("textured-pei".into());
        let code = |facts: Value, pieces: usize| {
            PlanBasis::new(&settings, facts, pieces)
                .plan()
                .unwrap()
                .plan
                .blocked
                .map(|notice| notice.code)
        };
        assert_eq!(
            code(facts.clone(), pieces(&facts) + 1),
            Some("facts_invalid")
        );
        assert_eq!(code(Value::Null, 0), Some("facts_invalid"));
        let mut negative = facts.clone();
        negative["pieces"][0]["contact_mm2"] = json!(-1.0);
        assert_eq!(code(negative, pieces(&facts)), Some("facts_invalid"));
        let mut unknown = facts.clone();
        unknown["pieces"][0]["colour"] = json!("red");
        assert_eq!(code(unknown, pieces(&facts)), Some("facts_invalid"));
        let mut no_family = facts.clone();
        no_family["family"] = Value::Null;
        assert_eq!(code(no_family, pieces(&facts)), Some("facts_invalid"));
        assert_eq!(
            PlanBasis::new(&settings, Value::Null, 0)
                .clearance_mm()
                .unwrap(),
            None
        );
    }

    #[test]
    fn the_printer_data_give_the_bed_limit_the_slots_and_the_keepout() {
        let mut settings = settings();
        settings.plate_type = Some("textured-pei".into());
        let (limit, feeds) = printer_data(&settings);
        // PLA's limit binds on the textured plate (machine and plate allow 120 C).
        assert!(limit < 120, "{limit}");
        assert_eq!(
            feeds
                .iter()
                .filter(|feed| feed.slot.parse::<u8>().is_ok())
                .count(),
            16
        );
        let main = feeds
            .iter()
            .find(|feed| feed.slot == "external_main")
            .unwrap();
        assert!(main.main);
        let aux = feeds
            .iter()
            .find(|feed| feed.slot == "external_auxiliary")
            .unwrap();
        assert!(!aux.main);
        let zones = keepouts(&settings.printer_id);
        assert_eq!(zones.len(), 1);
        assert_eq!(zones[0].id, "start_load_line");
        assert_eq!((zones[0].min, zones[0].max), ([55.0, 0.0], [145.0, 8.0]));
    }

    fn proven(material_id: &str) -> ProvenBaseline {
        ProvenBaseline {
            printer_id: "bambu-x2d".into(),
            plate_type: "textured-pei".into(),
            material_id: material_id.into(),
            printed_on: "2026-11-17".into(),
            source: "a ring that held".into(),
            values: super::super::baseline::BaselineFields {
                level: 1,
                brim_kind: "outer".into(),
                brim_width_mm: 8,
                first_layer_wall_speed_mm_s: 15,
                first_layer_infill_speed_mm_s: 25,
                first_layer_bed_c: 65,
                layers_bed_c: 60,
                part_fan_off_layers: 4,
                aux_fan_first_layers_pct: 0,
                aux_fan_max_pct: 20,
                wall_loops: 4,
            },
            recorded_at_ms: 1,
        }
    }

    #[test]
    fn a_proven_baseline_is_a_floor_of_the_plan_and_part_of_its_basis() {
        let facts = ring_facts();
        let mut settings = settings();
        settings.plate_type = Some("textured-pei".into());
        let alone = PlanBasis::new(&settings, facts.clone(), pieces(&facts));
        let basis = alone
            .clone()
            .with_baseline(Some(proven(&settings.material.id)));
        let rules = alone.plan().unwrap();
        let computed = basis.plan().unwrap();
        assert_ne!(rules.sha256, computed.sha256);
        let value = |field: Field| computed.plan.value(field).unwrap();
        assert_eq!(value(Field::BrimWidth).whole(), Some(8));
        assert_eq!(value(Field::FirstLayerWallSpeed).whole(), Some(15));
        assert_eq!(value(Field::FirstLayerBed).whole(), Some(65));
        assert_eq!(value(Field::PartFanOffLayers).whole(), Some(4));
        assert_eq!(value(Field::WallLoops).whole(), Some(4));
        assert_eq!(value(Field::BrimGap), Setting::Decimal(0.0));
        assert_eq!(required_clearance(&computed.plan), 9.0);
        // The basis with its baseline is stored, and an old record without one still reads.
        let text = serde_json::to_value(&basis).unwrap();
        assert_eq!(
            text["baseline"]["values"]["adhesion.brim_width_mm"],
            json!(8)
        );
        let mut old = serde_json::to_value(&alone).unwrap();
        assert!(old.get("baseline").is_none());
        old.as_object_mut().unwrap().remove("baseline");
        let read: PlanBasis = serde_json::from_value(old).unwrap();
        assert_eq!(read.plan().unwrap().sha256, rules.sha256);
        // The label is not part of the plan: another label, the same plan.
        let mut relabelled = proven(&settings.material.id);
        relabelled.source = "the same print, other words".into();
        let other = alone
            .clone()
            .with_baseline(Some(relabelled))
            .plan()
            .unwrap();
        assert_eq!(other.sha256, computed.sha256);
        // A baseline of another material is no floor of this plate.
        let foreign = alone
            .with_baseline(Some(proven("petg-generic")))
            .plan()
            .unwrap();
        assert_eq!(foreign.sha256, rules.sha256);
    }

    #[test]
    fn the_plan_view_says_where_every_value_came_from() {
        let facts = ring_facts();
        let mut settings = settings();
        settings.plate_type = Some("textured-pei".into());
        let entry = proven(&settings.material.id);
        let basis = PlanBasis::new(&settings, facts.clone(), pieces(&facts))
            .with_baseline(Some(entry.clone()));
        let computed = basis.plan().unwrap();
        let baseline = Some(&entry);
        let origin_of = |field: Field| origin(computed.plan.decision(field).unwrap(), baseline);
        assert_eq!(
            origin_of(Field::BrimWidth),
            "proven on your printer (2026-11-17, a ring that held)"
        );
        assert_eq!(
            origin_of(Field::FirstLayerWallSpeed),
            origin_of(Field::BrimWidth)
        );
        // The card already gives the ring 65 C at its own level: the rule says so.
        assert_eq!(origin_of(Field::FirstLayerBed), "rule CARD");
        assert_eq!(origin_of(Field::BrimGap), "rule FIT-2");
        assert_eq!(origin_of(Field::PlateType), "your settings");
        assert!(!origin_of(Field::BrimWidth).contains("choice"));
        assert_eq!(origin_of(Field::Process), "the intent (INT-1)");
        assert_eq!(origin_of(Field::SpeedCap), "the installed profile");
        let assumptions = assumptions(&basis);
        assert!(
            assumptions
                .iter()
                .any(|notice| notice.code == "proven_baseline")
        );
    }
}
