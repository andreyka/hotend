//! The slicer facade: [`Slicer::detect`] and [`Slicer::slice`] of PROTOCOL.md section 6.
//!
//! Order of a slice, each step a precondition of the next:
//! 1. the job is checked: paths, profile ids, the plate type, the printer's nozzle rules,
//!    and that a 3MF model file holds the model and nothing else ([`crate::input`]);
//! 2. an installed slicer of the printer's kind is chosen and the version policy applied;
//! 3. the settings are written into the job folder. For Bambu Studio they are the system
//!    profiles of the installation, flattened inside the sandbox ([`crate::system`]); for
//!    PrusaSlicer and for the stand-in slicer they are files chosen by id
//!    ([`crate::settings`]);
//! 4. the vendor command line runs inside the sandbox, with the job folder as its working
//!    directory. Nothing it prints is relied on;
//! 5. the slicer's own account of the run (`result.json`, Bambu Studio) is read as hostile
//!    input: a missing one or one that reports a failure ends the slice;
//! 6. the sliced file is checked, hashed and parsed as hostile input, and compared with
//!    what was asked for: the version policy (on the version the file states), the plate
//!    type and the bed temperature that goes with it, the positions of the parts, the
//!    reach of the nozzles, the filament map.
//!
//! A real Bambu Studio is never run for a plate nobody chose: the plate on the printer is
//! named by the job or by the settings, or the slice is refused.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use hotend_sandbox::{Limits, Profile};
use hotend_settings::PrintSettings;

use crate::command::{
    self, BedArgument, CONFIG_DIR, Invocation, Mode, OUTPUT_DIR, PROFILES_DIR, PROJECT_NAME, Plan,
};
use crate::detect::{self, Installation};
use crate::error::SlicerError;
use crate::flatten::{FlattenLimits, SettingItem};
use crate::job::{
    BedReport, Extent, FilamentUsage, PrintedExtent, SliceJob, SliceResult, SliceWarning,
    SlicerInfo, untrusted_text, valid_profile_id,
};
use crate::keymap;
use crate::output::result_json::{self, RESULT_NAME, SlicerReport};
use crate::output::{self, OutputLimits, Parsed};
use crate::printer::{FilamentMapRule, Printer, PrinterFacts};
use crate::project::{ProjectExport, ProjectKeys};
use crate::proof::{Baseline, Expected, GcodeFacts, ProofReport};
use crate::run::{RunOutput, run_sandboxed};
use crate::settings::{ProfileOrigin, ProfileRole, SettingsProfile, SettingsStore};
use crate::system::{self, BedType, ProfileItems, Selection, SystemProfiles};
use crate::version::{SlicerKind, SlicerVersion, Verdict, VersionPolicy};

/// Largest model file handed to a slicer.
const MAX_INPUT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Most model files in one job.
const MAX_INPUTS: usize = 16;
/// How far the printed extent may fall short of the box of the placed parts before the
/// slicer is taken to have moved them. A line is printed inside the outline of its part
/// (0.2 mm in the measured file), and a thin tip may not print at all; an arranged plate
/// differs by far more.
const POSITION_SLACK_MM: f64 = 3.0;
/// How far the printed extent may reach beyond the box of the placed parts. A brim (5 mm
/// wide in the measured settings), a skirt (2 mm away from the parts there) and supports
/// stand a little outside the parts; parts that were moved stand far outside.
const OUTSIDE_ALLOWANCE_MM: f64 = 15.0;
/// Slack when the printed extent is compared with a nozzle's reach, for rounding.
const REACH_SLACK_MM: f64 = 0.05;
/// How far the commanded bed temperature may differ from the plate's setting before it
/// is taken to be another temperature.
const BED_SLACK_C: f64 = 1.0;

/// How a [`Slicer`] finds programs and profiles and how far it lets a slicer run.
#[derive(Debug, Clone)]
pub struct SlicerConfig {
    /// Explicit executables from the application's settings. An entry here wins over the
    /// environment variable and stops any search for that slicer.
    pub executables: HashMap<SlicerKind, PathBuf>,
    /// The application's folder of vetted settings profiles, if it has one.
    pub profile_dir: Option<PathBuf>,
    /// The pinned-version table.
    pub policy: VersionPolicy,
    /// Sandbox limits of one slice; the wall-clock limit is the time cap.
    pub limits: Limits,
    /// Limits for reading the slicer's output.
    pub output_limits: OutputLimits,
    /// The folder of the installed Bambu Studio that holds the vendors' profile folders
    /// (`resources/profiles`). `None` looks for it next to the program
    /// ([`system::profile_folder`]). With it set, the stand-in slicer is given flattened
    /// system profiles too, as a real Bambu Studio always is.
    pub system_profiles: Option<PathBuf>,
    /// The plate that is on the printer (an id of `profiles/system-profiles.toml`), for a
    /// job that names none. With `None` such a job is refused for a real Bambu Studio
    /// (`bed_type_not_chosen`); only the stand-in slicer then slices for the table's
    /// `stand_in_bed_type`.
    pub default_bed_type: Option<String>,
    /// Limits for reading the system profiles.
    pub flatten_limits: FlattenLimits,
    /// The program that reads the installed system profiles inside the sandbox. `None` is
    /// `hotend-profile-flatten`, built with this crate and found next to the running
    /// program. Whatever is named here runs confined like the slicer.
    pub profile_reader: Option<PathBuf>,
}

impl SlicerConfig {
    /// The defaults: no explicit executables, demo profiles only, the built-in version
    /// policy and the sandbox's `slicer` limits.
    pub fn new() -> Result<Self, SlicerError> {
        Ok(Self {
            executables: HashMap::new(),
            profile_dir: None,
            policy: VersionPolicy::builtin()?,
            limits: Profile::Slicer.default_limits(),
            output_limits: OutputLimits::default(),
            system_profiles: None,
            default_bed_type: None,
            flatten_limits: FlattenLimits::default(),
            profile_reader: None,
        })
    }
}

/// Detects slicers and runs slices. Detection results are kept for the lifetime of the
/// value; make a new one to look again.
#[derive(Debug)]
pub struct Slicer {
    config: SlicerConfig,
    facts: PrinterFacts,
    store: SettingsStore,
    system: SystemProfiles,
    detected: Mutex<HashMap<SlicerKind, Vec<Installation>>>,
}

impl Slicer {
    pub fn new(config: SlicerConfig) -> Result<Self, SlicerError> {
        let store = match &config.profile_dir {
            Some(dir) => SettingsStore::with_dir(dir),
            None => SettingsStore::demo_only(),
        };
        Ok(Self {
            config,
            facts: PrinterFacts::builtin()?,
            store,
            system: SystemProfiles::builtin()?,
            detected: Mutex::new(HashMap::new()),
        })
    }

    /// The printer facts this slicer checks jobs against.
    pub fn printers(&self) -> &PrinterFacts {
        &self.facts
    }

    /// The settings profiles a job can name for PrusaSlicer and for the stand-in slicer.
    pub fn settings(&self) -> &SettingsStore {
        &self.store
    }

    /// The system profiles and plate types a Bambu Studio job can name.
    pub fn system_profiles(&self) -> &SystemProfiles {
        &self.system
    }

    /// Every installed slicer found on this machine, with its version and the policy's
    /// verdict. Each one's version is read by running `--help` inside the sandbox.
    pub fn detect(&self) -> Result<Vec<Installation>, SlicerError> {
        let mut all = Vec::new();
        for kind in SlicerKind::ALL {
            all.extend(self.detect_kind(kind)?);
        }
        Ok(all)
    }

    /// The installed slicers of one kind (see [`Slicer::detect`]).
    pub fn detect_kind(&self, kind: SlicerKind) -> Result<Vec<Installation>, SlicerError> {
        let mut cache = self.detected.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(known) = cache.get(&kind) {
            return Ok(known.clone());
        }
        let setting = self.config.executables.get(&kind).map(PathBuf::as_path);
        let found = detect::detect(kind, setting, &self.config.policy)?;
        cache.insert(kind, found.clone());
        Ok(found)
    }

