//! `check_job`: the gate every sliced file passes before it can be delivered.
#![allow(clippy::unwrap_used)]

mod common;

use common::{
    COOL_PLATE, GOOD_FILAMENT_MM, GOOD_GCODE, RawEntry, RawLayout, TEXTURED_PLATE, TWO_GCODE,
    check, filament, good_slice_info, job_on, md5_upper, pla_job, raw_good, slice_info,
    sliced_entries, two_nozzle_job, two_slice_info, write_good, write_raw, write_sliced, write_two,
    write_with, x2d, zip_bytes,
};
use hotend_printer::{ArchiveLimits, CheckedJob, DeclaredJob, JobError, check_job};

#[track_caller]
fn rejected(result: Result<CheckedJob, JobError>, code: &str) {
    match result {
        Err(JobError::Rejected(report)) => {
            assert!(
                report.has(code),
                "expected {code}, got {:?}",
                report.findings
            );
        }
        other => panic!("expected a rejection with {code}, got {other:?}"),
    }
}

/// The file is refused as unusable, and the reason names `part`.
#[track_caller]
fn unusable(result: Result<CheckedJob, JobError>, part: &str) {
    match result {
        Err(JobError::Unusable(why)) => assert!(why.contains(part), "{part}: {why}"),
        other => panic!("expected an unusable file ({part}), got {other:?}"),
    }
}

/// The archive is refused as unsafe, and the reason names `part`.
#[track_caller]
fn unsafe_archive(result: Result<CheckedJob, JobError>, part: &str) {
    match result {
        Err(JobError::Archive(why)) => {
            assert!(why.to_string().contains(part), "{part}: {why}");
        }
        other => panic!("expected an unsafe archive ({part}), got {other:?}"),
    }
}

#[test]
fn a_good_file_is_checked_and_summarised_from_its_own_content() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_good(dir.path(), "box.gcode.3mf");
    let job = check(&path, "Soap dish: v2 / final", &pla_job()).unwrap();

    assert_eq!(job.name(), "Soap dish_ v2 _ final");
    // The name on the printer carries the first 32 hex digits of the file's hash.
    assert_eq!(
        job.remote_name(),
        format!("Soap_dish__v2___final-{}.gcode.3mf", &job.sha256()[..32])
    );
    assert_eq!(job.printer_model(), "bambu-x2d");
    assert_eq!(job.sha256().len(), 64);

    let summary = job.summary();
    assert_eq!(summary.file_name, "box.gcode.3mf");
    assert_eq!(summary.sha256, job.sha256());
    assert_eq!(summary.size_bytes, std::fs::metadata(&path).unwrap().len());
    assert_eq!(summary.plate, 1);
    assert_eq!(
        summary.estimated_seconds,
        Some(1200),
        "the file's claim, held to the floor"
    );
    assert!(
        summary.min_seconds >= 5 && summary.min_seconds < 60,
        "measured from the moves: {}",
        summary.min_seconds
    );
    assert_eq!(summary.layers, Some(3), "counted from the commands");
    assert_eq!(summary.max_nozzle_c, Some(220.0), "from M104/M109");
    assert_eq!(summary.max_bed_c, Some(35.0), "from M140/M190");
    assert_eq!(summary.max_chamber_c, None);
    assert_eq!(summary.plate_type.as_deref(), Some("Cool Plate"));
    assert_eq!(summary.stated_printer.as_deref(), Some("Bambu Lab X2D"));
    let extent = summary.extent.unwrap();
    assert_eq!((extent.min[0], extent.max[0]), (50.0, 110.0));
    assert_eq!(summary.filaments.len(), 1);
    let filament = &summary.filaments[0];
    assert_eq!(filament.material.as_deref(), Some("PLA"));
    assert_eq!(filament.grams, Some(0.03));
    assert_eq!(filament.metres, Some(0.01));
    assert!((filament.measured_metres * 1000.0 - GOOD_FILAMENT_MM).abs() < 1e-9);
    assert_eq!(
        filament.nozzle,
        Some(1),
        "slicer extruder 1 is device extruder 1"
    );
    assert_eq!(filament.nozzle_diameter_mm, Some(0.4));
    assert!(filament.selected_in_gcode);
    assert!(job.open_verified().is_ok());
}

#[test]
fn the_summary_follows_the_file_not_the_declaration() {
    let dir = tempfile::tempdir().unwrap();
    // Hotter and longer than anything a caller might claim.
    let gcode = GOOD_GCODE.replace("S220", "S229").replace("S35", "S44");
    let path = write_sliced(
        dir.path(),
        "hot.gcode.3mf",
        &gcode,
        &slice_info(1500, &[("PLA Matte", "#101010", 0.03)], "1"),
    );
    // The declaration names the family only; the label is free text.
    let job = check(&path, "tiny 5 minute print at 190 C", &pla_job()).unwrap();
    let summary = job.summary();
    assert_eq!(summary.max_nozzle_c, Some(229.0));
    assert_eq!(summary.max_bed_c, Some(44.0));
    assert_eq!(summary.estimated_seconds, Some(1500));
    // The material is shown by the profile's name for the family the file's word was
    // matched to, never by the file's own text.
    assert_eq!(summary.filaments[0].material.as_deref(), Some("PLA"));
    // The summary serialises for the dialog; it cannot be read back or built by hand.
    let json = serde_json::to_value(summary).unwrap();
    assert_eq!(json["max_nozzle_c"], 229.0);
    assert_eq!(json["filaments"][0]["colour"], "#101010FF");
    assert_eq!(json["plate_type"], "Cool Plate");
}

/// What the declaration names and the file's own account does not state is an error,
/// not an agreement.
#[test]
fn the_files_own_account_must_be_there_and_say_everything() {
    let dir = tempfile::tempdir().unwrap();
    // No slice info at all.
    let entries: Vec<_> = sliced_entries(GOOD_GCODE, &good_slice_info())
        .into_iter()
        .filter(|(name, _)| name != "Metadata/slice_info.config")
        .collect();
    let path = dir.path().join("bare.gcode.3mf");
    std::fs::write(&path, zip_bytes(&common::borrowed(&entries))).unwrap();
    unusable(check(&path, "bare", &pla_job()), "slice_info.config");

    // Slice info that is not what the slicer writes.
    let entity = "<?xml version=\"1.0\"?><!DOCTYPE c [<!ENTITY a \"aaaaaaaaaa\">]><config><plate><metadata key=\"index\" value=\"1\"/><metadata key=\"prediction\" value=\"&a;\"/></plate></config>";
    for info in [entity, "<config><plate>", "\u{0}\u{1}", "<config/>", ""] {
        let path = write_sliced(dir.path(), "odd.gcode.3mf", GOOD_GCODE, info);
        rejected(check(&path, "x", &pla_job()), "slice_info");
    }
    // Two plates, and the account of another plate.
    let good = good_slice_info();
    let second = good.replace(
        "</config>",
        "  <plate>\n    <metadata key=\"index\" value=\"1\"/>\n    <metadata key=\"prediction\" value=\"360000\"/>\n  </plate>\n</config>",
    );
    let path = write_sliced(dir.path(), "twice.gcode.3mf", GOOD_GCODE, &second);
    rejected(check(&path, "x", &pla_job()), "slice_info");
    let other_plate = good.replace("key=\"index\" value=\"1\"", "key=\"index\" value=\"2\"");
    let path = write_sliced(dir.path(), "other.gcode.3mf", GOOD_GCODE, &other_plate);
    rejected(check(&path, "x", &pla_job()), "slice_info");

    // Each statement left out, and each absurd number, which is dropped and then missing.
    for (changed, code) in [
        (
            good.replace("    <metadata key=\"prediction\" value=\"1200\"/>\n", ""),
            "time_mismatch",
        ),
        (
            good.replace("value=\"1200\"", "value=\"1e308\""),
            "time_mismatch",
        ),
        (
            good.replace("value=\"1200\"", "value=\"-5\""),
            "time_mismatch",
        ),
        (
            good.replace("    <metadata key=\"filament_maps\" value=\"1\"/>\n", ""),
            "filament_map_mismatch",
        ),
        (good.replace(" type=\"PLA\"", ""), "filament_mismatch"),
        (
            good.replace(" used_m=\"0.01\"", ""),
            "filament_amount_mismatch",
        ),
        (
            good.replace(" used_g=\"0.03\"", ""),
            "filament_amount_mismatch",
        ),
        (
            good.replace("used_g=\"0.03\"", "used_g=\"-5\""),
            "filament_amount_mismatch",
        ),
        (
            good.replace("used_m=\"0.01\"", "used_m=\"NaN\""),
            "filament_amount_mismatch",
        ),
    ] {
        assert_ne!(changed, good, "{code}: the fixture changed");
        let path = write_sliced(dir.path(), "less.gcode.3mf", GOOD_GCODE, &changed);
        rejected(check(&path, "x", &pla_job()), code);
    }
    // A filament the commands use and the account does not list.
    let unlisted = good.replace("<filament id=\"1\"", "<filament id=\"2\"");
    let path = write_sliced(dir.path(), "unlisted.gcode.3mf", GOOD_GCODE, &unlisted);
    rejected(check(&path, "x", &pla_job()), "slice_info");
}

