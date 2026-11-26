//! The mapping layer: which Bambu Studio profile key carries each of Hotend's print settings,
//! and how a value is written into it (`profiles/keymap-bambu-studio.toml`, data).
//!
//! The core never names a key. It hands this crate the typed [`PrintSettings`], and
//! [`Keymap::items`] turns every field that is not "as the installed profile has it" into
//! items `{role, key, op, text, absent}`. A text comes only from the fixed formatters of
//! [`Form`]: a whole number, a decimal with at most two places, a percentage with its sign,
//! `0` or `1`, a token looked up in the row's closed table, a colour `#RRGGBB`, or a plate name
//! of `profiles/system-profiles.toml`. No string of the agent, the engine or the person can
//! become a value.
//!
//! Guarantees, whatever the table says:
//! - a key is lower-case ASCII letters, digits and `_`, and is not on the constant deny list
//!   [`DENIED`] (G-code, scripts, paths, names, identities, machine limits). A row that names
//!   such a key, a machine row, or a key with any other character fails the load;
//! - every row's range is the field's constant hard range (`hotend_settings::bounds`), so the
//!   range is checked twice: in the model and beside the profile ([`Row::text_valid`], which
//!   the profile helper runs again inside the sandbox);
//! - a row may add a key the installed profile lacks only when the key is one of
//!   [`ADD_ALLOWED`], the keys a measured run showed absent and working when added;
//! - a row or a value that was not measured with the real program ([`Row::is_verified`]) is
//!   only used when the asked value equals what the installed profile already holds: then
//!   nothing is written and the item is reported unchanged. Any other value is refused with
//!   `setting_unmeasured` ([`resolve`]); it is never written unseen;
//! - `cap` lowers every entry of a list to at most the value and raises none; an entry that is
//!   not a plain number is `setting_not_numeric`, and an entry of 0 (which can mean "follow
//!   another setting") is `setting_not_applied` unless the row says 0 is off.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use hotend_settings::bounds::{self, Kind};
use hotend_settings::model::{Colour, Field, PlateType, PrintSettings, Unit, Value};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value as Json;

use crate::error::SlicerError;
use crate::system::BedType;

const KEYMAP_TOML: &str = include_str!("../profiles/keymap-bambu-studio.toml");
/// The data file of the keymap, for messages.
pub const KEYMAP_FILE: &str = "profiles/keymap-bambu-studio.toml";

/// The keys a row may add when the installed profile lacks them: measured absent in the system
/// profiles of 02.09.01.70 and working when added (the hand-made project of 17 November 2026).
pub const ADD_ALLOWED: [&str; 3] = ["curr_bed_type", "brim_type", "filament_colour"];

/// The only machine keys Hotend ever writes: the nozzle inventory of both nozzles, without
/// which an exported project fails to slice on its own (return code -100, measured). Their
/// values are machine facts of `[printer.nozzle_inventory]` in `system-profiles.toml`.
pub const NOZZLE_INVENTORY_KEYS: [&str; 6] = [
    "extruder_nozzle_count",
    "extruder_nozzle_diameter",
    "extruder_nozzle_slot",
    "extruder_nozzle_stats",
    "extruder_nozzle_volume_type",
    "nozzle_volume_type",
];

/// Keys that are never an item, in any role. A constant, not data.
#[derive(Debug)]
pub struct Denied {
    /// Denied when the key is exactly one of these.
    pub exact: &'static [&'static str],
    /// Denied when the key contains one of these.
    pub contains: &'static [&'static str],
    /// Denied when the key starts with one of these.
    pub prefixes: &'static [&'static str],
    /// Denied when the key ends with one of these.
    pub suffixes: &'static [&'static str],
}

/// The hard deny list.
pub const DENIED: Denied = Denied {
    exact: &[
        "inherits",
        "include",
        "from",
        "type",
        "instantiation",
        "version",
    ],
    contains: &[
        "gcode",
        "script",
        "post_process",
        "path",
        "file",
        "url",
        "host",
        "token",
        "password",
        "notes",
        "name",
        "_id",
        "thumbnail",
        "inherits",
        "compatible",
        "condition",
        "different_settings",
    ],
    prefixes: &[
        "machine_max_",
        "printable_",
        "extruder_printable_",
        "bed_exclude",
        "wrapping_exclude",
        "bed_custom_",
    ],
    suffixes: &["_gcode", "_dir"],
};

impl Denied {
    /// Whether `key` is denied.
    pub fn matches(&self, key: &str) -> bool {
        self.exact.contains(&key)
            || self.contains.iter().any(|part| key.contains(part))
            || self.prefixes.iter().any(|prefix| key.starts_with(prefix))
            || self.suffixes.iter().any(|suffix| key.ends_with(suffix))
    }
}

/// A key's characters: 1 to 64 lower-case ASCII letters, digits and `_`, starting with a letter.
pub fn valid_key(key: &str) -> bool {
    key.len() <= 64
        && key.starts_with(|c: char| c.is_ascii_lowercase())
        && key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Whether an item may name `key` in `role`: the character rule, the deny list, and for the
/// machine role only the six nozzle inventory keys.
pub fn key_allowed(role: Role, key: &str) -> bool {
    valid_key(key)
        && !DENIED.matches(key)
        && (role != Role::Machine || NOZZLE_INVENTORY_KEYS.contains(&key))
}

/// An enum token of a slicer: 1 to 32 characters of ASCII letters, digits, `_` and brackets.
pub fn valid_token(token: &str) -> bool {
    (1..=32).contains(&token.len())
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '(' | ')'))
}

/// A plate name: words of ASCII letters and digits with single spaces, 48 characters at most.
pub fn valid_plate_name(text: &str) -> bool {
    text.len() <= 48
        && text
            .split(' ')
            .all(|word| !word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// A nozzle inventory value: 1 to 16 characters of ASCII letters, digits, `#` and `.`.
pub fn valid_inventory_value(text: &str) -> bool {
    (1..=16).contains(&text.len())
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '#' | '.'))
}

macro_rules! small_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $token:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            /// The word of the data file.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $token),+
                }
            }
        }
    };
}

small_enum! {
    /// Which settings file a key belongs to.
    Role { Process => "process", Filament => "filament", Machine => "machine" }
}

small_enum! {
    /// The shape of a key's value in the installed profile.
    Shape { Scalar => "scalar", PerFilament => "per_filament", PerVariant => "per_variant" }
}

