//! Port of packages/ai/test/openai-responses-message-id.test.ts

use indexmap::IndexSet;
use pi_ai::api::openai_responses_shared::convert_responses_messages;
use pi_ai::compat::{get_model, normalize_context};
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, Context, Message, StopReason, TextContent, ThinkingContent, Usage,
    UserMessage, UserMessageContent,
};
use pi_js::time;
use serde_json::Value;

#[test]
fn generates_unique_fallback_message_ids_for_multiple_text_blocks_in_one_assistant_turn() {
    let model = get_model("openai-codex", "gpt-5.5").unwrap();
    let now = time::now_ms();

    let assistant = AssistantMessage {
        content: vec![
            AssistantContentBlock::Thinking(ThinkingContent::new("private reasoning")),
            AssistantContentBlock::Text(TextContent::new("visible answer")),
        ],
        api: "anthropic-messages".to_owned(),
        provider: "anthropic".to_owned(),
        model: "claude-opus-4-8".to_owned(),
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
        timestamp: now - 1000,
    };

    let context = normalize_context(&Context {
        system_prompt: Some("You are concise.".to_owned()),
        messages: vec![
            Message::User(UserMessage {
                content: UserMessageContent::Text("hello".to_owned()),
                timestamp: now - 2000,
            }),
            Message::Assistant(assistant),
        ],
        tools: None,
    });

    let mut allowed = IndexSet::new();
    allowed.insert("openai".to_owned());
    allowed.insert("openai-codex".to_owned());
    allowed.insert("opencode".to_owned());

    let input = convert_responses_messages(&model, &context, &allowed, None);
    let message_ids: Vec<String> = input
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("message"))
        .filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect();

    assert_eq!(message_ids, vec!["msg_pi_1", "msg_pi_1_1"]);
    let unique: IndexSet<&str> = message_ids.iter().map(String::as_str).collect();
    assert_eq!(unique.len(), message_ids.len());
}
