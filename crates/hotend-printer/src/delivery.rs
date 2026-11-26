//! Three ways to get a checked job to a printer, behind one interface.
//!
//! | Path | Type | What Hotend can do |
//! | --- | --- | --- |
//! | A printer Hotend talks to | [`PrinterDelivery`] | upload, then start after confirmation, pause, resume, stop, live status, AMS contents: for real over the LAN in Developer Mode, as a simulation on the simulated printer |
//! | Bambu Connect hand-off | [`BambuConnectHandOff`] | place the file and build the `bambu-connect://import-file` link; the user sends it from Bambu Connect |
//! | Export | [`ExportToFolder`] | write the file to a folder or USB drive |
//!
//! Guarantees:
//! - every path takes a [`CheckedJob`], so nothing that failed the linter can be delivered;
//! - a file is placed by writing the job's own copy of the checked bytes (never the
//!   file on disk again): the copy is hashed while it is written and discarded unless it
//!   has the job's hash; the destination name is the job's cleaned label followed by the
//!   first 32 hex digits of the file's SHA-256, so two different files never get the
//!   same name, and it stays inside the chosen folder;
//! - [`Delivery::capabilities`] states what each path can and cannot do and what it costs
//!   the user, so the interface can explain the trade-off without knowing the paths. A
//!   [`PrinterDelivery`] has the capabilities of the printer it delivers to
//!   ([`Printer::capabilities`]): the simulated printer never describes itself with a real
//!   printer's abilities and limits;
//! - the hand-off link is released only against a [`HumanConfirmation`] for that job,
//!   and only while the placed file still holds the confirmed bytes (it is read again)
//!   and still resolves to a file directly inside the chosen [`HandOffFolder`]. The link
//!   is a [`BambuConnectLink`], which nothing else can make: its text always starts with
//!   `bambu-connect://import-file?path=` and names that one file. Opening it is the job
//!   of a [`crate::opener::LinkOpener`], which the caller holds; this module starts no
//!   program. What Bambu Connect then reads from that path, and what it shows, is outside
//!   this crate: a program that can write the folder after the link was released can
//!   still change the file, so the folder should be one only this user can write.
//!
//! The link format is Bambu's documented one (wiki, "Third-party integration"):
//! `bambu-connect://import-file?path=<encoded path>&name=<encoded name>&version=1.0.0`.
//! The path is the Windows form with backslashes under WSL and on Windows
//! ([`crate::handoff`]), encoded as `encodeURIComponent` encodes it: the form proven with
//! Bambu Connect on 2026-11-18.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::handoff::{HandOffFolder, HandOffPathError, PlacedPath};
use crate::job::{CheckedJob, JobError, hex};
use crate::printer::{Printer, PrinterError, UploadedJob};
use crate::token::{HumanConfirmation, TokenError};

/// Which path a delivery is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryKind {
    /// The printer simulated inside Hotend: the job runs, nothing is made.
    Simulated,
    /// Direct control over the local network, printer in Developer Mode.
    Lan,
    /// Hand-off to Bambu Connect.
    BambuConnect,
    /// Export to a folder or USB drive.
    Export,
}

/// What a delivery path can and cannot do, for the interface to show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Capabilities {
    /// The path.
    pub kind: DeliveryKind,
    /// Short name.
    pub name: &'static str,
    /// Hotend puts the file on the printer itself.
    pub sends_file: bool,
    /// Hotend can start the print (after the confirmation dialog).
    pub starts_print: bool,
    /// The path takes pause, resume and stop, and speed and light, from Hotend.
    pub controls_print: bool,
    /// Hotend sees live status and the AMS contents.
    pub live_status: bool,
    /// Hotend learns how the print ended.
    pub reports_result: bool,
    /// What the path does, one line each.
    pub can: Vec<&'static str>,
    /// What it does not do.
    pub cannot: Vec<&'static str>,
    /// What choosing it costs the user.
    pub cost: &'static str,
    /// What it needs to work; empty when it needs nothing.
    pub needs: Vec<&'static str>,
}

