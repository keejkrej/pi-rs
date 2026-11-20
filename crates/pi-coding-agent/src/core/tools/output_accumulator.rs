//! Port of packages/coding-agent/src/core/tools/output-accumulator.ts

use std::collections::VecDeque;

use pi_js::fs::File;

use crate::core::tools::truncate::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncationOptions, TruncationResult, truncate_tail,
};
use crate::utils::output_files::create_output_file_stream;

/// Bounded rolling output buffer for shell tools (`bash` / `powershell`).
///
/// Mirrors `OutputAccumulator` from TypeScript (`7fbbd5f4a` + `#12062` over-long tail fix):
/// retains up to `max_retained_bytes` in memory and spills complete output to a temporary file
/// once total output crosses `spill_threshold_bytes` (`DEFAULT_MAX_BYTES`).
pub struct OutputAccumulator {
    chunks: VecDeque<String>,
    retained_bytes: usize,
    max_retained_bytes: usize,
    total_bytes: usize,
    total_lines: usize,
    has_trailing_partial: bool,
    spill_threshold_bytes: usize,
    temp_file_prefix: String,
    temp_file_path: Option<String>,
    temp_file: Option<File>,
}

impl OutputAccumulator {
    pub fn new(max_bytes: Option<usize>, temp_file_prefix: Option<&str>) -> Self {
        let spill = max_bytes.unwrap_or(DEFAULT_MAX_BYTES);
        // Retain at least 2x the tail window in memory so `truncate_tail` sees complete trailing lines
        let retained = spill.saturating_mul(2).max(DEFAULT_MAX_BYTES);
        Self {
            chunks: VecDeque::new(),
            retained_bytes: 0,
            max_retained_bytes: retained,
            total_bytes: 0,
            total_lines: 0,
            has_trailing_partial: false,
            spill_threshold_bytes: spill,
            temp_file_prefix: temp_file_prefix.unwrap_or("pi-bash").to_string(),
            temp_file_path: None,
            temp_file: None,
        }
    }

    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    pub fn temp_file_path(&self) -> Option<&str> {
        self.temp_file_path.as_deref()
    }

    pub fn push(&mut self, chunk: &str) {
        if chunk.is_empty() {
            return;
        }

        // Count newline boundaries incrementally across streamed chunks
        let newlines = chunk.bytes().filter(|&b| b == b'\n').count();
        if self.total_bytes == 0 {
            self.total_lines = 1 + newlines;
            self.has_trailing_partial = !chunk.ends_with('\n');
        } else {
            self.total_lines += newlines;
            if self.has_trailing_partial {
                self.has_trailing_partial = !chunk.ends_with('\n');
            } else {
                self.has_trailing_partial = !chunk.ends_with('\n');
            }
        }

        self.total_bytes += chunk.len();

        // Start spilling full output to a temp file as soon as output crosses `spill_threshold_bytes`
        if self.temp_file.is_none()
            && self.total_bytes > self.spill_threshold_bytes
            && let Ok((path, mut file)) = create_output_file_stream(&self.temp_file_prefix, ".log")
        {
            for existing in &self.chunks {
                let _ = file.write_all(existing.as_bytes());
            }
            let _ = file.write_all(chunk.as_bytes());
            self.temp_file_path = Some(path);
            self.temp_file = Some(file);
        } else if let Some(file) = self.temp_file.as_mut() {
            let _ = file.write_all(chunk.as_bytes());
        }

        // #12062: If a single chunk exceeds `max_retained_bytes`, trim its leading bytes at a valid UTF-8 boundary
        let stored_chunk = if chunk.len() > self.max_retained_bytes {
            let start = ceil_char_boundary(chunk, chunk.len() - self.max_retained_bytes);
            chunk[start..].to_string()
        } else {
            chunk.to_string()
        };

        self.retained_bytes += stored_chunk.len();
        self.chunks.push_back(stored_chunk);

        while self.chunks.len() > 1
            && let Some(front) = self.chunks.front()
            && self.retained_bytes.saturating_sub(front.len()) >= self.max_retained_bytes
        {
            if let Some(removed) = self.chunks.pop_front() {
                self.retained_bytes = self.retained_bytes.saturating_sub(removed.len());
            }
        }
    }

    pub fn flush(&mut self) {
        if let Some(file) = self.temp_file.as_mut() {
            let _ = file.sync();
        }
    }

    /// Persist retained output to a temp file when output was truncated (e.g. by line count
    /// even if byte size remained below `spill_threshold_bytes`).
    pub fn ensure_temp_file(&mut self) -> Option<String> {
        if let Some(path) = self.temp_file_path.clone() {
            self.flush();
            return Some(path);
        }
        if let Ok((path, mut file)) = create_output_file_stream(&self.temp_file_prefix, ".log") {
            for existing in &self.chunks {
                let _ = file.write_all(existing.as_bytes());
            }
            let _ = file.sync();
            self.temp_file_path = Some(path.clone());
            self.temp_file = Some(file);
            return Some(path);
        }
        None
    }

