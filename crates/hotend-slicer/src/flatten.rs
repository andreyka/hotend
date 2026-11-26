//! Flattens the system profiles of an installed Bambu Studio.
//!
//! The profiles under `resources/profiles/<vendor>/{machine,process,filament}` are layered:
//! a profile names its parent in `inherits` and may pull in templates through `include`.
//! The command line resolves that only in part (measured on 02.09.01.70: an unflattened
//! filament profile gave a density of 0 and a weight of 0.00 g), and the vendor's own
//! documentation asks for "a full config". So Hotend flattens them itself: the parent
//! chain first, then the templates a profile includes, then the profile's own keys, each
//! step overriding the one before.
//!
//! The profile files are data from a program the user installed, and they are files Hotend
//! did not write. They are therefore parsed only inside the sandbox: [`helper_main`] is the
//! body of the `hotend-profile-flatten` program, which [`crate::system`] starts under the
//! `slicer` sandbox profile with the profile folder read-only. The core reads nothing but
//! the small report that program writes.
//!
//! Guarantees, whatever the folder holds:
//! - a profile is found by comparing its name with the file names that are really in the
//!   role's folder; no path is ever built from a name, so a name cannot leave the folder.
//!   Names with a path separator, `..`, a leading dot or a control character are refused
//!   before the lookup;
//! - symbolic links are never followed: a linked file is not a profile and asking for it by
//!   name is an error, and a vendor folder or a role folder that is a link is refused;
//! - only a profile that says it is a preset of its role (`instantiation` true and a `type`
//!   equal to the role) is flattened for use, so a base profile cannot be sliced with;
//! - G-code and scripts come from the machine profile only ([`forbidden_key`]): a process
//!   or filament profile that carries a routine, and any profile that carries a
//!   post-processing script, is refused, and such a key without content is left out;
//! - every file is size-capped, the parent chain is depth-capped, cycles through `inherits`
//!   or `include` are detected, and the number of files read for one profile is capped;
//! - values are carried as the bytes the file had ([`serde_json::value::RawValue`]): a
//!   number is not reformatted, an object or a `null` is passed through untouched, and
//!   nothing in a profile is interpreted or executed;
//! - settings are put on top only from the request the core wrote, never from a profile, and
//!   only as items of the keymap compiled into this program ([`crate::keymap`]): the key must
//!   be a row of its role and pass the deny list, the text must have the row's form and lie in
//!   its range, and a row or value that was never measured is used only when the installed
//!   profile already holds it. The machine profile gets nothing but the nozzle inventory, whose
//!   values come from the table compiled into this program. The report lists what was written,
//!   with the installed and the new entries.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::keymap::{self, Absent, Keymap, Op, Role};
use crate::system::SystemProfiles;

/// File name of the report the helper writes into its output folder.
pub const REPORT_NAME: &str = "flatten-report.json";
/// Keys of a template that are about the template itself and are not copied.
const TEMPLATE_META_KEYS: [&str; 7] = [
    "name",
    "type",
    "from",
    "instantiation",
    "inherits",
    "include",
    "setting_id",
];
/// Names offered in a "not found" error.
const MAX_SUGGESTIONS: usize = 40;

/// Size and count limits for reading a profile tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlattenLimits {
    /// Largest profile file.
    pub max_file_bytes: u64,
    /// Longest `inherits` and `include` nesting, counted in profiles.
    pub max_depth: usize,
    /// Most templates one profile may include.
    pub max_includes: usize,
    /// Most files read to flatten one profile.
    pub max_files: usize,
    /// Most directory entries looked at in one role's folder.
    pub max_dir_entries: usize,
    /// How many folder levels below the role's folder are searched.
    pub max_dir_depth: usize,
    /// Most keys a flattened profile may have.
    pub max_keys: usize,
    /// Largest flattened profile that is written.
    pub max_output_bytes: usize,
}

impl Default for FlattenLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 1024 * 1024,
            max_depth: 16,
            max_includes: 16,
            max_files: 64,
            max_dir_entries: 50_000,
            max_dir_depth: 3,
            max_keys: 4096,
            max_output_bytes: 4 * 1024 * 1024,
        }
    }
}

/// Why a profile could not be flattened.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FlattenError {
    #[error(
        "`{0}` cannot name a profile: names have no path separator, no `..`, no leading dot and no control character"
    )]
    BadName(String),
    #[error("the profile folder {0} cannot be read: {1}")]
    Folder(String, String),
    #[error("the profile folder {0} has more than {1} entries")]
    FolderTooLarge(String, usize),
    #[error("no profile named `{name}` in {folder}{}", suggestion(.available))]
    NotFound {
        name: String,
        folder: String,
        available: Vec<String>,
    },
    #[error("`{name}` exists {count} times below {folder}; a profile name must be unique")]
    Ambiguous {
        name: String,
        folder: String,
        count: usize,
    },
    #[error(
        "`{0}` is a symbolic link or lies outside the profile folder; links are never followed"
    )]
    Escape(String),
    #[error("`{name}` is larger than {limit} bytes")]
    TooLarge { name: String, limit: u64 },
    #[error("`{name}` cannot be read: {reason}")]
    Unreadable { name: String, reason: String },
    #[error("`{name}` is not a profile: {reason}")]
    Malformed { name: String, reason: String },
    #[error("the profiles inherit in a circle: {}", .0.join(" -> "))]
    Cycle(Vec<String>),
    #[error("`{name}` is nested more than {limit} profiles deep")]
    TooDeep { name: String, limit: usize },
    #[error("flattening `{name}` needs more than {limit} files")]
    TooManyFiles { name: String, limit: usize },
    #[error("`{name}` includes more than {limit} templates")]
    TooManyIncludes { name: String, limit: usize },
    #[error("`{name}` flattens to more than {limit} keys or bytes")]
    OutputTooLarge { name: String, limit: usize },
    #[error("the setting `{0}` cannot be overridden, or not with that value")]
    BadOverride(String),
    /// An item of the keymap that cannot be applied; `code` is a mapping code of
    /// [`crate::keymap::MAPPING_CODES`].
    #[error("the setting `{key}` cannot be applied: {detail}")]
    Setting {
        code: &'static str,
        key: String,
        detail: String,
    },
    #[error(
        "`{name}` is not a {role} preset to slice with: {reason}. Only a profile with \
         `instantiation` true and the `type` of its role is used"
    )]
    NotAPreset {
        name: String,
        role: String,
        reason: String,
    },
    #[error(
        "the {role} profile `{name}` sets `{key}`. G-code and scripts are taken from the \
         machine profile only (a filament profile may bring its own start and end G-code), \
         and a post-processing script from no profile at all"
    )]
    ForbiddenKey {
        name: String,
        role: String,
        key: String,
    },
}

fn suggestion(available: &[String]) -> String {
    if available.is_empty() {
        String::new()
    } else {
        format!("; similar names there: {}", available.join(", "))
    }
}

impl FlattenError {
    /// Stable code of this error, for the report.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => "not_found",
            Self::BadName(_) => "bad_name",
            Self::Folder(..) | Self::FolderTooLarge(..) => "folder",
            Self::Ambiguous { .. } => "ambiguous",
            Self::Escape(_) => "escape",
            Self::TooLarge { .. } | Self::OutputTooLarge { .. } => "too_large",
            Self::Unreadable { .. } => "unreadable",
            Self::Malformed { .. } => "malformed",
            Self::Cycle(_) => "cycle",
            Self::TooDeep { .. } => "too_deep",
            Self::TooManyFiles { .. } | Self::TooManyIncludes { .. } => "too_many",
            Self::BadOverride(_) => "bad_override",
            Self::Setting { code, .. } => code,
            Self::NotAPreset { .. } => "not_a_preset",
            Self::ForbiddenKey { .. } => "forbidden_key",
        }
    }
}

/// Whether `name` may be looked up at all.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('.')
        && !name.contains("..")
        && !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
}

/// Text from a profile tree, made safe to put into a message.
fn shown(text: &str) -> String {
    crate::job::untrusted_text(text, 120)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    File(PathBuf),
    /// A symbolic link with this name was seen. It is never read.
    Link,
}

/// The profile files of one role's folder, by name (the file name without `.json`).
#[derive(Debug)]
struct Index {
    folder: PathBuf,
    /// Resolved path of `folder`; every file read must lie below it.
    real: PathBuf,
    names: BTreeMap<String, Vec<Entry>>,
}

