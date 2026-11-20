//! Port of packages/mcp/src/input-schema.ts

use indexmap::IndexMap;
use serde_json::{Map, Value};

/// The tool's input schema as an object schema. Tool parameters must be objects, MCP servers may omit
/// `type`, and some providers reject object schemas without `properties`. Other keywords are kept.
pub fn normalize_mcp_input_schema(schema: &IndexMap<String, Value>) -> IndexMap<String, Value> {
    let mut normalized = schema.clone();
    if normalized.get("type").is_none_or(Value::is_null) {
        normalized.insert("type".to_string(), Value::String("object".to_string()));
    }
    if !normalized.contains_key("properties") {
        normalized.insert("properties".to_string(), Value::Object(Map::new()));
    }
    normalized
}
