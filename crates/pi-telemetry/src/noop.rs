//! Port of packages/telemetry/src/noop.ts

use async_trait::async_trait;

use crate::{SpanAttributes, SpanCallback, SpanOptions, SpanResult, SpanStatus, TelemetryContext, TelemetrySpan};

// PORT: TS types the const as `TelemetryContext`; the value is the shared frozen span.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoopTelemetryContext;

/// Shared telemetry context used when an application does not provide one.
pub const NOOP_TELEMETRY_CONTEXT: NoopTelemetryContext = NoopTelemetryContext;

#[async_trait]
impl TelemetryContext for NoopTelemetryContext {
    #[allow(unused_variables)]
    async fn start_span(&self, options: SpanOptions, callback: SpanCallback) -> SpanResult {
        start_noop_span(options, callback).await
    }
}

impl TelemetrySpan for NoopTelemetryContext {
    #[allow(unused_variables)]
    fn add_event(&self, _name: &str, _attributes: Option<SpanAttributes>) {}

    #[allow(unused_variables)]
    fn set_attributes(&self, _attributes: SpanAttributes) {}

    #[allow(unused_variables)]
    fn set_status(&self, _status: SpanStatus) {}
}

#[allow(unused_variables)]
async fn start_noop_span(options: SpanOptions, callback: SpanCallback) -> SpanResult {
    todo!("port: start_noop_span")
}
