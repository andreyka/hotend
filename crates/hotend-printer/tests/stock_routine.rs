//! The G-code check on files in the shape Bambu Studio writes: section markers, the
//! printer's own start and end routines, and for each kind of harm in the threat model
//! (`src/lint.rs`) a file that must fail.
//!
//! The fixtures are written by hand for these tests from the command table, in a made-up
//! order and length. Single commands carry values measured in real slices, as the table does;
//! no longer passage and no comment of the vendor's routines is reproduced (compared with the
//! real files: no more than two consecutive lines are the same). The comments in the
//! fixtures are the block markers (`HEADER_BLOCK_START`, `CONFIG_BLOCK_START`,
//! `EXECUTABLE_BLOCK_START` and their ends), the section markers and the statements the check
//! reads.
#![allow(clippy::unwrap_used)]

mod common;

use common::{
    CLOSE, COOL_PLATE, END_START, OPEN, START_END, STATEMENTS, STOCK_BODY, STOCK_END,
    STOCK_PREAMBLE, STOCK_START, TEXTURED_PLATE, filament, for_textured, job_on, marked, pla_job,
    stock, stock_with_body, stock_with_end, stock_with_start, x2d,
};
use hotend_printer::lint::{LintReport, lint_gcode};
use hotend_printer::{ConfigKind, DeclaredJob, Section, Severity};

fn lint_as(job: &DeclaredJob, gcode: &str) -> LintReport {
    lint_gcode(gcode.as_bytes(), job, &x2d(), u64::MAX).unwrap()
}

fn lint(gcode: &str) -> LintReport {
    lint_as(&pla_job(), gcode)
}

fn error_codes(report: &LintReport) -> Vec<&'static str> {
    report
        .findings
        .iter()
        .filter(|finding| finding.severity == Severity::Error)
        .map(|finding| finding.code)
        .collect()
}

/// The file must be refused, and its first error must be `code`.
#[track_caller]
fn refused(gcode: &str, code: &str) -> LintReport {
    let report = lint(gcode);
    assert!(
        !report.passed() && error_codes(&report).first() == Some(&code),
        "expected {code} first, got {:?}",
        report.findings
    );
    report
}

#[track_caller]
fn accepted(gcode: &str) -> LintReport {
    let report = lint(gcode);
    assert!(report.passed(), "{:?}", report.findings);
    report
}

/// Each of `lines`, added to the end of the start routine, must be refused as `code`.
#[track_caller]
fn refused_in_start(lines: &[&str], code: &str) {
    for line in lines {
        let report = lint(&stock_with_start(line));
        assert!(
            !report.passed() && error_codes(&report).first() == Some(&code),
            "{line:?} in the start routine: expected {code}, got {:?}",
            report.findings
        );
    }
}

#[track_caller]
fn refused_in_body(lines: &[&str], code: &str) {
    for line in lines {
        let report = lint(&stock_with_body(line));
        assert!(
            !report.passed() && error_codes(&report).first() == Some(&code),
            "{line:?} in the print: expected {code}, got {:?}",
            report.findings
        );
    }
}

#[track_caller]
fn refused_in_end(lines: &[&str], code: &str) {
    for line in lines {
        let report = lint(&stock_with_end(line));
        assert!(
            !report.passed() && error_codes(&report).first() == Some(&code),
            "{line:?} in the end routine: expected {code}, got {:?}",
            report.findings
        );
    }
}

// ---------------------------------------------------------------------------------------
// The file that must pass
// ---------------------------------------------------------------------------------------

#[test]
fn a_stock_shaped_file_passes_and_its_configuration_commands_are_reported() {
    let report = accepted(&stock());
    // One warning, and it is the list of accepted configuration commands.
    assert_eq!(report.warnings, 1, "{:?}", report.findings);
    let warning = &report.findings[0];
    assert_eq!(warning.code, "configuration_commands");
    assert_eq!(warning.severity, Severity::Warning);
    for part in [
        "M211 X0 Y0 Z0 once",
        "M211 Z1 once",
        "M500 D1 once",
        "M201 X18000 Y18000 Z400 E4000 once",
        "M17 D once",
        "G29.1 Z0.022 once",
        "any other form is refused",
    ] {
        assert!(
            warning.message.contains(part),
            "{part}: {}",
            warning.message
        );
    }

    let stats = &report.stats;
    let lines = |text: &str| text.lines().count() as u64;
    assert!(stats.sections.marked);
    assert_eq!(stats.sections.start, lines(STOCK_START));
    assert_eq!(stats.sections.body, lines(STOCK_BODY));
    assert_eq!(stats.sections.end, lines(STOCK_END));
    assert_eq!(
        stats.commands,
        lines(STOCK_START) + lines(STOCK_BODY) + lines(STOCK_END)
    );
    assert_eq!(stats.total_layers, Some(2));
    assert_eq!(stats.first_remaining_minutes, Some(9));
    assert_eq!(stats.slots_used.iter().copied().collect::<Vec<_>>(), [1]);
    assert_eq!(stats.max_nozzle_c, Some(220.0));
    assert_eq!(stats.max_nozzle_reference_c, Some(235.0));
    assert_eq!(stats.max_bed_c, Some(35.0));
    assert_eq!(stats.max_chamber_c, None);
    assert_eq!(stats.printing_moves, 4);
    // Measured from the commands: two layers, 5.2 mm pushed and 0.8 mm pulled back at
    // the end, and the time the moves take at the speeds they name.
    assert_eq!(stats.layers_printed, 2);
    assert_eq!(stats.filament_mm.len(), 1);
    assert!(
        (stats.filament_mm[0] - 4.4).abs() < 1e-9,
        "{:?}",
        stats.filament_mm
    );
    assert!(
        stats.min_seconds > 3.0 && stats.min_seconds < 30.0,
        "{}",
        stats.min_seconds
    );
    let extent = stats.extent.unwrap();
    assert_eq!(extent.min, [60.0, 60.0, 0.2]);
    assert_eq!(extent.max, [110.0, 100.0, 0.4]);
    // The two relative moves made before homing.
    assert_eq!(stats.unplaced_moves, 2);
    assert_eq!(stats.stated.printer_model.as_deref(), Some("Bambu Lab X2D"));
    assert_eq!(stats.stated.plate_type.as_deref(), Some("Cool Plate"));
    assert_eq!(stats.stated.nozzle_diameters_mm, [0.4, 0.4]);

    // Every accepted configuration command, with its place and its count.
    let configuration: Vec<(ConfigKind, &str, Section, u32)> = stats
        .configuration
        .iter()
        .map(|used| (used.kind, used.command.as_str(), used.section, used.count))
        .collect();
    assert_eq!(
        configuration,
        [
            (
                ConfigKind::MotionLimits,
                "M201 X18000 Y18000 Z400 E4000",
                Section::Start,
                1
            ),
            (
                ConfigKind::MotionLimits,
                "M203 X450 Y450 Z18 E25",
                Section::Start,
                1
            ),
            (
                ConfigKind::MotionLimits,
                "M204 P9000 R4000 T9000",
                Section::Start,
                1
            ),
            (
                ConfigKind::MotionLimits,
                "M205 X8 Y8 Z2 E2",
                Section::Start,
                1
            ),
            (ConfigKind::TravelLimits, "M211 X0 Y0 Z0", Section::Start, 1),
            (ConfigKind::StoredSettings, "M500 D1", Section::Start, 1),
            (ConfigKind::MotorCurrent, "M17 D", Section::Start, 1),
            (ConfigKind::ZOffset, "G29.1 Z0.022", Section::Start, 1),
            (ConfigKind::MotorCurrent, "M17 Z0.4", Section::End, 1),
            (ConfigKind::MotorCurrent, "M17 S", Section::End, 1),
            (ConfigKind::MotorCurrent, "M17 R", Section::End, 1),
            (ConfigKind::TravelLimits, "M211 Z1", Section::End, 1),
            (ConfigKind::MotionLimits, "M201.2 K1", Section::End, 1),
        ]
    );
    // A row of unknown meaning says so to the person who confirms.
    assert!(stats.configuration[5].what.starts_with("unknown"));
}

#[test]
fn the_dialect_of_the_stock_routine_is_read_as_one_command_per_line() {
    // Parameters with the letters M, T, R, H and N, a sub-code, a tool with a parameter,
    // lower case, a line number and a checksum.
    accepted(&stock_with_start(
        "M1006 A12 B14 L33 C21 D16 M27 E5 F11 N41\nM1006 W\nG28 Z P0 T400\nm400 s2\nN7 M400 P200*33\nG150.3\nM970.3 Q0 A7 K0 O1",
    ));
    // The same letters are still a second command where the row does not list them.
    refused_in_start(
        &[
            "M400 M500 D1",
            "M1006 S1 G28",
            "G28 X T300 R G1 X300",
            "M106 P1 S0 T1",
            "M211 X0 Y0 Z0 M500",
        ],
        "multiple_commands",
    );
    // A listed letter in a shape the table does not have is refused too.
    refused_in_start(
        &["M1006 S1 M50", "M1006 A12 B14 L33"],
        "unexpected_parameters",
    );
    // A sub-code is part of the name: these are other commands, and not in the table.
    refused_in_start(
        &["G150.9", "M620.99 S1", "G29.3", "M1002.1 x"],
        "unknown_command",
    );
}

