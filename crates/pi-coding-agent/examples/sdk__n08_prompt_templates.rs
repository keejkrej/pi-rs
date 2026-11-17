//! Port of packages/coding-agent/examples/sdk/08-prompt-templates.ts

use std::process::ExitCode;
use std::sync::Arc;

use pi_coding_agent::config::get_agent_dir;
use pi_coding_agent::core::prompt_templates::PromptTemplate;
use pi_coding_agent::core::resource_loader::{
    DefaultResourceLoader, DefaultResourceLoaderOptions, LoadPromptsResult, ResourceLoader,
};
use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::source_info::{SyntheticSourceOptions, create_synthetic_source_info};

async fn run() -> pi_coding_agent::Result<()> {
    let deploy_template = PromptTemplate {
        name: "deploy".to_string(),
        description: "Deploy the application".to_string(),
        file_path: "/virtual/prompts/deploy.md".to_string(),
        source_info: create_synthetic_source_info(
            "/virtual/prompts/deploy.md",
            SyntheticSourceOptions {
                source: "sdk".to_string(),
                ..Default::default()
            },
        ),
        content: "# Deploy Instructions\n\n1. Build: cargo build\n2. Test: cargo test\n3. Deploy".to_string(),
    };

    let loader = Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: Some(pi_js::env::cwd()),
        agent_dir: Some(get_agent_dir()),
        prompts_override: Some(Arc::new(move |mut current| {
            current.prompts.push(deploy_template.clone());
            LoadPromptsResult {
                prompts: current.prompts,
                diagnostics: current.diagnostics,
            }
        })),
        ..Default::default()
    }));
    loader.reload(None).await?;

    let discovered = loader.get_prompts().prompts;
    pi_js::console::log("Discovered prompt templates:");
    for template in &discovered {
        pi_js::console::log(&format!("  /{}: {}", template.name, template.description));
    }

    let created = create_agent_session(Some(CreateAgentSessionOptions {
        resource_loader: Some(loader),
        session_manager: Some(SessionManager::in_memory(None, None, None)?),
        ..Default::default()
    }))
    .await?;
    pi_js::console::log(&format!(
        "Session created with {} prompt templates",
        discovered.len() + 1
    ));
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
