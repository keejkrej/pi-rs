//! Schema builders producing plain JSON Schema values with optional property markers and TypeBox `t::*`
//! compatibility constructors.

use std::ops::Deref;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

/// Plain JSON Schema document with hidden `optional` metadata for [`obj`] / [`open_obj`] / [`t::object`].
///
/// Serializes as the bare JSON Schema `Value` (hidden flags are never emitted to JSON).
#[derive(Clone, Debug, PartialEq)]
pub struct Schema {
    value: Value,
    optional: bool,
}

impl Schema {
    /// Wraps a raw JSON Schema `Value` (`Value::Object` or `Value::Bool`).
    pub fn from_value(value: Value) -> Self {
        Self { value, optional: false }
    }

    /// Borrows the underlying JSON Schema `Value`.
    pub fn as_value(&self) -> &Value {
        &self.value
    }

    /// Clones the underlying JSON Schema `Value`.
    pub fn to_value(&self) -> Value {
        self.value.clone()
    }

    /// Consumes `self` and returns the underlying JSON Schema `Value`.
    pub fn into_value(self) -> Value {
        self.value
    }

    /// Whether this property schema is marked optional inside an object definition.
    pub fn is_optional(&self) -> bool {
        self.optional
    }

    /// Marks this schema as an optional object property (not emitted in JSON).
    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    /// Appends or replaces a JSON Schema keyword in-place on an object schema.
    pub fn with(mut self, key: impl AsRef<str>, value: impl IntoArg) -> Self {
        let key = key.as_ref();
        if let Value::Object(ref mut map) = self.value {
            map.insert(key.to_string(), value.into_json_arg());
        }
        self
    }

    /// Sets `"title"`.
    pub fn title(self, title: impl Into<String>) -> Self {
        self.with("title", title.into())
    }

    /// Sets `"description"`.
    pub fn description(self, description: impl Into<String>) -> Self {
        self.with("description", description.into())
    }

    /// Sets `"default"`.
    pub fn default_value(self, default: impl IntoArg) -> Self {
        self.with("default", default)
    }

    /// Sets `"deprecated"`.
    pub fn deprecated(self, deprecated: bool) -> Self {
        self.with("deprecated", deprecated)
    }

    /// Sets `"examples"`.
    pub fn examples(self, examples: Vec<Value>) -> Self {
        self.with("examples", Value::Array(examples))
    }

    /// Sets `"minLength"`.
    pub fn min_length(self, n: usize) -> Self {
        self.with("minLength", n)
    }

    /// Sets `"maxLength"`.
    pub fn max_length(self, n: usize) -> Self {
        self.with("maxLength", n)
    }

    /// Sets `"pattern"`.
    pub fn pattern(self, pattern: impl Into<String>) -> Self {
        self.with("pattern", pattern.into())
    }

    /// Sets `"format"`.
    pub fn format(self, format: impl Into<String>) -> Self {
        self.with("format", format.into())
    }

    /// Sets `"minimum"`.
    pub fn minimum(self, n: impl IntoArg) -> Self {
        self.with("minimum", n)
    }

    /// Sets `"maximum"`.
    pub fn maximum(self, n: impl IntoArg) -> Self {
        self.with("maximum", n)
    }

    /// Sets `"exclusiveMinimum"`.
    pub fn exclusive_minimum(self, n: impl IntoArg) -> Self {
        self.with("exclusiveMinimum", n)
    }

    /// Sets `"exclusiveMaximum"`.
    pub fn exclusive_maximum(self, n: impl IntoArg) -> Self {
        self.with("exclusiveMaximum", n)
    }

    /// Sets `"multipleOf"`.
    pub fn multiple_of(self, n: impl IntoArg) -> Self {
        self.with("multipleOf", n)
    }

    /// Sets `"minItems"`.
    pub fn min_items(self, n: usize) -> Self {
        self.with("minItems", n)
    }

    /// Sets `"maxItems"`.
    pub fn max_items(self, n: usize) -> Self {
        self.with("maxItems", n)
    }

    /// Sets `"uniqueItems"`.
    pub fn unique_items(self, unique: bool) -> Self {
        self.with("uniqueItems", unique)
    }
}

