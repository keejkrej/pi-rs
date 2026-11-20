//! Port of packages/telemetry/test/metrics.test.ts

use std::sync::Arc;

use indexmap::IndexMap;
use pi_telemetry::{
    AttributeValue, HistogramOptions, InMemoryTelemetryContext, InstrumentOptions, MetricKind, NOOP_TELEMETRY_CONTEXT,
    NOOP_TELEMETRY_METRICS, SpanAttributes, SpanOptions, SpanValue, TelemetryAttributeCardinality, TelemetryContext,
    TelemetryMetricAttributeDefinition, TelemetryMetricDefinition, TelemetryMetrics, TelemetrySchemaDefinition,
    create_typed_metrics, define_telemetry_schema,
};
use pretty_assertions::assert_eq;

fn traffic_schema() -> TelemetrySchemaDefinition {
    let mut bytes_attrs = IndexMap::new();
    bytes_attrs.insert(
        "direction".to_string(),
        TelemetryMetricAttributeDefinition::String {
            required: true,
            values: Some(vec!["in".to_string(), "out".to_string()]),
            examples: None,
            description: "Direction".to_string(),
            sensitive: None,
            cardinality: Some(TelemetryAttributeCardinality::Low),
        },
    );

    let mut metrics = IndexMap::new();
    metrics.insert(
        "network.bytes".to_string(),
        TelemetryMetricDefinition::Counter {
            description: "Encoded bytes".to_string(),
            unit: Some("By".to_string()),
            attributes: bytes_attrs,
        },
    );
    metrics.insert(
        "network.active".to_string(),
        TelemetryMetricDefinition::UpDownCounter {
            description: "Open links".to_string(),
            unit: None,
            attributes: IndexMap::new(),
        },
    );

    define_telemetry_schema(TelemetrySchemaDefinition {
        version: 1,
        spans: IndexMap::new(),
        metrics: Some(metrics),
    })
}

fn latency_schema() -> TelemetrySchemaDefinition {
    let mut duration_attrs = IndexMap::new();
    duration_attrs.insert(
        "method".to_string(),
        TelemetryMetricAttributeDefinition::String {
            required: false,
            values: None,
            examples: None,
            description: "Method".to_string(),
            sensitive: None,
            cardinality: None,
        },
    );

    let mut metrics = IndexMap::new();
    metrics.insert(
        "call.duration".to_string(),
        TelemetryMetricDefinition::Histogram {
            description: "Call duration".to_string(),
            unit: Some("ms".to_string()),
            boundaries: Some(vec![1.0, 10.0, 100.0]),
            attributes: duration_attrs,
        },
    );

    define_telemetry_schema(TelemetrySchemaDefinition {
        version: 1,
        spans: IndexMap::new(),
        metrics: Some(metrics),
    })
}

#[tokio::test]
async fn binds_schema_names_kinds_metadata_and_exact_measurement_attributes() {
    let traffic = traffic_schema();
    let latency = latency_schema();
    let adapter = InMemoryTelemetryContext::new();
    let metrics = create_typed_metrics(adapter.clone(), &[traffic.clone(), latency]);

    let mut out_attrs = SpanAttributes::new();
    out_attrs.insert("direction".to_string(), AttributeValue::String("out".to_string()));
    metrics.counter("network.bytes").add(128.0, Some(out_attrs.clone()));
    metrics.up_down_counter("network.active").add(1.0, None);
    metrics.up_down_counter("network.active").add(-1.0, None);

    let metrics_cb = metrics.clone();
    adapter
        .start_span(
            SpanOptions::new("request"),
            Arc::new(move |span| {
                let metrics = metrics_cb.clone();
                Box::pin(async move {
                    let mut method_attrs = SpanAttributes::new();
                    method_attrs.insert("method".to_string(), AttributeValue::String("read".to_string()));
                    metrics
                        .histogram("call.duration")
                        .record(12.0, Some(method_attrs), Some(&span));
                    metrics.histogram("call.duration").record(13.0, None, None);
                    metrics.histogram("call.duration").record(14.0, None, Some(&span));
                    Ok(Box::new(()) as SpanValue)
                })
            }),
        )
        .await
        .expect("request span should resolve");

    let measurements = adapter.get_metrics();
    assert_eq!(measurements.len(), 6);
    assert_eq!(measurements[0].kind, MetricKind::Counter);
    assert_eq!(measurements[0].name, "network.bytes");
    assert_eq!(measurements[0].description.as_deref(), Some("Encoded bytes"));
    assert_eq!(measurements[0].unit.as_deref(), Some("By"));
    assert_eq!(measurements[0].value, 128.0);
    assert_eq!(measurements[0].attributes, out_attrs);

    let span_context = adapter.get_spans()[0].context.clone();
    assert_eq!(measurements[3].kind, MetricKind::Histogram);
    assert_eq!(measurements[3].boundaries, Some(vec![1.0, 10.0, 100.0]));
    assert_eq!(measurements[3].context.as_ref(), Some(&span_context));
    assert_eq!(measurements[4].context, None);
    assert_eq!(measurements[5].context.as_ref(), Some(&span_context));

    let serialized = pi_js::json::stringify(&traffic);
    let parsed: TelemetrySchemaDefinition = pi_js::json::parse_as(&serialized).expect("schema should round-trip");
    assert_eq!(parsed, traffic);
}

