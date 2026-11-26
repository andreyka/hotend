//! A sliced file, checked: the only thing that can be delivered to a printer.
//!
//! [`check_job`] is the single gate. It reads the `.gcode.3mf` as hostile input, lints its
//! G-code and builds the summary a human confirms. Guarantees:
//! - **one read.** The file must be a regular file that is not itself a symbolic link (a
//!   folder on the way to it may be one), with one hard link, non-empty and under the
//!   size limit; anything else, a named pipe included, is refused before it is opened
//!   for reading. It is then read once, from start to end, into memory that only this
//!   process can change, and hashed while it is read. Everything after that (the
//!   archive checks, the G-code check, the summary, every delivery) uses those bytes
//!   and never the file again, so the hash, the check and what is sent are of the same
//!   bytes whatever happens to the file meanwhile. The file on disk is looked at again
//!   only to refuse a job whose file was changed since ([`JobError::Changed`]);
//! - the archive passes [`crate::archive`] (limits, one reading, nothing extracted) and
//!   holds only entries a sliced file for this printer has: the G-code of the declared
//!   plate and no other G-code, its MD5, which must be the G-code's, and the slicer's
//!   metadata. A second plate is refused: nothing travels to a printer unread;
//! - a [`CheckedJob`] exists only if the linter found no error ([`crate::lint`]) and the
//!   file agrees with the declared job: the plate, the build plate, the printer, every
//!   filament with its material, nozzle and nozzle size, and the filament map. What the
//!   declaration names and the file does not state is an error, not an agreement. It
//!   cannot be built any other way: its fields are private and it is not `Deserialize`;
//! - the [`JobSummary`] says where each value comes from. Measured from the commands of
//!   the G-code: temperatures, the printed box, the number of layers, the length of
//!   filament pushed, a lower bound of the print time, and the accepted configuration
//!   commands. Claims of the file that the check holds to those measurements: the print
//!   time, the filament length and the filament weight (a file whose claim does not
//!   fit is refused; the weight only through a range of densities). A claim of the file
//!   that is only shown, marked as such: the colour. Taken from a
//!   closed list of the printer profile after the file's statement was matched against
//!   it: the printer, the build plate, the material, the nozzle size. The plate number
//!   and the build plate are the declared ones, which the file was held to. No free
//!   text of the file or of the application reaches the summary;
//! - [`JobSummary::review_text`] is the text a person confirms. It is built from the
//!   summary alone, and a confirmation is bound to its hash ([`crate::token`]).
//!
//! The layout of the sliced file (`Metadata/plate_<n>.gcode` and its `.md5`,
//! `Metadata/slice_info.config` with `prediction`, `filament_maps` and one `<filament>`
//! per used filament, `Metadata/project_settings.config`, `Metadata/plate_<n>.json`) was
//! read on 2026-11-17 from a file written by Bambu Studio 02.09.01.70 for the X2D.

use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::archive::{Archive, ArchiveError, ArchiveLimits};
use crate::lint::{
    ConfigurationUse, DeclaredJob, Extent, Finding, LintReport, MAX_FINDINGS, Severity,
    configuration_sentence, lint_gcode, quoted,
};
use crate::md5::Md5;
use crate::profile::{PrinterProfile, Section};
use crate::status::Rgba;

const SLICE_INFO: &str = "Metadata/slice_info.config";
const PROJECT_SETTINGS: &str = "Metadata/project_settings.config";
/// XML nodes accepted in `slice_info.config`.
const MAX_XML_NODES: u32 = 200_000;

