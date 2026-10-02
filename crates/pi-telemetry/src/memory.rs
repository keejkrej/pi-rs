//! Port of packages/telemetry/src/memory.ts

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{
    AttributeValue, SpanAttributes, SpanCallback, SpanOptions, SpanResult, SpanStatus, SpanValue, TelemetryContext,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedTelemetryEvent {
    pub name: String,
    pub attributes: SpanAttributes,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedTelemetrySpan {
    pub id: i64,
    // PORT: `parentId` is `number | null` and is always present (JSON null), not omitted.
    #[serde(default)]
    pub parent_id: Option<i64>,
    pub name: String,
    pub attributes: SpanAttributes,
    pub events: Vec<RecordedTelemetryEvent>,
    pub status: SpanStatus,
    pub settled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_sequence: Option<i64>,
}

#[allow(dead_code)]
#[derive(Debug)]
struct MutableRecordedTelemetryEvent {
    name: String,
    attributes: SpanAttributes,
}

#[allow(dead_code)]
#[derive(Debug)]
struct MutableRecordedTelemetrySpan {
    id: i64,
    parent_id: Option<i64>,
    name: String,
    attributes: SpanAttributes,
    events: Vec<MutableRecordedTelemetryEvent>,
    status: SpanStatus,
    explicit_status: bool,
    settled: bool,
    end_sequence: Option<i64>,
}

#[allow(dead_code)]
#[derive(Debug)]
struct InMemoryTelemetryState {
    spans: Vec<MutableRecordedTelemetrySpan>,
    next_span_id: i64,
    next_end_sequence: i64,
}

#[derive(Debug)]
struct InMemoryTelemetryInner {
    state: Mutex<InMemoryTelemetryState>,
}

/// Backend-neutral reference implementation that records spans in process memory.
/// Create a fresh instance to isolate tests or independent recording scopes.
#[derive(Clone, Debug)]
pub struct InMemoryTelemetryContext {
    inner: Arc<InMemoryTelemetryInner>,
}

impl InMemoryTelemetryContext {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(InMemoryTelemetryInner {
                state: Mutex::new(InMemoryTelemetryState {
                    spans: Vec::new(),
                    next_span_id: 1,
                    next_end_sequence: 1,
                }),
            }),
        }
    }

    /// Returns detached snapshots in span-start order.
    #[allow(unused_variables)]
    pub fn get_spans(&self) -> Vec<RecordedTelemetrySpan> {
        todo!("port: InMemoryTelemetryContext::get_spans")
    }
}

#[async_trait]
impl TelemetryContext for InMemoryTelemetryContext {
    async fn start_span(&self, options: SpanOptions, callback: SpanCallback) -> SpanResult {
        start_in_memory_span(&self.inner.state, None, options, callback).await
    }
}

#[allow(dead_code, unused_variables)]
fn copy_attribute_value(value: &AttributeValue) -> AttributeValue {
    todo!("port: copy_attribute_value")
}

#[allow(dead_code, unused_variables)]
fn copy_attributes(attributes: Option<&SpanAttributes>) -> SpanAttributes {
    todo!("port: copy_attributes")
}

#[allow(dead_code, unused_variables)]
fn merge_attributes(current: &SpanAttributes, attributes: &SpanAttributes) -> SpanAttributes {
    todo!("port: merge_attributes")
}

#[allow(dead_code, unused_variables)]
fn copy_status(status: &SpanStatus) -> SpanStatus {
    todo!("port: copy_status")
}

// Error inspection is passive. Fall through to an error status without details.
#[allow(dead_code, unused_variables)]
fn automatic_error_status(error: &SpanValue) -> SpanStatus {
    todo!("port: automatic_error_status")
}

#[allow(dead_code, unused_variables)]
fn settle_span(
    state: &mut InMemoryTelemetryState,
    span: &mut MutableRecordedTelemetrySpan,
    failed: bool,
    error: Option<&SpanValue>,
) {
    todo!("port: settle_span")
}

#[allow(dead_code, unused_variables)]
fn create_span(
    state: &mut InMemoryTelemetryState,
    parent: Option<&MutableRecordedTelemetrySpan>,
    options: &SpanOptions,
) -> MutableRecordedTelemetrySpan {
    todo!("port: create_span")
}

// Recording is passive. Ignore malformed or unreadable telemetry payloads.
#[allow(unused_variables)]
async fn start_in_memory_span(
    state: &Mutex<InMemoryTelemetryState>,
    parent_id: Option<i64>,
    options: SpanOptions,
    callback: SpanCallback,
) -> SpanResult {
    todo!("port: start_in_memory_span")
}