#[test]
fn requires_histogram_attributes_when_the_schema_declares_them() {
    let mut attrs = IndexMap::new();
    attrs.insert(
        "cache".to_string(),
        TelemetryMetricAttributeDefinition::Boolean {
            required: true,
            values: None,
            examples: None,
            description: "Cache hit".to_string(),
            sensitive: None,
            cardinality: None,
        },
    );
    let mut schema_metrics = IndexMap::new();
    schema_metrics.insert(
        "required".to_string(),
        TelemetryMetricDefinition::Histogram {
            description: "Required tag".to_string(),
            unit: None,
            boundaries: None,
            attributes: attrs,
        },
    );
    let schema = define_telemetry_schema(TelemetrySchemaDefinition {
        version: 1,
        spans: IndexMap::new(),
        metrics: Some(schema_metrics),
    });

    let adapter = InMemoryTelemetryContext::new();
    let metrics = create_typed_metrics(adapter.clone(), &[schema]);
    let mut rec_attrs = SpanAttributes::new();
    rec_attrs.insert("cache".to_string(), AttributeValue::Boolean(true));
    metrics.histogram("required").record(1.0, Some(rec_attrs.clone()), None);
    let measurements = adapter.get_metrics();
    assert_eq!(measurements.len(), 1);
    assert_eq!(measurements[0].attributes, rec_attrs);
}

#[tokio::test]
async fn returns_detached_measurement_snapshots_including_exemplars_and_boundaries() {
    let telemetry = InMemoryTelemetryContext::new();
    let histogram = telemetry.histogram(
        "duration",
        Some(HistogramOptions {
            description: None,
            unit: None,
            boundaries: Some(vec![1.0, 2.0]),
        }),
    );

    let hist_cb = Arc::clone(&histogram);
    telemetry
        .start_span(
            SpanOptions::new("request"),
            Arc::new(move |span| {
                let hist = Arc::clone(&hist_cb);
                Box::pin(async move {
                    let mut attrs = SpanAttributes::new();
                    attrs.insert(
                        "tags".to_string(),
                        AttributeValue::StringArray(vec!["original".to_string()]),
                    );
                    hist.record(1.0, Some(attrs), Some(&span));
                    Ok(Box::new(()) as SpanValue)
                })
            }),
        )
        .await
        .expect("request span should resolve");

    let mut measurements = telemetry.get_metrics();
    let measurement = measurements.first_mut().expect("expected measurement");
    measurement.boundaries.as_mut().expect("boundaries").push(3.0);
    measurement.attributes.insert(
        "tags".to_string(),
        AttributeValue::StringArray(vec!["mutated".to_string()]),
    );
    measurement
        .context
        .as_mut()
        .expect("context")
        .insert("pi-memory-span".to_string(), "999".to_string());

    let fresh = telemetry.get_metrics();
    assert_eq!(fresh[0].boundaries, Some(vec![1.0, 2.0]));
    let mut expected_attrs = SpanAttributes::new();
    expected_attrs.insert(
        "tags".to_string(),
        AttributeValue::StringArray(vec!["original".to_string()]),
    );
    assert_eq!(fresh[0].attributes, expected_attrs);
    assert_eq!(fresh[0].context, Some(telemetry.get_spans()[0].context.clone()));
}

#[test]
fn shares_frozen_noop_instruments() {
    let options = InstrumentOptions {
        description: None,
        unit: Some("By".to_string()),
    };
    let hist_options = HistogramOptions::from(&options);
    let mut attributes = SpanAttributes::new();
    attributes.insert("direction".to_string(), AttributeValue::String("out".to_string()));

    let counter = NOOP_TELEMETRY_METRICS.counter("bytes", Some(options.clone()));
    let active = NOOP_TELEMETRY_METRICS.up_down_counter("active", Some(options));
    let histogram = NOOP_TELEMETRY_METRICS.histogram("duration", Some(hist_options));

    assert!(Arc::ptr_eq(&counter, &NOOP_TELEMETRY_METRICS.counter("other", None)));
    assert!(Arc::ptr_eq(
        &active,
        &NOOP_TELEMETRY_METRICS.up_down_counter("other", None),
    ));
    assert!(Arc::ptr_eq(
        &histogram,
        &NOOP_TELEMETRY_METRICS.histogram("other", None)
    ));

    counter.add(1.0, Some(attributes.clone()));
    active.add(-1.0, Some(attributes.clone()));
    histogram.record(1.0, Some(attributes), Some(&NOOP_TELEMETRY_CONTEXT));
}
