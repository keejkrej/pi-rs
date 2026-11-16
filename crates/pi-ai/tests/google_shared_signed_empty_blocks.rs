//! Port of packages/ai/test/google-shared-signed-empty-blocks.test.ts

use pi_ai::api::google_shared::convert_messages;
use pi_ai::utils::transcript::normalize_context;
use pi_ai::{
    AssistantContentBlock, AssistantMessage, Context, JsonObject, Message, Model, ModelCost, ModelInput, StopReason,
    TextContent, ThinkingContent, ToolCall, Usage, UserMessage,
};
use pretty_assertions::assert_eq;
use serde_json::json;

const VALID_SIG: &str = "AAAAAAAAAAAAAAAAAAAAAA==";

fn make_model(id: &str) -> Model {
    Model {
        r#type: None,
        id: id.to_owned(),
        name: id.to_owned(),
        api: "google-generative-ai".to_owned(),
        provider: "google".to_owned(),
        base_url: "https://example.com".to_owned(),
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 128_000,
        max_tokens: 8_192,
        headers: None,
        sampling_params: None,
        compat: None,
    }
}

fn bash_ls_tool_call() -> AssistantContentBlock {
    let mut args = JsonObject::new();
    args.insert("command".to_owned(), json!("ls"));
    AssistantContentBlock::ToolCall(ToolCall::new("call_1", "bash", args))
}

fn make_context(model_id: &str, content: Vec<AssistantContentBlock>) -> Context {
    let now = pi_js::time::now_ms();
    Context {
        system_prompt: None,
        messages: vec![
            Message::User(UserMessage {
                content: "Hi".into(),
                timestamp: now,
            }),
            Message::Assistant(AssistantMessage {
                content,
                api: "google-generative-ai".to_owned(),
                provider: "google".to_owned(),
                model: model_id.to_owned(),
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
            }),
        ],
        tools: None,
    }
}

#[test]
fn keeps_signed_empty_thinking_block_so_signature_is_echoed_back() {
    let model = make_model("gemini-3-pro-preview");
    let contents = convert_messages(
        &model,
        &normalize_context(&make_context(
            &model.id,
            vec![
                AssistantContentBlock::Thinking(ThinkingContent {
                    thinking: String::new(),
                    thinking_signature: Some(VALID_SIG.to_owned()),
                    redacted: None,
                }),
                bash_ls_tool_call(),
            ],
        )),
    );

    let model_turn = contents
        .iter()
        .find(|c| c.role == "model")
        .expect("model turn should exist");
    let signed: Vec<_> = model_turn
        .parts
        .iter()
        .filter(|p| p.thought_signature.as_deref() == Some(VALID_SIG))
        .collect();
    assert_eq!(signed.len(), 1);
    assert_eq!(signed[0].thought, Some(true));
}

#[test]
fn keeps_signed_empty_text_block_the_same_way() {
    let model = make_model("gemini-3-pro-preview");
    let contents = convert_messages(
        &model,
        &normalize_context(&make_context(
            &model.id,
            vec![
                AssistantContentBlock::Text(TextContent {
                    text: String::new(),
                    text_signature: Some(VALID_SIG.to_owned()),
                }),
                bash_ls_tool_call(),
            ],
        )),
    );

    let model_turn = contents
        .iter()
        .find(|c| c.role == "model")
        .expect("model turn should exist");
    let signed_count = model_turn
        .parts
        .iter()
        .filter(|p| p.thought_signature.as_deref() == Some(VALID_SIG))
        .count();
    assert_eq!(signed_count, 1);
}

#[test]
fn still_drops_unsigned_empty_blocks() {
    let model = make_model("gemini-3-pro-preview");
    let contents = convert_messages(
        &model,
        &normalize_context(&make_context(
            &model.id,
            vec![
                AssistantContentBlock::Thinking(ThinkingContent::new("")),
                AssistantContentBlock::Text(TextContent::new("   ")),
                bash_ls_tool_call(),
            ],
        )),
    );

    let model_turn = contents
        .iter()
        .find(|c| c.role == "model")
        .expect("model turn should exist");
    assert_eq!(model_turn.parts.len(), 1);
    assert!(model_turn.parts[0].function_call.is_some());
}

#[test]
fn still_drops_signed_empty_blocks_from_a_different_provider_model() {
    let model = make_model("gemini-3-pro-preview");
    let contents = convert_messages(
        &model,
        &normalize_context(&make_context(
            "other-model",
            vec![
                AssistantContentBlock::Thinking(ThinkingContent {
                    thinking: String::new(),
                    thinking_signature: Some(VALID_SIG.to_owned()),
                    redacted: None,
                }),
                AssistantContentBlock::Text(TextContent {
                    text: String::new(),
                    text_signature: Some(VALID_SIG.to_owned()),
                }),
                bash_ls_tool_call(),
            ],
        )),
    );

    let model_turn = contents
        .iter()
        .find(|c| c.role == "model")
        .expect("model turn should exist");
    assert_eq!(model_turn.parts.len(), 1);
    assert!(model_turn.parts[0].function_call.is_some());
    assert!(!pi_js::json::stringify(model_turn).contains(VALID_SIG));
}
