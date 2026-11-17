//! Port of packages/coding-agent/src/modes/interactive/components/scoped-models-selector.ts

use std::sync::{Arc, Mutex};

use indexmap::{IndexMap, IndexSet};
use pi_ai::Model;
use pi_tui::{
    Component, Container, Focusable, Input, Key, Spacer, Text, TuiMouseEvent, TuiMouseEventResult, fuzzy_filter,
    get_keybindings, matches_key,
};

use super::super::model_search::{get_distinct_model_name, get_model_search_text};
use super::super::theme::theme::{ThemeColor, theme};
use super::dynamic_border::DynamicBorder;
use super::keybinding_hints::{get_select_page_index, key_display_text};

type EnabledIds = Option<Vec<String>>;

fn is_enabled(enabled_ids: &EnabledIds, id: &str) -> bool {
    match enabled_ids {
        None => true,
        Some(ids) => ids.iter().any(|x| x == id),
    }
}

fn normalize_enabled(result: Vec<String>, all_ids: &[String]) -> EnabledIds {
    if result.len() == all_ids.len() && result.iter().all(|id| all_ids.contains(id)) {
        None
    } else {
        Some(result)
    }
}

fn toggle_id(enabled_ids: &EnabledIds, all_ids: &[String], id: &str) -> EnabledIds {
    match enabled_ids {
        None => Some(all_ids.iter().filter(|m| m.as_str() != id).cloned().collect()),
        Some(ids) => {
            if let Some(pos) = ids.iter().position(|x| x == id) {
                let mut next = Vec::with_capacity(ids.len() - 1);
                next.extend_from_slice(&ids[..pos]);
                next.extend_from_slice(&ids[pos + 1..]);
                Some(next)
            } else {
                let mut next = ids.clone();
                next.push(id.to_string());
                normalize_enabled(next, all_ids)
            }
        }
    }
}

fn enable_all(enabled_ids: &EnabledIds, all_ids: &[String], target_ids: Option<&[String]>) -> EnabledIds {
    let Some(ids) = enabled_ids else {
        return None;
    };
    let targets = target_ids.unwrap_or(all_ids);
    let mut result = ids.clone();
    for id in targets {
        if !result.contains(id) {
            result.push(id.clone());
        }
    }
    normalize_enabled(result, all_ids)
}

fn clear_all(enabled_ids: &EnabledIds, all_ids: &[String], target_ids: Option<&[String]>) -> EnabledIds {
    match enabled_ids {
        None => match target_ids {
            Some(t) => Some(all_ids.iter().filter(|id| !t.contains(id)).cloned().collect()),
            None => Some(Vec::new()),
        },
        Some(ids) => {
            let targets: IndexSet<&str> = match target_ids {
                Some(t) => t.iter().map(String::as_str).collect(),
                None => ids.iter().map(String::as_str).collect(),
            };
            Some(
                ids.iter()
                    .filter(|id| !targets.contains(id.as_str()))
                    .cloned()
                    .collect(),
            )
        }
    }
}

fn move_id(enabled_ids: &EnabledIds, id: &str, delta: isize) -> EnabledIds {
    let Some(ids) = enabled_ids else {
        return None;
    };
    let mut list = ids.clone();
    let Some(index) = list.iter().position(|x| x == id) else {
        return Some(list);
    };
    let new_index = index as isize + delta;
    if new_index < 0 || (new_index as usize) >= list.len() {
        return Some(list);
    }
    list.swap(index, new_index as usize);
    Some(list)
}

fn get_sorted_ids(enabled_ids: &EnabledIds, all_ids: &[String]) -> Vec<String> {
    match enabled_ids {
        None => all_ids.to_vec(),
        Some(ids) => {
            let enabled_set: IndexSet<&str> = ids.iter().map(String::as_str).collect();
            let mut out = ids.clone();
            for id in all_ids {
                if !enabled_set.contains(id.as_str()) {
                    out.push(id.clone());
                }
            }
            out
        }
    }
}

