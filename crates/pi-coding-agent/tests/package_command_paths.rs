//! Port of packages/coding-agent/test/package-command-paths.test.ts (unit coding-agent-interactive-components-config-selector)

// @generated-mods begin (scaffold-owned, do not edit)
#[path = "package_command_paths/part_2.rs"]
mod part_2;
// @generated-mods end

use std::sync::Arc;

use pi_coding_agent::core::package_manager::{
    DefaultPackageManager, PackageManagerOptions, PathMetadata, ResolvedPaths, ResolvedResource,
};
use pi_coding_agent::core::settings_manager::{
    InMemorySettingsStorage, PackageFilterSource, PackageSource, SettingsManager, SettingsManagerCreateOptions,
    SettingsScope,
};
use pi_coding_agent::core::source_info::{SourceOrigin, SourceScope};
use pi_coding_agent::modes::interactive::components::config_selector::{
    ConfigSelectorComponent, ConfigWriteScope, ScopedResolvedPaths,
};
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_js::env::testing::EnvGuard;
use pi_tui::{Component, KeybindingsManager, TUI_KEYBINDINGS, set_keybindings};
use serde_json::json;
use serial_test::serial;
use tempfile::TempDir;

struct ConfigSelectorFixture {
    _home_env: EnvGuard,
    _agent_env: EnvGuard,
    _temp_dir: TempDir,
    root: String,
    agent_dir: String,
    project_dir: String,
}

