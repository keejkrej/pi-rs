//! Port of packages/telemetry/src/testing/conformance.ts

use std::fmt::Debug;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;

use crate::memory::{
    RecordedTelemetryEvent, RecordedTelemetryLink, RecordedTelemetryMeasurement, RecordedTelemetrySpan,
};
use crate::metrics::{HistogramOptions, InstrumentOptions, TelemetryMetrics};
use crate::testing::types::{TelemetryAdapterConformanceCase, TelemetryAdapterFixture, TelemetryAdapterFixtureFactory};
use crate::{
    AttributeValue, Error, MetricKind, NOOP_TELEMETRY_CONTEXT, Result, SpanAttributes, SpanLink, SpanOptions,
    SpanResult, SpanStatus, SpanStatusError, SpanValue, TelemetryCarrier, TelemetryContext, TelemetryPropagation,
    TelemetrySpan,
};

type ConformanceTest =
    Arc<dyn Fn(Arc<dyn TelemetryAdapterFixture + Send + Sync>) -> pi_js::BoxFuture<Result<()>> + Send + Sync>;

fn create_case(
    factory: &TelemetryAdapterFixtureFactory,
    group: &str,
    name: &str,
    test: ConformanceTest,
) -> TelemetryAdapterConformanceCase {
    let factory = Arc::clone(factory);
    TelemetryAdapterConformanceCase::new(
        group,
        name,
        Arc::new(move || {
            let factory = Arc::clone(&factory);
            let test = Arc::clone(&test);
            Box::pin(async move {
                let fixture: Arc<dyn TelemetryAdapterFixture + Send + Sync> = Arc::from(factory().await?);
                let result = test(Arc::clone(&fixture)).await;
                let dispose_result = fixture.async_dispose().await;
                result?;
                dispose_result?;
                Ok(())
            })
        }),
    )
}

fn find_span<'a>(spans: &'a [RecordedTelemetrySpan], name: &str) -> Result<&'a RecordedTelemetrySpan> {
    spans
        .iter()
        .find(|candidate| candidate.name == name)
        .ok_or_else(|| Error::msg(format!("Expected recorded span {name}")))
}

fn assert_true(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition { Ok(()) } else { Err(Error::msg(message)) }
}

fn assert_eq_val<T: PartialEq + Debug>(actual: &T, expected: &T, label: &str) -> Result<()> {
    if actual == expected {
        Ok(())
    } else {
        Err(Error::msg(format!("{label}: expected {expected:?}, got {actual:?}")))
    }
}

fn assert_opt_arc_eq<T: ?Sized>(actual: &Option<Arc<T>>, expected: &Option<Arc<T>>, label: &str) -> Result<()> {
    match (actual, expected) {
        (None, None) => Ok(()),
        (Some(a), Some(b)) if Arc::ptr_eq(a, b) => Ok(()),
        _ => Err(Error::msg(format!("{label}: capability instance mismatch"))),
    }
}

async fn rejects_with_js_error(
    operation: impl Future<Output = SpanResult>,
    expected_name: &str,
    expected_message: &str,
) -> Result<()> {
    match operation.await {
        Ok(_) => Err(Error::msg("Expected operation to reject")),
        Err(err) => {
            let js_err = err
                .downcast_ref::<Error>()
                .ok_or_else(|| Error::msg("Expected rejection to contain pi_js::Error"))?;
            assert_eq_val(&js_err.name(), &expected_name, "error.name")?;
            assert_eq_val(&js_err.message(), &expected_message.to_string(), "error.message")
        }
    }
}

async fn rejects_with_value<T: PartialEq + Debug + 'static>(
    operation: impl Future<Output = SpanResult>,
    expected: &T,
    label: &str,
) -> Result<()> {
    match operation.await {
        Ok(_) => Err(Error::msg("Expected operation to reject")),
        Err(err) => {
            let actual = err
                .downcast_ref::<T>()
                .ok_or_else(|| Error::msg(format!("{label}: rejection value had unexpected type")))?;
            assert_eq_val(actual, expected, label)
        }
    }
}

fn span_err_from_span_value(value: SpanValue, fallback: &str) -> Error {
    match value.downcast::<Error>() {
        Ok(err) => *err,
        Err(_) => Error::msg(fallback),
    }
}

fn assert_capabilities(
    context: &dyn TelemetryContext,
    expected_metrics: &Option<Arc<dyn TelemetryMetrics + Send + Sync>>,
    expected_propagation: &Option<Arc<dyn TelemetryPropagation + Send + Sync>>,
    label: &str,
) -> Result<()> {
    assert_opt_arc_eq(&context.metrics(), expected_metrics, &format!("{label}.metrics"))?;
    assert_opt_arc_eq(
        &context.propagation(),
        expected_propagation,
        &format!("{label}.propagation"),
    )
}

#[derive(Debug, PartialEq)]
struct ExpectedObject {
    value: i64,
}

#[derive(Debug, PartialEq)]
struct PlainError {
    kind: &'static str,
}