#[derive(Clone)]
struct ModelItem {
    full_id: String,
    model: Option<Model>,
    enabled: bool,
}

#[derive(Clone)]
pub struct ModelsConfig {
    pub all_models: Vec<Model>,
    pub enabled_model_ids: Option<Vec<String>>,
    pub refresh_status: Option<String>,
}

#[derive(Clone)]
pub struct ModelsCallbacks {
    pub on_change: Arc<dyn Fn(Option<Vec<String>>) + Send + Sync>,
    pub on_persist: Arc<dyn Fn(Option<Vec<String>>) + Send + Sync>,
    pub on_cancel: Arc<dyn Fn() + Send + Sync>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshStatusKind {
    Muted,
    Success,
    Warning,
}

struct ScopedModelsState {
    models_by_id: IndexMap<String, Model>,
    all_ids: Vec<String>,
    enabled_ids: EnabledIds,
    filtered_items: Vec<ModelItem>,
    selected_index: usize,
    focused: bool,
    is_dirty: bool,
}

struct ScopedModelsInner {
    container: Container,
    search_input: Input,
    list_container: Container,
    footer_text: Text,
    refresh_status_text: Option<Text>,
    callbacks: ModelsCallbacks,
    max_visible: usize,
    state: Mutex<ScopedModelsState>,
}

#[derive(Clone)]
pub struct ScopedModelsSelectorComponent {
    inner: Arc<ScopedModelsInner>,
}

impl ScopedModelsSelectorComponent {
    pub fn new(config: ModelsConfig, callbacks: ModelsCallbacks) -> Self {
        let mut models_by_id = IndexMap::new();
        let mut all_ids = Vec::with_capacity(config.all_models.len());
        for model in config.all_models {
            let full_id = format!("{}/{}", model.provider, model.id);
            models_by_id.insert(full_id.clone(), model);
            all_ids.push(full_id);
        }

        let enabled_ids = config.enabled_model_ids.clone();
        let filtered_items = Self::build_items(&enabled_ids, &all_ids, &models_by_id);

        let container = Container::new();
        let t = theme();
        container.add_child(Arc::new(DynamicBorder::default()));
        container.add_child(Arc::new(Spacer::new(Some(1))));
        container.add_child(Arc::new(Text::new(
            Some(&t.fg(ThemeColor::Accent, &t.bold("Model Configuration"))),
            Some(0),
            Some(0),
            None,
        )));
        container.add_child(Arc::new(Text::new(
            Some(&t.fg(
                ThemeColor::Muted,
                &format!(
                    "Session-only. {} to save to settings.",
                    key_display_text(&"app.models.save".to_string())
                ),
            )),
            Some(0),
            Some(0),
            None,
        )));
        container.add_child(Arc::new(Spacer::new(Some(1))));

        let search_input = Input::new(None);
        container.add_child(Arc::new(search_input.clone()));
        container.add_child(Arc::new(Spacer::new(Some(1))));

        let list_container = Container::new();
        container.add_child(Arc::new(list_container.clone()));
        container.add_child(Arc::new(Spacer::new(Some(1))));

        let refresh_status_text = config.refresh_status.as_ref().map(|status| {
            let txt = Text::new(
                Some(&t.fg(ThemeColor::Muted, &format!("  {status}"))),
                Some(0),
                Some(0),
                None,
            );
            container.add_child(Arc::new(txt.clone()));
            txt
        });

        let footer_text = Text::new(
            Some(&Self::format_footer_text(&enabled_ids, &models_by_id, &all_ids, false)),
            Some(0),
            Some(0),
            None,
        );
        container.add_child(Arc::new(footer_text.clone()));
        container.add_child(Arc::new(DynamicBorder::default()));

        let comp = Self {
            inner: Arc::new(ScopedModelsInner {
                container,
                search_input,
                list_container,
                footer_text,
                refresh_status_text,
                callbacks,
                max_visible: 8,
                state: Mutex::new(ScopedModelsState {
                    models_by_id,
                    all_ids,
                    enabled_ids,
                    filtered_items,
                    selected_index: 0,
                    focused: false,
                    is_dirty: false,
                }),
            }),
        };
        comp.update_list();
        comp
    }