small_enum! {
    /// How a value becomes text.
    Form {
        Integer => "integer",
        Decimal2 => "decimal2",
        Percent => "percent",
        Flag01 => "flag01",
        Enum => "enum",
        Colour => "colour",
        Plate => "plate",
    }
}

small_enum! {
    /// What an item does with the entries of the installed value.
    Op { Set => "set", Cap => "cap" }
}

small_enum! {
    /// What happens when the installed profile lacks the key.
    Absent { Refuse => "refuse", Add => "add" }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRow {
    field: String,
    role: Role,
    key: String,
    shape: Shape,
    form: Form,
    op: Op,
    absent: Absent,
    #[serde(default)]
    min: Option<f64>,
    #[serde(default)]
    max: Option<f64>,
    verified: bool,
    #[serde(default)]
    verified_texts: Vec<String>,
    #[serde(default)]
    tokens: BTreeMap<String, String>,
    #[serde(default)]
    zero_is_off: bool,
    #[serde(default = "enabled_default")]
    enabled: bool,
    #[serde(default)]
    plate: Option<String>,
}

fn enabled_default() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawKeymap {
    schema: u32,
    revision: String,
    measured_on: String,
    map: Vec<RawRow>,
}

/// One row of the keymap: one profile key of one field.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub field: Field,
    pub role: Role,
    pub key: String,
    pub shape: Shape,
    pub form: Form,
    pub op: Op,
    pub absent: Absent,
    /// The field's hard range, for the numeric forms.
    pub range: Option<(f64, f64)>,
    /// A real run changed this key and the sliced file stated the asked value.
    pub verified: bool,
    /// When not empty, only these texts are verified.
    pub verified_texts: Vec<String>,
    /// Hotend's token and the slicer's, for an enum row, in the order of the field's tokens.
    pub tokens: Vec<(&'static str, String)>,
    /// An entry of 0 means "off" and passes a cap.
    pub zero_is_off: bool,
    /// Reserved rows are neither written nor judged.
    pub enabled: bool,
    /// The plate the row holds for (the bed temperature rows).
    pub plate: Option<PlateType>,
}

/// What a sliced file states for a mapped key, parsed with the row's form. Nothing of the
/// file's text is kept: a number, a flag, one of Hotend's own tokens, a colour, the id of a
/// plate of Hotend's table, or `unreadable`.
#[derive(Debug, Clone, PartialEq)]
pub enum Echo {
    Number(f64),
    Flag(bool),
    Token(&'static str),
    Colour(Colour),
    Plate(String),
    Unreadable,
}

impl Serialize for Echo {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Echo::Number(number) => serializer.serialize_f64(*number),
            Echo::Flag(flag) => serializer.serialize_bool(*flag),
            Echo::Token(token) => serializer.serialize_str(token),
            Echo::Colour(colour) => colour.serialize(serializer),
            Echo::Plate(id) => serializer.serialize_str(id),
            Echo::Unreadable => serializer.serialize_str("unreadable"),
        }
    }
}

impl std::fmt::Display for Echo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Echo::Number(number) => f.write_str(&decimal_text(*number)),
            Echo::Flag(true) => f.write_str("on"),
            Echo::Flag(false) => f.write_str("off"),
            Echo::Token(token) => f.write_str(token),
            Echo::Colour(colour) => write!(f, "{colour}"),
            Echo::Plate(id) => f.write_str(id),
            Echo::Unreadable => f.write_str("unreadable"),
        }
    }
}

impl Echo {
    /// Whether two parsed values are the same: numbers to within rounding to two places.
    pub fn same(&self, other: &Echo) -> bool {
        match (self, other) {
            (Echo::Number(a), Echo::Number(b)) => (a - b).abs() < 0.005,
            (Echo::Unreadable, _) | (_, Echo::Unreadable) => false,
            (a, b) => a == b,
        }
    }
}

