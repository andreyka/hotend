//! The error type of the slicer integration.

use std::path::PathBuf;

/// Why a slice did not produce a usable result. Every variant has a stable code
/// ([`SlicerError::code`]) for the tool gateway and a message that says what to do next.
/// Messages never contain file contents or environment values.
#[derive(Debug, thiserror::Error)]
pub enum SlicerError {
    /// A data file compiled into this crate is malformed. This is a build defect.
    #[error("built-in slicer data is invalid ({file}): {message}")]
    Data { file: &'static str, message: String },

    /// The job names something this build does not know or is put together wrongly.
    #[error("invalid slice job: {0}")]
    InvalidJob(String),

    /// The printer id is not in the printer profile file.
    #[error("this build has no printer profile for `{0}`")]
    UnknownPrinter(String),

    /// A part does not fit the nozzle that is meant to print it.
    #[error(
        "`{what}` does not fit the {role} nozzle (device extruder {device_extruder}): {detail}. \
         Make it smaller, reorient it, or print it with the other nozzle"
    )]
    OutOfReach {
        what: String,
        role: String,
        device_extruder: i64,
        detail: String,
    },

    /// A material is assigned to a nozzle that must not print it.
    #[error(
        "{family} (slot {slot}) cannot be printed by the {role} nozzle (device extruder \
         {device_extruder}); on this printer it is allowed only on device extruder(s) {allowed:?}"
    )]
    MaterialNotAllowed {
        family: String,
        slot: u32,
        role: String,
        device_extruder: i64,
        allowed: Vec<i64>,
    },

    /// No installed slicer of the needed kind was found.
    #[error(
        "{kind} was not found. Install it, or point {env} (or the slicer path in the settings) \
         at its executable"
    )]
    NotInstalled {
        kind: &'static str,
        env: &'static str,
    },

    /// The version policy does not allow this slicer version.
    #[error("{kind} {version} will not be used: {reason}")]
    VersionRefused {
        kind: &'static str,
        version: String,
        reason: String,
    },

    /// A settings profile id has no file.
    #[error(
        "no {role} settings profile `{id}` for {kind}; installed profiles are looked up in \
         {searched}"
    )]
    ProfileNotFound {
        kind: &'static str,
        role: &'static str,
        id: String,
        searched: String,
    },

    /// A settings profile exists but must not be used this way.
    #[error("settings profile `{id}` cannot be used: {reason}")]
    ProfileUnusable { id: String, reason: String },

    /// The slicer needs a mesh and only exact geometry was given.
    #[error(
        "{kind} is documented to read {accepted} files only, and `{input}` is neither. Export \
         the version as 3MF first"
    )]
    InputFormat {
        kind: &'static str,
        accepted: &'static str,
        input: String,
    },

    /// A model file holds more than the model.
    #[error(
        "the model file `{input}` is not handed to the slicer: {reason}. A model file for \
         slicing holds the model and nothing else; settings or G-code inside it could \
         change what is printed"
    )]
    InputRefused { input: String, reason: String },

    /// Nobody said which plate is on the printer.
    #[error(
        "the plate that is on the printer has not been chosen, and the slicer's own default \
         (the Cool Plate, with the bed at 35 C for PLA) is not taken on trust. Choose the \
         plate in Hotend's settings (`plate_type`) and slice again; the plate types are: \
         {known}"
    )]
    BedTypeNotChosen { known: String },

    /// The sandbox could not be set up; nothing was run.
    #[error("the slicer was not started because the sandbox is unavailable: {0}")]
    Sandbox(#[from] hotend_sandbox::SandboxError),

    /// The slicer ran longer than its wall-clock limit and was killed.
    #[error("the slicer was stopped after {seconds} s without finishing")]
    Timeout { seconds: u64 },

    /// The slicer exited with an error.
    #[error("the slicer failed ({status}): {stderr_tail}")]
    Failed { status: String, stderr_tail: String },

    /// The slicer left no account of the run. Bambu Studio's only machine-readable result
    /// is `result.json`; without it nothing says whether the slice worked.
    #[error(
        "the slicer finished without writing `{expected}`, so it is not known whether the \
         slice worked; nothing from this run is used. {detail}"
    )]
    ResultMissing { expected: String, detail: String },

    /// The slicer's own result file reports a failure.
    #[error("the slicer reported an error (return code {return_code}): {message}")]
    Reported { return_code: i64, message: String },

    /// The file was sliced for another plate than the job asked for, or does not say.
    #[error(
        "the job asked for the {asked}, but the sliced file {got}. The bed temperature \
         would be wrong for the plate on the printer, so the file is not used"
    )]
    BedTypeNotApplied { asked: String, got: String },

    /// The file's commands heat the bed in a way its own settings for the plate do not
    /// explain.
    #[error(
        "the sliced file says it was sliced for the {plate}, and {problem}. The file is \
         not used"
    )]
    BedTemperature { plate: String, problem: String },

    /// With the parts kept where the plate has them, the sliced file does not show where
    /// it prints.
    #[error(
        "it could not be checked that the slicer kept the positions of the plate: \
         {reason}. The checks made on those positions do not hold for a file that may \
         print elsewhere, so the file is not used"
    )]
    PositionsUnknown { reason: String },

    /// The slicer moved the parts although it was told to keep them where the plate has
    /// them, so the checks made on those positions do not hold for the file.
    #[error(
        "the slicer did not keep the positions of the plate: the parts lie at {expected}, \
         but the sliced file prints at {printed}. The file is not used"
    )]
    PositionsNotKept { expected: String, printed: String },

    /// The slicer reported success but its output is missing, too large or not a plain file
    /// inside the job folder.
    #[error("the slicer's output cannot be used: {0}")]
    UnusableOutput(String),

    /// The sliced file is usable but does not state a number the caller cannot do without.
    #[error("incomplete result: the sliced file does not state the {0}")]
    IncompleteResult(String),

    /// The sliced file is not what it should be: a broken or dangerous archive, or content
    /// that contradicts the job.
    #[error("the sliced file was rejected: {0}")]
    HostileOutput(String),

    /// A file operation in the job folder failed.
    /// One of Hotend's print settings cannot be written into the slicer's settings. `code`
    /// is one of [`crate::keymap::MAPPING_CODES`]; the message names the setting in plain
    /// words and says what to do.
    #[error("{message}")]
    Setting { code: &'static str, message: String },

    /// The nozzle fitted to the printer has no system profile in Hotend's table.
    #[error(
        "the nozzle fitted to the printer is {fitted} mm, and Hotend has no Bambu Studio \
         profile for a {fitted} mm nozzle on {printer} yet (it has: {available} mm). Nothing \
         was sliced. If the nozzle setting is wrong, correct it in the printer settings"
    )]
    NozzleNotSupported {
        fitted: String,
        printer: String,
        available: String,
    },

    /// The sliced file does not show the settings Hotend asked for, or does not show enough
    /// to tell (the proof, [`crate::proof`]). Nothing of the run is used.
    #[error("Hotend's check of the sliced file against its print settings failed: {message}")]
    Proof { code: &'static str, message: String },

    /// The editable project could not be made or did not prove self-contained.
    #[error("{message}")]
    Project { code: &'static str, message: String },

    /// A file operation in the job folder failed.
    #[error("{action} {path}: {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl SlicerError {
    /// Stable machine-readable code of this error.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Data { .. } => "internal_data",
            Self::InvalidJob(_) => "invalid_job",
            Self::UnknownPrinter(_) => "unknown_printer",
            Self::OutOfReach { .. } => "out_of_reach",
            Self::MaterialNotAllowed { .. } => "material_not_allowed",
            Self::NotInstalled { .. } => "slicer_not_installed",
            Self::VersionRefused { .. } => "slicer_version_refused",
            Self::ProfileNotFound { .. } => "profile_not_found",
            Self::ProfileUnusable { .. } => "profile_unusable",
            Self::InputFormat { .. } => "input_format",
            Self::Sandbox(_) => "sandbox_unavailable",
            Self::Timeout { .. } => "slicer_timeout",
            Self::Failed { .. } => "slicer_failed",
            Self::ResultMissing { .. } => "result_missing",
            Self::Reported { .. } => "slicer_failed",
            Self::BedTypeNotApplied { .. } => "bed_type_not_applied",
            Self::BedTemperature { .. } => "bed_temperature_not_applied",
            Self::Setting { code, .. } | Self::Proof { code, .. } | Self::Project { code, .. } => {
                code
            }
            Self::NozzleNotSupported { .. } => "nozzle_not_supported",
            Self::PositionsUnknown { .. } => "positions_unknown",
            Self::InputRefused { .. } => "input_refused",
            Self::BedTypeNotChosen { .. } => "bed_type_not_chosen",
            Self::PositionsNotKept { .. } => "positions_not_kept",
            Self::UnusableOutput(_) => "unusable_output",
            Self::IncompleteResult(_) => "incomplete_result",
            Self::HostileOutput(_) => "hostile_output",
            Self::Io { .. } => "io",
        }
    }

    pub(crate) fn io(
        action: &'static str,
        path: impl Into<PathBuf>,
        source: std::io::Error,
    ) -> Self {
        Self::Io {
            action,
            path: path.into(),
            source,
        }
    }
}
