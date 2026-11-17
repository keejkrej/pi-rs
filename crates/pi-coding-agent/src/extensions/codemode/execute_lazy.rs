//! Port of packages/coding-agent/src/extensions/codemode/execute.lazy.ts

use pi_agent_core::AgentToolResult;
use pi_js::Result;
use pi_js::abort::AbortSignal;

use crate::core::extensions::types::ExtensionToolContext;
use crate::extensions::codemode::tool::{CodemodeToolInput, CodemodeToolOptions};

pub type CodemodeUpdateFn = Option<pi_agent_core::AgentToolUpdateCallback>;

pub async fn execute_codemode_lazy(
    tool_call_id: String,
    input: CodemodeToolInput,
    signal: Option<AbortSignal>,
    on_update: CodemodeUpdateFn,
    ctx: Option<ExtensionToolContext>,
    options: CodemodeToolOptions,
) -> Result<AgentToolResult> {
    crate::extensions::codemode::execute::execute_codemode(tool_call_id, input, signal, on_update, ctx, options).await
}
