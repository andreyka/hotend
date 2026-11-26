//! Printer facts as data, and the rules a job must pass before it is sliced.
//!
//! The facts live in `profiles/printers.toml`, each with its own `verified` flag. This module
//! guarantees three things for a dual-nozzle printer such as the X2D:
//!
//! - a filament is only ever assigned to a nozzle by device extruder id, and the slicer's own
//!   numbering is looked up from the same data row (never derived from a name or an order);
//! - a job whose part cannot be reached by its nozzle, or whose material is not allowed on
//!   its nozzle, is refused before any slicer runs;
//! - the filament map handed to the slicer is always explicit, one entry per filament slot.

use serde::Deserialize;

use crate::error::SlicerError;
use crate::job::{Extent, FilamentAssignment};
use crate::version::SlicerKind;

const PRINTERS_TOML: &str = include_str!("../profiles/printers.toml");
const PRINTERS_FILE: &str = "profiles/printers.toml";

/// Slack when comparing a part against a reach limit, in millimetres.
const REACH_TOLERANCE_MM: f64 = 1e-6;

/// The printer profile file.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrinterFacts {
    pub schema: u32,
    pub revision: String,
    #[serde(rename = "printer")]
    pub printers: Vec<Printer>,
}

/// One printer model.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Printer {
    pub id: String,
    pub name: String,
    /// The slicer that produces jobs for this printer.
    pub slicer: SlicerKind,
    #[serde(rename = "nozzle")]
    pub nozzles: Vec<Nozzle>,
    #[serde(rename = "material_rule", default)]
    pub material_rules: Vec<MaterialRule>,
    /// How the filament-to-nozzle map is given to the slicer; `None` for single-nozzle
    /// printers.
    pub filament_map: Option<FilamentMapRule>,
}

/// One nozzle of a printer.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Nozzle {
    pub role: String,
    pub drive: String,
    pub ids: NozzleIds,
    pub reach: Reach,
    pub max_speed: Option<MaxSpeed>,
    pub source: String,
}

/// The two numberings of one nozzle. They are a fact pair: always read both from here.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NozzleIds {
    /// The extruder id the printer itself uses.
    pub device_extruder: i64,
    /// The slicer's 1-based extruder number for the same nozzle.
    pub slicer_extruder: u32,
    pub verified: bool,
}

/// The box a nozzle can reach, `[low, high]` per axis in millimetres, in bed coordinates.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reach {
    pub x: [f64; 2],
    pub y: [f64; 2],
    pub z: [f64; 2],
    pub verified: bool,
}

impl Reach {
    fn axes(&self) -> [[f64; 2]; 3] {
        [self.x, self.y, self.z]
    }

    fn span(&self) -> [f64; 3] {
        self.axes().map(|[low, high]| high - low)
    }
}

/// A speed limit of a nozzle. Recorded, not enforced here.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaxSpeed {
    pub mm_s: f64,
    pub verified: bool,
}

/// Material families that only the listed nozzles may print.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialRule {
    pub families: Vec<String>,
    pub device_extruders: Vec<i64>,
    pub verified: bool,
    pub source: String,
}

/// The only filament grouping mode Hotend ever writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FilamentMapMode {
    Manual,
}

/// How the explicit filament map is written for the slicer and read back from its result.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilamentMapRule {
    pub mode: FilamentMapMode,
    pub mode_option: String,
    pub mode_value: String,
    pub maps_option: String,
    pub maps_separator: String,
    pub result_maps_key: String,
    pub verified: bool,
    pub source: String,
}

impl PrinterFacts {
    /// The printer profiles compiled into this build.
    pub fn builtin() -> Result<Self, SlicerError> {
        Self::from_toml(PRINTERS_TOML).map_err(|message| SlicerError::Data {
            file: PRINTERS_FILE,
            message,
        })
    }

