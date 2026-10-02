//! TypeBox 1.3.27 subset used by pi (`Type.*`, `Value.Check` / `Errors` / `Convert` / `Clean` /
//! `Default`, interpretive `Compile`).
//!
//! `Schema` serializes as the bare JSON Schema. Hidden TypeBox state (`~kind`, `~optional`) is
//! not part of the JSON. `minLength` / `maxLength` count UTF-16 code units.

mod check;
mod format;
mod mutate;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::json::number_value;

/// JSON Schema document plus the hidden TypeBox flags builders need.
#[derive(Clone, Debug)]
pub struct Schema {
    fields: Vec<(String, Slot)>,
    kind: Option<Kind>,
    optional: bool,
    typebox: bool,
    /// Boolean schemas (`true` / `false`) serialize as a JSON boolean.
    boolean: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    String,
    Number,
    Integer,
    Boolean,
    Null,
    Any,
    Unknown,
    Object,
    Array,
    Tuple,
    Union,
    Intersect,
    Record,
    Literal,
    Enum,
    Never,
}

#[derive(Clone, Debug)]
enum Slot {
    Value(Value),
    Schema(Box<Schema>),
    List(Vec<Schema>),
    Map(Vec<(String, Schema)>),
}

/// One validation failure. `message` is the TypeBox `en_US` string. `params` keeps the
/// keyword payload (`requiredProperties`, `limit`, …) in TypeBox key order.
#[derive(Clone, Debug, PartialEq)]
pub struct ValueError {
    pub instance_path: String,
    pub schema_path: String,
    pub keyword: String,
    pub message: String,
    pub params: Value,
}

/// Compiled checker. `check` / `errors` match `Value.Check` / `Value.Errors` (the interpretive
/// engine, not the code emitter).
#[derive(Clone, Debug)]
pub struct Validator {
    schema: Schema,
}

impl Validator {
    pub fn check(&self, value: &Value) -> bool {
        value::check(&self.schema, value)
    }

    pub fn errors(&self, value: &Value) -> Vec<ValueError> {
        value::errors(&self.schema, value)
    }
}

/// `Compile(schema)`.
pub fn compile(schema: &Schema) -> Validator {
    Validator { schema: schema.clone() }
}

/// `Value.Check` / `Errors` / `Convert` / `Clean` / `Default`.
///
/// `convert`, `clean`, and `default` take ownership and return the mutated value, which is what
/// TypeBox returns after writing through the object it was given.
pub mod value {
    use super::{Schema, ValueError};
    use serde_json::Value;

    pub fn check(schema: &Schema, value: &Value) -> bool {
        super::check::check(schema, value)
    }

    pub fn errors(schema: &Schema, value: &Value) -> Vec<ValueError> {
        super::check::errors(schema, value)
    }

    pub fn convert(schema: &Schema, value: Value) -> Value {
        super::mutate::convert(schema, value)
    }

    pub fn clean(schema: &Schema, value: Value) -> Value {
        super::mutate::clean(schema, value)
    }

    pub fn default(schema: &Schema, value: Value) -> Value {
        super::mutate::default(schema, value)
    }
}

impl Schema {
    pub(crate) fn bare() -> Schema {
        Schema {
            fields: Vec::new(),
            kind: None,
            optional: false,
            typebox: false,
            boolean: None,
        }
    }

    fn fresh(kind: Kind) -> Schema {
        let mut s = Schema::bare();
        s.kind = Some(kind);
        s.typebox = true;
        s
    }

    pub fn is_typebox(&self) -> bool {
        self.typebox
    }

    pub fn is_optional(&self) -> bool {
        self.optional
    }

    pub(crate) fn set_optional(&mut self, yes: bool) {
        self.optional = yes;
    }

    /// Hidden `~optional`. Not a JSON Schema keyword.
    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    /// Append a keyword, or replace it in place when the key is already present.
    /// Schema-valued keywords keep the nested type (so `Convert` still sees `~kind`).
    pub fn with(mut self, key: impl AsRef<str>, value: impl IntoArg) -> Self {
        let key = key.as_ref();
        self.set_slot(key, slot_from_arg(key, value.into_arg()));
        self
    }

    pub fn to_value(&self) -> Value {
        if let Some(b) = self.boolean {
            return Value::Bool(b);
        }
        let mut map = Map::new();
        for (key, slot) in &self.fields {
            map.insert(key.clone(), slot.to_value());
        }
        Value::Object(map)
    }

    fn set_slot(&mut self, key: impl Into<String>, slot: Slot) {
        let key = key.into();
        if let Some(i) = self.fields.iter().position(|(k, _)| k == &key) {
            self.fields[i].1 = slot;
        } else {
            self.fields.push((key, slot));
        }
    }

