//! Port of packages/coding-agent/test/settings-manager.test.ts

use pi_ai::ModelThinkingLevel;
use pi_coding_agent::core::http_dispatcher::DEFAULT_HTTP_IDLE_TIMEOUT_MS;
use pi_coding_agent::core::settings_manager::{
    CacheWarmingMode, DefaultProjectTrust, FullscreenExitOutput, MermaidRenderingMode, PackageFilterSource,
    PackageSource, ResolvedRetrySettings, RetrySettings, Settings, SettingsManager, SettingsManagerCreateOptions,
    SettingsScope,
};
use pi_js::env::testing::{EnvGuard, PlatformGuard};
use pi_tui::terminal_image::{ImageProtocol, TerminalCapabilityOverrides};
use pi_tui::{ScrollViewScrollbar, TuiMode, WheelScrollLines};
use serde_json::{Value, json};
use tempfile::TempDir;

fn setup_dirs() -> (TempDir, String, String) {
    let temp = TempDir::new().expect("temp dir");
    let agent_dir = temp.path().join("agent");
    let project_dir = temp.path().join("project");
    std::fs::create_dir_all(&agent_dir).expect("mkdir agent_dir");
    std::fs::create_dir_all(project_dir.join(".pi")).expect("mkdir .pi");
    (
        temp,
        agent_dir.to_str().expect("utf8").to_owned(),
        project_dir.to_str().expect("utf8").to_owned(),
    )
}

