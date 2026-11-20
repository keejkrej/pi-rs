//! Port of packages/tui/src/components/select-list.ts

#![allow(dead_code, unused_imports, unused_variables)]

use std::sync::{Arc, Mutex, MutexGuard};

use crate::keybindings::get_select_page_index;
use crate::tui::{Component, TuiMouseButton, TuiMouseEvent, TuiMouseEventResult, TuiMouseEventType};

const DEFAULT_PRIMARY_COLUMN_WIDTH: i64 = 32;
const PRIMARY_COLUMN_GAP: i64 = 2;
const MIN_DESCRIPTION_WIDTH: i64 = 10;

pub type SelectItemCallback = Arc<dyn Fn(&SelectItem) + Send + Sync>;
pub type SelectCancelCallback = Arc<dyn Fn() + Send + Sync>;
pub type SelectListStyleFn = Arc<dyn Fn(&str) -> String + Send + Sync>;
pub type SelectListTruncatePrimaryFn = Arc<dyn Fn(&SelectListTruncatePrimaryContext) -> String + Send + Sync>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectItem {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

/// Theme callbacks. Object-literal order: `selectedPrefix`, `selectedText`, `description`, `scrollInfo`, `noMatch`.
///
/// PORT: `PartialEq`/`Eq` are `Arc` pointer identity so [`crate::components::editor::EditorTheme`] can derive them.
/// TS compares these functions by reference.
#[derive(Clone)]
pub struct SelectListTheme {
    pub selected_prefix: SelectListStyleFn,
    pub selected_text: SelectListStyleFn,
    pub description: SelectListStyleFn,
    pub scroll_info: SelectListStyleFn,
    pub no_match: SelectListStyleFn,
}

impl PartialEq for SelectListTheme {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.selected_prefix, &other.selected_prefix)
            && Arc::ptr_eq(&self.selected_text, &other.selected_text)
            && Arc::ptr_eq(&self.description, &other.description)
            && Arc::ptr_eq(&self.scroll_info, &other.scroll_info)
            && Arc::ptr_eq(&self.no_match, &other.no_match)
    }
}

impl Eq for SelectListTheme {}

impl std::fmt::Debug for SelectListTheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SelectListTheme").finish_non_exhaustive()
    }
}

/// Passed to `SelectListLayoutOptions.truncatePrimary`.
///
/// Field order is the object literal: `text`, `maxWidth`, `columnWidth`, `item`, `isSelected`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectListTruncatePrimaryContext {
    pub text: String,
    pub max_width: i64,
    pub column_width: i64,
    pub item: SelectItem,
    pub is_selected: bool,
}

