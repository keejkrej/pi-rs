//! Port of packages/telemetry/test/store.test.ts

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_telemetry::memory::testing::MockClock;
use pi_telemetry::{
    AttributeValue, InMemoryTelemetryContext, MetricQuery, RecordedTelemetryEvent, Result, SpanAttributes, SpanLink,
    SpanOptions, SpanStatus, SpanStatusError, SpanValue, StoredSpan, StoredSpanEvent, StoredTrace, TelemetryContext,
    TelemetryMetrics, TelemetryPropagation, TelemetrySpan, TelemetryStore, TraceQuery, telemetry_now,
};
use serial_test::serial;

#[path = "support/mod.rs"]
mod support;
use support::store_conformance::{self, StoreFixture, StoreFixtureFactory};

struct InMemoryStoreFixture {
    context: InMemoryTelemetryContext,
}

#[async_trait]
impl StoreFixture for InMemoryStoreFixture {
    fn context(&self) -> Arc<dyn TelemetryContext + Send + Sync> {
        self.context.as_context()
    }

    fn metrics(&self) -> Arc<dyn TelemetryMetrics + Send + Sync> {
        self.context.metrics().expect("in-memory metrics")
    }

    fn propagation(&self) -> Arc<dyn TelemetryPropagation + Send + Sync> {
        self.context.propagation().expect("in-memory propagation")
    }

    fn store(&self) -> Arc<dyn TelemetryStore + Send + Sync> {
        Arc::new(self.context.clone())
    }

    async fn flush(&self) -> Result<()> {
        Ok(())
    }

    async fn async_dispose(&self) -> Result<()> {
        Ok(())
    }
}

fn in_memory_store_factory() -> StoreFixtureFactory {
    Arc::new(|| {
        Box::pin(async {
            Ok(Box::new(InMemoryStoreFixture {
                context: InMemoryTelemetryContext::new(),
            }) as Box<dyn StoreFixture>)
        })
    })
}

mod in_memory_telemetry_store_conformance {
    use super::*;

    #[tokio::test]
    #[serial]
    async fn returns_only_settled_traces_complete_span_snapshots_and_detached_results() {
        store_conformance::returns_only_settled_traces_complete_span_snapshots_and_detached_results(
            &in_memory_store_factory(),
        )
        .await;
    }

    #[tokio::test]
    #[serial]
    async fn filters_by_first_span_name_and_start_any_error_and_all_attributes_on_one_span() {
        store_conformance::filters_by_first_span_name_and_start_any_error_and_all_attributes_on_one_span(
            &in_memory_store_factory(),
        )
        .await;
    }

    #[tokio::test]
    #[serial]
    async fn pages_complete_traces_without_duplicates_when_trace_starts_tie() {
        store_conformance::pages_complete_traces_without_duplicates_when_trace_starts_tie(&in_memory_store_factory())
            .await;
    }

    #[tokio::test]
    #[serial]
    async fn inherits_remote_trace_ids_and_preserves_remote_parents_and_cross_trace_links() {
        store_conformance::inherits_remote_trace_ids_and_preserves_remote_parents_and_cross_trace_links(
            &in_memory_store_factory(),
        )
        .await;
    }

    #[tokio::test]
    #[serial]
    async fn accepts_external_string_identities_without_imposing_an_id_format() {
        store_conformance::accepts_external_string_identities_without_imposing_an_id_format(&in_memory_store_factory())
            .await;
    }

    #[tokio::test]
    #[serial]
    async fn queries_metric_names_inclusive_interval_bounds_typed_attribute_equality_and_detached_values() {
        store_conformance::queries_metric_names_inclusive_interval_bounds_typed_attribute_equality_and_detached_values(
            &in_memory_store_factory(),
        )
        .await;
    }
}

mod in_memory_store_snapshots {
    use pretty_assertions::assert_eq;

    use super::*;

