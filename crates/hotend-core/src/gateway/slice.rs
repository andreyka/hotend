//! The seam between the gateway and a slicer. `hotend-core` defines the contract;
//! `hotend-slicer` (or a test double) implements it and is injected with
//! [`crate::gateway::Gateway::set_slice_backend`]. Until then the `slice` and
//! `propose_print` tools answer with the typed error code `not_configured`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};

use crate::engine::MaterialContext;

/// One piece on a prepared plate, as placed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlacedPiece {
    /// The object's name in the plate's 3MF.
    pub name: String,
    /// The nozzle that prints it (the profile's nozzle id).
    pub nozzle_id: i64,
    /// Its bounding box in plate coordinates, mm.
    pub min: [f64; 3],
    pub max: [f64; 3],
}

/// What the gateway asks a slicer to do for one plate of a prepared version.
#[derive(Debug, Clone, PartialEq)]
pub struct SliceRequest {
    pub project_id: String,
    pub version: u32,
    /// The plate to slice, read-only: a 3MF with one object per piece, in millimetres,
    /// already oriented and in plate coordinates. The slicer must not arrange it again.
    pub plate_file: PathBuf,
    /// The pieces in that file.
    pub pieces: Vec<PlacedPiece>,
    /// An existing folder for the slicer's outputs: `versions/<n>/slice/<profile-hash>/`.
    pub output_dir: PathBuf,
    /// Process profile id, as the agent or the user named it.
    pub profile: String,
    /// The plate's number in the preparation, from 1.
    pub plate: u32,
    pub printer_id: String,
    pub material: MaterialContext,
    /// The build plate on the printer, as the id the settings store (`cool`,
    /// `textured-pei`, ...); `None` when the person has not chosen one. A backend must
    /// slice for this plate or fail, and must not pick one for a file that can be printed.
    pub plate_type: Option<String>,
    /// The slicer executables the user chose in the settings, by slicer kind. A backend
    /// that looks for slicers itself uses these first.
    pub slicer_executables: BTreeMap<String, PathBuf>,
    /// Hotend's print settings for this plate, from its print plan; `None` slices with
    /// the installed profiles as they are (the stand-in slicer and older callers). Only
    /// `hotend-slicer` knows which profile key carries a field.
    pub print_settings: Option<hotend_settings::PrintSettings>,
    /// The diameter of the nozzle fitted to the printer, in mm: the person's printer
    /// setting, or 0.4 when they have not set it (shown to them as an assumption). A
    /// backend refuses a size it has no profile for.
    pub nozzle_diameter_mm: f64,
    /// Hotend's own renders of this plate, from which a backend makes the plate previews
    /// of the sliced file when its slicer drew none; `None` (the default setting,
    /// [`crate::settings::PreviewsMode::Slicer`]) asks for none. The sliced file then
    /// carries the warning `hotend_made_previews`.
    pub plate_previews: Option<PlateRenders>,
}

/// Code of the slice warning that the sliced file's plate previews were made by Hotend
/// from [`PlateRenders`]; the print review shows it.
pub const HOTEND_MADE_PREVIEWS: &str = "hotend_made_previews";

/// Hotend's own renders of a prepared plate (`prepare/plates/<n>/views/`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlateRenders {
    /// The isometric view.
    pub iso: PathBuf,
    /// The view from above.
    pub top: PathBuf,
}

/// A slicer program the backend knows, found on this machine or not, for the settings
/// page and the doctor report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlicerInstallation {
    /// `bambu-studio` or `prusa-slicer`.
    pub kind: String,
    /// The slicer's name for people.
    pub name: String,
    /// Where it is; `None` when it was not found.
    pub path: Option<PathBuf>,
    /// How it was found: `configured`, `on PATH` or `usual install folder`.
    pub found: Option<String>,
    pub version: Option<String>,
    /// Hotend's stand-in slicer: estimates only, output not printable.
    pub stand_in: bool,
    /// The version policy's word: `allow`, `warn`, `refuse`, or `missing`.
    pub verdict: String,
    /// Why, when the verdict is not `allow`.
    pub reason: Option<String>,
    /// What the user can do about it.
    pub fix: Option<String>,
    /// Whether a slice would run it.
    pub usable: bool,
    /// The printers of this build that are sliced with it.
    pub printers: Vec<String>,
}

/// Filament used by one slot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilamentUse {
    pub slot: u32,
    pub grams: f64,
    pub metres: f64,
    /// Device extruder id of the nozzle that prints this slot.
    pub nozzle_id: i64,
}

/// A warning from the slicer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SliceWarning {
    pub code: String,
    pub message: String,
}

/// Which slicer produced a result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlicerInfo {
    pub name: String,
    pub version: String,
}

/// The build plate a file was sliced for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SliceBed {
    /// The plate's id: the one the request named, or the one the backend chose when the
    /// request named none (only a stand-in may).
    pub id: String,
    /// The slicer's own name for it, as the sliced file states it.
    pub plate_type: String,
    /// The highest bed temperature of the file, when it states one.
    pub temperature_c: Option<f64>,
}