/// `None` on every field is the TS default `{}`.
///
/// `minPrimaryColumnWidth` / `maxPrimaryColumnWidth` are integral column counts (`i64`) so
/// `Some(12)` / `Some(32)` in the editor's slash-command layout type-check.
#[derive(Clone, Default)]
pub struct SelectListLayoutOptions {
    pub min_primary_column_width: Option<i64>,
    pub max_primary_column_width: Option<i64>,
    pub truncate_primary: Option<SelectListTruncatePrimaryFn>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VisibleRange {
    start_index: i64,
    end_index: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PrimaryColumnBounds {
    min: i64,
    max: i64,
}

struct SelectListFields {
    items: Vec<SelectItem>,
    filtered_items: Vec<SelectItem>,
    selected_index: i64,
    mouse_pressed_index: Option<i64>,
    max_visible: i64,
    theme: SelectListTheme,
    layout: SelectListLayoutOptions,
    on_select: Option<SelectItemCallback>,
    on_cancel: Option<SelectCancelCallback>,
    on_selection_change: Option<SelectItemCallback>,
}

struct SelectListInner {
    fields: Mutex<SelectListFields>,
}

/// PORT: TS class with identity. Handle so containers can store `Arc<dyn Component>`.
#[derive(Clone)]
pub struct SelectList {
    inner: Arc<SelectListInner>,
}

fn normalize_to_single_line(text: &str) -> String {
    let mut collapsed = String::with_capacity(text.len());
    let mut in_newline = false;
    for ch in text.chars() {
        if ch == '\r' || ch == '\n' {
            if !in_newline {
                collapsed.push(' ');
                in_newline = true;
            }
        } else {
            in_newline = false;
            collapsed.push(ch);
        }
    }
    pi_js::str16::trim(&collapsed).to_string()
}

fn clamp(value: i64, min: i64, max: i64) -> i64 {
    min.max(max.min(value))
}

impl SelectList {
    fn fields(&self) -> MutexGuard<'_, SelectListFields> {
        self.inner.fields.lock().unwrap()
    }

    /// `layout` `None` is the TS default `{}`.
    pub fn new(
        items: Vec<SelectItem>,
        max_visible: i64,
        theme: SelectListTheme,
        layout: Option<SelectListLayoutOptions>,
    ) -> Self {
        let filtered_items = items.clone();
        Self {
            inner: Arc::new(SelectListInner {
                fields: Mutex::new(SelectListFields {
                    items,
                    filtered_items,
                    selected_index: 0,
                    mouse_pressed_index: None,
                    max_visible,
                    theme,
                    layout: layout.unwrap_or_default(),
                    on_select: None,
                    on_cancel: None,
                    on_selection_change: None,
                }),
            }),
        }
    }

    pub fn on_select(&self) -> Option<SelectItemCallback> {
        self.fields().on_select.clone()
    }

    pub fn set_on_select(&self, on_select: Option<SelectItemCallback>) {
        self.fields().on_select = on_select;
    }

    pub fn on_cancel(&self) -> Option<SelectCancelCallback> {
        self.fields().on_cancel.clone()
    }

    pub fn set_on_cancel(&self, on_cancel: Option<SelectCancelCallback>) {
        self.fields().on_cancel = on_cancel;
    }

    pub fn on_selection_change(&self) -> Option<SelectItemCallback> {
        self.fields().on_selection_change.clone()
    }

    pub fn set_on_selection_change(&self, on_selection_change: Option<SelectItemCallback>) {
        self.fields().on_selection_change = on_selection_change;
    }

    pub fn set_filter(&self, filter: &str) {
        let lower_filter = filter.to_lowercase();
        let mut fields = self.fields();
        fields.filtered_items = fields
            .items
            .iter()
            .filter(|item| item.value.to_lowercase().starts_with(&lower_filter))
            .cloned()
            .collect();
        fields.selected_index = 0;
    }

    pub fn set_selected_index(&self, index: i64) {
        let mut fields = self.fields();
        let max_idx = (fields.filtered_items.len() as i64) - 1;
        fields.selected_index = clamp(index, 0, max_idx);
    }

    /// No cached state to invalidate currently.
    pub fn invalidate(&self) {}

    pub fn render(&self, width: usize) -> Vec<String> {
        let (
            theme,
            truncate_primary,
            filtered_empty,
            visible_items,
            start_index,
            end_index,
            selected_index,
            total_len,
            primary_column_width,
        ) = {
            let fields = self.fields();
            let theme = fields.theme.clone();
            if fields.filtered_items.is_empty() {
                (theme, None, true, Vec::new(), 0, 0, fields.selected_index, 0, 0)
            } else {
                let VisibleRange { start_index, end_index } = Self::get_visible_range_locked(&fields);
                let start = start_index.max(0) as usize;
                let end = end_index.max(start_index).min(fields.filtered_items.len() as i64) as usize;
                let visible_items = fields.filtered_items[start..end].to_vec();
                let primary_column_width = Self::get_primary_column_width_locked(&fields);
                (
                    theme,
                    fields.layout.truncate_primary.clone(),
                    false,
                    visible_items,
                    start_index,
                    end_index,
                    fields.selected_index,
                    fields.filtered_items.len() as i64,
                    primary_column_width,
                )
            }
        };

        if filtered_empty {
            return vec![(theme.no_match)("  No matching commands")];
        }

        let mut lines = Vec::new();
        for (offset, item) in visible_items.iter().enumerate() {
            let idx = start_index + (offset as i64);
            let is_selected = idx == selected_index;
            let description_single_line = item
                .description
                .as_deref()
                .map(normalize_to_single_line)
                .filter(|s| !s.is_empty());
            lines.push(Self::render_item_with(
                &theme,
                truncate_primary.as_ref(),
                item,
                is_selected,
                width,
                description_single_line.as_deref(),
                primary_column_width,
            ));
        }

        if start_index > 0 || end_index < total_len {
            let scroll_text = format!("  ({}/{})", selected_index + 1, total_len);
            lines.push((theme.scroll_info)(&scroll_text));
        }

        lines
    }

    pub fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        let mut notify_item: Option<(SelectItemCallback, SelectItem)> = None;
        let mut select_item: Option<(SelectItemCallback, SelectItem)> = None;
        let result: TuiMouseEventResult;

        {
            let mut fields = self.fields();
            if fields.filtered_items.is_empty() {
                return None;
            }

            if event.r#type == TuiMouseEventType::Wheel && event.wheel_delta.is_some_and(|d| d != 0) {
                let wheel_delta = event.wheel_delta.unwrap_or(0);
                let delta = if wheel_delta < 0 { -1 } else { 1 };
                let previous_index = fields.selected_index;
                let max_idx = (fields.filtered_items.len() as i64) - 1;
                fields.selected_index = clamp(fields.selected_index + delta, 0, max_idx);
                let changed = fields.selected_index != previous_index;
                if changed {
                    notify_item = Self::selection_change_payload(&fields);
                }
                result = TuiMouseEventResult {
                    handled: Some(true),
                    render: Some(changed),
                    ..Default::default()
                };
            } else {
                if event.button != TuiMouseButton::Left
                    || (event.r#type != TuiMouseEventType::Press && event.r#type != TuiMouseEventType::Click)
                {
                    return None;
                }
                let VisibleRange { start_index, end_index } = Self::get_visible_range_locked(&fields);
                let item_index = start_index + event.y;
                if item_index < start_index || item_index >= end_index {
                    return None;
                }

                match event.r#type {
                    TuiMouseEventType::Press => {
                        fields.mouse_pressed_index = Some(item_index);
                        if fields.selected_index != item_index {
                            fields.selected_index = item_index;
                            notify_item = Self::selection_change_payload(&fields);
                        }
                        result = TuiMouseEventResult {
                            handled: Some(true),
                            focus: Some(true),
                            ..Default::default()
                        };
                    }
                    TuiMouseEventType::Click => {
                        let clicked_index = fields.mouse_pressed_index.unwrap_or(item_index);
                        fields.mouse_pressed_index = None;
                        let changed = fields.selected_index != clicked_index;
                        fields.selected_index = clicked_index;
                        if changed {
                            notify_item = Self::selection_change_payload(&fields);
                        }
                        if let Some(cb) = fields.on_select.clone()
                            && let Some(item) = Self::get_selected_item_locked(&fields)
                        {
                            select_item = Some((cb, item));
                        }
                        result = TuiMouseEventResult {
                            handled: Some(true),
                            ..Default::default()
                        };
                    }
                    _ => return None,
                }
            }
        }