    pub fn retained_text(&self) -> String {
        let mut out = String::with_capacity(self.retained_bytes);
        for chunk in &self.chunks {
            out.push_str(chunk);
        }
        out
    }

    /// Produce a tail-truncated view plus metadata matching TS `TruncationResult`.
    pub fn snapshot(&mut self, max_lines: Option<usize>, max_bytes: Option<usize>) -> TruncationResult {
        self.flush();
        let text = self.retained_text();
        let bytes_limit = max_bytes.unwrap_or(self.spill_threshold_bytes);
        let lines_limit = max_lines.unwrap_or(DEFAULT_MAX_LINES);
        let mut res = truncate_tail(
            &text,
            Some(TruncationOptions {
                max_lines: Some(lines_limit),
                max_bytes: Some(bytes_limit),
            }),
        );

        // Preserve true totals from before retained-buffer eviction (#12062)
        if self.total_bytes > text.len() {
            res.truncated = true;
            if res.truncated_by.is_none() {
                res.truncated_by = Some(crate::core::tools::truncate::TruncatedBy::Bytes);
            }
            res.total_bytes = self.total_bytes as i64;
            res.total_lines = (self.total_lines as i64).max(res.total_lines);
        }
        res
    }
}

fn ceil_char_boundary(s: &str, mut idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

fn floor_char_boundary(s: &str, mut idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// Bounded 1 MiB buffer for `structured_content.output` in `bash` / `powershell`.
///
/// Keeps all bytes verbatim up to `max_bytes`; above `max_bytes`, retains the first half
/// and last half separated by a `"\n\n[... N bytes omitted ...]\n\n"` marker.
pub struct StructuredOutputBuffer {
    max_bytes: usize,
    half_bytes: usize,
    head: String,
    tail_chunks: VecDeque<String>,
    tail_bytes: usize,
    total_bytes: usize,
    overflowed: bool,
}

impl StructuredOutputBuffer {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            half_bytes: max_bytes / 2,
            head: String::new(),
            tail_chunks: VecDeque::new(),
            tail_bytes: 0,
            total_bytes: 0,
            overflowed: false,
        }
    }

    pub fn push(&mut self, chunk: &str) {
        if chunk.is_empty() {
            return;
        }
        self.total_bytes += chunk.len();

        let mut remaining = chunk;
        if self.head.len() < self.half_bytes {
            let space = self.half_bytes - self.head.len();
            if remaining.len() <= space {
                self.head.push_str(remaining);
                return;
            }
            let split = floor_char_boundary(remaining, space);
            self.head.push_str(&remaining[..split]);
            remaining = &remaining[split..];
        }

        if remaining.is_empty() {
            return;
        }

        // Retain trailing half once total output crosses `max_bytes`
        let max_tail = self.half_bytes.saturating_sub(64);
        let kept = if self.overflowed && remaining.len() > max_tail {
            let start = ceil_char_boundary(remaining, remaining.len() - max_tail);
            remaining[start..].to_string()
        } else {
            remaining.to_string()
        };

        self.tail_bytes += kept.len();
        self.tail_chunks.push_back(kept);

        if self.head.len() + self.tail_bytes > self.max_bytes {
            self.overflowed = true;
            while self.tail_bytes > max_tail {
                let Some(mut front) = self.tail_chunks.pop_front() else {
                    break;
                };
                let excess = self.tail_bytes - max_tail;
                if front.len() <= excess {
                    self.tail_bytes -= front.len();
                } else {
                    let cut = ceil_char_boundary(&front, excess);
                    let remainder = front.split_off(cut);
                    self.tail_bytes -= cut;
                    self.tail_chunks.push_front(remainder);
                    break;
                }
            }
        }
    }

    pub fn finish(&self) -> (String, bool) {
        if !self.overflowed {
            let mut out = String::with_capacity(self.head.len() + self.tail_bytes);
            out.push_str(&self.head);
            for c in &self.tail_chunks {
                out.push_str(c);
            }
            return (out, false);
        }

        let mut tail = String::with_capacity(self.tail_bytes);
        for c in &self.tail_chunks {
            tail.push_str(c);
        }
        let omitted = self
            .total_bytes
            .saturating_sub(self.head.len())
            .saturating_sub(tail.len());
        let marker = format!("\n\n[... {omitted} bytes omitted ...]\n\n");
        let mut out = String::with_capacity(self.head.len() + marker.len() + tail.len());
        out.push_str(&self.head);
        out.push_str(&marker);
        out.push_str(&tail);
        (out, true)
    }
}
