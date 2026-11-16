//! Port of packages/ai/test/google-shared-image-tool-result-routing.test.ts

use pi_ai::api::google_shared::convert_messages;
use pi_ai::utils::transcript::normalize_context;
use pi_ai::{
    AssistantContentBlock, AssistantMessage, Context, ImageContent, JsonObject, Message, Model, ModelCost, ModelInput,
    StopReason, TextContent, ToolCall, ToolResultMessage, Usage, UserContentBlock, UserMessage,
};
use pretty_assertions::assert_eq;
use serde_json::json;

fn make_model(api: &str, provider: &str, id: &str) -> Model {
    Model {
        r#type: None,
        id: id.to_owned(),
        name: id.to_owned(),
        api: api.to_owned(),
        provider: provider.to_owned(),
        base_url: "https://example.com".to_owned(),
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text, ModelInput::Image],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 128_000,
        max_tokens: 8_192,
        headers: None,
        sampling_params: None,
        compat: None,
    }
}

fn path_args(path: &str) -> JsonObject {
    let mut map = JsonObject::new();
    map.insert("path".to_owned(), json!(path));
    map
}

fn make_context(model: &Model) -> Context {
    let now = pi_js::time::now_ms();
    Context {
        system_prompt: None,
        messages: vec![
            Message::User(UserMessage {
                content: "read the files".into(),
                timestamp: now,
            }),
            Message::Assistant(AssistantMessage {
                content: vec![
                    AssistantContentBlock::ToolCall(ToolCall::new("call_a", "read", path_args("a.txt"))),
                    AssistantContentBlock::ToolCall(ToolCall::new("call_img", "read", path_args("image.png"))),
                    AssistantContentBlock::ToolCall(ToolCall::new("call_b", "read", path_args("b.txt"))),
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
            }),
            Message::ToolResult(ToolResultMessage {
                tool_call_id: "call_a".to_owned(),
                tool_name: "read".to_owned(),
                content: vec![UserContentBlock::Text(TextContent::new("alpha text"))],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: false,
                timestamp: now,
            }),
            Message::ToolResult(ToolResultMessage {
                tool_call_id: "call_img".to_owned(),
                tool_name: "read".to_owned(),
                content: vec![UserContentBlock::Image(ImageContent::new("abc", "image/png"))],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: false,
                timestamp: now,
            }),
            Message::ToolResult(ToolResultMessage {
                tool_call_id: "call_b".to_owned(),
                tool_name: "read".to_owned(),
                content: vec![UserContentBlock::Text(TextContent::new("beta text"))],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: false,
                timestamp: now,
            }),
        ],
        tools: None,
    }
}

#[test]
fn keeps_separate_synthetic_image_turn_for_gemini_2x_google_api_models() {
    let model = make_model("google-generative-ai", "google", "gemini-2.5-flash");
    let contents = convert_messages(&model, &normalize_context(&make_context(&model)));

    assert_eq!(contents.len(), 5);
    assert!(contents[2].parts.iter().all(|part| part.function_response.is_some()));
    assert_eq!(contents[3].parts[0].text.as_deref(), Some("Tool result image:"));
    assert!(contents[3].parts[1].inline_data.is_some());
    assert!(contents[4].parts[0].function_response.is_some());
}

#[test]
fn nests_image_tool_results_for_gemini_3_google_api_models() {
    let model = make_model("google-generative-ai", "google", "gemini-3-pro-preview");
    let contents = convert_messages(&model, &normalize_context(&make_context(&model)));

    assert_eq!(contents.len(), 3);
    let tool_result_turn = &contents[2];
    assert_eq!(tool_result_turn.parts.len(), 3);
    let image_response = tool_result_turn.parts[1]
        .function_response
        .as_ref()
        .expect("image tool result should produce functionResponse");
    let nested = image_response
        .parts
        .as_ref()
        .expect("Gemini 3 should nest image parts in functionResponse");
    assert_eq!(nested.len(), 1);
    assert!(nested[0].inline_data.is_some());
}
