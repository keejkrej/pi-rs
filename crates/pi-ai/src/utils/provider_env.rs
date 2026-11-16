//! Port of packages/ai/src/utils/provider-env.ts

use crate::types::ProviderEnv;

/// Resolve a provider env value from scoped overrides before process env.
pub fn get_provider_env_value(name: &str, env: Option<&ProviderEnv>) -> Option<String> {
    if let Some(val) = env.and_then(|e| e.get(name)) {
        if !val.is_empty() {
            return Some(val.clone());
        }
    }
    pi_js::env::var(name).filter(|v| !v.is_empty())
}