/// Why a sliced file cannot be delivered.
#[derive(Debug, thiserror::Error)]
pub enum JobError {
    /// The file or the declaration cannot be used at all.
    #[error("the sliced file cannot be used: {0}")]
    Unusable(String),
    /// The archive is unsafe.
    #[error(transparent)]
    Archive(#[from] ArchiveError),
    /// The linter found errors. The report says which; slice again or fix the profile.
    #[error("the sliced file failed the safety check with {} error(s); first: {}", .0.errors, first_error(.0))]
    Rejected(Box<LintReport>),
    /// The file changed after it was checked.
    #[error("the sliced file changed after it was checked; check it again")]
    Changed,
    /// Reading failed.
    #[error("cannot read the sliced file: {0}")]
    Io(#[from] std::io::Error),
}

fn first_error(report: &LintReport) -> String {
    report
        .findings
        .iter()
        .find(|finding| finding.severity == Severity::Error)
        .map(|finding| format!("line {}: {}", finding.line, finding.message))
        .unwrap_or_default()
}

/// One filament of the job, as checked.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SummaryFilament {
    /// The slicer's 1-based filament number.
    pub slot: u32,
    /// The material family, by the printer profile's name for it. The file's own word
    /// was matched against the profile and against the declared job.
    pub material: Option<String>,
    /// Colour the slicer recorded. A claim of the file, shown as one.
    pub colour: Option<Rgba>,
    /// Grams the file says it uses. A claim of the file; held to a plausible density
    /// for the measured length.
    pub grams: Option<f64>,
    /// Metres the file says it uses. A claim of the file; refused unless it fits
    /// [`Self::measured_metres`].
    pub metres: Option<f64>,
    /// Metres of this filament the commands push, less what they pull back.
    pub measured_metres: f64,
    /// Device extruder id of the nozzle the file's own filament map assigns; it is the
    /// declared one.
    pub nozzle: Option<u8>,
    /// The printer profile's word for that nozzle ("main", "auxiliary").
    pub nozzle_role: Option<String>,
    /// The nozzle size the job declares and the file states, in millimetres.
    pub nozzle_diameter_mm: Option<f64>,
    /// Whether the G-code selects this filament.
    pub selected_in_gcode: bool,
}

/// What a sliced file will make the printer do. This is what the confirmation dialog
/// shows; the module documentation says where each value comes from.
///
/// ```compile_fail
/// // A summary cannot be written by hand, so nothing but the file can fill the dialog.
/// let _ = hotend_printer::JobSummary {
///     file_name: String::new(), sha256: String::new(), size_bytes: 0, plate: 1,
///     estimated_seconds: None, min_seconds: 0, layers: None, filaments: Vec::new(),
///     max_nozzle_c: None, max_bed_c: None, max_chamber_c: None, extent: None,
///     printer_name: String::new(), stated_printer: None, plate_type: None,
///     max_nozzle_reference_c: None, configuration: Vec::new(), unplaced_moves: 0,
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct JobSummary {
    /// Name of the file on disk: letters, digits, space, `.`, `_` and `-` only.
    pub file_name: String,
    /// SHA-256 of the file, lower-case hex.
    pub sha256: String,
    /// Size of the file.
    pub size_bytes: u64,
    /// The plate that will be printed: the declared one, the only one in the file.
    pub plate: u32,
    /// Print time the file states, in seconds. A claim of the file; it is at least
    /// [`Self::min_seconds`] and not implausibly more, or the file is refused.
    pub estimated_seconds: Option<u64>,
    /// Seconds the moves take at the speeds they name: the print takes at least this
    /// long. Measured from the commands.
    pub min_seconds: u64,
    /// Number of layers, counted from the commands (heights at which plastic is laid).
    pub layers: Option<u32>,
    /// The filaments.
    pub filaments: Vec<SummaryFilament>,
    /// Highest nozzle temperature any command in the G-code sets, °C.
    pub max_nozzle_c: Option<f64>,
    /// Highest bed temperature any command sets, °C.
    pub max_bed_c: Option<f64>,
    /// Highest chamber temperature any command sets, °C.
    pub max_chamber_c: Option<f64>,
    /// The box around everything the G-code prints.
    pub extent: Option<Extent>,
    /// Name of the printer model the file was checked for (from the profile).
    pub printer_name: String,
    /// The printer the file says it was sliced for; one of the profile's names for it.
    pub stated_printer: Option<String>,
    /// The build plate: the one declared for the job, which the file was sliced for and
    /// whose bed temperature ceiling the commands were held to.
    pub plate_type: Option<String>,
    /// Highest value of a vendor parameter that may be a nozzle temperature, °C. It is
    /// held to the same limits as a set temperature.
    pub max_nozzle_reference_c: Option<f64>,
    /// The accepted commands that change limits or stored configuration: stock lines of
    /// the printer's start and end routines, in the only form the check accepts.
    pub configuration: Vec<ConfigurationUse>,
    /// Moves that start from a position only the printer knows (short, start routine only).
    pub unplaced_moves: u32,
}

/// A whole number of seconds as `2 h 5 min` or `11 min 50 s`.
fn duration_text(seconds: u64) -> String {
    let (hours, minutes, rest) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);
    match (hours, minutes) {
        (0, 0) => format!("{rest} s"),
        (0, _) => format!("{minutes} min {rest} s"),
        _ => format!("{hours} h {minutes} min"),
    }
}

/// A number without a trailing `.0`.
fn plain(value: f64) -> String {
    let rounded = (value * 100.0).round() / 100.0;
    format!("{rounded}")
}

