//! [`hotend_core::SliceBackend`] on top of [`Slicer`]: what the application injects with
//! `Core::set_slice_backend` ([`install`] does it).
//!
//! The backend uses the request's `output_dir` as the job folder, so everything the slicer
//! reads or writes stays in `versions/<n>/slice/<profile-hash>/`. The input is the plate
//! the core prepared: a 3MF with the pieces already oriented and placed, which both
//! slicers read. The slicer is told not to arrange it, and the reach check runs on the
//! pieces where they lie. The core's request names one material, so the job has one
//! filament slot on the printer's main nozzle; a plate with a piece for another nozzle is
//! refused instead of being printed with the wrong one.
//!
//! The plate type is the one the core's request names: the plate the person chose in the
//! settings (`plate_type`). Without it a real Bambu Studio is not run at all
//! (`bed_type_not_chosen`): which plate is on the printer is not something to guess. Only
//! the stand-in slices for the plate `profiles/system-profiles.toml` names for it, and the
//! core does not let such a slice become a print. The plate a file was sliced for is
//! `SliceSummary::bed`; for a real slicer's file the warning `bed_plate` also tells the
//! user which plate and bed temperature the file is for. A PrusaSlicer job has no plate
//! type (the bed temperature is in its filament settings), so the request's plate is not
//! passed on for a printer that is sliced with PrusaSlicer.
//!
//! `SliceSummary` has no way to say "unknown". If the slicer's output does not state the
//! time, the layer count or a filament's use, the slice fails with `incomplete_result`
//! rather than report a made-up number. ([`Slicer::slice`] itself returns such a result
//! with `None` and a warning.)

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;
use hotend_core::gateway::CatalogueEntry;
use hotend_core::gateway::{
    FilamentUse, ProjectKeys as CoreProjectKeys, SliceWarning as CoreWarning,
    SlicerInfo as CoreSlicerInfo,
};
use hotend_core::{
    Core, ProjectExportSummary, SliceBackend, SliceBed, SliceError, SliceRequest, SliceSummary,
    SlicerCatalogue, SlicerInstallation,
};

use crate::detect::{Found, Installation};
use crate::error::SlicerError;
use crate::job::{Extent, FilamentAssignment, PlatePreviews, SliceJob, SliceResult};
use crate::slicer::{Slicer, SlicerConfig};
use crate::system::SystemProfiles;
use crate::version::{SlicerKind, Verdict};

/// Folder below the job folder that holds the model files.
const INPUT_DIR: &str = "input";
/// The plate's file name in the input folder.
const INPUT_NAME: &str = "plate.3mf";
/// Folder in the core's data directory with the application's vetted settings profiles.
pub const PROFILE_DIR: &str = "slicer-profiles";
/// Environment variable that names the installed Bambu Studio's profile folder
/// (`resources/profiles`) when it is not next to the program.
pub const SYSTEM_PROFILES_ENV: &str = "HOTEND_BAMBU_PROFILES";

/// Which slicer programs an installed backend runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Programs {
    /// The vendor slicers found on this machine, or chosen in the settings.
    Installed,
    /// Hotend's stand-in for both. Its numbers are estimates from the mesh volume and its
    /// output is not printable.
    StandIn,
}

/// Installs the slicer backend into the core.
pub fn install(core: &Core, programs: Programs) -> Result<(), SlicerError> {
    let mut config = SlicerConfig::new()?;
    let backend = match programs {
        Programs::Installed => {
            let profiles = core.data_dir().join(PROFILE_DIR);
            if profiles.is_dir() {
                config.profile_dir = Some(profiles);
            }
            let set = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
            config.system_profiles = set(SYSTEM_PROFILES_ENV).map(PathBuf::from);
            CoreSliceBackend::from_config(config)
        }
        Programs::StandIn => {
            let fake = crate::fake::locate().ok_or_else(|| {
                SlicerError::InvalidJob(format!(
                    "the stand-in slicer `{}` is not next to the running program; build it \
                     with `cargo build --workspace`",
                    crate::fake::executable_name()
                ))
            })?;
            for kind in SlicerKind::ALL {
                config.executables.insert(kind, fake.clone());
            }
            CoreSliceBackend::new(Arc::new(Slicer::new(config)?))
        }
    };
    core.set_slice_backend(Arc::new(backend));
    Ok(())
}

/// The executables a [`Slicer`] was made for, and that slicer.
type Configured = (BTreeMap<String, PathBuf>, Arc<Slicer>);

