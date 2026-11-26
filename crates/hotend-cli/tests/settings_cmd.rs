//! `hotend settings`: the Bambu Connect hand-off folder is chosen, shown and cleared from
//! the command line, and a folder a hand-off could not use is refused with exit code 2
//! without changing the settings. Nothing here needs the engine.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::{Command, Output};

use hotend_core::HostKind;

fn hotend(data: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hotend"));
    command.arg("--data-dir").arg(data).args(args);
    for name in ["HOTEND_DATA_DIR", "HOTEND_SECRETS", "HOTEND_BAMBU_STUDIO"] {
        command.env_remove(name);
    }
    command.output().expect("cannot run hotend")
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn shown_folder(data: &Path) -> serde_json::Value {
    let json = hotend(data, &["settings", "show", "--json"]);
    assert_eq!(json.status.code(), Some(0), "{}", text(&json));
    let settings: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    settings["handoff_folder"].clone()
}

#[test]
fn the_hand_off_folder_is_set_shown_and_cleared() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");

    let shown = hotend(&data, &["settings", "show"]);
    assert_eq!(shown.status.code(), Some(0), "{}", text(&shown));
    assert!(
        text(&shown).contains("hand-off folder:    not chosen"),
        "{}",
        text(&shown)
    );
    assert_eq!(shown_folder(&data), serde_json::Value::Null);

    // A folder that does not exist, and a relative path, are refused on every host.
    let missing = tmp.path().join("missing");
    let refused = hotend(
        &data,
        &[
            "settings",
            "set",
            "--handoff-folder",
            missing.to_str().unwrap(),
        ],
    );
    assert_eq!(refused.status.code(), Some(2), "{}", text(&refused));
    assert!(text(&refused).contains("not changed"), "{}", text(&refused));
    assert_eq!(shown_folder(&data), serde_json::Value::Null);
    let relative = hotend(&data, &["settings", "set", "--handoff-folder", "Prints"]);
    assert_ne!(relative.status.code(), Some(0), "{}", text(&relative));
    assert_eq!(shown_folder(&data), serde_json::Value::Null);

    // An existing folder: kept and shown off WSL; under WSL a folder off the Windows
    // drives is refused with the sentence that says what to do.
    let inbox = tmp.path().join("inbox");
    std::fs::create_dir(&inbox).unwrap();
    let set = hotend(
        &data,
        &[
            "settings",
            "set",
            "--handoff-folder",
            inbox.to_str().unwrap(),
        ],
    );
    let inbox = if HostKind::detect() == HostKind::Wsl {
        assert_eq!(set.status.code(), Some(2), "{}", text(&set));
        assert!(text(&set).contains("Windows drive"), "{}", text(&set));
        assert_eq!(shown_folder(&data), serde_json::Value::Null);
        // A folder that exists on a Windows drive is only read here, never written to.
        let windows = Path::new("/mnt/c/Windows");
        if !windows.is_dir() {
            return;
        }
        let set = hotend(
            &data,
            &["settings", "set", "--handoff-folder", "/mnt/c/Windows"],
        );
        assert_eq!(set.status.code(), Some(0), "{}", text(&set));
        windows.to_path_buf()
    } else {
        assert_eq!(set.status.code(), Some(0), "{}", text(&set));
        inbox
    };
    assert_eq!(shown_folder(&data), serde_json::json!(inbox));
    let shown = hotend(&data, &["settings", "show"]);
    assert!(
        text(&shown).contains(&format!("hand-off folder:    {}", inbox.display())),
        "{}",
        text(&shown)
    );

    // Cleared, and not both at once.
    let both = hotend(
        &data,
        &[
            "settings",
            "set",
            "--handoff-folder",
            inbox.to_str().unwrap(),
            "--clear-handoff-folder",
        ],
    );
    assert_eq!(both.status.code(), Some(2), "{}", text(&both));
    let cleared = hotend(&data, &["settings", "set", "--clear-handoff-folder"]);
    assert_eq!(cleared.status.code(), Some(0), "{}", text(&cleared));
    assert_eq!(shown_folder(&data), serde_json::Value::Null);
}

/// A hand-off folder that cannot be used is refused before anything is written: the slicer
/// and every other setting named in the same command stay as they were, and the message
/// says that nothing changed.
#[test]
fn a_bad_hand_off_folder_changes_nothing_else_named_with_it() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let all = |data: &Path| {
        let json = hotend(data, &["settings", "show", "--json"]);
        assert_eq!(json.status.code(), Some(0), "{}", text(&json));
        serde_json::from_slice::<serde_json::Value>(&json.stdout).unwrap()
    };
    let before = all(&data);
    let program = tmp.path().join("bambu-studio");
    std::fs::write(&program, b"#!/bin/sh\n").unwrap();
    let missing = tmp.path().join("missing");
    let slicer = format!("bambu-studio={}", program.display());
    let refused = hotend(
        &data,
        &[
            "settings",
            "set",
            "--slicer",
            &slicer,
            "--max-turns",
            "7",
            "--handoff-folder",
            missing.to_str().unwrap(),
        ],
    );
    assert_eq!(refused.status.code(), Some(2), "{}", text(&refused));
    assert!(
        text(&refused).contains("the settings were not changed"),
        "{}",
        text(&refused)
    );
    let after = all(&data);
    assert_eq!(after, before);
    assert_eq!(after["slicer_executables"], before["slicer_executables"]);
    assert_ne!(after["max_turns"], serde_json::json!(7));
}
