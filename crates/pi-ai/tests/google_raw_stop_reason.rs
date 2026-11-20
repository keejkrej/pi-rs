//! Port of packages/ai/test/google-raw-stop-reason.test.ts

use std::sync::{Arc, Mutex};

use pi_ai::api::google_generative_ai::{GoogleOptions, stream as stream_google_generative_ai};
use pi_ai::api::google_vertex::{GoogleVertexOptions, stream as stream_google_vertex, testing::mock_adc_token};
use pi_ai::providers::google_models::google_models;
use pi_ai::providers::google_vertex_models::google_vertex_models;
use pi_ai::utils::transcript::normalize_context;
use pi_ai::{AssistantContentBlock, Context, Message, Model, ProviderHeaders, StopReason, StreamOptions, UserMessage};
use pi_js::BoxFuture;
use pi_js::fetch::testing::response;
use pi_js::fetch::{Headers, testing::mock_fetch};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

fn sample_context() -> Context {
    Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: "hello".into(),
            timestamp: pi_js::time::now_ms(),
        })],
        tools: None,
    }
}

fn pi_user_agent() -> String {
    pi_ai::utils::identity::format_user_agent(None)
}

fn sse_body_from_chunks(chunks: &[Value]) -> String {
    let mut out = String::new();
    for chunk in chunks {
        out.push_str("data: ");
        out.push_str(&pi_js::json::stringify(chunk));
        out.push_str("\r\n\r\n");
    }
    out
}

fn make_finish_chunk(finish_reason: &str, include_function_call: bool) -> Value {
    let content = if include_function_call {
        json!({
            "parts": [
                {
                    "functionCall": {
                        "id": "call-1",
                        "name": "echo",
                        "args": { "value": "truncated" }
                    }
                }
            ]
        })
    } else {
        Value::Null
    };

    let mut candidate = serde_json::Map::new();
    candidate.insert("finishReason".to_owned(), json!(finish_reason));
    if !content.is_null() {
        candidate.insert("content".to_owned(), content);
    }

    json!({
        "responseId": "google-response-id",
        "candidates": [Value::Object(candidate)],
        "usageMetadata": {
            "promptTokenCount": 1,
            "candidatesTokenCount": 0,
            "totalTokenCount": 1
        }
    })
}

async fn capture_google_headers(headers: Option<ProviderHeaders>) -> Headers {
    let captured: Arc<Mutex<Option<Headers>>> = Arc::new(Mutex::new(None));
    let captured_ref = Arc::clone(&captured);
    let sse = sse_body_from_chunks(&[make_finish_chunk("STOP", false)]);

    let _fetch_guard = mock_fetch(move |req| {
        let slot = Arc::clone(&captured_ref);
        let body = sse.clone();
        Box::pin(async move {
            *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(req.headers.clone());
            Ok(response(200, &[("content-type", "text/event-stream")], body))
        })
    });

    let model = google_models()
        .get("gemini-2.5-flash")
        .expect("gemini-2.5-flash model should exist")
        .clone();
    let opts = GoogleOptions {
        base: StreamOptions {
            api_key: Some("test-api-key".to_owned()),
            headers,
            ..Default::default()
        },
        ..Default::default()
    };

    let _ = stream_google_generative_ai(&model, &normalize_context(&sample_context()), Some(opts.into()))
        .result()
        .await;

    captured
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
        .expect("fetch should have captured headers")
}

#[tokio::test]
async fn preserves_raw_gemini_finish_reasons_for_google_generative_ai_errors() {
    let sse = sse_body_from_chunks(&[make_finish_chunk("MALFORMED_FUNCTION_CALL", false)]);
    let _fetch_guard = mock_fetch(move |_req| {
        let body = sse.clone();
        Box::pin(async move { Ok(response(200, &[("content-type", "text/event-stream")], body)) })
    });

    let model = google_models()
        .get("gemini-2.5-flash")
        .expect("gemini-2.5-flash should exist")
        .clone();
    let opts = GoogleOptions {
        base: StreamOptions {
            api_key: Some("test-api-key".to_owned()),
            ..Default::default()
        },
        ..Default::default()
    };

    let message = stream_google_generative_ai(&model, &normalize_context(&sample_context()), Some(opts.into()))
        .result()
        .await;

    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("MALFORMED_FUNCTION_CALL"));
    assert_eq!(
        message.error_message.as_deref(),
        Some("Provider stopped with: MALFORMED_FUNCTION_CALL")
    );
}

