//! Port of `diff/libesm/diff/css.js` (diff@8.0.4).

use crate::vendor::jsdiff::diff::base::{Diff, base_equals_str, remove_empty_strings};
use crate::vendor::jsdiff::types::{AbortableDiffOptions, Change, DiffCssOptions, truthy};
use crate::vendor::jsdiff::util::string::is_js_whitespace;

#[derive(Clone, Copy, Debug, Default)]
pub struct CssDiff;

impl Diff for CssDiff {
    type Value = str;
    type Output = String;
    type Token = String;
    type Options = DiffCssOptions;

    fn tokenize(&self, value: &str, _options: &DiffCssOptions) -> Vec<String> {
        // value.split(/([{}:;,]|\s+)/)
        let mut out = Vec::new();
        let mut start = 0;
        let mut chars = value.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            let end = if matches!(c, '{' | '}' | ':' | ';' | ',') {
                i + 1
            } else if is_js_whitespace(c) {
                let mut end = i + c.len_utf8();
                while let Some(&(j, d)) = chars.peek() {
                    if !is_js_whitespace(d) {
                        break;
                    }
                    end = j + d.len_utf8();
                    chars.next();
                }
                end
            } else {
                continue;
            };
            out.push(value[start..i].to_string());
            out.push(value[i..end].to_string());
            start = end;
        }
        out.push(value[start..].to_string());
        out
    }

    fn remove_empty(&self, tokens: Vec<String>) -> Vec<String> {
        remove_empty_strings(tokens)
    }

    fn equals(&self, left: &String, right: &String, _options: &DiffCssOptions) -> bool {
        base_equals_str(left, right, false)
    }

    fn join(&self, tokens: &[String]) -> String {
        tokens.concat()
    }

    fn one_change_per_token(&self, options: &DiffCssOptions) -> bool {
        truthy(options.one_change_per_token)
    }
}

pub const CSS_DIFF: CssDiff = CssDiff;

/// diffs two blocks of text, comparing CSS tokens.
///
/// @returns a list of change objects.
pub fn diff_css(old_str: &str, new_str: &str, options: Option<DiffCssOptions>) -> Vec<Change> {
    CssDiff.diff(old_str, new_str, &options.unwrap_or_default())
}

// PORT: `diffCss` with `maxEditLength` / `timeout`; `None` where jsdiff returns `undefined`.
pub fn diff_css_abortable(
    old_str: &str,
    new_str: &str,
    options: Option<DiffCssOptions>,
    abortable: AbortableDiffOptions,
) -> Option<Vec<Change>> {
    CssDiff.diff_abortable(old_str, new_str, &options.unwrap_or_default(), &abortable)
}
