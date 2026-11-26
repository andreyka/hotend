//! Print preparation, gauges and calibration: the engine ops that turn a built version into
//! plates, export test gauges and take caliper readings into the calibration table.
//!
//! Guarantees:
//! - None of these runs model code; the jobs read the stored B-rep of a version, feature
//!   cards as data, or numbers.
//! - A preparation is stored with its version under `prepare/`, at paths the core chooses;
//!   it replaces the earlier preparation of that version as a whole or not at all.
//! - The date of a measurement is the core's clock, never the caller's or the engine's.
//! - The calibration table is replaced atomically and only by a table for the printer in
//!   the settings.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::plan::{self, PlanBasis, PlanNotice, PlatePlan};
use super::{
    MAX_CALIBRATION_BYTES, MAX_GEOMETRY_BYTES, MAX_PNG_BYTES, Modeler, ModelingError, PNG_MAGIC,
    expect_path,
};
use crate::engine::{
    Anchor, CalibrateRequest, CalibrationChange, CardRef, CheckResult, EngineRequest, EngineStatus,
    Gauge, GaugeReading, GaugeRequest, Keepout, Orientation, Plate, PrepareRequest, PrepareStage,
    Question, Unplaced, UpHint,
};
use crate::jobs::{FinishedJob, JobError, JobScope};
use crate::store::NewAudit;
use crate::util::{is_model_name, now_ms, now_rfc3339};

/// Most copies of one part on a preparation (the engine's limit).
pub const MAX_COPIES: u32 = 64;
const PREPARE_FILE: &str = "prepare.json";
const MAX_PREPARE_BYTES: u64 = 8 * 1024 * 1024;

/// What the user chose for a preparation. Empty means the engine's choices.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PrepareOptions {
    /// Copies per part, 1 to 64.
    pub copies: BTreeMap<String, u32>,
    /// The direction of a part, as modelled, that should point up: `+x`, `-x`, `+y`, `-y`,
    /// `+z` or `-z`.
    pub orientations: BTreeMap<String, String>,
}

/// A version laid out on plates, as stored in `versions/<n>/prepare/prepare.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preparation {
    pub project_id: String,
    pub version: u32,
    /// The printer and material the plates were made for.
    pub printer_id: String,
    pub material_id: String,
    pub options: PrepareOptions,
    /// `ok`, or `check_error` when a piece fits no plate.
    pub status: EngineStatus,
    pub orientations: Vec<Orientation>,
    /// Paths in a plate are relative to `dir`.
    pub plates: Vec<Plate>,
    pub unplaced: Vec<Unplaced>,
    pub checks: Vec<CheckResult>,
    pub warnings: Vec<String>,
    pub created_at_ms: i64,
    /// The room kept free round every piece for its brim, in mm: the widest brim the print
    /// settings give any piece, plus 1 mm. `None` for a preparation made
    /// before the print settings, in one pass and without facts; such a preparation is laid
    /// out again before a plan or a slice is made from it.
    #[serde(default)]
    pub clearance_mm: Option<f64>,
    /// The distance of every grown footprint from the edge of the reach, mm.
    #[serde(default)]
    pub margin_mm: Option<f64>,
    /// The parts of the bed the layout kept free (the printer data's keep-out zones).
    #[serde(default)]
    pub keepouts: Vec<Keepout>,
    /// The plan of every plate as it was when the version was prepared. The settings may
    /// have changed since: `print_plan_get` gives the plan as it is now.
    #[serde(default)]
    pub plans: Vec<PlatePlanSummary>,
    /// The folder that holds `prepare.json` and the plates.
    pub dir: PathBuf,
}

/// The plan of one plate, in short.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatePlanSummary {
    pub plate: u32,
    /// SHA-256 of the plan.
    pub sha256: String,
    /// Why there are no print settings for the plate yet, when there are none.
    pub blocked: Option<PlanNotice>,
    /// Which settings a slice of the plate would use, in words.
    pub settings_source: String,
}

