//! Port of packages/coding-agent/examples/sdk/04-skills.ts

use std::process::ExitCode;
use std::sync::Arc;

use pi_coding_agent::config::get_agent_dir;
use pi_coding_agent::core::resource_loader::{DefaultResourceLoader, DefaultResourceLoaderOptions, SkillsResult};
use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::skills::Skill;
use pi_coding_agent::core::source_info::{SyntheticSourceOptions, create_synthetic_source_info};

async fn run() -> pi_coding_agent::Result<()> {
    let custom_skill = Skill {
        name: "my-skill".to_string(),
        description: "Custom project instructions".to_string(),
        file_path: "/virtual/SKILL.md".to_string(),
        base_dir: "/virtual".to_string(),
        source_info: create_synthetic_source_info(
            "/virtual/SKILL.md",
            SyntheticSourceOptions {
                source: "sdk".to_string(),
                ..Default::default()
            },
        ),
        disable_model_invocation: false,
    };

    let loader = Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: pi_js::env::cwd(),
        agent_dir: get_agent_dir(),
        skills_override: Some(Arc::new(move |current| {
            let mut filtered: Vec<Skill> = current
                .skills
                .into_iter()
                .filter(|s| s.name.contains("browser") || s.name.contains("search"))
                .collect();
            filtered.push(custom_skill.clone());
            SkillsResult {
                skills: filtered,
                diagnostics: current.diagnostics,
            }
        })),
        ..Default::default()
    }));
    loader.reload(None).await?;

    let skills_res = loader.get_skills();
    let names: Vec<&str> = skills_res.skills.iter().map(|s| s.name.as_str()).collect();
    pi_js::console::log(&format!("Discovered skills: {}", names.join(", ")));
    if !skills_res.diagnostics.is_empty() {
        pi_js::console::log(&format!("Warnings: {:?}", skills_res.diagnostics));
    }

    let created = create_agent_session(Some(CreateAgentSessionOptions {
        resource_loader: Some(loader),
        session_manager: Some(SessionManager::in_memory(None, None, None)?),
        ..Default::default()
    }))
    .await?;
    pi_js::console::log("Session created with filtered skills");
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
