//! Port of packages/coding-agent/test/model-runtime-test-utils.ts

use std::sync::Arc;

use pi_ai::CredentialStore;
pub use pi_coding_agent::core::model_registry::{ModelRegistry, ResolvedRequestAuth};
pub use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
pub use pi_coding_agent::core::models_store::InMemoryCodingAgentModelsStore;
pub use pi_coding_agent::core::provider_composer::{ProviderChatModelConfig, ProviderConfigInput, ProviderModelConfig};
use pi_js::Result;

/// Load optional `models.json` configuration without introducing file-backed catalog locks into unit tests.
pub async fn create_model_registry(
    credentials: Arc<dyn CredentialStore + Send + Sync>,
    models_path: Option<&str>,
) -> Result<ModelRegistry> {
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(credentials),
        models_path: models_path.map(|p| Some(p.to_owned())),
        models_store: Some(Arc::new(InMemoryCodingAgentModelsStore::new())),
        allow_model_network: Some(false),
        ..Default::default()
    })
    .await?;
    Ok(ModelRegistry::new(runtime))
}

pub async fn create_in_memory_model_registry(
    credentials: Arc<dyn CredentialStore + Send + Sync>,
) -> Result<ModelRegistry> {
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(credentials),
        models_path: Some(None),
        allow_model_network: Some(false),
        ..Default::default()
    })
    .await?;
    Ok(ModelRegistry::new(runtime))
}

pub fn get_model_runtime(model_registry: &ModelRegistry) -> ModelRuntime {
    model_registry.runtime().clone()
}
