//! Port of packages/telemetry/test/telemetry.test.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;

use indexmap::IndexMap;
use pi_telemetry::{
    AttributeValue, InMemoryTelemetryContext, NOOP_TELEMETRY_CONTEXT, SpanAttributes, SpanOptions, SpanStatus,
    SpanValue, TelemetryAttributeCardinality, TelemetryAttributeDefinition, TelemetryContext,
    TelemetryEventAttributeDefinition, TelemetryEventDefinition, TelemetryParentDefinition, TelemetrySchemaDefinition,
    TelemetrySpan, TelemetrySpanDefinition, TelemetrySpanStatusDefault, TelemetrySpanStatusDefinition,
    TelemetryStartAttributeDefinition, create_typed_span_starter, define_telemetry_schema,
};

#[derive(Debug, PartialEq)]
struct PlainError {
    kind: &'static str,
}

mod telemetry_schemas {
    use pretty_assertions::{assert_eq, assert_ne};

    use super::*;

    #[test]
    fn preserves_serializable_definitions_and_infers_exact_attributes() {
        let mut start_attributes = IndexMap::new();
        start_attributes.insert(
            "kind".to_string(),
            TelemetryStartAttributeDefinition::String {
                required: true,
                values: Some(vec!["read".to_string(), "write".to_string()]),
                examples: None,
                description: "Kind".to_string(),
                sensitive: None,
                cardinality: None,
            },
        );

        let mut event_attributes = IndexMap::new();
        event_attributes.insert(
            "outcome".to_string(),
            TelemetryEventAttributeDefinition::String {
                required: true,
                values: Some(vec!["ok".to_string(), "error".to_string()]),
                examples: None,
                description: "Outcome".to_string(),
                sensitive: None,
                cardinality: None,
            },
        );

        let mut events = IndexMap::new();
        events.insert(
            "result".to_string(),
            TelemetryEventDefinition {
                description: "Result".to_string(),
                attributes: event_attributes,
            },
        );

        let mut spans = IndexMap::new();
        spans.insert(
            "operation".to_string(),
            TelemetrySpanDefinition {
                description: "Test operation".to_string(),
                parents: TelemetryParentDefinition::Any,
                start_attributes,
                end_attributes: IndexMap::<String, TelemetryAttributeDefinition>::new(),
                events: Some(events),
                status: TelemetrySpanStatusDefinition {
                    r#default: TelemetrySpanStatusDefault::Ok,
                    error_when: "The operation fails".to_string(),
                },
            },
        );

        let definition = TelemetrySchemaDefinition { version: 1, spans };
        let schema = define_telemetry_schema(definition.clone());
        assert_eq!(schema, definition);

        let serialized = pi_js::json::stringify(&schema);
        let parsed: TelemetrySchemaDefinition =
            pi_js::json::parse_as(&serialized).expect("schema JSON should round-trip");
        assert_eq!(parsed, schema);

        // Verify optional metadata enum variants serialize cleanly too.
        assert_ne!(
            pi_js::json::stringify(&TelemetryAttributeCardinality::Low),
            pi_js::json::stringify(&TelemetryAttributeCardinality::High)
        );
        // PORT: TS compile-time conditional type assertions (`expectTypeOf`, `@ts-expect-error`) are N/A
        // in Rust because conditional schema types are erased per PORTING.md §3.2.
    }

