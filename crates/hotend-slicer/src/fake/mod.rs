//! The fake slicer: a stand-in for a vendor slicer in tests and in the offline demo.
//!
//! It accepts the command-line style Hotend uses for Bambu Studio (`--slice`,
//! `--load-settings`, `--export-3mf`, ...) and for PrusaSlicer (`--export-gcode`, `--load`,
//! `--output`), reads the same settings files and writes files of the same shape. Time,
//! filament and layers are simple estimates from the mesh volume and height.
//!
//! In the Bambu Studio style it behaves the way the real program was measured to behave
//! (02.09.01.70), so that Hotend's tests take the same path as a real run:
//! - it prints nothing, whether it succeeds or fails. What happened is in `result.json` in
//!   the output folder (in the working directory when no output folder was given), in the
//!   real shape: `return_code`, `error_string` and one `sliced_plates` entry per plate;
//! - it writes `plate_<n>.gcode` and the `.gcode.3mf` named by `--export-3mf` into the
//!   output folder; the archive holds the G-code, its MD5 (`Metadata/plate_<n>.gcode.md5`),
//!   `Metadata/slice_info.config` and `Metadata/plate_<n>.json`;
//! - its G-code says in comments what it was sliced for, as the real program's settings
//!   block does: the printer model and the nozzle sizes of the machine settings it was
//!   given (when they state them) and the plate type;
//! - it moves the parts to the middle of the bed unless it is told `--arrange 0`;
//! - it does not resolve `inherits` or `include` in a settings file. A filament file
//!   without a density of its own therefore gives a weight of 0.00 g and an empty filament
//!   id, which is what the real program did with a system profile that was not flattened;
//! - it slices for the Cool Plate unless `curr_bed_type` says otherwise (on the command
//!   line or in the process settings), and takes the bed temperature from the filament
//!   setting of that plate.
//!
//! What it is not: a slicer. The "G-code" it writes is comments only, so a printer that
//! received it would do nothing. It rejects options it does not know, so a change to
//! Hotend's command lines shows up as a failing test, but it cannot show whether a real
//! slicer accepts them.
//!
//! Because the file prints nothing, everything it says about itself says so: the time,
//! the filament length and weight in the G-code header and in `slice_info.config` are 0,
//! and no layer count is stated. A file that claimed a print it does not contain would be
//! refused by the printer layer's G-code check, which holds those claims to the commands;
//! this file passes that check on what it is, an empty print, with the warning
//! `no_moves`. The numbers the stand-in makes up for the model are not claims of the
//! file. They stand in a block of their own (`HOTEND_ESTIMATE_BLOCK_START` to
//! `HOTEND_ESTIMATE_BLOCK_END`), which says what it is; Hotend's adapter reads it for a
//! stand-in installation only, and `result.json` carries the same estimates.

pub mod md5;
pub mod mesh;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use self::mesh::MeshStats;
use crate::detect::FAKE_BANNER;
use crate::output::result_json::RESULT_NAME;

/// Version the fake slicer reports.
pub const FAKE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Share of the mesh volume that ends up as extruded plastic (walls plus sparse infill).
const FILL_FRACTION: f64 = 0.35;
/// Volumetric flow the time estimate assumes, in mm3/s.
const FLOW_MM3_S: f64 = 8.0;
/// Fixed time per layer and per job, in seconds.
const SECONDS_PER_LAYER: f64 = 3.0;
const SECONDS_PER_JOB: f64 = 60.0;

/// File name of the fake slicer's executable.
pub fn executable_name() -> String {
    format!("hotend-fake-slicer{}", std::env::consts::EXE_SUFFIX)
}

/// Where the fake slicer's executable is when it was built with the running program: next
/// to the current executable, or one folder up when running from cargo's `deps` folder.
/// `None` when it is not there. To use it, set it as the explicit executable of a slicer
/// kind in [`crate::SlicerConfig::executables`].
pub fn locate() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let name = executable_name();
    let beside = dir.join(&name);
    if beside.is_file() {
        return Some(beside);
    }
    let above = dir
        .parent()
        .filter(|_| dir.file_name().is_some_and(|n| n == "deps"))?;
    Some(above.join(name)).filter(|path| path.is_file())
}

/// The numbers the fake slicer makes up for one job.
#[derive(Debug, Clone, PartialEq)]
pub struct Estimate {
    pub layers: u32,
    pub time_s: u64,
    /// `(grams, metres)` per filament, in slot order.
    pub filaments: Vec<(f64, f64)>,
}

/// One filament as the fake slicer understands it.
#[derive(Debug, Clone, PartialEq)]
pub struct FakeFilament {
    pub kind: String,
    /// 0 when the settings file states none, as in a profile that was not flattened.
    pub density_g_cm3: f64,
    pub diameter_mm: f64,
    /// The vendor's id of the filament; empty when the settings file has none.
    pub filament_id: String,
    /// Bed temperature for the plate the job is sliced for; 0 when the file states none.
    pub bed_c: f64,
    /// Nozzle temperature; 0 when the file states none.
    pub nozzle_c: f64,
}

/// Estimates a print: layers from the tallest mesh, plastic from the total volume shared
/// equally between the filaments.
pub fn estimate(
    meshes: &[MeshStats],
    layer_height: f64,
    filaments: &[FakeFilament],
) -> Result<Estimate, String> {
    let volume: f64 = meshes.iter().map(|mesh| mesh.volume_mm3).sum();
    let height = meshes.iter().map(MeshStats::height).fold(0.0, f64::max);
    if !(layer_height > 0.0 && layer_height.is_finite()) {
        return Err("the layer height must be a positive number".into());
    }
    if filaments.is_empty() {
        return Err("no filament settings were loaded".into());
    }
    if !(volume > 0.0 && height > 0.0) {
        return Err("the model has no volume".into());
    }
    let layers = (height / layer_height - 1e-9).ceil().max(1.0);
    if layers > 1_000_000.0 {
        return Err("the model would have more than a million layers".into());
    }
    let extruded = volume * FILL_FRACTION;
    let share = extruded / filaments.len() as f64;
    let filaments = filaments
        .iter()
        .map(|filament| {
            let area = std::f64::consts::PI * (filament.diameter_mm / 2.0).powi(2);
            let grams = share / 1000.0 * filament.density_g_cm3;
            let metres = share / area / 1000.0;
            // Two decimals, as the real slicers print them.
            (
                (grams * 100.0).round() / 100.0,
                (metres * 100.0).round() / 100.0,
            )
        })
        .collect();
    let time = extruded / FLOW_MM3_S + layers * SECONDS_PER_LAYER + SECONDS_PER_JOB;
    Ok(Estimate {
        layers: layers as u32,
        time_s: time.round() as u64,
        filaments,
    })
}

