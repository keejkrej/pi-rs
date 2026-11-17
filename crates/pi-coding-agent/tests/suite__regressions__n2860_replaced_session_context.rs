//! Port of `packages/coding-agent/test/suite/regressions/2860-replaced-session-context.test.ts`.
// WIRE(@ca-cli-w3): full `create_agent_session_runtime` / `withSession` rebind tests await `core::agent_session_runtime`.
// WIRE(@codemode-w3): extension stale `pi`/`ctx` invalidation checks await `core::extensions::runner`.

use pi_agent_core::AgentMessage;
use pi_ai::{
    AssistantContentBlock, AssistantMessage, Message, StopReason, Usage, UserMessage, UserMessageContent, faux_text,
};
use pi_coding_agent::core::session_manager::{NewSessionOptions, SessionManager};
use tempfile::TempDir;

fn user_msg(text: &str) -> AgentMessage {
    AgentMessage::Message(Message::User(UserMessage {
        content: UserMessageContent::Text(text.to_owned()),
        timestamp: 1,
    }))
}

fn assistant_msg(text: &str) -> AgentMessage {
    AgentMessage::Message(Message::Assistant(AssistantMessage {
        content: vec![AssistantContentBlock::Text(faux_text(text))],
        api: "faux".to_owned(),
        provider: "faux".to_owned(),
        model: "faux".to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 2,
    }))
}

#[test]
fn replacement_session_fork_and_switch_isolate_context_and_session_files() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().join("cwd");
    let sess_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&sess_dir).unwrap();
    let cwd_str = cwd.to_string_lossy().into_owned();
    let dir_str = sess_dir.to_string_lossy().into_owned();

    let sm = SessionManager::create(&cwd_str, Some(&dir_str), None).unwrap();
    sm.append_message(user_msg("seed")).unwrap();
    sm.append_message(assistant_msg("seed reply")).unwrap();
    let original_file = sm.get_session_file().unwrap();

    // New session with parent_session yields fresh file path and empty context
    let replaced_file = sm
        .new_session(Some(NewSessionOptions {
            id: None,
            parent_session: Some(original_file.clone()),
        }))
        .unwrap();
    assert!(replaced_file.is_some());
    assert_ne!(replaced_file.as_deref(), Some(original_file.as_str()));
    assert!(sm.build_session_context().messages.is_empty());

    sm.append_message(user_msg("Hello from the new session!")).unwrap();
    sm.append_message(assistant_msg("hello reply")).unwrap();
    assert_eq!(sm.build_session_context().messages.len(), 2);

    // Fork from original file creates a third distinct session retaining only prior branch
    let forked = SessionManager::fork_from(&original_file, &cwd_str, Some(&dir_str), None).unwrap();
    forked.append_message(user_msg("fork callback message")).unwrap();
    forked.append_message(assistant_msg("fork reply")).unwrap();
    assert_eq!(forked.build_session_context().messages.len(), 4);
    assert_ne!(forked.get_session_file(), replaced_file);

    // Switching back via open restores target session cleanly
    let switched = SessionManager::open(&original_file, Some(&dir_str), None).unwrap();
    assert_eq!(switched.build_session_context().messages.len(), 2);
}
