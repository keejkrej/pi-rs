//! Port of `diff/libesm/types.d.ts` (diff@8.0.4).

// PORT: option interfaces become flat structs (inherited fields are repeated). Numeric options are
// `Option<f64>` (JS numbers) so `NaN` / `Infinity` behave as in JS. The `callback` (async mode)
// options are not ported.

use std::sync::Arc;

use serde::Serialize;

// PORT: fields are in the key order of the objects jsdiff builds, so serializing reproduces
// `JSON.stringify` output.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeObject<V> {
    /// How many tokens (e.g. chars for `diffChars`, lines for `diffLines`) the value in the change object consists of
    pub count: usize,
    /// true if the value was inserted into the new string, otherwise false
    pub added: bool,
    /// true if the value was removed from the old string, otherwise false
    pub removed: bool,
    /// The concatenated content of all the tokens represented by this change object - i.e. generally the text that is either added, deleted, or common, as a single string.
    /// In cases where tokens are considered common but are non-identical (e.g. because an option like `ignoreCase` or a custom `comparator` was used), the value from the *new* string will be provided here.
    pub value: V,
}

pub type Change = ChangeObject<String>;

pub type ArrayChange<T> = ChangeObject<Vec<T>>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommonDiffOptions {
    /// If `true`, the array of change objects returned will contain one change object per token (e.g. one per line if calling `diffLines`), instead of runs of consecutive tokens that are all added / all removed / all conserved being combined into a single change object.
    pub one_change_per_token: Option<bool>,
}

// PORT: `TimeoutOption | MaxEditLengthOption` as one struct with both fields optional. It is passed
// to the `*_abortable` variants, which return `None` where jsdiff returns `undefined`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AbortableDiffOptions {
    /// A number specifying the maximum edit distance to consider between the old and new texts.
    /// You can use this to limit the computational cost of diffing large, very different texts by giving up early if the cost will be huge.
    /// This option can be passed either to diffing functions (`diffLines`, `diffChars`, etc) or to patch-creation function (`structuredPatch`, `createPatch`, etc), all of which will indicate that the max edit length was reached by returning `undefined` instead of whatever they'd normally return.
    pub max_edit_length: Option<f64>,
    /// A number of milliseconds after which the diffing algorithm will abort and return `undefined`.
    /// Supported by the same functions as `maxEditLength`.
    pub timeout: Option<f64>,
}

pub type TimeoutOption = AbortableDiffOptions;

pub type MaxEditLengthOption = AbortableDiffOptions;

pub type Comparator<T> = Arc<dyn Fn(&T, &T) -> bool + Send + Sync>;

pub struct DiffArraysOptions<T> {
    /// If `true`, the array of change objects returned will contain one change object per token (e.g. one per line if calling `diffLines`), instead of runs of consecutive tokens that are all added / all removed / all conserved being combined into a single change object.
    pub one_change_per_token: Option<bool>,
    pub comparator: Option<Comparator<T>>,
}

impl<T> Default for DiffArraysOptions<T> {
    fn default() -> Self {
        Self {
            one_change_per_token: None,
            comparator: None,
        }
    }
}

impl<T> Clone for DiffArraysOptions<T> {
    fn clone(&self) -> Self {
        Self {
            one_change_per_token: self.one_change_per_token,
            comparator: self.comparator.clone(),
        }
    }
}

pub type DiffArraysOptionsNonabortable<T> = DiffArraysOptions<T>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffCharsOptions {
    /// If `true`, the array of change objects returned will contain one change object per token (e.g. one per line if calling `diffLines`), instead of runs of consecutive tokens that are all added / all removed / all conserved being combined into a single change object.
    pub one_change_per_token: Option<bool>,
    /// If `true`, the uppercase and lowercase forms of a character are considered equal.
    /// @default false
    pub ignore_case: Option<bool>,
}

pub type DiffCharsOptionsNonabortable = DiffCharsOptions;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffLinesOptions {
    /// If `true`, the array of change objects returned will contain one change object per token (e.g. one per line if calling `diffLines`), instead of runs of consecutive tokens that are all added / all removed / all conserved being combined into a single change object.
    pub one_change_per_token: Option<bool>,
    /// `true` to remove all trailing CR (`\r`) characters before performing the diff.
    /// This helps to get a useful diff when diffing UNIX text files against Windows text files.
    /// @default false
    pub strip_trailing_cr: Option<bool>,
    /// `true` to treat the newline character at the end of each line as its own token.
    /// This allows for changes to the newline structure to occur independently of the line content and to be treated as such.
    /// In general this is the more human friendly form of `diffLines`; the default behavior with this option turned off is better suited for patches and other computer friendly output.
    ///
    /// Note that while using `ignoreWhitespace` in combination with `newlineIsToken` is not an error, results may not be as expected.
    /// With `ignoreWhitespace: true` and `newlineIsToken: false`, changing a completely empty line to contain some spaces is treated as a non-change, but with `ignoreWhitespace: true` and `newlineIsToken: true`, it is treated as an insertion.
    /// This is because the content of a completely blank line is not a token at all in `newlineIsToken` mode.
    ///
    /// @default false
    pub newline_is_token: Option<bool>,
    /// `true` to ignore a missing newline character at the end of the last line when comparing it to other lines.
    /// (By default, the line `'b\n'` in text `'a\nb\nc'` is not considered equal to the line `'b'` in text `'a\nb'`; this option makes them be considered equal.)
    /// Ignored if `ignoreWhitespace` or `newlineIsToken` are also true.
    /// @default false
    pub ignore_newline_at_eof: Option<bool>,
    /// `true` to ignore leading and trailing whitespace characters when checking if two lines are equal.
    /// @default false
    pub ignore_whitespace: Option<bool>,
}

