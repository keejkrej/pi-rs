//! Port of packages/tui/src/components/scroll-view.ts

#![allow(dead_code, unused_imports, unused_variables)]

use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

use crate::layout_node::{LayoutComponent, LayoutNode, ScrollLayoutState, ScrollOverscroll};
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

impl ScrollView {
    fn state(&self) -> MutexGuard<'_, ScrollViewState> {
        self.inner.state.lock().unwrap()
    }

    /// `options` `None` is `{}`.
    pub fn new(component: DynComponent, options: Option<ScrollViewOptions>) -> Self {
        todo!("port: ScrollView::new")
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
        todo!("port: ScrollView::is_scrollbar_visible")
    }

    pub fn is_scrollbar_active(&self) -> bool {
        self.state().scrollbar_active
    }

    pub fn set_scrollbar(&self, scrollbar: ScrollViewScrollbar) {
        todo!("port: ScrollView::set_scrollbar")
    }

    pub fn get_content_width(&self, width: i64) -> i64 {
        todo!("port: ScrollView::get_content_width")
    }

    fn mark_scrollbar_activity(&self) {
        todo!("port: ScrollView::mark_scrollbar_activity")
    }

    fn hide_transient_scrollbar(&self) {
        todo!("port: ScrollView::hide_transient_scrollbar")
    }

    pub fn set_scrollbar_active(&self, active: bool) {
        todo!("port: ScrollView::set_scrollbar_active")
    }

    /// `scroll_top` stays `f64` so non-finite values can keep the current offset.
    pub fn scroll_to(&self, scroll_top: f64, options: Option<ScrollViewScrollToOptions>) {
        todo!("port: ScrollView::scroll_to")
    }

    pub fn scroll_by(&self, lines: f64) -> i64 {
        todo!("port: ScrollView::scroll_by")
    }

    pub fn scroll_to_start(&self) {
        todo!("port: ScrollView::scroll_to_start")
    }

    pub fn scroll_to_end(&self) {
        todo!("port: ScrollView::scroll_to_end")
    }

    pub fn update_layout(&self, content_height: i64, viewport_height: i64, request_render: RequestRender) {
        todo!("port: ScrollView::update_layout")
    }

    pub fn add_child(&self, component: DynComponent) {
        todo!("port: ScrollView::add_child")
    }

    pub fn remove_child(&self, component: &DynComponent) {
        todo!("port: ScrollView::remove_child")
    }

    pub fn clear(&self) {
        todo!("port: ScrollView::clear")
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        todo!("port: ScrollView::render")
    }

    pub fn invalidate(&self) {
        Component::invalidate(&self.inner.container);
    }

    pub fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        Component::handle_mouse(&self.inner.container, event)
    }

    pub fn layout_node(&self) -> LayoutNode {
        todo!("port: ScrollView::layout_node")
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
