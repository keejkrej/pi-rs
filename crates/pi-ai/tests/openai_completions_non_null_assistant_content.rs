//! Port of packages/ai/test/openai-completions-non-null-assistant-content.test.ts

use indexmap::IndexMap;
use pi_ai::api::openai_completions::{ResolvedOpenAICompletionsCompat, convert_messages, get_compat};
use pi_ai::compat::{get_model, normalize_context};
use pi_ai::types::{
    AssistantContentBlock, AssistantMessage, CacheControlFormat, Context, MaxTokensField, Message, Model, ModelInput,
    StopReason, TextContent, ThinkingFormat, ToolCall, ToolResultMessage, TranscriptContext, Usage, UserContentBlock,
    UserMessage, UserMessageContent,
};
use pi_js::time;
use serde_json::{Value, json};

fn build_model() -> Model {
    let mut base = get_model("openai", "gpt-4o-mini").unwrap();
    base.compat = None;
    base.api = "openai-completions".to_owned();
    base.input = vec![ModelInput::Text, ModelInput::Image];
    base
}

fn base_compat(model: &Model) -> ResolvedOpenAICompletionsCompat {
    ResolvedOpenAICompletionsCompat {
        supports_store: true,
        supports_developer_role: true,
        supports_reasoning_effort: true,
        supports_usage_in_streaming: true,
        supports_finish_reason: true,
        max_tokens_field: MaxTokensField::MaxCompletionTokens,
        requires_tool_result_name: false,
        requires_assistant_after_tool_result: false,
        requires_non_null_assistant_content: false,
        requires_tools_with_tool_history: false,
        requires_thinking_as_text: false,
        requires_reasoning_content_on_assistant_messages: Some(false),
        thinking_format: ThinkingFormat::OpenAI,
        supports_strict_mode: true,
        supports_open_ai_grammar_tools: false,
        cache_control_format: Some(CacheControlFormat::Anthropic),
        ..get_compat(model)
    }
}

fn build_context(model: &Model) -> TranscriptContext {
    let now = time::now_ms();
    let mut args = IndexMap::new();
    args.insert("command".to_owned(), json!("true"));
    let assistant = AssistantMessage {
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
    };
    normalize_context(&Context {
        system_prompt: None,
        messages: vec![
            Message::User(UserMessage {
                content: UserMessageContent::Text("Run the command".to_owned()),
                timestamp: now - 1,
            }),
            Message::Assistant(assistant),
            Message::ToolResult(ToolResultMessage {
                tool_call_id: "tool-1".to_owned(),
                tool_name: "bash".to_owned(),
                content: vec![UserContentBlock::Text(TextContent::new("ok"))],
                details: None,
                is_error: false,
                nested_calls: None,
                usage: None,
                timestamp: now + 1,
                duration_ms: None,
            }),
            Message::User(UserMessage {
                content: UserMessageContent::Text("Thanks".to_owned()),
                timestamp: now + 2,
            }),
        ],
        tools: None,
    })
}

#[test]
fn handles_requires_non_null_assistant_content_and_assistant_after_tool_result() {
    let model = build_model();
    let ctx = build_context(&model);
    let default_compat = base_compat(&model);

    let default_msgs = convert_messages(&model, &ctx, &default_compat, None);
    let default_assts: Vec<&Value> = default_msgs
        .iter()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
        .collect();
    assert_eq!(default_assts.len(), 1);
    assert_eq!(default_assts[0].get("content"), Some(&Value::Null));

    let non_null_compat = ResolvedOpenAICompletionsCompat {
        requires_non_null_assistant_content: true,
        ..default_compat.clone()
    };
    let non_null_msgs = convert_messages(&model, &ctx, &non_null_compat, None);
    let non_null_roles: Vec<&str> = non_null_msgs
        .iter()
        .map(|m| m.get("role").and_then(Value::as_str).unwrap())
        .collect();
    assert_eq!(non_null_roles, vec!["user", "assistant", "tool", "user"]);
    let non_null_assts: Vec<&Value> = non_null_msgs
        .iter()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
        .collect();
    assert_eq!(non_null_assts.len(), 1);
    assert_eq!(non_null_assts[0].get("content"), Some(&json!("")));

    let after_compat = ResolvedOpenAICompletionsCompat {
        requires_assistant_after_tool_result: true,
        ..default_compat
    };
    let after_msgs = convert_messages(&model, &ctx, &after_compat, None);
    let after_roles: Vec<&str> = after_msgs
        .iter()
        .map(|m| m.get("role").and_then(Value::as_str).unwrap())
        .collect();
    assert_eq!(after_roles, vec!["user", "assistant", "tool", "assistant", "user"]);
    let after_assts: Vec<&Value> = after_msgs
        .iter()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
        .collect();
    assert_eq!(after_assts[0].get("content"), Some(&json!("")));
    assert_eq!(
        after_assts[1].get("content"),
        Some(&json!("I have processed the tool results."))
    );
}