impl JobSummary {
    /// What a person is asked to confirm, line by line: time, layers, filament, nozzle,
    /// temperatures, build plate, the printed area and every accepted command that changes
    /// limits or stored configuration. Built from this summary only. Each line says
    /// whether its value was measured from the commands or is what the file says.
    pub fn review_lines(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "Print file \"{}\", plate {}, checked for the {}.",
            self.file_name, self.plate, self.printer_name
        )];
        if let Some(stated) = &self.stated_printer {
            lines.push(format!("The file says it was sliced for: {stated}."));
        }
        let floor = duration_text(self.min_seconds);
        lines.push(match self.estimated_seconds {
            Some(stated) => format!(
                "Time: the file says {}. Its moves alone take at least {floor} at the speeds they name; heating, the printer's own routines and acceleration come on top.",
                duration_text(stated)
            ),
            None => format!(
                "Time: the file states none. Its moves alone take at least {floor} at the speeds they name."
            ),
        });
        lines.push(match self.layers {
            Some(layers) => format!("Layers: {layers}, counted from the commands."),
            None => "Layers: the commands lay no plastic.".to_owned(),
        });
        for filament in &self.filaments {
            let mut text = format!(
                "Filament {}: {}",
                filament.slot,
                filament.material.as_deref().unwrap_or("unknown material")
            );
            match (filament.nozzle, &filament.nozzle_role) {
                (Some(id), Some(role)) => {
                    text.push_str(&format!(" on the {role} nozzle (id {id})"))
                }
                (Some(id), None) => text.push_str(&format!(" on nozzle id {id}")),
                _ => {}
            }
            if let Some(size) = filament.nozzle_diameter_mm {
                text.push_str(&format!(", {} mm nozzle", plain(size)));
            }
            text.push_str(&format!(
                ". The commands push {} m of it.",
                plain(filament.measured_metres)
            ));
            let mut says = Vec::new();
            if let Some(metres) = filament.metres {
                says.push(format!("{} m", plain(metres)));
            }
            if let Some(grams) = filament.grams {
                says.push(format!("{} g", plain(grams)));
            }
            if let Some(colour) = &filament.colour {
                // Opaque colours are shown as people write them, without the alpha byte.
                let hex = colour.hex();
                let shown = hex.strip_suffix("FF").unwrap_or(&hex);
                says.push(format!("colour #{shown}"));
            }
            if !says.is_empty() {
                text.push_str(&format!(" The file says: {}.", says.join(", ")));
            }
            if !filament.selected_in_gcode {
                text.push_str(" The G-code does not select it.");
            }
            lines.push(text);
        }
        let mut nozzle = match self.max_nozzle_c {
            Some(max) => format!("Nozzle temperature: up to {} C.", plain(max)),
            None => "Nozzle temperature: the file sets none.".to_owned(),
        };
        if let Some(reference) = self.max_nozzle_reference_c
            && self.max_nozzle_c.is_none_or(|max| reference > max)
        {
            nozzle.push_str(&format!(
                " A setting of the printer's own routine names {} C and may heat the nozzle that far; it is within the filament's limit.",
                plain(reference)
            ));
        }
        lines.push(nozzle);
        let mut bed = match self.max_bed_c {
            Some(max) => format!("Bed temperature: up to {} C.", plain(max)),
            None => "Bed temperature: the file sets none.".to_owned(),
        };
        if let Some(plate) = &self.plate_type {
            bed.push_str(&format!(
                " Build plate: {plate}, as chosen for this job; the file was sliced for it."
            ));
        }
        lines.push(bed);
        lines.push(match self.max_chamber_c {
            Some(max) if max > 0.0 => format!("Chamber temperature: up to {} C.", plain(max)),
            _ => "Chamber heating: off.".to_owned(),
        });
        lines.push("When the file ends, every heater has been switched off.".to_owned());
        if let Some(extent) = &self.extent {
            lines.push(format!(
                "Printed area: X {} to {} mm, Y {} to {} mm, up to {} mm high.",
                plain(extent.min[0]),
                plain(extent.max[0]),
                plain(extent.min[1]),
                plain(extent.max[1]),
                plain(extent.max[2])
            ));
        } else {
            lines.push("Printed area: the file prints nothing.".to_owned());
        }
        if self.configuration.is_empty() {
            lines.push(
                "The file contains no command that changes the printer's limits or stored settings."
                    .to_owned(),
            );
        } else {
            lines.push(
                "The printer's own start and end routines in this file change settings of the printer. Each command below is part of the slicer's stock routine for this printer and is accepted only in exactly this form:"
                    .to_owned(),
            );
            let mut kinds: Vec<_> = self.configuration.iter().map(|used| used.kind).collect();
            kinds.sort();
            kinds.dedup();
            for kind in kinds {
                lines.push(format!("- {}", kind.explain()));
                for used in self.configuration.iter().filter(|used| used.kind == kind) {
                    let times = if used.count == 1 {
                        "once".to_owned()
                    } else {
                        format!("{} times", used.count)
                    };
                    lines.push(format!(
                        "    {}, {times} in {}: {}.",
                        used.command,
                        used.section.phrase(),
                        used.what
                    ));
                }
            }
        }
        if self.unplaced_moves > 0 {
            lines.push(format!(
                "{} short move(s) in {} start from a position only the printer knows; they go no further than the stock routine's, and none is written to end closer to the bed than where the printer left the nozzle.",
                self.unplaced_moves,
                Section::Start.phrase()
            ));
        }
        lines.push(
            "Pictures, names, times and amounts that the printer's screen or Bambu's own programs show for this job are read by them from the file; Hotend did not check those."
                .to_owned(),
        );
        lines.push(format!("SHA-256 of the file: {}.", self.sha256));
        lines
    }

    /// [`Self::review_lines`] as one text.
    pub fn review_text(&self) -> String {
        self.review_lines().join("\n")
    }

    /// The accepted configuration commands in one sentence part, or `None` when there
    /// are none.
    pub fn configuration_sentence(&self) -> Option<String> {
        (!self.configuration.is_empty()).then(|| configuration_sentence(&self.configuration))
    }
}

/// A sliced file that passed [`check_job`]. It owns the bytes that were checked.
pub struct CheckedJob {
    path: PathBuf,
    name: String,
    declared: DeclaredJob,
    printer_model: String,
    summary: JobSummary,
    lint: LintReport,
    bytes: Arc<[u8]>,
}

impl std::fmt::Debug for CheckedJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CheckedJob")
            .field("path", &self.path)
            .field("name", &self.name)
            .field("declared", &self.declared)
            .field("printer_model", &self.printer_model)
            .field("sha256", &self.summary.sha256)
            .field("size_bytes", &self.summary.size_bytes)
            .finish_non_exhaustive()
    }
}

/// How many hex digits of the file's SHA-256 a remote name carries: 128 bits, so two
/// different files cannot be made to share a name.
const NAME_HASH_DIGITS: usize = 32;

impl CheckedJob {
    /// Where the file was read from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The application's label for the job, cleaned. Not read from the file.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The file name the job gets on a printer or a drive: the label, the first 32 hex
    /// digits of the file's SHA-256, and `.gcode.3mf`. The hash is part of the name so
    /// that two different files never share one: a start or a link that names a file
    /// names its content. A start command names the file to print by this name only, and
    /// a printer's storage is shared: a later upload of another file under the same label
    /// cannot take this one's place.
    pub fn remote_name(&self) -> String {
        format!(
            "{}-{}.gcode.3mf",
            self.name.replace(' ', "_"),
            &self.summary.sha256[..NAME_HASH_DIGITS]
        )
    }

    /// SHA-256 of the file as it was checked.
    pub fn sha256(&self) -> &str {
        &self.summary.sha256
    }

    /// The declaration the file was held to.
    pub fn declared(&self) -> &DeclaredJob {
        &self.declared
    }

    /// Id of the printer model the job was checked for.
    pub fn printer_model(&self) -> &str {
        &self.printer_model
    }

    /// What the file will make the printer do.
    pub fn summary(&self) -> &JobSummary {
        &self.summary
    }

    /// The linter's report: warnings and statistics (a checked job has no errors).
    pub fn lint(&self) -> &LintReport {
        &self.lint
    }

    /// The bytes that were checked, to send. They are this job's own copy; the file on
    /// disk is not read for them. A job whose file was changed or replaced since the
    /// check is refused all the same ([`JobError::Changed`]): somebody is writing where
    /// Hotend's sliced files are kept, and the person should check again.
    pub fn open_verified(&self) -> Result<Cursor<Arc<[u8]>>, JobError> {
        self.verify_on_disk()?;
        Ok(Cursor::new(Arc::clone(&self.bytes)))
    }

    /// The checked bytes, without looking at the file on disk: for reading what the
    /// checked archive holds (for example [`crate::delivery::bambu_connect_missing`]).
    pub(crate) fn checked_bytes(&self) -> Cursor<Arc<[u8]>> {
        Cursor::new(Arc::clone(&self.bytes))
    }