impl Capabilities {
    /// The simulated printer: the whole path runs, and nothing is made. Every sentence is
    /// about the simulation, none about a real printer.
    pub fn simulated() -> Self {
        Self {
            kind: DeliveryKind::Simulated,
            name: "Simulated printer",
            sends_file: true,
            starts_print: true,
            controls_print: true,
            live_status: true,
            reports_result: true,
            can: vec![
                "Runs the confirmed job on a printer simulated inside Hotend",
                "Shows progress as a real printer would report it",
                "Stops the job when you ask",
            ],
            cannot: vec![
                "Make anything: nothing is heated, moved or printed",
                "Tell you how the part comes out of a real printer",
            ],
            cost: "Nothing is printed: the job runs on a printer simulated inside Hotend, with simulated time and a simulated AMS.",
            needs: Vec::new(),
        }
    }

    /// A real printer controlled over the local network in Developer Mode.
    pub fn lan() -> Self {
        Self {
            kind: DeliveryKind::Lan,
            name: "Direct control (Developer Mode)",
            sends_file: true,
            starts_print: true,
            controls_print: true,
            live_status: true,
            reports_result: true,
            can: vec![
                "Uploads the sliced job to the printer",
                "Starts it after you confirm",
                "Pauses, resumes and stops the print",
                "Shows live status and what is loaded in the AMS",
            ],
            cannot: vec![
                "Work while the printer is connected to Bambu Cloud",
                "Write to the printer's built-in storage (a USB drive is needed)",
            ],
            cost: "The printer leaves Bambu Cloud: no Bambu Handy app, no MakerWorld print history, and Bambu gives no support in this mode. Anyone on the network with the access code can control the printer.",
            needs: vec![
                "LAN Only mode and Developer Mode switched on at the printer",
                "The access code from the printer's screen",
                "A USB drive in the printer",
            ],
        }
    }
}

/// Why a delivery failed.
#[derive(Debug, thiserror::Error)]
pub enum DeliveryError {
    /// The destination folder cannot be used.
    #[error("the folder `{0}` cannot be used: {1}")]
    Folder(String, String),
    /// The job's file is unusable or changed.
    #[error(transparent)]
    Job(#[from] JobError),
    /// The printer failed.
    #[error(transparent)]
    Printer(#[from] PrinterError),
    /// The confirmation does not cover this.
    #[error(transparent)]
    Confirmation(#[from] TokenError),
    /// Writing failed.
    #[error("cannot write `{0}`: {1}")]
    Write(String, std::io::Error),
    /// The hand-off folder or the placed file's path cannot be used.
    #[error(transparent)]
    Path(#[from] HandOffPathError),
    /// The sliced file lacks entries Bambu Connect needs to import it
    /// ([`bambu_connect_missing`]).
    #[error("{}", not_importable_message(.0))]
    NotImportable(Vec<String>),
}

/// The result of a delivery.
#[derive(Debug)]
pub enum Delivered {
    /// The file is on the printer's storage; start it with
    /// [`crate::printer::start_print`].
    OnPrinter(UploadedJob),
    /// The file is in place for Bambu Connect; the link is released by
    /// [`HandOff::link`].
    HandOff(HandOff),
    /// The file was written to the folder.
    Exported {
        /// Where it is.
        path: PathBuf,
    },
}

/// One way to deliver a checked job.
pub trait Delivery {
    /// What this path can and cannot do.
    fn capabilities(&self) -> Capabilities;

    /// Delivers the job.
    fn deliver(&mut self, job: &CheckedJob) -> Result<Delivered, DeliveryError>;
}

/// Delivery to a printer Hotend controls (the simulated printer, or the LAN transport).
/// What it can do is what that printer says it can do.
pub struct PrinterDelivery<'a> {
    printer: &'a mut dyn Printer,
}

impl<'a> PrinterDelivery<'a> {
    /// Delivery to `printer`.
    pub fn new(printer: &'a mut dyn Printer) -> Self {
        Self { printer }
    }
}

impl Delivery for PrinterDelivery<'_> {
    fn capabilities(&self) -> Capabilities {
        self.printer.capabilities()
    }

    fn deliver(&mut self, job: &CheckedJob) -> Result<Delivered, DeliveryError> {
        Ok(Delivered::OnPrinter(self.printer.upload(job)?))
    }
}

/// Characters `encodeURIComponent` leaves alone besides letters and digits.
const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

/// The start of every Bambu Connect import link.
pub const LINK_PREFIX: &str = "bambu-connect://import-file?path=";

/// The longest `name` a link carries.
const LINK_NAME_CHARS: usize = 64;

/// The import link's text for a file at `path` (as Bambu Connect must read it: the
/// Windows form under WSL and on Windows) shown as `name`. Both are encoded as
/// `encodeURIComponent` encodes them, so nothing in either can add a parameter or end the
/// link. This is the encoding only: the link an opener takes is a [`BambuConnectLink`].
pub fn bambu_connect_link(path: &str, name: &str) -> String {
    format!(
        "{LINK_PREFIX}{}&name={}&version=1.0.0",
        utf8_percent_encode(path, COMPONENT),
        utf8_percent_encode(name, COMPONENT)
    )
}

/// The `name` of a link: letters, digits, space, `.`, `_` and `-` of the job's label, at
/// most 64 characters; anything else becomes `_`. A label with no letter or digit is
/// named "Hotend print".
pub fn link_name(label: &str) -> String {
    let cleaned: String = label
        .chars()
        .take(LINK_NAME_CHARS)
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches([' ', '.', '_', '-']);
    if cleaned.chars().any(|c| c.is_ascii_alphanumeric()) {
        cleaned.to_owned()
    } else {
        "Hotend print".to_owned()
    }
}

/// The Bambu Connect import link of one placed, confirmed file. Only [`HandOff::link`]
/// makes one: it cannot be built from text, deserialised or changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BambuConnectLink {
    text: String,
    path: String,
}

impl BambuConnectLink {
    /// The link, starting with [`LINK_PREFIX`].
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The path the link names, as Bambu Connect reads it.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// A link for `path` shown as `name`, built by the same encoding, for this crate's
    /// tests of the openers' commands. Not in any other build.
    #[cfg(test)]
    pub(crate) fn for_tests(path: &str, name: &str) -> Self {
        Self {
            text: bambu_connect_link(path, &link_name(name)),
            path: path.to_owned(),
        }
    }

