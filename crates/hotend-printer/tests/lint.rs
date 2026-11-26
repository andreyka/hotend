//! The G-code check on handcrafted G-code without section markers, including attempts to
//! get past it. In such a file every line is held to the rows of the print itself. Files
//! in the shape Bambu Studio writes are in `stock_routine.rs`.
#![allow(clippy::unwrap_used)]

mod common;

use common::{
    COOL_PLATE, GOOD_FILAMENT_MM, GOOD_GCODE, STATEMENTS, TEXTURED_PLATE, filament, job_on,
    pla_job, two_nozzle_job, x2d,
};
use hotend_printer::lint::{LintReport, MAX_FINDINGS, lint_gcode};
use hotend_printer::{DeclaredJob, Severity};

/// Homed, absolute, at (50, 50, 0.2), PLA selected and its nozzle at 220 C: a known
/// state to start from.
const READY: &str = "G90\nM83\nG28\nT0\nM104 S220\nG1 Z0.2 F600\nG1 X50 Y50 F9000\n";
/// Every heater off, as a file must end.
const OFF: &str = "M104 S0 T1\nM104 S0 T0\nM140 S0\nM141 S0\n";
/// The line number of the first line after [`STATEMENTS`] and [`READY`].
const FIRST_LINE: u64 = 11;

fn lint_as(job: &DeclaredJob, gcode: &str) -> LintReport {
    lint_gcode(gcode.as_bytes(), job, &x2d(), u64::MAX).unwrap()
}

fn lint(gcode: &str) -> LintReport {
    lint_as(&pla_job(), gcode)
}

/// Lints `gcode` between what a file says it was sliced for and [`READY`] in front and
/// [`OFF`] behind, so that nothing but `gcode` can be wrong with it.
fn after_ready(gcode: &str) -> LintReport {
    lint(&whole(gcode))
}

fn whole(gcode: &str) -> String {
    format!("{STATEMENTS}{READY}{gcode}\n{OFF}")
}

fn error_codes(report: &LintReport) -> Vec<&'static str> {
    report
        .findings
        .iter()
        .filter(|finding| finding.severity == Severity::Error)
        .map(|finding| finding.code)
        .collect()
}

#[track_caller]
fn refused(gcode: &str, code: &str) {
    let report = after_ready(gcode);
    assert!(
        !report.passed() && error_codes(&report).contains(&code),
        "{gcode:?} should be refused as {code}, got {:?}",
        report.findings
    );
}

#[track_caller]
fn accepted(gcode: &str) {
    let report = after_ready(gcode);
    assert!(report.passed(), "{gcode:?}: {:?}", report.findings);
}

#[test]
fn a_clean_print_passes_and_is_measured() {
    let report = lint(GOOD_GCODE);
    assert!(report.passed(), "{:?}", report.findings);
    assert_eq!(report.warnings, 0);
    let stats = &report.stats;
    assert_eq!(stats.max_nozzle_c, Some(220.0));
    assert_eq!(stats.max_bed_c, Some(35.0));
    assert_eq!(stats.max_chamber_c, None);
    assert_eq!(stats.total_layers, Some(3));
    assert_eq!(stats.first_remaining_minutes, Some(12));
    assert_eq!(stats.slots_used.iter().copied().collect::<Vec<_>>(), [1]);
    assert_eq!(stats.printing_moves, 5);
    let extent = stats.extent.unwrap();
    assert_eq!(extent.min, [50.0, 50.0, 0.2]);
    // The counter-clockwise arc from (100, 50) to (110, 60) stays within x <= 110.
    assert_eq!(extent.max, [110.0, 100.0, 0.6]);
    // Measured from the commands: the layers, the filament and the least time.
    assert_eq!(stats.layers_printed, 3);
    assert!((stats.filament_mm[0] - GOOD_FILAMENT_MM).abs() < 1e-9);
    assert!(
        stats.min_seconds > 5.0 && stats.min_seconds < 60.0,
        "{}",
        stats.min_seconds
    );
    assert_eq!(stats.stated.printer_model.as_deref(), Some("Bambu Lab X2D"));
    assert_eq!(stats.stated.plate_type.as_deref(), Some("Cool Plate"));
}

