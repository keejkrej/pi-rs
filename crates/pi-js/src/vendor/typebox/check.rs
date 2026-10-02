//! Interpretive JSON Schema check / error walk (TypeBox `schema/engine`, en_US messages).
//! Keyword groups do not short-circuit while collecting errors. `Check` does.

use serde_json::{Value, json};

use super::format;
use super::{Schema, ValueError};

const MAX_ERRORS: usize = 8;
const MAX_DEPTH: usize = 64;

pub(crate) fn check(schema: &Schema, value: &Value) -> bool {
    let json = schema.to_value();
    check_value(&json, &json, value, 0)
}

pub(crate) fn errors(schema: &Schema, value: &Value) -> Vec<ValueError> {
    let json = schema.to_value();
    let mut ctx = ErrCtx::default();
    let _ = error_value(&json, &json, value, "#", "", &mut ctx, 0);
    ctx.finish()
}

pub(crate) fn regex_test(pattern: &str, flags: &str, text: &str) -> bool {
    match crate::regex::ecma(pattern, flags) {
        Ok(re) => crate::regex::test(&re, text),
        Err(_) => false,
    }
}

pub(crate) fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(n), Value::Number(m)) => match (n.as_f64(), m.as_f64()) {
            (Some(x), Some(y)) => x == y,
            _ => n == m,
        },
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| json_eq(p, q)),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| json_eq(v, w)))
        }
        _ => a == b,
    }
}

#[derive(Clone, Debug)]
struct Raw {
    keyword: String,
    schema_path: String,
    instance_path: String,
    params: Value,
}

#[derive(Default)]
struct ErrCtx {
    errors: Vec<Raw>,
}

impl ErrCtx {
    fn at_capacity(&self) -> bool {
        self.errors.len() >= MAX_ERRORS
    }

    fn add(&mut self, keyword: &str, schema_path: &str, instance_path: &str, params: Value) -> bool {
        if !self.at_capacity() {
            self.errors.push(Raw {
                keyword: keyword.to_string(),
                schema_path: schema_path.to_string(),
                instance_path: instance_path.to_string(),
                params,
            });
        }
        false
    }

    fn absorb(&mut self, other: ErrCtx) {
        for raw in other.errors {
            if self.at_capacity() {
                break;
            }
            self.errors.push(raw);
        }
    }

    fn finish(self) -> Vec<ValueError> {
        self.errors
            .into_iter()
            .map(|raw| ValueError {
                instance_path: raw.instance_path,
                schema_path: raw.schema_path,
                keyword: raw.keyword.clone(),
                message: localize(&raw.keyword, &raw.params),
                params: raw.params,
            })
            .collect()
    }
}

fn localize(keyword: &str, params: &Value) -> String {
    match keyword {
        "additionalProperties" => "must not have additional properties".to_string(),
        "anyOf" => "must match a schema in anyOf".to_string(),
        "boolean" => "schema is false".to_string(),
        "const" => "must be equal to constant".to_string(),
        "contains" => "must contain at least 1 valid item".to_string(),
        "enum" => "must be equal to one of the allowed values".to_string(),
        "exclusiveMaximum" | "exclusiveMinimum" | "maximum" | "minimum" => {
            let cmp = params.get("comparison").and_then(Value::as_str).unwrap_or("");
            format!("must be {cmp} {}", show(params.get("limit")))
        }
        "format" => {
            let f = params.get("format").and_then(Value::as_str).unwrap_or("");
            format!("must match format \"{f}\"")
        }
        "maxItems" => format!("must not have more than {} items", show(params.get("limit"))),
        "maxLength" => format!("must not have more than {} characters", show(params.get("limit"))),
        "maxProperties" => format!("must not have more than {} properties", show(params.get("limit"))),
        "minItems" => format!("must not have fewer than {} items", show(params.get("limit"))),
        "minLength" => format!("must not have fewer than {} characters", show(params.get("limit"))),
        "minProperties" => format!("must not have fewer than {} properties", show(params.get("limit"))),
        "multipleOf" => format!("must be multiple of {}", show(params.get("multipleOf"))),
        "not" => "must not be valid".to_string(),
        "oneOf" => "must match exactly one schema in oneOf".to_string(),
        "pattern" => {
            let p = params.get("pattern").and_then(Value::as_str).unwrap_or("");
            format!("must match pattern \"{p}\"")
        }
        "required" => {
            let names = params
                .get("requiredProperties")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))
                .unwrap_or_default();
            format!("must have required properties {names}")
        }
        "type" => match params.get("type") {
            Some(Value::String(s)) => format!("must be {s}"),
            Some(Value::Array(a)) => {
                let parts: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
                format!("must be either {}", parts.join(" or "))
            }
            _ => "must be".to_string(),
        },
        "uniqueItems" => "must not have duplicate items".to_string(),
        _ => "an unknown validation error occurred".to_string(),
    }
}