    #[tokio::test]
    async fn combines_schema_vocabularies_and_binds_child_starters_to_their_parent_spans() {
        let mut op_start_attrs = IndexMap::new();
        op_start_attrs.insert(
            "kind".to_string(),
            TelemetryStartAttributeDefinition::String {
                required: true,
                values: Some(vec!["read".to_string(), "write".to_string()]),
                examples: None,
                description: "Kind".to_string(),
                sensitive: None,
                cardinality: None,
            },
        );
        let mut op_spans = IndexMap::new();
        op_spans.insert(
            "operation".to_string(),
            TelemetrySpanDefinition {
                description: "Operation".to_string(),
                parents: TelemetryParentDefinition::RootOrExternal,
                start_attributes: op_start_attrs,
                end_attributes: IndexMap::new(),
                events: None,
                status: TelemetrySpanStatusDefinition {
                    r#default: TelemetrySpanStatusDefault::Ok,
                    error_when: "The operation fails".to_string(),
                },
            },
        );
        let operation_schema = define_telemetry_schema(TelemetrySchemaDefinition {
            version: 1,
            spans: op_spans,
        });

        let mut req_start_attrs = IndexMap::new();
        req_start_attrs.insert(
            "provider".to_string(),
            TelemetryStartAttributeDefinition::String {
                required: true,
                values: None,
                examples: None,
                description: "Provider".to_string(),
                sensitive: None,
                cardinality: None,
            },
        );
        let mut req_end_attrs = IndexMap::new();
        req_end_attrs.insert(
            "response".to_string(),
            TelemetryAttributeDefinition::String {
                values: None,
                examples: None,
                description: "Response kind".to_string(),
                sensitive: None,
                cardinality: None,
            },
        );
        let mut req_spans = IndexMap::new();
        req_spans.insert(
            "request".to_string(),
            TelemetrySpanDefinition {
                description: "Request".to_string(),
                parents: TelemetryParentDefinition::Spans {
                    spans: vec!["operation".to_string()],
                },
                start_attributes: req_start_attrs,
                end_attributes: req_end_attrs,
                events: None,
                status: TelemetrySpanStatusDefinition {
                    r#default: TelemetrySpanStatusDefault::Ok,
                    error_when: "The request fails".to_string(),
                },
            },
        );
        let request_schema = define_telemetry_schema(TelemetrySchemaDefinition {
            version: 3,
            spans: req_spans,
        });

        let telemetry_context = InMemoryTelemetryContext::new();
        let start_span = create_typed_span_starter(telemetry_context.clone(), &[operation_schema, request_schema]);

        let mut op_attrs = SpanAttributes::new();
        op_attrs.insert("kind".to_string(), AttributeValue::String("read".to_string()));
        let result = start_span
            .start(
                "operation",
                op_attrs,
                Arc::new(|_operation_span, start_child_span| {
                    Box::pin(async move {
                        let mut req_attrs = SpanAttributes::new();
                        req_attrs.insert("provider".to_string(), AttributeValue::String("example".to_string()));
                        start_child_span
                            .start(
                                "request",
                                req_attrs,
                                Arc::new(|request_span, _grandchild_starter| {
                                    Box::pin(async move {
                                        let mut end_attrs = SpanAttributes::new();
                                        end_attrs.insert(
                                            "response".to_string(),
                                            AttributeValue::String("cached".to_string()),
                                        );
                                        request_span.set_attributes(end_attrs);
                                        Ok(Box::new(42_i64) as SpanValue)
                                    })
                                }),
                            )
                            .await
                    })
                }),
            )
            .await
            .expect("operation span should resolve");

        assert_eq!(result.downcast_ref::<i64>().copied(), Some(42));

        let spans = telemetry_context.get_spans();
        let operation_span = spans
            .iter()
            .find(|span| span.name == "operation")
            .expect("expected operation span");
        let request_span = spans
            .iter()
            .find(|span| span.name == "request")
            .expect("expected request span");
        assert_eq!(operation_span.parent_id, None);
        assert_eq!(request_span.parent_id, Some(operation_span.id));
        assert_eq!(
            request_span.attributes.get("response"),
            Some(&AttributeValue::String("cached".to_string()))
        );

        // PORT: JS Proxy `unreadable(schemas)` and `@ts-expect-error` compile-time checks are N/A in Rust.

        let mut sync_attrs = SpanAttributes::new();
        sync_attrs.insert("kind".to_string(), AttributeValue::String("write".to_string()));
        let sync_err = start_span
            .start(
                "operation",
                sync_attrs,
                Arc::new(|_span, _child| Box::pin(async { Err(Box::new(PlainError { kind: "sync" }) as SpanValue) })),
            )
            .await
            .expect_err("sync error span should reject");
        assert_eq!(
            sync_err.downcast_ref::<PlainError>(),
            Some(&PlainError { kind: "sync" })
        );

        let mut async_attrs = SpanAttributes::new();
        async_attrs.insert("provider".to_string(), AttributeValue::String("example".to_string()));
        let async_err = start_span
            .start(
                "request",
                async_attrs,
                Arc::new(|_span, _child| {
                    Box::pin(async {
                        tokio::task::yield_now().await;
                        Err(Box::new(PlainError { kind: "async" }) as SpanValue)
                    })
                }),
            )
            .await
            .expect_err("async error span should reject");
        assert_eq!(
            async_err.downcast_ref::<PlainError>(),
            Some(&PlainError { kind: "async" })
        );
    }
}

mod noop_telemetry_context {
    use pretty_assertions::assert_eq;

