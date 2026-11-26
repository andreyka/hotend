//! Prints what Hotend reads from a file Bambu Studio sliced, and from its `result.json`:
//!
//! ```text
//! cargo run -q -p hotend-slicer --example read_sliced -- plate_1.gcode.3mf [result.json]
//! ```
//!
//! For checking a file that was sliced by hand (for example by the Windows program, which
//! Hotend cannot run). Nothing is sliced, sent or changed.

use std::path::PathBuf;
use std::process::ExitCode;

use hotend_slicer::output::{OutputLimits, bambu, result_json};
use hotend_slicer::{SlicerKind, SlicerVersion, SystemProfiles, VersionPolicy};

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1).map(PathBuf::from);
    let Some(sliced) = args.next() else {
        eprintln!("usage: read_sliced <file.gcode.3mf> [result.json]");
        return ExitCode::from(2);
    };
    match read(&sliced, args.next()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn read(sliced: &PathBuf, result: Option<PathBuf>) -> Result<(), String> {
    let table = SystemProfiles::builtin().map_err(|e| e.to_string())?;
    let file = std::fs::File::open(sliced).map_err(|e| format!("{}: {e}", sliced.display()))?;
    let parsed = bambu::parse(
        file,
        1,
        Some("filament_maps"),
        &table.bed_types,
        &OutputLimits::default(),
    )
    .map_err(|e| e.to_string())?;
    println!("slicer version : {:?}", parsed.slicer_version);
    if let Some(version) = &parsed.slicer_version {
        let policy = VersionPolicy::builtin().map_err(|e| e.to_string())?;
        let verdict = policy.verdict(
            SlicerKind::BambuStudio,
            SlicerVersion::parse(version).as_ref(),
        );
        println!("version policy : {verdict:?}");
    }
    println!("time (s)       : {:?}", parsed.time_s);
    println!("layers         : {:?}", parsed.layers);
    for (id, (grams, metres)) in &parsed.filaments {
        println!("filament {id}     : {grams:?} g, {metres:?} m");
    }
    println!("filament map   : {:?}", parsed.reported_map);
    let known = parsed
        .bed_type
        .as_deref()
        .and_then(|value| table.bed_type_by_value(value))
        .map(|bed| bed.id.as_str());
    println!(
        "plate type     : {:?} (Hotend's id: {known:?})",
        parsed.bed_type
    );
    println!("bed (C)        : {:?}", parsed.bed_temperature_c);
    println!("nozzle (C)     : {:?}", parsed.nozzle_temperature_c);
    println!("printed extent : {:?}", parsed.extent);
    println!("density missing: {}", parsed.density_missing);
    for warning in &parsed.warnings {
        println!("warning        : {} ({})", warning.code, warning.message);
    }
    if let Some(path) = result {
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let report = result_json::parse(&bytes)?;
        println!("result.json    : {report:?}");
    }
    Ok(())
}
