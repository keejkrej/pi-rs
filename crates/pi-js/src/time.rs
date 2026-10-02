//! pi_js::time (Rust-only; API contract: PORTING.md Appendix A).
//!
//! `Date.now()`, `performance.now()`, `Date.prototype.toISOString`, `Date.parse`,
//! `setTimeout` / `setInterval` and an abortable `sleep`.
//!
//! Fake timers: tests use `#[tokio::test(start_paused = true)]` plus
//! [`testing::set_system_time`]. Once a system time is set on a thread, [`now_ms`]
//! returns it advanced by the paused tokio clock, so `tokio::time::advance` moves
//! `Date.now()` exactly like `vi.advanceTimersByTime`. Timers are tokio tasks whose
//! deadlines are fixed at creation time, so `set_timeout(100, f)` followed by
//! `advance(100ms)` runs `f`.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use chrono::{Local, LocalResult, NaiveDateTime, Offset, TimeZone};

use crate::abort::{AbortReason, AbortSignal};
use crate::error::{Error, Result};

/// Largest valid time value (`8.64e15` ms, ECMA-262 TimeClip).
const MAX_TIME_MS: i64 = 8_640_000_000_000_000;
/// V8 `DateCache::kMaxTimeBeforeUTCInMs`: local times may exceed the range by ten days.
const MAX_TIME_BEFORE_UTC_MS: i64 = MAX_TIME_MS + 864_000_000;
const MS_PER_DAY: i64 = 86_400_000;
/// Node `TIMEOUT_MAX` (2^31 - 1).
const TIMEOUT_MAX: u64 = 2_147_483_647;

thread_local! {
    /// (fake wall-clock ms, tokio instant when it was set)
    static FAKE_NOW: Cell<Option<(i64, tokio::time::Instant)>> = const { Cell::new(None) };
    static TIMERS: RefCell<Vec<Weak<TimerState>>> = const { RefCell::new(Vec::new()) };
}

fn real_now_ms() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(e) => {
            // Before 1970: floor of a negative value.
            let d = e.duration();
            let ms = d.as_millis() as i64;
            if d.as_nanos() % 1_000_000 == 0 { -ms } else { -ms - 1 }
        }
    }
}

/// `Date.now()`.
pub fn now_ms() -> i64 {
    if let Some((base, at)) = FAKE_NOW.with(|f| f.get()) {
        let elapsed = tokio::time::Instant::now().saturating_duration_since(at);
        return base + elapsed.as_millis() as i64;
    }
    real_now_ms()
}

static PERF_START: OnceLock<tokio::time::Instant> = OnceLock::new();

/// `performance.now()`: milliseconds (with sub-millisecond precision) since the first call
/// in this process. Follows the paused tokio clock in tests.
pub fn performance_now() -> f64 {
    let start = *PERF_START.get_or_init(tokio::time::Instant::now);
    let now = tokio::time::Instant::now();
    // Differences stay exact even when a paused test clock is behind `start`.
    match now.checked_duration_since(start) {
        Some(d) => d.as_secs_f64() * 1000.0,
        None => -(start.duration_since(now).as_secs_f64() * 1000.0),
    }
}

// ---------------------------------------------------------------------------------------
// Calendar math (ECMA-262 MakeDay / MakeDate and their inverse).

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `Date.prototype.toISOString()`; `RangeError: Invalid time value` outside ±8.64e15.
pub fn try_iso_string(ms: i64) -> Result<String> {
    if !(-MAX_TIME_MS..=MAX_TIME_MS).contains(&ms) {
        return Err(Error::js("RangeError", "Invalid time value"));
    }
    let days = ms.div_euclid(MS_PER_DAY);
    let rem = ms.rem_euclid(MS_PER_DAY);
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss, mss) = (rem / 3_600_000, rem / 60_000 % 60, rem / 1000 % 60, rem % 1000);
    let year = if (0..=9999).contains(&y) {
        format!("{y:04}")
    } else if y < 0 {
        format!("-{:06}", -y)
    } else {
        format!("+{y:06}")
    };
    Ok(format!("{year}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}.{mss:03}Z"))
}

/// `new Date(ms).toISOString()`.
///
/// PORT: JS throws `RangeError: Invalid time value` for out-of-range times; this
/// infallible form returns `"Invalid Date"` instead. Use [`try_iso_string`] to observe the
/// error.
pub fn iso_string(ms: i64) -> String {
    try_iso_string(ms).unwrap_or_else(|_| "Invalid Date".to_string())
}

/// `new Date().toISOString()`.
pub fn iso_now() -> String {
    iso_string(now_ms())
}

/// `Date.parse(s)`; `None` for NaN.
pub fn parse_date(s: &str) -> Option<i64> {
    date_parser::parse(s, local_to_utc)
}

/// Local wall-clock ms (as if UTC) to UTC ms, using the offset in effect before a
/// transition for ambiguous and skipped local times (ICU `UCAL_TZ_LOCAL_FORMER`, as V8).
fn local_to_utc(local_ms: i64) -> i64 {
    local_ms - local_offset_ms(local_ms)
}

