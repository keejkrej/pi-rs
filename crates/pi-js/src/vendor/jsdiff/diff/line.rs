//! Port of `diff/libesm/diff/line.js` (diff@8.0.4).

use crate::vendor::jsdiff::diff::base::{Diff, base_equals_str, remove_empty_strings};
use crate::vendor::jsdiff::types::{AbortableDiffOptions, Change, DiffLinesOptions, truthy};
use crate::vendor::jsdiff::util::string::js_trim;

#[derive(Clone, Copy, Debug, Default)]
pub struct LineDiff;

impl Diff for LineDiff {
    type Value = str;
    type Output = String;
    type Token = String;
    type Options = DiffLinesOptions;

    fn tokenize(&self, value: &str, options: &DiffLinesOptions) -> Vec<String> {
        tokenize(value, options)
    }

    fn remove_empty(&self, tokens: Vec<String>) -> Vec<String> {
        remove_empty_strings(tokens)
    }

    fn equals(&self, left: &String, right: &String, options: &DiffLinesOptions) -> bool {
        let mut left: &str = left;
        let mut right: &str = right;
        // If we're ignoring whitespace, we need to normalise lines by stripping
        // whitespace before checking equality. (This has an annoying interaction
        // with newlineIsToken that requires special handling: if newlines get their
        // own token, then we DON'T want to trim the *newline* tokens down to empty
        // strings, since this would cause us to treat whitespace-only line content
        // as equal to a separator between lines, which would be weird and
        // inconsistent with the documented behavior of the options.)
        if truthy(options.ignore_whitespace) {
            if !truthy(options.newline_is_token) || !left.contains('\n') {
                left = js_trim(left);
            }
            if !truthy(options.newline_is_token) || !right.contains('\n') {
                right = js_trim(right);
            }
        } else if truthy(options.ignore_newline_at_eof) && !truthy(options.newline_is_token) {
            if let Some(stripped) = left.strip_suffix('\n') {
                left = stripped;
            }
            if let Some(stripped) = right.strip_suffix('\n') {
                right = stripped;
            }
        }
        base_equals_str(left, right, false)
    }

    fn join(&self, tokens: &[String]) -> String {
        tokens.concat()
    }

    fn one_change_per_token(&self, options: &DiffLinesOptions) -> bool {
        truthy(options.one_change_per_token)
    }
}

pub const LINE_DIFF: LineDiff = LineDiff;

/// diffs two blocks of text, treating each line as a token.
/// @returns a list of change objects.
pub fn diff_lines(old_str: &str, new_str: &str, options: Option<DiffLinesOptions>) -> Vec<Change> {
    LineDiff.diff(old_str, new_str, &options.unwrap_or_default())
}

// PORT: `diffLines` with `maxEditLength` / `timeout`; `None` where jsdiff returns `undefined`.
pub fn diff_lines_abortable(
    old_str: &str,
    new_str: &str,
    options: Option<DiffLinesOptions>,
    abortable: AbortableDiffOptions,
) -> Option<Vec<Change>> {
    LineDiff.diff_abortable(old_str, new_str, &options.unwrap_or_default(), &abortable)
}

// PORT: `generateOptions(options, { ignoreWhitespace: true })`.
fn trimmed_options(options: Option<DiffLinesOptions>) -> DiffLinesOptions {
    let mut options = options.unwrap_or_default();
    if options.ignore_whitespace.is_none() {
        options.ignore_whitespace = Some(true);
    }
    options
}

pub fn diff_trimmed_lines(old_str: &str, new_str: &str, options: Option<DiffLinesOptions>) -> Vec<Change> {
    LineDiff.diff(old_str, new_str, &trimmed_options(options))
}

// PORT: `diffTrimmedLines` with `maxEditLength` / `timeout`; `None` where jsdiff returns `undefined`.
pub fn diff_trimmed_lines_abortable(
    old_str: &str,
    new_str: &str,
    options: Option<DiffLinesOptions>,
    abortable: AbortableDiffOptions,
) -> Option<Vec<Change>> {
    LineDiff.diff_abortable(old_str, new_str, &trimmed_options(options), &abortable)
}

/// Exported standalone so it can be used from jsonDiff too.
pub fn tokenize(value: &str, options: &DiffLinesOptions) -> Vec<String> {
    let stripped;
    let mut value = value;
    if truthy(options.strip_trailing_cr) {
        // remove one \r before \n to match GNU diff's --strip-trailing-cr behavior
        stripped = value.replace("\r\n", "\n");
        value = &stripped;
    }
    let mut lines_and_newlines = split_lines_and_newlines(value);
    // Ignore the final empty token that occurs if the string ends with a new line
    if lines_and_newlines.last().is_some_and(|last| last.is_empty()) {
        lines_and_newlines.pop();
    }
    // Merge the content and line separators into single tokens
    let mut ret_lines: Vec<String> = Vec::with_capacity(lines_and_newlines.len() / 2 + 1);
    for (i, line) in lines_and_newlines.into_iter().enumerate() {
        if i % 2 == 1 && !truthy(options.newline_is_token) {
            if let Some(last) = ret_lines.last_mut() {
                last.push_str(line);
            }
        } else {
            ret_lines.push(line.to_string());
        }
    }
    ret_lines
}

// PORT: `value.split(/(\n|\r\n)/)`: contents interleaved with the captured separators.
fn split_lines_and_newlines(value: &str) -> Vec<&str> {
    let bytes = value.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        let sep_len = if bytes[i] == b'\n' {
            1
        } else if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
            2
        } else {
            i += 1;
            continue;
        };
        parts.push(&value[start..i]);
        parts.push(&value[i..i + sep_len]);
        i += sep_len;
        start = i;
    }
    parts.push(&value[start..]);
    parts
}
