//! The only requests Hotend can send to a printer: a closed list of typed commands.
//!
//! Guarantees:
//! - [`Command`] is the complete allowlist: start a prepared job, pause, resume, stop, set
//!   the speed preset, switch the chamber light, ask for a status push. There is no variant,
//!   constructor or serialisation path for raw G-code (`print.gcode_line`), for printing an
//!   arbitrary file on the printer (`print.gcode_file`), or for any firmware, calibration,
//!   AMS or camera command; a test serialises every variant and checks the wire text;
//! - [`Command::StartPreparedJob`] carries a [`StartCommand`], which only
//!   [`crate::printer::start_print`] can create, by consuming a [`crate::token::StartToken`].
//!   `StartCommand` cannot be cloned, deserialised or built from its parts;
//! - model differences of the wire format (flag types, key spellings, AMS ids) come from the
//!   printer profile, not from code.
//!
//! Wire formats follow OpenBambuAPI `mqtt.md` and ha-bambulab `pybambu/commands.py`.

use serde_json::{Map, Value, json};

use crate::profile::{FlagType, FlatHt, PrinterProfile};

/// Delivery guarantee of a request, as MQTT quality of service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Qos {
    /// Sent once; may be lost, never repeated.
    AtMostOnce,
    /// Repeated until acknowledged.
    AtLeastOnce,
}

/// One of the printer's four speed presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeedProfile {
    /// Quietest.
    Silent,
    /// The default.
    Standard,
    /// Faster.
    Sport,
    /// Fastest.
    Ludicrous,
}

/// Where one filament of the sliced project comes from on the printer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilamentSource {
    /// A tray of an AMS unit. `unit` is the unit's id as reported (0-3, or 128-135 for an
    /// AMS HT, whose only tray is 0).
    Ams {
        /// Unit id.
        unit: u8,
        /// Tray within the unit.
        tray: u8,
    },
    /// An external spool holder, by its tray id (254 or 255).
    External(u8),
    /// The plate does not use this filament.
    Unused,
}

/// The printer's own preparation steps for a job. Hotend asks for them; the printer does
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartOptions {
    /// Automatic bed levelling before the print.
    pub bed_leveling: bool,
    /// Flow calibration before the print.
    pub flow_calibration: bool,
    /// Vibration compensation before the print.
    pub vibration_calibration: bool,
    /// First-layer inspection.
    pub first_layer_inspection: bool,
    /// Record a timelapse.
    pub timelapse: bool,
}

impl Default for StartOptions {
    fn default() -> Self {
        Self {
            bed_leveling: true,
            flow_calibration: true,
            vibration_calibration: true,
            first_layer_inspection: true,
            timelapse: false,
        }
    }
}

/// The start of one uploaded, linted, human-confirmed job. Created only by
/// [`crate::printer::start_print`].
///
/// ```compile_fail
/// // There is no public constructor and the fields are private.
/// let _ = hotend_printer::protocol::command::StartCommand {
///     plate: 1,
///     remote_name: String::new(),
///     job_name: String::new(),
///     sources: Vec::new(),
///     options: Default::default(),
/// };
/// ```
#[derive(Debug)]
pub struct StartCommand {
    plate: u32,
    remote_name: String,
    job_name: String,
    sources: Vec<FilamentSource>,
    options: StartOptions,
}

impl StartCommand {
    pub(crate) fn new(
        plate: u32,
        remote_name: String,
        job_name: String,
        sources: Vec<FilamentSource>,
        options: StartOptions,
    ) -> Self {
        Self {
            plate,
            remote_name,
            job_name,
            sources,
            options,
        }
    }

    /// The plate to print.
    pub fn plate(&self) -> u32 {
        self.plate
    }

    /// Name of the uploaded file on the printer's storage.
    pub fn remote_name(&self) -> &str {
        &self.remote_name
    }

    /// The job's display name.
    pub fn job_name(&self) -> &str {
        &self.job_name
    }

    /// The source of each project filament, in slot order.
    pub fn sources(&self) -> &[FilamentSource] {
        &self.sources
    }

    /// The preparation steps asked for.
    pub fn options(&self) -> StartOptions {
        self.options
    }
}

