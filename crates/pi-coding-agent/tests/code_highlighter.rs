//! Port of packages/coding-agent/test/code-highlighter.test.ts

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use pi_ai::types::{TextContent, UserContentBlock};
use pi_coding_agent::core::keybindings::{KEYBINDINGS, KeybindingsManager};
use pi_coding_agent::core::tools::write::create_write_tool_definition;
use pi_coding_agent::extensions::codemode::tool::{CodemodeToolOptions, create_codemode_tool_definition};
use pi_coding_agent::modes::interactive::components::tool_execution::{
    ToolExecutionComponent, ToolExecutionResult, ToolRenderers,
};
use pi_coding_agent::modes::interactive::theme::theme::{CodeHighlighter, highlight_code, init_theme};
use pi_coding_agent::utils::syntax_highlight::{load_all_highlight_languages, testing};
use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
use pi_tui::{KeybindingsManager as TuiKeybindingsManager, TUI, TUI_KEYBINDINGS, TuiMainScreen, set_keybindings};
use serde_json::json;
use serial_test::serial;

struct FakeTerminal;
#[async_trait]
impl Terminal for FakeTerminal {
    fn start(&self, _: TerminalInputHandler, _: TerminalResizeHandler) {}
    fn stop(&self) {}
    async fn drain_input(&self, _: Option<i64>, _: Option<i64>) {}
    fn write(&self, _: &str) {}
    fn columns(&self) -> i64 {
        80
    }
    fn rows(&self) -> i64 {
        24
    }
    fn kitty_protocol_active(&self) -> bool {
        false
    }
    fn move_by(&self, _: i64) {}
    fn hide_cursor(&self) {}
    fn show_cursor(&self) {}
    fn clear_line(&self) {}
    fn clear_from_cursor(&self) {}
    fn clear_screen(&self) {}
    fn set_title(&self, _: &str) {}
    fn set_progress(&self, _: bool) {}
}

struct TestGuard;
impl Drop for TestGuard {
    fn drop(&mut self) {
        init_theme(Some("dark"), None);
        set_keybindings(TuiKeybindingsManager::new(TUI_KEYBINDINGS.clone(), None));
    }
}

fn setup() -> (TestGuard, Arc<dyn TUI + Send + Sync>) {
    init_theme(Some("dark"), Some(false));
    let kb = KeybindingsManager::new(None, None);
    set_keybindings(TuiKeybindingsManager::new(
        KEYBINDINGS.clone(),
        Some(kb.get_user_bindings()),
    ));
    let ui: Arc<dyn TUI + Send + Sync> = Arc::new(TuiMainScreen::new(Arc::new(FakeTerminal), None, None));
    (TestGuard, ui)
}

#[tokio::test]
#[serial]
async fn reuses_last_result_until_code_theme_or_grammars_change() {
    let _guard = setup();
    let code = "const page = html`<div class=\"x\">ok</div>`;";
    let highlighter = CodeHighlighter::new();
    let first = highlighter.highlight(code, Some("javascript"));
    assert!(Arc::ptr_eq(&highlighter.highlight(code, Some("javascript")), &first));
    assert!(!Arc::ptr_eq(
        &highlighter.highlight("const a = 1;", Some("javascript")),
        &first
    ));

    init_theme(Some("light"), Some(false));
    assert_eq!(
        *highlighter.highlight(code, Some("javascript")),
        highlight_code(code, Some("javascript"))
    );
    load_all_highlight_languages().await;
    assert_eq!(
        *highlighter.highlight(code, Some("javascript")),
        highlight_code(code, Some("javascript"))
    );
}

#[tokio::test(start_paused = true)]
#[serial]
async fn codemode_highlights_script_once_across_progress_timer_ticks_expanding_and_completion() {
    let (_guard, ui) = setup();
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_c = Arc::clone(&calls);
    let _hook = testing::override_on_highlight(Arc::new(move || {
        calls_c.fetch_add(1, Ordering::SeqCst);
    }));

    let cm_def = create_codemode_tool_definition(CodemodeToolOptions::default());
    let row = ToolExecutionComponent::new(
        "codemode",
        "c",
        json!({ "code": "await tools.read({});" }),
        None,
        Some(ToolRenderers::from_tool_definition(&cm_def)),
        ui,
        "/",
    );
    row.set_args_complete();
    row.mark_execution_started();
    row.update_result(
        ToolExecutionResult {
            content: Vec::new(),
            details: Some(json!({ "calls": [] })),
            is_error: false,
            duration_ms: None,
        },
        Some(true),
    );
    tokio::time::advance(Duration::from_millis(2000)).await;
    row.set_expanded(true);
    row.update_result(
        ToolExecutionResult {
            content: vec![UserContentBlock::Text(TextContent::new("done"))],
            details: Some(json!({ "calls": [] })),
            is_error: false,
            duration_ms: None,
        },
        Some(false),
    );
    row.render(80);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
#[serial]
fn write_highlights_unchanged_first_lines_and_complete_file_only_once() {
    let (_guard, ui) = setup();
    let content = (0..60)
        .map(|i| format!("const v{i} = {i};"))
        .collect::<Vec<_>>()
        .join("\n");
    let write_def = create_write_tool_definition("/", None);
    let row = ToolExecutionComponent::new(
        "write",
        "w",
        json!({ "path": "a.ts", "content": content }),
        None,
        Some(ToolRenderers::from_tool_definition(&write_def)),
        ui,
        "/",
    );
    row.update_args(json!({ "path": "a.ts", "content": format!("{content} //") }));
    row.render(80);

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_c = Arc::clone(&calls);
    let _hook = testing::override_on_highlight(Arc::new(move || {
        calls_c.fetch_add(1, Ordering::SeqCst);
    }));

    // Only the changed last line is highlighted again (prefix 50 lines hit CodeHighlighter cache).
    row.update_args(json!({ "path": "a.ts", "content": format!("{content} // more") }));
    row.render(80);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    row.set_args_complete();
    row.set_expanded(true);
    row.set_expanded(false);
    row.render(80);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
