//! Port of packages/coding-agent/src/extensions/llama/provider.ts

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;

use crate::extensions::llama::client::{
    ListOptions, LlamaClient, LlamaModelInfo, LlamaModelStatus, LlamaServerProps, PropsOptions, llama_inference_url,
    normalize_llama_server_url,
};
use pi_ai::api::llama_cpp_classify_lazy::llama_cpp_classify_api;
use pi_ai::api::openai_completions_lazy::openai_completions_api;
use pi_ai::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AuthCheck, AuthContext, AuthPrompt, AuthResult, AuthType,
    Credential, ModelAuth, ProviderAuth, ProviderAuthInteraction,
};
use pi_ai::models::{ModelsPublication, Provider, RefreshModelsContext};
use pi_ai::types::{
    AnyModel, ApiStreamOptions, ClassifierContext, ClassifierModel, ClassifierOptions, ClassifierResult, KnownApi,
    KnownClassifierApi, MaxTokensField, Model, ModelCompat, ModelCost, ModelModality, ModelThinkingLevel,
    OpenAICompletionsCompat, ProviderClassifier, ProviderStreams, SimpleStreamOptions, ThinkingFormat,
    ThinkingLevelMap, TranscriptContext,
};
use pi_ai::utils::event_stream::AssistantMessageEventStream;
use pi_js::abort::AbortSignal;
use pi_js::{BoxFuture, Result};

pub const LLAMA_PROVIDER_ID: &str = "llama.cpp";
pub const DEFAULT_LLAMA_SERVER_URL: &str = "http://127.0.0.1:9931";
const DEFAULT_INFERENCE_URL: &str = "http://127.0.0.1:9931/v1";

fn credential_server_url(credential: Option<&ApiKeyCredential>) -> Option<String> {
    let val = credential?.env.as_ref()?.get("LLAMA_BASE_URL")?;
    let trimmed = pi_js::str16::trim(val);
    if trimmed.is_empty() {
        None
    } else {
        normalize_llama_server_url(trimmed).ok()
    }
}

async fn resolve_server_url(ctx: &dyn AuthContext, credential: Option<&ApiKeyCredential>) -> Result<Option<String>> {
    if let Some(val) = credential
        .and_then(|c| c.env.as_ref())
        .and_then(|e| e.get("LLAMA_BASE_URL"))
    {
        let trimmed = pi_js::str16::trim(val);
        if !trimmed.is_empty() {
            return Ok(Some(normalize_llama_server_url(trimmed)?));
        }
    }
    if let Some(from_env) = ctx.env("LLAMA_BASE_URL").await {
        let trimmed = pi_js::str16::trim(&from_env);
        if !trimmed.is_empty() {
            return Ok(Some(normalize_llama_server_url(trimmed)?));
        }
    }
    Ok(None)
}

fn model_is_selectable(model: &LlamaModelInfo, router_autoload: bool) -> bool {
    if model.status.value == LlamaModelStatus::Loaded {
        return true;
    }
    if model.status.value == LlamaModelStatus::Sleeping {
        return true;
    }
    router_autoload
        && model.status.value == LlamaModelStatus::Unloaded
        && model.status.failed != Some(true)
        && model.source.as_deref() == Some("preset")
}

async fn router_autoload_enabled(client: &LlamaClient, catalog: &[LlamaModelInfo], signal: &AbortSignal) -> bool {
    if !catalog
        .iter()
        .any(|m| m.status.value == LlamaModelStatus::Unloaded && m.source.as_deref() == Some("preset"))
    {
        return false;
    }
    client
        .props(PropsOptions {
            model: None,
            signal: Some(signal.clone()),
        })
        .await
        .map(|p| p.models_autoload == Some(true))
        .unwrap_or(false)
}

fn configured_context_window(model: &LlamaModelInfo) -> Option<i64> {
    let args = model.status.args.as_deref()?;
    for idx in 0..args.len().saturating_sub(1) {
        let flag = args[idx].as_str();
        if flag != "--ctx-size" && flag != "-c" && flag != "-ctx" {
            continue;
        }
        if let Ok(ctx_val) = args[idx + 1].parse::<i64>()
            && ctx_val > 0
        {
            return Some(ctx_val);
        }
    }
    None
}

fn context_window_of(model: &LlamaModelInfo, cached_context_window: Option<i64>) -> i64 {
    if let Some(n_ctx) = model.meta.as_ref().and_then(|m| m.n_ctx)
        && n_ctx > 0
    {
        return n_ctx;
    }
    if let Some(configured) = configured_context_window(model) {
        return configured;
    }
    if let Some(cached) = cached_context_window
        && cached > 0
    {
        return cached;
    }
    if let Some(n_ctx_train) = model.meta.as_ref().and_then(|m| m.n_ctx_train)
        && n_ctx_train > 0
    {
        return n_ctx_train;
    }
    128_000
}

