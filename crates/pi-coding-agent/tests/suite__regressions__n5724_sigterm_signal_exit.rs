//! Port of packages/coding-agent/test/suite/regressions/5724-sigterm-signal-exit.test.ts

#[path = "support/mod.rs"]
mod support;

// WIRE(@tui-w3): waiting for InteractiveMode::shutdown in modes::interactive.

#[ignore = "WIRE(@tui-w3): waiting for InteractiveMode::shutdown in modes::interactive"]
#[tokio::test]
async fn keeps_signal_handlers_registered_while_signal_triggered_cleanup_is_pending() {}
