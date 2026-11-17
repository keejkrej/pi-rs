//! Port of packages/coding-agent/test/first-time-setup.test.ts

use pi_coding_agent::cli::startup_ui::should_run_first_time_setup;
use pi_coding_agent::config::ENV_AGENT_DIR;
use pi_coding_agent::core::settings_manager::SettingsManager;
use serial_test::serial;

#[test]
#[serial]
fn returns_true_when_experimental_default_agent_dir_and_no_settings_json() {
    let _exp = pi_js::env::testing::EnvGuard::set("PI_EXPERIMENTAL", Some("1"));
    let _agent = pi_js::env::testing::EnvGuard::set(&ENV_AGENT_DIR, None);
    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");

    assert!(should_run_first_time_setup(settings_path.to_str()));
}

#[test]
#[serial]
fn returns_false_when_experimental_features_are_disabled() {
    let _exp = pi_js::env::testing::EnvGuard::set("PI_EXPERIMENTAL", None);
    let _agent = pi_js::env::testing::EnvGuard::set(&ENV_AGENT_DIR, None);
    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");

    assert!(!should_run_first_time_setup(settings_path.to_str()));
}

#[test]
#[serial]
fn returns_false_when_a_custom_agent_dir_is_set() {
    let _exp = pi_js::env::testing::EnvGuard::set("PI_EXPERIMENTAL", Some("1"));
    let dir = tempfile::tempdir().unwrap();
    let _agent = pi_js::env::testing::EnvGuard::set(&ENV_AGENT_DIR, dir.path().to_str());
    let settings_path = dir.path().join("settings.json");

    assert!(!should_run_first_time_setup(settings_path.to_str()));
}

#[test]
#[serial]
fn returns_false_when_settings_json_already_exists() {
    let _exp = pi_js::env::testing::EnvGuard::set("PI_EXPERIMENTAL", Some("1"));
    let _agent = pi_js::env::testing::EnvGuard::set(&ENV_AGENT_DIR, None);
    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");
    std::fs::write(&settings_path, "{}").unwrap();

    assert!(!should_run_first_time_setup(settings_path.to_str()));
}

#[test]
fn analytics_defaults_to_disabled_with_no_tracking_identifier() {
    let manager = SettingsManager::in_memory(None, None);

    assert!(!manager.get_enable_analytics());
    assert!(manager.get_tracking_id().is_none());
}

#[test]
fn analytics_generates_and_keeps_tracking_identifier_across_toggles() {
    let manager = SettingsManager::in_memory(None, None);

    manager.set_enable_analytics(true);
    let tracking_id = manager.get_tracking_id();
    assert!(tracking_id.as_ref().is_some_and(|id| !id.is_empty()));

    manager.set_enable_analytics(false);
    assert!(!manager.get_enable_analytics());
    assert_eq!(manager.get_tracking_id(), tracking_id);

    manager.set_enable_analytics(true);
    assert_eq!(manager.get_tracking_id(), tracking_id);
}