#[test]
fn moves_outside_the_reach_are_refused() {
    refused("G1 X300 Y10", "out_of_reach");
    refused("G1 X256.01", "out_of_reach");
    refused("G1 Y-0.5", "out_of_reach");
    refused("G1 Z260.5", "out_of_reach");
    refused("G0 X1000 F9000", "out_of_reach");
    accepted("G1 X256 Y256 Z260");
    accepted("G1 X0 Y0");
    // The main nozzle may travel 0.2 mm in front of the plate; it may not print there.
    accepted("G1 Y-0.2");
    refused("G1 Y-0.2\nG1 X80 E1", "extrusion_outside_reach");
    // The guarded move of the start routine is not a command of the print.
    refused("G91\nG380 S2 Z5 F300", "command_out_of_place");
}

#[test]
fn lowercase_comments_line_numbers_and_checksums_do_not_hide_a_command() {
    refused("g1 x300 y10", "out_of_reach");
    refused("G1 X300 ; a perfectly safe move", "out_of_reach");
    accepted("G1 X10 ; G1 X300");
    accepted(";G1 X300\n ; M500");
    refused("N10 G1 X300*71", "out_of_reach");
    refused("N11 M500*12", "denied_command");
    refused("n12 m997", "denied_command");
    accepted("N13 G1 X20*5");
    refused("G1 X20*zz", "ambiguous_syntax");
    // A line number glued to the command is not something to guess at.
    refused("N10G1 X300", "unknown_command");
    refused("G1X300", "unknown_command");
}

#[test]
fn relative_moves_are_followed_and_the_position_is_never_redefined() {
    accepted("G91\nG1 X100\nG1 X100");
    refused("G91\nG1 X100\nG1 X100\nG1 X100", "out_of_reach");
    refused("G91\nG1 Y-60", "out_of_reach");
    // The position is never redefined on X, Y or Z, whatever the values: the stock
    // slice resets the extrusion counter only.
    refused("G1 X200\nG92 X0\nG1 X100", "unexpected_parameters");
    refused("G1 X200\nG92 X0\nG1 X50", "unexpected_parameters");
    refused("G92 X0 Y0", "unexpected_parameters");
    refused("G92 Z5", "unexpected_parameters");
    refused("G92 X50 Y50 Z0.2 E0", "unexpected_parameters");
    accepted("G92 E0");
    refused("G92", "ambiguous_syntax");
    refused("G28\nG92 X0", "unexpected_parameters");
    // A relative move from there cannot be placed. Outside the start routine of a file
    // with section markers it is refused, however short, and however moves are split up.
    refused("G28\nG91\nG1 Z5", "unverifiable_move");
    refused("G28\nG91\nG1 Z500", "unverifiable_move");
    refused("G28\nG91\nG1 Z30.5", "unverifiable_move");
    refused("G28\nG91\nG1 Z20\nG1 Z-5\nG1 Z16", "unverifiable_move");
    refused("G28\nG91\nG1 Z20\nG1 Z-30\nG1 Z-1", "unverifiable_move");
    refused("G28 X\nG91\nG1 X10 E1", "unverifiable_move");
    // So is it after a tool change: the printer moves the nozzle to change filament.
    refused("G1 X60 E1\nT0\nG1 X70 E1", "unverifiable_move");
    accepted("G1 X60 E1\nT0\nG1 Z0.2\nG1 X60 Y50\nG1 X70 E1");
    // Homing one axis leaves the others known.
    accepted("G28 Z\nG1 Z0.2\nG91\nG1 X10 E1");
}

#[test]
fn arcs_are_checked_over_their_whole_path() {
    // Both ends are inside; the counter-clockwise half circle bulges to x = 259.
    refused("G1 X254 Y100\nG3 X254 Y110 I0 J5", "out_of_reach");
    // The clockwise half goes the other way round and stays inside.
    accepted("G1 X254 Y100\nG2 X254 Y110 I0 J5");
    // A full circle around a point near the edge.
    refused("G1 X250 Y100\nG2 I5 J0", "out_of_reach");
    accepted("G1 X240 Y100\nG2 I5 J0");
    // Radius form: the short way stays inside, the long way does not.
    accepted("G1 X250 Y100\nG3 X250 Y110 R5.5");
    refused("G1 X250 Y100\nG3 X250 Y110 R-9", "out_of_reach");
    refused("G1 X100 Y100\nG2 X150 Y100 R10", "malformed_arc");
    refused("G1 X100 Y100\nG2 X150 Y100 I5 J5", "malformed_arc");
    refused("G1 X100 Y100\nG2 X110 Y100", "malformed_arc");
    refused("G1 X100 Y100\nG2 X110 Y100 I5 J0 R5", "malformed_arc");
    refused("G28\nG2 X10 Y10 I5 J0", "unverifiable_move");
    refused("G1 X10 Y10 I5 J5", "unexpected_parameters");
    // A turn count is accepted as 1 only, and then the nozzle goes all the way round.
    accepted("G1 X240 Y100\nG2 X240 Y110 I0 J5 P1");
    refused("G1 X254 Y100\nG2 X254 Y110 I0 J5 P1", "out_of_reach");
    refused("G1 X240 Y100\nG2 X240 Y110 I0 J5 P2", "value_out_of_range");
    // Other planes are not on the allowlist.
    refused("G18", "unknown_command");
}

