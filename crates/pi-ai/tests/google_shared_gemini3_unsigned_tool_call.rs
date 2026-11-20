//! Port of packages/ai/test/google-shared-gemini3-unsigned-tool-call.test.ts

use pi_ai::api::google_shared::{convert_messages, requires_tool_call_id};
use pi_ai::utils::transcript::normalize_context;
use pi_ai::{
    AssistantContentBlock, AssistantMessage, Context, JsonObject, Message, Model, ModelCost, ModelInput, StopReason,
    ToolCall, ToolResultMessage, Usage, UserContentBlock, UserMessage,
};
use pretty_assertions::assert_eq;
use serde_json::json;

fn make_gemini3_model(api: &str, provider: &str, id: &str) -> Model {
    Model {
        r#type: None,
        id: id.to_owned(),
        name: "Gemini 3 Pro Preview".to_owned(),
        api: api.to_owned(),
        provider: provider.to_owned(),
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
        sampling_params_by_thinking_level: None,
    }
}

fn make_args(cmd: &str) -> JsonObject {
    let mut map = JsonObject::new();
    map.insert("command".to_owned(), json!(cmd));
    map
}

fn make_context(api: &str, provider: &str, model_id: &str, thought_signature: Option<&str>) -> Context {
    let now = pi_js::time::now_ms();
    let mut call_1 = ToolCall::new("call_1", "bash", make_args("echo hi"));
    call_1.thought_signature = thought_signature.map(str::to_owned);
    let call_2 = ToolCall::new("call_2", "bash", make_args("ls -la"));

    Context {
        system_prompt: None,
        messages: vec![
            Message::User(UserMessage {
                content: "Hi".into(),
                timestamp: now,
            }),
            Message::Assistant(AssistantMessage {
                content: vec![
                    AssistantContentBlock::ToolCall(call_1),
                    AssistantContentBlock::ToolCall(call_2),
                ],
                api: api.to_owned(),
                provider: provider.to_owned(),
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
                duration_ms: None,
            }),
            Message::ToolResult(ToolResultMessage {
                tool_call_id: "call_1".to_owned(),
                tool_name: "bash".to_owned(),
                content: vec![UserContentBlock::Text(pi_ai::TextContent::new("hi"))],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: false,
                timestamp: now,
                duration_ms: None,
            }),
            Message::ToolResult(ToolResultMessage {
                tool_call_id: "call_2".to_owned(),
                tool_name: "bash".to_owned(),
                content: vec![UserContentBlock::Text(pi_ai::TextContent::new("files"))],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: false,
                timestamp: now,
                duration_ms: None,
            }),
        ],
        tools: None,
    }
}

#[test]
fn preserves_tool_call_ids_for_gemini_3_history() {
    let models = [
        make_gemini3_model("google-generative-ai", "google", "gemini-3-pro-preview"),
        make_gemini3_model("google-generative-ai", "google", "gemini-3.6-flash"),
        make_gemini3_model("google-vertex", "google-vertex", "gemini-3-pro-preview"),
    ];

    for model in models {
        let context = make_context(&model.api, &model.provider, &model.id, None);
        let contents = convert_messages(&model, &normalize_context(&context));

        let function_call_ids: Vec<String> = contents
            .iter()
            .flat_map(|c| &c.parts)
            .filter_map(|p| p.function_call.as_ref().and_then(|fc| fc.id.clone()))
            .collect();
        let function_response_ids: Vec<String> = contents
            .iter()
            .flat_map(|c| &c.parts)
            .filter_map(|p| p.function_response.as_ref().and_then(|fr| fr.id.clone()))
            .collect();

        assert_eq!(function_call_ids, vec!["call_1", "call_2"]);
        assert_eq!(function_response_ids, vec!["call_1", "call_2"]);
    }
}

