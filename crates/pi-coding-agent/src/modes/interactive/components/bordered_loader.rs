//! Port of packages/coding-agent/src/modes/interactive/components/bordered-loader.ts

use std::sync::Arc;

use pi_js::abort::{AbortController, AbortSignal};
use pi_tui::{CancellableLoader, Component, Container, Loader, Spacer, TUI, Text, TuiMouseEvent, TuiMouseEventResult};

use super::super::theme::theme::{Theme, ThemeColor};
use super::dynamic_border::DynamicBorder;
use super::keybinding_hints::key_hint;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BorderedLoaderOptions {
    pub cancellable: Option<bool>,
}

enum InnerLoader {
    Cancellable(CancellableLoader),
    NonCancellable(Loader, AbortController),
}

struct BorderedLoaderInner {
    container: Container,
    loader: InnerLoader,
}

impl Drop for BorderedLoaderInner {
    fn drop(&mut self) {
        match &self.loader {
            InnerLoader::Cancellable(cl) => cl.dispose(),
            InnerLoader::NonCancellable(l, _) => l.stop(),
        }
    }
}

/// Loader wrapped with borders for extension UI.
#[derive(Clone)]
pub struct BorderedLoader {
    inner: Arc<BorderedLoaderInner>,
}

impl BorderedLoader {
    pub fn new(
        tui: Arc<dyn TUI + Send + Sync>,
        t: &Theme,
        message: &str,
        options: Option<BorderedLoaderOptions>,
    ) -> Self {
        let cancellable = options.and_then(|o| o.cancellable).unwrap_or(true);
        let container = Container::new();
        let t_border = t.clone();
        let border_color: Arc<dyn Fn(&str) -> String + Send + Sync> =
            Arc::new(move |s: &str| t_border.fg(ThemeColor::Border, s));

        container.add_child(Arc::new(DynamicBorder::new(Some(Arc::clone(&border_color)))));

        let t_spin = t.clone();
        let t_msg = t.clone();
        let spinner_fn: Arc<dyn Fn(&str) -> String + Send + Sync> =
            Arc::new(move |s: &str| t_spin.fg(ThemeColor::Accent, s));
        let msg_fn: Arc<dyn Fn(&str) -> String + Send + Sync> = Arc::new(move |s: &str| t_msg.fg(ThemeColor::Muted, s));

        let loader = if cancellable {
            let cl = CancellableLoader::new(tui, spinner_fn, msg_fn, Some(message), None);
            container.add_child(Arc::new(cl.clone()));
            InnerLoader::Cancellable(cl)
        } else {
            let l = Loader::new(tui, spinner_fn, msg_fn, Some(message), None);
            container.add_child(Arc::new(l.clone()));
            InnerLoader::NonCancellable(l, AbortController::new())
        };

        if cancellable {
            container.add_child(Arc::new(Spacer::new(Some(1))));
            container.add_child(Arc::new(Text::new(
                Some(&key_hint(&"tui.select.cancel".to_string(), "cancel")),
                Some(1),
                Some(0),
                None,
            )));
        }
        container.add_child(Arc::new(Spacer::new(Some(1))));
        container.add_child(Arc::new(DynamicBorder::new(Some(border_color))));

        Self {
            inner: Arc::new(BorderedLoaderInner { container, loader }),
        }
    }

    pub fn signal(&self) -> AbortSignal {
        match &self.inner.loader {
            InnerLoader::Cancellable(cl) => cl.signal(),
            InnerLoader::NonCancellable(_, ctrl) => ctrl.signal(),
        }
    }

    pub fn set_on_abort(&self, on_abort: Option<Arc<dyn Fn() + Send + Sync>>) {
        if let InnerLoader::Cancellable(cl) = &self.inner.loader {
            cl.set_on_abort(on_abort);
        }
    }

    pub fn handle_input(&self, data: &str) {
        if let InnerLoader::Cancellable(cl) = &self.inner.loader {
            cl.handle_input(data);
        }
    }

    pub fn dispose(&self) {
        match &self.inner.loader {
            InnerLoader::Cancellable(cl) => cl.dispose(),
            InnerLoader::NonCancellable(l, _) => l.stop(),
        }
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        self.inner.container.render(width)
    }

    pub fn invalidate(&self) {
        self.inner.container.invalidate();
    }
}

impl Component for BorderedLoader {
    fn render(&self, width: usize) -> Vec<String> {
        BorderedLoader::render(self, width)
    }

    fn handle_input(&self, data: &str) {
        BorderedLoader::handle_input(self, data)
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.container.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        BorderedLoader::invalidate(self)
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
