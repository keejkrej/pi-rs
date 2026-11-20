//! Port of packages/mcp/src/transports/lines.ts

#![allow(dead_code, unused_variables)]

use std::sync::{Arc, Mutex};

use pi_js::Error;

use crate::protocol::jsonrpc::{JsonRpcMessage, parse_json_rpc_message};
use crate::transports::transport::DEFAULT_MAX_MESSAGE_BYTES;

pub const DEFAULT_MAX_STDERR_BYTES: i64 = 64 * 1024;

#[derive(Clone)]
pub struct JsonRpcLineReaderOptions {
    pub max_message_bytes: Option<i64>,
    pub max_stderr_bytes: Option<i64>,
    /// Every stderr chunk, decoded as UTF-8.
    pub on_stderr: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    pub on_message: Arc<dyn Fn(&JsonRpcMessage) + Send + Sync>,
    pub on_error: Arc<dyn Fn(&Error) + Send + Sync>,
}

struct JsonRpcLineReaderState {
    /// Chunks of the incomplete message received so far.
    stdout_chunks: Vec<Vec<u8>>,
    stdout_pending_bytes: usize,
    stderr_buffer: Vec<u8>,
}

/// Reads a program's output as MCP's stdio framing: newline-delimited JSON-RPC messages on stdout, and a bounded
/// tail of stderr for diagnostics.
#[derive(Clone)]
pub struct JsonRpcLineReader {
    options: Arc<JsonRpcLineReaderOptions>,
    state: Arc<Mutex<JsonRpcLineReaderState>>,
}

impl JsonRpcLineReader {
    pub fn new(options: JsonRpcLineReaderOptions) -> Self {
        Self {
            options: Arc::new(options),
            state: Arc::new(Mutex::new(JsonRpcLineReaderState {
                stdout_chunks: Vec::new(),
                stdout_pending_bytes: 0,
                stderr_buffer: Vec::new(),
            })),
        }
    }

    pub fn options(&self) -> &JsonRpcLineReaderOptions {
        &self.options
    }

    /// The last `max_stderr_bytes` of stderr.
    pub fn stderr(&self) -> String {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        String::from_utf8_lossy(&state.stderr_buffer).into_owned()
    }

    /// Takes the next stdout bytes and reports every complete message, or the error parsing it. Bytes of an
    /// incomplete message are kept as chunks and joined once when its newline arrives, so a large message that
    /// arrives in many chunks is copied and scanned only once.
    pub fn read_stdout(&self, chunk: &[u8]) {
        let max_message_bytes = self
            .options
            .max_message_bytes
            .unwrap_or(DEFAULT_MAX_MESSAGE_BYTES)
            .max(0) as usize;

        enum Frame {
            Line(Vec<u8>),
            Oversized,
        }

        let mut frames = Vec::new();
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let mut start = 0usize;
            while start <= chunk.len() {
                if let Some(rel_nl) = chunk[start..].iter().position(|&b| b == b'\n') {
                    let newline = start + rel_nl;
                    let tail = &chunk[start..newline];
                    let total_len = state.stdout_pending_bytes + tail.len();
                    let frame = if total_len > max_message_bytes {
                        state.stdout_chunks.clear();
                        state.stdout_pending_bytes = 0;
                        Some(Frame::Oversized)
                    } else if state.stdout_chunks.is_empty() {
                        Some(Frame::Line(tail.to_vec()))
                    } else {
                        let mut line = Vec::with_capacity(total_len);
                        for part in state.stdout_chunks.drain(..) {
                            line.extend_from_slice(&part);
                        }
                        line.extend_from_slice(tail);
                        state.stdout_pending_bytes = 0;
                        Some(Frame::Line(line))
                    };
                    if let Some(frame) = frame {
                        frames.push(frame);
                    }
                    start = newline + 1;
                    if start == chunk.len() {
                        break;
                    }
                } else {
                    if start < chunk.len() {
                        state.stdout_chunks.push(chunk[start..].to_vec());
                        state.stdout_pending_bytes += chunk.len() - start;
                    }
                    if state.stdout_pending_bytes > max_message_bytes {
                        state.stdout_chunks.clear();
                        state.stdout_pending_bytes = 0;
                        frames.push(Frame::Oversized);
                    }
                    break;
                }
            }
        }

        for frame in frames {
            match frame {
                Frame::Oversized => {
                    (self.options.on_error)(&Error::msg(format!(
                        "MCP stdio message exceeds {max_message_bytes} bytes"
                    )));
                }
                Frame::Line(bytes) => {
                    let line = bytes.strip_suffix(b"\r").unwrap_or(&bytes);
                    let text = String::from_utf8_lossy(line);
                    if pi_js::str16::trim(&text).is_empty() {
                        continue;
                    }
                    match pi_js::json::parse(&text).and_then(|v| parse_json_rpc_message(&v)) {
                        Ok(message) => (self.options.on_message)(&message),
                        Err(err) => (self.options.on_error)(&err),
                    }
                }
            }
        }
    }

    /// Takes the next stderr bytes into the tail and passes them to `on_stderr`.
    pub fn read_stderr(&self, chunk: &[u8]) {
        let max_stderr_bytes = self.options.max_stderr_bytes.unwrap_or(DEFAULT_MAX_STDERR_BYTES).max(0) as usize;
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if max_stderr_bytes == 0 {
                state.stderr_buffer.clear();
            } else {
                state.stderr_buffer.extend_from_slice(chunk);
                if state.stderr_buffer.len() > max_stderr_bytes {
                    let drop_len = state.stderr_buffer.len() - max_stderr_bytes;
                    state.stderr_buffer.drain(..drop_len);
                }
            }
        }
        if let Some(cb) = &self.options.on_stderr {
            let text = String::from_utf8_lossy(chunk);
            cb(&text);
        }
    }

    /// Ends stdout: a message without its newline is reported as an error and dropped.
    pub fn end(&self) {
        let has_incomplete = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.stdout_pending_bytes = 0;
            let chunks = std::mem::take(&mut state.stdout_chunks);
            chunks.into_iter().any(|c| {
                let s = String::from_utf8_lossy(&c);
                !pi_js::str16::trim(&s).is_empty()
            })
        };
        if has_incomplete {
            (self.options.on_error)(&Error::msg(
                "MCP stdio server closed with an incomplete JSON-RPC message",
            ));
        }
    }
}