impl Deref for Schema {
    type Target = Value;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl AsRef<Value> for Schema {
    fn as_ref(&self) -> &Value {
        &self.value
    }
}

impl From<Value> for Schema {
    fn from(value: Value) -> Self {
        Self::from_value(value)
    }
}

impl From<bool> for Schema {
    fn from(value: bool) -> Self {
        Self::from_value(Value::Bool(value))
    }
}

impl From<Schema> for Value {
    fn from(schema: Schema) -> Self {
        schema.value
    }
}

impl From<pi_js::vendor::typebox::Schema> for Schema {
    fn from(value: pi_js::vendor::typebox::Schema) -> Self {
        let converted = Self::from_value(value.to_value());
        if value.is_optional() {
            converted.optional()
        } else {
            converted
        }
    }
}

impl From<Schema> for pi_js::vendor::typebox::Schema {
    fn from(value: Schema) -> Self {
        let opt = value.is_optional();
        let converted = pi_js::vendor::typebox::Schema::from(value.into_value());
        if opt { converted.optional() } else { converted }
    }
}

impl Serialize for Schema {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.value.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Schema {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Ok(Self::from_value(value))
    }
}

/// Values accepted by [`Schema::with`] and literal builders.
pub trait IntoArg {
    fn into_json_arg(self) -> Value;
}

impl IntoArg for Schema {
    fn into_json_arg(self) -> Value {
        self.value
    }
}

impl IntoArg for Value {
    fn into_json_arg(self) -> Value {
        self
    }
}

impl IntoArg for &str {
    fn into_json_arg(self) -> Value {
        Value::String(self.to_string())
    }
}

impl IntoArg for String {
    fn into_json_arg(self) -> Value {
        Value::String(self)
    }
}

impl IntoArg for bool {
    fn into_json_arg(self) -> Value {
        Value::Bool(self)
    }
}

macro_rules! impl_num_arg {
    ($($t:ty),*) => {$(
        impl IntoArg for $t {
            fn into_json_arg(self) -> Value {
                pi_js::json::number_value(self as f64)
            }
        }
    )*};
}
impl_num_arg!(i32, i64, u32, u64, usize, f64);

/// Common annotation fields accepted by every builder.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Annotations {
    pub title: Option<String>,
    pub description: Option<String>,
    pub examples: Option<Vec<Value>>,
    pub deprecated: Option<bool>,
    pub default_value: Option<Value>,
}

impl Annotations {
    fn apply(self, map: &mut Map<String, Value>) {
        if let Some(title) = self.title {
            map.insert("title".to_string(), Value::String(title));
        }
        if let Some(description) = self.description {
            map.insert("description".to_string(), Value::String(description));
        }
        if let Some(examples) = self.examples {
            map.insert("examples".to_string(), Value::Array(examples));
        }
        if let Some(deprecated) = self.deprecated {
            map.insert("deprecated".to_string(), Value::Bool(deprecated));
        }
        if let Some(default_val) = self.default_value {
            map.insert("default".to_string(), default_val);
        }
    }
}

/// Options for [`str_with`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StrOptions {
    pub annotations: Annotations,
    pub min_length: Option<usize>,
    pub max_length: Option<usize>,
    pub pattern: Option<String>,
    pub format: Option<String>,
}

/// Options for [`num_with`] and [`int_with`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NumOptions {
    pub annotations: Annotations,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
    pub exclusive_minimum: Option<f64>,
    pub exclusive_maximum: Option<f64>,
    pub multiple_of: Option<f64>,
}

/// Options for [`bigint_with`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BigintOptions {
    pub annotations: Annotations,
    pub minimum: Option<i64>,
    pub maximum: Option<i64>,
    pub exclusive_minimum: Option<i64>,
    pub exclusive_maximum: Option<i64>,
    pub multiple_of: Option<i64>,
}

/// Options for [`bytes_with`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BytesOptions {
    pub annotations: Annotations,
    pub min_length: Option<usize>,
    pub max_length: Option<usize>,
}

/// Options for [`arr_with`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ArrOptions {
    pub annotations: Annotations,
    pub min_items: Option<usize>,
    pub max_items: Option<usize>,
    pub unique_items: Option<bool>,
}

/// Options for [`obj_with`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ObjOptions {
    pub annotations: Annotations,
    pub open: bool,
}

/// Options for [`string_enum`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StringEnumOptions {
    pub description: Option<String>,
    pub default_value: Option<String>,
}

