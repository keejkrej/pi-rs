//! Port of packages/coding-agent/src/core/model-resolver.ts

use std::cmp::Ordering;
use std::sync::LazyLock;

use indexmap::{IndexMap, IndexSet};
use pi_ai::{AuthOperationOptions, Model, ModelThinkingLevel, models_are_equal};
use pi_js::Result;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::core::defaults::DEFAULT_THINKING_LEVEL;
use crate::core::model_registry::ModelRegistry;
use crate::core::model_runtime::ModelRuntime;
use crate::vendor::chalk;
use crate::vendor::minimatch::{MinimatchOptions, minimatch};

static DATE_SUFFIX_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"-\d{8}$").expect("valid regex"));

pub const VALID_THINKING_LEVELS: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// Parse a string into a [`ModelThinkingLevel`] (`off`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`).
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
        ("amazon-bedrock", "us.anthropic.claude-opus-5-5"),
        ("ant-ling", "Ring-2.6-1T"),
        ("anthropic", "claude-opus-5-5"),
        ("openai", "gpt-6.1-sol"),
        ("azure", "gpt-6.1-sol"),
        ("openai-codex", "gpt-6.1-sol"),
        ("radius", "balanced"),
        ("nvidia", "nvidia/nemotron-3.5-super-vl-120b-a12b"),
        ("deepseek", "deepseek-v4-pro"),
        ("google", "gemini-4-argon"),
        ("google-vertex", "gemini-4-argon"),
        ("github-copilot", "gpt-6.1-sol"),
        ("openrouter", "moonshotai/kimi-k2.7-code"),
        ("vercel-ai-gateway", "zai/glm-5.3"),
        ("xai", "grok-4.7"),
        ("groq", "openai/gpt-oss-120b"),
        ("cerebras", "gpt-oss-120b"),
        ("zai", "glm-5.3"),
        ("zai-coding-cn", "glm-5.3"),
        ("mistral", "devstral-medium-latest"),
        ("minimax", "MiniMax-M3"),
        ("minimax-cn", "MiniMax-M3"),
        ("moonshotai", "kimi-k2.7-code"),
        ("moonshotai-cn", "kimi-k2.7-code"),
        ("huggingface", "moonshotai/Kimi-K2.7-Code"),
        ("fireworks", "accounts/fireworks/models/kimi-k3"),
        ("together", "moonshotai/Kimi-K3"),
        ("baseten", "zai-org/GLM-5.3"),
        ("opencode", "kimi-k2.7-code"),
        ("opencode-go", "kimi-k3.1-pro"),
        ("kimi-coding", "kimi-for-coding"),
        ("meta", "muse-spark-1.3"),
        ("cloudflare-workers-ai", "@cf/moonshotai/kimi-k2.7-code"),
        ("cloudflare-ai-gateway", "workers-ai/@cf/moonshotai/kimi-k2.7-code"),
        ("qwen-token-plan", "qwen3.8-max"),
        ("qwen-token-plan-cn", "qwen3.8-max"),
        ("qwen-token-plan-individual", "qwen3.8-max"),
        ("qwen-token-plan-individual-cn", "qwen3.8-max"),
        ("xiaomi", "mimo-v2.6-pro"),
        ("xiaomi-token-plan-cn", "mimo-v2.6-pro"),
        ("xiaomi-token-plan-ams", "mimo-v2.6-pro"),
        ("xiaomi-token-plan-sgp", "mimo-v2.6-pro"),
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
        self.runtime().get_models(None)
    }

    fn get_model(&self, provider_id: &str, model_id: &str) -> Option<Model> {
        self.runtime().get_model(provider_id, model_id)
    }

    fn get_available_snapshot(&self) -> Vec<Model> {
        self.runtime().get_available_snapshot()
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

/// How trailing `:<suffix>` is handled when resolving model references.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThinkingSuffixMode {
    None,
    Strict,
    Lenient,
}

