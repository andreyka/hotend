//! The editable Bambu Studio project, made in two runs of the program (A and C).
//!
//! Run A writes a project from the plate and the settings files Hotend flattened and put its
//! settings on top of. The command line leaves the project's list of changed settings
//! (`different_settings_to_system` in `Metadata/project_settings.config`) empty, and the window
//! then replaces every changed value with the system preset's (measured on 02.09.01.70). So
//! Hotend makes one edit of its own: [`fill_difference_list`] replaces only the text inside
//! that one list, with the key names the profile helper reported as changed (Hotend's keymap
//! keys, sorted and joined by `;` as the window writes them: process, filament 1, printer),
//! never with text from a file. It then checks that every other entry of the archive is
//! byte-identical uncompressed and in the same order, and that the settings file differs only in
//! that list. Run C then slices the filled project alone, admitted only by the SHA-256 held in
//! memory, and the proof judges its sliced file ([`crate::Slicer::export_project`]).

use std::io::{Cursor, Read, Write};
use std::path::PathBuf;

use serde::Serialize;
use zip::ZipArchive;

use crate::error::SlicerError;
use crate::job::SliceResult;
use crate::output::{OutputLimits, bambu};
use crate::proof::ProofReport;

/// The settings file of a project.
pub const PROJECT_SETTINGS: &str = "Metadata/project_settings.config";
/// The list a project names its changed settings in.
pub const DIFFERENCE_KEY: &str = "different_settings_to_system";
/// Largest settings file that is read.
const MAX_SETTINGS_BYTES: u64 = 8 * 1024 * 1024;

/// Keymap keys that the window's own projects do not list as a difference to the system
/// preset although Hotend writes them: the filament colour (a holder sliced in the window
/// states `#898989` and lists no colour key). Measured on 02.09.01.70.
pub const NOT_LISTED: [&str; 1] = ["filament_colour"];

/// Keys the program keeps as mirrors of a keymap key: it states them with the same value
/// whether or not they were written (measured: a test slice of 18 November 2026 states
/// `optional_cooling_fan_speed` 30 and `first_x_layer_optional_fan_speed` 0 although only the
/// auxiliary fan keys were written, and so does the exported project), and the window lists
/// them beside the keymap key (the holder slice). When the keymap key is listed, its
/// mirror is listed too.
pub const LIST_MIRRORS: [(&str, &str); 2] = [
    ("additional_cooling_fan_speed", "optional_cooling_fan_speed"),
    (
        "first_x_layer_fan_speed",
        "first_x_layer_optional_fan_speed",
    ),
];

/// The changed keys per preset, in the order of the project's list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ProjectKeys {
    pub process: Vec<String>,
    /// Filament 1; version 1 has one filament.
    pub filament: Vec<String>,
    /// The printer preset: always empty (the window keeps the nozzle inventory without it).
    pub printer: Vec<String>,
}

impl ProjectKeys {
    /// The three texts of the list: each preset's keys sorted and joined by `;`.
    pub fn texts(&self) -> [String; 3] {
        let join = |keys: &[String]| {
            let mut keys: Vec<&str> = keys
                .iter()
                .map(String::as_str)
                .filter(|key| !NOT_LISTED.contains(key))
                .collect();
            let mirrors: Vec<&str> = LIST_MIRRORS
                .iter()
                .filter(|(source, _)| keys.contains(source))
                .map(|(_, mirror)| *mirror)
                .collect();
            keys.extend(mirrors);
            keys.sort_unstable();
            keys.dedup();
            keys.join(";")
        };
        [
            join(&self.process),
            join(&self.filament),
            join(&self.printer),
        ]
    }
}

/// An editable project whose sliced form passed the proof.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectExport {
    /// The filled project, inside the job folder.
    pub project: PathBuf,
    /// Its SHA-256, lower-case hex, computed when it was written and checked again before it
    /// is returned.
    pub sha256: String,
    /// The keys the project lists as changed, per preset.
    pub keys: ProjectKeys,
    /// The proof of run C's sliced file.
    pub proof: ProofReport,
    /// Run C's slice.
    pub result: SliceResult,
}

fn refused(message: impl Into<String>) -> SlicerError {
    SlicerError::Project {
        code: "project_export_failed",
        message: format!(
            "the editable project cannot be used: {}. Nothing was handed over",
            message.into()
        ),
    }
}

/// Whether a list text holds only key names joined by `;`.
fn plain_list(text: &str) -> bool {
    text.split(';')
        .all(|key| key.is_empty() || crate::keymap::valid_key(key))
}

