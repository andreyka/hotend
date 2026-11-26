//! The whole pipeline against the fake slicer, through the real sandbox: detection, the X2D
//! rules, settings files (the built-in ones and the flattened system profiles of an
//! installation), the command line, the sandboxed run and result parsing.
//!
//! These tests prove Hotend's side of the contract. They cannot prove that a vendor slicer
//! accepts the command line; `real_prusaslicer.rs` does that for PrusaSlicer, and
//! `real_reference.rs` reads a file the real Bambu Studio wrote.

#![allow(clippy::unwrap_used)]

mod common;

use std::io::Read;

use common::*;
use hotend_slicer::{Found, Slicer, SlicerConfig, SlicerKind, Verdict};

#[test]
fn the_fake_is_detected_only_as_an_explicit_override_and_is_labelled() {
    if !sandbox_ready("the_fake_is_detected_only_as_an_explicit_override_and_is_labelled") {
        return;
    }
    let slicer = slicer_with_fake(&[SlicerKind::BambuStudio]);
    let found = slicer.detect().unwrap();
    let bambu: Vec<_> = found
        .iter()
        .filter(|i| i.kind == SlicerKind::BambuStudio)
        .collect();
    assert_eq!(bambu.len(), 1, "an override stops the search: {bambu:?}");
    let fake = bambu[0];
    assert!(fake.fake);
    assert_eq!(fake.found, Found::Override);
    assert_eq!(fake.path, fake_slicer());
    assert_eq!(fake.version_text(), env!("CARGO_PKG_VERSION"));
    assert!(
        matches!(&fake.verdict, Verdict::Warn(reason) if reason.contains("must not be printed"))
    );
    assert_eq!(
        fake.display_name(),
        "Hotend fake slicer (bambu-studio command line)"
    );
    // The same installation is what a slice would use.
    assert_eq!(&slicer.installation(SlicerKind::BambuStudio).unwrap(), fake);

    // An override that does not exist is an error, not a silent fallback to a search.
    let mut config = SlicerConfig::new().unwrap();
    config
        .executables
        .insert(SlicerKind::BambuStudio, "/nonexistent/bambu-studio".into());
    let error = Slicer::new(config).unwrap().detect().unwrap_err();
    assert_eq!(error.code(), "invalid_job");
}

#[test]
fn without_an_installed_bambu_studio_a_slice_says_how_to_configure_one() {
    if !sandbox_ready("without_an_installed_bambu_studio_a_slice_says_how_to_configure_one") {
        return;
    }
    let slicer = Slicer::new(SlicerConfig::new().unwrap()).unwrap();
    if !slicer
        .detect()
        .unwrap()
        .iter()
        .any(|i| i.kind == SlicerKind::BambuStudio)
    {
        let dir = JobDir::new(&["box_20x30x10.3mf"]);
        let job = x2d_job(
            &dir,
            "box_20x30x10.3mf",
            vec![filament(1, "pla-basic", "PLA", 1)],
        );
        let error = slicer.slice(&job).unwrap_err();
        assert_eq!(error.code(), "slicer_not_installed");
        assert!(error.to_string().contains("HOTEND_BAMBU_STUDIO"), "{error}");
    } else {
        println!("SKIPPED the not-installed case: a Bambu Studio is installed on this machine");
    }
}

