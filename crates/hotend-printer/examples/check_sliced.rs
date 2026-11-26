//! Runs Hotend's check on one sliced `.gcode.3mf` and prints the verdict, the text a person
//! would confirm, and what the check measured.
//!
//! `cargo run -p hotend-printer --example check_sliced -- <file.gcode.3mf> [model] [material] [nozzle] [plate type]`
//!
//! The model defaults to `bambu-x2d`, the material to `PLA`, the nozzle (device extruder id)
//! to 1 and the build plate to `Cool Plate`. The job is plate 1 with one filament in slot 1
//! on a 0.4 mm nozzle. Nothing here contacts a printer; it
//! only reads the file. A path that ends in `.gcode` is checked as plain G-code. A file
//! that is refused is printed with every reason. The exit code is 0 when the file is
//! acceptable and 1 when it is not.
#![allow(clippy::print_stdout, clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::fs::File;
use std::path::PathBuf;

use hotend_printer::archive::Archive;
use hotend_printer::lint::lint_gcode;
use hotend_printer::{ArchiveLimits, DeclaredFilament, DeclaredJob, JobError, Profiles, check_job};

fn main() {
    let mut args = std::env::args().skip(1);
    let path =
        PathBuf::from(args.next().expect(
            "usage: check_sliced <file.gcode.3mf> [model] [material] [nozzle] [plate type]",
        ));
    let model = args.next().unwrap_or_else(|| "bambu-x2d".to_owned());
    let material = args.next().unwrap_or_else(|| "PLA".to_owned());
    let nozzle: u8 = args
        .next()
        .map_or(1, |n| n.parse().expect("nozzle is a number"));
    let plate_type = args.next().unwrap_or_else(|| "Cool Plate".to_owned());

    let profiles = Profiles::builtin().expect("builtin profiles");
    let profile = profiles.printer(&model).expect("printer model");
    let declared = DeclaredJob {
        plate: 1,
        plate_type,
        filaments: vec![DeclaredFilament {
            slot: 1,
            material,
            nozzle,
            nozzle_diameter_mm: 0.4,
        }],
    };
    let limits = ArchiveLimits::default();

    if path.extension().is_some_and(|ext| ext == "gcode") {
        // A plain G-code file: lint only, for diagnosis.
        let report =
            lint_gcode(File::open(&path).unwrap(), &declared, &profile, u64::MAX).expect("read");
        print_report(&report);
        std::process::exit(i32::from(!report.passed()));
    }

    println!("== check_job (archive + slice_info + lint), declared {declared:?}");
    let acceptable = match check_job(&path, "gauge", &declared, &profile, &limits) {
        Ok(job) => {
            println!("ACCEPTABLE: 0 errors, {} warning(s)", job.lint().warnings);
            println!("\n== what a person is asked to confirm");
            println!("{}", job.summary().review_text());
            println!("\n== warnings");
            for finding in &job.lint().findings {
                println!("{}: {}", finding.code, finding.message);
            }
            println!("\n== summary");
            println!("{}", serde_json::to_string_pretty(job.summary()).unwrap());
            true
        }
        Err(JobError::Rejected(report)) => {
            println!(
                "REJECTED: {} error(s), {} warning(s)",
                report.errors, report.warnings
            );
            for finding in &report.findings {
                println!(
                    "{:?}\t{}\tline {}\t{}",
                    finding.severity, finding.code, finding.line, finding.message
                );
            }
            false
        }
        Err(other) => {
            println!("NOT CHECKED: {other}");
            false
        }
    };

    if acceptable {
        println!("\n== lint_gcode on Metadata/plate_1.gcode alone");
        let mut archive = Archive::open(File::open(&path).unwrap(), &limits).expect("archive");
        let reader = archive
            .reader("Metadata/plate_1.gcode", limits.max_gcode_bytes)
            .expect("archive entry")
            .expect("Metadata/plate_1.gcode present");
        let report = lint_gcode(reader, &declared, &profile, limits.max_gcode_bytes).expect("read");
        print_report(&report);
    }
    std::process::exit(i32::from(!acceptable));
}

fn print_report(report: &hotend_printer::LintReport) {
    println!(
        "passed: {}  errors: {}  warnings: {}",
        report.passed(),
        report.errors,
        report.warnings
    );
    let mut by_code: BTreeMap<&str, u64> = BTreeMap::new();
    for finding in &report.findings {
        // Kept findings only; MAX_FINDINGS caps them.
        *by_code.entry(finding.code).or_default() += 1;
    }
    println!("kept findings by code: {by_code:?}");
    for finding in &report.findings {
        println!(
            "{:?}\t{}\tline {}\t{}\t| {}",
            finding.severity, finding.code, finding.line, finding.message, finding.excerpt
        );
    }
    println!(
        "stats: {}",
        serde_json::to_string_pretty(&report.stats).unwrap()
    );
}
