use std::boxed::Box as StdBox;
use std::cell::Cell;
use std::collections::BTreeMap;

use crate::autocomplete::{AutocompleteProvider, AutocompleteSuggestions};
use crate::editor_component::EditorComponent;
use crate::keybindings::get_keybindings;
use crate::keys::{decode_printable_key, parse_key};
use crate::kill_ring::KillRing;
use crate::terminal_image::{
    ImageDimensions, ImageProtocol, ImageRenderOptions, allocate_image_id, get_capabilities,
    get_cell_dimensions, get_image_dimensions, image_fallback, render_image,
};
use crate::tui::{CURSOR_MARKER, Component, Focusable};
use crate::undo_stack::UndoStack;
use crate::utils::{
    slice_by_column, truncate_to_width, truncate_to_width_with, visible_width, wrap_text,
};

#[derive(Debug, Clone)]
pub struct Text {
    text: String,
    padding_x: usize,
    padding_y: usize,
}

impl Default for Text {
    fn default() -> Self {
        Self::new("")
    }
}

impl Text {
    pub fn new(text: impl Into<String>) -> Self {
        Self::with_padding(text, 1, 1)
    }

    pub fn with_padding(text: impl Into<String>, padding_x: usize, padding_y: usize) -> Self {
        Self {
            text: text.into(),
            padding_x,
            padding_y,
        }
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
    }

    pub fn value(&self) -> &str {
        &self.text
    }
}