#[test]
fn long_comment_lines_do_not_fail_the_file_but_are_capped() {
    // The slicer writes whole routines into two config comments, each longer than a line
    // with a command may be.
    let long = format!("; machine_start_gcode = {}\n", "M400\\n".repeat(3000));
    assert!(long.len() > 3 * hotend_printer::lint::MAX_LINE_BYTES);
    let gcode = marked(
        &format!("{STOCK_PREAMBLE}{long}{long}"),
        STOCK_START,
        STOCK_BODY,
        STOCK_END,
    );
    let report = accepted(&gcode);
    assert_eq!(report.warnings, 1);
    assert_eq!(
        report.stats.stated.printer_model.as_deref(),
        Some("Bambu Lab X2D")
    );

    // What looks like a comment but carries a command after a carriage return is read.
    let hidden = format!("; {}\rM997\n", "x".repeat(5000));
    refused(&stock_with_body(hidden.trim_end()), "denied_command");
    // A long line that starts with a command stays an error, with or without a comment.
    let report = refused(
        &stock_with_body(&format!("G1 X70 ; {}", "x".repeat(5000))),
        "line_too_long",
    );
    assert_eq!(report.errors, 1);
    // The total is capped.
    let huge = format!("; {}\n", "x".repeat(700 * 1024));
    let gcode = marked(
        &format!("{STOCK_PREAMBLE}{huge}{huge}"),
        STOCK_START,
        STOCK_BODY,
        STOCK_END,
    );
    refused(&gcode, "comment_too_large");
}

#[test]
fn a_file_of_comments_only_passes_only_if_it_claims_nothing_it_does_not_do() {
    // A file with no command at all harms nothing, and says so in a warning.
    let silent = format!("; HEADER_BLOCK_START\n; HEADER_BLOCK_END\n\n{STATEMENTS}");
    let report = accepted(&silent);
    assert!(report.has("no_moves"));
    assert!(!report.stats.sections.marked);
    assert_eq!(report.stats.layers_printed, 0);
    assert_eq!(
        report.stats.stated.plate_type.as_deref(),
        Some("Cool Plate")
    );
    // One that says it has twelve layers and lays none is refused: what it states is
    // not what it does.
    let claims = format!(
        "; HEADER_BLOCK_START\n; total layer number: 12\n; HEADER_BLOCK_END\n\n{STATEMENTS}\n; layer num/total_layer_count: 1/12\n"
    );
    let report = refused(&claims, "layer_count_mismatch");
    assert_eq!(report.errors, 1);
    assert_eq!(report.stats.total_layers, Some(12));
    // And one that does not say what it was sliced for is refused for that.
    let report = lint("; HEADER_BLOCK_START\n; HEADER_BLOCK_END\n; nozzle_temperature = 220\n");
    for code in ["wrong_printer", "plate_mismatch", "nozzle_mismatch"] {
        assert!(report.has(code), "{code}: {:?}", report.findings);
    }
}

// ---------------------------------------------------------------------------------------
// Harm 1: a tampered or substituted file. The attacker writes the markers.
// ---------------------------------------------------------------------------------------

/// The attacker's best case, made explicit. A forger who writes the markers can call any
/// leading part of the file "start routine" and any trailing part "end routine". What that
/// buys is listed here: every stock command at its stock values, each as often as the
/// table allows, the firmware routines of the stock start, and the five moves the stock
/// routine makes from a position the file does not give: no further than it makes them,
/// and none that brings the nozzle closer to the bed than the printer left it. All of it
/// is reported. Nothing else passes, and the second half of the test shows each next step
/// failing.
#[test]
fn the_most_a_forged_start_routine_gains_is_the_stock_commands_at_their_stock_values() {
    let forged_start = "\
M211 X0 Y0 Z0
M211 X0 Y0 Z0
M500 D1
M205 X9 Y9 Z3 E2.5
M201 X20000 Y20000 Z500 E30000
M17 D
M204 P20000 R30000 T20000
M203 X1000 Y1000 Z20 E30
M17 D
G29.1 Z0
G29.1 Z0.022
G91
G1 Y-16 F900
G380 S2 Z22 F900
G380 S2 Z-12 F900
G1 Z6
G1 Z5
G90
G28 X T300 R
G28 Z P0 T400
M104 S240
M140 S45
G150.1
G150.2
G150.3
G29 A2 X0 Y0 I256 J256 R
M620.10 A1 F523.843 H0.4 T240 P240 S1
";
    // The forger must still say what the file is for, and must switch every heater off.
    let forged_end = "M104 S0 T0\nM104 S0 T1\nM140 S0\n";
    let gcode = marked(STOCK_PREAMBLE, forged_start, STOCK_BODY, forged_end);
    let report = accepted(&gcode);
    // All of it is shown: the configuration commands in the warning and in the numbers.
    assert_eq!(report.warnings, 1);
    assert!(report.has("configuration_commands"));
    let counted: u32 = report
        .stats
        .configuration
        .iter()
        .map(|used| used.count)
        .sum();
    assert_eq!(counted, 11);
    assert_eq!(report.stats.unplaced_moves, 5);
    assert_eq!(
        report.stats.max_nozzle_c,
        Some(240.0),
        "the limit of PLA, not above"
    );
    assert_eq!(
        report.stats.max_bed_c,
        Some(45.0),
        "the ceiling of the Cool Plate, not above"
    );

    // One step further, each time, and the file fails.
    let step = |line: &str, code: &str| {
        let gcode = marked(
            STOCK_PREAMBLE,
            &format!("{forged_start}{line}\n"),
            STOCK_BODY,
            forged_end,
        );
        let report = lint(&gcode);
        assert!(
            !report.passed() && error_codes(&report).first() == Some(&code),
            "{line:?}: expected {code}, got {:?}",
            report.findings
        );
    };
    // A stock configuration command once more than the stock routine has it.
    step("M211 X0 Y0 Z0", "too_many");
    step("M500 D1", "too_many");
    step("M201 X20000 Y20000 Z500 E30000", "too_many");
    step("G29.1 Z0", "too_many");
    // The same commands with other values or letters.
    step("M201.2 K1", "command_out_of_place");
    step("M204 P20001 R30000 T20000", "value_out_of_range");
    step("M211 S0", "denied_command");
    step("M211 X0 Y0 Z1", "value_out_of_range");
    // One more move from a position the file does not give, however short and whichever
    // way; a longer one; one the other way; one towards the bed.
    step("G91\nG1 Z0.1", "too_many");
    step("G91\nG1 Y-0.1", "too_many");
    step("G91\nG1 Z-0.1", "unverifiable_move");
    step("G91\nG380 S2 Z-0.1 F900", "unverifiable_move");
    for (other, code) in [
        ("G1 Y-16.5 F900", "unverifiable_move"),
        ("G1 Y16 F900", "unverifiable_move"),
        ("G1 X1 F900", "unverifiable_move"),
        ("G1 Z-16 F900", "unverifiable_move"),
    ] {
        let changed = forged_start.replacen("G1 Y-16 F900", other, 1);
        refused(
            &marked(STOCK_PREAMBLE, &changed, STOCK_BODY, forged_end),
            code,
        );
    }
    // The lowering is accepted only behind the lift, and only when it is shorter.
    let lower_first = forged_start.replacen(
        "G380 S2 Z22 F900\nG380 S2 Z-12 F900\n",
        "G380 S2 Z-12 F900\nG380 S2 Z22 F900\n",
        1,
    );
    refused(
        &marked(STOCK_PREAMBLE, &lower_first, STOCK_BODY, forged_end),
        "unverifiable_move",
    );
    // Hotter than the filament may be, or than the build plate takes.
    step("M104 S241", "temperature_over_limit");
    step("M140 S46", "temperature_over_limit");
    // A Z trim other than the two values the stock routine sets: a trim below zero
    // brings every layer closer to the plate than the file says.
    for trim in ["G29.1 Z-0.05", "G29.1 Z-0.02", "G29.1 Z0.05"] {
        let changed = forged_start.replacen("G29.1 Z0.022", trim, 1);
        refused(
            &marked(STOCK_PREAMBLE, &changed, STOCK_BODY, forged_end),
            "value_out_of_range",
        );
    }
    // A purge on the spot, which the stock routine leaves to the printer's own routines.
    step("M83\nG1 E20 F200", "extrusion_in_place");
    // Heaters left on at the end.
    let left_on = marked(
        STOCK_PREAMBLE,
        forged_start,
        STOCK_BODY,
        "M104 S0 T1\nM140 S0\n",
    );
    refused(&left_on, "heaters_left_on");
    // Plastic along a path, a move outside the travel box, a command of the print.
    step(
        "M83\nG1 X10 Y10 Z0.3\nG1 X50 Y10 E2",
        "printing_outside_body",
    );
    step("G1 X300", "out_of_reach");
    step("M981 S1 P20000", "command_out_of_place");
    // And the claim cannot be stretched: the start routine has a length limit.
    let padded = format!("{forged_start}{}", "M400\n".repeat(600));
    refused(
        &marked(STOCK_PREAMBLE, &padded, STOCK_BODY, forged_end),
        "section_too_long",
    );
}

