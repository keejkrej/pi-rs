//! Port of packages/tui/src/layout.ts

#![allow(dead_code, unused_imports, unused_variables)]

use std::sync::{Arc, Mutex, Weak};

use indexmap::IndexMap;

use crate::components::scroll_view::{ScrollView, ScrollViewScrollbar, scroll_view_from_layout_state};
use crate::components::stack::{allocate_stack_sizes, visible_stack_entries};
use crate::layout_node::{LayoutNode, LayoutViewport, StackAlign, StackBasis, StackLayoutType, get_layout_node};
use crate::terminal_image::{crop_kitty_image_line, get_kitty_image_metadata, get_max_kitty_image_rows, is_image_line};
use crate::tui::{CURSOR_MARKER, Component, composite_tui_line};
use crate::utils::{
    extract_ansi_code, get_active_background_ansi, get_grapheme_cell_range, slice_by_column, visible_width,
};

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

fn js_round(x: f64) -> i64 {
    (x + 0.5).floor() as i64
}

fn intersect(a: &LayoutRect, b: &LayoutRect) -> LayoutRect {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = (a.x + a.width).min(b.x + b.width);
    let bottom = (a.y + a.height).min(b.y + b.height);
    LayoutRect {
        x,
        y,
        width: (right - x).max(0),
        height: (bottom - y).max(0),
    }
}

fn render_cached(context: &mut LayoutContext, component: &DynComponent, width: i64) -> Vec<String> {
    let safe_width = width.max(1);
    let key = ComponentCacheKey(Arc::clone(component));
    if let Some(widths) = context.render_cache.get(&key)
        && let Some(lines) = widths.get(&safe_width)
    {
        return lines.clone();
    }
    let lines = component.render(safe_width as usize);
    context
        .render_cache
        .entry(key)
        .or_default()
        .insert(safe_width, lines.clone());
    lines
}

fn measure_height(context: &mut LayoutContext, component: &DynComponent, width: i64) -> i64 {
    render_cached(context, component, width).len() as i64
}

fn measure_width(context: &mut LayoutContext, component: &DynComponent, width: i64) -> i64 {
    render_cached(context, component, width)
        .iter()
        .map(|line| visible_width(line))
        .max()
        .unwrap_or(0)
}

fn with_parent(r#box: LayoutBox, parent: &LayoutBox) -> LayoutBox {
    *r#box.inner.parent.lock().unwrap() = Some(Arc::downgrade(&parent.inner));
    r#box
}

fn translate_box(r#box: &LayoutBox, delta_y: i64) {
    r#box.inner.rect.lock().unwrap().y += delta_y;
    let children = r#box.children();
    for child in &children {
        translate_box(child, delta_y);
    }
}

fn update_clips(r#box: &LayoutBox, parent_clip: LayoutRect) {
    let clip = {
        let rect = *r#box.inner.rect.lock().unwrap();
        let c = intersect(&parent_clip, &rect);
        *r#box.inner.clip.lock().unwrap() = c;
        c
    };
    let children = r#box.children();
    for child in &children {
        update_clips(child, clip);
    }
}