/// Where the three empty strings of the list are in the settings text: the byte offset of each
/// opening quote. `None` unless the key occurs exactly once and its value is a list of exactly
/// three empty strings, as the command line writes it.
fn empty_list_slots(text: &str) -> Option<[usize; 3]> {
    let quoted = format!("\"{DIFFERENCE_KEY}\"");
    let mut found = text.match_indices(&quoted);
    let (at, _) = found.next()?;
    if found.next().is_some() {
        return None;
    }
    let bytes = text.as_bytes();
    let mut index = at + quoted.len();
    let skip = |index: &mut usize| {
        while bytes.get(*index).is_some_and(u8::is_ascii_whitespace) {
            *index += 1;
        }
    };
    skip(&mut index);
    if bytes.get(index) != Some(&b':') {
        return None;
    }
    index += 1;
    skip(&mut index);
    if bytes.get(index) != Some(&b'[') {
        return None;
    }
    index += 1;
    let mut slots = Vec::new();
    loop {
        skip(&mut index);
        if bytes.get(index..index + 2) != Some(b"\"\"") {
            return None;
        }
        slots.push(index);
        index += 2;
        skip(&mut index);
        match bytes.get(index) {
            Some(b',') => index += 1,
            Some(b']') => break,
            _ => return None,
        }
        if slots.len() > 3 {
            return None;
        }
    }
    slots.try_into().ok()
}

/// Reads every entry of an archive, in order, uncompressed, with the archive checks of the
/// output reader.
fn entries(bytes: &[u8], limits: &OutputLimits) -> Result<Vec<(String, Vec<u8>)>, SlicerError> {
    let mut reader = Cursor::new(bytes);
    bambu::check_directory(&mut reader, limits).map_err(|e| refused(e.to_string()))?;
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| refused(format!("the project is not a readable archive ({e})")))?;
    bambu::check_entries(&mut archive, limits).map_err(|e| refused(e.to_string()))?;
    let mut list = Vec::new();
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|e| refused(format!("an entry cannot be read ({e})")))?;
        let name = entry
            .name()
            .map_err(|_| refused("an entry name is not text"))?
            .into_owned();
        let mut data = Vec::new();
        entry
            .take(limits.max_entry_bytes + 1)
            .read_to_end(&mut data)
            .map_err(|e| refused(format!("`{name}` cannot be read ({e})")))?;
        list.push((name, data));
    }
    Ok(list)
}