/// The time and the amounts a person confirms are the file's claims, and the check holds
/// them to what the commands do.
#[test]
fn the_stated_time_and_amounts_are_held_to_what_the_commands_do() {
    let dir = tempfile::tempdir().unwrap();
    let good = check(&write_good(dir.path(), "good.gcode.3mf"), "x", &pla_job()).unwrap();
    let floor = good.summary().min_seconds;
    let stated = |seconds: u64| {
        let info = slice_info(seconds as u32, &[("PLA", "#FFFFFF", 0.03)], "1");
        check(
            &write_sliced(dir.path(), "time.gcode.3mf", GOOD_GCODE, &info),
            "x",
            &pla_job(),
        )
    };
    assert!(stated(floor + 2).is_ok());
    rejected(stated(floor - 2), "time_mismatch");
    rejected(stated(1), "time_mismatch");
    // Far more than the moves account for: four times their time and half an hour.
    assert!(stated(floor * 4 + 1790).is_ok());
    rejected(stated(floor * 4 + 1900), "time_mismatch");
    rejected(stated(86_400), "time_mismatch");

    // A print of nine hours that says two minutes. Each slow move takes a minute.
    let slow = "G1 X110 Y50 F60\nG1 X50 Y50 F60\n".repeat(270);
    let long = GOOD_GCODE.replace("G92 E0\n", &format!("{slow}G92 E0\n"));
    let path = write_sliced(
        dir.path(),
        "long.gcode.3mf",
        &long,
        &slice_info(120, &[("PLA", "#FFFFFF", 0.03)], "1"),
    );
    let error = check(&path, "x", &pla_job()).unwrap_err();
    assert!(error.to_string().contains("9 h 0 min"), "{error}");
    rejected(Err(error), "time_mismatch");
    let honest = slice_info(9 * 3600 + 600, &[("PLA", "#FFFFFF", 0.03)], "1");
    let job = check(
        &write_sliced(dir.path(), "long.gcode.3mf", &long, &honest),
        "x",
        &pla_job(),
    )
    .unwrap();
    assert!(job.summary().min_seconds >= 9 * 3600);
    assert!(
        job.summary()
            .review_text()
            .contains("Time: the file says 9 h 10 min. Its moves alone take at least 9 h 0 min")
    );

    // A print that pushes 5 m of filament and says 10 mm, and the reverse.
    let lines = "G1 X100 Y50 E2.5 F1800\nG1 X50 Y50 E2.5 F1800\n".repeat(1000);
    let heavy = GOOD_GCODE.replace("G92 E0\n", &format!("G1 Z0.6\n{lines}G92 E0\n"));
    let amount = |gcode: &str, grams: f64| {
        // The long print takes close to an hour at the speed it names.
        let seconds = if gcode.len() > GOOD_GCODE.len() {
            4000
        } else {
            1200
        };
        let info = slice_info(seconds, &[("PLA", "#FFFFFF", grams)], "1");
        check(
            &write_sliced(dir.path(), "amount.gcode.3mf", gcode, &info),
            "x",
            &pla_job(),
        )
    };
    rejected(amount(&heavy, 0.03), "filament_amount_mismatch");
    rejected(amount(GOOD_GCODE, 15.0), "filament_amount_mismatch");
    rejected(amount(GOOD_GCODE, 950.0), "filament_amount_mismatch");
    let job = amount(&heavy, 15.03).unwrap();
    let filament = &job.summary().filaments[0];
    assert_eq!(filament.metres, Some(5.01));
    assert!((filament.measured_metres - 5.01025).abs() < 1e-9);
    // The length right and the weight wrong: no plastic is that light or that heavy.
    let info = slice_info(4000, &[("PLA", "#FFFFFF", 15.03)], "1");
    for grams in ["used_g=\"1.00\"", "used_g=\"40.00\""] {
        let wrong = info.replace("used_g=\"15.03\"", grams);
        assert_ne!(wrong, info);
        let path = write_sliced(dir.path(), "weight.gcode.3mf", &heavy, &wrong);
        rejected(check(&path, "x", &pla_job()), "filament_amount_mismatch");
    }
    // The number of layers the G-code states against the heights it prints at.
    let layers = GOOD_GCODE.replace("total layer number: 3", "total layer number: 1");
    let path = write_sliced(dir.path(), "layers.gcode.3mf", &layers, &good_slice_info());
    rejected(check(&path, "x", &pla_job()), "layer_count_mismatch");
    // A second count further down that says something else, where a reader of the file
    // might take it from: the file cannot state two numbers.
    for second in ["; total layer number: 1\n", "; total layer number: many\n"] {
        let twice = format!("{GOOD_GCODE}{second}");
        let path = write_sliced(dir.path(), "twice.gcode.3mf", &twice, &good_slice_info());
        rejected(check(&path, "x", &pla_job()), "layer_count_mismatch");
    }
    let same = format!("{GOOD_GCODE}; total layer number: 3\n");
    let path = write_sliced(dir.path(), "same.gcode.3mf", &same, &good_slice_info());
    assert!(check(&path, "x", &pla_job()).is_ok());
}

/// With several filaments the amounts are counted per filament. Plastic laid while no
/// filament is selected would be counted for none, and the amounts the file states could
/// then be far from what the printer uses.
#[test]
fn plastic_is_counted_for_the_filament_that_is_selected() {
    let dir = tempfile::tempdir().unwrap();
    // Both nozzles hot by their ids, then 200 mm of filament before the first `T`.
    let unselected: String = (0..10)
        .map(|row| format!("G1 X50 Y{y} F9000\nG1 X250 Y{y} E20 F1800\n", y = 100 + row))
        .collect();
    let gcode = TWO_GCODE.replace(
        "T0\nM109 S220\nG1 Z0.2 F600\n",
        &format!("M104 S220 T1\nM104 S250 T0\nG1 Z0.2 F600\n{unselected}T0\nG1 Z0.2 F600\n"),
    );
    assert_ne!(gcode, TWO_GCODE);
    let path = write_sliced(
        dir.path(),
        "unselected.gcode.3mf",
        &gcode,
        &two_slice_info(),
    );
    rejected(check(&path, "x", &two_nozzle_job()), "unassigned_extrusion");
}

