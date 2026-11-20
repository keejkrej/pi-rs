//! Port of packages/ai/src/utils/validation.ts

use pi_schema::{CoercingValidator, CompileOptions, UnresolvedRefs, compile_coercing};
use serde_json::{Map, Value};

use crate::types::{Tool, ToolCall};

fn compile_tool_validator(schema: &Tool) -> pi_js::Result<CoercingValidator> {
    // A `$ref` that names nothing (a dangling definition in an MCP server's schema) rejects only values at its place,
    // instead of making every call of the tool fail.
    compile_coercing(
        schema.parameters.to_value(),
        CompileOptions::default()
            .with_default_formats()
            .unresolved_refs(UnresolvedRefs::Reject),
    )
    .map_err(|err| pi_js::Error::msg(format!("Invalid schema for tool \"{}\": {err}", schema.name)))
}

/// Format a `pi-schema` JSON Pointer path (`/edits/0/oldText` -> `edits.0.oldText`, `""` -> `root`).
fn format_validation_path(pointer: &str) -> String {
    if pointer.is_empty() {
        return "root".to_owned();
    }
    pointer
        .trim_start_matches('/')
        .split('/')
        .map(|segment| segment.replace("~1", "/").replace("~0", "~"))
        .collect::<Vec<_>>()
        .join(".")
}

/// Finds a tool by name and validates the tool call arguments against its JSON Schema.
pub fn validate_tool_call(tools: &[Tool], tool_call: &ToolCall) -> pi_js::Result<Value> {
    let Some(tool) = tools.iter().find(|t| t.name == tool_call.name) else {
        return Err(pi_js::Error::msg(format!("Tool \"{}\" not found", tool_call.name)));
    };
    validate_tool_arguments(tool, tool_call)
}

/// Validates tool call arguments against the tool's JSON Schema (any JSON Schema: `pi-schema`, `TypeBox`, plain objects).
///
/// Arguments are coerced first via `pi-schema::compile_coercing` (`"42"` -> `42`, single value -> array,
/// `null` -> omitted optional property). Returns an owned copy so downstream mutations do not mutate the transcript.
pub fn validate_tool_arguments(tool: &Tool, tool_call: &ToolCall) -> pi_js::Result<Value> {
    if let Some(args_error) = &tool_call.arguments_error {
        return Err(pi_js::Error::msg(format!(
            "Invalid arguments for tool \"{}\": {args_error}",
            tool_call.name
        )));
    }

    let mut raw_map = Map::with_capacity(tool_call.arguments.len());
    for (k, v) in &tool_call.arguments {
        raw_map.insert(k.clone(), v.clone());
    }
    let args = Value::Object(raw_map);

    let validator = compile_tool_validator(tool)?;
    match validator.parse(&args) {
        Ok(coerced) => Ok(coerced),
        Err(errors) => {
            let formatted = if errors.is_empty() {
                "Unknown validation error".to_owned()
            } else {
                errors
                    .iter()
                    .map(|error| format!("  - {}: {}", format_validation_path(&error.path), error.message))
                    .collect::<Vec<_>>()
                    .join("\n")
            };

            let args_json = pi_js::json::stringify_pretty(
                &Value::Object(
                    tool_call
                        .arguments
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                ),
                "  ",
            );
            let error_message = format!(
                "Validation failed for tool \"{}\":\n{}\n\nReceived arguments:\n{}",
                tool_call.name, formatted, args_json
            );
            Err(pi_js::Error::msg(error_message))
        }
    }
}
