//! Port of packages/coding-agent/test/suite/regressions/3616-settings-inmemory-reload.test.ts

#[path = "support/mod.rs"]
mod support;

use pi_ai::types::ModelThinkingLevel;
use pi_coding_agent::core::resource_loader::{DefaultResourceLoader, DefaultResourceLoaderOptions, ResourceLoader};
use pi_coding_agent::core::settings_manager::{CompactionSettings, ImageSettings, Settings, SettingsManager};
use pretty_assertions::assert_eq;

#[tokio::test]
async fn preserves_initial_settings_after_direct_reload() {
    let settings = Settings {
        default_thinking_level: Some(ModelThinkingLevel::High),
        images: Some(ImageSettings {
            auto_resize: Some(false),
            ..Default::default()
        }),
        compaction: Some(CompactionSettings {
            enabled: Some(false),
            ..Default::default()
        }),
        ..Default::default()
    };
    let settings_manager = SettingsManager::in_memory(Some(settings.clone()), None);

    settings_manager.reload().await;

    assert_eq!(
        settings_manager.get_default_thinking_level(),
        Some(ModelThinkingLevel::High)
    );
    assert!(!settings_manager.get_image_auto_resize());
    assert!(!settings_manager.get_compaction_enabled());
    assert_eq!(settings_manager.get_global_settings(), settings);
}

#[tokio::test]
async fn preserves_initial_settings_when_default_resource_loader_reloads() {
    let temp_dir = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-3616-"])).unwrap();
    let agent_dir = pi_js::path::join(&[&temp_dir, "agent"]);
    pi_js::fs::mkdir(&agent_dir, true, None).unwrap();

    let settings_manager = SettingsManager::in_memory(
        Some(Settings {
            default_thinking_level: Some(ModelThinkingLevel::High),
            images: Some(ImageSettings {
                auto_resize: Some(false),
                ..Default::default()
            }),
            compaction: Some(CompactionSettings {
                enabled: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        }),
        None,
    );
    let resource_loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: temp_dir.clone(),
        agent_dir,
        settings_manager: Some(settings_manager.clone()),
        no_extensions: true,
        no_skills: true,
        no_prompt_templates: true,
        no_themes: true,
        no_context_files: true,
        ..Default::default()
    });

    resource_loader.reload(None).await.unwrap();

    assert_eq!(
        settings_manager.get_default_thinking_level(),
        Some(ModelThinkingLevel::High)
    );
    assert!(!settings_manager.get_image_auto_resize());
    assert!(!settings_manager.get_compaction_enabled());

    let _ = pi_js::fs::rm(&temp_dir, true, true);
}

#[tokio::test]
async fn preserves_initial_settings_after_an_unrelated_setter_flush_and_reload() {
    let settings_manager = SettingsManager::in_memory(
        Some(Settings {
            images: Some(ImageSettings {
                auto_resize: Some(false),
                ..Default::default()
            }),
            compaction: Some(CompactionSettings {
                enabled: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        }),
        None,
    );

    settings_manager.set_theme("dark");
    settings_manager.flush().await;
    settings_manager.reload().await;

    assert_eq!(settings_manager.get_theme().as_deref(), Some("dark"));
    assert!(!settings_manager.get_image_auto_resize());
    assert!(!settings_manager.get_compaction_enabled());
    assert_eq!(
        settings_manager.get_global_settings(),
        Settings {
            images: Some(ImageSettings {
                auto_resize: Some(false),
                ..Default::default()
            }),
            compaction: Some(CompactionSettings {
                enabled: Some(false),
                ..Default::default()
            }),
            theme: Some("dark".to_string()),
            ..Default::default()
        }
    );
}