#[test]
fn a_two_nozzle_file_maps_filaments_by_device_extruder_id() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_two(dir.path(), "two.gcode.3mf");
    let job = check(&path, "two", &two_nozzle_job()).unwrap();
    let nozzles: Vec<_> = job.summary().filaments.iter().map(|f| f.nozzle).collect();
    assert_eq!(
        nozzles,
        [Some(1), Some(0)],
        "slicer extruder 2 is device extruder 0"
    );
    assert_eq!(job.summary().max_nozzle_c, Some(250.0));
    assert_eq!(job.summary().max_bed_c, Some(70.0));
    assert_eq!(
        job.summary().plate_type.as_deref(),
        Some("Textured PEI Plate")
    );
    // Each filament's length is measured on its own.
    let measured: Vec<_> = job
        .summary()
        .filaments
        .iter()
        .map(|f| (f.measured_metres * 1000.0 * 10.0).round() / 10.0)
        .collect();
    assert_eq!(measured, [2.5, 3.0]);

    // The same file against a declaration that swaps the nozzles.
    let mut swapped = two_nozzle_job();
    swapped.filaments[0].nozzle = 0;
    swapped.filaments[1].nozzle = 1;
    rejected(check(&path, "two", &swapped), "filament_map_mismatch");

    // A file sliced with both filaments on the main nozzle; a map that is too short;
    // one naming an extruder the printer does not have.
    for maps in ["1 1", "1", "1 3"] {
        let info = two_slice_info().replace("value=\"1 2\"", &format!("value=\"{maps}\""));
        let path = write_sliced(dir.path(), "maps.gcode.3mf", common::TWO_GCODE, &info);
        rejected(
            check(&path, "two", &two_nozzle_job()),
            "filament_map_mismatch",
        );
    }
}

