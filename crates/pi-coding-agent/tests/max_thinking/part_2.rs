//! Port of packages/coding-agent/test/max-thinking.test.ts (unit coding-agent-cli-args-auth-migrations)

use pi_ai::types::ModelThinkingLevel;
use pi_coding_agent::cli::args::is_valid_thinking_level;
use pi_coding_agent::core::settings_manager::SettingsManager;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn max_thinking_level_is_accepted_by_cli_and_settings() {
    assert!(is_valid_thinking_level("max"));

    let settings = SettingsManager::in_memory(None, None);
    settings.set_default_thinking_level(ModelThinkingLevel::Max);
    settings.flush().await;
    assert_eq!(settings.get_default_thinking_level(), Some(ModelThinkingLevel::Max));
}