    #[tokio::test]
    #[serial]
    async fn records_wall_clock_boundaries_and_relative_event_times_without_changing_normalized_event_snapshots() {
        let clock = Arc::new(MockClock::new(Some(1000.0), Some(100.0)));
        let telemetry = InMemoryTelemetryContext::new();
        let id_holder: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let id_cb = Arc::clone(&id_holder);
        let clock_cb = Arc::clone(&clock);
        let telemetry_cb = telemetry.clone();

        telemetry
            .start_span(
                SpanOptions::new("timed"),
                Arc::new(move |span| {
                    let id_cb = Arc::clone(&id_cb);
                    let clock_cb = Arc::clone(&clock_cb);
                    let telemetry_cb = telemetry_cb.clone();
                    Box::pin(async move {
                        let carrier = telemetry_cb.inject(&span).expect("carrier");
                        *id_cb.lock().unwrap() =
                            format!("{}:{}", carrier["pi-memory-scope"], carrier["pi-memory-span"]);
                        clock_cb.set_perf(110.0);
                        let mut event_attrs = SpanAttributes::new();
                        event_attrs.insert("count".to_string(), AttributeValue::Number(1.0));
                        span.add_event("event", Some(event_attrs));
                        clock_cb.set_perf(125.0);
                        Ok(Box::new(()) as SpanValue)
                    })
                }),
            )
            .await
            .expect("timed span");

        let id = id_holder.lock().unwrap().clone();
        let mut expected_event_attrs = SpanAttributes::new();
        expected_event_attrs.insert("count".to_string(), AttributeValue::Number(1.0));
        assert_eq!(
            telemetry.get_trace(&id).await.expect("get_trace"),
            Some(StoredTrace {
                trace_id: id.clone(),
                spans: vec![StoredSpan {
                    id: id.clone(),
                    trace_id: id,
                    parent_id: None,
                    name: "timed".to_string(),
                    started_ms: 1000.0,
                    ended_ms: 1025.0,
                    attributes: SpanAttributes::new(),
                    events: vec![StoredSpanEvent {
                        name: "event".to_string(),
                        time_ms: 10.0,
                        attributes: expected_event_attrs.clone(),
                    }],
                    links: Vec::new(),
                    status: SpanStatus::Ok,
                }],
            })
        );
        assert_eq!(
            telemetry.get_spans()[0].events,
            vec![RecordedTelemetryEvent {
                name: "event".to_string(),
                attributes: expected_event_attrs,
            }]
        );
    }

