//! Port of packages/telemetry/src/index.ts

// @generated-mods begin (scaffold-owned, do not edit)
pub mod memory;
pub mod noop;
pub mod testing;
// @generated-mods end
pub use pi_js::{Error, Result};

use std::any::Any;
use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_js::BoxFuture;
use serde::{Deserialize, Serialize};

// PORT: untagged, in TS union order. Empty arrays match `StringArray` first. Numbers are `f64`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AttributeValue {
    String(String),
    Number(f64),
    Boolean(bool),
    StringArray(Vec<String>),
    NumberArray(Vec<f64>),
    BooleanArray(Vec<bool>),
}

pub type SpanAttributes = IndexMap<String, AttributeValue>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpanOptions {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<SpanAttributes>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpanStatusError {
    pub name: String,
    pub message: String,
}

// PORT: discriminator is `status`, not `type`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all_fields = "camelCase")]
pub enum SpanStatus {
    #[serde(rename = "ok")]
    Ok,
    #[serde(rename = "error")]
    Error {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<SpanStatusError>,
    },
}

// PORT: `startSpan` preserves any thrown value (`Error`, plain object, or `undefined`), not `pi_js::Error`.
// `T` is type-erased so `TelemetryContext` stays dyn-compatible. Callbacks are `Arc<dyn Fn + Send + Sync>`.
pub type SpanValue = Box<dyn Any + Send>;
pub type SpanResult = std::result::Result<SpanValue, SpanValue>;
pub type SpanCallback = Arc<dyn Fn(Arc<dyn TelemetrySpan + Send + Sync>) -> BoxFuture<SpanResult> + Send + Sync>;

#[async_trait]
pub trait TelemetryContext: Send + Sync {
    async fn start_span(&self, options: SpanOptions, callback: SpanCallback) -> SpanResult;
}

pub trait TelemetrySpan: TelemetryContext {
    fn add_event(&self, name: &str, attributes: Option<SpanAttributes>);
    fn set_attributes(&self, attributes: SpanAttributes);
    fn set_status(&self, status: SpanStatus);
}

#[async_trait]
impl<T> TelemetryContext for Arc<T>
where
    T: TelemetryContext + ?Sized,
{
    async fn start_span(&self, options: SpanOptions, callback: SpanCallback) -> SpanResult {
        (**self).start_span(options, callback).await
    }
}

impl<T> TelemetrySpan for Arc<T>
where
    T: TelemetrySpan + ?Sized,
{
    fn add_event(&self, name: &str, attributes: Option<SpanAttributes>) {
        (**self).add_event(name, attributes);
    }

    fn set_attributes(&self, attributes: SpanAttributes) {
        (**self).set_attributes(attributes);
    }

    fn set_status(&self, status: SpanStatus) {
        (**self).set_status(status);
    }
}

pub use crate::noop::NOOP_TELEMETRY_CONTEXT;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TelemetryAttributeType {
    #[serde(rename = "string")]
    String,
    #[serde(rename = "number")]
    Number,
    #[serde(rename = "boolean")]
    Boolean,
    #[serde(rename = "string[]")]
    StringArray,
    #[serde(rename = "number[]")]
    NumberArray,
    #[serde(rename = "boolean[]")]
    BooleanArray,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TelemetryAttributeCardinality {
    #[serde(rename = "low")]
    Low,
    #[serde(rename = "high")]
    High,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryAttributeMetadata {
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensitive: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cardinality: Option<TelemetryAttributeCardinality>,
}

// PORT: schema object literals disagree with interface order. Most common literal is
// `type`, `required` (start/event only), `values`/`elementValues`, `description`.
// `examples`, `sensitive`, and `cardinality` follow the interface after those keys.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum TelemetryAttributeDefinition {
    #[serde(rename = "string")]
    String {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        values: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        examples: Option<Vec<String>>,
        description: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sensitive: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cardinality: Option<TelemetryAttributeCardinality>,
    },
    #[serde(rename = "number")]
    Number {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        values: Option<Vec<f64>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        examples: Option<Vec<f64>>,
        description: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sensitive: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cardinality: Option<TelemetryAttributeCardinality>,
    },
    #[serde(rename = "boolean")]
    Boolean {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        values: Option<Vec<bool>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        examples: Option<Vec<bool>>,
        description: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sensitive: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cardinality: Option<TelemetryAttributeCardinality>,
    },
    #[serde(rename = "string[]")]
    StringArray {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        element_values: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        examples: Option<Vec<Vec<String>>>,
        description: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sensitive: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cardinality: Option<TelemetryAttributeCardinality>,
    },
    #[serde(rename = "number[]")]
    NumberArray {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        element_values: Option<Vec<f64>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        examples: Option<Vec<Vec<f64>>>,
        description: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sensitive: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cardinality: Option<TelemetryAttributeCardinality>,
    },
    #[serde(rename = "boolean[]")]
    BooleanArray {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        element_values: Option<Vec<bool>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        examples: Option<Vec<Vec<bool>>>,
        description: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sensitive: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cardinality: Option<TelemetryAttributeCardinality>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum TelemetryStartAttributeDefinition {
    #[serde(rename = "string")]
    String {
        required: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        values: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        examples: Option<Vec<String>>,
        description: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sensitive: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cardinality: Option<TelemetryAttributeCardinality>,
    },
    #[serde(rename = "number")]
    Number {
        required: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        values: Option<Vec<f64>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        examples: Option<Vec<f64>>,
        description: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sensitive: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cardinality: Option<TelemetryAttributeCardinality>,
    },
    #[serde(rename = "boolean")]
    Boolean {
        required: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        values: Option<Vec<bool>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        examples: Option<Vec<bool>>,
        description: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sensitive: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cardinality: Option<TelemetryAttributeCardinality>,
    },
    #[serde(rename = "string[]")]
    StringArray {
        required: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        element_values: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        examples: Option<Vec<Vec<String>>>,
        description: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sensitive: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cardinality: Option<TelemetryAttributeCardinality>,
    },
    #[serde(rename = "number[]")]
    NumberArray {
        required: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        element_values: Option<Vec<f64>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        examples: Option<Vec<Vec<f64>>>,
        description: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sensitive: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cardinality: Option<TelemetryAttributeCardinality>,
    },
    #[serde(rename = "boolean[]")]
    BooleanArray {
        required: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        element_values: Option<Vec<bool>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        examples: Option<Vec<Vec<bool>>>,
        description: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sensitive: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cardinality: Option<TelemetryAttributeCardinality>,
    },
}

// PORT: identical to `TelemetryStartAttributeDefinition` (`definition & { required: boolean }`).
pub type TelemetryEventAttributeDefinition = TelemetryStartAttributeDefinition;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryEventDefinition {
    pub description: String,
    pub attributes: IndexMap<String, TelemetryEventAttributeDefinition>,
}

// PORT: discriminator is `kind`, not `type`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum TelemetryParentDefinition {
    #[serde(rename = "any")]
    Any,
    #[serde(rename = "root_or_external")]
    RootOrExternal,
    #[serde(rename = "spans")]
    Spans { spans: Vec<String> },
}

// PORT: `status.default` is the string literal `"ok"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TelemetrySpanStatusDefault {
    #[serde(rename = "ok")]
    Ok,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetrySpanStatusDefinition {
    #[serde(rename = "default")]
    pub r#default: TelemetrySpanStatusDefault,
    pub error_when: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetrySpanDefinition {
    pub description: String,
    pub parents: TelemetryParentDefinition,
    pub start_attributes: IndexMap<String, TelemetryStartAttributeDefinition>,
    pub end_attributes: IndexMap<String, TelemetryAttributeDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<IndexMap<String, TelemetryEventDefinition>>,
    pub status: TelemetrySpanStatusDefinition,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetrySchemaDefinition {
    pub version: i64,
    pub spans: IndexMap<String, TelemetrySpanDefinition>,
}

/// Typed identity helper for serializable telemetry schema data.
pub fn define_telemetry_schema(schema: TelemetrySchemaDefinition) -> TelemetrySchemaDefinition {
    schema
}

// PORT: TS conditional types erased to concrete types (PORTING.md §3.2).
pub type InferRequiredAndOptionalAttributes = SpanAttributes;
pub type InferStartAttributes = SpanAttributes;
pub type InferOptionalAttributes = SpanAttributes;
pub type ExactTelemetryAttributes = SpanAttributes;
pub type InferEventAttributes = SpanAttributes;
pub type TelemetrySchemaSpanName = String;
pub type TelemetrySchemaSpanStartAttributes = SpanAttributes;
pub type TelemetrySchemaSpanEndAttributes = SpanAttributes;
pub type TelemetrySchemaSpanEventName = String;
pub type TelemetrySchemaSpanEventAttributes = SpanAttributes;
pub type SchemaTelemetrySpan = Arc<dyn TelemetrySpan + Send + Sync>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetrySchemaSpanUnion {
    pub name: String,
    pub start_attributes: SpanAttributes,
    pub end_attributes: SpanAttributes,
    pub events: IndexMap<String, SpanAttributes>,
}

pub type TypedSpanCallback =
    Arc<dyn Fn(Arc<dyn TelemetrySpan + Send + Sync>, TypedSpanStarter) -> BoxFuture<SpanResult> + Send + Sync>;

type TypedSpanStarterFn = dyn Fn(&str, SpanAttributes, TypedSpanCallback) -> BoxFuture<SpanResult> + Send + Sync;

/// A per-span overload set bound to one explicit parent context and one or more schemas.
#[derive(Clone)]
pub struct TypedSpanStarter {
    inner: Arc<TypedSpanStarterFn>,
}

impl TypedSpanStarter {
    // PORT: TS calls the starter as a function.
    pub async fn start(&self, name: &str, attributes: SpanAttributes, callback: TypedSpanCallback) -> SpanResult {
        (self.inner)(name, attributes, callback).await
    }
}

/// Bind an explicit parent context to the combined span vocabulary of one or more schemas.
/// Schema values are used only for type inference; no runtime schema validation is performed.
#[allow(unused_variables)]
pub fn create_typed_span_starter<C>(telemetry_context: C, schemas: &[TelemetrySchemaDefinition]) -> TypedSpanStarter
where
    C: TelemetryContext + 'static,
{
    todo!("port: create_typed_span_starter")
}

pub use crate::memory::{InMemoryTelemetryContext, RecordedTelemetryEvent, RecordedTelemetrySpan};
