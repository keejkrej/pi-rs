use anyhow::{Context, Result};
use async_trait::async_trait;
use glob::Pattern;
use regex::Regex;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::Mutex;
use walkdir::WalkDir;

use crate::messages::{TextContent, UserContentBlock};
use crate::models::ToolSpec;

#[derive(Debug, Clone)]
pub struct ToolExecutionResult {
    pub content: Vec<UserContentBlock>,
    pub is_error: bool,
    pub details: Option<Value>,
}

impl ToolExecutionResult {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![UserContentBlock::Text(TextContent::new(text))],
            is_error: false,
            details: None,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            content: vec![UserContentBlock::Text(TextContent::new(text))],
            is_error: true,
            details: None,
        }
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    async fn execute(&self, cwd: &Path, arguments: Value) -> Result<ToolExecutionResult>;
}

#[derive(Clone)]
pub struct BuiltInToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
    _mutation_lock: Arc<Mutex<()>>,
}

impl BuiltInToolRegistry {
    pub fn new() -> Self {
        let mutation_lock = Arc::new(Mutex::new(()));
        let mut tools: BTreeMap<String, Arc<dyn Tool>> = BTreeMap::new();
        tools.insert("read".into(), Arc::new(ReadTool));
        tools.insert("write".into(), Arc::new(WriteTool(mutation_lock.clone())));
        tools.insert("edit".into(), Arc::new(EditTool(mutation_lock.clone())));
        tools.insert("bash".into(), Arc::new(BashTool));
        tools.insert("grep".into(), Arc::new(GrepTool));
        tools.insert("find".into(), Arc::new(FindTool));
        tools.insert("ls".into(), Arc::new(LsTool));
        Self {
            tools,
            _mutation_lock: mutation_lock,
        }
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|tool| tool.spec()).collect()
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }
}

struct ReadTool;
struct BashTool;
struct GrepTool;
struct FindTool;
struct LsTool;
struct WriteTool(Arc<Mutex<()>>);
struct EditTool(Arc<Mutex<()>>);

#[async_trait]
impl Tool for ReadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read".into(),
            description: "Read a UTF-8 text file from disk".into(),
            input_schema: json!({
                "type": "object",
                "required": ["path"],
                "properties": {
                    "path": {"type": "string"},
                    "start_line": {"type": "integer"},
                    "limit": {"type": "integer"}
                }
            }),
            requires_permission: false,
        }
    }

    async fn execute(&self, cwd: &Path, arguments: Value) -> Result<ToolExecutionResult> {
        let path = resolve_path(
            cwd,
            arguments
                .get("path")
                .and_then(Value::as_str)
                .context("missing path")?,
        );
        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let start = arguments
            .get("start_line")
            .and_then(Value::as_u64)
            .unwrap_or(1) as usize;
        let limit = arguments
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(200) as usize;
        let slice = content
            .lines()
            .enumerate()
            .skip(start.saturating_sub(1))
            .take(limit)
            .map(|(idx, line)| format!("{:>5} {}", idx + 1, line))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolExecutionResult::text(slice))
    }
}

#[async_trait]
impl Tool for WriteTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write".into(),
            description: "Write a text file, creating parent directories when needed".into(),
            input_schema: json!({
                "type": "object",
                "required": ["path", "content"],
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                }
            }),
            requires_permission: true,
        }
    }

    async fn execute(&self, cwd: &Path, arguments: Value) -> Result<ToolExecutionResult> {
        let _guard = self.0.lock().await;
        let path = resolve_path(
            cwd,
            arguments
                .get("path")
                .and_then(Value::as_str)
                .context("missing path")?,
        );
        let content = arguments
            .get("content")
            .and_then(Value::as_str)
            .context("missing content")?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, content)?;
        Ok(ToolExecutionResult {
            content: vec![UserContentBlock::Text(TextContent::new(format!(
                "Wrote {} bytes to {}",
                content.len(),
                path.display()
            )))],
            is_error: false,
            details: Some(json!({"path": path})),
        })
    }
}

