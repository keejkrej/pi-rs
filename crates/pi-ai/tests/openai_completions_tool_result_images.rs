//! Port of packages/ai/test/openai-completions-tool-result-images.test.ts

use indexmap::IndexMap;
use pi_ai::api::openai_completions::{ResolvedOpenAICompletionsCompat, convert_messages, get_compat};
use pi_ai::compat::{get_model, normalize_context};
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, CacheControlFormat, Context, ImageContent, Message, Model, ModelInput,
    StopReason, TextContent, ToolCall, ToolResultMessage, Usage, UserContentBlock, UserMessage, UserMessageContent,
};
use pi_js::time;
use serde_json::{Value, json};

fn gpt4o_image_model() -> Model {
    let mut base = get_model("openai", "gpt-4o-mini").unwrap();
    base.compat = None;
    base.api = "openai-completions".to_owned();
    base.input = vec![ModelInput::Text, ModelInput::Image];
    base
}

fn image_compat(model: &Model) -> ResolvedOpenAICompletionsCompat {
    let mut c = get_compat(model);
    c.cache_control_format = Some(CacheControlFormat::Anthropic);
    c
}

fn build_tool_result(tool_call_id: &str, timestamp: i64) -> ToolResultMessage {
    ToolResultMessage {
        tool_call_id: tool_call_id.to_owned(),
        tool_name: "read".to_owned(),
        content: vec![
            UserContentBlock::Text(TextContent::new("Read image file [image/png]")),
            UserContentBlock::Image(ImageContent {
                data: "ZmFrZQ==".to_owned(),
                mime_type: "image/png".to_owned(),
            }),
        ],
        details: None,
        is_error: false,
        nested_calls: None,
        usage: None,
        timestamp,
    }
}

fn build_empty_tool_result(tool_call_id: &str, timestamp: i64) -> ToolResultMessage {
    ToolResultMessage {
        tool_call_id: tool_call_id.to_owned(),
        tool_name: "bash".to_owned(),
        content: vec![UserContentBlock::Text(TextContent::new(""))],
        details: None,
        is_error: false,
        nested_calls: None,
        usage: None,
        timestamp,
    }
}

#[test]
fn omits_empty_text_parts_from_user_messages_with_images() {
    let model = gpt4o_image_model();
    let compat = image_compat(&model);
    let context = normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Blocks(vec![
                UserContentBlock::Text(TextContent::new("")),
                UserContentBlock::Image(ImageContent {
                    data: "ZmFrZQ==".to_owned(),
                    mime_type: "image/png".to_owned(),
                }),
            ]),
            timestamp: time::now_ms(),
        })],
        tools: None,
    });

    assert_eq!(
        convert_messages(&model, &context, &compat, None),
        vec![json!({
            "role": "user",
            "content": [{
                "type": "image_url",
                "image_url": { "url": "data:image/png;base64,ZmFrZQ==" },
            }],
        })]
    );
}

#[test]
fn batches_tool_result_images_after_consecutive_tool_results() {
    let model = gpt4o_image_model();
    let compat = image_compat(&model);
    let now = time::now_ms();

    let mut a1 = IndexMap::new();
    a1.insert("path".to_owned(), json!("img-1.png"));
    let mut a2 = IndexMap::new();
    a2.insert("path".to_owned(), json!("img-2.png"));

    let assistant = AssistantMessage {
        content: vec![
            AssistantContentBlock::ToolCall(ToolCall::new("tool-1", "read", a1)),
            AssistantContentBlock::ToolCall(ToolCall::new("tool-2", "read", a2)),
        ],
        api: model.api.clone(),
        provider: model.provider.clone(),
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
        timestamp: now,
    };

    let context = normalize_context(&Context {
        system_prompt: None,
        messages: vec![
            Message::User(UserMessage {
                content: UserMessageContent::Text("Read the images".to_owned()),
                timestamp: now - 2,
            }),
            Message::Assistant(assistant),
            Message::ToolResult(build_tool_result("tool-1", now + 1)),
            Message::ToolResult(build_tool_result("tool-2", now + 2)),
        ],
        tools: None,
    });

    let messages = convert_messages(&model, &context, &compat, None);
    let roles: Vec<&str> = messages
        .iter()
        .filter_map(|m| m.get("role").and_then(Value::as_str))
        .collect();
    assert_eq!(roles, vec!["user", "assistant", "tool", "tool", "user"]);

    let image_msg = messages.last().unwrap();
    let parts = image_msg.get("content").and_then(Value::as_array).unwrap();
    let image_count = parts
        .iter()
        .filter(|p| p.get("type").and_then(Value::as_str) == Some("image_url"))
        .count();
    assert_eq!(image_count, 2);
}

#[test]
fn uses_no_tool_output_placeholder_for_empty_tool_results_without_images() {
    let model = gpt4o_image_model();
    let compat = image_compat(&model);
    let now = time::now_ms();

    let mut a1 = IndexMap::new();
    a1.insert("command".to_owned(), json!("true"));

    let assistant = AssistantMessage {
        content: vec![AssistantContentBlock::ToolCall(ToolCall::new("tool-1", "bash", a1))],
        api: model.api.clone(),
        provider: model.provider.clone(),
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
        timestamp: now,
    };

    let context = normalize_context(&Context {
        system_prompt: None,
        messages: vec![
            Message::User(UserMessage {
                content: UserMessageContent::Text("Run the command".to_owned()),
                timestamp: now - 1,
            }),
            Message::Assistant(assistant),
            Message::ToolResult(build_empty_tool_result("tool-1", now + 1)),
        ],
        tools: None,
    });

    let messages = convert_messages(&model, &context, &compat, None);
    let tool_msg = messages
        .iter()
        .find(|m| m.get("role").and_then(Value::as_str) == Some("tool"))
        .expect("expected tool message");
    let content = tool_msg.get("content").and_then(Value::as_str).unwrap();
    assert_eq!(content, "(no tool output)");
    assert!(!content.contains("see attached image"));
}
