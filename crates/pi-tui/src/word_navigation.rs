//! Port of packages/tui/src/word-navigation.ts

#![allow(dead_code, unused_variables)]

use std::sync::Arc;

use crate::utils::{PUNCTUATION_REGEX, SegmentData, get_word_segmenter, is_whitespace_char};

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
    if cursor <= 0 {
        return 0;
    }

    let text_before_cursor = pi_js::str16::slice(text, 0, Some(cursor));
    let segment_fn = options.as_ref().and_then(|o| o.segment.as_ref());
    let is_atomic = options.as_ref().and_then(|o| o.is_atomic_segment.as_ref());
    let mut segments = match segment_fn {
        Some(f) => f(&text_before_cursor),
        None => get_word_segmenter().segment(&text_before_cursor),
    };
    let mut new_cursor = cursor;

    let check_atomic = |s: &str| -> bool { is_atomic.is_some_and(|f| f(s)) };

    // Skip trailing whitespace
    while let Some(last) = segments.last() {
        if check_atomic(&last.segment) || !is_whitespace_char(&last.segment) {
            break;
        }
        let seg = segments.pop().unwrap();
        new_cursor -= pi_js::str16::len(&seg.segment) as i64;
    }

    let Some(last) = segments.last() else {
        return new_cursor;
    };

    if check_atomic(&last.segment) {
        // Skip one atomic segment.
        new_cursor -= pi_js::str16::len(&last.segment) as i64;
    } else if last.is_word_like.unwrap_or(false) {
        // Skip inside one word-like segment, preserving ASCII punctuation boundaries.
        let segment = &last.segment;
        let seg_len = pi_js::str16::len(segment) as i64;
        if let Some(last_match) = PUNCTUATION_REGEX.find_iter(segment).last() {
            let match_end_u16 = pi_js::str16::byte_to_utf16(segment, last_match.end()) as i64;
            new_cursor -= seg_len - match_end_u16;
        } else {
            new_cursor -= seg_len;
        }
    } else {
        // Skip non-word non-whitespace run (punctuation)
        while let Some(seg) = segments.last() {
            if check_atomic(&seg.segment) || seg.is_word_like.unwrap_or(false) || is_whitespace_char(&seg.segment) {
                break;
            }
            let popped = segments.pop().unwrap();
            new_cursor -= pi_js::str16::len(&popped.segment) as i64;
        }
    }

    new_cursor
}

/// Find the cursor position after moving one word forward from `cursor` in `text`.
/// Skips leading whitespace, then stops at the next word/punctuation boundary.
///
/// Pure function - does not mutate any state.
pub fn find_word_forward(text: &str, cursor: i64, options: Option<WordNavigationOptions>) -> i64 {
    let total_len = pi_js::str16::len(text) as i64;
    if cursor >= total_len {
        return total_len;
    }

    let text_after_cursor = pi_js::str16::slice(text, cursor, None);
    let segment_fn = options.as_ref().and_then(|o| o.segment.as_ref());
    let is_atomic = options.as_ref().and_then(|o| o.is_atomic_segment.as_ref());
    let segments = match segment_fn {
        Some(f) => f(&text_after_cursor),
        None => get_word_segmenter().segment(&text_after_cursor),
    };
    let mut iter = segments.into_iter().peekable();
    let mut new_cursor = cursor;

    let check_atomic = |s: &str| -> bool { is_atomic.is_some_and(|f| f(s)) };

    // Skip leading whitespace
    while let Some(seg) = iter.peek() {
        if check_atomic(&seg.segment) || !is_whitespace_char(&seg.segment) {
            break;
        }
        new_cursor += pi_js::str16::len(&seg.segment) as i64;
        iter.next();
    }

    let Some(next) = iter.peek() else {
        return new_cursor;
    };

    if check_atomic(&next.segment) {
        // Skip one atomic segment.
        new_cursor += pi_js::str16::len(&next.segment) as i64;
    } else if next.is_word_like.unwrap_or(false) {
        // Skip inside one word-like segment, preserving ASCII punctuation boundaries.
        let segment = &next.segment;
        let offset = match PUNCTUATION_REGEX.find(segment) {
            Some(m) => pi_js::str16::byte_to_utf16(segment, m.start()) as i64,
            None => pi_js::str16::len(segment) as i64,
        };
        new_cursor += offset;
    } else {
        // Skip non-word non-whitespace run (punctuation)
        for seg in iter {
            if check_atomic(&seg.segment) || seg.is_word_like.unwrap_or(false) || is_whitespace_char(&seg.segment) {
                break;
            }
            new_cursor += pi_js::str16::len(&seg.segment) as i64;
        }
    }

    new_cursor
}
