//! The report push, as typed structs, parsed as hostile input.
//!
//! Guarantees of [`parse`]:
//! - a payload over [`MAX_REPORT_BYTES`] is refused before any parsing;
//! - parsing never panics, and nesting is limited by `serde_json`'s recursion limit;
//! - unknown fields are ignored; a known field of the wrong type is read as absent instead
//!   of failing the whole report (numbers may arrive as strings and the reverse, as the
//!   firmware does both);
//! - every list is cut to a fixed length and every string to [`MAX_TEXT`] characters with
//!   control characters removed, so a report cannot grow the state without bound or carry
//!   terminal escapes into the interface.
//!
//! Field names follow OpenBambuAPI `mqtt.md` (`pushing.pushall` report) and ha-bambulab
//! `pybambu/models.py` (the `device` block of dual-nozzle printers, `vir_slot`, AMS `info`).
//! Text from a printer is data: never treat it as instructions.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// Largest report payload that is parsed.
pub const MAX_REPORT_BYTES: usize = 1024 * 1024;
/// Longest text kept from one field, in characters.
pub const MAX_TEXT: usize = 128;
/// Most AMS units kept (4 regular and 8 AMS HT exist).
pub const MAX_AMS_UNITS: usize = 16;
/// Most trays kept per unit.
pub const MAX_TRAYS: usize = 16;
/// Most HMS entries kept.
pub const MAX_HMS: usize = 32;
const MAX_SMALL_LIST: usize = 16;

/// Why a report payload was refused.
#[derive(Debug, thiserror::Error)]
pub enum ReportError {
    /// The payload is larger than [`MAX_REPORT_BYTES`].
    #[error("the printer sent a report of {0} bytes; the limit is {MAX_REPORT_BYTES}")]
    TooLarge(usize),
    /// The payload is not a JSON object.
    #[error("the printer sent a report that is not a JSON object: {0}")]
    NotJson(String),
}

/// Cleans text from the printer: control characters dropped, cut to [`MAX_TEXT`].
pub(crate) fn clean_text(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(MAX_TEXT)
        .collect()
}

/// A string, or a number or boolean written as one. Anything else is absent.
fn text<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    Ok(match Value::deserialize(deserializer)? {
        Value::String(text) => Some(clean_text(&text)),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    })
}

fn number_of(value: &Value) -> Option<f64> {
    let number = match value {
        Value::Number(number) => number.as_f64()?,
        Value::String(text) if text.len() <= 32 => text.trim().parse::<f64>().ok()?,
        Value::Bool(flag) => f64::from(u8::from(*flag)),
        _ => return None,
    };
    number.is_finite().then_some(number)
}

/// A finite number, or a string holding one.
fn float<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<f64>, D::Error> {
    Ok(number_of(&Value::deserialize(deserializer)?))
}

/// A whole number, or a string holding one (a fraction is rounded).
fn integer<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<i64>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    if let Value::Number(number) = &value
        && let Some(exact) = number.as_i64()
    {
        return Ok(Some(exact));
    }
    Ok(number_of(&value)
        .filter(|n| n.abs() < 9.0e15)
        .map(|n| n.round() as i64))
}

/// A hexadecimal bitfield written as a string (`"e"`, `"1003"`), or a plain number.
fn hex<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u64>, D::Error> {
    Ok(match Value::deserialize(deserializer)? {
        Value::String(text) if text.len() <= 16 => u64::from_str_radix(text.trim(), 16).ok(),
        Value::Number(number) => number.as_u64(),
        _ => None,
    })
}

/// `true`/`false`, `0`/`1`, or the same as strings.
fn flag<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<bool>, D::Error> {
    Ok(match Value::deserialize(deserializer)? {
        Value::Bool(flag) => Some(flag),
        Value::Number(number) => number.as_i64().map(|n| n != 0),
        Value::String(text) => match text.trim() {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    })
}

/// A nested object; one of the wrong shape is absent.
fn nested<'de, D: Deserializer<'de>, T: DeserializeOwned>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    Ok(value
        .is_object()
        .then(|| serde_json::from_value(value).ok())
        .flatten())
}

