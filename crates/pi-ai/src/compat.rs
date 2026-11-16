//! Port of packages/ai/src/compat.ts

// @generated-mods begin (scaffold-owned, do not edit)
pub mod extension_oauth_types;
// @generated-mods end

use std::sync::{Arc, LazyLock, Mutex};

use indexmap::IndexMap;
use pi_js::error::Error;
use serde_json::{Map, Value};

pub use crate::api::anthropic_messages_lazy::anthropic_messages_api;
pub use crate::api::azure_openai_responses_lazy::azure_openai_responses_api;
pub use crate::api::bedrock_converse_stream_lazy::bedrock_converse_stream_api;
pub use crate::api::google_generative_ai_lazy::google_generative_ai_api;
pub use crate::api::google_vertex_lazy::google_vertex_api;
pub use crate::api::mistral_conversations_lazy::mistral_conversations_api;
pub use crate::api::openai_codex_responses_lazy::openai_codex_responses_api;
pub use crate::api::openai_completions_lazy::openai_completions_api;
pub use crate::api::openai_responses_lazy::openai_responses_api;
pub use crate::api::pi_messages_lazy::pi_messages_api;
pub use crate::env_api_keys::*;
pub use crate::image_models::*;
pub use crate::images::*;
pub use crate::images_api_registry::*;
pub use crate::legacy_api_aliases::*;
pub use crate::models::{
    calculate_cost, clamp_thinking_level, get_model_type, get_supported_thinking_levels, has_api, is_model_type,
    models_are_equal,
};
pub use crate::providers::images::register_builtins::*;
pub use crate::utils::overflow::{is_context_overflow, is_recoverable_length};
pub use crate::utils::text::content_text;
pub use crate::utils::transcript::normalize_context;
pub use crate::utils::validation::{validate_tool_arguments, validate_tool_call};

use crate::api::lazy::lazy_stream;
use crate::models::{ModelsApiStreamOptions, ModelsSimpleStreamOptions, MutableModels, Provider};
use crate::providers::all::builtin_models as build_all_builtin_models;
use crate::providers::faux::{FauxProviderRegistration, RegisterFauxProviderOptions, create_faux_core};
use crate::types::{
    ApiStreamOptions, AssistantMessage, Context, Model, ProviderStreams, SimpleStreamOptions, StreamOptions,
    TranscriptContext,
};
use crate::utils::event_stream::AssistantMessageEventStream;
use crate::utils::uuid::uuidv7;

const AMBIENT_AUTH_MARKER: &str = "<authenticated>";

#[derive(Clone)]
pub struct ApiProvider {
    pub api: String,
    pub stream: crate::types::StreamFunction,
    pub stream_simple: crate::types::SimpleStreamFunction,
}

#[derive(Clone)]
struct RegisteredApiProvider {
    provider: ApiProvider,
    source_id: Option<String>,
}

static API_REGISTRY: LazyLock<Mutex<IndexMap<String, RegisteredApiProvider>>> =
    LazyLock::new(|| Mutex::new(IndexMap::new()));

static BUILTIN_API_INSTANCES: LazyLock<Mutex<IndexMap<String, usize>>> = LazyLock::new(|| Mutex::new(IndexMap::new()));

fn registry_lock() -> std::sync::MutexGuard<'static, IndexMap<String, RegisteredApiProvider>> {
    match API_REGISTRY.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

fn wrap_stream(expected_api: String, inner: crate::types::StreamFunction) -> crate::types::StreamFunction {
    Arc::new(move |model, context, options| {
        if model.api != expected_api {
            let err_api = model.api.clone();
            let exp = expected_api.clone();
            return lazy_stream(model, async move {
                Err(Error::msg(format!("Mismatched api: {err_api} expected {exp}")))
            });
        }
        inner(model, context, options)
    })
}

fn wrap_stream_simple(
    expected_api: String,
    inner: crate::types::SimpleStreamFunction,
) -> crate::types::SimpleStreamFunction {
    Arc::new(move |model, context, options| {
        if model.api != expected_api {
            let err_api = model.api.clone();
            let exp = expected_api.clone();
            return lazy_stream(model, async move {
                Err(Error::msg(format!("Mismatched api: {err_api} expected {exp}")))
            });
        }
        inner(model, context, options)
    })
}