fn is_uuid_v4_like(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && [8, 4, 4, 4, 12]
            .into_iter()
            .zip(&parts)
            .all(|(len, part)| part.len() == len && part.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

mod preserves_externally_added_settings {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn should_preserve_enabled_models_when_changing_thinking_level() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        let settings_path = format!("{agent_dir}/settings.json");
        std::fs::write(
            &settings_path,
            pi_js::json::stringify(&json!({ "theme": "dark", "defaultModel": "claude-sonnet" })),
        )
        .unwrap();

        let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);

        let mut current: Value = pi_js::json::parse(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        current["enabledModels"] = json!(["claude-opus-4-5", "gpt-5.2-codex"]);
        std::fs::write(&settings_path, pi_js::json::stringify_pretty(&current, "  ")).unwrap();

        manager.set_default_thinking_level(ModelThinkingLevel::High);
        manager.flush().await;

        let saved: Value = pi_js::json::parse(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        assert_eq!(saved["enabledModels"], json!(["claude-opus-4-5", "gpt-5.2-codex"]));
        assert_eq!(saved["defaultThinkingLevel"], json!("high"));
        assert_eq!(saved["theme"], json!("dark"));
        assert_eq!(saved["defaultModel"], json!("claude-sonnet"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn should_preserve_custom_settings_when_changing_theme() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        let settings_path = format!("{agent_dir}/settings.json");
        std::fs::write(
            &settings_path,
            pi_js::json::stringify(&json!({ "defaultModel": "claude-sonnet" })),
        )
        .unwrap();

        let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);

        let mut current: Value = pi_js::json::parse(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        current["shellPath"] = json!("/bin/zsh");
        current["extensions"] = json!(["/path/to/extension.ts"]);
        std::fs::write(&settings_path, pi_js::json::stringify_pretty(&current, "  ")).unwrap();

        manager.set_theme("light");
        manager.flush().await;

        let saved: Value = pi_js::json::parse(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        assert_eq!(saved["shellPath"], json!("/bin/zsh"));
        assert_eq!(saved["extensions"], json!(["/path/to/extension.ts"]));
        assert_eq!(saved["theme"], json!("light"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn should_let_in_memory_changes_override_file_changes_for_same_key() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        let settings_path = format!("{agent_dir}/settings.json");
        std::fs::write(&settings_path, pi_js::json::stringify(&json!({ "theme": "dark" }))).unwrap();

        let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);

        let mut current: Value = pi_js::json::parse(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        current["defaultThinkingLevel"] = json!("low");
        std::fs::write(&settings_path, pi_js::json::stringify_pretty(&current, "  ")).unwrap();

        manager.set_default_thinking_level(ModelThinkingLevel::High);
        manager.flush().await;

        let saved: Value = pi_js::json::parse(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        assert_eq!(saved["defaultThinkingLevel"], json!("high"));
    }
}

mod device_id_tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn creates_one_global_device_id_and_reuses_it_in_later_processes() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        let settings_path = format!("{agent_dir}/settings.json");
        std::fs::write(&settings_path, pi_js::json::stringify(&json!({ "theme": "dark" }))).unwrap();
        std::fs::write(
            format!("{project_dir}/.pi/settings.json"),
            pi_js::json::stringify(&json!({ "deviceId": "project-device" })),
        )
        .unwrap();

        let first = SettingsManager::create(&project_dir, Some(&agent_dir), None);
        let device_id = first.get_or_create_device_id();
        first.flush().await;

        assert!(is_uuid_v4_like(&device_id), "unexpected device_id: {device_id}");
        assert_eq!(first.get_or_create_device_id(), device_id);
        assert_eq!(
            SettingsManager::create(&project_dir, Some(&agent_dir), None).get_or_create_device_id(),
            device_id
        );
        let saved: Value = pi_js::json::parse(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        assert_eq!(saved, json!({ "theme": "dark", "deviceId": device_id }));
    }
}

mod packages_migration {
    use super::*;

    #[test]
    fn should_keep_local_only_extensions_in_extensions_array() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        let settings_path = format!("{agent_dir}/settings.json");
        std::fs::write(
            &settings_path,
            pi_js::json::stringify(&json!({ "extensions": ["/local/ext.ts", "./relative/ext.ts"] })),
        )
        .unwrap();

        let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);
        assert_eq!(manager.get_packages(), Vec::<PackageSource>::new());
        assert_eq!(
            manager.get_extension_paths(),
            vec!["/local/ext.ts".to_owned(), "./relative/ext.ts".to_owned()]
        );
    }

    #[test]
    fn should_handle_packages_with_filtering_objects() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        let settings_path = format!("{agent_dir}/settings.json");
        std::fs::write(
            &settings_path,
            pi_js::json::stringify(&json!({
                "packages": [
                    "npm:simple-pkg",
                    {
                        "source": "npm:shitty-extensions",
                        "extensions": ["extensions/oracle.ts"],
                        "skills": []
                    }
                ]
            })),
        )
        .unwrap();

        let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);
        let packages = manager.get_packages();
        assert_eq!(packages.len(), 2);
        assert_eq!(packages[0], PackageSource::String("npm:simple-pkg".to_owned()));
        assert_eq!(
            packages[1],
            PackageSource::Object(PackageFilterSource {
                source: "npm:shitty-extensions".to_owned(),
                autoload: None,
                extensions: Some(vec!["extensions/oracle.ts".to_owned()]),
                skills: Some(Vec::new()),
                prompts: None,
                themes: None,
            })
        );
    }
}

mod reload_tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn should_reload_global_settings_from_disk() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        let settings_path = format!("{agent_dir}/settings.json");
        std::fs::write(
            &settings_path,
            pi_js::json::stringify(&json!({
                "theme": "dark",
                "extensions": ["/before.ts"]
            })),
        )
        .unwrap();

        let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);
        std::fs::write(
            &settings_path,
            pi_js::json::stringify(&json!({
                "theme": "light",
                "extensions": ["/after.ts"],
                "defaultModel": "claude-sonnet"
            })),
        )
        .unwrap();

        manager.reload().await;
        assert_eq!(manager.get_theme().as_deref(), Some("light"));
        assert_eq!(manager.get_extension_paths(), vec!["/after.ts".to_owned()]);
        assert_eq!(manager.get_default_model().as_deref(), Some("claude-sonnet"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn should_keep_previous_settings_and_report_the_file_path_when_the_file_is_invalid() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        let settings_path = format!("{agent_dir}/settings.json");
        std::fs::write(&settings_path, pi_js::json::stringify(&json!({ "theme": "dark" }))).unwrap();

        let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);
        std::fs::write(&settings_path, "{ invalid json").unwrap();
        manager.reload().await;

        assert_eq!(manager.get_theme().as_deref(), Some("dark"));
        let errors = manager.drain_errors();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].scope, SettingsScope::Global);
        assert_eq!(errors[0].path.as_deref(), Some(settings_path.as_str()));
    }
}

mod theme_setting {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn stores_slash_separated_automatic_theme_settings_separately_from_fixed_theme_names() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        let settings_path = format!("{agent_dir}/settings.json");
        std::fs::write(
            &settings_path,
            pi_js::json::stringify(&json!({ "theme": "light/dark" })),
        )
        .unwrap();

        let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);
        assert_eq!(manager.get_theme(), None);
        assert_eq!(manager.get_theme_setting().as_deref(), Some("light/dark"));

        manager.set_theme("solarized-light/tokyo-night");
        manager.flush().await;

        let saved: Value = pi_js::json::parse(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        assert_eq!(saved["theme"], json!("solarized-light/tokyo-night"));
    }
}

mod error_tracking {
    use super::*;

