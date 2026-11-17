//! Port of packages/tui/test/mouse-components.test.ts

mod support;

use std::sync::{Arc, Mutex};

use pi_tui::components::editor::{Editor, EditorTheme};
use pi_tui::components::input::Input;
use pi_tui::components::select_list::{SelectItem, SelectList, SelectListTheme};
use pi_tui::components::settings_list::{SettingItem, SettingsList, SettingsListOptions, SettingsListTheme};
use pi_tui::tui::{
    Component, Focusable, OverlayOptions, SizeValue, TUI, TuiMouseButton, TuiMouseEvent, TuiMouseEventResult,
    TuiMouseEventType,
};
use pi_tui::tui_alt_screen::{CopySelectionResult, TuiAltScreen, TuiAltScreenOptions};
use pi_tui::utils::strip_terminal_sequences;

use support::virtual_terminal::VirtualTerminal;

fn select_theme() -> SelectListTheme {
    let identity: Arc<dyn Fn(&str) -> String + Send + Sync> = Arc::new(|s: &str| s.to_string());
    SelectListTheme {
        selected_prefix: Arc::clone(&identity),
        selected_text: Arc::clone(&identity),
        description: Arc::clone(&identity),
        scroll_info: Arc::clone(&identity),
        no_match: identity,
    }
}

fn settings_theme() -> SettingsListTheme {
    let identity: Arc<dyn Fn(&str) -> String + Send + Sync> = Arc::new(|s: &str| s.to_string());
    let label_identity: Arc<dyn Fn(&str, bool) -> String + Send + Sync> =
        Arc::new(|s: &str, _selected: bool| s.to_string());
    SettingsListTheme {
        label: Arc::clone(&label_identity),
        value: label_identity,
        description: Arc::clone(&identity),
        cursor: "> ".to_string(),
        hint: identity,
    }
}

fn editor_theme() -> EditorTheme {
    EditorTheme {
        border_color: Arc::new(|s: &str| s.to_string()),
        select_list: select_theme(),
    }
}

fn mouse_event(
    event_type: TuiMouseEventType,
    x: i64,
    y: i64,
    width: i64,
    height: i64,
    button: TuiMouseButton,
    wheel_delta: Option<i64>,
) -> TuiMouseEvent {
    TuiMouseEvent {
        r#type: event_type,
        button,
        x,
        y,
        screen_x: x,
        screen_y: y,
        width,
        height,
        shift: false,
        alt: false,
        ctrl: false,
        wheel_delta,
        click_count: None,
    }
}

fn press_event(x: i64, y: i64, width: i64, height: i64) -> TuiMouseEvent {
    mouse_event(
        TuiMouseEventType::Press,
        x,
        y,
        width,
        height,
        TuiMouseButton::Left,
        None,
    )
}

fn click_event(x: i64, y: i64, width: i64, height: i64) -> TuiMouseEvent {
    mouse_event(
        TuiMouseEventType::Click,
        x,
        y,
        width,
        height,
        TuiMouseButton::Left,
        None,
    )
}

fn move_event(x: i64, y: i64, width: i64, height: i64) -> TuiMouseEvent {
    mouse_event(TuiMouseEventType::Move, x, y, width, height, TuiMouseButton::None, None)
}

fn wheel_event(x: i64, y: i64, width: i64, height: i64, wheel_delta: i64) -> TuiMouseEvent {
    mouse_event(
        TuiMouseEventType::Wheel,
        x,
        y,
        width,
        height,
        TuiMouseButton::None,
        Some(wheel_delta),
    )
}

struct InputOverlay {
    input: Input,
    focused: Mutex<bool>,
}

impl InputOverlay {
    fn new() -> Self {
        Self {
            input: Input::new(None),
            focused: Mutex::new(false),
        }
    }
}

impl Focusable for InputOverlay {
    fn focused(&self) -> bool {
        *self.focused.lock().unwrap()
    }

    fn set_focused(&self, focused: bool) {
        *self.focused.lock().unwrap() = focused;
        self.input.set_focused(focused);
    }
}

impl Component for InputOverlay {
    fn render(&self, width: usize) -> Vec<String> {
        self.input.render(width)
    }

    fn handle_input(&self, data: &str) {
        self.input.handle_input(data);
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.input.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        self.input.invalidate();
    }

    fn has_handle_input(&self) -> bool {
        true
    }

    fn as_focusable(&self) -> Option<&dyn Focusable> {
        Some(self)
    }
}

struct SubmenuHost {
    list: SelectList,
}

