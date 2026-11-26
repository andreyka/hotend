//! The printer's state as Hotend sees it, built by merging report pushes.
//!
//! [`PrinterStatus::apply`] takes one parsed report and updates only what the report
//! carries, because some firmware sends complete reports and some only what changed.
//! Everything here is a plain value; nothing is trusted beyond what
//! [`crate::protocol::report`] already cut and cleaned.
//!
//! Nozzles are keyed by device extruder id, AMS units by their reported id. Which nozzle an
//! AMS unit feeds comes from the unit's `info` bits (see [`crate::profile::AmsRules`]).
//! Stage numbers and their meaning follow ha-bambulab `const.py` (`CURRENT_STAGE_IDS`).

use std::collections::BTreeMap;

use serde::Serialize;

use crate::profile::PrinterProfile;
use crate::protocol::hms::HmsCode;
use crate::protocol::report::{PrintReport, TrayReport};

/// The printer's own job state (`gcode_state`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GcodeState {
    /// Nothing is running.
    Idle,
    /// A job was accepted and is being prepared (also `SLICING` and `INIT`).
    Prepare,
    /// A job is running (heating, calibrating or printing).
    Running,
    /// The job is paused.
    Pause,
    /// The last job finished.
    Finish,
    /// The last job failed or was cancelled.
    Failed,
    /// The printer says it is offline.
    Offline,
    /// No report has said yet, or the value is new to this version.
    Unknown,
}

impl GcodeState {
    fn from_wire(text: &str) -> Self {
        match text.to_ascii_uppercase().as_str() {
            "IDLE" => Self::Idle,
            "PREPARE" | "SLICING" | "INIT" => Self::Prepare,
            "RUNNING" => Self::Running,
            "PAUSE" => Self::Pause,
            "FINISH" => Self::Finish,
            "FAILED" => Self::Failed,
            "OFFLINE" => Self::Offline,
            _ => Self::Unknown,
        }
    }
}

/// What the printer is doing, in the words the interface uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Nothing is running.
    Idle,
    /// A job was accepted: checks, homing, levelling, calibration, loading filament.
    Preparing,
    /// Waiting for the bed, a nozzle or the chamber to reach temperature.
    Heating,
    /// Laying down plastic.
    Printing,
    /// Paused, by the user or by the printer.
    Paused,
    /// The job finished.
    Finished,
    /// The job failed or was cancelled.
    Failed,
    /// The printer is offline.
    Offline,
    /// Not known yet.
    Unknown,
}

/// Stage numbers during which the printer only waits for a temperature.
const HEATING_STAGES: [i64; 6] = [2, 7, 49, 54, 58, 63];
/// Stage numbers of printing itself, or of "no stage".
const PRINTING_STAGES: [i64; 3] = [0, -1, 255];

/// The printer's name for a stage number (ha-bambulab `CURRENT_STAGE_IDS`), or `"unknown"`.
pub fn stage_name(stage: i64) -> &'static str {
    match stage {
        -1 | 255 => "idle",
        0 => "printing",
        1 => "auto_bed_leveling",
        2 => "heatbed_preheating",
        3 => "vibration_compensation",
        4 => "changing_filament",
        5 => "m400_pause",
        6 => "paused_filament_runout",
        7 => "heating_hotend",
        8 => "calibrating_extrusion",
        9 => "scanning_bed_surface",
        10 => "inspecting_first_layer",
        11 => "identifying_build_plate_type",
        13 => "homing_toolhead",
        14 => "cleaning_nozzle_tip",
        15 => "checking_extruder_temperature",
        16 => "paused_user",
        17 => "paused_front_cover_falling",
        19 => "calibrating_extrusion_flow",
        20 => "paused_nozzle_temperature_malfunction",
        21 => "paused_heat_bed_temperature_malfunction",
        22 => "filament_unloading",
        23 => "paused_skipped_step",
        24 => "filament_loading",
        26 => "paused_ams_lost",
        27 => "paused_low_fan_speed_heat_break",
        28 => "paused_chamber_temperature_control_error",
        29 => "cooling_chamber",
        30 => "paused_user_gcode",
        32 => "paused_nozzle_filament_covered_detected",
        33 => "paused_cutter_error",
        34 => "paused_first_layer_error",
        35 => "paused_nozzle_clog",
        39 => "calibrate_nozzle_offset",
        49 => "heating_chamber",
        52 => "check_material",
        54 => "waiting_for_heatbed_temperature",
        58 => "thermal_preconditioning",
        63 => "waiting_chamber_temperature_equalize",
        64 => "preparing_hotend",
        76 => "pre_extrusion_before_printing",
        77 => "preparing_ams",
        _ => "unknown",
    }
}

