//! Port of packages/ai/src/utils/validation.ts

#![allow(dead_code, unused_variables)]

use pi_js::vendor::typebox::{Schema, Validator, ValueError};
use serde_json::{Map, Value};

use crate::types::{Tool, ToolCall};

// PORT: `JsonSchemaObject` is `Schema` (TypeBox builders and plain JSON schemas).
// `TLocalizedValidationError` is `ValueError`.
//
// PORT: `TYPEBOX_KIND` is `Symbol.for("TypeBox.Kind")`. TypeBox 1.3.27 stores `~kind` as a
// string key and does not define that symbol, so `getOwnPropertySymbols` does not see it.
// `Schema::is_typebox` is not this check.
//
// PORT: `validatorCache` is a `WeakMap` keyed by schema object identity. It lives in
// `get_validator`; `Schema` has no JS object identity.

fn get_schema_types(schema: &Schema) -> Vec<String> {
    todo!("port: get_schema_types")
}

fn matches_json_type(value: &Value, r#type: &str) -> bool {
    todo!("port: matches_json_type")
}

fn get_sub_schema_validator(schema: &Schema) -> Option<Validator> {
    todo!("port: get_sub_schema_validator")
}

fn coerce_primitive_by_type(value: Value, r#type: &str) -> Value {
    todo!("port: coerce_primitive_by_type")
}

fn apply_schema_object_coercion(value: &mut Map<String, Value>, schema: &Schema) {
    todo!("port: apply_schema_object_coercion")
}

fn apply_schema_array_coercion(value: &mut Vec<Value>, schema: &Schema) {
    todo!("port: apply_schema_array_coercion")
}

fn coerce_with_union_schema(value: Value, schemas: &[Schema]) -> Value {
    todo!("port: coerce_with_union_schema")
}

fn coerce_with_json_schema(value: Value, schema: &Schema) -> Value {
    todo!("port: coerce_with_json_schema")
}

fn normalize_optional_nulls(value: &mut Value, schema: &Schema) {
    todo!("port: normalize_optional_nulls")
}

fn get_validator(schema: &Schema) -> pi_js::Result<Validator> {
    todo!("port: get_validator")
}

fn format_validation_path(error: &ValueError) -> String {
    todo!("port: format_validation_path")
}

/// Finds a tool by name and validates the tool call arguments against its TypeBox schema
/// @param tools Array of tool definitions
/// @param toolCall The tool call from the LLM
/// @returns The validated arguments
/// @throws Error if tool is not found or validation fails
pub fn validate_tool_call(tools: &[Tool], tool_call: &ToolCall) -> pi_js::Result<Value> {
    todo!("port: validate_tool_call")
}

/// Validates tool call arguments against the tool's TypeBox schema
/// @param tool The tool definition with TypeBox schema
/// @param toolCall The tool call from the LLM
/// @returns The validated (and potentially coerced) arguments
/// @throws Error with formatted message if validation fails
pub fn validate_tool_arguments(tool: &Tool, tool_call: &ToolCall) -> pi_js::Result<Value> {
    todo!("port: validate_tool_arguments")
}
