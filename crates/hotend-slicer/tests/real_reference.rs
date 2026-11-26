//! Reads what the real Bambu Studio wrote: the recorded output of version 02.09.01.70,
//! sliced for the X2D on 17 November 2026 (flattened system profiles, `--arrange 0`, one
//! PLA filament, the command line's default plate).
//!
//! The recorded files hold the vendor's start and end G-code and profile text, so they are
//! not in this repository and these tests are not part of `cargo test`: they are marked
//! `ignore`. Where the recording is available, name its folder and run them:
//!
//! ```text
//! HOTEND_REAL_SLICE_REF=/path/to/recording \
//!     cargo test -p hotend-slicer --test real_reference -- --ignored
//! ```
//!
//! The folder holds `plate_1.gcode.3mf` and `real-cli-logs/` with the three recorded
//! `result.json` files. Run like that, a missing folder fails the test: a pass always means
//! the real files were read. No slicer runs here; the last test replays the recorded
//! output through the same code that follows a real run.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::PathBuf;

use common::*;
use hotend_slicer::output::{OutputLimits, bambu, result_json};
use hotend_slicer::{SlicerKind, SlicerVersion, SystemProfiles, Verdict, VersionPolicy};

fn recording() -> PathBuf {
    let dir = std::env::var_os("HOTEND_REAL_SLICE_REF")
        .expect("set HOTEND_REAL_SLICE_REF to the folder of the recorded real output");
    let dir = std::fs::canonicalize(PathBuf::from(dir)).unwrap();
    assert!(dir.join("plate_1.gcode.3mf").is_file(), "{}", dir.display());
    dir
}

#[test]
#[ignore = "needs the recorded output of a real Bambu Studio; set HOTEND_REAL_SLICE_REF and run with `-- --ignored`"]
fn the_real_sliced_file_is_read_with_the_measured_numbers() {
    let file = std::fs::File::open(recording().join("plate_1.gcode.3mf")).unwrap();
    let beds = SystemProfiles::builtin().unwrap().bed_types;
    let parsed = bambu::parse(
        file,
        1,
        Some("filament_maps"),
        &beds,
        &OutputLimits::default(),
    )
    .unwrap();

    assert_eq!(parsed.slicer_version.as_deref(), Some("02.09.01.70"));
    // Total time, start routine included: 11 min 50 s.
    assert_eq!(parsed.time_s, Some(710.0));
    assert_eq!(parsed.layers, Some(26));
    // One filament: 4.52 g and 1490.77 mm (the header's figures; the metadata rounds the
    // length to 1.49 m), on slicer extruder 1.
    assert_eq!(
        parsed.filaments.iter().collect::<Vec<_>>(),
        [(&1, &(Some(4.52), Some(1.49077)))]
    );
    assert_eq!(parsed.reported_map, Some(vec![1]));
    assert!(!parsed.density_missing);
    // The plate the command line chose by itself, and the temperatures the file commands.
    assert_eq!(parsed.bed_type.as_deref(), Some("Cool Plate"));
    assert_eq!(parsed.bed_temperature_c, Some(35.0));
    assert_eq!(parsed.nozzle_temperature_c, Some(220.0));
    // Where the file prints: the extruding moves between the start and the end routine.
    let extent = parsed.extent.unwrap();
    assert_eq!(extent.source, "gcode");
    assert_eq!(extent.x, [2.215, 133.393]);
    assert_eq!(extent.y, [2.21, 95.391]);
    assert_eq!(extent.z, Some([0.2, 5.2]));
    let codes: Vec<&str> = parsed.warnings.iter().map(|w| w.code.as_str()).collect();
    assert!(codes.is_empty(), "{codes:?}");

    // The version the file states is the measured one of the policy.
    let policy = VersionPolicy::builtin().unwrap();
    let version = SlicerVersion::parse(parsed.slicer_version.as_deref().unwrap());
    let verdict = policy.verdict(SlicerKind::BambuStudio, version.as_ref());
    assert!(
        matches!(&verdict, Verdict::Warn(reason) if reason.contains("was measured against")),
        "{verdict:?}"
    );
}

#[test]
#[ignore = "needs the recorded output of a real Bambu Studio; set HOTEND_REAL_SLICE_REF and run with `-- --ignored`"]
fn the_real_result_files_are_read() {
    let logs = recording().join("real-cli-logs");
    let read = |name: &str| result_json::parse(&std::fs::read(logs.join(name)).unwrap()).unwrap();

    // Flattened profiles, `--arrange 0`.
    let kept = read("result-flat-arrange0.json");
    assert!(kept.succeeded());
    assert_eq!(kept.error, "Success.");
    let plate = kept.plate(1).unwrap();
    // Seconds with a fraction here; the sliced file rounds the total to 710. The "main"
    // time is not the model time of the file's header (401 s) and is not compared with it.
    let near = |value: Option<f64>, whole: f64| value.is_some_and(|v| (v - whole).abs() < 1.0);
    assert!(near(plate.total_s, 710.0), "{:?}", plate.total_s);
    assert!(near(plate.model_s, 373.0), "{:?}", plate.model_s);
    assert_eq!(plate.warning, "");
    assert_eq!(plate.filaments.len(), 1);
    assert_eq!(
        (
            plate.filaments[0].id,
            plate.filaments[0].filament_id.as_str()
        ),
        (1, "GFA00")
    );
    // Not rounded here; the sliced file says 4.52 g.
    let grams = plate.filaments[0].total_g.unwrap();
    assert!((grams - 4.52).abs() < 0.005, "{grams}");

    // System profiles that were not flattened: "success" with nothing weighed.
    let unflattened = read("result-unflattened.json");
    assert!(unflattened.succeeded());
    let filament = &unflattened.plate(1).unwrap().filaments[0];
    assert_eq!(
        (filament.filament_id.as_str(), filament.total_g),
        ("", Some(0.0))
    );

    // `--info` alone: a result without plates.
    let info = read("02-info-result.json");
    assert!(info.succeeded());
    assert!(info.plates.is_empty());
}

