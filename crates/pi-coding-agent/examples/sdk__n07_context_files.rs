//! Port of packages/coding-agent/examples/sdk/07-context-files.ts

use std::process::ExitCode;
use std::sync::Arc;

use pi_coding_agent::config::get_agent_dir;
use pi_coding_agent::core::resource_loader::{ContextFile, DefaultResourceLoader, DefaultResourceLoaderOptions};
use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};
use pi_coding_agent::core::session_manager::SessionManager;

async fn run() -> pi_coding_agent::Result<()> {
    let loader = Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: pi_js::env::cwd(),
        agent_dir: get_agent_dir(),
        agents_files_override: Some(Arc::new(|mut current| {
            current.push(ContextFile {
                path: "/virtual/AGENTS.md".to_string(),
                content: "# Project Guidelines\n\n## Code Style\n- Prefer Result over panic\n- Prefer immutable data\n"
                    .to_string(),
            });
            current
        })),
        ..Default::default()
    }));
    loader.reload(None).await?;

    let discovered = loader.get_agents_files();
    pi_js::console::log("Discovered context files:");
    for file in &discovered {
        pi_js::console::log(&format!("  - {} ({} chars)", file.path, file.content.len()));
    }

    let created = create_agent_session(Some(CreateAgentSessionOptions {
        resource_loader: Some(loader),
        session_manager: Some(SessionManager::in_memory(None, None, None)?),
        ..Default::default()
    }))
    .await?;
    pi_js::console::log(&format!("Session created with {} context files", discovered.len() + 1));
    created.session.dispose();
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