fn zero_cost() -> ModelCost {
    ModelCost {
        input: 0.0,
        output: 0.0,
        cache_read: 0.0,
        cache_write: 0.0,
        tiers: None,
    }
}

fn to_pi_classifier_model(
    model: &LlamaModelInfo,
    server_url: &str,
    cached_context_window: Option<i64>,
) -> ClassifierModel {
    ClassifierModel {
        id: model.id.clone(),
        name: model.id.clone(),
        api: KnownClassifierApi::LlamaCppClassify.as_str().to_string(),
        provider: LLAMA_PROVIDER_ID.to_string(),
        base_url: server_url.to_string(),
        input: vec![ModelModality::Text],
        input_limits: None,
        context_window: context_window_of(model, cached_context_window),
        cost: zero_cost(),
        headers: None,
    }
}

fn to_pi_model(
    model: &LlamaModelInfo,
    server_url: &str,
    props: Option<&LlamaServerProps>,
    cached_context_window: Option<i64>,
) -> Result<Model> {
    let context_window = context_window_of(model, cached_context_window);
    let reasoning = props
        .and_then(|p| p.chat_template.as_deref())
        .is_some_and(|tpl| tpl.contains("enable_thinking"));
    let thinking_level_map = if reasoning {
        let mut map = ThinkingLevelMap::new();
        map.insert(ModelThinkingLevel::Off, Some("off".to_string()));
        map.insert(ModelThinkingLevel::Minimal, None);
        map.insert(ModelThinkingLevel::Low, None);
        map.insert(ModelThinkingLevel::Medium, Some("medium".to_string()));
        map.insert(ModelThinkingLevel::High, None);
        map.insert(ModelThinkingLevel::XHigh, None);
        Some(map)
    } else {
        None
    };
    let supports_image = model
        .architecture
        .as_ref()
        .and_then(|a| a.input_modalities.as_ref())
        .is_some_and(|mods| mods.iter().any(|m| m == "image"));
    let input = if supports_image {
        vec![ModelModality::Text, ModelModality::Image]
    } else {
        vec![ModelModality::Text]
    };

    Ok(Model {
        r#type: None,
        id: model.id.clone(),
        name: model.id.clone(),
        api: KnownApi::OpenAICompletions.as_str().to_string(),
        provider: LLAMA_PROVIDER_ID.to_string(),
        base_url: llama_inference_url(server_url)?,
        reasoning,
        thinking_level_map,
        prompt_cache: None,
        input,
        input_limits: None,
        cost: zero_cost(),
        context_window,
        max_tokens: context_window,
        headers: None,
        sampling_params: None,
        compat: Some(ModelCompat::from(OpenAICompletionsCompat {
            supports_store: Some(false),
            supports_developer_role: Some(false),
            supports_reasoning_effort: Some(false),
            supports_usage_in_streaming: Some(true),
            supports_strict_mode: Some(false),
            max_tokens_field: Some(MaxTokensField::MaxTokens),
            thinking_format: if reasoning {
                Some(ThinkingFormat::QwenChatTemplate)
            } else {
                None
            },
            ..OpenAICompletionsCompat::default()
        })),
    })
}

struct LlamaApiKeyAuth;

#[async_trait]
impl ApiKeyAuth for LlamaApiKeyAuth {
    fn name(&self) -> &str {
        "llama.cpp server"
    }

    fn supports_login(&self) -> bool {
        true
    }

    async fn login(&self, interaction: ProviderAuthInteraction) -> Result<ApiKeyCredential> {
        let env_url = pi_js::env::var("LLAMA_BASE_URL").unwrap_or_default();
        let placeholder = if env_url.is_empty() {
            DEFAULT_LLAMA_SERVER_URL.to_string()
        } else {
            env_url.clone()
        };
        let entered_url = interaction
            .prompt(AuthPrompt::text("llama.cpp server URL", Some(placeholder)))
            .await?;
        let trimmed_entered = pi_js::str16::trim(&entered_url);
        let raw_url = if !trimmed_entered.is_empty() {
            trimmed_entered.to_string()
        } else if !pi_js::str16::trim(&env_url).is_empty() {
            env_url
        } else {
            DEFAULT_LLAMA_SERVER_URL.to_string()
        };
        let server_url = normalize_llama_server_url(&raw_url)?;
        let raw_key = interaction
            .prompt(AuthPrompt::secret("API key (optional)", None))
            .await?;
        let api_key = pi_js::str16::trim(&raw_key).to_string();
        let api_key_opt = if api_key.is_empty() { None } else { Some(api_key) };

        LlamaClient::new(&server_url, api_key_opt.as_deref())?
            .list(ListOptions {
                reload: false,
                signal: Some(interaction.signal.clone()),
            })
            .await?;

        let mut env = IndexMap::new();
        env.insert("LLAMA_BASE_URL".to_string(), server_url);
        Ok(ApiKeyCredential {
            key: api_key_opt,
            env: Some(env),
        })
    }

