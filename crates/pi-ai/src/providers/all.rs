//! Port of packages/ai/src/providers/all.ts

use std::sync::Arc;

use indexmap::IndexMap;

use crate::auth::helpers::env_api_key_auth;
use crate::auth::types::ProviderAuth;
use crate::model_catalog::{
    ChatModelCatalog, ClassifierModelCatalog, ImageModelCatalog, flatten_chat_model_catalog,
    flatten_classifier_model_catalog, flatten_image_model_catalog, parse_model_groups_json,
};
use crate::models::{
    CreateModelsOptions, CreateProviderApi, CreateProviderOptions, MutableModels, Provider, create_models,
    create_provider,
};
use crate::models_generated::{
    BUILTIN_MODEL_PROVIDER_IDS, classifier_models_catalog, image_models_catalog, models_catalog,
};
use crate::providers::{
    amazon_bedrock::amazon_bedrock_provider, ant_ling::ant_ling_provider, anthropic::anthropic_provider,
    azure::azure_provider, baseten::baseten_provider, cerebras::cerebras_provider,
    cloudflare_ai_gateway::cloudflare_ai_gateway_provider, cloudflare_workers_ai::cloudflare_workers_ai_provider,
    deepseek::deepseek_provider, fireworks::fireworks_provider, github_copilot::github_copilot_provider,
    google::google_provider, google_vertex::google_vertex_provider, groq::groq_provider,
    huggingface::huggingface_provider, kimi_coding::kimi_coding_provider, meta::meta_provider,
    minimax::minimax_provider, minimax_cn::minimax_cn_provider, mistral::mistral_provider,
    moonshotai::moonshotai_provider, moonshotai_cn::moonshotai_cn_provider, nvidia::nvidia_provider,
    openai::openai_provider, openai_codex::openai_codex_provider, opencode::opencode_provider,
    opencode_go::opencode_go_provider, openrouter::openrouter_provider, qwen_token_plan::qwen_token_plan_provider,
    qwen_token_plan_cn::qwen_token_plan_cn_provider, qwen_token_plan_individual::qwen_token_plan_individual_provider,
    qwen_token_plan_individual_cn::qwen_token_plan_individual_cn_provider, together::together_provider,
    typesafe::typesafe_provider, vercel_ai_gateway::vercel_ai_gateway_provider, xai::xai_provider,
    xiaomi::xiaomi_provider, xiaomi_token_plan_ams::xiaomi_token_plan_ams_provider,
    xiaomi_token_plan_cn::xiaomi_token_plan_cn_provider, xiaomi_token_plan_sgp::xiaomi_token_plan_sgp_provider,
    zai::zai_provider, zai_coding_cn::zai_coding_cn_provider,
};
use crate::types::{
    AnyModel, ApiStreamOptions, ClassifierModel, ImageModel, Model, ProviderStreams, SimpleStreamOptions,
    TranscriptContext,
};
use crate::utils::event_stream::AssistantMessageEventStream;

pub use crate::providers::radius::radius_provider;

const MODEL_DATA_MANIFEST_JSON: &str = include_str!("data/.manifest.json");

pub type BuiltinProvider = &'static str;

pub(crate) struct ProviderModelCatalog {
    pub(crate) chat: ChatModelCatalog,
    pub(crate) image: ImageModelCatalog,
    pub(crate) classifier: ClassifierModelCatalog,
}

pub(crate) fn load_provider_catalog(provider: &str, raw_json: &str) -> ProviderModelCatalog {
    let Ok(groups) = parse_model_groups_json(raw_json) else {
        return ProviderModelCatalog {
            chat: IndexMap::new(),
            image: IndexMap::new(),
            classifier: IndexMap::new(),
        };
    };
    ProviderModelCatalog {
        chat: flatten_chat_model_catalog(provider, &groups).unwrap_or_default(),
        image: flatten_image_model_catalog(provider, &groups).unwrap_or_default(),
        classifier: flatten_classifier_model_catalog(provider, &groups).unwrap_or_default(),
    }
}

struct FallbackStaticProvider {
    id: String,
    name: String,
    base_url: Option<String>,
    auth: ProviderAuth,
    models: Vec<AnyModel>,
    error_message: String,
}

impl Provider for FallbackStaticProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn base_url(&self) -> Option<&str> {
        self.base_url.as_deref()
    }

    fn auth(&self) -> &ProviderAuth {
        &self.auth
    }

    fn get_models(&self) -> Vec<Model> {
        self.models.iter().filter_map(|m| m.as_chat().cloned()).collect()
    }

    fn get_all_models(&self) -> Option<Vec<AnyModel>> {
        Some(self.models.clone())
    }

    fn stream(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        _options: Option<ApiStreamOptions>,
    ) -> AssistantMessageEventStream {
        crate::api::lazy::lazy_stream(model, {
            let msg = self.error_message.clone();
            async move { Err(pi_js::Error::msg(msg)) }
        })
    }

    fn stream_simple(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        _options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        crate::api::lazy::lazy_stream(model, {
            let msg = self.error_message.clone();
            async move { Err(pi_js::Error::msg(msg)) }
        })
    }
}

pub(crate) fn build_provider(options: CreateProviderOptions) -> Arc<dyn Provider + Send + Sync> {
    let id = options.id.clone();
    let name = options.name.clone().unwrap_or_else(|| id.clone());
    let base_url = options.base_url.clone();
    let auth = options.auth.clone();
    let models = options.models.clone();
    match create_provider(options) {
        Ok(provider) => provider,
        Err(err) => Arc::new(FallbackStaticProvider {
            id,
            name,
            base_url,
            auth,
            models,
            error_message: err.to_string(),
        }),
    }
}

