//! The proof on hand-written G-code streams: the marker words, fan words and keys are
//! Hotend's own tables, every value is made up here, and no text of a real file is copied.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use hotend_settings::PrintSettings;
use hotend_slicer::SystemProfiles;
use hotend_slicer::keymap::Keymap;
use hotend_slicer::output::gcode::ScanLimits;
use hotend_slicer::proof::{self, Expected, GcodeFacts, ProofReport, ProofStatus};

/// A whole stream: header, settings, a start routine that sets the bed and the fans, and four
/// layers. `body` replaces the commands after the settings block when given.
fn stream(settings: &PrintSettings, edit: impl FnOnce(&mut Vec<(String, String)>)) -> String {
    let mut lines = echo_lines(settings);
    edit(&mut lines);
    let mut text = String::from(
        "; HEADER_BLOCK_START\n; filament: 1\n; HEADER_BLOCK_END\n\n; CONFIG_BLOCK_START\n",
    );
    for (key, value) in &lines {
        text.push_str(&format!("; {key} = {value}\n"));
    }
    text.push_str("; CONFIG_BLOCK_END\n\n");
    text.push_str(BODY);
    text
}

const BODY: &str = "\
M140 S65
M106 S0
M106 P2 S102
M106 P10 S102
M106 P3 S127
M83
; MACHINE_START_GCODE_END
; CHANGE_LAYER
M106 P2 S0
M106 P10 S0
; FEATURE: Brim
G1 X10 Y10 F1200
G1 X20 Y10 E1
G1 E-.8 F1800
G1 E.8
; FEATURE: Outer wall
G1 X20 Y20 E1 F1200
; FEATURE: Inner wall
G1 X21 Y20 E1
; FEATURE: Bottom surface
G1 X30 Y20 E1 F1800
; CHANGE_LAYER
M140 S60
; FEATURE: Outer wall
G1 X30 Y30 E1 F6000
; CHANGE_LAYER
G1 X40 Y30 E1
; CHANGE_LAYER
M106 S255
; FEATURE: Sparse infill
G1 X40 Y40 E1
";

fn judge(settings: &PrintSettings, text: &str) -> ProofReport {
    let facts = GcodeFacts::builtin().unwrap();
    let limits = ScanLimits {
        header_bytes: 1 << 20,
        scan_bytes: 1 << 30,
        line_bytes: 16 * 1024,
    };
    let scan = proof::scan(text.as_bytes(), facts, &limits);
    let expected = Expected::for_settings(settings, None).unwrap();
    proof::judge(
        &expected,
        &scan,
        Keymap::builtin().unwrap(),
        facts,
        &SystemProfiles::builtin().unwrap().bed_types,
    )
}

fn set(lines: &mut [(String, String)], key: &str, value: &str) {
    lines.iter_mut().find(|(k, _)| k == key).unwrap().1 = value.to_owned();
}

#[test]
fn a_file_that_shows_every_asked_setting_passes() {
    let settings = recipe_settings();
    let report = judge(&settings, &stream(&settings, |_| {}));
    assert!(report.passed(), "{report:#?}");
    // Every fact was measured, and the chamber fan is only reported.
    let fact = |id: &str| report.facts.iter().find(|fact| fact.id == id).unwrap();
    assert_eq!(fact("bed_first_layer").measured, Some(65.0));
    assert_eq!(fact("bed_layer_3").measured, Some(60.0));
    assert_eq!(fact("part_fan_delay").measured, Some(4.0));
    assert_eq!(fact("first_layer_wall_speed").measured, Some(20.0));
    assert_eq!(fact("first_layer_fill_speed").measured, Some(30.0));
    assert_eq!(fact("chamber_fan_first_layer").measured, Some(127.0));
    assert!(fact("chamber_fan_first_layer").ok);
}

