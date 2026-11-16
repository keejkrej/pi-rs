//! Port of packages/agent/test/proxy.test.ts

mod support;

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use pi_agent_core::{ProxyAssistantMessageEvent, ProxyStreamOptions, stream_proxy};
use pi_ai::{AssistantContentBlock, AssistantMessageEvent, Context, Model, StopReason, TranscriptContext, Usage};
use pi_js::abort::AbortController;
use pi_js::fetch::testing::{chunked_response, mock_fetch, response};
use pi_js::time::performance_now;
use serde_json::{Value, json};

fn test_model() -> Model {
    serde_json::from_value(json!({
        "id": "gpt-5.4",
        "name": "GPT-5.4",
        "api": "openai-responses",
        "provider": "openai",
        "baseUrl": "https://api.openai.com/v1",
        "reasoning": true,
        "input": ["text"],
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": 400000,
        "maxTokens": 128000
    }))
    .expect("valid test model")
}

fn empty_usage() -> Usage {
    serde_json::from_value(json!({
        "input": 0,
        "output": 0,
        "cacheRead": 0,
        "cacheWrite": 0,
        "totalTokens": 0,
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
    }))
    .expect("valid usage")
}

fn empty_context() -> Context {
    Context::Transcript(TranscriptContext { messages: Vec::new() })
}

fn sse_body(events: &[Value]) -> String {
    events
        .iter()
        .map(|ev| format!("data: {}\n\n", pi_js::json::stringify(ev)))
        .collect()
}

fn event_kind(event: &AssistantMessageEvent) -> &'static str {
    match event {
        AssistantMessageEvent::Start { .. } => "start",
        AssistantMessageEvent::TextStart { .. } => "text_start",
        AssistantMessageEvent::TextDelta { .. } => "text_delta",
        AssistantMessageEvent::TextEnd { .. } => "text_end",
        AssistantMessageEvent::ThinkingStart { .. } => "thinking_start",
        AssistantMessageEvent::ThinkingDelta { .. } => "thinking_delta",
        AssistantMessageEvent::ThinkingEnd { .. } => "thinking_end",
        AssistantMessageEvent::ToolCallStart { .. } => "toolcall_start",
        AssistantMessageEvent::ToolCallDelta { .. } => "toolcall_delta",
        AssistantMessageEvent::ToolCallEnd { .. } => "toolcall_end",
        AssistantMessageEvent::Done { .. } => "done",
        AssistantMessageEvent::Error { .. } => "error",
    }
}

#[tokio::test]
async fn preserves_tool_call_metadata_received_only_on_toolcall_end() {
    let body = sse_body(&[
        json!({ "type": "start" }),
        json!({
            "type": "toolcall_start",
            "contentIndex": 0,
            "id": "call_test|fc_test",
            "toolName": "lookup"
        }),
        json!({
            "type": "toolcall_delta",
            "contentIndex": 0,
            "delta": "{\"value\":\"hello\"}"
        }),
        json!({
            "type": "toolcall_end",
            "contentIndex": 0,
            "toolCall": {
                "type": "toolCall",
                "id": "call_test|fc_test",
                "name": "lookup",
                "arguments": { "value": "hello" },
                "namespace": "dynamic_tools"
            }
        }),
        json!({ "type": "done", "reason": "toolUse", "usage": empty_usage() }),
    ]);

    let _guard = mock_fetch(move |_req| {
        let body = body.clone();
        Box::pin(async move { Ok(response(200, &[], body)) })
    });

    let mut stream = stream_proxy(
        test_model(),
        empty_context(),
        ProxyStreamOptions::new("test-token", "https://proxy.example.com"),
    );
    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev);
    }
    let result = stream.result().await;

    let end_event = events
        .iter()
        .find(|ev| matches!(ev, AssistantMessageEvent::ToolCallEnd { .. }))
        .expect("expected toolcall_end");
    let end_json = serde_json::to_value(end_event).expect("serialize toolcall_end");
    assert_eq!(end_json["toolCall"]["namespace"], "dynamic_tools");

    let first_block = serde_json::to_value(&result.content[0]).expect("serialize first block");
    assert_eq!(first_block["type"], "toolCall");
    assert_eq!(first_block["arguments"], json!({ "value": "hello" }));
    assert_eq!(first_block["namespace"], "dynamic_tools");
}

