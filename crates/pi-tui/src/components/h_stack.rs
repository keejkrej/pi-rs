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
        Self {
            inner: Arc::new(HStackInner {
                stack: Stack::new(StackLayoutType::HStack, children, options),
            }),
        }
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
        use crate::components::stack::{allocate_stack_sizes, visible_stack_entries};
        use crate::layout_node::{LayoutViewport, StackAlign};
        use crate::tui::composite_tui_line;
        use crate::utils::visible_width;

        const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
        let safe_width = (width as i64).max(1);
        let viewport = LayoutViewport {
            width: safe_width,
            height: MAX_SAFE_INTEGER,
        };
        let entries = visible_stack_entries(&self.inner.stack.entries(), &viewport);
        if entries.is_empty() {
            return Vec::new();
        }

        let intrinsic_widths: Vec<i64> = entries
            .iter()
            .map(|entry| {
                entry
                    .component
                    .render(safe_width as usize)
                    .iter()
                    .map(|line| visible_width(line))
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        let gap = self.inner.stack.gap();
        let widths = allocate_stack_sizes(&entries, &intrinsic_widths, Some(safe_width), gap);
        let rendered: Vec<Vec<String>> = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                if widths[index] == 0 {
                    Vec::new()
                } else {
                    entry.component.render(widths[index].max(0) as usize)
                }
            })
            .collect();

        let height = rendered.iter().map(|lines| lines.len()).max().unwrap_or(0);
        let mut result = vec![String::new(); height];
        let align = self.inner.stack.align();
        let mut x = 0i64;
        for (index, lines) in rendered.into_iter().enumerate() {
            let child_width = widths[index];
            let offset: isize = match align {
                StackAlign::Center => (height as isize - lines.len() as isize).div_euclid(2),
                StackAlign::End => height as isize - lines.len() as isize,
                StackAlign::Stretch | StackAlign::Start => 0,
            };
            for (row, line) in lines.into_iter().enumerate() {
                let target = row as isize + offset;
                if target < 0 || target >= result.len() as isize {
                    continue;
                }
                let target = target as usize;
                result[target] = composite_tui_line(&result[target], &line, x, child_width, safe_width);
            }
            x += child_width + gap;
        }
        result
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
