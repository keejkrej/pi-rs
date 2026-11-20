//! Port of packages/ai/src/utils/openrouter-cost.ts

use serde::{Deserialize, Serialize};

use crate::types::{Model, Usage};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpenRouterCostDetails {
    /// Total inference cost billed through the user's upstream provider account, when available.
    pub upstream_inference_cost: Option<f64>,
    /// Upstream cost for prompt processing, including any provider-side cache pricing.
    pub upstream_inference_prompt_cost: f64,
    /// Upstream cost for completion generation.
    pub upstream_inference_completions_cost: f64,
    /// Metered server-tool cost already included in OpenRouter's charged `cost`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_tool_cost: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpenRouterUsageAccounting {
    /// Amount charged to the user's OpenRouter account.
    pub cost: Option<f64>,
    /// Whether OpenRouter used the user's upstream provider key.
    pub is_byok: bool,
    pub cost_details: Option<OpenRouterCostDetails>,
}

fn is_valid_cost(value: Option<f64>) -> Option<f64> {
    value.filter(|v| v.is_finite() && *v >= 0.0)
}

pub fn is_openrouter_model_endpoint(provider: &str, base_url: &str) -> bool {
    provider == "openrouter" || base_url.contains("openrouter.ai")
}

pub fn is_openrouter_model(model: &Model) -> bool {
    is_openrouter_model_endpoint(&model.provider, &model.base_url)
}

/// Replace the catalog total with OpenRouter's billed cost when the response contains complete, valid accounting.
pub fn apply_openrouter_reported_cost(usage: &mut Usage, reported: &OpenRouterUsageAccounting) {
    let Some(cost) = is_valid_cost(reported.cost) else {
        return;
    };

    if !reported.is_byok {
        usage.cost.total = cost;
        usage.cost.total_from_provider = Some(true);
        return;
    }

    let Some(details) = &reported.cost_details else {
        return;
    };
    let upstream_cost = match details.upstream_inference_cost {
        Some(v) => match is_valid_cost(Some(v)) {
            Some(valid) => valid,
            None => return,
        },
        None => {
            let Some(prompt) = is_valid_cost(Some(details.upstream_inference_prompt_cost)) else {
                return;
            };
            let Some(completions) = is_valid_cost(Some(details.upstream_inference_completions_cost)) else {
                return;
            };
            prompt + completions
        }
    };
    if !upstream_cost.is_finite() || upstream_cost < 0.0 {
        return;
    }
    usage.cost.total = cost + upstream_cost;
    usage.cost.total_from_provider = Some(true);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::UsageCost;

    fn sample_usage(catalog_total: f64) -> Usage {
        Usage {
            input: 100,
            output: 50,
            cost: UsageCost {
                input: 0.01,
                output: 0.02,
                cache_read: 0.0,
                cache_write: 0.0,
                total: catalog_total,
                total_from_provider: Some(false),
            },
            ..Usage::default()
        }
    }

    #[test]
    fn non_byok_replaces_total_when_valid() {
        let mut usage = sample_usage(0.03);
        apply_openrouter_reported_cost(
            &mut usage,
            &OpenRouterUsageAccounting {
                cost: Some(0.0123),
                is_byok: false,
                cost_details: None,
            },
        );
        assert_eq!(usage.cost.total, 0.0123);
        assert_eq!(usage.cost.total_from_provider, Some(true));
    }

    #[test]
    fn byok_sums_platform_and_upstream_or_component_costs() {
        let mut usage = sample_usage(0.05);
        apply_openrouter_reported_cost(
            &mut usage,
            &OpenRouterUsageAccounting {
                cost: Some(0.001),
                is_byok: true,
                cost_details: Some(OpenRouterCostDetails {
                    upstream_inference_cost: None,
                    upstream_inference_prompt_cost: 0.01,
                    upstream_inference_completions_cost: 0.02,
                    server_tool_cost: None,
                }),
            },
        );
        assert!((usage.cost.total - 0.031).abs() < 1e-12);
        assert_eq!(usage.cost.total_from_provider, Some(true));
    }

    #[test]
    fn ignores_invalid_or_incomplete_accounting() {
        let mut usage = sample_usage(0.05);
        apply_openrouter_reported_cost(
            &mut usage,
            &OpenRouterUsageAccounting {
                cost: Some(-1.0),
                is_byok: false,
                cost_details: None,
            },
        );
        assert_eq!(usage.cost.total, 0.05);
        assert_eq!(usage.cost.total_from_provider, Some(false));

        apply_openrouter_reported_cost(
            &mut usage,
            &OpenRouterUsageAccounting {
                cost: Some(0.01),
                is_byok: true,
                cost_details: None,
            },
        );
        assert_eq!(usage.cost.total, 0.05);
    }
}
