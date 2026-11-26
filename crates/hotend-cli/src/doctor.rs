//! `hotend doctor`: what this machine has, what is missing and how to fix it.
//!
//! The report itself is made by [`hotend_core::doctor`], the same data the app shows. This
//! module adds the one thing only the command line has, the stand-in slicer next to the
//! program, and prints the report as a table or as JSON. Exit code 1 means a hard
//! requirement is missing: a usable sandbox, the Python environment, or a self-test (the
//! engine builds a part and a scripted agent calls a tool, both inside the sandbox).

use std::sync::Arc;

use hotend_core::{
    DoctorCheck, DoctorOptions, DoctorReport, DoctorStatus, SliceBackend, doctor as core_doctor,
};
use hotend_slicer::{CoreSliceBackend, SlicerConfig};

use crate::error::CliError;
use crate::{render, table};

fn word(status: DoctorStatus) -> &'static str {
    match status {
        DoctorStatus::Ok => "ok",
        DoctorStatus::Warn => "warn",
        DoctorStatus::Fail => "FAIL",
        DoctorStatus::Skipped => "skipped",
        DoctorStatus::Info => "-",
    }
}

/// The row for Hotend's own stand-in slicer, placed after the installed slicers.
fn stand_in_row() -> DoctorCheck {
    match hotend_slicer::fake::locate() {
        Some(path) => DoctorCheck::new(
            "slicer.fake",
            "slicer: stand-in",
            DoctorStatus::Info,
            format!(
                "{} (used by `hotend demo` and `hotend slice --fake`; estimates only, \
                 output not printable)",
                path.display()
            ),
        ),
        None => DoctorCheck::new(
            "slicer.fake",
            "slicer: stand-in",
            DoctorStatus::Warn,
            "not found",
        )
        .fix("build it with `cargo build --workspace`; `hotend demo` needs it"),
    }
}

pub async fn run(as_json: bool) -> Result<(), CliError> {
    let slicers = SlicerConfig::new()
        .ok()
        .map(|config| Arc::new(CoreSliceBackend::from_config(config)) as Arc<dyn SliceBackend>);
    let mut report = core_doctor::report(DoctorOptions {
        slicers,
        skip_self_tests: false,
    })
    .await;
    let after_slicers = report
        .checks
        .iter()
        .rposition(|check| check.id.starts_with("slicer"))
        .map_or(report.checks.len(), |index| index + 1);
    report.checks.insert(after_slicers, stand_in_row());

    if as_json {
        let text = serde_json::to_string_pretty(&report)
            .map_err(|error| CliError::failed(error.to_string()))?;
        println!("{text}");
    } else {
        print_report(&report);
    }
    if report.ok {
        Ok(())
    } else {
        Err(CliError::Reported)
    }
}

fn bullet(text: &str) {
    println!(
        "{}",
        render::indented(&render::clean(text), "    ").replacen("    ", "  - ", 1)
    );
}

fn print_report(report: &DoctorReport) {
    let build = if report.debug_build {
        "debug"
    } else {
        "release"
    };
    println!("hotend {} ({build} build)\n", report.hotend);
    let rows: Vec<Vec<String>> = report
        .checks
        .iter()
        .map(|check| {
            vec![
                check.label.clone(),
                word(check.status).to_owned(),
                render::clean(&check.detail),
            ]
        })
        .collect();
    print!(
        "{}",
        table::render_wrapped(&["check", "status", "detail"], &rows, render::WIDTH + 14)
    );

    if let Some(limitations) = report
        .sandbox
        .as_ref()
        .map(|probe| &probe.limitations)
        .filter(|l| !l.is_empty())
    {
        println!("\nSandbox limitations on this machine:");
        for limitation in limitations {
            bullet(limitation);
        }
    }
    let hard = |check: &&DoctorCheck| check.required && check.status == DoctorStatus::Fail;
    let advice: Vec<&DoctorCheck> = report
        .checks
        .iter()
        .filter(|check| check.fix.is_some() && !hard(check))
        .collect();
    if !advice.is_empty() {
        println!("\nOptional, to get more:");
        for check in advice {
            bullet(&format!(
                "{}: {}",
                check.label.trim(),
                check.fix.as_deref().unwrap_or("")
            ));
        }
    }
    let missing: Vec<&DoctorCheck> = report.checks.iter().filter(hard).collect();
    if missing.is_empty() {
        println!("\nAll hard requirements are met. Try `hotend demo`.");
    } else {
        println!("\n{} hard requirement(s) missing:", missing.len());
        for check in missing {
            let fix = check.fix.as_deref().unwrap_or("see the detail above");
            bullet(&format!("{}: {fix}", check.label.trim()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_failed_required_check_is_shown_as_a_hard_failure() {
        assert_eq!(word(DoctorStatus::Fail), "FAIL");
        assert_eq!(word(DoctorStatus::Info), "-");
        let row = stand_in_row();
        assert_eq!(row.id, "slicer.fake");
        assert!(!row.required);
    }
}
