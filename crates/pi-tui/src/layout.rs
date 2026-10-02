//! Port of packages/tui/src/layout.ts

#![allow(dead_code, unused_imports, unused_variables)]

use std::sync::{Arc, Mutex, Weak};

use indexmap::IndexMap;

use crate::components::scroll_view::ScrollView;
use crate::layout_node::LayoutViewport;
use crate::tui::Component;

// PORT: object literals disagree on optional `LayoutBox` keys (`lines`/`lineOffset`,
// `scrollView`/`scrollContentLines`, `parent`). Field order follows the interface.
// PORT: `parent` is a `Weak` back-edge. `children` own the tree.

type DynComponent = Arc<dyn Component>;
type RequestRender = Arc<dyn Fn() + Send + Sync>;

/// Identity key for `Map<Component, …>`. TS map keys are object identity.
struct ComponentCacheKey(DynComponent);

impl PartialEq for ComponentCacheKey {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for ComponentCacheKey {}

impl std::hash::Hash for ComponentCacheKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (Arc::as_ptr(&self.0) as *const ()).hash(state);
    }
}

/// `/^(?:\x1b\]133;[ABC](?:\x07|\x1b\\))+/`
static OSC133_ZONE_PREFIX: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new("^(?:\\x1b\\]133;[ABC](?:\\x07|\\x1b\\\\))+").expect("OSC133_ZONE_PREFIX")
});

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutRect {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
}

#[derive(Clone)]
pub struct LayoutBox {
    inner: Arc<LayoutBoxInner>,
}

struct LayoutBoxInner {
    component: DynComponent,
    rect: Mutex<LayoutRect>,
    clip: Mutex<LayoutRect>,
    children: Mutex<Vec<LayoutBox>>,
    parent: Mutex<Option<Weak<LayoutBoxInner>>>,
    lines: Option<Vec<String>>,
    line_offset: Option<i64>,
    scroll_view: Option<ScrollView>,
    scroll_content_lines: Option<Vec<String>>,
    layer: i64,
}

impl LayoutBox {
    pub fn component(&self) -> DynComponent {
        Arc::clone(&self.inner.component)
    }

    pub fn rect(&self) -> LayoutRect {
        *self.inner.rect.lock().unwrap()
    }

    pub fn clip(&self) -> LayoutRect {
        *self.inner.clip.lock().unwrap()
    }

    pub fn children(&self) -> Vec<LayoutBox> {
        self.inner.children.lock().unwrap().clone()
    }

    pub fn parent(&self) -> Option<LayoutBox> {
        self.inner
            .parent
            .lock()
            .unwrap()
            .as_ref()
            .and_then(Weak::upgrade)
            .map(|inner| LayoutBox { inner })
    }

    pub fn lines(&self) -> Option<Vec<String>> {
        self.inner.lines.clone()
    }

    pub fn line_offset(&self) -> Option<i64> {
        self.inner.line_offset
    }

    pub fn scroll_view(&self) -> Option<ScrollView> {
        self.inner.scroll_view.clone()
    }

    pub fn scroll_content_lines(&self) -> Option<Vec<String>> {
        self.inner.scroll_content_lines.clone()
    }

    pub fn layer(&self) -> i64 {
        self.inner.layer
    }
}

#[derive(Clone)]
pub struct LayoutFrame {
    pub root: LayoutBox,
    pub width: i64,
    pub height: i64,
    pub lines: Vec<String>,
    pub primary_scroll_view: Option<ScrollView>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScrollbarGeometry {
    pub column: i64,
    pub track_top: i64,
    pub track_height: i64,
    pub thumb_top: i64,
    pub thumb_height: i64,
    pub max_scroll_top: i64,
}

struct LayoutContext {
    viewport: LayoutViewport,
    render_cache: IndexMap<ComponentCacheKey, IndexMap<i64, Vec<String>>>,
    request_render: RequestRender,
    primary_scroll_view: Option<ScrollView>,
}

struct LayoutHit {
    r#box: LayoutBox,
    depth: i64,
}

struct ScrollViewHit {
    scroll_view: ScrollView,
    depth: i64,
}

fn intersect(a: &LayoutRect, b: &LayoutRect) -> LayoutRect {
    todo!("port: intersect")
}

fn render_cached(context: &LayoutContext, component: &DynComponent, width: i64) -> Vec<String> {
    todo!("port: render_cached")
}

fn measure_height(context: &LayoutContext, component: &DynComponent, width: i64) -> i64 {
    todo!("port: measure_height")
}

fn measure_width(context: &LayoutContext, component: &DynComponent, width: i64) -> i64 {
    todo!("port: measure_width")
}

fn with_parent(r#box: LayoutBox, parent: &LayoutBox) -> LayoutBox {
    todo!("port: with_parent")
}

fn translate_box(r#box: &LayoutBox, delta_y: i64) {
    todo!("port: translate_box")
}

fn update_clips(r#box: &LayoutBox, parent_clip: LayoutRect) {
    todo!("port: update_clips")
}

fn layout_component(
    context: &mut LayoutContext,
    component: &DynComponent,
    x: i64,
    y: i64,
    width: i64,
    height: Option<i64>,
    clip: LayoutRect,
) -> LayoutBox {
    todo!("port: layout_component")
}

fn replace_scrollbar_cell(
    line: &str,
    column: i64,
    total_width: i64,
    replacement: &str,
    preserve_target_background: bool,
) -> String {
    todo!("port: replace_scrollbar_cell")
}

pub fn get_scrollbar_geometry(r#box: &LayoutBox, include_hidden_auto: Option<bool>) -> Option<ScrollbarGeometry> {
    todo!("port: get_scrollbar_geometry")
}

fn paint_scrollbar(r#box: &LayoutBox, screen: &mut [String], total_width: i64) {
    todo!("port: paint_scrollbar")
}

fn paint_box(r#box: &LayoutBox, screen: &mut [String], total_width: i64) {
    todo!("port: paint_box")
}

pub fn render_layout_frame(root: DynComponent, width: i64, height: i64, request_render: RequestRender) -> LayoutFrame {
    todo!("port: render_layout_frame")
}

fn contains_point(rect: &LayoutRect, x: i64, y: i64) -> bool {
    todo!("port: contains_point")
}

/// Return the visual hit path from the deepest component to the layout root.
pub fn get_layout_boxes_at(frame: &LayoutFrame, x: i64, y: i64) -> Vec<LayoutBox> {
    todo!("port: get_layout_boxes_at")
}

pub fn get_scroll_view_box(frame: &LayoutFrame, scroll_view: &ScrollView) -> Option<LayoutBox> {
    todo!("port: get_scroll_view_box")
}

pub fn get_scroll_views_at(frame: &LayoutFrame, x: i64, y: i64) -> Vec<ScrollView> {
    todo!("port: get_scroll_views_at")
}
