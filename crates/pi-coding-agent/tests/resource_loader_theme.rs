//! Port of packages/coding-agent/test/resource-loader-theme.test.ts

use std::fs;
use std::path::PathBuf;

use pi_coding_agent::core::resource_loader::{DefaultResourceLoader, DefaultResourceLoaderOptions};
use pi_coding_agent::core::settings_manager::{Settings, SettingsManager};
use pi_js::env::testing::EnvGuard;
use pi_tui::terminal_image::TerminalCapabilityOverrides;
use pi_tui::{reset_capabilities_cache, set_capability_overrides};
use serde_json::{Value, json};
use serial_test::serial;
use tempfile::TempDir;

struct ThemeFixture {
    _temp_dir: TempDir,
    agent_dir: String,
    cwd: String,
    theme_path: String,
}

impl ThemeFixture {
    fn new() -> Self {
        let temp_dir = tempfile::tempdir().expect("create tempdir");
        let agent_dir = temp_dir.path().join("agent");
        let cwd = temp_dir.path().join("project");
        fs::create_dir_all(&agent_dir).expect("create agent_dir");
        fs::create_dir_all(&cwd).expect("create cwd");

        let dark_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("modes")
            .join("interactive")
            .join("theme")
            .join("dark.json");
        let dark_raw = fs::read_to_string(&dark_path).expect("read dark.json");
        let mut theme_json: Value = pi_js::json::parse(&dark_raw).expect("parse dark.json");
        theme_json["name"] = json!("capability-test");
        theme_json["colors"]["userMessageBg"] = json!("#3c3544");

        let theme_path = temp_dir.path().join("capability-test.json");
        fs::write(&theme_path, pi_js::json::stringify(&theme_json)).expect("write theme");

        Self {
            _temp_dir: temp_dir,
            agent_dir: agent_dir.to_string_lossy().into_owned(),
            cwd: cwd.to_string_lossy().into_owned(),
            theme_path: theme_path.to_string_lossy().into_owned(),
        }
    }
}

struct CapabilityResetGuard;

impl Drop for CapabilityResetGuard {
    fn drop(&mut self) {
        set_capability_overrides(TerminalCapabilityOverrides::default());
        reset_capabilities_cache();
    }
}

#[tokio::test]
#[serial]
async fn uses_true_color_true_setting_over_256_color_environment() {
    let fx = ThemeFixture::new();
    let _cap_guard = CapabilityResetGuard;
    let _env_guard = EnvGuard::set("PI_TRUE_COLOR", Some("0"));
    set_capability_overrides(TerminalCapabilityOverrides::default());
    reset_capabilities_cache();

    let settings: Settings = serde_json::from_value(json!({ "terminal": { "trueColor": true } })).expect("settings");
    let loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: fx.cwd,
        agent_dir: fx.agent_dir,
        settings_manager: Some(SettingsManager::in_memory(Some(settings), None)),
        additional_theme_paths: vec![fx.theme_path],
        no_extensions: true,
        no_skills: true,
        no_prompt_templates: true,
        no_context_files: true,
        ..Default::default()
    });
    loader.reload(None).await.expect("reload");

    let themes = loader.get_themes();
    let loaded_theme = themes
        .themes
        .iter()
        .find(|t| t.name.as_deref() == Some("capability-test"))
        .expect("capability-test theme loaded");
    assert_eq!(loaded_theme.bg("userMessageBg", "x"), "\x1b[48;2;60;53;68mx\x1b[49m");
}

#[tokio::test]
#[serial]
async fn uses_true_color_false_setting_over_truecolor_environment() {
    let fx = ThemeFixture::new();
    let _cap_guard = CapabilityResetGuard;
    let _env_guard = EnvGuard::set("PI_TRUE_COLOR", Some("1"));
    set_capability_overrides(TerminalCapabilityOverrides::default());
    reset_capabilities_cache();

    let settings: Settings = serde_json::from_value(json!({ "terminal": { "trueColor": false } })).expect("settings");
    let loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: fx.cwd,
        agent_dir: fx.agent_dir,
        settings_manager: Some(SettingsManager::in_memory(Some(settings), None)),
        additional_theme_paths: vec![fx.theme_path],
        no_extensions: true,
        no_skills: true,
        no_prompt_templates: true,
        no_context_files: true,
        ..Default::default()
    });
    loader.reload(None).await.expect("reload");

    let themes = loader.get_themes();
    let loaded_theme = themes
        .themes
        .iter()
        .find(|t| t.name.as_deref() == Some("capability-test"))
        .expect("capability-test theme loaded");
    assert_eq!(loaded_theme.bg("userMessageBg", "x"), "\x1b[48;5;59mx\x1b[49m");
}

#[tokio::test]
#[serial]
async fn returns_to_automatic_detection_after_explicit_setting_is_removed() {
    let fx = ThemeFixture::new();
    let _cap_guard = CapabilityResetGuard;
    let _env_guard = EnvGuard::set("PI_TRUE_COLOR", Some("1"));
    set_capability_overrides(TerminalCapabilityOverrides::default());
    reset_capabilities_cache();

    let settings_path = PathBuf::from(&fx.agent_dir).join("settings.json");
    fs::write(
        &settings_path,
        pi_js::json::stringify(&json!({ "terminal": { "trueColor": false } })),
    )
    .expect("write settings");
    let settings_manager = SettingsManager::create(&fx.cwd, Some(&fx.agent_dir), None);
    let loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: fx.cwd,
        agent_dir: fx.agent_dir,
        settings_manager: Some(settings_manager.clone()),
        additional_theme_paths: vec![fx.theme_path],
        no_extensions: true,
        no_skills: true,
        no_prompt_templates: true,
        no_context_files: true,
        ..Default::default()
    });
    loader.reload(None).await.expect("initial reload");
    set_capability_overrides(settings_manager.get_terminal_capability_overrides());

    fs::write(&settings_path, "{}").expect("clear settings");
    loader.reload(None).await.expect("second reload");

    let themes = loader.get_themes();
    let loaded_theme = themes
        .themes
        .iter()
        .find(|t| t.name.as_deref() == Some("capability-test"))
        .expect("capability-test theme loaded");
    assert_eq!(loaded_theme.bg("userMessageBg", "x"), "\x1b[48;2;60;53;68mx\x1b[49m");
}