impl Index {
    fn scan(folder: &Path, limits: &FlattenLimits) -> Result<Self, FlattenError> {
        let shown_folder = folder.display().to_string();
        // A role folder that is a link would make the link's target the folder that
        // profiles may not leave. `symlink_metadata` does not follow it.
        let meta = std::fs::symlink_metadata(folder)
            .map_err(|e| FlattenError::Folder(shown_folder.clone(), e.to_string()))?;
        if meta.file_type().is_symlink() {
            return Err(FlattenError::Escape(shown(&shown_folder)));
        }
        let real = std::fs::canonicalize(folder)
            .map_err(|e| FlattenError::Folder(shown_folder.clone(), e.to_string()))?;
        let mut names: BTreeMap<String, Vec<Entry>> = BTreeMap::new();
        let mut seen = 0usize;
        let mut pending = vec![(real.clone(), 0usize)];
        while let Some((dir, depth)) = pending.pop() {
            let entries = std::fs::read_dir(&dir)
                .map_err(|e| FlattenError::Folder(shown_folder.clone(), e.to_string()))?;
            for entry in entries {
                let entry =
                    entry.map_err(|e| FlattenError::Folder(shown_folder.clone(), e.to_string()))?;
                seen += 1;
                if seen > limits.max_dir_entries {
                    return Err(FlattenError::FolderTooLarge(
                        shown_folder,
                        limits.max_dir_entries,
                    ));
                }
                // `file_type` does not follow a link.
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                let path = entry.path();
                let stem = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.strip_suffix(".json"))
                    .map(str::to_owned);
                if kind.is_symlink() {
                    if let Some(stem) = stem {
                        names.entry(stem).or_default().push(Entry::Link);
                    }
                } else if kind.is_dir() {
                    if depth < limits.max_dir_depth {
                        pending.push((path, depth + 1));
                    }
                } else if kind.is_file()
                    && let Some(stem) = stem
                {
                    names.entry(stem).or_default().push(Entry::File(path));
                }
            }
        }
        Ok(Self {
            folder: folder.to_path_buf(),
            real,
            names,
        })
    }

    fn path(&self, name: &str) -> Result<&Path, FlattenError> {
        if !valid_name(name) {
            return Err(FlattenError::BadName(shown(name)));
        }
        let folder = || self.folder.display().to_string();
        match self.names.get(name).map(Vec::as_slice) {
            None | Some([]) => Err(FlattenError::NotFound {
                name: shown(name),
                folder: folder(),
                available: self.similar(name),
            }),
            Some([Entry::File(path)]) => Ok(path),
            Some(entries) if entries.contains(&Entry::Link) => {
                Err(FlattenError::Escape(shown(name)))
            }
            Some(entries) => {
                // Several files of one name: the one directly in the role's folder wins.
                let direct: Vec<&Path> = entries
                    .iter()
                    .filter_map(|entry| match entry {
                        Entry::File(path) if path.parent() == Some(self.real.as_path()) => {
                            Some(path.as_path())
                        }
                        _ => None,
                    })
                    .collect();
                match direct.as_slice() {
                    [path] => Ok(path),
                    _ => Err(FlattenError::Ambiguous {
                        name: shown(name),
                        folder: folder(),
                        count: entries.len(),
                    }),
                }
            }
        }
    }

    /// Names that share the text after the last `@` with `name` (the printer a profile is
    /// for), or else its first word; at most [`MAX_SUGGESTIONS`].
    fn similar(&self, name: &str) -> Vec<String> {
        let tag = name.rsplit_once('@').map(|(_, tag)| format!("@{tag}"));
        let first = name.split(' ').next().unwrap_or("").to_owned();
        let by = |test: &dyn Fn(&str) -> bool| -> Vec<String> {
            self.names
                .keys()
                .filter(|known| test(known))
                .take(MAX_SUGGESTIONS)
                .map(|known| shown(known))
                .collect()
        };
        let tagged = tag
            .map(|tag| by(&|known| known.ends_with(&tag) || known.contains(&format!("{tag} "))))
            .unwrap_or_default();
        if !tagged.is_empty() {
            return tagged;
        }
        if first.is_empty() {
            Vec::new()
        } else {
            by(&|known| known.starts_with(&first))
        }
    }
}

type Values = BTreeMap<String, Box<RawValue>>;

/// One flattened profile.
#[derive(Debug)]
pub struct Flattened {
    /// The name that was asked for.
    pub name: String,
    /// The `inherits` chain, root first, the profile itself last.
    pub chain: Vec<String>,
    /// Every template that was included, in the order it was applied.
    pub includes: Vec<String>,
    values: Values,
}

impl Flattened {
    /// Number of settings.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// The raw JSON text of one setting.
    pub fn raw(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(|value| value.get())
    }

    /// One setting, parsed. `None` when it is absent.
    pub fn value(&self, key: &str) -> Option<serde_json::Value> {
        self.raw(key).and_then(|raw| serde_json::from_str(raw).ok())
    }

    /// Puts `value` under `key`. Only [`apply_items`] calls this, with a value
    /// [`keymap::resolve`] made for a key the keymap allows.
    fn put(&mut self, key: &str, value: &serde_json::Value) -> Result<(), FlattenError> {
        let raw = serde_json::to_string(value)
            .ok()
            .and_then(|json| RawValue::from_string(json).ok())
            .ok_or_else(|| FlattenError::BadOverride(shown(key)))?;
        self.values.insert(key.to_owned(), raw);
        Ok(())
    }

    /// Whether the profile lists `machine` among the printers it is for: `Some(true)`,
    /// `Some(false)`, or `None` when it does not say.
    pub fn compatible_with(&self, machine: &str) -> Option<bool> {
        match self.value("compatible_printers")? {
            serde_json::Value::Array(list) if !list.is_empty() => {
                Some(list.iter().any(|entry| entry.as_str() == Some(machine)))
            }
            _ => None,
        }
    }

    /// Checks that the profile is a preset of `role` and takes out the keys a profile of
    /// that role must not carry ([`forbidden_key`]). One with content is an error; the
    /// ones without are returned, in order.
    pub fn restrict_to_role(&mut self, role: &str) -> Result<Vec<String>, FlattenError> {
        let not_a_preset = |reason: String| FlattenError::NotAPreset {
            name: shown(&self.name),
            role: role.to_owned(),
            reason,
        };
        match self.value("type") {
            Some(serde_json::Value::String(kind)) if kind == role => {}
            Some(other) => {
                return Err(not_a_preset(format!(
                    "its `type` is {}",
                    shown(&other.to_string())
                )));
            }
            None => return Err(not_a_preset("it states no `type`".into())),
        }
        match self.value("instantiation") {
            Some(serde_json::Value::Bool(true)) => {}
            Some(serde_json::Value::String(text)) if text == "true" => {}
            Some(other) => {
                return Err(not_a_preset(format!(
                    "its `instantiation` is {}, as for a base that other profiles build on",
                    shown(&other.to_string())
                )));
            }
            None => return Err(not_a_preset("it states no `instantiation`".into())),
        }
        let forbidden: Vec<String> = self
            .values
            .keys()
            .filter(|key| forbidden_key(role, key))
            .cloned()
            .collect();
        for key in &forbidden {
            if !self.raw(key).is_some_and(without_content) {
                return Err(FlattenError::ForbiddenKey {
                    name: shown(&self.name),
                    role: role.to_owned(),
                    key: shown(key),
                });
            }
        }
        for key in &forbidden {
            self.values.remove(key);
        }
        Ok(forbidden.iter().map(|key| shown(key)).collect())
    }

    /// The flattened profile as JSON.
    pub fn to_json(&self, limits: &FlattenLimits) -> Result<Vec<u8>, FlattenError> {
        let too_large = || FlattenError::OutputTooLarge {
            name: shown(&self.name),
            limit: limits.max_output_bytes,
        };
        let bytes = serde_json::to_vec(&self.values).map_err(|_| too_large())?;
        if bytes.len() > limits.max_output_bytes {
            return Err(too_large());
        }
        Ok(bytes)
    }
}

/// What the helper did with the items of one profile.
#[derive(Debug, Default)]
struct Applied {
    applied: Vec<AppliedReport>,
    unchanged: Vec<String>,
}

