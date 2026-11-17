//! Port of packages/coding-agent/examples/sdk/05-tools.ts

use std::process::ExitCode;

use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};
use pi_coding_agent::core::session_manager::SessionManager;

async fn run() -> pi_coding_agent::Result<()> {
    let created_ro = create_agent_session(Some(CreateAgentSessionOptions {
        tools: Some(vec![
            "read".to_string(),
            "grep".to_string(),
            "find".to_string(),
            "ls".to_string(),
        ]),
        session_manager: Some(SessionManager::in_memory(None, None, None)?),
        ..Default::default()
    }))
    .await?;
    pi_js::console::log("Read-only session created");
    created_ro.session.dispose();

    let created_custom = create_agent_session(Some(CreateAgentSessionOptions {
        tools: Some(vec!["read".to_string(), "bash".to_string(), "grep".to_string()]),
        session_manager: Some(SessionManager::in_memory(None, None, None)?),
        ..Default::default()
    }))
    .await?;
    pi_js::console::log("Custom tools session created");
    created_custom.session.dispose();

    let custom_cwd = "/path/to/project";
    let created_cwd = create_agent_session(Some(CreateAgentSessionOptions {
        cwd: Some(custom_cwd.to_string()),
        tools: Some(vec![
            "read".to_string(),
            "bash".to_string(),
            "edit".to_string(),
            "write".to_string(),
        ]),
        session_manager: Some(SessionManager::in_memory(Some(custom_cwd), None, None)?),
        ..Default::default()
    }))
    .await?;
    pi_js::console::log("Custom cwd session created");
    created_cwd.session.dispose();

    let created_specific = create_agent_session(Some(CreateAgentSessionOptions {
        cwd: Some(custom_cwd.to_string()),
        tools: Some(vec!["read".to_string(), "bash".to_string(), "grep".to_string()]),
        session_manager: Some(SessionManager::in_memory(Some(custom_cwd), None, None)?),
        ..Default::default()
    }))
    .await?;
    pi_js::console::log("Specific tools with custom cwd session created");
    created_specific.session.dispose();

    Ok(())
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