    fn build_items(
        enabled_ids: &EnabledIds,
        all_ids: &[String],
        models_by_id: &IndexMap<String, Model>,
    ) -> Vec<ModelItem> {
        get_sorted_ids(enabled_ids, all_ids)
            .into_iter()
            .map(|id| ModelItem {
                model: models_by_id.get(&id).cloned(),
                enabled: is_enabled(enabled_ids, &id),
                full_id: id,
            })
            .collect()
    }

    fn format_footer_text(
        enabled_ids: &EnabledIds,
        models_by_id: &IndexMap<String, Model>,
        all_ids: &[String],
        is_dirty: bool,
    ) -> String {
        let enabled_count = enabled_ids
            .as_ref()
            .map(|ids| ids.iter().filter(|id| models_by_id.contains_key(*id)).count())
            .unwrap_or(all_ids.len());
        let unavailable_count = enabled_ids
            .as_ref()
            .map(|ids| ids.iter().filter(|id| !models_by_id.contains_key(*id)).count())
            .unwrap_or(0);
        let count_text = if enabled_ids.is_none() {
            "all enabled".to_string()
        } else if unavailable_count > 0 {
            format!(
                "{enabled_count}/{} enabled · {unavailable_count} unavailable",
                all_ids.len()
            )
        } else {
            format!("{enabled_count}/{} enabled", all_ids.len())
        };
        let parts = [
            format!("{} toggle", key_display_text(&"tui.select.confirm".to_string())),
            format!("{} all", key_display_text(&"app.models.enableAll".to_string())),
            format!("{} clear", key_display_text(&"app.models.clearAll".to_string())),
            format!(
                "{} provider",
                key_display_text(&"app.models.toggleProvider".to_string())
            ),
            format!(
                "{}/{} reorder",
                key_display_text(&"app.models.reorderUp".to_string()),
                key_display_text(&"app.models.reorderDown".to_string())
            ),
            format!("{} save", key_display_text(&"app.models.save".to_string())),
            count_text,
        ];
        let t = theme();
        let base = t.fg(ThemeColor::Dim, &format!("  {}", parts.join(" · ")));
        if is_dirty {
            format!("{base} {}", t.fg(ThemeColor::Warning, "(unsaved)"))
        } else {
            base
        }
    }

    pub fn update_models(&self, models: &[Model], enabled_model_ids: Option<Option<&[String]>>) {
        let selected_id = {
            let mut state = self.inner.state.lock().unwrap();
            let selected_id = state
                .filtered_items
                .get(state.selected_index)
                .map(|i| i.full_id.clone());
            if let Some(next_enabled) = enabled_model_ids {
                state.enabled_ids = next_enabled.map(|s| s.to_vec());
            }
            state.models_by_id.clear();
            state.all_ids.clear();
            for model in models {
                let full_id = format!("{}/{}", model.provider, model.id);
                state.models_by_id.insert(full_id.clone(), model.clone());
                state.all_ids.push(full_id);
            }
            selected_id
        };
        self.refresh();
        if let Some(sid) = selected_id {
            let mut updated = false;
            {
                let mut state = self.inner.state.lock().unwrap();
                if let Some(pos) = state.filtered_items.iter().position(|item| item.full_id == sid) {
                    state.selected_index = pos;
                    updated = true;
                }
            }
            if updated {
                self.update_list();
            }
        }
    }

