//! Port of packages/tui/src/utils.ts

#![allow(dead_code, non_upper_case_globals, unused_variables)]

use std::sync::{Arc, LazyLock, Mutex};

use indexmap::IndexMap;

// segmenters (shared instance)

/// One `Intl.SegmentData` value.
///
/// Field order is `segment`, `index`, `input`, `isWordLike`.
/// `index` is a UTF-16 code-unit offset. `is_word_like` is `None` for graphemes
/// (`undefined`) and `Some` for word granularity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentData {
    pub segment: String,
    pub index: i64,
    pub input: String,
    pub is_word_like: Option<bool>,
}

/// Shared `Intl.Segmenter` stand-in.
///
/// PORT: there is no `Intl.Segmenter` object. This tag selects grapheme or word
/// granularity; the ICU segmenters live in `pi_js::intl`. `segment` returns owned
/// [`SegmentData`] so paste-marker merges and the default segmenter share one type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segmenter {
    word: bool,
}

const GRAPHEME_SEGMENTER: Segmenter = Segmenter { word: false };
const WORD_SEGMENTER: Segmenter = Segmenter { word: true };

impl Segmenter {
    /// `Intl.Segmenter.prototype.segment`.
    pub fn segment(&self, text: &str) -> Vec<SegmentData> {
        todo!("port: Segmenter::segment")
    }
}

/// Get the shared grapheme segmenter instance.
pub fn get_grapheme_segmenter() -> &'static Segmenter {
    &GRAPHEME_SEGMENTER
}

/// Get the shared word segmenter instance.
pub fn get_word_segmenter() -> &'static Segmenter {
    &WORD_SEGMENTER
}

/// `/^(?:\p{Default_Ignorable_Code_Point}|\p{Control}|\p{Mark}|\p{Surrogate})+$/v`
///
/// PORT: `\p{Surrogate}` is omitted. A Rust `&str` cannot contain UTF-16 surrogates,
/// and `regex` has no Surrogate category.
static ZERO_WIDTH_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^(?:\p{Default_Ignorable_Code_Point}|\p{Control}|\p{Mark})+$").expect("zeroWidthRegex")
});

/// `/^[\p{Default_Ignorable_Code_Point}\p{Control}\p{Format}\p{Mark}\p{Surrogate}]+/v`
///
/// PORT: `\p{Surrogate}` is omitted (see [`ZERO_WIDTH_REGEX`]).
static LEADING_NON_PRINTING_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^[\p{Default_Ignorable_Code_Point}\p{Control}\p{Format}\p{Mark}]+")
        .expect("leadingNonPrintingRegex")
});

/// `/^(?:\p{Default_Ignorable_Code_Point}|\p{Control}|\p{Format}|\p{Mark}|\p{Surrogate})$/v`
///
/// PORT: `\p{Surrogate}` is omitted (see [`ZERO_WIDTH_REGEX`]).
static NON_PRINTING_CHAR_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^(?:\p{Default_Ignorable_Code_Point}|\p{Control}|\p{Format}|\p{Mark})$")
        .expect("nonPrintingCharRegex")
});

static MARK_CHAR_REGEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^\p{Mark}$").expect("markCharRegex"));

// Marks that terminals allocate cells for when attached to a base character.
// This includes Unicode spacing marks and non-spacing exceptions in legacy wcwidth tables.
static TERMINAL_SPACING_MARK_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"^(?:[\p{Spacing_Mark}--[\u{1734}\u{302E}\u{302F}]]|[\u{065F}\u{0F7F}\u{102B}\u{102C}\u{1031}\u{1033}-\u{1035}\u{1038}\u{103A}-\u{103E}])+$",
    )
    .expect("terminalSpacingMarkRegex")
});

/// `/^\p{RGI_Emoji}$/v`
///
/// PORT: `\p{RGI_Emoji}` is an ES2024 string property. `regex` / `fancy_regex` cannot
/// compile it. The body of [`grapheme_width`] compiles this with `pi_js::regex::ecma(..., "v")`.
const RGI_EMOJI_PATTERN: &str = r"^\p{RGI_Emoji}$";

// Cache for non-ASCII strings
const WIDTH_CACHE_SIZE: usize = 512;
// PORT: JS `Map` insertion order, so the oldest key is the first one deleted.
static WIDTH_CACHE: LazyLock<Mutex<IndexMap<String, i64>>> = LazyLock::new(|| Mutex::new(IndexMap::new()));