    /// A link holding `text` as it is, unencoded, for this crate's tests of the openers'
    /// refusals. Not in any other build.
    #[cfg(test)]
    pub(crate) fn raw_for_tests(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            path: String::new(),
        }
    }
}

impl std::fmt::Display for BambuConnectLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// A job placed for Bambu Connect.
#[derive(Debug)]
pub struct HandOff {
    placed: PlacedPath,
    folder: HandOffFolder,
    name: String,
    sha256: String,
}

impl HandOff {
    /// Where the file was placed, as Hotend's process sees it.
    pub fn path(&self) -> &Path {
        &self.placed.local
    }

    /// The Windows form of that path, under WSL and on Windows.
    pub fn windows_path(&self) -> Option<&str> {
        self.placed.windows.as_deref()
    }

    /// The `bambu-connect://import-file` link for the placed file. It is released only
    /// against a confirmation a human gave for this job
    /// ([`crate::token::ConfirmationRequest::hand_off`]), while the placed file still holds
    /// the confirmed bytes and still resolves to the same file directly inside the
    /// hand-off folder; the caller then gives it to its opener.
    pub fn link(
        &self,
        job: &CheckedJob,
        confirmation: HumanConfirmation,
    ) -> Result<BambuConnectLink, DeliveryError> {
        confirmation.redeem_for_hand_off(job)?;
        if job.sha256() != self.sha256 {
            return Err(TokenError::WrongFile.into());
        }
        // The placed file is read again: the link names a path, and the path must hold
        // the confirmed bytes at the moment the link is released.
        if !job.is_copy(&self.placed.local) {
            return Err(JobError::Changed.into());
        }
        // And it is still the file inside the chosen folder that the link was made for.
        let now = self.folder.resolve(&self.placed.local)?;
        if now != self.placed {
            return Err(HandOffPathError::OutsideFolder(
                self.placed
                    .local
                    .to_string_lossy()
                    .chars()
                    .take(200)
                    .collect(),
            )
            .into());
        }
        Ok(BambuConnectLink {
            text: bambu_connect_link(&now.link_path, &link_name(&self.name)),
            path: now.link_path,
        })
    }
}

/// Hand-off to Bambu Connect: the file is placed in the chosen folder and a link is built.
#[derive(Debug, Clone)]
pub struct BambuConnectHandOff {
    folder: Option<HandOffFolder>,
}

impl BambuConnectHandOff {
    /// Hand-off through `folder`, a folder Bambu Connect can read.
    pub fn new(folder: HandOffFolder) -> Self {
        Self {
            folder: Some(folder),
        }
    }

