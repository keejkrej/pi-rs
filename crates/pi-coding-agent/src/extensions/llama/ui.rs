//! Port of packages/coding-agent/src/extensions/llama/ui.ts

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::Value;

use crate::core::extensions::types::{ExtensionCommandContext, NotifyType, Theme, mcp_wire::KeybindingsManager};
use crate::extensions::llama::client::{LlamaModelInfo, LlamaModelStatus, LlamaProgress};
use crate::extensions::llama::huggingface::HuggingFaceModel;
use crate::extensions::wire::{DynamicBorder, key_hint};
use pi_js::abort::{AbortController, AbortReason, AbortSignal};
use pi_js::time::Timeout;
use pi_js::{BoxFuture, Result};
use pi_tui::components::input::Input;
use pi_tui::components::select_list::{
    SelectItem, SelectList, SelectListLayoutOptions, SelectListStyleFn, SelectListTheme,
};
use pi_tui::components::spacer::Spacer;
use pi_tui::components::text::Text;
use pi_tui::fuzzy::fuzzy_filter;
use pi_tui::tui::{Component, Container, Focusable, TUI};
use pi_tui::utils::{truncate_to_width, visible_width};

const DOWNLOAD_VALUE: &str = "\0download";

#[derive(Clone, Debug)]
pub enum LlamaManagerAction {
    Model(LlamaModelInfo),
    Download,
    Close,
}

#[derive(Clone, Debug)]
pub struct ProgressState {
    pub title: String,
    pub model: String,
    pub message: String,
    pub ratio: Option<f64>,
    pub detail: Option<String>,
}

fn context_label(model: &LlamaModelInfo) -> Option<String> {
    if let Some(ctx) = model.meta.as_ref().and_then(|m| m.n_ctx.or(m.n_ctx_train))
        && ctx > 0
    {
        return Some(if ctx >= 1000 {
            format!("{}k", (ctx as f64 / 1000.0).round() as i64)
        } else {
            ctx.to_string()
        });
    }
    let args = model.status.args.as_deref()?;
    for idx in 0..args.len().saturating_sub(1) {
        let flag = args[idx].as_str();
        if flag != "--ctx-size" && flag != "-c" && flag != "-ctx" {
            continue;
        }
        if let Ok(val) = args[idx + 1].parse::<i64>()
            && val > 0
        {
            return Some(if val >= 1000 {
                format!("{}k", (val as f64 / 1000.0).round() as i64)
            } else {
                val.to_string()
            });
        }
    }
    None
}

fn model_description(model: &LlamaModelInfo) -> String {
    let mut details = Vec::new();
    let loaded = matches!(
        model.status.value,
        LlamaModelStatus::Loaded | LlamaModelStatus::Sleeping
    );
    if loaded {
        details.push("loaded".to_string());
    } else if model.status.value != LlamaModelStatus::Unloaded {
        let label = match model.status.value {
            LlamaModelStatus::Loading => "loading",
            LlamaModelStatus::Downloading => "downloading",
            LlamaModelStatus::Loaded | LlamaModelStatus::Sleeping | LlamaModelStatus::Unloaded => "",
        };
        if !label.is_empty() {
            details.push(label.to_string());
        }
    }
    if loaded && let Some(ctx) = context_label(model) {
        details.push(format!("{ctx} context"));
    }
    details.join(" · ")
}

fn select_theme(theme: &Theme) -> SelectListTheme {
    let t1 = theme.clone();
    let t2 = theme.clone();
    let t3 = theme.clone();
    let t4 = theme.clone();
    let t5 = theme.clone();
    let selected_prefix: SelectListStyleFn = Arc::new(move |s| t1.fg("accent", s));
    let selected_text: SelectListStyleFn = Arc::new(move |s| t2.fg("accent", s));
    let description: SelectListStyleFn = Arc::new(move |s| t3.fg("muted", s));
    let scroll_info: SelectListStyleFn = Arc::new(move |s| t4.fg("dim", s));
    let no_match: SelectListStyleFn = Arc::new(move |s| t5.fg("warning", s));
    SelectListTheme {
        selected_prefix,
        selected_text,
        description,
        scroll_info,
        no_match,
    }
}