/// `{ "type": "string" }`.
pub fn str() -> Schema {
    str_with(StrOptions::default())
}

/// `{ "type": "string", ... }`.
pub fn str_with(options: StrOptions) -> Schema {
    let mut map = Map::new();
    map.insert("type".to_string(), Value::String("string".to_string()));
    if let Some(min) = options.min_length {
        map.insert("minLength".to_string(), pi_js::json::number_value(min as f64));
    }
    if let Some(max) = options.max_length {
        map.insert("maxLength".to_string(), pi_js::json::number_value(max as f64));
    }
    if let Some(pattern) = options.pattern {
        map.insert("pattern".to_string(), Value::String(pattern));
    }
    if let Some(format) = options.format {
        map.insert("format".to_string(), Value::String(format));
    }
    options.annotations.apply(&mut map);
    Schema::from_value(Value::Object(map))
}

/// `{ "type": "number" }`.
pub fn num() -> Schema {
    num_with(NumOptions::default())
}

/// `{ "type": "number", ... }`.
pub fn num_with(options: NumOptions) -> Schema {
    numeric_schema("number", options)
}

/// `{ "type": "integer" }`.
pub fn int() -> Schema {
    int_with(NumOptions::default())
}

/// `{ "type": "integer", ... }`.
pub fn int_with(options: NumOptions) -> Schema {
    numeric_schema("integer", options)
}

fn numeric_schema(type_name: &str, options: NumOptions) -> Schema {
    let mut map = Map::new();
    map.insert("type".to_string(), Value::String(type_name.to_string()));
    if let Some(min) = options.minimum {
        map.insert("minimum".to_string(), pi_js::json::number_value(min));
    }
    if let Some(max) = options.maximum {
        map.insert("maximum".to_string(), pi_js::json::number_value(max));
    }
    if let Some(xmin) = options.exclusive_minimum {
        map.insert("exclusiveMinimum".to_string(), pi_js::json::number_value(xmin));
    }
    if let Some(xmax) = options.exclusive_maximum {
        map.insert("exclusiveMaximum".to_string(), pi_js::json::number_value(xmax));
    }
    if let Some(mult) = options.multiple_of {
        map.insert("multipleOf".to_string(), pi_js::json::number_value(mult));
    }
    options.annotations.apply(&mut map);
    Schema::from_value(Value::Object(map))
}

/// `{ "type": "bigint" }`.
pub fn bigint() -> Schema {
    bigint_with(BigintOptions::default())
}

/// `{ "type": "bigint", ... }`.
pub fn bigint_with(options: BigintOptions) -> Schema {
    let mut map = Map::new();
    map.insert("type".to_string(), Value::String("bigint".to_string()));
    if let Some(min) = options.minimum {
        map.insert("minimum".to_string(), Value::from(min));
    }
    if let Some(max) = options.maximum {
        map.insert("maximum".to_string(), Value::from(max));
    }
    if let Some(xmin) = options.exclusive_minimum {
        map.insert("exclusiveMinimum".to_string(), Value::from(xmin));
    }
    if let Some(xmax) = options.exclusive_maximum {
        map.insert("exclusiveMaximum".to_string(), Value::from(xmax));
    }
    if let Some(mult) = options.multiple_of {
        map.insert("multipleOf".to_string(), Value::from(mult));
    }
    options.annotations.apply(&mut map);
    Schema::from_value(Value::Object(map))
}

/// `{ "type": "boolean" }`.
pub fn bool() -> Schema {
    bool_with(Annotations::default())
}

/// `{ "type": "boolean", ... }`.
pub fn bool_with(annotations: Annotations) -> Schema {
    let mut map = Map::new();
    map.insert("type".to_string(), Value::String("boolean".to_string()));
    annotations.apply(&mut map);
    Schema::from_value(Value::Object(map))
}

/// `{ "type": "null" }`.
pub fn nul() -> Schema {
    nul_with(Annotations::default())
}

/// `{ "type": "null", ... }`.
pub fn nul_with(annotations: Annotations) -> Schema {
    let mut map = Map::new();
    map.insert("type".to_string(), Value::String("null".to_string()));
    annotations.apply(&mut map);
    Schema::from_value(Value::Object(map))
}

/// `{}` (matches any value).
pub fn unknown() -> Schema {
    unknown_with(Annotations::default())
}

