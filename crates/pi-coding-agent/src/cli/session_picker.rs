//! Port of packages/coding-agent/src/cli/session-picker.ts

//! TUI session selector for --resume flag

use std::sync::Arc;

use pi_js::BoxFuture;
use pi_js::abort::AbortSignal;

use crate::core::session_manager::{SessionInfo, SessionListProgress};
use crate::core::settings_manager::SettingsManager;

pub type SessionsLoader = Arc<
    dyn Fn(Option<SessionListProgress>, Option<AbortSignal>) -> BoxFuture<pi_js::Result<Vec<SessionInfo>>>
        + Send
        + Sync,
>;

/// Show TUI session selector and return selected session path or None if cancelled
pub async fn select_session(
    _current_sessions_loader: SessionsLoader,
    _all_sessions_loader: SessionsLoader,
    _settings_manager: &SettingsManager,
) -> pi_js::Result<Option<String>> {
    // WIRE(@tui-w3): wire SessionSelectorComponent and create_startup_tui once modes/interactive lands in Stage 3B-2
    Err(pi_js::Error::msg(
        "Session picker TUI requires interactive mode (Stage 3B-2)",
    ))
}
