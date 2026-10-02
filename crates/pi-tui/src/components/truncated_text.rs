//! Port of packages/tui/src/components/truncated-text.ts

#![allow(dead_code, unused_imports, unused_variables)]

use std::sync::{Arc, Mutex, MutexGuard};

use crate::tui::{Component, TuiMouseEvent, TuiMouseEventResult};

struct TruncatedTextState {
    text: String,
    padding_x: i64,
    padding_y: i64,
}

struct TruncatedTextInner {
    state: Mutex<TruncatedTextState>,
}

/// Text component that truncates to fit viewport width.
#[derive(Clone)]
pub struct TruncatedText {
    inner: Arc<TruncatedTextInner>,
}

impl PartialEq for TruncatedText {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for TruncatedText {}

impl TruncatedText {
    fn state(&self) -> MutexGuard<'_, TruncatedTextState> {
        self.inner.state.lock().unwrap()
    }

    /// `padding_x` and `padding_y` `None` are `0`.
    pub fn new(text: &str, padding_x: Option<i64>, padding_y: Option<i64>) -> Self {
        todo!("port: TruncatedText::new")
    }

    /// No cached state to invalidate currently.
    pub fn invalidate(&self) {}

    pub fn render(&self, width: usize) -> Vec<String> {
        todo!("port: TruncatedText::render")
    }
}

impl Component for TruncatedText {
    fn render(&self, width: usize) -> Vec<String> {
        TruncatedText::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        None
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        TruncatedText::invalidate(self)
    }
}