    /// The path without a folder: for stating its capabilities only. Delivering through
    /// it fails.
    pub fn description() -> Self {
        Self { folder: None }
    }
}

impl Delivery for BambuConnectHandOff {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            kind: DeliveryKind::BambuConnect,
            name: "Send with Bambu Connect",
            sends_file: false,
            starts_print: false,
            controls_print: false,
            live_status: false,
            reports_result: false,
            can: vec![
                "Saves the checked file in your hand-off folder and asks Bambu Connect to import it",
                "You choose the printer and press Print in Bambu Connect",
            ],
            cannot: vec![
                "Start, pause or stop the print from Hotend",
                "Tell Hotend whether the print succeeded",
                "Show the AMS contents (unless the printer also reports over the LAN)",
            ],
            cost: "Nothing is given up: the printer stays in Bambu Cloud. Hotend gets no result back.",
            needs: vec![
                "Bambu Connect installed on this computer",
                "A hand-off folder Bambu Connect can read (under WSL: a folder on a Windows drive, chosen in Settings)",
            ],
        }
    }

    fn deliver(&mut self, job: &CheckedJob) -> Result<Delivered, DeliveryError> {
        // A file Bambu Connect cannot import is not placed: its import dialog would
        // accept the link and then do nothing (seen on 2026-11-21).
        let missing = bambu_connect_missing(job);
        if !missing.is_empty() {
            return Err(DeliveryError::NotImportable(missing));
        }
        let folder = self.folder.clone().ok_or_else(|| {
            DeliveryError::Folder(String::new(), "no hand-off folder was chosen".to_owned())
        })?;
        // The folder is still the directory that was chosen (and shown): a folder that has
        // since become a link to somewhere else is not written into.
        if folder.path().canonicalize().ok().as_deref() != Some(folder.path()) {
            return Err(DeliveryError::Folder(
                folder.path().to_string_lossy().chars().take(200).collect(),
                "it is no longer the folder that was chosen".to_owned(),
            ));
        }
        let path = place(job, folder.path())?;
        let placed = match folder.resolve(&path) {
            Ok(placed) => placed,
            Err(error) => {
                // A copy whose path cannot be named is not left behind.
                remove_in(folder.path(), &path);
                return Err(error.into());
            }
        };
        Ok(Delivered::HandOff(HandOff {
            placed,
            folder,
            name: job.name().to_owned(),
            sha256: job.sha256().to_owned(),
        }))
    }
}

/// The archive entries Bambu Connect needs to import a sliced file for plate `plate`.
///
/// Measured with Bambu Connect: a sliced file with all of these was imported (2026-11-18,
/// made by Bambu Studio on Windows); one that had everything but the plate previews
/// (made headless by the Linux Bambu Studio, which cannot draw them without a display)
/// was accepted by the import dialog and then silently not imported (2026-11-21). Only
/// `plate_<n>.png` is required of the previews: whether Bambu Connect also needs
/// `plate_<n>_small.png`, `plate_no_light_<n>.png`, `top_<n>.png` or `pick_<n>.png` is
/// not known.
pub fn bambu_connect_entries(plate: u32) -> [String; 4] {
    [
        format!("Metadata/plate_{plate}.gcode"),
        format!("Metadata/plate_{plate}.gcode.md5"),
        "Metadata/slice_info.config".to_owned(),
        format!("Metadata/plate_{plate}.png"),
    ]
}

/// The entries of [`bambu_connect_entries`] for the job's plate that its checked bytes
/// lack, in that order. Empty when Bambu Connect has what it needs. A file whose archive
/// cannot be read again lacks all of them.
pub fn bambu_connect_missing(job: &CheckedJob) -> Vec<String> {
    bambu_connect_missing_in(job.checked_bytes(), job.declared().plate)
}

/// [`bambu_connect_missing`] for the archive in `reader`, for plate `plate`.
pub fn bambu_connect_missing_in(reader: impl Read + std::io::Seek, plate: u32) -> Vec<String> {
    let needed = bambu_connect_entries(plate);
    match crate::archive::Archive::open(reader, &crate::archive::ArchiveLimits::default()) {
        Ok(archive) => needed
            .into_iter()
            .filter(|name| !archive.has(name))
            .collect(),
        Err(_) => needed.to_vec(),
    }
}