#[test]
fn a_print_declared_as_start_or_end_routine_cannot_print() {
    // The whole print moved in front of the marker that ends the start routine.
    let gcode = marked(
        STOCK_PREAMBLE,
        &format!("{STOCK_START}{STOCK_BODY}"),
        "",
        STOCK_END,
    );
    let report = lint(&gcode);
    assert!(report.has("printing_outside_body"), "{:?}", report.findings);
    assert_eq!(report.stats.printing_moves, 0);
    assert_eq!(report.stats.extent, None);
    // And behind the marker that starts the end routine.
    let gcode = marked(
        STOCK_PREAMBLE,
        STOCK_START,
        "",
        &format!("{STOCK_BODY}{STOCK_END}"),
    );
    let report = lint(&gcode);
    assert!(report.has("printing_outside_body"), "{:?}", report.findings);
    assert_eq!(report.stats.printing_moves, 0);
    // Moves alone, so that nothing else is wrong with the file.
    let moves = "M83\nG1 X50 Y50 Z0.3 F3000\nG1 X90 Y50 E2\n";
    let report = refused(
        &marked(
            STOCK_PREAMBLE,
            &format!("{STOCK_START}{moves}"),
            STOCK_BODY,
            STOCK_END,
        ),
        "printing_outside_body",
    );
    assert_eq!(report.errors, 1);
    assert!(report.findings[0].message.contains("the start routine"));
    let report = refused(
        &marked(
            STOCK_PREAMBLE,
            STOCK_START,
            STOCK_BODY,
            &format!("{moves}{STOCK_END}"),
        ),
        "printing_outside_body",
    );
    assert_eq!(report.errors, 1);
    assert!(report.findings[0].message.contains("the end routine"));
    // An arc is a path too.
    refused_in_start(
        &["M83\nG1 X50 Y50 Z0.3\nG2 X60 Y50 I5 J0 E1"],
        "printing_outside_body",
    );
    // A purge on the spot is not a path, but the stock routine leaves purging to the
    // printer's own routines: plastic pushed on the spot is a blob wherever it stands.
    refused_in_start(&["M83\nG1 E20 F200"], "extrusion_in_place");
}

#[test]
fn a_section_marker_cannot_be_repeated_moved_or_forged_into_a_plain_file() {
    // The start routine cannot be opened again in the print to get its rows back.
    for marker in [OPEN, START_END, CLOSE] {
        let report = refused(
            &stock_with_body(&format!("{marker}\nM211 X0 Y0 Z0")),
            "section_marker",
        );
        // The rows of the start routine did not come back with the marker.
        assert!(
            report.has("denied_command"),
            "{marker}: {:?}",
            report.findings
        );
    }
    // Nor can the end routine begin twice, or the start routine end twice.
    refused(&stock_with_end(END_START), "section_marker");
    refused(&stock_with_start(OPEN), "section_marker");
    refused(&stock_with_end(START_END), "section_marker");
    // Markers out of order.
    let swapped =
        format!("{OPEN}\n{STOCK_START}{END_START}\n{STOCK_END}{START_END}\n{STOCK_BODY}{CLOSE}\n");
    refused(&swapped, "section_marker");
    // A file whose first command stands before any marker has no sections: a marker that
    // follows opens nothing, and the rows of the start routine do not apply.
    let late = format!("G90\n{OPEN}\nM211 X0 Y0 Z0\n{START_END}\n");
    let report = refused(&late, "section_marker");
    assert!(report.has("denied_command"));
    assert!(!report.stats.sections.marked);
    // Nothing may follow the marker that ends the program.
    let report = refused(&format!("{}G1 X300 Y300\n", stock()), "section_marker");
    assert_eq!(report.findings[0].line, stock().lines().count() as u64 + 1);
    // A marker inside the comment of a command line, or with other text, is not a marker.
    refused_in_body(
        &[
            "G1 X70 ; MACHINE_END_GCODE_START\nM211 Z1",
            "; MACHINE_END_GCODE_START now\nM211 Z1",
            "; machine_end_gcode_start\nM211 Z1",
        ],
        "denied_command",
    );
}

#[test]
fn a_marked_file_that_stops_early_is_refused() {
    let whole = stock();
    for marker in [START_END, END_START, CLOSE] {
        let cut = &whole[..whole.find(marker).unwrap()];
        let report = refused(cut, "truncated");
        assert!(report.findings[0].message.contains(&marker[2..]));
    }
    // An opener and nothing else.
    refused(&format!("{OPEN}\n"), "truncated");
}

// ---------------------------------------------------------------------------------------
// Harm 2: custom G-code injected through a profile, into the start routine, the end
// routine or between layers.
// ---------------------------------------------------------------------------------------

#[test]
fn an_unknown_vendor_command_is_refused_wherever_it_stands() {
    let unknown = [
        "M1005 X1 Y1",
        "G39 S1",
        "M9999",
        "G150.4",
        "M973 S1",
        "M620.1 E F500 T240",
    ];
    refused_in_start(&unknown, "unknown_command");
    refused_in_body(&unknown, "unknown_command");
    refused_in_end(&unknown, "unknown_command");
    // Known words in a shape or with a value the stock routine does not use.
    refused_in_start(
        &[
            "M972 S19 P0 X1",
            "M972 S19",
            "M975",
            "G150.2 F100",
            "M1012.5 N1",
        ],
        "unexpected_parameters",
    );
    refused_in_start(
        &[
            "M972 S99 P0",
            "M1012.1 T1000",
            "M970.3 Q0 A6 K0 O1",
            "G29.2 S2",
            "M1006 A12 B14 L33 C21 D16 M27 E5 F11 N63",
            "M620.10 A0 F900 H0.4 T235 P220 S1",
            "M620.10 A0 F300 H0.8 T235 P220 S1",
            "M400 S600",
            "G150.1 F20000",
        ],
        "value_out_of_range",
    );
    // A flag is a letter alone.
    refused_in_start(&["M17 D5", "G28 X T300 R1"], "value_out_of_range");
    refused_in_start(&["G28 X5 T300 R"], "unexpected_parameters");
}

#[test]
fn commands_of_the_start_and_end_routines_are_refused_between_layers() {
    refused_in_body(
        &[
            "G380 S2 Z10 F900",
            "G150.1 F6000",
            "G29 A1 X60 Y60 I80 J80 R",
            "G130 O0 X90 Y-0.2 Z0.6 F4 L40 E12 D4",
            "M620.10 A0 F300 H0.4 T235 P220 S1",
            "T65279",
            "M17 D",
            "G29.1 Z0",
        ],
        "command_out_of_place",
    );
    // The start routine's own forms of commands the print also has.
    refused_in_body(
        &[
            "M400 S5",
            "G28 X T300 R",
            "M204 P9000 R4000 T9000",
            "M142 P6 R30 S40 U0.6 V0.8",
        ],
        "unexpected_parameters",
    );
    refused_in_body(&["M622.1 S0"], "value_out_of_range");
    // Commands of the print are not commands of the routines either.
    refused_in_start(
        &["M624 AQAAAAAAAAA=", "M981 S1 P20000", "M971 S11 C10 O0 T0"],
        "command_out_of_place",
    );
    refused_in_end(
        &["M625", "G130 O0 X90 Y-0.2 Z0.6 F4 L40 E12 D4"],
        "command_out_of_place",
    );
    // The start routine's commands do not pass in the end routine, and the reverse.
    refused_in_end(&["M211 X0 Y0 Z0", "M500 D1"], "denied_command");
    refused_in_start(&["M211 Z1"], "denied_command");
    refused_in_start(&["T65279", "M201.2 K1"], "command_out_of_place");
    refused_in_start(&["M620 S65279 B", "M17 Z0.4"], "unexpected_parameters");
}

#[test]
fn a_routine_longer_than_the_stock_one_is_refused() {
    let report = refused(
        &stock_with_start("M400\n".repeat(600).trim_end()),
        "section_too_long",
    );
    assert_eq!(report.errors, 1, "reported once");
    refused(
        &stock_with_end("M400\n".repeat(200).trim_end()),
        "section_too_long",
    );
    // The print itself has no such limit.
    accepted(&stock_with_body("M400\n".repeat(2000).trim_end()));
}

// ---------------------------------------------------------------------------------------
// Harm 3: a nozzle driven outside its reach.
// ---------------------------------------------------------------------------------------

#[test]
fn a_printing_move_outside_the_reach_is_refused() {
    // The print ends at X 60, Y 100, Z 0.4.
    refused_in_body(
        &[
            "G1 X256.5 Y100 E5",
            "G1 X60 Y-3 E5",
            "G1 Z261\nG1 X70 E1",
            // Both ends of the arc are inside; it bulges to X 257.
            "G1 X252 Y100\nG3 X252 Y110 I0 J5 E1",
        ],
        "out_of_reach",
    );
    // The travel box of the main nozzle reaches 0.2 mm in front of the plate, where the
    // start routine draws its load line. Plastic may not be laid there by a move.
    let report = refused(
        &stock_with_body("G1 Y-0.1\nG1 X90 E1"),
        "extrusion_outside_reach",
    );
    assert_eq!(
        report.stats.printing_moves, 4,
        "the refused move is not counted"
    );
    accepted(&stock_with_body("G1 Y-0.1\nG1 X90\nG1 Y0\nG1 X60 E1"));
    // The edge itself is inside.
    accepted(&stock_with_body("G1 X256 Y256 E9\nG1 X0 Y0 E9"));
}

