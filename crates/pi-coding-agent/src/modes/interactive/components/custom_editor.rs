//! Port of packages/coding-agent/src/modes/interactive/components/custom-editor.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_tui::autocomplete::AutocompleteProvider;
use pi_tui::components::editor::EditorCursor;
use pi_tui::{
    Component, Editor, EditorOptions, EditorTheme, Focusable, KeybindingsManager, TUI, TuiMouseEvent,
    TuiMouseEventResult, strip_terminal_sequences, visible_width,
};

use super::status_indicator::StatusIndicator;

type BorderColor = Arc<dyn Fn(&str) -> String + Send + Sync>;
type TextCallback = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CustomEditorOptions {
    pub padding_x: Option<f64>,
    pub autocomplete_max_visible: Option<f64>,
    /// Render working, compaction, summarization, and retry status in the editor's top border.
    pub embed_working_status: Option<bool>,
}

struct CustomEditorState {
    working_status_indicator: Option<StatusIndicator>,
    action_handlers: IndexMap<String, Arc<dyn Fn() + Send + Sync>>,
    on_escape: Option<Arc<dyn Fn() + Send + Sync>>,
    on_ctrl_d: Option<Arc<dyn Fn() + Send + Sync>>,
    on_paste_image: Option<Arc<dyn Fn() + Send + Sync>>,
    on_extension_shortcut: Option<Arc<dyn Fn(&str) -> bool + Send + Sync>>,
}

struct CustomEditorInner {
    editor: Editor,
    // WIRE(@tui-w3): Use `crate::core::keybindings::KeybindingsManager` once @mcp merges onto main.
    keybindings: KeybindingsManager,
    embed_working_status: bool,
    state: Mutex<CustomEditorState>,
}

/// Custom editor that handles app-level keybindings for coding-agent.
#[derive(Clone)]
pub struct CustomEditor {
    inner: Arc<CustomEditorInner>,
}

impl CustomEditor {
    pub fn new(
        tui: Arc<dyn TUI + Send + Sync>,
        theme: EditorTheme,
        keybindings: KeybindingsManager,
        options: Option<CustomEditorOptions>,
    ) -> Self {
        let opts = options.unwrap_or_default();
        let editor_opts = EditorOptions {
            padding_x: opts.padding_x,
            autocomplete_max_visible: opts.autocomplete_max_visible,
        };
        let editor = Editor::new(tui, theme, Some(editor_opts));
        Self {
            inner: Arc::new(CustomEditorInner {
                editor,
                keybindings,
                embed_working_status: opts.embed_working_status.unwrap_or(false),
                state: Mutex::new(CustomEditorState {
                    working_status_indicator: None,
                    action_handlers: IndexMap::new(),
                    on_escape: None,
                    on_ctrl_d: None,
                    on_paste_image: None,
                    on_extension_shortcut: None,
                }),
            }),
        }
    }

    pub fn embed_working_status(&self) -> bool {
        self.inner.embed_working_status
    }

    pub fn set_working_status_indicator(&self, indicator: Option<StatusIndicator>) {
        self.inner.state.lock().unwrap().working_status_indicator = indicator;
    }

    pub fn set_on_escape(&self, handler: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.inner.state.lock().unwrap().on_escape = handler;
    }

    pub fn set_on_ctrl_d(&self, handler: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.inner.state.lock().unwrap().on_ctrl_d = handler;
    }

    pub fn set_on_paste_image(&self, handler: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.inner.state.lock().unwrap().on_paste_image = handler;
    }

    pub fn set_on_extension_shortcut(&self, handler: Option<Arc<dyn Fn(&str) -> bool + Send + Sync>>) {
        self.inner.state.lock().unwrap().on_extension_shortcut = handler;
    }

    /// Register a handler for an app action.
    pub fn on_action(&self, action: &str, handler: Arc<dyn Fn() + Send + Sync>) {
        self.inner
            .state
            .lock()
            .unwrap()
            .action_handlers
            .insert(action.to_string(), handler);
    }

    pub fn editor(&self) -> &Editor {
        &self.inner.editor
    }

    pub fn focused(&self) -> bool {
        self.inner.editor.focused()
    }

    pub fn set_focused(&self, focused: bool) {
        self.inner.editor.set_focused(focused);
    }

