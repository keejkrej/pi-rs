//! Port of packages/coding-agent/src/core/model-registry.ts

use std::sync::Arc;

use indexmap::IndexMap;
use pi_ai::models::{
    ModelsApiStreamOptions, ModelsClassifierOptions, ModelsImagesOptions, ModelsRefreshOptions, ModelsRefreshResult,
    ModelsSimpleStreamOptions, Provider,
};
use pi_ai::types::{
    AnyModel, AssistantImages, ClassifierContext, ClassifierModel, ClassifierResult, Context, ImageModel,
    ImagesContext, Model, ModelType, ProviderHeaders,
};
use pi_ai::{AssistantMessage, AssistantMessageEventStream, AuthOperationOptions, AuthResult};
use pi_js::Result;

use crate::core::model_runtime::ModelRuntime;
pub use crate::core::provider_composer::{AuthStatus, ProviderConfigInput, clear_api_key_cache};
use crate::core::virtual_models::VirtualModelDefinition;

#[derive(Clone, Debug, PartialEq)]
pub enum ResolvedRequestAuth {
    Ok {
        api_key: Option<String>,
        headers: Option<ProviderHeaders>,
        base_url: Option<String>,
        env: Option<IndexMap<String, String>>,
    },
    Err {
        error: String,
    },
}

impl ResolvedRequestAuth {
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok { .. })
    }

    pub fn api_key(&self) -> Option<&str> {
        match self {
            Self::Ok { api_key, .. } => api_key.as_deref(),
            Self::Err { .. } => None,
        }
    }

    pub fn error(&self) -> Option<&str> {
        match self {
            Self::Err { error } => Some(error),
            Self::Ok { .. } => None,
        }
    }
}

pub enum RegisterProviderArg {
    Native(Arc<dyn Provider + Send + Sync>),
    Config { name: String, config: ProviderConfigInput },
}

/// Synchronous compatibility facade exposed to extensions.
/// Coding-agent internals use `ModelRuntime` directly.
#[derive(Clone)]
pub struct ModelRegistry {
    runtime: ModelRuntime,
}

impl ModelRegistry {
    pub fn new(runtime: ModelRuntime) -> Self {
        Self { runtime }
    }

    pub fn runtime(&self) -> &ModelRuntime {
        &self.runtime
    }

    /// Reload `models.json` asynchronously. Await before making synchronous registry reads.
    pub async fn refresh(&self, options: Option<ModelsRefreshOptions>) -> Result<ModelsRefreshResult> {
        self.runtime.refresh(options).await
    }

    pub fn get_error(&self) -> Option<String> {
        self.runtime.get_error()
    }

    pub fn get_all(&self) -> Vec<Model> {
        self.runtime.get_models(None)
    }

    pub fn get_available(&self) -> Vec<Model> {
        self.runtime.get_available_snapshot()
    }

    pub fn find(&self, provider: &str, model_id: &str) -> Option<Model> {
        self.runtime.get_model(provider, model_id)
    }

    pub fn find_of_type(&self, model_type: ModelType, provider: &str, model_id: &str) -> Option<AnyModel> {
        self.runtime.get_model_of_type(model_type, provider, model_id)
    }

    pub fn has_configured_auth(&self, model: &Model) -> bool {
        self.runtime.has_configured_auth(&model.provider)
    }

    pub async fn get_api_key_and_headers(&self, model: &Model) -> ResolvedRequestAuth {
        let any_model = AnyModel::Chat(model.clone());
        match self.runtime.get_auth_model(&any_model, None).await {
            Ok(Some(resolution)) => ResolvedRequestAuth::Ok {
                api_key: resolution.auth.api_key,
                headers: resolution.auth.headers,
                base_url: resolution.auth.base_url,
                env: resolution.env,
            },
            Ok(None) => match self.runtime.get_compatibility_request_config(model) {
                Ok(compat) => {
                    if compat.auth_header {
                        ResolvedRequestAuth::Err {
                            error: format!("No API key found for \"{}\"", model.provider),
                        }
                    } else {
                        ResolvedRequestAuth::Ok {
                            api_key: None,
                            headers: compat.headers,
                            base_url: None,
                            env: None,
                        }
                    }
                }
                Err(err) => {
                    let msg = err.to_string();
                    ResolvedRequestAuth::Err {
                        error: if msg == "authHeader requires a resolved API key" {
                            format!("No API key found for \"{}\"", model.provider)
                        } else {
                            msg
                        },
                    }
                }
            },
            Err(err) => {
                let msg = err.to_string();
                let error = if msg == "authHeader requires a resolved API key" {
                    format!("No API key found for \"{}\"", model.provider)
                } else {
                    msg
                };
                ResolvedRequestAuth::Err { error }
            }
        }
    }

