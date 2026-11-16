//! Port of packages/coding-agent/src/modes/interactive/components/show-images-selector.ts

use std::sync::Arc;

use pi_tui::{
    Component, Container, SelectItem, SelectList, SelectListLayoutOptions, TuiMouseEvent, TuiMouseEventResult,
};

use super::super::theme::theme::get_select_list_theme;
use super::dynamic_border::DynamicBorder;

const SHOW_IMAGES_SELECT_LIST_LAYOUT: SelectListLayoutOptions = SelectListLayoutOptions {
    min_primary_column_width: Some(12),
    max_primary_column_width: Some(32),
    truncate_primary: None,
};

#[derive(Clone)]
pub struct ShowImagesSelectorComponent {
    container: Container,
    select_list: SelectList,
}

impl ShowImagesSelectorComponent {
    pub fn new(
        current_value: bool,
        on_select: Arc<dyn Fn(bool) + Send + Sync>,
        on_cancel: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let items = vec![
            SelectItem {
                value: "yes".to_string(),
                label: "Yes".to_string(),
                description: Some("Show images inline in terminal".to_string()),
            },
            SelectItem {
                value: "no".to_string(),
                label: "No".to_string(),
                description: Some("Show text placeholder instead".to_string()),
            },
        ];

        let container = Container::new();
        container.add_child(Arc::new(DynamicBorder::default()));

        let select_list = SelectList::new(items, 5, get_select_list_theme(), Some(SHOW_IMAGES_SELECT_LIST_LAYOUT));
        select_list.set_selected_index(if current_value { 0 } else { 1 });
        select_list.set_on_select(Some(Arc::new(move |item: &SelectItem| {
            on_select(item.value == "yes");
        })));
        select_list.set_on_cancel(Some(Arc::new(move || {
            on_cancel();
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

    pub fn invalidate(&self) {
        self.container.invalidate();
    }
}

impl Component for ShowImagesSelectorComponent {
    fn render(&self, width: usize) -> Vec<String> {
        ShowImagesSelectorComponent::render(self, width)
    }

    fn handle_input(&self, data: &str) {
        self.select_list.handle_input(data);
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.container.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        ShowImagesSelectorComponent::invalidate(self)
    }

    fn is_container_handle_mouse(&self) -> bool {
        true
    }

    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        Some(self.container.children())
    }
}
