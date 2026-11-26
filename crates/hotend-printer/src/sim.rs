//! The simulated printer: what the application and every test talk to until a real printer
//! is allowed.
//!
//! It implements [`Printer`] in-process, with no socket and no file outside the sliced
//! file it is given to read. To keep it honest it does not set its state directly: on every
//! call it writes a status report as the JSON a printer of the H2 generation would send and
//! runs it through the real parser and merger ([`crate::protocol::report`],
//! [`crate::status`]), and it records every command as the wire text the LAN transport
//! would publish ([`SimulatedPrinter::wire_log`]).
//!
//! What it models:
//! - the job state machine: idle, preparing, heating, printing with progress and layers,
//!   paused, finished, failed with an HMS code, cancelled;
//! - AMS units with trays, colours, materials, remaining filament, the nozzle each unit
//!   feeds, and drying;
//! - time that runs faster than real time ([`SimConfig::time_scale`]) or only when told
//!   ([`SimulatedPrinter::advance`]);
//! - the X2D behaviour that a start is accepted while the printer stays idle
//!   ([`StartQuirk::AcceptedButStaysIdle`]), a printer outside Developer Mode that reports
//!   status but refuses commands, and a printer without a USB drive.
//!
//! It is not a firmware emulator: temperatures follow a straight line, a print takes the
//! time the sliced file predicts, and nothing is extruded.

use std::collections::BTreeMap;
use std::io::Read;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::delivery::Capabilities;
use crate::ident::{PrinterIdentity, Serial};
use crate::job::{CheckedJob, hex};
use crate::printer::{Printer, PrinterError, UploadedJob};
use crate::profile::PrinterProfile;
use crate::protocol::command::Command;
use crate::protocol::hms::HmsCode;
use crate::protocol::report;
use crate::status::{AmsUnit, PrinterStatus, Rgba, Tray};

/// What the simulated printer does with a start command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartQuirk {
    /// It starts the job.
    None,
    /// It accepts the command and stays idle, as an X2D does while its AMS dries filament.
    AcceptedButStaysIdle,
}

/// A failure the simulated printer runs into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimFault {
    /// Progress at which the print fails, in percent.
    pub at_percent: u8,
    /// The health message it reports.
    pub hms: HmsCode,
    /// The print error number it reports.
    pub print_error: u32,
}

/// How the simulated printer is set up.
#[derive(Debug, Clone)]
pub struct SimConfig {
    /// Who the printer is.
    pub identity: PrinterIdentity,
    /// The AMS units and what is in them.
    pub ams: Vec<AmsUnit>,
    /// The external spool holders.
    pub external: Vec<Tray>,
    /// Simulated seconds per real second. 0 means time moves only with
    /// [`SimulatedPrinter::advance`].
    pub time_scale: f64,
    /// How long preparation (homing, levelling) takes.
    pub prepare: Duration,
    /// How long heating takes.
    pub heat: Duration,
    /// How long printing takes; `None` uses the time the sliced file predicts (10 minutes
    /// when it predicts none).
    pub print: Option<Duration>,
    /// What a start command does.
    pub start_quirk: StartQuirk,
    /// A failure to run into.
    pub fault: Option<SimFault>,
    /// Whether the printer is in Developer Mode. Outside it, commands are refused and
    /// status is still reported.
    pub developer_mode: bool,
    /// Whether a USB drive is in the printer (uploads need one).
    pub storage_present: bool,
}

fn tray(id: u8, material: &str, product: &str, colour: [u8; 4], remaining: u8) -> Tray {
    Tray {
        id,
        material: Some(material.to_owned()),
        product: Some(product.to_owned()),
        colour: Some(Rgba(colour)),
        colours: vec![Rgba(colour)],
        profile_id: None,
        remaining_percent: Some(remaining),
        nozzle_min_c: None,
        nozzle_max_c: None,
    }
}

