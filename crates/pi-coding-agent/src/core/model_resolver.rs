//! Port of packages/coding-agent/src/core/model-resolver.ts

use std::sync::LazyLock;

use indexmap::IndexMap;
use pi_ai::{AuthOperationOptions, Model, ModelThinkingLevel, models_are_equal};
use pi_js::{Error, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::core::defaults::DEFAULT_THINKING_LEVEL;
use crate::core::model_registry::ModelRegistry;
use crate::core::model_runtime::ModelRuntime;
use crate::vendor::chalk;
use crate::vendor::minimatch::{MinimatchOptions, minimatch};

static DATE_SUFFIX_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"-\d{8}$").expect("valid regex"));

// WIRE(@ca-cli-w3): share with cli::args::is_valid_thinking_level once cli/args.rs lands
pub fn parse_valid_thinking_level(value: &str) -> Option<ModelThinkingLevel> {
    match value {
        "off" => Some(ModelThinkingLevel::Off),
        "minimal" => Some(ModelThinkingLevel::Minimal),
        "low" => Some(ModelThinkingLevel::Low),
        "medium" => Some(ModelThinkingLevel::Medium),
        "high" => Some(ModelThinkingLevel::High),
        "xhigh" => Some(ModelThinkingLevel::XHigh),
        "max" => Some(ModelThinkingLevel::Max),
        _ => None,
    }
}

pub fn is_valid_thinking_level(value: &str) -> bool {
    parse_valid_thinking_level(value).is_some()
}

/// Default chat model IDs for providers with built-in chat models, in priority order.
pub fn default_model_per_provider() -> &'static [(&'static str, &'static str)] {
    &[
        ("amazon-bedrock", "us.anthropic.claude-opus-4-6-v1"),
        ("ant-ling", "Ring-2.6-1T"),
        ("anthropic", "claude-opus-4-8"),
        ("openai", "gpt-5.5"),
        ("azure-openai-responses", "gpt-5.4"),
        ("openai-codex", "gpt-6.1-sol"),
        ("radius", "balanced"),
        ("nvidia", "nvidia/nemotron-3-super-120b-a12b"),
        ("deepseek", "deepseek-v4-pro"),
        ("google", "gemini-3.1-pro-preview"),
        ("google-vertex", "gemini-3.1-pro-preview"),
        ("github-copilot", "gpt-5.4"),
        ("openrouter", "moonshotai/kimi-k2.6"),
        ("vercel-ai-gateway", "zai/glm-5.1"),
        ("xai", "grok-4.7"),
        ("groq", "openai/gpt-oss-120b"),
        ("cerebras", "gpt-oss-120b"),
        ("zai", "glm-5.3"),
        ("zai-coding-cn", "glm-5.3"),
        ("mistral", "devstral-medium-latest"),
        ("minimax", "MiniMax-M2.7"),
        ("minimax-cn", "MiniMax-M2.7"),
        ("moonshotai", "kimi-k2.6"),
        ("moonshotai-cn", "kimi-k2.6"),
        ("huggingface", "moonshotai/Kimi-K2.6"),
        ("fireworks", "accounts/fireworks/models/kimi-k3"),
        ("together", "moonshotai/Kimi-K3"),
        ("baseten", "zai-org/GLM-5.2"),
        ("opencode", "kimi-k2.6"),
        ("opencode-go", "kimi-k3"),
        ("kimi-coding", "kimi-for-coding"),
        ("meta", "muse-spark-1.3"),
        ("cloudflare-workers-ai", "@cf/moonshotai/kimi-k2.6"),
        ("cloudflare-ai-gateway", "workers-ai/@cf/moonshotai/kimi-k2.6"),
        ("qwen-token-plan", "qwen3.7-max"),
        ("qwen-token-plan-cn", "qwen3.7-max"),
        ("qwen-token-plan-individual", "qwen3.8-max"),
        ("xiaomi", "mimo-v2.5-pro"),
        ("xiaomi-token-plan-cn", "mimo-v2.5-pro"),
        ("xiaomi-token-plan-ams", "mimo-v2.5-pro"),
        ("xiaomi-token-plan-sgp", "mimo-v2.5-pro"),
    ]
}

pub fn get_default_model_for_provider(provider: &str) -> Option<&'static str> {
    default_model_per_provider()
        .iter()
        .find(|(p, _)| *p == provider)
        .map(|(_, id)| *id)
}

