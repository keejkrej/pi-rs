//! Port of packages/ai/src/providers/schema-builders.ts

use pi_schema::{Annotations, ObjOptions, Schema, StrOptions, obj_with, str_with, unknown_with};
use serde_json::{Map, Value};

/// An object that allows other properties; `additional_properties: true` emits `"additionalProperties": true` explicitly.
pub fn open_object<I, K>(properties: I, annotations: Annotations, additional_properties: bool) -> Schema
where
    I: IntoIterator<Item = (K, Schema)>,
    K: Into<String>,
{
    let schema = obj_with(
        properties,
        ObjOptions {
            annotations,
            open: true,
        },
    );
    if additional_properties {
        schema.with("additionalProperties", true)
    } else {
        schema
    }
}

/// One string literal: `{ "type": "string", "const": value, ...annotations }`.
pub fn string_literal(value: impl Into<String>, annotations: Annotations) -> Schema {
    let mut map = Map::new();
    map.insert("type".to_owned(), Value::String("string".to_owned()));
    map.insert("const".to_owned(), Value::String(value.into()));
    if let Value::Object(ann_map) = str_with(StrOptions {
        annotations,
        ..Default::default()
    })
    .into_value()
    {
        for (k, v) in ann_map {
            if k != "type" {
                map.insert(k, v);
            }
        }
    }
    Schema::from_value(Value::Object(map))
}

/// An object with any string keys and values matching `value`:
/// `{ "type": "object", "patternProperties": { "^.*$": value }, ...annotations }`.
pub fn string_record(value: Schema, annotations: Annotations) -> Schema {
    let mut pattern_props = Map::new();
    pattern_props.insert("^.*$".to_owned(), value.into_value());

    let mut map = Map::new();
    map.insert("type".to_owned(), Value::String("object".to_owned()));
    map.insert("patternProperties".to_owned(), Value::Object(pattern_props));
    if let Value::Object(ann_map) = unknown_with(annotations).into_value() {
        for (k, v) in ann_map {
            map.insert(k, v);
        }
    }
    Schema::from_value(Value::Object(map))
}
