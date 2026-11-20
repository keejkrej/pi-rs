//! Port of packages/telemetry/test/propagation.test.ts

use std::process::Command;
use std::sync::{Arc, Mutex};
use std::task::Poll;

use futures::FutureExt;
use pi_telemetry::{
    AttributeValue, Error, InMemoryTelemetryContext, NOOP_TELEMETRY_CONTEXT, NOOP_TELEMETRY_PROPAGATION,
    RecordedTelemetryLink, RecordedTelemetrySpan, SpanAttributes, SpanLink, SpanOptions, SpanValue, TelemetryCarrier,
    TelemetryContext, TelemetryPropagation, TelemetrySpan,
};
use pretty_assertions::assert_eq;

#[tokio::test]
async fn continues_a_parent_in_another_process_without_shared_recorder_state() {
    let telemetry = InMemoryTelemetryContext::new();
    let carrier_holder: Arc<Mutex<Option<TelemetryCarrier>>> = Arc::new(Mutex::new(None));
    let carrier_cb = Arc::clone(&carrier_holder);
    let telemetry_cb = telemetry.clone();

    telemetry
        .start_span(
            SpanOptions::new("caller"),
            Arc::new(move |span| {
                let carrier_cb = Arc::clone(&carrier_cb);
                let telemetry_cb = telemetry_cb.clone();
                Box::pin(async move {
                    *carrier_cb.lock().unwrap() = telemetry_cb.inject(&span);
                    Ok(Box::new(()) as SpanValue)
                })
            }),
        )
        .await
        .expect("caller span should resolve");

    let carrier = carrier_holder.lock().unwrap().clone().expect("expected carrier");
    let output = Command::new(env!("CARGO_BIN_EXE_fixture-propagation-worker"))
        .arg(pi_js::json::stringify(&carrier))
        .output()
        .expect("spawn propagation worker");
    assert!(
        output.status.success(),
        "worker failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("worker stdout utf8");
    let recorded: Vec<RecordedTelemetrySpan> = pi_js::json::parse_as(&stdout).expect("parse worker spans");

    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].parent_context.as_ref(), Some(&carrier));
    assert_eq!(recorded[0].parent_id, None);
    assert_ne!(recorded[0].context, carrier);
}

