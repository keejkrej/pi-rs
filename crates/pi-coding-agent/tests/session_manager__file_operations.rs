//! Port of `packages/coding-agent/test/session-manager/file-operations.test.ts`.

use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use pi_agent_core::AgentMessage;
use pi_ai::{
    AssistantContentBlock, AssistantMessage, Message, StopReason, Usage, UserMessage, UserMessageContent, faux_text,
};
use pi_coding_agent::core::session_manager::{
    SessionInfo, SessionManager, find_most_recent_session, load_entries_from_file,
};
use pi_js::abort::AbortController;
use serde_json::{Value, json};
use tempfile::TempDir;

const HEADER_SCAN_LIMIT_BYTES: usize = 1024 * 1024;

fn user_msg(text: &str) -> AgentMessage {
    AgentMessage::Message(Message::User(UserMessage {
        content: UserMessageContent::Text(text.to_owned()),
        timestamp: 1,
    }))
}

fn assistant_msg(text: &str) -> AgentMessage {
    AgentMessage::Message(Message::Assistant(AssistantMessage {
        content: vec![AssistantContentBlock::Text(faux_text(text))],
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
    }))
}

fn write_session_header(file: &Path, cwd: &str, id: &str, prefix: &str) {
    let header = pi_js::json::stringify(&json!({
        "type": "session",
        "version": 3,
        "id": id,
        "timestamp": "2025-01-01T00:00:00Z",
        "cwd": cwd,
    }));
    std::fs::write(file, format!("{prefix}{header}\n")).unwrap();
}

fn read_session_file_roles(path: &Path) -> Vec<String> {
    let raw = std::fs::read_to_string(path).unwrap();
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: Value = pi_js::json::parse(l).unwrap();
            let t = v.get("type").and_then(Value::as_str).unwrap_or("");
            if t == "message" {
                v.get("message")
                    .and_then(|m| m.get("role"))
                    .and_then(Value::as_str)
                    .unwrap_or("message")
                    .to_owned()
            } else {
                t.to_owned()
            }
        })
        .collect()
}

#[test]
fn load_entries_from_file_cases() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();

    assert!(
        load_entries_from_file(dir.join("nonexistent.jsonl").to_str().unwrap())
            .unwrap()
            .is_empty()
    );

    let empty = dir.join("empty.jsonl");
    std::fs::write(&empty, "").unwrap();
    assert!(load_entries_from_file(empty.to_str().unwrap()).unwrap().is_empty());

    let no_hdr = dir.join("no-header.jsonl");
    std::fs::write(&no_hdr, "{\"type\":\"message\",\"id\":\"1\"}\n").unwrap();
    assert!(load_entries_from_file(no_hdr.to_str().unwrap()).unwrap().is_empty());

    let malformed = dir.join("malformed.jsonl");
    std::fs::write(&malformed, "not json\n").unwrap();
    assert!(load_entries_from_file(malformed.to_str().unwrap()).unwrap().is_empty());

    let valid = dir.join("valid.jsonl");
    std::fs::write(
        &valid,
        "{\"type\":\"session\",\"id\":\"abc\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"cwd\":\"/tmp\"}\n\
         {\"type\":\"message\",\"id\":\"1\",\"parentId\":null,\"timestamp\":\"2025-01-01T00:00:01Z\",\"message\":{\"role\":\"user\",\"content\":\"hi\",\"timestamp\":1}}\n",
    )
    .unwrap();
    let valid_entries = load_entries_from_file(valid.to_str().unwrap()).unwrap();
    assert_eq!(valid_entries.len(), 2);

    let mixed = dir.join("mixed.jsonl");
    std::fs::write(
        &mixed,
        "{\"type\":\"session\",\"id\":\"abc\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"cwd\":\"/tmp\"}\n\
         not valid json\n\
         {\"type\":\"message\",\"id\":\"1\",\"parentId\":null,\"timestamp\":\"2025-01-01T00:00:01Z\",\"message\":{\"role\":\"user\",\"content\":\"hi\",\"timestamp\":1}}\n",
    )
    .unwrap();
    assert_eq!(load_entries_from_file(mixed.to_str().unwrap()).unwrap().len(), 2);

    let unterminated = dir.join("unterminated.jsonl");
    let content = "{\"type\":\"session\",\"id\":\"abc\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"cwd\":\"/tmp\"}\n\
         {\"type\":\"message\",\"id\":\"1\",\"parentId\":null,\"timestamp\":\"2025-01-01T00:00:01Z\",\"message\":{\"role\":\"user\",\"content\":\"hi\",\"timestamp\":1}}";
    std::fs::write(&unterminated, content).unwrap();
    assert_eq!(load_entries_from_file(unterminated.to_str().unwrap()).unwrap().len(), 2);
    assert_eq!(std::fs::read_to_string(&unterminated).unwrap(), format!("{content}\n"));

    let malformed_tail = dir.join("malformed-tail.jsonl");
    let tail_content = "{\"type\":\"session\",\"id\":\"abc\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"cwd\":\"/tmp\"}\n{\"type\":\"message\"";
    std::fs::write(&malformed_tail, tail_content).unwrap();
    assert_eq!(
        load_entries_from_file(malformed_tail.to_str().unwrap()).unwrap().len(),
        1
    );
    assert_eq!(
        std::fs::read_to_string(&malformed_tail).unwrap(),
        format!("{tail_content}\n")
    );

    let non_session = dir.join("invalid.jsonl");
    let non_session_content = "{\"type\":\"message\",\"id\":\"1\"}";
    std::fs::write(&non_session, non_session_content).unwrap();
    assert!(
        load_entries_from_file(non_session.to_str().unwrap())
            .unwrap()
            .is_empty()
    );
    assert_eq!(std::fs::read_to_string(&non_session).unwrap(), non_session_content);
}