#[test]
fn an_x2d_job_runs_end_to_end_with_an_explicit_filament_map() {
    if !sandbox_ready("an_x2d_job_runs_end_to_end_with_an_explicit_filament_map") {
        return;
    }
    let slicer = slicer_with_fake(&[SlicerKind::BambuStudio]);
    let dir = JobDir::new(&["box_20x30x10.3mf"]);
    // Slot 1 on the auxiliary nozzle (device extruder 0), slot 2 on the main one (device 1).
    let job = x2d_job(
        &dir,
        "box_20x30x10.3mf",
        vec![
            filament(1, "pla-basic", "PLA", 0),
            filament(2, "petg-basic", "PETG", 1),
        ],
    );
    let result = slicer.slice(&job).unwrap();

    assert_eq!(result.status, "ok");
    assert_eq!(result.output, "output/plate_1.gcode.3mf");
    let output = dir.path.join(&result.output);
    assert_eq!(result.sha256, sha256(&output));
    assert_eq!(
        result.slicer.name,
        "Hotend fake slicer (bambu-studio command line)"
    );
    assert_eq!(result.slicer.version, env!("CARGO_PKG_VERSION"));
    // The box is 6000 mm3 and 10 mm tall; the fake's estimate at 0.2 mm layers.
    assert_eq!(result.layers, Some(50));
    assert_eq!(result.time_s, Some(473.0));
    assert_eq!(result.filament.len(), 2);
    assert_eq!(
        (result.filament[0].slot, result.filament[0].nozzle_id),
        (1, 0)
    );
    assert_eq!(
        (result.filament[1].slot, result.filament[1].nozzle_id),
        (2, 1)
    );
    assert_eq!(result.filament[0].grams, Some(1.3));
    assert_eq!(result.filament[0].metres, Some(0.44));
    // PETG is denser than PLA: the settings files were really read per slot.
    assert_eq!(result.filament[1].grams, Some(1.33));
    assert_eq!(
        codes(&result.warnings),
        ["fake_slicer", "demo_profile", "unverified_printer_facts"]
    );

    // The slicer was told the manual map by slicer extruder number: device 0 is slicer
    // extruder 2, device 1 is slicer extruder 1. The fake echoes what it was given.
    let mut archive = zip::ZipArchive::new(std::fs::File::open(&output).unwrap()).unwrap();
    let mut info = String::new();
    archive
        .by_name("Metadata/slice_info.config")
        .unwrap()
        .read_to_string(&mut info)
        .unwrap();
    assert!(
        info.contains(r#"<metadata key="filament_maps" value="2 1"/>"#),
        "{info}"
    );

    // Settings were written into the job folder, and the slicer's home is inside it.
    for name in [
        "machine.json",
        "process.json",
        "filament_1.json",
        "filament_2.json",
    ] {
        assert!(dir.path.join("profiles").join(name).is_file(), "{name}");
    }
    assert!(dir.path.join(".sandbox/home").is_dir());
    // Nothing appeared next to the job folder.
    let siblings: Vec<_> = std::fs::read_dir(dir.outside())
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(siblings.len(), 1, "{siblings:?}");

    // The JSON is the SliceResult of PROTOCOL.md section 6.
    let json = serde_json::to_value(&result).unwrap();
    let keys: Vec<&str> = json
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "bed",
            "extent",
            "filament",
            "layers",
            "nozzle_temperature_c",
            "output",
            "sha256",
            "slicer",
            "status",
            "time_s",
            "warnings"
        ]
    );
    // The plate the file was sliced for is the one the table states for the stand-in, not
    // the slicer's own, and the bed temperature is the one the file gives for that plate.
    assert_eq!(
        json["bed"],
        serde_json::json!({
            "id": "textured-pei",
            "plate_type": "Textured PEI Plate",
            "temperature_c": 70.0
        })
    );
    assert_eq!(result.nozzle_temperature_c, Some(250.0));
    // The stand-in arranged the box (20 x 30 mm) around the middle of the 256 mm bed, as
    // the real program does when it is not told `--arrange 0`.
    let extent = result.extent.as_ref().unwrap();
    assert_eq!((extent.x, extent.y), ([118.0, 138.0], [113.0, 143.0]));
    assert_eq!(extent.source, "plate_metadata");

    // A second slice into the same folder starts clean and gives the same numbers.
    std::fs::write(dir.path.join("output/leftover.txt"), b"old").unwrap();
    let again = slicer.slice(&job).unwrap();
    assert!(!dir.path.join("output/leftover.txt").exists());
    assert_eq!((again.time_s, again.layers), (result.time_s, result.layers));
}

#[test]
fn a_finer_process_profile_changes_the_result() {
    if !sandbox_ready("a_finer_process_profile_changes_the_result") {
        return;
    }
    let slicer = slicer_with_fake(&[SlicerKind::BambuStudio]);
    let dir = JobDir::new(&["box_20x30x10.stl"]);
    let mut job = x2d_job(
        &dir,
        "box_20x30x10.stl",
        vec![filament(1, "pla-basic", "PLA", 1)],
    );
    job.process_profile = "demo-0.12mm".into();
    let result = slicer.slice(&job).unwrap();
    assert_eq!(result.layers, Some(84));
    assert_eq!(result.filament[0].grams, Some(2.6));

    job.process_profile = "no-such-profile".into();
    assert_eq!(slicer.slice(&job).unwrap_err().code(), "profile_not_found");
    job.process_profile = "../../etc/passwd".into();
    assert_eq!(slicer.slice(&job).unwrap_err().code(), "invalid_job");
}

#[test]
fn x2d_rules_stop_a_job_before_anything_is_written_or_run() {
    if !sandbox_ready("x2d_rules_stop_a_job_before_anything_is_written_or_run") {
        return;
    }
    let slicer = slicer_with_fake(&[SlicerKind::BambuStudio]);
    let dir = JobDir::new(&["box_20x30x10.3mf"]);
    let untouched =
        |dir: &JobDir| !dir.path.join("profiles").exists() && !dir.path.join("output").exists();

    // TPU on the auxiliary nozzle.
    let job = x2d_job(
        &dir,
        "box_20x30x10.3mf",
        vec![filament(1, "tpu-95a", "TPU 95A", 0)],
    );
    assert_eq!(
        slicer.slice(&job).unwrap_err().code(),
        "material_not_allowed"
    );
    assert!(untouched(&dir));
    // The same filament on the main nozzle is fine.
    let job = x2d_job(
        &dir,
        "box_20x30x10.3mf",
        vec![filament(1, "tpu-95a", "TPU 95A", 1)],
    );
    assert!(slicer.slice(&job).is_ok());

    let dir = JobDir::new(&["box_20x30x10.3mf"]);
    // A part placed left of X = 20.5 mm cannot be printed by the auxiliary nozzle.
    let mut job = x2d_job(
        &dir,
        "box_20x30x10.3mf",
        vec![filament(1, "pla-basic", "PLA", 0)],
    );
    job.arrange = false;
    job.extents[0].placed = true;
    assert_eq!(slicer.slice(&job).unwrap_err().code(), "out_of_reach");
    assert!(untouched(&dir));
    // A nozzle the printer does not have.
    let job = x2d_job(
        &dir,
        "box_20x30x10.3mf",
        vec![filament(1, "pla-basic", "PLA", 2)],
    );
    assert_eq!(slicer.slice(&job).unwrap_err().code(), "invalid_job");
    // A STEP file: Bambu Studio is documented for 3MF and STL.
    std::fs::copy(
        fixture("box_20x30x10.step"),
        dir.path.join("input/box.step"),
    )
    .unwrap();
    let job = x2d_job(&dir, "box.step", vec![filament(1, "pla-basic", "PLA", 1)]);
    assert_eq!(slicer.slice(&job).unwrap_err().code(), "input_format");
    assert!(untouched(&dir));

    // Without part sizes the slice runs, and says the reach was not checked.
    let mut job = x2d_job(
        &dir,
        "box_20x30x10.3mf",
        vec![filament(1, "pla-basic", "PLA", 0)],
    );
    job.extents.clear();
    let result = slicer.slice(&job).unwrap();
    assert!(codes(&result.warnings).contains(&"reach_unchecked"));
}