#[test]
fn a_move_that_does_not_print_is_held_to_the_travel_box() {
    refused_in_start(
        &[
            "G1 X-1",
            "G1 Y-0.3",
            "G1 X257 Y100",
            "G1 Z260.5",
            "G0 Y300 F9000",
        ],
        "out_of_reach",
    );
    refused_in_end(&["G1 X128 Y257", "G1 Z300", "G1 X-20"], "out_of_reach");
    refused_in_body(&["G1 Y-0.25", "G0 X256.2"], "out_of_reach");
    // Firmware routines that take coordinates are held to the same box.
    refused_in_start(
        &[
            "G29 A1 X200 Y60 I80 J80 R",
            "G29 A1 X60 Y-5 I80 J80 R",
            "G29 A1 X60 Y60 I-10 J80 R",
        ],
        "out_of_reach",
    );
    let line = |x: &str, y: &str, z: &str, length: &str| {
        marked(
            STOCK_PREAMBLE,
            &STOCK_START.replace(
                "G130 O0 X90 Y-0.2 Z0.6 F4 L40 E12 D4",
                &format!("G130 O0 X{x} Y{y} Z{z} F4 L{length} E12 D4"),
            ),
            STOCK_BODY,
            STOCK_END,
        )
    };
    accepted(&line("90", "-0.2", "0.6", "40"));
    // The file does not say which way the line runs, so it must fit both ways.
    refused(&line("30", "-0.2", "0.6", "40"), "out_of_reach");
    refused(&line("230", "-0.2", "0.6", "40"), "out_of_reach");
    refused(&line("90", "-0.5", "0.6", "40"), "out_of_reach");
    refused(&line("90", "-0.2", "300", "40"), "out_of_reach");
    refused(&line("90", "-0.2", "-1", "40"), "too_close_to_bed");
    // Closer to the plate than a first layer.
    refused(&line("90", "-0.2", "0.1", "40"), "too_close_to_bed");
    refused(&line("90", "-0.2", "0.6", "0"), "out_of_reach");
    // And only once.
    refused_in_start(&["G130 O0 X90 Y-0.2 Z0.6 F4 L40 E12 D4"], "too_many");
    // The position is never redefined on X, Y or Z: the stock routine does not do it,
    // and with an offset in force no coordinate would mean what it says.
    refused_in_start(
        &["G1 X100 Y100 Z5\nG92 Z0\nG29 A1 X60 Y60 I80 J80 R"],
        "unexpected_parameters",
    );
}

#[test]
fn moves_from_a_position_the_file_does_not_give_are_short_few_and_only_in_the_start_routine() {
    // The start routine ends with the position unknown (a firmware routine ran last).
    refused_in_start(
        &[
            "G91\nG1 Z22.5",
            "G91\nG1 Y-17",
            "G91\nG1 X1",
            "G91\nG1 X0.5 Y1 Z1",
            // The guarded move is accepted in relative positioning only, and only this far.
            "G380 S2 Z10 F900",
        ],
        "unverifiable_move",
    );
    refused_in_start(
        &[
            "G91\nG380 S2 Z25 F900",
            "G91\nG380 S3 Z5 F900",
            "G91\nG380 S2 Z5 F9000",
        ],
        "value_out_of_range",
    );
    refused_in_start(&["G91\nG380 S2 X5 Z5 F900"], "unexpected_parameters");
    // From a height the file gives, the guarded move must also end inside the travel box.
    refused_in_start(&["G90\nG1 Z255\nG91\nG380 S2 Z10 F900"], "out_of_reach");
    accepted(&stock_with_start(
        "G90\nG1 Z20\nG91\nG380 S2 Z10 F900\nG380 S2 Z-5 F900\nG90",
    ));
    let report = accepted(&stock_with_start("G91\nG1 Z6\nG1 Y-16\nG90"));
    assert_eq!(report.stats.unplaced_moves, 4);
    // In the print and in the end routine such a move is refused, however short.
    let body_after_routine = STOCK_BODY.replacen("G90\nM83\n", "G91\nG1 Z1\nG90\nM83\n", 1);
    refused(
        &marked(STOCK_PREAMBLE, STOCK_START, &body_after_routine, STOCK_END),
        "unverifiable_move",
    );
    refused_in_end(&["G91\nG1 Z2"], "unverifiable_move");
    // A relative move from a known position is placed and checked like any other.
    accepted(&stock_with_body("G91\nG1 Z2\nG1 X20\nG90"));
    refused_in_body(&["G91\nG1 Z300"], "out_of_reach");
    // Plastic is never laid from a position the file does not give.
    let blind_print = STOCK_BODY.replacen("G1 Z0.4 F1200\nG1 X60 Y60 F9000\nG1 Z0.2\n", "", 1);
    refused(
        &marked(STOCK_PREAMBLE, STOCK_START, &blind_print, STOCK_END),
        "unverifiable_move",
    );
    // Nor at a height it does not give.
    let no_height = STOCK_BODY.replacen(
        "G1 Z0.4 F1200\nG1 X60 Y60 F9000\nG1 Z0.2\n",
        "G1 X60 Y60 F9000\n",
        1,
    );
    refused(
        &marked(STOCK_PREAMBLE, STOCK_START, &no_height, STOCK_END),
        "unverifiable_move",
    );
}

/// The stock start routine switches the printer's own travel limit check off, so a move
/// from a position the file does not give is checked by nothing but this rule. A lower Z
/// is a smaller gap between the nozzle and the bed.
#[test]
fn moves_from_a_position_the_file_does_not_give_cannot_close_the_gap_to_the_bed_or_add_up() {
    // A start routine that homes, so the file does not say where the nozzle is.
    let homed = |lines: &str| {
        let start = format!("M211 X0 Y0 Z0\nM140 S35\nM104 S220\nG28\nG91\n{lines}G90\n");
        marked(STOCK_PREAMBLE, &start, STOCK_BODY, STOCK_END)
    };
    let refused_after_homing = |lines: &str, code: &str| {
        let report = lint(&homed(lines));
        assert!(
            !report.passed() && error_codes(&report).first() == Some(&code),
            "{lines:?} after homing: expected {code}, got {:?}",
            report.findings
        );
    };
    // Towards the bed: twelve moves of 22 mm are 264 mm, more than the printer is high.
    refused_after_homing(&"G1 Z-22 F900\n".repeat(12), "unverifiable_move");
    // One such move is already too many, however short, plain or guarded.
    refused_after_homing("G1 Z-22 F900\n", "unverifiable_move");
    refused_after_homing("G1 Z-0.5 F900\n", "unverifiable_move");
    refused_after_homing("G0 Z-1\n", "unverifiable_move");
    refused_after_homing("G380 S2 Z-12 F900\n", "unverifiable_move");
    refused_after_homing(&"G380 S2 Z-12 F900\n".repeat(12), "unverifiable_move");

    // The stock pair: a guarded lift, then a guarded lowering that is shorter.
    let report = accepted(&homed("G380 S2 Z22 F900\nG380 S2 Z-12 F900\n"));
    assert_eq!(report.stats.unplaced_moves, 2);
    // Lower than the lift went, in one step or in two.
    refused_after_homing("G380 S2 Z10 F900\nG380 S2 Z-12 F900\n", "unverifiable_move");
    refused_after_homing(
        "G380 S2 Z20 F900\nG380 S2 Z-12 F900\nG380 S2 Z-12 F900\n",
        "unverifiable_move",
    );
    // A plain move is not guarded, so it never lowers, not even after a lift.
    refused_after_homing("G380 S2 Z22 F900\nG1 Z-5 F900\n", "unverifiable_move");
    refused_after_homing("G1 Z6 F900\nG1 Z-6 F900\n", "unverifiable_move");
    // The lift counts only until the printer moves by itself again: after homing, a
    // firmware routine or a tool change the nozzle is somewhere else.
    for between in [
        "G28",
        "G28 Z",
        "G150.1",
        "M620 M\nM620 S0A H-1 B\nT0 H-1\nM621 S0A B",
    ] {
        refused_after_homing(
            &format!("G380 S2 Z22 F900\n{between}\nG380 S2 Z-12 F900\n"),
            "unverifiable_move",
        );
    }
    // Nor does a lift count that the printer may have skipped.
    refused_after_homing(
        "M622 J1\nG380 S2 Z22 F900\nM623\nG380 S2 Z-12 F900\n",
        "unverifiable_move",
    );

    // Away from the bed a plain move is as short as the stock ones, and sideways it goes
    // only the one way the stock routine goes.
    accepted(&homed("G1 Z6 F900\n"));
    accepted(&homed("G1 Y-16 F9000\n"));
    refused_after_homing("G1 Z6.5 F900\n", "unverifiable_move");
    refused_after_homing("G1 Y16 F9000\n", "unverifiable_move");
    refused_after_homing("G1 Y-16.5 F9000\n", "unverifiable_move");
    refused_after_homing("G1 X-1\n", "unverifiable_move");
    refused_after_homing("G1 X1\n", "unverifiable_move");

    // The moves do not add up to more than the stock routine travels this way: 16 mm in
    // Y and 45 mm in Z over the whole file, in five moves.
    refused_after_homing("G1 Y-16 F9000\nG1 Y-16 F9000\n", "unverifiable_move");
    refused_after_homing("G1 Y-10 F9000\nG28 Y\nG1 Y-10 F9000\n", "too_many");
    let lifts = "G380 S2 Z22 F900\nG28 Z\nG380 S2 Z22 F900\nG28 Z\n";
    accepted(&homed(lifts));
    refused_after_homing(&format!("{lifts}G1 Z2 F900\n"), "too_many");
    // A lift cannot be stacked on a lift either: 22 mm is as far as one goes.
    refused_after_homing("G380 S2 Z22 F900\nG1 Z1 F900\n", "unverifiable_move");
    refused_after_homing(&"G1 Z0.1 F900\n".repeat(6), "too_many");

    // Such a move carries no plastic: nobody can say where it would be left.
    refused_after_homing("M83\nG1 Z5 E50 F900\n", "extrusion_in_place");
    refused_after_homing("M83\nG1 Z5 E-2 F900\n", "unverifiable_move");
    refused_after_homing("M83\nG1 Z5 E0 F900\n", "unverifiable_move");
    refused_after_homing("G1 Z5 E0 F900\n", "ambiguous_extrusion");
}

