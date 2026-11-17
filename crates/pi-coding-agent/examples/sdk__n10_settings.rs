//! Port of packages/coding-agent/examples/sdk/10-settings.ts

use std::process::ExitCode;

use pi_ai::ModelThinkingLevel;
use pi_coding_agent::core::sdk::{CreateAgentSessionOptions, create_agent_session};
use pi_coding_agent::core::session_manager::SessionManager;
use pi_coding_agent::core::settings_manager::{CompactionSettings, RetrySettings, Settings, SettingsManager};

async fn run() -> pi_coding_agent::Result<()> {
    let cwd = pi_js::env::cwd();

    let settings_from_disk = SettingsManager::create(&cwd, None, None);
    let global_json = pi_js::json::stringify_pretty(&settings_from_disk.get_global_settings(), 2)?;
    pi_js::console::log(&format!("Current settings: {global_json}"));

    let settings_manager = SettingsManager::create(&cwd, None, None);
    settings_manager.apply_overrides(&Settings {
        compaction: Some(CompactionSettings {
            enabled: Some(false),
            ..Default::default()
        }),
        retry: Some(RetrySettings {
            enabled: Some(true),
            max_retries: Some(5),
            base_delay_ms: Some(1000),
            ..Default::default()
        }),
        ..Default::default()
    });

    let custom_session = create_agent_session(Some(CreateAgentSessionOptions {
        settings_manager: Some(settings_manager.clone()),
        session_manager: Some(SessionManager::in_memory(None, None, None)?),
        ..Default::default()
    }))
    .await?;
    pi_js::console::log("Session created with custom settings");
    custom_session.session.dispose();

    settings_manager.set_default_thinking_level(ModelThinkingLevel::Low);
    settings_manager.flush().await;

    for err in settings_manager.drain_errors() {
        pi_js::console::warn(&format!("Warning ({:?} settings): {}", err.scope, err.error));
    }

    let in_memory_settings = SettingsManager::in_memory(
        Some(Settings {
            compaction: Some(CompactionSettings {
                enabled: Some(false),
                ..Default::default()
            }),
            retry: Some(RetrySettings {
                enabled: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        }),
        None,
    );

    let test_session = create_agent_session(Some(CreateAgentSessionOptions {
        settings_manager: Some(in_memory_settings),
        session_manager: Some(SessionManager::in_memory(None, None, None)?),
        ..Default::default()
    }))
    .await?;
    pi_js::console::log("Test session created with in-memory settings");
    test_session.session.dispose();

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
