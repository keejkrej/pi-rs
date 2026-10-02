//! pi_js::text (Rust-only; API contract: PORTING.md Appendix A).
//!
//! WHATWG `TextDecoder("utf-8")` and Node `Buffer` UTF-8 helpers. Invalid input is replaced
//! with U+FFFD per maximal subpart (the WHATWG / Unicode "substitution of maximal subparts"
//! practice), which is what both `TextDecoder` and `Buffer#toString("utf8")` do in Node.

use crate::error::{Error, JsError, Result};

/// Options of `new TextDecoder("utf-8", { fatal, ignoreBOM })`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TextDecoderOptions {
    /// Throw on invalid data instead of substituting U+FFFD (see [`Utf8StreamDecoder::try_decode`]).
    pub fatal: bool,
    /// Keep a leading byte order mark instead of stripping it.
    pub ignore_bom: bool,
}

/// `new TextDecoder()` (UTF-8): `decode(bytes, { stream })`.
///
/// Incomplete sequences at the end of a `stream: true` chunk are held back until the next call.
/// A leading U+FEFF of each decoded stream is stripped unless `ignore_bom` is set; the stream
/// restarts after every non-streaming call.
#[derive(Debug, Default, Clone)]
pub struct Utf8StreamDecoder {
    options: TextDecoderOptions,
    /// Bytes of an incomplete sequence carried over from the previous `stream: true` call.
    pending: Vec<u8>,
    /// Whether the start of the current stream has been processed (BOM handled).
    bom_seen: bool,
}

/// Node's `ERR_ENCODING_INVALID_ENCODED_DATA` for a fatal UTF-8 decoder.
fn invalid_encoded_data() -> Error {
    Error::Js(JsError {
        name: "TypeError".to_string(),
        message: "The encoded data was not valid for encoding utf-8".to_string(),
        code: Some("ERR_ENCODING_INVALID_ENCODED_DATA".to_string()),
        cause: None,
    })
}

impl Utf8StreamDecoder {
    /// `new TextDecoder()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// `new TextDecoder("utf-8", options)`.
    pub fn with_options(options: TextDecoderOptions) -> Self {
        Self {
            options,
            pending: Vec::new(),
            bom_seen: false,
        }
    }

    /// `decoder.decode(bytes, { stream })` with U+FFFD substitution. The `fatal` option is not
    /// consulted here; use [`Utf8StreamDecoder::try_decode`] for fatal decoders.
    pub fn decode(&mut self, bytes: &[u8], stream: bool) -> String {
        self.run(bytes, stream, false).unwrap_or_default()
    }

    /// `decoder.decode(bytes, { stream })` honoring `fatal`: invalid data fails with
    /// `TypeError [ERR_ENCODING_INVALID_ENCODED_DATA]: The encoded data was not valid for
    /// encoding utf-8` and resets the decoder.
    pub fn try_decode(&mut self, bytes: &[u8], stream: bool) -> Result<String> {
        let fatal = self.options.fatal;
        self.run(bytes, stream, fatal).ok_or_else(|| {
            self.pending.clear();
            self.bom_seen = false;
            invalid_encoded_data()
        })
    }

    /// Decodes; `None` means invalid data was found while `fatal` is set.
    fn run(&mut self, bytes: &[u8], stream: bool, fatal: bool) -> Option<String> {
        let mut input = std::mem::take(&mut self.pending);
        input.extend_from_slice(bytes);
        let mut out = String::with_capacity(input.len());
        let mut rest: &[u8] = &input;
        loop {
            match std::str::from_utf8(rest) {
                Ok(valid) => {
                    out.push_str(valid);
                    break;
                }
                Err(e) => {
                    let (valid, after) = rest.split_at(e.valid_up_to());
                    // `valid_up_to` marks a valid UTF-8 prefix.
                    out.push_str(std::str::from_utf8(valid).unwrap_or_default());
                    match e.error_len() {
                        Some(n) => {
                            if fatal {
                                return None;
                            }
                            out.push('\u{FFFD}');
                            rest = &after[n..];
                        }
                        None => {
                            // An incomplete but so far valid sequence at the end of the input.
                            if stream {
                                self.pending = after.to_vec();
                            } else {
                                if fatal {
                                    return None;
                                }
                                out.push('\u{FFFD}');
                            }
                            break;
                        }
                    }
                }
            }
        }
        if !self.bom_seen && !out.is_empty() {
            self.bom_seen = true;
            if !self.options.ignore_bom && out.starts_with('\u{FEFF}') {
                out.drain(..'\u{FEFF}'.len_utf8());
            }
        }
        if !stream {
            self.pending.clear();
            self.bom_seen = false;
        }
        Some(out)
    }
}

/// `new TextDecoder().decode(bytes)`: lossy UTF-8 with a leading BOM stripped.
pub fn decode_utf8(bytes: &[u8]) -> String {
    Utf8StreamDecoder::new().decode(bytes, false)
}

/// `Buffer.from(bytes).toString("utf8")`: lossy UTF-8; a leading BOM is kept.
pub fn buffer_to_string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// `Buffer.from(bytes).toString("latin1")` (also `"binary"`): each byte becomes the char with
/// that code (U+0000..U+00FF).
pub fn latin1_to_string(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}

/// `Buffer.from(bytes).toString("ascii")`: like [`latin1_to_string`] with the high bit of each
/// byte cleared (`0xE9` becomes `"i"`).
pub fn ascii_to_string(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b & 0x7F)).collect()
}

/// `Buffer.byteLength(s, "utf8")` / `new TextEncoder().encode(s).length`.
pub fn utf8_byte_length(s: &str) -> usize {
    s.len()
}
