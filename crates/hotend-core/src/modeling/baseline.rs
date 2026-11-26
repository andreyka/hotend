//! Proven baselines: per data folder, the sticking settings of a print the person reported
//! as good, one record per printer, plate type and material (docs/PRINT_SETTINGS.md,
//! "Proven baselines").
//!
//! Guarantees:
//! - The records live in their own file, `<data folder>/proven-baselines.json`, never in
//!   the settings: nothing is shipped as a default, and a data folder without the file has
//!   no baseline, so the rules decide alone.
//! - Every value is a typed Hotend field inside its hard range (checked by
//!   `hotend_settings::inputs::Baseline::new` when a record is written and every time it is
//!   read); no slicer key can be stored.
//! - The source line is free text held to one printable line of at most
//!   [`MAX_SOURCE_CHARS`] characters (`hotend_settings::propose::Note`). It is a label that
//!   the plan view shows beside the decisions; it never reaches a sentence, a file name or a
//!   value.
//! - A file that cannot be read whole (not a plain file, too large, not JSON of this
//!   layout, too many records) is an error, never an empty list: a baseline that silently
//!   vanished would make the next plan less careful.
//! - Each record is checked on its own. A record that cannot be used (a value out of its
//!   range, a printer this build no longer has, a field it does not know, its printer,
//!   plate and material recorded twice) is kept in the file as written, never taken by a
//!   plan, and named by `hotend baseline show` as a warning. It blocks only the plans of
//!   its own printer, plate and material, with an error that says how to correct or remove
//!   it; plans of the others are made as usual. A record that does not name all three
//!   applies to no plan and blocks none. `remove` takes a record out by its printer, plate
//!   and material, `set` of the same three replaces it, and `remove --invalid`
//!   ([`BaselineStore::remove_invalid`]) takes out every record that cannot be used, and
//!   only those.

use std::path::{Path, PathBuf};

use hotend_settings::inputs::{Baseline, BaselineValues};
use hotend_settings::model::{BrimKind, Day, MaterialId, PlateType};
use hotend_settings::propose::Note;
use serde::{Deserialize, Serialize};

use super::{Modeler, ModelingError};
use crate::store::NewAudit;

/// The file below the data folder.
pub const BASELINES_FILE: &str = "proven-baselines.json";
/// The layout version of the file.
pub const BASELINES_SCHEMA: u32 = 1;
/// The most records a data folder keeps.
pub const MAX_BASELINES: usize = 64;
/// The longest source line, in characters.
pub const MAX_SOURCE_CHARS: usize = 120;
const MAX_FILE_BYTES: u64 = 256 * 1024;

/// The values of a proven baseline, keyed by Hotend's field names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaselineFields {
    #[serde(rename = "adhesion.level")]
    pub level: u8,
    #[serde(rename = "adhesion.brim_kind")]
    pub brim_kind: String,
    #[serde(rename = "adhesion.brim_width_mm")]
    pub brim_width_mm: i64,
    #[serde(rename = "first_layer.wall_speed_mm_s")]
    pub first_layer_wall_speed_mm_s: i64,
    #[serde(rename = "first_layer.infill_speed_mm_s")]
    pub first_layer_infill_speed_mm_s: i64,
    #[serde(rename = "first_layer.bed_c")]
    pub first_layer_bed_c: i64,
    #[serde(rename = "layers.bed_c")]
    pub layers_bed_c: i64,
    #[serde(rename = "cooling.part_fan_off_layers")]
    pub part_fan_off_layers: i64,
    #[serde(rename = "cooling.aux_fan_first_layers_pct")]
    pub aux_fan_first_layers_pct: i64,
    #[serde(rename = "cooling.aux_fan_max_pct")]
    pub aux_fan_max_pct: i64,
    #[serde(rename = "shell.wall_loops")]
    pub wall_loops: i64,
}

/// One proven baseline as stored and shown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvenBaseline {
    /// The printer profile id, such as `bambu-x2d`.
    pub printer_id: String,
    /// Hotend's plate id, such as `textured-pei`.
    pub plate_type: String,
    /// The material id, such as `pla-generic`.
    pub material_id: String,
    /// The day of the print, `YYYY-MM-DD`.
    pub printed_on: String,
    /// The person's words for the print: a label, shown and never interpreted.
    pub source: String,
    pub values: BaselineFields,
    /// When the record was written, milliseconds since the epoch.
    pub recorded_at_ms: i64,
}

