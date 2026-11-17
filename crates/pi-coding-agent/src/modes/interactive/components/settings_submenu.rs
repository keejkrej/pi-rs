//! Port of packages/coding-agent/src/modes/interactive/components/settings-submenu.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_tui::{
    Component, Container, Input, SelectItem, SelectList, SelectListLayoutOptions, Spacer, Text, TuiMouseEvent,
    TuiMouseEventResult, fuzzy_filter, get_keybindings,
};

use super::super::theme::theme::{ThemeColor, get_select_list_theme, theme};

const SUBMENU_SELECT_LIST_LAYOUT: SelectListLayoutOptions = SelectListLayoutOptions {
    min_primary_column_width: Some(12),
    max_primary_column_width: Some(32),
    truncate_primary: None,
};

#[derive(Clone, Default)]
pub struct SelectSubmenuOptions {
    pub searchable: bool,
    pub layout: Option<SelectListLayoutOptions>,
}

struct SelectSubmenuState {
    select_list: SelectList,
}

struct SelectSubmenuInner {
    container: Container,
    list_slot: Container,
    all_options: Vec<SelectItem>,
    list_layout: SelectListLayoutOptions,
    search_input: Option<Input>,
    on_select: Arc<dyn Fn(&str) + Send + Sync>,
    on_cancel: Arc<dyn Fn() + Send + Sync>,
    on_selection_change: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    state: Mutex<SelectSubmenuState>,
}

/// Single-step submenu that shows a titled select list.
/// With `searchable: true`, typing filters the list using fuzzy matching.
#[derive(Clone)]
pub struct SelectSubmenu {
    inner: Arc<SelectSubmenuInner>,
}

impl SelectSubmenu {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        title: &str,
        description: &str,
        options: Vec<SelectItem>,
        current_value: &str,
        on_select: Arc<dyn Fn(&str) + Send + Sync>,
        on_cancel: Arc<dyn Fn() + Send + Sync>,
        on_selection_change: Option<Arc<dyn Fn(&str) + Send + Sync>>,
        submenu_options: Option<SelectSubmenuOptions>,
    ) -> Self {
        let opts = submenu_options.unwrap_or_default();
        let list_layout = opts.layout.unwrap_or(SUBMENU_SELECT_LIST_LAYOUT);
        let t = theme();

        let container = Container::new();
        container.add_child(Arc::new(Text::new(
            Some(&t.bold(&t.fg(ThemeColor::Accent, title))),
            Some(0),
            Some(0),
            None,
        )));

        if !description.is_empty() {
            container.add_child(Arc::new(Spacer::new(Some(1))));
            container.add_child(Arc::new(Text::new(
                Some(&t.fg(ThemeColor::Muted, description)),
                Some(0),
                Some(0),
                None,
            )));
        }

        let search_input = if opts.searchable {
            container.add_child(Arc::new(Spacer::new(Some(1))));
            let input = Input::new(None);
            container.add_child(Arc::new(input.clone()));
            Some(input)
        } else {
            None
        };

        container.add_child(Arc::new(Spacer::new(Some(1))));

        let select_list = Self::build_select_list(
            &options,
            current_value,
            list_layout.clone(),
            &on_select,
            &on_cancel,
            &on_selection_change,
        );
        let list_slot = Container::new();
        list_slot.add_child(Arc::new(select_list.clone()));
        container.add_child(Arc::new(list_slot.clone()));

        container.add_child(Arc::new(Spacer::new(Some(1))));
        let hint = if opts.searchable {
            "  Type to filter \u{00b7} Enter to select \u{00b7} Esc to go back"
        } else {
            "  Enter to select \u{00b7} Esc to go back"
        };
        container.add_child(Arc::new(Text::new(
            Some(&t.fg(ThemeColor::Dim, hint)),
            Some(0),
            Some(0),
            None,
        )));

        let comp = Self {
            inner: Arc::new(SelectSubmenuInner {
                container,
                list_slot,
                all_options: options,
                list_layout,
                search_input: search_input.clone(),
                on_select,
                on_cancel,
                on_selection_change,
                state: Mutex::new(SelectSubmenuState { select_list }),
            }),
        };

        if let Some(input) = search_input {
            let weak = Arc::downgrade(&comp.inner);
            input.set_on_submit(Some(Arc::new(move |_| {
                if let Some(inner) = weak.upgrade() {
                    let list = inner.state.lock().unwrap().select_list.clone();
                    list.handle_input("\r");
                }
            })));
        }

        comp
    }

    fn build_select_list(
        options: &[SelectItem],
        preselect: &str,
        layout: SelectListLayoutOptions,
        on_select: &Arc<dyn Fn(&str) + Send + Sync>,
        on_cancel: &Arc<dyn Fn() + Send + Sync>,
        on_selection_change: &Option<Arc<dyn Fn(&str) + Send + Sync>>,
    ) -> SelectList {
        let max_visible = options.len().min(10).max(1) as i64;
        let list = SelectList::new(options.to_vec(), max_visible, get_select_list_theme(), Some(layout));
        if let Some(idx) = options.iter().position(|o| o.value == preselect) {
            list.set_selected_index(idx as i64);
        }
        let on_sel_c = Arc::clone(on_select);
        list.set_on_select(Some(Arc::new(move |item: &SelectItem| {
            on_sel_c(&item.value);
        })));
        let on_can_c = Arc::clone(on_cancel);
        list.set_on_cancel(Some(Arc::new(move || {
            on_can_c();
        })));
        if let Some(cb) = on_selection_change {
            let cb_c = Arc::clone(cb);
            list.set_on_selection_change(Some(Arc::new(move |item: &SelectItem| {
                cb_c(&item.value);
            })));
        }
        list
    }

    fn apply_filter(&self, query: &str) {
        let filtered = if query.is_empty() {
            self.inner.all_options.clone()
        } else {
            fuzzy_filter(
                &self.inner.all_options,
                query,
                Arc::new(|item: &SelectItem| format!("{} {}", item.label, item.description.as_deref().unwrap_or(""))),
            )
        };
        let new_list = Self::build_select_list(
            &filtered,
            "",
            self.inner.list_layout.clone(),
            &self.inner.on_select,
            &self.inner.on_cancel,
            &self.inner.on_selection_change,
        );
        self.inner.list_slot.clear();
        self.inner.list_slot.add_child(Arc::new(new_list.clone()));
        self.inner.state.lock().unwrap().select_list = new_list;
    }

    pub fn handle_input(&self, data: &str) {
        if let Some(ref input) = self.inner.search_input {
            let kb = get_keybindings();
            let is_nav = kb.matches(data, "tui.select.up")
                || kb.matches(data, "tui.select.down")
                || kb.matches(data, "tui.select.confirm")
                || kb.matches(data, "tui.select.cancel");
            if is_nav {
                let list = self.inner.state.lock().unwrap().select_list.clone();
                list.handle_input(data);
            } else {
                input.handle_input(data);
                let query = input.get_value();
                self.apply_filter(&query);
            }
        } else {
            let list = self.inner.state.lock().unwrap().select_list.clone();
            list.handle_input(data);
        }
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        self.inner.container.render(width)
    }

    pub fn invalidate(&self) {
        self.inner.container.invalidate();
    }
}

