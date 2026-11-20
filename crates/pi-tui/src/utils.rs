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
        let segs = if self.word {
            pi_js::intl::words(text)
        } else {
            pi_js::intl::graphemes(text)
        };
        segs.into_iter()
            .map(|s| SegmentData {
                segment: s.segment.to_owned(),
                index: s.index as i64,
                input: text.to_owned(),
                is_word_like: s.is_word_like,
            })
            .collect()
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
static RGI_EMOJI_TEST: LazyLock<Box<dyn Fn(&str) -> bool + Send + Sync>> = LazyLock::new(|| {
    let re = pi_js::regex::ecma(RGI_EMOJI_PATTERN, "v").expect("rgiEmojiRegex");
    Box::new(move |segment: &str| pi_js::regex::test(&re, segment))
});

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
    let Some(ch) = segment.chars().next() else {
        return false;
    };
    let cp = ch as u32;
    (0x1f000..=0x1fbff).contains(&cp)
        || (0x2300..=0x23ff).contains(&cp)
        || (0x2600..=0x27bf).contains(&cp)
        || (0x2b50..=0x2b55).contains(&cp)
        || segment.contains('\u{fe0f}')
        || segment.encode_utf16().take(3).count() > 2
}

fn is_printable_ascii(s: &str) -> bool {
    s.bytes().all(|b| (0x20..=0x7e).contains(&b))
}

struct FragmentWidth {
    text: String,
    width: i64,
}

fn find_text_run_end(bytes: &[u8], start: usize, stop_at_tab: bool) -> usize {
    let mut end = start;
    while end < bytes.len() {
        let b = bytes[end];
        if stop_at_tab && b == b'\t' {
            break;
        }
        if b == 0x1b && ansi_code_length_bytes(bytes, end) > 0 {
            break;
        }
        end += 1;
    }
    end
}

fn truncate_fragment_to_width(text: &str, max_width: i64) -> FragmentWidth {
    if max_width <= 0 || text.is_empty() {
        return FragmentWidth {
            text: String::new(),
            width: 0,
        };
    }

    if is_printable_ascii(text) {
        let take = (max_width as usize).min(text.len());
        let clipped = &text[..take];
        return FragmentWidth {
            text: clipped.to_owned(),
            width: clipped.len() as i64,
        };
    }

    let has_ansi = text.contains('\x1b');
    let has_tabs = text.contains('\t');
    if !has_ansi && !has_tabs {
        let mut result = String::new();
        let mut width = 0;
        for seg in pi_js::intl::graphemes(text) {
            let w = grapheme_width(seg.segment);
            if width + w > max_width {
                break;
            }
            result.push_str(seg.segment);
            width += w;
        }
        return FragmentWidth { text: result, width };
    }

    let bytes = text.as_bytes();
    let mut result = String::new();
    let mut width = 0;
    let mut i = 0;
    let mut pending_ansi = String::new();

    while i < bytes.len() {
        let ansi_len = ansi_code_length_bytes(bytes, i);
        if ansi_len > 0 {
            pending_ansi.push_str(&text[i..i + ansi_len]);
            i += ansi_len;
            continue;
        }

        if bytes[i] == b'\t' {
            if width + 3 > max_width {
                break;
            }
            if !pending_ansi.is_empty() {
                result.push_str(&pending_ansi);
                pending_ansi.clear();
            }
            result.push('\t');
            width += 3;
            i += 1;
            continue;
        }

        let end = find_text_run_end(bytes, i, true);
        for seg in pi_js::intl::graphemes(&text[i..end]) {
            let w = grapheme_width(seg.segment);
            if width + w > max_width {
                return FragmentWidth { text: result, width };
            }
            if !pending_ansi.is_empty() {
                result.push_str(&pending_ansi);
                pending_ansi.clear();
            }
            result.push_str(seg.segment);
            width += w;
        }
        i = end;
    }

    FragmentWidth { text: result, width }
}

fn finalize_truncated_result(
    prefix: &str,
    prefix_width: i64,
    ellipsis: &str,
    ellipsis_width: i64,
    max_width: i64,
    pad: bool,
) -> String {
    let reset = "\x1b[0m";
    let hyperlink_close = get_active_osc8_close(prefix);
    let vis_width = prefix_width + ellipsis_width;
    let mut result = if !ellipsis.is_empty() {
        format!("{prefix}{hyperlink_close}{reset}{ellipsis}{reset}")
    } else {
        format!("{prefix}{hyperlink_close}{reset}")
    };

    if pad {
        let pad_len = (max_width - vis_width).max(0) as usize;
        if pad_len > 0 {
            result.extend(std::iter::repeat_n(' ', pad_len));
        }
    }
    result
}

