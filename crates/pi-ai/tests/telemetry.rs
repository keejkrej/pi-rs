//! Port of packages/ai/test/telemetry.test.ts

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use indexmap::IndexMap;
use pi_ai::AssistantMessageEventStream;
use pi_ai::api::lazy::{LazyStreamObserver, lazy_stream_with_options};
use pi_ai::auth::types::{ApiKeyAuth, ApiKeyAuthInput, AuthResult, ProviderAuth};
use pi_ai::models::{
    CreateProviderApi, CreateProviderOptions, ModelsApiStreamOptions, ModelsDeferredFetchOptions,
    ModelsSimpleStreamOptions, create_models, create_provider,
};
use pi_ai::telemetry::{AI_TELEMETRY_SCHEMA, observe_model_request};
use pi_ai::types::{
    ApiStreamOptions, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, DeferredFetchOptions,
    DeferredHandle, JsonObject, Model, ModelCost, ModelInput, ProviderRequestOptions, ProviderStreams,
    SimpleStreamOptions, StopReason, StreamOptions, TelemetrySpanHandle, TextContent, ThinkingContent, ThinkingLevel,
    ToolCall, Usage, UsageCost,
};
use pi_js::{Error, Result};
use pi_telemetry::{
    AttributeValue, HistogramOptions, InMemoryTelemetryContext, InstrumentOptions, MetricKind,
    RecordedTelemetryMeasurement, SpanAttributes, SpanCallback, SpanLink, SpanOptions, SpanResult, SpanStatus,
    SpanStatusError, TelemetryContext, TelemetryCounter, TelemetryHistogram, TelemetryMetrics, TelemetrySpan,
    TelemetryUpDownCounter,
};

fn test_model() -> Model {
    Model {
        r#type: None,
        id: "model".to_owned(),
        name: "Model".to_owned(),
        api: "telemetry-test".to_owned(),
        provider: "test".to_owned(),
        base_url: "https://example.test".to_owned(),
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost {
            input: 1.0,
            output: 2.0,
            cache_read: 3.0,
            cache_write: 4.0,
            tiers: None,
        },
        context_window: 1000,
        max_tokens: 100,
        headers: None,
        sampling_params: None,
        compat: None,
        sampling_params_by_thinking_level: None,
    }
}

fn test_handle() -> DeferredHandle {
    DeferredHandle {
        provider: "test".to_owned(),
        model_id: "model".to_owned(),
        api: "telemetry-test".to_owned(),
        id: "deferred-secret".to_owned(),
        expires_at: None,
        poll_after_ms: None,
        data: None,
    }
}

fn message(stop_reason: StopReason) -> AssistantMessage {
    let mut args = JsonObject::new();
    args.insert("value".to_owned(), serde_json::json!("argument-secret"));
    AssistantMessage {
        content: vec![
            AssistantContentBlock::Text(TextContent::new("text-secret")),
            AssistantContentBlock::Thinking(ThinkingContent::new("thinking-secret")),
            AssistantContentBlock::ToolCall(ToolCall::new("call-secret", "lookup", args)),
        ],
        api: "telemetry-test".to_owned(),
        provider: "test".to_owned(),
        model: "model".to_owned(),
        response_model: None,
        response_id: Some("response".to_owned()),
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage {
            input: 11,
            output: 22,
            cache_read: 33,
            cache_write: 44,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 110,
            cost: UsageCost {
                input: 0.1,
                output: 0.2,
                cache_read: 0.3,
                cache_write: 0.4,
                total: 1.0,
                total_from_provider: None,
            },
        },
        stop_reason,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: pi_js::time::now_ms(),
        duration_ms: None,
    }
}

fn sample_events(result: &AssistantMessage) -> Vec<AssistantMessageEvent> {
    let tool_call = match &result.content[2] {
        AssistantContentBlock::ToolCall(tc) => tc.clone(),
        _ => panic!("missing tool call"),
    };
    let terminal = match result.stop_reason {
        StopReason::Error | StopReason::Aborted => AssistantMessageEvent::Error {
            reason: result.stop_reason,
            error: result.clone(),
        },
        StopReason::Pending => AssistantMessageEvent::Done {
            reason: StopReason::Deferred,
            message: result.clone(),
        },
        other => AssistantMessageEvent::Done {
            reason: other,
            message: result.clone(),
        },
    };
    vec![
        AssistantMessageEvent::Start {
            partial: result.clone(),
        },
        AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "text-secret".to_owned(),
            partial: result.clone(),
        },
        AssistantMessageEvent::TextEnd {
            content_index: 0,
            content: "text-secret".to_owned(),
            partial: result.clone(),
        },
        AssistantMessageEvent::ThinkingEnd {
            content_index: 1,
            content: "thinking-secret".to_owned(),
            partial: result.clone(),
        },
        AssistantMessageEvent::ToolCallDelta {
            content_index: 2,
            delta: "{\"value\":".to_owned(),
            partial: result.clone(),
        },
        AssistantMessageEvent::ToolCallDelta {
            content_index: 2,
            delta: "\"argument-secret\"}".to_owned(),
            partial: result.clone(),
        },
        AssistantMessageEvent::ToolCallEnd {
            content_index: 2,
            tool_call,
            partial: result.clone(),
        },
        terminal,
    ]
}