fn frame(theme: &Theme, title: &str, body: Vec<Arc<dyn Component>>, footer: Option<&str>) -> Container {
    let container = Container::new();
    let t_top = theme.clone();
    container.add_child(Arc::new(DynamicBorder::new(Arc::new(move |s| t_top.fg("accent", s)))));
    container.add_child(Arc::new(Text::new(
        Some(&theme.fg("accent", &theme.bold(title))),
        Some(1),
        Some(0),
        None,
    )));
    for child in body {
        container.add_child(child);
    }
    if let Some(foot) = footer {
        container.add_child(Arc::new(Spacer::new(Some(1))));
        container.add_child(Arc::new(Text::new(
            Some(&theme.fg("dim", foot)),
            Some(1),
            Some(0),
            None,
        )));
    }
    let t_bot = theme.clone();
    container.add_child(Arc::new(DynamicBorder::new(Arc::new(move |s| t_bot.fg("accent", s)))));
    container
}

pub type HfSearchFn = Arc<dyn Fn(String, AbortSignal) -> BoxFuture<Result<Vec<HuggingFaceModel>>> + Send + Sync>;

#[async_trait]
pub trait LlamaUi: Send + Sync {
    async fn show_models(&self, server_url: &str, models: &[LlamaModelInfo]) -> LlamaManagerAction;
    async fn select(&self, title: &str, options: &[String]) -> Option<String>;
    async fn confirm(&self, title: &str, message: &str) -> bool;
    async fn connection_error(&self, server_url: &str, message: &str) -> bool;
    async fn search_models(&self, search: HfSearchFn) -> Option<String>;
    fn show_status(&self, title: &str, message: &str);
    async fn progress(&self, state: &ProgressState);
    fn update_progress(&self, state: &ProgressState);
}

