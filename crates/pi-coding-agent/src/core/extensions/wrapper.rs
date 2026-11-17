//! Port of packages/coding-agent/src/core/extensions/wrapper.ts

use std::sync::Arc;

use pi_agent_core::types::AgentTool;

use crate::core::tools::tool_definition_wrapper::wrap_tool_definition;

use super::runner::ExtensionRunner;
use super::types::RegisteredTool;

/// Wrap a RegisteredTool into an AgentTool.
/// Uses the runner's `create_tool_context()` for consistent context across tools and event handlers.
pub fn wrap_registered_tool(registered_tool: &RegisteredTool, runner: &ExtensionRunner) -> AgentTool {
    let runner = runner.clone();
    wrap_tool_definition(
        registered_tool.definition.clone(),
        Some(Arc::new(move |tool_call_id, signal| {
            runner.create_tool_context(tool_call_id, signal)
        })),
    )
}

/// Wrap all registered tools into AgentTools.
/// Uses the runner's `create_tool_context()` for consistent context across tools and event handlers.
pub fn wrap_registered_tools(registered_tools: &[RegisteredTool], runner: &ExtensionRunner) -> Vec<AgentTool> {
    registered_tools
        .iter()
        .map(|tool| wrap_registered_tool(tool, runner))
        .collect()
}