fn local_offset_ms(local_ms: i64) -> i64 {
    // chrono covers years -262143..=262142; clamp for the offset lookup only.
    const LIMIT: i64 = 8_200_000_000_000_000;
    let clamped = local_ms.clamp(-LIMIT, LIMIT);
    let Some(naive) = chrono::DateTime::from_timestamp_millis(clamped).map(|d| d.naive_utc()) else {
        return 0;
    };
    let offset_at = |utc: NaiveDateTime| i64::from(Local.offset_from_utc_datetime(&utc).fix().local_minus_utc()) * 1000;
    // An offset is valid for this wall time when the instant it implies really has it.
    // (chrono reports the transition instant itself as ambiguous, e.g. 02:00 on a
    // fall-back night in New York, although only one reading exists.)
    let valid = |o: i64| offset_at(naive - chrono::Duration::milliseconds(o)) == o;
    let mut candidates: Vec<i64> = match Local.offset_from_local_datetime(&naive) {
        LocalResult::Single(o) => vec![i64::from(o.fix().local_minus_utc()) * 1000],
        LocalResult::Ambiguous(a, b) => {
            vec![
                i64::from(a.fix().local_minus_utc()) * 1000,
                i64::from(b.fix().local_minus_utc()) * 1000,
            ]
        }
        LocalResult::None => Vec::new(),
    };
    let day = chrono::Duration::days(1);
    candidates.extend([offset_at(naive - day), offset_at(naive + day)]);
    // Ambiguous wall times use the earlier instant (the larger offset); skipped wall times
    // use the offset in effect before the transition.
    candidates
        .into_iter()
        .filter(|o| valid(*o))
        .max()
        .unwrap_or_else(|| offset_at(naive - day))
}

/// Port of V8's `DateParser` (src/date/dateparser*.{h,cc}) and `ParseDateTimeString`.
mod date_parser {
    const K_NONE: i32 = i32::MAX;
    const MAX_SIGNIFICANT_DIGITS: i32 = 9;

    fn between(x: i32, lo: i32, hi: i32) -> bool {
        (lo..=hi).contains(&x)
    }

    fn is_white_space(c: u32) -> bool {
        matches!(
            c,
            0x09 | 0x0B | 0x0C | 0x20 | 0xA0 | 0x1680 | 0x2000..=0x200A | 0x202F | 0x205F | 0x3000 | 0xFEFF
        )
    }

    fn is_line_terminator(c: u32) -> bool {
        matches!(c, 0x0A | 0x0D | 0x2028 | 0x2029)
    }

