//! Port of packages/tui/src/components/input.ts

#![allow(dead_code, unused_imports, unused_variables)]

use std::sync::{Arc, Mutex, MutexGuard};

use crate::keybindings::get_keybindings;
use crate::keys::decode_kitty_printable;
use crate::kill_ring::{KillRing, KillRingPushOptions};
use crate::tui::{
    CURSOR_MARKER, Component, Focusable, TuiMouseButton, TuiMouseEvent, TuiMouseEventResult, TuiMouseEventType,
    render_fake_cursor,
};
use crate::undo_stack::UndoStack;
use crate::utils::{get_grapheme_segmenter, is_whitespace_char, slice_by_column, truncate_to_width, visible_width};
use crate::word_navigation::{find_word_backward, find_word_forward};

pub type InputSubmitCallback = Arc<dyn Fn(&str) + Send + Sync>;
pub type InputEscapeCallback = Arc<dyn Fn() + Send + Sync>;
pub type InputPlaceholderStyle = Arc<dyn Fn(&str) -> String + Send + Sync>;

#[derive(Clone, Default)]
pub struct InputOptions {
    pub prompt: Option<String>,
    pub placeholder: Option<String>,
    pub placeholder_style: Option<InputPlaceholderStyle>,
}

/// `{ value, cursor }` undo snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
struct InputState {
    value: String,
    cursor: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LastAction {
    /// `"kill"`
    Kill,
    /// `"yank"`
    Yank,
    /// `"type-word"`
    TypeWord,
}

struct InputFields {
    value: String,
    /// Cursor position in the value.
    cursor: i64,
    prompt: String,
    placeholder: String,
    placeholder_style: InputPlaceholderStyle,
    on_submit: Option<InputSubmitCallback>,
    on_escape: Option<InputEscapeCallback>,
    /// Focusable interface - set by TUI when focus changes.
    focused: bool,
    /// Bracketed paste mode buffering.
    paste_buffer: String,
    is_in_paste: bool,
    /// Kill ring for Emacs-style kill/yank operations.
    kill_ring: KillRing,
    last_action: Option<LastAction>,
    /// Undo support.
    undo_stack: UndoStack<InputState>,
    rendered_start_column: i64,
}

struct InputInner {
    fields: Mutex<InputFields>,
}

/// Input component - single-line text input with horizontal scrolling.
///
/// PORT: TS class with identity. Handle so containers can store `Arc<dyn Component>`.
#[derive(Clone)]
pub struct Input {
    inner: Arc<InputInner>,
}

impl Input {
    fn fields(&self) -> MutexGuard<'_, InputFields> {
        self.inner.fields.lock().unwrap()
    }

    /// `options` `None` is the TS default `{}`.
    ///
    /// Defaults applied inside: `prompt` `"> "`, `placeholder` `""`, `placeholderStyle` identity.
    pub fn new(options: Option<InputOptions>) -> Self {
        let opts = options.unwrap_or_default();
        Self {
            inner: Arc::new(InputInner {
                fields: Mutex::new(InputFields {
                    value: String::new(),
                    cursor: 0,
                    prompt: opts.prompt.unwrap_or_else(|| "> ".to_string()),
                    placeholder: opts.placeholder.unwrap_or_default(),
                    placeholder_style: opts
                        .placeholder_style
                        .unwrap_or_else(|| Arc::new(|text: &str| text.to_string())),
                    on_submit: None,
                    on_escape: None,
                    focused: false,
                    paste_buffer: String::new(),
                    is_in_paste: false,
                    kill_ring: KillRing::new(),
                    last_action: None,
                    undo_stack: UndoStack::new(),
                    rendered_start_column: 0,
                }),
            }),
        }
    }

    pub fn get_value(&self) -> String {
        self.fields().value.clone()
    }

    pub fn set_value(&self, value: &str) {
        let mut fields = self.fields();
        fields.value = value.to_string();
        fields.cursor = fields.cursor.min(pi_js::str16::len(value) as i64);
    }

    pub fn on_submit(&self) -> Option<InputSubmitCallback> {
        self.fields().on_submit.clone()
    }

    pub fn set_on_submit(&self, on_submit: Option<InputSubmitCallback>) {
        self.fields().on_submit = on_submit;
    }

