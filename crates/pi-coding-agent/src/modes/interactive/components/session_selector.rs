//! Port of packages/coding-agent/src/modes/interactive/components/session-selector.ts

use std::path::Path;
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_js::abort::{AbortController, AbortSignal};
use pi_js::time::Timeout;
use pi_js::{BoxFuture, Result};
use pi_tui::{
    Component, Container, Focusable, Input, KeybindingsManager, Spacer, Text, TuiMouseEvent, TuiMouseEventResult,
    get_keybindings, truncate_to_width, visible_width,
};

use super::super::theme::theme::{ThemeBg, ThemeColor, theme};
use super::dynamic_border::DynamicBorder;
use super::keybinding_hints::{key_hint, key_text};
use super::session_selector_search::{NameFilter, SessionInfo, SortMode, filter_and_sort_sessions, has_session_name};

// WIRE(@tui-w3): Replace local path helpers with crate::utils::paths::{canonicalize_path,
// get_path_comparison_key, shorten_home_path} and keybindings with crate::core::keybindings::KeybindingsManager
// and SessionInfo/SessionListProgress with crate::core::session_manager once @mcp lands on main.

pub type SessionListProgress = Arc<dyn Fn(usize, usize, Option<&[SessionInfo]>) + Send + Sync>;

pub type SessionsLoader =
    Arc<dyn Fn(Option<SessionListProgress>, AbortSignal) -> BoxFuture<Result<Vec<SessionInfo>>> + Send + Sync>;

pub type RenameSessionFn = Arc<dyn Fn(String, Option<String>) -> BoxFuture<Result<()>> + Send + Sync>;

#[derive(Clone, Default)]
pub struct SessionSelectorOptions {
    pub rename_session: Option<RenameSessionFn>,
    pub show_rename_hint: Option<bool>,
    pub keybindings: Option<Arc<KeybindingsManager>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionScope {
    Current,
    All,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatusMessageKind {
    Info,
    Error,
}

#[derive(Clone, Debug)]
struct StatusMessage {
    kind: StatusMessageKind,
    message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectorMode {
    List,
    Rename,
}

fn canonicalize_path(path: &str) -> String {
    pi_js::fs::realpath(path).unwrap_or_else(|_| path.to_string())
}

fn get_path_comparison_key(path: &str) -> String {
    let resolved = if pi_js::env::platform() == "win32" {
        pi_js::path::win32::resolve(&[path])
    } else {
        pi_js::path::posix::resolve(&[path])
    };
    if pi_js::env::platform() == "win32" {
        resolved.to_ascii_lowercase()
    } else {
        resolved
    }
}

fn get_session_path_key(path: &str) -> String {
    get_path_comparison_key(&canonicalize_path(path))
}

fn shorten_home_path(path: &str) -> String {
    let home = pi_js::env::home_dir();
    if path.is_empty() || home.is_empty() {
        return path.to_string();
    }
    if path == home {
        "~".to_string()
    } else if let Some(rest) = path.strip_prefix(&home) {
        let first = rest.chars().next();
        if first == Some('/') || first == Some('\\') {
            format!("~{rest}")
        } else {
            path.to_string()
        }
    } else {
        path.to_string()
    }
}

fn format_session_date(modified_ms: i64) -> String {
    let now_ms = pi_js::time::now_ms();
    let diff_ms = now_ms - modified_ms;
    let diff_mins = diff_ms / 60_000;
    let diff_hours = diff_ms / 3_600_000;
    let diff_days = diff_ms / 86_400_000;

    if diff_mins < 1 {
        "now".to_string()
    } else if diff_mins < 60 {
        format!("{diff_mins}m")
    } else if diff_hours < 24 {
        format!("{diff_hours}h")
    } else if diff_days < 7 {
        format!("{diff_days}d")
    } else if diff_days < 30 {
        format!("{}w", diff_days / 7)
    } else if diff_days < 365 {
        format!("{}mo", diff_days / 30)
    } else {
        format!("{}y", diff_days / 365)
    }
}

struct SessionSelectorHeaderState {
    scope: SessionScope,
    sort_mode: SortMode,
    name_filter: NameFilter,
    loading: bool,
    load_progress: Option<(usize, usize)>,
    show_path: bool,
    confirming_delete_path: Option<String>,
    status_message: Option<StatusMessage>,
    status_timeout: Option<Timeout>,
    show_rename_hint: bool,
}

#[derive(Clone)]
struct SessionSelectorHeader {
    request_render: Arc<dyn Fn() + Send + Sync>,
    state: Arc<Mutex<SessionSelectorHeaderState>>,
}

impl SessionSelectorHeader {
    fn new(
        scope: SessionScope,
        sort_mode: SortMode,
        name_filter: NameFilter,
        request_render: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self {
            request_render,
            state: Arc::new(Mutex::new(SessionSelectorHeaderState {
                scope,
                sort_mode,
                name_filter,
                loading: false,
                load_progress: None,
                show_path: false,
                confirming_delete_path: None,
                status_message: None,
                status_timeout: None,
                show_rename_hint: false,
            })),
        }
    }

    fn set_scope(&self, scope: SessionScope) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).scope = scope;
    }

    fn set_sort_mode(&self, sort_mode: SortMode) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).sort_mode = sort_mode;
    }

    fn set_name_filter(&self, name_filter: NameFilter) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).name_filter = name_filter;
    }

    fn set_loading(&self, loading: bool) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.loading = loading;
        state.load_progress = None;
    }

    fn set_progress(&self, loaded: usize, total: usize) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).load_progress = Some((loaded, total));
    }

    fn set_show_path(&self, show_path: bool) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).show_path = show_path;
    }

    fn set_show_rename_hint(&self, show: bool) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).show_rename_hint = show;
    }

    fn set_confirming_delete_path(&self, path: Option<String>) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .confirming_delete_path = path;
    }

    fn set_status_message(&self, msg: Option<StatusMessage>, auto_hide_ms: Option<u64>) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(prev) = state.status_timeout.take() {
            prev.clear();
        }
        state.status_message = msg.clone();
        if msg.is_none() {
            return;
        }
        if let Some(ms) = auto_hide_ms
            && tokio::runtime::Handle::try_current().is_ok()
        {
            let header = self.clone();
            let timeout = pi_js::time::set_timeout(ms, move || {
                {
                    let mut st = header.state.lock().unwrap_or_else(|e| e.into_inner());
                    st.status_message = None;
                    st.status_timeout = None;
                }
                (header.request_render)();
            });
            timeout.unref();
            state.status_timeout = Some(timeout);
        }
    }
}

impl Drop for SessionSelectorHeaderState {
    fn drop(&mut self) {
        if let Some(prev) = self.status_timeout.take() {
            prev.clear();
        }
    }
}