    pub fn set_refresh_status(&self, message: &str, kind: RefreshStatusKind) {
        if let Some(ref txt) = self.inner.refresh_status_text {
            let color = match kind {
                RefreshStatusKind::Muted => ThemeColor::Muted,
                RefreshStatusKind::Success => ThemeColor::Success,
                RefreshStatusKind::Warning => ThemeColor::Warning,
            };
            txt.set_text(&theme().fg(color, &format!("  {message}")));
        }
    }

    fn refresh(&self) {
        let query = self.inner.search_input.get_value();
        let footer = {
            let mut state = self.inner.state.lock().unwrap();
            let items = Self::build_items(&state.enabled_ids, &state.all_ids, &state.models_by_id);
            let filtered = if query.is_empty() {
                items
            } else {
                fuzzy_filter(
                    &items,
                    &query,
                    Arc::new(|item: &ModelItem| match &item.model {
                        Some(m) => get_model_search_text(&m.id, &m.provider, Some(&m.name)),
                        None => item.full_id.clone(),
                    }),
                )
            };
            state.filtered_items = filtered;
            if state.filtered_items.is_empty() {
                state.selected_index = 0;
            } else {
                state.selected_index = state.selected_index.min(state.filtered_items.len() - 1);
            }
            Self::format_footer_text(&state.enabled_ids, &state.models_by_id, &state.all_ids, state.is_dirty)
        };
        self.update_list();
        self.inner.footer_text.set_text(&footer);
    }

    fn notify_change(&self) {
        let enabled = self.inner.state.lock().unwrap().enabled_ids.clone();
        (self.inner.callbacks.on_change)(enabled);
    }

    fn update_list(&self) {
        self.inner.list_container.clear();
        let (filtered_items, selected_index) = {
            let state = self.inner.state.lock().unwrap();
            (state.filtered_items.clone(), state.selected_index)
        };
        let t = theme();

        if filtered_items.is_empty() {
            self.inner.list_container.add_child(Arc::new(Text::new(
                Some(&t.fg(ThemeColor::Muted, "  No matching models")),
                Some(0),
                Some(0),
                None,
            )));
            return;
        }

        let len = filtered_items.len();
        let max_visible = self.inner.max_visible;
        let max_start = len.saturating_sub(max_visible);
        let start_index = selected_index.saturating_sub(max_visible / 2).min(max_start);
        let end_index = (start_index + max_visible).min(len);

        for (i, item) in filtered_items.iter().enumerate().take(end_index).skip(start_index) {
            let is_selected = i == selected_index;
            let prefix = if is_selected {
                t.fg(ThemeColor::Accent, "→ ")
            } else {
                "  ".to_string()
            };
            let id = item
                .model
                .as_ref()
                .map(|m| m.id.as_str())
                .unwrap_or(item.full_id.as_str());
            let styled_id = if item.model.is_some() {
                id.to_string()
            } else {
                t.strikethrough(id)
            };
            let model_text = if is_selected {
                t.fg(ThemeColor::Accent, &styled_id)
            } else {
                styled_id
            };
            let name_text = item
                .model
                .as_ref()
                .and_then(|m| get_distinct_model_name(&m.id, Some(&m.name)))
                .map(|distinct| t.fg(ThemeColor::Muted, &format!(" ({distinct})")))
                .unwrap_or_default();
            let provider_badge = t.fg(
                ThemeColor::Muted,
                &item
                    .model
                    .as_ref()
                    .map(|m| format!(" [{}]", m.provider))
                    .unwrap_or_else(|| " [unavailable]".to_string()),
            );
            let status = if item.model.is_some() && item.enabled {
                t.fg(ThemeColor::Accent, "✓ ")
            } else {
                "  ".to_string()
            };
            let row = format!("{prefix}{status}{model_text}{name_text}{provider_badge}");
            self.inner
                .list_container
                .add_child(Arc::new(Text::new(Some(&row), Some(0), Some(0), None)));
        }

        if start_index > 0 || end_index < len {
            let scroll = t.fg(ThemeColor::Muted, &format!("  ({}/{})", selected_index + 1, len));
            self.inner
                .list_container
                .add_child(Arc::new(Text::new(Some(&scroll), Some(0), Some(0), None)));
        }

        if let Some(selected) = filtered_items.get(selected_index) {
            self.inner.list_container.add_child(Arc::new(Spacer::new(Some(1))));
            let desc = match &selected.model {
                Some(m) => format!("  Model Name: {}", m.name),
                None => "  Model unavailable".to_string(),
            };
            self.inner.list_container.add_child(Arc::new(Text::new(
                Some(&t.fg(ThemeColor::Muted, &desc)),
                Some(0),
                Some(0),
                None,
            )));
        }
    }