#[test]
fn reads_cwd_across_edge_headers_and_opens_large_sparse_files() {
    let tmp = TempDir::new().unwrap();
    let dir_str = tmp.path().to_str().unwrap();
    let stored_cwd = tmp.path().join("stored-project");
    let stored_str = stored_cwd.to_str().unwrap();

    for (prefix, sid) in [
        ("\n  \n".to_owned(), "leading-blank".to_owned()),
        ("not json\n{broken json\n".to_owned(), "leading-malformed".to_owned()),
        (String::new(), "a".repeat(8192)),
    ] {
        let f = tmp.path().join("header.jsonl");
        write_session_header(&f, stored_str, &sid, &prefix);
        let sm = SessionManager::open(f.to_str().unwrap(), Some(dir_str), None).unwrap();
        assert_eq!(sm.get_session_id(), sid);
        assert_eq!(sm.get_cwd(), stored_str);
    }

    let override_cwd = tmp.path().join("override-project").to_string_lossy().into_owned();
    for (name, id, prefix) in [
        ("large-header", "a".repeat(HEADER_SCAN_LIMIT_BYTES + 1), String::new()),
        (
            "large-prefix",
            "large-prefix".to_owned(),
            format!("{}\n", "x".repeat(HEADER_SCAN_LIMIT_BYTES + 1)),
        ),
    ] {
        let f = tmp.path().join(format!("{name}.jsonl"));
        write_session_header(&f, stored_str, &id, &prefix);
        for ov in [None, Some(override_cwd.as_str())] {
            let sm = SessionManager::open(f.to_str().unwrap(), Some(dir_str), ov).unwrap();
            assert_eq!(sm.get_session_id(), id);
            assert_eq!(sm.get_cwd(), ov.unwrap_or(stored_str));
        }
    }

    // Large sparse session file (> 16 MiB)
    let large = tmp.path().join("large.jsonl");
    std::fs::write(
        &large,
        "{\"type\":\"session\",\"version\":3,\"id\":\"abc\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"cwd\":\"/tmp\"}\n",
    )
    .unwrap();
    {
        let mut f = std::fs::OpenOptions::new().write(true).open(&large).unwrap();
        f.seek(SeekFrom::Start(32 * 1024 * 1024)).unwrap();
        f.write_all(b"\n").unwrap();
    }
    {
        let mut f = std::fs::OpenOptions::new().append(true).open(&large).unwrap();
        f.write_all(
            b"{\"type\":\"message\",\"id\":\"1\",\"parentId\":null,\"timestamp\":\"2025-01-01T00:00:01Z\",\"message\":{\"role\":\"user\",\"content\":\"hi\",\"timestamp\":1}}\n",
        )
        .unwrap();
    }
    let sm = SessionManager::open(large.to_str().unwrap(), Some(dir_str), None).unwrap();
    assert_eq!(sm.get_session_id(), "abc");
    assert_eq!(sm.get_entries().len(), 1);
    assert_eq!(sm.build_session_context().messages.len(), 1);
}

