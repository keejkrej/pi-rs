//! Port of packages/telemetry/src/noop.ts

use std::sync::{Arc, LazyLock};

use async_trait::async_trait;

use crate::{SpanAttributes, SpanCallback, SpanOptions, SpanResult, SpanStatus, TelemetryContext, TelemetrySpan};

// PORT: TS types the const as `TelemetryContext`; the value is the shared frozen span.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoopTelemetryContext;

/// Shared telemetry context used when an application does not provide one.
pub const NOOP_TELEMETRY_CONTEXT: NoopTelemetryContext = NoopTelemetryContext;

static NOOP_TELEMETRY_SPAN: LazyLock<Arc<dyn TelemetrySpan + Send + Sync>> =
    LazyLock::new(|| Arc::new(NoopTelemetryContext));

#[async_trait]
impl TelemetryContext for NoopTelemetryContext {
    async fn start_span(&self, options: SpanOptions, callback: SpanCallback) -> SpanResult {
        start_noop_span(options, callback).await
    }
}

impl TelemetrySpan for NoopTelemetryContext {
    fn add_event(&self, _name: &str, _attributes: Option<SpanAttributes>) {}

    fn set_attributes(&self, _attributes: SpanAttributes) {}

    fn set_status(&self, _status: SpanStatus) {}
}

async fn start_noop_span(_options: SpanOptions, callback: SpanCallback) -> SpanResult {
    callback(Arc::clone(&*NOOP_TELEMETRY_SPAN)).await
}
