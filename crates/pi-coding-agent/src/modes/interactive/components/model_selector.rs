//! Port of packages/coding-agent/src/modes/interactive/components/model-selector.ts

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use indexmap::IndexSet;
use pi_ai::Model;
use pi_js::abort::AbortController;
use pi_js::intl::locale_compare;
use pi_js::time::{Timeout, set_timeout};
use pi_tui::{
    Component, Container, Focusable, Input, Spacer, TUI, Text, TuiMouseEvent, TuiMouseEventResult, fuzzy_filter,
    get_keybindings,
};

use super::super::model_catalog_refresh::{ModelCatalogRuntime, refresh_model_catalogs};
use super::super::model_search::{get_distinct_model_name, get_model_selector_search_text};
use super::super::theme::theme::{ThemeColor, theme};
use super::dynamic_border::DynamicBorder;
use super::keybinding_hints::{get_select_page_index, key_display_text, key_hint};

const MAX_VISIBLE: usize = 10;

#[derive(Clone)]
struct ModelItem {
    provider: String,
    id: String,
    model: Model,
}

#[derive(Clone, Debug)]
pub struct ScopedModelItem {
    pub model: Model,
    pub thinking_level: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefaultModelReference {
    pub provider: String,
    pub id: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ModelScope {
    All,
    Scoped,
}

fn models_equal(a: Option<&Model>, b: &Model) -> bool {
    let Some(a) = a else {
        return false;
    };
    a.r#type == b.r#type && a.provider == b.provider && a.id == b.id
}

struct ModelSelectorState {
    all_models: Vec<ModelItem>,
    scoped_models: Vec<ScopedModelItem>,
    scoped_model_items: Vec<ModelItem>,
    active_models: Vec<ModelItem>,
    filtered_models: Vec<ModelItem>,
    selected_index: usize,
    error_message: Option<String>,
    refresh_status_message: String,
    refresh_status_success: bool,
    scope: ModelScope,
    focused: bool,
    refresh_timeout: Option<Timeout>,
}

struct ModelSelectorInner {
    container: Container,
    search_input: Input,
    list_container: Container,
    scope_text: Option<Text>,
    scope_hint_text: Option<Text>,
    tui: Arc<dyn TUI + Send + Sync>,
    current_model: Option<Model>,
    model_runtime: Arc<dyn ModelCatalogRuntime>,
    on_select: Arc<dyn Fn(Model) + Send + Sync>,
    on_select_as_default: Option<Arc<dyn Fn(Model) + Send + Sync>>,
    on_cancel: Arc<dyn Fn() + Send + Sync>,
    default_model: Option<DefaultModelReference>,
    refresh_abort: Arc<AbortController>,
    closed: AtomicBool,
    state: Mutex<ModelSelectorState>,
}

impl Drop for ModelSelectorInner {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
        if let Ok(mut guard) = self.state.lock()
            && let Some(timer) = guard.refresh_timeout.take()
        {
            timer.clear();
        }
        self.refresh_abort.abort(None);
    }
}

/// Component that renders a model selector with search.
#[derive(Clone)]
pub struct ModelSelectorComponent {
    inner: Arc<ModelSelectorInner>,
}

impl ModelSelectorComponent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tui: Arc<dyn TUI + Send + Sync>,
        current_model: Option<Model>,
        model_runtime: Arc<dyn ModelCatalogRuntime>,
        scoped_models: Vec<ScopedModelItem>,
        on_select: Arc<dyn Fn(Model) + Send + Sync>,
        on_cancel: Arc<dyn Fn() + Send + Sync>,
        initial_search_input: Option<&str>,
        on_select_as_default: Option<Arc<dyn Fn(Model) + Send + Sync>>,
        default_model: Option<DefaultModelReference>,
    ) -> Self {
        let initial_scope = if scoped_models.is_empty() {
            ModelScope::All
        } else {
            ModelScope::Scoped
        };
        let t = theme();

        let container = Container::new();
        container.add_child(Arc::new(DynamicBorder::default()));
        container.add_child(Arc::new(Spacer::new(Some(1))));

        let (scope_text, scope_hint_text) = if !scoped_models.is_empty() {
            let st = Text::new(Some(&Self::format_scope_text(initial_scope)), Some(0), Some(0), None);
            let sht = Text::new(Some(&Self::format_scope_hint_text()), Some(0), Some(0), None);
            container.add_child(Arc::new(st.clone()));
            container.add_child(Arc::new(sht.clone()));
            (Some(st), Some(sht))
        } else {
            container.add_child(Arc::new(Text::new(
                Some(&t.fg(
                    ThemeColor::Warning,
                    "Only showing models from configured providers. Use /login to add providers.",
                )),
                Some(0),
                Some(0),
                None,
            )));
            (None, None)
        };
        container.add_child(Arc::new(Spacer::new(Some(1))));

        let search_input = Input::new(None);
        if let Some(init) = initial_search_input
            && !init.is_empty()
        {
            search_input.set_value(init);
        }
        container.add_child(Arc::new(search_input.clone()));
        container.add_child(Arc::new(Spacer::new(Some(1))));

        let list_container = Container::new();
        container.add_child(Arc::new(list_container.clone()));
        container.add_child(Arc::new(Spacer::new(Some(1))));

        if on_select_as_default.is_some() {
            let hint = format!(
                "  {} to select · {} to set as default · {} to cancel",
                key_display_text(&"tui.select.confirm".to_string()),
                key_display_text(&"app.models.save".to_string()),
                key_display_text(&"tui.select.cancel".to_string()),
            );
            container.add_child(Arc::new(Text::new(
                Some(&t.fg(ThemeColor::Dim, &hint)),
                Some(0),
                Some(0),
                None,
            )));
        }

        container.add_child(Arc::new(DynamicBorder::default()));

        let comp = Self {
            inner: Arc::new(ModelSelectorInner {
                container,
                search_input,
                list_container,
                scope_text,
                scope_hint_text,
                tui,
                current_model,
                model_runtime,
                on_select,
                on_select_as_default,
                on_cancel,
                default_model,
                refresh_abort: Arc::new(AbortController::new()),
                closed: AtomicBool::new(false),
                state: Mutex::new(ModelSelectorState {
                    all_models: Vec::new(),
                    scoped_models,
                    scoped_model_items: Vec::new(),
                    active_models: Vec::new(),
                    filtered_models: Vec::new(),
                    selected_index: 0,
                    error_message: None,
                    refresh_status_message: "Refreshing model catalogs…".to_string(),
                    refresh_status_success: false,
                    scope: initial_scope,
                    focused: false,
                    refresh_timeout: None,
                }),
            }),
        };

        let weak = Arc::downgrade(&comp.inner);
        comp.inner.search_input.set_on_submit(Some(Arc::new(move |_| {
            if let Some(inner) = weak.upgrade() {
                let comp = ModelSelectorComponent { inner };
                let selected = {
                    let s = comp.inner.state.lock().unwrap();
                    s.filtered_models.get(s.selected_index).map(|m| m.model.clone())
                };
                if let Some(model) = selected {
                    comp.handle_select(model);
                }
            }
        })));

        comp.load_models_from_snapshot();
        if let Some(init) = initial_search_input
            && !init.is_empty()
        {
            comp.filter_models(init, None);
        } else {
            comp.update_list();
        }
        comp.inner.tui.request_render(None);

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let comp_bg = comp.clone();
            handle.spawn(async move {
                comp_bg.refresh_models().await;
            });
        }

        comp
    }

    pub fn dispose(&self) {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(timer) = self.inner.state.lock().unwrap().refresh_timeout.take() {
            timer.clear();
        }
        self.inner.refresh_abort.abort(None);
    }

    fn format_scope_text(scope: ModelScope) -> String {
        let t = theme();
        let all_text = if scope == ModelScope::All {
            t.fg(ThemeColor::Accent, "all")
        } else {
            t.fg(ThemeColor::Muted, "all")
        };
        let scoped_text = if scope == ModelScope::Scoped {
            t.fg(ThemeColor::Accent, "scoped")
        } else {
            t.fg(ThemeColor::Muted, "scoped")
        };
        format!(
            "{}{}{}{}",
            t.fg(ThemeColor::Muted, "Scope: "),
            all_text,
            t.fg(ThemeColor::Muted, " | "),
            scoped_text
        )
    }

    fn format_scope_hint_text() -> String {
        format!(
            "{}{}",
            key_hint(&"tui.input.tab".to_string(), "scope"),
            theme().fg(ThemeColor::Muted, " (all/scoped)")
        )
    }

    fn is_default_model(&self, model: &Model) -> bool {
        self.inner
            .default_model
            .as_ref()
            .is_some_and(|d| d.provider.as_str() == model.provider && d.id == model.id)
    }

    fn is_default_search(query: &str) -> bool {
        let normalized = query.trim().to_lowercase();
        !normalized.is_empty() && "default".starts_with(&normalized)
    }

    fn sort_models(&self, mut models: Vec<ModelItem>) -> Vec<ModelItem> {
        let current = self.inner.current_model.as_ref();
        models.sort_by(|a, b| {
            let a_cur = models_equal(current, &a.model);
            let b_cur = models_equal(current, &b.model);
            if a_cur != b_cur {
                return b_cur.cmp(&a_cur);
            }
            let a_def = self.is_default_model(&a.model);
            let b_def = self.is_default_model(&b.model);
            if a_def != b_def {
                return b_def.cmp(&a_def);
            }
            locale_compare(&a.provider, &b.provider)
        });
        models
    }

    fn load_models_from_snapshot(&self) {
        let models: Vec<ModelItem> = self
            .inner
            .model_runtime
            .get_available_snapshot()
            .into_iter()
            .map(|model| ModelItem {
                provider: model.provider.to_string(),
                id: model.id.clone(),
                model,
            })
            .collect();
        let sorted_all = self.sort_models(models);

        let mut state = self.inner.state.lock().unwrap();
        state.all_models = sorted_all;
        for scoped in &mut state.scoped_models {
            if let Some(refreshed) = self
                .inner
                .model_runtime
                .get_model(&scoped.model.provider, &scoped.model.id)
            {
                scoped.model = refreshed;
            }
        }
        state.scoped_model_items = state
            .scoped_models
            .iter()
            .map(|s| ModelItem {
                provider: s.model.provider.to_string(),
                id: s.model.id.clone(),
                model: s.model.clone(),
            })
            .collect();
        state.active_models = if state.scope == ModelScope::Scoped {
            state.scoped_model_items.clone()
        } else {
            state.all_models.clone()
        };
        state.filtered_models = state.active_models.clone();
        let current_idx = state
            .filtered_models
            .iter()
            .position(|item| models_equal(self.inner.current_model.as_ref(), &item.model));
        state.selected_index = match current_idx {
            Some(idx) => idx,
            None => state.selected_index.min(state.filtered_models.len().saturating_sub(1)),
        };
    }

    async fn refresh_models(&self) {
        let timeout_ms: u64 = 15_000;
        let timed_out = Arc::new(AtomicBool::new(false));
        let timed_out_c = Arc::clone(&timed_out);
        let abort_c = Arc::clone(&self.inner.refresh_abort);
        let timer = set_timeout(timeout_ms, move || {
            timed_out_c.store(true, Ordering::SeqCst);
            abort_c.abort(None);
        });
        self.inner.state.lock().unwrap().refresh_timeout = Some(timer);

        let outcome =
            refresh_model_catalogs(Arc::clone(&self.inner.model_runtime), self.inner.refresh_abort.signal()).await;

        if let Some(timer) = self.inner.state.lock().unwrap().refresh_timeout.take() {
            timer.clear();
        }
        if self.inner.closed.load(Ordering::SeqCst) {
            return;
        }

        let (selected_model, query) = {
            let state = self.inner.state.lock().unwrap();
            (
                state.filtered_models.get(state.selected_index).map(|m| m.model.clone()),
                self.inner.search_input.get_value(),
            )
        };

        match outcome {
            Ok(result) => {
                let mut status_msg = String::new();
                let mut status_ok = false;

                let err_msg = if result.aborted && timed_out.load(Ordering::SeqCst) {
                    Some("Model refresh timed out; showing cached models.".to_string())
                } else if result.errors.len() == 1 {
                    let first_key = result.errors.keys().next().map(String::as_str).unwrap_or("");
                    Some(format!("Could not refresh {first_key}; showing cached models."))
                } else if result.errors.len() > 1 {
                    let keys: Vec<&str> = result.errors.keys().map(String::as_str).collect();
                    Some(format!(
                        "Could not refresh {} model catalogs ({}); showing cached models.",
                        result.errors.len(),
                        keys.join(", ")
                    ))
                } else {
                    let err = self.inner.model_runtime.get_error();
                    if err.is_none() {
                        status_msg = "Model catalogs refreshed.".to_string();
                        status_ok = true;
                    }
                    err
                };

                {
                    let mut state = self.inner.state.lock().unwrap();
                    state.refresh_status_message = status_msg;
                    state.refresh_status_success = status_ok;
                    state.error_message = err_msg;
                }
                self.load_models_from_snapshot();
                self.filter_models(&query, selected_model.as_ref());
            }
            Err(err) => {
                {
                    let mut state = self.inner.state.lock().unwrap();
                    state.refresh_status_message.clear();
                    state.error_message = Some(if timed_out.load(Ordering::SeqCst) {
                        "Model refresh timed out; showing cached models.".to_string()
                    } else {
                        format!("Could not refresh model catalogs: {err}")
                    });
                }
                self.update_list();
            }
        }
        self.inner.tui.request_render(None);
    }

    fn set_scope(&self, scope: ModelScope) {
        let query = self.inner.search_input.get_value();
        {
            let mut state = self.inner.state.lock().unwrap();
            if state.scope == scope {
                return;
            }
            state.scope = scope;
            state.active_models = if scope == ModelScope::Scoped {
                state.scoped_model_items.clone()
            } else {
                state.all_models.clone()
            };
            let current_idx = state
                .active_models
                .iter()
                .position(|item| models_equal(self.inner.current_model.as_ref(), &item.model));
            state.selected_index = current_idx.unwrap_or(0);
        }
        self.filter_models(&query, None);
        if let Some(ref scope_text) = self.inner.scope_text {
            scope_text.set_text(&Self::format_scope_text(scope));
        }
    }

    fn filter_models(&self, query: &str, preferred_model: Option<&Model>) {
        let active_models = self.inner.state.lock().unwrap().active_models.clone();
        let default_model_ref = self.inner.default_model.clone();
        let filtered_models = if !query.is_empty() {
            let filtered = fuzzy_filter(
                &active_models,
                query,
                Arc::new(move |item: &ModelItem| {
                    let is_def = default_model_ref
                        .as_ref()
                        .is_some_and(|d| d.provider.as_str() == item.model.provider && d.id == item.model.id);
                    let default_text = if is_def { " default" } else { "" };
                    let search = get_model_selector_search_text(&item.id, &item.provider, Some(&item.model.name));
                    format!("{search}{default_text}")
                }),
            );
            if Self::is_default_search(query) {
                let default_items: Vec<ModelItem> = active_models
                    .iter()
                    .filter(|item| self.is_default_model(&item.model))
                    .cloned()
                    .collect();
                let default_keys: IndexSet<String> = default_items
                    .iter()
                    .map(|item| format!("{}\0{}", item.provider, item.id))
                    .collect();
                let mut combined = default_items;
                for item in filtered {
                    let key = format!("{}\0{}", item.provider, item.id);
                    if !default_keys.contains(&key) {
                        combined.push(item);
                    }
                }
                combined
            } else {
                filtered
            }
        } else {
            active_models
        };

        {
            let mut state = self.inner.state.lock().unwrap();
            let preferred_index = preferred_model.and_then(|pm| {
                filtered_models
                    .iter()
                    .position(|item| models_equal(Some(pm), &item.model))
            });
            state.filtered_models = filtered_models;
            state.selected_index = if let Some(idx) = preferred_index {
                idx
            } else if !query.is_empty() {
                0
            } else {
                state.selected_index.min(state.filtered_models.len().saturating_sub(1))
            };
        }
        self.update_list();
    }

    fn update_list(&self) {
        self.inner.list_container.clear();
        let (filtered, selected_index, error_message, refresh_status_message, refresh_status_success) = {
            let state = self.inner.state.lock().unwrap();
            (
                state.filtered_models.clone(),
                state.selected_index,
                state.error_message.clone(),
                state.refresh_status_message.clone(),
                state.refresh_status_success,
            )
        };
        let t = theme();
        let len = filtered.len();
        let max_start = len.saturating_sub(MAX_VISIBLE);
        let start_index = selected_index.saturating_sub(MAX_VISIBLE / 2).min(max_start);
        let end_index = (start_index + MAX_VISIBLE).min(len);

        for (i, item) in filtered.iter().enumerate().take(end_index).skip(start_index) {
            let is_selected = i == selected_index;
            let is_current = models_equal(self.inner.current_model.as_ref(), &item.model);
            let default_badge = if self.is_default_model(&item.model) {
                t.fg(ThemeColor::Muted, " · default")
            } else {
                String::new()
            };
            let cursor = if is_selected {
                t.fg(ThemeColor::Accent, "→ ")
            } else {
                "  ".to_string()
            };
            let current_marker = if is_current {
                t.fg(ThemeColor::Accent, "✓ ")
            } else {
                "  ".to_string()
            };
            let model_text = if is_selected {
                t.fg(ThemeColor::Accent, &item.id)
            } else {
                item.id.clone()
            };
            let name_text = get_distinct_model_name(&item.id, Some(&item.model.name))
                .map(|name| t.fg(ThemeColor::Muted, &format!(" ({name})")))
                .unwrap_or_default();
            let provider_badge = t.fg(ThemeColor::Muted, &format!("[{}]", item.provider));
            let line = format!("{cursor}{current_marker}{model_text}{name_text} {provider_badge}{default_badge}");
            self.inner
                .list_container
                .add_child(Arc::new(Text::new(Some(&line), Some(0), Some(0), None)));
        }

        if start_index > 0 || end_index < len {
            let scroll = t.fg(ThemeColor::Muted, &format!("  ({}/{len})", selected_index + 1));
            self.inner
                .list_container
                .add_child(Arc::new(Text::new(Some(&scroll), Some(0), Some(0), None)));
        }

        if let Some(ref err) = error_message {
            for line in err.split('\n') {
                self.inner.list_container.add_child(Arc::new(Text::new(
                    Some(&t.fg(ThemeColor::Error, line)),
                    Some(0),
                    Some(0),
                    None,
                )));
            }
        } else if len == 0 {
            self.inner.list_container.add_child(Arc::new(Text::new(
                Some(&t.fg(ThemeColor::Muted, "  No matching models")),
                Some(0),
                Some(0),
                None,
            )));
        } else if let Some(selected) = filtered.get(selected_index) {
            self.inner.list_container.add_child(Arc::new(Spacer::new(Some(1))));
            self.inner.list_container.add_child(Arc::new(Text::new(
                Some(&t.fg(ThemeColor::Muted, &format!("  Model Name: {}", selected.model.name))),
                Some(0),
                Some(0),
                None,
            )));
        }

        if !refresh_status_message.is_empty() {
            let color = if refresh_status_success {
                ThemeColor::Success
            } else {
                ThemeColor::Muted
            };
            self.inner.list_container.add_child(Arc::new(Spacer::new(Some(1))));
            self.inner.list_container.add_child(Arc::new(Text::new(
                Some(&t.fg(color, &format!("  {refresh_status_message}"))),
                Some(0),
                Some(0),
                None,
            )));
        }
    }

    fn handle_select(&self, model: Model) {
        self.dispose();
        (self.inner.on_select)(model);
    }

    pub fn get_search_input(&self) -> &Input {
        &self.inner.search_input
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
        let (len, selected_index, has_scoped, current_scope) = {
            let state = self.inner.state.lock().unwrap();
            (
                state.filtered_models.len(),
                state.selected_index,
                !state.scoped_model_items.is_empty(),
                state.scope,
            )
        };

        if kb.matches(key_data, "tui.input.tab") {
            if has_scoped {
                let next = if current_scope == ModelScope::All {
                    ModelScope::Scoped
                } else {
                    ModelScope::All
                };
                self.set_scope(next);
                if let Some(ref sht) = self.inner.scope_hint_text {
                    sht.set_text(&Self::format_scope_hint_text());
                }
            }
            return;
        }

        let page_index = get_select_page_index(&kb, key_data, selected_index, len, MAX_VISIBLE);

        if kb.matches(key_data, "tui.select.up") {
            if len == 0 {
                return;
            }
            {
                let mut state = self.inner.state.lock().unwrap();
                state.selected_index = if state.selected_index == 0 {
                    len - 1
                } else {
                    state.selected_index - 1
                };
            }
            self.update_list();
        } else if kb.matches(key_data, "tui.select.down") {
            if len == 0 {
                return;
            }
            {
                let mut state = self.inner.state.lock().unwrap();
                state.selected_index = if state.selected_index + 1 >= len {
                    0
                } else {
                    state.selected_index + 1
                };
            }
            self.update_list();
        } else if let Some(next_idx) = page_index {
            if len == 0 {
                return;
            }
            self.inner.state.lock().unwrap().selected_index = next_idx;
            self.update_list();
        } else if kb.matches(key_data, "tui.select.confirm") {
            let selected = {
                let s = self.inner.state.lock().unwrap();
                s.filtered_models.get(s.selected_index).map(|m| m.model.clone())
            };
            if let Some(model) = selected {
                self.handle_select(model);
            }
        } else if kb.matches(key_data, "tui.select.cancel") {
            self.dispose();
            (self.inner.on_cancel)();
        } else if kb.matches(key_data, "app.models.save")
            && let Some(ref on_save) = self.inner.on_select_as_default
        {
            let selected = {
                let s = self.inner.state.lock().unwrap();
                s.filtered_models.get(s.selected_index).map(|m| m.model.clone())
            };
            if let Some(model) = selected {
                self.dispose();
                on_save(model);
            }
        } else {
            self.inner.search_input.handle_input(key_data);
            let query = self.inner.search_input.get_value();
            self.filter_models(&query, None);
        }
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        self.inner.container.render(width)
    }

    pub fn invalidate(&self) {
        self.inner.container.invalidate();
    }
}

impl Focusable for ModelSelectorComponent {
    fn focused(&self) -> bool {
        ModelSelectorComponent::focused(self)
    }

    fn set_focused(&self, focused: bool) {
        ModelSelectorComponent::set_focused(self, focused)
    }
}

impl Component for ModelSelectorComponent {
    fn render(&self, width: usize) -> Vec<String> {
        ModelSelectorComponent::render(self, width)
    }

    fn handle_input(&self, data: &str) {
        ModelSelectorComponent::handle_input(self, data)
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.container.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        ModelSelectorComponent::invalidate(self)
    }

    fn has_handle_input(&self) -> bool {
        true
    }

    fn captures_viewport_input(&self) -> bool {
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