/// A number with at most two decimal places and no trailing zeros: `0`, `0.15`, `0.2`, `12`.
pub fn decimal_text(value: f64) -> String {
    let text = format!("{value:.2}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" {
        "0".to_owned()
    } else {
        text.to_owned()
    }
}

/// A plain non-negative number: digits with at most one point inside, at most 12 characters.
pub fn plain_number(text: &str) -> Option<f64> {
    let plain = !text.is_empty()
        && text.len() <= 12
        && text.chars().all(|c| c.is_ascii_digit() || c == '.')
        && text.matches('.').count() <= 1
        && !text.starts_with('.')
        && !text.ends_with('.');
    if !plain {
        return None;
    }
    text.parse::<f64>().ok().filter(|value| value.is_finite())
}

impl Row {
    /// Whether `text` is a measured value of this row.
    pub fn is_verified(&self, text: &str) -> bool {
        self.verified
            && (self.verified_texts.is_empty() || self.verified_texts.iter().any(|t| t == text))
    }

    /// The number a text of a numeric form stands for.
    fn number(&self, text: &str) -> Option<f64> {
        match self.form {
            Form::Integer => text
                .chars()
                .all(|c| c.is_ascii_digit())
                .then(|| plain_number(text))
                .flatten(),
            Form::Decimal2 => plain_number(text),
            Form::Percent => text.strip_suffix('%').and_then(|number| {
                number
                    .chars()
                    .all(|c| c.is_ascii_digit())
                    .then(|| plain_number(number))
                    .flatten()
            }),
            _ => None,
        }
    }

    /// Whether `text` has the row's form and lies in its range. `plates` are the plate names
    /// Hotend knows. The profile helper checks every item with this again.
    pub fn text_valid(&self, text: &str, plates: &[&str]) -> bool {
        match self.form {
            Form::Integer | Form::Decimal2 | Form::Percent => {
                match (self.number(text), self.range) {
                    (Some(number), Some((min, max))) => {
                        number >= min - 1e-9 && number <= max + 1e-9
                    }
                    _ => false,
                }
            }
            Form::Flag01 => matches!(text, "0" | "1"),
            Form::Enum => self.tokens.iter().any(|(_, token)| token == text),
            Form::Colour => text.len() == 7 && Colour::parse(text).is_some(),
            Form::Plate => valid_plate_name(text) && plates.contains(&text),
        }
    }

    /// The text of a model value, from the fixed formatter of the row's form. `plate` is the
    /// slicer's name of the asked plate. `None` when the value does not fit the form.
    pub fn format(&self, value: &Value, plate: &str) -> Option<String> {
        match (self.form, value) {
            (Form::Integer, Value::Whole(whole)) if *whole >= 0 => Some(whole.to_string()),
            (Form::Percent, Value::Whole(whole)) if *whole >= 0 => Some(format!("{whole}%")),
            (Form::Decimal2, Value::Decimal(decimal)) if *decimal >= 0.0 => {
                Some(decimal_text(*decimal))
            }
            (Form::Flag01, Value::Flag(flag)) => Some(if *flag { "1" } else { "0" }.to_owned()),
            (Form::Enum, Value::Token(token)) => self
                .tokens
                .iter()
                .find(|(ours, _)| ours == token)
                .map(|(_, theirs)| theirs.clone()),
            (Form::Colour, Value::Colour(colour)) => Some(colour.to_string()),
            (Form::Plate, Value::Token(_)) => Some(plate.to_owned()),
            _ => None,
        }
    }

    /// What a model value is, in the terms an echo is parsed into. `plate` is the id of the
    /// asked plate.
    pub fn expected(&self, value: &Value, plate: &str) -> Option<Echo> {
        match (self.form, value) {
            (Form::Integer | Form::Percent, Value::Whole(whole)) => {
                Some(Echo::Number(*whole as f64))
            }
            (Form::Decimal2, Value::Decimal(decimal)) => Some(Echo::Number(*decimal)),
            (Form::Flag01, Value::Flag(flag)) => Some(Echo::Flag(*flag)),
            (Form::Enum, Value::Token(token)) => Some(Echo::Token(token)),
            (Form::Colour, Value::Colour(colour)) => Some(Echo::Colour(*colour)),
            (Form::Plate, Value::Token(_)) => Some(Echo::Plate(plate.to_owned())),
            _ => None,
        }
    }

    /// The entries of an echoed value: the whole text for a scalar, else the comma-separated
    /// parts (one per extruder for a six-entry key, measured as `20,20`). At most 16.
    pub fn echo_entries<'a>(&self, text: &'a str) -> Vec<&'a str> {
        match self.shape {
            Shape::Scalar => vec![text.trim()],
            _ => text.split(',').take(17).map(str::trim).collect(),
        }
    }

    /// Parses one echoed entry with the row's form. `beds` give the plate names and ids.
    pub fn parse_echo(&self, entry: &str, beds: &[BedType]) -> Echo {
        let parsed = match self.form {
            Form::Integer | Form::Decimal2 => plain_number(entry).map(Echo::Number),
            Form::Percent => entry
                .strip_suffix('%')
                .and_then(plain_number)
                .map(Echo::Number),
            Form::Flag01 => match entry {
                "0" => Some(Echo::Flag(false)),
                "1" => Some(Echo::Flag(true)),
                _ => None,
            },
            Form::Enum => self
                .tokens
                .iter()
                .find(|(_, theirs)| theirs == entry)
                .map(|(ours, _)| Echo::Token(ours)),
            Form::Colour => Colour::parse(entry).map(Echo::Colour),
            Form::Plate => beds
                .iter()
                .find(|bed| bed.slicer_value == entry)
                .map(|bed| Echo::Plate(bed.id.clone())),
        };
        parsed.unwrap_or(Echo::Unreadable)
    }

    /// Whether an echoed value may stand for this row's field at all: a number inside the
    /// field's hard range, or any value of the row's closed list.
    pub fn echo_in_range(&self, echo: &Echo) -> bool {
        match (echo, self.range) {
            (Echo::Unreadable, _) => false,
            (Echo::Number(number), Some((min, max))) => {
                *number >= min - 1e-9 && *number <= max + 1e-9
            }
            (Echo::Number(_), None) => false,
            _ => true,
        }
    }
}

/// One setting Hotend puts on top of an installed profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Item {
    pub field: Field,
    pub role: Role,
    pub key: String,
    pub op: Op,
    pub text: String,
    pub absent: Absent,
}

/// What [`resolve`] decided for one item.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    /// Write `value`. `changed` says whether the entries differ from the installed ones.
    Write {
        value: Json,
        old: Option<Vec<String>>,
        new: Vec<String>,
        changed: bool,
    },
    /// The row is not measured, and the asked value is what the installed profile holds:
    /// nothing is written.
    Unchanged { old: Vec<String> },
}

/// Why an item cannot be applied. `code` is one of the mapping codes (docs/PRINT_SETTINGS.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapError {
    pub code: &'static str,
    pub key: String,
    pub detail: String,
}

/// Longest entry of an installed value that is reported.
pub const MAX_ENTRY_CHARS: usize = 64;
/// Most entries of an installed value that are reported.
pub const MAX_ENTRIES: usize = 12;

/// The entries of an installed value in the row's shape.
fn entries(row: &Row, value: &Json) -> Result<Vec<String>, MapError> {
    let mismatch = |detail: &str| MapError {
        code: "setting_shape_mismatch",
        key: row.key.clone(),
        detail: detail.to_owned(),
    };
    let list: Vec<String> = match (row.shape, value) {
        (Shape::Scalar, Json::String(text)) => vec![text.clone()],
        (Shape::Scalar, _) => return Err(mismatch("the installed value is not a single text")),
        (Shape::PerFilament | Shape::PerVariant, Json::Array(list)) => list
            .iter()
            .map(|entry| match entry {
                Json::String(text) => Ok(text.clone()),
                _ => Err(mismatch("an entry of the installed list is not a text")),
            })
            .collect::<Result<_, _>>()?,
        _ => return Err(mismatch("the installed value is not a list")),
    };
    let length_ok = match row.shape {
        Shape::Scalar => true,
        Shape::PerFilament => list.len() == 1,
        Shape::PerVariant => (1..=MAX_ENTRIES).contains(&list.len()),
    };
    if !length_ok || list.len() > MAX_ENTRIES {
        return Err(mismatch("the installed list has another length"));
    }
    if list
        .iter()
        .any(|entry| entry.chars().count() > MAX_ENTRY_CHARS)
    {
        return Err(mismatch("an installed entry is too long"));
    }
    Ok(list)
}

fn same_entries(row: &Row, old: &[String], new: &[String]) -> bool {
    old.len() == new.len()
        && old.iter().zip(new).all(|(a, b)| {
            a == b
                || matches!(
                    (row.number(a).or_else(|| plain_number(a)), row.number(b)),
                    (Some(x), Some(y)) if (x - y).abs() < 1e-9
                )
        })
}

