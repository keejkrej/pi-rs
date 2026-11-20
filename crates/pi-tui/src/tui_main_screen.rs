//! Port of packages/tui/src/tui-main-screen.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexSet;

use crate::terminal::Terminal;
use crate::terminal_colors::{TerminalColorScheme, TerminalColors};
use crate::tui::{
    Component, CursorPosition, OverlayHandle, OverlayOptions, QueryTerminalColorsOptions, TUI, TuiBase,
    TuiBaseOverrides, TuiInputListener, TuiMode, TuiMouseEvent, TuiMouseEventResult, TuiStopOptions,
};

const KITTY_SEQUENCE_PREFIX: &str = "\x1b_G";
const MAX_RENDER_WRITE_CHARS: i64 = 1024 * 1024;

/// Streams terminal output in 1 MiB chunks so a full render never forms one string large enough to exceed V8's limit.
///
/// `append()` fills the current chunk and flushes it when full. Oversized input is split at chunk boundaries, preserving
/// surrogate pairs so each write remains valid UTF-16. Callers append synchronized-output begin/end sequences themselves;
/// the final `flush()` writes any remainder, including the end sequence.
struct BoundedTerminalWriter {
    buffer: String,
    written_chars: i64,
    write: Box<dyn Fn(&str) + Send + Sync>,
}

fn utf16_len(s: &str) -> usize {
    if s.is_ascii() {
        s.len()
    } else {
        s.chars().map(char::len_utf16).sum()
    }
}

impl BoundedTerminalWriter {
    fn new(write: Box<dyn Fn(&str) + Send + Sync>) -> Self {
        Self {
            buffer: String::new(),
            written_chars: 0,
            write,
        }
    }

    /// Append terminal data, flushing full chunks as needed. Callers must call `flush()` after the final append.
    fn append(&mut self, value: &str) {
        let max_chars = MAX_RENDER_WRITE_CHARS as usize;
        let mut offset = 0usize;
        let mut buffer_chars = utf16_len(&self.buffer);
        while offset < value.len() {
            let capacity = max_chars.saturating_sub(buffer_chars);
            if capacity == 0 {
                self.flush();
                buffer_chars = 0;
                continue;
            }

            let rest = &value[offset..];
            let (end_rel, added_chars) = if rest.is_ascii() {
                let take = rest.len().min(capacity);
                (take, take)
            } else {
                let mut taken_bytes = 0usize;
                let mut taken_u16 = 0usize;
                for ch in rest.chars() {
                    let ch_u16 = ch.len_utf16();
                    if taken_u16 + ch_u16 > capacity {
                        break;
                    }
                    taken_u16 += ch_u16;
                    taken_bytes += ch.len_utf8();
                }
                (taken_bytes, taken_u16)
            };

            if end_rel == 0 {
                self.flush();
                buffer_chars = 0;
                continue;
            }

            self.buffer.push_str(&rest[..end_rel]);
            offset += end_rel;
            buffer_chars += added_chars;
            if buffer_chars >= max_chars {
                self.flush();
                buffer_chars = 0;
            }
        }
    }

    /// Write the current chunk, if any, and retain only its character count for debug output.
    fn flush(&mut self) {
        if self.buffer.is_empty() {
            return;
        }
        (self.write)(&self.buffer);
        self.written_chars += utf16_len(&self.buffer) as i64;
        self.buffer.clear();
    }

    fn length(&self) -> i64 {
        self.written_chars + utf16_len(&self.buffer) as i64
    }
}

struct KittyImageHeader {
    ids: Vec<i64>,
    rows: i64,
}

fn parse_kitty_image_header(line: &str) -> Option<KittyImageHeader> {
    let sequence_start = line.find(KITTY_SEQUENCE_PREFIX)?;
    let params_start = sequence_start + KITTY_SEQUENCE_PREFIX.len();
    let params_rel_end = line[params_start..].find(';')?;
    let params_str = &line[params_start..params_start + params_rel_end];

    let mut ids = Vec::new();
    let mut rows = 1i64;
    for param in params_str.split(',') {
        let mut parts = param.split('=');
        let Some(key) = parts.next() else {
            continue;
        };
        let Some(value) = parts.next() else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        let Ok(number_value) = value.trim().parse::<i64>() else {
            continue;
        };
        if number_value <= 0 || number_value > 0xffff_ffff {
            continue;
        }
        if key == "i" {
            ids.push(number_value);
        } else if key == "r" {
            rows = number_value;
        }
    }
    Some(KittyImageHeader { ids, rows })
}

fn extract_kitty_image_ids(line: &str) -> Vec<i64> {
    parse_kitty_image_header(line).map(|h| h.ids).unwrap_or_default()
}

fn extract_kitty_image_rows(line: &str) -> i64 {
    parse_kitty_image_header(line).map(|h| h.rows).unwrap_or(1)
}

fn is_termux_session() -> bool {
    pi_js::env::var("TERMUX_VERSION").is_some_and(|v| !v.is_empty())
}