#[tokio::test]
async fn find_most_recent_and_custom_flat_dir_and_corrupted_recovery() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    let dir_str = dir.to_str().unwrap();

    assert_eq!(find_most_recent_session(dir_str, None), None);
    assert_eq!(
        find_most_recent_session(dir.join("nonexistent").to_str().unwrap(), None),
        None
    );

    std::fs::write(dir.join("file.txt"), "hello").unwrap();
    std::fs::write(dir.join("file.json"), "{}").unwrap();
    assert_eq!(find_most_recent_session(dir_str, None), None);

    std::fs::write(dir.join("invalid.jsonl"), "{\"type\":\"message\"}\n").unwrap();
    assert_eq!(find_most_recent_session(dir_str, None), None);

    let f1 = dir.join("older.jsonl");
    let f2 = dir.join("newer.jsonl");
    std::fs::write(
        &f1,
        "{\"type\":\"session\",\"id\":\"old\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"cwd\":\"/tmp\"}\n",
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(15)).await;
    std::fs::write(
        &f2,
        "{\"type\":\"session\",\"id\":\"new\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"cwd\":\"/tmp\"}\n",
    )
    .unwrap();
    assert_eq!(
        find_most_recent_session(dir_str, None),
        Some(f2.to_string_lossy().into_owned())
    );

    let oversized = dir.join("oversized.jsonl");
    std::fs::write(&oversized, "x".repeat(HEADER_SCAN_LIMIT_BYTES + 1)).unwrap();
    assert_eq!(
        find_most_recent_session(dir_str, None),
        Some(f2.to_string_lossy().into_owned())
    );

    // Filter by cwd
    let cwd_dir = tmp.path().join("cwd-filter");
    std::fs::create_dir_all(&cwd_dir).unwrap();
    let project_a = cwd_dir.join("project-a");
    let project_b = cwd_dir.join("project-b");
    let pa = project_a.to_str().unwrap();
    let pb = project_b.to_str().unwrap();
    write_session_header(&cwd_dir.join("a.jsonl"), pa, "a", "");
    tokio::time::sleep(Duration::from_millis(15)).await;
    write_session_header(&cwd_dir.join("b.jsonl"), pb, "b", "");
    assert_eq!(
        find_most_recent_session(cwd_dir.to_str().unwrap(), Some(pa)),
        Some(cwd_dir.join("a.jsonl").to_string_lossy().into_owned())
    );
    assert_eq!(
        find_most_recent_session(cwd_dir.to_str().unwrap(), Some(pb)),
        Some(cwd_dir.join("b.jsonl").to_string_lossy().into_owned())
    );

    // Custom flat dir + cancel listing
    let flat_dir = tmp.path().join("flat");
    std::fs::create_dir_all(&flat_dir).unwrap();
    std::fs::create_dir_all(&project_a).unwrap();
    std::fs::create_dir_all(&project_b).unwrap();
    let flat_str = flat_dir.to_str().unwrap();
    let sm_a = SessionManager::create(pa, Some(flat_str), None).unwrap();
    sm_a.append_message(user_msg("from A")).unwrap();
    sm_a.append_message(assistant_msg("reply A")).unwrap();
    let path_a = sm_a.get_session_file().unwrap();
    tokio::time::sleep(Duration::from_millis(15)).await;
    let sm_b = SessionManager::create(pb, Some(flat_str), None).unwrap();
    sm_b.append_message(user_msg("from B")).unwrap();
    sm_b.append_message(assistant_msg("reply B")).unwrap();
    let path_b = sm_b.get_session_file().unwrap();

    let list_a = SessionManager::list(pa, Some(flat_str), None, None).await.unwrap();
    assert_eq!(
        list_a.iter().map(|s| s.path.as_str()).collect::<Vec<_>>(),
        vec![path_a.as_str()]
    );
    let all = SessionManager::list_all(Some(flat_str), None, None).await.unwrap();
    assert_eq!(all.len(), 2);
    assert!(all.iter().any(|s| s.path == path_a) && all.iter().any(|s| s.path == path_b));
    let cont_a = SessionManager::continue_recent(pa, Some(flat_str)).unwrap();
    assert_eq!(cont_a.get_session_file(), Some(path_a));

    let cancel_dir = tmp.path().join("cancel-flat");
    std::fs::create_dir_all(&cancel_dir).unwrap();
    let cancel_str = cancel_dir.to_str().unwrap();
    for i in 0..12 {
        write_session_header(&cancel_dir.join(format!("s-{i}.jsonl")), pa, &format!("id-{i}"), "");
    }

    let ctrl = Arc::new(AbortController::new());
    let ctrl_cb = Arc::clone(&ctrl);
    let aborted = Arc::new(AtomicBool::new(false));
    let aborted_cb = Arc::clone(&aborted);
    let progress = move |_loaded: usize, _total: usize, partial: Option<&[SessionInfo]>| {
        if partial.is_some() && !aborted_cb.swap(true, Ordering::SeqCst) {
            ctrl_cb.abort(None);
        }
    };
    let sig = ctrl.signal();
    let cancelled = SessionManager::list_all(Some(cancel_str), Some(&progress), Some(&sig)).await;
    assert!(cancelled.unwrap_err().is_abort());
    assert!(
        SessionManager::list_all(None, None, Some(&sig))
            .await
            .unwrap_err()
            .is_abort()
    );

    // Corrupted files + empty file rewrite
    let corrupt_dir = tmp.path().join("corrupt");
    std::fs::create_dir_all(&corrupt_dir).unwrap();
    let cdir = corrupt_dir.to_str().unwrap();
    let empty_file = corrupt_dir.join("empty.jsonl");
    std::fs::write(&empty_file, "").unwrap();
    let sm_empty = SessionManager::open(empty_file.to_str().unwrap(), Some(cdir), None).unwrap();
    assert!(!sm_empty.get_session_id().is_empty());
    let content = std::fs::read_to_string(&empty_file).unwrap();
    assert!(content.contains("\"type\":\"session\""));

    let sm_reopened = SessionManager::open(empty_file.to_str().unwrap(), Some(cdir), None).unwrap();
    assert_eq!(sm_reopened.get_session_id(), sm_empty.get_session_id());

    let no_hdr_file = corrupt_dir.join("no-header.jsonl");
    let orig = "{\"type\":\"message\",\"id\":\"abc\",\"parentId\":\"orphaned\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"message\":{\"role\":\"assistant\",\"content\":\"test\"}}\n";
    std::fs::write(&no_hdr_file, orig).unwrap();
    let err_no_hdr = match SessionManager::open(no_hdr_file.to_str().unwrap(), Some(cdir), None) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("expected invalid session error"),
    };
    assert!(err_no_hdr.contains("Session file is not a valid pi session"));
    assert_eq!(std::fs::read_to_string(&no_hdr_file).unwrap(), orig);

    // #10000: lazy file creation on first user message & incremental append
    let persist_dir = tmp.path().join("persist");
    std::fs::create_dir_all(&persist_dir).unwrap();
    let pdir = persist_dir.to_str().unwrap();
    let setup_only = SessionManager::create(pdir, Some(pdir), None).unwrap();
    setup_only
        .append_model_change("anthropic", "claude-sonnet-4-5")
        .unwrap();
    setup_only.append_thinking_level_change("off").unwrap();
    assert!(!Path::new(&setup_only.get_session_file().unwrap()).exists());

    let first_user = SessionManager::create(pdir, Some(pdir), None).unwrap();
    first_user
        .append_model_change("anthropic", "claude-sonnet-4-5")
        .unwrap();
    first_user.append_message(user_msg("first question")).unwrap();
    let file_u = first_user.get_session_file().unwrap();
    assert_eq!(
        read_session_file_roles(Path::new(&file_u)),
        vec!["session", "model_change", "user"]
    );
    assert_eq!(
        SessionManager::open(&file_u, Some(pdir), None)
            .unwrap()
            .build_session_context()
            .messages
            .len(),
        1
    );

    let inc = SessionManager::create(pdir, Some(pdir), None).unwrap();
    inc.append_message(user_msg("first question")).unwrap();
    inc.append_custom_entry("preset-state", Some(json!({ "name": "plan" })))
        .unwrap();
    inc.append_message(assistant_msg("first answer")).unwrap();
    assert_eq!(
        read_session_file_roles(Path::new(&inc.get_session_file().unwrap())),
        vec!["session", "user", "custom", "assistant"]
    );
}
