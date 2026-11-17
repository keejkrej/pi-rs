//! Port of packages/coding-agent/test/suite/regressions/2753-reload-stale-resource-settings.test.ts (unit coding-agent-session-bootstrap)

// @generated-mods begin (scaffold-owned, do not edit)
#[path = "suite__regressions__n2753_reload_stale_resource_settings/part_2.rs"]
mod part_2;
// @generated-mods end

use std::sync::Arc;

use indexmap::IndexMap;
use pi_ai::providers::faux::{FauxModelDefinition, RegisterFauxProviderOptions, register_faux_provider};
use pi_ai::{ApiKeyCredential, Credential, CredentialStore};
use pi_coding_agent::core::agent_session_runtime::{
    CreateAgentSessionFromServicesOptions, CreateAgentSessionRuntimeFactory, CreateAgentSessionRuntimeOptions,
    create_agent_session_from_services, create_agent_session_runtime, create_agent_session_services,
};
use pi_coding_agent::core::agent_session_services::CreateAgentSessionServicesOptions;
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::extensions::types::{
    ExtensionApi, InlineExtension, ProviderChatModelConfig, ProviderConfig, ProviderModelConfig,
};
use pi_coding_agent::core::model_runtime::{ModelRuntime, ModelRuntimeCreateOptions};
use pi_coding_agent::core::resource_loader::DefaultResourceLoaderOptions;
use pi_coding_agent::core::session_manager::SessionManager;
use pretty_assertions::assert_eq;
use serde_json::json;

#[tokio::test]
async fn applies_updated_top_level_prompt_settings_on_reload_after_startup() {
    let temp_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-2753-"])).unwrap();
    let agent_dir = pi_js::path::join(&[&temp_dir, "agent"]);
    let prompts_dir = pi_js::path::join(&[&agent_dir, "prompts"]);
    pi_js::fs::mkdir(&prompts_dir, true).unwrap();
    pi_js::fs::write_file(
        &pi_js::path::join(&[&prompts_dir, "test.md"]),
        b"Echo test prompt\n",
        None,
    )
    .unwrap();

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

    let auth_storage = Arc::new(AuthStorage::in_memory(None));
    let provider_id = faux.get_model().provider;
    auth_storage
        .modify(
            &provider_id,
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

    let model_runtime = ModelRuntime::create(ModelRuntimeCreateOptions {
        credentials: Some(auth_storage),
        models_path: Some(pi_js::path::join(&[&agent_dir, "models.json"])),
        allow_model_network: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();

    let agent_dir_for_factory = agent_dir.clone();
    let mr_for_factory = model_runtime.clone();
    let faux_for_factory = faux.clone();
    let create_runtime: CreateAgentSessionRuntimeFactory = Arc::new(move |opts| {
        let agent_dir_inner = agent_dir_for_factory.clone();
        let mr_inner = mr_for_factory.clone();
        let faux_inner = faux_for_factory.clone();
        Box::pin(async move {
            let faux_for_ext = faux_inner.clone();
            let factory = Arc::new(move |pi: ExtensionApi| {
                let faux_ext = faux_for_ext.clone();
                async move {
                    let base_model = faux_ext.get_model();
                    let models = faux_ext
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
                            })
                        })
                        .collect();
                    let mut headers = IndexMap::new();
                    let _ = &mut headers;
                    pi.register_provider(
                        &base_model.provider,
                        ProviderConfig {
                            base_url: Some(base_model.base_url),
                            api_key: Some("faux-key".to_string()),
                            api: Some(faux_ext.api().to_string()),
                            models: Some(models),
                            ..Default::default()
                        },
                    )?;
                    Ok(())
                }
            });

            let services = create_agent_session_services(CreateAgentSessionServicesOptions {
                cwd: opts.cwd,
                agent_dir: Some(agent_dir_inner),
                model_runtime: Some(mr_inner),
                resource_loader_options: Some(DefaultResourceLoaderOptions {
                    extension_factories: Some(vec![InlineExtension::Factory(factory)]),
                    no_skills: Some(true),
                    no_themes: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await?;

            let created = create_agent_session_from_services(CreateAgentSessionFromServicesOptions {
                services: services.clone(),
                session_manager: opts.session_manager,
                session_start_event: opts.session_start_event,
                model: Some(faux_inner.get_model()),
                thinking_level: None,
                scoped_models: None,
                tools: None,
                exclude_tools: None,
                no_tools: None,
                custom_tools: None,
            })
            .await?;

            let diagnostics = services.diagnostics.clone();
            Ok(
                pi_coding_agent::core::agent_session_runtime::CreateAgentSessionRuntimeResult::from_session_result(
                    created,
                    services,
                    diagnostics,
                ),
            )
        })
    });

    let runtime = create_agent_session_runtime(
        create_runtime,
        CreateAgentSessionRuntimeOptions {
            cwd: temp_dir.clone(),
            agent_dir: agent_dir.clone(),
            session_manager: SessionManager::create(&temp_dir, None, None).unwrap(),
            session_start_event: None,
        },
    )
    .await
    .unwrap();

    assert!(runtime.session().prompt_templates().iter().any(|p| p.name == "test"));

    let settings_json = pi_js::json::stringify_pretty(&json!({ "prompts": ["-prompts/test.md"] }), 2);
    pi_js::fs::write_file(
        &pi_js::path::join(&[&agent_dir, "settings.json"]),
        format!("{settings_json}\n").as_bytes(),
        None,
    )
    .unwrap();

    runtime.session().reload().await.unwrap();

    assert_eq!(
        runtime.services().settings_manager.get_global_settings().prompts,
        Some(vec!["-prompts/test.md".to_string()])
    );
    assert!(!runtime.session().prompt_templates().iter().any(|p| p.name == "test"));

    runtime.session().dispose();
    faux.unregister();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}
