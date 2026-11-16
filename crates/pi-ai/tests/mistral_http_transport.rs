//! Port of packages/ai/test/mistral-http-transport.test.ts

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use indexmap::IndexMap;
use pi_ai::api::mistral_conversations::{
    MistralOptions, MistralPromptMode, MistralReasoningEffort, MistralToolChoice, stream_with_options,
};
use pi_ai::providers::mistral_models::mistral_models;
use pi_ai::utils::transcript::normalize_context;
use pi_ai::{
    AssistantContentBlock, AssistantMessage, Context, FetchFunction, ImageContent, JsonObject, Message, Model,
    OnPayloadFn, OnProviderStreamEventFn, OnResponseFn, ProviderHeaders, ProviderResponse, StopReason, StreamOptions,
    TextContent, ThinkingContent, Tool, ToolCall, ToolResultMessage, Type, Usage, UserContentBlock, UserMessage,
};
use pi_js::abort::AbortController;
use pi_js::fetch::testing::{chunked_response, response};
use pi_js::fetch::{Headers, Response};
use pi_js::{BoxFuture, Result};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

fn pi_user_agent() -> String {
    format!(
        "pi ({} {}; {})",
        pi_js::env::platform(),
        pi_js::env::os_release(),
        pi_js::env::arch()
    )
}

fn create_sse_response(events: &[Value], extra_headers: &[(&str, &str)]) -> Response {
    let mut body = String::new();
    for event in events {
        body.push_str("data: ");
        body.push_str(&pi_js::json::stringify(event));
        body.push_str("\r\n\r\n");
    }
    body.push_str("data: [DONE]\r\n\r\n");

    let mut hdrs = vec![("content-type", "text/event-stream")];
    hdrs.extend_from_slice(extra_headers);
    response(200, &hdrs, body)
}

fn create_bytewise_sse_response(event: &Value) -> Response {
    let text = format!("data: {}\r\n\r\ndata: [DONE]\r\n\r\n", pi_js::json::stringify(event));
    let chunks: Vec<Bytes> = text.as_bytes().iter().map(|&b| Bytes::copy_from_slice(&[b])).collect();
    chunked_response(200, &[("content-type", "text/event-stream")], chunks)
}

