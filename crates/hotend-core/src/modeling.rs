//! Model operations on a project: build a new version, change a parameter, re-run checks,
//! render, measure, search the docs and export.
//!
//! This is the one place that turns an engine job into project state, used by both the tool
//! gateway (for the agent) and the facade (for the UI and the CLI).
//!
//! Guarantees:
//! - A version is created only from a build whose worker exited normally with status `ok`
//!   or `check_error` and whose required artefacts all passed the hostile-input checks.
//!   Anything else leaves the project untouched.
//! - Artefacts are stored under names the core chooses; a result that reports any other
//!   path for them is refused.
//! - Builds of one project never overlap.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::engine::{
    Artifacts, BuildRequest, CheckOptions, DocEntry, DocsRequest, EngineContext, EngineLimits,
    EngineRequest, EngineResult, EngineStatus, ExportFormat, ExportRequest, MeasureRequest,
    Outputs, Section, VIEW_NAMES, ViewsRequest,
};
use crate::jobs::{FinishedJob, JobError, JobFile, JobRunner, JobScope};
use crate::paths::{DataDir, PathError};
use crate::settings::{SETTINGS_KEY, Settings};
use crate::store::{NewAudit, NewVersion, Store, StoreError, Version, VersionStatus};
use crate::util::{is_model_name, sha256_hex};

mod baseline;
mod plan;
mod plates;

pub use baseline::{
    BASELINES_FILE, BaselineFields, BaselineRemoval, BaselineStore, InvalidBaseline, MAX_BASELINES,
    MAX_SOURCE_CHARS, ProvenBaseline,
};
pub use plan::{
    PRINT_MARGIN_MM, PlanAsItStands, PlanBaseline, PlanBasis, PlanChecklistItem, PlanDecision,
    PlanNotice, PlanRecord, PlatePlan, PrintPlanView, required_clearance, settings_source,
    verify_record,
};
pub use plates::{
    Calibration, CalibrationUpdate, GaugeExport, GaugeFile, MAX_COPIES, PlatePlanSummary,
    Preparation, PrepareOptions,
};

/// Largest model script (the engine's own limit).
pub const MAX_CODE_BYTES: usize = 512 * 1024;
const MAX_GEOMETRY_BYTES: u64 = 256 * 1024 * 1024;
const MAX_INDEX_BYTES: u64 = 64 * 1024;
const MAX_PNG_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CALIBRATION_BYTES: u64 = 1024 * 1024;
/// Wall-clock limit of a `sweep` job, and the time after which it starts no further setting
/// (those left are reported as not run).
const SWEEP_WALL_SECONDS: u64 = 240;
const SWEEP_BUDGET_SECONDS: f64 = 180.0;
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

/// Why a model operation failed.
#[derive(Debug, thiserror::Error)]
pub enum ModelingError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Job(#[from] JobError),
    #[error(transparent)]
    Path(#[from] PathError),
    /// The project has no version yet; build one first.
    #[error("the project has no model yet; write model code first")]
    NoCurrentVersion,
    /// An argument is out of bounds.
    #[error("{0}")]
    Invalid(String),
    /// A file of the core's own could not be read or written.
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl ModelingError {
    /// A short stable code for audit entries and tool errors.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Store(StoreError::NotFound { .. }) => "not_found",
            Self::Store(_) | Self::Path(_) | Self::Io { .. } => "storage_error",
            Self::Job(error) => error.code(),
            Self::NoCurrentVersion => "no_current_version",
            Self::Invalid(_) => "invalid_arguments",
        }
    }
}

/// What a version was built for: the settings its fits were resolved from and its checks
/// ran against. Stored with the version as `built_for.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuiltFor {
    pub printer_id: String,
    pub material_id: String,
    /// SHA-256 of the printer's calibration table at the build; `None` when there was
    /// none (the engine's default fits).
    pub calibration_sha256: Option<String>,
}

impl BuiltFor {
    const FILE: &'static str = "built_for.json";

