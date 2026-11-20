//! Port of packages/ai/src/api/constrained-sampling.ts

use std::fmt;

use indexmap::{IndexMap, IndexSet};
use pi_js::vendor::typebox::Schema as TSchema;
use pi_js::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::types::{
    ConstrainedSampling, ConstrainedSamplingConfig, GrammarFormat, JsonObject, JsonSchemaStrict, JsonValue, Tool,
};

#[derive(Debug)]
struct UnsupportedStrictJsonSchemaError(String);

impl fmt::Display for UnsupportedStrictJsonSchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UnsupportedStrictJsonSchemaError {}

impl UnsupportedStrictJsonSchemaError {
    fn into_error(self) -> Error {
        Error::typed("UnsupportedStrictJsonSchemaError", self)
    }
}

/// Returns true when a provider's strict mode rejects this schema keyword with this value.
pub type UnsupportedStrictSchemaKeywordCheck<'a> = &'a dyn Fn(&str, &JsonValue) -> bool;

const UNSUPPORTED_STRICT_SCHEMA_KEYS: [&str; 16] = [
    "$ref",
    "$defs",
    "definitions",
    "allOf",
    "oneOf",
    "patternProperties",
    "dependentSchemas",
    "dependencies",
    "unevaluatedProperties",
    "propertyNames",
    "contains",
    "prefixItems",
    "not",
    "if",
    "then",
    "else",
];

fn is_json_schema_object(value: &Value) -> bool {
    value.is_object()
}

fn is_structured_schema(schema: &Value) -> bool {
    let Some(obj) = schema.as_object() else {
        return false;
    };
    let has_object_or_array_type = match obj.get("type") {
        Some(Value::String(s)) => s == "object" || s == "array",
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(Value::as_str)
            .any(|s| s == "object" || s == "array"),
        _ => false,
    };
    has_object_or_array_type || obj.contains_key("properties") || obj.contains_key("items")
}

fn schema_allows_null(schema: &Value) -> bool {
    let Some(obj) = schema.as_object() else {
        return false;
    };
    if obj.get("type").is_some_and(|t| {
        t.as_str() == Some("null")
            || t.as_array()
                .is_some_and(|arr| arr.iter().any(|v| v.as_str() == Some("null")))
    }) {
        return true;
    }
    if obj.get("const").is_some_and(Value::is_null)
        || obj
            .get("enum")
            .and_then(Value::as_array)
            .is_some_and(|arr| arr.iter().any(Value::is_null))
    {
        return true;
    }
    obj.get("anyOf")
        .and_then(Value::as_array)
        .is_some_and(|arr| arr.iter().any(schema_allows_null))
}

