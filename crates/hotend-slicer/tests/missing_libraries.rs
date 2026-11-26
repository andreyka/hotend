//! A slicer the dynamic loader cannot start is refused, and the refusal names the missing
//! library in plain words (what `hotend doctor` shows).
//!
//! The stand-in prints what the Linux loader printed for the unpacked Bambu Studio
//! 02.09.01.70 AppImage on an Ubuntu 24.04 without the WebKitGTK 4.1 packages, and exits
//! with the loader's code 127. It runs inside the sandbox, as every probe does.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used)]

mod common;

use std::os::unix::fs::PermissionsExt;

use common::*;
use hotend_slicer::detect::{Candidate, probe};
use hotend_slicer::{Found, SlicerKind, Verdict, VersionPolicy};

#[test]
fn a_slicer_with_a_missing_shared_library_is_refused_with_its_name() {
    if !sandbox_ready("a_slicer_with_a_missing_shared_library_is_refused_with_its_name") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("bambu-studio/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let program = bin.join("bambu-studio");
    std::fs::write(
        &program,
        "#!/bin/sh\necho \"$0: error while loading shared libraries: libwebkit2gtk-4.1.so.0: \
         cannot open shared object file: No such file or directory\" >&2\nexit 127\n",
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();

    let candidate = Candidate {
        kind: SlicerKind::BambuStudio,
        path: program,
        found: Found::Override,
    };
    let installation = probe(&candidate, &VersionPolicy::builtin().unwrap(), None);
    assert_eq!(installation.version, None);
    let Verdict::Refuse(reason) = &installation.verdict else {
        panic!("not refused: {installation:?}");
    };
    assert!(
        reason.contains("shared library libwebkit2gtk-4.1.so.0 is missing"),
        "{reason}"
    );
}