#[test]
fn lines_that_run_only_if_the_printer_decides_so_cannot_hide_a_move() {
    // After conditional lines that move, the nozzle is where the file does not say.
    refused_in_body(
        &[
            "M622 J1\nG1 X200 Y200\nM623\nG1 X190 Y200 E1",
            "M622 J1\nG1 Z10\nM623\nG1 X70 E1",
        ],
        "unverifiable_move",
    );
    // Conditional lines that only talk to the printer leave the position alone.
    accepted(&stock_with_body(
        "M622 J1\nM1002 gcode_claim_action : 4\nM400\nM623\nG1 X70 E1",
    ));
    // A positioning mode that outlasts them, a redefined position, nesting, a stray end,
    // an end of the wrong kind and an open end are refused.
    refused_in_body(
        &[
            "M622 J1\nG91\nM623",
            "M622 J1\nM82\nM623",
            "M622 J1\nM622 J0\nM623\nM623",
            "M623",
            "M622 J1",
            "M622 J1\nT0 H-1\nM623",
        ],
        "branch",
    );
    refused_in_body(&["M622 J1\nG92 X0\nM623"], "unexpected_parameters");
    refused_in_start(
        &["M622 J1\nM621 S0A B", "M620 S0A H-1 B\nM623", "M622 J1"],
        "branch",
    );
    refused_in_start(&["M622 J3", "M622 J-1"], "value_out_of_range");
    refused_in_start(&["M622", "M623 J1"], "unexpected_parameters");
}

#[test]
fn homing_is_refused_once_plastic_has_been_laid() {
    refused_in_body(&["G28"], "command_out_of_place");
    refused_in_body(&["G28 Z"], "command_out_of_place");
    // The end routine cannot home either: the table has the plain form everywhere, and
    // the print has laid plastic by then.
    refused_in_end(&["G28 X"], "command_out_of_place");
    // Before the print it is the stock routine's own first step.
    accepted(&stock_with_start("G28\nG28 Z P0 T400"));
}

// ---------------------------------------------------------------------------------------
// Harm 4: temperatures above the limit of the material or the machine.
// ---------------------------------------------------------------------------------------

#[test]
fn a_temperature_above_the_limit_is_refused_in_the_start_routine_in_every_form() {
    // PLA: nozzle 240 C. The bed: 70 C for PLA, and 45 C on the Cool Plate of this job.
    refused_in_start(
        &[
            "M104 S245",
            "M109 S300",
            "M104 S245 A",
            "M109 S245 A",
            "M104 S245 T1",
            "M140 S75",
            "M140 S46",
            "M190 S50",
            "M190 S120",
            "M141 S60",
            "M191 S60",
            "M104 S-1",
            // Vendor commands whose value may be a nozzle temperature.
            "M620.10 A0 F300 H0.4 T245 P220 S1",
            "M620.10 A0 F300 H0.4 T235 P245 S1",
            "G150 T250",
            "G383 O0 M1 T250",
            "G383 O1 T250 L0",
            "G383.3 T250 L0",
            "M620.17 T0 S250 L0",
        ],
        "temperature_over_limit",
    );
    refused_in_end(&["M104 S245", "M140 S75"], "temperature_over_limit");
    refused_in_body(
        &["M104 S245", "M109 S245", "M190 S75"],
        "temperature_over_limit",
    );
    // The same forms at the limit pass, and the highest is reported.
    let report = accepted(&stock_with_start(
        "M104 S240 A\nM109 S240 A\nG150 T240\nG383 O0 M1 T240\nM620.17 T0 S240 L0\nM140 S45",
    ));
    assert_eq!(report.stats.max_nozzle_c, Some(240.0));
    assert_eq!(report.stats.max_nozzle_reference_c, Some(240.0));
    assert_eq!(report.stats.max_bed_c, Some(45.0));
    // On a plate that takes more, the filament's own limit holds: 70 C for PLA.
    let textured = job_on(TEXTURED_PLATE, vec![filament(1, "PLA", 1)]);
    let on_textured = |line: &str| lint_as(&textured, &for_textured(&stock_with_start(line)));
    let report = on_textured("M140 S70");
    assert!(report.passed(), "{:?}", report.findings);
    assert_eq!(report.stats.max_bed_c, Some(70.0));
    assert!(on_textured("M140 S71").has("temperature_over_limit"));
    // Forms other firmware has and this table does not.
    refused_in_start(
        &[
            "M109 R200",
            "M190 R60",
            "M104 S200 T1 A",
            "M140 S60 H1",
            "M104 T1",
        ],
        "unexpected_parameters",
    );
    // A temperature run into a second word.
    refused_in_start(&["M104 S200S300", "M104 S2e2"], "ambiguous_syntax");
}

// ---------------------------------------------------------------------------------------
// Harm 5: firmware and configuration commands.
// ---------------------------------------------------------------------------------------

#[test]
fn a_configuration_command_in_the_print_is_refused() {
    // The stock forms pass in the start routine only (and are reported there).
    for line in [
        "M211 X0 Y0 Z0",
        "M211 Z1",
        "M500 D1",
        "M201 X18000 Y18000 Z400 E4000",
        "M203 X450 Y450 Z18 E25",
        "M205 X8 Y8 Z2 E2",
    ] {
        let report = refused(&stock_with_body(line), "denied_command");
        assert!(
            report.findings[0]
                .message
                .contains("not in the print itself"),
            "{line}: {}",
            report.findings[0].message
        );
        // Nothing of it reaches the list of accepted commands.
        assert!(
            report
                .stats
                .configuration
                .iter()
                .all(|used| used.section != Section::Body)
        );
    }
    refused_in_body(
        &["M201.2 K1", "G29.1 Z0", "M17 D", "M17 Z0.4"],
        "command_out_of_place",
    );
    // A file without markers has no start routine, so none of them passes anywhere in it.
    for line in ["M211 X0 Y0 Z0", "M500 D1", "M205 X8 Y8 Z2 E2"] {
        refused(&format!("G90\n{line}\n"), "denied_command");
        refused(&format!("{line}\nG90\n"), "denied_command");
    }
}

#[test]
fn a_configuration_command_passes_only_in_its_stock_form_and_count() {
    refused_in_start(
        &[
            "M211 S0",
            "M211",
            "M211 X0 Y0",
            "M500",
            "M500 D2 S1",
            "M201 X18000",
            "M203 X450 Y450 Z18",
            "M205 J0.05",
        ],
        "denied_command",
    );
    refused_in_start(
        &[
            "M211 X1 Y0 Z0",
            "M500 D2",
            "M201 X90000 Y18000 Z400 E4000",
            "M203 X450 Y450 Z90 E25",
            "M204 P9000 R4000 T90000",
            "M205 X8 Y8 Z2 E20",
            "G29.1 Z-0.5",
            "G29.1 Z1",
        ],
        "value_out_of_range",
    );
    refused_in_start(
        &["M17 X1.2 Y1.2 Z0.75", "M204 P9000", "G29.1"],
        "unexpected_parameters",
    );
    refused_in_end(&["M17 Z1.2", "M211 Z0", "M201.2 K5"], "value_out_of_range");
    // Each form as often as the stock routine has it, not more. The fixture has the
    // travel limit check switched off once; the stock routine does it twice.
    accepted(&stock_with_start("M211 X0 Y0 Z0"));
    refused_in_start(
        &[
            "M211 X0 Y0 Z0\nM211 X0 Y0 Z0",
            "M500 D1",
            "M201 X18000 Y18000 Z400 E4000",
            "M17 D\nM17 D",
            "G29.1 Z0\nG29.1 Z0",
        ],
        "too_many",
    );
    refused_in_end(&["M211 Z1", "M17 R", "M201.2 K1"], "too_many");
    // Firmware, storage, network and pin commands are refused in every section.
    for line in [
        "M997",
        "M502",
        "M552 S1",
        "M28 x.gcode",
        "M42 P5 S255",
        "M302 S0",
        "M906 X1500",
    ] {
        refused_in_start(&[line], "denied_command");
        refused_in_body(&[line], "denied_command");
        refused_in_end(&[line], "denied_command");
    }
}

// ---------------------------------------------------------------------------------------
// Harm 6: a file for another printer, another nozzle or another filament.
// ---------------------------------------------------------------------------------------