/// The recorded output, replayed: a script copies the real sliced file and the real
/// `result.json` to where the real program put them, and Hotend judges them as it would
/// after running the real program.
#[cfg(unix)]
#[test]
#[ignore = "needs the recorded output of a real Bambu Studio; set HOTEND_REAL_SLICE_REF and run with `-- --ignored`"]
fn the_recorded_run_passes_the_checks_that_follow_a_real_run() {
    if !sandbox_ready("the_recorded_run_passes_the_checks_that_follow_a_real_run") {
        return;
    }
    let recording = recording();
    let mut replay = Script::new(
        SlicerKind::BambuStudio,
        &format!(
            "cp '{dir}/plate_1.gcode.3mf' output/plate_1.gcode.3mf\n\
             cp '{dir}/real-cli-logs/result-flat-arrange0.json' output/result.json",
            dir = recording.display()
        ),
    );
    replay.installation.read_only.push(recording);

    let tree = SystemTree::new();
    let slicer = tree.slicer();
    let dir = JobDir::new(&["box_20x30x10.3mf"]);
    let mut job = x2d_job(
        &dir,
        "box_20x30x10.3mf",
        vec![filament(1, "pla-basic", "PLA", 1)],
    );
    // The plate of the recorded run: parts between 2 and 133.6 mm in X and 2 and 95.6 mm
    // in Y, 5.2 mm tall, kept where they lie, on the plate the command line defaulted to.
    job.arrange = false;
    job.extents[0].min = [2.0, 2.0, 0.0];
    job.extents[0].max = [133.6, 95.6, 5.2];
    job.extents[0].placed = true;
    job.bed_type = Some("cool".into());

    let result = slicer.slice_with(&replay.installation, &job).unwrap();
    assert_eq!(result.slicer.version, "02.09.01.70");
    assert_eq!((result.time_s, result.layers), (Some(710.0), Some(26)));
    assert_eq!(result.filament.len(), 1);
    assert_eq!(
        (
            result.filament[0].slot,
            result.filament[0].nozzle_id,
            result.filament[0].grams,
            result.filament[0].metres
        ),
        (1, 1, Some(4.52), Some(1.49077))
    );
    let bed = result.bed.as_ref().unwrap();
    assert_eq!(
        (bed.id.as_str(), bed.plate_type.as_str(), bed.temperature_c),
        ("cool", "Cool Plate", Some(35.0))
    );
    assert_eq!(result.nozzle_temperature_c, Some(220.0));
    assert_eq!(result.extent.as_ref().unwrap().x, [2.215, 133.393]);
    // The version comes from the file (the program printed nothing) and is the measured
    // one; nothing in the file disagrees with result.json. The plate Hotend set is named.
    // The recorded run was made before Hotend put its print settings into a slice, and
    // this job carries none, so the slice says it was made with the profiles as they are.
    assert_eq!(
        codes(&result.warnings),
        [
            "slicer_version",
            "print_settings_missing",
            "profile_overrides",
            "unverified_printer_facts"
        ]
    );
    let overrides = result
        .warnings
        .iter()
        .find(|warning| warning.code == "profile_overrides")
        .unwrap();
    assert!(
        overrides.message.contains("curr_bed_type = Cool Plate"),
        "{}",
        overrides.message
    );

    // The same file for a job that asked for the textured plate is refused: it would heat
    // the bed for another plate than the one on the printer.
    job.bed_type = None;
    let error = slicer.slice_with(&replay.installation, &job).unwrap_err();
    assert_eq!(error.code(), "bed_type_not_applied");
    // And for a plate whose parts lie elsewhere: the file does not print where they are.
    job.bed_type = Some("cool".into());
    job.extents[0].min = [100.0, 120.0, 0.0];
    job.extents[0].max = [231.6, 213.6, 5.2];
    let error = slicer.slice_with(&replay.installation, &job).unwrap_err();
    assert_eq!(error.code(), "positions_not_kept");
    // And for a plate with one small part inside the printed area: the file prints far
    // more than that part, so it is not this job's.
    job.extents[0].min = [60.0, 40.0, 0.0];
    job.extents[0].max = [80.0, 60.0, 5.2];
    let error = slicer.slice_with(&replay.installation, &job).unwrap_err();
    assert_eq!(error.code(), "positions_not_kept");
    assert!(error.to_string().contains("133.4"), "{error}");
}