/// A measured temperature and its target, °C.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Temperature {
    /// The reading; `None` until reported.
    pub current: Option<f64>,
    /// The target; 0 means the heater is off.
    pub target: Option<f64>,
}

impl Temperature {
    /// Unpacks the H2 generation's packed value: current in the low 16 bits, target in the
    /// high 16 bits.
    fn from_packed(packed: i64) -> Option<Self> {
        let packed = u32::try_from(packed).ok()?;
        Some(Self {
            current: Some(f64::from(packed & 0xFFFF)),
            target: Some(f64::from(packed >> 16)),
        })
    }
}

/// An RGBA colour. Serialised as the CSS colour `#RRGGBBAA`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgba(pub [u8; 4]);

impl Serialize for Rgba {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!("#{}", self.hex()))
    }
}

impl Rgba {
    /// Parses `RRGGBBAA` or `RRGGBB` (alpha 255), with or without a leading `#`.
    pub fn parse(text: &str) -> Option<Self> {
        let hex = text.trim().trim_start_matches('#');
        if !hex.is_ascii() || !(hex.len() == 6 || hex.len() == 8) {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        Some(Self([
            byte(0)?,
            byte(2)?,
            byte(4)?,
            if hex.len() == 8 { byte(6)? } else { 255 },
        ]))
    }

    /// The colour as `RRGGBBAA`.
    pub fn hex(&self) -> String {
        let [r, g, b, a] = self.0;
        format!("{r:02X}{g:02X}{b:02X}{a:02X}")
    }
}

/// One tray of an AMS unit, or an external spool holder.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Tray {
    /// Tray id within its unit (0-3), or 254 / 255 for an external holder.
    pub id: u8,
    /// Material, for example `PLA`. `None` for an empty tray.
    pub material: Option<String>,
    /// Product line, for example `PLA Basic`.
    pub product: Option<String>,
    /// Main colour.
    pub colour: Option<Rgba>,
    /// All colours of a multi-colour spool.
    pub colours: Vec<Rgba>,
    /// Bambu filament profile id, for example `GFA00`.
    pub profile_id: Option<String>,
    /// Percent of the spool left; `None` when the printer does not know.
    pub remaining_percent: Option<u8>,
    /// The spool's own lowest nozzle temperature, °C.
    pub nozzle_min_c: Option<u16>,
    /// The spool's own highest nozzle temperature, °C.
    pub nozzle_max_c: Option<u16>,
}

impl Tray {
    /// Whether the tray holds a filament the printer knows the material of.
    pub fn is_loaded(&self) -> bool {
        self.material.as_deref().is_some_and(|m| !m.is_empty())
    }

    fn from_report(id: u8, report: &TrayReport) -> Self {
        let text = |value: &Option<String>| value.clone().filter(|text| !text.is_empty());
        let degrees =
            |value: Option<i64>| value.and_then(|v| u16::try_from(v).ok()).filter(|v| *v > 0);
        Self {
            id,
            material: text(&report.tray_type),
            product: text(&report.tray_sub_brands),
            colour: report.tray_color.as_deref().and_then(Rgba::parse),
            colours: report
                .cols
                .iter()
                .flatten()
                .filter_map(|c| Rgba::parse(c))
                .collect(),
            profile_id: text(&report.tray_info_idx),
            remaining_percent: report
                .remain
                .and_then(|v| u8::try_from(v).ok())
                .filter(|v| *v <= 100),
            nozzle_min_c: degrees(report.nozzle_temp_min),
            nozzle_max_c: degrees(report.nozzle_temp_max),
        }
    }
}

/// One AMS unit.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AmsUnit {
    /// Unit id: 0-3 regular, 128-135 AMS HT.
    pub id: u8,
    /// Whether this is an AMS HT (one tray).
    pub high_temperature: bool,
    /// Device extruder id of the nozzle the unit feeds. `None`: not reported, or the unit
    /// is not bound to one nozzle.
    pub nozzle: Option<u8>,
    /// Humidity index, 1 (wet) to 5 (dry).
    pub humidity: Option<u8>,
    /// Temperature inside the unit, °C.
    pub temperature: Option<f64>,
    /// Minutes of filament drying left; `None` or 0 when not drying.
    pub drying_minutes_left: Option<u32>,
    /// The trays, by id.
    pub trays: Vec<Tray>,
}