impl ProvenBaseline {
    /// The baseline as the rules take it. Every value is checked here.
    pub fn to_input(&self) -> Result<Baseline, ModelingError> {
        let invalid = |what: String| ModelingError::Invalid(what);
        if crate::profiles::printer_profile(&self.printer_id).is_none() {
            return Err(invalid(format!(
                "this build has no printer `{}`; name a printer profile id such as bambu-x2d",
                self.printer_id
            )));
        }
        let plate = PlateType::from_token(&self.plate_type).ok_or_else(|| {
            invalid(
                "the plate must be cool, textured-pei, high-temp, engineering or \
                 cool-supertack"
                    .to_owned(),
            )
        })?;
        let material = MaterialId::parse(&self.material_id).ok_or_else(|| {
            invalid(
                "the material id must be lower-case letters, digits and dashes, such as \
                 pla-generic"
                    .to_owned(),
            )
        })?;
        let printed_on = Day::parse(&self.printed_on).ok_or_else(|| {
            invalid("the day of the print must be a real date written YYYY-MM-DD".to_owned())
        })?;
        if Note::new(&self.source, 1, MAX_SOURCE_CHARS)
            .is_none_or(|note| note.as_str() != self.source)
        {
            return Err(invalid(format!(
                "the source must be one printable line of 1 to {MAX_SOURCE_CHARS} characters"
            )));
        }
        let fields = &self.values;
        let brim_kind = BrimKind::from_token(&fields.brim_kind)
            .ok_or_else(|| invalid("the brim kind must be none or outer".to_owned()))?;
        let values = BaselineValues {
            level: fields.level,
            brim_kind,
            brim_width_mm: fields.brim_width_mm,
            first_layer_wall_speed_mm_s: fields.first_layer_wall_speed_mm_s,
            first_layer_infill_speed_mm_s: fields.first_layer_infill_speed_mm_s,
            first_layer_bed_c: fields.first_layer_bed_c,
            layers_bed_c: fields.layers_bed_c,
            part_fan_off_layers: fields.part_fan_off_layers,
            aux_fan_first_layers_pct: fields.aux_fan_first_layers_pct,
            aux_fan_max_pct: fields.aux_fan_max_pct,
            wall_loops: fields.wall_loops,
        };
        Baseline::new(plate, material, printed_on, values)
            .map_err(|error| invalid(error.to_string()))
    }

    /// Whether this is the record of `printer_id`, `plate_type` and `material_id`.
    pub fn is_for(&self, printer_id: &str, plate_type: &str, material_id: &str) -> bool {
        self.printer_id == printer_id
            && self.plate_type == plate_type
            && self.material_id == material_id
    }
}

/// The file as stored. Each record is read on its own, so that one record that cannot be
/// used does not take the others with it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BaselinesFile {
    schema: u32,
    baselines: Vec<serde_json::Value>,
}

/// A record of the file that cannot be used: it is kept in the file as it is, never taken
/// by a plan, and named by `hotend baseline show` until it is corrected or removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvalidBaseline {
    /// The record's printer, plate and material as the file writes them (empty where the
    /// file has no text there).
    pub printer_id: String,
    pub plate_type: String,
    pub material_id: String,
    /// Why it cannot be used.
    pub problem: String,
}

impl InvalidBaseline {
    /// Whether the record names a printer, a plate and a material. One that does not
    /// applies to no plan.
    pub fn has_key(&self) -> bool {
        !self.printer_id.is_empty() && !self.plate_type.is_empty() && !self.material_id.is_empty()
    }

    fn is_for(&self, printer_id: &str, plate_type: &str, material_id: &str) -> bool {
        self.printer_id == printer_id
            && self.plate_type == plate_type
            && self.material_id == material_id
    }
}

/// What `remove` took out of the file.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BaselineRemoval {
    /// The usable record that was removed, if there was one.
    pub removed: Option<ProvenBaseline>,
    /// The records of the same printer, plate and material that could not be used and were
    /// removed with it.
    pub invalid: Vec<InvalidBaseline>,
}

