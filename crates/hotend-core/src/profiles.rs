//! Printer facts for the core, the engine's checks and the slicer integration.
//!
//! One source of truth per printer:
//! - a printer the printer layer drives (the Bambu Lab X2D) is read from
//!   `hotend-printer`'s `profiles/bambu.toml` through [`hotend_printer::Profiles`];
//! - any other printer (a Prusa, which Hotend slices for but does not drive) is listed in
//!   this crate's `profiles/printers.json`, and nowhere else.
//!
//! `hotend-slicer` keeps a generated copy of the nozzle facts in its `printers.toml`; a
//! test there fails when the copy differs from what this module returns. Every fact carries
//! a `verified` flag and none is verified on a real printer yet (IMPLEMENTATION_BRIEF
//! section 7).

use serde::{Deserialize, Serialize};

use crate::engine::{NozzleContext, PrinterContext, Reach};

const OTHER_PRINTERS_JSON: &str = include_str!("../profiles/printers.json");

/// The printer used when the settings name none.
pub const DEFAULT_PRINTER_ID: &str = "bambu-x2d";

/// The nozzle diameter models are checked for. The fitted nozzle is the user's choice, not
/// a fact of the printer model; until it is a setting every nozzle counts as 0.4 mm.
pub const DEFAULT_NOZZLE_DIAMETER: f64 = 0.4;

/// Where a printer's facts are kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileSource {
    /// `hotend-printer`'s profile file: Hotend can check and deliver jobs for it.
    PrinterLayer,
    /// This crate's `printers.json`: Hotend can model and slice for it, not print.
    Core,
}

/// One printer model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrinterProfile {
    pub id: String,
    pub name: String,
    pub source: ProfileSource,
    pub nozzles: Vec<NozzleProfile>,
}