pub fn register_api_provider(provider: ApiProvider, source_id: Option<&str>) {
    let wrapped = ApiProvider {
        api: provider.api.clone(),
        stream: wrap_stream(provider.api.clone(), provider.stream),
        stream_simple: wrap_stream_simple(provider.api.clone(), provider.stream_simple),
    };
    registry_lock().insert(
        provider.api,
        RegisteredApiProvider {
            provider: wrapped,
            source_id: source_id.map(str::to_string),
        },
    );
}

pub fn get_api_provider(api: &str) -> Option<ApiProvider> {
    registry_lock().get(api).map(|e| e.provider.clone())
}

pub fn get_api_providers() -> Vec<ApiProvider> {
    registry_lock().values().map(|e| e.provider.clone()).collect()
}

pub fn unregister_api_providers(source_id: &str) {
    registry_lock().retain(|_, entry| entry.source_id.as_deref() != Some(source_id));
}

pub fn clear_api_providers() {
    registry_lock().clear();
}

pub fn register_faux_provider(options: Option<RegisterFauxProviderOptions>) -> FauxProviderRegistration {
    let core = create_faux_core(options.unwrap_or_default());
    let source_id = format!(
        "faux-provider-{}",
        uuidv7(None).unwrap_or_else(|_| "fallback".to_string())
    );
    let core_stream = core.clone();
    let stream_fn: crate::types::StreamFunction = Arc::new(move |model, context, opts| {
        let simple_opts = opts.map(|o| SimpleStreamOptions {
            base: o.base,
            tool_choice: None,
            reasoning: None,
            deferred: None,
            thinking_budgets: None,
        });
        core_stream.stream(model, context, simple_opts)
    });
    let core_simple = core.clone();
    let stream_simple_fn: crate::types::SimpleStreamFunction =
        Arc::new(move |model, context, opts| core_simple.stream_simple(model, context, opts));

    let api_id = core.api().to_string();
    register_api_provider(
        ApiProvider {
            api: api_id,
            stream: stream_fn,
            stream_simple: stream_simple_fn,
        },
        Some(&source_id),
    );
    let sid_for_drop = source_id.clone();
    FauxProviderRegistration::new(
        core,
        Arc::new(move || {
            unregister_api_providers(&sid_for_drop);
        }),
    )
}

fn builtin_api_pairs() -> Vec<(&'static str, ProviderStreams)> {
    vec![
        ("anthropic-messages", anthropic_messages_api()),
        ("openai-completions", openai_completions_api()),
        ("openai-responses", openai_responses_api()),
        ("openai-codex-responses", openai_codex_responses_api()),
        ("azure-openai-responses", azure_openai_responses_api()),
        ("bedrock-converse-stream", bedrock_converse_stream_api()),
        ("google-generative-ai", google_generative_ai_api()),
        ("google-vertex", google_vertex_api()),
        ("mistral-conversations", mistral_conversations_api()),
        ("pi-messages", pi_messages_api()),
    ]
}

fn stream_ptr(p: &ApiProvider) -> usize {
    Arc::as_ptr(&p.stream) as *const () as usize
}

pub fn register_built_in_api_providers() {
    let mut instances = match BUILTIN_API_INSTANCES.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    for (api, streams) in builtin_api_pairs() {
        if get_api_provider(api).is_none() {
            register_api_provider(
                ApiProvider {
                    api: api.to_string(),
                    stream: streams.stream,
                    stream_simple: streams.stream_simple,
                },
                None,
            );
        }
        if let Some(inst) = get_api_provider(api) {
            instances.insert(api.to_string(), stream_ptr(&inst));
        }
    }
}

pub fn reset_api_providers() {
    clear_api_providers();
    if let Ok(mut g) = BUILTIN_API_INSTANCES.lock() {
        g.clear();
    }
    register_built_in_api_providers();
}

fn ensure_builtin_apis_registered() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        register_built_in_api_providers();
    });
}

static COMPAT_MODELS: LazyLock<MutableModels> = LazyLock::new(|| {
    ensure_builtin_apis_registered();
    build_all_builtin_models(None)
});

pub fn builtin_models() -> &'static MutableModels {
    &COMPAT_MODELS
}

pub fn get_model(provider: &str, id: &str) -> Option<Model> {
    builtin_models().get_model(provider, id)
}

pub fn get_models(provider: Option<&str>) -> Vec<Model> {
    builtin_models().get_models(provider)
}

pub fn get_providers() -> Vec<Arc<dyn Provider + Send + Sync>> {
    builtin_models().get_providers()
}

