//! Port of packages/coding-agent/src/modes/interactive/components/thinking-selector.ts

use std::sync::{Arc, Mutex};

use pi_agent_core::ThinkingLevel;
use pi_tui::{
    Component, Container, Focusable, Input, SelectItem, SelectList, SelectListLayoutOptions, Spacer, Text,
    TuiMouseEvent, TuiMouseEventResult, fuzzy_filter, get_keybindings,
};

use super::super::theme::theme::{ThemeColor, get_select_list_theme, theme};
use super::dynamic_border::DynamicBorder;
use super::keybinding_hints::key_display_text;

const THINKING_SELECT_LIST_LAYOUT: SelectListLayoutOptions = SelectListLayoutOptions {
    min_primary_column_width: Some(12),
    max_primary_column_width: Some(32),
    truncate_primary: None,
};

fn level_as_str(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Off => "off",
        ThinkingLevel::Minimal => "minimal",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::Xhigh => "xhigh",
    }
}

fn parse_level(s: &str) -> Option<ThinkingLevel> {
    match s {
        "off" => Some(ThinkingLevel::Off),
        "minimal" => Some(ThinkingLevel::Minimal),
        "low" => Some(ThinkingLevel::Low),
        "medium" => Some(ThinkingLevel::Medium),
        "high" => Some(ThinkingLevel::High),
        "xhigh" => Some(ThinkingLevel::Xhigh),
        _ => None,
    }
}

fn level_description(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Off => "No reasoning",
        ThinkingLevel::Minimal => "Very brief reasoning (~1k tokens)",
        ThinkingLevel::Low => "Light reasoning (~2k tokens)",
        ThinkingLevel::Medium => "Moderate reasoning (~8k tokens)",
        ThinkingLevel::High => "Deep reasoning (~16k tokens)",
        ThinkingLevel::Xhigh => "Extra-high reasoning (~32k tokens)",
    }
}

struct ThinkingSelectorState {
    select_list: SelectList,
    focused: bool,
}

struct ThinkingSelectorInner {
    container: Container,
    search_input: Input,
    select_list_child_index: usize,
    all_items: Vec<SelectItem>,
    on_select: Arc<dyn Fn(ThinkingLevel) + Send + Sync>,
    on_cancel: Arc<dyn Fn() + Send + Sync>,
    on_select_as_default: Option<Arc<dyn Fn(ThinkingLevel) + Send + Sync>>,
    state: Mutex<ThinkingSelectorState>,
}

/// Component that renders a thinking level selector with borders.
#[derive(Clone)]
pub struct ThinkingSelectorComponent {
    inner: Arc<ThinkingSelectorInner>,
}

impl ThinkingSelectorComponent {
    pub fn new(
        current_level: ThinkingLevel,
        available_levels: Vec<ThinkingLevel>,
        on_select: Arc<dyn Fn(ThinkingLevel) + Send + Sync>,
        on_cancel: Arc<dyn Fn() + Send + Sync>,
        on_select_as_default: Option<Arc<dyn Fn(ThinkingLevel) + Send + Sync>>,
        default_thinking_level: Option<ThinkingLevel>,
    ) -> Self {
        let all_items: Vec<SelectItem> = available_levels
            .into_iter()
            .map(|level| {
                let s = level_as_str(level);
                let mark = if level == current_level { "✓ " } else { "  " };
                let desc = if Some(level) == default_thinking_level {
                    format!("{} · default", level_description(level))
                } else {
                    level_description(level).to_string()
                };
                SelectItem {
                    value: s.to_string(),
                    label: format!("{mark}{s}"),
                    description: Some(desc),
                }
            })
            .collect();

        let container = Container::new();
        container.add_child(Arc::new(DynamicBorder::default()));
        container.add_child(Arc::new(Spacer::new(Some(1))));
        container.add_child(Arc::new(Text::new(Some("Thinking Level"), Some(0), Some(0), None)));
        container.add_child(Arc::new(Spacer::new(Some(1))));
        container.add_child(Arc::new(Text::new(
            Some(&format!(
                "{} cycles thinking levels in-session",
                key_display_text(&"app.thinking.cycle".to_string())
            )),
            Some(0),
            Some(0),
            None,
        )));
        container.add_child(Arc::new(Spacer::new(Some(1))));

        let search_input = Input::new(None);
        container.add_child(Arc::new(search_input.clone()));
        container.add_child(Arc::new(Spacer::new(Some(1))));

        let select_list = Self::build_select_list(
            all_items.clone(),
            Some(current_level),
            Arc::clone(&on_select),
            Arc::clone(&on_cancel),
        );
        let list_c = select_list.clone();
        search_input.set_on_submit(Some(Arc::new(move |_| {
            list_c.handle_input("\r");
        })));

        let select_list_child_index = container.children().len();
        container.add_child(Arc::new(select_list.clone()));
        container.add_child(Arc::new(Spacer::new(Some(1))));
        container.add_child(Arc::new(Text::new(
            Some(&theme().fg(
                ThemeColor::Dim,
                &format!(
                    "  {} to select · {} to set as default · {} to cancel",
                    key_display_text(&"tui.select.confirm".to_string()),
                    key_display_text(&"app.thinking.save".to_string()),
                    key_display_text(&"tui.select.cancel".to_string()),
                ),
            )),
            Some(0),
            Some(0),
            None,
        )));
        container.add_child(Arc::new(DynamicBorder::default()));

        Self {
            inner: Arc::new(ThinkingSelectorInner {
                container,
                search_input,
                select_list_child_index,
                all_items,
                on_select,
                on_cancel,
                on_select_as_default,
                state: Mutex::new(ThinkingSelectorState {
                    select_list,
                    focused: false,
                }),
            }),
        }
    }

