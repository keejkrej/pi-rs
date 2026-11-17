//! Port of packages/coding-agent/test/model-resolver.test.ts
//!
//! // WIRE(@core-session-w3): add persisted default-model scoping tests using `AgentSession` once `core::agent_session` lands.

use std::sync::Arc;

use indexmap::IndexMap;
use pi_ai::auth::types::AuthOperationOptions;
use pi_ai::providers::all::{get_builtin_models, get_builtin_providers};
use pi_ai::types::{Model, ModelCost, ModelInput, ModelThinkingLevel};
use pi_coding_agent::core::model_resolver::{
    FindInitialModelOptions, ModelResolverRuntime, ModelScopeDiagnostic, ModelScopeDiagnosticCode,
    ResolveCliModelOptions, ScopedModel, default_model_per_provider, find_initial_model,
    get_default_model_for_provider, parse_model_pattern, resolve_cli_model, resolve_model_scope,
    resolve_model_scope_with_diagnostics, restore_model_from_session,
};
use pi_js::Result;
use pi_js::console::testing::capture as capture_console;

fn mk_model(provider: &str, id: &str, name: &str, reasoning: bool) -> Model {
    Model {
        r#type: None,
        id: id.to_owned(),
        name: name.to_owned(),
        api: "anthropic-messages".to_owned(),
        provider: provider.to_owned(),
        base_url: "https://example.invalid".to_owned(),
        reasoning,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost {
            input: 1.0,
            output: 2.0,
            cache_read: 0.1,
            cache_write: 1.0,
            tiers: None,
        },
        context_window: 128_000,
        max_tokens: 8192,
        headers: None,
        sampling_params: None,
        compat: None,
    }
}

fn sample_models() -> Vec<Model> {
    vec![
        mk_model("anthropic", "claude-sonnet-4-5", "Claude Sonnet 4.5", true),
        mk_model("openai", "gpt-4o", "GPT-4o", false),
        mk_model("openrouter", "qwen/qwen3-coder:exacto", "Qwen3 Coder Exacto", true),
        mk_model("openrouter", "openai/gpt-4o:extended", "GPT-4o Extended", false),
    ]
}

struct MockRuntime {
    models: Vec<Model>,
    available: Vec<Model>,
    authed: Vec<String>,
}

impl MockRuntime {
    fn from_models(models: Vec<Model>) -> Self {
        Self {
            available: models.clone(),
            models,
            authed: Vec::new(),
        }
    }

    fn with_auth(mut self, providers: &[&str]) -> Self {
        self.authed = providers.iter().map(|s| (*s).to_owned()).collect();
        self
    }

    fn with_available(mut self, available: Vec<Model>) -> Self {
        self.available = available;
        self
    }
}

#[async_trait::async_trait]
impl ModelResolverRuntime for MockRuntime {
    fn get_models(&self) -> Vec<Model> {
        self.models.clone()
    }

    fn get_model(&self, provider_id: &str, model_id: &str) -> Option<Model> {
        self.models
            .iter()
            .find(|m| m.provider == provider_id && m.id == model_id)
            .cloned()
    }

    fn get_available_snapshot(&self) -> Vec<Model> {
        self.available.clone()
    }

    fn has_configured_auth(&self, provider_id: &str) -> bool {
        self.authed.iter().any(|p| p == provider_id)
    }

    async fn get_available(
        &self,
        provider_id: Option<&str>,
        _options: Option<AuthOperationOptions>,
    ) -> Result<Vec<Model>> {
        Ok(match provider_id {
            Some(pid) => self.available.iter().filter(|m| m.provider == pid).cloned().collect(),
            None => self.available.clone(),
        })
    }
}

