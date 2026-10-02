//! pi_js::json (Rust-only; API contract: PORTING.md Appendix A).
//!
//! `JSON.stringify` / `JSON.parse` with V8 (Node 24) semantics:
//!
//! - Numbers print like `Number#toString()` (`1.0` → `1`, `-0` → `0`, `1e21` → `1e+21`); NaN and
//!   ±Infinity serialize as `null`; integers beyond ±2^53 print as the double JS would hold.
//! - Strings escape only `"`, `\`, and control characters below U+0020 (`\b \f \n \r \t`, else
//!   lowercase `\u00xx`); no HTML, `/` or U+2028/U+2029 escaping.
//! - Object keys that are array indices (canonical integers `0..=4294967294`) come first in
//!   ascending numeric order, then the other keys in insertion order, as for every JS object.
//!   This applies to `stringify` output and to objects produced by `parse`.
//! - `parse` reports the exact Node 24 `SyntaxError` messages, accepts unlimited nesting, and
//!   decodes lone surrogate escapes to U+FFFD.

use std::io;

use serde::Serialize;
use serde_json::ser::{CharEscape, Formatter};
use serde_json::{Map, Number, Value};

use crate::error::{Error, Result};
use crate::{num, str16};

/// 2^53: integers up to this magnitude are exact doubles and print as plain integers.
const MAX_EXACT_INT: u64 = 1 << 53;

// ---------------------------------------------------------------------------------------------
// JSON.stringify
// ---------------------------------------------------------------------------------------------

/// `JSON.stringify(v)`.
///
/// Panics only when `v`'s `Serialize` impl fails (e.g. a map with non-string keys), which has
/// no JS equivalent; use [`try_stringify`] to handle that as an error.
pub fn stringify<T: Serialize + ?Sized>(v: &T) -> String {
    try_stringify(v).unwrap_or_else(|e| panic!("pi_js::json::stringify: {e}"))
}

/// `JSON.stringify(v, null, indent)`. Like JS, only the first 10 UTF-16 units of `indent` are
/// used, and an empty `indent` produces the compact form.
///
/// Panics only when `v`'s `Serialize` impl fails; see [`try_stringify_pretty`].
pub fn stringify_pretty<T: Serialize + ?Sized>(v: &T, indent: &str) -> String {
    try_stringify_pretty(v, indent).unwrap_or_else(|e| panic!("pi_js::json::stringify_pretty: {e}"))
}

/// [`stringify`] that reports `Serialize` failures as a `TypeError` instead of panicking.
pub fn try_stringify<T: Serialize + ?Sized>(v: &T) -> Result<String> {
    write_json(v, "")
}

/// [`stringify_pretty`] that reports `Serialize` failures as a `TypeError` instead of panicking.
pub fn try_stringify_pretty<T: Serialize + ?Sized>(v: &T, indent: &str) -> Result<String> {
    let gap = str16::slice(indent, 0, Some(10));
    write_json(v, &gap)
}

/// `Number#toString()` (`String(n)`), the format `JSON.stringify` uses for finite numbers.
pub fn number_to_string(n: f64) -> String {
    num::to_js_string(n)
}

/// The `serde_json::Value` for the JS number `x`, normalized the way [`parse`] stores numbers:
/// integral values within ±2^53 become integer `Number`s (so they compare equal to `json!(1)`
/// and deserialize into integer types), other finite values stay `f64`, and non-finite values
/// become `null` (what `JSON.stringify` writes for them).
pub fn number_value(x: f64) -> Value {
    if !x.is_finite() {
        // PORT: serde_json cannot hold NaN/Infinity; `null` is what JSON.stringify emits.
        return Value::Null;
    }
    if x.trunc() == x && x.abs() <= MAX_EXACT_INT as f64 {
        // -0 is stored as 0 (JSON.stringify prints both as `0`).
        if x >= 0.0 {
            Value::from(x as u64)
        } else {
            Value::from(x as i64)
        }
    } else {
        Number::from_f64(x).map_or(Value::Null, Value::Number)
    }
}

fn write_json<T: Serialize + ?Sized>(v: &T, gap: &str) -> Result<String> {
    let mut out = Vec::with_capacity(128);
    {
        let formatter = JsFormatter {
            out: &mut out,
            gap: gap.as_bytes(),
            depth: 0,
            frames: Vec::new(),
            entries: Vec::new(),
        };
        let mut ser = serde_json::Serializer::with_formatter(NullWriter, formatter);
        v.serialize(&mut ser)
            .map_err(|e| Error::js("TypeError", e.to_string()))?;
    }
    Ok(String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()))
}

