//! Port of packages/coding-agent/test/custom-editor-history-keybindings.test.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use pi_coding_agent::core::keybindings::{KEYBINDINGS, KeybindingsManager as AppKeybindingsManager};
use pi_coding_agent::modes::interactive::components::custom_editor::CustomEditor;
use pi_coding_agent::modes::interactive::theme::theme::{get_editor_theme, init_theme};
use pi_tui::keybindings::KeyIdOrList;
use pi_tui::terminal::{TerminalInputHandler, TerminalResizeHandler};
use pi_tui::{KeybindingsConfig, KeybindingsManager, TUI_KEYBINDINGS, Terminal, TuiMainScreen, set_keybindings};
use serial_test::serial;

struct TestGuard;

impl Drop for TestGuard {
    fn drop(&mut self) {
        init_theme(Some("dark"), None);
        set_keybindings(KeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
    }
}

struct NoopTerminal;

#[async_trait]
impl Terminal for NoopTerminal {
    fn start(&self, _on_input: TerminalInputHandler, _on_resize: TerminalResizeHandler) {}
    fn stop(&self) {}
    async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}
    fn write(&self, _data: &str) {}
    fn columns(&self) -> i64 {
        80
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

#[test]
#[serial]
fn gives_an_explicit_history_binding_precedence_over_model_cycling() {
    let _g = TestGuard;
    init_theme(Some("dark"), None);

    let mut user = KeybindingsConfig::new();
    user.insert(
        "tui.editor.historyPrevious".to_string(),
        Some(KeyIdOrList::One("ctrl+p".to_string())),
    );
    user.insert(
        "tui.editor.historyNext".to_string(),
        Some(KeyIdOrList::One("ctrl+n".to_string())),
    );
    let keybindings = AppKeybindingsManager::new(Some(user.clone()), None);
    set_keybindings(KeybindingsManager::new(KEYBINDINGS.clone(), Some(user)));

    let tui = Arc::new(TuiMainScreen::new(Arc::new(NoopTerminal), None, None));
    let editor = CustomEditor::new(tui, get_editor_theme(), keybindings, None);
    let model_cycles = Arc::new(AtomicUsize::new(0));
    let mc_c = Arc::clone(&model_cycles);
    editor.on_action(
        "app.model.cycleForward",
        Arc::new(move || {
            mc_c.fetch_add(1, Ordering::SeqCst);
        }),
    );

    editor.add_to_history("previous prompt");
    editor.set_text("draft");

    editor.handle_input("\x10"); // Ctrl+P
    assert_eq!(editor.get_text(), "previous prompt");
    assert_eq!(model_cycles.load(Ordering::SeqCst), 0);

    editor.handle_input("\x0e"); // Ctrl+N
    assert_eq!(editor.get_text(), "draft");
}