/// Calculate the terminal width of a single grapheme cluster.
/// Based on code from the string-width library, but includes a possible-emoji
/// check to avoid running the RGI_Emoji regex unnecessarily.
fn grapheme_width(segment: &str) -> i64 {
    if segment.len() == 1 {
        let b = segment.as_bytes()[0];
        if (0x20..=0x7e).contains(&b) {
            return 1;
        }
        if b == 0x09 {
            return 3;
        }
    }

    // Some marks occupy cells even without a base character.
    if TERMINAL_SPACING_MARK_REGEX.is_match(segment) {
        return segment.chars().count() as i64;
    }

    // Zero-width clusters
    if ZERO_WIDTH_REGEX.is_match(segment) {
        return 0;
    }

    // Emoji check with pre-filter
    if could_be_emoji(segment) && RGI_EMOJI_TEST(segment) {
        return 2;
    }

    // Get base visible codepoint
    let base = LEADING_NON_PRINTING_REGEX.replace(segment, "");
    let Some(first_ch) = base.chars().next() else {
        return 0;
    };
    let cp = first_ch as u32;

    // Regional indicator symbols (U+1F1E6..U+1F1FF) are often rendered as
    // full-width emoji in terminals, even when isolated during streaming.
    // Keep width conservative (2) to avoid terminal auto-wrap drift artifacts.
    if (0x1f1e6..=0x1f1ff).contains(&cp) {
        return 2;
    }

    let mut width = crate::vendor::east_asian_width::east_asian_width(i64::from(cp), None).unwrap_or(1);

    // Intl.Segmenter can group multiple terminal-spacing code points into one
    // grapheme. Count trailing visible code points that terminals may allocate
    // cells for: Indic consonants after marks, halfwidth/fullwidth forms, and
    // Thai/Lao AM vowels.
    let mut follows_mark = false;
    let mut chars = base.chars();
    chars.next();
    let mut char_buf = [0u8; 4];
    for ch in chars {
        let ch_str = ch.encode_utf8(&mut char_buf);
        if TERMINAL_SPACING_MARK_REGEX.is_match(ch_str) {
            width += 1;
            follows_mark = false;
        } else if MARK_CHAR_REGEX.is_match(ch_str) {
            follows_mark = true;
        } else if !NON_PRINTING_CHAR_REGEX.is_match(ch_str) {
            let c = ch as u32;
            if follows_mark || (0xff00..=0xffef).contains(&c) {
                width += crate::vendor::east_asian_width::east_asian_width(i64::from(c), None).unwrap_or(1);
            } else if c == 0x0e33 || c == 0x0eb3 {
                width += 1;
            }
            follows_mark = false;
        }
    }

    width
}

/// Calculate the visible width of a string in terminal columns.
pub fn visible_width(str: &str) -> i64 {
    if str.is_empty() {
        return 0;
    }

    // Fast path: printable ASCII, tabs, and ANSI escape sequences.
    let ascii_width = ascii_visible_width(str);
    if ascii_width != -1 {
        return ascii_width;
    }

    // Check cache
    if let Some(&cached) = WIDTH_CACHE.lock().unwrap().get(str) {
        return cached;
    }

    // Normalize: tabs to 3 spaces, strip ANSI escape codes
    let tab_expanded = if str.contains('\t') {
        str.replace('\t', "   ")
    } else {
        String::new()
    };
    let clean_source = if str.contains('\t') { tab_expanded.as_str() } else { str };

    let clean_owned;
    let clean = if clean_source.contains('\x1b') {
        clean_owned = strip_terminal_sequences(clean_source);
        clean_owned.as_str()
    } else {
        clean_source
    };

    // Calculate width
    let mut width = 0;
    for seg in pi_js::intl::graphemes(clean) {
        width += grapheme_width(seg.segment);
    }

    // Cache result
    let mut cache = WIDTH_CACHE.lock().unwrap();
    if cache.len() >= WIDTH_CACHE_SIZE && !cache.contains_key(str) {
        cache.shift_remove_index(0);
    }
    cache.insert(str.to_owned(), width);

    width
}

/// Remove ANSI, OSC, and APC control sequences while preserving visible text.
pub fn strip_terminal_sequences(str: &str) -> String {
    if !str.contains('\x1b') {
        return str.to_owned();
    }
    let bytes = str.as_bytes();
    let mut result = String::with_capacity(str.len());
    let mut copy_from = 0;
    let mut i = 0;
    while let Some(rel) = str[i..].find('\x1b') {
        let esc_pos = i + rel;
        let len = ansi_code_length_bytes(bytes, esc_pos);
        if len > 0 {
            result.push_str(&str[copy_from..esc_pos]);
            i = esc_pos + len;
            copy_from = i;
        } else {
            i = esc_pos + 1;
        }
    }
    result.push_str(&str[copy_from..]);
    result
}

fn extract_ansi_codes(str: &str) -> String {
    let bytes = str.as_bytes();
    let mut result = String::new();
    let mut i = 0;
    while let Some(rel) = str[i..].find('\x1b') {
        let esc_pos = i + rel;
        let len = ansi_code_length_bytes(bytes, esc_pos);
        if len > 0 {
            result.push_str(&str[esc_pos..esc_pos + len]);
            i = esc_pos + len;
        } else {
            i = esc_pos + 1;
        }
    }
    result
}

fn is_whitespace_token(token: &str) -> bool {
    let bytes = token.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x1b {
            let len = ansi_code_length_bytes(bytes, i);
            if len == 0 {
                return false;
            }
            i += len;
        } else if b == 0x20 || (0x09..=0x0d).contains(&b) {
            i += 1;
        } else if b < 0x80 {
            return false;
        } else {
            let Some(ch) = token[i..].chars().next() else {
                return false;
            };
            if !pi_js::str16::is_js_whitespace(ch) {
                return false;
            }
            i += ch.len_utf8();
        }
    }
    true
}

/// Replace each run of C0 and C1 control characters (`ESC`, `BEL`, newlines, ...) with one space.
pub fn replace_control_characters(str: &str) -> String {
    let mut out = String::with_capacity(str.len());
    let mut in_run = false;
    for ch in str.chars() {
        let is_ctrl = ch <= '\u{001f}' || ('\u{007f}'..='\u{009f}').contains(&ch);
        if is_ctrl {
            if !in_run {
                out.push(' ');
                in_run = true;
            }
        } else {
            in_run = false;
            out.push(ch);
        }
    }
    out
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
    let bytes = line.as_bytes();
    let mut current_col = 0;
    let mut i = 0;
    while i < bytes.len() {
        let ansi_len = ansi_code_length_bytes(bytes, i);
        if ansi_len > 0 {
            i += ansi_len;
            continue;
        }
        let text_end = find_text_run_end(bytes, i, false);
        for seg in pi_js::intl::graphemes(&line[i..text_end]) {
            let width = grapheme_width(seg.segment);
            if width > 0 && column >= current_col && column < current_col + width {
                return Some(GraphemeCellRange {
                    start: current_col,
                    end: current_col + width,
                });
            }
            current_col += width;
        }
        i = text_end;
    }
    None
}

