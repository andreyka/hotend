//! Reads what Bambu Studio's G-code says about itself, in one pass over the stream.
//!
//! The forms were measured on a file sliced by Bambu Studio 02.09.01.70 for the X2D:
//!
//! - a header of comments between `; HEADER_BLOCK_START` and `; HEADER_BLOCK_END`: the
//!   program and its version (`; BambuStudio <version>`), `; model printing time: <time>;
//!   total estimated time: <time>` (a time is written like `11m 50s`),
//!   `; total layer number: N`, `; total filament length
//!   [mm] : ...`, `; total filament weight [g] : ...`, `; filament: <numbers>`,
//!   `; filament_density: ...` and `; max_z_height: ...`;
//! - the settings the file was sliced with, one `; key = value` comment each, between
//!   `; CONFIG_BLOCK_START` and `; CONFIG_BLOCK_END`;
//! - the commands. The printer's start routine ends at `; MACHINE_START_GCODE_END` and its
//!   end routine begins at `; MACHINE_END_GCODE_START`. Commands may be indented.
//!
//! From the commands this module takes the highest bed temperature (`M140`, `M190`) and
//! nozzle temperature (`M104`, `M109`) and the printed extent: the box around every move
//! that extrudes while moving in X or Y, arcs over their whole path, between the two
//! markers when the file has them. Nothing here judges whether the file is safe to print;
//! that is the G-code check of `hotend-printer`.
//!
//! The stream is hostile input: lines longer than the cap are skipped without being kept,
//! the header and settings are only read from the first bytes of the stream, at most a
//! fixed number of settings is kept, and a number that does not parse is ignored.

use std::collections::BTreeMap;
use std::io::Read;

use super::number;

/// Most `; key = value` settings kept from one file.
const MAX_CONFIG_KEYS: usize = 4096;
/// Longest setting value that is kept.
const MAX_CONFIG_VALUE: usize = 256;
/// Temperatures above this are not believed.
const MAX_TEMPERATURE_C: f64 = 1000.0;
/// Coordinates beyond this are not believed.
const MAX_COORDINATE_MM: f64 = 100_000.0;

/// A box in X, Y and Z.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

impl Bounds {
    fn include(bounds: &mut Option<Self>, point: [f64; 3]) {
        match bounds {
            None => {
                *bounds = Some(Self {
                    min: point,
                    max: point,
                });
            }
            Some(bounds) => {
                for (axis, value) in point.into_iter().enumerate() {
                    bounds.min[axis] = bounds.min[axis].min(value);
                    bounds.max[axis] = bounds.max[axis].max(value);
                }
            }
        }
    }
}

/// What one G-code stream states.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Scan {
    /// The version after `; BambuStudio`.
    pub slicer_version: Option<String>,
    pub total_time_s: Option<f64>,
    pub model_time_s: Option<f64>,
    pub layers: Option<u32>,
    /// The filament numbers of `; filament:`, and the lists that run parallel to it.
    pub filament_ids: Vec<u32>,
    pub filament_mm: Vec<f64>,
    pub filament_g: Vec<f64>,
    pub filament_density: Vec<f64>,
    pub max_z: Option<f64>,
    /// The settings of the config block.
    pub config: BTreeMap<String, String>,
    pub bed_max_c: Option<f64>,
    pub nozzle_max_c: Option<f64>,
    /// The printed extent; `None` when no move extrudes.
    pub printed: Option<Bounds>,
    /// The stream was longer than the scan limit or could not be read to its end, so the
    /// temperatures and the extent are not those of the whole file.
    pub incomplete: bool,
    /// The estimate block of Hotend's stand-in slicer ([`crate::fake`]): what it makes up
    /// for the model, apart from what its file says about itself.
    pub estimate: StandInEstimate,
}

/// The stand-in slicer's estimates, as far as its G-code states them. They are used for a
/// stand-in installation only; a real program's file is never read through them.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct StandInEstimate {
    pub time_s: Option<f64>,
    pub layers: Option<u32>,
    /// Parallel to the header's `; filament:` list.
    pub filament_mm: Vec<f64>,
    pub filament_g: Vec<f64>,
    pub height_mm: Option<f64>,
}

