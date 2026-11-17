//! Port of packages/ai/test/pi-messages.test.ts (unit ai-providers-builtin)

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use indexmap::IndexMap;
use pi_ai::api::pi_messages::{PiMessagesOptions, PiMessagesToolChoice, stream, stream_simple};
use pi_ai::compat::{get_api_provider, register_built_in_api_providers};
use pi_ai::types::{
    AssistantContentBlock, AssistantMessageEvent, Context, FetchFunction, Message, Model, ModelCost, ModelInput,
    OnProviderStreamEventFn, OnResponseFn, StopReason, StreamOptions, TextContent, ToolCall, Usage, UsageCost,
    UserMessage, UserMessageContent,
};
use pi_ai::utils::transcript::normalize_context;
use pi_js::fetch::{Headers, Response as FetchResponse};
use serde_json::{Value, json};

fn create_model(base_url: &str) -> Model {
    Model {
        r#type: None,
        id: "auto".to_string(),
        name: "Radius Auto".to_string(),
        api: "pi-messages".to_string(),
        provider: "radius".into(),
        base_url: base_url.to_string(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost {
            input: 1.0,
            output: 2.0,
            cache_read: 0.1,
            cache_write: 0.2,
            tiers: None,
        },
        context_window: 128000,
        max_tokens: 16384,
        headers: None,
        sampling_params: None,
        compat: None,
    }
}

fn sample_context() -> Context {
    Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("Hello".to_string()),
            timestamp: 1,
        })],
        tools: None,
    }
}

fn sample_usage_json() -> Value {
    json!({
        "input": 10,
        "output": 5,
        "cacheRead": 0,
        "cacheWrite": 0,
        "totalTokens": 15,
        "cost": { "input": 0.1, "output": 0.2, "cacheRead": 0, "cacheWrite": 0, "total": 0.3 }
    })
}

fn sample_usage() -> Usage {
    Usage {
        input: 10,
        output: 5,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 15,
        cost: UsageCost {
            input: 0.1,
            output: 0.2,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.3,
        },
    }
}

#[derive(Clone)]
struct RecordedRequest {
    url: String,
    headers: IndexMap<String, String>,
    body: Option<Value>,
}

fn sse_fetch(
    events: Vec<Value>,
    extra_headers: Vec<(&'static str, &'static str)>,
) -> (FetchFunction, Arc<Mutex<Vec<RecordedRequest>>>) {
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let rec = Arc::clone(&recorded);
    let body_bytes: Vec<u8> = events
        .into_iter()
        .map(|ev| format!("data: {ev}\n\n"))
        .collect::<String>()
        .into_bytes();

    let fetch: FetchFunction = Arc::new(move |req| {
        let rec = Arc::clone(&rec);
        let extra_headers = extra_headers.clone();
        let body_bytes = body_bytes.clone();
        Box::pin(async move {
            let mut hdrs = IndexMap::new();
            for (k, v) in req.headers.entries() {
                hdrs.insert(k.to_ascii_lowercase(), v);
            }
            rec.lock().unwrap().push(RecordedRequest {
                url: req.url.clone(),
                headers: hdrs,
                body: req.body_json(),
            });
            let mut resp_headers = Headers::new();
            resp_headers.set("content-type", "text/event-stream");
            for (k, v) in extra_headers {
                resp_headers.set(k, v);
            }
            Ok(FetchResponse::new(200, resp_headers, body_bytes))
        })
    });
    (fetch, recorded)
}