/// Drop leading OSC 133 markers: the terminal reads `133;A` as a new shell prompt, which ends program status (OSC 7501).
fn strip_osc133_zone_prefix(mut line: &str) -> String {
    while let Some(rest) = line.strip_prefix("\x1b]133;") {
        let Some(kind) = rest.as_bytes().first().copied() else {
            break;
        };
        if !matches!(kind, b'A' | b'B' | b'C') {
            break;
        }
        let after_kind = &rest[1..];
        if let Some(after) = after_kind.strip_prefix('\x07') {
            line = after;
        } else if let Some(after) = after_kind.strip_prefix("\x1b\\") {
            line = after;
        } else {
            break;
        }
    }
    line.to_string()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TuiMainScreenRenderState {
    pub previous_lines: Vec<String>,
    pub previous_width: i64,
    pub previous_height: i64,
    pub cursor_row: i64,
    pub hardware_cursor_row: i64,
    pub max_lines_rendered: i64,
    pub previous_viewport_top: i64,
}

struct TuiMainScreenState {
    previous_lines: Vec<String>,
    previous_kitty_image_ids: IndexSet<i64>,
    previous_width: i64,
    previous_height: i64,
    cursor_row: i64,
    hardware_cursor_row: i64,
    max_lines_rendered: i64,
    previous_viewport_top: i64,
}

struct ChangedRange {
    first_changed: i64,
    last_changed: i64,
}

struct TuiMainScreenInner {
    self_weak: std::sync::Weak<TuiMainScreenInner>,
    base: TuiBase,
    state: Mutex<TuiMainScreenState>,
}

/// TUI implementation that renders into the terminal's main screen and scrollback.
///
/// PORT: `extends TuiBase` is a composed [`TuiBase`]. After [`TuiMainScreen::new`], call
/// [`TuiBase::bind_overrides`] with this screen (`Weak` inside the base breaks the cycle).
/// `mode` is `"regular"`.
#[derive(Clone)]
pub struct TuiMainScreen {
    inner: Arc<TuiMainScreenInner>,
}

impl TuiMainScreen {
    pub fn new(terminal: Arc<dyn Terminal>, show_hardware_cursor: Option<bool>, log_directory: Option<&str>) -> Self {
        let base = TuiBase::new(terminal, show_hardware_cursor, log_directory);
        let screen = Self {
            inner: Arc::new_cyclic(|weak| TuiMainScreenInner {
                self_weak: std::sync::Weak::clone(weak),
                base,
                state: Mutex::new(TuiMainScreenState {
                    previous_lines: Vec::new(),
                    previous_kitty_image_ids: IndexSet::new(),
                    previous_width: 0,
                    previous_height: 0,
                    cursor_row: 0,
                    hardware_cursor_row: 0,
                    max_lines_rendered: 0,
                    previous_viewport_top: 0,
                }),
            }),
        };
        let overrides: Arc<dyn TuiBaseOverrides> = screen.inner.clone();
        screen.base().bind_overrides(overrides);
        screen
    }

    fn base(&self) -> &TuiBase {
        &self.inner.base
    }

    pub fn mode(&self) -> TuiMode {
        TuiMode::Regular
    }

    pub fn capture_render_state(&self) -> TuiMainScreenRenderState {
        let state = self.inner.state.lock().unwrap();
        TuiMainScreenRenderState {
            previous_lines: state.previous_lines.clone(),
            previous_width: state.previous_width,
            previous_height: state.previous_height,
            cursor_row: state.cursor_row,
            hardware_cursor_row: state.hardware_cursor_row,
            max_lines_rendered: state.max_lines_rendered,
            previous_viewport_top: state.previous_viewport_top,
        }
    }

    pub fn restore_render_state(&self, render_state: TuiMainScreenRenderState) {
        let mut state = self.inner.state.lock().unwrap();
        state.previous_lines = render_state
            .previous_lines
            .into_iter()
            .map(|line| {
                if crate::terminal_image::is_image_line(&line) {
                    String::new()
                } else {
                    line
                }
            })
            .collect();
        state.previous_kitty_image_ids = IndexSet::new();
        state.previous_width = render_state.previous_width;
        state.previous_height = render_state.previous_height;
        state.cursor_row = render_state.cursor_row;
        state.hardware_cursor_row = render_state.hardware_cursor_row;
        state.max_lines_rendered = render_state.max_lines_rendered;
        state.previous_viewport_top = render_state.previous_viewport_top;
    }

    pub fn children(&self) -> Vec<Arc<dyn Component>> {
        self.base().children()
    }

    pub fn set_children(&self, children: Vec<Arc<dyn Component>>) {
        self.base().set_children(children);
    }

    pub fn terminal(&self) -> Arc<dyn Terminal> {
        self.base().terminal()
    }

    pub fn on_debug(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        self.base().on_debug()
    }

    pub fn set_on_debug(&self, on_debug: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.base().set_on_debug(on_debug);
    }

    pub fn full_redraws(&self) -> i64 {
        self.base().full_redraws()
    }

    pub fn add_child(&self, component: Arc<dyn Component>) {
        self.base().add_child(component);
    }

    pub fn remove_child(&self, component: &Arc<dyn Component>) {
        self.base().remove_child(component);
    }

    pub fn clear(&self) {
        self.base().clear();
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        self.base().render(width)
    }

    pub fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.base().handle_mouse(event)
    }

    pub fn invalidate(&self) {
        self.base().invalidate();
    }

    pub fn get_show_hardware_cursor(&self) -> bool {
        self.base().get_show_hardware_cursor()
    }

    pub fn set_show_hardware_cursor(&self, enabled: bool) {
        self.base().set_show_hardware_cursor(enabled);
    }

    pub fn get_clear_on_shrink(&self) -> bool {
        self.base().get_clear_on_shrink()
    }

    pub fn set_clear_on_shrink(&self, enabled: bool) {
        self.base().set_clear_on_shrink(enabled);
    }

    pub fn get_focused_component(&self) -> Option<Arc<dyn Component>> {
        self.base().get_focused_component()
    }

    pub fn set_focus(&self, component: Option<Arc<dyn Component>>) {
        self.base().set_focus(component);
    }

    pub fn show_overlay(&self, component: Arc<dyn Component>, options: Option<OverlayOptions>) -> OverlayHandle {
        self.base().show_overlay(component, options)
    }

    pub fn hide_overlay(&self) {
        self.base().hide_overlay();
    }

    pub fn has_overlay(&self) -> bool {
        self.base().has_overlay()
    }

    pub fn has_overlay_entries(&self) -> bool {
        self.base().has_overlay_entries()
    }

    pub fn start(&self) {
        self.base().start();
    }

    pub fn stop(&self, options: Option<TuiStopOptions>) {
        self.base().stop(options);
    }

    pub fn render_now(&self, force: Option<bool>) {
        self.base().render_now(force);
    }

    pub fn request_render(&self, force: Option<bool>) {
        self.base().request_render(force);
    }

    pub fn add_input_listener(&self, listener: TuiInputListener) -> pi_js::Unsubscribe {
        self.base().add_input_listener(listener)
    }

    pub fn remove_input_listener(&self, listener: &TuiInputListener) {
        self.base().remove_input_listener(listener);
    }

    pub fn on_terminal_color_scheme_change(
        &self,
        listener: Arc<dyn Fn(TerminalColorScheme) + Send + Sync>,
    ) -> pi_js::Unsubscribe {
        self.base().on_terminal_color_scheme_change(listener)
    }

    pub fn set_terminal_color_scheme_notifications(&self, enabled: bool) {
        self.base().set_terminal_color_scheme_notifications(enabled);
    }

    pub async fn query_terminal_colors(&self, options: QueryTerminalColorsOptions) -> TerminalColors {
        self.base().query_terminal_colors(options).await
    }

    fn collect_kitty_image_ids(&self, lines: &[String]) -> IndexSet<i64> {
        let mut ids = IndexSet::new();
        for line in lines {
            for id in extract_kitty_image_ids(line) {
                ids.insert(id);
            }
        }
        ids
    }

    fn delete_kitty_images(&self, ids: &IndexSet<i64>) -> String {
        let mut buffer = String::new();
        for &id in ids {
            buffer.push_str(&crate::terminal_image::delete_kitty_image(id));
        }
        buffer
    }

    fn get_kitty_image_reserved_rows(&self, lines: &[String], index: i64, max_index: Option<i64>) -> i64 {
        let Ok(idx_usize) = usize::try_from(index) else {
            return 1;
        };
        let line = lines.get(idx_usize).map(String::as_str).unwrap_or("");
        let rows = extract_kitty_image_rows(line);
        if rows <= 1 {
            return 1;
        }

        let max_idx = max_index.unwrap_or(lines.len() as i64 - 1);
        let max_rows = rows.min(max_idx - index + 1).min(lines.len() as i64 - index);
        let mut reserved_rows = 1i64;
        while reserved_rows < max_rows {
            let next_line = usize::try_from(index + reserved_rows)
                .ok()
                .and_then(|u| lines.get(u))
                .map(String::as_str)
                .unwrap_or("");
            if crate::terminal_image::is_image_line(next_line) || crate::utils::visible_width(next_line) > 0 {
                break;
            }
            reserved_rows += 1;
        }
        reserved_rows
    }

    fn expand_changed_range_for_kitty_images(
        &self,
        first_changed: i64,
        last_changed: i64,
        new_lines: &[String],
    ) -> ChangedRange {
        let prev_lines = self.inner.state.lock().unwrap().previous_lines.clone();
        let mut expanded_first_changed = first_changed;
        let mut expanded_last_changed = last_changed;

        let mut expand_for_lines = |lines: &[String]| {
            for (i, line) in lines.iter().enumerate() {
                if extract_kitty_image_ids(line).is_empty() {
                    continue;
                }
                let i_i64 = i as i64;
                let block_end = i_i64 + self.get_kitty_image_reserved_rows(lines, i_i64, None) - 1;
                if i_i64 >= first_changed || (i_i64 <= last_changed && block_end >= first_changed) {
                    expanded_first_changed = expanded_first_changed.min(i_i64);
                    expanded_last_changed = expanded_last_changed.max(block_end);
                }
            }
        };

        expand_for_lines(&prev_lines);
        expand_for_lines(new_lines);
        ChangedRange {
            first_changed: expanded_first_changed,
            last_changed: expanded_last_changed,
        }
    }

    fn delete_changed_kitty_images(&self, first_changed: i64, last_changed: i64) -> String {
        if first_changed < 0 || last_changed < first_changed {
            return String::new();
        }
        let prev_lines = self.inner.state.lock().unwrap().previous_lines.clone();
        let mut ids = IndexSet::new();
        let max_line = last_changed.min(prev_lines.len() as i64 - 1);
        for i in first_changed..=max_line {
            if let Ok(u) = usize::try_from(i) {
                for id in extract_kitty_image_ids(&prev_lines[u]) {
                    ids.insert(id);
                }
            }
        }
        self.delete_kitty_images(&ids)
    }

    /// Position the hardware cursor for IME candidate window.
    fn position_hardware_cursor(&self, cursor_pos: Option<CursorPosition>, total_lines: i64) {
        let terminal = self.terminal();
        let Some(cursor_pos) = cursor_pos.filter(|_| total_lines > 0) else {
            terminal.hide_cursor();
            return;
        };

        // Clamp cursor position to valid range
        let target_row = cursor_pos.row.max(0).min(total_lines - 1);
        let target_col = cursor_pos.col.max(0);

        // Move cursor from current position to target
        let hw_row = self.inner.state.lock().unwrap().hardware_cursor_row;
        let row_delta = target_row - hw_row;
        let mut buffer = String::new();
        if row_delta > 0 {
            buffer.push_str(&format!("\x1b[{row_delta}B")); // Move down
        } else if row_delta < 0 {
            buffer.push_str(&format!("\x1b[{}A", -row_delta)); // Move up
        }
        // Move to absolute column (1-indexed)
        buffer.push_str(&format!("\x1b[{}G", target_col + 1));

        if !buffer.is_empty() {
            terminal.write(&buffer);
        }

        self.inner.state.lock().unwrap().hardware_cursor_row = target_row;
        if self.get_show_hardware_cursor() {
            terminal.show_cursor();
        } else {
            terminal.hide_cursor();
        }
    }
}