fn show(v: Option<&Value>) -> String {
    match v {
        Some(Value::Number(n)) => match n.as_f64() {
            Some(f) => crate::json::number_to_string(f),
            None => n.to_string(),
        },
        Some(Value::String(s)) => s.clone(),
        Some(other) => crate::json::stringify(other),
        None => String::new(),
    }
}

fn has(schema: &Value, key: &str) -> bool {
    schema.as_object().is_some_and(|m| m.contains_key(key))
}

fn as_f64(v: &Value) -> Option<f64> {
    v.as_f64().filter(|n| n.is_finite())
}

fn is_object(v: &Value) -> bool {
    v.is_object()
}

fn check_type_name(name: &str, value: &Value) -> bool {
    match name {
        "object" => is_object(value),
        "array" => value.is_array(),
        "boolean" => value.is_boolean(),
        "integer" => as_f64(value).is_some_and(crate::num::is_integer),
        "number" => as_f64(value).is_some(),
        "null" => value.is_null(),
        "string" => value.is_string(),
        "bigint" | "constructor" | "function" | "symbol" | "undefined" | "void" => false,
        _ => true,
    }
}

fn check_type(schema: &Value, value: &Value) -> bool {
    match schema.get("type") {
        Some(Value::Array(names)) => names
            .iter()
            .filter_map(Value::as_str)
            .any(|n| check_type_name(n, value)),
        Some(Value::String(name)) => check_type_name(name, value),
        Some(_) => true,
        None => true,
    }
}

fn check_value(root: &Value, schema: &Value, value: &Value, depth: usize) -> bool {
    if depth > MAX_DEPTH {
        return true;
    }
    match schema {
        Value::Bool(b) => *b,
        Value::Object(_) => check_object(root, schema, value, depth),
        _ => true,
    }
}

fn check_object(root: &Value, schema: &Value, value: &Value, depth: usize) -> bool {
    if has(schema, "type") && !check_type(schema, value) {
        return false;
    }
    if is_object(value) {
        if has(schema, "required") && !check_required(schema, value) {
            return false;
        }
        if has(schema, "additionalProperties") && !check_additional(root, schema, value, depth) {
            return false;
        }
        if has(schema, "patternProperties") && !check_pattern_props(root, schema, value, depth) {
            return false;
        }
        if has(schema, "properties") && !check_properties(root, schema, value, depth) {
            return false;
        }
        if has(schema, "minProperties") && !check_min_props(schema, value) {
            return false;
        }
        if has(schema, "maxProperties") && !check_max_props(schema, value) {
            return false;
        }
    }
    if let Some(arr) = value.as_array() {
        if has(schema, "additionalItems") && !check_additional_items(root, schema, arr, depth) {
            return false;
        }
        if has(schema, "items") && !check_items(root, schema, arr, depth) {
            return false;
        }
        if has(schema, "maxItems") && (arr.len() as f64) > num(schema, "maxItems") {
            return false;
        }
        if has(schema, "minItems") && (arr.len() as f64) < num(schema, "minItems") {
            return false;
        }
        if has(schema, "prefixItems") && !check_prefix(root, schema, arr, depth) {
            return false;
        }
        if has(schema, "uniqueItems") && !check_unique(schema, arr) {
            return false;
        }
    }
    if let Some(s) = value.as_str() {
        if has(schema, "maxLength") && crate::str16::len(s) as f64 > num(schema, "maxLength") {
            return false;
        }
        if has(schema, "minLength") && (crate::str16::len(s) as f64) < num(schema, "minLength") {
            return false;
        }
        if has(schema, "format") && !check_format(schema, s) {
            return false;
        }
        if has(schema, "pattern") && !check_pattern(schema, s) {
            return false;
        }
    }
    if let Some(n) = as_f64(value) {
        if has(schema, "exclusiveMaximum") && !(n < num(schema, "exclusiveMaximum")) {
            return false;
        }
        if has(schema, "exclusiveMinimum") && !(n > num(schema, "exclusiveMinimum")) {
            return false;
        }
        if has(schema, "maximum") && !(n <= num(schema, "maximum")) {
            return false;
        }
        if has(schema, "minimum") && !(n >= num(schema, "minimum")) {
            return false;
        }
        if has(schema, "multipleOf") && !is_multiple_of(n, num(schema, "multipleOf")) {
            return false;
        }
    }
    if has(schema, "$ref") && !check_ref(root, schema, value, depth) {
        return false;
    }
    if has(schema, "const") && !json_eq(value, &schema["const"]) {
        return false;
    }
    if has(schema, "enum") && !check_enum(schema, value) {
        return false;
    }
    if has(schema, "not") && check_value(root, &schema["not"], value, depth + 1) {
        return false;
    }
    if has(schema, "allOf") && !check_all(root, schema, value, depth) {
        return false;
    }
    if has(schema, "anyOf") && !check_any(root, schema, value, depth) {
        return false;
    }
    if has(schema, "oneOf") && !check_one(root, schema, value, depth) {
        return false;
    }
    true
}

