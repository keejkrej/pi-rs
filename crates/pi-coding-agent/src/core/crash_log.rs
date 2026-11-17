//! Port of packages/coding-agent/src/core/crash-log.ts

use indexmap::IndexSet;
use serde::{Deserialize, Serialize};

use crate::config::{VERSION, get_agent_dir};
use crate::core::source_info::{SourceInfo, SourceOrigin, is_synthetic_path};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrashKind {
    UncaughtException,
    FatalError,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrashRecord {
    pub timestamp: String,
    pub version: String,
    pub kind: CrashKind,
    pub message: String,
    pub stack: Option<String>,
    pub session_file: Option<String>,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notified: Option<bool>,
}

const MAX_CRASH_RECORDS: usize = 5;
const MAX_AGE_MS: i64 = 7 * 24 * 60 * 60 * 1000;

pub fn crash_log_path(agent_dir: Option<&str>) -> String {
    let owned;
    let dir = match agent_dir {
        Some(d) => d,
        None => {
            owned = get_agent_dir();
            &owned
        }
    };
    pi_js::path::join(&[dir, "crashes.json"])
}

pub fn read_crash_log(path: Option<&str>) -> Vec<CrashRecord> {
    let owned;
    let p = match path {
        Some(s) => s,
        None => {
            owned = crash_log_path(None);
            &owned
        }
    };
    let Ok(content) = pi_js::fs::read_to_string(p) else {
        return Vec::new();
    };
    let Ok(records) = pi_js::json::parse_as::<Vec<serde_json::Value>>(&content) else {
        return Vec::new();
    };
    records
        .into_iter()
        .filter_map(|v| serde_json::from_value::<CrashRecord>(v).ok())
        .collect()
}

fn write_crash_log(records: &[CrashRecord], path: &str) -> pi_js::Result<()> {
    let dir = pi_js::path::dirname(path);
    pi_js::fs::mkdir(&dir, true, None)?;
    let formatted = format!("{}\n", pi_js::json::stringify_pretty(&records, "  "));
    pi_js::fs::write_file(path, formatted.as_bytes(), None)
}

/// Extension stack metadata projection (`Pick<Extension, "path" | "resolvedPath" | "sourceInfo">`).
/// WIRE(@codemode-w3): impl `From<&crate::core::extensions::Extension>` once `core/extensions` lands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionStackMetadata {
    pub path: String,
    pub resolved_path: String,
    pub source_info: SourceInfo,
}

fn normalize_stack_path(value: &str) -> String {
    value.replace('\\', "/").trim_end_matches('/').to_owned()
}

fn is_win_drive_path(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'/'
}

fn stack_contains_path(stack: &str, target_path: &str, include_descendants: bool) -> bool {
    let target = normalize_stack_path(target_path);
    if target.is_empty() || is_synthetic_path(&target) {
        return false;
    }
    let case_insensitive = is_win_drive_path(&target);
    let haystack = if case_insensitive {
        stack.to_ascii_lowercase()
    } else {
        stack.to_owned()
    };
    let needle = if case_insensitive {
        target.to_ascii_lowercase()
    } else {
        target
    };
    if include_descendants {
        return haystack.contains(&format!("{needle}/"));
    }

    let mut search_from = 0usize;
    while let Some(rel) = haystack[search_from..].find(&needle) {
        let idx = search_from + rel;
        let next = haystack[idx + needle.len()..].chars().next();
        if next.is_none() || matches!(next, Some(':' | ')')) || next.is_some_and(char::is_whitespace) {
            return true;
        }
        search_from = idx + needle.len();
    }
    false
}

fn is_script_extension(path: &str) -> bool {
    [".ts", ".mts", ".cts", ".js", ".mjs", ".cjs"]
        .iter()
        .any(|ext| path.ends_with(ext))
}

fn is_index_script(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .is_some_and(|name| name.starts_with("index.") && is_script_extension(name))
}

fn has_remote_prefix(source: &str) -> bool {
    source.starts_with("npm:")
        || source.starts_with("git:")
        || source.starts_with("https://")
        || source.starts_with("http://")
        || source.starts_with("ssh://")
}