    #[tokio::test]
    #[serial]
    async fn stamps_starts_with_telemetry_now_spans_of_one_millisecond_apart_to_the_microsecond_in_their_order() {
        let clock = Arc::new(MockClock::new(Some(1000.0), Some(100.0)));
        let telemetry = InMemoryTelemetryContext::new();

        let clock1 = Arc::clone(&clock);
        telemetry
            .start_span(
                SpanOptions::new("first"),
                Arc::new(move |_span| {
                    let clock1 = Arc::clone(&clock1);
                    Box::pin(async move {
                        clock1.set_perf(100.25);
                        Ok(Box::new(()) as SpanValue)
                    })
                }),
            )
            .await
            .expect("first");

        let clock2 = Arc::clone(&clock);
        telemetry
            .start_span(
                SpanOptions::new("second"),
                Arc::new(move |_span| {
                    let clock2 = Arc::clone(&clock2);
                    Box::pin(async move {
                        clock2.set_perf(100.5);
                        Ok(Box::new(()) as SpanValue)
                    })
                }),
            )
            .await
            .expect("second");

        let traces = telemetry.find_traces(None).await.expect("find_traces").traces;
        let summary: Vec<(String, f64, f64)> = traces
            .into_iter()
            .flat_map(|t| t.spans)
            .map(|s| (s.name, s.started_ms, s.ended_ms))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("second".to_string(), 1000.25, 1000.5),
                ("first".to_string(), 1000.0, 1000.25),
            ]
        );
    }

    #[tokio::test]
    #[serial]
    async fn maps_raw_metric_measurements_to_count_one_rows_without_exposing_private_timing_in_get_metrics() {
        let _clock = MockClock::new(Some(20_001.0), None);
        let telemetry = InMemoryTelemetryContext::new();
        telemetry.counter("calls", None).add(2.0, None);
        telemetry.counter("calls", None).add(3.0, None);

        let rows: Vec<(i64, i64, f64)> = telemetry
            .find_metrics(MetricQuery {
                name: "calls".to_string(),
                ..Default::default()
            })
            .await
            .expect("find_metrics")
            .into_iter()
            .map(|row| (row.interval_ms, row.count, row.sum))
            .collect();
        assert_eq!(rows, vec![(20_000, 1, 2.0), (20_000, 1, 3.0)]);

        let metrics = telemetry.get_metrics();
        let raw: Vec<(&str, &str, f64)> = metrics
            .iter()
            .map(|m| {
                (
                    if m.kind == pi_telemetry::MetricKind::Counter {
                        "counter"
                    } else {
                        "other"
                    },
                    m.name.as_str(),
                    m.value,
                )
            })
            .collect();
        assert_eq!(raw, vec![("counter", "calls", 2.0), ("counter", "calls", 3.0)]);
    }

    #[tokio::test]
    async fn detaches_link_attributes_and_error_details() {
        let telemetry = InMemoryTelemetryContext::new();
        let target_holder: Arc<Mutex<Option<Arc<dyn TelemetrySpan + Send + Sync>>>> = Arc::new(Mutex::new(None));
        let target_cb = Arc::clone(&target_holder);
        telemetry
            .start_span(
                SpanOptions::new("target"),
                Arc::new(move |span| {
                    let target_cb = Arc::clone(&target_cb);
                    Box::pin(async move {
                        *target_cb.lock().unwrap() = Some(span);
                        Ok(Box::new(()) as SpanValue)
                    })
                }),
            )
            .await
            .expect("target span");
        let target = target_holder.lock().unwrap().clone().expect("target");

        let mut tags = SpanAttributes::new();
        tags.insert(
            "tags".to_string(),
            AttributeValue::StringArray(vec!["original".to_string()]),
        );
        telemetry
            .start_span(
                SpanOptions {
                    name: "source".to_string(),
                    attributes: None,
                    links: Some(vec![SpanLink {
                        context: target as Arc<dyn TelemetryContext + Send + Sync>,
                        attributes: Some(tags),
                    }]),
                },
                Arc::new(|span| {
                    Box::pin(async move {
                        span.set_status(SpanStatus::Error {
                            error: Some(SpanStatusError {
                                name: "Expected".to_string(),
                                message: "original".to_string(),
                            }),
                        });
                        Ok(Box::new(()) as SpanValue)
                    })
                }),
            )
            .await
            .expect("source span");

        let mut trace = telemetry
            .find_traces(Some(TraceQuery {
                name: Some("source".to_string()),
                ..Default::default()
            }))
            .await
            .expect("find_traces")
            .traces[0]
            .clone();
        trace.spans[0].links[0].attributes.as_mut().expect("link attrs").insert(
            "tags".to_string(),
            AttributeValue::StringArray(vec!["changed".to_string()]),
        );
        if let SpanStatus::Error {
            error: Some(ref mut err),
        } = trace.spans[0].status
        {
            err.message = "changed".to_string();
        }

        let fresh = telemetry
            .get_trace(&trace.trace_id)
            .await
            .expect("get_trace")
            .expect("fresh trace");
        assert_eq!(
            fresh.spans[0].links[0].attributes.as_ref().and_then(|a| a.get("tags")),
            Some(&AttributeValue::StringArray(vec!["original".to_string()]))
        );
        assert_eq!(
            fresh.spans[0].status,
            SpanStatus::Error {
                error: Some(SpanStatusError {
                    name: "Expected".to_string(),
                    message: "original".to_string(),
                }),
            }
        );
    }
}

mod telemetry_now_tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    #[serial]
    fn counts_sub_millisecond_steps_of_the_monotonic_clock_while_the_wall_clock_stays_in_its_millisecond() {
        let clock = MockClock::new(Some(50_000.0), Some(1000.25));
        let first = telemetry_now();
        clock.set_perf(1000.2504);
        assert!((telemetry_now() - first).abs() < 1e-6);
        clock.set_perf(1000.7512);
        assert!(((telemetry_now() - first) - 0.501).abs() < 1e-6);
        let scaled = telemetry_now() * 1000.0;
        assert_eq!(scaled.fract(), 0.0);
    }

    #[test]
    #[serial]
    fn follows_a_step_of_the_wall_clock_past_10_ms() {
        let clock = MockClock::new(Some(80_000.0), Some(1000.0));
        assert_eq!(telemetry_now(), 80_000.0);
        clock.set_perf(1003.5);
        clock.set_wall(80_005.0);
        // Within 10 ms the anchored clock keeps its own spacing.
        assert_eq!(telemetry_now(), 80_003.5);
        clock.set_perf(1004.0);
        clock.set_wall(3_650_004.0);
        assert_eq!(telemetry_now(), 3_650_004.0);
        clock.set_perf(1004.125);
        assert_eq!(telemetry_now(), 3_650_004.125);
    }
}