    #[test]
    fn should_collect_and_clear_load_errors_via_drain_errors() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        let global_path = format!("{agent_dir}/settings.json");
        let project_path = format!("{project_dir}/.pi/settings.json");
        std::fs::write(&global_path, "{ invalid global json").unwrap();
        std::fs::write(&project_path, "{ invalid project json").unwrap();

        let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);
        let errors = manager.drain_errors();
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].scope, SettingsScope::Global);
        assert_eq!(errors[0].path.as_deref(), Some(global_path.as_str()));
        assert_eq!(errors[1].scope, SettingsScope::Project);
        assert_eq!(errors[1].path.as_deref(), Some(project_path.as_str()));
        assert!(manager.drain_errors().is_empty());
    }
}

mod project_trust {
    use super::*;

    #[test]
    fn should_skip_project_settings_when_project_is_not_trusted() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        std::fs::write(
            format!("{agent_dir}/settings.json"),
            pi_js::json::stringify(&json!({ "theme": "global" })),
        )
        .unwrap();
        std::fs::write(
            format!("{project_dir}/.pi/settings.json"),
            pi_js::json::stringify(&json!({ "theme": "project" })),
        )
        .unwrap();

        let manager = SettingsManager::create(
            &project_dir,
            Some(&agent_dir),
            Some(SettingsManagerCreateOptions {
                project_trusted: Some(false),
            }),
        );
        assert!(!manager.is_project_trusted());
        assert_eq!(manager.get_theme().as_deref(), Some("global"));
        assert_eq!(manager.get_project_settings(), Settings::default());
    }

    #[test]
    fn should_reload_project_settings_after_trust_changes_to_true() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        std::fs::write(
            format!("{agent_dir}/settings.json"),
            pi_js::json::stringify(&json!({ "theme": "global" })),
        )
        .unwrap();
        std::fs::write(
            format!("{project_dir}/.pi/settings.json"),
            pi_js::json::stringify(&json!({ "theme": "project" })),
        )
        .unwrap();

        let manager = SettingsManager::create(
            &project_dir,
            Some(&agent_dir),
            Some(SettingsManagerCreateOptions {
                project_trusted: Some(false),
            }),
        );
        manager.set_project_trusted(true);
        assert!(manager.is_project_trusted());
        assert_eq!(manager.get_theme().as_deref(), Some("project"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn should_fail_project_settings_writes_when_project_is_not_trusted() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        let project_settings_path = format!("{project_dir}/.pi/settings.json");
        std::fs::write(
            &project_settings_path,
            pi_js::json::stringify(&json!({ "packages": ["npm:existing"] })),
        )
        .unwrap();

        let manager = SettingsManager::create(
            &project_dir,
            Some(&agent_dir),
            Some(SettingsManagerCreateOptions {
                project_trusted: Some(false),
            }),
        );
        let err = manager
            .set_project_packages(vec![PackageSource::String("npm:new".to_owned())])
            .expect_err("untrusted write should error");
        assert!(
            err.to_string()
                .contains("Project is not trusted; refusing to write project settings")
        );
        manager.flush().await;

        assert_eq!(manager.get_project_settings(), Settings::default());
        let saved: Value = pi_js::json::parse(&std::fs::read_to_string(&project_settings_path).unwrap()).unwrap();
        assert_eq!(saved, json!({ "packages": ["npm:existing"] }));
    }

    #[test]
    fn should_read_default_project_trust_from_global_settings_only() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        std::fs::write(
            format!("{agent_dir}/settings.json"),
            pi_js::json::stringify(&json!({ "defaultProjectTrust": "always" })),
        )
        .unwrap();
        std::fs::write(
            format!("{project_dir}/.pi/settings.json"),
            pi_js::json::stringify(&json!({ "defaultProjectTrust": "never" })),
        )
        .unwrap();

        let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);
        assert_eq!(manager.get_default_project_trust(), DefaultProjectTrust::Always);
    }

    #[test]
    fn should_default_invalid_project_trust_settings_to_ask() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        std::fs::write(
            format!("{agent_dir}/settings.json"),
            pi_js::json::stringify(&json!({ "defaultProjectTrust": "sometimes" })),
        )
        .unwrap();

        let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);
        assert_eq!(manager.get_default_project_trust(), DefaultProjectTrust::Ask);
    }
}

mod project_settings_directory_creation {
    use super::*;

    #[test]
    fn should_not_create_pi_folder_when_only_reading_project_settings() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        std::fs::write(
            format!("{agent_dir}/settings.json"),
            pi_js::json::stringify(&json!({ "theme": "dark" })),
        )
        .unwrap();
        std::fs::remove_dir_all(format!("{project_dir}/.pi")).unwrap();

