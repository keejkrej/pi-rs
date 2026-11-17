//! Port of packages/coding-agent/test/suite/regressions/7150-rpc-prompt-during-compaction.test.ts

// #7150

// BLOCKED on Stage 3A core: requires `core::agent_session::AgentSession` (`prompt` with
// `PromptOptions { source: "rpc", preflight_result }` and `compact()`), `core::session_manager::SessionManager`,
// `core::settings_manager::SettingsManager`, `core::extensions`, and `tests/support/suite/harness.rs`,
// which are all still 1-line stubs on `swarm/tui-w3`.
mod issue_7150_rpc_prompt_during_manual_compaction {
    #[test]
    #[ignore = "BLOCKED on Stage 3A core: AgentSession, SessionManager, SettingsManager, ExtensionAPI, and tests/support/suite/harness.rs not merged yet"]
    fn rejects_an_rpc_prompt_while_manual_compaction_is_in_progress() {}
}
