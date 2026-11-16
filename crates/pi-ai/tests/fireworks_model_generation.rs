//! Port of packages/ai/test/fireworks-model-generation.test.ts
// PORT: packages/ai/scripts/fireworks-models.ts is not ported per PORTING.md §2.6 (`rust: null`);
// asserts the resulting generated Fireworks catalog thinking/adaptive metadata directly against
// `fireworks_models()` and `get_supported_thinking_levels()`.

use indexmap::IndexMap;

use pi_ai::models::get_supported_thinking_levels;
use pi_ai::providers::fireworks_models::fireworks_models;
use pi_ai::types::{ModelCompat, ModelThinkingLevel};

#[test]
fn messages_reasoners_enable_force_adaptive_thinking_and_native_levels() {
    let models = fireworks_models();

    let deepseek = models
        .get("accounts/fireworks/models/deepseek-v4p1-flash")
        .expect("deepseek-v4p1-flash");
    let ds_compat = deepseek
        .compat
        .as_ref()
        .and_then(ModelCompat::as_anthropic_messages)
        .expect("anthropic compat");
    assert_eq!(ds_compat.force_adaptive_thinking, Some(true));
    assert_eq!(
        get_supported_thinking_levels(deepseek),
        vec![
            ModelThinkingLevel::Off,
            ModelThinkingLevel::Low,
            ModelThinkingLevel::High,
            ModelThinkingLevel::Max,
        ]
    );

    let qwen_2p4t = models
        .get("accounts/fireworks/models/qwen3p8-2p4t-a95b")
        .expect("qwen3p8-2p4t-a95b");
    let qwen_compat = qwen_2p4t
        .compat
        .as_ref()
        .and_then(ModelCompat::as_anthropic_messages)
        .expect("anthropic compat");
    assert_eq!(qwen_compat.force_adaptive_thinking, Some(true));
    let mut expected_qwen = IndexMap::new();
    expected_qwen.insert(ModelThinkingLevel::Off, Some("none".to_owned()));
    expected_qwen.insert(ModelThinkingLevel::Minimal, None);
    expected_qwen.insert(ModelThinkingLevel::Low, Some("low".to_owned()));
    expected_qwen.insert(ModelThinkingLevel::Medium, Some("medium".to_owned()));
    expected_qwen.insert(ModelThinkingLevel::High, None);
    expected_qwen.insert(ModelThinkingLevel::XHigh, Some("xhigh".to_owned()));
    expected_qwen.insert(ModelThinkingLevel::Max, None);
    assert_eq!(qwen_2p4t.thinking_level_map.as_ref(), Some(&expected_qwen));
}

#[test]
fn toggle_and_fixed_reasoning_models_omit_force_adaptive_thinking() {
    let models = fireworks_models();
    for id in [
        "accounts/fireworks/models/nemotron-3-ultra-nvfp4",
        "accounts/fireworks/models/nemotron-lightning-3p5-30b-a3b",
    ] {
        let model = models.get(id).expect(id);
        let compat = model
            .compat
            .as_ref()
            .and_then(ModelCompat::as_anthropic_messages)
            .expect("anthropic compat");
        assert_eq!(compat.force_adaptive_thinking, None, "{id}");
        assert_eq!(model.thinking_level_map, None, "{id}");
    }
}

#[test]
fn glm_and_kimi_openai_compat_models_preserve_distinct_effort_maps() {
    let models = fireworks_models();
    for id in [
        "accounts/fireworks/models/glm-5p3",
        "accounts/fireworks/routers/glm-5p3-fast",
        "accounts/fireworks/models/kimi-k3",
        "accounts/fireworks/routers/kimi-k3-fast",
    ] {
        let model = models.get(id).expect(id);
        assert_eq!(
            get_supported_thinking_levels(model),
            vec![
                ModelThinkingLevel::Low,
                ModelThinkingLevel::High,
                ModelThinkingLevel::Max,
            ],
            "{id}"
        );
    }
}
