//! Port of packages/tui/src/components/select-list.ts

#![allow(dead_code, unused_imports, unused_variables)]

use std::sync::{Arc, Mutex, MutexGuard};

use crate::tui::{Component, TuiMouseEvent, TuiMouseEventResult};

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
    todo!("port: normalize_to_single_line")
}

fn clamp(value: i64, min: i64, max: i64) -> i64 {
    todo!("port: clamp")
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
        todo!("port: SelectList::new")
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
        todo!("port: SelectList::set_filter")
    }

    pub fn set_selected_index(&self, index: i64) {
        todo!("port: SelectList::set_selected_index")
    }

    /// No cached state to invalidate currently.
    pub fn invalidate(&self) {}

    pub fn render(&self, width: usize) -> Vec<String> {
        todo!("port: SelectList::render")
    }

    pub fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        todo!("port: SelectList::handle_mouse")
    }

    pub fn handle_input(&self, key_data: &str) {
        todo!("port: SelectList::handle_input")
    }

    pub fn get_selected_item(&self) -> Option<SelectItem> {
        todo!("port: SelectList::get_selected_item")
    }

    fn get_visible_range(&self) -> VisibleRange {
        todo!("port: SelectList::get_visible_range")
    }

    fn render_item(
        &self,
        item: &SelectItem,
        is_selected: bool,
        width: usize,
        description_single_line: Option<&str>,
        primary_column_width: i64,
    ) -> String {
        todo!("port: SelectList::render_item")
    }

    fn get_primary_column_width(&self) -> i64 {
        todo!("port: SelectList::get_primary_column_width")
    }

    fn get_primary_column_bounds(&self) -> PrimaryColumnBounds {
        todo!("port: SelectList::get_primary_column_bounds")
    }

    fn truncate_primary(&self, item: &SelectItem, is_selected: bool, max_width: i64, column_width: i64) -> String {
        todo!("port: SelectList::truncate_primary")
    }

    fn get_display_value(&self, item: &SelectItem) -> String {
        todo!("port: SelectList::get_display_value")
    }

    fn notify_selection_change(&self) {
        todo!("port: SelectList::notify_selection_change")
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

    fn invalidate(&self) {
        SelectList::invalidate(self)
    }

    fn has_handle_input(&self) -> bool {
        true
    }
}
