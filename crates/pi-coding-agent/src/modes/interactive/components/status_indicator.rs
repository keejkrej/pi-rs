//! Port of packages/coding-agent/src/modes/interactive/components/status-indicator.ts

use std::sync::{Arc, Mutex};

use pi_tui::{Component, Loader, LoaderIndicatorOptions, TUI, TuiMouseEvent, TuiMouseEventResult, truncate_to_width};

use super::super::theme::theme::{ThemeColor, theme};
use super::countdown_timer::CountdownTimer;
use super::keybinding_hints::key_text;

pub type WorkingIndicatorOptions = LoaderIndicatorOptions;
pub type ColorFn = Arc<dyn Fn(&str) -> String + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusIndicatorKind {
    Working,
    Retry,
    Compaction,
    BranchSummary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactionStatusReason {
    Manual,
    Threshold,
    Overflow,
}

struct StatusIndicatorInner {
    kind: StatusIndicatorKind,
    loader: Loader,
    countdown: Mutex<Option<CountdownTimer>>,
}

impl Drop for StatusIndicatorInner {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.countdown.lock()
            && let Some(cd) = guard.take()
        {
            cd.dispose();
        }
        self.loader.stop();
    }
}

#[derive(Clone)]
pub struct StatusIndicator {
    inner: Arc<StatusIndicatorInner>,
}

impl StatusIndicator {
    pub fn new(
        kind: StatusIndicatorKind,
        ui: Arc<dyn TUI + Send + Sync>,
        spinner_color_fn: ColorFn,
        message_color_fn: ColorFn,
        message: &str,
        indicator: Option<WorkingIndicatorOptions>,
    ) -> Self {
        let loader = Loader::new(ui, spinner_color_fn, message_color_fn, Some(message), indicator);
        Self {
            inner: Arc::new(StatusIndicatorInner {
                kind,
                loader,
                countdown: Mutex::new(None),
            }),
        }
    }

    pub fn working(
        ui: Arc<dyn TUI + Send + Sync>,
        message: &str,
        indicator: Option<WorkingIndicatorOptions>,
        color_fn: Option<ColorFn>,
    ) -> Self {
        let spin = color_fn
            .clone()
            .unwrap_or_else(|| Arc::new(|s: &str| theme().fg(ThemeColor::Accent, s)));
        let msg = color_fn.unwrap_or_else(|| Arc::new(|s: &str| theme().fg(ThemeColor::Muted, s)));
        Self::new(StatusIndicatorKind::Working, ui, spin, msg, message, indicator)
    }

    pub fn retry(ui: Arc<dyn TUI + Send + Sync>, attempt: i64, max_attempts: i64, delay_ms: i64) -> Self {
        let format_retry_message = move |seconds: i64| {
            format!(
                "Retrying ({attempt}/{max_attempts}) in {seconds}s... ({} to cancel)",
                key_text(&"app.interrupt".to_string())
            )
        };
        let initial_secs = ((delay_ms as f64) / 1000.0).ceil() as i64;
        let status = Self::new(
            StatusIndicatorKind::Retry,
            Arc::clone(&ui),
            Arc::new(|s: &str| theme().fg(ThemeColor::Warning, s)),
            Arc::new(|s: &str| theme().fg(ThemeColor::Muted, s)),
            &format_retry_message(initial_secs),
            None,
        );

        let weak = Arc::downgrade(&status.inner);
        let fmt_c = format_retry_message;
        let cd = CountdownTimer::new(
            delay_ms,
            Some(ui),
            Arc::new(move |seconds| {
                if let Some(inner) = weak.upgrade() {
                    inner.loader.set_message(&fmt_c(seconds));
                }
            }),
            Arc::new(|| {}),
        );
        *status.inner.countdown.lock().unwrap() = Some(cd);
        status
    }

    pub fn compaction(
        ui: Arc<dyn TUI + Send + Sync>,
        reason: CompactionStatusReason,
        color_fn: Option<ColorFn>,
    ) -> Self {
        let cancel_hint = format!("({} to cancel)", key_text(&"app.interrupt".to_string()));
        let label = match reason {
            CompactionStatusReason::Manual => format!("Compacting context... {cancel_hint}"),
            CompactionStatusReason::Threshold => format!("Auto-compacting... {cancel_hint}"),
            CompactionStatusReason::Overflow => format!("Context overflow detected, Auto-compacting... {cancel_hint}"),
        };
        let spin = color_fn
            .clone()
            .unwrap_or_else(|| Arc::new(|s: &str| theme().fg(ThemeColor::Accent, s)));
        let msg = color_fn.unwrap_or_else(|| Arc::new(|s: &str| theme().fg(ThemeColor::Muted, s)));
        Self::new(StatusIndicatorKind::Compaction, ui, spin, msg, &label, None)
    }

    pub fn branch_summary(ui: Arc<dyn TUI + Send + Sync>) -> Self {
        let label = format!(
            "Summarizing branch... ({} to cancel)",
            key_text(&"app.interrupt".to_string())
        );
        Self::new(
            StatusIndicatorKind::BranchSummary,
            ui,
            Arc::new(|s: &str| theme().fg(ThemeColor::Accent, s)),
            Arc::new(|s: &str| theme().fg(ThemeColor::Muted, s)),
            &label,
            None,
        )
    }

    pub fn kind(&self) -> StatusIndicatorKind {
        self.inner.kind
    }

    pub fn set_message(&self, message: &str) {
        self.inner.loader.set_message(message);
    }

    pub fn start(&self) {
        self.inner.loader.start();
    }

    pub fn stop(&self) {
        self.inner.loader.stop();
    }

    pub fn render_in_border(&self, width: usize) -> String {
        let lines = self.inner.loader.render(width + 2);
        let line = lines.get(1).map(String::as_str).unwrap_or("");
        let stripped = line.strip_prefix(' ').unwrap_or(line).trim_end();
        truncate_to_width(stripped, width as i64, Some(""), None)
    }

    pub fn render_spinner_in_border(&self, width: usize) -> String {
        let rendered = self.inner.loader.get_rendered_indicator();
        truncate_to_width(&rendered, width as i64, Some(""), None)
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        self.inner.loader.render(width)
    }

    pub fn invalidate(&self) {
        self.inner.loader.invalidate();
    }

    pub fn dispose(&self) {
        if let Some(cd) = self.inner.countdown.lock().unwrap().take() {
            cd.dispose();
        }
        self.inner.loader.stop();
    }
}

impl Component for StatusIndicator {
    fn render(&self, width: usize) -> Vec<String> {
        StatusIndicator::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        None
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        StatusIndicator::invalidate(self)
    }
}

#[derive(Clone, Default)]
pub struct IdleStatus;

impl IdleStatus {
    pub fn new() -> Self {
        Self
    }

    pub fn invalidate(&self) {}

    pub fn render(&self, width: usize) -> Vec<String> {
        let empty = " ".repeat(width);
        vec![empty.clone(), empty]
    }
}

impl Component for IdleStatus {
    fn render(&self, width: usize) -> Vec<String> {
        IdleStatus::render(self, width)
    }

    fn invalidate(&self) {
        IdleStatus::invalidate(self)
    }
}
