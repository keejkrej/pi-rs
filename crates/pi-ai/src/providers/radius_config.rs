//! Port of packages/ai/src/providers/radius-config.ts

use serde::{Deserialize, Serialize};
use serde_json::Value;

use pi_js::abort::AbortSignal;
use pi_js::fetch::{Headers, Request};
use pi_js::{Error, Result};

use crate::auth::types::OAuthCredential;
use crate::types::{Model, ModelCost, ModelInput, ThinkingLevelMap};
use crate::utils::text::owned_slice;

pub const DEFAULT_RADIUS_GATEWAY: &str = "https://radius.pi.dev";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RadiusGatewayModel {
    pub id: String,
    pub name: String,
    pub reasoning: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level_map: Option<ThinkingLevelMap>,
    pub input: Vec<ModelInput>,
    pub cost: ModelCost,
    pub context_window: i64,
    pub max_tokens: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RadiusGatewayConfig {
    pub base_url: String,
    pub models: Vec<RadiusGatewayModel>,
}

fn is_radius_gateway_model_shape(value: &Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    obj.get("id").is_some_and(Value::is_string)
        && obj.get("name").is_some_and(Value::is_string)
        && obj.get("reasoning").is_some_and(Value::is_boolean)
        && obj.get("input").is_some_and(Value::is_array)
        && obj.get("cost").is_some_and(Value::is_object)
        && obj.get("contextWindow").is_some_and(Value::is_number)
        && obj.get("maxTokens").is_some_and(Value::is_number)
}

fn sanitize_radius_gateway_config(config: &Value) -> Option<RadiusGatewayConfig> {
    let obj = config.as_object()?;
    let base_url = obj.get("baseUrl")?.as_str()?;
    let raw_models = obj.get("models")?.as_array()?;
    let mut models = Vec::new();
    for item in raw_models {
        if !is_radius_gateway_model_shape(item) {
            continue;
        }
        if let Ok(parsed) = serde_json::from_value::<RadiusGatewayModel>(item.clone()) {
            models.push(parsed);
        }
    }
    Some(RadiusGatewayConfig {
        base_url: base_url.to_owned(),
        models,
    })
}

pub fn normalize_radius_gateway_url(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    let with_scheme = if lower.starts_with("http://") || lower.starts_with("https://") {
        value.to_owned()
    } else {
        format!("https://{value}")
    };
    with_scheme.trim_end_matches('/').to_owned()
}

pub fn get_radius_credential_config(credential: Option<&OAuthCredential>) -> Option<RadiusGatewayConfig> {
    credential
        .and_then(|cred| cred.extra.get("gatewayConfig"))
        .and_then(sanitize_radius_gateway_config)
}

pub fn get_radius_models_from_config(provider_id: &str, config: &RadiusGatewayConfig) -> Vec<Model> {
    config
        .models
        .iter()
        .map(|model| Model {
            r#type: None,
            id: model.id.clone(),
            name: model.name.clone(),
            api: "pi-messages".to_owned(),
            provider: provider_id.to_owned(),
            base_url: config.base_url.clone(),
            reasoning: model.reasoning,
            thinking_level_map: model.thinking_level_map.clone(),
            prompt_cache: None,
            input: model.input.clone(),
            input_limits: None,
            cost: model.cost.clone(),
            context_window: model.context_window,
            max_tokens: model.max_tokens,
            headers: None,
            sampling_params: None,
            sampling_params_by_thinking_level: None,
            compat: None,
        })
        .collect()
}

pub fn get_radius_models(provider_id: &str, credential: Option<&OAuthCredential>) -> Vec<Model> {
    match get_radius_credential_config(credential) {
        Some(config) => get_radius_models_from_config(provider_id, &config),
        None => Vec::new(),
    }
}

fn truncate_http_body(body: &str) -> String {
    let trimmed = pi_js::str16::trim(body);
    if pi_js::str16::len(trimmed) > 512 {
        format!("{}…", owned_slice(trimmed, 0, Some(512)))
    } else {
        trimmed.to_owned()
    }
}

pub async fn load_radius_gateway_config(
    gateway: &str,
    api_key: Option<&str>,
    signal: Option<&AbortSignal>,
) -> Result<RadiusGatewayConfig> {
    let base = url::Url::parse(gateway).map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?;
    let url = base
        .join("/v1/config")
        .map_err(|e| Error::js("TypeError", format!("Invalid URL: {e}")))?;

    let mut headers = Headers::new();
    headers.set("accept", "application/json");
    if let Some(key) = api_key.filter(|k| !k.is_empty()) {
        headers.set("authorization", &format!("Bearer {key}"));
    }

    let mut request = Request::get(url.to_string()).with_headers(headers);
    if let Some(sig) = signal {
        request = request.signal(sig.clone());
    }

    let response = pi_js::fetch::fetch(request).await?;
    if !response.ok() {
        let body = match response.text().await {
            Ok(t) => t,
            Err(_) => String::new(),
        };
        return Err(Error::msg(format!(
            "Could not load Radius config from {gateway}: {}: {}",
            response.status(),
            truncate_http_body(&body)
        )));
    }

    let body = response.text().await?;
    let json_val = pi_js::json::parse(&body)?;
    sanitize_radius_gateway_config(&json_val).ok_or_else(|| Error::msg(format!("Invalid Radius config from {gateway}")))
}
