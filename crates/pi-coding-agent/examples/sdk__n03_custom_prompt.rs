//! Port of packages/coding-agent/examples/sdk/03-custom-prompt.ts

use std::process::ExitCode;
use std::sync::Arc;

use pi_ai::types::AssistantMessageEvent;
use pi_coding_agent::config::get_agent_dir;
use pi_coding_agent::core::agent_session::AgentSessionEvent;
use pi_coding_agent::core::resource_loader::{DefaultResourceLoader, DefaultResourceLoaderOptions};
use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};
use pi_coding_agent::core::session_manager::SessionManager;

async fn run() -> pi_coding_agent::Result<()> {
    let cwd = pi_js::env::cwd();
    let agent_dir = get_agent_dir();

    // Option 1: Replace prompt entirely
    let loader1 = Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: cwd.clone(),
        agent_dir: agent_dir.clone(),
        system_prompt_override: Some(Arc::new(|_| {
            Some(
                "You are a helpful assistant that speaks like a pirate.\nAlways end responses with \"Arrr!\""
                    .to_string(),
            )
        })),
        append_system_prompt_override: Some(Arc::new(|_| Vec::new())),
        ..Default::default()
    }));
    loader1.reload(None).await?;

    let created1 = create_agent_session(Some(CreateAgentSessionOptions {
        resource_loader: Some(loader1),
        session_manager: Some(SessionManager::in_memory(None, None, None)?),
        ..Default::default()
    }))
    .await?;
    let session1 = created1.session;
    let _unsub1 = session1.subscribe(|event| {
        if let AgentSessionEvent::MessageUpdate {
            assistant_message_event: AssistantMessageEvent::TextDelta { delta, .. },
            ..
        } = event
        {
            pi_js::console::stdout_write(&delta);
        }
    });
    pi_js::console::log("=== Replace prompt ===");
    let res1 = session1.prompt("What is 2 + 2?", None).await;
    pi_js::console::log("\n");
    session1.dispose();
    res1?;

    // Option 2: Append instructions to the default prompt
    let loader2 = Arc::new(DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd,
        agent_dir,
        append_system_prompt_override: Some(Arc::new(|mut base| {
            base.push(
                "## Additional Instructions\n- Always be concise\n- Use bullet points when listing things".to_string(),
            );
            base
        })),
        ..Default::default()
    }));
    loader2.reload(None).await?;

    let created2 = create_agent_session(Some(CreateAgentSessionOptions {
        resource_loader: Some(loader2),
        session_manager: Some(SessionManager::in_memory(None, None, None)?),
        ..Default::default()
    }))
    .await?;
    let session2 = created2.session;
    let _unsub2 = session2.subscribe(|event| {
        if let AgentSessionEvent::MessageUpdate {
            assistant_message_event: AssistantMessageEvent::TextDelta { delta, .. },
            ..
        } = event
        {
            pi_js::console::stdout_write(&delta);
        }
    });
    pi_js::console::log("=== Modify prompt ===");
    let res2 = session2.prompt("List 3 benefits of Rust.", None).await;
    pi_js::console::log("");
    session2.dispose();
    res2
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