/// Applies the keymap items of a request item, and for the machine profile the nozzle
/// inventory, to a flattened profile. Each item is checked against the keymap compiled into
/// this program, whatever the request says: the key must be a row of its role and pass the
/// deny list, the operation and the absent policy must be the row's, and the text must have
/// the row's form and lie in its range. The values of the inventory come from the table
/// compiled into this program, never from the request.
fn apply_items(
    profile: &mut Flattened,
    item: &RequestItem,
    machine: &str,
) -> Result<Applied, FlattenError> {
    let setting = |code: &'static str, key: &str, detail: &str| FlattenError::Setting {
        code,
        key: shown(key),
        detail: detail.to_owned(),
    };
    let keymap = Keymap::builtin().map_err(|_| {
        setting(
            "setting_key_denied",
            "",
            "the keymap of this program is invalid",
        )
    })?;
    let table = SystemProfiles::builtin().map_err(|_| {
        setting(
            "setting_key_denied",
            "",
            "the profile table of this program is invalid",
        )
    })?;
    let plates: Vec<&str> = table
        .bed_types
        .iter()
        .map(|bed| bed.slicer_value.as_str())
        .collect();
    let role = match item.role.as_str() {
        "machine" => Role::Machine,
        "process" => Role::Process,
        _ => Role::Filament,
    };
    let mut done = Applied::default();
    let mut seen = std::collections::BTreeSet::new();
    for asked in &item.items {
        let key = asked.key.as_str();
        if !seen.insert(key) {
            return Err(setting(
                "setting_key_denied",
                key,
                "the setting is asked for twice",
            ));
        }
        if !keymap::key_allowed(role, key) {
            return Err(setting(
                "setting_key_denied",
                key,
                "the setting is on the deny list or not a key",
            ));
        }
        let Some(row) = keymap.row(role, key).filter(|row| row.enabled) else {
            return Err(setting(
                "setting_key_denied",
                key,
                "the setting is not a row of the keymap for this profile",
            ));
        };
        if row.op != asked.op || row.absent != asked.absent || !row.text_valid(&asked.text, &plates)
        {
            return Err(setting(
                "setting_out_of_range",
                key,
                "the value does not have the form or the range of the setting",
            ));
        }
        match keymap::resolve(row, &asked.text, profile.value(key).as_ref())
            .map_err(|error| setting(error.code, key, &error.detail))?
        {
            keymap::Resolution::Write {
                value,
                old,
                new,
                changed,
            } => {
                profile.put(key, &value)?;
                done.applied.push(AppliedReport {
                    key: key.to_owned(),
                    old,
                    new,
                    changed,
                });
            }
            keymap::Resolution::Unchanged { .. } => done.unchanged.push(key.to_owned()),
        }
    }
    if item.inventory {
        let inventory = table
            .printers
            .iter()
            .find(|printer| printer.machine == machine)
            .and_then(|printer| printer.nozzle_inventory.as_ref())
            .filter(|_| role == Role::Machine)
            .ok_or_else(|| {
                setting(
                    "setting_key_denied",
                    "",
                    "the nozzle inventory belongs to the machine profile of a printer that has one",
                )
            })?;
        for (key, values) in &inventory.values {
            if !keymap::key_allowed(Role::Machine, key)
                || !values
                    .iter()
                    .all(|value| keymap::valid_inventory_value(value))
            {
                return Err(setting(
                    "setting_key_denied",
                    key,
                    "not an inventory setting",
                ));
            }
            let old = match profile.value(key) {
                None => None,
                Some(serde_json::Value::Array(list))
                    if list.len() <= keymap::MAX_ENTRIES
                        && list.iter().all(|entry| {
                            entry
                                .as_str()
                                .is_some_and(|text| text.chars().count() <= keymap::MAX_ENTRY_CHARS)
                        }) =>
                {
                    Some(
                        list.iter()
                            .filter_map(|entry| entry.as_str().map(str::to_owned))
                            .collect::<Vec<_>>(),
                    )
                }
                Some(_) => {
                    return Err(setting(
                        "setting_shape_mismatch",
                        key,
                        "the installed value is not a short list of texts",
                    ));
                }
            };
            let value = serde_json::Value::Array(
                values
                    .iter()
                    .cloned()
                    .map(serde_json::Value::String)
                    .collect(),
            );
            profile.put(key, &value)?;
            done.applied.push(AppliedReport {
                key: key.clone(),
                changed: old.as_ref() != Some(values),
                old,
                new: values.clone(),
            });
        }
    }
    Ok(done)
}

/// Whether a profile of `role` must not carry `key`.
///
/// What the printer is told around and between the layers is the machine profile's
/// business. The slicer loads the process settings after the machine's and the filament
/// settings after both, so a routine in one of them could replace the machine's. A
/// filament may bring its own start and end G-code, as the vendor's filaments do. A
/// post-processing script is a program the slicer would run; Hotend never wants one.
pub fn forbidden_key(role: &str, key: &str) -> bool {
    if key == "post_process" {
        return true;
    }
    match role {
        "machine" => false,
        "filament" => {
            key.ends_with("_gcode") && !matches!(key, "filament_start_gcode" | "filament_end_gcode")
        }
        _ => key.ends_with("_gcode"),
    }
}

/// Whether a raw JSON value carries nothing: `null`, an empty or blank string, or a list
/// of nothing but those.
fn without_content(raw: &str) -> bool {
    fn empty(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::Null => true,
            serde_json::Value::String(text) => text.trim().is_empty(),
            serde_json::Value::Array(list) => list.iter().all(empty),
            _ => false,
        }
    }
    serde_json::from_str::<serde_json::Value>(raw).is_ok_and(|value| empty(&value))
}

struct Reader<'a> {
    index: &'a Index,
    limits: &'a FlattenLimits,
    wanted: &'a str,
    files: usize,
    includes: Vec<String>,
}

impl Reader<'_> {
    fn load(&mut self, name: &str) -> Result<Values, FlattenError> {
        let path = self.index.path(name)?;
        self.files += 1;
        if self.files > self.limits.max_files {
            return Err(FlattenError::TooManyFiles {
                name: shown(self.wanted),
                limit: self.limits.max_files,
            });
        }
        let unreadable = |reason: String| FlattenError::Unreadable {
            name: shown(name),
            reason,
        };
        // The folder may change between the scan and now: check again that the file is a
        // plain file that resolves to a place inside the folder, and never follow a link.
        let real = std::fs::canonicalize(path).map_err(|e| unreadable(e.to_string()))?;
        if !real.starts_with(&self.index.real) {
            return Err(FlattenError::Escape(shown(name)));
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(path).map_err(|e| {
            if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
                FlattenError::Escape(shown(name))
            } else {
                unreadable(e.to_string())
            }
        })?;
        let meta = file.metadata().map_err(|e| unreadable(e.to_string()))?;
        if !meta.is_file() {
            return Err(unreadable("it is not a regular file".into()));
        }
        let too_large = || FlattenError::TooLarge {
            name: shown(name),
            limit: self.limits.max_file_bytes,
        };
        if meta.len() > self.limits.max_file_bytes {
            return Err(too_large());
        }
        let mut bytes = Vec::new();
        file.take(self.limits.max_file_bytes + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| unreadable(e.to_string()))?;
        if bytes.len() as u64 > self.limits.max_file_bytes {
            return Err(too_large());
        }
        // Files written on Windows may start with a byte order mark.
        let text = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
        serde_json::from_slice::<Values>(text).map_err(|e| FlattenError::Malformed {
            name: shown(name),
            reason: format!("it is not a JSON object ({})", shown(&e.to_string())),
        })
    }

    /// The names in `key`: a string, a list of strings, or nothing.
    fn names(&self, owner: &str, values: &Values, key: &str) -> Result<Vec<String>, FlattenError> {
        let malformed = || FlattenError::Malformed {
            name: shown(owner),
            reason: format!("`{key}` is neither a name nor a list of names"),
        };
        let Some(raw) = values.get(key) else {
            return Ok(Vec::new());
        };
        match serde_json::from_str::<serde_json::Value>(raw.get()).map_err(|_| malformed())? {
            serde_json::Value::Null => Ok(Vec::new()),
            serde_json::Value::String(name) if name.is_empty() => Ok(Vec::new()),
            serde_json::Value::String(name) => Ok(vec![name]),
            serde_json::Value::Array(list) => list
                .into_iter()
                .map(|item| match item {
                    serde_json::Value::String(name) if !name.is_empty() => Ok(name),
                    _ => Err(malformed()),
                })
                .collect(),
            _ => Err(malformed()),
        }
    }

    /// Flattens `name`. `stack` holds the profiles being resolved above it, for the cycle
    /// check. Returns the values and the `inherits` chain, root first.
    fn resolve(
        &mut self,
        name: &str,
        stack: &mut Vec<String>,
    ) -> Result<(Values, Vec<String>), FlattenError> {
        if stack.iter().any(|above| above == name) {
            let mut circle: Vec<String> = stack.iter().map(|name| shown(name)).collect();
            circle.push(shown(name));
            return Err(FlattenError::Cycle(circle));
        }
        if stack.len() >= self.limits.max_depth {
            return Err(FlattenError::TooDeep {
                name: shown(self.wanted),
                limit: self.limits.max_depth,
            });
        }
        let own = self.load(name)?;
        let parents = self.names(name, &own, "inherits")?;
        let includes = self.names(name, &own, "include")?;
        if parents.len() > 1 {
            return Err(FlattenError::Malformed {
                name: shown(name),
                reason: "`inherits` names more than one profile".into(),
            });
        }
        if includes.len() > self.limits.max_includes {
            return Err(FlattenError::TooManyIncludes {
                name: shown(name),
                limit: self.limits.max_includes,
            });
        }
        stack.push(name.to_owned());
        let (mut values, mut chain) = match parents.first() {
            Some(parent) => self.resolve(parent, stack)?,
            None => (Values::new(), Vec::new()),
        };
        for include in &includes {
            let (template, _) = self.resolve(include, stack)?;
            self.includes.push(shown(include));
            for (key, value) in template {
                if !TEMPLATE_META_KEYS.contains(&key.as_str()) {
                    values.insert(key, value);
                }
            }
        }
        stack.pop();
        for (key, value) in own {
            if key != "inherits" && key != "include" {
                values.insert(key, value);
            }
        }
        if values.len() > self.limits.max_keys {
            return Err(FlattenError::OutputTooLarge {
                name: shown(self.wanted),
                limit: self.limits.max_keys,
            });
        }
        chain.push(shown(name));
        Ok((values, chain))
    }
}

