//! Port of packages/tui/src/components/editor.ts

#![allow(dead_code, non_upper_case_globals, unused_imports, unused_variables)]

use std::sync::{Arc, LazyLock, Mutex, MutexGuard, Weak};

use indexmap::{IndexMap, IndexSet};

use crate::autocomplete::{AutocompleteItem, AutocompleteProvider, AutocompleteSuggestions, GetSuggestionsOptions};
use crate::components::select_list::{SelectItem, SelectList, SelectListLayoutOptions, SelectListTheme};
use crate::keybindings::{KeybindingsManager, get_keybindings};
use crate::keys::{decode_printable_key, matches_key};
use crate::kill_ring::{KillRing, KillRingPushOptions};
use crate::tui::{
    CURSOR_MARKER, Component, Focusable, TUI, TuiMouseButton, TuiMouseEvent, TuiMouseEventResult, TuiMouseEventType,
};
use crate::undo_stack::UndoStack;
use crate::utils::{
    autocomplete_boundary_regex, autocomplete_separator_regex, cjk_break_regex, get_grapheme_segmenter,
    get_word_segmenter, is_whitespace_char, slice_by_column, visible_width,
};
use crate::word_navigation::{WordNavigationOptions, find_word_backward, find_word_forward};

// PORT: shared segmenters stay in `crate::utils` (`get_grapheme_segmenter`, `get_word_segmenter`). Not redeclared.

