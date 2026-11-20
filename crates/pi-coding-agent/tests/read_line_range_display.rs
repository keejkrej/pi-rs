//! Port of packages/coding-agent/test/read-line-range-display.test.ts

use std::sync::Arc;

use async_trait::async_trait;
use pi_agent_core::AgentMessage;
use pi_ai::{Message, StopReason};
use pi_coding_agent::core::export_html::{ExportOptions, export_from_file};
use pi_coding_agent::core::keybindings::{KEYBINDINGS, KeybindingsManager};
use pi_coding_agent::core::session_manager::{CURRENT_SESSION_VERSION, SessionManager, SessionMessageEntry};
use pi_coding_agent::core::tools::read::create_read_tool_definition;
use pi_coding_agent::core::tools::render_utils::format_read_line_range;
use pi_coding_agent::modes::interactive::components::tool_execution::{ToolExecutionComponent, ToolRenderers};
use pi_coding_agent::modes::interactive::components::tree_selector::TreeSelectorComponent;
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_coding_agent::utils::ansi::strip_ansi;
use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
use pi_tui::{
    Component, KeybindingsManager as TuiKeybindingsManager, TUI, TUI_KEYBINDINGS, TuiMainScreen, set_keybindings,
};
use serde_json::{Value, json};
use serial_test::serial;

struct FakeTerminal;

#[async_trait]
impl Terminal for FakeTerminal {
    fn start(&self, _on_input: TerminalInputHandler, _on_resize: TerminalResizeHandler) {}
    fn stop(&self) {}
    async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}
    fn write(&self, _data: &str) {}
    fn columns(&self) -> i64 {
        120
    }
    fn rows(&self) -> i64 {
        40
    }
    fn kitty_protocol_active(&self) -> bool {
        false
    }
    fn move_by(&self, _lines: i64) {}
    fn hide_cursor(&self) {}
    fn show_cursor(&self) {}
    fn clear_line(&self) {}
    fn clear_from_cursor(&self) {}
    fn clear_screen(&self) {}
    fn set_title(&self, _title: &str) {}
    fn set_progress(&self, _active: bool) {}
}

struct TestGuard;

impl Drop for TestGuard {
    fn drop(&mut self) {
        init_theme(Some("dark"), None);
        set_keybindings(TuiKeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
    }
}

// #9887
#[test]
fn formats_raw_read_arguments() {
    let cases: [(Option<Value>, Option<Value>, &str); 8] = [
        (Some(json!(25)), Some(json!(13)), ":25-37"),
        (Some(json!("25")), Some(json!("13")), ":25-37"),
        (None, Some(json!("13")), ":1-13"),
        (Some(json!("25")), None, ":25"),
        (None, None, ""),
        (Some(json!("abc")), Some(json!(13)), ""),
        (Some(json!(25)), Some(json!("")), ""),
        (Some(json!(25)), Some(json!("Infinity")), ""),
    ];
    for (offset, limit, expected) in cases {
        assert_eq!(
            format_read_line_range(offset.as_ref(), limit.as_ref()),
            expected,
            "offset={offset:?}, limit={limit:?}"
        );
    }
}

// #9887
#[tokio::test]
#[serial]
async fn shows_string_ranges_in_tui_session_tree_and_html_export() {
    let _guard = TestGuard;
    init_theme(Some("dark"), Some(false));
    let kb = KeybindingsManager::new(None, None);
    set_keybindings(TuiKeybindingsManager::new(
        KEYBINDINGS.clone(),
        Some(kb.get_user_bindings()),
    ));

    let temp = tempfile::tempdir().expect("tempdir");
    let temp_dir = temp.path().to_str().expect("utf8");
    let args = json!({ "path": "notes.txt", "offset": "25", "limit": "13" });

    let assistant_msg: Message = serde_json::from_value(json!({
        "role": "assistant",
        "content": [{ "type": "toolCall", "id": "call", "name": "read", "arguments": args }],
        "api": "anthropic-messages",
        "provider": "anthropic",
        "model": "test-model",
        "usage": {
            "input": 0,
            "output": 0,
            "cacheRead": 0,
            "cacheWrite": 0,
            "totalTokens": 0,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
        },
        "stopReason": StopReason::ToolUse,
        "timestamp": 0
    }))
    .expect("assistant msg");
    let result_msg: Message = serde_json::from_value(json!({
        "role": "toolResult",
        "toolCallId": "call",
        "toolName": "read",
        "content": [],
        "isError": false,
        "timestamp": 0
    }))
    .expect("tool result msg");

    let assistant = SessionMessageEntry {
        id: "assistant".to_string(),
        parent_id: None,
        timestamp: "1970-01-01T00:00:00.000Z".to_string(),
        message: AgentMessage::Message(assistant_msg),
    };
    let result = SessionMessageEntry {
        id: "result".to_string(),
        parent_id: Some("assistant".to_string()),
        timestamp: "1970-01-01T00:00:00.000Z".to_string(),
        message: AgentMessage::Message(result_msg),
    };
    let header = json!({
        "type": "session",
        "version": CURRENT_SESSION_VERSION,
        "id": "range-test",
        "timestamp": "1970-01-01T00:00:00.000Z",
        "cwd": temp_dir,
    });

    let session_path = temp.path().join("session.jsonl");
    let jsonl = format!(
        "{}\n{}\n{}\n",
        pi_js::json::stringify(&header),
        pi_js::json::stringify(&pi_coding_agent::core::session_manager::SessionEntry::Message(
            assistant
        )),
        pi_js::json::stringify(&pi_coding_agent::core::session_manager::SessionEntry::Message(result)),
    );
    std::fs::write(&session_path, jsonl).expect("write session.jsonl");

    let ui: Arc<dyn TUI + Send + Sync> = Arc::new(TuiMainScreen::new(Arc::new(FakeTerminal), None, None));
    let read_def = create_read_tool_definition(temp_dir, None);
    let tool = ToolExecutionComponent::new(
        "read",
        "call",
        args,
        None,
        Some(ToolRenderers::from_tool_definition(&read_def)),
        ui,
        temp_dir,
    );
    let rendered_tool = strip_ansi(&tool.render(120).join("\n"));
    assert!(
        rendered_tool.contains("read notes.txt:25-37"),
        "tool render: {rendered_tool}"
    );

    let session_mgr = SessionManager::open(session_path.to_str().expect("utf8"), None, None).expect("open session");
    let tree = TreeSelectorComponent::new(
        session_mgr.get_tree(),
        Some("result".to_string()),
        40,
        Arc::new(|_| {}),
        Arc::new(|| {}),
        None,
        None,
        None,
    );
    let rendered_tree = strip_ansi(&tree.get_tree_list().render(120).join("\n"));
    assert!(
        rendered_tree.contains("[read: notes.txt:25-37]"),
        "tree render: {rendered_tree}"
    );

    let out_html = temp.path().join("out.html");
    let exported_path = export_from_file(
        session_path.to_str().expect("utf8"),
        Some(ExportOptions {
            output_path: Some(out_html.to_str().expect("utf8").to_string()),
            theme_name: None,
            theme_override: None,
            tool_renderer: None,
        }),
    )
    .await
    .expect("export html");
    let html = std::fs::read_to_string(&exported_path).expect("read exported html");
    assert!(html.contains("function formatReadLineRange(offset, limit)"));
    assert!(html.contains("<span class=\"line-numbers\">"));
    assert!(html.contains("return `[read: ${path}${formatReadLineRange(args.offset, args.limit)}]`;"));
    assert!(html.contains("id=\"session-data\""));
}