/// Fills the project's list of changed settings with `keys` and returns the new archive.
/// Nothing else changes: the check after writing compares every other entry uncompressed and
/// the order of all entries, and the settings file as JSON without the list.
pub fn fill_difference_list(
    project: &[u8],
    keys: &ProjectKeys,
    limits: &OutputLimits,
) -> Result<Vec<u8>, SlicerError> {
    let texts = keys.texts();
    if !texts.iter().all(|text| plain_list(text)) {
        return Err(refused("a changed setting is not a plain key name"));
    }
    let before = entries(project, limits)?;
    let settings_at = before
        .iter()
        .position(|(name, _)| name == PROJECT_SETTINGS)
        .ok_or_else(|| refused(format!("it holds no `{PROJECT_SETTINGS}`")))?;
    let settings = &before[settings_at].1;
    if settings.len() as u64 > MAX_SETTINGS_BYTES {
        return Err(refused("its settings file is too large"));
    }
    let text = std::str::from_utf8(settings).map_err(|_| refused("its settings are not text"))?;
    let slots = empty_list_slots(text).ok_or_else(|| {
        refused(format!(
            "its `{DIFFERENCE_KEY}` is not the list of three empty texts the command line writes"
        ))
    })?;
    let mut filled = String::with_capacity(text.len() + 256);
    let mut from = 0;
    for (slot, list) in slots.iter().zip(&texts) {
        filled.push_str(&text[from..=*slot]);
        filled.push_str(list);
        from = slot + 1;
    }
    filled.push_str(&text[from..]);

    // The new archive: every entry copied raw, in order, except the settings file.
    let mut source = ZipArchive::new(Cursor::new(project))
        .map_err(|e| refused(format!("the project is not a readable archive ({e})")))?;
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for index in 0..source.len() {
        if index == settings_at {
            let entry = source
                .by_index(index)
                .map_err(|e| refused(format!("an entry cannot be read ({e})")))?;
            let mut options =
                zip::write::SimpleFileOptions::default().compression_method(entry.compression());
            if let Some(time) = entry.last_modified() {
                options = options.last_modified_time(time);
            }
            drop(entry);
            writer
                .start_file(PROJECT_SETTINGS, options)
                .and_then(|()| writer.write_all(filled.as_bytes()).map_err(Into::into))
                .map_err(|e| refused(format!("the settings cannot be written ({e})")))?;
        } else {
            let entry = source
                .by_index_raw(index)
                .map_err(|e| refused(format!("an entry cannot be read ({e})")))?;
            writer
                .raw_copy_file(entry)
                .map_err(|e| refused(format!("an entry cannot be copied ({e})")))?;
        }
    }
    let written = writer
        .finish()
        .map_err(|e| refused(format!("the archive cannot be finished ({e})")))?
        .into_inner();

    // The check: same entries in the same order, byte-identical uncompressed, except the
    // settings file, which differs only in the list.
    let after = entries(&written, limits)?;
    let same_order =
        before.len() == after.len() && before.iter().zip(&after).all(|((a, _), (b, _))| a == b);
    if !same_order {
        return Err(refused(
            "the filled archive does not keep the entries and their order",
        ));
    }
    for (index, ((name, old), (_, new))) in before.iter().zip(&after).enumerate() {
        if index != settings_at && old != new {
            return Err(refused(format!(
                "`{name}` changed while the list was filled"
            )));
        }
    }
    let parse = |bytes: &[u8]| serde_json::from_slice::<serde_json::Value>(bytes).ok();
    let (Some(mut old), Some(mut new)) =
        (parse(&before[settings_at].1), parse(&after[settings_at].1))
    else {
        return Err(refused("its settings file is not JSON"));
    };
    let new_list = new
        .as_object_mut()
        .and_then(|object| object.remove(DIFFERENCE_KEY));
    let old_list = old
        .as_object_mut()
        .and_then(|object| object.remove(DIFFERENCE_KEY));
    let expected: Vec<serde_json::Value> =
        texts.iter().cloned().map(serde_json::Value::from).collect();
    if old != new
        || old_list != Some(serde_json::Value::from(vec![""; 3]))
        || new_list != Some(serde_json::Value::Array(expected))
    {
        return Err(refused(
            "the settings file changed beyond the list of changed settings",
        ));
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::output::bambu::tests::archive;

    /// A settings file in the shape the command line writes, made up for this test.
    const SETTINGS: &str = "{\n    \"brim_width\": \"5\",\n    \"different_settings_to_system\": [\n        \"\",\n        \"\",\n        \"\"\n    ],\n    \"wall_loops\": \"3\"\n}\n";

    fn keys() -> ProjectKeys {
        ProjectKeys {
            process: vec![
                "wall_loops".into(),
                "curr_bed_type".into(),
                "brim_object_gap".into(),
            ],
            filament: vec![
                "textured_plate_temp".into(),
                "filament_colour".into(),
                "additional_cooling_fan_speed".into(),
            ],
            printer: Vec::new(),
        }
    }

    fn project(settings: &str) -> Vec<u8> {
        archive(&[
            ("[Content_Types].xml", b"<Types/>".as_slice()),
            ("3D/3dmodel.model", b"<model/>".as_slice()),
            (PROJECT_SETTINGS, settings.as_bytes()),
            ("Metadata/model_settings.config", b"<config/>".as_slice()),
        ])
    }

    #[test]
    fn only_the_text_inside_the_list_changes() {
        let filled =
            fill_difference_list(&project(SETTINGS), &keys(), &OutputLimits::default()).unwrap();
        let after = entries(&filled, &OutputLimits::default()).unwrap();
        let names: Vec<&str> = after.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            [
                "[Content_Types].xml",
                "3D/3dmodel.model",
                PROJECT_SETTINGS,
                "Metadata/model_settings.config"
            ]
        );
        let text = String::from_utf8(after[2].1.clone()).unwrap();
        // Sorted, joined by `;`, the layout kept, the colour not listed and the mirror of the
        // auxiliary fan limit listed beside it (measured).
        assert_eq!(
            text,
            SETTINGS.replacen(
                "[\n        \"\",\n        \"\",\n        \"\"\n    ]",
                "[\n        \"brim_object_gap;curr_bed_type;wall_loops\",\n        \"additional_cooling_fan_speed;optional_cooling_fan_speed;textured_plate_temp\",\n        \"\"\n    ]",
                1
            )
        );
        assert_eq!(after[0].1, b"<Types/>");
    }

    #[test]
    fn anything_but_the_command_lines_empty_list_is_refused() {
        for settings in [
            // Already filled, two lists, too few or too many entries, not a list.
            SETTINGS.replacen("\"\",\n        \"\",\n        \"\"", "\"x\", \"\", \"\"", 1),
            SETTINGS.replacen("\"wall_loops\"", "\"different_settings_to_system\"", 1),
            SETTINGS.replacen("\"\",\n        \"\",\n        \"\"", "\"\", \"\"", 1),
            SETTINGS.replacen(
                "\"\",\n        \"\",\n        \"\"",
                "\"\", \"\", \"\", \"\"",
                1,
            ),
            SETTINGS.replacen(
                "[\n        \"\",\n        \"\",\n        \"\"\n    ]",
                "\"\"",
                1,
            ),
        ] {
            let error =
                fill_difference_list(&project(&settings), &keys(), &OutputLimits::default())
                    .unwrap_err();
            assert_eq!(error.code(), "project_export_failed", "{settings}");
        }
        // A project without settings, and names that are not keys.
        let bare = archive(&[("3D/3dmodel.model", b"<model/>".as_slice())]);
        assert!(fill_difference_list(&bare, &keys(), &OutputLimits::default()).is_err());
        let mut odd = keys();
        odd.process.push("wall\"loops".into());
        assert!(fill_difference_list(&project(SETTINGS), &odd, &OutputLimits::default()).is_err());
    }
}
