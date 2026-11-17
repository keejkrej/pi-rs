//! Port of packages/coding-agent/src/extensions/mcp/log.ts

use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use serde_json::Value;

const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

fn format_data(data: &Value) -> String {
    match data {
        Value::String(s) => s.clone(),
        other => pi_js::json::stringify(other),
    }
}

/// Format one `notifications/message` from `server` as a log line; continuation lines are indented.
pub fn format_mcp_log_message(server: &str, params: Option<&Value>, now_ms: Option<i64>) -> String {
    let timestamp = match now_ms {
        Some(ms) => pi_js::time::iso_string(ms),
        None => pi_js::time::iso_now(),
    };
    let mut fallback_map = serde_json::Map::new();
    let map = match params {
        Some(Value::Object(m)) => m,
        Some(other) => {
            fallback_map.insert("data".to_string(), other.clone());
            &fallback_map
        }
        None => &fallback_map,
    };
    let level = map.get("level").and_then(Value::as_str).unwrap_or("info");
    let logger = map
        .get("logger")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(|s| format!(" {s}:"))
        .unwrap_or_default();
    let raw_text = map
        .get("data")
        .map(format_data)
        .unwrap_or_else(|| "undefined".to_string());
    let text = raw_text.replace("\r\n", "\n").replace('\n', "\n    ");
    format!("{timestamp} [{server}] {level}{logger} {text}\n")
}

/// Appends server log messages to one file. Write errors are ignored: logging must not break tools.
pub struct McpServerLog {
    pub path: String,
    size: Mutex<Option<u64>>,
}

impl McpServerLog {
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            size: Mutex::new(None),
        }
    }

    pub fn write(&self, server: &str, params: Option<&Value>) {
        let line = format_mcp_log_message(server, params, None);
        let mut size_guard = self.size.lock().unwrap_or_else(|e| e.into_inner());
        let p = Path::new(&self.path);
        if size_guard.is_none() {
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            *size_guard = Some(Self::current_size(p));
        }
        if size_guard.is_some_and(|s| s > MAX_LOG_BYTES) {
            if Self::current_size(p) > MAX_LOG_BYTES {
                let _ = std::fs::rename(p, format!("{}.1", self.path));
            }
            *size_guard = Some(Self::current_size(p));
        }
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(p)
            && file.write_all(line.as_bytes()).is_ok()
            && let Some(sz) = size_guard.as_mut()
        {
            *sz += line.len() as u64;
        }
    }

    fn current_size(path: &Path) -> u64 {
        std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
    }
}