    struct InputReader<'a> {
        index: usize,
        buffer: &'a [u16],
        ch: u32,
    }

    impl<'a> InputReader<'a> {
        fn new(buffer: &'a [u16]) -> Self {
            let mut r = InputReader {
                index: 0,
                buffer,
                ch: 0,
            };
            r.next();
            r
        }

        fn position(&self) -> usize {
            self.index
        }

        fn next(&mut self) {
            self.ch = if self.index < self.buffer.len() {
                u32::from(self.buffer[self.index])
            } else {
                0
            };
            self.index += 1;
        }

        fn read_unsigned_numeral(&mut self) -> i32 {
            let mut n: i32 = 0;
            let mut i = 0;
            while self.ch == u32::from(b'0') {
                self.next();
            }
            while self.is_ascii_digit() {
                if i < MAX_SIGNIFICANT_DIGITS {
                    n = n * 10 + (self.ch - u32::from(b'0')) as i32;
                }
                i += 1;
                self.next();
            }
            n
        }

        fn read_word(&mut self, prefix: &mut [u32; 3]) -> usize {
            let mut len = 0;
            while self.is_ascii_alpha_or_above() && !self.is_white_space_char() {
                if len < prefix.len() {
                    prefix[len] = self.ch | 0x20;
                }
                self.next();
                len += 1;
            }
            for slot in prefix.iter_mut().skip(len) {
                *slot = 0;
            }
            len
        }

        fn skip(&mut self, c: u8) -> bool {
            if self.ch == u32::from(c) {
                self.next();
                return true;
            }
            false
        }

        fn skip_white_space(&mut self) -> bool {
            if is_white_space(self.ch) || is_line_terminator(self.ch) {
                self.next();
                return true;
            }
            false
        }

        fn skip_parentheses(&mut self) -> bool {
            if self.ch != u32::from(b'(') {
                return false;
            }
            let mut balance = 0;
            loop {
                if self.ch == u32::from(b')') {
                    balance -= 1;
                } else if self.ch == u32::from(b'(') {
                    balance += 1;
                }
                self.next();
                if !(balance > 0 && self.ch != 0) {
                    break;
                }
            }
            true
        }

        fn is_end(&self) -> bool {
            self.ch == 0
        }

        fn is_ascii_digit(&self) -> bool {
            (u32::from(b'0')..=u32::from(b'9')).contains(&self.ch)
        }

        fn is_ascii_alpha_or_above(&self) -> bool {
            self.ch >= u32::from(b'A')
        }

        fn is_white_space_char(&self) -> bool {
            is_white_space(self.ch)
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum KeywordType {
        Invalid,
        MonthName,
        TimeZoneName,
        TimeSeparator,
        AmPm,
    }

    const KEYWORDS: [([u8; 3], KeywordType, i32); 27] = [
        (*b"jan", KeywordType::MonthName, 1),
        (*b"feb", KeywordType::MonthName, 2),
        (*b"mar", KeywordType::MonthName, 3),
        (*b"apr", KeywordType::MonthName, 4),
        (*b"may", KeywordType::MonthName, 5),
        (*b"jun", KeywordType::MonthName, 6),
        (*b"jul", KeywordType::MonthName, 7),
        (*b"aug", KeywordType::MonthName, 8),
        (*b"sep", KeywordType::MonthName, 9),
        (*b"oct", KeywordType::MonthName, 10),
        (*b"nov", KeywordType::MonthName, 11),
        (*b"dec", KeywordType::MonthName, 12),
        (*b"am\0", KeywordType::AmPm, 0),
        (*b"pm\0", KeywordType::AmPm, 12),
        (*b"ut\0", KeywordType::TimeZoneName, 0),
        (*b"utc", KeywordType::TimeZoneName, 0),
        (*b"z\0\0", KeywordType::TimeZoneName, 0),
        (*b"gmt", KeywordType::TimeZoneName, 0),
        (*b"cdt", KeywordType::TimeZoneName, -5),
        (*b"cst", KeywordType::TimeZoneName, -6),
        (*b"edt", KeywordType::TimeZoneName, -4),
        (*b"est", KeywordType::TimeZoneName, -5),
        (*b"mdt", KeywordType::TimeZoneName, -6),
        (*b"mst", KeywordType::TimeZoneName, -7),
        (*b"pdt", KeywordType::TimeZoneName, -7),
        (*b"pst", KeywordType::TimeZoneName, -8),
        (*b"t\0\0", KeywordType::TimeSeparator, 0),
    ];

    fn keyword_lookup(pre: &[u32; 3], len: usize) -> (KeywordType, i32) {
        for (word, ty, value) in KEYWORDS.iter() {
            let matches = (0..3).all(|j| pre[j] == u32::from(word[j]));
            // Word longer than keyword is only allowed for month names.
            if matches && (len <= 3 || *ty == KeywordType::MonthName) {
                return (*ty, *value);
            }
        }
        (KeywordType::Invalid, 0)
    }

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Tag {
        Invalid,
        Unknown,
        WhiteSpace,
        Number,
        Symbol,
        EndOfInput,
        Keyword(KeywordType),
    }

    #[derive(Clone, Copy, Debug)]
    struct DateToken {
        tag: Tag,
        length: usize,
        value: i32,
    }

    impl DateToken {
        fn is_invalid(&self) -> bool {
            self.tag == Tag::Invalid
        }
        fn is_number(&self) -> bool {
            self.tag == Tag::Number
        }
        fn is_symbol(&self) -> bool {
            self.tag == Tag::Symbol
        }
        fn is_white_space(&self) -> bool {
            self.tag == Tag::WhiteSpace
        }
        fn is_end_of_input(&self) -> bool {
            self.tag == Tag::EndOfInput
        }
        fn is_keyword(&self) -> bool {
            matches!(self.tag, Tag::Keyword(_))
        }
        fn number(&self) -> i32 {
            self.value
        }
        fn keyword_type(&self) -> KeywordType {
            match self.tag {
                Tag::Keyword(k) => k,
                _ => KeywordType::Invalid,
            }
        }
        fn keyword_value(&self) -> i32 {
            self.value
        }
        fn symbol(&self) -> u8 {
            self.value as u8
        }
        fn is_symbol_char(&self, c: u8) -> bool {
            self.is_symbol() && self.symbol() == c
        }
        fn is_keyword_type(&self, k: KeywordType) -> bool {
            self.tag == Tag::Keyword(k)
        }
        fn is_fixed_length_number(&self, length: usize) -> bool {
            self.is_number() && self.length == length
        }
        fn is_ascii_sign(&self) -> bool {
            self.tag == Tag::Symbol && (self.value == i32::from(b'-') || self.value == i32::from(b'+'))
        }
        fn ascii_sign(&self) -> i32 {
            44 - self.value
        }
        fn is_keyword_z(&self) -> bool {
            self.tag == Tag::Keyword(KeywordType::TimeZoneName) && self.length == 1 && self.value == 0
        }

        fn keyword(ty: KeywordType, value: i32, length: usize) -> Self {
            DateToken {
                tag: Tag::Keyword(ty),
                length,
                value,
            }
        }
        fn number_token(value: i32, length: usize) -> Self {
            DateToken {
                tag: Tag::Number,
                length,
                value,
            }
        }
        fn symbol_token(c: u8) -> Self {
            DateToken {
                tag: Tag::Symbol,
                length: 1,
                value: i32::from(c),
            }
        }
        fn white_space(length: usize) -> Self {
            DateToken {
                tag: Tag::WhiteSpace,
                length,
                value: 0,
            }
        }
        fn end_of_input() -> Self {
            DateToken {
                tag: Tag::EndOfInput,
                length: 0,
                value: -1,
            }
        }
        fn invalid() -> Self {
            DateToken {
                tag: Tag::Invalid,
                length: 0,
                value: -1,
            }
        }
        fn unknown() -> Self {
            DateToken {
                tag: Tag::Unknown,
                length: 1,
                value: -1,
            }
        }
    }

    struct Tokenizer<'a> {
        input: InputReader<'a>,
        next: DateToken,
    }

    impl<'a> Tokenizer<'a> {
        fn new(mut input: InputReader<'a>) -> Self {
            let next = Self::scan(&mut input);
            Tokenizer { input, next }
        }

        fn next(&mut self) -> DateToken {
            let result = self.next;
            self.next = Self::scan(&mut self.input);
            result
        }

        fn peek(&self) -> DateToken {
            self.next
        }

        fn skip_symbol(&mut self, c: u8) -> bool {
            if self.next.is_symbol_char(c) {
                self.next = Self::scan(&mut self.input);
                return true;
            }
            false
        }

        fn scan(input: &mut InputReader<'_>) -> DateToken {
            let pre_pos = input.position();
            if input.is_end() {
                return DateToken::end_of_input();
            }
            if input.is_ascii_digit() {
                let n = input.read_unsigned_numeral();
                let length = input.position() - pre_pos;
                return DateToken::number_token(n, length);
            }
            for c in *b":-+.)" {
                if input.skip(c) {
                    return DateToken::symbol_token(c);
                }
            }
            if input.is_ascii_alpha_or_above() && !input.is_white_space_char() {
                let mut buffer = [0u32; 3];
                let length = input.read_word(&mut buffer);
                let (ty, value) = keyword_lookup(&buffer, length);
                return DateToken::keyword(ty, value, length);
            }
            if input.skip_white_space() {
                return DateToken::white_space(input.position() - pre_pos);
            }
            if input.skip_parentheses() {
                return DateToken::unknown();
            }
            input.next();
            DateToken::unknown()
        }
    }

    struct DayComposer {
        comp: [i32; 3],
        index: usize,
        named_month: i32,
        is_iso_date: bool,
    }

    impl DayComposer {
        fn new() -> Self {
            DayComposer {
                comp: [0; 3],
                index: 0,
                named_month: K_NONE,
                is_iso_date: false,
            }
        }
        fn is_empty(&self) -> bool {
            self.index == 0
        }
        fn add(&mut self, n: i32) -> bool {
            if self.index == 3 {
                return false;
            }
            self.comp[self.index] = n;
            self.index += 1;
            true
        }
        fn set_named_month(&mut self, n: i32) {
            self.named_month = n;
        }
        fn set_iso_date(&mut self) {
            self.is_iso_date = true;
        }
        fn is_month(x: i32) -> bool {
            between(x, 1, 12)
        }
        fn is_day(x: i32) -> bool {
            between(x, 1, 31)
        }

        /// Returns (year, month0, day).
        fn write(&mut self) -> Option<(i32, i32, i32)> {
            if self.index < 1 {
                return None;
            }
            // Day and month defaults to 1.
            while self.index < 3 {
                self.comp[self.index] = 1;
                self.index += 1;
            }

            let mut year = 0; // Default year is 0 (=> 2000) for KJS compatibility.
            let month;
            let day;

            if self.named_month == K_NONE {
                if self.is_iso_date || (self.index == 3 && !Self::is_day(self.comp[0])) {
                    // YMD
                    year = self.comp[0];
                    month = self.comp[1];
                    day = self.comp[2];
                } else {
                    // MD(Y)
                    month = self.comp[0];
                    day = self.comp[1];
                    if self.index == 3 {
                        year = self.comp[2];
                    }
                }
            } else {
                month = self.named_month;
                if self.index == 1 {
                    // MD or DM
                    day = self.comp[0];
                } else if !Self::is_day(self.comp[0]) {
                    // YMD, MYD, or YDM
                    year = self.comp[0];
                    day = self.comp[1];
                } else {
                    // DMY, MDY, or DYM
                    day = self.comp[0];
                    year = self.comp[1];
                }
            }

            if !self.is_iso_date {
                if between(year, 0, 49) {
                    year += 2000;
                } else if between(year, 50, 99) {
                    year += 1900;
                }
            }

            if !Self::is_month(month) || !Self::is_day(day) {
                return None;
            }
            Some((year, month - 1, day))
        }
    }

    struct TimeComposer {
        comp: [i32; 4],
        index: usize,
        hour_offset: i32,
    }

    impl TimeComposer {
        fn new() -> Self {
            TimeComposer {
                comp: [0; 4],
                index: 0,
                hour_offset: K_NONE,
            }
        }
        fn is_empty(&self) -> bool {
            self.index == 0
        }
        fn is_expecting(&self, n: i32) -> bool {
            (self.index == 1 && Self::is_minute(n))
                || (self.index == 2 && Self::is_second(n))
                || (self.index == 3 && Self::is_millisecond(n))
        }
        fn add(&mut self, n: i32) -> bool {
            if self.index < 4 {
                self.comp[self.index] = n;
                self.index += 1;
                true
            } else {
                false
            }
        }
        fn add_final(&mut self, n: i32) -> bool {
            if !self.add(n) {
                return false;
            }
            while self.index < 4 {
                self.comp[self.index] = 0;
                self.index += 1;
            }
            true
        }
        fn set_hour_offset(&mut self, n: i32) {
            self.hour_offset = n;
        }
        fn is_minute(x: i32) -> bool {
            between(x, 0, 59)
        }
        fn is_hour(x: i32) -> bool {
            between(x, 0, 23)
        }
        fn is_second(x: i32) -> bool {
            between(x, 0, 59)
        }
        fn is_hour12(x: i32) -> bool {
            between(x, 0, 12)
        }
        fn is_millisecond(x: i32) -> bool {
            between(x, 0, 999)
        }

        fn write(&mut self) -> Option<(i32, i32, i32, i32)> {
            // All time slots default to 0
            while self.index < 4 {
                self.comp[self.index] = 0;
                self.index += 1;
            }
            let [mut hour, minute, second, millisecond] = self.comp;

            if self.hour_offset != K_NONE {
                if !Self::is_hour12(hour) {
                    return None;
                }
                hour %= 12;
                hour += self.hour_offset;
            }

            if (!Self::is_hour(hour)
                || !Self::is_minute(minute)
                || !Self::is_second(second)
                || !Self::is_millisecond(millisecond))
                // A 24th hour is allowed if minutes, seconds, and milliseconds are 0
                && (hour != 24 || minute != 0 || second != 0 || millisecond != 0)
            {
                return None;
            }
            Some((hour, minute, second, millisecond))
        }
    }

    struct TimeZoneComposer {
        sign: i32,
        hour: i32,
        minute: i32,
    }

    impl TimeZoneComposer {
        fn new() -> Self {
            TimeZoneComposer {
                sign: K_NONE,
                hour: K_NONE,
                minute: K_NONE,
            }
        }
        fn set(&mut self, offset_in_hours: i32) {
            self.sign = if offset_in_hours < 0 { -1 } else { 1 };
            self.hour = offset_in_hours * self.sign;
            self.minute = 0;
        }
        fn set_sign(&mut self, sign: i32) {
            self.sign = if sign < 0 { -1 } else { 1 };
        }
        fn set_absolute_hour(&mut self, hour: i32) {
            self.hour = hour;
        }
        fn set_absolute_minute(&mut self, minute: i32) {
            self.minute = minute;
        }
        fn is_expecting(&self, n: i32) -> bool {
            self.hour != K_NONE && self.minute == K_NONE && TimeComposer::is_minute(n)
        }
        fn is_utc(&self) -> bool {
            self.hour == 0 && self.minute == 0
        }
        fn is_empty(&self) -> bool {
            self.hour == K_NONE
        }

        /// `Some(None)` = local time, `Some(Some(seconds))` = UTC offset.
        fn write(&mut self) -> Option<Option<i64>> {
            if self.sign != K_NONE {
                if self.hour == K_NONE {
                    self.hour = 0;
                }
                if self.minute == K_NONE {
                    self.minute = 0;
                }
                let total = (self.hour as u32)
                    .wrapping_mul(3600)
                    .wrapping_add((self.minute as u32).wrapping_mul(60));
                // Smi::kMaxValue on 64-bit Node builds (31-bit Smis are not used).
                if total > i32::MAX as u32 {
                    return None;
                }
                let mut total = i64::from(total);
                if self.sign < 0 {
                    total = -total;
                }
                Some(Some(total))
            } else {
                Some(None)
            }
        }
    }

    fn read_milliseconds(token: DateToken) -> i32 {
        // Read first three significant digits of the original numeral,
        // as inferred from the value and the number of digits.
        // I.e., use the number of digits to see if there were
        // leading zeros.
        let mut number = token.number();
        let mut length = token.length as i32;
        if length < 3 {
            // Less than three digits. Multiply to put most significant digit
            // in hundreds position.
            if length == 1 {
                number *= 100;
            } else if length == 2 {
                number *= 10;
            }
        } else if length > 3 {
            if length > MAX_SIGNIFICANT_DIGITS {
                length = MAX_SIGNIFICANT_DIGITS;
            }
            // More than three digits. Divide by 10^(length - 3) to get three
            // most significant digits.
            let mut factor = 1;
            loop {
                factor *= 10;
                length -= 1;
                if length <= 3 {
                    break;
                }
            }
            number /= factor;
        }
        number
    }

    fn parse_es5_date_time(
        scanner: &mut Tokenizer<'_>,
        day: &mut DayComposer,
        time: &mut TimeComposer,
        tz: &mut TimeZoneComposer,
    ) -> DateToken {
        // Parse mandatory date: [('-'|'+')yy]yyyy[':'MM[':'DD]]
        if scanner.peek().is_ascii_sign() {
            // Keep the sign token, so invalid dates can be detected later.
            let sign_token = scanner.next();
            if !scanner.peek().is_fixed_length_number(6) {
                return sign_token;
            }
            let sign = sign_token.ascii_sign();
            let year = scanner.next().number();
            if sign < 0 && year == 0 {
                return sign_token;
            }
            day.add(sign * year);
        } else if scanner.peek().is_fixed_length_number(4) {
            day.add(scanner.next().number());
        } else {
            return scanner.next();
        }
        if scanner.skip_symbol(b'-') {
            if !scanner.peek().is_fixed_length_number(2) || !DayComposer::is_month(scanner.peek().number()) {
                return scanner.next();
            }
            day.add(scanner.next().number());
            if scanner.skip_symbol(b'-') {
                if !scanner.peek().is_fixed_length_number(2) || !DayComposer::is_day(scanner.peek().number()) {
                    return scanner.next();
                }
                day.add(scanner.next().number());
            }
        }
        // Check for optional time: 'T'HH':'mm[':'ss['.'sss]]Z
        if !scanner.peek().is_keyword_type(KeywordType::TimeSeparator) {
            if !scanner.peek().is_end_of_input() {
                return scanner.next();
            }
        } else {
            // ES5 Date Time String time part is present.
            scanner.next();
            if !scanner.peek().is_fixed_length_number(2) || !between(scanner.peek().number(), 0, 24) {
                return DateToken::invalid();
            }
            // Allow 24:00[:00[.000]], but no other time starting with 24.
            let hour_is_24 = scanner.peek().number() == 24;
            time.add(scanner.next().number());
            if !scanner.skip_symbol(b':') {
                return DateToken::invalid();
            }
            if !scanner.peek().is_fixed_length_number(2)
                || !TimeComposer::is_minute(scanner.peek().number())
                || (hour_is_24 && scanner.peek().number() > 0)
            {
                return DateToken::invalid();
            }
            time.add(scanner.next().number());
            if scanner.skip_symbol(b':') {
                if !scanner.peek().is_fixed_length_number(2)
                    || !TimeComposer::is_second(scanner.peek().number())
                    || (hour_is_24 && scanner.peek().number() > 0)
                {
                    return DateToken::invalid();
                }
                time.add(scanner.next().number());
                if scanner.skip_symbol(b'.') {
                    if !scanner.peek().is_number() || (hour_is_24 && scanner.peek().number() > 0) {
                        return DateToken::invalid();
                    }
                    // Allow more or less than the mandated three digits.
                    time.add(read_milliseconds(scanner.next()));
                }
            }
            // Check for optional timezone designation: 'Z' | ('+'|'-')hh':'mm
            if scanner.peek().is_keyword_z() {
                scanner.next();
                tz.set(0);
            } else if scanner.peek().is_symbol_char(b'+') || scanner.peek().is_symbol_char(b'-') {
                tz.set_sign(if scanner.next().symbol() == b'+' { 1 } else { -1 });
                if scanner.peek().is_fixed_length_number(4) {
                    // hhmm extension syntax.
                    let hourmin = scanner.next().number();
                    let hour = hourmin / 100;
                    let min = hourmin % 100;
                    if !TimeComposer::is_hour(hour) || !TimeComposer::is_minute(min) {
                        return DateToken::invalid();
                    }
                    tz.set_absolute_hour(hour);
                    tz.set_absolute_minute(min);
                } else {
                    if !scanner.peek().is_fixed_length_number(2) || !TimeComposer::is_hour(scanner.peek().number()) {
                        return DateToken::invalid();
                    }
                    tz.set_absolute_hour(scanner.next().number());
                    if !scanner.skip_symbol(b':') {
                        return DateToken::invalid();
                    }
                    if !scanner.peek().is_fixed_length_number(2) || !TimeComposer::is_minute(scanner.peek().number()) {
                        return DateToken::invalid();
                    }
                    tz.set_absolute_minute(scanner.next().number());
                }
            }
            if !scanner.peek().is_end_of_input() {
                return DateToken::invalid();
            }
        }
        // Successfully parsed ES5 Date Time String.
        // ES#sec-date-time-string-format Date Time String Format
        // "When the time zone offset is absent, date-only forms are interpreted
        //  as a UTC time and date-time forms are interpreted as a local time."
        if tz.is_empty() && time.is_empty() {
            tz.set(0);
        }
        day.set_iso_date();
        DateToken::end_of_input()
    }

    /// Output: (year, month0, day, hour, minute, second, ms, utc offset seconds or None).
    type Parsed = (i32, i32, i32, i32, i32, i32, i32, Option<i64>);

    fn parse_components(units: &[u16]) -> Option<Parsed> {
        let mut scanner = Tokenizer::new(InputReader::new(units));
        let mut tz = TimeZoneComposer::new();
        let mut time = TimeComposer::new();
        let mut day = DayComposer::new();

        // First try getting as far as possible with trying to parse as an
        // ES5 Date Time String.
        let next_unhandled_token = parse_es5_date_time(&mut scanner, &mut day, &mut time, &mut tz);
        if next_unhandled_token.is_invalid() {
            return None;
        }
        let mut has_read_number = !day.is_empty();
        // If there's anything left, continue with the legacy parser.
        let mut token = next_unhandled_token;
        while !token.is_end_of_input() {
            if token.is_number() {
                has_read_number = true;
                let n = token.number();
                if scanner.skip_symbol(b':') {
                    if scanner.skip_symbol(b':') {
                        // n + "::"
                        if !time.is_empty() {
                            return None;
                        }
                        time.add(n);
                        time.add(0);
                    } else {
                        // n + ":"
                        if !time.add(n) {
                            return None;
                        }
                        if scanner.peek().is_symbol_char(b'.') {
                            scanner.next();
                        }
                    }
                } else if scanner.skip_symbol(b'.') && time.is_expecting(n) {
                    time.add(n);
                    if !scanner.peek().is_number() {
                        return None;
                    }
                    let ms = read_milliseconds(scanner.next());
                    if ms < 0 {
                        return None;
                    }
                    time.add_final(ms);
                } else if tz.is_expecting(n) {
                    tz.set_absolute_minute(n);
                } else if time.is_expecting(n) {
                    time.add_final(n);
                    // Require end, white space, "Z", "+" or "-" immediately after
                    // finalizing time.
                    let peek = scanner.peek();
                    if !peek.is_end_of_input()
                        && !peek.is_white_space()
                        && !peek.is_keyword_z()
                        && !peek.is_ascii_sign()
                    {
                        return None;
                    }
                } else {
                    if !day.add(n) {
                        return None;
                    }
                    scanner.skip_symbol(b'-');
                }
            } else if token.is_keyword() {
                if token.keyword_type() == KeywordType::AmPm && !time.is_empty() {
                    time.set_hour_offset(token.keyword_value());
                } else if token.keyword_type() == KeywordType::MonthName {
                    day.set_named_month(token.keyword_value());
                    scanner.skip_symbol(b'-');
                } else if token.keyword_type() == KeywordType::TimeZoneName && has_read_number {
                    tz.set(token.keyword_value());
                } else {
                    // Garbage words are illegal if a number has been read.
                    if has_read_number {
                        return None;
                    }
                    // The first number has to be separated from garbage words by
                    // whitespace or other separators.
                    if scanner.peek().is_number() {
                        return None;
                    }
                }
            } else if token.is_ascii_sign() && (tz.is_utc() || !time.is_empty()) {
                // Parse UTC offset (only after UTC or time).
                tz.set_sign(token.ascii_sign());
                // The following number may be empty.
                let mut n = 0;
                let mut length = 0;
                if scanner.peek().is_number() {
                    let next_token = scanner.next();
                    length = next_token.length;
                    n = next_token.number();
                }
                has_read_number = true;

                if scanner.peek().is_symbol_char(b':') {
                    tz.set_absolute_hour(n);
                    tz.set_absolute_minute(K_NONE);
                } else if length == 2 || length == 1 {
                    // Handle time zones like GMT-8
                    tz.set_absolute_hour(n);
                    tz.set_absolute_minute(0);
                } else if length == 4 || length == 3 {
                    // Handle time zones like GMT-0800
                    tz.set_absolute_hour(n / 100);
                    tz.set_absolute_minute(n % 100);
                } else {
                    // No need to accept time zones like GMT-12345
                    return None;
                }
            } else if (token.is_ascii_sign() || token.is_symbol_char(b')')) && has_read_number {
                // Extra sign or ')' is illegal if a number has been read.
                return None;
            } else {
                // Ignore other characters and whitespace.
            }
            token = scanner.next();
        }

        let (y, mo, d) = day.write()?;
        let (h, mi, s, ms) = time.write()?;
        let offset = tz.write()?;
        Some((y, mo, d, h, mi, s, ms, offset))
    }

    /// ECMA-262 MakeDay with V8's year range check (kMinYear/kMaxYear = ±1e6).
    fn make_day(year: i32, month0: i32, date: i32) -> Option<i64> {
        if !(-1_000_000..=1_000_000).contains(&year) {
            return None;
        }
        let mut y = i64::from(year) + i64::from(month0) / 12;
        let mut m = i64::from(month0) % 12;
        if m < 0 {
            m += 12;
            y -= 1;
        }
        Some(super::days_from_civil(y, m + 1, 1) + i64::from(date) - 1)
    }

    /// `ParseDateTimeString` (builtins-date.cc). `local_to_utc` converts local wall time.
    pub(super) fn parse(s: &str, local_to_utc: fn(i64) -> i64) -> Option<i64> {
        let units: Vec<u16> = s.encode_utf16().collect();
        let (y, mo, d, h, mi, sec, ms, offset) = parse_components(&units)?;
        let day = make_day(y, mo, d)?;
        let time = i64::from(h) * 3_600_000 + i64::from(mi) * 60_000 + i64::from(sec) * 1000 + i64::from(ms);
        let mut date = day * super::MS_PER_DAY + time;
        match offset {
            None => {
                if !(-super::MAX_TIME_BEFORE_UTC_MS..=super::MAX_TIME_BEFORE_UTC_MS).contains(&date) {
                    return None;
                }
                date = local_to_utc(date);
            }
            Some(off) => {
                date -= off * 1000;
                if !(-super::MAX_TIME_MS..=super::MAX_TIME_MS).contains(&date) {
                    return None;
                }
            }
        }
        // TimeClip
        if !(-super::MAX_TIME_MS..=super::MAX_TIME_MS).contains(&date) {
            return None;
        }
        Some(date)
    }
}

