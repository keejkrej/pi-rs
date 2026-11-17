//! Port of packages/coding-agent/examples/extensions/hello.ts

use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use pi_agent_core::types::AgentToolResult;
use pi_ai::types::{TextContent, UserContentBlock};
use pi_coding_agent::core::extensions::types::{
    Extension, ExtensionApi, ExtensionFactory, InlineExtension, ToolDefinition,
};
use pi_js::vendor::typebox::t;

pub fn hello_tool() -> ToolDefinition {
    ToolDefinition {
        name: "hello".to_string(),
        label: "Hello".to_string(),
        description: "A simple greeting tool".to_string(),
        prompt_snippet: None,
        prompt_guidelines: None,
        parameters: t::object([("name", t::string().with("description", "Name to greet"))]),
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
                let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent::new(format!("Hello, {name}!")))],
                    details: Some(serde_json::json!({ "greeted": name })),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                    control: None,
                })
            })
        }),
        render_call: None,
        render_result: None,
    }
}

pub struct HelloExtension;

#[async_trait]
impl Extension for HelloExtension {
    async fn register(&self, pi: ExtensionApi) -> pi_js::Result<()> {
        pi.register_tool(hello_tool())
    }
}

pub fn create_extension() -> ExtensionFactory {
    Arc::new(HelloExtension)
}

fn main() -> ExitCode {
    pi_coding_agent::cli::run_with(vec![InlineExtension::Factory(create_extension())])
}
