//! Port of packages/coding-agent/src/core/provider-composer.ts

use std::sync::{Arc, Mutex, PoisonError};

use indexmap::{IndexMap, IndexSet};
use pi_ai::api::lazy::lazy_stream_with_options;
use pi_ai::compat::extension_oauth_types::{
    OAuthAuthInfo, OAuthCredentials, OAuthDeviceCodeInfo, OAuthLoginCallbacks, OAuthPrompt, OAuthSelectPrompt,
};
use pi_ai::compat::get_api_provider;
use pi_ai::models::{ModelsPublication, Provider, RefreshModelsAuth, RefreshModelsContext};
use pi_ai::types::{
    AnyModel, ApiStreamOptions, AssistantImages, ClassifierContext, ClassifierModel, ClassifierOptions,
    ClassifierResult, DeferredCancelOptions, DeferredFetchOptions, DeferredHandle, ImageModel, ImagesContext,
    ImagesOptions, Model, ModelCompat, ModelCost, ModelInput, ModelInputLimits, ModelOutput, ModelPromptCache,
    ProviderClassifier, ProviderHeaders, ProviderImages, SamplingParamsByThinkingLevel, SimpleStreamFunction,
    SimpleStreamOptions, SystemOneCompat, ThinkingLevelMap, TranscriptContext,
};
use pi_ai::utils::model_operations::{classifier_error_result, image_error_result};
use pi_ai::{
    ApiKeyAuth, ApiKeyAuthInput, ApiKeyCredential, AssistantMessageEventStream, AuthCheck, AuthContext, AuthEvent,
    AuthPrompt, AuthResult, AuthSelectOption, AuthType, Credential, LoginOptions, ModelAuth, ModelType, OAuthAuth,
    OAuthCredential, ProviderAuth, ProviderAuthInteraction, is_model_type,
};
use pi_js::abort::AbortSignal;
use pi_js::{BoxFuture, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::core::model_config::{ModelConfig, ModelsJsonModel, ModelsJsonModelOverride, ModelsJsonProvider};
pub use crate::core::resolve_config_value::clear_config_value_cache as clear_api_key_cache;
use crate::core::resolve_config_value::{
    get_config_value_env_var_names, is_command_config_value, is_config_value_configured, resolve_config_value_or_throw,
    resolve_headers_or_throw,
};

#[derive(Clone)]
pub struct ExtensionOAuthConfig {
    pub name: String,
    pub is_subscription: Option<bool>,
    pub uses_callback_server: Option<bool>,
    pub login: Arc<dyn Fn(Arc<dyn OAuthLoginCallbacks>) -> BoxFuture<Result<OAuthCredentials>> + Send + Sync>,
    pub refresh_token: Arc<dyn Fn(OAuthCredentials, AbortSignal) -> BoxFuture<Result<OAuthCredentials>> + Send + Sync>,
    pub get_api_key: Arc<dyn Fn(&OAuthCredentials) -> String + Send + Sync>,
    pub modify_models: Option<Arc<dyn Fn(Vec<Model>, &OAuthCredentials) -> Vec<Model> + Send + Sync>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderChatModelConfig {
    pub id: String,
    pub name: String,
    pub api: Option<String>,
    pub base_url: Option<String>,
    pub input: Vec<ModelInput>,
    pub input_limits: Option<ModelInputLimits>,
    pub cost: ModelCost,
    pub headers: Option<IndexMap<String, String>>,
    pub reasoning: bool,
    pub thinking_level_map: Option<ThinkingLevelMap>,
    pub prompt_cache: Option<ModelPromptCache>,
    pub context_window: i64,
    pub max_tokens: i64,
    pub sampling_params: Option<IndexMap<String, Value>>,
    pub sampling_params_by_thinking_level: Option<SamplingParamsByThinkingLevel>,
    pub compat: Option<ModelCompat>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderImageModelConfig {
    pub id: String,
    pub name: String,
    pub api: Option<String>,
    pub base_url: Option<String>,
    pub input: Vec<ModelInput>,
    pub input_limits: Option<ModelInputLimits>,
    pub cost: ModelCost,
    pub headers: Option<IndexMap<String, String>>,
    pub output: Vec<ModelOutput>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderClassifierModelConfig {
    pub id: String,
    pub name: String,
    pub api: Option<String>,
    pub base_url: Option<String>,
    pub input: Vec<ModelInput>,
    pub input_limits: Option<ModelInputLimits>,
    pub cost: ModelCost,
    pub headers: Option<IndexMap<String, String>>,
    pub context_window: i64,
    pub compat: Option<SystemOneCompat>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ProviderModelConfig {
    Chat(ProviderChatModelConfig),
    Image(ProviderImageModelConfig),
    Classifier(ProviderClassifierModelConfig),
}

impl ProviderModelConfig {
    pub fn id(&self) -> &str {
        match self {
            Self::Chat(m) => &m.id,
            Self::Image(m) => &m.id,
            Self::Classifier(m) => &m.id,
        }
    }

    pub fn api(&self) -> Option<&str> {
        match self {
            Self::Chat(m) => m.api.as_deref(),
            Self::Image(m) => m.api.as_deref(),
            Self::Classifier(m) => m.api.as_deref(),
        }
    }

    pub fn base_url(&self) -> Option<&str> {
        match self {
            Self::Chat(m) => m.base_url.as_deref(),
            Self::Image(m) => m.base_url.as_deref(),
            Self::Classifier(m) => m.base_url.as_deref(),
        }
    }

    pub fn headers(&self) -> Option<&IndexMap<String, String>> {
        match self {
            Self::Chat(m) => m.headers.as_ref(),
            Self::Image(m) => m.headers.as_ref(),
            Self::Classifier(m) => m.headers.as_ref(),
        }
    }

    pub fn model_type(&self) -> ModelType {
        match self {
            Self::Chat(_) => ModelType::Chat,
            Self::Image(_) => ModelType::Image,
            Self::Classifier(_) => ModelType::Classifier,
        }
    }
}

pub type ExtensionRefreshModelsFn =
    Arc<dyn Fn(RefreshModelsContext) -> BoxFuture<Result<Vec<ProviderModelConfig>>> + Send + Sync>;

/// Input type for the extension `registerProvider` API.
#[derive(Clone, Default)]
pub struct ProviderConfigInput {
    pub name: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub api: Option<String>,
    pub stream_simple: Option<SimpleStreamFunction>,
    pub images: Option<IndexMap<String, ProviderImages>>,
    pub classifiers: Option<IndexMap<String, ProviderClassifier>>,
    pub headers: Option<IndexMap<String, String>>,
    pub auth_header: Option<bool>,
    pub oauth: Option<ExtensionOAuthConfig>,
    pub models: Option<Vec<ProviderModelConfig>>,
    pub refresh_models: Option<ExtensionRefreshModelsFn>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthStatusSource {
    Stored,
    Runtime,
    Environment,
    Fallback,
    ModelsJsonKey,
    ModelsJsonCommand,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatus {
    pub configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<AuthStatusSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

fn get_all_provider_models(provider: Option<&dyn Provider>) -> Vec<AnyModel> {
    match provider {
        Some(p) => p
            .get_all_models()
            .unwrap_or_else(|| p.get_models().into_iter().map(AnyModel::Chat).collect()),
        None => Vec::new(),
    }
}

fn merge_compat_json(api: &str, base: Option<&ModelCompat>, override_val: Option<&Value>) -> Option<ModelCompat> {
    let Some(override_json) = override_val else {
        return base.cloned();
    };
    let Value::Object(override_map) = override_json else {
        return ModelCompat::deserialize_for_api(api, override_json.clone()).ok();
    };
    let mut base_map = match base.and_then(|b| serde_json::to_value(b).ok()) {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };
    let mut merged = base_map.clone();
    for (k, v) in override_map {
        merged.insert(k.clone(), v.clone());
    }
    for key in [
        "openRouterRouting",
        "vercelGatewayRouting",
        "chatTemplateKwargs",
        "chatTemplateArgs",
    ] {
        let base_value = base_map.get_mut(key).and_then(|v| v.as_object()).cloned();
        let override_value = override_map.get(key).and_then(|v| v.as_object()).cloned();
        if base_value.is_some() || override_value.is_some() {
            let mut nested = base_value.unwrap_or_default();
            if let Some(ovr) = override_value {
                for (nk, nv) in ovr {
                    nested.insert(nk, nv);
                }
            }
            merged.insert(key.to_owned(), Value::Object(nested));
        }
    }
    match ModelCompat::deserialize_for_api(api, Value::Object(merged.clone())) {
        Ok(c) => Some(c),
        Err(_) => Some(ModelCompat::Other(Value::Object(merged))),
    }
}

fn merge_input_limits(
    base: Option<ModelInputLimits>,
    override_limits: Option<&ModelInputLimits>,
) -> Option<ModelInputLimits> {
    let Some(ovr) = override_limits else {
        return base;
    };
    let base_images = base.and_then(|b| b.images);
    let images = match ovr.images {
        Some(ovr_img) => {
            let resize = match ovr_img.resize {
                Some(ovr_r) => {
                    let base_r = base_images.and_then(|bi| bi.resize);
                    Some(pi_ai::ModelImageResizeOptions {
                        max_width: ovr_r.max_width.or_else(|| base_r.and_then(|r| r.max_width)),
                        max_height: ovr_r.max_height.or_else(|| base_r.and_then(|r| r.max_height)),
                        max_bytes: ovr_r.max_bytes.or_else(|| base_r.and_then(|r| r.max_bytes)),
                        jpeg_quality: ovr_r.jpeg_quality.or_else(|| base_r.and_then(|r| r.jpeg_quality)),
                    })
                }
                None => base_images.and_then(|bi| bi.resize),
            };
            Some(pi_ai::ModelImageInputLimits {
                resize,
                max_per_message: ovr_img
                    .max_per_message
                    .or_else(|| base_images.and_then(|b| b.max_per_message)),
                max_per_request: ovr_img
                    .max_per_request
                    .or_else(|| base_images.and_then(|b| b.max_per_request)),
            })
        }
        None => base_images,
    };
    Some(ModelInputLimits {
        max_request_bytes: ovr.max_request_bytes.or_else(|| base.and_then(|b| b.max_request_bytes)),
        images,
    })
}

fn merge_sampling_params_by_thinking_level(
    base: Option<SamplingParamsByThinkingLevel>,
    override_map: Option<&SamplingParamsByThinkingLevel>,
) -> Option<SamplingParamsByThinkingLevel> {
    let Some(ovr) = override_map else {
        return base;
    };
    let mut merged = base.unwrap_or_default();
    for (level, params) in ovr {
        let entry = merged.entry(*level).or_default();
        for (k, v) in params {
            entry.insert(k.clone(), v.clone());
        }
    }
    Some(merged)
}

fn merge_classifier_compat(base: Option<&SystemOneCompat>, override_val: Option<&Value>) -> Option<SystemOneCompat> {
    let Some(ovr) = override_val else {
        return base.cloned();
    };
    let mut merged = match base.and_then(|c| serde_json::to_value(c).ok()) {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };
    if let Value::Object(ovr_map) = ovr {
        for (k, v) in ovr_map {
            merged.insert(k.clone(), v.clone());
        }
    }
    serde_json::from_value(Value::Object(merged))
        .ok()
        .or_else(|| base.cloned())
}

fn apply_model_override(mut model: Model, ovr: &ModelsJsonModelOverride) -> Model {
    if let Some(ref name) = ovr.name {
        model.name = name.clone();
    }
    if let Some(reasoning) = ovr.reasoning {
        model.reasoning = reasoning;
    }
    if let Some(ref map) = ovr.thinking_level_map {
        let mut merged = model.thinking_level_map.unwrap_or_default();
        for (k, v) in map {
            merged.insert(*k, v.clone());
        }
        model.thinking_level_map = Some(merged);
    }
    if let Some(ref input) = ovr.input {
        model.input = input.clone();
    }
    model.input_limits = merge_input_limits(model.input_limits, ovr.input_limits.as_ref());
    if let Some(ref cost_ovr) = ovr.cost {
        model.cost = ModelCost {
            input: cost_ovr.input.unwrap_or(model.cost.input),
            output: cost_ovr.output.unwrap_or(model.cost.output),
            cache_read: cost_ovr.cache_read.unwrap_or(model.cost.cache_read),
            cache_write: cost_ovr.cache_write.unwrap_or(model.cost.cache_write),
            tiers: cost_ovr.tiers.clone().or(model.cost.tiers),
        };
    }
    if let Some(ref pc) = ovr.prompt_cache {
        let base_pc = model.prompt_cache.unwrap_or_default();
        model.prompt_cache = Some(ModelPromptCache {
            short: pc.short.or(base_pc.short),
            long: pc.long.or(base_pc.long),
        });
    }
    if let Some(cw) = ovr.context_window {
        model.context_window = cw as i64;
    }
    if let Some(mt) = ovr.max_tokens {
        model.max_tokens = mt as i64;
    }
    if let Some(ref sp) = ovr.sampling_params {
        let mut merged = model.sampling_params.unwrap_or_default();
        for (k, v) in sp {
            merged.insert(k.clone(), v.clone());
        }
        model.sampling_params = Some(merged);
    }
    model.sampling_params_by_thinking_level = merge_sampling_params_by_thinking_level(
        model.sampling_params_by_thinking_level,
        ovr.sampling_params_by_thinking_level.as_ref(),
    );
    let api = model.api.clone();
    model.compat = merge_compat_json(&api, model.compat.as_ref(), ovr.compat.as_ref());
    model
}

fn model_from_json(
    provider_id: &str,
    definition: &ModelsJsonModel,
    provider_config: &ModelsJsonProvider,
    defaults: Option<&Model>,
) -> Result<Model> {
    let api = definition
        .api
        .as_deref()
        .or(provider_config.api.as_deref())
        .or_else(|| defaults.map(|d| d.api.as_str()))
        .ok_or_else(|| {
            Error::msg(format!(
                "Provider {provider_id}, model {}: no \"api\" specified. Set at provider or model level.",
                definition.id
            ))
        })?
        .to_owned();
    let base_url = definition
        .base_url
        .as_deref()
        .or(provider_config.base_url.as_deref())
        .or_else(|| defaults.map(|d| d.base_url.as_str()))
        .ok_or_else(|| {
            Error::msg(format!(
                "Provider {provider_id}: \"baseUrl\" is required when defining custom models."
            ))
        })?
        .to_owned();

    if let Some(cw) = definition.context_window
        && cw <= 0.0
    {
        return Err(Error::msg(format!(
            "Provider {provider_id}, model {}: invalid contextWindow",
            definition.id
        )));
    }
    if let Some(mt) = definition.max_tokens
        && mt <= 0.0
    {
        return Err(Error::msg(format!(
            "Provider {provider_id}, model {}: invalid maxTokens",
            definition.id
        )));
    }

    let provider_compat = merge_compat_json(&api, None, provider_config.compat.as_ref());
    let compat = merge_compat_json(&api, provider_compat.as_ref(), definition.compat.as_ref());

    Ok(Model {
        r#type: None,
        id: definition.id.clone(),
        name: definition.name.clone().unwrap_or_else(|| definition.id.clone()),
        api,
        provider: provider_id.to_owned(),
        base_url,
        reasoning: definition.reasoning.unwrap_or(false),
        thinking_level_map: definition.thinking_level_map.clone(),
        prompt_cache: definition.prompt_cache,
        input: definition.input.clone().unwrap_or_else(|| vec![ModelInput::Text]),
        input_limits: definition.input_limits,
        cost: definition.cost.clone().unwrap_or_default(),
        context_window: definition.context_window.map(|v| v as i64).unwrap_or(128_000),
        max_tokens: definition.max_tokens.map(|v| v as i64).unwrap_or(16_384),
        headers: None,
        sampling_params: definition.sampling_params.clone(),
        sampling_params_by_thinking_level: definition.sampling_params_by_thinking_level.clone(),
        compat,
    })
}

fn find_model_defaults<'a>(models: &'a [AnyModel], model_id: &str, api: Option<&str>) -> Option<&'a Model> {
    let chat_models: Vec<&Model> = models.iter().filter_map(|m| m.as_chat()).collect();
    chat_models
        .iter()
        .copied()
        .find(|m| m.id == model_id)
        .or_else(|| api.and_then(|a| chat_models.iter().copied().find(|m| m.api == a)))
        .or_else(|| chat_models.iter().copied().find(|m| m.api == "openai-completions"))
        .or_else(|| chat_models.first().copied())
}

fn find_extension_model_defaults<'a>(models: &'a [AnyModel], definition: &ProviderModelConfig) -> Option<&'a AnyModel> {
    let kind = definition.model_type();
    let candidates: Vec<&AnyModel> = models.iter().filter(|m| is_model_type(m, kind)).collect();
    candidates
        .iter()
        .copied()
        .find(|m| m.id() == definition.id())
        .or_else(|| {
            definition
                .api()
                .and_then(|a| candidates.iter().copied().find(|m| m.api() == a))
        })
        .or_else(|| {
            if kind == ModelType::Chat {
                candidates.iter().copied().find(|m| m.api() == "openai-completions")
            } else {
                None
            }
        })
        .or_else(|| candidates.first().copied())
}

fn extension_model_from_definition(
    provider_id: &str,
    models: &[AnyModel],
    config: &ProviderConfigInput,
    definition: &ProviderModelConfig,
) -> Result<AnyModel> {
    let kind = definition.model_type();
    let defaults = find_extension_model_defaults(models, definition);
    let api = definition
        .api()
        .or_else(|| {
            if kind == ModelType::Chat {
                config.api.as_deref()
            } else {
                None
            }
        })
        .or_else(|| defaults.map(|d| d.api()))
        .ok_or_else(|| {
            let suffix = if kind == ModelType::Chat {
                " or provider level"
            } else {
                ""
            };
            Error::msg(format!(
                "Provider {provider_id}, model {}: no \"api\" specified. Set it at model level{suffix}.",
                definition.id()
            ))
        })?
        .to_owned();
    let base_url = definition
        .base_url()
        .or(config.base_url.as_deref())
        .or_else(|| defaults.map(|d| d.base_url()))
        .ok_or_else(|| {
            Error::msg(format!(
                "Provider {provider_id}: \"baseUrl\" is required when defining custom models."
            ))
        })?
        .to_owned();

    match definition {
        ProviderModelConfig::Image(def) => Ok(AnyModel::Image(ImageModel {
            id: def.id.clone(),
            name: def.name.clone(),
            api,
            provider: provider_id.to_owned(),
            base_url,
            input: def.input.clone(),
            input_limits: def.input_limits,
            output: def.output.clone(),
            cost: def.cost.clone(),
            headers: None,
        })),
        ProviderModelConfig::Classifier(def) => Ok(AnyModel::Classifier(ClassifierModel {
            id: def.id.clone(),
            name: def.name.clone(),
            api,
            provider: provider_id.to_owned(),
            base_url,
            input: def.input.clone(),
            input_limits: def.input_limits,
            context_window: def.context_window,
            cost: def.cost.clone(),
            headers: None,
            compat: def.compat.clone(),
        })),
        ProviderModelConfig::Chat(def) => Ok(AnyModel::Chat(Model {
            r#type: None,
            id: def.id.clone(),
            name: def.name.clone(),
            api,
            provider: provider_id.to_owned(),
            base_url,
            reasoning: def.reasoning,
            thinking_level_map: def.thinking_level_map.clone(),
            prompt_cache: def.prompt_cache,
            input: def.input.clone(),
            input_limits: def.input_limits,
            cost: def.cost.clone(),
            context_window: def.context_window,
            max_tokens: def.max_tokens,
            headers: None,
            sampling_params: def.sampling_params.clone(),
            sampling_params_by_thinking_level: def.sampling_params_by_thinking_level.clone(),
            compat: def.compat.clone(),
        })),
    }
}

fn set_any_model_base_url(model: AnyModel, base_url: String) -> AnyModel {
    match model {
        AnyModel::Chat(mut m) => {
            m.base_url = base_url;
            AnyModel::Chat(m)
        }
        AnyModel::Image(mut m) => {
            m.base_url = base_url;
            AnyModel::Image(m)
        }
        AnyModel::Classifier(mut m) => {
            m.base_url = base_url;
            AnyModel::Classifier(m)
        }
        AnyModel::Unknown(mut v) => {
            if let Some(obj) = v.as_object_mut() {
                obj.insert("baseUrl".to_owned(), Value::String(base_url));
            }
            AnyModel::Unknown(v)
        }
    }
}

fn apply_models_json(
    provider_id: &str,
    base_models: &[AnyModel],
    config: Option<&ModelsJsonProvider>,
) -> Result<Vec<AnyModel>> {
    let Some(config) = config else {
        return Ok(base_models.to_vec());
    };
    if config.oauth.is_some() && config.base_url.is_none() {
        return Err(Error::msg(format!(
            "Provider {provider_id}: \"baseUrl\" is required when \"oauth\" is set."
        )));
    }
    let has_overrides = config.model_overrides.as_ref().is_some_and(|m| !m.is_empty());
    let has_models = config.models.as_ref().is_some_and(|m| !m.is_empty());
    if !has_models
        && config.base_url.is_none()
        && config.headers.is_none()
        && config.compat.is_none()
        && !has_overrides
        && config.api_key.is_none()
        && config.oauth.is_none()
        && config.auth_header.is_none()
    {
        return Err(Error::msg(format!(
            "Provider {provider_id}: must specify \"baseUrl\", \"headers\", \"compat\", \"modelOverrides\", or \"models\"."
        )));
    }

    let mut models: Vec<AnyModel> = base_models
        .iter()
        .cloned()
        .map(|model| {
            let base_url = if config.oauth.as_deref() == Some("radius") {
                model.base_url().to_owned()
            } else {
                config.base_url.clone().unwrap_or_else(|| model.base_url().to_owned())
            };
            match model {
                AnyModel::Chat(mut chat) => {
                    chat.base_url = base_url;
                    let api = chat.api.clone();
                    chat.compat = merge_compat_json(&api, chat.compat.as_ref(), config.compat.as_ref());
                    AnyModel::Chat(chat)
                }
                AnyModel::Classifier(mut classifier) => {
                    classifier.base_url = base_url;
                    // Classifier APIs read their own compat fields, such as how a System One service takes images.
                    classifier.compat = merge_classifier_compat(classifier.compat.as_ref(), config.compat.as_ref());
                    AnyModel::Classifier(classifier)
                }
                other => set_any_model_base_url(other, base_url),
            }
        })
        .collect();

    if let Some(defs) = config.models.as_ref() {
        for definition in defs {
            let existing_index = models
                .iter()
                .position(|m| is_model_type(m, ModelType::Chat) && m.id() == definition.id);
            let defaults = find_model_defaults(
                &models,
                &definition.id,
                definition.api.as_deref().or(config.api.as_deref()),
            )
            .cloned();
            let model = AnyModel::Chat(model_from_json(provider_id, definition, config, defaults.as_ref())?);
            if let Some(idx) = existing_index {
                models[idx] = model;
            } else {
                models.push(model);
            }
        }
    }
    Ok(models)
}

fn apply_extension(
    provider_id: &str,
    models: Vec<AnyModel>,
    config: Option<&ProviderConfigInput>,
) -> Result<Vec<AnyModel>> {
    let Some(config) = config else {
        return Ok(models);
    };
    let Some(ext_models) = config.models.as_ref() else {
        if let Some(ref base_url) = config.base_url {
            return Ok(models
                .into_iter()
                .map(|m| set_any_model_base_url(m, base_url.clone()))
                .collect());
        }
        return Ok(models);
    };
    ext_models
        .iter()
        .map(|def| extension_model_from_definition(provider_id, &models, config, def))
        .collect()
}

struct OAuthLoginCallbacksAdapter {
    interaction: ProviderAuthInteraction,
}

#[async_trait::async_trait]
impl OAuthLoginCallbacks for OAuthLoginCallbacksAdapter {
    fn on_auth(&self, info: OAuthAuthInfo) {
        self.interaction.notify(AuthEvent::AuthUrl {
            url: info.url,
            instructions: info.instructions,
        });
    }

    fn on_device_code(&self, info: OAuthDeviceCodeInfo) {
        self.interaction.notify(AuthEvent::DeviceCode {
            user_code: info.user_code,
            verification_uri: info.verification_uri,
            interval_seconds: info.interval_seconds,
            expires_in_seconds: info.expires_in_seconds,
            open_browser_on_confirm: info.open_browser_on_confirm,
            copy_code_on_confirm: info.copy_code_on_confirm,
        });
    }

    async fn on_prompt(&self, prompt: OAuthPrompt) -> Result<String> {
        self.interaction
            .prompt(AuthPrompt::text(prompt.message, prompt.placeholder))
            .await
    }

    fn on_progress(&self, message: &str) {
        self.interaction.notify(AuthEvent::Progress {
            message: message.to_owned(),
        });
    }

    async fn on_manual_code_input(&self) -> Result<String> {
        self.interaction
            .prompt(AuthPrompt::manual_code("Paste the authorization code", None))
            .await
    }

    async fn on_select(&self, prompt: OAuthSelectPrompt) -> Result<Option<String>> {
        let options = prompt
            .options
            .into_iter()
            .map(|o| AuthSelectOption {
                id: o.id,
                label: o.label,
                description: None,
            })
            .collect();
        self.interaction
            .prompt(AuthPrompt::select(prompt.message, options))
            .await
            .map(Some)
    }

    fn signal(&self) -> Option<&AbortSignal> {
        Some(&self.interaction.signal)
    }
}

fn to_oauth_credential(creds: OAuthCredentials) -> OAuthCredential {
    OAuthCredential {
        refresh: creds.refresh,
        access: creds.access,
        expires: creds.expires,
        extra: creds.extra,
    }
}

fn to_oauth_credentials(cred: OAuthCredential) -> OAuthCredentials {
    OAuthCredentials {
        refresh: cred.refresh,
        access: cred.access,
        expires: cred.expires,
        extra: cred.extra,
    }
}

struct AdaptedExtensionOAuth {
    config: ExtensionOAuthConfig,
}

#[async_trait::async_trait]
impl OAuthAuth for AdaptedExtensionOAuth {
    fn name(&self) -> &str {
        &self.config.name
    }

    fn is_subscription(&self) -> bool {
        self.config.is_subscription.unwrap_or(false)
    }

    async fn login(
        &self,
        interaction: ProviderAuthInteraction,
        _options: Option<LoginOptions>,
    ) -> Result<OAuthCredential> {
        let callbacks: Arc<dyn OAuthLoginCallbacks> = Arc::new(OAuthLoginCallbacksAdapter { interaction });
        let creds = (self.config.login)(callbacks).await?;
        Ok(to_oauth_credential(creds))
    }

    async fn refresh(&self, credential: OAuthCredential, signal: AbortSignal) -> Result<OAuthCredential> {
        let creds = (self.config.refresh_token)(to_oauth_credentials(credential), signal).await?;
        Ok(to_oauth_credential(creds))
    }

    async fn to_auth(&self, credential: OAuthCredential) -> Result<ModelAuth> {
        let creds = to_oauth_credentials(credential);
        Ok(ModelAuth {
            api_key: Some((self.config.get_api_key)(&creds)),
            headers: None,
            base_url: None,
        })
    }
}

fn with_configured_auth(
    mut auth: ModelAuth,
    headers: Option<IndexMap<String, String>>,
    auth_header: bool,
) -> Result<ModelAuth> {
    let mut merged: ProviderHeaders = auth.headers.unwrap_or_default();
    if let Some(hdrs) = headers {
        for (k, v) in hdrs {
            merged.insert(k, Some(v));
        }
    }
    if auth_header {
        let key = auth
            .api_key
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::msg("authHeader requires a resolved API key"))?;
        merged.insert("Authorization".to_owned(), Some(format!("Bearer {key}")));
    }
    auth.headers = if merged.is_empty() { None } else { Some(merged) };
    Ok(auth)
}

fn configured_api_key<'a>(
    config: Option<&'a ModelsJsonProvider>,
    extension: Option<&'a ProviderConfigInput>,
) -> Option<&'a str> {
    extension
        .and_then(|e| e.api_key.as_deref())
        .or_else(|| config.and_then(|c| c.api_key.as_deref()))
}

fn configured_headers(
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ProviderConfigInput>,
) -> Option<IndexMap<String, String>> {
    let cfg_h = config.and_then(|c| c.headers.as_ref());
    let ext_h = extension.and_then(|e| e.headers.as_ref());
    if cfg_h.is_none() && ext_h.is_none() {
        return None;
    }
    let mut out = IndexMap::new();
    if let Some(h) = cfg_h {
        for (k, v) in h {
            out.insert(k.clone(), v.clone());
        }
    }
    if let Some(h) = ext_h {
        for (k, v) in h {
            out.insert(k.clone(), v.clone());
        }
    }
    Some(out)
}

async fn config_context_env(
    values: &[String],
    ctx: &dyn AuthContext,
    explicit: Option<&IndexMap<String, String>>,
) -> Option<IndexMap<String, String>> {
    let mut env = explicit.cloned().unwrap_or_default();
    let mut names = IndexSet::new();
    for val in values {
        for name in get_config_value_env_var_names(val) {
            names.insert(name);
        }
    }
    for name in names {
        if env.contains_key(&name) {
            continue;
        }
        if let Some(value) = ctx.env(&name).await {
            env.insert(name, value);
        }
    }
    if env.is_empty() { None } else { Some(env) }
}

struct ComposedApiKeyAuth {
    provider_id: String,
    name: String,
    inherited: Option<Arc<dyn ApiKeyAuth>>,
    raw_key: Option<String>,
    raw_headers: Option<IndexMap<String, String>>,
    auth_header: bool,
}

#[async_trait::async_trait]
impl ApiKeyAuth for ComposedApiKeyAuth {
    fn name(&self) -> &str {
        &self.name
    }

    fn supports_login(&self) -> bool {
        match self.inherited.as_ref() {
            Some(inh) => inh.supports_login(),
            None => true,
        }
    }

    async fn login(&self, interaction: ProviderAuthInteraction) -> Result<ApiKeyCredential> {
        if let Some(ref inh) = self.inherited
            && inh.supports_login()
        {
            return inh.login(interaction).await;
        }
        interaction.signal.throw_if_aborted()?;
        let key = interaction.prompt(AuthPrompt::secret("Enter API key", None)).await?;
        interaction.signal.throw_if_aborted()?;
        Ok(ApiKeyCredential {
            key: Some(key),
            env: None,
        })
    }

    fn supports_check(&self) -> bool {
        true
    }

    async fn check(&self, input: ApiKeyAuthInput) -> Result<Option<AuthCheck>> {
        if let Some(ref cred) = input.credential {
            if let Some(ref inh) = self.inherited {
                if inh.supports_check() {
                    return inh.check(input).await;
                }
            }
            if cred.key.as_ref().is_some_and(|k| !k.is_empty()) {
                return Ok(Some(AuthCheck {
                    source: Some("stored credential".to_owned()),
                    auth_type: AuthType::ApiKey,
                }));
            }
            if let Some(ref inh) = self.inherited {
                let resolved = inh.resolve(input).await?;
                return Ok(resolved.map(|r| AuthCheck {
                    source: r.source,
                    auth_type: AuthType::ApiKey,
                }));
            }
            return Ok(None);
        }

        if let Some(ref raw_key) = self.raw_key {
            if is_command_config_value(raw_key) {
                return Ok(Some(AuthCheck {
                    source: Some("configured API key".to_owned()),
                    auth_type: AuthType::ApiKey,
                }));
            }
            for name in get_config_value_env_var_names(raw_key) {
                if input.ctx.env(&name).await.is_none() {
                    return Ok(None);
                }
            }
            return Ok(Some(AuthCheck {
                source: Some("configured API key".to_owned()),
                auth_type: AuthType::ApiKey,
            }));
        }

        if let Some(ref inh) = self.inherited {
            if inh.supports_check() {
                return inh.check(input).await;
            }
            let resolved = inh.resolve(input).await?;
            return Ok(resolved.map(|r| AuthCheck {
                source: r.source,
                auth_type: AuthType::ApiKey,
            }));
        }
        Ok(None)
    }

    async fn resolve(&self, input: ApiKeyAuthInput) -> Result<Option<AuthResult>> {
        let auth_ctx = input.ctx.clone();
        let cred_env = input.credential.as_ref().and_then(|c| c.env.clone());
        let result = if let Some(ref cred) = input.credential {
            if let Some(ref inh) = self.inherited {
                inh.resolve(input).await?
            } else if let Some(ref key) = cred.key
                && !key.is_empty()
            {
                Some(AuthResult {
                    auth: ModelAuth {
                        api_key: Some(key.clone()),
                        headers: None,
                        base_url: None,
                    },
                    env: cred.env.clone(),
                    source: Some("stored credential".to_owned()),
                })
            } else {
                None
            }
        } else if let Some(ref raw_key) = self.raw_key {
            let env = config_context_env(&[raw_key.clone()], auth_ctx.as_ref(), None).await;
            let key = resolve_config_value_or_throw(
                raw_key,
                &format!("API key for provider \"{}\"", self.provider_id),
                env.as_ref(),
            )?;
            if let Some(ref inh) = self.inherited {
                let next_input = ApiKeyAuthInput {
                    ctx: auth_ctx.clone(),
                    credential: Some(ApiKeyCredential {
                        key: Some(key),
                        env: None,
                    }),
                    signal: input.signal.clone(),
                };
                inh.resolve(next_input).await?
            } else {
                Some(AuthResult {
                    auth: ModelAuth {
                        api_key: Some(key),
                        headers: None,
                        base_url: None,
                    },
                    env: None,
                    source: Some("configured API key".to_owned()),
                })
            }
        } else if let Some(ref inh) = self.inherited {
            inh.resolve(input).await?
        } else {
            None
        };

        let Some(mut resolved) = result else {
            return Ok(None);
        };
        let mut explicit_env = cred_env.unwrap_or_default();
        if let Some(ref res_env) = resolved.env {
            for (k, v) in res_env {
                explicit_env.insert(k.clone(), v.clone());
            }
        }
        let header_values: Vec<String> = self
            .raw_headers
            .as_ref()
            .map(|h| h.values().cloned().collect())
            .unwrap_or_default();
        let header_env = config_context_env(&header_values, auth_ctx.as_ref(), Some(&explicit_env)).await;
        let headers = resolve_headers_or_throw(
            self.raw_headers.as_ref(),
            &format!("provider \"{}\"", self.provider_id),
            header_env.as_ref(),
        )?;
        resolved.auth = with_configured_auth(resolved.auth, headers, self.auth_header)?;
        Ok(Some(resolved))
    }
}

fn compose_api_key_auth(
    provider_id: &str,
    base: Option<&dyn Provider>,
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ProviderConfigInput>,
) -> Option<Arc<dyn ApiKeyAuth>> {
    let inherited = base.and_then(|b| b.auth().api_key.clone());
    let raw_key = configured_api_key(config, extension).map(str::to_owned);
    let has_oauth =
        extension.and_then(|e| e.oauth.as_ref()).is_some() || base.and_then(|b| b.auth().oauth.as_ref()).is_some();
    if inherited.is_none() && raw_key.is_none() && has_oauth {
        return None;
    }
    let name = inherited
        .as_ref()
        .map(|a| a.name().to_owned())
        .unwrap_or_else(|| "API key".to_owned());
    let raw_headers = configured_headers(config, extension);
    let auth_header = extension
        .and_then(|e| e.auth_header)
        .or_else(|| config.and_then(|c| c.auth_header))
        .unwrap_or(false);
    Some(Arc::new(ComposedApiKeyAuth {
        provider_id: provider_id.to_owned(),
        name,
        inherited,
        raw_key,
        raw_headers,
        auth_header,
    }))
}

struct ComposedOAuthAuth {
    provider_id: String,
    inner: Arc<dyn OAuthAuth>,
    raw_headers: Option<IndexMap<String, String>>,
    auth_header: bool,
}

#[async_trait::async_trait]
impl OAuthAuth for ComposedOAuthAuth {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn is_subscription(&self) -> bool {
        self.inner.is_subscription()
    }

    fn login_label(&self) -> Option<&str> {
        self.inner.login_label()
    }

    async fn login(
        &self,
        interaction: ProviderAuthInteraction,
        options: Option<LoginOptions>,
    ) -> Result<OAuthCredential> {
        self.inner.login(interaction, options).await
    }

    async fn refresh(&self, credential: OAuthCredential, signal: AbortSignal) -> Result<OAuthCredential> {
        self.inner.refresh(credential, signal).await
    }

    async fn to_auth(&self, credential: OAuthCredential) -> Result<ModelAuth> {
        let env_from_extra: Option<IndexMap<String, String>> = credential
            .extra
            .get("env")
            .and_then(|v| serde_json::from_value::<IndexMap<String, String>>(v.clone()).ok());
        let auth = self.inner.to_auth(credential).await?;
        let headers = resolve_headers_or_throw(
            self.raw_headers.as_ref(),
            &format!("provider \"{}\"", self.provider_id),
            env_from_extra.as_ref(),
        )?;
        with_configured_auth(auth, headers, self.auth_header)
    }
}

fn compose_oauth_auth(
    provider_id: &str,
    base: Option<&dyn Provider>,
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ProviderConfigInput>,
) -> Option<Arc<dyn OAuthAuth>> {
    let inner: Arc<dyn OAuthAuth> = match extension.and_then(|e| e.oauth.clone()) {
        Some(ext_oauth) => Arc::new(AdaptedExtensionOAuth { config: ext_oauth }),
        None => base.and_then(|b| b.auth().oauth.clone())?,
    };
    let raw_headers = configured_headers(config, extension);
    let auth_header = extension
        .and_then(|e| e.auth_header)
        .or_else(|| config.and_then(|c| c.auth_header))
        .unwrap_or(false);
    Some(Arc::new(ComposedOAuthAuth {
        provider_id: provider_id.to_owned(),
        inner,
        raw_headers,
        auth_header,
    }))
}

fn raw_model_headers(
    model: &AnyModel,
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ProviderConfigInput>,
) -> Option<IndexMap<String, String>> {
    let is_chat = is_model_type(model, ModelType::Chat);
    let model_kind = pi_ai::get_model_type(model);
    let chat_definition = if is_chat {
        config
            .and_then(|c| c.models.as_ref())
            .and_then(|models| models.iter().find(|entry| entry.id == model.id()))
    } else {
        None
    };
    let extension_model = extension.and_then(|e| e.models.as_ref()).and_then(|models| {
        models
            .iter()
            .find(|entry| entry.model_type() == model_kind && entry.id() == model.id())
    });

    let mut headers = IndexMap::new();
    if is_chat
        && let Some(ovr_hdrs) = config
            .and_then(|c| c.model_overrides.as_ref())
            .and_then(|m| m.get(model.id()))
            .and_then(|o| o.headers.as_ref())
    {
        for (k, v) in ovr_hdrs {
            headers.insert(k.clone(), v.clone());
        }
    }
    if let Some(def_hdrs) = chat_definition.and_then(|d| d.headers.as_ref()) {
        for (k, v) in def_hdrs {
            headers.insert(k.clone(), v.clone());
        }
    }
    if let Some(ext_hdrs) = extension_model.and_then(|m| m.headers()) {
        for (k, v) in ext_hdrs {
            headers.insert(k.clone(), v.clone());
        }
    }
    if headers.is_empty() { None } else { Some(headers) }
}

pub fn validate_extension_provider(
    provider_id: &str,
    base: Option<&dyn Provider>,
    models_config: Option<&ModelsJsonProvider>,
    extension: &ProviderConfigInput,
) -> Result<()> {
    if extension.stream_simple.is_some() && extension.api.is_none() {
        return Err(Error::msg(format!(
            "Provider {provider_id}: \"api\" is required when registering streamSimple."
        )));
    }
    let json_applied = apply_models_json(provider_id, &get_all_provider_models(base), models_config)?;
    let _ = apply_extension(provider_id, json_applied, Some(extension))?;
    Ok(())
}

struct ComposedProviderState {
    extension_oauth_credential: Option<OAuthCredentials>,
    refreshed_extension_models: Option<Vec<ProviderModelConfig>>,
}

struct ComposedProvider {
    provider_id: String,
    name: String,
    base_url: Option<String>,
    base_headers: Option<ProviderHeaders>,
    auth: ProviderAuth,
    base: Option<Arc<dyn Provider + Send + Sync>>,
    config: Option<ModelsJsonProvider>,
    extension: Option<ProviderConfigInput>,
    state: Arc<Mutex<ComposedProviderState>>,
}

impl ComposedProvider {
    fn compute_all_models(&self) -> Result<Vec<AnyModel>> {
        let (oauth_cred, refreshed_models) = {
            let guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            (
                guard.extension_oauth_credential.clone(),
                guard.refreshed_extension_models.clone(),
            )
        };
        let mut ext_buf = self.extension.clone();
        if let Some(refreshed) = refreshed_models
            && let Some(ref mut ext) = ext_buf
        {
            ext.models = Some(refreshed);
        }
        let base_models = get_all_provider_models(self.base.as_deref().map(|b| b as &dyn Provider));
        let json_applied = apply_models_json(&self.provider_id, &base_models, self.config.as_ref())?;
        let mut models = apply_extension(&self.provider_id, json_applied, ext_buf.as_ref())?;

        if let Some(ref cred) = oauth_cred
            && let Some(modify) = self
                .extension
                .as_ref()
                .and_then(|e| e.oauth.as_ref())
                .and_then(|o| o.modify_models.as_ref())
        {
            let chat_models: Vec<Model> = models.iter().filter_map(|m| m.as_chat().cloned()).collect();
            let non_chat: Vec<AnyModel> = models
                .into_iter()
                .filter(|m| !is_model_type(m, ModelType::Chat))
                .collect();
            let mut modified: Vec<AnyModel> = modify(chat_models, cred).into_iter().map(AnyModel::Chat).collect();
            modified.extend(non_chat);
            models = modified;
        }

        let overrides = self.config.as_ref().and_then(|c| c.model_overrides.as_ref());
        Ok(models
            .into_iter()
            .map(|m| match m {
                AnyModel::Chat(chat) => {
                    if let Some(ovr) = overrides.and_then(|o| o.get(&chat.id)) {
                        AnyModel::Chat(apply_model_override(chat, ovr))
                    } else {
                        AnyModel::Chat(chat)
                    }
                }
                other => other,
            })
            .collect())
    }

    fn supports_base_api(&self, api: &str) -> bool {
        self.base
            .as_ref()
            .is_some_and(|b| b.get_models().iter().any(|entry| entry.api == api))
    }

    fn stream_chat(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<ApiStreamOptions>,
        simple_options: Option<SimpleStreamOptions>,
        simple: bool,
    ) -> AssistantMessageEventStream {
        let ext_stream_simple = self.extension.as_ref().and_then(|e| e.stream_simple.clone());
        let ext_api = self.extension.as_ref().and_then(|e| e.api.clone());
        let base = self.base.clone();
        let supports_base = self.supports_base_api(&model.api);
        let model_owned = model.clone();
        let context_owned = context.clone();

        let signal = options
            .as_ref()
            .and_then(|o| o.base.signal.clone())
            .or_else(|| simple_options.as_ref().and_then(|o| o.base.signal.clone()));

        lazy_stream_with_options(
            model,
            async move {
                let effective_simple = if simple {
                    simple_options.unwrap_or_default()
                } else {
                    SimpleStreamOptions {
                        base: options.as_ref().map(|o| o.base.clone()).unwrap_or_default(),
                        ..Default::default()
                    }
                };

                if let Some(stream_simple_fn) = ext_stream_simple
                    && ext_api.as_deref() == Some(model_owned.api.as_str())
                {
                    return Ok(stream_simple_fn(&model_owned, &context_owned, Some(effective_simple)));
                }
                if let Some(base_provider) = base
                    && supports_base
                {
                    return Ok(if simple {
                        base_provider.stream_simple(&model_owned, &context_owned, Some(effective_simple))
                    } else {
                        base_provider.stream(&model_owned, &context_owned, options)
                    });
                }
                let api_provider = get_api_provider(&model_owned.api)
                    .ok_or_else(|| Error::msg(format!("No API provider registered for api: {}", model_owned.api)))?;
                Ok(if simple {
                    (api_provider.stream_simple)(&model_owned, &context_owned, Some(effective_simple))
                } else {
                    (api_provider.stream)(&model_owned, &context_owned, options)
                })
            },
            None,
            signal,
        )
    }
}

fn clone_refresh_context(ctx: &RefreshModelsContext) -> RefreshModelsContext {
    RefreshModelsContext {
        credential: ctx.credential.clone(),
        stored: ctx.stored.clone(),
        publish: Arc::clone(&ctx.publish),
        allow_network: ctx.allow_network,
        force: ctx.force,
        signal: ctx.signal.clone(),
        base_url: ctx.base_url.clone(),
    }
}

impl Provider for ComposedProvider {
    fn id(&self) -> &str {
        &self.provider_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn base_url(&self) -> Option<&str> {
        self.base_url.as_deref()
    }

    fn headers(&self) -> Option<&ProviderHeaders> {
        self.base_headers.as_ref()
    }

    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<Model> {
        self.compute_all_models()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|m| m.into_chat())
            .collect()
    }

    fn get_all_models(&self) -> Option<Vec<AnyModel>> {
        Some(self.compute_all_models().unwrap_or_default())
    }

    fn refresh_models_auth(&self) -> RefreshModelsAuth {
        let ext_overrides_refresh = self.extension.as_ref().is_some_and(|e| {
            e.refresh_models.is_some() || e.oauth.as_ref().and_then(|o| o.modify_models.as_ref()).is_some()
        });
        if ext_overrides_refresh {
            RefreshModelsAuth::Credential
        } else {
            self.base
                .as_ref()
                .map_or(RefreshModelsAuth::Credential, |b| b.refresh_models_auth())
        }
    }

    fn refresh_models(&self, context: RefreshModelsContext) -> Option<BoxFuture<Result<()>>> {
        let ext_refresh = self.extension.as_ref().and_then(|e| e.refresh_models.clone());
        let has_modify_models = self
            .extension
            .as_ref()
            .and_then(|e| e.oauth.as_ref())
            .and_then(|o| o.modify_models.as_ref())
            .is_some();

        let base_fut = self
            .base
            .as_ref()
            .and_then(|b| b.refresh_models(clone_refresh_context(&context)));

        if base_fut.is_none() && ext_refresh.is_none() && !has_modify_models {
            return None;
        }

        let provider_id = self.provider_id.clone();
        let base = self.base.clone();
        let config = self.config.clone();
        let extension = self.extension.clone();
        let state = self.state.clone();

        Some(Box::pin(async move {
            if let Some(fut) = base_fut {
                fut.await?;
            }
            let refreshed = match ext_refresh {
                Some(refresh_fn) => Some(refresh_fn(clone_refresh_context(&context)).await?),
                None => None,
            };
            if context.signal.aborted() {
                return Ok(());
            }
            let oauth_credential = match context.credential {
                Some(Credential::OAuth(c)) => Some(to_oauth_credentials(c)),
                _ => None,
            };
            let refreshed_for_update = refreshed.clone();
            if let Some(ref refreshed_models) = refreshed_for_update {
                let mut ext_buf = extension.clone().unwrap_or_default();
                ext_buf.models = Some(refreshed_models.clone());
                let base_models = get_all_provider_models(base.as_deref().map(|b| b as &dyn Provider));
                let json_applied = apply_models_json(&provider_id, &base_models, config.as_ref())?;
                let _ = apply_extension(&provider_id, json_applied, Some(&ext_buf))?;
            }
            let _ = (context.publish)(ModelsPublication {
                persist: None,
                update: Some(Arc::new(move || {
                    let mut guard = state.lock().unwrap_or_else(PoisonError::into_inner);
                    if let Some(ref refreshed_models) = refreshed_for_update {
                        guard.refreshed_extension_models = Some(refreshed_models.clone());
                    }
                    guard.extension_oauth_credential = oauth_credential.clone();
                })),
            })
            .await?;
            Ok(())
        }))
    }

    fn filter_models(&self, models: &[Model], credential: Option<&Credential>) -> Option<Vec<Model>> {
        self.base.as_ref().and_then(|b| b.filter_models(models, credential))
    }

    fn filter_all_models(&self, models: &[AnyModel], credential: Option<&Credential>) -> Option<Vec<AnyModel>> {
        self.base.as_ref().and_then(|b| b.filter_all_models(models, credential))
    }

    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<ApiStreamOptions>,
    ) -> AssistantMessageEventStream {
        self.stream_chat(model, context, options, None, false)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        self.stream_chat(model, context, None, options, true)
    }

    fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<DeferredFetchOptions>,
    ) -> Option<AssistantMessageEventStream> {
        self.base
            .as_ref()
            .and_then(|b| b.fetch_deferred(model, handle, options))
    }

    fn cancel_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<DeferredCancelOptions>,
    ) -> Option<BoxFuture<Result<()>>> {
        self.base
            .as_ref()
            .and_then(|b| b.cancel_deferred(model, handle, options))
    }

    fn generate_images(
        &self,
        model: &ImageModel,
        context: &ImagesContext,
        options: Option<ImagesOptions>,
    ) -> Option<BoxFuture<AssistantImages>> {
        let ext_impl = self
            .extension
            .as_ref()
            .and_then(|e| e.images.as_ref())
            .and_then(|m| m.get(&model.api));
        if let Some(implementation) = ext_impl {
            return Some(implementation.generate_images(model, context, options));
        }
        let base_fut = self
            .base
            .as_ref()
            .and_then(|b| b.generate_images(model, context, options));
        if base_fut.is_some() {
            return base_fut;
        }
        let has_extension_images = self
            .extension
            .as_ref()
            .and_then(|e| e.images.as_ref())
            .is_some_and(|m| !m.is_empty());
        if has_extension_images {
            let model_owned = model.clone();
            let provider_id = self.provider_id.clone();
            return Some(Box::pin(async move {
                image_error_result(
                    &model_owned,
                    &Error::msg(format!(
                        "Provider {provider_id} has no image implementation for \"{}\"",
                        model_owned.api
                    )),
                    false,
                )
            }));
        }
        None
    }

    fn classify(
        &self,
        model: &ClassifierModel,
        context: &ClassifierContext,
        options: Option<ClassifierOptions>,
    ) -> Option<BoxFuture<ClassifierResult>> {
        let ext_impl = self
            .extension
            .as_ref()
            .and_then(|e| e.classifiers.as_ref())
            .and_then(|m| m.get(&model.api));
        if let Some(implementation) = ext_impl {
            return Some(implementation.classify(model, context, options));
        }
        let base_fut = self.base.as_ref().and_then(|b| b.classify(model, context, options));
        if base_fut.is_some() {
            return base_fut;
        }
        let has_extension_classifiers = self
            .extension
            .as_ref()
            .and_then(|e| e.classifiers.as_ref())
            .is_some_and(|m| !m.is_empty());
        if has_extension_classifiers {
            let model_owned = model.clone();
            let provider_id = self.provider_id.clone();
            return Some(Box::pin(async move {
                classifier_error_result(
                    &model_owned,
                    &Error::msg(format!(
                        "Provider {provider_id} has no classifier implementation for \"{}\"",
                        model_owned.api
                    )),
                    false,
                )
            }));
        }
        None
    }
}