// ---------------------------------------------------------------------------------------
// Timers

struct TimerState {
    cleared: AtomicBool,
    done: AtomicBool,
    abort: Mutex<Option<tokio::task::AbortHandle>>,
}

impl TimerState {
    fn new() -> Arc<Self> {
        let state = Arc::new(TimerState {
            cleared: AtomicBool::new(false),
            done: AtomicBool::new(false),
            abort: Mutex::new(None),
        });
        TIMERS.with(|t| {
            let mut t = t.borrow_mut();
            if t.len() >= 64 {
                t.retain(|w| w.upgrade().is_some_and(|s| s.is_pending()));
            }
            t.push(Arc::downgrade(&state));
        });
        state
    }

    fn is_pending(&self) -> bool {
        !self.cleared.load(Ordering::SeqCst) && !self.done.load(Ordering::SeqCst)
    }

    fn clear(&self) {
        self.cleared.store(true, Ordering::SeqCst);
        if let Some(h) = self.abort.lock().unwrap().take() {
            h.abort();
        }
    }

    fn set_abort(&self, h: tokio::task::AbortHandle) {
        let mut slot = self.abort.lock().unwrap();
        if self.cleared.load(Ordering::SeqCst) {
            h.abort();
        } else {
            *slot = Some(h);
        }
    }
}