impl Preparation {
    /// The 3MF of a plate (numbered from 1), if that plate has one.
    pub fn plate_file(&self, index: u32) -> Option<PathBuf> {
        let plate = self.plates.iter().find(|plate| plate.index == index)?;
        Some(self.dir.join(plate.threemf.as_ref()?))
    }

    /// Hotend's render `view` (`iso`, `top`, ...) of plate `index`, when the layout made
    /// one: the source of Hotend-made plate previews.
    pub fn plate_view(&self, index: u32, view: &str) -> Option<PathBuf> {
        let plate = self.plates.iter().find(|plate| plate.index == index)?;
        Some(self.dir.join(plate.views.get(view)?))
    }

    /// Whether the preparation was made for the print settings: in two passes, with the
    /// facts of every plate.
    pub fn has_print_facts(&self) -> bool {
        self.clearance_mm.is_some() && self.plates.iter().all(|plate| plate.facts.is_some())
    }

    /// Whether the preparation can be planned and sliced under these settings: made for
    /// the same printer and material, in two passes, for the same keep-out zones.
    pub fn is_current_for(&self, settings: &crate::settings::Settings) -> bool {
        self.printer_id == settings.printer_id
            && self.material_id == settings.material.id
            && self.has_print_facts()
            && self.keepouts == plan::keepouts(&settings.printer_id)
    }
}

/// One file of an exported gauge plate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GaugeFile {
    pub plate: u32,
    /// `3mf`, or the name of a view for a picture.
    pub kind: String,
    pub path: PathBuf,
}

/// Gauges exported into a folder of the user's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GaugeExport {
    pub gauges: Vec<Gauge>,
    pub plates: Vec<Plate>,
    pub unplaced: Vec<Unplaced>,
    pub warnings: Vec<String>,
    pub files: Vec<GaugeFile>,
}

/// The calibration table of the printer in the settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Calibration {
    pub printer_id: String,
    /// The table of PROTOCOL section 4.2, or `None` when nothing was measured or saved yet
    /// (the engine then uses its guessed defaults).
    pub table: Option<Value>,
}

/// What recording gauge readings changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalibrationUpdate {
    pub printer_id: String,
    pub material_id: String,
    pub nozzle_id: Option<i64>,
    /// The date stored with the readings, from the core's clock (RFC 3339, UTC).
    pub measured_at: String,
    pub changes: Vec<CalibrationChange>,
    /// The provenance of the entry before this update.
    pub entry_provenance: Option<String>,
    pub table: Value,
}

fn invalid(message: impl Into<String>) -> ModelingError {
    ModelingError::Invalid(message.into())
}

fn io_error(path: &Path) -> impl FnOnce(std::io::Error) -> ModelingError + use<> {
    let path = path.to_path_buf();
    move |source| ModelingError::Io { path, source }
}

fn check_destination(destination: &Path) -> Result<(), ModelingError> {
    if destination.is_dir() {
        Ok(())
    } else {
        Err(invalid(format!(
            "the folder {} does not exist",
            destination.display()
        )))
    }
}

impl Modeler {
    fn calibration_file(&self, printer_id: &str) -> PathBuf {
        self.data
            .root()
            .join("calibration")
            .join(format!("{printer_id}.json"))
    }

    /// The questions the engine has for the user about a version: requirements of the
    /// contract that rest on a guessed parameter.
    pub fn questions(&self, project_id: &str, number: u32) -> Result<Vec<Question>, ModelingError> {
        Ok(self
            .version_detail(project_id, number)?
            .result
            .questions
            .unwrap_or_default())
    }