    pub fn border_color(&self) -> BorderColor {
        self.inner.editor.border_color()
    }

    pub fn set_border_color(&self, border_color: BorderColor) {
        self.inner.editor.set_border_color(border_color);
    }

    pub fn set_on_submit(&self, on_submit: Option<TextCallback>) {
        self.inner.editor.set_on_submit(on_submit);
    }

    pub fn set_on_change(&self, on_change: Option<TextCallback>) {
        self.inner.editor.set_on_change(on_change);
    }

    pub fn disable_submit(&self) -> bool {
        self.inner.editor.disable_submit()
    }

    pub fn set_disable_submit(&self, disable_submit: bool) {
        self.inner.editor.set_disable_submit(disable_submit);
    }

    pub fn get_padding_x(&self) -> i64 {
        self.inner.editor.get_padding_x()
    }

    pub fn set_padding_x(&self, padding: f64) {
        self.inner.editor.set_padding_x(padding);
    }

    pub fn set_autocomplete_max_visible(&self, max_visible: f64) {
        self.inner.editor.set_autocomplete_max_visible(max_visible);
    }

    pub fn set_autocomplete_provider(&self, provider: Arc<dyn AutocompleteProvider + Send + Sync>) {
        self.inner.editor.set_autocomplete_provider(provider);
    }

    pub fn add_to_history(&self, text: &str) {
        self.inner.editor.add_to_history(text);
    }

    pub fn get_text(&self) -> String {
        self.inner.editor.get_text()
    }

    pub fn get_expanded_text(&self) -> String {
        self.inner.editor.get_expanded_text()
    }

    pub fn get_lines(&self) -> Vec<String> {
        self.inner.editor.get_lines()
    }

    pub fn get_cursor(&self) -> EditorCursor {
        self.inner.editor.get_cursor()
    }

    pub fn set_text(&self, text: &str) {
        self.inner.editor.set_text(text);
    }

    pub fn insert_text_at_cursor(&self, text: &str) {
        self.inner.editor.insert_text_at_cursor(text);
    }

    pub fn is_showing_autocomplete(&self) -> bool {
        self.inner.editor.is_showing_autocomplete()
    }

    fn parse_hidden_line_count(top_border_line: &str) -> i64 {
        let stripped = strip_terminal_sequences(top_border_line);
        let Some(start) = stripped.find(" ↑ ") else {
            return 0;
        };
        let rest = &stripped[start + " ↑ ".len()..];
        let Some(end) = rest.find(" more ") else {
            return 0;
        };
        rest[..end].trim().parse::<i64>().unwrap_or(0)
    }

    fn render_top_border(&self, width: usize, hidden_line_count: i64, default_line: &str) -> String {
        let indicator = self.inner.state.lock().unwrap().working_status_indicator.clone();
        let Some(indicator) = indicator else {
            return default_line.to_string();
        };
        if !self.inner.embed_working_status || width == 0 {
            return default_line.to_string();
        }

        let mut status = indicator.render_in_border(width.saturating_sub(5).max(1));
        let mut status_width = usize::try_from(visible_width(&status).max(0)).unwrap_or(0);
        if status_width == 0 {
            return default_line.to_string();
        }

        let overflow_label = (hidden_line_count > 0).then(|| format!(" ↑ {hidden_line_count} more "));
        let overflow_label_width = overflow_label
            .as_deref()
            .map(|l| usize::try_from(visible_width(l).max(0)).unwrap_or(0))
            .unwrap_or(0);
        let overflow_start = width.saturating_sub(overflow_label_width) / 2;
        let can_fit_overflow = |sw: usize| -> bool {
            overflow_label.is_some() && overflow_label_width + 2 <= width && overflow_start >= 3 + sw + 1 + 1
        };

        if overflow_label.is_some() && !can_fit_overflow(status_width) {
            status = indicator.render_spinner_in_border(width);
            status_width = usize::try_from(visible_width(&status).max(0)).unwrap_or(0);
        }

        let border_fn = self.inner.editor.border_color();
        if let Some(ref label) = overflow_label
            && can_fit_overflow(status_width)
        {
            let left_block_width = 3 + status_width + 1;
            let left_fill = "─".repeat(overflow_start.saturating_sub(left_block_width));
            let right_fill = "─".repeat(width.saturating_sub(overflow_start + overflow_label_width));
            return format!(
                "{}{}{}",
                border_fn("── "),
                status,
                border_fn(&format!(" {left_fill}{label}{right_fill}"))
            );
        }

        if width >= status_width + 5 {
            let fill = "─".repeat(width.saturating_sub(status_width + 4));
            return format!("{}{}{}", border_fn("── "), status, border_fn(&format!(" {fill}")));
        }

        status = indicator.render_spinner_in_border(width);
        status_width = usize::try_from(visible_width(&status).max(0)).unwrap_or(0);
        let prefix_width = 3usize.min(width.saturating_sub(status_width));
        let suffix_width = width.saturating_sub(prefix_width + status_width);
        format!(
            "{}{}{}",
            border_fn(&"─".repeat(prefix_width)),
            status,
            border_fn(&"─".repeat(suffix_width))
        )
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        let mut lines = self.inner.editor.render(width);
        if self.inner.embed_working_status && width > 0 && !lines.is_empty() {
            let hidden = Self::parse_hidden_line_count(&lines[0]);
            lines[0] = self.render_top_border(width, hidden, &lines[0]);
        }
        lines
    }

