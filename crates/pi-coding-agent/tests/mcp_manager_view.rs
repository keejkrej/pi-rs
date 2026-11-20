//! Port of packages/coding-agent/test/mcp-manager-view.test.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use pi_coding_agent::core::keybindings::KEYBINDINGS;
use pi_coding_agent::core::keybindings::KeybindingsManager;
use pi_coding_agent::extensions::mcp::ui::{McpManagerView, McpMenu, McpUi};
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_coding_agent::utils::ansi::strip_ansi;
use pi_tui::components::select_list::SelectItem;
use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
use pi_tui::{
    Component, KeybindingsManager as TuiKeybindingsManager, TUI, TUI_KEYBINDINGS, TuiMainScreen, set_keybindings,
};
use serial_test::serial;

const ESCAPE: &str = "\x1b";

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
        24
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

struct TestGuard;

impl Drop for TestGuard {
    fn drop(&mut self) {
        init_theme(Some("dark"), None);
        set_keybindings(TuiKeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
    }
}

fn setup() -> (TestGuard, Arc<dyn TUI>, TuiKeybindingsManager) {
    init_theme(Some("dark"), Some(false));
    let kb = KeybindingsManager::new(None, None);
    let tui_kb = TuiKeybindingsManager::new(KEYBINDINGS.clone(), Some(kb.get_user_bindings()));
    set_keybindings(tui_kb.clone());
    let ui: Arc<dyn TUI> = Arc::new(TuiMainScreen::new(Arc::new(FakeTerminal), None, None));
    (TestGuard, ui, tui_kb)
}

fn rendered(view: &McpManagerView) -> String {
    strip_ansi(&view.render(80).join("\n"))
}

// #10565
#[test]
#[serial]
fn status_screen_cancels_running_operation_with_cancel_key() {
    let (_guard, ui, kb) = setup();
    let view = McpManagerView::new(ui, pi_coding_agent::core::extensions::types::Theme::default(), kb);
    let cancelled = Arc::new(AtomicUsize::new(0));
    let cancelled_c = Arc::clone(&cancelled);
    view.status(
        "Sign in to issues",
        "Contacting the authorization server…",
        Some(Arc::new(move || {
            cancelled_c.fetch_add(1, Ordering::SeqCst);
        })),
    );
    assert!(rendered(&view).contains("cancel"));
    view.handle_input(ESCAPE);
    assert_eq!(cancelled.load(Ordering::SeqCst), 1);
}

#[test]
#[serial]
fn status_screen_ignores_cancel_key_when_not_cancellable() {
    let (_guard, ui, kb) = setup();
    let view = McpManagerView::new(ui, pi_coding_agent::core::extensions::types::Theme::default(), kb);
    view.status("Sign in to issues", "Connecting…", None);
    assert!(!rendered(&view).contains("cancel"));
    view.handle_input(ESCAPE);
}

// #7629
#[tokio::test]
#[serial]
async fn menu_captures_fullscreen_viewport_keys_only_while_menu_list_is_shown() {
    let (_guard, ui, kb) = setup();
    let view = McpManagerView::new(ui, pi_coding_agent::core::extensions::types::Theme::default(), kb);
    let items: Vec<SelectItem> = (0..20)
        .map(|index| SelectItem {
            value: format!("server-{index}"),
            label: format!("server-{index}"),
            description: None,
        })
        .collect();

    let view_c = view.clone();
    let result_handle = tokio::spawn(async move {
        view_c
            .menu(
                Arc::new(move || McpMenu {
                    title: "MCP servers".to_string(),
                    details: None,
                    error: None,
                    items: items.clone(),
                    empty: None,
                    selected: None,
                    confirm_label: "open".to_string(),
                    cancel_label: "close".to_string(),
                }),
                None,
            )
            .await
    });
    tokio::task::yield_now().await;

    assert!(view.captures_viewport_input());
    view.handle_input("\x1b[6~"); // PageDown
    view.handle_input("\r");
    let chosen = result_handle.await.expect("join menu task");
    assert_eq!(chosen.as_deref(), Some("server-12"));

    view.status("Sign in to issues", "Connecting…", None);
    assert!(!view.captures_viewport_input());
}
