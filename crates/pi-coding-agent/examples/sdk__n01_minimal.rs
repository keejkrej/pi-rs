//! Port of packages/coding-agent/examples/sdk/01-minimal.ts

use std::process::ExitCode;

use pi_ai::types::AssistantMessageEvent;
use pi_coding_agent::core::agent_session::AgentSessionEvent;
use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};

async fn run() -> pi_coding_agent::Result<()> {
    let created = create_agent_session(Some(CreateAgentSessionOptions::default())).await?;
    let session = created.session;

    let _unsub = session.subscribe(|event| {
        if let AgentSessionEvent::MessageUpdate {
            assistant_message_event: AssistantMessageEvent::TextDelta { delta, .. },
            ..
        } = event
        {
            pi_js::console::stdout_write(&delta);
        }
    });

    let prompt_res = session.prompt("What files are in the current directory?", None).await;
    for msg in session.state().messages {
        if let Ok(json) = pi_js::json::stringify(&msg) {
            pi_js::console::log(&json);
        }
    }
    pi_js::console::log("");
    session.dispose();
    prompt_res
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