enum Source {
    /// One slicer, whatever the settings say.
    Fixed(Arc<Slicer>),
    /// A slicer made from the configuration plus the executables chosen in the settings,
    /// and made again when those change.
    Configured {
        config: Box<SlicerConfig>,
        current: Mutex<Option<Configured>>,
    },
}

/// The slicer backend for the core's tool gateway.
pub struct CoreSliceBackend {
    source: Source,
}

impl CoreSliceBackend {
    /// A backend that always runs `slicer`; the settings' executables are ignored.
    pub fn new(slicer: Arc<Slicer>) -> Self {
        Self {
            source: Source::Fixed(slicer),
        }
    }

    /// A backend that looks for the slicers itself, using the executables the user chose in
    /// the settings first.
    pub fn from_config(config: SlicerConfig) -> Self {
        Self {
            source: Source::Configured {
                config: Box::new(config),
                current: Mutex::new(None),
            },
        }
    }

    /// The slicer for a set of chosen executables. `fresh` looks for the programs again.
    fn slicer(
        &self,
        executables: &BTreeMap<String, PathBuf>,
        fresh: bool,
    ) -> Result<Arc<Slicer>, SlicerError> {
        let (config, current) = match &self.source {
            Source::Fixed(slicer) => return Ok(Arc::clone(slicer)),
            Source::Configured { config, current } => (config, current),
        };
        let mut current = current.lock().unwrap_or_else(|p| p.into_inner());
        if let (false, Some((made_for, slicer))) = (fresh, current.as_ref())
            && made_for == executables
        {
            return Ok(Arc::clone(slicer));
        }
        let mut config = (**config).clone();
        for (name, path) in executables {
            let kind = SlicerKind::ALL
                .into_iter()
                .find(|kind| kind.as_str() == name)
                .ok_or_else(|| {
                    SlicerError::InvalidJob(format!(
                        "the settings name an executable for `{}`, which is not a slicer of \
                         this build",
                        crate::job::untrusted_text(name, 40)
                    ))
                })?;
            config.executables.insert(kind, path.clone());
        }
        let slicer = Arc::new(Slicer::new(config)?);
        *current = Some((executables.clone(), Arc::clone(&slicer)));
        Ok(slicer)
    }

    async fn run(&self, request: SliceRequest) -> Result<SliceSummary, SlicerError> {
        let (slicer, job) = self.job_for(&request)?;
        let result = tokio::task::spawn_blocking(move || slicer.slice(&job))
            .await
            .map_err(|e| SlicerError::Failed {
                status: "the slice task stopped".into(),
                stderr_tail: e.to_string(),
            })??;
        to_summary(result)
    }

    /// Makes the editable Bambu Studio project of the plate of `request`, with the plate's
    /// print settings (`print_settings` must be `Some`), into the request's `output_dir`
    /// ([`Slicer::export_project`]). The project is returned only when its lone re-slice passed
    /// the proof; the returned `result` is that re-slice. Nothing is sent anywhere.
    pub async fn export_project(
        &self,
        request: SliceRequest,
    ) -> Result<crate::project::ProjectExport, SlicerError> {
        let (slicer, job) = self.job_for(&request)?;
        tokio::task::spawn_blocking(move || slicer.export_project(&job))
            .await
            .map_err(|e| SlicerError::Failed {
                status: "the export task stopped".into(),
                stderr_tail: e.to_string(),
            })?
    }