struct AmbientAuth;

#[async_trait]
impl ApiKeyAuth for AmbientAuth {
    fn name(&self) -> &str {
        "Test"
    }

    async fn resolve(&self, _input: ApiKeyAuthInput) -> Result<Option<AuthResult>> {
        Ok(Some(AuthResult::default()))
    }
}

fn setup(result: AssistantMessage) -> (pi_ai::models::MutableModels, Vec<AssistantMessageEvent>) {
    let models = create_models(None);
    let events = sample_events(&result);
    let ev_stream_1 = events.clone();
    let res_1 = result.clone();
    let ev_stream_2 = events.clone();
    let res_2 = result.clone();
    let ev_stream_3 = events.clone();
    let res_3 = result;

    let respond = |evs: &[AssistantMessageEvent], res: &AssistantMessage| -> AssistantMessageEventStream {
        let stream = AssistantMessageEventStream::new();
        for ev in evs {
            stream.push(ev.clone());
        }
        stream.end(Some(res.clone()));
        stream
    };

    let provider = create_provider(CreateProviderOptions {
        id: "test".to_owned(),
        name: None,
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(AmbientAuth)),
            oauth: None,
        },
        models: vec![pi_ai::types::AnyModel::Chat(test_model())],
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::Single(ProviderStreams {
            stream: Arc::new(move |_, _, _| respond(&ev_stream_1, &res_1)),
            stream_simple: Arc::new(move |_, _, _| respond(&ev_stream_2, &res_2)),
            fetch_deferred: Some(Arc::new(move |_, _, _| respond(&ev_stream_3, &res_3))),
            cancel_deferred: None,
        })),
        images: None,
        classifiers: None,
    })
    .unwrap();
    models.set_provider(provider);
    (models, events)
}