impl AmsUnit {
    /// Whether the unit is drying filament now.
    pub fn is_drying(&self) -> bool {
        self.drying_minutes_left.is_some_and(|minutes| minutes > 0)
    }

    /// The tray with this id.
    pub fn tray(&self, id: u8) -> Option<&Tray> {
        self.trays.iter().find(|tray| tray.id == id)
    }
}

/// Everything the printer has reported, merged.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PrinterStatus {
    /// The printer's own job state.
    pub gcode_state: GcodeState,
    /// Current stage number; see [`stage_name`].
    pub stage: Option<i64>,
    /// Progress of the job in percent.
    pub progress_percent: Option<u8>,
    /// Minutes the printer expects the job to take from now.
    pub remaining_minutes: Option<u32>,
    /// Current layer.
    pub layer: Option<u32>,
    /// Layers of the job.
    pub total_layers: Option<u32>,
    /// Name of the job.
    pub job_name: Option<String>,
    /// File being printed.
    pub gcode_file: Option<String>,
    /// Nozzle temperatures by device extruder id.
    pub nozzles: BTreeMap<u8, Temperature>,
    /// The nozzle in use (device extruder id), on printers that report it.
    pub active_nozzle: Option<u8>,
    /// Bed temperature.
    pub bed: Temperature,
    /// Chamber temperature.
    pub chamber: Temperature,
    /// The print error number, when not zero.
    pub print_error: Option<u32>,
    /// Health messages in force.
    pub hms: Vec<HmsCode>,
    /// AMS units by id.
    pub ams: Vec<AmsUnit>,
    /// External spool holders.
    pub external: Vec<Tray>,
    /// Speed preset number.
    pub speed_level: Option<u8>,
    /// Whether the chamber light is on.
    pub chamber_light: Option<bool>,
    /// Whether external storage is present.
    pub storage_present: Option<bool>,
    /// How many status reports were merged.
    pub reports: u64,
}

impl Default for PrinterStatus {
    fn default() -> Self {
        Self {
            gcode_state: GcodeState::Unknown,
            stage: None,
            progress_percent: None,
            remaining_minutes: None,
            layer: None,
            total_layers: None,
            job_name: None,
            gcode_file: None,
            nozzles: BTreeMap::new(),
            active_nozzle: None,
            bed: Temperature::default(),
            chamber: Temperature::default(),
            print_error: None,
            hms: Vec::new(),
            ams: Vec::new(),
            external: Vec::new(),
            speed_level: None,
            chamber_light: None,
            storage_present: None,
            reports: 0,
        }
    }
}

fn small<T: TryFrom<i64>>(value: Option<i64>) -> Option<T> {
    value.and_then(|v| T::try_from(v).ok())
}

impl PrinterStatus {
    /// Merges one `print` report. Replies to commands (anything whose `command` is not
    /// `push_status`) carry no state and are ignored.
    pub fn apply(&mut self, report: &PrintReport, profile: &PrinterProfile) {
        if report
            .command
            .as_deref()
            .is_some_and(|c| c != "push_status")
        {
            return;
        }
        self.reports = self.reports.saturating_add(1);
        if let Some(state) = &report.gcode_state {
            self.gcode_state = GcodeState::from_wire(state);
        }
        if let Some(stage) = report.stg_cur {
            self.stage = Some(stage);
        }
        if let Some(percent) = report.mc_percent {
            self.progress_percent = Some(percent.clamp(0, 100) as u8);
        }
        if let Some(minutes) = small::<u32>(report.mc_remaining_time) {
            self.remaining_minutes = Some(minutes);
        }
        if let Some(layer) = small::<u32>(report.layer_num) {
            self.layer = Some(layer);
        }
        if let Some(layers) = small::<u32>(report.total_layer_num) {
            self.total_layers = Some(layers);
        }
        if let Some(name) = &report.subtask_name {
            self.job_name = Some(name.clone()).filter(|name| !name.is_empty());
        }
        if let Some(file) = &report.gcode_file {
            self.gcode_file = Some(file.clone()).filter(|file| !file.is_empty());
        }
        if let Some(error) = report.print_error {
            self.print_error = u32::try_from(error).ok().filter(|code| *code != 0);
        }
        if let Some(entries) = &report.hms {
            self.hms = entries
                .iter()
                .filter_map(|entry| HmsCode::from_report(entry.attr, entry.code))
                .collect();
        }
        self.apply_temperatures(report, profile);
        self.apply_ams(report, profile);
        if let Some(level) = small::<u8>(report.spd_lvl) {
            self.speed_level = Some(level);
        }
        if let Some(lights) = &report.lights_report {
            for light in lights {
                if light.node.as_deref() == Some("chamber_light") {
                    self.chamber_light = light.mode.as_deref().map(|mode| mode != "off");
                }
            }
        }
        if let Some(present) = report.sdcard {
            self.storage_present = Some(present);
        }
    }

