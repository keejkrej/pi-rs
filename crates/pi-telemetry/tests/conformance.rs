//! Port of packages/telemetry/test/conformance.test.ts

use std::sync::Arc;

use async_trait::async_trait;
use pi_telemetry::TelemetryMetrics;
use pi_telemetry::memory::{
    InMemoryTelemetryContext, RecordedTelemetryEvent, RecordedTelemetryMeasurement, RecordedTelemetrySpan,
};
use pi_telemetry::testing::{
    TelemetryAdapterFixture, TelemetryAdapterFixtureFactory, create_telemetry_adapter_conformance,
};
use pi_telemetry::{
    AttributeValue, Result, SpanAttributes, SpanOptions, SpanValue, TelemetryContext, TelemetryPropagation,
};

struct InMemoryFixture {
    context: InMemoryTelemetryContext,
}

#[async_trait]
impl TelemetryAdapterFixture for InMemoryFixture {
    fn context(&self) -> Arc<dyn TelemetryContext + Send + Sync> {
        self.context.as_context()
    }

    fn propagation(&self) -> Option<Arc<dyn TelemetryPropagation + Send + Sync>> {
        self.context.propagation()
    }

    fn metrics(&self) -> Option<Arc<dyn TelemetryMetrics + Send + Sync>> {
        self.context.metrics()
    }

    async fn get_metrics(&self) -> Result<Vec<RecordedTelemetryMeasurement>> {
        Ok(self.context.get_metrics())
    }

    async fn get_spans(&self) -> Result<Vec<RecordedTelemetrySpan>> {
        Ok(self.context.get_spans())
    }

    async fn async_dispose(&self) -> Result<()> {
        Ok(())
    }
}

fn in_memory_fixture_factory() -> TelemetryAdapterFixtureFactory {
    Arc::new(|| {
        Box::pin(async {
            let context = InMemoryTelemetryContext::new();
            Ok(Box::new(InMemoryFixture { context }) as Box<dyn TelemetryAdapterFixture + Send + Sync>)
        })
    })
}

async fn run_conformance_case(group: &str, name: &str) {
    let cases = create_telemetry_adapter_conformance(in_memory_fixture_factory());
    let case = cases
        .into_iter()
        .find(|candidate| candidate.group == group && candidate.name == name)
        .unwrap_or_else(|| panic!("Missing conformance case: {group} :: {name}"));
    if let Err(err) = case.run().await {
        panic!("Conformance case failed ({group} :: {name}): {err}");
    }
}

mod in_memory_telemetry_context_conformance {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn registers_all_conformance_cases() {
        let cases = create_telemetry_adapter_conformance(in_memory_fixture_factory());
        assert_eq!(cases.len(), 14);
    }

    mod capabilities {
        use super::*;

        #[tokio::test]
        async fn carries_adapter_capabilities_through_contexts_and_spans() {
            run_conformance_case(
                "capabilities",
                "carries adapter capabilities through contexts and spans",
            )
            .await;
        }
    }

    mod metrics {
        use super::*;

        #[tokio::test]
        async fn records_ordered_measurements_and_optional_histogram_exemplars() {
            run_conformance_case(
                "metrics",
                "records ordered measurements and optional histogram exemplars",
            )
            .await;
        }

        #[tokio::test]
        async fn copies_instrument_metadata_and_measurement_attributes() {
            run_conformance_case("metrics", "copies instrument metadata and measurement attributes").await;
        }
    }

    mod links {
        use super::*;

        #[tokio::test]
        async fn records_initial_and_late_links_without_changing_parents() {
            run_conformance_case("links", "records initial and late links without changing parents").await;
        }

        #[tokio::test]
        async fn ignores_links_added_after_source_settlement() {
            run_conformance_case("links", "ignores links added after source settlement").await;
        }
    }

    mod propagation {
        use super::*;

        #[tokio::test]
        async fn roundtrips_parent_identity_through_json_without_retaining_the_carrier() {
            run_conformance_case(
                "propagation",
                "roundtrips parent identity through JSON without retaining the carrier",
            )
            .await;
        }

        #[tokio::test]
        async fn absent_and_unknown_carriers_produce_recorded_roots() {
            run_conformance_case("propagation", "absent and unknown carriers produce recorded roots").await;
        }

