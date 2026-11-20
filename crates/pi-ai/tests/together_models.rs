//! Port of packages/ai/test/together-models.test.ts

use indexmap::IndexMap;

use pi_ai::compat::get_model;
use pi_ai::env_api_keys::{find_env_keys, get_env_api_key};
use pi_ai::types::{MaxTokensField, ModelCompat, ModelInput, ModelThinkingLevel, ThinkingFormat};
use pi_js::env::testing::EnvGuard;

#[test]
fn registers_default_kimi_k3_model_via_openai_completions() {
    let model = get_model("together", "moonshotai/Kimi-K3").expect("Kimi-K3 model");
    assert_eq!(model.api, "openai-completions");
    assert_eq!(model.provider, "together");
    assert_eq!(model.base_url, "https://api.together.ai/v1");
    assert!(model.reasoning);

    let mut expected_map = IndexMap::new();
    expected_map.insert(ModelThinkingLevel::Minimal, None);
    expected_map.insert(ModelThinkingLevel::Low, None);
    expected_map.insert(ModelThinkingLevel::Medium, None);
    assert_eq!(model.thinking_level_map.as_ref(), Some(&expected_map));

    assert_eq!(model.input, vec![ModelInput::Text, ModelInput::Image]);
    assert_eq!(model.context_window, 1_048_576);
    assert_eq!(model.max_tokens, 1_048_576);
    assert_eq!(model.cost.input, 3.0);
    assert_eq!(model.cost.output, 15.0);
    assert_eq!(model.cost.cache_read, 0.3);
    assert_eq!(model.cost.cache_write, 0.0);

    let compat = model
        .compat
        .as_ref()
        .and_then(ModelCompat::as_openai_completions)
        .expect("openai-completions compat");
    assert_eq!(compat.supports_store, Some(false));
    assert_eq!(compat.supports_developer_role, Some(false));
    assert_eq!(compat.supports_reasoning_effort, Some(false));
    assert_eq!(compat.max_tokens_field, Some(MaxTokensField::MaxTokens));
    assert_eq!(compat.thinking_format, Some(ThinkingFormat::Together));
    assert_eq!(compat.supports_strict_mode, Some(false));
    assert_eq!(compat.supports_long_cache_retention, Some(false));
}

#[test]
fn models_together_reasoning_controls_from_api_surface() {
    let gpt_oss = get_model("together", "openai/gpt-oss-120b").expect("gpt-oss-120b");
    let mut expected_gpt = IndexMap::new();
    expected_gpt.insert(ModelThinkingLevel::Off, None);
    expected_gpt.insert(ModelThinkingLevel::Minimal, None);
    expected_gpt.insert(ModelThinkingLevel::Low, Some("low".to_owned()));
    expected_gpt.insert(ModelThinkingLevel::Medium, Some("medium".to_owned()));
    expected_gpt.insert(ModelThinkingLevel::High, Some("high".to_owned()));
    expected_gpt.insert(ModelThinkingLevel::Max, None);
    expected_gpt.insert(ModelThinkingLevel::XHigh, None);
    assert_eq!(gpt_oss.thinking_level_map.as_ref(), Some(&expected_gpt));
    let gpt_compat = gpt_oss
        .compat
        .as_ref()
        .and_then(ModelCompat::as_openai_completions)
        .expect("gpt compat");
    assert_eq!(gpt_compat.supports_reasoning_effort, Some(true));
    assert_eq!(gpt_compat.thinking_format, Some(ThinkingFormat::OpenAI));

    let deepseek = get_model("together", "deepseek-ai/DeepSeek-V4-Pro-0813").expect("DeepSeek-V4-Pro-0813");
    let mut expected_ds = IndexMap::new();
    expected_ds.insert(ModelThinkingLevel::Minimal, None);
    expected_ds.insert(ModelThinkingLevel::Low, None);
    expected_ds.insert(ModelThinkingLevel::Medium, None);
    expected_ds.insert(ModelThinkingLevel::High, Some("high".to_owned()));
    expected_ds.insert(ModelThinkingLevel::XHigh, None);
    assert_eq!(deepseek.thinking_level_map.as_ref(), Some(&expected_ds));
    let ds_compat = deepseek
        .compat
        .as_ref()
        .and_then(ModelCompat::as_openai_completions)
        .expect("deepseek compat");
    assert_eq!(ds_compat.supports_reasoning_effort, Some(true));
    assert_eq!(ds_compat.thinking_format, Some(ThinkingFormat::Together));
}

#[test]
fn resolves_together_api_key_from_environment() {
    let _guard = EnvGuard::set("TOGETHER_API_KEY", Some("test-together-key"));
    assert_eq!(find_env_keys("together", None), Some(vec!["TOGETHER_API_KEY"]));
    assert_eq!(get_env_api_key("together", None), Some("test-together-key".to_owned()));
}
