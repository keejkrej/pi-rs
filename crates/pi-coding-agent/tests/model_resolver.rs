//! Port of packages/coding-agent/test/model-resolver.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::Arc;

use indexmap::IndexMap;
use pi_ai::auth::types::AuthOperationOptions;
use pi_ai::providers::all::{get_builtin_models, get_builtin_providers};
use pi_ai::types::{Model, ModelCost, ModelInput, ModelThinkingLevel};
use pi_coding_agent::core::model_resolver::{
    FindInitialModelOptions, ModelReferenceRequest, ModelReferenceResult, ModelResolverRuntime, ModelScopeDiagnostic,
    ModelScopeDiagnosticCode, ResolveCliModelOptions, ScopedModel, ThinkingSuffixMode, default_model_per_provider,
    find_initial_model, get_default_model_for_provider, parse_model_pattern, resolve_cli_model,
    resolve_model_reference, resolve_model_scope, resolve_model_scope_with_diagnostics, restore_model_from_session,
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
        sampling_params_by_thinking_level: None,
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
    assert!(empty.model.is_none());
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
        inline_api_key: None,
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
        inline_api_key: None,
        model_runtime: &rt,
    });
    assert!(fuzzy.error.is_none());
    assert_eq!(fuzzy.model.unwrap().id, "gpt-4o");

    let with_t = resolve_cli_model(ResolveCliModelOptions {
        cli_provider: None,
        cli_model: Some("sonnet:high"),
        cli_thinking: None,
        inline_api_key: None,
        model_runtime: &rt,
    });
    assert_eq!(with_t.thinking_level, Some(ModelThinkingLevel::High));
    assert_eq!(with_t.model.unwrap().id, "claude-sonnet-4-5");

    let exact_id = resolve_cli_model(ResolveCliModelOptions {
        cli_provider: None,
        cli_model: Some("openai/gpt-4o:extended"),
        cli_thinking: None,
        inline_api_key: None,
        model_runtime: &rt,
    });
    let em = exact_id.model.unwrap();
    assert_eq!(em.provider, "openrouter");
    assert_eq!(em.id, "openai/gpt-4o:extended");

    let no_double_prefix = resolve_cli_model(ResolveCliModelOptions {
        cli_provider: Some("openrouter"),
        cli_model: Some("openrouter/openai/ghost-model"),
        cli_thinking: None,
        inline_api_key: None,
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
        inline_api_key: None,
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
        inline_api_key: None,
        model_runtime: &auth_rt,
    });
    assert!(unique.error.is_none());
    assert_eq!(unique.model.unwrap().provider, "openai-codex");

    let unauth_rt = MockRuntime::from_models(vec![azure, codex]);
    let ambig = resolve_cli_model(ResolveCliModelOptions {
        cli_provider: None,
        cli_model: Some("gpt-5.6-sol"),
        cli_thinking: None,
        inline_api_key: None,
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
        inline_api_key: None,
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
        inline_api_key: None,
        model_runtime: &nw_rt,
    });
    assert_eq!(explicit_t.model.unwrap().id, "zai-org/GLM-5.1-FP8:high");
    assert!(explicit_t.thinking_level.is_none());
}

