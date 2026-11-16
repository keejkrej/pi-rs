//! Port of packages/ai/test/max-thinking.test.ts (unit ai-providers-builtin)

use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use pi_ai::api::openai_codex_responses::stream_simple as stream_simple_codex;
use pi_ai::compat::{clamp_thinking_level, get_model, get_supported_thinking_levels, normalize_context};
use pi_ai::types::{
    Context, Message, Model, ModelCost, ModelInput, ModelThinkingLevel, SimpleStreamOptions, StreamOptions,
    ThinkingLevel, ThinkingLevelMap, UserMessage, UserMessageContent,
};
use serde_json::Value;

fn mock_token() -> String {
    let payload = STANDARD.encode(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acc_test"}}"#);
    format!("aaa.{payload}.bbb")
}

#[test]
fn max_thinking_level_is_opt_in_and_exposed_for_codex_models() {
    let model = Model {
        r#type: None,
        id: "ordinary-reasoning".to_string(),
        name: "Ordinary Reasoning".to_string(),
        api: "openai-completions".to_string(),
        provider: "test".into(),
        base_url: "https://example.com/v1".to_string(),
        reasoning: true,
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
    };

    assert_eq!(
        get_supported_thinking_levels(&model),
        vec![
            ModelThinkingLevel::Off,
            ModelThinkingLevel::Minimal,
            ModelThinkingLevel::Low,
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High,
        ]
    );
    assert_eq!(
        clamp_thinking_level(&model, ModelThinkingLevel::Max),
        ModelThinkingLevel::High
    );

    for model_id in [
        "gpt-5.6-luna",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-6-luna",
        "gpt-6-sol",
    ] {
        let m = get_model("openai-codex", model_id).unwrap();
        let tmap = m.thinking_level_map.as_ref().unwrap();
        assert_eq!(
            tmap.get(&ModelThinkingLevel::XHigh).and_then(|v| v.as_deref()),
            Some("xhigh")
        );
        assert_eq!(
            tmap.get(&ModelThinkingLevel::Max).and_then(|v| v.as_deref()),
            Some("max")
        );
        assert_eq!(
            get_supported_thinking_levels(&m),
            vec![
                ModelThinkingLevel::Off,
                ModelThinkingLevel::Minimal,
                ModelThinkingLevel::Low,
                ModelThinkingLevel::Medium,
                ModelThinkingLevel::High,
                ModelThinkingLevel::XHigh,
                ModelThinkingLevel::Max,
            ]
        );
    }

    let mut hole_map = ThinkingLevelMap::new();
    hole_map.insert(ModelThinkingLevel::XHigh, None);
    hole_map.insert(ModelThinkingLevel::Max, Some("max".to_string()));
    let hole_model = Model {
        id: "high-and-max".to_string(),
        name: "High and Max".to_string(),
        thinking_level_map: Some(hole_map),
        ..model
    };
    assert_eq!(
        get_supported_thinking_levels(&hole_model),
        vec![
            ModelThinkingLevel::Off,
            ModelThinkingLevel::Minimal,
            ModelThinkingLevel::Low,
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High,
            ModelThinkingLevel::Max,
        ]
    );
    assert_eq!(
        clamp_thinking_level(&hole_model, ModelThinkingLevel::XHigh),
        ModelThinkingLevel::Max
    );
}

#[tokio::test]
async fn sends_max_to_codex_responses_api() {
    for model_id in ["gpt-5.6-sol", "gpt-6-astra", "gpt-6-sol", "gpt-6-luna", "gpt-6.1-sol"] {
        let model = get_model("openai-codex", model_id).unwrap();
        let context = normalize_context(&Context {
            system_prompt: Some("You are a helpful assistant.".to_string()),
            messages: vec![Message::User(UserMessage {
                content: UserMessageContent::Text("Hello".to_string()),
                timestamp: 1,
            })],
            tools: None,
        });
        let captured: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
        let cap = Arc::clone(&captured);

        stream_simple_codex(
            &model,
            &context,
            Some(SimpleStreamOptions {
                base: StreamOptions {
                    api_key: Some(mock_token()),
                    on_payload: Some(Arc::new(move |req, _m| {
                        let cap = Arc::clone(&cap);
                        Box::pin(async move {
                            *cap.lock().unwrap() = Some(req);
                            Err(pi_js::Error::msg("payload captured"))
                        })
                    })),
                    ..Default::default()
                },
                reasoning: Some(ThinkingLevel::Max),
                ..Default::default()
            }),
        )
        .result()
        .await;

        let payload = captured.lock().unwrap().clone().expect("payload");
        assert_eq!(payload["reasoning"]["effort"], "max", "{model_id}");
        assert_eq!(payload["reasoning"]["summary"], "auto", "{model_id}");
    }
}
