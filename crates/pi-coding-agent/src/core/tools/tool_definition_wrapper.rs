//! Port of packages/coding-agent/src/core/tools/tool-definition-wrapper.ts

use std::sync::Arc;

use pi_agent_core::types::{AgentTool, AgentToolResult};
use pi_js::abort::AbortSignal;
use serde_json::Value;

use crate::core::extensions::types::{ExtensionToolContext, ToolDefinition};

/// Creates the context for one tool call.
pub type ToolContextFactory = Arc<dyn Fn(&str, Option<AbortSignal>) -> ExtensionToolContext + Send + Sync>;

/// Wrap a ToolDefinition into an AgentTool for the core runtime.
pub fn wrap_tool_definition(definition: ToolDefinition, ctx_factory: Option<ToolContextFactory>) -> AgentTool {
    let def = definition.clone();
    let name_for_err = def.name.clone();
    AgentTool {
        name: definition.name,
        label: definition.label,
        description: definition.description,
        parameters: definition.parameters,
        output_schema: definition.output_schema.map(|s| s.to_value()),
        constrained_sampling: match definition.constrained_sampling {
            Some(crate::core::extensions::types::ConstrainedSamplingOption::Config(cfg)) => Some(cfg),
            _ => None,
        },
        prepare_arguments: definition.prepare_arguments,
        execution_mode: definition.execution_mode,
        replay: None,
        execute: Arc::new(move |tool_call_id, params, signal, on_update| {
            let execute_fn = Arc::clone(&def.execute);
            let ctx_factory = ctx_factory.clone();
            let name_for_err = name_for_err.clone();
            Box::pin(async move {
                let ctx = ctx_factory.map(|f| f(&tool_call_id, signal.clone())).ok_or_else(|| {
                    pi_js::Error::msg(format!(
                        "Tool \"{name_for_err}\" requires ExtensionToolContext when invoked outside session wrapper",
                    ))
                })?;
                let params_val: Value = params;
                execute_fn(tool_call_id, params_val, signal, on_update, ctx).await
            })
        }),
    }
}

/// Wrap multiple ToolDefinitions into AgentTools for the core runtime.
pub fn wrap_tool_definitions(
    definitions: Vec<ToolDefinition>,
    ctx_factory: Option<ToolContextFactory>,
) -> Vec<AgentTool> {
    definitions
        .into_iter()
        .map(|def| wrap_tool_definition(def, ctx_factory.clone()))
        .collect()
}

/// Synthesize a minimal ToolDefinition from an AgentTool.
pub fn create_tool_definition_from_agent_tool(tool: AgentTool) -> ToolDefinition {
    let exec = Arc::clone(&tool.execute);
    ToolDefinition {
        name: tool.name,
        label: tool.label,
        description: tool.description,
        prompt_snippet: None,
        prompt_guidelines: None,
        parameters: tool.parameters,
        constrained_sampling: tool
            .constrained_sampling
            .map(crate::core::extensions::types::ConstrainedSamplingOption::Config),
        render_shell: None,
        prepare_arguments: tool.prepare_arguments,
        output_schema: tool.output_schema.map(pi_js::vendor::typebox::t::unsafe_),
        exposure: None,
        namespace: None,
        annotations: None,
        default_active: None,
        prepare_loadout: None,
        execution_mode: tool.execution_mode,
        execute: Arc::new(move |tool_call_id, params, signal, on_update, _ctx| {
            let exec = Arc::clone(&exec);
            Box::pin(async move {
                let res: AgentToolResult = exec(tool_call_id, params, signal, on_update).await?;
                Ok(res)
            })
        }),
        render_call: None,
        render_result: None,
    }
}