async fn settled(telemetry: &InMemoryTelemetryContext) {
    for _ in 0..200 {
        let spans = telemetry.get_spans();
        if spans.len() >= 2 && spans.iter().all(|s| s.settled) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("spans did not settle: {:?}", telemetry.get_spans());
}

fn without_duration(mut msg: AssistantMessage) -> AssistantMessage {
    msg.duration_ms = None;
    msg
}

fn without_event_durations(evs: Vec<AssistantMessageEvent>) -> Vec<AssistantMessageEvent> {
    evs.into_iter()
        .map(|ev| match ev {
            AssistantMessageEvent::Done { reason, message } => AssistantMessageEvent::Done {
                reason,
                message: without_duration(message),
            },
            AssistantMessageEvent::Error { reason, error } => AssistantMessageEvent::Error {
                reason,
                error: without_duration(error),
            },
            other => other,
        })
        .collect()
}

fn serialize_span_json(span: &pi_telemetry::RecordedTelemetrySpan) -> String {
    pi_js::json::stringify(&serde_json::json!({
        "name": span.name,
        "attributes": span.attributes,
        "events": span.events.iter().map(|e| serde_json::json!({ "name": e.name, "attributes": e.attributes })).collect::<Vec<_>>(),
        "status": format!("{:?}", span.status),
    }))
}

#[tokio::test]
async fn observes_stream_stream_simple_and_stream_deferred_without_altering_events_or_results() {
    #[derive(Clone, Copy)]
    enum Method {
        Stream,
        StreamSimple,
        StreamDeferred,
    }

    for method in [Method::Stream, Method::StreamSimple, Method::StreamDeferred] {
        let telemetry = InMemoryTelemetryContext::new();
        let expected_result = message(StopReason::Stop);
        let (models, expected_events) = setup(expected_result.clone());
        let model = test_model();
        let handle = test_handle();

        telemetry
            .start_span(
                SpanOptions {
                    name: "caller".to_owned(),
                    ..Default::default()
                },
                Arc::new(move |parent| {
                    let models = models.clone();
                    let model = model.clone();
                    let handle = handle.clone();
                    let expected_events = expected_events.clone();
                    let expected_result = expected_result.clone();
                    Box::pin(async move {
                        let parent_handle: TelemetrySpanHandle = Arc::clone(&parent);
                        let mut stream = match method {
                            Method::Stream => models.stream(
                                &model,
                                &Context {
                                    system_prompt: None,
                                    messages: vec![],
                                    tools: None,
                                },
                                Some(ModelsApiStreamOptions {
                                    base: ApiStreamOptions {
                                        base: StreamOptions {
                                            telemetry: Some(parent_handle),
                                            ..Default::default()
                                        },
                                        ..Default::default()
                                    },
                                    transform_headers: None,
                                }),
                            ),
                            Method::StreamSimple => models.stream_simple(
                                &model,
                                &Context {
                                    system_prompt: None,
                                    messages: vec![],
                                    tools: None,
                                },
                                Some(ModelsSimpleStreamOptions {
                                    base: SimpleStreamOptions {
                                        base: StreamOptions {
                                            telemetry: Some(parent_handle),
                                            ..Default::default()
                                        },
                                        reasoning: Some(ThinkingLevel::High),
                                        ..Default::default()
                                    },
                                    transform_headers: None,
                                }),
                            ),
                            Method::StreamDeferred => models.stream_deferred(
                                &model,
                                &handle,
                                Some(ModelsDeferredFetchOptions {
                                    base: DeferredFetchOptions {
                                        base: ProviderRequestOptions {
                                            telemetry: Some(parent_handle),
                                            ..Default::default()
                                        },
                                        wait: None,
                                    },
                                    transform_headers: None,
                                }),
                            ),
                        };

                        let mut received = Vec::new();
                        while let Some(ev) = stream.next().await {
                            received.push(ev);
                        }
                        assert_eq!(without_event_durations(received), expected_events);
                        assert_eq!(without_duration(stream.result().await), expected_result);
                        Ok(Box::new(()) as pi_telemetry::SpanValue)
                    })
                }),
            )
            .await
            .unwrap();

        settled(&telemetry).await;
        let spans = telemetry.get_spans();
        assert_eq!(spans.len(), 2);
        let parent = &spans[0];
        let span = &spans[1];
        assert_eq!(
            span.name,
            if matches!(method, Method::StreamDeferred) {
                "pi.ai.poll"
            } else {
                "pi.ai.request"
            }
        );
        assert_eq!(span.parent_id, Some(parent.id));

        let mut expected_attrs: IndexMap<String, AttributeValue> = IndexMap::new();
        expected_attrs.insert("provider".to_owned(), AttributeValue::String("test".to_owned()));
        expected_attrs.insert("api".to_owned(), AttributeValue::String("telemetry-test".to_owned()));
        expected_attrs.insert("model".to_owned(), AttributeValue::String("model".to_owned()));
        if matches!(method, Method::StreamSimple) {
            expected_attrs.insert("reasoning".to_owned(), AttributeValue::String("high".to_owned()));
        }
        expected_attrs.insert("stop".to_owned(), AttributeValue::String("stop".to_owned()));
        expected_attrs.insert("usage.input".to_owned(), AttributeValue::Number(11.0));
        expected_attrs.insert("usage.output".to_owned(), AttributeValue::Number(22.0));
        expected_attrs.insert("usage.cache_read".to_owned(), AttributeValue::Number(33.0));
        expected_attrs.insert("usage.cache_write".to_owned(), AttributeValue::Number(44.0));
        expected_attrs.insert("cost.total".to_owned(), AttributeValue::Number(1.0));
        expected_attrs.insert("response.id".to_owned(), AttributeValue::String("response".to_owned()));
        assert_eq!(span.attributes, expected_attrs);

        assert_eq!(span.events.len(), 4);
        assert_eq!(span.events[0].name, "pi.ai.first_chunk");
        assert!(span.events[0].attributes.is_empty());
        assert_eq!(span.events[1].name, "pi.ai.block");
        assert_eq!(
            span.events[1].attributes.get("type"),
            Some(&AttributeValue::String("text".to_owned()))
        );
        assert_eq!(
            span.events[1].attributes.get("index"),
            Some(&AttributeValue::Number(0.0))
        );
        assert_eq!(
            span.events[1].attributes.get("chars"),
            Some(&AttributeValue::Number(11.0))
        );
        assert_eq!(
            span.events[2].attributes.get("type"),
            Some(&AttributeValue::String("thinking".to_owned()))
        );
        assert_eq!(
            span.events[2].attributes.get("index"),
            Some(&AttributeValue::Number(1.0))
        );
        assert_eq!(
            span.events[2].attributes.get("chars"),
            Some(&AttributeValue::Number(15.0))
        );
        assert_eq!(
            span.events[3].attributes.get("type"),
            Some(&AttributeValue::String("tool_call".to_owned()))
        );
        assert_eq!(
            span.events[3].attributes.get("index"),
            Some(&AttributeValue::Number(2.0))
        );
        assert_eq!(
            span.events[3].attributes.get("chars"),
            Some(&AttributeValue::Number(27.0))
        );
        assert_eq!(
            span.events[3].attributes.get("tool"),
            Some(&AttributeValue::String("lookup".to_owned()))
        );

        let serialized = serialize_span_json(span);
        for secret in [
            "text-secret",
            "thinking-secret",
            "argument-secret",
            "call-secret",
            "deferred-secret",
        ] {
            assert!(!serialized.contains(secret), "leaked {secret}");
        }

        let measurements = telemetry.get_metrics();
        assert_eq!(measurements.len(), 7);
        let counter_vals: Vec<f64> = measurements
            .iter()
            .filter(|m| m.kind == MetricKind::Counter)
            .map(|m| m.value)
            .collect();
        assert_eq!(counter_vals, vec![11.0, 22.0, 33.0, 44.0, 1.0]);
        for m in &measurements {
            let mut keys: Vec<&str> = m.attributes.keys().map(String::as_str).collect();
            keys.sort_unstable();
            if m.name == "pi.ai.tokens" {
                assert_eq!(keys, vec!["kind", "model", "provider"]);
            } else if m.name == "pi.ai.request.duration" {
                assert_eq!(keys, vec!["model", "provider", "stop"]);
            } else {
                assert_eq!(keys, vec!["model", "provider"]);
            }
            if m.kind == MetricKind::Histogram {
                assert_eq!(m.context.as_ref(), Some(&span.context));
            }
        }
    }
}

#[tokio::test]
async fn reports_error_and_aborted_statuses_with_bounded_messages_and_no_secret_leaks() {
    // 1. Error vs Aborted status
    for reason in [StopReason::Error, StopReason::Aborted] {
        let telemetry = InMemoryTelemetryContext::new();
        let mut expected_result = message(reason);
        expected_result.error_message = Some("error-secret".to_owned());
        let (models, _) = setup(expected_result.clone());
        let model = test_model();

        let delivered = telemetry
            .start_span(
                SpanOptions {
                    name: "caller".to_owned(),
                    ..Default::default()
                },
                Arc::new(move |span| {
                    let models = models.clone();
                    let model = model.clone();
                    Box::pin(async move {
                        let out = models
                            .complete_simple(
                                &model,
                                &Context {
                                    system_prompt: None,
                                    messages: vec![],
                                    tools: None,
                                },
                                Some(ModelsSimpleStreamOptions {
                                    base: SimpleStreamOptions {
                                        base: StreamOptions {
                                            telemetry: Some(span),
                                            ..Default::default()
                                        },
                                        ..Default::default()
                                    },
                                    transform_headers: None,
                                }),
                            )
                            .await;
                        Ok(Box::new(out) as pi_telemetry::SpanValue)
                    })
                }),
            )
            .await
            .unwrap();
        let delivered = delivered.downcast::<AssistantMessage>().unwrap();
        assert_eq!(without_duration(*delivered), expected_result);
        settled(&telemetry).await;

        let span = &telemetry.get_spans()[1];
        if reason == StopReason::Error {
            assert_eq!(
                span.status,
                SpanStatus::Error {
                    error: Some(SpanStatusError {
                        name: "provider_error".to_owned(),
                        message: "error-secret".to_owned(),
                    }),
                }
            );
        } else {
            assert_eq!(span.status, SpanStatus::Ok);
        }
        assert_eq!(
            span.attributes.get("stop"),
            Some(&AttributeValue::String(
                if reason == StopReason::Error {
                    "error"
                } else {
                    "aborted"
                }
                .to_owned()
            ))
        );
        let attrs_json = pi_js::json::stringify(&span.attributes);
        assert!(!attrs_json.contains("error-secret"));
        if reason == StopReason::Aborted {
            assert!(!serialize_span_json(span).contains("error-secret"));
        }
    }

    // 2. Bounds poll error messages to 500 chars including huge 4MB messages and missing messages.
    for msg_len in [0usize, 4_000_000] {
        let telemetry = InMemoryTelemetryContext::new();
        let mut expected_result = message(StopReason::Error);
        if msg_len > 0 {
            expected_result.error_message = Some(format!("{}discarded-tail", "x".repeat(msg_len)));
        }
        let (models, _) = setup(expected_result.clone());
        let model = test_model();
        let handle = test_handle();

        telemetry
            .start_span(
                SpanOptions {
                    name: "caller".to_owned(),
                    ..Default::default()
                },
                Arc::new(move |span| {
                    let models = models.clone();
                    let model = model.clone();
                    let handle = handle.clone();
                    Box::pin(async move {
                        let _ = models
                            .fetch_deferred(
                                &model,
                                &handle,
                                Some(ModelsDeferredFetchOptions {
                                    base: DeferredFetchOptions {
                                        base: ProviderRequestOptions {
                                            telemetry: Some(span),
                                            ..Default::default()
                                        },
                                        wait: None,
                                    },
                                    transform_headers: None,
                                }),
                            )
                            .await;
                        Ok(Box::new(()) as pi_telemetry::SpanValue)
                    })
                }),
            )
            .await
            .unwrap();
        settled(&telemetry).await;

        let span = &telemetry.get_spans()[1];
        let expected_err = if msg_len == 0 { String::new() } else { "x".repeat(500) };
        assert_eq!(
            span.status,
            SpanStatus::Error {
                error: Some(SpanStatusError {
                    name: "provider_error".to_owned(),
                    message: expected_err,
                }),
            }
        );
        assert!(!serialize_span_json(span).contains("discarded-tail"));
    }

    // 3. Includes setup failures on unknown providers.
    {
        let telemetry = InMemoryTelemetryContext::new();
        let models = create_models(None);
        let model = test_model();
        let out = telemetry
            .start_span(
                SpanOptions {
                    name: "caller".to_owned(),
                    ..Default::default()
                },
                Arc::new(move |span| {
                    let models = models.clone();
                    let model = model.clone();
                    Box::pin(async move {
                        let res = models
                            .complete(
                                &model,
                                &Context {
                                    system_prompt: None,
                                    messages: vec![],
                                    tools: None,
                                },
                                Some(ModelsApiStreamOptions {
                                    base: ApiStreamOptions {
                                        base: StreamOptions {
                                            telemetry: Some(span),
                                            ..Default::default()
                                        },
                                        ..Default::default()
                                    },
                                    transform_headers: None,
                                }),
                            )
                            .await;
                        Ok(Box::new(res) as pi_telemetry::SpanValue)
                    })
                }),
            )
            .await
            .unwrap()
            .downcast::<AssistantMessage>()
            .unwrap();
        assert_eq!(out.stop_reason, StopReason::Error);
        settled(&telemetry).await;
        let span = &telemetry.get_spans()[1];
        assert!(span.attributes.get("reasoning").is_none());
        assert_eq!(span.events.len(), 1);
        assert_eq!(span.events[0].name, "pi.ai.first_chunk");
        assert_eq!(
            span.status,
            SpanStatus::Error {
                error: Some(SpanStatusError {
                    name: "provider_error".to_owned(),
                    message: "Unknown provider: test".to_owned(),
                }),
            }
        );
    }
}

#[tokio::test]
async fn interleaved_tool_deltas_tracing_only_noop_metrics_and_instrument_caching() {
    // 1. Counts interleaved tool deltas independently.
    {
        let telemetry = InMemoryTelemetryContext::new();
        let res = message(StopReason::Stop);
        let model = test_model();
        telemetry
            .start_span(
                SpanOptions {
                    name: "caller".to_owned(),
                    ..Default::default()
                },
                Arc::new(move |span| {
                    let model = model.clone();
                    let res = res.clone();
                    Box::pin(async move {
                        let observer = observe_model_request(&model, Some(&span), None, None, false);
                        let tc = ToolCall::new("call", "lookup", JsonObject::new());
                        for (index, delta) in [(2usize, "ab"), (3, "123"), (2, "c")] {
                            observer.on_event(&AssistantMessageEvent::ToolCallDelta {
                                content_index: index,
                                delta: delta.to_owned(),
                                partial: res.clone(),
                            });
                        }
                        for index in [3usize, 2, 4] {
                            observer.on_event(&AssistantMessageEvent::ToolCallEnd {
                                content_index: index,
                                tool_call: tc.clone(),
                                partial: res.clone(),
                            });
                        }
                        observer.on_result(Some(&res));
                        for _ in 0..5 {
                            tokio::task::yield_now().await;
                        }
                        Ok(Box::new(()) as pi_telemetry::SpanValue)
                    })
                }),
            )
            .await
            .unwrap();
        settled(&telemetry).await;
        let block_events = &telemetry.get_spans()[1].events[1..];
        assert_eq!(block_events.len(), 3);
        let summary: Vec<(f64, f64, &str)> = block_events
            .iter()
            .map(|e| {
                (
                    match e.attributes.get("index") {
                        Some(AttributeValue::Number(n)) => *n,
                        _ => -1.0,
                    },
                    match e.attributes.get("chars") {
                        Some(AttributeValue::Number(n)) => *n,
                        _ => -1.0,
                    },
                    match e.attributes.get("tool") {
                        Some(AttributeValue::String(s)) => s.as_str(),
                        _ => "",
                    },
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![(3.0, 3.0, "lookup"), (2.0, 3.0, "lookup"), (4.0, 0.0, "lookup")]
        );
    }

    // 2. Supports tracing-only span without metric capabilities.
    {
        struct TracingOnlySpan(TelemetrySpanHandle);
        #[async_trait]
        impl TelemetryContext for TracingOnlySpan {
            fn metrics(&self) -> Option<Arc<dyn TelemetryMetrics + Send + Sync>> {
                None
            }
            async fn start_span(&self, options: SpanOptions, callback: SpanCallback) -> SpanResult {
                self.0.start_span(options, callback).await
            }
        }
        impl TelemetrySpan for TracingOnlySpan {
            fn add_link(&self, link: SpanLink) {
                self.0.add_link(link);
            }
            fn add_event(&self, name: &str, attributes: Option<SpanAttributes>) {
                self.0.add_event(name, attributes);
            }
            fn set_attributes(&self, attributes: SpanAttributes) {
                self.0.set_attributes(attributes);
            }
            fn set_status(&self, status: SpanStatus) {
                self.0.set_status(status);
            }
        }

        let adapter = InMemoryTelemetryContext::new();
        let (models, _) = setup(message(StopReason::Stop));
        let model = test_model();
        adapter
            .start_span(
                SpanOptions {
                    name: "caller".to_owned(),
                    ..Default::default()
                },
                Arc::new(move |span| {
                    let models = models.clone();
                    let model = model.clone();
                    Box::pin(async move {
                        let tracing_only: TelemetrySpanHandle = Arc::new(TracingOnlySpan(span));
                        let _ = models
                            .complete_simple(
                                &model,
                                &Context {
                                    system_prompt: None,
                                    messages: vec![],
                                    tools: None,
                                },
                                Some(ModelsSimpleStreamOptions {
                                    base: SimpleStreamOptions {
                                        base: StreamOptions {
                                            telemetry: Some(tracing_only),
                                            ..Default::default()
                                        },
                                        ..Default::default()
                                    },
                                    transform_headers: None,
                                }),
                            )
                            .await;
                        Ok(Box::new(()) as pi_telemetry::SpanValue)
                    })
                }),
            )
            .await
            .unwrap();
        settled(&adapter).await;
        assert_eq!(adapter.get_spans().len(), 2);
        assert!(adapter.get_metrics().is_empty());
    }

    // 3. Defaults to no-op telemetry and records schema invariants.
    {
        let (models, _) = setup(message(StopReason::Stop));
        let out = models
            .complete_simple(
                &test_model(),
                &Context {
                    system_prompt: None,
                    messages: vec![],
                    tools: None,
                },
                None,
            )
            .await;
        assert_eq!(out.stop_reason, StopReason::Stop);
        let req_span = AI_TELEMETRY_SCHEMA.spans.get("pi.ai.request").unwrap();
        assert!(matches!(
            req_span.start_attributes.get("provider"),
            Some(pi_telemetry::TelemetryStartAttributeDefinition::String { required: true, .. })
        ));
    }

    // 4. Records usage for polls only when terminal.
    for reason in [
        StopReason::Pending,
        StopReason::Deferred,
        StopReason::Stop,
        StopReason::Error,
    ] {
        let telemetry = InMemoryTelemetryContext::new();
        let (models, _) = setup(message(reason));
        let model = test_model();
        let handle = test_handle();
        telemetry
            .start_span(
                SpanOptions {
                    name: "caller".to_owned(),
                    ..Default::default()
                },
                Arc::new(move |span| {
                    let models = models.clone();
                    let model = model.clone();
                    let handle = handle.clone();
                    Box::pin(async move {
                        let _ = models
                            .fetch_deferred(
                                &model,
                                &handle,
                                Some(ModelsDeferredFetchOptions {
                                    base: DeferredFetchOptions {
                                        base: ProviderRequestOptions {
                                            telemetry: Some(span),
                                            ..Default::default()
                                        },
                                        wait: None,
                                    },
                                    transform_headers: None,
                                }),
                            )
                            .await;
                        Ok(Box::new(()) as pi_telemetry::SpanValue)
                    })
                }),
            )
            .await
            .unwrap();
        settled(&telemetry).await;
        let counters = telemetry
            .get_metrics()
            .into_iter()
            .filter(|m| m.kind == MetricKind::Counter)
            .count();
        assert_eq!(
            counters,
            if matches!(reason, StopReason::Pending | StopReason::Deferred) {
                0
            } else {
                5
            }
        );
    }

    // 5. Monotonic non-negative durations & metric attributes/units.
    {
        let telemetry = InMemoryTelemetryContext::new();
        let res = message(StopReason::Stop);
        let model = test_model();
        telemetry
            .start_span(
                SpanOptions {
                    name: "caller".to_owned(),
                    ..Default::default()
                },
                Arc::new(move |span| {
                    let model = model.clone();
                    let res = res.clone();
                    Box::pin(async move {
                        let observer = observe_model_request(&model, Some(&span), None, None, false);
                        tokio::time::sleep(Duration::from_millis(5)).await;
                        observer.on_event(&AssistantMessageEvent::Start { partial: res.clone() });
                        tokio::time::sleep(Duration::from_millis(5)).await;
                        observer.on_result(Some(&res));
                        for _ in 0..5 {
                            tokio::task::yield_now().await;
                        }
                        Ok(Box::new(()) as pi_telemetry::SpanValue)
                    })
                }),
            )
            .await
            .unwrap();
        settled(&telemetry).await;
        let metrics: Vec<RecordedTelemetryMeasurement> = telemetry.get_metrics();
        assert_eq!(metrics.len(), 7);
        assert_eq!(metrics[0].name, "pi.ai.first_chunk.duration");
        assert_eq!(metrics[0].unit.as_deref(), Some("ms"));
        assert_eq!(metrics[1].name, "pi.ai.request.duration");
        assert_eq!(metrics[1].unit.as_deref(), Some("ms"));
        assert!(metrics[1].value >= metrics[0].value && metrics[0].value >= 0.0);
        assert!(metrics[0].context.is_some() && metrics[1].context.is_some());
    }

    // 6. Creates instruments once per metrics adapter instance across multiple requests.
    {
        struct SpyMetrics {
            inner: Arc<dyn TelemetryMetrics + Send + Sync>,
            counters: Arc<Mutex<Vec<String>>>,
            histograms: Arc<Mutex<Vec<String>>>,
        }
        impl TelemetryMetrics for SpyMetrics {
            fn counter(
                &self,
                name: &str,
                options: Option<InstrumentOptions>,
            ) -> Arc<dyn TelemetryCounter + Send + Sync> {
                self.counters.lock().unwrap().push(name.to_owned());
                self.inner.counter(name, options)
            }
            fn up_down_counter(
                &self,
                name: &str,
                options: Option<InstrumentOptions>,
            ) -> Arc<dyn TelemetryUpDownCounter + Send + Sync> {
                self.inner.up_down_counter(name, options)
            }
            fn histogram(
                &self,
                name: &str,
                options: Option<HistogramOptions>,
            ) -> Arc<dyn TelemetryHistogram + Send + Sync> {
                self.histograms.lock().unwrap().push(name.to_owned());
                self.inner.histogram(name, options)
            }
        }
        struct SpySpan {
            inner: TelemetrySpanHandle,
            metrics: Arc<dyn TelemetryMetrics + Send + Sync>,
        }
        #[async_trait]
        impl TelemetryContext for SpySpan {
            fn metrics(&self) -> Option<Arc<dyn TelemetryMetrics + Send + Sync>> {
                Some(Arc::clone(&self.metrics))
            }
            async fn start_span(&self, options: SpanOptions, callback: SpanCallback) -> SpanResult {
                self.inner.start_span(options, callback).await
            }
        }
        impl TelemetrySpan for SpySpan {
            fn add_link(&self, link: SpanLink) {
                self.inner.add_link(link);
            }
            fn add_event(&self, name: &str, attributes: Option<SpanAttributes>) {
                self.inner.add_event(name, attributes);
            }
            fn set_attributes(&self, attributes: SpanAttributes) {
                self.inner.set_attributes(attributes);
            }
            fn set_status(&self, status: SpanStatus) {
                self.inner.set_status(status);
            }
        }

        let telemetry = InMemoryTelemetryContext::new();
        let spy_counters = Arc::new(Mutex::new(Vec::new()));
        let spy_histograms = Arc::new(Mutex::new(Vec::new()));
        let spy_metrics: Arc<dyn TelemetryMetrics + Send + Sync> = Arc::new(SpyMetrics {
            inner: telemetry.metrics().unwrap(),
            counters: Arc::clone(&spy_counters),
            histograms: Arc::clone(&spy_histograms),
        });
        let (models, _) = setup(message(StopReason::Stop));
        for call in 0..3 {
            let models = models.clone();
            let metrics = Arc::clone(&spy_metrics);
            telemetry
                .start_span(
                    SpanOptions {
                        name: "caller".to_owned(),
                        ..Default::default()
                    },
                    Arc::new(move |span| {
                        let models = models.clone();
                        let spy: TelemetrySpanHandle = Arc::new(SpySpan {
                            inner: span,
                            metrics: Arc::clone(&metrics),
                        });
                        Box::pin(async move {
                            let model = test_model();
                            let handle = test_handle();
                            match call {
                                0 => {
                                    let _ = models
                                        .complete_simple(
                                            &model,
                                            &Context {
                                                system_prompt: None,
                                                messages: vec![],
                                                tools: None,
                                            },
                                            Some(ModelsSimpleStreamOptions {
                                                base: SimpleStreamOptions {
                                                    base: StreamOptions {
                                                        telemetry: Some(spy),
                                                        ..Default::default()
                                                    },
                                                    ..Default::default()
                                                },
                                                transform_headers: None,
                                            }),
                                        )
                                        .await;
                                }
                                1 => {
                                    let _ = models
                                        .fetch_deferred(
                                            &model,
                                            &handle,
                                            Some(ModelsDeferredFetchOptions {
                                                base: DeferredFetchOptions {
                                                    base: ProviderRequestOptions {
                                                        telemetry: Some(spy),
                                                        ..Default::default()
                                                    },
                                                    wait: None,
                                                },
                                                transform_headers: None,
                                            }),
                                        )
                                        .await;
                                }
                                _ => {
                                    let _ = models
                                        .complete(
                                            &model,
                                            &Context {
                                                system_prompt: None,
                                                messages: vec![],
                                                tools: None,
                                            },
                                            Some(ModelsApiStreamOptions {
                                                base: ApiStreamOptions {
                                                    base: StreamOptions {
                                                        telemetry: Some(spy),
                                                        ..Default::default()
                                                    },
                                                    ..Default::default()
                                                },
                                                transform_headers: None,
                                            }),
                                        )
                                        .await;
                                }
                            }
                            Ok(Box::new(()) as pi_telemetry::SpanValue)
                        })
                    }),
                )
                .await
                .unwrap();
        }
        settled(&telemetry).await;
        assert_eq!(telemetry.get_metrics().len(), 21);
        // PORT: In Rust `observe_model_request` creates a `TypedTelemetryMetrics` wrapper per request (no global
        // WeakMap cache across `Arc<dyn TelemetryMetrics>` trait objects), registering each instrument once per call.
        assert_eq!(
            *spy_histograms.lock().unwrap(),
            vec![
                "pi.ai.request.duration",
                "pi.ai.first_chunk.duration",
                "pi.ai.request.duration",
                "pi.ai.first_chunk.duration",
                "pi.ai.request.duration",
                "pi.ai.first_chunk.duration",
            ]
        );
        assert_eq!(
            *spy_counters.lock().unwrap(),
            vec![
                "pi.ai.tokens",
                "pi.ai.cost",
                "pi.ai.tokens",
                "pi.ai.cost",
                "pi.ai.tokens",
                "pi.ai.cost",
            ]
        );
    }
}

#[tokio::test]
async fn lazy_stream_observation_delivers_events_before_result_once() {
    let model = test_model();
    let expected = message(StopReason::Stop);

    // 1. Pushes events before observing terminal result, once per terminal result.
    {
        let order = Arc::new(Mutex::new(Vec::<String>::new()));
        let order_ev = Arc::clone(&order);
        let order_res = Arc::clone(&order);
        let expected_res = expected.clone();
        let src_msg = expected.clone();
        let result_calls = Arc::new(AtomicUsize::new(0));
        let rc = Arc::clone(&result_calls);

        let stream = lazy_stream_with_options(
            &model,
            async move {
                let s = AssistantMessageEventStream::new();
                s.push(AssistantMessageEvent::Done {
                    reason: StopReason::Stop,
                    message: src_msg.clone(),
                });
                s.end(Some(src_msg));
                Ok(s)
            },
            Some(LazyStreamObserver {
                on_event: Some(Arc::new(move |_| {
                    order_ev.lock().unwrap().push("event".to_owned());
                })),
                on_result: Some(Arc::new(move |received| {
                    rc.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(received.cloned().map(without_duration), Some(expected_res.clone()));
                    order_res.lock().unwrap().push("result".to_owned());
                })),
            }),
            None,
        );

        assert_eq!(without_duration(stream.result().await), expected);
        for _ in 0..50 {
            if result_calls.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(result_calls.load(Ordering::SeqCst), 1);
        assert_eq!(*order.lock().unwrap(), vec!["event", "result"]);
    }

    // 2. Reports terminal result from event stream even when source.end(None) omits payload.
    {
        let observed = Arc::new(Mutex::new(Vec::<Option<AssistantMessage>>::new()));
        let obs_c = Arc::clone(&observed);
        let src_msg = expected.clone();
        let stream = lazy_stream_with_options(
            &model,
            async move {
                let s = AssistantMessageEventStream::new();
                s.push(AssistantMessageEvent::Done {
                    reason: StopReason::Stop,
                    message: src_msg,
                });
                s.end(None);
                Ok(s)
            },
            Some(LazyStreamObserver {
                on_event: None,
                on_result: Some(Arc::new(move |val| {
                    obs_c.lock().unwrap().push(val.cloned());
                })),
            }),
            None,
        );
        let got = stream.result().await;
        for _ in 0..50 {
            if !observed.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(without_duration(got), expected);
        let obs_norm: Vec<Option<AssistantMessage>> = observed
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .map(|o| o.map(without_duration))
            .collect();
        assert_eq!(obs_norm, vec![Some(expected.clone())]);
    }

    // 3. Reports setup failure events and error result once.
    {
        let observed = Arc::new(Mutex::new(Vec::<String>::new()));
        let obs_ev = Arc::clone(&observed);
        let obs_res = Arc::clone(&observed);
        let stream = lazy_stream_with_options(
            &model,
            async { Err::<AssistantMessageEventStream, _>(Error::msg("setup-secret")) },
            Some(LazyStreamObserver {
                on_event: Some(Arc::new(move |ev| {
                    let kind = match ev {
                        AssistantMessageEvent::Error { .. } => "error",
                        _ => "other",
                    };
                    obs_ev.lock().unwrap().push(kind.to_owned());
                })),
                on_result: Some(Arc::new(move |res| {
                    obs_res.lock().unwrap().push(match res.map(|r| r.stop_reason) {
                        Some(StopReason::Error) => "error".to_owned(),
                        _ => "other".to_owned(),
                    });
                })),
            }),
            None,
        );
        let out = stream.result().await;
        assert_eq!(out.error_message.as_deref(), Some("setup-secret"));
        assert_eq!(*observed.lock().unwrap(), vec!["error", "error"]);
    }
}