    fn supports_check(&self) -> bool {
        true
    }

    async fn check(&self, input: ApiKeyAuthInput) -> Result<Option<AuthCheck>> {
        let cred = input.credential.as_ref();
        let server_url = resolve_server_url(input.ctx.as_ref(), cred).await?;
        Ok(server_url.map(|_| AuthCheck {
            source: Some(if cred.is_some() {
                "stored credential".to_string()
            } else {
                "LLAMA_BASE_URL".to_string()
            }),
            auth_type: AuthType::ApiKey,
        }))
    }

    async fn resolve(&self, input: ApiKeyAuthInput) -> Result<Option<AuthResult>> {
        let cred = input.credential.as_ref();
        let Some(server_url) = resolve_server_url(input.ctx.as_ref(), cred).await? else {
            return Ok(None);
        };
        let api_key = match cred.and_then(|c| c.key.clone()) {
            Some(k) => k,
            None => input
                .ctx
                .env("LLAMA_API_KEY")
                .await
                .unwrap_or_else(|| "local".to_string()),
        };
        let mut env = cred.and_then(|c| c.env.clone()).unwrap_or_default();
        env.insert("LLAMA_BASE_URL".to_string(), server_url.clone());
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: Some(api_key),
                headers: None,
                base_url: Some(llama_inference_url(&server_url)?),
            },
            env: Some(env),
            source: Some(if cred.is_some() {
                "stored credential".to_string()
            } else {
                "LLAMA_BASE_URL".to_string()
            }),
        }))
    }
}

#[derive(Clone, Default)]
struct CatalogState {
    models: Vec<Model>,
    classifiers: Vec<ClassifierModel>,
}

struct LlamaProvider {
    auth: ProviderAuth,
    catalog: Arc<Mutex<CatalogState>>,
    streams: ProviderStreams,
    classifier: ProviderClassifier,
}

impl Provider for LlamaProvider {
    fn id(&self) -> &str {
        LLAMA_PROVIDER_ID
    }

    fn name(&self) -> &str {
        "llama.cpp"
    }

    fn base_url(&self) -> Option<&str> {
        Some(DEFAULT_INFERENCE_URL)
    }

    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<Model> {
        self.catalog.lock().unwrap_or_else(|e| e.into_inner()).models.clone()
    }

    fn get_all_models(&self) -> Option<Vec<AnyModel>> {
        let guard = self.catalog.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = Vec::with_capacity(guard.models.len() + guard.classifiers.len());
        all.extend(guard.models.iter().cloned().map(AnyModel::Chat));
        all.extend(guard.classifiers.iter().cloned().map(AnyModel::Classifier));
        Some(all)
    }