    /// The installation of `kind` that slices will use: the first one found that the
    /// version policy does not refuse.
    pub fn installation(&self, kind: SlicerKind) -> Result<Installation, SlicerError> {
        let found = self.detect_kind(kind)?;
        if let Some(usable) = found.iter().find(|i| !i.verdict.is_refused()) {
            return Ok(usable.clone());
        }
        match found.first() {
            Some(refused) => Err(refusal(refused)),
            None => Err(SlicerError::NotInstalled {
                kind: kind.display_name(),
                env: kind.override_env(),
            }),
        }
    }

    /// Slices a job with the installed slicer of its printer's kind.
    pub fn slice(&self, job: &SliceJob) -> Result<SliceResult, SlicerError> {
        let printer = self.facts.printer(&job.printer)?;
        // Refuse a job the printer cannot print before looking for a program at all.
        printer.check_job(&job.filaments, &job.extents)?;
        let installation = self.installation(printer.slicer)?;
        self.slice_with(&installation, job)
    }

    /// Slices a job with a specific installation.
    pub fn slice_with(
        &self,
        installation: &Installation,
        job: &SliceJob,
    ) -> Result<SliceResult, SlicerError> {
        let setup = self.setup(installation, job, false)?;
        let invocation = self.invocation(&setup, &setup.job_dir, &setup.inputs, Mode::Slice);
        let run = self.execute(&setup, &setup.job_dir, &invocation)?;
        let (result, _) = self.finish(&setup, job, &setup.job_dir, &invocation, &run)?;
        Ok(result)
    }

    /// Makes the editable Bambu Studio project of a job with Hotend's print settings
    /// (runs A and C): run A exports a project from the flattened profiles
    /// with the settings and the nozzle inventory on top, without slicing; the list of changed
    /// settings is filled ([`crate::project::fill_difference_list`]); run C slices
    /// that file alone, in a folder of its own without the profiles, admitted only by the
    /// SHA-256 held in memory by this call; and the proof judges run C's sliced file. The
    /// project is returned only when that proof passed. A real Bambu Studio only.
    pub fn export_project(&self, job: &SliceJob) -> Result<ProjectExport, SlicerError> {
        let printer = self.facts.printer(&job.printer)?;
        printer.check_job(&job.filaments, &job.extents)?;
        let installation = self.installation(printer.slicer)?;
        self.export_project_with(&installation, job)
    }

    /// [`Slicer::export_project`] with a specific installation.
    pub fn export_project_with(
        &self,
        installation: &Installation,
        job: &SliceJob,
    ) -> Result<ProjectExport, SlicerError> {
        if installation.kind != SlicerKind::BambuStudio || installation.fake {
            return Err(SlicerError::Project {
                code: "project_export_failed",
                message: "only a real Bambu Studio makes an editable project; the stand-in \
                          slicer makes none"
                    .into(),
            });
        }
        if job.print_settings.is_none() {
            return Err(SlicerError::InvalidJob(
                "an editable project carries Hotend's print settings, and this job has none; \
                 plan the plate's print settings first"
                    .into(),
            ));
        }
        let setup = self.setup(installation, job, true)?;
        let job_dir = setup.job_dir.clone();

        // Run A: the project, without slicing.
        let invocation = self.invocation(&setup, &job_dir, &setup.inputs, Mode::ExportProject);
        let run = self.execute(&setup, &job_dir, &invocation)?;
        let failed = |detail: String| SlicerError::Project {
            code: "project_export_failed",
            message: format!(
                "Bambu Studio wrote no editable project ({detail}); nothing was handed over"
            ),
        };
        let result_path = format!("{OUTPUT_DIR}/{RESULT_NAME}");
        if let Some(report) = result_json::read(&job_dir, &result_path)?
            && !report.succeeded()
        {
            return Err(failed(format!(
                "the program reported return code {}",
                report.return_code
            )));
        }
        if run.code != Some(0) {
            return Err(failed(format!("exit code {:?}", run.code)));
        }
        let limits = &self.config.output_limits;
        let project_path = hotend_core::import::resolve_inside(
            &job_dir,
            &format!("{OUTPUT_DIR}/{PROJECT_NAME}"),
            limits.max_file_bytes,
        )
        .map_err(|e| failed(format!("no usable `{PROJECT_NAME}`: {e}")))?;
        let exported = read_capped(&project_path, limits.max_file_bytes)?;

        // The list of changed settings, from what the helper reported as changed.
        let prepared = setup
            .prepared
            .as_ref()
            .ok_or_else(|| failed("no profiles".into()))?;
        let keys = ProjectKeys {
            process: prepared.changed_keys(keymap::Role::Process),
            filament: prepared.changed_keys(keymap::Role::Filament),
            printer: Vec::new(),
        };
        let filled = crate::project::fill_difference_list(&exported, &keys, limits)?;
        let sha256 = output::sha256_hex(&filled);

        // Run C: the filled project alone, in a folder without the profiles.
        let proof_dir = job_dir.join(PROOF_DIR);
        reset_folder(&proof_dir)?;
        for name in ["input", OUTPUT_DIR] {
            let path = proof_dir.join(name);
            std::fs::create_dir(&path).map_err(|e| SlicerError::io("create", &path, e))?;
        }
        let project_rel = format!("input/{PROJECT_NAME}");
        let project = proof_dir.join(&project_rel);
        std::fs::write(&project, &filled).map_err(|e| SlicerError::io("write", &project, e))?;
        // The only way a project is admitted as a slicer's input: its hash equals the one this
        // call computed for the bytes it wrote.
        if output::sha256_hex(&read_capped(&project, limits.max_file_bytes)?) != sha256 {
            return Err(failed(
                "the project changed on disk before it was sliced".into(),
            ));
        }
        let inputs = vec![project_rel];
        let invocation = self.invocation(&setup, &proof_dir, &inputs, Mode::ResliceProject);
        let run = self.execute(&setup, &proof_dir, &invocation)?;
        let (result, proof) = self
            .finish(&setup, job, &proof_dir, &invocation, &run)
            .map_err(|error| match error.code() {
                "slicer_failed" | "result_missing" | "unusable_output" => SlicerError::Project {
                    code: "project_not_self_contained",
                    message: format!(
                        "the editable project did not slice on its own ({error}). The first \
                         suspect is the nozzle inventory of the machine settings; nothing was \
                         handed over"
                    ),
                },
                _ => error,
            })?;
        let proof = proof.ok_or_else(|| failed("the proof did not run".into()))?;
        // Handed over only when the bytes are still the ones that were sliced and proven.
        if output::sha256_hex(&read_capped(&project, limits.max_file_bytes)?) != sha256 {
            return Err(failed(
                "the project changed on disk after it was sliced".into(),
            ));
        }
        Ok(ProjectExport {
            project,
            sha256,
            keys,
            proof,
            result,
        })
    }

