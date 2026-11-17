//! Port of packages/coding-agent/examples/extensions/modal-editor.ts

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, KeybindingsManager, SessionStartEvent,
};
use pi_tui::autocomplete::AutocompleteProvider;
use pi_tui::components::editor::{Editor, EditorTheme};
use pi_tui::editor_component::{EditorBorderColor, EditorComponent, EditorTextCallback};
use pi_tui::{Component, Focusable, TUI, matches_key, truncate_to_width, visible_width};

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditMode {
    Normal,
    Insert,
}

fn normal_key_seq(data: &str) -> Option<Option<&'static str>> {
    match data {
        "h" => Some(Some("\x1b[D")),
        "j" => Some(Some("\x1b[B")),
        "k" => Some(Some("\x1b[A")),
        "l" => Some(Some("\x1b[C")),
        "0" => Some(Some("\x01")),
        "$" => Some(Some("\x05")),
        "x" => Some(Some("\x1b[3~")),
        "i" | "a" => Some(None),
        _ => None,
    }
}

struct ModalEditor {
    // WIRE(@tui-w3): switch inner Editor to CustomEditor once @tui merges.
    inner: Editor,
    mode: Mutex<EditMode>,
}

impl ModalEditor {
    fn new(tui: Arc<dyn TUI>, theme: EditorTheme, _kb: KeybindingsManager) -> Self {
        Self {
            inner: Editor::new(tui, theme, None),
            mode: Mutex::new(EditMode::Insert),
        }
    }
}

impl Component for ModalEditor {
    fn render(&self, width: usize) -> Vec<String> {
        let mut lines = self.inner.render(width);
        if lines.is_empty() {
            return lines;
        }
        let mode = *self.mode.lock().unwrap_or_else(|e| e.into_inner());
        let label = match mode {
            EditMode::Normal => " NORMAL ",
            EditMode::Insert => " INSERT ",
        };
        if let Some(last) = lines.last_mut() {
            let vis = visible_width(last);
            if vis >= label.len() as i64 {
                let target_w = (width as i64).saturating_sub(label.len() as i64);
                *last = format!("{}{label}", truncate_to_width(last, target_w, Some(""), None));
            }
        }
        lines
    }

    fn handle_input(&self, data: &str) {
        if matches_key(data, "escape") {
            let was_insert = {
                let mut guard = self.mode.lock().unwrap_or_else(|e| e.into_inner());
                if *guard == EditMode::Insert {
                    *guard = EditMode::Normal;
                    true
                } else {
                    false
                }
            };
            if !was_insert {
                self.inner.handle_input(data);
            }
            return;
        }

        let current_mode = *self.mode.lock().unwrap_or_else(|e| e.into_inner());
        if current_mode == EditMode::Insert {
            self.inner.handle_input(data);
            return;
        }

        if let Some(seq_opt) = normal_key_seq(data) {
            if data == "i" {
                let mut guard = self.mode.lock().unwrap_or_else(|e| e.into_inner());
                *guard = EditMode::Insert;
            } else if data == "a" {
                {
                    let mut guard = self.mode.lock().unwrap_or_else(|e| e.into_inner());
                    *guard = EditMode::Insert;
                }
                self.inner.handle_input("\x1b[C");
            } else if let Some(seq) = seq_opt {
                self.inner.handle_input(seq);
            }
            return;
        }

        if data.chars().count() == 1 && data.chars().next().is_some_and(|c| c as u32 >= 32) {
            return;
        }
        self.inner.handle_input(data);
    }

    fn has_handle_input(&self) -> bool {
        true
    }

    fn invalidate(&self) {
        self.inner.invalidate();
    }

    fn as_focusable(&self) -> Option<&dyn Focusable> {
        Some(&self.inner)
    }
}

impl EditorComponent for ModalEditor {
    fn get_text(&self) -> String {
        self.inner.get_text()
    }

    fn set_text(&self, text: &str) {
        self.inner.set_text(text);
    }

    fn on_submit(&self) -> Option<EditorTextCallback> {
        self.inner.on_submit()
    }

    fn set_on_submit(&self, on_submit: Option<EditorTextCallback>) {
        self.inner.set_on_submit(on_submit);
    }

    fn on_change(&self) -> Option<EditorTextCallback> {
        self.inner.on_change()
    }

    fn set_on_change(&self, on_change: Option<EditorTextCallback>) {
        self.inner.set_on_change(on_change);
    }

    fn add_to_history(&self, text: &str) {
        self.inner.add_to_history(text);
    }

    fn insert_text_at_cursor(&self, text: &str) {
        self.inner.insert_text_at_cursor(text);
    }

    fn get_expanded_text(&self) -> String {
        self.inner.get_expanded_text()
    }

    fn set_autocomplete_provider(&self, provider: Arc<dyn AutocompleteProvider>) {
        self.inner.set_autocomplete_provider(provider);
    }

    fn border_color(&self) -> Option<EditorBorderColor> {
        self.inner.border_color()
    }

    fn set_border_color(&self, border_color: Option<EditorBorderColor>) {
        self.inner.set_border_color(border_color);
    }

    fn set_padding_x(&self, padding: f64) {
        self.inner.set_padding_x(padding);
    }

    fn set_autocomplete_max_visible(&self, max_visible: f64) {
        self.inner.set_autocomplete_max_visible(max_visible);
    }
}

pub struct ModalEditorExtension;

#[async_trait]
impl Extension for ModalEditorExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        pi.on::<SessionStartEvent>(Arc::new(|_event, ctx| {
            Box::pin(async move {
                ctx.ui()?.set_editor_component(Some(Arc::new(|tui, theme, kb| {
                    Arc::new(ModalEditor::new(tui, theme, kb))
                })));
                Ok(None)
            })
        }));

        Ok(())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(ModalEditorExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