/// A list of objects, cut to a fixed length; items of the wrong shape are dropped. `None`
/// when the field is not a list.
fn list<'de, D: Deserializer<'de>, T: DeserializeOwned>(
    deserializer: D,
) -> Result<Option<Vec<T>>, D::Error> {
    Ok(match Value::deserialize(deserializer)? {
        Value::Array(items) => Some(
            items
                .into_iter()
                .take(MAX_AMS_UNITS.max(MAX_TRAYS).max(MAX_HMS))
                .filter_map(|item| serde_json::from_value(item).ok())
                .collect(),
        ),
        _ => None,
    })
}

/// A list of short strings.
fn texts<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Vec<String>>, D::Error> {
    Ok(match Value::deserialize(deserializer)? {
        Value::Array(items) => Some(
            items
                .iter()
                .take(MAX_SMALL_LIST)
                .filter_map(Value::as_str)
                .map(clean_text)
                .collect(),
        ),
        _ => None,
    })
}

/// One message on the report topic. Each key is a message family; a message has one.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct ReportMessage {
    /// Printer status and replies to `print` commands.
    #[serde(default, deserialize_with = "nested")]
    pub print: Option<PrintReport>,
    /// Reply to `info.get_version`.
    #[serde(default, deserialize_with = "nested")]
    pub info: Option<InfoReport>,
    /// Reply to a `system` command (`ledctrl`).
    #[serde(default, deserialize_with = "nested")]
    pub system: Option<CommandReply>,
}

/// The envelope fields every reply carries.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct CommandReply {
    /// The command this replies to.
    #[serde(default, deserialize_with = "text")]
    pub command: Option<String>,
    /// The request's sequence id.
    #[serde(default, deserialize_with = "text")]
    pub sequence_id: Option<String>,
    /// `success` or `fail`, in any letter case.
    #[serde(default, deserialize_with = "text")]
    pub result: Option<String>,
    /// The printer's reason for a failure.
    #[serde(default, deserialize_with = "text")]
    pub reason: Option<String>,
}

