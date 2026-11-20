//! Port of packages/tui/src/components/settings-list.ts

#![allow(dead_code, unused_imports, unused_variables)]

use std::sync::{
    Arc, Mutex, MutexGuard,
    atomic::{AtomicBool, Ordering},
};

use crate::components::input::Input;
use crate::keybindings::get_select_page_index;
use crate::tui::{Component, TuiMouseButton, TuiMouseEvent, TuiMouseEventResult, TuiMouseEventType};

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
        let search_enabled = options.and_then(|o| o.enable_search).unwrap_or(false);
        let filtered_indexes = (0..items.len() as i64).collect();
        let search_input = if search_enabled { Some(Input::new(None)) } else { None };
        Self {
            inner: Arc::new(SettingsListInner {
                fields: Mutex::new(SettingsListFields {
                    items,
                    filtered_indexes,
                    theme,
                    selected_index: 0,
                    mouse_pressed_index: None,
                    max_visible,
                    on_change,
                    on_cancel,
                    search_input,
                    search_enabled,
                    submenu_component: None,
                    submenu_item_index: None,
                    navigate_after_close: None,
                }),
            }),
        }
    }

    /// Update an item's currentValue.
    pub fn update_value(&self, id: &str, new_value: &str) {
        let mut fields = self.fields();
        if let Some(item) = fields.items.iter_mut().find(|item| item.id == id) {
            item.current_value = new_value.to_string();
        }
    }

    /// Move selection to the item with the given id (no-op if not found).
    pub fn select_item(&self, id: &str) {
        let mut fields = self.fields();
        Self::select_item_locked(&mut fields, id);
    }

    fn select_item_locked(fields: &mut SettingsListFields, id: &str) {
        let pos = if fields.search_enabled {
            fields.filtered_indexes.iter().position(|&idx| {
                usize::try_from(idx)
                    .ok()
                    .and_then(|i| fields.items.get(i))
                    .is_some_and(|item| item.id == id)
            })
        } else {
            fields.items.iter().position(|item| item.id == id)
        };
        if let Some(idx) = pos {
            fields.selected_index = idx as i64;
        }
    }

    pub fn invalidate(&self) {
        let sub = self.fields().submenu_component.clone();
        if let Some(sub) = sub {
            sub.invalidate();
        }
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        let sub = self.fields().submenu_component.clone();
        if let Some(sub) = sub {
            return sub.render(width);
        }
        self.render_main_list(width)
    }

    fn render_main_list(&self, width: usize) -> Vec<String> {
        let (
            search_enabled,
            search_input,
            items_empty,
            all_items_max_label,
            display_items,
            selected_index,
            max_visible,
            theme,
        ) = {
            let fields = self.fields();
            let max_lbl = fields
                .items
                .iter()
                .map(|item| crate::utils::visible_width(&item.label))
                .max()
                .unwrap_or(0)
                .min(36);
            (
                fields.search_enabled,
                fields.search_input.clone(),
                fields.items.is_empty(),
                max_lbl,
                Self::get_display_items_locked(&fields),
                fields.selected_index,
                fields.max_visible,
                fields.theme.clone(),
            )
        };

        let mut lines = Vec::new();
        if search_enabled && let Some(input) = search_input {
            lines.extend(input.render(width));
            lines.push(String::new());
        }

        if items_empty {
            lines.push((theme.hint)("  No settings available"));
            if search_enabled {
                Self::add_hint_line_with(&theme, search_enabled, &mut lines, width);
            }
            return lines;
        }

        if search_enabled && display_items.is_empty() {
            lines.push(crate::utils::truncate_to_width(
                &(theme.hint)("  No matching settings"),
                width as i64,
                None,
                None,
            ));
            Self::add_hint_line_with(&theme, search_enabled, &mut lines, width);
            return lines;
        }

        let max_label_width = all_items_max_label;

        let VisibleRange { start_index, end_index } =
            Self::get_visible_range_for(selected_index, max_visible, display_items.len() as i64);

        for i in start_index..end_index {
            let item = &display_items[i as usize];
            let is_selected = i == selected_index;

            let prefix = if is_selected { theme.cursor.as_str() } else { "  " };
            let prefix_width = crate::utils::visible_width(prefix);

            let label_width = crate::utils::visible_width(&item.label);
            let label_padding = " ".repeat(max_label_width.saturating_sub(label_width).max(0) as usize);
            let label_padded = format!("{}{label_padding}", item.label);
            let label_text = (theme.label)(&label_padded, is_selected);

            let separator = "  ";
            let used_width = prefix_width + max_label_width + crate::utils::visible_width(separator);
            let max_value_width = (width as i64) - used_width - 2;
            let truncated_value =
                crate::utils::truncate_to_width(&item.current_value, max_value_width, Some(""), Some(false));
            let value_text = (theme.value)(&truncated_value, is_selected);

            lines.push(crate::utils::truncate_to_width(
                &format!("{prefix}{label_text}{separator}{value_text}"),
                width as i64,
                None,
                None,
            ));
        }

        let total_display = display_items.len() as i64;
        if start_index > 0 || end_index < total_display {
            let scroll_text = format!("  ({}/{})", selected_index + 1, total_display);
            lines.push((theme.hint)(&crate::utils::truncate_to_width(
                &scroll_text,
                (width as i64) - 2,
                Some(""),
                Some(false),
            )));
        }

        if let Some(selected_item) = usize::try_from(selected_index)
            .ok()
            .and_then(|idx| display_items.get(idx))
            && let Some(desc) = selected_item.description.as_deref().filter(|s| !s.is_empty())
        {
            lines.push(String::new());
            let wrapped = crate::utils::wrap_text_with_ansi(desc, (width as i64) - 4);
            for line in wrapped {
                lines.push((theme.description)(&format!("  {line}")));
            }
        }

        Self::add_hint_line_with(&theme, search_enabled, &mut lines, width);
        lines
    }

    pub fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        let (sub, search_enabled, search_input) = {
            let fields = self.fields();
            (
                fields.submenu_component.clone(),
                fields.search_enabled,
                fields.search_input.clone(),
            )
        };
        if let Some(sub) = sub {
            return sub.handle_mouse(event).map(|mut r| {
                r.focus = Some(true);
                r
            });
        }

        if search_enabled && let Some(input) = search_input {
            if event.y == 0 {
                return input.handle_mouse(event).map(|mut r| {
                    r.focus = Some(true);
                    r
                });
            }
            if event.y == 1 {
                return None;
            }
        }

        let mut should_activate = false;
        let result: TuiMouseEventResult;
        {
            let mut fields = self.fields();
            let display_len = Self::get_display_len_locked(&fields);
            if display_len == 0 {
                return None;
            }

            if event.r#type == TuiMouseEventType::Wheel && event.wheel_delta.is_some_and(|d| d != 0) {
                let wheel_delta = event.wheel_delta.unwrap_or(0);
                let delta = if wheel_delta < 0 { -1 } else { 1 };
                let previous_index = fields.selected_index;
                fields.selected_index = 0_i64.max((display_len - 1).min(fields.selected_index + delta));
                result = TuiMouseEventResult {
                    handled: Some(true),
                    render: Some(fields.selected_index != previous_index),
                    ..Default::default()
                };
            } else {
                if event.button != TuiMouseButton::Left
                    || (event.r#type != TuiMouseEventType::Press && event.r#type != TuiMouseEventType::Click)
                {
                    return None;
                }
                let row_offset = if fields.search_enabled { 2_i64 } else { 0_i64 };
                let VisibleRange { start_index, end_index } =
                    Self::get_visible_range_for(fields.selected_index, fields.max_visible, display_len);

                let item_index = start_index + event.y - row_offset;
                if item_index < start_index || item_index >= end_index {
                    return None;
                }

                match event.r#type {
                    TuiMouseEventType::Press => {
                        fields.mouse_pressed_index = Some(item_index);
                        fields.selected_index = item_index;
                        result = TuiMouseEventResult {
                            handled: Some(true),
                            focus: Some(true),
                            ..Default::default()
                        };
                    }
                    TuiMouseEventType::Click => {
                        fields.selected_index = fields.mouse_pressed_index.unwrap_or(item_index);
                        fields.mouse_pressed_index = None;
                        should_activate = true;
                        result = TuiMouseEventResult {
                            handled: Some(true),
                            ..Default::default()
                        };
                    }
                    _ => return None,
                }
            }
        }

        if should_activate {
            self.activate_item();
        }

        Some(result)
    }

    pub fn handle_input(&self, data: &str) {
        let sub = self.fields().submenu_component.clone();
        if let Some(sub) = sub {
            sub.handle_input(data);
            return;
        }

        let kb = crate::keybindings::get_keybindings();

        enum Action {
            Done,
            Activate,
            SpaceWithSearch(Input),
            Cancel(SettingsCancelCallback),
            Search(Input),
        }

        let action = {
            let mut fields = self.fields();
            let display_len = Self::get_display_len_locked(&fields);
            if kb.matches(data, "tui.select.up") {
                if display_len > 0 {
                    fields.selected_index = if fields.selected_index == 0 {
                        display_len - 1
                    } else {
                        fields.selected_index - 1
                    };
                }
                Action::Done
            } else if kb.matches(data, "tui.select.down") {
                if display_len > 0 {
                    fields.selected_index = if fields.selected_index == display_len - 1 {
                        0
                    } else {
                        fields.selected_index + 1
                    };
                }
                Action::Done
            } else if let Some(next_index) =
                get_select_page_index(&kb, data, fields.selected_index, display_len, fields.max_visible)
            {
                if display_len > 0 {
                    fields.selected_index = next_index;
                }
                Action::Done
            } else if kb.matches(data, "tui.select.confirm") {
                Action::Activate
            } else if data == " " {
                if !fields.search_enabled {
                    Action::Activate
                } else if let Some(search_input) = fields.search_input.clone() {
                    Action::SpaceWithSearch(search_input)
                } else {
                    Action::Activate
                }
            } else if kb.matches(data, "tui.select.cancel") {
                Action::Cancel(fields.on_cancel.clone())
            } else if fields.search_enabled
                && let Some(search_input) = fields.search_input.clone()
            {
                Action::Search(search_input)
            } else {
                Action::Done
            }
        };

        match action {
            Action::Done => {}
            Action::Activate => self.activate_item(),
            Action::SpaceWithSearch(search_input) => {
                if search_input.get_value().is_empty() {
                    self.activate_item();
                } else {
                    search_input.handle_input(data);
                    self.apply_filter(&search_input.get_value());
                }
            }
            Action::Cancel(cb) => cb(),
            Action::Search(search_input) => {
                search_input.handle_input(data);
                self.apply_filter(&search_input.get_value());
            }
        }
    }

    fn get_display_len_locked(fields: &SettingsListFields) -> i64 {
        if fields.search_enabled {
            fields.filtered_indexes.len() as i64
        } else {
            fields.items.len() as i64
        }
    }

    fn get_display_items_locked(fields: &SettingsListFields) -> Vec<SettingItem> {
        if fields.search_enabled {
            fields
                .filtered_indexes
                .iter()
                .filter_map(|&idx| usize::try_from(idx).ok().and_then(|i| fields.items.get(i).cloned()))
                .collect()
        } else {
            fields.items.clone()
        }
    }

    fn get_display_items(&self) -> Vec<SettingItem> {
        Self::get_display_items_locked(&self.fields())
    }

    fn get_visible_range_for(selected_index: i64, max_visible: i64, len: i64) -> VisibleRange {
        let start_index = 0_i64.max((selected_index - max_visible.div_euclid(2)).min(len - max_visible));
        let end_index = (start_index + max_visible).min(len);
        VisibleRange { start_index, end_index }
    }

    fn get_visible_range(&self, display_items: &[SettingItem]) -> VisibleRange {
        let fields = self.fields();
        Self::get_visible_range_for(fields.selected_index, fields.max_visible, display_items.len() as i64)
    }

    fn activate_item(&self) {
        enum ActivateKind {
            None,
            Submenu {
                item_id: String,
                real_idx: usize,
                current_value: String,
                submenu_fn: SettingsSubmenuFn,
            },
            Cycle {
                id: String,
                new_value: String,
                on_change: SettingsChangeCallback,
            },
        }

        let kind = {
            let mut fields = self.fields();
            let real_idx = if fields.search_enabled {
                usize::try_from(fields.selected_index)
                    .ok()
                    .and_then(|i| fields.filtered_indexes.get(i).copied())
                    .and_then(|idx| usize::try_from(idx).ok())
            } else {
                usize::try_from(fields.selected_index).ok()
            };

            let Some(real_idx) = real_idx.filter(|&i| i < fields.items.len()) else {
                return;
            };

            let submenu_opt = fields.items[real_idx].submenu.clone();
            let values_opt = fields.items[real_idx].values.clone();
            let item_id = fields.items[real_idx].id.clone();
            let current_value = fields.items[real_idx].current_value.clone();

            if let Some(submenu_fn) = submenu_opt {
                fields.submenu_item_index = Some(fields.selected_index);
                ActivateKind::Submenu {
                    item_id,
                    real_idx,
                    current_value,
                    submenu_fn,
                }
            } else if let Some(values) = values_opt.filter(|v| !v.is_empty()) {
                let current_index = values.iter().position(|v| v == &current_value);
                let next_index = match current_index {
                    Some(idx) => (idx + 1) % values.len(),
                    None => 0,
                };
                let new_value = values[next_index].clone();
                let on_change = fields.on_change.clone();
                fields.items[real_idx].current_value = new_value.clone();
                ActivateKind::Cycle {
                    id: item_id,
                    new_value,
                    on_change,
                }
            } else {
                ActivateKind::None
            }
        };

        match kind {
            ActivateKind::None => {}
            ActivateKind::Cycle {
                id,
                new_value,
                on_change,
            } => on_change(&id, &new_value),
            ActivateKind::Submenu {
                item_id,
                real_idx,
                current_value,
                submenu_fn,
            } => {
                let list = self.clone();
                let closed_synchronously = Arc::new(AtomicBool::new(false));
                let closed_flag = closed_synchronously.clone();
                let done: SettingsDoneCallback = Arc::new(move |selected_value, options| {
                    closed_flag.store(true, Ordering::SeqCst);
                    if let Some(val) = selected_value {
                        let on_change = {
                            let mut fields = list.fields();
                            if fields.items.get(real_idx).is_some_and(|i| i.id == item_id) {
                                fields.items[real_idx].current_value = val.clone();
                            } else if let Some(item) = fields.items.iter_mut().find(|i| i.id == item_id) {
                                item.current_value = val.clone();
                            }
                            fields.on_change.clone()
                        };
                        on_change(&item_id, &val);
                    }
                    {
                        let mut fields = list.fields();
                        if let Some(nav) = options.and_then(|o| o.navigate_to) {
                            fields.navigate_after_close = Some(nav);
                        }
                        Self::close_submenu_locked(&mut fields);
                    }
                });

                let component = submenu_fn(&current_value, done);
                if !closed_synchronously.load(Ordering::SeqCst) {
                    self.fields().submenu_component = Some(component);
                }
            }
        }
    }

    fn close_submenu_locked(fields: &mut SettingsListFields) {
        fields.submenu_component = None;
        if let Some(nav) = fields.navigate_after_close.take() {
            Self::select_item_locked(fields, &nav);
        } else if let Some(idx) = fields.submenu_item_index {
            fields.selected_index = idx;
        }
        fields.submenu_item_index = None;
    }

    fn close_submenu(&self) {
        Self::close_submenu_locked(&mut self.fields());
    }

    fn apply_filter(&self, query: &str) {
        let (indices, items_snapshot) = {
            let fields = self.fields();
            (
                (0..fields.items.len() as i64).collect::<Vec<i64>>(),
                fields.items.clone(),
            )
        };
        let filtered = crate::fuzzy::fuzzy_filter(
            &indices,
            query,
            Arc::new(move |idx: &i64| items_snapshot[*idx as usize].label.clone()),
        );
        let mut fields = self.fields();
        fields.filtered_indexes = filtered;
        fields.selected_index = 0;
    }

    fn add_hint_line_with(theme: &SettingsListTheme, search_enabled: bool, lines: &mut Vec<String>, width: usize) {
        lines.push(String::new());
        let hint_text = if search_enabled {
            "  Type to search · Enter/Space to change · Esc to cancel"
        } else {
            "  Enter/Space to change · Esc to cancel"
        };
        lines.push(crate::utils::truncate_to_width(
            &(theme.hint)(hint_text),
            width as i64,
            Some(""),
            Some(false),
        ));
    }

    fn add_hint_line(&self, lines: &mut Vec<String>, width: usize) {
        let (theme, search_enabled) = {
            let fields = self.fields();
            (fields.theme.clone(), fields.search_enabled)
        };
        Self::add_hint_line_with(&theme, search_enabled, lines, width);
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

    fn captures_viewport_input(&self) -> bool {
        true
    }

    fn invalidate(&self) {
        SettingsList::invalidate(self)
    }

    fn has_handle_input(&self) -> bool {
        true
    }
}
