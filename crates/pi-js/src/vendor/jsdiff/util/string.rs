//! Port of `diff/libesm/util/string.js` (diff@8.0.4).

// PORT: the JS helpers index strings by UTF-16 code unit; these work on `char`s. Results only differ
// where JS would split a surrogate pair (e.g. `longestCommonPrefix('😀', '😃')` is a lone surrogate
// in JS and `""` here). `diffWords` only calls them on whitespace, which is all in the BMP.

use crate::error::{Error, Result};
use crate::vendor::jsdiff::types::WordSegmenter;

// PORT: The ECMAScript `WhiteSpace` + `LineTerminator` set (what `\s` and `String#trim` use).
pub(crate) fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
    )
}

// PORT: `/\s/.test(s)`.
pub(crate) fn has_js_whitespace(s: &str) -> bool {
    s.chars().any(is_js_whitespace)
}

// PORT: `s.trim()`.
pub(crate) fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_whitespace)
}

// PORT: `JSON.stringify(s)` for a string.
pub(crate) fn json_quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

pub fn longest_common_prefix(str1: &str, str2: &str) -> String {
    let mut end = 0;
    for ((i, a), b) in str1.char_indices().zip(str2.chars()) {
        if a != b {
            return str1[..i].to_string();
        }
        end = i + a.len_utf8();
    }
    str1[..end].to_string()
}

pub fn longest_common_suffix(str1: &str, str2: &str) -> String {
    // Unlike longestCommonPrefix, we need a special case to handle all scenarios
    // where we return the empty string since str1.slice(-0) will return the
    // entire string.
    if str1.is_empty() || str2.is_empty() || str1.chars().next_back() != str2.chars().next_back() {
        return String::new();
    }
    let mut start = str1.len();
    for ((i, a), b) in str1.char_indices().rev().zip(str2.chars().rev()) {
        if a != b {
            return str1[start..].to_string();
        }
        start = i;
    }
    str1[start..].to_string()
}

pub fn replace_prefix(string: &str, old_prefix: &str, new_prefix: &str) -> Result<String> {
    if !string.starts_with(old_prefix) {
        return Err(Error::msg(format!(
            "string {} doesn't start with prefix {}; this is a bug",
            json_quote(string),
            json_quote(old_prefix)
        )));
    }
    Ok(format!("{}{}", new_prefix, &string[old_prefix.len()..]))
}

pub fn replace_suffix(string: &str, old_suffix: &str, new_suffix: &str) -> Result<String> {
    if old_suffix.is_empty() {
        return Ok(format!("{string}{new_suffix}"));
    }
    if !string.ends_with(old_suffix) {
        return Err(Error::msg(format!(
            "string {} doesn't end with suffix {}; this is a bug",
            json_quote(string),
            json_quote(old_suffix)
        )));
    }
    Ok(format!("{}{}", &string[..string.len() - old_suffix.len()], new_suffix))
}

pub fn remove_prefix(string: &str, old_prefix: &str) -> Result<String> {
    replace_prefix(string, old_prefix, "")
}

pub fn remove_suffix(string: &str, old_suffix: &str) -> Result<String> {
    replace_suffix(string, old_suffix, "")
}

pub fn maximum_overlap(string1: &str, string2: &str) -> String {
    let count = overlap_count(string1, string2);
    string2.chars().take(count).collect()
}

// Nicked from https://stackoverflow.com/a/60422853/1709587
fn overlap_count(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    // Deal with cases where the strings differ in length
    let mut start_a = 0;
    if a.len() > b.len() {
        start_a = a.len() - b.len();
    }
    let mut end_b = b.len();
    if a.len() < b.len() {
        end_b = a.len();
    }
    // Create a back-reference for each index
    //   that should be followed in case of a mismatch.
    //   We only need B to make these references:
    let mut map = vec![0usize; end_b.max(1)];
    let mut k = 0; // Index that lags behind j
    map[0] = 0;
    for j in 1..end_b {
        if b[j] == b[k] {
            map[j] = map[k]; // skip over the same character (optional optimisation)
        } else {
            map[j] = k;
        }
        while k > 0 && b[j] != b[k] {
            k = map[k];
        }
        if b[j] == b[k] {
            k += 1;
        }
    }
    // Phase 2: use these references while iterating over A
    k = 0;
    for &ai in &a[start_a..] {
        while k > 0 && b.get(k) != Some(&ai) {
            k = map.get(k).copied().unwrap_or(0);
        }
        if b.get(k) == Some(&ai) {
            k += 1;
        }
    }
    k
}