const CJK_BREAK_PATTERN: &str = r"[\p{Script_Extensions=Han}\p{Script_Extensions=Hiragana}\p{Script_Extensions=Katakana}\p{Script_Extensions=Hangul}\p{Script_Extensions=Bopomofo}]";

const CJK_PUNCT_LITERALS: &str = "[，．：；！？（）［］｛｝“”‘’…—]";

pub static cjk_break_regex: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(CJK_BREAK_PATTERN).expect("cjkBreakRegex"));

pub static cjk_punctuation_regex: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    let pattern = format!("(?:(?=\\p{{Punctuation}}){CJK_BREAK_PATTERN}|{CJK_PUNCT_LITERALS})");
    fancy_regex::Regex::new(&pattern).expect("cjkPunctuationRegex")
});

/// PORT: JS `\s` is `pi_js::regex::JS_WS_CLASS` (§13.2). `as_str()` (via `LazyLock`'s deref)
/// is that translated pattern, which is what `autocomplete.ts` / `editor.ts` embed from `.source`.
pub static autocomplete_separator_regex: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    let cjk = cjk_punctuation_regex.as_str();
    let pattern = format!("(?:{}|{cjk})", pi_js::regex::JS_WS_CLASS);
    fancy_regex::Regex::new(&pattern).expect("autocompleteSeparatorRegex")
});

pub static autocomplete_boundary_regex: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    let separator = autocomplete_separator_regex.as_str();
    let pattern = format!("(?:^|{separator})");
    fancy_regex::Regex::new(&pattern).expect("autocompleteBoundaryRegex")
});

/// Check if a grapheme cluster (after segmentation) could possibly be an RGI emoji.
/// This is a fast heuristic to avoid the expensive rgiEmojiRegex test.
/// The tested Unicode blocks are deliberately broad to account for future
/// Unicode additions.
fn could_be_emoji(segment: &str) -> bool {
    todo!("port: could_be_emoji")
}

fn is_printable_ascii(s: &str) -> bool {
    todo!("port: is_printable_ascii")
}

struct FragmentWidth {
    text: String,
    width: i64,
}

fn truncate_fragment_to_width(text: &str, max_width: i64) -> FragmentWidth {
    todo!("port: truncate_fragment_to_width")
}

fn finalize_truncated_result(
    prefix: &str,
    prefix_width: i64,
    ellipsis: &str,
    ellipsis_width: i64,
    max_width: i64,
    pad: bool,
) -> String {
    todo!("port: finalize_truncated_result")
}

/// Calculate the terminal width of a single grapheme cluster.
/// Based on code from the string-width library, but includes a possible-emoji
/// check to avoid running the RGI_Emoji regex unnecessarily.
fn grapheme_width(segment: &str) -> i64 {
    todo!("port: grapheme_width")
}

/// Calculate the visible width of a string in terminal columns.
pub fn visible_width(str: &str) -> i64 {
    todo!("port: visible_width")
}

/// Remove ANSI, OSC, and APC control sequences while preserving visible text.
pub fn strip_terminal_sequences(str: &str) -> String {
    todo!("port: strip_terminal_sequences")
}

/// `{ start, end }` from [`get_grapheme_cell_range`]. Not exported from the TS module;
/// public here because the function returns it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphemeCellRange {
    pub start: i64,
    pub end: i64,
}

/// Return the terminal-cell range occupied by the grapheme at a visible column.
pub fn get_grapheme_cell_range(line: &str, column: i64) -> Option<GraphemeCellRange> {
    todo!("port: get_grapheme_cell_range")
}

/// Return the OSC 8 hyperlink covering a visible terminal column.
pub fn get_osc8_link_at_column(line: &str, column: i64) -> Option<String> {
    todo!("port: get_osc8_link_at_column")
}

/// Normalize text for terminal output without changing logical editor content.
/// Some terminals render precomposed Thai/Lao AM vowels inconsistently during
/// differential repaint. Their compatibility decompositions have the same cell
/// width but avoid stale-cell artifacts in terminal renderers. Visible tabs are
/// expanded to the fixed width used by layout so terminal tab stops cannot wrap
/// a logical line, while tabs inside terminal string sequences stay untouched.
const THAI_LAO_AM_REGEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"[\u{0e33}\u{0eb3}]").expect("THAI_LAO_AM_REGEX"));
const THAI_LAO_AM_GLOBAL_REGEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"[\u{0e33}\u{0eb3}]").expect("THAI_LAO_AM_GLOBAL_REGEX"));

pub fn normalize_terminal_output(str: &str) -> String {
    todo!("port: normalize_terminal_output")
}

