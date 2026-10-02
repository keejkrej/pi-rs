//! `Value.Convert` / `Clean` / `Default` (TypeBox 1.3.27).
//!
//! Dispatch is on the hidden `~kind`. A schema parsed from JSON has no kind, so convert and
//! clean leave the value alone. `Default` still applies a root `default` keyword.
//! Tuple holes that are JS `undefined` are written as JSON `null` (`JSON.stringify`).

use serde_json::{Map, Value};

use super::check::{check, json_eq, regex_test};
use super::{Kind, Schema};

pub(crate) fn convert(schema: &Schema, value: Value) -> Value {
    convert_type(schema, value)
}

pub(crate) fn clean(schema: &Schema, value: Value) -> Value {
    clean_type(schema, value)
}

pub(crate) fn default(schema: &Schema, value: Value) -> Value {
    from_default(schema, Some(value)).unwrap_or(Value::Null)
}

fn convert_type(schema: &Schema, value: Value) -> Value {
    match schema.kind() {
        Some(Kind::Array) => convert_array(schema, value),
        Some(Kind::Boolean) => try_boolean(&value).unwrap_or(value),
        Some(Kind::Enum) => convert_type(&super::enum_to_union(schema), value),
        Some(Kind::Integer) => convert_integer(value),
        Some(Kind::Intersect) => convert_type(&evaluate_type(schema), value),
        Some(Kind::Literal) => convert_literal(schema, value),
        Some(Kind::Null) => try_null(&value).unwrap_or(value),
        Some(Kind::Number) => try_number(&value).unwrap_or(value),
        Some(Kind::Object) => convert_object(schema, value),
        Some(Kind::Record) => convert_record(schema, value),
        Some(Kind::String) => try_string(&value).unwrap_or(value),
        Some(Kind::Tuple) => convert_tuple(schema, value),
        Some(Kind::Union) => convert_union(schema, value),
        _ => value,
    }
}

fn clean_type(schema: &Schema, value: Value) -> Value {
    match schema.kind() {
        Some(Kind::Array) => clean_array(schema, value),
        Some(Kind::Intersect) => clean_type(&evaluate_type(schema), value),
        Some(Kind::Object) => clean_object(schema, value),
        Some(Kind::Record) => clean_record(schema, value),
        Some(Kind::Tuple) => clean_tuple(schema, value),
        Some(Kind::Union) => clean_union(schema, value),
        _ => value,
    }
}

fn from_default(schema: &Schema, value: Option<Value>) -> Option<Value> {
    let value = if value.is_none() && schema.has_key("default") {
        schema.json("default").cloned()
    } else {
        value
    };
    match schema.kind() {
        Some(Kind::Array) => default_array(schema, value),
        Some(Kind::Intersect) => from_default(&evaluate_type(schema), value),
        Some(Kind::Object) => default_object(schema, value),
        Some(Kind::Record) => default_record(schema, value),
        Some(Kind::Tuple) => default_tuple(schema, value),
        Some(Kind::Union) => default_union(schema, value),
        _ => value,
    }
}

fn try_number(value: &Value) -> Option<Value> {
    match value {
        Value::Bool(b) => Some(crate::json::number_value(if *b { 1.0 } else { 0.0 })),
        Value::Number(n) => {
            let x = n.as_f64()?;
            if x.is_finite() { Some(value.clone()) } else { None }
        }
        Value::Null => Some(crate::json::number_value(0.0)),
        Value::String(s) => {
            let n = crate::num::number(s);
            if n.is_finite() {
                return Some(crate::json::number_value(n));
            }
            match s.to_lowercase().as_str() {
                "false" => Some(crate::json::number_value(0.0)),
                "true" => Some(crate::json::number_value(1.0)),
                _ => None,
            }
        }
        _ => None,
    }
}

fn try_string(value: &Value) -> Option<Value> {
    match value {
        Value::Bool(true) => Some(Value::String("true".to_string())),
        Value::Bool(false) => Some(Value::String("false".to_string())),
        Value::Number(n) => Some(Value::String(crate::num::to_js_string(n.as_f64().unwrap_or(0.0)))),
        Value::Null => Some(Value::String("null".to_string())),
        Value::String(_) => Some(value.clone()),
        _ => None,
    }
}

