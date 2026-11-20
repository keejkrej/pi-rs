//! Port of packages/tui/test/keybindings.test.ts

use pi_tui::keybindings::{KeyIdOrList, KeybindingsConfig, KeybindingsManager, TUI_KEYBINDINGS};

#[test]
fn binds_ctrl_j_as_a_default_newline_alias() {
    let keybindings = KeybindingsManager::new((&*TUI_KEYBINDINGS).clone(), None);

    assert_eq!(keybindings.get_keys("tui.input.newLine"), vec!["shift+enter", "ctrl+j"]);
    assert!(keybindings.matches("\n", "tui.input.newLine"));
    assert!(keybindings.matches("\x1b[106;5u", "tui.input.newLine"));
}

#[test]
fn binds_modified_and_unmodified_editor_viewport_navigation() {
    let keybindings = KeybindingsManager::new((&*TUI_KEYBINDINGS).clone(), None);

    assert_eq!(
        keybindings.get_keys("tui.editor.cursorLineStart"),
        vec!["home", "ctrl+a"]
    );
    assert_eq!(keybindings.get_keys("tui.editor.cursorLineEnd"), vec!["end", "ctrl+e"]);
    assert_eq!(keybindings.get_keys("tui.editor.pageUp"), vec!["pageUp", "ctrl+pageUp"]);
    assert_eq!(
        keybindings.get_keys("tui.editor.pageDown"),
        vec!["pageDown", "ctrl+pageDown"]
    );
}

#[test]
fn leaves_dedicated_prompt_history_navigation_unbound_by_default() {
    let keybindings = KeybindingsManager::new((&*TUI_KEYBINDINGS).clone(), None);

    assert_eq!(keybindings.get_keys("tui.editor.historyPrevious"), Vec::<String>::new());
    assert_eq!(keybindings.get_keys("tui.editor.historyNext"), Vec::<String>::new());
}

#[test]
fn binds_unmodified_terminal_viewport_shortcuts_to_alternate_screen_navigation() {
    let keybindings = KeybindingsManager::new((&*TUI_KEYBINDINGS).clone(), None);

    assert_eq!(keybindings.get_keys("tui.altScreen.pageUp"), vec!["pageUp"]);
    assert_eq!(keybindings.get_keys("tui.altScreen.pageDown"), vec!["pageDown"]);
    assert_eq!(keybindings.get_keys("tui.altScreen.halfPageUp"), Vec::<String>::new());
    assert_eq!(keybindings.get_keys("tui.altScreen.halfPageDown"), Vec::<String>::new());
    assert_eq!(keybindings.get_keys("tui.altScreen.lineUp"), Vec::<String>::new());
    assert_eq!(keybindings.get_keys("tui.altScreen.lineDown"), Vec::<String>::new());
    assert_eq!(
        keybindings.get_keys("tui.altScreen.previousPrompt"),
        vec!["ctrl+shift+up", "ctrl+up"]
    );
    assert_eq!(
        keybindings.get_keys("tui.altScreen.nextPrompt"),
        vec!["ctrl+shift+down", "ctrl+down"]
    );
    assert_eq!(keybindings.get_keys("tui.altScreen.search"), vec!["ctrl+shift+f"]);
    assert_eq!(
        keybindings.get_keys("tui.altScreen.searchNext"),
        vec!["enter", "ctrl+g"]
    );
    assert_eq!(
        keybindings.get_keys("tui.altScreen.searchPrevious"),
        vec!["shift+enter", "ctrl+shift+g"]
    );
    assert_eq!(keybindings.get_keys("tui.altScreen.searchClose"), vec!["escape"]);
    assert_eq!(keybindings.get_keys("tui.altScreen.top"), vec!["ctrl+home"]);
    assert_eq!(keybindings.get_keys("tui.altScreen.bottom"), vec!["ctrl+end"]);
}

#[test]
fn computes_clamped_select_page_indices() {
    use pi_tui::get_select_page_index;
    let keybindings = KeybindingsManager::new((&*TUI_KEYBINDINGS).clone(), None);

    assert_eq!(get_select_page_index(&keybindings, "a", 0, 12, 5), None);
    assert_eq!(get_select_page_index(&keybindings, "\x1b[6~", 0, 12, 5), Some(5));
    assert_eq!(get_select_page_index(&keybindings, "\x1b[6~", 10, 12, 5), Some(11));
    assert_eq!(get_select_page_index(&keybindings, "\x1b[5~", 2, 12, 5), Some(0));
    assert_eq!(get_select_page_index(&keybindings, "\x1b[6~", 0, 0, 5), Some(0));
}

#[test]
fn does_not_evict_selector_confirm_when_input_submit_is_rebound() {
    let mut user_bindings = KeybindingsConfig::new();
    user_bindings.insert(
        "tui.input.submit".into(),
        Some(KeyIdOrList::Many(vec!["enter".into(), "ctrl+enter".into()])),
    );
    let keybindings = KeybindingsManager::new((&*TUI_KEYBINDINGS).clone(), Some(user_bindings));

    assert_eq!(keybindings.get_keys("tui.input.submit"), vec!["enter", "ctrl+enter"]);
    assert_eq!(keybindings.get_keys("tui.select.confirm"), vec!["enter"]);
}

#[test]
fn does_not_evict_cursor_bindings_when_another_action_reuses_the_same_key() {
    let mut user_bindings = KeybindingsConfig::new();
    user_bindings.insert(
        "tui.select.up".into(),
        Some(KeyIdOrList::Many(vec!["up".into(), "ctrl+p".into()])),
    );
    let keybindings = KeybindingsManager::new((&*TUI_KEYBINDINGS).clone(), Some(user_bindings));

    assert_eq!(keybindings.get_keys("tui.select.up"), vec!["up", "ctrl+p"]);
    assert_eq!(keybindings.get_keys("tui.editor.cursorUp"), vec!["up"]);
}

#[test]
fn still_reports_direct_user_binding_conflicts_without_evicting_defaults() {
    let mut user_bindings = KeybindingsConfig::new();
    user_bindings.insert("tui.input.submit".into(), Some(KeyIdOrList::One("ctrl+x".into())));
    user_bindings.insert("tui.select.confirm".into(), Some(KeyIdOrList::One("ctrl+x".into())));
    let keybindings = KeybindingsManager::new((&*TUI_KEYBINDINGS).clone(), Some(user_bindings));

    let conflicts = keybindings.get_conflicts();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].key, "ctrl+x");
    assert_eq!(conflicts[0].keybindings, vec!["tui.input.submit", "tui.select.confirm"]);
    assert_eq!(keybindings.get_keys("tui.editor.cursorLeft"), vec!["left", "ctrl+b"]);
}
