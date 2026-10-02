//! Port of packages/tui/src/components/loader.ts

#![allow(dead_code, unused_imports, unused_variables)]

use std::sync::{Arc, Mutex, MutexGuard};

use crate::components::text::Text;
use crate::tui::{Component, TUI, TuiMouseEvent, TuiMouseEventResult};

// PORT: `class Loader extends Text` is composition plus delegation (§9.4).

type DynTui = Arc<dyn TUI + Send + Sync>;
type ColorFn = Arc<dyn Fn(&str) -> String + Send + Sync>;

const DEFAULT_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const DEFAULT_INTERVAL_MS: i64 = 80;

/// Field order is the interface: `frames`, `intervalMs`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoaderIndicatorOptions {
    /// Animation frames. Use an empty array to hide the indicator.
    pub frames: Option<Vec<String>>,
    /// Frame interval in milliseconds for animated indicators.
    pub interval_ms: Option<i64>,
}

struct LoaderState {
    frames: Vec<String>,
    interval_ms: i64,
    current_frame: i64,
    interval: Option<pi_js::time::Interval>,
    render_indicator_verbatim: bool,
    spinner_color_fn: ColorFn,
    message_color_fn: ColorFn,
    message: String,
}

struct LoaderInner {
    text: Text,
    ui: DynTui,
    state: Mutex<LoaderState>,
}

/// Loader component that updates with an optional spinning animation.
#[derive(Clone)]
pub struct Loader {
    inner: Arc<LoaderInner>,
}

impl PartialEq for Loader {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for Loader {}

impl Loader {
    fn state(&self) -> MutexGuard<'_, LoaderState> {
        self.inner.state.lock().unwrap()
    }

    /// `message` `None` is `"Loading..."`.
    pub fn new(
        ui: DynTui,
        spinner_color_fn: ColorFn,
        message_color_fn: ColorFn,
        message: Option<&str>,
        indicator: Option<LoaderIndicatorOptions>,
    ) -> Self {
        todo!("port: Loader::new")
    }

    pub fn set_text(&self, text: &str) {
        self.inner.text.set_text(text);
    }

    pub fn set_custom_bg_fn(&self, custom_bg_fn: Option<ColorFn>) {
        self.inner.text.set_custom_bg_fn(custom_bg_fn);
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        todo!("port: Loader::render")
    }

    pub fn start(&self) {
        todo!("port: Loader::start")
    }

    pub fn stop(&self) {
        todo!("port: Loader::stop")
    }

    pub fn set_message(&self, message: &str) {
        todo!("port: Loader::set_message")
    }

    pub fn invalidate(&self) {
        todo!("port: Loader::invalidate")
    }

    pub fn set_indicator(&self, indicator: Option<LoaderIndicatorOptions>) {
        todo!("port: Loader::set_indicator")
    }

    fn restart_animation(&self) {
        todo!("port: Loader::restart_animation")
    }

    /// PORT: TS `protected`. Public so another crate can delegate (§9.4).
    pub fn get_rendered_indicator(&self) -> String {
        todo!("port: Loader::get_rendered_indicator")
    }

    fn update_display(&self) {
        todo!("port: Loader::update_display")
    }
}

impl Component for Loader {
    fn render(&self, width: usize) -> Vec<String> {
        Loader::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        None
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        Loader::invalidate(self)
    }
}