/// Node coerces delays outside `1..=TIMEOUT_MAX` to 1 ms; 0 stays 0 here (fake-timer
/// semantics; a real 0 ms tokio sleep still yields to the scheduler first).
fn normalize_delay(ms: u64) -> u64 {
    if ms > TIMEOUT_MAX { 1 } else { ms }
}

/// Handle returned by [`set_timeout`]. Dropping it does not cancel the timer (as in JS).
#[derive(Clone)]
pub struct Timeout {
    state: Arc<TimerState>,
}

impl Timeout {
    /// `clearTimeout(t)`.
    pub fn clear(&self) {
        self.state.clear();
    }

    /// `t.unref()`: no-op (tokio never waits for timer tasks).
    pub fn unref(&self) -> &Self {
        self
    }

    /// `t.ref()`: no-op.
    pub fn ref_(&self) -> &Self {
        self
    }

    /// Whether the callback has neither run nor been cleared.
    pub fn is_pending(&self) -> bool {
        self.state.is_pending()
    }
}

/// Handle returned by [`set_interval`]. Dropping it does not cancel the interval.
#[derive(Clone)]
pub struct Interval {
    state: Arc<TimerState>,
}

impl Interval {
    /// `clearInterval(i)`.
    pub fn clear(&self) {
        self.state.clear();
    }

