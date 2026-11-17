//! Port of packages/coding-agent/test/tool-renderer-examples.test.ts
//!
//! WIRE(@w3-b / @ca-cli): Blocked on `AgentSession`, `DefaultResourceLoader`, `ToolExecutionComponent`,
//! and `init_theme` landing before integration tests can execute.

#[test]
#[ignore = "WIRE(@w3-b / @ca-cli): waiting on AgentSession, DefaultResourceLoader, ToolExecutionComponent"]
fn tool_renderer_examples_pending_stage_3b_and_ca_cli() {}

#[cfg(any())]
mod pending_stage_3b_and_ca_cli {
    // Full test specs to wire upon merge:
    // - keeps the system prompt unchanged for built-in tool renderer and minimal mode examples (#10072)
    // - keeps minimal mode's edit tool in the default shell (#10072)
}
