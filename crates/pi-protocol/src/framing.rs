//! Port of packages/protocol/src/framing.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

use pi_js::Result;

const FRAME_HEADER_LENGTH: usize = 4;
const MAX_UINT32: i64 = 0xffff_ffff;
const PAYLOAD_BLOCK_SIZE: usize = 64 * 1024;

/// Default upper bound for one framed CBOR payload.
pub const DEFAULT_MAX_FRAME_LENGTH: i64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameDecoderOptions {
    pub max_frame_length: Option<i64>,
}

/// JS name `FrameError`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct FrameError {
    pub message: String,
}

impl FrameError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

fn resolve_max_frame_length(options: Option<FrameDecoderOptions>) -> Result<i64> {
    todo!("port: resolve_max_frame_length")
}

/// Prefixes a payload with its unsigned 32-bit big-endian byte length.
pub fn encode_frame(payload: &[u8]) -> Result<Vec<u8>> {
    todo!("port: encode_frame")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DecoderState {
    Open,
    Ended,
    Failed,
}

struct FrameDecoderFields {
    header: [u8; FRAME_HEADER_LENGTH],
    header_length: usize,
    payload_blocks: Vec<Vec<u8>>,
    /// Index of the block being filled inside [`FrameDecoderFields::payload_blocks`].
    /// PORT: TS holds the block array itself; the block also lives in `payload_blocks`.
    current_payload_block: Option<usize>,
    current_payload_block_length: usize,
    expected_payload_length: Option<i64>,
    payload_length: i64,
    state: DecoderState,
}

struct FrameDecoderInner {
    max_frame_length: i64,
    fields: Mutex<FrameDecoderFields>,
}

/// Incrementally splits arbitrary byte chunks into length-prefixed payloads.
///
/// PORT: TS class with identity. Handle; methods take `&self`.
#[derive(Clone)]
pub struct FrameDecoder {
    inner: Arc<FrameDecoderInner>,
}

impl FrameDecoder {
    pub fn new(options: Option<FrameDecoderOptions>) -> Self {
        todo!("port: FrameDecoder::new")
    }

    pub fn push(&self, chunk: &[u8]) -> Result<Vec<Vec<u8>>> {
        todo!("port: FrameDecoder::push")
    }

    pub fn end(&self) -> Result<()> {
        todo!("port: FrameDecoder::end")
    }

    fn fail(&self, message: &str) -> Result<()> {
        todo!("port: FrameDecoder::fail")
    }
}
