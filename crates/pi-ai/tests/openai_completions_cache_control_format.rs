//! Port of packages/ai/test/openai-completions-cache-control-format.test.ts

use std::sync::{Arc, Mutex};

use pi_ai::api::openai_completions::stream as stream_openai_completions;
use pi_ai::compat::{get_model, normalize_context};
use pi_ai::types::{
    ApiStreamOptions, AssistantContentBlock, AssistantMessage, CacheControlFormat, CacheRetention, Context, Message,
    Model, ModelCompat, ModelCost, ModelInput, OpenAICompletionsCompat, StopReason, StreamOptions, TextContent, Tool,
    ToolCall, ToolResultMessage, Usage, UserContentBlock, UserMessage, UserMessageContent,
};
use pi_js::fetch::{Headers, Response};
use pi_js::time;
use pi_js::vendor::typebox::t;
use serde_json::{Value, json};

fn ok_sse() -> String {
    format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({
            "id": "chatcmpl-test",
            "choices": [{ "delta": {}, "finish_reason": "stop" }],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 1,
                "prompt_tokens_details": { "cached_tokens": 0 },
                "completion_tokens_details": { "reasoning_tokens": 0 },
            }
        })
    )
}

async fn capture_payload(
    model: &Model,
    cache_retention: Option<CacheRetention>,
    messages: Option<Vec<Message>>,
) -> Value {
    let captured = Arc::new(Mutex::new(None::<Value>));
    let cap_c = captured.clone();

    let ctx = normalize_context(&Context {
        system_prompt: Some("System prompt".to_owned()),
        messages: messages.unwrap_or_else(|| {
            vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Hello".to_owned()),
                timestamp: time::now_ms(),
            })]
        }),
        tools: Some(vec![Tool {
            name: "read".to_owned(),
            description: "Read a file".to_owned(),
            parameters: t::object([("path", t::string())]),
            constrained_sampling: None,
        }]),
    });

    stream_openai_completions(
        model,
        &ctx,
        Some(ApiStreamOptions {
            base: StreamOptions {
                api_key: Some("test-key".to_owned()),
                cache_retention,
                on_payload: Some(Arc::new(move |params, _| {
                    let c = cap_c.clone();
                    Box::pin(async move {
                        *c.lock().unwrap() = Some(params);
                        Ok(None)
                    })
                })),
                fetch: Some(Arc::new(|_| {
                    Box::pin(async {
                        Ok(Response::new(
                            200,
                            Headers::from_pairs([("content-type", "text/event-stream")]),
                            ok_sse(),
                        ))
                    })
                })),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;

    captured.lock().unwrap().take().unwrap()
}

fn get_instruction_message(params: &Value) -> Option<&Value> {
    params.get("messages").and_then(Value::as_array).and_then(|msgs| {
        msgs.iter()
            .find(|m| matches!(m.get("role").and_then(Value::as_str), Some("system" | "developer")))
    })
}

fn expect_anthropic_cache_markers(params: &Value) {
    let inst = get_instruction_message(params).expect("expected instruction message");
    let inst_content = inst
        .get("content")
        .and_then(Value::as_array)
        .expect("expected instruction content array");
    assert_eq!(
        inst_content[0].get("cache_control"),
        Some(&json!({ "type": "ephemeral" }))
    );

    let tools = params.get("tools").and_then(Value::as_array).unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].get("cache_control"), Some(&json!({ "type": "ephemeral" })));

    let msgs = params.get("messages").and_then(Value::as_array).unwrap();
    let last = msgs.last().unwrap();
    assert_eq!(last.get("role"), Some(&json!("user")));
    let last_content = last.get("content").and_then(Value::as_array).unwrap();
    assert_eq!(
        last_content[0].get("cache_control"),
        Some(&json!({ "type": "ephemeral" }))
    );
}

#[tokio::test]
async fn applies_anthropic_style_cache_markers_when_model_compat_enables_them() {
    let model = Model {
        r#type: None,
        id: "custom-qwen".to_owned(),
        name: "Custom Qwen".to_owned(),
        api: "openai-completions".to_owned(),
        provider: "openrouter".to_owned(),
        base_url: "https://example.com/v1".to_owned(),
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 128_000,
        max_tokens: 32_000,
        headers: None,
        sampling_params: None,
        compat: Some(ModelCompat::OpenAICompletions(OpenAICompletionsCompat {
            cache_control_format: Some(CacheControlFormat::Anthropic),
            ..Default::default()
        })),
        sampling_params_by_thinking_level: None,
    };

    let params = capture_payload(&model, None, None).await;
    expect_anthropic_cache_markers(&params);
}

#[tokio::test]
async fn preserves_anthropic_style_cache_markers_for_openrouter_anthropic_batch_aliases() {
    let model = get_model("openrouter", "anthropic/claude-fable-5.1:batch").unwrap();
    let params = capture_payload(&model, None, None).await;
    expect_anthropic_cache_markers(&params);
}

#[tokio::test]
async fn moves_the_conversation_cache_marker_to_a_tool_result() {
    let model = get_model("openrouter", "anthropic/claude-fable-5.1:batch").unwrap();
    let ts = time::now_ms();

    let mut args = indexmap::IndexMap::new();
    args.insert("path".to_owned(), json!("README.md"));

    let messages = vec![
        Message::User(UserMessage {
            content: UserMessageContent::Text("Read the file".to_owned()),
            timestamp: ts,
        }),
        Message::Assistant(AssistantMessage {
            content: vec![AssistantContentBlock::ToolCall(ToolCall::new("call_1", "read", args))],
            api: "openai-completions".to_owned(),
            provider: "openrouter".to_owned(),
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
            timestamp: ts,
            duration_ms: None,
        }),
        Message::ToolResult(ToolResultMessage {
            tool_call_id: "call_1".to_owned(),
            tool_name: "read".to_owned(),
            content: vec![UserContentBlock::Text(TextContent::new("file contents"))],
            details: None,
            is_error: false,
            nested_calls: None,
            usage: None,
            timestamp: ts,
            duration_ms: None,
        }),
    ];

    let params = capture_payload(&model, None, Some(messages)).await;
    let msgs = params.get("messages").and_then(Value::as_array).unwrap();
    let user_msg = msgs.iter().find(|m| m.get("role") == Some(&json!("user"))).unwrap();
    assert_eq!(user_msg.get("content"), Some(&json!("Read the file")));

    let tool_msg = msgs.last().unwrap();
    assert_eq!(tool_msg.get("role"), Some(&json!("tool")));
    let tool_content = tool_msg.get("content").and_then(Value::as_array).unwrap();
    assert_eq!(
        tool_content[0].get("cache_control"),
        Some(&json!({ "type": "ephemeral" }))
    );
}

#[tokio::test]
async fn omits_anthropic_style_cache_markers_when_cache_retention_is_none() {
    let model = Model {
        r#type: None,
        id: "custom-qwen".to_owned(),
        name: "Custom Qwen".to_owned(),
        api: "openai-completions".to_owned(),
        provider: "openrouter".to_owned(),
        base_url: "https://example.com/v1".to_owned(),
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 128_000,
        max_tokens: 32_000,
        headers: None,
        sampling_params: None,
        compat: Some(ModelCompat::OpenAICompletions(OpenAICompletionsCompat {
            cache_control_format: Some(CacheControlFormat::Anthropic),
            ..Default::default()
        })),
        sampling_params_by_thinking_level: None,
    };

    let params = capture_payload(&model, Some(CacheRetention::None), None).await;
    let inst = get_instruction_message(&params).unwrap();
    assert!(!inst.get("content").unwrap().is_array());
    let tools = params.get("tools").and_then(Value::as_array).unwrap();
    assert!(tools[0].get("cache_control").is_none());
    let msgs = params.get("messages").and_then(Value::as_array).unwrap();
    assert!(msgs.last().unwrap().get("content").unwrap().is_string());
}
