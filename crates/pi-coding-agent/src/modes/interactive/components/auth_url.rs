//! Port of packages/coding-agent/src/modes/interactive/components/auth-url.ts

use std::sync::Arc;

use pi_js::BoxFuture;
use pi_tui::{Component, Container, TUI, Text, TuiMouseEvent, TuiMouseEventResult, hyperlink};

use super::super::theme::theme::{ThemeColor, theme};
use super::keybinding_hints::key_hint;
use super::login_dialog::{copy_to_clipboard_with_override, current_copy_to_clipboard_override};

struct AuthUrlInner {
    container: Container,
    url: String,
    request_render: Arc<dyn Fn() + Send + Sync>,
    hint: Text,
}

/// A sign-in URL with a click hint and a copy hint. Hosts call `copy()` when `app.message.copy` is
/// pressed, since a long URL wraps and often cannot be selected or clicked as a whole (SSH, tmux).
#[derive(Clone)]
pub struct AuthUrlComponent {
    inner: Arc<AuthUrlInner>,
}

impl AuthUrlComponent {
    pub fn new(tui: Arc<dyn TUI + Send + Sync>, url: &str) -> Self {
        Self::with_request_render(Arc::new(move || tui.request_render(None)), url)
    }

    pub(crate) fn with_request_render(request_render: Arc<dyn Fn() + Send + Sync>, url: &str) -> Self {
        let container = Container::new();
        let t = theme();
        container.add_child(Arc::new(Text::new(
            Some(&t.fg(ThemeColor::Accent, &hyperlink(url, url))),
            Some(1),
            Some(0),
            None,
        )));
        let hint = Text::new(Some(""), Some(1), Some(0), None);
        container.add_child(Arc::new(hint.clone()));

        let comp = Self {
            inner: Arc::new(AuthUrlInner {
                container,
                url: url.to_string(),
                request_render,
                hint,
            }),
        };
        comp.set_hint(&key_hint(&"app.message.copy".to_string(), "to copy"));
        comp
    }

    pub fn url(&self) -> &str {
        &self.inner.url
    }

    fn set_hint(&self, suffix: &str) {
        let click_hint = if pi_js::env::platform() == "darwin" {
            "Cmd+click to open"
        } else {
            "Ctrl+click to open"
        };
        let t = theme();
        self.inner.hint.set_text(&format!(
            "{} {} {suffix}",
            t.fg(ThemeColor::Dim, &hyperlink(click_hint, &self.inner.url)),
            t.fg(ThemeColor::Dim, "•"),
        ));
        (self.inner.request_render)();
    }

    pub(crate) async fn copy_with_override(
        &self,
        copy_override: Option<Arc<dyn Fn(&str) -> BoxFuture<pi_js::Result<()>> + Send + Sync>>,
    ) {
        match copy_to_clipboard_with_override(&self.inner.url, copy_override).await {
            Ok(()) => self.set_hint(&theme().fg(ThemeColor::Success, "Copied URL to clipboard")),
            Err(err) => self.set_hint(&theme().fg(ThemeColor::Error, &err.to_string())),
        }
    }

    pub async fn copy(&self) {
        let copy_override = current_copy_to_clipboard_override();
        self.copy_with_override(copy_override).await;
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        self.inner.container.render(width)
    }

    pub fn invalidate(&self) {
        self.inner.container.invalidate();
    }
}

impl Component for AuthUrlComponent {
    fn render(&self, width: usize) -> Vec<String> {
        AuthUrlComponent::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.container.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        AuthUrlComponent::invalidate(self)
    }

    fn is_container_handle_mouse(&self) -> bool {
        true
    }

    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        Some(self.inner.container.children())
    }
}