    pub(crate) fn kind(&self) -> Option<Kind> {
        self.kind
    }

    pub(crate) fn has_key(&self, key: &str) -> bool {
        self.fields.iter().any(|(k, _)| k == key)
    }

    pub(crate) fn json(&self, key: &str) -> Option<&Value> {
        match self.slot(key) {
            Some(Slot::Value(v)) => Some(v),
            _ => None,
        }
    }

    pub(crate) fn child(&self, key: &str) -> Option<&Schema> {
        match self.slot(key) {
            Some(Slot::Schema(s)) => Some(s),
            _ => None,
        }
    }

    pub(crate) fn list(&self, key: &str) -> &[Schema] {
        match self.slot(key) {
            Some(Slot::List(xs)) => xs,
            _ => &[],
        }
    }

    pub(crate) fn map(&self, key: &str) -> &[(String, Schema)] {
        match self.slot(key) {
            Some(Slot::Map(xs)) => xs,
            _ => &[],
        }
    }

    fn slot(&self, key: &str) -> Option<&Slot> {
        self.fields.iter().find(|(k, _)| k == key).map(|(_, s)| s)
    }
}

impl Slot {
    fn to_value(&self) -> Value {
        match self {
            Slot::Value(v) => v.clone(),
            Slot::Schema(s) => s.to_value(),
            Slot::List(xs) => Value::Array(xs.iter().map(Schema::to_value).collect()),
            Slot::Map(xs) => {
                let mut map = Map::new();
                for (k, s) in xs {
                    map.insert(k.clone(), s.to_value());
                }
                Value::Object(map)
            }
        }
    }
}

impl Serialize for Schema {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Schema {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Ok(Schema::from(value))
    }
}

impl From<Value> for Schema {
    fn from(value: Value) -> Self {
        match value {
            Value::Bool(b) => {
                let mut s = Schema::bare();
                s.boolean = Some(b);
                s
            }
            Value::Object(map) => {
                let mut s = Schema::bare();
                for (k, v) in map {
                    s.fields.push((k, Slot::Value(v)));
                }
                s
            }
            _ => Schema::bare(),
        }
    }
}

fn json_str(s: &str) -> Slot {
    Slot::Value(Value::String(s.to_string()))
}

fn json_num(n: f64) -> Slot {
    Slot::Value(number_value(n))
}

fn is_schema_key(key: &str) -> bool {
    matches!(
        key,
        "additionalProperties"
            | "additionalItems"
            | "unevaluatedProperties"
            | "unevaluatedItems"
            | "not"
            | "if"
            | "then"
            | "else"
            | "contains"
            | "propertyNames"
            | "items"
    )
}

fn is_list_key(key: &str) -> bool {
    matches!(key, "anyOf" | "allOf" | "oneOf" | "prefixItems" | "items")
}

fn is_map_key(key: &str) -> bool {
    matches!(
        key,
        "properties" | "patternProperties" | "$defs" | "definitions" | "dependentSchemas"
    )
}

fn slot_from_arg(key: &str, arg: Arg) -> Slot {
    match arg {
        Arg::Schema(s) if is_schema_key(key) => Slot::Schema(Box::new(s)),
        Arg::Schema(s) => Slot::Value(s.to_value()),
        Arg::Json(Value::Array(items)) if is_list_key(key) => Slot::List(items.into_iter().map(Schema::from).collect()),
        Arg::Json(Value::Object(map)) if is_schema_key(key) => Slot::Schema(Box::new(Schema::from(Value::Object(map)))),
        Arg::Json(Value::Object(map)) if is_map_key(key) => {
            Slot::Map(map.into_iter().map(|(k, v)| (k, Schema::from(v))).collect())
        }
        Arg::Json(v) => Slot::Value(v),
    }
}

pub(crate) fn object_of(props: Vec<(String, Schema)>) -> Schema {
    let required: Vec<Value> = props
        .iter()
        .filter(|(_, s)| !s.optional)
        .map(|(k, _)| Value::String(k.clone()))
        .collect();
    let mut s = Schema::fresh(Kind::Object);
    s.set_slot("type", json_str("object"));
    if !required.is_empty() {
        s.set_slot("required", Slot::Value(Value::Array(required)));
    }
    s.set_slot("properties", Slot::Map(props));
    s
}

pub(crate) fn union_of(items: Vec<Schema>) -> Schema {
    let mut s = Schema::fresh(Kind::Union);
    s.set_slot("anyOf", Slot::List(items));
    s
}

pub(crate) fn never_schema() -> Schema {
    Schema::fresh(Kind::Never)
}