/// `{ ...annotations }`.
pub fn unknown_with(annotations: Annotations) -> Schema {
    let mut map = Map::new();
    annotations.apply(&mut map);
    Schema::from_value(Value::Object(map))
}

/// `{ "const": value }`.
pub fn lit(value: impl IntoArg) -> Schema {
    lit_with(value, Annotations::default())
}

/// `{ "const": value, ... }`.
pub fn lit_with(value: impl IntoArg, annotations: Annotations) -> Schema {
    let mut map = Map::new();
    map.insert("const".to_string(), value.into_json_arg());
    annotations.apply(&mut map);
    Schema::from_value(Value::Object(map))
}

/// `{ "enum": [...] }`.
pub fn one_of<I, V>(values: I) -> Schema
where
    I: IntoIterator<Item = V>,
    V: IntoArg,
{
    one_of_with(values, Annotations::default())
}

/// `{ "enum": [...], ... }`.
pub fn one_of_with<I, V>(values: I, annotations: Annotations) -> Schema
where
    I: IntoIterator<Item = V>,
    V: IntoArg,
{
    let arr: Vec<Value> = values.into_iter().map(IntoArg::into_json_arg).collect();
    let mut map = Map::new();
    map.insert("enum".to_string(), Value::Array(arr));
    annotations.apply(&mut map);
    Schema::from_value(Value::Object(map))
}

/// `{ "anyOf": [...] }`.
pub fn union<I>(schemas: I) -> Schema
where
    I: IntoIterator<Item = Schema>,
{
    union_with(schemas, Annotations::default())
}

/// `{ "anyOf": [...], ... }`.
pub fn union_with<I>(schemas: I, annotations: Annotations) -> Schema
where
    I: IntoIterator<Item = Schema>,
{
    let arr: Vec<Value> = schemas.into_iter().map(Schema::into_value).collect();
    let mut map = Map::new();
    map.insert("anyOf".to_string(), Value::Array(arr));
    annotations.apply(&mut map);
    Schema::from_value(Value::Object(map))
}

/// `{ "anyOf": [schema, { "type": "null" }] }`.
pub fn nullable(schema: Schema) -> Schema {
    nullable_with(schema, Annotations::default())
}

/// `{ "anyOf": [schema, { "type": "null" }], ... }`.
pub fn nullable_with(schema: Schema, annotations: Annotations) -> Schema {
    union_with([schema, nul()], annotations)
}

/// `{ "type": "array", "items": items }`.
pub fn arr(items: Schema) -> Schema {
    arr_with(items, ArrOptions::default())
}

/// `{ "type": "array", "items": items, ... }`.
pub fn arr_with(items: Schema, options: ArrOptions) -> Schema {
    let mut map = Map::new();
    map.insert("type".to_string(), Value::String("array".to_string()));
    map.insert("items".to_string(), items.into_value());
    if let Some(min) = options.min_items {
        map.insert("minItems".to_string(), pi_js::json::number_value(min as f64));
    }
    if let Some(max) = options.max_items {
        map.insert("maxItems".to_string(), pi_js::json::number_value(max as f64));
    }
    if let Some(unique) = options.unique_items {
        map.insert("uniqueItems".to_string(), Value::Bool(unique));
    }
    options.annotations.apply(&mut map);
    Schema::from_value(Value::Object(map))
}

/// `{ "type": "array", "prefixItems": [...], "items": false, "minItems": n }`.
pub fn tuple<I>(items: I) -> Schema
where
    I: IntoIterator<Item = Schema>,
{
    tuple_with(items, Annotations::default())
}

/// `{ "type": "array", "prefixItems": [...], "items": false, "minItems": n, ... }`.
pub fn tuple_with<I>(items: I, annotations: Annotations) -> Schema
where
    I: IntoIterator<Item = Schema>,
{
    let list: Vec<Value> = items.into_iter().map(Schema::into_value).collect();
    let len = list.len();
    let mut map = Map::new();
    map.insert("type".to_string(), Value::String("array".to_string()));
    map.insert("prefixItems".to_string(), Value::Array(list));
    map.insert("items".to_string(), Value::Bool(false));
    map.insert("minItems".to_string(), pi_js::json::number_value(len as f64));
    annotations.apply(&mut map);
    Schema::from_value(Value::Object(map))
}

