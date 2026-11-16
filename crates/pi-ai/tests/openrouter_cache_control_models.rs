//! Port of packages/ai/test/openrouter-cache-control-models.test.ts

use pi_ai::providers::all::get_builtin_model;
use pi_ai::types::CacheControlFormat;

const OPENROUTER_ANTHROPIC_LATEST_MODEL_IDS: [&str; 4] = [
    "~anthropic/claude-fable-latest",
    "~anthropic/claude-haiku-latest",
    "~anthropic/claude-opus-latest",
    "~anthropic/claude-sonnet-latest",
];

#[test]
fn keeps_completions_cache_control_for_openrouter_anthropic_latest_aliases() {
    for model_id in OPENROUTER_ANTHROPIC_LATEST_MODEL_IDS {
        let model = get_builtin_model("openrouter", model_id)
            .unwrap_or_else(|| panic!("missing builtin openrouter model {model_id}"));
        assert_eq!(model.api, "openai-completions", "unexpected API for {model_id}");
        let cache_control = model
            .compat
            .as_ref()
            .and_then(|c| c.as_openai_completions())
            .and_then(|c| c.cache_control_format);
        assert_eq!(cache_control, Some(CacheControlFormat::Anthropic));
    }
}