    /// `i.unref()`: no-op.
    pub fn unref(&self) -> &Self {
        self
    }

    /// `i.ref()`: no-op.
    pub fn ref_(&self) -> &Self {
        self
    }

    /// Whether the interval is still active.
    pub fn is_pending(&self) -> bool {
        self.state.is_pending()
    }
}

/// `setTimeout(f, ms)`. Runs on the current tokio runtime (falls back to a std thread
/// outside a runtime). The deadline is fixed when this is called.
pub fn set_timeout(ms: u64, f: impl FnOnce() + Send + 'static) -> Timeout {
    let state = TimerState::new();
    let delay = Duration::from_millis(normalize_delay(ms));
    let st = state.clone();
    let run = move || {
        if st.cleared.load(Ordering::SeqCst) {
            return;
        }
        st.done.store(true, Ordering::SeqCst);
        f();
    };
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            let deadline = tokio::time::Instant::now() + delay;
            let jh = handle.spawn(async move {
                tokio::time::sleep_until(deadline).await;
                run();
            });
            state.set_abort(jh.abort_handle());
        }
        Err(_) => {
            // Outside a runtime (sync callers) a plain thread provides the delay.
            std::thread::spawn(move || {
                std::thread::sleep(delay);
                run();
            });
        }
    }
    Timeout { state }
}

