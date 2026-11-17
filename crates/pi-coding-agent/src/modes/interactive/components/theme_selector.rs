//! Port of packages/coding-agent/src/modes/interactive/components/theme-selector.ts

use std::sync::Arc;

use pi_tui::{
    Component, Container, SelectItem, SelectList, SelectListLayoutOptions, TuiMouseEvent, TuiMouseEventResult,
};

use super::super::theme::theme::{get_available_themes, get_select_list_theme};
use super::dynamic_border::DynamicBorder;

const THEME_SELECT_LIST_LAYOUT: SelectListLayoutOptions = SelectListLayoutOptions {
    min_primary_column_width: Some(12),
    max_primary_column_width: Some(32),
    truncate_primary: None,
};

/// Component that renders a theme selector.
#[derive(Clone)]
pub struct ThemeSelectorComponent {
    container: Container,
    select_list: SelectList,
}

impl ThemeSelectorComponent {
    pub fn new(
        current_theme: &str,
        on_select: Arc<dyn Fn(&str) + Send + Sync>,
        on_cancel: Arc<dyn Fn() + Send + Sync>,
        on_preview: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> Self {
        let themes = get_available_themes();
        let theme_items: Vec<SelectItem> = themes
            .iter()
            .map(|name| SelectItem {
                value: name.clone(),
                label: name.clone(),
                description: if name == current_theme {
                    Some("(current)".to_string())
                } else {
                    None
                },
            })
            .collect();

        let container = Container::new();
        container.add_child(Arc::new(DynamicBorder::default()));

        let select_list = SelectList::new(theme_items, 10, get_select_list_theme(), Some(THEME_SELECT_LIST_LAYOUT));
        if let Some(idx) = themes.iter().position(|name| name == current_theme) {
            select_list.set_selected_index(idx as i64);
        }

        select_list.set_on_select(Some(Arc::new(move |item: &SelectItem| {
            on_select(&item.value);
        })));
        select_list.set_on_cancel(Some(Arc::new(move || {
            on_cancel();
        })));
        select_list.set_on_selection_change(Some(Arc::new(move |item: &SelectItem| {
            on_preview(&item.value);
        })));

        container.add_child(Arc::new(select_list.clone()));
        container.add_child(Arc::new(DynamicBorder::default()));

        Self { container, select_list }
    }

    pub fn get_select_list(&self) -> &SelectList {
        &self.select_list
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    pub fn handle_input(&self, key_data: &str) {
        self.select_list.handle_input(key_data);
    }

    pub fn invalidate(&self) {
        self.container.invalidate();
    }
}

impl Component for ThemeSelectorComponent {
    fn render(&self, width: usize) -> Vec<String> {
        ThemeSelectorComponent::render(self, width)
    }

    fn handle_input(&self, data: &str) {
        ThemeSelectorComponent::handle_input(self, data)
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.container.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        ThemeSelectorComponent::invalidate(self)
    }

    fn has_handle_input(&self) -> bool {
        true
    }

    fn is_container_handle_mouse(&self) -> bool {
        true
    }

    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        Some(self.container.children())
    }
}