/// Returns true if the string consistently uses Windows line endings.
pub fn has_only_win_line_endings(string: &str) -> bool {
    string.contains("\r\n") && !string.starts_with('\n') && !has_lf_without_cr(string)
}

/// Returns true if the string consistently uses Unix line endings.
pub fn has_only_unix_line_endings(string: &str) -> bool {
    !string.contains("\r\n") && string.contains('\n')
}

// PORT: `string.match(/[^\r]\n/)`.
fn has_lf_without_cr(string: &str) -> bool {
    let bytes = string.as_bytes();
    (1..bytes.len()).any(|i| bytes[i] == b'\n' && bytes[i - 1] != b'\r')
}

/// Split a string into segments using a word segmenter, merging consecutive
/// segments if they are both whitespace segments. Whitespace segments can
/// appear adjacent to one another for two reasons:
/// - newlines always get their own segment
/// - where a diacritic is attached to a whitespace character in the text, the
///   segment ends after the diacritic, so e.g. " \u0300 " becomes two segments.
///
/// This function therefore runs the segmenter's .segment() method and then
/// merges consecutive segments of whitespace into a single part.
pub fn segment(string: &str, segmenter: &WordSegmenter) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    for segment in segmenter(string) {
        match parts.last_mut() {
            Some(last) if has_js_whitespace(last) && has_js_whitespace(&segment) => last.push_str(&segment),
            _ => parts.push(segment),
        }
    }
    parts
}

// The functions below take a `segmenter` argument so that, when called from
// diffWords when it is using a segmenter, they can use a notion of what
// constitutes "whitespace" that is consistent with the segmenter.
//
// USUALLY this will be identical to the result of the non-segmenter-based
// logic, but it differs in at least one case: when whitespace characters are
// modified by diacritics. A word segmenter considers these diacritics to be
// part of the whitespace, whereas our non-segmenter-based logic does not.
//
// Because the segmenter-based approach necessarily requires segmenting the
// entire string, we offer a leadingAndTrailingWs function to allow getting the
// whitespace prefix AND whitespace suffix with a single call to the segmenter,
// for efficiency's sake.
pub fn trailing_ws(string: &str, segmenter: Option<&WordSegmenter>) -> String {
    if let Some(segmenter) = segmenter {
        return leading_and_trailing_ws(string, Some(segmenter)).1;
    }
    let mut start = string.len();
    for (i, c) in string.char_indices().rev() {
        if !is_js_whitespace(c) {
            break;
        }
        start = i;
    }
    string[start..].to_string()
}

pub fn leading_ws(string: &str, segmenter: Option<&WordSegmenter>) -> String {
    if let Some(segmenter) = segmenter {
        return leading_and_trailing_ws(string, Some(segmenter)).0;
    }
    let end = string.find(|c: char| !is_js_whitespace(c)).unwrap_or(string.len());
    string[..end].to_string()
}

pub fn leading_and_trailing_ws(string: &str, segmenter: Option<&WordSegmenter>) -> (String, String) {
    let Some(segmenter) = segmenter else {
        return (leading_ws(string, None), trailing_ws(string, None));
    };
    let segments = segment(string, segmenter);
    let head = match segments.first() {
        Some(first) if has_js_whitespace(first) => first.clone(),
        _ => String::new(),
    };
    let tail = match segments.last() {
        Some(last) if has_js_whitespace(last) => last.clone(),
        _ => String::new(),
    };
    (head, tail)
}