/// Return the OSC 8 hyperlink covering a visible terminal column.
pub fn get_osc8_link_at_column(line: &str, column: i64) -> Option<String> {
    let bytes = line.as_bytes();
    let mut active_url: Option<String> = None;
    let mut current_col = 0;
    let mut i = 0;
    while i < bytes.len() {
        let ansi_len = ansi_code_length_bytes(bytes, i);
        if ansi_len > 0 {
            let code = &line[i..i + ansi_len];
            if let Some(hl_opt) = parse_osc8_hyperlink(code) {
                active_url = match hl_opt {
                    Some(hl) if !hl.url.contains('\x07') && !hl.url.contains('\x1b') => Some(hl.url),
                    None => None,
                    _ => active_url,
                };
            }
            i += ansi_len;
            continue;
        }
        let text_end = find_text_run_end(bytes, i, false);
        for seg in pi_js::intl::graphemes(&line[i..text_end]) {
            let width = if seg.segment == "\t" {
                3
            } else {
                grapheme_width(seg.segment)
            };
            if column >= current_col && column < current_col + width {
                return active_url;
            }
            current_col += width;
        }
        i = text_end;
    }
    None
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
    let normalized = if THAI_LAO_AM_REGEX.is_match(str) {
        let s = str
            .replace('\u{0e33}', "\u{0e4d}\u{0e32}")
            .replace('\u{0eb3}', "\u{0ecd}\u{0eb2}");
        std::borrow::Cow::Owned(s)
    } else {
        std::borrow::Cow::Borrowed(str)
    };
    if !normalized.contains('\t') {
        return normalized.into_owned();
    }

    let s = normalized.as_ref();
    let bytes = s.as_bytes();
    let mut result = String::with_capacity(s.len());
    let mut copy_from = 0;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\x1b' {
            let len = ansi_code_length_bytes(bytes, i);
            if len > 0 {
                i += len;
                continue;
            }
        } else if b == b'\t' {
            result.push_str(&s[copy_from..i]);
            result.push_str("   ");
            i += 1;
            copy_from = i;
            continue;
        }
        i += 1;
    }
    result.push_str(&s[copy_from..]);
    result
}

/// `{ code, length }` from [`extract_ansi_code`]. `None` is TS `null`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractedAnsiCode {
    pub code: String,
    pub length: i64,
}

fn utf16_to_byte_index(s: &str, u16_pos: i64) -> Option<usize> {
    if u16_pos < 0 {
        return None;
    }
    let target = u16_pos as usize;
    if s.is_ascii() {
        return (target <= s.len()).then_some(target);
    }
    let mut u16_idx = 0;
    for (byte_idx, ch) in s.char_indices() {
        if u16_idx == target {
            return Some(byte_idx);
        }
        u16_idx += ch.len_utf16();
        if u16_idx > target {
            return None;
        }
    }
    (u16_idx == target).then_some(s.len())
}

fn ansi_code_length_bytes(bytes: &[u8], pos: usize) -> usize {
    if pos >= bytes.len() || bytes[pos] != 0x1b {
        return 0;
    }

    let Some(&next) = bytes.get(pos + 1) else {
        return 0;
    };

    // CSI sequence: ESC [ ... m/G/K/H/J
    if next == b'[' {
        for j in (pos + 2)..bytes.len() {
            let c = bytes[j];
            if matches!(c, b'm' | b'G' | b'K' | b'H' | b'J') {
                return j + 1 - pos;
            }
        }
        return 0;
    }

    // OSC / APC sequence: ESC ] ... BEL/ST or ESC _ ... BEL/ST
    if next == b']' || next == b'_' {
        for j in (pos + 2)..bytes.len() {
            let c = bytes[j];
            if c == 0x07 {
                return j + 1 - pos;
            }
            if c == 0x1b && bytes.get(j + 1) == Some(&b'\\') {
                return j + 2 - pos;
            }
        }
        return 0;
    }

    0
}

/// Extract ANSI escape sequences from a string at the given position.
pub fn extract_ansi_code(str: &str, pos: i64) -> Option<ExtractedAnsiCode> {
    let byte_pos = utf16_to_byte_index(str, pos)?;
    let byte_len = ansi_code_length_bytes(str.as_bytes(), byte_pos);
    if byte_len == 0 {
        return None;
    }
    let code = &str[byte_pos..byte_pos + byte_len];
    let length = pi_js::str16::len(code) as i64;
    Some(ExtractedAnsiCode {
        code: code.to_owned(),
        length,
    })
}

/// Width of a string made of printable ASCII, tabs, and ANSI escape sequences, or -1 if it contains
/// anything else. Matches `visibleWidth` for those strings without allocating.
fn ascii_visible_width(str: &str) -> i64 {
    let bytes = str.as_bytes();
    let mut width: i64 = 0;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if (0x20..=0x7e).contains(&b) {
            width += 1;
            i += 1;
        } else if b == 0x09 {
            width += 3;
            i += 1;
        } else if b == 0x1b {
            let len = ansi_code_length_bytes(bytes, i);
            if len == 0 {
                return -1;
            }
            i += len;
        } else {
            return -1;
        }
    }
    width
}