    /// The slicer and the job for a request: one filament slot on the printer's main nozzle,
    /// the plate copied into the job folder, the plate type, the print settings and the fitted
    /// nozzle size of the request.
    fn job_for(&self, request: &SliceRequest) -> Result<(Arc<Slicer>, SliceJob), SlicerError> {
        let slicer = self.slicer(&request.slicer_executables, false)?;
        let printer = slicer.printers().printer(&request.printer_id)?.clone();
        let nozzle = printer
            .main_nozzle()
            .or(printer.nozzles.first())
            .ok_or_else(|| SlicerError::UnknownPrinter(request.printer_id.clone()))?;
        let filaments = vec![FilamentAssignment {
            slot: 1,
            filament_profile: request.material.id.clone(),
            family: request.material.family.clone(),
            nozzle_id: nozzle.ids.device_extruder,
        }];
        if let Some(piece) = request
            .pieces
            .iter()
            .find(|piece| piece.nozzle_id != nozzle.ids.device_extruder)
        {
            return Err(SlicerError::InvalidJob(format!(
                "`{}` is laid out for nozzle {}, but a slice has one filament, on nozzle {}",
                crate::job::untrusted_text(&piece.name, 80),
                piece.nozzle_id,
                nozzle.ids.device_extruder
            )));
        }
        let extents: Vec<Extent> = request
            .pieces
            .iter()
            .take(256)
            .map(|piece| Extent {
                name: crate::job::untrusted_text(&piece.name, 80),
                slot: 1,
                min: piece.min,
                max: piece.max,
                placed: true,
            })
            .collect();
        // Fail on the printer's rules before anything is copied.
        printer.check_job(&filaments, &extents)?;

        let input_dir = request.output_dir.join(INPUT_DIR);
        reset_dir(&input_dir)?;
        let target = input_dir.join(INPUT_NAME);
        std::fs::copy(&request.plate_file, &target)
            .map_err(|e| SlicerError::io("copy the plate to", &target, e))?;

        let job = SliceJob {
            job_dir: request.output_dir.clone(),
            inputs: vec![PathBuf::from(INPUT_DIR).join(INPUT_NAME)],
            printer: request.printer_id.clone(),
            process_profile: request.profile.clone(),
            filaments,
            // Every prepared plate is a file of its own, so it is the slicer's plate 1.
            plate: 1,
            arrange: false,
            extents,
            // The plate the person chose in the settings. PrusaSlicer has no plate types.
            bed_type: request
                .plate_type
                .clone()
                .filter(|_| printer.slicer == SlicerKind::BambuStudio),
            print_settings: request.print_settings.clone(),
            nozzle_diameter_mm: Some(request.nozzle_diameter_mm),
            // Hotend-made plate previews, only when the settings ask for them.
            plate_previews: request
                .plate_previews
                .as_ref()
                .map(|renders| PlatePreviews {
                    iso: renders.iso.clone(),
                    top: renders.top.clone(),
                }),
        };
        Ok((slicer, job))
    }
}

fn installation_row(installation: &Installation, printers: &[String]) -> SlicerInstallation {
    let (verdict, reason) = match &installation.verdict {
        Verdict::Allow => ("allow", None),
        Verdict::Warn(reason) => ("warn", Some(reason.clone())),
        Verdict::Refuse(reason) => ("refuse", Some(reason.clone())),
    };
    SlicerInstallation {
        kind: installation.kind.as_str().to_owned(),
        name: installation.display_name(),
        path: Some(installation.path.clone()),
        found: Some(
            match installation.found {
                Found::Override => "configured",
                Found::SearchPath => "on PATH",
                Found::KnownLocation => "usual install folder",
            }
            .to_owned(),
        ),
        version: installation.version.as_ref().map(|v| v.as_str().to_owned()),
        stand_in: installation.fake,
        verdict: verdict.to_owned(),
        reason: reason.or_else(|| installation.note.clone()),
        fix: installation.verdict.is_refused().then(|| {
            "fix what the reason names, or install a version the policy allows; a refused \
             slicer is never run"
                .to_owned()
        }),
        usable: !installation.verdict.is_refused(),
        printers: printers.to_vec(),
    }
}

fn missing_row(
    kind: SlicerKind,
    printers: &[String],
    reason: Option<String>,
) -> SlicerInstallation {
    SlicerInstallation {
        kind: kind.as_str().to_owned(),
        name: kind.display_name().to_owned(),
        path: None,
        found: None,
        version: None,
        stand_in: false,
        verdict: "missing".to_owned(),
        reason,
        fix: Some(format!(
            "install {} yourself (Hotend never downloads a slicer); if it is not found, \
             choose its executable in the settings or set {}",
            kind.display_name(),
            kind.override_env()
        )),
        usable: false,
        printers: printers.to_vec(),
    }
}

