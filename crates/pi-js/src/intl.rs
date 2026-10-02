//! pi_js::intl (Rust-only; API contract: PORTING.md Appendix A).
//!
//! The parts of ECMA-402 that pi uses, pinned to what Node 24 (V8 13.6, ICU 78) prints for the
//! `en-US` default locale (PORTING.md §13.2):
//!
//! - `a.localeCompare(b)`: ICU root collation, tertiary strength (`en` has no tailoring).
//! - `n.toLocaleString()`: `Intl.NumberFormat("en-US")` defaults: grouping with `,`, 0 to 3
//!   fraction digits, half-expand rounding of the shortest round-trip decimal, `NaN`, `∞`, `-0`.
//! - `date.toLocaleString()` / `date.toLocaleTimeString()`: `M/D/YYYY, h:mm:ss AM` in the local
//!   time zone, plain space before the day period (V8 replaces ICU's U+202F), proleptic Gregorian
//!   calendar, era year (`1 - year`) for years <= 0, `Invalid Date` outside +-8.64e15 ms.
//! - `new Intl.Segmenter(undefined, { granularity })`: UAX #29 grapheme and word segmentation with
//!   dictionary-based word breaking for CJK and South-East Asian scripts (as ICU4C does).

use std::cmp::Ordering;
use std::sync::LazyLock;

use chrono::{DateTime, Offset, TimeZone, Utc};
use icu_collator::options::CollatorOptions;
use icu_collator::{CollatorBorrowed, CollatorPreferences};
use icu_segmenter::options::WordBreakInvariantOptions;
use icu_segmenter::{GraphemeClusterSegmenter, GraphemeClusterSegmenterBorrowed, WordSegmenter, WordSegmenterBorrowed};

static COLLATOR: LazyLock<CollatorBorrowed<'static>> = LazyLock::new(|| {
    CollatorBorrowed::try_new(CollatorPreferences::default(), CollatorOptions::default())
        .expect("compiled root collation data is always available")
});

const GRAPHEME_SEGMENTER: GraphemeClusterSegmenterBorrowed<'static> = GraphemeClusterSegmenter::new();

static WORD_SEGMENTER: LazyLock<WordSegmenterBorrowed<'static>> =
    LazyLock::new(|| WordSegmenter::new_dictionary(WordBreakInvariantOptions::default()));

/// `a.localeCompare(b)` (ICU root collation, the `en-US` default).
pub fn locale_compare(a: &str, b: &str) -> Ordering {
    COLLATOR.compare(a, b)
}

/// `n.toLocaleString()` for a number in `en-US`.
///
/// ICU formats the shortest round-trip decimal of `n` (not its exact binary value), so
/// `(1.0005).toLocaleString()` is `"1.001"` even though the double is slightly below 1.0005.
pub fn number_to_locale_string(n: f64) -> String {
    if n.is_nan() {
        return "NaN".to_string();
    }
    let negative = n.is_sign_negative();
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    if n.is_infinite() {
        out.push('∞');
        return out;
    }
    if n == 0.0 {
        out.push('0');
        return out;
    }
    // The shortest round-trip decimal, with JS (and ICU double-conversion) tie-breaking:
    // 1e15 + 0.25 has two 17-digit candidates and both pick the even one, `...0.2`.
    let mut buf = ryu_js::Buffer::new();
    let (mut digits, mut point) = decimal_digits(buf.format_finite(n.abs()));
    round_half_expand(&mut digits, &mut point, 3);
    format_grouped_decimal(&mut out, &digits, point);
    out
}

/// Splits a JS number string (`"123.45"`, `"1e+21"`, `"1.5e-7"`) into significant digits and the
/// decimal point position: the value is `0.d1d2d3... * 10^point`.
fn decimal_digits(s: &str) -> (Vec<u8>, i64) {
    let (mantissa, exponent) = match s.split_once('e') {
        Some((m, e)) => (m, e.trim_start_matches('+').parse::<i64>().expect("JS number exponent")),
        None => (s, 0),
    };
    let int_len = mantissa.find('.').unwrap_or(mantissa.len()) as i64;
    let mut digits: Vec<u8> = mantissa.bytes().filter(u8::is_ascii_digit).map(|b| b - b'0').collect();
    let leading_zeros = digits.iter().take_while(|&&d| d == 0).count();
    digits.drain(..leading_zeros);
    (digits, int_len + exponent - leading_zeros as i64)
}