impl Component for SessionSelectorHeader {
    fn render(&self, width: usize) -> Vec<String> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let t = theme();
        let title = if state.scope == SessionScope::Current {
            "Resume Session (Current Folder)"
        } else {
            "Resume Session (All)"
        };
        let left_text = t.bold(title);

        let sort_label = match state.sort_mode {
            SortMode::Threaded => "Threaded",
            SortMode::Recent => "Recent",
            SortMode::Relevance => "Fuzzy",
        };
        let sort_text = format!(
            "{}{}",
            t.fg(ThemeColor::Muted, "Sort: "),
            t.fg(ThemeColor::Accent, sort_label)
        );

        let name_label = match state.name_filter {
            NameFilter::All => "All",
            NameFilter::Named => "Named",
        };
        let name_text = format!(
            "{}{}",
            t.fg(ThemeColor::Muted, "Name: "),
            t.fg(ThemeColor::Accent, name_label)
        );

        let scope_text = if state.loading {
            let progress_text = state
                .load_progress
                .map(|(loaded, total)| format!("{loaded}/{total}"))
                .unwrap_or_else(|| "...".to_string());
            format!(
                "{}{}",
                t.fg(ThemeColor::Muted, "○ Current Folder | "),
                t.fg(ThemeColor::Accent, &format!("Loading {progress_text}"))
            )
        } else if state.scope == SessionScope::Current {
            format!(
                "{}{}",
                t.fg(ThemeColor::Accent, "◉ Current Folder"),
                t.fg(ThemeColor::Muted, " | ○ All")
            )
        } else {
            format!(
                "{}{}",
                t.fg(ThemeColor::Muted, "○ Current Folder | "),
                t.fg(ThemeColor::Accent, "◉ All")
            )
        };

        let right_text = truncate_to_width(
            &format!("{scope_text}  {name_text}  {sort_text}"),
            width as i64,
            Some(""),
            None,
        );
        let available_left = (width as i64)
            .saturating_sub(visible_width(&right_text))
            .saturating_sub(1)
            .max(0);
        let left = truncate_to_width(&left_text, available_left, Some(""), None);
        let spacing = width
            .saturating_sub(visible_width(&left).max(0) as usize)
            .saturating_sub(visible_width(&right_text).max(0) as usize);

        let (hint_line_1, hint_line_2) = if state.confirming_delete_path.is_some() {
            let confirm_hint = format!(
                "Delete session? {} · {}",
                key_hint(&"tui.select.confirm".to_string(), "confirm"),
                key_hint(&"tui.select.cancel".to_string(), "cancel")
            );
            (
                t.fg(
                    ThemeColor::Error,
                    &truncate_to_width(&confirm_hint, width as i64, Some("…"), None),
                ),
                String::new(),
            )
        } else if let Some(ref status) = state.status_message {
            let color = if status.kind == StatusMessageKind::Error {
                ThemeColor::Error
            } else {
                ThemeColor::Accent
            };
            (
                t.fg(
                    color,
                    &truncate_to_width(&status.message, width as i64, Some("…"), None),
                ),
                String::new(),
            )
        } else {
            let path_state = if state.show_path { "(on)" } else { "(off)" };
            let sep = t.fg(ThemeColor::Muted, " · ");
            let hint1 = format!(
                "{}{sep}{}",
                key_hint(&"tui.input.tab".to_string(), "scope"),
                t.fg(ThemeColor::Muted, "re:<pattern> regex · \"phrase\" exact")
            );
            let mut hint2_parts = vec![
                key_hint(&"app.session.toggleSort".to_string(), "sort"),
                key_hint(&"app.session.toggleNamedFilter".to_string(), "named"),
                key_hint(&"app.session.delete".to_string(), "delete"),
                key_hint(&"app.session.togglePath".to_string(), &format!("path {path_state}")),
            ];
            if state.show_rename_hint {
                hint2_parts.push(key_hint(&"app.session.rename".to_string(), "rename"));
            }
            let hint2 = hint2_parts.join(&sep);
            (
                truncate_to_width(&hint1, width as i64, Some("…"), None),
                truncate_to_width(&hint2, width as i64, Some("…"), None),
            )
        };

        vec![
            format!("{left}{}{right_text}", " ".repeat(spacing)),
            hint_line_1,
            hint_line_2,
        ]
    }

    fn handle_input(&self, _data: &str) {}

    fn invalidate(&self) {}
}

struct SessionTreeNode {
    session: SessionInfo,
    children: Vec<usize>,
    latest_activity: i64,
}

#[derive(Clone, Debug)]
struct FlatSessionNode {
    session: SessionInfo,
    depth: usize,
    is_last: bool,
    ancestor_continues: Vec<bool>,
}

fn build_and_flatten_session_tree(sessions: &[SessionInfo]) -> Vec<FlatSessionNode> {
    if sessions.is_empty() {
        return Vec::new();
    }

    let mut nodes: Vec<SessionTreeNode> = sessions
        .iter()
        .map(|session| SessionTreeNode {
            session: session.clone(),
            children: Vec::new(),
            latest_activity: session.modified,
        })
        .collect();

    let mut by_path: IndexMap<String, usize> = IndexMap::new();
    for (idx, session) in sessions.iter().enumerate() {
        by_path.insert(get_session_path_key(&session.path), idx);
    }

    let mut roots: Vec<usize> = Vec::new();
    for (idx, session) in sessions.iter().enumerate() {
        let parent_idx = session
            .parent_session_path
            .as_deref()
            .map(get_session_path_key)
            .and_then(|pk| by_path.get(&pk).copied())
            .filter(|&pidx| pidx != idx);
        if let Some(pidx) = parent_idx {
            nodes[pidx].children.push(idx);
        } else {
            roots.push(idx);
        }
    }

    fn compute_latest(idx: usize, nodes: &mut [SessionTreeNode], visited: &mut [bool]) -> i64 {
        if visited[idx] {
            return nodes[idx].latest_activity;
        }
        visited[idx] = true;
        let children = nodes[idx].children.clone();
        let mut latest = nodes[idx].session.modified;
        for child_idx in children {
            latest = latest.max(compute_latest(child_idx, nodes, visited));
        }
        nodes[idx].latest_activity = latest;
        latest
    }

    let mut visited = vec![false; nodes.len()];
    for &root in &roots {
        compute_latest(root, &mut nodes, &mut visited);
    }

    roots.sort_by(|&a, &b| nodes[b].latest_activity.cmp(&nodes[a].latest_activity));
    for i in 0..nodes.len() {
        if nodes[i].children.len() > 1 {
            let mut ch = std::mem::take(&mut nodes[i].children);
            ch.sort_by(|&a, &b| nodes[b].latest_activity.cmp(&nodes[a].latest_activity));
            nodes[i].children = ch;
        }
    }

    fn walk(
        idx: usize,
        depth: usize,
        ancestor_continues: &[bool],
        is_last: bool,
        nodes: &[SessionTreeNode],
        out: &mut Vec<FlatSessionNode>,
        seen: &mut [bool],
    ) {
        if seen[idx] {
            return;
        }
        seen[idx] = true;
        let node = &nodes[idx];
        out.push(FlatSessionNode {
            session: node.session.clone(),
            depth,
            is_last,
            ancestor_continues: ancestor_continues.to_vec(),
        });
        let continues = depth > 0 && !is_last;
        let mut next_ancestors = ancestor_continues.to_vec();
        next_ancestors.push(continues);
        for (i, &child_idx) in node.children.iter().enumerate() {
            walk(
                child_idx,
                depth + 1,
                &next_ancestors,
                i == node.children.len() - 1,
                nodes,
                out,
                seen,
            );
        }
    }

    let mut result = Vec::with_capacity(nodes.len());
    let mut seen = vec![false; nodes.len()];
    for (i, &root_idx) in roots.iter().enumerate() {
        walk(root_idx, 0, &[], i == roots.len() - 1, &nodes, &mut result, &mut seen);
    }
    result
}