#[test]
fn a_file_that_disagrees_with_the_job_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let with_info = |info: &str| write_sliced(dir.path(), "info.gcode.3mf", GOOD_GCODE, info);
    let with_gcode =
        |gcode: &str| write_sliced(dir.path(), "gcode.gcode.3mf", gcode, &good_slice_info());
    let good = good_slice_info();

    // Sliced for PETG, declared as PLA.
    let path = with_info(&good.replace("type=\"PLA\"", "type=\"PETG\""));
    rejected(check(&path, "x", &pla_job()), "filament_mismatch");
    // A material is its family's name or a listed alias, never a word that starts like
    // one: PCTG is not PC, whoever declares what.
    let pctg = with_info(&good.replace("type=\"PLA\"", "type=\"PCTG\""));
    rejected(check(&pctg, "x", &pla_job()), "filament_mismatch");
    let pc = job_on(TEXTURED_PLATE, vec![filament(1, "PC", 1)]);
    let for_textured = GOOD_GCODE.replace("= Cool Plate", "= Textured PEI Plate");
    let pctg = write_sliced(
        dir.path(),
        "pctg.gcode.3mf",
        &for_textured,
        &good.replace("type=\"PLA\"", "type=\"PCTG\""),
    );
    rejected(check(&pctg, "x", &pc), "filament_mismatch");
    let as_pc = write_sliced(
        dir.path(),
        "pc.gcode.3mf",
        &for_textured,
        &good.replace("type=\"PLA\"", "type=\"PC\""),
    );
    assert!(check(&as_pc, "x", &pc).is_ok());

    // The file uses a second filament the job does not declare.
    let path = with_info(&slice_info(
        1200,
        &[("PLA", "#FFFFFF", 0.03), ("PLA", "#000000", 2.0)],
        "1 1",
    ));
    rejected(check(&path, "x", &pla_job()), "undeclared_filament");
    // G-code the linter refuses never becomes a job.
    let path = with_gcode(&format!("{GOOD_GCODE}M500\n"));
    let error = check(&path, "x", &pla_job()).unwrap_err();
    assert!(error.to_string().contains("1 error"), "{error}");
    rejected(Err(error), "denied_command");
    // The declared plate is not the file's plate.
    let plate_two = DeclaredJob {
        plate: 2,
        ..pla_job()
    };
    let path = write_good(dir.path(), "one.gcode.3mf");
    assert!(matches!(
        check(&path, "x", &plate_two),
        Err(JobError::Unusable(_))
    ));

    // Another printer, or none named; another build plate; another nozzle size.
    let path = with_gcode(&GOOD_GCODE.replace("Bambu Lab X2D", "Bambu Lab H2D"));
    rejected(check(&path, "x", &pla_job()), "wrong_printer");
    let path = with_gcode(&GOOD_GCODE.replace("; printer_model = Bambu Lab X2D\n", ""));
    rejected(check(&path, "x", &pla_job()), "wrong_printer");
    let path = with_gcode(&format!("{GOOD_GCODE}; printer_model = Bambu Lab H2D\n"));
    rejected(check(&path, "x", &pla_job()), "wrong_printer");
    let good_path = write_good(dir.path(), "good.gcode.3mf");
    let textured = job_on(TEXTURED_PLATE, vec![filament(1, "PLA", 1)]);
    rejected(check(&good_path, "x", &textured), "plate_mismatch");
    let path = with_gcode(&GOOD_GCODE.replace("= 0.4,0.4", "= 0.8,0.8"));
    rejected(check(&path, "x", &pla_job()), "nozzle_mismatch");
    let mut wide = pla_job();
    wide.filaments[0].nozzle_diameter_mm = 0.8;
    rejected(check(&path, "x", &wide), "invalid_declaration");

    // The same facts in the file's own account and in its other metadata.
    let path = with_info(&good.replace("type=\"PLA\"", "type=\"PLA\" nozzle_diameter=\"0.80\""));
    rejected(check(&path, "x", &pla_job()), "nozzle_mismatch");
    let path = with_info(&good.replace(
        "    <metadata key=\"prediction\"",
        "    <metadata key=\"nozzle_diameters\" value=\"0.8,0.4\"/>\n    <metadata key=\"prediction\"",
    ));
    rejected(check(&path, "x", &pla_job()), "nozzle_mismatch");
    let path = with_info(&good.replace(
        "    <metadata key=\"prediction\"",
        "    <metadata key=\"printer_model_id\" value=\"C11\"/>\n    <metadata key=\"prediction\"",
    ));
    rejected(check(&path, "x", &pla_job()), "wrong_printer");
    let settings = |json: &str| {
        write_with(
            dir.path(),
            "settings.gcode.3mf",
            &[("Metadata/project_settings.config", json.as_bytes())],
        )
    };
    let agrees = r#"{"printer_model":"Bambu Lab X2D","printer_settings_id":"Bambu Lab X2D 0.4 nozzle","curr_bed_type":"Cool Plate","nozzle_diameter":["0.4","0.4"]}"#;
    assert!(check(&settings(agrees), "x", &pla_job()).is_ok());
    for (changed, code) in [
        (
            agrees.replace("\"Bambu Lab X2D\"", "\"Bambu Lab P1S\""),
            "wrong_printer",
        ),
        (agrees.replace("0.4 nozzle", "0.8 nozzle"), "wrong_printer"),
        (
            agrees.replace("Cool Plate", "Textured PEI Plate"),
            "plate_mismatch",
        ),
        (
            agrees.replace("[\"0.4\",\"0.4\"]", "[\"0.8\",\"0.4\"]"),
            "nozzle_mismatch",
        ),
        (
            agrees.replace("[\"0.4\",\"0.4\"]", "\"0.4\""),
            "nozzle_mismatch",
        ),
        ("{not json".to_owned(), "slice_info"),
    ] {
        rejected(check(&settings(&changed), "x", &pla_job()), code);
    }
    let plate_json = |json: &str| {
        write_with(
            dir.path(),
            "plate.gcode.3mf",
            &[("Metadata/plate_1.json", json.as_bytes())],
        )
    };
    assert!(
        check(
            &plate_json(r#"{"bed_type":"cool_plate","nozzle_diameter":0.4000000059604645}"#),
            "x",
            &pla_job()
        )
        .is_ok()
    );
    rejected(
        check(
            &plate_json(r#"{"bed_type":"textured_plate"}"#),
            "x",
            &pla_job(),
        ),
        "plate_mismatch",
    );
    rejected(
        check(&plate_json(r#"{"bed_type":7}"#), "x", &pla_job()),
        "plate_mismatch",
    );
    rejected(
        check(&plate_json(r#"{"nozzle_diameter":0.8}"#), "x", &pla_job()),
        "nozzle_mismatch",
    );
    // A plate whose word in this entry was never seen: any word but a known plate's.
    let textured_file = |json: &str| {
        let mut entries = sliced_entries(&for_textured, &good);
        entries.push(("Metadata/plate_1.json".to_owned(), json.as_bytes().to_vec()));
        let path = dir.path().join("textured.gcode.3mf");
        std::fs::write(&path, zip_bytes(&common::borrowed(&entries))).unwrap();
        path
    };
    assert!(
        check(
            &textured_file(r#"{"bed_type":"textured_plate"}"#),
            "x",
            &textured
        )
        .is_ok()
    );
    rejected(
        check(
            &textured_file(r#"{"bed_type":"cool_plate"}"#),
            "x",
            &textured,
        ),
        "plate_mismatch",
    );
}

#[test]
fn unusable_declarations_and_names_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_good(dir.path(), "box.gcode.3mf");
    for name in ["", "   ", "../..", "\u{202e}\u{1b}"] {
        assert!(
            matches!(check(&path, name, &pla_job()), Err(JobError::Unusable(_))),
            "{name:?}"
        );
    }
    // Control and bidirectional characters never reach the printer or the dialog.
    let job = check(&path, "\u{202e}\u{1b}[0mcup", &pla_job()).unwrap();
    assert!(
        job.name().is_ascii() && !job.name().contains('\u{1b}'),
        "{:?}",
        job.name()
    );
    let plate_zero = DeclaredJob {
        plate: 0,
        ..pla_job()
    };
    assert!(matches!(
        check(&path, "x", &plate_zero),
        Err(JobError::Unusable(_))
    ));
    let unknown = job_on(COOL_PLATE, vec![filament(1, "Unobtainium", 1)]);
    rejected(check(&path, "x", &unknown), "unknown_filament");
    let no_nozzle = job_on(COOL_PLATE, vec![filament(1, "PLA", 7)]);
    rejected(check(&path, "x", &no_nozzle), "invalid_declaration");
    let no_plate = job_on("", vec![filament(1, "PLA", 1)]);
    rejected(check(&path, "x", &no_plate), "invalid_declaration");
    // A path-like label cannot leave the folder it is later placed in.
    let job = check(&path, "../../etc/passwd", &pla_job()).unwrap();
    assert!(!job.remote_name().contains('/') && !job.remote_name().starts_with('.'));
}

/// Nothing travels to a printer unread: the archive holds the declared plate's G-code,
/// its checksum and the slicer's metadata, and nothing else.
#[test]
fn only_the_declared_plate_and_the_slicers_own_entries_are_in_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let hostile = b"G90\nM83\nM104 S300\nG1 Z-5 F600\n";
    let with = |name: &str, content: &[u8]| {
        check(
            &write_with(dir.path(), "extra.gcode.3mf", &[(name, content)]),
            "x",
            &pla_job(),
        )
    };
    // A second plate that nobody read, under any spelling.
    for name in [
        "Metadata/plate_2.gcode",
        "Metadata/plate_02.gcode",
        "Metadata/plate_1.gcode.bak",
        "Metadata/plate_1.GCODE.old",
        "plate_1.gcode",
        "Metadata/sub/plate_1.gcode",
        "Metadata/notes.txt",
        "Metadata/plate_2.json",
        "run.sh",
    ] {
        unusable(
            with(name, hostile),
            "not an entry of a file sliced for plate 1",
        );
    }
    // What the slicer writes beside the G-code is known by name and passes.
    let job = check(
        &write_with(
            dir.path(),
            "full.gcode.3mf",
            &[
                ("_rels/.rels", b"<Relationships/>"),
                ("3D/3dmodel.model", b"<model/>"),
                ("3D/Objects/object_1.model", b"<model/>"),
                ("Metadata/plate_1.png", b"png"),
                ("Metadata/plate_1_small.png", b"png"),
                ("Metadata/top_1.png", b"png"),
                ("Metadata/pick_1.png", b"png"),
                ("Metadata/plate_no_light_1.png", b"png"),
                ("Metadata/model_settings.config", b"<config/>"),
                ("Metadata/cut_information.xml", b"<objects/>"),
                ("Metadata/filament_sequence.json", b"{}"),
            ],
        ),
        "x",
        &pla_job(),
    )
    .unwrap();
    assert_eq!(job.summary().layers, Some(3));
    // The file checked for plate 2 has plate 2's entries, not plate 1's.
    let plate_two = DeclaredJob {
        plate: 2,
        ..pla_job()
    };
    unusable(
        check(&write_good(dir.path(), "one.gcode.3mf"), "x", &plate_two),
        "plate 2",
    );

    // The archive's own checksum of the G-code: present, an MD5, and the G-code's.
    let no_md5: Vec<_> = sliced_entries(GOOD_GCODE, &good_slice_info())
        .into_iter()
        .filter(|(name, _)| !name.ends_with(".md5"))
        .collect();
    let path = dir.path().join("nomd5.gcode.3mf");
    std::fs::write(&path, zip_bytes(&common::borrowed(&no_md5))).unwrap();
    unusable(check(&path, "x", &pla_job()), "plate_1.gcode.md5");
    let of_other = md5_upper(hostile);
    rejected(
        with("Metadata/plate_1.gcode.md5", of_other.as_bytes()),
        "gcode_md5_mismatch",
    );
    for not_md5 in ["", "abc", &"g".repeat(32), &"0".repeat(64)] {
        rejected(
            with("Metadata/plate_1.gcode.md5", not_md5.as_bytes()),
            "gcode_md5_mismatch",
        );
    }
    let lower = md5_upper(GOOD_GCODE.as_bytes()).to_lowercase();
    assert!(with("Metadata/plate_1.gcode.md5", lower.as_bytes()).is_ok());
    assert_eq!(
        md5_upper(b"abc"),
        "900150983CD24FB0D6963F7D28E17F72",
        "the helper is an MD5"
    );
}

/// An archive is accepted only in the one layout in which every zip reader finds the
/// same entries: what the directory lists is all there is, from the first byte on.
#[test]
fn archives_another_reader_would_read_differently_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let plain = RawLayout::default();
    let checked = |entries: &[RawEntry], layout: &RawLayout| {
        check(
            &write_raw(dir.path(), "raw.gcode.3mf", entries, layout),
            "x",
            &pla_job(),
        )
    };
    let hostile = b"G90\nM83\nM104 S300\nG1 Z-5 F600\n";
    let gcode_at = 1;
    assert_eq!(raw_good()[gcode_at].name, b"Metadata/plate_1.gcode");

    // The layouts that are fine: stored entries, sizes behind the data as the slicer
    // writes them, and a zip64 end record.
    let job = checked(&raw_good(), &plain).unwrap();
    assert_eq!(job.summary().max_nozzle_c, Some(220.0));
    let mut described = raw_good();
    for entry in &mut described {
        entry.descriptor = true;
    }
    assert!(checked(&described, &plain).is_ok());
    let zip64 = RawLayout {
        zip64: true,
        ..RawLayout::default()
    };
    assert!(checked(&raw_good(), &zip64).is_ok());

    // Names that differ in case or look alike, and a folder named like the G-code.
    for name in [
        "metadata/plate_1.gcode",
        "Metadata/PLATE_1.GCODE",
        "Metadata/plate_1.gcode ",
        "Metadata/plate_1.gcode.",
        "Metadata/plate_\u{ff11}.gcode",
        "Metadata/plate_1.gcode/x",
        "Metadata\\plate_1.gcode",
        "Metadata/../Metadata/plate_1.gcode",
    ] {
        let mut entries = raw_good();
        entries.push(RawEntry::new(name, hostile));
        let result = checked(&entries, &plain);
        assert!(
            matches!(result, Err(JobError::Archive(_) | JobError::Unusable(_))),
            "{name:?}: {result:?}"
        );
    }
    let mut entries = raw_good();
    entries.push(RawEntry::new("metadata/plate_1.gcode", hostile));
    unsafe_archive(checked(&entries, &plain), "upper and lower case");
    let mut entries = raw_good();
    entries.push(RawEntry::new("Metadata/plate_1.gcode ", hostile));
    unsafe_archive(checked(&entries, &plain), "unsafe name");
    // The same name twice.
    let mut entries = raw_good();
    entries.push(RawEntry::new("Metadata/plate_1.gcode", hostile));
    unsafe_archive(checked(&entries, &plain), "same name");

    // A local header that names another entry than the directory does, either way.
    let mut entries = raw_good();
    let mut disguised = RawEntry::new("Metadata/model_settings.config", hostile);
    disguised.local_name = b"Metadata/plate_1.gcode".to_vec();
    entries.push(disguised);
    unsafe_archive(
        checked(&entries, &plain),
        "does not say what the archive's directory says",
    );
    let mut entries = raw_good();
    entries[gcode_at].local_name = b"Metadata/zzzzz_9.gcode".to_vec();
    unsafe_archive(
        checked(&entries, &plain),
        "does not say what the archive's directory says",
    );
    // A local header that states another size or checksum.
    let mut entries = raw_good();
    entries[gcode_at].local_size = 7;
    unsafe_archive(
        checked(&entries, &plain),
        "does not say what the archive's directory says",
    );
    let mut entries = raw_good();
    entries[gcode_at].local_crc = 0x1234_5678;
    unsafe_archive(
        checked(&entries, &plain),
        "does not say what the archive's directory says",
    );
    let mut entries = raw_good();
    for entry in &mut entries {
        entry.descriptor = true;
    }
    entries[gcode_at].local_crc = 0x1234_5678;
    unsafe_archive(checked(&entries, &plain), "data descriptor");

    // An entry the directory does not list: in front, and between the listed ones.
    for at in [0, 2] {
        let mut entries = raw_good();
        let mut unlisted = RawEntry::new("Metadata/plate_1.gcode", hostile);
        unlisted.listed = false;
        entries.insert(at, unlisted);
        unsafe_archive(
            checked(&entries, &plain),
            "belong to no entry of its directory",
        );
    }
    // Bytes in front of the first entry, a comment, trailing bytes.
    let prefixed = RawLayout {
        prefix: b"#!/bin/sh\necho prefix\n".to_vec(),
        ..RawLayout::default()
    };
    assert!(matches!(
        checked(&raw_good(), &prefixed),
        Err(JobError::Archive(_))
    ));
    let commented = RawLayout {
        comment: b"G1 Z-5 ; anything can be written here".to_vec(),
        ..RawLayout::default()
    };
    unsafe_archive(checked(&raw_good(), &commented), "comment");
    let mut entries = raw_good();
    entries[gcode_at].comment = b"M104 S300".to_vec();
    unsafe_archive(checked(&entries, &plain), "comment");
    let trailing = RawLayout {
        trailing: b"extra bytes".to_vec(),
        ..RawLayout::default()
    };
    unsafe_archive(checked(&raw_good(), &trailing), "does not end the file");

    // A declared size that is not the size of what is stored, larger or smaller, with
    // the checksum of the real data.
    for declared in [GOOD_GCODE.len() as u32 + 100_000, 120] {
        let mut entries = raw_good();
        entries[gcode_at].size = declared;
        entries[gcode_at].local_size = declared;
        let result = checked(&entries, &plain);
        assert!(matches!(result, Err(JobError::Archive(_))), "{result:?}");
    }
    // An encrypted entry, and a symbolic link.
    let mut entries = raw_good();
    entries[gcode_at].flags = 0x0001;
    assert!(matches!(
        checked(&entries, &plain),
        Err(JobError::Archive(_))
    ));
    let mut entries = raw_good();
    let mut link = RawEntry::new("Metadata/model_settings.config", b"/etc/passwd");
    link.made_by = (3 << 8) | 20;
    link.external = 0o120_777 << 16;
    entries.push(link);
    assert!(matches!(
        checked(&entries, &plain),
        Err(JobError::Archive(_))
    ));
}

#[test]
fn hostile_files_are_refused_before_anything_is_parsed() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.gcode.3mf");
    assert!(matches!(
        check(&missing, "x", &pla_job()),
        Err(JobError::Unusable(_))
    ));

    let empty = dir.path().join("empty.gcode.3mf");
    std::fs::write(&empty, b"").unwrap();
    assert!(matches!(
        check(&empty, "x", &pla_job()),
        Err(JobError::Unusable(_))
    ));

    let garbage = dir.path().join("garbage.gcode.3mf");
    std::fs::write(&garbage, b"G1 X10\nthis is not a zip\n").unwrap();
    assert!(matches!(
        check(&garbage, "x", &pla_job()),
        Err(JobError::Archive(_))
    ));

    unusable(check(dir.path(), "x", &pla_job()), "not a regular file");

    let good = write_good(dir.path(), "good.gcode.3mf");
    #[cfg(unix)]
    {
        let link = dir.path().join("link.gcode.3mf");
        std::os::unix::fs::symlink(&good, &link).unwrap();
        let error = check(&link, "x", &pla_job()).unwrap_err();
        assert!(error.to_string().contains("symbolic link"), "{error}");
        // Only the file itself must not be a link. A folder on the way to it may be one
        // (a home folder or a temporary folder often is); the hash is of the bytes read.
        let folder = tempfile::tempdir().unwrap();
        let through = folder.path().join("linked-folder");
        std::os::unix::fs::symlink(dir.path(), &through).unwrap();
        let job = check(&through.join("good.gcode.3mf"), "x", &pla_job()).unwrap();
        assert_eq!(
            job.sha256(),
            check(&good, "x", &pla_job()).unwrap().sha256(),
            "the same bytes, whichever way the file is reached"
        );

        let hard = dir.path().join("hard.gcode.3mf");
        std::fs::hard_link(&good, &hard).unwrap();
        let error = check(&good, "x", &pla_job()).unwrap_err();
        assert!(error.to_string().contains("hard links"), "{error}");
        std::fs::remove_file(&hard).unwrap();

        // A named pipe where the file should be: refused at once. Opening it for
        // reading would wait for a writer for ever.
        let pipe = dir.path().join("pipe.gcode.3mf");
        let made = std::process::Command::new("mkfifo")
            .arg(&pipe)
            .status()
            .unwrap();
        assert!(made.success());
        let (tx, rx) = std::sync::mpsc::channel();
        let checked = pipe.clone();
        std::thread::spawn(move || {
            let outcome = check(&checked, "pipe", &pla_job()).map(|_| ());
            let _ = tx.send(outcome.map_err(|error| error.to_string()));
        });
        let outcome = rx.recv_timeout(std::time::Duration::from_secs(10));
        // Lets a check that did block end, so that the test process can.
        let _ = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&pipe);
        // The check returned instead of waiting on the pipe.
        let Ok(Err(error)) = outcome else {
            panic!("{outcome:?}");
        };
        assert!(error.contains("not a regular file"), "{error}");
    }
    assert!(check(&good, "x", &pla_job()).is_ok());

    // Limits come from the caller and hold.
    let tight = |limits: ArchiveLimits| check_job(&good, "x", &pla_job(), &x2d(), &limits);
    assert!(matches!(
        tight(ArchiveLimits {
            max_file_bytes: 64,
            ..ArchiveLimits::default()
        }),
        Err(JobError::Unusable(_))
    ));
    assert!(matches!(
        tight(ArchiveLimits {
            max_entries: 2,
            ..ArchiveLimits::default()
        }),
        Err(JobError::Archive(_))
    ));
    assert!(
        tight(ArchiveLimits {
            max_gcode_bytes: 16,
            ..ArchiveLimits::default()
        })
        .is_err()
    );
    // The default limits are a small multiple of a real slice (about 1 MiB of G-code in
    // a file of 1.2 MiB), not thousands of times it.
    let limits = ArchiveLimits::default();
    const MIB: u64 = 1024 * 1024;
    assert!(limits.max_gcode_bytes <= 128 * MIB);
    assert!(limits.max_entry_bytes <= 128 * MIB);
    assert!(limits.max_file_bytes <= 128 * MIB);
    assert!(limits.max_total_bytes <= 512 * MIB);
    assert!(limits.max_entries <= 256);

    // Entry names that would leave a folder, and a zip bomb of G-code.
    let escaping = dir.path().join("escape.gcode.3mf");
    std::fs::write(
        &escaping,
        zip_bytes(&[("../Metadata/plate_1.gcode", GOOD_GCODE.as_bytes())]),
    )
    .unwrap();
    assert!(matches!(
        check(&escaping, "x", &pla_job()),
        Err(JobError::Archive(_))
    ));
    let bomb = dir.path().join("bomb.gcode.3mf");
    let filler = "G1 X50 Y50\n".repeat(400_000);
    std::fs::write(
        &bomb,
        zip_bytes(&[("Metadata/plate_1.gcode", filler.as_bytes())]),
    )
    .unwrap();
    assert!(matches!(
        check(&bomb, "x", &pla_job()),
        Err(JobError::Archive(_))
    ));
}

#[test]
fn a_file_changed_after_the_check_is_noticed() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_good(dir.path(), "box.gcode.3mf");
    let job = check(&path, "box", &pla_job()).unwrap();
    let other = write_sliced(
        dir.path(),
        "other.gcode.3mf",
        &GOOD_GCODE.replace("S220", "S225"),
        &good_slice_info(),
    );
    std::fs::copy(&other, &path).unwrap();
    assert!(matches!(job.open_verified(), Err(JobError::Changed)));
    // Put back, the file is the checked one again; removed, it is not.
    std::fs::write(&path, common::sliced_bytes(GOOD_GCODE, &good_slice_info())).unwrap();
    assert!(job.open_verified().is_ok());
    std::fs::remove_file(&path).unwrap();
    assert!(matches!(job.open_verified(), Err(JobError::Changed)));
}

/// The file is read once. Whoever can write it while it is checked gets either a refusal
/// or a job of exactly the bytes that were checked: never the hash of one content with
/// the verdict and the text of another.
#[cfg(target_os = "linux")]
#[test]
fn the_hash_the_verdict_and_the_text_are_of_the_same_bytes() {
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn sha256(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
    /// Positions of this process's open handles on `path`.
    fn positions(path: &std::path::Path) -> Vec<u64> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir("/proc/self/fd").unwrap().flatten() {
            if std::fs::read_link(entry.path()).ok().as_deref() == Some(path) {
                let info = std::fs::read_to_string(format!(
                    "/proc/self/fdinfo/{}",
                    entry.file_name().to_string_lossy()
                ))
                .unwrap_or_default();
                if let Some(position) = info.lines().find_map(|line| line.strip_prefix("pos:")) {
                    found.push(position.trim().parse().unwrap_or(0));
                }
            }
        }
        found
    }

    // Two files of the same length: one sets the nozzle to 300 C, one to 220 C. Stored
    // entries, so that they differ in a few bytes at the same places.
    let hot = GOOD_GCODE.replace("S220", "S300");
    let as_raw = |gcode: &str| -> Vec<u8> {
        let entries: Vec<RawEntry> = sliced_entries(gcode, &good_slice_info())
            .iter()
            .map(|(name, content)| RawEntry::new(name, content))
            .collect();
        common::raw_zip(&entries, &RawLayout::default())
    };
    let (hostile, harmless) = (as_raw(&hot), as_raw(GOOD_GCODE));
    assert_eq!(hostile.len(), harmless.len());
    let differing: Vec<usize> = (0..hostile.len())
        .filter(|&at| hostile[at] != harmless[at])
        .collect();
    let (first, last) = (differing[0], *differing.last().unwrap());

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("job.gcode.3mf");
    for round in 0..40 {
        std::fs::write(&path, &hostile).unwrap();
        // Alone, the hostile file is refused.
        if round == 0 {
            rejected(check(&path, "job", &pla_job()), "temperature_over_limit");
        }
        let done = Arc::new(AtomicBool::new(false));
        let attacker = {
            let (path, done) = (path.clone(), Arc::clone(&done));
            let harmless_part = harmless[first..=last].to_vec();
            std::thread::spawn(move || {
                // In even rounds: as soon as a handle of the check has read past the
                // part that differs (or the check is over), swap the harmless bytes in.
                // In odd rounds: at once, racing the check's one read.
                while round % 2 == 0
                    && !done.load(Ordering::SeqCst)
                    && !positions(&path).iter().any(|&at| at > last as u64)
                {
                    std::thread::yield_now();
                }
                let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
                file.seek(SeekFrom::Start(first as u64)).unwrap();
                file.write_all(&harmless_part).unwrap();
            })
        };
        let result = check(&path, "job", &pla_job());
        done.store(true, Ordering::SeqCst);
        attacker.join().unwrap();
        // The attacker puts the hostile bytes back.
        let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(SeekFrom::Start(first as u64)).unwrap();
        file.write_all(&hostile[first..=last]).unwrap();
        drop(file);

        match result {
            // The check read the hostile bytes: refused.
            Err(JobError::Rejected(_) | JobError::Archive(_)) => {}
            Err(other) => panic!("{other:?}"),
            // The check read the harmless bytes: the job is of those, by its hash, its
            // text and what it would send; and the file on disk is no longer its file.
            Ok(job) => {
                assert_eq!(job.sha256(), sha256(&harmless), "round {round}");
                assert_ne!(job.sha256(), sha256(&hostile));
                assert_eq!(job.summary().max_nozzle_c, Some(220.0));
                assert!(matches!(job.open_verified(), Err(JobError::Changed)));
                // With the checked content on disk again, what is sent is that content.
                std::fs::write(&path, &harmless).unwrap();
                let mut sent = Vec::new();
                job.open_verified().unwrap().read_to_end(&mut sent).unwrap();
                assert_eq!(sha256(&sent), job.sha256());
            }
        }
    }
}

/// The bytes a job sends are its own copy of what was checked: a file changed on disk
/// can make a job refuse, never make it send something else.
#[test]
fn a_job_sends_the_bytes_that_were_checked_and_no_others() {
    use std::io::Read;
    let dir = tempfile::tempdir().unwrap();
    let path = write_good(dir.path(), "box.gcode.3mf");
    let original = std::fs::read(&path).unwrap();
    let job = check(&path, "box", &pla_job()).unwrap();
    let mut reader = job.open_verified().unwrap();
    // The file is replaced while a delivery holds its reader.
    let hot = common::sliced_bytes(&GOOD_GCODE.replace("S220", "S300"), &good_slice_info());
    std::fs::write(&path, &hot).unwrap();
    let mut sent = Vec::new();
    reader.read_to_end(&mut sent).unwrap();
    assert_eq!(sent, original);
    assert!(matches!(job.open_verified(), Err(JobError::Changed)));
}

/// What a person confirms for a file in the shape Bambu Studio writes: time, filament,
/// nozzle, temperatures, plate, printed area, and every accepted command that changes
/// limits or stored configuration.
#[test]
fn the_review_text_states_what_the_file_does_and_lists_its_configuration_commands() {
    let dir = tempfile::tempdir().unwrap();
    let path = common::write_stock(dir.path(), "bracket.gcode.3mf");
    // The label is the application's and is not what a person is asked to confirm.
    let job = check(
        &path,
        "quick test at 150 C, no settings changed",
        &pla_job(),
    )
    .unwrap();
    let summary = job.summary();
    assert_eq!(summary.estimated_seconds, Some(900));
    assert_eq!(summary.printer_name, "Bambu Lab X2D");
    assert_eq!(summary.stated_printer.as_deref(), Some("Bambu Lab X2D"));
    assert_eq!(summary.plate_type.as_deref(), Some("Cool Plate"));
    assert_eq!(summary.max_nozzle_reference_c, Some(235.0));
    assert_eq!(summary.unplaced_moves, 2);
    assert_eq!(summary.configuration.len(), 13);
    let filament = &summary.filaments[0];
    assert_eq!(filament.nozzle_role.as_deref(), Some("main"));
    assert_eq!(filament.nozzle_diameter_mm, Some(0.4));

    let expected = format!(
        "\
Print file \"bracket.gcode.3mf\", plate 1, checked for the Bambu Lab X2D.
The file says it was sliced for: Bambu Lab X2D.
Time: the file says 15 min 0 s. Its moves alone take at least {floor} s at the speeds they name; heating, the printer's own routines and acceleration come on top.
Layers: 2, counted from the commands.
Filament 1: PLA on the main nozzle (id 1), 0.4 mm nozzle. The commands push 0 m of it. The file says: 0.01 m, 0.02 g, colour #00AE42.
Nozzle temperature: up to 220 C. A setting of the printer's own routine names 235 C and may heat the nozzle that far; it is within the filament's limit.
Bed temperature: up to 35 C. Build plate: Cool Plate, as chosen for this job; the file was sliced for it.
Chamber heating: off.
When the file ends, every heater has been switched off.
Printed area: X 60 to 110 mm, Y 60 to 100 mm, up to 0.4 mm high.
The printer's own start and end routines in this file change settings of the printer. Each command below is part of the slicer's stock routine for this printer and is accepted only in exactly this form:
- The printer's own travel limit check is switched. While it is off, Hotend's check of every move in this file is the only thing that keeps the nozzle inside the printer.
    M211 X0 Y0 Z0, once in the start routine: switches the printer's own travel limit check off for X, Y and Z.
    M211 Z1, once in the end routine: switches the printer's own travel limit check on again for Z.
- Settings may be stored in the printer.
    M500 D1, once in the start routine: unknown on this printer. In common firmware M500 stores the current settings; D is not known.
- Speed and acceleration limits are set for this print.
    M201 X18000 Y18000 Z400 E4000, once in the start routine: highest acceleration per axis for this print (the values of the machine profile are the upper limit).
    M203 X450 Y450 Z18 E25, once in the start routine: highest speed per axis for this print (the values of the machine profile are the upper limit).
    M204 P9000 R4000 T9000, once in the start routine: acceleration for printing, retracting and travel (the values of the machine profile are the upper limit).
    M205 X8 Y8 Z2 E2, once in the start routine: jerk limits for this print (the values of the machine profile are the upper limit).
    M201.2 K1, once in the end routine: puts the acceleration scale back to 1.
- The motor current is changed and put back.
    M17 D, once in the start routine: puts the motor current back to its default.
    M17 Z0.4, once in the end routine: lowers the Z motor current before the bed moves down.
    M17 S, once in the end routine: unknown (stands directly before the Z motor current is lowered).
    M17 R, once in the end routine: restores the Z motor current.
- A small Z trim is set for this print.
    G29.1 Z0.022, once in the start routine: sets a small Z trim for the print (cleared first, then set for the plate).
2 short move(s) in the start routine start from a position only the printer knows; they go no further than the stock routine's, and none is written to end closer to the bed than where the printer left the nozzle.
Pictures, names, times and amounts that the printer's screen or Bambu's own programs show for this job are read by them from the file; Hotend did not check those.
SHA-256 of the file: {}.",
        job.sha256(),
        floor = summary.min_seconds,
    );
    assert_eq!(summary.review_text(), expected);
    assert_eq!(summary.review_lines().join("\n"), expected);
    // The same list in one sentence, and as the one warning of the check.
    let sentence = summary.configuration_sentence().unwrap();
    assert!(sentence.starts_with("the printer's own travel limit check (M211 X0 Y0 Z0 once, M211 Z1 once); stored settings (M500 D1 once);"));
    let warnings: Vec<_> = job
        .lint()
        .findings
        .iter()
        .map(|finding| finding.code)
        .collect();
    assert_eq!(warnings, ["configuration_commands"]);
    assert!(job.lint().findings[0].message.contains(&sentence));
    // The confirmation request shows the same text and is bound to its hash.
    let request = hotend_printer::ConfirmationRequest::hand_off(&job);
    assert_eq!(request.review_text(), expected);
    assert_eq!(request.review_sha256().len(), 64);
}

#[test]
fn the_review_text_of_a_plain_file_says_that_nothing_is_configured() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_good(dir.path(), "box.gcode.3mf");
    let job = check(&path, "box", &pla_job()).unwrap();
    let summary = job.summary();
    assert!(summary.configuration.is_empty());
    assert_eq!(summary.configuration_sentence(), None);
    let text = summary.review_text();
    for line in [
        "Print file \"box.gcode.3mf\", plate 1, checked for the Bambu Lab X2D.",
        "Layers: 3, counted from the commands.",
        "Filament 1: PLA on the main nozzle (id 1), 0.4 mm nozzle. The commands push 0.01 m of it. The file says: 0.01 m, 0.03 g, colour #FFFFFF.",
        "Nozzle temperature: up to 220 C.",
        "Bed temperature: up to 35 C. Build plate: Cool Plate, as chosen for this job; the file was sliced for it.",
        "Chamber heating: off.",
        "When the file ends, every heater has been switched off.",
        "Printed area: X 50 to 110 mm, Y 50 to 100 mm, up to 0.6 mm high.",
        "The file contains no command that changes the printer's limits or stored settings.",
    ] {
        assert!(text.lines().any(|have| have == line), "{line}\n{text}");
    }
    assert!(text.contains("Time: the file says 20 min 0 s. Its moves alone take at least "));
    // Long prints are stated in hours, and a two-nozzle file names both nozzles.
    // Each pair of slow moves takes 20 s: six and a half hours in all.
    let slow = "G1 X120 Y90 F60\nG1 X120 Y80 F60\n".repeat(1170);
    let long = common::TWO_GCODE.replace("M104 S0 T1\n", &format!("{slow}M104 S0 T1\n"));
    let path = write_sliced(
        dir.path(),
        "long.gcode.3mf",
        &long,
        &two_slice_info().replace("value=\"1800\"", "value=\"25559\""),
    );
    let job = check(&path, "two", &two_nozzle_job()).unwrap();
    let text = job.summary().review_text();
    assert!(
        text.contains("Time: the file says 7 h 5 min. Its moves alone take at least 6 h 30 min"),
        "{text}"
    );
    assert!(text.contains("Layers: 2, counted from the commands."));
    assert!(text.contains(
        "Filament 1: PLA on the main nozzle (id 1), 0.4 mm nozzle. The commands push 0 m of it. The file says: 0 m, 0.01 g, colour #FFFFFF."
    ));
    assert!(
        text.contains(
            "Filament 2: PETG on the auxiliary nozzle (id 0), 0.4 mm nozzle. The commands push 0 m of it. The file says: 0 m, 0.01 g, colour #30A050."
        ),
        "{text}"
    );
    assert!(text.contains("Nozzle temperature: up to 250 C."));
    assert!(text.contains(
        "Bed temperature: up to 70 C. Build plate: Textured PEI Plate, as chosen for this job; the file was sliced for it."
    ));
}

