//! Port of packages/tui/src/components/v-stack.ts

#![allow(dead_code, unused_imports, unused_variables)]

use std::sync::Arc;

pub use crate::components::stack::{StackChild, StackEntry, StackEntryOptions, StackOptions};

use crate::components::stack::Stack;
use crate::layout_node::StackLayoutType;
use crate::layout_node::{LayoutComponent, LayoutNode};
use crate::tui::{Component, TuiMouseEvent, TuiMouseEventResult};

// PORT: `class VStack extends Stack` is composition. `layoutType` is `"vstack"`.

type DynComponent = Arc<dyn Component>;

struct VStackInner {
    stack: Stack,
}

#[derive(Clone)]
pub struct VStack {
    inner: Arc<VStackInner>,
}

impl PartialEq for VStack {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for VStack {}

impl VStack {
    /// `children` `None` is `[]`. `options` `None` is `{}`. Layout type is [`StackLayoutType::VStack`].
    pub fn new(children: Option<Vec<StackChild>>, options: Option<StackOptions>) -> Self {
        todo!("port: VStack::new")
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
        todo!("port: VStack::render")
    }

    pub fn layout_node(&self) -> LayoutNode {
        self.inner.stack.layout_node()
    }
}

impl Component for VStack {
    fn render(&self, width: usize) -> Vec<String> {
        VStack::render(self, width)
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

impl LayoutComponent for VStack {
    fn layout_node(&self) -> LayoutNode {
        VStack::layout_node(self)
    }
}