impl BaselineRemoval {
    /// Whether anything was removed.
    pub fn is_empty(&self) -> bool {
        self.removed.is_none() && self.invalid.is_empty()
    }
}

/// One record of the file: as written, with its key and whether it can be used.
struct StoredRecord {
    raw: serde_json::Value,
    checked: Result<ProvenBaseline, InvalidBaseline>,
}

impl StoredRecord {
    fn is_for(&self, printer_id: &str, plate_type: &str, material_id: &str) -> bool {
        match &self.checked {
            Ok(entry) => entry.is_for(printer_id, plate_type, material_id),
            Err(invalid) => invalid.is_for(printer_id, plate_type, material_id),
        }
    }
}

/// The record `raw` checked on its own.
fn check_record(raw: &serde_json::Value) -> Result<ProvenBaseline, InvalidBaseline> {
    let text = |key: &str| {
        raw.get(key)
            .and_then(serde_json::Value::as_str)
            .map(|text| text.chars().take(80).collect::<String>())
            .unwrap_or_default()
    };
    let invalid = |problem: String| InvalidBaseline {
        printer_id: text("printer_id"),
        plate_type: text("plate_type"),
        material_id: text("material_id"),
        problem,
    };
    let entry: ProvenBaseline = serde_json::from_value(raw.clone())
        .map_err(|_| invalid("it is not a proven baseline record of this version".to_owned()))?;
    entry
        .to_input()
        .map_err(|error| invalid(error.to_string()))?;
    Ok(entry)
}

/// The proven baselines of one data folder.
#[derive(Debug, Clone)]
pub struct BaselineStore {
    path: PathBuf,
}

impl BaselineStore {
    /// The store of the data folder at `root`.
    pub fn new(root: &Path) -> Self {
        Self {
            path: root.join(BASELINES_FILE),
        }
    }