fn make_box(
    component: DynComponent,
    rect: LayoutRect,
    clip: LayoutRect,
    children: Vec<LayoutBox>,
    lines: Option<Vec<String>>,
    line_offset: Option<i64>,
    scroll_view: Option<ScrollView>,
    scroll_content_lines: Option<Vec<String>>,
) -> LayoutBox {
    LayoutBox {
        inner: Arc::new(LayoutBoxInner {
            component,
            rect: Mutex::new(rect),
            clip: Mutex::new(clip),
            children: Mutex::new(children),
            parent: Mutex::new(None),
            lines,
            line_offset,
            scroll_view,
            scroll_content_lines,
            layer: 0,
        }),
    }
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
    let safe_width = width.max(1);
    let Some(node) = get_layout_node(&**component) else {
        let lines = render_cached(context, component, safe_width);
        let allocated_height = match height {
            None => lines.len() as i64,
            Some(h) => h.max(0),
        };
        let mut line_offset = 0i64;
        if (lines.len() as i64) > allocated_height && allocated_height > 0 {
            if let Some(cursor_line) = lines.iter().position(|line| line.contains(CURSOR_MARKER))
                && (cursor_line as i64) >= allocated_height
            {
                line_offset = cursor_line as i64 - allocated_height + 1;
            }
        }
        let rect = LayoutRect {
            x,
            y,
            width: safe_width,
            height: allocated_height,
        };
        return make_box(
            Arc::clone(component),
            rect,
            intersect(&clip, &rect),
            Vec::new(),
            Some(lines),
            Some(line_offset),
            None,
            None,
        );
    };

    match node {
        LayoutNode::Scroll(scroll_node) => {
            let previous_scroll_top = scroll_node.state.scroll_top();
            let content_width = scroll_node.state.get_content_width(safe_width);
            let child_box = layout_component(
                context,
                &scroll_node.component,
                x,
                y - previous_scroll_top,
                content_width,
                None,
                clip,
            );
            let content_height = child_box.rect().height;
            let viewport_height = match height {
                None => content_height,
                Some(h) => h.max(0),
            };
            scroll_node
                .state
                .update_layout(content_height, viewport_height, Arc::clone(&context.request_render));
            translate_box(&child_box, previous_scroll_top - scroll_node.state.scroll_top());
            let scroll_view = scroll_view_from_layout_state(&scroll_node.state);
            if let Some(ref sv) = scroll_view
                && (scroll_node.state.primary() || context.primary_scroll_view.is_none())
            {
                context.primary_scroll_view = Some(sv.clone());
            }
            let rect = LayoutRect {
                x,
                y,
                width: safe_width,
                height: viewport_height,
            };
            let child_clip = intersect(&clip, &rect);
            let scroll_content_lines = render_cached(context, &scroll_node.component, content_width);
            let r#box = make_box(
                Arc::clone(component),
                rect,
                child_clip,
                vec![child_box.clone()],
                None,
                None,
                scroll_view,
                Some(scroll_content_lines),
            );
            *child_box.inner.parent.lock().unwrap() = Some(Arc::downgrade(&r#box.inner));
            update_clips(&child_box, child_clip);
            r#box
        }
        LayoutNode::Stack(stack_node) => {
            let entries = visible_stack_entries(&stack_node.entries, &context.viewport);
            let gap_total = ((entries.len() as i64) - 1).max(0) * stack_node.gap;
            if stack_node.r#type == StackLayoutType::VStack {
                let intrinsic_heights: Vec<i64> = entries
                    .iter()
                    .map(|entry| match entry.basis {
                        Some(StackBasis::Fixed(n)) => n.floor() as i64,
                        _ => measure_height(context, &entry.component, safe_width),
                    })
                    .collect();
                let sizes = allocate_stack_sizes(&entries, &intrinsic_heights, height, stack_node.gap);
                let natural_height = sizes.iter().sum::<i64>() + gap_total;
                let allocated_height = match height {
                    None => natural_height,
                    Some(h) => h.max(0),
                };
                let rect = LayoutRect {
                    x,
                    y,
                    width: safe_width,
                    height: allocated_height,
                };
                let box_clip = intersect(&clip, &rect);
                let r#box = make_box(
                    Arc::clone(component),
                    rect,
                    box_clip,
                    Vec::with_capacity(entries.len()),
                    None,
                    None,
                    None,
                    None,
                );
                let mut child_y = y;
                let mut children = Vec::with_capacity(entries.len());
                for (index, entry) in entries.iter().enumerate() {
                    let child_size = sizes[index];
                    let child = with_parent(
                        layout_component(
                            context,
                            &entry.component,
                            x,
                            child_y,
                            safe_width,
                            Some(child_size),
                            box_clip,
                        ),
                        &r#box,
                    );
                    children.push(child);
                    child_y += child_size + stack_node.gap;
                }
                *r#box.inner.children.lock().unwrap() = children;
                return r#box;
            }

            let intrinsic_widths: Vec<i64> = entries
                .iter()
                .map(|entry| match entry.basis {
                    Some(StackBasis::Fixed(n)) => n.floor() as i64,
                    _ => measure_width(context, &entry.component, safe_width),
                })
                .collect();
            let widths = allocate_stack_sizes(&entries, &intrinsic_widths, Some(safe_width), stack_node.gap);
            let intrinsic_heights: Vec<i64> = entries
                .iter()
                .enumerate()
                .map(|(index, entry)| measure_height(context, &entry.component, widths[index].max(1)))
                .collect();
            let allocated_height = match height {
                None => intrinsic_heights.iter().copied().max().unwrap_or(0),
                Some(h) => h.max(0),
            };
            let rect = LayoutRect {
                x,
                y,
                width: safe_width,
                height: allocated_height,
            };
            let box_clip = intersect(&clip, &rect);
            let r#box = make_box(
                Arc::clone(component),
                rect,
                box_clip,
                Vec::with_capacity(entries.len()),
                None,
                None,
                None,
                None,
            );
            let mut child_x = x;
            let mut children = Vec::with_capacity(entries.len());
            for (index, entry) in entries.iter().enumerate() {
                let natural_child_height = intrinsic_heights[index];
                let child_height = if stack_node.align == StackAlign::Stretch {
                    allocated_height
                } else {
                    allocated_height.min(natural_child_height)
                };
                let mut child_y = y;
                match stack_node.align {
                    StackAlign::Center => child_y += (allocated_height - child_height).div_euclid(2),
                    StackAlign::End => child_y += allocated_height - child_height,
                    StackAlign::Stretch | StackAlign::Start => {}
                }
                let child_width = widths[index];
                if child_width == 0 {
                    let zero_box = make_box(
                        Arc::clone(&entry.component),
                        LayoutRect {
                            x: child_x,
                            y: child_y,
                            width: 0,
                            height: child_height,
                        },
                        LayoutRect {
                            x: child_x,
                            y: child_y,
                            width: 0,
                            height: 0,
                        },
                        Vec::new(),
                        None,
                        None,
                        None,
                        None,
                    );
                    *zero_box.inner.parent.lock().unwrap() = Some(Arc::downgrade(&r#box.inner));
                    children.push(zero_box);
                } else {
                    children.push(with_parent(
                        layout_component(
                            context,
                            &entry.component,
                            child_x,
                            child_y,
                            child_width,
                            Some(child_height),
                            box_clip,
                        ),
                        &r#box,
                    ));
                }
                child_x += child_width + stack_node.gap;
            }
            *r#box.inner.children.lock().unwrap() = children;
            r#box
        }
    }
}