    pub fn focused(&self) -> bool {
        self.inner.state.lock().unwrap().focused
    }

    pub fn set_focused(&self, value: bool) {
        self.inner.state.lock().unwrap().focused = value;
        self.inner.search_input.set_focused(value);
    }

    pub fn get_search_input(&self) -> &Input {
        &self.inner.search_input
    }

    pub fn handle_input(&self, data: &str) {
        let kb = get_keybindings();

        if kb.matches(data, "tui.select.up") {
            let empty = {
                let mut state = self.inner.state.lock().unwrap();
                if state.filtered_items.is_empty() {
                    true
                } else {
                    state.selected_index = if state.selected_index == 0 {
                        state.filtered_items.len() - 1
                    } else {
                        state.selected_index - 1
                    };
                    false
                }
            };
            if !empty {
                self.update_list();
            }
            return;
        }
        if kb.matches(data, "tui.select.down") {
            let empty = {
                let mut state = self.inner.state.lock().unwrap();
                if state.filtered_items.is_empty() {
                    true
                } else {
                    state.selected_index = if state.selected_index == state.filtered_items.len() - 1 {
                        0
                    } else {
                        state.selected_index + 1
                    };
                    false
                }
            };
            if !empty {
                self.update_list();
            }
            return;
        }
        let page_index = {
            let state = self.inner.state.lock().unwrap();
            get_select_page_index(
                &kb,
                data,
                state.selected_index,
                state.filtered_items.len(),
                self.inner.max_visible,
            )
        };
        if let Some(next_idx) = page_index {
            let empty = {
                let mut state = self.inner.state.lock().unwrap();
                if state.filtered_items.is_empty() {
                    true
                } else {
                    state.selected_index = next_idx;
                    false
                }
            };
            if !empty {
                self.update_list();
            }
            return;
        }

        let reorder_up = kb.matches(data, "app.models.reorderUp");
        let reorder_down = kb.matches(data, "app.models.reorderDown");
        if reorder_up || reorder_down {
            let moved = {
                let mut state = self.inner.state.lock().unwrap();
                let mut moved = false;
                if let Some(ref enabled) = state.enabled_ids
                    && let Some(item) = state.filtered_items.get(state.selected_index).cloned()
                    && enabled.contains(&item.full_id)
                {
                    let delta: isize = if reorder_up { -1 } else { 1 };
                    if let Some(current_index) = enabled.iter().position(|x| x == &item.full_id) {
                        let new_index = current_index as isize + delta;
                        if new_index >= 0 && (new_index as usize) < enabled.len() {
                            state.enabled_ids = move_id(&state.enabled_ids, &item.full_id, delta);
                            state.is_dirty = true;
                            state.selected_index = (state.selected_index as isize + delta).max(0) as usize;
                            moved = true;
                        }
                    }
                }
                moved
            };
            if moved {
                self.refresh();
                self.notify_change();
            }
            return;
        }

        if kb.matches(data, "tui.select.confirm") {
            let changed = {
                let mut state = self.inner.state.lock().unwrap();
                if let Some(item) = state.filtered_items.get(state.selected_index).cloned() {
                    state.enabled_ids = toggle_id(&state.enabled_ids, &state.all_ids, &item.full_id);
                    state.is_dirty = true;
                    true
                } else {
                    false
                }
            };
            if changed {
                self.refresh();
                self.notify_change();
            }
            return;
        }

        if kb.matches(data, "app.models.enableAll") {
            let query = self.inner.search_input.get_value();
            {
                let mut state = self.inner.state.lock().unwrap();
                let targets: Option<Vec<String>> = if query.is_empty() {
                    None
                } else {
                    Some(state.filtered_items.iter().map(|i| i.full_id.clone()).collect())
                };
                state.enabled_ids = enable_all(&state.enabled_ids, &state.all_ids, targets.as_deref());
                state.is_dirty = true;
            }
            self.refresh();
            self.notify_change();
            return;
        }

        if kb.matches(data, "app.models.clearAll") {
            let query = self.inner.search_input.get_value();
            {
                let mut state = self.inner.state.lock().unwrap();
                let targets: Option<Vec<String>> = if query.is_empty() {
                    None
                } else {
                    Some(state.filtered_items.iter().map(|i| i.full_id.clone()).collect())
                };
                state.enabled_ids = clear_all(&state.enabled_ids, &state.all_ids, targets.as_deref());
                state.is_dirty = true;
            }
            self.refresh();
            self.notify_change();
            return;
        }

        if kb.matches(data, "app.models.toggleProvider") {
            let changed = {
                let mut state = self.inner.state.lock().unwrap();
                if let Some(provider) = state
                    .filtered_items
                    .get(state.selected_index)
                    .and_then(|i| i.model.as_ref().map(|m| m.provider.clone()))
                {
                    let provider_ids: Vec<String> = state
                        .all_ids
                        .iter()
                        .filter(|id| state.models_by_id.get(*id).is_some_and(|m| m.provider == provider))
                        .cloned()
                        .collect();
                    let all_provider_enabled = provider_ids.iter().all(|id| is_enabled(&state.enabled_ids, id));
                    state.enabled_ids = if all_provider_enabled {
                        clear_all(&state.enabled_ids, &state.all_ids, Some(&provider_ids))
                    } else {
                        enable_all(&state.enabled_ids, &state.all_ids, Some(&provider_ids))
                    };
                    state.is_dirty = true;
                    true
                } else {
                    false
                }
            };
            if changed {
                self.refresh();
                self.notify_change();
            }
            return;
        }

        if kb.matches(data, "app.models.save") {
            let (enabled, footer) = {
                let mut state = self.inner.state.lock().unwrap();
                state.is_dirty = false;
                (
                    state.enabled_ids.clone(),
                    Self::format_footer_text(&state.enabled_ids, &state.models_by_id, &state.all_ids, false),
                )
            };
            (self.inner.callbacks.on_persist)(enabled);
            self.inner.footer_text.set_text(&footer);
            return;
        }

        if matches_key(data, &Key::ctrl("c")) {
            if !self.inner.search_input.get_value().is_empty() {
                self.inner.search_input.set_value("");
                self.refresh();
            } else {
                (self.inner.callbacks.on_cancel)();
            }
            return;
        }

        if matches_key(data, Key::escape) {
            (self.inner.callbacks.on_cancel)();
            return;
        }

        self.inner.search_input.handle_input(data);
        self.refresh();
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        self.inner.container.render(width)
    }

    pub fn invalidate(&self) {
        self.inner.container.invalidate();
    }
}

impl Focusable for ScopedModelsSelectorComponent {
    fn focused(&self) -> bool {
        ScopedModelsSelectorComponent::focused(self)
    }

    fn set_focused(&self, focused: bool) {
        ScopedModelsSelectorComponent::set_focused(self, focused)
    }
}

impl Component for ScopedModelsSelectorComponent {
    fn render(&self, width: usize) -> Vec<String> {
        ScopedModelsSelectorComponent::render(self, width)
    }

    fn handle_input(&self, data: &str) {
        ScopedModelsSelectorComponent::handle_input(self, data)
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.container.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        ScopedModelsSelectorComponent::invalidate(self)
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