#[tokio::test]
async fn default_model_per_provider_and_find_initial_and_restore() {
    assert_eq!(get_default_model_for_provider("openai"), Some("gpt-6.1-sol"));
    assert_eq!(get_default_model_for_provider("openai-codex"), Some("gpt-6.1-sol"));
    assert_eq!(get_default_model_for_provider("zai"), Some("glm-5.3"));
    assert_eq!(get_default_model_for_provider("vercel-ai-gateway"), Some("zai/glm-5.3"));
    assert_eq!(get_default_model_for_provider("anthropic"), Some("claude-opus-5-5"));
    assert_eq!(get_default_model_for_provider("xai"), Some("grok-4.7"));
    assert_eq!(
        get_default_model_for_provider("qwen-token-plan-individual"),
        Some("qwen3.8-max")
    );
    assert_eq!(
        get_default_model_for_provider("qwen-token-plan-individual-cn"),
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

    let ai_gateway_model = mk_model(
        "vercel-ai-gateway",
        "anthropic/claude-opus-4-6",
        "Claude Opus 4.6",
        true,
    );
    let rt1 = MockRuntime::from_models(vec![]).with_available(vec![ai_gateway_model]);
    let init1 = find_initial_model(FindInitialModelOptions {
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
    assert_eq!(init1.model.as_ref().unwrap().provider, "vercel-ai-gateway");
    assert_eq!(init1.model.as_ref().unwrap().id, "anthropic/claude-opus-4-6");

    // Ignores unauthenticated saved default and picks authenticated available model
    let saved_ds = mk_model("deepseek", "deepseek-v4-flash", "DeepSeek", true);
    let local_ds = mk_model("spark-two", "deepseek-v4-flash", "DeepSeek", true);
    let rt2 = MockRuntime::from_models(vec![saved_ds])
        .with_auth(&["spark-two"])
        .with_available(vec![local_ds]);
    let init2 = find_initial_model(FindInitialModelOptions {
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

#[tokio::test]
async fn agent_session_restores_persisted_model_and_cycles_scoped_models() {
    use pi_ai::providers::faux::FauxModelDefinition;
    use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, ScopedModelEntry, create_agent_session};
    use pi_coding_agent::core::session_manager::SessionManager;
    use pi_coding_agent::core::settings_manager::SettingsManager;
    use support::suite::harness::{HarnessOptions, create_harness};

    let harness = create_harness(HarnessOptions {
        models: Some(vec![
            FauxModelDefinition {
                id: "m1".to_owned(),
                name: None,
                reasoning: Some(true),
                input: None,
                input_limits: None,
                cost: None,
                context_window: None,
                max_tokens: None,
            },
            FauxModelDefinition {
                id: "m2".to_owned(),
                name: None,
                reasoning: Some(false),
                input: None,
                input_limits: None,
                cost: None,
                context_window: None,
                max_tokens: None,
            },
        ]),
        ..Default::default()
    })
    .await;

    let m1 = harness.get_model_by_id("m1").unwrap();
    let m2 = harness.get_model_by_id("m2").unwrap();
    let sm = SessionManager::in_memory(Some(&harness.temp_dir), None, None).unwrap();
    sm.append_model_change(&m2.provider, &m2.id).unwrap();
    sm.append_message(pi_agent_core::AgentMessage::Message(pi_ai::Message::User(
        pi_ai::UserMessage {
            content: pi_ai::UserMessageContent::Text("persisted prompt".to_owned()),
            timestamp: 1,
        },
    )))
    .unwrap();

    let created = create_agent_session(Some(CreateAgentSessionOptions {
        cwd: Some(harness.temp_dir.clone()),
        agent_dir: Some(harness.temp_dir.clone()),
        session_manager: Some(sm),
        settings_manager: Some(SettingsManager::in_memory(None, None)),
        model_runtime: Some(harness.session.model_runtime().clone()),
        scoped_models: Some(vec![
            ScopedModelEntry {
                model: m1.clone(),
                thinking_level: Some(ModelThinkingLevel::Medium),
            },
            ScopedModelEntry {
                model: m2.clone(),
                thinking_level: None,
            },
        ]),
        ..Default::default()
    }))
    .await
    .unwrap();
    assert_eq!(created.session.model().map(|m| m.id), Some("m2".to_owned()));
    let cycled = created.session.cycle_model(None, None).await.unwrap().unwrap();
    assert_eq!(cycled.model.id, "m1");
    created.session.dispose();
    harness.cleanup();
}

#[test]
fn resolve_model_reference_covers_precedence_ambiguity_and_provider_constraints() {
    use std::collections::HashSet;

    let catalog = vec![
        mk_model("anthropic", "claude-sonnet-4-5", "claude-sonnet-4-5", true),
        mk_model(
            "anthropic",
            "claude-sonnet-4-5-20250929",
            "claude-sonnet-4-5-20250929",
            true,
        ),
        mk_model("anthropic", "claude-opus-4-8", "claude-opus-4-8", true),
        mk_model("anthropic", "exact", "exact", true),
        mk_model("anthropic", "shared-pro", "shared-pro", true),
        mk_model("openai", "gpt-4o", "gpt-4o", true),
        mk_model("openrouter", "qwen/qwen3-coder:exacto", "qwen/qwen3-coder:exacto", true),
        mk_model("openrouter", "openai/gpt-4o:extended", "openai/gpt-4o:extended", true),
        mk_model("gw-a", "shared", "shared", true),
        mk_model("gw-a", "anthropic/future", "anthropic/future", true),
        mk_model("gw-a", "anthropic/exact", "anthropic/exact", true),
        mk_model("gw-a", "anthropic/claude-opus-4.8", "anthropic/claude-opus-4.8", true),
        mk_model("gw-a", "anthropic/claude-opus", "anthropic/claude-opus", true),
        mk_model("gw-b", "shared", "shared", true),
        mk_model("gw-b", "anthropic/future", "anthropic/future", true),
        mk_model("gw-b", "claude-opus-4-8", "claude-opus-4-8", true),
        mk_model("gw-b", "gw-b/free", "gw-b/free", true),
        mk_model("gw-b", "zz:free", "zz:free", true),
        mk_model(
            "llama=http://host:10800",
            "unsloth/qwen-gguf:BF16",
            "unsloth/qwen-gguf:BF16",
            true,
        ),
    ];

    enum Expected {
        NotFound,
        Ambiguous {
            candidates: &'static [&'static str],
            usable_count: usize,
        },
        Resolved {
            model: &'static str,
            thinking_level: Option<ModelThinkingLevel>,
            warning: Option<&'static str>,
        },
    }

    struct Case {
        name: &'static str,
        text: &'static str,
        usable: &'static [&'static str],
        provider: Option<&'static str>,
        thinking_suffix: ThinkingSuffixMode,
        fuzzy: bool,
        expected: Expected,
    }

    fn c(name: &'static str, text: &'static str, expected: Expected) -> Case {
        Case {
            name,
            text,
            usable: &[],
            provider: None,
            thinking_suffix: ThinkingSuffixMode::Strict,
            fuzzy: true,
            expected,
        }
    }

    let cases = vec![
        c(
            "exact id",
            "claude-sonnet-4-5",
            Expected::Resolved {
                model: "anthropic/claude-sonnet-4-5",
                thinking_level: None,
                warning: None,
            },
        ),
        c(
            "exact id is case-insensitive",
            "GPT-4O",
            Expected::Resolved {
                model: "openai/gpt-4o",
                thinking_level: None,
                warning: None,
            },
        ),
        c(
            "fuzzy prefers aliases over dated ids",
            "sonnet",
            Expected::Resolved {
                model: "anthropic/claude-sonnet-4-5",
                thinking_level: None,
                warning: None,
            },
        ),
        c(
            "thinking suffix",
            "sonnet:high",
            Expected::Resolved {
                model: "anthropic/claude-sonnet-4-5",
                thinking_level: Some(ModelThinkingLevel::High),
                warning: None,
            },
        ),
        c(
            "colon in literal id is not a thinking suffix",
            "qwen/qwen3-coder:exacto",
            Expected::Resolved {
                model: "openrouter/qwen/qwen3-coder:exacto",
                thinking_level: None,
                warning: None,
            },
        ),
        c(
            "provider prefix, colon id, and thinking suffix",
            "openrouter/qwen/qwen3-coder:exacto:high",
            Expected::Resolved {
                model: "openrouter/qwen/qwen3-coder:exacto",
                thinking_level: Some(ModelThinkingLevel::High),
                warning: None,
            },
        ),
        c(
            "provider id containing a slash",
            "llama=http://host:10800/unsloth/qwen-gguf:BF16",
            Expected::Resolved {
                model: "llama=http://host:10800/unsloth/qwen-gguf:BF16",
                thinking_level: None,
                warning: None,
            },
        ),
        c(
            "provider id containing a slash with thinking suffix",
            "llama=http://host:10800/unsloth/qwen-gguf:BF16:high",
            Expected::Resolved {
                model: "llama=http://host:10800/unsloth/qwen-gguf:BF16",
                thinking_level: Some(ModelThinkingLevel::High),
                warning: None,
            },
        ),
        c(
            "raw id wins when provider prefix names no matching model",
            "openai/gpt-4o:extended",
            Expected::Resolved {
                model: "openrouter/openai/gpt-4o:extended",
                thinking_level: None,
                warning: None,
            },
        ),
        c(
            "fuzzy searches prefixed provider first",
            "openrouter/qwen",
            Expected::Resolved {
                model: "openrouter/qwen/qwen3-coder:exacto",
                thinking_level: None,
                warning: None,
            },
        ),
        c("strict keeps invalid suffix in id", "gpt-4o:random", Expected::NotFound),
        Case {
            thinking_suffix: ThinkingSuffixMode::Lenient,
            ..c(
                "lenient drops invalid suffix with warning",
                "gpt-4o:random",
                Expected::Resolved {
                    model: "openai/gpt-4o",
                    thinking_level: None,
                    warning: Some("Invalid thinking level \"random\""),
                },
            )
        },
        Case {
            thinking_suffix: ThinkingSuffixMode::Lenient,
            ..c(
                "lenient invalid outer suffix discards inner thinking level",
                "qwen/qwen3-coder:exacto:high:random",
                Expected::Resolved {
                    model: "openrouter/qwen/qwen3-coder:exacto",
                    thinking_level: None,
                    warning: Some("Invalid thinking level \"random\""),
                },
            )
        },
        Case {
            thinking_suffix: ThinkingSuffixMode::None,
            ..c("none keeps suffix in id", "gpt-4o:high", Expected::NotFound)
        },
        Case {
            fuzzy: false,
            ..c("exact-only skips fuzzy", "sonnet", Expected::NotFound)
        },
        c("empty text", "  ", Expected::NotFound),
        Case {
            usable: &["gw-b"],
            ..c(
                "sole usable provider wins",
                "shared",
                Expected::Resolved {
                    model: "gw-b/shared",
                    thinking_level: None,
                    warning: None,
                },
            )
        },
        c(
            "no usable provider is ambiguous instead of falling through to fuzzy",
            "shared",
            Expected::Ambiguous {
                candidates: &["gw-a/shared", "gw-b/shared"],
                usable_count: 0,
            },
        ),
        Case {
            usable: &["gw-a", "gw-b"],
            ..c(
                "several usable providers are ambiguous",
                "shared",
                Expected::Ambiguous {
                    candidates: &["gw-a/shared", "gw-b/shared"],
                    usable_count: 2,
                },
            )
        },
        Case {
            usable: &["gw-b"],
            ..c(
                "thinking suffix does not change provider selection",
                "shared:high",
                Expected::Resolved {
                    model: "gw-b/shared",
                    thinking_level: Some(ModelThinkingLevel::High),
                    warning: None,
                },
            )
        },
        Case {
            usable: &["gw-a", "gw-b"],
            ..c(
                "thinking suffix does not bypass ambiguity",
                "shared:high",
                Expected::Ambiguous {
                    candidates: &["gw-a/shared", "gw-b/shared"],
                    usable_count: 2,
                },
            )
        },
        Case {
            usable: &["gw-b"],
            ..c(
                "provider prefix without direct model selects usable raw id",
                "anthropic/future",
                Expected::Resolved {
                    model: "gw-b/anthropic/future",
                    thinking_level: None,
                    warning: None,
                },
            )
        },
        Case {
            usable: &["anthropic", "gw-a"],
            ..c(
                "usable qualified match beats usable raw id",
                "anthropic/exact",
                Expected::Resolved {
                    model: "anthropic/exact",
                    thinking_level: None,
                    warning: None,
                },
            )
        },
        Case {
            usable: &["gw-a"],
            ..c(
                "usable raw id beats unusable qualified match",
                "anthropic/exact",
                Expected::Resolved {
                    model: "gw-a/anthropic/exact",
                    thinking_level: None,
                    warning: None,
                },
            )
        },
        c(
            "unusable qualified match kept when nothing is usable",
            "anthropic/exact",
            Expected::Resolved {
                model: "anthropic/exact",
                thinking_level: None,
                warning: None,
            },
        ),
        Case {
            usable: &["anthropic"],
            ..c(
                "named provider usable fuzzy match beats raw id elsewhere",
                "anthropic/claude-opus",
                Expected::Resolved {
                    model: "anthropic/claude-opus-4-8",
                    thinking_level: None,
                    warning: None,
                },
            )
        },
        Case {
            usable: &["gw-a"],
            ..c(
                "usable raw id beats named provider unusable fuzzy match",
                "anthropic/claude-opus",
                Expected::Resolved {
                    model: "gw-a/anthropic/claude-opus",
                    thinking_level: None,
                    warning: None,
                },
            )
        },
        c(
            "named provider fuzzy match kept when nothing is usable",
            "anthropic/claude-opus",
            Expected::Resolved {
                model: "anthropic/claude-opus-4-8",
                thinking_level: None,
                warning: None,
            },
        ),
        c(
            "named provider literal id beats fuzzy matches",
            "gw-b/free",
            Expected::Resolved {
                model: "gw-b/gw-b/free",
                thinking_level: None,
                warning: None,
            },
        ),
        Case {
            usable: &["gw-a"],
            ..c(
                "fuzzy prefers usable providers",
                "opus",
                Expected::Resolved {
                    model: "gw-a/anthropic/claude-opus-4.8",
                    thinking_level: None,
                    warning: None,
                },
            )
        },
        Case {
            usable: &["anthropic", "gw-a", "gw-b"],
            ..c(
                "fuzzy id tie across usable providers is ambiguous",
                "opus",
                Expected::Ambiguous {
                    candidates: &["anthropic/claude-opus-4-8", "gw-b/claude-opus-4-8"],
                    usable_count: 2,
                },
            )
        },
        Case {
            usable: &["anthropic"],
            ..c(
                "fuzzy id tie resolves to sole usable provider",
                "opus",
                Expected::Resolved {
                    model: "anthropic/claude-opus-4-8",
                    thinking_level: None,
                    warning: None,
                },
            )
        },
        Case {
            provider: Some("gw-a"),
            usable: &["gw-b"],
            ..c(
                "explicit provider ignores usable providers elsewhere",
                "shared",
                Expected::Resolved {
                    model: "gw-a/shared",
                    thinking_level: None,
                    warning: None,
                },
            )
        },
        Case {
            provider: Some("gw-b"),
            ..c(
                "explicit provider accepts own prefix",
                "GW-B/shared",
                Expected::Resolved {
                    model: "gw-b/shared",
                    thinking_level: None,
                    warning: None,
                },
            )
        },
        Case {
            provider: Some("openai"),
            ..c(
                "explicit provider allows fuzzy",
                "4o",
                Expected::Resolved {
                    model: "openai/gpt-4o",
                    thinking_level: None,
                    warning: None,
                },
            )
        },
        Case {
            provider: Some("gw-a"),
            ..c("explicit provider never leaves provider", "gpt-4o", Expected::NotFound)
        },
    ];

    let mut reversed_catalog = catalog.clone();
    reversed_catalog.reverse();

    for case in cases {
        let usable_set: HashSet<&str> = case.usable.iter().copied().collect();
        for models in [&catalog, &reversed_catalog] {
            let res = resolve_model_reference(
                ModelReferenceRequest {
                    text: case.text,
                    provider: case.provider,
                    thinking_suffix: case.thinking_suffix,
                    fuzzy: case.fuzzy,
                },
                models,
                |provider| usable_set.contains(provider),
            );
            match (&case.expected, res) {
                (Expected::NotFound, ModelReferenceResult::NotFound { .. }) => {}
                (
                    Expected::Ambiguous {
                        candidates,
                        usable_count,
                    },
                    ModelReferenceResult::Ambiguous {
                        candidates: got,
                        usable_count: got_count,
                        ..
                    },
                ) => {
                    let mut got_refs: Vec<String> = got.iter().map(|m| format!("{}/{}", m.provider, m.id)).collect();
                    got_refs.sort();
                    assert_eq!(got_refs, *candidates, "{}", case.name);
                    assert_eq!(got_count, *usable_count, "{}", case.name);
                }
                (
                    Expected::Resolved {
                        model,
                        thinking_level,
                        warning,
                    },
                    ModelReferenceResult::Resolved {
                        model: got,
                        thinking_level: got_tl,
                        warning: got_warn,
                    },
                ) => {
                    assert_eq!(format!("{}/{}", got.provider, got.id), *model, "{}", case.name);
                    assert_eq!(got_tl, *thinking_level, "{}", case.name);
                    if let Some(w) = warning {
                        assert!(
                            got_warn.as_deref().is_some_and(|s| s.contains(w)),
                            "{}: expected warning containing {w}, got {got_warn:?}",
                            case.name
                        );
                    } else {
                        assert_eq!(got_warn, None, "{}", case.name);
                    }
                }
                (_, other) => panic!("case '{}' got unexpected result: {other:?}", case.name),
            }
        }
    }
}

#[test]
fn resolve_cli_model_duplicate_raw_ids_behind_provider_prefix_10160() {
    let direct = mk_model("anthropic", "claude-sonnet-4-5", "Sonnet 4.5", true);
    let openrouter = mk_model("openrouter", "anthropic/claude-opus-5.5", "Opus 5.5 OR", true);
    let vercel = mk_model("vercel-ai-gateway", "anthropic/claude-opus-5.5", "Opus 5.5 V", true);

    let authed_rt = MockRuntime::from_models(vec![direct.clone(), openrouter.clone(), vercel.clone()])
        .with_auth(&["vercel-ai-gateway"]);
    for (cli_model, expected_tl) in [
        ("anthropic/claude-opus-5.5", None),
        ("anthropic/claude-opus-5.5:high", Some(ModelThinkingLevel::High)),
    ] {
        let res = resolve_cli_model(ResolveCliModelOptions {
            cli_provider: None,
            cli_model: Some(cli_model),
            cli_thinking: None,
            inline_api_key: None,
            model_runtime: &authed_rt,
        });
        assert_eq!(res.error, None);
        assert_eq!(res.model.unwrap().provider, "vercel-ai-gateway");
        assert_eq!(res.thinking_level, expected_tl);
    }

    let unauth_rt = MockRuntime::from_models(vec![direct, openrouter, vercel]);
    let ambig = resolve_cli_model(ResolveCliModelOptions {
        cli_provider: None,
        cli_model: Some("anthropic/claude-opus-5.5"),
        cli_thinking: None,
        inline_api_key: None,
        model_runtime: &unauth_rt,
    });
    assert!(ambig.model.is_none());
    let err = ambig.error.unwrap();
    assert!(err.contains("openrouter/anthropic/claude-opus-5.5, vercel-ai-gateway/anthropic/claude-opus-5.5"));
    assert!(err.contains("No matching provider is authenticated. Use --provider or provider/model."));

    let api_key_res = resolve_cli_model(ResolveCliModelOptions {
        cli_provider: None,
        cli_model: Some("anthropic/claude-opus-5.5"),
        cli_thinking: None,
        inline_api_key: Some(true),
        model_runtime: &authed_rt,
    });
    assert!(api_key_res.model.is_none());
    assert!(
        api_key_res
            .error
            .unwrap()
            .contains("--api-key does not choose a provider.")
    );
}
