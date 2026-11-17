//! Port of packages/coding-agent/test/suite/regressions/5943-session-start-notify.test.ts

#[path = "support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex, PoisonError};

use pi_coding_agent::core::agent_session::{ExtensionBindings, ReloadOptions};
use pi_coding_agent::core::extensions::types::{
    ExtensionApi, ExtensionMode, ExtensionUIContext, ExtensionUIDialogOptions, InlineExtension, NotifyType,
    SessionStartEvent,
};
use pretty_assertions::assert_eq;
use support::suite::harness::{HarnessOptions, create_harness};

struct NotifyRecorderUi {
    events: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl ExtensionUIContext for NotifyRecorderUi {
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
        Ok(false)
    }
    async fn input(
        &self,
        _title: &str,
        _placeholder: Option<&str>,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        Ok(None)
    }
    fn notify(&self, message: &str, _notify_type: Option<NotifyType>) {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(message.to_string());
    }
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

// WIRE(@tui-w3): InteractiveMode tests deferred until modes::interactive lands.
#[ignore = "WIRE(@tui-w3): waiting for InteractiveMode::show_loaded_resources in modes::interactive"]
#[tokio::test]
async fn renders_loaded_resources_before_restored_messages_without_stale_entries() {}

#[ignore = "WIRE(@tui-w3): waiting for InteractiveMode::rebind_current_session in modes::interactive"]
#[tokio::test]
async fn renders_replacement_session_state_before_session_start_handlers_can_notify() {}

#[ignore = "WIRE(@tui-w3): waiting for InteractiveMode::rebind_current_session in modes::interactive"]
#[tokio::test]
async fn subscribes_before_replacement_session_start_handlers_send_messages() {}

#[ignore = "WIRE(@tui-w3): waiting for InteractiveMode::rebind_current_session in modes::interactive"]
#[tokio::test]
async fn subscribes_before_replacement_session_start_handlers_send_user_messages() {}

#[tokio::test]
async fn runs_the_reload_render_hook_before_reload_session_start_handlers_can_notify() {
    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    let ev_ext = Arc::clone(&events);
    let ext = InlineExtension::Factory(Arc::new(move |pi: ExtensionApi| {
        let ev = Arc::clone(&ev_ext);
        async move {
            pi.on::<SessionStartEvent>(move |event, ctx| {
                let ev = Arc::clone(&ev);
                Box::pin(async move {
                    let reason = match event.reason {
                        pi_coding_agent::core::extensions::types::SessionStartReason::Startup => "startup",
                        pi_coding_agent::core::extensions::types::SessionStartReason::Reload => "reload",
                        pi_coding_agent::core::extensions::types::SessionStartReason::New => "new",
                        pi_coding_agent::core::extensions::types::SessionStartReason::Resume => "resume",
                        pi_coding_agent::core::extensions::types::SessionStartReason::Fork => "fork",
                    };
                    ev.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(format!("start:{reason}"));
                    ctx.ui()?.notify(&format!("notify:{reason}"), Some(NotifyType::Error));
                    Ok(None)
                })
            });
            Ok(())
        }
    }));

    let harness = create_harness(HarnessOptions {
        extension_factories: Some(vec![ext]),
        ..Default::default()
    })
    .await;

    let ui = Arc::new(NotifyRecorderUi {
        events: Arc::clone(&events),
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

    assert_eq!(
        *events.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["start:startup", "notify:startup"]
    );

    events.lock().unwrap_or_else(PoisonError::into_inner).clear();
    let ev_hook = Arc::clone(&events);
    let hook_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hc_c = Arc::clone(&hook_calls);
    harness
        .session
        .reload(Some(ReloadOptions {
            before_session_start: Some(Box::new(move || {
                let ev = Arc::clone(&ev_hook);
                let hc = Arc::clone(&hc_c);
                Box::pin(async move {
                    hc.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    ev.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push("render".to_string());
                    Ok(())
                })
            })),
        }))
        .await
        .unwrap();

    assert_eq!(hook_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        *events.lock().unwrap_or_else(PoisonError::into_inner),
        vec!["render", "start:reload", "notify:reload"]
    );
    harness.cleanup();
}

#[ignore = "WIRE(@tui-w3): waiting for InteractiveMode::handle_reload_command in modes::interactive"]
#[tokio::test]
async fn refreshes_hide_thinking_block_before_rebuilding_chat_during_reload() {}

#[ignore = "WIRE(@tui-w3): waiting for InteractiveMode::handle_reload_command in modes::interactive"]
#[tokio::test]
async fn keeps_the_reload_blocker_focused_until_async_reload_completes() {}