    /// Checks that the file on disk is still the file that was checked.
    pub(crate) fn verify_on_disk(&self) -> Result<(), JobError> {
        if self.is_copy(&self.path) {
            Ok(())
        } else {
            Err(JobError::Changed)
        }
    }

    /// Whether the file at `path` is, right now, a regular file with the bytes that were
    /// checked.
    pub(crate) fn is_copy(&self, path: &Path) -> bool {
        read_once(path, self.summary.size_bytes)
            .is_ok_and(|(bytes, hash)| hash == self.summary.sha256 && *bytes == *self.bytes)
    }

    /// A stable text of the declaration, for binding a confirmation to it.
    pub(crate) fn declaration_text(&self) -> String {
        let mut text = format!(
            "plate={};plate_type={:?};model={:?}",
            self.declared.plate, self.declared.plate_type, self.printer_model
        );
        for filament in &self.declared.filaments {
            text.push_str(&format!(
                ";filament={},{:?},{},{}",
                filament.slot, filament.material, filament.nozzle, filament.nozzle_diameter_mm
            ));
        }
        text
    }
}

/// Lower-case hex.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// SHA-256 of `bytes`, lower-case hex.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Reads the regular file at `path` once, from start to end, and hashes what was read.
///
/// The file is refused unless it is a regular file that is not a symbolic link at its
/// last component, has one hard link, is not empty and is at most `max_bytes` long. The
/// kind of file is looked at before it is opened (opening a named pipe for reading
/// would wait for a writer for ever) and again on the open handle.
fn read_once(path: &Path, max_bytes: u64) -> Result<(Vec<u8>, String), JobError> {
    let unusable = |why: &str| JobError::Unusable(why.to_owned());
    let before = std::fs::symlink_metadata(path)
        .map_err(|_| unusable("the file does not exist or cannot be inspected"))?;
    if before.file_type().is_symlink() {
        return Err(unusable("the file is a symbolic link"));
    }
    if !before.file_type().is_file() {
        return Err(unusable("it is not a regular file"));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Not through a link, and never waiting on a pipe or a device that took the
        // file's place since the look above.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(unusable("it is not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() > 1 {
            return Err(unusable("the file has several hard links"));
        }
    }
    if meta.len() == 0 {
        return Err(unusable("the file is empty"));
    }
    if meta.len() > max_bytes {
        return Err(JobError::Unusable(format!(
            "the file is {} bytes; the limit is {max_bytes}",
            meta.len()
        )));
    }
    let mut bytes = Vec::with_capacity(meta.len().min(64 * 1024 * 1024) as usize);
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(JobError::Unusable(format!(
            "the file grew past the limit of {max_bytes} bytes while it was read"
        )));
    }
    let hash = sha256_hex(&bytes);
    Ok((bytes, hash))
}

/// The application's label as a safe name: letters, digits, space, `.`, `_`, `-`; other
/// characters become `_`. `None` when nothing usable is left.
fn clean_name(name: &str) -> Option<String> {
    let cleaned: String = name
        .trim()
        .chars()
        .take(80)
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches(['.', ' ', '-']).to_owned();
    cleaned
        .chars()
        .any(|c| c.is_ascii_alphanumeric())
        .then_some(cleaned)
}

/// The name of the file on disk for the review text: letters, digits, space, `.`, `_`
/// and `-`, at most 60 characters; anything else becomes `_`. A name cannot write a
/// sentence into what a person confirms, and cannot reorder it with invisible characters.
fn shown_file_name(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_string_lossy();
    let shown: String = name
        .chars()
        .take(60)
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    (!shown.is_empty()).then_some(shown)
}

/// One `<filament>` of the file's own account of the plate.
#[derive(Debug)]
struct InfoFilament {
    slot: u32,
    material: Option<String>,
    colour: Option<Rgba>,
    grams: Option<f64>,
    metres: Option<f64>,
    nozzle_diameter_mm: Option<f64>,
}

#[derive(Debug, Default)]
struct SliceInfo {
    prediction_s: Option<u64>,
    filaments: Vec<InfoFilament>,
    /// Slicer extruder number per filament, in slot order.
    filament_maps: Option<Vec<u8>>,
    /// `nozzle_diameters`, one per slicer extruder.
    nozzle_diameters_mm: Option<Vec<f64>>,
    /// `printer_model_id`; the slicer leaves it empty.
    printer_model_id: Option<String>,
    problems: Vec<String>,
}

fn non_negative(text: &str) -> Option<f64> {
    let value: f64 = text.trim().parse().ok()?;
    (value.is_finite() && value >= 0.0).then_some(value)
}

fn nozzle_size(text: &str) -> Option<f64> {
    non_negative(text).filter(|size| *size > 0.0 && *size <= 2.0)
}

