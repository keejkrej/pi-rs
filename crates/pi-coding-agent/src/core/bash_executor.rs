//! Port of packages/coding-agent/src/core/bash-executor.ts

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use pi_js::Result;
use pi_js::abort::AbortSignal;
use pi_js::text::Utf8StreamDecoder;

pub use crate::core::tools::bash::{BashExecOptions, BashExecResult, BashOperations};
use crate::core::tools::truncate::truncate_tail as truncate_tail_with_options;
pub use crate::core::tools::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncatedBy, TruncationResult};
use crate::utils::ansi::{split_incomplete_ansi_suffix, strip_ansi};
use crate::utils::output_files::create_output_file_stream;
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
        discarded_output: bool,
        temp_file_path: Option<String>,
        temp_file: Option<pi_js::fs::File>,
        total_bytes: usize,
        decoder: Utf8StreamDecoder,
        pending_ansi: String,
        on_chunk: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    }

    impl Accumulator {
        fn ensure_temp_file(&mut self) {
            if self.temp_file_path.is_some() {
                return;
            }
            if let Ok((path, mut file)) = create_output_file_stream("pi-bash", ".log") {
                for chunk in &self.output_chunks {
                    let _ = file.write_all(chunk.as_bytes());
                }
                self.temp_file_path = Some(path);
                self.temp_file = Some(file);
            }
        }

        fn append_text(
            &mut self,
            raw_text: &str,
            max_output_bytes: usize,
        ) -> Option<(String, Option<Arc<dyn Fn(&str) + Send + Sync>>)> {
            let text = sanitize_binary_output(&strip_ansi(raw_text)).replace('\r', "");
            if text.is_empty() {
                return None;
            }
            if self.total_bytes > DEFAULT_MAX_BYTES {
                self.ensure_temp_file();
            }
            if let Some(ref mut file) = self.temp_file {
                let _ = file.write_all(text.as_bytes());
            }
            self.output_bytes += text.len();
            self.output_chunks.push(text.clone());
            while self.output_bytes > max_output_bytes && self.output_chunks.len() > 1 {
                let removed_len = self.output_chunks.drain(..1).next().map_or(0, |s| s.len());
                self.output_bytes = self.output_bytes.saturating_sub(removed_len);
                self.discarded_output = true;
            }
            Some((text, self.on_chunk.clone()))
        }
    }

    let state = Arc::new(Mutex::new(Accumulator {
        output_chunks: Vec::new(),
        output_bytes: 0,
        discarded_output: false,
        temp_file_path: None,
        temp_file: None,
        total_bytes: 0,
        decoder: Utf8StreamDecoder::new(),
        pending_ansi: String::new(),
        on_chunk: opts.on_chunk.clone(),
    }));

    let state_cb = state.clone();
    let on_data: Arc<dyn Fn(&[u8]) + Send + Sync> = Arc::new(move |data: &[u8]| {
        let emitted = {
            let Ok(mut acc) = state_cb.lock() else {
                return;
            };
            acc.total_bytes += data.len();
            let decoded = acc.decoder.decode(data, true);
            let combined = format!("{}{decoded}", acc.pending_ansi);
            let split = split_incomplete_ansi_suffix(&combined);
            acc.pending_ansi = split.pending;
            acc.append_text(&split.complete, max_output_bytes)
        };
        if let Some((text, Some(cb))) = emitted {
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

    let cancelled = opts.signal.as_ref().is_some_and(AbortSignal::aborted);
    let exit_code = match exec_res {
        Ok(result) => {
            if cancelled {
                None
            } else {
                result.exit_code.and_then(|c| i32::try_from(c).ok())
            }
        }
        Err(err) => {
            if !cancelled {
                if let Ok(mut acc) = state.lock()
                    && let Some(file) = acc.temp_file.take()
                {
                    let _ = file.sync();
                }
                return Err(err);
            }
            None
        }
    };

    let (flushed_emitted, full_output, truncation_result, discarded_output, full_output_path) = {
        let mut acc = state
            .lock()
            .map_err(|_| pi_js::Error::msg("bash accumulator poisoned"))?;
        let rest_decoded = acc.decoder.decode(&[], false);
        let rest = format!("{}{rest_decoded}", acc.pending_ansi);
        acc.pending_ansi.clear();
        let flushed = acc.append_text(&rest, max_output_bytes);
        let full_output = acc.output_chunks.join("");
        let truncation_result = truncate_tail(&full_output);
        if truncation_result.truncated {
            acc.ensure_temp_file();
        }
        if let Some(file) = acc.temp_file.take() {
            let _ = file.sync();
        }
        (
            flushed,
            full_output,
            truncation_result,
            acc.discarded_output,
            acc.temp_file_path.clone(),
        )
    };
    if let Some((text, Some(cb))) = flushed_emitted {
        cb(&text);
    }

    Ok(BashResult {
        output: if truncation_result.truncated {
            truncation_result.content
        } else {
            full_output
        },
        exit_code,
        cancelled,
        truncated: discarded_output || truncation_result.truncated,
        full_output_path,
    })
}