    fn build_select_list(
        items: Vec<SelectItem>,
        preselect: Option<ThinkingLevel>,
        on_select: Arc<dyn Fn(ThinkingLevel) + Send + Sync>,
        on_cancel: Arc<dyn Fn() + Send + Sync>,
    ) -> SelectList {
        let max_visible = items.len().max(1);
        let list = SelectList::new(
            items.clone(),
            max_visible as i64,
            get_select_list_theme(),
            Some(THINKING_SELECT_LIST_LAYOUT),
        );
        if let Some(level) = preselect {
            let level_str = level_as_str(level);
            if let Some(idx) = items.iter().position(|item| item.value == level_str) {
                list.set_selected_index(idx as i64);
            }
        }
        list.set_on_select(Some(Arc::new(move |item: &SelectItem| {
            if let Some(level) = parse_level(&item.value) {
                on_select(level);
            }
        })));
        list.set_on_cancel(Some(Arc::new(move || {
            on_cancel();
        })));
        list
    }

    fn apply_filter(&self, query: &str) {
        let filtered = if query.is_empty() {
            self.inner.all_items.clone()
        } else {
            fuzzy_filter(
                &self.inner.all_items,
                query,
                Arc::new(|item: &SelectItem| format!("{} {}", item.value, item.description.as_deref().unwrap_or(""))),
            )
        };
        let current_list = self.get_select_list();
        let selected_value = current_list
            .get_selected_item()
            .and_then(|item| parse_level(&item.value));
        let new_list = Self::build_select_list(
            filtered,
            selected_value,
            Arc::clone(&self.inner.on_select),
            Arc::clone(&self.inner.on_cancel),
        );
        let list_for_submit = new_list.clone();
        self.inner.search_input.set_on_submit(Some(Arc::new(move |_| {
            list_for_submit.handle_input("\r");
        })));

        let mut children = self.inner.container.children();
        if let Some(slot) = children.get_mut(self.inner.select_list_child_index) {
            *slot = Arc::new(new_list.clone());
            self.inner.container.set_children(children);
        }
        self.inner.state.lock().unwrap().select_list = new_list;
    }

    pub fn focused(&self) -> bool {
        self.inner.state.lock().unwrap().focused
    }

    pub fn set_focused(&self, value: bool) {
        self.inner.state.lock().unwrap().focused = value;
        self.inner.search_input.set_focused(value);
    }

    pub fn handle_input(&self, key_data: &str) {
        let kb = get_keybindings();
        if kb.matches(key_data, "app.thinking.save")
            && let Some(ref save_cb) = self.inner.on_select_as_default
        {
            if let Some(item) = self.get_select_list().get_selected_item()
                && let Some(level) = parse_level(&item.value)
            {
                save_cb(level);
            }
            return;
        }

        let is_nav = kb.matches(key_data, "tui.select.up")
            || kb.matches(key_data, "tui.select.down")
            || kb.matches(key_data, "tui.select.confirm")
            || kb.matches(key_data, "tui.select.cancel");
        if is_nav {
            self.get_select_list().handle_input(key_data);
            return;
        }

        self.inner.search_input.handle_input(key_data);
        let query = self.inner.search_input.get_value();
        self.apply_filter(&query);
    }

    pub fn get_select_list(&self) -> SelectList {
        self.inner.state.lock().unwrap().select_list.clone()
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        self.inner.container.render(width)
    }

    pub fn invalidate(&self) {
        self.inner.container.invalidate();
    }
}

impl Focusable for ThinkingSelectorComponent {
    fn focused(&self) -> bool {
        ThinkingSelectorComponent::focused(self)
    }

    fn set_focused(&self, focused: bool) {
        ThinkingSelectorComponent::set_focused(self, focused)
    }
}

impl Component for ThinkingSelectorComponent {
    fn render(&self, width: usize) -> Vec<String> {
        ThinkingSelectorComponent::render(self, width)
    }

    fn handle_input(&self, data: &str) {
        ThinkingSelectorComponent::handle_input(self, data)
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.container.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        ThinkingSelectorComponent::invalidate(self)
    }

    fn has_handle_input(&self) -> bool {
        true
    }

    fn is_container_handle_mouse(&self) -> bool {
        true
    }

    fn as_focusable(&self) -> Option<&dyn Focusable> {
        Some(self)
    }

    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        Some(self.inner.container.children())
    }
}