        let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);
        assert!(!std::path::Path::new(&format!("{project_dir}/.pi")).exists());
        assert_eq!(manager.get_theme().as_deref(), Some("dark"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn should_create_pi_folder_when_writing_project_settings() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        std::fs::write(
            format!("{agent_dir}/settings.json"),
            pi_js::json::stringify(&json!({ "theme": "dark" })),
        )
        .unwrap();
        std::fs::remove_dir_all(format!("{project_dir}/.pi")).unwrap();

        let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);
        assert!(!std::path::Path::new(&format!("{project_dir}/.pi")).exists());

        manager
            .set_project_packages(vec![PackageSource::Object(PackageFilterSource {
                source: "npm:test-pkg".to_owned(),
                autoload: None,
                extensions: None,
                skills: None,
                prompts: None,
                themes: None,
            })])
            .unwrap();
        manager.flush().await;

        assert!(std::path::Path::new(&format!("{project_dir}/.pi")).exists());
        assert!(std::path::Path::new(&format!("{project_dir}/.pi/settings.json")).exists());
    }
}

mod terminal_capability_overrides {
    use super::*;

    fn overrides_from_json(terminal_json: Value) -> TerminalCapabilityOverrides {
        let settings: Settings = serde_json::from_value(json!({ "terminal": terminal_json })).unwrap();
        SettingsManager::in_memory(Some(settings), None).get_terminal_capability_overrides()
    }

    #[test]
    fn maps_explicit_values_and_omits_auto_values() {
        assert_eq!(
            overrides_from_json(json!({ "images": false, "trueColor": false, "hyperlinks": false })),
            TerminalCapabilityOverrides {
                images: Some(None),
                true_color: Some(false),
                hyperlinks: Some(false),
            }
        );
        assert_eq!(
            overrides_from_json(json!({ "images": "kitty", "trueColor": true, "hyperlinks": true })),
            TerminalCapabilityOverrides {
                images: Some(Some(ImageProtocol::Kitty)),
                true_color: Some(true),
                hyperlinks: Some(true),
            }
        );
        assert_eq!(
            overrides_from_json(json!({ "images": "auto", "trueColor": "auto", "hyperlinks": "auto" })),
            TerminalCapabilityOverrides::default()
        );
    }
}

mod retry_settings {
    use super::*;

    #[test]
    fn defaults_and_overrides_agent_retry_delay_cap() {
        assert_eq!(
            SettingsManager::in_memory(None, None).get_retry_settings(),
            ResolvedRetrySettings {
                enabled: true,
                max_retries: 3,
                base_delay_ms: 2_000,
                max_agent_delay_ms: 60_000,
            }
        );
        assert_eq!(
            SettingsManager::in_memory(
                Some(Settings {
                    retry: Some(RetrySettings {
                        enabled: Some(true),
                        max_retries: Some(10),
                        base_delay_ms: Some(500),
                        max_agent_delay_ms: Some(5_000),
                        provider: None,
                    }),
                    ..Default::default()
                }),
                None
            )
            .get_retry_settings(),
            ResolvedRetrySettings {
                enabled: true,
                max_retries: 10,
                base_delay_ms: 500,
                max_agent_delay_ms: 5_000,
            }
        );
    }
}

mod http_idle_timeout_ms {
    use super::*;

    #[test]
    fn should_default_to_5_minutes() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);
        assert_eq!(
            manager.get_http_idle_timeout_ms().unwrap(),
            DEFAULT_HTTP_IDLE_TIMEOUT_MS
        );
    }

    #[test]
    fn should_use_merged_global_and_project_settings() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        std::fs::write(
            format!("{agent_dir}/settings.json"),
            pi_js::json::stringify(&json!({ "httpIdleTimeoutMs": 300_000 })),
        )
        .unwrap();
        std::fs::write(
            format!("{project_dir}/.pi/settings.json"),
            pi_js::json::stringify(&json!({ "httpIdleTimeoutMs": 0 })),
        )
        .unwrap();

        let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);
        assert_eq!(manager.get_http_idle_timeout_ms().unwrap(), 0);
    }

    #[test]
    fn should_reject_invalid_timeout_values() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        std::fs::write(
            format!("{agent_dir}/settings.json"),
            pi_js::json::stringify(&json!({ "httpIdleTimeoutMs": -1 })),
        )
        .unwrap();

        let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);
        let err = manager.get_http_idle_timeout_ms().expect_err("negative timeout");
        assert!(err.to_string().contains("Invalid httpIdleTimeoutMs setting"));
    }
}

mod cache_warming {
    use super::*;

    #[test]
    fn defaults_to_idle_and_ignores_project_settings() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        assert_eq!(
            SettingsManager::create(&project_dir, Some(&agent_dir), None).get_cache_warming_mode(),
            CacheWarmingMode::Idle
        );