/// Length of the ANSI/OSC/APC escape sequence starting at `pos`, or 0 if there is none.
fn ansi_code_length(str: &str, pos: i64) -> i64 {
    let Some(byte_pos) = utf16_to_byte_index(str, pos) else {
        return 0;
    };
    let byte_len = ansi_code_length_bytes(str.as_bytes(), byte_pos);
    if byte_len == 0 {
        0
    } else {
        pi_js::str16::len(&str[byte_pos..byte_pos + byte_len]) as i64
    }
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
    if !ansi_code.starts_with("\x1b]8;") {
        return None;
    }

    let terminator = if ansi_code.ends_with('\x07') {
        Osc8Terminator::Bel
    } else if ansi_code.ends_with("\x1b\\") {
        Osc8Terminator::St
    } else {
        return None;
    };

    let term_len = if terminator == Osc8Terminator::Bel { 1 } else { 2 };
    if ansi_code.len() < 4 + term_len {
        return None;
    }

    let body = &ansi_code[4..ansi_code.len() - term_len];
    let (params, url) = body.split_once(';')?;
    if url.is_empty() {
        return Some(None);
    }

    Some(Some(ActiveHyperlink {
        params: params.to_owned(),
        url: url.to_owned(),
        terminator,
    }))
}

fn format_osc8_hyperlink(hyperlink: &ActiveHyperlink) -> String {
    let term = if hyperlink.terminator == Osc8Terminator::Bel {
        "\x07"
    } else {
        "\x1b\\"
    };
    format!("\x1b]8;{};{}{term}", hyperlink.params, hyperlink.url)
}

fn format_osc8_close(terminator: Osc8Terminator) -> String {
    let term = if terminator == Osc8Terminator::Bel {
        "\x07"
    } else {
        "\x1b\\"
    };
    format!("\x1b]8;;{term}")
}

fn get_active_osc8_close(prefix: &str) -> String {
    if !prefix.contains("\x1b]8;") {
        return String::new();
    }

    let bytes = prefix.as_bytes();
    let mut active_terminator: Option<Osc8Terminator> = None;
    let mut i = 0;
    while let Some(rel) = prefix[i..].find('\x1b') {
        let esc_pos = i + rel;
        let len = ansi_code_length_bytes(bytes, esc_pos);
        if len > 0 {
            if let Some(hl_opt) = parse_osc8_hyperlink(&prefix[esc_pos..esc_pos + len]) {
                active_terminator = hl_opt.map(|hl| hl.terminator);
            }
            i = esc_pos + len;
        } else {
            i = esc_pos + 1;
        }
    }

    active_terminator.map(format_osc8_close).unwrap_or_default()
}

#[derive(Default)]
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

impl AnsiCodeTrackerState {
    fn reset(&mut self) {
        self.bold = false;
        self.dim = false;
        self.italic = false;
        self.underline = false;
        self.blink = false;
        self.inverse = false;
        self.hidden = false;
        self.strikethrough = false;
        self.fg_color = None;
        self.bg_color = None;
        // SGR reset does not affect OSC 8 hyperlink state
    }

    fn clear(&mut self) {
        self.reset();
        self.active_hyperlink = None;
    }

    fn process(&mut self, ansi_code: &str) {
        if let Some(hl_opt) = parse_osc8_hyperlink(ansi_code) {
            self.active_hyperlink = hl_opt;
            return;
        }

        if !ansi_code.ends_with('m') {
            return;
        }

        let Some(csi_pos) = ansi_code.find("\x1b[") else {
            return;
        };
        let rest = &ansi_code[csi_pos + 2..];
        let Some(m_pos) = rest.find('m') else {
            return;
        };
        let params = &rest[..m_pos];
        if !params.bytes().all(|b| b.is_ascii_digit() || b == b';') {
            return;
        }

        if params.is_empty() || params == "0" {
            self.reset();
            return;
        }

        let parts: Vec<&str> = params.split(';').collect();
        let mut i = 0;
        while i < parts.len() {
            let Ok(code) = parts[i].parse::<i64>() else {
                i += 1;
                continue;
            };

            if code == 38 || code == 48 {
                if parts.get(i + 1) == Some(&"5")
                    && let Some(&p2) = parts.get(i + 2)
                {
                    let color_code = format!("{};5;{}", parts[i], p2);
                    if code == 38 {
                        self.fg_color = Some(color_code);
                    } else {
                        self.bg_color = Some(color_code);
                    }
                    i += 3;
                    continue;
                } else if parts.get(i + 1) == Some(&"2")
                    && let (Some(&p2), Some(&p3), Some(&p4)) = (parts.get(i + 2), parts.get(i + 3), parts.get(i + 4))
                {
                    let color_code = format!("{};2;{p2};{p3};{p4}", parts[i]);
                    if code == 38 {
                        self.fg_color = Some(color_code);
                    } else {
                        self.bg_color = Some(color_code);
                    }
                    i += 5;
                    continue;
                }
            }

            match code {
                0 => self.reset(),
                1 => self.bold = true,
                2 => self.dim = true,
                3 => self.italic = true,
                4 => self.underline = true,
                5 => self.blink = true,
                7 => self.inverse = true,
                8 => self.hidden = true,
                9 => self.strikethrough = true,
                21 => self.bold = false,
                22 => {
                    self.bold = false;
                    self.dim = false;
                }
                23 => self.italic = false,
                24 => self.underline = false,
                25 => self.blink = false,
                27 => self.inverse = false,
                28 => self.hidden = false,
                29 => self.strikethrough = false,
                39 => self.fg_color = None,
                49 => self.bg_color = None,
                30..=37 | 90..=97 => self.fg_color = Some(code.to_string()),
                40..=47 | 100..=107 => self.bg_color = Some(code.to_string()),
                _ => {}
            }
            i += 1;
        }
    }