#[tokio::test]
async fn isolates_crossed_concurrent_remote_parents_and_links_shared_roots() {
    let caller = InMemoryTelemetryContext::new();
    let provider = InMemoryTelemetryContext::new();
    let activation: Arc<Mutex<Option<Arc<dyn TelemetrySpan + Send + Sync>>>> = Arc::new(Mutex::new(None));
    let gate = Arc::new(tokio::sync::Notify::new());
    let shared: Arc<Mutex<Option<futures::future::Shared<pi_js::BoxFuture<std::result::Result<(), &'static str>>>>>> =
        Arc::new(Mutex::new(None));

    let run_branch = |name: &'static str| {
        let caller = caller.clone();
        let provider = provider.clone();
        let activation = Arc::clone(&activation);
        let gate = Arc::clone(&gate);
        let shared = Arc::clone(&shared);
        async move {
            let caller_cb = caller.clone();
            caller
                .start_span(
                    SpanOptions::new(name),
                    Arc::new(move |trigger| {
                        let caller = caller_cb.clone();
                        let provider = provider.clone();
                        let activation = Arc::clone(&activation);
                        let gate = Arc::clone(&gate);
                        let shared = Arc::clone(&shared);
                        Box::pin(async move {
                            let parent = provider.extract(caller.inject(&trigger).as_ref());
                            parent
                                .start_span(
                                    SpanOptions::new(format!("{name}.method")),
                                    Arc::new(move |getter| {
                                        let provider = provider.clone();
                                        let activation = Arc::clone(&activation);
                                        let gate = Arc::clone(&gate);
                                        let shared = Arc::clone(&shared);
                                        Box::pin(async move {
                                            let existing = shared.lock().unwrap().clone();
                                            let shared_fut = match existing {
                                                Some(fut) => fut,
                                                None => {
                                                    let act_setter = Arc::clone(&activation);
                                                    let gate_waiter = Arc::clone(&gate);
                                                    let getter_ctx: Arc<dyn TelemetryContext + Send + Sync> =
                                                        Arc::clone(&getter) as Arc<dyn TelemetryContext + Send + Sync>;
                                                    let mut raw: pi_js::BoxFuture<
                                                        std::result::Result<(), &'static str>,
                                                    > = Box::pin(async move {
                                                        provider
                                                            .start_span(
                                                                SpanOptions {
                                                                    name: "activation".to_string(),
                                                                    attributes: None,
                                                                    links: Some(vec![SpanLink::new(getter_ctx)]),
                                                                },
                                                                Arc::new(move |span| {
                                                                    let act_setter = Arc::clone(&act_setter);
                                                                    let gate_waiter = Arc::clone(&gate_waiter);
                                                                    Box::pin(async move {
                                                                        *act_setter.lock().unwrap() = Some(span);
                                                                        gate_waiter.notified().await;
                                                                        Ok(Box::new(()) as SpanValue)
                                                                    })
                                                                }),
                                                            )
                                                            .await
                                                            .map(|_| ())
                                                            .map_err(|_| "activation failed")
                                                    });
                                                    // PORT: Drive activation to `gate_waiter.notified()` so `activation` handle is ready for concurrent callers.
                                                    let poll = futures::poll!(raw.as_mut());
                                                    assert!(matches!(poll, Poll::Pending));
                                                    let fut = raw.shared();
                                                    *shared.lock().unwrap() = Some(fut.clone());
                                                    fut
                                                }
                                            };

                                            if let Some(act_span) = activation.lock().unwrap().clone() {
                                                getter.add_link(SpanLink::new(
                                                    act_span as Arc<dyn TelemetryContext + Send + Sync>,
                                                ));
                                            }
                                            if name == "later" {
                                                gate.notify_waiters();
                                            }
                                            shared_fut.await.map_err(|msg| Box::new(Error::msg(msg)) as SpanValue)?;
                                            Ok(Box::new(()) as SpanValue)
                                        })
                                    }),
                                )
                                .await
                        })
                    }),
                )
                .await
        }
    };

    let (r1, r2) = tokio::join!(run_branch("first"), run_branch("later"));
    r1.expect("first branch should resolve");
    r2.expect("later branch should resolve");

    let spans = provider.get_spans();
    let root = spans
        .iter()
        .find(|span| span.name == "activation")
        .expect("activation span");
    assert_eq!(root.parent_context, None);
    let first = spans
        .iter()
        .find(|span| span.name == "first.method")
        .expect("first.method span");
    let later = spans
        .iter()
        .find(|span| span.name == "later.method")
        .expect("later.method span");

    let caller_spans = caller.get_spans();
    assert_eq!(first.parent_context.as_ref(), Some(&caller_spans[0].context));
    assert_eq!(later.parent_context.as_ref(), Some(&caller_spans[1].context));
    assert_eq!(root.links[0].context, first.context);
    assert_eq!(later.links[0].context, root.context);
}

#[tokio::test]
async fn detaches_carrier_and_link_snapshots_and_retains_settled_target_identities() {
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
        .expect("target span should resolve");

    let target = target_holder.lock().unwrap().clone().expect("expected target");
    let original = telemetry.inject(&target).expect("expected original carrier");
    let mut injected = telemetry.inject(&target).expect("expected injected carrier");
    injected.insert("pi-memory-span".to_string(), "999".to_string());
    assert_eq!(telemetry.inject(&target).as_ref(), Some(&original));

    let mut link_attrs = SpanAttributes::new();
    link_attrs.insert(
        "tags".to_string(),
        AttributeValue::StringArray(vec!["original".to_string()]),
    );
    telemetry
        .start_span(
            SpanOptions {
                name: "linked".to_string(),
                attributes: None,
                links: Some(vec![SpanLink {
                    context: target as Arc<dyn TelemetryContext + Send + Sync>,
                    attributes: Some(link_attrs.clone()),
                }]),
            },
            Arc::new(|_span| Box::pin(async { Ok(Box::new(()) as SpanValue) })),
        )
        .await
        .expect("linked span should resolve");

    let mut spans = telemetry.get_spans();
    assert_eq!(spans.len(), 2);
    spans[0].context.insert("pi-memory-span".to_string(), "123".to_string());
    spans[1].links[0]
        .context
        .insert("pi-memory-span".to_string(), "456".to_string());
    spans[1].links[0].attributes.insert(
        "tags".to_string(),
        AttributeValue::StringArray(vec!["mutated".to_string()]),
    );

    assert_eq!(
        telemetry.get_spans()[1].links,
        vec![RecordedTelemetryLink {
            context: original,
            attributes: link_attrs,
        }]
    );
}

