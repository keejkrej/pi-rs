//! Port of packages/coding-agent/src/core/bash-executor.ts

use std::io::Write;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_js::abort::AbortSignal;
use pi_js::text::Utf8StreamDecoder;
use pi_js::{BoxFuture, Result};

use crate::utils::ansi::strip_ansi;
use crate::utils::shell::sanitize_binary_output;

// WIRE(@codemode-w3): unify with `crate::core::tools::truncate::DEFAULT_MAX_BYTES` and `truncate_tail`.
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;
pub const DEFAULT_MAX_LINES: usize = 2000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TruncationResult {
    pub content: String,
    pub truncated: bool,
    pub truncated_by: Option<&'static str>,
    pub total_lines: usize,
    pub total_bytes: usize,
    pub output_lines: usize,
    pub output_bytes: usize,
    pub last_line_partial: bool,
    pub first_line_exceeds_limit: bool,
    pub max_lines: usize,
    pub max_bytes: usize,
}

fn truncate_tail_utf8_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut start = text.len() - max_bytes;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_owned()
}

pub fn truncate_tail(content: &str) -> TruncationResult {
    let total_bytes = content.len();
    let lines: Vec<&str> = content.split('\n').collect();
    let total_lines = lines.len();

    if total_lines <= DEFAULT_MAX_LINES && total_bytes <= DEFAULT_MAX_BYTES {
        return TruncationResult {
            content: content.to_owned(),
            truncated: false,
            truncated_by: None,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines: DEFAULT_MAX_LINES,
            max_bytes: DEFAULT_MAX_BYTES,
        };
    }

    let mut output_lines_vec: Vec<String> = Vec::new();
    let mut output_bytes_count = 0usize;
    let mut truncated_by = "lines";

    for i in (0..lines.len()).rev() {
        if output_lines_vec.len() >= DEFAULT_MAX_LINES {
            break;
        }
        let line = lines[i];
        let line_bytes = line.len() + usize::from(!output_lines_vec.is_empty());
        if output_bytes_count + line_bytes > DEFAULT_MAX_BYTES {
            truncated_by = "bytes";
            if output_lines_vec.is_empty() {
                let truncated_line = truncate_tail_utf8_bytes(line, DEFAULT_MAX_BYTES);
                let out_bytes = truncated_line.len();
                return TruncationResult {
                    output_bytes: out_bytes,
                    output_lines: 1,
                    content: truncated_line,
                    truncated: true,
                    truncated_by: Some("bytes"),
                    total_lines,
                    total_bytes,
                    last_line_partial: true,
                    first_line_exceeds_limit: false,
                    max_lines: DEFAULT_MAX_LINES,
                    max_bytes: DEFAULT_MAX_BYTES,
                };
            }
            break;
        }
        output_lines_vec.push(line.to_owned());
        output_bytes_count += line_bytes;
    }

    output_lines_vec.reverse();
    let output_content = output_lines_vec.join("\n");
    let out_bytes = output_content.len();
    let out_lines = output_lines_vec.len();

    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        output_lines: out_lines,
        output_bytes: out_bytes,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines: DEFAULT_MAX_LINES,
        max_bytes: DEFAULT_MAX_BYTES,
    }
}

