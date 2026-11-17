//! Port of packages/coding-agent/src/core/session-export.ts

use pi_js::Result;
use serde_json::{Map, Value};

use crate::core::session_manager::{CURRENT_SESSION_VERSION, SessionHeader, SessionManager};
use crate::utils::paths::{PathInputOptions, resolve_path};

pub type TrailingEntriesFn<'a> = &'a dyn Fn(Option<&str>, &str) -> Vec<Value>;

/// Serialize the current branch and optional export-only entries as JSONL.
pub fn serialize_session_branch(
    session_manager: &SessionManager,
    create_trailing_entries: Option<TrailingEntriesFn<'_>>,
) -> Result<String> {
    let timestamp = pi_js::time::iso_now();
    let header = SessionHeader {
        version: Some(CURRENT_SESSION_VERSION),
        id: session_manager.get_session_id(),
        timestamp: timestamp.clone(),
        cwd: session_manager.get_cwd(),
        parent_session: None,
    };

    let mut lines: Vec<String> = Vec::new();
    let header_val = serde_json::to_value(&header).map_err(|e| pi_js::Error::msg(e.to_string()))?;
    let header_map = header_val.as_object().cloned().unwrap_or_default();
    let mut ordered_header = Map::new();
    ordered_header.insert("type".to_owned(), Value::String("session".to_owned()));
    for (k, v) in header_map {
        ordered_header.insert(k, v);
    }
    lines.push(pi_js::json::try_stringify(&ordered_header)?);

    let mut parent_id: Option<String> = None;
    for entry in session_manager.get_branch(None) {
        let entry_id = entry.id().to_owned();
        let entry_val = serde_json::to_value(&entry).map_err(|e| pi_js::Error::msg(e.to_string()))?;
        let mut entry_map = entry_val.as_object().cloned().unwrap_or_default();
        entry_map.insert(
            "parentId".to_owned(),
            match parent_id {
                Some(ref p) => Value::String(p.clone()),
                None => Value::Null,
            },
        );
        lines.push(pi_js::json::try_stringify(&entry_map)?);
        parent_id = Some(entry_id);
    }

    if let Some(cb) = create_trailing_entries {
        for trailing in cb(parent_id.as_deref(), &timestamp) {
            lines.push(pi_js::json::try_stringify(&trailing)?);
        }
    }

    let mut out = lines.join("\n");
    out.push('\n');
    Ok(out)
}

/// Write the current session branch and optional export-only entries as JSONL.
pub fn export_session_to_jsonl(
    session_manager: &SessionManager,
    output_path: Option<&str>,
    create_trailing_entries: Option<TrailingEntriesFn<'_>>,
) -> Result<String> {
    let default_name;
    let raw_output = match output_path {
        Some(p) => p,
        None => {
            let sanitized = pi_js::time::iso_now().replace([':', '.'], "-");
            default_name = format!("session-{sanitized}.jsonl");
            &default_name
        }
    };
    let cwd = pi_js::env::cwd();
    let file_path = resolve_path(raw_output, Some(&cwd), PathInputOptions::default());
    let dir = pi_js::path::dirname(&file_path);
    if !pi_js::fs::exists(&dir) {
        pi_js::fs::mkdir(&dir, true, None)?;
    }
    let content = serialize_session_branch(session_manager, create_trailing_entries)?;
    pi_js::fs::write_file(&file_path, content, None)?;
    Ok(file_path)
}
