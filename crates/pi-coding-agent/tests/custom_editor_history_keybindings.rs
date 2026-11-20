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

#[tokio::test]
#[serial]
async fn pages_an_open_autocomplete_menu_before_pasting_an_image_with_same_key_7629() {
    use async_trait::async_trait;
    use pi_tui::autocomplete::{ApplyCompletionResult, GetSuggestionsOptions};
    use pi_tui::{AutocompleteItem, AutocompleteProvider, AutocompleteSuggestions};

    struct TestAutocomplete;

    #[async_trait]
    impl AutocompleteProvider for TestAutocomplete {
        async fn get_suggestions(
            &self,
            _lines: &[String],
            _cursor_line: i64,
            _cursor_col: i64,
            _options: GetSuggestionsOptions,
        ) -> Option<AutocompleteSuggestions> {
            Some(AutocompleteSuggestions {
                items: (0..12)
                    .map(|index| AutocompleteItem {
                        value: format!("/command-{index}"),
                        label: format!("command-{index}"),
                        description: None,
                    })
                    .collect(),
                prefix: "/".to_owned(),
            })
        }

        fn apply_completion(
            &self,
            lines: &[String],
            cursor_line: i64,
            _cursor_col: i64,
            item: &AutocompleteItem,
            _prefix: &str,
        ) -> ApplyCompletionResult {
            let new_lines: Vec<String> = lines
                .iter()
                .enumerate()
                .map(|(idx, line)| {
                    if idx as i64 == cursor_line {
                        item.value.clone()
                    } else {
                        line.clone()
                    }
                })
                .collect();
            ApplyCompletionResult {
                lines: new_lines,
                cursor_line,
                cursor_col: item.value.len() as i64,
            }
        }
    }

    let mut user = KeybindingsConfig::new();
    user.insert(
        "tui.select.pageDown".to_string(),
        Some(KeyIdOrList::One("ctrl+v".to_string())),
    );
    user.insert(
        "app.clipboard.pasteImage".to_string(),
        Some(KeyIdOrList::One("ctrl+v".to_string())),
    );
    let keybindings = AppKeybindingsManager::new(Some(user.clone()), None);
    set_keybindings(KeybindingsManager::new(KEYBINDINGS.clone(), Some(user)));

    let tui = Arc::new(TuiMainScreen::new(Arc::new(NoopTerminal), None, None));
    let editor = CustomEditor::new(tui, get_editor_theme(), keybindings, None);
    let pastes = Arc::new(AtomicUsize::new(0));
    let pastes_c = Arc::clone(&pastes);
    editor.set_on_paste_image(Some(Arc::new(move || {
        pastes_c.fetch_add(1, Ordering::SeqCst);
    })));
    let submitted = Arc::new(std::sync::Mutex::new(String::new()));
    let submitted_c = Arc::clone(&submitted);
    editor.set_on_submit(Some(Arc::new(move |text| {
        *submitted_c.lock().unwrap() = text.to_owned();
    })));
    editor.set_autocomplete_provider(Arc::new(TestAutocomplete));

    editor.handle_input("/");
    for _ in 0..50 {
        if editor.is_showing_autocomplete() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(editor.is_showing_autocomplete(), "autocomplete menu should open");

    editor.handle_input("\x16"); // Ctrl+V pages autocomplete instead of pasting image
    editor.handle_input("\r");
    assert_eq!(pastes.load(Ordering::SeqCst), 0);
    assert_eq!(&*submitted.lock().unwrap(), "/command-5");

    editor.handle_input("\x16"); // Ctrl+V with autocomplete closed triggers pasteImage
    assert_eq!(pastes.load(Ordering::SeqCst), 1);
}