#[test]
fn parse_model_pattern_handles_simple_thinking_and_openrouter_colons() {
    let models = sample_models();

    let exact = parse_model_pattern("claude-sonnet-4-5", &models, None);
    assert_eq!(exact.model.unwrap().id, "claude-sonnet-4-5");
    assert!(exact.thinking_level.is_none());
    assert!(exact.warning.is_none());

    let partial = parse_model_pattern("sonnet", &models, None);
    assert_eq!(partial.model.unwrap().id, "claude-sonnet-4-5");
    assert!(partial.thinking_level.is_none());

    let none = parse_model_pattern("nonexistent", &models, None);
    assert!(none.model.is_none());
    assert!(none.thinking_level.is_none());
    assert!(none.warning.is_none());

    let high = parse_model_pattern("sonnet:high", &models, None);
    assert_eq!(high.model.unwrap().id, "claude-sonnet-4-5");
    assert_eq!(high.thinking_level, Some(ModelThinkingLevel::High));
    assert!(high.warning.is_none());

    let medium = parse_model_pattern("gpt-4o:medium", &models, None);
    assert_eq!(medium.model.unwrap().id, "gpt-4o");
    assert_eq!(medium.thinking_level, Some(ModelThinkingLevel::Medium));

    for (raw, expected) in [
        ("off", ModelThinkingLevel::Off),
        ("minimal", ModelThinkingLevel::Minimal),
        ("low", ModelThinkingLevel::Low),
        ("medium", ModelThinkingLevel::Medium),
        ("high", ModelThinkingLevel::High),
        ("xhigh", ModelThinkingLevel::XHigh),
        ("max", ModelThinkingLevel::Max),
    ] {
        let res = parse_model_pattern(&format!("sonnet:{raw}"), &models, None);
        assert_eq!(res.model.unwrap().id, "claude-sonnet-4-5");
        assert_eq!(res.thinking_level, Some(expected));
        assert!(res.warning.is_none());
    }

    let invalid = parse_model_pattern("sonnet:random", &models, None);
    assert_eq!(invalid.model.unwrap().id, "claude-sonnet-4-5");
    assert!(invalid.thinking_level.is_none());
    let w = invalid.warning.unwrap();
    assert!(w.contains("Invalid thinking level"));
    assert!(w.contains("random"));

    // OpenRouter colons
    let or1 = parse_model_pattern("qwen/qwen3-coder:exacto", &models, None);
    assert_eq!(or1.model.unwrap().id, "qwen/qwen3-coder:exacto");
    assert!(or1.thinking_level.is_none());
    assert!(or1.warning.is_none());

    let or2 = parse_model_pattern("openrouter/qwen/qwen3-coder:exacto", &models, None);
    let m2 = or2.model.unwrap();
    assert_eq!(m2.id, "qwen/qwen3-coder:exacto");
    assert_eq!(m2.provider, "openrouter");
    assert!(or2.warning.is_none());

    let or3 = parse_model_pattern("qwen/qwen3-coder:exacto:high", &models, None);
    assert_eq!(or3.model.unwrap().id, "qwen/qwen3-coder:exacto");
    assert_eq!(or3.thinking_level, Some(ModelThinkingLevel::High));
    assert!(or3.warning.is_none());

    let or4 = parse_model_pattern("openrouter/qwen/qwen3-coder:exacto:high", &models, None);
    let m4 = or4.model.unwrap();
    assert_eq!(m4.id, "qwen/qwen3-coder:exacto");
    assert_eq!(m4.provider, "openrouter");
    assert_eq!(or4.thinking_level, Some(ModelThinkingLevel::High));

    let or_ext = parse_model_pattern("openai/gpt-4o:extended", &models, None);
    assert_eq!(or_ext.model.unwrap().id, "openai/gpt-4o:extended");
    assert!(or_ext.thinking_level.is_none());

    let or_bad = parse_model_pattern("qwen/qwen3-coder:exacto:random", &models, None);
    assert_eq!(or_bad.model.unwrap().id, "qwen/qwen3-coder:exacto");
    assert!(or_bad.thinking_level.is_none());
    assert!(or_bad.warning.unwrap().contains("Invalid thinking level"));

    let empty = parse_model_pattern("", &models, None);
    assert!(empty.model.is_some());
    let trailing_colon = parse_model_pattern("sonnet:", &models, None);
    assert_eq!(trailing_colon.model.unwrap().id, "claude-sonnet-4-5");
    assert!(trailing_colon.warning.unwrap().contains("Invalid thinking level"));
}