        #[tokio::test]
        async fn propagates_across_fresh_adapters_and_multiple_hops() {
            run_conformance_case("propagation", "propagates across fresh adapters and multiple hops").await;
        }
    }

    mod callback_lifecycle {
        use super::*;

        #[tokio::test]
        async fn admits_once_synchronously_and_preserves_the_result() {
            run_conformance_case(
                "callback lifecycle",
                "admits once synchronously and preserves the result",
            )
            .await;
        }

        #[tokio::test]
        async fn preserves_synchronous_and_asynchronous_rejection_values() {
            run_conformance_case(
                "callback lifecycle",
                "preserves synchronous and asynchronous rejection values",
            )
            .await;
        }
    }

    mod status {
        use super::*;

        #[tokio::test]
        async fn uses_last_explicit_status_without_automatic_overwrite() {
            run_conformance_case("status", "uses last explicit status without automatic overwrite").await;
        }
    }

    mod recording {
        use super::*;

        #[tokio::test]
        async fn merges_attributes_and_records_ordered_events() {
            run_conformance_case("recording", "merges attributes and records ordered events").await;
        }

        #[tokio::test]
        async fn makes_calls_after_settlement_inert() {
            run_conformance_case("recording", "makes calls after settlement inert").await;
        }
    }

    mod parentage {
        use super::*;

        #[tokio::test]
        async fn records_nested_and_concurrent_child_relationships() {
            run_conformance_case("parentage", "records nested and concurrent child relationships").await;
        }
    }

    #[tokio::test]
    async fn returns_detached_snapshots_without_exposing_mutable_recording_state() {
        let context = InMemoryTelemetryContext::new();
        let context_in_cb = context.clone();
        let open_snapshot = Arc::new(std::sync::Mutex::new(None::<(bool, Option<i64>)>));
        let open_snapshot_cb = Arc::clone(&open_snapshot);

        let mut start_attrs = SpanAttributes::new();
        start_attrs.insert(
            "tags".to_string(),
            AttributeValue::StringArray(vec!["initial".to_string()]),
        );

        context
            .start_span(
                SpanOptions {
                    name: "snapshot".to_string(),
                    attributes: Some(start_attrs),
                    links: None,
                },
                Arc::new(move |span| {
                    let context_in_cb = context_in_cb.clone();
                    let open_snapshot_cb = Arc::clone(&open_snapshot_cb);
                    Box::pin(async move {
                        let mut event_attrs = SpanAttributes::new();
                        event_attrs.insert("value".to_string(), AttributeValue::Number(1.0));
                        span.add_event("event", Some(event_attrs));
                        let spans = context_in_cb.get_spans();
                        let open = spans.first().expect("expected open span");
                        *open_snapshot_cb.lock().unwrap() = Some((open.settled, open.end_sequence));
                        Ok(Box::new(()) as SpanValue)
                    })
                }),
            )
            .await
            .expect("snapshot span should resolve");

        assert_eq!(*open_snapshot.lock().unwrap(), Some((false, None)));

        let mut first_spans = context.get_spans();
        let first = first_spans.first_mut().expect("expected settled span");
        assert_eq!(first.settled, true);
        assert_eq!(first.end_sequence, Some(1));
        assert_eq!(first.parent_id, None);
        first.attributes.insert(
            "tags".to_string(),
            AttributeValue::StringArray(vec!["mutated".to_string()]),
        );
        first.events[0]
            .attributes
            .insert("value".to_string(), AttributeValue::Number(2.0));

        let second_spans = context.get_spans();
        let second = second_spans.first().expect("expected second snapshot");
        let mut expected_attrs = SpanAttributes::new();
        expected_attrs.insert(
            "tags".to_string(),
            AttributeValue::StringArray(vec!["initial".to_string()]),
        );
        let mut expected_event_attrs = SpanAttributes::new();
        expected_event_attrs.insert("value".to_string(), AttributeValue::Number(1.0));
        assert_eq!(second.attributes, expected_attrs);
        assert_eq!(
            second.events,
            vec![RecordedTelemetryEvent {
                name: "event".to_string(),
                attributes: expected_event_attrs,
            }]
        );
    }
}