/// Applies one item to the installed value `old` (`None`: the profile lacks the key). Pure:
/// the profile helper writes what this returns. `text` must have passed
/// [`Row::text_valid`].
pub fn resolve(row: &Row, text: &str, old: Option<&Json>) -> Result<Resolution, MapError> {
    let fail = |code: &'static str, detail: String| MapError {
        code,
        key: row.key.clone(),
        detail,
    };
    let old_entries = old.map(|value| entries(row, value)).transpose()?;
    let new_entries: Vec<String> = match (&old_entries, row.op) {
        (None, _) => match row.absent {
            Absent::Refuse => {
                return Err(fail(
                    "setting_key_missing",
                    "the installed profile does not have this setting".into(),
                ));
            }
            Absent::Add => vec![text.to_owned()],
        },
        (Some(old), Op::Set) => vec![text.to_owned(); old.len()],
        (Some(old), Op::Cap) => {
            let cap = row
                .number(text)
                .ok_or_else(|| fail("setting_out_of_range", "the limit is not a number".into()))?;
            let mut capped = Vec::with_capacity(old.len());
            for entry in old {
                let Some(number) = plain_number(entry) else {
                    return Err(fail(
                        "setting_not_numeric",
                        "an installed entry is not a plain number, so a limit cannot be shown to \
                         hold"
                            .into(),
                    ));
                };
                if number == 0.0 && !row.zero_is_off {
                    return Err(fail(
                        "setting_not_applied",
                        "an installed entry is 0, which can mean \"follow another setting\", so \
                         the limit cannot be shown to hold"
                            .into(),
                    ));
                }
                capped.push(if number <= cap {
                    entry.clone()
                } else {
                    text.to_owned()
                });
            }
            capped
        }
    };
    let changed = old_entries
        .as_ref()
        .is_none_or(|old| !same_entries(row, old, &new_entries));
    if !row.is_verified(text) {
        return match (changed, old_entries) {
            (false, Some(old)) => Ok(Resolution::Unchanged { old }),
            _ => Err(fail(
                "setting_unmeasured",
                "this value has never been measured with this slicer".into(),
            )),
        };
    }
    let value = match row.shape {
        Shape::Scalar => Json::String(new_entries[0].clone()),
        _ => Json::Array(new_entries.iter().cloned().map(Json::String).collect()),
    };
    Ok(Resolution::Write {
        value,
        old: old_entries,
        new: new_entries,
        changed,
    })
}

/// A field's value for people: the number with its unit, or the token.
pub fn shown_value(field: Field, value: &Value) -> String {
    let unit = match bounds::spec(field).unit {
        Unit::Millimetre => " mm",
        Unit::MillimetrePerSecond => " mm/s",
        Unit::MillimetrePerSecondSquared => " mm/s2",
        Unit::CubicMillimetrePerSecond => " mm3/s",
        Unit::Celsius => " C",
        Unit::Percent => " percent",
        Unit::Degrees => " degrees",
        Unit::Layers => " layers",
        _ => "",
    };
    match value {
        Value::Unset => "as the installed profile has it".to_owned(),
        Value::Whole(whole) => format!("{whole}{unit}"),
        Value::Decimal(decimal) => format!("{}{unit}", decimal_text(*decimal)),
        Value::Token(token) => (*token).to_owned(),
        Value::Flag(true) => "on".to_owned(),
        Value::Flag(false) => "off".to_owned(),
        Value::Colour(colour) => colour.to_string(),
        Value::Slot(slot) => slot.to_string(),
    }
}

/// The fields no key carries: the spool and nozzle are chosen through the filament, the
/// intent and the level act through other fields, and the process preset selects a profile.
pub const UNMAPPED_FIELDS: [Field; 5] = [
    Field::Spool,
    Field::NozzleId,
    Field::Intent,
    Field::Level,
    Field::Process,
];

/// Fields that do nothing while supports are off. With `supports.enabled` off they are not
/// written; their echo is still judged against the field's hard range.
pub const SUPPORT_DETAIL_FIELDS: [Field; 3] = [
    Field::SupportsKind,
    Field::SupportsThreshold,
    Field::SupportsWhere,
];

/// The keymap of `profiles/keymap-bambu-studio.toml`.
#[derive(Debug, Clone, PartialEq)]
pub struct Keymap {
    pub revision: String,
    /// The slicer version and runs the verified rows were measured on.
    pub measured_on: String,
    rows: Vec<Row>,
}

