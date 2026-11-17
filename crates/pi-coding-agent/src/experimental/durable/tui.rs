//! Port of packages/coding-agent/src/experimental/durable/tui.ts

use crate::core::settings_manager::SettingsManager;
use crate::experimental::durable::runtime::{DurableController, DurableViewSource};

/// Run the durable harness interactive TUI over one view source and controller.
pub async fn run_durable_tui(
    _source: &DurableViewSource,
    _controller: &DurableController,
    _settings: &SettingsManager,
) -> pi_js::Result<()> {
    // WIRE(@tui-w3): connect modes::interactive::{components::*, theme::*, tui_renderer::*}
    // (AssistantMessageComponent, CustomEditor, DynamicBorder, format_tokens, key_text,
    //  WorkingStatusIndicator, ToolExecutionComponent, UserMessageComponent, InteractiveThemeController)
    // once @tui lands src/modes/interactive/**.
    Err(pi_js::Error::msg(
        "Durable TUI requires modes::interactive widgets from @tui (@tui-w3)",
    ))
}
