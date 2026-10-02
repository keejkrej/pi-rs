//! Port of packages/tui/src/components/spacer.ts

#![allow(dead_code, unused_imports, unused_variables)]

use std::sync::{Arc, Mutex};

use crate::tui::{Component, TuiMouseEvent, TuiMouseEventResult};

/// Spacer component that renders empty lines.
#[derive(Clone)]
pub struct Spacer {
    inner: Arc<SpacerInner>,
}

struct SpacerInner {
    lines: Mutex<i64>,
}

impl PartialEq for Spacer {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for Spacer {}

impl Spacer {
    /// `lines` `None` is `1`.
    pub fn new(lines: Option<i64>) -> Self {
        todo!("port: Spacer::new")
    }

    pub fn set_lines(&self, lines: i64) {
        *self.inner.lines.lock().unwrap() = lines;
    }

    /// No cached state to invalidate currently.
    pub fn invalidate(&self) {}

    pub fn render(&self, width: usize) -> Vec<String> {
        todo!("port: Spacer::render")
    }
}

impl Component for Spacer {
    fn render(&self, width: usize) -> Vec<String> {
        Spacer::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        None
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        Spacer::invalidate(self)
    }
}