    /// Everything before a run: the checks of the job, the plate, the settings files (for
    /// Bambu Studio the flattened system profiles with the plate, the preset's overrides and
    /// the plan's settings on top; `inventory` adds the nozzle inventory of a project export).
    fn setup<'a>(
        &'a self,
        installation: &'a Installation,
        job: &'a SliceJob,
        inventory: bool,
    ) -> Result<Setup<'a>, SlicerError> {
        let kind = installation.kind;
        let printer = self.facts.printer(&job.printer)?;
        if printer.slicer != kind {
            return Err(SlicerError::InvalidJob(format!(
                "{} is sliced with {}, not {}",
                printer.name,
                printer.slicer.display_name(),
                kind.display_name()
            )));
        }
        if installation.verdict.is_refused() {
            return Err(refusal(installation));
        }
        printer.check_job(&job.filaments, &job.extents)?;
        let filament_map = match &printer.filament_map {
            Some(rule) => Some((rule, printer.filament_map(&job.filaments)?)),
            None => None,
        };
        if !(1..=64).contains(&job.plate) {
            return Err(SlicerError::InvalidJob("the plate must be 1 to 64".into()));
        }
        if kind == SlicerKind::PrusaSlicer && (job.plate != 1 || job.filaments.len() != 1) {
            return Err(SlicerError::InvalidJob(
                "PrusaSlicer jobs have one plate and one filament in this version".into(),
            ));
        }
        // The fitted nozzle: a size without a system profile is not sliced with another's.
        if kind == SlicerKind::BambuStudio
            && let (Some(fitted), Some(table)) =
                (job.nozzle_diameter_mm, self.system.printer(&job.printer))
        {
            let ours: Option<f64> = table.nozzle.parse().ok();
            if !fitted.is_finite() || ours.is_none_or(|ours| (ours - fitted).abs() > 1e-6) {
                return Err(SlicerError::NozzleNotSupported {
                    fitted: if fitted.is_finite() {
                        crate::keymap::decimal_text(fitted)
                    } else {
                        "unknown".to_owned()
                    },
                    printer: printer.name.clone(),
                    available: table.nozzle.clone(),
                });
            }
        }
        let job_dir = check_job_dir(&job.job_dir)?;
        if kind == SlicerKind::BambuStudio && job_dir.to_string_lossy().contains(';') {
            return Err(SlicerError::InvalidJob(
                "the job folder path contains a semicolon, which Bambu Studio's settings \
                 lists cannot carry"
                    .into(),
            ));
        }
        let inputs = check_inputs(&job_dir, &job.inputs, kind, &self.config.output_limits)?;
        // What follows a run of the vendor's own program is stricter than what follows the
        // stand-in, whose file cannot be printed.
        let real_bambu = kind == SlicerKind::BambuStudio && !installation.fake;
        let mut warnings = Vec::new();

        // Hotend's print settings: used with a real Bambu Studio only. The stand-in's file
        // is not printable and is labelled so; it slices as it always has.
        let settings = job.print_settings.as_ref().filter(|_| real_bambu);
        if real_bambu && settings.is_none() {
            warnings.push(SliceWarning::new(
                "print_settings_missing",
                "this slice was made with the installed slicer profiles as they are, without \
                 Hotend's print settings for the plate (brim, first layer, bed and fans); \
                 Hotend has not checked it against a print plan",
            ));
        }
        let mut process_id = job.process_profile.clone();
        let mut material_ids: Vec<String> = job
            .filaments
            .iter()
            .map(|f| f.filament_profile.clone())
            .collect();
        let mut plan_bed: Option<String> = None;
        if let Some(settings) = settings {
            let filament = match (settings.filaments(), job.filaments.as_slice()) {
                ([filament], [slot]) => {
                    if !filament.family.as_str().eq_ignore_ascii_case(&slot.family) {
                        return Err(SlicerError::InvalidJob(format!(
                            "the print settings are for {}, but the job's filament is {}",
                            filament.family,
                            untrusted_text(&slot.family, 20)
                        )));
                    }
                    filament
                }
                _ => {
                    return Err(SlicerError::InvalidJob(
                        "print settings are for a plate with one filament, and this job does \
                         not have exactly one"
                            .into(),
                    ));
                }
            };
            let process = settings.process().ok_or_else(|| {
                SlicerError::InvalidJob("the print settings name no process preset".into())
            })?;
            if process.as_str() != job.process_profile
                && self
                    .system
                    .select(&job.printer, &job.process_profile, &[])
                    .ok()
                    .zip(self.system.select(&job.printer, process.as_str(), &[]).ok())
                    .is_none_or(|(asked, planned)| asked.process != planned.process)
            {
                warnings.push(SliceWarning::new(
                    "process_from_plan",
                    format!(
                        "the print plan uses the process preset `{process}`, so it was used \
                         instead of `{}`",
                        untrusted_text(&job.process_profile, 60)
                    ),
                ));
            }
            process_id = process.as_str().to_owned();
            if job.filaments[0].filament_profile != filament.material_id.as_str() {
                warnings.push(SliceWarning::new(
                    "material_from_plan",
                    format!(
                        "the print plan is for the material `{}`, so its profile was used \
                         instead of `{}`",
                        filament.material_id,
                        untrusted_text(&job.filaments[0].filament_profile, 60)
                    ),
                ));
            }
            material_ids = vec![filament.material_id.as_str().to_owned()];
            let plate = settings.plate_type().ok_or_else(|| {
                SlicerError::InvalidJob("the print settings name no plate type".into())
            })?;
            if let Some(asked) = &job.bed_type
                && asked != plate.as_str()
            {
                return Err(SlicerError::InvalidJob(format!(
                    "the print settings are for the plate `{plate}`, but the slice was asked \
                     for `{}`; choose one plate in the settings and plan again",
                    untrusted_text(asked, 40)
                )));
            }
            plan_bed = Some(plate.as_str().to_owned());
        }

        // The plate type: Bambu Studio's own default is the Cool Plate with a 35 C bed,
        // whatever plate is on the printer, so the job always states one. Which plate is
        // on the printer is not something Hotend can know or should guess.
        let chosen_bed = job
            .bed_type
            .as_deref()
            .or(plan_bed.as_deref())
            .or(self.config.default_bed_type.as_deref());
        let bed: Option<&BedType> = match kind {
            SlicerKind::BambuStudio => Some(match chosen_bed {
                Some(id) => self.system.bed_type(id)?,
                None if real_bambu => {
                    return Err(SlicerError::BedTypeNotChosen {
                        known: self.system.bed_type_ids().join(", "),
                    });
                }
                None => self.system.bed_type(&self.system.stand_in_bed_type)?,
            }),
            SlicerKind::PrusaSlicer if job.bed_type.is_some() => {
                return Err(SlicerError::InvalidJob(
                    "PrusaSlicer has no plate types; the bed temperature is in its filament \
                     settings"
                        .into(),
                ));
            }
            SlicerKind::PrusaSlicer => None,
        };

        // Settings. A Bambu Studio is given the system profiles of the installation,
        // flattened; everything else gets files chosen by id.
        for id in std::iter::once(&process_id).chain(material_ids.iter()) {
            if !valid_profile_id(id) {
                return Err(SlicerError::InvalidJob(format!(
                    "`{}` is not a valid settings profile id",
                    untrusted_text(id, 60)
                )));
            }
        }
        let system_folder = match kind {
            SlicerKind::BambuStudio => {
                system::profile_folder(installation, self.config.system_profiles.as_deref())
            }
            SlicerKind::PrusaSlicer => None,
        };
        let selection = match (&system_folder, bed) {
            (Some(_), Some(_)) => {
                let materials: Vec<&str> = material_ids.iter().map(String::as_str).collect();
                Some(self.system.select(&job.printer, &process_id, &materials)?)
            }
            _ if real_bambu => {
                return Err(SlicerError::ProfileNotFound {
                    kind: kind.display_name(),
                    role: "machine",
                    id: job.printer.clone(),
                    searched: format!(
                        "the `resources/profiles` folder of the installation, which was not \
                         found next to {}. Set the folder in the settings",
                        installation.path.display()
                    ),
                });
            }
            _ => None,
        };
        let mut stored: Vec<(String, SettingsProfile)> = Vec::new();
        if selection.is_none() {
            stored.push((
                command::settings_path(kind, "machine", None),
                self.store.load(kind, ProfileRole::Machine, &job.printer)?,
            ));
            stored.push((
                command::settings_path(kind, "process", None),
                self.store
                    .load(kind, ProfileRole::Process, &job.process_profile)?,
            ));
            for filament in &job.filaments {
                stored.push((
                    command::settings_path(kind, "filament", Some(filament.slot)),
                    self.store
                        .load(kind, ProfileRole::Filament, &filament.filament_profile)?,
                ));
            }
        }
        let demo = stored
            .iter()
            .any(|(_, profile)| profile.origin == ProfileOrigin::Demo);

        prepare_job_dir(&job_dir)?;
        let mut prepared = None;
        if let (Some(selection), Some(folder), Some(bed)) = (&selection, &system_folder, bed) {
            let file_name = |role: &str, slot: Option<u32>| {
                let path = command::settings_path(kind, role, slot);
                path.rsplit('/').next().unwrap_or_default().to_owned()
            };
            let mut outputs = vec![file_name("machine", None), file_name("process", None)];
            outputs.extend(
                job.filaments
                    .iter()
                    .map(|filament| file_name("filament", Some(filament.slot))),
            );
            let mut items = ProfileItems {
                process: selection.base_items(bed)?,
                filament: Vec::new(),
                inventory,
            };
            if let Some(settings) = settings {
                for item in keymap::Keymap::builtin()?.items(settings, bed)? {
                    if item.key == "curr_bed_type" {
                        continue; // the plate is already one of the base items
                    }
                    let setting = SettingItem {
                        key: item.key,
                        op: item.op,
                        text: item.text,
                        absent: item.absent,
                    };
                    match item.role {
                        keymap::Role::Process => items.process.push(setting),
                        keymap::Role::Filament => items.filament.push(setting),
                        keymap::Role::Machine => {}
                    }
                }
            }
            prepared = Some(system::flatten_into_job(
                &job_dir,
                folder,
                selection,
                &items,
                settings,
                &outputs,
                &self.config.flatten_limits,
                self.config.profile_reader.as_deref(),
            )?);
        } else {
            for (relative, profile) in &stored {
                let path = job_dir.join(relative);
                std::fs::write(&path, &profile.content)
                    .map_err(|e| SlicerError::io("write", &path, e))?;
            }
        }
        Ok(Setup {
            installation,
            kind,
            printer,
            job_dir,
            inputs,
            real_bambu,
            bed,
            selection,
            prepared,
            demo,
            filament_map,
            settings,
            warnings,
            plate: job.plate,
            arrange: job.arrange,
            filament_slots: job.filaments.len(),
        })
    }

    /// The command line of one run in `job_dir` on `inputs`.
    fn invocation(
        &self,
        setup: &Setup<'_>,
        job_dir: &Path,
        inputs: &[String],
        mode: Mode,
    ) -> Invocation {
        // The plate option is passed only where the plate is not in the process settings:
        // the stand-in's stored demo profiles. A flattened process profile carries it as
        // `curr_bed_type` (measured), and the option is passed there only if the table says
        // so.
        let pass_option = self.system.bed_type_option.used || setup.selection.is_none();
        command::build(
            setup.kind,
            &Plan {
                job_dir,
                inputs,
                filament_slots: setup.filament_slots,
                filament_map: setup
                    .filament_map
                    .as_ref()
                    .map(|(rule, numbers)| (*rule, numbers.as_slice())),
                plate: setup.plate,
                arrange: setup.arrange,
                bed_type: setup.bed.filter(|_| pass_option).map(|bed| BedArgument {
                    option: &self.system.bed_type_option.option,
                    value: &bed.slicer_value,
                    option_verified: self.system.bed_type_option.verified,
                    value_verified: bed.verified,
                }),
                mode,
            },
        )
    }

    /// Runs the program once, confined, in `job_dir`.
    fn execute(
        &self,
        setup: &Setup<'_>,
        job_dir: &Path,
        invocation: &Invocation,
    ) -> Result<RunOutput, SlicerError> {
        // A file left where the output is expected must not be mistaken for this run's.
        for candidate in &invocation.output_candidates {
            let path = job_dir.join(candidate);
            if let Ok(meta) = std::fs::symlink_metadata(&path) {
                let removed = if meta.file_type().is_dir() {
                    std::fs::remove_dir_all(&path)
                } else {
                    std::fs::remove_file(&path)
                };
                removed.map_err(|e| SlicerError::io("clear", &path, e))?;
            }
        }
        let run = run_sandboxed(
            job_dir,
            &setup.installation.read_only,
            self.config.limits,
            &setup.installation.path,
            &invocation.args,
        )?;
        match run.code {
            Some(124) => Err(SlicerError::Timeout {
                seconds: self.config.limits.wall_seconds,
            }),
            Some(125) => Err(SlicerError::Sandbox(
                hotend_sandbox::SandboxError::Unavailable(untrusted_text(&run.text(), 600)),
            )),
            _ => Ok(run),
        }
    }

    /// Everything after a run: the slicer's own account, the sliced file read as hostile
    /// input and compared with the job, and with print settings the proof.
    fn finish(
        &self,
        setup: &Setup<'_>,
        job: &SliceJob,
        job_dir: &Path,
        invocation: &Invocation,
        run: &RunOutput,
    ) -> Result<(SliceResult, Option<ProofReport>), SlicerError> {
        let kind = setup.kind;
        let installation = setup.installation;
        let printer = setup.printer;
        let real_bambu = setup.real_bambu;
        let bed = setup.bed;
        let selection = &setup.selection;
        let prepared = &setup.prepared;
        let mut warnings = setup.warnings.clone();
        // What the program printed is never needed to decide anything (the Windows build of
        // Bambu Studio prints nothing at all); it is only added to an error for the reader.
        let tail = || {
            let text = untrusted_text(&run.text(), usize::MAX);
            if text.is_empty() {
                return "The program printed nothing.".to_owned();
            }
            let skip = text.chars().count().saturating_sub(600);
            let tail: String = text.chars().skip(skip).collect();
            format!("slicer output (untrusted): {tail}")
        };
        // For the error of a real run that fails without saying why: what in this run no
        // real program has been seen to accept.
        let suspects = || {
            let mut list = invocation.unconfirmed.clone();
            if let Some(selection) = selection {
                list.extend(selection.unverified.iter().map(|name| {
                    format!("the settings of `{name}` (this profile has not been sliced with)")
                }));
            }
            if !real_bambu || list.is_empty() {
                return String::new();
            }
            format!(
                " Not yet confirmed by a run of a real Bambu Studio, and so the first \
                 suspects: {}. Run the program by hand with and without them (the README \
                 of hotend-slicer has the steps).",
                list.join("; ")
            )
        };
        // Bambu Studio's own account of the run comes first: it is the only thing that
        // says whether the slice worked.
        let result_path = format!("{OUTPUT_DIR}/{RESULT_NAME}");
        let report: Option<SlicerReport> = match kind {
            SlicerKind::BambuStudio => result_json::read(job_dir, &result_path)?,
            SlicerKind::PrusaSlicer => None,
        };
        if let Some(report) = report.as_ref().filter(|report| !report.succeeded()) {
            let words = if report.error.is_empty() {
                "it gave no message.".to_owned()
            } else {
                format!("{} (the slicer's words, untrusted).", report.error)
            };
            return Err(SlicerError::Reported {
                return_code: report.return_code,
                message: format!("{words}{}", suspects()),
            });
        }
        match run.code {
            Some(0) => {}
            Some(code) => {
                return Err(SlicerError::Failed {
                    status: format!("exit code {code}"),
                    stderr_tail: format!("{}{}", tail(), suspects()),
                });
            }
            None => {
                return Err(SlicerError::Failed {
                    status: "the sandbox launcher was killed".into(),
                    stderr_tail: format!("{}{}", tail(), suspects()),
                });
            }
        }
        if kind == SlicerKind::BambuStudio && report.is_none() {
            return Err(SlicerError::ResultMissing {
                expected: result_path,
                detail: format!("{}{}", tail(), suspects()),
            });
        }

        let limits = &self.config.output_limits;
        // Hotend-made plate previews, when asked for and the slicer drew none: put in
        // before the file is hashed, parsed and proven, so all of that is of the final file.
        let mut previews_added = false;
        if let (Some(previews), SlicerKind::BambuStudio) = (&job.plate_previews, kind)
            && let Some((relative, file)) =
                output::open_output(job_dir, &invocation.output_candidates, limits)?
        {
            drop(file);
            previews_added =
                crate::previews::add(&job_dir.join(&relative), job.plate, previews, limits)?;
        }
        let Some((relative, mut file)) =
            output::open_output(job_dir, &invocation.output_candidates, limits)?
        else {
            return Err(SlicerError::UnusableOutput(format!(
                "the slicer exited normally but wrote no `{}`. {}",
                invocation.output_candidates[0],
                tail()
            )));
        };
        let sha256 = output::sha256_of(&mut file, Path::new(&relative))?;
        let mut parsed = match kind {
            SlicerKind::BambuStudio => output::bambu::parse(
                &mut file,
                job.plate,
                printer
                    .filament_map
                    .as_ref()
                    .map(|rule| rule.result_maps_key.as_str()),
                &self.system.bed_types,
                limits,
            )?,
            SlicerKind::PrusaSlicer => output::prusa::parse(&mut file, limits)?,
        };
        // The stand-in's file prints nothing and says so; what the stand-in reports for
        // the model are its estimates, which it writes apart from the file's own claims.
        if installation.fake {
            parsed.use_stand_in_estimate();
        }

        // The version. The file states which program sliced it, and that statement is what
        // the version policy judges, whatever the program said when it was asked: a
        // program that prints nothing cannot be asked at all, and one that was asked may
        // not be the one that sliced.
        let mut verdict = installation.verdict.clone();
        let mut version = installation.version_text();
        if real_bambu {
            let stated = parsed
                .slicer_version
                .as_deref()
                .and_then(SlicerVersion::parse);
            verdict = self.config.policy.verdict(kind, stated.as_ref());
            let stated_text = stated
                .as_ref()
                .map_or_else(|| "unknown".to_owned(), |v| v.as_str().to_owned());
            if let Verdict::Refuse(reason) = &verdict {
                return Err(SlicerError::VersionRefused {
                    kind: kind.display_name(),
                    version: stated_text,
                    reason: reason.clone(),
                });
            }
            if let Some(probed) = &installation.version
                && stated
                    .as_ref()
                    .is_none_or(|stated| probed.cmp(stated) != std::cmp::Ordering::Equal)
            {
                warnings.push(SliceWarning::new(
                    "slicer_version_mismatch",
                    match &stated {
                        Some(stated) => format!(
                            "the program said it is version {probed}, the file it sliced \
                             says {stated}; the file's statement is the one that is judged"
                        ),
                        None => format!(
                            "the program said it is version {probed}, but the file it \
                             sliced states no version"
                        ),
                    },
                ));
            }
            version = stated_text;
        }
        match &verdict {
            Verdict::Warn(reason) if installation.fake => {
                warnings.insert(0, SliceWarning::new("fake_slicer", reason.clone()));
            }
            Verdict::Warn(reason) => {
                warnings.insert(0, SliceWarning::new("slicer_version", reason.clone()));
            }
            _ => {}
        }
        if previews_added {
            warnings.push(SliceWarning::new(
                crate::previews::WARNING_CODE,
                crate::previews::warning_message(),
            ));
        }
        if setup.demo {
            warnings.push(SliceWarning::new(
                "demo_profile",
                "sliced with a built-in demo settings profile, which is generic and untuned; do \
                 not print this file",
            ));
        }
        if let Some(prepared) = prepared
            && !prepared.compatibility_unstated.is_empty()
        {
            warnings.push(SliceWarning::new(
                "profile_compatibility",
                format!(
                    "these installed profiles do not say which printers they are for: {}",
                    prepared.compatibility_unstated.join(", ")
                ),
            ));
        }
        if let Some(prepared) = prepared
            && !prepared.dropped.is_empty()
        {
            warnings.push(SliceWarning::new(
                "profile_keys_dropped",
                format!(
                    "the installed profiles carried G-code or script settings without \
                     content in a place where only the machine profile may set them; they \
                     were left out: {}",
                    prepared.dropped.join(", ")
                ),
            ));
        }
        if let Some(selection) = selection
            && !selection.unverified.is_empty()
        {
            warnings.push(SliceWarning::new(
                "unverified_profile_names",
                format!(
                    "these system profile names follow the vendor's pattern but have not been \
                     checked against a real installation by Hotend's authors: {}",
                    selection.unverified.join(", ")
                ),
            ));
        }
        if let Some(prepared) = prepared {
            let list: Vec<String> = prepared
                .overrides
                .iter()
                .map(|(key, value)| format!("{key} = {value}"))
                .collect();
            warnings.push(SliceWarning::new(
                "profile_overrides",
                format!(
                    "set by Hotend on top of the installed system profiles: {}",
                    list.join(", ")
                ),
            ));
        }
        if job.extents.is_empty() {
            warnings.push(SliceWarning::new(
                "reach_unchecked",
                "no part sizes were given, so the nozzle reach was not checked before slicing",
            ));
        }
        let unverified = printer.unverified_facts();
        if !unverified.is_empty() {
            warnings.push(SliceWarning::new(
                "unverified_printer_facts",
                format!(
                    "these facts about {} have not been confirmed on a real printer: {}",
                    printer.name,
                    unverified.join(", ")
                ),
            ));
        }

        // What the file was sliced for, against what was asked.
        let bed_report = match bed {
            Some(bed) => Some(check_bed(bed, &parsed, &mut warnings)?),
            None => None,
        };
        if kind == SlicerKind::BambuStudio {
            check_printed_extent(
                job,
                printer,
                parsed.extent.as_ref(),
                real_bambu,
                &mut warnings,
            )?;
        }
        if let Some(report) = &report {
            compare_report(report, job.plate, &parsed, &mut warnings);
        }
        let nozzle_temperature_c = parsed.nozzle_temperature_c;
        let extent = parsed.extent.take();
        let density_missing = parsed.density_missing;
        let map_numbers = setup
            .filament_map
            .as_ref()
            .map(|(_, numbers)| numbers.clone());
        let filament = assemble(job, printer, map_numbers.as_deref(), parsed, &mut warnings)?;
        // Measured with a system profile that was not flattened: the slice "succeeds" with
        // a filament density of 0 and a weight of 0.00 g. Such a file was sliced with
        // incomplete settings and is not used.
        // (A part of a few cubic millimetres also rounds to 0.00 g; a centimetre of filament
        // does not, at any real density.)
        let unweighed = filament
            .usage
            .iter()
            .any(|u| u.grams == Some(0.0) && u.metres.is_some_and(|metres| metres >= 0.01));
        if density_missing || unweighed {
            return Err(SlicerError::UnusableOutput(
                "the sliced file states a filament density or weight of 0 for filament that \
                 is used. The slicer was given incomplete filament settings (a system \
                 profile that is not flattened does this), so the file is not used"
                    .into(),
            ));
        }

        // The proof: the file must show the settings Hotend asked for.
        let proof = match (setup.settings, bed, prepared) {
            (Some(settings), Some(bed), Some(prepared)) => {
                let written = prepared
                    .applied
                    .iter()
                    .map(|(_, applied)| (applied.key.clone(), applied.new.clone()))
                    .collect();
                let expected = Expected {
                    settings: settings.clone(),
                    bed: bed.clone(),
                    written: Some(written),
                    baseline: Baseline::builtin()?,
                };
                let facts = GcodeFacts::builtin()?;
                let path = job_dir.join(&relative);
                let file =
                    std::fs::File::open(&path).map_err(|e| SlicerError::io("open", &path, e))?;
                let scan = output::bambu::scan_for_proof(file, job.plate, facts, limits)?;
                let report = crate::proof::judge(
                    &expected,
                    &scan,
                    keymap::Keymap::builtin()?,
                    facts,
                    &self.system.bed_types,
                );
                if !report.passed() {
                    return Err(report.error());
                }
                warnings.push(SliceWarning::new(
                    "print_settings_proven",
                    "the sliced file states every print setting Hotend asked for, and its \
                     first layer was checked: bed, fans, speeds and brim",
                ));
                Some(report)
            }
            _ => None,
        };
        Ok((
            SliceResult {
                status: "ok".to_owned(),
                output: relative,
                sha256,
                time_s: filament.time_s,
                filament: filament.usage,
                layers: filament.layers,
                bed: bed_report,
                nozzle_temperature_c,
                extent,
                warnings,
                slicer: SlicerInfo {
                    name: installation.display_name(),
                    version,
                },
            },
            proof,
        ))
    }
}