#[test]
fn a_prusa_job_runs_end_to_end_with_the_fake() {
    if !sandbox_ready("a_prusa_job_runs_end_to_end_with_the_fake") {
        return;
    }
    let slicer = slicer_with_fake(&[SlicerKind::PrusaSlicer]);
    let dir = JobDir::new(&["box_20x30x10.stl"]);
    let job = prusa_job(&dir, "box_20x30x10.stl");
    let result = slicer.slice(&job).unwrap();
    assert_eq!(result.output, "output/plate_1.gcode");
    assert_eq!(result.sha256, sha256(&dir.path.join(&result.output)));
    assert_eq!(
        result.slicer.name,
        "Hotend fake slicer (prusa-slicer command line)"
    );
    assert_eq!((result.layers, result.time_s), (Some(50), Some(473.0)));
    assert_eq!(result.filament.len(), 1);
    assert_eq!(result.filament[0].nozzle_id, 0);
    assert_eq!(
        (result.filament[0].grams, result.filament[0].metres),
        (Some(2.6), Some(0.87))
    );
    assert_eq!(
        codes(&result.warnings),
        ["fake_slicer", "demo_profile", "unverified_printer_facts"]
    );

    // One plate, one filament for PrusaSlicer in this version.
    let mut two = job.clone();
    two.filaments.push(filament(2, "pla-basic", "PLA", 0));
    assert_eq!(slicer.slice(&two).unwrap_err().code(), "invalid_job");
    let mut plate = job.clone();
    plate.plate = 2;
    assert_eq!(slicer.slice(&plate).unwrap_err().code(), "invalid_job");
    // A printer is sliced by its own slicer only.
    let fake = slicer.installation(SlicerKind::PrusaSlicer).unwrap();
    let x2d = x2d_job(
        &dir,
        "box_20x30x10.stl",
        vec![filament(1, "pla-basic", "PLA", 1)],
    );
    assert_eq!(
        slicer.slice_with(&fake, &x2d).unwrap_err().code(),
        "invalid_job"
    );
}

#[test]
fn installed_system_profiles_are_flattened_in_the_sandbox_and_sliced_with() {
    if !sandbox_ready("installed_system_profiles_are_flattened_in_the_sandbox_and_sliced_with") {
        return;
    }
    let tree = SystemTree::new();
    let slicer = tree.slicer();
    let dir = JobDir::new(&["box_20x30x10.3mf"]);
    let job = x2d_job(
        &dir,
        "box_20x30x10.3mf",
        vec![
            filament(1, "pla-basic", "PLA", 1),
            filament(2, "petg-basic", "PETG", 0),
        ],
    );
    let result = slicer.slice(&job).unwrap();

    // The leaf profiles of the tree hold no density, temperature or bed size themselves.
    // The numbers below exist only because the parents and templates were merged in.
    assert_eq!(result.layers, Some(50));
    assert_eq!(result.filament[0].grams, Some(1.3));
    assert_eq!(result.filament[1].grams, Some(1.33));
    assert_eq!(result.nozzle_temperature_c, Some(250.0));
    let bed = result.bed.as_ref().unwrap();
    assert_eq!(
        (bed.id.as_str(), bed.plate_type.as_str(), bed.temperature_c),
        ("textured-pei", "Textured PEI Plate", Some(70.0))
    );
    // No built-in settings were used, and the profile name Hotend has not seen on a real
    // installation is named.
    assert_eq!(
        codes(&result.warnings),
        [
            "fake_slicer",
            "unverified_profile_names",
            "profile_overrides",
            "unverified_printer_facts"
        ]
    );
    let unverified = &result.warnings[1].message;
    assert!(
        unverified.contains("Bambu PETG Basic @BBL X2D 0.4 nozzle")
            && !unverified.contains("Bambu PLA Basic"),
        "{unverified}"
    );
    // Every value Hotend set on top of the installation's profiles is named.
    let overrides = &result.warnings[2].message;
    assert!(
        overrides.ends_with("profiles: curr_bed_type = Textured PEI Plate"),
        "{overrides}"
    );

    // What the slicer was given: one flat file per role.
    let machine = job_profile(&dir, "machine.json");
    assert_eq!(machine["name"], "Bambu Lab X2D 0.4 nozzle");
    // The child's value replaces the parent's, the template fills in what neither has, and
    // a value of a type no profile is known to use is carried through unchanged.
    assert_eq!(machine["printable_height"], "260");
    assert_eq!(
        machine["nozzle_diameter"],
        serde_json::json!(["0.4", "0.4"])
    );
    assert_eq!(machine["printable_area"][2], "256x256");
    assert_eq!(machine["test_only_number"], 7);
    let process = job_profile(&dir, "process.json");
    assert_eq!(process["layer_height"], "0.2");
    assert_eq!(process["wall_loops"], "2");
    // The plate type is Hotend's, over the tree's own.
    assert_eq!(process["curr_bed_type"], "Textured PEI Plate");
    let pla = job_profile(&dir, "filament_1.json");
    assert_eq!(pla["filament_density"], serde_json::json!(["1.24"]));
    assert_eq!(pla["filament_diameter"], serde_json::json!(["1.75"]));
    assert_eq!(pla["textured_plate_temp"], serde_json::json!(["55"]));
    assert_eq!(pla["cool_plate_temp"], serde_json::json!(["35"]));
    for profile in [&machine, &process, &pla] {
        assert!(
            profile.get("inherits").is_none() && profile.get("include").is_none(),
            "{profile}"
        );
    }
    // The reader's report says what each file was made from.
    let report = job_profile(&dir, "flatten-report.json");
    assert_eq!(report["ok"], true);
    assert_eq!(
        report["items"][0]["chain"],
        serde_json::json!(["fdm_machine_common", "Bambu Lab X2D 0.4 nozzle"])
    );
    assert_eq!(
        report["items"][0]["includes"],
        serde_json::json!(["Test bed template"])
    );
    assert_eq!(
        report["items"][2]["chain"],
        serde_json::json!([
            "fdm_filament_common",
            "Bambu PLA Basic @base",
            "Bambu PLA Basic @BBL X2D 0.4 nozzle"
        ])
    );
    assert_eq!(report["items"][2]["compatible"], "yes");

    assert_eq!(report["items"][2]["dropped"], serde_json::json!([]));

    // The finer preset is found the same way.
    let mut fine = job.clone();
    fine.process_profile = "fine-0.12mm".into();
    assert_eq!(slicer.slice(&fine).unwrap().layers, Some(84));
}