/// Flattens the profile `name` of the folder `role_dir` (for example
/// `<install>/resources/profiles/BBL/filament`).
pub fn flatten(
    role_dir: &Path,
    name: &str,
    limits: &FlattenLimits,
) -> Result<Flattened, FlattenError> {
    if !valid_name(name) {
        return Err(FlattenError::BadName(shown(name)));
    }
    let index = Index::scan(role_dir, limits)?;
    let mut reader = Reader {
        index: &index,
        limits,
        wanted: name,
        files: 0,
        includes: Vec::new(),
    };
    let (values, chain) = reader.resolve(name, &mut Vec::new())?;
    Ok(Flattened {
        name: name.to_owned(),
        chain,
        includes: reader.includes,
        values,
    })
}

/// What the core asks the helper to do. The core writes it; the helper trusts it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// The vendor's profile folder: it holds `machine`, `process` and `filament`.
    pub root: PathBuf,
    /// The machine profile's name, for the compatibility note of the other profiles.
    pub machine: String,
    /// The defaults when a request written by hand leaves them out.
    #[serde(default)]
    pub limits: FlattenLimits,
    pub items: Vec<RequestItem>,
}

/// One profile to flatten.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestItem {
    /// `machine`, `process` or `filament`.
    pub role: String,
    pub name: String,
    /// File name of the flattened profile in the output folder.
    pub output: String,
    /// Settings to put on top, each a row of the keymap ([`crate::keymap`]).
    #[serde(default)]
    pub items: Vec<SettingItem>,
    /// Write the printer's nozzle inventory (machine profile only; values from the table
    /// compiled into the helper).
    #[serde(default)]
    pub inventory: bool,
}

/// One setting to put on top of a flattened profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingItem {
    pub key: String,
    pub op: Op,
    pub text: String,
    pub absent: Absent,
}

/// What the helper wrote for one key: the installed entries (`None`: the key was absent) and
/// the new ones. Read back as hostile input: only keys of the keymap or of the inventory, at
/// most [`keymap::MAX_ENTRIES`] entries of [`keymap::MAX_ENTRY_CHARS`] characters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppliedReport {
    pub key: String,
    pub old: Option<Vec<String>>,
    pub new: Vec<String>,
    /// Whether the new entries differ from the installed ones.
    pub changed: bool,
}