/// What [`Slicer::setup`] prepared for the runs of one job.
struct Setup<'a> {
    installation: &'a Installation,
    kind: SlicerKind,
    printer: &'a Printer,
    job_dir: PathBuf,
    inputs: Vec<String>,
    real_bambu: bool,
    bed: Option<&'a BedType>,
    selection: Option<Selection>,
    prepared: Option<system::Prepared>,
    demo: bool,
    filament_map: Option<(&'a FilamentMapRule, Vec<u32>)>,
    /// The print settings that were put on top of the profiles: a real Bambu Studio only.
    settings: Option<&'a PrintSettings>,
    warnings: Vec<SliceWarning>,
    plate: u32,
    arrange: bool,
    filament_slots: usize,
}

/// Folder below the job folder of run C: the filled project and what the run writes.
pub const PROOF_DIR: &str = "proof";

/// Reads a file of the job folder, at most `cap` bytes.
fn read_capped(path: &Path, cap: u64) -> Result<Vec<u8>, SlicerError> {
    let file = std::fs::File::open(path).map_err(|e| SlicerError::io("open", path, e))?;
    let mut bytes = Vec::new();
    file.take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| SlicerError::io("read", path, e))?;
    if bytes.len() as u64 > cap {
        return Err(SlicerError::UnusableOutput(format!(
            "{} is larger than {cap} bytes",
            path.display()
        )));
    }
    Ok(bytes)
}