impl TuiBaseOverrides for TuiMainScreenInner {
    fn mode(&self) -> TuiMode {
        TuiMode::Regular
    }

    fn do_render(&self) {
        if let Some(inner) = self.self_weak.upgrade() {
            TuiMainScreen { inner }.do_render();
        }
    }

    fn reset_render_state(&self) {
        if let Some(inner) = self.self_weak.upgrade() {
            TuiMainScreen { inner }.reset_render_state();
        }
    }

    fn before_terminal_stop(&self, options: &TuiStopOptions) {
        if let Some(inner) = self.self_weak.upgrade() {
            TuiMainScreen { inner }.before_terminal_stop(options);
        }
    }
}

impl TuiBaseOverrides for TuiMainScreen {
    fn mode(&self) -> TuiMode {
        TuiMainScreen::mode(self)
    }

    fn do_render(&self) {
        if self.base().stopped() {
            return;
        }
        let terminal = self.terminal();
        let width = terminal.columns();
        let height = terminal.rows();

        let (
            prev_lines,
            prev_kitty_image_ids,
            prev_width,
            prev_height,
            prev_cursor_row,
            mut hardware_cursor_row,
            prev_max_lines_rendered,
            prev_viewport_top_state,
        ) = {
            let state = self.inner.state.lock().unwrap();
            (
                state.previous_lines.clone(),
                state.previous_kitty_image_ids.clone(),
                state.previous_width,
                state.previous_height,
                state.cursor_row,
                state.hardware_cursor_row,
                state.max_lines_rendered,
                state.previous_viewport_top,
            )
        };

        let width_changed = prev_width != 0 && prev_width != width;
        let height_changed = prev_height != 0 && prev_height != height;
        let previous_buffer_length = if prev_height > 0 {
            prev_viewport_top_state + prev_height
        } else {
            height
        };
        let mut prev_viewport_top = if height_changed {
            0.max(previous_buffer_length - height)
        } else {
            prev_viewport_top_state
        };
        let mut viewport_top = prev_viewport_top;
        let compute_line_diff = |target_row: i64, hw_row: i64, p_vt: i64, vt: i64| -> i64 {
            let current_screen_row = hw_row - p_vt;
            let target_screen_row = target_row - vt;
            target_screen_row - current_screen_row
        };

        // Render all components to get new lines
        // Resolve fake cursors before compositing so overlays and line slicing see plain SGR codes
        // Drop OSC 133 markers: the terminal reads 133;A as a new shell prompt, which ends program status (OSC 7501)
        let render_width = usize::try_from(width.max(0)).unwrap_or(0);
        let mut new_lines = self.base().resolve_fake_cursors(
            self.render(render_width)
                .into_iter()
                .map(|line| strip_osc133_zone_prefix(&line))
                .collect(),
        );

        // Composite overlays into the rendered lines (before differential compare)
        if self.has_overlay_entries() {
            new_lines = self.base().composite_overlays(new_lines, width, height);
        }

        // Extract cursor position before applying line resets (marker must be found first)
        let cursor_pos = self.base().extract_cursor_position(&mut new_lines, height);

        let new_lines = self.base().apply_line_resets(new_lines);

        // Helper to clear scrollback and viewport and render all new lines
        let full_render = |clear: bool, new_lines: Vec<String>| {
            self.base().set_full_redraw_count(self.base().full_redraw_count() + 1);
            let term_write = Arc::clone(&terminal);
            let mut output = BoundedTerminalWriter::new(Box::new(move |data| term_write.write(data)));
            output.append("\x1b[?2026h"); // Begin synchronized output
            if clear {
                output.append(&self.delete_kitty_images(&prev_kitty_image_ids));
                output.append("\x1b[2J\x1b[H\x1b[3J"); // Clear screen, home, then clear scrollback
            }
            let mut i = 0usize;
            while i < new_lines.len() {
                if i > 0 {
                    output.append("\r\n");
                }
                let line = &new_lines[i];
                let is_image = crate::terminal_image::is_image_line(line);
                let image_reserved_rows = if is_image {
                    self.get_kitty_image_reserved_rows(&new_lines, i as i64, None)
                } else {
                    1
                };
                if image_reserved_rows > 1 && image_reserved_rows <= height {
                    for _ in 1..image_reserved_rows {
                        output.append("\r\n");
                    }
                    output.append(&format!("\x1b[{}A", image_reserved_rows - 1));
                    output.append(line);
                    output.append(&format!("\x1b[{}B", image_reserved_rows - 1));
                    i += image_reserved_rows as usize;
                    continue;
                }
                output.append(line);
                i += 1;
            }
            output.append("\x1b[?2026l"); // End synchronized output
            output.flush();

            let cursor_row = (new_lines.len() as i64 - 1).max(0);
            let buffer_length = height.max(new_lines.len() as i64);
            let max_lines = if clear {
                new_lines.len() as i64
            } else {
                prev_max_lines_rendered.max(new_lines.len() as i64)
            };
            {
                let mut state = self.inner.state.lock().unwrap();
                state.cursor_row = cursor_row;
                state.hardware_cursor_row = cursor_row;
                state.max_lines_rendered = max_lines;
                state.previous_viewport_top = 0.max(buffer_length - height);
            }
            self.position_hardware_cursor(cursor_pos, new_lines.len() as i64);
            let collected_ids = self.collect_kitty_image_ids(&new_lines);
            let mut state = self.inner.state.lock().unwrap();
            state.previous_lines = new_lines;
            state.previous_kitty_image_ids = collected_ids;
            state.previous_width = width;
            state.previous_height = height;
        };

        let redraw_log_dir = if pi_js::env::var("PI_TUI_DEBUG_REDRAW").as_deref() == Some("1") {
            self.base().log_directory().map(str::to_string)
        } else {
            None
        };
        let log_redraw = |reason: &str, new_len: usize| {
            let Some(ref dir) = redraw_log_dir else {
                return;
            };
            let log_path = std::path::PathBuf::from(dir).join("pi-tui-debug.log");
            let msg = format!(
                "[{}] fullRender: {} (prev={}, new={}, height={})\n",
                pi_js::time::iso_now(),
                reason,
                prev_lines.len(),
                new_len,
                height
            );
            if let Some(parent) = log_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&log_path) {
                let _ = std::io::Write::write_all(&mut file, msg.as_bytes());
            }
        };

