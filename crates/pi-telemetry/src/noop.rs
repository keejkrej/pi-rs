//! Port of packages/telemetry/src/noop.ts

use std::sync::{Arc, LazyLock};

use async_trait::async_trait;

use crate::metrics::{
    HistogramOptions, InstrumentOptions, TelemetryCounter, TelemetryHistogram, TelemetryMetrics, TelemetryUpDownCounter,
};
use crate::{
    SpanAttributes, SpanCallback, SpanLink, SpanOptions, SpanResult, SpanStatus, TelemetryCarrier, TelemetryContext,
    TelemetryPropagation, TelemetrySpan,
};

struct NoopInstrument;

impl TelemetryCounter for NoopInstrument {
    fn add(&self, _value: f64, _attributes: Option<SpanAttributes>) {}
}

impl TelemetryUpDownCounter for NoopInstrument {
    fn add(&self, _value: f64, _attributes: Option<SpanAttributes>) {}
}

impl TelemetryHistogram for NoopInstrument {
    fn record(&self, _value: f64, _attributes: Option<SpanAttributes>, _context: Option<&dyn TelemetryContext>) {}
}

static NOOP_COUNTER: LazyLock<Arc<dyn TelemetryCounter + Send + Sync>> = LazyLock::new(|| Arc::new(NoopInstrument));
static NOOP_UP_DOWN_COUNTER: LazyLock<Arc<dyn TelemetryUpDownCounter + Send + Sync>> =
    LazyLock::new(|| Arc::new(NoopInstrument));
static NOOP_HISTOGRAM: LazyLock<Arc<dyn TelemetryHistogram + Send + Sync>> = LazyLock::new(|| Arc::new(NoopInstrument));

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoopTelemetryMetrics;

/// Shared instruments that never inspect or retain metadata, measurements, or contexts.
pub const NOOP_TELEMETRY_METRICS: NoopTelemetryMetrics = NoopTelemetryMetrics;

static NOOP_METRICS_ARC: LazyLock<Arc<dyn TelemetryMetrics + Send + Sync>> =
    LazyLock::new(|| Arc::new(NoopTelemetryMetrics));

impl TelemetryMetrics for NoopTelemetryMetrics {
    fn counter(&self, _name: &str, _options: Option<InstrumentOptions>) -> Arc<dyn TelemetryCounter + Send + Sync> {
        Arc::clone(&*NOOP_COUNTER)
    }

    fn up_down_counter(
        &self,
        _name: &str,
        _options: Option<InstrumentOptions>,
    ) -> Arc<dyn TelemetryUpDownCounter + Send + Sync> {
        Arc::clone(&*NOOP_UP_DOWN_COUNTER)
    }

    fn histogram(&self, _name: &str, _options: Option<HistogramOptions>) -> Arc<dyn TelemetryHistogram + Send + Sync> {
        Arc::clone(&*NOOP_HISTOGRAM)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoopTelemetryPropagation;

/// Shared propagation capability that neither inspects nor retains its inputs.
pub const NOOP_TELEMETRY_PROPAGATION: NoopTelemetryPropagation = NoopTelemetryPropagation;

static NOOP_PROPAGATION_ARC: LazyLock<Arc<dyn TelemetryPropagation + Send + Sync>> =
    LazyLock::new(|| Arc::new(NoopTelemetryPropagation));

static NOOP_CONTEXT_ARC: LazyLock<Arc<dyn TelemetryContext + Send + Sync>> =
    LazyLock::new(|| Arc::new(NoopTelemetryContext));

impl TelemetryPropagation for NoopTelemetryPropagation {
    fn inject(&self, _context: &dyn TelemetryContext) -> Option<TelemetryCarrier> {
        None
    }

    fn extract(&self, _carrier: Option<&TelemetryCarrier>) -> Arc<dyn TelemetryContext + Send + Sync> {
        Arc::clone(&*NOOP_CONTEXT_ARC)
    }
}

// PORT: TS types the const as `TelemetrySpan`; Rust exposes a unit struct implementing both `TelemetryContext` and `TelemetrySpan`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoopTelemetryContext;

/// Shared telemetry context used when an application does not provide one.
pub const NOOP_TELEMETRY_CONTEXT: NoopTelemetryContext = NoopTelemetryContext;

static NOOP_TELEMETRY_SPAN: LazyLock<Arc<dyn TelemetrySpan + Send + Sync>> =
    LazyLock::new(|| Arc::new(NoopTelemetryContext));

#[async_trait]
impl TelemetryContext for NoopTelemetryContext {
    fn metrics(&self) -> Option<Arc<dyn TelemetryMetrics + Send + Sync>> {
        Some(Arc::clone(&*NOOP_METRICS_ARC))
    }

    fn propagation(&self) -> Option<Arc<dyn TelemetryPropagation + Send + Sync>> {
        Some(Arc::clone(&*NOOP_PROPAGATION_ARC))
    }

    async fn start_span(&self, options: SpanOptions, callback: SpanCallback) -> SpanResult {
        start_noop_span(options, callback).await
    }
}

impl TelemetrySpan for NoopTelemetryContext {
    fn add_link(&self, _link: SpanLink) {}

    fn add_event(&self, _name: &str, _attributes: Option<SpanAttributes>) {}

    fn set_attributes(&self, _attributes: SpanAttributes) {}

    fn set_status(&self, _status: SpanStatus) {}
}

async fn start_noop_span(_options: SpanOptions, callback: SpanCallback) -> SpanResult {
    callback(Arc::clone(&*NOOP_TELEMETRY_SPAN)).await
}
