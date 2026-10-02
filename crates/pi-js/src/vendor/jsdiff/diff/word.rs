//! Port of `diff/libesm/diff/word.js` (diff@8.0.4).

use crate::error::Result;
use crate::vendor::jsdiff::diff::base::{Diff, base_equals_str, remove_empty_strings};
use crate::vendor::jsdiff::types::{AbortableDiffOptions, Change, DiffWordsOptions, WordSegmenter, truthy};
use crate::vendor::jsdiff::util::string::{
    has_js_whitespace, is_js_whitespace, js_trim, leading_and_trailing_ws, leading_ws, longest_common_prefix,
    longest_common_suffix, maximum_overlap, remove_prefix, remove_suffix, replace_prefix, replace_suffix, segment,
    trailing_ws,
};

// Based on https://en.wikipedia.org/wiki/Latin_script_in_Unicode
//
// Chars/ranges counted as "word" characters by this regex are as follows:
//
// + U+00AD  Soft hyphen
// + 00C0–00FF (letters with diacritics from the Latin-1 Supplement), except:
//   - U+00D7  × Multiplication sign
//   - U+00F7  ÷ Division sign
// + Latin Extended-A, 0100–017F
// + Latin Extended-B, 0180–024F
// + IPA Extensions, 0250–02AF
// + Spacing Modifier Letters, 02B0–02FF, except:
//   - U+02C7  ˇ &#711;  Caron
//   - U+02D8  ˘ &#728;  Breve
//   - U+02D9  ˙ &#729;  Dot Above
//   - U+02DA  ˚ &#730;  Ring Above
//   - U+02DB  ˛ &#731;  Ogonek
//   - U+02DC  ˜ &#732;  Small Tilde
//   - U+02DD  ˝ &#733;  Double Acute Accent
// + Latin Extended Additional, 1E00–1EFF
fn is_extended_word_char(c: char) -> bool {
    matches!(
        c,
        'a'..='z'
            | 'A'..='Z'
            | '0'..='9'
            | '_'
            | '\u{AD}'
            | '\u{C0}'..='\u{D6}'
            | '\u{D8}'..='\u{F6}'
            | '\u{F8}'..='\u{2C6}'
            | '\u{2C8}'..='\u{2D7}'
            | '\u{2DE}'..='\u{2FF}'
            | '\u{1E00}'..='\u{1EFF}'
    )
}

// PORT: Byte length of the run of chars at the start of `s` satisfying `pred` (at least one char must match).
fn run_len(s: &str, pred: impl Fn(char) -> bool) -> usize {
    s.find(|c: char| !pred(c)).unwrap_or(s.len())
}