fn compact_count(value: i64) -> String {
    if value >= 1_000_000 {
        let dec = if value >= 10_000_000 { 0 } else { 1 };
        format!("{:.*}M", dec, value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        let dec = if value >= 100_000 { 0 } else { 1 };
        format!("{:.*}k", dec, value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

struct HuggingFaceSearchState {
    results: Vec<HuggingFaceModel>,
    filtered_results: Vec<HuggingFaceModel>,
    selected_index: usize,
    query: String,
    status: String,
    debounce: Option<Timeout>,
    request: Option<Arc<AbortController>>,
    closed: bool,
    focused: bool,
}

#[derive(Clone)]
struct HuggingFaceSearch {
    tui: Arc<dyn TUI>,
    theme: Theme,
    keybindings: KeybindingsManager,
    search: HfSearchFn,
    cache: Arc<Mutex<IndexMap<String, Vec<HuggingFaceModel>>>>,
    on_select: Arc<Mutex<Option<tokio::sync::oneshot::Sender<Option<String>>>>>,
    container: Container,
    input: Input,
    results_container: Container,
    state: Arc<Mutex<HuggingFaceSearchState>>,
}

static EXACT_HF_RE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"^[^/\s]+/[^:\s]+(?::[^\s:]+)?$").expect("valid hf model regex"));

impl HuggingFaceSearch {
    fn new(
        tui: Arc<dyn TUI>,
        theme: Theme,
        keybindings: KeybindingsManager,
        search: HfSearchFn,
        cache: Arc<Mutex<IndexMap<String, Vec<HuggingFaceModel>>>>,
        tx: tokio::sync::oneshot::Sender<Option<String>>,
    ) -> Self {
        let container = Container::new();
        let input = Input::new(None);
        let results_container = Container::new();
        container.add_child(Arc::new(Text::new(
            Some(&theme.fg("dim", "Model name or owner/repository[:quant]")),
            Some(1),
            Some(0),
            None,
        )));
        container.add_child(Arc::new(input.clone()));
        container.add_child(Arc::new(Spacer::new(Some(1))));
        container.add_child(Arc::new(results_container.clone()));

        let instance = Self {
            tui,
            theme,
            keybindings,
            search,
            cache,
            on_select: Arc::new(Mutex::new(Some(tx))),
            container,
            input,
            results_container,
            state: Arc::new(Mutex::new(HuggingFaceSearchState {
                results: Vec::new(),
                filtered_results: Vec::new(),
                selected_index: 0,
                query: String::new(),
                status: "Type at least 2 characters".to_string(),
                debounce: None,
                request: None,
                closed: false,
                focused: false,
            })),
        };
        instance.update_results();
        instance
    }

    fn update_results(&self) {
        self.results_container.clear();
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let max_visible = 10usize;
        let len = st.filtered_results.len();
        let start = if len <= max_visible {
            0
        } else {
            st.selected_index.saturating_sub(max_visible / 2).min(len - max_visible)
        };
        let end = (start + max_visible).min(len);
        for (index, model) in st.filtered_results.iter().enumerate().take(end).skip(start) {
            let prefix = if index == st.selected_index { "→ " } else { "  " };
            let details = format!("{} downloads", compact_count(model.downloads));
            let text = if index == st.selected_index {
                self.theme.fg("accent", &format!("{prefix}{}  {details}", model.id))
            } else {
                format!(
                    "{prefix}{}{}",
                    model.id,
                    self.theme.fg("muted", &format!("  {details}"))
                )
            };
            self.results_container
                .add_child(Arc::new(Text::new(Some(&text), Some(0), Some(0), None)));
        }
        if start > 0 || end < len {
            self.results_container.add_child(Arc::new(Text::new(
                Some(&self.theme.fg("dim", &format!("  ({}/{len})", st.selected_index + 1))),
                Some(0),
                Some(0),
                None,
            )));
        }
        if len == 0 || st.status == "Searching Hugging Face…" {
            self.results_container.add_child(Arc::new(Text::new(
                Some(&self.theme.fg("dim", &format!("  {}", st.status))),
                Some(0),
                Some(0),
                None,
            )));
        }
        drop(st);
        self.tui.request_render(None);
    }

    fn filter_results(&self) {
        {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if !st.query.is_empty() {
                let query = st.query.clone();
                let matches = fuzzy_filter(&st.results, &query, Arc::new(|m: &HuggingFaceModel| m.id.clone()));
                let ids: indexmap::IndexSet<String> = matches.into_iter().map(|m| m.id).collect();
                st.filtered_results = st.results.iter().filter(|m| ids.contains(&m.id)).cloned().collect();
            } else {
                st.filtered_results = st.results.clone();
            }
            if st.filtered_results.is_empty() {
                st.selected_index = 0;
            } else {
                st.selected_index = st.selected_index.min(st.filtered_results.len() - 1);
            }
        }
        self.update_results();
    }

    fn schedule_search(&self) {
        let query = {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(t) = st.debounce.take() {
                t.clear();
            }
            if let Some(req) = st.request.take() {
                req.abort(Some(AbortReason::error("AbortError", "superseded")));
            }
            if st.query.len() < 2 {
                st.status = "Type at least 2 characters".to_string();
                drop(st);
                self.filter_results();
                return;
            }
            let cache_key = st.query.to_ascii_lowercase();
            let cached = self
                .cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&cache_key)
                .cloned();
            if let Some(results) = cached {
                st.status = if results.is_empty() {
                    "No GGUF models found".to_string()
                } else {
                    String::new()
                };
                st.results = results;
                drop(st);
                self.filter_results();
                return;
            }
            st.status = "Searching Hugging Face…".to_string();
            st.query.clone()
        };
        self.filter_results();
        let this = self.clone();
        let timer = pi_js::time::set_timeout(500, move || {
            let inner = this.clone();
            tokio::spawn(async move {
                inner.run_search(query).await;
            });
        });
        self.state.lock().unwrap_or_else(|e| e.into_inner()).debounce = Some(timer);
    }

    async fn run_search(&self, query: String) {
        let controller = Arc::new(AbortController::new());
        let signal = controller.signal();
        {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            st.request = Some(Arc::clone(&controller));
        }
        let outcome = (self.search)(query.clone(), signal.clone()).await;
        let mut should_filter = false;
        {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Ok(ref results) = outcome {
                self.cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(query.to_ascii_lowercase(), results.clone());
            }
            if !st.closed && !signal.aborted() && st.query == query {
                match outcome {
                    Ok(results) => {
                        st.status = if results.is_empty() {
                            "No GGUF models found".to_string()
                        } else {
                            String::new()
                        };
                        st.results = results;
                        st.selected_index = 0;
                    }
                    Err(err) => {
                        st.results.clear();
                        st.status = err.message().to_string();
                    }
                }
                should_filter = true;
            }
            if st.request.as_ref().is_some_and(|r| r.signal().ptr_eq(&signal)) {
                st.request = None;
            }
        }
        if should_filter {
            self.filter_results();
        }
    }

    fn close(&self, model: Option<String>) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.closed {
            return;
        }
        st.closed = true;
        if let Some(t) = st.debounce.take() {
            t.clear();
        }
        if let Some(req) = st.request.take() {
            req.abort(Some(AbortReason::error("AbortError", "closed")));
        }
        drop(st);
        if let Some(tx) = self.on_select.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = tx.send(model);
        }
    }
}

