//! Port of packages/telemetry/src/index.ts

// @generated-mods begin (scaffold-owned, do not edit)
pub mod memory;
pub mod metrics;
pub mod noop;
pub mod render;
pub mod testing;
// @generated-mods end
pub use pi_js::{Error, Result};

use std::any::Any;
use std::fmt;
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

/// Opaque, JSON-serializable adapter-owned propagation data.
pub type TelemetryCarrier = IndexMap<String, String>;

/// Optional transport capability implemented by the application's telemetry adapter.
pub trait TelemetryPropagation: Send + Sync {
    fn inject(&self, context: &dyn TelemetryContext) -> Option<TelemetryCarrier>;
    /// Absent or unknown carriers use the root; recognized identifiers are opaque strings.
    fn extract(&self, carrier: Option<&TelemetryCarrier>) -> Arc<dyn TelemetryContext + Send + Sync>;
}

impl<T> TelemetryPropagation for Arc<T>
where
    T: TelemetryPropagation + ?Sized,
{
    fn inject(&self, context: &dyn TelemetryContext) -> Option<TelemetryCarrier> {
        (**self).inject(context)
    }

    fn extract(&self, carrier: Option<&TelemetryCarrier>) -> Arc<dyn TelemetryContext + Send + Sync> {
        (**self).extract(carrier)
    }
}

#[derive(Clone)]
pub struct SpanLink {
    pub context: Arc<dyn TelemetryContext + Send + Sync>,
    pub attributes: Option<SpanAttributes>,
}

impl SpanLink {
    pub fn new(context: Arc<dyn TelemetryContext + Send + Sync>) -> Self {
        Self {
            context,
            attributes: None,
        }
    }
}

impl fmt::Debug for SpanLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpanLink")
            .field("attributes", &self.attributes)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Default)]
pub struct SpanOptions {
    pub name: String,
    pub attributes: Option<SpanAttributes>,
    pub links: Option<Vec<SpanLink>>,
}