/// Regex matching paste markers like `[paste #1 +123 lines]` or `[paste #2 1234 chars]`.
static PASTE_MARKER_REGEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\[paste #(\d+)( (\+\d+ lines|\d+ chars))?\]").expect("PASTE_MARKER_REGEX"));

/// Non-global version for single-segment testing.
static PASTE_MARKER_SINGLE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^\[paste #(\d+)( (\+\d+ lines|\d+ chars))?\]$").expect("PASTE_MARKER_SINGLE"));

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
    pattern.push_str(r"[(\[{<`]*");
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

fn utf16_len(text: &str) -> i64 {
    i64::try_from(pi_js::str16::len(text)).unwrap_or(i64::MAX)
}

fn utf16_slice(text: &str, start: i64, end: Option<i64>) -> String {
    pi_js::str16::slice(text, start, end)
}

fn utf16_substring(text: &str, start: i64, end: Option<i64>) -> String {
    pi_js::str16::substring(
        text,
        usize::try_from(start.max(0)).unwrap_or(0),
        end.map(|value| usize::try_from(value.max(0)).unwrap_or(0)),
    )
}

fn from_util_segments(segments: Vec<crate::utils::SegmentData>) -> Vec<SegmentData> {
    segments
        .into_iter()
        .map(|seg| SegmentData {
            segment: seg.segment,
            index: seg.index,
            input: seg.input,
            is_word_like: seg.is_word_like,
        })
        .collect()
}

fn to_util_segments(segments: Vec<SegmentData>) -> Vec<crate::utils::SegmentData> {
    segments
        .into_iter()
        .map(|seg| crate::utils::SegmentData {
            segment: seg.segment,
            index: seg.index,
            input: seg.input,
            is_word_like: seg.is_word_like,
        })
        .collect()
}

/// Check if a segment is a paste marker (i.e. was merged by segmentWithMarkers).
fn is_paste_marker(segment: &str) -> bool {
    PASTE_MARKER_SINGLE.is_match(segment)
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
    let base_segmenter = match mode {
        SegmentMode::Word => get_word_segmenter(),
        SegmentMode::Grapheme => get_grapheme_segmenter(),
    };

    // Fast path: no paste markers in the text or no valid IDs.
    if valid_ids.is_empty() || !text.contains("[paste #") {
        return from_util_segments(base_segmenter.segment(text));
    }

    // Find all marker spans with valid IDs.
    struct MarkerSpan {
        start: i64,
        end: i64,
        text: String,
    }
    let mut markers: Vec<MarkerSpan> = Vec::new();
    for cap in PASTE_MARKER_REGEX.captures_iter(text) {
        let Some(full) = cap.get(0) else {
            continue;
        };
        let Some(id_match) = cap.get(1) else {
            continue;
        };
        let Ok(id) = id_match.as_str().parse::<i64>() else {
            continue;
        };
        if !valid_ids.contains(&id) {
            continue;
        }
        let start = i64::try_from(pi_js::str16::byte_to_utf16(text, full.start())).unwrap_or(0);
        let end = i64::try_from(pi_js::str16::byte_to_utf16(text, full.end())).unwrap_or(start);
        markers.push(MarkerSpan {
            start,
            end,
            text: full.as_str().to_string(),
        });
    }
    if markers.is_empty() {
        return from_util_segments(base_segmenter.segment(text));
    }

    // Build merged segment list.
    let base_segments = base_segmenter.segment(text);
    let mut result: Vec<SegmentData> = Vec::new();
    let mut marker_idx = 0usize;

    for seg in base_segments {
        // Skip past markers that are entirely before this segment.
        while marker_idx < markers.len() && markers[marker_idx].end <= seg.index {
            marker_idx += 1;
        }

        let marker = markers.get(marker_idx);
        if let Some(marker) = marker
            && seg.index >= marker.start
            && seg.index < marker.end
        {
            // This segment falls inside a marker.
            // If this is the first segment of the marker, emit a merged segment.
            if seg.index == marker.start {
                result.push(SegmentData {
                    segment: marker.text.clone(),
                    index: marker.start,
                    input: text.to_string(),
                    is_word_like: None,
                });
            }
            // Otherwise skip (already merged into the first segment).
        } else {
            result.push(SegmentData {
                segment: seg.segment,
                index: seg.index,
                input: seg.input,
                is_word_like: seg.is_word_like,
            });
        }
    }

    result
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
    if line.is_empty() || max_width <= 0 {
        return vec![TextChunk {
            text: String::new(),
            start_index: 0,
            end_index: 0,
        }];
    }

    let line_width = visible_width(line);
    let line_len = utf16_len(line);
    if line_width <= max_width {
        return vec![TextChunk {
            text: line.to_string(),
            start_index: 0,
            end_index: line_len,
        }];
    }

    let mut chunks: Vec<TextChunk> = Vec::new();
    let owned_segments;
    let segments: &[SegmentData] = match pre_segmented {
        Some(segs) => segs,
        None => {
            owned_segments = from_util_segments(get_grapheme_segmenter().segment(line));
            &owned_segments
        }
    };

    let mut current_width: i64 = 0;
    let mut chunk_start: i64 = 0;

    // Wrap opportunity: the position after the last whitespace before a non-whitespace
    // grapheme, i.e. where a line break is allowed.
    let mut wrap_opp_index: i64 = -1;
    let mut wrap_opp_width: i64 = 0;

    for (i, seg) in segments.iter().enumerate() {
        let grapheme = &seg.segment;
        let g_width = visible_width(grapheme);
        let char_index = seg.index;
        let is_ws = !is_paste_marker(grapheme) && is_whitespace_char(grapheme);

        // Overflow check before advancing.
        if current_width + g_width > max_width {
            if wrap_opp_index >= 0 && current_width - wrap_opp_width + g_width <= max_width {
                // Backtrack to last wrap opportunity (the remaining content
                // plus the current grapheme still fits within maxWidth).
                chunks.push(TextChunk {
                    text: utf16_slice(line, chunk_start, Some(wrap_opp_index)),
                    start_index: chunk_start,
                    end_index: wrap_opp_index,
                });
                chunk_start = wrap_opp_index;
                current_width -= wrap_opp_width;
            } else if chunk_start < char_index {
                // No viable wrap opportunity: force-break at current position.
                // This also handles the case where backtracking to a word
                // boundary wouldn't help because the remaining content plus
                // the current grapheme (e.g. a wide character) still exceeds
                // maxWidth.
                chunks.push(TextChunk {
                    text: utf16_slice(line, chunk_start, Some(char_index)),
                    start_index: chunk_start,
                    end_index: char_index,
                });
                chunk_start = char_index;
                current_width = 0;
            }
            wrap_opp_index = -1;
        }

        if g_width > max_width {
            // Single atomic segment wider than maxWidth (e.g. paste marker
            // in a narrow terminal). Re-wrap it at grapheme granularity.
            //
            // The segment remains logically atomic for cursor
            // movement / editing — the split is purely visual for word-wrap layout.
            let sub_graphemes = from_util_segments(get_grapheme_segmenter().segment(grapheme));
            if sub_graphemes.len() > 1 {
                let sub_chunks = word_wrap_line(grapheme, max_width, Some(&sub_graphemes));
                if let Some((last, head)) = sub_chunks.split_last() {
                    for sc in head {
                        chunks.push(TextChunk {
                            text: sc.text.clone(),
                            start_index: char_index + sc.start_index,
                            end_index: char_index + sc.end_index,
                        });
                    }
                    chunk_start = char_index + last.start_index;
                    current_width = visible_width(&last.text);
                    wrap_opp_index = -1;
                    continue;
                }
            }
        }

        // Advance.
        current_width += g_width;

        // Record wrap opportunity: whitespace followed by non-whitespace
        // (multiple spaces join; the break point is after the last space),
        // or at a boundary where either side is CJK (CJK allows breaking
        // between any adjacent characters).
        if let Some(next) = segments.get(i + 1) {
            if is_ws && (is_paste_marker(&next.segment) || !is_whitespace_char(&next.segment)) {
                wrap_opp_index = next.index;
                wrap_opp_width = current_width;
            } else if !is_ws && !is_whitespace_char(&next.segment) {
                let is_cjk = !is_paste_marker(grapheme) && cjk_break_regex.is_match(grapheme);
                let next_is_cjk = !is_paste_marker(&next.segment) && cjk_break_regex.is_match(&next.segment);
                if is_cjk || next_is_cjk {
                    wrap_opp_index = next.index;
                    wrap_opp_width = current_width;
                }
            }
        }
    }

    // Push final chunk.
    chunks.push(TextChunk {
        text: utf16_slice(line, chunk_start, None),
        start_index: chunk_start,
        end_index: line_len,
    });

    chunks
}

fn escape_character_class(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(
            ch,
            '\\' | '^' | '$' | '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '-'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn build_trigger_pattern(trigger_characters: &[String]) -> fancy_regex::Regex {
    let token_start = &*autocomplete_token_start_source;
    let suffix = unquoted_autocomplete_suffix_regex.as_str();
    let escaped: String = trigger_characters.iter().map(|c| escape_character_class(c)).collect();
    let pattern = if escaped.is_empty() {
        format!("{token_start}@\"[^\"]*$")
    } else {
        format!("{token_start}(?:@\"[^\"]*|[{}]{suffix})$", escaped)
    };
    fancy_regex::Regex::new(&pattern).expect("autocompleteTriggerPattern")
}

fn build_debounce_pattern(trigger_characters: &[String]) -> fancy_regex::Regex {
    let token_start = &*autocomplete_token_start_source;
    let suffix = unquoted_autocomplete_suffix_regex.as_str();
    let escaped_without_at: String = trigger_characters
        .iter()
        .filter(|character| character.as_str() != "@")
        .map(|character| escape_character_class(character))
        .collect();
    let pattern = if escaped_without_at.is_empty() {
        format!("{token_start}@(?:\"[^\"]*|{suffix})$")
    } else {
        format!(
            "{token_start}(?:@(?:\"[^\"]*|{suffix})|[{}]{suffix})$",
            escaped_without_at
        )
    };
    fancy_regex::Regex::new(&pattern).expect("autocompleteDebouncePattern")
}

fn create_scroll_border(direction: ScrollBorderDirection, hidden_line_count: i64, width: i64) -> String {
    let arrow = match direction {
        ScrollBorderDirection::Up => "↑",
        ScrollBorderDirection::Down => "↓",
    };
    let available_width = width.max(0);
    let label = format!(" {arrow} {hidden_line_count} more ");
    let label_width = visible_width(&label);
    if label_width + 2 <= available_width {
        let left_width = (available_width - label_width) / 2;
        let right_width = available_width - left_width - label_width;
        return format!(
            "{}{}{}",
            "─".repeat(usize::try_from(left_width).unwrap_or(0)),
            label,
            "─".repeat(usize::try_from(right_width).unwrap_or(0))
        );
    }

    let indicator = format!("─── {arrow} {hidden_line_count} more ");
    let remaining = available_width - visible_width(&indicator);
    if remaining >= 0 {
        return format!("{}{}", indicator, "─".repeat(usize::try_from(remaining).unwrap_or(0)));
    }

    let ellipsis = utf16_slice("...", 0, Some(available_width));
    let indicator_width = available_width - visible_width(&ellipsis);
    format!(
        "{}{}",
        slice_by_column(&indicator, 0, indicator_width, Some(true)),
        ellipsis
    )
}

fn normalize_padding_x(padding: f64) -> i64 {
    if padding.is_finite() {
        (padding.floor() as i64).max(0)
    } else {
        0
    }
}

fn normalize_autocomplete_max_visible(max_visible: f64) -> i64 {
    if max_visible.is_finite() {
        (max_visible.floor() as i64).clamp(3, 20)
    } else {
        5
    }
}

impl Editor {
    fn fields(&self) -> MutexGuard<'_, EditorFields> {
        self.inner.fields.lock().unwrap()
    }

    pub fn new(tui: TuiHandle, theme: EditorTheme, options: Option<EditorOptions>) -> Self {
        let opts = options.unwrap_or_default();
        let padding_x = normalize_padding_x(opts.padding_x.unwrap_or(0.0));
        let autocomplete_max_visible = normalize_autocomplete_max_visible(opts.autocomplete_max_visible.unwrap_or(5.0));
        let trigger_characters: Vec<String> = DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let autocomplete_trigger_pattern = build_trigger_pattern(&trigger_characters);
        let autocomplete_debounce_pattern = build_debounce_pattern(&trigger_characters);
        let border_color = Arc::clone(&theme.border_color);

        Self {
            inner: Arc::new(EditorInner {
                fields: Mutex::new(EditorFields {
                    state: EditorState {
                        lines: vec![String::new()],
                        cursor_line: 0,
                        cursor_col: 0,
                    },
                    focused: false,
                    tui,
                    theme,
                    padding_x,
                    last_width: 80,
                    rendered_visible_line_count: 1,
                    rendered_autocomplete_height: 0,
                    scroll_offset: 0,
                    border_color,
                    autocomplete_provider: None,
                    autocomplete_trigger_characters: trigger_characters,
                    autocomplete_trigger_pattern,
                    autocomplete_debounce_pattern,
                    autocomplete_list: None,
                    autocomplete_state: None,
                    autocomplete_prefix: String::new(),
                    autocomplete_max_visible,
                    autocomplete_abort: None,
                    autocomplete_debounce_timer: None,
                    autocomplete_request_task: Box::pin(async {}),
                    autocomplete_start_token: 0,
                    autocomplete_request_id: 0,
                    pastes: IndexMap::new(),
                    paste_counter: 0,
                    paste_buffer: String::new(),
                    is_in_paste: false,
                    history: Vec::new(),
                    history_index: -1,
                    history_draft: None,
                    kill_ring: KillRing::new(),
                    last_action: None,
                    jump_mode: None,
                    preferred_visual_col: None,
                    snapped_from_cursor_col: None,
                    undo_stack: UndoStack::new(),
                    on_submit: None,
                    on_change: None,
                    disable_submit: false,
                }),
            }),
        }
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
        let new_padding = normalize_padding_x(padding);
        let tui = {
            let mut fields = self.fields();
            if fields.padding_x == new_padding {
                return;
            }
            fields.padding_x = new_padding;
            Arc::clone(&fields.tui)
        };
        tui.request_render(None);
    }

    pub fn get_autocomplete_max_visible(&self) -> i64 {
        self.fields().autocomplete_max_visible
    }

    pub fn set_autocomplete_max_visible(&self, max_visible: f64) {
        let new_max_visible = normalize_autocomplete_max_visible(max_visible);
        let tui = {
            let mut fields = self.fields();
            if fields.autocomplete_max_visible == new_max_visible {
                return;
            }
            fields.autocomplete_max_visible = new_max_visible;
            Arc::clone(&fields.tui)
        };
        tui.request_render(None);
    }

    pub fn set_autocomplete_provider(&self, provider: AutocompleteProviderHandle) {
        self.cancel_autocomplete();
        let triggers = provider.trigger_characters().unwrap_or_default();
        {
            let mut fields = self.fields();
            fields.autocomplete_provider = Some(provider);
        }
        self.set_autocomplete_trigger_characters(&triggers);
    }

    /// Add a prompt to history for up/down arrow navigation.
    /// Called after successful submission.
    pub fn add_to_history(&self, text: &str) {
        let trimmed = pi_js::str16::trim(text);
        if trimmed.is_empty() {
            return;
        }
        let mut fields = self.fields();
        // Don't add consecutive duplicates
        if fields.history.first().is_some_and(|first| first == trimmed) {
            return;
        }
        fields.history.insert(0, trimmed.to_string());
        // Limit history size
        if fields.history.len() > 100 {
            fields.history.pop();
        }
    }

    /// No cached state to invalidate currently.
    pub fn invalidate(&self) {}

    /// PORT: `protected`. Public so another crate can delegate (§9.4).
    pub fn render_top_border(&self, width: usize, hidden_line_count: i64) -> String {
        let border = if hidden_line_count > 0 {
            create_scroll_border(ScrollBorderDirection::Up, hidden_line_count, width as i64)
        } else {
            "─".repeat(width)
        };
        let border_color = Arc::clone(&self.fields().border_color);
        border_color(&border)
    }

    /// PORT: `protected`. Public so another crate can delegate (§9.4).
    pub fn render_bottom_border(&self, width: usize, hidden_line_count: i64) -> String {
        let border = if hidden_line_count > 0 {
            create_scroll_border(ScrollBorderDirection::Down, hidden_line_count, width as i64)
        } else {
            "─".repeat(width)
        };
        let border_color = Arc::clone(&self.fields().border_color);
        border_color(&border)
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        let width_i64 = width as i64;
        let max_padding = (width_i64 - 1).div_euclid(2).max(0);
        let tui = Arc::clone(&self.fields().tui);
        let terminal_rows = tui.terminal().rows();
        let max_visible_lines = (((terminal_rows as f64) * 0.3).floor() as i64).max(5);

        struct RenderPlan {
            padding_x: usize,
            content_width: i64,
            scroll_offset: i64,
            visible_lines: Vec<LayoutLine>,
            lines_below: i64,
            emit_cursor_marker: bool,
            valid_ids: IndexSet<i64>,
            autocomplete_list: Option<SelectList>,
        }

        let plan = {
            let mut fields = self.fields();
            let padding_x = fields.padding_x.min(max_padding);
            let content_width = (width_i64 - padding_x * 2).max(1);
            // Layout width: with padding the cursor can overflow into it,
            // without padding we reserve 1 column for the cursor.
            let layout_width = (content_width - if padding_x != 0 { 0 } else { 1 }).max(1);
            // Store for cursor navigation (must match wrapping width)
            fields.last_width = layout_width;

            let layout_lines = fields.layout_text(layout_width);
            let layout_len = layout_lines.len() as i64;

            let cursor_line_index = layout_lines
                .iter()
                .position(|line| line.has_cursor)
                .map(|idx| idx as i64)
                .unwrap_or(0);

            // Adjust scroll offset to keep cursor visible
            if cursor_line_index < fields.scroll_offset {
                fields.scroll_offset = cursor_line_index;
            } else if cursor_line_index >= fields.scroll_offset + max_visible_lines {
                fields.scroll_offset = cursor_line_index - max_visible_lines + 1;
            }

            // Clamp scroll offset to valid range
            let max_scroll_offset = (layout_len - max_visible_lines).max(0);
            fields.scroll_offset = fields.scroll_offset.max(0).min(max_scroll_offset);

            let start = usize::try_from(fields.scroll_offset).unwrap_or(0);
            let end = usize::try_from(fields.scroll_offset + max_visible_lines)
                .unwrap_or(usize::MAX)
                .min(layout_lines.len());
            let visible_lines = layout_lines[start..end].to_vec();
            fields.rendered_visible_line_count = visible_lines.len() as i64;
            let lines_below = layout_len - (fields.scroll_offset + visible_lines.len() as i64);

            let autocomplete_list = if fields.autocomplete_state.is_some() {
                fields.autocomplete_list.clone()
            } else {
                None
            };
            if autocomplete_list.is_none() {
                fields.rendered_autocomplete_height = 0;
            }

            RenderPlan {
                padding_x: usize::try_from(padding_x).unwrap_or(0),
                content_width,
                scroll_offset: fields.scroll_offset,
                visible_lines,
                lines_below,
                emit_cursor_marker: fields.focused,
                valid_ids: fields.valid_paste_ids(),
                autocomplete_list,
            }
        };

        let mut result: Vec<String> = Vec::with_capacity(plan.visible_lines.len() + 2);
        let left_padding = " ".repeat(plan.padding_x);
        let right_padding = &left_padding;

        // Render top border (with scroll indicator if scrolled down)
        result.push(self.render_top_border(width, plan.scroll_offset));

        // Render each visible layout line
        // Emit hardware cursor marker when focused so TUI can position the
        // hardware cursor for IME candidate-window placement even while
        // autocomplete (e.g. slash-command menu) is visible.
        for layout_line in &plan.visible_lines {
            let mut display_text = layout_line.text.clone();
            let mut line_visible_width = visible_width(&layout_line.text);
            let mut cursor_in_padding = false;

            // Add cursor if this line has it
            if layout_line.has_cursor
                && let Some(cursor_pos) = layout_line.cursor_pos
            {
                let before = utf16_slice(&display_text, 0, Some(cursor_pos));
                let after = utf16_slice(&display_text, cursor_pos, None);

                // Hardware cursor marker (zero-width, emitted before fake cursor for IME positioning)
                let marker = if plan.emit_cursor_marker { CURSOR_MARKER } else { "" };

                if !after.is_empty() {
                    // Cursor is on a character (grapheme) - replace it with highlighted version
                    // Get the first grapheme from 'after'
                    let after_graphemes = segment_with_markers(&after, SegmentMode::Grapheme, &plan.valid_ids);
                    let first_grapheme = after_graphemes.first().map(|g| g.segment.as_str()).unwrap_or("");
                    let rest_after = utf16_slice(&after, utf16_len(first_grapheme), None);
                    let cursor = format!("\x1b[7m{first_grapheme}\x1b[0m");
                    display_text = format!("{before}{marker}{cursor}{rest_after}");
                    // lineVisibleWidth stays the same - we're replacing, not adding
                } else {
                    // Cursor is at the end - add highlighted space
                    display_text = format!("{before}{marker}\x1b[7m \x1b[0m");
                    line_visible_width += 1;
                    // If cursor overflows content width into the padding, flag it
                    if line_visible_width > plan.content_width && plan.padding_x > 0 {
                        cursor_in_padding = true;
                    }
                }
            }

            // Calculate padding based on actual visible width
            let padding_len = usize::try_from((plan.content_width - line_visible_width).max(0)).unwrap_or(0);
            let padding = " ".repeat(padding_len);
            let line_right_padding = if cursor_in_padding {
                right_padding.get(1..).unwrap_or("")
            } else {
                right_padding.as_str()
            };

            // Render the line (no side borders, just horizontal lines above and below)
            result.push(format!("{left_padding}{display_text}{padding}{line_right_padding}"));
        }

        // Render bottom border (with scroll indicator if more content below)
        result.push(self.render_bottom_border(width, plan.lines_below));

        // Add autocomplete list if active
        if let Some(list) = plan.autocomplete_list {
            let autocomplete_result = list.render(usize::try_from(plan.content_width).unwrap_or(1));
            self.fields().rendered_autocomplete_height = autocomplete_result.len() as i64;
            for line in autocomplete_result {
                let line_width = visible_width(&line);
                let padding_len = usize::try_from((plan.content_width - line_width).max(0)).unwrap_or(0);
                let line_padding = " ".repeat(padding_len);
                result.push(format!("{left_padding}{line}{line_padding}{right_padding}"));
            }
        }

        result
    }

    pub fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        let autocomplete_target = {
            let fields = self.fields();
            let autocomplete_start_row = fields.rendered_visible_line_count + 2;
            if fields.autocomplete_state.is_some()
                && let Some(list) = fields.autocomplete_list.as_ref()
                && event.y >= autocomplete_start_row
                && event.y < autocomplete_start_row + fields.rendered_autocomplete_height
            {
                let max_padding = (event.width - 1).div_euclid(2).max(0);
                let padding_x = fields.padding_x.min(max_padding);
                let content_width = (event.width - padding_x * 2).max(1);
                let sub_event = TuiMouseEvent {
                    x: event.x - padding_x,
                    y: event.y - autocomplete_start_row,
                    width: content_width,
                    height: fields.rendered_autocomplete_height,
                    ..*event
                };
                Some((list.clone(), sub_event))
            } else {
                None
            }
        };

        if let Some((list, sub_event)) = autocomplete_target {
            let result = list.handle_mouse(&sub_event);
            return result.map(|mut res| {
                res.focus = Some(true);
                res
            });
        }

        // Leave press/drag/release unhandled so the renderer's screen-level text
        // selection can run over the editor rows (drag to select, release to copy).
        // The renderer synthesizes a click when press and release land on the same
        // cell without movement, which is the gesture that positions the cursor.
        if event.r#type != TuiMouseEventType::Click || event.button != TuiMouseButton::Left {
            return None;
        }

        let should_update_autocomplete = {
            let mut fields = self.fields();
            if event.y <= 0 || event.y > fields.rendered_visible_line_count {
                return Some(TuiMouseEventResult {
                    handled: Some(true),
                    focus: Some(true),
                    ..Default::default()
                });
            }

            let visual_lines = fields.build_visual_line_map(fields.last_width);
            let visual_line_index = fields.scroll_offset + event.y - 1;
            let Ok(vl_idx_usize) = usize::try_from(visual_line_index) else {
                return Some(TuiMouseEventResult {
                    handled: Some(true),
                    focus: Some(true),
                    ..Default::default()
                });
            };
            let Some(&visual_line) = visual_lines.get(vl_idx_usize) else {
                return Some(TuiMouseEventResult {
                    handled: Some(true),
                    focus: Some(true),
                    ..Default::default()
                });
            };

            let logical_line = usize::try_from(visual_line.logical_line)
                .ok()
                .and_then(|idx| fields.state.lines.get(idx))
                .cloned()
                .unwrap_or_default();
            let chunk_end = visual_line.start_col + visual_line.length;
            let chunk = utf16_slice(&logical_line, visual_line.start_col, Some(chunk_end));
            let chunk_len = utf16_len(&chunk);
            let max_padding = (event.width - 1).div_euclid(2).max(0);
            let padding_x = fields.padding_x.min(max_padding);
            let target_column = (event.x - padding_x).max(0);
            let mut visible_column: i64 = 0;
            let mut target_index = chunk_len;
            let mut last_grapheme_index: i64 = 0;

            for grapheme in fields.segment(&chunk, SegmentMode::Grapheme) {
                let next_column = visible_column + visible_width(&grapheme.segment);
                last_grapheme_index = grapheme.index;
                if target_column < next_column {
                    target_index = grapheme.index;
                    break;
                }
                visible_column = next_column;
            }

            let is_last_segment = vl_idx_usize + 1 == visual_lines.len()
                || visual_lines
                    .get(vl_idx_usize + 1)
                    .is_some_and(|next| next.logical_line != visual_line.logical_line);
            if !is_last_segment && target_index == chunk_len && chunk_len > 0 {
                target_index = last_grapheme_index;
            }

            fields.state.cursor_line = visual_line.logical_line;
            fields.set_cursor_col(visual_line.start_col + target_index);
            fields.last_action = None;
            fields.exit_history_browsing();
            fields.autocomplete_state.is_some()
        };

        if should_update_autocomplete {
            self.update_autocomplete();
        }

        Some(TuiMouseEventResult {
            handled: Some(true),
            focus: Some(true),
            ..Default::default()
        })
    }

    pub fn handle_input(&self, data: &str) {
        let kb = get_keybindings();

        // Handle character jump mode (awaiting next character to jump to)
        let active_jump_mode = self.fields().jump_mode;
        if let Some(direction) = active_jump_mode {
            // Cancel if the hotkey is pressed again
            if kb.matches(data, "tui.editor.jumpForward") || kb.matches(data, "tui.editor.jumpBackward") {
                self.fields().jump_mode = None;
                return;
            }

            let printable = decode_printable_key(data).or_else(|| {
                if pi_js::str16::char_code_at(data, 0).is_some_and(|code| code >= 32) {
                    Some(data.to_string())
                } else {
                    None
                }
            });
            if let Some(printable) = printable {
                self.fields().jump_mode = None;
                self.jump_to_char(&printable, direction);
                return;
            }

            // Control character - cancel and fall through to normal handling
            self.fields().jump_mode = None;
        }

        // Handle bracketed paste mode
        let owned_data: Option<String>;
        let data = if data.contains("\x1b[200~") {
            let mut fields = self.fields();
            fields.is_in_paste = true;
            fields.paste_buffer.clear();
            owned_data = Some(data.replacen("\x1b[200~", "", 1));
            owned_data.as_deref().unwrap_or("")
        } else {
            data
        };

        let paste_finished = {
            let mut fields = self.fields();
            if fields.is_in_paste {
                fields.paste_buffer.push_str(data);
                if let Some(end_index) = fields.paste_buffer.find("\x1b[201~") {
                    let paste_content = fields.paste_buffer[..end_index].to_string();
                    let remaining = fields.paste_buffer[end_index + 6..].to_string();
                    fields.is_in_paste = false;
                    fields.paste_buffer.clear();
                    Some(Some((paste_content, remaining)))
                } else {
                    Some(None)
                }
            } else {
                None
            }
        };

        if let Some(finished) = paste_finished {
            if let Some((paste_content, remaining)) = finished {
                if !paste_content.is_empty() {
                    self.handle_paste(&paste_content);
                }
                if !remaining.is_empty() {
                    self.handle_input(&remaining);
                }
            }
            return;
        }

        // Ctrl+C - let parent handle (exit/clear)
        if kb.matches(data, "tui.input.copy") {
            return;
        }

        // Undo
        if kb.matches(data, "tui.editor.undo") {
            self.undo();
            return;
        }

        // Handle autocomplete mode
        let autocomplete_list = {
            let fields = self.fields();
            if fields.autocomplete_state.is_some() {
                fields.autocomplete_list.clone()
            } else {
                None
            }
        };

        if let Some(list) = autocomplete_list {
            if kb.matches(data, "tui.select.cancel") {
                self.cancel_autocomplete();
                return;
            }

            if kb.matches(data, "tui.select.up")
                || kb.matches(data, "tui.select.down")
                || kb.matches(data, "tui.select.pageUp")
                || kb.matches(data, "tui.select.pageDown")
            {
                list.handle_input(data);
                return;
            }

            if kb.matches(data, "tui.input.tab") {
                if let Some(selected) = list.get_selected_item() {
                    let item = AutocompleteItem {
                        value: selected.value,
                        label: selected.label,
                        description: selected.description,
                    };
                    let snapshot = {
                        let mut fields = self.fields();
                        if let Some(provider) = fields.autocomplete_provider.clone() {
                            fields.push_undo_snapshot();
                            fields.last_action = None;
                            Some((
                                provider,
                                fields.state.lines.clone(),
                                fields.state.cursor_line,
                                fields.state.cursor_col,
                                fields.autocomplete_prefix.clone(),
                            ))
                        } else {
                            None
                        }
                    };
                    if let Some((provider, lines, cursor_line, cursor_col, prefix)) = snapshot {
                        let result = provider.apply_completion(&lines, cursor_line, cursor_col, &item, &prefix);
                        let change = {
                            let mut fields = self.fields();
                            fields.state.lines = result.lines;
                            fields.state.cursor_line = result.cursor_line;
                            fields.set_cursor_col(result.cursor_col);
                            (fields.on_change.clone(), fields.get_text())
                        };
                        self.cancel_autocomplete();
                        if let Some(on_change) = change.0 {
                            on_change(&change.1);
                        }
                    }
                }
                return;
            }

            if kb.matches(data, "tui.select.confirm") {
                if let Some(selected) = list.get_selected_item() {
                    let item = AutocompleteItem {
                        value: selected.value,
                        label: selected.label,
                        description: selected.description,
                    };
                    let snapshot = {
                        let mut fields = self.fields();
                        if let Some(provider) = fields.autocomplete_provider.clone() {
                            fields.push_undo_snapshot();
                            fields.last_action = None;
                            Some((
                                provider,
                                fields.state.lines.clone(),
                                fields.state.cursor_line,
                                fields.state.cursor_col,
                                fields.autocomplete_prefix.clone(),
                            ))
                        } else {
                            None
                        }
                    };
                    if let Some((provider, lines, cursor_line, cursor_col, prefix)) = snapshot {
                        let result = provider.apply_completion(&lines, cursor_line, cursor_col, &item, &prefix);
                        let change = {
                            let mut fields = self.fields();
                            fields.state.lines = result.lines;
                            fields.state.cursor_line = result.cursor_line;
                            fields.set_cursor_col(result.cursor_col);
                            (fields.on_change.clone(), fields.get_text())
                        };
                        self.cancel_autocomplete();
                        if prefix.starts_with('/') {
                            // Fall through to submit
                        } else {
                            if let Some(on_change) = change.0 {
                                on_change(&change.1);
                            }
                            return;
                        }
                    }
                }
            }
        }

        // Tab - trigger completion
        if kb.matches(data, "tui.input.tab") && !self.is_showing_autocomplete() {
            self.handle_tab_completion();
            return;
        }

        // Deletion actions
        if kb.matches(data, "tui.editor.deleteToLineEnd") {
            self.delete_to_end_of_line();
            return;
        }
        if kb.matches(data, "tui.editor.deleteToLineStart") {
            self.delete_to_start_of_line();
            return;
        }
        if kb.matches(data, "tui.editor.deleteWordBackward") {
            self.delete_word_backwards();
            return;
        }
        if kb.matches(data, "tui.editor.deleteWordForward") {
            self.delete_word_forward();
            return;
        }
        if kb.matches(data, "tui.editor.deleteCharBackward") || matches_key(data, "shift+backspace") {
            self.handle_backspace();
            return;
        }
        if kb.matches(data, "tui.editor.deleteCharForward") || matches_key(data, "shift+delete") {
            self.handle_forward_delete();
            return;
        }

        // Kill ring actions
        if kb.matches(data, "tui.editor.yank") {
            self.yank();
            return;
        }
        if kb.matches(data, "tui.editor.yankPop") {
            self.yank_pop();
            return;
        }

        // Dedicated history actions always browse entries instead of moving the cursor.
        if kb.matches(data, "tui.editor.historyPrevious") {
            self.cancel_autocomplete();
            self.navigate_history(-1);
            return;
        }
        if kb.matches(data, "tui.editor.historyNext") {
            self.cancel_autocomplete();
            self.navigate_history(1);
            return;
        }

        // Cursor movement actions
        if kb.matches(data, "tui.editor.cursorLineStart") {
            self.move_to_line_start();
            return;
        }
        if kb.matches(data, "tui.editor.cursorLineEnd") {
            self.move_to_line_end();
            return;
        }
        if kb.matches(data, "tui.editor.cursorWordLeft") {
            self.move_word_backwards();
            return;
        }
        if kb.matches(data, "tui.editor.cursorWordRight") {
            self.move_word_forwards();
            return;
        }

        // New line
        let data_u16_len = pi_js::str16::len(data);
        if kb.matches(data, "tui.input.newLine")
            || (pi_js::str16::char_code_at(data, 0) == Some(10) && data_u16_len > 1)
            || data == "\x1b\r"
            || data == "\x1b[13;2~"
            || (data_u16_len > 1 && data.contains('\x1b') && data.contains('\r'))
            || (data == "\n" && data_u16_len == 1)
        {
            if self.should_submit_on_backslash_enter(data, &kb) {
                self.handle_backspace();
                self.submit_value();
                return;
            }
            self.add_new_line();
            return;
        }

        // Submit (Enter)
        if kb.matches(data, "tui.input.submit") {
            let (disable_submit, has_backslash) = {
                let fields = self.fields();
                if fields.disable_submit {
                    (true, false)
                } else {
                    let current_line = usize::try_from(fields.state.cursor_line)
                        .ok()
                        .and_then(|idx| fields.state.lines.get(idx))
                        .map(String::as_str)
                        .unwrap_or("");
                    let has_backslash = fields.state.cursor_col > 0
                        && pi_js::str16::char_at(current_line, (fields.state.cursor_col - 1) as usize) == "\\";
                    (false, has_backslash)
                }
            };
            if disable_submit {
                return;
            }

            // Workaround for terminals without Shift+Enter support:
            // If char before cursor is \, delete it and insert newline instead of submitting.
            if has_backslash {
                self.handle_backspace();
                self.add_new_line();
                return;
            }

            self.submit_value();
            return;
        }

        // Arrow key navigation (with history support)
        if kb.matches(data, "tui.editor.cursorUp") {
            enum UpAction {
                HistoryPrev,
                LineStart,
                MoveUp,
            }
            let action = {
                let fields = self.fields();
                if fields.is_on_first_visual_line()
                    && (fields.is_editor_empty() || fields.history_index > -1 || fields.state.cursor_col == 0)
                {
                    UpAction::HistoryPrev
                } else if fields.is_on_first_visual_line() {
                    UpAction::LineStart
                } else {
                    UpAction::MoveUp
                }
            };
            match action {
                UpAction::HistoryPrev => self.navigate_history(-1),
                UpAction::LineStart => self.move_to_line_start(),
                UpAction::MoveUp => self.move_cursor(-1, 0),
            }
            return;
        }
        if kb.matches(data, "tui.editor.cursorDown") {
            enum DownAction {
                HistoryNext,
                LineEnd,
                MoveDown,
            }
            let action = {
                let fields = self.fields();
                if fields.history_index > -1 && fields.is_on_last_visual_line() {
                    DownAction::HistoryNext
                } else if fields.is_on_last_visual_line() {
                    DownAction::LineEnd
                } else {
                    DownAction::MoveDown
                }
            };
            match action {
                DownAction::HistoryNext => self.navigate_history(1),
                DownAction::LineEnd => self.move_to_line_end(),
                DownAction::MoveDown => self.move_cursor(1, 0),
            }
            return;
        }
        if kb.matches(data, "tui.editor.cursorRight") {
            self.move_cursor(0, 1);
            return;
        }
        if kb.matches(data, "tui.editor.cursorLeft") {
            self.move_cursor(0, -1);
            return;
        }

        // Page up/down - scroll by page and move cursor
        if kb.matches(data, "tui.editor.pageUp") {
            self.page_scroll(-1);
            return;
        }
        if kb.matches(data, "tui.editor.pageDown") {
            self.page_scroll(1);
            return;
        }

        // Character jump mode triggers
        if kb.matches(data, "tui.editor.jumpForward") {
            self.fields().jump_mode = Some(JumpMode::Forward);
            return;
        }
        if kb.matches(data, "tui.editor.jumpBackward") {
            self.fields().jump_mode = Some(JumpMode::Backward);
            return;
        }

        // Shift+Space - insert regular space
        if matches_key(data, "shift+space") {
            self.insert_character(" ", None);
            return;
        }

        if let Some(printable) = decode_printable_key(data) {
            self.insert_character(&printable, None);
            return;
        }

        // Regular characters
        if pi_js::str16::char_code_at(data, 0).is_some_and(|code| code >= 32) {
            self.insert_character(data, None);
        }
    }

    pub fn get_text(&self) -> String {
        self.fields().get_text()
    }

    /// Get text with paste markers expanded to their actual content.
    /// Use this when you need the full content (e.g., for external editor).
    pub fn get_expanded_text(&self) -> String {
        let fields = self.fields();
        fields.expand_paste_markers(&fields.get_text())
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
        self.cancel_autocomplete();
        let change = {
            let mut fields = self.fields();
            fields.last_action = None;
            fields.exit_history_browsing();
            let normalized = EditorFields::normalize_text(text);
            // Push undo snapshot if content differs (makes programmatic changes undoable)
            if fields.get_text() != normalized {
                fields.push_undo_snapshot();
            }
            fields.pastes.clear();
            fields.paste_counter = 0;
            fields.set_text_internal(&normalized, CursorPlacement::End)
        };
        if let Some(on_change) = change.0 {
            on_change(&change.1);
        }
    }

    /// Insert text at the current cursor position.
    /// Used for programmatic insertion (e.g., clipboard image markers).
    /// This is atomic for undo - single undo restores entire pre-insert state.
    pub fn insert_text_at_cursor(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.cancel_autocomplete();
        let change = {
            let mut fields = self.fields();
            fields.push_undo_snapshot();
            fields.last_action = None;
            fields.exit_history_browsing();
            fields.insert_text_at_cursor_internal(text)
        };
        if let Some((Some(on_change), updated_text)) = change {
            on_change(&updated_text);
        }
    }

    pub fn is_showing_autocomplete(&self) -> bool {
        self.fields().autocomplete_state.is_some()
    }

    /// Set of currently valid paste IDs, for marker-aware segmentation.
    fn valid_paste_ids(&self) -> IndexSet<i64> {
        self.fields().valid_paste_ids()
    }

    /// Segment text with paste-marker awareness, only merging markers with valid IDs.
    fn segment(&self, text: &str, mode: SegmentMode) -> Vec<SegmentData> {
        self.fields().segment(text, mode)
    }

    fn is_editor_empty(&self) -> bool {
        self.fields().is_editor_empty()
    }

    fn is_on_first_visual_line(&self) -> bool {
        self.fields().is_on_first_visual_line()
    }

    fn is_on_last_visual_line(&self) -> bool {
        self.fields().is_on_last_visual_line()
    }

    /// `direction` is `1` or `-1`. Up (`-1`) increases the history index; down (`1`) decreases it.
    fn navigate_history(&self, direction: i64) {
        let change = {
            let mut fields = self.fields();
            fields.last_action = None;
            if fields.history.is_empty() {
                return;
            }

            let new_index = fields.history_index - direction;
            if new_index < -1 || new_index >= fields.history.len() as i64 {
                return;
            }

            // Capture state when first entering history browsing mode
            if fields.history_index == -1 && new_index >= 0 {
                fields.push_undo_snapshot();
                fields.history_draft = Some(fields.state.clone());
            }

            fields.history_index = new_index;

            if fields.history_index == -1 {
                if let Some(draft) = fields.history_draft.take() {
                    fields.state = draft;
                    fields.preferred_visual_col = None;
                    fields.snapped_from_cursor_col = None;
                    fields.scroll_offset = 0;
                    (fields.on_change.clone(), fields.get_text())
                } else {
                    fields.set_text_internal("", CursorPlacement::End)
                }
            } else {
                let entry = fields
                    .history
                    .get(usize::try_from(fields.history_index).unwrap_or(usize::MAX))
                    .cloned()
                    .unwrap_or_default();
                let placement = if direction == -1 {
                    CursorPlacement::Start
                } else {
                    CursorPlacement::End
                };
                fields.set_text_internal(&entry, placement)
            }
        };

        if let Some(on_change) = change.0 {
            on_change(&change.1);
        }
    }

    fn exit_history_browsing(&self) {
        self.fields().exit_history_browsing();
    }

    /// Internal setText that doesn't reset history state - used by navigateHistory.
    ///
    /// `cursor_placement` defaults to `"end"` inside the body.
    fn set_text_internal(&self, text: &str, cursor_placement: Option<CursorPlacement>) {
        let change = self
            .fields()
            .set_text_internal(text, cursor_placement.unwrap_or(CursorPlacement::End));
        if let Some(on_change) = change.0 {
            on_change(&change.1);
        }
    }

    fn layout_text(&self, content_width: i64) -> Vec<LayoutLine> {
        self.fields().layout_text(content_width)
    }

    fn expand_paste_markers(&self, text: &str) -> String {
        self.fields().expand_paste_markers(text)
    }

    /// Normalize text for editor storage:
    /// - Normalize line endings (\r\n and \r -> \n)
    /// - Expand tabs to 4 spaces
    fn normalize_text(&self, text: &str) -> String {
        EditorFields::normalize_text(text)
    }

    /// Internal text insertion at cursor. Handles single and multi-line text.
    /// Does not push undo snapshots or trigger autocomplete - caller is responsible.
    /// Normalizes line endings and calls onChange once at the end.
    fn insert_text_at_cursor_internal(&self, text: &str) {
        let change = self.fields().insert_text_at_cursor_internal(text);
        if let Some((Some(on_change), updated_text)) = change {
            on_change(&updated_text);
        }
    }

    /// `skip_undo_coalescing` defaults to false inside the body.
    fn insert_character(&self, ch: &str, skip_undo_coalescing: Option<bool>) {
        let skip = skip_undo_coalescing.unwrap_or(false);
        enum PostInsert {
            Update,
            TryTrigger,
            None,
        }

        let (change, post) = {
            let mut fields = self.fields();
            fields.exit_history_browsing();

            // Undo coalescing (fish-style):
            // - Consecutive word chars coalesce into one undo unit
            // - Space captures state before itself (so undo removes space+following word together)
            // - Each space is separately undoable
            // Skip coalescing when called from atomic operations (e.g., handlePaste)
            if !skip {
                if is_whitespace_char(ch) || fields.last_action != Some(LastAction::TypeWord) {
                    fields.push_undo_snapshot();
                }
                fields.last_action = Some(LastAction::TypeWord);
            }

            let line_idx = usize::try_from(fields.state.cursor_line).unwrap_or(0);
            let line = fields.state.lines.get(line_idx).cloned().unwrap_or_default();
            let before = utf16_slice(&line, 0, Some(fields.state.cursor_col));
            let after = utf16_slice(&line, fields.state.cursor_col, None);

            if line_idx < fields.state.lines.len() {
                fields.state.lines[line_idx] = format!("{before}{ch}{after}");
            }
            let new_col = fields.state.cursor_col + utf16_len(ch);
            fields.set_cursor_col(new_col);

            let change = (fields.on_change.clone(), fields.get_text());

            let post = if fields.autocomplete_state.is_none() {
                if ch == "/" && fields.is_at_start_of_message() {
                    PostInsert::TryTrigger
                } else if fields.autocomplete_trigger_characters.iter().any(|c| c == ch) {
                    let current_line = usize::try_from(fields.state.cursor_line)
                        .ok()
                        .and_then(|idx| fields.state.lines.get(idx))
                        .map(String::as_str)
                        .unwrap_or("");
                    let text_before_cursor = utf16_slice(current_line, 0, Some(fields.state.cursor_col));
                    if fields
                        .autocomplete_trigger_pattern
                        .is_match(&text_before_cursor)
                        .unwrap_or(false)
                    {
                        PostInsert::TryTrigger
                    } else {
                        PostInsert::None
                    }
                } else if ch
                    .chars()
                    .any(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
                    || cjk_break_regex.is_match(ch)
                {
                    let current_line = usize::try_from(fields.state.cursor_line)
                        .ok()
                        .and_then(|idx| fields.state.lines.get(idx))
                        .map(String::as_str)
                        .unwrap_or("");
                    let text_before_cursor = utf16_slice(current_line, 0, Some(fields.state.cursor_col));
                    if fields.is_in_slash_command_context(&text_before_cursor)
                        || fields
                            .autocomplete_trigger_pattern
                            .is_match(&text_before_cursor)
                            .unwrap_or(false)
                    {
                        PostInsert::TryTrigger
                    } else {
                        PostInsert::None
                    }
                } else {
                    PostInsert::None
                }
            } else {
                PostInsert::Update
            };

            (change, post)
        };

        if let Some(on_change) = change.0 {
            on_change(&change.1);
        }

        match post {
            PostInsert::Update => self.update_autocomplete(),
            PostInsert::TryTrigger => self.try_trigger_autocomplete(None),
            PostInsert::None => {}
        }
    }

    fn handle_paste(&self, pasted_text: &str) {
        self.cancel_autocomplete();

        let change = {
            let mut fields = self.fields();
            fields.exit_history_browsing();
            fields.last_action = None;
            fields.push_undo_snapshot();

            // Some terminals (e.g. tmux popups with extended-keys-format=csi-u) re-encode
            // control bytes inside bracketed paste as CSI-u Ctrl+<letter> sequences
            // (ESC [ <codepoint> ; 5 u). Decode those back to their literal byte so the
            // per-char filter below preserves newlines instead of stripping ESC and
            // leaking the printable tail (e.g. "[106;5u") into the editor.
            static CSI_U_CTRL_REGEX: LazyLock<regex::Regex> =
                LazyLock::new(|| regex::Regex::new(r"\x1b\[(\d+);5u").expect("CSI_U_CTRL_REGEX"));
            let decoded_text = CSI_U_CTRL_REGEX.replace_all(pasted_text, |caps: &regex::Captures| {
                if let Some(code) = caps.get(1).and_then(|m| m.as_str().parse::<u32>().ok()) {
                    if (97..=122).contains(&code)
                        && let Some(ch) = char::from_u32(code - 96)
                    {
                        return ch.to_string();
                    }
                    if (65..=90).contains(&code)
                        && let Some(ch) = char::from_u32(code - 64)
                    {
                        return ch.to_string();
                    }
                }
                caps[0].to_string()
            });

            // Clean the pasted text: normalize line endings, expand tabs
            let clean_text = EditorFields::normalize_text(&decoded_text);

            // Filter out non-printable characters except newlines
            let mut filtered_text: String = clean_text
                .chars()
                .filter(|&ch| ch == '\n' || (ch as u32) >= 32)
                .collect();

            // If pasting a file path (starts with /, ~, or .) and the character before
            // the cursor is a word character, prepend a space for better readability
            if filtered_text.starts_with('/') || filtered_text.starts_with('~') || filtered_text.starts_with('.') {
                let current_line = usize::try_from(fields.state.cursor_line)
                    .ok()
                    .and_then(|idx| fields.state.lines.get(idx))
                    .map(String::as_str)
                    .unwrap_or("");
                if fields.state.cursor_col > 0 {
                    let char_before = pi_js::str16::char_at(current_line, (fields.state.cursor_col - 1) as usize);
                    if char_before
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
                    {
                        filtered_text = format!(" {filtered_text}");
                    }
                }
            }

            // Split into lines to check for large paste
            let line_count = filtered_text.split('\n').count();
            let total_chars = utf16_len(&filtered_text);
            if line_count > 10 || total_chars > 1000 {
                // Store the paste and insert a marker
                fields.paste_counter += 1;
                let paste_id = fields.paste_counter;
                fields.pastes.insert(paste_id, filtered_text);

                // Insert marker like "[paste #1 +123 lines]" or "[paste #1 1234 chars]"
                let marker = if line_count > 10 {
                    format!("[paste #{paste_id} +{line_count} lines]")
                } else {
                    format!("[paste #{paste_id} {total_chars} chars]")
                };
                fields.insert_text_at_cursor_internal(&marker)
            } else {
                // Single or multi-line paste - insert atomically (do not trigger autocomplete during paste)
                fields.insert_text_at_cursor_internal(&filtered_text)
            }
        };

        if let Some((Some(on_change), updated_text)) = change {
            on_change(&updated_text);
        }
    }

    fn add_new_line(&self) {
        self.cancel_autocomplete();
        let change = {
            let mut fields = self.fields();
            fields.exit_history_browsing();
            fields.last_action = None;
            fields.push_undo_snapshot();

            let line_idx = usize::try_from(fields.state.cursor_line).unwrap_or(0);
            let current_line = fields.state.lines.get(line_idx).cloned().unwrap_or_default();
            let before = utf16_slice(&current_line, 0, Some(fields.state.cursor_col));
            let after = utf16_slice(&current_line, fields.state.cursor_col, None);

            // Split current line
            if line_idx < fields.state.lines.len() {
                fields.state.lines[line_idx] = before;
                fields.state.lines.insert(line_idx + 1, after);
            } else {
                fields.state.lines.push(after);
            }

            // Move cursor to start of new line
            fields.state.cursor_line += 1;
            fields.set_cursor_col(0);

            (fields.on_change.clone(), fields.get_text())
        };
        if let Some(on_change) = change.0 {
            on_change(&change.1);
        }
    }

    fn should_submit_on_backslash_enter(&self, data: &str, kb: &KeybindingsManager) -> bool {
        let submit_keys = kb.get_keys("tui.input.submit");
        let has_shift_enter = submit_keys
            .iter()
            .any(|key| key == "shift+enter" || key == "shift+return");
        let fields = self.fields();
        if fields.disable_submit {
            return false;
        }
        if !matches_key(data, "enter") {
            return false;
        }
        if !has_shift_enter {
            return false;
        }
        let current_line = usize::try_from(fields.state.cursor_line)
            .ok()
            .and_then(|idx| fields.state.lines.get(idx))
            .map(String::as_str)
            .unwrap_or("");
        fields.state.cursor_col > 0
            && pi_js::str16::char_at(current_line, (fields.state.cursor_col - 1) as usize) == "\\"
    }

    fn submit_value(&self) {
        self.cancel_autocomplete();
        let (on_change, on_submit, result) = {
            let mut fields = self.fields();
            let raw = fields.get_text();
            let result = pi_js::str16::trim(&fields.expand_paste_markers(&raw)).to_string();

            fields.state = EditorState {
                lines: vec![String::new()],
                cursor_line: 0,
                cursor_col: 0,
            };
            fields.pastes.clear();
            fields.paste_counter = 0;
            fields.exit_history_browsing();
            fields.scroll_offset = 0;
            fields.undo_stack.clear();
            fields.last_action = None;

            (fields.on_change.clone(), fields.on_submit.clone(), result)
        };
        if let Some(cb) = on_change {
            cb("");
        }
        if let Some(cb) = on_submit {
            cb(&result);
        }
    }

    fn handle_backspace(&self) {
        enum PostBackspace {
            Update,
            TryTrigger,
            None,
        }

        let (change, post) = {
            let mut fields = self.fields();
            fields.exit_history_browsing();
            fields.last_action = None;

            if fields.state.cursor_col > 0 {
                fields.push_undo_snapshot();

                let line_idx = usize::try_from(fields.state.cursor_line).unwrap_or(0);
                let line = fields.state.lines.get(line_idx).cloned().unwrap_or_default();
                let before_cursor = utf16_slice(&line, 0, Some(fields.state.cursor_col));

                // Find the last grapheme in the text before cursor
                let graphemes = fields.segment(&before_cursor, SegmentMode::Grapheme);
                let last_grapheme = graphemes.last();
                let grapheme_length = last_grapheme.map(|g| utf16_len(&g.segment)).unwrap_or(1);

                if let Some(last) = last_grapheme
                    && let Some(caps) = PASTE_MARKER_SINGLE.captures(&last.segment)
                    && let Some(target_id) = caps.get(1).and_then(|m| m.as_str().parse::<i64>().ok())
                {
                    fields.pastes.shift_remove(&target_id);
                    fields.paste_counter -= 1;

                    // Shift registry entries down in ascending id order, independent
                    // of marker order in the text ([paste #3] becomes [paste #2] when
                    // [paste #1] is removed).
                    let mut higher_ids: Vec<i64> = fields.pastes.keys().copied().filter(|&id| id > target_id).collect();
                    higher_ids.sort_unstable();
                    for id in higher_ids {
                        if let Some(content) = fields.pastes.shift_remove(&id) {
                            fields.pastes.insert(id - 1, content);
                        }
                    }

                    // Renumber markers with ids greater than the removed one.
                    fields.state.lines = fields
                        .state
                        .lines
                        .iter()
                        .map(|l| {
                            PASTE_MARKER_REGEX
                                .replace_all(l, |m: &regex::Captures| {
                                    let x = m.get(1).and_then(|g| g.as_str().parse::<i64>().ok()).unwrap_or(0);
                                    if x <= target_id {
                                        m[0].to_string()
                                    } else {
                                        let suffix = m.get(2).map_or("", |g| g.as_str());
                                        format!("[paste #{}{}]", x - 1, suffix)
                                    }
                                })
                                .into_owned()
                        })
                        .collect();
                }

                let updated_line = fields.state.lines.get(line_idx).cloned().unwrap_or_default();
                let before = utf16_slice(&updated_line, 0, Some(fields.state.cursor_col - grapheme_length));
                let after = utf16_slice(&updated_line, fields.state.cursor_col, None);
                if line_idx < fields.state.lines.len() {
                    fields.state.lines[line_idx] = format!("{before}{after}");
                }
                let new_col = fields.state.cursor_col - grapheme_length;
                fields.set_cursor_col(new_col);
            } else if fields.state.cursor_line > 0 {
                fields.push_undo_snapshot();

                let line_idx = usize::try_from(fields.state.cursor_line).unwrap_or(1);
                let current_line = fields.state.lines.get(line_idx).cloned().unwrap_or_default();
                let previous_line = fields.state.lines.get(line_idx - 1).cloned().unwrap_or_default();
                let prev_len = utf16_len(&previous_line);

                fields.state.lines[line_idx - 1] = format!("{previous_line}{current_line}");
                fields.state.lines.remove(line_idx);
                fields.state.cursor_line -= 1;
                fields.set_cursor_col(prev_len);
            }

            let change = (fields.on_change.clone(), fields.get_text());
            let post = if fields.autocomplete_state.is_some() {
                PostBackspace::Update
            } else {
                let current_line = usize::try_from(fields.state.cursor_line)
                    .ok()
                    .and_then(|idx| fields.state.lines.get(idx))
                    .map(String::as_str)
                    .unwrap_or("");
                let text_before_cursor = utf16_slice(current_line, 0, Some(fields.state.cursor_col));
                if fields.is_in_slash_command_context(&text_before_cursor)
                    || fields
                        .autocomplete_trigger_pattern
                        .is_match(&text_before_cursor)
                        .unwrap_or(false)
                {
                    PostBackspace::TryTrigger
                } else {
                    PostBackspace::None
                }
            };
            (change, post)
        };

        if let Some(on_change) = change.0 {
            on_change(&change.1);
        }

        match post {
            PostBackspace::Update => self.update_autocomplete(),
            PostBackspace::TryTrigger => self.try_trigger_autocomplete(None),
            PostBackspace::None => {}
        }
    }

    fn set_cursor_col(&self, col: i64) {
        self.fields().set_cursor_col(col);
    }

    /// Move cursor to a target visual line, applying sticky column logic.
    /// Shared by moveCursor() and pageScroll().
    fn move_to_visual_line(&self, visual_lines: &[VisualLine], current_visual_line: i64, target_visual_line: i64) {
        self.fields()
            .move_to_visual_line(visual_lines, current_visual_line, target_visual_line);
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
        self.fields()
            .compute_vertical_move_column(current_visual_col, source_max_visual_col, target_max_visual_col)
    }

    fn move_to_line_start(&self) {
        let mut fields = self.fields();
        fields.last_action = None;
        fields.set_cursor_col(0);
    }

    fn move_to_line_end(&self) {
        let mut fields = self.fields();
        fields.last_action = None;
        let current_line_len = usize::try_from(fields.state.cursor_line)
            .ok()
            .and_then(|idx| fields.state.lines.get(idx))
            .map(|line| utf16_len(line))
            .unwrap_or(0);
        fields.set_cursor_col(current_line_len);
    }

    fn delete_to_start_of_line(&self) {
        let change = {
            let mut fields = self.fields();
            fields.exit_history_browsing();

            let line_idx = usize::try_from(fields.state.cursor_line).unwrap_or(0);
            let current_line = fields.state.lines.get(line_idx).cloned().unwrap_or_default();

            if fields.state.cursor_col > 0 {
                fields.push_undo_snapshot();

                // Calculate text to be deleted and save to kill ring (backward deletion = prepend)
                let deleted_text = utf16_slice(&current_line, 0, Some(fields.state.cursor_col));
                let accumulate = fields.last_action == Some(LastAction::Kill);
                fields.kill_ring.push(
                    &deleted_text,
                    KillRingPushOptions {
                        prepend: true,
                        accumulate: Some(accumulate),
                    },
                );
                fields.last_action = Some(LastAction::Kill);

                // Delete from start of line up to cursor
                if line_idx < fields.state.lines.len() {
                    fields.state.lines[line_idx] = utf16_slice(&current_line, fields.state.cursor_col, None);
                }
                fields.set_cursor_col(0);
            } else if fields.state.cursor_line > 0 {
                fields.push_undo_snapshot();

                // At start of line - merge with previous line, treating newline as deleted text
                let accumulate = fields.last_action == Some(LastAction::Kill);
                fields.kill_ring.push(
                    "\n",
                    KillRingPushOptions {
                        prepend: true,
                        accumulate: Some(accumulate),
                    },
                );
                fields.last_action = Some(LastAction::Kill);

                let previous_line = fields.state.lines.get(line_idx - 1).cloned().unwrap_or_default();
                let prev_len = utf16_len(&previous_line);
                fields.state.lines[line_idx - 1] = format!("{previous_line}{current_line}");
                fields.state.lines.remove(line_idx);
                fields.state.cursor_line -= 1;
                fields.set_cursor_col(prev_len);
            }

            (fields.on_change.clone(), fields.get_text())
        };

        if let Some(on_change) = change.0 {
            on_change(&change.1);
        }
    }

    fn delete_to_end_of_line(&self) {
        let change = {
            let mut fields = self.fields();
            fields.exit_history_browsing();

            let line_idx = usize::try_from(fields.state.cursor_line).unwrap_or(0);
            let current_line = fields.state.lines.get(line_idx).cloned().unwrap_or_default();
            let line_len = utf16_len(&current_line);

            if fields.state.cursor_col < line_len {
                fields.push_undo_snapshot();

                // Calculate text to be deleted and save to kill ring (forward deletion = append)
                let deleted_text = utf16_slice(&current_line, fields.state.cursor_col, None);
                let accumulate = fields.last_action == Some(LastAction::Kill);
                fields.kill_ring.push(
                    &deleted_text,
                    KillRingPushOptions {
                        prepend: false,
                        accumulate: Some(accumulate),
                    },
                );
                fields.last_action = Some(LastAction::Kill);

                // Delete from cursor to end of line
                if line_idx < fields.state.lines.len() {
                    fields.state.lines[line_idx] = utf16_slice(&current_line, 0, Some(fields.state.cursor_col));
                }
            } else if fields.state.cursor_line < fields.state.lines.len() as i64 - 1 {
                fields.push_undo_snapshot();

                // At end of line - merge with next line, treating newline as deleted text
                let accumulate = fields.last_action == Some(LastAction::Kill);
                fields.kill_ring.push(
                    "\n",
                    KillRingPushOptions {
                        prepend: false,
                        accumulate: Some(accumulate),
                    },
                );
                fields.last_action = Some(LastAction::Kill);

                let next_line = fields.state.lines.get(line_idx + 1).cloned().unwrap_or_default();
                fields.state.lines[line_idx] = format!("{current_line}{next_line}");
                fields.state.lines.remove(line_idx + 1);
            }

            (fields.on_change.clone(), fields.get_text())
        };

        if let Some(on_change) = change.0 {
            on_change(&change.1);
        }
    }

    fn delete_word_backwards(&self) {
        let change = {
            let mut fields = self.fields();
            fields.exit_history_browsing();

            let line_idx = usize::try_from(fields.state.cursor_line).unwrap_or(0);
            let current_line = fields.state.lines.get(line_idx).cloned().unwrap_or_default();

            // If at start of line, behave like backspace at column 0 (merge with previous line)
            if fields.state.cursor_col == 0 {
                if fields.state.cursor_line > 0 {
                    fields.push_undo_snapshot();

                    // Treat newline as deleted text (backward deletion = prepend)
                    let accumulate = fields.last_action == Some(LastAction::Kill);
                    fields.kill_ring.push(
                        "\n",
                        KillRingPushOptions {
                            prepend: true,
                            accumulate: Some(accumulate),
                        },
                    );
                    fields.last_action = Some(LastAction::Kill);

                    let previous_line = fields.state.lines.get(line_idx - 1).cloned().unwrap_or_default();
                    let prev_len = utf16_len(&previous_line);
                    fields.state.lines[line_idx - 1] = format!("{previous_line}{current_line}");
                    fields.state.lines.remove(line_idx);
                    fields.state.cursor_line -= 1;
                    fields.set_cursor_col(prev_len);
                }
            } else {
                fields.push_undo_snapshot();

                // Save lastAction before cursor movement (moveWordBackwards resets it)
                let was_kill = fields.last_action == Some(LastAction::Kill);

                let old_cursor_col = fields.state.cursor_col;
                fields.move_word_backwards();
                let delete_from = fields.state.cursor_col;
                fields.set_cursor_col(old_cursor_col);

                let deleted_text = utf16_slice(&current_line, delete_from, Some(fields.state.cursor_col));
                fields.kill_ring.push(
                    &deleted_text,
                    KillRingPushOptions {
                        prepend: true,
                        accumulate: Some(was_kill),
                    },
                );
                fields.last_action = Some(LastAction::Kill);

                let before = utf16_slice(&current_line, 0, Some(delete_from));
                let after = utf16_slice(&current_line, fields.state.cursor_col, None);
                if line_idx < fields.state.lines.len() {
                    fields.state.lines[line_idx] = format!("{before}{after}");
                }
                fields.set_cursor_col(delete_from);
            }

            (fields.on_change.clone(), fields.get_text())
        };

        if let Some(on_change) = change.0 {
            on_change(&change.1);
        }
    }

    fn delete_word_forward(&self) {
        let change = {
            let mut fields = self.fields();
            fields.exit_history_browsing();

            let line_idx = usize::try_from(fields.state.cursor_line).unwrap_or(0);
            let current_line = fields.state.lines.get(line_idx).cloned().unwrap_or_default();
            let line_len = utf16_len(&current_line);

            // If at end of line, merge with next line (delete the newline)
            if fields.state.cursor_col >= line_len {
                if fields.state.cursor_line < fields.state.lines.len() as i64 - 1 {
                    fields.push_undo_snapshot();

                    // Treat newline as deleted text (forward deletion = append)
                    let accumulate = fields.last_action == Some(LastAction::Kill);
                    fields.kill_ring.push(
                        "\n",
                        KillRingPushOptions {
                            prepend: false,
                            accumulate: Some(accumulate),
                        },
                    );
                    fields.last_action = Some(LastAction::Kill);

                    let next_line = fields.state.lines.get(line_idx + 1).cloned().unwrap_or_default();
                    fields.state.lines[line_idx] = format!("{current_line}{next_line}");
                    fields.state.lines.remove(line_idx + 1);
                }
            } else {
                fields.push_undo_snapshot();

                // Save lastAction before cursor movement (moveWordForwards resets it)
                let was_kill = fields.last_action == Some(LastAction::Kill);

                let old_cursor_col = fields.state.cursor_col;
                fields.move_word_forwards();
                let delete_to = fields.state.cursor_col;
                fields.set_cursor_col(old_cursor_col);

                let deleted_text = utf16_slice(&current_line, fields.state.cursor_col, Some(delete_to));
                fields.kill_ring.push(
                    &deleted_text,
                    KillRingPushOptions {
                        prepend: false,
                        accumulate: Some(was_kill),
                    },
                );
                fields.last_action = Some(LastAction::Kill);

                let before = utf16_slice(&current_line, 0, Some(fields.state.cursor_col));
                let after = utf16_slice(&current_line, delete_to, None);
                if line_idx < fields.state.lines.len() {
                    fields.state.lines[line_idx] = format!("{before}{after}");
                }
            }

            (fields.on_change.clone(), fields.get_text())
        };

        if let Some(on_change) = change.0 {
            on_change(&change.1);
        }
    }

    fn handle_forward_delete(&self) {
        enum PostDelete {
            Update,
            TryTrigger,
            None,
        }

        let (change, post) = {
            let mut fields = self.fields();
            fields.exit_history_browsing();
            fields.last_action = None;

            let line_idx = usize::try_from(fields.state.cursor_line).unwrap_or(0);
            let current_line = fields.state.lines.get(line_idx).cloned().unwrap_or_default();
            let line_len = utf16_len(&current_line);

            if fields.state.cursor_col < line_len {
                fields.push_undo_snapshot();

                // Delete grapheme at cursor position (handles emojis, combining characters, etc.)
                let after_cursor = utf16_slice(&current_line, fields.state.cursor_col, None);

                // Find the first grapheme at cursor
                let graphemes = fields.segment(&after_cursor, SegmentMode::Grapheme);
                let grapheme_length = graphemes.first().map(|g| utf16_len(&g.segment)).unwrap_or(1);

                let before = utf16_slice(&current_line, 0, Some(fields.state.cursor_col));
                let after = utf16_slice(&current_line, fields.state.cursor_col + grapheme_length, None);
                if line_idx < fields.state.lines.len() {
                    fields.state.lines[line_idx] = format!("{before}{after}");
                }
            } else if fields.state.cursor_line < fields.state.lines.len() as i64 - 1 {
                fields.push_undo_snapshot();

                // At end of line - merge with next line
                let next_line = fields.state.lines.get(line_idx + 1).cloned().unwrap_or_default();
                fields.state.lines[line_idx] = format!("{current_line}{next_line}");
                fields.state.lines.remove(line_idx + 1);
            }

            let change = (fields.on_change.clone(), fields.get_text());
            let post = if fields.autocomplete_state.is_some() {
                PostDelete::Update
            } else {
                let updated_line = fields.state.lines.get(line_idx).map(String::as_str).unwrap_or("");
                let text_before_cursor = utf16_slice(updated_line, 0, Some(fields.state.cursor_col));
                if fields.is_in_slash_command_context(&text_before_cursor)
                    || fields
                        .autocomplete_trigger_pattern
                        .is_match(&text_before_cursor)
                        .unwrap_or(false)
                {
                    PostDelete::TryTrigger
                } else {
                    PostDelete::None
                }
            };
            (change, post)
        };

        if let Some(on_change) = change.0 {
            on_change(&change.1);
        }

        match post {
            PostDelete::Update => self.update_autocomplete(),
            PostDelete::TryTrigger => self.try_trigger_autocomplete(None),
            PostDelete::None => {}
        }
    }

    /// Build a mapping from visual lines to logical positions.
    /// Returns an array where each element represents a visual line with:
    /// - logicalLine: index into this.state.lines
    /// - startCol: starting column in the logical line
    /// - length: length of this visual line segment
    fn build_visual_line_map(&self, width: i64) -> Vec<VisualLine> {
        self.fields().build_visual_line_map(width)
    }

    /// Find the visual line index that contains the given logical position.
    fn find_visual_line_at(&self, visual_lines: &[VisualLine], line: i64, col: i64) -> i64 {
        EditorFields::find_visual_line_at(visual_lines, line, col)
    }

    /// Find the visual line index for the current cursor position.
    fn find_current_visual_line(&self, visual_lines: &[VisualLine]) -> i64 {
        self.fields().find_current_visual_line(visual_lines)
    }

    fn move_cursor(&self, delta_line: i64, delta_col: i64) {
        let should_update_autocomplete = {
            let mut fields = self.fields();
            fields.last_action = None;
            let visual_lines = fields.build_visual_line_map(fields.last_width);
            let current_visual_line = fields.find_current_visual_line(&visual_lines);

            if delta_line != 0 {
                let target_visual_line = current_visual_line + delta_line;
                if target_visual_line >= 0 && target_visual_line < visual_lines.len() as i64 {
                    fields.move_to_visual_line(&visual_lines, current_visual_line, target_visual_line);
                }
            }

            if delta_col != 0 {
                let line_idx = usize::try_from(fields.state.cursor_line).unwrap_or(0);
                let current_line = fields.state.lines.get(line_idx).cloned().unwrap_or_default();
                let current_line_len = utf16_len(&current_line);

                if delta_col > 0 {
                    // Moving right - move by one grapheme (handles emojis, combining characters, etc.)
                    if fields.state.cursor_col < current_line_len {
                        let after_cursor = utf16_slice(&current_line, fields.state.cursor_col, None);
                        let graphemes = fields.segment(&after_cursor, SegmentMode::Grapheme);
                        let step = graphemes.first().map(|g| utf16_len(&g.segment)).unwrap_or(1);
                        let next_col = fields.state.cursor_col + step;
                        fields.set_cursor_col(next_col);
                    } else if fields.state.cursor_line < fields.state.lines.len() as i64 - 1 {
                        // Wrap to start of next logical line
                        fields.state.cursor_line += 1;
                        fields.set_cursor_col(0);
                    } else {
                        // At end of last line - can't move, but set preferredVisualCol for up/down navigation
                        if let Some(current_vl) = usize::try_from(current_visual_line)
                            .ok()
                            .and_then(|idx| visual_lines.get(idx))
                        {
                            fields.preferred_visual_col = Some(fields.state.cursor_col - current_vl.start_col);
                        }
                    }
                } else {
                    // Moving left - move by one grapheme (handles emojis, combining characters, etc.)
                    if fields.state.cursor_col > 0 {
                        let before_cursor = utf16_slice(&current_line, 0, Some(fields.state.cursor_col));
                        let graphemes = fields.segment(&before_cursor, SegmentMode::Grapheme);
                        let step = graphemes.last().map(|g| utf16_len(&g.segment)).unwrap_or(1);
                        let next_col = fields.state.cursor_col - step;
                        fields.set_cursor_col(next_col);
                    } else if fields.state.cursor_line > 0 {
                        // Wrap to end of previous logical line
                        fields.state.cursor_line -= 1;
                        let prev_len = usize::try_from(fields.state.cursor_line)
                            .ok()
                            .and_then(|idx| fields.state.lines.get(idx))
                            .map(|l| utf16_len(l))
                            .unwrap_or(0);
                        fields.set_cursor_col(prev_len);
                    }
                }
            }

            fields.autocomplete_state.is_some()
        };

        // Keep an open autocomplete picker in sync with the new cursor position
        if should_update_autocomplete {
            self.update_autocomplete();
        }
    }

    /// Scroll by a page (direction: -1 for up, 1 for down).
    /// Moves cursor by the page size while keeping it in bounds.
    fn page_scroll(&self, direction: i64) {
        let tui = Arc::clone(&self.fields().tui);
        let terminal_rows = tui.terminal().rows();
        let page_size = (((terminal_rows as f64) * 0.3).floor() as i64).max(5);

        let mut fields = self.fields();
        fields.last_action = None;

        let visual_lines = fields.build_visual_line_map(fields.last_width);
        let current_visual_line = fields.find_current_visual_line(&visual_lines);
        let max_vl = (visual_lines.len() as i64 - 1).max(0);
        let target_visual_line = (current_visual_line + direction * page_size).max(0).min(max_vl);

        fields.move_to_visual_line(&visual_lines, current_visual_line, target_visual_line);
    }

    fn move_word_backwards(&self) {
        self.fields().move_word_backwards();
    }

    /// Yank (paste) the most recent kill ring entry at cursor position.
    fn yank(&self) {
        let change = {
            let mut fields = self.fields();
            if fields.kill_ring.length() == 0 {
                return;
            }
            let Some(text) = fields.kill_ring.peek() else {
                return;
            };
            fields.push_undo_snapshot();
            let changed = fields.insert_yanked_text(&text);
            fields.last_action = Some(LastAction::Yank);
            changed
        };
        if let Some(on_change) = change.0 {
            on_change(&change.1);
        }
    }

    /// Cycle through kill ring (only works immediately after yank or yank-pop).
    /// Replaces the last yanked text with the previous entry in the ring.
    fn yank_pop(&self) {
        let change = {
            let mut fields = self.fields();
            // Only works if we just yanked and have more than one entry
            if fields.last_action != Some(LastAction::Yank) || fields.kill_ring.length() <= 1 {
                return;
            }
            fields.push_undo_snapshot();

            // Delete the previously yanked text (still at end of ring before rotation)
            fields.delete_yanked_text();

            // Rotate the ring: move end to front
            fields.kill_ring.rotate();

            // Insert the new most recent entry (now at end after rotation)
            let change = if let Some(text) = fields.kill_ring.peek() {
                fields.insert_yanked_text(&text)
            } else {
                (fields.on_change.clone(), fields.get_text())
            };
            fields.last_action = Some(LastAction::Yank);
            change
        };

        if let Some(on_change) = change.0 {
            on_change(&change.1);
        }
    }

    /// Insert text at cursor position (used by yank operations).
    fn insert_yanked_text(&self, text: &str) {
        let change = self.fields().insert_yanked_text(text);
        if let Some(on_change) = change.0 {
            on_change(&change.1);
        }
    }

    /// Delete the previously yanked text (used by yank-pop).
    /// The yanked text is derived from killRing[end] since it hasn't been rotated yet.
    fn delete_yanked_text(&self) {
        let change = self.fields().delete_yanked_text();
        if let Some((Some(on_change), text)) = change {
            on_change(&text);
        }
    }

    fn push_undo_snapshot(&self) {
        self.fields().push_undo_snapshot();
    }

    fn undo(&self) {
        let change = {
            let mut fields = self.fields();
            fields.exit_history_browsing();
            let Some(snapshot) = fields.undo_stack.pop() else {
                return;
            };
            fields.state = snapshot.state;
            fields.pastes = snapshot.pastes;
            fields.paste_counter = snapshot.paste_counter;
            fields.last_action = None;
            fields.preferred_visual_col = None;
            (fields.on_change.clone(), fields.get_text())
        };

        if let Some(on_change) = change.0 {
            on_change(&change.1);
        }
    }

    /// Jump to the first occurrence of a character in the specified direction.
    /// Multi-line search. Case-sensitive. Skips the current cursor position.
    fn jump_to_char(&self, ch: &str, direction: JumpMode) {
        let mut fields = self.fields();
        fields.last_action = None;
        let is_forward = matches!(direction, JumpMode::Forward);
        let lines_len = fields.state.lines.len() as i64;
        let step: i64 = if is_forward { 1 } else { -1 };

        let mut line_idx = fields.state.cursor_line;
        while (is_forward && line_idx < lines_len) || (!is_forward && line_idx >= 0) {
            let line = usize::try_from(line_idx)
                .ok()
                .and_then(|idx| fields.state.lines.get(idx))
                .map(String::as_str)
                .unwrap_or("");
            let is_current_line = line_idx == fields.state.cursor_line;

            let idx = if is_forward {
                let from = if is_current_line {
                    usize::try_from((fields.state.cursor_col + 1).max(0)).unwrap_or(0)
                } else {
                    0
                };
                pi_js::str16::index_of(line, ch, from)
            } else if is_current_line {
                let from = usize::try_from((fields.state.cursor_col - 1).max(0)).unwrap_or(0);
                pi_js::str16::last_index_of_from(line, ch, from)
            } else {
                pi_js::str16::last_index_of(line, ch)
            };

            if let Some(found) = idx {
                fields.state.cursor_line = line_idx;
                fields.set_cursor_col(found as i64);
                return;
            }

            line_idx += step;
        }
    }

    fn move_word_forwards(&self) {
        self.fields().move_word_forwards();
    }

    /// Slash menu only allowed on the first line of the editor.
    fn is_slash_menu_allowed(&self) -> bool {
        self.fields().is_slash_menu_allowed()
    }

    /// Helper method to check if cursor is at start of message (for slash command detection).
    fn is_at_start_of_message(&self) -> bool {
        self.fields().is_at_start_of_message()
    }

    fn is_in_slash_command_context(&self, text_before_cursor: &str) -> bool {
        self.fields().is_in_slash_command_context(text_before_cursor)
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
        if prefix.is_empty() {
            return -1;
        }

        let mut first_prefix_index: i64 = -1;
        for (i, item) in items.iter().enumerate() {
            if item.value == prefix {
                return i as i64;
            }
            if first_prefix_index == -1 && item.value.starts_with(prefix) {
                first_prefix_index = i as i64;
            }
        }
        first_prefix_index
    }

    fn create_autocomplete_list(&self, prefix: &str, items: &[AutocompleteItem]) -> SelectList {
        let layout = if prefix.starts_with('/') {
            Some((*SLASH_COMMAND_SELECT_LIST_LAYOUT).clone())
        } else {
            None
        };
        let select_items: Vec<SelectItem> = items
            .iter()
            .map(|item| SelectItem {
                value: item.value.clone(),
                label: item.label.clone(),
                description: item.description.clone(),
            })
            .collect();
        let (max_visible, select_list_theme) = {
            let fields = self.fields();
            (fields.autocomplete_max_visible, fields.theme.select_list.clone())
        };
        let list = SelectList::new(select_items, max_visible, select_list_theme, layout);
        let editor_weak = Arc::downgrade(&self.inner);
        list.set_on_select(Some(Arc::new(move |selected: &SelectItem| {
            let Some(inner) = editor_weak.upgrade() else {
                return;
            };
            let editor = Editor { inner };
            let item = AutocompleteItem {
                value: selected.value.clone(),
                label: selected.label.clone(),
                description: selected.description.clone(),
            };
            let snapshot = {
                let mut fields = editor.fields();
                if let Some(provider) = fields.autocomplete_provider.clone() {
                    fields.push_undo_snapshot();
                    fields.last_action = None;
                    Some((
                        provider,
                        fields.state.lines.clone(),
                        fields.state.cursor_line,
                        fields.state.cursor_col,
                        fields.autocomplete_prefix.clone(),
                    ))
                } else {
                    None
                }
            };
            if let Some((provider, lines, cursor_line, cursor_col, prefix)) = snapshot {
                let result = provider.apply_completion(&lines, cursor_line, cursor_col, &item, &prefix);
                let change = {
                    let mut fields = editor.fields();
                    fields.state.lines = result.lines;
                    fields.state.cursor_line = result.cursor_line;
                    fields.set_cursor_col(result.cursor_col);
                    (fields.on_change.clone(), fields.get_text())
                };
                editor.cancel_autocomplete();
                if let Some(on_change) = change.0 {
                    on_change(&change.1);
                }
            }
        })));
        list
    }

    /// `explicit_tab` defaults to false inside the body.
    fn try_trigger_autocomplete(&self, explicit_tab: Option<bool>) {
        self.request_autocomplete(AutocompleteRequestOptions {
            force: false,
            explicit_tab: explicit_tab.unwrap_or(false),
        });
    }

    fn handle_tab_completion(&self) {
        let is_slash = {
            let fields = self.fields();
            if fields.autocomplete_provider.is_none() {
                return;
            }
            let current_line = usize::try_from(fields.state.cursor_line)
                .ok()
                .and_then(|idx| fields.state.lines.get(idx))
                .map(String::as_str)
                .unwrap_or("");
            let before_cursor = utf16_slice(current_line, 0, Some(fields.state.cursor_col));
            fields.is_in_slash_command_context(&before_cursor)
                && !pi_js::str16::trim_start(&before_cursor).contains(' ')
        };

        if is_slash {
            self.handle_slash_command_completion();
        } else {
            self.force_file_autocomplete(Some(true));
        }
    }

    fn handle_slash_command_completion(&self) {
        self.request_autocomplete(AutocompleteRequestOptions {
            force: false,
            explicit_tab: true,
        });
    }

    /// `explicit_tab` defaults to false inside the body.
    fn force_file_autocomplete(&self, explicit_tab: Option<bool>) {
        self.request_autocomplete(AutocompleteRequestOptions {
            force: true,
            explicit_tab: explicit_tab.unwrap_or(false),
        });
    }

    fn spawn_autocomplete_request(&self, start_token: i64, options: AutocompleteRequestOptions) {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let prev_task = {
            let mut fields = self.fields();
            std::mem::replace(
                &mut fields.autocomplete_request_task,
                Box::pin(async move {
                    let _ = rx.await;
                }),
            )
        };
        let editor = self.clone();
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    prev_task.await;
                    editor.start_autocomplete_request(start_token, options).await;
                    let _ = tx.send(());
                });
            }
            Err(_) => {
                std::thread::spawn(move || {
                    if let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() {
                        rt.block_on(async move {
                            prev_task.await;
                            editor.start_autocomplete_request(start_token, options).await;
                            let _ = tx.send(());
                        });
                    } else {
                        let _ = tx.send(());
                    }
                });
            }
        }
    }

    fn request_autocomplete(&self, options: AutocompleteRequestOptions) {
        let (provider, lines, cursor_line, cursor_col) = {
            let fields = self.fields();
            let Some(provider) = fields.autocomplete_provider.clone() else {
                return;
            };
            (
                provider,
                fields.state.lines.clone(),
                fields.state.cursor_line,
                fields.state.cursor_col,
            )
        };

        if options.force && !provider.should_trigger_file_completion(&lines, cursor_line, cursor_col) {
            return;
        }

        self.cancel_autocomplete_request();

        let (start_token, debounce_ms) = {
            let mut fields = self.fields();
            fields.autocomplete_start_token += 1;
            let start_token = fields.autocomplete_start_token;
            let debounce_ms = fields.get_autocomplete_debounce_ms(&options);
            (start_token, debounce_ms)
        };

        if debounce_ms > 0 {
            let editor_weak = Arc::downgrade(&self.inner);
            let timer = pi_js::time::set_timeout(u64::try_from(debounce_ms).unwrap_or(0), move || {
                if let Some(inner) = editor_weak.upgrade() {
                    let editor = Editor { inner };
                    let should_run = {
                        let mut fields = editor.fields();
                        if fields.autocomplete_start_token != start_token {
                            false
                        } else {
                            fields.autocomplete_debounce_timer = None;
                            true
                        }
                    };
                    if should_run {
                        editor.spawn_autocomplete_request(start_token, options);
                    }
                }
            });
            let stale = {
                let mut fields = self.fields();
                if fields.autocomplete_start_token == start_token {
                    fields.autocomplete_debounce_timer = Some(timer);
                    None
                } else {
                    Some(timer)
                }
            };
            if let Some(stale_timer) = stale {
                stale_timer.clear();
            }
            return;
        }

        self.spawn_autocomplete_request(start_token, options);
    }

    async fn start_autocomplete_request(&self, start_token: i64, options: AutocompleteRequestOptions) {
        let (signal, request_id, snapshot_text, snapshot_line, snapshot_col) = {
            let mut fields = self.fields();
            if start_token != fields.autocomplete_start_token || fields.autocomplete_provider.is_none() {
                return;
            }
            let controller = pi_js::abort::AbortController::new();
            let signal = controller.signal();
            fields.autocomplete_abort = Some(controller);
            fields.autocomplete_request_id += 1;
            let request_id = fields.autocomplete_request_id;
            let snapshot_text = fields.get_text();
            let snapshot_line = fields.state.cursor_line;
            let snapshot_col = fields.state.cursor_col;
            (signal, request_id, snapshot_text, snapshot_line, snapshot_col)
        };

        self.run_autocomplete_request(
            request_id,
            &signal,
            &snapshot_text,
            snapshot_line,
            snapshot_col,
            &options,
        )
        .await;
    }

    fn set_autocomplete_trigger_characters(&self, trigger_characters: &[String]) {
        let mut next: Vec<String> = DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        for character in trigger_characters {
            if utf16_len(character) != 1
                || character == "/"
                || is_whitespace_char(character)
                || next.iter().any(|existing| existing == character)
            {
                continue;
            }
            next.push(character.clone());
        }
        let trigger_pattern = build_trigger_pattern(&next);
        let debounce_pattern = build_debounce_pattern(&next);
        let mut fields = self.fields();
        fields.autocomplete_trigger_characters = next;
        fields.autocomplete_trigger_pattern = trigger_pattern;
        fields.autocomplete_debounce_pattern = debounce_pattern;
    }

    fn get_autocomplete_debounce_ms(&self, options: &AutocompleteRequestOptions) -> i64 {
        self.fields().get_autocomplete_debounce_ms(options)
    }

    async fn run_autocomplete_request(
        &self,
        request_id: i64,
        signal: &pi_js::abort::AbortSignal,
        snapshot_text: &str,
        snapshot_line: i64,
        snapshot_col: i64,
        options: &AutocompleteRequestOptions,
    ) {
        let Some((provider, lines, cursor_line, cursor_col)) = ({
            let fields = self.fields();
            fields.autocomplete_provider.clone().map(|p| {
                (
                    p,
                    fields.state.lines.clone(),
                    fields.state.cursor_line,
                    fields.state.cursor_col,
                )
            })
        }) else {
            return;
        };

        let suggestions = provider
            .get_suggestions(
                &lines,
                cursor_line,
                cursor_col,
                GetSuggestionsOptions {
                    signal: signal.clone(),
                    force: Some(options.force),
                },
            )
            .await;

        if !self.is_autocomplete_request_current(request_id, signal, snapshot_text, snapshot_line, snapshot_col) {
            return;
        }

        self.fields().autocomplete_abort = None;

        let Some(suggestions) = suggestions else {
            self.cancel_autocomplete();
            self.tui().request_render(None);
            return;
        };
        if suggestions.items.is_empty() {
            self.cancel_autocomplete();
            self.tui().request_render(None);
            return;
        }

        if options.force && options.explicit_tab && suggestions.items.len() == 1 {
            let item = suggestions.items[0].clone();
            let snapshot = {
                let mut fields = self.fields();
                let Some(provider) = fields.autocomplete_provider.clone() else {
                    return;
                };
                fields.push_undo_snapshot();
                fields.last_action = None;
                (
                    provider,
                    fields.state.lines.clone(),
                    fields.state.cursor_line,
                    fields.state.cursor_col,
                )
            };
            let result = snapshot
                .0
                .apply_completion(&snapshot.1, snapshot.2, snapshot.3, &item, &suggestions.prefix);
            let (change, tui) = {
                let mut fields = self.fields();
                fields.state.lines = result.lines;
                fields.state.cursor_line = result.cursor_line;
                fields.set_cursor_col(result.cursor_col);
                ((fields.on_change.clone(), fields.get_text()), Arc::clone(&fields.tui))
            };
            if let Some(on_change) = change.0 {
                on_change(&change.1);
            }
            tui.request_render(None);
            return;
        }

        let next_state = if options.force {
            AutocompleteState::Force
        } else {
            AutocompleteState::Regular
        };
        self.apply_autocomplete_suggestions(&suggestions, next_state);
        self.tui().request_render(None);
    }

    fn is_autocomplete_request_current(
        &self,
        request_id: i64,
        signal: &pi_js::abort::AbortSignal,
        snapshot_text: &str,
        snapshot_line: i64,
        snapshot_col: i64,
    ) -> bool {
        if signal.aborted() {
            return false;
        }
        let fields = self.fields();
        request_id == fields.autocomplete_request_id
            && fields.get_text() == snapshot_text
            && fields.state.cursor_line == snapshot_line
            && fields.state.cursor_col == snapshot_col
    }

    fn apply_autocomplete_suggestions(&self, suggestions: &AutocompleteSuggestions, state: AutocompleteState) {
        let list = self.create_autocomplete_list(&suggestions.prefix, &suggestions.items);
        let best_match_index = self.get_best_autocomplete_match_index(&suggestions.items, &suggestions.prefix);
        if best_match_index >= 0 {
            list.set_selected_index(best_match_index);
        }
        let mut fields = self.fields();
        fields.autocomplete_prefix = suggestions.prefix.clone();
        fields.autocomplete_list = Some(list);
        fields.autocomplete_state = Some(state);
    }

    fn cancel_autocomplete_request(&self) {
        let (timer, abort) = {
            let mut fields = self.fields();
            fields.cancel_autocomplete_request_parts()
        };
        if let Some(t) = timer {
            t.clear();
        }
        if let Some(controller) = abort {
            controller.abort(None);
        }
    }

    fn clear_autocomplete_ui(&self) {
        self.fields().clear_autocomplete_ui();
    }

    fn cancel_autocomplete(&self) {
        let (timer, abort) = {
            let mut fields = self.fields();
            let parts = fields.cancel_autocomplete_request_parts();
            fields.clear_autocomplete_ui();
            parts
        };
        if let Some(t) = timer {
            t.clear();
        }
        if let Some(controller) = abort {
            controller.abort(None);
        }
    }

    fn update_autocomplete(&self) {
        let active = {
            let fields = self.fields();
            if fields.autocomplete_provider.is_none() {
                None
            } else {
                fields.autocomplete_state
            }
        };
        let Some(state) = active else {
            return;
        };
        self.request_autocomplete(AutocompleteRequestOptions {
            force: matches!(state, AutocompleteState::Force),
            explicit_tab: false,
        });
    }
}

