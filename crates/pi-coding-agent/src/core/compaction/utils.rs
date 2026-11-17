//! Shared utilities for compaction and branch summarization.
//!
//! Port of `packages/coding-agent/src/core/compaction/utils.ts`.

use indexmap::IndexSet;
use pi_agent_core::AgentMessage;
use pi_ai::{AssistantContentBlock, JsonObject, Message, content_text};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ============================================================================
// File Operation Tracking
// ============================================================================

/// Cumulative read/written/edited file sets tracked across compacted turns.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOperations {
    pub read: IndexSet<String>,
    pub written: IndexSet<String>,
    pub edited: IndexSet<String>,
}

/// Read-only vs modified file lists extracted from [`FileOperations`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileLists {
    pub read_files: Vec<String>,
    pub modified_files: Vec<String>,
}

pub fn create_file_ops() -> FileOperations {
    FileOperations::default()
}

/// Extract file operations from tool calls in an assistant message, or from the nested calls
/// recorded on a tool result.
pub fn extract_file_ops_from_message(message: &AgentMessage, file_ops: &mut FileOperations) {
    let AgentMessage::Message(msg) = message else {
        return;
    };
    match msg {
        Message::ToolResult(tool_result) => {
            // Calls made from codemode scripts are recorded on the script's result.
            if let Some(nested) = &tool_result.nested_calls {
                for call in &nested.calls {
                    add_file_op(&call.name, call.arguments.as_ref(), file_ops);
                }
            }
        }
        Message::Assistant(assistant) => {
            for block in &assistant.content {
                if let AssistantContentBlock::ToolCall(tc) = block {
                    add_file_op(&tc.name, Some(&tc.arguments), file_ops);
                }
            }
        }
        Message::User(_) | Message::System(_) => {}
    }
}

fn add_file_op(tool_name: &str, args: Option<&JsonObject>, file_ops: &mut FileOperations) {
    let Some(path) = args.and_then(|map| map.get("path")).and_then(Value::as_str) else {
        return;
    };
    if path.is_empty() {
        return;
    }
    match tool_name {
        "read" => {
            file_ops.read.insert(path.to_owned());
        }
        "write" => {
            file_ops.written.insert(path.to_owned());
        }
        "edit" => {
            file_ops.edited.insert(path.to_owned());
        }
        _ => {}
    }
}

/// Compute final file lists from file operations.
/// Returns `read_files` (files only read, not modified) and `modified_files`.
pub fn compute_file_lists(file_ops: &FileOperations) -> FileLists {
    let mut modified: IndexSet<String> = IndexSet::new();
    for path in &file_ops.edited {
        modified.insert(path.clone());
    }
    for path in &file_ops.written {
        modified.insert(path.clone());
    }

    let mut read_only: Vec<String> = file_ops
        .read
        .iter()
        .filter(|path| !modified.contains(*path))
        .cloned()
        .collect();
    read_only.sort_by(|a, b| pi_js::str16::cmp(a, b));

    let mut modified_files: Vec<String> = modified.into_iter().collect();
    modified_files.sort_by(|a, b| pi_js::str16::cmp(a, b));

    FileLists {
        read_files: read_only,
        modified_files,
    }
}

/// Format file operations as XML tags for summary.
pub fn format_file_operations(read_files: &[String], modified_files: &[String]) -> String {
    let mut sections: Vec<String> = Vec::new();
    if !read_files.is_empty() {
        sections.push(format!("<read-files>\n{}\n</read-files>", read_files.join("\n")));
    }
    if !modified_files.is_empty() {
        sections.push(format!(
            "<modified-files>\n{}\n</modified-files>",
            modified_files.join("\n")
        ));
    }
    if sections.is_empty() {
        return String::new();
    }
    format!("\n\n{}", sections.join("\n\n"))
}

// ============================================================================
// Message Serialization
// ============================================================================

/// Maximum UTF-16 characters for a tool result in serialized summaries.
const TOOL_RESULT_MAX_CHARS: usize = 2000;

/// Truncate text to a maximum UTF-16 character length for summarization without splitting surrogate pairs.
/// Keeps the beginning and appends a truncation marker.
fn truncate_for_summary(text: &str, max_chars: usize) -> String {
    let len = pi_js::str16::len(text);
    if len <= max_chars {
        return text.to_owned();
    }
    let truncated_chars = len - max_chars;
    let byte_end = pi_js::str16::utf16_to_byte(text, max_chars);
    format!(
        "{}\n\n[... {truncated_chars} more characters truncated]",
        &text[..byte_end]
    )
}

/// Serialize LLM messages to text for summarization.
/// This prevents the model from treating it as a conversation to continue.
/// Call `convert_to_llm()` first to handle custom message types.
///
/// Tool results are truncated to keep the summarization request within
/// reasonable token budgets. Full content is not needed for summarization.
pub fn serialize_conversation(messages: &[Message]) -> String {
    let mut parts: Vec<String> = Vec::new();

    for msg in messages {
        match msg {
            Message::User(user) => {
                let content = content_text(&user.content, Some(""));
                if !content.is_empty() {
                    parts.push(format!("[User]: {content}"));
                }
            }
            Message::Assistant(assistant) => {
                let mut thinking_parts: Vec<&str> = Vec::new();
                let mut tool_calls: Vec<String> = Vec::new();

                for block in &assistant.content {
                    match block {
                        AssistantContentBlock::Thinking(thinking) => {
                            thinking_parts.push(&thinking.thinking);
                        }
                        AssistantContentBlock::ToolCall(tc) => {
                            let args_str = tc
                                .arguments
                                .iter()
                                .map(|(k, v)| format!("{k}={}", pi_js::json::stringify(v)))
                                .collect::<Vec<_>>()
                                .join(", ");
                            tool_calls.push(format!("{}({args_str})", tc.name));
                        }
                        AssistantContentBlock::Text(_) => {}
                    }
                }

                if !thinking_parts.is_empty() {
                    parts.push(format!("[Assistant thinking]: {}", thinking_parts.join("\n")));
                }
                if assistant
                    .content
                    .iter()
                    .any(|block| matches!(block, AssistantContentBlock::Text(_)))
                {
                    parts.push(format!("[Assistant]: {}", content_text(&assistant.content, None)));
                }
                if !tool_calls.is_empty() {
                    parts.push(format!("[Assistant tool calls]: {}", tool_calls.join("; ")));
                }
            }
            Message::ToolResult(tool_result) => {
                let content = content_text(&tool_result.content, Some(""));
                if !content.is_empty() {
                    parts.push(format!(
                        "[Tool result]: {}",
                        truncate_for_summary(&content, TOOL_RESULT_MAX_CHARS)
                    ));
                }
            }
            Message::System(_) => {}
        }
    }

    parts.join("\n\n")
}

// ============================================================================
// Summarization System Prompt
// ============================================================================

pub const SUMMARIZATION_SYSTEM_PROMPT: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.

Do NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";
