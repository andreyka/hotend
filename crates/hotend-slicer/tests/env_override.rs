//! The environment override, in a test binary of its own because it changes the process
//! environment.

#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use hotend_slicer::{Found, Slicer, SlicerConfig, SlicerKind};

#[test]
fn the_environment_variable_names_the_slicer_and_stops_the_search() {
    if !sandbox_ready("the_environment_variable_names_the_slicer_and_stops_the_search") {
        return;
    }
    // The fake slicer is found next to the test binaries, as the demo finds it.
    let fake = hotend_slicer::fake::locate().unwrap();
    assert_eq!(fake, fake_slicer());
    // SAFETY: this test binary has one test, so nothing else reads the environment now.
    unsafe {
        std::env::set_var("HOTEND_BAMBU_STUDIO", &fake);
        std::env::set_var("HOTEND_PRUSA_SLICER", &fake);
    }
    let slicer = Slicer::new(SlicerConfig::new().unwrap()).unwrap();
    let found = slicer.detect().unwrap();
    assert_eq!(found.len(), 2, "{found:?}");
    for installation in &found {
        assert!(installation.fake);
        assert_eq!(installation.found, Found::Override);
        assert_eq!(installation.path, fake);
    }
    let dir = JobDir::new(&["box_20x30x10.3mf"]);
    let job = x2d_job(
        &dir,
        "box_20x30x10.3mf",
        vec![filament(1, "pla-basic", "PLA", 1)],
    );
    let result = slicer.slice(&job).unwrap();
    assert_eq!(result.layers, Some(50));
    assert!(codes(&result.warnings).contains(&"fake_slicer"));

    // A setting wins over the environment.
    let mut config = SlicerConfig::new().unwrap();
    config
        .executables
        .insert(SlicerKind::BambuStudio, "/nonexistent/from-settings".into());
    assert!(Slicer::new(config).unwrap().detect().is_err());
}
