//! `CoreSliceBackend`: the seam `hotend_core::SliceBackend`, driven the way the core's
//! gateway drives it, with the fake slicer in the sandbox.

#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;

use common::*;
use hotend_core::{SliceBackend, SliceError};
use hotend_slicer::{CoreSliceBackend, SlicerKind};

fn failure(error: SliceError) -> (String, String) {
    match error {
        SliceError::Failed { code, message } => (code, message),
        SliceError::NotConfigured => panic!("not configured"),
    }
}

#[tokio::test]
async fn an_x2d_request_is_sliced_into_the_versions_slice_folder() {
    if !sandbox_ready("an_x2d_request_is_sliced_into_the_versions_slice_folder") {
        return;
    }
    let slicer = Arc::new(slicer_with_fake(&[
        SlicerKind::BambuStudio,
        SlicerKind::PrusaSlicer,
    ]));
    let backend = CoreSliceBackend::new(Arc::clone(&slicer));
    let version = Version::new([20.0, 30.0, 10.0]);
    let summary = backend
        .slice(version.request("bambu-x2d", "PLA", "pla-basic"))
        .await
        .unwrap();

    assert_eq!(summary.status, "ok");
    assert_eq!(summary.output, "output/plate_1.gcode.3mf");
    assert_eq!((summary.time_s, summary.layers), (473.0, 50));
    // One filament, on the main nozzle: device extruder 1.
    assert_eq!(summary.filament.len(), 1);
    assert_eq!(
        (summary.filament[0].slot, summary.filament[0].nozzle_id),
        (1, 1)
    );
    assert_eq!(
        (summary.filament[0].grams, summary.filament[0].metres),
        (2.6, 0.87)
    );
    let codes: Vec<&str> = summary.warnings.iter().map(|w| w.code.as_str()).collect();
    assert_eq!(
        codes,
        ["fake_slicer", "demo_profile", "unverified_printer_facts"]
    );

    // The checks the core's gateway makes on a summary: the output is a plain file inside
    // the output folder, and its hash is the reported one.
    let path =
        hotend_core::import::resolve_inside(&version.output_dir, &summary.output, 1 << 30).unwrap();
    assert_eq!(
        hotend_core::import::sha256_file(&path).unwrap(),
        summary.sha256
    );
    // The version folder itself was not written to; everything is below the slice folder.
    let mut names: Vec<String> = std::fs::read_dir(&version.dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["prepare", "slice"]);
    assert!(version.output_dir.join("input/plate.3mf").is_file());

    // TPU on the X2D goes to the main nozzle, which is allowed.
    let tpu = backend
        .slice(version.request("bambu-x2d", "TPU", "tpu-95a"))
        .await
        .unwrap();
    assert_eq!(tpu.filament[0].nozzle_id, 1);
}