impl Focusable for HuggingFaceSearch {
    fn focused(&self) -> bool {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).focused
    }

    fn set_focused(&self, focused: bool) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).focused = focused;
        self.input.set_focused(focused);
    }
}

impl Component for HuggingFaceSearch {
    fn render(&self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    fn handle_input(&self, data: &str) {
        if self.keybindings.matches(data, "tui.select.up") {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if !st.filtered_results.is_empty() {
                st.selected_index = if st.selected_index == 0 {
                    st.filtered_results.len() - 1
                } else {
                    st.selected_index - 1
                };
                drop(st);
                self.update_results();
            }
            return;
        }
        if self.keybindings.matches(data, "tui.select.down") {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if !st.filtered_results.is_empty() {
                st.selected_index = if st.selected_index + 1 >= st.filtered_results.len() {
                    0
                } else {
                    st.selected_index + 1
                };
                drop(st);
                self.update_results();
            }
            return;
        }
        if self.keybindings.matches(data, "tui.select.confirm") {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let selected = if EXACT_HF_RE.is_match(&st.query) {
                Some(st.query.clone())
            } else {
                st.filtered_results.get(st.selected_index).map(|m| m.id.clone())
            };
            drop(st);
            if selected.is_some() {
                self.close(selected);
            }
            return;
        }
        if self.keybindings.matches(data, "tui.select.cancel") {
            self.close(None);
            return;
        }
        self.input.handle_input(data);
        let query = pi_js::str16::trim(&self.input.get_value()).to_string();
        {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if query == st.query {
                return;
            }
            st.query = query;
        }
        self.schedule_search();
    }

    fn invalidate(&self) {
        self.container.invalidate();
    }
}

struct LlamaViewState {
    content: Container,
    input_handler: Option<Arc<dyn Component>>,
    input_target: Option<Arc<dyn Focusable>>,
    progress_waiters: Vec<tokio::sync::oneshot::Sender<()>>,
    showing_progress: bool,
    focused: bool,
}

#[derive(Clone)]
struct LlamaView {
    tui: Arc<dyn TUI>,
    theme: Theme,
    keybindings: KeybindingsManager,
    search_cache: Arc<Mutex<IndexMap<String, Vec<HuggingFaceModel>>>>,
    state: Arc<Mutex<LlamaViewState>>,
}

impl LlamaView {
    fn new(tui: Arc<dyn TUI>, theme: Theme, keybindings: KeybindingsManager) -> Self {
        let initial = frame(
            &theme,
            "llama.cpp models",
            vec![Arc::new(Text::new(
                Some(&theme.fg("muted", "Loading…")),
                Some(1),
                Some(1),
                None,
            ))],
            None,
        );
        Self {
            tui,
            theme,
            keybindings,
            search_cache: Arc::new(Mutex::new(IndexMap::new())),
            state: Arc::new(Mutex::new(LlamaViewState {
                content: initial,
                input_handler: None,
                input_target: None,
                progress_waiters: Vec::new(),
                showing_progress: false,
                focused: false,
            })),
        }
    }

    fn set_content(
        &self,
        content: Container,
        input_handler: Option<Arc<dyn Component>>,
        input_target: Option<Arc<dyn Focusable>>,
    ) {
        {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref prev) = st.input_target {
                prev.set_focused(false);
            }
            st.progress_waiters.clear();
            st.showing_progress = false;
            st.content = content;
            st.input_handler = input_handler;
            st.input_target = input_target;
            let focused = st.focused;
            if let Some(ref target) = st.input_target {
                target.set_focused(focused);
            }
        }
        self.tui.request_render(None);
    }
}

impl Focusable for LlamaView {
    fn focused(&self) -> bool {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).focused
    }

    fn set_focused(&self, focused: bool) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.focused = focused;
        if let Some(ref target) = st.input_target {
            target.set_focused(focused);
        }
    }
}