#[test]
fn the_plate_type_is_hotends_choice_and_is_reported() {
    if !sandbox_ready("the_plate_type_is_hotends_choice_and_is_reported") {
        return;
    }
    let tree = SystemTree::new();
    let dir = JobDir::new(&["box_20x30x10.3mf"]);
    let mut job = x2d_job(
        &dir,
        "box_20x30x10.3mf",
        vec![filament(1, "pla-basic", "PLA", 1)],
    );
    let bed = |slicer: &Slicer, job: &hotend_slicer::SliceJob| {
        let bed = slicer.slice(job).unwrap().bed.unwrap();
        (bed.id, bed.plate_type, bed.temperature_c)
    };
    // The job names no plate: the one the application's settings state.
    let slicer = tree.slicer();
    assert_eq!(
        bed(&slicer, &job),
        (
            "textured-pei".into(),
            "Textured PEI Plate".into(),
            Some(55.0)
        )
    );
    // The job names one.
    job.bed_type = Some("cool".into());
    assert_eq!(
        bed(&slicer, &job),
        ("cool".into(), "Cool Plate".into(), Some(35.0))
    );
    // A plate type Hotend does not know is refused with the ones it knows.
    job.bed_type = Some("glass".into());
    let error = slicer.slice(&job).unwrap_err();
    assert_eq!(error.code(), "invalid_job");
    assert!(error.to_string().contains("textured-pei"), "{error}");
    // Another setting, another plate.
    job.bed_type = None;
    let mut config = SlicerConfig::new().unwrap();
    config
        .executables
        .insert(SlicerKind::BambuStudio, fake_slicer());
    config.system_profiles = Some(tree.root.clone());
    config.default_bed_type = Some("engineering".into());
    let configured = Slicer::new(config).unwrap();
    assert_eq!(
        bed(&configured, &job),
        ("engineering".into(), "Engineering Plate".into(), Some(45.0))
    );
    // PrusaSlicer has no plate types.
    let prusa = slicer_with_fake(&[SlicerKind::PrusaSlicer]);
    let stl = JobDir::new(&["box_20x30x10.stl"]);
    let mut prusa_job = prusa_job(&stl, "box_20x30x10.stl");
    assert!(prusa.slice(&prusa_job).unwrap().bed.is_none());
    prusa_job.bed_type = Some("cool".into());
    assert_eq!(prusa.slice(&prusa_job).unwrap_err().code(), "invalid_job");
}

#[test]
fn placed_parts_are_sliced_where_they_lie() {
    if !sandbox_ready("placed_parts_are_sliced_where_they_lie") {
        return;
    }
    let tree = SystemTree::new();
    let slicer = tree.slicer();
    let dir = JobDir::new(&["box_20x30x10.3mf"]);
    let mut job = x2d_job(
        &dir,
        "box_20x30x10.3mf",
        vec![filament(1, "pla-basic", "PLA", 1)],
    );
    job.arrange = false;
    job.extents[0].placed = true;
    let result = slicer.slice(&job).unwrap();
    let extent = result.extent.unwrap();
    assert_eq!((extent.x, extent.y), ([0.0, 20.0], [0.0, 30.0]));

    // The same job with `arrange` on lets the slicer place the box; nothing is compared
    // then, because the positions are the slicer's to choose.
    job.arrange = true;
    job.extents[0].placed = false;
    let extent = slicer.slice(&job).unwrap().extent.unwrap();
    assert_eq!((extent.x, extent.y), ([118.0, 138.0], [113.0, 143.0]));
}