fn num(schema: &Value, key: &str) -> f64 {
    schema.get(key).and_then(as_f64).unwrap_or(0.0)
}

fn check_required(schema: &Value, value: &Value) -> bool {
    let Some(req) = schema.get("required").and_then(Value::as_array) else {
        return true;
    };
    let Some(obj) = value.as_object() else { return true };
    req.iter().filter_map(Value::as_str).all(|k| obj.contains_key(k))
}

fn properties_pattern(schema: &Value) -> Option<String> {
    let mut patterns = Vec::new();
    if let Some(pp) = schema.get("patternProperties").and_then(Value::as_object) {
        patterns.extend(pp.keys().cloned());
    }
    if let Some(props) = schema.get("properties").and_then(Value::as_object) {
        for key in props.keys() {
            patterns.push(format!("^{}$", escape_regex(key)));
        }
    }
    if patterns.is_empty() {
        None
    } else {
        Some(format!("({})", patterns.join("|")))
    }
}

fn escape_regex(key: &str) -> String {
    let mut out = String::new();
    for c in key.chars() {
        if ".*+?^${}()|[]\\".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn key_covered(pattern: &Option<String>, key: &str) -> bool {
    match pattern {
        Some(p) => regex_test(p, "u", key),
        None => false,
    }
}

fn check_additional(root: &Value, schema: &Value, value: &Value, depth: usize) -> bool {
    let Some(obj) = value.as_object() else { return true };
    let add = &schema["additionalProperties"];
    let pattern = properties_pattern(schema);
    for (key, child) in obj {
        if key_covered(&pattern, key) {
            continue;
        }
        if !check_value(root, add, child, depth + 1) {
            return false;
        }
    }
    true
}

fn check_pattern_props(root: &Value, schema: &Value, value: &Value, depth: usize) -> bool {
    let Some(pp) = schema.get("patternProperties").and_then(Value::as_object) else {
        return true;
    };
    let Some(obj) = value.as_object() else { return true };
    for (pattern, sub) in pp {
        for (key, child) in obj {
            if regex_test(pattern, "u", key) && !check_value(root, sub, child, depth + 1) {
                return false;
            }
        }
    }
    true
}

fn check_properties(root: &Value, schema: &Value, value: &Value, depth: usize) -> bool {
    let Some(props) = schema.get("properties").and_then(Value::as_object) else {
        return true;
    };
    let Some(obj) = value.as_object() else { return true };
    for (key, sub) in props {
        if let Some(child) = obj.get(key)
            && !check_value(root, sub, child, depth + 1)
        {
            return false;
        }
    }
    true
}

fn check_min_props(schema: &Value, value: &Value) -> bool {
    value
        .as_object()
        .is_none_or(|m| m.len() as f64 >= num(schema, "minProperties"))
}

fn check_max_props(schema: &Value, value: &Value) -> bool {
    value
        .as_object()
        .is_none_or(|m| m.len() as f64 <= num(schema, "maxProperties"))
}

fn check_additional_items(root: &Value, schema: &Value, arr: &[Value], depth: usize) -> bool {
    let Some(items) = schema.get("items").and_then(Value::as_array) else {
        return true;
    };
    let Some(add) = schema.get("additionalItems") else {
        return true;
    };
    for (index, item) in arr.iter().enumerate() {
        if index >= items.len() && !check_value(root, add, item, depth + 1) {
            return false;
        }
    }
    true
}

fn check_items(root: &Value, schema: &Value, arr: &[Value], depth: usize) -> bool {
    match schema.get("items") {
        Some(Value::Array(items)) => {
            for (index, sub) in items.iter().enumerate() {
                if arr.len() > index && !check_value(root, sub, &arr[index], depth + 1) {
                    return false;
                }
            }
            true
        }
        Some(sub) => {
            let offset = schema
                .get("prefixItems")
                .and_then(Value::as_array)
                .map(|a| a.len())
                .unwrap_or(0);
            arr.iter()
                .enumerate()
                .skip(offset)
                .all(|(_, item)| check_value(root, sub, item, depth + 1))
        }
        None => true,
    }
}

fn check_prefix(root: &Value, schema: &Value, arr: &[Value], depth: usize) -> bool {
    if arr.is_empty() {
        return true;
    }
    let Some(items) = schema.get("prefixItems").and_then(Value::as_array) else {
        return true;
    };
    for (index, sub) in items.iter().enumerate() {
        if arr.len() > index && !check_value(root, sub, &arr[index], depth + 1) {
            return false;
        }
    }
    true
}

fn check_unique(schema: &Value, arr: &[Value]) -> bool {
    if schema.get("uniqueItems") == Some(&Value::Bool(false)) {
        return true;
    }
    let mut seen: Vec<&Value> = Vec::new();
    for item in arr {
        if seen.iter().any(|s| json_eq(s, item)) {
            return false;
        }
        seen.push(item);
    }
    true
}

fn check_format(schema: &Value, text: &str) -> bool {
    match schema.get("format").and_then(Value::as_str) {
        Some(name) => format::test(name, text),
        None => true,
    }
}

fn check_pattern(schema: &Value, text: &str) -> bool {
    match schema.get("pattern").and_then(Value::as_str) {
        Some(pattern) => regex_test(pattern, "u", text),
        None => true,
    }
}

fn is_multiple_of(dividend: f64, divisor: f64) -> bool {
    if !dividend.is_finite() {
        return true;
    }
    if crate::num::is_integer(dividend) && ((1.0 / divisor) % 1.0) == 0.0 {
        return true;
    }
    let m = dividend % divisor;
    m.abs().min((m - divisor).abs()).min((m + divisor).abs()) < 1e-10
}

fn check_enum(schema: &Value, value: &Value) -> bool {
    schema
        .get("enum")
        .and_then(Value::as_array)
        .is_some_and(|opts| opts.iter().any(|o| json_eq(value, o)))
}

fn check_all(root: &Value, schema: &Value, value: &Value, depth: usize) -> bool {
    schema
        .get("allOf")
        .and_then(Value::as_array)
        .is_none_or(|xs| xs.iter().all(|s| check_value(root, s, value, depth + 1)))
}

fn check_any(root: &Value, schema: &Value, value: &Value, depth: usize) -> bool {
    schema
        .get("anyOf")
        .and_then(Value::as_array)
        .is_some_and(|xs| xs.iter().any(|s| check_value(root, s, value, depth + 1)))
}

fn check_one(root: &Value, schema: &Value, value: &Value, depth: usize) -> bool {
    let Some(xs) = schema.get("oneOf").and_then(Value::as_array) else {
        return true;
    };
    xs.iter().filter(|s| check_value(root, s, value, depth + 1)).count() == 1
}

fn unescape_pointer(seg: &str) -> String {
    seg.replace("~1", "/").replace("~0", "~")
}

fn pointer_get<'a>(mut cur: &'a Value, pointer: &str) -> Option<&'a Value> {
    if pointer.is_empty() {
        return Some(cur);
    }
    for seg in pointer.split('/').skip(1) {
        let key = unescape_pointer(seg);
        cur = match cur {
            Value::Object(map) => map.get(&key)?,
            Value::Array(arr) => {
                if key == "0"
                    || (key.as_bytes().first().is_some_and(|c| *c != b'0') && key.bytes().all(|c| c.is_ascii_digit()))
                {
                    arr.get(key.parse::<usize>().ok()?)?
                } else {
                    return None;
                }
            }
            _ => return None,
        };
    }
    Some(cur)
}