/// `{ code, length }` from [`extract_ansi_code`]. `None` is TS `null`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractedAnsiCode {
    pub code: String,
    pub length: i64,
}

/// Extract ANSI escape sequences from a string at the given position.
pub fn extract_ansi_code(str: &str, pos: i64) -> Option<ExtractedAnsiCode> {
    todo!("port: extract_ansi_code")
}

/// Width of a string made of printable ASCII, tabs, and ANSI escape sequences, or -1 if it contains
/// anything else. Matches `visibleWidth` for those strings without allocating.
fn ascii_visible_width(str: &str) -> i64 {
    todo!("port: ascii_visible_width")
}

/// Length of the ANSI/OSC/APC escape sequence starting at `pos`, or 0 if there is none.
fn ansi_code_length(str: &str, pos: i64) -> i64 {
    todo!("port: ansi_code_length")
}

/// `"\x07" | "\x1b\\"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Osc8Terminator {
    /// `"\x07"`
    Bel,
    /// `"\x1b\\"`
    St,
}

struct ActiveHyperlink {
    params: String,
    url: String,
    terminator: Osc8Terminator,
}

/// `ActiveHyperlink | null | undefined`.
/// `None` = not an OSC 8 sequence, `Some(None)` = close (empty url), `Some(Some(_))` = open.
fn parse_osc8_hyperlink(ansi_code: &str) -> Option<Option<ActiveHyperlink>> {
    todo!("port: parse_osc8_hyperlink")
}

fn format_osc8_hyperlink(hyperlink: &ActiveHyperlink) -> String {
    todo!("port: format_osc8_hyperlink")
}

fn format_osc8_close(terminator: Osc8Terminator) -> String {
    todo!("port: format_osc8_close")
}

fn get_active_osc8_close(prefix: &str) -> String {
    todo!("port: get_active_osc8_close")
}

struct AnsiCodeTrackerState {
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    blink: bool,
    inverse: bool,
    hidden: bool,
    strikethrough: bool,
    /// Stores the full code like "31" or "38;5;240". `None` is `null`.
    fg_color: Option<String>,
    /// Stores the full code like "41" or "48;5;240". `None` is `null`.
    bg_color: Option<String>,
    active_hyperlink: Option<ActiveHyperlink>,
}

struct AnsiCodeTrackerInner {
    state: Mutex<AnsiCodeTrackerState>,
}

/// Track active ANSI SGR codes to preserve styling across line breaks.
///
/// PORT: TS class with identity.
#[derive(Clone)]
struct AnsiCodeTracker {
    inner: Arc<AnsiCodeTrackerInner>,
}

impl AnsiCodeTracker {
    fn new() -> Self {
        todo!("port: AnsiCodeTracker::new")
    }

    fn process(&self, ansi_code: &str) {
        todo!("port: AnsiCodeTracker::process")
    }

    fn reset(&self) {
        todo!("port: AnsiCodeTracker::reset")
    }

    /// Clear all state for reuse.
    fn clear(&self) {
        todo!("port: AnsiCodeTracker::clear")
    }

    fn get_active_codes(&self) -> String {
        todo!("port: AnsiCodeTracker::get_active_codes")
    }

    fn get_active_background_code(&self) -> String {
        todo!("port: AnsiCodeTracker::get_active_background_code")
    }

    fn has_active_codes(&self) -> bool {
        todo!("port: AnsiCodeTracker::has_active_codes")
    }

    /// Get reset codes for attributes that need to be turned off at line end.
    /// Underline must be closed to prevent bleeding into padding.
    /// Active OSC 8 hyperlinks must be closed and re-opened on the next line.
    /// Returns empty string if no attributes need closing.
    fn get_line_end_reset(&self) -> String {
        todo!("port: AnsiCodeTracker::get_line_end_reset")
    }
}

fn update_tracker_from_text(text: &str, tracker: &AnsiCodeTracker) {
    todo!("port: update_tracker_from_text")
}

/// Return only the background color active at the end of an ANSI-styled string.
pub fn get_active_background_ansi(text: &str) -> String {
    todo!("port: get_active_background_ansi")
}

fn grapheme_segments(text: &str) -> Vec<String> {
    todo!("port: grapheme_segments")
}

/// Split text into words while keeping ANSI codes attached.
fn split_into_tokens_with_ansi(text: &str) -> Vec<String> {
    todo!("port: split_into_tokens_with_ansi")
}

