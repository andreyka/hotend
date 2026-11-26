//! Reading what a slicer wrote, as hostile input.
//!
//! Guarantees:
//! - the sliced file is used only if it is a regular file inside the job folder, reached
//!   without a symbolic link, with one hard link and under the size limit
//!   ([`hotend_core::import::resolve_inside`]), and it is hashed and parsed through one
//!   open handle, so the hash is of the bytes that were parsed;
//! - no parser reads more than its byte cap, whatever sizes the file declares;
//! - a number the file does not state stays `None` and gets a warning. Unknown fields are
//!   ignored.

pub mod bambu;
pub mod gcode;
pub mod prusa;
pub mod result_json;

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::SlicerError;
use crate::job::{PrintedExtent, SliceWarning};

/// Size and count limits for a slicer's output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputLimits {
    /// Largest sliced file.
    pub max_file_bytes: u64,
    /// Most entries a `.gcode.3mf` archive may have.
    pub max_zip_entries: u64,
    /// Largest central directory of the archive.
    pub max_central_directory_bytes: u64,
    /// Largest declared uncompressed size of one archive entry.
    pub max_entry_bytes: u64,
    /// Largest declared uncompressed size of all entries together.
    pub max_total_bytes: u64,
    /// Highest declared compression ratio of an entry larger than 1 MiB.
    pub max_compression_ratio: u64,
    /// Most bytes read from a metadata entry such as `Metadata/slice_info.config`.
    pub max_metadata_bytes: u64,
    /// Bytes from the start of the G-code inside a `.gcode.3mf` in which its header and
    /// its settings block are read.
    pub max_gcode_header_bytes: u64,
    /// Most bytes of that G-code that are scanned for temperatures and the printed extent.
    /// A longer file is still hashed and returned; its extent is then unknown.
    pub max_gcode_scan_bytes: u64,
    /// Longest G-code line that is looked at; longer lines are skipped.
    pub max_line_bytes: usize,
}

impl Default for OutputLimits {
    fn default() -> Self {
        const MIB: u64 = 1024 * 1024;
        Self {
            max_file_bytes: 4096 * MIB,
            max_zip_entries: 2048,
            max_central_directory_bytes: 8 * MIB,
            max_entry_bytes: 4096 * MIB,
            max_total_bytes: 8192 * MIB,
            max_compression_ratio: 200,
            max_metadata_bytes: 4 * MIB,
            max_gcode_header_bytes: MIB,
            max_gcode_scan_bytes: 4096 * MIB,
            max_line_bytes: 16 * 1024,
        }
    }
}

/// What a parser could read from a sliced file. A field the file does not state is `None`.
#[derive(Debug, Default, PartialEq)]
pub struct Parsed {
    pub time_s: Option<f64>,
    pub layers: Option<u32>,
    /// `(grams, metres)` by the slicer's 1-based filament number.
    pub filaments: BTreeMap<u32, (Option<f64>, Option<f64>)>,
    /// The filament map the slicer says it used: slicer extruder per filament, in order.
    pub reported_map: Option<Vec<u32>>,
    /// The plate type the file says it was sliced for (the slicer's own name for it).
    pub bed_type: Option<String>,
    /// The highest bed temperature of the file, in degrees Celsius: the one its commands
    /// set, or the one its settings give for the plate when it has no such command.
    pub bed_temperature_c: Option<f64>,
    /// The highest bed temperature the file's commands set.
    pub bed_commanded_c: Option<f64>,
    /// The lowest and the highest bed temperature that the file's settings give for the
    /// plate it says it was sliced for (over its filaments and over the first and the
    /// other layers).
    pub bed_setting_c: Option<[f64; 2]>,
    /// The highest nozzle temperature of the file, in degrees Celsius.
    pub nozzle_temperature_c: Option<f64>,
    /// Where the file prints.
    pub extent: Option<PrintedExtent>,
    /// The version the slicer wrote into the file.
    pub slicer_version: Option<String>,
    /// The file states a filament density of 0: its filament settings were incomplete.
    pub density_missing: bool,
    /// The estimates of Hotend's stand-in slicer, when the G-code carries its estimate
    /// block: time in seconds, layers, `(grams, metres)` by filament number, and the
    /// height of the model. See [`Parsed::use_stand_in_estimate`].
    pub stand_in: Option<StandInNumbers>,
    pub warnings: Vec<SliceWarning>,
}