impl SpanOptions {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            attributes: None,
            links: None,
        }
    }
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredSpanEvent {
    pub name: String,
    pub time_ms: f64,
    pub attributes: SpanAttributes,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredSpanLink {
    pub trace_id: String,
    pub span_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<SpanAttributes>,
}

/// A settled span snapshot, independent of the recording adapter. Times are milliseconds at microsecond resolution;
/// event times are relative to span start.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredSpan {
    pub id: String,
    pub trace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub name: String,
    /// Wall-clock milliseconds since the epoch (`telemetry_now`).
    pub started_ms: f64,
    pub ended_ms: f64,
    pub attributes: SpanAttributes,
    pub events: Vec<StoredSpanEvent>,
    pub links: Vec<StoredSpanLink>,
    pub status: SpanStatus,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredTrace {
    pub trace_id: String,
    pub spans: Vec<StoredSpan>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceQuery {
    /// Name of the earliest stored span, with span ID breaking timestamp ties.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// All equalities must match one span; that span may be anywhere in the trace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<SpanAttributes>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errors: Option<bool>,
    /// Inclusive bounds on the earliest stored span's start time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_max: Option<f64>,
    /// Positive page size; defaults to 20.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Opaque continuation from the previous page; retain the same filters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindTracesResult {
    pub traces: Vec<StoredTrace>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricQuery {
    pub name: String,
    /// Inclusive bounds on interval start time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_min: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_max: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<SpanAttributes>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MetricKind {
    #[serde(rename = "counter")]
    Counter,
    #[serde(rename = "upDownCounter")]
    UpDownCounter,
    #[serde(rename = "histogram")]
    Histogram,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredMetric {
    pub interval_ms: i64,
    pub name: String,
    pub kind: MetricKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    pub attributes: SpanAttributes,
    pub count: i64,
    pub sum: f64,
    pub min: f64,
    pub max: f64,
}

/// Read settled traces without depending on a storage backend.
#[async_trait]
pub trait TelemetryStore: Send + Sync {
    async fn find_traces(&self, query: Option<TraceQuery>) -> Result<FindTracesResult>;
    async fn get_trace(&self, trace_id: &str) -> Result<Option<StoredTrace>>;
    async fn find_metrics(&self, query: MetricQuery) -> Result<Vec<StoredMetric>>;
}

#[async_trait]
impl<T> TelemetryStore for Arc<T>
where
    T: TelemetryStore + ?Sized,
{
    async fn find_traces(&self, query: Option<TraceQuery>) -> Result<FindTracesResult> {
        (**self).find_traces(query).await
    }

    async fn get_trace(&self, trace_id: &str) -> Result<Option<StoredTrace>> {
        (**self).get_trace(trace_id).await
    }

    async fn find_metrics(&self, query: MetricQuery) -> Result<Vec<StoredMetric>> {
        (**self).find_metrics(query).await
    }
}

// PORT: `startSpan` preserves any thrown value (`Error`, plain object, or `undefined`), not `pi_js::Error`.
// `T` is type-erased so `TelemetryContext` stays dyn-compatible. Callbacks are `Arc<dyn Fn + Send + Sync>`.
pub type SpanValue = Box<dyn Any + Send>;
pub type SpanResult = std::result::Result<SpanValue, SpanValue>;
pub type SpanCallback = Arc<dyn Fn(Arc<dyn TelemetrySpan + Send + Sync>) -> BoxFuture<SpanResult> + Send + Sync>;

/// Adapter contract for trusted typed inputs; malformed input behavior is unspecified.
#[async_trait]
pub trait TelemetryContext: Send + Sync {
    /// Adapter capabilities remain available on child spans and extracted parents.
    fn metrics(&self) -> Option<Arc<dyn TelemetryMetrics + Send + Sync>> {
        None
    }

    fn propagation(&self) -> Option<Arc<dyn TelemetryPropagation + Send + Sync>> {
        None
    }

    #[doc(hidden)]
    fn _in_memory_carrier(&self) -> Option<TelemetryCarrier> {
        None
    }

    async fn start_span(&self, options: SpanOptions, callback: SpanCallback) -> SpanResult;
}

pub trait TelemetrySpan: TelemetryContext {
    fn add_link(&self, _link: SpanLink) {}
    fn add_event(&self, name: &str, attributes: Option<SpanAttributes>);
    fn set_attributes(&self, attributes: SpanAttributes);
    fn set_status(&self, status: SpanStatus);
}

#[async_trait]
impl<T> TelemetryContext for Arc<T>
where
    T: TelemetryContext + ?Sized,
{
    fn metrics(&self) -> Option<Arc<dyn TelemetryMetrics + Send + Sync>> {
        (**self).metrics()
    }

    fn propagation(&self) -> Option<Arc<dyn TelemetryPropagation + Send + Sync>> {
        (**self).propagation()
    }

    fn _in_memory_carrier(&self) -> Option<TelemetryCarrier> {
        (**self)._in_memory_carrier()
    }

    async fn start_span(&self, options: SpanOptions, callback: SpanCallback) -> SpanResult {
        (**self).start_span(options, callback).await
    }
}

impl<T> TelemetrySpan for Arc<T>
where
    T: TelemetrySpan + ?Sized,
{
    fn add_link(&self, link: SpanLink) {
        (**self).add_link(link);
    }

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

pub use crate::metrics::{
    HistogramOptions, InstrumentOptions, SchemaTelemetryCounter, SchemaTelemetryHistogram,
    SchemaTelemetryUpDownCounter, TelemetryCounter, TelemetryHistogram, TelemetryMetricAttributeDefinition,
    TelemetryMetricDefinition, TelemetryMetrics, TelemetryUpDownCounter, TypedTelemetryMetrics, create_typed_metrics,
};
pub use crate::noop::{NOOP_TELEMETRY_CONTEXT, NOOP_TELEMETRY_METRICS, NOOP_TELEMETRY_PROPAGATION};

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

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetrySchemaDefinition {
    pub version: i64,
    pub spans: IndexMap<String, TelemetrySpanDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<IndexMap<String, TelemetryMetricDefinition>>,
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

fn bind_typed_span_starter<C>(telemetry_context: C) -> TypedSpanStarter
where
    C: TelemetryContext + 'static,
{
    let telemetry_context = Arc::new(telemetry_context);
    TypedSpanStarter {
        inner: Arc::new(move |name, attributes, callback| {
            let telemetry_context = Arc::clone(&telemetry_context);
            let options = SpanOptions {
                name: name.to_string(),
                attributes: Some(attributes),
                links: None,
            };
            Box::pin(async move {
                let span_callback: SpanCallback = Arc::new(move |span| {
                    let child_starter = bind_typed_span_starter(Arc::clone(&span));
                    callback(span, child_starter)
                });
                telemetry_context.start_span(options, span_callback).await
            })
        }),
    }
}

/// Bind an explicit parent context to the combined span vocabulary of one or more schemas.
/// Schema values are used only for type inference; no runtime schema validation is performed.
pub fn create_typed_span_starter<C>(telemetry_context: C, _schemas: &[TelemetrySchemaDefinition]) -> TypedSpanStarter
where
    C: TelemetryContext + 'static,
{
    bind_typed_span_starter(telemetry_context)
}

pub use crate::memory::{
    InMemoryTelemetryContext, RecordedTelemetryEvent, RecordedTelemetryLink, RecordedTelemetryMeasurement,
    RecordedTelemetrySpan, telemetry_now,
};