    /// The path of the file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn io(&self) -> impl Fn(std::io::Error) -> ModelingError + '_ {
        |source| ModelingError::Io {
            path: self.path.clone(),
            source,
        }
    }

    fn corrupt(&self, what: &str) -> ModelingError {
        ModelingError::Invalid(format!(
            "the proven baselines in {} cannot be used ({what}); correct the file or remove \
             it with `hotend baseline remove`",
            self.path.display()
        ))
    }

    /// Every record of the file, each checked on its own. No file: no record. A file that
    /// cannot be read whole (not a plain file, too large, not JSON of this layout, too many
    /// records) is an error. A record that cannot be used, and every record of a printer,
    /// plate and material that is recorded twice, is reported as invalid.
    fn records(&self) -> Result<Vec<StoredRecord>, ModelingError> {
        let metadata = match std::fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(self.io()(error)),
        };
        if !metadata.is_file() {
            return Err(self.corrupt("it is not a plain file"));
        }
        if metadata.len() > MAX_FILE_BYTES {
            return Err(self.corrupt("it is too large"));
        }
        let bytes = std::fs::read(&self.path).map_err(self.io())?;
        let file: BaselinesFile = serde_json::from_slice(&bytes)
            .map_err(|_| self.corrupt("it is not a baseline file of this version"))?;
        if file.schema != BASELINES_SCHEMA {
            return Err(self.corrupt("it has another layout version"));
        }
        if file.baselines.len() > MAX_BASELINES {
            return Err(self.corrupt("it holds too many records"));
        }
        let mut records: Vec<StoredRecord> = file
            .baselines
            .into_iter()
            .map(|raw| StoredRecord {
                checked: check_record(&raw),
                raw,
            })
            .collect();
        // A printer, plate and material recorded twice: neither record is used.
        let keys: Vec<Option<(String, String, String)>> = records
            .iter()
            .map(|record| {
                record.checked.as_ref().ok().map(|entry| {
                    (
                        entry.printer_id.clone(),
                        entry.plate_type.clone(),
                        entry.material_id.clone(),
                    )
                })
            })
            .collect();
        for (index, record) in records.iter_mut().enumerate() {
            let Some(key) = &keys[index] else { continue };
            if keys
                .iter()
                .filter(|other| other.as_ref() == Some(key))
                .count()
                > 1
            {
                record.checked = Err(InvalidBaseline {
                    printer_id: key.0.clone(),
                    plate_type: key.1.clone(),
                    material_id: key.2.clone(),
                    problem: "this printer, plate and material is recorded twice".to_owned(),
                });
            }
        }
        Ok(records)
    }

    /// Every usable record. Records that cannot be used are left out here and named by
    /// [`Self::invalid`].
    pub fn list(&self) -> Result<Vec<ProvenBaseline>, ModelingError> {
        Ok(self
            .records()?
            .into_iter()
            .filter_map(|record| record.checked.ok())
            .collect())
    }

    /// The records of the file that cannot be used, and why.
    pub fn invalid(&self) -> Result<Vec<InvalidBaseline>, ModelingError> {
        Ok(self
            .records()?
            .into_iter()
            .filter_map(|record| record.checked.err())
            .collect())
    }

    /// The record of a printer, plate and material. A record of the three that cannot be
    /// used is an error, never `None`: the plan it would have made less careful is not made
    /// as if there were none. A record that does not name all three applies to no plan and
    /// is passed over here (`hotend baseline show` warns of it). Records of other
    /// printers, plates and materials do not matter here.
    pub fn find(
        &self,
        printer_id: &str,
        plate_type: &str,
        material_id: &str,
    ) -> Result<Option<ProvenBaseline>, ModelingError> {
        let mut found = None;
        for record in self.records()? {
            match record.checked {
                Ok(entry) if entry.is_for(printer_id, plate_type, material_id) => {
                    found = Some(entry);
                }
                Ok(_) => {}
                Err(invalid)
                    if invalid.has_key() && invalid.is_for(printer_id, plate_type, material_id) =>
                {
                    return Err(ModelingError::Invalid(format!(
                        "the proven baseline of {printer_id} / {plate_type} / {material_id} in \
                         {} cannot be used ({}); correct it, record it again with `hotend \
                         baseline set`, or remove it with `hotend baseline remove --printer \
                         {printer_id} --plate {plate_type} --material {material_id}`",
                        self.path.display(),
                        invalid.problem
                    )));
                }
                Err(_) => {}
            }
        }
        Ok(found)
    }

    /// Removes every record that cannot be used on its own (a record with a value out of
    /// its range, of a printer this build does not have, with a field it does not know, or
    /// without a printer, plate or material), and returns them. A printer, plate and
    /// material recorded twice is not removed here: each copy is a good record, and which
    /// one is meant is the person's to say (`hotend baseline set` records one again,
    /// `hotend baseline remove` with the key removes both). Every other record is written
    /// back as it was. Nothing is written when there is none.
    pub fn remove_invalid(&self) -> Result<Vec<InvalidBaseline>, ModelingError> {
        let (invalid, kept): (Vec<StoredRecord>, Vec<StoredRecord>) = self
            .records()?
            .into_iter()
            .partition(|record| check_record(&record.raw).is_err());
        if invalid.is_empty() {
            return Ok(Vec::new());
        }
        self.write(kept.into_iter().map(|record| record.raw).collect())?;
        Ok(invalid
            .into_iter()
            .filter_map(|record| record.checked.err())
            .collect())
    }

    /// Writes a record, replacing every record of the same printer, plate and material
    /// (also one that could not be used). Records of others are kept as they are written,
    /// usable or not. Returns the usable record it replaced.
    pub fn set(&self, entry: ProvenBaseline) -> Result<Option<ProvenBaseline>, ModelingError> {
        entry.to_input()?;
        let (same, mut kept): (Vec<StoredRecord>, Vec<StoredRecord>) =
            self.records()?.into_iter().partition(|record| {
                record.is_for(&entry.printer_id, &entry.plate_type, &entry.material_id)
            });
        if kept.len() >= MAX_BASELINES {
            return Err(ModelingError::Invalid(format!(
                "a data folder keeps at most {MAX_BASELINES} proven baselines; remove one first"
            )));
        }
        let replaced = same.into_iter().find_map(|record| record.checked.ok());
        let raw = serde_json::to_value(&entry)
            .map_err(|error| self.io()(std::io::Error::other(error)))?;
        kept.push(StoredRecord {
            raw,
            checked: Ok(entry),
        });
        let key = |record: &StoredRecord| match &record.checked {
            Ok(entry) => (
                entry.printer_id.clone(),
                entry.plate_type.clone(),
                entry.material_id.clone(),
            ),
            Err(invalid) => (
                invalid.printer_id.clone(),
                invalid.plate_type.clone(),
                invalid.material_id.clone(),
            ),
        };
        kept.sort_by_key(key);
        self.write(kept.into_iter().map(|record| record.raw).collect())?;
        Ok(replaced)
    }

    /// Removes every record of a printer, plate and material: the usable one and any that
    /// cannot be used (so a broken record can always be taken out by its key). Nothing is
    /// written when there is none.
    pub fn remove(
        &self,
        printer_id: &str,
        plate_type: &str,
        material_id: &str,
    ) -> Result<BaselineRemoval, ModelingError> {
        let (same, kept): (Vec<StoredRecord>, Vec<StoredRecord>) = self
            .records()?
            .into_iter()
            .partition(|record| record.is_for(printer_id, plate_type, material_id));
        if same.is_empty() {
            return Ok(BaselineRemoval::default());
        }
        let mut removal = BaselineRemoval::default();
        for record in same {
            match record.checked {
                Ok(entry) => removal.removed = Some(entry),
                Err(invalid) => removal.invalid.push(invalid),
            }
        }
        self.write(kept.into_iter().map(|record| record.raw).collect())?;
        Ok(removal)
    }

    fn write(&self, baselines: Vec<serde_json::Value>) -> Result<(), ModelingError> {
        let file = BaselinesFile {
            schema: BASELINES_SCHEMA,
            baselines,
        };
        let bytes = serde_json::to_vec_pretty(&file)
            .map_err(|error| self.io()(std::io::Error::other(error)))?;
        let temporary = self.path.with_extension("json.new");
        std::fs::write(&temporary, bytes).map_err(self.io())?;
        std::fs::rename(&temporary, &self.path).map_err(self.io())
    }
}

