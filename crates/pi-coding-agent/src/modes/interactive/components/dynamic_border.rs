//! Port of packages/coding-agent/src/modes/interactive/components/dynamic-border.ts

use std::sync::Arc;

use pi_tui::{Component, TuiMouseEvent, TuiMouseEventResult};

use super::super::theme::theme::{ThemeColor, theme};

pub type BorderColorFn = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// Dynamic border component that adjusts to viewport width.
#[derive(Clone)]
pub struct DynamicBorder {
    color: BorderColorFn,
}

impl Default for DynamicBorder {
    fn default() -> Self {
        Self::new(None)
    }
}

impl DynamicBorder {
    pub fn new(color: Option<BorderColorFn>) -> Self {
        Self {
            color: color.unwrap_or_else(|| Arc::new(|s: &str| theme().fg(ThemeColor::Border, s))),
        }
    }

    pub fn invalidate(&self) {}

    pub fn render(&self, width: usize) -> Vec<String> {
        vec![(self.color)(&"─".repeat(width.max(1)))]
    }
}

impl Component for DynamicBorder {
    fn render(&self, width: usize) -> Vec<String> {
        DynamicBorder::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        None
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        DynamicBorder::invalidate(self)
    }
}
