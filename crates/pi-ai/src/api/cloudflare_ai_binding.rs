//! Port of packages/ai/src/api/cloudflare-ai-binding.ts

use std::sync::Arc;

use pi_js::{Error, Result};

use crate::types::FetchFunction;

/// The Workers AI binding (`env.AI`), modeled structurally without a runtime-specific SDK crate.
#[derive(Clone, Default)]
pub struct AiBinding {
    pub ai_gateway_log_id: Option<String>,
    pub fetch: Option<FetchFunction>,
}

/// Placeholder auth header value for binding-routed requests.
///
/// API implementations require an API key or recognized auth header (`authorization`, `x-api-key`,
/// `cf-aig-authorization`) before dispatch. Pair with `Authorization: None` / `x-api-key: None`
/// so placeholder SDK auth headers never override AI Gateway's stored BYOK keys.
pub const CLOUDFLARE_GATEWAY_BINDING_AUTH_SENTINEL: &str = "cloudflare-gateway-binding";

/// Create a `FetchFunction` backed by the AI binding, for models whose `base_url` names a route
/// the binding serves (for example `https://workers-binding.ai/ai-gateway/gateways/{gateway}/...`).
pub fn create_ai_binding_fetch(binding: &AiBinding) -> Result<FetchFunction> {
    let Some(binding_fetch) = binding.fetch.clone() else {
        return Err(Error::js(
            "TypeError",
            "createAiBindingFetch: the AI binding does not expose fetch()",
        ));
    };
    Ok(Arc::new(move |request| (binding_fetch)(request)))
}