impl SimConfig {
    /// An X2D with one AMS on the main nozzle (four PLA spools) and one AMS HT on the
    /// auxiliary nozzle (PETG), in Developer Mode, with a USB drive, on manual time.
    pub fn x2d(profile: &PrinterProfile) -> Self {
        let nozzle = |slicer: u8| {
            profile
                .nozzle_by_slicer_id(slicer)
                .map(|n| n.device_extruder)
        };
        let ht = profile.family().ams.ht_unit_id_base;
        // A fixed, obviously artificial serial with the model's prefix.
        let prefix = profile
            .identity()
            .serial_prefixes
            .first()
            .cloned()
            .unwrap_or_default();
        let serial = Serial::new(&format!("{prefix}0SIM00000001"))
            .or_else(|_| Serial::new("SIM0000000000001"))
            .unwrap_or_else(|_| unreachable_serial());
        Self {
            identity: PrinterIdentity {
                serial,
                model: profile.id().to_owned(),
                name: format!("Simulated {}", profile.name()),
            },
            ams: vec![
                AmsUnit {
                    id: 0,
                    high_temperature: false,
                    nozzle: nozzle(1),
                    humidity: Some(4),
                    temperature: Some(24.0),
                    drying_minutes_left: Some(0),
                    trays: vec![
                        tray(0, "PLA", "PLA Basic", [0xFF, 0xFF, 0xFF, 0xFF], 80),
                        tray(1, "PLA", "PLA Basic", [0x10, 0x10, 0x10, 0xFF], 55),
                        tray(2, "PLA", "PLA Matte", [0xD0, 0x30, 0x30, 0xFF], 30),
                        tray(3, "PLA", "PLA Basic", [0x20, 0x60, 0xD0, 0xFF], 95),
                    ],
                },
                AmsUnit {
                    id: ht,
                    high_temperature: true,
                    nozzle: nozzle(2),
                    humidity: Some(5),
                    temperature: Some(30.0),
                    drying_minutes_left: Some(0),
                    trays: vec![tray(0, "PETG", "PETG HF", [0x30, 0xA0, 0x50, 0xFF], 70)],
                },
            ],
            external: Vec::new(),
            time_scale: 0.0,
            prepare: Duration::from_secs(60),
            heat: Duration::from_secs(120),
            print: None,
            start_quirk: StartQuirk::None,
            fault: None,
            developer_mode: true,
            storage_present: true,
        }
    }
}

/// `Serial::new` cannot fail on these literals; this keeps the constructor total anyway.
fn unreachable_serial() -> Serial {
    #[allow(clippy::expect_used)]
    Serial::new("SIMULATED0000001").expect("a valid literal")
}

#[derive(Debug, Clone)]
struct Stored {
    sha256: String,
    plate_seconds: Option<u64>,
    layers: Option<u32>,
    nozzle_c: f64,
    bed_c: f64,
    filaments: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    Finished,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone)]
struct Run {
    name: String,
    file: String,
    stored: Stored,
    /// Time the job has been active (not paused).
    active: Duration,
    paused: bool,
    ended: Option<Ending>,
}

/// An in-process printer. See the module documentation.
pub struct SimulatedPrinter {
    profile: PrinterProfile,
    config: SimConfig,
    storage: BTreeMap<String, Stored>,
    run: Option<Run>,
    status: PrinterStatus,
    print_error: u32,
    hms: Vec<HmsCode>,
    light: bool,
    speed: u8,
    sequence: u64,
    wire_log: Vec<String>,
    manual: Duration,
    last_real: Instant,
    last_tick: Duration,
    real_scaled: Duration,
}

impl SimulatedPrinter {
    /// A simulated printer of `profile`'s model.
    pub fn new(profile: PrinterProfile, config: SimConfig) -> Self {
        let standard = profile.family().speed_levels.standard;
        let mut printer = Self {
            profile,
            config,
            storage: BTreeMap::new(),
            run: None,
            status: PrinterStatus::default(),
            print_error: 0,
            hms: Vec::new(),
            light: true,
            speed: standard,
            sequence: 0,
            wire_log: Vec::new(),
            manual: Duration::ZERO,
            last_real: Instant::now(),
            last_tick: Duration::ZERO,
            real_scaled: Duration::ZERO,
        };
        printer.push();
        printer
    }

    /// The configuration, to change between calls (AMS contents, quirks, faults).
    pub fn config_mut(&mut self) -> &mut SimConfig {
        &mut self.config
    }

    /// Moves simulated time forward.
    pub fn advance(&mut self, by: Duration) {
        self.manual += by;
        self.tick();
    }

    /// Starts or stops filament drying in an AMS unit.
    pub fn set_drying(&mut self, unit: u8, minutes_left: u32) {
        if let Some(ams) = self.config.ams.iter_mut().find(|ams| ams.id == unit) {
            ams.drying_minutes_left = Some(minutes_left);
        }
        self.tick();
    }

