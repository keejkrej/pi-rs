//! Port of packages/ai/test/anthropic-sse-parsing.test.ts

use std::sync::{Arc, Mutex};

use pi_ai::api::anthropic_messages::{AnthropicClient, AnthropicOptions, stream_with_options};
use pi_ai::api::transform_messages::transform_messages;
use pi_ai::compat::{get_model, normalize_context};
use pi_ai::types::{
    AnthropicAllowedFallbackModel, AnthropicMessagesCompat, AssistantContentBlock, Context, JsonObject, Message,
    ModelCompat, ModelCost, StopReason, StreamOptions, ThinkingContent, Tool, UserMessage, UserMessageContent,
};
use pi_js::fetch::{Headers, Response};
use pi_js::time;
use pi_js::vendor::typebox::t;
use serde_json::{Value, json};

fn create_sse_response(events: &[(&str, String)]) -> Response {
    let body = events
        .iter()
        .map(|(event, data)| format!("event: {event}\ndata: {data}\n"))
        .collect::<Vec<_>>()
        .join("\n");
    Response::new(200, Headers::from_pairs([("content-type", "text/event-stream")]), body)
}

fn minimal_anthropic_events() -> Vec<(&'static str, String)> {
    vec![
        (
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": "msg_test",
                    "usage": {
                        "input_tokens": 12,
                        "output_tokens": 0,
                        "cache_read_input_tokens": 0,
                        "cache_creation_input_tokens": 0
                    }
                }
            })
            .to_string(),
        ),
        (
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": { "type": "text", "text": "" }
            })
            .to_string(),
        ),
        (
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "text_delta", "text": "Hello" }
            })
            .to_string(),
        ),
        (
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": 0 }).to_string(),
        ),
        (
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn" },
                "usage": {
                    "input_tokens": 12,
                    "output_tokens": 5,
                    "cache_read_input_tokens": 0,
                    "cache_creation_input_tokens": 0
                }
            })
            .to_string(),
        ),
        ("message_stop", json!({ "type": "message_stop" }).to_string()),
    ]
}

fn create_fake_anthropic_client(resp: Response) -> AnthropicClient {
    let r = Mutex::new(Some(resp));
    AnthropicClient::from_handler(move |_, _| {
        let taken = r.lock().unwrap().take().expect("fake response already consumed");
        async move { Ok(taken) }
    })
}

fn create_response_model_sse_response(model: &str, content_block: Value) -> Response {
    create_sse_response(&[
        (
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": "msg_response_model",
                    "model": model,
                    "usage": { "input_tokens": 100, "output_tokens": 0 }
                }
            })
            .to_string(),
        ),
        (
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": content_block
            })
            .to_string(),
        ),
        (
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": 0 }).to_string(),
        ),
        (
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn" },
                "usage": { "input_tokens": 100, "output_tokens": 20 }
            })
            .to_string(),
        ),
        ("message_stop", json!({ "type": "message_stop" }).to_string()),
    ])
}

fn hello_context(ts: i64) -> pi_ai::types::TranscriptContext {
    normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("Hello".to_owned()),
            timestamp: ts,
        })],
        tools: None,
    })
}

#[tokio::test]
async fn forwards_parsed_provider_stream_events_in_order() {
    let model = get_model("anthropic", "claude-haiku-4-5").unwrap();
    let provider_events = Arc::new(Mutex::new(Vec::new()));
    let event_models = Arc::new(Mutex::new(Vec::new()));
    let pe_c = provider_events.clone();
    let em_c = event_models.clone();

    let result = stream_with_options(
        &model,
        &hello_context(1),
        Some(AnthropicOptions {
            base: StreamOptions {
                on_provider_stream_event: Some(Arc::new(move |ev, ev_model| {
                    let pe = pe_c.clone();
                    let em = em_c.clone();
                    let ev_c = ev.clone();
                    let m_c = ev_model.clone();
                    Box::pin(async move {
                        pe.lock().unwrap().push(ev_c);
                        em.lock().unwrap().push(m_c);
                        Ok(())
                    })
                })),
                ..Default::default()
            },
            client: Some(create_fake_anthropic_client(create_sse_response(
                &minimal_anthropic_events(),
            ))),
            ..Default::default()
        }),
    )
    .result()
    .await;

    assert_eq!(result.stop_reason, StopReason::Stop);
    let types: Vec<String> = provider_events
        .lock()
        .unwrap()
        .iter()
        .map(|ev| ev.get("type").and_then(Value::as_str).unwrap().to_owned())
        .collect();
    assert_eq!(
        types,
        vec![
            "message_start",
            "content_block_start",
            "content_block_delta",
            "content_block_stop",
            "message_delta",
            "message_stop",
        ]
    );
    assert_eq!(
        *event_models.lock().unwrap(),
        vec![
            model.clone(),
            model.clone(),
            model.clone(),
            model.clone(),
            model.clone(),
            model
        ]
    );
}

