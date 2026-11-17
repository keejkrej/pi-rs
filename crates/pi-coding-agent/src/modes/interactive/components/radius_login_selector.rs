//! Port of packages/coding-agent/src/modes/interactive/components/radius-login-selector.ts

use std::sync::{Arc, Mutex};

use pi_js::time::{Interval, set_interval};
use pi_tui::colors::ParseColorValue;
use pi_tui::{
    Color, ColorMixSpace, Component, RgbColorValue, TUI, Text, TuiMouseEvent, TuiMouseEventResult, foreground_ansi,
    mix_colors, parse_color,
};

use super::super::theme::theme::{ThemeColor, theme};
use super::extension_selector::ExtensionSelectorComponent;

const CHARS_PER_COLOR: f64 = 4.0;
const CHARS_PER_SECOND: f64 = 10.0;
const ANIMATION_FRAME_MS: u64 = 50;

fn radius_colors() -> [Color; 4] {
    let fallback = Color::Rgb(RgbColorValue {
        r: 77.0,
        g: 154.0,
        b: 191.0,
    });
    [
        parse_color(ParseColorValue::Text("#4d9abf")).unwrap_or(fallback),
        parse_color(ParseColorValue::Text("#83ccd2")).unwrap_or(fallback),
        parse_color(ParseColorValue::Text("#f1be57")).unwrap_or(fallback),
        parse_color(ParseColorValue::Text("#f09082")).unwrap_or(fallback),
    ]
}

fn radius_shimmer(text: &str, elapsed_ms: f64) -> String {
    let colors = radius_colors();
    let mode = theme().get_color_mode();
    let cycle = (colors.len() as f64) * CHARS_PER_COLOR;
    let offset = (elapsed_ms / 1000.0) * CHARS_PER_SECOND;
    let mut result = String::new();
    for (index, ch) in text.chars().enumerate() {
        let position = (((index as f64 - offset) % cycle) + cycle) % cycle;
        let band = (position / CHARS_PER_COLOR).floor() as usize % colors.len();
        let t = position / CHARS_PER_COLOR - (band as f64);
        let amount = t * t * (3.0 - 2.0 * t);
        let from = colors[band];
        let to = colors[(band + 1) % colors.len()];
        let mixed = mix_colors(from, to, amount, Some(ColorMixSpace::Srgb)).unwrap_or(from);
        result.push_str(&foreground_ansi(mixed, mode));
        result.push(ch);
    }
    result.push_str("\x1b[39m");
    result
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RadiusOption {
    pub label: String,
    pub text: String,
}

struct RadiusMenuState {
    animation_timer: Option<Interval>,
    animating: bool,
}

struct RadiusMenuInner {
    selector: ExtensionSelectorComponent,
    radius_option: RadiusOption,
    animation_start_ms: f64,
    state: Mutex<RadiusMenuState>,
}

impl Drop for RadiusMenuInner {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.state.lock()
            && let Some(timer) = guard.animation_timer.take()
        {
            timer.clear();
        }
    }
}

#[derive(Clone)]
pub struct RadiusLoginMenuComponent {
    inner: Arc<RadiusMenuInner>,
}

impl RadiusLoginMenuComponent {
    fn new(
        tui: Arc<dyn TUI + Send + Sync>,
        title: &str,
        options: Vec<String>,
        radius_option: RadiusOption,
        on_select: Arc<dyn Fn(&str) + Send + Sync>,
        on_cancel: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let state_ref: Arc<Mutex<Option<Arc<RadiusMenuInner>>>> = Arc::new(Mutex::new(None));
        let sr1 = Arc::clone(&state_ref);
        let sel_cb: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |opt: &str| {
            if let Some(ref inner) = *sr1.lock().unwrap() {
                Self::stop_animation_inner(inner);
            }
            on_select(opt);
        });
        let sr2 = Arc::clone(&state_ref);
        let can_cb: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(ref inner) = *sr2.lock().unwrap() {
                Self::stop_animation_inner(inner);
            }
            on_cancel();
        });

        let selector = ExtensionSelectorComponent::new(title, options, sel_cb, can_cb, None);
        let inner = Arc::new(RadiusMenuInner {
            selector,
            radius_option,
            animation_start_ms: pi_js::time::performance_now(),
            state: Mutex::new(RadiusMenuState {
                animation_timer: None,
                animating: false,
            }),
        });
        *state_ref.lock().unwrap() = Some(Arc::clone(&inner));

        let weak = Arc::downgrade(&inner);
        let timer = set_interval(ANIMATION_FRAME_MS, move || {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            if inner.state.lock().unwrap().animating {
                tui.request_render(None);
            }
        });
        inner.state.lock().unwrap().animation_timer = Some(timer);

        Self { inner }
    }

    fn stop_animation_inner(inner: &RadiusMenuInner) {
        let mut guard = inner.state.lock().unwrap();
        if let Some(timer) = guard.animation_timer.take() {
            timer.clear();
        }
        guard.animating = false;
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        let mut lines = self.inner.selector.render(width);
        let t = theme();
        let opt = &self.inner.radius_option;
        let selected_line_text = format!(
            "{}{}",
            t.fg(ThemeColor::Accent, "→ "),
            t.fg(ThemeColor::Accent, &opt.label)
        );
        let selected_line = Text::new(Some(&selected_line_text), Some(1), Some(0), None)
            .render(width)
            .into_iter()
            .next();
        let found_index = selected_line.and_then(|sl| lines.iter().position(|line| line == &sl));
        let is_animating = found_index.is_some();
        self.inner.state.lock().unwrap().animating = is_animating;
        if let Some(index) = found_index {
            let elapsed = pi_js::time::performance_now() - self.inner.animation_start_ms;
            let shimmer = radius_shimmer(&opt.text, elapsed);
            let suffix = opt.label.strip_prefix(&opt.text).unwrap_or("");
            let animated_line = format!("{}{}{}", t.fg(ThemeColor::Accent, "→ "), shimmer, suffix);
            lines[index] = Text::new(Some(&animated_line), Some(1), Some(0), None)
                .render(width)
                .into_iter()
                .next()
                .unwrap_or_default();
        }
        lines
    }

    pub fn handle_input(&self, key_data: &str) {
        self.inner.selector.handle_input(key_data);
    }

    pub fn dispose(&self) {
        Self::stop_animation_inner(&self.inner);
        self.inner.selector.dispose();
    }

    pub fn invalidate(&self) {
        self.inner.selector.invalidate();
    }
}

impl Component for RadiusLoginMenuComponent {
    fn render(&self, width: usize) -> Vec<String> {
        RadiusLoginMenuComponent::render(self, width)
    }

    fn handle_input(&self, data: &str) {
        RadiusLoginMenuComponent::handle_input(self, data)
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.selector.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        RadiusLoginMenuComponent::invalidate(self)
    }

    fn has_handle_input(&self) -> bool {
        true
    }

    fn is_container_handle_mouse(&self) -> bool {
        true
    }

    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        self.inner.selector.container_children()
    }
}

/// Top-level `/login` selector whose Radius option shimmers in the Radius logo colors while selected.
pub fn create_login_menu_selector(
    tui: Arc<dyn TUI + Send + Sync>,
    title: &str,
    options: Vec<String>,
    radius_option: RadiusOption,
    on_select: Arc<dyn Fn(&str) + Send + Sync>,
    on_cancel: Arc<dyn Fn() + Send + Sync>,
) -> RadiusLoginMenuComponent {
    RadiusLoginMenuComponent::new(tui, title, options, radius_option, on_select, on_cancel)
}
