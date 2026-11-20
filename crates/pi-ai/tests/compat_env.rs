//! Port of packages/ai/test/compat-env.test.ts (unit ai-providers-builtin)

use std::sync::{Arc, Mutex};

use pi_ai::compat::{ApiProvider, complete, register_api_provider, reset_api_providers};
use pi_ai::types::{
    ApiStreamOptions, AssistantContentBlock, AssistantMessage, Context, Message, Model, ModelCost, ModelInput,
    StopReason, StreamOptions, TextContent, Usage, UserMessage, UserMessageContent,
};
use pi_ai::utils::event_stream::AssistantMessageEventStream;
use serial_test::serial;

struct ResetGuard;

impl Drop for ResetGuard {
    fn drop(&mut self) {
        reset_api_providers();
    }
}

fn make_context() -> Context {
    Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hi".to_string()),
            timestamp: 1,
        })],
        tools: None,
    }
}

fn make_model() -> Model {
    Model {
        r#type: None,
        id: "test-model".to_string(),
        name: "Test Model".to_string(),
        api: "openai-responses".to_string(),
        provider: "custom-openai".into(),
        base_url: "https://example.test/v1".to_string(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 128000,
        max_tokens: 4096,
        headers: None,
        sampling_params: None,
        compat: None,
        sampling_params_by_thinking_level: None,
    }
}

fn make_message(model: &Model) -> AssistantMessage {
    AssistantMessage {
        content: vec![AssistantContentBlock::Text(TextContent {
            text: "ok".to_string(),
            text_signature: None,
        })],
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
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
        timestamp: 1,
        duration_ms: None,
    }
}

#[tokio::test]
#[serial]
async fn dispatches_unknown_providers_through_legacy_api_registry() {
    reset_api_providers();
    let _guard = ResetGuard;

    let captured_api_key: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let cap_stream = Arc::clone(&captured_api_key);
    let cap_simple = Arc::clone(&captured_api_key);

    register_api_provider(
        ApiProvider {
            api: "openai-responses".to_string(),
            stream: Arc::new(move |model, _ctx, opts| {
                *cap_stream.lock().unwrap() = opts.as_ref().and_then(|o| o.base.api_key.clone());
                let stream = AssistantMessageEventStream::new();
                let output = make_message(model);
                stream.end(Some(output));
                stream
            }),
            stream_simple: Arc::new(move |model, _ctx, opts| {
                *cap_simple.lock().unwrap() = opts.as_ref().and_then(|o| o.base.api_key.clone());
                let stream = AssistantMessageEventStream::new();
                let output = make_message(model);
                stream.end(Some(output));
                stream
            }),
        },
        None,
    );

    let model = make_model();
    let context = make_context();
    let opts = ApiStreamOptions {
        base: StreamOptions {
            api_key: Some("request-key".to_string()),
            ..Default::default()
        },
        ..Default::default()
    };

    let msg = complete(&model, &context, Some(opts)).await;
    assert_eq!(msg.stop_reason, StopReason::Stop);
    assert_eq!(captured_api_key.lock().unwrap().as_deref(), Some("request-key"));
}