    fn refresh_models(&self, context: RefreshModelsContext) -> Option<BoxFuture<Result<()>>> {
        let catalog = Arc::clone(&self.catalog);
        Some(Box::pin(async move {
            let mut cached_context_windows: IndexMap<String, i64> = IndexMap::new();
            if let Some(ref stored) = context.stored {
                let mut restored = Vec::new();
                let mut restored_classifiers = Vec::new();
                for entry in &stored.models {
                    match entry {
                        AnyModel::Chat(m)
                            if m.provider == LLAMA_PROVIDER_ID && m.api == KnownApi::OpenAICompletions.as_str() =>
                        {
                            cached_context_windows.insert(m.id.clone(), m.context_window);
                            restored.push(m.clone());
                        }
                        AnyModel::Classifier(c)
                            if c.provider == LLAMA_PROVIDER_ID
                                && c.api == KnownClassifierApi::LlamaCppClassify.as_str() =>
                        {
                            cached_context_windows.insert(c.id.clone(), c.context_window);
                            restored_classifiers.push(c.clone());
                        }
                        _ => {}
                    }
                }
                let catalog_pub = Arc::clone(&catalog);
                let published = (context.publish)(ModelsPublication {
                    persist: None,
                    update: Some(Arc::new(move || {
                        let mut guard = catalog_pub.lock().unwrap_or_else(|e| e.into_inner());
                        guard.models = restored.clone();
                        guard.classifiers = restored_classifiers.clone();
                    })),
                })
                .await?;
                if !published {
                    return Ok(());
                }
            }

            if !context.allow_network || context.signal.aborted() {
                return Ok(());
            }
            let Some(Credential::ApiKey(api_cred)) = context.credential.as_ref() else {
                return Ok(());
            };
            let Some(server_url) = credential_server_url(Some(api_cred)) else {
                return Ok(());
            };
            let client = LlamaClient::new(&server_url, api_cred.key.as_deref())?;
            let listed = client
                .list(ListOptions {
                    reload: false,
                    signal: Some(context.signal.clone()),
                })
                .await?;
            if context.signal.aborted() {
                return Ok(());
            }
            let router_autoload = router_autoload_enabled(&client, &listed, &context.signal).await;
            if context.signal.aborted() {
                return Ok(());
            }
            let selectable: Vec<&LlamaModelInfo> = listed
                .iter()
                .filter(|m| model_is_selectable(m, router_autoload))
                .collect();

            let mut refreshed = Vec::with_capacity(selectable.len());
            for model in &selectable {
                let cached = cached_context_windows.get(&model.id).copied();
                if model.status.value != LlamaModelStatus::Loaded {
                    refreshed.push(to_pi_model(model, &server_url, None, cached)?);
                } else {
                    let props = client
                        .props(PropsOptions {
                            model: Some(model.id.clone()),
                            signal: Some(context.signal.clone()),
                        })
                        .await?;
                    refreshed.push(to_pi_model(model, &server_url, Some(&props), cached)?);
                }
            }
            let refreshed_classifiers: Vec<ClassifierModel> = selectable
                .iter()
                .map(|m| to_pi_classifier_model(m, &server_url, cached_context_windows.get(&m.id).copied()))
                .collect();

            if context.signal.aborted() {
                return Ok(());
            }

            let mut persisted_models = Vec::with_capacity(refreshed.len() + refreshed_classifiers.len());
            persisted_models.extend(refreshed.iter().cloned().map(AnyModel::Chat));
            persisted_models.extend(refreshed_classifiers.iter().cloned().map(AnyModel::Classifier));

            let catalog_pub = Arc::clone(&catalog);
            (context.publish)(ModelsPublication {
                persist: Some(Some(pi_ai::models_store::ModelsStoreEntry {
                    models: persisted_models,
                    last_modified: None,
                    checked_at: Some(pi_js::time::now_ms()),
                    etag: None,
                })),
                update: Some(Arc::new(move || {
                    let mut guard = catalog_pub.lock().unwrap_or_else(|e| e.into_inner());
                    guard.models = refreshed.clone();
                    guard.classifiers = refreshed_classifiers.clone();
                })),
            })
            .await?;
            Ok(())
        }))
    }

    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<ApiStreamOptions>,
    ) -> AssistantMessageEventStream {
        self.streams.stream(model, context, options)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        self.streams.stream_simple(model, context, options)
    }

    fn classify(
        &self,
        model: &ClassifierModel,
        context: &ClassifierContext,
        options: Option<ClassifierOptions>,
    ) -> Option<BoxFuture<ClassifierResult>> {
        Some(self.classifier.classify(model, context, options))
    }
}

#[derive(Clone)]
pub struct LlamaProviderController {
    pub provider: Arc<dyn Provider>,
    catalog: Arc<Mutex<CatalogState>>,
}

#[derive(Default)]
pub struct SetCatalogOptions {
    pub router_autoload: bool,
}

impl LlamaProviderController {
    pub fn set_catalog(&self, catalog: &[LlamaModelInfo], server_url: &str, options: SetCatalogOptions) -> Result<()> {
        let mut models = Vec::new();
        let mut classifiers = Vec::new();
        for model in catalog {
            if model_is_selectable(model, options.router_autoload) {
                models.push(to_pi_model(model, server_url, None, None)?);
                classifiers.push(to_pi_classifier_model(model, server_url, None));
            }
        }
        let mut guard = self.catalog.lock().unwrap_or_else(|e| e.into_inner());
        guard.models = models;
        guard.classifiers = classifiers;
        Ok(())
    }
}

pub fn create_llama_provider() -> LlamaProviderController {
    let catalog = Arc::new(Mutex::new(CatalogState::default()));
    let provider: Arc<dyn Provider> = Arc::new(LlamaProvider {
        auth: ProviderAuth {
            api_key: Some(Arc::new(LlamaApiKeyAuth)),
            oauth: None,
        },
        catalog: Arc::clone(&catalog),
        streams: openai_completions_api(),
        classifier: llama_cpp_classify_api(),
    });
    LlamaProviderController { provider, catalog }
}