#[tokio::test]
async fn preserves_raw_gemini_finish_reasons_for_google_vertex_errors() {
    let _adc_guard = mock_adc_token("test-vertex-token");
    let sse = sse_body_from_chunks(&[make_finish_chunk("SAFETY", false)]);
    let _fetch_guard = mock_fetch(move |_req| {
        let body = sse.clone();
        Box::pin(async move { Ok(response(200, &[("content-type", "text/event-stream")], body)) })
    });

    let model = google_vertex_models()
        .get("gemini-3-flash-preview")
        .expect("gemini-3-flash-preview should exist")
        .clone();
    let opts = GoogleVertexOptions {
        project: Some("test-project".to_owned()),
        location: Some("us-central1".to_owned()),
        ..Default::default()
    };

    let message = stream_google_vertex(&model, &normalize_context(&sample_context()), Some(opts.into()))
        .result()
        .await;

    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("SAFETY"));
    assert_eq!(message.error_message.as_deref(), Some("Provider stopped with: SAFETY"));
}

#[tokio::test]
async fn preserves_max_tokens_with_tool_call_as_length_for_both_adapters() {
    let google_model = google_models()
        .get("gemini-2.5-flash")
        .expect("gemini-2.5-flash")
        .clone();
    let vertex_model = google_vertex_models()
        .get("gemini-3-flash-preview")
        .expect("gemini-3-flash-preview")
        .clone();
    let ctx = normalize_context(&sample_context());

    let _adc_guard = mock_adc_token("test-vertex-token");
    let sse = sse_body_from_chunks(&[make_finish_chunk("MAX_TOKENS", true)]);
    let _fetch_guard = mock_fetch(move |_req| {
        let body = sse.clone();
        Box::pin(async move { Ok(response(200, &[("content-type", "text/event-stream")], body)) })
    });

    let g_msg = stream_google_generative_ai(
        &google_model,
        &ctx,
        Some(
            GoogleOptions {
                base: StreamOptions {
                    api_key: Some("test-api-key".to_owned()),
                    ..Default::default()
                },
                ..Default::default()
            }
            .into(),
        ),
    )
    .result()
    .await;
    assert_eq!(g_msg.stop_reason, StopReason::Length);
    assert_eq!(g_msg.raw_stop_reason.as_deref(), Some("MAX_TOKENS"));
    assert!(
        g_msg
            .content
            .iter()
            .any(|b| matches!(b, AssistantContentBlock::ToolCall(_)))
    );

    let v_msg = stream_google_vertex(
        &vertex_model,
        &ctx,
        Some(
            GoogleVertexOptions {
                project: Some("test-project".to_owned()),
                location: Some("us-central1".to_owned()),
                ..Default::default()
            }
            .into(),
        ),
    )
    .result()
    .await;
    assert_eq!(v_msg.stop_reason, StopReason::Length);
    assert_eq!(v_msg.raw_stop_reason.as_deref(), Some("MAX_TOKENS"));
    assert!(
        v_msg
            .content
            .iter()
            .any(|b| matches!(b, AssistantContentBlock::ToolCall(_)))
    );
}

#[tokio::test]
async fn maps_stop_with_tool_call_to_tool_use_for_both_adapters() {
    let google_model = google_models()
        .get("gemini-2.5-flash")
        .expect("gemini-2.5-flash")
        .clone();
    let vertex_model = google_vertex_models()
        .get("gemini-3-flash-preview")
        .expect("gemini-3-flash-preview")
        .clone();
    let ctx = normalize_context(&sample_context());

    let _adc_guard = mock_adc_token("test-vertex-token");
    let sse = sse_body_from_chunks(&[make_finish_chunk("STOP", true)]);
    let _fetch_guard = mock_fetch(move |_req| {
        let body = sse.clone();
        Box::pin(async move { Ok(response(200, &[("content-type", "text/event-stream")], body)) })
    });

    let g_msg = stream_google_generative_ai(
        &google_model,
        &ctx,
        Some(
            GoogleOptions {
                base: StreamOptions {
                    api_key: Some("test-api-key".to_owned()),
                    ..Default::default()
                },
                ..Default::default()
            }
            .into(),
        ),
    )
    .result()
    .await;
    assert_eq!(g_msg.stop_reason, StopReason::ToolUse);
    assert_eq!(g_msg.raw_stop_reason.as_deref(), Some("STOP"));
    assert!(
        g_msg
            .content
            .iter()
            .any(|b| matches!(b, AssistantContentBlock::ToolCall(_)))
    );

    let v_msg = stream_google_vertex(
        &vertex_model,
        &ctx,
        Some(
            GoogleVertexOptions {
                project: Some("test-project".to_owned()),
                location: Some("us-central1".to_owned()),
                ..Default::default()
            }
            .into(),
        ),
    )
    .result()
    .await;
    assert_eq!(v_msg.stop_reason, StopReason::ToolUse);
    assert_eq!(v_msg.raw_stop_reason.as_deref(), Some("STOP"));
    assert!(
        v_msg
            .content
            .iter()
            .any(|b| matches!(b, AssistantContentBlock::ToolCall(_)))
    );
}

