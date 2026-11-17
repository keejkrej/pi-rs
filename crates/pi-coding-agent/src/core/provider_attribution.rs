//! Port of packages/coding-agent/src/core/provider-attribution.ts

use indexmap::IndexMap;
use pi_ai::{Model, ProviderHeaders};

use crate::core::settings_manager::SettingsManager;
use crate::core::telemetry::is_install_telemetry_enabled;

const OPENROUTER_HOST: &str = "openrouter.ai";
const NVIDIA_NIM_HOST: &str = "integrate.api.nvidia.com";
const CLOUDFLARE_API_HOST: &str = "api.cloudflare.com";
const CLOUDFLARE_AI_GATEWAY_HOST: &str = "gateway.ai.cloudflare.com";
const OPENCODE_HOST: &str = "opencode.ai";

fn matches_host(base_url: &str, expected_host: &str) -> bool {
    url::Url::parse(base_url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h == expected_host))
        .unwrap_or(false)
}

fn is_open_router_model(model: &Model) -> bool {
    model.provider == "openrouter" || model.base_url.contains(OPENROUTER_HOST)
}

fn is_nvidia_nim_model(model: &Model) -> bool {
    model.provider == "nvidia" || matches_host(&model.base_url, NVIDIA_NIM_HOST)
}

fn is_cloudflare_model(model: &Model) -> bool {
    model.provider == "cloudflare-workers-ai"
        || model.provider == "cloudflare-ai-gateway"
        || matches_host(&model.base_url, CLOUDFLARE_API_HOST)
        || matches_host(&model.base_url, CLOUDFLARE_AI_GATEWAY_HOST)
}

fn get_default_attribution_headers(
    model: &Model,
    settings_manager: &SettingsManager,
) -> Option<IndexMap<String, String>> {
    if !is_install_telemetry_enabled(settings_manager, None) {
        return None;
    }

    let mut headers = IndexMap::new();
    if is_open_router_model(model) {
        headers.insert("HTTP-Referer".to_owned(), "https://pi.dev".to_owned());
        headers.insert("X-OpenRouter-Title".to_owned(), "pi".to_owned());
        headers.insert("X-OpenRouter-Categories".to_owned(), "cli-agent".to_owned());
        return Some(headers);
    }

    if is_nvidia_nim_model(model) {
        headers.insert("X-BILLING-INVOKE-ORIGIN".to_owned(), "Pi".to_owned());
        return Some(headers);
    }

    if is_cloudflare_model(model) {
        headers.insert("User-Agent".to_owned(), "pi-coding-agent".to_owned());
        return Some(headers);
    }

    None
}

fn get_session_headers(model: &Model, session_id: Option<&str>) -> Option<IndexMap<String, String>> {
    let sid = session_id.filter(|s| !s.is_empty())?;
    if model.provider != "opencode" && model.provider != "opencode-go" && !matches_host(&model.base_url, OPENCODE_HOST)
    {
        return None;
    }
    let mut headers = IndexMap::new();
    headers.insert("x-opencode-session".to_owned(), sid.to_owned());
    headers.insert("x-opencode-client".to_owned(), "pi".to_owned());
    Some(headers)
}

pub fn merge_provider_attribution_headers(
    model: &Model,
    settings_manager: &SettingsManager,
    session_id: Option<&str>,
    header_sources: &[Option<&ProviderHeaders>],
) -> Option<ProviderHeaders> {
    let mut merged: ProviderHeaders = IndexMap::new();

    if let Some(session_hdrs) = get_session_headers(model, session_id) {
        for (k, v) in session_hdrs {
            merged.insert(k, Some(v));
        }
    }
    if let Some(default_hdrs) = get_default_attribution_headers(model, settings_manager) {
        for (k, v) in default_hdrs {
            merged.insert(k, Some(v));
        }
    }
    for source in header_sources.iter().flatten() {
        for (k, v) in *source {
            merged.insert(k.clone(), v.clone());
        }
    }

    if merged.is_empty() { None } else { Some(merged) }
}
