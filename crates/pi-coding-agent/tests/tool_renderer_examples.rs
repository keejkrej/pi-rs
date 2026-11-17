//! Port of packages/coding-agent/test/tool-renderer-examples.test.ts
//!
//! WIRE(@tui-w3): Blocked on `ToolExecutionComponent` and `init_theme` in `modes::interactive`
//! landing before TUI integration tests can execute.

#[test]
#[ignore = "WIRE(@tui-w3): waiting on ToolExecutionComponent and init_theme in modes::interactive"]
fn tool_renderer_examples_pending_tui() {}

#[cfg(any())]
mod pending_tui {
    // Full test specs to wire upon merge:
    // - keeps the system prompt unchanged for built-in tool renderer and minimal mode examples (#10072)
    // - keeps minimal mode's edit tool in the default shell (#10072)
}