    /// Every command received, as the JSON the LAN transport would publish, oldest first.
    pub fn wire_log(&self) -> &[String] {
        &self.wire_log
    }

    /// Names of the files on the simulated storage.
    pub fn stored_files(&self) -> Vec<String> {
        self.storage.keys().cloned().collect()
    }

    fn now(&mut self) -> Duration {
        let real = self.last_real.elapsed();
        self.last_real = Instant::now();
        if self.config.time_scale > 0.0 && self.config.time_scale.is_finite() {
            self.real_scaled += real.mul_f64(self.config.time_scale.min(1.0e6));
        }
        self.manual + self.real_scaled
    }

    fn print_time(&self, run: &Run) -> Duration {
        self.config
            .print
            .unwrap_or_else(|| Duration::from_secs(run.stored.plate_seconds.unwrap_or(600)))
            .max(Duration::from_secs(1))
    }

    /// Progress of the printing part, 0.0 to 1.0.
    fn progress(&self, run: &Run) -> f64 {
        let before = self.config.prepare + self.config.heat;
        (run.active.saturating_sub(before).as_secs_f64() / self.print_time(run).as_secs_f64())
            .clamp(0.0, 1.0)
    }

    /// Advances the job to the current simulated time and reports.
    fn tick(&mut self) {
        let now = self.now();
        let passed = now.saturating_sub(self.last_tick);
        self.last_tick = now;
        for ams in &mut self.config.ams {
            if let Some(minutes) = ams.drying_minutes_left.filter(|minutes| *minutes > 0) {
                let done = (passed.as_secs() / 60) as u32;
                ams.drying_minutes_left = Some(minutes.saturating_sub(done));
            }
        }
        if let Some(mut run) = self.run.take() {
            if run.ended.is_none() && !run.paused {
                run.active += passed;
                let progress = self.progress(&run);
                if let Some(fault) = self.config.fault
                    && progress * 100.0 >= f64::from(fault.at_percent)
                    && run.active >= self.config.prepare + self.config.heat
                {
                    run.ended = Some(Ending::Failed);
                    self.hms = vec![fault.hms];
                    self.print_error = fault.print_error;
                } else if progress >= 1.0 {
                    run.ended = Some(Ending::Finished);
                }
            }
            self.run = Some(run);
        }
        self.push();
    }

    /// Writes a status report as JSON and merges it through the real parser.
    fn push(&mut self) {
        let payload = self.report().to_string();
        if let Ok(message) = report::parse(payload.as_bytes())
            && let Some(print) = message.print
        {
            // This printer always sends everything, so a unit that was removed is gone.
            self.status.ams.clear();
            self.status.external.clear();
            self.status.apply(&print, &self.profile);
        }
    }

