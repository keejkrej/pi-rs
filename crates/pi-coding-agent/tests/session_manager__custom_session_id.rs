//! Port of `packages/coding-agent/test/session-manager/custom-session-id.test.ts`.

use std::path::Path;

use pi_agent_core::AgentMessage;
use pi_ai::{Message, UserMessage, UserMessageContent};
use pi_coding_agent::core::session_manager::{NewSessionOptions, SessionManager};
use regex::Regex;
use serde_json::json;
use tempfile::TempDir;

fn uuid_v7_re() -> Regex {
    Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$").unwrap()
}

#[test]
fn uses_provided_id_and_allows_interior_punctuation() {
    let session = SessionManager::in_memory(None, None, None).unwrap();
    session
        .new_session(Some(NewSessionOptions {
            id: Some("my-custom-id".to_owned()),
            parent_session: None,
        }))
        .unwrap();
    assert_eq!(session.get_session_id(), "my-custom-id");

    let mem = SessionManager::in_memory(
        None,
        Some(NewSessionOptions {
            id: Some("memory-session-id".to_owned()),
            parent_session: None,
        }),
        None,
    )
    .unwrap();
    assert_eq!(mem.get_session_id(), "memory-session-id");
    assert_eq!(mem.get_header().unwrap().id, "memory-session-id");
    assert!(mem.get_session_file().is_none());

    let punctuated = SessionManager::in_memory(None, None, None).unwrap();
    punctuated
        .new_session(Some(NewSessionOptions {
            id: Some("abc-123_def.456".to_owned()),
            parent_session: None,
        }))
        .unwrap();
    assert_eq!(punctuated.get_session_id(), "abc-123_def.456");
    assert_eq!(punctuated.get_header().unwrap().id, "abc-123_def.456");
}

#[test]
fn rejects_invalid_custom_session_ids() {
    let invalid_ids = [
        "", "-abc", "abc-", "_abc", "abc_", ".abc", "abc.", "abc/def", "abc\\def", "abc def",
    ];
    for id in invalid_ids {
        let session = SessionManager::in_memory(None, None, None).unwrap();
        let err = session
            .new_session(Some(NewSessionOptions {
                id: Some(id.to_owned()),
                parent_session: None,
            }))
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("Session id must be non-empty, contain only alphanumeric characters")
        );
    }
}

#[test]
fn generates_uuidv7_when_omitted_and_supports_persisted_created_and_forked_ids() {
    let re = uuid_v7_re();
    let default_sm = SessionManager::in_memory(None, None, None).unwrap();
    assert!(re.is_match(&default_sm.get_session_id()));
    assert_eq!(default_sm.get_header().unwrap().id, default_sm.get_session_id());

    default_sm.new_session(None).unwrap();
    assert!(re.is_match(&default_sm.get_session_id()));

    default_sm
        .new_session(Some(NewSessionOptions {
            id: None,
            parent_session: Some("parent.jsonl".to_owned()),
        }))
        .unwrap();
    assert!(re.is_match(&default_sm.get_session_id()));

    let tmp = TempDir::new().unwrap();
    let dir_str = tmp.path().to_str().unwrap();
    let created = SessionManager::create(
        dir_str,
        Some(dir_str),
        Some(NewSessionOptions {
            id: Some("created-session-id".to_owned()),
            parent_session: None,
        }),
    )
    .unwrap();
    assert_eq!(created.get_session_id(), "created-session-id");
    assert_eq!(created.get_header().unwrap().id, "created-session-id");
    let sf = created.get_session_file().unwrap();
    let base = Path::new(&sf).file_name().unwrap().to_str().unwrap();
    let created_re = Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}-\d{2}-\d{2}-\d{3}Z_created-session-id\.jsonl$").unwrap();
    assert!(created_re.is_match(base));
    assert!(!Path::new(&sf).exists());

    let branch_sm = SessionManager::in_memory(None, None, None).unwrap();
    let first_id = branch_sm
        .append_message(AgentMessage::Message(Message::User(UserMessage {
            content: UserMessageContent::Text("hello".to_owned()),
            timestamp: 1,
        })))
        .unwrap();
    branch_sm.create_branched_session(&first_id).unwrap();
    assert!(re.is_match(&branch_sm.get_session_id()));
    assert_eq!(branch_sm.get_header().unwrap().id, branch_sm.get_session_id());

    let source_path = tmp.path().join("source.jsonl");
    let hdr = pi_js::json::stringify(&json!({
        "type": "session",
        "version": 3,
        "id": "legacy-session-id",
        "timestamp": "2026-01-01T00:00:00.000Z",
        "cwd": dir_str,
    }));
    std::fs::write(&source_path, format!("{hdr}\n")).unwrap();
    let src_str = source_path.to_str().unwrap();

    let forked_default = SessionManager::fork_from(src_str, dir_str, Some(dir_str), None).unwrap();
    let h1 = forked_default.get_header().unwrap();
    assert!(re.is_match(&h1.id));
    assert_eq!(h1.parent_session.as_deref(), Some(src_str));

    let forked_custom = SessionManager::fork_from(
        src_str,
        dir_str,
        Some(dir_str),
        Some(NewSessionOptions {
            id: Some("forked-session-id".to_owned()),
            parent_session: None,
        }),
    )
    .unwrap();
    let h2 = forked_custom.get_header().unwrap();
    assert_eq!(h2.id, "forked-session-id");
    assert_eq!(h2.parent_session.as_deref(), Some(src_str));
    let forked_file = forked_custom.get_session_file().unwrap();
    let forked_base = Path::new(&forked_file).file_name().unwrap().to_str().unwrap();
    let forked_re = Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}-\d{2}-\d{2}-\d{3}Z_forked-session-id\.jsonl$").unwrap();
    assert!(forked_re.is_match(forked_base));
}