fn try_boolean(value: &Value) -> Option<Value> {
    match value {
        Value::Bool(_) => Some(value.clone()),
        Value::Number(n) => {
            let x = n.as_f64()?;
            if x == 0.0 {
                Some(Value::Bool(false))
            } else if x == 1.0 {
                Some(Value::Bool(true))
            } else {
                None
            }
        }
        Value::Null => Some(Value::Bool(false)),
        Value::String(s) => match s.to_lowercase().as_str() {
            "false" => Some(Value::Bool(false)),
            "true" => Some(Value::Bool(true)),
            _ if s == "0" => Some(Value::Bool(false)),
            _ if s == "1" => Some(Value::Bool(true)),
            _ => None,
        },
        _ => None,
    }
}

fn try_null(value: &Value) -> Option<Value> {
    match value {
        Value::Bool(false) | Value::Null => Some(Value::Null),
        Value::Bool(true) => None,
        Value::Number(n) => {
            if n.as_f64() == Some(0.0) {
                Some(Value::Null)
            } else {
                None
            }
        }
        Value::String(s) => {
            let lower = s.to_lowercase();
            if lower == "undefined" || lower == "null" || s.is_empty() || s == "0" {
                Some(Value::Null)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn convert_integer(value: Value) -> Value {
    match try_number(&value) {
        Some(Value::Number(n)) => crate::json::number_value(n.as_f64().unwrap_or(0.0).trunc()),
        Some(other) => other,
        None => value,
    }
}

fn convert_literal(schema: &Schema, value: Value) -> Value {
    let Some(constant) = schema.json("const") else {
        return value;
    };
    if json_eq(constant, &value) {
        return value;
    }
    let coerced = match constant {
        Value::Bool(_) => try_boolean(&value),
        Value::Number(_) => try_number(&value),
        Value::String(_) => try_string(&value),
        Value::Null => try_null(&value),
        _ => None,
    };
    match coerced {
        Some(next) if json_eq(constant, &next) => next,
        _ => value,
    }
}

fn convert_array(schema: &Schema, value: Value) -> Value {
    let items = schema.child("items").cloned().unwrap_or_else(Schema::bare);
    let values = match value {
        Value::Array(items) => items,
        other => vec![other],
    };
    Value::Array(values.into_iter().map(|v| convert_type(&items, v)).collect())
}

fn convert_tuple(schema: &Schema, value: Value) -> Value {
    let Value::Array(mut values) = value else { return value };
    let items = schema.list("items");
    let n = values.len().min(items.len());
    for i in 0..n {
        let current = std::mem::replace(&mut values[i], Value::Null);
        values[i] = convert_type(&items[i], current);
    }
    Value::Array(values)
}

fn convert_union(schema: &Schema, value: Value) -> Value {
    if schema.list("anyOf").iter().any(|arm| check(arm, &value)) {
        return value;
    }
    for arm in schema.list("anyOf") {
        let candidate = convert_type(arm, value.clone());
        if check(schema, &candidate) {
            return candidate;
        }
    }
    value
}

fn convert_object(schema: &Schema, value: Value) -> Value {
    let Value::Object(mut map) = value else { return value };
    let props = schema.map("properties").to_vec();
    let keys: Vec<String> = map.keys().cloned().collect();
    for (name, prop) in &props {
        let pattern = format!("^{name}$");
        for key in &keys {
            if !regex_test(&pattern, "", key) {
                continue;
            }
            if let Some(current) = map.get(key).cloned() {
                map.insert(key.clone(), convert_type(prop, current));
            }
        }
    }
    if let Extra::Schema(additional) = extra(schema) {
        for (name, _) in &props {
            let pattern = format!("^{name}$");
            for key in &keys {
                if regex_test(&pattern, "", key) {
                    continue;
                }
                if let Some(current) = map.get(key).cloned() {
                    map.insert(key.clone(), convert_type(&additional, current));
                }
            }
        }
    }
    Value::Object(map)
}

fn convert_record(schema: &Schema, value: Value) -> Value {
    let Value::Object(mut map) = value else { return value };
    let entries = schema.map("patternProperties").to_vec();
    let keys: Vec<String> = map.keys().cloned().collect();
    for (pattern, sub) in &entries {
        let wrapped = format!("^{pattern}$");
        for key in &keys {
            if !regex_test(&wrapped, "", key) {
                continue;
            }
            if let Some(current) = map.get(key).cloned() {
                map.insert(key.clone(), convert_type(sub, current));
            }
        }
    }
    if let Extra::Schema(additional) = extra(schema) {
        for (pattern, _) in &entries {
            let wrapped = format!("^{pattern}$");
            for key in &keys {
                if regex_test(&wrapped, "", key) {
                    continue;
                }
                if let Some(current) = map.get(key).cloned() {
                    map.insert(key.clone(), convert_type(&additional, current));
                }
            }
        }
    }
    Value::Object(map)
}

fn clean_array(schema: &Schema, value: Value) -> Value {
    let Value::Array(values) = value else { return value };
    let Some(items) = schema.child("items").cloned() else {
        return Value::Array(values);
    };
    Value::Array(values.into_iter().map(|v| clean_type(&items, v)).collect())
}

fn clean_tuple(schema: &Schema, value: Value) -> Value {
    let Value::Array(mut values) = value else { return value };
    let items = schema.list("items");
    let n = values.len().min(items.len());
    for i in 0..n {
        let current = std::mem::replace(&mut values[i], Value::Null);
        values[i] = clean_type(&items[i], current);
    }
    values.truncate(n);
    Value::Array(values)
}

fn clean_union(schema: &Schema, value: Value) -> Value {
    for arm in schema.list("anyOf") {
        let cleaned = clean_type(arm, value.clone());
        if check(arm, &cleaned) {
            return cleaned;
        }
    }
    value
}

fn clean_object(schema: &Schema, value: Value) -> Value {
    let Value::Object(map) = value else { return value };
    let props = schema.map("properties");
    let additional = extra(schema);
    let mut out = Map::new();
    for (key, current) in map {
        if let Some((_, prop)) = props.iter().find(|(name, _)| name == &key) {
            out.insert(key, clean_type(prop, current));
            continue;
        }
        match &additional {
            Extra::Bool(true) => {
                out.insert(key, current);
            }
            Extra::Schema(sub) if check(sub, &current) => {
                out.insert(key, clean_type(sub, current));
            }
            _ => {}
        }
    }
    Value::Object(out)
}

fn clean_record(schema: &Schema, value: Value) -> Value {
    let Value::Object(map) = value else { return value };
    let Some((pattern, rec)) = schema.map("patternProperties").first() else {
        return Value::Object(map);
    };
    let additional = extra(schema);
    let mut out = Map::new();
    for (key, current) in map {
        if regex_test(pattern, "", &key) {
            out.insert(key, clean_type(rec, current));
            continue;
        }
        match &additional {
            Extra::Bool(true) => {
                out.insert(key, current);
            }
            Extra::Schema(sub) if check(sub, &current) => {
                out.insert(key, clean_type(sub, current));
            }
            _ => {}
        }
    }
    Value::Object(out)
}

fn default_array(schema: &Schema, value: Option<Value>) -> Option<Value> {
    let Some(Value::Array(values)) = value else {
        return value;
    };
    let Some(items) = schema.child("items").cloned() else {
        return Some(Value::Array(values));
    };
    Some(Value::Array(
        values
            .into_iter()
            .map(|v| from_default(&items, Some(v)).unwrap_or(Value::Null))
            .collect(),
    ))
}

fn default_tuple(schema: &Schema, value: Option<Value>) -> Option<Value> {
    let Some(Value::Array(mut values)) = value else {
        return value;
    };
    let items = schema.list("items").to_vec();
    let max = items.len().max(values.len());
    for i in 0..max {
        if i < items.len() {
            let current = if i < values.len() {
                Some(values[i].clone())
            } else {
                None
            };
            // JS writes `undefined` into the hole. JSON.stringify emits null.
            let next = from_default(&items[i], current).unwrap_or(Value::Null);
            if i < values.len() {
                values[i] = next;
            } else {
                values.push(next);
            }
        }
    }
    Some(Value::Array(values))
}

fn default_union(schema: &Schema, value: Option<Value>) -> Option<Value> {
    for arm in schema.list("anyOf") {
        let result = from_default(arm, value.clone());
        if let Some(ref next) = result {
            if check(arm, next) {
                return result;
            }
        }
    }
    value
}

fn default_object(schema: &Schema, value: Option<Value>) -> Option<Value> {
    let Some(Value::Object(mut map)) = value else {
        return value;
    };
    let props = schema.map("properties").to_vec();
    for (key, prop) in &props {
        let current = map.get(key).cloned();
        let property_value = from_default(prop, current);
        let unassignable = property_value.is_none() && (prop.is_optional() || !prop.has_key("default"));
        if unassignable {
            continue;
        }
        if let Some(next) = property_value {
            map.insert(key.clone(), next);
        }
    }
    if let Extra::Schema(additional) = extra(schema) {
        let known: Vec<String> = props.iter().map(|(k, _)| k.clone()).collect();
        let keys: Vec<String> = map.keys().cloned().collect();
        for key in keys {
            if known.iter().any(|name| name == &key) {
                continue;
            }
            if let Some(current) = map.get(&key).cloned() {
                if let Some(next) = from_default(&additional, Some(current)) {
                    map.insert(key, next);
                }
            }
        }
    }
    Some(Value::Object(map))
}

fn default_record(schema: &Schema, value: Option<Value>) -> Option<Value> {
    let Some(Value::Object(mut map)) = value else {
        return value;
    };
    let Some((pattern, rec)) = schema
        .map("patternProperties")
        .first()
        .map(|(p, s)| (p.clone(), s.clone()))
    else {
        return Some(Value::Object(map));
    };
    if rec.has_key("default") {
        let keys: Vec<String> = map.keys().cloned().collect();
        for key in keys {
            if !regex_test(&pattern, "", &key) {
                continue;
            }
            if let Some(current) = map.get(&key).cloned() {
                if let Some(next) = from_default(&rec, Some(current)) {
                    map.insert(key, next);
                }
            }
        }
    }
    if let Extra::Schema(additional) = extra(schema) {
        let keys: Vec<String> = map.keys().cloned().collect();
        for key in keys {
            if regex_test(&pattern, "", &key) {
                continue;
            }
            if let Some(current) = map.get(&key).cloned() {
                if let Some(next) = from_default(&additional, Some(current)) {
                    map.insert(key, next);
                }
            }
        }
    }
    Some(Value::Object(map))
}

enum Extra {
    Absent,
    Bool(bool),
    Schema(Schema),
}

fn extra(schema: &Schema) -> Extra {
    if let Some(sub) = schema.child("additionalProperties") {
        Extra::Schema(sub.clone())
    } else if let Some(Value::Bool(b)) = schema.json("additionalProperties") {
        Extra::Bool(*b)
    } else {
        Extra::Absent
    }
}

fn evaluate_type(schema: &Schema) -> Schema {
    match schema.kind() {
        Some(Kind::Enum) => super::enum_to_union(schema),
        Some(Kind::Intersect) => evaluate_intersect(schema.list("allOf")),
        Some(Kind::Union) => {
            let broadened = broaden(schema.list("anyOf"));
            union_fast(&broadened)
        }
        _ => schema.clone(),
    }
}

fn evaluate_intersect(types: &[Schema]) -> Schema {
    let distributed = distribute(types);
    let broadened = broaden(&distributed);
    union_fast(&broadened)
}

fn union_fast(types: &[Schema]) -> Schema {
    match types {
        [] => super::never_schema(),
        [one] => one.clone(),
        many => super::union_of(many.to_vec()),
    }
}

fn broaden(types: &[Schema]) -> Vec<Schema> {
    let mut out = Vec::new();
    for ty in types {
        let evaluated = evaluate_type(ty);
        if evaluated.kind() != Some(Kind::Never) {
            out.push(evaluated);
        }
    }
    out
}

fn distribute(types: &[Schema]) -> Vec<Schema> {
    fn rec(types: &[Schema], result: Vec<Schema>) -> Vec<Schema> {
        if types.is_empty() {
            return result;
        }
        let next = if types[0].kind() == Some(Kind::Union) {
            let arms = types[0].list("anyOf").to_vec();
            distribute_union(&arms, &result)
        } else {
            distribute_type(&types[0], &result)
        };
        rec(&types[1..], next)
    }
    rec(types, Vec::new())
}

fn distribute_union(types: &[Schema], distribution: &[Schema]) -> Vec<Schema> {
    let mut result = Vec::new();
    for ty in types {
        if ty.kind() == Some(Kind::Union) {
            let arms = ty.list("anyOf").to_vec();
            result.extend(distribute_union(&arms, distribution));
        } else {
            result.extend(distribute_type(ty, distribution));
        }
    }
    result
}

fn distribute_type(ty: &Schema, types: &[Schema]) -> Vec<Schema> {
    if types.is_empty() {
        return vec![ty.clone()];
    }
    types.iter().map(|left| distribute_op(left, ty)).collect()
}

fn distribute_op(left: &Schema, right: &Schema) -> Schema {
    let left = evaluate_type(left);
    let right = evaluate_type(right);
    if left.kind() == Some(Kind::Union) || right.kind() == Some(Kind::Union) {
        evaluate_intersect(&[left, right])
    } else {
        narrow(&left, &right)
    }
}

fn narrow(left: &Schema, right: &Schema) -> Schema {
    match (left.kind(), right.kind()) {
        (Some(Kind::Never), _) => left.clone(),
        (Some(Kind::Any), _) => left.clone(),
        (Some(Kind::Unknown), _) => right.clone(),
        (_, Some(Kind::Never)) => right.clone(),
        (_, Some(Kind::Any)) => right.clone(),
        (_, Some(Kind::Unknown)) => left.clone(),
        _ => {
            let left_ok = can_composite(left);
            let right_ok = can_composite(right);
            if left_ok && right_ok {
                composite(left, right)
            } else if left_ok {
                left.clone()
            } else if right_ok {
                right.clone()
            } else {
                narrow_compare(left, right)
            }
        }
    }
}

fn can_composite(schema: &Schema) -> bool {
    matches!(schema.kind(), Some(Kind::Object | Kind::Tuple))
}

fn composite(left: &Schema, right: &Schema) -> Schema {
    let left_props = properties_of(left);
    let right_props = properties_of(right);
    let mut keys: Vec<String> = left_props.iter().map(|(k, _)| k.clone()).collect();
    for (key, _) in &right_props {
        if !keys.iter().any(|k| k == key) {
            keys.push(key.clone());
        }
    }
    let mut props = Vec::new();
    for key in keys {
        let l = left_props.iter().find(|(k, _)| k == &key);
        let r = right_props.iter().find(|(k, _)| k == &key);
        let prop = match (l, r) {
            (Some((_, a)), Some((_, b))) => composite_property(a, b),
            (Some((_, a)), None) => a.clone(),
            (None, Some((_, b))) => b.clone(),
            (None, None) => super::never_schema(),
        };
        props.push((key, prop));
    }
    super::object_of(props)
}

fn composite_property(left: &Schema, right: &Schema) -> Schema {
    let optional = left.is_optional() && right.is_optional();
    let mut property = evaluate_intersect(&[left.clone(), right.clone()]);
    property.set_optional(optional);
    property
}

fn properties_of(schema: &Schema) -> Vec<(String, Schema)> {
    match schema.kind() {
        Some(Kind::Object) => schema.map("properties").to_vec(),
        Some(Kind::Tuple) => schema
            .list("items")
            .iter()
            .enumerate()
            .map(|(i, s)| (i.to_string(), s.clone()))
            .collect(),
        _ => Vec::new(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Rel {
    Equal,
    LeftInside,
    RightInside,
    Disjoint,
}

fn narrow_compare(left: &Schema, right: &Schema) -> Schema {
    match compare(left, right) {
        Rel::Equal | Rel::RightInside => right.clone(),
        Rel::LeftInside => left.clone(),
        Rel::Disjoint => super::never_schema(),
    }
}

fn compare(left: &Schema, right: &Schema) -> Rel {
    use Kind::{Boolean, Integer, Literal, Null, Number, String};
    match (left.kind(), right.kind()) {
        (Some(a), Some(b)) if a == b && matches!(a, String | Number | Integer | Boolean | Null) => Rel::Equal,
        (Some(Integer), Some(Number)) => Rel::LeftInside,
        (Some(Number), Some(Integer)) => Rel::RightInside,
        (Some(Literal), Some(Literal)) if consts_eq(left, right) => Rel::Equal,
        (Some(Literal), Some(String)) if const_is(left, Value::is_string) => Rel::LeftInside,
        (Some(String), Some(Literal)) if const_is(right, Value::is_string) => Rel::RightInside,
        (Some(Literal), Some(Number)) if const_is(left, Value::is_number) => Rel::LeftInside,
        (Some(Number), Some(Literal)) if const_is(right, Value::is_number) => Rel::RightInside,
        (Some(Literal), Some(Boolean)) if const_is(left, Value::is_boolean) => Rel::LeftInside,
        (Some(Boolean), Some(Literal)) if const_is(right, Value::is_boolean) => Rel::RightInside,
        (Some(Literal), Some(Null)) if const_is(left, Value::is_null) => Rel::LeftInside,
        (Some(Null), Some(Literal)) if const_is(right, Value::is_null) => Rel::RightInside,
        (Some(Literal), Some(Integer)) if literal_integer(left) => Rel::LeftInside,
        (Some(Integer), Some(Literal)) if literal_integer(right) => Rel::RightInside,
        _ => Rel::Disjoint,
    }
}

fn const_is(schema: &Schema, pred: fn(&Value) -> bool) -> bool {
    schema.json("const").is_some_and(pred)
}

fn consts_eq(left: &Schema, right: &Schema) -> bool {
    match (left.json("const"), right.json("const")) {
        (Some(a), Some(b)) => json_eq(a, b),
        _ => false,
    }
}

fn literal_integer(schema: &Schema) -> bool {
    schema
        .json("const")
        .and_then(Value::as_f64)
        .is_some_and(crate::num::is_integer)
}
