//! Port of packages/coding-agent/test/tool-renderer-examples.test.ts

#[allow(dead_code)]
#[path = "../examples/extensions__built_in_tool_renderer.rs"]
mod built_in_tool_renderer_example;

#[allow(dead_code)]
#[path = "../examples/extensions__minimal_mode.rs"]
mod minimal_mode_example;

use std::sync::Arc;

use async_trait::async_trait;
use pi_ai::compat::get_model;
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::extensions::types::{
    ExtensionFactory, InlineExtension as CoreInlineExtension, ToolDefinition,
};
use pi_coding_agent::core::model_runtime::{ModelRuntime, ModelRuntimeCreateOptions};
use pi_coding_agent::core::resource_loader::{DefaultResourceLoader, DefaultResourceLoaderOptions};
use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::settings_manager::SettingsManager;
use pi_coding_agent::modes::interactive::components::tool_execution::{ToolExecutionComponent, ToolRenderers};
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
use pi_tui::{TUI, TuiMainScreen};
use serde_json::json;
use serial_test::serial;

struct FakeTerminal;

#[async_trait]
impl Terminal for FakeTerminal {
    fn start(&self, _on_input: TerminalInputHandler, _on_resize: TerminalResizeHandler) {}
    fn stop(&self) {}
    async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}
    fn write(&self, _data: &str) {}
    fn columns(&self) -> i64 {
        120
    }
    fn rows(&self) -> i64 {
        40
    }
    fn kitty_protocol_active(&self) -> bool {
        false
    }
    fn move_by(&self, _lines: i64) {}
    fn hide_cursor(&self) {}
    fn show_cursor(&self) {}
    fn clear_line(&self) {}
    fn clear_from_cursor(&self) {}
    fn clear_screen(&self) {}
    fn set_title(&self, _title: &str) {}
    fn set_progress(&self, _active: bool) {}
}

fn create_fake_tui() -> Arc<dyn TUI + Send + Sync> {
    Arc::new(TuiMainScreen::new(Arc::new(FakeTerminal), Some(false), None))
}

struct SessionStateSnapshot {
    system_prompt: String,
    edit_tool_definition: Option<ToolDefinition>,
}

async fn get_session_state(
    temp_dir: &str,
    agent_dir_str: &str,
    extension_factory: Option<ExtensionFactory>,
    tools: Vec<String>,
) -> SessionStateSnapshot {
    let temp_dir = temp_dir.to_string();
    let agent_dir_str = agent_dir_str.to_string();

    let settings_manager = SettingsManager::in_memory(None, None);
    let extension_factories = extension_factory
        .into_iter()
        .map(|f| CoreInlineExtension::Factory(f).into())
        .collect();
    let resource_loader = Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: temp_dir.clone(),
        agent_dir: agent_dir_str.clone(),
        settings_manager: Some(settings_manager.clone()),
        extension_factories,
        no_extensions: true,
        no_skills: true,
        no_prompt_templates: true,
        no_themes: true,
        no_context_files: true,
        ..Default::default()
    }));
    resource_loader.reload(None).await.expect("reload resource loader");
    assert!(resource_loader.get_extensions().errors.is_empty());

    let model_runtime = ModelRuntime::create(ModelRuntimeCreateOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(None))),
        allow_model_network: Some(false),
        ..Default::default()
    })
    .await
    .expect("create model_runtime");

    let created = create_agent_session(Some(CreateAgentSessionOptions {
        cwd: Some(temp_dir.clone()),
        agent_dir: Some(agent_dir_str),
        model: get_model("anthropic", "claude-sonnet-4-5"),
        model_runtime: Some(model_runtime),
        settings_manager: Some(settings_manager),
        session_manager: Some(SessionManager::in_memory(Some(&temp_dir), None, None).expect("session manager")),
        resource_loader: Some(resource_loader),
        tools: Some(tools),
        ..Default::default()
    }))
    .await
    .expect("create_agent_session");

    let system_prompt = created.session.system_prompt();
    let edit_tool_definition = created.session.get_tool_definition("edit");
    created.session.dispose();

    SessionStateSnapshot {
        system_prompt,
        edit_tool_definition,
    }
}

fn render_edit_tool(definition: &ToolDefinition, cwd: &str) -> Vec<String> {
    let component = ToolExecutionComponent::new(
        "edit",
        "edit-shell-test",
        json!({ "path": "notes.txt", "oldText": "before", "newText": "after" }),
        None,
        Some(ToolRenderers::from_tool_definition(definition)),
        create_fake_tui(),
        cwd,
    );
    component.render(40)
}

#[tokio::test]
#[serial]
async fn keeps_the_system_prompt_unchanged_for_built_in_tool_renderer_and_minimal_mode_examples() {
    init_theme(Some("dark"), Some(false));
    let dir = tempfile::tempdir().expect("create tempdir");
    let temp_dir = dir.path().to_string_lossy().into_owned();
    let agent_dir = dir.path().join("agent");
    std::fs::create_dir_all(&agent_dir).expect("create agent dir");
    let agent_dir_str = agent_dir.to_string_lossy().into_owned();

    let baseline_renderer = get_session_state(
        &temp_dir,
        &agent_dir_str,
        None,
        vec![
            "read".to_string(),
            "bash".to_string(),
            "edit".to_string(),
            "write".to_string(),
        ],
    )
    .await;
    let with_renderer = get_session_state(
        &temp_dir,
        &agent_dir_str,
        Some(built_in_tool_renderer_example::create_extension()),
        vec![
            "read".to_string(),
            "bash".to_string(),
            "edit".to_string(),
            "write".to_string(),
        ],
    )
    .await;
    assert_eq!(with_renderer.system_prompt, baseline_renderer.system_prompt);

    let baseline_minimal = get_session_state(
        &temp_dir,
        &agent_dir_str,
        None,
        vec![
            "read".to_string(),
            "bash".to_string(),
            "write".to_string(),
            "edit".to_string(),
            "find".to_string(),
            "grep".to_string(),
            "ls".to_string(),
        ],
    )
    .await;
    let with_minimal = get_session_state(
        &temp_dir,
        &agent_dir_str,
        Some(minimal_mode_example::create_extension()),
        vec![
            "read".to_string(),
            "bash".to_string(),
            "write".to_string(),
            "edit".to_string(),
            "find".to_string(),
            "grep".to_string(),
            "ls".to_string(),
        ],
    )
    .await;
    assert_eq!(with_minimal.system_prompt, baseline_minimal.system_prompt);
}

#[tokio::test]
#[serial]
async fn keeps_minimal_mode_edit_tool_in_the_default_shell() {
    init_theme(Some("dark"), Some(false));
    let dir = tempfile::tempdir().expect("create tempdir");
    let temp_dir = dir.path().to_string_lossy().into_owned();
    let agent_dir = dir.path().join("agent");
    std::fs::create_dir_all(&agent_dir).expect("create agent dir");
    let agent_dir_str = agent_dir.to_string_lossy().into_owned();

    let minimal = get_session_state(
        &temp_dir,
        &agent_dir_str,
        Some(minimal_mode_example::create_extension()),
        vec!["edit".to_string()],
    )
    .await;
    let edit_def = minimal.edit_tool_definition.expect("minimal mode registered edit tool");
    let mut default_shell_def = edit_def.clone();
    default_shell_def.render_shell = None;

    assert_eq!(
        render_edit_tool(&edit_def, &temp_dir),
        render_edit_tool(&default_shell_def, &temp_dir)
    );
}