impl Component for LlamaView {
    fn render(&self, width: usize) -> Vec<String> {
        let content = self.state.lock().unwrap_or_else(|e| e.into_inner()).content.clone();
        content
            .render(width)
            .into_iter()
            .map(|line| {
                if visible_width(&line) > width as i64 {
                    truncate_to_width(&line, width as i64, Some(""), None)
                } else {
                    line
                }
            })
            .collect()
    }

    fn handle_input(&self, data: &str) {
        let (waiters, handler) = {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if !st.progress_waiters.is_empty() && self.keybindings.matches(data, "tui.select.cancel") {
                (std::mem::take(&mut st.progress_waiters), None)
            } else {
                (Vec::new(), st.input_handler.clone())
            }
        };
        if !waiters.is_empty() {
            for w in waiters {
                let _ = w.send(());
            }
            return;
        }
        if let Some(h) = handler {
            h.handle_input(data);
        }
        self.tui.request_render(None);
    }

    fn invalidate(&self) {
        let content = self.state.lock().unwrap_or_else(|e| e.into_inner()).content.clone();
        content.invalidate();
    }
}

#[async_trait]
impl LlamaUi for LlamaView {
    async fn show_models(&self, server_url: &str, models: &[LlamaModelInfo]) -> LlamaManagerAction {
        let mut sorted = models.to_vec();
        sorted.sort_by(|left, right| {
            let r_loaded = u8::from(right.status.value == LlamaModelStatus::Loaded);
            let l_loaded = u8::from(left.status.value == LlamaModelStatus::Loaded);
            r_loaded
                .cmp(&l_loaded)
                .then_with(|| pi_js::intl::locale_compare(&left.id, &right.id))
        });
        let by_id: IndexMap<String, LlamaModelInfo> = sorted.iter().map(|m| (m.id.clone(), m.clone())).collect();
        let mut items: Vec<SelectItem> = sorted
            .iter()
            .map(|m| {
                let desc = model_description(m);
                SelectItem {
                    value: m.id.clone(),
                    label: m.id.clone(),
                    description: if desc.is_empty() { None } else { Some(desc) },
                }
            })
            .collect();
        items.push(SelectItem {
            value: DOWNLOAD_VALUE.to_string(),
            label: "Download model…".to_string(),
            description: Some("Hugging Face owner/repository[:quant]".to_string()),
        });

        let max_vis = items.len().min(12) as i64;
        let list = SelectList::new(
            items,
            max_vis,
            select_theme(&self.theme),
            Some(SelectListLayoutOptions {
                min_primary_column_width: Some(36),
                max_primary_column_width: Some(56),
                truncate_primary: None,
            }),
        );
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx_sel = Arc::new(Mutex::new(Some(tx)));
        let tx_can = Arc::clone(&tx_sel);
        list.set_on_select(Some(Arc::new(move |item| {
            let action = if item.value == DOWNLOAD_VALUE {
                LlamaManagerAction::Download
            } else if let Some(model) = by_id.get(&item.value) {
                LlamaManagerAction::Model(model.clone())
            } else {
                LlamaManagerAction::Close
            };
            if let Some(sender) = tx_sel.lock().unwrap_or_else(|e| e.into_inner()).take() {
                let _ = sender.send(action);
            }
        })));
        list.set_on_cancel(Some(Arc::new(move || {
            if let Some(sender) = tx_can.lock().unwrap_or_else(|e| e.into_inner()).take() {
                let _ = sender.send(LlamaManagerAction::Close);
            }
        })));

        let list_arc: Arc<dyn Component> = Arc::new(list);
        let footer = format!(
            "{} • {}",
            key_hint("tui.select.confirm", "load/unload/download", &self.theme),
            key_hint("tui.select.cancel", "close", &self.theme),
        );
        self.set_content(
            frame(
                &self.theme,
                "llama.cpp models",
                vec![
                    Arc::new(Text::new(
                        Some(&self.theme.fg("dim", server_url)),
                        Some(1),
                        Some(0),
                        None,
                    )),
                    Arc::new(Spacer::new(Some(1))),
                    Arc::clone(&list_arc),
                ],
                Some(&footer),
            ),
            Some(list_arc),
            None,
        );
        rx.await.unwrap_or(LlamaManagerAction::Close)
    }

