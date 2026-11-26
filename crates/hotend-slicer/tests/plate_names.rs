//! The build plates are named in two tables: the slicer's (`profiles/system-profiles.toml`,
//! the plates a slice can be asked for) and the printer layer's
//! (`hotend-printer/profiles/bambu.toml`, the plates a sliced file can be checked for).
//! A plate travels from the settings to both by its id, and the check compares the name
//! the file states with the name of its own row, so the two tables must agree on both.

#![allow(clippy::unwrap_used)]

use std::path::Path;

fn table(path: &str) -> toml::Table {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    toml::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn text<'a>(row: &'a toml::Value, key: &str) -> &'a str {
    row.get(key)
        .and_then(toml::Value::as_str)
        .unwrap_or_else(|| panic!("a row has no `{key}`: {row:?}"))
}

#[test]
fn every_plate_the_check_knows_is_a_plate_the_slicer_is_asked_for_by_the_same_id_and_name() {
    let slicer = table("profiles/system-profiles.toml");
    let bed_types = slicer["bed_type"].as_array().unwrap();
    let printer = table("../hotend-printer/profiles/bambu.toml");
    let mut checked = 0;
    for printer in printer["printer"].as_array().unwrap() {
        for plate in printer["plate"].as_array().unwrap() {
            let (id, name) = (text(plate, "id"), text(plate, "name"));
            let asked: Vec<&toml::Value> = bed_types
                .iter()
                .filter(|bed| text(bed, "id") == id)
                .collect();
            assert_eq!(asked.len(), 1, "plate `{id}` in the slicer's table");
            // The slicer writes `slicer_value` into the file as `curr_bed_type`, and the
            // check holds that statement to the plate's `name`.
            assert_eq!(text(asked[0], "slicer_value"), name, "plate `{id}`");
            assert_eq!(text(asked[0], "name"), name, "plate `{id}`");
            checked += 1;
        }
    }
    assert!(checked >= 2, "the printer table names the plates it checks");
    // The stand-in slices for a plate the check knows, so its file can be reviewed.
    let stand_in = text(&toml::Value::Table(slicer.clone()), "stand_in_bed_type").to_owned();
    assert!(
        printer["printer"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|printer| printer["plate"].as_array().unwrap())
            .any(|plate| text(plate, "id") == stand_in),
        "the stand-in's plate `{stand_in}` has no row in the printer table"
    );
}
