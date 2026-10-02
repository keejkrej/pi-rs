//! Port of packages/protocol/src/cbor/options.ts

#![allow(dead_code, unused_variables)]

use std::sync::{LazyLock, Mutex};

use pi_js::Result;
use pi_js::text::{TextDecoderOptions, Utf8StreamDecoder};

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
        todo!("port: TextDecoder::decode")
    }
}

pub static text_decoder: LazyLock<TextDecoder> = LazyLock::new(TextDecoder::new);

fn resolve_limit(name: &str, value: i64, maximum: i64) -> Result<i64> {
    todo!("port: resolve_limit")
}

pub fn resolve_options(options: Option<CborOptions>) -> Result<ResolvedCborOptions> {
    todo!("port: resolve_options")
}
