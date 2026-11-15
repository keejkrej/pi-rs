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
    match value {
        Some(v) if v.is_finite() => {
            let floored = v.floor().max(0.0);
            if floored >= i64::MAX as f64 {
                i64::MAX
            } else {
                floored as i64
            }
        }
        _ => fallback,
    }
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
        let opts = options.unwrap_or_default();
        let stack = Self {
            inner: Arc::new(StackInner {
                container: Container::new(),
                entries: Mutex::new(Vec::new()),
                gap: normalize_size(opts.gap.map(|v| v as f64), 0),
                align: opts.align.unwrap_or(StackAlign::Stretch),
                layout_type,
            }),
        };
        if let Some(children) = children {
            for child in children {
                match child {
                    StackChild::Entry(entry) => {
                        stack.add_child(
                            entry.component,
                            Some(StackEntryOptions {
                                basis: entry.basis,
                                grow: entry.grow,
                                shrink: entry.shrink,
                                min_size: entry.min_size,
                                max_size: entry.max_size,
                                visible: entry.visible,
                            }),
                        );
                    }
                    StackChild::Component(component) => {
                        stack.add_child(component, None);
                    }
                }
            }
        }
        stack
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
        const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
        let opts = options.unwrap_or_default();
        self.inner.container.add_child(component.clone());
        self.state_entries().push(StackLayoutEntry {
            component,
            basis: opts.basis,
            grow: opts.grow.map(|v| normalize_size(Some(v as f64), 0)),
            shrink: opts.shrink.map(|v| normalize_size(Some(v as f64), 1)),
            min_size: opts.min_size.map(|v| normalize_size(Some(v as f64), 0)),
            max_size: opts.max_size.map(|v| normalize_size(Some(v as f64), MAX_SAFE_INTEGER)),
            visible: opts.visible,
        });
    }

    pub fn remove_child(&self, component: &DynComponent) {
        self.inner.container.remove_child(component);
        let mut entries = self.state_entries();
        if let Some(index) = entries.iter().position(|e| Arc::ptr_eq(&e.component, component)) {
            entries.remove(index);
        }
    }

    pub fn clear(&self) {
        self.inner.container.clear();
        self.state_entries().clear();
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
        use crate::layout_node::StackLayoutNode;
        LayoutNode::Stack(StackLayoutNode {
            r#type: self.inner.layout_type,
            entries: self.entries(),
            gap: self.inner.gap,
            align: self.inner.align,
        })
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

const STACK_MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

fn clamp_size(size: i64, entry: &StackLayoutEntry) -> i64 {
    let min = entry.min_size.unwrap_or(0).max(0);
    let max = entry.max_size.unwrap_or(STACK_MAX_SAFE_INTEGER).max(min);
    size.max(0).min(max).max(min)
}

fn distribute(sizes: &mut [i64], entries: &[StackLayoutEntry], amount: i64, mode: DistributeMode) {
    let mut remaining = amount;
    while remaining > 0 {
        let mut candidates: Vec<(usize, &StackLayoutEntry)> = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            let eligible = match mode {
                DistributeMode::Grow => {
                    entry.grow.unwrap_or(0) > 0 && sizes[index] < entry.max_size.unwrap_or(STACK_MAX_SAFE_INTEGER)
                }
                DistributeMode::Shrink => entry.shrink.unwrap_or(1) > 0 && sizes[index] > entry.min_size.unwrap_or(0),
            };
            if eligible {
                candidates.push((index, entry));
            }
        }
        if candidates.is_empty() {
            return;
        }

        let total_weight: i128 = candidates
            .iter()
            .map(|(index, entry)| match mode {
                DistributeMode::Grow => i128::from(entry.grow.unwrap_or(0)),
                DistributeMode::Shrink => i128::from(entry.shrink.unwrap_or(1)) * i128::from(sizes[*index].max(1)),
            })
            .sum();
        if total_weight <= 0 {
            return;
        }

        let mut distributed = 0i64;
        for (index, entry) in candidates {
            if remaining <= 0 {
                break;
            }
            let weight: i128 = match mode {
                DistributeMode::Grow => i128::from(entry.grow.unwrap_or(0)),
                DistributeMode::Shrink => i128::from(entry.shrink.unwrap_or(1)) * i128::from(sizes[index].max(1)),
            };
            let proposed = ((i128::from(remaining) * weight) / total_weight).max(1) as i64;
            let capacity = match mode {
                DistributeMode::Grow => entry.max_size.unwrap_or(STACK_MAX_SAFE_INTEGER) - sizes[index],
                DistributeMode::Shrink => sizes[index] - entry.min_size.unwrap_or(0),
            };
            let delta = remaining.min(proposed).min(capacity);
            if delta <= 0 {
                continue;
            }
            match mode {
                DistributeMode::Grow => sizes[index] += delta,
                DistributeMode::Shrink => sizes[index] -= delta,
            }
            remaining -= delta;
            distributed += delta;
        }
        if distributed == 0 {
            return;
        }
    }
}

pub fn visible_stack_entries(entries: &[StackLayoutEntry], viewport: &LayoutViewport) -> Vec<StackLayoutEntry> {
    entries
        .iter()
        .filter(|entry| entry.visible.as_ref().is_none_or(|f| f(viewport)))
        .cloned()
        .collect()
}

pub fn allocate_stack_sizes(
    entries: &[StackLayoutEntry],
    intrinsic_sizes: &[i64],
    available_size: Option<i64>,
    gap: i64,
) -> Vec<i64> {
    let mut sizes: Vec<i64> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let base = match &entry.basis {
                None | Some(StackBasis::Auto) => intrinsic_sizes.get(index).copied().unwrap_or(0),
                Some(StackBasis::Fixed(n)) => {
                    if n.is_nan() {
                        0
                    } else if *n == f64::INFINITY {
                        STACK_MAX_SAFE_INTEGER
                    } else if *n == f64::NEG_INFINITY {
                        0
                    } else {
                        let f = n.floor();
                        if f >= STACK_MAX_SAFE_INTEGER as f64 {
                            STACK_MAX_SAFE_INTEGER
                        } else if f <= 0.0 {
                            0
                        } else {
                            f as i64
                        }
                    }
                }
            };
            clamp_size(base, entry)
        })
        .collect();

    let Some(available_size) = available_size else {
        return sizes;
    };

    let content_size = (available_size - (entries.len() as i64 - 1).max(0) * gap).max(0);
    let total: i64 = sizes.iter().sum();
    if total < content_size {
        distribute(&mut sizes, entries, content_size - total, DistributeMode::Grow);
    } else if total > content_size {
        distribute(&mut sizes, entries, total - content_size, DistributeMode::Shrink);
    }
    sizes
}