fn parse_slice_info(bytes: &[u8], plate: u32) -> SliceInfo {
    let mut info = SliceInfo::default();
    let Ok(text) = std::str::from_utf8(bytes) else {
        info.problems
            .push(format!("`{SLICE_INFO}` is not UTF-8 text"));
        return info;
    };
    let options = roxmltree::ParsingOptions {
        allow_dtd: false,
        nodes_limit: MAX_XML_NODES,
        ..roxmltree::ParsingOptions::default()
    };
    let Ok(document) = roxmltree::Document::parse_with_options(text, options) else {
        info.problems
            .push(format!("`{SLICE_INFO}` is not well-formed XML"));
        return info;
    };
    fn elements<'a, 'input>(
        node: roxmltree::Node<'a, 'input>,
        tag: &str,
    ) -> Vec<roxmltree::Node<'a, 'input>> {
        node.children()
            .filter(|child| child.is_element() && child.tag_name().name() == tag)
            .collect()
    }
    fn metadata(node: roxmltree::Node<'_, '_>, key: &str) -> Option<String> {
        elements(node, "metadata")
            .into_iter()
            .find(|item| item.attribute("key") == Some(key))
            .and_then(|item| item.attribute("value").map(str::to_owned))
    }
    // The file holds one plate, the declared one. A second `<plate>` would be a second
    // account that this check and another reader could choose between.
    let plates = elements(document.root_element(), "plate");
    let [node] = plates.as_slice() else {
        info.problems.push(format!(
            "`{SLICE_INFO}` describes {} plates; a file for one job describes exactly one",
            plates.len()
        ));
        return info;
    };
    let node = *node;
    if metadata(node, "index").as_deref().map(str::trim) != Some(plate.to_string().as_str()) {
        info.problems.push(format!(
            "`{SLICE_INFO}` does not describe plate {plate}, the plate of this job"
        ));
        return info;
    }
    info.prediction_s = metadata(node, "prediction")
        .as_deref()
        .and_then(non_negative)
        .filter(|seconds| *seconds < 1.0e9)
        .map(|seconds| seconds.round() as u64);
    info.printer_model_id = metadata(node, "printer_model_id");
    info.nozzle_diameters_mm = metadata(node, "nozzle_diameters").map(|value| {
        value
            .split(',')
            .take(8)
            .map(|size| nozzle_size(size).unwrap_or(f64::NAN))
            .collect()
    });
    for filament in elements(node, "filament") {
        let Some(slot) = filament
            .attribute("id")
            .and_then(|id| id.trim().parse::<u32>().ok())
            .filter(|id| (1..=crate::lint::MAX_FILAMENTS as u32).contains(id))
        else {
            info.problems
                .push("a filament entry has no usable id".to_owned());
            continue;
        };
        if info.filaments.iter().any(|known| known.slot == slot) {
            info.problems
                .push(format!("filament {slot} is listed twice"));
            continue;
        }
        info.filaments.push(InfoFilament {
            slot,
            material: filament.attribute("type").map(str::to_owned),
            colour: filament.attribute("color").and_then(Rgba::parse),
            grams: filament.attribute("used_g").and_then(non_negative),
            metres: filament.attribute("used_m").and_then(non_negative),
            // A size that cannot be read is a size that equals nothing.
            nozzle_diameter_mm: filament
                .attribute("nozzle_diameter")
                .map(|size| nozzle_size(size).unwrap_or(f64::NAN)),
        });
    }
    info.filaments.sort_by_key(|filament| filament.slot);
    info.filament_maps = metadata(node, "filament_maps").and_then(|value| {
        let numbers: Option<Vec<u8>> = value
            .split([' ', ',', ';'])
            .filter(|token| !token.is_empty())
            .map(|token| token.parse::<u8>().ok())
            .collect();
        numbers.filter(|numbers| !numbers.is_empty() && numbers.len() <= crate::lint::MAX_FILAMENTS)
    });
    info
}

/// Adds a finding about the file as a whole.
fn add(report: &mut LintReport, severity: Severity, code: &'static str, message: String) {
    match severity {
        Severity::Error => report.errors += 1,
        Severity::Warning => report.warnings += 1,
    }
    if report.findings.len() < MAX_FINDINGS {
        report.findings.push(Finding {
            severity,
            code,
            line: 0,
            message,
            excerpt: String::new(),
        });
    }
}

fn fail(report: &mut LintReport, code: &'static str, message: String) {
    add(report, Severity::Error, code, message);
}

/// Holds what the file's metadata says of the print's time against the remaining time its
/// own progress command tells the printer. The summary a person confirms shows the
/// metadata; a file whose G-code tells the printer a much longer time is pointed out.
/// Only an understatement counts, and only one beyond what the slicer's own two figures
/// differ by. (The stated time is also held, as an error, to what the moves take; the
/// stated filament is held to what the commands push in [`cross_check`].)
fn check_claims(info: &SliceInfo, report: &mut LintReport) {
    let stats = &report.stats;
    if let (Some(stated), Some(minutes)) = (info.prediction_s, stats.first_remaining_minutes) {
        let in_gcode = u64::from(minutes) * 60;
        if in_gcode > stated.saturating_mul(2).saturating_add(600) {
            add(
                report,
                Severity::Warning,
                "time_understated",
                format!(
                    "the file's summary says the print takes {} min, but its G-code tells the printer {minutes} min; the longer time is the likely one",
                    stated.div_ceil(60)
                ),
            );
        }
    }
}

