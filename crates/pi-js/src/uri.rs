//! pi_js::uri (Rust-only; API contract: PORTING.md Appendix A).
//!
//! The ECMAScript URI functions `encodeURIComponent`, `encodeURI`, `decodeURIComponent` and
//! `decodeURI` (percent-encoding of UTF-8 with uppercase hex; decoding fails with
//! `URIError: URI malformed`).

use crate::error::{Error, Result};

const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";

/// Characters `encodeURIComponent` leaves alone: `A-Z a-z 0-9 - _ . ! ~ * ' ( )`.
fn is_unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')')
}

/// `uriReserved` plus `#`: the extra characters `encodeURI` keeps and `decodeURI` does not
/// decode.
fn is_uri_reserved(b: u8) -> bool {
    matches!(
        b,
        b';' | b'/' | b'?' | b':' | b'@' | b'&' | b'=' | b'+' | b'$' | b',' | b'#'
    )
}

fn encode(s: &str, keep: impl Fn(u8) -> bool) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if keep(b) {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(HEX_UPPER[usize::from(b >> 4)]));
            out.push(char::from(HEX_UPPER[usize::from(b & 0xF)]));
        }
    }
    out
}

/// `encodeURIComponent(s)`.
pub fn encode_uri_component(s: &str) -> String {
    encode(s, is_unreserved)
}

/// `encodeURI(s)`: like [`encode_uri_component`] but keeps `; / ? : @ & = + $ , #`.
pub fn encode_uri(s: &str) -> String {
    encode(s, |b| is_unreserved(b) || is_uri_reserved(b))
}

fn uri_malformed() -> Error {
    Error::js("URIError", "URI malformed")
}

fn hex_value(b: u8) -> Option<u8> {
    char::from(b).to_digit(16).map(|d| d as u8)
}

/// Reads the `%XX` escape at byte `k` (which holds `%`).
fn escaped_byte(b: &[u8], k: usize) -> Result<u8> {
    if k + 2 >= b.len() {
        return Err(uri_malformed());
    }
    match (hex_value(b[k + 1]), hex_value(b[k + 2])) {
        (Some(h), Some(l)) => Ok(h << 4 | l),
        _ => Err(uri_malformed()),
    }
}

/// The spec's `Decode(string, reservedSet)`.
fn decode(s: &str, keep_escaped: impl Fn(u8) -> bool) -> Result<String> {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut k = 0;
    while k < b.len() {
        if b[k] != b'%' {
            out.push(b[k]);
            k += 1;
            continue;
        }
        let start = k;
        let byte = escaped_byte(b, k)?;
        k += 2;
        if byte < 0x80 {
            if keep_escaped(byte) {
                out.extend_from_slice(&b[start..=k]);
            } else {
                out.push(byte);
            }
            k += 1;
            continue;
        }
        // Number of leading 1 bits: the length of the UTF-8 sequence.
        let n = byte.leading_ones() as usize;
        if n == 1 || n > 4 {
            return Err(uri_malformed());
        }
        let mut octets = vec![byte];
        if k + 3 * (n - 1) >= b.len() {
            return Err(uri_malformed());
        }
        for _ in 1..n {
            k += 1;
            if b[k] != b'%' {
                return Err(uri_malformed());
            }
            let cont = escaped_byte(b, k)?;
            if cont & 0xC0 != 0x80 {
                return Err(uri_malformed());
            }
            k += 2;
            octets.push(cont);
        }
        // Rejects overlong forms, surrogates and code points above U+10FFFF.
        if std::str::from_utf8(&octets).is_err() {
            return Err(uri_malformed());
        }
        out.extend_from_slice(&octets);
        k += 1;
    }
    String::from_utf8(out).map_err(|_| uri_malformed())
}

/// `decodeURIComponent(s)`.
pub fn decode_uri_component(s: &str) -> Result<String> {
    decode(s, |_| false)
}

/// `decodeURI(s)`: like [`decode_uri_component`] but escapes of `; / ? : @ & = + $ , #` stay
/// as written.
pub fn decode_uri(s: &str) -> Result<String> {
    decode(s, is_uri_reserved)
}