    pub fn on_escape(&self) -> Option<InputEscapeCallback> {
        self.fields().on_escape.clone()
    }

    pub fn set_on_escape(&self, on_escape: Option<InputEscapeCallback>) {
        self.fields().on_escape = on_escape;
    }

    /// Focusable `focused` flag.
    pub fn focused(&self) -> bool {
        self.fields().focused
    }

    pub fn set_focused(&self, focused: bool) {
        self.fields().focused = focused;
    }

    pub fn handle_input(&self, data: &str) {
        enum PendingCallback {
            None,
            Escape(InputEscapeCallback),
            Submit(InputSubmitCallback, String),
        }

        let mut remaining_after_paste: Option<String> = None;
        let pending_cb = {
            let mut fields = self.fields();
            let mut chunk = data.to_string();

            if let Some(pos) = chunk.find("\x1b[200~") {
                fields.is_in_paste = true;
                fields.paste_buffer.clear();
                chunk.replace_range(pos..pos + 6, "");
            }

            if fields.is_in_paste {
                fields.paste_buffer.push_str(&chunk);
                if let Some(end_index) = fields.paste_buffer.find("\x1b[201~") {
                    let paste_content = fields.paste_buffer[..end_index].to_string();
                    Self::handle_paste_locked(&mut fields, &paste_content);
                    fields.is_in_paste = false;
                    let remaining = fields.paste_buffer[end_index + 6..].to_string();
                    fields.paste_buffer.clear();
                    if !remaining.is_empty() {
                        remaining_after_paste = Some(remaining);
                    }
                }
                PendingCallback::None
            } else {
                let kb = get_keybindings();
                if kb.matches(&chunk, "tui.select.cancel") {
                    match &fields.on_escape {
                        Some(cb) => PendingCallback::Escape(Arc::clone(cb)),
                        None => PendingCallback::None,
                    }
                } else if kb.matches(&chunk, "tui.editor.undo") {
                    Self::undo_locked(&mut fields);
                    PendingCallback::None
                } else if kb.matches(&chunk, "tui.input.submit") || chunk == "\n" {
                    match &fields.on_submit {
                        Some(cb) => PendingCallback::Submit(Arc::clone(cb), fields.value.clone()),
                        None => PendingCallback::None,
                    }
                } else if kb.matches(&chunk, "tui.editor.deleteCharBackward") {
                    Self::handle_backspace_locked(&mut fields);
                    PendingCallback::None
                } else if kb.matches(&chunk, "tui.editor.deleteCharForward") {
                    Self::handle_forward_delete_locked(&mut fields);
                    PendingCallback::None
                } else if kb.matches(&chunk, "tui.editor.deleteWordBackward") {
                    Self::delete_word_backwards_locked(&mut fields);
                    PendingCallback::None
                } else if kb.matches(&chunk, "tui.editor.deleteWordForward") {
                    Self::delete_word_forward_locked(&mut fields);
                    PendingCallback::None
                } else if kb.matches(&chunk, "tui.editor.deleteToLineStart") {
                    Self::delete_to_line_start_locked(&mut fields);
                    PendingCallback::None
                } else if kb.matches(&chunk, "tui.editor.deleteToLineEnd") {
                    Self::delete_to_line_end_locked(&mut fields);
                    PendingCallback::None
                } else if kb.matches(&chunk, "tui.editor.yank") {
                    Self::yank_locked(&mut fields);
                    PendingCallback::None
                } else if kb.matches(&chunk, "tui.editor.yankPop") {
                    Self::yank_pop_locked(&mut fields);
                    PendingCallback::None
                } else if kb.matches(&chunk, "tui.editor.cursorLeft") {
                    fields.last_action = None;
                    if fields.cursor > 0 {
                        let before = pi_js::str16::slice(&fields.value, 0, Some(fields.cursor));
                        let graphemes = get_grapheme_segmenter().segment(&before);
                        let delta = graphemes
                            .last()
                            .map(|g| pi_js::str16::len(&g.segment) as i64)
                            .unwrap_or(1);
                        fields.cursor -= delta;
                    }
                    PendingCallback::None
                } else if kb.matches(&chunk, "tui.editor.cursorRight") {
                    fields.last_action = None;
                    let total_len = pi_js::str16::len(&fields.value) as i64;
                    if fields.cursor < total_len {
                        let after = pi_js::str16::slice(&fields.value, fields.cursor, None);
                        let graphemes = get_grapheme_segmenter().segment(&after);
                        let delta = graphemes
                            .first()
                            .map(|g| pi_js::str16::len(&g.segment) as i64)
                            .unwrap_or(1);
                        fields.cursor += delta;
                    }
                    PendingCallback::None
                } else if kb.matches(&chunk, "tui.editor.cursorLineStart") {
                    fields.last_action = None;
                    fields.cursor = 0;
                    PendingCallback::None
                } else if kb.matches(&chunk, "tui.editor.cursorLineEnd") {
                    fields.last_action = None;
                    fields.cursor = pi_js::str16::len(&fields.value) as i64;
                    PendingCallback::None
                } else if kb.matches(&chunk, "tui.editor.cursorWordLeft") {
                    Self::move_word_backwards_locked(&mut fields);
                    PendingCallback::None
                } else if kb.matches(&chunk, "tui.editor.cursorWordRight") {
                    Self::move_word_forwards_locked(&mut fields);
                    PendingCallback::None
                } else if let Some(kitty_printable) = decode_kitty_printable(&chunk) {
                    Self::insert_character_locked(&mut fields, &kitty_printable);
                    PendingCallback::None
                } else {
                    let has_control_chars = chunk.chars().any(|ch| {
                        let code = ch as u32;
                        code < 32 || code == 0x7f || (0x80..=0x9f).contains(&code)
                    });
                    if !has_control_chars {
                        Self::insert_character_locked(&mut fields, &chunk);
                    }
                    PendingCallback::None
                }
            }
        };

        if let Some(remaining) = remaining_after_paste {
            self.handle_input(&remaining);
        }
        match pending_cb {
            PendingCallback::None => {}
            PendingCallback::Escape(cb) => cb(),
            PendingCallback::Submit(cb, val) => cb(&val),
        }
    }

