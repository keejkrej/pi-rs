//! Port of packages/coding-agent/src/experimental/vacation/tui.ts

use crate::core::settings_manager::SettingsManager;
use crate::experimental::vacation::runtime::{DurableController, DurableViewSource};

/// Run the vacation planner interactive TUI over one view source and controller.
pub async fn run_durable_tui(
    source: &DurableViewSource,
    controller: &DurableController,
    settings: &SettingsManager,
) -> pi_js::Result<()> {
    // WIRE(@tui-w3): shares modes::interactive wiring with durable::tui::run_durable_tui.
    crate::experimental::durable::tui::run_durable_tui(source, controller, settings).await
}
