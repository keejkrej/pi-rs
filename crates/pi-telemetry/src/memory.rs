//! Port of packages/telemetry/src/memory.ts

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{
    AttributeValue, NOOP_TELEMETRY_CONTEXT, SpanAttributes, SpanCallback, SpanOptions, SpanResult, SpanStatus,
    SpanStatusError, SpanValue, TelemetryContext, TelemetrySpan,
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

#[derive(Debug)]
struct MutableRecordedTelemetryEvent {
    name: String,
    attributes: SpanAttributes,
}

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
    pub fn get_spans(&self) -> Vec<RecordedTelemetrySpan> {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .spans
            .iter()
            .map(|span| RecordedTelemetrySpan {
                id: span.id,
                parent_id: span.parent_id,
                name: span.name.clone(),
                attributes: copy_attributes(Some(&span.attributes)),
                events: span
                    .events
                    .iter()
                    .map(|event| RecordedTelemetryEvent {
                        name: event.name.clone(),
                        attributes: copy_attributes(Some(&event.attributes)),
                    })
                    .collect(),
                status: copy_status(&span.status),
                settled: span.settled,
                end_sequence: span.end_sequence,
            })
            .collect()
    }
}

impl Default for InMemoryTelemetryContext {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TelemetryContext for InMemoryTelemetryContext {
    async fn start_span(&self, options: SpanOptions, callback: SpanCallback) -> SpanResult {
        start_in_memory_span(&self.inner, None, options, callback).await
    }
}

#[derive(Clone)]
struct InMemoryTelemetrySpan {
    inner: Arc<InMemoryTelemetryInner>,
    span_id: i64,
}

#[async_trait]
impl TelemetryContext for InMemoryTelemetrySpan {
    async fn start_span(&self, options: SpanOptions, callback: SpanCallback) -> SpanResult {
        start_in_memory_span(&self.inner, Some(self.span_id), options, callback).await
    }
}

impl TelemetrySpan for InMemoryTelemetrySpan {
    fn add_event(&self, name: &str, attributes: Option<SpanAttributes>) {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(span) = find_span_mut(&mut state.spans, self.span_id) else {
            return;
        };
        if span.settled {
            return;
        }
        span.events.push(MutableRecordedTelemetryEvent {
            name: name.to_string(),
            attributes: copy_attributes(attributes.as_ref()),
        });
    }

    fn set_attributes(&self, attributes: SpanAttributes) {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(span) = find_span_mut(&mut state.spans, self.span_id) else {
            return;
        };
        if span.settled {
            return;
        }
        span.attributes = merge_attributes(&span.attributes, &attributes);
    }

