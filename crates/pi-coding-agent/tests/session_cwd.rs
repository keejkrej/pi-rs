//! Port of `packages/coding-agent/test/session-cwd.test.ts`.
// WIRE(@ca-cli-w3): `create_agent_session_runtime` rejection before factory invocation awaits `core::agent_session_runtime`.

use std::path::Path;

use pi_coding_agent::core::session_cwd::{
    MissingSessionCwdError, SessionCwdIssue, SessionCwdSource, assert_session_cwd_exists,
    format_missing_session_cwd_error, format_missing_session_cwd_prompt, get_missing_session_cwd_issue,
};
use pi_coding_agent::core::session_manager::SessionManager;
use serde_json::json;
use tempfile::TempDir;

struct SmCwd<'a>(&'a SessionManager);

impl SessionCwdSource for SmCwd<'_> {
    fn get_cwd(&self) -> String {
        self.0.get_cwd()
    }

    fn get_session_file(&self) -> Option<String> {
        self.0.get_session_file()
    }
}

fn write_session_file(path: &Path, cwd: &Path) {
    let header = pi_js::json::stringify(&json!({
        "type": "session",
        "version": 3,
        "id": "session-id",
        "timestamp": "2026-01-01T00:00:00.000Z",
        "cwd": cwd.to_string_lossy(),
    }));
    std::fs::write(path, format!("{header}\n")).unwrap();
}

#[test]
fn detects_missing_session_cwd_from_persisted_sessions() {
    let tmp = TempDir::new().unwrap();
    let fallback_dir = tmp.path().join("fallback");
    std::fs::create_dir_all(&fallback_dir).unwrap();
    let missing_cwd = fallback_dir.join("does-not-exist");
    let session_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&session_dir).unwrap();
    let session_file = session_dir.join("session.jsonl");
    write_session_file(&session_file, &missing_cwd);

    let fallback_str = fallback_dir.to_string_lossy().into_owned();
    let sm = SessionManager::open(session_file.to_str().unwrap(), None, None).unwrap();
    let issue = get_missing_session_cwd_issue(&SmCwd(&sm), &fallback_str).unwrap();
    assert_eq!(
        issue,
        SessionCwdIssue {
            session_file: sm.get_session_file(),
            session_cwd: missing_cwd.to_string_lossy().into_owned(),
            fallback_cwd: fallback_str,
        }
    );

    let err_msg = format_missing_session_cwd_error(&issue);
    assert!(err_msg.contains("does-not-exist"));
    let prompt_msg = format_missing_session_cwd_prompt(&issue);
    assert!(prompt_msg.contains(&issue.fallback_cwd));
}

#[test]
fn supports_overriding_effective_cwd_when_opening_session() {
    let tmp = TempDir::new().unwrap();
    let fallback_dir = tmp.path().join("fallback");
    std::fs::create_dir_all(&fallback_dir).unwrap();
    let missing_cwd = fallback_dir.join("does-not-exist");
    let session_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&session_dir).unwrap();
    let session_file = session_dir.join("session.jsonl");
    write_session_file(&session_file, &missing_cwd);

    let fallback_str = fallback_dir.to_string_lossy().into_owned();
    let sm = SessionManager::open(session_file.to_str().unwrap(), None, Some(&fallback_str)).unwrap();
    assert_eq!(sm.get_cwd(), fallback_str);
    assert!(get_missing_session_cwd_issue(&SmCwd(&sm), &fallback_str).is_none());
}

#[test]
fn assert_session_cwd_exists_returns_controlled_error_when_stored_cwd_is_missing() {
    let tmp = TempDir::new().unwrap();
    let fallback_dir = tmp.path().join("fallback");
    std::fs::create_dir_all(&fallback_dir).unwrap();
    let missing_cwd = fallback_dir.join("does-not-exist");
    let session_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&session_dir).unwrap();
    let session_file = session_dir.join("session.jsonl");
    write_session_file(&session_file, &missing_cwd);

    let fallback_str = fallback_dir.to_string_lossy().into_owned();
    let sm = SessionManager::open(session_file.to_str().unwrap(), None, None).unwrap();
    let err = assert_session_cwd_exists(&SmCwd(&sm), &fallback_str).unwrap_err();
    let missing_err = MissingSessionCwdError::new(get_missing_session_cwd_issue(&SmCwd(&sm), &fallback_str).unwrap());
    assert!(err.to_string().contains(&missing_err.to_string()));
}
