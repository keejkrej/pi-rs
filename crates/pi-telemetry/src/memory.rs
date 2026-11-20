//! Port of packages/telemetry/src/memory.ts

use std::sync::{Arc, LazyLock, Mutex, Weak};

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::metrics::{
    HistogramOptions, InstrumentOptions, TelemetryCounter, TelemetryHistogram, TelemetryMetrics, TelemetryUpDownCounter,
};
use crate::{
    AttributeValue, FindTracesResult, MetricKind, MetricQuery, Result, SpanAttributes, SpanCallback, SpanLink,
    SpanOptions, SpanResult, SpanStatus, SpanStatusError, SpanValue, StoredMetric, StoredSpan, StoredSpanEvent,
    StoredSpanLink, StoredTrace, TelemetryCarrier, TelemetryContext, TelemetryPropagation, TelemetrySpan,
    TelemetryStore, TraceQuery,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedTelemetryEvent {
    pub name: String,
    pub attributes: SpanAttributes,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedTelemetryLink {
    pub context: TelemetryCarrier,
    pub attributes: SpanAttributes,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedTelemetrySpan {
    pub id: i64,
    // PORT: `parentId` is `number | null` and is always present (JSON null), not omitted.
    #[serde(default)]
    pub parent_id: Option<i64>,
    pub context: TelemetryCarrier,
    // PORT: `parentContext` is `TelemetryCarrier | null` and is always present (JSON null), not omitted.
    #[serde(default)]
    pub parent_context: Option<TelemetryCarrier>,
    pub links: Vec<RecordedTelemetryLink>,
    pub name: String,
    pub attributes: SpanAttributes,
    pub events: Vec<RecordedTelemetryEvent>,
    pub status: SpanStatus,
    pub settled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_sequence: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedTelemetryMeasurement {
    pub kind: MetricKind,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundaries: Option<Vec<f64>>,
    pub value: f64,
    pub attributes: SpanAttributes,
    // PORT: `context` is `TelemetryCarrier | null` and is always present (JSON null), not omitted.
    #[serde(default)]
    pub context: Option<TelemetryCarrier>,
}

#[derive(Debug)]
struct MutableRecordedTelemetryEvent {
    time_ms: f64,
    name: String,
    attributes: SpanAttributes,
}

#[derive(Debug)]
struct MutableRecordedTelemetrySpan {
    started_ms: f64,
    clock: f64,
    ended_ms: Option<f64>,
    id: i64,
    parent_id: Option<i64>,
    context: TelemetryCarrier,
    parent_context: Option<TelemetryCarrier>,
    links: Vec<RecordedTelemetryLink>,
    name: String,
    attributes: SpanAttributes,
    events: Vec<MutableRecordedTelemetryEvent>,
    status: SpanStatus,
    explicit_status: bool,
    settled: bool,
    end_sequence: Option<i64>,
}

#[derive(Debug)]
struct RecordedMeasurementEntry {
    interval_ms: i64,
    measurement: RecordedTelemetryMeasurement,
}

#[derive(Clone)]
struct InMemoryParent {
    context: TelemetryCarrier,
    local_id: Option<i64>,
    check_settled: bool,
}

fn carrier_id(carrier: &TelemetryCarrier) -> String {
    let scope = carrier.get("pi-memory-scope").map_or("", String::as_str);
    let span = carrier.get("pi-memory-span").map_or("", String::as_str);
    format!("{scope}:{span}")
}

// The clock of span times: wall-clock milliseconds at microsecond resolution, from the monotonic high-resolution
// clock anchored to the wall clock (`telemetry_now`, also Chord's SQLite store's).

/// How far the anchored clock may part from `Date.now()` before it is anchored anew: more than `Date.now()`'s own
/// truncation to whole milliseconds, less than any step worth following (a wall clock set, a sleep the monotonic clock
/// did not count).
const TOLERANCE_MS: f64 = 10.0;

static WALL_NOW_OVERRIDE: LazyLock<pi_js::seam::Slot<dyn Fn() -> f64 + Send + Sync>> =
    LazyLock::new(pi_js::seam::Slot::new);
static PERF_NOW_OVERRIDE: LazyLock<pi_js::seam::Slot<dyn Fn() -> f64 + Send + Sync>> =
    LazyLock::new(pi_js::seam::Slot::new);

fn wall_now_ms() -> f64 {
    if let Some(override_fn) = WALL_NOW_OVERRIDE.get() {
        return override_fn();
    }
    pi_js::time::now_ms() as f64
}

fn perf_now_ms() -> f64 {
    if let Some(override_fn) = PERF_NOW_OVERRIDE.get() {
        return override_fn();
    }
    pi_js::time::performance_now()
}

/// The wall-clock time of `performance.now()`'s zero; `performance.timeOrigin` until the first step is followed.
static ANCHOR_MS: LazyLock<Mutex<f64>> = LazyLock::new(|| Mutex::new(wall_now_ms() - perf_now_ms()));

/// Now as milliseconds since the epoch, rounded to the microsecond: `performance.now()` (monotonic, sub-millisecond)
/// plus an anchor to the wall clock. While the two agree within 10 ms, times in one process keep their order and
/// spacing to the microsecond; past that the clock follows `Date.now()` again, at whole milliseconds. Every adapter
/// of this package and Chord's SQLite store stamp span starts with it, so their times agree.
pub fn telemetry_now() -> f64 {
    let clock = perf_now_ms();
    let wall = wall_now_ms();
    let mut anchor = ANCHOR_MS.lock().unwrap_or_else(|e| e.into_inner());
    if (*anchor + clock - wall).abs() > TOLERANCE_MS {
        *anchor = wall - clock;
    }
    microseconds(*anchor + clock)
}

/// `ms` rounded to the microsecond, as span times are.
fn microseconds(ms: f64) -> f64 {
    pi_js::num::math_round(ms * 1000.0) / 1000.0
}

#[derive(Debug)]
struct InMemoryTelemetryState {
    scope: String,
    spans: Vec<MutableRecordedTelemetrySpan>,
    measurements: Vec<RecordedMeasurementEntry>,
    next_span_id: i64,
    next_end_sequence: i64,
}

#[derive(Debug)]
struct InMemoryTelemetryInner {
    self_weak: Weak<InMemoryTelemetryInner>,
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
        let inner = Arc::new_cyclic(|weak| InMemoryTelemetryInner {
            self_weak: weak.clone(),
            state: Mutex::new(InMemoryTelemetryState {
                scope: pi_js::crypto::random_uuid(),
                spans: Vec::new(),
                measurements: Vec::new(),
                next_span_id: 1,
                next_end_sequence: 1,
            }),
        });
        Self { inner }
    }

    /// Returns the shared root `TelemetryContext` handle for this recorder.
    pub fn as_context(&self) -> Arc<dyn TelemetryContext + Send + Sync> {
        Arc::clone(&self.inner) as Arc<dyn TelemetryContext + Send + Sync>
    }

    /// Returns detached measurements in recording order, without aggregation.
    pub fn get_metrics(&self) -> Vec<RecordedTelemetryMeasurement> {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .measurements
            .iter()
            .map(|entry| RecordedTelemetryMeasurement {
                kind: entry.measurement.kind,
                name: entry.measurement.name.clone(),
                description: entry.measurement.description.clone(),
                unit: entry.measurement.unit.clone(),
                boundaries: entry.measurement.boundaries.clone(),
                value: entry.measurement.value,
                attributes: copy_attributes(Some(&entry.measurement.attributes)),
                context: entry.measurement.context.as_ref().map(copy_carrier),
            })
            .collect()
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
                context: copy_carrier(&span.context),
                parent_context: span.parent_context.as_ref().map(copy_carrier),
                links: span
                    .links
                    .iter()
                    .map(|link| RecordedTelemetryLink {
                        context: copy_carrier(&link.context),
                        attributes: copy_attributes(Some(&link.attributes)),
                    })
                    .collect(),
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
    fn metrics(&self) -> Option<Arc<dyn TelemetryMetrics + Send + Sync>> {
        self.inner.metrics()
    }

    fn propagation(&self) -> Option<Arc<dyn TelemetryPropagation + Send + Sync>> {
        self.inner.propagation()
    }

    async fn start_span(&self, options: SpanOptions, callback: SpanCallback) -> SpanResult {
        start_in_memory_span(&self.inner, None, options, callback).await
    }
}

impl TelemetryPropagation for InMemoryTelemetryContext {
    fn inject(&self, context: &dyn TelemetryContext) -> Option<TelemetryCarrier> {
        self.inner.inject(context)
    }

    fn extract(&self, carrier: Option<&TelemetryCarrier>) -> Arc<dyn TelemetryContext + Send + Sync> {
        self.inner.extract(carrier)
    }
}

impl TelemetryMetrics for InMemoryTelemetryContext {
    fn counter(&self, name: &str, options: Option<InstrumentOptions>) -> Arc<dyn TelemetryCounter + Send + Sync> {
        self.inner.counter(name, options)
    }

    fn up_down_counter(
        &self,
        name: &str,
        options: Option<InstrumentOptions>,
    ) -> Arc<dyn TelemetryUpDownCounter + Send + Sync> {
        self.inner.up_down_counter(name, options)
    }

    fn histogram(&self, name: &str, options: Option<HistogramOptions>) -> Arc<dyn TelemetryHistogram + Send + Sync> {
        self.inner.histogram(name, options)
    }
}

#[async_trait]
impl TelemetryStore for InMemoryTelemetryContext {
    async fn find_traces(&self, query: Option<TraceQuery>) -> Result<FindTracesResult> {
        self.inner.find_traces(query).await
    }

    async fn get_trace(&self, trace_id: &str) -> Result<Option<StoredTrace>> {
        self.inner.get_trace(trace_id).await
    }

    async fn find_metrics(&self, query: MetricQuery) -> Result<Vec<StoredMetric>> {
        self.inner.find_metrics(query).await
    }
}

#[async_trait]
impl TelemetryContext for InMemoryTelemetryInner {
    fn metrics(&self) -> Option<Arc<dyn TelemetryMetrics + Send + Sync>> {
        self.self_weak
            .upgrade()
            .map(|arc| arc as Arc<dyn TelemetryMetrics + Send + Sync>)
    }

    fn propagation(&self) -> Option<Arc<dyn TelemetryPropagation + Send + Sync>> {
        self.self_weak
            .upgrade()
            .map(|arc| arc as Arc<dyn TelemetryPropagation + Send + Sync>)
    }

    async fn start_span(&self, options: SpanOptions, callback: SpanCallback) -> SpanResult {
        let Some(inner) = self.self_weak.upgrade() else {
            return crate::NOOP_TELEMETRY_CONTEXT.start_span(options, callback).await;
        };
        start_in_memory_span(&inner, None, options, callback).await
    }
}

impl TelemetryPropagation for InMemoryTelemetryInner {
    /// Returns a detached carrier for a known span or extracted parent, including settled spans.
    fn inject(&self, context: &dyn TelemetryContext) -> Option<TelemetryCarrier> {
        context._in_memory_carrier()
    }

    /// Missing or non-string carrier identities fall back to this recorder's root context.
    fn extract(&self, carrier: Option<&TelemetryCarrier>) -> Arc<dyn TelemetryContext + Send + Sync> {
        let root = self
            .self_weak
            .upgrade()
            .map(|arc| arc as Arc<dyn TelemetryContext + Send + Sync>);
        let Some(carrier) = carrier else {
            return root.unwrap_or_else(|| Arc::new(crate::NOOP_TELEMETRY_CONTEXT));
        };
        let (Some(scope), Some(span), Some(trace_id)) = (
            carrier.get("pi-memory-scope"),
            carrier.get("pi-memory-span"),
            carrier.get("pi-memory-trace"),
        ) else {
            return root.unwrap_or_else(|| Arc::new(crate::NOOP_TELEMETRY_CONTEXT));
        };
        let mut context = TelemetryCarrier::new();
        context.insert("pi-memory-scope".to_string(), scope.clone());
        context.insert("pi-memory-span".to_string(), span.clone());
        context.insert("pi-memory-trace".to_string(), trace_id.clone());

        let Some(inner) = self.self_weak.upgrade() else {
            return Arc::new(crate::NOOP_TELEMETRY_CONTEXT);
        };
        let local_id = {
            let state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if scope == &state.scope {
                span.parse::<i64>().ok()
            } else {
                None
            }
        };
        Arc::new(InMemoryExtractedParent {
            inner,
            context,
            local_id,
        })
    }
}

impl TelemetryMetrics for InMemoryTelemetryInner {
    fn counter(&self, name: &str, options: Option<InstrumentOptions>) -> Arc<dyn TelemetryCounter + Send + Sync> {
        let inner = self.self_weak.upgrade();
        let histogram_options = options.as_ref().map(HistogramOptions::from);
        Arc::new(InMemoryInstrument::new(
            inner,
            MetricKind::Counter,
            name,
            histogram_options.as_ref(),
        ))
    }

    fn up_down_counter(
        &self,
        name: &str,
        options: Option<InstrumentOptions>,
    ) -> Arc<dyn TelemetryUpDownCounter + Send + Sync> {
        let inner = self.self_weak.upgrade();
        let histogram_options = options.as_ref().map(HistogramOptions::from);
        Arc::new(InMemoryInstrument::new(
            inner,
            MetricKind::UpDownCounter,
            name,
            histogram_options.as_ref(),
        ))
    }

    fn histogram(&self, name: &str, options: Option<HistogramOptions>) -> Arc<dyn TelemetryHistogram + Send + Sync> {
        let inner = self.self_weak.upgrade();
        Arc::new(InMemoryInstrument::new(
            inner,
            MetricKind::Histogram,
            name,
            options.as_ref(),
        ))
    }
}

impl InMemoryTelemetryInner {
    fn stored_spans(&self) -> Vec<StoredSpan> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut spans: Vec<StoredSpan> = state
            .spans
            .iter()
            .filter(|span| span.settled)
            .map(|span| StoredSpan {
                id: carrier_id(&span.context),
                trace_id: span.context.get("pi-memory-trace").cloned().unwrap_or_default(),
                parent_id: span.parent_context.as_ref().map(carrier_id),
                name: span.name.clone(),
                started_ms: span.started_ms,
                ended_ms: span.ended_ms.unwrap_or(span.started_ms),
                attributes: copy_attributes(Some(&span.attributes)),
                events: span
                    .events
                    .iter()
                    .map(|event| StoredSpanEvent {
                        name: event.name.clone(),
                        time_ms: event.time_ms,
                        attributes: copy_attributes(Some(&event.attributes)),
                    })
                    .collect(),
                links: span
                    .links
                    .iter()
                    .map(|link| StoredSpanLink {
                        trace_id: link.context.get("pi-memory-trace").cloned().unwrap_or_default(),
                        span_id: carrier_id(&link.context),
                        attributes: Some(copy_attributes(Some(&link.attributes))),
                    })
                    .collect(),
                status: copy_status(&span.status),
            })
            .collect();
        // PORT: sort ties on `started_ms` by `(id.len(), id)` so span `scope:9` precedes `scope:10` within one microsecond.
        spans.sort_by(|a, b| {
            a.started_ms
                .total_cmp(&b.started_ms)
                .then_with(|| a.id.len().cmp(&b.id.len()))
                .then_with(|| pi_js::str16::cmp(&a.id, &b.id))
        });
        spans
    }
}

#[async_trait]
impl TelemetryStore for InMemoryTelemetryInner {
    async fn get_trace(&self, trace_id: &str) -> Result<Option<StoredTrace>> {
        let spans: Vec<StoredSpan> = self
            .stored_spans()
            .into_iter()
            .filter(|span| span.trace_id == trace_id)
            .collect();
        if spans.is_empty() {
            Ok(None)
        } else {
            Ok(Some(StoredTrace {
                trace_id: trace_id.to_string(),
                spans,
            }))
        }
    }

    async fn find_traces(&self, query: Option<TraceQuery>) -> Result<FindTracesResult> {
        let query = query.unwrap_or_default();
        let limit = query.limit.unwrap_or(20);
        let after: Option<(f64, String)> = match query.cursor.as_deref() {
            None => None,
            Some(raw) => Some(pi_js::json::parse_as(raw)?),
        };

        let mut grouped: IndexMap<String, Vec<StoredSpan>> = IndexMap::new();
        for span in self.stored_spans() {
            grouped.entry(span.trace_id.clone()).or_default().push(span);
        }

        let mut matches: Vec<StoredTrace> = grouped
            .into_iter()
            .filter_map(|(trace_id, spans)| {
                let first = spans.first()?;
                if let Some(name) = query.name.as_deref()
                    && first.name != name
                {
                    return None;
                }
                if let Some(start_min) = query.start_min
                    && first.started_ms < start_min
                {
                    return None;
                }
                if let Some(start_max) = query.start_max
                    && first.started_ms > start_max
                {
                    return None;
                }
                if let Some((after_ms, after_id)) = after.as_ref()
                    && !(first.started_ms < *after_ms
                        || (first.started_ms == *after_ms
                            && pi_js::str16::cmp(&trace_id, after_id) == std::cmp::Ordering::Greater))
                {
                    return None;
                }
                if query.errors == Some(true)
                    && !spans.iter().any(|span| matches!(span.status, SpanStatus::Error { .. }))
                {
                    return None;
                }
                if let Some(attrs) = query.attributes.as_ref()
                    && !spans.iter().any(|span| attributes_match(&span.attributes, attrs))
                {
                    return None;
                }
                Some(StoredTrace { trace_id, spans })
            })
            .collect();

        matches.sort_by(|a, b| {
            let a_start = a.spans.first().map_or(0.0, |s| s.started_ms);
            let b_start = b.spans.first().map_or(0.0, |s| s.started_ms);
            b_start
                .total_cmp(&a_start)
                .then_with(|| pi_js::str16::cmp(&a.trace_id, &b.trace_id))
        });

        let has_more = matches.len() > limit;
        let traces: Vec<StoredTrace> = matches.into_iter().take(limit).collect();
        let cursor = if has_more {
            traces.last().and_then(|last| {
                last.spans
                    .first()
                    .map(|first| pi_js::json::stringify(&(first.started_ms, &last.trace_id)))
            })
        } else {
            None
        };

        Ok(FindTracesResult { traces, cursor })
    }

    /// Read raw measurements as count-one rows, without changing get_metrics() snapshots.
    async fn find_metrics(&self, query: MetricQuery) -> Result<Vec<StoredMetric>> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let rows = state
            .measurements
            .iter()
            .filter(|entry| {
                entry.measurement.name == query.name
                    && query.start_min.is_none_or(|start_min| entry.interval_ms >= start_min)
                    && query.start_max.is_none_or(|start_max| entry.interval_ms <= start_max)
                    && query
                        .attributes
                        .as_ref()
                        .is_none_or(|attrs| attributes_match(&entry.measurement.attributes, attrs))
            })
            .map(|entry| StoredMetric {
                interval_ms: entry.interval_ms,
                name: entry.measurement.name.clone(),
                kind: entry.measurement.kind,
                unit: entry.measurement.unit.clone(),
                attributes: copy_attributes(Some(&entry.measurement.attributes)),
                count: 1,
                sum: entry.measurement.value,
                min: entry.measurement.value,
                max: entry.measurement.value,
            })
            .collect();
        Ok(rows)
    }
}

struct InMemoryInstrument {
    inner: Option<Arc<InMemoryTelemetryInner>>,
    kind: MetricKind,
    name: String,
    description: Option<String>,
    unit: Option<String>,
    boundaries: Option<Vec<f64>>,
}

impl InMemoryInstrument {
    fn new(
        inner: Option<Arc<InMemoryTelemetryInner>>,
        kind: MetricKind,
        name: &str,
        options: Option<&HistogramOptions>,
    ) -> Self {
        Self {
            inner,
            kind,
            name: name.to_string(),
            description: options.and_then(|o| o.description.clone()),
            unit: options.and_then(|o| o.unit.clone()),
            boundaries: if kind == MetricKind::Histogram {
                options.and_then(|o| o.boundaries.clone())
            } else {
                None
            },
        }
    }

    fn record_measurement(
        &self,
        value: f64,
        attributes: Option<SpanAttributes>,
        context: Option<&dyn TelemetryContext>,
    ) {
        let Some(inner) = &self.inner else {
            return;
        };
        let carrier = context.and_then(TelemetryContext::_in_memory_carrier);
        let interval_ms = (wall_now_ms() / 10_000.0).floor() as i64 * 10_000;
        let measurement = RecordedTelemetryMeasurement {
            kind: self.kind,
            name: self.name.clone(),
            description: self.description.clone(),
            unit: self.unit.clone(),
            boundaries: self.boundaries.clone(),
            value,
            attributes: copy_attributes(attributes.as_ref()),
            context: carrier,
        };
        let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
        state.measurements.push(RecordedMeasurementEntry {
            interval_ms,
            measurement,
        });
    }
}

impl TelemetryCounter for InMemoryInstrument {
    fn add(&self, value: f64, attributes: Option<SpanAttributes>) {
        self.record_measurement(value, attributes, None);
    }
}

impl TelemetryUpDownCounter for InMemoryInstrument {
    fn add(&self, value: f64, attributes: Option<SpanAttributes>) {
        self.record_measurement(value, attributes, None);
    }
}

impl TelemetryHistogram for InMemoryInstrument {
    fn record(&self, value: f64, attributes: Option<SpanAttributes>, context: Option<&dyn TelemetryContext>) {
        self.record_measurement(value, attributes, context);
    }
}

#[derive(Clone)]
struct InMemoryExtractedParent {
    inner: Arc<InMemoryTelemetryInner>,
    context: TelemetryCarrier,
    local_id: Option<i64>,
}

#[async_trait]
impl TelemetryContext for InMemoryExtractedParent {
    fn metrics(&self) -> Option<Arc<dyn TelemetryMetrics + Send + Sync>> {
        self.inner.metrics()
    }

    fn propagation(&self) -> Option<Arc<dyn TelemetryPropagation + Send + Sync>> {
        self.inner.propagation()
    }

    fn _in_memory_carrier(&self) -> Option<TelemetryCarrier> {
        Some(copy_carrier(&self.context))
    }

    async fn start_span(&self, options: SpanOptions, callback: SpanCallback) -> SpanResult {
        start_in_memory_span(
            &self.inner,
            Some(InMemoryParent {
                context: copy_carrier(&self.context),
                local_id: self.local_id,
                check_settled: false,
            }),
            options,
            callback,
        )
        .await
    }
}

#[derive(Clone)]
struct InMemoryInertSpan {
    inner: Arc<InMemoryTelemetryInner>,
}

#[async_trait]
impl TelemetryContext for InMemoryInertSpan {
    fn metrics(&self) -> Option<Arc<dyn TelemetryMetrics + Send + Sync>> {
        self.inner.metrics()
    }

    fn propagation(&self) -> Option<Arc<dyn TelemetryPropagation + Send + Sync>> {
        self.inner.propagation()
    }

    async fn start_span(&self, options: SpanOptions, callback: SpanCallback) -> SpanResult {
        start_inert_span(&self.inner, options, callback).await
    }
}

impl TelemetrySpan for InMemoryInertSpan {
    fn add_link(&self, _link: SpanLink) {}

    fn add_event(&self, _name: &str, _attributes: Option<SpanAttributes>) {}

    fn set_attributes(&self, _attributes: SpanAttributes) {}

    fn set_status(&self, _status: SpanStatus) {}
}

async fn start_inert_span(
    inner: &Arc<InMemoryTelemetryInner>,
    _options: SpanOptions,
    callback: SpanCallback,
) -> SpanResult {
    let span: Arc<dyn TelemetrySpan + Send + Sync> = Arc::new(InMemoryInertSpan {
        inner: Arc::clone(inner),
    });
    callback(span).await
}

#[derive(Clone)]
struct InMemoryTelemetrySpan {
    inner: Arc<InMemoryTelemetryInner>,
    span_id: i64,
    context: TelemetryCarrier,
}

#[async_trait]
impl TelemetryContext for InMemoryTelemetrySpan {
    fn metrics(&self) -> Option<Arc<dyn TelemetryMetrics + Send + Sync>> {
        self.inner.metrics()
    }

    fn propagation(&self) -> Option<Arc<dyn TelemetryPropagation + Send + Sync>> {
        self.inner.propagation()
    }

    fn _in_memory_carrier(&self) -> Option<TelemetryCarrier> {
        Some(copy_carrier(&self.context))
    }

    async fn start_span(&self, options: SpanOptions, callback: SpanCallback) -> SpanResult {
        start_in_memory_span(
            &self.inner,
            Some(InMemoryParent {
                context: copy_carrier(&self.context),
                local_id: Some(self.span_id),
                check_settled: true,
            }),
            options,
            callback,
        )
        .await
    }
}

impl TelemetrySpan for InMemoryTelemetrySpan {
    fn add_link(&self, link: SpanLink) {
        let Some(copied) = copy_link(&link) else {
            return;
        };
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(span) = find_span_mut(&mut state.spans, self.span_id) else {
            return;
        };
        if span.settled {
            return;
        }
        span.links.push(copied);
    }

    fn add_event(&self, name: &str, attributes: Option<SpanAttributes>) {
        let now_perf = perf_now_ms();
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(span) = find_span_mut(&mut state.spans, self.span_id) else {
            return;
        };
        if span.settled {
            return;
        }
        span.events.push(MutableRecordedTelemetryEvent {
            time_ms: microseconds(now_perf - span.clock),
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

fn is_scalar(value: &AttributeValue) -> bool {
    matches!(
        value,
        AttributeValue::String(_) | AttributeValue::Number(_) | AttributeValue::Boolean(_)
    )
}

fn attributes_match(span_attributes: &SpanAttributes, query_attributes: &SpanAttributes) -> bool {
    query_attributes
        .iter()
        .all(|(key, value)| is_scalar(value) && span_attributes.get(key) == Some(value))
}

fn copy_carrier(carrier: &TelemetryCarrier) -> TelemetryCarrier {
    carrier.clone()
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

fn copy_link(link: &SpanLink) -> Option<RecordedTelemetryLink> {
    let context = link.context._in_memory_carrier()?;
    Some(RecordedTelemetryLink {
        context,
        attributes: copy_attributes(link.attributes.as_ref()),
    })
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

fn settle_span(
    state: &mut InMemoryTelemetryState,
    span_id: i64,
    ended_perf: f64,
    failed: bool,
    error: Option<&SpanValue>,
) {
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
    span.ended_ms = Some(microseconds(span.started_ms + ended_perf - span.clock));
    span.end_sequence = Some(next_end_sequence);
    state.next_end_sequence += 1;
}

fn create_span(
    state: &mut InMemoryTelemetryState,
    parent: Option<&InMemoryParent>,
    options: &SpanOptions,
    started_ms: f64,
    clock: f64,
) -> MutableRecordedTelemetrySpan {
    let name = options.name.clone();
    let attributes = copy_attributes(options.attributes.as_ref());
    let mut links = Vec::new();
    if let Some(opt_links) = &options.links {
        for link in opt_links {
            if let Some(copy) = copy_link(link) {
                links.push(copy);
            }
        }
    }
    let id = state.next_span_id;
    state.next_span_id += 1;
    let trace_id = parent
        .and_then(|p| p.context.get("pi-memory-trace").cloned())
        .unwrap_or_else(|| format!("{}:{id}", state.scope));
    let mut context = TelemetryCarrier::new();
    context.insert("pi-memory-scope".to_string(), state.scope.clone());
    context.insert("pi-memory-span".to_string(), id.to_string());
    context.insert("pi-memory-trace".to_string(), trace_id);

    MutableRecordedTelemetrySpan {
        started_ms,
        clock,
        ended_ms: None,
        id,
        parent_id: parent.and_then(|p| p.local_id),
        context,
        parent_context: parent.map(|p| copy_carrier(&p.context)),
        links,
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
            let ended_perf = perf_now_ms();
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            settle_span(&mut state, self.span_id, ended_perf, true, None);
        }
    }
}

async fn start_in_memory_span(
    inner: &Arc<InMemoryTelemetryInner>,
    parent: Option<InMemoryParent>,
    options: SpanOptions,
    callback: SpanCallback,
) -> SpanResult {
    if let Some(parent_ref) = parent.as_ref()
        && parent_ref.check_settled
    {
        let parent_settled = {
            let state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
            parent_ref
                .local_id
                .is_none_or(|pid| find_span(&state.spans, pid).is_none_or(|p| p.settled))
        };
        if parent_settled {
            return start_inert_span(inner, options, callback).await;
        }
    }

    let started_ms = telemetry_now();
    let clock = perf_now_ms();
    let created = {
        let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(parent_ref) = parent.as_ref()
            && parent_ref.check_settled
            && parent_ref
                .local_id
                .is_none_or(|pid| find_span(&state.spans, pid).is_none_or(|p| p.settled))
        {
            None
        } else {
            let recorded_span = create_span(&mut state, parent.as_ref(), &options, started_ms, clock);
            let span_id = recorded_span.id;
            let context = copy_carrier(&recorded_span.context);
            state.spans.push(recorded_span);
            Some((span_id, context))
        }
    };
    let Some((span_id, context)) = created else {
        return start_inert_span(inner, options, callback).await;
    };

    let mut guard = SettleGuard {
        inner,
        span_id,
        disarmed: false,
    };

    let span: Arc<dyn TelemetrySpan + Send + Sync> = Arc::new(InMemoryTelemetrySpan {
        inner: Arc::clone(inner),
        span_id,
        context,
    });
    let result = callback(span).await;

    let ended_perf = perf_now_ms();
    guard.disarmed = true;
    {
        let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
        match &result {
            Ok(_) => settle_span(&mut state, span_id, ended_perf, false, None),
            Err(error) => settle_span(&mut state, span_id, ended_perf, true, Some(error)),
        }
    }

    result
}

#[doc(hidden)]
pub mod testing {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// Scoped deterministic clock override for `telemetry_now` and `wall_now_ms`.
    pub struct MockClock {
        wall: Arc<Mutex<Option<f64>>>,
        perf: Arc<Mutex<Option<f64>>>,
        previous_anchor: f64,
        _guards: Mutex<(pi_js::seam::Guard, pi_js::seam::Guard)>,
    }

    impl MockClock {
        pub fn new(wall_ms: Option<f64>, perf_ms: Option<f64>) -> Self {
            let previous_anchor = {
                let mut anchor = ANCHOR_MS.lock().unwrap_or_else(|e| e.into_inner());
                let prev = *anchor;
                *anchor = (pi_js::time::now_ms() as f64) - pi_js::time::performance_now();
                prev
            };

            let wall = Arc::new(Mutex::new(wall_ms));
            let perf = Arc::new(Mutex::new(perf_ms));

            let wall_slot = Arc::clone(&wall);
            let wall_guard = WALL_NOW_OVERRIDE.set(Arc::new(move || {
                let val = *wall_slot.lock().unwrap_or_else(|e| e.into_inner());
                val.unwrap_or_else(|| pi_js::time::now_ms() as f64)
            }));

            let perf_slot = Arc::clone(&perf);
            let perf_guard = PERF_NOW_OVERRIDE.set(Arc::new(move || {
                let val = *perf_slot.lock().unwrap_or_else(|e| e.into_inner());
                val.unwrap_or_else(pi_js::time::performance_now)
            }));

            Self {
                wall,
                perf,
                previous_anchor,
                _guards: Mutex::new((wall_guard, perf_guard)),
            }
        }

        pub fn set_wall(&self, wall_ms: f64) {
            *self.wall.lock().unwrap_or_else(|e| e.into_inner()) = Some(wall_ms);
        }

        pub fn set_perf(&self, perf_ms: f64) {
            *self.perf.lock().unwrap_or_else(|e| e.into_inner()) = Some(perf_ms);
        }
    }

    impl Drop for MockClock {
        fn drop(&mut self) {
            let mut anchor = ANCHOR_MS.lock().unwrap_or_else(|e| e.into_inner());
            *anchor = self.previous_anchor;
        }
    }
}