/// Runs the fake slicer with `args` (without the program name) and returns its exit code:
/// 0 success, 2 a command line it does not accept, 3 unusable settings, 4 unusable model,
/// 5 the output could not be written. In the Bambu Studio style nothing is written to `out`
/// or `err` except for `--help`; a failure's code and message are in `result.json`.
pub fn run(args: &[OsString], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let args: Vec<String> = match args.iter().map(|arg| arg.clone().into_string()).collect() {
        Ok(args) => args,
        Err(_) => {
            let _ = writeln!(err, "hotend-fake-slicer: arguments must be UTF-8");
            return 2;
        }
    };
    if args.is_empty() || args.iter().any(|arg| arg == "--help" || arg == "-h") {
        let _ = writeln!(
            out,
            "{FAKE_BANNER}{FAKE_VERSION} (not a real slicer: writes placeholder files for Hotend's tests and demo)\n\
             Usage, Bambu Studio style:  hotend-fake-slicer --load-settings \"machine.json;process.json\" \
             --load-filaments \"f1.json;f2.json\" [--filament-map-mode=Manual --filament-map=1,2] \
             [--curr-bed-type=NAME] [--arrange N] [--orient] [--debug N] [--info] --slice N [--outputdir DIR] \
             --export-3mf NAME file.3mf|file.stl ...\n\
             Usage, PrusaSlicer style:   hotend-fake-slicer --export-gcode [--datadir DIR] [--loglevel N] \
             --load FILE.ini ... [--dont-arrange] --output FILE.gcode file.stl|file.3mf ..."
        );
        return 0;
    }
    if args
        .iter()
        .any(|arg| arg == "--export-gcode" || arg == "-g")
    {
        return match prusa_style(&args, out) {
            Ok(()) => 0,
            Err((code, message)) => {
                let _ = writeln!(err, "hotend-fake-slicer: {message}");
                code
            }
        };
    }
    // Bambu Studio style: silent. The outcome goes into result.json and the exit code.
    let result_dir = option_value(&args, "--outputdir").map_or_else(PathBuf::new, PathBuf::from);
    match bambu_style(&args, &result_dir) {
        Ok(()) => 0,
        Err((code, message)) => {
            let report = serde_json::json!({
                "error_string": message,
                "return_code": code,
                "plate_index": 0,
            });
            let _ = write_output(&result_dir.join(RESULT_NAME), report.to_string().as_bytes());
            code
        }
    }
}

/// The value that follows the option `name`, if both are there.
fn option_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let index = args.iter().position(|arg| arg == name)?;
    args.get(index + 1).map(String::as_str)
}

type Failure = (i32, String);

fn usage(message: impl Into<String>) -> Failure {
    (2, message.into())
}

fn settings(message: impl Into<String>) -> Failure {
    (3, message.into())
}

/// Takes the value of an option that needs one.
fn value<'a>(args: &'a [String], index: &mut usize, name: &str) -> Result<&'a str, Failure> {
    *index += 1;
    args.get(*index)
        .map(String::as_str)
        .ok_or_else(|| usage(format!("{name} needs a value")))
}

fn read_meshes(inputs: &[String]) -> Result<Vec<MeshStats>, Failure> {
    if inputs.is_empty() {
        return Err(usage("no model file was given"));
    }
    inputs
        .iter()
        .map(|input| mesh::read(Path::new(input)).map_err(|message| (4, message)))
        .collect()
}

fn write_output(path: &Path, bytes: &[u8]) -> Result<(), Failure> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|e| (5, format!("cannot create {}: {e}", parent.display())))?;
    }
    std::fs::write(path, bytes).map_err(|e| (5, format!("cannot write {}: {e}", path.display())))
}

/// A setting from a Bambu-style JSON profile: a string, a number or the first item of a
/// list of those.
fn json_setting(profile: &serde_json::Value, key: &str) -> Option<String> {
    let value = profile.get(key)?;
    let value = value
        .as_array()
        .and_then(|list| list.first())
        .unwrap_or(value);
    match value {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn load_json(path: &str) -> Result<serde_json::Value, Failure> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| settings(format!("cannot read the settings file {path}: {e}")))?;
    match serde_json::from_str(&text) {
        Ok(value @ serde_json::Value::Object(_)) => Ok(value),
        _ => Err(settings(format!("{path} is not a JSON object"))),
    }
}

fn positive(text: Option<String>, default: f64, what: &str) -> Result<f64, Failure> {
    match text {
        None => Ok(default),
        Some(text) => text
            .trim()
            .trim_end_matches("mm")
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite() && *value > 0.0)
            .ok_or_else(|| settings(format!("the setting {what} is not a positive number"))),
    }
}

/// A number from a settings file; `default` when the file does not state it.
fn amount(text: Option<String>, default: f64, what: &str) -> Result<f64, Failure> {
    match text {
        None => Ok(default),
        Some(text) => text
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite() && *value >= 0.0)
            .ok_or_else(|| settings(format!("the setting {what} is not a number"))),
    }
}

/// The bed size of a machine: the largest X and Y of its `printable_area` corners
/// (`"0x0", "256x0", ...`), or 256 x 256 mm.
fn bed_size(machine: &serde_json::Value) -> [f64; 2] {
    let mut size = [0.0f64; 2];
    for corner in machine
        .get("printable_area")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|corner| corner.as_str())
    {
        if let Some((x, y)) = corner.split_once('x')
            && let (Ok(x), Ok(y)) = (x.trim().parse::<f64>(), y.trim().parse::<f64>())
        {
            size = [size[0].max(x), size[1].max(y)];
        }
    }
    if size[0] > 0.0 && size[1] > 0.0 {
        size
    } else {
        [256.0, 256.0]
    }
}

/// Everything the fake slicer writes for one plate.
struct FakePlate {
    plate: u32,
    estimate: Estimate,
    filaments: Vec<FakeFilament>,
    /// Slicer extruder per filament.
    map: Vec<u32>,
    layer_height: f64,
    /// The slicer's name of the plate type, and the filament setting with its temperature.
    bed_type: String,
    bed_key: String,
    /// `[min x, min y, max x, max y]` of the parts where they are printed.
    outline: [f64; 4],
    height: f64,
    /// Whether the parts were moved; then `result.json` lists them with their boxes.
    arranged: bool,
    triangles: u64,
    /// `printer_model` of the machine settings, when they state one.
    printer_model: Option<String>,
    /// `nozzle_diameter` of the machine settings, one entry per nozzle, as written there.
    nozzle_diameters: Vec<String>,
}

