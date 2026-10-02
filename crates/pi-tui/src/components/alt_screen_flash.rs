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
        todo!("port: AltScreenFlashContainer::flash")
    }

    pub fn dispose(&self) {
        todo!("port: AltScreenFlashContainer::dispose")
    }

    pub fn invalidate(&self) {}

    pub fn render(&self, width: usize) -> Vec<String> {
        todo!("port: AltScreenFlashContainer::render")
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