impl Component for SelectSubmenu {
    fn render(&self, width: usize) -> Vec<String> {
        SelectSubmenu::render(self, width)
    }

    fn handle_input(&self, data: &str) {
        SelectSubmenu::handle_input(self, data)
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.container.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        SelectSubmenu::invalidate(self)
    }

    fn has_handle_input(&self) -> bool {
        true
    }

    fn is_container_handle_mouse(&self) -> bool {
        true
    }

    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        Some(self.inner.container.children())
    }
}

pub type StepString = Arc<dyn Fn(&IndexMap<String, String>) -> String + Send + Sync>;

#[derive(Clone)]
pub enum StepText {
    Static(String),
    Dynamic(StepString),
}

impl StepText {
    fn resolve(&self, ctx: &IndexMap<String, String>) -> String {
        match self {
            Self::Static(s) => s.clone(),
            Self::Dynamic(f) => f(ctx),
        }
    }
}

/// One step in a [`SteppedSubmenu`].
#[derive(Clone)]
pub struct SteppedSubmenuStep {
    pub key: String,
    pub title: StepText,
    pub description: StepText,
    pub options: Arc<dyn Fn(&IndexMap<String, String>) -> Vec<SelectItem> + Send + Sync>,
    pub preselect: Option<Arc<dyn Fn(&IndexMap<String, String>) -> Option<String> + Send + Sync>>,
    pub searchable: bool,
    pub layout: Option<SelectListLayoutOptions>,
}

#[derive(Clone, Debug, Default)]
pub struct SteppedSubmenuOptions {
    pub start_at_step: Option<usize>,
    pub initial_context: Option<IndexMap<String, String>>,
    pub loop_on_complete: bool,
}

struct SteppedSubmenuState {
    active_component: Arc<dyn Component>,
    context: IndexMap<String, String>,
}

struct SteppedSubmenuInner {
    steps: Vec<SteppedSubmenuStep>,
    on_complete: Arc<dyn Fn(&IndexMap<String, String>) + Send + Sync>,
    on_cancel: Arc<dyn Fn() + Send + Sync>,
    opts: SteppedSubmenuOptions,
    state: Mutex<SteppedSubmenuState>,
}

