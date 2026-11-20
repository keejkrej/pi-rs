//! Port of `packages/coding-agent/test/session-info-modified-timestamp.test.ts`.

use std::time::{Duration, UNIX_EPOCH};

use pi_agent_core::AgentMessage;
use pi_ai::{AssistantContentBlock, AssistantMessage, Message, StopReason, Usage, faux_text};
use pi_coding_agent::core::session_manager::{FileEntry, SessionHeader, SessionManager};
use tempfile::TempDir;

fn assistant_msg(text: &str, timestamp: i64) -> AgentMessage {
    AgentMessage::Message(Message::Assistant(AssistantMessage {
        content: vec![AssistantContentBlock::Text(faux_text(text))],
        api: "openai-completions".to_owned(),
        provider: "openai".to_owned(),
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
        timestamp,
        duration_ms: None,
    }))
}

#[tokio::test]
async fn uses_last_user_or_assistant_message_timestamp_instead_of_file_mtime() {
    let tmp = TempDir::new().unwrap();
    let file_path = tmp.path().join("modified.jsonl");
    let path_str = file_path.to_str().unwrap().to_owned();

    let header = FileEntry::Session(SessionHeader {
        version: Some(3),
        id: "test-session".to_owned(),
        timestamp: pi_js::time::iso_string(0),
        cwd: "/tmp".to_owned(),
        parent_session: None,
    });
    std::fs::write(&file_path, format!("{}\n", pi_js::json::stringify(&header))).unwrap();

    let mgr = SessionManager::open(&path_str, None, None).unwrap();
    mgr.append_message(assistant_msg("hi", 1_700_000_000_000)).unwrap();

    let before_mtime_ms = std::fs::metadata(&file_path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;

    tokio::time::sleep(Duration::from_millis(15)).await;

    let mgr2 = SessionManager::open(&path_str, None, None).unwrap();
    let msg_time = 1_700_000_042_000;
    mgr2.append_message(assistant_msg("later", msg_time)).unwrap();

    let dir_str = tmp.path().to_str().unwrap();
    let sessions = SessionManager::list("/tmp", Some(dir_str), None, None).await.unwrap();
    let info = sessions.iter().find(|s| s.path == path_str).unwrap();
    assert_eq!(info.modified, msg_time);
    assert_ne!(info.modified, before_mtime_ms);
}
