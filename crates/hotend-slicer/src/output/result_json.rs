//! Reads `result.json`, the only machine-readable account Bambu Studio's command line gives
//! of a run. The Windows program writes nothing to its console, so whether a slice worked
//! is known from this file alone.
//!
//! The shape was measured on Bambu Studio 02.09.01.70:
//!
//! ```json
//! { "return_code": 0, "error_string": "Success.", "plate_index": 0, "layer_height": 0.2,
//!   "sliced_plates": [ { "id": 1, "total_predication": 710.19, "main_predication": 372.73,
//!       "filaments": [ { "id": 1, "filament_id": "GFA00", "total_used_g": 4.51,
//!                        "main_used_g": 4.51 } ],
//!       "warning_message": "", "objects": [ { "id": 5, "name": "...", "bbox": { ... } } ] } ] }
//! ```
//!
//! `sliced_plates` is missing when nothing was sliced (`--info`), and `objects` is missing
//! when the positions of the input were kept (`--arrange 0`). The `bbox` of an object is
//! not the printed size and is not read: the printed extent comes from the G-code.
//!
//! The file is hostile input: size-capped, every field type-checked, unknown fields
//! ignored, lists cut, text flattened. A number that is missing, negative or not finite is
//! `None`, never zero.

use std::io::Read;
use std::path::Path;

use crate::error::SlicerError;
use crate::job::untrusted_text;

/// File name of the report in the slicer's output folder.
pub const RESULT_NAME: &str = "result.json";
/// Largest `result.json` that is read.
const MAX_RESULT_BYTES: u64 = 1024 * 1024;
const MAX_PLATES: usize = 64;
const MAX_FILAMENTS: usize = 64;

/// What `result.json` says about a run.
#[derive(Debug, Clone, PartialEq)]
pub struct SlicerReport {
    /// 0 for success.
    pub return_code: i64,
    /// The slicer's own words for the outcome, flattened and cut.
    pub error: String,
    pub plates: Vec<PlateReport>,
}

/// One entry of `sliced_plates`.
#[derive(Debug, Clone, PartialEq)]
pub struct PlateReport {
    /// Plate number, from 1.
    pub id: u32,
    /// Total estimated time in seconds, start routine included.
    pub total_s: Option<f64>,
    /// The slicer's `main_predication`, in seconds. In the measured run it was 373 s
    /// where the sliced file's header gave 401 s of model time, so it is kept as stated
    /// and compared with nothing.
    pub model_s: Option<f64>,
    pub filaments: Vec<FilamentReport>,
    /// The slicer's warning for the plate; empty when it has none.
    pub warning: String,
}

/// One entry of a plate's `filaments`.
#[derive(Debug, Clone, PartialEq)]
pub struct FilamentReport {
    /// The slicer's 1-based filament number.
    pub id: u32,
    /// The vendor's id of the filament; empty when the settings did not carry one.
    pub filament_id: String,
    pub total_g: Option<f64>,
}

impl SlicerReport {
    pub fn succeeded(&self) -> bool {
        self.return_code == 0
    }

    pub fn plate(&self, id: u32) -> Option<&PlateReport> {
        self.plates.iter().find(|plate| plate.id == id)
    }
}

fn amount(value: Option<&serde_json::Value>) -> Option<f64> {
    value
        .and_then(serde_json::Value::as_f64)
        .filter(|number| number.is_finite() && *number >= 0.0)
}

fn small_id(value: Option<&serde_json::Value>) -> Option<u32> {
    value
        .and_then(serde_json::Value::as_u64)
        .filter(|id| (1..=64).contains(id))
        .map(|id| id as u32)
}

fn text(value: Option<&serde_json::Value>, max: usize) -> String {
    value
        .and_then(serde_json::Value::as_str)
        .map(|text| untrusted_text(text, max))
        .unwrap_or_default()
}

/// Parses the bytes of a `result.json`.
pub fn parse(bytes: &[u8]) -> Result<SlicerReport, String> {
    if bytes.len() as u64 > MAX_RESULT_BYTES {
        return Err("it is larger than 1 MiB".into());
    }
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| format!("it is not JSON ({e})"))?;
    let object = value.as_object().ok_or("it is not a JSON object")?;
    let return_code = object
        .get("return_code")
        .and_then(serde_json::Value::as_i64)
        .ok_or("it does not state a return code")?;
    let mut plates = Vec::new();
    if let Some(list) = object.get("sliced_plates") {
        let list = list.as_array().ok_or("`sliced_plates` is not a list")?;
        for plate in list.iter().take(MAX_PLATES) {
            let plate = plate.as_object().ok_or("a sliced plate is not an object")?;
            let id = small_id(plate.get("id")).ok_or("a sliced plate has no usable id")?;
            if plates.iter().any(|known: &PlateReport| known.id == id) {
                return Err(format!("plate {id} is listed twice"));
            }
            let mut filaments = Vec::new();
            if let Some(list) = plate.get("filaments").and_then(serde_json::Value::as_array) {
                for filament in list.iter().take(MAX_FILAMENTS) {
                    let Some(id) = small_id(filament.get("id")) else {
                        continue;
                    };
                    filaments.push(FilamentReport {
                        id,
                        filament_id: text(filament.get("filament_id"), 40),
                        total_g: amount(filament.get("total_used_g")),
                    });
                }
            }
            plates.push(PlateReport {
                id,
                total_s: amount(plate.get("total_predication")),
                model_s: amount(plate.get("main_predication")),
                filaments,
                warning: text(plate.get("warning_message"), 300),
            });
        }
    }
    Ok(SlicerReport {
        return_code,
        error: text(object.get("error_string"), 300),
        plates,
    })
}

