//! Port of packages/durable/src/harness/output.ts

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use pi_js::text::Utf8StreamDecoder;
use tokio::sync::oneshot;

use crate::harness::types::OutputRetainMode;
use crate::truncate::utf8_byte_length;

/// Retention limits of one tool's output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputLimits {
    pub max_bytes: usize,
    pub max_lines: usize,
    pub retain: OutputRetainMode,
}

/// Retained output and what the limits dropped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundedOutput {
    pub text: String,
    pub dropped_bytes: usize,
    pub dropped_lines: usize,
}

/// An exact slice of the input within the limits, and what it left out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputSlice {
    pub text: String,
    pub bytes: usize,
    pub dropped_bytes: usize,
    pub dropped_lines: usize,
}

const NEWLINE: u8 = b'\n';

/// Remove control characters that break display and transcripts; tabs and newlines stay.
pub fn sanitize_output(text: &str) -> String {
    text.chars()
        .filter(|&c| {
            !matches!(
                c,
                '\u{0000}'..='\u{0008}' | '\u{000B}'..='\u{001F}' | '\u{FFF9}'..='\u{FFFB}'
            )
        })
        .collect()
}

/// Bound `text` to whole lines within the limits: the first lines for `Head`, the last lines for `Tail`.
/// The result is an exact slice, trailing newline included. A single line longer than `max_bytes` is
/// cut at the byte limit on a character boundary.
pub fn bound_output(text: &str, limits: OutputLimits) -> OutputSlice {
    let bytes = text.as_bytes();
    let (from, to) = match limits.retain {
        OutputRetainMode::Head => head_range(bytes, limits),
        OutputRetainMode::Tail => tail_range(bytes, limits),
    };
    let kept = &bytes[from..to];
    let kept_text = if kept.len() == bytes.len() {
        text.to_owned()
    } else {
        String::from_utf8_lossy(kept).into_owned()
    };
    OutputSlice {
        text: kept_text,
        bytes: kept.len(),
        dropped_bytes: bytes.len().saturating_sub(kept.len()),
        dropped_lines: line_count(bytes).saturating_sub(line_count(kept)),
    }
}

fn find_newline_from(bytes: &[u8], start: usize) -> Option<usize> {
    if start >= bytes.len() {
        return None;
    }
    bytes[start..].iter().position(|&b| b == NEWLINE).map(|idx| start + idx)
}

fn rfind_newline_up_to(bytes: &[u8], max_inclusive: usize) -> Option<usize> {
    if bytes.is_empty() {
        return None;
    }
    let end = (max_inclusive + 1).min(bytes.len());
    bytes[..end].iter().rposition(|&b| b == NEWLINE)
}

fn head_range(bytes: &[u8], limits: OutputLimits) -> (usize, usize) {
    if limits.max_lines == 0 || limits.max_bytes == 0 {
        return (0, 0);
    }
    let mut end = bytes.len();
    let mut lines = 0usize;
    let mut pos = 0usize;
    while let Some(index) = find_newline_from(bytes, pos) {
        lines += 1;
        if lines == limits.max_lines {
            end = index + 1;
            break;
        }
        pos = index + 1;
    }
    if end > limits.max_bytes {
        end = match rfind_newline_up_to(bytes, limits.max_bytes - 1) {
            Some(newline) => newline + 1,
            None => character_end(bytes, limits.max_bytes),
        };
    }
    (0, end)
}

fn tail_range(bytes: &[u8], limits: OutputLimits) -> (usize, usize) {
    if limits.max_lines == 0 || limits.max_bytes == 0 || bytes.is_empty() {
        return (bytes.len(), bytes.len());
    }
    // A trailing newline ends the last line rather than starting another.
    let mut last = if bytes[bytes.len() - 1] == NEWLINE {
        bytes.len().checked_sub(2)
    } else {
        Some(bytes.len() - 1)
    };
    let mut start = 0usize;
    let mut lines = 1usize;
    while let Some(search_end) = last {
        let Some(index) = rfind_newline_up_to(bytes, search_end) else {
            break;
        };
        if lines == limits.max_lines {
            start = index + 1;
            break;
        }
        lines += 1;
        last = index.checked_sub(1);
    }
    if bytes.len() - start > limits.max_bytes {
        let from = bytes.len() - limits.max_bytes;
        let newline = find_newline_from(bytes, from - 1);
        start = match newline {
            Some(nl) if nl + 1 < bytes.len() => nl + 1,
            _ => character_start(bytes, from),
        };
    }
    (start, bytes.len())
}