/// Find loaded extensions with source files in a stack trace.
pub fn find_extension_stack_matches(stack: Option<&str>, extensions: &[ExtensionStackMetadata]) -> Vec<String> {
    let Some(stack) = stack else {
        return Vec::new();
    };
    let normalized_stack = stack
        .split('\n')
        .skip(1)
        .filter(|line| {
            let trimmed = line.trim_start_matches(char::is_whitespace);
            trimmed.len() < line.len() && trimmed.starts_with("at") && trimmed[2..].starts_with(char::is_whitespace)
        })
        .map(|line| {
            pi_js::uri::decode_uri(line)
                .unwrap_or_else(|_| line.to_owned())
                .replace('\\', "/")
        })
        .collect::<Vec<_>>()
        .join("\n");

    let mut matches = Vec::new();
    let mut seen = IndexSet::new();

    for extension in extensions {
        let resolved_path = normalize_stack_path(&extension.resolved_path);
        let single_file_package = extension.source_info.origin == SourceOrigin::Package
            && !has_remote_prefix(&extension.source_info.source)
            && is_script_extension(&extension.source_info.source);
        let package_root = if extension.source_info.origin == SourceOrigin::Package && !single_file_package {
            extension.source_info.base_dir.as_deref()
        } else {
            None
        };
        let slash_idx = resolved_path.rfind('/');
        let directory_entry = is_index_script(&resolved_path);

        let matched = if let Some(pkg_root) = package_root {
            stack_contains_path(&normalized_stack, pkg_root, true)
        } else if directory_entry && let Some(sidx) = slash_idx {
            stack_contains_path(&normalized_stack, &resolved_path[..sidx], true)
        } else {
            stack_contains_path(&normalized_stack, &resolved_path, false)
        };
        if !matched {
            continue;
        }

        let label = if extension.source_info.origin == SourceOrigin::Package && !extension.source_info.source.is_empty()
        {
            extension.source_info.source.clone()
        } else {
            extension.path.clone()
        };
        if seen.insert(label.clone()) {
            matches.push(label);
        }
    }

    matches
}

pub fn record_crash(
    kind: CrashKind,
    message: &str,
    stack: Option<String>,
    session_file: Option<String>,
    cwd: &str,
    path: Option<&str>,
) -> Option<CrashRecord> {
    let owned;
    let p = match path {
        Some(s) => s,
        None => {
            owned = crash_log_path(None);
            &owned
        }
    };
    let record = CrashRecord {
        timestamp: pi_js::time::iso_now(),
        version: (*VERSION).to_owned(),
        kind,
        message: message.to_owned(),
        stack,
        session_file,
        cwd: cwd.to_owned(),
        notified: None,
    };
    let mut existing = read_crash_log(Some(p));
    existing.push(record.clone());
    if existing.len() > MAX_CRASH_RECORDS {
        let start = existing.len() - MAX_CRASH_RECORDS;
        existing = existing[start..].to_vec();
    }
    write_crash_log(&existing, p).ok()?;
    Some(record)
}

pub fn take_unnotified_crash(path: Option<&str>, now: Option<i64>) -> Option<CrashRecord> {
    let owned;
    let p = match path {
        Some(s) => s,
        None => {
            owned = crash_log_path(None);
            &owned
        }
    };
    let now_ms = now.unwrap_or_else(pi_js::time::now_ms);
    let mut records = read_crash_log(Some(p));
    let crash = records
        .iter()
        .rev()
        .find(|record| {
            !record.notified.unwrap_or(false)
                && pi_js::time::parse_date(&record.timestamp).is_some_and(|ts| now_ms - ts <= MAX_AGE_MS)
        })
        .cloned()?;

    for record in &mut records {
        if !record.notified.unwrap_or(false) {
            record.notified = Some(true);
        }
    }
    let _ = write_crash_log(&records, p);
    Some(crash)
}

pub fn clear_crash_log(path: Option<&str>) {
    let owned;
    let p = match path {
        Some(s) => s,
        None => {
            owned = crash_log_path(None);
            &owned
        }
    };
    let _ = pi_js::fs::rm(p, false, true);
}