/// What the stand-in slicer estimates for the model it was given.
#[derive(Debug, Clone, PartialEq)]
pub struct StandInNumbers {
    pub time_s: f64,
    pub layers: u32,
    pub filaments: BTreeMap<u32, (f64, f64)>,
    pub height_mm: Option<f64>,
}

impl Parsed {
    /// Replaces the file's own numbers by the stand-in slicer's estimates.
    ///
    /// The stand-in's file prints nothing and says so: its time, filament and layers are
    /// none. What the stand-in makes up for the model stands in a block of its own, and
    /// that is what a stand-in run reports, under the warning `fake_slicer`. Call this
    /// for a stand-in installation only: a real program's file is believed for what it
    /// says about itself, whatever else it carries.
    pub(crate) fn use_stand_in_estimate(&mut self) {
        let Some(estimate) = self.stand_in.take() else {
            return;
        };
        self.time_s = Some(estimate.time_s);
        self.layers = Some(estimate.layers);
        self.filaments = estimate
            .filaments
            .into_iter()
            .map(|(id, (grams, metres))| (id, (Some(grams), Some(metres))))
            .collect();
        // The outline the stand-in records beside its G-code has no height of its own.
        if let (Some(extent), Some(height)) = (&mut self.extent, estimate.height_mm)
            && extent.source != "gcode"
        {
            extent.z = Some([0.0, height]);
        }
        self.warnings
            .retain(|warning| !matches!(warning.code.as_str(), "time_unknown" | "layers_unknown"));
    }

    pub(crate) fn warn(&mut self, code: &str, message: impl Into<String>) {
        // A hostile file must not be able to flood the result with warnings.
        if self.warnings.len() < 64 {
            self.warnings.push(SliceWarning::new(code, message));
        }
    }
}

/// Opens the first of `candidates` (paths relative to `job_dir`) that exists, after checking
/// that it is a plain file inside the job folder. `Ok(None)` when none exists.
pub(crate) fn open_output(
    job_dir: &Path,
    candidates: &[String],
    limits: &OutputLimits,
) -> Result<Option<(String, File)>, SlicerError> {
    for relative in candidates {
        // A symbolic link counts as present, so that it is refused rather than skipped.
        if std::fs::symlink_metadata(job_dir.join(relative)).is_err() {
            continue;
        }
        let path = hotend_core::import::resolve_inside(job_dir, relative, limits.max_file_bytes)
            .map_err(|e| SlicerError::UnusableOutput(e.to_string()))?;
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let file = options
            .open(&path)
            .map_err(|e| SlicerError::io("open", &path, e))?;
        let meta = file
            .metadata()
            .map_err(|e| SlicerError::io("inspect", &path, e))?;
        if !meta.is_file() || meta.len() > limits.max_file_bytes {
            return Err(SlicerError::UnusableOutput(format!(
                "`{relative}` is not a regular file under the size limit"
            )));
        }
        if meta.len() == 0 {
            return Err(SlicerError::UnusableOutput(format!(
                "`{relative}` is empty"
            )));
        }
        return Ok(Some((relative.clone(), file)));
    }
    Ok(None)
}

/// SHA-256 of an open file from its start, as lower-case hex. Leaves the file at its start.
pub(crate) fn sha256_of(file: &mut File, path_for_errors: &Path) -> Result<String, SlicerError> {
    let io = |e| SlicerError::io("read", path_for_errors, e);
    file.seek(SeekFrom::Start(0)).map_err(io)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(io)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    file.seek(SeekFrom::Start(0)).map_err(io)?;
    Ok(hotend_core::util::hex(&hasher.finalize()))
}

