//! Port of packages/ai/test/openrouter-reasoning-options.test.ts (Block 2: OpenRouter mandatory reasoning payloads)

use std::sync::{Arc, Mutex};

use pi_ai::api::openai_completions::stream_simple;
use pi_ai::types::{
    Context, Message, Model, ModelCompat, ModelCost, ModelInput, ModelThinkingLevel, OpenAICompletionsCompat,
    SimpleStreamOptions, StreamOptions, ThinkingFormat, ThinkingLevel, ThinkingLevelMap, UserMessage,
    UserMessageContent,
};
use pi_ai::utils::transcript::normalize_context;
use serde_json::Value;

fn openrouter_model(thinking_level_map: Option<ThinkingLevelMap>) -> Model {
    Model {
        r#type: None,
        id: "stealth/ox-alpha".to_string(),
        name: "Ox Alpha".to_string(),
        api: "openai-completions".to_string(),
        provider: "openrouter".into(),
        base_url: "https://example.invalid/v1".to_string(),
        reasoning: true,
        thinking_level_map,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 128000,
        max_tokens: 4096,
        headers: None,
        sampling_params: None,
        compat: Some(ModelCompat::OpenAICompletions(OpenAICompletionsCompat {
            thinking_format: Some(ThinkingFormat::OpenRouter),
            ..Default::default()
        })),
    }
}

async fn capture_payload(model: &Model, reasoning: Option<ThinkingLevel>) -> Value {
    let context = normalize_context(&Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("Hello".to_string()),
            timestamp: 0,
        })],
        tools: None,
    });
    let slot: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let cap = Arc::clone(&slot);
    stream_simple(
        model,
        &context,
        Some(SimpleStreamOptions {
            base: StreamOptions {
                api_key: Some("test".to_string()),
                on_payload: Some(Arc::new(move |req, _m| {
                    let cap = Arc::clone(&cap);
                    Box::pin(async move {
                        *cap.lock().unwrap() = Some(req);
                        Err(pi_js::Error::msg("payload captured"))
                    })
                })),
                ..Default::default()
            },
            reasoning,
            ..Default::default()
        }),
    )
    .result()
    .await;
    slot.lock().unwrap().clone().expect("payload captured")
}

#[tokio::test]
async fn openrouter_mandatory_and_optional_reasoning_payloads() {
    let mut mandatory_map = ThinkingLevelMap::new();
    mandatory_map.insert(ModelThinkingLevel::Off, None);
    mandatory_map.insert(ModelThinkingLevel::Minimal, None);
    mandatory_map.insert(ModelThinkingLevel::Low, Some("low".to_string()));
    mandatory_map.insert(ModelThinkingLevel::Medium, None);
    mandatory_map.insert(ModelThinkingLevel::High, Some("high".to_string()));
    mandatory_map.insert(ModelThinkingLevel::XHigh, None);
    mandatory_map.insert(ModelThinkingLevel::Max, Some("max".to_string()));

    let omitted = capture_payload(&openrouter_model(Some(mandatory_map.clone())), None).await;
    assert!(omitted.get("reasoning").is_none(), "payload={omitted}");

    let low = capture_payload(&openrouter_model(Some(mandatory_map)), Some(ThinkingLevel::Low)).await;
    assert_eq!(low["reasoning"]["effort"], "low");

    let optional_disabled = capture_payload(&openrouter_model(None), None).await;
    assert_eq!(optional_disabled["reasoning"]["effort"], "none");
}