/// The `SliceResult` of PROTOCOL.md section 6.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SliceSummary {
    pub status: String,
    /// The sliced file, relative to [`SliceRequest::output_dir`].
    pub output: String,
    /// SHA-256 of the sliced file. The gateway recomputes it and refuses a mismatch.
    pub sha256: String,
    pub time_s: f64,
    pub filament: Vec<FilamentUse>,
    pub layers: u32,
    /// The plate the file was sliced for; `None` for a slicer without plate types and in
    /// records written before the plate was part of a slice.
    #[serde(default)]
    pub bed: Option<SliceBed>,
    pub warnings: Vec<SliceWarning>,
    pub slicer: SlicerInfo,
}

/// The keys an editable project lists as changed from the system presets
/// (`different_settings_to_system`), per preset, as the project file lists them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectKeys {
    pub process: Vec<String>,
    pub filament: Vec<String>,
    pub printer: Vec<String>,
}

/// What a backend made of [`SliceBackend::export_project`]: the editable project of one
/// plate, with Hotend's print settings, and the project sliced once on its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectExportSummary {
    /// The project file (`.3mf`), relative to [`SliceRequest::output_dir`].
    pub project: String,
    /// SHA-256 of the project file. The core recomputes it and refuses a mismatch.
    pub sha256: String,
    /// The keys the project lists as changed, per preset.
    pub keys: ProjectKeys,
    /// The project sliced alone by the same program (the test slice); its `output` is
    /// relative to [`SliceRequest::output_dir`].
    pub slice: SliceSummary,
    /// The proof's verdict on the test slice: `passed`. A backend returns no export for
    /// any other verdict.
    pub proof: String,
    /// True when a stand-in made it. The core never uses such an export.
    pub stand_in: bool,
}

/// One thing a slice can be made with: a process preset, a material or a build plate, by
/// the id a job or the settings name it with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogueEntry {
    /// The id a job or the settings use (`standard-0.20mm`, `pla-generic`, `textured-pei`).
    pub id: String,
    /// Other ids that select the same entry.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// The slicer's own name for it (the system profile, or the plate's name).
    pub name: String,
    /// Whether the name was seen and sliced with on a real installation.
    pub verified: bool,
}

/// What a backend can slice with for one printer: the process presets a `slice` may name,
/// the materials the settings may choose and the build plates. Data from the backend's own
/// table; nothing on the machine is read to make it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SlicerCatalogue {
    pub printer_id: String,
    pub processes: Vec<CatalogueEntry>,
    pub materials: Vec<CatalogueEntry>,
    pub plates: Vec<CatalogueEntry>,
}

impl SlicerCatalogue {
    /// Whether `id` names one of the materials, by its id or an alias.
    pub fn knows_material(&self, id: &str) -> bool {
        self.materials
            .iter()
            .any(|entry| entry.id == id || entry.aliases.iter().any(|alias| alias == id))
    }

    /// The material ids, comma-separated, for a message.
    pub fn material_ids(&self) -> String {
        self.materials
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Why slicing failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SliceError {
    /// No slicer backend has been injected into the gateway.
    #[error("slicing is not configured in this build: no slicer backend is installed")]
    NotConfigured,
    /// The slicer ran and failed, or could not run.
    #[error("slicing failed ({code}): {message}")]
    Failed { code: String, message: String },
}

/// A slicer the gateway can call.
///
/// Implementations run the vendor slicer inside the sandbox themselves and must write only
/// below `request.output_dir`. Write the method as
/// `fn slice(&self, request: SliceRequest) -> BoxFuture<'_, ...> { Box::pin(async move { ... }) }`.
pub trait SliceBackend: Send + Sync {
    /// Slices one version and returns the summary of PROTOCOL.md section 6.
    fn slice(&self, request: SliceRequest) -> BoxFuture<'_, Result<SliceSummary, SliceError>>;

    /// The slicer programs this backend can run, found or not, given the executables the
    /// user chose in the settings (by slicer kind). May block while it probes programs.
    /// The default is none.
    fn installations(&self, _executables: &BTreeMap<String, PathBuf>) -> Vec<SlicerInstallation> {
        Vec::new()
    }

    /// The process presets, materials and build plates this backend can slice with for
    /// `printer_id`; `None` when it has no table for that printer. Must not block on
    /// programs or files. The default is `None`.
    fn catalogue(&self, _printer_id: &str) -> Option<SlicerCatalogue> {
        None
    }

    /// Makes the editable project of the plate of `request` (its print settings must be
    /// `Some`) into `request.output_dir`: the slicer's own project file with Hotend's
    /// settings, which the slicer's window opens and slices again. Returned only when the
    /// project, sliced alone by the same program, passed the proof of the settings. The
    /// default is [`SliceError::NotConfigured`]: a backend that cannot make one says so.
    fn export_project(
        &self,
        _request: SliceRequest,
    ) -> BoxFuture<'_, Result<ProjectExportSummary, SliceError>> {
        Box::pin(async { Err(SliceError::NotConfigured) })
    }
}