        // First render - just output everything without clearing (assumes clean screen)
        if prev_lines.is_empty() && !width_changed && !height_changed {
            log_redraw("first render", new_lines.len());
            full_render(false, new_lines);
            return;
        }

        // Width changes always need a full re-render because wrapping changes.
        if width_changed {
            log_redraw(
                &format!("terminal width changed ({prev_width} -> {width})"),
                new_lines.len(),
            );
            full_render(true, new_lines);
            return;
        }

        // Height changes normally need a full re-render to keep the visible viewport aligned,
        // but Termux changes height when the software keyboard shows or hides.
        if height_changed && !is_termux_session() {
            log_redraw(
                &format!("terminal height changed ({prev_height} -> {height})"),
                new_lines.len(),
            );
            full_render(true, new_lines);
            return;
        }

        // Content shrunk below the working area and no overlays - re-render to clear empty rows
        if self.get_clear_on_shrink()
            && (new_lines.len() as i64) < prev_max_lines_rendered
            && !self.has_overlay_entries()
        {
            log_redraw(
                &format!("clearOnShrink (maxLinesRendered={prev_max_lines_rendered})"),
                new_lines.len(),
            );
            full_render(true, new_lines);
            return;
        }

        // Find first and last changed lines
        let mut first_changed: i64 = -1;
        let mut last_changed: i64 = -1;
        let max_lines = new_lines.len().max(prev_lines.len());
        for i in 0..max_lines {
            let old_line = prev_lines.get(i).map(String::as_str).unwrap_or("");
            let new_line = new_lines.get(i).map(String::as_str).unwrap_or("");
            if old_line != new_line {
                if first_changed == -1 {
                    first_changed = i as i64;
                }
                last_changed = i as i64;
            }
        }
        let appended_lines = new_lines.len() > prev_lines.len();
        if appended_lines {
            if first_changed == -1 {
                first_changed = prev_lines.len() as i64;
            }
            last_changed = new_lines.len() as i64 - 1;
        }
        if first_changed != -1 {
            let expanded = self.expand_changed_range_for_kitty_images(first_changed, last_changed, &new_lines);
            first_changed = expanded.first_changed;
            last_changed = expanded.last_changed;
        }
        let append_start = appended_lines && first_changed == prev_lines.len() as i64 && first_changed > 0;

