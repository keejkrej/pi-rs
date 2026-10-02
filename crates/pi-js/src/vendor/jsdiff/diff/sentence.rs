//! Port of `diff/libesm/diff/sentence.js` (diff@8.0.4).

use crate::vendor::jsdiff::diff::base::{Diff, base_equals_str, remove_empty_strings};
use crate::vendor::jsdiff::types::{AbortableDiffOptions, Change, DiffSentencesOptions, truthy};
use crate::vendor::jsdiff::util::string::is_js_whitespace;

fn is_sentence_end_punct(c: char) -> bool {
    c == '.' || c == '!' || c == '?'
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SentenceDiff;

impl Diff for SentenceDiff {
    type Value = str;
    type Output = String;
    type Token = String;
    type Options = DiffSentencesOptions;

    fn tokenize(&self, value: &str, _options: &DiffSentencesOptions) -> Vec<String> {
        // If in future we drop support for environments that don't support lookbehinds, we can replace
        // this entire function with:
        //     return value.split(/(?<=[.!?])(\s+|$)/);
        // but until then, for similar reasons to the trailingWs function in string.ts, we are forced
        // to do this verbosely "by hand" instead of using a regex.
        let chars: Vec<(usize, char)> = value.char_indices().collect();
        let n = chars.len();
        let byte_at = |i: usize| if i < n { chars[i].0 } else { value.len() };
        let mut result = Vec::new();
        let mut token_start = 0usize;
        let mut i = 0usize;
        while i < n {
            if i == n - 1 {
                result.push(value[token_start..].to_string());
                break;
            }
            if is_sentence_end_punct(chars[i].1) && is_js_whitespace(chars[i + 1].1) {
                // We've hit a sentence break - i.e. a punctuation mark followed by whitespace.
                // We now want to push TWO tokens to the result:
                // 1. the sentence
                result.push(value[token_start..byte_at(i + 1)].to_string());
                // 2. the whitespace
                i += 1;
                token_start = byte_at(i);
                while i + 1 < n && is_js_whitespace(chars[i + 1].1) {
                    i += 1;
                }
                result.push(value[token_start..byte_at(i + 1)].to_string());
                // Then the next token (a sentence) starts on the character after the whitespace.
                // (It's okay if this is off the end of the string - then the outer loop will terminate
                // here anyway.)
                token_start = byte_at(i + 1);
            }
            i += 1;
        }
        result
    }

    fn remove_empty(&self, tokens: Vec<String>) -> Vec<String> {
        remove_empty_strings(tokens)
    }

    fn equals(&self, left: &String, right: &String, _options: &DiffSentencesOptions) -> bool {
        base_equals_str(left, right, false)
    }

    fn join(&self, tokens: &[String]) -> String {
        tokens.concat()
    }

    fn one_change_per_token(&self, options: &DiffSentencesOptions) -> bool {
        truthy(options.one_change_per_token)
    }
}

pub const SENTENCE_DIFF: SentenceDiff = SentenceDiff;

/// diffs two blocks of text, treating each sentence, and the whitespace between each pair of sentences, as a token.
/// The characters `.`, `!`, and `?`, when followed by whitespace, are treated as marking the end of a sentence; nothing else besides the end of the string is considered to mark a sentence end.
///
/// (For more sophisticated detection of sentence breaks, including support for non-English punctuation, consider instead tokenizing with an [`Intl.Segmenter`](https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/Intl/Segmenter) with `granularity: 'sentence'` and passing the result to `diffArrays`.)
///
/// @returns a list of change objects.
pub fn diff_sentences(old_str: &str, new_str: &str, options: Option<DiffSentencesOptions>) -> Vec<Change> {
    SentenceDiff.diff(old_str, new_str, &options.unwrap_or_default())
}

// PORT: `diffSentences` with `maxEditLength` / `timeout`; `None` where jsdiff returns `undefined`.
pub fn diff_sentences_abortable(
    old_str: &str,
    new_str: &str,
    options: Option<DiffSentencesOptions>,
    abortable: AbortableDiffOptions,
) -> Option<Vec<Change>> {
    SentenceDiff.diff_abortable(old_str, new_str, &options.unwrap_or_default(), &abortable)
}