impl Keymap {
    /// The keymap compiled into this build.
    pub fn builtin() -> Result<&'static Keymap, SlicerError> {
        static BUILTIN: OnceLock<Result<Keymap, String>> = OnceLock::new();
        BUILTIN
            .get_or_init(|| Keymap::from_toml(KEYMAP_TOML))
            .as_ref()
            .map_err(|message| SlicerError::Data {
                file: KEYMAP_FILE,
                message: message.clone(),
            })
    }

    /// Parses and checks a keymap in the format of the data file.
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let raw: RawKeymap = toml::from_str(text).map_err(|e| e.to_string())?;
        if raw.schema != 1 {
            return Err(format!("unsupported schema {}", raw.schema));
        }
        if raw.revision.is_empty() || raw.measured_on.is_empty() {
            return Err("the revision and the measurement must be named".into());
        }
        let mut rows = Vec::new();
        for raw_row in raw.map {
            let row = check_row(raw_row)?;
            if rows.iter().any(|known: &Row| known.key == row.key) {
                return Err(format!("`{}` has two rows", row.key));
            }
            rows.push(row);
        }
        // Every field a plan can write has a row, and every plate its two bed rows.
        for field in Field::ALL {
            if UNMAPPED_FIELDS.contains(field) {
                continue;
            }
            if matches!(field, Field::FirstLayerBed | Field::LayersBed) {
                for plate in PlateType::ALL {
                    if *plate == PlateType::CoolSupertack {
                        continue;
                    }
                    if !rows
                        .iter()
                        .any(|row| row.field == *field && row.plate == Some(*plate))
                    {
                        return Err(format!("no row for `{field}` on the plate `{plate}`"));
                    }
                }
            } else if !rows.iter().any(|row| row.field == *field) {
                return Err(format!("no row for the field `{field}`"));
            }
        }
        Ok(Self {
            revision: raw.revision,
            measured_on: raw.measured_on,
            rows,
        })
    }

    /// Every row, in the order of the data file.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The row of a key in a role.
    pub fn row(&self, role: Role, key: &str) -> Option<&Row> {
        self.rows
            .iter()
            .find(|row| row.role == role && row.key == key)
    }

    /// The row of a key, in any role.
    pub fn row_by_key(&self, key: &str) -> Option<&Row> {
        self.rows.iter().find(|row| row.key == key)
    }

    /// Every key of the keymap, in the order of the data file.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.rows.iter().map(|row| row.key.as_str())
    }

    /// Whether `row` applies to a plate with these settings: the plate rows of the asked
    /// plate only, the support details only while supports are on, reserved rows never.
    pub fn applies(row: &Row, settings: &PrintSettings) -> bool {
        let supports_off = settings.get(Field::SupportsEnabled) == Value::Flag(false);
        row.enabled
            && row
                .plate
                .is_none_or(|plate| Some(plate) == settings.plate_type())
            && !(supports_off && SUPPORT_DETAIL_FIELDS.contains(&row.field))
    }

    /// The items for a plate's settings, in the order of the data file. `bed` is the plate the
    /// slice is for; it must be the plate of the settings. A field that is "as the installed
    /// profile has it" gives no item.
    pub fn items(&self, settings: &PrintSettings, bed: &BedType) -> Result<Vec<Item>, SlicerError> {
        let plate = settings.plate_type().ok_or_else(|| {
            SlicerError::InvalidJob("the print settings name no plate type".into())
        })?;
        if plate.as_str() != bed.id {
            return Err(SlicerError::InvalidJob(format!(
                "the print settings are for the plate `{plate}`, but the slice is for `{}`; \
                 choose one plate and plan again",
                bed.id
            )));
        }
        let mut items = Vec::new();
        for row in self.rows.iter().filter(|row| Self::applies(row, settings)) {
            let value = settings.get(row.field);
            if value.is_unset() {
                continue;
            }
            let text = row
                .format(&value, &bed.slicer_value)
                .filter(|text| row.text_valid(text, &[bed.slicer_value.as_str()]))
                .ok_or_else(|| SlicerError::Setting {
                    code: "setting_out_of_range",
                    message: format!(
                        "the {} of {} cannot be written into the slicer's settings: it does \
                         not fit the setting's form or range",
                        row.field.plain_name(),
                        shown_value(row.field, &value)
                    ),
                })?;
            items.push(Item {
                field: row.field,
                role: row.role,
                key: row.key.clone(),
                op: row.op,
                text,
                absent: row.absent,
            });
        }
        Ok(items)
    }

    /// The items for a plate's settings resolved against the installed profile values `base`
    /// (by role and key; `None` when the profile lacks the key): what would be written and what
    /// is left unchanged. The profile helper does the same inside the sandbox on the real
    /// profiles; this is the same logic for a caller that holds the values.
    pub fn items_for(
        &self,
        settings: &PrintSettings,
        bed: &BedType,
        base: &dyn Fn(Role, &str) -> Option<Json>,
    ) -> Result<Vec<(Item, Resolution)>, SlicerError> {
        let mut resolved = Vec::new();
        for item in self.items(settings, bed)? {
            let row = self
                .row(item.role, &item.key)
                .ok_or_else(|| SlicerError::Data {
                    file: KEYMAP_FILE,
                    message: format!("`{}` has no row", item.key),
                })?;
            let outcome = resolve(row, &item.text, base(item.role, &item.key).as_ref())
                .map_err(|error| self.error(&error, Some(settings)))?;
            resolved.push((item, outcome));
        }
        Ok(resolved)
    }

    /// The error of a mapping failure, in plain words: the field, the asked value, and what is
    /// wrong. `settings` give the asked value; without them the item's text is not repeated.
    pub fn error(&self, error: &MapError, settings: Option<&PrintSettings>) -> SlicerError {
        let row = self.row_by_key(&error.key);
        let field = row.map_or("a setting", |row| row.field.plain_name());
        let asked = match (row, settings) {
            (Some(row), Some(settings)) => {
                format!(" of {}", shown_value(row.field, &settings.get(row.field)))
            }
            _ => String::new(),
        };
        let message = match error.code {
            "setting_unmeasured" => format!(
                "Hotend's {field}{asked} has never been measured with this slicer: no real \
                 slice has shown that the slicer applies it, and it differs from what the \
                 installed profile holds. Hotend does not write a setting it has not seen \
                 work, so nothing was sliced. Choose the value the plan proposes without the \
                 change, or wait until this setting has been measured"
            ),
            "setting_key_missing" => format!(
                "the installed slicer profile has no setting for the {field}, and Hotend does \
                 not invent one (a slicer may ignore it in silence), so nothing was sliced"
            ),
            "setting_not_numeric" | "setting_not_applied" => format!(
                "the limit for the {field}{asked} cannot be applied: {}. Nothing was sliced",
                error.detail
            ),
            "setting_shape_mismatch" => format!(
                "the installed slicer profile holds the {field} in another form than Hotend \
                 knows ({}), so nothing was written and nothing was sliced",
                error.detail
            ),
            "setting_key_denied" => format!(
                "a setting Hotend never writes was asked for ({}); nothing was sliced",
                error.detail
            ),
            _ => format!(
                "the {field}{asked} cannot be written into the slicer's settings ({}); nothing \
                 was sliced",
                error.detail
            ),
        };
        SlicerError::Setting {
            code: known_code(error.code),
            message,
        }
    }
}

/// The mapping codes, so that a code read back from the helper's report is one of ours.
pub const MAPPING_CODES: [&str; 7] = [
    "setting_unmeasured",
    "setting_out_of_range",
    "setting_key_denied",
    "setting_key_missing",
    "setting_not_numeric",
    "setting_shape_mismatch",
    "setting_not_applied",
];

/// The constant for a code; an unknown one becomes `setting_out_of_range`.
pub fn known_code(code: &str) -> &'static str {
    MAPPING_CODES
        .iter()
        .copied()
        .find(|known| *known == code)
        .unwrap_or("setting_out_of_range")
}