impl Modeler {
    /// Records a proven baseline, replacing the one of the same printer, plate and material,
    /// and audits it. The core dates the record now. Returns the record it replaced.
    pub fn baseline_set(
        &self,
        mut entry: ProvenBaseline,
    ) -> Result<Option<ProvenBaseline>, ModelingError> {
        entry.recorded_at_ms = crate::util::now_ms();
        let outcome = self.baselines().set(entry.clone());
        self.audit_baseline("set", &entry, &outcome);
        outcome
    }

    /// Removes the proven baseline of a printer, plate and material, with any record of the
    /// three that could not be used, and audits it.
    pub fn baseline_remove(
        &self,
        printer_id: &str,
        plate_type: &str,
        material_id: &str,
    ) -> Result<BaselineRemoval, ModelingError> {
        let outcome = self.baselines().remove(printer_id, plate_type, material_id);
        if let Ok(removal) = &outcome {
            if let Some(removed) = &removal.removed {
                self.audit_baseline("remove", removed, &Ok(None));
            }
            for invalid in &removal.invalid {
                self.audit_invalid_removed(invalid);
            }
        }
        outcome
    }

    /// Removes every record of the file that cannot be used, and only those, and audits
    /// each. Returns what was removed (empty when there was none).
    pub fn baseline_remove_invalid(&self) -> Result<Vec<InvalidBaseline>, ModelingError> {
        let outcome = self.baselines().remove_invalid();
        if let Ok(removed) = &outcome {
            for invalid in removed {
                self.audit_invalid_removed(invalid);
            }
        }
        outcome
    }

    fn audit_invalid_removed(&self, invalid: &InvalidBaseline) {
        let _ = self.store.audit(NewAudit {
            project_id: None,
            run_id: None,
            kind: "baseline",
            name: "remove_invalid".to_owned(),
            args_sha256: None,
            outcome: "ok".to_owned(),
            detail: serde_json::json!({
                "printer_id": invalid.printer_id,
                "plate_type": invalid.plate_type,
                "material_id": invalid.material_id,
                "problem": invalid.problem,
            }),
        });
    }

