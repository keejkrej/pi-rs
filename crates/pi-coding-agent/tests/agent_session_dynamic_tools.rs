//! Port of packages/coding-agent/test/agent-session-dynamic-tools.test.ts

use std::sync::{Arc, Mutex, PoisonError};

use indexmap::IndexMap;
use pi_agent_core::types::AgentToolResult;
use pi_ai::compat::get_model;
use pi_ai::types::{TextContent, UserContentBlock};
use pi_coding_agent::core::agent_session::ExtensionBindings;
use pi_coding_agent::core::extensions::types::{ExtensionApi, InlineExtension, SessionStartEvent, ToolDefinition};
use pi_coding_agent::core::resource_loader::{DefaultResourceLoader, DefaultResourceLoaderOptions, ResourceLoader};
use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};
use pi_coding_agent::core::session_manager::{NewSessionOptions, SessionManager};
use pi_coding_agent::core::settings_manager::SettingsManager;
use pi_coding_agent::core::source_info::{SourceOrigin, SourceScope};
use pi_coding_agent::core::tools::bash::{BashToolOptions, create_bash_tool_definition};
use pi_js::vendor::typebox::t;
use pretty_assertions::assert_eq;
use serde_json::json;

fn empty_tool_def(name: &str, label: &str, description: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        label: label.to_string(),
        description: description.to_string(),
        prompt_snippet: None,
        prompt_guidelines: None,
        parameters: t::object([] as [(&str, pi_js::vendor::typebox::Schema); 0]),
        constrained_sampling: None,
        render_shell: None,
        prepare_arguments: None,
        output_schema: None,
        exposure: None,
        namespace: None,
        annotations: None,
        default_active: None,
        prepare_loadout: None,
        execution_mode: None,
        execute: Arc::new(|_id, _params, _sig, _upd, _ctx| {
            Box::pin(async move {
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new("ok"))],
                    details: Some(json!({})),
                    ..Default::default()
                })
            })
        }),
        render_call: None,
        render_result: None,
    }
}

fn setup_temp_dirs() -> (String, String) {
    let temp_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-dynamic-tool-test-"])).unwrap();
    let agent_dir = pi_js::path::join(&[&temp_dir, "agent"]);
    pi_js::fs::mkdir(&agent_dir, true, None).unwrap();
    (temp_dir, agent_dir)
}

