//! Port of packages/coding-agent/examples/sdk/14-codemode-mcp.ts

use std::process::ExitCode;
use std::sync::Arc;

use pi_ai::types::AssistantMessageEvent;
use pi_coding_agent::config::get_agent_dir;
use pi_coding_agent::core::agent_session::AgentSessionEvent;
use pi_coding_agent::core::extensions::types::{CodemodeMode, InlineExtension};
use pi_coding_agent::core::resource_loader::{DefaultResourceLoader, DefaultResourceLoaderOptions};
use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::settings_manager::{Settings, SettingsManager};
use pi_coding_agent::extensions::codemode::{CodemodeExtensionOptions, create_codemode_extension};
use pi_coding_agent::extensions::mcp::{McpExtensionOptions, create_mcp_extension};
use pi_coding_agent::extensions::tool_search::create_tool_search_extension;

async fn run() -> pi_coding_agent::Result<()> {
    let cwd = pi_js::env::cwd();

    let resource_loader = Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: cwd.clone(),
        agent_dir: get_agent_dir(),
        extension_factories: vec![
            InlineExtension::Factory(create_codemode_extension(CodemodeExtensionOptions {
                mode: Some(CodemodeMode::On),
                ..Default::default()
            }))
            .into(),
            InlineExtension::Factory(create_tool_search_extension()).into(),
            InlineExtension::Factory(create_mcp_extension(McpExtensionOptions::default())).into(),
        ],
        ..Default::default()
    }));
    resource_loader.reload(None).await?;

    let settings_manager = SettingsManager::create(&cwd, None, None);
    settings_manager.apply_overrides(&Settings {
        default_tools: Some(vec!["+codemode".to_string(), "+tool_search".to_string()]),
        ..Default::default()
    });

    let created = create_agent_session(Some(CreateAgentSessionOptions {
        resource_loader: Some(resource_loader),
        settings_manager: Some(settings_manager),
        session_manager: Some(SessionManager::in_memory(None, None, None)?),
        ..Default::default()
    }))
    .await?;
    let session = created.session;

    session.bind_extensions(Default::default()).await?;
    pi_js::console::log(&format!("Active tools: {}", session.get_active_tool_names().join(", ")));
    let _unsub = session.subscribe(|event| {
        if let AgentSessionEvent::MessageUpdate {
            assistant_message_event: AssistantMessageEvent::TextDelta { delta, .. },
            ..
        } = event
        {
            pi_js::console::stdout_write(&delta);
        }
    });
    let res = session
        .prompt(
            "Use codemode to count the Rust files in src/ and list the three largest.",
            None,
        )
        .await;
    pi_js::console::log("");
    session.dispose();
    res
}

fn main() -> ExitCode {
    pi_js::ensure_crypto_provider();
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(err) => {
            pi_js::console::error(&format!("Failed to build runtime: {err}"));
            return ExitCode::from(1);
        }
    };
    match rt.block_on(run()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            pi_js::console::error(&format!("Error: {err}"));
            ExitCode::from(1)
        }
    }
}
