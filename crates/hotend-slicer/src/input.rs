//! Checks a model file before a slicer is given it.
//!
//! The plate a slice starts from is written by the sandboxed worker, and whatever a
//! sandboxed child wrote is hostile input. A 3MF is an archive, and the slicers read more
//! from one than the model: Bambu Studio and PrusaSlicer both keep project files in it,
//! with slicer settings, per-object settings and G-code to insert at a layer. Whether the
//! command line applies those next to the settings it is given has not been measured, so
//! the question is not left open: a 3MF that holds anything but the model is not handed to
//! a slicer.
//!
//! Guarantees: the archive passes the same checks as a slicer's output (one plain central
//! directory, entry count and declared sizes capped, safe names, no duplicates, no
//! overlapping, encrypted or linked entries), and every entry is one of the parts of a
//! plain 3MF model ([`model_part`]). Nothing is extracted and no entry is read.

use std::path::Path;

use zip::ZipArchive;

use crate::error::SlicerError;
use crate::job::untrusted_text;
use crate::output::{OutputLimits, bambu};

/// Whether `name` is a part of a plain 3MF model: the content types, the package's
/// relationships, the model, and the object files and relationships of a model that is
/// split over several files. Folder entries for those are allowed too.
pub fn model_part(name: &str) -> bool {
    if matches!(
        name,
        "[Content_Types].xml"
            | "_rels/.rels"
            | "_rels/"
            | "3D/"
            | "3D/3dmodel.model"
            | "3D/_rels/"
            | "3D/_rels/3dmodel.model.rels"
            | "3D/Objects/"
    ) {
        return true;
    }
    name.strip_prefix("3D/Objects/")
        .and_then(|file| file.strip_suffix(".model"))
        .is_some_and(|stem| {
            !stem.is_empty()
                && stem.len() <= 120
                && stem
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
                && !stem.starts_with('.')
                && !stem.contains("..")
        })
}

/// Checks the 3MF at `path` (already resolved inside the job folder). `shown` is its path
/// for messages.
pub(crate) fn check_model_archive(
    path: &Path,
    shown: &str,
    limits: &OutputLimits,
) -> Result<(), SlicerError> {
    let refused = |reason: String| SlicerError::InputRefused {
        input: shown.to_owned(),
        reason,
    };
    // The archive checks speak of "the sliced file"; here they are about the model file.
    let archive_problem = |error: SlicerError| match error {
        SlicerError::HostileOutput(reason) | SlicerError::UnusableOutput(reason) => {
            refused(format!("it is not a 3MF archive Hotend accepts ({reason})"))
        }
        other => other,
    };
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options
        .open(path)
        .map_err(|e| SlicerError::io("open", path, e))?;
    bambu::check_directory(&mut file, limits).map_err(archive_problem)?;
    let mut archive = ZipArchive::new(file)
        .map_err(|e| refused(format!("it is not a readable zip archive ({e})")))?;
    bambu::check_entries(&mut archive, limits).map_err(archive_problem)?;
    let mut extra: Vec<String> = archive
        .file_names_raw()
        .filter(|name| !std::str::from_utf8(name).is_ok_and(model_part))
        .map(|name| format!("`{}`", untrusted_text(&String::from_utf8_lossy(name), 80)))
        .collect();
    if extra.is_empty() {
        return Ok(());
    }
    extra.sort();
    let more = extra.len().saturating_sub(8);
    extra.truncate(8);
    let mut list = extra.join(", ");
    if more > 0 {
        list.push_str(&format!(" and {more} more"));
    }
    Err(refused(format!("it holds {list}")))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn only_the_parts_of_a_plain_model_are_model_parts() {
        for good in [
            "[Content_Types].xml",
            "_rels/.rels",
            "3D/3dmodel.model",
            "3D/_rels/3dmodel.model.rels",
            "3D/Objects/object_1.model",
            "3D/Objects/part-2.b.model",
            "3D/",
            "3D/Objects/",
        ] {
            assert!(model_part(good), "{good}");
        }
        for bad in [
            "Metadata/project_settings.config",
            "Metadata/model_settings.config",
            "Metadata/custom_gcode_per_layer.xml",
            "Metadata/Slic3r_PE.config",
            "Metadata/plate_1.gcode",
            "Metadata/",
            "3D/3dmodel.model.bak",
            "3d/3dmodel.model",
            "3D/Objects/.model",
            "3D/Objects/a/b.model",
            "3D/Objects/..model",
            "3D/Objects/part.gcode",
            "3D/other.model",
            "readme.txt",
            "",
        ] {
            assert!(!model_part(bad), "{bad}");
        }
    }

    #[test]
    fn an_archive_with_anything_else_is_refused_and_names_it() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("plate.3mf");
        let limits = OutputLimits::default();
        let write = |entries: &[(&str, &[u8])]| {
            std::fs::write(&path, bambu::tests::archive(entries)).unwrap();
        };
        let plain: [(&str, &[u8]); 3] = [
            ("[Content_Types].xml", b"<Types/>"),
            ("_rels/.rels", b"<Relationships/>"),
            ("3D/3dmodel.model", b"<model/>"),
        ];
        write(&plain);
        check_model_archive(&path, "input/plate.3mf", &limits).unwrap();

        let mut with_settings = plain.to_vec();
        with_settings.push(("Metadata/project_settings.config", b"{}"));
        with_settings.push(("Metadata/custom_gcode_per_layer.xml", b"<x/>"));
        write(&with_settings);
        let error = check_model_archive(&path, "input/plate.3mf", &limits).unwrap_err();
        assert_eq!(error.code(), "input_refused");
        let text = error.to_string();
        assert!(
            text.contains("input/plate.3mf")
                && text.contains(
                    "`Metadata/custom_gcode_per_layer.xml`, `Metadata/project_settings.config`"
                ),
            "{text}"
        );

        // More entries than the limit, or a file that is no archive: refused as an input,
        // not reported as a sliced file.
        std::fs::write(&path, b"not a zip archive, just some text").unwrap();
        let error = check_model_archive(&path, "input/plate.3mf", &limits).unwrap_err();
        assert_eq!(error.code(), "input_refused");
        assert!(!error.to_string().contains("sliced file"), "{error}");
        write(&plain);
        let few = OutputLimits {
            max_zip_entries: 2,
            ..limits
        };
        let error = check_model_archive(&path, "input/plate.3mf", &few).unwrap_err();
        assert_eq!(error.code(), "input_refused");
    }
}
