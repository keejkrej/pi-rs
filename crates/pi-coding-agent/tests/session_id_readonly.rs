//! Port of `packages/coding-agent/test/session-id-readonly.test.ts`.

use std::path::Path;
use std::process::Command;

use pi_agent_core::AgentMessage;
use pi_ai::{
    AssistantContentBlock, AssistantMessage, Message, StopReason, Usage, UserMessage, UserMessageContent, faux_text,
};
use pi_coding_agent::cli::args::Args;
use pi_coding_agent::config::ENV_AGENT_DIR;
use pi_coding_agent::core::session_manager::{NewSessionOptions, SessionManager};
use pi_coding_agent::core::settings_manager::SettingsManager;
use pi_coding_agent::main_::create_session_manager;
use tempfile::TempDir;

fn persist_session(sm: &SessionManager, content: &str) {
    sm.append_message(AgentMessage::Message(Message::User(UserMessage {
        content: UserMessageContent::Text(content.to_owned()),
        timestamp: 1,
    })))
    .unwrap();
    sm.append_message(AgentMessage::Message(Message::Assistant(AssistantMessage {
        content: vec![AssistantContentBlock::Text(faux_text("persisted"))],
        api: "anthropic-messages".to_owned(),
        provider: "anthropic".to_owned(),
        model: "test".to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage {
            input: 1,
            output: 1,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 2,
            cost: Default::default(),
        },
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 2,
    })))
    .unwrap();
}

#[test]
fn does_not_persist_custom_id_until_assistant_message_and_finds_reopened_renamed_sessions() {
    let tmp = TempDir::new().unwrap();
    let project_dir = tmp.path().join("project");
    let session_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::create_dir_all(&session_dir).unwrap();

    let project_str = project_dir.to_string_lossy().into_owned();
    let session_dir_str = session_dir.to_string_lossy().into_owned();

    // Unpersisted session with explicit ID writes nothing to disk until an assistant turn
    let read_only = SessionManager::in_memory(Some(&project_str), None, None).unwrap();
    assert!(read_only.get_session_file().is_none());

    let created = SessionManager::create(
        &project_str,
        Some(&session_dir_str),
        Some(NewSessionOptions {
            id: Some("persisted-id".to_owned()),
            parent_session: None,
        }),
    )
    .unwrap();
    assert_eq!(created.get_session_id(), "persisted-id");
    assert!(!std::path::Path::new(created.get_session_file().as_deref().unwrap()).exists());
    persist_session(&created, "persist me");
    assert!(std::path::Path::new(created.get_session_file().as_deref().unwrap()).exists());

    let found_path = SessionManager::find_by_id(&project_str, "persisted-id", Some(&session_dir_str)).unwrap();
    assert_eq!(Some(found_path.as_str()), created.get_session_file().as_deref());

    // Exact lookup resolves renamed session file cleanly
    let renamed_path = session_dir.join("imported-session.jsonl");
    std::fs::rename(&found_path, &renamed_path).unwrap();
    let found_renamed = SessionManager::find_by_id(&project_str, "persisted-id", Some(&session_dir_str)).unwrap();
    assert_eq!(found_renamed, renamed_path.to_string_lossy());
}

#[test]
fn filters_exact_ids_by_cwd_in_custom_session_directory() {
    let tmp = TempDir::new().unwrap();
    let project_a = tmp.path().join("project-a");
    let project_b = tmp.path().join("project-b");
    let session_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&project_a).unwrap();
    std::fs::create_dir_all(&project_b).unwrap();
    std::fs::create_dir_all(&session_dir).unwrap();

    let a_str = project_a.to_string_lossy().into_owned();
    let b_str = project_b.to_string_lossy().into_owned();
    let dir_str = session_dir.to_string_lossy().into_owned();

    let foreign = SessionManager::create(
        &b_str,
        Some(&dir_str),
        Some(NewSessionOptions {
            id: Some("foreign-id".to_owned()),
            parent_session: None,
        }),
    )
    .unwrap();
    persist_session(&foreign, "foreign session");

    assert!(SessionManager::find_by_id(&a_str, "foreign-id", Some(&dir_str)).is_none());
    assert_eq!(
        SessionManager::find_by_id(&b_str, "foreign-id", Some(&dir_str)),
        foreign.get_session_file()
    );
}

