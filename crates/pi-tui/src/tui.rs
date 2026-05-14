use std::boxed::Box as StdBox;
use std::collections::BTreeSet;

use crate::terminal::Terminal;
use crate::terminal_image::delete_kitty_image;
use crate::utils::{slice_by_column, truncate_to_width, truncate_to_width_with, visible_width};

pub const CURSOR_MARKER: &str = "\u{1b}_pi:c\u{7}";

pub trait Component {
    fn render(&self, width: usize) -> Vec<String>;

    fn handle_input(&mut self, _data: &str) {}

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&mut self) {}
}

pub trait Focusable {
    fn set_focused(&mut self, focused: bool);
    fn focused(&self) -> bool;
}

#[derive(Default)]
pub struct Container {
    children: Vec<StdBox<dyn Component>>,
}

impl Container {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_child<C>(&mut self, component: C)
    where
        C: Component + 'static,
    {
        self.children.push(StdBox::new(component));
    }

    pub fn remove_child(&mut self, index: usize) -> Option<StdBox<dyn Component>> {
        if index < self.children.len() {
            Some(self.children.remove(index))
        } else {
            None
        }
    }

    pub fn clear(&mut self) {
        self.children.clear();
    }

    pub fn len(&self) -> usize {
        self.children.len()
    }

    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }

    pub fn children(&self) -> &[StdBox<dyn Component>] {
        &self.children
    }

    pub fn children_mut(&mut self) -> &mut [StdBox<dyn Component>] {
        &mut self.children
    }
}

impl Component for Container {
    fn render(&self, width: usize) -> Vec<String> {
        self.children
            .iter()
            .flat_map(|child| child.render(width))
            .map(|line| truncate_to_width(&line, width))
            .collect()
    }

    fn handle_input(&mut self, data: &str) {
        if let Some(child) = self.children.last_mut() {
            child.handle_input(data);
        }
    }