    fn audit_baseline(
        &self,
        name: &str,
        entry: &ProvenBaseline,
        outcome: &Result<Option<ProvenBaseline>, ModelingError>,
    ) {
        // The values and the day, never the person's label.
        let _ = self.store.audit(NewAudit {
            project_id: None,
            run_id: None,
            kind: "baseline",
            name: name.to_owned(),
            args_sha256: None,
            outcome: match outcome {
                Ok(_) => "ok".to_owned(),
                Err(error) => error.code().to_owned(),
            },
            detail: serde_json::json!({
                "printer_id": entry.printer_id,
                "plate_type": entry.plate_type,
                "material_id": entry.material_id,
                "printed_on": entry.printed_on,
                "values": entry.values,
            }),
        });
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn valid() -> ProvenBaseline {
        ProvenBaseline {
            printer_id: "bambu-x2d".into(),
            plate_type: "textured-pei".into(),
            material_id: "pla-generic".into(),
            printed_on: "2026-11-17".into(),
            source: "holder print, reported good".into(),
            values: BaselineFields {
                level: 1,
                brim_kind: "outer".into(),
                brim_width_mm: 5,
                first_layer_wall_speed_mm_s: 20,
                first_layer_infill_speed_mm_s: 30,
                first_layer_bed_c: 65,
                layers_bed_c: 60,
                part_fan_off_layers: 3,
                aux_fan_first_layers_pct: 0,
                aux_fan_max_pct: 30,
                wall_loops: 3,
            },
            recorded_at_ms: 1,
        }
    }

    #[test]
    fn a_record_is_written_found_replaced_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let store = BaselineStore::new(dir.path());
        assert!(store.list().unwrap().is_empty());
        assert_eq!(store.set(valid()).unwrap(), None);
        let found = store
            .find("bambu-x2d", "textured-pei", "pla-generic")
            .unwrap()
            .unwrap();
        assert_eq!(found, valid());
        assert_eq!(
            store.find("bambu-x2d", "cool", "pla-generic").unwrap(),
            None
        );
        let mut newer = valid();
        newer.values.brim_width_mm = 6;
        assert_eq!(store.set(newer.clone()).unwrap(), Some(valid()));
        assert_eq!(store.list().unwrap(), vec![newer.clone()]);
        assert_eq!(
            store
                .remove("bambu-x2d", "textured-pei", "pla-generic")
                .unwrap()
                .removed,
            Some(newer)
        );
        assert!(
            store
                .remove("bambu-x2d", "textured-pei", "pla-generic")
                .unwrap()
                .is_empty()
        );
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn a_record_outside_the_hard_ranges_or_with_a_bad_label_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = BaselineStore::new(dir.path());
        let cases: Vec<fn(&mut ProvenBaseline)> = vec![
            |b| b.values.first_layer_wall_speed_mm_s = 5,
            |b| b.values.first_layer_bed_c = 130,
            |b| b.values.layers_bed_c = 70,
            |b| b.values.brim_kind = "inner".into(),
            |b| b.values.brim_kind = "brim_ears".into(),
            |b| b.values.level = 3,
            |b| b.values.wall_loops = 1,
            |b| b.printer_id = "no-such-printer".into(),
            |b| b.plate_type = "glass".into(),
            |b| b.material_id = "PLA Generic".into(),
            |b| b.printed_on = "2026-11-31".into(),
            |b| b.source = String::new(),
            |b| b.source = "two\nlines".into(),
            |b| b.source = "x".repeat(MAX_SOURCE_CHARS + 1),
            |b| b.source = "hidden\u{202e}text".into(),
        ];
        for (index, change) in cases.into_iter().enumerate() {
            let mut entry = valid();
            change(&mut entry);
            assert!(store.set(entry).is_err(), "case {index}");
        }
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn a_file_that_cannot_be_checked_is_an_error_not_an_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        let store = BaselineStore::new(dir.path());
        for text in [
            "not json",
            r#"{"schema": 2, "baselines": []}"#,
            r#"{"schema": 1, "baselines": [], "extra": 1}"#,
        ] {
            std::fs::write(store.path(), text).unwrap();
            assert!(store.list().is_err(), "{text}");
            assert!(store.find("bambu-x2d", "cool", "pla-generic").is_err());
        }
        // A value edited by hand outside its range: the record is not used, and its plan
        // is refused rather than made without it.
        std::fs::remove_file(store.path()).unwrap();
        store.set(valid()).unwrap();
        let text = std::fs::read_to_string(store.path()).unwrap();
        let key = ("bambu-x2d", "textured-pei", "pla-generic");
        std::fs::write(
            store.path(),
            text.replace(
                "\"first_layer.wall_speed_mm_s\": 20",
                "\"first_layer.wall_speed_mm_s\": 200",
            ),
        )
        .unwrap();
        assert!(store.list().unwrap().is_empty());
        assert_eq!(store.invalid().unwrap().len(), 1);
        assert!(store.find(key.0, key.1, key.2).is_err());
        // A slicer key is not a field of the record.
        std::fs::write(
            store.path(),
            text.replace(
                "\"adhesion.level\": 1",
                "\"adhesion.level\": 1, \"brim_type\": \"no_brim\"",
            ),
        )
        .unwrap();
        assert!(store.list().unwrap().is_empty());
        let invalid = store.invalid().unwrap();
        assert_eq!(invalid.len(), 1);
        assert_eq!(
            (
                invalid[0].printer_id.as_str(),
                invalid[0].plate_type.as_str(),
                invalid[0].material_id.as_str()
            ),
            key
        );
        assert!(store.find(key.0, key.1, key.2).is_err());
        // Twice the same printer, plate and material: neither is used.
        let mut file: serde_json::Value = serde_json::from_str(&text).unwrap();
        let entry = file["baselines"][0].clone();
        file["baselines"].as_array_mut().unwrap().push(entry);
        std::fs::write(store.path(), file.to_string()).unwrap();
        assert!(store.list().unwrap().is_empty());
        assert_eq!(store.invalid().unwrap().len(), 2);
        assert!(store.find(key.0, key.1, key.2).is_err());
        // Both are removed by their key.
        let removal = store.remove(key.0, key.1, key.2).unwrap();
        assert_eq!((removal.removed, removal.invalid.len()), (None, 2));
        assert!(store.invalid().unwrap().is_empty());
    }