/// The last character boundary at or before `index`.
pub fn character_end(bytes: &[u8], index: usize) -> usize {
    let mut end = index.min(bytes.len());
    while end > 0 && end < bytes.len() && (bytes[end] & 0xc0) == 0x80 {
        end -= 1;
    }
    end
}

/// The first character boundary at or after `index`.
fn character_start(bytes: &[u8], index: usize) -> usize {
    let mut start = index.min(bytes.len());
    while start < bytes.len() && (bytes[start] & 0xc0) == 0x80 {
        start += 1;
    }
    start
}

fn line_count(bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        return 0;
    }
    let newlines = bytes.iter().filter(|&&b| b == NEWLINE).count();
    newlines + usize::from(bytes[bytes.len() - 1] != NEWLINE)
}

#[derive(Clone, Debug)]
struct OutputChunk {
    text: String,
    bytes: usize,
    newlines: usize,
}

/// Bounded running output of one tool call.
#[derive(Debug)]
pub struct OutputBuffer {
    limits: OutputLimits,
    decoder: Utf8StreamDecoder,
    chunks: VecDeque<OutputChunk>,
    stored_bytes: usize,
    stored_newlines: usize,
    full: bool,
    total_bytes: usize,
    total_newlines: usize,
    ends_with_newline: bool,
}

impl OutputBuffer {
    pub fn new(limits: OutputLimits) -> Self {
        Self {
            limits,
            decoder: Utf8StreamDecoder::new(),
            chunks: VecDeque::new(),
            stored_bytes: 0,
            stored_newlines: 0,
            full: false,
            total_bytes: 0,
            total_newlines: 0,
            ends_with_newline: true,
        }
    }

    /// Bytes currently held; bounded by the limits plus one chunk.
    pub fn stored_bytes(&self) -> usize {
        self.stored_bytes
    }

    /// Accept a string chunk; returns whether anything was accepted.
    pub fn push_str(&mut self, chunk: &str) -> bool {
        let mut combined = self.decoder.decode(&[], false);
        combined.push_str(chunk);
        self.accept(combined)
    }

    /// Accept raw UTF-8 stream bytes; returns whether anything was accepted.
    pub fn push_bytes(&mut self, chunk: &[u8]) -> bool {
        let text = self.decoder.decode(chunk, true);
        self.accept(text)
    }

    /// Flush an incomplete trailing character as a replacement character; call when the stream ends.
    pub fn end(&mut self) {
        let trailing = self.decoder.decode(&[], false);
        self.accept(trailing);
    }

    fn accept(&mut self, text: String) -> bool {
        if text.is_empty() {
            return false;
        }
        let bytes = utf8_byte_length(&text);
        let newlines = count_newlines(&text);
        self.total_bytes += bytes;
        self.total_newlines += newlines;
        self.ends_with_newline = text.ends_with('\n');
        if self.full {
            return true;
        }
        self.chunks.push_back(OutputChunk { text, bytes, newlines });
        self.stored_bytes += bytes;
        self.stored_newlines += newlines;
        if self.limits.retain == OutputRetainMode::Head {
            self.full = self.stored_bytes > self.limits.max_bytes || self.stored_newlines >= self.limits.max_lines;
            return true;
        }
        while self.chunks.len() > 1 {
            let Some(first) = self.chunks.front() else {
                break;
            };
            let bytes_after = self.stored_bytes - first.bytes;
            let newlines_after = self.stored_newlines - first.newlines;
            if bytes_after <= self.limits.max_bytes + 1 && newlines_after <= self.limits.max_lines + 1 {
                break;
            }
            if let Some(dropped) = self.chunks.pop_front() {
                self.stored_bytes -= dropped.bytes;
                self.stored_newlines -= dropped.newlines;
            }
        }
        true
    }