fn create_terminal_event(finish_reason: &str) -> Value {
    json!({
        "id": "mistral-response-id",
        "model": "mistral-large-latest",
        "choices": [{ "index": 0, "finish_reason": finish_reason, "delta": {} }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
    })
}

fn mistral_model(id: &str) -> Model {
    mistral_models()
        .get(id)
        .expect("model should exist in mistral catalog")
        .clone()
}

#[tokio::test]
async fn serializes_sdk_style_payloads_to_mistral_wire_format() {
    let model = mistral_model("mistral-large-latest");
    let context = Context {
        system_prompt: Some("Be precise".to_owned()),
        messages: vec![Message::User(UserMessage {
            content: pi_ai::UserMessageContent::Blocks(vec![
                UserContentBlock::Text(TextContent::new("describe")),
                UserContentBlock::Image(ImageContent::new("aGVsbG8=", "image/png")),
            ]),
            timestamp: 1,
        })],
        tools: Some(vec![Tool {
            name: "lookup".to_owned(),
            description: "Look something up".to_owned(),
            parameters: Type::object([("query", Type::string())]),
            constrained_sampling: None,
        }]),
    };

    let req_url: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let req_headers: Arc<Mutex<Option<Headers>>> = Arc::new(Mutex::new(None));
    let req_body: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let cb_payload: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let cb_response: Arc<Mutex<Option<ProviderResponse>>> = Arc::new(Mutex::new(None));

    let url_slot = Arc::clone(&req_url);
    let hdr_slot = Arc::clone(&req_headers);
    let body_slot = Arc::clone(&req_body);
    let fetch_fn: FetchFunction = Arc::new(move |req| {
        let u = Arc::clone(&url_slot);
        let h = Arc::clone(&hdr_slot);
        let b = Arc::clone(&body_slot);
        Box::pin(async move {
            *u.lock().unwrap_or_else(|p| p.into_inner()) = Some(req.url.clone());
            *h.lock().unwrap_or_else(|p| p.into_inner()) = Some(req.headers.clone());
            *b.lock().unwrap_or_else(|p| p.into_inner()) = req.body_text();
            Ok(create_sse_response(
                &[create_terminal_event("stop")],
                &[("x-request-id", "request-1")],
            ))
        }) as BoxFuture<Result<Response>>
    });

    let p_slot = Arc::clone(&cb_payload);
    let on_payload: OnPayloadFn<Model> = Arc::new(move |payload, _model| {
        let s = Arc::clone(&p_slot);
        Box::pin(async move {
            *s.lock().unwrap_or_else(|p| p.into_inner()) = Some(payload.clone());
            let mut next = payload.as_object().cloned().unwrap_or_default();
            next.insert("topP".to_owned(), json!(0.9));
            next.insert("randomSeed".to_owned(), json!(42));
            next.insert(
                "responseFormat".to_owned(),
                json!({
                    "type": "json_schema",
                    "jsonSchema": {
                        "name": "result",
                        "schemaDefinition": {
                            "type": "object",
                            "properties": { "maxTokens": { "type": "number" } }
                        }
                    }
                }),
            );
            next.insert("presencePenalty".to_owned(), json!(0.1));
            next.insert("frequencyPenalty".to_owned(), json!(0.2));
            next.insert("parallelToolCalls".to_owned(), json!(true));
            next.insert("safePrompt".to_owned(), json!(true));
            Ok(Some(Value::Object(next)))
        }) as BoxFuture<Result<Option<Value>>>
    });

    let r_slot = Arc::clone(&cb_response);
    let on_response: OnResponseFn<Model> = Arc::new(move |resp, _model| {
        let s = Arc::clone(&r_slot);
        Box::pin(async move {
            *s.lock().unwrap_or_else(|p| p.into_inner()) = Some(resp);
            Ok(())
        }) as BoxFuture<Result<()>>
    });

    let mut headers = ProviderHeaders::new();
    headers.insert("x-custom".to_owned(), Some("value".to_owned()));

    let opts = MistralOptions {
        base: StreamOptions {
            api_key: Some("secret".to_owned()),
            fetch: Some(fetch_fn),
            headers: Some(headers),
            max_tokens: Some(123),
            session_id: Some("session-1".to_owned()),
            on_payload: Some(on_payload),
            on_response: Some(on_response),
            ..Default::default()
        },
        prompt_mode: Some(MistralPromptMode::Reasoning),
        reasoning_effort: Some(MistralReasoningEffort::High),
        tool_choice: Some(MistralToolChoice::Function {
            name: "lookup".to_owned(),
        }),
    };

    let message = stream_with_options(&model, &normalize_context(&context), Some(opts))
        .result()
        .await;

    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(
        req_url.lock().unwrap_or_else(|p| p.into_inner()).as_deref(),
        Some("https://api.mistral.ai/v1/chat/completions")
    );

    let headers = req_headers
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
        .expect("request headers captured");
    assert_eq!(headers.get("authorization").as_deref(), Some("Bearer secret"));
    assert_eq!(headers.get("accept").as_deref(), Some("text/event-stream"));
    assert_eq!(headers.get("x-affinity").as_deref(), Some("session-1"));
    assert_eq!(headers.get("x-custom").as_deref(), Some("value"));
    assert_eq!(headers.get("user-agent").as_deref(), Some(pi_user_agent().as_str()));

    let callback_payload = cb_payload
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
        .expect("on_payload captured");
    assert_eq!(callback_payload["maxTokens"], json!(123));
    assert_eq!(callback_payload["promptMode"], json!("reasoning"));
    assert_eq!(callback_payload["promptCacheKey"], json!("session-1"));

    let mut expected_resp_hdrs = IndexMap::new();
    expected_resp_hdrs.insert("content-type".to_owned(), "text/event-stream".to_owned());
    expected_resp_hdrs.insert("x-request-id".to_owned(), "request-1".to_owned());
    assert_eq!(
        cb_response.lock().unwrap_or_else(|p| p.into_inner()).clone(),
        Some(ProviderResponse {
            status: 200,
            headers: expected_resp_hdrs,
        })
    );

    let raw_body = req_body
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
        .expect("request body captured");
    let wire_payload: Value = pi_js::json::parse(&raw_body).expect("valid wire JSON");
    assert_eq!(wire_payload["max_tokens"], json!(123));
    assert_eq!(wire_payload["prompt_mode"], json!("reasoning"));
    assert_eq!(wire_payload["reasoning_effort"], json!("high"));
    assert_eq!(
        wire_payload["tool_choice"],
        json!({ "type": "function", "function": { "name": "lookup" } })
    );
    assert_eq!(wire_payload["prompt_cache_key"], json!("session-1"));
    assert_eq!(wire_payload["top_p"], json!(0.9));
    assert_eq!(wire_payload["random_seed"], json!(42));
    assert_eq!(wire_payload["presence_penalty"], json!(0.1));
    assert_eq!(wire_payload["frequency_penalty"], json!(0.2));
    assert_eq!(wire_payload["parallel_tool_calls"], json!(true));
    assert_eq!(wire_payload["safe_prompt"], json!(true));
    assert_eq!(
        wire_payload["response_format"],
        json!({
            "type": "json_schema",
            "json_schema": {
                "name": "result",
                "schema": {
                    "type": "object",
                    "properties": { "maxTokens": { "type": "number" } }
                }
            }
        })
    );
    let wire_obj = wire_payload.as_object().expect("wire payload object");
    assert!(!wire_obj.contains_key("maxTokens"));
    assert!(!wire_obj.contains_key("promptMode"));
    assert!(!wire_obj.contains_key("promptCacheKey"));
    assert_eq!(
        wire_payload["messages"],
        json!([
            { "role": "system", "content": "Be precise" },
            {
                "role": "user",
                "content": [
                    { "type": "text", "text": "describe" },
                    { "type": "image_url", "image_url": "data:image/png;base64,aGVsbG8=" }
                ]
            }
        ])
    );
}

#[tokio::test]
async fn serializes_assistant_thinking_tool_calls_and_tool_results_for_replay() {
    let model = mistral_model("mistral-large-latest");
    let mut lookup_args = JsonObject::new();
    lookup_args.insert("query".to_owned(), json!("pi"));

    let context = Context {
        system_prompt: None,
        messages: vec![
            Message::Assistant(AssistantMessage {
                content: vec![
                    AssistantContentBlock::Thinking(ThinkingContent::new("reason")),
                    AssistantContentBlock::Text(TextContent::new("answer")),
                    AssistantContentBlock::ToolCall(ToolCall::new("abc123456", "lookup", lookup_args)),
                ],
                api: "mistral-conversations".to_owned(),
                provider: "mistral".to_owned(),
                model: model.id.clone(),
                response_model: None,
                response_id: None,
                provider_thinking_level: None,
                thinking_level: None,
                diagnostics: None,
                usage: Usage::default(),
                stop_reason: StopReason::ToolUse,
                deferred: None,
                error_message: None,
                raw_stop_reason: None,
                end_turn: None,
                timestamp: 1,
            }),
            Message::ToolResult(ToolResultMessage {
                tool_call_id: "abc123456".to_owned(),
                tool_name: "lookup".to_owned(),
                content: vec![
                    UserContentBlock::Text(TextContent::new("found")),
                    UserContentBlock::Image(ImageContent::new("aGVsbG8=", "image/png")),
                ],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: false,
                timestamp: 2,
            }),
        ],
        tools: None,
    };

    let req_body: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let body_slot = Arc::clone(&req_body);
    let fetch_fn: FetchFunction = Arc::new(move |req| {
        let s = Arc::clone(&body_slot);
        Box::pin(async move {
            *s.lock().unwrap_or_else(|p| p.into_inner()) = req.body_text();
            Ok(create_sse_response(&[create_terminal_event("stop")], &[]))
        })
    });

    let opts = MistralOptions {
        base: StreamOptions {
            api_key: Some("test".to_owned()),
            fetch: Some(fetch_fn),
            ..Default::default()
        },
        ..Default::default()
    };

    let message = stream_with_options(&model, &normalize_context(&context), Some(opts))
        .result()
        .await;

    assert_eq!(message.stop_reason, StopReason::Stop);
    let raw_body = req_body
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
        .expect("body captured");
    let wire_payload: Value = pi_js::json::parse(&raw_body).expect("valid wire JSON");
    assert_eq!(
        wire_payload["messages"],
        json!([
            {
                "role": "assistant",
                "prefix": false,
                "content": [
                    { "type": "thinking", "thinking": [{ "type": "text", "text": "reason" }] },
                    { "type": "text", "text": "answer" }
                ],
                "tool_calls": [
                    {
                        "id": "abc123456",
                        "type": "function",
                        "function": { "name": "lookup", "arguments": "{\"query\":\"pi\"}" },
                        "index": 0
                    }
                ]
            },
            {
                "role": "tool",
                "tool_call_id": "abc123456",
                "name": "lookup",
                "content": [
                    { "type": "text", "text": "found" },
                    { "type": "image_url", "image_url": "data:image/png;base64,aGVsbG8=" }
                ]
            }
        ])
    );
}

#[tokio::test]
async fn parses_native_thinking_text_fragmented_tool_calls_and_cached_token_usage() {
    let model = mistral_model("mistral-large-latest");
    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: "hello".into(),
            timestamp: 1,
        })],
        tools: None,
    };

    let events = vec![
        json!({
            "id": "response-1",
            "model": model.id,
            "choices": [{
                "index": 0,
                "finish_reason": null,
                "delta": { "content": [{ "type": "thinking", "thinking": [{ "type": "text", "text": "reason" }] }] }
            }]
        }),
        json!({
            "id": "response-1",
            "model": model.id,
            "choices": [{
                "index": 0,
                "finish_reason": null,
                "delta": { "content": [{ "type": "text", "text": "answer" }] }
            }]
        }),
        json!({
            "id": "response-1",
            "model": model.id,
            "choices": [{
                "index": 0,
                "finish_reason": null,
                "delta": {
                    "tool_calls": [{
                        "id": "abc123456",
                        "index": 0,
                        "function": { "name": "lookup", "arguments": "{\"query\":" }
                    }]
                }
            }]
        }),
        json!({
            "id": "response-1",
            "model": model.id,
            "choices": [{
                "index": 0,
                "finish_reason": "tool_calls",
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "function": { "name": "", "arguments": "\"pi\"}" }
                    }]
                }
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 4,
                "total_tokens": 14,
                "prompt_tokens_details": { "cached_tokens": 3 }
            }
        }),
    ];

    let fetch_fn: FetchFunction = Arc::new(move |_req| {
        let evs = events.clone();
        Box::pin(async move { Ok(create_sse_response(&evs, &[])) })
    });

    let opts = MistralOptions {
        base: StreamOptions {
            api_key: Some("test".to_owned()),
            fetch: Some(fetch_fn),
            ..Default::default()
        },
        ..Default::default()
    };

    let message = stream_with_options(&model, &normalize_context(&context), Some(opts))
        .result()
        .await;

    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert_eq!(message.raw_stop_reason.as_deref(), Some("tool_calls"));
    assert_eq!(message.response_id.as_deref(), Some("response-1"));

    let mut expected_args = JsonObject::new();
    expected_args.insert("query".to_owned(), json!("pi"));
    assert_eq!(
        message.content,
        vec![
            AssistantContentBlock::Thinking(ThinkingContent::new("reason")),
            AssistantContentBlock::Text(TextContent::new("answer")),
            AssistantContentBlock::ToolCall(ToolCall::new("abc123456", "lookup", expected_args)),
        ]
    );
    assert_eq!(message.usage.input, 7);
    assert_eq!(message.usage.output, 4);
    assert_eq!(message.usage.cache_read, 3);
    assert_eq!(message.usage.cache_write, 0);
    assert_eq!(message.usage.total_tokens, 14);
}