        if let Some((cb, item)) = notify_item {
            cb(&item);
        }
        if let Some((cb, item)) = select_item {
            cb(&item);
        }

        Some(result)
    }

    pub fn handle_input(&self, key_data: &str) {
        let kb = crate::keybindings::get_keybindings();

        enum InputAction {
            None,
            NotifySelection(SelectItemCallback, SelectItem),
            Select(SelectItemCallback, SelectItem),
            Cancel(SelectCancelCallback),
        }

        let action = {
            let mut fields = self.fields();
            let len = fields.filtered_items.len() as i64;
            if kb.matches(key_data, "tui.select.up") {
                if len > 0 {
                    fields.selected_index = if fields.selected_index == 0 {
                        len - 1
                    } else {
                        fields.selected_index - 1
                    };
                    Self::selection_change_payload(&fields)
                        .map_or(InputAction::None, |(cb, item)| InputAction::NotifySelection(cb, item))
                } else {
                    InputAction::None
                }
            } else if kb.matches(key_data, "tui.select.down") {
                if len > 0 {
                    fields.selected_index = if fields.selected_index == len - 1 {
                        0
                    } else {
                        fields.selected_index + 1
                    };
                    Self::selection_change_payload(&fields)
                        .map_or(InputAction::None, |(cb, item)| InputAction::NotifySelection(cb, item))
                } else {
                    InputAction::None
                }
            } else if let Some(next_index) =
                get_select_page_index(&kb, key_data, fields.selected_index, len, fields.max_visible)
            {
                if len > 0 && next_index != fields.selected_index {
                    fields.selected_index = next_index;
                    Self::selection_change_payload(&fields)
                        .map_or(InputAction::None, |(cb, item)| InputAction::NotifySelection(cb, item))
                } else {
                    InputAction::None
                }
            } else if kb.matches(key_data, "tui.select.confirm") {
                match (fields.on_select.clone(), Self::get_selected_item_locked(&fields)) {
                    (Some(cb), Some(item)) => InputAction::Select(cb, item),
                    _ => InputAction::None,
                }
            } else if kb.matches(key_data, "tui.select.cancel") {
                fields.on_cancel.clone().map_or(InputAction::None, InputAction::Cancel)
            } else {
                InputAction::None
            }
        };

        match action {
            InputAction::None => {}
            InputAction::NotifySelection(cb, item) | InputAction::Select(cb, item) => cb(&item),
            InputAction::Cancel(cb) => cb(),
        }
    }

    pub fn get_selected_item(&self) -> Option<SelectItem> {
        let fields = self.fields();
        Self::get_selected_item_locked(&fields)
    }

    fn get_selected_item_locked(fields: &SelectListFields) -> Option<SelectItem> {
        usize::try_from(fields.selected_index)
            .ok()
            .and_then(|idx| fields.filtered_items.get(idx).cloned())
    }

    fn selection_change_payload(fields: &SelectListFields) -> Option<(SelectItemCallback, SelectItem)> {
        let cb = fields.on_selection_change.clone()?;
        let item = Self::get_selected_item_locked(fields)?;
        Some((cb, item))
    }

    fn get_visible_range_locked(fields: &SelectListFields) -> VisibleRange {
        let len = fields.filtered_items.len() as i64;
        let start_index =
            0_i64.max((fields.selected_index - fields.max_visible.div_euclid(2)).min(len - fields.max_visible));
        let end_index = (start_index + fields.max_visible).min(len);
        VisibleRange { start_index, end_index }
    }

    fn get_visible_range(&self) -> VisibleRange {
        Self::get_visible_range_locked(&self.fields())
    }

    fn render_item_with(
        theme: &SelectListTheme,
        truncate_primary: Option<&SelectListTruncatePrimaryFn>,
        item: &SelectItem,
        is_selected: bool,
        width: usize,
        description_single_line: Option<&str>,
        primary_column_width: i64,
    ) -> String {
        let prefix = if is_selected { "→ " } else { "  " };
        let prefix_width = crate::utils::visible_width(prefix) as i64;

        if let Some(desc) = description_single_line.filter(|s| !s.is_empty())
            && width > 40
        {
            let effective_primary_column_width =
                1_i64.max(primary_column_width.min((width as i64) - prefix_width - MIN_DESCRIPTION_WIDTH - 2));
            let max_primary_width = 1_i64.max(effective_primary_column_width - PRIMARY_COLUMN_GAP);
            let truncated_value = Self::truncate_primary_with(
                truncate_primary,
                item,
                is_selected,
                max_primary_width,
                effective_primary_column_width,
            );
            let truncated_width = crate::utils::visible_width(&truncated_value) as i64;
            let spacing_len = 1_i64.max(effective_primary_column_width - truncated_width);
            let spacing = " ".repeat(spacing_len as usize);
            let description_start = prefix_width + truncated_width + spacing_len;
            let remaining_width = (width as i64) - description_start - 2;
            if remaining_width > MIN_DESCRIPTION_WIDTH {
                let truncated_desc = crate::utils::truncate_to_width(desc, remaining_width, Some(""), Some(false));
                if is_selected {
                    (theme.selected_text)(&format!("{prefix}{truncated_value}{spacing}{truncated_desc}"))
                } else {
                    let desc_text = (theme.description)(&format!("{spacing}{truncated_desc}"));
                    format!("{prefix}{truncated_value}{desc_text}")
                }
            } else {
                let max_width = (width as i64) - prefix_width - 2;
                let truncated_value =
                    Self::truncate_primary_with(truncate_primary, item, is_selected, max_width, max_width);
                if is_selected {
                    (theme.selected_text)(&format!("{prefix}{truncated_value}"))
                } else {
                    format!("{prefix}{truncated_value}")
                }
            }
        } else {
            let max_width = (width as i64) - prefix_width - 2;
            let truncated_value =
                Self::truncate_primary_with(truncate_primary, item, is_selected, max_width, max_width);
            if is_selected {
                (theme.selected_text)(&format!("{prefix}{truncated_value}"))
            } else {
                format!("{prefix}{truncated_value}")
            }
        }
    }

    fn render_item(
        &self,
        item: &SelectItem,
        is_selected: bool,
        width: usize,
        description_single_line: Option<&str>,
        primary_column_width: i64,
    ) -> String {
        let (theme, truncate_primary) = {
            let fields = self.fields();
            (fields.theme.clone(), fields.layout.truncate_primary.clone())
        };
        Self::render_item_with(
            &theme,
            truncate_primary.as_ref(),
            item,
            is_selected,
            width,
            description_single_line,
            primary_column_width,
        )
    }

    fn get_primary_column_width_locked(fields: &SelectListFields) -> i64 {
        let bounds = Self::get_primary_column_bounds_locked(fields);
        let mut widest = 0_i64;
        for item in &fields.filtered_items {
            let w = crate::utils::visible_width(&Self::get_display_value_locked(item)) as i64;
            if w > widest {
                widest = w;
            }
        }
        clamp(widest + PRIMARY_COLUMN_GAP, bounds.min, bounds.max)
    }

    fn get_primary_column_width(&self) -> i64 {
        Self::get_primary_column_width_locked(&self.fields())
    }

    fn get_primary_column_bounds_locked(fields: &SelectListFields) -> PrimaryColumnBounds {
        let raw_min = fields
            .layout
            .min_primary_column_width
            .unwrap_or(DEFAULT_PRIMARY_COLUMN_WIDTH);
        let raw_max = fields
            .layout
            .max_primary_column_width
            .unwrap_or(DEFAULT_PRIMARY_COLUMN_WIDTH);
        let min = 1_i64.max(raw_min.min(raw_max));
        let max = min.max(raw_max);
        PrimaryColumnBounds { min, max }
    }

    fn get_primary_column_bounds(&self) -> PrimaryColumnBounds {
        Self::get_primary_column_bounds_locked(&self.fields())
    }

    fn truncate_primary_with(
        truncate_primary: Option<&SelectListTruncatePrimaryFn>,
        item: &SelectItem,
        is_selected: bool,
        max_width: i64,
        column_width: i64,
    ) -> String {
        let display_value = Self::get_display_value_locked(item);
        let target_width = 1_i64.max(max_width);
        if let Some(custom_fn) = truncate_primary {
            let custom = custom_fn(&SelectListTruncatePrimaryContext {
                text: display_value,
                max_width,
                column_width,
                item: item.clone(),
                is_selected,
            });
            crate::utils::truncate_to_width(&custom, target_width, Some(""), Some(false))
        } else {
            crate::utils::truncate_to_width(&display_value, target_width, Some("..."), Some(false))
        }
    }

    fn truncate_primary(&self, item: &SelectItem, is_selected: bool, max_width: i64, column_width: i64) -> String {
        let truncate_primary = self.fields().layout.truncate_primary.clone();
        Self::truncate_primary_with(truncate_primary.as_ref(), item, is_selected, max_width, column_width)
    }

    fn get_display_value_locked(item: &SelectItem) -> String {
        if item.label.is_empty() {
            item.value.clone()
        } else {
            item.label.clone()
        }
    }

    fn get_display_value(&self, item: &SelectItem) -> String {
        Self::get_display_value_locked(item)
    }

    fn notify_selection_change(&self) {
        let payload = {
            let fields = self.fields();
            Self::selection_change_payload(&fields)
        };
        if let Some((cb, item)) = payload {
            cb(&item);
        }
    }
}

impl Component for SelectList {
    fn render(&self, width: usize) -> Vec<String> {
        SelectList::render(self, width)
    }

    fn handle_input(&self, data: &str) {
        SelectList::handle_input(self, data)
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        SelectList::handle_mouse(self, event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn captures_viewport_input(&self) -> bool {
        true
    }

    fn invalidate(&self) {
        SelectList::invalidate(self)
    }

    fn has_handle_input(&self) -> bool {
        true
    }
}
