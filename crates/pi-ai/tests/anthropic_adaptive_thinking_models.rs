//! Port of packages/ai/test/anthropic-adaptive-thinking-models.test.ts

use pi_ai::compat::{get_models, get_providers};
use pi_ai::types::ModelCompat;

const EXPECTED_CURRENT_ADAPTIVE_THINKING_MODELS: &[&str] = &[
    "anthropic/claude-fable-5",
    "anthropic/claude-opus-4-8",
    "anthropic/claude-opus-5",
    "anthropic/claude-sonnet-5",
    "cloudflare-ai-gateway/claude-fable-5",
    "fireworks/accounts/fireworks/models/deepseek-v4p1-flash",
    "fireworks/accounts/fireworks/models/gpt-oss-120b",
    "fireworks/accounts/fireworks/models/qwen3p8-max",
    "kimi-coding/kimi-for-coding",
    "kimi-coding/k3",
    "kimi-coding/kimi-for-coding-highspeed",
    "opencode/claude-opus-4-8",
    "opencode/claude-opus-5",
    "vercel-ai-gateway/anthropic/claude-opus-4.8",
    "vercel-ai-gateway/anthropic/claude-opus-5",
    "vercel-ai-gateway/anthropic/claude-sonnet-5",
];

fn matches_adaptive_pattern(model_id: &str) -> bool {
    if model_id.starts_with("fireworks/") || model_id.starts_with("kimi-coding/") {
        return true;
    }
    const PATTERNS: &[&str] = &[
        "opus-4-6",
        "opus-4.6",
        "opus.4-6",
        "opus.4.6",
        "opus-4-7",
        "opus-4.7",
        "opus.4-7",
        "opus.4.7",
        "opus-4-8",
        "opus-4.8",
        "opus.4-8",
        "opus.4.8",
        "opus-5",
        "opus.5",
        "sonnet-4-6",
        "sonnet-4.6",
        "sonnet.4-6",
        "sonnet.4.6",
        "sonnet-5",
        "sonnet.5",
        "fable-5",
        "fable.5",
    ];
    PATTERNS.iter().any(|pat| model_id.contains(pat))
}

#[test]
fn marks_built_in_anthropic_messages_models_that_use_adaptive_thinking() {
    let mut flagged: Vec<String> = get_providers()
        .into_iter()
        .flat_map(|p| get_models(Some(p.id())))
        .filter(|m| m.api == "anthropic-messages")
        .filter(|m| {
            matches!(
                &m.compat,
                Some(ModelCompat::AnthropicMessages(c)) if c.force_adaptive_thinking == Some(true)
            )
        })
        .map(|m| format!("{}/{}", m.provider, m.id))
        .collect();
    flagged.sort();

    for expected in EXPECTED_CURRENT_ADAPTIVE_THINKING_MODELS {
        assert!(
            flagged.iter().any(|id| id == expected),
            "expected adaptive model {expected} to be flagged, flagged={flagged:?}"
        );
    }

    for model_id in &flagged {
        assert!(
            matches_adaptive_pattern(model_id),
            "unexpected adaptive model flagged: {model_id}"
        );
    }
}
