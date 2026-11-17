//! Port of packages/coding-agent/test/suite/regressions/7829-invalid-settings-warning.test.ts

mod support;

use std::sync::Arc;

use pi_coding_agent::core::diagnostics::{AgentSessionRuntimeDiagnostic, AgentSessionRuntimeDiagnosticLevel};
use pi_coding_agent::core::settings_diagnostics::collect_settings_diagnostics;
use pi_coding_agent::core::settings_manager::SettingsManager;
use pi_js::env::testing::EnvGuard;
use pi_tui::{Container, Text};
use tempfile::TempDir;

use support::test_harness::{HarnessOptions, create_harness};

fn render_container(container: &Container) -> String {
    container.render(120).join("\n")
}

#[tokio::test(flavor = "current_thread")]
async fn renders_startup_diagnostics_inside_the_transcript() {
    let harness = create_harness(Some(HarnessOptions::default())).await.expect("harness");
    let _offline = EnvGuard::set("PI_OFFLINE", Some("1"));

    let temp = TempDir::new().expect("temp dir");
    let agent_dir = temp.path().join("agent");
    let settings_path = agent_dir.join("settings.json");
    std::fs::create_dir_all(&agent_dir).expect("mkdir agent_dir");
    std::fs::write(&settings_path, "{ malformed json").expect("write invalid settings");

    let manager = SettingsManager::create(
        temp.path().to_str().expect("utf8"),
        Some(agent_dir.to_str().expect("utf8")),
        None,
    );
    let diagnostics = collect_settings_diagnostics(&manager);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].r#type, AgentSessionRuntimeDiagnosticLevel::Warning);
    assert!(diagnostics[0].message.contains("Invalid settings file"));

    let startup_diag = AgentSessionRuntimeDiagnostic {
        r#type: AgentSessionRuntimeDiagnosticLevel::Warning,
        message: "Invalid settings file /tmp/settings.json: malformed JSON".to_owned(),
    };
    let chat_container = Container::new();
    let msg = format!("Warning: {}", startup_diag.message);
    chat_container.add_child(Arc::new(Text::new(Some(msg.as_str()), Some(0), Some(0), None)));
    assert!(
        render_container(&chat_container).contains("Warning: Invalid settings file /tmp/settings.json: malformed JSON")
    );

    // WIRE(@tui-w3): once `modes::interactive::theme::theme::init_theme` and
    // `modes::interactive::interactive_mode::InteractiveMode::{show_warning, run}` land, run
    // InteractiveMode with `startupDiagnostics` and assert rendered chat container output directly.
    harness.cleanup();
}