impl EditorFields {
    fn valid_paste_ids(&self) -> IndexSet<i64> {
        self.pastes.keys().copied().collect()
    }

    fn segment(&self, text: &str, mode: SegmentMode) -> Vec<SegmentData> {
        segment_with_markers(text, mode, &self.valid_paste_ids())
    }

    fn get_text(&self) -> String {
        self.state.lines.join("\n")
    }

    fn is_editor_empty(&self) -> bool {
        self.state.lines.len() == 1 && self.state.lines[0].is_empty()
    }

    fn is_on_first_visual_line(&self) -> bool {
        let visual_lines = self.build_visual_line_map(self.last_width);
        self.find_current_visual_line(&visual_lines) == 0
    }

    fn is_on_last_visual_line(&self) -> bool {
        let visual_lines = self.build_visual_line_map(self.last_width);
        let current = self.find_current_visual_line(&visual_lines);
        current == visual_lines.len() as i64 - 1
    }

    fn set_cursor_col(&mut self, col: i64) {
        self.state.cursor_col = col;
        self.preferred_visual_col = None;
        self.snapped_from_cursor_col = None;
    }

    fn exit_history_browsing(&mut self) {
        self.history_index = -1;
        self.history_draft = None;
    }

    fn push_undo_snapshot(&mut self) {
        let snapshot = EditorSnapshot {
            state: self.state.clone(),
            pastes: self.pastes.clone(),
            paste_counter: self.paste_counter,
        };
        self.undo_stack.push(&snapshot);
    }

