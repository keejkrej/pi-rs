//! Port of packages/protocol/src/framing.ts

use std::sync::{Arc, Mutex, PoisonError};

use pi_js::{Error, Result};

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

fn frame_error(message: impl Into<String>) -> Error {
    Error::typed("FrameError", FrameError::new(message))
}

fn resolve_max_frame_length(options: Option<FrameDecoderOptions>) -> Result<i64> {
    let value = options
        .and_then(|o| o.max_frame_length)
        .unwrap_or(DEFAULT_MAX_FRAME_LENGTH);
    if value < 0 || value > MAX_UINT32 {
        return Err(Error::js(
            "RangeError",
            format!("maxFrameLength must be an integer between 0 and {MAX_UINT32}"),
        ));
    }
    Ok(value)
}

/// Prefixes a payload with its unsigned 32-bit big-endian byte length.
pub fn encode_frame(payload: &[u8]) -> Result<Vec<u8>> {
    let length = u32::try_from(payload.len())
        .map_err(|_| Error::js("RangeError", "Frame payload exceeds the unsigned 32-bit length limit"))?;
    let mut frame = Vec::with_capacity(FRAME_HEADER_LENGTH + payload.len());
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
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
    /// PORT: Fallible constructor returning `RangeError` for out-of-range `max_frame_length`.
    pub fn try_new(options: Option<FrameDecoderOptions>) -> Result<Self> {
        let max_frame_length = resolve_max_frame_length(options)?;
        Ok(Self {
            inner: Arc::new(FrameDecoderInner {
                max_frame_length,
                fields: Mutex::new(FrameDecoderFields {
                    header: [0; FRAME_HEADER_LENGTH],
                    header_length: 0,
                    payload_blocks: Vec::new(),
                    current_payload_block: None,
                    current_payload_block_length: 0,
                    expected_payload_length: None,
                    payload_length: 0,
                    state: DecoderState::Open,
                }),
            }),
        })
    }

    /// PORT: TS constructor throws `RangeError` for out-of-range `maxFrameLength`; skeleton `new`
    /// returns `Self`, so this panics on invalid options. Use [`FrameDecoder::try_new`] to handle
    /// untrusted options without panicking.
    pub fn new(options: Option<FrameDecoderOptions>) -> Self {
        Self::try_new(options).unwrap_or_else(|e| panic!("{e}"))
    }

    pub fn push(&self, chunk: &[u8]) -> Result<Vec<Vec<u8>>> {
        let mut fields = self.inner.fields.lock().unwrap_or_else(PoisonError::into_inner);
        if fields.state == DecoderState::Ended {
            return Err(frame_error("Frame decoder has ended"));
        }
        if fields.state == DecoderState::Failed {
            return Err(frame_error("Frame decoder has failed"));
        }

        let mut frames = Vec::new();
        let mut chunk_offset = 0usize;
        while chunk_offset < chunk.len() {
            if fields.expected_payload_length.is_none() {
                let header_bytes = (FRAME_HEADER_LENGTH - fields.header_length).min(chunk.len() - chunk_offset);
                let dst_start = fields.header_length;
                fields.header[dst_start..dst_start + header_bytes]
                    .copy_from_slice(&chunk[chunk_offset..chunk_offset + header_bytes]);
                fields.header_length += header_bytes;
                chunk_offset += header_bytes;
                if fields.header_length < FRAME_HEADER_LENGTH {
                    continue;
                }

                let frame_length = i64::from(fields.header[0]) * 0x1_000_000
                    + i64::from(fields.header[1]) * 0x1_0000
                    + i64::from(fields.header[2]) * 0x100
                    + i64::from(fields.header[3]);
                fields.header_length = 0;
                if frame_length > self.inner.max_frame_length {
                    drop(fields);
                    return self.fail(&format!(
                        "Frame length {frame_length} exceeds configured limit of {}",
                        self.inner.max_frame_length
                    ));
                }
                if frame_length == 0 {
                    frames.push(Vec::new());
                    continue;
                }
                fields.expected_payload_length = Some(frame_length);
                fields.payload_blocks.clear();
                fields.current_payload_block = None;
                fields.current_payload_block_length = 0;
                fields.payload_length = 0;
            }

            let Some(expected_payload_length) = fields.expected_payload_length else {
                continue;
            };
            while chunk_offset < chunk.len() && fields.payload_length < expected_payload_length {
                let need_new_block = match fields.current_payload_block {
                    None => true,
                    Some(idx) => fields.current_payload_block_length == fields.payload_blocks[idx].len(),
                };
                if need_new_block {
                    let remaining = (expected_payload_length - fields.payload_length) as usize;
                    let block = vec![0u8; PAYLOAD_BLOCK_SIZE.min(remaining)];
                    fields.payload_blocks.push(block);
                    fields.current_payload_block = Some(fields.payload_blocks.len() - 1);
                    fields.current_payload_block_length = 0;
                }
                let Some(idx) = fields.current_payload_block else {
                    continue;
                };
                let block_len = fields.payload_blocks[idx].len();
                let payload_bytes = (block_len - fields.current_payload_block_length).min(chunk.len() - chunk_offset);
                let dst_start = fields.current_payload_block_length;
                fields.payload_blocks[idx][dst_start..dst_start + payload_bytes]
                    .copy_from_slice(&chunk[chunk_offset..chunk_offset + payload_bytes]);
                fields.current_payload_block_length += payload_bytes;
                fields.payload_length += payload_bytes as i64;
                chunk_offset += payload_bytes;
            }
            if fields.payload_length == expected_payload_length {
                if fields.payload_blocks.len() == 1 {
                    if let Some(block) = fields.payload_blocks.pop() {
                        frames.push(block);
                    }
                } else {
                    let mut payload = Vec::with_capacity(expected_payload_length as usize);
                    for payload_block in fields.payload_blocks.drain(..) {
                        payload.extend_from_slice(&payload_block);
                    }
                    frames.push(payload);
                }
                fields.payload_blocks.clear();
                fields.current_payload_block = None;
                fields.current_payload_block_length = 0;
                fields.expected_payload_length = None;
                fields.payload_length = 0;
            }
        }
        Ok(frames)
    }

    pub fn end(&self) -> Result<()> {
        let mut fields = self.inner.fields.lock().unwrap_or_else(PoisonError::into_inner);
        if fields.state == DecoderState::Ended {
            return Err(frame_error("Frame decoder has ended"));
        }
        if fields.state == DecoderState::Failed {
            return Err(frame_error("Frame decoder has failed"));
        }
        if fields.header_length != 0 || fields.expected_payload_length.is_some() {
            drop(fields);
            return self.fail("Truncated frame at end of stream");
        }
        fields.state = DecoderState::Ended;
        Ok(())
    }

    fn fail<T>(&self, message: &str) -> Result<T> {
        let mut fields = self.inner.fields.lock().unwrap_or_else(PoisonError::into_inner);
        fields.state = DecoderState::Failed;
        fields.header_length = 0;
        fields.payload_blocks.clear();
        fields.current_payload_block = None;
        fields.current_payload_block_length = 0;
        fields.expected_payload_length = None;
        fields.payload_length = 0;
        Err(frame_error(message))
    }
}
