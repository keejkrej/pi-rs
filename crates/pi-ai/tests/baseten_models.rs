//! Port of packages/ai/test/baseten-models.test.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use serde_json::json;

use pi_ai::compat::{get_model, stream_simple};
use pi_ai::env_api_keys::{find_env_keys, get_env_api_key};
use pi_ai::models::get_supported_thinking_levels;
use pi_ai::types::{
    Context, Message, ModelCompat, ModelInput, ModelThinkingLevel, SimpleStreamOptions, StreamOptions, ThinkingFormat,
    ThinkingLevel, UserMessage, UserMessageContent,
};
use pi_js::Error;
use pi_js::env::testing::EnvGuard;

#[test]
fn keeps_both_glm_5_2_endpoints_text_only() {
    let base = get_model("baseten", "zai-org/GLM-5.2").expect("GLM-5.2 model");
    let fast = get_model("baseten", "zai-org/GLM-5.2-Fast").expect("GLM-5.2-Fast model");
    assert_eq!(base.input, vec![ModelInput::Text]);
    assert_eq!(fast.input, vec![ModelInput::Text]);
}

#[tokio::test]
async fn models_kimi_k2_6_reasoning_as_explicit_off_on_toggle() {
    let model = get_model("baseten", "moonshotai/Kimi-K2.6").expect("Kimi-K2.6 model");

    let mut expected_map = IndexMap::new();
    expected_map.insert(ModelThinkingLevel::Off, Some("off".to_owned()));
    expected_map.insert(ModelThinkingLevel::Minimal, None);
    expected_map.insert(ModelThinkingLevel::Low, None);
    expected_map.insert(ModelThinkingLevel::Medium, None);
    expected_map.insert(ModelThinkingLevel::High, Some("high".to_owned()));
    expected_map.insert(ModelThinkingLevel::XHigh, None);
    expected_map.insert(ModelThinkingLevel::Max, None);
    assert_eq!(model.thinking_level_map.as_ref(), Some(&expected_map));

    let compat = model
        .compat
        .as_ref()
        .and_then(ModelCompat::as_openai_completions)
        .expect("openai-completions compat");
    assert_eq!(compat.supports_reasoning_effort, Some(false));
    assert_eq!(compat.thinking_format, Some(ThinkingFormat::Baseten));
    assert!(
        compat
            .chat_template_args
            .as_ref()
            .and_then(|args| args.get("enable_thinking"))
            .is_some()
    );
    assert_eq!(
        get_supported_thinking_levels(&model),
        vec![ModelThinkingLevel::Off, ModelThinkingLevel::High]
    );

    let captured = Arc::new(Mutex::new(None));
    let captured_ref = Arc::clone(&captured);
    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("test".to_owned()),
            timestamp: 0,
        })],
        tools: None,
    };

    let mut base = StreamOptions::default();
    base.api_key = Some("test-baseten-key".to_owned());
    base.on_payload = Some(Arc::new(move |value, _| {
        let mut guard = captured_ref.lock().unwrap();
        *guard = Some(value);
        Box::pin(async { Err(Error::msg("payload captured")) })
    }));
    let opts = SimpleStreamOptions {
        base,
        reasoning: Some(ThinkingLevel::High),
        ..Default::default()
    };

    let _ = stream_simple(&model, &context, Some(opts)).result().await;
    let payload = captured.lock().unwrap().clone().expect("captured payload");
    assert_eq!(
        payload.get("chat_template_args"),
        Some(&json!({ "enable_thinking": true }))
    );
    assert!(payload.get("reasoning_effort").is_none());
}

#[tokio::test]
async fn sends_baseten_chat_template_args_with_reasoning_effort() {
    let model = get_model("baseten", "zai-org/GLM-5.2").expect("GLM-5.2 model");
    let captured = Arc::new(Mutex::new(None));
    let captured_ref = Arc::clone(&captured);
    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("test".to_owned()),
            timestamp: 0,
        })],
        tools: None,
    };

    let mut base = StreamOptions::default();
    base.api_key = Some("test-baseten-key".to_owned());
    base.on_payload = Some(Arc::new(move |value, _| {
        let mut guard = captured_ref.lock().unwrap();
        *guard = Some(value);
        Box::pin(async { Err(Error::msg("payload captured")) })
    }));
    let opts = SimpleStreamOptions {
        base,
        reasoning: Some(ThinkingLevel::High),
        ..Default::default()
    };

    let _ = stream_simple(&model, &context, Some(opts)).result().await;
    let payload = captured.lock().unwrap().clone().expect("captured payload");
    assert_eq!(
        payload.get("chat_template_args"),
        Some(&json!({ "enable_thinking": true }))
    );
    assert_eq!(payload.get("reasoning_effort"), Some(&json!("high")));
}

#[tokio::test]
async fn disables_baseten_opt_in_reasoning_when_thinking_is_off() {
    let model = get_model("baseten", "zai-org/GLM-5.2").expect("GLM-5.2 model");
    let captured = Arc::new(Mutex::new(None));
    let captured_ref = Arc::clone(&captured);
    let context = Context {
        system_prompt: None,
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("test".to_owned()),
            timestamp: 0,
        })],
        tools: None,
    };

    let mut base = StreamOptions::default();
    base.api_key = Some("test-baseten-key".to_owned());
    base.on_payload = Some(Arc::new(move |value, _| {
        let mut guard = captured_ref.lock().unwrap();
        *guard = Some(value);
        Box::pin(async { Err(Error::msg("payload captured")) })
    }));
    let opts = SimpleStreamOptions {
        base,
        ..Default::default()
    };

    let _ = stream_simple(&model, &context, Some(opts)).result().await;
    let payload = captured.lock().unwrap().clone().expect("captured payload");
    assert_eq!(
        payload.get("chat_template_args"),
        Some(&json!({ "enable_thinking": false }))
    );
    assert_eq!(payload.get("reasoning_effort"), Some(&json!("none")));
}

#[test]
fn resolves_baseten_api_key_from_environment() {
    let _guard = EnvGuard::set("BASETEN_API_KEY", Some("test-baseten-key"));
    assert_eq!(find_env_keys("baseten", None), Some(vec!["BASETEN_API_KEY"]));
    assert_eq!(get_env_api_key("baseten", None), Some("test-baseten-key".to_owned()));
}
