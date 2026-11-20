//! Port of packages/ai/test/openai-responses-foreign-toolcall-id.test.ts

use indexmap::{IndexMap, IndexSet};
use pi_ai::api::openai_responses_shared::convert_responses_messages;
use pi_ai::compat::{get_model, normalize_context};
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, Context, Message, StopReason, ToolCall, Usage, UserMessage,
    UserMessageContent,
};
use pi_ai::utils::hash::short_hash;
use pi_js::time;
use serde_json::Value;

const COPILOT_RAW_TOOL_CALL_ID: &str = "call_4VnzVawQXPB9MgYib7CiQFEY|I9b95oN1wD/cHXKTw3PpRkL6KkCtzTJhUxMouMWYwHeTo2j3htzfSk7YPx2vifiIM4g3A8XXyOj8q4Bt6SLUG7gqY1E3ELkrkVQNHglRfUmWj84lqxJY+Puieb3VKyX0FB+83TUzn91cDMF/4gzt990IzqVrc+nIb9RRscRD070Du16q1glydVjWR0SBJsE6TbY/esOjFpqplogQqrajm1eI++f3eLi73R6q7hVusY0QbeFySVxABCjhN0lXB04caBe1rzHjYzul6MAXj7uq+0r17VLq+yrtyYhN12wkmFqHeqTyEei6EFPbMy24Nc+IbJlkP0OCg02W+gOnyBFcbi2ctvJFSOhSjt1CqBdqCnnhwUqXjbWiT0wh3DmLScRgTHmGkaI+oAcQQjfic65nxj+TnEkReA==";

#[test]
fn hashes_foreign_responses_item_ids_that_exceed_64_characters() {
    let model = get_model("openai-codex", "gpt-6.1-sol").unwrap();
    let now = time::now_ms();

    let mut args = IndexMap::new();
    args.insert("path".to_owned(), Value::String("README.md".to_owned()));

    let assistant = AssistantMessage {
        content: vec![AssistantContentBlock::ToolCall(ToolCall::new(
            COPILOT_RAW_TOOL_CALL_ID,
            "read",
            args,
        ))],
        api: "openai-responses".to_owned(),
        provider: "github-copilot".to_owned(),
        model: "gpt-6.1-sol".to_owned(),
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
        timestamp: now - 1000,
        duration_ms: None,
    };

    let context = normalize_context(&Context {
        system_prompt: Some("You are a test harness.".to_owned()),
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
    let function_call = input
        .iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .expect("expected function_call item");

    let (_, raw_item_id) = COPILOT_RAW_TOOL_CALL_ID.split_once('|').unwrap();
    let expected_id = format!("fc_{}", short_hash(raw_item_id));
    let actual_id = function_call
        .get("id")
        .and_then(Value::as_str)
        .expect("expected function_call id");

    assert_eq!(actual_id, expected_id);
    assert!(actual_id.len() <= 64);
    assert!(actual_id.starts_with("fc_"));
    assert!(actual_id[3..].chars().all(|c| c.is_ascii_alphanumeric()));
}
