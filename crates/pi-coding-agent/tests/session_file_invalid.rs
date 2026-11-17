//! Port of `packages/coding-agent/test/session-file-invalid.test.ts`.
// WIRE(@ca-cli-w3): CLI `--session` spawn test (exit code 1 and stackless stderr) awaits bin `pi` CLI entrypoint.

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
