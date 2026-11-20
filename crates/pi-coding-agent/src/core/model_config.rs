//! Port of packages/coding-agent/src/core/model-config.ts

use std::sync::{Arc, LazyLock};

use indexmap::IndexMap;
use pi_ai::types::SamplingParamsByThinkingLevel;
use pi_ai::{ModelCost, ModelCostTier, ModelInput, ModelInputLimits, ModelPromptCache, ThinkingLevelMap};
use pi_schema::{
    CompileOptions, NumOptions, Schema, StrOptions, ValidationError, Validator, arr, bool_with, compile, map_with,
    num_with, obj_with, opt, str, str_with, unknown,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::core::pi_ai_schemas::{
    model_cost_schema, model_input_limits_schema, model_input_modality_schema, model_prompt_cache_schema,
    partial_model_cost_schema, provider_compat_schema, thinking_level_map_schema,
};
use crate::utils::json::strip_json_comments;
use crate::utils::paths::normalize_path;
use crate::utils::text::strip_bom;

static MODELS_CONFIG_VALIDATOR: LazyLock<Validator> =
    LazyLock::new(|| compile(models_config_schema(), CompileOptions::default()).expect("valid models config schema"));

fn sampling_params_schema() -> Schema {
    map_with(unknown(), pi_schema::Annotations::default())
}

fn sampling_params_by_thinking_level_schema() -> Schema {
    obj_with(
        [
            ("off", opt(sampling_params_schema())),
            ("minimal", opt(sampling_params_schema())),
            ("low", opt(sampling_params_schema())),
            ("medium", opt(sampling_params_schema())),
            ("high", opt(sampling_params_schema())),
            ("xhigh", opt(sampling_params_schema())),
            ("max", opt(sampling_params_schema())),
        ],
        pi_schema::ObjOptions::default(),
    )
    .description("samplingParams per thinking level, merged over samplingParams.")
}

fn headers_schema() -> Schema {
    map_with(
        str(),
        pi_schema::Annotations {
            description: Some("Request headers. Values resolve like apiKey.".to_owned()),
            ..Default::default()
        },
    )
}

fn non_empty_str(description: &str) -> Schema {
    str_with(StrOptions {
        annotations: pi_schema::Annotations {
            description: Some(description.to_owned()),
            ..Default::default()
        },
        min_length: Some(1),
        ..Default::default()
    })
}

pub fn model_definition_schema() -> Schema {
    obj_with(
        [
            ("id", non_empty_str("Model ID sent to the API.")),
            (
                "name",
                opt(non_empty_str("Display name, also matched by --model patterns.")),
            ),
            ("api", opt(non_empty_str("Overrides the provider api."))),
            ("baseUrl", opt(non_empty_str("Overrides the provider baseUrl."))),
            (
                "reasoning",
                opt(bool_with(pi_schema::Annotations {
                    description: Some("Whether the model supports thinking.".to_owned()),
                    ..Default::default()
                })
                .with("default", false)),
            ),
            ("thinkingLevelMap", opt(thinking_level_map_schema())),
            (
                "input",
                opt(arr(model_input_modality_schema()).with("default", json!(["text"]))),
            ),
            ("inputLimits", opt(model_input_limits_schema())),
            ("cost", opt(model_cost_schema())),
            ("promptCache", opt(model_prompt_cache_schema())),
            (
                "contextWindow",
                opt(num_with(NumOptions {
                    annotations: pi_schema::Annotations {
                        description: Some("Context window in tokens.".to_owned()),
                        ..Default::default()
                    },
                    exclusive_minimum: Some(0.0),
                    ..Default::default()
                })
                .with("default", 128_000)),
            ),
            (
                "maxTokens",
                opt(num_with(NumOptions {
                    annotations: pi_schema::Annotations {
                        description: Some("Maximum output tokens.".to_owned()),
                        ..Default::default()
                    },
                    exclusive_minimum: Some(0.0),
                    ..Default::default()
                })
                .with("default", 16_384)),
            ),
            (
                "samplingParams",
                opt(map_with(
                    unknown(),
                    pi_schema::Annotations {
                        description: Some(
                            "Fields merged into every request body. Applies only to OpenAI-compatible APIs.".to_owned(),
                        ),
                        ..Default::default()
                    },
                )),
            ),
            (
                "samplingParamsByThinkingLevel",
                opt(sampling_params_by_thinking_level_schema()),
            ),
            ("headers", opt(headers_schema())),
            ("compat", opt(provider_compat_schema())),
        ],
        pi_schema::ObjOptions::default(),
    )
}

pub fn model_override_schema() -> Schema {
    obj_with(
        [
            (
                "name",
                opt(non_empty_str("Display name, also matched by --model patterns.")),
            ),
            (
                "reasoning",
                opt(bool_with(pi_schema::Annotations {
                    description: Some("Whether the model supports thinking.".to_owned()),
                    ..Default::default()
                })),
            ),
            ("thinkingLevelMap", opt(thinking_level_map_schema())),
            ("input", opt(arr(model_input_modality_schema()))),
            ("inputLimits", opt(model_input_limits_schema())),
            ("cost", opt(partial_model_cost_schema())),
            ("promptCache", opt(model_prompt_cache_schema())),
            (
                "contextWindow",
                opt(num_with(NumOptions {
                    annotations: pi_schema::Annotations {
                        description: Some("Context window in tokens.".to_owned()),
                        ..Default::default()
                    },
                    exclusive_minimum: Some(0.0),
                    ..Default::default()
                })),
            ),
            (
                "maxTokens",
                opt(num_with(NumOptions {
                    annotations: pi_schema::Annotations {
                        description: Some("Maximum output tokens.".to_owned()),
                        ..Default::default()
                    },
                    exclusive_minimum: Some(0.0),
                    ..Default::default()
                })),
            ),
            (
                "samplingParams",
                opt(map_with(
                    unknown(),
                    pi_schema::Annotations {
                        description: Some(
                            "Fields merged into every request body. Applies only to OpenAI-compatible APIs.".to_owned(),
                        ),
                        ..Default::default()
                    },
                )),
            ),
            (
                "samplingParamsByThinkingLevel",
                opt(sampling_params_by_thinking_level_schema()),
            ),
            ("headers", opt(headers_schema())),
            ("compat", opt(provider_compat_schema())),
        ],
        pi_schema::ObjOptions::default(),
    )
}

pub fn provider_config_schema() -> Schema {
    obj_with(
        [
            ("name", opt(non_empty_str("Display name."))),
            (
                "baseUrl",
                opt(non_empty_str(
                    "API base URL. Also applies to the provider's built-in models.",
                )),
            ),
            (
                "apiKey",
                opt(non_empty_str(
                    "Literal key, \"$NAME\" environment interpolation, or \"!command\" output.",
                )),
            ),
            (
                "api",
                opt(non_empty_str(
                    "API for the provider's models, such as \"openai-completions\", \"openai-responses\", \"anthropic-messages\", or \"google-generative-ai\".",
                )),
            ),
            (
                "oauth",
                opt(pi_schema::lit("radius").description("OAuth login flow. Requires baseUrl.")),
            ),
            ("headers", opt(headers_schema())),
            ("compat", opt(provider_compat_schema())),
            (
                "authHeader",
                opt(bool_with(pi_schema::Annotations {
                    description: Some("Whether to send the resolved API key as Authorization: Bearer.".to_owned()),
                    ..Default::default()
                })
                .with("default", false)),
            ),
            ("models", opt(arr(model_definition_schema()))),
            (
                "modelOverrides",
                opt(map_with(model_override_schema(), pi_schema::Annotations::default())),
            ),
        ],
        pi_schema::ObjOptions::default(),
    )
}

pub fn models_config_schema() -> Schema {
    obj_with(
        [
            ("$schema", opt(str())),
            (
                "providers",
                map_with(provider_config_schema(), pi_schema::Annotations::default()),
            ),
        ],
        pi_schema::ObjOptions::default(),
    )
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsJsonModel {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level_map: Option<ThinkingLevelMap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Vec<ModelInput>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_limits: Option<ModelInputLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<ModelCost>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache: Option<ModelPromptCache>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling_params: Option<IndexMap<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling_params_by_thinking_level: Option<SamplingParamsByThinkingLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<IndexMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compat: Option<Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsJsonModelOverrideCost {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tiers: Option<Vec<ModelCostTier>>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsJsonModelOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level_map: Option<ThinkingLevelMap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Vec<ModelInput>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_limits: Option<ModelInputLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<ModelsJsonModelOverrideCost>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache: Option<ModelPromptCache>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling_params: Option<IndexMap<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling_params_by_thinking_level: Option<SamplingParamsByThinkingLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<IndexMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compat: Option<Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsJsonProvider {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<IndexMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compat: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_header: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<ModelsJsonModel>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_overrides: Option<IndexMap<String, ModelsJsonModelOverride>>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelsJson {
    #[serde(rename = "$schema", default)]
    pub schema: Option<String>,
    pub providers: IndexMap<String, ModelsJsonProvider>,
}

fn format_validation_path(error: &ValidationError) -> String {
    let trimmed = error.path.strip_prefix('/').unwrap_or(&error.path);
    let path = trimmed.replace('/', ".");
    if path.is_empty() { "root".to_owned() } else { path }
}

#[derive(Debug)]
struct ModelConfigInner {
    providers: IndexMap<String, ModelsJsonProvider>,
    error: Option<String>,
}

/// One immutable load of `models.json`.
#[derive(Clone, Debug)]
pub struct ModelConfig {
    inner: Arc<ModelConfigInner>,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self::empty()
    }
}

impl ModelConfig {
    pub fn empty() -> Self {
        Self::from_parts(IndexMap::new(), None)
    }

    pub fn from_parts(providers: IndexMap<String, ModelsJsonProvider>, error: Option<String>) -> Self {
        Self {
            inner: Arc::new(ModelConfigInner { providers, error }),
        }
    }

    pub async fn load(models_json_path: Option<&str>) -> Self {
        let Some(raw_path) = models_json_path else {
            return Self::empty();
        };
        let path = normalize_path(raw_path, Default::default());
        let content = match pi_js::fs::promises::read_to_string(&path).await {
            Ok(c) => c,
            Err(err) => {
                if err.code() == Some("ENOENT") {
                    return Self::empty();
                }
                return Self::from_parts(
                    IndexMap::new(),
                    Some(format!("Failed to load models.json: {}\n\nFile: {path}", err.message())),
                );
            }
        };

        let parsed = match pi_js::json::parse(&strip_json_comments(strip_bom(&content))) {
            Ok(v) => v,
            Err(err) => {
                return Self::from_parts(
                    IndexMap::new(),
                    Some(format!(
                        "Failed to parse models.json: {}\n\nFile: {path}",
                        err.message()
                    )),
                );
            }
        };

        if !MODELS_CONFIG_VALIDATOR.check(&parsed) {
            let formatted = MODELS_CONFIG_VALIDATOR
                .errors(&parsed)
                .into_iter()
                .map(|err| format!("  - {}: {}", format_validation_path(&err), err.message))
                .collect::<Vec<_>>()
                .join("\n");
            let errors = if formatted.is_empty() {
                "Unknown schema error".to_owned()
            } else {
                formatted
            };
            return Self::from_parts(
                IndexMap::new(),
                Some(format!("Invalid models.json schema:\n{errors}\n\nFile: {path}")),
            );
        }

        let config: ModelsJson = match serde_json::from_value(parsed) {
            Ok(cfg) => cfg,
            Err(err) => {
                return Self::from_parts(
                    IndexMap::new(),
                    Some(format!("Invalid models.json schema:\n  - root: {err}\n\nFile: {path}")),
                );
            }
        };

        Self::from_parts(config.providers, None)
    }

    pub fn get_provider(&self, provider_id: &str) -> Option<&ModelsJsonProvider> {
        self.inner.providers.get(provider_id)
    }

    pub fn get_provider_ids(&self) -> Vec<String> {
        self.inner.providers.keys().cloned().collect()
    }

    pub fn get_error(&self) -> Option<&str> {
        self.inner.error.as_deref()
    }
}