    fn normalize_text(text: &str) -> String {
        text.replace("\r\n", "\n").replace('\r', "\n").replace('\t', "    ")
    }

    fn expand_paste_markers(&self, text: &str) -> String {
        if self.pastes.is_empty() {
            return text.to_string();
        }
        PASTE_MARKER_REGEX
            .replace_all(text, |caps: &regex::Captures| {
                if let Some(id) = caps.get(1).and_then(|m| m.as_str().parse::<i64>().ok())
                    && let Some(content) = self.pastes.get(&id)
                {
                    return content.clone();
                }
                caps[0].to_string()
            })
            .into_owned()
    }

    fn set_text_internal(&mut self, text: &str, cursor_placement: CursorPlacement) -> (Option<TextCallback>, String) {
        let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        self.state.lines = if lines.is_empty() { vec![String::new()] } else { lines };
        self.state.cursor_line = match cursor_placement {
            CursorPlacement::Start => 0,
            CursorPlacement::End => self.state.lines.len() as i64 - 1,
        };
        let col = match cursor_placement {
            CursorPlacement::Start => 0,
            CursorPlacement::End => usize::try_from(self.state.cursor_line)
                .ok()
                .and_then(|idx| self.state.lines.get(idx))
                .map(|line| utf16_len(line))
                .unwrap_or(0),
        };
        self.set_cursor_col(col);
        self.scroll_offset = 0;

        (self.on_change.clone(), self.get_text())
    }

