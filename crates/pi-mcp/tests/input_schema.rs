use indexmap::IndexMap;
use pi_mcp::normalize_mcp_input_schema;
use pretty_assertions::assert_eq;
use serde_json::{Value, from_value, json};

fn map_of(value: Value) -> IndexMap<String, Value> {
    from_value(value).unwrap()
}

#[test]
fn adds_type_object_and_empty_properties_when_missing() {
    assert_eq!(
        normalize_mcp_input_schema(&map_of(json!({}))),
        map_of(json!({ "type": "object", "properties": {} }))
    );
    assert_eq!(
        normalize_mcp_input_schema(&map_of(json!({ "type": "object" }))),
        map_of(json!({ "type": "object", "properties": {} }))
    );
}

#[test]
fn keeps_the_schemas_own_keywords() {
    let schema = map_of(json!({
        "type": "object",
        "properties": { "query": { "type": "string" } },
        "required": ["query"],
        "additionalProperties": false
    }));
    assert_eq!(normalize_mcp_input_schema(&schema), schema);

    assert_eq!(
        normalize_mcp_input_schema(&map_of(json!({
            "properties": { "a": {} },
            "$defs": { "x": {} }
        }))),
        map_of(json!({
            "properties": { "a": {} },
            "$defs": { "x": {} },
            "type": "object"
        }))
    );
}