impl Component for SubmenuHost {
    fn render(&self, width: usize) -> Vec<String> {
        self.list.render(width)
    }

    fn handle_input(&self, data: &str) {
        self.list.handle_input(data);
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.list.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        self.list.invalidate();
    }

    fn has_handle_input(&self) -> bool {
        true
    }
}

#[test]
fn positions_a_single_line_input_cursor_on_press() {
    let input = Input::new(None);
    input.set_value("hello");
    input.render(20);

    let result = input.handle_mouse(&press_event(4, 0, 20, 1));
    assert_eq!(result.as_ref().and_then(|r| r.handled), Some(true));
    assert_eq!(result.as_ref().and_then(|r| r.focus), Some(true));

    input.handle_input("X");
    assert_eq!(input.get_value(), "heXllo");
}

#[test]
fn selects_and_activates_list_rows() {
    let items: Vec<SelectItem> = ["a", "b", "c", "d", "e"]
        .into_iter()
        .map(|value| SelectItem {
            value: value.to_string(),
            label: value.to_string(),
            description: None,
        })
        .collect();
    let list = SelectList::new(items, 3, select_theme(), None);
    let selected = Arc::new(Mutex::new(Vec::<String>::new()));
    let selected_clone = Arc::clone(&selected);
    list.set_on_select(Some(Arc::new(move |item: &SelectItem| {
        selected_clone.lock().unwrap().push(item.value.clone());
    })));

    assert_eq!(
        list.handle_mouse(&press_event(1, 2, 40, 3)).and_then(|r| r.handled),
        Some(true)
    );
    assert_eq!(list.get_selected_item().map(|i| i.value).as_deref(), Some("c"));

    assert_eq!(
        list.handle_mouse(&click_event(1, 2, 40, 3)).and_then(|r| r.handled),
        Some(true)
    );
    assert_eq!(*selected.lock().unwrap(), vec!["c".to_string()]);
}

#[test]
fn activates_settings_rows() {
    let changes = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let changes_clone = Arc::clone(&changes);
    let items = vec![
        SettingItem {
            id: "mode".to_string(),
            label: "Mode".to_string(),
            description: None,
            current_value: "fast".to_string(),
            values: Some(vec!["fast".to_string(), "slow".to_string()]),
            submenu: None,
        },
        SettingItem {
            id: "other".to_string(),
            label: "Other".to_string(),
            description: None,
            current_value: "off".to_string(),
            values: Some(vec!["off".to_string(), "on".to_string()]),
            submenu: None,
        },
        SettingItem {
            id: "third".to_string(),
            label: "Third".to_string(),
            description: None,
            current_value: "low".to_string(),
            values: Some(vec!["low".to_string(), "high".to_string()]),
            submenu: None,
        },
        SettingItem {
            id: "fourth".to_string(),
            label: "Fourth".to_string(),
            description: None,
            current_value: "no".to_string(),
            values: Some(vec!["no".to_string(), "yes".to_string()]),
            submenu: None,
        },
    ];
    let list = SettingsList::new(
        items,
        3,
        settings_theme(),
        Arc::new(move |id, value| {
            changes_clone.lock().unwrap().push((id.to_string(), value.to_string()));
        }),
        Arc::new(|| {}),
        None,
    );

    assert_eq!(
        list.handle_mouse(&press_event(1, 2, 40, 5)).and_then(|r| r.handled),
        Some(true)
    );
    assert_eq!(
        list.handle_mouse(&click_event(1, 2, 40, 5)).and_then(|r| r.handled),
        Some(true)
    );
    assert_eq!(
        *changes.lock().unwrap(),
        vec![("third".to_string(), "high".to_string())]
    );
}

#[test]
fn ignores_hover_and_clicks_visible_select_list_rows_after_scrolling() {
    for row in [0_i64, 4] {
        let items: Vec<SelectItem> = (0..12)
            .map(|i| SelectItem {
                value: format!("item-{i}"),
                label: format!("Item {i}"),
                description: None,
            })
            .collect();
        let list = SelectList::new(items, 5, select_theme(), None);
        let selected = Arc::new(Mutex::new(Vec::<String>::new()));
        let selected_clone = Arc::clone(&selected);
        list.set_on_select(Some(Arc::new(move |item: &SelectItem| {
            selected_clone.lock().unwrap().push(item.value.clone());
        })));
        list.set_selected_index(5);

        assert_eq!(
            list.handle_mouse(&wheel_event(1, row, 80, 6, 1))
                .and_then(|r| r.handled),
            Some(true)
        );
        assert_eq!(list.get_selected_item().map(|i| i.value).as_deref(), Some("item-6"));

        let before = list.render(80);
        for y in 0..5 {
            assert!(list.handle_mouse(&move_event(1, y, 80, 6)).is_none());
        }
        assert_eq!(list.render(80), before);

        assert_eq!(
            list.handle_mouse(&press_event(1, row, 80, 6)).and_then(|r| r.handled),
            Some(true)
        );
        list.render(80);
        assert_eq!(
            list.handle_mouse(&click_event(1, row, 80, 6)).and_then(|r| r.handled),
            Some(true)
        );
        assert_eq!(*selected.lock().unwrap(), vec![format!("item-{}", 4 + row)]);
    }
}

