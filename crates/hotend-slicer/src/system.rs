//! The system profiles of an installed Bambu Studio as the base of every real slice.
//!
//! Hotend does not ship slicer settings for a real printer. The machine, process and
//! filament settings come from the profiles the installed program ships, chosen by name for
//! the printer, the process preset and the material of the job
//! (`profiles/system-profiles.toml`). They are flattened into the job folder by the
//! `hotend-profile-flatten` program **inside the sandbox** ([`crate::flatten`]), and Hotend
//! sets only a short, fixed list of settings on top: the overrides of the process preset and
//! the plate type of the job.
//!
//! Guarantees:
//! - the core never parses a system profile. It writes a request, runs the reader in the
//!   sandbox and reads back a small report as hostile input. The reader sees the vendor's
//!   profile folder and its own program file read-only and can write only into the job's
//!   `profiles/` folder; it sees neither the model nor what lies next to Hotend's programs;
//! - a job names a process preset, a material and a plate type by Hotend's ids, and only
//!   an id the table lists selects a profile. A name of the installation cannot be given
//!   in a job, so nothing the agent passes chooses among the installed files;
//! - a printer, process or material without a system profile is an error that names what
//!   was looked for and where; a profile the installation lists for other printers, or one
//!   that is not a preset of its role, is an error too;
//! - a job cannot carry setting values: every override comes from the table compiled into
//!   this crate and is checked against the type and range of its setting;
//! - the vendor folder is a real folder directly below the profile folder, never a link.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};

use hotend_sandbox::{Limits, Profile};
use serde::Deserialize;

use crate::command::PROFILES_DIR;
use crate::detect::Installation;
use crate::error::SlicerError;
use hotend_settings::PrintSettings;

use crate::flatten::{
    self, AppliedReport, FlattenLimits, Report, Request, RequestItem, SettingItem,
};
use crate::job::untrusted_text;
use crate::keymap::{self, Keymap, Role};
use crate::run::run_sandboxed;
use crate::version::SlicerKind;

const TABLE_TOML: &str = include_str!("../profiles/system-profiles.toml");
const TABLE_FILE: &str = "profiles/system-profiles.toml";
/// File name of the request the core writes for the profile reader.
const REQUEST_NAME: &str = "flatten-request.json";
/// Largest report that is read back.
const MAX_REPORT_BYTES: u64 = 256 * 1024;
/// Wall-clock limit of the profile reader.
const READER_WALL_SECONDS: u64 = 60;

/// A plate type Hotend can slice for.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BedType {
    /// Hotend's id, for jobs and settings.
    pub id: String,
    /// The plate's name for people.
    pub name: String,
    /// Value of the slicer setting `curr_bed_type`.
    pub slicer_value: String,
    /// Filament setting that holds the bed temperature for this plate.
    pub temperature_key: String,
    /// Filament setting that holds the first layer's bed temperature for this plate.
    pub first_layer_temperature_key: String,
    /// Whether the value was sliced with on a real installation.
    pub verified: bool,
}

/// The nozzle inventory of a printer's two nozzles: the six machine keys an exported project
/// needs to slice on its own ([`crate::keymap::NOZZLE_INVENTORY_KEYS`]).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NozzleInventory {
    pub verified: bool,
    pub measured_on: String,
    /// Key to the list of values, one per nozzle.
    pub values: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessPreset {
    pub id: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Name of the system process profile.
    pub base: String,
    pub verified: bool,
    /// Settings set on top of the base: process rows of the keymap with a measured value.
    #[serde(default)]
    pub overrides: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialEntry {
    pub id: String,
    /// The material's name in system profile names, before the printer's suffix.
    pub base: String,
    pub verified: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemPrinter {
    pub id: String,
    /// Folder of the vendor below the installation's profile folder.
    pub vendor: String,
    pub nozzle: String,
    /// Name of the system machine profile.
    pub machine: String,
    pub machine_verified: bool,
    pub filament_suffix: String,
    /// The nozzle inventory written into the machine settings of a project export.
    #[serde(default)]
    pub nozzle_inventory: Option<NozzleInventory>,
    #[serde(default)]
    pub process: Vec<ProcessPreset>,
    #[serde(default)]
    pub material: Vec<MaterialEntry>,
}

/// How the plate type is told to the slicer on its command line.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BedTypeOption {
    /// The option, written as `<option>=<plate name>`.
    pub option: String,
    /// Whether a real program has been run with the option.
    pub verified: bool,
    /// Whether the option is passed at all. Without it the plate is told through
    /// `curr_bed_type` in the process settings only.
    #[serde(rename = "use")]
    pub used: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct TableFile {
    schema: u32,
    revision: String,
    measured_on: String,
    stand_in_bed_type: String,
    bed_type_option: BedTypeOption,
    bed_type: Vec<BedType>,
    printer: Vec<SystemPrinter>,
}

/// The table of `profiles/system-profiles.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemProfiles {
    pub revision: String,
    /// The installation the verified names were measured on.
    pub measured_on: String,
    /// The plate the stand-in slicer slices for when nobody chose one. A real slice has
    /// no such default: the plate on the printer must be stated.
    pub stand_in_bed_type: String,
    pub bed_type_option: BedTypeOption,
    pub bed_types: Vec<BedType>,
    pub printers: Vec<SystemPrinter>,
}

/// The system profiles chosen for one job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub vendor: String,
    pub machine: String,
    pub process: String,
    pub process_overrides: BTreeMap<String, String>,
    /// One system filament profile per slot, in slot order.
    pub filaments: Vec<String>,
    /// The chosen names that have not been seen on a real installation.
    pub unverified: Vec<String>,
}