fn unwrap_provider_result(
    id: &str,
    result: pi_js::Result<Arc<dyn Provider + Send + Sync>>,
) -> Arc<dyn Provider + Send + Sync> {
    match result {
        Ok(provider) => provider,
        Err(err) => Arc::new(FallbackStaticProvider {
            id: id.to_owned(),
            name: id.to_owned(),
            base_url: None,
            auth: ProviderAuth::default(),
            models: Vec::new(),
            error_message: err.to_string(),
        }),
    }
}

pub(crate) fn build_env_provider(
    id: &str,
    name: &str,
    base_url: &str,
    auth_label: &str,
    env_vars: &[&str],
    models: &ChatModelCatalog,
    api: ProviderStreams,
) -> Arc<dyn Provider + Send + Sync> {
    build_provider(CreateProviderOptions {
        id: id.to_owned(),
        name: Some(name.to_owned()),
        base_url: Some(base_url.to_owned()),
        headers: None,
        auth: ProviderAuth {
            api_key: Some(env_api_key_auth(auth_label, env_vars)),
            oauth: None,
        },
        models: models.values().cloned().map(AnyModel::Chat).collect(),
        fetch_models: None,
        filter_models: None,
        filter_all_models: None,
        api: Some(CreateProviderApi::Single(api)),
        images: None,
        classifiers: None,
    })
}

/// Typed read of one generated built-in chat model.
pub fn get_builtin_model(provider: &str, model_id: &str) -> Option<Model> {
    models_catalog().get(provider)?.get(model_id).cloned()
}

/// Typed read of one generated built-in image model.
pub fn get_builtin_image_model(provider: &str, model_id: &str) -> Option<ImageModel> {
    image_models_catalog().get(provider)?.get(model_id).cloned()
}

/// Typed read of one generated built-in classifier model.
pub fn get_builtin_classifier_model(provider: &str, model_id: &str) -> Option<ClassifierModel> {
    classifier_models_catalog().get(provider)?.get(model_id).cloned()
}

pub fn get_builtin_providers() -> Vec<&'static str> {
    BUILTIN_MODEL_PROVIDER_IDS.to_vec()
}

/// Generation timestamp shared by all built-in provider catalogs.
pub fn get_builtin_model_data_generated_at() -> Option<i64> {
    let manifest = pi_js::json::parse(MODEL_DATA_MANIFEST_JSON).ok()?;
    let generated_at = manifest.get("generatedAt")?.as_str()?;
    pi_js::time::parse_date(generated_at)
}

pub fn get_builtin_models(provider: &str) -> Vec<Model> {
    models_catalog()
        .get(provider)
        .map(|models| models.values().cloned().collect())
        .unwrap_or_default()
}

pub fn get_builtin_image_models(provider: &str) -> Vec<ImageModel> {
    image_models_catalog()
        .get(provider)
        .map(|models| models.values().cloned().collect())
        .unwrap_or_default()
}

pub fn get_builtin_classifier_models(provider: &str) -> Vec<ClassifierModel> {
    classifier_models_catalog()
        .get(provider)
        .map(|models| models.values().cloned().collect())
        .unwrap_or_default()
}

pub fn get_all_builtin_models(provider: &str) -> Vec<AnyModel> {
    let mut out = Vec::new();
    out.extend(get_builtin_models(provider).into_iter().map(AnyModel::Chat));
    out.extend(get_builtin_image_models(provider).into_iter().map(AnyModel::Image));
    out.extend(
        get_builtin_classifier_models(provider)
            .into_iter()
            .map(AnyModel::Classifier),
    );
    out
}

/// All built-in providers, freshly constructed.
pub fn builtin_providers() -> Vec<Arc<dyn Provider + Send + Sync>> {
    vec![
        amazon_bedrock_provider(),
        ant_ling_provider(),
        unwrap_provider_result("anthropic", anthropic_provider()),
        unwrap_provider_result("azure", azure_provider()),
        baseten_provider(),
        cerebras_provider(),
        cloudflare_ai_gateway_provider(),
        cloudflare_workers_ai_provider(),
        deepseek_provider(),
        fireworks_provider(),
        github_copilot_provider(),
        google_provider(),
        google_vertex_provider(),
        groq_provider(),
        huggingface_provider(),
        kimi_coding_provider(),
        meta_provider(),
        minimax_provider(),
        minimax_cn_provider(),
        mistral_provider(),
        moonshotai_provider(),
        moonshotai_cn_provider(),
        nvidia_provider(),
        unwrap_provider_result("openai", openai_provider()),
        unwrap_provider_result("openai-codex", openai_codex_provider()),
        opencode_provider(),
        opencode_go_provider(),
        openrouter_provider(),
        qwen_token_plan_provider(),
        qwen_token_plan_cn_provider(),
        qwen_token_plan_individual_provider(),
        qwen_token_plan_individual_cn_provider(),
        radius_provider(),
        together_provider(),
        typesafe_provider(),
        vercel_ai_gateway_provider(),
        xai_provider(),
        xiaomi_provider(),
        xiaomi_token_plan_ams_provider(),
        xiaomi_token_plan_cn_provider(),
        xiaomi_token_plan_sgp_provider(),
        zai_provider(),
        zai_coding_cn_provider(),
    ]
}

/// A `Models` collection with every built-in provider registered.
pub fn builtin_models(options: Option<CreateModelsOptions>) -> MutableModels {
    let models = create_models(options);
    for provider in builtin_providers() {
        models.set_provider(provider);
    }
    models
}
