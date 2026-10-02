//! Port of packages/tui/src/components/input.ts

#![allow(dead_code, unused_imports, unused_variables)]

use std::sync::{Arc, Mutex, MutexGuard};

use crate::kill_ring::KillRing;
use crate::tui::{Component, Focusable, TuiMouseEvent, TuiMouseEventResult};
use crate::undo_stack::UndoStack;

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
        todo!("port: Input::new")
    }

    pub fn get_value(&self) -> String {
        self.fields().value.clone()
    }

    pub fn set_value(&self, value: &str) {
        todo!("port: Input::set_value")
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
        todo!("port: Input::handle_input")
    }

    pub fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        todo!("port: Input::handle_mouse")
    }

    fn insert_character(&self, ch: &str) {
        todo!("port: Input::insert_character")
    }

    fn handle_backspace(&self) {
        todo!("port: Input::handle_backspace")
    }

    fn handle_forward_delete(&self) {
        todo!("port: Input::handle_forward_delete")
    }

    fn delete_to_line_start(&self) {
        todo!("port: Input::delete_to_line_start")
    }

    fn delete_to_line_end(&self) {
        todo!("port: Input::delete_to_line_end")
    }

    fn delete_word_backwards(&self) {
        todo!("port: Input::delete_word_backwards")
    }

    fn delete_word_forward(&self) {
        todo!("port: Input::delete_word_forward")
    }

    fn yank(&self) {
        todo!("port: Input::yank")
    }

    fn yank_pop(&self) {
        todo!("port: Input::yank_pop")
    }

    fn push_undo(&self) {
        todo!("port: Input::push_undo")
    }

    fn undo(&self) {
        todo!("port: Input::undo")
    }

    fn move_word_backwards(&self) {
        todo!("port: Input::move_word_backwards")
    }

    fn move_word_forwards(&self) {
        todo!("port: Input::move_word_forwards")
    }

    fn handle_paste(&self, pasted_text: &str) {
        todo!("port: Input::handle_paste")
    }

    /// No cached state to invalidate currently.
    pub fn invalidate(&self) {}

    pub fn render(&self, width: usize) -> Vec<String> {
        todo!("port: Input::render")
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