    fn report(&self) -> Value {
        let config = &self.config;
        let (state, stage, percent, remaining, layer, layers, name, file) = match &self.run {
            None => ("IDLE", 255, 0, 0, 0, 0, String::new(), String::new()),
            Some(run) => {
                let progress = self.progress(run);
                let percent = (progress * 100.0).floor() as u32;
                let total = self.print_time(run);
                let remaining = (total.as_secs_f64() * (1.0 - progress) / 60.0).ceil() as u32;
                let layers = run.stored.layers.unwrap_or(100);
                let layer = (f64::from(layers) * progress).floor() as u32;
                let (state, stage) = match run.ended {
                    Some(Ending::Finished) => ("FINISH", 255),
                    Some(Ending::Failed | Ending::Cancelled) => ("FAILED", 255),
                    None if run.paused => ("PAUSE", 16),
                    None if run.active < config.prepare / 2 => ("PREPARE", 13),
                    None if run.active < config.prepare => ("RUNNING", 1),
                    None if run.active < config.prepare + config.heat / 2 => ("RUNNING", 2),
                    None if run.active < config.prepare + config.heat => ("RUNNING", 7),
                    None => ("RUNNING", 0),
                };
                (
                    state,
                    stage,
                    percent,
                    remaining,
                    layer,
                    layers,
                    run.name.clone(),
                    run.file.clone(),
                )
            }
        };
        // Temperatures: a straight line to the target while heating, the target after.
        let heat_fraction = self.run.as_ref().map_or(0.0, |run| {
            if run.ended.is_some() {
                0.0
            } else {
                (run.active.saturating_sub(config.prepare).as_secs_f64()
                    / config.heat.as_secs_f64().max(1.0))
                .clamp(0.0, 1.0)
            }
        });
        let targets = self
            .run
            .as_ref()
            .filter(|run| run.ended.is_none())
            .map_or((0.0, 0.0), |run| (run.stored.nozzle_c, run.stored.bed_c));
        let packed = |target: f64| {
            let current = (25.0 + (target - 25.0).max(0.0) * heat_fraction).round() as u32;
            (((target.round() as u32) & 0xFFFF) << 16) | (current & 0xFFFF)
        };
        let active = self
            .profile
            .nozzle_by_slicer_id(1)
            .map_or(0, |nozzle| nozzle.device_extruder);
        let extruders: Vec<Value> = self
            .profile
            .nozzles()
            .iter()
            .map(|nozzle| {
                let target = if nozzle.device_extruder == active {
                    targets.0
                } else {
                    0.0
                };
                json!({"id": nozzle.device_extruder, "temp": packed(target)})
            })
            .collect();
        let tray_json = |tray: &Tray| match &tray.material {
            None => json!({"id": tray.id.to_string()}),
            Some(material) => json!({
                "id": tray.id.to_string(),
                "tray_type": material,
                "tray_sub_brands": tray.product.clone().unwrap_or_default(),
                "tray_color": tray.colour.map(|c| c.hex()).unwrap_or_default(),
                "cols": tray.colours.iter().map(Rgba::hex).collect::<Vec<_>>(),
                "tray_info_idx": tray.profile_id.clone().unwrap_or_default(),
                "remain": tray.remaining_percent.map_or(-1, i64::from),
                "nozzle_temp_min": tray.nozzle_min_c.unwrap_or(0).to_string(),
                "nozzle_temp_max": tray.nozzle_max_c.unwrap_or(0).to_string(),
            }),
        };
        let rules = &self.profile.family().ams;
        let units: Vec<Value> = config
            .ams
            .iter()
            .map(|unit| {
                let kind: u64 = if unit.high_temperature { 4 } else { 3 };
                let nozzle = unit
                    .nozzle
                    .map_or(u64::from(rules.nozzle_unbound), u64::from);
                let info = kind | (nozzle << rules.nozzle_shift);
                json!({
                    "id": unit.id.to_string(),
                    "info": format!("{info:X}"),
                    "humidity": unit.humidity.unwrap_or(0).to_string(),
                    "temp": unit.temperature.unwrap_or(0.0).to_string(),
                    "dry_time": unit.drying_minutes_left.unwrap_or(0),
                    "tray": unit.trays.iter().map(tray_json).collect::<Vec<_>>(),
                })
            })
            .collect();
        json!({"print": {
            "command": "push_status",
            "sequence_id": self.status.reports.to_string(),
            "gcode_state": state,
            "stg_cur": stage,
            "mc_percent": percent,
            "mc_remaining_time": remaining,
            "layer_num": layer,
            "total_layer_num": layers,
            "subtask_name": name,
            "gcode_file": file,
            "print_type": if self.run.is_some() { "local" } else { "" },
            "print_error": self.print_error,
            "hms": self.hms.iter().map(|code| json!({"attr": code.attr, "code": code.code})).collect::<Vec<_>>(),
            "device": {
                "extruder": {"info": extruders, "state": (u64::from(active) << 4) | self.profile.nozzles().len() as u64},
                "bed": {"info": {"temp": packed(targets.1)}},
                "ctc": {"info": {"temp": 28}},
            },
            "ams": {"ams": units},
            "vir_slot": config.external.iter().map(tray_json).collect::<Vec<_>>(),
            "spd_lvl": self.speed,
            "lights_report": [{"node": "chamber_light", "mode": if self.light { "on" } else { "off" }}],
            "sdcard": config.storage_present,
        }})
    }

    fn known_error(&self, meaning: &str) -> u32 {
        self.profile
            .family()
            .known_codes
            .iter()
            .find(|known| {
                known.meaning == meaning && known.kind == crate::profile::CodeKind::PrintError
            })
            .map_or(0xFFFF_FFFF, |known| known.code)
    }

    fn busy(&self) -> bool {
        self.run.as_ref().is_some_and(|run| run.ended.is_none())
    }
}

impl Printer for SimulatedPrinter {
    fn identity(&self) -> &PrinterIdentity {
        &self.config.identity
    }

