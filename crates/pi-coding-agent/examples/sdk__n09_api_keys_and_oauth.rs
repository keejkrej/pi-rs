//! Port of packages/coding-agent/examples/sdk/09-api-keys-and-oauth.ts

use std::process::ExitCode;

use pi_coding_agent::core::model_runtime::{ModelRuntime, ModelRuntimeCreateOptions};
use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};
use pi_coding_agent::core::session_manager::SessionManager;

async fn run() -> pi_coding_agent::Result<()> {
    let model_runtime = ModelRuntime::create(ModelRuntimeCreateOptions::default()).await?;
    let default_auth_session = create_agent_session(Some(CreateAgentSessionOptions {
        session_manager: Some(SessionManager::in_memory(None, None, None)?),
        model_runtime: Some(model_runtime.clone()),
        ..Default::default()
    }))
    .await?;
    pi_js::console::log("Session with default model runtime");
    default_auth_session.session.dispose();

    let custom_runtime = ModelRuntime::create(ModelRuntimeCreateOptions {
        auth_path: Some("/tmp/my-app/auth.json".to_string()),
        models_path: Some(Some("/tmp/my-app/models.json".to_string())),
        ..Default::default()
    })
    .await?;
    let custom_auth_session = create_agent_session(Some(CreateAgentSessionOptions {
        session_manager: Some(SessionManager::in_memory(None, None, None)?),
        model_runtime: Some(custom_runtime),
        ..Default::default()
    }))
    .await?;
    pi_js::console::log("Session with custom auth and models locations");
    custom_auth_session.session.dispose();

    model_runtime.set_runtime_api_key("anthropic", "sk-my-temp-key");
    let runtime_key_session = create_agent_session(Some(CreateAgentSessionOptions {
        session_manager: Some(SessionManager::in_memory(None, None, None)?),
        model_runtime: Some(model_runtime),
        ..Default::default()
    }))
    .await?;
    pi_js::console::log("Session with runtime API key override");
    runtime_key_session.session.dispose();

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