#[tokio::test]
async fn streams_text_and_tool_calls_and_resolves_terminal_message() {
    let events = vec![
        json!({ "type": "start" }),
        json!({ "type": "text_start", "contentIndex": 0 }),
        json!({ "type": "text_delta", "contentIndex": 0, "delta": "Hel" }),
        json!({ "type": "text_delta", "contentIndex": 0, "delta": "lo" }),
        json!({ "type": "text_end", "contentIndex": 0, "content": "Hello" }),
        json!({ "type": "toolcall_start", "contentIndex": 1, "id": "call_1", "toolName": "read" }),
        json!({ "type": "toolcall_delta", "contentIndex": 1, "delta": "{\"path\":" }),
        json!({ "type": "toolcall_delta", "contentIndex": 1, "delta": "\"a.txt\"}" }),
        json!({
            "type": "toolcall_end",
            "contentIndex": 1,
            "toolCall": { "type": "toolCall", "id": "call_1", "name": "read", "arguments": { "path": "a.txt" } }
        }),
        json!({
            "type": "done",
            "reason": "toolUse",
            "usage": sample_usage_json(),
            "responseId": "resp_1",
            "providerThinkingLevel": "high"
        }),
    ];
    let (fetch, recorded) = sse_fetch(events, Vec::new());
    let model = create_model("http://127.0.0.1:9999/v1");
    let mut custom_headers = IndexMap::new();
    custom_headers.insert("x-custom".to_string(), Some("1".to_string()));

    let mut es = stream(
        &model,
        &normalize_context(&sample_context()),
        Some(
            PiMessagesOptions {
                base: StreamOptions {
                    api_key: Some("test-key".to_string()),
                    session_id: Some("session-1".to_string()),
                    max_tokens: Some(100),
                    headers: Some(custom_headers),
                    fetch: Some(fetch),
                    ..Default::default()
                },
                tool_choice: Some(PiMessagesToolChoice::Auto),
                ..Default::default()
            }
            .into(),
        ),
    );

    let mut seen_events = Vec::new();
    let mut partial_reasons = Vec::new();
    while let Some(ev) = es.next().await {
        match &ev {
            AssistantMessageEvent::Start { partial }
            | AssistantMessageEvent::TextStart { partial, .. }
            | AssistantMessageEvent::TextDelta { partial, .. }
            | AssistantMessageEvent::TextEnd { partial, .. }
            | AssistantMessageEvent::ThinkingStart { partial, .. }
            | AssistantMessageEvent::ThinkingDelta { partial, .. }
            | AssistantMessageEvent::ThinkingEnd { partial, .. }
            | AssistantMessageEvent::ToolCallStart { partial, .. }
            | AssistantMessageEvent::ToolCallDelta { partial, .. }
            | AssistantMessageEvent::ToolCallEnd { partial, .. } => {
                partial_reasons.push(partial.stop_reason);
            }
            _ => {}
        }
        seen_events.push(ev);
    }
    let message = es.result().await;

    assert_eq!(partial_reasons[0], StopReason::Pending);
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert_eq!(message.usage, sample_usage());
    assert_eq!(message.response_id.as_deref(), Some("resp_1"));
    assert_eq!(message.provider_thinking_level.as_deref(), Some("high"));
    assert_eq!(message.model, "auto");
    assert_eq!(message.provider.as_str(), "radius");
    assert_eq!(
        message.content,
        vec![
            AssistantContentBlock::Text(TextContent {
                text: "Hello".to_string(),
                text_signature: None,
            }),
            AssistantContentBlock::ToolCall(ToolCall {
                id: "call_1".to_string(),
                name: "read".to_string(),
                namespace: None,
                arguments: [("path".to_string(), json!("a.txt"))].into_iter().collect(),
                thought_signature: None,
            }),
        ]
    );
    assert!(
        seen_events
            .iter()
            .any(|e| matches!(e, AssistantMessageEvent::TextDelta { .. }))
    );
    assert_eq!(
        seen_events
            .iter()
            .filter(|e| matches!(e, AssistantMessageEvent::ToolCallEnd { .. }))
            .count(),
        1
    );

    let reqs = recorded.lock().unwrap();
    assert_eq!(reqs.len(), 1);
    let r = &reqs[0];
    assert!(r.url.ends_with("/v1/messages"));
    assert_eq!(
        r.headers.get("authorization").map(String::as_str),
        Some("Bearer test-key")
    );
    assert_eq!(r.headers.get("x-custom").map(String::as_str), Some("1"));
    let body = r.body.as_ref().unwrap();
    assert_eq!(body["model"], "auto");
    assert_eq!(body["options"]["maxTokens"], 100);
    assert_eq!(body["options"]["sessionId"], "session-1");
    assert_eq!(body["options"]["toolChoice"], "auto");
}