impl Selection {
    /// The settings Hotend puts on top of the process profile whatever the plan says, in the
    /// order they are applied: the preset's overrides, then the plate type. Each is an item of
    /// the keymap ([`crate::keymap`]), so the helper checks it like any other.
    pub fn base_items(&self, bed: &BedType) -> Result<Vec<SettingItem>, SlicerError> {
        let keymap = Keymap::builtin()?;
        let mut items = Vec::new();
        let pairs = self
            .process_overrides
            .iter()
            .map(|(key, text)| (key.as_str(), text.as_str()))
            .chain(std::iter::once((
                "curr_bed_type",
                bed.slicer_value.as_str(),
            )));
        for (key, text) in pairs {
            let row = keymap
                .row(Role::Process, key)
                .ok_or_else(|| SlicerError::Data {
                    file: TABLE_FILE,
                    message: format!("`{key}` is not a process row of the keymap"),
                })?;
            items.push(SettingItem {
                key: key.to_owned(),
                op: row.op,
                text: text.to_owned(),
                absent: row.absent,
            });
        }
        Ok(items)
    }

    /// What [`Selection::base_items`] sets, as `(key, value)`, for messages.
    pub fn overrides(&self, bed: &BedType) -> Vec<(String, String)> {
        let mut overrides: Vec<(String, String)> = self
            .process_overrides
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        overrides.push(("curr_bed_type".to_owned(), bed.slicer_value.clone()));
        overrides
    }
}

impl SystemProfiles {
    /// The table compiled into this build.
    pub fn builtin() -> Result<Self, SlicerError> {
        Self::from_toml(TABLE_TOML).map_err(|message| SlicerError::Data {
            file: TABLE_FILE,
            message,
        })
    }