pub type DeleteSessionFn = Arc<dyn Fn(&str) -> BoxFuture<()> + Send + Sync>;

struct SessionListState {
    all_sessions: Vec<SessionInfo>,
    filtered_sessions: Vec<FlatSessionNode>,
    selected_index: usize,
    selection_touched: bool,
    pending_selected_path: Option<String>,
    show_cwd: bool,
    sort_mode: SortMode,
    name_filter: NameFilter,
    show_path: bool,
    confirming_delete_path: Option<String>,
    focused: bool,
    on_select: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    on_cancel: Option<Arc<dyn Fn() + Send + Sync>>,
    on_exit: Option<Arc<dyn Fn() + Send + Sync>>,
    on_toggle_scope: Option<Arc<dyn Fn() + Send + Sync>>,
    on_toggle_sort: Option<Arc<dyn Fn() + Send + Sync>>,
    on_toggle_name_filter: Option<Arc<dyn Fn() + Send + Sync>>,
    on_toggle_path: Option<Arc<dyn Fn(bool) + Send + Sync>>,
    on_delete_confirmation_change: Option<Arc<dyn Fn(Option<&str>) + Send + Sync>>,
    on_delete_session: Option<DeleteSessionFn>,
    on_rename_session: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    on_error: Option<Arc<dyn Fn(&str) + Send + Sync>>,
}

struct SessionListInner {
    search_input: Input,
    keybindings: Arc<KeybindingsManager>,
    current_session_path_key: Option<String>,
    max_visible: usize,
    state: Mutex<SessionListState>,
}

#[derive(Clone)]
pub struct SessionList {
    inner: Arc<SessionListInner>,
}

impl SessionList {
    fn new(
        sessions: Vec<SessionInfo>,
        show_cwd: bool,
        sort_mode: SortMode,
        name_filter: NameFilter,
        keybindings: Arc<KeybindingsManager>,
        current_session_file_path: Option<&str>,
    ) -> Self {
        let search_input = Input::new(None);
        let current_session_path_key = current_session_file_path.map(get_session_path_key);
        let mut state = SessionListState {
            all_sessions: sessions,
            filtered_sessions: Vec::new(),
            selected_index: 0,
            selection_touched: false,
            pending_selected_path: None,
            show_cwd,
            sort_mode,
            name_filter,
            show_path: false,
            confirming_delete_path: None,
            focused: false,
            on_select: None,
            on_cancel: None,
            on_exit: None,
            on_toggle_scope: None,
            on_toggle_sort: None,
            on_toggle_name_filter: None,
            on_toggle_path: None,
            on_delete_confirmation_change: None,
            on_delete_session: None,
            on_rename_session: None,
            on_error: None,
        };
        Self::filter_sessions_locked(&mut state, "");

        let list = Self {
            inner: Arc::new(SessionListInner {
                search_input: search_input.clone(),
                keybindings,
                current_session_path_key,
                max_visible: 10,
                state: Mutex::new(state),
            }),
        };

        let submit_list = list.clone();
        search_input.set_on_submit(Some(Arc::new(move |_| {
            let (selected_path, on_select) = {
                let state = submit_list.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                let path = state
                    .filtered_sessions
                    .get(state.selected_index)
                    .map(|n| n.session.path.clone());
                (path, state.on_select.clone())
            };
            if let (Some(path), Some(cb)) = (selected_path, on_select) {
                cb(&path);
            }
        })));

        list
    }

    pub fn get_selected_session_path(&self) -> Option<String> {
        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .filtered_sessions
            .get(state.selected_index)
            .map(|n| n.session.path.clone())
    }

    pub fn set_sort_mode(&self, sort_mode: SortMode) {
        let query = self.inner.search_input.get_value();
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        state.sort_mode = sort_mode;
        Self::filter_sessions_locked(&mut state, &query);
    }

    pub fn set_name_filter(&self, name_filter: NameFilter) {
        let query = self.inner.search_input.get_value();
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        state.name_filter = name_filter;
        Self::filter_sessions_locked(&mut state, &query);
    }

    pub fn set_sessions(&self, sessions: Vec<SessionInfo>, show_cwd: bool, partial: bool) {
        let query = self.inner.search_input.get_value();
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        let selected_path = if state.selection_touched {
            state.pending_selected_path.clone().or_else(|| {
                state
                    .filtered_sessions
                    .get(state.selected_index)
                    .map(|n| n.session.path.clone())
            })
        } else {
            None
        };
        state.all_sessions = sessions;
        state.show_cwd = show_cwd;
        Self::filter_sessions_locked(&mut state, &query);
        if !state.selection_touched {
            state.selected_index = 0;
        } else if let Some(target_path) = selected_path {
            if let Some(pos) = state
                .filtered_sessions
                .iter()
                .position(|n| n.session.path == target_path)
            {
                state.selected_index = pos;
                state.pending_selected_path = None;
            } else if partial {
                state.pending_selected_path = Some(target_path);
            } else {
                state.pending_selected_path = None;
            }
        }
    }