    fn get_active_codes(&self) -> String {
        let mut codes: Vec<&str> = Vec::new();
        if self.bold {
            codes.push("1");
        }
        if self.dim {
            codes.push("2");
        }
        if self.italic {
            codes.push("3");
        }
        if self.underline {
            codes.push("4");
        }
        if self.blink {
            codes.push("5");
        }
        if self.inverse {
            codes.push("7");
        }
        if self.hidden {
            codes.push("8");
        }
        if self.strikethrough {
            codes.push("9");
        }
        if let Some(ref fg) = self.fg_color {
            codes.push(fg.as_str());
        }
        if let Some(ref bg) = self.bg_color {
            codes.push(bg.as_str());
        }

        let mut result = if codes.is_empty() {
            String::new()
        } else {
            format!("\x1b[{}m", codes.join(";"))
        };
        if let Some(ref hl) = self.active_hyperlink {
            result.push_str(&format_osc8_hyperlink(hl));
        }
        result
    }

    fn get_active_background_code(&self) -> String {
        match self.bg_color {
            Some(ref bg) => format!("\x1b[{bg}m"),
            None => String::new(),
        }
    }

    fn has_active_codes(&self) -> bool {
        self.bold
            || self.dim
            || self.italic
            || self.underline
            || self.blink
            || self.inverse
            || self.hidden
            || self.strikethrough
            || self.fg_color.is_some()
            || self.bg_color.is_some()
            || self.active_hyperlink.is_some()
    }

    fn get_line_end_reset(&self) -> String {
        let mut result = String::new();
        if self.underline {
            result.push_str("\x1b[24m");
        }
        if self.strikethrough {
            result.push_str("\x1b[29m");
        }
        if let Some(ref hl) = self.active_hyperlink {
            result.push_str(&format_osc8_close(hl.terminator));
        }
        result
    }
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
        Self {
            inner: Arc::new(AnsiCodeTrackerInner {
                state: Mutex::new(AnsiCodeTrackerState::default()),
            }),
        }
    }

    fn process(&self, ansi_code: &str) {
        self.inner.state.lock().unwrap().process(ansi_code);
    }

    fn reset(&self) {
        self.inner.state.lock().unwrap().reset();
    }

    /// Clear all state for reuse.
    fn clear(&self) {
        self.inner.state.lock().unwrap().clear();
    }

    fn get_active_codes(&self) -> String {
        self.inner.state.lock().unwrap().get_active_codes()
    }

    fn get_active_background_code(&self) -> String {
        self.inner.state.lock().unwrap().get_active_background_code()
    }

    fn has_active_codes(&self) -> bool {
        self.inner.state.lock().unwrap().has_active_codes()
    }

    /// Get reset codes for attributes that need to be turned off at line end.
    /// Underline must be closed to prevent bleeding into padding.
    /// Active OSC 8 hyperlinks must be closed and re-opened on the next line.
    /// Returns empty string if no attributes need closing.
    fn get_line_end_reset(&self) -> String {
        self.inner.state.lock().unwrap().get_line_end_reset()
    }
}

fn update_tracker_from_text(text: &str, tracker: &AnsiCodeTracker) {
    let bytes = text.as_bytes();
    let mut state = tracker.inner.state.lock().unwrap();
    let mut i = 0;
    while let Some(rel) = text[i..].find('\x1b') {
        let esc_pos = i + rel;
        let len = ansi_code_length_bytes(bytes, esc_pos);
        if len > 0 {
            state.process(&text[esc_pos..esc_pos + len]);
            i = esc_pos + len;
        } else {
            i = esc_pos + 1;
        }
    }
}

/// Return only the background color active at the end of an ANSI-styled string.
pub fn get_active_background_ansi(text: &str) -> String {
    let tracker = AnsiCodeTracker::new();
    update_tracker_from_text(text, &tracker);
    tracker.get_active_background_code()
}