    /// Retained, sanitized output and what the limits dropped from the whole stream.
    pub fn snapshot(&mut self) -> BoundedOutput {
        let stored = if self.chunks.len() == 1 {
            self.chunks.front().map(|c| c.text.clone()).unwrap_or_default()
        } else {
            self.chunks.iter().map(|c| c.text.as_str()).collect::<String>()
        };
        let kept = bound_output(&stored, self.limits);
        let stored_lines = lines(self.stored_newlines, stored.is_empty() || stored.ends_with('\n'));
        let kept_lines = stored_lines.saturating_sub(kept.dropped_lines);
        if self.limits.retain == OutputRetainMode::Tail || self.chunks.len() > 1 {
            let text = if self.limits.retain == OutputRetainMode::Tail {
                kept.text.clone()
            } else {
                stored
            };
            let bytes = if self.limits.retain == OutputRetainMode::Tail {
                kept.bytes
            } else {
                self.stored_bytes
            };
            self.chunks.clear();
            if !text.is_empty() {
                let newlines = count_newlines(&text);
                self.chunks.push_back(OutputChunk { text, bytes, newlines });
            }
            self.stored_bytes = bytes;
            self.stored_newlines = self.chunks.front().map(|c| c.newlines).unwrap_or(0);
        }
        BoundedOutput {
            text: sanitize_output(&kept.text),
            dropped_bytes: self.total_bytes.saturating_sub(kept.bytes),
            dropped_lines: lines(self.total_newlines, self.ends_with_newline).saturating_sub(kept_lines),
        }
    }
}

fn lines(newlines: usize, terminated: boolean_alias::Bool) -> usize {
    newlines + usize::from(!terminated)
}

mod boolean_alias {
    pub type Bool = bool;
}

fn count_newlines(text: &str) -> usize {
    text.bytes().filter(|&b| b == NEWLINE).count()
}

/// Minimum pause between progress commits; each commit also buys a pause proportional to what it wrote.
const MIN_PROGRESS_INTERVAL_MS: f64 = 100.0;
const PROGRESS_BYTES_PER_SECOND: f64 = 100.0 * 1024.0;

pub type ProgressWriteFn = Arc<dyn Fn() -> pi_js::BoxFuture<pi_js::Result<usize>> + Send + Sync>;
pub type ProgressErrorFn = Arc<dyn Fn(pi_js::Error) + Send + Sync>;

struct ProgressState {
    waiters: Vec<oneshot::Sender<pi_js::Result<()>>>,
    timer_handle: Option<tokio::task::JoinHandle<()>>,
    in_flight: bool,
    in_flight_notify: Vec<oneshot::Sender<()>>,
    next_at: i64,
    dirty: bool,
    stopped: bool,
}

/// Adaptive progress commits: first change commits immediately; subsequent commits wait at least
/// 100 ms and proportional to bytes written at 100 KiB/s. At most one commit is in flight.
#[derive(Clone)]
pub struct Progress {
    inner: Arc<ProgressInner>,
}

struct ProgressInner {
    write: ProgressWriteFn,
    on_error: ProgressErrorFn,
    state: Mutex<ProgressState>,
}

impl Progress {
    pub fn new(write: ProgressWriteFn, on_error: ProgressErrorFn) -> Self {
        Self {
            inner: Arc::new(ProgressInner {
                write,
                on_error,
                state: Mutex::new(ProgressState {
                    waiters: Vec::new(),
                    timer_handle: None,
                    in_flight: false,
                    in_flight_notify: Vec::new(),
                    next_at: 0,
                    dirty: false,
                    stopped: false,
                }),
            }),
        }
    }