    fn invalidate(&mut self) {
        for child in &mut self.children {
            child.invalidate();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayAnchor {
    Center,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    TopCenter,
    BottomCenter,
    LeftCenter,
    RightCenter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OverlayMargin {
    pub top: usize,
    pub right: usize,
    pub bottom: usize,
    pub left: usize,
}

impl OverlayMargin {
    pub fn all(value: usize) -> Self {
        Self {
            top: value,
            right: value,
            bottom: value,
            left: value,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeValue {
    Columns(usize),
    Percent(u8),
}

impl SizeValue {
    pub fn resolve(self, reference: usize) -> usize {
        match self {
            SizeValue::Columns(value) => value,
            SizeValue::Percent(percent) => reference.saturating_mul(percent as usize) / 100,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayOptions {
    pub width: Option<SizeValue>,
    pub min_width: Option<usize>,
    pub max_height: Option<SizeValue>,
    pub anchor: OverlayAnchor,
    pub offset_x: isize,
    pub offset_y: isize,
    pub row: Option<SizeValue>,
    pub col: Option<SizeValue>,
    pub margin: OverlayMargin,
    pub non_capturing: bool,
}

impl Default for OverlayOptions {
    fn default() -> Self {
        Self {
            width: None,
            min_width: None,
            max_height: None,
            anchor: OverlayAnchor::Center,
            offset_x: 0,
            offset_y: 0,
            row: None,
            col: None,
            margin: OverlayMargin::default(),
            non_capturing: false,
        }
    }
}

struct Overlay {
    id: usize,
    component: StdBox<dyn Component>,
    options: OverlayOptions,
    hidden: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayHandle {
    id: usize,
}

impl OverlayHandle {
    pub fn id(&self) -> usize {
        self.id
    }
}

pub struct TUI<T: Terminal> {
    pub terminal: T,
    container: Container,
    overlays: Vec<Overlay>,
    focused_overlay: Option<usize>,
    next_overlay_id: usize,
    running: bool,
    previous_kitty_image_ids: BTreeSet<u32>,
    previous_width: usize,
    previous_height: usize,
    max_lines_rendered: usize,
    clear_on_shrink: bool,
    full_redraws: usize,
}

impl<T: Terminal> TUI<T> {
    pub fn new(terminal: T) -> Self {
        Self {
            terminal,
            container: Container::new(),
            overlays: Vec::new(),
            focused_overlay: None,
            next_overlay_id: 1,
            running: false,
            previous_kitty_image_ids: BTreeSet::new(),
            previous_width: 0,
            previous_height: 0,
            max_lines_rendered: 0,
            clear_on_shrink: std::env::var("PI_CLEAR_ON_SHRINK").is_ok_and(|value| value == "1"),
            full_redraws: 0,
        }
    }

    pub fn add_child<C>(&mut self, component: C)
    where
        C: Component + 'static,
    {
        self.container.add_child(component);
    }

    pub fn remove_child(&mut self, index: usize) -> Option<StdBox<dyn Component>> {
        self.container.remove_child(index)
    }

    pub fn clear(&mut self) {
        self.container.clear();
    }

    pub fn start(&mut self) -> anyhow::Result<()> {
        self.running = true;
        self.terminal.start()
    }

    pub fn stop(&mut self) -> anyhow::Result<()> {
        self.running = false;
        self.terminal.stop()
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    pub fn request_render(&mut self) -> anyhow::Result<()> {
        self.render_now_with_full_redraw(false)
    }

    pub fn request_render_full(&mut self) -> anyhow::Result<()> {
        self.render_now_with_full_redraw(true)
    }

    pub fn full_redraws(&self) -> usize {
        self.full_redraws
    }

    pub fn set_clear_on_shrink(&mut self, clear: bool) {
        self.clear_on_shrink = clear;
    }

    pub fn render_lines(&self) -> Vec<String> {
        let width = self.terminal.columns();
        let height = self.terminal.rows();
        let mut lines = self.container.render(width);
        for overlay in &self.overlays {
            if overlay.hidden {
                continue;
            }
            let overlay_width = overlay
                .options
                .width
                .map(|value| value.resolve(width))
                .unwrap_or(width.min(80))
                .max(overlay.options.min_width.unwrap_or(0))
                .min(width);
            let mut overlay_lines = overlay.component.render(overlay_width);
            if let Some(max_height) = overlay
                .options
                .max_height
                .map(|value| value.resolve(height))
            {
                overlay_lines.truncate(max_height);
            }
            let overlay_lines = overlay_lines
                .into_iter()
                .map(|line| truncate_to_width_with(&line, overlay_width, "", true))
                .collect::<Vec<_>>();
            let (row, col) = resolve_overlay_position(
                &overlay.options,
                width,
                height,
                overlay_width,
                overlay_lines.len(),
            );
            composite_overlay(&mut lines, &overlay_lines, row, col, width);
        }
        lines
            .into_iter()
            .map(|line| truncate_to_width(&line.replace(CURSOR_MARKER, ""), width))
            .collect()
    }

    pub fn render_now(&mut self) -> anyhow::Result<()> {
        self.render_now_with_full_redraw(false)
    }

    pub fn render_now_with_full_redraw(&mut self, full_redraw: bool) -> anyhow::Result<()> {
        let width = self.terminal.columns();
        let height = self.terminal.rows();
        let lines = self.render_lines();
        let current_image_ids = extract_kitty_image_ids_from_lines(&lines);
        let resized = self.previous_width != 0
            && (self.previous_width != width
                || (self.previous_height != height && std::env::var("TERMUX_VERSION").is_err()));
        let shrunk = self.clear_on_shrink
            && self.max_lines_rendered > 0
            && lines.len() < self.max_lines_rendered;
        let full_redraw = full_redraw || resized || shrunk;
        let mut prefix = String::new();

        for image_id in &self.previous_kitty_image_ids {
            prefix.push_str(&delete_kitty_image(*image_id));
        }

        if full_redraw {
            self.full_redraws += 1;
            prefix.push_str("\u{1b}[2J\u{1b}[H");
        }

        self.previous_width = width;
        self.previous_height = height;
        self.max_lines_rendered = self.max_lines_rendered.max(lines.len());
        if full_redraw {
            self.max_lines_rendered = lines.len();
        }
        self.previous_kitty_image_ids = current_image_ids;
        let output = format!("{prefix}{}", lines.join("\r\n"));
        self.terminal.write(&output)
    }

    pub fn handle_input(&mut self, data: &str) {
        if let Some(id) = self.focused_overlay {
            if let Some(overlay) = self
                .overlays
                .iter_mut()
                .find(|overlay| overlay.id == id && !overlay.hidden)
            {
                overlay.component.handle_input(data);
                return;
            }
        }
        self.container.handle_input(data);
    }

    pub fn show_overlay<C>(&mut self, component: C, options: OverlayOptions) -> OverlayHandle
    where
        C: Component + 'static,
    {
        let id = self.next_overlay_id;
        self.next_overlay_id += 1;
        if !options.non_capturing {
            self.focused_overlay = Some(id);
        }
        self.overlays.push(Overlay {
            id,
            component: StdBox::new(component),
            options,
            hidden: false,
        });
        OverlayHandle { id }
    }

    pub fn hide_overlay(&mut self) -> Option<OverlayHandle> {
        let overlay = self.overlays.pop()?;
        if self.focused_overlay == Some(overlay.id) {
            self.focused_overlay = self.overlays.last().map(|overlay| overlay.id);
        }
        Some(OverlayHandle { id: overlay.id })
    }

    pub fn hide_overlay_handle(&mut self, handle: OverlayHandle) -> bool {
        if let Some(index) = self
            .overlays
            .iter()
            .position(|overlay| overlay.id == handle.id)
        {
            self.overlays.remove(index);
            if self.focused_overlay == Some(handle.id) {
                self.focused_overlay = self.overlays.last().map(|overlay| overlay.id);
            }
            true
        } else {
            false
        }
    }

    pub fn set_overlay_hidden(&mut self, handle: OverlayHandle, hidden: bool) -> bool {
        if let Some(overlay) = self
            .overlays
            .iter_mut()
            .find(|overlay| overlay.id == handle.id)
        {
            overlay.hidden = hidden;
            true
        } else {
            false
        }
    }

    pub fn is_overlay_hidden(&self, handle: OverlayHandle) -> bool {
        self.overlays
            .iter()
            .find(|overlay| overlay.id == handle.id)
            .is_none_or(|overlay| overlay.hidden)
    }

    pub fn focus_overlay(&mut self, handle: OverlayHandle) -> bool {
        if self.overlays.iter().any(|overlay| overlay.id == handle.id) {
            self.focused_overlay = Some(handle.id);
            true
        } else {
            false
        }
    }

    pub fn unfocus_overlay(&mut self, handle: OverlayHandle) -> bool {
        if self.focused_overlay == Some(handle.id) {
            self.focused_overlay = None;
            true
        } else {
            false
        }
    }

    pub fn is_overlay_focused(&self, handle: OverlayHandle) -> bool {
        self.focused_overlay == Some(handle.id)
    }

    pub fn has_overlay(&self) -> bool {
        self.overlays.iter().any(|overlay| !overlay.hidden)
    }
}

fn extract_kitty_image_ids_from_lines(lines: &[String]) -> BTreeSet<u32> {
    let mut ids = BTreeSet::new();
    for line in lines {
        let mut search_from = 0;
        while let Some(start) = line[search_from..].find("\u{1b}_G") {
            let sequence_start = search_from + start;
            let params_start = sequence_start + "\u{1b}_G".len();
            let Some(params_end_rel) = line[params_start..].find(';') else {
                break;
            };
            let params_end = params_start + params_end_rel;
            for param in line[params_start..params_end].split(',') {
                if let Some(value) = param.strip_prefix("i=") {
                    if let Ok(id) = value.parse::<u32>() {
                        if id > 0 {
                            ids.insert(id);
                        }
                    }
                }
            }
            search_from = params_end + 1;
        }
    }
    ids
}

fn resolve_overlay_position(
    options: &OverlayOptions,
    term_width: usize,
    term_height: usize,
    overlay_width: usize,
    overlay_height: usize,
) -> (usize, usize) {
    let margin = options.margin;
    let available_bottom = term_height.saturating_sub(margin.bottom);
    let max_row = available_bottom.saturating_sub(overlay_height);
    let available_right = term_width.saturating_sub(margin.right);
    let max_col = available_right.saturating_sub(overlay_width);

    let mut row = if let Some(row) = options.row {
        row.resolve(term_height)
    } else {
        match options.anchor {
            OverlayAnchor::TopLeft | OverlayAnchor::TopRight | OverlayAnchor::TopCenter => {
                margin.top
            }
            OverlayAnchor::BottomLeft
            | OverlayAnchor::BottomRight
            | OverlayAnchor::BottomCenter => max_row,
            _ => term_height.saturating_sub(overlay_height) / 2,
        }
    };

    let mut col = if let Some(col) = options.col {
        col.resolve(term_width)
    } else {
        match options.anchor {
            OverlayAnchor::TopLeft | OverlayAnchor::BottomLeft | OverlayAnchor::LeftCenter => {
                margin.left
            }
            OverlayAnchor::TopRight | OverlayAnchor::BottomRight | OverlayAnchor::RightCenter => {
                max_col
            }
            _ => term_width.saturating_sub(overlay_width) / 2,
        }
    };

    row = apply_offset(row, options.offset_y).clamp(margin.top, max_row.max(margin.top));
    col = apply_offset(col, options.offset_x).clamp(margin.left, max_col.max(margin.left));
    (row, col)
}

fn apply_offset(value: usize, offset: isize) -> usize {
    if offset.is_negative() {
        value.saturating_sub(offset.unsigned_abs())
    } else {
        value.saturating_add(offset as usize)
    }
}

fn composite_overlay(
    lines: &mut Vec<String>,
    overlay_lines: &[String],
    row: usize,
    col: usize,
    width: usize,
) {
    if overlay_lines.is_empty() || width == 0 {
        return;
    }
    if lines.len() < row + overlay_lines.len() {
        lines.resize(row + overlay_lines.len(), String::new());
    }
    for (offset, overlay_line) in overlay_lines.iter().enumerate() {
        let target_index = row + offset;
        let base = lines.get(target_index).cloned().unwrap_or_default();
        let overlay_width = visible_width(overlay_line).min(width.saturating_sub(col));
        let before = slice_by_column(&base, 0, col.min(width), false);
        let mut composed = before;
        composed.push_str(&" ".repeat(col.saturating_sub(visible_width(&composed))));
        composed.push_str(&truncate_to_width_with(
            overlay_line,
            overlay_width,
            "",
            true,
        ));
        let after_start = col.saturating_add(overlay_width).min(width);
        let after = slice_by_column(&base, after_start, width.saturating_sub(after_start), false);
        composed.push_str(&after);
        let composed_width = visible_width(&composed);
        if composed_width < width {
            composed.push_str(&" ".repeat(width - composed_width));
        }
        lines[target_index] = truncate_to_width_with(&composed, width, "", false);
    }
}

impl<T: Terminal> Component for TUI<T> {
    fn render(&self, _width: usize) -> Vec<String> {
        self.render_lines()
    }

    fn handle_input(&mut self, data: &str) {
        TUI::handle_input(self, data);
    }

    fn invalidate(&mut self) {
        self.container.invalidate();
        for overlay in &mut self.overlays {
            overlay.component.invalidate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    #[derive(Default)]
    struct TestTerminal {
        columns: usize,
        rows: usize,
        writes: String,
    }

    impl TestTerminal {
        fn new(columns: usize, rows: usize) -> Self {
            Self {
                columns,
                rows,
                writes: String::new(),
            }
        }
    }

    impl Terminal for TestTerminal {
        fn start(&mut self) -> Result<()> {
            Ok(())
        }
        fn stop(&mut self) -> Result<()> {
            Ok(())
        }
        fn write(&mut self, data: &str) -> Result<()> {
            self.writes.push_str(data);
            Ok(())
        }
        fn columns(&self) -> usize {
            self.columns
        }
        fn rows(&self) -> usize {
            self.rows
        }
    }

    struct StaticLines(Vec<String>);
    impl Component for StaticLines {
        fn render(&self, _width: usize) -> Vec<String> {
            self.0.clone()
        }
    }

    #[test]
    fn overlays_composite_at_explicit_position() {
        let terminal = TestTerminal::new(12, 6);
        let mut tui = TUI::new(terminal);
        tui.add_child(StaticLines(vec![
            "abcdefghijkl".into(),
            "mnopqrstuvwx".into(),
        ]));
        tui.show_overlay(
            StaticLines(vec!["OVR".into()]),
            OverlayOptions {
                row: Some(SizeValue::Columns(1)),
                col: Some(SizeValue::Columns(4)),
                width: Some(SizeValue::Columns(3)),
                ..OverlayOptions::default()
            },
        );
        let lines = tui.render_lines();
        assert_eq!(lines[0], "abcdefghijkl");
        assert_eq!(lines[1], "mnopOVRtuvwx");
    }

    #[test]
    fn overlays_respect_anchor_and_margin() {
        let terminal = TestTerminal::new(10, 5);
        let mut tui = TUI::new(terminal);
        tui.add_child(StaticLines(vec!["..........".into(); 5]));
        tui.show_overlay(
            StaticLines(vec!["XX".into()]),
            OverlayOptions {
                anchor: OverlayAnchor::BottomRight,
                margin: OverlayMargin::all(1),
                width: Some(SizeValue::Columns(2)),
                ..OverlayOptions::default()
            },
        );
        let lines = tui.render_lines();
        assert_eq!(lines[3], ".......XX.");
    }

    #[test]
    fn render_strips_cursor_marker_from_output() {
        let terminal = TestTerminal::new(10, 4);
        let mut tui = TUI::new(terminal);
        tui.add_child(StaticLines(vec![format!("ab{CURSOR_MARKER}cd")]));
        let lines = tui.render_lines();
        assert_eq!(lines[0], "abcd");
    }

    #[test]
    fn render_deletes_previous_kitty_images_before_redraw() {
        let terminal = TestTerminal::new(40, 6);
        let mut tui = TUI::new(terminal);
        tui.add_child(StaticLines(vec![
            "\u{1b}_Ga=T,f=100,q=2,i=42;AAAA\u{1b}\\".into(),
        ]));
        tui.request_render().unwrap();
        tui.request_render_full().unwrap();
        let writes = &tui.terminal.writes;
        let delete_index = writes
            .find("\u{1b}_Ga=d,d=I,i=42,q=2\u{1b}\\")
            .expect("delete image");
        let clear_index = writes.find("\u{1b}[2J\u{1b}[H").expect("clear screen");
        assert!(delete_index < clear_index);
        assert_eq!(tui.full_redraws(), 1);
    }

    #[test]
    fn render_full_redraws_on_resize_and_shrink() {
        let terminal = TestTerminal::new(20, 6);
        let mut tui = TUI::new(terminal);
        tui.set_clear_on_shrink(true);
        tui.add_child(StaticLines(vec![
            "line 1".into(),
            "line 2".into(),
            "line 3".into(),
        ]));
        tui.request_render().unwrap();
        assert_eq!(tui.full_redraws(), 0);

        tui.terminal.columns = 30;
        tui.request_render().unwrap();
        assert_eq!(tui.full_redraws(), 1);

        tui.clear();
        tui.add_child(StaticLines(vec!["line 1".into()]));
        tui.request_render().unwrap();
        assert_eq!(tui.full_redraws(), 2);
    }
}