fn replace_scrollbar_cell(
    line: &str,
    column: i64,
    total_width: i64,
    replacement: &str,
    preserve_target_background: bool,
) -> String {
    if is_image_line(line) {
        return line.to_string();
    }

    let grapheme_range = get_grapheme_cell_range(line, column);
    let start = grapheme_range.map(|r| r.start).unwrap_or(column);
    let end = grapheme_range.map(|r| r.end).unwrap_or(column + 1);
    let before = slice_by_column(line, 0, start, Some(true));
    let target = slice_by_column(line, start, end - start, Some(true));
    let after = slice_by_column(line, end, (total_width - end).max(0), Some(true));

    let mut target_prefix = String::new();
    let mut target_index = 0i64;
    let target_len = pi_js::str16::len(&target) as i64;
    while target_index < target_len {
        let Some(ansi) = extract_ansi_code(&target, target_index) else {
            break;
        };
        target_prefix.push_str(&ansi.code);
        target_index += ansi.length;
    }

    let before_padding = " ".repeat((start - visible_width(&before)).max(0) as usize);
    let cell_padding_before = " ".repeat((column - start).max(0) as usize);
    let cell_padding_after = " ".repeat((end - column - 1).max(0) as usize);
    let active_bg = if preserve_target_background {
        get_active_background_ansi(&target_prefix)
    } else {
        String::new()
    };
    let target_style = format!("\x1b[0m\x1b]8;;\x07{active_bg}");
    format!("{before}{before_padding}{target_style}{cell_padding_before}{replacement}{cell_padding_after}{after}")
}

