//! Port of packages/tui/src/word-navigation.ts

#![allow(dead_code, unused_variables)]

use std::sync::Arc;

use crate::utils::SegmentData;

/// Options for word navigation functions.
/// When omitted, uses the default Intl.Segmenter word segmentation.
///
/// `None` for [`find_word_backward`] / [`find_word_forward`] is the omitted options object.
#[derive(Clone, Default)]
pub struct WordNavigationOptions {
    /// Custom segmenter returning word segments for the given text.
    pub segment: Option<Arc<dyn Fn(&str) -> Vec<SegmentData> + Send + Sync>>,
    /// Predicate identifying atomic segments that should be treated as single units (e.g. paste markers).
    pub is_atomic_segment: Option<Arc<dyn Fn(&str) -> bool + Send + Sync>>,
}

/// Find the cursor position after moving one word backward from `cursor` in `text`.
/// Skips trailing whitespace, then stops at the next word/punctuation boundary.
///
/// Pure function - does not mutate any state.
pub fn find_word_backward(text: &str, cursor: i64, options: Option<WordNavigationOptions>) -> i64 {
    todo!("port: find_word_backward")
}

/// Find the cursor position after moving one word forward from `cursor` in `text`.
/// Skips leading whitespace, then stops at the next word/punctuation boundary.
///
/// Pure function - does not mutate any state.
pub fn find_word_forward(text: &str, cursor: i64, options: Option<WordNavigationOptions>) -> i64 {
    todo!("port: find_word_forward")
}