/// Compose built-in, `models.json`, and extension layers without reading credentials.
pub fn compose_model_provider(
    provider_id: &str,
    base: Option<Arc<dyn Provider + Send + Sync>>,
    model_config: &ModelConfig,
    extension: Option<&ProviderConfigInput>,
) -> Result<Arc<dyn Provider + Send + Sync>> {
    let config = model_config.get_provider(provider_id).cloned();
    let base_ref = base.as_deref().map(|b| b as &dyn Provider);
    let api_key = compose_api_key_auth(provider_id, base_ref, config.as_ref(), extension);
    let oauth = compose_oauth_auth(provider_id, base_ref, config.as_ref(), extension);
    if api_key.is_none() && oauth.is_none() {
        return Err(Error::msg(format!(
            "Provider {provider_id}: no authentication method configured."
        )));
    }

    let name = extension
        .and_then(|e| e.name.clone())
        .or_else(|| config.as_ref().and_then(|c| c.name.clone()))
        .or_else(|| base.as_ref().map(|b| b.name().to_owned()))
        .or_else(|| extension.and_then(|e| e.oauth.as_ref()).map(|o| o.name.clone()))
        .unwrap_or_else(|| provider_id.to_owned());

    let base_url = extension
        .and_then(|e| e.base_url.clone())
        .or_else(|| config.as_ref().and_then(|c| c.base_url.clone()))
        .or_else(|| base.as_ref().and_then(|b| b.base_url().map(str::to_owned)));

    let base_headers = base.as_ref().and_then(|b| b.headers().cloned());

    let composed = Arc::new(ComposedProvider {
        provider_id: provider_id.to_owned(),
        name,
        base_url,
        base_headers,
        auth: ProviderAuth { api_key, oauth },
        base,
        config,
        extension: extension.cloned(),
        state: Arc::new(Mutex::new(ComposedProviderState {
            extension_oauth_credential: None,
            refreshed_extension_models: None,
        })),
    });

    // Validate eagerly so registration/reload reports structural errors immediately.
    let _ = composed.compute_all_models()?;
    Ok(composed)
}