/// `print.*`: a status push (`command` = `push_status`) or the reply to a print command.
/// Printers of some generations send only the fields that changed, so every field is
/// optional.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct PrintReport {
    /// `push_status`, or the command a reply answers.
    #[serde(default, deserialize_with = "text")]
    pub command: Option<String>,
    /// Sequence id of the push or of the request answered.
    #[serde(default, deserialize_with = "text")]
    pub sequence_id: Option<String>,
    /// Reply only: `success` or `fail`.
    #[serde(default, deserialize_with = "text")]
    pub result: Option<String>,
    /// Reply only: the reason for a failure.
    #[serde(default, deserialize_with = "text")]
    pub reason: Option<String>,

    /// `IDLE`, `PREPARE`, `RUNNING`, `PAUSE`, `FINISH`, `FAILED`, `SLICING`, `INIT`.
    #[serde(default, deserialize_with = "text")]
    pub gcode_state: Option<String>,
    /// Current stage number (see [`crate::status::stage_name`]); -1 or 255 when idle.
    #[serde(default, deserialize_with = "integer")]
    pub stg_cur: Option<i64>,
    /// Progress in percent.
    #[serde(default, deserialize_with = "integer")]
    pub mc_percent: Option<i64>,
    /// Remaining time in minutes.
    #[serde(default, deserialize_with = "integer")]
    pub mc_remaining_time: Option<i64>,
    /// Current layer.
    #[serde(default, deserialize_with = "integer")]
    pub layer_num: Option<i64>,
    /// Layers of the job.
    #[serde(default, deserialize_with = "integer")]
    pub total_layer_num: Option<i64>,
    /// The file being printed.
    #[serde(default, deserialize_with = "text")]
    pub gcode_file: Option<String>,
    /// The job's name.
    #[serde(default, deserialize_with = "text")]
    pub subtask_name: Option<String>,
    /// `local`, `cloud` or empty.
    #[serde(default, deserialize_with = "text")]
    pub print_type: Option<String>,
    /// The print error number; 0 for none.
    #[serde(default, deserialize_with = "integer")]
    pub print_error: Option<i64>,
    /// Health messages.
    #[serde(default, deserialize_with = "list")]
    pub hms: Option<Vec<HmsReport>>,

    /// Single-nozzle firmware: nozzle temperature, °C.
    #[serde(default, deserialize_with = "float")]
    pub nozzle_temper: Option<f64>,
    /// Single-nozzle firmware: nozzle target, °C.
    #[serde(default, deserialize_with = "float")]
    pub nozzle_target_temper: Option<f64>,
    /// Bed temperature, °C (older firmware).
    #[serde(default, deserialize_with = "float")]
    pub bed_temper: Option<f64>,
    /// Bed target, °C (older firmware).
    #[serde(default, deserialize_with = "float")]
    pub bed_target_temper: Option<f64>,
    /// Chamber temperature, °C (older firmware).
    #[serde(default, deserialize_with = "float")]
    pub chamber_temper: Option<f64>,
    /// The `device` block of newer firmware: per-extruder state and packed temperatures.
    #[serde(default, deserialize_with = "nested")]
    pub device: Option<DeviceReport>,

    /// The AMS units and their trays.
    #[serde(default, deserialize_with = "nested")]
    pub ams: Option<AmsReport>,
    /// External spool of single-nozzle printers.
    #[serde(default, deserialize_with = "nested")]
    pub vt_tray: Option<TrayReport>,
    /// External spools of dual-nozzle printers (ids 254 and 255).
    #[serde(default, deserialize_with = "list")]
    pub vir_slot: Option<Vec<TrayReport>>,

    /// Speed preset number.
    #[serde(default, deserialize_with = "integer")]
    pub spd_lvl: Option<i64>,
    /// State of the lights.
    #[serde(default, deserialize_with = "list")]
    pub lights_report: Option<Vec<LightReport>>,
    /// Whether external storage is present.
    #[serde(default, deserialize_with = "flag")]
    pub sdcard: Option<bool>,
    /// Wi-Fi signal, for example `-45dBm`.
    #[serde(default, deserialize_with = "text")]
    pub wifi_signal: Option<String>,
}

/// One health message: two 32-bit halves of the HMS code.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct HmsReport {
    /// Upper half: module and part.
    #[serde(default, deserialize_with = "integer")]
    pub attr: Option<i64>,
    /// Lower half: severity and number.
    #[serde(default, deserialize_with = "integer")]
    pub code: Option<i64>,
}

/// One light.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct LightReport {
    /// `chamber_light`, `work_light`, ...
    #[serde(default, deserialize_with = "text")]
    pub node: Option<String>,
    /// `on`, `off` or `flashing`.
    #[serde(default, deserialize_with = "text")]
    pub mode: Option<String>,
}

/// `print.device` of the H2 / X2 generation (ha-bambulab `models.py`).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct DeviceReport {
    /// Per-extruder state.
    #[serde(default, deserialize_with = "nested")]
    pub extruder: Option<ExtruderBlock>,
    /// Bed: `info.temp` packs current (low 16 bits) and target (high 16 bits).
    #[serde(default, deserialize_with = "nested")]
    pub bed: Option<PackedBlock>,
    /// Chamber: `info.temp`, packed the same way.
    #[serde(default, deserialize_with = "nested")]
    pub ctc: Option<PackedBlock>,
}

/// `device.extruder`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct ExtruderBlock {
    /// One entry per extruder.
    #[serde(default, deserialize_with = "list")]
    pub info: Option<Vec<ExtruderReport>>,
    /// Bits 0-3: number of extruders; bits 4-7: the active extruder (15 while switching).
    #[serde(default, deserialize_with = "integer")]
    pub state: Option<i64>,
}