    async fn select(&self, title: &str, options: &[String]) -> Option<String> {
        let items: Vec<SelectItem> = options
            .iter()
            .map(|o| SelectItem {
                value: o.clone(),
                label: o.clone(),
                description: None,
            })
            .collect();
        let list = SelectList::new(items, options.len().min(12) as i64, select_theme(&self.theme), None);
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx_sel = Arc::new(Mutex::new(Some(tx)));
        let tx_can = Arc::clone(&tx_sel);
        list.set_on_select(Some(Arc::new(move |item| {
            if let Some(sender) = tx_sel.lock().unwrap_or_else(|e| e.into_inner()).take() {
                let _ = sender.send(Some(item.value.clone()));
            }
        })));
        list.set_on_cancel(Some(Arc::new(move || {
            if let Some(sender) = tx_can.lock().unwrap_or_else(|e| e.into_inner()).take() {
                let _ = sender.send(None);
            }
        })));
        let list_arc: Arc<dyn Component> = Arc::new(list);
        let footer = format!(
            "{} • {}",
            key_hint("tui.select.confirm", "select", &self.theme),
            key_hint("tui.select.cancel", "cancel", &self.theme),
        );
        self.set_content(
            frame(
                &self.theme,
                title,
                vec![Arc::new(Spacer::new(Some(1))), Arc::clone(&list_arc)],
                Some(&footer),
            ),
            Some(list_arc),
            None,
        );
        rx.await.ok().flatten()
    }

    async fn confirm(&self, title: &str, message: &str) -> bool {
        self.select(&format!("{title}\n{message}"), &["Yes".to_string(), "No".to_string()])
            .await
            .as_deref()
            == Some("Yes")
    }

    async fn connection_error(&self, server_url: &str, message: &str) -> bool {
        self.select(
            &format!("llama.cpp unavailable\n{server_url}\n\n{message}"),
            &["Retry".to_string(), "Close".to_string()],
        )
        .await
        .as_deref()
            == Some("Retry")
    }

    async fn search_models(&self, search: HfSearchFn) -> Option<String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let component = HuggingFaceSearch::new(
            Arc::clone(&self.tui),
            self.theme.clone(),
            self.keybindings.clone(),
            search,
            Arc::clone(&self.search_cache),
            tx,
        );
        let comp_arc: Arc<dyn Component> = Arc::new(component.clone());
        let focus_arc: Arc<dyn Focusable> = Arc::new(component);
        let footer = format!(
            "{} • {}",
            key_hint("tui.select.confirm", "select", &self.theme),
            key_hint("tui.select.cancel", "back", &self.theme),
        );
        self.set_content(
            frame(
                &self.theme,
                "Download model",
                vec![Arc::new(Spacer::new(Some(1))), Arc::clone(&comp_arc)],
                Some(&footer),
            ),
            Some(comp_arc),
            Some(focus_arc),
        );
        rx.await.ok().flatten()
    }

    fn show_status(&self, title: &str, message: &str) {
        self.set_content(
            frame(
                &self.theme,
                title,
                vec![
                    Arc::new(Spacer::new(Some(1))),
                    Arc::new(Text::new(
                        Some(&self.theme.fg("muted", message)),
                        Some(1),
                        Some(0),
                        None,
                    )),
                ],
                None,
            ),
            None,
            None,
        );
    }

    async fn progress(&self, state: &ProgressState) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            st.progress_waiters.push(tx);
            st.showing_progress = true;
        }
        self.update_progress(state);
        let _ = rx.await;
    }

    fn update_progress(&self, state: &ProgressState) {
        {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if !st.showing_progress {
                return;
            }
        }
        let mut body: Vec<Arc<dyn Component>> = vec![
            Arc::new(Text::new(
                Some(&self.theme.fg("text", &state.model)),
                Some(1),
                Some(0),
                None,
            )),
            Arc::new(Spacer::new(Some(1))),
            Arc::new(Text::new(
                Some(&self.theme.fg("muted", &state.message)),
                Some(1),
                Some(0),
                None,
            )),
        ];
        if let Some(ratio) = state.ratio {
            let available = 40usize;
            let clamped = ratio.clamp(0.0, 1.0);
            let filled = (clamped * available as f64).round() as usize;
            let bar = format!(
                "{}{} {}%",
                "█".repeat(filled),
                "─".repeat(available - filled),
                (clamped * 100.0).round() as i64
            );
            body.push(Arc::new(Text::new(
                Some(&self.theme.fg("accent", &bar)),
                Some(1),
                Some(0),
                None,
            )));
        }
        if let Some(ref detail) = state.detail {
            body.push(Arc::new(Text::new(
                Some(&self.theme.fg("dim", detail)),
                Some(1),
                Some(0),
                None,
            )));
        }
        let footer = key_hint("tui.select.cancel", "stop", &self.theme);
        let next_content = frame(&self.theme, &state.title, body, Some(&footer));
        {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            st.content = next_content;
            st.input_handler = None;
        }
        self.tui.request_render(None);
    }
}