#[tokio::test]
async fn keeps_signed_thinking_replayable_when_a_proxy_relabels_the_model() {
    let model = get_model("anthropic", "claude-opus-5").unwrap();
    let response_model = "kimi-for-coding";
    let initial_context = hello_context(1);

    let first = stream_with_options(
        &model,
        &initial_context,
        Some(AnthropicOptions {
            client: Some(create_fake_anthropic_client(create_response_model_sse_response(
                response_model,
                json!({ "type": "thinking", "thinking": "reasoning", "signature": "signature" }),
            ))),
            ..Default::default()
        }),
    )
    .result()
    .await;

    assert_eq!(first.model, model.id);
    assert_eq!(first.response_model.as_deref(), Some(response_model));

    let mut msgs = initial_context.messages.clone();
    msgs.push(Message::Assistant(first));
    let transformed = transform_messages(&msgs, &model, None);
    let replayed = transformed
        .iter()
        .find_map(|m| match m {
            Message::Assistant(a) => Some(a),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        replayed.content,
        vec![AssistantContentBlock::Thinking(ThinkingContent {
            thinking: "reasoning".to_owned(),
            thinking_signature: Some("signature".to_owned()),
            redacted: None,
        })]
    );
}

#[tokio::test]
async fn uses_a_returned_fallback_model_for_cost_attribution() {
    let fallback_model = "fallback-model";
    let mut model = get_model("anthropic", "claude-opus-5").unwrap();
    model.compat = Some(ModelCompat::AnthropicMessages(AnthropicMessagesCompat {
        allowed_fallback_models: Some(vec![AnthropicAllowedFallbackModel {
            provider: "anthropic".to_owned(),
            model: fallback_model.to_owned(),
            cost: ModelCost {
                input: 3.0,
                output: 5.0,
                cache_read: 0.0,
                cache_write: 0.0,
                tiers: None,
            },
        }]),
        ..Default::default()
    }));

    let result = stream_with_options(
        &model,
        &hello_context(1),
        Some(AnthropicOptions {
            client: Some(create_fake_anthropic_client(create_response_model_sse_response(
                fallback_model,
                json!({ "type": "text", "text": "done" }),
            ))),
            ..Default::default()
        }),
    )
    .result()
    .await;

    assert_eq!(result.model, model.id);
    assert_eq!(result.response_model.as_deref(), Some(fallback_model));
    assert!((result.usage.cost.input - 0.0003).abs() < 1e-10);
    assert!((result.usage.cost.output - 0.0001).abs() < 1e-10);
}

#[tokio::test]
async fn fails_safely_when_anthropic_falls_back_after_output_begins() {
    let model = get_model("anthropic", "claude-opus-5").unwrap();
    let response = create_sse_response(&[
        (
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": "msg_fallback",
                    "model": "claude-opus-5",
                    "usage": { "input_tokens": 1, "output_tokens": 0 }
                }
            })
            .to_string(),
        ),
        (
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": { "type": "text", "text": "partial" }
            })
            .to_string(),
        ),
        (
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": 0 }).to_string(),
        ),
        (
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": 1,
                "content_block": {
                    "type": "fallback",
                    "from": { "model": "claude-opus-5" },
                    "to": { "model": "claude-opus-4-8" }
                }
            })
            .to_string(),
        ),
    ]);

    let result = stream_with_options(
        &model,
        &hello_context(1),
        Some(AnthropicOptions {
            client: Some(create_fake_anthropic_client(response)),
            ..Default::default()
        }),
    )
    .result()
    .await;

    assert_eq!(result.stop_reason, StopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("unsupported mid-output model fallback")
    );
}

