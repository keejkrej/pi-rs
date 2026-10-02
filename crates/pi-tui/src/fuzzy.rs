//! Port of packages/tui/src/fuzzy.ts
//!
//! Fuzzy matching utilities.
//! Matches if all query characters appear in order (not necessarily consecutive).
//! Lower score = better match.

#![allow(dead_code, unused_variables)]

use std::sync::Arc;

/// Field order is `matches`, then `score`.
///
/// `score` is an `f64` because the matcher adds `i * 0.1`.
#[derive(Clone, Debug, PartialEq)]
pub struct FuzzyMatch {
    pub matches: bool,
    pub score: f64,
}

pub fn fuzzy_match(query: &str, text: &str) -> FuzzyMatch {
    todo!("port: fuzzy_match")
}

/// Filter and sort items by fuzzy match quality (best matches first).
/// Supports whitespace- and slash-separated tokens: all tokens must match.
pub fn fuzzy_filter<T: Clone>(items: &[T], query: &str, get_text: Arc<dyn Fn(&T) -> String + Send + Sync>) -> Vec<T> {
    todo!("port: fuzzy_filter")
}
