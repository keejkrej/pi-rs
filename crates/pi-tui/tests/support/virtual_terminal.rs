//! Port of packages/tui/test/virtual-terminal.ts
//!
//! Pure-Rust VT100/xterm-compatible virtual screen buffer for headless TUI tests.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellColor {
    Default,
    Palette(u8),
    Rgb(u8, u8, u8),
}

impl CellColor {
    fn as_xterm_value(self) -> i64 {
        match self {
            Self::Default => -1,
            Self::Palette(idx) => i64::from(idx),
            Self::Rgb(r, g, b) => (i64::from(r) << 16) | (i64::from(g) << 8) | i64::from(b),
        }
    }

    fn mode(self) -> u8 {
        match self {
            Self::Default => 0,
            Self::Palette(idx) if idx < 16 => 1,
            Self::Palette(_) => 2,
            Self::Rgb(_, _, _) => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CellStyle {
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    inverse: bool,
    strikethrough: bool,
    fg: CellColor,
    bg: CellColor,
}

impl Default for CellStyle {
    fn default() -> Self {
        Self {
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            inverse: false,
            strikethrough: false,
            fg: CellColor::Default,
            bg: CellColor::Default,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VirtualCell {
    chars: String,
    width: usize,
    style: CellStyle,
}

impl VirtualCell {
    fn blank(style: CellStyle) -> Self {
        Self {
            chars: String::new(),
            width: 1,
            style,
        }
    }

    fn continuation(style: CellStyle) -> Self {
        Self {
            chars: String::new(),
            width: 0,
            style,
        }
    }

    pub fn get_chars(&self) -> &str {
        &self.chars
    }

    pub fn get_width(&self) -> usize {
        self.width
    }

    pub fn is_fg_default(&self) -> bool {
        self.style.fg == CellColor::Default
    }

    pub fn is_bg_default(&self) -> bool {
        self.style.bg == CellColor::Default
    }

    pub fn get_fg_color(&self) -> i64 {
        self.style.fg.as_xterm_value()
    }

    pub fn get_bg_color(&self) -> i64 {
        self.style.bg.as_xterm_value()
    }

    pub fn get_fg_color_mode(&self) -> u8 {
        self.style.fg.mode()
    }

    pub fn get_bg_color_mode(&self) -> u8 {
        self.style.bg.mode()
    }

    pub fn is_bold(&self) -> i32 {
        i32::from(self.style.bold)
    }

    pub fn is_dim(&self) -> i32 {
        i32::from(self.style.dim)
    }

    pub fn is_italic(&self) -> i32 {
        i32::from(self.style.italic)
    }

    pub fn is_underline(&self) -> i32 {
        i32::from(self.style.underline)
    }

    pub fn is_inverse(&self) -> i32 {
        i32::from(self.style.inverse)
    }

    pub fn is_strikethrough(&self) -> i32 {
        i32::from(self.style.strikethrough)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VirtualCursorPosition {
    pub x: usize,
    pub y: usize,
}

#[derive(Clone, Debug)]
struct ScreenBuffer {
    cols: usize,
    rows: usize,
    lines: Vec<Vec<VirtualCell>>,
    scrollback: Vec<Vec<VirtualCell>>,
    cursor_x: usize,
    cursor_y: usize,
    saved_x: usize,
    saved_y: usize,
    saved_style: CellStyle,
    pending_wrap: bool,
    scroll_top: usize,
    scroll_bottom: usize,
    style: CellStyle,
}

impl ScreenBuffer {
    fn new(cols: usize, rows: usize) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        Self {
            cols,
            rows,
            lines: vec![Self::empty_line(cols); rows],
            scrollback: Vec::new(),
            cursor_x: 0,
            cursor_y: 0,
            saved_x: 0,
            saved_y: 0,
            saved_style: CellStyle::default(),
            pending_wrap: false,
            scroll_top: 0,
            scroll_bottom: rows - 1,
            style: CellStyle::default(),
        }
    }

    fn empty_line(cols: usize) -> Vec<VirtualCell> {
        vec![VirtualCell::blank(CellStyle::default()); cols]
    }

    fn line_to_string(line: &[VirtualCell]) -> String {
        // Match xterm.js `translateToString(true)`: trim trailing unwritten/erased cells (`chars.is_empty()`),
        // while preserving explicitly written trailing space characters.
        let mut end = line.len();
        while end > 0 {
            let cell = &line[end - 1];
            if cell.width > 0 && !cell.chars.is_empty() {
                break;
            }
            end -= 1;
        }
        let mut out = String::with_capacity(end);
        for cell in &line[..end] {
            if cell.width == 0 {
                continue;
            }
            if cell.chars.is_empty() {
                out.push(' ');
            } else {
                out.push_str(&cell.chars);
            }
        }
        out
    }

    fn resize(&mut self, new_cols: usize, new_rows: usize) {
        let new_cols = new_cols.max(1);
        let new_rows = new_rows.max(1);
        for line in &mut self.lines {
            line.resize(new_cols, VirtualCell::blank(CellStyle::default()));
        }
        while self.lines.len() < new_rows {
            self.lines.push(Self::empty_line(new_cols));
        }
        if self.lines.len() > new_rows {
            let excess = self.lines.len() - new_rows;
            // Push overflowing top lines up into scrollback if needed or crop bottom empty rows
            let mut trimmed_bottom = 0;
            while trimmed_bottom < excess && self.lines.last().is_some_and(|l| Self::line_to_string(l).is_empty()) {
                self.lines.pop();
                trimmed_bottom += 1;
            }
            let remaining = excess - trimmed_bottom;
            if remaining > 0 {
                for line in self.lines.drain(..remaining) {
                    self.scrollback.push(line);
                }
            }
        }
        self.cols = new_cols;
        self.rows = new_rows;
        self.scroll_top = 0;
        self.scroll_bottom = new_rows - 1;
        self.cursor_x = self.cursor_x.min(new_cols - 1);
        self.cursor_y = self.cursor_y.min(new_rows - 1);
        self.pending_wrap = false;
    }

    fn scroll_up(&mut self, count: usize, record_scrollback: bool) {
        let top = self.scroll_top.min(self.rows - 1);
        let bottom = self.scroll_bottom.min(self.rows - 1);
        if top > bottom {
            return;
        }
        let region_len = bottom - top + 1;
        let n = count.min(region_len);
        for _ in 0..n {
            let removed = self.lines.remove(top);
            if record_scrollback && top == 0 && bottom == self.rows - 1 {
                self.scrollback.push(removed);
            }
            self.lines.insert(bottom, Self::empty_line(self.cols));
        }
    }

    fn scroll_down(&mut self, count: usize) {
        let top = self.scroll_top.min(self.rows - 1);
        let bottom = self.scroll_bottom.min(self.rows - 1);
        if top > bottom {
            return;
        }
        let region_len = bottom - top + 1;
        let n = count.min(region_len);
        for _ in 0..n {
            self.lines.remove(bottom);
            self.lines.insert(top, Self::empty_line(self.cols));
        }
    }

    fn line_feed(&mut self, record_scrollback: bool) {
        self.pending_wrap = false;
        if self.cursor_y == self.scroll_bottom {
            self.scroll_up(1, record_scrollback);
        } else if self.cursor_y + 1 < self.rows {
            self.cursor_y += 1;
        }
    }

    fn reverse_index(&mut self) {
        self.pending_wrap = false;
        if self.cursor_y == self.scroll_top {
            self.scroll_down(1);
        } else if self.cursor_y > 0 {
            self.cursor_y -= 1;
        }
    }

    fn clear_cell(&mut self, row: usize, col: usize) {
        if row >= self.rows || col >= self.cols {
            return;
        }
        // If clearing the left half of a wide char, clear continuation too.
        if self.lines[row][col].width == 2 && col + 1 < self.cols {
            self.lines[row][col + 1] = VirtualCell::blank(CellStyle::default());
        } else if self.lines[row][col].width == 0 && col > 0 && self.lines[row][col - 1].width == 2 {
            self.lines[row][col - 1] = VirtualCell::blank(CellStyle::default());
        }
        self.lines[row][col] = VirtualCell::blank(self.style);
    }

    fn erase_in_line(&mut self, mode: i64) {
        let row = self.cursor_y;
        match mode {
            0 => {
                for col in self.cursor_x..self.cols {
                    self.clear_cell(row, col);
                }
            }
            1 => {
                let end = (self.cursor_x + 1).min(self.cols);
                for col in 0..end {
                    self.clear_cell(row, col);
                }
            }
            2 => {
                for col in 0..self.cols {
                    self.clear_cell(row, col);
                }
            }
            _ => {}
        }
    }

    fn erase_in_display(&mut self, mode: i64) {
        match mode {
            0 => {
                self.erase_in_line(0);
                for row in (self.cursor_y + 1)..self.rows {
                    self.lines[row] = vec![VirtualCell::blank(self.style); self.cols];
                }
            }
            1 => {
                for row in 0..self.cursor_y {
                    self.lines[row] = vec![VirtualCell::blank(self.style); self.cols];
                }
                self.erase_in_line(1);
            }
            2 => {
                for row in 0..self.rows {
                    self.lines[row] = vec![VirtualCell::blank(self.style); self.cols];
                }
            }
            3 => {
                self.scrollback.clear();
            }
            _ => {}
        }
    }

    fn put_char(&mut self, ch: char, record_scrollback: bool) {
        let w = char_display_width(ch);
        if w == 0 {
            // Combining / zero-width character: attach to previous cell if any.
            let target = if self.pending_wrap {
                Some((self.cursor_y, self.cursor_x))
            } else if self.cursor_x > 0 {
                let mut prev = self.cursor_x - 1;
                if self.lines[self.cursor_y][prev].width == 0 && prev > 0 {
                    prev -= 1;
                }
                Some((self.cursor_y, prev))
            } else {
                None
            };
            if let Some((r, c)) = target {
                if self.lines[r][c].chars.is_empty() {
                    self.lines[r][c].chars.push(' ');
                }
                self.lines[r][c].chars.push(ch);
            }
            return;
        }

        if self.pending_wrap || (w == 2 && self.cursor_x + 1 >= self.cols && self.cursor_x == self.cols - 1) {
            self.pending_wrap = false;
            self.cursor_x = 0;
            self.line_feed(record_scrollback);
        }

        let row = self.cursor_y;
        let col = self.cursor_x;
        if col >= self.cols || row >= self.rows {
            return;
        }

        // Clear any wide glyph we might be overwriting partially.
        if self.lines[row][col].width == 0 && col > 0 && self.lines[row][col - 1].width == 2 {
            self.lines[row][col - 1] = VirtualCell::blank(self.style);
        }
        if self.lines[row][col].width == 2 && col + 1 < self.cols {
            self.lines[row][col + 1] = VirtualCell::blank(self.style);
        }

        if w == 2 && col + 1 < self.cols {
            if self.lines[row][col + 1].width == 2 && col + 2 < self.cols {
                self.lines[row][col + 2] = VirtualCell::blank(self.style);
            }
            let mut s = String::new();
            s.push(ch);
            self.lines[row][col] = VirtualCell {
                chars: s,
                width: 2,
                style: self.style,
            };
            self.lines[row][col + 1] = VirtualCell::continuation(self.style);
            if col + 2 >= self.cols {
                self.cursor_x = self.cols - 1;
                self.pending_wrap = true;
            } else {
                self.cursor_x = col + 2;
            }
        } else {
            let mut s = String::new();
            s.push(ch);
            self.lines[row][col] = VirtualCell {
                chars: s,
                width: 1,
                style: self.style,
            };
            if col + 1 >= self.cols {
                self.cursor_x = self.cols - 1;
                self.pending_wrap = true;
            } else {
                self.cursor_x = col + 1;
            }
        }
    }
}

#[derive(Clone, Debug)]
enum ParseState {
    Ground,
    Escape,
    EscapeIntermediate,
    Csi(String),
    Osc(String, bool),
    Apc(bool),
    Dcs(bool),
    StringConsume(bool),
}

struct VirtualTerminalInner {
    cols: usize,
    rows: usize,
    primary: ScreenBuffer,
    alternate: ScreenBuffer,
    using_alternate: bool,
    parse_state: ParseState,
    input_handler: Option<TerminalInputHandler>,
    resize_handler: Option<TerminalResizeHandler>,
    writes: Vec<String>,
    raw_output: String,
    cursor_visible: bool,
    title: String,
}

impl VirtualTerminalInner {
    fn new(cols: usize, rows: usize) -> Self {
        Self {
            cols,
            rows,
            primary: ScreenBuffer::new(cols, rows),
            alternate: ScreenBuffer::new(cols, rows),
            using_alternate: false,
            parse_state: ParseState::Ground,
            input_handler: None,
            resize_handler: None,
            writes: Vec::new(),
            raw_output: String::new(),
            cursor_visible: true,
            title: String::new(),
        }
    }

    fn active(&self) -> &ScreenBuffer {
        if self.using_alternate {
            &self.alternate
        } else {
            &self.primary
        }
    }

    fn active_mut(&mut self) -> &mut ScreenBuffer {
        if self.using_alternate {
            &mut self.alternate
        } else {
            &mut self.primary
        }
    }

    fn reset(&mut self) {
        self.primary = ScreenBuffer::new(self.cols, self.rows);
        self.alternate = ScreenBuffer::new(self.cols, self.rows);
        self.using_alternate = false;
        self.parse_state = ParseState::Ground;
        self.cursor_visible = true;
        self.title.clear();
    }

    fn clear_viewport(&mut self) {
        let cols = self.cols;
        let rows = self.rows;
        let buf = self.active_mut();
        buf.lines = vec![ScreenBuffer::empty_line(cols); rows];
        buf.cursor_x = 0;
        buf.cursor_y = 0;
        buf.pending_wrap = false;
    }

    fn process_str(&mut self, data: &str) {
        self.writes.push(data.to_string());
        self.raw_output.push_str(data);
        for ch in data.chars() {
            self.process_char(ch);
        }
    }

    fn process_char(&mut self, ch: char) {
        let state = std::mem::replace(&mut self.parse_state, ParseState::Ground);
        match state {
            ParseState::Ground => match ch {
                '\x1b' => self.parse_state = ParseState::Escape,
                '\r' => {
                    let buf = self.active_mut();
                    buf.cursor_x = 0;
                    buf.pending_wrap = false;
                }
                '\n' | '\x0b' | '\x0c' => {
                    let record = !self.using_alternate;
                    self.active_mut().line_feed(record);
                }
                '\x08' => {
                    let buf = self.active_mut();
                    buf.cursor_x = buf.cursor_x.saturating_sub(1);
                    buf.pending_wrap = false;
                }
                '\t' => {
                    let buf = self.active_mut();
                    let next_tab = ((buf.cursor_x / 8) + 1) * 8;
                    buf.cursor_x = next_tab.min(buf.cols - 1);
                    buf.pending_wrap = false;
                }
                c if (c as u32) < 0x20 || c as u32 == 0x7f => {}
                c => {
                    let record = !self.using_alternate;
                    self.active_mut().put_char(c, record);
                }
            },
            ParseState::Escape => match ch {
                '[' => self.parse_state = ParseState::Csi(String::new()),
                ']' => self.parse_state = ParseState::Osc(String::new(), false),
                '_' => self.parse_state = ParseState::Apc(false),
                'P' => self.parse_state = ParseState::Dcs(false),
                'X' | '^' => self.parse_state = ParseState::StringConsume(false),
                '(' | ')' | '*' | '+' | '-' | '.' | '/' | '#' | ' ' => {
                    self.parse_state = ParseState::EscapeIntermediate;
                }
                '7' => {
                    let buf = self.active_mut();
                    buf.saved_x = buf.cursor_x;
                    buf.saved_y = buf.cursor_y;
                    buf.saved_style = buf.style;
                }
                '8' => {
                    let buf = self.active_mut();
                    buf.cursor_x = buf.saved_x.min(buf.cols - 1);
                    buf.cursor_y = buf.saved_y.min(buf.rows - 1);
                    buf.style = buf.saved_style;
                    buf.pending_wrap = false;
                }
                'D' => {
                    let record = !self.using_alternate;
                    self.active_mut().line_feed(record);
                }
                'E' => {
                    let record = !self.using_alternate;
                    let buf = self.active_mut();
                    buf.cursor_x = 0;
                    buf.line_feed(record);
                }
                'M' => {
                    self.active_mut().reverse_index();
                }
                'c' => {
                    self.reset();
                }
                _ => {}
            },
            ParseState::EscapeIntermediate => {
                if (' '..='/').contains(&ch) {
                    self.parse_state = ParseState::EscapeIntermediate;
                }
            }
            ParseState::Csi(mut buf) => {
                if ('@'..='~').contains(&ch) {
                    self.dispatch_csi(&buf, ch);
                } else if ch == '\x1b' {
                    self.parse_state = ParseState::Escape;
                } else {
                    buf.push(ch);
                    self.parse_state = ParseState::Csi(buf);
                }
            }
            ParseState::Osc(mut buf, prev_esc) => {
                if ch == '\x07' {
                    self.dispatch_osc(&buf);
                } else if prev_esc && ch == '\\' {
                    self.dispatch_osc(&buf);
                } else if ch == '\x1b' {
                    self.parse_state = ParseState::Osc(buf, true);
                } else {
                    if prev_esc {
                        buf.push('\x1b');
                    }
                    buf.push(ch);
                    self.parse_state = ParseState::Osc(buf, false);
                }
            }
            ParseState::Apc(prev_esc) => {
                if ch == '\x07' || (prev_esc && ch == '\\') {
                    // Done
                } else if ch == '\x1b' {
                    self.parse_state = ParseState::Apc(true);
                } else {
                    self.parse_state = ParseState::Apc(false);
                }
            }
            ParseState::Dcs(prev_esc) => {
                if prev_esc && ch == '\\' {
                    // Done
                } else if ch == '\x1b' {
                    self.parse_state = ParseState::Dcs(true);
                } else {
                    self.parse_state = ParseState::Dcs(false);
                }
            }
            ParseState::StringConsume(prev_esc) => {
                if ch == '\x07' || (prev_esc && ch == '\\') {
                    // Done
                } else if ch == '\x1b' {
                    self.parse_state = ParseState::StringConsume(true);
                } else {
                    self.parse_state = ParseState::StringConsume(false);
                }
            }
        }
    }

    fn dispatch_osc(&mut self, payload: &str) {
        if let Some((code, rest)) = payload.split_once(';') {
            if matches!(code, "0" | "2") {
                self.title = rest.to_string();
            }
        }
    }

    fn dispatch_csi(&mut self, params_raw: &str, final_ch: char) {
        if let Some(priv_params) = params_raw.strip_prefix('?') {
            for p in priv_params.split(';') {
                let code = p.parse::<i64>().unwrap_or(-1);
                match (code, final_ch) {
                    (25, 'h') => self.cursor_visible = true,
                    (25, 'l') => self.cursor_visible = false,
                    (1049, 'h') => {
                        self.primary.saved_x = self.primary.cursor_x;
                        self.primary.saved_y = self.primary.cursor_y;
                        self.primary.saved_style = self.primary.style;
                        self.alternate = ScreenBuffer::new(self.cols, self.rows);
                        self.using_alternate = true;
                    }
                    (1049, 'l') => {
                        self.using_alternate = false;
                        self.primary.cursor_x = self.primary.saved_x.min(self.cols - 1);
                        self.primary.cursor_y = self.primary.saved_y.min(self.rows - 1);
                        self.primary.style = self.primary.saved_style;
                        self.primary.pending_wrap = false;
                    }
                    (47 | 1047, 'h') => {
                        if code == 1047 {
                            self.alternate = ScreenBuffer::new(self.cols, self.rows);
                        }
                        self.using_alternate = true;
                    }
                    (47 | 1047, 'l') => {
                        self.using_alternate = false;
                    }
                    _ => {}
                }
            }
            return;
        }

        if params_raw.starts_with('>') || params_raw.starts_with('<') || params_raw.starts_with('=') {
            return;
        }

        let clean = params_raw.trim_end_matches(|c: char| !c.is_ascii_digit() && c != ';' && c != ':');
        let num = |idx: usize, default: usize| -> usize {
            clean
                .split(';')
                .nth(idx)
                .and_then(|s| s.split(':').next())
                .filter(|s| !s.is_empty())
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(default)
        };

        let record = !self.using_alternate;
        match final_ch {
            'A' => {
                let n = num(0, 1).max(1);
                let buf = self.active_mut();
                let limit = if buf.cursor_y >= buf.scroll_top {
                    buf.scroll_top
                } else {
                    0
                };
                buf.cursor_y = buf.cursor_y.saturating_sub(n).max(limit);
                buf.pending_wrap = false;
            }
            'B' => {
                let n = num(0, 1).max(1);
                let buf = self.active_mut();
                let limit = if buf.cursor_y <= buf.scroll_bottom {
                    buf.scroll_bottom
                } else {
                    buf.rows - 1
                };
                buf.cursor_y = (buf.cursor_y + n).min(limit);
                buf.pending_wrap = false;
            }
            'C' => {
                let n = num(0, 1).max(1);
                let buf = self.active_mut();
                buf.cursor_x = (buf.cursor_x + n).min(buf.cols - 1);
                buf.pending_wrap = false;
            }
            'D' => {
                let n = num(0, 1).max(1);
                let buf = self.active_mut();
                buf.cursor_x = buf.cursor_x.saturating_sub(n);
                buf.pending_wrap = false;
            }
            'E' => {
                let n = num(0, 1).max(1);
                let buf = self.active_mut();
                buf.cursor_y = (buf.cursor_y + n).min(buf.rows - 1);
                buf.cursor_x = 0;
                buf.pending_wrap = false;
            }
            'F' => {
                let n = num(0, 1).max(1);
                let buf = self.active_mut();
                buf.cursor_y = buf.cursor_y.saturating_sub(n);
                buf.cursor_x = 0;
                buf.pending_wrap = false;
            }
            'G' | '`' => {
                let col = num(0, 1).max(1) - 1;
                let buf = self.active_mut();
                buf.cursor_x = col.min(buf.cols - 1);
                buf.pending_wrap = false;
            }
            'd' => {
                let row = num(0, 1).max(1) - 1;
                let buf = self.active_mut();
                buf.cursor_y = row.min(buf.rows - 1);
                buf.pending_wrap = false;
            }
            'H' | 'f' => {
                let row = num(0, 1).max(1) - 1;
                let col = num(1, 1).max(1) - 1;
                let buf = self.active_mut();
                buf.cursor_y = row.min(buf.rows - 1);
                buf.cursor_x = col.min(buf.cols - 1);
                buf.pending_wrap = false;
            }
            'J' => {
                let mode = num(0, 0) as i64;
                let buf = self.active_mut();
                buf.erase_in_display(mode);
            }
            'K' => {
                let mode = num(0, 0) as i64;
                let buf = self.active_mut();
                buf.erase_in_line(mode);
            }
            'L' => {
                let n = num(0, 1).max(1);
                let buf = self.active_mut();
                if buf.cursor_y >= buf.scroll_top && buf.cursor_y <= buf.scroll_bottom {
                    let bottom = buf.scroll_bottom;
                    let cy = buf.cursor_y;
                    let count = n.min(bottom - cy + 1);
                    for _ in 0..count {
                        buf.lines.remove(bottom);
                        buf.lines.insert(cy, ScreenBuffer::empty_line(buf.cols));
                    }
                    buf.cursor_x = 0;
                }
                buf.pending_wrap = false;
            }
            'M' => {
                let n = num(0, 1).max(1);
                let buf = self.active_mut();
                if buf.cursor_y >= buf.scroll_top && buf.cursor_y <= buf.scroll_bottom {
                    let bottom = buf.scroll_bottom;
                    let cy = buf.cursor_y;
                    let count = n.min(bottom - cy + 1);
                    for _ in 0..count {
                        buf.lines.remove(cy);
                        buf.lines.insert(bottom, ScreenBuffer::empty_line(buf.cols));
                    }
                    buf.cursor_x = 0;
                }
                buf.pending_wrap = false;
            }
            'S' => {
                let n = num(0, 1).max(1);
                self.active_mut().scroll_up(n, record);
            }
            'T' => {
                let n = num(0, 1).max(1);
                self.active_mut().scroll_down(n);
            }
            'r' => {
                let buf = self.active_mut();
                let top = num(0, 1).max(1) - 1;
                let bottom = num(1, buf.rows).max(1) - 1;
                if top < bottom && bottom < buf.rows {
                    buf.scroll_top = top;
                    buf.scroll_bottom = bottom;
                } else {
                    buf.scroll_top = 0;
                    buf.scroll_bottom = buf.rows - 1;
                }
                buf.cursor_x = 0;
                buf.cursor_y = 0;
                buf.pending_wrap = false;
            }
            's' => {
                let buf = self.active_mut();
                buf.saved_x = buf.cursor_x;
                buf.saved_y = buf.cursor_y;
                buf.saved_style = buf.style;
            }
            'u' => {
                let buf = self.active_mut();
                buf.cursor_x = buf.saved_x.min(buf.cols - 1);
                buf.cursor_y = buf.saved_y.min(buf.rows - 1);
                buf.style = buf.saved_style;
                buf.pending_wrap = false;
            }
            'm' => {
                self.apply_sgr(params_raw);
            }
            _ => {}
        }
    }

    fn apply_sgr(&mut self, params_raw: &str) {
        let style = &mut self.active_mut().style;
        if params_raw.is_empty() {
            *style = CellStyle::default();
            return;
        }
        let tokens: Vec<&str> = params_raw.split(';').collect();
        let mut i = 0;
        while i < tokens.len() {
            let tok = tokens[i];
            if tok.contains(':') {
                let sub: Vec<u16> = tok.split(':').map(|s| s.parse::<u16>().unwrap_or(0)).collect();
                match sub.as_slice() {
                    [4, 0, ..] => style.underline = false,
                    [4, _, ..] => style.underline = true,
                    [38, 5, idx, ..] => style.fg = CellColor::Palette((*idx).min(255) as u8),
                    [38, 2, _, r, g, b, ..] | [38, 2, r, g, b] => {
                        style.fg = CellColor::Rgb((*r).min(255) as u8, (*g).min(255) as u8, (*b).min(255) as u8);
                    }
                    [48, 5, idx, ..] => style.bg = CellColor::Palette((*idx).min(255) as u8),
                    [48, 2, _, r, g, b, ..] | [48, 2, r, g, b] => {
                        style.bg = CellColor::Rgb((*r).min(255) as u8, (*g).min(255) as u8, (*b).min(255) as u8);
                    }
                    _ => {}
                }
                i += 1;
                continue;
            }

            let code = if tok.is_empty() {
                0
            } else {
                tok.parse::<i64>().unwrap_or(0)
            };
            match code {
                0 => *style = CellStyle::default(),
                1 => style.bold = true,
                2 => style.dim = true,
                3 => style.italic = true,
                4 => style.underline = true,
                7 => style.inverse = true,
                9 => style.strikethrough = true,
                21 | 22 => {
                    style.bold = false;
                    style.dim = false;
                }
                23 => style.italic = false,
                24 => style.underline = false,
                27 => style.inverse = false,
                29 => style.strikethrough = false,
                30..=37 => style.fg = CellColor::Palette((code - 30) as u8),
                38 => {
                    if i + 2 < tokens.len() && tokens[i + 1] == "5" {
                        let idx = tokens[i + 2].parse::<u8>().unwrap_or(0);
                        style.fg = CellColor::Palette(idx);
                        i += 2;
                    } else if i + 4 < tokens.len() && tokens[i + 1] == "2" {
                        let r = tokens[i + 2].parse::<u8>().unwrap_or(0);
                        let g = tokens[i + 3].parse::<u8>().unwrap_or(0);
                        let b = tokens[i + 4].parse::<u8>().unwrap_or(0);
                        style.fg = CellColor::Rgb(r, g, b);
                        i += 4;
                    }
                }
                39 => style.fg = CellColor::Default,
                40..=47 => style.bg = CellColor::Palette((code - 40) as u8),
                48 => {
                    if i + 2 < tokens.len() && tokens[i + 1] == "5" {
                        let idx = tokens[i + 2].parse::<u8>().unwrap_or(0);
                        style.bg = CellColor::Palette(idx);
                        i += 2;
                    } else if i + 4 < tokens.len() && tokens[i + 1] == "2" {
                        let r = tokens[i + 2].parse::<u8>().unwrap_or(0);
                        let g = tokens[i + 3].parse::<u8>().unwrap_or(0);
                        let b = tokens[i + 4].parse::<u8>().unwrap_or(0);
                        style.bg = CellColor::Rgb(r, g, b);
                        i += 4;
                    }
                }
                49 => style.bg = CellColor::Default,
                58 => {
                    if i + 2 < tokens.len() && tokens[i + 1] == "5" {
                        i += 2;
                    } else if i + 4 < tokens.len() && tokens[i + 1] == "2" {
                        i += 4;
                    }
                }
                90..=97 => style.fg = CellColor::Palette((code - 90 + 8) as u8),
                100..=107 => style.bg = CellColor::Palette((code - 100 + 8) as u8),
                _ => {}
            }
            i += 1;
        }
    }
}

#[derive(Clone)]
pub struct VirtualTerminal {
    inner: Arc<Mutex<VirtualTerminalInner>>,
}

impl Default for VirtualTerminal {
    fn default() -> Self {
        Self::new(80, 24)
    }
}

impl VirtualTerminal {
    pub fn new(columns: i64, rows: i64) -> Self {
        let cols = usize::try_from(columns.max(1)).unwrap_or(80);
        let rows = usize::try_from(rows.max(1)).unwrap_or(24);
        Self {
            inner: Arc::new(Mutex::new(VirtualTerminalInner::new(cols, rows))),
        }
    }

    pub fn send_input(&self, data: &str) {
        let handler = self.inner.lock().ok().and_then(|g| g.input_handler.clone());
        if let Some(cb) = handler {
            cb(data);
        }
    }

    pub fn resize(&self, columns: i64, rows: i64) {
        let new_cols = usize::try_from(columns.max(1)).unwrap_or(1);
        let new_rows = usize::try_from(rows.max(1)).unwrap_or(1);
        let handler = if let Ok(mut g) = self.inner.lock() {
            g.cols = new_cols;
            g.rows = new_rows;
            g.primary.resize(new_cols, new_rows);
            g.alternate.resize(new_cols, new_rows);
            g.resize_handler.clone()
        } else {
            None
        };
        if let Some(cb) = handler {
            cb();
        }
    }

    pub async fn flush(&self) {
        tokio::task::yield_now().await;
    }

    pub async fn flush_and_get_viewport(&self) -> Vec<String> {
        self.flush().await;
        self.get_viewport()
    }

    pub fn get_viewport(&self) -> Vec<String> {
        let Ok(g) = self.inner.lock() else {
            return Vec::new();
        };
        g.active()
            .lines
            .iter()
            .map(|l| ScreenBuffer::line_to_string(l))
            .collect()
    }

    pub fn get_scroll_buffer(&self) -> Vec<String> {
        let Ok(g) = self.inner.lock() else {
            return Vec::new();
        };
        let buf = g.active();
        let mut lines: Vec<String> = buf.scrollback.iter().map(|l| ScreenBuffer::line_to_string(l)).collect();
        for l in &buf.lines {
            lines.push(ScreenBuffer::line_to_string(l));
        }
        lines
    }

    pub fn get_cell(&self, row: usize, col: usize) -> Option<VirtualCell> {
        let g = self.inner.lock().ok()?;
        g.active().lines.get(row)?.get(col).cloned()
    }

    pub fn clear(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.clear_viewport();
        }
    }

    pub fn reset(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.reset();
        }
    }

    pub fn get_cursor_position(&self) -> VirtualCursorPosition {
        let Ok(g) = self.inner.lock() else {
            return VirtualCursorPosition { x: 0, y: 0 };
        };
        let buf = g.active();
        VirtualCursorPosition {
            x: buf.cursor_x,
            y: buf.cursor_y,
        }
    }

    pub async fn wait_for_render(&self) {
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(28)).await;
        self.flush().await;
    }

    pub fn writes(&self) -> Vec<String> {
        self.inner.lock().map(|g| g.writes.clone()).unwrap_or_default()
    }

    pub fn raw_output(&self) -> String {
        self.inner.lock().map(|g| g.raw_output.clone()).unwrap_or_default()
    }

    pub fn clear_writes(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.writes.clear();
            g.raw_output.clear();
        }
    }

    pub fn title(&self) -> String {
        self.inner.lock().map(|g| g.title.clone()).unwrap_or_default()
    }

    pub fn is_cursor_visible(&self) -> bool {
        self.inner.lock().map(|g| g.cursor_visible).unwrap_or(true)
    }

    pub fn is_alternate_screen(&self) -> bool {
        self.inner.lock().map(|g| g.using_alternate).unwrap_or(false)
    }
}

#[async_trait]
impl Terminal for VirtualTerminal {
    fn start(&self, on_input: TerminalInputHandler, on_resize: TerminalResizeHandler) {
        if let Ok(mut g) = self.inner.lock() {
            g.input_handler = Some(on_input);
            g.resize_handler = Some(on_resize);
            g.process_str("\x1b[?2004h");
        }
    }

    fn stop(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.process_str("\x1b[?2004l");
            g.input_handler = None;
            g.resize_handler = None;
        }
    }

    async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}

    fn write(&self, data: &str) {
        if let Ok(mut g) = self.inner.lock() {
            g.process_str(data);
        }
    }

    fn columns(&self) -> i64 {
        self.inner.lock().map(|g| g.cols as i64).unwrap_or(80)
    }

    fn rows(&self) -> i64 {
        self.inner.lock().map(|g| g.rows as i64).unwrap_or(24)
    }

    fn kitty_protocol_active(&self) -> bool {
        true
    }

    fn move_by(&self, lines: i64) {
        if lines > 0 {
            self.write(&format!("\x1b[{lines}B"));
        } else if lines < 0 {
            self.write(&format!("\x1b[{}A", -lines));
        }
    }

    fn hide_cursor(&self) {
        self.write("\x1b[?25l");
    }

    fn show_cursor(&self) {
        self.write("\x1b[?25h");
    }

    fn clear_line(&self) {
        self.write("\x1b[K");
    }

    fn clear_from_cursor(&self) {
        self.write("\x1b[J");
    }

    fn clear_screen(&self) {
        self.write("\x1b[2J\x1b[H");
    }

    fn set_title(&self, title: &str) {
        self.write(&format!("\x1b]0;{title}\x07"));
    }

    fn set_progress(&self, _active: bool) {}
}

fn char_display_width(ch: char) -> usize {
    let cp = ch as u32;
    // Zero-width formatters / combining marks
    if cp == 0x200B
        || cp == 0x200C
        || cp == 0x200D
        || cp == 0xFEFF
        || (0x0300..=0x036F).contains(&cp)
        || (0x1AB0..=0x1AFF).contains(&cp)
        || (0x1DC0..=0x1DFF).contains(&cp)
        || (0x20D0..=0x20FF).contains(&cp)
        || (0xFE00..=0xFE0F).contains(&cp)
        || (0xFE20..=0xFE2F).contains(&cp)
        || (0xE0100..=0xE01EF).contains(&cp)
    {
        return 0;
    }
    // Wide East Asian & Emoji blocks
    if (0x1100..=0x115F).contains(&cp)
        || (0x2329..=0x232A).contains(&cp)
        || (0x2E80..=0x303E).contains(&cp)
        || (0x3040..=0x33FF).contains(&cp)
        || (0x3400..=0x4DBF).contains(&cp)
        || (0x4E00..=0xA4CF).contains(&cp)
        || (0xAC00..=0xD7A3).contains(&cp)
        || (0xF900..=0xFAFF).contains(&cp)
        || (0xFE10..=0xFE19).contains(&cp)
        || (0xFE30..=0xFE6B).contains(&cp)
        || (0xFF01..=0xFF60).contains(&cp)
        || (0xFFE0..=0xFFE6).contains(&cp)
        || (0x1F300..=0x1FAFF).contains(&cp)
        || (0x20000..=0x3FFFD).contains(&cp)
    {
        return 2;
    }
    1
}