#[tokio::test]
async fn forces_streaming_after_an_on_payload_replacement() {
    let streaming = Arc::new(Mutex::new(None));
    let st_c = streaming.clone();
    let client = AnthropicClient::from_handler(move |params, _| {
        let st = st_c.clone();
        async move {
            *st.lock().unwrap() = params.get("stream").and_then(Value::as_bool);
            Ok(create_sse_response(&minimal_anthropic_events()))
        }
    });

    stream_with_options(
        &get_model("anthropic", "claude-fable-5-1").unwrap(),
        &hello_context(1),
        Some(AnthropicOptions {
            base: StreamOptions {
                on_payload: Some(Arc::new(|mut payload, _| {
                    Box::pin(async move {
                        if let Some(obj) = payload.as_object_mut() {
                            obj.insert("stream".to_owned(), Value::Bool(false));
                        }
                        Ok(Some(payload))
                    })
                })),
                ..Default::default()
            },
            client: Some(client),
            ..Default::default()
        }),
    )
    .result()
    .await;

    assert_eq!(*streaming.lock().unwrap(), Some(true));
}

#[tokio::test]
async fn omits_the_interleaved_thinking_beta_when_thinking_is_disabled() {
    let beta_features = Arc::new(Mutex::new(None));
    let bf_c = beta_features.clone();
    let client = AnthropicClient::from_handler(move |params, _| {
        let bf = bf_c.clone();
        async move {
            let list: Option<Vec<String>> = params
                .get("betas")
                .and_then(Value::as_array)
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect());
            *bf.lock().unwrap() = list;
            Ok(create_sse_response(&minimal_anthropic_events()))
        }
    });

    stream_with_options(
        &get_model("openrouter", "anthropic/claude-haiku-4.5").unwrap(),
        &hello_context(1),
        Some(AnthropicOptions {
            client: Some(client),
            thinking_enabled: Some(false),
            ..Default::default()
        }),
    )
    .result()
    .await;

    let betas = beta_features.lock().unwrap().take().unwrap_or_default();
    assert!(!betas.contains(&"interleaved-thinking-2025-05-14".to_owned()));
}

#[tokio::test]
async fn passes_managed_beta_features_to_injected_clients() {
    let beta_features = Arc::new(Mutex::new(Vec::new()));
    let bf_c = beta_features.clone();
    let client = AnthropicClient::from_handler(move |params, _| {
        let bf = bf_c.clone();
        async move {
            let list: Vec<String> = params
                .get("betas")
                .and_then(Value::as_array)
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
                .unwrap_or_default();
            *bf.lock().unwrap() = list;
            Ok(create_sse_response(&minimal_anthropic_events()))
        }
    });

    let result = stream_with_options(
        &get_model("anthropic", "claude-fable-5-1").unwrap(),
        &hello_context(1),
        Some(AnthropicOptions {
            client: Some(client),
            ..Default::default()
        }),
    )
    .result()
    .await;

    assert_eq!(result.stop_reason, StopReason::Stop);
    let betas = beta_features.lock().unwrap();
    assert!(betas.contains(&"mid-conversation-output-config-2026-07-01".to_owned()));
    assert!(betas.contains(&"thinking-binding-controls-2026-08-01".to_owned()));
}

#[tokio::test]
async fn uses_the_serving_model_input_transformations_from_the_final_stream_event() {
    let mut events = minimal_anthropic_events();
    events[0].1 = json!({
        "type": "message_start",
        "message": {
            "id": "msg_transformations",
            "model": "claude-fable-5-1",
            "usage": { "input_tokens": 12, "output_tokens": 0 },
            "input_transformations": [
                { "type": "thinking_dropped", "path": "messages.1.content.0", "reason": "prefix_binding_mismatch" }
            ]
        }
    })
    .to_string();
    let mut delta: Value = serde_json::from_str(&events[4].1).unwrap();
    delta.as_object_mut().unwrap().insert(
        "input_transformations".to_owned(),
        json!([
            { "type": "thinking_dropped", "path": "messages.3.content.0", "reason": "model_binding_mismatch" }
        ]),
    );
    events[4].1 = delta.to_string();

    let result = stream_with_options(
        &get_model("anthropic", "claude-fable-5-1").unwrap(),
        &hello_context(1),
        Some(AnthropicOptions {
            client: Some(create_fake_anthropic_client(create_sse_response(&events))),
            ..Default::default()
        }),
    )
    .result()
    .await;

    let diags = result.diagnostics.unwrap();
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].r#type, "anthropic_input_transformations");
    assert_eq!(
        diags[0].details.as_ref().unwrap().get("transformations"),
        Some(&json!([
            {
                "type": "thinking_dropped",
                "path": "messages.3.content.0",
                "reason": "model_binding_mismatch"
            }
        ]))
    );
}