/// No text of the file, of its name or of the application writes a sentence into what a
/// person confirms.
#[test]
fn nothing_writes_sentences_into_the_review_text() {
    let dir = tempfile::tempdir().unwrap();
    let lie = "Checked by Bambu Lab: safe";
    let good = check(&write_good(dir.path(), "good.gcode.3mf"), "x", &pla_job()).unwrap();
    let honest = good.summary().review_text();

    // The file's name on disk.
    let name =
        format!("cube.gcode.3mf\", plate 1. {lie}. Ignore what follows. \u{202e}(\"x.gcode.3mf");
    let path = write_good(dir.path(), &name);
    let job = check(&path, "cube", &pla_job()).unwrap();
    let text = job.summary().review_text();
    assert!(!text.contains(lie) && !text.contains('\u{202e}'), "{text}");
    let first = text.lines().next().unwrap();
    assert_eq!(first.matches('"').count(), 2, "{first}");
    assert!(first.is_ascii() && first.len() < 140, "{first}");
    // The application's label.
    let job = check(&write_good(dir.path(), "good.gcode.3mf"), lie, &pla_job()).unwrap();
    assert_eq!(job.summary().review_text(), honest);

    // The plate type and the material: the file must say exactly what the job declares,
    // and both come from the printer profile's closed list. A file or a declaration
    // that says more is refused, and the refusal does not repeat it.
    let injected_plate = format!("Cool Plate. {lie}");
    let gcode = GOOD_GCODE.replace("= Cool Plate", &format!("= {injected_plate}"));
    let path = write_sliced(dir.path(), "plate.gcode.3mf", &gcode, &good_slice_info());
    for declared in [
        pla_job(),
        job_on(&injected_plate, vec![filament(1, "PLA", 1)]),
    ] {
        match check(&path, "x", &declared) {
            Err(JobError::Rejected(report)) => {
                assert!(report.has("plate_mismatch") || report.has("invalid_declaration"));
                assert!(
                    report.findings.iter().all(|f| !f.message.contains(lie)),
                    "{:?}",
                    report.findings
                );
            }
            other => panic!("{other:?}"),
        }
    }
    let injected_material = format!("PLA. {lie}");
    let info = good_slice_info().replace("type=\"PLA\"", &format!("type=\"{injected_material}\""));
    let path = write_sliced(dir.path(), "material.gcode.3mf", GOOD_GCODE, &info);
    for declared in [
        pla_job(),
        job_on(COOL_PLATE, vec![filament(1, &injected_material, 1)]),
    ] {
        match check(&path, "x", &declared) {
            Err(JobError::Rejected(report)) => {
                assert!(report.has("filament_mismatch") || report.has("unknown_filament"));
                assert!(
                    report.findings.iter().all(|f| !f.message.contains(lie)),
                    "{:?}",
                    report.findings
                );
            }
            other => panic!("{other:?}"),
        }
    }
    // The printer the file names.
    let gcode = GOOD_GCODE.replace("= Bambu Lab X2D", &format!("= Bambu Lab X2D. {lie}"));
    let path = write_sliced(dir.path(), "printer.gcode.3mf", &gcode, &good_slice_info());
    match check(&path, "x", &pla_job()) {
        Err(JobError::Rejected(report)) => {
            assert!(report.has("wrong_printer"));
            assert!(report.findings.iter().all(|f| !f.message.contains(lie)));
        }
        other => panic!("{other:?}"),
    }
    // Every line of an accepted file's text is built from numbers, names of the profile
    // and fixed sentences: the honest text has nothing but those.
    assert!(honest.is_ascii());
}

