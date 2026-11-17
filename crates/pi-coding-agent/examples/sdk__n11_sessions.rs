//! Port of packages/coding-agent/examples/sdk/11-sessions.ts

use std::process::ExitCode;

use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};
use pi_coding_agent::core::session_manager::SessionManager;

async fn run() -> pi_coding_agent::Result<()> {
    let cwd = pi_js::env::cwd();

    let in_mem = create_agent_session(Some(CreateAgentSessionOptions {
        session_manager: Some(SessionManager::in_memory(None, None, None)?),
        ..Default::default()
    }))
    .await?;
    pi_js::console::log(&format!(
        "In-memory session: {}",
        in_mem.session.session_file().as_deref().unwrap_or("(none)")
    ));
    in_mem.session.dispose();

    let new_sess = create_agent_session(Some(CreateAgentSessionOptions {
        session_manager: Some(SessionManager::create(&cwd, None, None)?),
        ..Default::default()
    }))
    .await?;
    pi_js::console::log(&format!(
        "New session file: {}",
        new_sess.session.session_file().as_deref().unwrap_or("(none)")
    ));
    new_sess.session.dispose();

    let continued = create_agent_session(Some(CreateAgentSessionOptions {
        session_manager: Some(SessionManager::continue_recent(&cwd, None)?),
        ..Default::default()
    }))
    .await?;
    if let Some(note) = continued.model_fallback_message {
        pi_js::console::log(&format!("Note: {note}"));
    }
    pi_js::console::log(&format!(
        "Continued session: {}",
        continued.session.session_file().as_deref().unwrap_or("(none)")
    ));
    continued.session.dispose();

    let sessions = SessionManager::list(&cwd, None, None, None).await?;
    pi_js::console::log(&format!("\nFound {} sessions:", sessions.len()));
    for info in sessions.iter().take(3) {
        let id_prefix: String = info.id.chars().take(8).collect();
        let msg_prefix: String = info.first_message.chars().take(30).collect();
        pi_js::console::log(&format!("  {id_prefix}... - \"{msg_prefix}...\""));
    }

    if let Some(first) = sessions.first() {
        let opened = create_agent_session(Some(CreateAgentSessionOptions {
            session_manager: Some(SessionManager::open(&first.path, None, None)?),
            ..Default::default()
        }))
        .await?;
        pi_js::console::log(&format!("\nOpened: {}", opened.session.session_id()));
        opened.session.dispose();
    }

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