fn grapheme_segments(text: &str) -> Vec<String> {
    pi_js::intl::graphemes(text)
        .into_iter()
        .map(|s| s.segment.to_owned())
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TokenKind {
    Space,
    Word,
}

/// Split text into words while keeping ANSI codes attached.
fn split_into_tokens_with_ansi(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut pending_ansi = String::new();
    let mut current_kind: Option<TokenKind> = None;
    let mut i = 0;

    let flush_current = |current: &mut String, current_kind: &mut Option<TokenKind>, tokens: &mut Vec<String>| {
        if current.is_empty() {
            return;
        }
        tokens.push(std::mem::take(current));
        *current_kind = None;
    };

    while i < bytes.len() {
        let ansi_len = ansi_code_length_bytes(bytes, i);
        if ansi_len > 0 {
            pending_ansi.push_str(&text[i..i + ansi_len]);
            i += ansi_len;
            continue;
        }

        let end = find_text_run_end(bytes, i, false);
        let chunk = &text[i..end];
        if is_printable_ascii(chunk) {
            for k in 0..chunk.len() {
                let seg = &chunk[k..k + 1];
                let segment_kind = if seg == " " { TokenKind::Space } else { TokenKind::Word };
                if !current.is_empty() && current_kind != Some(segment_kind) {
                    flush_current(&mut current, &mut current_kind, &mut tokens);
                }
                if !pending_ansi.is_empty() {
                    current.push_str(&pending_ansi);
                    pending_ansi.clear();
                }
                current_kind = Some(segment_kind);
                current.push_str(seg);
            }
        } else {
            for seg in pi_js::intl::graphemes(chunk) {
                let segment = seg.segment;
                let segment_is_space = segment == " ";
                if !segment_is_space && cjk_break_regex.is_match(segment) {
                    flush_current(&mut current, &mut current_kind, &mut tokens);
                    let mut token = std::mem::take(&mut pending_ansi);
                    token.push_str(segment);
                    tokens.push(token);
                    continue;
                }

                let segment_kind = if segment_is_space {
                    TokenKind::Space
                } else {
                    TokenKind::Word
                };
                if !current.is_empty() && current_kind != Some(segment_kind) {
                    flush_current(&mut current, &mut current_kind, &mut tokens);
                }
                if !pending_ansi.is_empty() {
                    current.push_str(&pending_ansi);
                    pending_ansi.clear();
                }
                current_kind = Some(segment_kind);
                current.push_str(segment);
            }
        }

        i = end;
    }

    if !pending_ansi.is_empty() {
        if !current.is_empty() {
            current.push_str(&pending_ansi);
        } else if let Some(last) = tokens.last_mut() {
            last.push_str(&pending_ansi);
        } else {
            current = pending_ansi;
        }
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

/// Flatten cached lines. V8 keeps a string built by concatenation as a tree of its parts until something reads it
/// whole, and a cached line kept as such a tree retains several times its own size. Converting a string to a number
/// reads it whole, so V8 flattens it in place; the strings' values do not change.
///
/// PORT: Rust strings are already flat. The function stays so callers match TS; it does not change values.
pub fn flatten_lines(lines: &[String]) {
    let _ = lines;
}

fn split_lines_js(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\r' {
            out.push(&text[start..i]);
            if bytes.get(i + 1) == Some(&b'\n') {
                i += 2;
            } else {
                i += 1;
            }
            start = i;
        } else if bytes[i] == b'\n' {
            out.push(&text[start..i]);
            i += 1;
            start = i;
        } else {
            i += 1;
        }
    }
    out.push(&text[start..]);
    out
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
    if text.is_empty() {
        return vec![String::new()];
    }

    let input_lines = split_lines_js(text);
    let mut result: Vec<String> = Vec::new();
    let tracker = AnsiCodeTracker::new();

    for input_line in input_lines {
        let prefix = if !result.is_empty() {
            tracker.get_active_codes()
        } else {
            String::new()
        };
        let combined = if prefix.is_empty() {
            std::borrow::Cow::Borrowed(input_line)
        } else {
            std::borrow::Cow::Owned(format!("{prefix}{input_line}"))
        };
        for wrapped_line in wrap_single_line(&combined, width) {
            result.push(wrapped_line);
        }
        update_tracker_from_text(input_line, &tracker);
    }

    if result.is_empty() { vec![String::new()] } else { result }
}

fn wrap_single_line(line: &str, width: i64) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }

    let visible_length = visible_width(line);
    if visible_length <= width {
        return vec![line.to_owned()];
    }

    let mut wrapped: Vec<String> = Vec::new();
    let tracker = AnsiCodeTracker::new();
    let tokens = split_into_tokens_with_ansi(line);

    let mut current_line = String::new();
    let mut current_visible_length: i64 = 0;

    for token in tokens {
        let token_visible_length = visible_width(&token);
        let is_whitespace = is_whitespace_token(&token);

        if token_visible_length > width && !is_whitespace {
            if !current_line.is_empty() {
                let mut trimmed = pi_js::str16::trim_end(&current_line).to_owned();
                let line_end_reset = tracker.get_line_end_reset();
                if !line_end_reset.is_empty() {
                    trimmed.push_str(&line_end_reset);
                }
                wrapped.push(trimmed);
                current_line.clear();
                current_visible_length = 0;
            }

            let broken = break_long_word(&token, width, &tracker);
            if let Some((last, head)) = broken.split_last() {
                wrapped.extend(head.iter().cloned());
                current_line = last.clone();
                current_visible_length = visible_width(&current_line);
            }
            continue;
        }

        let total_needed = current_visible_length + token_visible_length;

        if total_needed > width && current_visible_length > 0 {
            let mut line_to_wrap = pi_js::str16::trim_end(&current_line).to_owned();
            if is_whitespace {
                line_to_wrap.push_str(&extract_ansi_codes(&token));
                update_tracker_from_text(&token, &tracker);
            }
            let line_end_reset = tracker.get_line_end_reset();
            if !line_end_reset.is_empty() {
                line_to_wrap.push_str(&line_end_reset);
            }
            wrapped.push(line_to_wrap);
            if is_whitespace {
                current_line = tracker.get_active_codes();
                current_visible_length = 0;
                continue;
            }
            current_line = format!("{}{token}", tracker.get_active_codes());
            current_visible_length = token_visible_length;
        } else if is_whitespace && total_needed > width && token.contains('\x1b') {
            current_line.push_str(&extract_ansi_codes(&token));
        } else {
            current_line.push_str(&token);
            current_visible_length += token_visible_length;
        }

        update_tracker_from_text(&token, &tracker);
    }

    if !current_line.is_empty() {
        wrapped.push(current_line);
    }

    if wrapped.is_empty() {
        vec![String::new()]
    } else {
        wrapped
            .into_iter()
            .map(|l| pi_js::str16::trim_end(&l).to_owned())
            .collect()
    }
}

pub static PUNCTUATION_REGEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r#"[(){}\[\]<>.,;:'"!?+=*/\\|&%^$#@~`-]"#).expect("PUNCTUATION_REGEX"));