/// What the person is told when a file lacks entries Bambu Connect needs.
fn not_importable_message(missing: &[String]) -> String {
    let previews = missing.iter().any(|name| name.ends_with(".png"));
    let mut message = format!(
        "Bambu Connect cannot import this sliced file: it lacks {}",
        missing
            .iter()
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    if previews {
        message.push_str(
            ". That is the plate preview Bambu Studio draws when it slices; the Linux Bambu \
             Studio cannot draw it without a display, so a slice made in Hotend's sandbox \
             has none, and Bambu Connect accepts such a file and then imports nothing",
        );
    }
    message.push_str(
        ". Nothing was saved or sent. Make the editable Bambu Studio project instead and \
         slice and print it in Bambu Studio, or save the file to a folder (or a USB drive \
         for the printer)",
    );
    message
}

/// Export to a folder or a USB drive.
#[derive(Debug, Clone)]
pub struct ExportToFolder {
    folder: PathBuf,
}

impl ExportToFolder {
    /// Export into `folder`, an existing folder (for example a mounted USB drive).
    pub fn new(folder: impl Into<PathBuf>) -> Self {
        Self {
            folder: folder.into(),
        }
    }
}

impl Delivery for ExportToFolder {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            kind: DeliveryKind::Export,
            name: "Save to a folder or USB drive",
            sends_file: false,
            starts_print: false,
            controls_print: false,
            live_status: false,
            reports_result: false,
            can: vec!["Writes the sliced job (.gcode.3mf) to a folder or USB drive"],
            cannot: vec![
                "Send anything to the printer",
                "Start, watch or stop the print",
            ],
            cost: "Manual: you carry the drive to the printer and start the print on its screen. It depends on nothing else.",
            needs: vec!["A folder or a USB drive to write to"],
        }
    }

    fn deliver(&mut self, job: &CheckedJob) -> Result<Delivered, DeliveryError> {
        Ok(Delivered::Exported {
            path: place(job, &self.folder)?,
        })
    }
}

/// Removes `path`, a file Hotend wrote directly inside `folder` (a canonical path), only
/// while `folder` is still that very directory: a folder that became a symbolic link since
/// it was checked is never removed through. (A link named `path` itself is removed as a
/// link; its target is untouched.)
fn remove_in(folder: &Path, path: &Path) {
    if folder.canonicalize().ok().as_deref() == Some(folder) && path.parent() == Some(folder) {
        let _ = std::fs::remove_file(path);
    }
}