#[tokio::test]
async fn exposes_session_state_before_custom_bash_spawn_hooks_and_supports_opting_out() {
    let (temp_dir, agent_dir) = setup_temp_dirs();
    let settings_manager = SettingsManager::create(&temp_dir, Some(&agent_dir), None);
    let session_dir = pi_js::path::join(&[&agent_dir, "sessions"]);
    let session_manager = SessionManager::create(
        &temp_dir,
        Some(&session_dir),
        Some(NewSessionOptions {
            id: Some("bash-env-test".to_string()),
            ..Default::default()
        }),
    )
    .unwrap();

    let session_env: Arc<Mutex<Option<IndexMap<String, String>>>> = Arc::new(Mutex::new(None));
    let opted_out_env: Arc<Mutex<Option<IndexMap<String, String>>>> = Arc::new(Mutex::new(None));

    let session_env_ref = Arc::clone(&session_env);
    let opted_out_env_ref = Arc::clone(&opted_out_env);
    let temp_dir_for_ext = temp_dir.clone();

    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let dir = temp_dir_for_ext.clone();
        let se = Arc::clone(&session_env_ref);
        let oe = Arc::clone(&opted_out_env_ref);
        async move {
            pi.register_tool(create_bash_tool_definition(
                &dir,
                Some(BashToolOptions {
                    spawn_hook: Some(Arc::new(move |ctx| {
                        *se.lock().unwrap_or_else(PoisonError::into_inner) = Some(ctx.env.clone());
                        ctx
                    })),
                    ..Default::default()
                }),
            ))?;
            let mut opted = create_bash_tool_definition(
                &dir,
                Some(BashToolOptions {
                    expose_session_environment: Some(false),
                    spawn_hook: Some(Arc::new(move |ctx| {
                        *oe.lock().unwrap_or_else(PoisonError::into_inner) = Some(ctx.env.clone());
                        ctx
                    })),
                    ..Default::default()
                }),
            );
            opted.name = "bash_without_session_env".to_string();
            opted.label = "bash without session env".to_string();
            pi.register_tool(opted)?;
            Ok(())
        }
    }));

    let resource_loader = Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: temp_dir.clone(),
        agent_dir: agent_dir.clone(),
        settings_manager: Some(settings_manager.clone()),
        extension_factories: vec![pi_coding_agent::core::resource_loader::InlineExtension::Core(ext)],
        ..Default::default()
    }));
    resource_loader.reload(None).await.unwrap();

    let model = get_model("anthropic", "claude-sonnet-4-5").unwrap();
    let created = create_agent_session(Some(CreateAgentSessionOptions {
        cwd: Some(temp_dir.clone()),
        agent_dir: Some(agent_dir),
        model: Some(model.clone()),
        thinking_level: Some(pi_ai::ModelThinkingLevel::High),
        settings_manager: Some(settings_manager),
        session_manager: Some(session_manager),
        resource_loader: Some(resource_loader),
        ..Default::default()
    }))
    .await
    .unwrap();
    let session = created.session;

    let bash_tool = session
        .agent()
        .state()
        .tools
        .into_iter()
        .find(|t| t.name == "bash")
        .unwrap();
    assert!(
        session
            .system_prompt()
            .contains("You can inspect PI_* environment variables for current model and session details.")
    );
    (bash_tool.execute)("bash-env".to_string(), json!({ "command": "printf ok" }), None, None)
        .await
        .unwrap();

    let captured_env = session_env
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .unwrap();
    assert_eq!(captured_env.get("PI_SESSION_ID"), Some(&session.session_id()));
    assert_eq!(
        captured_env.get("PI_SESSION_FILE").map(String::as_str),
        session.session_file().as_deref()
    );
    assert_eq!(captured_env.get("PI_PROVIDER"), Some(&model.provider));
    assert_eq!(captured_env.get("PI_MODEL"), Some(&model.id));
    assert_eq!(
        captured_env.get("PI_REASONING_LEVEL").map(String::as_str),
        Some(session.thinking_level().as_str())
    );

    let opted_tool = session
        .agent()
        .state()
        .tools
        .into_iter()
        .find(|t| t.name == "bash_without_session_env")
        .unwrap();
    (opted_tool.execute)("bash-no-env".to_string(), json!({ "command": "printf ok" }), None, None)
        .await
        .unwrap();

    let opted_captured = opted_out_env
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .unwrap();
    assert!(!opted_captured.contains_key("PI_SESSION_ID"));
    assert!(!opted_captured.contains_key("PI_SESSION_FILE"));
    assert!(!opted_captured.contains_key("PI_PROVIDER"));
    assert!(!opted_captured.contains_key("PI_MODEL"));
    assert!(!opted_captured.contains_key("PI_REASONING_LEVEL"));

    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn refreshes_tool_registry_when_tools_are_registered_after_initialization() {
    let (temp_dir, agent_dir) = setup_temp_dirs();
    let settings_manager = SettingsManager::create(&temp_dir, Some(&agent_dir), None);
    let session_manager = SessionManager::in_memory(Some(&temp_dir), None, None).unwrap();

    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        let pi_ev = pi.clone();
        pi.on::<SessionStartEvent>(move |_ev, _ctx| {
            let pi_handle = pi_ev.clone();
            Box::pin(async move {
                let mut def = empty_tool_def("dynamic_tool", "Dynamic Tool", "Tool registered from session_start");
                def.prompt_snippet = Some("Run dynamic test behavior".to_string());
                def.prompt_guidelines = Some(vec![
                    "Use dynamic_tool when the user asks for dynamic behavior tests.".to_string(),
                ]);
                pi_handle.register_tool(def)?;
                Ok(None)
            })
        });
        Ok(())
    }));

    let resource_loader = Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: temp_dir.clone(),
        agent_dir: agent_dir.clone(),
        settings_manager: Some(settings_manager.clone()),
        extension_factories: vec![pi_coding_agent::core::resource_loader::InlineExtension::Core(ext)],
        ..Default::default()
    }));
    resource_loader.reload(None).await.unwrap();

    let created = create_agent_session(Some(CreateAgentSessionOptions {
        cwd: Some(temp_dir.clone()),
        agent_dir: Some(agent_dir),
        model: Some(get_model("anthropic", "claude-sonnet-4-5").unwrap()),
        settings_manager: Some(settings_manager),
        session_manager: Some(session_manager),
        resource_loader: Some(resource_loader),
        ..Default::default()
    }))
    .await
    .unwrap();
    let session = created.session;

    assert!(!session.get_all_tools().iter().any(|t| t.name == "dynamic_tool"));

    session.bind_extensions(ExtensionBindings::default()).await.unwrap();

    let all_tools = session.get_all_tools();
    let dynamic_tool = all_tools.iter().find(|t| t.name == "dynamic_tool").unwrap();
    let read_tool = all_tools.iter().find(|t| t.name == "read").unwrap();

    assert_eq!(
        dynamic_tool.prompt_guidelines,
        Some(vec![
            "Use dynamic_tool when the user asks for dynamic behavior tests.".to_string()
        ])
    );
    assert_eq!(dynamic_tool.source_info.path, "<inline:1>");
    assert_eq!(dynamic_tool.source_info.source, "inline");
    assert_eq!(dynamic_tool.source_info.scope, SourceScope::Temporary);
    assert_eq!(dynamic_tool.source_info.origin, SourceOrigin::TopLevel);
    assert_eq!(read_tool.source_info.path, "builtin:read");
    assert_eq!(read_tool.source_info.source, "builtin");
    assert_eq!(read_tool.source_info.scope, SourceScope::Temporary);
    assert_eq!(read_tool.source_info.origin, SourceOrigin::TopLevel);
    assert!(session.get_active_tool_names().iter().any(|n| n == "dynamic_tool"));
    assert!(
        session
            .system_prompt()
            .contains("- dynamic_tool: Run dynamic test behavior")
    );
    assert!(
        session
            .system_prompt()
            .contains("- Use dynamic_tool when the user asks for dynamic behavior tests.")
    );

    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn returns_source_metadata_for_sdk_custom_tools() {
    let (temp_dir, agent_dir) = setup_temp_dirs();
    let settings_manager = SettingsManager::create(&temp_dir, Some(&agent_dir), None);
    let session_manager = SessionManager::in_memory(Some(&temp_dir), None, None).unwrap();

    let resource_loader = Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: temp_dir.clone(),
        agent_dir: agent_dir.clone(),
        settings_manager: Some(settings_manager.clone()),
        ..Default::default()
    }));
    resource_loader.reload(None).await.unwrap();

    let created = create_agent_session(Some(CreateAgentSessionOptions {
        cwd: Some(temp_dir.clone()),
        agent_dir: Some(agent_dir),
        model: Some(get_model("anthropic", "claude-sonnet-4-5").unwrap()),
        settings_manager: Some(settings_manager),
        session_manager: Some(session_manager),
        resource_loader: Some(resource_loader),
        custom_tools: Some(vec![empty_tool_def(
            "sdk_tool",
            "SDK Tool",
            "Tool registered through createAgentSession",
        )]),
        ..Default::default()
    }))
    .await
    .unwrap();
    let session = created.session;

    let all = session.get_all_tools();
    let sdk_tool = all.iter().find(|t| t.name == "sdk_tool").unwrap();
    assert_eq!(sdk_tool.source_info.path, "<sdk:sdk_tool>");
    assert_eq!(sdk_tool.source_info.source, "sdk");
    assert_eq!(sdk_tool.source_info.scope, SourceScope::Temporary);
    assert_eq!(sdk_tool.source_info.origin, SourceOrigin::TopLevel);
    assert!(session.get_active_tool_names().iter().any(|n| n == "sdk_tool"));

    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn keeps_custom_tools_active_but_omits_them_from_available_tools_when_prompt_snippet_is_not_provided() {
    let (temp_dir, agent_dir) = setup_temp_dirs();
    let settings_manager = SettingsManager::create(&temp_dir, Some(&agent_dir), None);
    let session_manager = SessionManager::in_memory(Some(&temp_dir), None, None).unwrap();

    let ext = InlineExtension::Factory(Arc::new(|pi: ExtensionApi| async move {
        let pi_ev = pi.clone();
        pi.on::<SessionStartEvent>(move |_ev, _ctx| {
            let pi_handle = pi_ev.clone();
            Box::pin(async move {
                pi_handle.register_tool(empty_tool_def(
                    "hidden_tool",
                    "Hidden Tool",
                    "Description should not appear in available tools",
                ))?;
                Ok(None)
            })
        });
        Ok(())
    }));

    let resource_loader = Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: temp_dir.clone(),
        agent_dir: agent_dir.clone(),
        settings_manager: Some(settings_manager.clone()),
        extension_factories: vec![pi_coding_agent::core::resource_loader::InlineExtension::Core(ext)],
        ..Default::default()
    }));
    resource_loader.reload(None).await.unwrap();

    let created = create_agent_session(Some(CreateAgentSessionOptions {
        cwd: Some(temp_dir.clone()),
        agent_dir: Some(agent_dir),
        model: Some(get_model("anthropic", "claude-sonnet-4-5").unwrap()),
        settings_manager: Some(settings_manager),
        session_manager: Some(session_manager),
        resource_loader: Some(resource_loader),
        ..Default::default()
    }))
    .await
    .unwrap();
    let session = created.session;

    session.bind_extensions(ExtensionBindings::default()).await.unwrap();

    assert!(session.get_all_tools().iter().any(|t| t.name == "hidden_tool"));
    assert!(session.get_active_tool_names().iter().any(|n| n == "hidden_tool"));
    assert!(!session.system_prompt().contains("hidden_tool"));
    assert!(
        !session
            .system_prompt()
            .contains("Description should not appear in available tools")
    );

    session.dispose();
    let _ = pi_js::fs::rm(&temp_dir, true, true);
}