#[tokio::test]
async fn ignores_empty_content_deltas() {
    let model = mistral_model("zai-glm-5-3");
    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: "hello".into(),
            timestamp: 1,
        })],
        tools: None,
    };

    let thinking = |text: &str| json!({ "type": "thinking", "thinking": [{ "type": "text", "text": text }] });
    let tool_call = |args: &str, first: bool| {
        if first {
            json!({
                "index": 0,
                "id": "abc123456",
                "function": { "name": "read", "arguments": args }
            })
        } else {
            json!({
                "index": 0,
                "function": { "name": "", "arguments": args }
            })
        }
    };

    let deltas = vec![
        json!({ "content": "" }),
        json!({ "content": [thinking("first part,")] }),
        json!({ "content": "" }),
        json!({ "content": [{ "type": "text", "text": "" }] }),
        json!({ "content": [thinking(" second part."), { "type": "text", "text": "Reading." }] }),
        json!({ "content": "", "tool_calls": [tool_call("", true)] }),
        json!({ "content": "", "tool_calls": [tool_call("{\"path\":", false)] }),
        json!({ "content": "", "tool_calls": [tool_call("\"a.txt\"}", false)] }),
        json!({ "content": "" }),
    ];

    let total = deltas.len();
    let events: Vec<Value> = deltas
        .into_iter()
        .enumerate()
        .map(|(i, delta)| {
            json!({
                "id": "response-1",
                "model": model.id,
                "choices": [{
                    "index": 0,
                    "finish_reason": if i == total - 1 { json!("tool_calls") } else { Value::Null },
                    "delta": delta
                }]
            })
        })
        .collect();

    let fetch_fn: FetchFunction = Arc::new(move |_req| {
        let evs = events.clone();
        Box::pin(async move { Ok(create_sse_response(&evs, &[])) })
    });

    let opts = MistralOptions {
        base: StreamOptions {
            api_key: Some("test".to_owned()),
            fetch: Some(fetch_fn),
            ..Default::default()
        },
        ..Default::default()
    };

    let message = stream_with_options(&model, &normalize_context(&context), Some(opts))
        .result()
        .await;

    let mut expected_args = JsonObject::new();
    expected_args.insert("path".to_owned(), json!("a.txt"));
    assert_eq!(
        message.content,
        vec![
            AssistantContentBlock::Thinking(ThinkingContent::new("first part, second part.")),
            AssistantContentBlock::Text(TextContent::new("Reading.")),
            AssistantContentBlock::ToolCall(ToolCall::new("abc123456", "read", expected_args)),
        ]
    );
}