/// `setInterval(f, ms)`.
///
/// PORT: missed ticks are delivered in a burst (tokio `MissedTickBehavior::Burst`), so
/// `advance(3500ms)` on a 1000 ms interval runs `f` three times, like vitest fake timers.
/// Node itself reschedules from the time the callback ran.
pub fn set_interval(ms: u64, mut f: impl FnMut() + Send + 'static) -> Interval {
    let state = TimerState::new();
    let period = Duration::from_millis(normalize_delay(ms).max(1));
    let st = state.clone();
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            let start = tokio::time::Instant::now() + period;
            let jh = handle.spawn(async move {
                let mut iv = tokio::time::interval_at(start, period);
                iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
                loop {
                    iv.tick().await;
                    if st.cleared.load(Ordering::SeqCst) {
                        break;
                    }
                    f();
                }
            });
            state.set_abort(jh.abort_handle());
        }
        Err(_) => {
            std::thread::spawn(move || {
                loop {
                    std::thread::sleep(period);
                    if st.cleared.load(Ordering::SeqCst) {
                        break;
                    }
                    f();
                }
            });
        }
    }
    Interval { state }
}

fn abort_error(signal: &AbortSignal) -> Error {
    Error::Abort(signal.reason().unwrap_or_else(AbortReason::abort))
}