fn resolve_ref<'a>(root: &'a Value, reference: &str) -> Option<&'a Value> {
    let frag = reference.strip_prefix('#')?;
    if frag.is_empty() {
        return Some(root);
    }
    let decoded = crate::uri::decode_uri_component(frag).ok()?;
    if !decoded.starts_with('/') {
        return None;
    }
    pointer_get(root, &decoded)
}

fn check_ref(root: &Value, schema: &Value, value: &Value, depth: usize) -> bool {
    let Some(reference) = schema.get("$ref").and_then(Value::as_str) else {
        return true;
    };
    match resolve_ref(root, reference) {
        Some(target) => check_value(root, target, value, depth + 1),
        None => false,
    }
}

fn error_value(
    root: &Value,
    schema: &Value,
    value: &Value,
    schema_path: &str,
    instance_path: &str,
    ctx: &mut ErrCtx,
    depth: usize,
) -> bool {
    if ctx.at_capacity() {
        return false;
    }
    if depth > MAX_DEPTH {
        return true;
    }
    match schema {
        Value::Bool(true) => true,
        Value::Bool(false) => ctx.add("boolean", schema_path, instance_path, json!({})),
        Value::Object(_) => error_object(root, schema, value, schema_path, instance_path, ctx, depth),
        _ => true,
    }
}

