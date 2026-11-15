//! Port of packages/tui/test/settings-list.test.ts

use std::sync::{Arc, Mutex};

use pi_tui::components::settings_list::{SettingsLabelFn, SettingsListOptions, SettingsTextFn};
use pi_tui::{SettingItem, SettingsList, SettingsListTheme};

fn test_theme() -> SettingsListTheme {
    let label_fn: SettingsLabelFn = Arc::new(|text, _selected| text.to_string());
    let text_fn: SettingsTextFn = Arc::new(|text| text.to_string());
    SettingsListTheme {
        label: Arc::clone(&label_fn),
        value: label_fn,
        description: Arc::clone(&text_fn),
        cursor: "> ".to_string(),
        hint: text_fn,
    }
}

fn base_items() -> Vec<SettingItem> {
    vec![SettingItem {
        id: "tui-mode".to_string(),
        label: "TUI mode".to_string(),
        description: None,
        current_value: "regular".to_string(),
        values: Some(vec!["regular".to_string(), "fullscreen".to_string()]),
        submenu: None,
    }]
}

#[test]
fn includes_spaces_in_an_active_search_instead_of_changing_the_selected_setting() {
    let changes: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let changes_clone = Arc::clone(&changes);
    let list = SettingsList::new(
        base_items(),
        10,
        test_theme(),
        Arc::new(move |id, value| changes_clone.lock().unwrap().push((id.to_string(), value.to_string()))),
        Arc::new(|| {}),
        Some(SettingsListOptions {
            enable_search: Some(true),
        }),
    );

    for ch in "TUI mode".chars() {
        let mut buf = [0_u8; 4];
        list.handle_input(ch.encode_utf8(&mut buf));
    }

    assert!(changes.lock().unwrap().is_empty());
    let rendered = list.render(80);
    assert!(rendered.first().is_some_and(|line| line.contains("TUI mode")));

    list.handle_input("\r");
    assert_eq!(
        *changes.lock().unwrap(),
        vec![("tui-mode".to_string(), "fullscreen".to_string())]
    );
}

#[test]
fn keeps_space_as_a_change_shortcut_before_a_search_query_is_entered() {
    let changes: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let changes_clone = Arc::clone(&changes);
    let list = SettingsList::new(
        base_items(),
        10,
        test_theme(),
        Arc::new(move |id, value| changes_clone.lock().unwrap().push((id.to_string(), value.to_string()))),
        Arc::new(|| {}),
        Some(SettingsListOptions {
            enable_search: Some(true),
        }),
    );

    list.handle_input(" ");

    assert_eq!(
        *changes.lock().unwrap(),
        vec![("tui-mode".to_string(), "fullscreen".to_string())]
    );
}

#[test]
fn moves_by_one_visible_page_with_selection_page_keys() {
    let items: Vec<SettingItem> = (0..12)
        .map(|index| SettingItem {
            id: format!("setting-{index}"),
            label: format!("Setting {index}"),
            description: None,
            current_value: index.to_string(),
            values: None,
            submenu: None,
        })
        .collect();

    let list = SettingsList::new(items, 5, test_theme(), Arc::new(|_, _| {}), Arc::new(|| {}), None);

    list.handle_input("\x1b[6~");
    assert!(list.render(80).join("\n").contains("> Setting 5"));
    list.handle_input("\x1b[6~");
    list.handle_input("\x1b[6~");
    assert!(list.render(80).join("\n").contains("> Setting 11"));
    list.handle_input("\x1b[5~");
    assert!(list.render(80).join("\n").contains("> Setting 6"));
}
