//! The contract between the engine worker's `result.json` and the core's strict types.
//!
//! The core refuses a result with a field it does not know, so a field the worker starts to
//! write without the core knowing breaks every build that carries it. Both sides are held
//! to one list, `fixtures/engine_result_fields.json`: this file proves that the Rust types
//! accept exactly the listed fields; `python/tests/result_contract.py` checks every result
//! the worker writes in its own tests against the same list. A field added on one side only
//! fails one of them. The list also holds the largest counts and sizes: this file proves
//! that the core accepts exactly that many, and the Python side that no tested result has
//! more.
//!
//! `fixtures/engine_result_example.json` is a result with every listed field, generated
//! here from the list (`HOTEND_UPDATE_ENGINE_EXAMPLE=1` writes it) and also read by the
//! desktop shell's `tests/wire.rs`. Nothing here needs Python.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use hotend_core::engine::{
    Artifacts, BBox, CalibrationChange, CheckResult, ComponentUse, DocEntry, EngineError,
    EngineRequest, EngineResult, Gauge, GaugeClearance, GaugeDimension, Orientation, ParamSpec,
    PartSummary, Plate, PlateItem, PlateLayout, Question, SweepFailure, SweepReport,
    SweepRequirement, SweepSample, Unplaced,
};
use hotend_settings::facts::{FitAtBed, InnerLoop, OverhangRegion, PieceFacts, PlateFacts};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// The text no name, id, colour or enumeration accepts.
const BAD: &str = "Bad Name!";

struct Contract {
    doc: Value,
    /// `object.field` and the value to use there instead of its example.
    instead: Option<(String, Value)>,
}

impl Contract {
    fn load() -> Self {
        let text = std::fs::read_to_string(fixture("engine_result_fields.json")).unwrap();
        Self {
            doc: serde_json::from_str(&text).unwrap(),
            instead: None,
        }
    }

    fn fields(&self, object: &str) -> &Map<String, Value> {
        self.doc["objects"][object]
            .as_object()
            .unwrap_or_else(|| panic!("the list has no object `{object}`"))
    }

    fn strings(&self, key: &str) -> Vec<String> {
        serde_json::from_value(self.doc[key].clone()).unwrap()
    }

    /// An object with every listed field the worker writes.
    fn object(&self, object: &str) -> Value {
        let mut out = Map::new();
        for (field, spec) in self.fields(object) {
            let spec = spec.as_str().unwrap();
            if spec == "core_only" {
                continue;
            }
            let path = format!("{object}.{field}");
            let value = match &self.instead {
                Some((at, value)) if *at == path => value.clone(),
                _ => match self.doc["examples"].get(&path) {
                    Some(value) => value.clone(),
                    None => self.example(spec),
                },
            };
            out.insert(field.clone(), value);
        }
        Value::Object(out)
    }

    /// An example value of a type of the list.
    fn example(&self, spec: &str) -> Value {
        let spec = bare(spec);
        if let Some(object) = spec.strip_prefix('@') {
            return self.object(object);
        }
        if let Some(values) = spec.strip_prefix("one_of:") {
            return json!(values.split(',').next().unwrap());
        }
        if let Some((item, count)) = list(spec) {
            let count = count.unwrap_or(1);
            return Value::Array((0..count).map(|_| self.example(item)).collect());
        }
        if let Some((key, item)) = map(spec) {
            let key = match key {
                "view" => "top",
                "piece" => "lid.1",
                "text" => "key",
                "name" => "lid",
                other => panic!("unknown key type `{other}`"),
            };
            return json!({key: self.example(item)});
        }
        match spec {
            "text" | "name" | "question_id" | "check_id" | "short_id" => json!("lid"),
            "piece" => json!("lid.1"),
            "path" => json!("model.step"),
            "color" => json!("#4c78a8"),
            "number" => json!(1.5),
            "integer" | "count" => json!(1),
            "bool" => json!(true),
            "any" | "object" => json!({"anything": [1, "two"]}),
            other => panic!("unknown type `{other}`"),
        }
    }

    /// A value the type must refuse; `None` when the type takes any text or JSON.
    fn refused(&self, spec: &str) -> Option<Value> {
        let spec = bare(spec);
        if spec.starts_with("one_of:") {
            return Some(json!(BAD));
        }
        if let Some((item, _)) = list(spec) {
            return Some(json!([self.refused(item)?]));
        }
        if let Some((key, item)) = map(spec) {
            return match key {
                "text" => Some(json!({"key": self.refused(item)?})),
                _ => Some(json!({BAD: self.example(item)})),
            };
        }
        match spec {
            "name" | "piece" | "question_id" | "check_id" | "short_id" | "color" => {
                Some(json!(BAD))
            }
            _ => None,
        }
    }
}

