//! Paths the window may name.
//!
//! Guarantees:
//!
//! - A command that writes into a folder or runs a program takes a path from the window
//!   only if the person chose exactly that path in a system picker during this session
//!   ([`PathGrants::grant`] is called by the picker commands and by nothing else). A page
//!   script therefore cannot make Hotend write into, or run, a place the person did not
//!   pick.
//! - A path is accepted only for what it was picked as ([`Picked`]): a folder picked for
//!   files is not accepted as a slicer program, and a picked program is not accepted as a
//!   folder to write into. The Bambu Connect hand-off folder is a kind of its own
//!   ([`Picked::HandOffFolder`]): only a folder picked under the hand-off picker's title
//!   ([`HANDOFF_TITLE`]) becomes it, and such a folder is not a folder for exports, nor is
//!   a folder picked for an export the hand-off folder.
//! - The folder a snapshot is exported into is a kind of its own too
//!   ([`Picked::SnapshotFolder`]): only a folder picked under the snapshot export picker's
//!   title ([`SNAPSHOT_TITLE`]) becomes it, and `snapshot_export` takes no other folder.
//! - The words on a picker are Hotend's own: the window names a title, and the picker
//!   shows it only if it is one of the titles the interface has ([`folder_title`],
//!   [`program_title`]); any other text is replaced by a plain one. A page script cannot
//!   ask for a folder under a pretext of its own wording.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use hotend_core::ApiError;

/// What a path was picked as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Picked {
    /// A folder to write files into (an export, gauge files, a sliced file).
    Folder,
    /// The Bambu Connect hand-off folder, picked under [`HANDOFF_TITLE`]: where Hotend
    /// places the checked file and the editable project from then on.
    HandOffFolder,
    /// The folder a snapshot's files are exported into, picked under [`SNAPSHOT_TITLE`].
    SnapshotFolder,
    /// A slicer program.
    Program,
}

/// What a folder picked under `title` (as the window asked for it) is granted as: the
/// hand-off folder for exactly [`HANDOFF_TITLE`], the snapshot export folder for exactly
/// [`SNAPSHOT_TITLE`], a folder to write files into for any other title.
pub fn folder_kind(title: &str) -> Picked {
    if title == HANDOFF_TITLE {
        Picked::HandOffFolder
    } else if title == SNAPSHOT_TITLE {
        Picked::SnapshotFolder
    } else {
        Picked::Folder
    }
}

/// The titles the interface gives its folder pickers (`S.project.exportFolderTitle`,
/// `S.print.folder.pickTitle`, `S.print.project.pickTitle` and `S.snapshots.exportTitle` in
/// `src/hotend/strings.ts`, `H.calibration.exportTitle` and `H.handoff.pickTitle` in
/// `src/host/strings.ts`). A test holds them to those files.
pub const FOLDER_TITLES: [&str; 6] = [
    "Choose a folder for the exported files",
    "Choose a folder or USB drive for the sliced file",
    "Choose a folder for the gauge files",
    "Choose the Bambu Connect hand-off folder",
    "Choose a folder for the Bambu Studio project",
    "Save snapshot files",
];

/// The title of the hand-off folder's picker (`H.handoff.pickTitle`): a folder picked under
/// it is granted as [`Picked::HandOffFolder`] and as nothing else.
pub const HANDOFF_TITLE: &str = FOLDER_TITLES[3];

/// The title of the snapshot export's picker (`S.snapshots.exportTitle`): a folder picked
/// under it is granted as [`Picked::SnapshotFolder`] and as nothing else.
pub const SNAPSHOT_TITLE: &str = FOLDER_TITLES[5];

/// The slicers a program can be picked for, by the names the interface shows.
pub const PROGRAM_NAMES: [&str; 2] = ["Bambu Studio", "PrusaSlicer"];

/// The title of a folder picker: `asked` when it is one of [`FOLDER_TITLES`], else a
/// plain one.
pub fn folder_title(asked: &str) -> &'static str {
    FOLDER_TITLES
        .into_iter()
        .find(|title| *title == asked)
        .unwrap_or("Choose a folder")
}

/// The title of a program picker: "Choose the <slicer> program" (`H.slicer.chooseTitle`)
/// for a slicer of [`PROGRAM_NAMES`], else a plain one.
pub fn program_title(asked: &str) -> String {
    PROGRAM_NAMES
        .into_iter()
        .map(|name| format!("Choose the {name} program"))
        .find(|title| title == asked)
        .unwrap_or_else(|| "Choose a slicer program".to_owned())
}