impl SliceBackend for CoreSliceBackend {
    fn slice(&self, request: SliceRequest) -> BoxFuture<'_, Result<SliceSummary, SliceError>> {
        Box::pin(async move {
            self.run(request).await.map_err(|error| SliceError::Failed {
                code: error.code().to_owned(),
                message: error.to_string(),
            })
        })
    }

    /// [`CoreSliceBackend::export_project`] through the core's seam: the project and its
    /// test slice with paths relative to the request's folder. The stand-in answers
    /// `project_export_failed`, as [`Slicer::export_project`] does.
    fn export_project(
        &self,
        request: SliceRequest,
    ) -> BoxFuture<'_, Result<ProjectExportSummary, SliceError>> {
        Box::pin(async move {
            let failed = |error: SlicerError| SliceError::Failed {
                code: error.code().to_owned(),
                message: error.to_string(),
            };
            let output_dir = request.output_dir.clone();
            // The inherent method, not this one: it returns the slicer's own export.
            let export = CoreSliceBackend::export_project(self, request)
                .await
                .map_err(failed)?;
            to_project_summary(&output_dir, export).map_err(failed)
        })
    }

    /// The table of system profiles (`profiles/system-profiles.toml`): the process presets
    /// and materials Hotend maps for the printer, and the build plates. Read from the
    /// built-in table only; no installation is probed.
    fn catalogue(&self, printer_id: &str) -> Option<SlicerCatalogue> {
        catalogue(&SystemProfiles::builtin().ok()?, printer_id)
    }

    fn installations(&self, executables: &BTreeMap<String, PathBuf>) -> Vec<SlicerInstallation> {
        let slicer = self.slicer(executables, true);
        let printers_of = |kind: SlicerKind| -> Vec<String> {
            slicer
                .as_ref()
                .map(|slicer| {
                    slicer
                        .printers()
                        .printers
                        .iter()
                        .filter(|printer| printer.slicer == kind)
                        .map(|printer| printer.id.clone())
                        .collect()
                })
                .unwrap_or_default()
        };
        let mut rows = Vec::new();
        for kind in SlicerKind::ALL {
            let printers = printers_of(kind);
            let found = slicer
                .as_ref()
                .map_err(ToString::to_string)
                .and_then(|slicer| slicer.detect_kind(kind).map_err(|e| e.to_string()));
            match found {
                Ok(found) if found.is_empty() => rows.push(missing_row(kind, &printers, None)),
                Ok(found) => rows.extend(
                    found
                        .iter()
                        .map(|installation| installation_row(installation, &printers)),
                ),
                Err(reason) => rows.push(missing_row(kind, &printers, Some(reason))),
            }
        }
        rows
    }
}

/// What a job can name for `printer_id` in Hotend's table of system profiles, as the
/// core's catalogue; `None` for a printer the table does not list (a printer sliced with
/// another slicer).
pub fn catalogue(table: &SystemProfiles, printer_id: &str) -> Option<SlicerCatalogue> {
    let printer = table.printer(printer_id)?;
    Some(SlicerCatalogue {
        printer_id: printer.id.clone(),
        processes: printer
            .process
            .iter()
            .map(|preset| CatalogueEntry {
                id: preset.id.clone(),
                aliases: preset.aliases.clone(),
                name: preset.base.clone(),
                verified: preset.verified,
            })
            .collect(),
        materials: printer
            .material
            .iter()
            .map(|material| CatalogueEntry {
                id: material.id.clone(),
                aliases: Vec::new(),
                name: format!("{}{}", material.base, printer.filament_suffix),
                verified: material.verified,
            })
            .collect(),
        plates: table
            .bed_types
            .iter()
            .map(|bed| CatalogueEntry {
                id: bed.id.clone(),
                aliases: Vec::new(),
                name: bed.name.clone(),
                verified: bed.verified,
            })
            .collect(),
    })
}

/// Converts a result to the core's summary. The summary's numbers are not optional, so a
/// result with an unknown number is refused, naming what is missing.
pub fn to_summary(result: SliceResult) -> Result<SliceSummary, SlicerError> {
    let mut missing = Vec::new();
    if result.time_s.is_none() {
        missing.push("print time".to_owned());
    }
    if result.layers.is_none() {
        missing.push("layer count".to_owned());
    }
    let mut filament = Vec::new();
    for usage in &result.filament {
        match (usage.grams, usage.metres) {
            (Some(grams), Some(metres)) => filament.push(FilamentUse {
                slot: usage.slot,
                grams,
                metres,
                nozzle_id: usage.nozzle_id,
            }),
            _ => missing.push(format!("filament use of slot {}", usage.slot)),
        }
    }
    let (Some(time_s), Some(layers), true) = (result.time_s, result.layers, missing.is_empty())
    else {
        return Err(SlicerError::IncompleteResult(missing.join(", ")));
    };
    // The plate travels in the summary. The stand-in's file is not printable and is
    // labelled so; for a real slicer the user must also read which plate the file is for.
    let stand_in = result.warnings.iter().any(|w| w.code == "fake_slicer");
    let mut warnings = result.warnings;
    if let (Some(bed), false) = (&result.bed, stand_in) {
        let temperature = bed.temperature_c.map_or_else(
            || "an unstated bed temperature".to_owned(),
            |c| format!("a bed temperature of {c} C"),
        );
        warnings.push(crate::job::SliceWarning::new(
            "bed_plate",
            format!(
                "this file was sliced for the {} with {temperature}; that plate must be on \
                 the printer",
                bed.plate_type
            ),
        ));
    }
    Ok(SliceSummary {
        status: result.status,
        output: result.output,
        sha256: result.sha256,
        time_s,
        filament,
        layers,
        bed: result.bed.map(|bed| SliceBed {
            id: bed.id,
            plate_type: bed.plate_type,
            temperature_c: bed.temperature_c,
        }),
        warnings: warnings
            .into_iter()
            .map(|warning| CoreWarning {
                code: warning.code,
                message: warning.message,
            })
            .collect(),
        slicer: CoreSlicerInfo {
            name: result.slicer.name,
            version: result.slicer.version,
        },
    })
}