    pub fn set_on_select(&self, cb: Option<Arc<dyn Fn(&str) + Send + Sync>>) {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).on_select = cb;
    }

    pub fn set_on_cancel(&self, cb: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).on_cancel = cb;
    }

    pub fn set_on_exit(&self, cb: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).on_exit = cb;
    }

    pub fn set_on_toggle_scope(&self, cb: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .on_toggle_scope = cb;
    }

    pub fn set_on_toggle_sort(&self, cb: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .on_toggle_sort = cb;
    }

    pub fn set_on_toggle_name_filter(&self, cb: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .on_toggle_name_filter = cb;
    }

    pub fn set_on_toggle_path(&self, cb: Option<Arc<dyn Fn(bool) + Send + Sync>>) {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .on_toggle_path = cb;
    }

    pub fn set_on_delete_confirmation_change(&self, cb: Option<Arc<dyn Fn(Option<&str>) + Send + Sync>>) {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .on_delete_confirmation_change = cb;
    }

    pub fn set_on_delete_session(&self, cb: Option<DeleteSessionFn>) {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .on_delete_session = cb;
    }

    pub fn set_on_rename_session(&self, cb: Option<Arc<dyn Fn(&str) + Send + Sync>>) {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .on_rename_session = cb;
    }

    pub fn set_on_error(&self, cb: Option<Arc<dyn Fn(&str) + Send + Sync>>) {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).on_error = cb;
    }

    fn filter_sessions_locked(state: &mut SessionListState, query: &str) {
        let trimmed = query.trim();
        let name_filtered: Vec<SessionInfo> = if state.name_filter == NameFilter::All {
            state.all_sessions.clone()
        } else {
            state
                .all_sessions
                .iter()
                .filter(|s| has_session_name(s))
                .cloned()
                .collect()
        };

        if state.sort_mode == SortMode::Threaded && trimmed.is_empty() {
            state.filtered_sessions = build_and_flatten_session_tree(&name_filtered);
        } else {
            let filtered = filter_and_sort_sessions(&name_filtered, query, state.sort_mode, Some(NameFilter::All));
            state.filtered_sessions = filtered
                .into_iter()
                .map(|session| FlatSessionNode {
                    session,
                    depth: 0,
                    is_last: true,
                    ancestor_continues: Vec::new(),
                })
                .collect();
        }
        state.selected_index = state
            .selected_index
            .min(state.filtered_sessions.len().saturating_sub(1));
    }

    fn is_current_session_path(&self, path: &str) -> bool {
        let Some(ref current_key) = self.inner.current_session_path_key else {
            return false;
        };
        get_session_path_key(path) == *current_key
    }

    fn start_delete_confirmation_for_selected_session(&self) {
        enum ConfirmAction {
            None,
            Error(&'static str, Option<Arc<dyn Fn(&str) + Send + Sync>>),
            Confirm(String, Option<Arc<dyn Fn(Option<&str>) + Send + Sync>>),
        }

        let action = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(selected) = state.filtered_sessions.get(state.selected_index).cloned() {
                if self.is_current_session_path(&selected.session.path) {
                    ConfirmAction::Error("Cannot delete the currently active session", state.on_error.clone())
                } else {
                    state.confirming_delete_path = Some(selected.session.path.clone());
                    ConfirmAction::Confirm(selected.session.path, state.on_delete_confirmation_change.clone())
                }
            } else {
                ConfirmAction::None
            }
        };

        match action {
            ConfirmAction::None => {}
            ConfirmAction::Error(msg, cb) => {
                if let Some(cb) = cb {
                    cb(msg);
                }
            }
            ConfirmAction::Confirm(path, cb) => {
                if let Some(cb) = cb {
                    cb(Some(&path));
                }
            }
        }
    }

    fn build_tree_prefix(node: &FlatSessionNode) -> String {
        if node.depth == 0 {
            return String::new();
        }
        let mut prefix = String::new();
        for &continues in &node.ancestor_continues {
            prefix.push_str(if continues { "│  " } else { "   " });
        }
        prefix.push_str(if node.is_last { "└─ " } else { "├─ " });
        prefix
    }
}

impl Focusable for SessionList {
    fn focused(&self) -> bool {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).focused
    }

    fn set_focused(&self, value: bool) {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).focused = value;
        self.inner.search_input.set_focused(value);
    }
}

impl Component for SessionList {
    fn render(&self, width: usize) -> Vec<String> {
        let t = theme();
        let mut lines = self.inner.search_input.render(width);
        lines.push(String::new());

        let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.filtered_sessions.is_empty() {
            let empty_message = if state.name_filter == NameFilter::Named {
                let toggle_key = key_text(&"app.session.toggleNamedFilter".to_string());
                if state.show_cwd {
                    format!("  No named sessions found. Press {toggle_key} to show all.")
                } else {
                    format!(
                        "  No named sessions in current folder. Press {toggle_key} to show all, or Tab to view all."
                    )
                }
            } else if state.show_cwd {
                "  No sessions found".to_string()
            } else {
                "  No sessions in current folder. Press Tab to view all.".to_string()
            };
            lines.push(t.fg(
                ThemeColor::Muted,
                &truncate_to_width(&empty_message, width as i64, Some("…"), None),
            ));
            return lines;
        }

        let len = state.filtered_sessions.len();
        let max_vis = self.inner.max_visible;
        let max_start = len.saturating_sub(max_vis);
        let start_index = state.selected_index.saturating_sub(max_vis / 2).min(max_start);
        let end_index = (start_index + max_vis).min(len);

        for (i, node) in state
            .filtered_sessions
            .iter()
            .enumerate()
            .take(end_index)
            .skip(start_index)
        {
            let session = &node.session;
            let is_selected = i == state.selected_index;
            let is_confirming_delete = state.confirming_delete_path.as_deref() == Some(session.path.as_str());
            let is_current = self.is_current_session_path(&session.path);

            let prefix = Self::build_tree_prefix(node);
            let has_name = session.name.as_ref().is_some_and(|n| !n.is_empty());
            let display_text = session.name.as_deref().unwrap_or(&session.first_message);
            let normalized_message: String = display_text
                .chars()
                .map(|c| {
                    let code = c as u32;
                    if code <= 0x1f || code == 0x7f { ' ' } else { c }
                })
                .collect::<String>()
                .trim()
                .to_string();

            let age = format_session_date(session.modified);
            let msg_count = session.message_count.to_string();
            let mut right_part = format!("{msg_count} {age}");
            if state.show_cwd && !session.cwd.is_empty() {
                right_part = format!("{} {right_part}", shorten_home_path(&session.cwd));
            }
            if state.show_path {
                right_part = format!("{} {right_part}", shorten_home_path(&session.path));
            }

            let cursor = if is_selected {
                t.fg(ThemeColor::Accent, "› ")
            } else {
                "  ".to_string()
            };

            let prefix_width = visible_width(&prefix).max(0) as usize;
            let right_width = visible_width(&right_part).max(0) as usize + 2;
            let available_for_msg = (width as isize) - 2 - (prefix_width as isize) - (right_width as isize);
            let truncated_msg =
                truncate_to_width(&normalized_message, available_for_msg.max(10) as i64, Some("…"), None);

            let message_color = if is_confirming_delete {
                Some(ThemeColor::Error)
            } else if is_current {
                Some(ThemeColor::Accent)
            } else if has_name {
                Some(ThemeColor::Warning)
            } else {
                None
            };
            let mut styled_msg = match message_color {
                Some(c) => t.fg(c, &truncated_msg),
                None => truncated_msg,
            };
            if is_selected {
                styled_msg = t.bold(&styled_msg);
            }

            let left_part = format!("{cursor}{}{styled_msg}", t.fg(ThemeColor::Dim, &prefix));
            let left_width = visible_width(&left_part).max(0) as usize;
            let right_vis_width = visible_width(&right_part).max(0) as usize;
            let spacing = width.saturating_sub(left_width).saturating_sub(right_vis_width).max(1);
            let styled_right = t.fg(
                if is_confirming_delete {
                    ThemeColor::Error
                } else {
                    ThemeColor::Dim
                },
                &right_part,
            );

            let mut line = format!("{left_part}{}{styled_right}", " ".repeat(spacing));
            if is_selected {
                line = t.bg(ThemeBg::SelectedBg, &line);
            }
            lines.push(truncate_to_width(&line, width as i64, None, None));
        }

        if start_index > 0 || end_index < len {
            let scroll_text = format!("  ({}/{len})", state.selected_index + 1);
            lines.push(t.fg(
                ThemeColor::Muted,
                &truncate_to_width(&scroll_text, width as i64, Some(""), None),
            ));
        }

        lines
    }