/// Limits of one scan.
#[derive(Debug, Clone, Copy)]
pub struct ScanLimits {
    /// Bytes from the start in which the header and the settings are read.
    pub header_bytes: u64,
    /// Bytes of the stream that are read at all.
    pub scan_bytes: u64,
    /// Longest line that is looked at.
    pub line_bytes: usize,
}

/// `1d 2h 3m 4s` in seconds.
fn duration(text: &str) -> Option<f64> {
    let mut seconds = 0.0;
    let mut parts = 0;
    for part in text.split_whitespace() {
        let unit = part.chars().last()?;
        let factor = match unit {
            'd' => 86_400.0,
            'h' => 3_600.0,
            'm' => 60.0,
            's' => 1.0,
            _ => return None,
        };
        let value: u32 = part[..part.len() - 1].parse().ok()?;
        seconds += f64::from(value) * factor;
        parts += 1;
    }
    (parts > 0 && parts <= 4).then_some(seconds)
}

/// A comma-separated list of non-negative numbers; empty when any entry is not one.
fn numbers(text: &str) -> Vec<f64> {
    let list: Option<Vec<f64>> = text.split(',').take(65).map(number).collect();
    list.filter(|list| list.len() <= 64).unwrap_or_default()
}

/// The value of `; <key><separator> value` when the comment body starts with `key`.
fn after<'a>(body: &'a str, key: &str, separator: char) -> Option<&'a str> {
    body.strip_prefix(key)?
        .trim_start()
        .strip_prefix(separator)
        .map(str::trim)
}