        // No changes - but still need to update hardware cursor position if it moved
        if first_changed == -1 {
            self.position_hardware_cursor(cursor_pos, new_lines.len() as i64);
            let mut state = self.inner.state.lock().unwrap();
            state.previous_viewport_top = prev_viewport_top;
            state.previous_height = height;
            return;
        }

        // All changes are in deleted lines (nothing to render, just clear)
        if first_changed >= new_lines.len() as i64 {
            if prev_lines.len() > new_lines.len() {
                let target_row = (new_lines.len() as i64 - 1).max(0);
                if target_row < prev_viewport_top {
                    log_redraw(
                        &format!("deleted lines moved viewport up ({target_row} < {prev_viewport_top})"),
                        new_lines.len(),
                    );
                    full_render(true, new_lines);
                    return;
                }
                let extra_lines = (prev_lines.len() - new_lines.len()) as i64;
                if extra_lines > height {
                    log_redraw(
                        &format!("extraLines > height ({extra_lines} > {height})"),
                        new_lines.len(),
                    );
                    full_render(true, new_lines);
                    return;
                }
                let term_write = Arc::clone(&terminal);
                let mut output = BoundedTerminalWriter::new(Box::new(move |data| term_write.write(data)));
                output.append("\x1b[?2026h");
                output.append(&self.delete_changed_kitty_images(first_changed, last_changed));
                let line_diff = compute_line_diff(target_row, hardware_cursor_row, prev_viewport_top, viewport_top);
                if line_diff > 0 {
                    output.append(&format!("\x1b[{line_diff}B"));
                } else if line_diff < 0 {
                    output.append(&format!("\x1b[{}A", -line_diff));
                }
                output.append("\r");
                let clear_start_offset = if new_lines.is_empty() { 0 } else { 1 };
                if extra_lines > 0 && clear_start_offset > 0 {
                    output.append(&format!("\x1b[{clear_start_offset}B"));
                }
                for i in 0..extra_lines {
                    output.append("\r\x1b[2K");
                    if i < extra_lines - 1 {
                        output.append("\x1b[1B");
                    }
                }
                let move_back = 0.max(extra_lines - 1 + clear_start_offset);
                if move_back > 0 {
                    output.append(&format!("\x1b[{move_back}A"));
                }
                output.append("\x1b[?2026l");
                output.flush();
                let mut state = self.inner.state.lock().unwrap();
                state.cursor_row = target_row;
                state.hardware_cursor_row = target_row;
            }
            self.position_hardware_cursor(cursor_pos, new_lines.len() as i64);
            let collected_ids = self.collect_kitty_image_ids(&new_lines);
            let mut state = self.inner.state.lock().unwrap();
            state.previous_lines = new_lines;
            state.previous_kitty_image_ids = collected_ids;
            state.previous_width = width;
            state.previous_height = height;
            state.previous_viewport_top = prev_viewport_top;
            return;
        }