    /// What a build would be for now.
    pub(crate) fn of(settings: &Settings, calibration: &Path) -> Self {
        let table = std::fs::symlink_metadata(calibration)
            .ok()
            .filter(|metadata| metadata.is_file() && metadata.len() <= MAX_CALIBRATION_BYTES)
            .and_then(|_| std::fs::read(calibration).ok());
        Self {
            printer_id: settings.printer_id.clone(),
            material_id: settings.material.id.clone(),
            calibration_sha256: table.map(|bytes| sha256_hex(&bytes)),
        }
    }

    /// The warning of a print of the version in `dir` sliced for `printer_id` and
    /// `material_id`, with the printer's calibration table at `calibration` as it is now,
    /// when the version was built for something else. `None` when they agree or the version
    /// does not say. The same sentence as the preparation's.
    pub(crate) fn print_warning(
        dir: &Path,
        number: u32,
        printer_id: &str,
        material_id: &str,
        calibration: &Path,
    ) -> Option<String> {
        let built = Self::read(dir)?;
        let table = std::fs::symlink_metadata(calibration)
            .ok()
            .filter(|metadata| metadata.is_file() && metadata.len() <= MAX_CALIBRATION_BYTES)
            .and_then(|_| std::fs::read(calibration).ok())
            .map(|bytes| sha256_hex(&bytes));
        let mut differs = Vec::new();
        if built.printer_id != printer_id {
            differs.push(format!(
                "the printer `{}` (now `{printer_id}`)",
                built.printer_id
            ));
        }
        if built.material_id != material_id {
            differs.push(format!(
                "the material `{}` (now `{material_id}`)",
                built.material_id
            ));
        }
        if differs.is_empty() && built.calibration_sha256 != table {
            differs.push("an earlier calibration of this printer".to_owned());
        }
        (!differs.is_empty()).then(|| {
            format!(
                "Version {number} was built and checked for {}. Its fits and checks do not \
                 hold for what the settings say now: build it again before you print it.",
                differs.join(" and ")
            )
        })
    }

    /// What the version in `dir` was built for, if it says.
    pub(crate) fn read(dir: &Path) -> Option<Self> {
        let path = dir.join(Self::FILE);
        let metadata = std::fs::symlink_metadata(&path).ok()?;
        if !metadata.is_file() || metadata.len() > MAX_INDEX_BYTES {
            return None;
        }
        serde_json::from_slice(&std::fs::read(&path).ok()?).ok()
    }
}

/// The outcome of a build that was meant to become a version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuildReport {
    /// The new version; `None` when the model script failed (`result.status` is
    /// `model_error` and `result.error` says why).
    pub version: Option<Version>,
    /// The engine's validated result.
    pub result: EngineResult,
}

/// A stored version with its engine result and folder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VersionDetail {
    pub version: Version,
    /// The engine result stored with the version (`result.json`).
    pub result: EngineResult,
    /// The version folder; artefact paths in `result.artifacts` are relative to it.
    pub dir: PathBuf,
}

/// One rendered view. As JSON, `png` is the image in base64.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderedView {
    pub name: String,
    #[serde(with = "png_base64")]
    pub png: Vec<u8>,
}

/// Image bytes as a base64 string, the form an interface can put in a data URL.
mod png_base64 {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        STANDARD.decode(text).map_err(serde::de::Error::custom)
    }
}

/// One file written by an export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportedFile {
    pub format: ExportFormat,
    /// The part, for per-part formats (STL).
    pub part: Option<String>,
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
}