#[async_trait]
impl Tool for EditTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit".into(),
            description: "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["path", "edits"],
                "properties": {
                    "path": {"type": "string", "description": "Path to the file to edit (relative or absolute)"},
                    "edits": {
                        "type": "array",
                        "description": "One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits. If two changes touch the same block or nearby lines, merge them into one edit instead.",
                        "items": {
                            "type": "object",
                            "required": ["oldText", "newText"],
                            "properties": {
                                "oldText": {
                                    "type": "string",
                                    "description": "Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call."
                                },
                                "newText": {
                                    "type": "string",
                                    "description": "Replacement text for this targeted edit."
                                }
                            }
                        }
                    }
                }
            }),
            requires_permission: true,
        }
    }

    async fn execute(&self, cwd: &Path, arguments: Value) -> Result<ToolExecutionResult> {
        let _guard = self.0.lock().await;
        let path = resolve_path(
            cwd,
            arguments
                .get("path")
                .and_then(Value::as_str)
                .context("missing path")?,
        );
        let edits = arguments
            .get("edits")
            .and_then(Value::as_array)
            .context("missing edits")?;

        let mut content = fs::read_to_string(&path)?;
        
        for edit in edits {
            let old_text = edit
                .get("oldText")
                .and_then(Value::as_str)
                .context("missing oldText in edit")?;
            let new_text = edit
                .get("newText")
                .and_then(Value::as_str)
                .context("missing newText in edit")?;

            if let Some(idx) = content.find(old_text) {
                let mut updated = String::with_capacity(content.len() + new_text.len());
                updated.push_str(&content[..idx]);
                updated.push_str(new_text);
                updated.push_str(&content[idx + old_text.len()..]);
                content = updated;
            } else {
                return Ok(ToolExecutionResult::error(format!(
                    "pattern not found in {}",
                    path.display()
                )));
            }
        }
        
        fs::write(&path, content)?;
        Ok(ToolExecutionResult::text(format!(
            "Edited {}",
            path.display()
        )))
    }
}

#[async_trait]
impl Tool for BashTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "bash".into(),
            description: "Run a shell command in the working directory".into(),
            input_schema: json!({
                "type": "object",
                "required": ["command"],
                "properties": {
                    "command": {"type": "string"}
                }
            }),
            requires_permission: true,
        }
    }

    async fn execute(&self, cwd: &Path, arguments: Value) -> Result<ToolExecutionResult> {
        let command = arguments
            .get("command")
            .and_then(Value::as_str)
            .context("missing command")?;
        let output = Command::new("zsh")
            .arg("-lc")
            .arg(command)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .output()
            .await?;
        let mut text = String::new();
        text.push_str(&String::from_utf8_lossy(&output.stdout));
        if !output.stderr.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&String::from_utf8_lossy(&output.stderr));
        }
        if text.is_empty() {
            text = format!("command exited with {}", output.status);
        }
        Ok(ToolExecutionResult {
            content: vec![UserContentBlock::Text(TextContent::new(text))],
            is_error: !output.status.success(),
            details: Some(json!({"exitCode": output.status.code()})),
        })
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "grep".into(),
            description: "Search text recursively with a regex pattern".into(),
            input_schema: json!({
                "type": "object",
                "required": ["pattern"],
                "properties": {
                    "pattern": {"type": "string"},
                    "path": {"type": "string"}
                }
            }),
            requires_permission: false,
        }
    }

    async fn execute(&self, cwd: &Path, arguments: Value) -> Result<ToolExecutionResult> {
        let pattern = Regex::new(
            arguments
                .get("pattern")
                .and_then(Value::as_str)
                .context("missing pattern")?,
        )?;
        let path = arguments
            .get("path")
            .and_then(Value::as_str)
            .map(|value| resolve_path(cwd, value))
            .unwrap_or_else(|| cwd.to_path_buf());
        let mut matches = Vec::new();
        for entry in WalkDir::new(path).into_iter().filter_map(Result::ok) {
            if !entry.file_type().is_file() {
                continue;
            }
            if let Ok(content) = fs::read_to_string(entry.path()) {
                for (idx, line) in content.lines().enumerate() {
                    if pattern.is_match(line) {
                        matches.push(format!(
                            "{}:{}:{}",
                            entry.path().display(),
                            idx + 1,
                            line.trim()
                        ));
                    }
                }
            }
            if matches.len() >= 200 {
                break;
            }
        }
        Ok(ToolExecutionResult::text(matches.join("\n")))
    }
}