fn has_explicit_api_key(api_key: Option<&str>) -> bool {
    api_key.is_some_and(|k| !pi_js::str16::trim(k).is_empty())
}

fn inject_env_api_key(model: &Model, mut base: StreamOptions) -> StreamOptions {
    if !has_explicit_api_key(base.api_key.as_deref()) {
        if let Some(key) = get_env_api_key(&model.provider, base.env.as_ref()) {
            if key != AMBIENT_AUTH_MARKER {
                base.api_key = Some(key);
            }
        }
    }
    base
}

fn has_resolved_cloudflare_auth(options: Option<&StreamOptions>) -> bool {
    let Some(opts) = options else {
        return false;
    };
    if has_explicit_api_key(opts.api_key.as_deref()) {
        return true;
    }
    opts.headers.as_ref().is_some_and(|h| {
        h.iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("cf-aig-authorization") && v.is_some())
    })
}

fn get_builtin_provider_for_model(model: &Model) -> Option<Arc<dyn Provider + Send + Sync>> {
    ensure_builtin_apis_registered();
    let current = get_api_provider(&model.api)?;
    let builtin_ptr = BUILTIN_API_INSTANCES
        .lock()
        .ok()
        .and_then(|g| g.get(&model.api).copied())?;
    if stream_ptr(&current) != builtin_ptr {
        return None;
    }
    let provider = builtin_models().get_provider(&model.provider)?;
    if provider.get_models().iter().any(|c| c.api == model.api) {
        Some(provider)
    } else {
        None
    }
}

pub fn stream(model: &Model, context: &Context, options: Option<ApiStreamOptions>) -> AssistantMessageEventStream {
    ensure_builtin_apis_registered();
    if let Some(builtin_prov) = get_builtin_provider_for_model(model) {
        if model.provider.starts_with("cloudflare-") && !has_resolved_cloudflare_auth(options.as_ref().map(|o| &o.base))
        {
            let m_opts = options.map(|o| ModelsApiStreamOptions {
                base: o,
                transform_headers: None,
            });
            return builtin_models().stream(model, context, m_opts);
        }
        let transcript: TranscriptContext = normalize_context(context);
        let mut req_opts = options.unwrap_or_default();
        req_opts.base = inject_env_api_key(model, req_opts.base);
        return builtin_prov.stream(model, &transcript, Some(req_opts));
    }

    let Some(api_provider) = get_api_provider(&model.api) else {
        let missing_api = model.api.clone();
        return lazy_stream(model, async move {
            Err(Error::msg(format!("No API provider registered for api: {missing_api}")))
        });
    };
    let transcript = normalize_context(context);
    let mut req_opts = options.unwrap_or_default();
    req_opts.base = inject_env_api_key(model, req_opts.base);
    (api_provider.stream)(model, &transcript, Some(req_opts))
}

pub async fn complete(model: &Model, context: &Context, options: Option<ApiStreamOptions>) -> AssistantMessage {
    stream(model, context, options).result().await
}

pub fn stream_simple(
    model: &Model,
    context: &Context,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    ensure_builtin_apis_registered();
    if let Some(builtin_prov) = get_builtin_provider_for_model(model) {
        if model.provider.starts_with("cloudflare-") && !has_resolved_cloudflare_auth(options.as_ref().map(|o| &o.base))
        {
            let m_opts = options.map(|o| ModelsSimpleStreamOptions {
                base: o,
                transform_headers: None,
            });
            return builtin_models().stream_simple(model, context, m_opts);
        }
        let transcript = normalize_context(context);
        let mut req_opts = options.unwrap_or_default();
        req_opts.base = inject_env_api_key(model, req_opts.base);
        return builtin_prov.stream_simple(model, &transcript, Some(req_opts));
    }

    let Some(api_provider) = get_api_provider(&model.api) else {
        let missing_api = model.api.clone();
        return lazy_stream(model, async move {
            Err(Error::msg(format!("No API provider registered for api: {missing_api}")))
        });
    };
    let transcript = normalize_context(context);
    let mut req_opts = options.unwrap_or_default();
    req_opts.base = inject_env_api_key(model, req_opts.base);
    (api_provider.stream_simple)(model, &transcript, Some(req_opts))
}

pub async fn complete_simple(
    model: &Model,
    context: &Context,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessage {
    stream_simple(model, context, options).result().await
}

/// Convert JSON helper used by callers migrating from `compat.ts`.
pub fn compat_json_object(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect()
}