/// Copies the checked file into `folder` under the job's remote name and returns the
/// absolute path of the copy. The copy replaces a file of the same name.
fn place(job: &CheckedJob, folder: &Path) -> Result<PathBuf, DeliveryError> {
    let shown = || {
        folder
            .to_string_lossy()
            .chars()
            .take(200)
            .collect::<String>()
    };
    let folder_error = |why: &str| DeliveryError::Folder(shown(), why.to_owned());
    let folder = folder
        .canonicalize()
        .map_err(|_| folder_error("it does not exist"))?;
    if !folder.is_dir() {
        return Err(folder_error("it is not a folder"));
    }
    let name = job.remote_name();
    let target = folder.join(&name);
    let partial = folder.join(format!(".{name}.part"));
    let write_error = |e: std::io::Error| DeliveryError::Write(name.clone(), e);

    let mut source = job.open_verified()?;
    // A leftover from an interrupted copy is removed; `create_new` then refuses anything
    // that appeared in its place, a symbolic link included.
    remove_in(&folder, &partial);
    let mut out = File::options()
        .write(true)
        .create_new(true)
        .open(&partial)
        .map_err(write_error)?;
    let copied = (|| -> std::io::Result<String> {
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read = source.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            out.write_all(&buffer[..read])?;
        }
        out.sync_all()?;
        Ok(hex(&hasher.finalize()))
    })();
    drop(out);
    match copied {
        Ok(hash) if hash == job.sha256() => {
            std::fs::rename(&partial, &target).map_err(|e| {
                remove_in(&folder, &partial);
                write_error(e)
            })?;
            Ok(target)
        }
        Ok(_) => {
            remove_in(&folder, &partial);
            Err(JobError::Changed.into())
        }
        Err(error) => {
            remove_in(&folder, &partial);
            Err(write_error(error))
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::archive::tests::archive;

    /// The entries of a sliced file for `plate` as the headless Linux slice wrote them on
    /// 2026-11-21 (structure only), plus `extra`.
    fn sliced(plate: u32, extra: &[&str]) -> Vec<u8> {
        let mut names = vec![
            "[Content_Types].xml".to_owned(),
            "3D/3dmodel.model".to_owned(),
            format!("Metadata/plate_{plate}.json"),
            format!("Metadata/plate_{plate}.gcode"),
            format!("Metadata/plate_{plate}.gcode.md5"),
            "Metadata/slice_info.config".to_owned(),
            "Metadata/project_settings.config".to_owned(),
        ];
        names.extend(extra.iter().map(|name| (*name).to_owned()));
        let entries: Vec<(&str, &[u8])> = names
            .iter()
            .map(|name| (name.as_str(), b"x".as_slice()))
            .collect();
        archive(&entries)
    }

    fn missing(bytes: Vec<u8>, plate: u32) -> Vec<String> {
        bambu_connect_missing_in(std::io::Cursor::new(bytes), plate)
    }

    #[test]
    fn a_slice_without_the_plate_preview_is_not_importable() {
        assert_eq!(missing(sliced(1, &[]), 1), ["Metadata/plate_1.png"]);
        // The preview of another plate does not count.
        assert_eq!(
            missing(sliced(2, &["Metadata/plate_1.png"]), 2),
            ["Metadata/plate_2.png"]
        );
        // The small preview alone is not the preview.
        assert_eq!(
            missing(sliced(1, &["Metadata/plate_1_small.png"]), 1),
            ["Metadata/plate_1.png"]
        );
    }

    #[test]
    fn a_slice_with_the_entries_bambu_connect_needs_is_importable() {
        let full = [
            "Metadata/plate_1.png",
            "Metadata/plate_1_small.png",
            "Metadata/plate_no_light_1.png",
            "Metadata/top_1.png",
            "Metadata/pick_1.png",
        ];
        assert!(missing(sliced(1, &full), 1).is_empty());
        assert!(missing(sliced(1, &full[..1]), 1).is_empty());
        assert!(missing(sliced(3, &["Metadata/plate_3.png"]), 3).is_empty());
    }

    #[test]
    fn every_missing_entry_is_named_and_an_unreadable_archive_lacks_all() {
        let bare = archive(&[("Metadata/plate_1.png", b"x")]);
        assert_eq!(
            missing(bare, 1),
            [
                "Metadata/plate_1.gcode",
                "Metadata/plate_1.gcode.md5",
                "Metadata/slice_info.config"
            ]
        );
        assert_eq!(
            missing(b"not a zip".to_vec(), 1),
            bambu_connect_entries(1).to_vec()
        );
    }

    #[test]
    fn the_refusal_says_why_and_what_to_do_instead() {
        let message =
            DeliveryError::NotImportable(vec!["Metadata/plate_1.png".to_owned()]).to_string();
        for part in [
            "Bambu Connect cannot import this sliced file",
            "`Metadata/plate_1.png`",
            "plate preview",
            "without a display",
            "Nothing was saved or sent",
            "editable Bambu Studio project",
            "save the file to a folder",
        ] {
            assert!(message.contains(part), "{part}: {message}");
        }
        let message =
            DeliveryError::NotImportable(vec!["Metadata/slice_info.config".to_owned()]).to_string();
        assert!(!message.contains("plate preview"), "{message}");
        assert!(
            message.contains("editable Bambu Studio project"),
            "{message}"
        );
    }

    /// A file is removed only while its folder is still the directory that was checked: a
    /// folder swapped for a link to another folder is never removed through.
    #[cfg(unix)]
    #[test]
    fn nothing_is_removed_through_a_folder_that_became_a_link() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let folder = root.join("inbox");
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir(&folder).unwrap();
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::write(folder.join("a.part"), b"x").unwrap();
        std::fs::write(elsewhere.join("a.part"), b"keep").unwrap();
        remove_in(&folder, &folder.join("a.part"));
        assert!(!folder.join("a.part").exists());
        // Only a file directly inside the folder.
        remove_in(&folder, &elsewhere.join("a.part"));
        assert!(elsewhere.join("a.part").exists());
        // The folder becomes a link to another one: nothing is removed there.
        std::fs::remove_dir(&folder).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &folder).unwrap();
        remove_in(&folder, &folder.join("a.part"));
        assert_eq!(std::fs::read(elsewhere.join("a.part")).unwrap(), b"keep");
    }
}
