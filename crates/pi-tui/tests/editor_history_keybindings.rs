//! Port of packages/tui/test/editor-history-keybindings.test.ts

mod support;

use std::sync::Arc;

use pi_tui::components::editor::EditorCursor;
use pi_tui::keybindings::KeyIdOrList;
use pi_tui::{Editor, KeybindingsConfig, KeybindingsManager, TUI, TUI_KEYBINDINGS, TuiMainScreen, set_keybindings};
use support::test_themes::default_editor_theme;
use support::virtual_terminal::VirtualTerminal;

struct RestoreKeybindingsGuard;

impl Drop for RestoreKeybindingsGuard {
    fn drop(&mut self) {
        set_keybindings(KeybindingsManager::new((&*TUI_KEYBINDINGS).clone(), None));
    }
}

fn create_test_tui() -> Arc<dyn TUI + Send + Sync> {
    let terminal = Arc::new(VirtualTerminal::new(80, 24));
    Arc::new(TuiMainScreen::new(terminal, None, None))
}

#[test]
#[serial_test::serial]
fn browses_history_directly_without_first_moving_the_cursor() {
    let _restore = RestoreKeybindingsGuard;
    let mut user_bindings = KeybindingsConfig::new();
    user_bindings.insert(
        "tui.editor.historyPrevious".to_string(),
        Some(KeyIdOrList::One("ctrl+p".to_string())),
    );
    user_bindings.insert(
        "tui.editor.historyNext".to_string(),
        Some(KeyIdOrList::One("ctrl+n".to_string())),
    );
    set_keybindings(KeybindingsManager::new(
        (&*TUI_KEYBINDINGS).clone(),
        Some(user_bindings),
    ));

    let editor = Editor::new(create_test_tui(), default_editor_theme(), None);
    editor.add_to_history("older prompt");
    editor.add_to_history("newer\nmultiline prompt");
    editor.set_text("draft");
    editor.handle_input("\x1b[D");
    editor.handle_input("\x1b[D");

    editor.handle_input("\x10"); // Ctrl+P
    assert_eq!(editor.get_text(), "newer\nmultiline prompt");
    assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 0 });

    editor.handle_input("\x10"); // Ctrl+P
    assert_eq!(editor.get_text(), "older prompt");

    editor.handle_input("\x0e"); // Ctrl+N
    assert_eq!(editor.get_text(), "newer\nmultiline prompt");
    assert_eq!(editor.get_cursor(), EditorCursor { line: 1, col: 16 });

    editor.handle_input("\x0e"); // Ctrl+N
    assert_eq!(editor.get_text(), "draft");
    assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 3 });
}
