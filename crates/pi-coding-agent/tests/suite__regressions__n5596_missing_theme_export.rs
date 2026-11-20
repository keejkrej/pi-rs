//! Port of packages/coding-agent/test/suite/regressions/5596-missing-theme-export.test.ts (unit coding-agent-agent-session-lifecycle)

// @generated-mods begin (scaffold-owned, do not edit)
#[path = "suite__regressions__n5596_missing_theme_export/part_2.rs"]
mod part_2;
// @generated-mods end

#[path = "support/mod.rs"]
mod support;

use std::sync::Arc;

use pi_agent_core::agent::{Agent, AgentInitialState, AgentOptions};
use pi_ai::compat::{register_faux_provider, stream_simple};
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxModelDefinition, FauxResponseStep, RegisterFauxProviderOptions,
    faux_assistant_message,
};
use pi_ai::{ApiKeyCredential, Credential, CredentialStore};
use pi_coding_agent::core::agent_session::{AgentSession, AgentSessionConfig};
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::extensions::types::{ProviderChatModelConfig, ProviderConfig, ProviderModelConfig};
use pi_coding_agent::core::messages::convert_to_llm;
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::settings_manager::{Settings, SettingsManager};
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pretty_assertions::assert_eq;
use serial_test::serial;
use support::model_runtime_test_utils::{create_in_memory_model_registry, get_model_runtime};
use support::suite::harness::create_suite_resource_loader;

#[tokio::test]
#[serial]
async fn exports_with_the_active_fallback_theme_when_the_configured_theme_is_missing() {
    struct ThemeGuard;
    impl Drop for ThemeGuard {
        fn drop(&mut self) {
            init_theme(Some("dark"), None);
        }
    }
    let _theme_guard = ThemeGuard;
    let temp_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-5596-"])).unwrap();
    let faux = register_faux_provider(Some(RegisterFauxProviderOptions {
        models: Some(vec![FauxModelDefinition {
            id: "faux-1".to_string(),
            name: None,
            reasoning: Some(false),
            input: None,
            input_limits: None,
            cost: None,
            context_window: None,
            max_tokens: None,
        }]),
        ..Default::default()
    }));
    faux.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
        FauxAssistantMessageContent::Text("hello".to_string()),
        None,
    ))]);

    let model = faux.get_model();
    let auth_storage = Arc::new(AuthStorage::in_memory(None));
    auth_storage
        .modify(
            &model.provider,
            Box::new(|_prev| {
                Box::pin(async move {
                    Ok(Some(Credential::ApiKey(ApiKeyCredential {
                        key: Some("faux-key".to_string()),
                        env: None,
                    })))
                })
            }),
            None,
        )
        .await
        .unwrap();

    let model_registry = create_in_memory_model_registry(auth_storage).await.unwrap();
    let models = faux
        .models()
        .iter()
        .map(|m| {
            ProviderModelConfig::Chat(ProviderChatModelConfig {
                id: m.id.clone(),
                name: m.name.clone(),
                api: Some(m.api.clone()),
                base_url: Some(m.base_url.clone()),
                input: m.input.clone(),
                input_limits: m.input_limits.clone(),
                cost: m.cost.clone(),
                headers: None,
                reasoning: m.reasoning,
                thinking_level_map: m.thinking_level_map.clone(),
                prompt_cache: m.prompt_cache.clone(),
                context_window: m.context_window,
                max_tokens: m.max_tokens,
                sampling_params: m.sampling_params.clone(),
                compat: m.compat.clone(),
                sampling_params_by_thinking_level: None,
            })
        })
        .collect();
    model_registry
        .register_provider(
            &model.provider,
            ProviderConfig {
                base_url: Some(model.base_url.clone()),
                api_key: Some("faux-key".to_string()),
                api: Some(faux.api().to_string()),
                models: Some(models),
                ..Default::default()
            }
            .into(),
        )
        .unwrap();

    let settings_manager = SettingsManager::in_memory(
        Some(Settings {
            theme: Some("missing-theme".to_string()),
            ..Default::default()
        }),
        None,
    );
    let session_dir = pi_js::path::join(&[&temp_dir, "sessions"]);
    let session_manager = SessionManager::create(&temp_dir, Some(&session_dir), None).unwrap();

    let agent = Agent::new(AgentOptions {
        get_api_key: Some(Arc::new(|_provider| {
            Box::pin(async { Ok(Some("faux-key".to_string())) })
        })),
        initial_state: Some(AgentInitialState {
            model: Some(model.clone()),
            system_prompt: Some("You are a test assistant.".to_string()),
            tools: Some(vec![]),
            ..Default::default()
        }),
        convert_to_llm: Some(Arc::new(|msgs| Box::pin(async move { Ok(convert_to_llm(&msgs)) }))),
        stream_fn: Some(Arc::new(|m, ctx, opts| {
            Box::pin(async move { Ok(stream_simple(&m, &ctx, opts)) })
        })),
        ..Default::default()
    });

    let session = AgentSession::new(AgentSessionConfig {
        agent,
        session_manager,
        settings_manager: settings_manager.clone(),
        cwd: temp_dir.clone(),
        scoped_models: None,
        resource_loader: create_suite_resource_loader(None),
        custom_tools: None,
        model_runtime: get_model_runtime(&model_registry),
        cache_warmer: None,
        initial_active_tool_names: None,
        uses_default_tools: None,
        allowed_tool_names: None,
        excluded_tool_names: None,
        base_tools_override: None,
        extension_runner_ref: None,
        session_start_event: None,
        initial_model: Some(model),
        initial_thinking_level: None,
        default_tool_modifiers: None,
    });

    session.prompt("hi", None).await.unwrap();
    init_theme(settings_manager.get_theme().as_deref(), None);

    let output_path = pi_js::path::join(&[&temp_dir, "export.html"]);
    let exported = session.export_to_html(Some(&output_path), None).await.unwrap();
    assert_eq!(exported, output_path);
    assert!(pi_js::fs::exists(&output_path));
    assert_eq!(settings_manager.get_theme().as_deref(), Some("missing-theme"));

    session.dispose();
    faux.unregister();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}
