//! The proof on a real holder slice: sliced in the Bambu Studio window (02.09.01.70) with
//! exactly the settings of the proven holder recipe. The file holds the
//! vendor's G-code and profile text, so it is not in this repository and these tests are not
//! part of `cargo test`: they are marked `ignore`. Where the reference folder is available:
//!
//! ```text
//! HOTEND_REAL_SLICE_DIR=/path/to/real-slice-ref \
//!     cargo test -p hotend-slicer --test real_proof -- --ignored
//! ```
//!
//! The folder holds `textured-b.gcode.3mf`, a test print of 344 layers. A missing folder or file
//! fails the test: a pass always means the real file was read. No slicer runs here.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::PathBuf;

use common::*;
use hotend_slicer::proof::{self, Expected};

fn holder() -> PathBuf {
    let dir = std::env::var_os("HOTEND_REAL_SLICE_DIR")
        .expect("set HOTEND_REAL_SLICE_DIR to the folder of the reference slices");
    let file = PathBuf::from(dir).join("textured-b.gcode.3mf");
    assert!(file.is_file(), "{}", file.display());
    file
}

#[test]
#[ignore = "needs a real holder slice; set HOTEND_REAL_SLICE_DIR and run with `-- --ignored`"]
fn the_real_holder_slice_passes_the_proof_of_its_recipe() {
    let expected = Expected::for_settings(&recipe_settings(), None).unwrap();
    let report = proof::check_sliced_file(&holder(), 1, &expected).unwrap();
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    assert!(report.passed(), "{}", report.sentence());
    let fact = |id: &str| report.facts.iter().find(|fact| fact.id == id).unwrap();
    // Measured in the file: bed 65 C on the first layer and 60 C from layer 2, the part fan
    // first on at layer 4, brim and walls at 20 mm/s and the bottom at 30 mm/s, the chamber fan
    // at 127 of 255 from the start routine.
    assert_eq!(fact("bed_first_layer").measured, Some(65.0));
    assert_eq!(fact("bed_layer_3").measured, Some(60.0));
    assert_eq!(fact("part_fan_delay").measured, Some(4.0));
    assert_eq!(fact("first_layer_wall_speed").measured, Some(20.0));
    assert_eq!(fact("first_layer_fill_speed").measured, Some(30.0));
    assert_eq!(fact("chamber_fan_first_layer").measured, Some(127.0));
}

#[test]
#[ignore = "needs a real holder slice; set HOTEND_REAL_SLICE_DIR and run with `-- --ignored`"]
fn the_real_holder_slice_fails_the_proof_of_a_stock_plan() {
    let expected = Expected::for_settings(&stock_settings(), None).unwrap();
    let report = proof::check_sliced_file(&holder(), 1, &expected).unwrap();
    println!("{}", report.sentence());
    assert!(!report.passed());
    assert_eq!(report.code, Some("setting_not_applied"));
}