    /// Parses a table in the format of `profiles/system-profiles.toml`.
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let file: TableFile = toml::from_str(text).map_err(|e| e.to_string())?;
        if file.schema != 1 {
            return Err(format!("unsupported schema {}", file.schema));
        }
        let plain = |text: &str| {
            !text.is_empty()
                && text.len() <= 64
                && text
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || " ._-".contains(c))
        };
        for bed in &file.bed_type {
            let key_ok = [&bed.temperature_key, &bed.first_layer_temperature_key]
                .iter()
                .all(|key| keymap::valid_key(key) && !keymap::DENIED.matches(key));
            if !plain(&bed.id) || !plain(&bed.slicer_value) || !key_ok || bed.name.is_empty() {
                return Err(format!("the bed type `{}` is malformed", bed.id));
            }
            if !keymap::valid_plate_name(&bed.slicer_value) {
                return Err(format!("the bed type `{}` cannot be written", bed.id));
            }
        }
        if !file.bed_type.iter().any(|b| b.id == file.stand_in_bed_type) {
            return Err(format!(
                "the stand-in's bed type `{}` is not listed",
                file.stand_in_bed_type
            ));
        }
        let option = &file.bed_type_option.option;
        let option_ok = option.strip_prefix("--").is_some_and(|name| {
            !name.is_empty()
                && name.len() <= 40
                && name.chars().all(|c| c.is_ascii_lowercase() || c == '-')
        });
        if !option_ok {
            return Err(format!("`{option}` is not a command-line option"));
        }
        for printer in &file.printer {
            if !plain(&printer.vendor) || printer.vendor.contains(['.', ' ']) {
                return Err(format!(
                    "the vendor folder of `{}` is malformed",
                    printer.id
                ));
            }
            let mut names = vec![printer.machine.clone()];
            names.extend(printer.process.iter().map(|p| p.base.clone()));
            names.extend(
                printer
                    .material
                    .iter()
                    .map(|m| format!("{}{}", m.base, printer.filament_suffix)),
            );
            if let Some(bad) = names.iter().find(|name| !flatten::valid_name(name)) {
                return Err(format!("`{bad}` cannot name a system profile"));
            }
            let map = Keymap::builtin().map_err(|e| e.to_string())?;
            let plates: Vec<&str> = file
                .bed_type
                .iter()
                .map(|bed| bed.slicer_value.as_str())
                .collect();
            for preset in &printer.process {
                for (key, text) in &preset.overrides {
                    let Some(row) = map
                        .row(Role::Process, key)
                        .filter(|row| key != "curr_bed_type" && row.enabled)
                    else {
                        return Err(format!(
                            "the preset `{}` overrides `{key}`, which is not a process setting \
                             of the keymap a preset may override",
                            preset.id
                        ));
                    };
                    if !row.text_valid(text, &plates) || !row.is_verified(text) {
                        return Err(format!(
                            "the preset `{}` sets `{key}` to a value that setting cannot have \
                             or that has not been measured (the keymap gives the form, the \
                             range and what is measured)",
                            preset.id
                        ));
                    }
                }
            }
            if let Some(inventory) = &printer.nozzle_inventory {
                let keys: Vec<&str> = inventory.values.keys().map(String::as_str).collect();
                let mut expected = keymap::NOZZLE_INVENTORY_KEYS.to_vec();
                expected.sort_unstable();
                let values_ok = inventory.values.values().all(|list| {
                    (1..=2).contains(&list.len())
                        && list
                            .iter()
                            .all(|value| keymap::valid_inventory_value(value))
                });
                if keys != expected || !values_ok || inventory.measured_on.is_empty() {
                    return Err(format!(
                        "the nozzle inventory of `{}` must give the six inventory keys, each \
                         with one or two plain values",
                        printer.id
                    ));
                }
            }
        }
        Ok(Self {
            revision: file.revision,
            measured_on: file.measured_on,
            stand_in_bed_type: file.stand_in_bed_type,
            bed_type_option: file.bed_type_option,
            bed_types: file.bed_type,
            printers: file.printer,
        })
    }

    /// The ids of the plate types, for messages.
    pub fn bed_type_ids(&self) -> Vec<String> {
        self.bed_types.iter().map(|b| b.id.clone()).collect()
    }

    /// The plate type with this id.
    pub fn bed_type(&self, id: &str) -> Result<&BedType, SlicerError> {
        self.bed_types.iter().find(|b| b.id == id).ok_or_else(|| {
            SlicerError::InvalidJob(format!(
                "`{}` is not a plate type; the plate types are: {}",
                untrusted_text(id, 40),
                self.bed_type_ids().join(", ")
            ))
        })
    }

    /// The plate type whose slicer value is `value`.
    pub fn bed_type_by_value(&self, value: &str) -> Option<&BedType> {
        self.bed_types.iter().find(|b| b.slicer_value == value)
    }

    /// The table's entry for a printer.
    pub fn printer(&self, id: &str) -> Option<&SystemPrinter> {
        self.printers.iter().find(|printer| printer.id == id)
    }

    /// The system profiles for a printer, a process preset and the materials of the slots.
    ///
    /// Only an id (or alias) the table lists selects a profile. The process id is what the
    /// agent's `slice` tool passes on, so an id that is not listed is refused instead of
    /// being taken as the name of an installed file: the agent picks from a list, it does
    /// not choose among the files of the installation.
    pub fn select(
        &self,
        printer_id: &str,
        process_id: &str,
        materials: &[&str],
    ) -> Result<Selection, SlicerError> {
        let kind = SlicerKind::BambuStudio.display_name();
        let printer = self
            .printer(printer_id)
            .ok_or_else(|| SlicerError::ProfileNotFound {
                kind,
                role: "machine",
                id: printer_id.to_owned(),
                searched: format!(
                    "Hotend's table of system profiles ({TABLE_FILE}), which knows these \
                     printers: {}",
                    self.printers
                        .iter()
                        .map(|p| p.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            })?;
        let mut unverified = Vec::new();
        if !printer.machine_verified {
            unverified.push(printer.machine.clone());
        }
        let preset = printer
            .process
            .iter()
            .find(|p| p.id == process_id || p.aliases.iter().any(|alias| alias == process_id))
            .ok_or_else(|| {
                let known: Vec<String> = printer
                    .process
                    .iter()
                    .map(|preset| match preset.aliases.as_slice() {
                        [] => preset.id.clone(),
                        aliases => format!("{} (also {})", preset.id, aliases.join(", ")),
                    })
                    .collect();
                SlicerError::InvalidJob(format!(
                    "`{}` is not a process preset for {printer_id}; the presets are: {}. A \
                     profile of the installed slicer cannot be named directly",
                    untrusted_text(process_id, 60),
                    known.join(", ")
                ))
            })?;
        if !preset.verified {
            unverified.push(preset.base.clone());
        }
        let mut filaments = Vec::new();
        for material in materials {
            let entry = printer
                .material
                .iter()
                .find(|m| m.id == *material)
                .ok_or_else(|| {
                    let known: Vec<&str> = printer.material.iter().map(|m| m.id.as_str()).collect();
                    SlicerError::InvalidJob(format!(
                        "`{}` is not a material Hotend has a system profile for on \
                         {printer_id}; the materials are: {}. A profile of the installed \
                         slicer cannot be named directly",
                        untrusted_text(material, 60),
                        known.join(", ")
                    ))
                })?;
            let name = format!("{}{}", entry.base, printer.filament_suffix);
            if !entry.verified {
                unverified.push(name.clone());
            }
            filaments.push(name);
        }
        Ok(Selection {
            vendor: printer.vendor.clone(),
            machine: printer.machine.clone(),
            process: preset.base.clone(),
            process_overrides: preset.overrides.clone(),
            filaments,
            unverified,
        })
    }
}

/// The folder of an installation that holds the vendors' profile folders
/// (`resources/profiles`), if it is where an installation is known or expected to have it.
///
/// Measured on Windows (02.09.01.70): `resources\profiles` next to `bambu-studio.exe`. The
/// other places are where a Linux archive and a macOS bundle are expected to keep it and
/// have not been seen. `explicit` (from the application's settings) wins and is final.
pub fn profile_folder(installation: &Installation, explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(explicit) = explicit {
        return Some(explicit.to_path_buf());
    }
    if installation.fake {
        return None;
    }
    let real = std::fs::canonicalize(&installation.path).ok()?;
    let dir = real.parent()?;
    let mut candidates = vec![dir.join("resources").join("profiles")];
    if let Some(prefix) = dir.parent() {
        candidates.push(prefix.join("resources").join("profiles"));
        candidates.push(prefix.join("Resources").join("profiles"));
    }
    candidates.into_iter().find(|path| path.is_dir())
}

/// What the profile reader did for one job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    /// The reader's report: per profile its chain, templates and number of settings.
    pub report: Report,
    /// Profiles that do not say which printers they are for.
    pub compatibility_unstated: Vec<String>,
    /// Keys without content that were left out of a process or filament profile because
    /// only the machine profile may carry them, as `<role>: <key>`.
    pub dropped: Vec<String>,
    /// What Hotend set on top of the system profiles, as `(key, value)`.
    pub overrides: Vec<(String, String)>,
    /// What the helper wrote, per role, checked: only keys of the keymap (or of the nozzle
    /// inventory for the machine), entries capped in number and length.
    pub applied: Vec<(Role, AppliedReport)>,
    /// Keys of rows that are not measured whose asked value the profile already held.
    pub unchanged: Vec<(Role, String)>,
}