    fn apply_temperatures(&mut self, report: &PrintReport, profile: &PrinterProfile) {
        let device = report.device.as_ref();
        let extruders = device.and_then(|d| d.extruder.as_ref());
        if let Some(entries) = extruders.and_then(|block| block.info.as_ref()) {
            for entry in entries {
                let Some(id) = small::<u8>(entry.id) else {
                    continue;
                };
                if profile.nozzle_by_device_id(id).is_none() {
                    continue;
                }
                if let Some(temperature) = entry.temp.and_then(Temperature::from_packed) {
                    self.nozzles.insert(id, temperature);
                }
            }
        } else if profile.nozzles().len() == 1 {
            // The top-level fields describe "the" nozzle. On a dual-nozzle printer they
            // follow whichever nozzle is active, so they are not attributed to an id.
            let id = profile.nozzles()[0].device_extruder;
            let entry = self.nozzles.entry(id).or_default();
            if report.nozzle_temper.is_some() {
                entry.current = report.nozzle_temper;
            }
            if report.nozzle_target_temper.is_some() {
                entry.target = report.nozzle_target_temper;
            }
        }
        if let Some(state) = extruders.and_then(|block| block.state) {
            // Bits 4-7; 15 while the printer switches nozzles: keep the last known one.
            let active = ((state >> 4) & 0xF) as u8;
            if profile.nozzle_by_device_id(active).is_some() {
                self.active_nozzle = Some(active);
            }
        }
        let packed = |block: Option<&crate::protocol::report::PackedBlock>| {
            block
                .and_then(|b| b.info.as_ref())
                .and_then(|info| info.temp)
                .and_then(Temperature::from_packed)
        };
        if let Some(bed) = packed(device.and_then(|d| d.bed.as_ref())) {
            self.bed = bed;
        } else {
            if report.bed_temper.is_some() {
                self.bed.current = report.bed_temper;
            }
            if report.bed_target_temper.is_some() {
                self.bed.target = report.bed_target_temper;
            }
        }
        if let Some(chamber) = packed(device.and_then(|d| d.ctc.as_ref())) {
            self.chamber = chamber;
        } else if report.chamber_temper.is_some() {
            self.chamber.current = report.chamber_temper;
        }
    }

    fn apply_ams(&mut self, report: &PrintReport, profile: &PrinterProfile) {
        let rules = &profile.family().ams;
        if let Some(units) = report.ams.as_ref().and_then(|ams| ams.ams.as_ref()) {
            for unit_report in units {
                let Some(id) = small::<u8>(unit_report.id) else {
                    continue;
                };
                let index = match self.ams.iter().position(|unit| unit.id == id) {
                    Some(index) => index,
                    None if self.ams.len() < crate::protocol::report::MAX_AMS_UNITS => {
                        self.ams.push(AmsUnit {
                            id,
                            high_temperature: rules.is_ht(id),
                            ..AmsUnit::default()
                        });
                        self.ams.len() - 1
                    }
                    None => continue,
                };
                let unit = &mut self.ams[index];
                if let Some(info) = unit_report.info {
                    unit.nozzle = rules
                        .nozzle_of_info(info)
                        .filter(|nozzle| profile.nozzle_by_device_id(*nozzle).is_some());
                }
                if let Some(humidity) = small::<u8>(unit_report.humidity) {
                    unit.humidity = Some(humidity);
                }
                if unit_report.temp.is_some() {
                    unit.temperature = unit_report.temp;
                }
                if let Some(minutes) = small::<u32>(unit_report.dry_time) {
                    unit.drying_minutes_left = Some(minutes);
                }
                for tray_report in unit_report.tray.iter().flatten() {
                    let Some(tray_id) = small::<u8>(tray_report.id) else {
                        continue;
                    };
                    let tray = Tray::from_report(tray_id, tray_report);
                    match unit.trays.iter().position(|known| known.id == tray_id) {
                        Some(at) => unit.trays[at] = tray,
                        None if unit.trays.len() < crate::protocol::report::MAX_TRAYS => {
                            unit.trays.push(tray);
                        }
                        None => {}
                    }
                }
                unit.trays.sort_by_key(|tray| tray.id);
            }
            self.ams.sort_by_key(|unit| unit.id);
        }
        let external: Vec<&TrayReport> = report
            .vir_slot
            .iter()
            .flatten()
            .chain(report.vt_tray.as_ref())
            .collect();
        for tray_report in external {
            let Some(id) = small::<u8>(tray_report.id).filter(|id| rules.external_ids.contains(id))
            else {
                continue;
            };
            let tray = Tray::from_report(id, tray_report);
            match self.external.iter_mut().find(|known| known.id == id) {
                Some(known) => *known = tray,
                None => self.external.push(tray),
            }
        }
    }