    fn set_status(&self, status: SpanStatus) {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(span) = find_span_mut(&mut state.spans, self.span_id) else {
            return;
        };
        if span.settled {
            return;
        }
        span.status = copy_status(&status);
        span.explicit_status = true;
    }
}

fn find_span(spans: &[MutableRecordedTelemetrySpan], id: i64) -> Option<&MutableRecordedTelemetrySpan> {
    if let Ok(idx) = usize::try_from(id - 1)
        && let Some(span) = spans.get(idx)
        && span.id == id
    {
        return Some(span);
    }
    spans.iter().find(|s| s.id == id)
}

fn find_span_mut(spans: &mut [MutableRecordedTelemetrySpan], id: i64) -> Option<&mut MutableRecordedTelemetrySpan> {
    if let Ok(idx) = usize::try_from(id - 1)
        && spans.get(idx).is_some_and(|s| s.id == id)
    {
        return spans.get_mut(idx);
    }
    spans.iter_mut().find(|s| s.id == id)
}

fn copy_attribute_value(value: &AttributeValue) -> AttributeValue {
    value.clone()
}

fn copy_attributes(attributes: Option<&SpanAttributes>) -> SpanAttributes {
    let mut copy = SpanAttributes::new();
    let Some(attributes) = attributes else {
        return copy;
    };
    for (name, value) in attributes {
        copy.insert(name.clone(), copy_attribute_value(value));
    }
    copy
}

fn merge_attributes(current: &SpanAttributes, attributes: &SpanAttributes) -> SpanAttributes {
    let mut merged = copy_attributes(Some(current));
    for (name, value) in attributes {
        merged.insert(name.clone(), copy_attribute_value(value));
    }
    merged
}

fn copy_status(status: &SpanStatus) -> SpanStatus {
    status.clone()
}

// Error inspection is passive. Fall through to an error status without details.
fn automatic_error_status(error: &SpanValue) -> SpanStatus {
    if let Some(js_err) = error.downcast_ref::<pi_js::Error>() {
        return SpanStatus::Error {
            error: Some(SpanStatusError {
                name: js_err.name().to_string(),
                message: js_err.message(),
            }),
        };
    }
    if let Some(js_err) = error.downcast_ref::<pi_js::JsError>() {
        return SpanStatus::Error {
            error: Some(SpanStatusError {
                name: js_err.name.clone(),
                message: js_err.message.clone(),
            }),
        };
    }
    if let Some(node_err) = error.downcast_ref::<pi_js::NodeError>() {
        return SpanStatus::Error {
            error: Some(SpanStatusError {
                name: "Error".to_string(),
                message: node_err.message.clone(),
            }),
        };
    }
    SpanStatus::Error { error: None }
}

fn settle_span(state: &mut InMemoryTelemetryState, span_id: i64, failed: bool, error: Option<&SpanValue>) {
    let next_end_sequence = state.next_end_sequence;
    let Some(span) = find_span_mut(&mut state.spans, span_id) else {
        return;
    };
    if span.settled {
        return;
    }
    if failed && !span.explicit_status {
        span.status = match error {
            Some(err) => automatic_error_status(err),
            None => SpanStatus::Error { error: None },
        };
    }
    span.settled = true;
    span.end_sequence = Some(next_end_sequence);
    state.next_end_sequence += 1;
}

fn create_span(
    state: &mut InMemoryTelemetryState,
    parent_id: Option<i64>,
    options: &SpanOptions,
) -> MutableRecordedTelemetrySpan {
    let id = state.next_span_id;
    state.next_span_id += 1;
    let name = options.name.clone();
    let attributes = copy_attributes(options.attributes.as_ref());
    MutableRecordedTelemetrySpan {
        id,
        parent_id,
        name,
        attributes,
        events: Vec::new(),
        status: SpanStatus::Ok,
        explicit_status: false,
        settled: false,
        end_sequence: None,
    }
}

struct SettleGuard<'a> {
    inner: &'a Arc<InMemoryTelemetryInner>,
    span_id: i64,
    disarmed: bool,
}

impl Drop for SettleGuard<'_> {
    fn drop(&mut self) {
        if !self.disarmed {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            settle_span(&mut state, self.span_id, true, None);
        }
    }
}

// Recording is passive. Ignore malformed or unreadable telemetry payloads.
async fn start_in_memory_span(
    inner: &Arc<InMemoryTelemetryInner>,
    parent_id: Option<i64>,
    options: SpanOptions,
    callback: SpanCallback,
) -> SpanResult {
    let span_id = {
        let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pid) = parent_id
            && find_span(&state.spans, pid).is_none_or(|p| p.settled)
        {
            None
        } else {
            let recorded_span = create_span(&mut state, parent_id, &options);
            let span_id = recorded_span.id;
            state.spans.push(recorded_span);
            Some(span_id)
        }
    };
    let Some(span_id) = span_id else {
        return NOOP_TELEMETRY_CONTEXT.start_span(options, callback).await;
    };

    let mut guard = SettleGuard {
        inner,
        span_id,
        disarmed: false,
    };

    let span: Arc<dyn TelemetrySpan + Send + Sync> = Arc::new(InMemoryTelemetrySpan {
        inner: Arc::clone(inner),
        span_id,
    });
    let result = callback(span).await;

    guard.disarmed = true;
    {
        let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
        match &result {
            Ok(_) => settle_span(&mut state, span_id, false, None),
            Err(error) => settle_span(&mut state, span_id, true, Some(error)),
        }
    }

    result
}