/// The type without the marks for `null` and for absent.
fn bare(spec: &str) -> &str {
    let spec = spec.strip_suffix('?').unwrap_or(spec);
    spec.strip_suffix("|null").unwrap_or(spec)
}

fn optional(spec: &str) -> bool {
    spec.ends_with('?')
}

fn nullable(spec: &str) -> bool {
    spec.strip_suffix('?').unwrap_or(spec).ends_with("|null")
}

/// The item type and the exact length of `[T]` and `[T;n]`.
fn list(spec: &str) -> Option<(&str, Option<usize>)> {
    let inner = spec.strip_prefix('[')?.strip_suffix(']')?;
    Some(match inner.rsplit_once(';') {
        Some((item, count)) => (item, Some(count.parse().unwrap())),
        None => (inner, None),
    })
}

/// The key type and the value type of `{K:T}`.
fn map(spec: &str) -> Option<(&str, &str)> {
    spec.strip_prefix('{')?.strip_suffix('}')?.split_once(':')
}

fn reads<T: DeserializeOwned>(value: &Value) -> Result<(), String> {
    serde_json::from_value::<T>(value.clone())
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// What reads an object of the list strictly.
type Reader = fn(&Value) -> Result<(), String>;

/// Every object of the list with the type that reads it. The order is of no importance; a
/// listed object without a reader, or a reader without an object, fails the first test.
fn readers() -> Vec<(&'static str, Reader)> {
    vec![
        ("result", reads::<EngineResult>),
        ("error", reads::<EngineError>),
        ("param_spec", reads::<ParamSpec>),
        ("bbox", reads::<BBox>),
        ("part", reads::<PartSummary>),
        ("check", reads::<CheckResult>),
        ("artifacts", reads::<Artifacts>),
        ("doc", reads::<DocEntry>),
        ("question", reads::<Question>),
        ("component", reads::<ComponentUse>),
        ("orientation", reads::<Orientation>),
        ("plate", reads::<Plate>),
        ("plate_item", reads::<PlateItem>),
        ("layout", reads::<PlateLayout>),
        ("unplaced", reads::<Unplaced>),
        ("gauge", reads::<Gauge>),
        ("gauge_dimension", reads::<GaugeDimension>),
        ("gauge_clearance", reads::<GaugeClearance>),
        ("change", reads::<CalibrationChange>),
        ("sweep", reads::<SweepReport>),
        ("sweep_sample", reads::<SweepSample>),
        ("sweep_failure", reads::<SweepFailure>),
        ("sweep_requirement", reads::<SweepRequirement>),
        ("piece_facts", reads::<PieceFacts>),
        ("overhang_region", reads::<OverhangRegion>),
        ("inner_loop", reads::<InnerLoop>),
        ("fit_at_bed", reads::<FitAtBed>),
    ]
}

/// The fields a strict type accepts, from its own refusal of one it does not.
fn accepted_fields(object: &str, read: Reader) -> BTreeSet<String> {
    let refusal = read(&json!({"__probe__": 0})).unwrap_err();
    let (_, expected) = refusal
        .split_once("unknown field `__probe__`, expected ")
        .unwrap_or_else(|| panic!("`{object}` does not refuse an unknown field: {refusal}"));
    expected
        .split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_strict_types_accept_exactly_the_listed_fields() {
    let contract = Contract::load();
    let mut listed: BTreeSet<&str> = contract.doc["objects"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    for (object, read) in readers() {
        assert!(listed.remove(object), "`{object}` is not listed");
        let fields: BTreeSet<String> = contract.fields(object).keys().cloned().collect();
        assert_eq!(
            fields,
            accepted_fields(object, read),
            "`{object}`: the list (left) and the Rust type (right) differ; change \
             fixtures/engine_result_fields.json and the worker together with the type"
        );
        let value = contract.object(object);
        if let Err(error) = read(&value) {
            panic!("`{object}` with every field is refused: {error}\n{value:#}");
        }
    }
    // Read by `PlateFacts::from_value`, which names no field when it refuses.
    assert!(listed.remove("plate_facts"));
    let facts = contract.object("plate_facts");
    PlateFacts::from_value(facts.clone()).unwrap();
    for field in contract.fields("plate_facts").keys() {
        let mut less = facts.clone();
        less.as_object_mut().unwrap().remove(field);
        assert!(PlateFacts::from_value(less).is_err(), "{field}");
    }
    let mut more = facts;
    more["__probe__"] = json!(0);
    assert!(PlateFacts::from_value(more).is_err());
    assert!(listed.is_empty(), "no Rust type reads {listed:?}");
}

#[test]
fn absent_and_null_fields_are_accepted_exactly_where_the_list_says() {
    let contract = Contract::load();
    for (object, read) in readers() {
        let whole = contract.object(object);
        for (field, spec) in contract.fields(object) {
            let spec = spec.as_str().unwrap();
            if spec == "core_only" {
                continue;
            }
            let mut less = whole.clone();
            less.as_object_mut().unwrap().remove(field);
            assert_eq!(
                read(&less).is_ok(),
                optional(spec),
                "`{object}.{field}` ({spec}) when it is absent"
            );
            // Opaque JSON takes null as it takes anything (`sanitize` may still refuse it).
            if matches!(bare(spec), "any" | "object") {
                continue;
            }
            let mut none = whole.clone();
            none[field.as_str()] = Value::Null;
            assert_eq!(
                read(&none).is_ok(),
                nullable(spec),
                "`{object}.{field}` ({spec}) when it is null"
            );
        }
    }
}

#[test]
fn every_listed_enumeration_value_is_accepted() {
    let mut contract = Contract::load();
    let objects: Vec<(&str, Reader)> = readers();
    let mut seen = 0;
    for (object, read) in objects {
        for (field, spec) in contract.fields(object).clone() {
            let Some(values) = bare(spec.as_str().unwrap()).strip_prefix("one_of:") else {
                continue;
            };
            for value in values.split(',') {
                contract.instead = Some((format!("{object}.{field}"), json!(value)));
                let whole = contract.object(object);
                read(&whole).unwrap_or_else(|e| panic!("{object}.{field} = {value}: {e}"));
                seen += 1;
                // The ones with rules of their own between two fields are read above only.
                let whole = contract.object("result");
                let coherent = !["result.status", "param_spec.provenance"]
                    .contains(&format!("{object}.{field}").as_str());
                if coherent {
                    sanitized(&whole).unwrap_or_else(|e| panic!("{object}.{field}={value}: {e}"));
                }
            }
        }
    }
    assert!(seen > 30, "only {seen} values were tried");
}

/// The whole way a result takes into the core: parsed, then sanitised.
fn sanitized(value: &Value) -> Result<EngineResult, String> {
    serde_json::from_value::<EngineResult>(value.clone())
        .map_err(|error| error.to_string())?
        .sanitize("build")
        .map_err(|error| error.0)
}

#[test]
fn a_result_with_every_field_is_accepted_and_loses_nothing() {
    let contract = Contract::load();
    let whole = contract.object("result");
    let typed = sanitized(&whole).unwrap_or_else(|e| panic!("{e}\n{whole:#}"));
    assert_eq!(serde_json::to_value(&typed).unwrap(), whole);
    // A failed job carries its error.
    let mut failed = whole.clone();
    failed["status"] = json!("model_error");
    failed["error"] = contract.object("error");
    assert_eq!(sanitized(&failed).unwrap().error.unwrap().kind, "lid");
}

#[test]
fn names_ids_and_enumerations_outside_the_list_are_refused() {
    let mut contract = Contract::load();
    // The facts stay JSON here; `hotend_settings` refuses them where a rule reads them.
    let facts = [
        "plate_facts",
        "piece_facts",
        "overhang_region",
        "inner_loop",
        "fit_at_bed",
        "error",
    ];
    let mut tried = 0;
    for (object, read) in readers() {
        for (field, spec) in contract.fields(object).clone() {
            contract.instead = None;
            let Some(bad) = contract.refused(spec.as_str().unwrap()) else {
                continue;
            };
            contract.instead = Some((format!("{object}.{field}"), bad));
            let refused = if facts.contains(&object) {
                read(&contract.object(object)).is_err()
            } else {
                sanitized(&contract.object("result")).is_err()
            };
            assert!(refused, "`{object}.{field}` accepts {BAD:?}");
            tried += 1;
        }
    }
    assert!(tried > 45, "only {tried} fields were tried");
}

/// An entry that differs from every other one where the core wants that.
fn entry(path: &str, value: Value, n: usize) -> Value {
    let mut value = value;
    match path {
        "result.components" => value["id"] = json!(format!("c{n}")),
        "result.parts" => value["name"] = json!(format!("p{n}")),
        "result.plates" => {
            value["index"] = json!(n + 1);
            value["threemf"] = Value::Null;
            value["views"] = json!({});
        }
        _ => {}
    }
    value
}

/// The result of the list with `count` entries in the list or map at `path`.
fn with_entries(contract: &mut Contract, path: &str, count: usize) -> Value {
    contract.instead = None;
    let (object, field) = path.split_once('.').unwrap();
    let spec = contract.fields(object)[field].as_str().unwrap().to_owned();
    let entries = match (list(bare(&spec)), map(bare(&spec))) {
        (Some((item, None)), _) => Value::Array(
            (0..count)
                .map(|n| entry(path, contract.example(item), n))
                .collect(),
        ),
        (_, Some((_, item))) => Value::Object(
            (0..count)
                .map(|n| (format!("k{n}"), contract.example(item)))
                .collect(),
        ),
        _ => panic!("`{path}` ({spec}) is no list or map"),
    };
    contract.instead = Some((path.to_owned(), entries));
    contract.object("result")
}

#[test]
fn the_core_accepts_the_listed_number_of_entries_and_not_one_more() {
    let mut contract = Contract::load();
    let maxima = contract.doc["max_items"].as_object().unwrap().clone();
    for (path, most) in &maxima {
        let most = usize::try_from(most.as_u64().unwrap()).unwrap();
        let full = with_entries(&mut contract, path, most);
        sanitized(&full).unwrap_or_else(|e| panic!("{path} with {most} entries: {e}"));
        let over = with_entries(&mut contract, path, most + 1);
        assert!(
            sanitized(&over).is_err(),
            "`{path}` accepts more than {most} entries"
        );
    }
    // Every list and map is bounded in the list, one way or the other.
    let other = contract.doc["other_bounds"].as_object().unwrap();
    for (object, fields) in contract.doc["objects"].as_object().unwrap() {
        for (field, spec) in fields.as_object().unwrap() {
            let spec = bare(spec.as_str().unwrap());
            let open = matches!(list(spec), Some((_, None))) || map(spec).is_some();
            let path = format!("{object}.{field}");
            assert_eq!(
                open,
                maxima.contains_key(&path) || other.contains_key(&path),
                "`{path}` ({spec}): a list or map needs a maximum in `max_items` (or its bound \
                 in `other_bounds`), and nothing else has one"
            );
        }
    }
}

#[test]
fn the_core_accepts_the_listed_json_sizes_and_not_one_byte_more() {
    let mut contract = Contract::load();
    let limits = contract.doc["max_json_bytes"].as_object().unwrap().clone();
    assert_eq!(limits.len(), 3);
    for (path, most) in limits {
        let most = usize::try_from(most.as_u64().unwrap()).unwrap();
        let padded = |contract: &mut Contract, padding: usize| {
            contract.instead = None;
            let pad = json!({"pad": "x".repeat(padding)});
            let value = match path.as_str() {
                "result.components" => {
                    let mut component = contract.object("component");
                    component["body"] = pad;
                    json!([component])
                }
                "result.references" => json!([pad]),
                _ => pad,
            };
            let bytes = value.to_string().len();
            contract.instead = Some((path.clone(), value));
            (contract.object("result"), bytes)
        };
        let (_, empty) = padded(&mut contract, 0);
        let (full, bytes) = padded(&mut contract, most - empty);
        assert_eq!(bytes, most);
        sanitized(&full).unwrap_or_else(|e| panic!("{path} with {most} bytes: {e}"));
        let (over, _) = padded(&mut contract, most - empty + 1);
        assert!(
            sanitized(&over).is_err(),
            "`{path}` accepts over {most} bytes"
        );
    }
    assert_eq!(
        contract.doc["max_result_bytes"].as_u64(),
        Some(hotend_core::jobs::MAX_RESULT_BYTES)
    );
}

#[test]
fn the_example_for_the_other_side_is_current() {
    let path = fixture("engine_result_example.json");
    let text = serde_json::to_string_pretty(&Contract::load().object("result")).unwrap() + "\n";
    if std::env::var_os("HOTEND_UPDATE_ENGINE_EXAMPLE").is_some_and(|v| v == "1") {
        std::fs::write(&path, &text).unwrap();
    }
    let stored = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        stored.replace("\r\n", "\n"),
        text,
        "run this test with HOTEND_UPDATE_ENGINE_EXAMPLE=1"
    );
}

/// The op of a request. A new kind of request does not compile until it is named here; its
/// result then needs its op and its fields in the list.
fn op(request: &EngineRequest) -> &'static str {
    match request {
        EngineRequest::Build(_) => "build",
        EngineRequest::Measure(_) => "measure",
        EngineRequest::Export(_) => "export",
        EngineRequest::Views(_) => "views",
        EngineRequest::Docs(_) => "docs",
        EngineRequest::Prepare(_) => "prepare",
        EngineRequest::Gauge(_) => "gauge",
        EngineRequest::Calibrate(_) => "calibrate",
        EngineRequest::Sweep(_) => "sweep",
    }
}

#[test]
fn the_listed_ops_are_the_ones_the_core_requests() {
    let contract = Contract::load();
    let ops = contract.strings("ops");
    assert_eq!(
        ops,
        [
            "build",
            "measure",
            "export",
            "views",
            "docs",
            "prepare",
            "gauge",
            "calibrate",
            "sweep"
        ]
    );
    let build = EngineRequest::Build(Default::default());
    assert_eq!(op(&build), build.op());
    for never in contract.strings("ops_the_core_never_requests") {
        assert!(!ops.contains(&never));
    }
}
