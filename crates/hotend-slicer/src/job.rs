//! What a caller asks for ([`SliceJob`]) and what it gets back ([`SliceResult`]).

use std::path::PathBuf;

use hotend_settings::PrintSettings;
use serde::{Deserialize, Serialize};

/// One slice. Settings are named by id only: a job cannot carry setting values or G-code.
#[derive(Debug, Clone, PartialEq)]
pub struct SliceJob {
    /// An existing folder. The slicer can write nowhere else, and everything this crate
    /// writes goes below it: `profiles/`, `output/`, `slicer-config/` and `.sandbox/`, which
    /// are emptied at the start of every slice.
    pub job_dir: PathBuf,
    /// Model files, relative to `job_dir`. Bambu Studio is documented to read 3MF and STL;
    /// PrusaSlicer also reads STEP.
    pub inputs: Vec<PathBuf>,
    /// Printer id from `profiles/printers.toml`. It selects the nozzle rules, the slicer and
    /// the machine settings profile of the same id.
    pub printer: String,
    /// Process settings profile id.
    pub process_profile: String,
    /// The filaments, slot 1 first, each with the nozzle that prints it.
    pub filaments: Vec<FilamentAssignment>,
    /// Plate to slice, from 1.
    pub plate: u32,
    /// Let the slicer arrange the models on the plate. With `false` they stay where the
    /// input file puts them.
    pub arrange: bool,
    /// Bounding boxes of what each slot prints, for the reach check. Empty means the check
    /// is skipped and the result says so.
    pub extents: Vec<Extent>,
    /// The plate that is on the printer: an id from `profiles/system-profiles.toml`
    /// (`cool`, `textured-pei`, `high-temp`, `engineering`). The application passes the
    /// plate of its settings here. `None` is the configured default
    /// ([`crate::SlicerConfig::default_bed_type`]). Bambu Studio only; PrusaSlicer
    /// takes the bed temperature from its filament settings.
    pub bed_type: Option<String>,
    /// Hotend's print settings for this plate, from its print plan. With a real Bambu
    /// Studio they select the process preset and the material and are put on top of the
    /// flattened system profiles through the keymap ([`crate::keymap`]), and the sliced file
    /// must pass the proof ([`crate::proof`]). `None` slices with the installed profiles as
    /// they are (with the warning `print_settings_missing` for a real Bambu Studio). The
    /// stand-in slicer ignores them.
    pub print_settings: Option<PrintSettings>,
    /// The diameter of the nozzle fitted to the printer, in mm. A Bambu Studio job for a size
    /// Hotend has no system profile for is refused (`nozzle_not_supported`). `None` is not
    /// checked.
    pub nozzle_diameter_mm: Option<f64>,
    /// Hotend's own renders of the plate, to make the plate previews from when the
    /// slicer wrote none ([`crate::previews`]). `None` (the default setting) adds nothing.
    /// Bambu Studio only.
    pub plate_previews: Option<PlatePreviews>,
}

/// Hotend's own renders of a prepared plate (PNG files the engine drew), from which the
/// plate previews of a sliced file are made when the slicer wrote none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatePreviews {
    /// The isometric view: `plate_<n>.png`, `plate_<n>_small.png`, `plate_no_light_<n>.png`.
    pub iso: PathBuf,
    /// The view from above: `top_<n>.png` and `pick_<n>.png`.
    pub top: PathBuf,
}

/// One filament slot and the nozzle it is routed to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilamentAssignment {
    /// Slot number from 1, as the slicer counts filaments.
    pub slot: u32,
    /// Filament settings profile id.
    pub filament_profile: String,
    /// Material family, for example `PLA` or `TPU`, for the material-to-nozzle rules.
    pub family: String,
    /// Device extruder id of the nozzle, as the printer reports it.
    pub nozzle_id: i64,
}

/// The bounding box of one part and the slot that prints it.
#[derive(Debug, Clone, PartialEq)]
pub struct Extent {
    pub name: String,
    pub slot: u32,
    pub min: [f64; 3],
    pub max: [f64; 3],
    /// `true` when the box is in bed coordinates and the slicer will not move the part.
    pub placed: bool,
}