#[tokio::test]
async fn failures_reach_the_core_as_typed_codes() {
    if !sandbox_ready("failures_reach_the_core_as_typed_codes") {
        return;
    }
    let slicer = Arc::new(slicer_with_fake(&[
        SlicerKind::BambuStudio,
        SlicerKind::PrusaSlicer,
    ]));
    let version = Version::new([20.0, 30.0, 10.0]);

    let backend = CoreSliceBackend::new(Arc::clone(&slicer));

    // A piece laid out for the auxiliary nozzle cannot be printed by the one filament of
    // a slice, which is on the main nozzle.
    let mut other_nozzle = version.request("bambu-x2d", "PLA", "pla-basic");
    other_nozzle.pieces[0].nozzle_id = 0;
    let (code, message) = failure(backend.slice(other_nozzle).await.unwrap_err());
    assert_eq!(code, "invalid_job");
    assert!(message.contains("nozzle 0"), "{message}");

    let (code, _) = failure(
        backend
            .slice(version.request("no-such-printer", "PLA", "pla-basic"))
            .await
            .unwrap_err(),
    );
    assert_eq!(code, "unknown_printer");
    let (code, _) = failure(
        backend
            .slice(version.request("bambu-x2d", "PLA", "unknown-filament"))
            .await
            .unwrap_err(),
    );
    assert_eq!(code, "profile_not_found");
    let mut request = version.request("bambu-x2d", "PLA", "pla-basic");
    request.profile = "no-such-process".into();
    let (code, _) = failure(backend.slice(request).await.unwrap_err());
    assert_eq!(code, "profile_not_found");

    // A piece that lies outside the main nozzle's reach, as the preparation placed it.
    let too_big = Version::new([300.0, 30.0, 10.0]);
    let (code, message) = failure(
        backend
            .slice(too_big.request("bambu-x2d", "PLA", "pla-basic"))
            .await
            .unwrap_err(),
    );
    assert_eq!(code, "out_of_reach");
    assert!(message.contains("box"), "{message}");
    assert!(
        !too_big.output_dir.join("input").exists(),
        "nothing is copied for a refused job"
    );

    // The same plate goes to the slicer of the other printer: a PrusaSlicer request is
    // sliced from the same 3MF.
    let prusa = backend
        .slice(version.request("prusa-mk4s", "PLA", "pla-basic"))
        .await
        .unwrap();
    assert_eq!(prusa.filament[0].nozzle_id, 0);
    assert!(version.output_dir.join("input/plate.3mf").is_file());

    // A fresh setup slices with the default material of the settings on either slicer:
    // each has a filament profile of that id.
    let material = hotend_core::Settings::default().material;
    for printer in ["prusa-mk4s", "bambu-x2d"] {
        let fresh = backend
            .slice(version.request(printer, &material.family, &material.id))
            .await
            .unwrap_or_else(|error| panic!("{printer} with {}: {error}", material.id));
        assert_eq!(fresh.filament.len(), 1, "{printer}");
    }
}

/// The backend reports the slicers it would run, found or not, for the settings page and
/// the doctor report.
#[test]
fn the_backend_lists_the_slicers_with_the_printers_they_serve() {
    // Finding a slicer runs it in the sandbox to read its version.
    if !sandbox_ready("the_backend_lists_the_slicers_with_the_printers_they_serve") {
        return;
    }
    let slicer = Arc::new(slicer_with_fake(&[SlicerKind::BambuStudio]));
    let rows = CoreSliceBackend::new(slicer).installations(&Default::default());
    let studio = rows.iter().find(|row| row.kind == "bambu-studio").unwrap();
    assert!(studio.stand_in && studio.usable && studio.path.is_some());
    assert_eq!(studio.printers, ["bambu-x2d"]);
    // PrusaSlicer is listed whether it is installed or not, with its printer.
    let prusa = rows.iter().find(|row| row.kind == "prusa-slicer").unwrap();
    assert_eq!(prusa.printers, ["prusa-mk4s"]);
    assert!(prusa.path.is_some() || (prusa.verdict == "missing" && prusa.fix.is_some()));
}

/// The core's seam carries the editable project too, and the stand-in makes none: it says
/// so with a code, and leaves no project behind.
#[tokio::test]
async fn the_stand_in_makes_no_editable_project_through_the_seam() {
    if !sandbox_ready("the_stand_in_makes_no_editable_project_through_the_seam") {
        return;
    }
    let slicer = Arc::new(slicer_with_fake(&[SlicerKind::BambuStudio]));
    let backend: Arc<dyn SliceBackend> = Arc::new(CoreSliceBackend::new(slicer));
    let version = Version::new([20.0, 30.0, 10.0]);
    let mut request = version.request("bambu-x2d", "PLA", "pla-basic");
    request.plate_type = Some("textured-pei".into());
    let (code, message) = failure(backend.export_project(request).await.unwrap_err());
    assert_eq!(code, "project_export_failed");
    assert!(message.contains("stand-in"), "{message}");
    assert!(!version.output_dir.join("proof").exists());
}