/// Holds the file's own account of the plate against the declaration and against what
/// the commands were measured to do, and returns the filaments for the summary. What
/// the account leaves out is an error: the real slice states all of it.
fn cross_check(
    info: SliceInfo,
    declared: &DeclaredJob,
    profile: &PrinterProfile,
    report: &mut LintReport,
) -> Vec<SummaryFilament> {
    let limits = &profile.family().gcode.limits;
    for problem in &info.problems {
        fail(report, "slice_info", problem.clone());
    }
    if !info.problems.is_empty() {
        return Vec::new();
    }
    // The printer's id: the slicer leaves it empty; anything else must be this printer's.
    if let Some(id) = info.printer_model_id.as_deref().map(str::trim)
        && !id.is_empty()
        && !profile
            .identity()
            .model_codes
            .iter()
            .any(|code| code.eq_ignore_ascii_case(id))
    {
        fail(
            report,
            "wrong_printer",
            format!(
                "the file's own account names the printer model id {}, which is not the {}",
                quoted(id),
                profile.name()
            ),
        );
    }
    // The time: stated, at least what the moves take, and not implausibly more.
    let floor = report.stats.min_seconds;
    match info.prediction_s {
        None => fail(
            report,
            "time_mismatch",
            "the file does not state how long the print takes".to_owned(),
        ),
        Some(stated) if (stated as f64) + 1.0 < floor => fail(
            report,
            "time_mismatch",
            format!(
                "the file says the print takes {}, but its moves alone take at least {} at the speeds they name",
                duration_text(stated),
                duration_text(floor as u64)
            ),
        ),
        Some(stated) if stated as f64 > floor * limits.time_max_factor + limits.time_overhead_s => {
            fail(
                report,
                "time_mismatch",
                format!(
                    "the file says the print takes {}, far more than its moves account for ({} at the speeds they name)",
                    duration_text(stated),
                    duration_text(floor as u64)
                ),
            );
        }
        Some(_) => {}
    }
    check_claims(&info, report);
    // The filament map, as device extruder ids: stated for every declared filament.
    let Some(maps) = &info.filament_maps else {
        fail(
            report,
            "filament_map_mismatch",
            "the file does not say which nozzle prints which filament".to_owned(),
        );
        return Vec::new();
    };
    if maps.len() < declared.filaments.len() {
        fail(
            report,
            "filament_map_mismatch",
            format!(
                "the file's filament map has {} entries; the job declares {} filaments",
                maps.len(),
                declared.filaments.len()
            ),
        );
    }
    for (index, slicer_extruder) in maps.iter().enumerate() {
        let Some(wanted) = declared.filaments.get(index) else {
            // A filament the declaration does not have is reported below.
            continue;
        };
        let nozzle = profile.nozzle_by_slicer_id(*slicer_extruder);
        if nozzle.map(|nozzle| nozzle.device_extruder) != Some(wanted.nozzle) {
            fail(
                report,
                "filament_map_mismatch",
                format!(
                    "the file was sliced with filament {} on slicer extruder {slicer_extruder}, which is not the nozzle the job assigns to it",
                    wanted.slot
                ),
            );
        }
        // The nozzle sizes the account lists per slicer extruder.
        if let Some(sizes) = &info.nozzle_diameters_mm {
            let stated = usize::from(*slicer_extruder)
                .checked_sub(1)
                .and_then(|at| sizes.get(at).copied());
            if stated.is_none_or(|size| !same_size(size, wanted.nozzle_diameter_mm)) {
                fail(
                    report,
                    "nozzle_mismatch",
                    format!(
                        "the file's own account does not list a nozzle of {} mm for filament {}",
                        wanted.nozzle_diameter_mm, wanted.slot
                    ),
                );
            }
        }
    }
    for filament in &info.filaments {
        if declared.filaments.len() < filament.slot as usize
            && (filament.grams.is_some_and(|grams| grams > 0.0)
                || filament.metres.is_some_and(|metres| metres > 0.0)
                || report.stats.slots_used.contains(&filament.slot))
        {
            fail(
                report,
                "undeclared_filament",
                format!(
                    "the file uses filament {}, which the job does not declare",
                    filament.slot
                ),
            );
        }
    }
    let unassigned = report.stats.unassigned_filament_mm.abs();
    let mut summary = Vec::new();
    for (index, wanted) in declared.filaments.iter().enumerate() {
        let slot = wanted.slot;
        let measured_mm = report.stats.filament_mm.get(index).copied().unwrap_or(0.0);
        let selected = report.stats.slots_used.contains(&slot);
        let listed = info.filaments.iter().find(|filament| filament.slot == slot);
        let nozzle = profile.nozzle_by_device_id(wanted.nozzle);
        let family = profile
            .filament_limits(&wanted.material)
            .map(|limits| limits.family.clone());
        let mut entry = SummaryFilament {
            slot,
            material: family.clone(),
            colour: None,
            grams: None,
            metres: None,
            measured_metres: measured_mm / 1000.0,
            nozzle: nozzle.map(|nozzle| nozzle.device_extruder),
            nozzle_role: nozzle.map(|nozzle| nozzle.role.clone()),
            nozzle_diameter_mm: Some(wanted.nozzle_diameter_mm),
            selected_in_gcode: selected,
        };
        let Some(listed) = listed else {
            // The slicer lists the filaments a plate uses. One it does not list must
            // not be used by the commands either.
            if selected || measured_mm.abs() > limits.length_slack_mm {
                fail(
                    report,
                    "slice_info",
                    format!(
                        "the commands use filament {slot}, but the file's own account does not list it"
                    ),
                );
            }
            summary.push(entry);
            continue;
        };
        // The material: stated, and of the declared family.
        let stated_family = listed
            .material
            .as_deref()
            .and_then(|material| profile.filament_limits(material))
            .map(|limits| limits.family.clone());
        match &listed.material {
            None => fail(
                report,
                "filament_mismatch",
                format!(
                    "the file does not say which material filament {slot} is; the job declares {}",
                    quoted(&wanted.material)
                ),
            ),
            Some(material) if stated_family.is_none() || stated_family != family => fail(
                report,
                "filament_mismatch",
                format!(
                    "the file was sliced for {} in slot {slot}; the job declares {}",
                    quoted(material),
                    quoted(&wanted.material)
                ),
            ),
            Some(_) => {}
        }
        if listed
            .nozzle_diameter_mm
            .is_some_and(|size| !same_size(size, wanted.nozzle_diameter_mm))
        {
            fail(
                report,
                "nozzle_mismatch",
                format!(
                    "the file's own account has filament {slot} on another nozzle size than the {} mm the job declares",
                    wanted.nozzle_diameter_mm
                ),
            );
        }
        // The amount: the stated length must be the length the commands push.
        match listed.metres {
            None => fail(
                report,
                "filament_amount_mismatch",
                format!("the file does not state how much of filament {slot} it uses"),
            ),
            Some(metres) => {
                let stated_mm = metres * 1000.0;
                let allowed = measured_mm.abs() * limits.length_tolerance
                    + limits.length_slack_mm
                    + unassigned;
                if (stated_mm - measured_mm).abs() > allowed {
                    fail(
                        report,
                        "filament_amount_mismatch",
                        format!(
                            "the file says it uses {} m of filament {slot}, but its commands push {} m",
                            plain(metres),
                            plain(measured_mm / 1000.0)
                        ),
                    );
                }
            }
        }
        // The weight: what the measured length weighs at a density plastic can have.
        if let Some(grams) = listed.grams {
            let radius = limits.filament_diameter_mm / 2.0;
            let per_mm = std::f64::consts::PI * radius * radius / 1000.0;
            let low = (measured_mm - limits.length_slack_mm - unassigned).max(0.0)
                * per_mm
                * limits.density_g_cm3[0]
                - 0.01;
            let high = (measured_mm.max(0.0) + limits.length_slack_mm + unassigned)
                * per_mm
                * limits.density_g_cm3[1]
                + 0.01;
            if grams < low || grams > high {
                fail(
                    report,
                    "filament_amount_mismatch",
                    format!(
                        "the file says filament {slot} weighs {} g; the {} m its commands push weigh between {} and {} g",
                        plain(grams),
                        plain(measured_mm / 1000.0),
                        plain(low.max(0.0)),
                        plain(high)
                    ),
                );
            }
        } else {
            fail(
                report,
                "filament_amount_mismatch",
                format!("the file does not state the weight of filament {slot}"),
            );
        }
        entry.colour = listed.colour;
        entry.grams = listed.grams;
        entry.metres = listed.metres;
        summary.push(entry);
    }
    summary
}