/// Removes a folder of ours (or whatever sits in its place, without following a link) and
/// creates it empty.
fn reset_folder(path: &Path) -> Result<(), SlicerError> {
    let removed = match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
        Ok(meta) if meta.file_type().is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
    };
    removed.map_err(|e| SlicerError::io("clear", path, e))?;
    std::fs::create_dir(path).map_err(|e| SlicerError::io("create", path, e))
}

/// Compares the plate type the file states, and the bed temperature its commands set,
/// with the plate the job asked for. What is reported is the file's own statement.
fn check_bed(
    bed: &BedType,
    parsed: &Parsed,
    warnings: &mut Vec<SliceWarning>,
) -> Result<BedReport, SlicerError> {
    let stated = match &parsed.bed_type {
        Some(got) if *got == bed.slicer_value => got.clone(),
        Some(got) => {
            return Err(SlicerError::BedTypeNotApplied {
                asked: bed.name.clone(),
                got: format!("says it was sliced for `{got}`"),
            });
        }
        // Nothing in such a file says it was sliced for the plate that was asked for. The
        // program's own default is another plate.
        None => {
            return Err(SlicerError::BedTypeNotApplied {
                asked: bed.name.clone(),
                got: "states no plate type at all".into(),
            });
        }
    };
    let problem = |problem: String| SlicerError::BedTemperature {
        plate: bed.name.clone(),
        problem,
    };
    match (parsed.bed_commanded_c, parsed.bed_setting_c) {
        (Some(commanded), Some([_, high])) if commanded > high + BED_SLACK_C => {
            return Err(problem(format!(
                "its commands heat the bed to {commanded} C although its own settings give \
                 {high} C for that plate"
            )));
        }
        (Some(commanded), Some([low, _])) if commanded < low - BED_SLACK_C => {
            warnings.push(SliceWarning::new(
                "bed_temperature_low",
                format!(
                    "the sliced file heats the bed to {commanded} C at most, below the \
                     {low} C its own settings give for the {}; the print may not stick",
                    bed.name
                ),
            ));
        }
        (Some(_), Some(_)) => {}
        (Some(commanded), None) => {
            return Err(problem(format!(
                "its commands heat the bed to {commanded} C, but its settings do not state \
                 the temperature of that plate (`{}`), so the commands cannot be checked",
                bed.temperature_key
            )));
        }
        // A file without a bed command: the setting is what it has to say.
        (None, Some(_)) => {}
        (None, None) => warnings.push(SliceWarning::new(
            "bed_temperature_unknown",
            "the sliced file does not state a bed temperature",
        )),
    }
    Ok(BedReport {
        id: bed.id.clone(),
        plate_type: stated,
        temperature_c: parsed.bed_temperature_c,
    })
}