    fn handle_input(&self, key_data: &str) {
        let kb = get_keybindings();

        enum ConfirmDecision {
            None,
            Confirm(
                String,
                Option<Arc<dyn Fn(Option<&str>) + Send + Sync>>,
                Option<DeleteSessionFn>,
            ),
            Cancel(Option<Arc<dyn Fn(Option<&str>) + Send + Sync>>),
        }

        let confirm_decision = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref confirming_path) = state.confirming_delete_path.clone() {
                if kb.matches(key_data, "tui.select.confirm") {
                    state.confirming_delete_path = None;
                    ConfirmDecision::Confirm(
                        confirming_path.clone(),
                        state.on_delete_confirmation_change.clone(),
                        state.on_delete_session.clone(),
                    )
                } else if kb.matches(key_data, "tui.select.cancel") {
                    state.confirming_delete_path = None;
                    ConfirmDecision::Cancel(state.on_delete_confirmation_change.clone())
                } else {
                    return;
                }
            } else {
                ConfirmDecision::None
            }
        };

        match confirm_decision {
            ConfirmDecision::Confirm(path, on_confirm_change, on_delete_session) => {
                if let Some(cb) = on_confirm_change {
                    cb(None);
                }
                if let Some(cb) = on_delete_session {
                    let fut = cb(&path);
                    if tokio::runtime::Handle::try_current().is_ok() {
                        tokio::spawn(fut);
                    }
                }
                return;
            }
            ConfirmDecision::Cancel(on_confirm_change) => {
                if let Some(cb) = on_confirm_change {
                    cb(None);
                }
                return;
            }
            ConfirmDecision::None => {}
        }

        if kb.matches(key_data, "tui.input.tab") {
            let cb = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .on_toggle_scope
                .clone();
            if let Some(cb) = cb {
                cb();
            }
            return;
        }

        if kb.matches(key_data, "app.session.toggleSort") {
            let cb = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .on_toggle_sort
                .clone();
            if let Some(cb) = cb {
                cb();
            }
            return;
        }

        if self
            .inner
            .keybindings
            .matches(key_data, &"app.session.toggleNamedFilter".to_string())
        {
            let cb = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .on_toggle_name_filter
                .clone();
            if let Some(cb) = cb {
                cb();
            }
            return;
        }

        if kb.matches(key_data, "app.session.togglePath") {
            let (show_path, cb) = {
                let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                state.show_path = !state.show_path;
                (state.show_path, state.on_toggle_path.clone())
            };
            if let Some(cb) = cb {
                cb(show_path);
            }
            return;
        }

        if kb.matches(key_data, "app.session.delete") {
            self.start_delete_confirmation_for_selected_session();
            return;
        }

        if kb.matches(key_data, "app.session.rename") {
            let (path, cb) = {
                let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                let path = state
                    .filtered_sessions
                    .get(state.selected_index)
                    .map(|n| n.session.path.clone());
                (path, state.on_rename_session.clone())
            };
            if let (Some(p), Some(cb)) = (path, cb) {
                cb(&p);
            }
            return;
        }

        if kb.matches(key_data, "app.session.deleteNoninvasive") {
            if !self.inner.search_input.get_value().is_empty() {
                self.inner.search_input.handle_input(key_data);
                let query = self.inner.search_input.get_value();
                let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                Self::filter_sessions_locked(&mut state, &query);
                return;
            }
            self.start_delete_confirmation_for_selected_session();
            return;
        }

        enum NavAction {
            None,
            Select(String),
            Cancel,
        }

        let (nav, on_select, on_cancel) = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.selection_touched = true;
            state.pending_selected_path = None;
            let mut nav = NavAction::None;

            if kb.matches(key_data, "tui.select.up") {
                state.selected_index = state.selected_index.saturating_sub(1);
            } else if kb.matches(key_data, "tui.select.down") {
                if !state.filtered_sessions.is_empty() {
                    state.selected_index = (state.selected_index + 1).min(state.filtered_sessions.len() - 1);
                }
            } else if kb.matches(key_data, "tui.select.pageUp") {
                state.selected_index = state.selected_index.saturating_sub(self.inner.max_visible);
            } else if kb.matches(key_data, "tui.select.pageDown") {
                if !state.filtered_sessions.is_empty() {
                    state.selected_index =
                        (state.selected_index + self.inner.max_visible).min(state.filtered_sessions.len() - 1);
                }
            } else if kb.matches(key_data, "tui.select.confirm") {
                if let Some(selected) = state.filtered_sessions.get(state.selected_index) {
                    nav = NavAction::Select(selected.session.path.clone());
                }
            } else if kb.matches(key_data, "tui.select.cancel") {
                nav = NavAction::Cancel;
            } else {
                drop(state);
                self.inner.search_input.handle_input(key_data);
                let query = self.inner.search_input.get_value();
                let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                Self::filter_sessions_locked(&mut st, &query);
                return;
            }
            (nav, state.on_select.clone(), state.on_cancel.clone())
        };

        match nav {
            NavAction::None => {}
            NavAction::Select(path) => {
                if let Some(cb) = on_select {
                    cb(&path);
                }
            }
            NavAction::Cancel => {
                if let Some(cb) = on_cancel {
                    cb();
                }
            }
        }
    }

    fn invalidate(&self) {}
}

struct DeleteFileResult {
    ok: bool,
    method: &'static str,
    error: Option<String>,
}