pub async fn show_llama_ui<F>(ctx: &ExtensionCommandContext, run: F) -> Result<()>
where
    F: FnOnce(Arc<dyn LlamaUi>) -> BoxFuture<Result<()>> + Send + 'static,
{
    let ui = ctx.base.ui()?;
    let ui_notify = Arc::clone(&ui);
    ui.custom(
        Box::new(move |tui, theme, keybindings, done| {
            Box::pin(async move {
                let view = LlamaView::new(tui, theme, keybindings);
                let ui_handle: Arc<dyn LlamaUi> = Arc::new(view.clone());
                let notify = Arc::clone(&ui_notify);
                tokio::spawn(async move {
                    if let Err(err) = run(ui_handle).await {
                        notify.notify(&err.message(), Some(NotifyType::Error));
                    }
                    done(Value::Null);
                });
                Ok(Arc::new(view) as Arc<dyn Component>)
            })
        }),
        None,
    )
    .await?;
    Ok(())
}

pub enum ProgressOutcome<T> {
    Cancelled,
    Completed(T),
}

pub struct RunWithProgressOptions<T: Send + 'static> {
    pub title: String,
    pub model: String,
    pub initial_message: String,
    pub cancel_title: String,
    pub cancel_message: String,
    pub run: Box<dyn FnOnce(AbortSignal, Arc<dyn Fn(LlamaProgress) + Send + Sync>) -> BoxFuture<Result<T>> + Send>,
    pub cancel: Box<dyn Fn() -> BoxFuture<Result<()>> + Send + Sync>,
}

pub async fn run_with_progress<T: Send + 'static>(
    ui: &Arc<dyn LlamaUi>,
    options: RunWithProgressOptions<T>,
) -> Result<ProgressOutcome<T>> {
    let controller = AbortController::new();
    let state = Arc::new(Mutex::new(ProgressState {
        title: options.title,
        model: options.model,
        message: options.initial_message,
        ratio: None,
        detail: None,
    }));
    let ui_upd = Arc::clone(ui);
    let st_upd = Arc::clone(&state);
    let update_fn: Arc<dyn Fn(LlamaProgress) + Send + Sync> = Arc::new(move |prog| {
        let snapshot = {
            let mut s = st_upd.lock().unwrap_or_else(|e| e.into_inner());
            s.message = prog.message;
            s.ratio = prog.ratio;
            s.detail = prog.detail;
            s.clone()
        };
        ui_upd.update_progress(&snapshot);
    });

    let run_fut = (options.run)(controller.signal(), update_fn);
    let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (done_tx, mut done_rx) = tokio::sync::oneshot::channel::<Result<T>>();
    let completed_bg = Arc::clone(&completed);
    tokio::spawn(async move {
        let res = run_fut.await;
        completed_bg.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = done_tx.send(res);
    });

    loop {
        if completed.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
        let snapshot = state.lock().unwrap_or_else(|e| e.into_inner()).clone();
        tokio::select! {
            res = &mut done_rx => {
                return match res {
                    Ok(Ok(val)) => Ok(ProgressOutcome::Completed(val)),
                    Ok(Err(e)) => Err(e),
                    Err(_) => Err(pi_js::Error::msg("Task cancelled")),
                };
            }
            () = ui.progress(&snapshot) => {}
        }
        if !completed.load(std::sync::atomic::Ordering::SeqCst) {
            let stop = ui.confirm(&options.cancel_title, &options.cancel_message).await;
            if stop && !completed.load(std::sync::atomic::Ordering::SeqCst) {
                let _ = (options.cancel)().await;
                controller.abort(Some(AbortReason::error("AbortError", "Cancelled")));
                let _ = done_rx.await;
                return Ok(ProgressOutcome::Cancelled);
            }
        }
    }

    match done_rx.await {
        Ok(Ok(val)) => Ok(ProgressOutcome::Completed(val)),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(pi_js::Error::msg("Task cancelled")),
    }
}
