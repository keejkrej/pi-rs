//! Port of packages/ai/test/zai-coding-plan-models.test.ts

use pi_ai::providers::all::get_builtin_model;
use pi_ai::types::{MaxTokensField, ModelCompat, ModelInput, ThinkingFormat};

#[test]
fn exposes_glm_4_6v_on_china_coding_plan_catalog() {
    let model = get_builtin_model("zai-coding-cn", "glm-4.6v").expect("glm-4.6v");
    assert_eq!(model.id, "glm-4.6v");
    assert_eq!(model.provider, "zai-coding-cn");
    assert_eq!(model.api, "openai-completions");
    assert_eq!(model.base_url, "https://open.bigmodel.cn/api/coding/paas/v4");
    assert!(model.reasoning);
    assert_eq!(model.input, vec![ModelInput::Text, ModelInput::Image]);
    assert_eq!(model.cost.input, 0.3);
    assert_eq!(model.cost.output, 0.9);
    assert_eq!(model.cost.cache_read, 0.05);
    assert_eq!(model.cost.cache_write, 0.0);
    assert_eq!(model.context_window, 128_000);
    assert_eq!(model.max_tokens, 32_768);

    let compat = model
        .compat
        .as_ref()
        .and_then(ModelCompat::as_openai_completions)
        .expect("openai-completions compat");
    assert_eq!(compat.max_tokens_field, Some(MaxTokensField::MaxTokens));
    assert_eq!(compat.thinking_format, Some(ThinkingFormat::Zai));
    assert_eq!(compat.zai_tool_stream, Some(true));
}

#[test]
fn uses_api_equivalent_reference_costs_for_coding_plan_models() {
    let glm_5_2 = get_builtin_model("zai", "glm-5.2").expect("glm-5.2");
    assert_eq!(glm_5_2.cost.input, 1.4);
    assert_eq!(glm_5_2.cost.output, 4.4);
    assert_eq!(glm_5_2.cost.cache_read, 0.26);
    assert_eq!(glm_5_2.cost.cache_write, 0.0);

    for provider in ["zai", "zai-coding-cn"] {
        let model = get_builtin_model(provider, "glm-5.3").expect("glm-5.3");
        assert_eq!(model.cost.input, 1.4);
        assert_eq!(model.cost.output, 4.4);
        assert_eq!(model.cost.cache_read, 0.26);
        assert_eq!(model.cost.cache_write, 0.0);
    }
}

#[test]
fn keeps_zero_costs_for_coding_plan_models_without_matching_api_price() {
    for (provider, model_id) in [
        ("zai", "glm-5.2-highspeed"),
        ("zai", "glm-5.3-highspeed"),
        ("zai-coding-cn", "glm-5.3-highspeed"),
    ] {
        let model = get_builtin_model(provider, model_id).expect(model_id);
        assert_eq!(model.cost.input, 0.0);
        assert_eq!(model.cost.output, 0.0);
        assert_eq!(model.cost.cache_read, 0.0);
        assert_eq!(model.cost.cache_write, 0.0);
    }
}