#[test]
fn several_commands_on_one_line_are_refused() {
    refused("G1 X10 Y10 G1 X300", "multiple_commands");
    refused("G1 X10 M500", "multiple_commands");
    refused("M400 M500", "multiple_commands");
    refused("G90 G1 X300", "multiple_commands");
    refused("M104 S200 M104 S300", "multiple_commands");
    refused("T0 G1 X300", "multiple_commands");
    refused("T0 M500", "multiple_commands");
    refused("G1 X10 T1", "multiple_commands");
    refused("M106 S255 T0", "multiple_commands");
    // A lone carriage return is a line break to some firmware.
    refused("G1 X10\rG1 X300\rG1 X20", "out_of_reach");
    assert!(lint(&whole("G1 X10").replace('\n', "\r\n")).passed());
    // A letter that is a parameter of one command is not a parameter of every command:
    // `T` names a nozzle on M104 and the tool takes `H`.
    accepted("M104 S200 T1\nT0 H-1");
    refused("M109 S200 T1", "multiple_commands");
    refused("M104 S200 T1 T1", "ambiguous_syntax");
}

#[test]
fn numbers_a_firmware_could_read_differently_are_refused() {
    refused("G1 X1e3", "ambiguous_syntax");
    refused("G1 X1E3 Y5", "ambiguous_syntax");
    refused("G1 Y0x10", "ambiguous_syntax");
    refused("G1 X10Y300", "ambiguous_syntax");
    refused("G1 X 300", "ambiguous_syntax");
    refused("G1 X10 X300", "ambiguous_syntax");
    refused("G1 X1.2.3", "malformed_number");
    refused("G1 X--5", "malformed_number");
    refused("G1 X", "malformed_number");
    refused("G1 X10 F0", "malformed_number");
    refused("M104 S", "malformed_number");
    refused("G1 X10 (G1 X300)", "ambiguous_syntax");
    refused("G1 X10 \u{0} Y5", "ambiguous_syntax");
    refused("G1 X１０", "ambiguous_syntax");
    refused("G01.5 X10", "unknown_command");
    accepted("G01 X10\nG1 X+10.50 Y-0");
}

#[test]
fn firmware_configuration_eeprom_network_and_unknown_commands_are_refused() {
    for (gcode, category) in [
        ("M500", "eeprom"),
        ("M502", "eeprom"),
        ("M997", "firmware"),
        ("M999", "firmware"),
        ("M92 X80", "configuration"),
        ("M301 P20", "configuration"),
        ("M302 S0", "configuration"),
        ("M211 S0", "configuration"),
        ("M211 X0 Y0 Z0", "configuration"),
        ("M500 D1", "eeprom"),
        ("M201 X20000 Y20000 Z500 E30000", "configuration"),
        ("M203 X500", "configuration"),
        ("M205 X9 Y9 Z3 E2.5", "configuration"),
        ("M906 X1500", "configuration"),
        ("M552 S1", "network"),
        ("M587 S\"net\"", "network"),
        ("M28 evil.gcode", "storage"),
        ("M23 other.gcode", "storage"),
        ("M30 plate.gcode", "storage"),
        ("M42 P5 S255", "pins"),
    ] {
        let report = after_ready(gcode);
        let finding = report
            .findings
            .iter()
            .find(|finding| finding.code == "denied_command")
            .unwrap_or_else(|| panic!("{gcode}: {:?}", report.findings));
        assert!(
            finding.message.contains(category),
            "{gcode}: {}",
            finding.message
        );
        assert_eq!(finding.line, FIRST_LINE);
        assert!(finding.excerpt.starts_with(&gcode[..4]));
    }
    for gcode in [
        "M9999",
        "G5 X10",
        "M0",
        "M600",
        "EXCLUDE_OBJECT_START NAME=a",
        "SET_VELOCITY_LIMIT VELOCITY=9999",
        "X300",
        "M",
        "G",
        "T",
        "T-1",
        "{if 1}G1 X300{endif}",
        "$H",
        "M900 K0.02 L1000 M10",
        "M109.1 S200",
    ] {
        refused(gcode, "unknown_command");
    }
    // Commands of the printer's own routines have no place in a file without them.
    for gcode in [
        "G29.1 Z0",
        "M17 D",
        "G150.1",
        "M620.10 A0 F300 H0.4 T235 P220 S1",
    ] {
        refused(gcode, "command_out_of_place");
    }
}