/// `{ "type": "object", "additionalProperties": value }`.
pub fn map(value: Schema) -> Schema {
    map_with(value, Annotations::default())
}

/// `{ "type": "object", "additionalProperties": value, ... }`.
pub fn map_with(value: Schema, annotations: Annotations) -> Schema {
    let mut map_obj = Map::new();
    map_obj.insert("type".to_string(), Value::String("object".to_string()));
    map_obj.insert("additionalProperties".to_string(), value.into_value());
    annotations.apply(&mut map_obj);
    Schema::from_value(Value::Object(map_obj))
}

/// Wraps an arbitrary JSON Schema `Value` or [`Schema`] without altering its JSON structure.
pub fn unsafe_schema(schema: impl Into<Schema>) -> Schema {
    schema.into()
}

/// Builds a recursive schema identified by `$id: id`, passing `{ "$ref": id }` into `build`.
pub fn recursive<F>(id: impl Into<String>, build: F) -> Schema
where
    F: FnOnce(Schema) -> Schema,
{
    let id = id.into();
    let mut ref_map = Map::new();
    ref_map.insert("$ref".to_string(), Value::String(id.clone()));
    let self_ref = Schema::from_value(Value::Object(ref_map));
    let mut built = build(self_ref).into_value();
    if let Value::Object(ref mut m) = built {
        m.insert("$id".to_string(), Value::String(id));
    }
    Schema::from_value(built)
}

/// `{ "type": "bytes" }`.
pub fn bytes() -> Schema {
    bytes_with(BytesOptions::default())
}

/// `{ "type": "bytes", ... }`.
pub fn bytes_with(options: BytesOptions) -> Schema {
    let mut map = Map::new();
    map.insert("type".to_string(), Value::String("bytes".to_string()));
    if let Some(min) = options.min_length {
        map.insert("minLength".to_string(), pi_js::json::number_value(min as f64));
    }
    if let Some(max) = options.max_length {
        map.insert("maxLength".to_string(), pi_js::json::number_value(max as f64));
    }
    options.annotations.apply(&mut map);
    Schema::from_value(Value::Object(map))
}

/// Marks a property schema as optional for [`obj`] / [`open_obj`] / [`t::object`].
pub fn opt(schema: Schema) -> Schema {
    schema.optional()
}

/// Closed object schema (`additionalProperties: false`, required unless [`opt`]).
pub fn obj<I, K>(properties: I) -> Schema
where
    I: IntoIterator<Item = (K, Schema)>,
    K: Into<String>,
{
    obj_with(properties, ObjOptions::default())
}

/// Object schema with options (`open: true` omits `additionalProperties: false`).
pub fn obj_with<I, K>(properties: I, options: ObjOptions) -> Schema
where
    I: IntoIterator<Item = (K, Schema)>,
    K: Into<String>,
{
    let mut props = Map::new();
    let mut required = Vec::new();
    for (name, prop) in properties {
        let name = name.into();
        if !prop.is_optional() {
            required.push(Value::String(name.clone()));
        }
        props.insert(name, prop.into_value());
    }
    let mut map = Map::new();
    map.insert("type".to_string(), Value::String("object".to_string()));
    map.insert("properties".to_string(), Value::Object(props));
    if !required.is_empty() {
        map.insert("required".to_string(), Value::Array(required));
    }
    if !options.open {
        map.insert("additionalProperties".to_string(), Value::Bool(false));
    }
    options.annotations.apply(&mut map);
    Schema::from_value(Value::Object(map))
}

/// Open object schema (`{ type: "object", properties, required }`, no `additionalProperties: false`).
pub fn open_obj<I, K>(properties: I) -> Schema
where
    I: IntoIterator<Item = (K, Schema)>,
    K: Into<String>,
{
    open_obj_with(properties, Annotations::default())
}

/// Open object schema with annotations.
pub fn open_obj_with<I, K>(properties: I, annotations: Annotations) -> Schema
where
    I: IntoIterator<Item = (K, Schema)>,
    K: Into<String>,
{
    obj_with(
        properties,
        ObjOptions {
            annotations,
            open: true,
        },
    )
}