async fn delete_session_file(session_path: &str) -> DeleteFileResult {
    let trash_args: Vec<&str> = if session_path.starts_with('-') {
        vec!["--", session_path]
    } else {
        vec![session_path]
    };

    let mut cmd = tokio::process::Command::new("trash");
    cmd.args(trash_args).env_clear().envs(pi_js::env::vars());
    let trash_output = cmd.output().await;

    let trash_succeeded = match &trash_output {
        Ok(out) => out.status.success() || !Path::new(session_path).exists(),
        Err(_) => !Path::new(session_path).exists(),
    };
    if trash_succeeded {
        return DeleteFileResult {
            ok: true,
            method: "trash",
            error: None,
        };
    }

    let trash_error_hint: Option<String> = match trash_output {
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            if stderr.is_empty() {
                None
            } else {
                let first = stderr.lines().next().unwrap_or(&stderr);
                let msg = format!("trash: {first}");
                Some(msg.chars().take(200).collect())
            }
        }
        Err(err) => {
            let msg = format!("trash: {err}");
            Some(msg.chars().take(200).collect())
        }
    };

    match tokio::fs::remove_file(session_path).await {
        Ok(()) => DeleteFileResult {
            ok: true,
            method: "unlink",
            error: None,
        },
        Err(err) => {
            let unlink_err = err.to_string();
            let error = match trash_error_hint {
                Some(hint) => format!("{unlink_err} ({hint})"),
                None => unlink_err,
            };
            DeleteFileResult {
                ok: false,
                method: "unlink",
                error: Some(error),
            }
        }
    }
}

struct SessionSelectorState {
    scope: SessionScope,
    sort_mode: SortMode,
    name_filter: NameFilter,
    current_sessions: Option<Vec<SessionInfo>>,
    all_sessions: Option<Vec<SessionInfo>>,
    current_load: Option<Arc<AbortController>>,
    all_load: Option<Arc<AbortController>>,
    mode: SelectorMode,
    rename_target_path: Option<String>,
    focused: bool,
}

struct SessionSelectorInner {
    container: Container,
    session_list: SessionList,
    header: SessionSelectorHeader,
    rename_input: Input,
    current_sessions_loader: SessionsLoader,
    all_sessions_loader: SessionsLoader,
    request_render: Arc<dyn Fn() + Send + Sync>,
    rename_session: Option<RenameSessionFn>,
    state: Mutex<SessionSelectorState>,
}

#[derive(Clone)]
pub struct SessionSelectorComponent {
    inner: Arc<SessionSelectorInner>,
}