/// The serializer's writer. All output goes through [`JsFormatter`], which writes into its own
/// buffer, so this never receives bytes.
struct NullWriter;

impl io::Write for NullWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// One open array or object.
struct Frame {
    has_value: bool,
    /// Index into `JsFormatter::entries` where this object's entries start.
    entries_base: usize,
    /// Whether any key of this object is an array index (then the entries get reordered).
    any_index: bool,
    /// Output offset where the current key's JSON string starts.
    key_start: usize,
}

/// The output range of one object member (`"key":value`, plus the pretty-print line prefix),
/// excluding the separating comma.
struct Entry {
    start: usize,
    end: usize,
    index: Option<u32>,
}

struct JsFormatter<'a> {
    out: &'a mut Vec<u8>,
    gap: &'a [u8],
    depth: usize,
    frames: Vec<Frame>,
    entries: Vec<Entry>,
}

impl JsFormatter<'_> {
    fn newline_indent(&mut self) {
        self.out.push(b'\n');
        for _ in 0..self.depth {
            self.out.extend_from_slice(self.gap);
        }
    }

    fn push_frame(&mut self) {
        self.frames.push(Frame {
            has_value: false,
            entries_base: self.entries.len(),
            any_index: false,
            key_start: 0,
        });
        self.depth += 1;
    }

    fn write_integer(&mut self, negative: bool, magnitude: u128) {
        if magnitude <= u128::from(MAX_EXACT_INT) {
            if negative && magnitude != 0 {
                self.out.push(b'-');
            }
            self.out.extend_from_slice(magnitude.to_string().as_bytes());
        } else {
            // Beyond 2^53 a JS number is a rounded double; print that double.
            let f = magnitude as f64;
            let f = if negative { -f } else { f };
            self.out.extend_from_slice(ryu_js::Buffer::new().format(f).as_bytes());
        }
    }

    /// Moves the array-index members of the object whose entries are `entries[base..]` in front
    /// of the others, in ascending numeric order (JS own-property order).
    fn reorder(&mut self, base: usize) {
        let entries = &self.entries[base..];
        let (Some(first), Some(last)) = (entries.first(), entries.last()) else {
            return;
        };
        let (region_start, region_end) = (first.start, last.end);
        let mut order: Vec<&Entry> = entries.iter().filter(|e| e.index.is_some()).collect();
        order.sort_by_key(|e| e.index);
        order.extend(entries.iter().filter(|e| e.index.is_none()));
        let mut buf = Vec::with_capacity(region_end - region_start);
        for (i, e) in order.iter().enumerate() {
            if i > 0 {
                buf.push(b',');
            }
            buf.extend_from_slice(&self.out[e.start..e.end]);
        }
        self.out[region_start..region_end].copy_from_slice(&buf);
    }
}

/// Parses a canonical array index (`0..=4294967294`, no leading zeros, no sign).
fn array_index(key: &[u8]) -> Option<u32> {
    if key.is_empty() || key.len() > 10 || !key.iter().all(u8::is_ascii_digit) {
        return None;
    }
    if key[0] == b'0' {
        return if key.len() == 1 { Some(0) } else { None };
    }
    let v = key.iter().fold(0u64, |acc, &d| acc * 10 + u64::from(d - b'0'));
    if v <= 4_294_967_294 { Some(v as u32) } else { None }
}

