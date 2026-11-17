//! Port of packages/coding-agent/src/core/bash-executor.ts

use std::future::Future;
use std::io::Write;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use pi_js::Result;
use pi_js::abort::AbortSignal;
use pi_js::text::Utf8StreamDecoder;

pub use crate::core::tools::bash::{BashExecOptions, BashExecResult, BashOperations};
use crate::core::tools::truncate::truncate_tail as truncate_tail_with_options;
pub use crate::core::tools::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncatedBy, TruncationResult};
use crate::utils::ansi::strip_ansi;
use crate::utils::shell::sanitize_binary_output;

pub fn truncate_tail(content: &str) -> TruncationResult {
    truncate_tail_with_options(content, None)
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
) -> Pin<Box<dyn Future<Output = Result<BashResult>> + Send + 'a>> {
    Box::pin(execute_bash_with_operations(command, cwd, operations, options))
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
    let on_data: Arc<dyn Fn(&[u8]) + Send + Sync> = Arc::new(move |data: &[u8]| {
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
            BashExecOptions {
                on_data: Some(on_data),
                signal: opts.signal.clone(),
                timeout: None,
                env: None,
            },
        )
        .await;

    let mut acc = state
        .lock()
        .map_err(|_| pi_js::Error::msg("bash accumulator poisoned"))?;
    let remaining = acc.decoder.decode(&[], false);
    if !remaining.is_empty() {
        let tail_text = sanitize_binary_output(&strip_ansi(&remaining)).replace('\r', "");
        if let Some(ref mut file) = acc.temp_file {
            let _ = file.write_all(tail_text.as_bytes());
        }
        acc.output_chunks.push(tail_text);
    }
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
                exit_code: if cancelled {
                    None
                } else {
                    result.exit_code.and_then(|c| i32::try_from(c).ok())
                },
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