#[test]
fn temperatures_are_held_to_the_declared_filament_and_the_machine() {
    accepted("M104 S240\nM109 S240\nM140 S45\nM190 S45\nM141 S45");
    // The bed: the Cool Plate of this job takes 45 C, whatever the filament allows.
    refused("M140 S46", "temperature_over_limit");
    refused("M140 S70", "temperature_over_limit");
    refused("M190 S60", "temperature_over_limit");
    // On a plate that takes more, the filament's limit holds: 70 C for PLA.
    let textured = job_on(TEXTURED_PLATE, vec![filament(1, "PLA", 1)]);
    let on_textured = |gcode: &str| {
        lint_as(
            &textured,
            &whole(gcode).replace("= Cool Plate", "= Textured PEI Plate"),
        )
    };
    let report = on_textured("M140 S70\nM190 S60");
    assert!(report.passed(), "{:?}", report.findings);
    assert!(on_textured("M140 S71").has("temperature_over_limit"));
    assert!(on_textured("M190 S71").has("temperature_over_limit"));
    refused("M104 S241", "temperature_over_limit");
    refused("m109 s300", "temperature_over_limit");
    // The wait-while-cooling form of other firmware is not in the table.
    refused("M109 R260", "unexpected_parameters");
    refused("M190 R60", "unexpected_parameters");
    refused("M140 S110", "temperature_over_limit");
    refused("M190 S71", "temperature_over_limit");
    refused("M141 S60", "temperature_over_limit");
    refused("M191 S65", "temperature_over_limit");
    refused("M104 S-5", "temperature_over_limit");
    refused("M104 S220 T1 M500", "multiple_commands");
    // `T` of M104 names a nozzle by device extruder id: 1 is the main nozzle, where the
    // PLA is; 0 is the auxiliary nozzle, which this job does not use.
    accepted("M104 S220 T1");
    refused("M104 S241 T1", "temperature_over_limit");
    refused("M104 S220 T0", "undeclared_nozzle");
    refused("M104 S220 T2", "undeclared_nozzle");
    accepted("M104 S0 T0");

    // ABS may go hotter than PLA, but never past the machine.
    let abs = job_on(TEXTURED_PLATE, vec![filament(1, "ABS", 1)]);
    assert!(!lint_as(&abs, "M104 S280\nM140 S100\nM141 S65\n").has("temperature_over_limit"));
    assert!(lint_as(&abs, "M104 S295\n").has("temperature_over_limit"));
    let pc = job_on(TEXTURED_PLATE, vec![filament(1, "PC", 1)]);
    assert!(!lint_as(&pc, "M104 S300\n").has("temperature_over_limit"));
    assert!(lint_as(&pc, "M104 S301\n").has("temperature_over_limit"));

    // With two filaments the active one decides; before a tool change, the higher limit.
    let two = two_nozzle_job();
    assert!(!lint_as(&two, "M104 S260\n").has("temperature_over_limit"));
    assert!(lint_as(&two, "T0\nM104 S260\n").has("temperature_over_limit"));
    assert!(!lint_as(&two, "T1\nM104 S260\n").has("temperature_over_limit"));
    assert!(lint_as(&two, "T1\nM104 S275\n").has("temperature_over_limit"));
    // A nozzle named on the line is held to the filament on it, whichever is active.
    assert!(!lint_as(&two, "T0\nM104 S260 T0\n").has("temperature_over_limit"));
    assert!(lint_as(&two, "T1\nM104 S260 T1\n").has("temperature_over_limit"));
}