/// Compares where the sliced file prints with where the nozzles can go and, when the
/// slicer was told to keep the parts where the plate has them, with those positions.
///
/// - The printed box must lie inside the reach of the nozzle that prints (inside the box
///   around the reaches when the job uses several nozzles). The reach was checked on the
///   parts before slicing; the file is what the printer runs.
/// - With `arrange` off, the printed box must cover the box of the placed parts to within
///   [`POSITION_SLACK_MM`] and must not reach more than [`OUTSIDE_ALLOWANCE_MM`] beyond
///   it. With more than one filament a prime tower stands apart from the parts, so there
///   the excess is reported instead of refused.
/// - With `arrange` off, a file that does not show where it prints is refused. For a real
///   program only the G-code counts: the outline it records beside the G-code is its own
///   claim. (`real` is false for the stand-in, whose G-code has no commands.)
fn check_printed_extent(
    job: &SliceJob,
    printer: &Printer,
    printed: Option<&PrintedExtent>,
    real: bool,
    warnings: &mut Vec<SliceWarning>,
) -> Result<(), SlicerError> {
    let placed: Vec<&Extent> = job.extents.iter().filter(|extent| extent.placed).collect();
    let kept = !job.arrange && !placed.is_empty();
    let Some(printed) = printed else {
        if kept {
            return Err(SlicerError::PositionsUnknown {
                reason: "the sliced file has no printing move that can be located".into(),
            });
        }
        warnings.push(SliceWarning::new(
            "extent_unknown",
            "the sliced file does not show where it prints, so it could not be checked \
             against the reach of the nozzles",
        ));
        return Ok(());
    };
    if kept && real && printed.source != "gcode" {
        return Err(SlicerError::PositionsUnknown {
            reason: "the G-code of the sliced file has no printing move that can be \
                     located, and the outline the slicer recorded beside it is not what \
                     the printer runs"
                .into(),
        });
    }

    // The reach of the nozzles the job uses.
    let mut nozzles: Vec<&crate::printer::Nozzle> = Vec::new();
    for filament in &job.filaments {
        if let Some(nozzle) = printer.nozzle(filament.nozzle_id)
            && !nozzles.iter().any(|known| std::ptr::eq(*known, nozzle))
        {
            nozzles.push(nozzle);
        }
    }
    if let Some(first) = nozzles.first() {
        let reach = |pick: fn(&crate::printer::Reach) -> [f64; 2]| {
            nozzles
                .iter()
                .map(|nozzle| pick(&nozzle.reach))
                .fold([f64::INFINITY, f64::NEG_INFINITY], |[low, high], [a, b]| {
                    [low.min(a), high.max(b)]
                })
        };
        let axes = [
            ("X", Some(printed.x), reach(|r| r.x)),
            ("Y", Some(printed.y), reach(|r| r.y)),
            ("Z", printed.z, reach(|r| r.z)),
        ];
        for (axis, range, [low, high]) in axes {
            let Some([from, to]) = range else {
                continue;
            };
            if from < low - REACH_SLACK_MM || to > high + REACH_SLACK_MM {
                return Err(SlicerError::OutOfReach {
                    what: "what the sliced file prints".into(),
                    role: first.role.clone(),
                    device_extruder: first.ids.device_extruder,
                    detail: format!(
                        "the file prints from {axis} {from} to {to} mm, and the nozzle{} of \
                         this job reach{} from {low} to {high} mm",
                        if nozzles.len() == 1 { "" } else { "s" },
                        if nozzles.len() == 1 { "es" } else { "" },
                    ),
                });
            }
        }
    }

    if !kept {
        return Ok(());
    }
    let low = |axis: usize| {
        placed
            .iter()
            .map(|e| e.min[axis])
            .fold(f64::INFINITY, f64::min)
    };
    let high = |axis: usize| {
        placed
            .iter()
            .map(|e| e.max[axis])
            .fold(f64::NEG_INFINITY, f64::max)
    };
    let ranges = [(printed.x, 0), (printed.y, 1)];
    let covered = ranges.iter().all(|(range, axis)| {
        range[0] <= low(*axis) + POSITION_SLACK_MM && range[1] >= high(*axis) - POSITION_SLACK_MM
    });
    // How far the file prints beyond the parts, on the side where it is furthest.
    let excess = ranges
        .iter()
        .map(|(range, axis)| (low(*axis) - range[0]).max(range[1] - high(*axis)))
        .fold(0.0, f64::max);
    let not_kept = || SlicerError::PositionsNotKept {
        expected: format!(
            "X {:.1} to {:.1}, Y {:.1} to {:.1}",
            low(0),
            high(0),
            low(1),
            high(1)
        ),
        printed: format!(
            "X {:.1} to {:.1}, Y {:.1} to {:.1}",
            printed.x[0], printed.x[1], printed.y[0], printed.y[1]
        ),
    };
    if !covered {
        return Err(not_kept());
    }
    if excess > OUTSIDE_ALLOWANCE_MM {
        if job.filaments.len() < 2 {
            return Err(not_kept());
        }
        warnings.push(SliceWarning::new(
            "printed_outside_parts",
            format!(
                "the sliced file prints up to {excess:.0} mm outside the box of the parts \
                 (X {:.1} to {:.1}, Y {:.1} to {:.1}). With more than one filament that \
                 can be a prime tower; check the preview",
                printed.x[0], printed.x[1], printed.y[0], printed.y[1]
            ),
        ));
    }
    Ok(())
}