    fn insert_text_at_cursor_internal(&mut self, text: &str) -> Option<(Option<TextCallback>, String)> {
        if text.is_empty() {
            return None;
        }

        let normalized = Self::normalize_text(text);
        let inserted_lines: Vec<&str> = normalized.split('\n').collect();

        let line_idx = usize::try_from(self.state.cursor_line).unwrap_or(0);
        let current_line = self.state.lines.get(line_idx).cloned().unwrap_or_default();
        let before_cursor = utf16_slice(&current_line, 0, Some(self.state.cursor_col));
        let after_cursor = utf16_slice(&current_line, self.state.cursor_col, None);

        if inserted_lines.len() == 1 {
            if line_idx < self.state.lines.len() {
                self.state.lines[line_idx] = format!("{before_cursor}{normalized}{after_cursor}");
            }
            let new_col = self.state.cursor_col + utf16_len(&normalized);
            self.set_cursor_col(new_col);
        } else {
            let mut next_lines = Vec::with_capacity(self.state.lines.len() + inserted_lines.len() - 1);
            next_lines.extend(self.state.lines[..line_idx].iter().cloned());
            next_lines.push(format!("{before_cursor}{}", inserted_lines[0]));
            for mid in &inserted_lines[1..inserted_lines.len() - 1] {
                next_lines.push((*mid).to_string());
            }
            let last_inserted = inserted_lines[inserted_lines.len() - 1];
            next_lines.push(format!("{last_inserted}{after_cursor}"));
            if line_idx + 1 < self.state.lines.len() {
                next_lines.extend(self.state.lines[line_idx + 1..].iter().cloned());
            }
            self.state.lines = next_lines;
            self.state.cursor_line += inserted_lines.len() as i64 - 1;
            self.set_cursor_col(utf16_len(last_inserted));
        }

        Some((self.on_change.clone(), self.get_text()))
    }

