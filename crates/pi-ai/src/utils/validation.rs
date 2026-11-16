//! Port of packages/ai/src/utils/validation.ts

use indexmap::IndexSet;
use pi_js::vendor::typebox::{Schema, Validator, ValueError, compile, value as typebox_value};
use serde_json::{Map, Value};

use crate::types::{Tool, ToolCall};

fn get_schema_types(schema: &Value) -> Vec<&str> {
    match schema.get("type") {
        Some(Value::String(s)) => vec![s.as_str()],
        Some(Value::Array(arr)) => arr.iter().filter_map(|v| v.as_str()).collect(),
        _ => Vec::new(),
    }
}

fn matches_json_type(value: &Value, expected_type: &str) -> bool {
    match expected_type {
        "number" => matches!(value, Value::Number(_)),
        "integer" => value.as_f64().is_some_and(|n| n.fract() == 0.0),
        "boolean" => matches!(value, Value::Bool(_)),
        "string" => matches!(value, Value::String(_)),
        "null" => matches!(value, Value::Null),
        "array" => matches!(value, Value::Array(_)),
        "object" => matches!(value, Value::Object(_)),
        _ => false,
    }
}

fn get_sub_schema_validator(schema: &Value) -> Option<Validator> {
    if !matches!(schema, Value::Object(_) | Value::Bool(_)) {
        return None;
    }
    Some(compile(&Schema::from(schema.clone())))
}

fn coerce_primitive_by_type(value: &Value, target_type: &str) -> Option<Value> {
    match target_type {
        "number" => match value {
            Value::Null => Some(Value::Number(0.into())),
            Value::String(s) if !pi_js::str16::trim(s).is_empty() => {
                let parsed = pi_js::num::number(s);
                if parsed.is_finite() {
                    Some(pi_js::json::number_value(parsed))
                } else {
                    None
                }
            }
            Value::Bool(b) => Some(Value::Number((if *b { 1 } else { 0 }).into())),
            _ => None,
        },
        "integer" => match value {
            Value::Null => Some(Value::Number(0.into())),
            Value::String(s) if !pi_js::str16::trim(s).is_empty() => {
                let parsed = pi_js::num::number(s);
                if parsed.is_finite() && parsed.fract() == 0.0 {
                    Some(pi_js::json::number_value(parsed))
                } else {
                    None
                }
            }
            Value::Bool(b) => Some(Value::Number((if *b { 1 } else { 0 }).into())),
            _ => None,
        },
        "boolean" => match value {
            Value::Null => Some(Value::Bool(false)),
            Value::String(s) if s == "true" => Some(Value::Bool(true)),
            Value::String(s) if s == "false" => Some(Value::Bool(false)),
            Value::Number(n) => match n.as_f64() {
                Some(v) if v == 1.0 => Some(Value::Bool(true)),
                Some(v) if v == 0.0 => Some(Value::Bool(false)),
                _ => None,
            },
            _ => None,
        },
        "string" => match value {
            Value::Null => Some(Value::String(String::new())),
            Value::Number(n) => {
                let s = n
                    .as_f64()
                    .map(pi_js::num::to_js_string)
                    .unwrap_or_else(|| n.to_string());
                Some(Value::String(s))
            }
            Value::Bool(b) => Some(Value::String(if *b { "true" } else { "false" }.to_string())),
            _ => None,
        },
        "null" => match value {
            Value::String(s) if s.is_empty() => Some(Value::Null),
            Value::Number(n) if n.as_f64() == Some(0.0) => Some(Value::Null),
            Value::Bool(false) => Some(Value::Null),
            _ => None,
        },
        _ => None,
    }
}

fn apply_schema_object_coercion(obj: &mut Map<String, Value>, schema: &Value) {
    let props = schema.get("properties").and_then(|v| v.as_object());
    let mut defined_keys: IndexSet<String> = IndexSet::new();

    if let Some(properties) = props {
        for (key, property_schema) in properties {
            defined_keys.insert(key.clone());
            if let Some(existing) = obj.get(key).cloned() {
                obj.insert(key.clone(), coerce_with_json_schema(existing, property_schema));
            }
        }
    }

    if let Some(additional_schema) = schema.get("additionalProperties").filter(|v| v.is_object()) {
        let extra_keys: Vec<String> = obj.keys().filter(|k| !defined_keys.contains(*k)).cloned().collect();
        for key in extra_keys {
            if let Some(existing) = obj.get(&key).cloned() {
                obj.insert(key, coerce_with_json_schema(existing, additional_schema));
            }
        }
    }
}

fn apply_schema_array_coercion(arr: &mut [Value], schema: &Value) {
    match schema.get("items") {
        Some(Value::Array(item_schemas)) => {
            for (index, elem) in arr.iter_mut().enumerate() {
                if let Some(item_schema) = item_schemas.get(index) {
                    let prev = std::mem::replace(elem, Value::Null);
                    *elem = coerce_with_json_schema(prev, item_schema);
                }
            }
        }
        Some(item_schema) if item_schema.is_object() => {
            for elem in arr.iter_mut() {
                let prev = std::mem::replace(elem, Value::Null);
                *elem = coerce_with_json_schema(prev, item_schema);
            }
        }
        _ => {}
    }
}

fn coerce_with_union_schema(value: Value, schemas: &[Value]) -> Value {
    for schema in schemas {
        if let Some(validator) = get_sub_schema_validator(schema) {
            if validator.check(&value) {
                return value;
            }
        }
    }

    for schema in schemas {
        let coerced = coerce_with_json_schema(value.clone(), schema);
        if let Some(validator) = get_sub_schema_validator(schema) {
            if validator.check(&coerced) {
                return coerced;
            }
        }
    }

    value
}