/// Converts a project export to the core's summary: the project and run C's sliced file
/// relative to `output_dir` (the job folder), run C as a [`SliceSummary`], and the proof's
/// verdict as a word.
pub fn to_project_summary(
    output_dir: &Path,
    export: crate::project::ProjectExport,
) -> Result<ProjectExportSummary, SlicerError> {
    let root = output_dir
        .canonicalize()
        .map_err(|e| SlicerError::io("find", output_dir, e))?;
    let project = export
        .project
        .canonicalize()
        .map_err(|e| SlicerError::io("find", &export.project, e))?;
    let relative = project
        .strip_prefix(&root)
        .ok()
        .and_then(|relative| relative.to_str())
        .map(|relative| relative.replace('\\', "/"))
        .ok_or_else(|| {
            SlicerError::InvalidJob("the editable project is not inside the job folder".into())
        })?;
    let proof = match export.proof.status {
        crate::proof::ProofStatus::Passed => "passed",
        crate::proof::ProofStatus::Failed => "failed",
        crate::proof::ProofStatus::Incomplete => "incomplete",
    };
    let stand_in = export
        .result
        .warnings
        .iter()
        .any(|w| w.code == "fake_slicer");
    let mut slice = to_summary(export.result)?;
    // Run C sliced inside the proof folder of the job folder.
    slice.output = format!("{}/{}", crate::slicer::PROOF_DIR, slice.output);
    // The keys as the project lists them (`different_settings_to_system`): without the keys
    // the window does not list, with the mirrors it lists beside them.
    let listed = |text: &str| -> Vec<String> {
        text.split(';')
            .filter(|key| !key.is_empty())
            .map(str::to_owned)
            .collect()
    };
    let [process, filament, printer] = export.keys.texts();
    Ok(ProjectExportSummary {
        project: relative,
        sha256: export.sha256,
        keys: CoreProjectKeys {
            process: listed(&process),
            filament: listed(&filament),
            printer: listed(&printer),
        },
        slice,
        proof: proof.to_owned(),
        stand_in,
    })
}