    fn layout_text(&self, content_width: i64) -> Vec<LayoutLine> {
        let mut layout_lines: Vec<LayoutLine> = Vec::new();

        if self.state.lines.is_empty() || (self.state.lines.len() == 1 && self.state.lines[0].is_empty()) {
            layout_lines.push(LayoutLine {
                text: String::new(),
                has_cursor: true,
                cursor_pos: Some(0),
            });
            return layout_lines;
        }

        for (i, line) in self.state.lines.iter().enumerate() {
            let is_current_line = i as i64 == self.state.cursor_line;
            let line_visible_width = visible_width(line);

            if line_visible_width <= content_width {
                if is_current_line {
                    layout_lines.push(LayoutLine {
                        text: line.clone(),
                        has_cursor: true,
                        cursor_pos: Some(self.state.cursor_col),
                    });
                } else {
                    layout_lines.push(LayoutLine {
                        text: line.clone(),
                        has_cursor: false,
                        cursor_pos: None,
                    });
                }
            } else {
                let segments = self.segment(line, SegmentMode::Grapheme);
                let chunks = word_wrap_line(line, content_width, Some(&segments));
                let chunks_len = chunks.len();

                for (chunk_index, chunk) in chunks.into_iter().enumerate() {
                    let cursor_pos = self.state.cursor_col;
                    let is_last_chunk = chunk_index + 1 == chunks_len;

                    let mut has_cursor_in_chunk = false;
                    let mut adjusted_cursor_pos = 0;

                    if is_current_line {
                        if is_last_chunk {
                            has_cursor_in_chunk = cursor_pos >= chunk.start_index;
                            adjusted_cursor_pos = cursor_pos - chunk.start_index;
                        } else {
                            has_cursor_in_chunk = cursor_pos >= chunk.start_index && cursor_pos < chunk.end_index;
                            if has_cursor_in_chunk {
                                adjusted_cursor_pos = cursor_pos - chunk.start_index;
                                let chunk_text_len = utf16_len(&chunk.text);
                                if adjusted_cursor_pos > chunk_text_len {
                                    adjusted_cursor_pos = chunk_text_len;
                                }
                            }
                        }
                    }

                    if has_cursor_in_chunk {
                        layout_lines.push(LayoutLine {
                            text: chunk.text,
                            has_cursor: true,
                            cursor_pos: Some(adjusted_cursor_pos),
                        });
                    } else {
                        layout_lines.push(LayoutLine {
                            text: chunk.text,
                            has_cursor: false,
                            cursor_pos: None,
                        });
                    }
                }
            }
        }

        layout_lines
    }

