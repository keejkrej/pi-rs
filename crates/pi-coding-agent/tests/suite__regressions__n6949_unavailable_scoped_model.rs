//! Port of packages/coding-agent/test/suite/regressions/6949-unavailable-scoped-model.test.ts

#[path = "support/mod.rs"]
mod support;

// WIRE(@tui-w3): waiting for ScopedModelsSelectorComponent and InteractiveMode::show_models_selector in modes::interactive.

#[ignore = "WIRE(@tui-w3): waiting for ScopedModelsSelectorComponent in modes::interactive"]
#[tokio::test]
async fn shows_and_removes_an_enabled_model_without_a_catalog_entry() {}

#[ignore = "WIRE(@tui-w3): waiting for InteractiveMode::show_models_selector in modes::interactive"]
#[tokio::test]
async fn passes_unmatched_settings_patterns_to_the_selector_with_one_combined_resolution() {}

#[ignore = "WIRE(@tui-w3): waiting for InteractiveMode::show_models_selector in modes::interactive"]
#[tokio::test]
async fn opens_when_only_a_session_scoped_model_is_unavailable() {}

#[ignore = "WIRE(@tui-w3): waiting for InteractiveMode::show_models_selector in modes::interactive"]
#[tokio::test]
async fn does_not_clear_a_partial_scope_when_an_enabled_model_is_unavailable() {}
