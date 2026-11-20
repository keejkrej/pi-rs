//! Port of packages/ai/src/providers/openrouter-api.ts

use serde::{Deserialize, Serialize};

use crate::types::{ModelCost, ModelCostTier};

/// A conditional price. `min_prompt_tokens` selects prompt-length pricing; `utc_*` fields select
/// time-of-day or weekday pricing. Missing rates keep the base price.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpenRouterPricingOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_prompt_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utc_start: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utc_end: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utc_days: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_cache_read: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_cache_write: Option<String>,
}

/// OpenRouter's per-token prices, as decimal strings.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OpenRouterPricing {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_cache_read: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_cache_write: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overrides: Option<Vec<OpenRouterPricingOverride>>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OpenRouterTopProvider {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<i64>,
}

/// The fields pi reads from a model in OpenRouter's `GET /models` and `GET /models/user`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpenRouterModel {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_provider: Option<OpenRouterTopProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<OpenRouterPricing>,
}

/// Convert one $/token price to $/1M tokens. 0 means free. Missing, invalid and negative prices use
/// the fallback: OpenRouter reports -1 for router models such as `openrouter/auto`, whose price
/// depends on the model they pick.
fn per_million(value: Option<&str>, fallback: f64) -> f64 {
    let Some(raw) = value else {
        return fallback;
    };
    let parsed = pi_js::num::parse_float(raw);
    if !parsed.is_finite() || parsed < 0.0 {
        return fallback;
    }
    let scaled = parsed * 1_000_000.0;
    format!("{scaled:.6}").parse::<f64>().unwrap_or(scaled)
}

/// Convert OpenRouter's $/token prices to pi's $/1M tokens. Missing, invalid and negative base
/// prices are 0. Prompt-length overrides become request-wide tiers; a rate a tier does not list
/// keeps the base price. Time-of-day overrides are skipped because [`ModelCost`] cannot express them.
pub fn open_router_cost(pricing: Option<&OpenRouterPricing>) -> ModelCost {
    let input = per_million(pricing.and_then(|p| p.prompt.as_deref()), 0.0);
    let output = per_million(pricing.and_then(|p| p.completion.as_deref()), 0.0);
    let cache_read = per_million(pricing.and_then(|p| p.input_cache_read.as_deref()), 0.0);
    let cache_write = per_million(pricing.and_then(|p| p.input_cache_write.as_deref()), 0.0);

    let tiers: Vec<ModelCostTier> = pricing
        .and_then(|p| p.overrides.as_ref())
        .into_iter()
        .flatten()
        .filter_map(|override_entry| {
            let min_prompt_tokens = override_entry.min_prompt_tokens?;
            if override_entry.utc_start.is_some()
                || override_entry.utc_end.is_some()
                || override_entry.utc_days.is_some()
            {
                return None;
            }
            Some(ModelCostTier {
                input_tokens_above: min_prompt_tokens,
                input: per_million(override_entry.prompt.as_deref(), input),
                output: per_million(override_entry.completion.as_deref(), output),
                cache_read: per_million(override_entry.input_cache_read.as_deref(), cache_read),
                cache_write: per_million(override_entry.input_cache_write.as_deref(), cache_write),
            })
        })
        .collect();

    ModelCost {
        input,
        output,
        cache_read,
        cache_write,
        tiers: (!tiers.is_empty()).then_some(tiers),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_openrouter_pricing_and_tiers() {
        let pricing = OpenRouterPricing {
            prompt: Some("0.0000015".to_owned()),
            completion: Some("0.000002".to_owned()),
            input_cache_read: Some("-1".to_owned()),
            input_cache_write: None,
            overrides: Some(vec![
                OpenRouterPricingOverride {
                    min_prompt_tokens: Some(128_000),
                    utc_start: None,
                    utc_end: None,
                    utc_days: None,
                    prompt: Some("0.000003".to_owned()),
                    completion: None,
                    input_cache_read: Some("0.0000005".to_owned()),
                    input_cache_write: None,
                },
                OpenRouterPricingOverride {
                    min_prompt_tokens: Some(1000),
                    utc_start: Some(0.0),
                    utc_end: Some(8.0),
                    utc_days: None,
                    prompt: Some("0.000001".to_owned()),
                    completion: None,
                    input_cache_read: None,
                    input_cache_write: None,
                },
            ]),
        };

        let cost = open_router_cost(Some(&pricing));
        assert_eq!(cost.input, 1.5);
        assert_eq!(cost.output, 2.0);
        assert_eq!(cost.cache_read, 0.0);
        assert_eq!(cost.cache_write, 0.0);
        let tiers = cost.tiers.expect("tiers");
        assert_eq!(tiers.len(), 1);
        assert_eq!(tiers[0].input_tokens_above, 128_000);
        assert_eq!(tiers[0].input, 3.0);
        assert_eq!(tiers[0].output, 2.0);
        assert_eq!(tiers[0].cache_read, 0.5);
    }
}