#[test]
fn ignores_hover_and_clicks_visible_settings_rows_after_scrolling() {
    for row in [0_i64, 4] {
        let changes = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
        let changes_clone = Arc::clone(&changes);
        let items: Vec<SettingItem> = (0..12)
            .map(|i| SettingItem {
                id: format!("item-{i}"),
                label: format!("Item {i}"),
                description: None,
                current_value: "off".to_string(),
                values: Some(vec!["off".to_string(), "on".to_string()]),
                submenu: None,
            })
            .collect();
        let list = SettingsList::new(
            items,
            5,
            settings_theme(),
            Arc::new(move |id, value| {
                changes_clone.lock().unwrap().push((id.to_string(), value.to_string()));
            }),
            Arc::new(|| {}),
            Some(SettingsListOptions {
                enable_search: Some(true),
            }),
        );
        list.select_item("item-5");

        assert_eq!(
            list.handle_mouse(&wheel_event(1, row + 2, 80, 8, 1))
                .and_then(|r| r.handled),
            Some(true)
        );

        let before = list.render(80);
        assert!(strip_terminal_sequences(&before[4]).starts_with("> Item 6"));
        assert!(strip_terminal_sequences(&before[(row + 2) as usize]).contains(&format!("Item {} ", 4 + row)));
        for y in 0..5 {
            assert!(list.handle_mouse(&move_event(1, y + 2, 80, 8)).is_none());
        }
        assert_eq!(list.render(80), before);

        assert_eq!(
            list.handle_mouse(&press_event(1, row + 2, 80, 8))
                .and_then(|r| r.handled),
            Some(true)
        );
        list.render(80);
        assert_eq!(
            list.handle_mouse(&click_event(1, row + 2, 80, 8))
                .and_then(|r| r.handled),
            Some(true)
        );
        assert_eq!(
            *changes.lock().unwrap(),
            vec![(format!("item-{}", 4 + row), "on".to_string())]
        );
    }
}

#[tokio::test]
async fn keeps_a_delegating_overlay_focused_when_its_nested_input_is_clicked() {
    let terminal = Arc::new(VirtualTerminal::new(20, 4));
    let tui = TuiAltScreen::new(terminal.clone(), None, None, None);
    let overlay = Arc::new(InputOverlay::new());
    overlay.input.set_value("hi");
    tui.show_overlay(
        overlay.clone(),
        Some(OverlayOptions {
            width: Some(SizeValue::Absolute(10.0)),
            row: Some(SizeValue::Absolute(0.0)),
            col: Some(SizeValue::Absolute(0.0)),
            ..Default::default()
        }),
    );
    tui.start();
    terminal.wait_for_render().await;

    terminal.send_input("\x1b[<0;5;1M");
    terminal.send_input("\x1b[<0;5;1m");
    terminal.send_input("!");
    terminal.wait_for_render().await;

    assert_eq!(overlay.input.get_value(), "hi!");
    assert!(
        tui.get_focused_component()
            .is_some_and(|c| Arc::ptr_eq(&c, &(overlay.clone() as Arc<dyn Component>)))
    );
    tui.stop(None);
}