#[test]
fn does_not_add_skip_thought_signature_validator_for_unsigned_google_ai_tool_calls() {
    let model = make_gemini3_model("google-generative-ai", "google", "gemini-3-pro-preview");
    let contents = convert_messages(
        &model,
        &normalize_context(&make_context(&model.api, &model.provider, "other-model", None)),
    );

    let model_turn = contents
        .iter()
        .find(|c| c.role == "model")
        .expect("model turn should exist");
    let function_call_parts: Vec<_> = model_turn.parts.iter().filter(|p| p.function_call.is_some()).collect();
    assert_eq!(function_call_parts.len(), 2);
    assert_eq!(function_call_parts[0].thought_signature, None);
    assert_eq!(function_call_parts[1].thought_signature, None);
    assert!(!pi_js::json::stringify(model_turn).contains("skip_thought_signature_validator"));

    let historical_text_count = model_turn
        .parts
        .iter()
        .filter_map(|p| p.text.as_deref())
        .filter(|t| t.contains("Historical context"))
        .count();
    assert_eq!(historical_text_count, 0);
}

#[test]
fn does_not_add_skip_thought_signature_validator_for_unsigned_vertex_tool_calls() {
    let model = make_gemini3_model("google-vertex", "google-vertex", "gemini-3-pro-preview");
    let contents = convert_messages(
        &model,
        &normalize_context(&make_context(&model.api, &model.provider, &model.id, None)),
    );
    let model_turn = contents
        .iter()
        .find(|c| c.role == "model")
        .expect("model turn should exist");
    let function_call_parts: Vec<_> = model_turn.parts.iter().filter(|p| p.function_call.is_some()).collect();

    assert_eq!(function_call_parts.len(), 2);
    assert_eq!(function_call_parts[0].thought_signature, None);
    assert_eq!(function_call_parts[1].thought_signature, None);
    assert!(!pi_js::json::stringify(model_turn).contains("skip_thought_signature_validator"));
}

#[test]
fn preserves_valid_thought_signature_when_present_for_same_provider_and_model() {
    let model = make_gemini3_model("google-generative-ai", "google", "gemini-3-pro-preview");
    let valid_sig = "AAAAAAAAAAAAAAAAAAAAAA==";
    let contents = convert_messages(
        &model,
        &normalize_context(&make_context(&model.api, &model.provider, &model.id, Some(valid_sig))),
    );
    let model_turn = contents
        .iter()
        .find(|c| c.role == "model")
        .expect("model turn should exist");
    let function_call_parts: Vec<_> = model_turn.parts.iter().filter(|p| p.function_call.is_some()).collect();

    assert_eq!(function_call_parts.len(), 2);
    assert_eq!(function_call_parts[0].thought_signature.as_deref(), Some(valid_sig));
    assert_eq!(function_call_parts[1].thought_signature, None);
}

#[test]
fn does_not_add_thought_signature_or_id_for_non_gemini3_models() {
    let model = make_gemini3_model("google-generative-ai", "google", "gemini-2.5-flash");
    let contents = convert_messages(
        &model,
        &normalize_context(&make_context(&model.api, &model.provider, "other-model", None)),
    );
    let model_turn = contents
        .iter()
        .find(|c| c.role == "model")
        .expect("model turn should exist");
    let function_call_parts: Vec<_> = model_turn.parts.iter().filter(|p| p.function_call.is_some()).collect();
    let function_response_parts: Vec<_> = contents
        .iter()
        .flat_map(|c| &c.parts)
        .filter(|p| p.function_response.is_some())
        .collect();

    assert_eq!(function_call_parts.len(), 2);
    assert!(
        function_call_parts
            .iter()
            .all(|p| { p.function_call.as_ref().is_some_and(|fc| fc.id.is_none()) })
    );
    assert!(function_call_parts.iter().all(|p| p.thought_signature.is_none()));
    assert_eq!(function_response_parts.len(), 2);
    assert!(
        function_response_parts
            .iter()
            .all(|p| { p.function_response.as_ref().is_some_and(|fr| fr.id.is_none()) })
    );
}

#[test]
fn requires_tool_call_id_cases() {
    let cases = [
        (false, "gemini-2.5-flash"),
        (true, "gemini-3.6-flash"),
        (true, "claude-sonnet-4-5"),
        (true, "gpt-oss-120b"),
    ];
    for (expected, model_id) in cases {
        assert_eq!(requires_tool_call_id(model_id), expected, "model_id={model_id}");
    }
}
