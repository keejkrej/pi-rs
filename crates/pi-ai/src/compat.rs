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

// WIRE(@mcp-w2): re-export bedrock_converse_stream_api, google_generative_ai_api,
// google_vertex_api, mistral_conversations_api when @mcp-w2 lands.

use crate::api::lazy::lazy_stream;
use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::models::{
    CreateProviderApi, CreateProviderOptions, ModelsApiStreamOptions, ModelsSimpleStreamOptions, MutableModels,
    Provider, create_models, create_provider,
};
use crate::models_generated::{
    BUILTIN_MODEL_PROVIDER_IDS, provider_chat_models, provider_classifier_models, provider_image_models,
};
use crate::providers::anthropic::anthropic_provider;
use crate::providers::azure_openai_responses::azure_openai_responses_provider;
use crate::providers::faux::{FauxProviderRegistration, RegisterFauxProviderOptions, create_faux_core};
use crate::providers::openai::openai_provider;
use crate::providers::openai_codex::openai_codex_provider;
use crate::types::{
    AnyModel, ApiStreamOptions, AssistantMessage, Context, Model, ProviderClassifier, ProviderImages, ProviderStreams,
    SimpleStreamOptions, StreamOptions, TranscriptContext,
};
use crate::utils::event_stream::AssistantMessageEventStream;
use crate::utils::model_operations::{classifier_error_result_message, image_error_result};
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
        ("pi-messages", pi_messages_api()),
        // WIRE(@mcp-w2): add google-generative-ai, google-vertex, mistral-conversations, bedrock-converse-stream
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

fn build_fallback_api_map() -> IndexMap<String, ProviderStreams> {
    let mut map = IndexMap::new();
    for (api, streams) in builtin_api_pairs() {
        map.insert(api.to_string(), streams);
    }
    map
}

// WIRE(@mcp-w2): delegate compat_models catalog construction to crate::providers::all::builtin_models().
static COMPAT_MODELS: LazyLock<MutableModels> = LazyLock::new(|| {
    ensure_builtin_apis_registered();
    let models = create_models(None);
    let primary: [Option<Arc<dyn Provider + Send + Sync>>; 4] = [
        anthropic_provider().ok(),
        azure_openai_responses_provider().ok(),
        openai_provider().ok(),
        openai_codex_provider().ok(),
    ];
    let mut registered_ids = indexmap::IndexSet::new();
    for prov in primary.into_iter().flatten() {
        registered_ids.insert(prov.id().to_string());
        models.set_provider(prov);
    }

    let api_map = build_fallback_api_map();
    for &provider_id in BUILTIN_MODEL_PROVIDER_IDS {
        if registered_ids.contains(provider_id) {
            continue;
        }
        let env_keys = find_env_keys(provider_id, None).unwrap_or_default();
        let api_key_auth = env_api_key_auth(provider_id, &env_keys);
        let chat_catalog = provider_chat_models(provider_id);
        let image_catalog = provider_image_models(provider_id);
        let classifier_catalog = provider_classifier_models(provider_id);
        let mut any_models: Vec<AnyModel> = Vec::new();
        let mut by_api = IndexMap::new();
        if let Some(chats) = chat_catalog {
            for m in chats.values() {
                any_models.push(AnyModel::Chat(m.clone()));
                if !by_api.contains_key(&m.api) {
                    if let Some(streams) = api_map.get(&m.api) {
                        by_api.insert(m.api.clone(), streams.clone());
                    } else {
                        let missing_api = m.api.clone();
                        let missing_api_simple = missing_api.clone();
                        by_api.insert(
                            missing_api.clone(),
                            ProviderStreams {
                                stream: Arc::new(move |model, _ctx, _opts| {
                                    let api_name = missing_api.clone();
                                    lazy_stream(model, async move {
                                        Err(Error::msg(format!("API {api_name} requires @mcp-w2 integration")))
                                    })
                                }),
                                stream_simple: Arc::new(move |model, _ctx, _opts| {
                                    let api_name = missing_api_simple.clone();
                                    lazy_stream(model, async move {
                                        Err(Error::msg(format!("API {api_name} requires @mcp-w2 integration")))
                                    })
                                }),
                                fetch_deferred: None,
                                cancel_deferred: None,
                            },
                        );
                    }
                }
            }
        }
        let mut by_image_api = IndexMap::new();
        if let Some(imgs) = image_catalog {
            for m in imgs.values() {
                any_models.push(AnyModel::Image(m.clone()));
                if !by_image_api.contains_key(&m.api) {
                    by_image_api.insert(
                        m.api.clone(),
                        ProviderImages {
                            generate_images: Arc::new(|model, ctx, opts| {
                                let m = model.clone();
                                let c = ctx.clone();
                                Box::pin(async move {
                                    let aborted = opts
                                        .as_ref()
                                        .and_then(|o| o.base.signal.as_ref())
                                        .is_some_and(|s| s.aborted());
                                    crate::images::generate_images(&m, &c, opts.map(Into::into))
                                        .await
                                        .unwrap_or_else(|e| image_error_result(&m, &e, aborted))
                                })
                            }),
                        },
                    );
                }
            }
        }
        let mut by_classifier_api = IndexMap::new();
        if let Some(clfs) = classifier_catalog {
            for m in clfs.values() {
                any_models.push(AnyModel::Classifier(m.clone()));
                if !by_classifier_api.contains_key(&m.api) {
                    let api_name = m.api.clone();
                    by_classifier_api.insert(
                        api_name.clone(),
                        ProviderClassifier {
                            classify: Arc::new(move |model, _ctx, _opts| {
                                let msg = format!("API {api_name} requires @mcp-w2 integration");
                                let m = model.clone();
                                Box::pin(async move { classifier_error_result_message(&m, msg, false) })
                            }),
                        },
                    );
                }
            }
        }
        if any_models.is_empty() {
            continue;
        }
        let api_opt = if by_api.is_empty() {
            None
        } else {
            Some(CreateProviderApi::ByApi(by_api))
        };
        let images_opt = if by_image_api.is_empty() {
            None
        } else {
            Some(by_image_api)
        };
        let classifiers_opt = if by_classifier_api.is_empty() {
            None
        } else {
            Some(by_classifier_api)
        };
        if let Ok(prov) = create_provider(CreateProviderOptions {
            id: provider_id.to_owned(),
            name: None,
            base_url: None,
            headers: None,
            auth: ProviderAuth {
                api_key: Some(api_key_auth),
                oauth: None,
            },
            models: any_models,
            fetch_models: None,
            filter_models: None,
            filter_all_models: None,
            api: api_opt,
            images: images_opt,
            classifiers: classifiers_opt,
        }) {
            models.set_provider(prov);
        }
    }
    models
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