/// SHA-256 of bytes in memory, as lower-case hex.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hotend_core::util::hex(&Sha256::digest(bytes))
}

/// Parses a non-negative finite number; anything else is `None`.
pub(crate) fn number(text: &str) -> Option<f64> {
    let value: f64 = text.trim().parse().ok()?;
    (value.is_finite() && value >= 0.0).then_some(value)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn numbers_must_be_finite_and_not_negative() {
        assert_eq!(number(" 12.5 "), Some(12.5));
        assert_eq!(number("0"), Some(0.0));
        for bad in ["", "abc", "-1", "NaN", "inf", "1e999", "1,5", "0x10"] {
            assert_eq!(number(bad), None, "{bad}");
        }
    }

    #[test]
    fn only_a_plain_file_inside_the_job_folder_is_opened() {
        let tmp = tempfile::tempdir().unwrap();
        let job = tmp.path().join("job");
        std::fs::create_dir_all(job.join("output")).unwrap();
        let limits = OutputLimits {
            max_file_bytes: 16,
            ..OutputLimits::default()
        };
        let candidates = ["output/a.gcode".to_owned(), "a.gcode".to_owned()];
        assert!(open_output(&job, &candidates, &limits).unwrap().is_none());

        std::fs::write(job.join("a.gcode"), b"G1 X1").unwrap();
        let (name, mut file) = open_output(&job, &candidates, &limits).unwrap().unwrap();
        assert_eq!(name, "a.gcode");
        let hash = sha256_of(&mut file, Path::new("a.gcode")).unwrap();
        assert_eq!(hash, hotend_core::util::sha256_hex(b"G1 X1"));
        assert_eq!(hash.len(), 64);
        // Hashing leaves the handle at the start for the parser.
        let mut again = String::new();
        file.read_to_string(&mut again).unwrap();
        assert_eq!(again, "G1 X1");

        std::fs::write(job.join("output/a.gcode"), b"").unwrap();
        let empty = open_output(&job, &candidates, &limits).unwrap_err();
        assert_eq!(empty.code(), "unusable_output");
        std::fs::write(job.join("output/a.gcode"), vec![b'x'; 17]).unwrap();
        assert_eq!(
            open_output(&job, &candidates, &limits).unwrap_err().code(),
            "unusable_output"
        );
        std::fs::remove_file(job.join("output/a.gcode")).unwrap();
        std::fs::create_dir(job.join("output/a.gcode")).unwrap();
        assert!(open_output(&job, &candidates, &limits).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn links_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let job = tmp.path().join("job");
        std::fs::create_dir_all(job.join("output")).unwrap();
        let secret = tmp.path().join("secret");
        std::fs::write(&secret, b"secret").unwrap();
        let limits = OutputLimits::default();
        let candidates = ["output/a.gcode".to_owned()];

        std::os::unix::fs::symlink(&secret, job.join("output/a.gcode")).unwrap();
        let error = open_output(&job, &candidates, &limits).unwrap_err();
        assert!(error.to_string().contains("symbolic link"), "{error}");
        std::fs::remove_file(job.join("output/a.gcode")).unwrap();

        std::fs::hard_link(&secret, job.join("output/a.gcode")).unwrap();
        let error = open_output(&job, &candidates, &limits).unwrap_err();
        assert!(error.to_string().contains("hard links"), "{error}");
        std::fs::remove_file(job.join("output/a.gcode")).unwrap();

        // A linked folder on the way is refused too.
        std::fs::remove_dir(job.join("output")).unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("a.gcode"), b"G1").unwrap();
        std::os::unix::fs::symlink(&elsewhere, job.join("output")).unwrap();
        let error = open_output(&job, &candidates, &limits).unwrap_err();
        assert!(error.to_string().contains("symbolic link"), "{error}");
    }
}