impl Prepared {
    /// The keys whose value the helper changed in `role`, sorted: what an exported project's
    /// difference list names for that preset.
    pub fn changed_keys(&self, role: Role) -> Vec<String> {
        let mut keys: Vec<String> = self
            .applied
            .iter()
            .filter(|(of, applied)| *of == role && applied.changed)
            .map(|(_, applied)| applied.key.clone())
            .collect();
        keys.sort_unstable();
        keys.dedup();
        keys
    }

    /// The new entries the helper wrote for `key`, if it wrote it.
    pub fn written(&self, key: &str) -> Option<&[String]> {
        self.applied
            .iter()
            .find(|(_, applied)| applied.key == key)
            .map(|(_, applied)| applied.new.as_slice())
    }
}

/// The settings put on top of the flattened profiles of one job.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileItems {
    /// Items for the process profile: the preset's overrides, the plate, the plan's.
    pub process: Vec<SettingItem>,
    /// Items for the filament profile of slot 1.
    pub filament: Vec<SettingItem>,
    /// Write the nozzle inventory into the machine profile (project export only).
    pub inventory: bool,
}

/// What a run of the profile reader may read besides the system's own paths: the vendor's
/// profile folder and the reader's program file. Not the folder the program lies in:
/// Hotend may be installed in a folder it shares with other things, and the reader
/// parses files Hotend did not write.
fn reader_view(vendor_dir: &Path, reader: &Path) -> Vec<PathBuf> {
    let reader = std::fs::canonicalize(reader).unwrap_or_else(|_| reader.to_path_buf());
    vec![vendor_dir.to_path_buf(), reader]
}