    pub fn handle_input(&self, data: &str) {
        let shortcut = self.inner.state.lock().unwrap().on_extension_shortcut.clone();
        if let Some(cb) = shortcut
            && cb(data)
        {
            return;
        }

        if self.inner.editor.is_showing_autocomplete()
            && (self.inner.keybindings.matches(data, "tui.select.pageUp")
                || self.inner.keybindings.matches(data, "tui.select.pageDown"))
        {
            self.inner.editor.handle_input(data);
            return;
        }

        if self.inner.keybindings.matches(data, "app.clipboard.pasteImage") {
            let cb = self.inner.state.lock().unwrap().on_paste_image.clone();
            if let Some(paste_cb) = cb {
                paste_cb();
            }
            return;
        }

        if self.inner.keybindings.matches(data, "app.interrupt") {
            if !self.inner.editor.is_showing_autocomplete() {
                let handler = {
                    let state = self.inner.state.lock().unwrap();
                    state
                        .on_escape
                        .clone()
                        .or_else(|| state.action_handlers.get("app.interrupt").cloned())
                };
                if let Some(h) = handler {
                    h();
                    return;
                }
            }
            self.inner.editor.handle_input(data);
            return;
        }

        if self.inner.keybindings.matches(data, "app.exit") {
            if self.inner.editor.get_text().is_empty() {
                let handler = {
                    let state = self.inner.state.lock().unwrap();
                    state
                        .on_ctrl_d
                        .clone()
                        .or_else(|| state.action_handlers.get("app.exit").cloned())
                };
                if let Some(h) = handler {
                    h();
                }
                return;
            }
        }

        if self.inner.keybindings.matches(data, "tui.editor.historyPrevious")
            || self.inner.keybindings.matches(data, "tui.editor.historyNext")
        {
            self.inner.editor.handle_input(data);
            return;
        }

        let matched_handler = {
            let state = self.inner.state.lock().unwrap();
            state
                .action_handlers
                .iter()
                .find(|(action, _)| {
                    action.as_str() != "app.interrupt"
                        && action.as_str() != "app.exit"
                        && self.inner.keybindings.matches(data, action)
                })
                .map(|(_, h)| Arc::clone(h))
        };
        if let Some(handler) = matched_handler {
            handler();
            return;
        }

        self.inner.editor.handle_input(data);
    }

    pub fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.editor.handle_mouse(event)
    }

    pub fn invalidate(&self) {
        self.inner.editor.invalidate();
    }
}

impl Focusable for CustomEditor {
    fn focused(&self) -> bool {
        CustomEditor::focused(self)
    }

    fn set_focused(&self, focused: bool) {
        CustomEditor::set_focused(self, focused)
    }
}

impl Component for CustomEditor {
    fn render(&self, width: usize) -> Vec<String> {
        CustomEditor::render(self, width)
    }

    fn handle_input(&self, data: &str) {
        CustomEditor::handle_input(self, data)
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        CustomEditor::handle_mouse(self, event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        CustomEditor::invalidate(self)
    }

    fn has_handle_input(&self) -> bool {
        true
    }

    fn as_focusable(&self) -> Option<&dyn Focusable> {
        Some(self)
    }
}