#[async_trait::async_trait]
pub trait ModelResolverRuntime: Send + Sync {
    fn get_models(&self) -> Vec<Model>;
    fn get_model(&self, provider_id: &str, model_id: &str) -> Option<Model>;
    fn get_available_snapshot(&self) -> Vec<Model>;
    fn has_configured_auth(&self, provider_id: &str) -> bool;
    async fn get_available(
        &self,
        provider_id: Option<&str>,
        options: Option<AuthOperationOptions>,
    ) -> Result<Vec<Model>>;
}

#[async_trait::async_trait]
impl ModelResolverRuntime for ModelRuntime {
    fn get_models(&self) -> Vec<Model> {
        self.get_models(None)
    }

    fn get_model(&self, provider_id: &str, model_id: &str) -> Option<Model> {
        self.get_model(provider_id, model_id)
    }

    fn get_available_snapshot(&self) -> Vec<Model> {
        self.get_available_snapshot()
    }

    fn has_configured_auth(&self, provider_id: &str) -> bool {
        self.has_configured_auth(provider_id)
    }

    async fn get_available(
        &self,
        provider_id: Option<&str>,
        options: Option<AuthOperationOptions>,
    ) -> Result<Vec<Model>> {
        self.get_available(provider_id, options).await
    }
}

#[async_trait::async_trait]
impl ModelResolverRuntime for ModelRegistry {
    fn get_models(&self) -> Vec<Model> {
        self.get_all()
    }

    fn get_model(&self, provider_id: &str, model_id: &str) -> Option<Model> {
        self.find(provider_id, model_id)
    }

    fn get_available_snapshot(&self) -> Vec<Model> {
        self.get_available()
    }

    fn has_configured_auth(&self, provider_id: &str) -> bool {
        self.runtime().has_configured_auth(provider_id)
    }