#[tokio::test]
async fn forwards_wire_events_debug_query_headers_and_handles_errors() {
    let wire_events = vec![
        json!({ "type": "start" }),
        json!({ "type": "text_start", "contentIndex": 0 }),
        json!({ "type": "text_delta", "contentIndex": 0, "delta": "Hello", "gatewayField": "upstream-value" }),
        json!({ "type": "text_end", "contentIndex": 0, "content": "Hello" }),
        json!({ "type": "done", "reason": "stop", "usage": sample_usage_json(), "responseId": "resp_1" }),
    ];
    let (fetch, _) = sse_fetch(wire_events.clone(), Vec::new());
    let model = create_model("http://127.0.0.1:9999/v1");
    let received: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let rec = Arc::clone(&received);
    let cb: OnProviderStreamEventFn = Arc::new(move |ev, _m| {
        let rec = Arc::clone(&rec);
        let ev = ev.clone();
        Box::pin(async move {
            rec.lock().unwrap().push(ev);
            Ok(())
        })
    });
    let msg = stream_simple(
        &model,
        &normalize_context(&sample_context()),
        Some(
            pi_ai::types::SimpleStreamOptions {
                base: StreamOptions {
                    api_key: Some("test-key".to_string()),
                    fetch: Some(fetch),
                    on_provider_stream_event: Some(cb),
                    ..Default::default()
                },
                ..Default::default()
            }
            .into(),
        ),
    )
    .result()
    .await;
    assert_eq!(msg.stop_reason, StopReason::Stop);
    assert_eq!(msg.response_id.as_deref(), Some("resp_1"));
    assert_eq!(*received.lock().unwrap(), wire_events);

    // debug=1 and onResponse headers
    let (fetch_dbg, req_dbg) = sse_fetch(
        vec![json!({ "type": "done", "reason": "stop", "usage": sample_usage_json() })],
        vec![("x-pi-gateway-upstream-provider", "anthropic")],
    );
    let obs_hdrs: Arc<Mutex<Option<IndexMap<String, String>>>> = Arc::new(Mutex::new(None));
    let oh = Arc::clone(&obs_hdrs);
    let on_resp: OnResponseFn<Model> = Arc::new(move |resp, _m| {
        let oh = Arc::clone(&oh);
        Box::pin(async move {
            *oh.lock().unwrap() = Some(resp.headers);
            Ok(())
        })
    });
    let mut simple_with_debug = pi_ai::types::ProviderStreamOptions::from(PiMessagesOptions {
        base: StreamOptions {
            api_key: Some("test-key".to_string()),
            fetch: Some(fetch_dbg),
            on_response: Some(on_resp),
            ..Default::default()
        },
        debug: Some(true),
        ..Default::default()
    });
    simple_with_debug.extra.insert("debug".to_string(), json!(true));
    let dbg_msg =
        pi_ai::api::pi_messages::stream(&model, &normalize_context(&sample_context()), Some(simple_with_debug))
            .result()
            .await;
    assert_eq!(dbg_msg.stop_reason, StopReason::Stop);
    assert!(req_dbg.lock().unwrap()[0].url.ends_with("/v1/messages?debug=1"));
    assert_eq!(
        obs_hdrs
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|h| h.get("x-pi-gateway-upstream-provider"))
            .map(String::as_str),
        Some("anthropic")
    );

    // 401 diagnostics
    let err_fetch: FetchFunction = Arc::new(|_req| {
        Box::pin(async {
            let mut h = Headers::new();
            h.set("content-type", "application/json");
            Ok(FetchResponse::new(
                401,
                h,
                br#"{"error":{"message":"Token expired","code":"unauthorized"}}"#.to_vec(),
            ))
        })
    });
    let err_msg = stream(
        &model,
        &normalize_context(&sample_context()),
        Some(
            PiMessagesOptions {
                base: StreamOptions {
                    api_key: Some("stale".to_string()),
                    fetch: Some(err_fetch),
                    ..Default::default()
                },
                ..Default::default()
            }
            .into(),
        ),
    )
    .result()
    .await;
    assert_eq!(err_msg.stop_reason, StopReason::Error);
    let emsg = err_msg.error_message.as_deref().unwrap();
    assert!(emsg.contains("401"), "{emsg}");
    assert!(emsg.contains("Token expired"), "{emsg}");
    assert!(emsg.contains("unauthorized"), "{emsg}");
    let diag = &err_msg.diagnostics.as_ref().unwrap()[0];
    assert_eq!(diag.r#type, "pi_messages_response_failure");
    assert_eq!(
        diag.details
            .as_ref()
            .and_then(|d| d.get("status"))
            .and_then(Value::as_u64),
        Some(401)
    );

    // Server-sent error event
    let (sse_err_fetch, _) = sse_fetch(
        vec![
            json!({ "type": "start" }),
            json!({ "type": "error", "reason": "error", "usage": sample_usage_json(), "errorMessage": "Upstream failed" }),
        ],
        Vec::new(),
    );
    let sse_err_msg = stream(
        &model,
        &normalize_context(&sample_context()),
        Some(
            PiMessagesOptions {
                base: StreamOptions {
                    api_key: Some("test-key".to_string()),
                    fetch: Some(sse_err_fetch),
                    ..Default::default()
                },
                ..Default::default()
            }
            .into(),
        ),
    )
    .result()
    .await;
    assert_eq!(sse_err_msg.stop_reason, StopReason::Error);
    assert_eq!(sse_err_msg.error_message.as_deref(), Some("Upstream failed"));

    // No API key
    let no_key_msg = stream(&model, &normalize_context(&sample_context()), None)
        .result()
        .await;
    assert_eq!(no_key_msg.stop_reason, StopReason::Error);
    assert!(
        no_key_msg
            .error_message
            .as_deref()
            .unwrap()
            .contains("No API key provided")
    );

    // Truncated stream without terminal event
    let (trunc_fetch, _) = sse_fetch(
        vec![
            json!({ "type": "start" }),
            json!({ "type": "text_start", "contentIndex": 0 }),
            json!({ "type": "text_delta", "contentIndex": 0, "delta": "partial" }),
        ],
        Vec::new(),
    );
    let trunc_msg = stream(
        &model,
        &normalize_context(&sample_context()),
        Some(
            PiMessagesOptions {
                base: StreamOptions {
                    api_key: Some("test-key".to_string()),
                    fetch: Some(trunc_fetch),
                    ..Default::default()
                },
                ..Default::default()
            }
            .into(),
        ),
    )
    .result()
    .await;
    assert_eq!(trunc_msg.stop_reason, StopReason::Error);
    assert!(
        trunc_msg
            .error_message
            .as_deref()
            .unwrap()
            .contains("stream ended without a terminal event")
    );
}

#[test]
fn is_registered_as_builtin_api_provider() {
    register_built_in_api_providers();
    assert!(get_api_provider("pi-messages").is_some());
}