        std::fs::write(
            format!("{project_dir}/.pi/settings.json"),
            pi_js::json::stringify(&json!({ "cacheWarming": "streaming" })),
        )
        .unwrap();
        assert_eq!(
            SettingsManager::create(&project_dir, Some(&agent_dir), None).get_cache_warming_mode(),
            CacheWarmingMode::Idle
        );

        std::fs::write(
            format!("{agent_dir}/settings.json"),
            pi_js::json::stringify(&json!({ "cacheWarming": "streaming" })),
        )
        .unwrap();
        assert_eq!(
            SettingsManager::create(&project_dir, Some(&agent_dir), None).get_cache_warming_mode(),
            CacheWarmingMode::Streaming
        );

        std::fs::write(
            format!("{agent_dir}/settings.json"),
            pi_js::json::stringify(&json!({ "cacheWarming": "bogus" })),
        )
        .unwrap();
        assert_eq!(
            SettingsManager::create(&project_dir, Some(&agent_dir), None).get_cache_warming_mode(),
            CacheWarmingMode::Idle
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn persists_the_mode_globally() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);
        manager.set_cache_warming_mode(CacheWarmingMode::Off);
        manager.flush().await;

        assert_eq!(
            SettingsManager::create(&project_dir, Some(&agent_dir), None).get_cache_warming_mode(),
            CacheWarmingMode::Off
        );
        let saved: Value =
            pi_js::json::parse(&std::fs::read_to_string(format!("{agent_dir}/settings.json")).unwrap()).unwrap();
        assert_eq!(saved, json!({ "cacheWarming": "off" }));
    }
}

mod external_editor {
    use super::*;

    #[test]
    fn should_resolve_editor_commands_by_precedence() {
        let _vis = EnvGuard::set("VISUAL", Some("vim"));
        let _ed = EnvGuard::set("EDITOR", Some("nano"));
        assert_eq!(
            SettingsManager::in_memory(
                Some(Settings {
                    external_editor: Some("code --wait".to_owned()),
                    ..Default::default()
                }),
                None
            )
            .get_external_editor_command(),
            "code --wait"
        );
        assert_eq!(
            SettingsManager::in_memory(None, None).get_external_editor_command(),
            "vim"
        );

        drop(_vis);
        let _vis_none = EnvGuard::set("VISUAL", None);
        let _ed_emacs = EnvGuard::set("EDITOR", Some("emacs"));
        assert_eq!(
            SettingsManager::in_memory(None, None).get_external_editor_command(),
            "emacs"
        );
    }

    #[test]
    fn should_fall_back_to_platform_defaults() {
        let _vis = EnvGuard::set("VISUAL", None);
        let _ed = EnvGuard::set("EDITOR", None);

        {
            let _plat = PlatformGuard::set("win32");
            assert_eq!(
                SettingsManager::in_memory(None, None).get_external_editor_command(),
                "notepad"
            );
        }
        {
            let _plat = PlatformGuard::set("darwin");
            assert_eq!(
                SettingsManager::in_memory(None, None).get_external_editor_command(),
                "nano"
            );
        }
        {
            let _plat = PlatformGuard::set("linux");
            assert_eq!(
                SettingsManager::in_memory(None, None).get_external_editor_command(),
                "nano"
            );
        }
    }
}