fn make_json_schema_node_strict(
    schema: &mut Value,
    is_unsupported_keyword: Option<UnsupportedStrictSchemaKeywordCheck<'_>>,
) -> Result<()> {
    let Some(obj) = schema.as_object_mut() else {
        return Err(UnsupportedStrictJsonSchemaError("boolean schemas are unsupported".to_string()).into_error());
    };

    for key in UNSUPPORTED_STRICT_SCHEMA_KEYS {
        if obj.contains_key(key) {
            return Err(UnsupportedStrictJsonSchemaError(format!("{key} schemas are unsupported")).into_error());
        }
    }

    if let Some(check) = is_unsupported_keyword {
        for (key, value) in obj.iter() {
            if check(key, value) {
                return Err(UnsupportedStrictJsonSchemaError(format!(
                    "{key}: {} is unsupported",
                    pi_js::json::stringify(value)
                ))
                .into_error());
            }
        }
    }

    if let Some(any_of) = obj.get_mut("anyOf") {
        let Some(variants) = any_of.as_array_mut() else {
            return Err(
                UnsupportedStrictJsonSchemaError("anyOf must contain at least one schema".to_string()).into_error(),
            );
        };
        if variants.is_empty() {
            return Err(
                UnsupportedStrictJsonSchemaError("anyOf must contain at least one schema".to_string()).into_error(),
            );
        }
        for variant in variants.iter_mut() {
            if is_structured_schema(variant) {
                return Err(
                    UnsupportedStrictJsonSchemaError("object and array unions are unsupported".to_string())
                        .into_error(),
                );
            }
            make_json_schema_node_strict(variant, is_unsupported_keyword)?;
        }
    }

    if let Some(items) = obj.get_mut("items") {
        if items.is_array() {
            return Err(UnsupportedStrictJsonSchemaError("tuple schemas are unsupported".to_string()).into_error());
        }
        make_json_schema_node_strict(items, is_unsupported_keyword)?;
    }

    let is_object_schema = obj.get("type").and_then(Value::as_str) == Some("object");
    if obj.contains_key("properties") && !is_object_schema {
        return Err(UnsupportedStrictJsonSchemaError("properties require type object".to_string()).into_error());
    }
    if !is_object_schema {
        return Ok(());
    }

    if let Some(additional_properties) = obj.get("additionalProperties") {
        if *additional_properties != Value::Bool(false) {
            return Err(UnsupportedStrictJsonSchemaError(
                "schema-valued or true additionalProperties is unsupported".to_string(),
            )
            .into_error());
        }
    }

    if let Some(properties) = obj.get("properties") {
        if !is_json_schema_object(properties) {
            return Err(
                UnsupportedStrictJsonSchemaError("object properties must be a schema map".to_string()).into_error(),
            );
        }
    }

    let required_keys: IndexSet<String> = match obj.get("required") {
        None => IndexSet::new(),
        Some(Value::Array(arr)) => {
            let mut set = IndexSet::with_capacity(arr.len());
            for item in arr {
                let Some(s) = item.as_str() else {
                    return Err(
                        UnsupportedStrictJsonSchemaError("object required must be a string array".to_string())
                            .into_error(),
                    );
                };
                set.insert(s.to_string());
            }
            set
        }
        Some(_) => {
            return Err(
                UnsupportedStrictJsonSchemaError("object required must be a string array".to_string()).into_error(),
            );
        }
    };

    let property_names: Vec<String> = match obj.get_mut("properties").and_then(Value::as_object_mut) {
        Some(properties) => {
            let names: Vec<String> = properties.keys().cloned().collect();
            if required_keys.iter().any(|key| !names.contains(key)) {
                return Err(
                    UnsupportedStrictJsonSchemaError("required contains an unknown property".to_string()).into_error(),
                );
            }
            for (key, prop) in properties.iter_mut() {
                make_json_schema_node_strict(prop, is_unsupported_keyword)?;
                if !required_keys.contains(key) && !schema_allows_null(prop) {
                    let mut null_schema = Map::new();
                    null_schema.insert("type".to_string(), Value::String("null".to_string()));
                    let mut union_schema = Map::new();
                    union_schema.insert(
                        "anyOf".to_string(),
                        Value::Array(vec![prop.clone(), Value::Object(null_schema)]),
                    );
                    *prop = Value::Object(union_schema);
                }
            }
            names
        }
        None => {
            if !required_keys.is_empty() {
                return Err(
                    UnsupportedStrictJsonSchemaError("required contains an unknown property".to_string()).into_error(),
                );
            }
            Vec::new()
        }
    };

    obj.insert(
        "required".to_string(),
        Value::Array(property_names.into_iter().map(Value::String).collect()),
    );
    obj.insert("additionalProperties".to_string(), Value::Bool(false));
    Ok(())
}

/// Convert a tool schema to the strict subset expected by provider constrained sampling.
pub fn make_strict_json_schema(
    schema: &TSchema,
    is_unsupported_keyword: Option<UnsupportedStrictSchemaKeywordCheck<'_>>,
) -> Result<JsonObject> {
    let mut cloned = schema.to_value();
    if !is_json_schema_object(&cloned) {
        return Err(UnsupportedStrictJsonSchemaError("root schema must have type object".to_string()).into_error());
    }
    make_json_schema_node_strict(&mut cloned, is_unsupported_keyword)?;
    let Some(obj) = cloned.as_object() else {
        return Err(UnsupportedStrictJsonSchemaError("root schema must have type object".to_string()).into_error());
    };
    if obj.get("type").and_then(Value::as_str) != Some("object") {
        return Err(UnsupportedStrictJsonSchemaError("root schema must have type object".to_string()).into_error());
    }
    Ok(obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
}

/// Returns true for a tool that asks for strict JSON-schema sampling only when possible (`strict: "prefer"`).
pub fn prefers_json_schema_strict_sampling(tool: &Tool) -> bool {
    matches!(
        &tool.constrained_sampling,
        Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::JsonSchema {
            strict: JsonSchemaStrict::Prefer
        }))
    )
}