fn error_object(
    root: &Value,
    schema: &Value,
    value: &Value,
    schema_path: &str,
    instance_path: &str,
    ctx: &mut ErrCtx,
    depth: usize,
) -> bool {
    let mut ok = true;
    if has(schema, "type") {
        ok &= error_type(schema, value, schema_path, instance_path, ctx);
    }
    if is_object(value) {
        let mut g = true;
        if has(schema, "required") {
            g &= error_required(schema, value, schema_path, instance_path, ctx);
        }
        if has(schema, "additionalProperties") {
            g &= error_additional(root, schema, value, schema_path, instance_path, ctx, depth);
        }
        if has(schema, "patternProperties") {
            g &= error_pattern_props(root, schema, value, schema_path, instance_path, ctx, depth);
        }
        if has(schema, "properties") {
            g &= error_properties(root, schema, value, schema_path, instance_path, ctx, depth);
        }
        if has(schema, "minProperties") {
            g &= error_limit(
                check_min_props(schema, value),
                "minProperties",
                schema,
                "minProperties",
                schema_path,
                instance_path,
                ctx,
            );
        }
        if has(schema, "maxProperties") {
            g &= error_limit(
                check_max_props(schema, value),
                "maxProperties",
                schema,
                "maxProperties",
                schema_path,
                instance_path,
                ctx,
            );
        }
        ok &= g;
    }
    if let Some(arr) = value.as_array() {
        let mut g = true;
        if has(schema, "additionalItems") {
            g &= error_additional_items(root, schema, arr, schema_path, instance_path, ctx, depth);
        }
        if has(schema, "items") {
            g &= error_items(root, schema, arr, schema_path, instance_path, ctx, depth);
        }
        if has(schema, "maxItems") {
            g &= error_limit(
                (arr.len() as f64) <= num(schema, "maxItems"),
                "maxItems",
                schema,
                "maxItems",
                schema_path,
                instance_path,
                ctx,
            );
        }
        if has(schema, "minItems") {
            g &= error_limit(
                (arr.len() as f64) >= num(schema, "minItems"),
                "minItems",
                schema,
                "minItems",
                schema_path,
                instance_path,
                ctx,
            );
        }
        if has(schema, "prefixItems") {
            g &= error_prefix(root, schema, arr, schema_path, instance_path, ctx, depth);
        }
        if has(schema, "uniqueItems") {
            g &= error_unique(schema, arr, schema_path, instance_path, ctx);
        }
        ok &= g;
    }
    if let Some(text) = value.as_str() {
        let mut g = true;
        if has(schema, "maxLength") {
            g &= error_limit(
                crate::str16::len(text) as f64 <= num(schema, "maxLength"),
                "maxLength",
                schema,
                "maxLength",
                schema_path,
                instance_path,
                ctx,
            );
        }
        if has(schema, "minLength") {
            g &= error_limit(
                crate::str16::len(text) as f64 >= num(schema, "minLength"),
                "minLength",
                schema,
                "minLength",
                schema_path,
                instance_path,
                ctx,
            );
        }
        if has(schema, "format") {
            g &= error_format(schema, text, schema_path, instance_path, ctx);
        }
        if has(schema, "pattern") {
            g &= error_pattern(schema, text, schema_path, instance_path, ctx);
        }
        ok &= g;
    }
    if let Some(n) = as_f64(value) {
        let mut g = true;
        if has(schema, "exclusiveMaximum") {
            g &= error_cmp(
                n < num(schema, "exclusiveMaximum"),
                "exclusiveMaximum",
                "<",
                schema,
                schema_path,
                instance_path,
                ctx,
            );
        }
        if has(schema, "exclusiveMinimum") {
            g &= error_cmp(
                n > num(schema, "exclusiveMinimum"),
                "exclusiveMinimum",
                ">",
                schema,
                schema_path,
                instance_path,
                ctx,
            );
        }
        if has(schema, "maximum") {
            g &= error_cmp(
                n <= num(schema, "maximum"),
                "maximum",
                "<=",
                schema,
                schema_path,
                instance_path,
                ctx,
            );
        }
        if has(schema, "minimum") {
            g &= error_cmp(
                n >= num(schema, "minimum"),
                "minimum",
                ">=",
                schema,
                schema_path,
                instance_path,
                ctx,
            );
        }
        if has(schema, "multipleOf") {
            g &= if is_multiple_of(n, num(schema, "multipleOf")) {
                true
            } else {
                let multiple = schema.get("multipleOf").cloned().unwrap_or(Value::Null);
                ctx.add(
                    "multipleOf",
                    schema_path,
                    instance_path,
                    json!({ "multipleOf": multiple }),
                )
            };
        }
        ok &= g;
    }
    if has(schema, "$ref") {
        ok &= error_ref(root, schema, value, instance_path, ctx, depth);
    }
    if has(schema, "const") {
        ok &= if json_eq(value, &schema["const"]) {
            true
        } else {
            ctx.add(
                "const",
                schema_path,
                instance_path,
                json!({ "allowedValue": schema["const"].clone() }),
            )
        };
    }
    if has(schema, "enum") {
        ok &= if check_enum(schema, value) {
            true
        } else {
            ctx.add(
                "enum",
                schema_path,
                instance_path,
                json!({ "allowedValues": schema["enum"].clone() }),
            )
        };
    }
    if has(schema, "not") {
        ok &= if !check_value(root, &schema["not"], value, depth + 1) {
            true
        } else {
            ctx.add("not", schema_path, instance_path, json!({}))
        };
    }
    if has(schema, "allOf") {
        ok &= error_all(root, schema, value, schema_path, instance_path, ctx, depth);
    }
    if has(schema, "anyOf") {
        ok &= error_any(root, schema, value, schema_path, instance_path, ctx, depth);
    }
    if has(schema, "oneOf") {
        ok &= error_one(root, schema, value, schema_path, instance_path, ctx, depth);
    }
    ok
}

