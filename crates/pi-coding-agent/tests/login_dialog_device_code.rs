//! Port of packages/coding-agent/test/login-dialog-device-code.test.ts

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_ai::AuthInfoLink;
use pi_coding_agent::core::keybindings::{KEYBINDINGS, KeybindingsManager};
use pi_coding_agent::modes::interactive::components::login_dialog::{DeviceCodeInfo, LoginDialogComponent, testing};
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_coding_agent::utils::ansi::strip_ansi;
use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
use pi_tui::{KeybindingsManager as TuiKeybindingsManager, TUI, TUI_KEYBINDINGS, TuiMainScreen, set_keybindings};
use serial_test::serial;

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

fn setup() -> (
    TestGuard,
    LoginDialogComponent,
    Arc<Mutex<Vec<String>>>,
    Arc<Mutex<Vec<String>>>,
    Arc<Mutex<Vec<(bool, Option<String>)>>>,
    pi_js::seam::Guard,
    pi_js::seam::Guard,
) {
    init_theme(Some("dark"), Some(false));
    let kb = KeybindingsManager::new(None, None);
    set_keybindings(TuiKeybindingsManager::new(
        KEYBINDINGS.clone(),
        Some(kb.get_user_bindings()),
    ));
    let ui: Arc<dyn TUI + Send + Sync> = Arc::new(TuiMainScreen::new(Arc::new(FakeTerminal), None, None));
    let opened = Arc::new(Mutex::new(Vec::<String>::new()));
    let copied = Arc::new(Mutex::new(Vec::<String>::new()));
    let completed = Arc::new(Mutex::new(Vec::<(bool, Option<String>)>::new()));

    let opened_c = Arc::clone(&opened);
    let browser_guard = testing::override_open_browser(move |url| {
        opened_c.lock().unwrap_or_else(|e| e.into_inner()).push(url.to_string());
    });
    let copied_c = Arc::clone(&copied);
    let clip_guard = testing::override_copy_to_clipboard(move |text| {
        let copied_c = Arc::clone(&copied_c);
        let text = text.to_string();
        Box::pin(async move {
            copied_c.lock().unwrap_or_else(|e| e.into_inner()).push(text);
            Ok(())
        })
    });

    let completed_c = Arc::clone(&completed);
    let dialog = LoginDialogComponent::new(
        ui,
        "custom-provider",
        Arc::new(move |ok, err| {
            completed_c
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((ok, err.map(str::to_string)));
        }),
        None,
        None,
    );
    dialog.set_focused(true);

    (TestGuard, dialog, opened, copied, completed, browser_guard, clip_guard)
}

fn render(dialog: &LoginDialogComponent) -> String {
    strip_ansi(&dialog.render(120).join("\n"))
}

fn device() -> DeviceCodeInfo {
    DeviceCodeInfo {
        user_code: "ABCD-EFGH".to_string(),
        verification_uri: "https://example.com/device".to_string(),
        ..Default::default()
    }
}

#[tokio::test]
#[serial]
async fn clears_auth_url_copy_action_when_showing_device_code() {
    let (_guard, dialog, opened, copied, _completed, _bg, _cg) = setup();
    dialog.show_auth("https://example.com/authorize", None);
    opened.lock().unwrap_or_else(|e| e.into_inner()).clear();
    dialog.show_device_code(device());
    dialog.handle_input("\x18");
    tokio::task::yield_now().await;
    assert!(copied.lock().unwrap_or_else(|e| e.into_inner()).is_empty());
    assert!(opened.lock().unwrap_or_else(|e| e.into_inner()).is_empty());

    dialog.handle_input("\r");
    tokio::task::yield_now().await;
    assert_eq!(&*copied.lock().unwrap_or_else(|e| e.into_inner()), &["ABCD-EFGH"]);
}

#[tokio::test]
#[serial]
async fn clears_device_actions_when_switching_to_browser_authentication() {
    let (_guard, dialog, opened, copied, _completed, _bg, _cg) = setup();
    dialog.show_device_code(device());
    dialog.show_auth("https://example.com/authorize", None);
    opened.lock().unwrap_or_else(|e| e.into_inner()).clear();

    dialog.handle_input("\r");
    tokio::task::yield_now().await;
    assert!(copied.lock().unwrap_or_else(|e| e.into_inner()).is_empty());
    assert!(opened.lock().unwrap_or_else(|e| e.into_inner()).is_empty());

    dialog.handle_input("\x18");
    tokio::task::yield_now().await;
    assert_eq!(
        &*copied.lock().unwrap_or_else(|e| e.into_inner()),
        &["https://example.com/authorize"]
    );
}

