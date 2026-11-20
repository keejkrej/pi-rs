//! Port of packages/coding-agent/src/core/preferred-providers.ts

use std::cmp::Ordering;

use pi_js::intl::locale_compare;

use crate::core::radius::RADIUS_PROVIDER_ID;

/// IDs of the preferred partner providers, most preferred first.
pub const PREFERRED_PROVIDER_IDS: &[&str] = &[RADIUS_PROVIDER_ID, "openrouter"];

/// Whether the provider is a preferred partner.
pub fn is_preferred_provider(provider_id: &str) -> bool {
    PREFERRED_PROVIDER_IDS.contains(&provider_id)
}

fn preferred_rank(provider_id: &str) -> usize {
    PREFERRED_PROVIDER_IDS
        .iter()
        .position(|&id| id == provider_id)
        .unwrap_or(PREFERRED_PROVIDER_IDS.len())
}

/// Orders providers: preferred partners first, in their order, then all others alphabetically by name.
pub fn compare_providers(a_id: &str, a_name: &str, b_id: &str, b_name: &str) -> Ordering {
    preferred_rank(a_id)
        .cmp(&preferred_rank(b_id))
        .then_with(|| locale_compare(a_name, b_name))
}