#[test]
fn tool_changes_follow_the_declared_filament_map() {
    refused("T1", "undeclared_tool");
    refused("T7", "undeclared_tool");
    // No number is special in the print: the two the stock end routine uses pass only
    // there, and the ones older notes list were never seen.
    refused("T255", "undeclared_tool");
    refused("T1000", "undeclared_tool");
    refused("T65279", "command_out_of_place");
    refused("T65535", "command_out_of_place");
    refused("T4294967296", "unknown_command");
    accepted("T0\nT0 H-1\nt0");

    let two = two_nozzle_job();
    let ready = "; printer_model = Bambu Lab X2D\n; curr_bed_type = Textured PEI Plate\n; nozzle_diameter = 0.4,0.4\nG90\nM83\nG28\nM104 S220 T1\nM104 S250 T0\n";
    // A tool change leaves the nozzle where the file does not say, so each case says
    // where it is afterwards.
    let run = |gcode: &str| {
        let placed = gcode.replace("@", "G1 Z0.2\nG1 X50 Y50");
        lint_as(&two, &format!("{ready}{placed}\n{OFF}"))
    };
    // Filament 2 is on the auxiliary nozzle (device id 0), which starts at X 20.5 and
    // ends at Z 256.
    let report = run("T0\n@\nG1 X10 Y10 E1");
    assert!(report.passed(), "{:?}", report.findings);
    let report = run("T1\n@\nG1 X30 Y10 E1");
    assert!(report.passed(), "{:?}", report.findings);
    let report = run("T1\n@\nG1 X10 Y10");
    assert!(report.has("out_of_reach"), "{:?}", report.findings);
    assert!(report.findings[0].message.contains("auxiliary"));
    assert!(run("T0\n@\nG1 Z258").passed());
    assert!(run("T1\n@\nG1 Z258").has("out_of_reach"));
    // The travel box in front of the plate was measured for the main nozzle only.
    assert!(run("T0\n@\nG1 Y-0.2").passed());
    assert!(run("T1\n@\nG1 Y-0.2").has("out_of_reach"));
    // Plastic is not laid from where the other nozzle stood.
    assert!(run("T0\n@\nG1 X10 Y10\nT1\nG1 X40 Y10 E2").has("unverifiable_move"));
    // Before any tool change a move must fit both nozzles.
    assert!(run("@\nG1 X10").has("out_of_reach"));
    assert!(run("@\nG1 Y-0.2").has("out_of_reach"));
    assert!(run("T2").has("undeclared_tool"));
    assert_eq!(
        run("T0\nT1\nT0")
            .stats
            .slots_used
            .into_iter()
            .collect::<Vec<_>>(),
        [1, 2]
    );
}

#[test]
fn plastic_is_never_laid_from_an_unknown_position() {
    assert!(lint("M83\nG1 X10 Y10 E5\n").has("unverifiable_move"));
    assert!(lint("G90\nM83\nG28\nG1 X10 Y10\nG1 X20 E1\n").has("unverifiable_move"));
    // Whether a move lays plastic is decided by the extrusion counter, which runs in
    // relative mode too: after 3 mm back, E 0.5 in absolute mode is 0.5 mm forward.
    let hidden = "G92 E3\nG1 E-3\nM82\n";
    let report = after_ready(&format!("{hidden}G1 X60 Y50 E0.5\nM83"));
    assert!(report.passed(), "{:?}", report.findings);
    assert_eq!(report.stats.printing_moves, 1);
    refused(
        &format!("{hidden}G1 Y-0.1\nG1 X60 E0.5"),
        "extrusion_outside_reach",
    );
    // A counter changed in lines that may not have run says nothing afterwards: nobody
    // can say how much filament an absolute E pushes, until the counter is set again.
    let maybe = "M82\nM622 J1\nG92 E100\nM623\n";
    refused(&format!("{maybe}G1 X60 E50"), "unverifiable_move");
    refused(&format!("{maybe}G1 X60 E100.5"), "unverifiable_move");
    accepted(&format!("{maybe}G92 E100\nG1 X60 E100.5\nM83"));
    // Filament pushed on the spot needs no position, but it is a blob: only what was
    // pulled back may be pushed again.
    assert!(lint("M83\nG1 E20 F200\n").has("extrusion_in_place"));
    assert!(lint("M82\nG1 E5\nG1 E3\n").has("extrusion_in_place"));
    refused("G1 E500", "extrusion_in_place");
    accepted("G1 E-0.8\nG1 E0.8");
    // E must not be written before the file has said how to read it.
    assert!(lint("G90\nG28\nT0\nG1 Z0.2\nG1 X50 Y50\nG1 X60 E0.4\n").has("ambiguous_extrusion"));
}