#[tokio::test]
async fn forwards_each_parsed_sse_payload_before_normalizing_it() {
    let model = mistral_model("mistral-large-latest");
    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: "hello".into(),
            timestamp: 1,
        })],
        tools: None,
    };

    let events = vec![
        json!({
            "id": "response-1",
            "provider_metadata": { "request": "test" },
            "choices": [{ "index": 0, "finish_reason": null, "delta": { "content": "hello" } }]
        }),
        json!({
            "id": "response-1",
            "choices": [{ "index": 0, "finish_reason": "stop", "delta": {} }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
        }),
    ];

    let received: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let event_models: Arc<Mutex<Vec<Model>>> = Arc::new(Mutex::new(Vec::new()));
    let r_ref = Arc::clone(&received);
    let m_ref = Arc::clone(&event_models);

    let on_event: OnProviderStreamEventFn = Arc::new(move |ev, ev_model| {
        let ev_owned = ev.clone();
        let m_owned = ev_model.clone();
        let r = Arc::clone(&r_ref);
        let m = Arc::clone(&m_ref);
        Box::pin(async move {
            tokio::task::yield_now().await;
            r.lock().unwrap_or_else(|p| p.into_inner()).push(ev_owned);
            m.lock().unwrap_or_else(|p| p.into_inner()).push(m_owned);
            Ok(())
        })
    });

    let evs_clone = events.clone();
    let fetch_fn: FetchFunction = Arc::new(move |_req| {
        let evs = evs_clone.clone();
        Box::pin(async move { Ok(create_sse_response(&evs, &[])) })
    });

    let opts = MistralOptions {
        base: StreamOptions {
            api_key: Some("test".to_owned()),
            fetch: Some(fetch_fn),
            on_provider_stream_event: Some(on_event),
            ..Default::default()
        },
        ..Default::default()
    };

    let result = stream_with_options(&model, &normalize_context(&context), Some(opts))
        .result()
        .await;

    assert_eq!(*received.lock().unwrap_or_else(|p| p.into_inner()), events);
    assert_eq!(
        *event_models.lock().unwrap_or_else(|p| p.into_inner()),
        vec![model.clone(), model]
    );
    assert_eq!(result.stop_reason, StopReason::Stop);
    assert_eq!(
        result.content,
        vec![AssistantContentBlock::Text(TextContent::new("hello"))]
    );
}