#[async_trait]
impl Tool for FindTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "find".into(),
            description: "Find files under the current working directory".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string"},
                    "path": {"type": "string"}
                }
            }),
            requires_permission: false,
        }
    }

    async fn execute(&self, cwd: &Path, arguments: Value) -> Result<ToolExecutionResult> {
        let pattern = arguments
            .get("pattern")
            .and_then(Value::as_str)
            .unwrap_or("*");
        let glob = Pattern::new(pattern)?;
        let path = arguments
            .get("path")
            .and_then(Value::as_str)
            .map(|value| resolve_path(cwd, value))
            .unwrap_or_else(|| cwd.to_path_buf());
        let mut results = Vec::new();
        for entry in WalkDir::new(path).into_iter().filter_map(Result::ok) {
            if entry.file_type().is_file() && glob.matches_path(entry.path()) {
                results.push(entry.path().display().to_string());
            }
            if results.len() >= 200 {
                break;
            }
        }
        Ok(ToolExecutionResult::text(results.join("\n")))
    }
}

#[async_trait]
impl Tool for LsTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ls".into(),
            description: "List directory contents. Returns entries sorted alphabetically, with '/' suffix for directories. Includes dotfiles. Output is truncated to 500 entries or 50KB (whichever is hit first).".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Directory to list (default: current directory)"},
                    "limit": {"type": "number", "description": "Maximum number of entries to return (default: 500)"}
                }
            }),
            requires_permission: false,
        }
    }

    async fn execute(&self, cwd: &Path, arguments: Value) -> Result<ToolExecutionResult> {
        let path = arguments
            .get("path")
            .and_then(Value::as_str)
            .map(|value| resolve_path(cwd, value))
            .unwrap_or_else(|| cwd.to_path_buf());
        let limit = arguments
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(500) as usize;

        let mut rows = Vec::new();
        let mut entries: Vec<_> = fs::read_dir(&path)?.filter_map(Result::ok).collect();
        entries.sort_by_cached_key(|e| e.file_name().to_string_lossy().to_lowercase());

        for entry in entries.into_iter().take(limit) {
            let metadata = entry.metadata()?;
            let suffix = if metadata.is_dir() { "/" } else { "" };
            rows.push(format!("{}{}", entry.file_name().to_string_lossy(), suffix));
        }

        if rows.is_empty() {
            return Ok(ToolExecutionResult::text("(empty directory)"));
        }

        Ok(ToolExecutionResult::text(rows.join("\n")))
    }
}

fn resolve_path(cwd: &Path, path: &str) -> PathBuf {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        cwd.join(candidate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn edit_tool_replaces_text() {
        let temp = std::env::temp_dir().join(format!("pi-rs-tools-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp).unwrap();
        let path = temp.join("file.txt");
        std::fs::write(&path, "alpha\nbeta\n").unwrap();

        let tool = EditTool(Arc::new(Mutex::new(())));
        tool.execute(
            &temp,
            json!({
                "path": path.display().to_string(),
                "edits": [
                    {
                        "oldText": "beta",
                        "newText": "gamma"
                    }
                ]
            }),
        )
        .await
        .unwrap();

        let updated = std::fs::read_to_string(path).unwrap();
        assert!(updated.contains("gamma"));
    }
}
