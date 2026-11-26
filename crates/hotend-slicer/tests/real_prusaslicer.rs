//! Tests against a real PrusaSlicer, when one is installed. They prove what the fake cannot:
//! that the real program is found, reports its version, accepts Hotend's command line and
//! settings files inside the sandbox, and writes G-code the parser reads.
//!
//! PrusaSlicer is optional and Hotend never installs it, so these tests are not part of
//! `cargo test`: they are marked `ignore` and cargo counts them as ignored. Where a
//! PrusaSlicer is installed, run them with
//!
//! ```text
//! cargo test -p hotend-slicer --test real_prusaslicer -- --ignored
//! ```
//!
//! Run like that, a test that finds no PrusaSlicer fails: a pass always means the real
//! program ran. (`HOTEND_TEST_ALLOW_SKIP=1` turns that failure into a printed `SKIPPED`,
//! as for the sandbox.) On Ubuntu 24.04, `apt-get install -y prusa-slicer` installs 2.7.2,
//! which the built-in version policy refuses; the slicing tests therefore use a test-only
//! policy that allows whatever version is installed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use hotend_slicer::{
    Found, Installation, Slicer, SlicerConfig, SlicerKind, SlicerVersion, Verdict, VersionPolicy,
};

/// A policy that allows every PrusaSlicer version. Only tests construct one.
fn permissive_policy() -> VersionPolicy {
    VersionPolicy::from_toml(
        "schema = 1\nrevision = \"test\"\n[[slicer]]\nkind = \"prusa-slicer\"\nunknown = \"refuse\"\n\
         [[slicer.rule]]\nverdict = \"allow\"\n",
    )
    .unwrap()
}

/// The real PrusaSlicer on this machine. Without one the test fails; only with
/// `HOTEND_TEST_ALLOW_SKIP=1` it is skipped instead (`None` after printing why).
fn real_prusaslicer(test: &str, slicer: &Slicer) -> Option<Installation> {
    if !sandbox_ready(test) {
        return None;
    }
    let problem = if std::env::var_os(SlicerKind::PrusaSlicer.override_env()).is_some() {
        "HOTEND_PRUSA_SLICER is set, so the installed program is not searched for"
    } else {
        let found = slicer
            .detect()
            .unwrap()
            .into_iter()
            .find(|i| i.kind == SlicerKind::PrusaSlicer && !i.fake);
        if found.is_some() {
            return found;
        }
        "no PrusaSlicer is installed (on Ubuntu: apt-get install -y prusa-slicer)"
    };
    if std::env::var_os("HOTEND_TEST_ALLOW_SKIP").is_some_and(|v| v == "1") {
        println!("SKIPPED {test}: {problem}");
        return None;
    }
    panic!("{test} cannot run: {problem} (set HOTEND_TEST_ALLOW_SKIP=1 to skip instead)");
}

#[test]
#[ignore = "needs an installed PrusaSlicer; run with `-- --ignored`"]
fn the_installed_prusaslicer_is_found_and_the_default_policy_judges_its_version() {
    let test = "the_installed_prusaslicer_is_found_and_the_default_policy_judges_its_version";
    let slicer = Slicer::new(SlicerConfig::new().unwrap()).unwrap();
    let Some(installation) = real_prusaslicer(test, &slicer) else {
        return;
    };
    println!("found {installation:?}");
    assert_ne!(installation.found, Found::Override);
    assert!(installation.path.is_absolute());
    let version = installation
        .version
        .clone()
        .expect("the version is read from `--help` run inside the sandbox");
    assert_eq!(installation.display_name(), "PrusaSlicer");

    let minimum = SlicerVersion::parse("2.9.7").unwrap();
    let dir = JobDir::new(&["box_20x30x10.step"]);
    let job = prusa_job(&dir, "box_20x30x10.step");
    if version < minimum {
        // Older than the security-fix release: refused, and nothing is run or written.
        assert!(
            matches!(&installation.verdict, Verdict::Refuse(reason) if reason.contains("2.9.7")),
            "{:?}",
            installation.verdict
        );
        let error = slicer.slice(&job).unwrap_err();
        assert_eq!(error.code(), "slicer_version_refused");
        assert!(error.to_string().contains(version.as_str()), "{error}");
        assert!(!dir.path.join("output").exists());
    } else {
        println!(
            "PrusaSlicer {version} is installed; the refusal of older versions is covered by unit tests"
        );
        assert!(!matches!(installation.verdict, Verdict::Warn(_)));
    }
}