    fn profile(&self) -> &PrinterProfile {
        &self.profile
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::simulated()
    }

    fn status(&mut self) -> Result<PrinterStatus, PrinterError> {
        self.tick();
        Ok(self.status.clone())
    }

    fn upload(&mut self, job: &CheckedJob) -> Result<UploadedJob, PrinterError> {
        self.tick();
        if !self.config.storage_present {
            return Err(PrinterError::Upload(
                "the printer has no USB drive; put one in and try again".to_owned(),
            ));
        }
        // Read what would be sent and check it is the file that was checked.
        let mut file = job.open_verified()?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|e| PrinterError::Upload(e.kind().to_string()))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        let sent = hex(&hasher.finalize());
        if sent != job.sha256() {
            return Err(PrinterError::Upload(
                "the file changed while it was being sent".to_owned(),
            ));
        }
        let summary = job.summary();
        self.storage.insert(
            job.remote_name(),
            Stored {
                sha256: sent,
                plate_seconds: summary.estimated_seconds,
                layers: summary.layers,
                nozzle_c: summary.max_nozzle_c.unwrap_or(0.0),
                bed_c: summary.max_bed_c.unwrap_or(0.0),
                filaments: job.declared().filaments.len(),
            },
        );
        Ok(UploadedJob::new(job, &self.config.identity))
    }

    fn send(&mut self, command: Command) -> Result<(), PrinterError> {
        self.tick();
        self.sequence += 1;
        self.wire_log
            .push(command.to_wire(self.sequence, &self.profile).payload);
        if matches!(command, Command::RequestStatus) {
            return Ok(());
        }
        if !self.config.developer_mode {
            // Outside Developer Mode the printer answers with an error and does nothing.
            self.print_error = self.known_error("not_authorised");
            self.push();
            return Ok(());
        }
        let nothing_running = || PrinterError::Refused("no print is running".to_owned());
        match command {
            Command::RequestStatus => {}
            Command::Light(on) => self.light = on,
            Command::SetSpeed(speed) => {
                let levels = &self.profile.family().speed_levels;
                self.speed = match speed {
                    crate::protocol::command::SpeedProfile::Silent => levels.silent,
                    crate::protocol::command::SpeedProfile::Standard => levels.standard,
                    crate::protocol::command::SpeedProfile::Sport => levels.sport,
                    crate::protocol::command::SpeedProfile::Ludicrous => levels.ludicrous,
                };
            }
            Command::Pause => match &mut self.run {
                Some(run) if run.ended.is_none() && !run.paused => run.paused = true,
                _ => return Err(nothing_running()),
            },
            Command::Resume => match &mut self.run {
                Some(run) if run.ended.is_none() && run.paused => run.paused = false,
                _ => return Err(PrinterError::Refused("no print is paused".to_owned())),
            },
            Command::Stop => {
                let cancelled = self.known_error("cancelled");
                match &mut self.run {
                    Some(run) if run.ended.is_none() => {
                        run.ended = Some(Ending::Cancelled);
                        self.print_error = cancelled;
                    }
                    _ => return Err(nothing_running()),
                }
            }
            Command::StartPreparedJob(start) => {
                if self.busy() {
                    return Err(PrinterError::Refused(
                        "a print is already running".to_owned(),
                    ));
                }
                let Some(stored) = self.storage.get(start.remote_name()).cloned() else {
                    return Err(PrinterError::Refused(format!(
                        "the printer has no file `{}`",
                        start.remote_name()
                    )));
                };
                if start.sources().len() != stored.filaments {
                    self.print_error = self.known_error("filament_mapping");
                } else if self.config.start_quirk == StartQuirk::None {
                    self.print_error = 0;
                    self.hms.clear();
                    self.run = Some(Run {
                        name: start.job_name().to_owned(),
                        file: start.remote_name().to_owned(),
                        stored,
                        active: Duration::ZERO,
                        paused: false,
                        ended: None,
                    });
                }
                // With the quirk the command is accepted and nothing happens.
            }
        }
        self.push();
        Ok(())
    }
}

impl SimulatedPrinter {
    /// SHA-256 of a stored file, for tests that check what was uploaded.
    pub fn stored_sha256(&self, remote_name: &str) -> Option<&str> {
        self.storage
            .get(remote_name)
            .map(|stored| stored.sha256.as_str())
    }
}
