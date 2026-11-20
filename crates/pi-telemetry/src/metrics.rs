//! Port of packages/telemetry/src/metrics.ts

use std::sync::Arc;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::{SpanAttributes, TelemetryContext, TelemetrySchemaDefinition, TelemetryStartAttributeDefinition};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistogramOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Bucket hints; callers supply finite, strictly increasing values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundaries: Option<Vec<f64>>,
}

impl From<InstrumentOptions> for HistogramOptions {
    fn from(options: InstrumentOptions) -> Self {
        Self {
            description: options.description,
            unit: options.unit,
            boundaries: None,
        }
    }
}

impl From<&InstrumentOptions> for HistogramOptions {
    fn from(options: &InstrumentOptions) -> Self {
        Self {
            description: options.description.clone(),
            unit: options.unit.clone(),
            boundaries: None,
        }
    }
}

impl From<&HistogramOptions> for InstrumentOptions {
    fn from(options: &HistogramOptions) -> Self {
        Self {
            description: options.description.clone(),
            unit: options.unit.clone(),
        }
    }
}

pub trait TelemetryCounter: Send + Sync {
    fn add(&self, value: f64, attributes: Option<SpanAttributes>);
}

pub trait TelemetryUpDownCounter: Send + Sync {
    fn add(&self, value: f64, attributes: Option<SpanAttributes>);
}

pub trait TelemetryHistogram: Send + Sync {
    fn record(&self, value: f64, attributes: Option<SpanAttributes>, context: Option<&dyn TelemetryContext>);
}

/// Optional adapter capability with synchronous factories and recording methods.
/// Typed inputs are trusted; callers supply finite measurements and nonnegative counter increments.
/// Malformed input behavior is unspecified.
pub trait TelemetryMetrics: Send + Sync {
    fn counter(&self, name: &str, options: Option<InstrumentOptions>) -> Arc<dyn TelemetryCounter + Send + Sync>;
    fn up_down_counter(
        &self,
        name: &str,
        options: Option<InstrumentOptions>,
    ) -> Arc<dyn TelemetryUpDownCounter + Send + Sync>;
    fn histogram(&self, name: &str, options: Option<HistogramOptions>) -> Arc<dyn TelemetryHistogram + Send + Sync>;
}

impl<T> TelemetryCounter for Arc<T>
where
    T: TelemetryCounter + ?Sized,
{
    fn add(&self, value: f64, attributes: Option<SpanAttributes>) {
        (**self).add(value, attributes);
    }
}

impl<T> TelemetryUpDownCounter for Arc<T>
where
    T: TelemetryUpDownCounter + ?Sized,
{
    fn add(&self, value: f64, attributes: Option<SpanAttributes>) {
        (**self).add(value, attributes);
    }
}

impl<T> TelemetryHistogram for Arc<T>
where
    T: TelemetryHistogram + ?Sized,
{
    fn record(&self, value: f64, attributes: Option<SpanAttributes>, context: Option<&dyn TelemetryContext>) {
        (**self).record(value, attributes, context);
    }
}

impl<T> TelemetryMetrics for Arc<T>
where
    T: TelemetryMetrics + ?Sized,
{
    fn counter(&self, name: &str, options: Option<InstrumentOptions>) -> Arc<dyn TelemetryCounter + Send + Sync> {
        (**self).counter(name, options)
    }

    fn up_down_counter(
        &self,
        name: &str,
        options: Option<InstrumentOptions>,
    ) -> Arc<dyn TelemetryUpDownCounter + Send + Sync> {
        (**self).up_down_counter(name, options)
    }

    fn histogram(&self, name: &str, options: Option<HistogramOptions>) -> Arc<dyn TelemetryHistogram + Send + Sync> {
        (**self).histogram(name, options)
    }
}

// PORT: identical runtime shape to `TelemetryStartAttributeDefinition`; TS `cardinality?: "low"` restriction is compile-time.
pub type TelemetryMetricAttributeDefinition = TelemetryStartAttributeDefinition;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum TelemetryMetricDefinition {
    #[serde(rename = "counter")]
    Counter {
        description: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
        attributes: IndexMap<String, TelemetryMetricAttributeDefinition>,
    },
    #[serde(rename = "upDownCounter")]
    UpDownCounter {
        description: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
        attributes: IndexMap<String, TelemetryMetricAttributeDefinition>,
    },
    #[serde(rename = "histogram")]
    Histogram {
        description: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        boundaries: Option<Vec<f64>>,
        attributes: IndexMap<String, TelemetryMetricAttributeDefinition>,
    },
}

impl TelemetryMetricDefinition {
    pub fn instrument_options(&self) -> InstrumentOptions {
        match self {
            Self::Counter { description, unit, .. }
            | Self::UpDownCounter { description, unit, .. }
            | Self::Histogram { description, unit, .. } => InstrumentOptions {
                description: Some(description.clone()),
                unit: unit.clone(),
            },
        }
    }

    pub fn histogram_options(&self) -> HistogramOptions {
        match self {
            Self::Counter { description, unit, .. } | Self::UpDownCounter { description, unit, .. } => {
                HistogramOptions {
                    description: Some(description.clone()),
                    unit: unit.clone(),
                    boundaries: None,
                }
            }
            Self::Histogram {
                description,
                unit,
                boundaries,
                ..
            } => HistogramOptions {
                description: Some(description.clone()),
                unit: unit.clone(),
                boundaries: boundaries.clone(),
            },
        }
    }
}

// PORT: TS conditional types erased to concrete instrument handles (PORTING.md §3.2).
pub type SchemaTelemetryCounter = Arc<dyn TelemetryCounter + Send + Sync>;
pub type SchemaTelemetryUpDownCounter = Arc<dyn TelemetryUpDownCounter + Send + Sync>;
pub type SchemaTelemetryHistogram = Arc<dyn TelemetryHistogram + Send + Sync>;

#[derive(Clone)]
pub struct TypedTelemetryMetrics {
    metrics: Arc<dyn TelemetryMetrics + Send + Sync>,
    definitions: IndexMap<String, TelemetryMetricDefinition>,
}

impl TypedTelemetryMetrics {
    pub fn counter(&self, name: &str) -> SchemaTelemetryCounter {
        let options = self
            .definitions
            .get(name)
            .map(TelemetryMetricDefinition::instrument_options);
        self.metrics.counter(name, options)
    }

    pub fn up_down_counter(&self, name: &str) -> SchemaTelemetryUpDownCounter {
        let options = self
            .definitions
            .get(name)
            .map(TelemetryMetricDefinition::instrument_options);
        self.metrics.up_down_counter(name, options)
    }

    pub fn histogram(&self, name: &str) -> SchemaTelemetryHistogram {
        let options = self
            .definitions
            .get(name)
            .map(TelemetryMetricDefinition::histogram_options);
        self.metrics.histogram(name, options)
    }
}

/// Bind instrument names, kinds, metadata, and exact measurement attributes to one or more schemas.
pub fn create_typed_metrics<M>(metrics: M, schemas: &[TelemetrySchemaDefinition]) -> TypedTelemetryMetrics
where
    M: TelemetryMetrics + 'static,
{
    let mut definitions = IndexMap::new();
    for schema in schemas {
        if let Some(schema_metrics) = &schema.metrics {
            for (name, definition) in schema_metrics {
                definitions.insert(name.clone(), definition.clone());
            }
        }
    }
    TypedTelemetryMetrics {
        metrics: Arc::new(metrics),
        definitions,
    }
}