    pub fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        if event.r#type != TuiMouseEventType::Press || event.button != TuiMouseButton::Left || event.y != 0 {
            return None;
        }
        let mut fields = self.fields();
        let visible_column = (event.x - 2).max(0);
        let target_column = fields.rendered_start_column + visible_column;
        let mut current_column = 0i64;
        fields.cursor = pi_js::str16::len(&fields.value) as i64;
        for grapheme in get_grapheme_segmenter().segment(&fields.value) {
            let next_column = current_column + visible_width(&grapheme.segment);
            if target_column < next_column {
                fields.cursor = grapheme.index;
                break;
            }
            current_column = next_column;
        }
        fields.last_action = None;
        Some(TuiMouseEventResult {
            handled: Some(true),
            focus: Some(true),
            ..Default::default()
        })
    }

    fn insert_character_locked(fields: &mut InputFields, ch: &str) {
        if is_whitespace_char(ch) || fields.last_action != Some(LastAction::TypeWord) {
            Self::push_undo_locked(fields);
        }
        fields.last_action = Some(LastAction::TypeWord);
        let before = pi_js::str16::slice(&fields.value, 0, Some(fields.cursor));
        let after = pi_js::str16::slice(&fields.value, fields.cursor, None);
        fields.value = format!("{before}{ch}{after}");
        fields.cursor += pi_js::str16::len(ch) as i64;
    }

    fn insert_character(&self, ch: &str) {
        Self::insert_character_locked(&mut self.fields(), ch);
    }

    fn handle_backspace_locked(fields: &mut InputFields) {
        fields.last_action = None;
        if fields.cursor > 0 {
            Self::push_undo_locked(fields);
            let before = pi_js::str16::slice(&fields.value, 0, Some(fields.cursor));
            let graphemes = get_grapheme_segmenter().segment(&before);
            let grapheme_len = graphemes
                .last()
                .map(|g| pi_js::str16::len(&g.segment) as i64)
                .unwrap_or(1);
            let head = pi_js::str16::slice(&fields.value, 0, Some(fields.cursor - grapheme_len));
            let tail = pi_js::str16::slice(&fields.value, fields.cursor, None);
            fields.value = format!("{head}{tail}");
            fields.cursor -= grapheme_len;
        }
    }

    fn handle_backspace(&self) {
        Self::handle_backspace_locked(&mut self.fields());
    }

    fn handle_forward_delete_locked(fields: &mut InputFields) {
        fields.last_action = None;
        let total_len = pi_js::str16::len(&fields.value) as i64;
        if fields.cursor < total_len {
            Self::push_undo_locked(fields);
            let after = pi_js::str16::slice(&fields.value, fields.cursor, None);
            let graphemes = get_grapheme_segmenter().segment(&after);
            let grapheme_len = graphemes
                .first()
                .map(|g| pi_js::str16::len(&g.segment) as i64)
                .unwrap_or(1);
            let head = pi_js::str16::slice(&fields.value, 0, Some(fields.cursor));
            let tail = pi_js::str16::slice(&fields.value, fields.cursor + grapheme_len, None);
            fields.value = format!("{head}{tail}");
        }
    }

    fn handle_forward_delete(&self) {
        Self::handle_forward_delete_locked(&mut self.fields());
    }

    fn delete_to_line_start_locked(fields: &mut InputFields) {
        if fields.cursor == 0 {
            return;
        }
        Self::push_undo_locked(fields);
        let deleted_text = pi_js::str16::slice(&fields.value, 0, Some(fields.cursor));
        fields.kill_ring.push(
            &deleted_text,
            KillRingPushOptions {
                prepend: true,
                accumulate: Some(fields.last_action == Some(LastAction::Kill)),
            },
        );
        fields.last_action = Some(LastAction::Kill);
        fields.value = pi_js::str16::slice(&fields.value, fields.cursor, None);
        fields.cursor = 0;
    }

    fn delete_to_line_start(&self) {
        Self::delete_to_line_start_locked(&mut self.fields());
    }

    fn delete_to_line_end_locked(fields: &mut InputFields) {
        let total_len = pi_js::str16::len(&fields.value) as i64;
        if fields.cursor >= total_len {
            return;
        }
        Self::push_undo_locked(fields);
        let deleted_text = pi_js::str16::slice(&fields.value, fields.cursor, None);
        fields.kill_ring.push(
            &deleted_text,
            KillRingPushOptions {
                prepend: false,
                accumulate: Some(fields.last_action == Some(LastAction::Kill)),
            },
        );
        fields.last_action = Some(LastAction::Kill);
        fields.value = pi_js::str16::slice(&fields.value, 0, Some(fields.cursor));
    }

    fn delete_to_line_end(&self) {
        Self::delete_to_line_end_locked(&mut self.fields());
    }

    fn delete_word_backwards_locked(fields: &mut InputFields) {
        if fields.cursor == 0 {
            return;
        }
        let was_kill = fields.last_action == Some(LastAction::Kill);
        Self::push_undo_locked(fields);
        let old_cursor = fields.cursor;
        Self::move_word_backwards_locked(fields);
        let delete_from = fields.cursor;
        fields.cursor = old_cursor;
        let deleted_text = pi_js::str16::slice(&fields.value, delete_from, Some(fields.cursor));
        fields.kill_ring.push(
            &deleted_text,
            KillRingPushOptions {
                prepend: true,
                accumulate: Some(was_kill),
            },
        );
        fields.last_action = Some(LastAction::Kill);
        let head = pi_js::str16::slice(&fields.value, 0, Some(delete_from));
        let tail = pi_js::str16::slice(&fields.value, fields.cursor, None);
        fields.value = format!("{head}{tail}");
        fields.cursor = delete_from;
    }

    fn delete_word_backwards(&self) {
        Self::delete_word_backwards_locked(&mut self.fields());
    }

    fn delete_word_forward_locked(fields: &mut InputFields) {
        let total_len = pi_js::str16::len(&fields.value) as i64;
        if fields.cursor >= total_len {
            return;
        }
        let was_kill = fields.last_action == Some(LastAction::Kill);
        Self::push_undo_locked(fields);
        let old_cursor = fields.cursor;
        Self::move_word_forwards_locked(fields);
        let delete_to = fields.cursor;
        fields.cursor = old_cursor;
        let deleted_text = pi_js::str16::slice(&fields.value, fields.cursor, Some(delete_to));
        fields.kill_ring.push(
            &deleted_text,
            KillRingPushOptions {
                prepend: false,
                accumulate: Some(was_kill),
            },
        );
        fields.last_action = Some(LastAction::Kill);
        let head = pi_js::str16::slice(&fields.value, 0, Some(fields.cursor));
        let tail = pi_js::str16::slice(&fields.value, delete_to, None);
        fields.value = format!("{head}{tail}");
    }

    fn delete_word_forward(&self) {
        Self::delete_word_forward_locked(&mut self.fields());
    }

    fn yank_locked(fields: &mut InputFields) {
        let Some(text) = fields.kill_ring.peek() else {
            return;
        };
        if text.is_empty() {
            return;
        }
        Self::push_undo_locked(fields);
        let before = pi_js::str16::slice(&fields.value, 0, Some(fields.cursor));
        let after = pi_js::str16::slice(&fields.value, fields.cursor, None);
        fields.value = format!("{before}{text}{after}");
        fields.cursor += pi_js::str16::len(&text) as i64;
        fields.last_action = Some(LastAction::Yank);
    }

    fn yank(&self) {
        Self::yank_locked(&mut self.fields());
    }

    fn yank_pop_locked(fields: &mut InputFields) {
        if fields.last_action != Some(LastAction::Yank) || fields.kill_ring.length() <= 1 {
            return;
        }
        Self::push_undo_locked(fields);
        let prev_text = fields.kill_ring.peek().unwrap_or_default();
        let prev_len = pi_js::str16::len(&prev_text) as i64;
        let head = pi_js::str16::slice(&fields.value, 0, Some((fields.cursor - prev_len).max(0)));
        let tail = pi_js::str16::slice(&fields.value, fields.cursor, None);
        fields.value = format!("{head}{tail}");
        fields.cursor -= prev_len;

        fields.kill_ring.rotate();
        let text = fields.kill_ring.peek().unwrap_or_default();
        let before = pi_js::str16::slice(&fields.value, 0, Some(fields.cursor));
        let after = pi_js::str16::slice(&fields.value, fields.cursor, None);
        fields.value = format!("{before}{text}{after}");
        fields.cursor += pi_js::str16::len(&text) as i64;
        fields.last_action = Some(LastAction::Yank);
    }

    fn yank_pop(&self) {
        Self::yank_pop_locked(&mut self.fields());
    }

    fn push_undo_locked(fields: &mut InputFields) {
        fields.undo_stack.push(&InputState {
            value: fields.value.clone(),
            cursor: fields.cursor,
        });
    }

    fn push_undo(&self) {
        Self::push_undo_locked(&mut self.fields());
    }

    fn undo_locked(fields: &mut InputFields) {
        let Some(snapshot) = fields.undo_stack.pop() else {
            return;
        };
        fields.value = snapshot.value;
        fields.cursor = snapshot.cursor;
        fields.last_action = None;
    }

    fn undo(&self) {
        Self::undo_locked(&mut self.fields());
    }

    fn move_word_backwards_locked(fields: &mut InputFields) {
        if fields.cursor == 0 {
            return;
        }
        fields.last_action = None;
        fields.cursor = find_word_backward(&fields.value, fields.cursor, None);
    }

    fn move_word_backwards(&self) {
        Self::move_word_backwards_locked(&mut self.fields());
    }

    fn move_word_forwards_locked(fields: &mut InputFields) {
        let total_len = pi_js::str16::len(&fields.value) as i64;
        if fields.cursor >= total_len {
            return;
        }
        fields.last_action = None;
        fields.cursor = find_word_forward(&fields.value, fields.cursor, None);
    }

    fn move_word_forwards(&self) {
        Self::move_word_forwards_locked(&mut self.fields());
    }

    fn handle_paste_locked(fields: &mut InputFields, pasted_text: &str) {
        fields.last_action = None;
        Self::push_undo_locked(fields);
        let clean_text = pasted_text
            .replace("\r\n", "")
            .replace('\r', "")
            .replace('\n', "")
            .replace('\t', "    ");
        let before = pi_js::str16::slice(&fields.value, 0, Some(fields.cursor));
        let after = pi_js::str16::slice(&fields.value, fields.cursor, None);
        fields.value = format!("{before}{clean_text}{after}");
        fields.cursor += pi_js::str16::len(&clean_text) as i64;
    }

    fn handle_paste(&self, pasted_text: &str) {
        Self::handle_paste_locked(&mut self.fields(), pasted_text);
    }

    /// No cached state to invalidate currently.
    pub fn invalidate(&self) {}

    pub fn render(&self, width: usize) -> Vec<String> {
        let (value, cursor, prompt, placeholder, placeholder_style, focused) = {
            let fields = self.fields();
            (
                fields.value.clone(),
                fields.cursor,
                fields.prompt.clone(),
                fields.placeholder.clone(),
                Arc::clone(&fields.placeholder_style),
                fields.focused,
            )
        };

        let available_width = width as i64 - visible_width(&prompt);
        if available_width <= 0 {
            return vec![truncate_to_width(&prompt, width as i64, Some(""), None)];
        }

        if value.is_empty() && !placeholder.is_empty() {
            let truncated = truncate_to_width(&placeholder, available_width, Some(""), None);
            let graphemes = get_grapheme_segmenter().segment(&truncated);
            let at_cursor = graphemes.first().map(|g| g.segment.as_str()).unwrap_or(" ");
            let after_cursor = pi_js::str16::slice(&truncated, pi_js::str16::len(at_cursor) as i64, None);
            let marker = if focused { CURSOR_MARKER } else { "" };
            let cursor_char = render_fake_cursor(&placeholder_style(at_cursor));
            let text_with_cursor = format!("{marker}{cursor_char}{}", placeholder_style(&after_cursor));
            let padding = " ".repeat((available_width - visible_width(&text_with_cursor)).max(0) as usize);
            return vec![format!("{prompt}{text_with_cursor}{padding}")];
        }

        let visible_text: String;
        let mut cursor_display = cursor;
        let mut rendered_start_column = 0i64;
        let total_width = visible_width(&value);
        let value_len = pi_js::str16::len(&value) as i64;

        if total_width < available_width {
            visible_text = value;
        } else {
            let scroll_width = if cursor == value_len {
                available_width - 1
            } else {
                available_width
            };
            let cursor_col = visible_width(&pi_js::str16::slice(&value, 0, Some(cursor)));

            if scroll_width > 0 {
                let half_width = scroll_width.div_euclid(2);
                let start_col = if cursor_col < half_width {
                    0
                } else if cursor_col > total_width - half_width {
                    (total_width - scroll_width).max(0)
                } else {
                    (cursor_col - half_width).max(0)
                };

                rendered_start_column = start_col;
                visible_text = slice_by_column(&value, start_col, scroll_width, Some(true));
                let before_cursor = slice_by_column(&value, start_col, (cursor_col - start_col).max(0), Some(true));
                cursor_display = pi_js::str16::len(&before_cursor) as i64;
            } else {
                visible_text = String::new();
                cursor_display = 0;
            }
        }

        {
            self.fields().rendered_start_column = rendered_start_column;
        }

        let slice_after_cursor = pi_js::str16::slice(&visible_text, cursor_display, None);
        let graphemes = get_grapheme_segmenter().segment(&slice_after_cursor);
        let at_cursor = graphemes.first().map(|g| g.segment.as_str()).unwrap_or(" ");
        let before_cursor = pi_js::str16::slice(&visible_text, 0, Some(cursor_display));
        let after_cursor = pi_js::str16::slice(
            &visible_text,
            cursor_display + pi_js::str16::len(at_cursor) as i64,
            None,
        );
        let marker = if focused { CURSOR_MARKER } else { "" };
        let cursor_char = render_fake_cursor(at_cursor);
        let text_with_cursor = format!("{before_cursor}{marker}{cursor_char}{after_cursor}");

        let visual_length = visible_width(&text_with_cursor);
        let padding = " ".repeat((available_width - visual_length).max(0) as usize);
        vec![format!("{prompt}{text_with_cursor}{padding}")]
    }
}

impl Component for Input {
    fn render(&self, width: usize) -> Vec<String> {
        Input::render(self, width)
    }

    fn handle_input(&self, data: &str) {
        Input::handle_input(self, data)
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        Input::handle_mouse(self, event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        Input::invalidate(self)
    }

    fn has_handle_input(&self) -> bool {
        true
    }

    fn as_focusable(&self) -> Option<&dyn Focusable> {
        Some(self)
    }
}

impl Focusable for Input {
    fn focused(&self) -> bool {
        Input::focused(self)
    }

    fn set_focused(&self, focused: bool) {
        Input::set_focused(self, focused)
    }
}