#[test]
fn harm_within_reach_is_refused_in_a_plain_file_too() {
    // Closer to the plate than the first layer, printing or travelling.
    refused("G1 Z0", "too_close_to_bed");
    refused("G1 Z0.05\nG1 X150 Y150 E4", "too_close_to_bed");
    refused("G1 X60 Y50 Z0.1 E0.4", "too_close_to_bed");
    // Plastic into the air.
    refused("G1 Z200\nG1 X150 Y50 E4", "extrusion_in_the_air");
    refused("G1 Z0.6\nG1 X60 Y50 E0.4", "extrusion_in_the_air");
    accepted("G1 X60 Y50 E0.4\nG1 Z0.4\nG1 X50 Y50 E0.4");
    // More plastic than a path takes.
    refused("G1 X60 Y50 E5", "over_extrusion");
    refused("G92 E0\nM82\nG1 X60 Y50 E400", "over_extrusion");
    // A cold nozzle.
    refused("M104 S0\nG1 X60 Y50 E0.4", "cold_extrusion");
    let never_heated = whole("G1 X60 Y50 E0.4").replace("M104 S220\n", "");
    assert!(lint(&never_heated).has("cold_extrusion"));
    // Heaters left on.
    assert!(lint(&format!("{STATEMENTS}{READY}")).has("heaters_left_on"));
    assert!(lint(&format!("{STATEMENTS}{READY}M140 S35\nM104 S0\n")).has("heaters_left_on"));
    // Faster than any move of a slice.
    refused("G1 X60 F999999", "value_out_of_range");
    accepted("G1 X60 F60000");
    // Absolute E under relative positioning.
    refused("M82\nG91\nG1 X5 E0.2", "ambiguous_extrusion");
}

#[test]
fn text_commands_carry_names_and_numbers_only() {
    accepted("M1002 gcode_claim_action : 14");
    accepted("M1002 judge_flag part_cooling_test_flag");
    accepted("M1002 set_filament_type:PLA");
    refused("M1002 set_filament_type:PETG", "filament_mismatch");
    // A flag of the print job is read, never set.
    refused("M1002 set_flag part_cooling_test_flag=1", "text_argument");
    refused("M1002 set_flag part_cooling_test_flag 1", "text_argument");
    refused("M1002 gcode_claim_action : 14 M500", "text_argument");
    refused("M1002 g28", "text_argument");
    refused("M1002 x\\nM500", "text_argument");
    refused("M117 hello", "unknown_command");
}

#[test]
fn the_declaration_itself_is_checked() {
    let gap = job_on(COOL_PLATE, vec![filament(2, "PLA", 1)]);
    assert!(lint_as(&gap, "").has("invalid_declaration"));
    let none = job_on(COOL_PLATE, Vec::new());
    assert!(lint_as(&none, "").has("invalid_declaration"));
    let ghost_nozzle = job_on(COOL_PLATE, vec![filament(1, "PLA", 2)]);
    assert!(lint_as(&ghost_nozzle, "").has("invalid_declaration"));
    let unknown = job_on(COOL_PLATE, vec![filament(1, "Unobtainium", 1)]);
    let report = lint_as(&unknown, "M104 S200\n");
    assert!(report.has("unknown_filament") && report.has("temperature_over_limit"));
    // The build plate and the nozzle size are part of the job, from the profile's lists.
    for plate in ["", "Glass", "cool plate", "Cool Plate "] {
        let job = job_on(plate, vec![filament(1, "PLA", 1)]);
        assert!(lint_as(&job, "").has("invalid_declaration"), "{plate:?}");
    }
    for size in [0.0, 0.2, 0.8, f64::NAN, -0.4] {
        let mut job = pla_job();
        job.filaments[0].nozzle_diameter_mm = size;
        assert!(lint_as(&job, "").has("invalid_declaration"), "{size}");
    }
    // A material is matched by its family name or a listed alias, never by its start:
    // PCTG is not PC, and "PLA something" is not PLA.
    for material in ["PCTG", "PLA Silk", "PLA Whatever", "PETG-CF", "PC FR"] {
        let job = job_on(COOL_PLATE, vec![filament(1, material, 1)]);
        assert!(lint_as(&job, "").has("unknown_filament"), "{material}");
    }
    for material in ["PLA", "pla", "PLA Basic", "PLA Matte"] {
        let job = job_on(COOL_PLATE, vec![filament(1, material, 1)]);
        let report = lint_as(&job, &whole("G1 X60"));
        assert!(report.passed(), "{material}: {:?}", report.findings);
    }
}

