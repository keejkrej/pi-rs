//! Port of packages/tui/src/components/box.ts

#![allow(dead_code, unused_imports, unused_variables)]

use std::sync::{Arc, Mutex, MutexGuard};

use crate::tui::{Component, TuiMouseEvent, TuiMouseEventResult};

// PORT: TS class `Box` is [`TuiBox`]. `Box` is a Rust prelude type.

type DynComponent = Arc<dyn Component>;
type BgFn = Arc<dyn Fn(&str) -> String + Send + Sync>;

struct RenderCache {
    child_lines: Vec<String>,
    width: usize,
    bg_sample: Option<String>,
    lines: Vec<String>,
}

struct MouseLayoutChild {
    component: DynComponent,
    height: i64,
}

struct MouseLayout {
    width: i64,
    children: Vec<MouseLayoutChild>,
}

struct TuiBoxState {
    children: Vec<DynComponent>,
    padding_x: i64,
    padding_y: i64,
    bg_fn: Option<BgFn>,
    cache: Option<RenderCache>,
    mouse_layout: Option<MouseLayout>,
}

struct TuiBoxInner {
    state: Mutex<TuiBoxState>,
}

/// Box component - a container that applies padding and background to all children.
///
/// PORT: not a `Container` subclass in TS. Own child list.
#[derive(Clone)]
pub struct TuiBox {
    inner: Arc<TuiBoxInner>,
}

impl PartialEq for TuiBox {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for TuiBox {}

impl TuiBox {
    fn state(&self) -> MutexGuard<'_, TuiBoxState> {
        self.inner.state.lock().unwrap()
    }

    /// `padding_x` and `padding_y` `None` are `1`.
    pub fn new(padding_x: Option<i64>, padding_y: Option<i64>, bg_fn: Option<BgFn>) -> Self {
        todo!("port: TuiBox::new")
    }

    pub fn children(&self) -> Vec<DynComponent> {
        self.state().children.clone()
    }

    pub fn add_child(&self, component: DynComponent) {
        todo!("port: TuiBox::add_child")
    }

    pub fn remove_child(&self, component: &DynComponent) {
        todo!("port: TuiBox::remove_child")
    }

    pub fn clear(&self) {
        todo!("port: TuiBox::clear")
    }

    /// Don't invalidate here - we'll detect bgFn changes by sampling output.
    pub fn set_bg_fn(&self, bg_fn: Option<BgFn>) {
        self.state().bg_fn = bg_fn;
    }

    fn invalidate_cache(&self) {
        todo!("port: TuiBox::invalidate_cache")
    }

    fn match_cache(&self, width: usize, child_lines: &[String], bg_sample: Option<&str>) -> bool {
        todo!("port: TuiBox::match_cache")
    }

    pub fn invalidate(&self) {
        todo!("port: TuiBox::invalidate")
    }

    pub fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        todo!("port: TuiBox::handle_mouse")
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        todo!("port: TuiBox::render")
    }

    fn apply_bg(&self, line: &str, width: usize) -> String {
        todo!("port: TuiBox::apply_bg")
    }
}

impl Component for TuiBox {
    fn render(&self, width: usize) -> Vec<String> {
        TuiBox::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        TuiBox::handle_mouse(self, event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        TuiBox::invalidate(self)
    }
}