fn same_size(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-3
}

/// Holds the statements of the archive's other metadata to the profile and the
/// declaration: `Metadata/project_settings.config` (printer, printer settings, build
/// plate, nozzle sizes) and `Metadata/plate_<n>.json` (build plate, nozzle size). The
/// slicer writes the same facts there as into the G-code's comments, so a file in which
/// they differ was put together from parts. An entry that is absent says nothing.
fn check_other_statements(
    settings: Option<&[u8]>,
    plate_json: Option<&[u8]>,
    declared: &DeclaredJob,
    profile: &PrinterProfile,
    report: &mut LintReport,
) {
    let files = profile.slice_file();
    let text_of = |value: &serde_json::Value| value.as_str().map(str::to_owned);
    if let Some(bytes) = settings {
        let Ok(settings) = serde_json::from_slice::<serde_json::Value>(bytes) else {
            fail(
                report,
                "slice_info",
                format!("`{PROJECT_SETTINGS}` cannot be read"),
            );
            return;
        };
        if let Some(model) = settings.get("printer_model")
            && !text_of(model).is_some_and(|model| files.printer_models.contains(&model))
        {
            fail(
                report,
                "wrong_printer",
                format!(
                    "the file's project settings name another printer than the {}",
                    profile.name()
                ),
            );
        }
        if let Some(id) = settings.get("printer_settings_id")
            && !text_of(id).is_some_and(|id| files.printer_settings_ids.contains(&id))
        {
            fail(
                report,
                "wrong_printer",
                format!(
                    "the file's project settings name printer settings that are not settings of the {} that Hotend knows",
                    profile.name()
                ),
            );
        }
        if let Some(plate) = settings.get("curr_bed_type")
            && text_of(plate).as_deref() != Some(declared.plate_type.as_str())
        {
            fail(
                report,
                "plate_mismatch",
                format!(
                    "the file's project settings name another build plate than {}, the plate chosen for this job",
                    quoted(&declared.plate_type)
                ),
            );
        }
        if let Some(sizes) = settings.get("nozzle_diameter") {
            let sizes: Vec<Option<f64>> = sizes
                .as_array()
                .map(|sizes| {
                    sizes
                        .iter()
                        .take(8)
                        .map(|size| size.as_str().and_then(nozzle_size))
                        .collect()
                })
                .unwrap_or_default();
            for filament in &declared.filaments {
                let stated = profile
                    .nozzle_by_device_id(filament.nozzle)
                    .and_then(|nozzle| usize::from(nozzle.slicer_extruder).checked_sub(1))
                    .and_then(|at| sizes.get(at).copied().flatten());
                if stated.is_none_or(|size| !same_size(size, filament.nozzle_diameter_mm)) {
                    fail(
                        report,
                        "nozzle_mismatch",
                        format!(
                            "the file's project settings do not have a nozzle of {} mm where filament {} prints",
                            filament.nozzle_diameter_mm, filament.slot
                        ),
                    );
                }
            }
        }
    }
    if let Some(bytes) = plate_json {
        let Ok(plate) = serde_json::from_slice::<serde_json::Value>(bytes) else {
            fail(
                report,
                "slice_info",
                "the plate's description (`Metadata/plate_<n>.json`) cannot be read".to_owned(),
            );
            return;
        };
        if let Some(bed) = plate.get("bed_type") {
            let stated = text_of(bed);
            let wanted = profile
                .plate(&declared.plate_type)
                .and_then(|plate| plate.json_name.clone());
            // The word this entry uses for a plate is known only for plates that were
            // seen in a real slice. For the others it must at least not be the word of
            // a plate that is known, which would be a file sliced for that plate.
            let agrees = match &wanted {
                Some(_) => stated == wanted,
                None => stated.as_ref().is_some_and(|stated| {
                    !profile
                        .plates()
                        .iter()
                        .any(|known| known.json_name.as_ref() == Some(stated))
                }),
            };
            if !agrees {
                fail(
                    report,
                    "plate_mismatch",
                    format!(
                        "the plate's description in the file names another build plate than {}, the plate chosen for this job",
                        quoted(&declared.plate_type)
                    ),
                );
            }
        }
        if let Some(size) = plate.get("nozzle_diameter")
            && !declared.filaments.iter().any(|filament| {
                size.as_f64()
                    .is_some_and(|size| same_size(size, filament.nozzle_diameter_mm))
            })
        {
            fail(
                report,
                "nozzle_mismatch",
                "the plate's description in the file names a nozzle size no filament of the job has"
                    .to_owned(),
            );
        }
    }
}

/// Feeds what is read through it into an MD5.
struct Digesting<R> {
    inner: R,
    md5: Md5,
}

impl<R: Read> Read for Digesting<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.md5.update(&buffer[..read]);
        Ok(read)
    }
}