impl SessionSelectorComponent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        current_sessions_loader: SessionsLoader,
        all_sessions_loader: SessionsLoader,
        on_select: Arc<dyn Fn(&str) + Send + Sync>,
        on_cancel: Arc<dyn Fn() + Send + Sync>,
        on_exit: Arc<dyn Fn() + Send + Sync>,
        request_render: Arc<dyn Fn() + Send + Sync>,
        options: SessionSelectorOptions,
        current_session_file_path: Option<&str>,
    ) -> Self {
        let keybindings = options.keybindings.unwrap_or_else(|| Arc::new(get_keybindings()));
        let scope = SessionScope::Current;
        let sort_mode = SortMode::Threaded;
        let name_filter = NameFilter::All;

        let header = SessionSelectorHeader::new(scope, sort_mode, name_filter, Arc::clone(&request_render));
        let can_rename = options.rename_session.is_some();
        header.set_show_rename_hint(options.show_rename_hint.unwrap_or(can_rename));

        let session_list = SessionList::new(
            Vec::new(),
            false,
            sort_mode,
            name_filter,
            keybindings,
            current_session_file_path,
        );
        let container = Container::new();
        let rename_input = Input::new(None);

        let comp = Self {
            inner: Arc::new(SessionSelectorInner {
                container,
                session_list: session_list.clone(),
                header: header.clone(),
                rename_input: rename_input.clone(),
                current_sessions_loader,
                all_sessions_loader,
                request_render,
                rename_session: options.rename_session,
                state: Mutex::new(SessionSelectorState {
                    scope,
                    sort_mode,
                    name_filter,
                    current_sessions: None,
                    all_sessions: None,
                    current_load: None,
                    all_load: None,
                    mode: SelectorMode::List,
                    rename_target_path: None,
                    focused: false,
                }),
            }),
        };

        comp.build_base_layout(Arc::new(session_list.clone()), true);

        let rename_comp = comp.clone();
        rename_input.set_on_submit(Some(Arc::new(move |value| {
            let c = rename_comp.clone();
            let v = value.to_string();
            if tokio::runtime::Handle::try_current().is_ok() {
                tokio::spawn(async move {
                    c.confirm_rename(&v).await;
                });
            }
        })));

        let select_comp = comp.clone();
        session_list.set_on_select(Some(Arc::new(move |session_path| {
            select_comp.inner.header.set_status_message(None, None);
            select_comp.cancel_loads();
            on_select(session_path);
        })));

        let cancel_comp = comp.clone();
        session_list.set_on_cancel(Some(Arc::new(move || {
            cancel_comp.inner.header.set_status_message(None, None);
            cancel_comp.cancel_loads();
            on_cancel();
        })));

        let exit_comp = comp.clone();
        session_list.set_on_exit(Some(Arc::new(move || {
            exit_comp.inner.header.set_status_message(None, None);
            exit_comp.cancel_loads();
            on_exit();
        })));

        let scope_comp = comp.clone();
        session_list.set_on_toggle_scope(Some(Arc::new(move || {
            scope_comp.toggle_scope();
        })));

        let sort_comp = comp.clone();
        session_list.set_on_toggle_sort(Some(Arc::new(move || {
            sort_comp.toggle_sort_mode();
        })));

        let name_comp = comp.clone();
        session_list.set_on_toggle_name_filter(Some(Arc::new(move || {
            name_comp.toggle_name_filter();
        })));

        let rename_req_comp = comp.clone();
        session_list.set_on_rename_session(Some(Arc::new(move |session_path| {
            if rename_req_comp.inner.rename_session.is_none() {
                return;
            }
            let current_name = {
                let state = rename_req_comp.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                let loading = match state.scope {
                    SessionScope::Current => state.current_load.is_some(),
                    SessionScope::All => state.all_load.is_some(),
                };
                if loading {
                    return;
                }
                let sessions = match state.scope {
                    SessionScope::All => state.all_sessions.as_deref().unwrap_or(&[]),
                    SessionScope::Current => state.current_sessions.as_deref().unwrap_or(&[]),
                };
                sessions
                    .iter()
                    .find(|s| s.path == session_path)
                    .and_then(|s| s.name.clone())
            };
            rename_req_comp.enter_rename_mode(session_path, current_name.as_deref());
        })));

        let path_comp = comp.clone();
        session_list.set_on_toggle_path(Some(Arc::new(move |show_path| {
            path_comp.inner.header.set_show_path(show_path);
            (path_comp.inner.request_render)();
        })));

        let del_conf_comp = comp.clone();
        session_list.set_on_delete_confirmation_change(Some(Arc::new(move |path| {
            del_conf_comp
                .inner
                .header
                .set_confirming_delete_path(path.map(str::to_string));
            (del_conf_comp.inner.request_render)();
        })));

        let err_comp = comp.clone();
        session_list.set_on_error(Some(Arc::new(move |msg| {
            err_comp.inner.header.set_status_message(
                Some(StatusMessage {
                    kind: StatusMessageKind::Error,
                    message: msg.to_string(),
                }),
                Some(3000),
            );
            (err_comp.inner.request_render)();
        })));

        let del_comp = comp.clone();
        session_list.set_on_delete_session(Some(Arc::new(move |session_path| {
            let c = del_comp.clone();
            let path = session_path.to_string();
            Box::pin(async move {
                c.handle_delete_session(&path).await;
            })
        })));

        comp.start_load_scope(SessionScope::Current);

        comp
    }

    pub fn captures_viewport_input(&self) -> bool {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).mode != SelectorMode::Rename
    }

    pub fn dispose(&self) {
        self.inner.header.set_status_message(None, None);
        self.cancel_loads();
    }

    pub fn get_session_list(&self) -> &SessionList {
        &self.inner.session_list
    }

    fn build_base_layout(&self, content: Arc<dyn Component>, show_header: bool) {
        let t = theme();
        self.inner.container.clear();
        self.inner.container.add_child(Arc::new(Spacer::new(Some(1))));
        self.inner
            .container
            .add_child(Arc::new(DynamicBorder::new(Some(Arc::new(move |s| {
                t.fg(ThemeColor::Accent, s)
            })))));
        self.inner.container.add_child(Arc::new(Spacer::new(Some(1))));
        if show_header {
            self.inner.container.add_child(Arc::new(self.inner.header.clone()));
            self.inner.container.add_child(Arc::new(Spacer::new(Some(1))));
        }
        self.inner.container.add_child(content);
        self.inner.container.add_child(Arc::new(Spacer::new(Some(1))));
        let t2 = theme();
        self.inner
            .container
            .add_child(Arc::new(DynamicBorder::new(Some(Arc::new(move |s| {
                t2.fg(ThemeColor::Accent, s)
            })))));
    }

    fn cancel_loads(&self) {
        let (curr, all) = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let c = state.current_load.take();
            let a = state.all_load.take();
            if c.is_some() {
                state.current_sessions = None;
            }
            if a.is_some() {
                state.all_sessions = None;
            }
            (c, a)
        };
        if let Some(c) = curr {
            c.abort(None);
        }
        if let Some(a) = all {
            a.abort(None);
        }
    }

    fn enter_rename_mode(&self, session_path: &str, current_name: Option<&str>) {
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.mode = SelectorMode::Rename;
            state.rename_target_path = Some(session_path.to_string());
        }
        self.inner.rename_input.set_value(current_name.unwrap_or(""));
        self.inner.rename_input.set_focused(true);

        let panel = Container::new();
        panel.add_child(Arc::new(Text::new(
            Some(&theme().bold("Rename Session")),
            Some(1),
            Some(0),
            None,
        )));
        panel.add_child(Arc::new(Spacer::new(Some(1))));
        panel.add_child(Arc::new(self.inner.rename_input.clone()));
        panel.add_child(Arc::new(Spacer::new(Some(1))));
        panel.add_child(Arc::new(Text::new(
            Some(&theme().fg(
                ThemeColor::Muted,
                &format!(
                    "{} to save · {} to cancel",
                    key_text(&"tui.select.confirm".to_string()),
                    key_text(&"tui.select.cancel".to_string())
                ),
            )),
            Some(1),
            Some(0),
            None,
        )));

        self.build_base_layout(Arc::new(panel), false);
        (self.inner.request_render)();
    }

    fn exit_rename_mode(&self) {
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.mode = SelectorMode::List;
            state.rename_target_path = None;
        }
        self.build_base_layout(Arc::new(self.inner.session_list.clone()), true);
        (self.inner.request_render)();
    }

    async fn confirm_rename(&self, value: &str) {
        let next = value.trim();
        if next.is_empty() {
            return;
        }
        let target = {
            let state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.rename_target_path.clone()
        };
        let Some(target) = target else {
            self.exit_rename_mode();
            return;
        };
        let Some(ref rename_fn) = self.inner.rename_session else {
            self.exit_rename_mode();
            return;
        };
        let _ = rename_fn(target, Some(next.to_string())).await;
        self.refresh_sessions_after_mutation().await;
        self.exit_rename_mode();
    }

    async fn handle_delete_session(&self, session_path: &str) {
        let result = delete_session_file(session_path).await;
        if result.ok {
            let (sessions, show_cwd) = {
                let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ref mut curr) = state.current_sessions {
                    curr.retain(|s| s.path != session_path);
                }
                if let Some(ref mut all) = state.all_sessions {
                    all.retain(|s| s.path != session_path);
                }
                let show_cwd = state.scope == SessionScope::All;
                let sessions = if show_cwd {
                    state.all_sessions.clone().unwrap_or_default()
                } else {
                    state.current_sessions.clone().unwrap_or_default()
                };
                (sessions, show_cwd)
            };
            self.inner.session_list.set_sessions(sessions, show_cwd, false);

            let msg = if result.method == "trash" {
                "Session moved to trash"
            } else {
                "Session deleted"
            };
            self.inner.header.set_status_message(
                Some(StatusMessage {
                    kind: StatusMessageKind::Info,
                    message: msg.to_string(),
                }),
                Some(2000),
            );
            self.refresh_sessions_after_mutation().await;
        } else {
            let err_msg = result.error.unwrap_or_else(|| "Unknown error".to_string());
            self.inner.header.set_status_message(
                Some(StatusMessage {
                    kind: StatusMessageKind::Error,
                    message: format!("Failed to delete: {err_msg}"),
                }),
                Some(3000),
            );
        }
        (self.inner.request_render)();
    }

    fn start_load_scope(&self, scope: SessionScope) -> Option<tokio::task::JoinHandle<()>> {
        let controller = Arc::new(AbortController::new());
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let already_loading = match scope {
                SessionScope::Current => state.current_load.is_some(),
                SessionScope::All => state.all_load.is_some(),
            };
            if already_loading {
                return None;
            }
            match scope {
                SessionScope::Current => state.current_load = Some(Arc::clone(&controller)),
                SessionScope::All => state.all_load = Some(Arc::clone(&controller)),
            }
        }

        let show_cwd = scope == SessionScope::All;
        self.inner.header.set_scope(scope);
        self.inner.header.set_loading(true);
        (self.inner.request_render)();

        let prog_comp = self.clone();
        let prog_ctrl = Arc::clone(&controller);
        let on_progress: SessionListProgress = Arc::new(move |loaded, total, partial_sessions| {
            let active_scope = {
                let mut state = prog_comp.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                let cur = match scope {
                    SessionScope::Current => state.current_load.as_ref(),
                    SessionScope::All => state.all_load.as_ref(),
                };
                if !cur.is_some_and(|c| Arc::ptr_eq(c, &prog_ctrl)) {
                    return;
                }
                if let Some(partial) = partial_sessions {
                    let owned = partial.to_vec();
                    match scope {
                        SessionScope::Current => state.current_sessions = Some(owned),
                        SessionScope::All => state.all_sessions = Some(owned),
                    }
                }
                state.scope
            };
            if let Some(partial) = partial_sessions
                && scope == active_scope
            {
                prog_comp
                    .inner
                    .session_list
                    .set_sessions(partial.to_vec(), show_cwd, true);
            }
            if scope != active_scope {
                return;
            }
            prog_comp.inner.header.set_progress(loaded, total);
            (prog_comp.inner.request_render)();
        });

        let signal = controller.signal();
        let fut = match scope {
            SessionScope::Current => (self.inner.current_sessions_loader)(Some(on_progress), signal),
            SessionScope::All => (self.inner.all_sessions_loader)(Some(on_progress), signal),
        };

        let finish_comp = self.clone();
        let finish_ctrl = Arc::clone(&controller);
        let finish_fut = async move {
            let loader_res = fut.await;
            let is_active = {
                let state = finish_comp.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                let cur = match scope {
                    SessionScope::Current => state.current_load.as_ref(),
                    SessionScope::All => state.all_load.as_ref(),
                };
                cur.is_some_and(|c| Arc::ptr_eq(c, &finish_ctrl))
            };
            if !is_active {
                return;
            }

            match loader_res {
                Ok(sessions) => {
                    let active_scope = {
                        let mut state = finish_comp.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                        match scope {
                            SessionScope::Current => {
                                state.current_sessions = Some(sessions.clone());
                                state.current_load = None;
                            }
                            SessionScope::All => {
                                state.all_sessions = Some(sessions.clone());
                                state.all_load = None;
                            }
                        }
                        state.scope
                    };
                    if scope != active_scope {
                        return;
                    }
                    finish_comp.inner.header.set_loading(false);
                    finish_comp.inner.session_list.set_sessions(sessions, show_cwd, false);
                    (finish_comp.inner.request_render)();
                }
                Err(err) => {
                    let active_scope = {
                        let mut state = finish_comp.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                        match scope {
                            SessionScope::Current => {
                                state.current_load = None;
                                state.current_sessions = None;
                            }
                            SessionScope::All => {
                                state.all_load = None;
                                state.all_sessions = None;
                            }
                        }
                        state.scope
                    };
                    if scope != active_scope {
                        return;
                    }
                    finish_comp.inner.header.set_loading(false);
                    finish_comp.inner.header.set_status_message(
                        Some(StatusMessage {
                            kind: StatusMessageKind::Error,
                            message: format!("Failed to load sessions: {err}"),
                        }),
                        Some(4000),
                    );
                    finish_comp.inner.session_list.set_sessions(Vec::new(), show_cwd, false);
                    (finish_comp.inner.request_render)();
                }
            }
        };

        if tokio::runtime::Handle::try_current().is_ok() {
            Some(tokio::spawn(finish_fut))
        } else {
            None
        }
    }

    async fn load_scope(&self, scope: SessionScope) {
        if let Some(handle) = self.start_load_scope(scope) {
            let _ = handle.await;
        }
    }

    fn toggle_sort_mode(&self) {
        let sort_mode = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.sort_mode = match state.sort_mode {
                SortMode::Threaded => SortMode::Recent,
                SortMode::Recent => SortMode::Relevance,
                SortMode::Relevance => SortMode::Threaded,
            };
            state.sort_mode
        };
        self.inner.header.set_sort_mode(sort_mode);
        self.inner.session_list.set_sort_mode(sort_mode);
        (self.inner.request_render)();
    }

    fn toggle_name_filter(&self) {
        let name_filter = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.name_filter = match state.name_filter {
                NameFilter::All => NameFilter::Named,
                NameFilter::Named => NameFilter::All,
            };
            state.name_filter
        };
        self.inner.header.set_name_filter(name_filter);
        self.inner.session_list.set_name_filter(name_filter);
        (self.inner.request_render)();
    }

    async fn refresh_sessions_after_mutation(&self) {
        self.cancel_loads();
        let scope = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.current_sessions = None;
            state.all_sessions = None;
            state.scope
        };
        self.load_scope(scope).await;
    }

    fn toggle_scope(&self) {
        let (scope, sessions, loading) = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.scope = if state.scope == SessionScope::Current {
                SessionScope::All
            } else {
                SessionScope::Current
            };
            let sessions = match state.scope {
                SessionScope::Current => state.current_sessions.clone(),
                SessionScope::All => state.all_sessions.clone(),
            };
            let loading = match state.scope {
                SessionScope::Current => state.current_load.is_some(),
                SessionScope::All => state.all_load.is_some(),
            };
            (state.scope, sessions, loading)
        };
        let is_none = sessions.is_none();
        self.inner.header.set_scope(scope);
        self.inner.header.set_loading(loading);
        self.inner.session_list.set_sessions(
            sessions.unwrap_or_default(),
            scope == SessionScope::All,
            loading || is_none,
        );
        (self.inner.request_render)();
        if is_none && !loading {
            self.start_load_scope(scope);
        }
    }
}

impl Focusable for SessionSelectorComponent {
    fn focused(&self) -> bool {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).focused
    }

    fn set_focused(&self, value: bool) {
        let mode = {
            let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            state.focused = value;
            state.mode
        };
        self.inner.session_list.set_focused(value);
        self.inner.rename_input.set_focused(value);
        if value && mode == SelectorMode::Rename {
            self.inner.rename_input.set_focused(true);
        }
    }
}

impl Component for SessionSelectorComponent {
    fn render(&self, width: usize) -> Vec<String> {
        self.inner.container.render(width)
    }

    fn handle_input(&self, data: &str) {
        let mode = self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).mode;
        if mode == SelectorMode::Rename {
            let kb = get_keybindings();
            if kb.matches(data, "tui.select.cancel") {
                self.exit_rename_mode();
                return;
            }
            self.inner.rename_input.handle_input(data);
            return;
        }
        self.inner.session_list.handle_input(data);
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.container.handle_mouse(event)
    }

    fn invalidate(&self) {
        self.inner.container.invalidate();
    }
}