        // Differential rendering can only touch what was actually visible.
        if first_changed < prev_viewport_top {
            log_redraw(
                &format!("firstChanged < viewportTop ({first_changed} < {prev_viewport_top})"),
                new_lines.len(),
            );
            full_render(true, new_lines);
            return;
        }

        // Render from first changed line to end
        let term_write = Arc::clone(&terminal);
        let mut output = BoundedTerminalWriter::new(Box::new(move |data| term_write.write(data)));
        output.append("\x1b[?2026h"); // Begin synchronized output
        output.append(&self.delete_changed_kitty_images(first_changed, last_changed));
        let prev_viewport_bottom = prev_viewport_top + height - 1;
        let move_target_row = if append_start { first_changed - 1 } else { first_changed };
        if move_target_row > prev_viewport_bottom {
            let current_screen_row = 0.max((height - 1).min(hardware_cursor_row - prev_viewport_top));
            let move_to_bottom = height - 1 - current_screen_row;
            if move_to_bottom > 0 {
                output.append(&format!("\x1b[{move_to_bottom}B"));
            }
            let scroll = move_target_row - prev_viewport_bottom;
            output.append(&"\r\n".repeat(usize::try_from(scroll.max(0)).unwrap_or(0)));
            prev_viewport_top += scroll;
            viewport_top += scroll;
            hardware_cursor_row = move_target_row;
        }

        // Move cursor to first changed line (use hardwareCursorRow for actual position)
        let line_diff = compute_line_diff(move_target_row, hardware_cursor_row, prev_viewport_top, viewport_top);
        if line_diff > 0 {
            output.append(&format!("\x1b[{line_diff}B")); // Move down
        } else if line_diff < 0 {
            output.append(&format!("\x1b[{}A", -line_diff)); // Move up
        }

        output.append(if append_start { "\r\n" } else { "\r" }); // Move to column 0

