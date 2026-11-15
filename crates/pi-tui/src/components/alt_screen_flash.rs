//! Port of packages/tui/src/components/alt-screen-flash.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

use pi_js::time::Timeout;

use crate::tui::{Component, TuiMouseEvent, TuiMouseEventResult};

const DEFAULT_DURATION_MS: i64 = 1000;

struct FlashEntry {
    id: i64,
    message: String,
    timer: Timeout,
}

struct AltScreenFlashState {
    entries: Vec<FlashEntry>,
    next_id: i64,
}

struct AltScreenFlashInner {
    state: Mutex<AltScreenFlashState>,
    request_render: Arc<dyn Fn() + Send + Sync>,
}

/// Transient messages composited by the alternate-screen renderer.
///
/// PORT: TS class with identity. Handle; methods take `&self`.
#[derive(Clone)]
pub struct AltScreenFlashContainer {
    inner: Arc<AltScreenFlashInner>,
}

impl AltScreenFlashContainer {
    pub fn new(request_render: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            inner: Arc::new(AltScreenFlashInner {
                state: Mutex::new(AltScreenFlashState {
                    entries: Vec::new(),
                    next_id: 0,
                }),
                request_render,
            }),
        }
    }

    /// `duration_ms` `None` is [`DEFAULT_DURATION_MS`] (1000).
    pub fn flash(&self, message: &str, duration_ms: Option<i64>) {
        let duration = duration_ms.unwrap_or(DEFAULT_DURATION_MS).max(0) as u64;
        {
            let mut state = self.inner.state.lock().unwrap();
            let id = state.next_id;
            state.next_id += 1;
            let inner = Arc::clone(&self.inner);
            let timer = pi_js::time::set_timeout(duration, move || {
                let removed = {
                    let mut state = inner.state.lock().unwrap();
                    if let Some(index) = state.entries.iter().position(|e| e.id == id) {
                        state.entries.remove(index);
                        true
                    } else {
                        false
                    }
                };
                if removed {
                    (inner.request_render)();
                }
            });
            timer.unref();
            state.entries.push(FlashEntry {
                id,
                message: message.to_string(),
                timer,
            });
        }
        (self.inner.request_render)();
    }

    pub fn dispose(&self) {
        let entries = {
            let mut state = self.inner.state.lock().unwrap();
            std::mem::take(&mut state.entries)
        };
        for entry in entries {
            entry.timer.clear();
        }
    }

    pub fn invalidate(&self) {}

    pub fn render(&self, width: usize) -> Vec<String> {
        let messages: Vec<String> = self
            .inner
            .state
            .lock()
            .unwrap()
            .entries
            .iter()
            .map(|e| e.message.clone())
            .collect();
        messages
            .into_iter()
            .map(|msg| {
                let message = crate::utils::truncate_to_width(&format!(" {msg} "), width as i64, Some(""), None);
                format!("\x1b[7m{message}\x1b[27m")
            })
            .collect()
    }
}

impl Component for AltScreenFlashContainer {
    fn render(&self, width: usize) -> Vec<String> {
        AltScreenFlashContainer::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        None
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        AltScreenFlashContainer::invalidate(self)
    }
}
