//! Port of packages/coding-agent/test/suite/regressions/startup-session-rebind-duplicate-subscription.test.ts

#[path = "support/mod.rs"]
mod support;

// WIRE(@tui-w3): InteractiveMode lives in @tui's `modes::interactive`. Re-enable once
// Stage 3B-2 lands `InteractiveMode::rebind_current_session`.
#[tokio::test]
#[ignore = "WIRE(@tui-w3): waiting for InteractiveMode::rebind_current_session in modes::interactive"]
async fn does_not_duplicate_session_subscriptions_across_startup_session_rebind_and_new_session() {
    // Ported assertions from startup-session-rebind-duplicate-subscription.test.ts:
    // 1. Construct InteractiveMode with a session runtime whose initial session has an
    //    extension subscription, rebind the current session via startup rebind / newSession(),
    //    and verify events/tool updates are emitted only once per session event.
}