fn error_type(schema: &Value, value: &Value, schema_path: &str, instance_path: &str, ctx: &mut ErrCtx) -> bool {
    if check_type(schema, value) {
        true
    } else {
        ctx.add(
            "type",
            schema_path,
            instance_path,
            json!({ "type": schema["type"].clone() }),
        )
    }
}

fn error_required(schema: &Value, value: &Value, schema_path: &str, instance_path: &str, ctx: &mut ErrCtx) -> bool {
    let Some(req) = schema.get("required").and_then(Value::as_array) else {
        return true;
    };
    let Some(obj) = value.as_object() else { return true };
    let missing: Vec<Value> = req
        .iter()
        .filter_map(Value::as_str)
        .filter(|k| !obj.contains_key(*k))
        .map(|k| Value::String(k.to_string()))
        .collect();
    if missing.is_empty() {
        true
    } else {
        ctx.add(
            "required",
            schema_path,
            instance_path,
            json!({ "requiredProperties": missing }),
        )
    }
}

fn error_additional(
    root: &Value,
    schema: &Value,
    value: &Value,
    schema_path: &str,
    instance_path: &str,
    ctx: &mut ErrCtx,
    depth: usize,
) -> bool {
    let Some(obj) = value.as_object() else { return true };
    let add = &schema["additionalProperties"];
    let pattern = properties_pattern(schema);
    let mut bad = Vec::new();
    let mut ok = true;
    for (key, child) in obj {
        if key_covered(&pattern, key) {
            continue;
        }
        let sp = format!("{schema_path}/additionalProperties");
        let ip = format!("{instance_path}/{key}");
        if !error_value(root, add, child, &sp, &ip, ctx, depth + 1) {
            bad.push(Value::String(key.clone()));
            ok = false;
        }
    }
    if ok {
        true
    } else {
        ctx.add(
            "additionalProperties",
            schema_path,
            instance_path,
            json!({ "additionalProperties": bad }),
        )
    }
}

fn error_pattern_props(
    root: &Value,
    schema: &Value,
    value: &Value,
    schema_path: &str,
    instance_path: &str,
    ctx: &mut ErrCtx,
    depth: usize,
) -> bool {
    let Some(pp) = schema.get("patternProperties").and_then(Value::as_object) else {
        return true;
    };
    let Some(obj) = value.as_object() else { return true };
    let mut ok = true;
    for (pattern, sub) in pp {
        let sp = format!("{schema_path}/patternProperties/{pattern}");
        for (key, child) in obj {
            if !regex_test(pattern, "u", key) {
                continue;
            }
            let ip = format!("{instance_path}/{key}");
            if !error_value(root, sub, child, &sp, &ip, ctx, depth + 1) {
                ok = false;
            }
        }
    }
    ok
}