/// Everything Hotend can ask a printer to do.
#[derive(Debug)]
pub enum Command {
    /// Start the uploaded job (`print.project_file`).
    StartPreparedJob(StartCommand),
    /// Pause the running print.
    Pause,
    /// Resume a paused print.
    Resume,
    /// Stop the print. It cannot be resumed.
    Stop,
    /// Choose a speed preset.
    SetSpeed(SpeedProfile),
    /// Switch the chamber light on (`true`) or off.
    Light(bool),
    /// Ask the printer for a complete status report.
    RequestStatus,
}

/// A request ready for the request topic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireMessage {
    /// The JSON text.
    pub payload: String,
    /// How it must be delivered.
    pub qos: Qos,
    /// The wire name of the command (`project_file`, `pause`, ...), for logs.
    pub command: &'static str,
}

impl Command {
    /// The wire name of the command.
    pub fn name(&self) -> &'static str {
        match self {
            Command::StartPreparedJob(_) => "project_file",
            Command::Pause => "pause",
            Command::Resume => "resume",
            Command::Stop => "stop",
            Command::SetSpeed(_) => "print_speed",
            Command::Light(_) => "ledctrl",
            Command::RequestStatus => "pushall",
        }
    }

    /// The request as JSON for a printer of `profile`'s family. `sequence_id` is the
    /// connection's running request number.
    pub fn to_wire(&self, sequence_id: u64, profile: &PrinterProfile) -> WireMessage {
        let sequence = sequence_id.to_string();
        let family = profile.family();
        // Pause, resume and stop go out at QoS 1 (OpenBambuAPI). A start goes out at most
        // once on purpose: a print start must never be delivered twice.
        let (body, qos) = match self {
            Command::Pause | Command::Resume | Command::Stop => (
                json!({"print": {"sequence_id": sequence, "command": self.name(), "param": ""}}),
                Qos::AtLeastOnce,
            ),
            Command::SetSpeed(speed) => {
                let levels = &family.speed_levels;
                let level = match speed {
                    SpeedProfile::Silent => levels.silent,
                    SpeedProfile::Standard => levels.standard,
                    SpeedProfile::Sport => levels.sport,
                    SpeedProfile::Ludicrous => levels.ludicrous,
                };
                (
                    json!({"print": {"sequence_id": sequence, "command": self.name(),
                                     "param": level.to_string()}}),
                    Qos::AtMostOnce,
                )
            }
            Command::Light(on) => (
                json!({"system": {"sequence_id": sequence, "command": self.name(),
                                  "led_node": "chamber_light",
                                  "led_mode": if *on { "on" } else { "off" },
                                  "led_on_time": 500, "led_off_time": 500,
                                  "loop_times": 0, "interval_time": 0}}),
                Qos::AtMostOnce,
            ),
            Command::RequestStatus => (
                json!({"pushing": {"sequence_id": sequence, "command": self.name(),
                                   "version": 1, "push_target": 1}}),
                Qos::AtMostOnce,
            ),
            Command::StartPreparedJob(start) => (
                json!({"print": project_file(start, sequence, profile)}),
                Qos::AtMostOnce,
            ),
        };
        WireMessage {
            payload: body.to_string(),
            qos,
            command: self.name(),
        }
    }
}

/// The value of one filament in the flat `ams_mapping` array.
pub(crate) fn flat_source(source: FilamentSource, profile: &PrinterProfile) -> i64 {
    let rules = &profile.family().ams;
    match source {
        FilamentSource::Unused => -1,
        FilamentSource::External(id) => i64::from(id),
        FilamentSource::Ams { unit, tray } if rules.is_ht(unit) => match rules.flat_ht {
            FlatHt::UnitId => i64::from(unit),
            FlatHt::Slot16 => 16 + i64::from(unit - rules.ht_unit_id_base) + i64::from(tray),
        },
        FilamentSource::Ams { unit, tray } => {
            i64::from(unit) * i64::from(rules.trays_per_unit) + i64::from(tray)
        }
    }
}

