//! Port of packages/coding-agent/src/experimental/client-tui-chat.ts

use pi_durable::{ConversationView, LiveState};
use pi_tui::Container;

/// The `pi.live` document of a view: the active run, the streaming answer, and running tools.
pub fn live_of(view: &ConversationView) -> LiveState {
    view.docs
        .get("pi.live")
        .and_then(|val| serde_json::from_value::<LiveState>(serde_json::Value::Object(val.clone())).ok())
        .unwrap_or_default()
}

/// Renders the root conversation's durable view for the service-only experimental presentation.
pub struct ExperimentalChatView {
    pub transcript: Container,
    pub pending_messages: Container,
    pub status: Container,
}

impl ExperimentalChatView {
    pub fn new(_cwd: impl Into<String>) -> pi_js::Result<Self> {
        // WIRE(@tui-w3): connect modes::interactive::{components::{AssistantMessageComponent,
        // StatusIndicator, ToolExecutionComponent, UserMessageComponent}, theme::theme} and
        // core::tools::renderers once @tui lands src/modes/interactive/**.
        Err(pi_js::Error::msg(
            "ExperimentalChatView requires modes::interactive widgets from @tui (@tui-w3)",
        ))
    }

    pub fn apply(&mut self, _view: &ConversationView) {}

    pub fn refresh_theme(&mut self, _view: &ConversationView) {}

    pub fn dispose(&mut self) {}
}
