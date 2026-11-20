//! Port of packages/coding-agent/test/model-search.test.ts

use std::sync::Arc;

use pi_coding_agent::modes::interactive::model_search::{get_distinct_model_name, get_model_selector_search_text};
use pi_tui::fuzzy_filter;

#[test]
fn get_distinct_model_name_hides_names_that_restate_the_id() {
    assert_eq!(
        get_distinct_model_name("claude-sonnet-4-5", Some("Claude Sonnet 4.5")),
        None
    );
    assert_eq!(
        get_distinct_model_name("openai/gpt-oss-120b", Some("OpenAI: gpt-oss-120b")),
        None
    );
    assert_eq!(
        get_distinct_model_name(
            "eu.anthropic.claude-sonnet-4-5-20250929-v1:0",
            Some("Claude Sonnet 4.5 (EU)")
        ),
        None
    );
    assert_eq!(get_distinct_model_name("gpt-5", Some("")), None);
    assert_eq!(get_distinct_model_name("gpt-5", None), None);
}

#[test]
fn get_distinct_model_name_shows_names_with_words_the_id_lacks() {
    let name = "MiniMax M3 [rasmus@openrouter, parasail]";
    assert_eq!(get_distinct_model_name("minimax/minimax-m3", Some(name)), Some(name));
    assert_eq!(
        get_distinct_model_name("gemini-3.1-flash-lite-image", Some("Nano Banana 2 Lite")),
        Some("Nano Banana 2 Lite")
    );
}

#[test]
fn get_distinct_model_name_requires_single_character_words_to_match_whole_id_token() {
    assert_eq!(
        get_distinct_model_name("mistral-small-2603", Some("Mistral Small 4")),
        Some("Mistral Small 4")
    );
}

#[derive(Clone)]
struct SearchModelItem {
    provider: &'static str,
    id: &'static str,
    name: Option<&'static str>,
}

#[test]
fn model_selector_search_ranks_models_containing_query_above_scattered_matches_11220() {
    let models = vec![
        SearchModelItem {
            provider: "openrouter",
            id: "openai/gpt-oss-120b",
            name: Some("OpenAI: gpt-oss-120b"),
        },
        SearchModelItem {
            provider: "openrouter",
            id: "anthropic/claude-sonnet-4.5",
            name: Some("Anthropic: Claude Sonnet 4.5"),
        },
        SearchModelItem {
            provider: "openrouter",
            id: "anthropic/claude-opus-4.1",
            name: Some("Anthropic: Claude Opus 4.1"),
        },
        SearchModelItem {
            provider: "anthropic",
            id: "claude-sonnet-4-5",
            name: Some("Claude Sonnet 4.5"),
        },
        SearchModelItem {
            provider: "anthropic",
            id: "claude-opus-4-1",
            name: Some("Claude Opus 4.1"),
        },
    ];

    let result = fuzzy_filter(
        &models,
        "opus",
        Arc::new(|m: &SearchModelItem| get_model_selector_search_text(m.id, m.provider, m.name)),
    );
    let mut top_two: Vec<&str> = result.iter().take(2).map(|m| m.id).collect();
    top_two.sort_unstable();
    assert_eq!(top_two, vec!["anthropic/claude-opus-4.1", "claude-opus-4-1"]);
}

#[test]
fn model_selector_search_ranks_typed_model_above_shared_provider_prefix_10065() {
    let models = vec![
        SearchModelItem {
            provider: "fireworks",
            id: "accounts/fireworks/models/deepseek-v4-flash-0731",
            name: None,
        },
        SearchModelItem {
            provider: "fireworks",
            id: "accounts/fireworks/models/kimi-k3",
            name: None,
        },
        SearchModelItem {
            provider: "fireworks",
            id: "accounts/fireworks/routers/firerouter",
            name: Some("FireRouter"),
        },
    ];

    let result = fuzzy_filter(
        &models,
        "firerouter",
        Arc::new(|m: &SearchModelItem| get_model_selector_search_text(m.id, m.provider, m.name)),
    );
    assert_eq!(
        result.first().map(|m| m.id),
        Some("accounts/fireworks/routers/firerouter")
    );
}