/// One extruder of `device.extruder.info`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct ExtruderReport {
    /// Device extruder id.
    #[serde(default, deserialize_with = "integer")]
    pub id: Option<i64>,
    /// Packed temperature: current in the low 16 bits, target in the high 16 bits.
    #[serde(default, deserialize_with = "integer")]
    pub temp: Option<i64>,
    /// The tray loaded now: AMS unit id in bits 8-15, tray in bits 0-7.
    #[serde(default, deserialize_with = "integer")]
    pub snow: Option<i64>,
    /// The tray being loaded, encoded like `snow`.
    #[serde(default, deserialize_with = "integer")]
    pub star: Option<i64>,
}

/// A block whose `info.temp` is a packed temperature.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct PackedBlock {
    /// The block's values.
    #[serde(default, deserialize_with = "nested")]
    pub info: Option<PackedInfo>,
}

/// `info` of a [`PackedBlock`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct PackedInfo {
    /// Current in the low 16 bits, target in the high 16 bits.
    #[serde(default, deserialize_with = "integer")]
    pub temp: Option<i64>,
}

/// `print.ams`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct AmsReport {
    /// The units.
    #[serde(default, deserialize_with = "list")]
    pub ams: Option<Vec<AmsUnitReport>>,
    /// Hex bitmask of trays that hold a spool: bit `unit * 4 + tray`.
    #[serde(default, deserialize_with = "hex")]
    pub tray_exist_bits: Option<u64>,
    /// Single-nozzle firmware: the active tray (`unit * 4 + tray`, 254 external, 255 none).
    #[serde(default, deserialize_with = "integer")]
    pub tray_now: Option<i64>,
}

/// One AMS unit.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct AmsUnitReport {
    /// Unit id: 0-3 regular, 128-135 AMS HT.
    #[serde(default, deserialize_with = "integer")]
    pub id: Option<i64>,
    /// Hex bitfield: hardware type in bits 0-3, the extruder the unit feeds in bits 8-11.
    #[serde(default, deserialize_with = "hex")]
    pub info: Option<u64>,
    /// Humidity index, 1 (wet) to 5 (dry).
    #[serde(default, deserialize_with = "integer")]
    pub humidity: Option<i64>,
    /// Temperature inside the unit, °C.
    #[serde(default, deserialize_with = "float")]
    pub temp: Option<f64>,
    /// Minutes of drying left; 0 when not drying.
    #[serde(default, deserialize_with = "integer")]
    pub dry_time: Option<i64>,
    /// The trays. An empty tray is a bare `{"id": n}`.
    #[serde(default, deserialize_with = "list")]
    pub tray: Option<Vec<TrayReport>>,
}

/// One tray of an AMS, or an external spool.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct TrayReport {
    /// Tray id within its unit; 254 or 255 for an external spool.
    #[serde(default, deserialize_with = "integer")]
    pub id: Option<i64>,
    /// Material, for example `PLA`.
    #[serde(default, deserialize_with = "text")]
    pub tray_type: Option<String>,
    /// Product line, for example `PLA Basic`.
    #[serde(default, deserialize_with = "text")]
    pub tray_sub_brands: Option<String>,
    /// Colour as `RRGGBBAA`.
    #[serde(default, deserialize_with = "text")]
    pub tray_color: Option<String>,
    /// All colours of a multi-colour spool, each `RRGGBBAA`.
    #[serde(default, deserialize_with = "texts")]
    pub cols: Option<Vec<String>>,
    /// Bambu filament profile id, for example `GFA00`.
    #[serde(default, deserialize_with = "text")]
    pub tray_info_idx: Option<String>,
    /// Percent of the spool left; -1 when unknown.
    #[serde(default, deserialize_with = "integer")]
    pub remain: Option<i64>,
    /// Lowest nozzle temperature of the filament, °C.
    #[serde(default, deserialize_with = "integer")]
    pub nozzle_temp_min: Option<i64>,
    /// Highest nozzle temperature of the filament, °C.
    #[serde(default, deserialize_with = "integer")]
    pub nozzle_temp_max: Option<i64>,
}

/// `info.get_version` reply.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct InfoReport {
    /// `get_version`.
    #[serde(default, deserialize_with = "text")]
    pub command: Option<String>,
    /// Firmware modules.
    #[serde(default, deserialize_with = "list")]
    pub module: Option<Vec<ModuleReport>>,
}