pub fn get_scrollbar_geometry(r#box: &LayoutBox, include_hidden_auto: Option<bool>) -> Option<ScrollbarGeometry> {
    let scroll_view = r#box.scroll_view()?;
    let rect = r#box.rect();
    let clip = r#box.clip();
    if rect.width <= 0 || rect.height <= 0 {
        return None;
    }

    let children = r#box.children();
    let content_height = children
        .first()
        .map(|c| c.rect().height)
        .or_else(|| r#box.inner.scroll_content_lines.as_ref().map(|l| l.len() as i64))
        .unwrap_or(0);
    let track_height = rect.height;
    let can_reveal_hidden_auto = include_hidden_auto == Some(true)
        && scroll_view.scrollbar() == ScrollViewScrollbar::Auto
        && content_height > track_height;
    if !scroll_view.is_scrollbar_visible() && !can_reveal_hidden_auto {
        return None;
    }

    let min_thumb_height = track_height.min(2);
    let raw_thumb = if content_height <= 0 {
        track_height
    } else {
        js_round((track_height as f64 * track_height as f64) / content_height as f64)
    };
    let thumb_height = raw_thumb.min(track_height).max(min_thumb_height);
    let max_scroll_top = (content_height - track_height).max(0);
    let max_thumb_top = track_height - thumb_height;
    let thumb_offset = if max_scroll_top == 0 {
        0
    } else {
        js_round((scroll_view.scroll_top() as f64 / max_scroll_top as f64) * max_thumb_top as f64)
    };
    let column = rect.x + rect.width - 1;
    if column < clip.x || column >= clip.x + clip.width {
        return None;
    }

    Some(ScrollbarGeometry {
        column,
        track_top: rect.y,
        track_height,
        thumb_top: rect.y + thumb_offset,
        thumb_height,
        max_scroll_top,
    })
}

fn paint_scrollbar(r#box: &LayoutBox, screen: &mut [String], total_width: i64) {
    let Some(geometry) = get_scrollbar_geometry(r#box, None) else {
        return;
    };
    let Some(scroll_view) = r#box.scroll_view() else {
        return;
    };
    let clip = r#box.clip();
    let preserve_bg = scroll_view.scrollbar() != ScrollViewScrollbar::Always;
    let thumb_style = scroll_view.scrollbar_thumb_style();
    let track_style = scroll_view.scrollbar_track_style();
    let is_active = scroll_view.is_scrollbar_active();

    for offset in 0..geometry.track_height {
        let row = geometry.track_top + offset;
        if row < clip.y || row >= clip.y + clip.height || row < 0 || row >= screen.len() as i64 {
            continue;
        }
        let is_thumb = row >= geometry.thumb_top && row < geometry.thumb_top + geometry.thumb_height;
        let replacement = if is_thumb {
            thumb_style(if is_active { "█" } else { "┃" })
        } else {
            track_style("│")
        };
        let idx = row as usize;
        screen[idx] = replace_scrollbar_cell(&screen[idx], geometry.column, total_width, &replacement, preserve_bg);
    }
}

fn paint_box(r#box: &LayoutBox, screen: &mut [String], total_width: i64) {
    let rect = r#box.rect();
    let clip = r#box.clip();
    if let Some(ref lines) = r#box.inner.lines {
        let offset = r#box.inner.line_offset.unwrap_or(0);
        let first_row = rect.y.max(clip.y).max(0);
        let last_row = (rect.y + rect.height)
            .min(clip.y + clip.height)
            .min(screen.len() as i64);
        for row in first_row..last_row {
            let line_idx = offset + row - rect.y;
            if line_idx < 0 {
                continue;
            }
            let Some(source_line) = lines.get(line_idx as usize) else {
                continue;
            };
            let mut line = OSC133_ZONE_PREFIX.replace(source_line, "").into_owned();
            if let Some(image_metadata) = get_kitty_image_metadata(&line) {
                let clip_bottom = (screen.len() as i64).min(clip.y + clip.height);
                let visible_rows = image_metadata.rows.min(clip_bottom - row);
                if visible_rows < image_metadata.rows {
                    line = crop_kitty_image_line(&line, 0, visible_rows);
                }
            }
            let row_idx = row as usize;
            if rect.x == 0 && rect.width >= total_width && (is_image_line(&line) || screen[row_idx].is_empty()) {
                screen[row_idx] = line;
            } else {
                screen[row_idx] = composite_tui_line(&screen[row_idx], &line, rect.x, rect.width, total_width);
            }
        }
    }

    for child in r#box.children() {
        paint_box(&child, screen, total_width);
    }

    if let Some(ref scroll_view) = r#box.inner.scroll_view
        && let Some(ref scroll_content_lines) = r#box.inner.scroll_content_lines
        && scroll_view.scroll_top() > 0
        && rect.height > 0
    {
        // Find the nearest image line above the viewport. Placeholder rows below an
        // image line may contain padding, borders, or side-by-side text, so the search
        // is bounded by the tallest registered image instead of by row content.
        let scroll_top = scroll_view.scroll_top();
        let min_row = 0_i64.max(scroll_top - get_max_kitty_image_rows());
        let upper = scroll_top.min(scroll_content_lines.len() as i64);
        for image_row in (min_row..upper).rev() {
            let image_line = &scroll_content_lines[image_row as usize];
            if !is_image_line(image_line) {
                continue;
            }
            let Some(metadata) = get_kitty_image_metadata(image_line) else {
                continue;
            };
            let hidden_rows = scroll_top - image_row;
            if hidden_rows < metadata.rows {
                let visible_rows = rect.height.min(metadata.rows - hidden_rows);
                let cropped = crop_kitty_image_line(image_line, hidden_rows, visible_rows);
                if rect.x == 0 && rect.width >= total_width && rect.y >= 0 && (rect.y as usize) < screen.len() {
                    screen[rect.y as usize] = cropped;
                }
            }
            break;
        }
    }

    paint_scrollbar(r#box, screen, total_width);
}

