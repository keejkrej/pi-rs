//! Port of packages/ai/test/model-catalog-types.test.ts (unit ai-providers-builtin)

use pi_ai::models_generated::provider_chat_models;
use pi_ai::types::ModelThinkingLevel;

#[test]
fn derives_model_api_id_and_provider_from_grouped_model_data() {
    let xai = provider_chat_models("xai").expect("xai catalog present");
    for id in ["grok-4.5", "grok-4.6", "grok-4.7", "grok-4.3"] {
        let model = &xai[id];
        assert_eq!(model.api, "openai-responses");
        assert_eq!(model.id, id);
        assert_eq!(model.provider, "xai");
    }
}

#[test]
fn routes_github_copilot_grok_and_gpt_models_through_responses_api() {
    let copilot = provider_chat_models("github-copilot").expect("github-copilot catalog present");
    assert_eq!(copilot["grok-4.5"].api, "openai-responses");

    let gpt_models: Vec<_> = copilot.values().filter(|m| m.id.starts_with("gpt-")).collect();
    assert!(!gpt_models.is_empty());
    assert!(gpt_models.iter().all(|m| m.api == "openai-responses"));
    assert_eq!(copilot["gpt-6-astra"].api, "openai-responses");

    for model_id in ["gpt-6-sol", "gpt-6-luna"] {
        let model = &copilot[model_id];
        assert_eq!(model.api, "openai-responses");
        assert_eq!(model.context_window, 1_000_000);
        assert_eq!(model.max_tokens, 128_000);
        let tmap = model.thinking_level_map.as_ref().expect("thinking_level_map");
        assert_eq!(
            tmap.get(&ModelThinkingLevel::Off).and_then(|v| v.as_deref()),
            Some("none")
        );
        assert_eq!(
            tmap.get(&ModelThinkingLevel::Max).and_then(|v| v.as_deref()),
            Some("max")
        );
    }
}