/// The layer count of a `; total layer number: N` comment body.
pub(crate) fn layer_line(body: &str) -> Option<u32> {
    after(body, "total layer number", ':')?
        .parse::<u32>()
        .ok()
        .filter(|layers| *layers > 0)
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum Block {
    None,
    Config,
}

struct State {
    scan: Scan,
    block: Block,
    absolute: bool,
    e_absolute: bool,
    position: [Option<f64>; 3],
    e: f64,
    start_marker: bool,
    end_marker: bool,
    /// Extruding moves of the whole stream, and of the part between the two markers.
    everywhere: Option<Bounds>,
    body: Option<Bounds>,
}

impl State {
    fn comment(&mut self, body: &str, in_header: bool) {
        match body {
            "MACHINE_START_GCODE_END" => self.start_marker = true,
            "MACHINE_END_GCODE_START" => self.end_marker = true,
            "CONFIG_BLOCK_START" if in_header => self.block = Block::Config,
            "CONFIG_BLOCK_END" => self.block = Block::None,
            _ => {}
        }
        if !in_header {
            return;
        }
        if self.block == Block::Config {
            if let Some((key, value)) = body.split_once(" = ").or_else(|| {
                // An empty value is written as `key = ` and loses its last space to `trim`.
                body.strip_suffix(" =").map(|key| (key, ""))
            }) {
                let key = key.trim();
                let plain = !key.is_empty()
                    && key.len() <= 64
                    && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
                if plain
                    && value.len() <= MAX_CONFIG_VALUE
                    && self.scan.config.len() < MAX_CONFIG_KEYS
                {
                    self.scan
                        .config
                        .entry(key.to_owned())
                        .or_insert_with(|| value.trim().to_owned());
                }
            }
            return;
        }
        let scan = &mut self.scan;
        // The stand-in's estimate block. Its keys start with "estimated", which no key of
        // the real header does.
        if body.starts_with("estimated ") {
            use crate::fake::{
                ESTIMATE_HEIGHT, ESTIMATE_LAYERS, ESTIMATE_LENGTH, ESTIMATE_TIME, ESTIMATE_WEIGHT,
            };
            let estimate = &mut scan.estimate;
            if let Some(rest) = after(body, ESTIMATE_TIME, ':') {
                if estimate.time_s.is_none() {
                    estimate.time_s = number(rest);
                }
            } else if let Some(rest) = after(body, ESTIMATE_LAYERS, ':') {
                if estimate.layers.is_none() {
                    estimate.layers = rest.parse::<u32>().ok().filter(|layers| *layers > 0);
                }
            } else if let Some(rest) = after(body, ESTIMATE_LENGTH, ':') {
                if estimate.filament_mm.is_empty() {
                    estimate.filament_mm = numbers(rest);
                }
            } else if let Some(rest) = after(body, ESTIMATE_WEIGHT, ':') {
                if estimate.filament_g.is_empty() {
                    estimate.filament_g = numbers(rest);
                }
            } else if let Some(rest) = after(body, ESTIMATE_HEIGHT, ':')
                && estimate.height_mm.is_none()
            {
                estimate.height_mm = number(rest);
            }
            return;
        }
        if let Some(rest) = body.strip_prefix("BambuStudio ") {
            if scan.slicer_version.is_none() {
                scan.slicer_version = crate::version::SlicerVersion::parse(rest)
                    .map(|version| version.as_str().to_owned());
            }
        } else if let Some(layers) = layer_line(body) {
            scan.layers.get_or_insert(layers);
        } else if let Some(rest) = after(body, "model printing time", ':') {
            let (model, total) = match rest.split_once(';') {
                Some((model, total)) => (model, after(total.trim(), "total estimated time", ':')),
                None => (rest, None),
            };
            if scan.model_time_s.is_none() {
                scan.model_time_s = duration(model);
            }
            if scan.total_time_s.is_none() {
                scan.total_time_s = total.and_then(duration);
            }
        } else if let Some(rest) = after(body, "total filament length [mm]", ':') {
            if scan.filament_mm.is_empty() {
                scan.filament_mm = numbers(rest);
            }
        } else if let Some(rest) = after(body, "total filament weight [g]", ':') {
            if scan.filament_g.is_empty() {
                scan.filament_g = numbers(rest);
            }
        } else if let Some(rest) = after(body, "filament_density", ':') {
            if scan.filament_density.is_empty() {
                scan.filament_density = numbers(rest);
            }
        } else if let Some(rest) = after(body, "max_z_height", ':') {
            if scan.max_z.is_none() {
                scan.max_z = number(rest);
            }
        } else if let Some(rest) = after(body, "filament", ':')
            && scan.filament_ids.is_empty()
        {
            let ids: Option<Vec<u32>> = rest
                .split(',')
                .take(65)
                .map(|id| {
                    id.trim()
                        .parse::<u32>()
                        .ok()
                        .filter(|id| (1..=64).contains(id))
                })
                .collect();
            scan.filament_ids = ids.filter(|ids| ids.len() <= 64).unwrap_or_default();
        }
    }

    fn command(&mut self, line: &str) {
        let mut words = line.split_whitespace();
        let Some(word) = words.next() else {
            return;
        };
        let mut params: [Option<f64>; 7] = [None; 7];
        const LETTERS: [char; 7] = ['X', 'Y', 'Z', 'E', 'I', 'J', 'S'];
        for token in words.take(16) {
            let mut chars = token.chars();
            let Some(letter) = chars.next().map(|c| c.to_ascii_uppercase()) else {
                continue;
            };
            if let Some(index) = LETTERS.iter().position(|known| *known == letter)
                && params[index].is_none()
            {
                params[index] = chars
                    .as_str()
                    .parse::<f64>()
                    .ok()
                    .filter(|value| value.is_finite() && value.abs() <= MAX_COORDINATE_MM);
            }
        }
        let [x, y, z, e, i, j, s] = params;
        let hotter = |known: &mut Option<f64>| {
            if let Some(s) = s.filter(|s| (0.0..=MAX_TEMPERATURE_C).contains(s)) {
                *known = Some(known.map_or(s, |known| known.max(s)));
            }
        };
        match word.to_ascii_uppercase().as_str() {
            "M140" | "M190" => hotter(&mut self.scan.bed_max_c),
            "M104" | "M109" => hotter(&mut self.scan.nozzle_max_c),
            "G90" => self.absolute = true,
            "G91" => self.absolute = false,
            "M82" => self.e_absolute = true,
            "M83" => self.e_absolute = false,
            "G92" => {
                for (axis, value) in [x, y, z].into_iter().enumerate() {
                    if value.is_some() {
                        self.position[axis] = value;
                    }
                }
                if let Some(e) = e {
                    self.e = e;
                }
            }
            "G28" => {
                // Homing: where the head is afterwards is the printer's business.
                self.position = [None; 3];
            }
            command @ ("G0" | "G1" | "G2" | "G3") => {
                let start = self.position;
                for (axis, value) in [x, y, z].into_iter().enumerate() {
                    if let Some(value) = value {
                        self.position[axis] = if self.absolute {
                            Some(value)
                        } else {
                            start[axis].map(|from| from + value)
                        };
                    }
                }
                let extruded = match e {
                    None => 0.0,
                    Some(e) if self.e_absolute => {
                        let delta = e - self.e;
                        self.e = e;
                        delta
                    }
                    Some(e) => e,
                };
                let moved = x.is_some() || y.is_some();
                if extruded > 0.0 && moved {
                    self.printed(command, start, i, j);
                }
            }
            _ => {}
        }
    }

    /// Adds one extruding move to the extent. Points with an unknown coordinate are left
    /// out: a move from an unknown place cannot be placed.
    fn printed(&mut self, command: &str, start: [Option<f64>; 3], i: Option<f64>, j: Option<f64>) {
        let known = |p: [Option<f64>; 3]| Some([p[0]?, p[1]?, p[2]?]);
        let Some(end) = known(self.position) else {
            return;
        };
        let mut points = vec![end];
        if let Some(start) = known(start) {
            points.push(start);
            if command != "G1" && command != "G0" {
                let (i, j) = (i.unwrap_or(0.0), j.unwrap_or(0.0));
                points.extend(arc_extremes(start, end, i, j, command == "G3"));
            }
        }
        let inside = self.start_marker && !self.end_marker;
        for point in points {
            Bounds::include(&mut self.everywhere, point);
            if inside {
                Bounds::include(&mut self.body, point);
            }
        }
    }
}

/// The points where an arc from `start` to `end` around `start + (i, j)` is furthest out
/// along X or Y. `ccw` is the direction of `G3`.
fn arc_extremes(start: [f64; 3], end: [f64; 3], i: f64, j: f64, ccw: bool) -> Vec<[f64; 3]> {
    use std::f64::consts::{FRAC_PI_2, TAU};
    let centre = [start[0] + i, start[1] + j];
    let radius = i.hypot(j);
    if !(radius.is_finite() && radius > 0.0 && radius <= MAX_COORDINATE_MM) {
        return Vec::new();
    }
    let from = (start[1] - centre[1]).atan2(start[0] - centre[0]);
    let to = (end[1] - centre[1]).atan2(end[0] - centre[0]);
    let turn = |from: f64, to: f64| {
        let turn = if ccw { to - from } else { from - to }.rem_euclid(TAU);
        // The same point at both ends is a full circle.
        if turn < 1e-9 { TAU } else { turn }
    };
    let sweep = turn(from, to);
    (0..4)
        .map(|quarter| f64::from(quarter) * FRAC_PI_2)
        .filter(|angle| {
            let reached = if ccw { angle - from } else { from - angle }.rem_euclid(TAU);
            reached <= sweep
        })
        .map(|angle| {
            [
                centre[0] + radius * angle.cos(),
                centre[1] + radius * angle.sin(),
                end[2],
            ]
        })
        .collect()
}

/// Scans a G-code stream.
pub(crate) fn scan<R: Read>(mut reader: R, limits: &ScanLimits) -> Scan {
    let mut state = State {
        scan: Scan::default(),
        block: Block::None,
        absolute: true,
        e_absolute: true,
        position: [None; 3],
        e: 0.0,
        start_marker: false,
        end_marker: false,
        everywhere: None,
        body: None,
    };
    let mut line: Vec<u8> = Vec::new();
    let mut too_long = false;
    let mut line_start: u64 = 0;
    let mut offset: u64 = 0;
    let mut chunk = vec![0u8; 64 * 1024];
    let finish_line = |state: &mut State, line: &mut Vec<u8>, too_long: bool, start: u64| {
        if !too_long && let Ok(text) = std::str::from_utf8(line) {
            let text = text.trim();
            match text.strip_prefix(';') {
                Some(body) => state.comment(body.trim(), start < limits.header_bytes),
                None => {
                    let code = text.split(';').next().unwrap_or("");
                    if !code.is_empty() {
                        state.command(code);
                    }
                }
            }
        }
        line.clear();
    };
    'read: loop {
        let wanted = (limits.scan_bytes - offset).min(chunk.len() as u64) as usize;
        if wanted == 0 {
            // There may be more: say so if there is.
            let mut probe = [0u8; 1];
            state.scan.incomplete = !matches!(reader.read(&mut probe), Ok(0));
            break;
        }
        let read = match reader.read(&mut chunk[..wanted]) {
            Ok(0) => break,
            Ok(read) => read,
            Err(_) => {
                state.scan.incomplete = true;
                break 'read;
            }
        };
        for &byte in &chunk[..read] {
            offset += 1;
            if byte == b'\n' {
                finish_line(&mut state, &mut line, too_long, line_start);
                too_long = false;
                line_start = offset;
            } else if line.len() < limits.line_bytes {
                line.push(byte);
            } else {
                too_long = true;
            }
        }
    }
    if !state.scan.incomplete {
        finish_line(&mut state, &mut line, too_long, line_start);
    }
    state.scan.printed = if state.start_marker {
        state.body
    } else {
        state.everywhere
    };
    if state.scan.incomplete {
        // Half a file's extent and temperatures would be read as the whole file's.
        state.scan.printed = None;
        state.scan.bed_max_c = None;
        state.scan.nozzle_max_c = None;
    }
    state.scan
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn limits() -> ScanLimits {
        ScanLimits {
            header_bytes: 64 * 1024,
            scan_bytes: 1 << 30,
            line_bytes: 1024,
        }
    }

    fn scan_text(text: &str) -> Scan {
        scan(text.as_bytes(), &limits())
    }

    /// A file in the form Bambu Studio writes, written by hand for this test: the block
    /// markers, the header keys and the command words are the real ones, every value is
    /// made up.
    pub(crate) const SLICED: &str = "\
; HEADER_BLOCK_START
; BambuStudio 02.09.01.70
; model printing time: 2m 5s; total estimated time: 1h 3m 20s
; total layer number: 3
; total filament length [mm] : 1250.50,300.25
; total filament volume [cm^3] : 3007.80,722.10
; total filament weight [g] : 3.79,0.95
; filament_density: 1.26,1.31
; filament_diameter: 1.75,1.75
; max_z_height: 0.60
; filament: 1,2
; HEADER_BLOCK_END

; CONFIG_BLOCK_START
; bed_exclude_area =
; curr_bed_type = Textured PEI Plate
; cool_plate_temp = 35,35
; textured_plate_temp = 55,60
; nozzle_temperature = 220,240
; filament_map = 1,2
; some_gcode = G1 X999 Y999 E5
; CONFIG_BLOCK_END

; EXECUTABLE_BLOCK_START
M73 P0 R63
  M140 S60 ; heat the bed
    M104 S150 A
G28 X
G90
M83
  M190 S60
  M109 S240
G1 X10 Y250 Z0.4 F3000
G1 X200 Y250 E12 ; a purge line of the start routine
; MACHINE_START_GCODE_END
; CHANGE_LAYER
; Z_HEIGHT: 0.2
G1 X20.5 Y30.25 Z.2 F6000
G1 X60.5 Y30.25 E1.5
G1 X60.5 Y70.75 E1.5
G1 E-.8 F1800
G1 X100 Y100
G1 E.8
G1 X60.5 Y70.75
; CHANGE_LAYER
G1 Z.4
G1 X20.5 Y70.75 E1.5
G1 Z.6
G3 X20.5 Y30.25 I0 J-20.25 E2
; MACHINE_END_GCODE_START
G1 X128 Y250 E-1
G1 X5 Y5 E3
M140 S0
M104 S0 T0
; EXECUTABLE_BLOCK_END
";

    #[test]
    fn the_header_and_the_settings_are_read() {
        let scan = scan_text(SLICED);
        assert_eq!(scan.slicer_version.as_deref(), Some("02.09.01.70"));
        assert_eq!(scan.total_time_s, Some(3800.0));
        assert_eq!(scan.model_time_s, Some(125.0));
        assert_eq!(scan.layers, Some(3));
        assert_eq!(scan.filament_ids, [1, 2]);
        assert_eq!(scan.filament_mm, [1250.5, 300.25]);
        assert_eq!(scan.filament_g, [3.79, 0.95]);
        assert_eq!(scan.filament_density, [1.26, 1.31]);
        assert_eq!(scan.max_z, Some(0.6));
        assert_eq!(scan.config["curr_bed_type"], "Textured PEI Plate");
        assert_eq!(scan.config["textured_plate_temp"], "55,60");
        assert_eq!(scan.config["bed_exclude_area"], "");
        assert_eq!(scan.config["some_gcode"], "G1 X999 Y999 E5");
        assert!(!scan.incomplete);
    }

    #[test]
    fn temperatures_and_the_printed_extent_come_from_the_commands() {
        let scan = scan_text(SLICED);
        // The highest values that are commanded, indented or not.
        assert_eq!(scan.bed_max_c, Some(60.0));
        assert_eq!(scan.nozzle_max_c, Some(240.0));
        // Only moves between the two markers that extrude while moving count: not the
        // purge line of the start routine, not the travel to 100/100, not the retraction,
        // not the end routine, and not the G-code inside a setting.
        let printed = scan.printed.unwrap();
        // The last move is a half circle to the left around 20.5/50.5 with radius 20.25:
        // it reaches X 0.25, further out than any straight move.
        assert_eq!(printed.min, [0.25, 30.25, 0.2]);
        assert_eq!(printed.max, [60.5, 70.75, 0.6]);
    }

    #[test]
    fn arcs_count_over_their_whole_path() {
        // A half circle around 50/50 with radius 10, from 60/50 over the top to 40/50.
        let half = scan_text("G90\nM83\nG1 X60 Y50 Z0.2\nG3 X40 Y50 I-10 J0 E1\n");
        let printed = half.printed.unwrap();
        assert!((printed.max[1] - 60.0).abs() < 1e-9, "{printed:?}");
        assert!((printed.min[1] - 50.0).abs() < 1e-9, "{printed:?}");
        assert_eq!((printed.min[0], printed.max[0]), (40.0, 60.0));
        // The same end points clockwise go under the bottom.
        let under = scan_text("G90\nM83\nG1 X60 Y50 Z0.2\nG2 X40 Y50 I-10 J0 E1\n");
        let printed = under.printed.unwrap();
        assert!((printed.min[1] - 40.0).abs() < 1e-9, "{printed:?}");
        assert!((printed.max[1] - 50.0).abs() < 1e-9, "{printed:?}");
        // A full circle.
        let full = scan_text("G90\nM83\nG1 X60 Y50 Z0.2\nG2 X60 Y50 I-10 J0 E1\n");
        let printed = full.printed.unwrap();
        for (value, expected) in [
            (printed.min[0], 40.0),
            (printed.max[0], 60.0),
            (printed.min[1], 40.0),
            (printed.max[1], 60.0),
        ] {
            assert!((value - expected).abs() < 1e-9, "{printed:?}");
        }
    }

    #[test]
    fn modes_and_unknown_positions_are_respected() {
        // Absolute extrusion: only a growing E prints. Relative moves add up.
        let scan = scan_text(
            "G90\nM82\nG92 E0\nG1 X10 Y10 Z1\nG1 X20 Y10 E1\nG1 X30 Y10 E1\nG1 X40 Y10 E0.5\n\
             G91\nG1 X5 Y5 E2\n",
        );
        let printed = scan.printed.unwrap();
        assert_eq!(printed.min, [10.0, 10.0, 1.0]);
        assert_eq!(printed.max, [45.0, 15.0, 1.0]);
        // Before any absolute position is known nothing can be placed.
        assert_eq!(scan_text("G91\nM83\nG1 X5 Y5 E1\n").printed, None);
        assert_eq!(scan_text("G90\nM83\nG1 X5 Y5 E1\n").printed, None);
        // After homing the position is unknown again.
        let homed = scan_text("G90\nM83\nG1 X5 Y5 Z1\nG28\nG1 X9 E1\n");
        assert_eq!(homed.printed, None);
        // A file without the markers counts every extruding move.
        let plain = scan_text("G90\nM83\nG1 X1 Y2 Z3\nG1 X4 Y5 E1\n");
        assert_eq!(plain.printed.unwrap().max, [4.0, 5.0, 3.0]);
        // Comments only: nothing printed, no temperatures.
        let comments = scan_text("; G1 X1 Y1 E1\n; M140 S60\n");
        assert_eq!(
            (comments.printed, comments.bed_max_c, comments.nozzle_max_c),
            (None, None, None)
        );
    }

    #[test]
    fn hostile_streams_are_bounded() {
        // Nonsense numbers are ignored.
        let odd = scan_text(
            "M140 S99999\nM140 S-5\nM140 Sabc\nM104 SNaN\nG90\nM83\nG1 X1e300 Y5 Z1 E1\nG1 Xinf Y5 E1\n",
        );
        assert_eq!(
            (odd.bed_max_c, odd.nozzle_max_c, odd.printed),
            (None, None, None)
        );
        // A line over the cap is skipped, not kept, and the next line is read normally.
        let long = format!(
            "; total layer number: {}\n; total layer number: 4\nM140 S{}\nM140 S50\n",
            "9".repeat(5000),
            "5".repeat(5000)
        );
        let skipped = scan_text(&long);
        assert_eq!((skipped.layers, skipped.bed_max_c), (Some(4), Some(50.0)));
        // The header is read only from the start of the stream.
        let late = format!(
            "{}; total layer number: 7\nM140 S45\n",
            "; filler\n".repeat(200)
        );
        let short = ScanLimits {
            header_bytes: 256,
            ..limits()
        };
        let late = scan(late.as_bytes(), &short);
        assert_eq!((late.layers, late.bed_max_c), (None, Some(45.0)));
        // A stream longer than the scan limit gives no extent and no temperatures.
        let cut = ScanLimits {
            scan_bytes: 30,
            ..limits()
        };
        let cut = scan(
            "; total layer number: 2\nM140 S60\nG90\nM83\nG1 X1 Y1 Z1\nG1 X2 E1\n".as_bytes(),
            &cut,
        );
        assert!(cut.incomplete);
        assert_eq!(
            (cut.layers, cut.bed_max_c, cut.printed),
            (Some(2), None, None)
        );
        // Too many settings: the first ones are kept.
        let mut many = String::from("; CONFIG_BLOCK_START\n");
        for i in 0..5000 {
            many.push_str(&format!("; k{i} = {i}\n"));
        }
        let many = scan(
            many.as_bytes(),
            &ScanLimits {
                header_bytes: 1 << 20,
                ..limits()
            },
        );
        assert_eq!(many.config.len(), MAX_CONFIG_KEYS);
        // Durations and lists.
        assert_eq!(duration("1d 2h 3m 4s"), Some(93_784.0));
        assert_eq!(duration("50s"), Some(50.0));
        for bad in ["", "soon", "5", "5x", "-5s", "1h 2h 3h 4h 5h"] {
            assert_eq!(duration(bad), None, "{bad}");
        }
        assert_eq!(numbers("1.5, 2"), [1.5, 2.0]);
        assert!(numbers("1.5,x").is_empty());
    }

    #[test]
    fn layer_lines_are_parsed_strictly() {
        let count = |text: &str| scan_text(text).layers;
        assert_eq!(count("; total layer number: 12\n"), Some(12));
        assert_eq!(count(";total layer number:3\r\n"), Some(3));
        assert_eq!(count("; total layer number: 0\n"), None);
        assert_eq!(count("; total layer number: many\n"), None);
        assert_eq!(count("G1 X1 ; total layer number: 9\n"), None);
        assert_eq!(
            count(&format!(";{}total layer number: 5\n", " ".repeat(2000))),
            None
        );
    }
}
