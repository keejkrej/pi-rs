//! Port of `diff/libesm/diff/character.js` (diff@8.0.4).

use crate::vendor::jsdiff::diff::base::{Diff, base_equals_str, remove_empty_strings};
use crate::vendor::jsdiff::types::{AbortableDiffOptions, Change, DiffCharsOptions, truthy};

// PORT: `CharacterDiff` (the base `Diff` behaviour: one token per code point).
#[derive(Clone, Copy, Debug, Default)]
pub struct CharacterDiff;

impl Diff for CharacterDiff {
    type Value = str;
    type Output = String;
    type Token = String;
    type Options = DiffCharsOptions;

    fn tokenize(&self, value: &str, _options: &DiffCharsOptions) -> Vec<String> {
        // Array.from(value)
        value.chars().map(String::from).collect()
    }

    fn remove_empty(&self, tokens: Vec<String>) -> Vec<String> {
        remove_empty_strings(tokens)
    }

    fn equals(&self, left: &String, right: &String, options: &DiffCharsOptions) -> bool {
        base_equals_str(left, right, truthy(options.ignore_case))
    }

    fn join(&self, tokens: &[String]) -> String {
        tokens.concat()
    }

    fn one_change_per_token(&self, options: &DiffCharsOptions) -> bool {
        truthy(options.one_change_per_token)
    }
}

pub const CHARACTER_DIFF: CharacterDiff = CharacterDiff;

/// diffs two blocks of text, treating each character as a token.
///
/// ("Characters" here means Unicode code points - the elements you get when you loop over a string with a `for ... of ...` loop.)
///
/// @returns a list of change objects.
pub fn diff_chars(old_str: &str, new_str: &str, options: Option<DiffCharsOptions>) -> Vec<Change> {
    CharacterDiff.diff(old_str, new_str, &options.unwrap_or_default())
}

// PORT: `diffChars` with `maxEditLength` / `timeout`; `None` where jsdiff returns `undefined`.
pub fn diff_chars_abortable(
    old_str: &str,
    new_str: &str,
    options: Option<DiffCharsOptions>,
    abortable: AbortableDiffOptions,
) -> Option<Vec<Change>> {
    CharacterDiff.diff_abortable(old_str, new_str, &options.unwrap_or_default(), &abortable)
}