    /// Parses and validates a file in the format of `profiles/printers.toml`.
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let facts: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        if facts.schema != 1 {
            return Err(format!("unsupported schema {}", facts.schema));
        }
        for printer in &facts.printers {
            printer
                .validate()
                .map_err(|problem| format!("printer `{}`: {problem}", printer.id))?;
        }
        Ok(facts)
    }

    /// The profile of one printer.
    pub fn printer(&self, id: &str) -> Result<&Printer, SlicerError> {
        self.printers
            .iter()
            .find(|printer| printer.id == id)
            .ok_or_else(|| SlicerError::UnknownPrinter(id.to_owned()))
    }
}

impl Printer {
    fn validate(&self) -> Result<(), String> {
        if self.nozzles.is_empty() {
            return Err("has no nozzle".into());
        }
        for (index, nozzle) in self.nozzles.iter().enumerate() {
            let twin = self.nozzles[..index].iter().any(|other| {
                other.ids.device_extruder == nozzle.ids.device_extruder
                    || other.ids.slicer_extruder == nozzle.ids.slicer_extruder
            });
            if twin {
                return Err("two nozzles share an extruder id".into());
            }
            let number = nozzle.ids.slicer_extruder as usize;
            if number == 0 || number > self.nozzles.len() {
                return Err("slicer extruder numbers must be 1..=number of nozzles".into());
            }
            for [low, high] in nozzle.reach.axes() {
                if !(low.is_finite() && high.is_finite() && low < high) {
                    return Err(format!("the {} nozzle has an empty reach", nozzle.role));
                }
            }
        }
        for rule in &self.material_rules {
            let known = rule
                .device_extruders
                .iter()
                .all(|id| self.nozzle(*id).is_some());
            if rule.families.is_empty() || rule.device_extruders.is_empty() || !known {
                return Err("a material rule names no family or an unknown extruder".into());
            }
        }
        match &self.filament_map {
            Some(map) => {
                let option = |text: &str| {
                    text.strip_prefix("--").is_some_and(|name| {
                        !name.is_empty()
                            && name.starts_with(|c: char| c.is_ascii_lowercase())
                            && name
                                .chars()
                                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                    })
                };
                let plain = |text: &str| {
                    !text.is_empty()
                        && text
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '_')
                };
                if !option(&map.mode_option)
                    || !option(&map.maps_option)
                    || !plain(&map.mode_value)
                    || !plain(&map.result_maps_key)
                    || !matches!(map.maps_separator.as_str(), "," | ";" | " ")
                {
                    return Err("the filament map section has a malformed option".into());
                }
            }
            None if self.nozzles.len() > 1 => {
                return Err("a printer with several nozzles needs a filament_map section".into());
            }
            None => {}
        }
        Ok(())
    }

    /// The nozzle with this device extruder id.
    pub fn nozzle(&self, device_extruder: i64) -> Option<&Nozzle> {
        self.nozzles
            .iter()
            .find(|nozzle| nozzle.ids.device_extruder == device_extruder)
    }

    /// The nozzle the profile calls `main`; `None` if the data names none.
    pub fn main_nozzle(&self) -> Option<&Nozzle> {
        self.nozzles.iter().find(|nozzle| nozzle.role == "main")
    }

    fn nozzle_of(&self, filament: &FilamentAssignment) -> Result<&Nozzle, SlicerError> {
        self.nozzle(filament.nozzle_id).ok_or_else(|| {
            SlicerError::InvalidJob(format!(
                "slot {} is assigned to device extruder {}, which {} does not have",
                filament.slot, filament.nozzle_id, self.name
            ))
        })
    }

    /// Checks the filament list itself: slots are 1, 2, ... without gaps (the slicer's map
    /// is positional) and every nozzle id exists on this printer.
    fn check_slots(&self, filaments: &[FilamentAssignment]) -> Result<(), SlicerError> {
        if filaments.is_empty() {
            return Err(SlicerError::InvalidJob("the job has no filament".into()));
        }
        if filaments.len() > 32 {
            return Err(SlicerError::InvalidJob("more than 32 filaments".into()));
        }
        for (index, filament) in filaments.iter().enumerate() {
            if filament.slot as usize != index + 1 {
                return Err(SlicerError::InvalidJob(format!(
                    "filament slots must be 1, 2, ... in order without gaps; found slot {} at \
                     position {}",
                    filament.slot,
                    index + 1
                )));
            }
            self.nozzle_of(filament)?;
        }
        Ok(())
    }

    /// Refuses a job this printer cannot print as assigned: a material on a nozzle that must
    /// not print it, or a part outside the reach of the nozzle that prints it.
    ///
    /// An extent with `placed` set is compared with the nozzle's reach box in bed
    /// coordinates. Otherwise only its size is compared with the size of the reach box,
    /// allowing a quarter turn about Z, because the slicer still arranges it; that is a
    /// necessary condition, not a proof that the arranged part is reachable.
    pub fn check_job(
        &self,
        filaments: &[FilamentAssignment],
        extents: &[Extent],
    ) -> Result<(), SlicerError> {
        self.check_slots(filaments)?;
        for filament in filaments {
            let nozzle = self.nozzle_of(filament)?;
            let family = normalise_family(&filament.family);
            if family.is_empty() && !self.material_rules.is_empty() {
                return Err(SlicerError::InvalidJob(format!(
                    "slot {} has no material family, so the nozzle rules of {} cannot be checked",
                    filament.slot, self.name
                )));
            }
            for rule in &self.material_rules {
                let restricted = rule.families.iter().any(|entry| {
                    let entry = normalise_family(entry);
                    !entry.is_empty() && family.starts_with(&entry)
                });
                if restricted && !rule.device_extruders.contains(&filament.nozzle_id) {
                    return Err(SlicerError::MaterialNotAllowed {
                        family: filament.family.clone(),
                        slot: filament.slot,
                        role: nozzle.role.clone(),
                        device_extruder: filament.nozzle_id,
                        allowed: rule.device_extruders.clone(),
                    });
                }
            }
        }
        for extent in extents {
            let filament = filaments
                .iter()
                .find(|filament| filament.slot == extent.slot)
                .ok_or_else(|| {
                    SlicerError::InvalidJob(format!(
                        "`{}` is printed by slot {}, which the job does not define",
                        extent.name, extent.slot
                    ))
                })?;
            let nozzle = self.nozzle_of(filament)?;
            check_extent(extent, nozzle)?;
        }
        Ok(())
    }

    /// The explicit filament map: for each filament slot in order, the slicer's extruder
    /// number of the nozzle that prints it. The pairing of device id and slicer number comes
    /// from one data row per nozzle.
    pub fn filament_map(&self, filaments: &[FilamentAssignment]) -> Result<Vec<u32>, SlicerError> {
        self.check_slots(filaments)?;
        filaments
            .iter()
            .map(|filament| Ok(self.nozzle_of(filament)?.ids.slicer_extruder))
            .collect()
    }

    /// Short descriptions of the facts of this printer that are still unverified.
    pub fn unverified_facts(&self) -> Vec<String> {
        let mut facts = Vec::new();
        for nozzle in &self.nozzles {
            if !nozzle.ids.verified {
                facts.push(format!("{} nozzle extruder ids", nozzle.role));
            }
            if !nozzle.reach.verified {
                facts.push(format!("{} nozzle reach", nozzle.role));
            }
            if nozzle
                .max_speed
                .as_ref()
                .is_some_and(|speed| !speed.verified)
            {
                facts.push(format!("{} nozzle speed limit", nozzle.role));
            }
        }
        if self.material_rules.iter().any(|rule| !rule.verified) {
            facts.push("material-to-nozzle rules".into());
        }
        if self.filament_map.as_ref().is_some_and(|map| !map.verified) {
            facts.push("filament map options".into());
        }
        facts
    }
}