/// One nozzle. `id` is the device extruder id; `slicer_extruder` is the slicer's 1-based
/// number for the same nozzle. Always map by id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NozzleProfile {
    pub id: i64,
    pub role: String,
    pub drive: String,
    pub slicer_extruder: u32,
    pub diameter: f64,
    pub reach: Reach,
    pub max_speed_mm_s: Option<f64>,
    /// True only when the ids, the reach and the speed limit were all confirmed on a real
    /// printer.
    pub verified: bool,
    /// Where the facts come from.
    pub source: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OtherPrinters {
    schema: u32,
    #[allow(dead_code)]
    note: String,
    printers: Vec<OtherPrinter>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OtherPrinter {
    id: String,
    name: String,
    nozzles: Vec<OtherNozzle>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OtherNozzle {
    id: i64,
    role: String,
    drive: String,
    slicer_extruder: u32,
    reach: Reach,
    max_speed_mm_s: Option<f64>,
    verified: bool,
    source: String,
}

/// Why the compiled-in printer facts cannot be used. Both files are part of the build, so
/// this is a defect of the build, not of the user's data.
#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("the printer layer's profile file is not usable: {0}")]
    PrinterLayer(String),
    #[error("hotend-core's printers.json is not usable: {0}")]
    Core(String),
}

fn from_printer_layer(profile: &hotend_printer::PrinterProfile) -> PrinterProfile {
    PrinterProfile {
        id: profile.id().to_owned(),
        name: profile.name().to_owned(),
        source: ProfileSource::PrinterLayer,
        nozzles: profile
            .nozzles()
            .iter()
            .map(|nozzle| NozzleProfile {
                id: i64::from(nozzle.device_extruder),
                role: nozzle.role.clone(),
                drive: nozzle.drive.clone(),
                slicer_extruder: u32::from(nozzle.slicer_extruder),
                diameter: DEFAULT_NOZZLE_DIAMETER,
                reach: Reach {
                    x: nozzle.reach.x,
                    y: nozzle.reach.y,
                    z: nozzle.reach.z,
                },
                max_speed_mm_s: nozzle.max_speed.as_ref().map(|speed| speed.mm_s),
                verified: nozzle.ids_verified
                    && nozzle.reach.verified
                    && nozzle.max_speed.as_ref().is_none_or(|speed| speed.verified),
                source: format!(
                    "hotend-printer profiles/bambu.toml, revision {}",
                    profile.revision()
                ),
            })
            .collect(),
    }
}

/// Every printer this build knows: the printer layer's first, then the core's own.
pub fn printer_profiles() -> Result<Vec<PrinterProfile>, ProfileError> {
    let layer = hotend_printer::Profiles::builtin()
        .map_err(|error| ProfileError::PrinterLayer(error.to_string()))?;
    let mut all: Vec<PrinterProfile> = layer.printers().iter().map(from_printer_layer).collect();
    let others: OtherPrinters = serde_json::from_str(OTHER_PRINTERS_JSON)
        .map_err(|error| ProfileError::Core(error.to_string()))?;
    if others.schema != 2 {
        return Err(ProfileError::Core(format!(
            "schema {} is not supported",
            others.schema
        )));
    }
    for printer in others.printers {
        if all.iter().any(|known| known.id == printer.id) {
            return Err(ProfileError::Core(format!(
                "`{}` is a printer of the printer layer and must not be listed twice",
                printer.id
            )));
        }
        all.push(PrinterProfile {
            id: printer.id,
            name: printer.name,
            source: ProfileSource::Core,
            nozzles: printer
                .nozzles
                .into_iter()
                .map(|nozzle| NozzleProfile {
                    id: nozzle.id,
                    role: nozzle.role,
                    drive: nozzle.drive,
                    slicer_extruder: nozzle.slicer_extruder,
                    diameter: DEFAULT_NOZZLE_DIAMETER,
                    reach: nozzle.reach,
                    max_speed_mm_s: nozzle.max_speed_mm_s,
                    verified: nozzle.verified,
                    source: nozzle.source,
                })
                .collect(),
        });
    }
    Ok(all)
}

/// The profile of one printer, if this build knows it.
pub fn printer_profile(id: &str) -> Option<PrinterProfile> {
    printer_profiles()
        .ok()?
        .into_iter()
        .find(|printer| printer.id == id)
}

/// The printer layer's own profile of a printer: what checking and delivering a job needs.
/// `None` for a printer Hotend does not drive.
pub fn printer_layer_profile(id: &str) -> Option<hotend_printer::PrinterProfile> {
    hotend_printer::Profiles::builtin().ok()?.printer(id).ok()
}

impl PrinterProfile {
    /// The part of the profile the engine needs for its build-volume and feature checks.
    pub fn engine_context(&self) -> PrinterContext {
        PrinterContext {
            id: self.id.clone(),
            nozzles: self
                .nozzles
                .iter()
                .map(|nozzle| NozzleContext {
                    id: nozzle.id,
                    role: nozzle.role.clone(),
                    diameter: nozzle.diameter,
                    reach: Some(nozzle.reach.clone()),
                })
                .collect(),
        }
    }

    /// The main nozzle: the one with the role `main`, else the first.
    pub fn main_nozzle(&self) -> Option<&NozzleProfile> {
        self.nozzles
            .iter()
            .find(|nozzle| nozzle.role == "main")
            .or(self.nozzles.first())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn the_x2d_comes_from_the_printer_layer_and_nothing_claims_to_be_verified() {
        let x2d = printer_profile(DEFAULT_PRINTER_ID).unwrap();
        assert_eq!(x2d.source, ProfileSource::PrinterLayer);
        assert!(x2d.nozzles.iter().all(|n| !n.verified));
        let main = x2d.main_nozzle().unwrap();
        assert_eq!((main.id, main.slicer_extruder), (1, 1));
        assert_eq!(main.reach.z, [0.0, 260.0]);
        let aux = x2d.nozzles.iter().find(|n| n.role == "auxiliary").unwrap();
        assert_eq!((aux.id, aux.slicer_extruder), (0, 2));
        assert_eq!(aux.reach.x, [20.5, 256.0]);
        assert_eq!(aux.max_speed_mm_s, Some(200.0));
        assert_eq!(x2d.engine_context().nozzles.len(), 2);
        assert!(printer_layer_profile(DEFAULT_PRINTER_ID).is_some());
        assert!(printer_profile("unknown").is_none());
    }

    /// The facts of one printer live in one file: drift between two copies is impossible
    /// because the second copy is refused.
    #[test]
    fn no_printer_is_described_in_both_files() {
        let all = printer_profiles().unwrap();
        let layer = hotend_printer::Profiles::builtin().unwrap();
        let others: OtherPrinters = serde_json::from_str(OTHER_PRINTERS_JSON).unwrap();
        for printer in &others.printers {
            assert!(layer.printer(&printer.id).is_err(), "{}", printer.id);
        }
        assert_eq!(all.len(), layer.printers().len() + others.printers.len());
        let prusa = printer_profile("prusa-mk4s").unwrap();
        assert_eq!(prusa.source, ProfileSource::Core);
        assert!(printer_layer_profile("prusa-mk4s").is_none());
        assert_eq!(prusa.main_nozzle().unwrap().reach.x, [0.0, 250.0]);
    }
}
