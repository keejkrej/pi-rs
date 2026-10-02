//! Port of packages/tui/src/components/h-stack.ts

#![allow(dead_code, unused_imports, unused_variables)]

use std::sync::Arc;

use crate::components::stack::{Stack, StackChild, StackEntryOptions, StackOptions};
use crate::layout_node::StackLayoutType;
use crate::layout_node::{LayoutComponent, LayoutNode};
use crate::tui::{Component, TuiMouseEvent, TuiMouseEventResult};

// PORT: `class HStack extends Stack` is composition. `layoutType` is `"hstack"`.

type DynComponent = Arc<dyn Component>;

struct HStackInner {
    stack: Stack,
}

#[derive(Clone)]
pub struct HStack {
    inner: Arc<HStackInner>,
}

impl PartialEq for HStack {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for HStack {}

impl HStack {
    /// `children` `None` is `[]`. `options` `None` is `{}`. Layout type is [`StackLayoutType::HStack`].
    pub fn new(children: Option<Vec<StackChild>>, options: Option<StackOptions>) -> Self {
        todo!("port: HStack::new")
    }

    pub fn children(&self) -> Vec<DynComponent> {
        self.inner.stack.children()
    }

    pub fn add_child(&self, component: DynComponent, options: Option<StackEntryOptions>) {
        self.inner.stack.add_child(component, options);
    }

    pub fn remove_child(&self, component: &DynComponent) {
        self.inner.stack.remove_child(component);
    }

    pub fn clear(&self) {
        self.inner.stack.clear();
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        todo!("port: HStack::render")
    }

    pub fn layout_node(&self) -> LayoutNode {
        self.inner.stack.layout_node()
    }
}

impl Component for HStack {
    fn render(&self, width: usize) -> Vec<String> {
        HStack::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.inner.stack.handle_mouse(event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        self.inner.stack.invalidate();
    }

    fn is_container_handle_mouse(&self) -> bool {
        true
    }

    fn container_children(&self) -> Option<Vec<DynComponent>> {
        Some(self.children())
    }

    fn as_layout_component(&self) -> Option<&dyn LayoutComponent> {
        Some(self)
    }
}

impl LayoutComponent for HStack {
    fn layout_node(&self) -> LayoutNode {
        HStack::layout_node(self)
    }
}
