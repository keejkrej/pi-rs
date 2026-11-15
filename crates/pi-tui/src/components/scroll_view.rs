//! Port of packages/tui/src/components/scroll-view.ts

#![allow(dead_code, unused_imports, unused_variables)]

use std::sync::{Arc, LazyLock, Mutex, MutexGuard, Weak};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::layout_node::{LayoutComponent, LayoutNode, ScrollLayoutNode, ScrollLayoutState, ScrollOverscroll};
use crate::tui::{Component, Container, TuiMouseEvent, TuiMouseEventResult};

// PORT: `class ScrollView extends Container` is composition plus delegation (§9.4).

type DynComponent = Arc<dyn Component>;
type StyleFn = Arc<dyn Fn(&str) -> String + Send + Sync>;
type RequestRender = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScrollViewScrollbar {
    #[serde(rename = "hidden")]
    Hidden,
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "always")]
    Always,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScrollViewAxis {
    #[serde(rename = "vertical")]
    Vertical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScrollViewFollow {
    #[serde(rename = "none")]
    None,
    #[serde(rename = "end")]
    End,
}

/// Field order is the interface.
#[derive(Clone, Default)]
pub struct ScrollViewOptions {
    pub axis: Option<ScrollViewAxis>,
    pub follow: Option<ScrollViewFollow>,
    pub primary: Option<bool>,
    pub overscroll: Option<ScrollOverscroll>,
    pub scrollbar: Option<ScrollViewScrollbar>,
    pub scrollbar_track_style: Option<StyleFn>,
    pub scrollbar_thumb_style: Option<StyleFn>,
    pub scrollbar_hide_delay_ms: Option<i64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScrollViewScrollToOptions {
    /// Keep follow-end disabled even when the target is the current content end.
    pub disable_follow: Option<bool>,
}

struct ScrollViewState {
    current_scrollbar: ScrollViewScrollbar,
    current_scroll_top: i64,
    content_height: i64,
    current_viewport_height: i64,
    following_end: bool,
    follow_suppressed_at_end: bool,
    request_render_callback: Option<RequestRender>,
    transient_scrollbar_visible: bool,
    scrollbar_active: bool,
    scrollbar_hide_timer: Option<pi_js::time::Timeout>,
}

struct ScrollViewInner {
    container: Container,
    child: DynComponent,
    follow_end: bool,
    primary: bool,
    overscroll: ScrollOverscroll,
    scrollbar_track_style: StyleFn,
    scrollbar_thumb_style: StyleFn,
    scrollbar_hide_delay_ms: i64,
    state: Mutex<ScrollViewState>,
}

#[derive(Clone)]
pub struct ScrollView {
    inner: Arc<ScrollViewInner>,
}

impl PartialEq for ScrollView {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for ScrollView {}

static SCROLL_VIEW_BY_STATE_PTR: LazyLock<Mutex<IndexMap<usize, Weak<ScrollViewInner>>>> =
    LazyLock::new(|| Mutex::new(IndexMap::new()));

struct ScrollStateAdapter {
    view: ScrollView,
    ptr_key: usize,
}

impl Drop for ScrollStateAdapter {
    fn drop(&mut self) {
        SCROLL_VIEW_BY_STATE_PTR.lock().unwrap().swap_remove(&self.ptr_key);
    }
}

impl ScrollLayoutState for ScrollStateAdapter {
    fn scroll_top(&self) -> i64 {
        self.view.scroll_top()
    }

    fn primary(&self) -> bool {
        self.view.primary()
    }

    fn overscroll(&self) -> ScrollOverscroll {
        self.view.overscroll()
    }

    fn viewport_height(&self) -> i64 {
        self.view.viewport_height()
    }

    fn get_content_width(&self, width: i64) -> i64 {
        self.view.get_content_width(width)
    }

    fn update_layout(&self, content_height: i64, viewport_height: i64, request_render: RequestRender) {
        self.view.update_layout(content_height, viewport_height, request_render);
    }
}

pub(crate) fn scroll_view_from_layout_state(state: &Arc<dyn ScrollLayoutState>) -> Option<ScrollView> {
    let key = Arc::as_ptr(state) as *const () as usize;
    let map = SCROLL_VIEW_BY_STATE_PTR.lock().unwrap();
    map.get(&key).and_then(Weak::upgrade).map(|inner| ScrollView { inner })
}

impl ScrollView {
    fn state(&self) -> MutexGuard<'_, ScrollViewState> {
        self.inner.state.lock().unwrap()
    }

    /// `options` `None` is `{}`.
    pub fn new(component: DynComponent, options: Option<ScrollViewOptions>) -> Self {
        let opts = options.unwrap_or_default();
        if let Some(axis) = opts.axis
            && axis != ScrollViewAxis::Vertical
        {
            panic!("Unsupported ScrollView axis");
        }
        let follow_end = opts.follow == Some(ScrollViewFollow::End);
        let container = Container::new();
        container.add_child(component.clone());
        Self {
            inner: Arc::new(ScrollViewInner {
                container,
                child: component,
                follow_end,
                primary: opts.primary.unwrap_or(false),
                overscroll: opts.overscroll.unwrap_or(ScrollOverscroll::Chain),
                scrollbar_track_style: opts
                    .scrollbar_track_style
                    .unwrap_or_else(|| Arc::new(|text: &str| format!("\x1b[90m{text}\x1b[39m"))),
                scrollbar_thumb_style: opts
                    .scrollbar_thumb_style
                    .unwrap_or_else(|| Arc::new(|text: &str| format!("\x1b[37m{text}\x1b[39m"))),
                scrollbar_hide_delay_ms: opts.scrollbar_hide_delay_ms.unwrap_or(1000).max(0),
                state: Mutex::new(ScrollViewState {
                    current_scrollbar: opts.scrollbar.unwrap_or(ScrollViewScrollbar::Hidden),
                    current_scroll_top: 0,
                    content_height: 0,
                    current_viewport_height: 0,
                    following_end: follow_end,
                    follow_suppressed_at_end: false,
                    request_render_callback: None,
                    transient_scrollbar_visible: false,
                    scrollbar_active: false,
                    scrollbar_hide_timer: None,
                }),
            }),
        }
    }

    pub fn children(&self) -> Vec<DynComponent> {
        self.inner.container.children()
    }

    pub fn follow_end(&self) -> bool {
        self.inner.follow_end
    }

    pub fn primary(&self) -> bool {
        self.inner.primary
    }

    pub fn overscroll(&self) -> ScrollOverscroll {
        self.inner.overscroll
    }

    pub fn scrollbar_track_style(&self) -> StyleFn {
        Arc::clone(&self.inner.scrollbar_track_style)
    }

    pub fn scrollbar_thumb_style(&self) -> StyleFn {
        Arc::clone(&self.inner.scrollbar_thumb_style)
    }

    pub fn scroll_top(&self) -> i64 {
        self.state().current_scroll_top
    }

    pub fn is_following_end(&self) -> bool {
        self.state().following_end
    }

    pub fn viewport_height(&self) -> i64 {
        self.state().current_viewport_height
    }

    pub fn scrollbar(&self) -> ScrollViewScrollbar {
        self.state().current_scrollbar
    }

    pub fn is_scrollbar_visible(&self) -> bool {
        let state = self.state();
        if state.current_scrollbar == ScrollViewScrollbar::Always {
            return state.current_viewport_height > 0;
        }
        state.current_scrollbar == ScrollViewScrollbar::Auto
            && state.content_height > state.current_viewport_height
            && state.transient_scrollbar_visible
    }

    pub fn is_scrollbar_active(&self) -> bool {
        self.state().scrollbar_active
    }

    pub fn set_scrollbar(&self, scrollbar: ScrollViewScrollbar) {
        let cb = {
            let mut state = self.state();
            if scrollbar == state.current_scrollbar {
                return;
            }
            state.current_scrollbar = scrollbar;
            if scrollbar != ScrollViewScrollbar::Auto {
                Self::hide_transient_scrollbar_locked(&mut state);
            } else if state.scrollbar_active {
                self.mark_scrollbar_activity_locked(&mut state);
            }
            state.request_render_callback.clone()
        };
        if let Some(cb) = cb {
            cb();
        }
    }

    pub fn get_content_width(&self, width: i64) -> i64 {
        let scrollbar = self.state().current_scrollbar;
        if scrollbar == ScrollViewScrollbar::Always && width > 1 {
            width - 1
        } else {
            width
        }
    }

    fn mark_scrollbar_activity(&self) {
        let mut state = self.state();
        self.mark_scrollbar_activity_locked(&mut state);
    }

    fn mark_scrollbar_activity_locked(&self, state: &mut ScrollViewState) {
        if state.current_scrollbar != ScrollViewScrollbar::Auto || state.content_height <= state.current_viewport_height
        {
            return;
        }
        state.transient_scrollbar_visible = true;
        if let Some(timer) = state.scrollbar_hide_timer.take() {
            timer.clear();
        }
        if state.scrollbar_active {
            return;
        }
        let weak_inner = Arc::downgrade(&self.inner);
        let delay = self.inner.scrollbar_hide_delay_ms.max(0) as u64;
        let timer = pi_js::time::set_timeout(delay, move || {
            let Some(inner) = weak_inner.upgrade() else {
                return;
            };
            let cb = {
                let mut state = inner.state.lock().unwrap();
                state.scrollbar_hide_timer = None;
                state.transient_scrollbar_visible = false;
                state.request_render_callback.clone()
            };
            if let Some(cb) = cb {
                cb();
            }
        });
        timer.unref();
        state.scrollbar_hide_timer = Some(timer);
    }

    fn hide_transient_scrollbar(&self) {
        let mut state = self.state();
        Self::hide_transient_scrollbar_locked(&mut state);
    }

    fn hide_transient_scrollbar_locked(state: &mut ScrollViewState) {
        state.transient_scrollbar_visible = false;
        if let Some(timer) = state.scrollbar_hide_timer.take() {
            timer.clear();
        }
    }

    pub fn set_scrollbar_active(&self, active: bool) {
        let cb = {
            let mut state = self.state();
            if active == state.scrollbar_active {
                return;
            }
            state.scrollbar_active = active;
            self.mark_scrollbar_activity_locked(&mut state);
            state.request_render_callback.clone()
        };
        if let Some(cb) = cb {
            cb();
        }
    }

    /// `scroll_top` stays `f64` so non-finite values can keep the current offset.
    pub fn scroll_to(&self, scroll_top: f64, options: Option<ScrollViewScrollToOptions>) {
        let disable_follow = options.and_then(|o| o.disable_follow) == Some(true);
        let cb = {
            let mut state = self.state();
            let requested = if scroll_top.is_finite() {
                scroll_top.trunc().clamp(i64::MIN as f64, i64::MAX as f64) as i64
            } else {
                state.current_scroll_top
            };
            let max_scroll_top = (state.content_height - state.current_viewport_height).max(0);
            let next = requested.clamp(0, max_scroll_top);
            let next_follow_suppressed_at_end = disable_follow && next == max_scroll_top;
            let next_following_end = !next_follow_suppressed_at_end && self.inner.follow_end && next == max_scroll_top;
            if next == state.current_scroll_top
                && next_following_end == state.following_end
                && next_follow_suppressed_at_end == state.follow_suppressed_at_end
            {
                return;
            }
            let moved = next != state.current_scroll_top;
            state.current_scroll_top = next;
            state.following_end = next_following_end;
            state.follow_suppressed_at_end = next_follow_suppressed_at_end;
            if moved {
                self.mark_scrollbar_activity_locked(&mut state);
            }
            state.request_render_callback.clone()
        };
        if let Some(cb) = cb {
            cb();
        }
    }

    pub fn scroll_by(&self, lines: f64) -> i64 {
        let requested = if lines.is_finite() {
            lines.trunc().clamp(i64::MIN as f64, i64::MAX as f64) as i64
        } else {
            0
        };
        if requested == 0 {
            return 0;
        }
        let (unused, cb) = {
            let mut state = self.state();
            let max_scroll_top = (state.content_height - state.current_viewport_height).max(0);
            let start = if state.following_end {
                max_scroll_top
            } else {
                state.current_scroll_top
            };
            let next = (start.saturating_add(requested)).clamp(0, max_scroll_top);
            let moved = next - start;
            let was_following_end = state.following_end;
            state.current_scroll_top = next;
            state.following_end = self.inner.follow_end && next == max_scroll_top;
            state.follow_suppressed_at_end = false;
            if moved != 0 {
                self.mark_scrollbar_activity_locked(&mut state);
            }
            let cb = if moved != 0 || state.following_end != was_following_end {
                state.request_render_callback.clone()
            } else {
                None
            };
            (requested - moved, cb)
        };
        if let Some(cb) = cb {
            cb();
        }
        unused
    }

    pub fn scroll_to_start(&self) {
        let cb = {
            let mut state = self.state();
            let target_follow = self.inner.follow_end && state.content_height <= state.current_viewport_height;
            let changed = state.current_scroll_top != 0 || state.following_end != target_follow;
            state.current_scroll_top = 0;
            state.following_end = target_follow;
            state.follow_suppressed_at_end = false;
            if !changed {
                return;
            }
            self.mark_scrollbar_activity_locked(&mut state);
            state.request_render_callback.clone()
        };
        if let Some(cb) = cb {
            cb();
        }
    }

    pub fn scroll_to_end(&self) {
        let cb = {
            let mut state = self.state();
            let next = (state.content_height - state.current_viewport_height).max(0);
            let changed = state.current_scroll_top != next || state.following_end != self.inner.follow_end;
            state.current_scroll_top = next;
            state.following_end = self.inner.follow_end;
            state.follow_suppressed_at_end = false;
            if !changed {
                return;
            }
            self.mark_scrollbar_activity_locked(&mut state);
            state.request_render_callback.clone()
        };
        if let Some(cb) = cb {
            cb();
        }
    }

    pub fn update_layout(&self, content_height: i64, viewport_height: i64, request_render: RequestRender) {
        let mut state = self.state();
        state.content_height = content_height.max(0);
        state.current_viewport_height = viewport_height.max(0);
        state.request_render_callback = Some(request_render);
        let max_scroll_top = (state.content_height - state.current_viewport_height).max(0);
        if state.following_end {
            state.current_scroll_top = max_scroll_top;
        } else {
            state.current_scroll_top = state.current_scroll_top.clamp(0, max_scroll_top);
        }
        if state.current_scroll_top < max_scroll_top {
            state.follow_suppressed_at_end = false;
        }
        if self.inner.follow_end && state.current_scroll_top == max_scroll_top && !state.follow_suppressed_at_end {
            state.following_end = true;
        }
        if state.content_height <= state.current_viewport_height {
            Self::hide_transient_scrollbar_locked(&mut state);
        }
    }

    pub fn add_child(&self, _component: DynComponent) {
        panic!("ScrollView has exactly one child");
    }

    pub fn remove_child(&self, _component: &DynComponent) {
        panic!("ScrollView child cannot be removed");
    }

    pub fn clear(&self) {
        panic!("ScrollView child cannot be cleared");
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        let content_width = self.get_content_width(width as i64).max(0) as usize;
        let lines = self.inner.child.render(content_width);
        if content_width as i64 == width as i64 {
            lines
        } else {
            lines.into_iter().map(|line| format!("{line} ")).collect()
        }
    }

    pub fn invalidate(&self) {
        Component::invalidate(&self.inner.container);
    }

    pub fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        Component::handle_mouse(&self.inner.container, event)
    }

    pub fn layout_node(&self) -> LayoutNode {
        let adapter: Arc<ScrollStateAdapter> = Arc::new_cyclic(|weak| {
            let ptr_key = weak.as_ptr() as *const () as usize;
            SCROLL_VIEW_BY_STATE_PTR
                .lock()
                .unwrap()
                .insert(ptr_key, Arc::downgrade(&self.inner));
            ScrollStateAdapter {
                view: self.clone(),
                ptr_key,
            }
        });
        LayoutNode::Scroll(ScrollLayoutNode {
            component: Arc::clone(&self.inner.child),
            state: adapter,
        })
    }
}

impl Component for ScrollView {
    fn render(&self, width: usize) -> Vec<String> {
        ScrollView::render(self, width)
    }

    fn handle_input(&self, _data: &str) {}

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        ScrollView::handle_mouse(self, event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        ScrollView::invalidate(self)
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

impl ScrollLayoutState for ScrollView {
    fn scroll_top(&self) -> i64 {
        ScrollView::scroll_top(self)
    }

    fn primary(&self) -> bool {
        ScrollView::primary(self)
    }

    fn overscroll(&self) -> ScrollOverscroll {
        ScrollView::overscroll(self)
    }

    fn viewport_height(&self) -> i64 {
        ScrollView::viewport_height(self)
    }

    fn get_content_width(&self, width: i64) -> i64 {
        ScrollView::get_content_width(self, width)
    }

    fn update_layout(&self, content_height: i64, viewport_height: i64, request_render: RequestRender) {
        ScrollView::update_layout(self, content_height, viewport_height, request_render)
    }
}

impl LayoutComponent for ScrollView {
    fn layout_node(&self) -> LayoutNode {
        ScrollView::layout_node(self)
    }
}
