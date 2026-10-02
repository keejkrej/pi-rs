//! Port of packages/tui/src/alt-screen-search.ts

#![allow(dead_code, unused_imports, unused_variables)]

use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use regex::Regex;

use crate::components::input::Input;
use crate::tui::{Component, Focusable, TuiMouseEvent, TuiMouseEventResult};

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

fn build_search_corpus(lines: &[String]) -> SearchCorpus {
    todo!("port: build_search_corpus")
}

fn normalize_query(query: &str) -> String {
    todo!("port: normalize_query")
}

fn escape_reg_exp(text: &str) -> String {
    todo!("port: escape_reg_exp")
}

fn find_search_corpus_matches(corpus: &SearchCorpus, normalized_query: &str) -> Vec<AltScreenSearchMatch> {
    todo!("port: find_search_corpus_matches")
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
        todo!("port: AltScreenSearchIndex::search")
    }
}

pub fn find_alt_screen_search_matches(lines: &[String], query: &str) -> Vec<AltScreenSearchMatch> {
    todo!("port: find_alt_screen_search_matches")
}

pub fn get_alt_screen_search_match_key(r#match: &AltScreenSearchMatch) -> String {
    todo!("port: get_alt_screen_search_match_key")
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
        todo!("port: AltScreenSearchComponent::new")
    }

    pub fn focused(&self) -> bool {
        self.state().focused
    }

    pub fn set_focused(&self, value: bool) {
        todo!("port: AltScreenSearchComponent::set_focused")
    }

    pub fn set_result(&self, index: i64, count: i64) {
        let mut state = self.state();
        state.result_index = index;
        state.result_count = count;
    }

    /// `row` / `column` are cells inside the rendered overlay. Result is `-1`, `1`, or absent.
    pub fn get_navigation_direction_at(&self, row: i64, column: i64) -> Option<i64> {
        todo!("port: AltScreenSearchComponent::get_navigation_direction_at")
    }

    pub fn set_hovered_navigation_direction(&self, direction: Option<i64>) -> bool {
        todo!("port: AltScreenSearchComponent::set_hovered_navigation_direction")
    }

    pub fn handle_input(&self, data: &str) {
        todo!("port: AltScreenSearchComponent::handle_input")
    }

    pub fn invalidate(&self) {
        self.inner.input.invalidate();
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        todo!("port: AltScreenSearchComponent::render")
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