#[tokio::test]
async fn forwards_each_sdk_chunk_in_order_before_normalizing_for_both_adapters() {
    let chunks = vec![
        json!({
            "responseId": "resp_google",
            "candidates": [{ "content": { "parts": [{ "text": "hello" }] } }]
        }),
        json!({
            "candidates": [{ "finishReason": "STOP" }],
            "usageMetadata": { "promptTokenCount": 2, "candidatesTokenCount": 1, "totalTokenCount": 3 }
        }),
    ];
    let sse = sse_body_from_chunks(&chunks);
    let _adc_guard = mock_adc_token("test-vertex-token");
    let _fetch_guard = mock_fetch(move |_req| {
        let body = sse.clone();
        Box::pin(async move { Ok(response(200, &[("content-type", "text/event-stream")], body)) })
    });

    let google_model = google_models()
        .get("gemini-2.5-flash")
        .expect("gemini-2.5-flash")
        .clone();
    let vertex_model = google_vertex_models()
        .get("gemini-3-flash-preview")
        .expect("gemini-3-flash-preview")
        .clone();
    let ctx = normalize_context(&sample_context());

    for (adapter_model, is_vertex) in [(google_model, false), (vertex_model, true)] {
        let received: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let event_models: Arc<Mutex<Vec<Model>>> = Arc::new(Mutex::new(Vec::new()));
        let rec_ref = Arc::clone(&received);
        let mod_ref = Arc::clone(&event_models);

        let cb: pi_ai::OnProviderStreamEventFn = Arc::new(move |chunk, ev_model| {
            let chunk_owned = chunk.clone();
            let model_owned = ev_model.clone();
            let r = Arc::clone(&rec_ref);
            let m = Arc::clone(&mod_ref);
            Box::pin(async move {
                tokio::task::yield_now().await;
                r.lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(chunk_owned);
                m.lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(model_owned);
                Ok(())
            }) as BoxFuture<pi_js::Result<()>>
        });

        let result = if is_vertex {
            stream_google_vertex(
                &adapter_model,
                &ctx,
                Some(
                    GoogleVertexOptions {
                        base: StreamOptions {
                            on_provider_stream_event: Some(cb),
                            ..Default::default()
                        },
                        project: Some("test-project".to_owned()),
                        location: Some("us-central1".to_owned()),
                        ..Default::default()
                    }
                    .into(),
                ),
            )
            .result()
            .await
        } else {
            stream_google_generative_ai(
                &adapter_model,
                &ctx,
                Some(
                    GoogleOptions {
                        base: StreamOptions {
                            api_key: Some("test-api-key".to_owned()),
                            on_provider_stream_event: Some(cb),
                            ..Default::default()
                        },
                        ..Default::default()
                    }
                    .into(),
                ),
            )
            .result()
            .await
        };

        let got_chunks = received.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
        let got_models = event_models
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();

        assert_eq!(got_chunks, chunks);
        assert_eq!(got_models, vec![adapter_model.clone(), adapter_model]);
        assert_eq!(result.stop_reason, StopReason::Stop);
        assert_eq!(result.response_id.as_deref(), Some("resp_google"));
        assert_eq!(
            result.content,
            vec![AssistantContentBlock::Text(pi_ai::TextContent::new("hello"))]
        );
    }
}

#[tokio::test]
async fn uses_pi_user_agent_by_default_and_lets_explicit_headers_override() {
    let default_headers = capture_google_headers(None).await;
    assert_eq!(
        default_headers.get("User-Agent").as_deref(),
        Some(pi_user_agent().as_str())
    );

    let mut overrides = ProviderHeaders::new();
    overrides.insert("User-Agent".to_owned(), Some("custom-agent".to_owned()));
    let custom_headers = capture_google_headers(Some(overrides)).await;
    assert_eq!(custom_headers.get("User-Agent").as_deref(), Some("custom-agent"));
}