/// Model operations. Cheap to share behind an `Arc`.
#[derive(Debug)]
pub struct Modeler {
    data: DataDir,
    store: Arc<Store>,
    jobs: JobRunner,
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

/// Files to place in a job folder: the name inside the folder and the content.
type JobFiles = Vec<(&'static str, JobFile)>;

impl Modeler {
    pub fn new(data: DataDir, store: Arc<Store>, jobs: JobRunner) -> Self {
        Self {
            data,
            store,
            jobs,
            locks: Mutex::new(HashMap::new()),
        }
    }

    /// The data directory layout.
    pub fn data(&self) -> &DataDir {
        &self.data
    }

    /// The store.
    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    /// The job runner.
    pub fn jobs(&self) -> &JobRunner {
        &self.jobs
    }

    /// The user's settings, or the defaults when none were saved.
    pub fn settings(&self) -> Result<Settings, StoreError> {
        match self.store.setting(SETTINGS_KEY)? {
            Some(value) => serde_json::from_value(value).map_err(|source| StoreError::Corrupt {
                what: "the settings",
                source,
            }),
            None => Ok(Settings::default()),
        }
    }

    fn project_lock(&self, project_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.locks.lock().unwrap_or_else(|p| p.into_inner());
        locks.entry(project_id.to_owned()).or_default().clone()
    }

    /// Printer, material and calibration for a build, plus the files to place in the job.
    fn context(&self) -> Result<(EngineContext, JobFiles), ModelingError> {
        let settings = self.settings()?;
        // Only a validated printer id (a slug) is used to build a path.
        let valid = settings.validate().is_ok();
        let mut context = EngineContext {
            printer: crate::profiles::printer_profile(&settings.printer_id)
                .map(|profile| profile.engine_context()),
            material: Some(settings.material),
            calibration_path: None,
        };
        let mut files = Vec::new();
        let calibration = self
            .data
            .root()
            .join("calibration")
            .join(format!("{}.json", settings.printer_id));
        if let Ok(metadata) = std::fs::symlink_metadata(&calibration)
            && metadata.is_file()
            && metadata.len() <= MAX_CALIBRATION_BYTES
            && valid
        {
            context.calibration_path = Some("calibration.json".to_owned());
            files.push(("calibration.json", JobFile::Copy(calibration)));
        }
        Ok((context, files))
    }

    /// The current version of a project.
    pub fn current_version(&self, project_id: &str) -> Result<Version, ModelingError> {
        let number = self
            .store
            .project(project_id)?
            .current_version
            .ok_or(ModelingError::NoCurrentVersion)?;
        Ok(self.store.version(project_id, number)?)
    }

    /// A version with its stored engine result.
    pub fn version_detail(
        &self,
        project_id: &str,
        number: u32,
    ) -> Result<VersionDetail, ModelingError> {
        let version = self.store.version(project_id, number)?;
        let dir = self.data.version_dir(project_id, number)?;
        let path = dir.join("result.json");
        let io = |source| ModelingError::Io {
            path: path.clone(),
            source,
        };
        let bytes = std::fs::read(&path).map_err(io)?;
        let result = serde_json::from_slice(&bytes).map_err(|e| io(std::io::Error::other(e)))?;
        Ok(VersionDetail {
            version,
            result,
            dir,
        })
    }

    fn read_code(&self, version: &Version) -> Result<String, ModelingError> {
        let path = self
            .data
            .version_dir(&version.project_id, version.number)?
            .join("model.py");
        std::fs::read_to_string(&path).map_err(|source| ModelingError::Io { path, source })
    }

    /// Builds `code` and, if it built, stores the result as the project's next version.
    pub async fn build_version(
        &self,
        scope: &JobScope,
        project_id: &str,
        code: &str,
        params: BTreeMap<String, f64>,
        origin: &str,
        note: &str,
    ) -> Result<BuildReport, ModelingError> {
        if code.len() > MAX_CODE_BYTES {
            return Err(ModelingError::Invalid(format!(
                "the model script is larger than {MAX_CODE_BYTES} bytes"
            )));
        }
        let lock = self.project_lock(project_id);
        let _guard = lock.lock().await;
        let parent = self.store.project(project_id)?.current_version;
        let settings = self.settings()?;
        let built_for = BuiltFor::of(
            &settings,
            &self
                .data
                .root()
                .join("calibration")
                .join(format!("{}.json", settings.printer_id)),
        );
        let (context, mut files) = self.context()?;
        files.push(("model.py", JobFile::Bytes(code.as_bytes().to_vec())));
        let request = EngineRequest::Build(BuildRequest {
            model_path: "model.py".to_owned(),
            params,
            context,
            outputs: Outputs {
                views: vec!["iso".to_owned()],
                ..Outputs::default()
            },
            checks: CheckOptions::product(),
            ..BuildRequest::default()
        });
        let job = self.jobs.run(scope, &request, files).await?;
        if job.result.status == EngineStatus::ModelError {
            return Ok(BuildReport {
                version: None,
                result: job.result.clone(),
            });
        }

        let staging = self
            .data
            .versions_dir(project_id)?
            .join(format!(".staging-{}", job.id));
        let code_owned = code.to_owned();
        let staged_dir = staging.clone();
        let staged = tokio::task::spawn_blocking(move || {
            let outcome = stage_version(&job, &staged_dir, &code_owned, &built_for);
            if outcome.is_err() {
                let _ = std::fs::remove_dir_all(&staged_dir);
            }
            outcome
        })
        .await
        .map_err(|e| ModelingError::Io {
            path: staging.clone(),
            source: std::io::Error::other(e),
        })
        .and_then(|outcome| outcome);
        let staged = match staged {
            Ok(staged) => staged,
            Err(error) => {
                // The job itself was audited as finished; record that its files were refused.
                let _ = self.store.audit(NewAudit {
                    project_id: Some(project_id.to_owned()),
                    run_id: scope.run_id.clone(),
                    kind: "version",
                    name: origin.to_owned(),
                    args_sha256: Some(sha256_hex(code.as_bytes())),
                    outcome: error.code().to_owned(),
                    detail: json!({"error": crate::util::truncate(&error.to_string(), 2000)}),
                });
                return Err(error);
            }
        };

        let new = NewVersion {
            parent,
            origin: origin.to_owned(),
            note: note.to_owned(),
            status: if staged.status == EngineStatus::Ok {
                VersionStatus::Ok
            } else {
                VersionStatus::CheckError
            },
            code_sha256: sha256_hex(code.as_bytes()),
            params: staged.params.clone(),
        };
        let version = match self
            .store
            .create_version(&self.data, project_id, &staging, new)
        {
            Ok(version) => version,
            Err(error) => {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(error.into());
            }
        };
        let _ = self.store.audit(NewAudit {
            project_id: Some(project_id.to_owned()),
            run_id: scope.run_id.clone(),
            kind: "version",
            name: origin.to_owned(),
            args_sha256: Some(version.code_sha256.clone()),
            outcome: "ok".to_owned(),
            detail: json!({"version": version.number, "status": staged.status.as_str()}),
        });
        Ok(BuildReport {
            version: Some(version),
            result: staged,
        })
    }

    /// Rebuilds the current version with one parameter changed, as a new version.
    pub async fn set_parameter(
        &self,
        scope: &JobScope,
        project_id: &str,
        name: &str,
        value: f64,
    ) -> Result<BuildReport, ModelingError> {
        if !is_model_name(name) || !value.is_finite() {
            return Err(ModelingError::Invalid(
                "a parameter needs a valid name and a finite number".to_owned(),
            ));
        }
        let current = self.current_version(project_id)?;
        let code = self.read_code(&current)?;
        let mut params = current.params.clone();
        params.insert(name.to_owned(), value);
        self.build_version(
            scope,
            project_id,
            &code,
            params,
            "set_parameter",
            &format!("{name} = {value}"),
        )
        .await
    }

    /// Runs the checks on the current version without creating a version.
    pub async fn run_checks(
        &self,
        scope: &JobScope,
        project_id: &str,
        only: Option<Vec<String>>,
        sample_ranges: bool,
    ) -> Result<EngineResult, ModelingError> {
        let current = self.current_version(project_id)?;
        let code = self.read_code(&current)?;
        let (context, mut files) = self.context()?;
        files.push(("model.py", JobFile::Bytes(code.into_bytes())));
        let request = EngineRequest::Build(BuildRequest {
            model_path: "model.py".to_owned(),
            params: current.params,
            context,
            outputs: Outputs::none(),
            checks: CheckOptions {
                only,
                sample_ranges,
                ..CheckOptions::product()
            },
            limits: EngineLimits::default(),
        });
        Ok(self.jobs.run(scope, &request, files).await?.result.clone())
    }

    /// Builds the current version at a few parameter settings, one after the other, without
    /// creating a version or storing anything (the engine's `sweep` op: the range sampler in
    /// the model process). Each setting overrides some of the version's parameters. The
    /// report is advice for the agent; nothing in it becomes a check outcome of a version.
    pub async fn sweep(
        &self,
        scope: &JobScope,
        project_id: &str,
        settings: Vec<BTreeMap<String, f64>>,
    ) -> Result<EngineResult, ModelingError> {
        if settings.is_empty() || settings.len() > crate::engine::MAX_SWEEP_SETTINGS {
            return Err(ModelingError::Invalid(format!(
                "a sweep builds 1 to {} settings",
                crate::engine::MAX_SWEEP_SETTINGS
            )));
        }
        let current = self.current_version(project_id)?;
        let code = self.read_code(&current)?;
        let (context, mut files) = self.context()?;
        files.push(("model.py", JobFile::Bytes(code.into_bytes())));
        let limits = EngineLimits {
            wall_seconds: SWEEP_WALL_SECONDS,
            ..EngineLimits::default()
        };
        let request = EngineRequest::Sweep(crate::engine::SweepRequest {
            model_path: "model.py".to_owned(),
            params: current.params,
            settings,
            context,
            min_wall: CheckOptions::product().min_wall,
            budget_seconds: SWEEP_BUDGET_SECONDS,
            limits,
        });
        Ok(self.jobs.run(scope, &request, files).await?.result.clone())
    }

    /// The geometry files of a version that the measure, export and views ops load.
    fn geometry_files(
        &self,
        project_id: &str,
        number: u32,
    ) -> Result<Vec<(&'static str, JobFile)>, ModelingError> {
        let dir = self.data.version_dir(project_id, number)?;
        Ok(["model.brep", "model.parts.json"]
            .into_iter()
            .map(|name| (name, JobFile::Copy(dir.join(name))))
            .collect())
    }

    /// Renders views of the current version, optionally cut open by a section plane.
    pub async fn render_views(
        &self,
        scope: &JobScope,
        project_id: &str,
        views: Vec<String>,
        section: Option<Section>,
        size: [u32; 2],
    ) -> Result<Vec<RenderedView>, ModelingError> {
        if views.is_empty()
            || views.len() > VIEW_NAMES.len()
            || views.iter().any(|v| !VIEW_NAMES.contains(&v.as_str()))
        {
            return Err(ModelingError::Invalid(format!(
                "views must be one or more of: {}",
                VIEW_NAMES.join(", ")
            )));
        }
        let current = self.current_version(project_id)?;
        let request = EngineRequest::Views(ViewsRequest {
            views: views.clone(),
            size,
            section,
            limits: EngineLimits::default(),
        });
        let files = self.geometry_files(project_id, current.number)?;
        let job = self.jobs.run(scope, &request, files).await?;
        tokio::task::spawn_blocking(move || {
            let mut rendered = Vec::new();
            for name in views {
                if rendered.iter().any(|r: &RenderedView| r.name == name) {
                    continue;
                }
                let expected = format!("views/{name}.png");
                if job.result.artifacts.views.get(&name) != Some(&expected) {
                    return Err(JobError::MalformedResult {
                        reason: format!("view `{name}` was not reported at {expected}"),
                    });
                }
                let png = job.read_artifact(&expected, MAX_PNG_BYTES)?;
                if !png.starts_with(PNG_MAGIC) {
                    return Err(JobError::MalformedResult {
                        reason: format!("{expected} is not a PNG image"),
                    });
                }
                rendered.push(RenderedView { name, png });
            }
            Ok(rendered)
        })
        .await
        .map_err(|e| ModelingError::Invalid(format!("the view import was interrupted: {e}")))?
        .map_err(ModelingError::Job)
    }

    /// Exact measurements on the current version. Returns one result object per query.
    pub async fn measure(
        &self,
        scope: &JobScope,
        project_id: &str,
        queries: Vec<Value>,
    ) -> Result<Vec<Value>, ModelingError> {
        let current = self.current_version(project_id)?;
        let request = EngineRequest::Measure(MeasureRequest {
            queries,
            limits: EngineLimits::default(),
        });
        let files = self.geometry_files(project_id, current.number)?;
        Ok(self
            .jobs
            .run(scope, &request, files)
            .await?
            .result
            .results
            .clone())
    }

    /// Searches the documentation of the CAD libraries installed in the engine runtime.
    /// Returns the library versions and the best excerpts.
    pub async fn docs(
        &self,
        scope: &JobScope,
        query: &str,
        limit: u32,
    ) -> Result<(BTreeMap<String, Option<String>>, Vec<DocEntry>), ModelingError> {
        let request = EngineRequest::Docs(DocsRequest {
            query: query.to_owned(),
            limit: limit.clamp(1, 20),
            limits: EngineLimits::default(),
        });
        let job = self.jobs.run(scope, &request, Vec::new()).await?;
        Ok((job.result.versions.clone(), job.result.docs.clone()))
    }

    /// Exports a version (the current one when `version` is `None`) into `destination`,
    /// an existing folder the user chose. Existing files of the same name are replaced.
    pub async fn export(
        &self,
        scope: &JobScope,
        project_id: &str,
        version: Option<u32>,
        formats: Vec<ExportFormat>,
        parts: Option<Vec<String>>,
        destination: &Path,
    ) -> Result<Vec<ExportedFile>, ModelingError> {
        if formats.is_empty() {
            return Err(ModelingError::Invalid(
                "name at least one export format".to_owned(),
            ));
        }
        if parts
            .as_ref()
            .is_some_and(|p| p.iter().any(|name| !is_model_name(name)))
        {
            return Err(ModelingError::Invalid(
                "a part name is not valid".to_owned(),
            ));
        }
        if !destination.is_dir() {
            return Err(ModelingError::Invalid(format!(
                "the export folder {} does not exist",
                destination.display()
            )));
        }
        let number = match version {
            Some(number) => self.store.version(project_id, number)?.number,
            None => self.current_version(project_id)?.number,
        };
        let request = EngineRequest::Export(ExportRequest {
            formats,
            parts,
            limits: EngineLimits::default(),
        });
        let files = self.geometry_files(project_id, number)?;
        let job = self.jobs.run(scope, &request, files).await?;
        let destination = destination.to_path_buf();
        tokio::task::spawn_blocking(move || export_files(&job, &destination))
            .await
            .map_err(|e| ModelingError::Invalid(format!("the export was interrupted: {e}")))?
            .map_err(ModelingError::Job)
    }
}

/// Requires that the child reported `expected` for an artefact the core stores under a
/// fixed name.
fn expect_path(
    reported: Option<&String>,
    expected: &str,
    required: bool,
) -> Result<bool, JobError> {
    match reported {
        Some(path) if path == expected => Ok(true),
        Some(_) => Err(JobError::MalformedResult {
            reason: format!("an artefact was not reported at its fixed path `{expected}`"),
        }),
        None if required => Err(JobError::MalformedResult {
            reason: format!("the build did not report `{expected}`"),
        }),
        None => Ok(false),
    }
}

/// Copies a finished build into a staging folder laid out as a version folder and returns
/// the result as it is stored (artefacts limited to what was copied).
fn stage_version(
    job: &FinishedJob,
    staging: &Path,
    code: &str,
    built_for: &BuiltFor,
) -> Result<EngineResult, ModelingError> {
    let io = |path: &Path| {
        let path = path.to_path_buf();
        move |source| ModelingError::Io { path, source }
    };
    std::fs::create_dir_all(staging).map_err(io(staging))?;
    let reported = &job.result.artifacts;
    let mut stored = Artifacts::default();

    for (path, slot, max, required) in [
        ("model.step", &reported.step, MAX_GEOMETRY_BYTES, true),
        ("model.brep", &reported.brep, MAX_GEOMETRY_BYTES, true),
        (
            "model.parts.json",
            &reported.parts_index,
            MAX_INDEX_BYTES,
            true,
        ),
        ("mesh.json", &reported.mesh, MAX_GEOMETRY_BYTES, false),
    ] {
        if expect_path(slot.as_ref(), path, required)? {
            job.copy_artifact(path, &staging.join(path), max)?;
        }
    }
    stored.step = reported.step.clone();
    stored.brep = reported.brep.clone();
    stored.parts_index = reported.parts_index.clone();
    stored.mesh = reported.mesh.clone();
    for (name, path) in &reported.views {
        let expected = format!("views/{name}.png");
        expect_path(Some(path), &expected, true)?;
        let target = staging.join("views").join(format!("{name}.png"));
        job.copy_artifact(&expected, &target, MAX_PNG_BYTES)?;
        let head = std::fs::read(&target).map_err(io(&target))?;
        if !head.starts_with(PNG_MAGIC) {
            return Err(JobError::MalformedResult {
                reason: format!("{expected} is not a PNG image"),
            }
            .into());
        }
        stored.views.insert(name.clone(), expected);
    }

    let mut result = job.result.clone();
    result.artifacts = stored;
    let write = |name: &str, bytes: Vec<u8>| {
        let path = staging.join(name);
        std::fs::write(&path, bytes).map_err(io(&path))
    };
    // A value that cannot be written is an error, never an empty file.
    let json_error = |error: serde_json::Error| io(staging)(std::io::Error::other(error));
    let to_json = |value: Value| serde_json::to_vec_pretty(&value).map_err(json_error);
    write("model.py", code.as_bytes().to_vec())?;
    write("params.json", to_json(json!(result.params))?)?;
    // Compact: for a design around a device indenting adds about 40 % (97 kB for 70 kB), and
    // the bounds of `EngineResult` count compact text. Readers parse it, old files included.
    let compact = serde_json::to_vec(&result).map_err(json_error)?;
    write("result.json", compact)?;
    write(BuiltFor::FILE, to_json(json!(built_for))?)?;
    seal(staging);
    Ok(result)
}

/// Marks every file of a version folder read-only. Best effort: immutability is enforced by
/// the store offering no way to change a version, this only guards against accidents.
fn seal(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_dir() {
            seal(&path);
        } else {
            let mut permissions = metadata.permissions();
            permissions.set_readonly(true);
            let _ = std::fs::set_permissions(&path, permissions);
        }
    }
}

fn export_files(job: &FinishedJob, destination: &Path) -> Result<Vec<ExportedFile>, JobError> {
    let artifacts = &job.result.artifacts;
    let mut wanted: Vec<(ExportFormat, Option<String>, String, String)> = Vec::new();
    if expect_path(artifacts.step.as_ref(), "export/model.step", false)? {
        wanted.push((
            ExportFormat::Step,
            None,
            "export/model.step".into(),
            "model.step".into(),
        ));
    }
    if expect_path(artifacts.threemf.as_ref(), "export/model.3mf", false)? {
        wanted.push((
            ExportFormat::ThreeMf,
            None,
            "export/model.3mf".into(),
            "model.3mf".into(),
        ));
    }
    for (part, path) in &artifacts.stl {
        let expected = format!("export/{part}.stl");
        expect_path(Some(path), &expected, true)?;
        wanted.push((
            ExportFormat::Stl,
            Some(part.clone()),
            expected,
            format!("{part}.stl"),
        ));
    }
    let mut written = Vec::new();
    for (format, part, source, name) in wanted {
        let path = destination.join(name);
        // Replace an earlier export, but never write through a link someone left there.
        if let Ok(metadata) = std::fs::symlink_metadata(&path) {
            let removed = if metadata.is_file() || metadata.file_type().is_symlink() {
                std::fs::remove_file(&path)
            } else {
                Err(std::io::Error::other("the destination is a folder"))
            };
            removed.map_err(|source| JobError::Io {
                path: path.clone(),
                source,
            })?;
        }
        let (bytes, sha256) = job.copy_artifact(&source, &path, MAX_GEOMETRY_BYTES)?;
        written.push(ExportedFile {
            format,
            part,
            path,
            bytes,
            sha256,
        });
    }
    Ok(written)
}