/// Upper-cases a family name and drops spaces, dashes and underscores: `PLA Aero` and
/// `pla-aero` become `PLAAERO`.
fn normalise_family(family: &str) -> String {
    family
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '_'))
        .flat_map(char::to_uppercase)
        .collect()
}

fn check_extent(extent: &Extent, nozzle: &Nozzle) -> Result<(), SlicerError> {
    let fail = |detail: String| SlicerError::OutOfReach {
        what: extent.name.clone(),
        role: nozzle.role.clone(),
        device_extruder: nozzle.ids.device_extruder,
        detail,
    };
    let finite = extent.min.iter().chain(&extent.max).all(|v| v.is_finite());
    let ordered = extent
        .min
        .iter()
        .zip(&extent.max)
        .all(|(low, high)| low <= high);
    if !finite || !ordered {
        return Err(SlicerError::InvalidJob(format!(
            "`{}` has an invalid bounding box",
            extent.name
        )));
    }
    if extent.placed {
        let axes = ["X", "Y", "Z"].into_iter().zip(nozzle.reach.axes());
        for (index, (axis, [low, high])) in axes.enumerate() {
            let (part_low, part_high) = (extent.min[index], extent.max[index]);
            if part_low < low - REACH_TOLERANCE_MM || part_high > high + REACH_TOLERANCE_MM {
                return Err(fail(format!(
                    "it spans {axis} {part_low:.2} to {part_high:.2} mm and the nozzle reaches \
                     {low:.2} to {high:.2} mm"
                )));
            }
        }
        return Ok(());
    }
    let size = [
        extent.max[0] - extent.min[0],
        extent.max[1] - extent.min[1],
        extent.max[2] - extent.min[2],
    ];
    let span = nozzle.reach.span();
    let fits = |a: f64, limit: f64| a <= limit + REACH_TOLERANCE_MM;
    let flat = (fits(size[0], span[0]) && fits(size[1], span[1]))
        || (fits(size[1], span[0]) && fits(size[0], span[1]));
    if !flat || !fits(size[2], span[2]) {
        return Err(fail(format!(
            "it measures {:.2} x {:.2} x {:.2} mm and the nozzle covers {:.2} x {:.2} x {:.2} mm",
            size[0], size[1], size[2], span[0], span[1], span[2]
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::path::Path;

    fn x2d() -> Printer {
        PrinterFacts::builtin()
            .unwrap()
            .printer("bambu-x2d")
            .unwrap()
            .clone()
    }

    fn filament(slot: u32, family: &str, nozzle_id: i64) -> FilamentAssignment {
        FilamentAssignment {
            slot,
            filament_profile: "pla-basic".into(),
            family: family.into(),
            nozzle_id,
        }
    }

    fn extent(slot: u32, min: [f64; 3], max: [f64; 3], placed: bool) -> Extent {
        Extent {
            name: "part".into(),
            slot,
            min,
            max,
            placed,
        }
    }

    #[test]
    fn the_x2d_facts_are_the_ones_in_the_brief_and_none_is_verified() {
        let facts = PrinterFacts::builtin().unwrap();
        assert_eq!(facts.schema, 1);
        let x2d = facts.printer("bambu-x2d").unwrap();
        assert_eq!(x2d.slicer, SlicerKind::BambuStudio);

        let main = x2d.nozzle(1).unwrap();
        assert_eq!(
            (main.role.as_str(), main.drive.as_str()),
            ("main", "direct")
        );
        assert_eq!(main.ids.slicer_extruder, 1);
        assert_eq!(main.reach.span(), [256.0, 256.0, 260.0]);
        assert!(main.max_speed.is_none());

        let aux = x2d.nozzle(0).unwrap();
        assert_eq!(
            (aux.role.as_str(), aux.drive.as_str()),
            ("auxiliary", "bowden")
        );
        assert_eq!(aux.ids.slicer_extruder, 2);
        assert_eq!(aux.reach.x, [20.5, 256.0]);
        assert_eq!(aux.reach.span(), [235.5, 256.0, 256.0]);
        assert_eq!(aux.max_speed.as_ref().unwrap().mm_s, 200.0);

        let rule = &x2d.material_rules[0];
        assert_eq!(rule.families, ["TPU", "PLA Aero", "PPA-CF"]);
        assert_eq!(rule.device_extruders, [1]);
        assert_eq!(
            x2d.filament_map.as_ref().unwrap().mode,
            FilamentMapMode::Manual
        );

        for printer in &facts.printers {
            for nozzle in &printer.nozzles {
                assert!(!nozzle.ids.verified && !nozzle.reach.verified);
                assert!(
                    nozzle
                        .max_speed
                        .as_ref()
                        .is_none_or(|speed| !speed.verified)
                );
            }
            assert!(printer.material_rules.iter().all(|rule| !rule.verified));
            assert!(
                printer
                    .filament_map
                    .as_ref()
                    .is_none_or(|map| !map.verified)
            );
        }
        assert_eq!(x2d.unverified_facts().len(), 7);
        assert!(matches!(
            facts.printer("nope"),
            Err(SlicerError::UnknownPrinter(_))
        ));
    }

    /// The nozzle blocks of one printer as this file spells them, from the core's profile.
    fn rendered_nozzles(profile: &hotend_core::profiles::PrinterProfile) -> String {
        let mut out = String::new();
        for nozzle in &profile.nozzles {
            let verified = nozzle.verified;
            let reach = &nozzle.reach;
            out.push_str("[[printer.nozzle]]\n");
            out.push_str(&format!("role = {:?}\n", nozzle.role));
            out.push_str(&format!("drive = {:?}\n", nozzle.drive));
            out.push_str(&format!(
                "ids = {{ device_extruder = {}, slicer_extruder = {}, verified = {verified} }}\n",
                nozzle.id, nozzle.slicer_extruder
            ));
            out.push_str(&format!(
                "reach = {{ x = {:?}, y = {:?}, z = {:?}, verified = {verified} }}\n",
                reach.x, reach.y, reach.z
            ));
            if let Some(speed) = nozzle.max_speed_mm_s {
                out.push_str(&format!(
                    "max_speed = {{ mm_s = {speed:?}, verified = {verified} }}\n"
                ));
            }
            out.push_str(&format!("source = {:?}\n\n", nozzle.source));
        }
        out
    }

    /// One source of truth for nozzle facts: the printer layer's profile for the X2D and
    /// the core's file for the others, both read through `hotend_core::profiles`. This
    /// file's nozzle blocks are rendered from there; a difference fails here. Run with
    /// `HOTEND_REGENERATE_PROFILES=1` to rewrite the blocks.
    #[test]
    fn the_nozzle_blocks_are_generated_from_the_core_profiles() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("profiles/printers.toml");
        let text = std::fs::read_to_string(&path).unwrap();
        let known = hotend_core::profiles::printer_profiles().unwrap();
        let marker = "# BEGIN GENERATED nozzles of ";
        let ids: Vec<&str> = text
            .lines()
            .filter_map(|line| line.strip_prefix(marker))
            .collect();
        let mut expected = text.clone();
        for id in &ids {
            let profile = known
                .iter()
                .find(|profile| profile.id == *id)
                .unwrap_or_else(|| panic!("the core has no profile for `{id}`"));
            let begin = format!("{marker}{id}\n");
            let end = format!("# END GENERATED nozzles of {id}\n");
            let from = expected.find(&begin).unwrap() + begin.len();
            let to = expected.find(&end).unwrap();
            expected.replace_range(from..to, &rendered_nozzles(profile));
        }
        if std::env::var_os("HOTEND_REGENERATE_PROFILES").is_some_and(|value| value == "1") {
            std::fs::write(&path, &expected).unwrap();
            return;
        }
        assert!(
            text == expected,
            "profiles/printers.toml has drifted from hotend_core::profiles; regenerate it \
             with HOTEND_REGENERATE_PROFILES=1 cargo test -p hotend-slicer --lib \
             the_nozzle_blocks_are_generated"
        );
        // What was compiled in is the checked file, every printer's nozzles are generated,
        // and every printer the core knows can be sliced.
        assert_eq!(text, PRINTERS_TOML);
        let facts = PrinterFacts::builtin().unwrap();
        for printer in &facts.printers {
            assert!(ids.contains(&printer.id.as_str()), "{}", printer.id);
            let profile = known.iter().find(|profile| profile.id == printer.id);
            assert_eq!(profile.map(|profile| &profile.name), Some(&printer.name));
        }
        for profile in &known {
            assert!(facts.printers.iter().any(|p| p.id == profile.id));
        }
    }

    #[test]
    fn the_filament_map_goes_by_device_id_not_by_position_or_name() {
        let x2d = x2d();
        // Device extruder 1 is slicer extruder 1; device extruder 0 is slicer extruder 2.
        let both = [filament(1, "PLA", 0), filament(2, "PLA", 1)];
        assert_eq!(x2d.filament_map(&both).unwrap(), [2, 1]);
        let swapped = [filament(1, "PLA", 1), filament(2, "PLA", 0)];
        assert_eq!(x2d.filament_map(&swapped).unwrap(), [1, 2]);
        let same = [
            filament(1, "PLA", 0),
            filament(2, "PETG", 0),
            filament(3, "PLA", 1),
        ];
        assert_eq!(x2d.filament_map(&same).unwrap(), [2, 2, 1]);

        // Renaming the roles in the data changes nothing: only the ids are used.
        let mut renamed = x2d.clone();
        for nozzle in &mut renamed.nozzles {
            nozzle.role = if nozzle.role == "main" {
                "auxiliary"
            } else {
                "main"
            }
            .into();
        }
        renamed.nozzles.reverse();
        assert_eq!(renamed.filament_map(&both).unwrap(), [2, 1]);
    }

    #[test]
    fn slots_must_be_contiguous_and_nozzles_must_exist() {
        let x2d = x2d();
        for bad in [
            vec![],
            vec![filament(2, "PLA", 1)],
            vec![filament(1, "PLA", 1), filament(3, "PLA", 1)],
            vec![filament(1, "PLA", 1), filament(1, "PLA", 0)],
            vec![filament(1, "PLA", 2)],
            vec![filament(1, "PLA", -1)],
        ] {
            let error = x2d.filament_map(&bad).unwrap_err();
            assert_eq!(error.code(), "invalid_job", "{bad:?}");
            assert_eq!(x2d.check_job(&bad, &[]).unwrap_err().code(), "invalid_job");
        }
    }

    #[test]
    fn main_only_materials_are_refused_on_the_auxiliary_nozzle() {
        let x2d = x2d();
        for family in [
            "TPU", "tpu", "TPU 95A", "PLA Aero", "PLA-AERO", "pla_aero", "PPA-CF", "ppacf",
        ] {
            assert!(
                x2d.check_job(&[filament(1, family, 1)], &[]).is_ok(),
                "{family} on main"
            );
            let error = x2d.check_job(&[filament(1, family, 0)], &[]).unwrap_err();
            assert_eq!(
                error.code(),
                "material_not_allowed",
                "{family} on auxiliary"
            );
        }
        for family in ["PLA", "PETG", "PLA Silk", "ABS", "PA-CF"] {
            assert!(
                x2d.check_job(&[filament(1, family, 0)], &[]).is_ok(),
                "{family}"
            );
        }
        // The rule applies to each slot, not just the first.
        let mixed = [filament(1, "PLA", 1), filament(2, "TPU", 0)];
        assert_eq!(
            x2d.check_job(&mixed, &[]).unwrap_err().code(),
            "material_not_allowed"
        );
        // No family means the rule cannot be checked.
        assert_eq!(
            x2d.check_job(&[filament(1, " ", 0)], &[])
                .unwrap_err()
                .code(),
            "invalid_job"
        );
    }

    #[test]
    fn placed_parts_are_checked_against_the_reach_box_of_their_nozzle() {
        let x2d = x2d();
        let main = [filament(1, "PLA", 1)];
        let aux = [filament(1, "PLA", 0)];
        let left_edge = extent(1, [5.0, 5.0, 0.0], [60.0, 60.0, 20.0], true);
        assert!(
            x2d.check_job(&main, std::slice::from_ref(&left_edge))
                .is_ok()
        );
        // The auxiliary nozzle starts at X = 20.5 mm.
        let error = x2d
            .check_job(&aux, std::slice::from_ref(&left_edge))
            .unwrap_err();
        assert_eq!(error.code(), "out_of_reach");
        assert!(error.to_string().contains("20.50"), "{error}");
        let at_limit = extent(1, [20.5, 0.0, 0.0], [256.0, 256.0, 256.0], true);
        assert!(x2d.check_job(&aux, std::slice::from_ref(&at_limit)).is_ok());
        // Z: 260 mm on the main nozzle, 256 mm on the auxiliary one.
        let tall = extent(1, [30.0, 30.0, 0.0], [60.0, 60.0, 258.0], true);
        assert!(x2d.check_job(&main, std::slice::from_ref(&tall)).is_ok());
        assert_eq!(
            x2d.check_job(&aux, &[tall]).unwrap_err().code(),
            "out_of_reach"
        );
        let too_tall = extent(1, [30.0, 30.0, 0.0], [60.0, 60.0, 260.5], true);
        assert_eq!(
            x2d.check_job(&main, &[too_tall]).unwrap_err().code(),
            "out_of_reach"
        );
    }

    #[test]
    fn unplaced_parts_are_checked_by_size_and_may_be_turned_about_z() {
        let x2d = x2d();
        let main = [filament(1, "PLA", 1)];
        let aux = [filament(1, "PLA", 0)];
        // 240 mm wide: fits the main nozzle as is, the auxiliary one only turned (235.5 x 256).
        let wide = extent(1, [-120.0, -50.0, 0.0], [120.0, 50.0, 10.0], false);
        assert!(x2d.check_job(&main, std::slice::from_ref(&wide)).is_ok());
        assert!(x2d.check_job(&aux, std::slice::from_ref(&wide)).is_ok());
        let square = extent(1, [0.0, 0.0, 0.0], [240.0, 240.0, 10.0], false);
        assert!(x2d.check_job(&main, std::slice::from_ref(&square)).is_ok());
        assert_eq!(
            x2d.check_job(&aux, &[square]).unwrap_err().code(),
            "out_of_reach"
        );
        let huge = extent(1, [0.0, 0.0, 0.0], [257.0, 10.0, 10.0], false);
        assert_eq!(
            x2d.check_job(&main, &[huge]).unwrap_err().code(),
            "out_of_reach"
        );
        // Each part is checked against the nozzle of its own slot.
        let two = [filament(1, "PLA", 1), filament(2, "PLA", 0)];
        let on_aux = extent(2, [0.0, 0.0, 0.0], [240.0, 240.0, 10.0], false);
        assert_eq!(
            x2d.check_job(&two, &[on_aux]).unwrap_err().code(),
            "out_of_reach"
        );
        let undefined_slot = extent(3, [0.0; 3], [1.0; 3], false);
        assert_eq!(
            x2d.check_job(&two, &[undefined_slot]).unwrap_err().code(),
            "invalid_job"
        );
        let nan = extent(1, [f64::NAN, 0.0, 0.0], [1.0; 3], false);
        assert_eq!(
            x2d.check_job(&main, &[nan]).unwrap_err().code(),
            "invalid_job"
        );
    }

    #[test]
    fn malformed_printer_files_are_rejected() {
        // The file is checked out with either line ending (`core.autocrlf` on Windows);
        // the facts are the same and the edits below are made on the LF form.
        let crlf = PRINTERS_TOML.replace("\r\n", "\n").replace('\n', "\r\n");
        assert_eq!(
            PrinterFacts::from_toml(&crlf).unwrap(),
            PrinterFacts::from_toml(PRINTERS_TOML).unwrap()
        );
        let good = &crlf.replace("\r\n", "\n");
        assert!(PrinterFacts::from_toml(good).is_ok());
        for (from, to) in [
            ("schema = 1", "schema = 2"),
            ("mode = \"manual\"", "mode = \"auto\""),
            (
                "mode_option = \"--filament-map-mode\"",
                "mode_option = \"filament_map_mode; rm\"",
            ),
            ("x = [20.5, 256.0]", "x = [256.0, 20.5]"),
            (
                "device_extruder = 0, slicer_extruder = 2",
                "device_extruder = 1, slicer_extruder = 2",
            ),
            (
                "device_extruder = 0, slicer_extruder = 2",
                "device_extruder = 0, slicer_extruder = 3",
            ),
            ("device_extruders = [1]", "device_extruders = [7]"),
            (
                "verified = false }\nreach = { x = [0.0, 256.0]",
                "}\nreach = { x = [0.0, 256.0]",
            ),
        ] {
            assert!(good.contains(from), "test is stale: {from}");
            let broken = good.replacen(from, to, 1);
            assert!(
                PrinterFacts::from_toml(&broken).is_err(),
                "{to} must be rejected"
            );
        }
    }
}
