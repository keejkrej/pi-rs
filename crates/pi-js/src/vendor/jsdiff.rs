//! Port of npm `diff` 8.0.4 (node_modules/diff); vendored per PORTING.md §2.6.
//!
//! Text diff implementation.
//!
//! This library supports the following APIs:
//! Diff.diffChars: Character by character diff
//! Diff.diffWords: Word (as defined by \b regex) diff which ignores whitespace
//! Diff.diffLines: Line based diff
//!
//! Diff.diffCss: Diff targeted at CSS content
//!
//! These methods are based on the implementation proposed in
//! "An O(ND) Difference Algorithm and its Variations" (Myers, 1986).
//! http://citeseerx.ist.psu.edu/viewdoc/summary?doi=10.1.1.4.6927

// PORT: `diffJson`, `jsonDiff` and `canonicalize` (diff/json.js) are not ported; pi never calls them.

pub mod convert;
pub mod diff;
pub mod patch;
pub mod types;
pub mod util;

pub use convert::dmp::convert_changes_to_dmp;
pub use convert::xml::convert_changes_to_xml;
pub use diff::array::{ArrayDiff, diff_arrays, diff_arrays_abortable};
pub use diff::base::{Diff, RawComponent};
pub use diff::character::{CHARACTER_DIFF, CharacterDiff, diff_chars, diff_chars_abortable};
pub use diff::css::{CSS_DIFF, CssDiff, diff_css, diff_css_abortable};
pub use diff::line::{
    LINE_DIFF, LineDiff, diff_lines, diff_lines_abortable, diff_trimmed_lines, diff_trimmed_lines_abortable,
};
pub use diff::sentence::{SENTENCE_DIFF, SentenceDiff, diff_sentences, diff_sentences_abortable};
pub use diff::word::{
    WORD_DIFF, WORDS_WITH_SPACE_DIFF, WordDiff, WordsWithSpaceDiff, diff_words, diff_words_abortable,
    diff_words_with_space, diff_words_with_space_abortable,
};
pub use patch::apply::{
    ApplyPatchOptions, ApplyPatchesOptions, CompareLine, PatchInput, PatchesInput, apply_patch, apply_patches,
};
pub use patch::create::{
    CreatePatchOptions, CreatePatchOptionsNonabortable, FILE_HEADERS_ONLY, HeaderOptions, INCLUDE_HEADERS,
    OMIT_HEADERS, StructuredPatchOptions, StructuredPatchOptionsNonabortable, create_patch, create_patch_abortable,
    create_two_files_patch, create_two_files_patch_abortable, format_patch, format_patch_array, structured_patch,
    structured_patch_abortable,
};
pub use patch::parse::parse_patch;
pub use patch::reverse::{reverse_patch, reverse_patch_array};
pub use types::{
    AbortableDiffOptions, ArrayChange, Change, ChangeObject, CommonDiffOptions, Comparator, DiffArraysOptions,
    DiffArraysOptionsNonabortable, DiffCharsOptions, DiffCharsOptionsNonabortable, DiffCssOptions,
    DiffCssOptionsNonabortable, DiffLinesOptions, DiffLinesOptionsNonabortable, DiffSentencesOptions,
    DiffSentencesOptionsNonabortable, DiffWordsOptions, DiffWordsOptionsNonabortable, MaxEditLengthOption,
    StructuredPatch, StructuredPatchHunk, TimeoutOption, WordSegmenter,
};
