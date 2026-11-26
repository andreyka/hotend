//! The check on a real sliced file, when one is at hand.
//!
//! A file written by Bambu Studio contains Bambu Lab's own start and end routines, so it
//! is never stored in this repository. Point `HOTEND_REAL_SLICE_DIR` at a folder that
//! holds `plate_1.gcode.3mf` (one PLA filament on the main nozzle of the X2D) and these
//! tests run on it:
//!
//! ```text
//! HOTEND_REAL_SLICE_DIR=/path/to/folder cargo test -p hotend-printer --test real_slice -- --ignored --nocapture
//! ```
//!
//! A plain `cargo test` reports them as ignored, with the reason. Run with `--ignored` they
//! fail without the variable, or when a file is not in that folder: none of them can pass
//! without the files.
//! Any real slice is held to the general assertions. The file the table was written from
//! (Bambu Studio 02.09.01.70, sliced 2026-11-17) is also held to the numbers measured on it.
//! It is recognised by `plate_1.gcode.3mf.sha256` in the same folder (what `sha256sum` prints
//! for it): without that file, or when it names another hash, those numbers are not compared.
//! The folder also holds the two textured slices, `textured-a.gcode.3mf` (37 layers)
//! and `textured-b.gcode.3mf` (344 layers), ring-shaped test prints; they are always held to
//! their numbers.
#![allow(clippy::unwrap_used, clippy::print_stderr)]

mod common;

use std::io::Read;
use std::path::PathBuf;

use common::{
    COOL_PLATE, END_START, START_END, TEXTURED_PLATE, check, filament, job_on, md5_upper, pla_job,
    x2d,
};
use hotend_printer::lint::{LintReport, lint_gcode};
use hotend_printer::{JobError, Section, Severity};

/// Why the tests that read the real files are ignored by a plain `cargo test`.
const NEEDS_FILES: &str = "HOTEND_REAL_SLICE_DIR is not set";

/// The folder with the real files. The tests that need it are `ignore`d and run with
/// `-- --ignored`; without the folder they fail: a run meant to check the real file must
/// not pass without it.
fn real_dir() -> PathBuf {
    let Some(dir) = std::env::var_os("HOTEND_REAL_SLICE_DIR") else {
        panic!("{NEEDS_FILES}");
    };
    PathBuf::from(dir)
}

/// The real sliced file.
fn real_file() -> PathBuf {
    real_file_in(&real_dir())
}

fn real_file_in(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("plate_1.gcode.3mf");
    assert!(
        path.is_file(),
        "HOTEND_REAL_SLICE_DIR is set, but {} does not exist",
        path.display()
    );
    path
}

#[test]
#[should_panic(expected = "HOTEND_REAL_SLICE_DIR is set, but")]
fn a_named_folder_without_the_file_fails_the_test() {
    let empty = tempfile::tempdir().unwrap();
    let _ = real_file_in(empty.path());
}