pub struct BashOperationsExecOptions {
    pub on_data: Box<dyn FnMut(&[u8]) + Send>,
    pub signal: Option<AbortSignal>,
    pub timeout: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BashOperationsResult {
    pub exit_code: Option<i32>,
}

/// WIRE(@codemode-w3): unify with `crate::core::tools::bash::BashOperations`.
#[async_trait]
pub trait BashOperations: Send + Sync {
    async fn exec(&self, command: &str, cwd: &str, options: BashOperationsExecOptions) -> Result<BashOperationsResult>;
}

#[derive(Clone, Default)]
pub struct BashExecutorOptions {
    pub on_chunk: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    pub signal: Option<AbortSignal>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BashResult {
    pub output: String,
    pub exit_code: Option<i32>,
    pub cancelled: bool,
    pub truncated: bool,
    pub full_output_path: Option<String>,
}

pub fn execute_bash_with_operations_future<'a>(
    command: &'a str,
    cwd: &'a str,
    operations: &'a dyn BashOperations,
    options: Option<BashExecutorOptions>,
) -> BoxFuture<Result<BashResult>> {
    let command = command.to_owned();
    let cwd = cwd.to_owned();
    // Helper for dyn callers if needed; primary entrypoint below takes `&dyn BashOperations`.
    let _ = (&command, &cwd, operations, options);
    unreachable!("use execute_bash_with_operations")
}

pub async fn execute_bash_with_operations(
    command: &str,
    cwd: &str,
    operations: &dyn BashOperations,
    options: Option<BashExecutorOptions>,
) -> Result<BashResult> {
    let opts = options.unwrap_or_default();
    let max_output_bytes = DEFAULT_MAX_BYTES * 2;

    struct Accumulator {
        output_chunks: Vec<String>,
        output_bytes: usize,
        temp_file_path: Option<String>,
        temp_file: Option<std::fs::File>,
        total_bytes: usize,
        decoder: Utf8StreamDecoder,
        on_chunk: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    }

    impl Accumulator {
        fn ensure_temp_file(&mut self) {
            if self.temp_file_path.is_some() {
                return;
            }
            let id = hex::encode(pi_js::crypto::random_bytes(8));
            let path = pi_js::path::join(&[&pi_js::env::tmp_dir(), &format!("pi-bash-{id}.log")]);
            if let Ok(mut file) = std::fs::File::create(&path) {
                for chunk in &self.output_chunks {
                    let _ = file.write_all(chunk.as_bytes());
                }
                self.temp_file_path = Some(path);
                self.temp_file = Some(file);
            }
        }
    }

    let state = Arc::new(Mutex::new(Accumulator {
        output_chunks: Vec::new(),
        output_bytes: 0,
        temp_file_path: None,
        temp_file: None,
        total_bytes: 0,
        decoder: Utf8StreamDecoder::new(),
        on_chunk: opts.on_chunk.clone(),
    }));

    let state_cb = state.clone();
    let on_data = Box::new(move |data: &[u8]| {
        let on_chunk_cb;
        let text;
        {
            let Ok(mut acc) = state_cb.lock() else {
                return;
            };
            acc.total_bytes += data.len();
            let decoded = acc.decoder.decode(data, true);
            text = sanitize_binary_output(&strip_ansi(&decoded)).replace('\r', "");

            if acc.total_bytes > DEFAULT_MAX_BYTES {
                acc.ensure_temp_file();
            }
            if let Some(ref mut file) = acc.temp_file {
                let _ = file.write_all(text.as_bytes());
            }
            acc.output_bytes += text.len();
            acc.output_chunks.push(text.clone());
            while acc.output_bytes > max_output_bytes && acc.output_chunks.len() > 1 {
                let removed_len = acc.output_chunks.drain(..1).next().map_or(0, |s| s.len());
                acc.output_bytes = acc.output_bytes.saturating_sub(removed_len);
            }
            on_chunk_cb = acc.on_chunk.clone();
        }
        if let Some(cb) = on_chunk_cb {
            cb(&text);
        }
    });

    let exec_res = operations
        .exec(
            command,
            cwd,
            BashOperationsExecOptions {
                on_data,
                signal: opts.signal.clone(),
                timeout: None,
            },
        )
        .await;

    let mut acc = state
        .lock()
        .map_err(|_| pi_js::Error::msg("bash accumulator poisoned"))?;
    let full_output = acc.output_chunks.join("");
    let truncation_result = truncate_tail(&full_output);
    if truncation_result.truncated {
        acc.ensure_temp_file();
    }
    if let Some(mut file) = acc.temp_file.take() {
        let _ = file.flush();
    }
    let full_output_path = acc.temp_file_path.clone();

    match exec_res {
        Ok(result) => {
            let cancelled = opts.signal.as_ref().is_some_and(AbortSignal::aborted);
            Ok(BashResult {
                output: if truncation_result.truncated {
                    truncation_result.content
                } else {
                    full_output
                },
                exit_code: if cancelled { None } else { result.exit_code },
                cancelled,
                truncated: truncation_result.truncated,
                full_output_path,
            })
        }
        Err(err) => {
            if opts.signal.as_ref().is_some_and(AbortSignal::aborted) {
                return Ok(BashResult {
                    output: if truncation_result.truncated {
                        truncation_result.content
                    } else {
                        full_output
                    },
                    exit_code: None,
                    cancelled: true,
                    truncated: truncation_result.truncated,
                    full_output_path,
                });
            }
            Err(err)
        }
    }
}
