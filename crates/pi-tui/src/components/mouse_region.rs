//! Port of packages/tui/src/components/mouse-region.ts

#![allow(dead_code, unused_imports, unused_variables)]

use std::sync::Arc;

use crate::tui::{Component, TuiMouseEvent, TuiMouseEventResult};

type DynComponent = Arc<dyn Component>;

pub type MouseRegionHandler = Arc<dyn Fn(&TuiMouseEvent) -> Option<TuiMouseEventResult> + Send + Sync>;

/// Adds mouse handling to an existing component without changing its rendering.
#[derive(Clone)]
pub struct MouseRegion {
    inner: Arc<MouseRegionInner>,
}

struct MouseRegionInner {
    child: DynComponent,
    on_mouse: MouseRegionHandler,
}

impl PartialEq for MouseRegion {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for MouseRegion {}

impl MouseRegion {
    pub fn new(child: DynComponent, on_mouse: MouseRegionHandler) -> Self {
        todo!("port: MouseRegion::new")
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        self.inner.child.render(width)
    }

    pub fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        todo!("port: MouseRegion::handle_mouse")
    }

    pub fn invalidate(&self) {
        self.inner.child.invalidate();
    }
}

impl Component for MouseRegion {
    fn render(&self, width: usize) -> Vec<String> {
        MouseRegion::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        MouseRegion::handle_mouse(self, event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        MouseRegion::invalidate(self)
    }
}