#[tokio::test]
#[serial]
async fn clears_both_action_types_when_showing_details() {
    let (_guard, dialog, opened, copied, _completed, _bg, _cg) = setup();
    dialog.show_auth("https://example.com/authorize", None);
    dialog.show_details(&["Choose a login method".to_string()]);
    dialog.handle_input("\x18");
    tokio::task::yield_now().await;
    assert!(copied.lock().unwrap_or_else(|e| e.into_inner()).is_empty());

    dialog.show_device_code(device());
    dialog.show_details(&["Choose a login method".to_string()]);
    opened.lock().unwrap_or_else(|e| e.into_inner()).clear();
    dialog.handle_input("\r");
    tokio::task::yield_now().await;
    assert!(copied.lock().unwrap_or_else(|e| e.into_inner()).is_empty());
    assert!(opened.lock().unwrap_or_else(|e| e.into_inner()).is_empty());
}

#[tokio::test]
#[serial]
async fn cancels_without_opening_browser_or_changing_clipboard() {
    let (_guard, dialog, opened, copied, completed, _bg, _cg) = setup();
    dialog.show_device_code(device());
    dialog.handle_input("\x1b");
    dialog.handle_input("\r");
    tokio::task::yield_now().await;
    assert!(dialog.signal().aborted());
    assert_eq!(
        *completed.lock().unwrap_or_else(|e| e.into_inner()),
        vec![(false, Some("Login cancelled".to_string()))]
    );
    assert!(opened.lock().unwrap_or_else(|e| e.into_inner()).is_empty());
    assert!(copied.lock().unwrap_or_else(|e| e.into_inner()).is_empty());
}

#[tokio::test]
#[serial]
async fn login_urls_only_open_http_and_sanitize_control_characters() {
    let (_guard, dialog, opened, _copied, _completed, _bg, _cg) = setup();

    dialog.show_auth("file:///Users/victim/Downloads/payload.app", None);
    assert!(opened.lock().unwrap_or_else(|e| e.into_inner()).is_empty());

    dialog.show_auth("https://example.com/authorize", None);
    assert_eq!(
        &*opened.lock().unwrap_or_else(|e| e.into_inner()),
        &["https://example.com/authorize"]
    );
    opened.lock().unwrap_or_else(|e| e.into_inner()).clear();

    let injected_web = "https://example.com/device\x07\x1b]52;c;Zm9v\x07";
    let injected_help = "https://example.com/help\x07\x1b]52;c;Zm9v\x07";
    dialog.show_device_code(DeviceCodeInfo {
        verification_uri: injected_web.to_string(),
        ..device()
    });
    dialog.show_info(
        "Docs",
        Some(&[AuthInfoLink {
            label: Some("Help".to_string()),
            url: injected_help.to_string(),
        }]),
        None,
    );
    let raw = dialog.render(120).join("\n");
    assert!(!raw.contains("\x1b]52"));
    assert!(render(&dialog).contains("https://example.com/device%07%1B]52;c;Zm9v"));
    assert!(render(&dialog).contains("Help: https://example.com/help%07%1B]52;c;Zm9v"));

    let injected = "x\x07\x1b]52;c;Zm9v\x07";
    dialog.show_auth(&format!("file:///tmp/{injected}"), None);
    assert!(!dialog.render(120).join("\n").contains("\x1b]52"));

    dialog.show_device_code(DeviceCodeInfo {
        verification_uri: injected.to_string(),
        ..device()
    });
    assert!(!dialog.render(120).join("\n").contains("\x1b]52"));
    assert!(render(&dialog).contains("x ]52;c;Zm9v"));

    dialog.show_info(
        "Docs",
        Some(&[AuthInfoLink {
            label: Some("Help".to_string()),
            url: injected.to_string(),
        }]),
        None,
    );
    assert!(!dialog.render(120).join("\n").contains("\x1b]52"));
    assert!(render(&dialog).contains("Help: x ]52;c;Zm9v"));
    assert!(opened.lock().unwrap_or_else(|e| e.into_inner()).is_empty());
}