/// Flatten cached lines. V8 keeps a string built by concatenation as a tree of its parts until something reads it
/// whole, and a cached line kept as such a tree retains several times its own size. Converting a string to a number
/// reads it whole, so V8 flattens it in place; the strings' values do not change.
///
/// PORT: Rust strings are already flat. The function stays so callers match TS; it does not change values.
pub fn flatten_lines(lines: &[String]) {
    todo!("port: flatten_lines")
}

/// Wrap text with ANSI codes preserved.
///
/// ONLY does word wrapping - NO padding, NO background colors.
/// Returns lines where each line is <= width visible chars.
/// Active ANSI codes are preserved across line breaks.
///
/// @param text - Text to wrap (may contain ANSI codes and newlines)
/// @param width - Maximum visible width per line
/// @returns Array of wrapped lines (NOT padded to width)
pub fn wrap_text_with_ansi(text: &str, width: i64) -> Vec<String> {
    todo!("port: wrap_text_with_ansi")
}

fn wrap_single_line(line: &str, width: i64) -> Vec<String> {
    todo!("port: wrap_single_line")
}

pub static PUNCTUATION_REGEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r#"[(){}[\]<>.,;:'"!?+=*/\\|&%^$#@~`-]"#).expect("PUNCTUATION_REGEX"));

/// Check if a character is whitespace.
pub fn is_whitespace_char(char: &str) -> bool {
    todo!("port: is_whitespace_char")
}

/// Check if a character is punctuation.
pub fn is_punctuation_char(char: &str) -> bool {
    todo!("port: is_punctuation_char")
}

fn break_long_word(word: &str, width: i64, tracker: &AnsiCodeTracker) -> Vec<String> {
    todo!("port: break_long_word")
}

/// Apply background color to a line, padding to full width.
///
/// @param line - Line of text (may contain ANSI codes)
/// @param width - Total width to pad to
/// @param bgFn - Background color function
/// @returns Line with background applied and padded to width
pub fn apply_background_to_line(line: &str, width: i64, bg_fn: Arc<dyn Fn(&str) -> String + Send + Sync>) -> String {
    todo!("port: apply_background_to_line")
}

/// Truncate text to fit within a maximum visible width, adding ellipsis if needed.
/// Optionally pad with spaces to reach exactly maxWidth.
/// Properly handles ANSI escape codes (they don't count toward width).
///
/// @param text - Text to truncate (may contain ANSI codes)
/// @param maxWidth - Maximum visible width
/// @param ellipsis - Ellipsis string to append when truncating (default: "...")
/// @param pad - If true, pad result with spaces to exactly maxWidth (default: false)
/// @returns Truncated text, optionally padded to exactly maxWidth
///
/// `ellipsis` `None` is `"..."`. `pad` `None` is false. Defaults are applied inside.
pub fn truncate_to_width(text: &str, max_width: i64, ellipsis: Option<&str>, pad: Option<bool>) -> String {
    todo!("port: truncate_to_width")
}

/// Extract a range of visible columns from a line. Handles ANSI codes and wide chars.
/// @param strict - If true, exclude wide chars at boundary that would extend past the range
///
/// `strict` `None` is false.
pub fn slice_by_column(line: &str, start_col: i64, length: i64, strict: Option<bool>) -> String {
    slice_with_width(line, start_col, length, strict).text
}

/// `{ text, width }` from [`slice_with_width`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SliceWithWidth {
    pub text: String,
    pub width: i64,
}

/// Like sliceByColumn but also returns the actual visible width of the result.
///
/// `strict` `None` is false.
pub fn slice_with_width(line: &str, start_col: i64, length: i64, strict: Option<bool>) -> SliceWithWidth {
    todo!("port: slice_with_width")
}

// Pooled tracker instance for extractSegments (avoids allocation per call)
static POOLED_STYLE_TRACKER: LazyLock<AnsiCodeTracker> = LazyLock::new(AnsiCodeTracker::new);

/// `{ before, beforeWidth, after, afterWidth }`. Field order matches the return literal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractedSegments {
    pub before: String,
    pub before_width: i64,
    pub after: String,
    pub after_width: i64,
}

/// Extract "before" and "after" segments from a line in a single pass.
/// Used for overlay compositing where we need content before and after the overlay region.
/// Preserves styling from before the overlay that should affect content after it.
///
/// `strict_after` `None` is false.
pub fn extract_segments(
    line: &str,
    before_end: i64,
    after_start: i64,
    after_len: i64,
    strict_after: Option<bool>,
) -> ExtractedSegments {
    todo!("port: extract_segments")
}