/// Creates runner-independent cases for the callback telemetry adapter contract.
pub fn create_telemetry_adapter_conformance(
    factory: TelemetryAdapterFixtureFactory,
) -> Vec<TelemetryAdapterConformanceCase> {
    vec![
        create_case(
            &factory,
            "capabilities",
            "carries adapter capabilities through contexts and spans",
            Arc::new(|fixture| {
                Box::pin(async move {
                    let root_ctx = fixture.context();
                    let metrics = fixture.metrics().or_else(|| root_ctx.metrics());
                    let propagation = fixture.propagation().or_else(|| root_ctx.propagation());
                    assert_capabilities(root_ctx.as_ref(), &metrics, &propagation, "root")?;

                    let settled: Arc<Mutex<Option<Arc<dyn TelemetrySpan + Send + Sync>>>> = Arc::new(Mutex::new(None));
                    let settled_cb = Arc::clone(&settled);
                    let metrics_cb = metrics.clone();
                    let propagation_cb = propagation.clone();

                    root_ctx
                        .start_span(
                            SpanOptions::new("capabilities"),
                            Arc::new(move |span| {
                                let settled_cb = Arc::clone(&settled_cb);
                                let metrics_cb = metrics_cb.clone();
                                let propagation_cb = propagation_cb.clone();
                                Box::pin(async move {
                                    *settled_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&span));
                                    assert_capabilities(span.as_ref(), &metrics_cb, &propagation_cb, "span")
                                        .map_err(|e| Box::new(e) as SpanValue)?;

                                    let metrics_child = metrics_cb.clone();
                                    let propagation_child = propagation_cb.clone();
                                    span.start_span(
                                        SpanOptions::new("capabilities.child"),
                                        Arc::new(move |child| {
                                            let metrics_child = metrics_child.clone();
                                            let propagation_child = propagation_child.clone();
                                            Box::pin(async move {
                                                assert_capabilities(
                                                    child.as_ref(),
                                                    &metrics_child,
                                                    &propagation_child,
                                                    "child",
                                                )
                                                .map_err(|e| Box::new(e) as SpanValue)?;
                                                Ok(Box::new(()) as SpanValue)
                                            })
                                        }),
                                    )
                                    .await?;

                                    if let Some(prop) = propagation_cb.as_ref() {
                                        let carrier = prop.inject(span.as_ref());
                                        let parent = prop.extract(carrier.as_ref());
                                        assert_capabilities(parent.as_ref(), &metrics_cb, &propagation_cb, "parent")
                                            .map_err(|e| Box::new(e) as SpanValue)?;

                                        let metrics_ext = metrics_cb.clone();
                                        let propagation_ext = propagation_cb.clone();
                                        parent
                                            .start_span(
                                                SpanOptions::new("capabilities.extracted"),
                                                Arc::new(move |ext| {
                                                    let metrics_ext = metrics_ext.clone();
                                                    let propagation_ext = propagation_ext.clone();
                                                    Box::pin(async move {
                                                        assert_capabilities(
                                                            ext.as_ref(),
                                                            &metrics_ext,
                                                            &propagation_ext,
                                                            "extracted",
                                                        )
                                                        .map_err(|e| Box::new(e) as SpanValue)?;
                                                        Ok(Box::new(()) as SpanValue)
                                                    })
                                                }),
                                            )
                                            .await?;
                                    }
                                    Ok(Box::new(()) as SpanValue)
                                })
                            }),
                        )
                        .await
                        .map_err(|err| span_err_from_span_value(err, "Expected capabilities span to resolve"))?;

                    let settled_span = settled
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone()
                        .ok_or_else(|| Error::msg("Expected settled span"))?;

                    let metrics_inert = metrics.clone();
                    let propagation_inert = propagation.clone();
                    settled_span
                        .start_span(
                            SpanOptions::new("capabilities.inert"),
                            Arc::new(move |inert| {
                                let metrics_inert = metrics_inert.clone();
                                let propagation_inert = propagation_inert.clone();
                                Box::pin(async move {
                                    assert_capabilities(inert.as_ref(), &metrics_inert, &propagation_inert, "inert")
                                        .map_err(|e| Box::new(e) as SpanValue)?;
                                    Ok(Box::new(()) as SpanValue)
                                })
                            }),
                        )
                        .await
                        .map_err(|err| span_err_from_span_value(err, "Expected capabilities.inert span to resolve"))?;
                    Ok(())
                })
            }),
        ),
        create_case(
            &factory,
            "metrics",
            "records ordered measurements and optional histogram exemplars",
            Arc::new(|fixture| {
                Box::pin(async move {
                    let Some(metrics) = fixture.metrics() else {
                        return Ok(());
                    };
                    let mut bytes_attrs = SpanAttributes::new();
                    bytes_attrs.insert("direction".to_string(), AttributeValue::String("out".to_string()));
                    metrics
                        .counter(
                            "bytes",
                            Some(InstrumentOptions {
                                description: Some("Encoded bytes".to_string()),
                                unit: Some("By".to_string()),
                            }),
                        )
                        .add(12.0, Some(bytes_attrs.clone()));

                    let active = metrics.up_down_counter("active", None);
                    active.add(1.0, None);
                    active.add(-1.0, None);

                    let histogram = metrics.histogram(
                        "duration",
                        Some(HistogramOptions {
                            description: None,
                            unit: Some("ms".to_string()),
                            boundaries: Some(vec![1.0, 10.0]),
                        }),
                    );

                    fixture
                        .context()
                        .start_span(
                            SpanOptions::new("measured"),
                            Arc::new(move |span| {
                                let histogram = Arc::clone(&histogram);
                                Box::pin(async move {
                                    let mut attrs = SpanAttributes::new();
                                    attrs.insert("method".to_string(), AttributeValue::String("read".to_string()));
                                    histogram.record(5.0, Some(attrs), Some(&span));
                                    Ok(Box::new(()) as SpanValue)
                                })
                            }),
                        )
                        .await
                        .map_err(|err| span_err_from_span_value(err, "Expected measured span to resolve"))?;

                    let measurements: Vec<RecordedTelemetryMeasurement> = fixture.get_metrics().await?;
                    let summary: Vec<(MetricKind, &str, f64)> = measurements
                        .iter()
                        .map(|m| (m.kind, m.name.as_str(), m.value))
                        .collect();
                    assert_eq_val(
                        &summary,
                        &vec![
                            (MetricKind::Counter, "bytes", 12.0),
                            (MetricKind::UpDownCounter, "active", 1.0),
                            (MetricKind::UpDownCounter, "active", -1.0),
                            (MetricKind::Histogram, "duration", 5.0),
                        ],
                        "measurements",
                    )?;
                    assert_eq_val(&measurements[0].attributes, &bytes_attrs, "measurements[0].attributes")?;
                    assert_eq_val(
                        &measurements[0].description,
                        &Some("Encoded bytes".to_string()),
                        "measurements[0].description",
                    )?;
                    assert_eq_val(&measurements[0].unit, &Some("By".to_string()), "measurements[0].unit")?;
                    assert_eq_val(
                        &measurements[3].boundaries,
                        &Some(vec![1.0, 10.0]),
                        "measurements[3].boundaries",
                    )?;
                    let spans = fixture.get_spans().await?;
                    assert_eq_val(
                        &measurements[3].context,
                        &Some(find_span(&spans, "measured")?.context.clone()),
                        "measurements[3].context",
                    )?;
                    Ok(())
                })
            }),
        ),
        create_case(
            &factory,
            "metrics",
            "copies instrument metadata and measurement attributes",
            Arc::new(|fixture| {
                Box::pin(async move {
                    let Some(metrics) = fixture.metrics() else {
                        return Ok(());
                    };
                    let mut boundaries = vec![1.0, 10.0];
                    let mut options = HistogramOptions {
                        description: Some("Original".to_string()),
                        unit: None,
                        boundaries: Some(boundaries.clone()),
                    };
                    let histogram = metrics.histogram("snapshot", Some(options.clone()));
                    boundaries.push(100.0);
                    options.description = Some("Changed".to_string());
                    let mut attributes = SpanAttributes::new();
                    attributes.insert(
                        "tags".to_string(),
                        AttributeValue::StringArray(vec!["original".to_string()]),
                    );
                    histogram.record(-1.0, Some(attributes.clone()), None);
                    attributes.insert(
                        "tags".to_string(),
                        AttributeValue::StringArray(vec!["changed".to_string()]),
                    );

                    let measurements = fixture.get_metrics().await?;
                    assert_eq_val(&measurements.len(), &1, "measurements.len")?;
                    let mut expected_attrs = SpanAttributes::new();
                    expected_attrs.insert(
                        "tags".to_string(),
                        AttributeValue::StringArray(vec!["original".to_string()]),
                    );
                    assert_eq_val(
                        &measurements[0].attributes,
                        &expected_attrs,
                        "measurements[0].attributes",
                    )?;
                    assert_eq_val(
                        &measurements[0].boundaries,
                        &Some(vec![1.0, 10.0]),
                        "measurements[0].boundaries",
                    )?;
                    assert_eq_val(
                        &measurements[0].description,
                        &Some("Original".to_string()),
                        "measurements[0].description",
                    )?;
                    assert_eq_val(&measurements[0].context, &None, "measurements[0].context")
                })
            }),
        ),
        create_case(
            &factory,
            "links",
            "records initial and late links without changing parents",
            Arc::new(|fixture| {
                Box::pin(async move {
                    let ctx = fixture.context();
                    let target_holder: Arc<Mutex<Option<Arc<dyn TelemetrySpan + Send + Sync>>>> =
                        Arc::new(Mutex::new(None));
                    let target_cb = Arc::clone(&target_holder);
                    ctx.start_span(
                        SpanOptions::new("target"),
                        Arc::new(move |span| {
                            let target_cb = Arc::clone(&target_cb);
                            Box::pin(async move {
                                *target_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(span);
                                Ok(Box::new(()) as SpanValue)
                            })
                        }),
                    )
                    .await
                    .map_err(|err| span_err_from_span_value(err, "Expected target span to resolve"))?;

                    let target_span = target_holder
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone()
                        .ok_or_else(|| Error::msg("Expected target span"))?;

                    let mut initial_attrs = SpanAttributes::new();
                    initial_attrs.insert(
                        "reason".to_string(),
                        AttributeValue::StringArray(vec!["initial".to_string()]),
                    );

                    let target_for_cb = Arc::clone(&target_span);
                    ctx.start_span(
                        SpanOptions {
                            name: "linked".to_string(),
                            attributes: None,
                            links: Some(vec![SpanLink {
                                context: Arc::clone(&target_span) as Arc<dyn TelemetryContext + Send + Sync>,
                                attributes: Some(initial_attrs.clone()),
                            }]),
                        },
                        Arc::new(move |span| {
                            let target_for_cb = Arc::clone(&target_for_cb);
                            Box::pin(async move {
                                let mut late_attrs = SpanAttributes::new();
                                late_attrs.insert("reason".to_string(), AttributeValue::String("late".to_string()));
                                span.add_link(SpanLink {
                                    context: target_for_cb,
                                    attributes: Some(late_attrs),
                                });
                                span.add_link(SpanLink {
                                    context: Arc::new(NOOP_TELEMETRY_CONTEXT),
                                    attributes: None,
                                });
                                Ok(Box::new(()) as SpanValue)
                            })
                        }),
                    )
                    .await
                    .map_err(|err| span_err_from_span_value(err, "Expected linked span to resolve"))?;

                    let spans = fixture.get_spans().await?;
                    let target_recorded = find_span(&spans, "target")?;
                    let linked = find_span(&spans, "linked")?;
                    assert_eq_val(&linked.parent_id, &None, "linked.parent_id")?;
                    assert_eq_val(&linked.parent_context, &None, "linked.parent_context")?;

                    let mut expected_late = SpanAttributes::new();
                    expected_late.insert("reason".to_string(), AttributeValue::String("late".to_string()));
                    assert_eq_val(
                        &linked.links,
                        &vec![
                            RecordedTelemetryLink {
                                context: target_recorded.context.clone(),
                                attributes: initial_attrs,
                            },
                            RecordedTelemetryLink {
                                context: target_recorded.context.clone(),
                                attributes: expected_late,
                            },
                        ],
                        "linked.links",
                    )
                })
            }),
        ),
        create_case(
            &factory,
            "links",
            "ignores links added after source settlement",
            Arc::new(|fixture| {
                Box::pin(async move {
                    let source_holder: Arc<Mutex<Option<Arc<dyn TelemetrySpan + Send + Sync>>>> =
                        Arc::new(Mutex::new(None));
                    let source_cb = Arc::clone(&source_holder);
                    fixture
                        .context()
                        .start_span(
                            SpanOptions::new("link-source"),
                            Arc::new(move |span| {
                                let source_cb = Arc::clone(&source_cb);
                                Box::pin(async move {
                                    *source_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(span);
                                    Ok(Box::new(()) as SpanValue)
                                })
                            }),
                        )
                        .await
                        .map_err(|err| span_err_from_span_value(err, "Expected link-source span to resolve"))?;

                    let source = source_holder
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone()
                        .ok_or_else(|| Error::msg("Expected source span"))?;
                    source.add_link(SpanLink {
                        context: Arc::clone(&source) as Arc<dyn TelemetryContext + Send + Sync>,
                        attributes: None,
                    });

                    let spans = fixture.get_spans().await?;
                    assert_true(
                        find_span(&spans, "link-source")?.links.is_empty(),
                        "Expected link-source links to remain empty",
                    )
                })
            }),
        ),
        create_case(
            &factory,
            "propagation",
            "roundtrips parent identity through JSON without retaining the carrier",
            Arc::new(|fixture| {
                Box::pin(async move {
                    let Some(propagation) = fixture.propagation() else {
                        return Ok(());
                    };
                    let prop_cb = Arc::clone(&propagation);
                    fixture
                        .context()
                        .start_span(
                            SpanOptions::new("sender"),
                            Arc::new(move |span| {
                                let propagation = Arc::clone(&prop_cb);
                                Box::pin(async move {
                                    let carrier = propagation
                                        .inject(&span)
                                        .ok_or_else(|| Box::new(Error::msg("Expected carrier")) as SpanValue)?;
                                    let wire_json = pi_js::json::stringify(&carrier);
                                    let mut wire: TelemetryCarrier =
                                        pi_js::json::parse_as(&wire_json).map_err(|e| Box::new(e) as SpanValue)?;
                                    let parent = propagation.extract(Some(&wire));
                                    for value in wire.values_mut() {
                                        *value = "mutated".to_string();
                                    }
                                    let receiver_res = parent
                                        .start_span(
                                            SpanOptions::new("receiver"),
                                            Arc::new(|_span| Box::pin(async { Ok(Box::new(42_i64) as SpanValue) })),
                                        )
                                        .await?;
                                    let val = receiver_res
                                        .downcast_ref::<i64>()
                                        .copied()
                                        .ok_or_else(|| Box::new(Error::msg("Expected 42")) as SpanValue)?;
                                    if val != 42 {
                                        return Err(Box::new(Error::msg("Expected receiver to return 42")) as SpanValue);
                                    }
                                    assert_eq_val(&propagation.inject(&span), &Some(carrier), "re-inject")
                                        .map_err(|e| Box::new(e) as SpanValue)?;
                                    Ok(Box::new(()) as SpanValue)
                                })
                            }),
                        )
                        .await
                        .map_err(|err| span_err_from_span_value(err, "Expected sender span to resolve"))?;

                    let spans = fixture.get_spans().await?;
                    assert_eq_val(
                        &find_span(&spans, "receiver")?.parent_context,
                        &Some(find_span(&spans, "sender")?.context.clone()),
                        "receiver.parent_context",
                    )
                })
            }),
        ),
        create_case(
            &factory,
            "propagation",
            "absent and unknown carriers produce recorded roots",
            Arc::new(|fixture| {
                Box::pin(async move {
                    let Some(propagation) = fixture.propagation() else {
                        return Ok(());
                    };
                    assert_eq_val(
                        &propagation.inject(&NOOP_TELEMETRY_CONTEXT),
                        &None,
                        "inject(NOOP_TELEMETRY_CONTEXT)",
                    )?;
                    let empty = TelemetryCarrier::new();
                    let mut unknown = TelemetryCarrier::new();
                    unknown.insert("unknown".to_string(), "format".to_string());
                    let carriers = [None, Some(&empty), Some(&unknown)];
                    for (index, carrier) in carriers.into_iter().enumerate() {
                        let context = propagation.extract(carrier);
                        let res = context
                            .start_span(
                                SpanOptions::new(format!("fallback-{index}")),
                                Arc::new(|_span| Box::pin(async { Ok(Box::new(42_i64) as SpanValue) })),
                            )
                            .await
                            .map_err(|err| span_err_from_span_value(err, "Expected fallback span to resolve"))?;
                        let val = res
                            .downcast_ref::<i64>()
                            .copied()
                            .ok_or_else(|| Error::msg("Expected 42"))?;
                        assert_eq_val(&val, &42, "fallback result")?;
                    }
                    for span in fixture.get_spans().await? {
                        assert_eq_val(&span.parent_context, &None, "fallback.parent_context")?;
                    }
                    Ok(())
                })
            }),
        ),
        create_case(
            &factory,
            "propagation",
            "propagates across fresh adapters and multiple hops",
            Arc::new({
                let factory = Arc::clone(&factory);
                move |fixture| {
                    let factory = Arc::clone(&factory);
                    Box::pin(async move {
                        let Some(sender_prop) = fixture.propagation() else {
                            return Ok(());
                        };
                        let receiver: Arc<dyn TelemetryAdapterFixture + Send + Sync> = Arc::from(factory().await?);
                        let worker: Arc<dyn TelemetryAdapterFixture + Send + Sync> = Arc::from(factory().await?);

                        let result = async {
                            let receiver_prop = receiver
                                .propagation()
                                .ok_or_else(|| Error::msg("Expected propagation on receiver fixture"))?;
                            let worker_prop = worker
                                .propagation()
                                .ok_or_else(|| Error::msg("Expected propagation on worker fixture"))?;

                            let sender_carrier_holder: Arc<Mutex<Option<TelemetryCarrier>>> =
                                Arc::new(Mutex::new(None));
                            let sender_carrier_cb = Arc::clone(&sender_carrier_holder);
                            let sender_prop_cb = Arc::clone(&sender_prop);
                            fixture
                                .context()
                                .start_span(
                                    SpanOptions::new("sender"),
                                    Arc::new(move |span| {
                                        let sender_carrier_cb = Arc::clone(&sender_carrier_cb);
                                        let sender_prop_cb = Arc::clone(&sender_prop_cb);
                                        Box::pin(async move {
                                            *sender_carrier_cb.lock().unwrap_or_else(|e| e.into_inner()) =
                                                sender_prop_cb.inject(&span);
                                            Ok(Box::new(()) as SpanValue)
                                        })
                                    }),
                                )
                                .await
                                .map_err(|err| span_err_from_span_value(err, "Expected sender span"))?;

                            let sender_carrier = sender_carrier_holder
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .clone()
                                .ok_or_else(|| Error::msg("Expected sender carrier"))?;

                            let sender_wire: TelemetryCarrier =
                                pi_js::json::parse_as(&pi_js::json::stringify(&sender_carrier))?;
                            let receiver_carrier_holder: Arc<Mutex<Option<TelemetryCarrier>>> =
                                Arc::new(Mutex::new(None));
                            let receiver_carrier_cb = Arc::clone(&receiver_carrier_holder);
                            let receiver_prop_cb = Arc::clone(&receiver_prop);
                            receiver_prop
                                .extract(Some(&sender_wire))
                                .start_span(
                                    SpanOptions::new("receiver"),
                                    Arc::new(move |span| {
                                        let receiver_carrier_cb = Arc::clone(&receiver_carrier_cb);
                                        let receiver_prop_cb = Arc::clone(&receiver_prop_cb);
                                        Box::pin(async move {
                                            *receiver_carrier_cb.lock().unwrap_or_else(|e| e.into_inner()) =
                                                receiver_prop_cb.inject(&span);
                                            Ok(Box::new(()) as SpanValue)
                                        })
                                    }),
                                )
                                .await
                                .map_err(|err| span_err_from_span_value(err, "Expected receiver span"))?;

                            let receiver_carrier = receiver_carrier_holder
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .clone()
                                .ok_or_else(|| Error::msg("Expected receiver carrier"))?;

                            let receiver_wire: TelemetryCarrier =
                                pi_js::json::parse_as(&pi_js::json::stringify(&receiver_carrier))?;
                            worker_prop
                                .extract(Some(&receiver_wire))
                                .start_span(
                                    SpanOptions::new("worker"),
                                    Arc::new(|_span| Box::pin(async { Ok(Box::new(()) as SpanValue) })),
                                )
                                .await
                                .map_err(|err| span_err_from_span_value(err, "Expected worker span"))?;

                            let receiver_spans = receiver.get_spans().await?;
                            let worker_spans = worker.get_spans().await?;
                            assert_eq_val(
                                &find_span(&receiver_spans, "receiver")?.parent_context,
                                &Some(sender_carrier),
                                "receiver.parent_context",
                            )?;
                            assert_eq_val(
                                &find_span(&worker_spans, "worker")?.parent_context,
                                &Some(receiver_carrier),
                                "worker.parent_context",
                            )
                        }
                        .await;

                        let worker_dispose = worker.async_dispose().await;
                        let receiver_dispose = receiver.async_dispose().await;
                        result?;
                        worker_dispose?;
                        receiver_dispose?;
                        Ok(())
                    })
                }
            }),
        ),
        create_case(
            &factory,
            "callback lifecycle",
            "admits once synchronously and preserves the result",
            Arc::new(|fixture| {
                Box::pin(async move {
                    let admitted = Arc::new(AtomicBool::new(false));
                    let calls = Arc::new(AtomicUsize::new(0));
                    let admitted_cb = Arc::clone(&admitted);
                    let calls_cb = Arc::clone(&calls);

                    let ctx = fixture.context();
                    let mut fut = ctx.start_span(
                        SpanOptions::new("success"),
                        Arc::new(move |_span| {
                            admitted_cb.store(true, Ordering::SeqCst);
                            calls_cb.fetch_add(1, Ordering::SeqCst);
                            Box::pin(async move { Ok(Box::new(ExpectedObject { value: 42 }) as SpanValue) })
                        }),
                    );

                    // PORT: Rust `async fn` futures are lazy until polled; polling once verifies synchronous admission before yielding.
                    let first_poll = futures::poll!(fut.as_mut());
                    assert_true(
                        admitted.load(Ordering::SeqCst),
                        "Expected callback to be admitted on first poll",
                    )?;
                    assert_eq_val(&calls.load(Ordering::SeqCst), &1, "calls")?;
                    let result = match first_poll {
                        Poll::Ready(res) => res,
                        Poll::Pending => fut.await,
                    };
                    let value = result.map_err(|_| Error::msg("Expected success span to resolve"))?;
                    let actual = value
                        .downcast_ref::<ExpectedObject>()
                        .ok_or_else(|| Error::msg("Expected preserved span return value"))?;
                    assert_eq_val(actual, &ExpectedObject { value: 42 }, "result")?;

                    let spans = fixture.get_spans().await?;
                    let success = find_span(&spans, "success")?;
                    assert_eq_val(&success.status, &SpanStatus::Ok, "success.status")?;
                    assert_eq_val(&success.settled, &true, "success.settled")?;
                    Ok(())
                })
            }),
        ),
        create_case(
            &factory,
            "callback lifecycle",
            "preserves synchronous and asynchronous rejection values",
            Arc::new(|fixture| {
                Box::pin(async move {
                    let ctx = fixture.context();

                    rejects_with_js_error(
                        ctx.start_span(
                            SpanOptions::new("sync-error"),
                            Arc::new(|_span| Box::pin(async { Err(Box::new(Error::msg("sync")) as SpanValue) })),
                        ),
                        "Error",
                        "sync",
                    )
                    .await?;

                    rejects_with_value(
                        ctx.start_span(
                            SpanOptions::new("async-error"),
                            Arc::new(|_span| {
                                Box::pin(async {
                                    tokio::task::yield_now().await;
                                    Err(Box::new(PlainError { kind: "async" }) as SpanValue)
                                })
                            }),
                        ),
                        &PlainError { kind: "async" },
                        "async-error",
                    )
                    .await?;

                    rejects_with_value(
                        ctx.start_span(
                            SpanOptions::new("undefined-error"),
                            Arc::new(|_span| Box::pin(async { Err(Box::new(()) as SpanValue) })),
                        ),
                        &(),
                        "undefined-error",
                    )
                    .await?;

                    let spans = fixture.get_spans().await?;
                    for name in ["sync-error", "async-error", "undefined-error"] {
                        let span = find_span(&spans, name)?;
                        assert_true(
                            matches!(span.status, SpanStatus::Error { .. }),
                            format!("Expected error status for span {name}"),
                        )?;
                    }
                    Ok(())
                })
            }),
        ),
        create_case(
            &factory,
            "status",
            "uses last explicit status without automatic overwrite",
            Arc::new(|fixture| {
                Box::pin(async move {
                    let ctx = fixture.context();

                    ctx.start_span(
                        SpanOptions::new("last-status"),
                        Arc::new(|span| {
                            Box::pin(async move {
                                span.set_status(SpanStatus::Error {
                                    error: Some(SpanStatusError {
                                        name: "Expected".to_string(),
                                        message: "first".to_string(),
                                    }),
                                });
                                span.set_status(SpanStatus::Ok);
                                Ok(Box::new(()) as SpanValue)
                            })
                        }),
                    )
                    .await
                    .map_err(|_| Error::msg("Expected last-status span to resolve"))?;

                    rejects_with_js_error(
                        ctx.start_span(
                            SpanOptions::new("explicit-before-throw"),
                            Arc::new(|span| {
                                Box::pin(async move {
                                    span.set_status(SpanStatus::Ok);
                                    Err(Box::new(Error::msg("after explicit status")) as SpanValue)
                                })
                            }),
                        ),
                        "Error",
                        "after explicit status",
                    )
                    .await?;

                    rejects_with_js_error(
                        ctx.start_span(
                            SpanOptions::new("explicit-before-rejection"),
                            Arc::new(|span| {
                                Box::pin(async move {
                                    span.set_status(SpanStatus::Error {
                                        error: Some(SpanStatusError {
                                            name: "Expected".to_string(),
                                            message: "async failure".to_string(),
                                        }),
                                    });
                                    tokio::task::yield_now().await;
                                    Err(Box::new(Error::msg("after async explicit status")) as SpanValue)
                                })
                            }),
                        ),
                        "Error",
                        "after async explicit status",
                    )
                    .await?;

                    ctx.start_span(
                        SpanOptions::new("expected-failure"),
                        Arc::new(|span| {
                            Box::pin(async move {
                                span.set_status(SpanStatus::Error {
                                    error: Some(SpanStatusError {
                                        name: "Expected".to_string(),
                                        message: "returned failure".to_string(),
                                    }),
                                });
                                Ok(Box::new(false) as SpanValue)
                            })
                        }),
                    )
                    .await
                    .map_err(|_| Error::msg("Expected expected-failure span to resolve"))?;

                    let spans = fixture.get_spans().await?;
                    assert_eq_val(
                        &find_span(&spans, "last-status")?.status,
                        &SpanStatus::Ok,
                        "last-status",
                    )?;
                    assert_eq_val(
                        &find_span(&spans, "explicit-before-throw")?.status,
                        &SpanStatus::Ok,
                        "explicit-before-throw",
                    )?;
                    assert_eq_val(
                        &find_span(&spans, "explicit-before-rejection")?.status,
                        &SpanStatus::Error {
                            error: Some(SpanStatusError {
                                name: "Expected".to_string(),
                                message: "async failure".to_string(),
                            }),
                        },
                        "explicit-before-rejection",
                    )?;
                    assert_eq_val(
                        &find_span(&spans, "expected-failure")?.status,
                        &SpanStatus::Error {
                            error: Some(SpanStatusError {
                                name: "Expected".to_string(),
                                message: "returned failure".to_string(),
                            }),
                        },
                        "expected-failure",
                    )?;
                    Ok(())
                })
            }),
        ),
        create_case(
            &factory,
            "recording",
            "merges attributes and records ordered events",
            Arc::new(|fixture| {
                Box::pin(async move {
                    let ctx = fixture.context();
                    let mut start_attrs = SpanAttributes::new();
                    start_attrs.insert("start".to_string(), AttributeValue::String("value".to_string()));
                    start_attrs.insert("overwrite".to_string(), AttributeValue::String("start".to_string()));

                    ctx.start_span(
                        SpanOptions {
                            name: "recording".to_string(),
                            attributes: Some(start_attrs),
                            links: None,
                        },
                        Arc::new(|span| {
                            Box::pin(async move {
                                let mut first_update = SpanAttributes::new();
                                first_update.insert("count".to_string(), AttributeValue::Number(1.0));
                                first_update
                                    .insert("overwrite".to_string(), AttributeValue::String("middle".to_string()));
                                span.set_attributes(first_update);

                                let mut second_update = SpanAttributes::new();
                                second_update
                                    .insert("overwrite".to_string(), AttributeValue::String("end".to_string()));
                                span.set_attributes(second_update);

                                let mut event_one_attrs = SpanAttributes::new();
                                event_one_attrs.insert("index".to_string(), AttributeValue::Number(1.0));
                                span.add_event("first", Some(event_one_attrs));

                                let mut event_two_attrs = SpanAttributes::new();
                                event_two_attrs.insert("index".to_string(), AttributeValue::Number(2.0));
                                span.add_event("second", Some(event_two_attrs));
                                Ok(Box::new(()) as SpanValue)
                            })
                        }),
                    )
                    .await
                    .map_err(|_| Error::msg("Expected recording span to resolve"))?;

                    let spans = fixture.get_spans().await?;
                    let span = find_span(&spans, "recording")?;
                    let mut expected_attrs = SpanAttributes::new();
                    expected_attrs.insert("start".to_string(), AttributeValue::String("value".to_string()));
                    expected_attrs.insert("overwrite".to_string(), AttributeValue::String("end".to_string()));
                    expected_attrs.insert("count".to_string(), AttributeValue::Number(1.0));
                    assert_eq_val(&span.attributes, &expected_attrs, "recording.attributes")?;

                    let mut expected_ev1_attrs = SpanAttributes::new();
                    expected_ev1_attrs.insert("index".to_string(), AttributeValue::Number(1.0));
                    let mut expected_ev2_attrs = SpanAttributes::new();
                    expected_ev2_attrs.insert("index".to_string(), AttributeValue::Number(2.0));
                    let expected_events = vec![
                        RecordedTelemetryEvent {
                            name: "first".to_string(),
                            attributes: expected_ev1_attrs,
                        },
                        RecordedTelemetryEvent {
                            name: "second".to_string(),
                            attributes: expected_ev2_attrs,
                        },
                    ];
                    assert_eq_val(&span.events, &expected_events, "recording.events")?;
                    Ok(())
                })
            }),
        ),
        create_case(
            &factory,
            "recording",
            "makes calls after settlement inert",
            Arc::new(|fixture| {
                Box::pin(async move {
                    let ctx = fixture.context();
                    let settled_span: Arc<Mutex<Option<Arc<dyn TelemetrySpan + Send + Sync>>>> =
                        Arc::new(Mutex::new(None));
                    let settled_span_cb = Arc::clone(&settled_span);
                    let mut initial_attrs = SpanAttributes::new();
                    initial_attrs.insert("value".to_string(), AttributeValue::String("initial".to_string()));

                    ctx.start_span(
                        SpanOptions {
                            name: "settled".to_string(),
                            attributes: Some(initial_attrs.clone()),
                            links: None,
                        },
                        Arc::new(move |span| {
                            let settled_span_cb = Arc::clone(&settled_span_cb);
                            Box::pin(async move {
                                *settled_span_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(span);
                                Ok(Box::new(()) as SpanValue)
                            })
                        }),
                    )
                    .await
                    .map_err(|_| Error::msg("Expected settled span to resolve"))?;

                    let captured_span = settled_span
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone()
                        .ok_or_else(|| Error::msg("Expected callback span"))?;

                    let mut late_attrs = SpanAttributes::new();
                    late_attrs.insert("value".to_string(), AttributeValue::String("late".to_string()));
                    captured_span.set_attributes(late_attrs);

                    let mut late_event_attrs = SpanAttributes::new();
                    late_event_attrs.insert("value".to_string(), AttributeValue::Boolean(true));
                    captured_span.add_event("late", Some(late_event_attrs));
                    captured_span.set_status(SpanStatus::Error { error: None });

                    let child_admitted = Arc::new(AtomicBool::new(false));
                    let child_admitted_cb = Arc::clone(&child_admitted);
                    let mut child_fut = captured_span.start_span(
                        SpanOptions::new("late-child"),
                        Arc::new(move |_span| {
                            child_admitted_cb.store(true, Ordering::SeqCst);
                            Box::pin(async { Ok(Box::new(7_i64) as SpanValue) })
                        }),
                    );
                    let first_poll = futures::poll!(child_fut.as_mut());
                    assert_true(
                        child_admitted.load(Ordering::SeqCst),
                        "Expected late-child to be admitted",
                    )?;
                    let child_result = match first_poll {
                        Poll::Ready(res) => res,
                        Poll::Pending => child_fut.await,
                    };
                    let child_val = child_result
                        .map_err(|_| Error::msg("Expected late-child to resolve"))?
                        .downcast_ref::<i64>()
                        .copied()
                        .ok_or_else(|| Error::msg("Expected late-child to return 7"))?;
                    assert_eq_val(&child_val, &7, "late-child")?;

                    let spans = fixture.get_spans().await?;
                    assert_eq_val(&spans.len(), &1, "spans.len")?;
                    assert_eq_val(&spans[0].attributes, &initial_attrs, "settled.attributes")?;
                    assert_true(spans[0].events.is_empty(), "Expected settled.events to be empty")?;
                    assert_eq_val(&spans[0].status, &SpanStatus::Ok, "settled.status")?;
                    Ok(())
                })
            }),
        ),
        create_case(
            &factory,
            "parentage",
            "records nested and concurrent child relationships",
            Arc::new(|fixture| {
                Box::pin(async move {
                    let ctx = fixture.context();
                    let first_gate = Arc::new(tokio::sync::Notify::new());
                    let first_gate_cb = Arc::clone(&first_gate);

                    ctx.start_span(
                        SpanOptions::new("parent"),
                        Arc::new(move |parent| {
                            let first_gate = Arc::clone(&first_gate_cb);
                            Box::pin(async move {
                                let first_gate_child = Arc::clone(&first_gate);
                                let mut first = parent.start_span(
                                    SpanOptions::new("first-child"),
                                    Arc::new(move |_span| {
                                        let gate = Arc::clone(&first_gate_child);
                                        Box::pin(async move {
                                            gate.notified().await;
                                            Ok(Box::new(()) as SpanValue)
                                        })
                                    }),
                                );

                                // PORT: Poll `first` once so `first-child` starts and suspends on `first_gate` before `second-child`.
                                let first_poll = futures::poll!(first.as_mut());
                                if !first_poll.is_pending() {
                                    return Err(Box::new(Error::msg("Expected first-child to suspend")) as SpanValue);
                                }

                                let second_res = parent
                                    .start_span(
                                        SpanOptions::new("second-child"),
                                        Arc::new(|_span| {
                                            Box::pin(async { Ok(Box::new("done".to_string()) as SpanValue) })
                                        }),
                                    )
                                    .await
                                    .map_err(|err| {
                                        Box::new(span_err_from_span_value(err, "Expected second-child to resolve"))
                                            as SpanValue
                                    })?;
                                let Some(second_str) = second_res.downcast_ref::<String>() else {
                                    return Err(
                                        Box::new(Error::msg("Expected second-child to return string")) as SpanValue
                                    );
                                };
                                if second_str != "done" {
                                    return Err(Box::new(Error::msg(format!(
                                        "second-child: expected \"done\", got {second_str:?}"
                                    ))) as SpanValue);
                                }

                                first_gate.notify_one();
                                first.await.map_err(|err| {
                                    Box::new(span_err_from_span_value(err, "Expected first-child to resolve"))
                                        as SpanValue
                                })?;
                                Ok(Box::new(()) as SpanValue)
                            })
                        }),
                    )
                    .await
                    .map_err(|err| span_err_from_span_value(err, "Expected parent span to resolve"))?;

                    let spans = fixture.get_spans().await?;
                    let parent = find_span(&spans, "parent")?;
                    let first = find_span(&spans, "first-child")?;
                    let second = find_span(&spans, "second-child")?;
                    assert_eq_val(&parent.parent_id, &None, "parent.parent_id")?;
                    assert_eq_val(&first.parent_id, &Some(parent.id), "first.parent_id")?;
                    assert_eq_val(&second.parent_id, &Some(parent.id), "second.parent_id")?;
                    let (Some(second_seq), Some(first_seq), Some(parent_seq)) =
                        (second.end_sequence, first.end_sequence, parent.end_sequence)
                    else {
                        return Err(Error::msg("Expected end_sequence on parent and child spans"));
                    };
                    assert_true(
                        second_seq < first_seq,
                        format!("Expected second.end_sequence ({second_seq}) < first.end_sequence ({first_seq})"),
                    )?;
                    assert_true(
                        first_seq < parent_seq,
                        format!("Expected first.end_sequence ({first_seq}) < parent.end_sequence ({parent_seq})"),
                    )?;
                    Ok(())
                })
            }),
        ),
    ]
}
