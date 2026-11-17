//! Port of packages/ai/test/xai-responses.test.ts (unit ai-providers-builtin)

// @generated-mods begin (scaffold-owned, do not edit)
#[path = "xai_responses/part_2.rs"]
mod part_2;
// @generated-mods end

use pi_ai::models::get_supported_thinking_levels;
use pi_ai::providers::xai::xai_provider;
use pi_ai::providers::xai_models::xai_models;
use pi_ai::types::{ModelInput, ModelThinkingLevel};

#[test]
fn excludes_retired_and_redundant_models_from_built_in_catalog() {
    let models = xai_models();
    for excluded in [
        "grok-3",
        "grok-3-fast",
        "grok-4.20-0309-non-reasoning",
        "grok-4.20-0309-reasoning",
        "grok-build-0.1",
        "grok-code-fast-1",
    ] {
        assert!(!models.contains_key(excluded), "unexpected model {excluded}");
    }
}

#[test]
fn routes_every_built_in_xai_model_through_responses() {
    let models = xai_models();
    for model in models.values() {
        assert_eq!(model.api, "openai-responses", "model {}", model.id);
    }

    let provider = xai_provider();
    assert_eq!(provider.get_models().len(), models.len());

    assert_eq!(
        get_supported_thinking_levels(models.get("grok-4.5").expect("grok-4.5")),
        vec![
            ModelThinkingLevel::Low,
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High,
        ]
    );
    assert_eq!(
        get_supported_thinking_levels(models.get("grok-4.6").expect("grok-4.6")),
        vec![
            ModelThinkingLevel::Low,
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High,
            ModelThinkingLevel::XHigh,
        ]
    );
    assert_eq!(
        get_supported_thinking_levels(models.get("grok-4.7").expect("grok-4.7")),
        vec![
            ModelThinkingLevel::Low,
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High,
            ModelThinkingLevel::XHigh,
        ]
    );
    assert_eq!(
        get_supported_thinking_levels(models.get("grok-4.3").expect("grok-4.3")),
        vec![
            ModelThinkingLevel::Off,
            ModelThinkingLevel::Low,
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High,
        ]
    );
}

#[test]
fn includes_grok_4_7_capabilities_and_long_context_pricing() {
    let grok = xai_models().get("grok-4.7").expect("grok-4.7");
    assert_eq!(grok.api, "openai-responses");
    assert!(grok.reasoning);
    assert_eq!(grok.input, vec![ModelInput::Text, ModelInput::Image]);
    assert_eq!(grok.context_window, 500_000);
    assert_eq!(grok.max_tokens, 500_000);
    assert_eq!(grok.cost.input, 2.0);
    assert_eq!(grok.cost.output, 6.0);
    assert_eq!(grok.cost.cache_read, 0.5);
    assert_eq!(grok.cost.cache_write, 0.0);

    let tiers = grok.cost.tiers.as_ref().expect("cost tiers");
    assert_eq!(tiers.len(), 1);
    assert_eq!(tiers[0].input_tokens_above, 200_000);
    assert_eq!(tiers[0].input, 4.0);
    assert_eq!(tiers[0].output, 12.0);
    assert_eq!(tiers[0].cache_read, 1.0);
    assert_eq!(tiers[0].cache_write, 0.0);
}