fn coerce_with_json_schema(value: Value, schema: &Value) -> Value {
    let mut next_value = value;

    if let Some(all_of) = schema.get("allOf").and_then(|v| v.as_array()) {
        for nested in all_of {
            next_value = coerce_with_json_schema(next_value, nested);
        }
    }

    if let Some(any_of) = schema.get("anyOf").and_then(|v| v.as_array()) {
        next_value = coerce_with_union_schema(next_value, any_of);
    }

    if let Some(one_of) = schema.get("oneOf").and_then(|v| v.as_array()) {
        next_value = coerce_with_union_schema(next_value, one_of);
    }

    let schema_types = get_schema_types(schema);
    let matches_union_member = schema_types.len() > 1 && schema_types.iter().any(|t| matches_json_type(&next_value, t));

    if !schema_types.is_empty() && !matches_union_member {
        for schema_type in &schema_types {
            if let Some(candidate) = coerce_primitive_by_type(&next_value, schema_type) {
                next_value = candidate;
                break;
            }
        }
    }

    if schema_types.contains(&"object") {
        if let Value::Object(ref mut obj) = next_value {
            apply_schema_object_coercion(obj, schema);
        }
    }

    if schema_types.contains(&"array") {
        if let Value::Array(ref mut arr) = next_value {
            apply_schema_array_coercion(arr, schema);
        }
    }

    next_value
}

fn normalize_optional_nulls(value: &mut Value, schema: &Value) {
    if let Value::Array(arr) = value {
        match schema.get("items") {
            Some(Value::Array(item_schemas)) => {
                for (index, item) in arr.iter_mut().enumerate() {
                    if let Some(item_schema) = item_schemas.get(index) {
                        normalize_optional_nulls(item, item_schema);
                    }
                }
            }
            Some(item_schema) if item_schema.is_object() => {
                for item in arr.iter_mut() {
                    normalize_optional_nulls(item, item_schema);
                }
            }
            _ => {}
        }
        return;
    }

    let (Value::Object(obj), Some(props)) = (value, schema.get("properties").and_then(|v| v.as_object())) else {
        return;
    };

    let required: IndexSet<&str> = schema
        .get("required")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .collect();

    for (key, property_schema) in props {
        let Some(prop_val) = obj.get(key) else {
            continue;
        };
        let is_null = matches!(prop_val, Value::Null);
        let is_ref = property_schema.get("$ref").and_then(|v| v.as_str()).is_some();

        if is_null
            && !required.contains(key.as_str())
            && !is_ref
            && get_sub_schema_validator(property_schema).is_some_and(|v| !v.check(&Value::Null))
        {
            obj.shift_remove(key);
        } else if let Some(prop_val_mut) = obj.get_mut(key) {
            normalize_optional_nulls(prop_val_mut, property_schema);
        }
    }
}

fn format_validation_path(error: &ValueError) -> String {
    let base_path = error.instance_path.trim_start_matches('/').replace('/', ".");
    if error.keyword == "required" {
        if let Some(req_prop) = error
            .params
            .get("requiredProperties")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
        {
            return if base_path.is_empty() {
                req_prop.to_string()
            } else {
                format!("{base_path}.{req_prop}")
            };
        }
    }
    if base_path.is_empty() {
        "root".to_string()
    } else {
        base_path
    }
}

/// Finds a tool by name and validates the tool call arguments against its TypeBox schema.
pub fn validate_tool_call(tools: &[Tool], tool_call: &ToolCall) -> pi_js::Result<Value> {
    let Some(tool) = tools.iter().find(|t| t.name == tool_call.name) else {
        return Err(pi_js::Error::msg(format!("Tool \"{}\" not found", tool_call.name)));
    };
    validate_tool_arguments(tool, tool_call)
}

/// Validates tool call arguments against the tool's TypeBox schema.
pub fn validate_tool_arguments(tool: &Tool, tool_call: &ToolCall) -> pi_js::Result<Value> {
    let schema_value = tool.parameters.to_value();

    let mut raw_map = Map::with_capacity(tool_call.arguments.len());
    for (k, v) in &tool_call.arguments {
        raw_map.insert(k.clone(), v.clone());
    }
    let mut args = Value::Object(raw_map);

    normalize_optional_nulls(&mut args, &schema_value);
    args = typebox_value::convert(&tool.parameters, args);

    let validator = compile(&tool.parameters);
    // In TypeBox 1.3.27, `~kind` is a string property so `TYPEBOX_KIND` symbol is always absent,
    // meaning JSON schema fallback coercion runs after `value::convert`.
    let coerced = coerce_with_json_schema(args.clone(), &schema_value);
    if coerced.is_object() {
        args = coerced;
    } else if coerced != args {
        if validator.check(&coerced) {
            return Ok(coerced);
        }
    }

    if validator.check(&args) {
        return Ok(args);
    }

    let errors = validator.errors(&args);
    let formatted_errors = if errors.is_empty() {
        "Unknown validation error".to_string()
    } else {
        errors
            .iter()
            .map(|error| format!("  - {}: {}", format_validation_path(error), error.message))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let received_json = pi_js::json::stringify_pretty(&tool_call.arguments, "  ");
    Err(pi_js::Error::msg(format!(
        "Validation failed for tool \"{}\":\n{}\n\nReceived arguments:\n{}",
        tool_call.name, formatted_errors, received_json
    )))
}