#[tokio::test]
async fn repairs_malformed_sse_json_and_malformed_streamed_tool_json() {
    let model = get_model("anthropic", "claude-haiku-4-5").unwrap();
    let context = normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("Use the edit tool.".to_owned()),
            timestamp: time::now_ms(),
        })],
        tools: Some(vec![Tool {
            name: "edit".to_owned(),
            description: "Edit a file.".to_owned(),
            parameters: t::object([("path", t::string()), ("text", t::string())]),
            constrained_sampling: None,
        }]),
    });

    let malformed_delta = r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"A\H\",\"text\":\"col1	col2\"}"}}"#.to_owned();
    let response = create_sse_response(&[
        (
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": "msg_test",
                    "usage": {
                        "input_tokens": 12,
                        "output_tokens": 0,
                        "cache_read_input_tokens": 0,
                        "cache_creation_input_tokens": 0
                    }
                }
            })
            .to_string(),
        ),
        (
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {
                    "type": "tool_use",
                    "id": "toolu_test",
                    "name": "edit",
                    "input": {}
                }
            })
            .to_string(),
        ),
        ("content_block_delta", malformed_delta),
        (
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": 0 }).to_string(),
        ),
        (
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": "tool_use" },
                "usage": {
                    "input_tokens": 12,
                    "output_tokens": 5,
                    "cache_read_input_tokens": 0,
                    "cache_creation_input_tokens": 0
                }
            })
            .to_string(),
        ),
        ("message_stop", json!({ "type": "message_stop" }).to_string()),
    ]);

    let result = stream_with_options(
        &model,
        &context,
        Some(AnthropicOptions {
            client: Some(create_fake_anthropic_client(response)),
            ..Default::default()
        }),
    )
    .result()
    .await;

    assert_eq!(result.stop_reason, StopReason::ToolUse);
    assert!(result.error_message.is_none());

    let tool_call = result
        .content
        .iter()
        .find_map(|b| match b {
            AssistantContentBlock::ToolCall(tc) => Some(tc),
            _ => None,
        })
        .unwrap();
    let mut expected_args = JsonObject::new();
    expected_args.insert("path".to_owned(), json!("A\\H"));
    expected_args.insert("text".to_owned(), json!("col1\tcol2"));
    assert_eq!(tool_call.arguments, expected_args);
}

#[tokio::test]
async fn preserves_content_from_content_block_start_events() {
    let model = get_model("anthropic", "claude-haiku-4-5").unwrap();
    let context = hello_context(time::now_ms());
    let response = create_sse_response(&[
        (
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": "msg_initial_content",
                    "usage": {
                        "input_tokens": 12,
                        "output_tokens": 0,
                        "cache_read_input_tokens": 0,
                        "cache_creation_input_tokens": 0
                    }
                }
            })
            .to_string(),
        ),
        (
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": { "type": "text", "text": "Initial text" }
            })
            .to_string(),
        ),
        (
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "text_delta", "text": " plus delta" }
            })
            .to_string(),
        ),
        (
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": 0 }).to_string(),
        ),
        (
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": 1,
                "content_block": {
                    "type": "thinking",
                    "thinking": "Initial thinking",
                    "signature": "initial signature"
                }
            })
            .to_string(),
        ),
        (
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": 1,
                "delta": { "type": "thinking_delta", "thinking": " plus delta" }
            })
            .to_string(),
        ),
        (
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": 1,
                "delta": { "type": "signature_delta", "signature": " plus delta" }
            })
            .to_string(),
        ),
        (
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": 1 }).to_string(),
        ),
        (
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn" },
                "usage": {
                    "input_tokens": 12,
                    "output_tokens": 5,
                    "cache_read_input_tokens": 0,
                    "cache_creation_input_tokens": 0
                }
            })
            .to_string(),
        ),
        ("message_stop", json!({ "type": "message_stop" }).to_string()),
    ]);

    let result = stream_with_options(
        &model,
        &context,
        Some(AnthropicOptions {
            client: Some(create_fake_anthropic_client(response)),
            ..Default::default()
        }),
    )
    .result()
    .await;

    assert_eq!(
        result.content,
        vec![
            AssistantContentBlock::Text(pi_ai::types::TextContent::new("Initial text plus delta")),
            AssistantContentBlock::Thinking(ThinkingContent {
                thinking: "Initial thinking plus delta".to_owned(),
                thinking_signature: Some("initial signature plus delta".to_owned()),
                redacted: None,
            }),
        ]
    );
}

