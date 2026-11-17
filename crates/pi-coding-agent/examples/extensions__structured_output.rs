//! Port of packages/coding-agent/examples/extensions/structured-output.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_agent_core::types::AgentToolResult;
use pi_ai::types::{TextContent, UserContentBlock};
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, ToolDefinition,
};
use pi_js::vendor::typebox::t;
use pi_tui::Text;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct StructuredOutputDetails {
    headline: String,
    summary: String,
    action_items: Vec<String>,
}

pub fn structured_output_tool() -> ToolDefinition {
    ToolDefinition {
        name: "structured_output".to_string(),
        label: "Structured Output".to_string(),
        description: "Return a final structured answer. Use this as your last action when the user asks for structured output or a machine-readable summary.".to_string(),
        prompt_snippet: Some("Emit a final structured answer as a terminating tool result".to_string()),
        prompt_guidelines: Some(vec![
            "Use structured_output as your final action when the user asks for structured output, JSON-like output, or a machine-readable summary.".to_string(),
            "After calling structured_output, do not emit another assistant response in the same turn.".to_string(),
        ]),
        parameters: t::object([
            ("headline", t::string().with("description", "Short title for the result")),
            ("summary", t::string().with("description", "One-paragraph summary")),
            ("actionItems", t::array(t::string()).with("description", "Concrete next steps or key bullets")),
        ]),
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
        execute: Arc::new(|_tool_call_id, params, _signal, _on_update, _ctx| {
            Box::pin(async move {
                let parsed: StructuredOutputDetails = serde_json::from_value(params)
                    .map_err(|e| pi_js::Error::msg(format!("Invalid parameters: {e}")))?;
                let details_val = serde_json::to_value(&parsed).ok();
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new(format!(
                        "Saved structured output: {}",
                        parsed.headline
                    )))],
                    details: details_val,
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: Some(true),
                    control: None,
                })
            })
        }),
        render_call: None,
        render_result: Some(Arc::new(|result, _opts, theme, _context| {
            let details = result
                .details
                .clone()
                .and_then(|v| serde_json::from_value::<StructuredOutputDetails>(v).ok());
            let Some(details) = details else {
                let text = match result.content.first() {
                    Some(UserContentBlock::Text(t)) => t.text.as_str(),
                    _ => "",
                };
                return Arc::new(Text::new(Some(text), Some(0), Some(0), None));
            };

            let mut lines = vec![
                theme.fg("toolTitle", &theme.bold(&details.headline)),
                theme.fg("text", &details.summary),
                String::new(),
            ];
            for (index, item) in details.action_items.iter().enumerate() {
                lines.push(theme.fg("muted", &format!("{}. {item}", index + 1)));
            }
            Arc::new(Text::new(Some(&lines.join("\n")), Some(0), Some(0), None))
        })),
    }
}

pub struct StructuredOutputExtension;

#[async_trait]
impl Extension for StructuredOutputExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        pi.register_tool(structured_output_tool())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(StructuredOutputExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