/// Every entry of the archive, by name.
fn entries_of(path: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    let file = std::fs::File::open(path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    (0..archive.len())
        .map(|index| {
            let mut entry = archive.by_index(index).unwrap();
            let mut content = Vec::new();
            entry.read_to_end(&mut content).unwrap();
            (entry.name().unwrap().into_owned(), content)
        })
        .collect()
}

/// The real archive with one entry replaced or added, written to `dir`.
fn rebuilt(path: &std::path::Path, dir: &std::path::Path, change: &[(&str, Vec<u8>)]) -> PathBuf {
    let mut entries = entries_of(path);
    for (name, content) in change {
        entries.retain(|(known, _)| known != name);
        entries.push(((*name).to_owned(), content.clone()));
    }
    let out = dir.join("changed.gcode.3mf");
    std::fs::write(&out, common::zip_bytes(&common::borrowed(&entries))).unwrap();
    out
}

/// The G-code of plate 1 inside the archive.
fn gcode_of(path: &std::path::Path) -> String {
    let file = std::fs::File::open(path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    let mut text = String::new();
    archive
        .by_name("Metadata/plate_1.gcode")
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    text
}

fn lint(gcode: &str) -> LintReport {
    lint_gcode(gcode.as_bytes(), &pla_job(), &x2d(), u64::MAX).unwrap()
}

#[test]
#[ignore = "needs the real files in HOTEND_REAL_SLICE_DIR; run with `-- --ignored`"]
fn the_real_sliced_file_is_acceptable_and_its_review_text_is_right() {
    let path = real_file();
    let job = match check(&path, "real slice", &pla_job()) {
        Ok(job) => job,
        Err(JobError::Rejected(report)) => panic!(
            "the real file was rejected with {} error(s): {:#?}",
            report.errors,
            &report.findings[..report.findings.len().min(20)]
        ),
        Err(other) => panic!("the real file cannot be used: {other}"),
    };
    let report = job.lint();
    let summary = job.summary();
    let stats = &report.stats;
    eprintln!("{}", summary.review_text());

    // Holds for any real slice of one PLA filament on the main nozzle.
    assert!(report.passed());
    let warnings: Vec<&str> = report.findings.iter().map(|finding| finding.code).collect();
    assert_eq!(
        warnings,
        ["configuration_commands"],
        "the only warning is the list of accepted configuration commands"
    );
    assert_eq!(stats.slots_used.iter().copied().collect::<Vec<_>>(), [1]);
    assert!(stats.sections.marked);
    assert!(stats.sections.start > 0 && stats.sections.body > 0 && stats.sections.end > 0);
    assert!(stats.printing_moves > 0);
    assert_eq!(summary.stated_printer.as_deref(), Some("Bambu Lab X2D"));
    assert!(summary.plate_type.is_some());
    // The time and the amount the file states are held to what its commands do.
    let stated = summary.estimated_seconds.unwrap();
    assert!(summary.min_seconds > 0 && summary.min_seconds <= stated);
    assert_eq!(summary.layers, Some(stats.layers_printed));
    assert_eq!(stats.total_layers, summary.layers);
    assert_eq!(summary.filaments.len(), 1);
    let filament = &summary.filaments[0];
    assert_eq!(filament.material.as_deref(), Some("PLA"));
    assert_eq!(
        (filament.nozzle, filament.nozzle_role.as_deref()),
        (Some(1), Some("main"))
    );
    assert!(filament.selected_in_gcode);
    let stated_metres = filament.metres.unwrap();
    assert!(
        (filament.measured_metres - stated_metres).abs() < 0.02 * stated_metres + 0.01,
        "{} m measured, {stated_metres} m stated",
        filament.measured_metres
    );
    assert!(
        summary
            .max_nozzle_c
            .is_some_and(|c| (170.0..=240.0).contains(&c))
    );
    assert!(summary.max_bed_c.is_some_and(|c| (0.0..=70.0).contains(&c)));
    // The stock routine switches the printer's own travel limit check off and on again,
    // and the person confirming is told so.
    let has = |command: &str, section: Section| {
        summary
            .configuration
            .iter()
            .any(|used| used.command == command && used.section == section)
    };
    assert!(has("M211 X0 Y0 Z0", Section::Start));
    assert!(has("M211 Z1", Section::End));
    assert!(has("M500 D1", Section::Start));
    assert!(
        summary
            .configuration
            .iter()
            .all(|used| used.section != Section::Body)
    );
    let text = summary.review_text();
    assert!(
        text.contains("M211 X0 Y0 Z0, 2 times in the start routine"),
        "{text}"
    );
    assert!(
        text.contains("M500 D1, once in the start routine: unknown"),
        "{text}"
    );

    if !job_is_recorded(&path) {
        eprintln!("this is not the recorded file; the measured numbers were not compared");
        return;
    }
    // The recorded file: what was measured on it.
    assert_eq!(summary.estimated_seconds, Some(710));
    assert_eq!(
        summary.min_seconds, 262,
        "the moves at the speeds they name"
    );
    assert_eq!(summary.layers, Some(26));
    assert!((filament.measured_metres - 1.487_368_66).abs() < 1e-6);
    assert!((stats.min_seconds - 262.499).abs() < 0.01);
    assert_eq!(filament.grams, Some(4.52));
    assert_eq!(filament.metres, Some(1.49));
    assert_eq!(
        filament
            .colour
            .as_ref()
            .map(|colour| colour.hex())
            .as_deref(),
        Some("00AE42FF")
    );
    assert_eq!(filament.nozzle_diameter_mm, Some(0.4));
    assert_eq!(summary.max_nozzle_c, Some(220.0));
    assert_eq!(summary.max_nozzle_reference_c, Some(240.0));
    assert_eq!(summary.max_bed_c, Some(35.0));
    assert_eq!(summary.max_chamber_c, Some(0.0));
    assert_eq!(summary.plate_type.as_deref(), Some("Cool Plate"));
    assert_eq!(summary.unplaced_moves, 5);
    assert_eq!(stats.lines, 42_200);
    assert_eq!(stats.commands, 38_695);
    assert_eq!(stats.moves, 37_301);
    assert_eq!(stats.printing_moves, 33_280);
    assert_eq!((stats.sections.start, stats.sections.end), (278, 77));
    let extent = summary.extent.unwrap();
    assert_eq!(extent.min, [2.215, 2.21, 0.2]);
    assert_eq!(extent.max, [133.393, 95.391, 5.2]);
    let counted: u32 = summary.configuration.iter().map(|used| used.count).sum();
    assert_eq!(counted, 16);
    for line in [
        "Print file \"plate_1.gcode.3mf\", plate 1, checked for the Bambu Lab X2D.",
        "The file says it was sliced for: Bambu Lab X2D.",
        "Time: the file says 11 min 50 s. Its moves alone take at least 4 min 22 s at the speeds they name; heating, the printer's own routines and acceleration come on top.",
        "Layers: 26, counted from the commands.",
        "Filament 1: PLA on the main nozzle (id 1), 0.4 mm nozzle. The commands push 1.49 m of it. The file says: 1.49 m, 4.52 g, colour #00AE42.",
        "Nozzle temperature: up to 220 C. A setting of the printer's own routine names 240 C and may heat the nozzle that far; it is within the filament's limit.",
        "Bed temperature: up to 35 C. Build plate: Cool Plate, as chosen for this job; the file was sliced for it.",
        "Chamber heating: off.",
        "When the file ends, every heater has been switched off.",
        "Printed area: X 2.22 to 133.39 mm, Y 2.21 to 95.39 mm, up to 5.2 mm high.",
        "5 short move(s) in the start routine start from a position only the printer knows; they go no further than the stock routine's, and none is written to end closer to the bed than where the printer left the nozzle.",
    ] {
        assert!(text.lines().any(|have| have == line), "{line}\n{text}");
    }
}

/// The real file with one harmful change each time. Every one must fail.
#[test]
#[ignore = "needs the real files in HOTEND_REAL_SLICE_DIR; run with `-- --ignored`"]
fn the_real_sliced_file_fails_with_a_harmful_change() {
    let path = real_file();
    let gcode = gcode_of(&path);
    assert!(lint(&gcode).passed());
    let start_end = format!("{START_END}\n");
    let end_start = format!("{END_START}\n");
    assert_eq!(gcode.matches(&start_end).count(), 1);
    assert_eq!(gcode.matches(&end_start).count(), 1);
    // `line` as the last line of the start routine, the first of the print, or the last
    // of the print.
    let in_start = |line: &str| gcode.replacen(&start_end, &format!("{line}\n{start_end}"), 1);
    let in_body = |line: &str| gcode.replacen(&start_end, &format!("{start_end}{line}\n"), 1);
    let before_end = |line: &str| gcode.replacen(&end_start, &format!("{line}\n{end_start}"), 1);

    let cases: Vec<(&str, String, &str)> = vec![
        (
            "a configuration command in the print",
            in_body("M211 X0 Y0 Z0"),
            "denied_command",
        ),
        (
            "stored settings written in the print",
            before_end("M500 D1"),
            "denied_command",
        ),
        (
            "a third switch of the travel limit check",
            in_start("M211 X0 Y0 Z0"),
            "too_many",
        ),
        (
            "the travel limit check in another form",
            in_start("M211 S0"),
            "denied_command",
        ),
        ("a firmware command", in_start("M997"), "denied_command"),
        (
            "an unknown vendor command",
            in_start("M9999 S1"),
            "unknown_command",
        ),
        (
            "a nozzle temperature above the limit",
            in_start("M104 S260"),
            "temperature_over_limit",
        ),
        (
            "a bed temperature above the limit",
            in_start("M140 S100"),
            "temperature_over_limit",
        ),
        (
            "a vendor temperature above the limit",
            in_start("G150 T260"),
            "temperature_over_limit",
        ),
        ("a second tool", in_body("T1"), "undeclared_tool"),
        (
            "the other nozzle heated",
            in_start("M104 S220 T0"),
            "undeclared_nozzle",
        ),
        (
            "a move outside the travel box",
            in_start("G1 X300 Y10"),
            "out_of_reach",
        ),
        (
            "a move outside the travel box in the print",
            in_body("G1 Y-5"),
            "out_of_reach",
        ),
        (
            "a descent towards the bed from a position the file does not give",
            in_start("G28\nG91\nG1 Z-22\nG90"),
            "unverifiable_move",
        ),
        (
            "a guarded descent that no lift stands before",
            in_start("G28\nG91\nG380 S2 Z-12 F900\nG90"),
            "unverifiable_move",
        ),
        (
            "one more lift from a position the file does not give than the stock routine makes",
            in_start("G28\nG91\nG1 Z5\nG90"),
            "too_many",
        ),
        (
            "plastic laid in the start routine",
            in_start("G1 X50 Y50 Z0.3\nG1 X90 Y50 E3"),
            "printing_outside_body",
        ),
        (
            "a second command on a line",
            in_body("G1 X10 G1 X300"),
            "multiple_commands",
        ),
        (
            "a forged marker",
            before_end("; EXECUTABLE_BLOCK_START\nM211 X0 Y0 Z0"),
            "section_marker",
        ),
        (
            "the print declared as start routine",
            gcode.replacen(&start_end, "", 1).replacen(
                &end_start,
                &format!("{start_end}{end_start}"),
                1,
            ),
            "printing_outside_body",
        ),
        (
            "a file cut off in the print",
            gcode[..gcode.find(&end_start).unwrap()].to_owned(),
            "truncated",
        ),
        (
            "the first layer pressed into the plate",
            in_body("G1 Z0.1"),
            "too_close_to_bed",
        ),
        (
            "a Z trim below zero",
            gcode.replacen("G29.1 Z0.022", "G29.1 Z-0.05", 1),
            "value_out_of_range",
        ),
        (
            "plastic laid into the air",
            before_end("G1 Z40\nG1 X60 Y60 E0.5"),
            "extrusion_in_the_air",
        ),
        (
            "half a metre of filament pushed on the spot",
            before_end("G1 E500"),
            "extrusion_in_place",
        ),
        (
            "more plastic along a path than the nozzle lays down",
            before_end("G1 Z5.2\nG1 X60 Y60 E20"),
            "over_extrusion",
        ),
        (
            "the bed left on at the end",
            {
                let (print, end) = gcode.split_at(gcode.find(&end_start).unwrap());
                format!("{print}{}", end.replacen("M140 S0", "M400", 1))
            },
            "heaters_left_on",
        ),
        (
            "the bed heated again in the end routine",
            gcode.replacen(&end_start, &format!("{end_start}M140 S35\n"), 1),
            "heating_in_end_routine",
        ),
        (
            "the nozzle switched off in the middle of the print",
            in_body("M104 S0"),
            "cold_extrusion",
        ),
        (
            "the position redefined",
            in_body("G92 X0 Y0"),
            "unexpected_parameters",
        ),
        (
            "a move faster than any of the slice",
            in_body("G1 X100 Y100 F999999"),
            "value_out_of_range",
        ),
        (
            "a bed temperature the build plate does not take",
            in_start("M140 S60"),
            "temperature_over_limit",
        ),
        (
            "a layer count that is not the commands'",
            gcode.replacen("; total layer number: 26", "; total layer number: 3", 1),
            "layer_count_mismatch",
        ),
        (
            "another printer named",
            gcode.replacen(
                "; printer_model = Bambu Lab X2D",
                "; printer_model = Bambu Lab H2D",
                1,
            ),
            "wrong_printer",
        ),
    ];
    for (what, changed, code) in &cases {
        let report = lint(changed);
        assert!(
            !report.passed() && report.has(code),
            "{what}: expected {code}, got {} error(s): {:?}",
            report.errors,
            report
                .findings
                .iter()
                .filter(|finding| finding.severity == Severity::Error)
                .take(3)
                .collect::<Vec<_>>()
        );
    }
    // The unchanged file against another job: another material, the other nozzle,
    // another build plate, another nozzle size.
    let petg = job_on(COOL_PLATE, vec![filament(1, "PETG", 1)]);
    let report = lint_gcode(gcode.as_bytes(), &petg, &x2d(), u64::MAX).unwrap();
    assert!(
        report.has("filament_mismatch"),
        "{:?}",
        &report.findings[..1]
    );
    let on_auxiliary = job_on(COOL_PLATE, vec![filament(1, "PLA", 0)]);
    assert!(matches!(
        check(&path, "x", &on_auxiliary),
        Err(JobError::Rejected(_))
    ));
    let rejected_as =
        |result: Result<hotend_printer::CheckedJob, JobError>, code: &str| match result {
            Err(JobError::Rejected(report)) => {
                assert!(report.has(code), "{code}: {:?}", report.findings);
            }
            other => panic!("expected {code}, got {other:?}"),
        };
    let on_textured = job_on(TEXTURED_PLATE, vec![filament(1, "PLA", 1)]);
    rejected_as(check(&path, "x", &on_textured), "plate_mismatch");

    // The archive with one change each time: what the file says about itself against
    // what its commands do, and content nobody read.
    let dir = tempfile::tempdir().unwrap();
    let entry = |name: &str| {
        let entries = entries_of(&path);
        let (_, content) = entries.iter().find(|(known, _)| known == name).unwrap();
        String::from_utf8(content.clone()).unwrap()
    };
    let info = entry("Metadata/slice_info.config");
    let with_info = |from: &str, to: &str| {
        assert!(info.contains(from), "{from}");
        rebuilt(
            &path,
            dir.path(),
            &[(
                "Metadata/slice_info.config",
                info.replacen(from, to, 1).into_bytes(),
            )],
        )
    };
    // Unchanged content, written again by another zip writer, is still acceptable.
    assert!(check(&rebuilt(&path, dir.path(), &[]), "x", &pla_job()).is_ok());
    if job_is_recorded(&path) {
        rejected_as(
            check(
                &with_info(
                    "key=\"prediction\" value=\"710\"",
                    "key=\"prediction\" value=\"60\"",
                ),
                "x",
                &pla_job(),
            ),
            "time_mismatch",
        );
        rejected_as(
            check(
                &with_info(
                    "key=\"prediction\" value=\"710\"",
                    "key=\"prediction\" value=\"86400\"",
                ),
                "x",
                &pla_job(),
            ),
            "time_mismatch",
        );
        for (from, to) in [
            ("used_m=\"1.49\"", "used_m=\"0.33\""),
            ("used_m=\"1.49\"", "used_m=\"100.00\""),
            ("used_g=\"4.52\"", "used_g=\"1.00\""),
            ("used_g=\"4.52\"", "used_g=\"300.00\""),
        ] {
            rejected_as(
                check(&with_info(from, to), "x", &pla_job()),
                "filament_amount_mismatch",
            );
        }
        rejected_as(
            check(&with_info("type=\"PLA\"", "type=\"PETG\""), "x", &pla_job()),
            "filament_mismatch",
        );
    }
    // The G-code changed after the slicer wrote its checksum.
    let harmless = format!("{gcode}; a comment more\n");
    rejected_as(
        check(
            &rebuilt(
                &path,
                dir.path(),
                &[("Metadata/plate_1.gcode", harmless.clone().into_bytes())],
            ),
            "x",
            &pla_job(),
        ),
        "gcode_md5_mismatch",
    );
    assert!(
        check(
            &rebuilt(
                &path,
                dir.path(),
                &[
                    ("Metadata/plate_1.gcode", harmless.clone().into_bytes()),
                    (
                        "Metadata/plate_1.gcode.md5",
                        md5_upper(harmless.as_bytes()).into_bytes()
                    ),
                ],
            ),
            "x",
            &pla_job()
        )
        .is_ok(),
        "with its own checksum the changed file is a file again"
    );
    // A second plate that nobody read.
    let second = rebuilt(
        &path,
        dir.path(),
        &[("Metadata/plate_2.gcode", b"M104 S300\nG1 Z-5\n".to_vec())],
    );
    assert!(matches!(
        check(&second, "x", &pla_job()),
        Err(JobError::Unusable(_))
    ));
}

/// Whether `path` is the file the numbers were measured on: the file `<path>.sha256` beside
/// it holds its hash. The hash is not written in this repository.
fn job_is_recorded(path: &std::path::Path) -> bool {
    let mut beside = path.as_os_str().to_owned();
    beside.push(".sha256");
    let Ok(text) = std::fs::read_to_string(&beside) else {
        return false;
    };
    let Some(recorded) = text.split_whitespace().next() else {
        return false;
    };
    check(path, "x", &pla_job()).is_ok_and(|job| job.sha256().eq_ignore_ascii_case(recorded))
}

// ---- The Textured PEI Plate ---------------------------------------------------------------

// Two files sliced for the Textured PEI Plate on 2026-11-17 (Bambu Studio 02.09.01.70,
// X2D, 0.4 mm nozzle, one Generic PLA with Hotend's settings), by their names in
// `HOTEND_REAL_SLICE_DIR`. The plate's rows of the table were written from them.
/// A ring-shaped test print of 37 layers.
const TEXTURED_A: &str = "textured-a.gcode.3mf";
/// A ring-shaped test print of 344 layers.
const TEXTURED_B: &str = "textured-b.gcode.3mf";

/// One of the textured slices. As for the first file, a missing folder or file fails.
fn textured_file(file: &str) -> PathBuf {
    let path = real_dir().join(file);
    assert!(
        path.is_file(),
        "HOTEND_REAL_SLICE_DIR is set, but {} does not exist",
        path.display()
    );
    path
}

fn textured_job() -> hotend_printer::DeclaredJob {
    job_on(TEXTURED_PLATE, vec![filament(1, "PLA", 1)])
}

/// `gcode` with the value of `letter` replaced on the first line that starts with
/// `command` after `after`; with `None` the word is dropped.
fn with_word(gcode: &str, after: &str, command: &str, letter: char, value: Option<&str>) -> String {
    let from = gcode.find(after).unwrap();
    let mut done = false;
    let mut out = String::with_capacity(gcode.len());
    out.push_str(&gcode[..from]);
    for line in gcode[from..].split_inclusive('\n') {
        let code = line.split(';').next().unwrap_or("");
        if done || !code.trim_start().starts_with(command) {
            out.push_str(line);
            continue;
        }
        done = true;
        let indent = &code[..code.len() - code.trim_start().len()];
        let words: Vec<String> = code
            .split_whitespace()
            .filter_map(|word| {
                if word.starts_with(letter) && !command.split_whitespace().any(|w| w == word) {
                    value.map(|value| format!("{letter}{value}"))
                } else {
                    Some(word.to_owned())
                }
            })
            .collect();
        out.push_str(indent);
        out.push_str(&words.join(" "));
        out.push('\n');
    }
    assert!(done, "no {command} line after {after}");
    out
}

#[test]
#[ignore = "needs the real files in HOTEND_REAL_SLICE_DIR; run with `-- --ignored`"]
fn the_real_textured_slices_are_acceptable_for_the_textured_plate() {
    for file in [TEXTURED_A, TEXTURED_B] {
        let path = textured_file(file);
        let job = match check(&path, "textured", &textured_job()) {
            Ok(job) => job,
            Err(JobError::Rejected(report)) => panic!(
                "{file} was rejected with {} error(s): {:#?}",
                report.errors,
                &report.findings[..report.findings.len().min(20)]
            ),
            Err(other) => panic!("{file} cannot be used: {other}"),
        };
        let summary = job.summary();
        let stats = &job.lint().stats;
        let warnings: Vec<&str> = job.lint().findings.iter().map(|f| f.code).collect();
        assert_eq!(warnings, ["configuration_commands"]);
        assert_eq!(summary.plate_type.as_deref(), Some(TEXTURED_PLATE));
        assert_eq!(summary.layers, Some(stats.layers_printed));
        let trims: Vec<&str> = summary
            .configuration
            .iter()
            .filter(|used| used.command.starts_with("G29.1"))
            .map(|used| used.command.as_str())
            .collect();
        assert_eq!(trims, ["G29.1 Z0", "G29.1 Z0.002"]);
        let text = summary.review_text();
        assert!(
            text.contains("Build plate: Textured PEI Plate, as chosen for this job; the file was sliced for it."),
            "{text}"
        );
        // What was measured on the two files.
        assert_eq!(summary.max_bed_c, Some(65.0));
        assert_eq!(summary.max_nozzle_c, Some(220.0));
        assert_eq!(summary.unplaced_moves, 5);
        assert_eq!(stats.sections.end, 77);
        let filament = &summary.filaments[0];
        let extent = summary.extent.unwrap();
        if file == TEXTURED_A {
            assert_eq!(summary.estimated_seconds, Some(1152));
            assert_eq!((summary.layers, summary.min_seconds), (Some(37), 713));
            assert_eq!((filament.grams, filament.metres), (Some(5.38), Some(1.8)));
            assert_eq!((stats.lines, stats.commands), (53_409, 49_078));
            assert_eq!(stats.sections.start, 278);
            assert_eq!(extent.max, [211.342, 178.989, 7.4]);
        } else {
            // 344 layers of 0.2 mm, 100.42 g, 3 h 37 min 46 s.
            assert_eq!(summary.estimated_seconds, Some(13_066));
            assert_eq!((summary.layers, summary.min_seconds), (Some(344), 9323));
            assert_eq!(
                (filament.grams, filament.metres),
                (Some(100.42), Some(33.67))
            );
            assert_eq!((stats.lines, stats.commands), (770_458, 708_061));
            assert_eq!(stats.sections.start, 276);
            assert_eq!(extent.min, [27.658, 27.655, 0.2]);
            assert_eq!(extent.max, [228.344, 228.339, 68.8]);
            assert!(
                text.lines()
                    .any(|line| line == "Layers: 344, counted from the commands.")
            );
        }
    }
}

/// A file is acceptable only for the plate it was sliced for: the names, the bed
/// temperature and the Z trim each say so on their own.
#[test]
#[ignore = "needs the real files in HOTEND_REAL_SLICE_DIR; run with `-- --ignored`"]
fn a_real_slice_is_refused_for_the_other_plate() {
    let codes = |result: Result<hotend_printer::CheckedJob, JobError>| match result {
        Err(JobError::Rejected(report)) => report
            .findings
            .iter()
            .filter(|finding| finding.severity == Severity::Error)
            .map(|finding| (finding.code, finding.message.clone()))
            .collect::<Vec<_>>(),
        other => panic!("expected a refusal, got {other:?}"),
    };
    let count = |found: &[(&str, String)], code: &str| {
        found.iter().filter(|(have, _)| *have == code).count()
    };
    for file in [TEXTURED_A, TEXTURED_B] {
        let path = textured_file(file);
        let found = codes(check(&path, "x", &pla_job()));
        assert_eq!(count(&found, "plate_mismatch"), 3, "{found:?}");
        assert_eq!(count(&found, "temperature_over_limit"), 6, "{found:?}");
        assert_eq!(count(&found, "value_out_of_range"), 1, "{found:?}");
        assert!(
            found
                .iter()
                .any(|(code, message)| *code == "value_out_of_range"
                    && message.starts_with("G29.1: Z0.002")),
            "{found:?}"
        );
        assert_eq!(found.len(), 10, "{found:?}");
    }
    let smooth = real_file();
    let found = codes(check(&smooth, "x", &textured_job()));
    assert_eq!(count(&found, "plate_mismatch"), 3, "{found:?}");
    assert!(
        found
            .iter()
            .any(|(code, message)| *code == "value_out_of_range"
                && message.starts_with("G29.1: Z0.022")),
        "{found:?}"
    );
    assert_eq!(found.len(), 4, "{found:?}");
}

/// A textured slice with one harmful change each time. Every one must fail.
#[test]
#[ignore = "needs the real files in HOTEND_REAL_SLICE_DIR; run with `-- --ignored`"]
fn a_real_textured_slice_fails_with_a_harmful_change() {
    let path = textured_file(TEXTURED_A);
    let gcode = gcode_of(&path);
    let lint =
        |gcode: &str| lint_gcode(gcode.as_bytes(), &textured_job(), &x2d(), u64::MAX).unwrap();
    assert!(lint(&gcode).passed());
    let open = "EXECUTABLE_BLOCK_START";
    let trim = |value: &str| with_word(&gcode, open, "G29.1 Z0.", 'Z', Some(value));
    let share = |value: Option<&str>| with_word(&gcode, START_END, "M142", 'U', value);
    let cooling =
        |letter: char, value: &str| with_word(&gcode, START_END, "M142", letter, Some(value));
    let bed_in_print = |value: &str| with_word(&gcode, START_END, "M140", 'S', Some(value));
    let bed_in_start = |value: &str| with_word(&gcode, open, "M190", 'S', Some(value));
    let cases: Vec<(&str, String, &str)> = vec![
        ("the Cool Plate's trim", trim("0.022"), "value_out_of_range"),
        (
            "a trim ten times as large",
            trim("0.02"),
            "value_out_of_range",
        ),
        ("a trim of a whole layer", trim("0.2"), "value_out_of_range"),
        ("a trim of 2 mm", trim("2"), "value_out_of_range"),
        ("a trim below zero", trim("-0.002"), "value_out_of_range"),
        ("a trim far below zero", trim("-0.2"), "value_out_of_range"),
        (
            // 0.3, 0.5 and 0.7 have been seen in real slices (0.5 since 18 November 2026).
            "a fan share nobody saw",
            share(Some("0.4")),
            "value_out_of_range",
        ),
        (
            "a fan share next to the one seen",
            share(Some("0.31")),
            "value_out_of_range",
        ),
        ("a fan share of 1", share(Some("1")), "value_out_of_range"),
        ("a fan share of 3", share(Some("3")), "value_out_of_range"),
        (
            "a fan share below zero",
            share(Some("-0.3")),
            "value_out_of_range",
        ),
        (
            "auto cooling without the share",
            share(None),
            "unexpected_parameters",
        ),
        ("another R", cooling('R', "60"), "value_out_of_range"),
        ("another S", cooling('S', "80"), "value_out_of_range"),
        ("another O", cooling('O', "90"), "value_out_of_range"),
        ("another V", cooling('V', "0.5"), "value_out_of_range"),
        (
            "a bed hotter than PLA takes, in the print",
            bed_in_print("71"),
            "temperature_over_limit",
        ),
        (
            "a bed hotter than PLA takes, in the start",
            bed_in_start("71"),
            "temperature_over_limit",
        ),
        (
            "a bed hotter than the printer's",
            bed_in_print("121"),
            "temperature_over_limit",
        ),
    ];
    for (what, changed, code) in cases {
        assert_ne!(changed, gcode, "{what}: nothing was changed");
        let report = lint(&changed);
        let errors: Vec<&str> = report
            .findings
            .iter()
            .filter(|finding| finding.severity == Severity::Error)
            .map(|finding| finding.code)
            .collect();
        assert_eq!(errors, [code], "{what}: {:?}", report.findings);
    }
    // The bed limit is the filament's: the same file is not acceptable as another
    // plastic's job, and 70 C in place of 60 C is still PLA's.
    assert!(lint(&bed_in_print("70")).passed());
}
