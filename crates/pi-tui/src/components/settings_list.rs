//! Port of packages/tui/src/components/settings-list.ts

#![allow(dead_code, unused_imports, unused_variables)]

use std::sync::{Arc, Mutex, MutexGuard};

use crate::components::input::Input;
use crate::tui::{Component, TuiMouseEvent, TuiMouseEventResult};

pub type SettingsLabelFn = Arc<dyn Fn(&str, bool) -> String + Send + Sync>;
pub type SettingsTextFn = Arc<dyn Fn(&str) -> String + Send + Sync>;
pub type SettingsChangeCallback = Arc<dyn Fn(&str, &str) + Send + Sync>;
pub type SettingsCancelCallback = Arc<dyn Fn() + Send + Sync>;

/// `done(selectedValue?, options?)`.
///
/// Field order of `options` is `navigateTo`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsSubmenuDoneOptions {
    pub navigate_to: Option<String>,
}

pub type SettingsDoneCallback = Arc<dyn Fn(Option<String>, Option<SettingsSubmenuDoneOptions>) + Send + Sync>;

pub type SettingsSubmenuFn = Arc<dyn Fn(&str, SettingsDoneCallback) -> Arc<dyn Component> + Send + Sync>;

#[derive(Clone)]
pub struct SettingItem {
    /// Unique identifier for this setting.
    pub id: String,
    /// Display label (left side).
    pub label: String,
    /// Optional description shown when selected.
    pub description: Option<String>,
    /// Current value to display (right side).
    pub current_value: String,
    /// If provided, Enter/Space cycles through these values.
    pub values: Option<Vec<String>>,
    /// If provided, Enter opens this submenu. Receives current value and done callback.
    /// `done()` accepts an optional selectedValue and an optional navigateTo id to move the cursor after close.
    pub submenu: Option<SettingsSubmenuFn>,
}

/// Object-literal order: `label`, `value`, `description`, `cursor`, `hint`.
#[derive(Clone)]
pub struct SettingsListTheme {
    pub label: SettingsLabelFn,
    pub value: SettingsLabelFn,
    pub description: SettingsTextFn,
    pub cursor: String,
    pub hint: SettingsTextFn,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsListOptions {
    pub enable_search: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VisibleRange {
    start_index: i64,
    end_index: i64,
}

struct SettingsListFields {
    items: Vec<SettingItem>,
    /// Indexes into `items`. TS `filteredItems` is those same objects, not copies
    /// (`updateValue` / submenu `done` write `currentValue` on the shared item).
    /// Search off: `0..items.len()`, filled in `new`.
    filtered_indexes: Vec<i64>,
    theme: SettingsListTheme,
    selected_index: i64,
    mouse_pressed_index: Option<i64>,
    max_visible: i64,
    on_change: SettingsChangeCallback,
    on_cancel: SettingsCancelCallback,
    search_input: Option<Input>,
    search_enabled: bool,
    /// Submenu state.
    submenu_component: Option<Arc<dyn Component>>,
    submenu_item_index: Option<i64>,
    navigate_after_close: Option<String>,
}

struct SettingsListInner {
    fields: Mutex<SettingsListFields>,
}

/// PORT: TS class with identity. Handle so containers can store `Arc<dyn Component>`.
#[derive(Clone)]
pub struct SettingsList {
    inner: Arc<SettingsListInner>,
}

impl SettingsList {
    fn fields(&self) -> MutexGuard<'_, SettingsListFields> {
        self.inner.fields.lock().unwrap()
    }

    /// `options` `None` is the TS default `{}` (`enableSearch` defaults to false inside).
    pub fn new(
        items: Vec<SettingItem>,
        max_visible: i64,
        theme: SettingsListTheme,
        on_change: SettingsChangeCallback,
        on_cancel: SettingsCancelCallback,
        options: Option<SettingsListOptions>,
    ) -> Self {
        todo!("port: SettingsList::new")
    }

    /// Update an item's currentValue.
    pub fn update_value(&self, id: &str, new_value: &str) {
        todo!("port: SettingsList::update_value")
    }

    /// Move selection to the item with the given id (no-op if not found).
    pub fn select_item(&self, id: &str) {
        todo!("port: SettingsList::select_item")
    }

    pub fn invalidate(&self) {
        todo!("port: SettingsList::invalidate")
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        todo!("port: SettingsList::render")
    }

    fn render_main_list(&self, width: usize) -> Vec<String> {
        todo!("port: SettingsList::render_main_list")
    }

    pub fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        todo!("port: SettingsList::handle_mouse")
    }

    pub fn handle_input(&self, data: &str) {
        todo!("port: SettingsList::handle_input")
    }

    fn get_display_items(&self) -> Vec<SettingItem> {
        todo!("port: SettingsList::get_display_items")
    }

    fn get_visible_range(&self, display_items: &[SettingItem]) -> VisibleRange {
        todo!("port: SettingsList::get_visible_range")
    }

    fn activate_item(&self) {
        todo!("port: SettingsList::activate_item")
    }

    fn close_submenu(&self) {
        todo!("port: SettingsList::close_submenu")
    }

    fn apply_filter(&self, query: &str) {
        todo!("port: SettingsList::apply_filter")
    }

    fn add_hint_line(&self, lines: &mut Vec<String>, width: usize) {
        todo!("port: SettingsList::add_hint_line")
    }
}

impl Component for SettingsList {
    fn render(&self, width: usize) -> Vec<String> {
        SettingsList::render(self, width)
    }

    fn handle_input(&self, data: &str) {
        SettingsList::handle_input(self, data)
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        SettingsList::handle_mouse(self, event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        SettingsList::invalidate(self)
    }

    fn has_handle_input(&self) -> bool {
        true
    }
}
