//! Port of `packages/coding-agent/test/session-file-invalid.test.ts`.

use std::process::Command;

use pi_coding_agent::config::ENV_AGENT_DIR;
use pi_coding_agent::core::session_manager::SessionManager;
use tempfile::TempDir;

#[test]
fn opening_invalid_session_file_fails_cleanly_and_preserves_original_content() {
    let tmp = TempDir::new().unwrap();
    let session_file = tmp.path().join("not-a-session.log");
    let original = "{\"type\":\"event\",\"data\":\"not a session\"}\n";
    std::fs::write(&session_file, original).unwrap();

    let err = match SessionManager::open(session_file.to_str().unwrap(), None, None) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("expected invalid session file error"),
    };
    assert!(!err.is_empty());
    assert_eq!(std::fs::read_to_string(&session_file).unwrap(), original);
}

#[test]
fn cli_session_prints_friendly_error_and_preserves_non_session_file_content() {
    let tmp = TempDir::new().unwrap();
    let agent_dir = tmp.path().join("agent");
    let project_dir = tmp.path().join("project");
    let session_file = tmp.path().join("not-a-session.log");
    let original = "{\"type\":\"event\",\"data\":\"not a session\"}\n";
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(&session_file, original).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_pi"))
        .args(["--session", session_file.to_str().unwrap(), "-p", "hi"])
        .current_dir(&project_dir)
        .env(&*ENV_AGENT_DIR, &agent_dir)
        .env("PI_OFFLINE", "1")
        .output()
        .expect("spawn pi");

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    let expected_path = session_file.to_string_lossy();
    assert!(
        stderr.contains(&format!(
            "Error: Session file is not a valid pi session: {expected_path}"
        )) || stderr.contains("is not a valid pi session"),
        "unexpected stderr: {stderr}"
    );
    assert!(!stderr.contains("SessionManager.open"), "unexpected stderr: {stderr}");
    assert_eq!(std::fs::read_to_string(&session_file).unwrap(), original);
}