/// Check if a character is whitespace.
pub fn is_whitespace_char(char: &str) -> bool {
    char.chars().any(pi_js::str16::is_js_whitespace)
}

/// Check if a character is punctuation.
pub fn is_punctuation_char(char: &str) -> bool {
    PUNCTUATION_REGEX.is_match(char)
}

enum WordSegment<'a> {
    Ansi(&'a str),
    Grapheme(&'a str),
}

fn break_long_word(word: &str, width: i64, tracker: &AnsiCodeTracker) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current_line = tracker.get_active_codes();
    let mut current_width: i64 = 0;

    let bytes = word.as_bytes();
    let mut i = 0;
    let mut segments: Vec<WordSegment<'_>> = Vec::new();

    while i < bytes.len() {
        let ansi_len = ansi_code_length_bytes(bytes, i);
        if ansi_len > 0 {
            segments.push(WordSegment::Ansi(&word[i..i + ansi_len]));
            i += ansi_len;
        } else {
            let end = find_text_run_end(bytes, i, false);
            for seg in pi_js::intl::graphemes(&word[i..end]) {
                segments.push(WordSegment::Grapheme(seg.segment));
            }
            i = end;
        }
    }

    for seg in segments {
        match seg {
            WordSegment::Ansi(code) => {
                current_line.push_str(code);
                tracker.process(code);
            }
            WordSegment::Grapheme(grapheme) => {
                if grapheme.is_empty() {
                    continue;
                }
                let gw = visible_width(grapheme);
                if current_width + gw > width {
                    let line_end_reset = tracker.get_line_end_reset();
                    if !line_end_reset.is_empty() {
                        current_line.push_str(&line_end_reset);
                    }
                    lines.push(current_line);
                    current_line = tracker.get_active_codes();
                    current_width = 0;
                }
                current_line.push_str(grapheme);
                current_width += gw;
            }
        }
    }

    if !current_line.is_empty() {
        lines.push(current_line);
    }

    if lines.is_empty() { vec![String::new()] } else { lines }
}