/// Rounds the decimal `0.digits * 10^point` to `fraction_digits` digits after the decimal point,
/// ties away from zero. Leaves `digits` without trailing zeros (possibly empty for zero).
fn round_half_expand(digits: &mut Vec<u8>, point: &mut i64, fraction_digits: i64) {
    let keep = *point + fraction_digits;
    if keep < 0 {
        digits.clear();
    } else if (keep as usize) < digits.len() {
        let keep = keep as usize;
        let round_up = digits[keep] >= 5;
        digits.truncate(keep);
        if round_up {
            let mut i = digits.len();
            loop {
                if i == 0 {
                    digits.insert(0, 1);
                    *point += 1;
                    break;
                }
                i -= 1;
                if digits[i] == 9 {
                    digits[i] = 0;
                } else {
                    digits[i] += 1;
                    break;
                }
            }
        }
    }
    while digits.last() == Some(&0) {
        digits.pop();
    }
}

/// Appends `0.digits * 10^point` with `,` grouping every 3 integer digits.
fn format_grouped_decimal(out: &mut String, digits: &[u8], point: i64) {
    let int_len = point.max(0) as usize;
    if int_len == 0 {
        out.push('0');
    } else {
        for i in 0..int_len {
            if i > 0 && (int_len - i).is_multiple_of(3) {
                out.push(',');
            }
            out.push(char::from(b'0' + digits.get(i).copied().unwrap_or(0)));
        }
    }
    let frac_start = point.max(0) as usize;
    if digits.len() > frac_start {
        out.push('.');
        for _ in point..0 {
            out.push('0');
        }
        for d in &digits[frac_start..] {
            out.push(char::from(b'0' + d));
        }
    }
}

/// `new Date(ms).toLocaleString()` in the local time zone, e.g. `"10/2/2026, 9:05:03 AM"`.
pub fn date_to_locale_string(ms: i64) -> String {
    date_to_locale_string_in(ms, &chrono::Local)
}

/// `new Date(ms).toLocaleTimeString()` in the local time zone: `"9:05:03 AM"`, or with
/// `two_digit` (`{ hour: "2-digit", minute: "2-digit", second: "2-digit" }`) `"09:05:03 AM"`.
pub fn date_to_locale_time_string(ms: i64, two_digit: bool) -> String {
    date_to_locale_time_string_in(ms, two_digit, &chrono::Local)
}

/// [`date_to_locale_string`] in an explicit time zone (Node with `TZ=<tz>`).
pub fn date_to_locale_string_in<Tz: TimeZone>(ms: i64, tz: &Tz) -> String {
    match LocalFields::new(ms, tz) {
        Some(f) => format!("{}/{}/{}, {}", f.month, f.day, f.era_year(), f.time(false)),
        None => INVALID_DATE.to_string(),
    }
}

/// [`date_to_locale_time_string`] in an explicit time zone (Node with `TZ=<tz>`).
pub fn date_to_locale_time_string_in<Tz: TimeZone>(ms: i64, two_digit: bool, tz: &Tz) -> String {
    match LocalFields::new(ms, tz) {
        Some(f) => f.time(two_digit),
        None => INVALID_DATE.to_string(),
    }
}

const INVALID_DATE: &str = "Invalid Date";

/// ECMAScript time values are limited to +-8.64e15 ms around the epoch.
const MAX_TIME_MS: i64 = 8_640_000_000_000_000;

const MS_PER_DAY: i64 = 86_400_000;

struct LocalFields {
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
}

impl LocalFields {
    fn new<Tz: TimeZone>(ms: i64, tz: &Tz) -> Option<LocalFields> {
        if !(-MAX_TIME_MS..=MAX_TIME_MS).contains(&ms) {
            return None;
        }
        let local = ms + local_offset_ms(ms, tz);
        let days = local.div_euclid(MS_PER_DAY);
        let ms_of_day = local.rem_euclid(MS_PER_DAY);
        let (year, month, day) = civil_from_days(days);
        let secs = ms_of_day / 1000;
        Some(LocalFields {
            year,
            month,
            day,
            hour: (secs / 3600) as u32,
            minute: (secs / 60 % 60) as u32,
            second: (secs % 60) as u32,
        })
    }