#[test]
fn a_profile_tree_that_cannot_be_flattened_stops_the_job_before_the_slicer_runs() {
    if !sandbox_ready(
        "a_profile_tree_that_cannot_be_flattened_stops_the_job_before_the_slicer_runs",
    ) {
        return;
    }
    let dir = JobDir::new(&["box_20x30x10.3mf"]);
    let job = x2d_job(
        &dir,
        "box_20x30x10.3mf",
        vec![filament(1, "pla-basic", "PLA", 1)],
    );
    let not_run = |dir: &JobDir| !dir.path.join("output/result.json").exists();

    // A profile the installation does not have: the error names what was looked for, where,
    // and what is there instead.
    let tree = SystemTree::new();
    std::fs::remove_file(
        tree.root
            .join("BBL/filament/Bambu PLA Basic @BBL X2D 0.4 nozzle.json"),
    )
    .unwrap();
    let error = tree.slicer().slice(&job).unwrap_err();
    assert_eq!(error.code(), "profile_not_found");
    let text = error.to_string();
    assert!(
        text.contains("Bambu PLA Basic @BBL X2D 0.4 nozzle")
            && text.contains("BBL")
            && text.contains("Bambu PETG Basic @BBL X2D 0.4 nozzle"),
        "{text}"
    );
    assert!(not_run(&dir));

    // A parent that is missing is named with the profile that asks for it.
    let tree = SystemTree::new();
    std::fs::remove_file(tree.root.join("BBL/process/fdm_process_common.json")).unwrap();
    let error = tree.slicer().slice(&job).unwrap_err();
    assert_eq!(error.code(), "profile_not_found");
    assert!(error.to_string().contains("fdm_process_common"), "{error}");

    // A circle of parents.
    let tree = SystemTree::new();
    tree.put(
        "machine",
        "fdm_machine_common",
        r#"{"name": "fdm_machine_common", "inherits": "Bambu Lab X2D 0.4 nozzle"}"#,
    );
    let error = tree.slicer().slice(&job).unwrap_err();
    assert_eq!(error.code(), "profile_unusable");
    assert!(error.to_string().contains("circle"), "{error}");
    assert!(not_run(&dir));

    // A parent named by a path out of the folder.
    let tree = SystemTree::new();
    std::fs::write(tree.root.join("outside.json"), r#"{"layer_height": "9"}"#).unwrap();
    tree.put(
        "process",
        "0.20mm Standard @BBL X2D",
        r#"{"name": "0.20mm Standard @BBL X2D", "inherits": "../../outside"}"#,
    );
    let error = tree.slicer().slice(&job).unwrap_err();
    assert_eq!(error.code(), "profile_unusable");
    assert!(not_run(&dir));

    // A profile that is a link to a file elsewhere.
    #[cfg(unix)]
    {
        let tree = SystemTree::new();
        let target = tree.root.join("elsewhere.json");
        std::fs::write(&target, r#"{"filament_density": ["1.24"]}"#).unwrap();
        let link = tree
            .root
            .join("BBL/filament/Bambu PLA Basic @BBL X2D 0.4 nozzle.json");
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let error = tree.slicer().slice(&job).unwrap_err();
        assert_eq!(error.code(), "profile_unusable");
        assert!(not_run(&dir));
    }

    // A file far larger than any profile.
    let tree = SystemTree::new();
    tree.put(
        "machine",
        "Test bed template",
        &format!(r#"{{"filler": "{}"}}"#, "x".repeat(2 * 1024 * 1024)),
    );
    let error = tree.slicer().slice(&job).unwrap_err();
    assert_eq!(error.code(), "profile_unusable");
    assert!(error.to_string().contains("large"), "{error}");
    assert!(not_run(&dir));

    // No profile folder at the configured place.
    let tree = SystemTree::new();
    std::fs::remove_dir_all(tree.root.join("BBL")).unwrap();
    let error = tree.slicer().slice(&job).unwrap_err();
    assert_eq!(error.code(), "profile_not_found");
}

/// The regression guard for flattening. The real program, given a system profile that was
/// not flattened, "succeeded" with a filament density of 0 and a weight of 0.00 g. The
/// stand-in does the same, and such a result is refused.
#[test]
fn a_slice_with_settings_that_lost_their_parents_is_refused() {
    if !sandbox_ready("a_slice_with_settings_that_lost_their_parents_is_refused") {
        return;
    }
    // The density of the tree's PLA is in the parent of the selected profile. Cut the
    // parent off, which is what a flattening that does not follow `inherits` amounts to.
    let tree = SystemTree::new();
    tree.put(
        "filament",
        "Bambu PLA Basic @BBL X2D 0.4 nozzle",
        r#"{"type": "filament", "name": "Bambu PLA Basic @BBL X2D 0.4 nozzle",
            "from": "system", "instantiation": "true",
            "compatible_printers": ["Bambu Lab X2D 0.4 nozzle"]}"#,
    );
    let dir = JobDir::new(&["box_20x30x10.3mf"]);
    let job = x2d_job(
        &dir,
        "box_20x30x10.3mf",
        vec![filament(1, "pla-basic", "PLA", 1)],
    );
    let error = tree.slicer().slice(&job).unwrap_err();
    assert_eq!(error.code(), "unusable_output");
    assert!(error.to_string().contains("not flattened"), "{error}");
    // The stand-in itself reported success, as the real program did.
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path.join("output/result.json")).unwrap())
            .unwrap();
    assert_eq!(report["return_code"], 0);
    assert_eq!(
        report["sliced_plates"][0]["filaments"][0]["total_used_g"],
        0.0
    );
    assert_eq!(
        report["sliced_plates"][0]["filaments"][0]["filament_id"],
        ""
    );
}

