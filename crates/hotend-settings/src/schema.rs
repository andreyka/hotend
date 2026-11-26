//! The JSON Schema of a proposal, generated from the field table.
//!
//! Guarantees:
//! - `field` can only be one of the fields the agent or the person may adjust;
//! - `value` has the type and the hard range of exactly that field (one `oneOf` branch per
//!   field), so a string where a number belongs, a number outside the range or a token that
//!   is not in the field's list is refused before any rule sees it;
//! - unknown properties are refused at every level, and the lengths of the list and of the
//!   texts are bounded.
//!
//! The schema cannot say "no field twice" or "the number is finite" (JSON has no other
//! numbers); the proposal step checks the first, and the parser gives the second. Nor can it
//! say that a reason shows something: the step refuses a reason of blanks and invisible
//! characters. `tests/schema.rs` runs the schema through a validator beside the step.

use serde_json::{Map, Value as Json, json};

use crate::bounds::{self, Kind};
use crate::model::Field;

/// The most changes one proposal may carry.
pub const MAX_CHANGES: usize = 16;
/// The longest reason, in characters.
pub const MAX_REASON_CHARS: usize = 240;
/// The longest summary or note, in characters.
pub const MAX_SUMMARY_CHARS: usize = 300;
/// The most plates a version can have, for the optional `plate` argument.
pub const MAX_PLATE: u32 = 64;

/// A text without control characters. Written with code point escapes that both the ECMA 262
/// dialect of JSON Schema and Rust's `regex` read the same way.
const NO_CONTROL_CHARACTERS: &str = "^[^\\u0000-\\u001F\\u007F-\\u009F]*$";

/// The schema of the `value` of one field: its type, and its hard range or its closed list.
pub fn value_schema(field: Field) -> Json {
    match bounds::spec(field).kind {
        Kind::Whole { min, max } => json!({"type": "integer", "minimum": min, "maximum": max}),
        Kind::Decimal { min, max } => json!({"type": "number", "minimum": min, "maximum": max}),
        Kind::Tokens(tokens) => json!({"type": "string", "enum": tokens}),
        Kind::Flag => json!({"type": "boolean"}),
        Kind::Colour => json!({"type": "string", "pattern": "^#[0-9A-Fa-f]{6}$"}),
        // Slots are never proposed; the pattern is here so that the table has no gap.
        Kind::Slot => {
            json!({"type": "string", "pattern": "^([1-9]|1[0-6]|external_main|external_auxiliary)$"})
        }
    }
}

fn text_schema(min_chars: usize, max_chars: usize) -> Json {
    json!({
        "type": "string",
        "minLength": min_chars,
        "maxLength": max_chars,
        "pattern": NO_CONTROL_CHARACTERS,
    })
}

/// The schema of the arguments of a proposal: `{plate?, changes: [{field, value, reason}],
/// summary}`.
pub fn proposal_schema() -> Json {
    let fields: Vec<Field> = bounds::adjustable_fields().collect();
    let names: Vec<&str> = fields.iter().map(|field| field.as_str()).collect();
    let branches: Vec<Json> = fields
        .iter()
        .map(|field| {
            json!({
                "properties": {
                    "field": {"const": field.as_str()},
                    "value": value_schema(*field),
                },
            })
        })
        .collect();

    let mut change = Map::new();
    change.insert("type".to_owned(), json!("object"));
    change.insert("additionalProperties".to_owned(), json!(false));
    change.insert("required".to_owned(), json!(["field", "value", "reason"]));
    change.insert(
        "properties".to_owned(),
        json!({
            "field": {"type": "string", "enum": names},
            "value": {},
            "reason": text_schema(1, MAX_REASON_CHARS),
        }),
    );
    change.insert("oneOf".to_owned(), Json::Array(branches));

    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "required": ["changes", "summary"],
        "properties": {
            "plate": {"type": "integer", "minimum": 1, "maximum": MAX_PLATE},
            "changes": {
                "type": "array",
                "minItems": 1,
                "maxItems": MAX_CHANGES,
                "items": Json::Object(change),
            },
            "summary": text_schema(0, MAX_SUMMARY_CHARS),
        },
    })
}
