//! Port of packages/ai/test/openrouter-cost.test.ts

use pi_ai::types::{Usage, UsageCost};
use pi_ai::utils::openrouter_cost::{
    OpenRouterCostDetails, OpenRouterUsageAccounting, apply_openrouter_reported_cost, is_openrouter_model_endpoint,
};

fn sample_usage(total: f64) -> Usage {
    Usage {
        input: 1,
        output: 1,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 2,
        cost: UsageCost {
            input: 0.25,
            output: 0.75,
            cache_read: 0.0,
            cache_write: 0.0,
            total,
            total_from_provider: None,
        },
    }
}

#[test]
fn is_openrouter_model_recognizes_builtin_provider_and_openrouter_endpoints() {
    assert!(is_openrouter_model_endpoint("openrouter", "https://example.com"));
    assert!(is_openrouter_model_endpoint(
        "openrouter-work",
        "https://openrouter.ai/api/v1"
    ));
    assert!(!is_openrouter_model_endpoint("custom", "https://example.com/v1"));
}

#[test]
fn apply_openrouter_reported_cost_handles_non_byok_byok_components_and_incomplete_accounting() {
    let mut non_byok = sample_usage(1.0);
    apply_openrouter_reported_cost(
        &mut non_byok,
        &OpenRouterUsageAccounting {
            cost: Some(0.0),
            is_byok: false,
            cost_details: Some(OpenRouterCostDetails {
                upstream_inference_cost: Some(0.0),
                upstream_inference_prompt_cost: 0.0,
                upstream_inference_completions_cost: 0.0,
                server_tool_cost: None,
            }),
        },
    );
    assert_eq!(non_byok.cost.total, 0.0);
    assert_eq!(non_byok.cost.total_from_provider, Some(true));

    let mut byok = sample_usage(1.0);
    apply_openrouter_reported_cost(
        &mut byok,
        &OpenRouterUsageAccounting {
            cost: Some(0.1),
            is_byok: true,
            cost_details: Some(OpenRouterCostDetails {
                upstream_inference_cost: Some(0.9),
                upstream_inference_prompt_cost: 0.6,
                upstream_inference_completions_cost: 0.3,
                server_tool_cost: None,
            }),
        },
    );
    assert_eq!(byok.cost.total, 1.0);
    assert_eq!(byok.cost.total_from_provider, Some(true));

    let mut split = sample_usage(1.0);
    apply_openrouter_reported_cost(
        &mut split,
        &OpenRouterUsageAccounting {
            cost: Some(0.1),
            is_byok: true,
            cost_details: Some(OpenRouterCostDetails {
                upstream_inference_cost: None,
                upstream_inference_prompt_cost: 0.6,
                upstream_inference_completions_cost: 0.3,
                server_tool_cost: None,
            }),
        },
    );
    assert!((split.cost.total - 1.0).abs() < 1e-12);
    assert_eq!(split.cost.total_from_provider, Some(true));

    let mut incomplete = sample_usage(1.0);
    incomplete.cost.total_from_provider = Some(false);
    apply_openrouter_reported_cost(
        &mut incomplete,
        &OpenRouterUsageAccounting {
            cost: None,
            is_byok: false,
            cost_details: None,
        },
    );
    apply_openrouter_reported_cost(
        &mut incomplete,
        &OpenRouterUsageAccounting {
            cost: Some(0.1),
            is_byok: true,
            cost_details: None,
        },
    );
    assert_eq!(incomplete.cost.total, 1.0);
    assert_eq!(incomplete.cost.total_from_provider, Some(false));
}
