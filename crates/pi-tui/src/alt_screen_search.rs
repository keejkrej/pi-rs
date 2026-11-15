//! Port of packages/tui/src/alt-screen-search.ts

#![allow(dead_code, unused_imports, unused_variables)]

use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use regex::Regex;

use crate::components::input::{Input, InputOptions};
use crate::keybindings::get_keybindings;
use crate::tui::{Component, Focusable, TuiMouseEvent, TuiMouseEventResult};
use crate::utils::{get_grapheme_segmenter, strip_terminal_sequences, truncate_to_width, visible_width};

/// `(text) => string` navigation-button style. Default is the identity function.
pub type NavigationButtonStyle = Arc<dyn Fn(&str, bool) -> String + Send + Sync>;

/// `(query) => void`.
pub type QueryChangeCallback = Arc<dyn Fn(&str) + Send + Sync>;

const PRINTABLE_ASCII: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[\x20-\x7e]*$").expect("PRINTABLE_ASCII"));

// PORT: shared grapheme segmenter is `crate::utils::get_grapheme_segmenter`. Not redeclared.

struct SearchSourceSpan {
    text_start: i64,
    text_end: i64,
    row: i64,
    start_col: i64,
    end_col: i64,
    linear_columns: bool,
}

struct SearchCorpus {
    text: String,
    spans: Vec<SearchSourceSpan>,
}

/// Field order is `row`, `startCol`, `endCol`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AltScreenSearchSegment {
    pub row: i64,
    pub start_col: i64,
    pub end_col: i64,
}

/// Field order is `segments`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AltScreenSearchMatch {
    pub segments: Vec<AltScreenSearchSegment>,
}

/// Field order is `matches`, then `changed`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AltScreenSearchResult {
    pub matches: Vec<AltScreenSearchMatch>,
    pub changed: bool,
}

static JS_WHITESPACE_ONLY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("^{}+$", pi_js::regex::JS_WS_CLASS)).expect("JS_WHITESPACE_ONLY"));
static JS_WHITESPACE_RUN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("{}+", pi_js::regex::JS_WS_CLASS)).expect("JS_WHITESPACE_RUN"));

fn build_search_corpus(lines: &[String]) -> SearchCorpus {
    let segmenter = get_grapheme_segmenter();
    let mut chunks: Vec<String> = Vec::new();
    let mut spans: Vec<SearchSourceSpan> = Vec::new();
    let mut text_length = 0i64;
    let mut pending_separator = false;

    for (row, raw_line) in lines.iter().enumerate() {
        let row = row as i64;
        let line = strip_terminal_sequences(raw_line);
        let mut column = 0i64;

        if PRINTABLE_ASCII.is_match(&line) {
            let bytes = line.as_bytes();
            let mut index = 0usize;
            while index < bytes.len() {
                if bytes[index] == b' ' {
                    if text_length > 0 {
                        pending_separator = true;
                    }
                    column += 1;
                    index += 1;
                    continue;
                }
                let mut end = index + 1;
                while end < bytes.len() && bytes[end] != b' ' {
                    end += 1;
                }
                if pending_separator {
                    chunks.push(" ".to_string());
                    text_length += 1;
                    pending_separator = false;
                }
                let text = &line[index..end];
                let len = text.len() as i64;
                chunks.push(text.to_string());
                spans.push(SearchSourceSpan {
                    text_start: text_length,
                    text_end: text_length + len,
                    row,
                    start_col: column,
                    end_col: column + len,
                    linear_columns: true,
                });
                text_length += len;
                column += len;
                index = end;
            }
        } else {
            for grapheme in segmenter.segment(&line) {
                let text = grapheme.segment;
                let width = visible_width(&text);
                if JS_WHITESPACE_ONLY.is_match(&text) {
                    if text_length > 0 {
                        pending_separator = true;
                    }
                    column += width;
                    continue;
                }
                if pending_separator {
                    chunks.push(" ".to_string());
                    text_length += 1;
                    pending_separator = false;
                }
                let utf16_len = pi_js::str16::len(&text) as i64;
                chunks.push(text);
                spans.push(SearchSourceSpan {
                    text_start: text_length,
                    text_end: text_length + utf16_len,
                    row,
                    start_col: column,
                    end_col: column + width,
                    linear_columns: false,
                });
                text_length += utf16_len;
                column += width;
            }
        }

        if text_length > 0 {
            pending_separator = true;
        }
    }

    SearchCorpus {
        text: chunks.concat(),
        spans,
    }
}