pub fn resolve_configured_model_headers(
    model: &AnyModel,
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ProviderConfigInput>,
    env: Option<&IndexMap<String, String>>,
) -> Result<Option<IndexMap<String, String>>> {
    let raw = raw_model_headers(model, config, extension);
    resolve_headers_or_throw(
        raw.as_ref(),
        &format!("model \"{}/{}\"", model.provider(), model.id()),
        env,
    )
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompatibilityRequestConfig {
    pub headers: Option<ProviderHeaders>,
    pub auth_header: bool,
}

pub fn resolve_compatibility_request_config(
    model: &AnyModel,
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ProviderConfigInput>,
) -> Result<CompatibilityRequestConfig> {
    let mut raw_combined = configured_headers(config, extension).unwrap_or_default();
    if let Some(model_hdrs) = raw_model_headers(model, config, extension) {
        for (k, v) in model_hdrs {
            raw_combined.insert(k, v);
        }
    }
    let raw_opt = if raw_combined.is_empty() {
        None
    } else {
        Some(raw_combined)
    };
    let configured = resolve_headers_or_throw(
        raw_opt.as_ref(),
        &format!("model \"{}/{}\"", model.provider(), model.id()),
        None,
    )?;
    let model_headers = model.headers();
    let headers = if model_headers.is_some() || configured.is_some() {
        let mut merged: ProviderHeaders = ProviderHeaders::new();
        if let Some(mh) = model_headers {
            for (k, v) in mh {
                merged.insert(k.clone(), Some(v.clone()));
            }
        }
        if let Some(cfg) = configured {
            for (k, v) in cfg {
                merged.insert(k, Some(v));
            }
        }
        Some(merged)
    } else {
        None
    };
    Ok(CompatibilityRequestConfig {
        headers,
        auth_header: extension
            .and_then(|e| e.auth_header)
            .or_else(|| config.and_then(|c| c.auth_header))
            .unwrap_or(false),
    })
}

pub fn configured_request_auth_status(
    config: Option<&ModelsJsonProvider>,
    extension: Option<&ProviderConfigInput>,
) -> Option<AuthStatus> {
    let value = configured_api_key(config, extension)?;
    if is_command_config_value(value) {
        return Some(AuthStatus {
            configured: true,
            source: Some(AuthStatusSource::ModelsJsonCommand),
            label: None,
        });
    }
    let names = get_config_value_env_var_names(value);
    if !names.is_empty() {
        return Some(if is_config_value_configured(value, None) {
            AuthStatus {
                configured: true,
                source: Some(AuthStatusSource::Environment),
                label: Some(names.join(", ")),
            }
        } else {
            AuthStatus {
                configured: false,
                source: None,
                label: None,
            }
        });
    }
    Some(AuthStatus {
        configured: true,
        source: Some(if extension.and_then(|e| e.api_key.as_ref()).is_some() {
            AuthStatusSource::Fallback
        } else {
            AuthStatusSource::ModelsJsonKey
        }),
        label: None,
    })
}
