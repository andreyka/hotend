//! `hotend baseline`: a proven baseline is recorded only with every value stated, shown,
//! replaced and removed, and refused with exit code 2 when a value is outside its field's
//! hard range or the label is not one printable line. Nothing here needs the engine.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::{Command, Output};

fn hotend(data: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hotend"));
    command.arg("--data-dir").arg(data).args(args);
    for name in ["HOTEND_DATA_DIR", "HOTEND_SECRETS", "HOTEND_BAMBU_STUDIO"] {
        command.env_remove(name);
    }
    command.output().expect("cannot run hotend")
}

/// A holder print that held, as the command to seed it is documented.
fn holder_args() -> Vec<&'static str> {
    vec![
        "baseline",
        "set",
        "--printer",
        "bambu-x2d",
        "--plate",
        "textured-pei",
        "--material",
        "pla-generic",
        "--printed-on",
        "2026-11-17",
        "--from-print",
        "holder print, reported good",
        "--level",
        "1",
        "--brim-kind",
        "outer",
        "--brim-width",
        "5",
        "--first-layer-wall-speed",
        "20",
        "--first-layer-infill-speed",
        "30",
        "--first-layer-bed",
        "65",
        "--layers-bed",
        "60",
        "--part-fan-off-layers",
        "3",
        "--aux-fan-first-layers",
        "0",
        "--aux-fan-max",
        "30",
        "--wall-loops",
        "3",
    ]
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn a_baseline_is_recorded_shown_replaced_and_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");

    let empty = hotend(&data, &["baseline", "show"]);
    assert_eq!(empty.status.code(), Some(0), "{}", text(&empty));
    assert!(text(&empty).contains("the rules decide alone"));

    let set = hotend(&data, &holder_args());
    assert_eq!(set.status.code(), Some(0), "{}", text(&set));
    let shown = text(&set);
    assert!(shown.contains("bambu-x2d / textured-pei / pla-generic: proven 2026-11-17"));
    assert!(
        shown.contains("first_layer.bed_c                  65 C"),
        "{shown}"
    );

    let json = hotend(&data, &["baseline", "show", "--json"]);
    let all: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(all.as_array().unwrap().len(), 1);
    let entry = &all[0];
    assert_eq!(entry["values"]["adhesion.brim_width_mm"], 5);
    assert_eq!(entry["values"]["cooling.aux_fan_max_pct"], 30);
    assert_eq!(entry["source"], "holder print, reported good");
    // Its own file in the data folder, not a setting.
    assert!(data.join("proven-baselines.json").is_file());

    let mut wider = holder_args();
    let at = wider.iter().position(|arg| *arg == "--brim-width").unwrap();
    wider[at + 1] = "6";
    let replaced = hotend(&data, &wider);
    assert_eq!(replaced.status.code(), Some(0), "{}", text(&replaced));
    assert!(text(&replaced).contains("replaces the earlier proven baseline"));

    let remove = [
        "baseline",
        "remove",
        "--printer",
        "bambu-x2d",
        "--plate",
        "textured-pei",
        "--material",
        "pla-generic",
    ];
    let removed = hotend(&data, &remove);
    assert_eq!(removed.status.code(), Some(0), "{}", text(&removed));
    assert!(text(&removed).contains("adhesion.brim_width_mm             6 mm"));
    let again = hotend(&data, &remove);
    assert_eq!(again.status.code(), Some(2), "{}", text(&again));
}

/// A record that can no longer be used (its printer is not a profile of this build) is
/// named as a warning by `baseline show`, does not hide the others, and is removed by its
/// key.
#[test]
fn a_record_that_cannot_be_used_is_shown_as_a_warning_and_can_be_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let set = hotend(&data, &holder_args());
    assert_eq!(set.status.code(), Some(0), "{}", text(&set));
    let mut cool = holder_args();
    let at = cool.iter().position(|arg| *arg == "--plate").unwrap();
    cool[at + 1] = "cool";
    assert_eq!(hotend(&data, &cool).status.code(), Some(0));
    let path = data.join("proven-baselines.json");
    let mut file: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for entry in file["baselines"].as_array_mut().unwrap() {
        if entry["plate_type"] == "cool" {
            entry["printer_id"] = "bambu-gone".into();
        }
    }
    std::fs::write(&path, file.to_string()).unwrap();

    let shown = hotend(&data, &["baseline", "show"]);
    assert_eq!(shown.status.code(), Some(0), "{}", text(&shown));
    let stderr = String::from_utf8_lossy(&shown.stderr);
    assert!(
        stderr.contains(
            "warning: the proven baseline bambu-gone / cool / pla-generic cannot be used"
        ),
        "{stderr}"
    );
    let stdout = String::from_utf8_lossy(&shown.stdout);
    assert!(
        stdout.contains("bambu-x2d / textured-pei / pla-generic: proven"),
        "{stdout}"
    );
    let json = hotend(&data, &["baseline", "show", "--json"]);
    let all: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(all.as_array().unwrap().len(), 1);

    let removed = hotend(
        &data,
        &[
            "baseline",
            "remove",
            "--printer",
            "bambu-gone",
            "--plate",
            "cool",
            "--material",
            "pla-generic",
        ],
    );
    assert_eq!(removed.status.code(), Some(0), "{}", text(&removed));
    assert!(
        text(&removed).contains("could not be used"),
        "{}",
        text(&removed)
    );
    let shown = hotend(&data, &["baseline", "show"]);
    assert!(
        !String::from_utf8_lossy(&shown.stderr).contains("warning"),
        "{}",
        text(&shown)
    );
}

