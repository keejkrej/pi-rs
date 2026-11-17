//! Port of `packages/coding-agent/test/export-html-whitespace.test.ts`.
// WIRE(@tui-w3): Theme reference uses `HtmlTheme` adapter until TUI theme merges.

use std::path::PathBuf;
use std::sync::Arc;

use pi_coding_agent::core::export_html::ansi_to_html::ansi_lines_to_html;
use pi_coding_agent::core::export_html::tool_renderer::{
    CustomToolRenderer, HtmlTheme, ToolHtmlRendererDeps, create_tool_html_renderer,
};
use pi_coding_agent::core::extensions::types::{ConstrainedSamplingOption, ToolDefinition, define_tool};
use pi_js::vendor::typebox::t;
use pi_tui::Component;
use regex::Regex;

struct StaticComponent(Vec<String>);

impl Component for StaticComponent {
    fn render(&self, _width: usize) -> Vec<String> {
        self.0.clone()
    }

    fn invalidate(&self) {}
}

fn custom_tool_definition() -> ToolDefinition {
    define_tool(ToolDefinition {
        name: "custom".to_owned(),
        label: "Custom".to_owned(),
        description: "Custom tool".to_owned(),
        prompt_snippet: None,
        prompt_guidelines: None,
        parameters: t::object::<[(&str, pi_js::vendor::typebox::Schema); 0], &str>([]),
        constrained_sampling: Some(ConstrainedSamplingOption::Disabled(true)),
        render_shell: None,
        prepare_arguments: None,
        output_schema: None,
        exposure: None,
        namespace: None,
        annotations: None,
        default_active: None,
        prepare_loadout: None,
        execution_mode: None,
        execute: Arc::new(|_, _, _, _, _| Box::pin(async { Ok(pi_agent_core::AgentToolResult::text("")) })),
        render_call: None,
        render_result: Some(Arc::new(|_, _, _, _| {
            Arc::new(StaticComponent(vec![
                String::new(),
                "\u{001b}[31mone\u{001b}[0m".to_owned(),
                "two".to_owned(),
                "\u{001b}[0m\u{001b}]8;;\u{0007}".to_owned(),
            ]))
        })),
    })
}

#[test]
fn preserves_whitespace_for_plain_text_tool_output_without_template_whitespace() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let css = std::fs::read_to_string(manifest_dir.join("src/core/export_html/template.css")).expect("read css");

    let preview_re =
        Regex::new(r"\.output-preview > div:not\(\.expand-hint\),\s*\.output-full > div:not\(\.expand-hint\) \{[\s\S]*?white-space:\s*pre-wrap;")
            .unwrap();
    let ansi_re = Regex::new(r"\.ansi-line\s*\{[\s\S]*?white-space:\s*pre;").unwrap();
    let forbidden_re = Regex::new(r"\.output-preview,\s*\.output-full\s*\{[\s\S]*?white-space:\s*pre-wrap;").unwrap();

    assert!(preview_re.is_match(&css));
    assert!(ansi_re.is_match(&css));
    assert!(!forbidden_re.is_match(&css));
}

#[test]
fn does_not_insert_source_whitespace_between_ansi_rendered_lines() {
    assert_eq!(
        ansi_lines_to_html(&["one", "two"]),
        "<div class=\"ansi-line\">one</div><div class=\"ansi-line\">two</div>"
    );
}

#[test]
fn trims_tui_spacing_lines_from_custom_tool_result_html() {
    let renderer = create_tool_html_renderer(ToolHtmlRendererDeps {
        get_tool_definition: Arc::new(|name| {
            if name == "custom" {
                Some(Arc::new(custom_tool_definition()) as Arc<dyn CustomToolRenderer>)
            } else {
                None
            }
        }),
        theme: HtmlTheme::default(),
        cwd: "/tmp".to_owned(),
        width: None,
    });

    let rendered = renderer.render_result("id", "custom", &[], None, false).unwrap();
    assert_eq!(
        rendered.expanded.as_deref(),
        Some(
            "<div class=\"ansi-line\"><span style=\"color:#800000\">one</span></div><div class=\"ansi-line\">two</div>"
        )
    );
}