pub type DiffLinesOptionsNonabortable = DiffLinesOptions;

// PORT: stands in for an `Intl.Segmenter` with `granularity: 'word'`; returns the `segment` strings
// of `segmenter.segment(text)`, in order.
pub type WordSegmenter = Arc<dyn Fn(&str) -> Vec<String> + Send + Sync>;

#[derive(Clone, Default)]
pub struct DiffWordsOptions {
    /// If `true`, the array of change objects returned will contain one change object per token (e.g. one per line if calling `diffLines`), instead of runs of consecutive tokens that are all added / all removed / all conserved being combined into a single change object.
    pub one_change_per_token: Option<bool>,
    /// Same as in `diffChars`.
    /// @default false
    pub ignore_case: Option<bool>,
    /// An optional [`Intl.Segmenter`](https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/Intl/Segmenter) object (which must have a `granularity` of `'word'`) for `diffWords` to use to split the text into words.
    ///
    /// Note that this is (deliberately) incorrectly typed as `any` to avoid users whose `lib` & `target` settings in tsconfig.json are older than es2022 getting type errors when they build about `Intl.Segmenter` not existing.
    /// This is kind of ugly, since it makes the type declarations worse for users who genuinely use this feature, but seemed worth it to avoid the majority of the library's users (who probably do not use this particular option) getting confusing errors and being forced to change their `lib` to es2022 (even if their own code doesn't use any es2022 functions).
    ///
    /// By default, `diffWords` does not use an `Intl.Segmenter`, just some regexes for splitting text into words. This will tend to give worse results than `Intl.Segmenter` would, but ensures the results are consistent across environments; `Intl.Segmenter` behaviour is only loosely specced and the implementations in browsers could in principle change dramatically in future. If you want to use `diffWords` with an `Intl.Segmenter` but ensure it behaves the same whatever environment you run it in, use an `Intl.Segmenter` polyfill instead of the JavaScript engine's native `Intl.Segmenter` implementation.
    ///
    /// Using an `Intl.Segmenter` should allow better word-level diffing of non-English text than the default behaviour. For instance, `Intl.Segmenter`s can generally identify via built-in dictionaries which sequences of adjacent Chinese characters form words, allowing word-level diffing of Chinese. By specifying a language when instantiating the segmenter (e.g. `new Intl.Segmenter('sv', {granularity: 'word'})`) you can also support language-specific rules, like treating Swedish's colon separated contractions (like *k:a* for *kyrka*) as single words; by default this would be seen as two words separated by a colon.
    // PORT: the segmenter is a callback returning word-granularity segments (see `WordSegmenter`);
    // the JS `resolvedOptions().granularity` check has no equivalent.
    pub intl_segmenter: Option<WordSegmenter>,
    // PORT: not in the TS interface; `diffWords` reads `options.ignoreWhitespace` and delegates to
    // `diffWordsWithSpace` when it is `false`.
    pub ignore_whitespace: Option<bool>,
}

impl std::fmt::Debug for DiffWordsOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiffWordsOptions")
            .field("one_change_per_token", &self.one_change_per_token)
            .field("ignore_case", &self.ignore_case)
            .field("intl_segmenter", &self.intl_segmenter.as_ref().map(|_| "<segmenter>"))
            .field("ignore_whitespace", &self.ignore_whitespace)
            .finish()
    }
}

pub type DiffWordsOptionsNonabortable = DiffWordsOptions;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffSentencesOptions {
    /// If `true`, the array of change objects returned will contain one change object per token (e.g. one per line if calling `diffLines`), instead of runs of consecutive tokens that are all added / all removed / all conserved being combined into a single change object.
    pub one_change_per_token: Option<bool>,
}

pub type DiffSentencesOptionsNonabortable = DiffSentencesOptions;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffCssOptions {
    /// If `true`, the array of change objects returned will contain one change object per token (e.g. one per line if calling `diffLines`), instead of runs of consecutive tokens that are all added / all removed / all conserved being combined into a single change object.
    pub one_change_per_token: Option<bool>,
}

pub type DiffCssOptionsNonabortable = DiffCssOptions;

// PORT: `oldFileName` / `newFileName` are `Option` because `parsePatch` leaves them `undefined` for a
// patch without `---` / `+++` headers; `formatPatch` prints such a name as `undefined`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StructuredPatch {
    pub old_file_name: Option<String>,
    pub new_file_name: Option<String>,
    pub old_header: Option<String>,
    pub new_header: Option<String>,
    pub hunks: Vec<StructuredPatchHunk>,
    pub index: Option<String>,
}

// PORT: line numbers are JS numbers (`f64`): `parsePatch` yields `NaN` for an unparseable `@@`
// header, and fractional `context` values propagate into the counts.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuredPatchHunk {
    pub old_start: f64,
    pub old_lines: f64,
    pub new_start: f64,
    pub new_lines: f64,
    pub lines: Vec<String>,
}

pub(crate) fn truthy(v: Option<bool>) -> bool {
    v.unwrap_or(false)
}