#[tokio::test]
async fn resolve_model_scope_with_diagnostics_and_cli_warnings() {
    let models = sample_models();
    let rt = MockRuntime::from_models(models.clone());

    let cap = capture_console();
    let patterns = vec![
        "sonnet:high".to_owned(),
        "gpt-4o:invalid".to_owned(),
        "missing".to_owned(),
    ];
    let result = resolve_model_scope_with_diagnostics(&patterns, &rt, None)
        .await
        .unwrap();
    assert_eq!(
        result
            .scoped_models
            .iter()
            .map(|s| s.model.id.as_str())
            .collect::<Vec<_>>(),
        vec!["claude-sonnet-4-5", "gpt-4o"]
    );
    assert_eq!(result.scoped_models[0].thinking_level, Some(ModelThinkingLevel::High));
    assert!(result.scoped_models[1].thinking_level.is_none());
    assert_eq!(
        result.diagnostics,
        vec![
            ModelScopeDiagnostic {
                diagnostic_type: "warning".to_owned(),
                message: "Invalid thinking level \"invalid\" in pattern \"gpt-4o:invalid\". Using default instead."
                    .to_owned(),
                code: ModelScopeDiagnosticCode::InvalidThinkingLevel,
                pattern: "gpt-4o:invalid".to_owned(),
            },
            ModelScopeDiagnostic {
                diagnostic_type: "warning".to_owned(),
                message: "No models match pattern \"missing\"".to_owned(),
                code: ModelScopeDiagnosticCode::NoMatch,
                pattern: "missing".to_owned(),
            },
        ]
    );
    assert!(cap.stderr().is_empty());

    cap.clear();
    let scoped = resolve_model_scope(&["missing".to_owned()], &rt, None).await.unwrap();
    assert!(scoped.is_empty());
    assert!(cap.stderr().contains("Warning: No models match pattern \"missing\""));

    // Bracketed exact references before globbing
    let mut with_bracket = models;
    with_bracket.push(mk_model("custom", "bracketed-model[1m]", "Bracketed", true));
    let bracket_rt = MockRuntime::from_models(with_bracket);
    let r1 = resolve_model_scope_with_diagnostics(&["custom/bracketed-model[1m]".to_owned()], &bracket_rt, None)
        .await
        .unwrap();
    assert_eq!(
        r1.scoped_models.iter().map(|s| s.model.id.as_str()).collect::<Vec<_>>(),
        vec!["bracketed-model[1m]"]
    );
    assert!(r1.diagnostics.is_empty());

    let r2 = resolve_model_scope_with_diagnostics(&["custom/bracketed-model[1m]:high".to_owned()], &bracket_rt, None)
        .await
        .unwrap();
    assert_eq!(r2.scoped_models[0].thinking_level, Some(ModelThinkingLevel::High));
    assert!(r2.diagnostics.is_empty());
}