pub fn render_layout_frame(root: DynComponent, width: i64, height: i64, request_render: RequestRender) -> LayoutFrame {
    let safe_width = width.max(1);
    let safe_height = height.max(1);
    let mut context = LayoutContext {
        viewport: LayoutViewport {
            width: safe_width,
            height: safe_height,
        },
        render_cache: IndexMap::new(),
        request_render,
        primary_scroll_view: None,
    };
    let root_box = layout_component(
        &mut context,
        &root,
        0,
        0,
        safe_width,
        Some(safe_height),
        LayoutRect {
            x: 0,
            y: 0,
            width: safe_width,
            height: safe_height,
        },
    );
    let mut lines = vec![String::new(); safe_height as usize];
    paint_box(&root_box, &mut lines, safe_width);
    LayoutFrame {
        root: root_box,
        width: safe_width,
        height: safe_height,
        lines,
        primary_scroll_view: context.primary_scroll_view,
    }
}

fn contains_point(rect: &LayoutRect, x: i64, y: i64) -> bool {
    x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
}

/// Return the visual hit path from the deepest component to the layout root.
pub fn get_layout_boxes_at(frame: &LayoutFrame, x: i64, y: i64) -> Vec<LayoutBox> {
    fn visit(r#box: &LayoutBox, depth: i64, x: i64, y: i64, out: &mut Vec<LayoutHit>) {
        if !contains_point(&r#box.clip(), x, y) {
            return;
        }
        out.push(LayoutHit {
            r#box: r#box.clone(),
            depth,
        });
        for child in r#box.children() {
            visit(&child, depth + 1, x, y, out);
        }
    }
    let mut result = Vec::new();
    visit(&frame.root, 0, x, y, &mut result);
    result.sort_by(|a, b| {
        b.r#box
            .layer()
            .cmp(&a.r#box.layer())
            .then_with(|| b.depth.cmp(&a.depth))
    });
    result.into_iter().map(|hit| hit.r#box).collect()
}

pub fn get_scroll_view_box(frame: &LayoutFrame, scroll_view: &ScrollView) -> Option<LayoutBox> {
    fn visit(r#box: &LayoutBox, target: &ScrollView) -> Option<LayoutBox> {
        if r#box.inner.scroll_view.as_ref() == Some(target) {
            return Some(r#box.clone());
        }
        for child in r#box.children() {
            if let Some(matched) = visit(&child, target) {
                return Some(matched);
            }
        }
        None
    }
    visit(&frame.root, scroll_view)
}

pub fn get_scroll_views_at(frame: &LayoutFrame, x: i64, y: i64) -> Vec<ScrollView> {
    fn visit(r#box: &LayoutBox, depth: i64, x: i64, y: i64, out: &mut Vec<ScrollViewHit>) {
        if !contains_point(&r#box.clip(), x, y) {
            return;
        }
        if let Some(sv) = r#box.scroll_view()
            && contains_point(&r#box.rect(), x, y)
        {
            out.push(ScrollViewHit { scroll_view: sv, depth });
        }
        for child in r#box.children() {
            visit(&child, depth + 1, x, y, out);
        }
    }
    let mut result = Vec::new();
    visit(&frame.root, 0, x, y, &mut result);
    result.sort_by(|a, b| b.depth.cmp(&a.depth));
    result.into_iter().map(|entry| entry.scroll_view).collect()
}
