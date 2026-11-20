//! Port of packages/ai/src/cache-warming.ts

use pi_js::abort::AbortSignal;

use crate::models::calculate_cost;
use crate::types::{AnyModel, CacheRetention, Model, SimpleStreamOptions, Usage};
use crate::utils::provider_env::get_provider_env_value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheWarmingPhase {
    Streaming,
    Idle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheWarmingAction {
    Warm,
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CacheWarmingDecision {
    pub phase: CacheWarmingPhase,
    pub warm_cost: f64,
    pub miss_cost: f64,
    pub continuation_probability: f64,
    pub expected_savings: f64,
    pub economics_available: bool,
    pub action: CacheWarmingAction,
}

/// Streaming warming never continues past this long after the real request that started it.
pub const CACHE_WARMING_MAX_AGE_MS: u64 = 60 * 60_000;

/// Idle warming uses a shorter horizon because continuation estimates become less reliable with age.
pub const CACHE_WARMING_MAX_IDLE_AGE_MS: u64 = 30 * 60_000;

/// A refresh is sent only when it is expected to save at least this many dollars.
pub const CACHE_WARMING_MINIMUM_EXPECTED_SAVINGS: f64 = 0.05;

/// Chance that a real request arrives before the cache entry expires while the agent sits idle.
pub const CACHE_WARMING_IDLE_CONTINUATION_PROBABILITY: f64 = 0.15;

/// Refresh at 90% of the TTL while preserving at least ten seconds of margin.
pub fn get_cache_warming_delay_ms(ttl_ms: u64) -> Option<u64> {
    if ttl_ms <= 10_000 {
        return None;
    }
    let ninety_pct = ((ttl_ms as f64) * 0.9).floor() as u64;
    let margin = ttl_ms - 10_000;
    Some(ninety_pct.min(margin).max(1))
}

/// Lifetime of the prompt cache entry a request writes, in milliseconds, from `model.prompt_cache` for
/// the active retention tier (`None` when caching is off or lifetime is unknown).
pub fn get_prompt_cache_ttl_ms(model: &Model, options: Option<&SimpleStreamOptions>) -> Option<u64> {
    let retention = options.and_then(|o| o.base.cache_retention).unwrap_or_else(|| {
        if get_provider_env_value("PI_CACHE_RETENTION", options.and_then(|o| o.base.env.as_ref())).as_deref()
            == Some("long")
        {
            CacheRetention::Long
        } else {
            CacheRetention::Short
        }
    });
    if retention == CacheRetention::None {
        return None;
    }
    let seconds = model.prompt_cache.as_ref()?.get(retention)?;
    Some(((seconds as f64) * 1000.0).max(0.0) as u64)
}

/// Whether replaying the request with a one-token output cap leaves its cache entry untouched.
pub fn is_cache_warming_replayable(model: &Model, options: Option<&SimpleStreamOptions>) -> bool {
    if options.and_then(|o| o.reasoning).is_none() || model.api != "anthropic-messages" {
        return true;
    }
    model
        .compat
        .as_ref()
        .and_then(|c| c.as_anthropic_messages())
        .and_then(|c| c.force_adaptive_thinking)
        == Some(true)
}

/// The options of a refresh: the request's own, capped at one output token, without retries,
/// and without replacing the conversation's provider thread state.
pub fn get_cache_warming_request_options(options: &SimpleStreamOptions, signal: AbortSignal) -> SimpleStreamOptions {
    let mut out = options.clone();
    out.base.max_tokens = Some(1);
    out.base.disable_continuation = Some(true);
    out.base.max_retries = Some(0);
    out.base.signal = Some(signal);
    out
}

/// Decide whether refreshing the cache entry of a `prompt_tokens`-token request pays for itself in `phase`.
pub fn evaluate_cache_warming(model: &Model, prompt_tokens: i64, phase: CacheWarmingPhase) -> CacheWarmingDecision {
    let cache_hit_cost = price(model, 0, 0, prompt_tokens, 0);
    let cache_miss_cost = if model.cost.cache_write > 0.0 {
        price(model, 0, 0, 0, prompt_tokens)
    } else {
        price(model, prompt_tokens, 0, 0, 0)
    };
    let warm_cost = price(model, 0, 1, prompt_tokens, 0);
    let miss_cost = (cache_miss_cost - cache_hit_cost).max(0.0);
    let continuation_probability = match phase {
        CacheWarmingPhase::Idle => CACHE_WARMING_IDLE_CONTINUATION_PROBABILITY,
        CacheWarmingPhase::Streaming => 1.0,
    };
    let economics_available = prompt_tokens > 0 && (cache_hit_cost > 0.0 || cache_miss_cost > 0.0);
    let expected_savings = continuation_probability * miss_cost - warm_cost;
    let action = if expected_savings >= CACHE_WARMING_MINIMUM_EXPECTED_SAVINGS {
        CacheWarmingAction::Warm
    } else {
        CacheWarmingAction::Stop
    };
    CacheWarmingDecision {
        phase,
        warm_cost,
        miss_cost,
        continuation_probability,
        expected_savings,
        economics_available,
        action,
    }
}

fn price(model: &Model, input: i64, output: i64, cache_read: i64, cache_write: i64) -> f64 {
    let mut usage = Usage {
        input,
        output,
        cache_read,
        cache_write,
        ..Usage::default()
    };
    calculate_cost(&AnyModel::Chat(model.clone()), &mut usage).total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AnthropicMessagesCompat, ModelCompat, ModelCost, ModelInput, ModelPromptCache, ThinkingLevel};
    use indexmap::IndexMap;

    fn sample_model() -> Model {
        Model {
            r#type: None,
            id: "claude-opus".to_owned(),
            name: "Claude Opus".to_owned(),
            api: "anthropic-messages".to_owned(),
            provider: "anthropic".to_owned(),
            base_url: "https://api.anthropic.com".to_owned(),
            reasoning: true,
            thinking_level_map: None,
            prompt_cache: Some(ModelPromptCache {
                short: Some(300),
                long: Some(3600),
            }),
            input: vec![ModelInput::Text],
            input_limits: None,
            cost: ModelCost {
                input: 15.0,
                output: 75.0,
                cache_read: 1.5,
                cache_write: 18.75,
                tiers: None,
            },
            context_window: 200_000,
            max_tokens: 16_384,
            headers: None,
            sampling_params: None,
            sampling_params_by_thinking_level: None,
            compat: None,
        }
    }

    #[test]
    fn delay_and_ttl_and_replayable_work() {
        assert_eq!(get_cache_warming_delay_ms(10_000), None);
        assert_eq!(get_cache_warming_delay_ms(300_000), Some(270_000));
        assert_eq!(get_cache_warming_delay_ms(20_000), Some(10_000));

        let mut model = sample_model();
        let mut opts = SimpleStreamOptions::default();
        opts.base.env = Some(IndexMap::new());
        assert_eq!(get_prompt_cache_ttl_ms(&model, Some(&opts)), Some(300_000));
        opts.base.cache_retention = Some(CacheRetention::Long);
        assert_eq!(get_prompt_cache_ttl_ms(&model, Some(&opts)), Some(3_600_000));
        opts.base.cache_retention = Some(CacheRetention::None);
        assert_eq!(get_prompt_cache_ttl_ms(&model, Some(&opts)), None);

        opts.reasoning = Some(ThinkingLevel::High);
        assert!(!is_cache_warming_replayable(&model, Some(&opts)));
        model.compat = Some(ModelCompat::AnthropicMessages(AnthropicMessagesCompat {
            force_adaptive_thinking: Some(true),
            ..Default::default()
        }));
        assert!(is_cache_warming_replayable(&model, Some(&opts)));
    }

    #[test]
    fn evaluates_warming_economics() {
        let model = sample_model();
        let big_stream = evaluate_cache_warming(&model, 50_000, CacheWarmingPhase::Streaming);
        assert_eq!(big_stream.action, CacheWarmingAction::Warm);
        assert!(big_stream.economics_available);

        let tiny = evaluate_cache_warming(&model, 100, CacheWarmingPhase::Idle);
        assert_eq!(tiny.action, CacheWarmingAction::Stop);
    }
}