#[test]
fn resolve_cli_model_covers_provider_prefix_ambiguity_and_fallback_suffix() {
    let models = sample_models();
    let rt = MockRuntime::from_models(models.clone());

    let res = resolve_cli_model(ResolveCliModelOptions {
        cli_provider: None,
        cli_model: Some("openai/gpt-4o"),
        cli_thinking: None,
        model_runtime: &rt,
    });
    assert!(res.error.is_none());
    let m = res.model.unwrap();
    assert_eq!(m.provider, "openai");
    assert_eq!(m.id, "gpt-4o");

    let fuzzy = resolve_cli_model(ResolveCliModelOptions {
        cli_provider: Some("openai"),
        cli_model: Some("4o"),
        cli_thinking: None,
        model_runtime: &rt,
    });
    assert!(fuzzy.error.is_none());
    assert_eq!(fuzzy.model.unwrap().id, "gpt-4o");

    let with_t = resolve_cli_model(ResolveCliModelOptions {
        cli_provider: None,
        cli_model: Some("sonnet:high"),
        cli_thinking: None,
        model_runtime: &rt,
    });
    assert_eq!(with_t.thinking_level, Some(ModelThinkingLevel::High));
    assert_eq!(with_t.model.unwrap().id, "claude-sonnet-4-5");

    let exact_id = resolve_cli_model(ResolveCliModelOptions {
        cli_provider: None,
        cli_model: Some("openai/gpt-4o:extended"),
        cli_thinking: None,
        model_runtime: &rt,
    });
    let em = exact_id.model.unwrap();
    assert_eq!(em.provider, "openrouter");
    assert_eq!(em.id, "openai/gpt-4o:extended");

    let no_double_prefix = resolve_cli_model(ResolveCliModelOptions {
        cli_provider: Some("openrouter"),
        cli_model: Some("openrouter/openai/ghost-model"),
        cli_thinking: None,
        model_runtime: &rt,
    });
    let nm = no_double_prefix.model.unwrap();
    assert_eq!(nm.provider, "openrouter");
    assert_eq!(nm.id, "openai/ghost-model");

    let empty_rt = MockRuntime::from_models(Vec::new());
    let no_models = resolve_cli_model(ResolveCliModelOptions {
        cli_provider: Some("openai"),
        cli_model: Some("gpt-4o"),
        cli_thinking: None,
        model_runtime: &empty_rt,
    });
    assert!(no_models.model.is_none());
    assert!(no_models.error.unwrap().contains("No models available"));

    // Ambiguous exact bare id prefers sole authenticated provider
    let azure = mk_model("azure-openai-responses", "gpt-5.6-sol", "GPT 5.6 Sol", true);
    let codex = mk_model("openai-codex", "gpt-5.6-sol", "GPT 5.6 Sol", true);
    let auth_rt = MockRuntime::from_models(vec![azure.clone(), codex.clone()]).with_auth(&["openai-codex"]);
    let unique = resolve_cli_model(ResolveCliModelOptions {
        cli_provider: None,
        cli_model: Some("gpt-5.6-sol"),
        cli_thinking: None,
        model_runtime: &auth_rt,
    });
    assert!(unique.error.is_none());
    assert_eq!(unique.model.unwrap().provider, "openai-codex");

    let unauth_rt = MockRuntime::from_models(vec![azure, codex]);
    let ambig = resolve_cli_model(ResolveCliModelOptions {
        cli_provider: None,
        cli_model: Some("gpt-5.6-sol"),
        cli_thinking: None,
        model_runtime: &unauth_rt,
    });
    let err = ambig.error.unwrap();
    assert!(err.contains("Model \"gpt-5.6-sol\" is ambiguous across providers"));
    assert!(err.contains("azure-openai-responses/gpt-5.6-sol"));
    assert!(err.contains("openai-codex/gpt-5.6-sol"));

    // Fallback custom model :thinking suffix (#5552)
    let mut with_nw = models;
    with_nw.push(mk_model("neuralwatt", "some-base-model", "Base", false));
    let nw_rt = MockRuntime::from_models(with_nw);
    let nw_res = resolve_cli_model(ResolveCliModelOptions {
        cli_provider: None,
        cli_model: Some("neuralwatt/zai-org/GLM-5.1-FP8:high"),
        cli_thinking: None,
        model_runtime: &nw_rt,
    });
    assert!(nw_res.error.is_none());
    let nwm = nw_res.model.unwrap();
    assert_eq!(nwm.provider, "neuralwatt");
    assert_eq!(nwm.id, "zai-org/GLM-5.1-FP8");
    assert!(nwm.reasoning);
    assert_eq!(nw_res.thinking_level, Some(ModelThinkingLevel::High));

    let explicit_t = resolve_cli_model(ResolveCliModelOptions {
        cli_provider: None,
        cli_model: Some("neuralwatt/zai-org/GLM-5.1-FP8:high"),
        cli_thinking: Some(ModelThinkingLevel::Medium),
        model_runtime: &nw_rt,
    });
    assert_eq!(explicit_t.model.unwrap().id, "zai-org/GLM-5.1-FP8:high");
    assert!(explicit_t.thinking_level.is_none());
}

