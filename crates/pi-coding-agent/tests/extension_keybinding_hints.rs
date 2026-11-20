//! Port of packages/coding-agent/test/extension-keybinding-hints.test.ts

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_coding_agent::core::extensions::types::ToolDefinition;
use pi_coding_agent::core::keybindings::{KEYBINDINGS, KeybindingsManager};
use pi_coding_agent::core::messages::CustomMessage;
use pi_coding_agent::core::session_manager::CustomEntry;
use pi_coding_agent::modes::interactive::components::custom_entry::CustomEntryComponent;
use pi_coding_agent::modes::interactive::components::custom_message::CustomMessageComponent;
use pi_coding_agent::modes::interactive::components::tool_execution::{ToolExecutionComponent, ToolRenderers};
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_coding_agent::utils::ansi::strip_ansi;
use pi_schema::unknown;
use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
use pi_tui::{
    KeybindingsConfig, KeybindingsManager as TuiKeybindingsManager, TUI, TUI_KEYBINDINGS, Text, TuiMainScreen,
    set_keybindings,
};
use serde_json::json;
use serial_test::serial;

const EXPECTED: &str = "ctrl+e | ctrl+e to expand";

struct FakeTerminal;

#[async_trait]
impl Terminal for FakeTerminal {
    fn start(&self, _on_input: TerminalInputHandler, _on_resize: TerminalResizeHandler) {}
    fn stop(&self) {}
    async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}
    fn write(&self, _data: &str) {}
    fn columns(&self) -> i64 {
        80
    }
    fn rows(&self) -> i64 {
        24
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

fn setup_host_keybindings() -> TestGuard {
    init_theme(Some("dark"), Some(false));
    let mut user_cfg: KeybindingsConfig = IndexMap::new();
    user_cfg.insert(
        "app.tools.expand".to_string(),
        Some(pi_tui::keybindings::KeyIdOrList::One("ctrl+e".to_string())),
    );
    let kb = KeybindingsManager::new(Some(user_cfg), None);
    set_keybindings(TuiKeybindingsManager::new(
        KEYBINDINGS.clone(),
        Some(kb.get_user_bindings()),
    ));
    TestGuard
}

#[test]
#[serial]
fn tool_renderers_receive_host_key_text_and_key_hint_in_render_context() {
    let _guard = setup_host_keybindings();
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen_c = Arc::clone(&seen);

    let tool_def = ToolDefinition {
        name: "custom_tool".to_string(),
        label: "custom_tool".to_string(),
        description: "custom tool".to_string(),
        prompt_snippet: None,
        prompt_guidelines: None,
        parameters: unknown().into(),
        constrained_sampling: None,
        render_shell: None,
        prepare_arguments: None,
        output_schema: None,
        exposure: None,
        namespace: None,
        annotations: None,
        default_active: None,
        prepare_loadout: None,
        execution_mode: None,
        execute: Arc::new(|_, _, _, _, _| {
            Box::pin(async {
                Ok(pi_agent_core::AgentToolResult {
                    content: vec![],
                    ..Default::default()
                })
            })
        }),
        render_call: Some(Arc::new(move |_args, _theme, ctx| {
            let text = format!(
                "{} | {}",
                ctx.key_text("app.tools.expand"),
                strip_ansi(&ctx.key_hint("app.tools.expand", "to expand"))
            );
            seen_c.lock().unwrap_or_else(|e| e.into_inner()).push(text);
            Arc::new(Text::new(Some("call"), Some(0), Some(0), None))
        })),
        render_result: None,
    };

    let tui: Arc<dyn TUI + Send + Sync> = Arc::new(TuiMainScreen::new(Arc::new(FakeTerminal), None, None));
    let comp = ToolExecutionComponent::new(
        "custom_tool",
        "tool-1",
        json!({}),
        None,
        Some(ToolRenderers::from_tool_definition(&tool_def)),
        tui,
        ".",
    );
    comp.render(80);
    assert_eq!(
        seen.lock()
            .unwrap_or_else(|e| e.into_inner())
            .last()
            .map(String::as_str),
        Some(EXPECTED)
    );
}

#[test]
#[serial]
fn message_renderers_receive_host_key_text_and_key_hint_in_render_options() {
    let _guard = setup_host_keybindings();
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen_c = Arc::clone(&seen);

    let message = CustomMessage {
        role: "custom".to_string(),
        custom_type: "test".to_string(),
        content: pi_coding_agent::core::messages::CustomMessageContent::Text("custom".to_string()),
        details: None,
        display: true,
        timestamp: 0,
    };
    let _comp = CustomMessageComponent::new(
        message,
        Some(Arc::new(move |_msg, opts, _theme| {
            let text = format!(
                "{} | {}",
                opts.key_text("app.tools.expand"),
                strip_ansi(&opts.key_hint("app.tools.expand", "to expand"))
            );
            seen_c.lock().unwrap_or_else(|e| e.into_inner()).push(text);
            Some(Arc::new(Text::new(Some("custom"), Some(0), Some(0), None)))
        })),
        None,
        None,
    );
    assert_eq!(&*seen.lock().unwrap_or_else(|e| e.into_inner()), &[EXPECTED]);
}

#[test]
#[serial]
fn entry_renderers_receive_host_key_text_and_key_hint_in_render_options() {
    let _guard = setup_host_keybindings();
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen_c = Arc::clone(&seen);

    let entry = CustomEntry {
        id: "entry-1".to_string(),
        parent_id: None,
        timestamp: "2026-01-01T00:00:00.000Z".to_string(),
        custom_type: "test".to_string(),
        data: None,
    };
    let _comp = CustomEntryComponent::new(
        entry,
        Arc::new(move |_entry, opts, _theme| {
            let text = format!(
                "{} | {}",
                opts.key_text("app.tools.expand"),
                strip_ansi(&opts.key_hint("app.tools.expand", "to expand"))
            );
            seen_c.lock().unwrap_or_else(|e| e.into_inner()).push(text);
            Some(Arc::new(Text::new(Some("custom"), Some(0), Some(0), None)))
        }),
    );
    assert_eq!(&*seen.lock().unwrap_or_else(|e| e.into_inner()), &[EXPECTED]);
}
