//! Port of packages/coding-agent/test/suite/regressions/2753-reload-stale-resource-settings.test.ts (unit coding-agent-ext-loader-resource-loader)
//!
//! // WIRE(@session-w3): add end-to-end `createAgentSessionRuntime` + `session.reload()` assertion
//! // once `core/agent_session_runtime.rs` and `core/agent_session.rs` land.

use std::fs;

use pi_coding_agent::core::resource_loader::{DefaultResourceLoader, DefaultResourceLoaderOptions};
use pi_coding_agent::core::settings_manager::SettingsManager;
use serde_json::json;
use tempfile::TempDir;

#[tokio::test]
async fn applies_updated_top_level_prompt_settings_on_reload_after_startup() {
    let temp_dir: TempDir = tempfile::tempdir().expect("create tempdir");
    let agent_dir = temp_dir.path().join("agent");
    let prompts_dir = agent_dir.join("prompts");
    fs::create_dir_all(&prompts_dir).expect("mkdir prompts");
    fs::write(prompts_dir.join("test.md"), "Echo test prompt\n").expect("write test.md");

    let settings_manager = SettingsManager::create(
        temp_dir.path().to_str().unwrap(),
        Some(agent_dir.to_str().unwrap()),
        None,
    );
    let loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: temp_dir.path().to_string_lossy().into_owned(),
        agent_dir: agent_dir.to_string_lossy().into_owned(),
        settings_manager: Some(settings_manager.clone()),
        no_skills: true,
        no_themes: true,
        ..Default::default()
    });
    loader.reload(None).await.expect("initial reload");

    let initial_names: Vec<String> = loader.get_prompts().prompts.into_iter().map(|p| p.name).collect();
    assert!(initial_names.contains(&"test".to_owned()));

    let settings_json = pi_js::json::try_stringify_pretty(&json!({ "prompts": ["-prompts/test.md"] }), "  ")
        .expect("stringify settings");
    fs::write(agent_dir.join("settings.json"), format!("{settings_json}\n")).expect("write updated settings.json");

    loader.reload(None).await.expect("reload after settings change");

    assert_eq!(
        loader.settings_manager().get_global_settings().prompts,
        Some(vec!["-prompts/test.md".to_owned()])
    );
    let reloaded_names: Vec<String> = loader.get_prompts().prompts.into_iter().map(|p| p.name).collect();
    assert!(!reloaded_names.contains(&"test".to_owned()));
}