impl Formatter for JsFormatter<'_> {
    fn write_null<W: ?Sized + io::Write>(&mut self, _w: &mut W) -> io::Result<()> {
        self.out.extend_from_slice(b"null");
        Ok(())
    }

    fn write_bool<W: ?Sized + io::Write>(&mut self, _w: &mut W, value: bool) -> io::Result<()> {
        self.out.extend_from_slice(if value { b"true" } else { b"false" });
        Ok(())
    }

    fn write_i8<W: ?Sized + io::Write>(&mut self, _w: &mut W, value: i8) -> io::Result<()> {
        self.write_integer(value < 0, u128::from(value.unsigned_abs()));
        Ok(())
    }

    fn write_i16<W: ?Sized + io::Write>(&mut self, _w: &mut W, value: i16) -> io::Result<()> {
        self.write_integer(value < 0, u128::from(value.unsigned_abs()));
        Ok(())
    }

    fn write_i32<W: ?Sized + io::Write>(&mut self, _w: &mut W, value: i32) -> io::Result<()> {
        self.write_integer(value < 0, u128::from(value.unsigned_abs()));
        Ok(())
    }

    fn write_i64<W: ?Sized + io::Write>(&mut self, _w: &mut W, value: i64) -> io::Result<()> {
        self.write_integer(value < 0, u128::from(value.unsigned_abs()));
        Ok(())
    }

    fn write_i128<W: ?Sized + io::Write>(&mut self, _w: &mut W, value: i128) -> io::Result<()> {
        self.write_integer(value < 0, value.unsigned_abs());
        Ok(())
    }

    fn write_u8<W: ?Sized + io::Write>(&mut self, _w: &mut W, value: u8) -> io::Result<()> {
        self.write_integer(false, u128::from(value));
        Ok(())
    }

    fn write_u16<W: ?Sized + io::Write>(&mut self, _w: &mut W, value: u16) -> io::Result<()> {
        self.write_integer(false, u128::from(value));
        Ok(())
    }

    fn write_u32<W: ?Sized + io::Write>(&mut self, _w: &mut W, value: u32) -> io::Result<()> {
        self.write_integer(false, u128::from(value));
        Ok(())
    }

    fn write_u64<W: ?Sized + io::Write>(&mut self, _w: &mut W, value: u64) -> io::Result<()> {
        self.write_integer(false, u128::from(value));
        Ok(())
    }

    fn write_u128<W: ?Sized + io::Write>(&mut self, _w: &mut W, value: u128) -> io::Result<()> {
        self.write_integer(false, value);
        Ok(())
    }

    fn write_f32<W: ?Sized + io::Write>(&mut self, _w: &mut W, value: f32) -> io::Result<()> {
        // JS has no f32: the value is the widened double.
        self.out
            .extend_from_slice(ryu_js::Buffer::new().format(f64::from(value)).as_bytes());
        Ok(())
    }

    fn write_f64<W: ?Sized + io::Write>(&mut self, _w: &mut W, value: f64) -> io::Result<()> {
        self.out
            .extend_from_slice(ryu_js::Buffer::new().format(value).as_bytes());
        Ok(())
    }

    fn write_number_str<W: ?Sized + io::Write>(&mut self, _w: &mut W, value: &str) -> io::Result<()> {
        self.out.extend_from_slice(value.as_bytes());
        Ok(())
    }

    fn begin_string<W: ?Sized + io::Write>(&mut self, _w: &mut W) -> io::Result<()> {
        self.out.push(b'"');
        Ok(())
    }

    fn end_string<W: ?Sized + io::Write>(&mut self, _w: &mut W) -> io::Result<()> {
        self.out.push(b'"');
        Ok(())
    }

    fn write_string_fragment<W: ?Sized + io::Write>(&mut self, _w: &mut W, fragment: &str) -> io::Result<()> {
        self.out.extend_from_slice(fragment.as_bytes());
        Ok(())
    }

    fn write_char_escape<W: ?Sized + io::Write>(&mut self, _w: &mut W, char_escape: CharEscape) -> io::Result<()> {
        const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
        let escaped: &[u8] = match char_escape {
            CharEscape::Quote => b"\\\"",
            CharEscape::ReverseSolidus => b"\\\\",
            CharEscape::Solidus => b"\\/",
            CharEscape::Backspace => b"\\b",
            CharEscape::FormFeed => b"\\f",
            CharEscape::LineFeed => b"\\n",
            CharEscape::CarriageReturn => b"\\r",
            CharEscape::Tab => b"\\t",
            CharEscape::AsciiControl(byte) => {
                let bytes = [
                    b'\\',
                    b'u',
                    b'0',
                    b'0',
                    HEX_DIGITS[(byte >> 4) as usize],
                    HEX_DIGITS[(byte & 0xF) as usize],
                ];
                self.out.extend_from_slice(&bytes);
                return Ok(());
            }
        };
        self.out.extend_from_slice(escaped);
        Ok(())
    }

    fn write_byte_array<W: ?Sized + io::Write>(&mut self, w: &mut W, value: &[u8]) -> io::Result<()> {
        self.begin_array(w)?;
        for (i, byte) in value.iter().enumerate() {
            self.begin_array_value(w, i == 0)?;
            self.write_u8(w, *byte)?;
            self.end_array_value(w)?;
        }
        self.end_array(w)
    }

    fn begin_array<W: ?Sized + io::Write>(&mut self, _w: &mut W) -> io::Result<()> {
        self.push_frame();
        self.out.push(b'[');
        Ok(())
    }

    fn end_array<W: ?Sized + io::Write>(&mut self, _w: &mut W) -> io::Result<()> {
        let frame = self.frames.pop();
        self.depth = self.depth.saturating_sub(1);
        if frame.is_some_and(|f| f.has_value) && !self.gap.is_empty() {
            self.newline_indent();
        }
        self.out.push(b']');
        Ok(())
    }

    fn begin_array_value<W: ?Sized + io::Write>(&mut self, _w: &mut W, first: bool) -> io::Result<()> {
        if !first {
            self.out.push(b',');
        }
        if !self.gap.is_empty() {
            self.newline_indent();
        }
        Ok(())
    }

    fn end_array_value<W: ?Sized + io::Write>(&mut self, _w: &mut W) -> io::Result<()> {
        if let Some(f) = self.frames.last_mut() {
            f.has_value = true;
        }
        Ok(())
    }

    fn begin_object<W: ?Sized + io::Write>(&mut self, _w: &mut W) -> io::Result<()> {
        self.push_frame();
        self.out.push(b'{');
        Ok(())
    }

    fn end_object<W: ?Sized + io::Write>(&mut self, _w: &mut W) -> io::Result<()> {
        let frame = self.frames.pop();
        self.depth = self.depth.saturating_sub(1);
        if let Some(f) = &frame {
            if f.any_index {
                self.reorder(f.entries_base);
            }
            self.entries.truncate(f.entries_base);
        }
        if frame.is_some_and(|f| f.has_value) && !self.gap.is_empty() {
            self.newline_indent();
        }
        self.out.push(b'}');
        Ok(())
    }

    fn begin_object_key<W: ?Sized + io::Write>(&mut self, _w: &mut W, first: bool) -> io::Result<()> {
        if !first {
            self.out.push(b',');
        }
        let start = self.out.len();
        if !self.gap.is_empty() {
            self.newline_indent();
        }
        let key_start = self.out.len();
        if let Some(f) = self.frames.last_mut() {
            f.key_start = key_start;
        }
        self.entries.push(Entry {
            start,
            end: start,
            index: None,
        });
        Ok(())
    }

    fn end_object_key<W: ?Sized + io::Write>(&mut self, _w: &mut W) -> io::Result<()> {
        if let Some(f) = self.frames.last_mut() {
            let key = &self.out[f.key_start..];
            let index = if key.len() >= 2 && key[0] == b'"' && key[key.len() - 1] == b'"' {
                array_index(&key[1..key.len() - 1])
            } else {
                None
            };
            if index.is_some() {
                f.any_index = true;
            }
            if let Some(e) = self.entries.last_mut() {
                e.index = index;
            }
        }
        Ok(())
    }

    fn begin_object_value<W: ?Sized + io::Write>(&mut self, _w: &mut W) -> io::Result<()> {
        self.out.push(b':');
        if !self.gap.is_empty() {
            self.out.push(b' ');
        }
        Ok(())
    }

    fn end_object_value<W: ?Sized + io::Write>(&mut self, _w: &mut W) -> io::Result<()> {
        let end = self.out.len();
        if let Some(e) = self.entries.last_mut() {
            e.end = end;
        }
        if let Some(f) = self.frames.last_mut() {
            f.has_value = true;
        }
        Ok(())
    }

    fn write_raw_fragment<W: ?Sized + io::Write>(&mut self, _w: &mut W, fragment: &str) -> io::Result<()> {
        self.out.extend_from_slice(fragment.as_bytes());
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// JSON.parse
// ---------------------------------------------------------------------------------------------

/// `JSON.parse(s)`.
///
/// Errors are `SyntaxError`s with the exact Node 24 (V8) message, e.g.
/// `Unexpected end of JSON input`, `Unexpected token 'x', "[x]" is not valid JSON`,
/// `Expected ',' or ']' after array element in JSON at position 4 (line 1 column 5)`.
/// Positions, lines and columns count UTF-16 code units.
///
/// Numbers are normalized like [`number_value`]: integral values within ±2^53 are integer
/// `Number`s (`1.0` and `1e3` parse as `1` and `1000`), others `f64`.
/// PORT: numbers that overflow to ±Infinity (e.g. `1e400`) parse as `null`, and lone surrogate
/// escapes (`"\ud800"`) decode to U+FFFD, since neither fits a Rust `Value`.
pub fn parse(s: &str) -> Result<Value> {
    Parser {
        src: s,
        b: s.as_bytes(),
        pos: 0,
    }
    .parse()
}

/// `JSON.parse(s) as T`: [`parse`], then deserialize the value into `T`.
///
/// A value that parses but does not fit `T` is reported as a `TypeError` carrying the serde
/// message (TS performs no such check).
pub fn parse_as<T: serde::de::DeserializeOwned>(s: &str) -> Result<T> {
    let value = parse(s)?;
    serde_json::from_value(value).map_err(|e| Error::js("TypeError", e.to_string()))
}

/// The V8 token class of the character an error points at; it selects the message template.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Token {
    Eos,
    Number,
    String,
    Other,
}