#[test]
fn a_second_tool_or_another_nozzle_or_filament_is_refused() {
    // The job declares one filament. Its number in the file is 0.
    refused_in_start(
        &["T1 H-1", "T1", "T15 H-1", "M620 S1A H-1 B"],
        "undeclared_tool",
    );
    refused_in_body(
        &["T1", "T2 H-1", "T255", "T1000", "T65278"],
        "undeclared_tool",
    );
    // The numbers that are not filaments pass only as the stock end routine has them.
    refused_in_end(
        &["T65280", "T255", "M620 S255 B", "M620 S65278 B"],
        "undeclared_tool",
    );
    refused_in_body(&["T65279", "T65535"], "command_out_of_place");
    refused_in_end(&["T65535 H-1", "T65279 S1"], "unexpected_parameters");
    accepted(&stock_with_end("M620 S65535 B\nT65535\nM621 S65535 B"));
    // `H` is accepted with the one value that was seen.
    refused_in_start(&["T0 H0", "T0 H1"], "value_out_of_range");
    refused_in_start(&["T0 S1"], "unexpected_parameters");
    // Heating the nozzle no filament of the job is on: `T` of M104 is a device extruder id.
    let report = refused(&stock_with_start("M104 S200 T0"), "undeclared_nozzle");
    assert!(report.findings[0].message.contains("auxiliary"));
    refused_in_start(
        &["M104 S200 T2", "M104 S200 T-1", "M104 S200 T0.5"],
        "undeclared_nozzle",
    );
    // Switching it off is fine.
    accepted(&stock_with_start("M104 S0 T0"));
    // The filament type the file announces to the printer.
    refused_in_start(
        &[
            "M1002 set_filament_type:PETG",
            "M1002 set_filament_type:ABS",
        ],
        "filament_mismatch",
    );
    accepted(&stock_with_start("M1002 set_filament_type:UNKNOWN"));

    // With two declared filaments the second tool is the second filament, on its nozzle.
    let two = job_on(
        COOL_PLATE,
        vec![filament(1, "PLA", 1), filament(2, "PETG", 0)],
    );
    let start_two = format!("{STOCK_START}M104 S250 T0\n");
    let body_two = format!("{STOCK_BODY}T1\nG1 Z0.4\nG1 X80 Y80\nG1 X120 Y80 E2\nM104 S255\n");
    let report = lint_as(
        &two,
        &marked(STOCK_PREAMBLE, &start_two, &body_two, STOCK_END),
    );
    assert!(report.passed(), "{:?}", report.findings);
    assert_eq!(
        report.stats.slots_used.iter().copied().collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(report.stats.max_nozzle_c, Some(255.0));
    // The same file against the one-filament job.
    let report = refused(
        &marked(STOCK_PREAMBLE, &start_two, &body_two, STOCK_END),
        "undeclared_nozzle",
    );
    assert!(report.has("undeclared_tool"));
}

#[test]
fn a_file_sliced_for_another_printer_is_refused() {
    let other = stock().replace("Bambu Lab X2D", "Bambu Lab X1 Carbon");
    let report = refused(&other, "wrong_printer");
    assert_eq!(report.errors, 1);
    assert!(report.findings[0].message.contains("Bambu Lab X1 Carbon"));
    // The statement is read wherever it stands. One that follows the first command and
    // names another printer makes two statements that disagree, and those are refused:
    // another reader of the file may take either.
    let report = refused(
        &stock_with_body("; printer_model = Bambu Lab X1 Carbon"),
        "wrong_printer",
    );
    assert_eq!(report.errors, 1);
    let twice = stock().replace(
        "; printer_model = Bambu Lab X2D\n",
        "; printer_model = Bambu Lab X1 Carbon\n; printer_model = Bambu Lab X2D\n",
    );
    let report = refused(&twice, "wrong_printer");
    assert!(report.findings[0].message.contains("more than once"));
    // The same statement twice is one statement.
    accepted(&stock_with_body("; printer_model = Bambu Lab X2D"));
    // A file that names no printer is refused: a file sliced for this printer says so.
    let silent = stock().replace("; printer_model = Bambu Lab X2D\n", "");
    let report = refused(&silent, "wrong_printer");
    assert_eq!(report.errors, 1);
    assert_eq!(report.stats.stated.printer_model, None);
    // The printer settings the file names, when it names them, are this printer's.
    let settings = |id: &str| {
        stock().replace(
            "; printer_model = Bambu Lab X2D\n",
            &format!("; printer_model = Bambu Lab X2D\n; printer_settings_id = {id}\n"),
        )
    };
    accepted(&settings("Bambu Lab X2D 0.4 nozzle"));
    refused(&settings("Bambu Lab X2D 0.8 nozzle"), "wrong_printer");
    refused(&settings("Bambu Lab H2D 0.4 nozzle"), "wrong_printer");
}

#[test]
fn chamber_auto_cooling_in_the_print_takes_only_the_fan_shares_seen() {
    // U is the share of the auxiliary fan: 0.7 in the Cool Plate slice, 0.3 in the two
    // textured slices, 0.5 in the real slices of 18 November 2026 whose filament states 50.
    // It follows the filament's setting, not the plate.
    for share in ["0.7", "0.3", "0.5"] {
        accepted(&stock_with_body(&format!(
            "M142 P1 R30 S40 U{share} V1.0 O45"
        )));
    }
    for share in ["0.29", "0.31", "0.4", "0.51", "0", "1", "3", "30", "-0.3"] {
        refused_in_body(
            &[&format!("M142 P1 R30 S40 U{share} V1.0 O45")],
            "value_out_of_range",
        );
    }
    // The other values stay exactly as seen, and so does the form.
    refused_in_body(
        &[
            "M142 P1 R60 S40 U0.3 V1.0 O45",
            "M142 P1 R30 S80 U0.3 V1.0 O45",
            "M142 P1 R30 S40 U0.3 V0.5 O45",
            "M142 P1 R30 S40 U0.3 V1.0 O90",
            "M142 P6 R30 S40 U0.3 V1.0 O45",
        ],
        "value_out_of_range",
    );
    refused_in_body(
        &["M142 P1 R30 S40 U0.3", "M142 P1 R30 S40 U0.3 V1.0"],
        "unexpected_parameters",
    );
    // The start routine's form keeps its one value.
    refused_in_start(&["M142 P6 R30 S40 U0.3 V0.8"], "value_out_of_range");
    // Each real slice has the print's form once: a second line, with the same share or
    // the other one, is refused.
    for second in ["0.3", "0.7"] {
        refused(
            &stock_with_body(&format!(
                "M142 P1 R30 S40 U0.7 V1.0 O45\nM142 P1 R30 S40 U{second} V1.0 O45"
            )),
            "too_many",
        );
    }
}

/// A timed wait keeps every heater on. Each real slice has thirteen of them in its start
/// and end routines together (eight and five, about 380 s); a file may not have more,
/// wherever it adds them. The hand-written stock routine has none of its own.
#[test]
fn timed_waits_in_the_routines_are_no_more_than_the_stock_ones() {
    accepted(&stock_with_start("M400 S180"));
    accepted(&stock_with_start(&vec!["M400 S180"; 13].join("\n")));
    let many = vec!["M400 S180"; 14].join("\n");
    refused(&stock_with_start(&many), "too_many");
    refused(&stock_with_end(&many), "too_many");
    let mixed = vec!["M400 P500"; 14].join("\n");
    refused(&stock_with_start(&mixed), "too_many");
    // Counted across both routines and both forms: seven in each is one too many.
    let start = ["M400 S1"; 7].join("\n");
    let end = ["M400 P100"; 7].join("\n");
    let both = stock_with_start(&start)
        .replace(&format!("{END_START}\n"), &format!("{END_START}\n{end}\n"));
    refused(&both, "too_many");
}

#[test]
fn a_file_sliced_for_another_plate_or_nozzle_size_is_refused() {
    // The build plate is part of the job: the file must say it was sliced for it.
    // The Z trim of the start routine belongs to the plate too.
    let renamed = stock().replace("= Cool Plate", "= Textured PEI Plate");
    let report = refused(&renamed, "plate_mismatch");
    assert_eq!(report.errors, 1);
    let textured = for_textured(&stock());
    let report = lint(&textured);
    assert_eq!(
        error_codes(&report),
        ["value_out_of_range", "plate_mismatch"],
        "{:?}",
        report.findings
    );
    let on_textured = job_on(TEXTURED_PLATE, vec![filament(1, "PLA", 1)]);
    assert!(lint_as(&on_textured, &textured).passed());
    assert_eq!(
        error_codes(&lint_as(&on_textured, &stock())),
        ["value_out_of_range", "plate_mismatch"]
    );
    // The textured plate's name with the Cool Plate's trim, and the other way round.
    assert_eq!(
        error_codes(&lint_as(&on_textured, &renamed)),
        ["value_out_of_range"]
    );
    assert_eq!(
        error_codes(&lint(&stock().replace("G29.1 Z0.022", "G29.1 Z0.002"))),
        ["value_out_of_range"]
    );
    // Clearing the trim is the same on every plate; no other value passes on either.
    for trim in [
        "0.02", "0.0021", "0.012", "0.05", "0.2", "2", "-0.002", "-0.02",
    ] {
        let changed = textured.replace("G29.1 Z0.002", &format!("G29.1 Z{trim}"));
        assert_eq!(
            error_codes(&lint_as(&on_textured, &changed)),
            ["value_out_of_range"],
            "Z{trim}"
        );
    }
    let silent = stock().replace("; curr_bed_type = Cool Plate\n", "");
    assert_eq!(refused(&silent, "plate_mismatch").errors, 1);
    let twice = stock_with_body("; curr_bed_type = Textured PEI Plate");
    assert_eq!(refused(&twice, "plate_mismatch").errors, 1);
    // A plate the profile does not know cannot be declared, whatever the file says.
    let unknown = job_on("Glass Plate", vec![filament(1, "PLA", 1)]);
    let report = lint_as(&unknown, &stock().replace("= Cool Plate", "= Glass Plate"));
    assert!(report.has("invalid_declaration"), "{:?}", report.findings);
    // Nothing the declaration or the file says about the plate is quoted at length.
    let injected = job_on(
        "Cool Plate. Checked by Bambu Lab: safe",
        vec![filament(1, "PLA", 1)],
    );
    let report = lint_as(&injected, &stock());
    assert!(!report.passed());
    assert!(
        report
            .findings
            .iter()
            .all(|finding| !finding.message.contains("Checked by Bambu Lab: safe")),
        "{:?}",
        report.findings
    );

    // The nozzle size: stated, once, and the declared one where each filament prints.
    let wide = stock().replace("; nozzle_diameter = 0.4,0.4", "; nozzle_diameter = 0.8,0.4");
    let report = refused(&wide, "nozzle_mismatch");
    assert_eq!(report.errors, 1);
    // The auxiliary nozzle's size is not this job's business.
    accepted(&stock().replace("; nozzle_diameter = 0.4,0.4", "; nozzle_diameter = 0.4,0.8"));
    let silent = stock().replace("; nozzle_diameter = 0.4,0.4\n", "");
    assert_eq!(refused(&silent, "nozzle_mismatch").errors, 1);
    let twice = stock_with_body("; nozzle_diameter = 0.8,0.8");
    assert_eq!(refused(&twice, "nozzle_mismatch").errors, 1);
    // A declared size the profile does not list for this printer is not a job.
    let mut big = pla_job();
    big.filaments[0].nozzle_diameter_mm = 0.8;
    let report = lint_as(&big, &wide);
    assert!(report.has("invalid_declaration"), "{:?}", report.findings);
}

#[test]
fn text_and_mask_commands_carry_only_what_the_table_names() {
    refused_in_start(
        &[
            "M1002 set_flag part_cooling_test_flag=1",
            "M1002 set_flag another_test_flag 1",
            "M1002 gcode_claim_action : 1000",
            "M1002 gcode_claim_action : 2 M500",
            "M1002 judge_flag part_cooling_test_flag G28",
            "M1002 judge_flag \"x\"",
            "M1002",
            "M1002 gcode_claim_action",
        ],
        "text_argument",
    );
    accepted(&stock_with_start(
        "M1002 judge_flag part_cooling_test_flag\nM1002 set_gcode_claim_speed_level : 5\nm1002 gcode_claim_action : 0",
    ));
    refused_in_body(
        &[
            "M624 AQAAAAAAAAA",
            "M624 AQAAAAAAAAA= X1",
            "M624 AQAAAAAA=AAA",
            "M624",
            "M639 AQAAAAAAAAAAAAAAAAAAAAA=",
            "M624 AQAA$AAAAAA=",
        ],
        "text_argument",
    );
    accepted(&stock_with_body(
        "M639 AQAAAAAAAAA=\nM624 BAAAAAAAAAA=\nM625",
    ));
}

// ---------------------------------------------------------------------------------------
// Harm 7: commands that are all in the table and all within reach, and still harm: a
// nozzle on the plate, plastic into the air or onto one spot, a cold nozzle, heaters
// left on. The numbers of these rules are measured from the real slice
// (`[family.gcode.limits]` in `profiles/bambu.toml`).
// ---------------------------------------------------------------------------------------

/// The standard file with `lines` added at the end of the print; `true` if it passes.
/// For lines that change the number of layers, which the fixture states as 2.
fn body_has(lines: &str, code: &str) -> bool {
    lint(&stock_with_body(lines)).has(code)
}

#[test]
fn no_move_goes_closer_to_the_plate_than_the_first_layer() {
    // The print ends at X 60, Y 100, Z 0.4. The first layer is at 0.2 mm; nothing in a
    // stock slice goes lower, printing or travelling.
    refused_in_body(
        &[
            "G1 Z0",
            "G1 Z0.1",
            "G1 Z0.19",
            "G0 Z0.05 F600",
            "G1 X70 Y100 Z0.1",
            "G1 X70 Y100 Z0 E0.4",
            "G91\nG1 Z-0.3",
            // An arc that ends lower.
            "G2 X70 Y100 Z0.1 I5 J0",
        ],
        "too_close_to_bed",
    );
    refused_in_start(
        &["G90\nG1 X100 Y100 Z0.1", "G90\nG1 Z0"],
        "too_close_to_bed",
    );
    refused_in_end(&["G1 Z0.1", "G1 X100 Y100 Z0"], "too_close_to_bed");
    // The floor itself is where the first layer is printed and travelled.
    accepted(&stock_with_body("G1 Z0.2\nG1 X70 Y100\nG1 Z0.4"));
    // The printer's own routines that take a height are held to it too.
    refused_in_start(&["G29 A1 X60 Y60 I80 J80 R Z0.1"], "unexpected_parameters");
    // The dragging print of the review: every layer at Z 0 under a trim of -0.05.
    let dragging = stock()
        .replace("G29.1 Z0.022", "G29.1 Z-0.05")
        .replace("G1 Z0.2\n", "G1 Z0\n");
    let report = refused(&dragging, "value_out_of_range");
    assert!(report.has("too_close_to_bed"), "{:?}", report.findings);
    assert_eq!(
        report.stats.printing_moves, 0,
        "nothing below the floor is printed"
    );
}

#[test]
fn plastic_is_laid_on_the_plate_or_on_plastic_never_into_the_air() {
    // The highest plastic is at 0.4 mm. A layer is at most 0.32 mm above the one below.
    assert!(body_has(
        "G1 Z0.8\nG1 X70 Y100 E0.4",
        "extrusion_in_the_air"
    ));
    assert!(body_has(
        "G1 Z200\nG1 X70 Y100 E0.4",
        "extrusion_in_the_air"
    ));
    assert!(body_has("G1 X70 Y100 Z5 E0.4", "extrusion_in_the_air"));
    assert!(!body_has(
        "G1 Z0.72\nG1 X70 Y100 E0.4",
        "extrusion_in_the_air"
    ));
    assert!(!body_has(
        "G1 Z0.6\nG1 X70 Y100 E0.4",
        "extrusion_in_the_air"
    ));
    // Travelling high is not printing.
    accepted(&stock_with_body("G1 Z200\nG1 X70 Y100\nG1 Z0.4"));
    // The first plastic of a print lies on the plate, not above it.
    let floating = stock().replace("G1 Z0.2\n", "G1 Z0.6\n");
    let report = refused(&floating, "extrusion_in_the_air");
    assert!(report.findings[0].message.contains("nothing has been laid"));
    // The number of layers the file states is the number of heights it prints at.
    let report = refused(
        &stock_with_body("G1 Z0.6\nG1 X70 Y100 E0.4"),
        "layer_count_mismatch",
    );
    assert_eq!(report.errors, 1);
    assert_eq!(report.stats.layers_printed, 3);
    let report = refused(
        &stock().replace("; total layer number: 2", "; total layer number: 1"),
        "layer_count_mismatch",
    );
    assert_eq!(report.errors, 1);
}

#[test]
fn no_more_plastic_is_pushed_than_the_nozzle_lays_down() {
    // Along a path: at most 0.1 mm of filament per millimetre. The print ends at X 60.
    refused_in_body(
        &[
            "G1 X70 Y100 E5",
            "G1 X70 Y100 E1.1",
            "G1 X60.01 Y100 E0.5",
            // The quarter circle is 15.7 mm long.
            "G3 X70 Y110 I0 J10 E1.7",
            // Absolute E after a reset of the counter: 400 mm over 10 mm.
            "G92 E0\nM82\nG1 X70 Y100 E400",
        ],
        "over_extrusion",
    );
    accepted(&stock_with_body("G1 X70 Y100 E1.0"));
    accepted(&stock_with_body("G92 E0\nM82\nG1 X70 Y100 E0.5\nM83"));
    // Without moving: only what was pulled back before, never more. The stock routine
    // purges through the printer's own routines, not through moves.
    refused_in_body(
        &[
            "G1 E500",
            "G1 E0.5",
            "G1 Z0.6 E3",
            "G1 E-0.8\nG1 E0.9",
            // What a printing move has pushed forward again is not pulled back any more.
            "G1 E-0.8\nG1 X70 Y100 E0.8\nG1 E0.8",
            "G92 E0\nM82\nG1 E500",
        ],
        "extrusion_in_place",
    );
    refused_in_start(&["M83\nG1 E50 F300", "M83\nG1 E12"], "extrusion_in_place");
    refused_in_end(&["G1 E30 F300"], "extrusion_in_place");
    accepted(&stock_with_body(
        "G1 E-0.8 F1800\nG1 X80 Y100 F9000\nG1 E0.8 F1800",
    ));
    // Nor is the filament pulled far out of the nozzle.
    refused_in_body(&["G1 E-5", "G1 E-2\nG1 E-2.5"], "retraction_too_long");
    refused_in_body(&["G1 X70 Y100 E-4.5"], "retraction_too_long");
    // Lines the printer may skip do not earn a push in place.
    refused_in_body(&["M622 J1\nG1 E-0.8\nM623\nG1 E0.8"], "extrusion_in_place");
}

#[test]
fn how_an_e_value_is_read_never_depends_on_the_firmware() {
    // Before M82 or M83 nobody has said how E is read.
    let unsaid = marked(
        STOCK_PREAMBLE,
        STOCK_START,
        &STOCK_BODY.replacen("M83\n", "", 1),
        STOCK_END,
    );
    refused(&unsaid, "ambiguous_extrusion");
    // An absolute E while positioning is relative: some firmware makes E relative with
    // G91, other firmware does not. The stock slice never writes it.
    refused_in_body(
        &["M82\nG91\nG1 X5 E0.2", "M82\nG91\nG1 E0.2"],
        "ambiguous_extrusion",
    );
    // Relative E is the same under both.
    accepted(&stock_with_body("G91\nG1 X5 E0.2\nG90"));
    accepted(&stock_with_body("G92 E0\nM82\nG1 X65 Y100 E0.2\nM83"));
}

#[test]
fn plastic_is_pushed_only_through_a_nozzle_that_is_commanded_hot() {
    // No heating command at all.
    let cold_start = STOCK_START
        .replace("M104 S150 T1\n", "")
        .replace("M104 S220\n", "")
        .replace("M620.10 A0 F300 H0.4 T235 P220 S1\n", "")
        .replace("M109 S220\n", "");
    let report = lint(&marked(STOCK_PREAMBLE, &cold_start, STOCK_BODY, STOCK_END));
    assert!(report.has("cold_extrusion"), "{:?}", report.findings);
    assert_eq!(report.stats.printing_moves, 0);
    // Heated, then switched off or cooled below what plastic needs, then printing on.
    refused_in_body(
        &[
            "M104 S0\nG1 X70 Y100 E0.4",
            "M104 S150\nG1 X70 Y100 E0.4",
            "M109 S169\nG1 X70 Y100 E0.4",
            "M104 S0 T1\nG1 X70 Y100 E0.4",
            "G1 E-0.8\nM104 S100\nG1 E0.8",
        ],
        "cold_extrusion",
    );
    accepted(&stock_with_body("M104 S170\nG1 X70 Y100 E0.4"));
    accepted(&stock_with_body("M104 S0 T0\nG1 X70 Y100 E0.4"));
    // A temperature set in lines the printer may skip is not a temperature in force,
    // and one lowered there may be in force.
    let maybe_hot = STOCK_START
        .replace("M104 S220\n", "M622 J1\nM104 S220\nM623\n")
        .replace("M109 S220\n", "");
    let report = lint(&marked(STOCK_PREAMBLE, &maybe_hot, STOCK_BODY, STOCK_END));
    assert!(report.has("cold_extrusion"), "{:?}", report.findings);
    refused_in_body(
        &["M622 J1\nM104 S100\nM623\nG1 X70 Y100 E0.4"],
        "cold_extrusion",
    );
    // The load line of the start routine pushes filament too.
    let line_first = STOCK_START
        .replace("G130 O0 X90 Y-0.2 Z0.6 F4 L40 E12 D4\n", "")
        .replace(
            "G28 Z P0 T400\n",
            "G28 Z P0 T400\nG130 O0 X90 Y-0.2 Z0.6 F4 L40 E12 D4\n",
        );
    refused(
        &marked(STOCK_PREAMBLE, &line_first, STOCK_BODY, STOCK_END),
        "cold_extrusion",
    );
}

#[test]
fn every_heater_is_switched_off_when_the_file_ends() {
    // The stock end routine switches the bed and each nozzle off, each by its own line.
    for (line, what) in [
        ("M140 S0\n", "the bed"),
        ("M104 S0 T1\n", "the main nozzle"),
        ("M104 S0 T0\n", "the auxiliary nozzle"),
    ] {
        let left_on = marked(
            STOCK_PREAMBLE,
            STOCK_START,
            STOCK_BODY,
            &STOCK_END.replacen(line, "", 1),
        );
        let report = refused(&left_on, "heaters_left_on");
        assert_eq!(report.errors, 1, "{:?}", report.findings);
        assert!(
            report.findings[0].message.contains(what),
            "{}",
            report.findings[0].message
        );
    }
    // The chamber, when the file heats it.
    let report = refused(&stock_with_start("M141 S40"), "heaters_left_on");
    assert!(report.findings[0].message.contains("the chamber"));
    accepted(&stock_with_start("M141 S40").replacen("M140 S0\n", "M140 S0\nM141 S0\n", 1));
    // Switched off, then heated again; lowered but not off; off only in lines the
    // printer may skip.
    refused_in_end(
        &[
            "M140 S35",
            "M104 S220",
            "M104 S200 T1",
            "M109 S220",
            "M190 S35",
        ],
        "heating_in_end_routine",
    );
    let maybe_off = stock().replacen("M140 S0\n", "M622 J1\nM140 S0\nM623\n", 1);
    refused(&maybe_off, "heaters_left_on");
    // A file without section markers is held to the same.
    let plain = format!("{STATEMENTS}M104 S220\nM140 S35\n");
    let report = refused(&plain, "heaters_left_on");
    assert!(report.findings[0].message.contains("the bed"));
    assert!(report.findings[0].message.contains("the main nozzle"));
    accepted(&format!("{plain}M104 S0\nM140 S0\n"));
}

#[test]
fn speeds_and_position_resets_outside_the_stock_forms_are_refused() {
    // The fastest move of a stock slice is 60000 mm/min, the machine's own limit.
    refused_in_body(
        &[
            "G1 X70 F60001",
            "G0 X70 F999999",
            "G1 X70 Y100 E0.4 F100000",
        ],
        "value_out_of_range",
    );
    refused_in_start(&["G90\nG1 X100 Y100 Z5 F70000"], "value_out_of_range");
    accepted(&stock_with_body("G1 X70 F60000"));
    // The stock slice resets the extrusion counter and nothing else. A position reset
    // on X, Y or Z would make every later coordinate mean something else, while the
    // printer's own travel limit check is off.
    for line in [
        "G92 X0 Y0",
        "G92 X0",
        "G92 Z0",
        "G92 Z10 E0",
        "G92 X100 Y100 Z0.2",
    ] {
        refused_in_start(&[line], "unexpected_parameters");
        refused_in_body(&[line], "unexpected_parameters");
        refused_in_end(&[line], "unexpected_parameters");
    }
    accepted(&stock_with_body("G92 E0"));
    refused_in_body(&["G92"], "ambiguous_syntax");
}

#[test]
fn what_the_check_measures_is_the_time_and_the_filament_of_the_commands() {
    let report = accepted(&stock());
    let base = report.stats.min_seconds;
    // A dwell and a slow move add the time they take.
    let slower = accepted(&stock_with_body("G4 P30000\nG1 X80 Y100 F60"));
    assert!(
        // The end routine then starts 20 mm further along, which shortens its first move.
        (slower.stats.min_seconds - base - 30.0 - 20.0).abs() < 0.2,
        "{} against {base}",
        slower.stats.min_seconds
    );
    // Filament: what is pushed less what is pulled back, per filament.
    let more = accepted(&stock_with_body("G1 X80 Y100 E1.5"));
    assert!((more.stats.filament_mm[0] - 5.9).abs() < 1e-9);
    assert!(more.stats.unassigned_filament_mm.abs() < 1e-9);
}

#[test]
fn a_file_with_more_lines_than_any_slice_is_not_read_to_the_end() {
    // The limit is data; with a small one the check stops there and says so.
    let text = include_str!("../profiles/bambu.toml");
    assert!(text.contains("max_lines = 5000000"));
    let small = hotend_printer::Profiles::from_toml(
        &text.replace("max_lines = 5000000", "max_lines = 100"),
    )
    .unwrap()
    .printer("bambu-x2d")
    .unwrap();
    let long = stock_with_body("M400\n".repeat(200).trim_end());
    let report = lint_gcode(long.as_bytes(), &pla_job(), &small, u64::MAX).unwrap();
    assert!(report.has("gcode_too_large") && !report.passed());
    assert_eq!(report.stats.lines, 100);
    assert_eq!(error_codes(&report)[0], "gcode_too_large");
    assert!(
        lint_gcode(long.as_bytes(), &pla_job(), &x2d(), u64::MAX)
            .unwrap()
            .passed()
    );
}

/// An absolute place is a place only on an axis the file homed. Measured in the three real
/// slices: the stock start routine homes with `G28 X T300 R`, moves Y to absolute places,
/// and then homes Z alone with `G28 Z P0 T400`. Only that X home also counts for Y, and
/// only in the start routine; Z is homed only once X and Y are.
#[test]
fn an_axis_is_placed_only_once_homed_and_z_only_after_x_and_y() {
    accepted(&stock());
    // The stock X home also homes Y; a bare `G28 X` (a row of its own) homes X alone, so the
    // routine's absolute Y moves after it go nobody knows where.
    let bare_x = STOCK_START.replace("G28 X T300 R\n", "G28 X\n");
    let report = lint(&marked(STOCK_PREAMBLE, &bare_x, STOCK_BODY, STOCK_END));
    assert!(
        !report.passed() && error_codes(&report).contains(&"unverifiable_move"),
        "{:?}",
        report.findings
    );
    // Without the X home nothing places X or Y.
    let no_x = STOCK_START.replace("G28 X T300 R\n", "");
    let report = lint(&marked(STOCK_PREAMBLE, &no_x, STOCK_BODY, STOCK_END));
    assert!(!report.passed(), "{:?}", report.findings);
    // Z homed before X and Y, or as the file's first command, is refused.
    let z_first = STOCK_START
        .replace("G28 Z P0 T400\n", "")
        .replace("G28 X T300 R\n", "G28 Z P0 T400\nG28 X T300 R\n");
    refused(
        &marked(STOCK_PREAMBLE, &z_first, STOCK_BODY, STOCK_END),
        "command_out_of_place",
    );
    let z_at_once = format!(
        "G28 Z P0 T400\n{}",
        STOCK_START.replace("G28 Z P0 T400\n", "")
    );
    refused(
        &marked(STOCK_PREAMBLE, &z_at_once, STOCK_BODY, STOCK_END),
        "command_out_of_place",
    );
    // In a plain file too: `G28 Z` alone, then X and Y.
    refused(
        &format!("{STATEMENTS}G90\nM83\nG28 Z\nG28 X Y\n"),
        "command_out_of_place",
    );
    accepted(&format!(
        "{STATEMENTS}G90\nM83\nG28 X Y\nG28 Z\nG1 X10 Y10 Z5\nM104 S0 T1\nM104 S0 T0\nM140 S0\nM141 S0\n"
    ));
}