mod tui_mode_tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn defaults_to_fullscreen_and_persists_regular_mode() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);
        assert_eq!(manager.get_tui_mode(), TuiMode::Fullscreen);

        manager.set_tui_mode(TuiMode::Regular);
        manager.flush().await;

        assert_eq!(manager.get_tui_mode(), TuiMode::Regular);
        let saved: Value =
            pi_js::json::parse(&std::fs::read_to_string(format!("{agent_dir}/settings.json")).unwrap()).unwrap();
        assert_eq!(saved["tuiMode"], json!("regular"));
    }

    #[test]
    fn falls_back_to_fullscreen_for_unsupported_values() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        std::fs::write(
            format!("{agent_dir}/settings.json"),
            pi_js::json::stringify(&json!({ "tuiMode": "other" })),
        )
        .unwrap();
        let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);
        assert_eq!(manager.get_tui_mode(), TuiMode::Fullscreen);
    }

    #[test]
    fn does_not_recognize_the_old_ui_mode_setting() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        std::fs::write(
            format!("{agent_dir}/settings.json"),
            pi_js::json::stringify(&json!({ "uiMode": "regular" })),
        )
        .unwrap();
        let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);
        assert_eq!(manager.get_tui_mode(), TuiMode::Fullscreen);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn validates_and_persists_fullscreen_settings() {
    let (_temp, agent_dir, project_dir) = setup_dirs();
    let settings_path = format!("{agent_dir}/settings.json");
    let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);
    assert_eq!(manager.get_fullscreen_exit_output(), FullscreenExitOutput::Transcript);
    assert_eq!(manager.get_fullscreen_scrollbar(), ScrollViewScrollbar::Auto);
    assert!(manager.get_fullscreen_copy_on_select());

    manager.set_fullscreen_exit_output(FullscreenExitOutput::ResumeHint);
    manager.set_fullscreen_scrollbar(ScrollViewScrollbar::Hidden);
    manager.set_fullscreen_copy_on_select(false);
    manager.flush().await;

    let saved: Value = pi_js::json::parse(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(saved["fullscreenExitOutput"], json!("resume-hint"));
    assert_eq!(saved["fullscreenScrollbar"], json!("hidden"));
    assert_eq!(saved["fullscreenCopyOnSelect"], json!(false));

    std::fs::write(
        &settings_path,
        pi_js::json::stringify(&json!({ "fullscreenExitOutput": "nothing", "fullscreenScrollbar": "sometimes" })),
    )
    .unwrap();
    let reloaded = SettingsManager::create(&project_dir, Some(&agent_dir), None);
    assert_eq!(reloaded.get_fullscreen_exit_output(), FullscreenExitOutput::Transcript);
    assert_eq!(reloaded.get_fullscreen_scrollbar(), ScrollViewScrollbar::Auto);
    assert!(reloaded.get_fullscreen_copy_on_select());
}

#[tokio::test(flavor = "current_thread")]
async fn persists_fullscreen_wheel_scroll_lines() {
    let (_temp, agent_dir, project_dir) = setup_dirs();
    let settings_path = format!("{agent_dir}/settings.json");
    let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);
    assert_eq!(manager.get_fullscreen_wheel_scroll_lines(), WheelScrollLines::AUTO);

    manager.set_fullscreen_wheel_scroll_lines(WheelScrollLines::Lines(3.0));
    manager.flush().await;
    let saved: Value = pi_js::json::parse(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(saved["fullscreenWheelScrollLines"], json!(3));

    for (raw_val, expected) in [
        (json!(7.9), WheelScrollLines::Lines(7.0)),
        (json!(0), WheelScrollLines::Lines(1.0)),
        (json!(1000), WheelScrollLines::Lines(100.0)),
        (json!("fast"), WheelScrollLines::AUTO),
        (Value::Null, WheelScrollLines::AUTO),
    ] {
        std::fs::write(
            &settings_path,
            pi_js::json::stringify(&json!({ "fullscreenWheelScrollLines": raw_val })),
        )
        .unwrap();
        assert_eq!(
            SettingsManager::create(&project_dir, Some(&agent_dir), None).get_fullscreen_wheel_scroll_lines(),
            expected
        );
    }
}

mod output_pad {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn should_default_to_1_and_persist_binary_values() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        let settings_path = format!("{agent_dir}/settings.json");
        let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);
        assert_eq!(manager.get_output_pad(), 1);

        manager.set_output_pad(0);
        manager.flush().await;
        assert_eq!(manager.get_output_pad(), 0);
        let saved: Value = pi_js::json::parse(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        assert_eq!(saved["outputPad"], json!(0));
    }

    #[test]
    fn should_treat_unsupported_output_pad_values_as_default_padding() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        std::fs::write(
            format!("{agent_dir}/settings.json"),
            pi_js::json::stringify(&json!({ "outputPad": 2 })),
        )
        .unwrap();
        let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);
        assert_eq!(manager.get_output_pad(), 1);
    }
}

mod markdown_mermaid {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn defaults_to_streaming_and_persists_rendering_modes() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        let settings_path = format!("{agent_dir}/settings.json");
        let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);
        assert_eq!(manager.get_mermaid_rendering_mode(), MermaidRenderingMode::Streaming);

        manager.set_mermaid_rendering_mode(MermaidRenderingMode::Final);
        manager.flush().await;
        assert_eq!(manager.get_mermaid_rendering_mode(), MermaidRenderingMode::Final);
        let saved: Value = pi_js::json::parse(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        assert_eq!(saved["markdown"]["mermaid"], json!("final"));
    }

    #[test]
    fn falls_back_to_streaming_for_unsupported_values() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        std::fs::write(
            format!("{agent_dir}/settings.json"),
            pi_js::json::stringify(&json!({ "markdown": { "mermaid": "sometimes" } })),
        )
        .unwrap();
        assert_eq!(
            SettingsManager::create(&project_dir, Some(&agent_dir), None).get_mermaid_rendering_mode(),
            MermaidRenderingMode::Streaming
        );
    }
}