    /// What the printer is doing, in the interface's words.
    pub fn phase(&self) -> Phase {
        match self.gcode_state {
            GcodeState::Idle => Phase::Idle,
            GcodeState::Prepare => Phase::Preparing,
            GcodeState::Pause => Phase::Paused,
            GcodeState::Finish => Phase::Finished,
            GcodeState::Failed => Phase::Failed,
            GcodeState::Offline => Phase::Offline,
            GcodeState::Unknown => Phase::Unknown,
            GcodeState::Running => match self.stage {
                Some(stage) if HEATING_STAGES.contains(&stage) => Phase::Heating,
                Some(stage) if PRINTING_STAGES.contains(&stage) => Phase::Printing,
                None => Phase::Printing,
                // Calibration and loading before the first layer is preparation; the same
                // stages in the middle of a print (a filament change) are part of printing.
                Some(_) if self.layer.unwrap_or(0) == 0 => Phase::Preparing,
                Some(_) => Phase::Printing,
            },
        }
    }

    /// The AMS unit with this id.
    pub fn ams_unit(&self, id: u8) -> Option<&AmsUnit> {
        self.ams.iter().find(|unit| unit.id == id)
    }

    /// Whether any AMS unit is drying filament.
    pub fn ams_drying(&self) -> bool {
        self.ams.iter().any(AmsUnit::is_drying)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::profile::Profiles;
    use crate::protocol::report::parse;

    fn x2d() -> PrinterProfile {
        Profiles::builtin().unwrap().printer("bambu-x2d").unwrap()
    }

    fn apply(status: &mut PrinterStatus, json: &str) {
        let message = parse(json.as_bytes()).unwrap();
        status.apply(&message.print.unwrap(), &x2d());
    }

    #[test]
    fn dual_nozzle_temperatures_and_ams_routing_are_keyed_by_device_id() {
        let mut status = PrinterStatus::default();
        apply(
            &mut status,
            r#"{"print": {"command": "push_status", "gcode_state": "RUNNING", "stg_cur": 0,
                "mc_percent": 30, "layer_num": 4,
                "device": {"extruder": {"info": [
                    {"id": 0, "temp": 14418140}, {"id": 1, "temp": 5767327}, {"id": 9, "temp": 1}],
                    "state": 18},
                  "bed": {"info": {"temp": 3932220}}, "ctc": {"info": {"temp": 43}}},
                "ams": {"ams": [
                    {"id": "0", "info": "1103", "tray": [{"id": "0", "tray_type": "PLA",
                        "tray_color": "FF0000FF", "cols": ["FF0000FF", "bogus"], "remain": 64,
                        "tray_info_idx": "GFA00", "nozzle_temp_min": "190", "nozzle_temp_max": "240"},
                        {"id": "1"}]},
                    {"id": "128", "info": "2004", "dry_time": 95, "tray": [{"id": "0",
                        "tray_type": "PA-CF", "remain": -1}]},
                    {"id": "1", "info": "E03", "tray": []}]},
                "vir_slot": [{"id": "254", "tray_type": "PETG", "tray_color": "00FF00FF"},
                             {"id": "255"}, {"id": "7", "tray_type": "ABS"}]}}"#,
        );
        // 14418140 = 220 << 16 | 220; 5767327 = 88 << 16 | 159.
        assert_eq!(
            status.nozzles[&0],
            Temperature {
                current: Some(220.0),
                target: Some(220.0)
            }
        );
        assert_eq!(
            status.nozzles[&1],
            Temperature {
                current: Some(159.0),
                target: Some(88.0)
            }
        );
        assert!(!status.nozzles.contains_key(&9));
        assert_eq!(status.active_nozzle, Some(1));
        assert_eq!(
            status.bed,
            Temperature {
                current: Some(60.0),
                target: Some(60.0)
            }
        );
        assert_eq!(status.chamber.current, Some(43.0));
        assert_eq!(status.phase(), Phase::Printing);