fn project_file(start: &StartCommand, sequence: String, profile: &PrinterProfile) -> Value {
    let family = profile.family();
    let rules = &family.project_file;
    let flag = |on: bool| match rules.flag_type {
        FlagType::Boolean => Value::Bool(on),
        FlagType::Integer => Value::from(u8::from(on)),
    };
    let options = start.options;
    let mut body = Map::new();
    let mut put = |key: &str, value: Value| {
        body.insert(key.to_owned(), value);
    };
    put("sequence_id", Value::String(sequence));
    put("command", Value::from("project_file"));
    put(
        "param",
        Value::String(format!("Metadata/plate_{}.gcode", start.plate)),
    );
    // Always "0" for a local print.
    for key in ["project_id", "profile_id", "task_id", "subtask_id"] {
        put(key, Value::from("0"));
    }
    put("subtask_name", Value::String(start.job_name.clone()));
    put("file", Value::from(""));
    put(
        "url",
        Value::String(format!(
            "{}{}",
            family.storage.url_prefix, start.remote_name
        )),
    );
    put("md5", Value::from(""));
    put("bed_type", Value::from("auto"));
    put("timelapse", Value::Bool(options.timelapse));
    put(&rules.bed_leveling_key, flag(options.bed_leveling));
    put("flow_cali", flag(options.flow_calibration));
    put("vibration_cali", flag(options.vibration_calibration));
    put("layer_inspect", flag(options.first_layer_inspection));
    put(
        "use_ams",
        Value::Bool(
            start
                .sources
                .iter()
                .any(|source| matches!(source, FilamentSource::Ams { .. })),
        ),
    );
    put(
        "ams_mapping",
        Value::Array(
            start
                .sources
                .iter()
                .map(|source| Value::from(flat_source(*source, profile)))
                .collect(),
        ),
    );
    if rules.send_ams_mapping2 {
        let pairs = start.sources.iter().map(|source| match *source {
            FilamentSource::Ams { unit, tray } => json!({"ams_id": unit, "slot_id": tray}),
            FilamentSource::External(id) => json!({"ams_id": id, "slot_id": 0}),
            FilamentSource::Unused => json!({"ams_id": 255, "slot_id": 255}),
        });
        put("ams_mapping2", Value::Array(pairs.collect()));
    }
    Value::Object(body)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::profile::Profiles;

    fn x2d() -> PrinterProfile {
        Profiles::builtin().unwrap().printer("bambu-x2d").unwrap()
    }

    fn start() -> StartCommand {
        StartCommand::new(
            1,
            "cable-box.gcode.3mf".to_owned(),
            "cable-box".to_owned(),
            vec![
                FilamentSource::Ams { unit: 0, tray: 2 },
                FilamentSource::Ams { unit: 128, tray: 0 },
                FilamentSource::External(254),
                FilamentSource::Unused,
            ],
            StartOptions::default(),
        )
    }

    /// One value of every variant. The `match` has no wildcard, so a new variant does not
    /// compile until it is added here and covered by the scan below.
    fn every_command() -> Vec<Command> {
        let mut all = Vec::new();
        let template = [
            Command::StartPreparedJob(start()),
            Command::Pause,
            Command::Resume,
            Command::Stop,
            Command::SetSpeed(SpeedProfile::Silent),
            Command::Light(true),
            Command::RequestStatus,
        ];
        for command in template {
            match command {
                Command::StartPreparedJob(_)
                | Command::Pause
                | Command::Resume
                | Command::Stop
                | Command::RequestStatus => all.push(command),
                Command::SetSpeed(_) => {
                    for speed in [
                        SpeedProfile::Silent,
                        SpeedProfile::Standard,
                        SpeedProfile::Sport,
                        SpeedProfile::Ludicrous,
                    ] {
                        all.push(Command::SetSpeed(speed));
                    }
                }
                Command::Light(_) => {
                    all.push(Command::Light(true));
                    all.push(Command::Light(false));
                }
            }
        }
        all
    }

    #[test]
    fn no_command_can_produce_raw_gcode_or_anything_off_the_allowlist() {
        let allowed = [
            "project_file",
            "pause",
            "resume",
            "stop",
            "print_speed",
            "ledctrl",
            "pushall",
        ];
        let profile = x2d();
        let mut seen = Vec::new();
        for (sequence, command) in every_command().iter().enumerate() {
            let wire = command.to_wire(sequence as u64, &profile);
            let text = wire.payload.to_ascii_lowercase();
            for forbidden in [
                "gcode_line",
                "gcode_file",
                "upgrade",
                "calibration",
                "get_access_code",
            ] {
                assert!(!text.contains(forbidden), "{forbidden} in {text}");
            }
            let value: Value = serde_json::from_str(&wire.payload).unwrap();
            let object = value.as_object().unwrap();
            assert_eq!(object.len(), 1, "one message family per request");
            let (family, body) = object.iter().next().unwrap();
            let name = body["command"].as_str().unwrap();
            assert!(allowed.contains(&name), "{name}");
            assert_eq!(name, wire.command);
            assert_eq!(body["sequence_id"], sequence.to_string());
            let expected_family = match name {
                "ledctrl" => "system",
                "pushall" => "pushing",
                _ => "print",
            };
            assert_eq!(family, expected_family);
            seen.push(name.to_owned());
        }
        for name in allowed {
            assert!(seen.iter().any(|s| s == name), "{name} never produced");
        }
    }

    #[test]
    fn pause_resume_and_stop_are_acknowledged_and_a_start_is_never_repeated() {
        let profile = x2d();
        for command in [Command::Pause, Command::Resume, Command::Stop] {
            let wire = command.to_wire(7, &profile);
            assert_eq!(wire.qos, Qos::AtLeastOnce);
            let value: Value = serde_json::from_str(&wire.payload).unwrap();
            assert_eq!(value["print"]["param"], "");
        }
        assert_eq!(
            Command::StartPreparedJob(start()).to_wire(1, &profile).qos,
            Qos::AtMostOnce
        );
        let speed: Value = serde_json::from_str(
            &Command::SetSpeed(SpeedProfile::Sport)
                .to_wire(1, &profile)
                .payload,
        )
        .unwrap();
        assert_eq!(speed["print"]["param"], "3");
        let light: Value =
            serde_json::from_str(&Command::Light(false).to_wire(1, &profile).payload).unwrap();
        assert_eq!(light["system"]["led_node"], "chamber_light");
        assert_eq!(light["system"]["led_mode"], "off");
    }

    #[test]
    fn project_file_follows_the_rules_from_the_profile() {
        let profile = x2d();
        let wire = Command::StartPreparedJob(start()).to_wire(41, &profile);
        let value: Value = serde_json::from_str(&wire.payload).unwrap();
        let body = &value["print"];
        assert_eq!(body["param"], "Metadata/plate_1.gcode");
        assert_eq!(body["url"], "ftp:///cable-box.gcode.3mf");
        assert_eq!(body["subtask_name"], "cable-box");
        assert_eq!(body["bed_leveling"], true);
        assert_eq!(body["flow_cali"], true);
        assert_eq!(body["layer_inspect"], true);
        assert_eq!(body["timelapse"], false);
        assert_eq!(body["use_ams"], true);
        // Regular tray: unit * 4 + tray. AMS HT: its unit id. External: its id. Unused: -1.
        // An AMS HT follows the regular units at 16-23, as ha-bambulab models.py records
        // of `print.ams_mapping`; the bare unit id (128) is no encoding it has seen.
        assert_eq!(body["ams_mapping"], json!([2, 16, 254, -1]));
        assert!(body.get("ams_mapping2").is_none());
        for key in ["project_id", "profile_id", "task_id", "subtask_id"] {
            assert_eq!(body[key], "0");
        }
    }

    #[test]
    fn another_generation_is_only_a_change_of_data() {
        let text = include_str!("../../profiles/bambu.toml")
            .replace("flag_type = \"boolean\"", "flag_type = \"integer\"")
            .replace(
                "bed_leveling_key = \"bed_leveling\"",
                "bed_leveling_key = \"bed_levelling\"",
            )
            .replace("send_ams_mapping2 = false", "send_ams_mapping2 = true")
            .replace("flat_ht = \"slot_16\"", "flat_ht = \"unit_id\"");
        let profile = Profiles::from_toml(&text)
            .unwrap()
            .printer("bambu-x2d")
            .unwrap();
        let wire = Command::StartPreparedJob(start()).to_wire(1, &profile);
        let value: Value = serde_json::from_str(&wire.payload).unwrap();
        let body = &value["print"];
        assert_eq!(body["bed_levelling"], 1);
        assert_eq!(body["layer_inspect"], 1);
        assert!(body.get("bed_leveling").is_none());
        assert_eq!(
            body["ams_mapping2"],
            json!([
                {"ams_id": 0, "slot_id": 2},
                {"ams_id": 128, "slot_id": 0},
                {"ams_id": 254, "slot_id": 0},
                {"ams_id": 255, "slot_id": 255}
            ])
        );
        assert_eq!(body["ams_mapping"], json!([2, 128, 254, -1]));
    }
}
