//! Port of packages/ai/src/api/azure-openai-config.ts

use indexmap::IndexMap;
use pi_js::error::{Error, Result};
use url::Url;

use crate::types::{Model, StreamOptions};
use crate::utils::provider_env::get_provider_env_value;

pub const DEFAULT_AZURE_API_VERSION: &str = "v1";

/// Azure models ship without a `base_url`: one resource per user, resolved per request.
#[derive(Debug, Clone, Default)]
pub struct AzureEndpointOptions {
    pub base: StreamOptions,
    pub azure_api_version: Option<String>,
    pub azure_resource_name: Option<String>,
    pub azure_base_url: Option<String>,
    pub azure_deployment_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AzureConfig {
    pub base_url: String,
    pub api_version: String,
}

fn parse_deployment_name_map(value: Option<&str>) -> IndexMap<String, String> {
    let mut map = IndexMap::new();
    let Some(raw) = value else {
        return map;
    };
    for entry in raw.split(',') {
        let trimmed = pi_js::str16::trim(entry);
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.split('=');
        let Some(model_id) = parts.next() else {
            continue;
        };
        let Some(deployment_name) = parts.next() else {
            continue;
        };
        let model_id = pi_js::str16::trim(model_id);
        let deployment_name = pi_js::str16::trim(deployment_name);
        if model_id.is_empty() || deployment_name.is_empty() {
            continue;
        }
        map.insert(model_id.to_owned(), deployment_name.to_owned());
    }
    map
}

pub fn resolve_deployment_name(model: &Model, options: Option<&AzureEndpointOptions>) -> String {
    if let Some(name) = options
        .and_then(|o| o.azure_deployment_name.as_deref())
        .filter(|s| !s.is_empty())
    {
        return name.to_owned();
    }
    let env = options.and_then(|o| o.base.env.as_ref());
    let env_map = get_provider_env_value("AZURE_OPENAI_DEPLOYMENT_NAME_MAP", env);
    if let Some(mapped) = parse_deployment_name_map(env_map.as_deref()).get(&model.id) {
        return mapped.clone();
    }
    model.id.clone()
}

pub fn normalize_azure_base_url(base_url: &str) -> Result<String> {
    let trimmed = pi_js::str16::trim(base_url).trim_end_matches('/');
    let mut url = Url::parse(trimmed).map_err(|_| Error::msg(format!("Invalid Azure OpenAI base URL: {base_url}")))?;

    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    let is_azure_host = host.ends_with(".openai.azure.com")
        || host.ends_with(".cognitiveservices.azure.com")
        || host.ends_with(".ai.azure.com");
    let normalized_path = url.path().trim_end_matches('/').to_owned();

    // Ensure Azure hosts have /openai/v1 as base path so the AzureOpenAI client
    // can append /deployments/<model>/... and ?api-version=v1 correctly.
    if is_azure_host
        && (normalized_path.is_empty()
            || normalized_path == "/"
            || normalized_path == "/openai"
            || normalized_path == "/openai/v1/responses")
    {
        url.set_path("/openai/v1");
        url.set_query(None);
    }

    Ok(url.to_string().trim_end_matches('/').to_owned())
}

fn build_default_base_url(resource_name: &str) -> String {
    format!("https://{resource_name}.openai.azure.com/openai/v1")
}

pub fn resolve_azure_base_url(model: &Model, options: Option<&AzureEndpointOptions>) -> Result<String> {
    let env = options.and_then(|o| o.base.env.as_ref());
    let base_url = options
        .and_then(|o| o.azure_base_url.as_deref())
        .map(pi_js::str16::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            get_provider_env_value("AZURE_OPENAI_BASE_URL", env)
                .map(|s| pi_js::str16::trim(&s).to_owned())
                .filter(|s| !s.is_empty())
        });
    let resource_name = options
        .and_then(|o| o.azure_resource_name.as_deref())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| get_provider_env_value("AZURE_OPENAI_RESOURCE_NAME", env).filter(|s| !s.is_empty()));

    let mut resolved_base_url = base_url;

    if resolved_base_url.is_none()
        && let Some(ref res_name) = resource_name
    {
        resolved_base_url = Some(build_default_base_url(res_name));
    }

    if resolved_base_url.is_none() && !model.base_url.is_empty() {
        resolved_base_url = Some(model.base_url.clone());
    }

    let Some(resolved) = resolved_base_url else {
        return Err(Error::msg(
            "Azure OpenAI base URL is required. Set AZURE_OPENAI_BASE_URL or AZURE_OPENAI_RESOURCE_NAME, or pass azureBaseUrl, azureResourceName, or model.baseUrl.",
        ));
    };

    normalize_azure_base_url(&resolved)
}

pub fn resolve_azure_config(model: &Model, options: Option<&AzureEndpointOptions>) -> Result<AzureConfig> {
    let env = options.and_then(|o| o.base.env.as_ref());
    let api_version = options
        .and_then(|o| o.azure_api_version.as_deref())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| get_provider_env_value("AZURE_OPENAI_API_VERSION", env).filter(|s| !s.is_empty()))
        .unwrap_or_else(|| DEFAULT_AZURE_API_VERSION.to_owned());

    Ok(AzureConfig {
        base_url: resolve_azure_base_url(model, options)?,
        api_version,
    })
}