/// Compares `result.json` with the sliced file. The file is what the printer gets, so its
/// numbers stand; a difference is reported.
fn compare_report(
    report: &SlicerReport,
    plate: u32,
    parsed: &Parsed,
    warnings: &mut Vec<SliceWarning>,
) {
    let Some(plate) = report.plate(plate) else {
        warnings.push(SliceWarning::new(
            "result_incomplete",
            format!("the slicer's result.json has no entry for plate {plate}"),
        ));
        return;
    };
    let differ = |a: f64, b: f64, slack: f64| (a - b).abs() > slack.max(0.01 * a.max(b));
    let mut differences = Vec::new();
    if let (Some(file), Some(stated)) = (parsed.time_s, plate.total_s)
        && differ(file, stated, 2.0)
    {
        differences.push(format!("print time {file} s against {stated} s"));
    }
    for filament in &plate.filaments {
        if let (Some((Some(file), _)), Some(stated)) =
            (parsed.filaments.get(&filament.id), filament.total_g)
            && differ(*file, stated, 0.02)
        {
            differences.push(format!(
                "filament {} {file} g against {stated} g",
                filament.id
            ));
        }
    }
    if !differences.is_empty() {
        warnings.push(SliceWarning::new(
            "summary_disagrees",
            format!(
                "the sliced file and the slicer's result.json differ: {}; the file's numbers \
                 are used",
                differences.join("; ")
            ),
        ));
    }
    if !plate.warning.is_empty() {
        warnings.push(SliceWarning::new(
            "slicer_warning",
            format!("{} (from result.json)", plate.warning),
        ));
    }
}

fn refusal(installation: &Installation) -> SlicerError {
    let reason = match &installation.verdict {
        Verdict::Refuse(reason) => reason.clone(),
        _ => String::new(),
    };
    SlicerError::VersionRefused {
        kind: installation.kind.display_name(),
        version: installation.version_text(),
        reason,
    }
}

struct Assembled {
    time_s: Option<f64>,
    layers: Option<u32>,
    usage: Vec<FilamentUsage>,
}

/// Joins what the parser read with what the job asked for. The nozzle of a slot is the one
/// the job assigned; if the slicer reports a different filament map, the result is refused.
fn assemble(
    job: &SliceJob,
    printer: &Printer,
    requested_map: Option<&[u32]>,
    mut parsed: Parsed,
    warnings: &mut Vec<SliceWarning>,
) -> Result<Assembled, SlicerError> {
    if let Some(requested) = requested_map {
        match &parsed.reported_map {
            Some(reported) if reported.len() == requested.len() => {
                if reported.as_slice() != requested {
                    return Err(SlicerError::HostileOutput(format!(
                        "the slicer used the filament map {reported:?} (slicer extruder per \
                         slot) but the job asked for {requested:?}; the file would send \
                         filament to the wrong nozzle"
                    )));
                }
            }
            _ => warnings.push(SliceWarning::new(
                "filament_map_unconfirmed",
                format!(
                    "the sliced file does not confirm the filament map {requested:?} that the \
                     job asked for on {}",
                    printer.name
                ),
            )),
        }
    }
    let mut usage = Vec::new();
    for filament in &job.filaments {
        let (grams, metres) = parsed
            .filaments
            .remove(&filament.slot)
            .unwrap_or((None, None));
        if grams.is_none() || metres.is_none() {
            warnings.push(SliceWarning::new(
                "filament_use_unknown",
                format!(
                    "the sliced file does not state how much filament slot {} uses",
                    filament.slot
                ),
            ));
        }
        usage.push(FilamentUsage {
            slot: filament.slot,
            grams,
            metres,
            nozzle_id: filament.nozzle_id,
        });
    }
    if !parsed.filaments.is_empty() {
        let extra: Vec<String> = parsed.filaments.keys().map(u32::to_string).collect();
        warnings.push(SliceWarning::new(
            "unexpected_filament",
            format!(
                "the sliced file reports filament(s) {} that the job does not define",
                extra.join(", ")
            ),
        ));
    }
    warnings.append(&mut parsed.warnings);
    Ok(Assembled {
        time_s: parsed.time_s,
        layers: parsed.layers,
        usage,
    })
}

/// The job folder must be an existing, absolute directory; its resolved path is used from
/// here on.
fn check_job_dir(job_dir: &Path) -> Result<PathBuf, SlicerError> {
    if !job_dir.is_absolute() || !job_dir.is_dir() {
        return Err(SlicerError::InvalidJob(format!(
            "the job folder {} is not an existing folder with an absolute path",
            job_dir.display()
        )));
    }
    let resolved =
        std::fs::canonicalize(job_dir).map_err(|e| SlicerError::io("resolve", job_dir, e))?;
    // The path goes into `;`-separated lists on Windows-style command lines only as a
    // relative name, but a line break in it could still confuse a slicer's own logging.
    if resolved.to_string_lossy().contains(['\n', '\r', '\0']) {
        return Err(SlicerError::InvalidJob(
            "the job folder path contains a line break".into(),
        ));
    }
    Ok(resolved)
}

/// Checks the model files and returns their paths relative to the job folder with `/`. A
/// 3MF must hold the model and nothing else.
fn check_inputs(
    job_dir: &Path,
    inputs: &[PathBuf],
    kind: SlicerKind,
    limits: &OutputLimits,
) -> Result<Vec<String>, SlicerError> {
    if inputs.is_empty() || inputs.len() > MAX_INPUTS {
        return Err(SlicerError::InvalidJob(format!(
            "a job needs 1 to {MAX_INPUTS} model files"
        )));
    }
    let (accepted, list): (&[&str], &'static str) = match kind {
        // The wiki's usage line: `bambu-studio [ OPTIONS ] [ file.3mf/file.stl ... ]`.
        SlicerKind::BambuStudio => (&["3mf", "stl"], "3MF and STL"),
        SlicerKind::PrusaSlicer => (&["3mf", "stl", "step", "stp"], "3MF, STL and STEP"),
    };
    let mut relative = Vec::new();
    for input in inputs {
        let plain = input
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)));
        let text = input.to_str().filter(|text| {
            plain && !text.starts_with('-') && !text.contains([';', '\n', '\r', '"'])
        });
        let Some(text) = text else {
            return Err(SlicerError::InvalidJob(format!(
                "the model path `{}` is not a plain relative path inside the job folder",
                untrusted_text(&input.to_string_lossy(), 120)
            )));
        };
        let text = text.replace(std::path::MAIN_SEPARATOR, "/");
        let extension = input
            .extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        if !accepted.contains(&extension.as_str()) {
            return Err(SlicerError::InputFormat {
                kind: kind.display_name(),
                accepted: list,
                input: text,
            });
        }
        let path = hotend_core::import::resolve_inside(job_dir, &text, MAX_INPUT_BYTES)
            .map_err(|e| SlicerError::InvalidJob(format!("the model file cannot be used: {e}")))?;
        // Both slicers read more from a 3MF than the model.
        if extension == "3mf" {
            crate::input::check_model_archive(&path, &text, limits)?;
        }
        relative.push(text);
    }
    Ok(relative)
}