#[tokio::test]
async fn links_a_new_root_to_an_extracted_remote_target() {
    let sender = InMemoryTelemetryContext::new();
    let receiver = InMemoryTelemetryContext::new();
    let carrier_holder: Arc<Mutex<Option<TelemetryCarrier>>> = Arc::new(Mutex::new(None));
    let carrier_cb = Arc::clone(&carrier_holder);
    let sender_cb = sender.clone();

    sender
        .start_span(
            SpanOptions::new("trigger"),
            Arc::new(move |span| {
                let carrier_cb = Arc::clone(&carrier_cb);
                let sender_cb = sender_cb.clone();
                Box::pin(async move {
                    *carrier_cb.lock().unwrap() = sender_cb.inject(&span);
                    Ok(Box::new(()) as SpanValue)
                })
            }),
        )
        .await
        .expect("trigger span should resolve");

    let carrier = carrier_holder.lock().unwrap().clone().expect("expected carrier");
    let wire: TelemetryCarrier = pi_js::json::parse_as(&pi_js::json::stringify(&carrier)).expect("roundtrip");
    let target = receiver.extract(Some(&wire));
    let target_cb = Arc::clone(&target);

    let mut restart_attrs = SpanAttributes::new();
    restart_attrs.insert("reason".to_string(), AttributeValue::String("restart".to_string()));
    receiver
        .start_span(
            SpanOptions {
                name: "background".to_string(),
                attributes: None,
                links: Some(vec![SpanLink {
                    context: target,
                    attributes: Some(restart_attrs.clone()),
                }]),
            },
            Arc::new(move |span| {
                let target_cb = Arc::clone(&target_cb);
                Box::pin(async move {
                    span.add_link(SpanLink::new(target_cb));
                    Ok(Box::new(()) as SpanValue)
                })
            }),
        )
        .await
        .expect("background span should resolve");

    let spans = receiver.get_spans();
    let recorded = spans.first().expect("expected recorded span");
    assert_eq!(recorded.parent_context, None);
    assert_eq!(
        recorded.links,
        vec![
            RecordedTelemetryLink {
                context: carrier.clone(),
                attributes: restart_attrs,
            },
            RecordedTelemetryLink {
                context: carrier,
                attributes: SpanAttributes::new(),
            },
        ]
    );
}

#[tokio::test]
async fn uses_the_root_for_absent_and_unknown_carriers() {
    let telemetry = InMemoryTelemetryContext::new();
    let root = telemetry.as_context();
    let empty = TelemetryCarrier::new();
    let mut unknown = TelemetryCarrier::new();
    unknown.insert("unknown".to_string(), "format".to_string());

    for carrier in [None, Some(&empty), Some(&unknown)] {
        assert!(Arc::ptr_eq(&telemetry.extract(carrier), &root));
    }

    assert_eq!(NOOP_TELEMETRY_PROPAGATION.inject(&NOOP_TELEMETRY_CONTEXT), None);
    let noop_extracted = NOOP_TELEMETRY_PROPAGATION.extract(Some(&unknown));
    assert!(noop_extracted._in_memory_carrier().is_none());

    NOOP_TELEMETRY_CONTEXT
        .start_span(
            SpanOptions::new("noop"),
            Arc::new(|span| {
                Box::pin(async move {
                    span.add_link(SpanLink::new(span.clone() as Arc<dyn TelemetryContext + Send + Sync>));
                    Ok(Box::new(()) as SpanValue)
                })
            }),
        )
        .await
        .expect("noop span should resolve");
}

#[tokio::test]
async fn treats_recognized_carrier_identifiers_as_opaque_strings() {
    let telemetry = InMemoryTelemetryContext::new();
    let mut carrier = TelemetryCarrier::new();
    carrier.insert("pi-memory-scope".to_string(), "external-recorder".to_string());
    carrier.insert("pi-memory-span".to_string(), "external-span".to_string());
    carrier.insert("pi-memory-trace".to_string(), "external-trace".to_string());

    telemetry
        .extract(Some(&carrier))
        .start_span(
            SpanOptions::new("receiver"),
            Arc::new(|_span| Box::pin(async { Ok(Box::new(()) as SpanValue) })),
        )
        .await
        .expect("receiver span should resolve");

    assert_eq!(telemetry.get_spans()[0].parent_context.as_ref(), Some(&carrier));
}