#[tokio::test]
async fn keeps_a_settings_list_focused_when_a_click_in_its_submenu_closes_the_submenu() {
    let terminal = Arc::new(VirtualTerminal::new(30, 6));
    let tui = TuiAltScreen::new(terminal.clone(), None, None, None);
    let changes = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let changes_clone = Arc::clone(&changes);
    let items = vec![
        SettingItem {
            id: "theme".to_string(),
            label: "Theme".to_string(),
            description: None,
            current_value: "first".to_string(),
            values: None,
            submenu: Some(Arc::new(|_current, done| {
                let submenu_list = SelectList::new(
                    vec![
                        SelectItem {
                            value: "first".to_string(),
                            label: "first".to_string(),
                            description: None,
                        },
                        SelectItem {
                            value: "second".to_string(),
                            label: "second".to_string(),
                            description: None,
                        },
                    ],
                    3,
                    select_theme(),
                    None,
                );
                let done_select = done.clone();
                submenu_list.set_on_select(Some(Arc::new(move |item| {
                    done_select(Some(item.value.clone()), None);
                })));
                submenu_list.set_on_cancel(Some(Arc::new(move || {
                    done(None, None);
                })));
                Arc::new(SubmenuHost { list: submenu_list })
            })),
        },
        SettingItem {
            id: "other".to_string(),
            label: "Other".to_string(),
            description: None,
            current_value: "off".to_string(),
            values: Some(vec!["off".to_string(), "on".to_string()]),
            submenu: None,
        },
    ];
    let list = Arc::new(SettingsList::new(
        items,
        5,
        settings_theme(),
        Arc::new(move |id, value| {
            changes_clone.lock().unwrap().push((id.to_string(), value.to_string()));
        }),
        Arc::new(|| {}),
        None,
    ));
    tui.add_child(list.clone());
    tui.set_focus(Some(list.clone()));
    tui.start();
    terminal.wait_for_render().await;

    terminal.send_input("\r");
    terminal.wait_for_render().await;
    terminal.send_input("\x1b[<0;3;2M");
    terminal.send_input("\x1b[<0;3;2m");
    terminal.wait_for_render().await;

    assert_eq!(
        *changes.lock().unwrap(),
        vec![("theme".to_string(), "second".to_string())]
    );
    assert!(
        tui.get_focused_component()
            .is_some_and(|c| Arc::ptr_eq(&c, &(list.clone() as Arc<dyn Component>)))
    );

    terminal.send_input("\x1b[B");
    terminal.send_input("\r");
    assert_eq!(
        *changes.lock().unwrap(),
        vec![
            ("theme".to_string(), "second".to_string()),
            ("other".to_string(), "on".to_string()),
        ]
    );
    tui.stop(None);
}

#[tokio::test]
async fn positions_and_focuses_the_multiline_editor_through_alternate_screen_dispatch() {
    let terminal = Arc::new(VirtualTerminal::new(20, 6));
    let tui = TuiAltScreen::new(terminal.clone(), None, None, None);
    let tui_handle: Arc<dyn TUI + Send + Sync> = Arc::new(tui.clone());
    let editor = Arc::new(Editor::new(tui_handle, editor_theme(), None));
    editor.set_text("hello");
    tui.add_child(editor.clone());
    tui.start();
    terminal.wait_for_render().await;

    terminal.send_input("\x1b[<0;3;2M");
    terminal.send_input("\x1b[<0;3;2m");
    terminal.send_input("X");
    terminal.wait_for_render().await;

    assert_eq!(editor.get_text(), "heXllo");
    assert!(
        tui.get_focused_component()
            .is_some_and(|c| Arc::ptr_eq(&c, &(editor.clone() as Arc<dyn Component>)))
    );
    tui.stop(None);
}

#[tokio::test]
async fn selects_and_copies_editor_text_on_drag_instead_of_moving_the_cursor() {
    let terminal = Arc::new(VirtualTerminal::new(20, 6));
    let copied = Arc::new(Mutex::new(Vec::<String>::new()));
    let copied_clone = Arc::clone(&copied);
    let tui = TuiAltScreen::new(
        terminal.clone(),
        None,
        None,
        Some(TuiAltScreenOptions {
            copy_selection: Some(Arc::new(move |text| {
                copied_clone.lock().unwrap().push(text);
                Box::pin(async { CopySelectionResult::Bool(true) })
            })),
            ..Default::default()
        }),
    );
    let tui_handle: Arc<dyn TUI + Send + Sync> = Arc::new(tui.clone());
    let editor = Arc::new(Editor::new(tui_handle, editor_theme(), None));
    editor.set_text("hello world");
    tui.add_child(editor.clone());
    tui.start();
    terminal.wait_for_render().await;

    let cursor_before = editor.get_cursor();
    terminal.send_input("\x1b[<0;1;2M");
    terminal.send_input("\x1b[<32;5;2M");
    terminal.send_input("\x1b[<0;5;2m");
    terminal.wait_for_render().await;

    assert_eq!(*copied.lock().unwrap(), vec!["hello".to_string()]);
    assert_eq!(editor.get_cursor(), cursor_before);
    tui.stop(None);
}
