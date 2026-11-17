//! Port of packages/evals/evals/configured-runtime.ts

use pi_ai::{
    Context, Model, ModelCostRates, ModelInput, ModelsRefreshOptions, ModelsSimpleStreamOptions, content_text,
};
use pi_coding_agent::core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use pi_js::Result;
use serde::{Deserialize, Serialize};

/// Normalized subset of model fields inspected by provider configuration evals.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelFields {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub reasoning: bool,
    pub input: Vec<ModelInput>,
    pub cost: ModelCostRates,
    pub context_window: i64,
    pub max_tokens: i64,
}

/// Completion probe summary extracted from `ModelRuntime::complete_simple`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeResponseFields {
    pub text: String,
    pub stop_reason: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

/// Structured result payload for [`inspect_provider`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProviderRuntimeResult {
    Success {
        #[serde(rename = "validRequestReceived")]
        valid_request_received: bool,
        model: ModelFields,
        response: ProbeResponseFields,
    },
    Error {
        error: String,
    },
}

/// Top-level output returned by [`inspect_provider`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderRuntimeOutput {
    pub result: ProviderRuntimeResult,
}

/// Structured result payload for [`inspect_added_model`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AddedModelResult {
    Success {
        model: ModelFields,
        #[serde(rename = "existingModelsPreserved")]
        existing_models_preserved: bool,
    },
    Error {
        error: String,
    },
}

/// Top-level output returned by [`inspect_added_model`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AddedModelOutput {
    pub result: AddedModelResult,
}

/// Probe scenario evaluated by [`inspect_provider`].
pub struct ProviderScenario {
    pub provider_id: String,
    pub model_id: String,
    pub create_context: Box<dyn Fn() -> Context + Send + Sync>,
    pub options: Option<ModelsSimpleStreamOptions>,
    pub valid_request_received: Box<dyn Fn() -> bool + Send + Sync>,
}

fn model_fields(model: &Model) -> ModelFields {
    ModelFields {
        id: model.id.clone(),
        name: model.name.clone(),
        provider: model.provider.clone(),
        reasoning: model.reasoning,
        input: model.input.clone(),
        cost: model.cost.rates(),
        context_window: model.context_window,
        max_tokens: model.max_tokens,
    }
}

/// Loads an offline [`ModelRuntime`] configured from an isolated agent directory.
pub async fn load_configured_model_runtime(agent_dir: &str) -> Result<ModelRuntime> {
    ModelRuntime::create(CreateModelRuntimeOptions {
        models_path: Some(Some(pi_js::path::join(&[agent_dir, "models.json"]))),
        auth_path: Some(pi_js::path::join(&[agent_dir, "auth.json"])),
        models_store_path: Some(pi_js::path::join(&[agent_dir, "models-store.json"])),
        allow_model_network: Some(false),
        ..Default::default()
    })
    .await
}

/// Refreshes `runtime`, checks configuration health, and runs a completion probe for `scenario`.
pub async fn inspect_provider(runtime: &ModelRuntime, scenario: ProviderScenario) -> Result<ProviderRuntimeOutput> {
    runtime
        .refresh(Some(ModelsRefreshOptions {
            allow_network: Some(false),
            ..Default::default()
        }))
        .await?;
    if let Some(config_error) = runtime.get_error() {
        return Ok(ProviderRuntimeOutput {
            result: ProviderRuntimeResult::Error { error: config_error },
        });
    }
    let Some(model) = runtime.get_model(&scenario.provider_id, &scenario.model_id) else {
        return Ok(ProviderRuntimeOutput {
            result: ProviderRuntimeResult::Error {
                error: format!(
                    "Model {}/{} is unavailable after reload.",
                    scenario.provider_id, scenario.model_id
                ),
            },
        });
    };

    let ctx = (scenario.create_context)();
    let response = runtime.complete_simple(&model, &ctx, scenario.options).await;
    Ok(ProviderRuntimeOutput {
        result: ProviderRuntimeResult::Success {
            valid_request_received: (scenario.valid_request_received)(),
            model: model_fields(&model),
            response: ProbeResponseFields {
                text: content_text(&response.content, None),
                stop_reason: response.stop_reason.as_str().to_string(),
                input_tokens: response.usage.input,
                output_tokens: response.usage.output,
            },
        },
    })
}

/// Verifies that an added model is present after reload while preserving all built-in provider models.
pub async fn inspect_added_model(
    runtime: &ModelRuntime,
    provider_id: &str,
    model_id: &str,
) -> Result<AddedModelOutput> {
    let pristine_runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        models_path: Some(None),
        allow_model_network: Some(false),
        ..Default::default()
    })
    .await?;
    let Some(pristine_provider) = pristine_runtime.get_provider(provider_id) else {
        return Ok(AddedModelOutput {
            result: AddedModelResult::Error {
                error: format!("Built-in provider {provider_id} is unavailable."),
            },
        });
    };
    let existing_model_ids: Vec<String> = pristine_provider
        .get_models()
        .into_iter()
        .map(|model| model.id)
        .collect();
    if existing_model_ids.is_empty() {
        return Ok(AddedModelOutput {
            result: AddedModelResult::Error {
                error: format!("Built-in provider {provider_id} has no models."),
            },
        });
    }

    runtime
        .refresh(Some(ModelsRefreshOptions {
            allow_network: Some(false),
            ..Default::default()
        }))
        .await?;
    if let Some(config_error) = runtime.get_error() {
        return Ok(AddedModelOutput {
            result: AddedModelResult::Error { error: config_error },
        });
    }
    let Some(model) = runtime.get_model(provider_id, model_id) else {
        return Ok(AddedModelOutput {
            result: AddedModelResult::Error {
                error: format!("Model {provider_id}/{model_id} is unavailable after reload."),
            },
        });
    };

    let existing_models_preserved = existing_model_ids
        .iter()
        .all(|id| runtime.get_model(provider_id, id).is_some());
    Ok(AddedModelOutput {
        result: AddedModelResult::Success {
            model: model_fields(&model),
            existing_models_preserved,
        },
    })
}