/// Empties the folders this crate owns in the job folder and creates them again. A leftover
/// that is a link or a file is removed, never followed.
fn prepare_job_dir(job_dir: &Path) -> Result<(), SlicerError> {
    for name in [
        PROFILES_DIR,
        OUTPUT_DIR,
        CONFIG_DIR,
        hotend_sandbox::SANDBOX_DIR,
    ] {
        let path = job_dir.join(name);
        let removed = match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
            Ok(meta) if meta.file_type().is_dir() => std::fs::remove_dir_all(&path),
            Ok(_) => std::fs::remove_file(&path),
        };
        removed.map_err(|e| SlicerError::io("clear", &path, e))?;
        if name != hotend_sandbox::SANDBOX_DIR {
            std::fs::create_dir(&path).map_err(|e| SlicerError::io("create", &path, e))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::job::FilamentAssignment;

    fn job(dir: &Path, filaments: Vec<FilamentAssignment>) -> SliceJob {
        SliceJob {
            job_dir: dir.to_path_buf(),
            inputs: vec![PathBuf::from("input/model.stl")],
            printer: "bambu-x2d".into(),
            process_profile: "demo-0.20mm".into(),
            filaments,
            plate: 1,
            arrange: true,
            extents: vec![],
            bed_type: None,
            print_settings: None,
            nozzle_diameter_mm: None,
            plate_previews: None,
        }
    }

    fn filament(slot: u32, nozzle_id: i64) -> FilamentAssignment {
        FilamentAssignment {
            slot,
            filament_profile: "pla-basic".into(),
            family: "PLA".into(),
            nozzle_id,
        }
    }

    #[test]
    fn model_paths_must_be_plain_relative_files_of_an_accepted_format() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir(dir.join("input")).unwrap();
        for name in [
            "model.stl",
            "model.3mf",
            "model.step",
            "MODEL.STP",
            "model.obj",
            "a;b.stl",
        ] {
            std::fs::write(dir.join("input").join(name), b"x").unwrap();
        }
        // The archive check of a 3MF is tested with real archives in `crate::input` and in
        // `tests/sandboxed_run.rs`; here the 3MF is a fixture's plain model.
        std::fs::write(
            dir.join("input/model.3mf"),
            crate::output::bambu::tests::archive(&[("3D/3dmodel.model", b"<model/>")]),
        )
        .unwrap();
        let limits = OutputLimits::default();
        let check = |path: &str, kind| check_inputs(&dir, &[PathBuf::from(path)], kind, &limits);
        let bambu = SlicerKind::BambuStudio;
        let prusa = SlicerKind::PrusaSlicer;
        assert_eq!(
            check("input/model.stl", bambu).unwrap(),
            ["input/model.stl"]
        );
        assert!(check("input/model.3mf", bambu).is_ok());
        // Bambu Studio is documented for 3MF and STL only.
        assert_eq!(
            check("input/model.step", bambu).unwrap_err().code(),
            "input_format"
        );
        assert!(check("input/model.step", prusa).is_ok());
        assert!(check("input/MODEL.STP", prusa).is_ok());
        assert_eq!(
            check("input/model.obj", prusa).unwrap_err().code(),
            "input_format"
        );
        for bad in [
            "../model.stl",
            "/etc/passwd.stl",
            "input/../input/model.stl",
            "-model.stl",
            "input/a;b.stl",
            "input/missing.stl",
            "input",
        ] {
            let error = check(bad, prusa).unwrap_err();
            assert!(
                matches!(error.code(), "invalid_job" | "input_format"),
                "{bad}: {error}"
            );
        }
        assert!(check_inputs(&dir, &[], prusa, &limits).is_err());
        // A 3MF that is not an archive, or holds more than the model, is refused for
        // either slicer.
        std::fs::write(dir.join("input/model.3mf"), b"x").unwrap();
        for kind in [bambu, prusa] {
            assert_eq!(
                check("input/model.3mf", kind).unwrap_err().code(),
                "input_refused"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_model_file_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir(dir.join("job")).unwrap();
        std::fs::write(dir.join("secret.stl"), b"x").unwrap();
        std::os::unix::fs::symlink(dir.join("secret.stl"), dir.join("job/model.stl")).unwrap();
        let error = check_inputs(
            &dir.join("job"),
            &[PathBuf::from("model.stl")],
            SlicerKind::PrusaSlicer,
            &OutputLimits::default(),
        )
        .unwrap_err();
        assert_eq!(error.code(), "invalid_job");
    }

    #[cfg(unix)]
    #[test]
    fn leftovers_in_the_job_folder_are_removed_without_following_links() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap();
        let job_dir = dir.join("job");
        let elsewhere = dir.join("elsewhere");
        std::fs::create_dir_all(job_dir.join("profiles/old")).unwrap();
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("keep.txt"), b"keep").unwrap();
        std::fs::write(job_dir.join("profiles/old/machine.json"), b"{}").unwrap();
        std::fs::write(job_dir.join("slicer-config"), b"a file, not a folder").unwrap();
        std::os::unix::fs::symlink(&elsewhere, job_dir.join("output")).unwrap();

        prepare_job_dir(&job_dir).unwrap();
        assert!(
            elsewhere.join("keep.txt").is_file(),
            "a link target was touched"
        );
        for name in ["profiles", "output", "slicer-config"] {
            let meta = std::fs::symlink_metadata(job_dir.join(name)).unwrap();
            assert!(meta.file_type().is_dir(), "{name}");
            assert_eq!(std::fs::read_dir(job_dir.join(name)).unwrap().count(), 0);
        }
    }

    #[test]
    fn a_reported_map_that_differs_from_the_job_is_refused() {
        let facts = PrinterFacts::builtin().unwrap();
        let x2d = facts.printer("bambu-x2d").unwrap();
        let dir = Path::new("/unused");
        let two = job(dir, vec![filament(1, 0), filament(2, 1)]);
        let requested = x2d.filament_map(&two.filaments).unwrap();
        assert_eq!(requested, [2, 1]);

        let with_map = |map: Option<Vec<u32>>| {
            let mut parsed = Parsed {
                reported_map: map,
                time_s: Some(10.0),
                layers: Some(5),
                ..Parsed::default()
            };
            parsed.filaments.insert(1, (Some(1.0), Some(0.3)));
            parsed.filaments.insert(2, (Some(2.0), Some(0.6)));
            parsed
        };
        let mut warnings = Vec::new();
        let ok = assemble(
            &two,
            x2d,
            Some(&requested),
            with_map(Some(vec![2, 1])),
            &mut warnings,
        )
        .unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        // The nozzle of each slot is the device extruder the job assigned.
        assert_eq!(ok.usage[0].nozzle_id, 0);
        assert_eq!(ok.usage[1].nozzle_id, 1);
        assert_eq!(ok.usage[1].grams, Some(2.0));

        let swapped = assemble(
            &two,
            x2d,
            Some(&requested),
            with_map(Some(vec![1, 2])),
            &mut warnings,
        );
        assert_eq!(swapped.err().unwrap().code(), "hostile_output");

        for unconfirmed in [None, Some(vec![2]), Some(vec![2, 1, 1])] {
            let mut warnings = Vec::new();
            assemble(
                &two,
                x2d,
                Some(&requested),
                with_map(unconfirmed),
                &mut warnings,
            )
            .unwrap();
            assert_eq!(warnings[0].code, "filament_map_unconfirmed");
        }
    }

    #[test]
    fn usage_the_file_does_not_state_is_null_with_a_warning() {
        let facts = PrinterFacts::builtin().unwrap();
        let x2d = facts.printer("bambu-x2d").unwrap();
        let two = job(Path::new("/unused"), vec![filament(1, 1), filament(2, 0)]);
        let mut parsed = Parsed::default();
        parsed.filaments.insert(1, (Some(1.5), None));
        parsed.filaments.insert(5, (Some(9.0), Some(9.0)));
        let mut warnings = Vec::new();
        let result = assemble(&two, x2d, None, parsed, &mut warnings).unwrap();
        assert_eq!(result.time_s, None);
        assert_eq!(result.layers, None);
        assert_eq!(
            (result.usage[0].grams, result.usage[0].metres),
            (Some(1.5), None)
        );
        assert_eq!(
            (result.usage[1].grams, result.usage[1].metres),
            (None, None)
        );
        let codes: Vec<&str> = warnings.iter().map(|w| w.code.as_str()).collect();
        assert_eq!(
            codes,
            [
                "filament_use_unknown",
                "filament_use_unknown",
                "unexpected_filament"
            ]
        );
    }
}
