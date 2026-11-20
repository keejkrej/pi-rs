//! Port of packages/ai/src/providers/azure.ts

use std::sync::Arc;

use indexmap::IndexMap;
use pi_js::{BoxFuture, Error, Result};
use serde_json::{Map, Value};

use crate::api::azure_openai_responses::normalize_azure_base_url;
use crate::api::azure_openai_responses_lazy::azure_openai_responses_api;
use crate::api::lazy::lazy_stream;
use crate::api::openai_completions_lazy::openai_completions_api;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::{CreateProviderApi, CreateProviderOptions, Provider, create_provider};
use crate::providers::azure_models::azure_models;
use crate::types::{
    AnyModel, ApiStreamOptions, Model, OnPayloadFn, ProviderEnv, ProviderStreams, SimpleStreamOptions, StreamOptions,
};
use crate::utils::provider_env::get_provider_env_value;

fn build_default_base_url(resource_name: &str) -> String {
    format!("https://{resource_name}.openai.azure.com/openai/v1")
}

fn parse_deployment_name_map(value: Option<&str>) -> IndexMap<String, String> {
    let mut map = IndexMap::new();
    let Some(raw) = value else {
        return map;
    };
    for entry in raw.split(',') {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some((model_id, deployment_name)) = trimmed.split_once('=') else {
            continue;
        };
        let model_id = model_id.trim();
        let deployment_name = deployment_name.trim();
        if model_id.is_empty() || deployment_name.is_empty() {
            continue;
        }
        map.insert(model_id.to_owned(), deployment_name.to_owned());
    }
    map
}

// PORT: Stage 2 will move endpoint/deployment helpers into `src/api/azure_openai_config.rs`.
fn resolve_azure_base_url_parts(
    model: &Model,
    env: Option<&ProviderEnv>,
    azure_base_url: Option<&str>,
    azure_resource_name: Option<&str>,
) -> Result<String> {
    let explicit_base = azure_base_url
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            get_provider_env_value("AZURE_OPENAI_BASE_URL", env)
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
        });
    let resource_name = azure_resource_name
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| get_provider_env_value("AZURE_OPENAI_RESOURCE_NAME", env).filter(|s| !s.is_empty()));

    let resolved = explicit_base
        .or_else(|| resource_name.as_deref().map(build_default_base_url))
        .or_else(|| (!model.base_url.is_empty()).then(|| model.base_url.clone()));

    let Some(raw_base_url) = resolved else {
        return Err(Error::msg(
            "Azure OpenAI base URL is required. Set AZURE_OPENAI_BASE_URL or AZURE_OPENAI_RESOURCE_NAME, or pass azureBaseUrl, azureResourceName, or model.baseUrl.",
        ));
    };

    normalize_azure_base_url(&raw_base_url)
}

fn resolve_deployment_name_parts(
    model: &Model,
    env: Option<&ProviderEnv>,
    azure_deployment_name: Option<&str>,
) -> String {
    if let Some(name) = azure_deployment_name.filter(|s| !s.is_empty()) {
        return name.to_owned();
    }
    let env_map = get_provider_env_value("AZURE_OPENAI_DEPLOYMENT_NAME_MAP", env);
    if let Some(mapped) = parse_deployment_name_map(env_map.as_deref()).get(&model.id) {
        return mapped.clone();
    }
    model.id.clone()
}

