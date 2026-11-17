//! Port of packages/coding-agent/test/suite/regressions/8261-subagent-project-trust.test.ts

#[path = "support/mod.rs"]
mod support;

#[allow(dead_code)]
#[path = "../examples/extensions__subagent/main.rs"]
mod subagent_example;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pi_agent_core::types::AgentMessage;
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxResponseStep, faux_assistant_message, faux_tool_call,
};
use pi_ai::types::StopReason;
use pi_coding_agent::core::agent_session::ExtensionBindings;
use pi_coding_agent::core::extensions::types::{
    ExtensionMode, ExtensionUIContext, ExtensionUIDialogOptions, InlineExtension, NotifyType,
};
use pretty_assertions::assert_eq;
use serde_json::json;
use support::suite::harness::{HarnessOptions, create_harness, get_message_text};

struct ConfirmUi {
    confirm_calls: Arc<AtomicUsize>,
    confirm_result: bool,
}

#[async_trait::async_trait]
impl ExtensionUIContext for ConfirmUi {
    async fn select(
        &self,
        _title: &str,
        _options: Vec<String>,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        Ok(None)
    }

    async fn confirm(
        &self,
        _title: &str,
        _message: &str,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<bool> {
        self.confirm_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.confirm_result)
    }

    async fn input(
        &self,
        _title: &str,
        _placeholder: Option<&str>,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        Ok(None)
    }

