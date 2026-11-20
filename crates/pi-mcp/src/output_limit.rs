//! Port of packages/mcp/src/output-limit.ts

use std::sync::Arc;

use pi_js::{BoxFuture, Result};

use crate::protocol::content::LlmContent;

/// Model-facing text of an MCP result beyond this is cut in the middle.
pub const MCP_OUTPUT_MAX_BYTES: usize = 20 * 1024;

/// Data passed to [`McpOutputSaver`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpOutputData {
    Text(String),
    Bytes(Vec<u8>),
}

impl McpOutputData {
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Text(s) => s.as_bytes(),
            Self::Bytes(b) => b.as_slice(),
        }
    }
}

/// Saves data the model cannot get in full, such as the full text of a truncated result, and returns
/// where it was saved (a path the model can read). `extension` includes the dot, for example `.txt`.
pub type McpOutputSaver = Arc<dyn Fn(McpOutputData, String) -> BoxFuture<Result<String>> + Send + Sync>;

/// Model-facing content within the limit, and where the full text went when it was saved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LimitedMcpContent {
    pub content: Vec<LlmContent>,
    pub full_output_path: Option<String>,
}

/// Text cut in the middle, with the size of the whole text.
struct MiddleTruncation {
    /// The start and the end of the text with a `…N chars truncated…` marker between them.
    content: String,
    total_bytes: usize,
    total_lines: usize,
}

/// Keep the start and the end of `text`, half of `max_bytes` each, and replace the middle with a
/// `…N chars truncated…` marker. Cuts only at character boundaries. `None` when `text` fits.
fn truncate_middle(text: &str, max_bytes: usize) -> Option<MiddleTruncation> {
    let total_bytes = text.len();
    if total_bytes <= max_bytes {
        return None;
    }
    let half = max_bytes / 2;
    let mut head_end = half;
    while head_end > 0 && !text.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = total_bytes.saturating_sub(max_bytes - half);
    while tail_start < total_bytes && !text.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    let tail_start = tail_start.max(head_end);
    let head = &text[..head_end];
    let tail = &text[tail_start..];
    let removed_chars = text[head_end..tail_start].chars().count();
    // A final newline ends the last line instead of starting another.
    let total_lines = text.split('\n').count() - usize::from(text.ends_with('\n'));
    Some(MiddleTruncation {
        content: format!("{head}…{removed_chars} chars truncated…{tail}"),
        total_bytes,
        total_lines,
    })
}

/// Keep model-facing text within [`MCP_OUTPUT_MAX_BYTES`]. Longer text becomes one text block in
/// Codex's truncation format, followed by the path of the file with the full text when `save_output`
/// is given; images follow it. Content within the limit is returned as it is.
pub async fn limit_mcp_content(content: Vec<LlmContent>, save_output: Option<McpOutputSaver>) -> LimitedMcpContent {
    let text_parts: Vec<&str> = content
        .iter()
        .filter_map(|block| match block {
            LlmContent::Text { text } => Some(text.as_str()),
            LlmContent::Image { .. } => None,
        })
        .collect();
    let combined = text_parts.join("\n");
    let Some(truncation) = truncate_middle(&combined, MCP_OUTPUT_MAX_BYTES) else {
        return LimitedMcpContent {
            content,
            full_output_path: None,
        };
    };

    let mut full_output_path = None;
    let mut where_suffix = String::new();
    if let Some(save) = save_output {
        match save(McpOutputData::Text(combined), ".txt".to_string()).await {
            Ok(path) => {
                where_suffix = format!("\n\n[Full output: {path} (read it with offset/limit)]");
                full_output_path = Some(path);
            }
            Err(error) => {
                where_suffix = format!("\n\n[Could not save the full output: {error}]");
            }
        }
    }

    let tokens = truncation.total_bytes.div_ceil(4);
    let text = format!(
        "Warning: truncated output (original token count: {tokens})\nTotal output lines: {}\n\n{}{where_suffix}",
        truncation.total_lines, truncation.content
    );
    let mut out = Vec::with_capacity(1 + content.len());
    out.push(LlmContent::Text { text });
    for block in content {
        if matches!(block, LlmContent::Image { .. }) {
            out.push(block);
        }
    }
    LimitedMcpContent {
        content: out,
        full_output_path,
    }
}
