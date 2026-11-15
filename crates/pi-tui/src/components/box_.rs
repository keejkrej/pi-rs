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
        Self {
            inner: Arc::new(TuiBoxInner {
                state: Mutex::new(TuiBoxState {
                    children: Vec::new(),
                    padding_x: padding_x.unwrap_or(1),
                    padding_y: padding_y.unwrap_or(1),
                    bg_fn,
                    cache: None,
                    mouse_layout: None,
                }),
            }),
        }
    }

    pub fn children(&self) -> Vec<DynComponent> {
        self.state().children.clone()
    }

    pub fn add_child(&self, component: DynComponent) {
        let mut state = self.state();
        state.children.push(component);
        state.cache = None;
    }

    pub fn remove_child(&self, component: &DynComponent) {
        let mut state = self.state();
        if let Some(index) = state.children.iter().position(|c| Arc::ptr_eq(c, component)) {
            state.children.remove(index);
            state.cache = None;
        }
    }

    pub fn clear(&self) {
        let mut state = self.state();
        state.children.clear();
        state.cache = None;
    }

    /// Don't invalidate here - we'll detect bgFn changes by sampling output.
    pub fn set_bg_fn(&self, bg_fn: Option<BgFn>) {
        self.state().bg_fn = bg_fn;
    }

    pub fn set_padding_x(&self, padding_x: i64) {
        let mut state = self.state();
        state.padding_x = padding_x;
        state.cache = None;
    }

    fn invalidate_cache(&self) {
        self.state().cache = None;
    }

    fn match_cache(&self, width: usize, child_lines: &[String], bg_sample: Option<&str>) -> bool {
        let state = self.state();
        Self::match_cache_state(state.cache.as_ref(), width, child_lines, bg_sample)
    }

    fn match_cache_state(
        cache: Option<&RenderCache>,
        width: usize,
        child_lines: &[String],
        bg_sample: Option<&str>,
    ) -> bool {
        match cache {
            Some(cache) => {
                cache.width == width
                    && cache.bg_sample.as_deref() == bg_sample
                    && cache.child_lines.len() == child_lines.len()
                    && cache.child_lines.iter().zip(child_lines.iter()).all(|(a, b)| a == b)
            }
            None => false,
        }
    }

    pub fn invalidate(&self) {
        let children = {
            let mut state = self.state();
            state.cache = None;
            state.children.clone()
        };
        for child in children {
            child.invalidate();
        }
    }

    pub fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        let (content_width, content_x, content_y, cached_children, fallback_children) = {
            let state = self.state();
            let content_width = (event.width - state.padding_x * 2).max(1);
            let content_y = event.y - state.padding_y;
            let content_x = event.x - state.padding_x;
            if content_y < 0 || content_x < 0 || content_x >= content_width {
                return None;
            }
            let cached = state.mouse_layout.as_ref().and_then(|ml| {
                if ml.width == content_width {
                    Some(
                        ml.children
                            .iter()
                            .map(|c| (c.component.clone(), c.height))
                            .collect::<Vec<_>>(),
                    )
                } else {
                    None
                }
            });
            let fallback = if cached.is_none() {
                Some(state.children.clone())
            } else {
                None
            };
            (content_width, content_x, content_y, cached, fallback)
        };

        let mouse_children: Vec<(DynComponent, i64)> = match cached_children {
            Some(children) => children,
            None => fallback_children
                .unwrap_or_default()
                .into_iter()
                .map(|component| {
                    let h = component.render(content_width.max(0) as usize).len() as i64;
                    (component, h)
                })
                .collect(),
        };

        let mut child_y = 0i64;
        for (child, child_height) in mouse_children {
            if content_y >= child_y && content_y < child_y + child_height {
                let mut forwarded = *event;
                forwarded.x = content_x;
                forwarded.y = content_y - child_y;
                forwarded.width = content_width;
                forwarded.height = child_height;
                return crate::tui::dispatch_mouse_event(&*child, &forwarded).map(|res| TuiMouseEventResult {
                    handled: Some(res.handled),
                    capture: res.capture,
                    focus: res.focus,
                    render: res.render,
                    target: Some(res.target),
                    focus_target: res.focus_target,
                });
            }
            child_y += child_height;
        }
        None
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        let (children, padding_x, padding_y, bg_fn) = {
            let state = self.state();
            if state.children.is_empty() {
                return Vec::new();
            }
            (
                state.children.clone(),
                state.padding_x,
                state.padding_y,
                state.bg_fn.clone(),
            )
        };

        let content_width = (width as i64 - padding_x * 2).max(1);
        let left_pad = " ".repeat(padding_x.max(0) as usize);

        let mut child_lines = Vec::new();
        let mut mouse_children = Vec::with_capacity(children.len());
        for child in children {
            let lines = child.render(content_width as usize);
            mouse_children.push(MouseLayoutChild {
                component: child,
                height: lines.len() as i64,
            });
            child_lines.extend(lines);
        }

        {
            let mut state = self.state();
            state.mouse_layout = Some(MouseLayout {
                width: content_width,
                children: mouse_children,
            });
        }

        if child_lines.is_empty() {
            return Vec::new();
        }

        let bg_sample = bg_fn.as_ref().map(|f| f("test"));

        {
            let state = self.state();
            if Self::match_cache_state(state.cache.as_ref(), width, &child_lines, bg_sample.as_deref()) {
                return state.cache.as_ref().unwrap().lines.clone();
            }
        }

        let apply = |line: &str| -> String {
            let vis_len = crate::utils::visible_width(line);
            let pad_needed = (width as i64 - vis_len).max(0) as usize;
            let padded = format!("{line}{}", " ".repeat(pad_needed));
            match &bg_fn {
                Some(f) => f(&padded),
                None => padded,
            }
        };

        let padding_y_count = padding_y.max(0) as usize;
        let mut result = Vec::with_capacity(padding_y_count * 2 + child_lines.len());
        for _ in 0..padding_y_count {
            result.push(apply(""));
        }
        for line in &child_lines {
            result.push(apply(&format!("{left_pad}{line}")));
        }
        for _ in 0..padding_y_count {
            result.push(apply(""));
        }

        crate::utils::flatten_lines(&result);
        {
            let mut state = self.state();
            state.cache = Some(RenderCache {
                child_lines,
                width,
                bg_sample,
                lines: result.clone(),
            });
        }

        result
    }

    fn apply_bg(&self, line: &str, width: usize) -> String {
        let bg_fn = self.state().bg_fn.clone();
        let vis_len = crate::utils::visible_width(line);
        let pad_needed = (width as i64 - vis_len).max(0) as usize;
        let padded = format!("{line}{}", " ".repeat(pad_needed));
        match bg_fn {
            Some(f) => f(&padded),
            None => padded,
        }
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