/// Errors that V8 reports with a fixed template plus position, line and column.
#[derive(Clone, Copy)]
enum Msg {
    NoNumberAfterMinusSign,
    UnterminatedFractionalNumber,
    ExponentPartMissingNumber,
    UnterminatedString,
    BadControlCharacter,
    BadEscapedCharacter,
    BadUnicodeEscape,
    ExpectedPropNameOrRBrace,
    ExpectedCommaOrRBrack,
    ExpectedCommaOrRBrace,
    ExpectedColonAfterPropertyName,
    ExpectedDoubleQuotedPropertyName,
    UnexpectedNonWhiteSpaceCharacter,
}

impl Msg {
    fn text(self) -> &'static str {
        match self {
            Msg::NoNumberAfterMinusSign => "No number after minus sign in JSON",
            Msg::UnterminatedFractionalNumber => "Unterminated fractional number in JSON",
            Msg::ExponentPartMissingNumber => "Exponent part is missing a number in JSON",
            Msg::UnterminatedString => "Unterminated string in JSON",
            Msg::BadControlCharacter => "Bad control character in string literal in JSON",
            Msg::BadEscapedCharacter => "Bad escaped character in JSON",
            Msg::BadUnicodeEscape => "Bad Unicode escape in JSON",
            Msg::ExpectedPropNameOrRBrace => "Expected property name or '}' in JSON",
            Msg::ExpectedCommaOrRBrack => "Expected ',' or ']' after array element in JSON",
            Msg::ExpectedCommaOrRBrace => "Expected ',' or '}' after property value in JSON",
            Msg::ExpectedColonAfterPropertyName => "Expected ':' after property name in JSON",
            Msg::ExpectedDoubleQuotedPropertyName => "Expected double-quoted property name in JSON",
            Msg::UnexpectedNonWhiteSpaceCharacter => "Unexpected non-whitespace character after JSON",
        }
    }
}