    fn build_visual_line_map(&self, width: i64) -> Vec<VisualLine> {
        let mut visual_lines: Vec<VisualLine> = Vec::new();

        for (i, line) in self.state.lines.iter().enumerate() {
            let line_vis_width = visible_width(line);
            if line.is_empty() {
                visual_lines.push(VisualLine {
                    logical_line: i as i64,
                    start_col: 0,
                    length: 0,
                });
            } else if line_vis_width <= width {
                visual_lines.push(VisualLine {
                    logical_line: i as i64,
                    start_col: 0,
                    length: utf16_len(line),
                });
            } else {
                let segments = self.segment(line, SegmentMode::Grapheme);
                let chunks = word_wrap_line(line, width, Some(&segments));
                for chunk in chunks {
                    visual_lines.push(VisualLine {
                        logical_line: i as i64,
                        start_col: chunk.start_index,
                        length: chunk.end_index - chunk.start_index,
                    });
                }
            }
        }

        visual_lines
    }

    fn find_visual_line_at(visual_lines: &[VisualLine], line: i64, col: i64) -> i64 {
        for (i, vl) in visual_lines.iter().enumerate() {
            if vl.logical_line != line {
                continue;
            }
            let offset = col - vl.start_col;
            let is_last_segment_of_line = i + 1 == visual_lines.len()
                || visual_lines
                    .get(i + 1)
                    .is_some_and(|next| next.logical_line != vl.logical_line);
            if offset >= 0 && (offset < vl.length || (is_last_segment_of_line && offset == vl.length)) {
                return i as i64;
            }
        }
        (visual_lines.len() as i64 - 1).max(0)
    }