#[test]
fn each_way_of_missing_the_asked_settings_has_its_own_code() {
    let settings = recipe_settings();
    let base = stream(&settings, |_| {});
    let cases: Vec<(&str, String)> = vec![
        // The echo.
        (
            "setting_not_applied",
            stream(&settings, |lines| {
                set(lines, "initial_layer_speed", "50,50")
            }),
        ),
        (
            "setting_not_applied",
            stream(&settings, |lines| set(lines, "wall_loops", "2")),
        ),
        (
            "setting_not_applied",
            stream(&settings, |lines| set(lines, "curr_bed_type", "Cool Plate")),
        ),
        // An unwritten sticking field must state the baseline.
        (
            "setting_not_applied",
            stream(&settings, |lines| {
                set(lines, "elefant_foot_compensation", "0.2")
            }),
        ),
        // Any echo outside the field's range, written or not; a token Hotend does not know.
        (
            "profile_value_out_of_range",
            stream(&settings, |lines| set(lines, "outer_wall_speed", "500,50")),
        ),
        (
            "profile_value_out_of_range",
            stream(&settings, |lines| set(lines, "brim_type", "auto_brim")),
        ),
        // The measured facts.
        (
            "bed_temperature_not_applied",
            base.replace("M140 S65", "M140 S55"),
        ),
        (
            "bed_temperature_not_applied",
            base.replace("M140 S60", "M140 S65"),
        ),
        ("fan_on_first_layer", base.replace("M106 P2 S0\n", "")),
        (
            "fan_on_first_layer",
            base.replace(
                "; CHANGE_LAYER\nM140 S60",
                "; CHANGE_LAYER\nM106 S255\nM140 S60",
            ),
        ),
        (
            "fan_on_first_layer",
            base.replace(
                "; FEATURE: Bottom surface",
                "M106 S10\n; FEATURE: Bottom surface",
            ),
        ),
        (
            "first_layer_too_fast",
            base.replace("G1 X10 Y10 F1200", "G1 X10 Y10 F3000"),
        ),
        (
            "first_layer_too_fast",
            base.replace("G1 X30 Y20 E1 F1800", "G1 X30 Y20 E1 F2400"),
        ),
        (
            "brim_missing",
            base.replace("; FEATURE: Brim", "; FEATURE: Outer wall"),
        ),
        (
            "unexpected_prime_tower",
            base.replace(
                "G1 X30 Y30 E1 F6000",
                "G1 X30 Y30 E1 F6000\n; FEATURE: Prime tower\nG1 X50 Y50 E1",
            ),
        ),
        (
            "unexpected_filament_change",
            base.replace("; filament: 1", "; filament: 1,2"),
        ),
        // Completeness comes first.
        (
            "proof_incomplete",
            stream(&settings, |lines| {
                lines.retain(|(key, _)| key != "brim_width")
            }),
        ),
        (
            "proof_incomplete",
            stream(&settings, |lines| {
                lines.push(("wall_loops".into(), "3".into()))
            }),
        ),
        ("proof_incomplete", base.replace("; CONFIG_BLOCK_END\n", "")),
        (
            "proof_incomplete",
            base.replace("; FEATURE: Inner wall", "; FEATURE: Skirt"),
        ),
        (
            "proof_incomplete",
            base.replace(
                "; FEATURE: Brim\nG1 X10 Y10 F1200",
                "; FEATURE: Brim\nG1 X10 Y10",
            ),
        ),
        ("proof_incomplete", base.replace("; CHANGE_LAYER\n", "")),
        ("proof_incomplete", base.replace("; filament: 1\n", "")),
    ];
    for (code, text) in cases {
        let report = judge(&settings, &text);
        assert_eq!(report.code, Some(code), "{report:#?}");
        assert!(!report.passed());
        if code == "proof_incomplete" {
            assert_eq!(report.status, ProofStatus::Incomplete);
        }
        // A failed proof is an error with that code and a plain sentence.
        let error = report.error();
        assert_eq!(error.code(), code);
        assert!(error.to_string().contains("Nothing") || code == "proof_incomplete");
    }
}

#[test]
fn a_plan_with_other_settings_does_not_pass_on_this_file() {
    let recipe = recipe_settings();
    let stock = stock_settings();
    let report = judge(&stock, &stream(&recipe, |_| {}));
    assert_eq!(report.code, Some("setting_not_applied"), "{report:#?}");
    let sentence = report.error().to_string();
    assert!(sentence.contains("asked"), "{sentence}");
}

#[test]
fn the_report_holds_parsed_values_only() {
    let settings = recipe_settings();
    // Text in the file that must never reach the report: in a setting Hotend reads, in one it
    // does not read, in a feature name and in a comment.
    let text = stream(&settings, |lines| {
        set(lines, "sparse_infill_pattern", "CANARY");
        lines.push(("printer_notes".into(), "CANARY".into()));
    })
    .replace("; FEATURE: Sparse infill", "; FEATURE: CANARY\n; CANARY");
    let report = judge(&settings, &text);
    assert_eq!(report.code, Some("profile_value_out_of_range"));
    let json = serde_json::to_string(&report).unwrap();
    assert!(!json.contains("CANARY"), "{json}");
    assert!(!report.error().to_string().contains("CANARY"));
    assert!(json.contains("\"unreadable\""), "{json}");
}