/// Apply background color to a line, padding to full width.
///
/// @param line - Line of text (may contain ANSI codes)
/// @param width - Total width to pad to
/// @param bgFn - Background color function
/// @returns Line with background applied and padded to width
pub fn apply_background_to_line(line: &str, width: i64, bg_fn: Arc<dyn Fn(&str) -> String + Send + Sync>) -> String {
    let visible_len = visible_width(line);
    let padding_needed = (width - visible_len).max(0) as usize;
    let mut with_padding = String::with_capacity(line.len() + padding_needed);
    with_padding.push_str(line);
    with_padding.extend(std::iter::repeat_n(' ', padding_needed));
    bg_fn(&with_padding)
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
    if max_width <= 0 {
        return String::new();
    }

    let ellipsis = ellipsis.unwrap_or("...");
    let pad = pad.unwrap_or(false);

    if text.is_empty() {
        return if pad {
            " ".repeat(max_width as usize)
        } else {
            String::new()
        };
    }

    let ellipsis_width = visible_width(ellipsis);
    if ellipsis_width >= max_width {
        let text_width = visible_width(text);
        if text_width <= max_width {
            if pad {
                let mut out = String::with_capacity(text.len() + (max_width - text_width) as usize);
                out.push_str(text);
                out.extend(std::iter::repeat_n(' ', (max_width - text_width) as usize));
                return out;
            }
            return text.to_owned();
        }

        let clipped_ellipsis = truncate_fragment_to_width(ellipsis, max_width);
        if clipped_ellipsis.width == 0 {
            return if pad {
                " ".repeat(max_width as usize)
            } else {
                String::new()
            };
        }
        return finalize_truncated_result("", 0, &clipped_ellipsis.text, clipped_ellipsis.width, max_width, pad);
    }

    if is_printable_ascii(text) {
        let text_len = text.len() as i64;
        if text_len <= max_width {
            if pad {
                let mut out = String::with_capacity(max_width as usize);
                out.push_str(text);
                out.extend(std::iter::repeat_n(' ', (max_width - text_len) as usize));
                return out;
            }
            return text.to_owned();
        }
        let target_width = max_width - ellipsis_width;
        return finalize_truncated_result(
            &text[..target_width as usize],
            target_width,
            ellipsis,
            ellipsis_width,
            max_width,
            pad,
        );
    }

    let target_width = max_width - ellipsis_width;
    let mut result = String::new();
    let mut pending_ansi = String::new();
    let mut visible_so_far: i64 = 0;
    let mut kept_width: i64 = 0;
    let mut keep_contiguous_prefix = true;
    let mut overflowed = false;
    let exhausted_input;
    let has_ansi = text.contains('\x1b');
    let has_tabs = text.contains('\t');

    if !has_ansi && !has_tabs {
        for seg in pi_js::intl::graphemes(text) {
            let width = grapheme_width(seg.segment);
            if keep_contiguous_prefix && kept_width + width <= target_width {
                result.push_str(seg.segment);
                kept_width += width;
            } else {
                keep_contiguous_prefix = false;
            }
            visible_so_far += width;
            if visible_so_far > max_width {
                overflowed = true;
                break;
            }
        }
        exhausted_input = !overflowed;
    } else {
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let ansi_len = ansi_code_length_bytes(bytes, i);
            if ansi_len > 0 {
                pending_ansi.push_str(&text[i..i + ansi_len]);
                i += ansi_len;
                continue;
            }

            if bytes[i] == b'\t' {
                if keep_contiguous_prefix && kept_width + 3 <= target_width {
                    if !pending_ansi.is_empty() {
                        result.push_str(&pending_ansi);
                        pending_ansi.clear();
                    }
                    result.push('\t');
                    kept_width += 3;
                } else {
                    keep_contiguous_prefix = false;
                    pending_ansi.clear();
                }
                visible_so_far += 3;
                if visible_so_far > max_width {
                    overflowed = true;
                    break;
                }
                i += 1;
                continue;
            }

            let end = find_text_run_end(bytes, i, true);
            for seg in pi_js::intl::graphemes(&text[i..end]) {
                let width = grapheme_width(seg.segment);
                if keep_contiguous_prefix && kept_width + width <= target_width {
                    if !pending_ansi.is_empty() {
                        result.push_str(&pending_ansi);
                        pending_ansi.clear();
                    }
                    result.push_str(seg.segment);
                    kept_width += width;
                } else {
                    keep_contiguous_prefix = false;
                    pending_ansi.clear();
                }

                visible_so_far += width;
                if visible_so_far > max_width {
                    overflowed = true;
                    break;
                }
            }
            if overflowed {
                break;
            }
            i = end;
        }
        exhausted_input = i >= bytes.len();
    }

    if !overflowed && exhausted_input {
        if pad {
            let pad_len = (max_width - visible_so_far).max(0) as usize;
            let mut out = String::with_capacity(text.len() + pad_len);
            out.push_str(text);
            out.extend(std::iter::repeat_n(' ', pad_len));
            return out;
        }
        return text.to_owned();
    }

    finalize_truncated_result(&result, kept_width, ellipsis, ellipsis_width, max_width, pad)
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
    if length <= 0 {
        return SliceWithWidth {
            text: String::new(),
            width: 0,
        };
    }
    let strict = strict.unwrap_or(false);
    let end_col = start_col + length;
    let bytes = line.as_bytes();
    let mut result = String::new();
    let mut result_width: i64 = 0;
    let mut current_col: i64 = 0;
    let mut i = 0;
    let mut pending_ansi = String::new();

    while i < bytes.len() {
        let ansi_len = ansi_code_length_bytes(bytes, i);
        if ansi_len > 0 {
            let ansi_code = &line[i..i + ansi_len];
            // APC sequences (cursor markers) are positional one-shot commands, not persistent style state.
            if current_col >= start_col && current_col < end_col {
                if !pending_ansi.is_empty() {
                    result.push_str(&pending_ansi);
                    pending_ansi.clear();
                }
                result.push_str(ansi_code);
            } else if current_col < start_col && !ansi_code.starts_with("\x1b_") {
                pending_ansi.push_str(ansi_code);
            }
            i += ansi_len;
            continue;
        }

        let text_end = find_text_run_end(bytes, i, false);
        for seg in pi_js::intl::graphemes(&line[i..text_end]) {
            let w = grapheme_width(seg.segment);
            let in_range = current_col >= start_col && current_col < end_col;
            let fits = !strict || current_col + w <= end_col;
            if in_range && fits {
                if !pending_ansi.is_empty() {
                    result.push_str(&pending_ansi);
                    pending_ansi.clear();
                }
                result.push_str(seg.segment);
                result_width += w;
            }
            current_col += w;
            if current_col >= end_col {
                break;
            }
        }
        i = text_end;
        if current_col >= end_col {
            break;
        }
    }

    SliceWithWidth {
        text: result,
        width: result_width,
    }
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
    let strict_after = strict_after.unwrap_or(false);
    let mut before = String::new();
    let mut before_width: i64 = 0;
    let mut after = String::new();
    let mut after_width: i64 = 0;
    let mut current_col: i64 = 0;
    let mut i = 0;
    let mut pending_ansi_before = String::new();
    let mut after_started = false;
    let after_end = after_start + after_len;

    let bytes = line.as_bytes();
    let mut style_tracker = POOLED_STYLE_TRACKER.inner.state.lock().unwrap();
    style_tracker.clear();

    while i < bytes.len() {
        let ansi_len = ansi_code_length_bytes(bytes, i);
        if ansi_len > 0 {
            let ansi_code = &line[i..i + ansi_len];
            style_tracker.process(ansi_code);
            if current_col < before_end {
                pending_ansi_before.push_str(ansi_code);
            } else if current_col >= after_start
                && current_col < after_end
                && (after_started || ansi_code.starts_with("\x1b_"))
            {
                after.push_str(ansi_code);
            }
            i += ansi_len;
            continue;
        }

        let text_end = find_text_run_end(bytes, i, false);
        for seg in pi_js::intl::graphemes(&line[i..text_end]) {
            let w = grapheme_width(seg.segment);

            if current_col < before_end && current_col + w <= before_end {
                if !pending_ansi_before.is_empty() {
                    before.push_str(&pending_ansi_before);
                    pending_ansi_before.clear();
                }
                before.push_str(seg.segment);
                before_width += w;
            } else if current_col >= after_start && current_col < after_end {
                let fits = !strict_after || current_col + w <= after_end;
                if fits {
                    if !after_started {
                        after.push_str(&style_tracker.get_active_codes());
                        after_started = true;
                    }
                    after.push_str(seg.segment);
                    after_width += w;
                }
            }

            current_col += w;
            let done = if after_len <= 0 {
                current_col >= before_end
            } else {
                current_col >= after_end
            };
            if done {
                break;
            }
        }
        i = text_end;
        let done = if after_len <= 0 {
            current_col >= before_end
        } else {
            current_col >= after_end
        };
        if done {
            break;
        }
    }

    ExtractedSegments {
        before,
        before_width,
        after,
        after_width,
    }
}