    async fn get_available(
        &self,
        provider_id: Option<&str>,
        options: Option<AuthOperationOptions>,
    ) -> Result<Vec<Model>> {
        self.runtime().get_available(provider_id, options).await
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopedModel {
    pub model: Model,
    pub thinking_level: Option<ModelThinkingLevel>,
}

fn is_alias(id: &str) -> bool {
    if id.ends_with("-latest") {
        return true;
    }
    !DATE_SUFFIX_RE.is_match(id)
}

/// Find an exact model reference match (bare model id or `provider/modelId`).
pub fn find_exact_model_reference_match(model_reference: &str, available_models: &[Model]) -> Option<Model> {
    let trimmed_reference = pi_js::str16::trim(model_reference);
    if trimmed_reference.is_empty() {
        return None;
    }
    let normalized_reference = trimmed_reference.to_ascii_lowercase();

    let canonical_matches: Vec<&Model> = available_models
        .iter()
        .filter(|m| format!("{}/{}", m.provider, m.id).to_ascii_lowercase() == normalized_reference)
        .collect();
    if canonical_matches.len() == 1 {
        return Some(canonical_matches[0].clone());
    }
    if canonical_matches.len() > 1 {
        return None;
    }

    if let Some(slash_index) = trimmed_reference.find('/') {
        let provider = pi_js::str16::trim(&trimmed_reference[..slash_index]);
        let model_id = pi_js::str16::trim(&trimmed_reference[slash_index + 1..]);
        if !provider.is_empty() && !model_id.is_empty() {
            let prov_lower = provider.to_ascii_lowercase();
            let id_lower = model_id.to_ascii_lowercase();
            let provider_matches: Vec<&Model> = available_models
                .iter()
                .filter(|m| m.provider.to_ascii_lowercase() == prov_lower && m.id.to_ascii_lowercase() == id_lower)
                .collect();
            if provider_matches.len() == 1 {
                return Some(provider_matches[0].clone());
            }
            if provider_matches.len() > 1 {
                return None;
            }
        }
    }

    let id_matches: Vec<&Model> = available_models
        .iter()
        .filter(|m| m.id.to_ascii_lowercase() == normalized_reference)
        .collect();
    if id_matches.len() == 1 {
        Some(id_matches[0].clone())
    } else {
        None
    }
}

fn try_match_model(model_pattern: &str, available_models: &[Model]) -> Option<Model> {
    if let Some(exact) = find_exact_model_reference_match(model_pattern, available_models) {
        return Some(exact);
    }

    let pat_lower = model_pattern.to_ascii_lowercase();
    let matches: Vec<&Model> = available_models
        .iter()
        .filter(|m| m.id.to_ascii_lowercase().contains(&pat_lower) || m.name.to_ascii_lowercase().contains(&pat_lower))
        .collect();
    if matches.is_empty() {
        return None;
    }

    let mut aliases: Vec<&Model> = matches.iter().copied().filter(|m| is_alias(&m.id)).collect();
    let mut dated_versions: Vec<&Model> = matches.iter().copied().filter(|m| !is_alias(&m.id)).collect();

    if !aliases.is_empty() {
        aliases.sort_by(|a, b| b.id.cmp(&a.id));
        Some(aliases[0].clone())
    } else {
        dated_versions.sort_by(|a, b| b.id.cmp(&a.id));
        Some(dated_versions[0].clone())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParsedModelResult {
    pub model: Option<Model>,
    pub thinking_level: Option<ModelThinkingLevel>,
    pub warning: Option<String>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ParseModelPatternOptions {
    pub allow_invalid_thinking_level_fallback: Option<bool>,
}

fn build_fallback_model(provider: &str, model_id: &str, available_models: &[Model]) -> Option<Model> {
    let provider_models: Vec<&Model> = available_models.iter().filter(|m| m.provider == provider).collect();
    if provider_models.is_empty() {
        return None;
    }
    let default_id = get_default_model_for_provider(provider);
    let base_model = default_id
        .and_then(|id| provider_models.iter().copied().find(|m| m.id == id))
        .unwrap_or(provider_models[0]);

    let mut out = base_model.clone();
    out.id = model_id.to_owned();
    out.name = model_id.to_owned();
    Some(out)
}

pub fn parse_model_pattern(
    pattern: &str,
    available_models: &[Model],
    options: Option<ParseModelPatternOptions>,
) -> ParsedModelResult {
    if let Some(exact) = try_match_model(pattern, available_models) {
        return ParsedModelResult {
            model: Some(exact),
            thinking_level: None,
            warning: None,
        };
    }

    let Some(last_colon) = pattern.rfind(':') else {
        return ParsedModelResult {
            model: None,
            thinking_level: None,
            warning: None,
        };
    };

    let prefix = &pattern[..last_colon];
    let suffix = &pattern[last_colon + 1..];

    if let Some(level) = parse_valid_thinking_level(suffix) {
        let result = parse_model_pattern(prefix, available_models, options);
        if result.model.is_some() {
            let thinking = if result.warning.is_some() { None } else { Some(level) };
            return ParsedModelResult {
                model: result.model,
                thinking_level: thinking,
                warning: result.warning,
            };
        }
        return result;
    }

    let allow_fallback = options
        .and_then(|o| o.allow_invalid_thinking_level_fallback)
        .unwrap_or(true);
    if !allow_fallback {
        return ParsedModelResult {
            model: None,
            thinking_level: None,
            warning: None,
        };
    }

    let result = parse_model_pattern(prefix, available_models, options);
    if result.model.is_some() {
        return ParsedModelResult {
            model: result.model,
            thinking_level: None,
            warning: Some(format!(
                "Invalid thinking level \"{suffix}\" in pattern \"{pattern}\". Using default instead."
            )),
        };
    }
    result
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelScopeDiagnosticCode {
    NoMatch,
    InvalidThinkingLevel,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelScopeDiagnostic {
    #[serde(rename = "type")]
    pub diagnostic_type: String,
    pub code: ModelScopeDiagnosticCode,
    pub message: String,
    pub pattern: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResolveModelScopeResult {
    pub scoped_models: Vec<ScopedModel>,
    pub diagnostics: Vec<ModelScopeDiagnostic>,
}

fn push_unique_scoped(scoped_models: &mut Vec<ScopedModel>, model: Model, thinking_level: Option<ModelThinkingLevel>) {
    let any_new = pi_ai::AnyModel::Chat(model.clone());
    if !scoped_models
        .iter()
        .any(|sm| models_are_equal(Some(&pi_ai::AnyModel::Chat(sm.model.clone())), Some(&any_new)))
    {
        scoped_models.push(ScopedModel { model, thinking_level });
    }
}

pub fn resolve_model_scope_from_models(patterns: &[String], models: &[Model]) -> ResolveModelScopeResult {
    let mut scoped_models: Vec<ScopedModel> = Vec::new();
    let mut diagnostics: Vec<ModelScopeDiagnostic> = Vec::new();

    for pattern in patterns {
        if pattern.contains('*') || pattern.contains('?') || pattern.contains('[') {
            let mut glob_pattern = pattern.as_str();
            let mut thinking_level = None;

            if let Some(colon_idx) = pattern.rfind(':') {
                let suffix = &pattern[colon_idx + 1..];
                if let Some(level) = parse_valid_thinking_level(suffix) {
                    thinking_level = Some(level);
                    glob_pattern = &pattern[..colon_idx];
                }
            }

            if let Some(exact) = find_exact_model_reference_match(glob_pattern, models) {
                push_unique_scoped(&mut scoped_models, exact, thinking_level);
                continue;
            }

            let mm_opts = Some(MinimatchOptions {
                nocase: true,
                ..Default::default()
            });
            let matching_models: Vec<Model> = models
                .iter()
                .filter(|m| {
                    let full_id = format!("{}/{}", m.provider, m.id);
                    minimatch(&full_id, glob_pattern, mm_opts.clone())
                        || minimatch(&m.id, glob_pattern, mm_opts.clone())
                })
                .cloned()
                .collect();

            if matching_models.is_empty() {
                diagnostics.push(ModelScopeDiagnostic {
                    diagnostic_type: "warning".to_owned(),
                    code: ModelScopeDiagnosticCode::NoMatch,
                    message: format!("No models match pattern \"{pattern}\""),
                    pattern: pattern.clone(),
                });
                continue;
            }

            for model in matching_models {
                push_unique_scoped(&mut scoped_models, model, thinking_level);
            }
            continue;
        }

        let parsed = parse_model_pattern(pattern, models, None);
        if let Some(warning) = parsed.warning {
            diagnostics.push(ModelScopeDiagnostic {
                diagnostic_type: "warning".to_owned(),
                code: ModelScopeDiagnosticCode::InvalidThinkingLevel,
                message: warning,
                pattern: pattern.clone(),
            });
        }
        let Some(model) = parsed.model else {
            diagnostics.push(ModelScopeDiagnostic {
                diagnostic_type: "warning".to_owned(),
                code: ModelScopeDiagnosticCode::NoMatch,
                message: format!("No models match pattern \"{pattern}\""),
                pattern: pattern.clone(),
            });
            continue;
        };
        push_unique_scoped(&mut scoped_models, model, parsed.thinking_level);
    }

    ResolveModelScopeResult {
        scoped_models,
        diagnostics,
    }
}

pub async fn resolve_model_scope_with_diagnostics(
    patterns: &[String],
    model_runtime: &dyn ModelResolverRuntime,
    options: Option<AuthOperationOptions>,
) -> Result<ResolveModelScopeResult> {
    let available = model_runtime.get_available(None, options).await?;
    Ok(resolve_model_scope_from_models(patterns, &available))
}

pub async fn resolve_model_scope(
    patterns: &[String],
    model_runtime: &dyn ModelResolverRuntime,
    options: Option<AuthOperationOptions>,
) -> Result<Vec<ScopedModel>> {
    let result = resolve_model_scope_with_diagnostics(patterns, model_runtime, options).await?;
    for diagnostic in &result.diagnostics {
        pi_js::console::warn(&chalk::yellow(&format!("Warning: {}", diagnostic.message)));
    }
    Ok(result.scoped_models)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResolveCliModelResult {
    pub model: Option<Model>,
    pub thinking_level: Option<ModelThinkingLevel>,
    pub warning: Option<String>,
    pub error: Option<String>,
}

pub struct ResolveCliModelOptions<'a> {
    pub cli_provider: Option<&'a str>,
    pub cli_model: Option<&'a str>,
    pub cli_thinking: Option<ModelThinkingLevel>,
    pub model_runtime: &'a dyn ModelResolverRuntime,
}

pub fn resolve_cli_model(options: ResolveCliModelOptions<'_>) -> ResolveCliModelResult {
    let Some(cli_model) = options.cli_model else {
        return ResolveCliModelResult::default();
    };

    let available_models = options.model_runtime.get_models();
    if available_models.is_empty() {
        return ResolveCliModelResult {
            error: Some("No models available. Check your installation or add models to models.json.".to_owned()),
            ..Default::default()
        };
    }

    let mut provider_map: IndexMap<String, String> = IndexMap::new();
    for m in &available_models {
        provider_map.insert(m.provider.to_ascii_lowercase(), m.provider.clone());
    }

    let mut provider: Option<String> = options
        .cli_provider
        .and_then(|p| provider_map.get(&p.to_ascii_lowercase()).cloned());
    if let Some(cli_prov) = options.cli_provider
        && provider.is_none()
    {
        return ResolveCliModelResult {
            error: Some(format!(
                "Unknown provider \"{cli_prov}\". Use --list-models to see available providers/models."
            )),
            ..Default::default()
        };
    }

    let mut pattern = cli_model.to_owned();
    let mut inferred_provider = false;

    if provider.is_none()
        && let Some(slash_idx) = cli_model.find('/')
    {
        let maybe_provider = &cli_model[..slash_idx];
        if let Some(canonical) = provider_map.get(&maybe_provider.to_ascii_lowercase()) {
            provider = Some(canonical.clone());
            pattern = cli_model[slash_idx + 1..].to_owned();
            inferred_provider = true;
        }
    }

    if provider.is_none() {
        let lower = cli_model.to_ascii_lowercase();
        let exact_matches: Vec<Model> = available_models
            .iter()
            .filter(|m| {
                m.id.to_ascii_lowercase() == lower || format!("{}/{}", m.provider, m.id).to_ascii_lowercase() == lower
            })
            .cloned()
            .collect();
        if exact_matches.len() == 1 {
            return ResolveCliModelResult {
                model: Some(exact_matches[0].clone()),
                ..Default::default()
            };
        }
        if exact_matches.len() > 1 {
            let authenticated_exact: Vec<Model> = exact_matches
                .iter()
                .filter(|m| options.model_runtime.has_configured_auth(&m.provider))
                .cloned()
                .collect();
            if authenticated_exact.len() == 1 {
                return ResolveCliModelResult {
                    model: Some(authenticated_exact[0].clone()),
                    ..Default::default()
                };
            }
            let mut match_names: Vec<String> = exact_matches
                .iter()
                .map(|m| format!("{}/{}", m.provider, m.id))
                .collect();
            match_names.sort();
            let matches = match_names.join(", ");
            let auth_hint = if authenticated_exact.is_empty() {
                "No matching provider is authenticated."
            } else {
                "More than one matching provider is authenticated."
            };
            return ResolveCliModelResult {
                error: Some(format!(
                    "Model \"{cli_model}\" is ambiguous across providers: {matches}. {auth_hint} Use --provider or provider/model."
                )),
                ..Default::default()
            };
        }
    }

    if options.cli_provider.is_some()
        && let Some(ref prov) = provider
    {
        let prefix = format!("{prov}/");
        if cli_model.len() >= prefix.len() && cli_model[..prefix.len()].eq_ignore_ascii_case(&prefix) {
            pattern = cli_model[prefix.len()..].to_owned();
        }
    }

    let candidates: Vec<Model> = match provider.as_deref() {
        Some(p) => available_models.iter().filter(|m| m.provider == p).cloned().collect(),
        None => available_models.clone(),
    };
    let parsed = parse_model_pattern(
        &pattern,
        &candidates,
        Some(ParseModelPatternOptions {
            allow_invalid_thinking_level_fallback: Some(false),
        }),
    );

    if let Some(model) = parsed.model {
        if inferred_provider {
            let cli_lower = cli_model.to_ascii_lowercase();
            let model_any = pi_ai::AnyModel::Chat(model.clone());
            let raw_exact_matches: Vec<Model> = available_models
                .iter()
                .filter(|m| {
                    m.id.to_ascii_lowercase() == cli_lower
                        && !models_are_equal(Some(&pi_ai::AnyModel::Chat((*m).clone())), Some(&model_any))
                })
                .cloned()
                .collect();
            if !raw_exact_matches.is_empty() && !options.model_runtime.has_configured_auth(&model.provider) {
                let auth_raw: Vec<Model> = raw_exact_matches
                    .into_iter()
                    .filter(|m| options.model_runtime.has_configured_auth(&m.provider))
                    .collect();
                if auth_raw.len() == 1 {
                    return ResolveCliModelResult {
                        model: Some(auth_raw[0].clone()),
                        ..Default::default()
                    };
                }
            }
        }
        return ResolveCliModelResult {
            model: Some(model),
            thinking_level: parsed.thinking_level,
            warning: parsed.warning,
            error: None,
        };
    }

    if inferred_provider {
        let lower = cli_model.to_ascii_lowercase();
        if let Some(exact) = available_models.iter().find(|m| {
            m.id.to_ascii_lowercase() == lower || format!("{}/{}", m.provider, m.id).to_ascii_lowercase() == lower
        }) {
            return ResolveCliModelResult {
                model: Some(exact.clone()),
                ..Default::default()
            };
        }
        let fallback = parse_model_pattern(
            cli_model,
            &available_models,
            Some(ParseModelPatternOptions {
                allow_invalid_thinking_level_fallback: Some(false),
            }),
        );
        if fallback.model.is_some() {
            return ResolveCliModelResult {
                model: fallback.model,
                thinking_level: fallback.thinking_level,
                warning: fallback.warning,
                error: None,
            };
        }
    }

    if let Some(ref prov) = provider {
        let mut fallback_pattern = pattern.clone();
        let mut fallback_thinking = None;
        if options.cli_thinking.is_none()
            && let Some(last_colon) = pattern.rfind(':')
        {
            let suffix = &pattern[last_colon + 1..];
            if let Some(level) = parse_valid_thinking_level(suffix) {
                fallback_pattern = pattern[..last_colon].to_owned();
                fallback_thinking = Some(level);
            }
        }

        if let Some(mut fallback_model) = build_fallback_model(prov, &fallback_pattern, &available_models) {
            let requested_thinking = options.cli_thinking.or(fallback_thinking);
            if let Some(req) = requested_thinking
                && req != ModelThinkingLevel::Off
            {
                fallback_model.reasoning = true;
            }
            let fallback_warning = match parsed.warning {
                Some(w) => {
                    format!(
                        "{w} Model \"{fallback_pattern}\" not found for provider \"{prov}\". Using custom model id."
                    )
                }
                None => {
                    format!("Model \"{fallback_pattern}\" not found for provider \"{prov}\". Using custom model id.")
                }
            };
            return ResolveCliModelResult {
                model: Some(fallback_model),
                thinking_level: fallback_thinking,
                warning: Some(fallback_warning),
                error: None,
            };
        }
    }

    let display = match provider {
        Some(p) => format!("{p}/{pattern}"),
        None => cli_model.to_owned(),
    };
    ResolveCliModelResult {
        model: None,
        thinking_level: None,
        warning: parsed.warning,
        error: Some(format!(
            "Model \"{display}\" not found. Use --list-models to see available models."
        )),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct InitialModelResult {
    pub model: Option<Model>,
    pub thinking_level: ModelThinkingLevel,
    pub fallback_message: Option<String>,
}

pub struct FindInitialModelOptions<'a> {
    pub cli_provider: Option<&'a str>,
    pub cli_model: Option<&'a str>,
    pub scoped_models: &'a [ScopedModel],
    pub is_continuing: bool,
    pub default_provider: Option<&'a str>,
    pub default_model_id: Option<&'a str>,
    pub default_thinking_level: Option<ModelThinkingLevel>,
    pub model_thinking_levels: Option<&'a IndexMap<String, ModelThinkingLevel>>,
    pub model_runtime: &'a dyn ModelResolverRuntime,
}

fn find_default_available_model(available_models: &[Model]) -> Option<Model> {
    if available_models.is_empty() {
        return None;
    }
    for (provider, default_id) in default_model_per_provider() {
        if let Some(matched) = available_models
            .iter()
            .find(|m| m.provider == *provider && m.id == *default_id)
        {
            return Some(matched.clone());
        }
    }
    Some(available_models[0].clone())
}

pub async fn find_initial_model(options: FindInitialModelOptions<'_>) -> Result<InitialModelResult> {
    if let (Some(cli_provider), Some(cli_model)) = (options.cli_provider, options.cli_model) {
        let resolved = resolve_cli_model(ResolveCliModelOptions {
            cli_provider: Some(cli_provider),
            cli_model: Some(cli_model),
            cli_thinking: None,
            model_runtime: options.model_runtime,
        });
        if let Some(err) = resolved.error {
            pi_js::console::error(&chalk::red(&err));
            return Err(Error::msg(err));
        }
        if let Some(model) = resolved.model {
            return Ok(InitialModelResult {
                model: Some(model),
                thinking_level: DEFAULT_THINKING_LEVEL,
                fallback_message: None,
            });
        }
    }

    if !options.scoped_models.is_empty() && !options.is_continuing {
        let scoped_model = &options.scoped_models[0];
        let key = format!("{}/{}", scoped_model.model.provider, scoped_model.model.id);
        let per_model = options.model_thinking_levels.and_then(|m| m.get(&key).copied());
        return Ok(InitialModelResult {
            model: Some(scoped_model.model.clone()),
            thinking_level: scoped_model
                .thinking_level
                .or(per_model)
                .or(options.default_thinking_level)
                .unwrap_or(DEFAULT_THINKING_LEVEL),
            fallback_message: None,
        });
    }

    if let (Some(def_provider), Some(def_model_id)) = (options.default_provider, options.default_model_id) {
        if let Some(found) = options.model_runtime.get_model(def_provider, def_model_id)
            && options.model_runtime.has_configured_auth(&found.provider)
        {
            let key = format!("{def_provider}/{def_model_id}");
            let thinking_level = options
                .model_thinking_levels
                .and_then(|m| m.get(&key).copied())
                .or(options.default_thinking_level)
                .unwrap_or(DEFAULT_THINKING_LEVEL);
            return Ok(InitialModelResult {
                model: Some(found),
                thinking_level,
                fallback_message: None,
            });
        }
    }

    let available_models = options.model_runtime.get_available_snapshot();
    if let Some(matched) = find_default_available_model(&available_models) {
        return Ok(InitialModelResult {
            model: Some(matched),
            thinking_level: DEFAULT_THINKING_LEVEL,
            fallback_message: None,
        });
    }

    Ok(InitialModelResult {
        model: None,
        thinking_level: DEFAULT_THINKING_LEVEL,
        fallback_message: None,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct RestoreModelResult {
    pub model: Option<Model>,
    pub fallback_message: Option<String>,
}

pub async fn restore_model_from_session(
    saved_provider: &str,
    saved_model_id: &str,
    current_model: Option<&Model>,
    should_print_messages: bool,
    model_runtime: &dyn ModelResolverRuntime,
) -> RestoreModelResult {
    let restored_model = model_runtime.get_model(saved_provider, saved_model_id);
    let has_configured_auth = restored_model
        .as_ref()
        .is_some_and(|m| model_runtime.has_configured_auth(&m.provider));

    if let Some(model) = restored_model.as_ref().filter(|_| has_configured_auth) {
        if should_print_messages {
            pi_js::console::log(&chalk::dim(&format!(
                "Restored model: {saved_provider}/{saved_model_id}"
            )));
        }
        return RestoreModelResult {
            model: Some(model.clone()),
            fallback_message: None,
        };
    }

    let reason = if restored_model.is_none() {
        "model no longer exists"
    } else {
        "no auth configured"
    };

    if should_print_messages {
        pi_js::console::error(&chalk::yellow(&format!(
            "Warning: Could not restore model {saved_provider}/{saved_model_id} ({reason})."
        )));
    }

    if let Some(cur) = current_model {
        if should_print_messages {
            pi_js::console::log(&chalk::dim(&format!("Falling back to: {}/{}", cur.provider, cur.id)));
        }
        return RestoreModelResult {
            model: Some(cur.clone()),
            fallback_message: Some(format!(
                "Could not restore model {saved_provider}/{saved_model_id} ({reason}). Using {}/{}.",
                cur.provider, cur.id
            )),
        };
    }

    let available_models = model_runtime.get_available_snapshot();
    if let Some(fallback_model) = find_default_available_model(&available_models) {
        if should_print_messages {
            pi_js::console::log(&chalk::dim(&format!(
                "Falling back to: {}/{}",
                fallback_model.provider, fallback_model.id
            )));
        }
        return RestoreModelResult {
            model: Some(fallback_model.clone()),
            fallback_message: Some(format!(
                "Could not restore model {saved_provider}/{saved_model_id} ({reason}). Using {}/{}.",
                fallback_model.provider, fallback_model.id
            )),
        };
    }

    RestoreModelResult {
        model: None,
        fallback_message: None,
    }
}
