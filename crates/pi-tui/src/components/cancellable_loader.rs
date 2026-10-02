//! Port of packages/tui/src/components/cancellable-loader.ts

#![allow(dead_code, unused_imports, unused_variables)]

use std::sync::{Arc, Mutex, MutexGuard};

use crate::components::loader::{Loader, LoaderIndicatorOptions};
use crate::tui::{Component, TUI, TuiMouseEvent, TuiMouseEventResult};

// PORT: `class CancellableLoader extends Loader` is composition plus delegation (§9.4).

type DynTui = Arc<dyn TUI + Send + Sync>;
type ColorFn = Arc<dyn Fn(&str) -> String + Send + Sync>;
type AbortCallback = Arc<dyn Fn() + Send + Sync>;

struct CancellableLoaderState {
    abort_controller: pi_js::abort::AbortController,
    on_abort: Option<AbortCallback>,
}

struct CancellableLoaderInner {
    loader: Loader,
    state: Mutex<CancellableLoaderState>,
}

/// Loader that can be cancelled with Escape.
/// Extends Loader with an AbortSignal for cancelling async operations.
///
/// ```text
/// const loader = new CancellableLoader(tui, cyan, dim, "Working...");
/// loader.onAbort = () => done(null);
/// doWork(loader.signal).then(done);
/// ```
#[derive(Clone)]
pub struct CancellableLoader {
    inner: Arc<CancellableLoaderInner>,
}

impl PartialEq for CancellableLoader {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for CancellableLoader {}

impl CancellableLoader {
    fn state(&self) -> MutexGuard<'_, CancellableLoaderState> {
        self.inner.state.lock().unwrap()
    }

    /// Same parameters as [`Loader::new`]. `message` `None` is `"Loading..."`.
    pub fn new(
        ui: DynTui,
        spinner_color_fn: ColorFn,
        message_color_fn: ColorFn,
        message: Option<&str>,
        indicator: Option<LoaderIndicatorOptions>,
    ) -> Self {
        todo!("port: CancellableLoader::new")
    }

    pub fn loader(&self) -> Loader {
        self.inner.loader.clone()
    }

    pub fn set_text(&self, text: &str) {
        self.inner.loader.set_text(text);
    }

    pub fn set_custom_bg_fn(&self, custom_bg_fn: Option<ColorFn>) {
        self.inner.loader.set_custom_bg_fn(custom_bg_fn);
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        self.inner.loader.render(width)
    }

    pub fn start(&self) {
        self.inner.loader.start();
    }

    pub fn stop(&self) {
        self.inner.loader.stop();
    }

    pub fn set_message(&self, message: &str) {
        self.inner.loader.set_message(message);
    }

    pub fn invalidate(&self) {
        self.inner.loader.invalidate();
    }

    pub fn set_indicator(&self, indicator: Option<LoaderIndicatorOptions>) {
        self.inner.loader.set_indicator(indicator);
    }

    /// PORT: TS `protected` on [`Loader`].
    pub fn get_rendered_indicator(&self) -> String {
        self.inner.loader.get_rendered_indicator()
    }

    /// Called when user presses Escape.
    pub fn on_abort(&self) -> Option<AbortCallback> {
        self.state().on_abort.clone()
    }

    pub fn set_on_abort(&self, on_abort: Option<AbortCallback>) {
        self.state().on_abort = on_abort;
    }

    /// AbortSignal that is aborted when user presses Escape.
    pub fn signal(&self) -> pi_js::abort::AbortSignal {
        self.state().abort_controller.signal()
    }

    /// Whether the loader was aborted.
    pub fn aborted(&self) -> bool {
        self.state().abort_controller.signal().aborted()
    }

    pub fn handle_input(&self, data: &str) {
        todo!("port: CancellableLoader::handle_input")
    }

    pub fn dispose(&self) {
        todo!("port: CancellableLoader::dispose")
    }
}

impl Component for CancellableLoader {
    fn render(&self, width: usize) -> Vec<String> {
        CancellableLoader::render(self, width)
    }

    fn handle_input(&self, data: &str) {
        CancellableLoader::handle_input(self, data)
    }

    fn handle_mouse(&self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        None
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn has_handle_input(&self) -> bool {
        true
    }

    fn invalidate(&self) {
        CancellableLoader::invalidate(self)
    }
}
