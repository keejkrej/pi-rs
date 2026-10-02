//! Port of packages/tui/src/components/editor.ts

#![allow(dead_code, non_upper_case_globals, unused_imports, unused_variables)]

use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use indexmap::{IndexMap, IndexSet};

use crate::autocomplete::{AutocompleteItem, AutocompleteProvider, AutocompleteSuggestions};
use crate::components::select_list::{SelectList, SelectListLayoutOptions, SelectListTheme};
use crate::keybindings::KeybindingsManager;
use crate::keys::{decode_printable_key, matches_key};
use crate::kill_ring::KillRing;
use crate::tui::{Component, Focusable, TUI, TuiMouseEvent, TuiMouseEventResult, CURSOR_MARKER};
use crate::undo_stack::UndoStack;
use crate::utils::{
    autocomplete_boundary_regex, autocomplete_separator_regex, cjk_break_regex, get_grapheme_segmenter,
    get_word_segmenter, is_whitespace_char, slice_by_column, visible_width,
};
use crate::word_navigation::{WordNavigationOptions, find_word_backward, find_word_forward};

// PORT: shared segmenters stay in `crate::utils` (`get_grapheme_segmenter`, `get_word_segmenter`). Not redeclared.

/// Regex matching paste markers like `[paste #1 +123 lines]` or `[paste #2 1234 chars]`.
static PASTE_MARKER_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\[paste #(\d+)( (\+\d+ lines|\d+ chars))?\]").expect("PASTE_MARKER_REGEX")
});

/// Non-global version for single-segment testing.
static PASTE_MARKER_SINGLE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^\[paste #(\d+)( (\+\d+ lines|\d+ chars))?\]$").expect("PASTE_MARKER_SINGLE")
});

const SLASH_COMMAND_SELECT_LIST_LAYOUT: LazyLock<SelectListLayoutOptions> = LazyLock::new(|| SelectListLayoutOptions {
    min_primary_column_width: Some(12),
    max_primary_column_width: Some(32),
    truncate_primary: None,
});

const ATTACHMENT_AUTOCOMPLETE_DEBOUNCE_MS: i64 = 20;

const DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS: &[&str] = &["@", "#"];

// Unquoted completions end at whitespace or CJK punctuation; quoted paths may contain either.
// PORT: negative lookahead, so this is `fancy_regex` (the `regex` crate has no lookaround).
static unquoted_autocomplete_suffix_regex: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    let source = autocomplete_separator_regex.as_str();
    let mut pattern = String::from("(?:(?!");
    pattern.push_str(source);
    pattern.push_str(").)*");
    fancy_regex::Regex::new(&pattern).expect("unquotedAutocompleteSuffixRegex")
});

// Trigger tokens may be wrapped in prose, e.g. "(@src/foo" or "`@src/foo".
static autocomplete_token_start_source: LazyLock<String> = LazyLock::new(|| {
    let mut pattern = String::new();
    pattern.push_str(autocomplete_boundary_regex.as_str());
    pattern.push_str("[([{<`]*");
    pattern
});

type BorderColor = Arc<dyn Fn(&str) -> String + Send + Sync>;
type TextCallback = Arc<dyn Fn(&str) + Send + Sync>;
type TuiHandle = Arc<dyn TUI + Send + Sync>;
type AutocompleteProviderHandle = Arc<dyn AutocompleteProvider + Send + Sync>;

/// One `Intl.SegmentData` value.
///
/// `segmentWithMarkers` writes `segment`, `index`, `input`. `isWordLike` is set for word granularity and absent for graphemes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentData {
    pub segment: String,
    pub index: i64,
    pub input: String,
    pub is_word_like: Option<bool>,
}

/// Represents a chunk of text for word-wrap layout.
/// Tracks both the text content and its position in the original line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextChunk {
    pub text: String,
    pub start_index: i64,
    pub end_index: i64,
}

/// Cursor position returned by [`Editor::get_cursor`].
///
/// TS inline object `{ line, col }`. Field order is `line`, `col`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditorCursor {
    pub line: i64,
    pub col: i64,
}