/// String enum schema (`{ type: "string", enum: [...] }`) compatible with Google/Gemini and other providers.
pub fn string_enum<I, S>(values: I, options: Option<StringEnumOptions>) -> Schema
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let enum_values: Vec<Value> = values
        .into_iter()
        .map(|v| Value::String(v.as_ref().to_string()))
        .collect();
    let mut obj = Map::new();
    obj.insert("type".to_string(), Value::String("string".to_string()));
    obj.insert("enum".to_string(), Value::Array(enum_values));
    if let Some(opts) = options {
        if let Some(desc) = opts.description
            && !desc.is_empty()
        {
            obj.insert("description".to_string(), Value::String(desc));
        }
        if let Some(def) = opts.default_value
            && !def.is_empty()
        {
            obj.insert("default".to_string(), Value::String(def));
        }
    }
    Schema::from_value(Value::Object(obj))
}

/// TypeBox-compatible `t::*` builders for callers migrating from `pi_js::vendor::typebox::t`.
pub mod t {
    use serde_json::{Map, Value};

    use super::{IntoArg, Schema, open_obj};

    pub fn object<I, K>(props: I) -> Schema
    where
        I: IntoIterator<Item = (K, Schema)>,
        K: Into<String>,
    {
        open_obj(props)
    }

    pub fn string() -> Schema {
        super::str()
    }

    pub fn number() -> Schema {
        super::num()
    }

    pub fn integer() -> Schema {
        super::int()
    }

    pub fn boolean() -> Schema {
        super::bool()
    }

    pub fn null() -> Schema {
        super::nul()
    }

    pub fn any() -> Schema {
        super::unknown()
    }

    pub fn unknown() -> Schema {
        super::unknown()
    }

    pub fn literal(value: impl IntoArg) -> Schema {
        let val = value.into_json_arg();
        let type_name = match &val {
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Null => "null",
            _ => "string",
        };
        let mut map = Map::new();
        map.insert("type".to_string(), Value::String(type_name.to_string()));
        map.insert("const".to_string(), val);
        Schema::from_value(Value::Object(map))
    }

    pub fn array(items: Schema) -> Schema {
        super::arr(items)
    }

    pub fn tuple<I>(items: I) -> Schema
    where
        I: IntoIterator<Item = Schema>,
    {
        let items: Vec<Value> = items.into_iter().map(Schema::into_value).collect();
        let n = items.len();
        let mut map = Map::new();
        map.insert("type".to_string(), Value::String("array".to_string()));
        map.insert("additionalItems".to_string(), Value::Bool(false));
        map.insert("items".to_string(), Value::Array(items));
        map.insert("minItems".to_string(), pi_js::json::number_value(n as f64));
        Schema::from_value(Value::Object(map))
    }

    pub fn union<I>(items: I) -> Schema
    where
        I: IntoIterator<Item = Schema>,
    {
        super::union(items)
    }

    pub fn intersect<I>(items: I) -> Schema
    where
        I: IntoIterator<Item = Schema>,
    {
        let arr: Vec<Value> = items.into_iter().map(Schema::into_value).collect();
        let mut map = Map::new();
        map.insert("allOf".to_string(), Value::Array(arr));
        Schema::from_value(Value::Object(map))
    }

    pub fn record(key: Schema, value: Schema) -> Schema {
        let key_type = key.as_value().get("type").and_then(Value::as_str);
        let pattern = match key_type {
            Some("integer") => Some("^-?(?:0|[1-9][0-9]*)$"),
            Some("number") => Some("^-?(?:0|[1-9][0-9]*)(?:\\.[0-9]+)?$"),
            Some("string") => Some(key.as_value().get("pattern").and_then(Value::as_str).unwrap_or("^.*$")),
            _ if key.as_value().as_object().is_some_and(Map::is_empty) => Some("^.*$"),
            _ => None,
        };
        if let Some(pat) = pattern {
            let mut pats = Map::new();
            pats.insert(pat.to_string(), value.into_value());
            let mut map = Map::new();
            map.insert("type".to_string(), Value::String("object".to_string()));
            map.insert("patternProperties".to_string(), Value::Object(pats));
            Schema::from_value(Value::Object(map))
        } else {
            open_obj(std::iter::empty::<(&str, Schema)>())
        }
    }

    pub fn enum_<I, V>(values: I) -> Schema
    where
        I: IntoIterator<Item = V>,
        V: IntoArg,
    {
        super::one_of(values)
    }

    pub fn unsafe_(schema: impl Into<Schema>) -> Schema {
        schema.into()
    }
}