fn check_row(raw: RawRow) -> Result<Row, String> {
    let key = raw.key;
    let bad = |message: &str| format!("the row `{key}`: {message}");
    let field = Field::from_token(&raw.field).ok_or_else(|| bad("unknown field"))?;
    if UNMAPPED_FIELDS.contains(&field) {
        return Err(bad("this field is carried by no key"));
    }
    if raw.role == Role::Machine {
        return Err(bad(
            "machine rows are not allowed; the nozzle inventory has its own path",
        ));
    }
    if !valid_key(&key) {
        return Err(bad("a key is lower-case ASCII letters, digits and `_`"));
    }
    if DENIED.matches(&key) {
        return Err(bad("the key is on the deny list"));
    }
    let kind = bounds::spec(field).kind;
    let form_fits = match (&kind, raw.form) {
        (Kind::Whole { .. }, Form::Integer | Form::Percent) => true,
        (Kind::Decimal { .. }, Form::Decimal2) => true,
        (Kind::Tokens(_), Form::Plate) => field == Field::PlateType,
        (Kind::Tokens(_), Form::Enum) => field != Field::PlateType,
        (Kind::Flag, Form::Flag01) => true,
        (Kind::Colour, Form::Colour) => true,
        _ => false,
    };
    if !form_fits {
        return Err(bad("the form does not fit the field"));
    }
    let range = bounds::numeric_range(field);
    match (raw.min, raw.max, range) {
        (Some(min), Some(max), Some((low, high)))
            if (min - low).abs() < 1e-9 && (max - high).abs() < 1e-9 => {}
        (None, None, None) => {}
        _ => return Err(bad("the range must be the field's hard range")),
    }
    let mut tokens = Vec::new();
    match (&kind, raw.form) {
        (Kind::Tokens(ours), Form::Enum) => {
            if raw.tokens.len() != ours.len() {
                return Err(bad(
                    "every token of the field needs exactly one slicer token",
                ));
            }
            for token in ours.iter() {
                let theirs = raw
                    .tokens
                    .get(*token)
                    .ok_or_else(|| bad("a token of the field has no slicer token"))?;
                if !valid_token(theirs) {
                    return Err(bad("a slicer token has a character that is not allowed"));
                }
                tokens.push((*token, theirs.clone()));
            }
        }
        _ if !raw.tokens.is_empty() => return Err(bad("only an enum row has tokens")),
        _ => {}
    }
    if raw.absent == Absent::Add
        && (!ADD_ALLOWED.contains(&key.as_str())
            || raw.shape == Shape::PerVariant
            || raw.op == Op::Cap)
    {
        return Err(bad("this key may not be added to a profile that lacks it"));
    }
    if raw.op == Op::Cap && !matches!(raw.form, Form::Integer | Form::Decimal2) {
        return Err(bad("only a number can be a limit"));
    }
    if raw.zero_is_off && raw.op != Op::Cap {
        return Err(bad("`zero_is_off` belongs to a limit"));
    }
    let plate = match (&raw.plate, field) {
        (Some(plate), Field::FirstLayerBed | Field::LayersBed) => Some(
            PlateType::from_token(plate)
                .filter(|plate| *plate != PlateType::CoolSupertack)
                .ok_or_else(|| bad("unknown plate"))?,
        ),
        (None, Field::FirstLayerBed | Field::LayersBed) => {
            return Err(bad("a bed temperature row names its plate"));
        }
        (Some(_), _) => return Err(bad("only a bed temperature row names a plate")),
        (None, _) => None,
    };
    let row = Row {
        field,
        role: raw.role,
        key: key.clone(),
        shape: raw.shape,
        form: raw.form,
        op: raw.op,
        absent: raw.absent,
        range,
        verified: raw.verified,
        verified_texts: raw.verified_texts,
        tokens,
        zero_is_off: raw.zero_is_off,
        enabled: raw.enabled,
        plate,
    };
    if !row.verified && !row.verified_texts.is_empty() {
        return Err(bad("`verified_texts` belongs to a verified row"));
    }
    for text in &row.verified_texts {
        // A plate name cannot be checked here; every other form can.
        if row.form == Form::Plate || !row.text_valid(text, &[]) {
            return Err(bad("a verified text is not a value of the row"));
        }
    }
    Ok(row)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn keymap() -> &'static Keymap {
        Keymap::builtin().unwrap()
    }

    fn row(key: &str) -> &'static Row {
        keymap().row_by_key(key).unwrap()
    }

    #[test]
    fn the_keys_of_the_keymap_are_pinned() {
        // The settings Hotend may write on top of an installed profile, and nothing else.
        let keys: Vec<&str> = keymap().keys().collect();
        assert_eq!(
            keys,
            [
                "curr_bed_type",
                "brim_type",
                "brim_width",
                "brim_object_gap",
                "skirt_loops",
                "elefant_foot_compensation",
                "initial_layer_print_height",
                "initial_layer_line_width",
                "initial_layer_infill_line_width",
                "initial_layer_speed",
                "initial_layer_infill_speed",
                "initial_layer_acceleration",
                "cool_plate_temp_initial_layer",
                "cool_plate_temp",
                "textured_plate_temp_initial_layer",
                "textured_plate_temp",
                "hot_plate_temp_initial_layer",
                "hot_plate_temp",
                "eng_plate_temp_initial_layer",
                "eng_plate_temp",
                "nozzle_temperature_initial_layer",
                "nozzle_temperature",
                "close_fan_the_first_x_layers",
                "first_x_layer_fan_speed",
                "additional_cooling_fan_speed",
                "wall_loops",
                "sparse_infill_density",
                "sparse_infill_pattern",
                "seam_position",
                "enable_support",
                "support_type",
                "support_threshold_angle",
                "support_on_build_plate_only",
                "outer_wall_speed",
                "inner_wall_speed",
                "sparse_infill_speed",
                "internal_solid_infill_speed",
                "top_surface_speed",
                "gap_infill_speed",
                "default_acceleration",
                "outer_wall_acceleration",
                "filament_max_volumetric_speed",
                "outer_wall_jerk",
                "inner_wall_jerk",
                "top_surface_jerk",
                "infill_jerk",
                "initial_layer_jerk",
                "enable_prime_tower",
                "filament_colour",
            ]
        );
        // Measured: only the keys the real test slices changed and echoed.
        let verified: Vec<&str> = keymap()
            .rows()
            .iter()
            .filter(|row| row.verified)
            .map(|row| row.key.as_str())
            .collect();
        assert_eq!(
            verified,
            [
                "curr_bed_type",
                "brim_type",
                "brim_width",
                "brim_object_gap",
                "skirt_loops",
                "initial_layer_speed",
                "initial_layer_infill_speed",
                "textured_plate_temp_initial_layer",
                "textured_plate_temp",
                "close_fan_the_first_x_layers",
                "first_x_layer_fan_speed",
                "additional_cooling_fan_speed",
                "wall_loops",
                "enable_support",
                "enable_prime_tower",
                "filament_colour",
            ]
        );
    }

    #[test]
    fn every_row_keeps_to_its_field_and_passes_the_deny_list() {
        for row in keymap().rows() {
            assert!(key_allowed(row.role, &row.key), "{}", row.key);
            assert_eq!(row.range, bounds::numeric_range(row.field), "{}", row.key);
            for (_, token) in &row.tokens {
                assert!(valid_token(token), "{token}");
            }
            assert!(
                row.absent == Absent::Refuse || ADD_ALLOWED.contains(&row.key.as_str()),
                "{}",
                row.key
            );
        }
        // Every enum value of the model has a slicer token.
        for field in Field::ALL {
            if let Kind::Tokens(tokens) = bounds::spec(*field).kind
                && *field != Field::PlateType
                && !UNMAPPED_FIELDS.contains(field)
            {
                let row = keymap()
                    .rows()
                    .iter()
                    .find(|row| row.field == *field)
                    .unwrap();
                for token in tokens {
                    assert!(row.tokens.iter().any(|(ours, _)| ours == token), "{token}");
                }
            }
        }
    }

    #[test]
    fn the_deny_list_holds_for_every_named_key() {
        for key in [
            "machine_start_gcode",
            "filament_start_gcode",
            "post_process",
            "print_host",
            "filename_format",
            "printable_area",
            "machine_max_speed_x",
            "inherits",
            "compatible_printers",
            "different_settings_to_system",
            "print_settings_id",
            "setting_id",
            "version",
            "type",
            "from",
            "include",
            "instantiation",
            "bed_exclude_area",
            "wrapping_exclude_area",
            "bed_custom_texture",
            "extruder_printable_area",
            "upload_url",
            "thumbnails",
            "notes",
            "printer_notes",
            "user_dir",
            "api_token",
            "host_password",
            "condition",
            "printer_model_name",
        ] {
            assert!(DENIED.matches(key), "{key}");
            assert!(!key_allowed(Role::Process, key), "{key}");
        }
        // Machine keys: only the nozzle inventory.
        for key in NOZZLE_INVENTORY_KEYS {
            assert!(key_allowed(Role::Machine, key), "{key}");
        }
        for key in ["nozzle_diameter", "wall_loops", "printer_variant"] {
            assert!(!key_allowed(Role::Machine, key), "{key}");
        }
    }

    #[test]
    fn a_table_that_opens_anything_fails_to_load() {
        let base = KEYMAP_TOML;
        let wall = "key = \"wall_loops\"";
        for (from, to) in [
            ("schema = 1", "schema = 2"),
            (wall, "key = \"machine_start_gcode\""),
            (wall, "key = \"filament_start_gcode\""),
            (wall, "key = \"post_process\""),
            (wall, "key = \"print_host\""),
            (wall, "key = \"filename_format\""),
            (wall, "key = \"printable_area\""),
            (wall, "key = \"machine_max_speed_x\""),
            (wall, "key = \"inherits\""),
            (wall, "key = \"compatible_printers\""),
            // Upper case and a look-alike character.
            (wall, "key = \"Wall_loops\""),
            (wall, "key = \"wall_lo\u{043e}ps\""),
            (wall, "key = \"wall loops\""),
            // A machine row.
            (
                "role = \"process\"\nkey = \"wall_loops\"",
                "role = \"machine\"\nkey = \"wall_loops\"",
            ),
            // A range other than the field's.
            ("min = 2\nmax = 6", "min = 2\nmax = 9"),
            // A key that may not be added.
            (
                "key = \"wall_loops\"\nshape = \"scalar\"\nform = \"integer\"\nop = \"set\"\nabsent = \"refuse\"",
                "key = \"wall_loops\"\nshape = \"scalar\"\nform = \"integer\"\nop = \"set\"\nabsent = \"add\"",
            ),
            // A token with a character that is not allowed.
            ("grid = \"grid\"", "grid = \"grid;M104\""),
            // An unknown column.
            (
                "verified = true\nverified_texts = [\"0\"]",
                "verified = true\nverified_texts = [\"0\"]\ntext = \"1\"",
            ),
            // A verified text that is not a value of the row.
            (
                "verified_texts = [\"outer_only\", \"no_brim\"]",
                "verified_texts = [\"auto_brim\"]",
            ),
        ] {
            assert!(base.contains(from), "{from}");
            assert!(
                Keymap::from_toml(&base.replacen(from, to, 1)).is_err(),
                "{to}"
            );
        }
        // A field without a row.
        let without = base
            .replace("key = \"wall_loops\"", "key = \"wall_loops_x\"")
            .replace(
                "field = \"shell.wall_loops\"",
                "field = \"shell.infill_pct\"",
            );
        assert!(Keymap::from_toml(&without).is_err());
    }

    #[test]
    fn values_are_written_by_the_fixed_formatters_only() {
        let colour = Colour::parse("#898989").unwrap();
        for (key, value, text) in [
            ("brim_width", Value::Whole(5), "5"),
            ("brim_object_gap", Value::Decimal(0.0), "0"),
            ("elefant_foot_compensation", Value::Decimal(0.15), "0.15"),
            ("initial_layer_print_height", Value::Decimal(0.2), "0.2"),
            ("sparse_infill_density", Value::Whole(15), "15%"),
            ("enable_prime_tower", Value::Flag(false), "0"),
            ("brim_type", Value::Token("outer"), "outer_only"),
            ("support_type", Value::Token("tree"), "tree(auto)"),
            ("support_on_build_plate_only", Value::Token("bed_only"), "1"),
            ("filament_colour", Value::Colour(colour), "#898989"),
            (
                "curr_bed_type",
                Value::Token("textured-pei"),
                "Textured PEI Plate",
            ),
        ] {
            let row = row(key);
            let written = row.format(&value, "Textured PEI Plate").unwrap();
            assert_eq!(written, text, "{key}");
            assert!(row.text_valid(&written, &["Textured PEI Plate"]), "{key}");
        }
        // Texts outside the form or the range never pass the check the helper runs again.
        for (key, text) in [
            ("brim_width", "16"),
            ("brim_width", "5.5"),
            ("brim_width", "-1"),
            ("brim_width", "5\nM104 S300"),
            ("brim_object_gap", "0.31"),
            ("brim_object_gap", "1e-1"),
            ("sparse_infill_density", "15"),
            ("sparse_infill_density", "60%"),
            ("enable_prime_tower", "2"),
            ("brim_type", "auto_brim"),
            ("filament_colour", "#89898"),
            ("filament_colour", "grey"),
            ("curr_bed_type", "Glass Plate"),
            ("curr_bed_type", "Textured PEI Plate;"),
        ] {
            assert!(
                !row(key).text_valid(text, &["Textured PEI Plate"]),
                "{key}={text:?}"
            );
        }
    }

    fn strings(list: &[&str]) -> Json {
        Json::Array(
            list.iter()
                .map(|text| Json::String((*text).to_owned()))
                .collect(),
        )
    }

    #[test]
    fn a_cap_lowers_each_entry_and_raises_none() {
        // A hand-written speed list of six variants, three direct drive and three Bowden.
        let speeds = Row {
            verified: true,
            ..row("outer_wall_speed").clone()
        };
        let installed = strings(&["200", "500", "500", "50", "50", "50"]);
        let Resolution::Write {
            value,
            new,
            changed,
            ..
        } = resolve(&speeds, "100", Some(&installed)).unwrap()
        else {
            panic!("written");
        };
        assert_eq!(value, strings(&["100", "100", "100", "50", "50", "50"]));
        assert_eq!(new.len(), 6);
        assert!(changed);
        // Not numeric: `nil` and a percentage.
        for odd in ["nil", "50%"] {
            let error = resolve(&speeds, "100", Some(&strings(&["200", odd]))).unwrap_err();
            assert_eq!(error.code, "setting_not_numeric");
        }
        // A 0 entry under a required cap, and under a row where 0 is off.
        let error = resolve(&speeds, "100", Some(&strings(&["0", "200"]))).unwrap_err();
        assert_eq!(error.code, "setting_not_applied");
        let fan = row("additional_cooling_fan_speed");
        assert!(matches!(
            resolve(fan, "30", Some(&strings(&["0"]))).unwrap(),
            Resolution::Write { changed: false, .. }
        ));
        let Resolution::Write { value, .. } = resolve(fan, "30", Some(&strings(&["70"]))).unwrap()
        else {
            panic!("written");
        };
        assert_eq!(value, strings(&["30"]));
        // The shape of the installed value must be the row's.
        for wrong in [
            Json::String("200".into()),
            strings(&[]),
            Json::Array(vec![Json::from(200)]),
        ] {
            assert_eq!(
                resolve(&speeds, "100", Some(&wrong)).unwrap_err().code,
                "setting_shape_mismatch"
            );
        }
        assert_eq!(
            resolve(fan, "30", Some(&strings(&["70", "70"])))
                .unwrap_err()
                .code,
            "setting_shape_mismatch"
        );
    }

    #[test]
    fn absent_keys_are_added_only_where_measured() {
        let missing = resolve(row("wall_loops"), "3", None).unwrap_err();
        assert_eq!(missing.code, "setting_key_missing");
        let Resolution::Write {
            value,
            old,
            changed,
            ..
        } = resolve(row("brim_type"), "outer_only", None).unwrap()
        else {
            panic!("written");
        };
        assert_eq!(value, Json::String("outer_only".into()));
        assert_eq!((old, changed), (None, true));
        let Resolution::Write { value, .. } =
            resolve(row("filament_colour"), "#898989", None).unwrap()
        else {
            panic!("written");
        };
        assert_eq!(value, strings(&["#898989"]));
    }

    #[test]
    fn an_unmeasured_value_is_used_only_when_the_profile_already_holds_it() {
        // Not measured: equal means unchanged and nothing written; different is refused.
        let density = row("sparse_infill_density");
        assert_eq!(
            resolve(density, "15%", Some(&Json::String("15%".into()))).unwrap(),
            Resolution::Unchanged {
                old: vec!["15%".into()]
            }
        );
        let refused = resolve(density, "20%", Some(&Json::String("15%".into()))).unwrap_err();
        assert_eq!(refused.code, "setting_unmeasured");
        // A cap that changes nothing on an unmeasured row is unchanged; one that lowers is not.
        let outer = row("outer_wall_speed");
        let installed = strings(&["200", "500", "500", "50", "50", "50"]);
        assert!(matches!(
            resolve(outer, "200", Some(&strings(&["200", "50"]))).unwrap(),
            Resolution::Unchanged { .. }
        ));
        assert_eq!(
            resolve(outer, "60", Some(&installed)).unwrap_err().code,
            "setting_unmeasured"
        );
        // A verified row with a value outside its measured texts is unmeasured too. (`no_brim`
        // was measured on 18 November 2026; the inner kinds never were.)
        let brim = row("brim_type");
        assert_eq!(
            resolve(brim, "inner_only", Some(&Json::String("auto_brim".into())))
                .unwrap_err()
                .code,
            "setting_unmeasured"
        );
        assert_eq!(
            resolve(brim, "outer_and_inner", None).unwrap_err().code,
            "setting_unmeasured"
        );
        assert!(matches!(
            resolve(brim, "no_brim", Some(&Json::String("auto_brim".into()))).unwrap(),
            Resolution::Write { changed: true, .. }
        ));
        let support = row("enable_support");
        assert_eq!(
            resolve(support, "1", Some(&Json::String("0".into())))
                .unwrap_err()
                .code,
            "setting_unmeasured"
        );
        // Numbers compare as numbers.
        let gap = row("brim_object_gap");
        assert!(matches!(
            resolve(gap, "0.1", Some(&Json::String("0.10".into()))).unwrap(),
            Resolution::Write { changed: false, .. }
        ));
    }

    #[test]
    fn echoes_are_parsed_into_values_and_never_kept_as_text() {
        let beds = crate::system::SystemProfiles::builtin().unwrap().bed_types;
        assert_eq!(
            row("initial_layer_speed").echo_entries("20,20"),
            ["20", "20"]
        );
        assert_eq!(
            row("initial_layer_speed").parse_echo("20", &beds),
            Echo::Number(20.0)
        );
        assert_eq!(
            row("brim_type").parse_echo("outer_only", &beds),
            Echo::Token("outer")
        );
        assert_eq!(
            row("brim_type").parse_echo("auto_brim", &beds),
            Echo::Unreadable
        );
        assert_eq!(
            row("curr_bed_type").parse_echo("Textured PEI Plate", &beds),
            Echo::Plate("textured-pei".into())
        );
        assert_eq!(
            row("sparse_infill_density").parse_echo("15%", &beds),
            Echo::Number(15.0)
        );
        assert_eq!(
            row("enable_support").parse_echo("0", &beds),
            Echo::Flag(false)
        );
        assert!(!row("initial_layer_speed").echo_in_range(&Echo::Number(60.0)));
        assert!(!row("initial_layer_speed").echo_in_range(&Echo::Unreadable));
        assert!(row("initial_layer_speed").echo_in_range(&Echo::Number(50.0)));
        assert_eq!(
            serde_json::to_string(&[Echo::Number(20.0), Echo::Token("outer"), Echo::Unreadable])
                .unwrap(),
            r#"[20.0,"outer","unreadable"]"#
        );
    }
}