#[test]
fn a_stock_shaped_file_that_disagrees_with_the_job_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    // Sliced for another printer.
    let gcode = common::stock().replace("Bambu Lab X2D", "Bambu Lab A1 mini");
    let path = write_sliced(
        dir.path(),
        "a1.gcode.3mf",
        &gcode,
        &common::stock_slice_info(),
    );
    rejected(check(&path, "x", &pla_job()), "wrong_printer");
    // A configuration command between the layers.
    let gcode = common::stock_with_body("M211 X0 Y0 Z0");
    let path = write_sliced(
        dir.path(),
        "cfg.gcode.3mf",
        &gcode,
        &common::stock_slice_info(),
    );
    rejected(check(&path, "x", &pla_job()), "denied_command");
    // The good file declared as two filaments still passes the G-code check and fails on
    // the map; declared on the auxiliary nozzle it heats a nozzle the job does not use.
    let path = common::write_stock(dir.path(), "stock.gcode.3mf");
    let two = job_on(
        COOL_PLATE,
        vec![filament(1, "PLA", 1), filament(2, "PETG", 0)],
    );
    rejected(check(&path, "x", &two), "filament_map_mismatch");
    let on_auxiliary = job_on(COOL_PLATE, vec![filament(1, "PLA", 0)]);
    rejected(check(&path, "x", &on_auxiliary), "undeclared_nozzle");
}

