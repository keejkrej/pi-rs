//! Port of packages/coding-agent/src/core/tools/truncate.ts

use serde::{Deserialize, Serialize};

pub const DEFAULT_MAX_LINES: usize = 2000;
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024; // 50KB
pub const GREP_MAX_LINE_LENGTH: usize = 500; // Max chars per grep match line

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TruncatedBy {
    Lines,
    Bytes,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TruncationResult {
    /// The truncated content
    pub content: String,
    /// Whether truncation occurred
    pub truncated: bool,
    /// Which limit was hit: "lines", "bytes", or null if not truncated
    pub truncated_by: Option<TruncatedBy>,
    /// Total number of lines in the original content
    pub total_lines: i64,
    /// Total number of bytes in the original content
    pub total_bytes: i64,
    /// Number of complete lines in the truncated output
    pub output_lines: i64,
    /// Number of bytes in the truncated output
    pub output_bytes: i64,
    /// Whether the last line was partially truncated (only for tail truncation edge case)
    pub last_line_partial: bool,
    /// Whether the first line exceeded the byte limit (for head truncation)
    pub first_line_exceeds_limit: bool,
    /// The max lines limit that was applied
    pub max_lines: i64,
    /// The max bytes limit that was applied
    pub max_bytes: i64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TruncationOptions {
    /// Maximum number of lines (default: 2000)
    pub max_lines: Option<usize>,
    /// Maximum number of bytes (default: 50KB)
    pub max_bytes: Option<usize>,
}

fn split_lines_for_counting(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = content.split('\n').collect();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// Format bytes as human-readable size.
pub fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{}KB", pi_js::num::to_fixed(bytes as f64 / 1024.0, 1))
    } else {
        format!("{}MB", pi_js::num::to_fixed(bytes as f64 / (1024.0 * 1024.0), 1))
    }
}

/// Truncate content from the head (keep first N lines/bytes).
/// Suitable for file reads where you want to see the beginning.
///
/// Never returns partial lines. If first line exceeds byte limit,
/// returns empty content with firstLineExceedsLimit=true.
pub fn truncate_head(content: &str, options: Option<TruncationOptions>) -> TruncationResult {
    let opts = options.unwrap_or_default();
    let max_lines = opts.max_lines.unwrap_or(DEFAULT_MAX_LINES);
    let max_bytes = opts.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);

    let total_bytes = content.len();
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len();

    // Check if no truncation needed
    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: content.to_string(),
            truncated: false,
            truncated_by: None,
            total_lines: total_lines as i64,
            total_bytes: total_bytes as i64,
            output_lines: total_lines as i64,
            output_bytes: total_bytes as i64,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines: max_lines as i64,
            max_bytes: max_bytes as i64,
        };
    }

    // Check if first line alone exceeds byte limit
    let first_line_bytes = lines.first().map_or(0, |l| l.len());
    if first_line_bytes > max_bytes {
        return TruncationResult {
            content: String::new(),
            truncated: true,
            truncated_by: Some(TruncatedBy::Bytes),
            total_lines: total_lines as i64,
            total_bytes: total_bytes as i64,
            output_lines: 0,
            output_bytes: 0,
            last_line_partial: false,
            first_line_exceeds_limit: true,
            max_lines: max_lines as i64,
            max_bytes: max_bytes as i64,
        };
    }

    // Collect complete lines that fit
    let mut output_lines_arr: Vec<&str> = Vec::new();
    let mut output_bytes_count = 0usize;
    let mut truncated_by = TruncatedBy::Lines;

    for (i, &line) in lines.iter().enumerate() {
        if i >= max_lines {
            break;
        }
        let line_bytes = line.len() + usize::from(i > 0); // +1 for newline
        if output_bytes_count + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            break;
        }
        output_lines_arr.push(line);
        output_bytes_count += line_bytes;
    }

    // If we exited due to line limit
    if output_lines_arr.len() >= max_lines && output_bytes_count <= max_bytes {
        truncated_by = TruncatedBy::Lines;
    }

    let output_content = output_lines_arr.join("\n");
    let final_output_bytes = output_content.len();

    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines: total_lines as i64,
        total_bytes: total_bytes as i64,
        output_lines: output_lines_arr.len() as i64,
        output_bytes: final_output_bytes as i64,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines: max_lines as i64,
        max_bytes: max_bytes as i64,
    }
}