fn normalize_query(query: &str) -> String {
    let collapsed = JS_WHITESPACE_RUN.replace_all(query, " ");
    pi_js::str16::trim(&collapsed).to_string()
}

fn escape_reg_exp(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(
            ch,
            '.' | '*' | '+' | '?' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn find_search_corpus_matches(corpus: &SearchCorpus, normalized_query: &str) -> Vec<AltScreenSearchMatch> {
    if normalized_query.is_empty() {
        return Vec::new();
    }
    let Ok(expression) = pi_js::regex::ecma(&escape_reg_exp(normalized_query), "giu") else {
        return Vec::new();
    };
    let mut matches = Vec::new();
    let mut span_index = 0usize;

    for m in pi_js::regex::match_all(&expression, &corpus.text) {
        let start = m.index as i64;
        let end = start + pi_js::str16::len(&m.matched) as i64;
        while span_index < corpus.spans.len() && corpus.spans[span_index].text_end <= start {
            span_index += 1;
        }

        let mut segments: Vec<AltScreenSearchSegment> = Vec::new();
        for span in &corpus.spans[span_index..] {
            if span.text_start >= end {
                break;
            }
            if span.text_end <= start {
                continue;
            }
            let start_col = if span.linear_columns {
                span.start_col + start.max(span.text_start) - span.text_start
            } else {
                span.start_col
            };
            let end_col = if span.linear_columns {
                span.start_col + end.min(span.text_end) - span.text_start
            } else {
                span.end_col
            };
            if let Some(previous) = segments.last_mut()
                && previous.row == span.row
                && start_col <= previous.end_col
            {
                previous.end_col = previous.end_col.max(end_col);
            } else {
                segments.push(AltScreenSearchSegment {
                    row: span.row,
                    start_col,
                    end_col,
                });
            }
        }
        while span_index < corpus.spans.len() && corpus.spans[span_index].text_end <= end {
            span_index += 1;
        }
        if !segments.is_empty() {
            matches.push(AltScreenSearchMatch { segments });
        }
    }

    matches
}

struct AltScreenSearchIndexState {
    source_lines: Option<Vec<String>>,
    corpus: Option<SearchCorpus>,
    normalized_query: Option<String>,
    matches: Vec<AltScreenSearchMatch>,
}

struct AltScreenSearchIndexInner {
    state: Mutex<AltScreenSearchIndexState>,
}

/// Cache the searchable corpus and matches while rendered transcript lines remain unchanged.
///
/// PORT: TS class with identity. Handle; methods take `&self`.
#[derive(Clone)]
pub struct AltScreenSearchIndex {
    inner: Arc<AltScreenSearchIndexInner>,
}

impl AltScreenSearchIndex {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(AltScreenSearchIndexInner {
                state: Mutex::new(AltScreenSearchIndexState {
                    source_lines: None,
                    corpus: None,
                    normalized_query: None,
                    matches: Vec::new(),
                }),
            }),
        }
    }

    pub fn search(&self, lines: &[String], query: &str) -> AltScreenSearchResult {
        let mut state = self.inner.state.lock().unwrap();
        let source_changed = match &state.source_lines {
            None => true,
            Some(prev) => prev.len() != lines.len() || prev.iter().zip(lines.iter()).any(|(a, b)| a != b),
        };
        if source_changed || state.corpus.is_none() {
            state.source_lines = Some(lines.to_vec());
            state.corpus = Some(build_search_corpus(lines));
        }

        let normalized_query = normalize_query(query);
        let changed = source_changed || state.normalized_query.as_deref() != Some(normalized_query.as_str());
        if changed {
            state.normalized_query = Some(normalized_query.clone());
            let corpus = state.corpus.as_ref().expect("corpus populated above");
            state.matches = find_search_corpus_matches(corpus, &normalized_query);
        }
        AltScreenSearchResult {
            matches: state.matches.clone(),
            changed,
        }
    }
}

pub fn find_alt_screen_search_matches(lines: &[String], query: &str) -> Vec<AltScreenSearchMatch> {
    let normalized_query = normalize_query(query);
    if normalized_query.is_empty() {
        Vec::new()
    } else {
        find_search_corpus_matches(&build_search_corpus(lines), &normalized_query)
    }
}

pub fn get_alt_screen_search_match_key(r#match: &AltScreenSearchMatch) -> String {
    match (r#match.segments.first(), r#match.segments.last()) {
        (Some(first), Some(last)) => format!("{}:{}:{}:{}", first.row, first.start_col, last.row, last.end_col),
        _ => String::new(),
    }
}

