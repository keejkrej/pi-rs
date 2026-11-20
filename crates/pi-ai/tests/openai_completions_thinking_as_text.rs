//! Port of packages/ai/test/openai-completions-thinking-as-text.test.ts

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use pi_ai::api::openai_completions::{
    ResolvedOpenAICompletionsCompat, convert_messages, get_compat, stream as stream_openai_completions,
};
use pi_ai::compat::normalize_context;
use pi_ai::types::{
    ApiStreamOptions, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, MaxTokensField, Message,
    Model, ModelCompat, ModelCost, ModelInput, OpenAICompletionsCompat, SessionAffinityFormat, StopReason,
    StreamOptions, TextContent, ThinkingContent, ThinkingFormat, Usage, UserMessage, UserMessageContent,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn thinking_as_text_model_compat() -> OpenAICompletionsCompat {
    OpenAICompletionsCompat {
        supports_store: Some(true),
        supports_developer_role: Some(true),
        supports_reasoning_effort: Some(true),
        supports_usage_in_streaming: Some(true),
        supports_finish_reason: Some(true),
        max_tokens_field: Some(MaxTokensField::MaxCompletionTokens),
        requires_tool_result_name: Some(false),
        requires_assistant_after_tool_result: Some(false),
        requires_thinking_as_text: Some(true),
        requires_reasoning_content_on_assistant_messages: Some(false),
        thinking_format: Some(ThinkingFormat::OpenAI),
        supports_strict_mode: Some(true),
        supports_open_ai_grammar_tools: Some(false),
        supports_mid_convo_system_messages: Some(false),
        supports_mid_convo_tool_additions: Some(false),
        send_session_affinity_headers: Some(false),
        session_affinity_format: Some(SessionAffinityFormat::OpenAI),
        supports_long_cache_retention: Some(true),
        zai_tool_stream: Some(false),
        supports_thinking_token_budget: Some(false),
        ..Default::default()
    }
}

fn build_model(base_url: &str) -> Model {
    Model {
        r#type: None,
        id: "repro-model".to_owned(),
        name: "Repro Model".to_owned(),
        api: "openai-completions".to_owned(),
        provider: "repro-provider".to_owned(),
        base_url: base_url.to_owned(),
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 128_000,
        max_tokens: 4096,
        headers: None,
        sampling_params: None,
        compat: Some(ModelCompat::OpenAICompletions(thinking_as_text_model_compat())),
        sampling_params_by_thinking_level: None,
    }
}

fn build_assistant(content: Vec<AssistantContentBlock>) -> AssistantMessage {
    AssistantMessage {
        content,
        api: "openai-completions".to_owned(),
        provider: "repro-provider".to_owned(),
        model: "repro-model".to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 2,
        duration_ms: None,
    }
}

fn build_context(assistant: AssistantMessage) -> Context {
    Context {
        system_prompt: None,
        messages: vec![
            Message::User(UserMessage {
                content: UserMessageContent::Text("hello".to_owned()),
                timestamp: 1,
            }),
            Message::Assistant(assistant),
            Message::User(UserMessage {
                content: UserMessageContent::Text("continue".to_owned()),
                timestamp: 3,
            }),
        ],
        tools: None,
    }
}

#[test]
fn serializes_same_model_thinking_plus_text_replay_as_assistant_text_parts() {
    let model = build_model("http://127.0.0.1:1");
    let compat: ResolvedOpenAICompletionsCompat = get_compat(&model);
    let messages = convert_messages(
        &model,
        &normalize_context(&build_context(build_assistant(vec![
            AssistantContentBlock::Thinking(ThinkingContent::new("internal reasoning")),
            AssistantContentBlock::Text(TextContent::new("visible answer")),
        ]))),
        &compat,
        None,
    );

    assert_eq!(
        messages[1],
        json!({
            "role": "assistant",
            "content": [
                { "type": "text", "text": "internal reasoning" },
                { "type": "text", "text": "visible answer" },
            ],
        })
    );
}

#[test]
fn serializes_same_model_thinking_only_replay_as_assistant_text_parts() {
    let model = build_model("http://127.0.0.1:1");
    let compat: ResolvedOpenAICompletionsCompat = get_compat(&model);
    let messages = convert_messages(
        &model,
        &normalize_context(&build_context(build_assistant(vec![AssistantContentBlock::Thinking(
            ThinkingContent::new("internal reasoning"),
        )]))),
        &compat,
        None,
    );

    assert_eq!(
        messages[1],
        json!({
            "role": "assistant",
            "content": [{ "type": "text", "text": "internal reasoning" }],
        })
    );
}

#[tokio::test]
async fn reaches_the_endpoint_when_replay_contains_both_thinking_and_text() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let request_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let rb_c = request_bodies.clone();

    let server_task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        let mut expected_len = None;
        let mut header_end = None;
        loop {
            let n = socket.read(&mut tmp).await.unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if header_end.is_none() {
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    header_end = Some(pos + 4);
                    let headers_str = String::from_utf8_lossy(&buf[..pos]);
                    for line in headers_str.lines() {
                        if let Some((k, v)) = line.split_once(':') {
                            if k.trim().eq_ignore_ascii_case("content-length") {
                                expected_len = v.trim().parse::<usize>().ok();
                            }
                        }
                    }
                }
            }
            if let (Some(h_end), Some(clen)) = (header_end, expected_len) {
                if buf.len() >= h_end + clen {
                    let body: Value = serde_json::from_slice(&buf[h_end..h_end + clen]).unwrap();
                    rb_c.lock().unwrap().push(body);
                    break;
                }
            }
        }

        let c1 = json!({
            "id": "chatcmpl-repro",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": "repro-model",
            "choices": [{ "index": 0, "delta": { "role": "assistant", "content": "ok" }, "finish_reason": null }],
        });
        let c2 = json!({
            "id": "chatcmpl-repro",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": "repro-model",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
        });
        let sse = format!("data: {c1}\n\ndata: {c2}\n\ndata: [DONE]\n\n");
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            sse.len(),
            sse
        );
        socket.write_all(resp.as_bytes()).await.unwrap();
        let _ = socket.shutdown().await;
    });

    let ctx = normalize_context(&build_context(build_assistant(vec![
        AssistantContentBlock::Thinking(ThinkingContent::new("internal reasoning")),
        AssistantContentBlock::Text(TextContent::new("visible answer")),
    ])));
    let mut es = stream_openai_completions(
        &build_model(&format!("http://127.0.0.1:{}", addr.port())),
        &ctx,
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("test-key".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        }),
    );

    let mut events = Vec::new();
    while let Some(ev) = es.next().await {
        events.push(ev);
    }
    server_task.await.unwrap();

    let bodies = request_bodies.lock().unwrap();
    assert_eq!(bodies.len(), 1);
    assert_eq!(
        bodies[0].get("messages").and_then(Value::as_array).unwrap()[1],
        json!({
            "role": "assistant",
            "content": [
                { "type": "text", "text": "internal reasoning" },
                { "type": "text", "text": "visible answer" },
            ],
        })
    );
    assert!(matches!(events.last(), Some(AssistantMessageEvent::Done { .. })));
}