/// V8 `kMaxContextCharacters`.
const MAX_CONTEXT_CHARACTERS: usize = 10;
/// V8 `kMinOriginalSourceLengthForContext`.
const MIN_ORIGINAL_SOURCE_LENGTH_FOR_CONTEXT: usize = MAX_CONTEXT_CHARACTERS * 2 + 1;

enum Container {
    Array(Vec<Value>),
    Object {
        map: Map<String, Value>,
        key: String,
        any_index: bool,
    },
}

struct Parser<'a> {
    src: &'a str,
    b: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.pos).copied()
    }

    fn skip_whitespace(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.peek() {
            self.pos += 1;
        }
    }

    // ---- error reporting (port of JsonParser::ReportUnexpectedToken) ----

    /// `(line, column)` of byte offset `pos`, counting UTF-16 units; `\r\n`, `\r` and `\n` are
    /// line breaks.
    fn file_location(&self, pos: usize) -> (usize, usize) {
        let mut line = 1;
        let mut last_line_break = 0;
        let mut i = 0;
        while i < pos {
            if self.b[i] == b'\r' && i + 1 < pos && self.b[i + 1] == b'\n' {
                // \r\n counts as a single newline.
                i += 1;
            }
            if self.b[i] == b'\r' || self.b[i] == b'\n' {
                line += 1;
                last_line_break = i + 1;
            }
            i += 1;
        }
        (line, 1 + str16::len(&self.src[last_line_break..pos]))
    }

    fn located(&self, pos: usize, text: &str) -> Error {
        let (line, column) = self.file_location(pos);
        let pos16 = str16::len(&self.src[..pos]);
        Error::js(
            "SyntaxError",
            format!("{text} at position {pos16} (line {line} column {column})"),
        )
    }

    fn error(&self, pos: usize, msg: Msg) -> Error {
        self.located(pos, msg.text())
    }

    /// Whether the whole source is one of the strings V8 special-cases (what `JSON.parse` sees
    /// for `undefined`, `NaN`, `Infinity` and plain objects).
    fn is_special_string(&self) -> bool {
        matches!(self.src, "[object Object]" | "undefined" | "Infinity" | "NaN")
    }

    fn unexpected_token(&self, pos: usize, token: Token) -> Error {
        match token {
            Token::Eos => Error::js("SyntaxError", "Unexpected end of JSON input"),
            Token::Number => self.located(pos, "Unexpected number in JSON"),
            Token::String => self.located(pos, "Unexpected string in JSON"),
            Token::Other => {
                if self.is_special_string() {
                    return Error::js("SyntaxError", format!("\"{}\" is not valid JSON", self.src));
                }
                let pos16 = str16::len(&self.src[..pos]);
                // The single UTF-16 unit at the cursor (half of an astral char becomes U+FFFD).
                let token = str16::char_at(self.src, pos16);
                let length = str16::len(self.src);
                let message = if length < MIN_ORIGINAL_SOURCE_LENGTH_FOR_CONTEXT {
                    format!("Unexpected token '{token}', \"{}\" is not valid JSON", self.src)
                } else if pos16 < MAX_CONTEXT_CHARACTERS {
                    let context = str16::substring(self.src, 0, Some(pos16 + MAX_CONTEXT_CHARACTERS));
                    format!("Unexpected token '{token}', \"{context}\"... is not valid JSON")
                } else if pos16 < length - MAX_CONTEXT_CHARACTERS {
                    let context = str16::substring(
                        self.src,
                        pos16 - MAX_CONTEXT_CHARACTERS,
                        Some(pos16 + MAX_CONTEXT_CHARACTERS),
                    );
                    format!("Unexpected token '{token}', ...\"{context}\"... is not valid JSON")
                } else {
                    let context = str16::substring(self.src, pos16 - MAX_CONTEXT_CHARACTERS, None);
                    format!("Unexpected token '{token}', ...\"{context}\" is not valid JSON")
                };
                Error::js("SyntaxError", message)
            }
        }
    }

    /// Port of `ReportUnexpectedCharacter`: the token class of the character at `pos`.
    fn unexpected_character(&self, pos: usize) -> Error {
        let token = match self.src[pos..].chars().next() {
            None => Token::Eos,
            Some('"') => Token::String,
            Some('-' | '0'..='9') => Token::Number,
            Some(_) => Token::Other,
        };
        self.unexpected_token(pos, token)
    }

    // ---- scanning ----

    /// Port of `ScanLiteral`; the cursor is on the literal's first character.
    fn scan_literal(&mut self, literal: &'static [u8]) -> Result<()> {
        let n = literal.len();
        let remaining = self.b.len() - self.pos;
        if remaining >= n && self.b[self.pos + 1..self.pos + n] == literal[1..] {
            self.pos += n;
            return Ok(());
        }
        self.pos += 1;
        for &expected in literal.iter().skip(1).take((n - 1).min(remaining - 1)) {
            if self.b[self.pos] != expected {
                return Err(self.unexpected_character(self.pos));
            }
            self.pos += 1;
        }
        Err(self.unexpected_token(self.pos, Token::Eos))
    }

    fn skip_digits(&mut self) {
        while let Some(b'0'..=b'9') = self.peek() {
            self.pos += 1;
        }
    }

    /// Port of `ParseJsonNumber`; the cursor is on `-` or a digit.
    fn parse_number(&mut self) -> Result<Value> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        if self.peek() == Some(b'0') {
            // Prefix zero is only allowed if it's the only digit before a decimal point or
            // exponent.
            self.pos += 1;
            if let Some(b'0'..=b'9') = self.peek() {
                return Err(self.unexpected_token(self.pos, Token::Number));
            }
        } else {
            let digits_start = self.pos;
            self.skip_digits();
            if self.pos == digits_start {
                return Err(self.error(self.pos, Msg::NoNumberAfterMinusSign));
            }
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error(self.pos, Msg::UnterminatedFractionalNumber));
            }
            self.skip_digits();
        }
        if let Some(b'e' | b'E') = self.peek() {
            self.pos += 1;
            if let Some(b'-' | b'+') = self.peek() {
                self.pos += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error(self.pos, Msg::ExponentPartMissingNumber));
            }
            self.skip_digits();
        }
        let x: f64 = self.src[start..self.pos].parse().unwrap_or(f64::NAN);
        Ok(number_value(x))
    }

    /// Port of `ScanJsonString`; the cursor is just past the opening quote. Leaves the cursor
    /// past the closing quote.
    fn scan_string(&mut self) -> Result<String> {
        let start = self.pos;
        // Fast path: no escapes.
        loop {
            match self.peek() {
                None => return Err(self.error(self.pos, Msg::UnterminatedString)),
                Some(b'"') => {
                    let s = self.src[start..self.pos].to_string();
                    self.pos += 1;
                    return Ok(s);
                }
                Some(b'\\') => break,
                Some(c) if c < 0x20 => return Err(self.error(self.pos, Msg::BadControlCharacter)),
                Some(_) => self.pos += 1,
            }
        }
        let mut out = String::with_capacity(self.pos - start + 16);
        out.push_str(&self.src[start..self.pos]);
        // A high surrogate from a `\u` escape waiting for its low half.
        let mut pending_high: Option<u16> = None;
        let flush = |out: &mut String, pending: &mut Option<u16>| {
            if pending.take().is_some() {
                out.push('\u{FFFD}');
            }
        };
        let mut run_start = self.pos;
        loop {
            match self.peek() {
                None => return Err(self.error(self.pos, Msg::UnterminatedString)),
                Some(b'"') => {
                    if self.pos > run_start {
                        flush(&mut out, &mut pending_high);
                        out.push_str(&self.src[run_start..self.pos]);
                    }
                    flush(&mut out, &mut pending_high);
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    if self.pos > run_start {
                        flush(&mut out, &mut pending_high);
                        out.push_str(&self.src[run_start..self.pos]);
                    }
                    self.pos += 1;
                    let Some(c) = self.src[self.pos..].chars().next() else {
                        return Err(self.unexpected_token(self.pos, Token::Eos));
                    };
                    if c as u32 > 0xFF {
                        return Err(self.unexpected_character(self.pos));
                    }
                    let simple = match c {
                        '"' => Some('"'),
                        '\\' => Some('\\'),
                        '/' => Some('/'),
                        'b' => Some('\u{0008}'),
                        'f' => Some('\u{000C}'),
                        'n' => Some('\n'),
                        'r' => Some('\r'),
                        't' => Some('\t'),
                        'u' => None,
                        _ => return Err(self.error(self.pos, Msg::BadEscapedCharacter)),
                    };
                    if let Some(ch) = simple {
                        flush(&mut out, &mut pending_high);
                        out.push(ch);
                    } else {
                        let mut value: u32 = 0;
                        for _ in 0..4 {
                            self.pos += 1;
                            let digit = self.peek().and_then(|d| char::from(d).to_digit(16));
                            let Some(digit) = digit else {
                                return Err(self.error(self.pos, Msg::BadUnicodeEscape));
                            };
                            value = value * 16 + digit;
                        }
                        let unit = value as u16;
                        match unit {
                            0xD800..=0xDBFF => {
                                flush(&mut out, &mut pending_high);
                                pending_high = Some(unit);
                            }
                            0xDC00..=0xDFFF => match pending_high.take() {
                                Some(high) => {
                                    let cp = 0x10000 + ((u32::from(high) - 0xD800) << 10) + (u32::from(unit) - 0xDC00);
                                    out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                                }
                                None => out.push('\u{FFFD}'),
                            },
                            _ => {
                                flush(&mut out, &mut pending_high);
                                out.push(char::from_u32(value).unwrap_or('\u{FFFD}'));
                            }
                        }
                    }
                    self.pos += 1;
                    run_start = self.pos;
                }
                Some(c) if c < 0x20 => return Err(self.error(self.pos, Msg::BadControlCharacter)),
                Some(_) => self.pos += 1,
            }
        }
    }

    /// Port of `ParseJsonValue` (iterative, so nesting depth is unlimited) plus the trailing
    /// non-whitespace check of `ParseJson`.
    fn parse(mut self) -> Result<Value> {
        let mut stack: Vec<Container> = Vec::new();
        loop {
            // Produce a value. Starting (but not immediately finishing) objects and arrays
            // continues the loop until a first member is complete.
            let mut value = loop {
                self.skip_whitespace();
                match self.peek() {
                    Some(b'"') => {
                        self.pos += 1;
                        break Value::String(self.scan_string()?);
                    }
                    Some(b'-' | b'0'..=b'9') => break self.parse_number()?,
                    Some(b'{') => {
                        self.pos += 1;
                        self.skip_whitespace();
                        if self.peek() == Some(b'}') {
                            self.pos += 1;
                            break Value::Object(Map::new());
                        }
                        let key = self.expect_property_name(Msg::ExpectedPropNameOrRBrace)?;
                        stack.push(Container::Object {
                            map: Map::new(),
                            key,
                            any_index: false,
                        });
                    }
                    Some(b'[') => {
                        self.pos += 1;
                        self.skip_whitespace();
                        if self.peek() == Some(b']') {
                            self.pos += 1;
                            break Value::Array(Vec::new());
                        }
                        stack.push(Container::Array(Vec::new()));
                    }
                    Some(b't') => {
                        self.scan_literal(b"true")?;
                        break Value::Bool(true);
                    }
                    Some(b'f') => {
                        self.scan_literal(b"false")?;
                        break Value::Bool(false);
                    }
                    Some(b'n') => {
                        self.scan_literal(b"null")?;
                        break Value::Null;
                    }
                    _ => return Err(self.unexpected_character(self.pos)),
                }
            };

            // Consume the produced value; finishing an array or object produces another one.
            loop {
                match stack.last_mut() {
                    None => {
                        self.skip_whitespace();
                        if self.pos < self.b.len() {
                            return Err(self.error(self.pos, Msg::UnexpectedNonWhiteSpaceCharacter));
                        }
                        return Ok(value);
                    }
                    Some(Container::Object { map, key, any_index }) => {
                        let key = std::mem::take(key);
                        if array_index(key.as_bytes()).is_some() {
                            *any_index = true;
                        }
                        map.insert(key, value);
                        self.skip_whitespace();
                        if self.peek() == Some(b',') {
                            self.pos += 1;
                            let next = self.expect_property_name(Msg::ExpectedDoubleQuotedPropertyName)?;
                            if let Some(Container::Object { key, .. }) = stack.last_mut() {
                                *key = next;
                            }
                            break;
                        }
                        if self.peek() != Some(b'}') {
                            return Err(self.error(self.pos, Msg::ExpectedCommaOrRBrace));
                        }
                        self.pos += 1;
                        let Some(Container::Object { map, any_index, .. }) = stack.pop() else {
                            unreachable!("object frame on top of the stack");
                        };
                        value = finish_object(map, any_index);
                    }
                    Some(Container::Array(items)) => {
                        items.push(value);
                        self.skip_whitespace();
                        if self.peek() == Some(b',') {
                            self.pos += 1;
                            break;
                        }
                        if self.peek() != Some(b']') {
                            return Err(self.error(self.pos, Msg::ExpectedCommaOrRBrack));
                        }
                        self.pos += 1;
                        let Some(Container::Array(items)) = stack.pop() else {
                            unreachable!("array frame on top of the stack");
                        };
                        value = Value::Array(items);
                    }
                }
            }
        }
    }

    /// `ExpectNext(STRING, msg)`, the key scan, then `ExpectNext(COLON, …)`.
    fn expect_property_name(&mut self, msg: Msg) -> Result<String> {
        self.skip_whitespace();
        if self.peek() != Some(b'"') {
            return Err(self.error(self.pos, msg));
        }
        self.pos += 1;
        let key = self.scan_string()?;
        self.skip_whitespace();
        if self.peek() != Some(b':') {
            return Err(self.error(self.pos, Msg::ExpectedColonAfterPropertyName));
        }
        self.pos += 1;
        Ok(key)
    }
}