struct AltScreenSearchState {
    result_count: i64,
    result_index: i64,
    previous_button_start: i64,
    previous_button_end: i64,
    next_button_start: i64,
    next_button_end: i64,
    /// `-1`, `1`, or absent.
    hovered_navigation_direction: Option<i64>,
    focused: bool,
}

struct AltScreenSearchInner {
    input: Input,
    on_query_change: QueryChangeCallback,
    navigation_button_style: NavigationButtonStyle,
    state: Mutex<AltScreenSearchState>,
}

/// Transcript find overlay. Implements [`Component`] and [`Focusable`].
///
/// PORT: TS class with identity. Handle; methods take `&self`.
#[derive(Clone)]
pub struct AltScreenSearchComponent {
    inner: Arc<AltScreenSearchInner>,
}

impl AltScreenSearchComponent {
    fn state(&self) -> MutexGuard<'_, AltScreenSearchState> {
        self.inner.state.lock().unwrap()
    }

    /// `navigation_button_style` `None` is `(text) => text`.
    pub fn new(on_query_change: QueryChangeCallback, navigation_button_style: Option<NavigationButtonStyle>) -> Self {
        let input = Input::new(Some(InputOptions {
            prompt: Some(" ".to_string()),
            placeholder: Some("Find in transcript".to_string()),
            placeholder_style: Some(Arc::new(|text: &str| format!("\x1b[2m{text}\x1b[22m"))),
        }));
        Self {
            inner: Arc::new(AltScreenSearchInner {
                input,
                on_query_change,
                navigation_button_style: navigation_button_style
                    .unwrap_or_else(|| Arc::new(|text: &str, _| text.to_string())),
                state: Mutex::new(AltScreenSearchState {
                    result_count: 0,
                    result_index: -1,
                    previous_button_start: -1,
                    previous_button_end: -1,
                    next_button_start: -1,
                    next_button_end: -1,
                    hovered_navigation_direction: None,
                    focused: false,
                }),
            }),
        }
    }

    pub fn focused(&self) -> bool {
        self.state().focused
    }

    pub fn set_focused(&self, value: bool) {
        self.state().focused = value;
        self.inner.input.set_focused(value);
    }

    pub fn set_result(&self, index: i64, count: i64) {
        let mut state = self.state();
        state.result_index = index;
        state.result_count = count;
    }

    /// `row` / `column` are cells inside the rendered overlay. Result is `-1`, `1`, or absent.
    pub fn get_navigation_direction_at(&self, row: i64, column: i64) -> Option<i64> {
        if row != 2 {
            return None;
        }
        let state = self.state();
        if column >= state.previous_button_start && column < state.previous_button_end {
            return Some(-1);
        }
        if column >= state.next_button_start && column < state.next_button_end {
            return Some(1);
        }
        None
    }

    pub fn set_hovered_navigation_direction(&self, direction: Option<i64>) -> bool {
        let mut state = self.state();
        if direction == state.hovered_navigation_direction {
            return false;
        }
        state.hovered_navigation_direction = direction;
        true
    }

    pub fn handle_input(&self, data: &str) {
        let previous = self.inner.input.get_value();
        self.inner.input.handle_input(data);
        let query = self.inner.input.get_value();
        if query != previous {
            (self.inner.on_query_change)(&query);
        }
    }

    pub fn invalidate(&self) {
        self.inner.input.invalidate();
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        let safe_width = (width as i64).max(1);
        let inner_width = (safe_width - 2).max(0);
        let is_darwin = pi_js::env::platform() == "darwin";
        let format_key = |key: Option<&str>| -> String {
            match key {
                Some(key) if !key.is_empty() => key
                    .split('+')
                    .map(|part| {
                        if is_darwin && part.eq_ignore_ascii_case("alt") {
                            "Option".to_string()
                        } else {
                            let mut chars = part.chars();
                            match chars.next() {
                                Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                                None => String::new(),
                            }
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("+"),
                _ => "Unbound".to_string(),
            }
        };

        let keybindings = get_keybindings();
        let prev_keys = keybindings.get_keys("tui.altScreen.searchPrevious");
        let next_keys = keybindings.get_keys("tui.altScreen.searchNext");
        let previous_key = format_key(prev_keys.first().map(String::as_str));
        let next_key = format_key(next_keys.first().map(String::as_str));

        let (result_index, result_count, hovered) = {
            let state = self.state();
            (
                state.result_index,
                state.result_count,
                state.hovered_navigation_direction,
            )
        };

        let query = self.inner.input.get_value();
        let result = if query.is_empty() {
            String::new()
        } else if result_count == 0 {
            "No matches".to_string()
        } else {
            format!("{}/{}", result_index + 1, result_count)
        };
        let result_space = (inner_width - 3).max(0);
        let visible_result = truncate_to_width(&result, result_space, Some(""), None);
        let result_text = if visible_result.is_empty() {
            String::new()
        } else {
            format!("\x1b[2m {visible_result} \x1b[22m")
        };
        let input_width = (inner_width - visible_width(&result_text)).max(0);
        let rendered_input = self.inner.input.render(input_width.max(1) as usize);
        let first_input_line = rendered_input.first().map(String::as_str).unwrap_or("");
        let input_line = truncate_to_width(first_input_line, input_width, Some(""), None);
        let input_padding = " ".repeat((input_width - visible_width(&input_line)).max(0) as usize);
        let content = format!("{input_line}{input_padding}{result_text}");

        let mut previous_button = format!("↑ {previous_key}");
        let mut next_button = format!("↓ {next_key}");
        let mut separator = " · ";
        let outer_gap_width = 1i64;
        let available_controls_width = (inner_width - outer_gap_width * 2 - 1).max(0);
        let mut controls_width =
            visible_width(&previous_button) + visible_width(separator) + visible_width(&next_button);
        if controls_width > available_controls_width {
            previous_button = "↑".to_string();
            next_button = "↓".to_string();
            separator = " ";
            controls_width = visible_width(&previous_button) + visible_width(separator) + visible_width(&next_button);
        }
        let show_buttons = controls_width <= available_controls_width;
        let rendered_buttons = if show_buttons {
            format!(
                "{}{}{}",
                (self.inner.navigation_button_style)(&previous_button, hovered == Some(-1)),
                separator,
                (self.inner.navigation_button_style)(&next_button, hovered == Some(1)),
            )
        } else {
            String::new()
        };
        let outer_gaps_width = if show_buttons { outer_gap_width * 2 } else { 0 };
        let right_rule_width = if !rendered_buttons.is_empty() && inner_width > controls_width + outer_gaps_width {
            1
        } else {
            0
        };
        let left_rule_width =
            (inner_width - if show_buttons { controls_width } else { 0 } - outer_gaps_width - right_rule_width).max(0);
        let previous_start = 1 + left_rule_width + outer_gap_width;

        {
            let mut state = self.state();
            if show_buttons {
                let prev_end = previous_start + visible_width(&previous_button);
                let next_start = prev_end + visible_width(separator);
                let next_end = next_start + visible_width(&next_button);
                state.previous_button_start = previous_start;
                state.previous_button_end = prev_end;
                state.next_button_start = next_start;
                state.next_button_end = next_end;
            } else {
                state.previous_button_start = -1;
                state.previous_button_end = -1;
                state.next_button_start = -1;
                state.next_button_end = -1;
            }
        }

        if safe_width == 1 {
            return vec!["┌".to_string(), "│".to_string(), "└".to_string()];
        }
        let button_gap = if rendered_buttons.is_empty() { "" } else { " " };
        vec![
            format!("┌{}┐", "─".repeat(inner_width as usize)),
            format!("│{content}│"),
            format!(
                "└{}{button_gap}{rendered_buttons}{button_gap}{}┘",
                "─".repeat(left_rule_width as usize),
                "─".repeat(right_rule_width as usize)
            ),
        ]
    }
}

impl Component for AltScreenSearchComponent {
    fn render(&self, width: usize) -> Vec<String> {
        AltScreenSearchComponent::render(self, width)
    }

    fn handle_input(&self, data: &str) {
        AltScreenSearchComponent::handle_input(self, data)
    }

    fn handle_mouse(&self, _event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        None
    }

    fn wants_key_release(&self) -> bool {
        false
    }

    fn invalidate(&self) {
        AltScreenSearchComponent::invalidate(self)
    }

    fn has_handle_input(&self) -> bool {
        true
    }

    fn as_focusable(&self) -> Option<&dyn Focusable> {
        Some(self)
    }
}

impl Focusable for AltScreenSearchComponent {
    fn focused(&self) -> bool {
        AltScreenSearchComponent::focused(self)
    }

    fn set_focused(&self, focused: bool) {
        AltScreenSearchComponent::set_focused(self, focused)
    }
}