/// Flattens the selected system profiles into `<job>/profiles/` by running the profile
/// reader in the sandbox, with `items` put on top (the plate type, the preset's overrides and
/// the plan's settings; the nozzle inventory for a project export). `settings` only give the
/// asked values for an error message. `profiles_root` is the installation's
/// `resources/profiles`; `outputs` are the file names for the machine, the process and each
/// filament, in that order. `reader` is the reader program; `None` is the one built with
/// this crate.
#[allow(clippy::too_many_arguments)]
pub(crate) fn flatten_into_job(
    job_dir: &Path,
    profiles_root: &Path,
    selection: &Selection,
    items: &ProfileItems,
    settings: Option<&PrintSettings>,
    outputs: &[String],
    limits: &FlattenLimits,
    reader: Option<&Path>,
) -> Result<Prepared, SlicerError> {
    let unusable = |reason: String| SlicerError::ProfileUnusable {
        id: selection.machine.clone(),
        reason,
    };
    let helper = match reader {
        Some(reader) => reader.to_path_buf(),
        None => flatten::locate_helper().ok_or_else(|| {
            unusable(format!(
                "the profile reader `{}` is not next to the running program, so the \
                 installed profiles cannot be read; it is built with this crate",
                flatten::helper_name()
            ))
        })?,
    };
    let vendor_dir = vendor_folder(profiles_root, selection)?;

    let mut names = vec![
        (
            "machine",
            selection.machine.clone(),
            Vec::new(),
            items.inventory,
        ),
        (
            "process",
            selection.process.clone(),
            items.process.clone(),
            false,
        ),
    ];
    for (slot, filament) in selection.filaments.iter().enumerate() {
        let own = if slot == 0 {
            items.filament.clone()
        } else {
            Vec::new()
        };
        names.push(("filament", filament.clone(), own, false));
    }
    if names.len() != outputs.len() {
        return Err(SlicerError::InvalidJob(
            "the settings files of the job do not match its filaments".into(),
        ));
    }
    let request = Request {
        root: vendor_dir.clone(),
        machine: selection.machine.clone(),
        limits: *limits,
        items: names
            .into_iter()
            .zip(outputs)
            .map(|((role, name, items, inventory), output)| RequestItem {
                role: role.to_owned(),
                name,
                output: output.clone(),
                items,
                inventory,
            })
            .collect(),
    };
    let out_dir = job_dir.join(PROFILES_DIR);
    let request_path = out_dir.join(REQUEST_NAME);
    let json = serde_json::to_vec(&request).map_err(|e| unusable(e.to_string()))?;
    std::fs::write(&request_path, json).map_err(|e| SlicerError::io("write", &request_path, e))?;

    // The reader's job folder is `profiles/`, not the job folder: it needs to write the
    // flattened files and nothing else, and it has no business with the model.
    let run = run_sandboxed(
        &out_dir,
        &reader_view(&vendor_dir, &helper),
        Limits {
            wall_seconds: READER_WALL_SECONDS,
            ..Profile::Slicer.default_limits()
        },
        &helper,
        &[
            OsString::from(&request_path),
            OsString::from(out_dir.as_os_str()),
        ],
    )?;
    // The home and temp folders the sandbox made for the reader are not settings.
    let leftovers = out_dir.join(hotend_sandbox::SANDBOX_DIR);
    if let Ok(meta) = std::fs::symlink_metadata(&leftovers) {
        let removed = if meta.file_type().is_dir() {
            std::fs::remove_dir_all(&leftovers)
        } else {
            std::fs::remove_file(&leftovers)
        };
        removed.map_err(|e| SlicerError::io("clear", &leftovers, e))?;
    }
    match run.code {
        Some(124) => {
            return Err(SlicerError::Timeout {
                seconds: READER_WALL_SECONDS,
            });
        }
        Some(125) => {
            return Err(SlicerError::Sandbox(
                hotend_sandbox::SandboxError::Unavailable(untrusted_text(&run.text(), 400)),
            ));
        }
        _ => {}
    }

    let relative = format!("{PROFILES_DIR}/{}", flatten::REPORT_NAME);
    let report = read_report(job_dir, &relative).map_err(|reason| {
        unusable(format!(
            "the profile reader stopped without a usable report ({reason}; exit code {:?})",
            run.code
        ))
    })?;
    if let Some(error) = &report.error {
        let message = untrusted_text(&error.message, 900);
        if keymap::MAPPING_CODES.contains(&error.code.as_str()) {
            let map = Keymap::builtin()?;
            let key = untrusted_text(&error.key, 64);
            // The detail is the helper's own sentence; only a key of the keymap names a field.
            let detail = match error.code.as_str() {
                "setting_not_numeric" => "an installed entry is not a plain number".to_owned(),
                "setting_not_applied" => {
                    "an installed entry is 0, which can mean \"follow another setting\"".to_owned()
                }
                "setting_shape_mismatch" => "the installed value has another shape".to_owned(),
                _ => format!("`{key}`"),
            };
            return Err(map.error(
                &keymap::MapError {
                    code: keymap::known_code(&error.code),
                    key,
                    detail,
                },
                settings,
            ));
        }
        return Err(match error.code.as_str() {
            "not_found" => SlicerError::ProfileNotFound {
                kind: SlicerKind::BambuStudio.display_name(),
                role: match error.role.as_str() {
                    "machine" => "machine",
                    "process" => "process",
                    _ => "filament",
                },
                id: untrusted_text(&error.name, 120),
                searched: format!(
                    "the installed program's folder {} ({message})",
                    vendor_dir.display()
                ),
            },
            _ => SlicerError::ProfileUnusable {
                id: untrusted_text(&error.name, 120),
                reason: format!("the installed system profile could not be flattened: {message}"),
            },
        });
    }
    let covers = report.ok
        && report.items.len() == outputs.len()
        && report
            .items
            .iter()
            .zip(&request.items)
            .all(|(item, asked)| item.role == asked.role && item.output == asked.output);
    if !covers {
        return Err(unusable(
            "the profile reader's report does not cover the job".into(),
        ));
    }
    for output in outputs {
        let relative = format!("{PROFILES_DIR}/{output}");
        hotend_core::import::resolve_inside(job_dir, &relative, 8 * 1024 * 1024).map_err(|e| {
            unusable(format!(
                "the flattened profile `{output}` cannot be used: {e}"
            ))
        })?;
    }
    // A profile the installation lists for other printers is not sliced with: its speeds,
    // temperatures and layer heights are for another machine or nozzle.
    if let Some(other) = report.items.iter().find(|item| item.compatible == "no") {
        return Err(SlicerError::ProfileUnusable {
            id: untrusted_text(&other.name, 120),
            reason: format!(
                "the installed program lists this {} profile for other printers, not for \
                 `{}`",
                untrusted_text(&other.role, 20),
                selection.machine
            ),
        });
    }
    let compatibility_unstated = report
        .items
        .iter()
        .filter(|item| item.role != "machine" && item.compatible != "yes")
        .map(|item| untrusted_text(&item.name, 120))
        .collect();
    let dropped = report
        .items
        .iter()
        .flat_map(|item| {
            item.dropped.iter().take(64).map(|key| {
                format!(
                    "{}: {}",
                    untrusted_text(&item.role, 20),
                    untrusted_text(key, 80)
                )
            })
        })
        .collect();
    // What the helper wrote, read back as hostile input.
    let map = Keymap::builtin()?;
    let mut applied = Vec::new();
    let mut unchanged = Vec::new();
    for (item, asked) in report.items.iter().zip(&request.items) {
        let role = match asked.role.as_str() {
            "machine" => Role::Machine,
            "process" => Role::Process,
            _ => Role::Filament,
        };
        let allowed = |key: &str| match role {
            Role::Machine => asked.inventory && keymap::NOZZLE_INVENTORY_KEYS.contains(&key),
            _ => {
                asked.items.iter().any(|setting| setting.key == key) && map.row(role, key).is_some()
            }
        };
        let entries_ok = |list: &[String]| {
            list.len() <= keymap::MAX_ENTRIES
                && list
                    .iter()
                    .all(|entry| entry.chars().count() <= keymap::MAX_ENTRY_CHARS)
        };
        for entry in item.applied.iter().take(128) {
            if !allowed(&entry.key)
                || !entries_ok(&entry.new)
                || !entry.old.as_deref().is_none_or(entries_ok)
            {
                return Err(unusable(
                    "the profile reader reported a setting Hotend did not ask for".into(),
                ));
            }
            applied.push((role, entry.clone()));
        }
        for key in item.unchanged.iter().take(128) {
            if !allowed(key) {
                return Err(unusable(
                    "the profile reader reported a setting Hotend did not ask for".into(),
                ));
            }
            unchanged.push((role, key.clone()));
        }
        // Every item asked for is either written or reported unchanged.
        let answered = asked.items.iter().all(|setting| {
            item.applied.iter().any(|entry| entry.key == setting.key)
                || item.unchanged.contains(&setting.key)
        });
        if !answered {
            return Err(unusable(
                "the profile reader did not report every setting Hotend asked for".into(),
            ));
        }
    }
    let overrides = applied
        .iter()
        .map(|(_, entry)| {
            let mut value = entry.new.join(",");
            if entry.new.len() > 1 && entry.new.iter().all(|text| *text == entry.new[0]) {
                value = entry.new[0].clone();
            }
            (entry.key.clone(), untrusted_text(&value, 64))
        })
        .collect();
    Ok(Prepared {
        report,
        compatibility_unstated,
        dropped,
        overrides,
        applied,
        unchanged,
    })
}