    /// ICU's `y` field in the Gregorian calendar is the era year: 0 is 1 BC, -1 is 2 BC.
    fn era_year(&self) -> i64 {
        if self.year <= 0 { 1 - self.year } else { self.year }
    }

    fn time(&self, two_digit: bool) -> String {
        let hour12 = match self.hour % 12 {
            0 => 12,
            h => h,
        };
        let period = if self.hour < 12 { "AM" } else { "PM" };
        if two_digit {
            format!("{hour12:02}:{:02}:{:02} {period}", self.minute, self.second)
        } else {
            format!("{hour12}:{:02}:{:02} {period}", self.minute, self.second)
        }
    }
}

/// Offset of `tz` from UTC at the instant `ms`, in milliseconds. Instants outside chrono's range
/// (about +-262,000 years; JS allows +-275,760) use the offset at the nearest representable instant.
fn local_offset_ms<Tz: TimeZone>(ms: i64, tz: &Tz) -> i64 {
    let utc = DateTime::from_timestamp_millis(ms).unwrap_or(if ms < 0 {
        DateTime::<Utc>::MIN_UTC
    } else {
        DateTime::<Utc>::MAX_UTC
    });
    let offset = tz.offset_from_utc_datetime(&utc.naive_utc());
    i64::from(offset.fix().local_minus_utc()) * 1000
}

/// Proleptic Gregorian (year, month, day) for days since 1970-01-01 (H. Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// One `Intl.Segmenter` segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment<'a> {
    pub segment: &'a str,
    /// UTF-16 code-unit offset (the JS `index`).
    pub index: usize,
    pub byte_index: usize,
    /// `isWordLike` for word granularity; `None` for graphemes.
    pub is_word_like: Option<bool>,
}

/// `[...new Intl.Segmenter().segment(s)]` (grapheme granularity, extended grapheme clusters).
pub fn graphemes(s: &str) -> Vec<Segment<'_>> {
    collect_segments(s, GRAPHEME_SEGMENTER.segment_str(s).map(|b| (b, None)))
}

/// `[...new Intl.Segmenter(undefined, { granularity: "word" }).segment(s)]`.
pub fn words(s: &str) -> Vec<Segment<'_>> {
    let mut out = collect_segments(
        s,
        WORD_SEGMENTER
            .segment_str(s)
            .iter_with_word_type()
            .map(|(b, t)| (b, Some(t.is_word_like()))),
    );
    // PORT: ICU4X 2.3 loses the rule status of a segment that ends in Extend/Format/ZWJ when more
    // text follows (`"e\u{301} x"`, `"नमस्ते दुनिया"`, `"1\u{20e3} "` report not word-like; ICU4C
    // reports word-like). At end of text the status is right, so re-segment such a segment alone.
    for seg in &mut out {
        if seg.is_word_like == Some(false)
            && seg.byte_index + seg.segment.len() < s.len()
            && seg.segment.chars().nth(1).is_some()
        {
            let mut alone = WORD_SEGMENTER
                .segment_str(seg.segment)
                .iter_with_word_type()
                .filter(|&(b, _)| b > 0);
            if let (Some((end, word_type)), None) = (alone.next(), alone.next())
                && end == seg.segment.len()
            {
                seg.is_word_like = Some(word_type.is_word_like());
            }
        }
    }
    out
}

/// `[...segmenter.segment(s)].length` for graphemes without allocating segments.
pub fn grapheme_count(s: &str) -> usize {
    GRAPHEME_SEGMENTER.segment_str(s).filter(|&b| b > 0).count()
}

fn collect_segments<'a>(s: &'a str, boundaries: impl Iterator<Item = (usize, Option<bool>)>) -> Vec<Segment<'a>> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut index = 0;
    for (end, is_word_like) in boundaries {
        if end <= start {
            continue;
        }
        let segment = &s[start..end];
        out.push(Segment {
            segment,
            index,
            byte_index: start,
            is_word_like,
        });
        index += segment.encode_utf16().count();
        start = end;
    }
    out
}
