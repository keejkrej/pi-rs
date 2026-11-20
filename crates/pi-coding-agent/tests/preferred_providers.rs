//! Port of packages/coding-agent/test/preferred-providers.test.ts

use pi_coding_agent::core::preferred_providers::{PREFERRED_PROVIDER_IDS, compare_providers, is_preferred_provider};
use pretty_assertions::assert_eq;

#[test]
fn orders_preferred_partners_first_then_the_rest_by_name() {
    let mut providers = vec![
        ("zai", "Z.AI"),
        ("openrouter", "OpenRouter"),
        ("anthropic", "Anthropic"),
        ("radius", "Radius"),
        ("google", "Google"),
    ];

    assert_eq!(PREFERRED_PROVIDER_IDS, &["radius", "openrouter"]);
    providers.sort_by(|a, b| compare_providers(a.0, a.1, b.0, b.1));
    let ids: Vec<&str> = providers.into_iter().map(|(id, _)| id).collect();
    assert_eq!(ids, vec!["radius", "openrouter", "anthropic", "google", "zai"]);
}

#[test]
fn recognizes_only_the_preferred_partners() {
    assert!(is_preferred_provider("radius"));
    assert!(is_preferred_provider("openrouter"));
    assert!(!is_preferred_provider("anthropic"));
}