/// The vendor's folder below the installation's profile folder: a real folder directly
/// below it, reached without a link. Returns its resolved path.
fn vendor_folder(profiles_root: &Path, selection: &Selection) -> Result<PathBuf, SlicerError> {
    let vendor_dir = profiles_root.join(&selection.vendor);
    let not_found = || SlicerError::ProfileNotFound {
        kind: SlicerKind::BambuStudio.display_name(),
        role: "machine",
        id: selection.machine.clone(),
        searched: format!(
            "{}, which does not exist. Set the folder of the installed Bambu Studio's \
             profiles (resources/profiles) in the settings",
            vendor_dir.display()
        ),
    };
    let linked = || SlicerError::ProfileUnusable {
        id: selection.machine.clone(),
        reason: format!(
            "the vendor folder {} is a symbolic link or does not lie directly in the profile \
             folder; links in the profile tree are never followed",
            vendor_dir.display()
        ),
    };
    let meta = std::fs::symlink_metadata(&vendor_dir).map_err(|_| not_found())?;
    if meta.file_type().is_symlink() {
        return Err(linked());
    }
    if !meta.is_dir() {
        return Err(not_found());
    }
    let root = std::fs::canonicalize(profiles_root).map_err(|_| not_found())?;
    let resolved = std::fs::canonicalize(&vendor_dir).map_err(|_| not_found())?;
    if resolved.parent() != Some(root.as_path()) {
        return Err(linked());
    }
    Ok(resolved)
}

