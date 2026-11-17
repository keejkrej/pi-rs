//! Port of packages/coding-agent/test/bug-report.test.ts (unit coding-agent-interactive-support)

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_coding_agent::core::keybindings::{KEYBINDINGS, KeybindingsManager as AppKeybindingsManager};
use pi_coding_agent::modes::interactive::bug_report::{
    BugReportBundleData, BugReportContext, BugReportDelivery, BugReportOptions, BugReportSession, report_bug,
};
use pi_coding_agent::modes::interactive::components::extension_editor::ExtensionEditorComponent;
use pi_coding_agent::modes::interactive::components::extension_selector::ExtensionSelectorComponent;
use pi_coding_agent::modes::interactive::theme::theme::{get_editor_theme, init_theme};
use pi_js::abort::AbortSignal;
use pi_js::{BoxFuture, Result};
use pi_tui::terminal::{TerminalInputHandler, TerminalResizeHandler};
use pi_tui::{
    Component, Container, Editor, KeybindingsManager, TUI, TUI_KEYBINDINGS, Terminal, TuiMainScreen, set_keybindings,
    strip_terminal_sequences,
};
use serde_json::json;
use serial_test::serial;

struct TestGuard;

impl Drop for TestGuard {
    fn drop(&mut self) {
        init_theme(Some("dark"), None);
        set_keybindings(KeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
    }
}

struct VirtualTerminal;

#[async_trait]
impl Terminal for VirtualTerminal {
    fn start(&self, _on_input: TerminalInputHandler, _on_resize: TerminalResizeHandler) {}
    fn stop(&self) {}
    async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}
    fn write(&self, _data: &str) {}
    fn columns(&self) -> i64 {
        120
    }
    fn rows(&self) -> i64 {
        40
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

struct FakeBugReportSession;

impl BugReportSession for FakeBugReportSession {
    fn model_name(&self) -> Option<String> {
        None
    }
    fn model_provider(&self) -> Option<String> {
        None
    }
    fn external_editor_command(&self) -> Option<String> {
        Some("nano".to_string())
    }
    fn summarize_for_bug_report(&self, _hint: Option<&str>, _signal: AbortSignal) -> BoxFuture<Result<String>> {
        Box::pin(async { Ok(String::new()) })
    }
    fn build_bundle(&self, _options: &BugReportOptions, _summary: Option<&str>) -> Result<BugReportBundleData> {
        Ok(BugReportBundleData {
            id: "id-1".to_string(),
            created_at: "2026-01-01T00:00:00.000Z".to_string(),
            hint: None,
            session_included: false,
            summary_included: false,
            crash_count: 0,
            payload: json!({}),
        })
    }
    fn upload_bundle(&self, _bundle: &BugReportBundleData, _signal: AbortSignal) -> BoxFuture<Result<String>> {
        Box::pin(async { Ok("id-1".to_string()) })
    }
    fn write_archive(&self, _bundle: &BugReportBundleData, _archive_path: &str) -> BoxFuture<Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn archive_file_name(&self, id: &str) -> String {
        format!("pi-bug-{id}.zip")
    }
    fn record_in_session(
        &self,
        _bundle: &BugReportBundleData,
        _delivery: BugReportDelivery,
        _archive_path: Option<&str>,
    ) {
    }
    fn clear_crash_log(&self) {}
}

#[tokio::test]
#[serial]
async fn preserves_line_breaks_in_pasted_descriptions() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);
    let keybindings = AppKeybindingsManager::new(None, None);
    set_keybindings(KeybindingsManager::new(KEYBINDINGS.clone(), None));

    let ui: Arc<dyn TUI + Send + Sync> = Arc::new(TuiMainScreen::new(Arc::new(VirtualTerminal), None, None));
    let editor_container = Container::new();
    let editor: Arc<dyn Component> = Arc::new(Editor::new(Arc::clone(&ui), get_editor_theme(), None));
    let statuses = Arc::new(Mutex::new(Vec::<String>::new()));
    let statuses_clone = Arc::clone(&statuses);

    let ctx = BugReportContext {
        session: Arc::new(FakeBugReportSession),
        ui: Arc::clone(&ui),
        editor_container: editor_container.clone(),
        editor: Arc::clone(&editor),
        keybindings,
        show_status: Arc::new(move |msg| statuses_clone.lock().unwrap().push(msg.to_string())),
        show_error: Arc::new(|_| {}),
    };

    let report_handle = tokio::spawn(async move {
        report_bug(&ctx, None).await;
    });

    for _ in 0..5 {
        tokio::task::yield_now().await;
    }

    let description_editor = editor_container.children()[0].clone();
    assert!(description_editor.render(120).join("\n").contains("Report a bug"));
    let _ = ExtensionEditorComponent::render;

    description_editor.handle_input("\x1b[200~Request failed\r\n  ↳ pi exiting...\r\nstack trace\x1b[201~");
    description_editor.handle_input("\r");
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }

    let transcript_selector = editor_container.children()[0].clone();
    assert!(
        transcript_selector
            .render(120)
            .join("\n")
            .contains("Include the session transcript?")
    );
    let _ = ExtensionSelectorComponent::render;
    transcript_selector.handle_input("j");
    transcript_selector.handle_input("\r");
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }

    let summary_selector = editor_container.children()[0].clone();
    assert!(
        summary_selector
            .render(120)
            .join("\n")
            .contains("Attach a summary written by")
    );
    summary_selector.handle_input("j");
    summary_selector.handle_input("\r");
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }

    let delivery_selector = editor_container.children()[0].clone();
    let delivery_lines: Vec<String> = delivery_selector
        .render(120)
        .into_iter()
        .map(|l| strip_terminal_sequences(&l))
        .collect();
    assert!(delivery_lines.iter().any(|l| l.contains("Description: Request failed")));
    assert!(delivery_lines.iter().any(|l| l.contains("↳ pi exiting...")));
    assert!(delivery_lines.iter().any(|l| l.contains("stack trace")));

    delivery_selector.handle_input("j");
    delivery_selector.handle_input("j");
    delivery_selector.handle_input("\r");
    report_handle.await.unwrap();

    assert_eq!(*statuses.lock().unwrap(), vec!["Bug report cancelled".to_string()]);
    assert!(Arc::ptr_eq(&editor_container.children()[0], &editor));
}