/// Removes a folder of ours (or whatever sits in its place, without following a link) and
/// creates it empty.
fn reset_dir(path: &Path) -> Result<(), SlicerError> {
    let removed = match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
        Ok(meta) if meta.file_type().is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
    };
    removed.map_err(|e| SlicerError::io("clear", path, e))?;
    std::fs::create_dir(path).map_err(|e| SlicerError::io("create", path, e))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::job::{FilamentUsage, SliceWarning, SlicerInfo};

    fn result() -> SliceResult {
        SliceResult {
            status: "ok".into(),
            output: "output/plate_1.gcode.3mf".into(),
            sha256: "ab".repeat(32),
            time_s: Some(473.0),
            filament: vec![FilamentUsage {
                slot: 1,
                grams: Some(2.6),
                metres: Some(0.87),
                nozzle_id: 1,
            }],
            layers: Some(50),
            bed: None,
            nozzle_temperature_c: None,
            extent: None,
            warnings: vec![SliceWarning::new("demo_profile", "x")],
            slicer: SlicerInfo {
                name: "Bambu Studio".into(),
                version: "02.05.03.00".into(),
            },
        }
    }

    /// The agent's `list_print_profiles` and the material check of the settings read this.
    #[test]
    fn the_catalogue_lists_the_presets_materials_and_plates_of_the_table() {
        let table = SystemProfiles::builtin().unwrap();
        let catalogue = catalogue(&table, "bambu-x2d").unwrap();
        let standard = catalogue
            .processes
            .iter()
            .find(|entry| entry.id == "standard-0.20mm")
            .unwrap();
        assert_eq!(standard.aliases, ["demo-0.20mm"]);
        assert!(standard.verified);
        assert!(catalogue.knows_material("pla-generic"));
        assert!(catalogue.knows_material("pla-basic"));
        assert!(!catalogue.knows_material("unobtainium"));
        assert!(catalogue.material_ids().contains("petg-basic"));
        let generic = catalogue
            .materials
            .iter()
            .find(|entry| entry.id == "pla-generic")
            .unwrap();
        assert_eq!(generic.name, "Generic PLA @BBL X2D 0.4 nozzle");
        assert!(
            catalogue
                .plates
                .iter()
                .any(|plate| plate.id == "textured-pei")
        );
        assert_eq!(super::catalogue(&table, "no-such-printer"), None);
        // The backend answers from the same table, without probing an installation.
        let backend = CoreSliceBackend::from_config(SlicerConfig::new().unwrap());
        assert_eq!(backend.catalogue("bambu-x2d"), Some(catalogue));
    }

    #[test]
    fn a_complete_result_becomes_a_summary_the_core_accepts() {
        let summary = to_summary(result()).unwrap();
        assert_eq!(summary.time_s, 473.0);
        assert_eq!(summary.layers, 50);
        assert_eq!(summary.filament[0].nozzle_id, 1);
        assert_eq!(summary.warnings[0].code, "demo_profile");
        // The JSON is the SliceResult of PROTOCOL.md section 6 without the fields the seam
        // does not carry yet, and the core's strict type reads it.
        let mut ours = serde_json::to_value(result()).unwrap();
        let theirs = serde_json::to_value(&summary).unwrap();
        for key in ["nozzle_temperature_c", "extent"] {
            assert!(ours.as_object_mut().unwrap().remove(key).is_some(), "{key}");
        }
        assert_eq!(ours, theirs);
        // A record written before the plate was part of a slice still loads.
        ours.as_object_mut().unwrap().remove("bed");
        let reparsed: SliceSummary = serde_json::from_value(ours).unwrap();
        assert_eq!(reparsed, summary);
    }

    #[test]
    fn the_plate_of_a_real_slice_reaches_the_user_as_a_warning() {
        let bed = crate::job::BedReport {
            id: "textured-pei".into(),
            plate_type: "Textured PEI Plate".into(),
            temperature_c: Some(55.0),
        };
        let mut real = result();
        real.warnings.clear();
        real.bed = Some(bed.clone());
        let summary = to_summary(real).unwrap();
        let carried = summary.bed.as_ref().unwrap();
        assert_eq!(
            (
                carried.id.as_str(),
                carried.plate_type.as_str(),
                carried.temperature_c
            ),
            ("textured-pei", "Textured PEI Plate", Some(55.0))
        );
        assert_eq!(summary.warnings.len(), 1);
        assert_eq!(summary.warnings[0].code, "bed_plate");
        assert_eq!(
            summary.warnings[0].message,
            "this file was sliced for the Textured PEI Plate with a bed temperature of 55 C; \
             that plate must be on the printer"
        );
        // The stand-in's file is labelled as not printable; its list of warnings stays as
        // it is.
        let mut stand_in = result();
        stand_in.bed = Some(bed);
        stand_in.warnings = vec![SliceWarning::new("fake_slicer", "x")];
        let summary = to_summary(stand_in).unwrap();
        assert_eq!(summary.warnings.len(), 1);
    }

    #[test]
    fn an_unknown_number_is_never_turned_into_a_value() {
        let mut no_time = result();
        no_time.time_s = None;
        let mut no_layers = result();
        no_layers.layers = None;
        let mut no_grams = result();
        no_grams.filament[0].grams = None;
        for (incomplete, word) in [
            (no_time, "print time"),
            (no_layers, "layer count"),
            (no_grams, "slot 1"),
        ] {
            let json = serde_json::to_value(&incomplete).unwrap();
            assert!(json.to_string().contains("null"));
            let error = to_summary(incomplete).unwrap_err();
            assert_eq!(error.code(), "incomplete_result");
            assert!(error.to_string().contains(word), "{error}");
        }
    }
}
