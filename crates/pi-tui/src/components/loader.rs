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

impl LoaderInner {
    // Ensure background interval does not outlive a dropped Loader.
}

impl Drop for LoaderInner {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock()
            && let Some(interval) = state.interval.take()
        {
            interval.clear();
        }
    }
}

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
        let loader = Self {
            inner: Arc::new(LoaderInner {
                text: Text::new(Some(""), Some(1), Some(0), None),
                ui,
                state: Mutex::new(LoaderState {
                    frames: DEFAULT_FRAMES.iter().map(|s| (*s).to_string()).collect(),
                    interval_ms: DEFAULT_INTERVAL_MS,
                    current_frame: 0,
                    interval: None,
                    render_indicator_verbatim: false,
                    spinner_color_fn,
                    message_color_fn,
                    message: message.unwrap_or("Loading...").to_string(),
                }),
            }),
        };
        loader.set_indicator(indicator);
        loader
    }

    pub fn set_text(&self, text: &str) {
        self.inner.text.set_text(text);
    }

    pub fn set_custom_bg_fn(&self, custom_bg_fn: Option<ColorFn>) {
        self.inner.text.set_custom_bg_fn(custom_bg_fn);
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        let mut out = vec![String::new()];
        out.extend(self.inner.text.render(width));
        out
    }

    pub fn start(&self) {
        self.update_display();
        self.restart_animation();
    }

    pub fn stop(&self) {
        let interval = self.state().interval.take();
        if let Some(iv) = interval {
            iv.clear();
        }
    }

    pub fn set_message(&self, message: &str) {
        {
            self.state().message = message.to_string();
        }
        self.update_display();
    }

    pub fn invalidate(&self) {
        self.inner.text.invalidate();
        self.update_display();
    }

    pub fn set_indicator(&self, indicator: Option<LoaderIndicatorOptions>) {
        {
            let mut state = self.state();
            state.render_indicator_verbatim = indicator.is_some();
            state.frames = match indicator.as_ref().and_then(|ind| ind.frames.as_ref()) {
                Some(frames) => frames.clone(),
                None => DEFAULT_FRAMES.iter().map(|s| (*s).to_string()).collect(),
            };
            state.interval_ms = indicator
                .and_then(|ind| ind.interval_ms)
                .filter(|&ms| ms > 0)
                .unwrap_or(DEFAULT_INTERVAL_MS);
            state.current_frame = 0;
        }
        self.start();
    }

    fn restart_animation(&self) {
        self.stop();
        let mut state = self.state();
        if state.frames.len() <= 1 {
            return;
        }
        let interval_ms = state.interval_ms.max(1) as u64;
        let weak_inner = Arc::downgrade(&self.inner);
        let interval = pi_js::time::set_interval(interval_ms, move || {
            let Some(inner) = weak_inner.upgrade() else {
                return;
            };
            let loader = Loader { inner };
            {
                let mut state = loader.state();
                if state.frames.is_empty() {
                    return;
                }
                state.current_frame = (state.current_frame + 1) % (state.frames.len() as i64);
            }
            loader.update_display();
        });
        state.interval = Some(interval);
    }

    /// PORT: TS `protected`. Public so another crate can delegate (§9.4).
    pub fn get_rendered_indicator(&self) -> String {
        let (frame, verbatim, spinner_fn) = {
            let state = self.state();
            let idx = if state.current_frame < 0 {
                None
            } else {
                Some(state.current_frame as usize)
            };
            let frame = idx.and_then(|i| state.frames.get(i)).cloned().unwrap_or_default();
            (
                frame,
                state.render_indicator_verbatim,
                Arc::clone(&state.spinner_color_fn),
            )
        };
        if verbatim { frame } else { spinner_fn(&frame) }
    }

    fn update_display(&self) {
        let rendered_frame = self.get_rendered_indicator();
        let (message, message_fn) = {
            let state = self.state();
            (state.message.clone(), Arc::clone(&state.message_color_fn))
        };
        let indicator = if rendered_frame.is_empty() {
            String::new()
        } else {
            format!("{rendered_frame} ")
        };
        self.set_text(&format!("{indicator}{}", message_fn(&message)));
        self.inner.ui.request_render(None);
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