    use super::*;

    #[tokio::test]
    async fn admits_callbacks_synchronously_and_reuses_one_inert_span() {
        let admitted = Arc::new(AtomicBool::new(false));
        let admitted_cb = Arc::clone(&admitted);
        let first_span: Arc<std::sync::Mutex<Option<Arc<dyn TelemetrySpan + Send + Sync>>>> =
            Arc::new(std::sync::Mutex::new(None));
        let first_span_cb = Arc::clone(&first_span);

        let mut fut = NOOP_TELEMETRY_CONTEXT.start_span(
            SpanOptions {
                name: "first".to_string(),
                attributes: None,
            },
            Arc::new(move |span| {
                let admitted_cb = Arc::clone(&admitted_cb);
                let first_span_cb = Arc::clone(&first_span_cb);
                Box::pin(async move {
                    admitted_cb.store(true, Ordering::SeqCst);
                    *first_span_cb.lock().unwrap() = Some(Arc::clone(&span));
                    let child = span
                        .start_span(
                            SpanOptions {
                                name: "child".to_string(),
                                attributes: None,
                            },
                            Arc::new(|child_span| Box::pin(async move { Ok(Box::new(child_span) as SpanValue) })),
                        )
                        .await
                        .expect("child span should resolve");
                    let child_span = child
                        .downcast_ref::<Arc<dyn TelemetrySpan + Send + Sync>>()
                        .expect("expected TelemetrySpan handle");
                    assert!(Arc::ptr_eq(child_span, &span));
                    Ok(Box::new(42_i64) as SpanValue)
                })
            }),
        );

        // PORT: Rust `async fn` futures are lazy until polled; polling once verifies synchronous admission before yielding.
        let first_poll = futures::poll!(fut.as_mut());
        assert_eq!(admitted.load(Ordering::SeqCst), true);
        let result = match first_poll {
            Poll::Ready(res) => res,
            Poll::Pending => fut.await,
        };
        let value = result.expect("noop span should resolve");
        assert_eq!(value.downcast_ref::<i64>().copied(), Some(42));
        assert!(first_span.lock().unwrap().is_some());
        // PORT: `Object.isFrozen(firstSpan)` is N/A in Rust; `NoopTelemetryContext` is an immutable Copy unit struct.
    }

    #[tokio::test]
    async fn preserves_synchronous_and_asynchronous_rejection_values() {
        let sync_err = NOOP_TELEMETRY_CONTEXT
            .start_span(
                SpanOptions {
                    name: "sync".to_string(),
                    attributes: None,
                },
                Arc::new(|_span| Box::pin(async { Err(Box::new(PlainError { kind: "sync" }) as SpanValue) })),
            )
            .await
            .expect_err("sync error should reject");
        assert_eq!(
            sync_err.downcast_ref::<PlainError>(),
            Some(&PlainError { kind: "sync" })
        );

        let async_err = NOOP_TELEMETRY_CONTEXT
            .start_span(
                SpanOptions {
                    name: "async".to_string(),
                    attributes: None,
                },
                Arc::new(|_span| {
                    Box::pin(async {
                        tokio::task::yield_now().await;
                        Err(Box::new(PlainError { kind: "async" }) as SpanValue)
                    })
                }),
            )
            .await
            .expect_err("async error should reject");
        assert_eq!(
            async_err.downcast_ref::<PlainError>(),
            Some(&PlainError { kind: "async" })
        );
    }

    #[tokio::test]
    async fn does_not_inspect_or_retain_telemetry_payloads() {
        let mut start_attrs = SpanAttributes::new();
        start_attrs.insert(
            "secret".to_string(),
            AttributeValue::String("prompt content".to_string()),
        );
        // PORT: JS Proxy `unreadable()` traps are N/A for Rust typed structs.
        let result = NOOP_TELEMETRY_CONTEXT
            .start_span(
                SpanOptions {
                    name: "operation".to_string(),
                    attributes: Some(start_attrs),
                },
                Arc::new(|span| {
                    Box::pin(async move {
                        let mut attrs = SpanAttributes::new();
                        attrs.insert("secret".to_string(), AttributeValue::String("content".to_string()));
                        span.add_event("event", Some(attrs.clone()));
                        span.set_attributes(attrs);
                        span.set_status(SpanStatus::Ok);
                        Ok(Box::new(()) as SpanValue)
                    })
                }),
            )
            .await;
        assert!(result.is_ok());
    }
}