/// Truncate content from the tail (keep last N lines/bytes).
/// Suitable for bash output where you want to see the end (errors, final results).
///
/// May return partial first line if the last line of original content exceeds byte limit.
pub fn truncate_tail(content: &str, options: Option<TruncationOptions>) -> TruncationResult {
    let opts = options.unwrap_or_default();
    let max_lines = opts.max_lines.unwrap_or(DEFAULT_MAX_LINES);
    let max_bytes = opts.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);

    let total_bytes = content.len();
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len();

    // Check if no truncation needed
    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: content.to_string(),
            truncated: false,
            truncated_by: None,
            total_lines: total_lines as i64,
            total_bytes: total_bytes as i64,
            output_lines: total_lines as i64,
            output_bytes: total_bytes as i64,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines: max_lines as i64,
            max_bytes: max_bytes as i64,
        };
    }

    // Work backwards from the end
    let mut output_lines_arr: Vec<String> = Vec::new();
    let mut output_bytes_count = 0usize;
    let mut truncated_by = TruncatedBy::Lines;
    let mut last_line_partial = false;

    for &line in lines.iter().rev() {
        if output_lines_arr.len() >= max_lines {
            break;
        }
        let line_bytes = line.len() + usize::from(!output_lines_arr.is_empty()); // +1 for newline
        if output_bytes_count + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            // Edge case: if we haven't added ANY lines yet and this line exceeds maxBytes,
            // take the end of the line (partial)
            if output_lines_arr.is_empty() {
                let truncated_line = truncate_string_to_bytes_from_end(line, max_bytes);
                output_bytes_count = truncated_line.len();
                output_lines_arr.insert(0, truncated_line);
                last_line_partial = true;
            }
            break;
        }
        output_lines_arr.insert(0, line.to_string());
        output_bytes_count += line_bytes;
    }

    // If we exited due to line limit
    if output_lines_arr.len() >= max_lines && output_bytes_count <= max_bytes {
        truncated_by = TruncatedBy::Lines;
    }

    let output_content = output_lines_arr.join("\n");
    let final_output_bytes = output_content.len();

    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines: total_lines as i64,
        total_bytes: total_bytes as i64,
        output_lines: output_lines_arr.len() as i64,
        output_bytes: final_output_bytes as i64,
        last_line_partial,
        first_line_exceeds_limit: false,
        max_lines: max_lines as i64,
        max_bytes: max_bytes as i64,
    }
}

/// Truncate a string to fit within a byte limit (from the end).
/// Handles multi-byte UTF-8 characters correctly.
fn truncate_string_to_bytes_from_end(s: &str, max_bytes: usize) -> String {
    let bytes = s.as_bytes();
    if bytes.len() <= max_bytes {
        return s.to_string();
    }
    let mut start = bytes.len().saturating_sub(max_bytes);
    while start < bytes.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].to_string()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TruncateLineResult {
    pub text: String,
    pub was_truncated: bool,
}

/// Truncate a single line to max characters, adding [truncated] suffix.
/// Used for grep match lines.
pub fn truncate_line(line: &str, max_chars: Option<usize>) -> TruncateLineResult {
    let max_chars = max_chars.unwrap_or(GREP_MAX_LINE_LENGTH);
    let len16 = pi_js::str16::len(line);
    if len16 <= max_chars {
        return TruncateLineResult {
            text: line.to_string(),
            was_truncated: false,
        };
    }
    // PORT: do not split surrogate pairs when slicing to UTF-16 limit (9e870f6f0 / safe Rust UTF-8 boundary)
    let mut units = 0usize;
    let mut end_byte = 0usize;
    for (idx, ch) in line.char_indices() {
        let ch_units = ch.len_utf16();
        if units + ch_units > max_chars {
            break;
        }
        units += ch_units;
        end_byte = idx + ch.len_utf8();
    }
    TruncateLineResult {
        text: format!("{}... [truncated]", &line[..end_byte]),
        was_truncated: true,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MiddleTruncationResult {
    /// The start and end of the content with a `…N chars truncated…` marker between them.
    pub content: String,
    pub truncated: bool,
    /// Characters left out.
    pub removed_chars: usize,
    pub total_bytes: usize,
    pub total_lines: usize,
}

/// Keep the start and the end of `content`, half of `maxBytes` each, and replace the middle with a
/// `…N chars truncated…` marker, like Codex does for tool output. Cuts only at character boundaries.
pub fn truncate_middle(content: &str, max_bytes: usize) -> MiddleTruncationResult {
    let total_bytes = content.len();
    let total_lines = split_lines_for_counting(content).len();
    if total_bytes <= max_bytes {
        return MiddleTruncationResult {
            content: content.to_string(),
            truncated: false,
            removed_chars: 0,
            total_bytes,
            total_lines,
        };
    }
    let half = max_bytes / 2;
    let mut head_end = half.min(total_bytes);
    while head_end > 0 && !content.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = total_bytes.saturating_sub(max_bytes - half);
    while tail_start < total_bytes && !content.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    if tail_start < head_end {
        tail_start = head_end;
    }
    let head = &content[..head_end];
    let tail = &content[tail_start..];
    let removed_chars = content[head_end..tail_start].chars().count();
    MiddleTruncationResult {
        content: format!("{head}\u{2026}{removed_chars} chars truncated\u{2026}{tail}"),
        truncated: true,
        removed_chars,
        total_bytes,
        total_lines,
    }
}