    /// One record that cannot be used (here: a printer this build no longer has) does not
    /// stop the plans of other printers, plates and materials, is named, is kept when
    /// another record is written, and can be removed by its key.
    #[test]
    fn a_bad_record_blocks_only_its_own_plans_and_can_be_removed() {
        let dir = tempfile::tempdir().unwrap();
        let store = BaselineStore::new(dir.path());
        store.set(valid()).unwrap();
        let mut cool = valid();
        cool.plate_type = "cool".into();
        store.set(cool.clone()).unwrap();
        // The cool-plate record now names a printer profile this build does not have.
        let text = std::fs::read_to_string(store.path()).unwrap();
        let mut file: serde_json::Value = serde_json::from_str(&text).unwrap();
        for entry in file["baselines"].as_array_mut().unwrap() {
            if entry["plate_type"] == "cool" {
                entry["printer_id"] = "bambu-gone".into();
            }
        }
        std::fs::write(store.path(), file.to_string()).unwrap();

        assert_eq!(store.list().unwrap(), vec![valid()]);
        assert_eq!(
            store
                .find("bambu-x2d", "textured-pei", "pla-generic")
                .unwrap(),
            Some(valid())
        );
        assert_eq!(
            store.find("bambu-x2d", "cool", "pla-generic").unwrap(),
            None
        );
        let error = store
            .find("bambu-gone", "cool", "pla-generic")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("bambu-gone / cool / pla-generic") && error.contains("remove"),
            "{error}"
        );
        let invalid = store.invalid().unwrap();
        assert_eq!(invalid.len(), 1);
        assert_eq!(invalid[0].printer_id, "bambu-gone");
        assert!(invalid[0].problem.contains("no printer"), "{invalid:?}");