fn error_properties(
    root: &Value,
    schema: &Value,
    value: &Value,
    schema_path: &str,
    instance_path: &str,
    ctx: &mut ErrCtx,
    depth: usize,
) -> bool {
    let Some(props) = schema.get("properties").and_then(Value::as_object) else {
        return true;
    };
    let Some(obj) = value.as_object() else { return true };
    let mut ok = true;
    for (key, sub) in props {
        let Some(child) = obj.get(key) else { continue };
        let sp = format!("{schema_path}/properties/{key}");
        let ip = format!("{instance_path}/{key}");
        if !error_value(root, sub, child, &sp, &ip, ctx, depth + 1) {
            ok = false;
        }
    }
    ok
}

fn error_limit(
    pass: bool,
    keyword: &str,
    schema: &Value,
    key: &str,
    schema_path: &str,
    instance_path: &str,
    ctx: &mut ErrCtx,
) -> bool {
    if pass {
        true
    } else {
        let limit = schema.get(key).cloned().unwrap_or(Value::Null);
        ctx.add(keyword, schema_path, instance_path, json!({ "limit": limit }))
    }
}

fn error_cmp(
    pass: bool,
    keyword: &str,
    comparison: &str,
    schema: &Value,
    schema_path: &str,
    instance_path: &str,
    ctx: &mut ErrCtx,
) -> bool {
    if pass {
        true
    } else {
        let limit = schema.get(keyword).cloned().unwrap_or(Value::Null);
        ctx.add(
            keyword,
            schema_path,
            instance_path,
            json!({ "comparison": comparison, "limit": limit }),
        )
    }
}

fn error_format(schema: &Value, text: &str, schema_path: &str, instance_path: &str, ctx: &mut ErrCtx) -> bool {
    if check_format(schema, text) {
        true
    } else {
        let format = schema.get("format").cloned().unwrap_or(Value::Null);
        ctx.add("format", schema_path, instance_path, json!({ "format": format }))
    }
}

fn error_pattern(schema: &Value, text: &str, schema_path: &str, instance_path: &str, ctx: &mut ErrCtx) -> bool {
    if check_pattern(schema, text) {
        true
    } else {
        let pattern = schema.get("pattern").cloned().unwrap_or(Value::Null);
        ctx.add("pattern", schema_path, instance_path, json!({ "pattern": pattern }))
    }
}

fn error_additional_items(
    root: &Value,
    schema: &Value,
    arr: &[Value],
    schema_path: &str,
    instance_path: &str,
    ctx: &mut ErrCtx,
    depth: usize,
) -> bool {
    let Some(items) = schema.get("items").and_then(Value::as_array) else {
        return true;
    };
    let Some(add) = schema.get("additionalItems") else {
        return true;
    };
    for (index, item) in arr.iter().enumerate() {
        if index < items.len() {
            continue;
        }
        let sp = format!("{schema_path}/additionalItems");
        let ip = format!("{instance_path}/{index}");
        if !error_value(root, add, item, &sp, &ip, ctx, depth + 1) {
            return false;
        }
    }
    true
}

fn error_items(
    root: &Value,
    schema: &Value,
    arr: &[Value],
    schema_path: &str,
    instance_path: &str,
    ctx: &mut ErrCtx,
    depth: usize,
) -> bool {
    match schema.get("items") {
        Some(Value::Array(items)) => {
            let mut ok = true;
            for (index, sub) in items.iter().enumerate() {
                if arr.len() <= index {
                    continue;
                }
                let sp = format!("{schema_path}/items/{index}");
                let ip = format!("{instance_path}/{index}");
                if !error_value(root, sub, &arr[index], &sp, &ip, ctx, depth + 1) {
                    ok = false;
                }
            }
            ok
        }
        Some(sub) => {
            let offset = schema
                .get("prefixItems")
                .and_then(Value::as_array)
                .map(|a| a.len())
                .unwrap_or(0);
            let mut ok = true;
            for (index, item) in arr.iter().enumerate().skip(offset) {
                let sp = format!("{schema_path}/items");
                let ip = format!("{instance_path}/{index}");
                if !error_value(root, sub, item, &sp, &ip, ctx, depth + 1) {
                    ok = false;
                }
            }
            ok
        }
        None => true,
    }
}