#[tokio::test]
async fn default_model_per_provider_and_find_initial_and_restore() {
    assert_eq!(get_default_model_for_provider("openai"), Some("gpt-5.5"));
    assert_eq!(get_default_model_for_provider("openai-codex"), Some("gpt-6.1-sol"));
    assert_eq!(get_default_model_for_provider("zai"), Some("glm-5.3"));
    assert_eq!(get_default_model_for_provider("vercel-ai-gateway"), Some("zai/glm-5.1"));
    assert_eq!(get_default_model_for_provider("xai"), Some("grok-4.7"));
    assert_eq!(
        get_default_model_for_provider("qwen-token-plan-individual"),
        Some("qwen3.8-max")
    );

    let map: IndexMap<&str, &str> = default_model_per_provider().iter().copied().collect();
    for provider in get_builtin_providers() {
        let chat_models = get_builtin_models(provider);
        let default_id = map.get(provider).copied();
        if chat_models.is_empty() {
            assert!(
                default_id.is_none(),
                "{provider} has no chat models and should have no default"
            );
            continue;
        }
        assert!(
            default_id.is_some_and(|id| chat_models.iter().any(|m| m.id == id)),
            "{provider} default {default_id:?} should exist in its generated catalog"
        );
    }

    let models = sample_models();
    let rt1 = MockRuntime::from_models(models);
    let init1 = find_initial_model(FindInitialModelOptions {
        cli_provider: Some("openrouter"),
        cli_model: Some("openrouter/openai/ghost-model"),
        scoped_models: &[],
        is_continuing: false,
        default_provider: None,
        default_model_id: None,
        default_thinking_level: None,
        model_thinking_levels: None,
        model_runtime: &rt1,
    })
    .await
    .unwrap();
    assert_eq!(init1.model.as_ref().unwrap().provider, "openrouter");
    assert_eq!(init1.model.as_ref().unwrap().id, "openai/ghost-model");

    // Ignores unauthenticated saved default and picks authenticated available model
    let saved_ds = mk_model("deepseek", "deepseek-v4-flash", "DeepSeek", true);
    let local_ds = mk_model("spark-two", "deepseek-v4-flash", "DeepSeek", true);
    let rt2 = MockRuntime::from_models(vec![saved_ds])
        .with_auth(&["spark-two"])
        .with_available(vec![local_ds]);
    let init2 = find_initial_model(FindInitialModelOptions {
        cli_provider: None,
        cli_model: None,
        scoped_models: &[],
        is_continuing: false,
        default_provider: Some("deepseek"),
        default_model_id: Some("deepseek-v4-flash"),
        default_thinking_level: None,
        model_thinking_levels: None,
        model_runtime: &rt2,
    })
    .await
    .unwrap();
    assert_eq!(init2.model.as_ref().unwrap().provider, "spark-two");
    assert_eq!(init2.model.as_ref().unwrap().id, "deepseek-v4-flash");

    // restore_model_from_session picks matching authenticated model
    let sonnet = mk_model("anthropic", "claude-sonnet-4-5", "Sonnet", true);
    let _unused_scoped = ScopedModel {
        model: sonnet.clone(),
        thinking_level: Some(ModelThinkingLevel::Medium),
    };
    let rt3 = MockRuntime::from_models(vec![sonnet]).with_auth(&["anthropic"]);
    let restored = restore_model_from_session("anthropic", "claude-sonnet-4-5", None, false, &rt3).await;
    assert_eq!(restored.model.as_ref().unwrap().id, "claude-sonnet-4-5");
    assert!(restored.fallback_message.is_none());
    let _unused: Option<Arc<dyn ModelResolverRuntime>> = None;
}