    fn find_current_visual_line(&self, visual_lines: &[VisualLine]) -> i64 {
        Self::find_visual_line_at(visual_lines, self.state.cursor_line, self.state.cursor_col)
    }

    fn move_to_visual_line(&mut self, visual_lines: &[VisualLine], current_visual_line: i64, target_visual_line: i64) {
        let Some(current_vl_idx) = usize::try_from(current_visual_line).ok() else {
            return;
        };
        let Some(target_vl_idx) = usize::try_from(target_visual_line).ok() else {
            return;
        };
        let (Some(&current_vl), Some(&target_vl)) = (visual_lines.get(current_vl_idx), visual_lines.get(target_vl_idx))
        else {
            return;
        };

        // When the cursor was snapped to a segment start, resolve the pre-snap
        // position against the VL it belongs to. This gives the correct visual
        // column even after a resize reshuffles VLs.
        let current_visual_col = if let Some(snapped_col) = self.snapped_from_cursor_col {
            let vl_index = Self::find_visual_line_at(visual_lines, current_vl.logical_line, snapped_col);
            let resolved_start = usize::try_from(vl_index)
                .ok()
                .and_then(|idx| visual_lines.get(idx))
                .map(|vl| vl.start_col)
                .unwrap_or(0);
            snapped_col - resolved_start
        } else {
            self.state.cursor_col - current_vl.start_col
        };

        // For non-last segments, clamp to length-1 to stay within the segment
        let is_last_source_segment = current_vl_idx + 1 == visual_lines.len()
            || visual_lines
                .get(current_vl_idx + 1)
                .is_some_and(|next| next.logical_line != current_vl.logical_line);
        let source_max_visual_col = if is_last_source_segment {
            current_vl.length
        } else {
            (current_vl.length - 1).max(0)
        };

        let is_last_target_segment = target_vl_idx + 1 == visual_lines.len()
            || visual_lines
                .get(target_vl_idx + 1)
                .is_some_and(|next| next.logical_line != target_vl.logical_line);
        let target_max_visual_col = if is_last_target_segment {
            target_vl.length
        } else {
            (target_vl.length - 1).max(0)
        };

        let move_to_visual_col =
            self.compute_vertical_move_column(current_visual_col, source_max_visual_col, target_max_visual_col);

        // Set cursor position
        self.state.cursor_line = target_vl.logical_line;
        let target_col = target_vl.start_col + move_to_visual_col;
        let logical_line = usize::try_from(target_vl.logical_line)
            .ok()
            .and_then(|idx| self.state.lines.get(idx))
            .cloned()
            .unwrap_or_default();
        self.state.cursor_col = target_col.min(utf16_len(&logical_line));

        // Snap cursor to atomic segment boundary (e.g. paste markers)
        let segments = self.segment(&logical_line, SegmentMode::Grapheme);
        for seg in segments {
            if seg.index > self.state.cursor_col {
                break;
            }
            let seg_len = utf16_len(&seg.segment);
            if seg_len <= 1 {
                continue;
            }
            if self.state.cursor_col < seg.index + seg_len {
                let is_continuation = seg.index < target_vl.start_col;
                let is_moving_down = target_visual_line > current_visual_line;

                if is_continuation && is_moving_down {
                    let seg_end = seg.index + seg_len;
                    let mut next = target_visual_line + 1;
                    while let Some(next_vl) = usize::try_from(next).ok().and_then(|idx| visual_lines.get(idx)) {
                        if next_vl.logical_line == target_vl.logical_line && next_vl.start_col < seg_end {
                            next += 1;
                        } else {
                            break;
                        }
                    }
                    if next < visual_lines.len() as i64 {
                        self.move_to_visual_line(visual_lines, current_visual_line, next);
                        return;
                    }
                }

                self.snapped_from_cursor_col = Some(self.state.cursor_col);
                self.state.cursor_col = seg.index;
                return;
            }
        }

        // No snap occurred – we moved out of the atomic segment.
        self.snapped_from_cursor_col = None;
    }

    fn compute_vertical_move_column(
        &mut self,
        current_visual_col: i64,
        source_max_visual_col: i64,
        target_max_visual_col: i64,
    ) -> i64 {
        let has_preferred = self.preferred_visual_col.is_some();
        let cursor_in_middle = current_visual_col < source_max_visual_col;
        let target_too_short = target_max_visual_col < current_visual_col;

        if !has_preferred || cursor_in_middle {
            if target_too_short {
                self.preferred_visual_col = Some(current_visual_col);
                return target_max_visual_col;
            }

            self.preferred_visual_col = None;
            return current_visual_col;
        }

        let preferred = self.preferred_visual_col.unwrap_or(0);
        let target_cant_fit_preferred = target_max_visual_col < preferred;
        if target_too_short || target_cant_fit_preferred {
            return target_max_visual_col;
        }

        self.preferred_visual_col = None;
        preferred
    }

    fn move_word_backwards(&mut self) {
        self.last_action = None;
        let current_line = usize::try_from(self.state.cursor_line)
            .ok()
            .and_then(|idx| self.state.lines.get(idx))
            .cloned()
            .unwrap_or_default();

        // If at start of line, move to end of previous line
        if self.state.cursor_col == 0 {
            if self.state.cursor_line > 0 {
                self.state.cursor_line -= 1;
                let prev_len = usize::try_from(self.state.cursor_line)
                    .ok()
                    .and_then(|idx| self.state.lines.get(idx))
                    .map(|l| utf16_len(l))
                    .unwrap_or(0);
                self.set_cursor_col(prev_len);
            }
            return;
        }

        let valid_ids = self.valid_paste_ids();
        let new_col = find_word_backward(
            &current_line,
            self.state.cursor_col,
            Some(WordNavigationOptions {
                segment: Some(Arc::new(move |text: &str| {
                    to_util_segments(segment_with_markers(text, SegmentMode::Word, &valid_ids))
                })),
                is_atomic_segment: Some(Arc::new(is_paste_marker)),
            }),
        );
        self.set_cursor_col(new_col);
    }

    fn move_word_forwards(&mut self) {
        self.last_action = None;
        let current_line = usize::try_from(self.state.cursor_line)
            .ok()
            .and_then(|idx| self.state.lines.get(idx))
            .cloned()
            .unwrap_or_default();

        // If at end of line, move to start of next line
        if self.state.cursor_col >= utf16_len(&current_line) {
            if self.state.cursor_line < self.state.lines.len() as i64 - 1 {
                self.state.cursor_line += 1;
                self.set_cursor_col(0);
            }
            return;
        }

        let valid_ids = self.valid_paste_ids();
        let new_col = find_word_forward(
            &current_line,
            self.state.cursor_col,
            Some(WordNavigationOptions {
                segment: Some(Arc::new(move |text: &str| {
                    to_util_segments(segment_with_markers(text, SegmentMode::Word, &valid_ids))
                })),
                is_atomic_segment: Some(Arc::new(is_paste_marker)),
            }),
        );
        self.set_cursor_col(new_col);
    }

    fn insert_yanked_text(&mut self, text: &str) -> (Option<TextCallback>, String) {
        self.exit_history_browsing();
        let lines: Vec<&str> = text.split('\n').collect();

        if lines.len() == 1 {
            let line_idx = usize::try_from(self.state.cursor_line).unwrap_or(0);
            let current_line = self.state.lines.get(line_idx).cloned().unwrap_or_default();
            let before = utf16_slice(&current_line, 0, Some(self.state.cursor_col));
            let after = utf16_slice(&current_line, self.state.cursor_col, None);
            if line_idx < self.state.lines.len() {
                self.state.lines[line_idx] = format!("{before}{text}{after}");
            }
            let new_col = self.state.cursor_col + utf16_len(text);
            self.set_cursor_col(new_col);
        } else {
            let line_idx = usize::try_from(self.state.cursor_line).unwrap_or(0);
            let current_line = self.state.lines.get(line_idx).cloned().unwrap_or_default();
            let before = utf16_slice(&current_line, 0, Some(self.state.cursor_col));
            let after = utf16_slice(&current_line, self.state.cursor_col, None);

            if line_idx < self.state.lines.len() {
                self.state.lines[line_idx] = format!("{before}{}", lines[0]);
            }
            for (i, mid) in lines.iter().enumerate().take(lines.len() - 1).skip(1) {
                self.state.lines.insert(line_idx + i, (*mid).to_string());
            }
            let last_line_index = line_idx + lines.len() - 1;
            let last_yank_line = lines[lines.len() - 1];
            self.state
                .lines
                .insert(last_line_index, format!("{last_yank_line}{after}"));

            self.state.cursor_line = last_line_index as i64;
            self.set_cursor_col(utf16_len(last_yank_line));
        }

        (self.on_change.clone(), self.get_text())
    }

    fn delete_yanked_text(&mut self) -> Option<(Option<TextCallback>, String)> {
        let yanked_text = self.kill_ring.peek()?;
        if yanked_text.is_empty() {
            return None;
        }

        let yank_lines: Vec<&str> = yanked_text.split('\n').collect();
        if yank_lines.len() == 1 {
            let line_idx = usize::try_from(self.state.cursor_line).unwrap_or(0);
            let current_line = self.state.lines.get(line_idx).cloned().unwrap_or_default();
            let delete_len = utf16_len(&yanked_text);
            let new_col = (self.state.cursor_col - delete_len).max(0);
            let before = utf16_slice(&current_line, 0, Some(new_col));
            let after = utf16_slice(&current_line, self.state.cursor_col, None);
            if line_idx < self.state.lines.len() {
                self.state.lines[line_idx] = format!("{before}{after}");
            }
            self.set_cursor_col(new_col);
        } else {
            let start_line = (self.state.cursor_line - (yank_lines.len() as i64 - 1)).max(0) as usize;
            let start_line_str = self.state.lines.get(start_line).cloned().unwrap_or_default();
            let start_col = (utf16_len(&start_line_str) - utf16_len(yank_lines[0])).max(0);
            let cursor_idx = usize::try_from(self.state.cursor_line).unwrap_or(start_line);
            let current_line = self.state.lines.get(cursor_idx).cloned().unwrap_or_default();
            let after_cursor = utf16_slice(&current_line, self.state.cursor_col, None);
            let before_yank = utf16_slice(&start_line_str, 0, Some(start_col));

            let remove_end = (start_line + yank_lines.len()).min(self.state.lines.len());
            self.state.lines.splice(
                start_line..remove_end,
                std::iter::once(format!("{before_yank}{after_cursor}")),
            );
            self.state.cursor_line = start_line as i64;
            self.set_cursor_col(start_col);
        }

        Some((self.on_change.clone(), self.get_text()))
    }

    fn is_slash_menu_allowed(&self) -> bool {
        self.state.cursor_line == 0
    }

    fn is_at_start_of_message(&self) -> bool {
        if !self.is_slash_menu_allowed() {
            return false;
        }
        let current_line = usize::try_from(self.state.cursor_line)
            .ok()
            .and_then(|idx| self.state.lines.get(idx))
            .map(String::as_str)
            .unwrap_or("");
        let before_cursor = utf16_slice(current_line, 0, Some(self.state.cursor_col));
        let trimmed = pi_js::str16::trim(&before_cursor);
        trimmed.is_empty() || trimmed == "/"
    }

    fn is_in_slash_command_context(&self, text_before_cursor: &str) -> bool {
        self.is_slash_menu_allowed() && pi_js::str16::trim_start(text_before_cursor).starts_with('/')
    }

    fn get_autocomplete_debounce_ms(&self, options: &AutocompleteRequestOptions) -> i64 {
        if options.explicit_tab || options.force {
            return 0;
        }
        let current_line = usize::try_from(self.state.cursor_line)
            .ok()
            .and_then(|idx| self.state.lines.get(idx))
            .map(String::as_str)
            .unwrap_or("");
        let text_before_cursor = utf16_slice(current_line, 0, Some(self.state.cursor_col));
        if self
            .autocomplete_debounce_pattern
            .is_match(&text_before_cursor)
            .unwrap_or(false)
        {
            ATTACHMENT_AUTOCOMPLETE_DEBOUNCE_MS
        } else {
            0
        }
    }

    fn cancel_autocomplete_request_parts(
        &mut self,
    ) -> (Option<pi_js::time::Timeout>, Option<pi_js::abort::AbortController>) {
        self.autocomplete_start_token += 1;
        let timer = self.autocomplete_debounce_timer.take();
        let abort = self.autocomplete_abort.take();
        (timer, abort)
    }

    fn clear_autocomplete_ui(&mut self) {
        self.autocomplete_state = None;
        self.autocomplete_list = None;
        self.autocomplete_prefix.clear();
    }
}

impl Component for Editor {
    fn render(&self, width: usize) -> Vec<String> {
        Editor::render(self, width)
    }

    fn has_handle_input(&self) -> bool {
        true
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

    fn as_focusable(&self) -> Option<&dyn Focusable> {
        Some(self)
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
