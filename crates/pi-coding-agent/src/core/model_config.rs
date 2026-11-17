//! Port of packages/coding-agent/src/core/model-config.ts

use std::sync::{Arc, LazyLock};

use indexmap::IndexMap;
use pi_ai::{ModelCost, ModelCostTier, ModelInput, ModelInputLimits, ModelPromptCache, ThinkingLevelMap};
use pi_js::vendor::typebox::{Schema, Validator, ValueError, compile, t};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::utils::json::strip_json_comments;
use crate::utils::paths::normalize_path;
use crate::utils::text::strip_bom;

static MODELS_CONFIG_VALIDATOR: LazyLock<Validator> = LazyLock::new(|| compile(&models_config_schema()));

fn models_config_schema() -> Schema {
    let percentile_cutoffs = t::object([
        ("p50", t::number().optional()),
        ("p75", t::number().optional()),
        ("p90", t::number().optional()),
        ("p99", t::number().optional()),
    ]);

    let open_router_routing = t::object([
        ("allow_fallbacks", t::boolean().optional()),
        ("require_parameters", t::boolean().optional()),
        (
            "data_collection",
            t::union([t::literal("deny"), t::literal("allow")]).optional(),
        ),
        ("zdr", t::boolean().optional()),
        ("enforce_distillable_text", t::boolean().optional()),
        ("order", t::array(t::string()).optional()),
        ("only", t::array(t::string()).optional()),
        ("ignore", t::array(t::string()).optional()),
        ("quantizations", t::array(t::string()).optional()),
        (
            "sort",
            t::union([
                t::string(),
                t::object([
                    ("by", t::string().optional()),
                    ("partition", t::union([t::string(), t::null()]).optional()),
                ]),
            ])
            .optional(),
        ),
        (
            "max_price",
            t::object([
                ("prompt", t::union([t::number(), t::string()]).optional()),
                ("completion", t::union([t::number(), t::string()]).optional()),
                ("image", t::union([t::number(), t::string()]).optional()),
                ("audio", t::union([t::number(), t::string()]).optional()),
                ("request", t::union([t::number(), t::string()]).optional()),
            ])
            .optional(),
        ),
        (
            "preferred_min_throughput",
            t::union([t::number(), percentile_cutoffs.clone()]).optional(),
        ),
        (
            "preferred_max_latency",
            t::union([t::number(), percentile_cutoffs]).optional(),
        ),
    ]);

    let vercel_gateway_routing = t::object([
        ("only", t::array(t::string()).optional()),
        ("order", t::array(t::string()).optional()),
    ]);

    let thinking_level_map_value = t::union([t::string(), t::null()]);
    let thinking_level_map = t::object([
        ("off", thinking_level_map_value.clone().optional()),
        ("minimal", thinking_level_map_value.clone().optional()),
        ("low", thinking_level_map_value.clone().optional()),
        ("medium", thinking_level_map_value.clone().optional()),
        ("high", thinking_level_map_value.clone().optional()),
        ("xhigh", thinking_level_map_value.clone().optional()),
        ("max", thinking_level_map_value.optional()),
    ]);

    let chat_template_kwarg_scalar = t::union([t::string(), t::number(), t::boolean(), t::null()]);
    let chat_template_kwarg_var = t::object([
        (
            "$var",
            t::union([t::literal("thinking.enabled"), t::literal("thinking.effort")]),
        ),
        ("omitWhenOff", t::boolean().optional()),
    ]);
    let chat_template_kwarg = t::union([chat_template_kwarg_scalar, chat_template_kwarg_var]);

    let session_affinity_format = t::union([
        t::literal("openai"),
        t::literal("openai-nosession"),
        t::literal("openrouter"),
    ]);

    let openai_completions_compat = t::object([
        ("supportsStore", t::boolean().optional()),
        ("supportsDeveloperRole", t::boolean().optional()),
        ("supportsReasoningEffort", t::boolean().optional()),
        ("supportsUsageInStreaming", t::boolean().optional()),
        ("supportsFinishReason", t::boolean().optional()),
        (
            "maxTokensField",
            t::union([t::literal("max_completion_tokens"), t::literal("max_tokens")]).optional(),
        ),
        ("requiresToolResultName", t::boolean().optional()),
        ("requiresAssistantAfterToolResult", t::boolean().optional()),
        ("requiresThinkingAsText", t::boolean().optional()),
        ("requiresReasoningContentOnAssistantMessages", t::boolean().optional()),
        (
            "thinkingFormat",
            t::union([
                t::literal("openai"),
                t::literal("openrouter"),
                t::literal("together"),
                t::literal("baseten"),
                t::literal("deepseek"),
                t::literal("zai"),
                t::literal("qwen"),
                t::literal("chat-template"),
                t::literal("qwen-chat-template"),
                t::literal("string-thinking"),
                t::literal("ant-ling"),
            ])
            .optional(),
        ),
        (
            "chatTemplateKwargs",
            t::record(t::string(), chat_template_kwarg.clone()).optional(),
        ),
        (
            "chatTemplateArgs",
            t::record(t::string(), chat_template_kwarg).optional(),
        ),
        ("cacheControlFormat", t::literal("anthropic").optional()),
        ("openRouterRouting", open_router_routing.optional()),
        ("vercelGatewayRouting", vercel_gateway_routing.optional()),
        ("supportsOpenAIGrammarTools", t::boolean().optional()),
        ("supportsStrictMode", t::boolean().optional()),
        ("sendSessionAffinityHeaders", t::boolean().optional()),
        ("sessionAffinityFormat", session_affinity_format.clone().optional()),
        ("supportsLongCacheRetention", t::boolean().optional()),
        ("vllmPriority", t::number().optional()),
    ]);

    let openai_responses_compat = t::object([
        ("supportsDeveloperRole", t::boolean().optional()),
        ("sessionAffinityFormat", session_affinity_format.optional()),
        ("supportsLongCacheRetention", t::boolean().optional()),
        ("supportsStrictMode", t::boolean().optional()),
        ("supportsOpenAIGrammarTools", t::boolean().optional()),
        ("supportsMaxOutputTokens", t::boolean().optional()),
    ]);

    let model_cost_tier = t::object([
        ("inputTokensAbove", t::number()),
        ("input", t::number()),
        ("output", t::number()),
        ("cacheRead", t::number()),
        ("cacheWrite", t::number()),
    ]);

    let model_cost = t::object([
        ("input", t::number()),
        ("output", t::number()),
        ("cacheRead", t::number()),
        ("cacheWrite", t::number()),
        ("tiers", t::array(model_cost_tier.clone()).optional()),
    ]);

    let model_prompt_cache = t::object([
        ("short", t::number().with("exclusiveMinimum", 0).optional()),
        ("long", t::number().with("exclusiveMinimum", 0).optional()),
    ]);

    let image_resize = t::object([
        ("maxWidth", t::integer().with("minimum", 1).optional()),
        ("maxHeight", t::integer().with("minimum", 1).optional()),
        ("maxBytes", t::integer().with("minimum", 1).optional()),
        (
            "jpegQuality",
            t::integer().with("minimum", 1).with("maximum", 100).optional(),
        ),
    ]);

    let model_input_limits = t::object([
        ("maxRequestBytes", t::integer().with("minimum", 1).optional()),
        (
            "images",
            t::object([
                ("resize", image_resize.optional()),
                ("maxPerMessage", t::integer().with("minimum", 1).optional()),
                ("maxPerRequest", t::integer().with("minimum", 1).optional()),
            ])
            .optional(),
        ),
    ]);

    let anthropic_messages_compat = t::object([
        ("supportsEagerToolInputStreaming", t::boolean().optional()),
        ("supportsLongCacheRetention", t::boolean().optional()),
        ("sendSessionAffinityHeaders", t::boolean().optional()),
        ("supportsCacheControlOnTools", t::boolean().optional()),
        ("supportsTemperature", t::boolean().optional()),
        ("forceAdaptiveThinking", t::boolean().optional()),
        ("allowEmptySignature", t::boolean().optional()),
        ("supportsStrictTools", t::boolean().optional()),
        ("supportsMidConvoEffort", t::boolean().optional()),
        (
            "allowedFallbackModels",
            t::array(t::object([
                ("provider", t::string().with("minLength", 1)),
                ("model", t::string().with("minLength", 1)),
                ("cost", model_cost.clone()),
            ]))
            .with("maxItems", 3)
            .optional(),
        ),
    ]);

    let provider_compat = t::union([
        openai_completions_compat,
        openai_responses_compat,
        anthropic_messages_compat,
    ]);

    let model_input = t::array(t::union([t::literal("text"), t::literal("image")]));

    let model_definition = t::object([
        ("id", t::string().with("minLength", 1)),
        ("name", t::string().with("minLength", 1).optional()),
        ("api", t::string().with("minLength", 1).optional()),
        ("baseUrl", t::string().with("minLength", 1).optional()),
        ("reasoning", t::boolean().optional()),
        ("thinkingLevelMap", thinking_level_map.clone().optional()),
        ("input", model_input.clone().optional()),
        ("inputLimits", model_input_limits.clone().optional()),
        ("cost", model_cost.optional()),
        ("promptCache", model_prompt_cache.clone().optional()),
        ("contextWindow", t::number().optional()),
        ("maxTokens", t::number().optional()),
        ("samplingParams", t::record(t::string(), t::unknown()).optional()),
        ("headers", t::record(t::string(), t::string()).optional()),
        ("compat", provider_compat.clone().optional()),
    ]);

    let model_override = t::object([
        ("name", t::string().with("minLength", 1).optional()),
        ("reasoning", t::boolean().optional()),
        ("thinkingLevelMap", thinking_level_map.optional()),
        ("input", model_input.optional()),
        ("inputLimits", model_input_limits.optional()),
        (
            "cost",
            t::object([
                ("input", t::number().optional()),
                ("output", t::number().optional()),
                ("cacheRead", t::number().optional()),
                ("cacheWrite", t::number().optional()),
                ("tiers", t::array(model_cost_tier).optional()),
            ])
            .optional(),
        ),
        ("promptCache", model_prompt_cache.optional()),
        ("contextWindow", t::number().optional()),
        ("maxTokens", t::number().optional()),
        ("samplingParams", t::record(t::string(), t::unknown()).optional()),
        ("headers", t::record(t::string(), t::string()).optional()),
        ("compat", provider_compat.clone().optional()),
    ]);

    let provider_config = t::object([
        ("name", t::string().with("minLength", 1).optional()),
        ("baseUrl", t::string().with("minLength", 1).optional()),
        ("apiKey", t::string().with("minLength", 1).optional()),
        ("api", t::string().with("minLength", 1).optional()),
        ("oauth", t::literal("radius").optional()),
        ("headers", t::record(t::string(), t::string()).optional()),
        ("compat", provider_compat.optional()),
        ("authHeader", t::boolean().optional()),
        ("models", t::array(model_definition).optional()),
        ("modelOverrides", t::record(t::string(), model_override).optional()),
    ]);

    t::object([("providers", t::record(t::string(), provider_config))])
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
    pub providers: IndexMap<String, ModelsJsonProvider>,
}

fn format_validation_path(error: &ValueError) -> String {
    if error.keyword == "required" {
        if let Some(required_property) = error
            .params
            .get("requiredProperties")
            .and_then(Value::as_array)
            .and_then(|arr| arr.first())
            .and_then(Value::as_str)
        {
            let trimmed = error.instance_path.strip_prefix('/').unwrap_or(&error.instance_path);
            let base_path = trimmed.replace('/', ".");
            return if base_path.is_empty() {
                required_property.to_owned()
            } else {
                format!("{base_path}.{required_property}")
            };
        }
    }
    let trimmed = error.instance_path.strip_prefix('/').unwrap_or(&error.instance_path);
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