#[derive(Clone)]
pub struct EditorTheme {
    pub border_color: BorderColor,
    pub select_list: SelectListTheme,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EditorOptions {
    pub padding_x: Option<f64>,
    pub autocomplete_max_visible: Option<f64>,
}

// Kitty CSI-u sequences for printable keys, including optional shifted/base codepoints.
#[derive(Clone, Debug, PartialEq, Eq)]
struct EditorState {
    lines: Vec<String>,
    cursor_line: i64,
    cursor_col: i64,
}

/// Undo snapshot: editor text state plus the paste registry.
#[derive(Clone, Debug, PartialEq, Eq)]
struct EditorSnapshot {
    state: EditorState,
    pastes: IndexMap<i64, String>,
    paste_counter: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LayoutLine {
    text: String,
    has_cursor: bool,
    cursor_pos: Option<i64>,
}

/// One visual line in [`Editor::build_visual_line_map`].
///
/// Field order is the object literal: `logicalLine`, `startCol`, `length`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VisualLine {
    logical_line: i64,
    start_col: i64,
    length: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AutocompleteState {
    /// `"regular"`
    Regular,
    /// `"force"`
    Force,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JumpMode {
    /// `"forward"`
    Forward,
    /// `"backward"`
    Backward,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CursorPlacement {
    /// `"start"`
    Start,
    /// `"end"`
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SegmentMode {
    /// `"word"`
    Word,
    /// `"grapheme"`
    Grapheme,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScrollBorderDirection {
    /// `"↑"`
    Up,
    /// `"↓"`
    Down,
}

/// `{ force, explicitTab }` passed to autocomplete requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AutocompleteRequestOptions {
    force: bool,
    explicit_tab: bool,
}

struct EditorFields {
    state: EditorState,
    /// Focusable interface - set by TUI when focus changes
    focused: bool,
    /// PORT: `protected tui`. Public getter; `CustomEditor` lives in another crate (§9.4 composition).
    tui: TuiHandle,
    theme: EditorTheme,
    padding_x: i64,
    /// Store last render geometry for cursor navigation and mouse hit-testing.
    last_width: i64,
    rendered_visible_line_count: i64,
    rendered_autocomplete_height: i64,
    /// Vertical scrolling support
    scroll_offset: i64,
    /// Border color (can be changed dynamically)
    border_color: BorderColor,
    autocomplete_provider: Option<AutocompleteProviderHandle>,
    autocomplete_trigger_characters: Vec<String>,
    autocomplete_trigger_pattern: fancy_regex::Regex,
    autocomplete_debounce_pattern: fancy_regex::Regex,
    autocomplete_list: Option<SelectList>,
    autocomplete_state: Option<AutocompleteState>,
    autocomplete_prefix: String,
    autocomplete_max_visible: i64,
    autocomplete_abort: Option<pi_js::abort::AbortController>,
    autocomplete_debounce_timer: Option<pi_js::time::Timeout>,
    autocomplete_request_task: pi_js::BoxFuture<()>,
    autocomplete_start_token: i64,
    autocomplete_request_id: i64,
    /// Paste tracking for large pastes. JS `Map` insertion order.
    pastes: IndexMap<i64, String>,
    paste_counter: i64,
    /// Bracketed paste mode buffering
    paste_buffer: String,
    is_in_paste: bool,
    /// Prompt history for up/down navigation
    history: Vec<String>,
    /// -1 = not browsing, 0 = most recent, 1 = older, etc.
    history_index: i64,
    history_draft: Option<EditorState>,
    /// Kill ring for Emacs-style kill/yank operations
    kill_ring: KillRing,
    last_action: Option<LastAction>,
    /// Character jump mode
    jump_mode: Option<JumpMode>,
    /// Preferred visual column for vertical cursor movement (sticky column)
    preferred_visual_col: Option<i64>,
    /// When the cursor is snapped to the start of an atomic segment, e.g. a
    /// paste marker, cursorCol no longer reflects where the cursor would have
    /// landed. This field stores the pre-snap cursorCol so that the next
    /// vertical move can resolve it to a visual column on whatever VL it belongs
    /// to.
    snapped_from_cursor_col: Option<i64>,
    /// Undo support
    undo_stack: UndoStack<EditorSnapshot>,
    on_submit: Option<TextCallback>,
    on_change: Option<TextCallback>,
    disable_submit: bool,
}

struct EditorInner {
    fields: Mutex<EditorFields>,
}

/// Multi-line text editor.
///
/// PORT: TS class with identity. Handle so containers can store `Arc<dyn Component>`.
#[derive(Clone)]
pub struct Editor {
    inner: Arc<EditorInner>,
}

/// Check if a segment is a paste marker (i.e. was merged by segmentWithMarkers).
fn is_paste_marker(segment: &str) -> bool {
    let _ = (&*PASTE_MARKER_SINGLE, segment);
    todo!("port: is_paste_marker")
}

/// A segmenter that wraps Intl.Segmenter and merges graphemes that fall
/// within paste markers into single atomic segments.  This makes cursor
/// movement, deletion, word-wrap, etc. treat paste markers as single units.
///
/// Only markers whose numeric ID exists in `validIds` are merged.
///
/// PORT: takes [`SegmentMode`] instead of an `Intl.Segmenter`. The body selects
/// `get_grapheme_segmenter` or `get_word_segmenter`.
fn segment_with_markers(text: &str, mode: SegmentMode, valid_ids: &IndexSet<i64>) -> Vec<SegmentData> {
    let _ = (&*PASTE_MARKER_REGEX, text, mode, valid_ids);
    todo!("port: segment_with_markers")
}

/// Split a line into word-wrapped chunks.
/// Wraps at word boundaries when possible, falling back to character-level
/// wrapping for words longer than the available width.
///
/// `line` is the text line to wrap.
/// `max_width` is the maximum visible width per chunk.
/// `pre_segmented` is optional pre-segmented graphemes (e.g. with paste-marker awareness).
/// When omitted the default Intl.Segmenter is used.
/// Returns chunks with text and position information.
pub fn word_wrap_line(line: &str, max_width: i64, pre_segmented: Option<&[SegmentData]>) -> Vec<TextChunk> {
    let _ = (line, max_width, pre_segmented, is_paste_marker, is_whitespace_char, visible_width, &cjk_break_regex);
    todo!("port: word_wrap_line")
}

fn escape_character_class(value: &str) -> String {
    todo!("port: escape_character_class")
}

fn build_trigger_pattern(trigger_characters: &[String]) -> fancy_regex::Regex {
    let _ = (
        trigger_characters,
        &*unquoted_autocomplete_suffix_regex,
        &*autocomplete_token_start_source,
        escape_character_class,
    );
    todo!("port: build_trigger_pattern")
}

fn build_debounce_pattern(trigger_characters: &[String]) -> fancy_regex::Regex {
    let _ = (
        trigger_characters,
        &*unquoted_autocomplete_suffix_regex,
        &*autocomplete_token_start_source,
        escape_character_class,
    );
    todo!("port: build_debounce_pattern")
}

fn create_scroll_border(direction: ScrollBorderDirection, hidden_line_count: i64, width: i64) -> String {
    let _ = (direction, hidden_line_count, width, slice_by_column, visible_width);
    todo!("port: create_scroll_border")
}

impl Editor {
    fn fields(&self) -> MutexGuard<'_, EditorFields> {
        self.inner.fields.lock().unwrap()
    }

    pub fn new(tui: TuiHandle, theme: EditorTheme, options: Option<EditorOptions>) -> Self {
        let _ = (
            tui,
            theme,
            options,
            DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS,
            ATTACHMENT_AUTOCOMPLETE_DEBOUNCE_MS,
            build_trigger_pattern,
            build_debounce_pattern,
        );
        todo!("port: Editor::new")
    }

    /// Focusable `focused` flag.
    pub fn focused(&self) -> bool {
        self.fields().focused
    }

    pub fn set_focused(&self, focused: bool) {
        self.fields().focused = focused;
    }

    /// `protected tui`.
    pub fn tui(&self) -> TuiHandle {
        Arc::clone(&self.fields().tui)
    }

    pub fn border_color(&self) -> BorderColor {
        Arc::clone(&self.fields().border_color)
    }

    pub fn set_border_color(&self, border_color: BorderColor) {
        self.fields().border_color = border_color;
    }

    pub fn on_submit(&self) -> Option<TextCallback> {
        self.fields().on_submit.clone()
    }

    pub fn set_on_submit(&self, on_submit: Option<TextCallback>) {
        self.fields().on_submit = on_submit;
    }

    pub fn on_change(&self) -> Option<TextCallback> {
        self.fields().on_change.clone()
    }

    pub fn set_on_change(&self, on_change: Option<TextCallback>) {
        self.fields().on_change = on_change;
    }

    pub fn disable_submit(&self) -> bool {
        self.fields().disable_submit
    }

    pub fn set_disable_submit(&self, disable_submit: bool) {
        self.fields().disable_submit = disable_submit;
    }

    pub fn get_padding_x(&self) -> i64 {
        self.fields().padding_x
    }

    pub fn set_padding_x(&self, padding: f64) {
        todo!("port: Editor::set_padding_x")
    }

    pub fn get_autocomplete_max_visible(&self) -> i64 {
        self.fields().autocomplete_max_visible
    }

    pub fn set_autocomplete_max_visible(&self, max_visible: f64) {
        todo!("port: Editor::set_autocomplete_max_visible")
    }

    pub fn set_autocomplete_provider(&self, provider: AutocompleteProviderHandle) {
        todo!("port: Editor::set_autocomplete_provider")
    }

    /// Add a prompt to history for up/down arrow navigation.
    /// Called after successful submission.
    pub fn add_to_history(&self, text: &str) {
        todo!("port: Editor::add_to_history")
    }

    /// No cached state to invalidate currently.
    pub fn invalidate(&self) {}

    /// PORT: `protected`. Public so another crate can delegate (§9.4).
    pub fn render_top_border(&self, width: usize, hidden_line_count: i64) -> String {
        let _ = (width, hidden_line_count, create_scroll_border);
        todo!("port: Editor::render_top_border")
    }

    /// PORT: `protected`. Public so another crate can delegate (§9.4).
    pub fn render_bottom_border(&self, width: usize, hidden_line_count: i64) -> String {
        let _ = (width, hidden_line_count, create_scroll_border);
        todo!("port: Editor::render_bottom_border")
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        let _ = (width, CURSOR_MARKER);
        todo!("port: Editor::render")
    }

    pub fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        todo!("port: Editor::handle_mouse")
    }

    pub fn handle_input(&self, data: &str) {
        let _ = (data, decode_printable_key, matches_key, KeybindingsManager::matches);
        todo!("port: Editor::handle_input")
    }

    pub fn get_text(&self) -> String {
        todo!("port: Editor::get_text")
    }

    /// Get text with paste markers expanded to their actual content.
    /// Use this when you need the full content (e.g., for external editor).
    pub fn get_expanded_text(&self) -> String {
        todo!("port: Editor::get_expanded_text")
    }

    pub fn get_lines(&self) -> Vec<String> {
        self.fields().state.lines.clone()
    }

    pub fn get_cursor(&self) -> EditorCursor {
        let fields = self.fields();
        EditorCursor {
            line: fields.state.cursor_line,
            col: fields.state.cursor_col,
        }
    }

    pub fn set_text(&self, text: &str) {
        todo!("port: Editor::set_text")
    }

    /// Insert text at the current cursor position.
    /// Used for programmatic insertion (e.g., clipboard image markers).
    /// This is atomic for undo - single undo restores entire pre-insert state.
    pub fn insert_text_at_cursor(&self, text: &str) {
        todo!("port: Editor::insert_text_at_cursor")
    }

    pub fn is_showing_autocomplete(&self) -> bool {
        self.fields().autocomplete_state.is_some()
    }

    /// Set of currently valid paste IDs, for marker-aware segmentation.
    fn valid_paste_ids(&self) -> IndexSet<i64> {
        todo!("port: Editor::valid_paste_ids")
    }

    /// Segment text with paste-marker awareness, only merging markers with valid IDs.
    fn segment(&self, text: &str, mode: SegmentMode) -> Vec<SegmentData> {
        let _ = (text, mode, get_grapheme_segmenter, get_word_segmenter, segment_with_markers);
        todo!("port: Editor::segment")
    }

    fn is_editor_empty(&self) -> bool {
        todo!("port: Editor::is_editor_empty")
    }

    fn is_on_first_visual_line(&self) -> bool {
        todo!("port: Editor::is_on_first_visual_line")
    }

    fn is_on_last_visual_line(&self) -> bool {
        todo!("port: Editor::is_on_last_visual_line")
    }

    /// `direction` is `1` or `-1`. Up (`-1`) increases the history index; down (`1`) decreases it.
    fn navigate_history(&self, direction: i64) {
        todo!("port: Editor::navigate_history")
    }

    fn exit_history_browsing(&self) {
        todo!("port: Editor::exit_history_browsing")
    }

    /// Internal setText that doesn't reset history state - used by navigateHistory.
    ///
    /// `cursor_placement` defaults to `"end"` inside the body.
    fn set_text_internal(&self, text: &str, cursor_placement: Option<CursorPlacement>) {
        todo!("port: Editor::set_text_internal")
    }

    fn layout_text(&self, content_width: i64) -> Vec<LayoutLine> {
        let _ = (content_width, word_wrap_line);
        todo!("port: Editor::layout_text")
    }

    fn expand_paste_markers(&self, text: &str) -> String {
        todo!("port: Editor::expand_paste_markers")
    }

    /// Normalize text for editor storage:
    /// - Normalize line endings (\r\n and \r -> \n)
    /// - Expand tabs to 4 spaces
    fn normalize_text(&self, text: &str) -> String {
        todo!("port: Editor::normalize_text")
    }

    /// Internal text insertion at cursor. Handles single and multi-line text.
    /// Does not push undo snapshots or trigger autocomplete - caller is responsible.
    /// Normalizes line endings and calls onChange once at the end.
    fn insert_text_at_cursor_internal(&self, text: &str) {
        todo!("port: Editor::insert_text_at_cursor_internal")
    }

    /// `skip_undo_coalescing` defaults to false inside the body.
    fn insert_character(&self, ch: &str, skip_undo_coalescing: Option<bool>) {
        let _ = (ch, skip_undo_coalescing, &cjk_break_regex, is_whitespace_char);
        todo!("port: Editor::insert_character")
    }

    fn handle_paste(&self, pasted_text: &str) {
        todo!("port: Editor::handle_paste")
    }

    fn add_new_line(&self) {
        todo!("port: Editor::add_new_line")
    }

    fn should_submit_on_backslash_enter(&self, data: &str, kb: &KeybindingsManager) -> bool {
        let _ = (data, kb, matches_key);
        todo!("port: Editor::should_submit_on_backslash_enter")
    }

    fn submit_value(&self) {
        todo!("port: Editor::submit_value")
    }

    fn handle_backspace(&self) {
        todo!("port: Editor::handle_backspace")
    }

    fn set_cursor_col(&self, col: i64) {
        todo!("port: Editor::set_cursor_col")
    }

    /// Move cursor to a target visual line, applying sticky column logic.
    /// Shared by moveCursor() and pageScroll().
    fn move_to_visual_line(&self, visual_lines: &[VisualLine], current_visual_line: i64, target_visual_line: i64) {
        todo!("port: Editor::move_to_visual_line")
    }

    /// Compute the target visual column for vertical cursor movement.
    /// Implements the sticky column decision table:
    ///
    /// | P | S | T | U | Scenario                                             | Set Preferred | Move To     |
    /// |---|---|---|---| ---------------------------------------------------- |---------------|-------------|
    /// | 0 | * | 0 | - | Start nav, target fits                               | null          | current     |
    /// | 0 | * | 1 | - | Start nav, target shorter                            | current       | target end  |
    /// | 1 | 0 | 0 | 0 | Clamped, target fits preferred                       | null          | preferred   |
    /// | 1 | 0 | 0 | 1 | Clamped, target longer but still can't fit preferred | keep          | target end  |
    /// | 1 | 0 | 1 | - | Clamped, target even shorter                         | keep          | target end  |
    /// | 1 | 1 | 0 | - | Rewrapped, target fits current                       | null          | current     |
    /// | 1 | 1 | 1 | - | Rewrapped, target shorter than current               | current       | target end  |
    ///
    /// Where:
    /// - P = preferred col is set
    /// - S = cursor in middle of source line (not clamped to end)
    /// - T = target line shorter than current visual col
    /// - U = target line shorter than preferred col
    fn compute_vertical_move_column(
        &self,
        current_visual_col: i64,
        source_max_visual_col: i64,
        target_max_visual_col: i64,
    ) -> i64 {
        todo!("port: Editor::compute_vertical_move_column")
    }

    fn move_to_line_start(&self) {
        todo!("port: Editor::move_to_line_start")
    }

    fn move_to_line_end(&self) {
        todo!("port: Editor::move_to_line_end")
    }

    fn delete_to_start_of_line(&self) {
        todo!("port: Editor::delete_to_start_of_line")
    }

    fn delete_to_end_of_line(&self) {
        todo!("port: Editor::delete_to_end_of_line")
    }

    fn delete_word_backwards(&self) {
        let _ = find_word_backward;
        todo!("port: Editor::delete_word_backwards")
    }

    fn delete_word_forward(&self) {
        let _ = find_word_forward;
        todo!("port: Editor::delete_word_forward")
    }

    fn handle_forward_delete(&self) {
        todo!("port: Editor::handle_forward_delete")
    }

    /// Build a mapping from visual lines to logical positions.
    /// Returns an array where each element represents a visual line with:
    /// - logicalLine: index into this.state.lines
    /// - startCol: starting column in the logical line
    /// - length: length of this visual line segment
    fn build_visual_line_map(&self, width: i64) -> Vec<VisualLine> {
        let _ = (width, visible_width, word_wrap_line);
        todo!("port: Editor::build_visual_line_map")
    }

    /// Find the visual line index that contains the given logical position.
    fn find_visual_line_at(&self, visual_lines: &[VisualLine], line: i64, col: i64) -> i64 {
        todo!("port: Editor::find_visual_line_at")
    }

    /// Find the visual line index for the current cursor position.
    fn find_current_visual_line(&self, visual_lines: &[VisualLine]) -> i64 {
        todo!("port: Editor::find_current_visual_line")
    }

    fn move_cursor(&self, delta_line: i64, delta_col: i64) {
        todo!("port: Editor::move_cursor")
    }

    /// Scroll by a page (direction: -1 for up, 1 for down).
    /// Moves cursor by the page size while keeping it in bounds.
    fn page_scroll(&self, direction: i64) {
        todo!("port: Editor::page_scroll")
    }

    fn move_word_backwards(&self) {
        let _ = (find_word_backward, WordNavigationOptions::default);
        todo!("port: Editor::move_word_backwards")
    }

    /// Yank (paste) the most recent kill ring entry at cursor position.
    fn yank(&self) {
        todo!("port: Editor::yank")
    }

    /// Cycle through kill ring (only works immediately after yank or yank-pop).
    /// Replaces the last yanked text with the previous entry in the ring.
    fn yank_pop(&self) {
        todo!("port: Editor::yank_pop")
    }

    /// Insert text at cursor position (used by yank operations).
    fn insert_yanked_text(&self, text: &str) {
        todo!("port: Editor::insert_yanked_text")
    }

    /// Delete the previously yanked text (used by yank-pop).
    /// The yanked text is derived from killRing[end] since it hasn't been rotated yet.
    fn delete_yanked_text(&self) {
        todo!("port: Editor::delete_yanked_text")
    }

    fn push_undo_snapshot(&self) {
        todo!("port: Editor::push_undo_snapshot")
    }

    fn undo(&self) {
        todo!("port: Editor::undo")
    }

    /// Jump to the first occurrence of a character in the specified direction.
    /// Multi-line search. Case-sensitive. Skips the current cursor position.
    fn jump_to_char(&self, ch: &str, direction: JumpMode) {
        todo!("port: Editor::jump_to_char")
    }

    fn move_word_forwards(&self) {
        let _ = (find_word_forward, WordNavigationOptions::default);
        todo!("port: Editor::move_word_forwards")
    }

    /// Slash menu only allowed on the first line of the editor.
    fn is_slash_menu_allowed(&self) -> bool {
        todo!("port: Editor::is_slash_menu_allowed")
    }

    /// Helper method to check if cursor is at start of message (for slash command detection).
    fn is_at_start_of_message(&self) -> bool {
        todo!("port: Editor::is_at_start_of_message")
    }

    fn is_in_slash_command_context(&self, text_before_cursor: &str) -> bool {
        todo!("port: Editor::is_in_slash_command_context")
    }

    /// Find the best autocomplete item index for the given prefix.
    /// Returns -1 if no match is found.
    ///
    /// Match priority:
    /// 1. Exact match (prefix === item.value) -> always selected
    /// 2. Prefix match -> first item whose value starts with prefix
    /// 3. No match -> -1 (keep default highlight)
    ///
    /// Matching is case-sensitive and checks item.value only.
    fn get_best_autocomplete_match_index(&self, items: &[AutocompleteItem], prefix: &str) -> i64 {
        todo!("port: Editor::get_best_autocomplete_match_index")
    }

    fn create_autocomplete_list(&self, prefix: &str, items: &[AutocompleteItem]) -> SelectList {
        let _ = (prefix, items, &*SLASH_COMMAND_SELECT_LIST_LAYOUT);
        todo!("port: Editor::create_autocomplete_list")
    }

    /// `explicit_tab` defaults to false inside the body.
    fn try_trigger_autocomplete(&self, explicit_tab: Option<bool>) {
        todo!("port: Editor::try_trigger_autocomplete")
    }

    fn handle_tab_completion(&self) {
        todo!("port: Editor::handle_tab_completion")
    }

    fn handle_slash_command_completion(&self) {
        todo!("port: Editor::handle_slash_command_completion")
    }

    /// `explicit_tab` defaults to false inside the body.
    fn force_file_autocomplete(&self, explicit_tab: Option<bool>) {
        todo!("port: Editor::force_file_autocomplete")
    }

    fn request_autocomplete(&self, options: AutocompleteRequestOptions) {
        let _ = (options, ATTACHMENT_AUTOCOMPLETE_DEBOUNCE_MS);
        todo!("port: Editor::request_autocomplete")
    }

    async fn start_autocomplete_request(&self, start_token: i64, options: AutocompleteRequestOptions) {
        todo!("port: Editor::start_autocomplete_request")
    }

    fn set_autocomplete_trigger_characters(&self, trigger_characters: &[String]) {
        let _ = (trigger_characters, DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS, is_whitespace_char);
        todo!("port: Editor::set_autocomplete_trigger_characters")
    }

    fn get_autocomplete_debounce_ms(&self, options: &AutocompleteRequestOptions) -> i64 {
        let _ = (options, ATTACHMENT_AUTOCOMPLETE_DEBOUNCE_MS);
        todo!("port: Editor::get_autocomplete_debounce_ms")
    }

    async fn run_autocomplete_request(
        &self,
        request_id: i64,
        controller: &pi_js::abort::AbortController,
        snapshot_text: &str,
        snapshot_line: i64,
        snapshot_col: i64,
        options: &AutocompleteRequestOptions,
    ) {
        todo!("port: Editor::run_autocomplete_request")
    }

    fn is_autocomplete_request_current(
        &self,
        request_id: i64,
        controller: &pi_js::abort::AbortController,
        snapshot_text: &str,
        snapshot_line: i64,
        snapshot_col: i64,
    ) -> bool {
        todo!("port: Editor::is_autocomplete_request_current")
    }

    fn apply_autocomplete_suggestions(&self, suggestions: &AutocompleteSuggestions, state: AutocompleteState) {
        todo!("port: Editor::apply_autocomplete_suggestions")
    }

    fn cancel_autocomplete_request(&self) {
        todo!("port: Editor::cancel_autocomplete_request")
    }

    fn clear_autocomplete_ui(&self) {
        todo!("port: Editor::clear_autocomplete_ui")
    }

    fn cancel_autocomplete(&self) {
        todo!("port: Editor::cancel_autocomplete")
    }

    fn update_autocomplete(&self) {
        todo!("port: Editor::update_autocomplete")
    }
}

impl Component for Editor {
    fn render(&self, width: usize) -> Vec<String> {
        Editor::render(self, width)
    }

    fn handle_input(&self, data: &str) {
        Editor::handle_input(self, data)
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        Editor::handle_mouse(self, event)
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        Editor::invalidate(self)
    }
}

impl Focusable for Editor {
    fn focused(&self) -> bool {
        Editor::focused(self)
    }

    fn set_focused(&self, focused: bool) {
        Editor::set_focused(self, focused)
    }
}