/// `await sleep(ms, signal)`: resolves after `ms`, or fails with `Error::Abort(reason)` as
/// soon as `signal` aborts (immediately if it is already aborted).
pub async fn sleep(ms: u64, signal: Option<&AbortSignal>) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(normalize_delay(ms));
    match signal {
        None => {
            tokio::time::sleep_until(deadline).await;
            Ok(())
        }
        Some(signal) => {
            if signal.aborted() {
                return Err(abort_error(signal));
            }
            tokio::select! {
                biased;
                _ = signal.cancelled() => Err(abort_error(signal)),
                _ = tokio::time::sleep_until(deadline) => Ok(()),
            }
        }
    }
}

/// Fake-timer helpers (`vi.setSystemTime`, `vi.getTimerCount`, `vi.clearAllTimers`).
pub mod testing {
    use super::*;

    /// `vi.setSystemTime(ms)`: from now on (on this thread) `now_ms()` returns `ms` plus
    /// the time the tokio clock advances.
    pub fn set_system_time(ms: i64) {
        FAKE_NOW.with(|f| f.set(Some((ms, tokio::time::Instant::now()))));
    }

    /// Freezes the wall clock at the current real time (then it follows the tokio clock).
    pub fn freeze_system_time() {
        set_system_time(real_now_ms());
    }

    /// `vi.useRealTimers()` for the wall clock.
    pub fn clear_system_time() {
        FAKE_NOW.with(|f| f.set(None));
    }

    /// `vi.getTimerCount()`: pending timeouts and intervals created on this thread.
    pub fn timer_count() -> usize {
        TIMERS.with(|t| {
            t.borrow()
                .iter()
                .filter(|w| w.upgrade().is_some_and(|s| s.is_pending()))
                .count()
        })
    }

    /// `vi.clearAllTimers()`: clears every pending timer created on this thread.
    pub fn clear_all_timers() {
        let timers: Vec<Weak<TimerState>> = TIMERS.with(|t| std::mem::take(&mut *t.borrow_mut()));
        for w in timers {
            if let Some(s) = w.upgrade() {
                s.clear();
            }
        }
    }

    /// `Date.parse` with local-time forms interpreted as UTC (what Node prints with
    /// `TZ=UTC`). Lets tests compare against vectors independent of the host time zone.
    pub fn parse_date_utc(s: &str) -> Option<i64> {
        super::date_parser::parse(s, |ms| ms)
    }
}
