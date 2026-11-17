//! Port of packages/coding-agent/test/suite/regressions/7829-invalid-settings-warning.test.ts

mod support;

use std::sync::Arc;

use async_trait::async_trait;
use pi_coding_agent::core::diagnostics::{AgentSessionRuntimeDiagnostic, AgentSessionRuntimeDiagnosticLevel};
use pi_coding_agent::core::settings_diagnostics::collect_settings_diagnostics;
use pi_coding_agent::core::settings_manager::SettingsManager;
use pi_coding_agent::modes::interactive::components::settings_selector::TuiMode;
use pi_coding_agent::modes::interactive::interactive_mode::{
    AgentSessionRuntimeDiagnostic as UiRuntimeDiagnostic, CancelledResult, InteractiveMode, InteractiveModeHooks,
    InteractiveModeOptions, InteractiveRuntimeHost, InteractiveSession, RuntimeForkOptions, RuntimeForkResult,
    SwitchSessionOptions,
};
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_js::BoxFuture;
use pi_js::env::testing::EnvGuard;
use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
use pi_tui::{Container, Text, strip_terminal_sequences};
use serial_test::serial;
use tempfile::TempDir;

use support::test_harness::{HarnessOptions, create_harness};

struct ThemeResetGuard;

impl Drop for ThemeResetGuard {
    fn drop(&mut self) {
        init_theme(Some("dark"), None);
    }
}

struct FakeTerminal;

#[async_trait]
impl Terminal for FakeTerminal {
    fn start(&self, _on_input: TerminalInputHandler, _on_resize: TerminalResizeHandler) {}
    fn stop(&self) {}
    async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}
    fn write(&self, _data: &str) {}
    fn columns(&self) -> i64 {
        120
    }
    fn rows(&self) -> i64 {
        30
    }
    fn kitty_protocol_active(&self) -> bool {
        false
    }
    fn move_by(&self, _lines: i64) {}
    fn hide_cursor(&self) {}
    fn show_cursor(&self) {}
    fn clear_line(&self) {}
    fn clear_from_cursor(&self) {}
    fn clear_screen(&self) {}
    fn set_title(&self, _title: &str) {}
    fn set_progress(&self, _active: bool) {}
}

struct HarnessHost {
    session: Arc<dyn InteractiveSession>,
    agent_dir: String,
    diagnostics: Vec<UiRuntimeDiagnostic>,
}

#[async_trait]
impl InteractiveRuntimeHost for HarnessHost {
    fn session(&self) -> Arc<dyn InteractiveSession> {
        Arc::clone(&self.session)
    }
    fn agent_dir(&self) -> String {
        self.agent_dir.clone()
    }
    fn diagnostics(&self) -> Vec<UiRuntimeDiagnostic> {
        self.diagnostics.clone()
    }
    fn model_fallback_message(&self) -> Option<String> {
        None
    }
    fn set_before_session_invalidate(&self, _cb: Arc<dyn Fn() + Send + Sync>) {}
    fn set_rebind_session(&self, _cb: Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>) {}
    async fn new_session(&self) -> pi_js::Result<CancelledResult> {
        Ok(CancelledResult { cancelled: false })
    }
    async fn fork(&self, _entry_id: &str, _options: Option<RuntimeForkOptions>) -> pi_js::Result<RuntimeForkResult> {
        Ok(RuntimeForkResult {
            cancelled: true,
            selected_text: None,
        })
    }
    async fn switch_session(
        &self,
        _session_path: &str,
        _options: Option<SwitchSessionOptions>,
    ) -> pi_js::Result<CancelledResult> {
        Ok(CancelledResult { cancelled: false })
    }
    async fn import_from_jsonl(
        &self,
        _input_path: &str,
        _cwd_override: Option<&str>,
    ) -> pi_js::Result<CancelledResult> {
        Ok(CancelledResult { cancelled: false })
    }
    async fn dispose(&self) {}
}

fn render_container(container: &Container) -> String {
    container.render(120).join("\n")
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn renders_startup_diagnostics_inside_the_transcript() {
    init_theme(Some("dark"), Some(false));
    let _theme_guard = ThemeResetGuard;
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

    let cwd_str = temp.path().to_str().expect("utf8").to_owned();
    let agent_str = agent_dir.to_str().expect("utf8").to_owned();
    let created =
        pi_coding_agent::core::sdk::create_agent_session(Some(pi_coding_agent::core::sdk::CreateAgentSessionOptions {
            cwd: Some(cwd_str.clone()),
            agent_dir: Some(agent_str.clone()),
            settings_manager: Some(manager),
            session_manager: Some(
                pi_coding_agent::core::session_manager::SessionManager::in_memory(Some(&cwd_str), None, None)
                    .expect("session_manager"),
            ),
            ..Default::default()
        }))
        .await
        .expect("create_agent_session");

    let ui_diags: Vec<UiRuntimeDiagnostic> = diagnostics
        .into_iter()
        .map(|d| UiRuntimeDiagnostic {
            diagnostic_type: pi_coding_agent::modes::interactive::interactive_mode::DiagnosticLevel::Warning,
            message: d.message,
        })
        .collect();
    let host = Arc::new(HarnessHost {
        session: Arc::new(created.session.clone()),
        agent_dir: agent_str,
        diagnostics: ui_diags.clone(),
    });
    let mode = InteractiveMode::new(
        host,
        InteractiveModeOptions {
            startup_diagnostics: Some(ui_diags),
            tui_mode: Some(TuiMode::Regular),
            terminal: Some(Arc::new(FakeTerminal)),
            hooks: Some(InteractiveModeHooks {
                exit_process: Arc::new(|_| {}),
                ..Default::default()
            }),
            ..Default::default()
        },
    );
    let mode_run = mode.clone();
    let run_task = tokio::spawn(async move {
        mode_run.run().await;
    });

    let expected_path = settings_path.to_str().expect("utf8");
    let mut rendered = String::new();
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        rendered = strip_terminal_sequences(&render_container(mode.chat_container()));
        if rendered.contains("Warning: Invalid settings file") && rendered.contains(expected_path) {
            break;
        }
    }
    assert!(
        rendered.contains("Warning: Invalid settings file") && rendered.contains(expected_path),
        "rendered was:\n{rendered}"
    );

    mode.shutdown(false).await;
    run_task.abort();
    created.session.dispose();
    harness.cleanup();
}