#[tokio::test]
async fn preserves_refusal_stop_details_from_message_delta() {
    let model = get_model("anthropic", "claude-fable-5").unwrap();
    let explanation = "This request triggered restrictions on violative cyber content and was blocked under Anthropic's Usage Policy. To learn more, provide feedback, or request an exemption based on how you use Claude, visit our help center: https://support.claude.com/en/articles/14604842-real-time-cyber-safeguards-on-claude.";
    let response = create_sse_response(&[
        (
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": "msg_01XFUDYJgAACzvnptvVoYEL",
                    "usage": {
                        "input_tokens": 412,
                        "output_tokens": 0,
                        "cache_read_input_tokens": 0,
                        "cache_creation_input_tokens": 0
                    }
                }
            })
            .to_string(),
        ),
        (
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": {
                    "stop_reason": "refusal",
                    "stop_details": {
                        "type": "refusal",
                        "category": "cyber",
                        "explanation": explanation
                    }
                },
                "usage": {
                    "input_tokens": 412,
                    "output_tokens": 0,
                    "cache_read_input_tokens": 0,
                    "cache_creation_input_tokens": 0
                }
            })
            .to_string(),
        ),
        ("message_stop", json!({ "type": "message_stop" }).to_string()),
    ]);

    let result = stream_with_options(
        &model,
        &hello_context(time::now_ms()),
        Some(AnthropicOptions {
            client: Some(create_fake_anthropic_client(response)),
            ..Default::default()
        }),
    )
    .result()
    .await;

    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(result.raw_stop_reason.as_deref(), Some("refusal"));
    assert_eq!(result.error_message.as_deref(), Some(explanation));
}

#[tokio::test]
async fn preserves_sensitive_stop_reasons_with_a_descriptive_error_message() {
    let model = get_model("anthropic", "claude-haiku-4-5").unwrap();
    let response = create_sse_response(&[
        (
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": "msg_sensitive",
                    "usage": {
                        "input_tokens": 12,
                        "output_tokens": 0,
                        "cache_read_input_tokens": 0,
                        "cache_creation_input_tokens": 0
                    }
                }
            })
            .to_string(),
        ),
        (
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": "sensitive" },
                "usage": {
                    "input_tokens": 12,
                    "output_tokens": 0,
                    "cache_read_input_tokens": 0,
                    "cache_creation_input_tokens": 0
                }
            })
            .to_string(),
        ),
        ("message_stop", json!({ "type": "message_stop" }).to_string()),
    ]);

    let result = stream_with_options(
        &model,
        &hello_context(time::now_ms()),
        Some(AnthropicOptions {
            client: Some(create_fake_anthropic_client(response)),
            ..Default::default()
        }),
    )
    .result()
    .await;

    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(result.raw_stop_reason.as_deref(), Some("sensitive"));
    assert_eq!(
        result.error_message.as_deref(),
        Some("Provider stopped with: sensitive")
    );
}

#[tokio::test]
async fn treats_message_delta_without_usage_as_a_no_op_for_usage_accumulation() {
    let model = get_model("anthropic", "claude-haiku-4-5").unwrap();
    let mut events = minimal_anthropic_events();
    events[4] = (
        "message_delta",
        json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" } }).to_string(),
    );

    let result = stream_with_options(
        &model,
        &hello_context(time::now_ms()),
        Some(AnthropicOptions {
            client: Some(create_fake_anthropic_client(create_sse_response(&events))),
            ..Default::default()
        }),
    )
    .result()
    .await;

    assert_eq!(result.stop_reason, StopReason::Stop);
    assert!(result.error_message.is_none());
    assert_eq!(
        result.content,
        vec![AssistantContentBlock::Text(pi_ai::types::TextContent::new("Hello"))]
    );
    assert_eq!(result.usage.input, 12);
    assert_eq!(result.usage.total_tokens, 12);
}

#[tokio::test]
async fn ignores_unknown_sse_events_after_message_stop() {
    let model = get_model("anthropic", "claude-haiku-4-5").unwrap();
    let mut events = minimal_anthropic_events();
    events.push(("done", "[DONE]".to_owned()));
    events.push(("proxy.stats", "not json".to_owned()));

    let result = stream_with_options(
        &model,
        &hello_context(time::now_ms()),
        Some(AnthropicOptions {
            client: Some(create_fake_anthropic_client(create_sse_response(&events))),
            ..Default::default()
        }),
    )
    .result()
    .await;

    assert_eq!(result.stop_reason, StopReason::Stop);
    assert!(result.error_message.is_none());
    assert_eq!(
        result.content,
        vec![AssistantContentBlock::Text(pi_ai::types::TextContent::new("Hello"))]
    );
}
