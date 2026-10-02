//! pi_js::b64 (Rust-only; API contract: PORTING.md Appendix A).
//!
//! Node `Buffer` base64 / base64url codecs and the WHATWG `atob` / `btoa` globals.

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};

use crate::error::{Error, Result};

/// `Buffer.from(b).toString("base64")` / `btoa` of bytes: standard alphabet, padded.
pub fn encode(b: &[u8]) -> String {
    STANDARD.encode(b)
}

/// `Buffer.from(b).toString("base64url")`: URL-safe alphabet, no padding.
pub fn encode_url(b: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(b)
}

/// Node's `unbase64` table: both alphabets are accepted; 255 marks every other byte.
fn unbase64(c: u8) -> u8 {
    match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' | b'-' => 62,
        b'/' | b'_' => 63,
        _ => 255,
    }
}

/// `Buffer.from(s, "base64")`: lenient. Both the standard and URL-safe alphabets are accepted,
/// characters outside them (whitespace, garbage) are skipped, decoding stops at the first `=`,
/// and a trailing partial group yields as many whole bytes as it covers. Never fails.
///
/// Like Node's decoder, each UTF-16 code unit is reduced to its low byte before lookup.
pub fn decode(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3 + 3);
    let mut acc: u32 = 0;
    let mut n = 0;
    for unit in s.encode_utf16() {
        let c = unit as u8;
        let v = unbase64(c);
        if v < 64 {
            acc = (acc << 6) | u32::from(v);
            n += 1;
            if n == 4 {
                out.extend_from_slice(&[(acc >> 16) as u8, (acc >> 8) as u8, acc as u8]);
                acc = 0;
                n = 0;
            }
        } else if c == b'=' {
            break;
        }
    }
    match n {
        2 => out.push((acc >> 4) as u8),
        3 => out.extend_from_slice(&[(acc >> 10) as u8, (acc >> 2) as u8]),
        _ => {}
    }
    out
}

/// `Buffer.from(s, "base64url")`: Node uses the same lenient decoder for both encodings.
pub fn decode_url(s: &str) -> Vec<u8> {
    decode(s)
}

fn invalid_character() -> Error {
    Error::js("InvalidCharacterError", "Invalid character")
}

/// ASCII whitespace as skipped by forgiving-base64 (TAB, LF, FF, CR, SPACE).
fn is_ascii_ws(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\u{000C}' | '\r' | ' ')
}

/// `atob(s)` as bytes: WHATWG forgiving-base64 decode (standard alphabet only, ASCII
/// whitespace ignored, at most two trailing `=` that must complete the last group).
///
/// Errors are `InvalidCharacterError` DOMExceptions with Node 24's messages: `Invalid character`
/// or `The string to be decoded is not correctly encoded.` (one dangling character).
pub fn atob_bytes(s: &str) -> Result<Vec<u8>> {
    // Strip trailing whitespace and up to two `=` (whitespace may sit between them).
    let mut body = s.trim_end_matches(is_ascii_ws);
    let mut equal_signs = 0;
    while equal_signs < 2 {
        let Some(stripped) = body.strip_suffix('=') else {
            break;
        };
        equal_signs += 1;
        body = stripped.trim_end_matches(is_ascii_ws);
    }
    let mut out = Vec::with_capacity(body.len() / 4 * 3 + 3);
    let mut acc: u32 = 0;
    let mut n = 0;
    for c in body.chars() {
        if is_ascii_ws(c) {
            continue;
        }
        let v = match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '+' | '/' => unbase64(c as u8),
            _ => return Err(invalid_character()),
        };
        acc = (acc << 6) | u32::from(v);
        n += 1;
        if n == 4 {
            out.extend_from_slice(&[(acc >> 16) as u8, (acc >> 8) as u8, acc as u8]);
            acc = 0;
            n = 0;
        }
    }
    match n {
        1 => {
            return Err(Error::js(
                "InvalidCharacterError",
                "The string to be decoded is not correctly encoded.",
            ));
        }
        2 => out.push((acc >> 4) as u8),
        3 => out.extend_from_slice(&[(acc >> 10) as u8, (acc >> 2) as u8]),
        _ => {}
    }
    if equal_signs > 0 && (out.len() % 3 == 0 || (out.len() % 3) + 1 + equal_signs != 4) {
        return Err(invalid_character());
    }
    Ok(out)
}

/// `atob(s)`: the decoded bytes as a "binary string" (each byte becomes the char with that
/// code, U+0000..U+00FF). See [`atob_bytes`] for the accepted input.
pub fn atob(s: &str) -> Result<String> {
    Ok(atob_bytes(s)?.into_iter().map(char::from).collect())
}

/// `btoa(s)`: base64 of a "binary string". Fails with `InvalidCharacterError: Invalid
/// character` when a char is above U+00FF.
pub fn btoa(s: &str) -> Result<String> {
    let mut bytes = Vec::with_capacity(s.len());
    for c in s.chars() {
        let code = c as u32;
        if code > 0xFF {
            return Err(invalid_character());
        }
        bytes.push(code as u8);
    }
    Ok(encode(&bytes))
}