#[test]
fn only_the_presets_and_materials_of_the_table_select_a_system_profile() {
    if !sandbox_ready("only_the_presets_and_materials_of_the_table_select_a_system_profile") {
        return;
    }
    let dir = JobDir::new(&["box_20x30x10.3mf"]);
    let job = x2d_job(
        &dir,
        "box_20x30x10.3mf",
        vec![filament(1, "pla-basic", "PLA", 1)],
    );
    let not_run = |dir: &JobDir| !dir.path.join("output/result.json").exists();

    // The process profile id is what the agent's `slice` tool passes on. It selects a
    // preset of Hotend's table and never a file of the installation by its name: not a
    // profile for another printer, not a base profile, not even the preset's own file.
    let tree = SystemTree::new();
    tree.put(
        "process",
        "0.28mm Draft @Other Printer 0.8 nozzle",
        r#"{"type": "process", "name": "0.28mm Draft @Other Printer 0.8 nozzle",
            "from": "system", "instantiation": "true", "inherits": "fdm_process_common",
            "layer_height": "0.28", "compatible_printers": ["Other Printer 0.8 nozzle"]}"#,
    );
    let slicer = tree.slicer();
    for id in [
        "0.28mm Draft @Other Printer 0.8 nozzle",
        "fdm_process_common",
        "0.20mm Standard @BBL X2D",
    ] {
        let mut named = job.clone();
        named.process_profile = id.into();
        let error = slicer.slice(&named).unwrap_err();
        assert_eq!(error.code(), "invalid_job", "{id}");
        let text = error.to_string();
        assert!(
            text.contains("standard-0.20mm") && text.contains("fine-0.12mm"),
            "{text}"
        );
        assert!(!dir.path.join("profiles").exists(), "{id}");
    }
    // The same holds for the material.
    let mut named = job.clone();
    named.filaments[0].filament_profile = "Bambu PLA Basic @base".into();
    let error = slicer.slice(&named).unwrap_err();
    assert_eq!(error.code(), "invalid_job");
    assert!(error.to_string().contains("petg-basic"), "{error}");

    // A profile the table does select, which the installation says is for other printers.
    let tree = SystemTree::new();
    tree.put(
        "process",
        "0.20mm Standard @BBL X2D",
        r#"{"type": "process", "name": "0.20mm Standard @BBL X2D", "from": "system",
            "instantiation": "true", "inherits": "fdm_process_common",
            "compatible_printers": ["Other Printer 0.8 nozzle"]}"#,
    );
    let error = tree.slicer().slice(&job).unwrap_err();
    assert_eq!(error.code(), "profile_unusable");
    assert!(
        error.to_string().contains("Bambu Lab X2D 0.4 nozzle")
            && error.to_string().contains("0.20mm Standard @BBL X2D"),
        "{error}"
    );
    assert!(not_run(&dir));

    // One that is a base for other profiles, not a preset to slice with.
    let tree = SystemTree::new();
    tree.put(
        "filament",
        "Bambu PLA Basic @BBL X2D 0.4 nozzle",
        r#"{"type": "filament", "name": "Bambu PLA Basic @BBL X2D 0.4 nozzle",
            "from": "system", "instantiation": "false", "inherits": "Bambu PLA Basic @base",
            "compatible_printers": ["Bambu Lab X2D 0.4 nozzle"]}"#,
    );
    let error = tree.slicer().slice(&job).unwrap_err();
    assert_eq!(error.code(), "profile_unusable");
    assert!(error.to_string().contains("instantiation"), "{error}");
    assert!(not_run(&dir));

    // One that lies in the folder of one role and says it is of another.
    let tree = SystemTree::new();
    tree.put(
        "process",
        "0.20mm Standard @BBL X2D",
        r#"{"type": "machine", "name": "0.20mm Standard @BBL X2D", "from": "system",
            "instantiation": "true", "compatible_printers": ["Bambu Lab X2D 0.4 nozzle"]}"#,
    );
    let error = tree.slicer().slice(&job).unwrap_err();
    assert_eq!(error.code(), "profile_unusable");
    assert!(not_run(&dir));

    // One that does not say which printers it is for is used, and the result says so.
    let tree = SystemTree::new();
    tree.put(
        "process",
        "0.20mm Standard @BBL X2D",
        r#"{"type": "process", "name": "0.20mm Standard @BBL X2D", "from": "system",
            "instantiation": "true", "inherits": "fdm_process_common"}"#,
    );
    let result = tree.slicer().slice(&job).unwrap();
    let warning = result
        .warnings
        .iter()
        .find(|w| w.code == "profile_compatibility")
        .unwrap();
    assert!(
        warning.message.contains("0.20mm Standard @BBL X2D"),
        "{}",
        warning.message
    );
}

