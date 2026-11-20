//! Port of packages/coding-agent/test/output-pad.test.ts

use std::sync::Arc;

use async_trait::async_trait;
use pi_ai::{TextContent, UserContentBlock};
use pi_coding_agent::core::extensions::types::ToolDefinition;
use pi_coding_agent::core::messages::CompactionSummaryMessage;
use pi_coding_agent::core::tools::edit::create_edit_tool_definition;
use pi_coding_agent::modes::interactive::components::bash_execution::BashExecutionComponent;
use pi_coding_agent::modes::interactive::components::compaction_summary_message::CompactionSummaryMessageComponent;
use pi_coding_agent::modes::interactive::components::tool_execution::{
    ToolExecutionComponent, ToolExecutionOptions, ToolExecutionResult, ToolRenderers,
};
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_coding_agent::utils::ansi::strip_ansi;
use pi_schema::unknown;
use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
use pi_tui::{TUI, TuiMainScreen};
use serial_test::serial;

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

enum PadTarget {
    Bash(BashExecutionComponent),
    Tool(ToolExecutionComponent),
    Compaction(CompactionSummaryMessageComponent),
}

impl PadTarget {
    fn render(&self, width: usize) -> Vec<String> {
        match self {
            Self::Bash(c) => c.render(width),
            Self::Tool(c) => c.render(width),
            Self::Compaction(c) => c.render(width),
        }
    }

    fn set_output_pad(&self, pad: i64) {
        match self {
            Self::Bash(c) => c.set_output_pad(pad),
            Self::Tool(c) => c.set_output_pad(pad),
            Self::Compaction(c) => c.set_output_pad(pad),
        }
    }
}

fn render_lines(component: &PadTarget) -> Vec<String> {
    component
        .render(60)
        .into_iter()
        .map(|line| strip_ansi(&line).trim_end().to_owned())
        .filter(|line| {
            line.chars()
                .any(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$' || ch == '(')
        })
        .collect()
}

fn create_tool(definition: Option<&ToolDefinition>, output_pad: i64, ui: Arc<dyn TUI + Send + Sync>) -> PadTarget {
    let renderers = definition.map(ToolRenderers::from_tool_definition);
    let component = ToolExecutionComponent::new(
        "custom_tool",
        "id",
        serde_json::json!({}),
        Some(ToolExecutionOptions {
            output_pad: Some(output_pad),
            ..Default::default()
        }),
        renderers,
        ui,
        "/",
    );
    component.update_result(
        ToolExecutionResult {
            content: vec![UserContentBlock::Text(TextContent::new("ok"))],
            details: None,
            is_error: false,
            duration_ms: None,
        },
        None,
    );
    PadTarget::Tool(component)
}

#[tokio::test]
#[serial]
async fn output_pad_renders_components_at_pad_0_and_1() {
    init_theme(Some("dark"), Some(false));
    let ui: Arc<dyn TUI + Send + Sync> = Arc::new(TuiMainScreen::new(Arc::new(FakeTerminal), None, None));

    let custom_def = ToolDefinition {
        name: "custom_tool".to_owned(),
        label: "custom_tool".to_owned(),
        description: "custom tool".to_owned(),
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
                    content: vec![UserContentBlock::Text(TextContent::new("ok"))],
                    ..Default::default()
                })
            })
        }),
        render_call: None,
        render_result: None,
    };

    let bash = {
        let comp = BashExecutionComponent::new("pwd", Arc::clone(&ui), Some(false), Some(0));
        comp.append_output("/tmp");
        comp.set_complete(Some(1), false, None, None);
        PadTarget::Bash(comp)
    };

    let custom = create_tool(Some(&custom_def), 0, Arc::clone(&ui));
    let without_def = create_tool(None, 0, Arc::clone(&ui));

    let self_rendered_edit = {
        let edit_def = create_edit_tool_definition("/", None);
        let comp = ToolExecutionComponent::new(
            "edit",
            "id",
            serde_json::json!({ "path": "file.txt", "edits": [{ "oldText": "old", "newText": "new" }] }),
            Some(ToolExecutionOptions {
                output_pad: Some(0),
                ..Default::default()
            }),
            Some(ToolRenderers::from_tool_definition(&edit_def)),
            Arc::clone(&ui),
            "/",
        );
        comp.update_result(
            ToolExecutionResult {
                content: vec![UserContentBlock::Text(TextContent::new("Could not find old text"))],
                details: None,
                is_error: true,
                duration_ms: None,
            },
            None,
        );
        PadTarget::Tool(comp)
    };

    let compaction = PadTarget::Compaction(CompactionSummaryMessageComponent::new_with_options(
        CompactionSummaryMessage {
            role: "compactionSummary".to_owned(),
            summary: "summary".to_owned(),
            tokens_before: 10,
            timestamp: 0,
        },
        None,
        Some(0),
        None,
        None,
        None,
    ));

    let cases = [
        ("bash execution", bash),
        ("tool execution", custom),
        ("tool execution without a definition", without_def),
        ("self-rendered edit result", self_rendered_edit),
        ("compaction summary", compaction),
    ];

    for (name, component) in cases {
        let lines = render_lines(&component);
        let leading_spaces: Vec<&String> = lines.iter().filter(|l| l.starts_with(' ')).collect();
        assert!(
            leading_spaces.is_empty(),
            "{name} expected no leading spaces at pad=0, got {leading_spaces:?}"
        );
        component.set_output_pad(1);
        let padded: Vec<String> = lines.iter().map(|line| format!(" {line}")).collect();
        assert_eq!(render_lines(&component), padded, "{name} at pad=1");
    }
}