/// The system's pickers. Both block until the person chose or cancelled (`None`).
pub trait Pickers: Send + Sync {
    fn pick_folder(&self, title: &str) -> Option<PathBuf>;
    fn pick_file(&self, title: &str) -> Option<PathBuf>;
}

/// Pickers that never return a choice.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoPickers;

impl Pickers for NoPickers {
    fn pick_folder(&self, _title: &str) -> Option<PathBuf> {
        None
    }
    fn pick_file(&self, _title: &str) -> Option<PathBuf> {
        None
    }
}

/// The paths picked in this session, each with what it was picked as.
#[derive(Debug, Default)]
pub struct PathGrants(Mutex<BTreeSet<(Picked, PathBuf)>>);

impl PathGrants {
    pub fn grant(&self, picked: Picked, path: &Path) {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert((picked, path.to_path_buf()));
    }

    /// `path`, if the person picked it as `wanted`.
    pub fn check(&self, wanted: Picked, path: &Path) -> Result<(), ApiError> {
        let granted = self
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(&(wanted, path.to_path_buf()));
        if granted {
            return Ok(());
        }
        let (what, dialog, next_step) = match wanted {
            Picked::Folder => (
                "as a folder",
                "folder",
                "Choose the folder with the Choose button.",
            ),
            Picked::HandOffFolder => (
                "as the Bambu Connect hand-off folder",
                "hand-off folder",
                "Choose the hand-off folder with its Choose button in Settings, Printer.",
            ),
            Picked::SnapshotFolder => (
                "as the folder for snapshot files",
                "snapshot export",
                "Choose the folder with the snapshot's Export button.",
            ),
            Picked::Program => (
                "as a program",
                "file",
                "Choose the program with the Choose button.",
            ),
        };
        Err(ApiError::InvalidInput {
            message: format!(
                "{} was not chosen {what} in a {dialog} dialog of this session",
                path.display()
            ),
            next_step: next_step.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn only_a_picked_path_is_accepted_and_only_exactly() {
        let grants = PathGrants::default();
        let picked = Path::new("/home/me/prints");
        assert!(grants.check(Picked::Folder, picked).is_err());
        grants.grant(Picked::Folder, picked);
        assert!(grants.check(Picked::Folder, picked).is_ok());
        assert!(grants.check(Picked::Folder, Path::new("/home/me")).is_err());
        assert!(
            grants
                .check(Picked::Folder, Path::new("/home/me/prints/sub"))
                .is_err()
        );
        assert!(
            grants
                .check(Picked::Folder, Path::new("/home/me/prints/../.ssh"))
                .is_err()
        );
    }

    #[test]
    fn a_path_is_accepted_only_for_what_it_was_picked_as() {
        let grants = PathGrants::default();
        let folder = Path::new("/home/me/photos");
        let program = Path::new("/opt/slicer/bin/slicer");
        grants.grant(Picked::Folder, folder);
        grants.grant(Picked::Program, program);
        // A folder picked for files is not a program to run, and a program is not a
        // folder to write into.
        let refused = grants.check(Picked::Program, folder).unwrap_err();
        assert!(
            matches!(&refused, ApiError::InvalidInput { message, .. }
                if message.contains("was not chosen as a program")),
            "{refused:?}"
        );
        assert!(grants.check(Picked::Folder, program).is_err());
        assert!(grants.check(Picked::Folder, folder).is_ok());
        assert!(grants.check(Picked::Program, program).is_ok());
    }

    /// Only the hand-off picker's own title makes a folder the hand-off folder, and that
    /// folder is granted as nothing else.
    #[test]
    fn only_the_hand_off_picker_grants_the_hand_off_folder() {
        assert_eq!(HANDOFF_TITLE, "Choose the Bambu Connect hand-off folder");
        assert_eq!(folder_kind(HANDOFF_TITLE), Picked::HandOffFolder);
        for title in FOLDER_TITLES
            .into_iter()
            .filter(|title| *title != HANDOFF_TITLE && *title != SNAPSHOT_TITLE)
        {
            assert_eq!(folder_kind(title), Picked::Folder, "{title}");
        }
        for pretext in [
            "",
            "Choose the Bambu Connect hand-off folder ",
            "choose the bambu connect hand-off folder",
            "Choose a folder",
        ] {
            assert_eq!(folder_kind(pretext), Picked::Folder, "{pretext:?}");
        }

        let grants = PathGrants::default();
        let export = Path::new("/home/me/exports");
        let handoff = Path::new("/mnt/d/Models/Hotend");
        grants.grant(
            folder_kind("Choose a folder for the exported files"),
            export,
        );
        grants.grant(folder_kind(HANDOFF_TITLE), handoff);
        // A folder picked for an export is not the hand-off folder ...
        let refused = grants.check(Picked::HandOffFolder, export).unwrap_err();
        assert!(
            matches!(&refused, ApiError::InvalidInput { message, next_step, .. }
                if message.contains("was not chosen as the Bambu Connect hand-off folder")
                    && next_step.contains("Settings")),
            "{refused:?}"
        );
        // ... and the hand-off folder is no folder for exports.
        assert!(grants.check(Picked::Folder, handoff).is_err());
        assert!(grants.check(Picked::HandOffFolder, handoff).is_ok());
        assert!(grants.check(Picked::Folder, export).is_ok());
    }

    /// Only the snapshot export picker's own title makes a folder the snapshot folder, and
    /// that folder is granted as nothing else.
    #[test]
    fn only_the_snapshot_picker_grants_the_snapshot_folder() {
        assert_eq!(SNAPSHOT_TITLE, "Save snapshot files");
        assert_eq!(folder_kind(SNAPSHOT_TITLE), Picked::SnapshotFolder);
        for pretext in [
            "Save snapshot files ",
            "save snapshot files",
            "Save snapshot",
        ] {
            assert_eq!(folder_kind(pretext), Picked::Folder, "{pretext:?}");
        }
        let grants = PathGrants::default();
        let export = Path::new("/home/me/exports");
        let snapshots = Path::new("/home/me/snapshots");
        grants.grant(
            folder_kind("Choose a folder for the exported files"),
            export,
        );
        grants.grant(folder_kind(SNAPSHOT_TITLE), snapshots);
        let refused = grants.check(Picked::SnapshotFolder, export).unwrap_err();
        assert!(
            matches!(&refused, ApiError::InvalidInput { message, .. }
                if message.contains("was not chosen as the folder for snapshot files")),
            "{refused:?}"
        );
        assert!(grants.check(Picked::Folder, snapshots).is_err());
        assert!(grants.check(Picked::HandOffFolder, snapshots).is_err());
        assert!(grants.check(Picked::SnapshotFolder, snapshots).is_ok());
    }

    #[test]
    fn a_picker_shows_only_titles_the_interface_has() {
        for title in FOLDER_TITLES {
            assert_eq!(folder_title(title), title);
        }
        for pretext in [
            "Choose your photos folder",
            "",
            "Choose a folder for the exported files ",
            "choose a folder for the gauge files",
        ] {
            assert_eq!(folder_title(pretext), "Choose a folder", "{pretext}");
        }
        assert_eq!(
            program_title("Choose the Bambu Studio program"),
            "Choose the Bambu Studio program"
        );
        assert_eq!(
            program_title("Choose the PrusaSlicer program"),
            "Choose the PrusaSlicer program"
        );
        for pretext in ["Choose your password file", "Choose the sudo program", ""] {
            assert_eq!(
                program_title(pretext),
                "Choose a slicer program",
                "{pretext}"
            );
        }
    }

    /// The table is the interface's own wording: when a title changes there, it changes
    /// here, or the picker falls back to the plain title.
    #[test]
    fn the_titles_are_the_interfaces_own() {
        let read = |relative: &str| {
            let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        };
        let strings = read("../src/hotend/strings.ts") + &read("../src/host/strings.ts");
        for title in FOLDER_TITLES {
            assert!(
                strings.contains(&format!("\"{title}\"")),
                "the interface no longer says \"{title}\""
            );
        }
        assert!(strings.contains("`Choose the ${name} program`"));
        for name in PROGRAM_NAMES {
            assert!(
                hotend_slicer::SlicerKind::ALL
                    .iter()
                    .any(|kind| kind.display_name() == name),
                "{name} is not a slicer Hotend knows"
            );
        }
        assert_eq!(PROGRAM_NAMES.len(), hotend_slicer::SlicerKind::ALL.len());
    }
}