        // Only render changed lines (firstChanged to lastChanged), not all lines to end
        let render_end = last_changed.min(new_lines.len() as i64 - 1);
        let mut i = first_changed;
        while i <= render_end {
            if i > first_changed {
                output.append("\r\n");
            }
            let line = &new_lines[i as usize];
            let is_image = crate::terminal_image::is_image_line(line);
            let image_reserved_rows = if is_image {
                self.get_kitty_image_reserved_rows(&new_lines, i, Some(render_end))
            } else {
                1
            };
            if image_reserved_rows > 1 {
                let image_start_screen_row = i - viewport_top;
                if image_start_screen_row < 0 || image_start_screen_row + image_reserved_rows > height {
                    log_redraw(
                        &format!(
                            "kitty image pre-clear would scroll ({image_start_screen_row} + {image_reserved_rows} > {height})"
                        ),
                        new_lines.len(),
                    );
                    full_render(true, new_lines);
                    return;
                }

                output.append("\x1b[2K");
                for _ in 1..image_reserved_rows {
                    output.append("\r\n\x1b[2K");
                }
                output.append(&format!("\x1b[{}A", image_reserved_rows - 1));
                output.append(line);
                output.append(&format!("\x1b[{}B", image_reserved_rows - 1));
                i += image_reserved_rows;
                continue;
            }

            output.append("\x1b[2K"); // Clear current line
            let line_width = crate::utils::visible_width(line);
            if !is_image && line_width > width {
                let crash_dir = self
                    .base()
                    .log_directory()
                    .map(ToString::to_string)
                    .unwrap_or_else(pi_js::env::tmp_dir);
                let crash_log_path = std::path::PathBuf::from(crash_dir).join("pi-tui-crash.log");
                let mut crash_lines = vec![
                    format!("Crash at {}", pi_js::time::iso_now()),
                    format!("Terminal width: {width}"),
                    format!("Line {i} visible width: {line_width}"),
                    String::new(),
                    "=== All rendered lines ===".to_string(),
                ];
                for (idx, l) in new_lines.iter().enumerate() {
                    crash_lines.push(format!("[{idx}] (w={}) {l}", crate::utils::visible_width(l)));
                }
                crash_lines.push(String::new());
                let crash_data = crash_lines.join("\n");
                if let Some(parent) = crash_log_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(&crash_log_path, crash_data);

                // Clean up terminal state before panicking
                self.stop(None);

                let error_msg = [
                    format!("Rendered line {i} exceeds terminal width ({line_width} > {width})."),
                    String::new(),
                    "This is likely caused by a custom TUI component not truncating its output.".to_string(),
                    "Use visibleWidth() to measure and truncateToWidth() to truncate lines.".to_string(),
                    String::new(),
                    format!("Debug log written to: {}", crash_log_path.display()),
                ]
                .join("\n");
                panic!("{error_msg}");
            }
            output.append(line);
            i += 1;
        }

        // Track where cursor ended up after rendering
        let mut final_cursor_row = render_end;

        // If we had more lines before, clear them and move cursor back
        if prev_lines.len() > new_lines.len() {
            if render_end < new_lines.len() as i64 - 1 {
                let move_down = new_lines.len() as i64 - 1 - render_end;
                output.append(&format!("\x1b[{move_down}B"));
                final_cursor_row = new_lines.len() as i64 - 1;
            }
            let extra_lines = (prev_lines.len() - new_lines.len()) as i64;
            for _ in new_lines.len()..prev_lines.len() {
                output.append("\r\n\x1b[2K");
            }
            output.append(&format!("\x1b[{extra_lines}A"));
        }

        output.append("\x1b[?2026l"); // End synchronized output

        if pi_js::env::var("PI_TUI_DEBUG").as_deref() == Some("1") {
            let debug_dir = std::path::Path::new("/tmp/tui");
            let _ = std::fs::create_dir_all(debug_dir);
            let now_ms = pi_js::time::now_ms();
            let debug_path = debug_dir.join(format!("render-{}-{}.log", now_ms, new_lines.len()));
            let debug_data = [
                format!("firstChanged: {first_changed}"),
                format!("viewportTop: {viewport_top}"),
                format!("cursorRow: {prev_cursor_row}"),
                format!("height: {height}"),
                format!("lineDiff: {line_diff}"),
                format!("hardwareCursorRow: {hardware_cursor_row}"),
                format!("renderEnd: {render_end}"),
                format!("finalCursorRow: {final_cursor_row}"),
                format!("cursorPos: {}", pi_js::json::stringify(&cursor_pos)),
                format!("newLines.length: {}", new_lines.len()),
                format!("previousLines.length: {}", prev_lines.len()),
                String::new(),
                "=== newLines ===".to_string(),
                pi_js::json::stringify_pretty(&new_lines, "  "),
                String::new(),
                "=== previousLines ===".to_string(),
                pi_js::json::stringify_pretty(&prev_lines, "  "),
                String::new(),
                "=== buffer ===".to_string(),
                format!("[{} chars written in bounded chunks]", output.length()),
            ]
            .join("\n");
            let _ = std::fs::write(&debug_path, debug_data);
        }

        output.flush();

        {
            let mut state = self.inner.state.lock().unwrap();
            state.cursor_row = (new_lines.len() as i64 - 1).max(0);
            state.hardware_cursor_row = final_cursor_row;
            state.max_lines_rendered = state.max_lines_rendered.max(new_lines.len() as i64);
            state.previous_viewport_top = prev_viewport_top.max(final_cursor_row - height + 1);
        }

        // Position hardware cursor for IME
        self.position_hardware_cursor(cursor_pos, new_lines.len() as i64);