#[test]
fn size_limits_hold_and_findings_are_capped() {
    let long = format!("G1 X10 ;{}\n", "x".repeat(5000));
    refused(&long, "line_too_long");
    let hidden = format!("G1 X10 {}G1 X300", " ".repeat(5000));
    refused(&hidden, "line_too_long");
    // A line that is only a comment may be longer; what follows it is still read.
    let comment = format!("; {}\nG1 X300", "x".repeat(5000));
    refused(&comment, "out_of_reach");
    accepted(&format!("  ; {}\nG1 X20", "x".repeat(5000)));

    let many = "M500\n".repeat(1000);
    let report = lint(&many);
    assert!(report.errors >= 1000);
    assert_eq!(report.findings.len(), MAX_FINDINGS);
    assert_eq!(report.stats.lines, 1000);

    let big = "G4 P1\n".repeat(100);
    let report = lint_gcode(big.as_bytes(), &pla_job(), &x2d(), 64).unwrap();
    assert!(report.has("gcode_too_large") && !report.passed());

    let empty = lint(&format!("{STATEMENTS}; only comments\n\n"));
    assert!(empty.passed() && empty.has("no_moves"));
}

/// Random bytes must never panic the linter, and must never pass with a move out of reach.
#[test]
fn random_input_never_panics() {
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let alphabet = b"GMTXYZEFIJRSNP0123456789.-+ ;*()\n\r\tgmxyze\x00\xff";
    for _ in 0..300 {
        let length = (next() % 400) as usize;
        let bytes: Vec<u8> = (0..length)
            .map(|_| alphabet[(next() % alphabet.len() as u64) as usize])
            .collect();
        let report = lint_gcode(bytes.as_slice(), &pla_job(), &x2d(), u64::MAX).unwrap();
        if let Some(extent) = report.stats.extent {
            assert!(extent.min[0] >= 0.0 && extent.max[0] <= 256.0);
            assert!(extent.min[1] >= 0.0 && extent.max[1] <= 256.0);
        }
    }
}

/// `gcode` after what a file says it was sliced for, with every heater off at its end, and
/// nothing else: no homing and no state to start from.
fn bare(job: &DeclaredJob, gcode: &str) -> LintReport {
    lint_as(job, &format!("{STATEMENTS}{gcode}{OFF}"))
}

#[test]
fn an_axis_is_placed_only_after_the_file_homed_it() {
    // No homing anywhere: the absolute moves go nobody knows where.
    let report = bare(
        &pla_job(),
        "M104 S220\nG90\nM83\nT0\nG1 X128 Y128 Z0.2\nG1 X200 Y128 E5\n",
    );
    assert!(
        error_codes(&report).contains(&"unverifiable_move"),
        "{:?}",
        report.findings
    );
    // Homing X and Y does not home Z.
    assert!(bare(&pla_job(), "G28 X Y\nG90\nG1 X10 Y10\n").passed());
    assert!(bare(&pla_job(), "G28 X Y\nG90\nG1 X10 Y10\nG1 Z0.2\n").has("unverifiable_move"));
    // The position is never redefined on X, Y or Z.
    assert!(bare(&pla_job(), "G92 Z0\n").has("unexpected_parameters"));

    // Ten metres of relative moves on each axis, before any homing.
    let far = format!("G91\n{}{}", "G1 Z-250\n".repeat(40), "G1 X250\n".repeat(40));
    let report = bare(&pla_job(), &far);
    assert!(
        !report.passed() && report.has("unverifiable_move"),
        "{:?}",
        report.findings
    );
    // The same after homing, where the position is not known either.
    let far = format!("G28\nG91\n{}", "G1 X250 Y250\n".repeat(40));
    let report = bare(&pla_job(), &far);
    assert!(
        !report.passed() && report.has("unverifiable_move"),
        "{:?}",
        report.findings
    );
    // Moves short enough to stay under the axis length each, but not in all.
    assert!(bare(&pla_job(), &"G91\nG1 Z8\n".repeat(4)).has("unverifiable_move"));
    // A lift before homing is a move from a position the file does not give. Outside
    // the start routine of a marked file no such move passes, however short
    // (`stock_routine.rs` has the ones the stock start routine makes).
    assert!(bare(&pla_job(), "G91\nG1 Z5\nG90\nG28\nG1 Z0.2\n").has("unverifiable_move"));
}