#[test]
#[ignore = "needs an installed PrusaSlicer; run with `-- --ignored`"]
fn the_real_prusaslicer_slices_step_and_stl_in_the_sandbox() {
    let test = "the_real_prusaslicer_slices_step_and_stl_in_the_sandbox";
    let mut config = SlicerConfig::new().unwrap();
    config.policy = permissive_policy();
    let slicer = Slicer::new(config).unwrap();
    let Some(installation) = real_prusaslicer(test, &slicer) else {
        return;
    };
    assert_eq!(installation.verdict, Verdict::Allow);

    let mut results = Vec::new();
    for input in ["box_20x30x10.step", "box_20x30x10.stl", "box_20x30x10.3mf"] {
        let dir = JobDir::new(&[input]);
        let result = slicer.slice(&prusa_job(&dir, input)).unwrap();
        println!("{input}: {}", serde_json::to_string(&result).unwrap());

        assert_eq!(result.status, "ok");
        assert_eq!(result.output, "output/plate_1.gcode");
        let output = dir.path.join(&result.output);
        assert_eq!(result.sha256, sha256(&output));
        assert_eq!(result.slicer.name, "PrusaSlicer");
        assert_eq!(result.slicer.version, installation.version_text());
        // A 10 mm tall box at 0.2 mm layers.
        assert_eq!(result.layers, Some(50), "{input}");
        // 6 cm3 of bounding volume, two perimeters, 15 % infill: a few grams and minutes.
        let time = result.time_s.expect("print time");
        assert!((120.0..3600.0).contains(&time), "{input}: {time} s");
        assert_eq!(result.filament.len(), 1);
        let grams = result.filament[0].grams.expect("grams");
        let metres = result.filament[0].metres.expect("metres");
        assert!((1.0..7.44).contains(&grams), "{input}: {grams} g");
        // Metres and grams agree through the filament's cross-section and the profile's
        // density (1.75 mm, 1.24 g/cm3): the two numbers were read from the right lines.
        let expected_grams = metres * 1000.0 * std::f64::consts::PI * 0.875 * 0.875 / 1000.0 * 1.24;
        assert!(
            (grams - expected_grams).abs() < 0.05,
            "{input}: {grams} g for {metres} m"
        );
        assert_eq!(result.filament[0].nozzle_id, 0);
        // Only the standing warnings: the summary was read without a gap.
        assert_eq!(
            codes(&result.warnings),
            ["demo_profile", "unverified_printer_facts"],
            "{input}"
        );

        // The G-code is text written by the real program, which ran under the sandbox
        // launcher (the launcher is what creates the home folder inside the job folder).
        let head = std::fs::read_to_string(&output).unwrap();
        assert!(
            head.starts_with("; generated by PrusaSlicer "),
            "{}",
            &head[..80.min(head.len())]
        );
        assert!(dir.path.join(".sandbox/home").is_dir());
        results.push(result);
    }
    // The same solid in three formats slices to the same amount of plastic.
    let grams: Vec<f64> = results
        .iter()
        .map(|r| r.filament[0].grams.unwrap())
        .collect();
    assert!(
        (grams[0] - grams[1]).abs() < 0.05 && (grams[0] - grams[2]).abs() < 0.05,
        "{grams:?}"
    );
}

#[test]
#[ignore = "needs an installed PrusaSlicer; run with `-- --ignored`"]
fn the_real_prusaslicer_reports_a_model_it_cannot_slice() {
    let test = "the_real_prusaslicer_reports_a_model_it_cannot_slice";
    let mut config = SlicerConfig::new().unwrap();
    config.policy = permissive_policy();
    let slicer = Slicer::new(config).unwrap();
    if real_prusaslicer(test, &slicer).is_none() {
        return;
    }
    let dir = JobDir::new(&[]);
    std::fs::write(
        dir.path.join("input/broken.stl"),
        b"solid nothing\nendsolid nothing\n",
    )
    .unwrap();
    let error = slicer.slice(&prusa_job(&dir, "broken.stl")).unwrap_err();
    println!("{error}");
    assert!(
        matches!(error.code(), "slicer_failed" | "unusable_output"),
        "{error}"
    );
}

#[tokio::test]
#[ignore = "needs an installed PrusaSlicer; run with `-- --ignored`"]
async fn a_core_request_for_a_prusa_printer_is_sliced_from_the_plate_by_the_real_program() {
    let test = "a_core_request_for_a_prusa_printer_is_sliced_from_the_plate_by_the_real_program";
    let mut config = SlicerConfig::new().unwrap();
    config.policy = permissive_policy();
    let slicer = std::sync::Arc::new(Slicer::new(config).unwrap());
    if real_prusaslicer(test, &slicer).is_none() {
        return;
    }
    let version = Version::new([20.0, 30.0, 10.0]);
    let backend = hotend_slicer::CoreSliceBackend::new(slicer);
    let summary = hotend_core::SliceBackend::slice(
        &backend,
        version.request("prusa-mk4s", "PLA", "pla-basic"),
    )
    .await
    .unwrap();
    println!("{}", serde_json::to_string(&summary).unwrap());
    assert_eq!(summary.output, "output/plate_1.gcode");
    assert_eq!(summary.layers, 50);
    assert!(summary.time_s > 120.0);
    assert!(summary.filament[0].grams > 1.0 && summary.filament[0].metres > 0.3);
    assert_eq!(summary.slicer.name, "PrusaSlicer");
    // What the core's gateway does with the summary.
    let path =
        hotend_core::import::resolve_inside(&version.output_dir, &summary.output, 1 << 30).unwrap();
    assert_eq!(
        hotend_core::import::sha256_file(&path).unwrap(),
        summary.sha256
    );
}
