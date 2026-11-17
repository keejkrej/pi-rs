//! Port of packages/coding-agent/test/suite/regressions/6596-taskkill-enoent.test.ts
//!
//! WIRE(@mcp-w3): Blocked on `utils::shell::kill_process_tree` child_process spawn test seam.

#[test]
#[ignore = "WIRE(@mcp-w3): waiting on utils::shell::kill_process_tree spawn test seam"]
fn taskkill_enoent_pending_mcp_w3() {}

#[cfg(any())]
mod pending_mcp_w3 {
    // Full test specs to wire upon @mcp-w3 merge:
    // - uses SystemRoot\System32\taskkill.exe (fallback C:\Windows) and consumes spawn ENOENT errors (#6596, #9490)
}