pub fn get_json_schema_tool_parameters(tool: &Tool, strict: Option<bool>) -> Result<TSchema> {
    if strict == Some(true) {
        let strict_obj = make_strict_json_schema(&tool.parameters, None)?;
        let map: Map<String, Value> = strict_obj.into_iter().collect();
        Ok(TSchema::from(Value::Object(map)))
    } else {
        Ok(tool.parameters.clone())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GrammarSamplingFormat {
    Lark,
    Regex,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrammarConstrainedSampling {
    pub format: GrammarSamplingFormat,
    pub definition: String,
    pub input_property: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GrammarToolInputJsonBuffer {
    pub input: String,
    pub started: bool,
    pub closed: bool,
}

pub fn get_grammar_tool_input(tool_name: &str, arguments: &JsonObject, input_property: &str) -> Result<String> {
    match arguments.get(input_property).and_then(Value::as_str) {
        Some(input) => Ok(input.to_string()),
        None => Err(Error::msg(format!(
            "Grammar tool call \"{tool_name}\" requires argument \"{input_property}\" to be a string."
        ))),
    }
}

fn json_string_inner(value: &str) -> String {
    let encoded = pi_js::json::stringify(&Value::String(value.to_string()));
    encoded
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(&encoded)
        .to_owned()
}

fn grammar_tool_input_json_start(buffer: &mut GrammarToolInputJsonBuffer, input_property: &str) -> String {
    if buffer.started {
        return String::new();
    }
    buffer.started = true;
    let prop_json = pi_js::json::stringify(&Value::String(input_property.to_string()));
    format!("{{{prop_json}:\"")
}

/// Append `input_delta` to a streaming grammar tool input and return the matching piece of its JSON arguments text,
/// or `None` for an empty delta. Costs time linear in the delta, never in the input so far.
pub fn append_grammar_tool_input_json_delta(
    buffer: &mut GrammarToolInputJsonBuffer,
    input_property: &str,
    input_delta: &str,
) -> Result<Option<String>> {
    if buffer.closed {
        return Err(Error::msg(format!(
            "grammar tool input for property \"{input_property}\" changed after it was closed"
        )));
    }
    if input_delta.is_empty() {
        return Ok(None);
    }
    buffer.input.push_str(input_delta);
    Ok(Some(format!(
        "{}{}",
        grammar_tool_input_json_start(buffer, input_property),
        json_string_inner(input_delta)
    )))
}

/// Close a streaming grammar tool input and return the rest of its JSON arguments text. `final_input` is the complete
/// input when the provider sends one; it must continue the input streamed so far. Closing again with the same input
/// returns `None`.
pub fn close_grammar_tool_input_json(
    buffer: &mut GrammarToolInputJsonBuffer,
    input_property: &str,
    final_input: Option<&str>,
) -> Result<Option<String>> {
    if buffer.closed {
        if final_input.is_none() || final_input == Some(buffer.input.as_str()) {
            return Ok(None);
        }
        return Err(Error::msg(format!(
            "grammar tool input for property \"{input_property}\" changed after it was closed"
        )));
    }
    let rest = match final_input {
        Some(final_val) => {
            let Some(suffix) = final_val.strip_prefix(&buffer.input) else {
                return Err(Error::msg(format!(
                    "grammar tool input for property \"{input_property}\" changed non-monotonically"
                )));
            };
            let rest = suffix.to_owned();
            buffer.input.clear();
            buffer.input.push_str(final_val);
            rest
        }
        None => String::new(),
    };
    buffer.closed = true;
    Ok(Some(format!(
        "{}{}\"}}",
        grammar_tool_input_json_start(buffer, input_property),
        json_string_inner(&rest)
    )))
}

fn infer_grammar_input_property(tool: &Tool) -> Result<String> {
    let schema = tool.parameters.to_value();
    let Some(obj) = schema.as_object() else {
        return Err(Error::msg(
            "grammar constrained sampling requires an object parameter schema",
        ));
    };
    if obj.get("type").and_then(Value::as_str) != Some("object") {
        return Err(Error::msg(
            "grammar constrained sampling requires an object parameter schema",
        ));
    }

    let Some(required) = obj.get("required").and_then(Value::as_array) else {
        return Err(Error::msg(
            "grammar constrained sampling requires exactly one required string property",
        ));
    };
    if required.len() != 1 {
        return Err(Error::msg(
            "grammar constrained sampling requires exactly one required string property",
        ));
    }
    let Some(input_property) = required[0].as_str() else {
        return Err(Error::msg(
            "grammar constrained sampling requires exactly one required string property",
        ));
    };

    let Some(prop_schema) = obj
        .get("properties")
        .and_then(Value::as_object)
        .and_then(|props| props.get(input_property))
    else {
        return Err(Error::msg(format!(
            "grammar constrained sampling requires a properties entry for {input_property}"
        )));
    };

    if prop_schema
        .as_object()
        .and_then(|p| p.get("type"))
        .and_then(Value::as_str)
        != Some("string")
    {
        return Err(Error::msg(format!(
            "grammar constrained sampling property {input_property} must have type string"
        )));
    }

    Ok(input_property.to_string())
}

/// Decide whether a JSON-schema tool is sent in strict mode. `is_unsupported_keyword` lets a provider
/// reject extra keywords its strict mode does not accept, so "prefer" tools fall back to non-strict.
pub fn resolve_json_schema_strict_sampling(
    tool: &Tool,
    supports_strict_mode: bool,
    is_unsupported_keyword: Option<UnsupportedStrictSchemaKeywordCheck<'_>>,
) -> Result<Option<bool>> {
    let Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::JsonSchema { strict })) =
        &tool.constrained_sampling
    else {
        return Ok(None);
    };

    if supports_strict_mode {
        match make_strict_json_schema(&tool.parameters, is_unsupported_keyword) {
            Ok(_) => Ok(Some(true)),
            Err(error) => {
                if error.downcast_ref::<UnsupportedStrictJsonSchemaError>().is_none() {
                    return Err(error);
                }
                if *strict != JsonSchemaStrict::Require {
                    return Ok(None);
                }
                Err(Error::msg(format!(
                    "Tool \"{}\" requires JSON-schema constrained sampling, but {error}.",
                    tool.name
                )))
            }
        }
    } else if *strict == JsonSchemaStrict::Require {
        Err(Error::msg(format!(
            "Tool \"{}\" requires JSON-schema constrained sampling, but strict tools are unsupported.",
            tool.name
        )))
    } else {
        Ok(None)
    }
}

pub fn resolve_grammar_constrained_sampling(
    tool: &Tool,
    supports_openai_grammar_tools: bool,
) -> Result<Option<GrammarConstrainedSampling>> {
    let Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::Grammar { variants })) = &tool.constrained_sampling
    else {
        return Ok(None);
    };

    if !supports_openai_grammar_tools {
        return Ok(None);
    }

    let lark_definition = variants
        .get(&GrammarFormat::OpenAILark)
        .filter(|s| !pi_js::str16::trim(s).is_empty());
    let regex_definition = variants
        .get(&GrammarFormat::OpenAIRegex)
        .filter(|s| !pi_js::str16::trim(s).is_empty());

    if lark_definition.is_none() && regex_definition.is_none() {
        return Err(Error::msg(format!(
            "Tool \"{}\" cannot use grammar constrained sampling: no supported grammar variant was provided.",
            tool.name
        )));
    }

    let input_property = infer_grammar_input_property(tool).map_err(|err| {
        Error::msg(format!(
            "Tool \"{}\" cannot use grammar constrained sampling: {err}.",
            tool.name
        ))
    })?;

    if let Some(def) = lark_definition {
        Ok(Some(GrammarConstrainedSampling {
            format: GrammarSamplingFormat::Lark,
            definition: def.clone(),
            input_property,
        }))
    } else if let Some(def) = regex_definition {
        Ok(Some(GrammarConstrainedSampling {
            format: GrammarSamplingFormat::Regex,
            definition: def.clone(),
            input_property,
        }))
    } else {
        Ok(None)
    }
}

pub fn create_grammar_tool_input_properties(
    tools: Option<&[Tool]>,
    supports_openai_grammar_tools: bool,
) -> Result<IndexMap<String, String>> {
    let mut properties = IndexMap::new();
    for tool in tools.unwrap_or(&[]) {
        if let Some(grammar) = resolve_grammar_constrained_sampling(tool, supports_openai_grammar_tools)? {
            properties.insert(tool.name.clone(), grammar.input_property);
        }
    }
    Ok(properties)
}
