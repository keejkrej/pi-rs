//! Port of packages/coding-agent/test/default-tools-setting.test.ts
//!
//! // WIRE(@session-w3): connect `create_agent_session` and `create_agent_session_services`
//! // once `core/sdk.rs` and `core/agent_session_services.rs` land.

use std::fs;
use std::sync::Arc;

use pi_coding_agent::core::resource_loader::{DefaultResourceLoader, DefaultResourceLoaderOptions};
use pi_coding_agent::core::settings_manager::{InMemorySettingsStorage, Settings, SettingsManager, SettingsScope};
use serde_json::json;
use tempfile::TempDir;

#[test]
fn resolves_explicit_default_tools_and_modifiers() {
    let settings: Settings = serde_json::from_value(json!({ "defaultTools": ["grep", "find"] })).expect("settings");
    let mgr = SettingsManager::in_memory(Some(settings), None);
    assert_eq!(
        mgr.get_default_tools(),
        Some(vec!["grep".to_owned(), "find".to_owned()])
    );

    let ps_settings: Settings = serde_json::from_value(json!({
        "defaultTools": ["read", "powershell", "edit", "write"]
    }))
    .expect("settings");
    let ps_mgr = SettingsManager::in_memory(Some(ps_settings), None);
    assert_eq!(
        ps_mgr.get_default_tools(),
        Some(vec![
            "read".to_owned(),
            "powershell".to_owned(),
            "edit".to_owned(),
            "write".to_owned()
        ])
    );

    let mod_settings: Settings = serde_json::from_value(json!({
        "defaultTools": ["+inactive_tool", "-write"]
    }))
    .expect("settings");
    let mod_mgr = SettingsManager::in_memory(Some(mod_settings), None);
    let resolved = mod_mgr.get_default_tools().expect("resolved");
    assert_eq!(
        resolved,
        vec![
            "read".to_owned(),
            "bash".to_owned(),
            "edit".to_owned(),
            "inactive_tool".to_owned()
        ]
    );
}

#[test]
fn merges_project_default_tools_modifiers_over_global_and_replaces_with_plain_list() {
    let storage = InMemorySettingsStorage::new();
    storage
        .with_lock(SettingsScope::Global, |_| {
            Ok(Some(pi_js::json::stringify(&json!({
                "defaultTools": ["read", "grep", "write"]
            }))))
        })
        .unwrap();
    storage
        .with_lock(SettingsScope::Project, |_| {
            Ok(Some(pi_js::json::stringify(&json!({
                "defaultTools": ["+inactive_tool", "-write"]
            }))))
        })
        .unwrap();
    let mgr = SettingsManager::from_storage(Arc::new(storage), None);
    assert_eq!(
        mgr.get_default_tools(),
        Some(vec!["read".to_owned(), "grep".to_owned(), "inactive_tool".to_owned()])
    );

    let replace_storage = InMemorySettingsStorage::new();
    replace_storage
        .with_lock(SettingsScope::Global, |_| {
            Ok(Some(pi_js::json::stringify(&json!({
                "defaultTools": ["read", "bash"]
            }))))
        })
        .unwrap();
    replace_storage
        .with_lock(SettingsScope::Project, |_| {
            Ok(Some(pi_js::json::stringify(&json!({
                "defaultTools": ["ls"]
            }))))
        })
        .unwrap();
    let mgr2 = SettingsManager::from_storage(Arc::new(replace_storage), None);
    assert_eq!(mgr2.get_default_tools(), Some(vec!["ls".to_owned()]));
}

#[tokio::test]
async fn reloads_updated_default_tools_from_disk_settings() {
    let temp_dir: TempDir = tempfile::tempdir().expect("create tempdir");
    let agent_dir = temp_dir.path().join("agent");
    fs::create_dir_all(&agent_dir).unwrap();
    let settings_path = agent_dir.join("settings.json");

    let settings_manager = SettingsManager::create(
        temp_dir.path().to_str().unwrap(),
        Some(agent_dir.to_str().unwrap()),
        None,
    );
    let loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: temp_dir.path().to_string_lossy().into_owned(),
        agent_dir: agent_dir.to_string_lossy().into_owned(),
        settings_manager: Some(settings_manager.clone()),
        no_extensions: true,
        no_skills: true,
        no_prompt_templates: true,
        no_themes: true,
        no_context_files: true,
        ..Default::default()
    });
    loader.reload(None).await.expect("initial reload");
    assert_eq!(settings_manager.get_default_tools(), None);

    fs::write(
        &settings_path,
        pi_js::json::stringify(&json!({ "defaultTools": ["+inactive_tool", "+grep"] })),
    )
    .unwrap();
    loader.reload(None).await.expect("reload after write");
    assert_eq!(
        settings_manager.get_default_tools(),
        Some(vec![
            "read".to_owned(),
            "bash".to_owned(),
            "edit".to_owned(),
            "write".to_owned(),
            "inactive_tool".to_owned(),
            "grep".to_owned(),
        ])
    );

    fs::write(
        &settings_path,
        pi_js::json::stringify(&json!({ "defaultTools": ["-read"] })),
    )
    .unwrap();
    loader.reload(None).await.expect("reload second edit");
    assert_eq!(
        settings_manager.get_default_tools(),
        Some(vec!["bash".to_owned(), "edit".to_owned(), "write".to_owned(),])
    );
}
