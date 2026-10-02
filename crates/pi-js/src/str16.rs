//! pi_js::str16 (Rust-only; API contract: PORTING.md Appendix A).
//!
//! JS string semantics on top of Rust UTF-8 strings. Every index and length is measured in
//! UTF-16 code units, exactly like `String.prototype.length`, `slice`, `substring`, `indexOf`,
//! `charCodeAt` and `padStart`.
//!
//! Rust strings cannot hold lone surrogates. Wherever JS would produce half of a surrogate pair
//! (a range boundary that splits an astral character), the lone half becomes U+FFFD.

use std::cmp::Ordering;

/// `s.length`: the number of UTF-16 code units.
pub fn len(s: &str) -> usize {
    // Every char contributes one unit, astral chars (4-byte UTF-8 sequences) contribute two.
    // A byte that is not a continuation byte starts a char; a lead byte >= 0xF0 starts an
    // astral char.
    s.bytes()
        .map(|b| usize::from((b & 0xC0) != 0x80) + usize::from(b >= 0xF0))
        .sum()
}

/// Locates UTF-16 unit `unit` in `s`.
///
/// Returns the byte offset of the char that contains the unit, and whether the unit is the
/// second (low surrogate) half of that char. Units at or past the end map to `(s.len(), false)`.
fn locate(s: &str, unit: usize) -> (usize, bool) {
    if s.is_ascii() {
        return (unit.min(s.len()), false);
    }
    let mut pos = 0usize;
    for (byte, c) in s.char_indices() {
        if pos == unit {
            return (byte, false);
        }
        let w = c.len_utf16();
        if w == 2 && pos + 1 == unit {
            return (byte, true);
        }
        pos += w;
        if pos > unit {
            // Unreachable: covered by the two checks above.
            return (byte, false);
        }
    }
    (s.len(), false)
}

/// The UTF-16 range `[from, to)` of `s` (both already clamped, `from < to`), with split
/// surrogate halves replaced by U+FFFD.
fn range(s: &str, from: usize, to: usize) -> String {
    if from >= to {
        return String::new();
    }
    if s.is_ascii() {
        let end = to.min(s.len());
        let start = from.min(end);
        return s[start..end].to_string();
    }
    let (b_from, from_split) = locate(s, from);
    let (b_to, to_split) = locate(s, to);
    let mut out = String::with_capacity(b_to.saturating_sub(b_from) + 6);
    let mut start = b_from;
    if from_split {
        // `from` points at the low surrogate of the astral char at `b_from`.
        out.push('\u{FFFD}');
        start = b_from + 4;
    }
    if b_to > start {
        out.push_str(&s[start..b_to]);
    }
    if to_split {
        // `to` points at the low surrogate of the astral char at `b_to`, so the range ends with
        // its high surrogate.
        out.push('\u{FFFD}');
    }
    out
}

fn clamp_relative(i: i64, len: i64) -> usize {
    if i < 0 {
        (len + i).max(0) as usize
    } else {
        i.min(len) as usize
    }
}

/// `s.slice(start, end)`: negative indices count from the end; the result is empty when
/// `start >= end` after clamping.
pub fn slice(s: &str, start: i64, end: Option<i64>) -> String {
    let len = len(s) as i64;
    let from = clamp_relative(start, len);
    let to = match end {
        None => len as usize,
        Some(e) => clamp_relative(e, len),
    };
    range(s, from, to)
}

/// `s.substring(start, end)`: indices are clamped to `[0, length]` and swapped when
/// `start > end`.
pub fn substring(s: &str, start: usize, end: Option<usize>) -> String {
    let len = len(s);
    let a = start.min(len);
    let b = end.unwrap_or(len).min(len);
    range(s, a.min(b), a.max(b))
}

/// `s.charCodeAt(i)`; `None` where JS returns `NaN` (index out of range).
pub fn char_code_at(s: &str, i: usize) -> Option<u16> {
    if s.is_ascii() {
        return s.as_bytes().get(i).map(|&b| u16::from(b));
    }
    s.encode_utf16().nth(i)
}

/// `s.indexOf(needle, from)`; `None` where JS returns `-1`.
pub fn index_of(s: &str, needle: &str, from: usize) -> Option<usize> {
    let start = from.min(len(s));
    if needle.is_empty() {
        return Some(start);
    }
    let (b, split) = locate(s, start);
    // A non-empty needle never starts with a low surrogate, so a match cannot begin in the
    // middle of an astral char: continue at the next char boundary.
    let (b, base) = if split { (b + 4, start + 1) } else { (b, start) };
    s[b..].find(needle).map(|i| base + len(&s[b..b + i]))
}

/// `s.lastIndexOf(needle)`; `None` where JS returns `-1`.
pub fn last_index_of(s: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(len(s));
    }
    s.rfind(needle).map(|b| len(&s[..b]))
}

