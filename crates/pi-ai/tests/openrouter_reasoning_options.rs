//! Port of packages/ai/test/openrouter-reasoning-options.test.ts (unit ai-scripts-generate-models-fetch)

// @generated-mods begin (scaffold-owned, do not edit)
#[path = "openrouter_reasoning_options/part_2.rs"]
mod part_2;
// @generated-mods end

// PORT: packages/ai/scripts/openrouter-reasoning-options.ts is not ported per PORTING.md §2.6 (`rust: null`);
// asserts the resulting mandatory/optional/absent `thinkingLevelMap` invariants directly against
// loaded `openrouter_models()` and `get_supported_thinking_levels()`.

use pi_ai::models::get_supported_thinking_levels;
use pi_ai::providers::openrouter_models::openrouter_models;
use pi_ai::types::ModelThinkingLevel;

#[test]
fn openrouter_mandatory_reasoning_models_mark_off_unavailable() {
    let models = openrouter_models();
    let mandatory = models
        .get("~anthropic/claude-opus-latest")
        .expect("~anthropic/claude-opus-latest");
    let map = mandatory
        .thinking_level_map
        .as_ref()
        .expect("thinking_level_map for mandatory reasoning model");
    assert_eq!(map.get(&ModelThinkingLevel::Off), Some(&None));
    assert!(!get_supported_thinking_levels(mandatory).contains(&ModelThinkingLevel::Off));
}

#[test]
fn openrouter_optional_effort_models_map_off_to_none_and_restrict_efforts() {
    let models = openrouter_models();
    let optional = models
        .get("~deepseek/deepseek-flash-latest")
        .expect("~deepseek/deepseek-flash-latest");
    let map = optional
        .thinking_level_map
        .as_ref()
        .expect("thinking_level_map for optional effort model");
    assert_eq!(
        map.get(&ModelThinkingLevel::Off).and_then(|v| v.as_deref()),
        Some("none")
    );
    assert_eq!(map.get(&ModelThinkingLevel::Minimal), Some(&None));
    assert_eq!(
        map.get(&ModelThinkingLevel::High).and_then(|v| v.as_deref()),
        Some("high")
    );
}

#[test]
fn openrouter_models_without_effort_controls_omit_thinking_level_map() {
    let models = openrouter_models();
    let haiku_batch = models
        .get("anthropic/claude-haiku-4.5:batch")
        .expect("anthropic/claude-haiku-4.5:batch");
    assert!(haiku_batch.reasoning);
    assert_eq!(haiku_batch.thinking_level_map, None);
}
