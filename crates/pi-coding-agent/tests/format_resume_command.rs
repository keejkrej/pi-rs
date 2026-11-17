//! Port of packages/coding-agent/test/format-resume-command.test.ts

use std::fs;
use std::path::PathBuf;

use pi_coding_agent::config::APP_NAME;
use pi_coding_agent::modes::interactive::components::session_selector::SessionListProgress;
use pi_coding_agent::modes::interactive::components::session_selector_search::SessionInfo;
use pi_coding_agent::modes::interactive::components::tree_selector::{SessionEntry, SessionTreeNode};
use pi_coding_agent::modes::interactive::interactive_mode::{
    InteractiveSessionManager, format_resume_command_with_tty,
};
use pi_js::BoxFuture;
use pi_js::abort::AbortSignal;
use tempfile::TempDir;

struct MockSessionManager {
    persisted: bool,
    session_file: Option<String>,
    session_id: String,
    session_dir: String,
    uses_default_session_dir: bool,
}

impl Default for MockSessionManager {
    fn default() -> Self {
        Self {
            persisted: true,
            session_file: None,
            session_id: "0197f6e4-4cf9-7f44-a2d8-f8f7f49ee9d3".to_string(),
            session_dir: "/tmp/pi-sessions".to_string(),
            uses_default_session_dir: true,
        }
    }
}

impl InteractiveSessionManager for MockSessionManager {
    fn get_cwd(&self) -> String {
        "/tmp".to_string()
    }
    fn get_session_dir(&self) -> String {
        self.session_dir.clone()
    }
    fn get_session_id(&self) -> String {
        self.session_id.clone()
    }
    fn get_session_file(&self) -> Option<String> {
        self.session_file.clone()
    }
    fn get_session_name(&self) -> Option<String> {
        None
    }
    fn is_persisted(&self) -> bool {
        self.persisted
    }
    fn uses_default_session_dir(&self) -> bool {
        self.uses_default_session_dir
    }
    fn get_leaf_id(&self) -> Option<String> {
        None
    }
    fn get_tree(&self) -> Vec<SessionTreeNode> {
        Vec::new()
    }
    fn get_entries(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn get_branch(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn build_context_entries(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn append_label_change(&self, _entry_id: &str, _label: Option<&str>) {}
    fn list_current_sessions(
        &self,
        _on_progress: Option<SessionListProgress>,
        _signal: AbortSignal,
    ) -> BoxFuture<pi_js::Result<Vec<SessionInfo>>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn list_all_sessions(
        &self,
        _on_progress: Option<SessionListProgress>,
        _signal: AbortSignal,
    ) -> BoxFuture<pi_js::Result<Vec<SessionInfo>>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn rename_session(&self, _session_file: &str, _next_name: Option<&str>) -> BoxFuture<pi_js::Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

fn create_temp_file() -> (TempDir, String) {
    let dir = tempfile::Builder::new()
        .prefix("pi-format-resume-command-")
        .tempdir()
        .expect("create tempdir");
    let file: PathBuf = dir.path().join("session.jsonl");
    fs::write(&file, "\n").expect("write temp session file");
    let file_str = file.to_string_lossy().into_owned();
    (dir, file_str)
}

#[test]
fn returns_a_session_resume_command_for_default_session_dirs() {
    let (_dir, session_file) = create_temp_file();
    let sm = MockSessionManager {
        session_file: Some(session_file),
        session_id: "test-session".to_string(),
        ..Default::default()
    };

    assert_eq!(
        format_resume_command_with_tty(&sm, true),
        Some(format!("{} --session test-session", *APP_NAME))
    );
}

#[test]
fn includes_unquoted_safe_session_dirs_for_non_default_session_dirs() {
    let (_dir, session_file) = create_temp_file();
    let sm = MockSessionManager {
        session_file: Some(session_file),
        session_id: "test-session".to_string(),
        session_dir: "/tmp/custom-pi-sessions".to_string(),
        uses_default_session_dir: false,
        ..Default::default()
    };

    assert_eq!(
        format_resume_command_with_tty(&sm, true),
        Some(format!(
            "{} --session-dir /tmp/custom-pi-sessions --session test-session",
            *APP_NAME
        ))
    );
}

#[test]
fn quotes_session_dirs_containing_spaces() {
    let (_dir, session_file) = create_temp_file();
    let sm = MockSessionManager {
        session_file: Some(session_file),
        session_id: "test-session".to_string(),
        session_dir: "/tmp/custom pi sessions".to_string(),
        uses_default_session_dir: false,
        ..Default::default()
    };

    assert_eq!(
        format_resume_command_with_tty(&sm, true),
        Some(format!(
            "{} --session-dir '/tmp/custom pi sessions' --session test-session",
            *APP_NAME
        ))
    );
}

#[test]
fn quotes_session_dirs_containing_single_quotes() {
    let (_dir, session_file) = create_temp_file();
    let sm = MockSessionManager {
        session_file: Some(session_file),
        session_id: "test-session".to_string(),
        session_dir: "/tmp/custom pi's sessions".to_string(),
        uses_default_session_dir: false,
        ..Default::default()
    };

    assert_eq!(
        format_resume_command_with_tty(&sm, true),
        Some(format!(
            "{} --session-dir '/tmp/custom pi'\\''s sessions' --session test-session",
            *APP_NAME
        ))
    );
}

#[test]
fn returns_none_when_stdout_is_not_a_tty() {
    let (_dir, session_file) = create_temp_file();
    let sm = MockSessionManager {
        session_file: Some(session_file),
        ..Default::default()
    };

    assert_eq!(format_resume_command_with_tty(&sm, false), None);
}

#[test]
fn returns_none_for_in_memory_sessions() {
    let (_dir, session_file) = create_temp_file();
    let sm = MockSessionManager {
        persisted: false,
        session_file: Some(session_file),
        ..Default::default()
    };

    assert_eq!(format_resume_command_with_tty(&sm, true), None);
}

#[test]
fn returns_none_when_the_session_file_is_missing() {
    let sm = MockSessionManager {
        session_file: Some("/tmp/pi-missing-session-does-not-exist.jsonl".to_string()),
        ..Default::default()
    };

    assert_eq!(format_resume_command_with_tty(&sm, true), None);
}

#[test]
fn returns_none_when_the_session_file_is_not_set() {
    let sm = MockSessionManager {
        session_file: None,
        ..Default::default()
    };

    assert_eq!(format_resume_command_with_tty(&sm, true), None);
}