/// A record that names no printer, plate and material applies to no plan: it is a warning
/// that points to `baseline remove --invalid`, which removes every record that cannot be
/// used and never a usable one. `--json` stays the usable records alone.
#[test]
fn records_that_cannot_be_used_are_removed_with_invalid_and_usable_ones_are_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let set = hotend(&data, &holder_args());
    assert_eq!(set.status.code(), Some(0), "{}", text(&set));
    let path = data.join("proven-baselines.json");
    let mut file: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let mut gone = file["baselines"][0].clone();
    gone["printer_id"] = "bambu-gone".into();
    gone["plate_type"] = "cool".into();
    let baselines = file["baselines"].as_array_mut().unwrap();
    baselines.push(serde_json::json!({"source": "half a record"}));
    baselines.push(gone);
    std::fs::write(&path, file.to_string()).unwrap();

    let shown = hotend(&data, &["baseline", "show"]);
    assert_eq!(shown.status.code(), Some(0), "{}", text(&shown));
    let stderr = String::from_utf8_lossy(&shown.stderr);
    assert!(
        stderr.contains("warning: the proven baseline ? / ? / ? cannot be used")
            && stderr.contains("applies to no plan")
            && stderr.contains("`hotend baseline remove --invalid`"),
        "{stderr}"
    );
    assert!(
        stderr.contains("bambu-gone / cool / pla-generic cannot be used"),
        "{stderr}"
    );
    let json = hotend(&data, &["baseline", "show", "--json"]);
    assert_eq!(json.status.code(), Some(0), "{}", text(&json));
    let all: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(all.as_array().unwrap().len(), 1);
    assert_eq!(all[0]["printer_id"], "bambu-x2d");

    // --invalid cannot be mixed with a key, and a key needs all three parts.
    let mixed = hotend(
        &data,
        &["baseline", "remove", "--invalid", "--printer", "bambu-x2d"],
    );
    assert_eq!(mixed.status.code(), Some(2), "{}", text(&mixed));
    let partial = hotend(&data, &["baseline", "remove", "--printer", "bambu-x2d"]);
    assert_eq!(partial.status.code(), Some(2), "{}", text(&partial));

    let removed = hotend(&data, &["baseline", "remove", "--invalid"]);
    assert_eq!(removed.status.code(), Some(0), "{}", text(&removed));
    let out = text(&removed);
    assert!(
        out.contains("removed 2 records that could not be used"),
        "{out}"
    );
    assert!(
        out.contains("removed the record of bambu-gone / cool / pla-generic"),
        "{out}"
    );
    let kept: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(kept["baselines"].as_array().unwrap().len(), 1);
    assert_eq!(kept["baselines"][0]["printer_id"], "bambu-x2d");
    let shown = hotend(&data, &["baseline", "show"]);
    assert!(
        !String::from_utf8_lossy(&shown.stderr).contains("warning"),
        "{}",
        text(&shown)
    );
    assert!(text(&shown).contains("bambu-x2d / textured-pei / pla-generic: proven"));

    // Nothing left to remove: said so, nothing changes.
    let again = hotend(&data, &["baseline", "remove", "--invalid"]);
    assert_eq!(again.status.code(), Some(0), "{}", text(&again));
    assert!(
        text(&again).contains("nothing was removed"),
        "{}",
        text(&again)
    );
    let json = hotend(&data, &["baseline", "show", "--json"]);
    let all: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(all.as_array().unwrap().len(), 1);
}

#[test]
fn a_baseline_outside_the_hard_ranges_or_with_a_bad_label_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    for (option, value) in [
        ("--first-layer-wall-speed", "5"),
        ("--first-layer-bed", "130"),
        ("--layers-bed", "70"),
        ("--brim-kind", "inner"),
        ("--brim-width", "0"),
        ("--wall-loops", "9"),
        ("--printed-on", "2026-11-31"),
        ("--printer", "no-such-printer"),
        ("--plate", "glass"),
        ("--from-print", "two\nlines"),
    ] {
        let mut args = holder_args();
        let at = args.iter().position(|arg| *arg == option).unwrap();
        args[at + 1] = value;
        let refused = hotend(&data, &args);
        assert_eq!(
            refused.status.code(),
            Some(2),
            "{option} {value}: {}",
            text(&refused)
        );
    }
    // A value is never taken from a default: leaving one out is an argument error.
    let mut missing = holder_args();
    let at = missing
        .iter()
        .position(|arg| *arg == "--aux-fan-max")
        .unwrap();
    missing.drain(at..at + 2);
    assert_eq!(hotend(&data, &missing).status.code(), Some(2));
    let shown = hotend(&data, &["baseline", "show", "--json"]);
    assert_eq!(String::from_utf8_lossy(&shown.stdout).trim(), "[]");
}