/// `s.lastIndexOf(needle, from)`: the last match that starts at or before UTF-16 index `from`.
pub fn last_index_of_from(s: &str, needle: &str, from: usize) -> Option<usize> {
    let start = from.min(len(s));
    if needle.is_empty() {
        return Some(start);
    }
    // A match may start at any char that begins at or before unit `start` (when `start` is the
    // low half of a pair, the pair's own start still qualifies), and may extend past it.
    let (b, _) = locate(s, start);
    let mut limit = (b + needle.len()).min(s.len());
    while !s.is_char_boundary(limit) {
        limit -= 1;
    }
    s[..limit].rfind(needle).map(|found| len(&s[..found]))
}

/// UTF-16 index of byte offset `byte`. Offsets inside a char round down to its start; offsets
/// past the end clamp to the length.
pub fn byte_to_utf16(s: &str, byte: usize) -> usize {
    let mut b = byte.min(s.len());
    while !s.is_char_boundary(b) {
        b -= 1;
    }
    len(&s[..b])
}

/// Byte offset of UTF-16 index `unit`. An index pointing at the low half of a surrogate pair
/// rounds down to the start of that char; indices past the end clamp to `s.len()`.
pub fn utf16_to_byte(s: &str, unit: usize) -> usize {
    locate(s, unit).0
}

fn filler(fill: &str, fill_len: usize) -> String {
    let unit = len(fill);
    let whole = fill_len / unit;
    let rest = fill_len % unit;
    let mut out = fill.repeat(whole);
    out.push_str(&range(fill, 0, rest));
    out
}

/// `s.padStart(target_len, fill)`.
pub fn pad_start(s: &str, target_len: usize, fill: &str) -> String {
    let l = len(s);
    if target_len <= l || fill.is_empty() {
        return s.to_string();
    }
    let mut out = filler(fill, target_len - l);
    out.push_str(s);
    out
}

/// `s.padEnd(target_len, fill)`.
pub fn pad_end(s: &str, target_len: usize, fill: &str) -> String {
    let l = len(s);
    if target_len <= l || fill.is_empty() {
        return s.to_string();
    }
    let mut out = s.to_string();
    out.push_str(&filler(fill, target_len - l));
    out
}

/// `s.trim()` with the JS whitespace set.
pub fn trim(s: &str) -> &str {
    s.trim_matches(is_js_whitespace)
}

/// `s.trimStart()` with the JS whitespace set.
pub fn trim_start(s: &str) -> &str {
    s.trim_start_matches(is_js_whitespace)
}

/// `s.trimEnd()` with the JS whitespace set.
pub fn trim_end(s: &str) -> &str {
    s.trim_end_matches(is_js_whitespace)
}

/// JS `WhiteSpace` plus `LineTerminator` (what `trim()` strips and regex `\s` matches):
/// TAB, LF, VT, FF, CR, SPACE, NBSP, U+1680, U+2000..U+200A, LS, PS, U+202F, U+205F, U+3000,
/// U+FEFF. U+0085 and U+180E are not included.
pub fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{0009}'..='\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

/// JS string comparison (`a < b`, default `Array.prototype.sort`): lexicographic over UTF-16
/// code units. Differs from Rust's byte order when astral chars meet U+E000..U+FFFF.
pub fn cmp(a: &str, b: &str) -> Ordering {
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    let common = ab.iter().zip(bb).take_while(|(x, y)| x == y).count();
    if common == ab.len() || common == bb.len() {
        // One string is a prefix of the other.
        return ab.len().cmp(&bb.len());
    }
    // Back up to the start of the differing chars; both strings agree on everything before.
    let mut start = common;
    while !a.is_char_boundary(start) {
        start -= 1;
    }
    a[start..].encode_utf16().cmp(b[start..].encode_utf16())
}

/// The UTF-16 code units of `s`.
pub fn to_utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// Builds a string from UTF-16 code units; lone surrogates become U+FFFD.
pub fn from_utf16_lossy(units: &[u16]) -> String {
    String::from_utf16_lossy(units)
}

/// `s.charAt(i)`: the code unit at `i` as a string (`""` when out of range; a lone surrogate
/// half becomes U+FFFD).
pub fn char_at(s: &str, i: usize) -> String {
    range(s, i, i.saturating_add(1).min(len(s)))
}

/// `s.codePointAt(i)`; `None` where JS returns `undefined`. Pointing at the low half of a pair
/// yields that low surrogate value, as in JS.
pub fn code_point_at(s: &str, i: usize) -> Option<u32> {
    let (b, split) = locate(s, i);
    let c = s[b..].chars().next()?;
    if split {
        let mut buf = [0u16; 2];
        c.encode_utf16(&mut buf);
        return Some(u32::from(buf[1]));
    }
    Some(c as u32)
}
