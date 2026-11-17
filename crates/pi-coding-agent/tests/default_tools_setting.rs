//! Port of packages/coding-agent/test/default-tools-setting.test.ts

use std::fs;
use std::sync::Arc;

use pi_coding_agent::core::agent_session_runtime::{
    CreateAgentSessionFromServicesOptions, CreateAgentSessionServicesOptions, create_agent_session_from_services,
    create_agent_session_services,
};
use pi_coding_agent::core::auth_storage::AuthStorage;
use pi_coding_agent::core::model_runtime::{ModelRuntime, ModelRuntimeCreateOptions};
use pi_coding_agent::core::resource_loader::{DefaultResourceLoader, DefaultResourceLoaderOptions};
use pi_coding_agent::core::sdk::NoToolsMode;
use pi_coding_agent::core::session_manager::SessionManager;
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

#[tokio::test]
async fn create_agent_session_services_and_session_respect_default_tools_setting_and_no_tools() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let cwd = temp_dir.path().to_string_lossy().into_owned();
    let agent_dir = temp_dir.path().join("agent").to_string_lossy().into_owned();

    let settings: Settings = serde_json::from_value(json!({ "defaultTools": ["grep", "find"] })).expect("settings");
    let settings_manager = SettingsManager::in_memory(Some(settings), None);
    let model_runtime = ModelRuntime::create(ModelRuntimeCreateOptions {
        credentials: Some(Arc::new(AuthStorage::in_memory(None))),
        allow_model_network: Some(false),
        ..Default::default()
    })
    .await
    .unwrap();

    let services = create_agent_session_services(CreateAgentSessionServicesOptions {
        cwd: cwd.clone(),
        agent_dir: Some(agent_dir),
        settings_manager: Some(settings_manager),
        model_runtime: Some(model_runtime.clone()),
        resource_loader_options: Some(DefaultResourceLoaderOptions {
            cwd: cwd.clone(),
            no_extensions: true,
            no_skills: true,
            no_prompt_templates: true,
            no_themes: true,
            no_context_files: true,
            ..Default::default()
        }),
        ..Default::default()
    })
    .await
    .unwrap();

    let created = create_agent_session_from_services(CreateAgentSessionFromServicesOptions {
        services: services.clone(),
        session_manager: SessionManager::in_memory(Some(&cwd), None, None).unwrap(),
        session_start_event: None,
        model: None,
        thinking_level: None,
        scoped_models: None,
        tools: None,
        exclude_tools: None,
        no_tools: None,
        custom_tools: None,
    })
    .await
    .unwrap();
    let mut active = created.session.get_active_tool_names();
    active.sort();
    assert_eq!(active, vec!["find", "grep"]);
    let prompt = created.session.system_prompt();
    assert!(prompt.contains("- grep:"));
    assert!(!prompt.contains("- read:"));
    created.session.dispose();

    let empty = create_agent_session_from_services(CreateAgentSessionFromServicesOptions {
        services,
        session_manager: SessionManager::in_memory(Some(&cwd), None, None).unwrap(),
        session_start_event: None,
        model: None,
        thinking_level: None,
        scoped_models: None,
        tools: None,
        exclude_tools: None,
        no_tools: Some(NoToolsMode::All),
        custom_tools: None,
    })
    .await
    .unwrap();
    assert!(empty.session.get_all_tools().is_empty());
    assert!(empty.session.get_active_tool_names().is_empty());
    empty.session.dispose();
}