    pub fn get_provider_auth_status(&self, provider: &str) -> AuthStatus {
        self.runtime.get_provider_auth_status(provider)
    }

    pub fn get_provider(&self, provider: &str) -> Option<Arc<dyn Provider + Send + Sync>> {
        self.runtime.get_provider(provider)
    }

    pub fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<ModelsApiStreamOptions>,
    ) -> AssistantMessageEventStream {
        self.runtime.stream(model, context, options)
    }

    pub fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<ModelsSimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        self.runtime.stream_simple(model, context, options)
    }

    pub async fn complete(
        &self,
        model: &Model,
        context: &Context,
        options: Option<ModelsApiStreamOptions>,
    ) -> AssistantMessage {
        self.runtime.complete(model, context, options).await
    }

    pub fn get_models_of_type(&self, model_type: ModelType, provider: Option<&str>) -> Vec<AnyModel> {
        self.runtime.get_models_of_type(model_type, provider)
    }

    pub async fn get_available_of_type(
        &self,
        model_type: ModelType,
        provider: Option<&str>,
        options: Option<AuthOperationOptions>,
    ) -> Result<Vec<AnyModel>> {
        self.runtime.get_available_of_type(model_type, provider, options).await
    }

    pub fn get_model_of_type(&self, model_type: ModelType, provider: &str, model_id: &str) -> Option<AnyModel> {
        self.runtime.get_model_of_type(model_type, provider, model_id)
    }

    pub async fn classify(
        &self,
        model: &ClassifierModel,
        context: &ClassifierContext,
        options: Option<ModelsClassifierOptions>,
    ) -> ClassifierResult {
        self.runtime.classify(model, context, options).await
    }

    pub async fn generate_images(
        &self,
        model: &ImageModel,
        context: &ImagesContext,
        options: Option<ModelsImagesOptions>,
    ) -> AssistantImages {
        self.runtime.generate_images(model, context, options).await
    }

    pub fn get_provider_display_name(&self, provider: &str) -> String {
        self.runtime
            .get_provider(provider)
            .map(|p| p.name().to_owned())
            .unwrap_or_else(|| provider.to_owned())
    }

    pub async fn get_provider_auth(&self, provider: &str) -> Result<Option<AuthResult>> {
        self.runtime.get_auth_provider_id(provider, None).await
    }

    pub async fn get_api_key_for_provider(&self, provider: &str) -> Option<String> {
        self.runtime
            .get_auth_provider_id(provider, None)
            .await
            .ok()
            .flatten()
            .and_then(|res| res.auth.api_key)
    }

    pub fn is_using_oauth(&self, model: &Model) -> bool {
        self.runtime.is_using_oauth(&model.provider)
    }

    pub fn register_native_provider(&self, provider: Arc<dyn Provider + Send + Sync>) -> Result<()> {
        self.runtime.register_native_provider(provider)
    }

    pub fn register_provider(&self, provider_name: &str, config: ProviderConfigInput) -> Result<()> {
        self.runtime.register_provider(provider_name, config)
    }

    pub fn register(&self, arg: RegisterProviderArg) -> Result<()> {
        match arg {
            RegisterProviderArg::Native(provider) => self.runtime.register_native_provider(provider),
            RegisterProviderArg::Config { name, config } => self.runtime.register_provider(&name, config),
        }
    }

    pub fn unregister_provider(&self, provider_name: &str) {
        self.runtime.unregister_provider(provider_name);
    }

    pub fn register_virtual_model(&self, definition: VirtualModelDefinition) -> Result<()> {
        self.runtime.register_virtual_model(definition)
    }

    pub fn unregister_virtual_model(&self, provider_name: &str, id: &str) {
        self.runtime.unregister_virtual_model(provider_name, id);
    }

    pub fn get_registered_provider_config(&self, provider_name: &str) -> Option<ProviderConfigInput> {
        self.runtime.get_registered_provider_config(provider_name)
    }

    pub fn get_registered_native_provider(&self, provider_name: &str) -> Option<Arc<dyn Provider + Send + Sync>> {
        self.runtime.get_registered_native_provider(provider_name)
    }

    pub fn get_registered_provider_ids(&self) -> Vec<String> {
        self.runtime.get_registered_provider_ids()
    }
}