impl Component for Text {
    fn render(&self, width: usize) -> Vec<String> {
        if self.text.trim().is_empty() {
            return Vec::new();
        }

        let content_width = width.saturating_sub(self.padding_x * 2).max(1);
        let left = " ".repeat(self.padding_x);
        let right = " ".repeat(self.padding_x);
        let empty = " ".repeat(width);
        let mut lines = Vec::new();

        for _ in 0..self.padding_y {
            lines.push(empty.clone());
        }
        for line in wrap_text(&self.text.replace('\t', "   "), content_width) {
            let mut padded = format!("{left}{line}{right}");
            let padding = width.saturating_sub(visible_width(&padded));
            padded.push_str(&" ".repeat(padding));
            lines.push(padded);
        }
        for _ in 0..self.padding_y {
            lines.push(empty.clone());
        }
        lines
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Spacer {
    lines: usize,
}

impl Default for Spacer {
    fn default() -> Self {
        Self::new(1)
    }
}

impl Spacer {
    pub fn new(lines: usize) -> Self {
        Self { lines }
    }

    pub fn set_lines(&mut self, lines: usize) {
        self.lines = lines;
    }
}

impl Component for Spacer {
    fn render(&self, _width: usize) -> Vec<String> {
        vec![String::new(); self.lines]
    }
}

#[derive(Debug, Clone)]
pub struct TruncatedText {
    text: String,
    padding_x: usize,
    padding_y: usize,
}

impl TruncatedText {
    pub fn new(text: impl Into<String>) -> Self {
        Self::with_padding(text, 0, 0)
    }

    pub fn with_padding(text: impl Into<String>, padding_x: usize, padding_y: usize) -> Self {
        Self {
            text: text.into(),
            padding_x,
            padding_y,
        }
    }
}

impl Component for TruncatedText {
    fn render(&self, width: usize) -> Vec<String> {
        let mut result = Vec::new();
        let empty = " ".repeat(width);
        for _ in 0..self.padding_y {
            result.push(empty.clone());
        }

        let available_width = width.saturating_sub(self.padding_x * 2).max(1);
        let single_line = self.text.split('\n').next().unwrap_or_default();
        let display = truncate_to_width(single_line, available_width);
        let mut line = format!(
            "{}{}{}",
            " ".repeat(self.padding_x),
            display,
            " ".repeat(self.padding_x)
        );
        line.push_str(&" ".repeat(width.saturating_sub(visible_width(&line))));
        result.push(line);

        for _ in 0..self.padding_y {
            result.push(empty.clone());
        }
        result
    }
}

#[derive(Default)]
pub struct Box {
    children: Vec<StdBox<dyn Component>>,
    padding_x: usize,
    padding_y: usize,
}

impl Box {
    pub fn new(padding_x: usize, padding_y: usize) -> Self {
        Self {
            children: Vec::new(),
            padding_x,
            padding_y,
        }
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
}

impl Component for Box {
    fn render(&self, width: usize) -> Vec<String> {
        if self.children.is_empty() {
            return Vec::new();
        }
        let content_width = width.saturating_sub(self.padding_x * 2).max(1);
        let left = " ".repeat(self.padding_x);
        let mut child_lines = Vec::new();
        for child in &self.children {
            for line in child.render(content_width) {
                child_lines.push(format!("{left}{line}"));
            }
        }
        if child_lines.is_empty() {
            return Vec::new();
        }

        let mut result = Vec::new();
        for _ in 0..self.padding_y {
            result.push(" ".repeat(width));
        }
        for mut line in child_lines {
            line.push_str(&" ".repeat(width.saturating_sub(visible_width(&line))));
            result.push(line);
        }
        for _ in 0..self.padding_y {
            result.push(" ".repeat(width));
        }
        result
    }

    fn invalidate(&mut self) {
        for child in &mut self.children {
            child.invalidate();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InputState {
    value: String,
    cursor: usize,
}

#[derive(Debug, Clone, Default)]
pub struct Input {
    value: String,
    cursor: usize,
    focused: bool,
    kill_ring: KillRing,
    undo_stack: UndoStack<InputState>,
    last_action: Option<InputAction>,
    last_yank: Option<(usize, usize)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputAction {
    KillBackward,
    KillForward,
    Yank,
    TypeWord,
}

impl Input {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_value(&self) -> &str {
        &self.value
    }

    pub fn set_value(&mut self, value: impl Into<String>) {
        self.value = value.into();
        self.cursor = self.cursor.min(self.value.len());
        self.last_action = None;
        self.last_yank = None;
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    fn state(&self) -> InputState {
        InputState {
            value: self.value.clone(),
            cursor: self.cursor,
        }
    }

    fn push_undo(&mut self) {
        self.undo_stack.push(&self.state());
    }

    fn break_yank(&mut self) {
        if self.last_action != Some(InputAction::Yank) {
            self.last_yank = None;
        }
    }

    fn previous_boundary(&self, cursor: usize) -> usize {
        self.value[..cursor]
            .char_indices()
            .last()
            .map(|(idx, _)| idx)
            .unwrap_or(0)
    }

    fn next_boundary(&self, cursor: usize) -> usize {
        self.value[cursor..]
            .char_indices()
            .nth(1)
            .map(|(idx, _)| cursor + idx)
            .unwrap_or(self.value.len())
    }

    fn insert_text(&mut self, text: &str) {
        let printable = text
            .chars()
            .filter(|ch| !ch.is_control())
            .collect::<String>();
        if printable.is_empty() {
            return;
        }
        let all_word = printable
            .chars()
            .all(|ch| ch.is_alphanumeric() || ch == '_');
        if !(all_word && self.last_action == Some(InputAction::TypeWord)) {
            self.push_undo();
        }
        for ch in printable.chars() {
            self.value.insert(self.cursor, ch);
            self.cursor += ch.len_utf8();
        }
        self.last_action = if all_word {
            Some(InputAction::TypeWord)
        } else {
            None
        };
        self.last_yank = None;
    }

    fn delete_range(
        &mut self,
        start: usize,
        end: usize,
        kill_prepend: bool,
        kill_action: Option<InputAction>,
    ) -> String {
        if start >= end || end > self.value.len() {
            return String::new();
        }
        self.push_undo();
        let deleted = self.value[start..end].to_string();
        self.value.drain(start..end);
        self.cursor = start;
        if let Some(action) = kill_action {
            let accumulate = self.last_action == Some(action);
            self.kill_ring
                .push(deleted.clone(), kill_prepend, accumulate);
            self.last_action = Some(action);
        } else {
            self.last_action = None;
        }
        self.last_yank = None;
        deleted
    }

    fn word_start_before_cursor(&self) -> usize {
        let mut pos = self.cursor;
        while pos > 0 {
            let prev = self.previous_boundary(pos);
            let ch = self.value[prev..pos].chars().next().unwrap_or_default();
            if !ch.is_whitespace() {
                break;
            }
            pos = prev;
        }
        while pos > 0 {
            let prev = self.previous_boundary(pos);
            let ch = self.value[prev..pos].chars().next().unwrap_or_default();
            if ch.is_whitespace() {
                break;
            }
            pos = prev;
        }
        pos
    }

    fn word_end_after_cursor(&self) -> usize {
        let mut pos = self.cursor;
        while pos < self.value.len() {
            let next = self.next_boundary(pos);
            let ch = self.value[pos..next].chars().next().unwrap_or_default();
            if !ch.is_whitespace() {
                break;
            }
            pos = next;
        }
        while pos < self.value.len() {
            let next = self.next_boundary(pos);
            let ch = self.value[pos..next].chars().next().unwrap_or_default();
            if ch.is_whitespace() {
                break;
            }
            pos = next;
        }
        pos
    }

    fn yank(&mut self) {
        let Some(text) = self.kill_ring.peek().map(str::to_string) else {
            return;
        };
        self.push_undo();
        let start = self.cursor;
        self.value.insert_str(self.cursor, &text);
        self.cursor += text.len();
        self.last_yank = Some((start, self.cursor));
        self.last_action = Some(InputAction::Yank);
    }

    fn yank_pop(&mut self) {
        if self.last_action != Some(InputAction::Yank) || self.kill_ring.len() <= 1 {
            return;
        }
        let Some((start, end)) = self.last_yank else {
            return;
        };
        if end > self.value.len() || start > end {
            return;
        }
        self.kill_ring.rotate();
        let Some(text) = self.kill_ring.peek().map(str::to_string) else {
            return;
        };
        self.value.replace_range(start..end, &text);
        self.cursor = start + text.len();
        self.last_yank = Some((start, self.cursor));
        self.last_action = Some(InputAction::Yank);
    }

    fn undo(&mut self) {
        if let Some(state) = self.undo_stack.pop() {
            self.value = state.value;
            self.cursor = state.cursor.min(self.value.len());
            self.last_action = None;
            self.last_yank = None;
        }
    }
}

impl Component for Input {
    fn render(&self, width: usize) -> Vec<String> {
        if !self.focused || width == 0 {
            return vec![truncate_to_width(&self.value, width)];
        }

        let cursor = self.cursor.min(self.value.len());
        let before = &self.value[..cursor];
        let after = &self.value[cursor..];
        let before_width = visible_width(before);
        let start_col = before_width.saturating_sub(width.saturating_sub(1));
        let before_visible = slice_by_column(before, start_col, width.saturating_sub(1), false);
        let before_visible_width = visible_width(&before_visible);
        let remaining_width = width.saturating_sub(before_visible_width);
        let after_visible = truncate_to_width_with(after, remaining_width, "", false);
        vec![format!("{before_visible}{CURSOR_MARKER}{after_visible}")]
    }

    fn handle_input(&mut self, data: &str) {
        match parse_key(data).as_deref() {
            Some("ctrl+-") => self.undo(),
            Some("ctrl+y") => self.yank(),
            Some("alt+y") => self.yank_pop(),
            Some("backspace") => {
                if self.cursor > 0 {
                    let prev = self.previous_boundary(self.cursor);
                    self.delete_range(prev, self.cursor, false, None);
                }
            }
            Some("delete") => {
                if self.cursor < self.value.len() {
                    let next = self.next_boundary(self.cursor);
                    self.delete_range(self.cursor, next, false, None);
                }
            }
            Some("ctrl+w") | Some("alt+backspace") => {
                let start = self.word_start_before_cursor();
                self.delete_range(start, self.cursor, true, Some(InputAction::KillBackward));
            }
            Some("alt+d") | Some("alt+delete") => {
                let end = self.word_end_after_cursor();
                self.delete_range(self.cursor, end, false, Some(InputAction::KillForward));
            }
            Some("ctrl+u") => {
                self.delete_range(0, self.cursor, true, Some(InputAction::KillBackward));
            }
            Some("ctrl+k") => {
                self.delete_range(
                    self.cursor,
                    self.value.len(),
                    false,
                    Some(InputAction::KillForward),
                );
            }
            Some("left") | Some("ctrl+b") => {
                if self.cursor > 0 {
                    self.cursor = self.previous_boundary(self.cursor);
                }
                self.last_action = None;
            }
            Some("right") | Some("ctrl+f") => {
                if self.cursor < self.value.len() {
                    self.cursor = self.next_boundary(self.cursor);
                }
                self.last_action = None;
            }
            Some("home") | Some("ctrl+a") => {
                self.cursor = 0;
                self.last_action = None;
            }
            Some("end") | Some("ctrl+e") => {
                self.cursor = self.value.len();
                self.last_action = None;
            }
            Some("enter") => {}
            _ => self.insert_text(data),
        }
        self.break_yank();
    }
}

impl Focusable for Input {
    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn focused(&self) -> bool {
        self.focused
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextChunk {
    pub text: String,
    pub start_index: usize,
    pub end_index: usize,
}

pub fn word_wrap_line(line: &str, max_width: usize) -> Vec<TextChunk> {
    if line.is_empty() || max_width == 0 {
        return vec![TextChunk {
            text: String::new(),
            start_index: 0,
            end_index: 0,
        }];
    }
    if visible_width(line) <= max_width {
        return vec![TextChunk {
            text: line.to_string(),
            start_index: 0,
            end_index: line.len(),
        }];
    }

    let chars = line.char_indices().collect::<Vec<_>>();
    let mut chunks = Vec::new();
    let mut current_width = 0;
    let mut chunk_start = 0;
    let mut wrap_opp_index: Option<usize> = None;
    let mut wrap_opp_width = 0;

    for (i, (char_index, ch)) in chars.iter().copied().enumerate() {
        let g_width = visible_width(&ch.to_string());
        let is_ws = ch.is_whitespace();

        if current_width + g_width > max_width {
            if let Some(opp) = wrap_opp_index {
                if current_width.saturating_sub(wrap_opp_width) + g_width <= max_width {
                    chunks.push(TextChunk {
                        text: line[chunk_start..opp].to_string(),
                        start_index: chunk_start,
                        end_index: opp,
                    });
                    chunk_start = opp;
                    current_width = current_width.saturating_sub(wrap_opp_width);
                } else if chunk_start < char_index {
                    chunks.push(TextChunk {
                        text: line[chunk_start..char_index].to_string(),
                        start_index: chunk_start,
                        end_index: char_index,
                    });
                    chunk_start = char_index;
                    current_width = 0;
                }
            } else if chunk_start < char_index {
                chunks.push(TextChunk {
                    text: line[chunk_start..char_index].to_string(),
                    start_index: chunk_start,
                    end_index: char_index,
                });
                chunk_start = char_index;
                current_width = 0;
            }
            wrap_opp_index = None;
        }

        current_width += g_width;
        if let Some((next_index, next_ch)) = chars.get(i + 1).copied() {
            if is_ws && !next_ch.is_whitespace() {
                wrap_opp_index = Some(next_index);
                wrap_opp_width = current_width;
            }
        }
    }

    chunks.push(TextChunk {
        text: line[chunk_start..].to_string(),
        start_index: chunk_start,
        end_index: line.len(),
    });
    chunks
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditorTheme {
    pub placeholder: Option<String>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct EditorOptions {
    pub placeholder: Option<String>,
    pub multiline: bool,
    pub theme: Option<EditorTheme>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EditorCursor {
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EditorState {
    text: String,
    cursor: usize,
}

#[derive(Default)]
pub struct Editor {
    text: String,
    cursor: usize,
    focused: bool,
    options: EditorOptions,
    history: Vec<String>,
    history_index: Option<usize>,
    history_draft: String,
    kill_ring: KillRing,
    undo_stack: UndoStack<EditorState>,
    last_action: Option<InputAction>,
    last_yank: Option<(usize, usize)>,
    is_in_paste: bool,
    paste_buffer: String,
    pastes: BTreeMap<usize, String>,
    paste_counter: usize,
    autocomplete_provider: Option<std::boxed::Box<dyn AutocompleteProvider>>,
    autocomplete_state: Option<AutocompleteSuggestions>,
    autocomplete_selected: usize,
    autocomplete_force: bool,
    on_submit: Option<std::boxed::Box<dyn FnMut(String)>>,
    on_change: Option<std::boxed::Box<dyn FnMut(String)>>,
}

impl Editor {
    pub fn new(options: EditorOptions) -> Self {
        Self {
            options,
            ..Self::default()
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn get_text(&self) -> &str {
        &self.text
    }

    pub fn set_autocomplete_provider<P: AutocompleteProvider + 'static>(&mut self, provider: P) {
        self.autocomplete_provider = Some(std::boxed::Box::new(provider));
    }

    pub fn is_showing_autocomplete(&self) -> bool {
        self.autocomplete_state.is_some()
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        let normalized = normalize_editor_text(&text.into());
        if self.text != normalized {
            self.push_undo();
        }
        self.text = normalized;
        self.cursor = self.text.len();
        self.history_index = None;
        self.history_draft.clear();
        self.last_action = None;
        self.last_yank = None;
        self.notify_change();
    }

    pub fn set_on_submit<F>(&mut self, callback: F)
    where
        F: FnMut(String) + 'static,
    {
        self.on_submit = Some(std::boxed::Box::new(callback));
    }

    pub fn set_on_change<F>(&mut self, callback: F)
    where
        F: FnMut(String) + 'static,
    {
        self.on_change = Some(std::boxed::Box::new(callback));
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn get_cursor(&self) -> EditorCursor {
        let (line, col) = self.cursor_line_col();
        EditorCursor { line, col }
    }

    pub fn get_lines(&self) -> Vec<String> {
        self.text.split('\n').map(str::to_string).collect()
    }

    pub fn add_to_history(&mut self, text: impl Into<String>) {
        let text = text.into();
        if text.trim().is_empty() {
            return;
        }
        if self.history.last() == Some(&text) {
            return;
        }
        self.history.push(text);
        if self.history.len() > 100 {
            let overflow = self.history.len() - 100;
            self.history.drain(0..overflow);
        }
    }

    pub fn history(&self) -> &[String] {
        &self.history
    }

    pub fn paste_ids(&self) -> Vec<usize> {
        self.pastes.keys().copied().collect()
    }

    fn state(&self) -> EditorState {
        EditorState {
            text: self.text.clone(),
            cursor: self.cursor,
        }
    }

    fn push_undo(&mut self) {
        self.undo_stack.push(&self.state());
    }

    fn undo(&mut self) {
        if let Some(state) = self.undo_stack.pop() {
            self.text = state.text;
            self.cursor = state.cursor.min(self.text.len());
            self.exit_history_mode();
            self.last_action = None;
            self.last_yank = None;
            self.notify_change();
        }
    }

    fn previous_boundary(&self, cursor: usize) -> usize {
        self.text[..cursor]
            .char_indices()
            .last()
            .map(|(idx, _)| idx)
            .unwrap_or(0)
    }

    fn next_boundary(&self, cursor: usize) -> usize {
        self.text[cursor..]
            .char_indices()
            .nth(1)
            .map(|(idx, _)| cursor + idx)
            .unwrap_or(self.text.len())
    }

    fn cursor_line_col(&self) -> (usize, usize) {
        let before_cursor = &self.text[..self.cursor.min(self.text.len())];
        let line = before_cursor.bytes().filter(|byte| *byte == b'\n').count();
        let col = before_cursor
            .rsplit_once('\n')
            .map(|(_, suffix)| suffix.chars().count())
            .unwrap_or_else(|| before_cursor.chars().count());
        (line, col)
    }

    fn line_count(&self) -> usize {
        self.text.bytes().filter(|byte| *byte == b'\n').count() + 1
    }

    fn byte_index_for_line_col(&self, target_line: usize, target_col: usize) -> usize {
        let mut line = 0;
        let mut col = 0;
        for (idx, ch) in self.text.char_indices() {
            if line == target_line && col == target_col {
                return idx;
            }
            if ch == '\n' {
                if line == target_line {
                    return idx;
                }
                line += 1;
                col = 0;
            } else if line == target_line {
                col += 1;
            }
        }
        self.text.len()
    }

    fn move_cursor_vertical(&mut self, direction: isize) -> bool {
        let (line, col) = self.cursor_line_col();
        let line_count = self.line_count();
        let Some(target_line) = line.checked_add_signed(direction) else {
            return false;
        };
        if target_line >= line_count {
            return false;
        }
        self.cursor = self.byte_index_for_line_col(target_line, col);
        true
    }

    fn exit_history_mode(&mut self) {
        self.history_index = None;
        self.history_draft.clear();
    }

    fn break_yank(&mut self) {
        if self.last_action != Some(InputAction::Yank) {
            self.last_yank = None;
        }
    }

    fn notify_change(&mut self) {
        if let Some(on_change) = self.on_change.as_mut() {
            on_change(self.text.clone());
        }
    }

    fn submit_value(&mut self) {
        let submitted = self.expand_paste_markers(&self.text).trim().to_string();
        self.text.clear();
        self.cursor = 0;
        self.exit_history_mode();
        self.kill_ring = KillRing::default();
        self.undo_stack.clear();
        self.last_action = None;
        self.last_yank = None;
        self.is_in_paste = false;
        self.paste_buffer.clear();
        self.pastes.clear();
        self.paste_counter = 0;
        self.notify_change();
        if let Some(on_submit) = self.on_submit.as_mut() {
            on_submit(submitted);
        }
    }

    fn navigate_history_up(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next_index = match self.history_index {
            None => {
                self.history_draft = self.text.clone();
                self.history.len() - 1
            }
            Some(0) => 0,
            Some(index) => index - 1,
        };
        self.history_index = Some(next_index);
        self.text = self.history[next_index].clone();
        self.cursor = self.text.len();
        self.notify_change();
    }

    fn navigate_history_down(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 >= self.history.len() {
            self.text = std::mem::take(&mut self.history_draft);
            self.history_index = None;
        } else {
            let next = index + 1;
            self.history_index = Some(next);
            self.text = self.history[next].clone();
        }
        self.cursor = self.text.len();
        self.notify_change();
    }

    fn line_start_index(&self) -> usize {
        self.text[..self.cursor.min(self.text.len())]
            .rfind('\n')
            .map(|idx| idx + 1)
            .unwrap_or(0)
    }

    fn line_end_index(&self) -> usize {
        self.text[self.cursor.min(self.text.len())..]
            .find('\n')
            .map(|idx| self.cursor + idx)
            .unwrap_or(self.text.len())
    }

    fn delete_range(
        &mut self,
        start: usize,
        end: usize,
        kill_prepend: bool,
        kill_action: Option<InputAction>,
    ) -> String {
        if start >= end || end > self.text.len() {
            return String::new();
        }
        self.push_undo();
        let deleted = self.text[start..end].to_string();
        self.text.drain(start..end);
        self.cursor = start;
        if let Some(action) = kill_action {
            let accumulate = self.last_action == Some(action);
            self.kill_ring
                .push(deleted.clone(), kill_prepend, accumulate);
            self.last_action = Some(action);
        } else {
            self.last_action = None;
        }
        self.last_yank = None;
        self.exit_history_mode();
        self.notify_change();
        deleted
    }

    fn word_start_before_cursor(&self) -> usize {
        let mut pos = self.cursor;
        while pos > 0 {
            let prev = self.previous_boundary(pos);
            let ch = self.text[prev..pos].chars().next().unwrap_or_default();
            if !ch.is_whitespace() {
                break;
            }
            pos = prev;
        }
        while pos > 0 {
            let prev = self.previous_boundary(pos);
            let ch = self.text[prev..pos].chars().next().unwrap_or_default();
            if ch.is_whitespace() {
                break;
            }
            pos = prev;
        }
        pos
    }

    fn word_end_after_cursor(&self) -> usize {
        let mut pos = self.cursor;
        while pos < self.text.len() {
            let next = self.next_boundary(pos);
            let ch = self.text[pos..next].chars().next().unwrap_or_default();
            if !ch.is_whitespace() {
                break;
            }
            pos = next;
        }
        while pos < self.text.len() {
            let next = self.next_boundary(pos);
            let ch = self.text[pos..next].chars().next().unwrap_or_default();
            if ch.is_whitespace() {
                break;
            }
            pos = next;
        }
        pos
    }

    fn move_word_backwards(&mut self) {
        self.cursor = self.word_start_before_cursor();
        self.last_action = None;
    }

    fn move_word_forwards(&mut self) {
        self.cursor = self.word_end_after_cursor();
        self.last_action = None;
    }

    fn delete_to_line_start(&mut self) {
        let start = self.line_start_index();
        if self.cursor > start {
            self.delete_range(start, self.cursor, true, Some(InputAction::KillBackward));
        } else if self.cursor > 0 {
            let prev = self.previous_boundary(self.cursor);
            self.delete_range(prev, self.cursor, true, Some(InputAction::KillBackward));
        }
    }

    fn delete_to_line_end(&mut self) {
        let end = self.line_end_index();
        if self.cursor < end {
            self.delete_range(self.cursor, end, false, Some(InputAction::KillForward));
        } else if self.cursor < self.text.len() {
            let next = self.next_boundary(self.cursor);
            self.delete_range(self.cursor, next, false, Some(InputAction::KillForward));
        }
    }

    fn yank(&mut self) {
        let Some(text) = self.kill_ring.peek().map(str::to_string) else {
            return;
        };
        self.push_undo();
        let start = self.cursor;
        self.text.insert_str(self.cursor, &text);
        self.cursor += text.len();
        self.last_yank = Some((start, self.cursor));
        self.last_action = Some(InputAction::Yank);
        self.notify_change();
    }

    fn yank_pop(&mut self) {
        if self.last_action != Some(InputAction::Yank) || self.kill_ring.len() <= 1 {
            return;
        }
        let Some((start, end)) = self.last_yank else {
            return;
        };
        if start > end || end > self.text.len() {
            return;
        }
        self.kill_ring.rotate();
        let Some(text) = self.kill_ring.peek().map(str::to_string) else {
            return;
        };
        self.text.replace_range(start..end, &text);
        self.cursor = start + text.len();
        self.last_yank = Some((start, self.cursor));
        self.last_action = Some(InputAction::Yank);
        self.notify_change();
    }

    fn char_before_cursor(&self) -> Option<char> {
        self.text[..self.cursor.min(self.text.len())]
            .chars()
            .next_back()
    }

    fn insert_text_without_undo(&mut self, text: &str) {
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.last_action = None;
        self.last_yank = None;
        self.exit_history_mode();
        self.notify_change();
    }

    fn decode_ctrl_csi_u_in_paste(text: &str) -> String {
        let mut result = String::new();
        let mut rest = text;
        while let Some(start) = rest.find("\u{1b}[") {
            result.push_str(&rest[..start]);
            let after_prefix = &rest[start + 2..];
            if let Some(end) = after_prefix.find(";5u") {
                let code_str = &after_prefix[..end];
                if !code_str.is_empty() && code_str.chars().all(|ch| ch.is_ascii_digit()) {
                    if let Ok(cp) = code_str.parse::<u32>() {
                        let decoded = match cp {
                            65..=90 => char::from_u32(cp - 64),
                            97..=122 => char::from_u32(cp - 96),
                            _ => None,
                        };
                        if let Some(ch) = decoded {
                            result.push(ch);
                            rest = &after_prefix[end + 3..];
                            continue;
                        }
                    }
                }
            }
            result.push_str("\u{1b}[");
            rest = after_prefix;
        }
        result.push_str(rest);
        result
    }

    fn handle_paste(&mut self, pasted_text: &str) {
        if pasted_text.is_empty() {
            return;
        }
        let decoded = Self::decode_ctrl_csi_u_in_paste(pasted_text);
        let clean = normalize_editor_text(&decoded)
            .chars()
            .filter(|ch| *ch == '\n' || !ch.is_control())
            .collect::<String>();
        if clean.is_empty() {
            return;
        }

        let mut filtered = clean;
        if matches!(filtered.chars().next(), Some('/') | Some('~') | Some('.'))
            && self
                .char_before_cursor()
                .is_some_and(|ch| ch == '_' || ch.is_alphanumeric())
        {
            filtered.insert(0, ' ');
        }

        let line_count = filtered.split('\n').count();
        let total_chars = filtered.chars().count();
        let insert = if line_count > 10 || total_chars > 1000 {
            self.paste_counter += 1;
            let paste_id = self.paste_counter;
            self.pastes.insert(paste_id, filtered);
            if line_count > 10 {
                format!("[paste #{paste_id} +{line_count} lines]")
            } else {
                format!("[paste #{paste_id} {total_chars} chars]")
            }
        } else {
            filtered
        };

        self.push_undo();
        self.insert_text_without_undo(&insert);
    }

    fn expand_paste_markers(&self, text: &str) -> String {
        let mut expanded = text.to_string();
        for (paste_id, paste_content) in &self.pastes {
            let prefix = format!("[paste #{paste_id}");
            let mut search_from = 0;
            while let Some(relative_start) = expanded[search_from..].find(&prefix) {
                let start = search_from + relative_start;
                let Some(relative_end) = expanded[start..].find(']') else {
                    break;
                };
                let end = start + relative_end + 1;
                expanded.replace_range(start..end, paste_content);
                search_from = start + paste_content.len();
            }
        }
        expanded
    }

    fn insert_text(&mut self, text: &str) {
        let normalized = normalize_editor_text(text);
        let printable = normalized
            .chars()
            .filter(|ch| *ch == '\n' || !ch.is_control())
            .collect::<String>();
        if printable.is_empty() {
            return;
        }
        self.exit_history_mode();
        let all_word = printable
            .chars()
            .all(|ch| ch.is_alphanumeric() || ch == '_');
        let all_whitespace = printable.chars().all(char::is_whitespace);
        if all_whitespace || !(all_word && self.last_action == Some(InputAction::TypeWord)) {
            self.push_undo();
        }
        for ch in printable.chars() {
            self.text.insert(self.cursor, ch);
            self.cursor += ch.len_utf8();
        }
        self.last_action = if all_word || all_whitespace {
            Some(InputAction::TypeWord)
        } else {
            None
        };
        self.last_yank = None;
        if self.autocomplete_state.is_none() {
            self.try_trigger_autocomplete(false);
        } else {
            self.update_autocomplete();
        }
        self.notify_change();
    }

    fn try_trigger_autocomplete(&mut self, explicit_tab: bool) {
        let (lines, line, col, force) = {
            let force = explicit_tab && self.autocomplete_force;
            (
                self.get_lines(),
                self.cursor_line_col().0,
                self.cursor_line_col().1,
                force,
            )
        };

        let result = if let Some(provider) = self.autocomplete_provider.as_ref() {
            if force && !provider.should_trigger_file_completion(&lines, line, col) {
                None
            } else {
                provider.get_suggestions(&lines, line, col, force)
            }
        } else {
            None
        };

        if let Some(suggestions) = result {
            if !suggestions.items.is_empty() {
                if force && explicit_tab && suggestions.items.len() == 1 {
                    let (new_lines, new_line, new_col) = self
                        .autocomplete_provider
                        .as_ref()
                        .unwrap()
                        .apply_completion(
                            &lines,
                            line,
                            col,
                            &suggestions.items[0],
                            &suggestions.prefix,
                        );
                    self.push_undo();
                    self.text = new_lines.join("\n");
                    self.cursor = self.byte_index_for_line_col(new_line, new_col);
                    self.last_action = None;
                    self.cancel_autocomplete();
                    self.notify_change();
                } else {
                    self.autocomplete_selected = 0;
                    self.autocomplete_state = Some(suggestions);
                }
            } else {
                self.cancel_autocomplete();
            }
        } else {
            self.cancel_autocomplete();
        }
    }

    fn update_autocomplete(&mut self) {
        let force = self.autocomplete_force;
        self.autocomplete_force = force;
        self.try_trigger_autocomplete(false);
    }

    fn cancel_autocomplete(&mut self) {
        self.autocomplete_state = None;
        self.autocomplete_force = false;
        self.autocomplete_selected = 0;
    }

    fn apply_selected_completion(&mut self) -> bool {
        let (lines, line, col, item, prefix) = {
            if let Some(state) = self.autocomplete_state.as_ref() {
                if let Some(item) = state.items.get(self.autocomplete_selected) {
                    (
                        self.get_lines(),
                        self.cursor_line_col().0,
                        self.cursor_line_col().1,
                        item.clone(),
                        state.prefix.clone(),
                    )
                } else {
                    return false;
                }
            } else {
                return false;
            }
        };

        if let Some(provider) = self.autocomplete_provider.as_ref() {
            let (new_lines, new_line, new_col) =
                provider.apply_completion(&lines, line, col, &item, &prefix);
            self.push_undo();
            self.text = new_lines.join("\n");
            self.cursor = self.byte_index_for_line_col(new_line, new_col);
            self.last_action = None;
            self.cancel_autocomplete();
            self.notify_change();
            true
        } else {
            false
        }
    }

    fn handle_tab_completion(&mut self) {
        if self.autocomplete_provider.is_none() {
            return;
        }
        let lines = self.get_lines();
        let (line, col) = self.cursor_line_col();
        let current_line = lines.get(line).map_or("", String::as_str);
        let text_before_cursor = &current_line[..col.min(current_line.len())];
        if text_before_cursor.trim_start().starts_with('/')
            && !text_before_cursor.trim_start().contains(' ')
        {
            self.autocomplete_force = false;
        } else {
            self.autocomplete_force = true;
        }
        self.try_trigger_autocomplete(true);
    }

    fn add_newline(&mut self) {
        self.exit_history_mode();
        self.push_undo();
        self.text.insert(self.cursor, '\n');
        self.cursor += 1;
        self.last_action = None;
        self.last_yank = None;
        self.notify_change();
    }

    fn backslash_enter_newline(&mut self) {
        if self.char_before_cursor() == Some('\\') {
            let prev = self.previous_boundary(self.cursor);
            self.text.drain(prev..self.cursor);
            self.cursor = prev;
            self.add_newline();
        } else {
            self.submit_value();
        }
    }
}

fn normalize_editor_text(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\t', "    ")
}

impl Component for Editor {
    fn render(&self, width: usize) -> Vec<String> {
        let mut value = if self.text.is_empty() {
            self.options.placeholder.clone().unwrap_or_default()
        } else {
            self.text.clone()
        };
        if self.focused {
            let cursor = self.cursor.min(value.len());
            value.insert_str(cursor, CURSOR_MARKER);
        }
        if self.options.multiline {
            wrap_text(&value, width)
        } else {
            vec![truncate_to_width(&value, width)]
        }
    }

    fn handle_input(&mut self, data: &str) {
        let mut data_owned = data.to_string();
        if data_owned.contains("\u{1b}[200~") {
            self.is_in_paste = true;
            self.paste_buffer.clear();
            data_owned = data_owned.replacen("\u{1b}[200~", "", 1);
        }
        if self.is_in_paste {
            self.paste_buffer.push_str(&data_owned);
            if let Some(end_index) = self.paste_buffer.find("\u{1b}[201~") {
                let paste_content = self.paste_buffer[..end_index].to_string();
                let remaining = self.paste_buffer[end_index + 6..].to_string();
                self.is_in_paste = false;
                self.paste_buffer.clear();
                self.handle_paste(&paste_content);
                if !remaining.is_empty() {
                    self.handle_input(&remaining);
                }
            }
            return;
        }

        if let Some(printable) = decode_printable_key(data) {
            self.insert_text(&printable);
            return;
        }

        if self.autocomplete_state.is_some() {
            match parse_key(data).as_deref() {
                Some("escape") | Some("ctrl+c") => {
                    self.cancel_autocomplete();
                    return;
                }
                Some("up") => {
                    if let Some(state) = &self.autocomplete_state {
                        if !state.items.is_empty() {
                            if self.autocomplete_selected == 0 {
                                self.autocomplete_selected = state.items.len() - 1;
                            } else {
                                self.autocomplete_selected -= 1;
                            }
                        }
                    }
                    return;
                }
                Some("down") => {
                    if let Some(state) = &self.autocomplete_state {
                        if !state.items.is_empty() {
                            self.autocomplete_selected =
                                (self.autocomplete_selected + 1) % state.items.len();
                        }
                    }
                    return;
                }
                Some("tab") | Some("enter") => {
                    if self.apply_selected_completion() {
                        return;
                    }
                }
                _ => {}
            }
        }

        match parse_key(data).as_deref() {
            Some("tab") if self.autocomplete_state.is_none() => self.handle_tab_completion(),
            Some("ctrl+-") => self.undo(),
            Some("backspace") => {
                if self.cursor > 0 {
                    let prev = self.previous_boundary(self.cursor);
                    self.delete_range(prev, self.cursor, false, None);
                }
            }
            Some("delete") | Some("ctrl+d") => {
                if self.cursor < self.text.len() {
                    let next = self.next_boundary(self.cursor);
                    self.delete_range(self.cursor, next, false, None);
                }
            }
            Some("ctrl+w") | Some("alt+backspace") => {
                let start = self.word_start_before_cursor();
                self.delete_range(start, self.cursor, true, Some(InputAction::KillBackward));
            }
            Some("alt+d") | Some("alt+delete") => {
                let end = self.word_end_after_cursor();
                self.delete_range(self.cursor, end, false, Some(InputAction::KillForward));
            }
            Some("ctrl+u") => self.delete_to_line_start(),
            Some("ctrl+k") => self.delete_to_line_end(),
            Some("ctrl+y") => self.yank(),
            Some("alt+y") => self.yank_pop(),
            Some("enter") if self.options.multiline => self.add_newline(),
            Some("shift+enter") => self.add_newline(),
            Some("enter") => self.backslash_enter_newline(),
            Some("up") => {
                if !self.move_cursor_vertical(-1)
                    && (self.text.is_empty() || self.history_index.is_some())
                {
                    self.navigate_history_up();
                }
                self.last_action = None;
            }
            Some("down") => {
                if !self.move_cursor_vertical(1) && self.history_index.is_some() {
                    self.navigate_history_down();
                }
                self.last_action = None;
            }
            Some("left") | Some("ctrl+b") => {
                if self.cursor > 0 {
                    self.cursor = self.previous_boundary(self.cursor);
                }
                self.last_action = None;
            }
            Some("right") | Some("ctrl+f") => {
                if self.cursor < self.text.len() {
                    self.cursor = self.next_boundary(self.cursor);
                }
                self.last_action = None;
            }
            Some("alt+left") | Some("ctrl+left") | Some("alt+b") => self.move_word_backwards(),
            Some("alt+right") | Some("ctrl+right") | Some("alt+f") => self.move_word_forwards(),
            Some("home") | Some("ctrl+a") => {
                self.cursor = self.line_start_index();
                self.last_action = None;
            }
            Some("end") | Some("ctrl+e") => {
                self.cursor = self.line_end_index();
                self.last_action = None;
            }
            _ => self.insert_text(data),
        }
        self.break_yank();
    }
}

impl Focusable for Editor {
    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn focused(&self) -> bool {
        self.focused
    }
}

impl EditorComponent for Editor {
    fn get_text(&self) -> &str {
        self.get_text()
    }

    fn set_text(&mut self, text: String) {
        self.set_text(text);
    }

    fn cursor(&self) -> usize {
        self.cursor()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoaderIndicatorOptions {
    pub frames: Vec<String>,
    pub interval_ms: u64,
}

impl Default for LoaderIndicatorOptions {
    fn default() -> Self {
        Self {
            frames: ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            interval_ms: 80,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Loader {
    message: String,
    frames: Vec<String>,
    current_frame: usize,
}

impl Loader {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            frames: ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            current_frame: 0,
        }
    }

    pub fn set_message(&mut self, message: impl Into<String>) {
        self.message = message.into();
    }

    pub fn tick(&mut self) {
        if !self.frames.is_empty() {
            self.current_frame = (self.current_frame + 1) % self.frames.len();
        }
    }
}

impl Component for Loader {
    fn render(&self, width: usize) -> Vec<String> {
        let frame = self
            .frames
            .get(self.current_frame)
            .map_or("", String::as_str);
        Text::with_padding(format!("{frame} {}", self.message), 1, 0).render(width)
    }
}

pub struct CancellableLoader {
    loader: Loader,
    aborted: bool,
    on_abort: Option<std::boxed::Box<dyn FnMut()>>,
}

impl CancellableLoader {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            loader: Loader::new(message),
            aborted: false,
            on_abort: None,
        }
    }

    pub fn set_message(&mut self, message: impl Into<String>) {
        self.loader.set_message(message);
    }

    pub fn tick(&mut self) {
        self.loader.tick();
    }

    pub fn aborted(&self) -> bool {
        self.aborted
    }

    pub fn set_on_abort<F>(&mut self, callback: F)
    where
        F: FnMut() + 'static,
    {
        self.on_abort = Some(std::boxed::Box::new(callback));
    }

    pub fn dispose(&mut self) {
        // No timer resources in the Rust loader yet; keep API parity.
    }
}

impl Component for CancellableLoader {
    fn render(&self, width: usize) -> Vec<String> {
        self.loader.render(width)
    }

    fn handle_input(&mut self, data: &str) {
        if get_keybindings().matches(data, "tui.select.cancel") {
            self.aborted = true;
            if let Some(on_abort) = self.on_abort.as_mut() {
                on_abort();
            }
        }
    }

    fn invalidate(&mut self) {
        self.loader.invalidate();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectItem {
    pub label: String,
    pub value: String,
    pub description: Option<String>,
}

impl SelectItem {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            description: None,
        }
    }
}

const DEFAULT_PRIMARY_COLUMN_WIDTH: usize = 32;
const PRIMARY_COLUMN_GAP: usize = 2;
const MIN_DESCRIPTION_WIDTH: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectListTheme {
    pub selected_prefix: String,
    pub unselected_prefix: String,
    pub no_match: String,
}

impl Default for SelectListTheme {
    fn default() -> Self {
        Self {
            selected_prefix: "→ ".to_string(),
            unselected_prefix: "  ".to_string(),
            no_match: "  No matching commands".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SelectListLayoutOptions {
    pub max_height: Option<usize>,
    pub min_primary_column_width: Option<usize>,
    pub max_primary_column_width: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectListTruncatePrimaryContext {
    pub text: String,
    pub max_width: usize,
    pub column_width: usize,
    pub item: SelectItem,
    pub is_selected: bool,
}

#[derive(Debug, Clone)]
pub struct SelectList {
    pub items: Vec<SelectItem>,
    filtered_items: Vec<SelectItem>,
    pub selected_index: usize,
    max_visible: usize,
    theme: SelectListTheme,
    layout: SelectListLayoutOptions,
}

impl Default for SelectList {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl SelectList {
    pub fn new(items: Vec<SelectItem>) -> Self {
        Self::with_options(
            items,
            5,
            SelectListTheme::default(),
            SelectListLayoutOptions::default(),
        )
    }

    pub fn with_options(
        items: Vec<SelectItem>,
        max_visible: usize,
        theme: SelectListTheme,
        layout: SelectListLayoutOptions,
    ) -> Self {
        Self {
            filtered_items: items.clone(),
            items,
            selected_index: 0,
            max_visible: max_visible.max(1),
            theme,
            layout,
        }
    }

    pub fn set_filter(&mut self, filter: &str) {
        let filter = filter.to_lowercase();
        self.filtered_items = self
            .items
            .iter()
            .filter(|item| item.value.to_lowercase().starts_with(&filter))
            .cloned()
            .collect();
        self.selected_index = 0;
    }

    pub fn set_selected_index(&mut self, index: usize) {
        self.selected_index = index.min(self.filtered_items.len().saturating_sub(1));
    }

    pub fn get_selected_item(&self) -> Option<&SelectItem> {
        self.filtered_items.get(self.selected_index)
    }

    fn primary_column_bounds(&self) -> (usize, usize) {
        let raw_min = self
            .layout
            .min_primary_column_width
            .or(self.layout.max_primary_column_width)
            .unwrap_or(DEFAULT_PRIMARY_COLUMN_WIDTH);
        let raw_max = self
            .layout
            .max_primary_column_width
            .or(self.layout.min_primary_column_width)
            .unwrap_or(DEFAULT_PRIMARY_COLUMN_WIDTH);
        (raw_min.min(raw_max).max(1), raw_min.max(raw_max).max(1))
    }

    fn primary_column_width(&self) -> usize {
        let (min, max) = self.primary_column_bounds();
        let widest = self
            .filtered_items
            .iter()
            .map(|item| visible_width(display_value(item)) + PRIMARY_COLUMN_GAP)
            .max()
            .unwrap_or(0);
        widest.clamp(min, max)
    }

    fn render_item(
        &self,
        item: &SelectItem,
        is_selected: bool,
        width: usize,
        primary_column_width: usize,
    ) -> String {
        let prefix = if is_selected {
            &self.theme.selected_prefix
        } else {
            &self.theme.unselected_prefix
        };
        let prefix_width = visible_width(prefix);
        let description = item
            .description
            .as_ref()
            .map(|desc| normalize_to_single_line(desc));

        if let Some(description) = description.as_deref().filter(|_| width > 40) {
            let effective_primary_width = primary_column_width
                .min(width.saturating_sub(prefix_width + 4))
                .max(1);
            let max_primary_width = effective_primary_width
                .saturating_sub(PRIMARY_COLUMN_GAP)
                .max(1);
            let primary = truncate_to_width_with(display_value(item), max_primary_width, "", false);
            let primary_width = visible_width(&primary);
            let spacing = " ".repeat(effective_primary_width.saturating_sub(primary_width).max(1));
            let description_start = prefix_width + primary_width + spacing.len();
            let remaining_width = width.saturating_sub(description_start + 2);
            if remaining_width > MIN_DESCRIPTION_WIDTH {
                let description = truncate_to_width_with(description, remaining_width, "", false);
                return format!("{prefix}{primary}{spacing}{description}");
            }
        }

        let max_width = width.saturating_sub(prefix_width + 2).max(1);
        let primary = truncate_to_width_with(display_value(item), max_width, "", false);
        format!("{prefix}{primary}")
    }
}

fn normalize_to_single_line(text: &str) -> String {
    text.replace(['\r', '\n'], " ").trim().to_string()
}

fn display_value(item: &SelectItem) -> &str {
    if item.label.is_empty() {
        &item.value
    } else {
        &item.label
    }
}

impl Component for SelectList {
    fn render(&self, width: usize) -> Vec<String> {
        if self.filtered_items.is_empty() {
            return vec![truncate_to_width(&self.theme.no_match, width)];
        }

        let primary_column_width = self.primary_column_width();
        let visible = self.layout.max_height.unwrap_or(self.max_visible).max(1);
        let start = self
            .selected_index
            .saturating_sub(visible / 2)
            .min(self.filtered_items.len().saturating_sub(visible));
        let end = (start + visible).min(self.filtered_items.len());

        let mut lines = Vec::new();
        for i in start..end {
            if let Some(item) = self.filtered_items.get(i) {
                lines.push(self.render_item(
                    item,
                    i == self.selected_index,
                    width,
                    primary_column_width,
                ));
            }
        }
        if start > 0 || end < self.filtered_items.len() {
            lines.push(truncate_to_width_with(
                &format!(
                    "  ({}/{})",
                    self.selected_index + 1,
                    self.filtered_items.len()
                ),
                width.saturating_sub(2),
                "",
                false,
            ));
        }
        lines
    }

    fn handle_input(&mut self, data: &str) {
        match parse_key(data).as_deref() {
            Some("up") if !self.filtered_items.is_empty() => {
                self.selected_index = if self.selected_index == 0 {
                    self.filtered_items.len() - 1
                } else {
                    self.selected_index - 1
                };
            }
            Some("down") if !self.filtered_items.is_empty() => {
                self.selected_index = if self.selected_index + 1 >= self.filtered_items.len() {
                    0
                } else {
                    self.selected_index + 1
                };
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingItem {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub current_value: String,
    pub values: Vec<String>,
}

impl SettingItem {
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        current_value: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            description: None,
            current_value: current_value.into(),
            values: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsListTheme {
    pub cursor: String,
    pub hint_prefix: String,
}

impl Default for SettingsListTheme {
    fn default() -> Self {
        Self {
            cursor: "→ ".to_string(),
            hint_prefix: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SettingsListOptions {
    pub enable_search: bool,
}

#[derive(Debug, Clone)]
pub struct SettingsList {
    items: Vec<SettingItem>,
    filtered_items: Vec<SettingItem>,
    theme: SettingsListTheme,
    selected_index: usize,
    max_visible: usize,
    search_enabled: bool,
    search_query: String,
    changes: Vec<(String, String)>,
}

impl SettingsList {
    pub fn new(items: Vec<SettingItem>) -> Self {
        Self::with_options(
            items,
            5,
            SettingsListTheme::default(),
            SettingsListOptions::default(),
        )
    }

    pub fn with_options(
        items: Vec<SettingItem>,
        max_visible: usize,
        theme: SettingsListTheme,
        options: SettingsListOptions,
    ) -> Self {
        Self {
            filtered_items: items.clone(),
            items,
            theme,
            selected_index: 0,
            max_visible: max_visible.max(1),
            search_enabled: options.enable_search,
            search_query: String::new(),
            changes: Vec::new(),
        }
    }

    pub fn update_value(&mut self, id: &str, new_value: impl Into<String>) {
        let new_value = new_value.into();
        for item in &mut self.items {
            if item.id == id {
                item.current_value = new_value.clone();
            }
        }
        for item in &mut self.filtered_items {
            if item.id == id {
                item.current_value = new_value.clone();
            }
        }
    }

    pub fn changes(&self) -> &[(String, String)] {
        &self.changes
    }

    pub fn selected_item(&self) -> Option<&SettingItem> {
        self.display_items().get(self.selected_index)
    }

    fn display_items(&self) -> &[SettingItem] {
        if self.search_enabled {
            &self.filtered_items
        } else {
            &self.items
        }
    }

    fn apply_filter(&mut self) {
        self.filtered_items =
            crate::fuzzy::fuzzy_filter(self.items.clone(), &self.search_query, |item| {
                item.label.clone()
            });
        self.selected_index = 0;
    }

    fn activate_item(&mut self) {
        let Some(item) = self.display_items().get(self.selected_index).cloned() else {
            return;
        };
        if item.values.is_empty() {
            return;
        }
        let current = item
            .values
            .iter()
            .position(|value| value == &item.current_value)
            .unwrap_or(0);
        let new_value = item.values[(current + 1) % item.values.len()].clone();
        self.update_value(&item.id, new_value.clone());
        self.changes.push((item.id, new_value));
    }
}

impl Component for SettingsList {
    fn render(&self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        if self.search_enabled {
            lines.push(truncate_to_width(
                &format!("Search: {}", self.search_query),
                width,
            ));
            lines.push(String::new());
        }

        if self.items.is_empty() {
            lines.push(truncate_to_width("  No settings available", width));
            return lines;
        }
        let display_items = self.display_items();
        if display_items.is_empty() {
            lines.push(truncate_to_width("  No matching settings", width));
            return lines;
        }

        let max_label_width = self
            .items
            .iter()
            .map(|item| visible_width(&item.label))
            .max()
            .unwrap_or(0)
            .min(30);
        let start = self
            .selected_index
            .saturating_sub(self.max_visible / 2)
            .min(display_items.len().saturating_sub(self.max_visible));
        let end = (start + self.max_visible).min(display_items.len());
        for i in start..end {
            let item = &display_items[i];
            let selected = i == self.selected_index;
            let prefix = if selected {
                self.theme.cursor.as_str()
            } else {
                "  "
            };
            let label_padding =
                " ".repeat(max_label_width.saturating_sub(visible_width(&item.label)));
            let used = visible_width(prefix) + max_label_width + 2;
            let value = truncate_to_width_with(
                &item.current_value,
                width.saturating_sub(used + 2),
                "",
                false,
            );
            lines.push(truncate_to_width(
                &format!("{prefix}{}{label_padding}  {value}", item.label),
                width,
            ));
        }
        if start > 0 || end < display_items.len() {
            lines.push(truncate_to_width_with(
                &format!("  ({}/{})", self.selected_index + 1, display_items.len()),
                width.saturating_sub(2),
                "",
                false,
            ));
        }
        if let Some(description) = self
            .selected_item()
            .and_then(|item| item.description.as_ref())
        {
            lines.push(String::new());
            for line in wrap_text(description, width.saturating_sub(4)) {
                lines.push(format!("  {line}"));
            }
        }
        lines.push(String::new());
        lines.push(truncate_to_width(
            if self.search_enabled {
                "  Type to search · Enter/Space to change · Esc to cancel"
            } else {
                "  Enter/Space to change · Esc to cancel"
            },
            width,
        ));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        match parse_key(data).as_deref() {
            Some("up") if !self.display_items().is_empty() => {
                self.selected_index = if self.selected_index == 0 {
                    self.display_items().len() - 1
                } else {
                    self.selected_index - 1
                };
            }
            Some("down") if !self.display_items().is_empty() => {
                self.selected_index = if self.selected_index + 1 >= self.display_items().len() {
                    0
                } else {
                    self.selected_index + 1
                };
            }
            Some("enter") | Some("space") => self.activate_item(),
            _ if self.search_enabled => {
                let text = data.replace(' ', "");
                if !text.is_empty() && text.chars().all(|ch| !ch.is_control()) {
                    self.search_query.push_str(&text);
                    self.apply_filter();
                }
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DefaultTextStyle {
    pub prefix: String,
    pub suffix: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MarkdownTheme {
    pub code: DefaultTextStyle,
    pub link: DefaultTextStyle,
    pub heading: DefaultTextStyle,
    pub quote: DefaultTextStyle,
    pub bold: DefaultTextStyle,
    pub italic: DefaultTextStyle,
    pub strikethrough: DefaultTextStyle,
}

#[derive(Debug, Clone, Default)]
pub struct Markdown {
    text: String,
    padding_x: usize,
    padding_y: usize,
    theme: MarkdownTheme,
    default_text_style: Option<DefaultTextStyle>,
}

impl Markdown {
    pub fn new(
        text: impl Into<String>,
        padding_x: usize,
        padding_y: usize,
        theme: MarkdownTheme,
    ) -> Self {
        Self {
            text: text.into(),
            padding_x,
            padding_y,
            theme,
            default_text_style: None,
        }
    }

    pub fn with_default_style(
        text: impl Into<String>,
        padding_x: usize,
        padding_y: usize,
        theme: MarkdownTheme,
        default_text_style: DefaultTextStyle,
    ) -> Self {
        Self {
            text: text.into(),
            padding_x,
            padding_y,
            theme,
            default_text_style: Some(default_text_style),
        }
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
    }

    fn style(&self, text: String, style: &DefaultTextStyle) -> String {
        format!("{}{}{}", style.prefix, text, style.suffix)
    }

    fn apply_inline_formatting(&self, text: &str) -> String {
        let mut result = String::new();
        let mut chars = text.chars().peekable();
        let mut in_bold = false;
        let mut in_italic = false;
        let mut in_code = false;
        let mut in_strike = false;

        while let Some(ch) = chars.next() {
            if ch == '`' {
                if in_code {
                    result.push_str(&self.theme.code.suffix);
                } else {
                    result.push_str(&self.theme.code.prefix);
                }
                in_code = !in_code;
                continue;
            }
            if !in_code {
                if ch == '*' {
                    if chars.peek() == Some(&'*') {
                        chars.next();
                        if in_bold {
                            result.push_str(&self.theme.bold.suffix);
                        } else {
                            result.push_str(&self.theme.bold.prefix);
                        }
                        in_bold = !in_bold;
                        continue;
                    } else {
                        if in_italic {
                            result.push_str(&self.theme.italic.suffix);
                        } else {
                            result.push_str(&self.theme.italic.prefix);
                        }
                        in_italic = !in_italic;
                        continue;
                    }
                }
                if ch == '~' {
                    if chars.peek() == Some(&'~') {
                        chars.next();
                        if in_strike {
                            result.push_str(&self.theme.strikethrough.suffix);
                        } else {
                            result.push_str(&self.theme.strikethrough.prefix);
                        }
                        in_strike = !in_strike;
                        continue;
                    }
                }
            }
            result.push(ch);
        }

        if in_bold {
            result.push_str(&self.theme.bold.suffix);
        }
        if in_italic {
            result.push_str(&self.theme.italic.suffix);
        }
        if in_code {
            result.push_str(&self.theme.code.suffix);
        }
        if in_strike {
            result.push_str(&self.theme.strikethrough.suffix);
        }

        result
    }

    fn render_markdown_line(&self, line: &str, width: usize, in_code: bool) -> Vec<String> {
        if in_code {
            return wrap_with_continuation_indent(
                &self.style(format!("  {line}"), &self.theme.code),
                width,
                "  ",
            );
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            return vec![String::new()];
        }
        if trimmed == "---" || trimmed == "***" || trimmed == "___" {
            return vec!["─".repeat(width.max(1))];
        }
        if trimmed.starts_with('|') && trimmed.ends_with('|') {
            return vec![truncate_to_width(&render_table_row(trimmed), width)];
        }
        if let Some(stripped) = trimmed.strip_prefix("#") {
            let heading = stripped.trim_start_matches('#').trim();
            return wrap_with_continuation_indent(
                &self.style(heading.to_string(), &self.theme.heading),
                width,
                "",
            );
        }
        if let Some(quote) = trimmed.strip_prefix('>') {
            let rendered = self.style(format!("│ {}", quote.trim()), &self.theme.quote);
            return wrap_with_continuation_indent(&rendered, width, "│ ");
        }
        if let Some((prefix, body)) = markdown_list_parts(line) {
            let rendered = format!("{prefix}{body}");
            let continuation = " ".repeat(visible_width(&prefix));
            return wrap_with_continuation_indent(&rendered, width, &continuation);
        }

        let styled = if let Some(style) = &self.default_text_style {
            self.style(self.apply_inline_formatting(trimmed), style)
        } else {
            self.apply_inline_formatting(trimmed)
        };
        wrap_text(&styled, width)
    }
}

impl Component for Markdown {
    fn render(&self, width: usize) -> Vec<String> {
        if self.text.trim().is_empty() {
            return Vec::new();
        }
        let content_width = width.saturating_sub(self.padding_x * 2).max(1);
        let mut content = Vec::new();
        let mut in_code = false;
        for line in self.text.replace('\t', "   ").lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("```") {
                content.push(trimmed.to_string());
                in_code = !in_code;
                continue;
            }
            content.extend(self.render_markdown_line(line, content_width, in_code));
        }

        let left = " ".repeat(self.padding_x);
        let right = " ".repeat(self.padding_x);
        let empty = " ".repeat(width);
        let mut result = Vec::new();
        for _ in 0..self.padding_y {
            result.push(empty.clone());
        }
        for line in content {
            let mut padded = format!("{left}{line}{right}");
            padded.push_str(&" ".repeat(width.saturating_sub(visible_width(&padded))));
            result.push(padded);
        }
        for _ in 0..self.padding_y {
            result.push(empty.clone());
        }
        result
    }
}

fn markdown_list_parts(line: &str) -> Option<(String, &str)> {
    let leading = line.chars().take_while(|ch| *ch == ' ').count();
    let normalized_indent = " ".repeat(leading * 2);
    let trimmed = line.trim_start();
    if let Some(body) = trimmed.strip_prefix("- ") {
        return Some((format!("{normalized_indent}- "), body));
    }
    if let Some(body) = trimmed.strip_prefix("* ") {
        return Some((format!("{normalized_indent}- "), body));
    }
    let dot = trimmed.find('.')?;
    if trimmed[..dot].chars().all(|ch| ch.is_ascii_digit()) && trimmed[dot + 1..].starts_with(' ') {
        let marker = &trimmed[..=dot];
        return Some((
            format!("{normalized_indent}{marker} "),
            trimmed[dot + 2..].trim_start(),
        ));
    }
    None
}

fn render_table_row(row: &str) -> String {
    let cells = row
        .trim_matches('|')
        .split('|')
        .map(str::trim)
        .collect::<Vec<_>>();
    if cells
        .iter()
        .all(|cell| cell.chars().all(|ch| ch == '-' || ch == ':' || ch == ' '))
    {
        let width = cells.iter().map(|cell| cell.len().max(3)).sum::<usize>()
            + cells.len().saturating_sub(1) * 3;
        return format!("├{}┤", "─".repeat(width));
    }
    format!("│ {} │", cells.join(" │ "))
}

fn wrap_with_continuation_indent(
    line: &str,
    width: usize,
    continuation_indent: &str,
) -> Vec<String> {
    let wrapped = wrap_text(line, width);
    if wrapped.len() <= 1 || continuation_indent.is_empty() {
        return wrapped;
    }
    wrapped
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            if index == 0 {
                line
            } else {
                truncate_to_width(&format!("{continuation_indent}{line}"), width)
            }
        })
        .collect()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageOptions {
    pub max_width_cells: Option<u32>,
    pub max_height_cells: Option<u32>,
    pub filename: Option<String>,
    pub image_id: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageTheme {
    pub fallback_prefix: String,
    pub fallback_suffix: String,
}

#[derive(Debug, Clone)]
pub struct Image {
    base64_data: String,
    mime_type: String,
    dimensions: ImageDimensions,
    theme: ImageTheme,
    options: ImageOptions,
    image_id: Cell<Option<u32>>,
}

impl Image {
    pub fn new(
        base64_data: impl Into<String>,
        mime_type: impl Into<String>,
        theme: ImageTheme,
        options: ImageOptions,
        dimensions: Option<ImageDimensions>,
    ) -> Self {
        let base64_data = base64_data.into();
        let mime_type = mime_type.into();
        let dimensions = dimensions
            .or_else(|| get_image_dimensions(&base64_data, &mime_type))
            .unwrap_or(ImageDimensions {
                width_px: 800,
                height_px: 600,
            });
        let image_id = options.image_id;
        Self {
            base64_data,
            mime_type,
            dimensions,
            theme,
            options,
            image_id: Cell::new(image_id),
        }
    }

    pub fn get_image_id(&self) -> Option<u32> {
        self.image_id.get()
    }
}

impl Component for Image {
    fn render(&self, width: usize) -> Vec<String> {
        let max_width = (width.saturating_sub(2) as u32)
            .max(1)
            .min(self.options.max_width_cells.unwrap_or(60));
        let cell = get_cell_dimensions();
        let default_max_height = ((max_width * cell.width_px.max(1)) as f64
            / cell.height_px.max(1) as f64)
            .ceil()
            .max(1.0) as u32;
        let max_height = self.options.max_height_cells.unwrap_or(default_max_height);
        let caps = get_capabilities();

        if let Some(protocol) = caps.images {
            let image_id = if protocol == ImageProtocol::Kitty {
                let id = self.image_id.get().unwrap_or_else(allocate_image_id);
                self.image_id.set(Some(id));
                Some(id)
            } else {
                self.image_id.get()
            };
            if let Some(result) = render_image(
                &self.base64_data,
                self.dimensions,
                ImageRenderOptions {
                    max_width_cells: Some(max_width),
                    max_height_cells: Some(max_height),
                    image_id,
                    move_cursor: false,
                    preserve_aspect_ratio: true,
                },
            ) {
                if protocol == ImageProtocol::Kitty {
                    let mut lines = vec![result.sequence];
                    lines.extend((1..result.rows).map(|_| String::new()));
                    return lines;
                }

                let mut lines = Vec::new();
                lines.extend((1..result.rows).map(|_| String::new()));
                let row_offset = result.rows.saturating_sub(1);
                let move_up = if row_offset > 0 {
                    format!("\u{1b}[{row_offset}A")
                } else {
                    String::new()
                };
                lines.push(format!("{move_up}{}", result.sequence));
                return lines;
            }
        }

        let fallback = image_fallback(
            &self.mime_type,
            Some(self.dimensions),
            self.options.filename.as_deref(),
        );
        vec![format!(
            "{}{}{}",
            self.theme.fallback_prefix, fallback, self.theme.fallback_suffix
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autocomplete::CombinedAutocompleteProvider;

    #[test]
    fn input_kill_ring_yank_and_yank_pop() {
        let mut input = Input::new();
        input.set_value("first");
        input.handle_input("\u{5}");
        input.handle_input("\u{17}");
        input.set_value("second");
        input.handle_input("\u{5}");
        input.handle_input("\u{17}");
        input.set_value("");

        input.handle_input("\u{19}");
        assert_eq!(input.get_value(), "second");
        input.handle_input("\u{1b}y");
        assert_eq!(input.get_value(), "first");
    }

    #[test]
    fn input_accumulates_consecutive_word_kills() {
        let mut input = Input::new();
        input.set_value("one two three");
        input.handle_input("\u{5}");
        input.handle_input("\u{17}");
        input.handle_input("\u{17}");
        input.handle_input("\u{17}");
        assert_eq!(input.get_value(), "");
        input.handle_input("\u{19}");
        assert_eq!(input.get_value(), "one two three");
    }

    #[test]
    fn input_undo_restores_delete() {
        let mut input = Input::new();
        input.handle_input("h");
        input.handle_input("i");
        input.handle_input("\u{7f}");
        assert_eq!(input.get_value(), "h");
        input.handle_input("\u{1b}[45;5u");
        assert_eq!(input.get_value(), "hi");
    }

    #[test]
    fn input_render_keeps_cursor_visible_for_long_wide_text() {
        let mut input = Input::new();
        input.set_value("가나다라마바사아자차카타파하");
        input.set_focused(true);
        input.handle_input("\u{5}");
        let line = input.render(10).remove(0);
        assert!(line.contains(CURSOR_MARKER));
        assert!(visible_width(&line) <= 10);
    }

    #[test]
    fn editor_navigates_prompt_history() {
        let mut editor = Editor::new(EditorOptions::default());
        editor.add_to_history("first");
        editor.add_to_history("second");
        editor.handle_input("\u{1b}[A");
        assert_eq!(editor.get_text(), "second");
        editor.handle_input("\u{1b}[A");
        assert_eq!(editor.get_text(), "first");
        editor.handle_input("\u{1b}[B");
        assert_eq!(editor.get_text(), "second");
        editor.handle_input("\u{1b}[B");
        assert_eq!(editor.get_text(), "");
    }

    #[test]
    fn editor_submits_trimmed_value_and_clears() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let submitted = Rc::new(RefCell::new(None));
        let changed = Rc::new(RefCell::new(Vec::new()));
        let mut editor = Editor::new(EditorOptions::default());
        let submitted_ref = Rc::clone(&submitted);
        editor.set_on_submit(move |text| *submitted_ref.borrow_mut() = Some(text));
        let changed_ref = Rc::clone(&changed);
        editor.set_on_change(move |text| changed_ref.borrow_mut().push(text));
        editor.set_text("  hello\r\nworld  ");
        editor.handle_input("\r");
        assert_eq!(submitted.borrow().as_deref(), Some("hello\nworld"));
        assert_eq!(editor.get_text(), "");
        assert_eq!(changed.borrow().last().map(String::as_str), Some(""));
    }

    #[test]
    fn editor_moves_vertically_before_history_and_limits_history() {
        let mut editor = Editor::new(EditorOptions::default());
        for index in 0..105 {
            editor.add_to_history(format!("prompt {index}"));
        }
        assert_eq!(
            editor.history().first().map(String::as_str),
            Some("prompt 5")
        );
        assert_eq!(editor.history().len(), 100);

        editor.set_text("line1\nline2");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 1, col: 5 });
        editor.handle_input("\u{1b}[A");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 5 });
        editor.handle_input("X");
        assert_eq!(editor.get_text(), "line1X\nline2");

        editor.set_text("");
        for _ in 0..100 {
            editor.handle_input("\u{1b}[A");
        }
        assert_eq!(editor.get_text(), "prompt 5");
    }

    #[test]
    fn editor_handles_backslash_enter_and_printable_key_sequences() {
        let mut editor = Editor::new(EditorOptions::default());
        editor.handle_input("\\");
        editor.handle_input("\r");
        assert_eq!(editor.get_text(), "\n");

        editor.set_text("");
        editor.handle_input("\u{1b}[69;2u");
        editor.handle_input("\u{1b}[27;2;70~");
        assert_eq!(editor.get_text(), "EF");

        editor.set_text("a\tb");
        assert_eq!(editor.get_text(), "a    b");
    }

    #[test]
    fn editor_deletes_moves_words_and_yanks_like_ts_editor() {
        let mut editor = Editor::new(EditorOptions::default());
        editor.set_text("hello world");
        editor.handle_input("\u{1}");
        for _ in 0..6 {
            editor.handle_input("\u{1b}[C");
        }
        editor.handle_input("\u{b}");
        assert_eq!(editor.get_text(), "hello ");
        editor.handle_input("\u{19}");
        assert_eq!(editor.get_text(), "hello world");

        editor.set_text("abc\ndef");
        editor.handle_input("\u{1}");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 1, col: 0 });
        editor.handle_input("\u{5}");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 1, col: 3 });
        editor.handle_input("\u{1}");
        editor.handle_input("\u{4}");
        assert_eq!(editor.get_text(), "abc\nef");

        editor.set_text("one two three");
        editor.handle_input("\u{1}");
        editor.handle_input("\u{1b}f");
        assert_eq!(editor.get_cursor(), EditorCursor { line: 0, col: 3 });
        editor.handle_input("\u{1b}d");
        assert_eq!(editor.get_text(), "one three");
    }

    #[test]
    fn editor_undo_coalesces_words_and_restores_edits() {
        let mut editor = Editor::new(EditorOptions::default());
        for ch in "hello world".chars() {
            editor.handle_input(&ch.to_string());
        }
        assert_eq!(editor.get_text(), "hello world");
        editor.handle_input("\u{1b}[45;5u");
        assert_eq!(editor.get_text(), "hello");
        editor.handle_input("\u{1b}[45;5u");
        assert_eq!(editor.get_text(), "");

        editor.set_text("abc");
        editor.handle_input("\u{7f}");
        assert_eq!(editor.get_text(), "ab");
        editor.handle_input("\u{1b}[45;5u");
        assert_eq!(editor.get_text(), "abc");
    }

    #[test]
    fn editor_handles_bracketed_paste_markers_and_submit_expansion() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let mut editor = Editor::new(EditorOptions::default());
        editor.set_text("open");
        editor.handle_input("\u{1b}[200~/tmp/file\u{1b}[201~");
        assert_eq!(editor.get_text(), "open /tmp/file");
        editor.handle_input("\u{1b}[45;5u");
        assert_eq!(editor.get_text(), "open");

        editor.set_text("");
        editor.handle_input("\u{1b}[200~line1\u{1b}[106;5uline2\u{1b}[201~");
        assert_eq!(editor.get_text(), "line1\nline2");

        let submitted = Rc::new(RefCell::new(None));
        let submitted_ref = Rc::clone(&submitted);
        editor.set_on_submit(move |text| *submitted_ref.borrow_mut() = Some(text));
        let large = (0..11)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        editor.set_text("");
        editor.handle_input(&format!("\u{1b}[200~{large}\u{1b}[201~"));
        assert_eq!(editor.paste_ids(), vec![1]);
        assert_eq!(editor.get_text(), "[paste #1 +11 lines]");
        editor.handle_input("\r");
        assert_eq!(submitted.borrow().as_deref(), Some(large.as_str()));
        assert!(editor.paste_ids().is_empty());
    }

    #[test]
    fn editor_triggers_and_applies_autocomplete() {
        let provider = CombinedAutocompleteProvider::new(
            vec![crate::autocomplete::SlashCommand::new("help")],
            ".",
        );
        let mut editor = Editor::new(EditorOptions::default());
        editor.set_autocomplete_provider(provider);

        editor.handle_input("/");
        editor.handle_input("h");
        editor.handle_input("e");
        assert!(editor.is_showing_autocomplete());

        editor.handle_input("\t"); // Applies first suggestion
        assert!(!editor.is_showing_autocomplete());
        assert_eq!(editor.get_text(), "/help ");

        // Tab triggers it again
        editor.set_text("src/");
        editor.handle_input("\t"); // Force triggers file suggestion
        assert!(
            editor.is_showing_autocomplete(),
            "Tab should trigger file autocomplete"
        );
        editor.handle_input("\u{1b}"); // Escape cancels it
        assert!(!editor.is_showing_autocomplete());
    }

    #[test]
    fn word_wrap_line_tracks_original_indices() {
        let chunks = word_wrap_line("alpha beta gamma", 10);
        assert_eq!(chunks[0].text, "alpha ");
        assert_eq!(chunks[0].start_index, 0);
        assert_eq!(chunks[0].end_index, 6);
        assert_eq!(chunks[1].text, "beta gamma");
        assert_eq!(chunks[1].start_index, 6);
        assert_eq!(chunks[1].end_index, "alpha beta gamma".len());
    }

    #[test]
    fn cancellable_loader_aborts_on_cancel() {
        let mut loader = CancellableLoader::new("Working...");
        loader.handle_input("\u{1b}");
        assert!(loader.aborted());
    }

    #[test]
    fn image_places_kitty_sequence_on_first_line() {
        crate::terminal_image::set_capabilities(crate::terminal_image::TerminalCapabilities {
            images: Some(crate::terminal_image::ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        });
        crate::terminal_image::set_cell_dimensions(crate::terminal_image::CellDimensions {
            width_px: 10,
            height_px: 10,
        });
        let image = Image::new(
            "AAAA",
            "image/png",
            ImageTheme::default(),
            ImageOptions {
                max_width_cells: Some(2),
                ..ImageOptions::default()
            },
            Some(ImageDimensions {
                width_px: 20,
                height_px: 20,
            }),
        );
        let lines = image.render(4);
        let image_id = image.get_image_id().expect("kitty image id allocated");
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("\u{1b}_G"));
        assert!(lines[0].contains(",C=1,"));
        assert!(lines[0].contains(&format!(",i={image_id}")));
        assert_eq!(lines[1], "");
        crate::terminal_image::reset_capabilities_cache();
        crate::terminal_image::set_cell_dimensions(crate::terminal_image::CellDimensions {
            width_px: 9,
            height_px: 18,
        });
    }

    #[test]
    fn select_list_aligns_descriptions_and_wraps_navigation() {
        let items = vec![
            SelectItem {
                value: "short".into(),
                label: "short".into(),
                description: Some("Line one\nLine two".into()),
            },
            SelectItem {
                value: "long".into(),
                label: "very-long-command-name-that-needs-truncation".into(),
                description: Some("second".into()),
            },
        ];
        let mut list = SelectList::with_options(
            items,
            5,
            SelectListTheme::default(),
            SelectListLayoutOptions {
                min_primary_column_width: Some(12),
                max_primary_column_width: Some(20),
                ..SelectListLayoutOptions::default()
            },
        );
        let rendered = list.render(80);
        assert!(rendered[0].contains("Line one Line two"));
        let first_desc = visible_width(&rendered[0][..rendered[0].find("Line one").unwrap()]);
        let second_desc = visible_width(&rendered[1][..rendered[1].find("second").unwrap()]);
        assert_eq!(first_desc, second_desc);
        list.handle_input("\u{1b}[A");
        assert_eq!(list.get_selected_item().unwrap().value, "long");
        list.handle_input("\u{1b}[B");
        assert_eq!(list.get_selected_item().unwrap().value, "short");
    }

    #[test]
    fn settings_list_cycles_values_and_filters() {
        let mut list = SettingsList::with_options(
            vec![
                SettingItem {
                    id: "theme".into(),
                    label: "Theme".into(),
                    description: Some("Choose theme".into()),
                    current_value: "dark".into(),
                    values: vec!["dark".into(), "light".into()],
                },
                SettingItem::new("model", "Model", "fast"),
            ],
            5,
            SettingsListTheme::default(),
            SettingsListOptions {
                enable_search: true,
            },
        );
        list.handle_input("t");
        assert_eq!(list.selected_item().unwrap().id, "theme");
        list.handle_input("\r");
        assert_eq!(list.selected_item().unwrap().current_value, "light");
        assert_eq!(list.changes(), &[("theme".into(), "light".into())]);
    }

    #[test]
    fn markdown_renders_nested_lists_tables_and_blockquotes() {
        let markdown = Markdown::new(
            "- Item 1\n  - Nested 1.1\n1. Ordered\n> quote text\n| Name | Age |\n| --- | --- |\n| Bob | 25 |",
            0,
            0,
            MarkdownTheme::default(),
        );
        let lines = markdown.render(80);
        assert!(lines.iter().any(|line| line.contains("- Item 1")));
        assert!(lines.iter().any(|line| line.contains("    - Nested 1.1")));
        assert!(lines.iter().any(|line| line.contains("1. Ordered")));
        assert!(lines.iter().any(|line| line.contains("│ quote text")));
        assert!(lines.iter().any(|line| line.contains("│ Name │ Age │")));
        assert!(lines.iter().any(|line| line.contains("─")));
    }

    #[test]
    fn markdown_renders_inline_formatting() {
        let theme = MarkdownTheme {
            bold: DefaultTextStyle {
                prefix: "<B>".into(),
                suffix: "</B>".into(),
            },
            italic: DefaultTextStyle {
                prefix: "<I>".into(),
                suffix: "</I>".into(),
            },
            code: DefaultTextStyle {
                prefix: "<C>".into(),
                suffix: "</C>".into(),
            },
            strikethrough: DefaultTextStyle {
                prefix: "<S>".into(),
                suffix: "</S>".into(),
            },
            ..MarkdownTheme::default()
        };
        let markdown = Markdown::new(
            "This is **bold** and *italic* and `code` and ~~strike~~!",
            0,
            0,
            theme,
        );
        let lines = markdown.render(80);
        assert_eq!(
            lines[0].trim_end(),
            "This is <B>bold</B> and <I>italic</I> and <C>code</C> and <S>strike</S>!"
        );
    }
}