/// One firmware module of the version reply.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct ModuleReport {
    /// Module name, for example `ota`.
    #[serde(default, deserialize_with = "text")]
    pub name: Option<String>,
    /// Software version.
    #[serde(default, deserialize_with = "text")]
    pub sw_ver: Option<String>,
    /// Hardware version.
    #[serde(default, deserialize_with = "text")]
    pub hw_ver: Option<String>,
    /// Product name, on firmware that reports it.
    #[serde(default, deserialize_with = "text")]
    pub product_name: Option<String>,
}

/// Parses one payload from the report topic.
pub fn parse(payload: &[u8]) -> Result<ReportMessage, ReportError> {
    if payload.len() > MAX_REPORT_BYTES {
        return Err(ReportError::TooLarge(payload.len()));
    }
    // serde would also read a struct from a JSON array, by position.
    if payload.iter().find(|byte| !byte.is_ascii_whitespace()) != Some(&b'{') {
        return Err(ReportError::NotJson("not an object".to_owned()));
    }
    let mut message: ReportMessage = serde_json::from_slice(payload).map_err(|error| {
        // The error text can quote the payload's position only, never its content.
        ReportError::NotJson(format!("{:?} at line {}", error.classify(), error.line()))
    })?;
    if let Some(print) = &mut message.print {
        cap(&mut print.hms, MAX_HMS);
        cap(&mut print.vir_slot, 2);
        cap(&mut print.lights_report, MAX_SMALL_LIST);
        if let Some(ams) = &mut print.ams {
            cap(&mut ams.ams, MAX_AMS_UNITS);
            for unit in ams.ams.iter_mut().flatten() {
                cap(&mut unit.tray, MAX_TRAYS);
            }
        }
        if let Some(extruder) = print.device.as_mut().and_then(|d| d.extruder.as_mut()) {
            cap(&mut extruder.info, 4);
        }
    }
    if let Some(info) = &mut message.info {
        cap(&mut info.module, MAX_SMALL_LIST);
    }
    Ok(message)
}