fn wrap_payload_with_deployment(
    model_id: &str,
    deployment_name: String,
    inner_on_payload: Option<OnPayloadFn<Model>>,
) -> Option<OnPayloadFn<Model>> {
    if deployment_name == model_id {
        return inner_on_payload;
    }
    Some(Arc::new(move |payload: Value, payload_model: &Model| {
        let mut obj = match payload {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        obj.insert("model".to_owned(), Value::String(deployment_name.clone()));
        let rewritten = Value::Object(obj);
        if let Some(ref inner) = inner_on_payload {
            let inner = Arc::clone(inner);
            let model_owned = payload_model.clone();
            Box::pin(async move { inner(rewritten, &model_owned).await }) as BoxFuture<Result<Option<Value>>>
        } else {
            Box::pin(async move { Ok(Some(rewritten)) }) as BoxFuture<Result<Option<Value>>>
        }
    }))
}

fn resolve_stream_options(
    model: &Model,
    base: &StreamOptions,
    azure_base_url: Option<&str>,
    azure_resource_name: Option<&str>,
    azure_deployment_name: Option<&str>,
) -> Result<(Model, StreamOptions)> {
    let mut resolved_model = model.clone();
    resolved_model.base_url =
        resolve_azure_base_url_parts(model, base.env.as_ref(), azure_base_url, azure_resource_name)?;
    let deployment_name = resolve_deployment_name_parts(model, base.env.as_ref(), azure_deployment_name);
    let mut next_base = base.clone();
    next_base.on_payload = wrap_payload_with_deployment(&model.id, deployment_name, base.on_payload.clone());
    Ok((resolved_model, next_base))
}

/// Resolve the Azure endpoint and deployment before dispatch, inside `lazy_stream` so an
/// unconfigured endpoint errors on the stream instead of throwing out of `stream()`.
fn azure_streams(streams: ProviderStreams) -> ProviderStreams {
    let stream_fn = Arc::clone(&streams.stream);
    let stream_simple_fn = Arc::clone(&streams.stream_simple);

    ProviderStreams {
        stream: Arc::new(move |model, context, options| {
            let model_owned = model.clone();
            let context_owned = context.clone();
            let stream_fn = Arc::clone(&stream_fn);
            lazy_stream(model, async move {
                let opts = options.unwrap_or_default();
                let get_str = |k: &str| opts.extra.get(k).and_then(Value::as_str).map(str::to_owned);
                let azure_base_url = get_str("azureBaseUrl");
                let azure_resource_name = get_str("azureResourceName");
                let azure_deployment_name = get_str("azureDeploymentName");
                let (resolved_model, next_base) = resolve_stream_options(
                    &model_owned,
                    &opts.base,
                    azure_base_url.as_deref(),
                    azure_resource_name.as_deref(),
                    azure_deployment_name.as_deref(),
                )?;
                let next_opts = ApiStreamOptions {
                    base: next_base,
                    extra: opts.extra,
                    extension: opts.extension,
                };
                Ok(stream_fn(&resolved_model, &context_owned, Some(next_opts)))
            })
        }),
        stream_simple: Arc::new(move |model, context, options| {
            let model_owned = model.clone();
            let context_owned = context.clone();
            let stream_simple_fn = Arc::clone(&stream_simple_fn);
            lazy_stream(model, async move {
                let opts = options.unwrap_or_default();
                let (resolved_model, next_base) = resolve_stream_options(&model_owned, &opts.base, None, None, None)?;
                let next_opts = SimpleStreamOptions {
                    base: next_base,
                    tool_choice: opts.tool_choice,
                    reasoning: opts.reasoning,
                    deferred: opts.deferred,
                    thinking_budgets: opts.thinking_budgets,
                };
                Ok(stream_simple_fn(&resolved_model, &context_owned, Some(next_opts)))
            })
        }),
        fetch_deferred: None,
        cancel_deferred: None,
    }
}

pub fn azure_provider() -> Result<Arc<dyn Provider + Send + Sync>> {
    let mut apis = IndexMap::new();
    apis.insert("azure-openai-responses".to_owned(), azure_openai_responses_api());
    apis.insert("openai-completions".to_owned(), azure_streams(openai_completions_api()));

    create_provider(CreateProviderOptions {
        id: "azure".to_owned(),
        name: Some("Azure".to_owned()),
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth("Azure OpenAI API key", &["AZURE_OPENAI_API_KEY"])),
            oauth: None,
        },
        models: azure_models().values().cloned().map(AnyModel::Chat).collect(),
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::ByApi(apis)),
        images: None,
        classifiers: None,
    })
}
