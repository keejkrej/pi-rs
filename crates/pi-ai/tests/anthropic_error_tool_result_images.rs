//! Port of packages/ai/test/anthropic-error-tool-result-images.test.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::compat::stream_simple;
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, Context, ImageContent, Message, Model, ModelCost, ModelInput,
    SimpleStreamOptions, StopReason, StreamOptions, TextContent, ToolCall, ToolResultMessage, Usage, UserContentBlock,
    UserMessage, UserMessageContent,
};
use pi_js::time;
use serde_json::{Value, json};

fn model() -> Model {
    Model {
        r#type: None,
        id: "claude-test".to_owned(),
        name: "Claude Test".to_owned(),
        api: "anthropic-messages".to_owned(),
        provider: "anthropic".into(),
        base_url: "http://127.0.0.1:9".to_owned(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text, ModelInput::Image],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 200_000,
        max_tokens: 1024,
        headers: None,
        sampling_params: None,
        compat: None,
        sampling_params_by_thinking_level: None,
    }
}

fn assistant() -> AssistantMessage {
    AssistantMessage {
        content: vec![
            AssistantContentBlock::ToolCall(ToolCall::new("call-1", "codemode", IndexMap::new())),
            AssistantContentBlock::ToolCall(ToolCall::new("call-2", "read", IndexMap::new())),
        ],
        api: "anthropic-messages".to_owned(),
        provider: "anthropic".into(),
        model: "claude-test".to_owned(),
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
        timestamp: time::now_ms(),
        duration_ms: None,
    }
}

async fn capture_payload(context: &Context) -> Value {
    let captured: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let cap = Arc::clone(&captured);
    stream_simple(
        &model(),
        context,
        Some(SimpleStreamOptions {
            base: StreamOptions {
                api_key: Some("fake-key".to_owned()),
                cache_retention: Some(pi_ai::types::CacheRetention::None),
                on_payload: Some(Arc::new(move |payload, _| {
                    let cap = Arc::clone(&cap);
                    Box::pin(async move {
                        *cap.lock().unwrap() = Some(payload);
                        Err(pi_js::Error::msg("payload captured"))
                    })
                })),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .result()
    .await;
    captured.lock().unwrap().clone().expect("Expected payload capture")
}

fn image_block(data: &str) -> Value {
    json!({
        "type": "image",
        "source": { "type": "base64", "media_type": "image/png", "data": data }
    })
}

// Regression for #10794: Anthropic rejects error tool results that contain images.
#[tokio::test]
async fn moves_images_from_error_tool_results_after_the_turn_and_keeps_others_unchanged() {
    let now = time::now_ms();
    let mut asst = assistant();
    let payload_err = capture_payload(&Context {
        system_prompt: None,
        messages: vec![
            Message::User(UserMessage {
                content: UserMessageContent::Text("Show the screenshots".to_owned()),
                timestamp: now,
            }),
            Message::Assistant(asst.clone()),
            Message::ToolResult(ToolResultMessage {
                tool_call_id: "call-1".to_owned(),
                tool_name: "codemode".to_owned(),
                content: vec![
                    UserContentBlock::Text(TextContent::new("Script failed")),
                    UserContentBlock::Image(ImageContent {
                        data: "AAAA".to_owned(),
                        mime_type: "image/png".to_owned(),
                    }),
                    UserContentBlock::Image(ImageContent {
                        data: "CCCC".to_owned(),
                        mime_type: "image/png".to_owned(),
                    }),
                    UserContentBlock::Text(TextContent::new("Script error")),
                ],
                details: None,
                is_error: true,
                nested_calls: None,
                usage: None,
                timestamp: now,
                duration_ms: None,
            }),
            Message::ToolResult(ToolResultMessage {
                tool_call_id: "call-2".to_owned(),
                tool_name: "read".to_owned(),
                content: vec![UserContentBlock::Image(ImageContent {
                    data: "BBBB".to_owned(),
                    mime_type: "image/png".to_owned(),
                })],
                details: None,
                is_error: false,
                nested_calls: None,
                usage: None,
                timestamp: now,
                duration_ms: None,
            }),
        ],
        tools: None,
    })
    .await;

    assert_eq!(
        payload_err["messages"][2],
        json!({
            "role": "user",
            "content": [
                {
                    "type": "tool_result",
                    "tool_use_id": "call-1",
                    "content": "Script failed\n(image attached after the tool results)\nScript error",
                    "is_error": true,
                },
                {
                    "type": "tool_result",
                    "tool_use_id": "call-2",
                    "content": [{ "type": "text", "text": "(see attached image)" }, image_block("BBBB")],
                    "is_error": false,
                },
                { "type": "text", "text": "Images from the error result of tool call call-1 (codemode):" },
                image_block("AAAA"),
                image_block("CCCC"),
            ],
        })
    );

    asst.content.truncate(1);
    let payload_no_img = capture_payload(&Context {
        system_prompt: None,
        messages: vec![
            Message::User(UserMessage {
                content: UserMessageContent::Text("Run it".to_owned()),
                timestamp: now,
            }),
            Message::Assistant(asst),
            Message::ToolResult(ToolResultMessage {
                tool_call_id: "call-1".to_owned(),
                tool_name: "codemode".to_owned(),
                content: vec![UserContentBlock::Text(TextContent::new("Script failed"))],
                details: None,
                is_error: true,
                nested_calls: None,
                usage: None,
                timestamp: now,
                duration_ms: None,
            }),
        ],
        tools: None,
    })
    .await;

    assert_eq!(
        payload_no_img["messages"][2],
        json!({
            "role": "user",
            "content": [{ "type": "tool_result", "tool_use_id": "call-1", "content": "Script failed", "is_error": true }],
        })
    );
}
