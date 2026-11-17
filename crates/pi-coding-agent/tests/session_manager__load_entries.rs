//! Port of `packages/coding-agent/test/session-manager/load-entries.test.ts`.

use pi_agent_core::AgentMessage;
use pi_ai::{Message, TextContent, UserContentBlock, UserMessage, UserMessageContent};
use pi_coding_agent::core::session_manager::{
    FileEntry, NewSessionOptions, SessionEntry, SessionHeader, SessionManager,
};
use regex::Regex;
use serde_json::json;

fn user_msg(text: &str) -> AgentMessage {
    AgentMessage::Message(Message::User(UserMessage {
        content: UserMessageContent::Blocks(vec![UserContentBlock::Text(TextContent {
            text: text.to_owned(),
            text_signature: None,
        })]),
        timestamp: 1,
    }))
}

fn stored_entries(build: impl FnOnce(&SessionManager)) -> Vec<FileEntry> {
    let source = SessionManager::in_memory(Some("/project"), None, None).unwrap();
    build(&source);
    source
        .get_entries()
        .into_iter()
        .map(FileEntry::from_session_entry)
        .collect()
}

#[test]
fn in_memory_preloaded_entries_adopts_rebuilds_and_handles_headers_and_migration() {
    let entries = stored_entries(|src| {
        src.append_message(user_msg("hello")).unwrap();
        src.append_model_change("anthropic", "claude-opus-4-5").unwrap();
        src.append_message(user_msg("again")).unwrap();
    });
    let session = SessionManager::in_memory(Some("/project"), None, Some(entries.clone())).unwrap();
    let expected: Vec<SessionEntry> = entries.iter().filter_map(|e| e.as_session_entry()).collect();
    assert_eq!(session.get_entries(), expected);

    // Keeps loaded leaf
    let msg_entries = stored_entries(|src| {
        src.append_message(user_msg("hello")).unwrap();
        src.append_message(user_msg("again")).unwrap();
    });
    let last_id = msg_entries.last().unwrap().as_session_entry().unwrap().id().to_owned();
    let cont = SessionManager::in_memory(Some("/project"), None, Some(msg_entries)).unwrap();
    let appended_id = cont.append_message(user_msg("continued")).unwrap();
    assert_eq!(cont.get_leaf_id().as_deref(), Some(appended_id.as_str()));
    assert_eq!(
        cont.get_entry(&appended_id).unwrap().parent_id(),
        Some(last_id.as_str())
    );

    // Never mints colliding ID
    let many = stored_entries(|src| {
        for i in 0..50 {
            src.append_message(user_msg(&format!("message {i}"))).unwrap();
        }
    });
    let many_ids: Vec<String> = many
        .iter()
        .filter_map(|e| e.as_session_entry().map(|s| s.id().to_owned()))
        .collect();
    let many_sm = SessionManager::in_memory(Some("/project"), None, Some(many)).unwrap();
    let new_id = many_sm.append_message(user_msg("continued")).unwrap();
    assert!(!many_ids.contains(&new_id));

    // Rebuilds branch structure and labels and compaction target
    let mut labelled_id = String::new();
    let branched = stored_entries(|src| {
        let first_id = src.append_message(user_msg("hello")).unwrap();
        labelled_id = first_id.clone();
        src.append_message(user_msg("abandoned")).unwrap();
        src.branch(&first_id).unwrap();
        src.append_message(user_msg("kept")).unwrap();
        src.append_label_change(&first_id, Some("checkpoint")).unwrap();
    });
    let branched_sm = SessionManager::in_memory(Some("/project"), None, Some(branched)).unwrap();
    let roots = branched_sm.get_tree();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].children.len(), 2);
    assert_eq!(branched_sm.get_label(&labelled_id).as_deref(), Some("checkpoint"));

    let mut kept_id = String::new();
    let compacted = stored_entries(|src| {
        src.append_message(user_msg("dropped")).unwrap();
        kept_id = src.append_message(user_msg("kept")).unwrap();
        src.append_compaction("summary so far", Some(&kept_id), 1000, None, None, None)
            .unwrap();
    });
    let comp_sm = SessionManager::in_memory(Some("/project"), None, Some(compacted)).unwrap();
    assert!(comp_sm.build_context_entries().iter().any(|e| e.id() == kept_id));

    // Header creation / UUIDv7 / off filesystem / empty entries / header adoption
    let re = Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$").unwrap();
    let single = stored_entries(|src| {
        src.append_message(user_msg("hello")).unwrap();
    });
    let restored = SessionManager::in_memory(
        Some("/project"),
        Some(NewSessionOptions {
            id: Some("restored-session".to_owned()),
            parent_session: None,
        }),
        Some(single.clone()),
    )
    .unwrap();
    assert_eq!(restored.get_session_id(), "restored-session");
    assert_eq!(restored.get_header().unwrap().cwd, "/project");
    assert!(restored.get_session_file().is_none());
    assert!(!restored.is_persisted());

    let gen_sm = SessionManager::in_memory(Some("/project"), None, Some(single.clone())).unwrap();
    assert!(re.is_match(&gen_sm.get_session_id()));

    let empty_sm = SessionManager::in_memory(
        Some("/project"),
        Some(NewSessionOptions {
            id: Some("empty-session".to_owned()),
            parent_session: None,
        }),
        Some(Vec::new()),
    )
    .unwrap();
    assert!(empty_sm.get_entries().is_empty());
    assert!(empty_sm.get_leaf_id().is_none());

    let mut with_header = vec![FileEntry::Session(SessionHeader {
        version: Some(3),
        id: "stored-session".to_owned(),
        timestamp: "2026-01-01T00:00:00Z".to_owned(),
        cwd: "/stored".to_owned(),
        parent_session: None,
    })];
    with_header.extend(single);
    let from_hdr = SessionManager::in_memory(
        Some("/project"),
        Some(NewSessionOptions {
            id: Some("ignored".to_owned()),
            parent_session: None,
        }),
        Some(with_header),
    )
    .unwrap();
    assert_eq!(from_hdr.get_session_id(), "stored-session");
    assert_eq!(from_hdr.get_header().unwrap().cwd, "/stored");

    // Migrates older header entries (v2 hookMessage -> custom), adopts headerless verbatim
    let hook_entry: FileEntry = pi_js::json::parse_as(&pi_js::json::stringify(&json!({
        "type": "message",
        "id": "abc12345",
        "parentId": null,
        "timestamp": "2026-01-01T00:00:01Z",
        "message": { "role": "hookMessage", "content": "from a hook", "timestamp": 1 }
    })))
    .unwrap();
    let v2 = vec![
        FileEntry::Session(SessionHeader {
            version: Some(2),
            id: "v2-session".to_owned(),
            timestamp: "2026-01-01T00:00:00Z".to_owned(),
            cwd: "/project".to_owned(),
            parent_session: None,
        }),
        hook_entry.clone(),
    ];
    let migrated_sm = SessionManager::in_memory(Some("/project"), None, Some(v2)).unwrap();
    assert_eq!(migrated_sm.get_header().unwrap().version, Some(3));
    match &migrated_sm.get_entries()[0] {
        SessionEntry::Message(m) => assert_eq!(m.message.role(), Some("custom")),
        other => panic!("expected message: {other:?}"),
    }

    let headerless_sm = SessionManager::in_memory(Some("/project"), None, Some(vec![hook_entry])).unwrap();
    match &headerless_sm.get_entries()[0] {
        SessionEntry::Message(m) => assert_eq!(m.message.role(), Some("hookMessage")),
        other => panic!("expected message: {other:?}"),
    }
}
