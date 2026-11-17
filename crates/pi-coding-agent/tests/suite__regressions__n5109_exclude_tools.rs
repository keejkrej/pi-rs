//! Port of packages/coding-agent/test/suite/regressions/5109-exclude-tools.test.ts
//!
//! WIRE(@ca-cli): Blocked on `tests/support/suite/harness.rs` and `AgentSession` landing.

#[test]
#[ignore = "WIRE(@ca-cli): waiting on suite::harness::create_harness and AgentSession"]
fn exclude_tools_pending_harness() {}

#[cfg(any())]
mod pending_ca_cli {
    // Full test specs to wire upon harness merge:
    // - filters built-in and extension tools from available and active tools
    // - lets excluded tools override the allowlist
    // - matches allowlist and denylist patterns
}