mod shell_command_prefix {
    use super::*;

    #[test]
    fn should_load_shell_command_prefix_from_settings() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        std::fs::write(
            format!("{agent_dir}/settings.json"),
            pi_js::json::stringify(&json!({ "shellCommandPrefix": "shopt -s expand_aliases" })),
        )
        .unwrap();
        let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);
        assert_eq!(
            manager.get_shell_command_prefix().as_deref(),
            Some("shopt -s expand_aliases")
        );
    }

    #[test]
    fn should_return_undefined_when_shell_command_prefix_is_not_set() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        std::fs::write(
            format!("{agent_dir}/settings.json"),
            pi_js::json::stringify(&json!({ "theme": "dark" })),
        )
        .unwrap();
        let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);
        assert_eq!(manager.get_shell_command_prefix(), None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn should_preserve_shell_command_prefix_when_saving_unrelated_settings() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        let settings_path = format!("{agent_dir}/settings.json");
        std::fs::write(
            &settings_path,
            pi_js::json::stringify(&json!({ "shellCommandPrefix": "shopt -s expand_aliases" })),
        )
        .unwrap();
        let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);
        manager.set_theme("light");
        manager.flush().await;

        let saved: Value = pi_js::json::parse(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        assert_eq!(saved["shellCommandPrefix"], json!("shopt -s expand_aliases"));
        assert_eq!(saved["theme"], json!("light"));
    }
}

mod default_tools {
    use super::*;

    #[test]
    fn loads_global_defaults_and_lets_project_settings_replace_them() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        std::fs::write(
            format!("{agent_dir}/settings.json"),
            pi_js::json::stringify(&json!({ "defaultTools": ["read", "bash"] })),
        )
        .unwrap();
        assert_eq!(
            SettingsManager::create(&project_dir, Some(&agent_dir), None).get_default_tools(),
            Some(vec!["read".to_owned(), "bash".to_owned()])
        );

        std::fs::write(
            format!("{project_dir}/.pi/settings.json"),
            pi_js::json::stringify(&json!({ "defaultTools": ["grep"] })),
        )
        .unwrap();
        assert_eq!(
            SettingsManager::create(&project_dir, Some(&agent_dir), None).get_default_tools(),
            Some(vec!["grep".to_owned()])
        );
    }

    #[test]
    fn preserves_an_empty_tool_list() {
        assert_eq!(
            SettingsManager::in_memory(
                Some(Settings {
                    default_tools: Some(Vec::new()),
                    ..Default::default()
                }),
                None
            )
            .get_default_tools(),
            Some(Vec::new())
        );
        assert_eq!(SettingsManager::in_memory(None, None).get_default_tools(), None);
    }

    #[test]
    fn applies_plus_and_minus_to_the_default_selection() {
        assert_eq!(
            SettingsManager::in_memory(
                Some(Settings {
                    default_tools: Some(vec!["+codemode".to_owned(), "-write".to_owned()]),
                    ..Default::default()
                }),
                None
            )
            .get_default_tools(),
            Some(vec![
                "read".to_owned(),
                "bash".to_owned(),
                "edit".to_owned(),
                "codemode".to_owned()
            ])
        );
        assert_eq!(
            SettingsManager::in_memory(
                Some(Settings {
                    default_tools: Some(vec!["read".to_owned(), "+grep".to_owned(), "+read".to_owned()]),
                    ..Default::default()
                }),
                None
            )
            .get_default_tools(),
            Some(vec!["read".to_owned(), "grep".to_owned()])
        );
    }

    #[test]
    fn layers_project_modifiers_on_top_of_the_global_selection() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        std::fs::write(
            format!("{agent_dir}/settings.json"),
            pi_js::json::stringify(&json!({ "defaultTools": ["read", "bash", "+codemode"] })),
        )
        .unwrap();
        std::fs::write(
            format!("{project_dir}/.pi/settings.json"),
            pi_js::json::stringify(&json!({ "defaultTools": ["-codemode", "+tool_search"] })),
        )
        .unwrap();

        let manager = SettingsManager::create(&project_dir, Some(&agent_dir), None);
        assert_eq!(
            manager.get_default_tools(),
            Some(vec!["read".to_owned(), "bash".to_owned(), "tool_search".to_owned()])
        );

        manager.apply_overrides(&Settings {
            default_tools: Some(vec!["+codemode".to_owned()]),
            ..Default::default()
        });
        assert_eq!(
            manager.get_default_tools(),
            Some(vec![
                "read".to_owned(),
                "bash".to_owned(),
                "tool_search".to_owned(),
                "codemode".to_owned()
            ])
        );
    }

    #[test]
    fn applies_project_modifiers_to_the_built_in_defaults_without_a_global_setting() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        std::fs::write(
            format!("{project_dir}/.pi/settings.json"),
            pi_js::json::stringify(&json!({ "defaultTools": ["+codemode"] })),
        )
        .unwrap();

        assert_eq!(
            SettingsManager::create(&project_dir, Some(&agent_dir), None).get_default_tools(),
            Some(vec![
                "read".to_owned(),
                "bash".to_owned(),
                "edit".to_owned(),
                "write".to_owned(),
                "codemode".to_owned()
            ])
        );
    }
}