// Regression tests for https://github.com/earendil-works/pi/issues/8996
#[tokio::test]
async fn processes_terminal_metadata_when_the_event_is_not_newline_terminated() {
    let start = format!("data: {}\n\n", pi_js::json::stringify(&json!({ "type": "start" })));
    let done = format!(
        "data: {}",
        pi_js::json::stringify(&json!({
            "type": "done",
            "reason": "stop",
            "usage": empty_usage(),
            "providerThinkingLevel": "high"
        }))
    );
    let body = format!("{start}{done}");

    let _guard = mock_fetch(move |_req| {
        let body = body.clone();
        Box::pin(async move { Ok(response(200, &[], body)) })
    });

    let mut stream = stream_proxy(
        test_model(),
        empty_context(),
        ProxyStreamOptions::new("test-token", "https://proxy.example.com"),
    );
    let mut kinds = Vec::new();
    while let Some(ev) = stream.next().await {
        kinds.push(event_kind(&ev));
    }
    let result = stream.result().await;

    assert_eq!(kinds, vec!["start", "done"]);
    assert_eq!(result.stop_reason, StopReason::Stop);
    assert_eq!(result.provider_thinking_level.as_deref(), Some("high"));
}

#[tokio::test]
async fn emits_an_error_instead_of_hanging_when_the_stream_ends_without_a_terminal_event() {
    let body = sse_body(&[json!({ "type": "start" })]);
    let _guard = mock_fetch(move |_req| {
        let body = body.clone();
        Box::pin(async move { Ok(response(200, &[], body)) })
    });

    let mut stream = stream_proxy(
        test_model(),
        empty_context(),
        ProxyStreamOptions::new("test-token", "https://proxy.example.com"),
    );
    let mut kinds = Vec::new();
    while let Some(ev) = stream.next().await {
        kinds.push(event_kind(&ev));
    }
    let result = stream.result().await;

    assert_eq!(kinds, vec!["start", "error"]);
    assert_eq!(result.stop_reason, StopReason::Error);
    assert!(
        result
            .error_message
            .as_deref()
            .is_some_and(|m| m.contains("Connection closed by proxy server")),
        "unexpected error_message: {:?}",
        result.error_message
    );
}

// #9265
#[tokio::test]
async fn shows_growing_partial_arguments_without_keeping_raw_text_in_linear_time() {
    let content = "line \"quoted\" \\ end\n".repeat(2_000);
    let text = pi_js::json::stringify(&json!({ "path": "a.ts", "content": content }));
    let mut proxy_events = vec![
        json!({ "type": "start" }),
        json!({ "type": "toolcall_start", "contentIndex": 0, "id": "call_1", "toolName": "write" }),
    ];
    for chunk in text.as_bytes().chunks(64) {
        let delta = std::str::from_utf8(chunk).expect("ascii json chunk");
        proxy_events.push(json!({ "type": "toolcall_delta", "contentIndex": 0, "delta": delta }));
    }
    proxy_events.push(json!({
        "type": "toolcall_end",
        "contentIndex": 0,
        "toolCall": {
            "type": "toolCall",
            "id": "call_1",
            "name": "write",
            "arguments": { "path": "a.ts", "content": content }
        }
    }));
    proxy_events.push(json!({ "type": "done", "reason": "toolUse", "usage": empty_usage() }));

    let body = sse_body(&proxy_events);
    let _guard = mock_fetch(move |_req| {
        let body = body.clone();
        Box::pin(async move { Ok(response(200, &[], body)) })
    });

    let started = performance_now();
    let mut stream = stream_proxy(
        test_model(),
        empty_context(),
        ProxyStreamOptions::new("test-token", "https://proxy.example.com"),
    );

    let mut shown = 0usize;
    let mut deltas = 0usize;
    while let Some(ev) = stream.next().await {
        let AssistantMessageEvent::ToolCallDelta { partial, .. } = ev else {
            continue;
        };
        let block = &partial.content[0];
        let block_json = serde_json::to_value(block).expect("serialize block");
        assert!(block_json.get("partialJson").is_none());
        if let AssistantContentBlock::ToolCall(tc) = block
            && let Some(Value::String(partial_content)) = tc.arguments.get("content")
        {
            if deltas.is_multiple_of(100) {
                assert!(content.starts_with(partial_content));
            }
            assert!(partial_content.len() >= shown);
            shown = partial_content.len();
            deltas += 1;
        }
    }
    let result = stream.result().await;

    assert!(shown > content.len().saturating_sub(128));
    match &result.content[0] {
        AssistantContentBlock::ToolCall(tc) => {
            assert_eq!(tc.id, "call_1");
            assert_eq!(tc.name, "write");
            assert_eq!(tc.arguments.get("path"), Some(&json!("a.ts")));
            assert_eq!(tc.arguments.get("content"), Some(&json!(content)));
        }
        other => panic!("expected tool call block, got {other:?}"),
    }
    assert!(performance_now() - started < 10_000.0);
}

