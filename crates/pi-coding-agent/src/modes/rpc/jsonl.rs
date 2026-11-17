//! Port of packages/coding-agent/src/modes/rpc/jsonl.ts

use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt};

/// Serialize a single strict JSONL record.
///
/// Framing is LF-only. Payload strings may contain other Unicode separators such as
/// U+2028 and U+2029. Clients must split records on `\n` only.
pub fn serialize_json_line(value: &Value) -> String {
    format!("{}\n", pi_js::json::stringify(value))
}

/// Serialize a serializable record as a strict LF-only JSONL line.
pub fn serialize_json_line_value<T: serde::Serialize>(value: &T) -> pi_js::Result<String> {
    let val = serde_json::to_value(value).map_err(|e| pi_js::Error::msg(e.to_string()))?;
    Ok(serialize_json_line(&val))
}

/// Stateful UTF-8 LF-only JSONL decoder matching Node `StringDecoder("utf8")` + LF framing.
///
/// Does not split on Unicode line/paragraph separators (U+2028, U+2029) or bare CR.
/// Strips a single trailing `\r` from `\r\n` lines and flushes any non-empty trailing
/// buffer on [`JsonlLineDecoder::finish`].
#[derive(Default)]
pub struct JsonlLineDecoder {
    utf8_pending: Vec<u8>,
    buffer: String,
}

impl JsonlLineDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_bytes(&mut self, chunk: &[u8], mut on_line: impl FnMut(&str)) {
        if chunk.is_empty() {
            return;
        }
        let decoded = self.decode_utf8_chunk(chunk, false);
        if !decoded.is_empty() {
            self.push_str(&decoded, &mut on_line);
        }
    }

    pub fn push_str(&mut self, chunk: &str, mut on_line: impl FnMut(&str)) {
        if !self.utf8_pending.is_empty() {
            let flushed = self.decode_utf8_chunk(&[], true);
            self.buffer.push_str(&flushed);
        }
        self.buffer.push_str(chunk);
        self.drain_complete_lines(&mut on_line);
    }

    pub fn finish(&mut self, mut on_line: impl FnMut(&str)) {
        let tail = self.decode_utf8_chunk(&[], true);
        if !tail.is_empty() {
            self.buffer.push_str(&tail);
        }
        self.drain_complete_lines(&mut on_line);
        if !self.buffer.is_empty() {
            let line = self.buffer.strip_suffix('\r').unwrap_or(&self.buffer);
            on_line(line);
            self.buffer.clear();
        }
    }

    fn drain_complete_lines(&mut self, on_line: &mut impl FnMut(&str)) {
        while let Some(newline_index) = self.buffer.find('\n') {
            {
                let raw = &self.buffer[..newline_index];
                let line = raw.strip_suffix('\r').unwrap_or(raw);
                on_line(line);
            }
            self.buffer.drain(..=newline_index);
        }
    }

    fn decode_utf8_chunk(&mut self, chunk: &[u8], final_chunk: bool) -> String {
        let mut combined;
        let bytes: &[u8] = if self.utf8_pending.is_empty() {
            chunk
        } else {
            combined = std::mem::take(&mut self.utf8_pending);
            combined.extend_from_slice(chunk);
            &combined
        };

        let mut out = String::new();
        let mut offset = 0;
        while offset < bytes.len() {
            match std::str::from_utf8(&bytes[offset..]) {
                Ok(valid) => {
                    out.push_str(valid);
                    offset = bytes.len();
                }
                Err(err) => {
                    let valid_up_to = err.valid_up_to();
                    if valid_up_to > 0
                        && let Ok(valid) = std::str::from_utf8(&bytes[offset..offset + valid_up_to])
                    {
                        out.push_str(valid);
                    }
                    offset += valid_up_to;
                    match err.error_len() {
                        Some(invalid_len) => {
                            out.push('\u{FFFD}');
                            offset += invalid_len;
                        }
                        None => {
                            if final_chunk {
                                out.push('\u{FFFD}');
                                offset = bytes.len();
                            } else {
                                self.utf8_pending.extend_from_slice(&bytes[offset..]);
                                break;
                            }
                        }
                    }
                }
            }
        }
        out
    }
}

/// Handle returned by [`attach_jsonl_line_reader`] allowing the reader task to be detached.
pub struct JsonlReaderHandle {
    abort: tokio::task::AbortHandle,
}

impl JsonlReaderHandle {
    pub fn detach(&self) {
        self.abort.abort();
    }
}

/// Attach an LF-only JSONL reader to an async byte stream.
///
/// Spawns a task on the current Tokio runtime that reads bytes until EOF or [`JsonlReaderHandle::detach`],
/// invokes `on_line` for each complete line (and any non-empty trailing line on EOF), and then invokes `on_end`
/// if provided.
pub fn attach_jsonl_line_reader<R>(
    mut reader: R,
    on_line: Arc<dyn Fn(&str) + Send + Sync>,
    on_end: Option<Arc<dyn Fn() + Send + Sync>>,
) -> JsonlReaderHandle
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let jh = tokio::spawn(async move {
        let mut decoder = JsonlLineDecoder::new();
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) => {
                    decoder.finish(|line| on_line(line));
                    if let Some(ref end) = on_end {
                        end();
                    }
                    break;
                }
                Ok(n) => {
                    decoder.push_bytes(&buf[..n], |line| on_line(line));
                }
                Err(_) => {
                    decoder.finish(|line| on_line(line));
                    if let Some(ref end) = on_end {
                        end();
                    }
                    break;
                }
            }
        }
    });
    JsonlReaderHandle {
        abort: jh.abort_handle(),
    }
}
