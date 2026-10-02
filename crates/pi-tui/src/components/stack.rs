//! Port of packages/tui/src/components/stack.ts

#![allow(dead_code, unused_imports, unused_variables)]

use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

use crate::layout_node::{
    LayoutComponent, LayoutNode, LayoutViewport, StackAlign, StackBasis, StackLayoutEntry, StackLayoutType,
};
use crate::tui::{Component, Container, TuiMouseEvent, TuiMouseEventResult};

// PORT: TS `abstract class Stack extends Container`. Composition plus delegation (§9.4).
// `new` takes `layout_type` because that TS field is abstract.
// `StackAlign`, `StackBasis`, and `StackLayoutType` live in `layout_node` (same unions).

type DynComponent = Arc<dyn Component>;

/// `(viewport: LayoutViewport) => boolean`
pub type StackVisibleFn = Arc<dyn Fn(&LayoutViewport) -> bool + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum DistributeMode {
    #[serde(rename = "grow")]
    Grow,
    #[serde(rename = "shrink")]
    Shrink,
}

/// Field order is the interface: `basis`, `grow`, `shrink`, `minSize`, `maxSize`, `visible`.
#[derive(Clone, Default)]
pub struct StackEntryOptions {
    pub basis: Option<StackBasis>,
    pub grow: Option<i64>,
    pub shrink: Option<i64>,
    pub min_size: Option<i64>,
    pub max_size: Option<i64>,
    pub visible: Option<StackVisibleFn>,
}

#[derive(Clone)]
pub struct StackEntry {
    pub component: DynComponent,
    pub basis: Option<StackBasis>,
    pub grow: Option<i64>,
    pub shrink: Option<i64>,
    pub min_size: Option<i64>,
    pub max_size: Option<i64>,
    pub visible: Option<StackVisibleFn>,
}

/// `Component | StackEntry`. Checked as entry when `"render"` is absent.
#[derive(Clone)]
pub enum StackChild {
    Entry(StackEntry),
    Component(DynComponent),
}

/// Field order is the interface: `gap`, `align`.
#[derive(Clone, Default)]
pub struct StackOptions {
    pub gap: Option<i64>,
    pub align: Option<StackAlign>,
}

fn is_stack_entry(child: &StackChild) -> bool {
    matches!(child, StackChild::Entry(_))
}

fn normalize_size(value: Option<f64>, fallback: i64) -> i64 {
    todo!("port: normalize_size")
}

struct StackInner {
    container: Container,
    entries: Mutex<Vec<StackLayoutEntry>>,
    gap: i64,
    align: StackAlign,
    layout_type: StackLayoutType,
}

/// Abstract stack. Constructed only by [`crate::components::v_stack::VStack`] and
/// [`crate::components::h_stack::HStack`].
#[derive(Clone)]
pub struct Stack {
    inner: Arc<StackInner>,
}

impl PartialEq for Stack {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for Stack {}

impl Stack {
    fn state_entries(&self) -> MutexGuard<'_, Vec<StackLayoutEntry>> {
        self.inner.entries.lock().unwrap()
    }

    /// `layout_type` selects `"vstack"` or `"hstack"`. `children` `None` is `[]`. `options` `None` is `{}`.
    pub(crate) fn new(
        layout_type: StackLayoutType,
        children: Option<Vec<StackChild>>,
        options: Option<StackOptions>,
    ) -> Self {
        todo!("port: Stack::new")
    }

    pub(crate) fn gap(&self) -> i64 {
        self.inner.gap
    }

    pub(crate) fn align(&self) -> StackAlign {
        self.inner.align
    }

    pub(crate) fn layout_type(&self) -> StackLayoutType {
        self.inner.layout_type
    }

    pub(crate) fn entries(&self) -> Vec<StackLayoutEntry> {
        self.state_entries().clone()
    }

    pub fn children(&self) -> Vec<DynComponent> {
        self.inner.container.children()
    }

    pub fn add_child(&self, component: DynComponent, options: Option<StackEntryOptions>) {
        todo!("port: Stack::add_child")
    }

    pub fn remove_child(&self, component: &DynComponent) {
        todo!("port: Stack::remove_child")
    }

    pub fn clear(&self) {
        todo!("port: Stack::clear")
    }

    pub fn invalidate(&self) {
        Component::invalidate(&self.inner.container);
    }

    pub fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        Component::handle_mouse(&self.inner.container, event)
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        Component::render(&self.inner.container, width)
    }

    pub fn layout_node(&self) -> LayoutNode {
        todo!("port: Stack::layout_node")
    }
}

impl Component for Stack {
    fn render(&self, width: usize) -> Vec<String> {
        Stack::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        Stack::handle_mouse(self, event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        Stack::invalidate(self)
    }

    /// TS `handleMouse` is `Container.prototype.handleMouse`.
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

impl LayoutComponent for Stack {
    fn layout_node(&self) -> LayoutNode {
        Stack::layout_node(self)
    }
}

fn clamp_size(size: i64, entry: &StackLayoutEntry) -> i64 {
    todo!("port: clamp_size")
}

fn distribute(sizes: &mut [i64], entries: &[StackLayoutEntry], amount: i64, mode: DistributeMode) {
    todo!("port: distribute")
}

pub fn visible_stack_entries(entries: &[StackLayoutEntry], viewport: &LayoutViewport) -> Vec<StackLayoutEntry> {
    todo!("port: visible_stack_entries")
}

pub fn allocate_stack_sizes(
    entries: &[StackLayoutEntry],
    intrinsic_sizes: &[i64],
    available_size: Option<i64>,
    gap: i64,
) -> Vec<i64> {
    todo!("port: allocate_stack_sizes")
}