pub(crate) fn literal_from_value(value: Value) -> Schema {
    let type_name = match &value {
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Null => "null",
        _ => "string",
    };
    let mut s = Schema::fresh(Kind::Literal);
    s.set_slot("type", json_str(type_name));
    s.set_slot("const", Slot::Value(value));
    s
}

pub(crate) fn enum_to_union(schema: &Schema) -> Schema {
    let values = schema
        .json("enum")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let lits: Vec<Schema> = values.into_iter().map(literal_from_value).collect();
    match lits.len() {
        0 => never_schema(),
        1 => lits.into_iter().next().unwrap(),
        _ => union_of(lits),
    }
}

fn create_record(pattern: String, value: Schema) -> Schema {
    let mut s = Schema::fresh(Kind::Record);
    s.set_slot("type", json_str("object"));
    s.set_slot("patternProperties", Slot::Map(vec![(pattern, value)]));
    s
}

fn record_from_key(key: &Schema, value: Schema) -> Schema {
    match key.kind {
        Some(Kind::Any) => create_record("^.*$".to_string(), value),
        Some(Kind::Boolean) => object_of(vec![("true".to_string(), value.clone()), ("false".to_string(), value)]),
        Some(Kind::Integer) => create_record("^-?(?:0|[1-9][0-9]*)$".to_string(), value),
        Some(Kind::Number) => create_record("^-?(?:0|[1-9][0-9]*)(?:\\.[0-9]+)?$".to_string(), value),
        Some(Kind::Literal) => literal_key_record(key, value),
        Some(Kind::String) => {
            if let Some(Value::String(pattern)) = key.json("pattern") {
                create_record(pattern.clone(), value)
            } else {
                create_record("^.*$".to_string(), value)
            }
        }
        Some(Kind::Enum) => record_from_key(&enum_to_union(key), value),
        Some(Kind::Union) => union_key_record(key, value),
        _ => object_of(Vec::new()),
    }
}

fn literal_key_record(key: &Schema, value: Schema) -> Schema {
    let Some(c) = key.json("const") else {
        return object_of(Vec::new());
    };
    if c.is_string() || c.is_number() {
        let name = match c {
            Value::String(s) => s.clone(),
            Value::Number(n) => crate::json::number_to_string(n.as_f64().unwrap_or(0.0)),
            _ => return object_of(Vec::new()),
        };
        object_of(vec![(name, value)])
    } else if c.as_bool() == Some(true) {
        object_of(vec![("true".to_string(), value)])
    } else if c.as_bool() == Some(false) {
        object_of(vec![("false".to_string(), value)])
    } else {
        object_of(Vec::new())
    }
}

fn flatten_union(schema: &Schema, out: &mut Vec<Schema>) {
    if schema.kind == Some(Kind::Union) {
        for arm in schema.list("anyOf") {
            flatten_union(arm, out);
        }
    } else {
        out.push(schema.clone());
    }
}

fn union_key_record(key: &Schema, value: Schema) -> Schema {
    let mut flat = Vec::new();
    flatten_union(key, &mut flat);
    let broad = flat
        .iter()
        .any(|t| matches!(t.kind, Some(Kind::String | Kind::Number | Kind::Integer)));
    if broad {
        return create_record("^.*$".to_string(), value);
    }
    let mut props = Vec::new();
    for arm in flat {
        if arm.kind != Some(Kind::Literal) {
            continue;
        }
        let Some(c) = arm.json("const") else { continue };
        if !(c.is_string() || c.is_number()) {
            continue;
        }
        let name = match c {
            Value::String(s) => s.clone(),
            Value::Number(n) => crate::json::number_to_string(n.as_f64().unwrap_or(0.0)),
            _ => continue,
        };
        props.push((name, value.clone()));
    }
    object_of(props)
}

/// Type builders. Names match `Type.*` except `enum` → `enum_` and `Unsafe` → `unsafe_`.
pub mod t {
    use super::{
        IntoJson, IntoLiteral, Kind, Schema, Slot, json_str, literal_from_value, object_of, record_from_key, union_of,
    };

    pub fn object<I, K>(props: I) -> Schema
    where
        I: IntoIterator<Item = (K, Schema)>,
        K: Into<String>,
    {
        object_of(props.into_iter().map(|(k, s)| (k.into(), s)).collect())
    }

    pub fn string() -> Schema {
        let mut s = Schema::fresh(Kind::String);
        s.set_slot("type", json_str("string"));
        s
    }

    pub fn number() -> Schema {
        let mut s = Schema::fresh(Kind::Number);
        s.set_slot("type", json_str("number"));
        s
    }