        // Writing and removing another record keeps the bad one as it was written.
        let mut wider = valid();
        wider.values.brim_width_mm = 6;
        assert_eq!(store.set(wider.clone()).unwrap(), Some(valid()));
        assert_eq!(store.invalid().unwrap(), invalid);
        assert_eq!(
            store
                .remove("bambu-x2d", "textured-pei", "pla-generic")
                .unwrap()
                .removed,
            Some(wider)
        );
        assert_eq!(store.invalid().unwrap(), invalid);

        // The bad record is removed by its own key.
        let removal = store.remove("bambu-gone", "cool", "pla-generic").unwrap();
        assert_eq!(removal.removed, None);
        assert_eq!(removal.invalid, invalid);
        assert!(store.list().unwrap().is_empty());
        assert!(store.invalid().unwrap().is_empty());

        // A record that does not name a printer, plate and material applies to no plan: it
        // blocks none, stays in the file, and is named as a record that cannot be used.
        std::fs::write(
            store.path(),
            r#"{"schema": 1, "baselines": [{"source": "half a record"},
                {"printer_id": "bambu-x2d", "material_id": "pla-generic"}]}"#,
        )
        .unwrap();
        assert_eq!(
            store
                .find("bambu-x2d", "textured-pei", "pla-generic")
                .unwrap(),
            None
        );
        assert_eq!(store.find("bambu-x2d", "", "pla-generic").unwrap(), None);
        assert_eq!(store.invalid().unwrap().len(), 2);
        assert!(store.invalid().unwrap().iter().all(|bad| !bad.has_key()));
        // A usable record beside them is found and written back untouched.
        store.set(valid()).unwrap();
        assert_eq!(
            store
                .find("bambu-x2d", "textured-pei", "pla-generic")
                .unwrap(),
            Some(valid())
        );
        assert_eq!(store.invalid().unwrap().len(), 2);
    }

    /// `remove --invalid` takes out every record that cannot be used on its own, and
    /// nothing else: a key recorded twice is left for the person to settle by key.
    #[test]
    fn every_record_that_cannot_be_used_and_only_those_can_be_removed() {
        let dir = tempfile::tempdir().unwrap();
        let store = BaselineStore::new(dir.path());
        // Nothing to remove: nothing is written, not even a file.
        assert!(store.remove_invalid().unwrap().is_empty());
        assert!(!store.path().exists());

        store.set(valid()).unwrap();
        let mut cool = valid();
        cool.plate_type = "cool".into();
        store.set(cool.clone()).unwrap();
        let text = std::fs::read_to_string(store.path()).unwrap();
        let mut file: serde_json::Value = serde_json::from_str(&text).unwrap();
        let baselines = file["baselines"].as_array_mut().unwrap();
        // Keyless, out of range, and a key recorded twice.
        baselines.push(serde_json::json!({"source": "half a record"}));
        let mut hot = serde_json::to_value(valid()).unwrap();
        hot["plate_type"] = "high-temp".into();
        hot["values"]["first_layer.bed_c"] = serde_json::json!(200);
        baselines.push(hot);
        let mut engineering = serde_json::to_value(valid()).unwrap();
        engineering["plate_type"] = "engineering".into();
        baselines.push(engineering.clone());
        baselines.push(engineering);
        std::fs::write(store.path(), file.to_string()).unwrap();
        assert_eq!(store.invalid().unwrap().len(), 4);

        let removed = store.remove_invalid().unwrap();
        assert_eq!(removed.len(), 2);
        // The two copies of the engineering record stay, still unused, until the person
        // records one again or removes the key.
        let left = store.invalid().unwrap();
        assert_eq!(left.len(), 2);
        assert!(left.iter().all(|bad| bad.plate_type == "engineering"));
        let mut kept = store.list().unwrap();
        kept.sort_by(|a, b| a.plate_type.cmp(&b.plate_type));
        assert_eq!(kept, vec![cool.clone(), valid()]);
        // Again: nothing left that this removes, and the usable records stay.
        assert!(store.remove_invalid().unwrap().is_empty());
        assert_eq!(store.list().unwrap().len(), 2);
        // Recording the key again settles the copies.
        let mut engineering = valid();
        engineering.plate_type = "engineering".into();
        store.set(engineering).unwrap();
        assert!(store.invalid().unwrap().is_empty());
        assert_eq!(store.list().unwrap().len(), 3);
    }
}