#[test]
fn commands_with_listed_parameters_are_held_to_their_bounds() {
    // The Z trim, motor current, speed and flow factors stand only in the start routine, in
    // the stock forms (`stock_routine.rs`); in the print they are refused whatever their
    // values.
    for gcode in [
        "G29.1 Z-0.04",
        "G29.1 Z-5",
        "G29.1 Z0.3",
        "G29.1 X1",
        "M17 X1.2 Y1.2 Z0.75",
        "M17 S",
        "M17 R",
        "M17",
        "M17 X9 Y9 Z9",
        "M17 E1",
        "M220 S100",
        "M220 S5000",
        "M221 S100",
        "M221 S1000",
        "M220 S1e2",
        "M220 S+-1",
        "M220 S100B1",
    ] {
        refused(gcode, "command_out_of_place");
    }
    // A dwell holds the heaters on: three minutes at most, and always with its length.
    accepted("G4 S5");
    accepted("G4 P500");
    accepted("G4 S180");
    accepted("G4 P180000");
    refused("G4", "unexpected_parameters");
    refused("G4 S181", "value_out_of_range");
    refused("G4 P180001", "value_out_of_range");
    refused("G4 S9999999", "value_out_of_range");
    refused("G4 P700000", "value_out_of_range");
    refused("G4 X1", "unexpected_parameters");
    // Acceleration within the table's limit; the motion limit commands are refused by
    // name in the print.
    accepted("M204 S10000");
    refused("M204 S999999", "value_out_of_range");
    refused("M201 X999999 Y999999 Z999999 E999999", "denied_command");
    refused("M203 X99999 Y99999 Z99999 E99999", "denied_command");
    refused("M205 X500", "denied_command");
    // A fan at full speed is a fan.
    accepted("M106 P1 S255");
}

#[test]
fn a_nozzle_named_on_a_temperature_command_is_held_to_what_it_can_mean() {
    // PLA on the main nozzle (id 1), PC on the auxiliary nozzle (id 0). `T<n>` alone
    // selects filament n (0-based); on a temperature command T names the nozzle by its
    // device id, as every real slice writes it (`M104 S.. T1` for the main nozzle in a
    // file with one filament).
    let job = job_on(
        COOL_PLATE,
        vec![filament(1, "PLA", 1), filament(2, "PC", 0)],
    );
    let homed = |gcode: &str| bare(&job, &format!("G90\nM83\nG28\n{gcode}"));
    // Without a T the active filament holds: PC may be 300 C, PLA may not.
    assert!(homed("T1\nM109 S300\n").passed());
    assert!(homed("T0\nM109 S300\n").has("temperature_over_limit"));
    // A nozzle named on the line is held to the filament on it, whatever is active.
    assert!(homed("T0\nM104 T1 S300\n").has("temperature_over_limit"));
    assert!(homed("T1\nM104 T0 S300\n").passed());
    assert!(homed("T0\nM104 T1 S230\n").passed());
    assert!(homed("T1\nM104 T0 S280\n").passed());
    // M109 takes no T; a nozzle that is not the printer's, or carries no filament of the
    // job, is not heated.
    assert!(homed("T0\nM109 S300 T0\n").has("multiple_commands"));
    assert!(homed("M104 T7 S200\n").has("undeclared_nozzle"));
    assert!(bare(&pla_job(), "G90\nM83\nG28\nM104 T0 S200\n").has("undeclared_nozzle"));

    // A letter the table does not list for the command is not skipped.
    refused("M104 B300 F1 S200", "unexpected_parameters");
    refused("M104 I1", "unexpected_parameters");
    refused("M104 H1 S230", "unexpected_parameters");
    refused("M140 S60 X1", "unexpected_parameters");
    refused("M141 P1 S40", "unexpected_parameters");
    // The old filament change settings command is not one this printer's slices use.
    refused("M620.1 E F299.3 T240", "unknown_command");
    refused("M620.1 E F299.3 T241", "unknown_command");
    refused("M620.1 X1 T240", "unknown_command");
}

#[test]
fn how_much_plastic_a_move_lays_never_depends_on_how_firmware_reads_g91() {
    // Absolute extrusion, then G91: firmware that switches the extruder with G91 pushes
    // 5 mm on each of the last moves, firmware that does not pulls back. Such a line is
    // refused, however little it pushes.
    let report = after_ready("G1 X10 Y10 Z0.3\nM82\nG92 E0\nG1 X20 E1\nG91\nG1 X5 E1.5");
    assert_eq!(error_codes(&report), ["ambiguous_extrusion"]);
    let report = after_ready("G1 X10 Y10 Z0.3\nM82\nG92 E0\nG1 X20 E100\nG91\nG1 X5 E5\nG1 X5 E5");
    assert!(report.has("ambiguous_extrusion") && !report.passed());
    refused(
        "M82\nG92 E0\nG1 X20 E100\nG28 Z\nG91\nG1 X5 E5",
        "ambiguous_extrusion",
    );
    // With an explicit M83 the two readings agree, and a retraction lays nothing.
    let report = after_ready("G91\nG1 X5 E-1\nG90\nM83\nG1 X60 E-1");
    assert!(report.passed(), "{:?}", report.findings);
    assert_eq!(report.stats.printing_moves, 0);
}
