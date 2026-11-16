//! Port of packages/ai/src/utils/json-parse.ts

use serde::de::DeserializeOwned;

use crate::types::{JsonObject, JsonValue};
use crate::vendor::partial_json::{allow, parse as partial_parse};

fn is_valid_json_escape(c: char) -> bool {
    matches!(c, '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' | 'u')
}

fn is_control_character(c: char) -> bool {
    (c as u32) <= 0x1f
}

fn escape_control_character(c: char) -> String {
    match c {
        '\x08' => "\\b".to_string(),
        '\x0c' => "\\f".to_string(),
        '\n' => "\\n".to_string(),
        '\r' => "\\r".to_string(),
        '\t' => "\\t".to_string(),
        _ => format!("\\u{:04x}", c as u32),
    }
}

/// Repairs malformed JSON string literals by:
/// - escaping raw control characters inside strings
/// - doubling backslashes before invalid escape characters
pub fn repair_json(json: &str) -> String {
    let mut repaired = String::with_capacity(json.len());
    let mut in_string = false;
    let mut chars = json.chars().peekable();

    while let Some(c) = chars.next() {
        if !in_string {
            repaired.push(c);
            if c == '"' {
                in_string = true;
            }
            continue;
        }

        if c == '"' {
            repaired.push(c);
            in_string = false;
            continue;
        }

        if c == '\\' {
            let Some(&next_char) = chars.peek() else {
                repaired.push_str("\\\\");
                continue;
            };

            if next_char == 'u' {
                let mut lookahead = chars.clone();
                let _ = lookahead.next(); // consume 'u'
                let mut hex = String::with_capacity(4);
                for _ in 0..4 {
                    if let Some(d) = lookahead.next() {
                        hex.push(d);
                    }
                }
                if hex.len() == 4 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                    repaired.push_str("\\u");
                    repaired.push_str(&hex);
                    chars = lookahead;
                    continue;
                }
            }

            if is_valid_json_escape(next_char) {
                repaired.push('\\');
                repaired.push(next_char);
                let _ = chars.next();
                continue;
            }

            repaired.push_str("\\\\");
            continue;
        }

        if is_control_character(c) {
            repaired.push_str(&escape_control_character(c));
        } else {
            repaired.push(c);
        }
    }

    repaired
}

pub fn parse_json_with_repair(json: &str) -> pi_js::Result<JsonValue> {
    parse_json_with_repair_as(json)
}

pub fn parse_json_with_repair_as<T: DeserializeOwned>(json: &str) -> pi_js::Result<T> {
    match pi_js::json::parse_as(json) {
        Ok(value) => Ok(value),
        Err(first_err) => {
            let repaired = repair_json(json);
            if repaired != json {
                return pi_js::json::parse_as(&repaired);
            }
            Err(first_err)
        }
    }
}

/// Attempts to parse potentially incomplete JSON during streaming into a JSON value.
/// Always returns a valid JSON value (empty object `{}` if empty or parsing fails).
pub fn parse_streaming_json_value(partial_json: Option<&str>) -> JsonValue {
    let Some(raw) = partial_json else {
        return JsonValue::Object(serde_json::Map::new());
    };
    if pi_js::str16::trim(raw).is_empty() {
        return JsonValue::Object(serde_json::Map::new());
    }

    if let Ok(value) = parse_json_with_repair(raw) {
        return value;
    }

    if let Ok(value) = partial_parse(raw, Some(allow::ALL)) {
        return value;
    }

    let repaired = repair_json(raw);
    if let Ok(value) = partial_parse(&repaired, Some(allow::ALL)) {
        return value;
    }

    JsonValue::Object(serde_json::Map::new())
}

/// Attempts to parse potentially incomplete JSON during streaming into a tool-arguments object.
/// Always returns a valid object (empty map if input is incomplete before an object or parsing fails).
pub fn parse_streaming_json(partial_json: Option<&str>) -> JsonObject {
    match parse_streaming_json_value(partial_json) {
        JsonValue::Object(map) => map.into_iter().collect(),
        _ => JsonObject::new(),
    }
}
