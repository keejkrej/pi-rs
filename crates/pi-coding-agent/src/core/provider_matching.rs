//! Port of packages/coding-agent/src/core/provider-matching.ts

pub trait NamedProvider {
    fn provider_id(&self) -> &str;
    fn provider_name(&self) -> &str;
}

impl<T: pi_ai::models::Provider + ?Sized> NamedProvider for T {
    fn provider_id(&self) -> &str {
        self.id()
    }

    fn provider_name(&self) -> &str {
        self.name()
    }
}

/// Match provider IDs and display names exactly, ignoring case and surrounding whitespace in the reference.
pub fn find_matching_providers<'a, T: NamedProvider>(providers: &'a [T], provider_ref: &str) -> Vec<&'a T> {
    let normalized = pi_js::str16::trim(provider_ref).to_ascii_lowercase();
    if normalized.is_empty() {
        return Vec::new();
    }
    providers
        .iter()
        .filter(|provider| {
            provider.provider_id().to_ascii_lowercase() == normalized
                || provider.provider_name().to_ascii_lowercase() == normalized
        })
        .collect()
}

/// Match `Arc<dyn Provider + Send + Sync>` slices directly.
pub fn find_matching_provider_arcs<'a>(
    providers: &'a [std::sync::Arc<dyn pi_ai::models::Provider + Send + Sync>],
    provider_ref: &str,
) -> Vec<&'a std::sync::Arc<dyn pi_ai::models::Provider + Send + Sync>> {
    let normalized = pi_js::str16::trim(provider_ref).to_ascii_lowercase();
    if normalized.is_empty() {
        return Vec::new();
    }
    providers
        .iter()
        .filter(|provider| {
            provider.id().to_ascii_lowercase() == normalized || provider.name().to_ascii_lowercase() == normalized
        })
        .collect()
}