/// Reads the reader's report as hostile input: a plain file inside the job folder, size
/// capped, strictly typed.
fn read_report(job_dir: &Path, relative: &str) -> Result<Report, String> {
    let path = hotend_core::import::resolve_inside(job_dir, relative, MAX_REPORT_BYTES)
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .and_then(|file| file.take(MAX_REPORT_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_REPORT_BYTES {
        return Err("the report is too large".into());
    }
    let report: Report = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if report.items.len() > 64 || report.items.iter().any(|item| item.chain.len() > 64) {
        return Err("the report is too long".into());
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn the_builtin_table_names_the_measured_x2d_profiles() {
        let table = SystemProfiles::builtin().unwrap();
        assert!(table.measured_on.contains("02.09.01.70"));
        let chosen = table
            .select("bambu-x2d", "standard-0.20mm", &["pla-basic"])
            .unwrap();
        assert_eq!(chosen.vendor, "BBL");
        assert_eq!(chosen.machine, "Bambu Lab X2D 0.4 nozzle");
        assert_eq!(chosen.process, "0.20mm Standard @BBL X2D");
        assert_eq!(chosen.filaments, ["Bambu PLA Basic @BBL X2D 0.4 nozzle"]);
        // The three names were sliced with on a real installation.
        assert!(chosen.unverified.is_empty(), "{:?}", chosen.unverified);
        // The demo's process id is the same preset.
        let alias = table
            .select("bambu-x2d", "demo-0.20mm", &["pla-basic"])
            .unwrap();
        assert_eq!(alias.process, chosen.process);
        // Names that follow the pattern but were not seen are listed as such.
        let guessed = table
            .select("bambu-x2d", "fine-0.12mm", &["pla-basic", "tpu-95a"])
            .unwrap();
        assert_eq!(
            guessed.unverified,
            ["0.12mm Fine @BBL X2D", "Bambu TPU 95A @BBL X2D 0.4 nozzle"]
        );
        // The plate type is the one override of a preset that sets none.
        let cool = table.bed_type("cool").unwrap();
        assert_eq!(
            chosen.overrides(cool),
            [("curr_bed_type".to_owned(), "Cool Plate".to_owned())]
        );
        let items = chosen.base_items(cool).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(
            (items[0].key.as_str(), items[0].text.as_str()),
            ("curr_bed_type", "Cool Plate")
        );
        // The default filament: the generic preset, measured.
        let generic = table
            .select("bambu-x2d", "standard-0.20mm", &["pla-generic"])
            .unwrap();
        assert_eq!(generic.filaments, ["Generic PLA @BBL X2D 0.4 nozzle"]);
        assert!(generic.unverified.is_empty());
        // The nozzle inventory of both nozzles, for a project export.
        let inventory = table.printers[0].nozzle_inventory.as_ref().unwrap();
        assert!(inventory.verified);
        assert_eq!(inventory.values["extruder_nozzle_slot"], ["0", "1"]);
        assert_eq!(table.printers[0].nozzle, "0.4");
    }

    #[test]
    fn an_id_the_table_does_not_list_selects_nothing() {
        let table = SystemProfiles::builtin().unwrap();
        // The process id is what the agent's `slice` tool passes on. A name of the
        // installation is not an id: not another printer's profile, not a base profile,
        // not the preset's own system name.
        for process in [
            "0.16mm Something @BBL X2D",
            "0.28mm Draft @Other Printer 0.8 nozzle",
            "fdm_process_common",
            "0.20mm Standard @BBL X2D",
            "",
        ] {
            let error = table
                .select("bambu-x2d", process, &["pla-basic"])
                .unwrap_err();
            assert_eq!(error.code(), "invalid_job", "{process}");
            let text = error.to_string();
            assert!(
                text.contains("standard-0.20mm (also demo-0.20mm)") && text.contains("fine-0.12mm"),
                "{text}"
            );
        }
        for material in [
            "Some Filament",
            "Bambu PLA Basic @base",
            "Bambu PLA Basic @BBL X2D 0.4 nozzle",
        ] {
            let error = table
                .select("bambu-x2d", "standard-0.20mm", &["pla-basic", material])
                .unwrap_err();
            assert_eq!(error.code(), "invalid_job", "{material}");
            assert!(
                error
                    .to_string()
                    .contains("pla-basic, pla-generic, petg-basic"),
                "{error}"
            );
        }
    }

    #[test]
    fn a_printer_without_system_profiles_is_named_in_the_error() {
        let table = SystemProfiles::builtin().unwrap();
        let error = table
            .select("prusa-mk4s", "standard-0.20mm", &["pla-basic"])
            .unwrap_err();
        assert_eq!(error.code(), "profile_not_found");
        let text = error.to_string();
        assert!(
            text.contains("prusa-mk4s") && text.contains("bambu-x2d"),
            "{text}"
        );
        let error = table
            .select("bambu-x2d", "../../etc/passwd", &["pla-basic"])
            .unwrap_err();
        assert_eq!(error.code(), "invalid_job");
    }

    #[test]
    fn plate_types_are_named_and_unknown_ones_are_refused() {
        let table = SystemProfiles::builtin().unwrap();
        // The plate of the stand-in slicer; a real slice has no default.
        let stand_in = table.bed_type(&table.stand_in_bed_type).unwrap();
        assert_eq!(stand_in.id, "textured-pei");
        assert_eq!(stand_in.slicer_value, "Textured PEI Plate");
        assert_eq!(stand_in.temperature_key, "textured_plate_temp");
        assert_eq!(
            stand_in.first_layer_temperature_key,
            "textured_plate_temp_initial_layer"
        );
        // Measured: two test slices, a hand-made project and a holder.
        assert!(stand_in.verified);
        // The plate goes into the process settings; the option is not passed.
        assert_eq!(table.bed_type_option.option, "--curr-bed-type");
        assert!(!table.bed_type_option.verified);
        assert!(!table.bed_type_option.used);
        // Every plate's two temperature keys are the keys of its two keymap rows.
        let map = Keymap::builtin().unwrap();
        for bed in &table.bed_types {
            for key in [&bed.temperature_key, &bed.first_layer_temperature_key] {
                let row = map.row(Role::Filament, key).unwrap();
                assert_eq!(row.plate.map(|plate| plate.as_str()), Some(bed.id.as_str()));
            }
        }
        let cool = table.bed_type("cool").unwrap();
        assert_eq!(
            (cool.slicer_value.as_str(), cool.verified),
            ("Cool Plate", true)
        );
        assert_eq!(table.bed_type_by_value("Cool Plate").unwrap().id, "cool");
        let error = table.bed_type("glass").unwrap_err();
        assert_eq!(error.code(), "invalid_job");
        assert!(error.to_string().contains("textured-pei"), "{error}");
    }

    #[test]
    fn malformed_tables_are_rejected() {
        let base = TABLE_TOML;
        for (from, to) in [
            ("schema = 1", "schema = 2"),
            (
                "stand_in_bed_type = \"textured-pei\"",
                "stand_in_bed_type = \"none\"",
            ),
            (
                "option = \"--curr-bed-type\"",
                "option = \"--curr-bed-type=x --post-process\"",
            ),
            // An override outside the range of its setting, or of the wrong type.
            (
                "base = \"0.20mm Standard @BBL X2D\"\nverified = true",
                "base = \"0.20mm Standard @BBL X2D\"\nverified = true\noverrides = { layer_height = \"9999\" }",
            ),
            (
                "base = \"0.20mm Standard @BBL X2D\"\nverified = true",
                "base = \"0.20mm Standard @BBL X2D\"\nverified = true\noverrides = { enable_support = \"-5\" }",
            ),
            (
                "base = \"0.20mm Standard @BBL X2D\"\nverified = true",
                "base = \"0.20mm Standard @BBL X2D\"\nverified = true\noverrides = { layer_height = \"0.9\" }",
            ),
            ("vendor = \"BBL\"", "vendor = \"../BBL\""),
            (
                "slicer_value = \"Cool Plate\"",
                "slicer_value = \"Cool\\nM140 S200\"",
            ),
            (
                "base = \"0.20mm Standard @BBL X2D\"",
                "base = \"../0.20mm Standard @BBL X2D\"",
            ),
            (
                "base = \"0.20mm Standard @BBL X2D\"\nverified = true",
                "base = \"0.20mm Standard @BBL X2D\"\nverified = true\noverrides = { machine_start_gcode = \"G1\" }",
            ),
            (
                "base = \"0.20mm Standard @BBL X2D\"\nverified = true",
                "base = \"0.20mm Standard @BBL X2D\"\nverified = true\noverrides = { curr_bed_type = \"Cool Plate\" }",
            ),
        ] {
            assert!(base.contains(from), "{from}");
            assert!(
                SystemProfiles::from_toml(&base.replace(from, to)).is_err(),
                "{to}"
            );
        }
        let allowed = base.replace(
            "base = \"0.20mm Standard @BBL X2D\"\nverified = true",
            "base = \"0.20mm Standard @BBL X2D\"\nverified = true\noverrides = { wall_loops = \"3\" }",
        );
        let table = SystemProfiles::from_toml(&allowed).unwrap();
        let chosen = table
            .select("bambu-x2d", "standard-0.20mm", &["pla-basic"])
            .unwrap();
        assert_eq!(chosen.process_overrides["wall_loops"], "3");
        // What is set on top is listed for the result, the plate type last.
        assert_eq!(
            chosen.overrides(table.bed_type("cool").unwrap()),
            [
                ("wall_loops".to_owned(), "3".to_owned()),
                ("curr_bed_type".to_owned(), "Cool Plate".to_owned())
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_vendor_folder_is_a_real_folder_directly_in_the_profile_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap().join("profiles");
        std::fs::create_dir_all(root.join("BBL")).unwrap();
        let table = SystemProfiles::builtin().unwrap();
        let chosen = table
            .select("bambu-x2d", "standard-0.20mm", &["pla-basic"])
            .unwrap();
        assert_eq!(vendor_folder(&root, &chosen).unwrap(), root.join("BBL"));

        // A link in its place is not followed, wherever it leads.
        std::fs::remove_dir(root.join("BBL")).unwrap();
        let elsewhere = root.parent().unwrap().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("BBL")).unwrap();
        let error = vendor_folder(&root, &chosen).unwrap_err();
        assert_eq!(error.code(), "profile_unusable");
        assert!(error.to_string().contains("symbolic link"), "{error}");
        std::fs::remove_file(root.join("BBL")).unwrap();
        std::fs::create_dir(root.join("inside")).unwrap();
        std::os::unix::fs::symlink(root.join("inside"), root.join("BBL")).unwrap();
        assert_eq!(
            vendor_folder(&root, &chosen).unwrap_err().code(),
            "profile_unusable"
        );
        // No folder at all, or a file, is "not found" with what to do about it.
        std::fs::remove_file(root.join("BBL")).unwrap();
        let error = vendor_folder(&root, &chosen).unwrap_err();
        assert_eq!(error.code(), "profile_not_found");
        std::fs::write(root.join("BBL"), b"x").unwrap();
        assert_eq!(
            vendor_folder(&root, &chosen).unwrap_err().code(),
            "profile_not_found"
        );
    }

    #[test]
    fn the_profile_reader_is_given_its_own_file_and_not_its_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let reader = root.join("bin").join("hotend-profile-flatten");
        std::fs::create_dir(root.join("bin")).unwrap();
        std::fs::write(&reader, b"x").unwrap();
        let vendor = root.join("profiles/BBL");
        assert_eq!(reader_view(&vendor, &reader), [vendor, reader]);
    }

    #[test]
    fn the_profile_folder_is_looked_for_next_to_the_program_only() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let install = |exe: &str, fake: bool| {
            let path = root.join(exe);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"x").unwrap();
            Installation {
                kind: SlicerKind::BambuStudio,
                path,
                found: crate::detect::Found::Override,
                version: None,
                fake,
                read_only: Vec::new(),
                verdict: crate::version::Verdict::Allow,
                note: None,
            }
        };
        // The layout measured on Windows: resources/profiles next to the program.
        let windows = install("win/bambu-studio.exe", false);
        assert_eq!(profile_folder(&windows, None), None);
        std::fs::create_dir_all(root.join("win/resources/profiles")).unwrap();
        assert_eq!(
            profile_folder(&windows, None),
            Some(root.join("win/resources/profiles"))
        );
        // A `bin` folder with the resources one level up.
        let linux = install("linux/bin/bambu-studio", false);
        std::fs::create_dir_all(root.join("linux/resources/profiles")).unwrap();
        assert_eq!(
            profile_folder(&linux, None),
            Some(root.join("linux/resources/profiles"))
        );
        // The stand-in has no profiles of its own; an explicit folder always wins.
        let fake = install("win/fake", true);
        assert_eq!(profile_folder(&fake, None), None);
        let explicit = root.join("chosen");
        assert_eq!(profile_folder(&fake, Some(&explicit)), Some(explicit));
    }
}