/// The `SliceResult` of PROTOCOL.md section 6. A number the slicer's output did not state is
/// `None` (JSON `null`) and has a warning; it is never estimated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SliceResult {
    /// Always `ok`: a slice that failed is an error, not a result.
    pub status: String,
    /// The sliced file, relative to the job folder.
    pub output: String,
    /// SHA-256 of the sliced file, lower-case hex.
    pub sha256: String,
    /// Estimated print time in seconds.
    pub time_s: Option<f64>,
    /// One entry per filament slot of the job, in slot order.
    pub filament: Vec<FilamentUsage>,
    pub layers: Option<u32>,
    /// The plate the file was sliced for and how hot it heats the bed. That plate must be
    /// on the printer. `None` when the slicer has no plate types (PrusaSlicer).
    pub bed: Option<BedReport>,
    /// The highest nozzle temperature of the file, in degrees Celsius.
    pub nozzle_temperature_c: Option<f64>,
    /// Where the file prints, read from the sliced file.
    pub extent: Option<PrintedExtent>,
    pub warnings: Vec<SliceWarning>,
    pub slicer: SlicerInfo,
}

/// The plate a file was sliced for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BedReport {
    /// Hotend's id of the plate type the job asked for.
    pub id: String,
    /// The plate's name as the sliced file states it, for example `Textured PEI Plate`.
    pub plate_type: String,
    /// The highest bed temperature the file commands, in degrees Celsius; `None` (with the
    /// warning `bed_temperature_unknown`) when the file does not state one.
    pub temperature_c: Option<f64>,
}

/// The box a sliced file prints in, in bed coordinates and millimetres.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrintedExtent {
    pub x: [f64; 2],
    pub y: [f64; 2],
    /// `None` when the source does not give heights.
    pub z: Option<[f64; 2]>,
    /// `gcode`: the box around the extruding moves of the G-code. `plate_metadata`: the
    /// outline of the objects on the plate as the slicer recorded it, used when the G-code
    /// has no moves to read.
    pub source: String,
}

/// Filament used by one slot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FilamentUsage {
    pub slot: u32,
    pub grams: Option<f64>,
    pub metres: Option<f64>,
    /// Device extruder id of the nozzle the job routed this slot to.
    pub nozzle_id: i64,
}

/// Something the caller should know about a result that was still produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SliceWarning {
    pub code: String,
    pub message: String,
}

impl SliceWarning {
    pub(crate) fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
        }
    }
}

/// The program that produced a result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlicerInfo {
    pub name: String,
    /// The version the program reported, or `unknown`.
    pub version: String,
}

/// Whether `id` can name a settings profile: 1 to 120 characters from letters, digits,
/// space and `._@()+-`, not starting with a dot or a dash and without `..`.
pub fn valid_profile_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 120
        && !id.starts_with(['.', '-', ' '])
        && !id.ends_with(' ')
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || " ._@()+-".contains(c))
}

/// Makes text that came out of a slicer safe to show: printable characters only, one line,
/// at most `max` characters.
pub(crate) fn untrusted_text(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut count = 0;
    let mut last_space = true;
    for c in text.chars() {
        let c = if c.is_control() || c.is_whitespace() {
            ' '
        } else {
            c
        };
        if c == ' ' && last_space {
            continue;
        }
        if count == max {
            out.push('…');
            break;
        }
        last_space = c == ' ';
        out.push(c);
        count += 1;
    }
    out.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_ids_cannot_name_a_path() {
        for good in [
            "demo-0.20mm",
            "0.20mm Standard @BBL X2D",
            "PLA (basic)+1",
            "a",
        ] {
            assert!(valid_profile_id(good), "{good}");
        }
        let long = "a".repeat(121);
        for bad in [
            "",
            ".hidden",
            "..",
            "a/b",
            "a\\b",
            "../x",
            "x..y",
            "-rf",
            " x",
            "x ",
            "a;b",
            "a\nb",
            "a\0b",
            "é",
            long.as_str(),
        ] {
            assert!(!valid_profile_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn untrusted_text_is_flattened_and_capped() {
        assert_eq!(untrusted_text("a\n\tb\u{1b}[31m  c", 40), "a b [31m c");
        assert_eq!(untrusted_text("abcdef", 3), "abc…");
        assert_eq!(untrusted_text("  \n ", 10), "");
    }
}