/// What the helper did. The core reads it as hostile input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub ok: bool,
    #[serde(default)]
    pub items: Vec<ReportItem>,
    #[serde(default)]
    pub error: Option<ReportError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportItem {
    pub role: String,
    pub name: String,
    pub output: String,
    pub chain: Vec<String>,
    pub includes: Vec<String>,
    pub keys: usize,
    /// `yes`, `no` or `unstated`: whether the profile lists the machine as compatible.
    pub compatible: String,
    /// Keys without content that were left out because a profile of this role must not
    /// carry them ([`forbidden_key`]).
    pub dropped: Vec<String>,
    /// What was written on top, in the order of the request.
    #[serde(default)]
    pub applied: Vec<AppliedReport>,
    /// Keys of rows that are not measured whose asked value the profile already held, so
    /// nothing was written.
    #[serde(default)]
    pub unchanged: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportError {
    pub code: String,
    pub role: String,
    pub name: String,
    pub message: String,
    /// The setting an item failure is about.
    #[serde(default)]
    pub key: String,
}

fn valid_role(role: &str) -> bool {
    matches!(role, "machine" | "process" | "filament")
}

fn valid_output(name: &str) -> bool {
    name.len() <= 40
        && name.strip_suffix(".json").is_some_and(|stem| {
            !stem.is_empty()
                && stem
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        })
}

/// Flattens every item of `request` and writes the files into `out_dir`. Nothing is
/// written for a request that fails.
pub fn run_request(request: &Request, out_dir: &Path) -> Result<Vec<ReportItem>, ReportError> {
    let fail = |item: &RequestItem, code: &str, message: String| ReportError {
        code: code.to_owned(),
        role: shown(&item.role),
        name: shown(&item.name),
        message,
        key: String::new(),
    };
    let mut written = Vec::new();
    let mut items = Vec::new();
    // A vendor folder that is a link would make the link's target the tree that is read.
    let root_is_link =
        std::fs::symlink_metadata(&request.root).is_ok_and(|meta| meta.file_type().is_symlink());
    for item in &request.items {
        if !valid_role(&item.role) || !valid_output(&item.output) {
            return Err(fail(item, "bad_request", "the request is malformed".into()));
        }
        if root_is_link {
            let error = FlattenError::Escape(shown(&request.root.display().to_string()));
            return Err(fail(item, error.code(), error.to_string()));
        }
        let flatten_item =
            || -> Result<(Flattened, Vec<String>, Applied, Vec<u8>), FlattenError> {
                let mut profile =
                    flatten(&request.root.join(&item.role), &item.name, &request.limits)?;
                let dropped = profile.restrict_to_role(&item.role)?;
                let applied = apply_items(&mut profile, item, &request.machine)?;
                let json = profile.to_json(&request.limits)?;
                Ok((profile, dropped, applied, json))
            };
        let (profile, dropped, applied, json) = flatten_item().map_err(|error| {
            let key = match &error {
                FlattenError::Setting { key, .. } => key.clone(),
                _ => String::new(),
            };
            ReportError {
                key,
                ..fail(item, error.code(), error.to_string())
            }
        })?;
        let compatible = if item.role == "machine" {
            "unstated"
        } else {
            match profile.compatible_with(&request.machine) {
                Some(true) => "yes",
                Some(false) => "no",
                None => "unstated",
            }
        };
        items.push(ReportItem {
            role: item.role.clone(),
            name: shown(&item.name),
            output: item.output.clone(),
            chain: profile.chain.clone(),
            includes: profile.includes.clone(),
            keys: profile.len(),
            compatible: compatible.to_owned(),
            dropped,
            applied: applied.applied,
            unchanged: applied.unchanged,
        });
        written.push((item, json));
    }
    for (item, json) in written {
        let path = out_dir.join(&item.output);
        std::fs::write(&path, json).map_err(|e| {
            fail(
                item,
                "write",
                format!("cannot write the flattened profile: {e}"),
            )
        })?;
    }
    Ok(items)
}

/// The `hotend-profile-flatten` program: `hotend-profile-flatten <request.json>
/// <output folder>`. It reads the request, flattens the profiles, writes them and a report
/// ([`REPORT_NAME`]) into the output folder and prints nothing. Exit codes: 0 done, 3 a
/// profile could not be flattened (the report says why), 2 it was called wrongly.
pub fn helper_main(args: &[OsString]) -> i32 {
    let [request_path, out_dir] = args else {
        return 2;
    };
    let out_dir = Path::new(out_dir);
    let request: Request = match std::fs::read(request_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    {
        Some(request) => request,
        None => return 2,
    };
    let (code, report) = match run_request(&request, out_dir) {
        Ok(items) => (
            0,
            Report {
                ok: true,
                items,
                error: None,
            },
        ),
        Err(error) => (
            3,
            Report {
                ok: false,
                items: Vec::new(),
                error: Some(error),
            },
        ),
    };
    let written = serde_json::to_vec(&report)
        .ok()
        .and_then(|json| std::fs::write(out_dir.join(REPORT_NAME), json).ok());
    if written.is_some() { code } else { 2 }
}

/// File name of the helper's executable.
pub fn helper_name() -> String {
    format!("hotend-profile-flatten{}", std::env::consts::EXE_SUFFIX)
}

/// Where the helper is when it was built with the running program: next to the current
/// executable, or one folder up when running from cargo's `deps` folder.
pub fn locate_helper() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let name = helper_name();
    let beside = dir.join(&name);
    if beside.is_file() {
        return Some(beside);
    }
    let above = dir
        .parent()
        .filter(|_| dir.file_name().is_some_and(|n| n == "deps"))?;
    Some(above.join(name)).filter(|path| path.is_file())
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// Writes `<root>/<role>/<name>.json`.
    pub(crate) fn put(root: &Path, role: &str, name: &str, json: &str) {
        let dir = root.join(role);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.json")), json).unwrap();
    }

    /// A small tree in the layout of an installed vendor folder. Every name and value in
    /// it was made up for these tests.
    pub(crate) fn tree() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        put(
            root,
            "filament",
            "base_common",
            r#"{"type": "filament", "name": "base_common", "from": "system", "instantiation": "false",
                "filament_diameter": ["1.75"], "filament_density": ["0"], "plate_a_temp": ["40"]}"#,
        );
        put(
            root,
            "filament",
            "base_pla",
            r#"{"type": "filament", "name": "base_pla", "inherits": "base_common",
                "filament_type": ["PLA"], "filament_density": ["1.25"], "odd": {"kept": [1, 2.50, null]}}"#,
        );
        put(
            root,
            "filament",
            "Test PLA @Test Printer 0.4 nozzle",
            r#"{"type": "filament", "name": "Test PLA @Test Printer 0.4 nozzle", "inherits": "base_pla",
                "from": "system", "instantiation": "true", "setting_id": "T1",
                "compatible_printers": ["Test Printer 0.4 nozzle"], "plate_a_temp": ["55"],
                "number": 0.20, "flag": true}"#,
        );
        put(
            root,
            "machine",
            "machine_common",
            r#"{"type": "machine", "name": "machine_common", "printable_height": "200",
                "nozzle_diameter": ["0.4"], "routine": "from the root"}"#,
        );
        put(
            root,
            "machine",
            "Test Printer template routine",
            r#"{"type": "machine", "name": "Test Printer template routine", "from": "system",
                "instantiation": "false", "setting_id": "X", "routine": "from the template"}"#,
        );
        put(
            root,
            "machine",
            "Test Printer 0.4 nozzle",
            r#"{"type": "machine", "name": "Test Printer 0.4 nozzle", "inherits": "machine_common",
                "instantiation": "true",
                "include": ["Test Printer template routine"], "nozzle_diameter": ["0.4", "0.4"],
                "printable_height": "250"}"#,
        );
        tmp
    }

    fn limits() -> FlattenLimits {
        FlattenLimits::default()
    }

    #[test]
    fn a_child_overrides_its_parents_and_templates_fill_in_between() {
        let tmp = tree();
        let filament = flatten(
            &tmp.path().join("filament"),
            "Test PLA @Test Printer 0.4 nozzle",
            &limits(),
        )
        .unwrap();
        assert_eq!(
            filament.chain,
            [
                "base_common",
                "base_pla",
                "Test PLA @Test Printer 0.4 nozzle"
            ]
        );
        // The middle profile's density wins over the root's 0, the leaf's temperature over
        // the root's, and what only the root has is still there.
        assert_eq!(filament.raw("filament_density"), Some(r#"["1.25"]"#));
        assert_eq!(filament.raw("plate_a_temp"), Some(r#"["55"]"#));
        assert_eq!(filament.raw("filament_diameter"), Some(r#"["1.75"]"#));
        assert_eq!(
            filament.raw("name"),
            Some(r#""Test PLA @Test Printer 0.4 nozzle""#)
        );
        assert_eq!(filament.raw("instantiation"), Some(r#""true""#));
        // The links themselves are gone: the result is a full config.
        assert_eq!(filament.raw("inherits"), None);
        assert_eq!(filament.raw("include"), None);
        // Values are carried byte for byte, whatever their type.
        assert_eq!(filament.raw("number"), Some("0.20"));
        assert_eq!(filament.raw("flag"), Some("true"));
        assert_eq!(filament.raw("odd"), Some(r#"{"kept": [1, 2.50, null]}"#));
        assert_eq!(
            filament.compatible_with("Test Printer 0.4 nozzle"),
            Some(true)
        );
        assert_eq!(filament.compatible_with("Other"), Some(false));

        let machine = flatten(
            &tmp.path().join("machine"),
            "Test Printer 0.4 nozzle",
            &limits(),
        )
        .unwrap();
        assert_eq!(machine.chain, ["machine_common", "Test Printer 0.4 nozzle"]);
        assert_eq!(machine.includes, ["Test Printer template routine"]);
        // The template overrides the parent; the profile's own keys override both.
        assert_eq!(machine.raw("routine"), Some(r#""from the template""#));
        assert_eq!(machine.raw("printable_height"), Some(r#""250""#));
        assert_eq!(machine.raw("nozzle_diameter"), Some(r#"["0.4", "0.4"]"#));
        // What a template says about itself is not copied.
        assert_eq!(machine.raw("setting_id"), None);
        assert_eq!(machine.raw("name"), Some(r#""Test Printer 0.4 nozzle""#));
        assert_eq!(machine.compatible_with("anything"), None);

        // The output is one JSON object that parses again.
        let json = machine.to_json(&limits()).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(parsed["routine"], "from the template");
        assert!(parsed.get("inherits").is_none());
    }

    fn setting(key: &str, op: Op, text: &str, absent: Absent) -> SettingItem {
        SettingItem {
            key: key.into(),
            op,
            text: text.into(),
            absent,
        }
    }

    /// A process and a filament profile with the measured key names and made-up values.
    fn settings_tree() -> tempfile::TempDir {
        let tmp = tree();
        put(
            tmp.path(),
            "process",
            "0.20mm Test @Test Printer",
            r#"{"type": "process", "instantiation": "true", "wall_loops": "2",
                "initial_layer_speed": ["50", "50", "50", "50", "50", "50"],
                "outer_wall_speed": ["200", "150", "150", "50", "50", "50"],
                "sparse_infill_density": "15%", "brim_object_gap": "0.1"}"#,
        );
        put(
            tmp.path(),
            "filament",
            "Test PLA @Test Printer 0.4 nozzle",
            r#"{"type": "filament", "instantiation": "true", "additional_cooling_fan_speed": ["70"],
                "textured_plate_temp": ["55"]}"#,
        );
        tmp
    }

    fn request(root: &Path, process: Vec<SettingItem>, filament: Vec<SettingItem>) -> Request {
        let item = |role: &str, name: &str, output: &str, items: Vec<SettingItem>| RequestItem {
            role: role.into(),
            name: name.into(),
            output: output.into(),
            items,
            inventory: false,
        };
        Request {
            root: root.to_path_buf(),
            machine: "Test Printer 0.4 nozzle".into(),
            limits: limits(),
            items: vec![
                item(
                    "machine",
                    "Test Printer 0.4 nozzle",
                    "machine.json",
                    Vec::new(),
                ),
                item(
                    "process",
                    "0.20mm Test @Test Printer",
                    "process.json",
                    process,
                ),
                item(
                    "filament",
                    "Test PLA @Test Printer 0.4 nozzle",
                    "filament_1.json",
                    filament,
                ),
            ],
        }
    }

    fn read(out: &Path, name: &str) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(out.join(name)).unwrap()).unwrap()
    }

    #[test]
    fn items_keep_the_shape_of_the_setting_and_are_reported() {
        let tmp = settings_tree();
        let out = tempfile::tempdir().unwrap();
        let process = vec![
            setting("curr_bed_type", Op::Set, "Textured PEI Plate", Absent::Add),
            setting("wall_loops", Op::Set, "3", Absent::Refuse),
            setting("initial_layer_speed", Op::Cap, "20", Absent::Refuse),
            setting("brim_type", Op::Set, "outer_only", Absent::Add),
            setting("brim_object_gap", Op::Set, "0", Absent::Refuse),
            // Not measured, and what the profile holds: nothing is written.
            setting("sparse_infill_density", Op::Set, "15%", Absent::Refuse),
            setting("outer_wall_speed", Op::Cap, "200", Absent::Refuse),
        ];
        let filament = vec![
            setting(
                "additional_cooling_fan_speed",
                Op::Cap,
                "30",
                Absent::Refuse,
            ),
            setting("textured_plate_temp", Op::Set, "60", Absent::Refuse),
            setting("filament_colour", Op::Set, "#898989", Absent::Add),
        ];
        let items = run_request(&request(tmp.path(), process, filament), out.path()).unwrap();
        let written = read(out.path(), "process.json");
        assert_eq!(written["curr_bed_type"], "Textured PEI Plate");
        assert_eq!(written["wall_loops"], "3");
        assert_eq!(
            written["initial_layer_speed"],
            serde_json::json!(["20", "20", "20", "20", "20", "20"])
        );
        assert_eq!(written["brim_type"], "outer_only");
        assert_eq!(written["sparse_infill_density"], "15%");
        let filament = read(out.path(), "filament_1.json");
        assert_eq!(
            filament["additional_cooling_fan_speed"],
            serde_json::json!(["30"])
        );
        assert_eq!(filament["textured_plate_temp"], serde_json::json!(["60"]));
        assert_eq!(filament["filament_colour"], serde_json::json!(["#898989"]));
        // The report: what was written with the installed and the new entries, and what was
        // left as it was.
        let process = &items[1];
        let keys: Vec<&str> = process.applied.iter().map(|a| a.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "curr_bed_type",
                "wall_loops",
                "initial_layer_speed",
                "brim_type",
                "brim_object_gap"
            ]
        );
        assert_eq!(process.applied[0].old, None);
        assert_eq!(process.applied[1].old, Some(vec!["2".to_owned()]));
        assert!(process.applied.iter().all(|a| a.changed));
        assert_eq!(
            process.unchanged,
            ["sparse_infill_density", "outer_wall_speed"]
        );
        assert!(items[0].applied.is_empty());
        assert_eq!(items[2].applied.len(), 3);
    }

    #[test]
    fn a_forged_request_cannot_write_anything_else() {
        let tmp = settings_tree();
        for (items, code) in [
            // Denied keys, whatever the operation.
            (
                vec![setting("machine_start_gcode", Op::Set, "G1", Absent::Add)],
                "setting_key_denied",
            ),
            (
                vec![setting("post_process", Op::Set, "x", Absent::Add)],
                "setting_key_denied",
            ),
            (
                vec![setting("inherits", Op::Set, "x", Absent::Add)],
                "setting_key_denied",
            ),
            (
                vec![setting("compatible_printers", Op::Set, "x", Absent::Add)],
                "setting_key_denied",
            ),
            (
                vec![setting("print_host", Op::Set, "x", Absent::Add)],
                "setting_key_denied",
            ),
            (
                vec![setting("filename_format", Op::Set, "x", Absent::Add)],
                "setting_key_denied",
            ),
            (
                vec![setting("printable_area", Op::Set, "x", Absent::Add)],
                "setting_key_denied",
            ),
            (
                vec![setting("machine_max_speed_x", Op::Set, "9", Absent::Add)],
                "setting_key_denied",
            ),
            (
                vec![setting("print_settings_id", Op::Set, "x", Absent::Add)],
                "setting_key_denied",
            ),
            // A key of the profile that is not a row of the keymap.
            (
                vec![setting("layer_height", Op::Set, "0.2", Absent::Refuse)],
                "setting_key_denied",
            ),
            // A filament key in the process profile.
            (
                vec![setting(
                    "textured_plate_temp",
                    Op::Set,
                    "60",
                    Absent::Refuse,
                )],
                "setting_key_denied",
            ),
            // The row's operation, absent policy, form and range, checked again.
            (
                vec![setting("wall_loops", Op::Cap, "3", Absent::Refuse)],
                "setting_out_of_range",
            ),
            (
                vec![setting("wall_loops", Op::Set, "3", Absent::Add)],
                "setting_out_of_range",
            ),
            (
                vec![setting("wall_loops", Op::Set, "9", Absent::Refuse)],
                "setting_out_of_range",
            ),
            (
                vec![setting(
                    "wall_loops",
                    Op::Set,
                    "3\nM104 S300",
                    Absent::Refuse,
                )],
                "setting_out_of_range",
            ),
            (
                vec![setting(
                    "curr_bed_type",
                    Op::Set,
                    "Glass Plate",
                    Absent::Add,
                )],
                "setting_out_of_range",
            ),
            // A value never measured that differs from the profile.
            (
                vec![setting(
                    "sparse_infill_density",
                    Op::Set,
                    "20%",
                    Absent::Refuse,
                )],
                "setting_unmeasured",
            ),
            // A key the profile lacks is not invented.
            (
                vec![setting("skirt_loops", Op::Set, "0", Absent::Refuse)],
                "setting_key_missing",
            ),
            // The same key twice.
            (
                vec![
                    setting("wall_loops", Op::Set, "3", Absent::Refuse),
                    setting("wall_loops", Op::Set, "4", Absent::Refuse),
                ],
                "setting_key_denied",
            ),
        ] {
            let out = tempfile::tempdir().unwrap();
            let error = run_request(&request(tmp.path(), items.clone(), Vec::new()), out.path())
                .unwrap_err();
            assert_eq!(error.code, code, "{items:?}");
            assert_eq!(error.role, "process");
            // Nothing is written for a request that fails.
            assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 0);
        }
        // Items for the machine profile are never written, and the inventory comes only from
        // the table compiled into the helper, for a machine that has one.
        let mut forged = request(tmp.path(), Vec::new(), Vec::new());
        forged.items[0].items = vec![setting(
            "nozzle_volume_type",
            Op::Set,
            "Standard",
            Absent::Add,
        )];
        let out = tempfile::tempdir().unwrap();
        assert_eq!(
            run_request(&forged, out.path()).unwrap_err().code,
            "setting_key_denied"
        );
        let mut inventory = request(tmp.path(), Vec::new(), Vec::new());
        inventory.items[0].inventory = true;
        assert_eq!(
            run_request(&inventory, out.path()).unwrap_err().code,
            "setting_key_denied",
            "the test machine has no inventory in the table"
        );
        let mut misplaced = request(tmp.path(), Vec::new(), Vec::new());
        misplaced.items[1].inventory = true;
        assert_eq!(
            run_request(&misplaced, out.path()).unwrap_err().code,
            "setting_key_denied"
        );
    }

    #[test]
    fn the_inventory_of_the_measured_printer_comes_from_the_table() {
        let tmp = tree();
        put(
            tmp.path(),
            "machine",
            "Bambu Lab X2D 0.4 nozzle",
            r#"{"type": "machine", "instantiation": "true", "nozzle_diameter": ["0.4", "0.4"]}"#,
        );
        let out = tempfile::tempdir().unwrap();
        let request = Request {
            root: tmp.path().to_path_buf(),
            machine: "Bambu Lab X2D 0.4 nozzle".into(),
            limits: limits(),
            items: vec![RequestItem {
                role: "machine".into(),
                name: "Bambu Lab X2D 0.4 nozzle".into(),
                output: "machine.json".into(),
                items: Vec::new(),
                inventory: true,
            }],
        };
        let items = run_request(&request, out.path()).unwrap();
        let machine = read(out.path(), "machine.json");
        assert_eq!(
            machine["extruder_nozzle_count"],
            serde_json::json!(["1", "1"])
        );
        assert_eq!(
            machine["extruder_nozzle_stats"],
            serde_json::json!(["Standard#1", "Standard#1"])
        );
        let keys: Vec<&str> = items[0].applied.iter().map(|a| a.key.as_str()).collect();
        let mut expected = keymap::NOZZLE_INVENTORY_KEYS.to_vec();
        expected.sort_unstable();
        assert_eq!(keys, expected);
    }

    #[test]
    fn only_a_preset_of_its_role_is_flattened_for_use() {
        let tmp = tree();
        let filament = tmp.path().join("filament");
        let restrict = |dir: &Path, name: &str, role: &str| {
            flatten(dir, name, &limits())
                .unwrap()
                .restrict_to_role(role)
        };
        // The leaf says it is a filament preset. Its base says it is not one.
        assert_eq!(
            restrict(&filament, "Test PLA @Test Printer 0.4 nozzle", "filament").unwrap(),
            Vec::<String>::new()
        );
        let error = restrict(&filament, "base_common", "filament").unwrap_err();
        assert_eq!(error.code(), "not_a_preset");
        assert!(error.to_string().contains("instantiation"), "{error}");
        // A profile that does not say inherits the statement of its parent; without any
        // statement it is not a preset.
        let error = restrict(&filament, "base_pla", "filament").unwrap_err();
        assert_eq!(error.code(), "not_a_preset");
        put(tmp.path(), "filament", "silent", r#"{"type": "filament"}"#);
        assert_eq!(
            restrict(&filament, "silent", "filament")
                .unwrap_err()
                .code(),
            "not_a_preset"
        );
        // A preset of another role, or of none, in this role's folder.
        put(
            tmp.path(),
            "filament",
            "misfiled",
            r#"{"type": "machine", "instantiation": "true"}"#,
        );
        let error = restrict(&filament, "misfiled", "filament").unwrap_err();
        assert_eq!(error.code(), "not_a_preset");
        assert!(error.to_string().contains("type"), "{error}");
        put(
            tmp.path(),
            "filament",
            "untyped",
            r#"{"instantiation": "true"}"#,
        );
        assert_eq!(
            restrict(&filament, "untyped", "filament")
                .unwrap_err()
                .code(),
            "not_a_preset"
        );
        // A template is not a preset either, and what it says about itself is not copied
        // into the profile that includes it.
        let machine = tmp.path().join("machine");
        assert_eq!(
            restrict(&machine, "Test Printer template routine", "machine")
                .unwrap_err()
                .code(),
            "not_a_preset"
        );
        restrict(&machine, "Test Printer 0.4 nozzle", "machine").unwrap();
    }

    #[test]
    fn gcode_and_scripts_come_from_the_machine_profile_only() {
        // The rule by key name.
        for (role, key, forbidden) in [
            ("process", "machine_start_gcode", true),
            ("process", "machine_end_gcode", true),
            ("process", "layer_change_gcode", true),
            ("process", "before_layer_change_gcode", true),
            ("process", "change_filament_gcode", true),
            ("process", "time_lapse_gcode", true),
            ("process", "filament_start_gcode", true),
            ("process", "post_process", true),
            ("process", "layer_height", false),
            ("process", "gcode_flavor", false),
            ("filament", "machine_start_gcode", true),
            ("filament", "machine_pause_gcode", true),
            ("filament", "post_process", true),
            ("filament", "filament_start_gcode", false),
            ("filament", "filament_end_gcode", false),
            ("machine", "machine_start_gcode", false),
            ("machine", "layer_change_gcode", false),
            ("machine", "post_process", true),
        ] {
            assert_eq!(forbidden_key(role, key), forbidden, "{role}: {key}");
        }

        let tmp = tempfile::tempdir().unwrap();
        let preset = |role: &str, name: &str, extra: &str| {
            put(
                tmp.path(),
                role,
                name,
                &format!(
                    r#"{{"type": "{role}", "name": "{name}", "instantiation": "true"{extra}}}"#
                ),
            );
            flatten(&tmp.path().join(role), name, &limits())
                .unwrap()
                .restrict_to_role(role)
        };
        // With content, wherever in the profile, such a key stops the flattening.
        for (role, extra, key) in [
            (
                "process",
                r#", "machine_start_gcode": "G1 Z-5 F9000""#,
                "machine_start_gcode",
            ),
            (
                "process",
                r#", "layer_change_gcode": ["", "M104 S300"]"#,
                "layer_change_gcode",
            ),
            (
                "process",
                r#", "post_process": ["/bin/sh -c id"]"#,
                "post_process",
            ),
            (
                "process",
                r#", "machine_end_gcode": {"nested": "M140 S120"}"#,
                "machine_end_gcode",
            ),
            ("process", r#", "time_lapse_gcode": 7"#, "time_lapse_gcode"),
            (
                "filament",
                r#", "machine_end_gcode": "M140 S120""#,
                "machine_end_gcode",
            ),
            ("machine", r#", "post_process": "run.sh""#, "post_process"),
        ] {
            let error = preset(role, "with content", extra).unwrap_err();
            assert_eq!(error.code(), "forbidden_key", "{extra}");
            let text = error.to_string();
            assert!(text.contains(key) && text.contains(role), "{text}");
        }
        // Without content it is left out and named.
        let dropped = preset(
            "process",
            "empty ones",
            r#", "post_process": [], "layer_change_gcode": "", "machine_start_gcode": ["", " "],
               "time_lapse_gcode": null, "layer_height": "0.2""#,
        )
        .unwrap();
        assert_eq!(
            dropped,
            [
                "layer_change_gcode",
                "machine_start_gcode",
                "post_process",
                "time_lapse_gcode"
            ]
        );
        // The machine keeps its routines; the filament keeps its own two.
        put(
            tmp.path(),
            "machine",
            "m",
            r#"{"type": "machine", "instantiation": "true", "machine_start_gcode": "G28",
                "layer_change_gcode": "; layer", "post_process": []}"#,
        );
        let mut machine = flatten(&tmp.path().join("machine"), "m", &limits()).unwrap();
        assert_eq!(
            machine.restrict_to_role("machine").unwrap(),
            ["post_process"]
        );
        assert_eq!(machine.raw("machine_start_gcode"), Some(r#""G28""#));
        assert_eq!(machine.raw("post_process"), None);
        put(
            tmp.path(),
            "filament",
            "f",
            r#"{"type": "filament", "instantiation": "true",
                "filament_start_gcode": ["; start"], "filament_end_gcode": ["; end"]}"#,
        );
        let mut filament = flatten(&tmp.path().join("filament"), "f", &limits()).unwrap();
        assert!(filament.restrict_to_role("filament").unwrap().is_empty());
        assert_eq!(filament.raw("filament_start_gcode"), Some(r#"["; start"]"#));
    }

    #[test]
    fn a_circle_is_found_through_inherits_and_through_include() {
        let tmp = tempfile::tempdir().unwrap();
        put(tmp.path(), "process", "a", r#"{"inherits": "b"}"#);
        put(tmp.path(), "process", "b", r#"{"inherits": "c"}"#);
        put(tmp.path(), "process", "c", r#"{"inherits": "a"}"#);
        let error = flatten(&tmp.path().join("process"), "a", &limits()).unwrap_err();
        assert_eq!(
            error,
            FlattenError::Cycle(vec!["a".into(), "b".into(), "c".into(), "a".into()])
        );
        put(tmp.path(), "process", "self", r#"{"inherits": "self"}"#);
        let error = flatten(&tmp.path().join("process"), "self", &limits()).unwrap_err();
        assert_eq!(error.code(), "cycle");
        put(tmp.path(), "process", "x", r#"{"include": ["y"]}"#);
        put(tmp.path(), "process", "y", r#"{"inherits": "x"}"#);
        let error = flatten(&tmp.path().join("process"), "x", &limits()).unwrap_err();
        assert_eq!(error.code(), "cycle");
        // The same template used at two levels is not a circle.
        put(tmp.path(), "process", "t", r#"{"k": "t"}"#);
        put(tmp.path(), "process", "top", r#"{"include": "t"}"#);
        put(
            tmp.path(),
            "process",
            "leaf",
            r#"{"inherits": "top", "include": ["t"]}"#,
        );
        let leaf = flatten(&tmp.path().join("process"), "leaf", &limits()).unwrap();
        assert_eq!(leaf.includes, ["t", "t"]);
    }

    #[test]
    fn depth_file_count_and_sizes_are_capped() {
        let tmp = tempfile::tempdir().unwrap();
        // A chain of 30 profiles: deeper than the cap of 16.
        for i in 0..30 {
            let json = if i == 0 {
                "{}".to_owned()
            } else {
                format!(r#"{{"inherits": "p{}"}}"#, i - 1)
            };
            put(tmp.path(), "process", &format!("p{i}"), &json);
        }
        let dir = tmp.path().join("process");
        assert_eq!(
            flatten(&dir, "p29", &limits()).unwrap_err().code(),
            "too_deep"
        );
        assert_eq!(flatten(&dir, "p15", &limits()).unwrap().chain.len(), 16);
        assert_eq!(
            flatten(&dir, "p16", &limits()).unwrap_err().code(),
            "too_deep"
        );

        // Many templates, each fine by itself: the number of files read is capped.
        let includes: Vec<String> = (0..10).map(|i| format!("\"p{i}\"")).collect();
        put(
            tmp.path(),
            "process",
            "wide",
            &format!(r#"{{"include": [{}]}}"#, includes.join(",")),
        );
        let few = FlattenLimits {
            max_files: 20,
            ..limits()
        };
        assert_eq!(flatten(&dir, "wide", &few).unwrap_err().code(), "too_many");
        let many: Vec<String> = (0..17).map(|_| "\"p0\"".to_owned()).collect();
        put(
            tmp.path(),
            "process",
            "greedy",
            &format!(r#"{{"include": [{}]}}"#, many.join(",")),
        );
        assert_eq!(
            flatten(&dir, "greedy", &limits()).unwrap_err().code(),
            "too_many"
        );

        // An oversized file is refused by its size, before it is parsed.
        let big = format!(r#"{{"k": "{}"}}"#, "x".repeat(2 * 1024 * 1024));
        put(tmp.path(), "process", "big", &big);
        assert_eq!(
            flatten(&dir, "big", &limits()).unwrap_err().code(),
            "too_large"
        );
        put(
            tmp.path(),
            "process",
            "child of big",
            r#"{"inherits": "big"}"#,
        );
        assert_eq!(
            flatten(&dir, "child of big", &limits()).unwrap_err().code(),
            "too_large"
        );
        // Too many keys, and an output over its cap.
        let keys: Vec<String> = (0..50).map(|i| format!(r#""k{i}": "v""#)).collect();
        put(
            tmp.path(),
            "process",
            "keys",
            &format!("{{{}}}", keys.join(",")),
        );
        let tight = FlattenLimits {
            max_keys: 49,
            ..limits()
        };
        assert_eq!(
            flatten(&dir, "keys", &tight).unwrap_err().code(),
            "too_large"
        );
        let small_output = FlattenLimits {
            max_output_bytes: 100,
            ..limits()
        };
        let keys = flatten(&dir, "keys", &small_output).unwrap();
        assert_eq!(keys.to_json(&small_output).unwrap_err().code(), "too_large");
        // A folder with more entries than the cap is not searched to its end.
        let tiny_folder = FlattenLimits {
            max_dir_entries: 10,
            ..limits()
        };
        assert_eq!(
            flatten(&dir, "p0", &tiny_folder).unwrap_err().code(),
            "folder"
        );
    }

    #[test]
    fn names_cannot_leave_the_folder() {
        let tmp = tree();
        let outside = tmp.path().join("secret.json");
        std::fs::write(&outside, r#"{"secret": "1"}"#).unwrap();
        let dir = tmp.path().join("filament");
        for name in [
            "../secret",
            "..",
            "a/../../secret",
            "/etc/passwd",
            "sub/base_pla",
            "sub\\base_pla",
            "C:secret",
            ".hidden",
            "",
            "a\nb",
        ] {
            let error = flatten(&dir, name, &limits()).unwrap_err();
            assert_eq!(error.code(), "bad_name", "{name:?}");
        }
        // The same names are refused when a profile inherits from them or includes them.
        put(
            tmp.path(),
            "filament",
            "climber",
            r#"{"inherits": "../secret"}"#,
        );
        assert_eq!(
            flatten(&dir, "climber", &limits()).unwrap_err().code(),
            "bad_name"
        );
        put(
            tmp.path(),
            "filament",
            "includer",
            r#"{"include": ["../machine/machine_common"]}"#,
        );
        assert_eq!(
            flatten(&dir, "includer", &limits()).unwrap_err().code(),
            "bad_name"
        );
        // A profile of another role is not found either: roles do not mix.
        let error = flatten(&dir, "machine_common", &limits()).unwrap_err();
        assert_eq!(error.code(), "not_found");
        // A missing name is reported with the names for the same printer.
        let error = flatten(&dir, "Test PETG @Test Printer 0.4 nozzle", &limits()).unwrap_err();
        let text = error.to_string();
        assert!(
            text.contains("Test PLA @Test Printer 0.4 nozzle") && text.contains("no profile named"),
            "{text}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn links_are_never_followed() {
        let tmp = tree();
        let outside = tmp.path().join("secret.json");
        std::fs::write(&outside, r#"{"secret": "1"}"#).unwrap();
        let dir = tmp.path().join("filament");
        // A linked file is not a profile, whether asked for directly or inherited from.
        std::os::unix::fs::symlink(&outside, dir.join("linked.json")).unwrap();
        assert_eq!(
            flatten(&dir, "linked", &limits()).unwrap_err().code(),
            "escape"
        );
        put(tmp.path(), "filament", "heir", r#"{"inherits": "linked"}"#);
        assert_eq!(
            flatten(&dir, "heir", &limits()).unwrap_err().code(),
            "escape"
        );
        // The role's folder itself must not be a link: the link's target would become the
        // folder that profiles may not leave.
        let linked_role = tmp.path().join("linked-role");
        std::os::unix::fs::symlink(&dir, &linked_role).unwrap();
        let error = flatten(&linked_role, "base_pla", &limits()).unwrap_err();
        assert_eq!(error.code(), "escape");
        assert!(error.to_string().contains("symbolic link"), "{error}");
        // Nor the vendor folder a request names.
        let linked_root = tmp.path().parent().unwrap().join(format!(
            "linked-root-{}",
            tmp.path().file_name().unwrap().to_string_lossy()
        ));
        std::os::unix::fs::symlink(tmp.path(), &linked_root).unwrap();
        let out = tempfile::tempdir().unwrap();
        let request = Request {
            root: linked_root.clone(),
            machine: "Test Printer 0.4 nozzle".into(),
            limits: limits(),
            items: vec![RequestItem {
                role: "machine".into(),
                name: "Test Printer 0.4 nozzle".into(),
                output: "machine.json".into(),
                items: Vec::new(),
                inventory: false,
            }],
        };
        let error = run_request(&request, out.path()).unwrap_err();
        std::fs::remove_file(&linked_root).unwrap();
        assert_eq!(error.code, "escape");
        assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 0);
        // A linked folder is not searched.
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("far.json"), "{}").unwrap();
        std::os::unix::fs::symlink(&elsewhere, dir.join("sub")).unwrap();
        assert_eq!(
            flatten(&dir, "far", &limits()).unwrap_err().code(),
            "not_found"
        );
        // A real subfolder is searched, and a name that exists twice below it is refused
        // unless one of the files lies directly in the role's folder.
        std::fs::create_dir(dir.join("a")).unwrap();
        std::fs::create_dir(dir.join("b")).unwrap();
        std::fs::write(dir.join("a/deep.json"), r#"{"k": "a"}"#).unwrap();
        assert_eq!(
            flatten(&dir, "deep", &limits()).unwrap().raw("k"),
            Some(r#""a""#)
        );
        std::fs::write(dir.join("b/deep.json"), r#"{"k": "b"}"#).unwrap();
        assert_eq!(
            flatten(&dir, "deep", &limits()).unwrap_err().code(),
            "ambiguous"
        );
        std::fs::write(dir.join("deep.json"), r#"{"k": "top"}"#).unwrap();
        assert_eq!(
            flatten(&dir, "deep", &limits()).unwrap().raw("k"),
            Some(r#""top""#)
        );
    }

    #[test]
    fn malformed_profiles_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("process");
        for (name, json) in [
            ("list", "[1, 2]"),
            ("cut", r#"{"a": "#),
            ("text", "hello"),
            ("number parent", r#"{"inherits": 7}"#),
            ("two parents", r#"{"inherits": ["a", "b"]}"#),
            ("odd include", r#"{"include": [1]}"#),
            ("deep", &format!("{}1{}", "[".repeat(500), "]".repeat(500))),
        ] {
            put(tmp.path(), "process", name, json);
            assert_eq!(
                flatten(&dir, name, &limits()).unwrap_err().code(),
                "malformed",
                "{name}"
            );
        }
        // An empty or null parent means "no parent"; a byte order mark is skipped.
        put(
            tmp.path(),
            "process",
            "root",
            r#"{"inherits": "", "include": null, "k": "v"}"#,
        );
        assert_eq!(flatten(&dir, "root", &limits()).unwrap().len(), 1);
        std::fs::write(dir.join("bom.json"), b"\xef\xbb\xbf{\"k\": \"v\"}").unwrap();
        assert_eq!(
            flatten(&dir, "bom", &limits()).unwrap().raw("k"),
            Some(r#""v""#)
        );
        assert_eq!(
            flatten(&tmp.path().join("missing"), "x", &limits())
                .unwrap_err()
                .code(),
            "folder"
        );
    }

    #[test]
    fn a_request_writes_all_profiles_or_none_and_reports_what_it_did() {
        let tmp = tree();
        put(
            tmp.path(),
            "process",
            "0.20mm Test @Test Printer",
            r#"{"type": "process", "instantiation": "true", "layer_height": "0.2",
                "compatible_printers": ["Another Printer"], "post_process": []}"#,
        );
        let out = tempfile::tempdir().unwrap();
        let item = |role: &str, name: &str, output: &str| RequestItem {
            role: role.into(),
            name: name.into(),
            output: output.into(),
            items: Vec::new(),
            inventory: false,
        };
        let mut request = Request {
            root: tmp.path().to_path_buf(),
            machine: "Test Printer 0.4 nozzle".into(),
            limits: limits(),
            items: vec![
                item("machine", "Test Printer 0.4 nozzle", "machine.json"),
                item("process", "0.20mm Test @Test Printer", "process.json"),
                item(
                    "filament",
                    "Test PLA @Test Printer 0.4 nozzle",
                    "filament_1.json",
                ),
            ],
        };
        request.items[1].items = vec![setting("curr_bed_type", Op::Set, "Cool Plate", Absent::Add)];
        let items = run_request(&request, out.path()).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(
            (items[0].keys, items[0].compatible.as_str()),
            (6, "unstated")
        );
        assert_eq!(items[1].compatible, "no");
        assert_eq!(items[1].dropped, ["post_process"]);
        assert_eq!(items[2].compatible, "yes");
        assert_eq!(items[2].chain.len(), 3);
        assert!(items[2].dropped.is_empty());
        let process: serde_json::Value =
            serde_json::from_slice(&std::fs::read(out.path().join("process.json")).unwrap())
                .unwrap();
        assert_eq!(process["curr_bed_type"], "Cool Plate");
        assert_eq!(process["layer_height"], "0.2");
        assert!(process.get("post_process").is_none(), "{process}");

        // A base profile, or one of another role, is not written for use.
        let mut base = request.clone();
        base.items[2] = item("filament", "base_pla", "filament_1.json");
        let error = run_request(&base, tempfile::tempdir().unwrap().path()).unwrap_err();
        assert_eq!(error.code, "not_a_preset");
        // An item outside the range of its setting stops the request.
        let mut wild = request.clone();
        wild.items[1]
            .items
            .push(setting("wall_loops", Op::Set, "9999", Absent::Refuse));
        let error = run_request(&wild, tempfile::tempdir().unwrap().path()).unwrap_err();
        assert_eq!(
            (error.code.as_str(), error.key.as_str()),
            ("setting_out_of_range", "wall_loops")
        );

        // One profile that cannot be flattened: nothing is written.
        let out = tempfile::tempdir().unwrap();
        request.items[2].name = "No Such Filament @Test Printer 0.4 nozzle".into();
        let error = run_request(&request, out.path()).unwrap_err();
        assert_eq!(
            (error.code.as_str(), error.role.as_str()),
            ("not_found", "filament")
        );
        assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 0);
        // A request cannot choose where a file goes.
        request.items[2] = item(
            "filament",
            "Test PLA @Test Printer 0.4 nozzle",
            "../escape.json",
        );
        assert_eq!(
            run_request(&request, out.path()).unwrap_err().code,
            "bad_request"
        );
        request.items[2] = item(
            "secret",
            "Test PLA @Test Printer 0.4 nozzle",
            "filament_1.json",
        );
        assert_eq!(
            run_request(&request, out.path()).unwrap_err().code,
            "bad_request"
        );
    }
}