        let collected_ids = self.collect_kitty_image_ids(&new_lines);
        let mut state = self.inner.state.lock().unwrap();
        state.previous_lines = new_lines;
        state.previous_kitty_image_ids = collected_ids;
        state.previous_width = width;
        state.previous_height = height;
    }

    fn reset_render_state(&self) {
        let mut state = self.inner.state.lock().unwrap();
        state.previous_lines.clear();
        state.previous_width = -1;
        state.previous_height = -1;
        state.cursor_row = 0;
        state.hardware_cursor_row = 0;
        state.max_lines_rendered = 0;
        state.previous_viewport_top = 0;
    }

    fn before_terminal_stop(&self, options: &TuiStopOptions) {
        if options.preserve_screen.unwrap_or(false) {
            return;
        }
        let (prev_lines_len, hardware_cursor_row) = {
            let state = self.inner.state.lock().unwrap();
            (state.previous_lines.len(), state.hardware_cursor_row)
        };
        if prev_lines_len == 0 {
            return;
        }
        let terminal = self.terminal();
        terminal.write(" ");
        let target_row = prev_lines_len as i64;
        let line_diff = target_row - hardware_cursor_row;
        if line_diff > 0 {
            terminal.write(&format!("\x1b[{line_diff}B"));
        } else if line_diff < 0 {
            terminal.write(&format!("\x1b[{}A", -line_diff));
        }
        terminal.write("\r\n");
    }
}

impl Component for TuiMainScreen {
    fn render(&self, width: usize) -> Vec<String> {
        TuiMainScreen::render(self, width)
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        TuiMainScreen::handle_mouse(self, event)
    }

    fn invalidate(&self) {
        TuiMainScreen::invalidate(self);
    }

    fn is_container_handle_mouse(&self) -> bool {
        true
    }

    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        Some(self.children())
    }
}

#[async_trait]
impl TUI for TuiMainScreen {
    fn mode(&self) -> TuiMode {
        TuiMainScreen::mode(self)
    }

    fn children(&self) -> Vec<Arc<dyn Component>> {
        TuiMainScreen::children(self)
    }

    fn set_children(&self, children: Vec<Arc<dyn Component>>) {
        TuiMainScreen::set_children(self, children);
    }

    fn terminal(&self) -> Arc<dyn Terminal> {
        TuiMainScreen::terminal(self)
    }

    fn on_debug(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        TuiMainScreen::on_debug(self)
    }

    fn set_on_debug(&self, on_debug: Option<Arc<dyn Fn() + Send + Sync>>) {
        TuiMainScreen::set_on_debug(self, on_debug);
    }

    fn full_redraws(&self) -> i64 {
        TuiMainScreen::full_redraws(self)
    }

    fn add_child(&self, component: Arc<dyn Component>) {
        TuiMainScreen::add_child(self, component);
    }

    fn remove_child(&self, component: &Arc<dyn Component>) {
        TuiMainScreen::remove_child(self, component);
    }

    fn clear(&self) {
        TuiMainScreen::clear(self);
    }

    fn get_show_hardware_cursor(&self) -> bool {
        TuiMainScreen::get_show_hardware_cursor(self)
    }

    fn set_show_hardware_cursor(&self, enabled: bool) {
        TuiMainScreen::set_show_hardware_cursor(self, enabled);
    }

    fn get_clear_on_shrink(&self) -> bool {
        TuiMainScreen::get_clear_on_shrink(self)
    }

    fn set_clear_on_shrink(&self, enabled: bool) {
        TuiMainScreen::set_clear_on_shrink(self, enabled);
    }

    fn set_focus(&self, component: Option<Arc<dyn Component>>) {
        TuiMainScreen::set_focus(self, component);
    }

    fn show_overlay(&self, component: Arc<dyn Component>, options: Option<OverlayOptions>) -> OverlayHandle {
        TuiMainScreen::show_overlay(self, component, options)
    }

    fn hide_overlay(&self) {
        TuiMainScreen::hide_overlay(self);
    }

    fn has_overlay(&self) -> bool {
        TuiMainScreen::has_overlay(self)
    }

    fn start(&self) {
        TuiMainScreen::start(self);
    }

    fn stop(&self, options: Option<TuiStopOptions>) {
        TuiMainScreen::stop(self, options);
    }

    fn render_now(&self, force: Option<bool>) {
        TuiMainScreen::render_now(self, force);
    }

    fn request_render(&self, force: Option<bool>) {
        TuiMainScreen::request_render(self, force);
    }

    fn add_input_listener(&self, listener: TuiInputListener) -> pi_js::Unsubscribe {
        TuiMainScreen::add_input_listener(self, listener)
    }

    fn remove_input_listener(&self, listener: &TuiInputListener) {
        TuiMainScreen::remove_input_listener(self, listener);
    }

    fn on_terminal_color_scheme_change(
        &self,
        listener: Arc<dyn Fn(TerminalColorScheme) + Send + Sync>,
    ) -> pi_js::Unsubscribe {
        TuiMainScreen::on_terminal_color_scheme_change(self, listener)
    }

    fn set_terminal_color_scheme_notifications(&self, enabled: bool) {
        TuiMainScreen::set_terminal_color_scheme_notifications(self, enabled);
    }

    async fn query_terminal_colors(&self, options: QueryTerminalColorsOptions) -> TerminalColors {
        TuiMainScreen::query_terminal_colors(self, options).await
    }
}
