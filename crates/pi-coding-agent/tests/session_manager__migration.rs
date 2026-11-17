//! Port of `packages/coding-agent/test/session-manager/migration.test.ts`.

use pi_coding_agent::core::session_manager::{FileEntry, migrate_session_entries};
use serde_json::json;

#[test]
fn adds_id_and_parent_id_to_v1_entries() {
    let mut entries: Vec<FileEntry> = vec![
        pi_js::json::parse_as(&pi_js::json::stringify(&json!({
            "type": "session",
            "id": "sess-1",
            "timestamp": "2025-01-01T00:00:00Z",
            "cwd": "/tmp",
        })))
        .unwrap(),
        pi_js::json::parse_as(&pi_js::json::stringify(&json!({
            "type": "message",
            "timestamp": "2025-01-01T00:00:01Z",
            "message": { "role": "user", "content": "hi", "timestamp": 1 },
        })))
        .unwrap(),
        pi_js::json::parse_as(&pi_js::json::stringify(&json!({
            "type": "message",
            "timestamp": "2025-01-01T00:00:02Z",
            "message": {
                "role": "assistant",
                "content": [{ "type": "text", "text": "hello" }],
                "api": "test",
                "provider": "test",
                "model": "test",
                "usage": { "input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2, "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 } },
                "stopReason": "stop",
                "timestamp": 2,
            },
        })))
        .unwrap(),
    ];

    migrate_session_entries(&mut entries);

    match &entries[0] {
        FileEntry::Session(h) => assert_eq!(h.version, Some(3)),
        other => panic!("expected session header: {other:?}"),
    }
    let m1_id = match &entries[1] {
        FileEntry::Message(m) => {
            assert_eq!(m.id.len(), 8);
            assert_eq!(m.parent_id, None);
            m.id.clone()
        }
        other => panic!("expected msg1: {other:?}"),
    };
    match &entries[2] {
        FileEntry::Message(m) => {
            assert_eq!(m.id.len(), 8);
            assert_eq!(m.parent_id.as_deref(), Some(m1_id.as_str()));
        }
        other => panic!("expected msg2: {other:?}"),
    }
}

#[test]
fn idempotent_on_already_migrated_entries() {
    let mut entries: Vec<FileEntry> = vec![
        pi_js::json::parse_as(&pi_js::json::stringify(&json!({
            "type": "session",
            "id": "sess-1",
            "version": 2,
            "timestamp": "2025-01-01T00:00:00Z",
            "cwd": "/tmp",
        })))
        .unwrap(),
        pi_js::json::parse_as(&pi_js::json::stringify(&json!({
            "type": "message",
            "id": "abc12345",
            "parentId": null,
            "timestamp": "2025-01-01T00:00:01Z",
            "message": { "role": "user", "content": "hi", "timestamp": 1 },
        })))
        .unwrap(),
        pi_js::json::parse_as(&pi_js::json::stringify(&json!({
            "type": "message",
            "id": "def67890",
            "parentId": "abc12345",
            "timestamp": "2025-01-01T00:00:02Z",
            "message": {
                "role": "assistant",
                "content": [{ "type": "text", "text": "hello" }],
                "api": "test",
                "provider": "test",
                "model": "test",
                "usage": { "input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2, "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 } },
                "stopReason": "stop",
                "timestamp": 2,
            },
        })))
        .unwrap(),
    ];

    migrate_session_entries(&mut entries);

    match &entries[1] {
        FileEntry::Message(m) => assert_eq!(m.id, "abc12345"),
        other => panic!("expected msg1: {other:?}"),
    }
    match &entries[2] {
        FileEntry::Message(m) => {
            assert_eq!(m.id, "def67890");
            assert_eq!(m.parent_id.as_deref(), Some("abc12345"));
        }
        other => panic!("expected msg2: {other:?}"),
    }
}