#[derive(Clone, Debug)]
pub struct ModelReferenceRequest<'a> {
    pub text: &'a str,
    pub provider: Option<&'a str>,
    pub thinking_suffix: ThinkingSuffixMode,
    pub fuzzy: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelReferenceTarget {
    pub provider: String,
    pub provider_models: Vec<Model>,
    pub model_text: String,
    pub model_id: String,
    pub thinking_level: Option<ModelThinkingLevel>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ModelReferenceResult {
    Resolved {
        model: Model,
        thinking_level: Option<ModelThinkingLevel>,
        warning: Option<String>,
    },
    Ambiguous {
        candidates: Vec<Model>,
        usable_count: usize,
        thinking_level: Option<ModelThinkingLevel>,
        warning: Option<String>,
    },
    NotFound {
        target: Option<ModelReferenceTarget>,
    },
}

fn split_thinking_suffix(text: &str) -> Option<(&str, ModelThinkingLevel)> {
    let colon_index = text.rfind(':')?;
    let suffix = &text[colon_index + 1..];
    let level = parse_valid_thinking_level(suffix)?;
    Some((&text[..colon_index], level))
}

fn split_provider_prefix(text: &str, models: &[Model]) -> Option<(String, String)> {
    // Provider ids may contain "/" themselves (e.g. URL-derived ids), so match known ids, longest first.
    let lower_text = text.to_ascii_lowercase();
    let mut best: Option<(String, String)> = None;
    for model in models {
        let prefix = pi_js::str16::trim(&model.provider).to_ascii_lowercase();
        if prefix.is_empty()
            || best
                .as_ref()
                .is_some_and(|(best_prov, _)| best_prov.len() >= model.provider.len())
        {
            continue;
        }
        if lower_text.len() < prefix.len() || !lower_text.is_char_boundary(prefix.len()) {
            continue;
        }
        let Some(slash_rel) = lower_text[prefix.len()..].find('/') else {
            continue;
        };
        let slash_idx = prefix.len() + slash_rel;
        if pi_js::str16::trim(&lower_text[..slash_idx]) != prefix {
            continue;
        }
        let model_text = pi_js::str16::trim(&text[slash_idx + 1..]).to_owned();
        if !model_text.is_empty() {
            best = Some((model.provider.clone(), model_text));
        }
    }
    best
}

fn is_alias(id: &str) -> bool {
    id.ends_with("-latest") || !DATE_SUFFIX_RE.is_match(id)
}

fn fuzzy_matches(text: &str, models: &[Model]) -> Vec<Model> {
    let lower_text = text.to_ascii_lowercase();
    models
        .iter()
        .filter(|model| {
            model.id.to_ascii_lowercase().contains(&lower_text) || model.name.to_ascii_lowercase().contains(&lower_text)
        })
        .cloned()
        .collect()
}

fn best_fuzzy_match(matches: &[Model]) -> Option<Model> {
    let aliases: Vec<&Model> = matches.iter().filter(|m| is_alias(&m.id)).collect();
    let mut pool: Vec<&Model> = if !aliases.is_empty() {
        aliases
    } else {
        matches.iter().collect()
    };
    pool.sort_by(|a, b| {
        let ord = pi_js::intl::locale_compare(&b.id, &a.id);
        if ord == Ordering::Equal { b.id.cmp(&a.id) } else { ord }
    });
    pool.first().map(|m| (*m).clone())
}

fn same_model(a: &Model, b: &Model) -> bool {
    a.provider == b.provider && a.id == b.id
}

fn select_reference<F>(text: &str, pool: &[Model], fuzzy: bool, usable: &F) -> Option<ModelReferenceResult>
where
    F: Fn(&str) -> bool,
{
    let lower_text = text.to_ascii_lowercase();
    let raw: Vec<Model> = pool
        .iter()
        .filter(|model| model.id.to_ascii_lowercase() == lower_text)
        .cloned()
        .collect();
    let split = split_provider_prefix(text, pool);
    let mut named: Option<Model> = None;
    if let Some((ref split_provider, ref split_model_text)) = split {
        let provider_models: Vec<Model> = pool
            .iter()
            .filter(|model| model.provider == *split_provider)
            .cloned()
            .collect();
        let lower_model_text = split_model_text.to_ascii_lowercase();
        named = provider_models
            .iter()
            .find(|model| model.id.to_ascii_lowercase() == lower_model_text)
            .cloned()
            .or_else(|| raw.iter().find(|model| model.provider == *split_provider).cloned())
            .or_else(|| {
                if fuzzy {
                    best_fuzzy_match(&fuzzy_matches(split_model_text, &provider_models))
                } else {
                    None
                }
            });
    }

    let others: Vec<Model> = match named {
        Some(ref n) => raw.into_iter().filter(|m| !same_model(m, n)).collect(),
        None => raw,
    };
    let mut candidates = Vec::new();
    if let Some(ref n) = named {
        candidates.push(n.clone());
    }
    candidates.extend(others.iter().cloned());
    if candidates.is_empty() {
        return None;
    }
    let providers: IndexSet<&str> = candidates.iter().map(|m| m.provider.as_str()).collect();
    if providers.len() == 1 {
        return Some(ModelReferenceResult::Resolved {
            model: candidates.into_iter().next().expect("non-empty"),
            thinking_level: None,
            warning: None,
        });
    }
    if let Some(ref n) = named
        && usable(&n.provider)
    {
        return Some(ModelReferenceResult::Resolved {
            model: n.clone(),
            thinking_level: None,
            warning: None,
        });
    }
    let usable_others: Vec<&Model> = others.iter().filter(|m| usable(&m.provider)).collect();
    if usable_others.len() == 1 {
        return Some(ModelReferenceResult::Resolved {
            model: usable_others[0].clone(),
            thinking_level: None,
            warning: None,
        });
    }
    if usable_others.is_empty()
        && let Some(n) = named
    {
        return Some(ModelReferenceResult::Resolved {
            model: n,
            thinking_level: None,
            warning: None,
        });
    }
    Some(ModelReferenceResult::Ambiguous {
        candidates,
        usable_count: usable_others.len(),
        thinking_level: None,
        warning: None,
    })
}

fn select_fuzzy<F>(matches: &[Model], usable: &F) -> Option<ModelReferenceResult>
where
    F: Fn(&str) -> bool,
{
    if matches.is_empty() {
        return None;
    }
    let distinct_providers: IndexSet<&str> = matches.iter().map(|m| m.provider.as_str()).collect();
    let usable_matches: Vec<Model> = if distinct_providers.len() > 1 {
        matches.iter().filter(|m| usable(&m.provider)).cloned().collect()
    } else {
        Vec::new()
    };
    let pool: &[Model] = if !usable_matches.is_empty() {
        &usable_matches
    } else {
        matches
    };
    let best = best_fuzzy_match(pool)?;
    let tied: Vec<Model> = pool.iter().filter(|m| m.id == best.id).cloned().collect();
    let tied_providers: IndexSet<&str> = tied.iter().map(|m| m.provider.as_str()).collect();
    if tied_providers.len() == 1 {
        return Some(ModelReferenceResult::Resolved {
            model: best,
            thinking_level: None,
            warning: None,
        });
    }
    Some(ModelReferenceResult::Ambiguous {
        candidates: tied.clone(),
        usable_count: if !usable_matches.is_empty() { tied.len() } else { 0 },
        thinking_level: None,
        warning: None,
    })
}

fn resolve_text<F>(text: &str, request: &ModelReferenceRequest<'_>, pool: &[Model], usable: &F) -> ModelReferenceResult
where
    F: Fn(&str) -> bool,
{
    if let Some(reference) = select_reference(text, pool, request.fuzzy, usable) {
        return reference;
    }
    if request.fuzzy
        && let Some(fuzzy) = select_fuzzy(&fuzzy_matches(text, pool), usable)
    {
        return fuzzy;
    }
    if request.thinking_suffix == ThinkingSuffixMode::None {
        return ModelReferenceResult::NotFound { target: None };
    }
    if let Some((base, level)) = split_thinking_suffix(text) {
        let inner = resolve_text(base, request, pool, usable);
        return match inner {
            ModelReferenceResult::NotFound { .. } => inner,
            ModelReferenceResult::Resolved {
                model,
                thinking_level: _,
                warning,
            } => {
                let effective_level = if warning.is_some() { None } else { Some(level) };
                ModelReferenceResult::Resolved {
                    model,
                    thinking_level: effective_level,
                    warning,
                }
            }
            ModelReferenceResult::Ambiguous {
                candidates,
                usable_count,
                thinking_level: _,
                warning,
            } => {
                let effective_level = if warning.is_some() { None } else { Some(level) };
                ModelReferenceResult::Ambiguous {
                    candidates,
                    usable_count,
                    thinking_level: effective_level,
                    warning,
                }
            }
        };
    }
    if request.thinking_suffix == ThinkingSuffixMode::Strict {
        return ModelReferenceResult::NotFound { target: None };
    }
    let Some(colon_index) = text.rfind(':') else {
        return ModelReferenceResult::NotFound { target: None };
    };
    let suffix = &text[colon_index + 1..];
    let inner = resolve_text(&text[..colon_index], request, pool, usable);
    let warning = Some(format!(
        "Invalid thinking level \"{suffix}\" in pattern \"{text}\". Using default instead."
    ));
    match inner {
        ModelReferenceResult::NotFound { .. } => inner,
        ModelReferenceResult::Resolved { model, .. } => ModelReferenceResult::Resolved {
            model,
            thinking_level: None,
            warning,
        },
        ModelReferenceResult::Ambiguous {
            candidates,
            usable_count,
            ..
        } => ModelReferenceResult::Ambiguous {
            candidates,
            usable_count,
            thinking_level: None,
            warning,
        },
    }
}

/// Resolve a model reference against `models`, choosing across providers using `usable` only when ambiguous.
pub fn resolve_model_reference<F>(
    request: ModelReferenceRequest<'_>,
    models: &[Model],
    usable: F,
) -> ModelReferenceResult
where
    F: Fn(&str) -> bool,
{
    let text = pi_js::str16::trim(request.text);
    if text.is_empty() {
        return ModelReferenceResult::NotFound { target: None };
    }
    let constraint = request.provider.map(str::to_ascii_lowercase);
    let filtered: Vec<Model>;
    let pool: &[Model] = if let Some(ref c) = constraint {
        filtered = models
            .iter()
            .filter(|m| m.provider.to_ascii_lowercase() == *c)
            .cloned()
            .collect();
        &filtered
    } else {
        models
    };
    let result = resolve_text(text, &request, pool, &usable);
    if !matches!(result, ModelReferenceResult::NotFound { .. }) {
        return result;
    }

    let split = split_provider_prefix(text, pool);
    let anchor = if constraint.is_some() {
        pool.first()
    } else {
        split
            .as_ref()
            .and_then(|(prov, _)| pool.iter().find(|m| m.provider == *prov))
    };
    let Some(anchor) = anchor else {
        return result;
    };
    let model_text = split.map(|(_, mt)| mt).unwrap_or_else(|| text.to_owned());
    let suffix = if request.thinking_suffix == ThinkingSuffixMode::None {
        None
    } else {
        split_thinking_suffix(&model_text)
    };
    let (model_id, thinking_level) = match suffix {
        Some((base, level)) => (base.to_owned(), Some(level)),
        None => (model_text.clone(), None),
    };
    let mut provider_models = vec![anchor.clone()];
    for model in pool {
        if model.provider == anchor.provider && !same_model(model, anchor) {
            provider_models.push(model.clone());
        }
    }
    ModelReferenceResult::NotFound {
        target: Some(ModelReferenceTarget {
            provider: anchor.provider.clone(),
            provider_models,
            model_text,
            model_id,
            thinking_level,
        }),
    }
}

/// Exact `provider/id` or bare id lookup without thinking suffixes or fuzzy matching.
pub fn find_exact_model_reference_match(model_reference: &str, models: &[Model]) -> Option<Model> {
    match resolve_model_reference(
        ModelReferenceRequest {
            text: model_reference,
            provider: None,
            thinking_suffix: ThinkingSuffixMode::None,
            fuzzy: false,
        },
        models,
        |_| true,
    ) {
        ModelReferenceResult::Resolved { model, .. } => Some(model),
        _ => None,
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

/// Compatibility helper built on `resolve_model_reference`.
pub fn parse_model_pattern(
    pattern: &str,
    available_models: &[Model],
    options: Option<ParseModelPatternOptions>,
) -> ParsedModelResult {
    let allow_fallback = options
        .and_then(|o| o.allow_invalid_thinking_level_fallback)
        .unwrap_or(true);
    let thinking_suffix = if allow_fallback {
        ThinkingSuffixMode::Lenient
    } else {
        ThinkingSuffixMode::Strict
    };
    match resolve_model_reference(
        ModelReferenceRequest {
            text: pattern,
            provider: None,
            thinking_suffix,
            fuzzy: true,
        },
        available_models,
        |_| true,
    ) {
        ModelReferenceResult::Resolved {
            model,
            thinking_level,
            warning,
        } => ParsedModelResult {
            model: Some(model),
            thinking_level,
            warning,
        },
        ModelReferenceResult::Ambiguous {
            candidates,
            thinking_level,
            warning,
            ..
        } => ParsedModelResult {
            model: candidates.into_iter().next(),
            thinking_level,
            warning,
        },
        ModelReferenceResult::NotFound { .. } => ParsedModelResult {
            model: None,
            thinking_level: None,
            warning: None,
        },
    }
}

fn build_fallback_model(provider_models: &[Model], model_id: &str) -> Model {
    let base_model = get_default_model_for_provider(&provider_models[0].provider)
        .and_then(|default_id| provider_models.iter().find(|m| m.id == default_id))
        .unwrap_or(&provider_models[0]);
    let mut out = base_model.clone();
    out.id = model_id.to_owned();
    out.name = model_id.to_owned();
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelScopeDiagnosticCode {
    NoMatch,
    InvalidThinkingLevel,
    Ambiguous,
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

    let push_no_match = |diagnostics: &mut Vec<ModelScopeDiagnostic>, pattern: &str| {
        diagnostics.push(ModelScopeDiagnostic {
            diagnostic_type: "warning".to_owned(),
            code: ModelScopeDiagnosticCode::NoMatch,
            message: format!("No models match pattern \"{pattern}\""),
            pattern: pattern.to_owned(),
        });
    };

    let push_ambiguous = |diagnostics: &mut Vec<ModelScopeDiagnostic>, pattern: &str, candidates: &[Model]| {
        let mut providers: Vec<String> = candidates
            .iter()
            .map(|m| m.provider.clone())
            .collect::<IndexSet<_>>()
            .into_iter()
            .collect();
        providers.sort_by(|a, b| {
            let ord = pi_js::intl::locale_compare(a, b);
            if ord == Ordering::Equal { a.cmp(b) } else { ord }
        });
        diagnostics.push(ModelScopeDiagnostic {
            diagnostic_type: "warning".to_owned(),
            code: ModelScopeDiagnosticCode::Ambiguous,
            message: format!(
                "Pattern \"{pattern}\" matches several providers: {}. Use provider/id to choose one.",
                providers.join(", ")
            ),
            pattern: pattern.to_owned(),
        });
    };

    for pattern in patterns {
        if pattern.contains('*') || pattern.contains('?') || pattern.contains('[') {
            let thinking = split_thinking_suffix(pattern);
            let glob_pattern = thinking.map(|(b, _)| b).unwrap_or(pattern.as_str());
            let thinking_level = thinking.map(|(_, l)| l);

            let matches: Vec<Model> = match resolve_model_reference(
                ModelReferenceRequest {
                    text: glob_pattern,
                    provider: None,
                    thinking_suffix: ThinkingSuffixMode::None,
                    fuzzy: false,
                },
                models,
                |_| true,
            ) {
                ModelReferenceResult::Resolved { model, .. } => vec![model],
                ModelReferenceResult::Ambiguous { candidates, .. } => {
                    push_ambiguous(&mut diagnostics, pattern, &candidates);
                    candidates
                }
                ModelReferenceResult::NotFound { .. } => {
                    let mm_opts = Some(MinimatchOptions {
                        nocase: true,
                        ..Default::default()
                    });
                    models
                        .iter()
                        .filter(|m| {
                            let full_id = format!("{}/{}", m.provider, m.id);
                            minimatch(&full_id, glob_pattern, mm_opts.clone())
                                || minimatch(&m.id, glob_pattern, mm_opts.clone())
                        })
                        .cloned()
                        .collect()
                }
            };

            if matches.is_empty() {
                push_no_match(&mut diagnostics, pattern);
            }
            for model in matches {
                push_unique_scoped(&mut scoped_models, model, thinking_level);
            }
            continue;
        }

        match resolve_model_reference(
            ModelReferenceRequest {
                text: pattern,
                provider: None,
                thinking_suffix: ThinkingSuffixMode::Lenient,
                fuzzy: true,
            },
            models,
            |_| true,
        ) {
            ModelReferenceResult::NotFound { .. } => {
                push_no_match(&mut diagnostics, pattern);
            }
            ModelReferenceResult::Resolved {
                model,
                thinking_level,
                warning,
            } => {
                if let Some(w) = warning {
                    diagnostics.push(ModelScopeDiagnostic {
                        diagnostic_type: "warning".to_owned(),
                        code: ModelScopeDiagnosticCode::InvalidThinkingLevel,
                        message: w,
                        pattern: pattern.clone(),
                    });
                }
                push_unique_scoped(&mut scoped_models, model, thinking_level);
            }
            ModelReferenceResult::Ambiguous {
                candidates,
                thinking_level,
                warning,
                ..
            } => {
                if let Some(w) = warning {
                    diagnostics.push(ModelScopeDiagnostic {
                        diagnostic_type: "warning".to_owned(),
                        code: ModelScopeDiagnosticCode::InvalidThinkingLevel,
                        message: w,
                        pattern: pattern.clone(),
                    });
                }
                push_ambiguous(&mut diagnostics, pattern, &candidates);
                for model in candidates {
                    push_unique_scoped(&mut scoped_models, model, thinking_level);
                }
            }
        }
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
    pub inline_api_key: Option<bool>,
    pub model_runtime: &'a dyn ModelResolverRuntime,
}

pub fn resolve_cli_model(options: ResolveCliModelOptions<'_>) -> ResolveCliModelResult {
    let Some(cli_model) = options.cli_model else {
        return ResolveCliModelResult::default();
    };

    let models = options.model_runtime.get_models();
    if models.is_empty() {
        return ResolveCliModelResult {
            error: Some("No models available. Check your installation or add models to models.json.".to_owned()),
            ..Default::default()
        };
    }

    let provider = options.cli_provider.and_then(|cli_prov| {
        let lower = cli_prov.to_ascii_lowercase();
        models
            .iter()
            .find(|m| m.provider.to_ascii_lowercase() == lower)
            .map(|m| m.provider.clone())
    });
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

    let inline_api_key = options.inline_api_key.unwrap_or(false);
    let result = resolve_model_reference(
        ModelReferenceRequest {
            text: cli_model,
            provider: provider.as_deref(),
            thinking_suffix: ThinkingSuffixMode::Strict,
            fuzzy: true,
        },
        &models,
        |provider_id| {
            if inline_api_key {
                false
            } else {
                options.model_runtime.has_configured_auth(provider_id)
            }
        },
    );

    match result {
        ModelReferenceResult::Resolved {
            model,
            thinking_level,
            warning,
        } => ResolveCliModelResult {
            model: Some(model),
            thinking_level,
            warning,
            error: None,
        },
        ModelReferenceResult::Ambiguous {
            candidates,
            usable_count,
            ..
        } => {
            let mut matches: Vec<String> = candidates.iter().map(|m| format!("{}/{}", m.provider, m.id)).collect();
            matches.sort_by(|a, b| {
                let ord = pi_js::intl::locale_compare(a, b);
                if ord == Ordering::Equal { a.cmp(b) } else { ord }
            });
            let auth_hint = if inline_api_key {
                "--api-key does not choose a provider."
            } else if usable_count == 0 {
                "No matching provider is authenticated."
            } else {
                "More than one matching provider is authenticated."
            };
            ResolveCliModelResult {
                model: None,
                thinking_level: None,
                warning: None,
                error: Some(format!(
                    "Model \"{cli_model}\" is ambiguous across providers: {}. {auth_hint} Use --provider or provider/model.",
                    matches.join(", ")
                )),
            }
        }
        ModelReferenceResult::NotFound { target: None } => ResolveCliModelResult {
            model: None,
            thinking_level: None,
            warning: None,
            error: Some(format!(
                "Model \"{cli_model}\" not found. Use --list-models to see available models."
            )),
        },
        ModelReferenceResult::NotFound { target: Some(target) } => {
            let model_id = if options.cli_thinking.is_some() {
                target.model_text.clone()
            } else {
                target.model_id.clone()
            };
            let fallback_thinking = if options.cli_thinking.is_some() {
                None
            } else {
                target.thinking_level
            };
            let mut fallback_model = build_fallback_model(&target.provider_models, &model_id);
            let requested_thinking = options.cli_thinking.or(fallback_thinking);
            if let Some(req) = requested_thinking
                && req != ModelThinkingLevel::Off
            {
                fallback_model.reasoning = true;
            }
            ResolveCliModelResult {
                model: Some(fallback_model),
                thinking_level: fallback_thinking,
                warning: Some(format!(
                    "Model \"{model_id}\" not found for provider \"{}\". Using custom model id.",
                    target.provider
                )),
                error: None,
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct InitialModelResult {
    pub model: Option<Model>,
    pub thinking_level: ModelThinkingLevel,
    pub fallback_message: Option<String>,
}

pub struct FindInitialModelOptions<'a> {
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