fn bambu_style(args: &[String], result_dir: &Path) -> Result<(), Failure> {
    let mut load_settings = None;
    let mut load_filaments = None;
    let mut export = None;
    let mut slice = None;
    let mut arrange = None;
    let mut info = false;
    let mut map_mode = None;
    let mut map = None;
    let mut bed_type = None;
    let mut inputs = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        match arg {
            "--debug" => {
                value(args, &mut index, arg)?
                    .parse::<i64>()
                    .map_err(|_| usage(format!("{arg} needs a number")))?;
            }
            "--arrange" => {
                arrange = Some(
                    value(args, &mut index, arg)?
                        .parse::<i64>()
                        .map_err(|_| usage(format!("{arg} needs a number")))?,
                );
            }
            "--orient" => {}
            "--info" => info = true,
            "--load-settings" => load_settings = Some(value(args, &mut index, arg)?),
            "--load-filaments" => load_filaments = Some(value(args, &mut index, arg)?),
            "--outputdir" => {
                value(args, &mut index, arg)?;
            }
            "--export-3mf" => export = Some(value(args, &mut index, arg)?),
            "--slice" => {
                slice = Some(
                    value(args, &mut index, arg)?
                        .parse::<u32>()
                        .map_err(|_| usage("--slice needs a plate number"))?,
                );
            }
            _ if arg.starts_with("--filament-map-mode=") => {
                map_mode = arg.split_once('=').map(|(_, value)| value);
            }
            _ if arg.starts_with("--filament-map=") => {
                map = arg.split_once('=').map(|(_, value)| value);
            }
            _ if arg.starts_with("--curr-bed-type=") => {
                bed_type = arg.split_once('=').map(|(_, value)| value.to_owned());
            }
            _ if arg.starts_with('-') => {
                return Err(usage(format!("unknown option {arg}")));
            }
            _ => inputs.push(arg.to_owned()),
        }
        index += 1;
    }
    if info && slice.is_none() {
        // Like the real program: nothing is sliced, and the report says so little.
        let report = serde_json::json!({
            "error_string": "Success.",
            "layer_height": 0.0,
            "plate_index": 0,
            "return_code": 0,
            "sparse_infill_density": 0.0,
            "wall_loops": 0,
        });
        return write_output(&result_dir.join(RESULT_NAME), report.to_string().as_bytes());
    }
    let plate = match slice {
        // Like the real program, 0 means every plate; the fake has exactly one.
        Some(0 | 1) => 1,
        Some(other) => {
            return Err(usage(format!(
                "there is no plate {other}; the fake slicer has one plate"
            )));
        }
        None => return Err(usage("nothing to do: --slice is missing")),
    };
    let export = export.ok_or_else(|| usage("--export-3mf is missing"))?;
    let files: Vec<&str> = load_settings
        .ok_or_else(|| usage("--load-settings is missing"))?
        .split(';')
        .collect();
    let [machine, process] = files.as_slice() else {
        return Err(usage("--load-settings needs \"machine.json;process.json\""));
    };
    let machine = load_json(machine)?;
    let process = load_json(process)?;
    let layer_height = positive(json_setting(&process, "layer_height"), 0.2, "layer_height")?;
    let nozzles = machine
        .get("nozzle_diameter")
        .and_then(|value| value.as_array())
        .map_or(1, |list| list.len().max(1));
    // What the machine settings say the file is sliced for. Only plain values are
    // repeated in the G-code: a name is letters, digits, spaces and a few signs, a size
    // is a number.
    let printer_model = json_setting(&machine, "printer_model").filter(|name| {
        !name.is_empty()
            && name.len() <= 60
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
    });
    let nozzle_diameters: Vec<String> = machine
        .get("nozzle_diameter")
        .and_then(|value| value.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|size| size.as_str())
                .map(str::trim)
                .filter(|size| size.parse::<f64>().is_ok_and(|mm| mm > 0.0 && mm <= 2.0))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();

    // The plate type: the command line wins over the process settings, and without
    // either it is the Cool Plate, as in the real program.
    let bed_type = bed_type
        .or_else(|| json_setting(&process, "curr_bed_type"))
        .unwrap_or_else(|| "Cool Plate".to_owned());
    let bed_key = crate::system::SystemProfiles::builtin()
        .ok()
        .and_then(|table| {
            table
                .bed_type_by_value(&bed_type)
                .map(|bed| bed.temperature_key.clone())
        })
        .ok_or_else(|| settings(format!("`{bed_type}` is not a plate type")))?;

    let mut filaments = Vec::new();
    for file in load_filaments
        .ok_or_else(|| usage("--load-filaments is missing"))?
        .split(';')
    {
        if file.is_empty() {
            return Err(usage(
                "the fake slicer has no project filaments to keep; name every filament file",
            ));
        }
        // `inherits` and `include` are not followed: what the file does not state itself
        // is missing, and a missing density is 0.
        let profile = load_json(file)?;
        filaments.push(FakeFilament {
            kind: json_setting(&profile, "filament_type").unwrap_or_else(|| "PLA".into()),
            density_g_cm3: amount(
                json_setting(&profile, "filament_density"),
                0.0,
                "filament_density",
            )?,
            diameter_mm: positive(
                json_setting(&profile, "filament_diameter"),
                1.75,
                "filament_diameter",
            )?,
            filament_id: json_setting(&profile, "filament_id").unwrap_or_default(),
            bed_c: amount(json_setting(&profile, &bed_key), 0.0, &bed_key)?,
            nozzle_c: amount(
                json_setting(&profile, "nozzle_temperature"),
                0.0,
                "nozzle_temperature",
            )?,
        });
    }

    // The filament map. With more than one nozzle the fake slicer insists on an explicit
    // manual map: it has no automatic grouping, and Hotend must never rely on one.
    let map: Vec<u32> = match (map_mode, map) {
        (Some(mode), Some(map)) if mode.eq_ignore_ascii_case("manual") => {
            let numbers: Option<Vec<u32>> = map.split(',').map(|n| n.trim().parse().ok()).collect();
            let numbers = numbers
                .ok_or_else(|| settings("--filament-map must be a list of extruder numbers"))?;
            if numbers.len() != filaments.len() {
                return Err(settings(format!(
                    "--filament-map has {} entries for {} filaments",
                    numbers.len(),
                    filaments.len()
                )));
            }
            if numbers.iter().any(|n| *n == 0 || *n as usize > nozzles) {
                return Err(settings(format!(
                    "--filament-map names an extruder this machine does not have (it has {nozzles})"
                )));
            }
            numbers
        }
        (Some(mode), _) if !mode.eq_ignore_ascii_case("manual") => {
            return Err(settings(format!(
                "the fake slicer only knows the Manual filament map mode, not `{mode}`"
            )));
        }
        (None, None) if nozzles == 1 => vec![1; filaments.len()],
        _ => {
            return Err(settings(
                "this machine has several nozzles: --filament-map-mode=Manual and --filament-map are both required",
            ));
        }
    };

    let meshes = read_meshes(&inputs)?;
    let estimate = estimate(&meshes, layer_height, &filaments).map_err(|message| (4, message))?;

    // Where the parts are printed: where the file has them with `--arrange 0`, otherwise
    // moved so that their common box sits in the middle of the bed.
    let mut outline = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for mesh in &meshes {
        outline = [
            outline[0].min(mesh.min[0]),
            outline[1].min(mesh.min[1]),
            outline[2].max(mesh.max[0]),
            outline[3].max(mesh.max[1]),
        ];
    }
    let arranged = arrange != Some(0);
    if arranged {
        let bed = bed_size(&machine);
        let shift = [
            bed[0] / 2.0 - (outline[0] + outline[2]) / 2.0,
            bed[1] / 2.0 - (outline[1] + outline[3]) / 2.0,
        ];
        outline = [
            outline[0] + shift[0],
            outline[1] + shift[1],
            outline[2] + shift[0],
            outline[3] + shift[1],
        ];
    }
    let fake = FakePlate {
        plate,
        estimate,
        filaments,
        map,
        layer_height,
        bed_type,
        bed_key,
        outline,
        height: meshes.iter().map(MeshStats::height).fold(0.0, f64::max),
        arranged,
        triangles: meshes.iter().map(|mesh| mesh.triangles).sum(),
        printer_model,
        nozzle_diameters,
    };

    let gcode = bambu_gcode(&fake);
    let archive = bambu_archive(&fake, &gcode)
        .map_err(|message| (5, format!("cannot build the archive: {message}")))?;
    write_output(&result_dir.join(export), &archive)?;
    write_output(
        &result_dir.join(format!("plate_{plate}.gcode")),
        gcode.as_bytes(),
    )?;
    write_output(
        &result_dir.join(RESULT_NAME),
        bambu_result(&fake).to_string().as_bytes(),
    )
}