    pub fn integer() -> Schema {
        let mut s = Schema::fresh(Kind::Integer);
        s.set_slot("type", json_str("integer"));
        s
    }

    pub fn boolean() -> Schema {
        let mut s = Schema::fresh(Kind::Boolean);
        s.set_slot("type", json_str("boolean"));
        s
    }

    pub fn null() -> Schema {
        let mut s = Schema::fresh(Kind::Null);
        s.set_slot("type", json_str("null"));
        s
    }

    pub fn any() -> Schema {
        Schema::fresh(Kind::Any)
    }

    pub fn unknown() -> Schema {
        Schema::fresh(Kind::Unknown)
    }

    pub fn literal(value: impl IntoLiteral) -> Schema {
        literal_from_value(value.into_literal())
    }

    pub fn array(items: Schema) -> Schema {
        let mut s = Schema::fresh(Kind::Array);
        s.set_slot("type", json_str("array"));
        s.set_slot("items", Slot::Schema(Box::new(items)));
        s
    }

    pub fn tuple<I>(items: I) -> Schema
    where
        I: IntoIterator<Item = Schema>,
    {
        let items: Vec<Schema> = items.into_iter().collect();
        let n = items.len();
        let mut s = Schema::fresh(Kind::Tuple);
        s.set_slot("type", json_str("array"));
        s.set_slot("additionalItems", Slot::Value(serde_json::Value::Bool(false)));
        s.set_slot("items", Slot::List(items));
        s.set_slot("minItems", super::json_num(n as f64));
        s
    }

    pub fn union<I>(items: I) -> Schema
    where
        I: IntoIterator<Item = Schema>,
    {
        union_of(items.into_iter().collect())
    }

    pub fn intersect<I>(items: I) -> Schema
    where
        I: IntoIterator<Item = Schema>,
    {
        let mut s = Schema::fresh(Kind::Intersect);
        s.set_slot("allOf", Slot::List(items.into_iter().collect()));
        s
    }

    pub fn record(key: Schema, value: Schema) -> Schema {
        record_from_key(&key, value)
    }

    pub fn enum_<I, V>(values: I) -> Schema
    where
        I: IntoIterator<Item = V>,
        V: IntoJson,
    {
        let arr = values.into_iter().map(|v| v.into_json()).collect();
        let mut s = Schema::fresh(Kind::Enum);
        s.set_slot("enum", Slot::Value(serde_json::Value::Array(arr)));
        s
    }

    /// `Type.Unsafe`. Keeps an existing `~kind` when `schema` is already a builder value.
    pub fn unsafe_(schema: impl Into<Schema>) -> Schema {
        let mut s = schema.into();
        s.typebox = true;
        s
    }
}

/// Value accepted by [`Schema::with`].
pub enum Arg {
    Json(Value),
    Schema(Schema),
}

pub trait IntoArg {
    fn into_arg(self) -> Arg;
}

impl IntoArg for Schema {
    fn into_arg(self) -> Arg {
        Arg::Schema(self)
    }
}

pub trait IntoJson {
    fn into_json(self) -> Value;
}

impl<T: IntoJson> IntoArg for T {
    fn into_arg(self) -> Arg {
        Arg::Json(self.into_json())
    }
}

impl IntoJson for Value {
    fn into_json(self) -> Value {
        self
    }
}
impl IntoJson for &str {
    fn into_json(self) -> Value {
        Value::String(self.to_string())
    }
}
impl IntoJson for String {
    fn into_json(self) -> Value {
        Value::String(self)
    }
}
impl IntoJson for bool {
    fn into_json(self) -> Value {
        Value::Bool(self)
    }
}

macro_rules! json_numbers {
    ($($t:ty),*) => {$(
        impl IntoJson for $t {
            fn into_json(self) -> Value {
                number_value(self as f64)
            }
        }
    )*};
}
json_numbers!(i32, i64, u32, u64, usize, f64);

pub trait IntoLiteral {
    fn into_literal(self) -> Value;
}

impl IntoLiteral for &str {
    fn into_literal(self) -> Value {
        Value::String(self.to_string())
    }
}
impl IntoLiteral for String {
    fn into_literal(self) -> Value {
        Value::String(self)
    }
}
impl IntoLiteral for bool {
    fn into_literal(self) -> Value {
        Value::Bool(self)
    }
}
macro_rules! lit_numbers {
    ($($t:ty),*) => {$(
        impl IntoLiteral for $t {
            fn into_literal(self) -> Value {
                number_value(self as f64)
            }
        }
    )*};
}
lit_numbers!(i32, i64, u32, u64, usize, f64);

#[cfg(test)]
mod tests;
