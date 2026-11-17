//! Port of `packages/coding-agent/test/session-id-readonly.test.ts`.
// WIRE(@ca-cli-w3): `--session-id --help` CLI spawn and `main::create_session_manager` tests await `bin` CLI & `main.rs`.

use pi_agent_core::AgentMessage;
use pi_ai::{
    AssistantContentBlock, AssistantMessage, Message, StopReason, Usage, UserMessage, UserMessageContent, faux_text,
};
use pi_coding_agent::core::session_manager::{NewSessionOptions, SessionManager};
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