/// `result.json` of a successful slice, in the shape the real program writes.
fn bambu_result(fake: &FakePlate) -> serde_json::Value {
    let filaments: Vec<serde_json::Value> = fake
        .filaments
        .iter()
        .zip(&fake.estimate.filaments)
        .enumerate()
        .map(|(index, (filament, (grams, _)))| {
            serde_json::json!({
                "filament_id": filament.filament_id,
                "id": index + 1,
                "main_used_g": grams,
                "total_used_g": grams,
            })
        })
        .collect();
    let mut plate = serde_json::json!({
        "filament_change_times": 0,
        "filaments": filaments,
        "id": fake.plate,
        "layer_filament_change": 0,
        "main_predication": fake.estimate.time_s,
        "total_predication": fake.estimate.time_s,
        "triangle_count": fake.triangles,
        "warning_message": "",
    });
    if fake.arranged {
        // The real program lists the objects only when it placed them itself.
        plate["objects"] = serde_json::json!([{
            "bbox": {
                "x": fake.outline[0],
                "y": fake.outline[1],
                "z": 0.0,
                "width": fake.outline[2] - fake.outline[0],
                "depth": fake.outline[3] - fake.outline[1],
                "height": fake.height,
            },
            "id": 1,
            "name": "model",
            "triangle_count": fake.triangles,
        }]);
    }
    serde_json::json!({
        "error_string": "Success.",
        "layer_height": fake.layer_height,
        "plate_index": 0,
        "return_code": 0,
        "sliced_plates": [plate],
        "sparse_infill_density": 0.0,
        "wall_loops": 0,
    })
}

fn duration_text(seconds: u64) -> String {
    let (d, h, m, s) = (
        seconds / 86_400,
        seconds / 3_600 % 24,
        seconds / 60 % 60,
        seconds % 60,
    );
    let mut parts = Vec::new();
    if d > 0 {
        parts.push(format!("{d}d"));
    }
    if d > 0 || h > 0 {
        parts.push(format!("{h}h"));
    }
    if d > 0 || h > 0 || m > 0 {
        parts.push(format!("{m}m"));
    }
    parts.push(format!("{s}s"));
    parts.join(" ")
}

const NOT_PRINTABLE: &str =
    "NOT PRINTABLE: placeholder written by Hotend's fake slicer; it contains no machine commands";

/// The markers around the stand-in's estimates in its G-code, and the keys inside.
pub const ESTIMATE_BLOCK_START: &str = "HOTEND_ESTIMATE_BLOCK_START";
pub const ESTIMATE_BLOCK_END: &str = "HOTEND_ESTIMATE_BLOCK_END";
pub const ESTIMATE_TIME: &str = "estimated time [s]";
pub const ESTIMATE_LAYERS: &str = "estimated layer number";
pub const ESTIMATE_LENGTH: &str = "estimated filament length [mm]";
pub const ESTIMATE_WEIGHT: &str = "estimated filament weight [g]";
pub const ESTIMATE_HEIGHT: &str = "estimated height [mm]";

