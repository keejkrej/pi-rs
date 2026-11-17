//! Port of packages/coding-agent/test/suite/regressions/3217-scoped-model-order.test.ts

#[path = "support/mod.rs"]
mod support;

// WIRE(@tui-w3): waiting for ModelSelectorComponent, ScopedModelsSelectorComponent, and init_theme in modes::interactive.

#[ignore = "WIRE(@tui-w3): waiting for ScopedModelsSelectorComponent in modes::interactive"]
#[tokio::test]
async fn propagates_reordered_scoped_models_back_to_the_session_state() {}

#[ignore = "WIRE(@tui-w3): waiting for ModelSelectorComponent in modes::interactive"]
#[tokio::test]
async fn preserves_scoped_model_order_in_the_model_scoped_tab() {}