fn error_prefix(
    root: &Value,
    schema: &Value,
    arr: &[Value],
    schema_path: &str,
    instance_path: &str,
    ctx: &mut ErrCtx,
    depth: usize,
) -> bool {
    if arr.is_empty() {
        return true;
    }
    let Some(items) = schema.get("prefixItems").and_then(Value::as_array) else {
        return true;
    };
    let mut ok = true;
    for (index, sub) in items.iter().enumerate() {
        if arr.len() <= index {
            continue;
        }
        let sp = format!("{schema_path}/prefixItems/{index}");
        let ip = format!("{instance_path}/{index}");
        if !error_value(root, sub, &arr[index], &sp, &ip, ctx, depth + 1) {
            ok = false;
        }
    }
    ok
}

fn error_unique(schema: &Value, arr: &[Value], schema_path: &str, instance_path: &str, ctx: &mut ErrCtx) -> bool {
    if schema.get("uniqueItems") == Some(&Value::Bool(false)) {
        return true;
    }
    let mut seen: Vec<&Value> = Vec::new();
    let mut dups = Vec::new();
    for (index, item) in arr.iter().enumerate() {
        if seen.iter().any(|s| json_eq(s, item)) {
            dups.push(Value::from(index));
        } else {
            seen.push(item);
        }
    }
    if dups.is_empty() {
        true
    } else {
        ctx.add(
            "uniqueItems",
            schema_path,
            instance_path,
            json!({ "duplicateItems": dups }),
        )
    }
}

fn error_ref(root: &Value, schema: &Value, value: &Value, instance_path: &str, ctx: &mut ErrCtx, depth: usize) -> bool {
    let Some(reference) = schema.get("$ref").and_then(Value::as_str) else {
        return true;
    };
    let mut child = ErrCtx::default();
    let ok = match resolve_ref(root, reference) {
        Some(target) => error_value(root, target, value, "#", instance_path, &mut child, depth + 1),
        None => error_value(
            root,
            &Value::Bool(false),
            value,
            "#",
            instance_path,
            &mut child,
            depth + 1,
        ),
    };
    if !ok {
        ctx.absorb(child);
    }
    ok
}

fn error_all(
    root: &Value,
    schema: &Value,
    value: &Value,
    schema_path: &str,
    instance_path: &str,
    ctx: &mut ErrCtx,
    depth: usize,
) -> bool {
    let Some(xs) = schema.get("allOf").and_then(Value::as_array) else {
        return true;
    };
    let mut failed = Vec::new();
    let mut passed = 0;
    for (index, sub) in xs.iter().enumerate() {
        let mut child = ErrCtx::default();
        let sp = format!("{schema_path}/allOf/{index}");
        if error_value(root, sub, value, &sp, instance_path, &mut child, depth + 1) {
            passed += 1;
        } else {
            failed.push(child);
        }
    }
    if passed == xs.len() {
        true
    } else {
        for child in failed {
            ctx.absorb(child);
        }
        false
    }
}

fn error_any(
    root: &Value,
    schema: &Value,
    value: &Value,
    schema_path: &str,
    instance_path: &str,
    ctx: &mut ErrCtx,
    depth: usize,
) -> bool {
    let Some(xs) = schema.get("anyOf").and_then(Value::as_array) else {
        return true;
    };
    let mut failed = Vec::new();
    let mut passed = 0;
    for (index, sub) in xs.iter().enumerate() {
        let mut child = ErrCtx::default();
        let sp = format!("{schema_path}/anyOf/{index}");
        if error_value(root, sub, value, &sp, instance_path, &mut child, depth + 1) {
            passed += 1;
        } else {
            failed.push(child);
        }
    }
    if passed > 0 {
        true
    } else {
        for child in failed {
            ctx.absorb(child);
        }
        ctx.add("anyOf", schema_path, instance_path, json!({}))
    }
}

fn error_one(
    root: &Value,
    schema: &Value,
    value: &Value,
    schema_path: &str,
    instance_path: &str,
    ctx: &mut ErrCtx,
    depth: usize,
) -> bool {
    let Some(xs) = schema.get("oneOf").and_then(Value::as_array) else {
        return true;
    };
    let mut failed = Vec::new();
    let mut passing = Vec::new();
    for (index, sub) in xs.iter().enumerate() {
        let mut child = ErrCtx::default();
        let sp = format!("{schema_path}/oneOf/{index}");
        if error_value(root, sub, value, &sp, instance_path, &mut child, depth + 1) {
            passing.push(Value::from(index));
        } else {
            failed.push(child);
        }
    }
    if passing.len() == 1 {
        true
    } else {
        if passing.is_empty() {
            for child in failed {
                ctx.absorb(child);
            }
        }
        ctx.add(
            "oneOf",
            schema_path,
            instance_path,
            json!({ "passingSchemas": passing }),
        )
    }
}