/// Joins numbers the way the real header and settings block list them.
fn list(values: impl Iterator<Item = f64>, decimals: usize) -> String {
    values
        .map(|value| format!("{value:.decimals$}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// The placeholder G-code: the header and the settings block of the real form, as comments,
/// and no command at all.
///
/// The header's time, length and weight are those of this file, which prints nothing: 0.
/// No layer count is stated, for the same reason. The stand-in's estimates for the model
/// follow in their own block.
fn bambu_gcode(fake: &FakePlate) -> String {
    let estimate = &fake.estimate;
    let numbers: Vec<String> = (1..=fake.filaments.len()).map(|n| n.to_string()).collect();
    let map: Vec<String> = fake.map.iter().map(u32::to_string).collect();
    let nothing = || list(fake.filaments.iter().map(|_| 0.0), 2);
    let mut gcode = format!(
        "; HEADER_BLOCK_START\n; HotendFakeSlicer {FAKE_VERSION}\n; {NOT_PRINTABLE}\n\
         ; model printing time: 0s; total estimated time: 0s\n\
         ; total filament length [mm] : {}\n\
         ; total filament weight [g] : {}\n\
         ; filament_density: {}\n\
         ; filament_diameter: {}\n\
         ; max_z_height: 0.00\n\
         ; filament: {}\n\
         ; HEADER_BLOCK_END\n\n\
         ; {ESTIMATE_BLOCK_START}\n\
         ; What a slice of this model is estimated to take. Not a statement about this file:\n\
         ; this file prints nothing.\n\
         ; {ESTIMATE_TIME}: {}\n\
         ; {ESTIMATE_LAYERS}: {}\n\
         ; {ESTIMATE_LENGTH} : {}\n\
         ; {ESTIMATE_WEIGHT} : {}\n\
         ; {ESTIMATE_HEIGHT}: {:.2}\n\
         ; {ESTIMATE_BLOCK_END}\n\n\
         ; CONFIG_BLOCK_START\n\
         ; curr_bed_type = {}\n\
         ; {} = {}\n\
         ; filament_map = {}\n\
         ; layer_height = {}\n\
         ; nozzle_temperature = {}\n",
        nothing(),
        nothing(),
        list(fake.filaments.iter().map(|f| f.density_g_cm3), 2),
        list(fake.filaments.iter().map(|f| f.diameter_mm), 2),
        numbers.join(","),
        estimate.time_s,
        estimate.layers,
        list(
            estimate.filaments.iter().map(|(_, metres)| metres * 1000.0),
            2
        ),
        list(estimate.filaments.iter().map(|(grams, _)| *grams), 2),
        fake.height,
        fake.bed_type,
        fake.bed_key,
        list(fake.filaments.iter().map(|f| f.bed_c), 0),
        map.join(","),
        fake.layer_height,
        list(fake.filaments.iter().map(|f| f.nozzle_c), 0),
    );
    // What the machine settings say the file is for, when they say it.
    if !fake.nozzle_diameters.is_empty() {
        gcode.push_str(&format!(
            "; nozzle_diameter = {}\n",
            fake.nozzle_diameters.join(",")
        ));
    }
    if let Some(model) = &fake.printer_model {
        gcode.push_str(&format!("; printer_model = {model}\n"));
    }
    gcode.push_str("; CONFIG_BLOCK_END\n\n");
    gcode
}

fn bambu_archive(fake: &FakePlate, gcode: &str) -> Result<Vec<u8>, String> {
    let (plate, filaments) = (fake.plate, &fake.filaments);
    let map_text: Vec<String> = fake.map.iter().map(u32::to_string).collect();
    let gcode_md5 = md5::hex_upper(gcode.as_bytes());
    let mut info = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<config>\n  <header>\n    \
         <header_item key=\"X-BBL-Client-Type\" value=\"hotend-fake-slicer\"/>\n    \
         <header_item key=\"X-BBL-Client-Version\" value=\"{FAKE_VERSION}\"/>\n  </header>\n  <plate>\n    \
         <metadata key=\"index\" value=\"{plate}\"/>\n    \
         <metadata key=\"prediction\" value=\"0\"/>\n    \
         <metadata key=\"weight\" value=\"0.00\"/>\n    \
         <metadata key=\"filament_maps\" value=\"{}\"/>\n",
        map_text.join(" ")
    );
    // The file's own account of the plate: it prints nothing and uses no filament.
    for (index, filament) in filaments.iter().enumerate() {
        let kind: String = filament
            .kind
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_'))
            .take(32)
            .collect();
        info.push_str(&format!(
            "    <filament id=\"{}\" type=\"{kind}\" color=\"#808080\" used_m=\"0.00\" used_g=\"0.00\"/>\n",
            index + 1
        ));
    }
    info.push_str("  </plate>\n</config>\n");
    // The outline of the parts on the plate, as the real program records it.
    let plate_json = serde_json::json!({
        "bbox_all": fake.outline,
        "is_seq_print": false,
        "version": 2,
    })
    .to_string();

    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    let entries: [(String, &str); 7] = [
        (
            "[Content_Types].xml".into(),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"model\" ContentType=\"application/vnd.ms-package.3dmanufacturing-3dmodel+xml\"/><Default Extension=\"gcode\" ContentType=\"text/x.gcode\"/></Types>\n",
        ),
        (
            "_rels/.rels".into(),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Target=\"/3D/3dmodel.model\" Id=\"rel-1\" Type=\"http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel\"/></Relationships>\n",
        ),
        (
            "3D/3dmodel.model".into(),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<model unit=\"millimeter\" xmlns=\"http://schemas.microsoft.com/3dmanufacturing/core/2015/02\"><resources/><build/></model>\n",
        ),
        (format!("Metadata/plate_{plate}.gcode"), gcode),
        (
            format!("Metadata/plate_{plate}.gcode.md5"),
            gcode_md5.as_str(),
        ),
        ("Metadata/slice_info.config".into(), info.as_str()),
        (format!("Metadata/plate_{plate}.json"), plate_json.as_str()),
    ];
    for (name, content) in entries {
        writer
            .start_file(name, options)
            .map_err(|e| e.to_string())?;
        writer
            .write_all(content.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    writer
        .finish()
        .map(std::io::Cursor::into_inner)
        .map_err(|e| e.to_string())
}

/// `key = value` lines of PrusaSlicer-style settings files; later files win.
fn load_ini(files: &[&str]) -> Result<BTreeMap<String, String>, Failure> {
    let mut values = BTreeMap::new();
    for file in files {
        let text = std::fs::read_to_string(file)
            .map_err(|e| settings(format!("cannot read the settings file {file}: {e}")))?;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with(['#', ';']) {
                continue;
            }
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| settings(format!("{file} has a line that is not `key = value`")))?;
            values.insert(key.trim().to_owned(), value.trim().to_owned());
        }
    }
    Ok(values)
}

fn prusa_style(args: &[String], out: &mut dyn Write) -> Result<(), Failure> {
    let mut loads = Vec::new();
    let mut output = None;
    let mut inputs = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        match arg {
            "--export-gcode" | "--gcode" | "-g" | "--dont-arrange" => {}
            "--datadir" => {
                value(args, &mut index, arg)?;
            }
            "--loglevel" => {
                value(args, &mut index, arg)?
                    .parse::<u8>()
                    .map_err(|_| usage("--loglevel needs a number"))?;
            }
            "--load" => loads.push(value(args, &mut index, arg)?),
            "--output" | "-o" => output = Some(value(args, &mut index, arg)?),
            _ if arg.starts_with('-') => return Err(usage(format!("unknown option {arg}"))),
            _ => inputs.push(arg.to_owned()),
        }
        index += 1;
    }
    let output = output.ok_or_else(|| usage("--output is missing"))?;
    let values = load_ini(&loads)?;
    let setting = |key: &str| {
        values
            .get(key)
            .map(|value| value.split(',').next().unwrap_or("").to_owned())
    };
    let layer_height = positive(setting("layer_height"), 0.2, "layer_height")?;
    let filament = FakeFilament {
        kind: setting("filament_type").unwrap_or_else(|| "PLA".into()),
        density_g_cm3: positive(setting("filament_density"), 1.24, "filament_density")?,
        diameter_mm: positive(setting("filament_diameter"), 1.75, "filament_diameter")?,
        filament_id: String::new(),
        bed_c: 0.0,
        nozzle_c: 0.0,
    };
    let meshes = read_meshes(&inputs)?;
    let estimate = estimate(&meshes, layer_height, std::slice::from_ref(&filament))
        .map_err(|message| (4, message))?;
    let (grams, metres) = estimate.filaments[0];
    let area = std::f64::consts::PI * (filament.diameter_mm / 2.0).powi(2);

    let mut gcode =
        format!("; generated by HotendFakeSlicer {FAKE_VERSION}\n; {NOT_PRINTABLE}\n\n");
    for layer in 1..=estimate.layers.min(100_000) {
        gcode.push_str(&format!(
            ";LAYER_CHANGE\n;Z:{:.3}\n",
            f64::from(layer) * layer_height
        ));
    }
    gcode.push_str(&format!(
        "; filament used [mm] = {:.2}\n; filament used [cm3] = {:.2}\n; filament used [g] = {grams:.2}\n\
         ; filament cost = 0.00\n; total filament used [g] = {grams:.2}\n; total filament cost = 0.00\n\
         ; estimated printing time (normal mode) = {}\n",
        metres * 1000.0,
        metres * 1000.0 * area / 1000.0,
        duration_text(estimate.time_s)
    ));
    write_output(Path::new(output), gcode.as_bytes())?;
    let _ = writeln!(out, "Slicing result exported to {output}");
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::mesh::tests::{ascii_stl, threemf_bytes};
    use super::*;
    use crate::output::{OutputLimits, bambu, prusa};

    fn pla() -> FakeFilament {
        FakeFilament {
            kind: "PLA".into(),
            density_g_cm3: 1.24,
            diameter_mm: 1.75,
            filament_id: String::new(),
            bed_c: 0.0,
            nozzle_c: 0.0,
        }
    }

    fn call(args: &[&str]) -> (i32, String, String) {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(&args, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[test]
    fn estimates_follow_volume_height_and_layer_height() {
        let cube = |size: [f64; 3]| MeshStats {
            volume_mm3: size[0] * size[1] * size[2],
            min: [0.0; 3],
            max: size,
            triangles: 12,
        };
        let small = estimate(&[cube([20.0, 30.0, 10.0])], 0.2, &[pla()]).unwrap();
        assert_eq!(small.layers, 50);
        // 6000 mm3 * 0.35 = 2100 mm3: 2.60 g of PLA, 0.87 m of 1.75 mm filament.
        assert_eq!(small.filaments, [(2.6, 0.87)]);
        assert_eq!(
            small.time_s,
            (2100.0 / 8.0 + 50.0 * 3.0 + 60.0f64).round() as u64
        );

        let big = estimate(&[cube([40.0, 30.0, 10.0])], 0.2, &[pla()]).unwrap();
        assert_eq!(big.layers, 50);
        assert!((big.filaments[0].0 - 2.0 * small.filaments[0].0).abs() < 0.011);
        assert!(big.time_s > small.time_s);

        let fine = estimate(&[cube([20.0, 30.0, 10.0])], 0.1, &[pla()]).unwrap();
        assert_eq!(fine.layers, 100);
        assert_eq!(fine.filaments, small.filaments);

        let two = estimate(&[cube([20.0, 30.0, 10.0])], 0.2, &[pla(), pla()]).unwrap();
        assert_eq!(two.filaments, [(1.3, 0.44), (1.3, 0.44)]);

        assert!(estimate(&[cube([20.0, 30.0, 10.0])], 0.0, &[pla()]).is_err());
        assert!(estimate(&[], 0.2, &[pla()]).is_err());
        assert!(estimate(&[cube([20.0, 30.0, 10.0])], 0.2, &[]).is_err());
    }

    #[test]
    fn durations_are_written_the_way_the_parser_reads_them() {
        assert_eq!(duration_text(45), "45s");
        assert_eq!(duration_text(964), "16m 4s");
        assert_eq!(duration_text(7201), "2h 0m 1s");
        assert_eq!(duration_text(93_784), "1d 2h 3m 4s");
    }

    #[test]
    fn help_prints_the_banner() {
        let (code, out, _) = call(&["--help"]);
        assert_eq!(code, 0);
        assert!(
            out.starts_with(&format!("{FAKE_BANNER}{FAKE_VERSION} ")),
            "{out}"
        );
    }

    struct Files {
        dir: tempfile::TempDir,
    }

    impl Files {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let files: [(&str, &str); 9] = [
                (
                    "machine.json",
                    r#"{"nozzle_diameter": ["0.4", "0.4"], "printable_area": ["0x0", "200x0", "200x180", "0x180"]}"#,
                ),
                ("single.json", r#"{"nozzle_diameter": ["0.4"]}"#),
                ("process.json", r#"{"layer_height": "0.2"}"#),
                (
                    "textured.json",
                    r#"{"layer_height": "0.2", "curr_bed_type": "Textured PEI Plate"}"#,
                ),
                (
                    "pla.json",
                    r#"{"filament_type": ["PLA"], "filament_density": ["1.24"], "filament_diameter": ["1.75"],
                        "filament_id": "TEST01", "nozzle_temperature": ["215"],
                        "cool_plate_temp": ["35"], "textured_plate_temp": ["57"]}"#,
                ),
                // A profile as an installation ships it: most of it is in its parent.
                (
                    "leaf.json",
                    r#"{"inherits": "parent", "filament_type": ["PLA"], "textured_plate_temp": ["57"]}"#,
                ),
                (
                    "parent.json",
                    r#"{"filament_density": ["1.24"], "filament_diameter": ["1.75"], "filament_id": "TEST01"}"#,
                ),
                ("machine.ini", "nozzle_diameter = 0.4\n"),
                (
                    "process.ini",
                    "# comment\nlayer_height = 0.2\nfilament_density = 1.24\n",
                ),
            ];
            for (name, content) in files {
                std::fs::write(dir.path().join(name), content).unwrap();
            }
            std::fs::write(dir.path().join("box.stl"), ascii_stl([20.0, 30.0, 10.0])).unwrap();
            std::fs::write(
                dir.path().join("box.3mf"),
                threemf_bytes([20.0, 30.0, 10.0]),
            )
            .unwrap();
            std::fs::write(dir.path().join("box.step"), b"ISO-10303-21;").unwrap();
            Self { dir }
        }

        fn path(&self, name: &str) -> String {
            self.dir.path().join(name).to_string_lossy().into_owned()
        }
    }

    /// Runs a Bambu-style slice of the box into `<dir>/out` and returns the exit code.
    /// Asserts that nothing was printed.
    fn slice_box(
        files: &Files,
        machine: &str,
        process: &str,
        filaments: &[&str],
        extra: &[&str],
    ) -> i32 {
        let settings = format!("{};{}", files.path(machine), files.path(process));
        let filaments: Vec<String> = filaments.iter().map(|name| files.path(name)).collect();
        let filaments = filaments.join(";");
        let (out_dir, model) = (files.path("out"), files.path("box.3mf"));
        let mut args = vec![
            "--debug",
            "2",
            "--slice",
            "0",
            "--load-settings",
            &settings,
            "--load-filaments",
            &filaments,
        ];
        args.extend(extra);
        args.extend([
            "--outputdir",
            &out_dir,
            "--export-3mf",
            "plate_1.gcode.3mf",
            &model,
        ]);
        let (code, out, err) = call(&args);
        assert_eq!(
            (out.as_str(), err.as_str()),
            ("", ""),
            "the fake printed something"
        );
        code
    }

    /// What the sliced file says about itself, read by the real parser.
    fn own_account(files: &Files) -> crate::output::Parsed {
        let file = std::fs::File::open(files.dir.path().join("out/plate_1.gcode.3mf")).unwrap();
        let beds = crate::system::SystemProfiles::builtin().unwrap().bed_types;
        bambu::parse(
            file,
            1,
            Some("filament_maps"),
            &beds,
            &OutputLimits::default(),
        )
        .unwrap()
    }

    /// The result of a stand-in run as the adapter reads it: the stand-in's estimates in
    /// place of the file's own numbers.
    fn sliced(files: &Files) -> crate::output::Parsed {
        let mut parsed = own_account(files);
        parsed.use_stand_in_estimate();
        parsed
    }

    /// The file prints nothing, and every place where a sliced file says what it prints
    /// says so. The estimates are apart from those places.
    #[test]
    fn the_file_claims_no_print_and_carries_the_estimates_apart() {
        let files = Files::new();
        let mut extra = MAP.to_vec();
        extra.extend(["--arrange", "0"]);
        assert_eq!(
            slice_box(
                &files,
                "machine.json",
                "process.json",
                &["pla.json", "pla.json"],
                &extra,
            ),
            0
        );
        let own = own_account(&files);
        assert_eq!((own.time_s, own.layers), (Some(0.0), None));
        assert_eq!(own.filaments[&1], (Some(0.0), Some(0.0)));
        assert_eq!(own.filaments[&2], (Some(0.0), Some(0.0)));
        assert_eq!(own.extent.as_ref().unwrap().z, Some([0.0, 0.0]));
        let codes: Vec<&str> = own.warnings.iter().map(|w| w.code.as_str()).collect();
        assert_eq!(codes, ["layers_unknown"]);
        let estimate = own.stand_in.clone().unwrap();
        assert_eq!((estimate.time_s, estimate.layers), (473.0, 50));
        assert_eq!(estimate.filaments[&1], (1.3, 0.44));
        assert_eq!(estimate.height_mm, Some(10.0));

        let bytes = std::fs::read(files.dir.path().join("out/plate_1.gcode.3mf")).unwrap();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut entry = |name: &str| {
            use std::io::Read;
            let mut text = String::new();
            archive
                .by_name(name)
                .unwrap()
                .read_to_string(&mut text)
                .unwrap();
            text
        };
        let gcode = entry("Metadata/plate_1.gcode");
        // No command: every line is a comment or empty.
        assert!(
            gcode
                .lines()
                .all(|line| line.is_empty() || line.starts_with(';')),
            "{gcode}"
        );
        assert!(gcode.contains("; model printing time: 0s; total estimated time: 0s\n"));
        assert!(gcode.contains("; total filament length [mm] : 0.00,0.00\n"));
        assert!(gcode.contains("; total filament weight [g] : 0.00,0.00\n"));
        assert!(!gcode.contains("total layer number"), "{gcode}");
        assert!(gcode.contains("; nozzle_diameter = 0.4,0.4\n"), "{gcode}");
        // These machine settings name no printer model, so the file names none.
        assert!(!gcode.contains("printer_model"), "{gcode}");
        // The archive's checksum is the G-code's.
        assert_eq!(
            entry("Metadata/plate_1.gcode.md5"),
            md5::hex_upper(gcode.as_bytes())
        );
        let info = entry("Metadata/slice_info.config");
        assert!(info.contains("key=\"prediction\" value=\"0\""), "{info}");
        assert_eq!(
            info.matches("used_m=\"0.00\" used_g=\"0.00\"").count(),
            2,
            "{info}"
        );
    }

    fn result_json(files: &Files) -> crate::output::result_json::SlicerReport {
        let bytes = std::fs::read(files.dir.path().join("out/result.json")).unwrap();
        crate::output::result_json::parse(&bytes).unwrap()
    }

    const MAP: [&str; 2] = ["--filament-map-mode=Manual", "--filament-map=2,1"];

    #[test]
    fn bambu_style_output_is_read_back_by_the_real_parser() {
        let files = Files::new();
        let mut extra = MAP.to_vec();
        extra.extend(["--arrange", "0"]);
        let code = slice_box(
            &files,
            "machine.json",
            "process.json",
            &["pla.json", "pla.json"],
            &extra,
        );
        assert_eq!(code, 0);
        let parsed = sliced(&files);
        assert_eq!(parsed.layers, Some(50));
        assert_eq!(parsed.time_s, Some(473.0));
        assert_eq!(parsed.filaments[&1], (Some(1.3), Some(0.44)));
        assert_eq!(parsed.filaments[&2], (Some(1.3), Some(0.44)));
        assert_eq!(parsed.reported_map, Some(vec![2, 1]));
        assert!(!parsed.density_missing);
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
        // The fake is not Bambu Studio and does not claim a Bambu Studio version.
        assert_eq!(parsed.slicer_version, None);

        // The output folder holds what the real program leaves there.
        let mut names: Vec<String> = std::fs::read_dir(files.dir.path().join("out"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["plate_1.gcode", "plate_1.gcode.3mf", "result.json"]);
        // result.json has the real shape and agrees with the sliced file.
        let report = result_json(&files);
        assert!(report.succeeded());
        assert_eq!(report.error, "Success.");
        let plate = report.plate(1).unwrap();
        assert_eq!(plate.total_s, Some(473.0));
        assert_eq!(plate.filaments.len(), 2);
        assert_eq!(plate.filaments[0].total_g, Some(1.3));
        assert_eq!(plate.filaments[0].filament_id, "TEST01");
        let raw: serde_json::Value = serde_json::from_slice(
            &std::fs::read(files.dir.path().join("out/result.json")).unwrap(),
        )
        .unwrap();
        let keys: Vec<&str> = raw["sliced_plates"][0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "filament_change_times",
                "filaments",
                "id",
                "layer_filament_change",
                "main_predication",
                "total_predication",
                "triangle_count",
                "warning_message"
            ]
        );
    }

    #[test]
    fn a_failure_is_silent_and_is_in_result_json() {
        let files = Files::new();
        // Two nozzles and no filament map.
        let code = slice_box(&files, "machine.json", "process.json", &["pla.json"], &[]);
        assert_eq!(code, 3);
        let report = result_json(&files);
        assert_eq!(report.return_code, 3);
        assert!(report.error.contains("several nozzles"), "{}", report.error);
        assert!(report.plates.is_empty());
        assert!(!files.dir.path().join("out/plate_1.gcode.3mf").exists());

        // `--info` alone slices nothing and still leaves a report.
        let out_dir = files.path("info");
        let (code, out, err) = call(&["--info", "--outputdir", &out_dir, &files.path("box.3mf")]);
        assert_eq!((code, out.as_str(), err.as_str()), (0, "", ""));
        let bytes = std::fs::read(files.dir.path().join("info/result.json")).unwrap();
        let report = crate::output::result_json::parse(&bytes).unwrap();
        assert!(report.succeeded() && report.plates.is_empty());
    }

    #[test]
    fn the_parts_are_moved_unless_arrange_is_0() {
        let files = Files::new();
        let outline = |files: &Files| {
            let extent = sliced(files).extent.unwrap();
            assert_eq!(extent.source, "plate_metadata");
            assert_eq!(extent.z, Some([0.0, 10.0]));
            (extent.x, extent.y)
        };
        let objects = |files: &Files| {
            let raw: serde_json::Value = serde_json::from_slice(
                &std::fs::read(files.dir.path().join("out/result.json")).unwrap(),
            )
            .unwrap();
            raw["sliced_plates"][0].get("objects").cloned()
        };
        // `--arrange 0`: the box stays where the file has it, and result.json lists no
        // objects, as measured.
        let mut keep = MAP.to_vec();
        keep.extend(["--arrange", "0"]);
        assert_eq!(
            slice_box(
                &files,
                "machine.json",
                "process.json",
                &["pla.json", "pla.json"],
                &keep
            ),
            0
        );
        assert_eq!(outline(&files), ([0.0, 20.0], [0.0, 30.0]));
        assert_eq!(objects(&files), None);
        // Without the option, and with `--arrange 1`, the box goes to the middle of the
        // 200 x 180 mm bed and result.json lists it.
        for extra in [MAP.to_vec(), [MAP.as_slice(), &["--arrange", "1"]].concat()] {
            assert_eq!(
                slice_box(
                    &files,
                    "machine.json",
                    "process.json",
                    &["pla.json", "pla.json"],
                    &extra
                ),
                0
            );
            assert_eq!(outline(&files), ([90.0, 110.0], [75.0, 105.0]));
            let objects = objects(&files).unwrap();
            assert_eq!(objects[0]["bbox"]["x"], 90.0);
            assert_eq!(objects[0]["bbox"]["width"], 20.0);
        }
    }

    #[test]
    fn the_plate_type_is_honoured_and_defaults_to_the_cool_plate() {
        let files = Files::new();
        let bed = |files: &Files| {
            let parsed = sliced(files);
            (
                parsed.bed_type.unwrap(),
                parsed.bed_temperature_c,
                parsed.nozzle_temperature_c,
            )
        };
        let one = ["--filament-map-mode=Manual", "--filament-map=1"];
        // Nothing said: the Cool Plate, with the filament's temperature for that plate.
        assert_eq!(
            slice_box(&files, "machine.json", "process.json", &["pla.json"], &one),
            0
        );
        assert_eq!(
            bed(&files),
            ("Cool Plate".to_owned(), Some(35.0), Some(215.0))
        );
        // The process settings name a plate.
        assert_eq!(
            slice_box(&files, "machine.json", "textured.json", &["pla.json"], &one),
            0
        );
        assert_eq!(
            bed(&files),
            ("Textured PEI Plate".to_owned(), Some(57.0), Some(215.0))
        );
        // The command line wins over the settings.
        let cool = [one.as_slice(), &["--curr-bed-type=Cool Plate"]].concat();
        assert_eq!(
            slice_box(
                &files,
                "machine.json",
                "textured.json",
                &["pla.json"],
                &cool
            ),
            0
        );
        assert_eq!(bed(&files).0, "Cool Plate");
        // A plate this filament file has no temperature for: 0, as from an incomplete
        // profile. A plate type that does not exist is an error.
        let hot = [one.as_slice(), &["--curr-bed-type=High Temp Plate"]].concat();
        assert_eq!(
            slice_box(&files, "machine.json", "process.json", &["pla.json"], &hot),
            0
        );
        assert_eq!(
            bed(&files),
            ("High Temp Plate".to_owned(), Some(0.0), Some(215.0))
        );
        let glass = [one.as_slice(), &["--curr-bed-type=Glass"]].concat();
        assert_eq!(
            slice_box(
                &files,
                "machine.json",
                "process.json",
                &["pla.json"],
                &glass
            ),
            3
        );
        assert!(result_json(&files).error.contains("not a plate type"));
    }

    #[test]
    fn settings_that_are_not_flattened_give_no_weight() {
        let files = Files::new();
        let one = ["--filament-map-mode=Manual", "--filament-map=1"];
        // The leaf of an inherits chain, as an installation ships it: the slice "works",
        // but the density, the weight and the filament id are missing, as measured.
        assert_eq!(
            slice_box(&files, "machine.json", "process.json", &["leaf.json"], &one),
            0
        );
        let parsed = sliced(&files);
        assert!(parsed.density_missing);
        assert_eq!(parsed.filaments[&1], (Some(0.0), Some(0.87)));
        let report = result_json(&files);
        assert!(report.succeeded());
        let filament = &report.plate(1).unwrap().filaments[0];
        assert_eq!(
            (filament.total_g, filament.filament_id.as_str()),
            (Some(0.0), "")
        );
        // The same settings in one file give the weight.
        assert_eq!(
            slice_box(&files, "machine.json", "process.json", &["pla.json"], &one),
            0
        );
        let parsed = sliced(&files);
        assert!(!parsed.density_missing);
        assert_eq!(parsed.filaments[&1], (Some(2.6), Some(0.87)));
    }

    #[test]
    fn bambu_style_rejects_what_it_does_not_understand() {
        let files = Files::new();
        let settings = format!(
            "{};{}",
            files.path("machine.json"),
            files.path("process.json")
        );
        let single = format!(
            "{};{}",
            files.path("single.json"),
            files.path("process.json")
        );
        let pla = files.path("pla.json");
        let stl = files.path("box.stl");
        let out_dir = files.path("out");
        let base = |settings: &str| -> Vec<String> {
            [
                "--load-settings",
                settings,
                "--load-filaments",
                &pla,
                "--slice",
                "1",
                "--outputdir",
                &out_dir,
                "--export-3mf",
                "p.gcode.3mf",
            ]
            .map(String::from)
            .to_vec()
        };
        let run_with = |settings: &str, extra: &[&str]| {
            let mut args = base(settings);
            args.extend(extra.iter().map(|s| s.to_string()));
            let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
            call(&borrowed)
        };
        // One nozzle: no map needed.
        assert_eq!(run_with(&single, &[&stl]).0, 0);
        // Two nozzles: the manual map is mandatory, complete and in range.
        assert_eq!(run_with(&settings, &[&stl]).0, 3);
        assert_eq!(run_with(&settings, &["--filament-map=1", &stl]).0, 3);
        assert_eq!(
            run_with(
                &settings,
                &[
                    "--filament-map-mode=Auto For Flush",
                    "--filament-map=1",
                    &stl
                ]
            )
            .0,
            3
        );
        assert_eq!(
            run_with(
                &settings,
                &["--filament-map-mode=Manual", "--filament-map=1,2", &stl]
            )
            .0,
            3
        );
        assert_eq!(
            run_with(
                &settings,
                &["--filament-map-mode=Manual", "--filament-map=3", &stl]
            )
            .0,
            3
        );
        assert_eq!(
            run_with(
                &settings,
                &["--filament-map-mode=Manual", "--filament-map=1", &stl]
            )
            .0,
            0
        );
        // Unknown options, missing inputs, unreadable models.
        assert_eq!(run_with(&single, &["--start-gcode=G28", &stl]).0, 2);
        assert_eq!(run_with(&single, &[]).0, 2);
        assert_eq!(run_with(&single, &[&files.path("box.step")]).0, 4);
        assert_eq!(run_with(&single, &[&files.path("missing.stl")]).0, 4);
        assert_eq!(run_with(&files.path("machine.json"), &[&stl]).0, 2);
        let missing = format!("{};{}", files.path("nope.json"), files.path("process.json"));
        assert_eq!(run_with(&missing, &[&stl]).0, 3);
        assert_eq!(
            call(&[
                "--slice",
                "2",
                "--outputdir",
                &out_dir,
                "--export-3mf",
                "x",
                &stl
            ])
            .0,
            2
        );
    }

    #[test]
    fn prusa_style_output_is_read_back_by_the_real_parser() {
        let files = Files::new();
        let output = files.path("out/plate_1.gcode");
        let (code, out, err) = call(&[
            "--export-gcode",
            "--datadir",
            &files.path("cfg"),
            "--loglevel",
            "2",
            "--load",
            &files.path("machine.ini"),
            "--load",
            &files.path("process.ini"),
            "--output",
            &output,
            &files.path("box.stl"),
        ]);
        assert_eq!(code, 0, "{out}{err}");
        let parsed = prusa::parse(
            std::fs::File::open(&output).unwrap(),
            &OutputLimits::default(),
        )
        .unwrap();
        assert_eq!(parsed.layers, Some(50));
        assert_eq!(parsed.time_s, Some(473.0));
        assert_eq!(parsed.filaments[&1], (Some(2.6), Some(0.87)));
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);

        assert_eq!(
            call(&[
                "--export-gcode",
                "--output",
                &output,
                &files.path("box.step")
            ])
            .0,
            4
        );
        assert_eq!(
            call(&[
                "--export-gcode",
                "--output",
                &output,
                "--start-gcode",
                "G28",
                &files.path("box.stl")
            ])
            .0,
            2
        );
        assert_eq!(call(&["--export-gcode", &files.path("box.stl")]).0, 2);
    }

    #[test]
    fn the_placeholder_gcode_has_no_machine_commands() {
        let files = Files::new();
        let one = ["--filament-map-mode=Manual", "--filament-map=1"];
        assert_eq!(
            slice_box(&files, "machine.json", "process.json", &["pla.json"], &one),
            0
        );
        let file = std::fs::File::open(files.dir.path().join("out/plate_1.gcode.3mf")).unwrap();
        let mut zip = zip::ZipArchive::new(file).unwrap();
        let mut gcode = String::new();
        std::io::Read::read_to_string(
            &mut zip.by_name("Metadata/plate_1.gcode").unwrap(),
            &mut gcode,
        )
        .unwrap();
        // The G-code beside the archive is the same text.
        assert_eq!(
            std::fs::read_to_string(files.dir.path().join("out/plate_1.gcode")).unwrap(),
            gcode
        );
        assert!(gcode.contains("NOT PRINTABLE"));
        assert!(
            gcode
                .lines()
                .all(|line| line.is_empty() || line.starts_with(';')),
            "{gcode}"
        );
    }
}