    fn notify(&self, _message: &str, _notify_type: Option<NotifyType>) {}
    fn on_terminal_input(
        &self,
        _handler: pi_coding_agent::core::extensions::types::TerminalInputHandler,
    ) -> pi_js::Unsubscribe {
        pi_js::Unsubscribe::noop()
    }
    fn set_status(&self, _key: &str, _text: Option<&str>) {}
    fn set_working_message(&self, _message: Option<&str>) {}
    fn set_working_visible(&self, _visible: bool) {}
    fn set_working_indicator(
        &self,
        _options: Option<pi_coding_agent::core::extensions::types::WorkingIndicatorOptions>,
    ) {
    }
    fn set_hidden_thinking_label(&self, _label: Option<&str>) {}
    fn set_widget_lines(
        &self,
        _key: &str,
        _content: Option<Vec<String>>,
        _options: Option<pi_coding_agent::core::extensions::types::ExtensionWidgetOptions>,
    ) {
    }
    fn set_widget_factory(
        &self,
        _key: &str,
        _factory: Option<pi_coding_agent::core::extensions::types::WidgetFactory>,
        _options: Option<pi_coding_agent::core::extensions::types::ExtensionWidgetOptions>,
    ) {
    }
    fn set_footer(&self, _factory: Option<pi_coding_agent::core::extensions::types::FooterFactory>) {}
    fn set_header(&self, _factory: Option<pi_coding_agent::core::extensions::types::HeaderFactory>) {}
    fn set_title(&self, _title: &str) {}
    async fn custom(
        &self,
        _factory: pi_coding_agent::core::extensions::types::CustomUiFactory,
        _options: Option<pi_coding_agent::core::extensions::types::ExtensionCustomOptions>,
    ) -> pi_js::Result<serde_json::Value> {
        Ok(serde_json::Value::Null)
    }
    fn paste_to_editor(&self, _text: &str) {}
    fn set_editor_text(&self, _text: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    async fn editor(&self, _title: &str, _prefill: Option<&str>) -> pi_js::Result<Option<String>> {
        Ok(None)
    }
    fn add_autocomplete_provider(
        &self,
        _factory: pi_coding_agent::core::extensions::types::AutocompleteProviderFactory,
    ) {
    }
    fn set_editor_component(&self, _factory: Option<pi_coding_agent::core::extensions::types::EditorFactory>) {}
    fn get_editor_component(&self) -> Option<pi_coding_agent::core::extensions::types::EditorFactory> {
        None
    }
    fn theme(&self) -> pi_coding_agent::core::extensions::types::Theme {
        pi_coding_agent::core::extensions::types::Theme::default()
    }
    fn get_all_themes(&self) -> Vec<pi_coding_agent::core::extensions::types::ThemeInfo> {
        Vec::new()
    }
    fn get_theme(&self, _name: &str) -> Option<pi_coding_agent::core::extensions::types::Theme> {
        None
    }
    fn set_theme_by_name(&self, _name: &str) -> pi_coding_agent::core::extensions::types::SetThemeResult {
        pi_coding_agent::core::extensions::types::SetThemeResult {
            success: false,
            error: None,
        }
    }
    fn set_theme(
        &self,
        _theme: pi_coding_agent::core::extensions::types::Theme,
    ) -> pi_coding_agent::core::extensions::types::SetThemeResult {
        pi_coding_agent::core::extensions::types::SetThemeResult {
            success: false,
            error: None,
        }
    }
    fn get_tools_expanded(&self) -> bool {
        false
    }
    fn set_tools_expanded(&self, _expanded: bool) {}
}

async fn run_project_agent(trusted: bool, confirm_result: bool) -> (usize, String) {
    let missing_home = pi_js::fs::mkdtemp(&pi_js::path::join(&[&pi_js::env::tmp_dir(), "pi-8261-home-"])).unwrap();
    let _home_guard = pi_js::env::testing::set("HOME", &missing_home);
    let _agent_dir_guard =
        pi_js::env::testing::set("PI_CODING_AGENT_DIR", &pi_js::path::join(&[&missing_home, "agent"]));

    let harness = create_harness(HarnessOptions {
        extension_factories: Some(vec![InlineExtension::Factory(subagent_example::create_extension())]),
        ..Default::default()
    })
    .await;

    let agents_dir = pi_js::path::join(&[&harness.temp_dir, ".pi", "agents"]);
    pi_js::fs::mkdir(&agents_dir, true, None).unwrap();
    pi_js::fs::write_file(
        &pi_js::path::join(&[&agents_dir, "project-agent.md"]),
        b"---\nname: project-agent\ndescription: Project test agent\n---\n\nHandle the delegated task.\n",
        None,
    )
    .unwrap();
    harness.settings_manager.set_project_trusted(trusted);

    let confirm_calls = Arc::new(AtomicUsize::new(0));
    let ui = Arc::new(ConfirmUi {
        confirm_calls: Arc::clone(&confirm_calls),
        confirm_result,
    });
    harness
        .session
        .bind_extensions(ExtensionBindings {
            ui_context: Some(ui),
            mode: Some(ExtensionMode::Tui),
            ..Default::default()
        })
        .await
        .unwrap();

    harness.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Blocks(vec![faux_tool_call(
                "subagent",
                json!({
                    "agent": "project-agent",
                    "task": "Test project trust",
                    "agentScope": "project",
                    "cwd": pi_js::path::join(&[&harness.temp_dir, "missing-cwd"]),
                }),
            )]),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Text("done".to_string()),
            None,
        )),
    ]);

    harness.session.prompt("Delegate this task", None).await.unwrap();

    let tool_result_text = harness
        .session
        .messages()
        .into_iter()
        .find(|m| m.role() == Some("toolResult"))
        .as_ref()
        .map(get_message_text)
        .unwrap_or_default();

    let count = confirm_calls.load(Ordering::SeqCst);
    harness.cleanup();
    let _ = pi_js::fs::rm(&missing_home, true, true);
    (count, tool_result_text)
}

#[serial_test::serial]
#[tokio::test]
async fn skips_per_call_confirmation_for_trusted_projects() {
    let (confirm_calls, tool_result) = run_project_agent(true, false).await;
    assert_eq!(confirm_calls, 0);
    assert!(!tool_result.contains("Canceled:"));
}

#[serial_test::serial]
#[tokio::test]
async fn keeps_confirmation_for_untrusted_interactive_projects() {
    let (confirm_calls, tool_result) = run_project_agent(false, false).await;
    assert_eq!(confirm_calls, 1);
    assert!(tool_result.contains("Canceled: project-local agents not approved."));
}
