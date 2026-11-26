//! Small helpers shared by the modules: ids, time, hashing and canonical JSON.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use sha2::{Digest, Sha256};

/// A new UUIDv7 id in its canonical lower-case hyphenated form.
pub fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// True when `id` is a UUID in canonical form. Ids become folder names, so anything else
/// (path separators, dots, upper case) is refused before it reaches the filesystem.
pub fn is_valid_id(id: &str) -> bool {
    id.len() == 36
        && uuid::Uuid::try_parse(id).is_ok_and(|parsed| parsed.hyphenated().to_string() == id)
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Lower-case hex of a byte string.
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // Writing to a String cannot fail.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// SHA-256 of `bytes` as lower-case hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// JSON text with object keys sorted, so equal values always give equal bytes.
pub fn canonical_json(value: &Value) -> String {
    fn write(value: &Value, out: &mut String) {
        match value {
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                out.push('{');
                for (i, key) in keys.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&Value::String(key.clone()).to_string());
                    out.push(':');
                    write(&map[key], out);
                }
                out.push('}');
            }
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(item, out);
                }
                out.push(']');
            }
            scalar => out.push_str(&scalar.to_string()),
        }
    }
    let mut out = String::new();
    write(value, &mut out);
    out
}

/// SHA-256 of the canonical JSON of `value`: the argument hash written to the audit log.
pub fn json_hash(value: &Value) -> String {
    sha256_hex(canonical_json(value).as_bytes())
}

/// `text` cut to at most `max` bytes on a character boundary, with a marker when cut.
pub fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [truncated]", &text[..end])
}

/// True when `name` is a model part or parameter name: `[a-z][a-z0-9_]{0,31}`.
pub fn is_model_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 32
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Fills a buffer from the operating system's random source and returns it as hex.
pub fn random_hex(bytes: usize) -> Result<String, getrandom::Error> {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer)?;
    Ok(hex(&buffer))
}

/// The current time as RFC 3339 in UTC, to the second: `2026-11-17T09:00:00Z`.
pub fn now_rfc3339() -> String {
    jiff::Timestamp::now()
        .strftime("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    #[test]
    fn ids_are_canonical_uuid_v7() {
        let id = new_id();
        assert!(is_valid_id(&id));
        assert_eq!(uuid::Uuid::parse_str(&id).unwrap().get_version_num(), 7);
        for bad in [
            "",
            "..",
            "../x",
            &id.to_uppercase(),
            &id.replace('-', ""),
            "a/b",
        ] {
            assert!(!is_valid_id(bad), "{bad:?} accepted");
        }
    }

    #[test]
    fn canonical_json_sorts_keys_at_every_level() {
        let a = json!({"b": 1, "a": {"y": [1, {"k": 1, "j": 2}], "x": "s\"q"}});
        let b = json!({"a": {"x": "s\"q", "y": [1, {"j": 2, "k": 1}]}, "b": 1});
        assert_eq!(canonical_json(&a), canonical_json(&b));
        assert_eq!(canonical_json(&json!({"b": 1, "a": 2})), r#"{"a":2,"b":1}"#);
        assert_eq!(json_hash(&a), json_hash(&b));
        assert_ne!(json_hash(&a), json_hash(&json!({})));
    }

    #[test]
    fn truncate_respects_character_boundaries() {
        assert_eq!(truncate("abc", 3), "abc");
        assert!(truncate("ééé", 3).starts_with('é'));
        assert!(truncate("abcdef", 2).starts_with("ab…"));
    }

    #[test]
    fn model_names_follow_the_protocol_pattern() {
        assert!(is_model_name("box") && is_model_name("a1_b"));
        for bad in ["", "Box", "1a", "a-b", "a/b", &"a".repeat(33)] {
            assert!(!is_model_name(bad));
        }
    }
}
