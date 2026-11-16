//! Port of packages/coding-agent/src/modes/interactive/components/extension-editor.ts

use std::sync::{Arc, Mutex};

use pi_tui::{
    Component, Container, Editor, EditorOptions, Focusable, KeybindingsManager, Spacer, TUI, Text, TuiMouseEvent,
    TuiMouseEventResult, get_keybindings,
};

use super::super::external_editor::{ExternalEditorOptions, ExternalEditorResult, edit_in_external_editor};
use super::super::theme::theme::{ThemeColor, get_editor_theme, theme};
use super::dynamic_border::DynamicBorder;
use super::keybinding_hints::key_hint;

#[derive(Clone, Debug, Default)]
pub struct ExtensionEditorOptions {
    pub description: Option<String>,
    pub editor: EditorOptions,
}

struct ExtensionEditorState {
    focused: bool,
}

struct ExtensionEditorInner {
    container: Container,
    editor: Editor,
    on_submit: Arc<dyn Fn(&str) + Send + Sync>,
    on_cancel: Arc<dyn Fn() + Send + Sync>,
    tui: Arc<dyn TUI + Send + Sync>,
    // WIRE(@tui-w3): Use `crate::core::keybindings::KeybindingsManager` once @mcp merges onto main.
    keybindings: KeybindingsManager,
    external_editor_command: String,
    external_editor_status: Text,
    state: Mutex<ExtensionEditorState>,
}

/// Multi-line editor component for extensions.
/// Supports Ctrl+G for external editor.
#[derive(Clone)]
pub struct ExtensionEditorComponent {
    inner: Arc<ExtensionEditorInner>,
}

impl ExtensionEditorComponent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tui: Arc<dyn TUI + Send + Sync>,
        keybindings: KeybindingsManager,
        title: &str,
        prefill: Option<&str>,
        on_submit: Arc<dyn Fn(&str) + Send + Sync>,
        on_cancel: Arc<dyn Fn() + Send + Sync>,
        options: Option<ExtensionEditorOptions>,
        external_editor_command: Option<&str>,
    ) -> Self {
        let default_cmd = if pi_js::env::platform() == "win32" {
            "notepad".to_string()
        } else {
            "nano".to_string()
        };
        let resolved_command = external_editor_command
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .or_else(|| pi_js::env::var("VISUAL").filter(|s| !s.is_empty()))
            .or_else(|| pi_js::env::var("EDITOR").filter(|s| !s.is_empty()))
            .unwrap_or(default_cmd);

        let opts = options.unwrap_or_default();
        let t = theme();

        let container = Container::new();
        container.add_child(Arc::new(DynamicBorder::default()));
        container.add_child(Arc::new(Spacer::new(Some(1))));

        container.add_child(Arc::new(Text::new(
            Some(&t.fg(ThemeColor::Accent, title)),
            Some(1),
            Some(0),
            None,
        )));
        if let Some(ref desc) = opts.description
            && !desc.is_empty()
        {
            container.add_child(Arc::new(Spacer::new(Some(1))));
            container.add_child(Arc::new(Text::new(
                Some(&t.fg(ThemeColor::Text, desc)),
                Some(1),
                Some(0),
                None,
            )));
        }
        container.add_child(Arc::new(Spacer::new(Some(1))));

        let editor = Editor::new(Arc::clone(&tui), get_editor_theme(), Some(opts.editor));
        if let Some(prefill_text) = prefill
            && !prefill_text.is_empty()
        {
            editor.set_text(prefill_text);
        }

        let external_editor_status = Text::new(Some(""), Some(1), Some(0), None);
        container.add_child(Arc::new(editor.clone()));
        container.add_child(Arc::new(external_editor_status.clone()));
        container.add_child(Arc::new(Spacer::new(Some(1))));

        let hint = format!(
            "{}  {}  {}  {}",
            key_hint(&"tui.select.confirm".to_string(), "submit"),
            key_hint(&"tui.input.newLine".to_string(), "newline"),
            key_hint(&"tui.select.cancel".to_string(), "cancel"),
            key_hint(&"app.editor.external".to_string(), "external editor"),
        );
        container.add_child(Arc::new(Text::new(Some(&hint), Some(1), Some(0), None)));
        container.add_child(Arc::new(Spacer::new(Some(1))));
        container.add_child(Arc::new(DynamicBorder::default()));

        let comp = Self {
            inner: Arc::new(ExtensionEditorInner {
                container,
                editor,
                on_submit,
                on_cancel,
                tui,
                keybindings,
                external_editor_command: resolved_command,
                external_editor_status,
                state: Mutex::new(ExtensionEditorState { focused: false }),
            }),
        };

        let weak = Arc::downgrade(&comp.inner);
        comp.inner.editor.set_on_submit(Some(Arc::new(move |text: &str| {
            if let Some(inner) = weak.upgrade() {
                (inner.on_submit)(text);
            }
        })));

        comp
    }

    pub fn focused(&self) -> bool {
        self.inner.state.lock().unwrap().focused
    }

    pub fn set_focused(&self, value: bool) {
        self.inner.state.lock().unwrap().focused = value;
        self.inner.editor.set_focused(value);
    }

    pub fn handle_input(&self, key_data: &str) {
        let kb = get_keybindings();
        if kb.matches(key_data, "tui.select.cancel") {
            (self.inner.on_cancel)();
            return;
        }
        if self.inner.keybindings.matches(key_data, "app.editor.external") {
            let inner = Arc::clone(&self.inner);
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    Self::open_external_editor_async(inner).await;
                });
            } else {
                std::thread::spawn(move || {
                    if let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() {
                        rt.block_on(Self::open_external_editor_async(inner));
                    }
                });
            }
            return;
        }
        self.inner.editor.handle_input(key_data);
    }

    async fn open_external_editor_async(inner: Arc<ExtensionEditorInner>) {
        let content = inner.editor.get_text();
        inner.tui.stop(None);
        let result = edit_in_external_editor(ExternalEditorOptions {
            command: inner.external_editor_command.clone(),
            content,
        })
        .await;
        match result {
            Ok(ExternalEditorResult::Complete { content }) => {
                inner.external_editor_status.set_text("");
                inner.editor.set_text(&content);
            }
            Ok(ExternalEditorResult::Failed { message }) => {
                inner
                    .external_editor_status
                    .set_text(&theme().fg(ThemeColor::Warning, &message));
            }
            Err(error) => {
                inner
                    .external_editor_status
                    .set_text(&theme().fg(ThemeColor::Warning, &error.to_string()));
            }
        }
        inner.tui.start();
        inner.tui.request_render(Some(true));
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        self.inner.container.render(width)
    }

    pub fn invalidate(&self) {
        self.inner.container.invalidate();
    }
}

impl Focusable for ExtensionEditorComponent {
    fn focused(&self) -> bool {
        ExtensionEditorComponent::focused(self)
    }

    fn set_focused(&self, focused: bool) {
        ExtensionEditorComponent::set_focused(self, focused)
    }
}

impl Component for ExtensionEditorComponent {
    fn render(&self, width: usize) -> Vec<String> {
        ExtensionEditorComponent::render(self, width)
    }

    fn handle_input(&self, data: &str) {
        ExtensionEditorComponent::handle_input(self, data)
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.container.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        ExtensionEditorComponent::invalidate(self)
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