#[tokio::test]
async fn parses_sse_and_utf8_sequences_split_across_transport_chunks() {
    let model = mistral_model("mistral-large-latest");
    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: "hello".into(),
            timestamp: 1,
        })],
        tools: None,
    };

    let event = json!({
        "id": "response-bytewise",
        "model": model.id,
        "choices": [{ "index": 0, "finish_reason": "stop", "delta": { "content": "héllo 🌍" } }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3 }
    });
    let fetch_fn: FetchFunction = Arc::new(move |_req| {
        let ev = event.clone();
        Box::pin(async move { Ok(create_bytewise_sse_response(&ev)) })
    });

    let opts = MistralOptions {
        base: StreamOptions {
            api_key: Some("test".to_owned()),
            fetch: Some(fetch_fn),
            ..Default::default()
        },
        ..Default::default()
    };

    let message = stream_with_options(&model, &normalize_context(&context), Some(opts))
        .result()
        .await;

    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(
        message.content,
        vec![AssistantContentBlock::Text(TextContent::new("héllo 🌍"))]
    );
}

#[tokio::test]
async fn honors_case_insensitive_header_overrides_and_explicit_affinity_suppression() {
    let mut model = mistral_model("mistral-large-latest");
    let mut model_headers = IndexMap::new();
    model_headers.insert("Authorization".to_owned(), "Bearer model-key".to_owned());
    model_headers.insert("X-Affinity".to_owned(), "model-affinity".to_owned());
    model.headers = Some(model_headers);

    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: "hello".into(),
            timestamp: 1,
        })],
        tools: None,
    };

    let req_headers: Arc<Mutex<Option<Headers>>> = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&req_headers);
    let fetch_fn: FetchFunction = Arc::new(move |req| {
        let s = Arc::clone(&slot);
        Box::pin(async move {
            *s.lock().unwrap_or_else(|p| p.into_inner()) = Some(req.headers.clone());
            Ok(create_sse_response(&[create_terminal_event("stop")], &[]))
        })
    });

    let mut opt_headers = ProviderHeaders::new();
    opt_headers.insert("authorization".to_owned(), None);
    opt_headers.insert("x-affinity".to_owned(), None);
    opt_headers.insert("User-Agent".to_owned(), Some("custom-agent".to_owned()));

    let opts = MistralOptions {
        base: StreamOptions {
            api_key: Some("request-key".to_owned()),
            fetch: Some(fetch_fn),
            session_id: Some("automatic-affinity".to_owned()),
            headers: Some(opt_headers),
            ..Default::default()
        },
        ..Default::default()
    };

    let _ = stream_with_options(&model, &normalize_context(&context), Some(opts))
        .result()
        .await;

    let headers = req_headers
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take()
        .expect("headers captured");
    assert!(!headers.has("authorization"));
    assert!(!headers.has("x-affinity"));
    assert_eq!(headers.get("user-agent").as_deref(), Some("custom-agent"));
}