/// Applies JS own-property order: array-index keys first (ascending), then the rest in
/// insertion order.
fn finish_object(map: Map<String, Value>, any_index: bool) -> Value {
    if !any_index {
        return Value::Object(map);
    }
    let mut indexed: Vec<(u32, String, Value)> = Vec::new();
    let mut named: Vec<(String, Value)> = Vec::new();
    for (k, v) in map {
        match array_index(k.as_bytes()) {
            Some(i) => indexed.push((i, k, v)),
            None => named.push((k, v)),
        }
    }
    indexed.sort_by_key(|e| e.0);
    let mut out = Map::new();
    for (_, k, v) in indexed {
        out.insert(k, v);
    }
    for (k, v) in named {
        out.insert(k, v);
    }
    Value::Object(out)
}

/// serde `with` module for `Option<Option<T>>` fields where absent, `null` and a value are all
/// distinct (`x?: T | null`). Use with
/// `#[serde(default, skip_serializing_if = "Option::is_none", with = "pi_js::json::double_option")]`.
pub mod double_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// `None` (absent; normally skipped) and `Some(None)` write `null`; `Some(Some(v))` writes `v`.
    pub fn serialize<T: Serialize, S: Serializer>(value: &Option<Option<T>>, serializer: S) -> Result<S::Ok, S::Error> {
        match value {
            Some(Some(v)) => v.serialize(serializer),
            Some(None) | None => serializer.serialize_none(),
        }
    }

    /// A present field becomes `Some(None)` for `null` and `Some(Some(v))` otherwise; an absent
    /// field stays `None` through `#[serde(default)]`.
    pub fn deserialize<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Option<T>>, D::Error> {
        Option::<T>::deserialize(deserializer).map(Some)
    }
}