impl ConfigSelectorFixture {
    fn new() -> Self {
        init_theme(Some("dark"), None);
        set_keybindings(KeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
        let temp_dir = tempfile::tempdir().expect("create tempdir");
        let root = temp_dir.path().to_string_lossy().into_owned();
        let home_dir = pi_js::path::join(&[&root, "home"]);
        let agent_dir = pi_js::path::join(&[&root, "agent"]);
        let project_dir = pi_js::path::join(&[&root, "project"]);
        pi_js::fs::mkdir(&home_dir, true, None).expect("mkdir home");
        pi_js::fs::mkdir(&agent_dir, true, None).expect("mkdir agent");
        pi_js::fs::mkdir(&project_dir, true, None).expect("mkdir project");
        let home_env = EnvGuard::set("HOME", Some(&home_dir));
        let agent_env = EnvGuard::set("PI_CODING_AGENT_DIR", Some(&agent_dir));
        Self {
            _home_env: home_env,
            _agent_env: agent_env,
            _temp_dir: temp_dir,
            root,
            agent_dir,
            project_dir,
        }
    }
}

impl Drop for ConfigSelectorFixture {
    fn drop(&mut self) {
        init_theme(Some("dark"), None);
        set_keybindings(KeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
    }
}

fn extension_paths(package_root: &str, source: &str, scope: SourceScope, names: &[&str]) -> ResolvedPaths {
    ResolvedPaths {
        extensions: names
            .iter()
            .map(|name| ResolvedResource {
                path: pi_js::path::join(&[package_root, "extensions", name]),
                enabled: true,
                metadata: PathMetadata {
                    source: source.to_string(),
                    scope,
                    origin: SourceOrigin::Package,
                    base_dir: Some(package_root.to_string()),
                    package_root: None,
                    changelog_path: None,
                },
            })
            .collect(),
        skills: Vec::new(),
        prompts: Vec::new(),
        themes: Vec::new(),
        install_failures: None,
    }
}

#[test]
#[serial]
fn cycles_project_package_overrides_in_config_local_mode() {
    let fixture = ConfigSelectorFixture::new();
    let storage = Arc::new(InMemorySettingsStorage::new());
    storage
        .with_lock(SettingsScope::Global, |_| {
            Ok(Some(pi_js::json::stringify(&json!({ "packages": ["npm:pi-tools"] }))))
        })
        .expect("seed global settings");
    let settings_manager = Arc::new(SettingsManager::from_storage(
        storage,
        Some(SettingsManagerCreateOptions {
            project_trusted: Some(true),
        }),
    ));
    let resolved_paths = extension_paths(
        &pi_js::path::join(&[&fixture.root, "pkg"]),
        "npm:pi-tools",
        SourceScope::User,
        &["bar.ts"],
    );
    let selector = ConfigSelectorComponent::new(
        &ScopedResolvedPaths {
            global: resolved_paths.clone(),
            project: resolved_paths,
        },
        Arc::clone(&settings_manager) as _,
        &fixture.project_dir,
        &fixture.agent_dir,
        Arc::new(|| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        Some(24),
        ConfigWriteScope::Project,
        true,
    );

    selector.get_resource_list().handle_input(" ");
    assert_eq!(
        settings_manager.get_project_settings().packages,
        Some(vec![PackageSource::Object(PackageFilterSource {
            source: "npm:pi-tools".to_string(),
            autoload: Some(false),
            extensions: Some(vec!["-extensions/bar.ts".to_string()]),
            skills: None,
            prompts: None,
            themes: None,
        })])
    );

    selector.get_resource_list().handle_input(" ");
    assert_eq!(
        settings_manager.get_project_settings().packages,
        Some(vec![PackageSource::Object(PackageFilterSource {
            source: "npm:pi-tools".to_string(),
            autoload: Some(false),
            extensions: Some(vec!["+extensions/bar.ts".to_string()]),
            skills: None,
            prompts: None,
            themes: None,
        })])
    );

    selector.get_resource_list().handle_input(" ");
    assert_eq!(settings_manager.get_project_settings().packages, Some(Vec::new()));
}

#[tokio::test]
#[serial]
async fn toggles_built_in_extensions_in_config_global_mode() {
    let fixture = ConfigSelectorFixture::new();
    let settings_manager = Arc::new(SettingsManager::from_storage(
        Arc::new(InMemorySettingsStorage::new()),
        Some(SettingsManagerCreateOptions {
            project_trusted: Some(true),
        }),
    ));
    let resolved_paths = DefaultPackageManager::new(PackageManagerOptions {
        cwd: fixture.project_dir.clone(),
        agent_dir: fixture.agent_dir.clone(),
        settings_manager: (*settings_manager).clone(),
        builtin_extensions: vec!["llama.cpp".to_string(), "mcp".to_string()],
    })
    .resolve(None)
    .await
    .expect("resolve");
    let selector = ConfigSelectorComponent::new(
        &ScopedResolvedPaths {
            global: resolved_paths.clone(),
            project: resolved_paths,
        },
        Arc::clone(&settings_manager) as _,
        &fixture.project_dir,
        &fixture.agent_dir,
        Arc::new(|| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        Some(24),
        ConfigWriteScope::Global,
        true,
    );

    let list = selector.get_resource_list();
    init_theme(Some("dark"), None);
    let rendered = list.render(80).join("\n");
    assert!(rendered.contains("Built-in"), "rendered: {rendered}");
    assert!(rendered.contains("llama.cpp"), "rendered: {rendered}");
    list.handle_input(" ");
    assert_eq!(
        settings_manager.get_global_settings().extensions,
        Some(vec!["-builtin:llama.cpp".to_string()])
    );
    list.handle_input(" ");
    assert_eq!(
        settings_manager.get_global_settings().extensions,
        Some(vec!["+builtin:llama.cpp".to_string()])
    );
}

#[tokio::test]
#[serial]
async fn cycles_project_built_in_extension_overrides_in_config_local_mode() {
    let fixture = ConfigSelectorFixture::new();
    let storage = Arc::new(InMemorySettingsStorage::new());
    storage
        .with_lock(SettingsScope::Global, |_| {
            Ok(Some(pi_js::json::stringify(&json!({ "extensions": ["-builtin:mcp"] }))))
        })
        .expect("seed global settings");
    let settings_manager = Arc::new(SettingsManager::from_storage(
        storage,
        Some(SettingsManagerCreateOptions {
            project_trusted: Some(true),
        }),
    ));
    let resolved_paths = DefaultPackageManager::new(PackageManagerOptions {
        cwd: fixture.project_dir.clone(),
        agent_dir: fixture.agent_dir.clone(),
        settings_manager: (*settings_manager).clone(),
        builtin_extensions: vec!["mcp".to_string()],
    })
    .resolve(None)
    .await
    .expect("resolve");
    let selector = ConfigSelectorComponent::new(
        &ScopedResolvedPaths {
            global: resolved_paths.clone(),
            project: resolved_paths,
        },
        Arc::clone(&settings_manager) as _,
        &fixture.project_dir,
        &fixture.agent_dir,
        Arc::new(|| {}),
        Arc::new(|| {}),
        Arc::new(|| {}),
        Some(24),
        ConfigWriteScope::Project,
        true,
    );

    let list = selector.get_resource_list();
    list.handle_input(" ");
    assert_eq!(
        settings_manager.get_project_settings().extensions,
        Some(vec!["+builtin:mcp".to_string()])
    );
    list.handle_input(" ");
    assert_eq!(
        settings_manager.get_project_settings().extensions,
        Some(vec!["-builtin:mcp".to_string()])
    );
    list.handle_input(" ");
    assert_eq!(settings_manager.get_project_settings().extensions, Some(Vec::new()));
}