#[tokio::test]
async fn aborts_while_waiting_for_an_sse_chunk() {
    let model = mistral_model("mistral-large-latest");
    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: "hello".into(),
            timestamp: 1,
        })],
        tools: None,
    };
    let controller = AbortController::new();
    let fetch_fn: FetchFunction = Arc::new(|_req| {
        Box::pin(async move {
            Ok(Response::from_stream(
                200,
                Headers::from_pairs([("content-type", "text/event-stream")]),
                futures::stream::pending::<Result<Bytes>>(),
            ))
        })
    });

    let opts = MistralOptions {
        base: StreamOptions {
            api_key: Some("test".to_owned()),
            fetch: Some(fetch_fn),
            signal: Some(controller.signal()),
            ..Default::default()
        },
        ..Default::default()
    };

    let result_fut = stream_with_options(&model, &normalize_context(&context), Some(opts)).result();
    controller.abort(None);
    let message = result_fut.await;

    assert_eq!(message.stop_reason, StopReason::Aborted);
}

#[tokio::test]
async fn applies_the_request_timeout_while_waiting_for_an_sse_chunk() {
    let model = mistral_model("mistral-large-latest");
    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: "hello".into(),
            timestamp: 1,
        })],
        tools: None,
    };
    let fetch_fn: FetchFunction = Arc::new(|_req| {
        Box::pin(async move {
            Ok(Response::from_stream(
                200,
                Headers::from_pairs([("content-type", "text/event-stream")]),
                futures::stream::pending::<Result<Bytes>>(),
            ))
        })
    });

    let opts = MistralOptions {
        base: StreamOptions {
            api_key: Some("test".to_owned()),
            fetch: Some(fetch_fn),
            timeout_ms: Some(5),
            ..Default::default()
        },
        ..Default::default()
    };

    let message = stream_with_options(&model, &normalize_context(&context), Some(opts))
        .result()
        .await;

    assert_eq!(message.stop_reason, StopReason::Error);
    assert!(
        message
            .error_message
            .as_deref()
            .is_some_and(|m| m.to_ascii_lowercase().contains("timeout")),
        "expected timeout error, got: {:?}",
        message.error_message
    );
}

#[tokio::test]
async fn preserves_http_status_and_response_bodies_in_errors() {
    let model = mistral_model("mistral-large-latest");
    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: "hello".into(),
            timestamp: 1,
        })],
        tools: None,
    };
    let fetch_fn: FetchFunction = Arc::new(|_req| {
        Box::pin(
            async move { Ok(response(403, &[], "{\"message\":\"blocked by gateway\"}").with_status_text("Forbidden")) },
        )
    });

    let opts = MistralOptions {
        base: StreamOptions {
            api_key: Some("test".to_owned()),
            fetch: Some(fetch_fn),
            ..Default::default()
        },
        ..Default::default()
    };

    let message = stream_with_options(&model, &normalize_context(&context), Some(opts))
        .result()
        .await;

    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(
        message.error_message.as_deref(),
        Some("Mistral API error (403): {\"message\":\"blocked by gateway\"}")
    );
}