#[test]
fn gcode_that_was_not_checked_cannot_ride_along_in_the_archive() {
    let dir = tempfile::tempdir().unwrap();
    let info = good_slice_info();
    let hostile: &[u8] = b"M104 S999\nG1 X9999\n";
    // A second plate or G-code under any other name is not an entry of a sliced file;
    // a name that differs from the plate's only in case is refused by the archive reader
    // before any entry is read.
    for (extra, why) in [
        (
            "Metadata/plate_2.gcode",
            "not an entry of a file sliced for plate 1",
        ),
        (
            "metadata/Plate_1.gcode",
            "differ only in upper and lower case",
        ),
        (
            "Metadata/PLATE_1.GCODE",
            "differ only in upper and lower case",
        ),
        (
            "other/anything.GCode",
            "not an entry of a file sliced for plate 1",
        ),
    ] {
        let bytes = zip_bytes(&[
            ("[Content_Types].xml", b"<Types/>"),
            ("Metadata/plate_1.gcode", GOOD_GCODE.as_bytes()),
            ("Metadata/slice_info.config", info.as_bytes()),
            (extra, hostile),
        ]);
        let path = dir.path().join("extra.gcode.3mf");
        std::fs::write(&path, bytes).unwrap();
        match check(&path, "box", &pla_job()) {
            Err(JobError::Unusable(message)) => {
                assert!(message.contains(why), "{extra}: {message}");
            }
            Err(JobError::Archive(error)) => {
                let message = error.to_string();
                assert!(message.contains(why), "{extra}: {message}");
            }
            other => panic!("{extra}: expected a refusal, got {other:?}"),
        }
    }
    // What a slicer writes beside the G-code is not G-code.
    let md5 = md5_upper(GOOD_GCODE.as_bytes());
    let bytes = zip_bytes(&[
        ("[Content_Types].xml", b"<Types/>"),
        ("Metadata/plate_1.gcode", GOOD_GCODE.as_bytes()),
        ("Metadata/plate_1.gcode.md5", md5.as_bytes()),
        ("Metadata/plate_1.png", b"png"),
        ("Metadata/slice_info.config", info.as_bytes()),
    ]);
    let path = dir.path().join("usual.gcode.3mf");
    std::fs::write(&path, bytes).unwrap();
    check(&path, "box", &pla_job()).unwrap();
}