fn cap<T>(list: &mut Option<Vec<T>>, max: usize) {
    if let Some(list) = list {
        list.truncate(max);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn a_status_push_is_read_with_strings_and_numbers_mixed() {
        let payload = br#"{"print": {
            "command": "push_status", "sequence_id": 2021, "gcode_state": "RUNNING",
            "mc_percent": "42", "mc_remaining_time": 17, "stg_cur": 0, "layer_num": 12,
            "total_layer_num": 50, "print_error": 0, "nozzle_temper": 219.6,
            "hms": [{"attr": 50331904, "code": 65543}, "junk", {"attr": "x"}],
            "ams": {"ams": [{"id": "0", "info": "1003", "humidity": "4", "temp": "22.7",
                     "tray": [{"id": "0"}, {"id": "1", "tray_type": "PLA", "remain": 80,
                               "tray_color": "DFE2E3FF", "cols": ["DFE2E3FF"],
                               "nozzle_temp_max": "240", "future_field": {"a": [1, 2]}}]}],
                    "tray_exist_bits": "e", "tray_now": "255"},
            "device": {"extruder": {"info": [{"id": 0, "temp": 14418140, "snow": 259},
                                             {"id": 1, "temp": 5767327, "snow": 3}], "state": 18},
                       "bed": {"info": {"temp": 6553700}}},
            "some_new_block": {"x": 1}, "sdcard": true}}"#;
        let print = parse(payload).unwrap().print.unwrap();
        assert_eq!(print.gcode_state.as_deref(), Some("RUNNING"));
        assert_eq!(print.sequence_id.as_deref(), Some("2021"));
        assert_eq!(print.mc_percent, Some(42));
        assert_eq!(print.nozzle_temper, Some(219.6));
        let hms = print.hms.unwrap();
        assert_eq!(hms.len(), 2, "the entry that is not an object is dropped");
        assert_eq!((hms[0].attr, hms[0].code), (Some(50_331_904), Some(65_543)));
        assert_eq!(hms[1].attr, None);
        let ams = print.ams.unwrap();
        assert_eq!(ams.tray_exist_bits, Some(0xe));
        let unit = &ams.ams.unwrap()[0];
        assert_eq!(
            (unit.id, unit.info, unit.temp),
            (Some(0), Some(0x1003), Some(22.7))
        );
        let trays = unit.tray.as_ref().unwrap();
        assert_eq!(trays[0].tray_type, None);
        assert_eq!(trays[1].nozzle_temp_max, Some(240));
        let extruders = print.device.unwrap().extruder.unwrap();
        assert_eq!(extruders.state, Some(18));
        assert_eq!(extruders.info.unwrap()[0].snow, Some(259));
        assert_eq!(print.sdcard, Some(true));
    }

    #[test]
    fn wrong_types_are_absent_not_fatal() {
        let payload = br#"{"print": {"gcode_state": ["RUNNING"], "mc_percent": {"a": 1},
            "ams": "none", "hms": 7, "device": [], "nozzle_temper": "NaN",
            "bed_temper": "1e999", "layer_num": 1e300, "vt_tray": null},
            "info": 3, "system": {"command": "ledctrl", "result": "SUCCESS"}}"#;
        let message = parse(payload).unwrap();
        let print = message.print.unwrap();
        assert_eq!(print, PrintReport::default());
        assert!(message.info.is_none());
        assert_eq!(message.system.unwrap().result.as_deref(), Some("SUCCESS"));
        // A message whose `print` is not an object has no print report.
        assert!(parse(br#"{"print": "x"}"#).unwrap().print.is_none());
    }

    #[test]
    fn size_depth_and_garbage_are_refused_without_panicking() {
        assert!(matches!(
            parse(&vec![b' '; MAX_REPORT_BYTES + 1]),
            Err(ReportError::TooLarge(_))
        ));
        let deep = format!("{}1{}", "[".repeat(100_000), "]".repeat(100_000));
        assert!(parse(deep.as_bytes()).is_err());
        let deep_object = format!("{{\"print\": {}{}}}", "{\"a\":".repeat(5_000), "1");
        assert!(parse(deep_object.as_bytes()).is_err());
        for garbage in [
            &b""[..],
            b"null",
            b"[]",
            b"7",
            b"{\"print\": ",
            b"\xff\xfe{}",
            b"{}{}",
        ] {
            assert!(parse(garbage).is_err(), "{garbage:?}");
        }
        assert_eq!(parse(b"{}").unwrap(), ReportMessage::default());
        // The error never repeats the payload.
        let error = parse(b"{\"print\": s3cr3t}").unwrap_err().to_string();
        assert!(!error.contains("s3cr3t"), "{error}");
    }

    #[test]
    fn lists_and_strings_are_cut() {
        let trays: Vec<String> = (0..60).map(|i| format!("{{\"id\": {i}}}")).collect();
        let units: Vec<String> = (0..60)
            .map(|i| format!("{{\"id\": {i}, \"tray\": [{}]}}", trays.join(",")))
            .collect();
        let hms: Vec<String> = (0..500).map(|i| format!("{{\"attr\": {i}}}")).collect();
        let name = format!("{}\u{1b}[31m{}", "a".repeat(10), "b".repeat(5000));
        let payload = format!(
            "{{\"print\": {{\"subtask_name\": {}, \"hms\": [{}], \"ams\": {{\"ams\": [{}]}}}}}}",
            serde_json::to_string(&name).unwrap(),
            hms.join(","),
            units.join(",")
        );
        let print = parse(payload.as_bytes()).unwrap().print.unwrap();
        let name = print.subtask_name.unwrap();
        assert_eq!(name.chars().count(), MAX_TEXT);
        assert!(!name.contains('\u{1b}'));
        assert_eq!(print.hms.unwrap().len(), MAX_HMS);
        let units = print.ams.unwrap().ams.unwrap();
        assert_eq!(units.len(), MAX_AMS_UNITS);
        assert_eq!(units[0].tray.as_ref().unwrap().len(), MAX_TRAYS);
    }
}
