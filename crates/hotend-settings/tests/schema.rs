//! The published JSON Schema of a proposal, run through a schema validator: what the schema
//! accepts is what the proposal step reads, and what it refuses the step refuses whole.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{book, calm, inputs};
use hotend_settings::bounds::{self, Kind};
use hotend_settings::inputs::Asker;
use hotend_settings::model::{Family, Field, PlateType};
use hotend_settings::schema::{MAX_CHANGES, MAX_REASON_CHARS, MAX_SUMMARY_CHARS, proposal_schema};
use serde_json::{Value as Json, json};

fn validator() -> jsonschema::Validator {
    let schema = proposal_schema();
    // The schema is itself a valid schema of the dialect it names.
    assert_eq!(
        schema["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert!(jsonschema::meta::is_valid(&schema));
    jsonschema::validator_for(&schema).expect("the proposal schema compiles")
}

/// Whether the proposal step reads the arguments: not refused whole.
fn read_by_hotend(arguments: &Json) -> bool {
    let base = inputs(Family::Pla, PlateType::TexturedPei, vec![calm()]);
    let outcome = book().propose_json(&base, Asker::Agent, arguments).unwrap();
    match &outcome.refused {
        None => true,
        Some(refusal) => {
            assert_eq!(refusal.code, "invalid_arguments");
            false
        }
    }
}

fn one(field: &str, value: Json) -> Json {
    json!({"changes": [{"field": field, "value": value, "reason": "a reason"}], "summary": "s"})
}

#[test]
fn the_schema_and_the_proposal_step_agree_on_the_examples() {
    let validator = validator();
    let long_reason = "r".repeat(MAX_REASON_CHARS + 1);
    let long_summary = "s".repeat(MAX_SUMMARY_CHARS + 1);
    let item = json!({"field": "adhesion.brim_width_mm", "value": 4, "reason": "wider"});
    let many: Vec<Json> = bounds::adjustable_fields()
        .take(MAX_CHANGES + 1)
        .map(|field| match bounds::spec(field).kind {
            Kind::Whole { min, .. } => {
                json!({"field": field.as_str(), "value": min, "reason": "r"})
            }
            Kind::Decimal { min, .. } => {
                json!({"field": field.as_str(), "value": min, "reason": "r"})
            }
            Kind::Tokens(tokens) => {
                json!({"field": field.as_str(), "value": tokens[0], "reason": "r"})
            }
            _ => json!({"field": field.as_str(), "value": true, "reason": "r"}),
        })
        .collect();
    assert_eq!(many.len(), MAX_CHANGES + 1);

    let accepted = [
        one("adhesion.brim_width_mm", json!(4)),
        one("adhesion.brim_width_mm", json!(15)),
        one("motion.max_flow_cap_mm3_s", json!(2.5)),
        one("adhesion.level", json!(2)),
        one("intent", json!("strong")),
        one("supports.enabled", json!(true)),
        one("shell.seam", json!("back")),
        json!({"plate": 2, "changes": [item], "summary": ""}),
        json!({"changes": [item, {"field": "shell.infill_pct", "value": 20, "reason": "lighter"}], "summary": "two"}),
        json!({"changes": [{"field": "shell.infill_pct", "value": 20, "reason": "größer – 大きく"}], "summary": "s"}),
        json!({"changes": many[..MAX_CHANGES], "summary": "s"}),
    ];
    for example in &accepted {
        assert!(validator.is_valid(example), "the schema refuses {example}");
        assert!(read_by_hotend(example), "Hotend refuses {example}");
    }

    let refused = [
        json!(7),
        json!([]),
        json!({}),
        json!({"summary": "s"}),
        json!({"changes": [], "summary": "s"}),
        json!({"changes": {}, "summary": "s"}),
        json!({"changes": [item]}),
        json!({"changes": [item], "summary": 3}),
        json!({"changes": [item], "summary": long_summary}),
        json!({"changes": [item], "summary": "two\nlines"}),
        json!({"changes": [item], "summary": "s", "destination": "C:/Users"}),
        json!({"changes": [item], "summary": "s", "plate": 0}),
        json!({"changes": [item], "summary": "s", "plate": 65}),
        json!({"changes": [item], "summary": "s", "plate": "1"}),
        json!({"changes": many, "summary": "s"}),
        json!({"changes": [{"field": "adhesion.brim_width_mm", "value": 4}], "summary": "s"}),
        json!({"changes": [{"field": "adhesion.brim_width_mm", "value": 4, "reason": ""}], "summary": "s"}),
        json!({"changes": [{"field": "adhesion.brim_width_mm", "value": 4, "reason": long_reason}], "summary": "s"}),
        json!({"changes": [{"field": "adhesion.brim_width_mm", "value": 4, "reason": "bell\u{7}"}], "summary": "s"}),
        json!({"changes": [{"field": "adhesion.brim_width_mm", "value": 4, "reason": "r", "note": "x"}], "summary": "s"}),
        one("adhesion.brim_width_mm", json!(16)),
        one("adhesion.brim_width_mm", json!(-1)),
        one("adhesion.brim_width_mm", json!(4.5)),
        one("adhesion.brim_width_mm", json!("4")),
        one("adhesion.brim_width_mm", json!(null)),
        one("adhesion.brim_width_mm", json!([4])),
        one("motion.max_flow_cap_mm3_s", json!(25.5)),
        one("adhesion.brim_kind", json!("outer; rm -rf")),
        one("adhesion.brim_kind", json!(1)),
        one("supports.enabled", json!("true")),
        one("supports.enabled", json!(1)),
        one("shell.seam", json!("Back")),
        one("intent", json!("fastest")),
        // Names that are no field.
        one("adhesion.brim_width", json!(4)),
        one("brim_width", json!(4)),
        one("", json!(4)),
    ];
    for example in &refused {
        assert!(!validator.is_valid(example), "the schema accepts {example}");
        assert!(!read_by_hotend(example), "Hotend reads {example}");
    }

    // A field nobody may propose is not in the schema. The proposal step is told apart
    // from it on purpose: it answers such a change by itself (`not_settable`), so that the
    // other changes of the call go on, and changes nothing for it.
    let base = inputs(Family::Pla, PlateType::TexturedPei, vec![calm()]);
    for example in [
        one("first_layer.nozzle_c", json!(220)),
        one("setup.plate_type", json!("cool")),
        one("filament.spool", json!("1")),
        one("adhesion.brim_gap_mm", json!(0.1)),
        one("adhesion.skirt_loops", json!(0)),
    ] {
        assert!(
            !validator.is_valid(&example),
            "the schema accepts {example}"
        );
        let outcome = book().propose_json(&base, Asker::Agent, &example).unwrap();
        assert_eq!(outcome.refused, None);
        assert_eq!(outcome.items[0].code, "not_settable", "{example}");
        assert_eq!(outcome.inputs, base, "{example}");
    }
}

/// Every adjustable field at the ends of its hard range and one step outside: the schema
/// and the proposal step draw the line at the same value.
#[test]
fn the_schema_and_the_proposal_step_agree_on_every_hard_range() {
    let validator = validator();
    let mut checked = 0;
    for field in bounds::adjustable_fields() {
        let name = field.as_str();
        let mut examples: Vec<(Json, bool)> = Vec::new();
        match bounds::spec(field).kind {
            Kind::Whole { min, max } => {
                examples.extend([
                    (json!(min), true),
                    (json!(max), true),
                    (json!(min - 1), false),
                    (json!(max + 1), false),
                    (json!(min as f64 + 0.5), false),
                    (json!("x"), false),
                ]);
            }
            Kind::Decimal { min, max } => {
                examples.extend([
                    (json!(min), true),
                    (json!(max), true),
                    (json!(min - 0.01), false),
                    (json!(max + 0.01), false),
                    (json!(true), false),
                ]);
            }
            Kind::Tokens(tokens) => {
                examples.extend(tokens.iter().map(|token| (json!(token), true)));
                examples.extend([(json!("no-such-token"), false), (json!(3), false)]);
            }
            Kind::Flag => {
                examples.extend([
                    (json!(true), true),
                    (json!(false), true),
                    (json!("on"), false),
                    (json!(0), false),
                ]);
            }
            Kind::Colour | Kind::Slot => panic!("{name} is adjustable and has no example"),
        }
        for (value, good) in examples {
            let example = one(name, value);
            assert_eq!(
                validator.is_valid(&example),
                good,
                "the schema on {example}"
            );
            assert_eq!(read_by_hotend(&example), good, "Hotend on {example}");
            checked += 1;
        }
    }
    assert!(checked > 100, "{checked}");
    // The fields that are not adjustable are not in the schema's list at all.
    let names = &proposal_schema()["properties"]["changes"]["items"]["properties"]["field"]["enum"];
    for field in Field::ALL {
        assert_eq!(
            names.as_array().unwrap().contains(&json!(field.as_str())),
            bounds::spec(*field).who.adjustable,
            "{field}"
        );
    }
}
