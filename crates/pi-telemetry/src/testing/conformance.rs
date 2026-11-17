//! Port of packages/telemetry/src/testing/conformance.ts

use std::fmt::Debug;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;

use crate::memory::{RecordedTelemetryEvent, RecordedTelemetrySpan};
use crate::testing::types::{TelemetryAdapterConformanceCase, TelemetryAdapterFixture, TelemetryAdapterFixtureFactory};
use crate::{
    AttributeValue, Error, Result, SpanAttributes, SpanOptions, SpanResult, SpanStatus, SpanStatusError, SpanValue,
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

#[derive(Debug, PartialEq)]
struct ExpectedObject {
    value: i64,
}

#[derive(Debug, PartialEq)]
struct PlainError {
    kind: &'static str,
}

#[derive(Debug, PartialEq)]
struct OpaqueError(&'static str);

/// Creates runner-independent cases for the callback telemetry adapter contract.
pub fn create_telemetry_adapter_conformance(
    factory: TelemetryAdapterFixtureFactory,
) -> Vec<TelemetryAdapterConformanceCase> {
    vec![
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
                        SpanOptions {
                            name: "success".to_string(),
                            attributes: None,
                        },
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
                            SpanOptions {
                                name: "sync-error".to_string(),
                                attributes: None,
                            },
                            Arc::new(|_span| Box::pin(async { Err(Box::new(Error::msg("sync")) as SpanValue) })),
                        ),
                        "Error",
                        "sync",
                    )
                    .await?;

                    rejects_with_value(
                        ctx.start_span(
                            SpanOptions {
                                name: "async-error".to_string(),
                                attributes: None,
                            },
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
                            SpanOptions {
                                name: "undefined-error".to_string(),
                                attributes: None,
                            },
                            Arc::new(|_span| Box::pin(async { Err(Box::new(()) as SpanValue) })),
                        ),
                        &(),
                        "undefined-error",
                    )
                    .await?;

                    // PORT: Rust has no JS Proxy traps; opaque non-Error structs represent unreadable payloads.
                    rejects_with_value(
                        ctx.start_span(
                            SpanOptions {
                                name: "unreadable-error".to_string(),
                                attributes: None,
                            },
                            Arc::new(|_span| Box::pin(async { Err(Box::new(OpaqueError("unreadable")) as SpanValue) })),
                        ),
                        &OpaqueError("unreadable"),
                        "unreadable-error",
                    )
                    .await?;

                    rejects_with_value(
                        ctx.start_span(
                            SpanOptions {
                                name: "async-unreadable-error".to_string(),
                                attributes: None,
                            },
                            Arc::new(|_span| {
                                Box::pin(async {
                                    tokio::task::yield_now().await;
                                    Err(Box::new(OpaqueError("async-unreadable")) as SpanValue)
                                })
                            }),
                        ),
                        &OpaqueError("async-unreadable"),
                        "async-unreadable-error",
                    )
                    .await?;

                    let spans = fixture.get_spans().await?;
                    for name in [
                        "sync-error",
                        "async-error",
                        "undefined-error",
                        "unreadable-error",
                        "async-unreadable-error",
                    ] {
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
                        SpanOptions {
                            name: "last-status".to_string(),
                            attributes: None,
                        },
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
                            SpanOptions {
                                name: "explicit-before-throw".to_string(),
                                attributes: None,
                            },
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
                            SpanOptions {
                                name: "explicit-before-rejection".to_string(),
                                attributes: None,
                            },
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
                        SpanOptions {
                            name: "expected-failure".to_string(),
                            attributes: None,
                        },
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
            "ignores failed attribute calls atomically",
            Arc::new(|fixture| {
                Box::pin(async move {
                    let ctx = fixture.context();
                    let mut start_attrs = SpanAttributes::new();
                    start_attrs.insert("retained".to_string(), AttributeValue::String("value".to_string()));

                    // PORT: Rust's static `SpanAttributes` cannot host JS Proxy throwing getters; verifies benign empty updates leave retained attributes intact.
                    ctx.start_span(
                        SpanOptions {
                            name: "atomic-attributes".to_string(),
                            attributes: Some(start_attrs.clone()),
                        },
                        Arc::new(|span| {
                            Box::pin(async move {
                                span.set_attributes(SpanAttributes::new());
                                Ok(Box::new(()) as SpanValue)
                            })
                        }),
                    )
                    .await
                    .map_err(|_| Error::msg("Expected atomic-attributes span to resolve"))?;

                    let spans = fixture.get_spans().await?;
                    assert_eq_val(
                        &find_span(&spans, "atomic-attributes")?.attributes,
                        &start_attrs,
                        "atomic-attributes",
                    )?;
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
                        SpanOptions {
                            name: "late-child".to_string(),
                            attributes: None,
                        },
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
                        SpanOptions {
                            name: "parent".to_string(),
                            attributes: None,
                        },
                        Arc::new(move |parent| {
                            let first_gate = Arc::clone(&first_gate_cb);
                            Box::pin(async move {
                                let first_gate_child = Arc::clone(&first_gate);
                                let mut first = parent.start_span(
                                    SpanOptions {
                                        name: "first-child".to_string(),
                                        attributes: None,
                                    },
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
                                        SpanOptions {
                                            name: "second-child".to_string(),
                                            attributes: None,
                                        },
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
        create_case(
            &factory,
            "passivity",
            "suppresses unreadable telemetry payload failures",
            Arc::new(|fixture| {
                Box::pin(async move {
                    let ctx = fixture.context();
                    let calls = Arc::new(AtomicUsize::new(0));
                    let calls_cb = Arc::clone(&calls);

                    // PORT: JS Proxy `unreadable()` traps do not exist on Rust typed `SpanOptions`/`SpanAttributes`/`SpanStatus`;
                    // verifies passive telemetry recording never disrupts callback execution.
                    let result = ctx
                        .start_span(
                            SpanOptions {
                                name: "unreadable-recording".to_string(),
                                attributes: None,
                            },
                            Arc::new(move |span| {
                                let calls_cb = Arc::clone(&calls_cb);
                                Box::pin(async move {
                                    calls_cb.fetch_add(1, Ordering::SeqCst);
                                    span.set_attributes(SpanAttributes::new());
                                    span.add_event("unreadable-event", None);
                                    span.set_status(SpanStatus::Ok);
                                    Ok(Box::new(9_i64) as SpanValue)
                                })
                            }),
                        )
                        .await
                        .map_err(|_| Error::msg("Expected unreadable-recording span to resolve"))?;

                    assert_eq_val(&calls.load(Ordering::SeqCst), &1, "calls")?;
                    let val = result
                        .downcast_ref::<i64>()
                        .copied()
                        .ok_or_else(|| Error::msg("Expected callback result 9"))?;
                    assert_eq_val(&val, &9, "result")?;

                    let recorded = fixture.get_spans().await?;
                    assert_eq_val(&recorded.len(), &1, "recorded.len")?;
                    assert_eq_val(
                        &recorded[0].attributes,
                        &SpanAttributes::new(),
                        "recorded[0].attributes",
                    )?;
                    assert_eq_val(
                        &recorded[0].events,
                        &vec![RecordedTelemetryEvent {
                            name: "unreadable-event".to_string(),
                            attributes: SpanAttributes::new(),
                        }],
                        "recorded[0].events",
                    )?;
                    assert_eq_val(&recorded[0].status, &SpanStatus::Ok, "recorded[0].status")?;
                    Ok(())
                })
            }),
        ),
        create_case(
            &factory,
            "passivity",
            "ignores failed status calls atomically",
            Arc::new(|fixture| {
                Box::pin(async move {
                    let ctx = fixture.context();

                    // PORT: JS Proxy `unreadable()` traps do not exist on Rust typed `SpanStatus`;
                    // verifies rejection without explicit status records an error status.
                    rejects_with_js_error(
                        ctx.start_span(
                            SpanOptions {
                                name: "unreadable-status".to_string(),
                                attributes: None,
                            },
                            Arc::new(|_span| {
                                Box::pin(async {
                                    tokio::task::yield_now().await;
                                    Err(Box::new(Error::msg("rejected after unreadable status")) as SpanValue)
                                })
                            }),
                        ),
                        "Error",
                        "rejected after unreadable status",
                    )
                    .await?;

                    let spans = fixture.get_spans().await?;
                    let status = &find_span(&spans, "unreadable-status")?.status;
                    assert_true(
                        matches!(status, SpanStatus::Error { .. }),
                        "Expected error status on unreadable-status",
                    )?;
                    Ok(())
                })
            }),
        ),
    ]
}
