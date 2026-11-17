//! Port of packages/coding-agent/test/first-time-setup-fork.test.ts

use pi_coding_agent::cli::startup_ui::{DistributionMetadata, should_run_first_time_setup, testing};
use pi_coding_agent::config::ENV_AGENT_DIR;
use serial_test::serial;

#[test]
#[serial]
fn returns_false_for_a_forked_package() {
    let _exp = pi_js::env::testing::set("PI_EXPERIMENTAL", Some("1"));
    let _agent = pi_js::env::testing::set(&ENV_AGENT_DIR, None);
    let _meta = testing::override_distribution_metadata(|| DistributionMetadata {
        package_name: "@example/pi-coding-agent".to_string(),
        app_name: "pi".to_string(),
        config_dir_name: ".pi".to_string(),
        env_agent_dir: "PI_CODING_AGENT_DIR".to_string(),
    });
    let dir = tempfile::tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");

    assert!(!should_run_first_time_setup(settings_path.to_str()));
}