#[tokio::test]
async fn streams_text_and_thinking_with_signatures_and_handles_http_errors() {
    let captured = Arc::new(Mutex::new(None::<(String, String, Option<String>, Option<Value>)>));
    let captured_ref = Arc::clone(&captured);

    let sse_chunks = vec![
        format!(
            "data: {}\n\ndata: {}\n\n",
            pi_js::json::stringify(&ProxyAssistantMessageEvent::Start),
            pi_js::json::stringify(&ProxyAssistantMessageEvent::ThinkingStart { content_index: 0 })
        ),
        format!(
            "data: {}\n\ndata: {}\n\n",
            pi_js::json::stringify(&ProxyAssistantMessageEvent::ThinkingDelta {
                content_index: 0,
                delta: "let me think".to_string(),
            }),
            pi_js::json::stringify(&ProxyAssistantMessageEvent::ThinkingEnd {
                content_index: 0,
                content_signature: Some("sig-think".to_string()),
            })
        ),
        format!(
            "data: {}\n\ndata: {}\n\n",
            pi_js::json::stringify(&ProxyAssistantMessageEvent::TextStart { content_index: 1 }),
            pi_js::json::stringify(&ProxyAssistantMessageEvent::TextDelta {
                content_index: 1,
                delta: "hello world".to_string(),
            })
        ),
        format!(
            "data: {}\n\ndata: {}\n\n",
            pi_js::json::stringify(&ProxyAssistantMessageEvent::TextEnd {
                content_index: 1,
                content_signature: Some("sig-text".to_string()),
            }),
            pi_js::json::stringify(&ProxyAssistantMessageEvent::Done {
                reason: StopReason::Stop,
                usage: empty_usage(),
                provider_thinking_level: None,
            })
        ),
    ];

    let _guard = mock_fetch(move |req| {
        let auth = req.headers().get("authorization").map(str::to_string);
        let parsed = req.body_json();
        *captured_ref.lock().expect("lock") = Some((req.method().to_string(), req.url().to_string(), auth, parsed));
        let chunks = sse_chunks.clone();
        Box::pin(async move { Ok(chunked_response(200, &[("content-type", "text/event-stream")], chunks)) })
    });

    let mut opts = ProxyStreamOptions::new("secret-token", "https://proxy.example.com/");
    opts.temperature = Some(0.25);
    opts.session_id = Some("sess-1".to_string());
    let mut stream = stream_proxy(test_model(), empty_context(), opts);
    while stream.next().await.is_some() {}
    let msg = stream.result().await;

    assert_eq!(msg.stop_reason, StopReason::Stop);
    assert_eq!(msg.content.len(), 2);
    match &msg.content[0] {
        AssistantContentBlock::Thinking(th) => {
            assert_eq!(th.thinking, "let me think");
            assert_eq!(th.thinking_signature.as_deref(), Some("sig-think"));
        }
        other => panic!("expected thinking block, got {other:?}"),
    }
    match &msg.content[1] {
        AssistantContentBlock::Text(t) => {
            assert_eq!(t.text, "hello world");
            assert_eq!(t.text_signature.as_deref(), Some("sig-text"));
        }
        other => panic!("expected text block, got {other:?}"),
    }

    let req_info = captured.lock().expect("lock").clone().expect("captured request");
    assert_eq!(req_info.0, "POST");
    assert_eq!(req_info.1, "https://proxy.example.com/api/stream");
    assert_eq!(req_info.2.as_deref(), Some("Bearer secret-token"));
    let body = req_info.3.expect("json request body");
    assert_eq!(body["options"]["temperature"], 0.25);
    assert_eq!(body["options"]["sessionId"], "sess-1");
}

#[tokio::test]
async fn surfaces_http_error_json_and_abort_stop_reason() {
    let _guard = mock_fetch(|_req| {
        Box::pin(async move {
            Ok(response(
                502,
                &[("content-type", "application/json")],
                r#"{"error":"upstream failed"}"#,
            ))
        })
    });

    let mut stream = stream_proxy(
        test_model(),
        empty_context(),
        ProxyStreamOptions::new("tok", "https://proxy.example.com"),
    );
    let mut kinds = Vec::new();
    while let Some(ev) = stream.next().await {
        kinds.push(event_kind(&ev));
    }
    let result = stream.result().await;
    assert_eq!(kinds, vec!["error"]);
    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(result.error_message.as_deref(), Some("Proxy error: upstream failed"));

    let controller = AbortController::new();
    controller.abort(None);
    let mut opts = ProxyStreamOptions::new("tok", "https://proxy.example.com");
    opts.signal = Some(controller.signal());
    let mut aborted_stream = stream_proxy(test_model(), empty_context(), opts);
    while aborted_stream.next().await.is_some() {}
    let aborted_msg = aborted_stream.result().await;
    assert_eq!(aborted_msg.stop_reason, StopReason::Aborted);
}