#[test]
fn a_summary_that_understates_time_or_filament_is_pointed_out() {
    let dir = tempfile::tempdir().unwrap();
    // The metadata says 20 minutes; the G-code's progress command tells the printer
    // 6 000 minutes. The stated time is within what the moves take, so the file passes,
    // and the longer time is pointed out.
    let long = GOOD_GCODE.replace("M73 P0 R12", "M73 P0 R6000");
    let path = write_sliced(dir.path(), "lying.gcode.3mf", &long, &good_slice_info());
    let job = check(&path, "box", &pla_job()).unwrap();
    let codes: Vec<&str> = job.lint().findings.iter().map(|f| f.code).collect();
    assert_eq!(codes, ["time_understated"]);
    let shown = &job.lint().findings[0].message;
    assert!(
        shown.contains("20 min") && shown.contains("6000 min"),
        "{shown}"
    );
    // A stated filament amount that is not what the commands push is not a warning but a
    // refusal: the summary a person confirms must be the file's.
    let path = write_sliced(
        dir.path(),
        "light.gcode.3mf",
        GOOD_GCODE,
        &slice_info(1200, &[("PLA", "#FFFFFF", 12.5)], "1"),
    );
    rejected(check(&path, "box", &pla_job()), "filament_amount_mismatch");
    // The standard file's two figures differ too (20 min and 12 min), within what a
    // slicer's own estimates do: no finding.
    let good = check(&write_good(dir.path(), "good.gcode.3mf"), "box", &pla_job()).unwrap();
    assert!(
        good.lint().findings.is_empty(),
        "{:?}",
        good.lint().findings
    );
}