fn has_session_with_id(root: &Path, session_id: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && has_session_with_id(&path, session_id) {
            return true;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        if let Ok(raw) = std::fs::read_to_string(&path)
            && let Some(first_line) = raw.lines().next()
            && let Ok(val) = pi_js::json::parse(first_line)
            && val.get("type").and_then(|v| v.as_str()) == Some("session")
            && val.get("id").and_then(|v| v.as_str()) == Some(session_id)
        {
            return true;
        }
    }
    false
}

#[test]
fn cli_does_not_persist_a_custom_id_for_metadata_commands() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap_or_else(|_| tmp.path().to_path_buf());
    let agent_dir = root.join("agent");
    let project_dir = root.join("project");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::create_dir_all(&project_dir).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_pi"))
        .args(["--session-id", "read-only-help", "--help"])
        .current_dir(&project_dir)
        .env(&*ENV_AGENT_DIR, &agent_dir)
        .env("PI_OFFLINE", "1")
        .output()
        .expect("spawn pi");

    assert_eq!(output.status.code(), Some(0));
    assert!(!has_session_with_id(&agent_dir.join("sessions"), "read-only-help"));
}

#[tokio::test]
async fn create_session_manager_handles_readonly_persisted_renamed_and_conflicting_fork_ids() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap_or_else(|_| tmp.path().to_path_buf());
    let project_dir = root.join("project");
    let session_dir = root.join("sessions");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::create_dir_all(&session_dir).unwrap();
    let project_str = project_dir.to_string_lossy().into_owned();
    let session_dir_str = session_dir.to_string_lossy().into_owned();
    let settings = SettingsManager::in_memory(None, None);

    let read_only = create_session_manager(
        &Args {
            session_id: Some("read-only".to_owned()),
            help: Some(true),
            ..Default::default()
        },
        &project_str,
        Some(&session_dir_str),
        &settings,
    )
    .await
    .unwrap();
    assert_eq!(read_only.get_session_id(), "read-only");
    assert!(read_only.get_session_file().is_none());

    let created = create_session_manager(
        &Args {
            session_id: Some("persisted-id".to_owned()),
            ..Default::default()
        },
        &project_str,
        Some(&session_dir_str),
        &settings,
    )
    .await
    .unwrap();
    persist_session(&created, "persist me");

    let reopened = create_session_manager(
        &Args {
            session_id: Some("persisted-id".to_owned()),
            ..Default::default()
        },
        &project_str,
        Some(&session_dir_str),
        &settings,
    )
    .await
    .unwrap();
    assert_eq!(reopened.get_session_file(), created.get_session_file());

    let renamed_path = session_dir.join("imported-session.jsonl");
    std::fs::rename(reopened.get_session_file().unwrap(), &renamed_path).unwrap();
    let reopened_renamed = create_session_manager(
        &Args {
            session_id: Some("persisted-id".to_owned()),
            ..Default::default()
        },
        &project_str,
        Some(&session_dir_str),
        &settings,
    )
    .await
    .unwrap();
    assert_eq!(
        reopened_renamed.get_session_file().as_deref(),
        Some(renamed_path.to_string_lossy().as_ref())
    );

    let fork_err = create_session_manager(
        &Args {
            fork: Some("persisted-id".to_owned()),
            session_id: Some("persisted-id".to_owned()),
            ..Default::default()
        },
        &project_str,
        Some(&session_dir_str),
        &settings,
    )
    .await
    .err()
    .expect("existing fork target id should fail");
    assert!(matches!(fork_err, pi_js::Error::Exit(1)));
}
