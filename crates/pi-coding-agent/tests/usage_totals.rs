//! Port of packages/coding-agent/test/usage-totals.test.ts

use pi_ai::{Usage, UsageCost};
use pi_coding_agent::core::usage_totals::combine_usage;
use pretty_assertions::assert_eq;

fn usage(total: f64, total_from_provider: Option<bool>) -> Usage {
    Usage {
        input: 1,
        output: 1,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 2,
        cost: UsageCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total,
            total_from_provider,
        },
    }
}

#[test]
fn marks_the_combined_total_as_provider_reported_only_when_both_totals_are() {
    let both = combine_usage(&usage(1.0, Some(true)), &usage(2.0, Some(true)));
    assert_eq!(both.cost.total, 3.0);
    assert_eq!(both.cost.total_from_provider, Some(true));

    assert_eq!(
        combine_usage(&usage(1.0, Some(true)), &usage(2.0, None))
            .cost
            .total_from_provider,
        Some(false)
    );
    assert_eq!(
        combine_usage(&usage(1.0, None), &usage(2.0, Some(true)))
            .cost
            .total_from_provider,
        Some(false)
    );
    assert_eq!(
        combine_usage(&usage(1.0, None), &usage(2.0, None))
            .cost
            .total_from_provider,
        Some(false)
    );
}