    /// The stored preparation of a version, if there is one.
    pub fn preparation(
        &self,
        project_id: &str,
        number: u32,
    ) -> Result<Option<Preparation>, ModelingError> {
        self.store.version(project_id, number)?;
        let dir = self.data.version_dir(project_id, number)?.join("prepare");
        let path = dir.join(PREPARE_FILE);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && metadata.len() <= MAX_PREPARE_BYTES => {}
            _ => return Ok(None),
        }
        let bytes = std::fs::read(&path).map_err(io_error(&path))?;
        let mut preparation: Preparation = serde_json::from_slice(&bytes)
            .map_err(|e| io_error(&path)(std::io::Error::other(e)))?;
        preparation.dir = dir;
        Ok(Some(preparation))
    }

    /// Refuses a nozzle id the printer in the settings does not have, before the engine is
    /// asked: a wrong id is the caller's slip, not a failure of the machine.
    fn known_nozzle(&self, printer_id: &str, nozzle_id: Option<i64>) -> Result<(), ModelingError> {
        let (Some(id), Some(printer)) = (nozzle_id, crate::profiles::printer_profile(printer_id))
        else {
            return Ok(());
        };
        if printer.nozzles.iter().any(|nozzle| nozzle.id == id) {
            return Ok(());
        }
        let known: Vec<String> = printer
            .nozzles
            .iter()
            .map(|nozzle| format!("{} ({})", nozzle.id, nozzle.role))
            .collect();
        Err(invalid(format!(
            "{} has no nozzle with the id {id}; its nozzles are {}",
            printer.name,
            known.join(", ")
        )))
    }

    /// A warning when the version was built, and so had its fits resolved and its checks
    /// run, for another printer, material or calibration than the settings name now.
    /// `None` when they agree, or when the version does not say what it was built for.
    fn built_for_other(
        &self,
        project_id: &str,
        number: u32,
        settings: &crate::settings::Settings,
    ) -> Option<String> {
        let dir = self.data.version_dir(project_id, number).ok()?;
        super::BuiltFor::print_warning(
            &dir,
            number,
            &settings.printer_id,
            &settings.material.id,
            &self.calibration_file(&settings.printer_id),
        )
    }

    /// Lays a version (the current one when `version` is `None`) out on plates and stores
    /// the result with the version, replacing an earlier preparation.
    pub async fn prepare(
        &self,
        scope: &JobScope,
        project_id: &str,
        version: Option<u32>,
        options: PrepareOptions,
    ) -> Result<Preparation, ModelingError> {
        let mut orientations = BTreeMap::new();
        for (part, up) in &options.orientations {
            if !is_model_name(part) || !["+x", "-x", "+y", "-y", "+z", "-z"].contains(&up.as_str())
            {
                return Err(invalid(format!(
                    "the orientation of `{part}` must be one of +x, -x, +y, -y, +z, -z"
                )));
            }
            orientations.insert(part.clone(), UpHint { up: json!(up) });
        }
        for (part, copies) in &options.copies {
            if !is_model_name(part) || !(1..=MAX_COPIES).contains(copies) {
                return Err(invalid(format!(
                    "the copies of `{part}` must be between 1 and {MAX_COPIES}"
                )));
            }
        }
        let number = match version {
            Some(number) => self.store.version(project_id, number)?.number,
            None => self.current_version(project_id)?.number,
        };
        // A part the version does not have is the caller's slip, not the engine's failure.
        let parts: Vec<String> = self
            .version_detail(project_id, number)?
            .result
            .parts
            .into_iter()
            .map(|part| part.name)
            .collect();
        for part in options.copies.keys().chain(options.orientations.keys()) {
            if !parts.is_empty() && !parts.contains(part) {
                return Err(invalid(format!(
                    "version {number} has no part `{part}`; its parts are {}",
                    parts.join(", ")
                )));
            }
        }
        let lock = self.project_lock(project_id);
        let _guard = lock.lock().await;
        let settings = self.settings()?;
        let (context, mut files) = self.context()?;
        if context.printer.is_none() {
            return Err(invalid(format!(
                "the printer `{}` in the settings is not known; choose a printer first",
                settings.printer_id
            )));
        }
        let built_for_other = self.built_for_other(project_id, number, &settings);
        files.extend(self.geometry_files(project_id, number)?);
        // Two passes. The first chooses the orientations and measures
        // the facts; the brim rules then say how much room each piece needs; the second
        // lays the same orientations out with that room, mid-bed, off the keep-out zones.
        let keepouts = plan::keepouts(&settings.printer_id);
        let base = PrepareRequest {
            context,
            copies: options.copies.clone(),
            margin: plan::PRINT_MARGIN_MM,
            anchor: Some(Anchor::Centre),
            keepouts: keepouts.clone(),
            ..PrepareRequest::default()
        };
        let orient = EngineRequest::Prepare(PrepareRequest {
            stage: Some(PrepareStage::Orient),
            clearance: Some(0.0),
            orientations,
            threemf: false,
            views: Vec::new(),
            ..base.clone()
        });
        let first = self.jobs.run(scope, &orient, files.clone()).await?;
        prepared_or_refused(&first)?;
        let pieces: u32 = first.result.orientations.iter().map(|o| o.copies).sum();
        let all_facts = first.result.facts.clone().unwrap_or(Value::Null);
        let baseline = self.baseline_for(&settings)?;
        let clearance = PlanBasis::new(&settings, all_facts, pieces as usize)
            .with_baseline(baseline.clone())
            .clearance_mm()?
            .unwrap_or(0.0);
        let pinned = first
            .result
            .orientations
            .iter()
            .map(|orientation| {
                (
                    orientation.part.clone(),
                    UpHint {
                        up: json!(orientation.up),
                    },
                )
            })
            .collect();
        let request = EngineRequest::Prepare(PrepareRequest {
            stage: Some(PrepareStage::Plate),
            clearance: Some(clearance),
            orientations: pinned,
            ..base
        });
        let job = self.jobs.run(scope, &request, files).await?;
        prepared_or_refused(&job)?;

        let version_dir = self.data.version_dir(project_id, number)?;
        let dir = version_dir.join("prepare");
        let staging = version_dir.join(format!(".prepare-{}", job.id));
        // The orientations as the first pass chose them (with the reasons and any request
        // that was not followed); the second pass took them as they were.
        let orientations = first
            .result
            .orientations
            .iter()
            .map(|chosen| {
                let laid = job
                    .result
                    .orientations
                    .iter()
                    .find(|laid| laid.part == chosen.part);
                Orientation {
                    facts: laid
                        .and_then(|laid| laid.facts.clone())
                        .or(chosen.facts.clone()),
                    ..chosen.clone()
                }
            })
            .collect();
        let mut preparation = Preparation {
            project_id: project_id.to_owned(),
            version: number,
            printer_id: settings.printer_id.clone(),
            material_id: settings.material.id.clone(),
            options,
            status: job.result.status,
            orientations,
            plates: job.result.plates.clone(),
            unplaced: job.result.unplaced.clone(),
            checks: job.result.checks.clone(),
            warnings: built_for_other
                .into_iter()
                .chain(first.result.warnings.iter().cloned())
                .chain(job.result.warnings.iter().cloned())
                .collect(),
            created_at_ms: now_ms(),
            clearance_mm: Some(clearance),
            margin_mm: Some(plan::PRINT_MARGIN_MM),
            keepouts,
            plans: Vec::new(),
            dir: staging.clone(),
        };
        // The plan of every plate, from that plate's own facts.
        let mut plans = Vec::new();
        for layout in &preparation.plates {
            let plate_plan =
                PlatePlan::of(&preparation, layout.index, &settings, baseline.clone())?;
            let blocked = plate_plan.blocked();
            preparation.plans.push(PlatePlanSummary {
                plate: layout.index,
                sha256: plate_plan.computed.sha256.clone(),
                settings_source: plan::settings_source(
                    Some(&plate_plan.computed.sha256),
                    blocked.as_ref().map(|notice| notice.code.as_str()),
                ),
                blocked,
            });
            plans.push(plate_plan);
        }
        preparation.dir = PathBuf::new();
        let stored = preparation.clone();
        let staged_preparation = Preparation {
            dir: staging.clone(),
            ..preparation.clone()
        };
        let (staged, target) = (staging.clone(), dir.clone());
        let outcome = tokio::task::spawn_blocking(move || {
            // A layout in which nothing could be placed has no plate file, and is stored
            // all the same: its `unplaced` list says why.
            let outcome = std::fs::create_dir_all(&staged)
                .map_err(io_error(&staged))
                .and_then(|()| {
                    stage_plates(&job, &stored.plates, &staged, |plate, kind| match kind {
                        None => plate.threemf.clone().map(PathBuf::from),
                        Some(view) => plate.views.get(view).map(PathBuf::from),
                    })
                    .map_err(ModelingError::Job)
                })
                .and_then(|_| {
                    for plate_plan in &plans {
                        plate_plan.store(&staged_preparation)?;
                    }
                    let path = staged.join(PREPARE_FILE);
                    let bytes = serde_json::to_vec_pretty(&stored).unwrap_or_default();
                    std::fs::write(&path, bytes).map_err(io_error(&path))?;
                    if target.exists() {
                        std::fs::remove_dir_all(&target).map_err(io_error(&target))?;
                    }
                    std::fs::rename(&staged, &target).map_err(io_error(&target))
                });
            if outcome.is_err() {
                let _ = std::fs::remove_dir_all(&staged);
            }
            outcome
        })
        .await
        .map_err(|e| io_error(&staging)(std::io::Error::other(e)))
        .and_then(|outcome| outcome);
        let _ = self.store.audit(NewAudit {
            project_id: Some(project_id.to_owned()),
            run_id: scope.run_id.clone(),
            kind: "prepare",
            name: "prepare".to_owned(),
            args_sha256: None,
            outcome: match &outcome {
                Ok(()) => preparation.status.as_str().to_owned(),
                Err(error) => error.code().to_owned(),
            },
            detail: json!({
                "version": number,
                "plates": preparation.plates.len(),
                "unplaced": preparation.unplaced.len(),
                "clearance_mm": preparation.clearance_mm,
                "plans": preparation.plans.iter().map(|summary| json!({
                    "plate": summary.plate,
                    "sha256": summary.sha256,
                    "blocked": summary.blocked.as_ref().map(|notice| notice.code.clone()),
                })).collect::<Vec<_>>(),
            }),
        });
        outcome?;
        Ok(Preparation { dir, ..preparation })
    }

    /// Exports the test gauges of feature cards as plates into `destination`, an existing
    /// folder the user chose. Without `cards`, the gauges of every card the project's current
    /// version uses.
    pub async fn gauge(
        &self,
        scope: &JobScope,
        project_id: Option<&str>,
        cards: Option<Vec<CardRef>>,
        nozzle_id: Option<i64>,
        destination: &Path,
    ) -> Result<GaugeExport, ModelingError> {
        check_destination(destination)?;
        let (context, mut files) = self.context()?;
        if context.printer.is_none() {
            return Err(invalid("the printer in the settings is not known"));
        }
        self.known_nozzle(&self.settings()?.printer_id, nozzle_id)?;
        match (&cards, project_id) {
            (Some(cards), _) if cards.is_empty() || cards.len() > 32 => {
                return Err(invalid("name between 1 and 32 feature cards"));
            }
            (Some(_), _) => {}
            (None, Some(project_id)) => {
                let number = self.current_version(project_id)?.number;
                files.extend(self.geometry_files(project_id, number)?);
            }
            (None, None) => {
                return Err(invalid(
                    "name the feature cards, or a project whose cards to use",
                ));
            }
        }
        let request = EngineRequest::Gauge(GaugeRequest {
            context,
            cards,
            nozzle_id,
            ..GaugeRequest::default()
        });
        let job = self.jobs.run(scope, &request, files).await?;
        if !matches!(
            job.result.status,
            EngineStatus::Ok | EngineStatus::CheckError
        ) {
            return Err(invalid(job.result.error.as_ref().map_or_else(
                || "the engine could not make the gauges".to_owned(),
                |error| error.message.clone(),
            )));
        }
        let destination = destination.to_path_buf();
        let export = tokio::task::spawn_blocking(move || {
            let files = stage_plates(&job, &job.result.plates, &destination, |plate, kind| {
                let index = plate.index;
                Some(PathBuf::from(match kind {
                    None => format!("gauge-plate-{index}.3mf"),
                    Some(view) => format!("gauge-plate-{index}-{view}.png"),
                }))
            })?;
            Ok::<_, JobError>(GaugeExport {
                gauges: job.result.gauges.clone(),
                plates: job.result.plates.clone(),
                unplaced: job.result.unplaced.clone(),
                warnings: job.result.warnings.clone(),
                files,
            })
        })
        .await
        .map_err(|e| invalid(format!("the gauge export was interrupted: {e}")))?
        .map_err(ModelingError::Job)?;
        let _ = self.store.audit(NewAudit {
            project_id: project_id.map(str::to_owned),
            run_id: scope.run_id.clone(),
            kind: "gauge",
            name: "gauge".to_owned(),
            args_sha256: None,
            outcome: "ok".to_owned(),
            detail: json!({"gauges": export.gauges.len(), "plates": export.plates.len()}),
        });
        Ok(export)
    }

    /// The calibration table of the printer in the settings.
    pub fn calibration(&self) -> Result<Calibration, ModelingError> {
        let settings = self.settings()?;
        settings.validate().map_err(invalid)?;
        let path = self.calibration_file(&settings.printer_id);
        let table = match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && metadata.len() <= MAX_CALIBRATION_BYTES => {
                let bytes = std::fs::read(&path).map_err(io_error(&path))?;
                Some(
                    serde_json::from_slice(&bytes)
                        .map_err(|e| io_error(&path)(std::io::Error::other(e)))?,
                )
            }
            _ => None,
        };
        Ok(Calibration {
            printer_id: settings.printer_id,
            table,
        })
    }

    /// Records caliper readings of a printed gauge in the calibration table of the printer
    /// and material in the settings. The readings are dated now, by the core's clock.
    pub async fn calibrate(
        &self,
        scope: &JobScope,
        readings: Vec<GaugeReading>,
        nozzle_id: Option<i64>,
    ) -> Result<CalibrationUpdate, ModelingError> {
        if readings.is_empty() || readings.len() > 64 {
            return Err(invalid("give between 1 and 64 readings"));
        }
        if readings
            .iter()
            .any(|r| !(r.drawn.is_finite() && r.measured.is_finite() && r.drawn > 0.0))
        {
            return Err(invalid("a reading needs a drawn and a measured size in mm"));
        }
        let settings = self.settings()?;
        settings.validate().map_err(invalid)?;
        self.known_nozzle(&settings.printer_id, nozzle_id)?;
        let (context, files) = self.context()?;
        let measured_at = now_rfc3339();
        let request = EngineRequest::Calibrate(CalibrateRequest {
            context,
            nozzle_id,
            measured_at: measured_at.clone(),
            readings,
            limits: crate::engine::EngineLimits::default(),
        });
        let job = self.jobs.run(scope, &request, files).await?;
        let table = match (&job.result.status, &job.result.table) {
            (EngineStatus::Ok, Some(table))
                if table.get("printer_id").and_then(Value::as_str)
                    == Some(settings.printer_id.as_str()) =>
            {
                table.clone()
            }
            (EngineStatus::Ok, _) => {
                return Err(ModelingError::Job(JobError::MalformedResult {
                    reason: "the calibration table is not for the printer in the settings"
                        .to_owned(),
                }));
            }
            _ => {
                return Err(invalid(job.result.error.as_ref().map_or_else(
                    || "the engine refused the readings".to_owned(),
                    |error| error.message.clone(),
                )));
            }
        };
        let path = self.calibration_file(&settings.printer_id);
        let bytes = serde_json::to_vec_pretty(&table).unwrap_or_default();
        if bytes.len() as u64 > MAX_CALIBRATION_BYTES {
            return Err(invalid("the calibration table is too large to store"));
        }
        write_atomically(&path, &bytes)?;
        let update = CalibrationUpdate {
            printer_id: settings.printer_id,
            material_id: settings.material.id,
            nozzle_id,
            measured_at,
            changes: job.result.changes.clone(),
            entry_provenance: job.result.entry_provenance.clone(),
            table,
        };
        let _ = self.store.audit(NewAudit {
            project_id: None,
            run_id: scope.run_id.clone(),
            kind: "calibration",
            name: update.printer_id.clone(),
            args_sha256: None,
            outcome: "ok".to_owned(),
            detail: json!({
                "material": update.material_id,
                "nozzle_id": update.nozzle_id,
                "measured_at": update.measured_at,
                "changes": update.changes,
            }),
        });
        Ok(update)
    }
}

