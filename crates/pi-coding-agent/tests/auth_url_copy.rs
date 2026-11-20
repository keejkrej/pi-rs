//! Port of packages/coding-agent/test/auth-url-copy.test.ts

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use pi_coding_agent::core::keybindings::{KEYBINDINGS, KeybindingsManager};
use pi_coding_agent::extensions::mcp::ui::{McpManagerView, McpUi};
use pi_coding_agent::modes::interactive::components::login_dialog::{DeviceCodeInfo, LoginDialogComponent, testing};
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_coding_agent::utils::ansi::strip_ansi;
use pi_js::abort::AbortController;
use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
use pi_tui::{
    Component, KeybindingsManager as TuiKeybindingsManager, TUI, TUI_KEYBINDINGS, TuiMainScreen, set_keybindings,
};
use serial_test::serial;

const CTRL_X: &str = "\x18";

fn long_url() -> String {
    format!("https://auth.example.invalid/authorize?{}", "x".repeat(300))
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

fn setup() -> (TestGuard, Arc<dyn TUI + Send + Sync>) {
    init_theme(Some("dark"), Some(false));
    let kb = KeybindingsManager::new(None, None);
    set_keybindings(TuiKeybindingsManager::new(
        KEYBINDINGS.clone(),
        Some(kb.get_user_bindings()),
    ));
    let ui: Arc<dyn TUI + Send + Sync> = Arc::new(TuiMainScreen::new(Arc::new(FakeTerminal), None, None));
    (TestGuard, ui)
}

fn rendered(component: &dyn Component) -> String {
    strip_ansi(&component.render(80).join("\n"))
}

#[tokio::test]
#[serial]
async fn login_dialog_copies_auth_url_instead_of_typing_into_code_input() {
    let (_guard, ui) = setup();
    let copied = Arc::new(Mutex::new(Vec::<String>::new()));
    let copied_c = Arc::clone(&copied);
    let _browser = testing::override_open_browser(|_| {});
    let _clip = testing::override_copy_to_clipboard(move |text| {
        let copied_c = Arc::clone(&copied_c);
        let text = text.to_string();
        Box::pin(async move {
            copied_c.lock().unwrap_or_else(|e| e.into_inner()).push(text);
            Ok(())
        })
    });

    let dialog = LoginDialogComponent::new(ui, "test", Arc::new(|_, _| {}), None, None);
    let url = long_url();
    dialog.show_auth(&url, None);
    let _manual = dialog.show_manual_input("Paste the code:");
    assert!(rendered(&dialog).contains("ctrl+x to copy"));

    dialog.handle_input(CTRL_X);
    for _ in 0..20 {
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(2)).await;
        if rendered(&dialog).contains("Copied URL to clipboard") {
            break;
        }
    }
    assert!(rendered(&dialog).contains("Copied URL to clipboard"));
    assert_eq!(&*copied.lock().unwrap_or_else(|e| e.into_inner()), &[url]);
}

#[tokio::test]
#[serial]
async fn login_dialog_ignores_copy_key_without_auth_url() {
    let (_guard, ui) = setup();
    let copied = Arc::new(Mutex::new(Vec::<String>::new()));
    let copied_c = Arc::clone(&copied);
    let _browser = testing::override_open_browser(|_| {});
    let _clip = testing::override_copy_to_clipboard(move |text| {
        let copied_c = Arc::clone(&copied_c);
        let text = text.to_string();
        Box::pin(async move {
            copied_c.lock().unwrap_or_else(|e| e.into_inner()).push(text);
            Ok(())
        })
    });

    let dialog = LoginDialogComponent::new(ui, "test", Arc::new(|_, _| {}), None, None);
    dialog.show_device_code(DeviceCodeInfo {
        user_code: "ABCD".to_string(),
        verification_uri: "https://example.invalid/device".to_string(),
        ..Default::default()
    });
    dialog.handle_input(CTRL_X);
    tokio::task::yield_now().await;
    assert!(copied.lock().unwrap_or_else(|e| e.into_inner()).is_empty());
}

#[tokio::test]
#[serial]
async fn mcp_sign_in_screen_copies_authorization_url() {
    let (_guard, _) = setup();
    let ui: Arc<dyn TUI> = Arc::new(TuiMainScreen::new(Arc::new(FakeTerminal), None, None));
    let copied = Arc::new(Mutex::new(Vec::<String>::new()));
    let copied_c = Arc::clone(&copied);
    let _clip = testing::override_copy_to_clipboard(move |text| {
        let copied_c = Arc::clone(&copied_c);
        let text = text.to_string();
        Box::pin(async move {
            copied_c.lock().unwrap_or_else(|e| e.into_inner()).push(text);
            Ok(())
        })
    });

    let kb = TuiKeybindingsManager::new(KEYBINDINGS.clone(), None);
    let view = McpManagerView::new(ui, pi_coding_agent::core::extensions::types::Theme::default(), kb);
    let controller = AbortController::new();
    let view_c = view.clone();
    let url = long_url();
    let url_c = url.clone();
    let signal = controller.signal();
    let handle = tokio::spawn(async move { view_c.redirect_url("Sign in to issues", &url_c, signal).await });
    tokio::task::yield_now().await;

    assert!(rendered(&view).contains("ctrl+x to copy"));
    view.handle_input(CTRL_X);
    for _ in 0..20 {
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(2)).await;
        if rendered(&view).contains("Copied URL to clipboard") {
            break;
        }
    }
    assert!(rendered(&view).contains("Copied URL to clipboard"));
    assert_eq!(&*copied.lock().unwrap_or_else(|e| e.into_inner()), &[url]);

    controller.abort(None);
    let _ = handle.await;
}