mod get_session_dir_tests {
    use super::*;

    #[test]
    fn should_return_undefined_when_not_set() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        std::fs::write(
            format!("{agent_dir}/settings.json"),
            pi_js::json::stringify(&json!({ "theme": "dark" })),
        )
        .unwrap();
        assert_eq!(
            SettingsManager::create(&project_dir, Some(&agent_dir), None).get_session_dir(),
            None
        );
    }

    #[test]
    fn should_return_global_session_dir() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        std::fs::write(
            format!("{agent_dir}/settings.json"),
            pi_js::json::stringify(&json!({ "sessionDir": "/tmp/sessions" })),
        )
        .unwrap();
        assert_eq!(
            SettingsManager::create(&project_dir, Some(&agent_dir), None)
                .get_session_dir()
                .as_deref(),
            Some("/tmp/sessions")
        );
    }

    #[test]
    fn should_return_project_session_dir_overriding_global() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        std::fs::write(
            format!("{agent_dir}/settings.json"),
            pi_js::json::stringify(&json!({ "sessionDir": "/global/sessions" })),
        )
        .unwrap();
        std::fs::write(
            format!("{project_dir}/.pi/settings.json"),
            pi_js::json::stringify(&json!({ "sessionDir": "./sessions" })),
        )
        .unwrap();
        assert_eq!(
            SettingsManager::create(&project_dir, Some(&agent_dir), None)
                .get_session_dir()
                .as_deref(),
            Some("./sessions")
        );
    }

    #[test]
    fn should_expand_tilde_in_session_dir() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        std::fs::write(
            format!("{agent_dir}/settings.json"),
            pi_js::json::stringify(&json!({ "sessionDir": "~/sessions" })),
        )
        .unwrap();
        let expected = pi_js::path::join(&[&pi_js::env::home_dir(), "sessions"]);
        assert_eq!(
            SettingsManager::create(&project_dir, Some(&agent_dir), None)
                .get_session_dir()
                .as_deref(),
            Some(expected.as_str())
        );
    }
}

mod get_shell_path_tests {
    use super::*;

    #[test]
    fn should_return_undefined_when_not_set() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        std::fs::write(
            format!("{agent_dir}/settings.json"),
            pi_js::json::stringify(&json!({ "theme": "dark" })),
        )
        .unwrap();
        assert_eq!(
            SettingsManager::create(&project_dir, Some(&agent_dir), None).get_shell_path(),
            None
        );
    }

    #[test]
    fn should_return_an_absolute_shell_path_unchanged() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        std::fs::write(
            format!("{agent_dir}/settings.json"),
            pi_js::json::stringify(&json!({ "shellPath": "/bin/zsh" })),
        )
        .unwrap();
        assert_eq!(
            SettingsManager::create(&project_dir, Some(&agent_dir), None)
                .get_shell_path()
                .as_deref(),
            Some("/bin/zsh")
        );
    }

    #[test]
    fn should_expand_tilde_in_shell_path() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        std::fs::write(
            format!("{agent_dir}/settings.json"),
            pi_js::json::stringify(&json!({ "shellPath": "~/.local/bin/agent-shell-sandbox" })),
        )
        .unwrap();
        let expected = pi_js::path::join(&[&pi_js::env::home_dir(), ".local", "bin", "agent-shell-sandbox"]);
        assert_eq!(
            SettingsManager::create(&project_dir, Some(&agent_dir), None)
                .get_shell_path()
                .as_deref(),
            Some(expected.as_str())
        );
    }

    #[test]
    fn should_expand_a_bare_tilde_in_shell_path() {
        let (_temp, agent_dir, project_dir) = setup_dirs();
        std::fs::write(
            format!("{agent_dir}/settings.json"),
            pi_js::json::stringify(&json!({ "shellPath": "~" })),
        )
        .unwrap();
        let expected = pi_js::env::home_dir();
        assert_eq!(
            SettingsManager::create(&project_dir, Some(&agent_dir), None)
                .get_shell_path()
                .as_deref(),
            Some(expected.as_str())
        );
    }
}