/// Accepts a finished preparation job, or turns its failure into the engine's message.
fn prepared_or_refused(job: &FinishedJob) -> Result<(), ModelingError> {
    if matches!(
        job.result.status,
        EngineStatus::Ok | EngineStatus::CheckError
    ) {
        return Ok(());
    }
    Err(invalid(job.result.error.as_ref().map_or_else(
        || "the engine could not prepare this version".to_owned(),
        |error| error.message.clone(),
    )))
}

/// Replaces `path` with `bytes` by writing a sibling file and renaming it.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), ModelingError> {
    let parent = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(io_error(parent))?;
    let temporary = path.with_extension("json.new");
    std::fs::write(&temporary, bytes).map_err(io_error(&temporary))?;
    std::fs::rename(&temporary, path).map_err(io_error(path))
}

/// Copies the 3MF and the pictures of every plate out of a job folder. `target` names the
/// file inside `root` for a plate's 3MF (`None`) or one of its views; the paths the child
/// reported were already checked against the fixed layout when the result was parsed.
fn stage_plates(
    job: &FinishedJob,
    plates: &[Plate],
    root: &Path,
    target: impl Fn(&Plate, Option<&str>) -> Option<PathBuf>,
) -> Result<Vec<GaugeFile>, JobError> {
    let mut files = Vec::new();
    let mut copy = |plate: &Plate, kind: Option<&str>, source: &str, max: u64| {
        let Some(relative) = target(plate, kind) else {
            return Ok(());
        };
        let path = root.join(relative);
        if let Ok(metadata) = std::fs::symlink_metadata(&path) {
            let removed = if metadata.is_dir() {
                Err(std::io::Error::other("the destination is a folder"))
            } else {
                std::fs::remove_file(&path)
            };
            removed.map_err(|source| JobError::Io {
                path: path.clone(),
                source,
            })?;
        }
        job.copy_artifact(source, &path, max)?;
        if kind.is_some() {
            let head = std::fs::read(&path).map_err(|source| JobError::Io {
                path: path.clone(),
                source,
            })?;
            if !head.starts_with(PNG_MAGIC) {
                return Err(JobError::MalformedResult {
                    reason: format!("{source} is not a PNG image"),
                });
            }
        }
        files.push(GaugeFile {
            plate: plate.index,
            kind: kind.unwrap_or("3mf").to_owned(),
            path,
        });
        Ok(())
    };
    for plate in plates {
        let index = plate.index;
        let threemf = format!("plates/{index}/plate.3mf");
        if expect_path(plate.threemf.as_ref(), &threemf, false)? {
            copy(plate, None, &threemf, MAX_GEOMETRY_BYTES)?;
        }
        for (view, reported) in &plate.views {
            let expected = format!("plates/{index}/views/{view}.png");
            expect_path(Some(reported), &expected, true)?;
            copy(plate, Some(view), &expected, MAX_PNG_BYTES)?;
        }
    }
    Ok(files)
}
