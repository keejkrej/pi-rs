//! Port of packages/protocol/src/cbor/options.ts

use std::sync::{LazyLock, Mutex, PoisonError};

use pi_js::text::{TextDecoderOptions, Utf8StreamDecoder};
use pi_js::{Error, Result};

pub const UINT32_BASE: i64 = 0x1_0000_0000;
pub const MAX_UINT32: i64 = 0xffff_ffff;
const MAX_CONFIGURED_DEPTH: i64 = 512;

/// Safe defaults for untrusted protocol payloads.
pub const DEFAULT_MAX_CBOR_BYTE_LENGTH: i64 = 16 * 1024 * 1024;
pub const DEFAULT_MAX_CBOR_CONTAINER_LENGTH: i64 = 1_000_000;
pub const DEFAULT_MAX_CBOR_DEPTH: i64 = 64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CborOptions {
    /// Maximum encoded input/output bytes and maximum byte/text string length.
    pub max_byte_length: Option<i64>,
    /// Maximum number of elements in an array or entries in a map.
    pub max_container_length: Option<i64>,
    /// Maximum recursive item depth.
    pub max_depth: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedCborOptions {
    pub max_byte_length: i64,
    pub max_container_length: i64,
    pub max_depth: i64,
}

/// JS name `CborError`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct CborError {
    pub message: String,
}

impl CborError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// `new TextEncoder()`.
#[derive(Clone, Copy, Debug, Default)]
pub struct TextEncoder;

impl TextEncoder {
    /// `TextEncoder#encode`. Rust strings are already Unicode scalars.
    pub fn encode(&self, input: &str) -> Vec<u8> {
        input.as_bytes().to_vec()
    }
}

#[allow(non_upper_case_globals)]
pub static text_encoder: TextEncoder = TextEncoder;

/// `new TextDecoder("utf-8", { fatal: true, ignoreBOM: true })`.
#[derive(Debug)]
pub struct TextDecoder {
    inner: Mutex<Utf8StreamDecoder>,
}

impl TextDecoder {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Utf8StreamDecoder::with_options(TextDecoderOptions {
                fatal: true,
                ignore_bom: true,
            })),
        }
    }

    pub fn decode(&self, bytes: &[u8]) -> Result<String> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .try_decode(bytes, false)
    }
}

impl Default for TextDecoder {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(non_upper_case_globals)]
pub static text_decoder: LazyLock<TextDecoder> = LazyLock::new(TextDecoder::new);

fn resolve_limit(name: &str, value: i64, maximum: i64) -> Result<i64> {
    if value < 0 || value > maximum {
        return Err(Error::js(
            "RangeError",
            format!("{name} must be an integer between 0 and {maximum}"),
        ));
    }
    Ok(value)
}

pub fn resolve_options(options: Option<CborOptions>) -> Result<ResolvedCborOptions> {
    let options = options.unwrap_or_default();
    Ok(ResolvedCborOptions {
        max_byte_length: resolve_limit(
            "maxByteLength",
            options.max_byte_length.unwrap_or(DEFAULT_MAX_CBOR_BYTE_LENGTH),
            MAX_UINT32,
        )?,
        max_container_length: resolve_limit(
            "maxContainerLength",
            options
                .max_container_length
                .unwrap_or(DEFAULT_MAX_CBOR_CONTAINER_LENGTH),
            MAX_UINT32,
        )?,
        max_depth: resolve_limit(
            "maxDepth",
            options.max_depth.unwrap_or(DEFAULT_MAX_CBOR_DEPTH),
            MAX_CONFIGURED_DEPTH,
        )?,
    })
}