        let regular = status.ams_unit(0).unwrap();
        assert_eq!(regular.nozzle, Some(1), "info bits 8-11");
        assert!(!regular.high_temperature);
        let tray = regular.tray(0).unwrap();
        assert_eq!(tray.material.as_deref(), Some("PLA"));
        assert_eq!(tray.colour.unwrap().hex(), "FF0000FF");
        assert_eq!(tray.colours.len(), 1);
        assert_eq!(
            (tray.remaining_percent, tray.nozzle_max_c),
            (Some(64), Some(240))
        );
        assert!(!regular.tray(1).unwrap().is_loaded());
        let ht = status.ams_unit(128).unwrap();
        assert!(ht.high_temperature && ht.is_drying());
        assert_eq!(ht.nozzle, Some(0));
        assert_eq!(ht.tray(0).unwrap().remaining_percent, None);
        assert_eq!(
            status.ams_unit(1).unwrap().nozzle,
            None,
            "0xE: bound to no single nozzle"
        );
        assert!(status.ams_drying());
        assert_eq!(
            status.external.len(),
            2,
            "only ids 254 and 255 are external holders"
        );
    }

    #[test]
    fn partial_reports_change_only_what_they_carry() {
        let mut status = PrinterStatus::default();
        assert_eq!(status.phase(), Phase::Unknown);
        apply(
            &mut status,
            r#"{"print": {"command": "push_status", "gcode_state": "RUNNING", "stg_cur": 2,
                "mc_percent": 0, "subtask_name": "box", "print_error": 0,
                "ams": {"ams": [{"id": "0", "info": "1003",
                         "tray": [{"id": "0", "tray_type": "PLA"}]}]}}}"#,
        );
        assert_eq!(status.phase(), Phase::Heating);
        apply(
            &mut status,
            r#"{"print": {"command": "push_status", "stg_cur": 1}}"#,
        );
        assert_eq!(status.phase(), Phase::Preparing);
        assert_eq!(status.job_name.as_deref(), Some("box"));
        apply(
            &mut status,
            r#"{"print": {"stg_cur": 4, "layer_num": 3, "mc_percent": 250}}"#,
        );
        assert_eq!(
            status.phase(),
            Phase::Printing,
            "a filament change in mid print"
        );
        assert_eq!(status.progress_percent, Some(100));
        assert_eq!(
            status
                .ams_unit(0)
                .unwrap()
                .tray(0)
                .unwrap()
                .material
                .as_deref(),
            Some("PLA")
        );
        // A reply to a command is not a status.
        apply(
            &mut status,
            r#"{"print": {"command": "pause", "result": "success", "gcode_state": "IDLE"}}"#,
        );
        assert_eq!(status.gcode_state, GcodeState::Running);
        apply(
            &mut status,
            r#"{"print": {"gcode_state": "FAILED", "print_error": 50348044,
                "hms": [{"attr": 50331904, "code": 65543}, {"attr": 0, "code": 0}]}}"#,
        );
        assert_eq!(status.phase(), Phase::Failed);
        assert_eq!(status.print_error, Some(0x0300_400C));
        assert_eq!(status.hms.len(), 1);
        apply(
            &mut status,
            r#"{"print": {"gcode_state": "weird", "hms": []}}"#,
        );
        assert_eq!(status.gcode_state, GcodeState::Unknown);
        assert!(status.hms.is_empty());
        assert_eq!(status.reports, 5);
    }

    #[test]
    fn colours_and_stage_names() {
        assert_eq!(Rgba::parse("#DFE2E3").unwrap().0, [0xDF, 0xE2, 0xE3, 255]);
        assert_eq!(Rgba::parse("00000000").unwrap().0, [0, 0, 0, 0]);
        for bad in ["", "red", "12345", "GGGGGGGG", "ÄÄÄÄÄÄ"] {
            assert!(Rgba::parse(bad).is_none(), "{bad}");
        }
        assert_eq!(stage_name(7), "heating_hotend");
        assert_eq!(stage_name(255), "idle");
        assert_eq!(stage_name(9999), "unknown");
    }
}
