//! Port of packages/ai/test/openai-responses-empty-tool-result.test.ts

use indexmap::IndexSet;
use pi_ai::api::openai_responses_shared::convert_responses_messages;
use pi_ai::compat::{get_model, normalize_context};
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, Context, JsonObject, Message, StopReason, TextContent, ToolCall,
    ToolResultMessage, Usage, UserContentBlock, UserMessage, UserMessageContent,
};
use pi_js::time;
use serde_json::{Value, json};

fn build_empty_tool_result(tool_call_id: &str, timestamp: i64, text: &str) -> Message {
    Message::ToolResult(ToolResultMessage {
        tool_call_id: tool_call_id.to_owned(),
        tool_name: "bash".to_owned(),
        content: vec![UserContentBlock::Text(TextContent::new(text))],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp,
        duration_ms: None,
    })
}

#[test]
fn uses_no_tool_output_placeholder_for_empty_tool_results_without_images() {
    let model = get_model("openai", "gpt-4o-mini").unwrap();
    let now = time::now_ms();
    let mut args = JsonObject::new();
    args.insert("command".to_owned(), json!("true"));

    let assistant = Message::Assistant(AssistantMessage {
        content: vec![AssistantContentBlock::ToolCall(ToolCall::new("tool-1", "bash", args))],
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
        duration_ms: None,
    });

    let allowed: IndexSet<String> = ["openai", "openai-codex", "opencode"]
        .into_iter()
        .map(str::to_owned)
        .collect();

    // Whitespace-only cases: https://github.com/earendil-works/pi/issues/8720
    for text in ["", "\r\n", " \n\t"] {
        let context = normalize_context(&Context {
            system_prompt: None,
            messages: vec![
                Message::User(UserMessage {
                    content: UserMessageContent::Text("Run the command".to_owned()),
                    timestamp: now - 1,
                }),
                assistant.clone(),
                build_empty_tool_result("tool-1", now + 1, text),
            ],
            tools: None,
        });

        let input = convert_responses_messages(&model, &context, &allowed, None);
        let function_call_output = input
            .into_iter()
            .find(|item| item.get("type").and_then(Value::as_str) == Some("function_call_output"))
            .unwrap();

        let output = function_call_output.get("output").and_then(Value::as_str).unwrap();
        assert_eq!(output, "(no tool output)");
        assert!(!output.contains("see attached image"));
    }
}