/// Reads `relative` (a path inside the job folder) if it exists. A file that exists but is
/// not a plain file inside the job folder, is too large or does not parse is an error; the
/// slicer wrote it, so it is not trusted.
pub(crate) fn read(job_dir: &Path, relative: &str) -> Result<Option<SlicerReport>, SlicerError> {
    if std::fs::symlink_metadata(job_dir.join(relative)).is_err() {
        return Ok(None);
    }
    let hostile =
        |reason: String| SlicerError::HostileOutput(format!("the slicer's `{relative}`: {reason}"));
    let path = hotend_core::import::resolve_inside(job_dir, relative, MAX_RESULT_BYTES)
        .map_err(|e| hostile(e.to_string()))?;
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .and_then(|file| file.take(MAX_RESULT_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|e| SlicerError::io("read", &path, e))?;
    parse(&bytes).map(Some).map_err(hostile)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// The shape of a slice that kept the positions (`--arrange 0`): no `objects`. The keys
    /// are the real ones; every value was written for this test.
    const KEPT: &str = r#"{
        "error_string": "Success.",
        "layer_height": 0.2,
        "peak_rss_mb": 140.5,
        "plate_index": 0,
        "return_code": 0,
        "sliced_plates": [
            {
                "feature_type_times": {"Inner wall": 80.5, "Travel": 81.25},
                "filament_change_times": 0,
                "filaments": [
                    {"filament_id": "ABC00", "id": 1, "main_used_g": 4.5, "total_used_g": 4.5}
                ],
                "id": 1,
                "layer_filament_change": 0,
                "main_predication": 372.5,
                "obj_cached_cnt": 0,
                "times": {"sliced_time": 305},
                "total_predication": 710.25,
                "triangle_count": 3244,
                "warning_message": ""
            }
        ],
        "sparse_infill_density": 15.0,
        "times": {"export_time": 289, "prepare_time": 22},
        "wall_loops": 2
    }"#;

    /// The shape of a slice the program arranged itself, from settings that were not
    /// flattened: `objects` with boxes, an empty filament id and no weight.
    const ARRANGED: &str = r#"{
        "error_string": "Success.",
        "layer_height": 0.2,
        "plate_index": 0,
        "return_code": 0,
        "sliced_plates": [
            {
                "filament_change_times": 0,
                "filaments": [
                    {"filament_id": "", "id": 1, "main_used_g": 0.0, "total_used_g": 0.0}
                ],
                "id": 1,
                "main_predication": 510.75,
                "objects": [
                    {"bbox": {"depth": 14.0, "height": 3.2, "width": 14.0,
                              "x": 177.5, "y": 160.75, "z": 0.0},
                     "id": 5, "name": "part.one", "triangle_count": 520}
                ],
                "total_predication": 511.5,
                "warning_message": "something\nto know"
            }
        ],
        "wall_loops": 2
    }"#;

    /// The shape `--info` leaves: nothing was sliced.
    const INFO: &str = r#"{"error_string": "Success.", "layer_height": 0.0, "peak_rss_mb": 76.9,
        "plate_index": 0, "return_code": 0, "sparse_infill_density": 0.0,
        "times": {"import_time": 16}, "wall_loops": 0}"#;

    #[test]
    fn the_two_shapes_of_a_slice_are_read() {
        let kept = parse(KEPT.as_bytes()).unwrap();
        assert!(kept.succeeded());
        assert_eq!(kept.error, "Success.");
        let plate = kept.plate(1).unwrap();
        assert_eq!((plate.total_s, plate.model_s), (Some(710.25), Some(372.5)));
        assert_eq!(plate.warning, "");
        assert_eq!(
            plate.filaments,
            [FilamentReport {
                id: 1,
                filament_id: "ABC00".into(),
                total_g: Some(4.5)
            }]
        );
        assert!(kept.plate(2).is_none());

        let arranged = parse(ARRANGED.as_bytes()).unwrap();
        let plate = arranged.plate(1).unwrap();
        assert_eq!(plate.total_s, Some(511.5));
        // The weight of 0 and the empty id are reported as they are; the caller decides.
        assert_eq!(plate.filaments[0].total_g, Some(0.0));
        assert_eq!(plate.filaments[0].filament_id, "");
        assert_eq!(plate.warning, "something to know");

        let info = parse(INFO.as_bytes()).unwrap();
        assert!(info.succeeded() && info.plates.is_empty());
    }

    #[test]
    fn a_failure_is_read_with_the_slicers_own_words() {
        let failed = parse(
            br#"{"error_string": "The input\tfile is not\u001b[31m valid.", "return_code": -5}"#,
        )
        .unwrap();
        assert!(!failed.succeeded());
        assert_eq!(failed.return_code, -5);
        assert_eq!(failed.error, "The input file is not [31m valid.");
    }

    #[test]
    fn hostile_reports_are_refused_or_cut() {
        for bad in [
            "",
            "[]",
            "\"text\"",
            "{}",
            r#"{"return_code": "0"}"#,
            r#"{"return_code": 0.5}"#,
            r#"{"return_code": 0, "sliced_plates": {}}"#,
            r#"{"return_code": 0, "sliced_plates": [7]}"#,
            r#"{"return_code": 0, "sliced_plates": [{"id": 0}]}"#,
            r#"{"return_code": 0, "sliced_plates": [{"id": 1}, {"id": 1}]}"#,
            r#"{"return_code": 0, "sliced_plates": [{"id": 4000000000}]}"#,
        ] {
            assert!(parse(bad.as_bytes()).is_err(), "{bad}");
        }
        let huge = format!(r#"{{"return_code": 0, "pad": "{}"}}"#, "x".repeat(1 << 20));
        assert!(parse(huge.as_bytes()).is_err());
        // Numbers of the wrong kind are unknown, never zero; strings are cut.
        let odd = parse(
            format!(
                r#"{{"return_code": 0, "error_string": "{}", "sliced_plates": [{{"id": 2,
                "total_predication": -1, "main_predication": "12",
                "filaments": [{{"id": 1, "total_used_g": 1e999}}, {{"id": 99}}, "x",
                              {{"id": 2, "filament_id": 5, "total_used_g": 3}}]}}]}}"#,
                "e".repeat(5000)
            )
            .as_bytes(),
        );
        // 1e999 is not a number serde_json accepts at all.
        assert!(odd.is_err());
        let odd = parse(
            format!(
                r#"{{"return_code": 0, "error_string": "{}", "sliced_plates": [{{"id": 2,
                "total_predication": -1, "main_predication": "12",
                "filaments": [{{"id": 1, "total_used_g": null}}, {{"id": 99}}, "x",
                              {{"id": 2, "filament_id": 5, "total_used_g": 3}}]}}]}}"#,
                "e".repeat(5000)
            )
            .as_bytes(),
        )
        .unwrap();
        assert_eq!(odd.error.chars().count(), 301);
        let plate = odd.plate(2).unwrap();
        assert_eq!((plate.total_s, plate.model_s), (None, None));
        assert_eq!(plate.filaments.len(), 2);
        assert_eq!(plate.filaments[0].total_g, None);
        assert_eq!(
            (
                plate.filaments[1].filament_id.as_str(),
                plate.filaments[1].total_g
            ),
            ("", Some(3.0))
        );
        // More plates than the cap are cut, not an error.
        let many: Vec<String> = (1..=64).map(|i| format!(r#"{{"id": {i}}}"#)).collect();
        let cut = parse(
            format!(
                r#"{{"return_code": 0, "sliced_plates": [{}, {{"id": 1}}]}}"#,
                many.join(",")
            )
            .as_bytes(),
        )
        .unwrap();
        assert_eq!(cut.plates.len(), 64);
    }

    #[cfg(unix)]
    #[test]
    fn only_a_plain_file_inside_the_job_folder_is_read() {
        let tmp = tempfile::tempdir().unwrap();
        let job = std::fs::canonicalize(tmp.path()).unwrap().join("job");
        std::fs::create_dir_all(job.join("output")).unwrap();
        assert_eq!(read(&job, "output/result.json").unwrap(), None);
        std::fs::write(job.join("output/result.json"), INFO).unwrap();
        assert!(
            read(&job, "output/result.json")
                .unwrap()
                .unwrap()
                .succeeded()
        );
        std::fs::write(job.join("output/result.json"), "not json").unwrap();
        assert_eq!(
            read(&job, "output/result.json").unwrap_err().code(),
            "hostile_output"
        );
        std::fs::remove_file(job.join("output/result.json")).unwrap();
        let secret = tmp.path().join("secret.json");
        std::fs::write(&secret, INFO).unwrap();
        std::os::unix::fs::symlink(&secret, job.join("output/result.json")).unwrap();
        assert_eq!(
            read(&job, "output/result.json").unwrap_err().code(),
            "hostile_output"
        );
    }
}