    /// Schedule a commit.
    pub fn mark(&self) {
        let should_flush = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.dirty = true;
            Self::schedule_locked(&self.inner, &mut guard)
        };
        if should_flush {
            Self::spawn_flush(Arc::clone(&self.inner));
        }
    }

    /// Schedule a commit; the future settles with the commit that includes this change.
    pub fn mark_and_wait(&self) -> Pin<Box<dyn Future<Output = pi_js::Result<()>> + Send>> {
        let (tx, rx) = oneshot::channel();
        let should_flush = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.waiters.push(tx);
            guard.dirty = true;
            Self::schedule_locked(&self.inner, &mut guard)
        };
        if should_flush {
            Self::spawn_flush(Arc::clone(&self.inner));
        }
        Box::pin(async move {
            match rx.await {
                Ok(result) => result,
                Err(_) => Ok(()),
            }
        })
    }

    /// Stop committing and wait for any commit in flight; returns unsettled waiters the final commit must settle.
    pub async fn stop(&self) -> Vec<oneshot::Sender<pi_js::Result<()>>> {
        let in_flight_rx = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.stopped = true;
            if let Some(handle) = guard.timer_handle.take() {
                handle.abort();
            }
            if guard.in_flight {
                let (tx, rx) = oneshot::channel();
                guard.in_flight_notify.push(tx);
                Some(rx)
            } else {
                None
            }
        };
        if let Some(rx) = in_flight_rx {
            let _ = rx.await;
        }
        let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut guard.waiters)
    }

    fn schedule_locked(inner: &Arc<ProgressInner>, state: &mut ProgressState) -> bool {
        if state.stopped || state.timer_handle.is_some() || state.in_flight {
            return false;
        }
        let wait = state.next_at - pi_js::time::now_ms();
        if wait <= 0 {
            if !state.dirty {
                return false;
            }
            state.dirty = false;
            state.in_flight = true;
            true
        } else {
            let weak = Arc::downgrade(inner);
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(wait as u64);
            state.timer_handle = Some(tokio::spawn(async move {
                tokio::time::sleep_until(deadline).await;
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                let should_flush = {
                    let mut guard = inner.state.lock().unwrap_or_else(|e| e.into_inner());
                    guard.timer_handle = None;
                    if guard.stopped || !guard.dirty || guard.in_flight {
                        false
                    } else {
                        guard.dirty = false;
                        guard.in_flight = true;
                        true
                    }
                };
                if should_flush {
                    Self::spawn_flush(inner);
                }
            }));
            false
        }
    }

    fn spawn_flush(inner: Arc<ProgressInner>) {
        tokio::spawn(async move {
            let waiters = {
                let mut guard = inner.state.lock().unwrap_or_else(|e| e.into_inner());
                std::mem::take(&mut guard.waiters)
            };
            let started = pi_js::time::now_ms();
            let result = (inner.write)().await;
            let (next_at, finish_res) = match result {
                Ok(bytes) => {
                    let pause = MIN_PROGRESS_INTERVAL_MS.max((bytes as f64 * 1000.0) / PROGRESS_BYTES_PER_SECOND);
                    (started + pause as i64, Ok(()))
                }
                Err(err) => (started + MIN_PROGRESS_INTERVAL_MS as i64, Err(err)),
            };
            match finish_res {
                Ok(()) => {
                    for waiter in waiters {
                        let _ = waiter.send(Ok(()));
                    }
                }
                Err(err) => {
                    let err_msg = err.to_string();
                    for waiter in waiters {
                        let _ = waiter.send(Err(pi_js::Error::msg(err_msg.clone())));
                    }
                    (inner.on_error)(err);
                }
            }
            let should_flush = {
                let mut guard = inner.state.lock().unwrap_or_else(|e| e.into_inner());
                guard.next_at = next_at;
                guard.in_flight = false;
                for notify in guard.in_flight_notify.drain(..) {
                    let _ = notify.send(());
                }
                if guard.dirty {
                    Self::schedule_locked(&inner, &mut guard)
                } else {
                    false
                }
            };
            if should_flush {
                Self::spawn_flush(inner);
            }
        });
    }
}