// Each token is one of the following:
// - A punctuation mark plus the surrounding whitespace
// - A word plus the surrounding whitespace
// - Pure whitespace (but only in the special case where the entire text
//   is just whitespace)
//
// We have to include surrounding whitespace in the tokens because the two
// alternative approaches produce horribly broken results:
// * If we just discard the whitespace, we can't fully reproduce the original
//   text from the sequence of tokens and any attempt to render the diff will
//   get the whitespace wrong.
// * If we have separate tokens for whitespace, then in a typical text every
//   second token will be a single space character. But this often results in
//   the optimal diff between two texts being a perverse one that preserves
//   the spaces between words but deletes and reinserts actual common words.
//   See https://github.com/kpdecker/jsdiff/issues/160#issuecomment-1866099640
//   for an example.
//
// Keeping the surrounding whitespace of course has implications for .equals
// and .join, not just .tokenize.
// This regex does NOT fully implement the tokenization rules described above.
// Instead, it gives runs of whitespace their own "token". The tokenize method
// then handles stitching whitespace tokens onto adjacent word or punctuation
// tokens.
//
// PORT: `value.match(/[<extendedWordChars>]+|\s+|[^<extendedWordChars>]/ug) || []`
fn tokenize_including_whitespace(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut rest = value;
    while let Some(c) = rest.chars().next() {
        let len = if is_extended_word_char(c) {
            run_len(rest, is_extended_word_char)
        } else if is_js_whitespace(c) {
            run_len(rest, is_js_whitespace)
        } else {
            c.len_utf8()
        };
        parts.push(&rest[..len]);
        rest = &rest[len..];
    }
    parts
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WordDiff;

impl Diff for WordDiff {
    type Value = str;
    type Output = String;
    type Token = String;
    type Options = DiffWordsOptions;

    fn equals(&self, left: &String, right: &String, options: &DiffWordsOptions) -> bool {
        if truthy(options.ignore_case) {
            return js_trim(&left.to_lowercase()) == js_trim(&right.to_lowercase());
        }
        js_trim(left) == js_trim(right)
    }

    fn tokenize(&self, value: &str, options: &DiffWordsOptions) -> Vec<String> {
        // We want `parts` to be an array whose elements alternate between being
        // pure whitespace and being pure non-whitespace. This is ALMOST what the
        // segments returned by a word-based Intl.Segmenter already look like,
        // but not quite - see explanation in the docs of our custom segment()
        // function.
        let parts: Vec<String> = match &options.intl_segmenter {
            Some(segmenter) => segment(value, segmenter),
            None => tokenize_including_whitespace(value)
                .into_iter()
                .map(String::from)
                .collect(),
        };
        let mut tokens: Vec<String> = Vec::new();
        let mut prev_part: Option<&str> = None;
        for part in &parts {
            if has_js_whitespace(part) {
                match (prev_part, tokens.last_mut()) {
                    (Some(_), Some(last)) => last.push_str(part),
                    _ => tokens.push(part.clone()),
                }
            } else if let Some(prev) = prev_part.filter(|prev| has_js_whitespace(prev)) {
                match tokens.last_mut() {
                    Some(last) if last == prev => last.push_str(part),
                    _ => tokens.push(format!("{prev}{part}")),
                }
            } else {
                tokens.push(part.clone());
            }
            prev_part = Some(part);
        }
        tokens
    }

    fn remove_empty(&self, tokens: Vec<String>) -> Vec<String> {
        remove_empty_strings(tokens)
    }

    fn join(&self, tokens: &[String]) -> String {
        // Tokens being joined here will always have appeared consecutively in the
        // same text, so we can simply strip off the leading whitespace from all the
        // tokens except the first (and except any whitespace-only tokens - but such
        // a token will always be the first and only token anyway) and then join them
        // and the whitespace around words and punctuation will end up correct.
        let mut out = String::new();
        for (i, token) in tokens.iter().enumerate() {
            if i == 0 {
                out.push_str(token);
            } else {
                out.push_str(token.trim_start_matches(is_js_whitespace));
            }
        }
        out
    }

    fn post_process(&self, mut changes: Vec<Change>, options: &DiffWordsOptions) -> Vec<Change> {
        if truthy(options.one_change_per_token) {
            return changes;
        }
        let segmenter = options.intl_segmenter.as_ref();
        let mut last_keep: Option<usize> = None;
        // Change objects representing any insertion or deletion since the last
        // "keep" change object. There can be at most one of each.
        let mut insertion: Option<usize> = None;
        let mut deletion: Option<usize> = None;
        for i in 0..changes.len() {
            if changes[i].added {
                insertion = Some(i);
            } else if changes[i].removed {
                deletion = Some(i);
            } else {
                if insertion.is_some() || deletion.is_some() {
                    // May be false at start of text
                    dedupe_whitespace_in_change_objects(
                        &mut changes,
                        last_keep,
                        deletion,
                        insertion,
                        Some(i),
                        segmenter,
                    );
                }
                last_keep = Some(i);
                insertion = None;
                deletion = None;
            }
        }
        if insertion.is_some() || deletion.is_some() {
            dedupe_whitespace_in_change_objects(&mut changes, last_keep, deletion, insertion, None, segmenter);
        }
        changes
    }

    fn one_change_per_token(&self, options: &DiffWordsOptions) -> bool {
        truthy(options.one_change_per_token)
    }
}

pub const WORD_DIFF: WordDiff = WordDiff;

/// diffs two blocks of text, treating each word and each punctuation mark as a token.
/// Whitespace is ignored when computing the diff (but preserved as far as possible in the final change objects).
///
/// @returns a list of change objects.
pub fn diff_words(old_str: &str, new_str: &str, options: Option<DiffWordsOptions>) -> Vec<Change> {
    let options = options.unwrap_or_default();
    // This option has never been documented and never will be (it's clearer to
    // just call `diffWordsWithSpace` directly if you need that behavior), but
    // has existed in jsdiff for a long time, so we retain support for it here
    // for the sake of backwards compatibility.
    if options.ignore_whitespace == Some(false) {
        return diff_words_with_space(old_str, new_str, Some(options));
    }
    WordDiff.diff(old_str, new_str, &options)
}

// PORT: `diffWords` with `maxEditLength` / `timeout`; `None` where jsdiff returns `undefined`.
pub fn diff_words_abortable(
    old_str: &str,
    new_str: &str,
    options: Option<DiffWordsOptions>,
    abortable: AbortableDiffOptions,
) -> Option<Vec<Change>> {
    let options = options.unwrap_or_default();
    if options.ignore_whitespace == Some(false) {
        return diff_words_with_space_abortable(old_str, new_str, Some(options), abortable);
    }
    WordDiff.diff_abortable(old_str, new_str, &options, &abortable)
}

// PORT: the string helpers throw "...; this is a bug" only when an internal invariant is broken;
// `diffWords` panics with that message instead of returning a `Result`.
fn invariant(result: Result<String>) -> String {
    match result {
        Ok(value) => value,
        Err(err) => panic!("{err}"),
    }
}

fn dedupe_whitespace_in_change_objects(
    changes: &mut [Change],
    start_keep: Option<usize>,
    deletion: Option<usize>,
    insertion: Option<usize>,
    end_keep: Option<usize>,
    segmenter: Option<&WordSegmenter>,
) {
    // Before returning, we tidy up the leading and trailing whitespace of the
    // change objects to eliminate cases where trailing whitespace in one object
    // is repeated as leading whitespace in the next.
    // Below are examples of the outcomes we want here to explain the code.
    // I=insert, K=keep, D=delete
    // 1. diffing 'foo bar baz' vs 'foo baz'
    //    Prior to cleanup, we have K:'foo ' D:' bar ' K:' baz'
    //    After cleanup, we want:   K:'foo ' D:'bar ' K:'baz'
    //
    // 2. Diffing 'foo bar baz' vs 'foo qux baz'
    //    Prior to cleanup, we have K:'foo ' D:' bar ' I:' qux ' K:' baz'
    //    After cleanup, we want K:'foo ' D:'bar' I:'qux' K:' baz'
    //
    // 3. Diffing 'foo\nbar baz' vs 'foo baz'
    //    Prior to cleanup, we have K:'foo ' D:'\nbar ' K:' baz'
    //    After cleanup, we want K'foo' D:'\nbar' K:' baz'
    //
    // 4. Diffing 'foo baz' vs 'foo\nbar baz'
    //    Prior to cleanup, we have K:'foo\n' I:'\nbar ' K:' baz'
    //    After cleanup, we ideally want K'foo' I:'\nbar' K:' baz'
    //    but don't actually manage this currently (the pre-cleanup change
    //    objects don't contain enough information to make it possible).
    //
    // 5. Diffing 'foo   bar baz' vs 'foo  baz'
    //    Prior to cleanup, we have K:'foo  ' D:'   bar ' K:'  baz'
    //    After cleanup, we want K:'foo  ' D:' bar ' K:'baz'
    //
    // Our handling is unavoidably imperfect in the case where there's a single
    // indel between keeps and the whitespace has changed. For instance, consider
    // diffing 'foo\tbar\nbaz' vs 'foo baz'. Unless we create an extra change
    // object to represent the insertion of the space character (which isn't even
    // a token), we have no way to avoid losing information about the texts'
    // original whitespace in the result we return. Still, we do our best to
    // output something that will look sensible if we e.g. print it with
    // insertions in green and deletions in red.
    // Between two "keep" change objects (or before the first or after the last
    // change object), we can have either:
    // * A "delete" followed by an "insert"
    // * Just an "insert"
    // * Just a "delete"
    // We handle the three cases separately.
    if let (Some(deletion), Some(insertion)) = (deletion, insertion) {
        let (old_ws_prefix, old_ws_suffix) = leading_and_trailing_ws(&changes[deletion].value, segmenter);
        let (new_ws_prefix, new_ws_suffix) = leading_and_trailing_ws(&changes[insertion].value, segmenter);
        if let Some(start_keep) = start_keep {
            let common_ws_prefix = longest_common_prefix(&old_ws_prefix, &new_ws_prefix);
            changes[start_keep].value = invariant(replace_suffix(
                &changes[start_keep].value,
                &new_ws_prefix,
                &common_ws_prefix,
            ));
            changes[deletion].value = invariant(remove_prefix(&changes[deletion].value, &common_ws_prefix));
            changes[insertion].value = invariant(remove_prefix(&changes[insertion].value, &common_ws_prefix));
        }
        if let Some(end_keep) = end_keep {
            let common_ws_suffix = longest_common_suffix(&old_ws_suffix, &new_ws_suffix);
            changes[end_keep].value = invariant(replace_prefix(
                &changes[end_keep].value,
                &new_ws_suffix,
                &common_ws_suffix,
            ));
            changes[deletion].value = invariant(remove_suffix(&changes[deletion].value, &common_ws_suffix));
            changes[insertion].value = invariant(remove_suffix(&changes[insertion].value, &common_ws_suffix));
        }
    } else if let Some(insertion) = insertion {
        // The whitespaces all reflect what was in the new text rather than
        // the old, so we essentially have no information about whitespace
        // insertion or deletion. We just want to dedupe the whitespace.
        // We do that by having each change object keep its trailing
        // whitespace and deleting duplicate leading whitespace where
        // present.
        if start_keep.is_some() {
            let ws = leading_ws(&changes[insertion].value, segmenter);
            changes[insertion].value = changes[insertion].value[ws.len()..].to_string();
        }
        if let Some(end_keep) = end_keep {
            let ws = leading_ws(&changes[end_keep].value, segmenter);
            changes[end_keep].value = changes[end_keep].value[ws.len()..].to_string();
        }
        // otherwise we've got a deletion and no insertion
    } else if let Some(deletion) = deletion {
        if let (Some(start_keep), Some(end_keep)) = (start_keep, end_keep) {
            let new_ws_full = leading_ws(&changes[end_keep].value, segmenter);
            let (del_ws_start, del_ws_end) = leading_and_trailing_ws(&changes[deletion].value, segmenter);
            // Any whitespace that comes straight after startKeep in both the old and
            // new texts, assign to startKeep and remove from the deletion.
            let new_ws_start = longest_common_prefix(&new_ws_full, &del_ws_start);
            changes[deletion].value = invariant(remove_prefix(&changes[deletion].value, &new_ws_start));
            // Any whitespace that comes straight before endKeep in both the old and
            // new texts, and hasn't already been assigned to startKeep, assign to
            // endKeep and remove from the deletion.
            let new_ws_end = longest_common_suffix(&invariant(remove_prefix(&new_ws_full, &new_ws_start)), &del_ws_end);
            changes[deletion].value = invariant(remove_suffix(&changes[deletion].value, &new_ws_end));
            changes[end_keep].value = invariant(replace_prefix(&changes[end_keep].value, &new_ws_full, &new_ws_end));
            // If there's any whitespace from the new text that HASN'T already been
            // assigned, assign it to the start:
            let unassigned = &new_ws_full[..new_ws_full.len() - new_ws_end.len()];
            changes[start_keep].value = invariant(replace_suffix(&changes[start_keep].value, &new_ws_full, unassigned));
        } else if let Some(end_keep) = end_keep {
            // We are at the start of the text. Preserve all the whitespace on
            // endKeep, and just remove whitespace from the end of deletion to the
            // extent that it overlaps with the start of endKeep.
            let end_keep_ws_prefix = leading_ws(&changes[end_keep].value, segmenter);
            let deletion_ws_suffix = trailing_ws(&changes[deletion].value, segmenter);
            let overlap = maximum_overlap(&deletion_ws_suffix, &end_keep_ws_prefix);
            changes[deletion].value = invariant(remove_suffix(&changes[deletion].value, &overlap));
        } else if let Some(start_keep) = start_keep {
            // We are at the END of the text. Preserve all the whitespace on
            // startKeep, and just remove whitespace from the start of deletion to
            // the extent that it overlaps with the end of startKeep.
            let start_keep_ws_suffix = trailing_ws(&changes[start_keep].value, segmenter);
            let deletion_ws_prefix = leading_ws(&changes[deletion].value, segmenter);
            let overlap = maximum_overlap(&start_keep_ws_suffix, &deletion_ws_prefix);
            changes[deletion].value = invariant(remove_prefix(&changes[deletion].value, &overlap));
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WordsWithSpaceDiff;

impl Diff for WordsWithSpaceDiff {
    type Value = str;
    type Output = String;
    type Token = String;
    type Options = DiffWordsOptions;

    fn tokenize(&self, value: &str, _options: &DiffWordsOptions) -> Vec<String> {
        // Slightly different to the tokenizeIncludingWhitespace regex used above in
        // that this one treats each individual newline as a distinct token, rather
        // than merging them into other surrounding whitespace. This was requested
        // in https://github.com/kpdecker/jsdiff/issues/180 &
        //    https://github.com/kpdecker/jsdiff/issues/211
        //
        // PORT: value.match(/(\r?\n)|[<extendedWordChars>]+|[^\S\n\r]+|[^<extendedWordChars>]/ug) || []
        let is_inline_ws = |c: char| is_js_whitespace(c) && c != '\n' && c != '\r';
        let mut tokens = Vec::new();
        let mut rest = value;
        while let Some(c) = rest.chars().next() {
            let len = if rest.starts_with("\r\n") {
                2
            } else if c == '\n' {
                1
            } else if is_extended_word_char(c) {
                run_len(rest, is_extended_word_char)
            } else if is_inline_ws(c) {
                run_len(rest, is_inline_ws)
            } else {
                c.len_utf8()
            };
            tokens.push(rest[..len].to_string());
            rest = &rest[len..];
        }
        tokens
    }

    fn remove_empty(&self, tokens: Vec<String>) -> Vec<String> {
        remove_empty_strings(tokens)
    }

    fn equals(&self, left: &String, right: &String, options: &DiffWordsOptions) -> bool {
        base_equals_str(left, right, truthy(options.ignore_case))
    }

    fn join(&self, tokens: &[String]) -> String {
        tokens.concat()
    }

    fn one_change_per_token(&self, options: &DiffWordsOptions) -> bool {
        truthy(options.one_change_per_token)
    }
}

pub const WORDS_WITH_SPACE_DIFF: WordsWithSpaceDiff = WordsWithSpaceDiff;

/// diffs two blocks of text, treating each word, punctuation mark, newline, or run of (non-newline) whitespace as a token.
/// @returns a list of change objects
pub fn diff_words_with_space(old_str: &str, new_str: &str, options: Option<DiffWordsOptions>) -> Vec<Change> {
    WordsWithSpaceDiff.diff(old_str, new_str, &options.unwrap_or_default())
}

// PORT: `diffWordsWithSpace` with `maxEditLength` / `timeout`; `None` where jsdiff returns `undefined`.
pub fn diff_words_with_space_abortable(
    old_str: &str,
    new_str: &str,
    options: Option<DiffWordsOptions>,
    abortable: AbortableDiffOptions,
) -> Option<Vec<Change>> {
    WordsWithSpaceDiff.diff_abortable(old_str, new_str, &options.unwrap_or_default(), &abortable)
}