/// Generic N-step submenu built on top of [`SelectSubmenu`].
#[derive(Clone)]
pub struct SteppedSubmenu {
    inner: Arc<SteppedSubmenuInner>,
}

impl SteppedSubmenu {
    pub fn new(
        steps: Vec<SteppedSubmenuStep>,
        on_complete: Arc<dyn Fn(&IndexMap<String, String>) + Send + Sync>,
        on_cancel: Arc<dyn Fn() + Send + Sync>,
        opts: Option<SteppedSubmenuOptions>,
    ) -> Self {
        let options = opts.unwrap_or_default();
        let start_step = options.start_at_step.unwrap_or(0);
        let initial_ctx = options.initial_context.clone().unwrap_or_default();
        let placeholder: Arc<dyn Component> = Arc::new(Spacer::new(Some(0)));

        let comp = Self {
            inner: Arc::new(SteppedSubmenuInner {
                steps,
                on_complete,
                on_cancel,
                opts: options,
                state: Mutex::new(SteppedSubmenuState {
                    active_component: placeholder,
                    context: initial_ctx,
                }),
            }),
        };
        let initial_comp = comp.build_step(start_step);
        comp.inner.state.lock().unwrap().active_component = initial_comp;
        comp
    }

    fn build_step(&self, step_index: usize) -> Arc<dyn Component> {
        let step = self.inner.steps[step_index].clone();
        let total = self.inner.steps.len();
        let step_label = if total > 1 {
            format!("Step {}/{} \u{00b7} ", step_index + 1, total)
        } else {
            String::new()
        };

        let ctx_snapshot = self.inner.state.lock().unwrap().context.clone();
        let title = step.title.resolve(&ctx_snapshot);
        let desc = step.description.resolve(&ctx_snapshot);
        let items = (step.options)(&ctx_snapshot);
        let preselect = step
            .preselect
            .as_ref()
            .and_then(|f| f(&ctx_snapshot))
            .unwrap_or_default();

        let self_fwd = self.clone();
        let step_key_fwd = step.key.clone();
        let on_select = Arc::new(move |value: &str| {
            {
                let mut state = self_fwd.inner.state.lock().unwrap();
                state.context.insert(step_key_fwd.clone(), value.to_string());
            }
            if step_index + 1 < total {
                let next = self_fwd.build_step(step_index + 1);
                self_fwd.inner.state.lock().unwrap().active_component = next;
            } else {
                let completed_ctx = self_fwd.inner.state.lock().unwrap().context.clone();
                (self_fwd.inner.on_complete)(&completed_ctx);
                if self_fwd.inner.opts.loop_on_complete {
                    self_fwd.inner.state.lock().unwrap().context.clear();
                    let next = self_fwd.build_step(0);
                    self_fwd.inner.state.lock().unwrap().active_component = next;
                } else {
                    (self_fwd.inner.on_cancel)();
                }
            }
        });

        let self_back = self.clone();
        let step_key_back = step.key.clone();
        let on_cancel = Arc::new(move || {
            if step_index > 0 {
                self_back
                    .inner
                    .state
                    .lock()
                    .unwrap()
                    .context
                    .shift_remove(&step_key_back);
                let prev = self_back.build_step(step_index - 1);
                self_back.inner.state.lock().unwrap().active_component = prev;
            } else {
                (self_back.inner.on_cancel)();
            }
        });

        let submenu_opts = (step.searchable || step.layout.is_some()).then_some(SelectSubmenuOptions {
            searchable: step.searchable,
            layout: step.layout,
        });

        Arc::new(SelectSubmenu::new(
            &title,
            &format!("{step_label}{desc}"),
            items,
            &preselect,
            on_select,
            on_cancel,
            None,
            submenu_opts,
        ))
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        let comp = self.inner.state.lock().unwrap().active_component.clone();
        comp.render(width)
    }

    pub fn handle_input(&self, data: &str) {
        let comp = self.inner.state.lock().unwrap().active_component.clone();
        comp.handle_input(data);
    }

    pub fn invalidate(&self) {
        let comp = self.inner.state.lock().unwrap().active_component.clone();
        comp.invalidate();
    }
}

impl Component for SteppedSubmenu {
    fn render(&self, width: usize) -> Vec<String> {
        SteppedSubmenu::render(self, width)
    }

    fn handle_input(&self, data: &str) {
        SteppedSubmenu::handle_input(self, data)
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        let comp = self.inner.state.lock().unwrap().active_component.clone();
        comp.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        SteppedSubmenu::invalidate(self)
    }

    fn has_handle_input(&self) -> bool {
        true
    }
}