#[test]
fn a_process_or_filament_profile_cannot_bring_gcode_or_a_script() {
    if !sandbox_ready("a_process_or_filament_profile_cannot_bring_gcode_or_a_script") {
        return;
    }
    let dir = JobDir::new(&["box_20x30x10.3mf"]);
    let job = x2d_job(
        &dir,
        "box_20x30x10.3mf",
        vec![filament(1, "pla-basic", "PLA", 1)],
    );
    let not_run = |dir: &JobDir| !dir.path.join("output/result.json").exists();
    let process_parent = |extra: &str| {
        format!(
            r#"{{"type": "process", "name": "fdm_process_common", "from": "system",
                "instantiation": "false", "layer_height": "0.3", "wall_loops": "2"{extra}}}"#
        )
    };
    let filament_parent = |extra: &str| {
        format!(
            r#"{{"type": "filament", "name": "fdm_filament_common", "from": "system",
                "instantiation": "false", "filament_diameter": ["1.75"],
                "cool_plate_temp": ["35"], "textured_plate_temp": ["45"]{extra}}}"#
        )
    };

    // The process settings are loaded after the machine's. A start routine, a layer change
    // routine or a post-processing script in them, from wherever in the chain, stops the
    // job: nothing but the machine profile decides what the printer is told around a print.
    for (role, parent, key) in [
        (
            "process",
            process_parent(r#", "machine_start_gcode": "G1 Z-5 F9000""#),
            "machine_start_gcode",
        ),
        (
            "process",
            process_parent(r#", "layer_change_gcode": "M104 S300""#),
            "layer_change_gcode",
        ),
        (
            "process",
            process_parent(r#", "post_process": ["/bin/sh -c id"]"#),
            "post_process",
        ),
        (
            "filament",
            filament_parent(r#", "machine_end_gcode": "M140 S120""#),
            "machine_end_gcode",
        ),
        (
            "filament",
            filament_parent(r#", "post_process": "/bin/sh -c id""#),
            "post_process",
        ),
    ] {
        let tree = SystemTree::new();
        let name = if role == "process" {
            "fdm_process_common"
        } else {
            "fdm_filament_common"
        };
        tree.put(role, name, &parent);
        let error = tree.slicer().slice(&job).unwrap_err();
        assert_eq!(error.code(), "profile_unusable", "{key}");
        assert!(error.to_string().contains(key), "{error}");
        assert!(not_run(&dir), "{key}");
        assert!(!dir.path.join("profiles/process.json").exists(), "{key}");
    }
    // A script to run on the G-code is not taken from the machine profile either.
    let tree = SystemTree::new();
    tree.put(
        "machine",
        "fdm_machine_common",
        r#"{"type": "machine", "name": "fdm_machine_common", "from": "system",
            "instantiation": "false", "printable_height": "200", "nozzle_diameter": ["0.4"],
            "post_process": ["/bin/sh -c id"]}"#,
    );
    let error = tree.slicer().slice(&job).unwrap_err();
    assert_eq!(error.code(), "profile_unusable");
    assert!(error.to_string().contains("post_process"), "{error}");

    // The same keys without content carry nothing. They are left out, so that an empty
    // one cannot erase the machine's routine either, and the result names them. The
    // machine keeps its routines and the filament its own two.
    let tree = SystemTree::new();
    tree.put(
        "process",
        "fdm_process_common",
        &process_parent(
            r#", "post_process": [], "layer_change_gcode": "", "machine_start_gcode": [""]"#,
        ),
    );
    tree.put(
        "filament",
        "fdm_filament_common",
        &filament_parent(
            r#", "filament_start_gcode": ["; made up start"], "filament_end_gcode": ["; made up end"],
               "machine_pause_gcode": """#,
        ),
    );
    tree.put(
        "machine",
        "fdm_machine_common",
        r#"{"type": "machine", "name": "fdm_machine_common", "from": "system",
            "instantiation": "false", "printable_height": "200", "nozzle_diameter": ["0.4"],
            "machine_start_gcode": "; made up routine", "post_process": []}"#,
    );
    let result = tree.slicer().slice(&job).unwrap();
    let process = job_profile(&dir, "process.json");
    for key in ["post_process", "layer_change_gcode", "machine_start_gcode"] {
        assert!(process.get(key).is_none(), "{key}: {process}");
    }
    let pla = job_profile(&dir, "filament_1.json");
    assert!(pla.get("machine_pause_gcode").is_none(), "{pla}");
    assert_eq!(
        pla["filament_start_gcode"],
        serde_json::json!(["; made up start"])
    );
    assert_eq!(
        pla["filament_end_gcode"],
        serde_json::json!(["; made up end"])
    );
    let machine = job_profile(&dir, "machine.json");
    assert_eq!(machine["machine_start_gcode"], "; made up routine");
    assert!(machine.get("post_process").is_none(), "{machine}");
    let warning = result
        .warnings
        .iter()
        .find(|w| w.code == "profile_keys_dropped")
        .unwrap();
    for key in [
        "layer_change_gcode",
        "machine_start_gcode",
        "machine_pause_gcode",
        "post_process",
    ] {
        assert!(warning.message.contains(key), "{}", warning.message);
    }
    let report = job_profile(&dir, "flatten-report.json");
    assert_eq!(
        report["items"][1]["dropped"],
        serde_json::json!(["layer_change_gcode", "machine_start_gcode", "post_process"])
    );
}

#[cfg(unix)]
#[test]
fn linked_folders_in_the_profile_tree_are_not_followed() {
    use std::os::unix::fs::symlink;
    if !sandbox_ready("linked_folders_in_the_profile_tree_are_not_followed") {
        return;
    }
    let dir = JobDir::new(&["box_20x30x10.3mf"]);
    let job = x2d_job(
        &dir,
        "box_20x30x10.3mf",
        vec![filament(1, "pla-basic", "PLA", 1)],
    );
    let refused = |tree: &SystemTree, what: &str| {
        let error = tree.slicer().slice(&job).unwrap_err();
        assert_eq!(error.code(), "profile_unusable", "{what}: {error}");
        assert!(
            error.to_string().contains("symbolic link"),
            "{what}: {error}"
        );
        assert!(!dir.path.join("output/result.json").exists(), "{what}");
    };

    // The vendor folder is a link to a folder elsewhere.
    let tree = SystemTree::new();
    let elsewhere = tree.root.parent().unwrap().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::rename(tree.root.join("BBL"), elsewhere.join("BBL")).unwrap();
    symlink(elsewhere.join("BBL"), tree.root.join("BBL")).unwrap();
    refused(&tree, "vendor folder");

    // A role folder is a link to another folder inside the vendor folder.
    let tree = SystemTree::new();
    std::fs::rename(tree.root.join("BBL/filament"), tree.root.join("BBL/other")).unwrap();
    symlink(tree.root.join("BBL/other"), tree.root.join("BBL/filament")).unwrap();
    refused(&tree, "role folder, inside");

    // A role folder is a link to a folder outside the vendor folder. The reader refuses
    // it for what it is; it does not take the sandbox to stop it.
    let tree = SystemTree::new();
    let outside = tree.root.parent().unwrap().join("outside-filament");
    std::fs::rename(tree.root.join("BBL/filament"), &outside).unwrap();
    symlink(&outside, tree.root.join("BBL/filament")).unwrap();
    refused(&tree, "role folder, outside");
}

#[cfg(unix)]
#[test]
fn the_profile_reader_sees_the_vendor_folder_and_its_own_file_and_writes_profiles_only() {
    use std::os::unix::fs::PermissionsExt;
    if !sandbox_ready(
        "the_profile_reader_sees_the_vendor_folder_and_its_own_file_and_writes_profiles_only",
    ) {
        return;
    }
    let tree = SystemTree::new();
    let dir = JobDir::new(&["box_20x30x10.3mf"]);
    let job = x2d_job(
        &dir,
        "box_20x30x10.3mf",
        vec![filament(1, "pla-basic", "PLA", 1)],
    );

    // A reader that tries what a reader taken over by a hostile profile could try, and
    // reports what worked. It lies in a folder it shares with a private file, as
    // Hotend's programs may.
    let tmp = tempfile::tempdir().unwrap();
    let folder = std::fs::canonicalize(tmp.path()).unwrap();
    std::fs::write(folder.join("beside-the-reader.txt"), "private").unwrap();
    let reader = folder.join("reader.sh");
    let vendor = tree.root.join("BBL");
    let body = format!(
        r#"#!/bin/sh
request="$1"
out="$2"
job="$(dirname "$out")"
here="$(dirname "$0")"
did=
cat "$here/beside-the-reader.txt" >/dev/null 2>&1 && did="$did read-beside"
ls "$here" >/dev/null 2>&1 && did="$did list-own-folder"
cat "$job/input/box_20x30x10.3mf" >/dev/null 2>&1 && did="$did read-model"
ls "$job" >/dev/null 2>&1 && did="$did list-job"
( echo x > "$job/planted.txt" ) 2>/dev/null && did="$did write-job"
( echo x > "$job/input/planted.3mf" ) 2>/dev/null && did="$did write-input"
( echo x > "$here/planted.txt" ) 2>/dev/null && did="$did write-own-folder"
( echo x > '{vendor}/planted.json' ) 2>/dev/null && did="$did write-vendor"
cat "$request" >/dev/null 2>&1 && did="$did read-request"
ls '{vendor}/process' >/dev/null 2>&1 && did="$did list-vendor"
printf '{{"ok": false, "items": [], "error": {{"code": "probe", "role": "machine", "name": "probe", "message": "did:%s."}}}}' "$did" > "$out/flatten-report.json"
"#,
        vendor = vendor.display()
    );
    std::fs::write(&reader, body).unwrap();
    std::fs::set_permissions(&reader, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut config = tree.config();
    config.profile_reader = Some(reader);
    let error = Slicer::new(config).unwrap().slice(&job).unwrap_err();
    assert_eq!(error.code(), "profile_unusable");
    // It read its request and the vendor's profiles and wrote its report: what it is for.
    // Nothing else worked.
    assert!(
        error.to_string().contains("did: read-request list-vendor."),
        "{error}"
    );
    for planted in [
        dir.path.join("planted.txt"),
        dir.path.join("input/planted.3mf"),
        folder.join("planted.txt"),
        vendor.join("planted.json"),
    ] {
        assert!(!planted.exists(), "{}", planted.display());
    }
    // What the sandbox made for the reader is gone, and the slicer was not run.
    let left: Vec<String> = std::fs::read_dir(dir.path.join("profiles"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(!left.iter().any(|name| name.starts_with('.')), "{left:?}");
    assert!(!dir.path.join("output/result.json").exists());
}