/// Checks the sliced file at `path` for delivery to a printer of `profile`.
///
/// `name` is the application's label for the job (it becomes part of the file name on
/// the printer); `declared` is the plate, the build plate and the filament map the
/// application sliced with. On success the job can be delivered ([`crate::delivery`])
/// and its summary confirmed ([`crate::token`]). [`JobError::Rejected`] carries the
/// linter's report.
pub fn check_job(
    path: &Path,
    name: &str,
    declared: &DeclaredJob,
    profile: &PrinterProfile,
    limits: &ArchiveLimits,
) -> Result<CheckedJob, JobError> {
    let name = clean_name(name)
        .ok_or_else(|| JobError::Unusable("the job has no usable name".to_owned()))?;
    if declared.plate == 0 || declared.plate > 999 {
        return Err(JobError::Unusable(
            "the plate number must be 1 to 999".to_owned(),
        ));
    }
    let plate = declared.plate;
    let file_name = shown_file_name(path)
        .ok_or_else(|| JobError::Unusable("the path names no file".to_owned()))?;
    // The one read of the file. From here on only these bytes are used.
    let (bytes, sha256) = read_once(path, limits.max_file_bytes)?;
    let bytes: Arc<[u8]> = bytes.into();
    let size_bytes = bytes.len() as u64;
    let mut archive = Archive::open(Cursor::new(Arc::clone(&bytes)), limits)?;

    // Only what a sliced file for this printer holds, and the declared plate alone.
    let files = profile.slice_file();
    let mut names = archive.names();
    names.sort();
    if let Some(strange) = names.iter().find(|name| !files.allows_entry(name, plate)) {
        return Err(JobError::Unusable(format!(
            "the archive holds {}, which is not an entry of a file sliced for plate {plate} of this printer; a second plate or any other content would travel to the printer unread",
            quoted(strange)
        )));
    }
    // The whole archive is what a hand-off, an export or an upload delivers, and a printer
    // or Bambu Connect lets a person pick any plate in it. Names that differ in case only
    // would let two readers of the archive see two different files (the archive reader
    // refuses those too), and only one G-code entry is checked, so it must be the only one.
    let mut folded: Vec<String> = names.iter().map(|name| name.to_lowercase()).collect();
    folded.sort();
    if folded.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(JobError::Unusable(
            "the archive holds two entries whose names differ only in upper and lower case"
                .to_owned(),
        ));
    }
    let gcode_name = format!("Metadata/plate_{plate}.gcode");
    if let Some(extra) = names
        .iter()
        .find(|name| **name != gcode_name && name.to_lowercase().ends_with(".gcode"))
    {
        return Err(JobError::Unusable(format!(
            "the archive holds G-code besides plate {plate} ({}); it was not checked, and a printer could be told to run it",
            quoted(extra)
        )));
    }
    for required in files.required_for(plate) {
        if !archive.has(&required) {
            return Err(JobError::Unusable(format!(
                "the archive has no `{required}`; a file sliced for plate {plate} has one"
            )));
        }
    }
    let info = match archive.read_capped(SLICE_INFO, limits.max_metadata_bytes)? {
        Some(bytes) => parse_slice_info(&bytes, plate),
        None => SliceInfo {
            problems: vec![format!("the sliced file has no `{SLICE_INFO}`")],
            ..SliceInfo::default()
        },
    };
    let settings = archive.read_capped(PROJECT_SETTINGS, limits.max_metadata_bytes)?;
    let plate_json = archive.read_capped(
        &format!("Metadata/plate_{plate}.json"),
        limits.max_metadata_bytes,
    )?;
    let stated_md5 = archive
        .read_capped(&format!("{gcode_name}.md5"), 64)?
        .unwrap_or_default();
    let (mut report, gcode_md5) = {
        let reader = archive
            .reader(&gcode_name, limits.max_gcode_bytes)?
            .ok_or_else(|| {
                JobError::Unusable(format!(
                    "the archive holds no G-code for plate {plate} (`{gcode_name}`)"
                ))
            })?;
        let mut reader = Digesting {
            inner: reader,
            md5: Md5::new(),
        };
        let report = lint_gcode(&mut reader, declared, profile, limits.max_gcode_bytes)
            .map_err(|e| ArchiveError::Hostile(format!("`{gcode_name}` cannot be read: {e}")))?;
        (report, reader.md5.finish())
    };
    // The archive's own checksum of the G-code. A report that stopped early (too large)
    // has an error already and did not read to the end.
    let stated_md5 = String::from_utf8_lossy(&stated_md5).trim().to_owned();
    let well_formed = stated_md5.len() == 32 && stated_md5.bytes().all(|b| b.is_ascii_hexdigit());
    if !well_formed {
        fail(
            &mut report,
            "gcode_md5_mismatch",
            format!("`{gcode_name}.md5` is not an MD5 (32 hex digits)"),
        );
    } else if report.passed() && !stated_md5.eq_ignore_ascii_case(&gcode_md5) {
        fail(
            &mut report,
            "gcode_md5_mismatch",
            format!(
                "`{gcode_name}.md5` is not the MD5 of the G-code in the file; the G-code was changed after it was sliced, or the file was put together from parts"
            ),
        );
    }
    check_other_statements(
        settings.as_deref(),
        plate_json.as_deref(),
        declared,
        profile,
        &mut report,
    );
    let prediction = info.prediction_s;
    let filaments = cross_check(info, declared, profile, &mut report);
    if !report.passed() {
        return Err(JobError::Rejected(Box::new(report)));
    }
    let stats = &report.stats;
    let summary = JobSummary {
        file_name,
        sha256,
        size_bytes,
        plate,
        estimated_seconds: prediction,
        min_seconds: stats.min_seconds.floor() as u64,
        layers: (stats.layers_printed > 0).then_some(stats.layers_printed),
        filaments,
        max_nozzle_c: stats.max_nozzle_c,
        max_bed_c: stats.max_bed_c,
        max_chamber_c: stats.max_chamber_c,
        extent: stats.extent,
        printer_name: profile.name().to_owned(),
        stated_printer: stats.stated.printer_model.clone(),
        // The profile's own name for the plate; the declaration was matched against it.
        plate_type: profile
            .plate(&declared.plate_type)
            .map(|plate| plate.name.clone()),
        max_nozzle_reference_c: stats.max_nozzle_reference_c,
        configuration: stats.configuration.clone(),
        unplaced_moves: stats.unplaced_moves,
    };
    Ok(CheckedJob {
        path: path.to_path_buf(),
        name,
        declared: declared.clone(),
        printer_model: profile.id().to_owned(),
        summary,
        lint: report,
        bytes,
    })
}
