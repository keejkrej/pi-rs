//! Port of packages/coding-agent/src/experimental/client-tui-chat.ts

use std::sync::Arc;

use async_trait::async_trait;
use indexmap::{IndexMap, IndexSet};
use pi_ai::{AssistantContentBlock, Message, ToolResultMessage, UserContentBlock, UserMessageContent};
use pi_durable::entries::{ASSISTANT_ENTRY, COMPACTION_ENTRY, RESET_ENTRY, TOOL_RESULT_ENTRY, USER_ENTRY};
use pi_durable::harness::inbox::InboxItem;
use pi_durable::harness::live::ToolSlotStatus;
use pi_durable::{ConversationView, InboxState, LiveState, ToolSlot};
use pi_tui::terminal::{Terminal, TerminalInputHandler, TerminalResizeHandler};
use pi_tui::{Component, Container, TUI, Text, TuiMainScreen, TuiMouseEvent, TuiMouseEventResult};
use serde_json::Value;

use crate::core::messages::CompactionSummaryMessage;
use crate::core::tools::bash::create_bash_tool_definition;
use crate::core::tools::edit::create_edit_tool_definition;
use crate::core::tools::find::create_find_tool_definition;
use crate::core::tools::grep::create_grep_tool_definition;
use crate::core::tools::ls::create_ls_tool_definition;
use crate::core::tools::powershell::create_power_shell_tool_definition;
use crate::core::tools::read::create_read_tool_definition;
use crate::core::tools::write::create_write_tool_definition;
use crate::modes::interactive::components::assistant_message::AssistantMessageComponent;
use crate::modes::interactive::components::compaction_summary_message::CompactionSummaryMessageComponent;
use crate::modes::interactive::components::dynamic_border::DynamicBorder;
use crate::modes::interactive::components::status_indicator::{CompactionStatusReason, StatusIndicator};
use crate::modes::interactive::components::tool_execution::{
    ToolExecutionComponent, ToolExecutionOptions, ToolExecutionResult, ToolRenderers,
};
use crate::modes::interactive::components::user_message::UserMessageComponent;
use crate::modes::interactive::theme::theme::{ThemeColor, theme};

/// The `pi.live` document of a view: the active run, the streaming answer, and running tools.
pub fn live_of(view: &ConversationView) -> LiveState {
    view.docs
        .get("pi.live")
        .and_then(|val| serde_json::from_value::<LiveState>(Value::Object(val.clone())).ok())
        .unwrap_or_default()
}

fn inbox_of(view: &ConversationView) -> InboxState {
    view.docs
        .get("pi.inbox")
        .and_then(|val| serde_json::from_value::<InboxState>(Value::Object(val.clone())).ok())
        .unwrap_or_default()
}

struct HeadlessTerminal;

#[async_trait]
impl Terminal for HeadlessTerminal {
    fn start(&self, _on_input: TerminalInputHandler, _on_resize: TerminalResizeHandler) {}
    fn stop(&self) {}
    async fn drain_input(&self, _max_ms: Option<i64>, _idle_ms: Option<i64>) {}
    fn write(&self, _data: &str) {}
    fn columns(&self) -> i64 {
        80
    }
    fn rows(&self) -> i64 {
        24
    }
    fn kitty_protocol_active(&self) -> bool {
        false
    }
    fn move_by(&self, _lines: i64) {}
    fn hide_cursor(&self) {}
    fn show_cursor(&self) {}
    fn clear_line(&self) {}
    fn clear_from_cursor(&self) {}
    fn clear_screen(&self) {}
    fn set_title(&self, _title: &str) {}
    fn set_progress(&self, _active: bool) {}
}

fn builtin_tool_renderers(cwd: &str) -> IndexMap<String, ToolRenderers> {
    let mut renderers = IndexMap::new();
    let defs = [
        create_read_tool_definition(cwd, None),
        create_bash_tool_definition(cwd, None),
        create_edit_tool_definition(cwd, None),
        create_write_tool_definition(cwd, None),
        create_grep_tool_definition(cwd, None),
        create_find_tool_definition(cwd, None),
        create_ls_tool_definition(cwd, None),
        create_power_shell_tool_definition(cwd, None),
    ];
    for def in defs {
        renderers.insert(def.name.clone(), ToolRenderers::from_tool_definition(&def));
    }
    renderers
}

fn user_message_text(content: &UserMessageContent) -> String {
    match content {
        UserMessageContent::Text(s) => s.clone(),
        UserMessageContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|b| match b {
                UserContentBlock::Text(tc) => Some(tc.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn live_tool_result(slot: &ToolSlot) -> ToolExecutionResult {
    let content = slot
        .output
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| vec![UserContentBlock::Text(pi_ai::TextContent::new(s))])
        .unwrap_or_default();
    ToolExecutionResult {
        content,
        details: slot.details.clone(),
        is_error: false,
        duration_ms: None,
    }
}

/// Renders the root conversation's durable view for the service-only experimental presentation.
pub struct ExperimentalChatView {
    pub transcript: Container,
    pub pending_messages: Container,
    pub status: Container,
    cwd: String,
    ui: Arc<dyn TUI + Send + Sync>,
    tool_renderers: IndexMap<String, ToolRenderers>,
    active_status: Option<StatusIndicator>,
}

impl ExperimentalChatView {
    pub fn new(cwd: impl Into<String>) -> pi_js::Result<Self> {
        Self::new_with_ui(cwd, None)
    }

    pub fn new_with_ui(cwd: impl Into<String>, ui: Option<Arc<dyn TUI + Send + Sync>>) -> pi_js::Result<Self> {
        let cwd_str = cwd.into();
        let tui: Arc<dyn TUI + Send + Sync> =
            ui.unwrap_or_else(|| Arc::new(TuiMainScreen::new(Arc::new(HeadlessTerminal), Some(false), None)));
        let tool_renderers = builtin_tool_renderers(&cwd_str);
        Ok(Self {
            transcript: Container::new(),
            pending_messages: Container::new(),
            status: Container::new(),
            cwd: cwd_str,
            ui: tui,
            tool_renderers,
            active_status: None,
        })
    }

    pub fn apply(&mut self, view: &ConversationView) {
        let live = live_of(view);
        self.transcript.clear();

        // Start from the most recent reset boundary so previous cleared turns stay out of active view.
        let start_idx = view
            .entries
            .iter()
            .rposition(|e| RESET_ENTRY.matches(Some(e)))
            .unwrap_or(0);
        let active_entries = &view.entries[start_idx..];

        // Index committed tool results by call ID so each tool call component pairs cleanly with its result.
        let mut results_by_call_id: IndexMap<String, ToolResultMessage> = IndexMap::new();
        for entry in active_entries {
            if TOOL_RESULT_ENTRY.matches(Some(entry))
                && let Some(models) = &entry.model
            {
                for msg in models {
                    if let Message::ToolResult(tr) = msg {
                        results_by_call_id.insert(tr.tool_call_id.clone(), tr.clone());
                    }
                }
            }
        }

        let live_slots_by_id: IndexMap<&str, &ToolSlot> = live
            .tools
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(|slot| (slot.call_id.as_str(), slot))
            .collect();

        let mut paired_call_ids: IndexSet<String> = IndexSet::new();

        for entry in active_entries {
            if RESET_ENTRY.matches(Some(entry)) {
                self.transcript.add_child(Arc::new(DynamicBorder::new(None)));
                if let Some(models) = &entry.model {
                    for msg in models {
                        if let Message::User(user) = msg {
                            let text = user_message_text(&user.content);
                            if !text.is_empty() {
                                self.transcript
                                    .add_child(Arc::new(UserMessageComponent::new(&text, None, None, None)));
                            }
                        }
                    }
                }
                continue;
            }

            if USER_ENTRY.matches(Some(entry)) {
                if let Some(models) = &entry.model {
                    for msg in models {
                        if let Message::User(user) = msg {
                            let text = user_message_text(&user.content);
                            self.transcript
                                .add_child(Arc::new(UserMessageComponent::new(&text, None, None, None)));
                        }
                    }
                }
                continue;
            }

            if COMPACTION_ENTRY.matches(Some(entry)) {
                let summary = entry
                    .model
                    .as_ref()
                    .and_then(|models| {
                        models.iter().find_map(|m| match m {
                            Message::User(u) => Some(user_message_text(&u.content)),
                            _ => None,
                        })
                    })
                    .unwrap_or_else(|| "Conversation compacted".to_string());
                let msg = CompactionSummaryMessage {
                    role: "compactionSummary".to_string(),
                    summary,
                    tokens_before: 0,
                    timestamp: 0,
                };
                self.transcript
                    .add_child(Arc::new(CompactionSummaryMessageComponent::new(msg, None)));
                continue;
            }

            if ASSISTANT_ENTRY.matches(Some(entry)) {
                if let Some(models) = &entry.model {
                    for msg in models {
                        if let Message::Assistant(assistant) = msg {
                            self.transcript.add_child(Arc::new(AssistantMessageComponent::new(
                                Some(assistant.clone()),
                                None,
                                None,
                                None,
                                None,
                                None,
                            )));

                            for block in &assistant.content {
                                if let AssistantContentBlock::ToolCall(tc) = block {
                                    paired_call_ids.insert(tc.id.clone());
                                    let renderer = self.tool_renderers.get(&tc.name).cloned();
                                    let args_map: serde_json::Map<String, Value> =
                                        tc.arguments.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                                    let comp = ToolExecutionComponent::new(
                                        &tc.name,
                                        &tc.id,
                                        Value::Object(args_map),
                                        Some(ToolExecutionOptions::default()),
                                        renderer,
                                        Arc::clone(&self.ui),
                                        &self.cwd,
                                    );
                                    if let Some(tr) = results_by_call_id.get(&tc.id) {
                                        comp.set_args_complete();
                                        comp.update_result(
                                            ToolExecutionResult {
                                                content: tr.content.clone(),
                                                details: tr.details.clone(),
                                                is_error: tr.is_error,
                                                duration_ms: None,
                                            },
                                            Some(false),
                                        );
                                    } else if let Some(slot) = live_slots_by_id.get(tc.id.as_str()) {
                                        comp.set_args_complete();
                                        match slot.status {
                                            ToolSlotStatus::Pending => {}
                                            ToolSlotStatus::Running => {
                                                comp.mark_execution_started();
                                                if slot.output.is_some() || slot.details.is_some() {
                                                    comp.update_result(live_tool_result(slot), Some(true));
                                                }
                                            }
                                            ToolSlotStatus::Done => {
                                                comp.update_result(live_tool_result(slot), Some(false));
                                            }
                                        }
                                    }
                                    self.transcript.add_child(Arc::new(comp));
                                }
                            }
                        }
                    }
                }
                continue;
            }

            if TOOL_RESULT_ENTRY.matches(Some(entry))
                && let Some(models) = &entry.model
            {
                for msg in models {
                    if let Message::ToolResult(tr) = msg
                        && !paired_call_ids.contains(&tr.tool_call_id)
                    {
                        let renderer = self.tool_renderers.get(&tr.tool_name).cloned();
                        let comp = ToolExecutionComponent::new(
                            &tr.tool_name,
                            &tr.tool_call_id,
                            Value::Null,
                            Some(ToolExecutionOptions::default()),
                            renderer,
                            Arc::clone(&self.ui),
                            &self.cwd,
                        );
                        comp.set_args_complete();
                        comp.update_result(
                            ToolExecutionResult {
                                content: tr.content.clone(),
                                details: tr.details.clone(),
                                is_error: tr.is_error,
                                duration_ms: None,
                            },
                            Some(false),
                        );
                        self.transcript.add_child(Arc::new(comp));
                    }
                }
            }
        }

        if let Some(ref generation) = live.generation
            && let Some(ref streaming_msg) = generation.message
        {
            let comp = AssistantMessageComponent::new(Some(streaming_msg.clone()), None, None, None, None, None);
            comp.update_content(streaming_msg.clone(), Some(true));
            self.transcript.add_child(Arc::new(comp));
        }

        self.apply_pending(view);
        self.apply_status(&live);
    }

    fn apply_pending(&mut self, view: &ConversationView) {
        self.pending_messages.clear();
        let inbox = inbox_of(view);
        let t = theme();
        for item in inbox.items {
            match item {
                InboxItem::Steer { content, .. } => {
                    let text = user_message_text(&content);
                    if !text.is_empty() {
                        let line = t.fg(ThemeColor::Dim, &format!("Steering: {text}"));
                        self.pending_messages
                            .add_child(Arc::new(Text::new(Some(&line), Some(1), Some(0), None)));
                    }
                }
                InboxItem::FollowUp { content, .. } => {
                    let text = user_message_text(&content);
                    if !text.is_empty() {
                        let line = t.fg(ThemeColor::Dim, &format!("Queued: {text}"));
                        self.pending_messages
                            .add_child(Arc::new(Text::new(Some(&line), Some(1), Some(0), None)));
                    }
                }
                InboxItem::Write { .. } => {}
            }
        }
    }

    fn apply_status(&mut self, live: &LiveState) {
        if let Some(prev) = self.active_status.take() {
            prev.dispose();
        }
        self.status.clear();

        if let Some(compaction) = live.compactions.as_deref().and_then(|c| c.first()) {
            let reason = match compaction.reason {
                pi_durable::harness::types::CompactionReason::Manual => CompactionStatusReason::Manual,
                pi_durable::harness::types::CompactionReason::Threshold => CompactionStatusReason::Threshold,
                pi_durable::harness::types::CompactionReason::Overflow => CompactionStatusReason::Overflow,
            };
            let indicator = StatusIndicator::compaction(Arc::clone(&self.ui), reason, None);
            self.status.add_child(Arc::new(indicator.clone()));
            self.active_status = Some(indicator);
            return;
        }

        if let Some(ref generation) = live.generation {
            if let Some(ref retry) = generation.retry {
                let msg = format!("Retrying: {}", retry.error);
                let indicator = StatusIndicator::working(Arc::clone(&self.ui), &msg, None, None);
                self.status.add_child(Arc::new(indicator.clone()));
                self.active_status = Some(indicator);
                return;
            }
            if generation.deferred.is_some() {
                let indicator = StatusIndicator::working(Arc::clone(&self.ui), "Waiting on response...", None, None);
                self.status.add_child(Arc::new(indicator.clone()));
                self.active_status = Some(indicator);
                return;
            }
        }

        if live.run.is_some() {
            let msg = if live
                .tools
                .as_deref()
                .is_some_and(|tools| tools.iter().any(|t| t.status == ToolSlotStatus::Running))
            {
                "Running tools..."
            } else {
                "Working..."
            };
            let indicator = StatusIndicator::working(Arc::clone(&self.ui), msg, None, None);
            self.status.add_child(Arc::new(indicator.clone()));
            self.active_status = Some(indicator);
        }
    }

    pub fn refresh_theme(&mut self, view: &ConversationView) {
        self.tool_renderers = builtin_tool_renderers(&self.cwd);
        self.transcript.invalidate();
        self.pending_messages.invalidate();
        self.status.invalidate();
        self.apply(view);
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        let mut lines = self.transcript.render(width);
        lines.extend(self.pending_messages.render(width));
        lines.extend(self.status.render(width));
        lines
    }

    pub fn dispose(&mut self) {
        if let Some(prev) = self.active_status.take() {
            prev.dispose();
        }
        self.transcript.clear();
        self.pending_messages.clear();
        self.status.clear();
    }
}

impl Component for ExperimentalChatView {
    fn render(&self, width: usize) -> Vec<String> {
        ExperimentalChatView::render(self, width)
    }

    fn invalidate(&self) {
        self.transcript.invalidate();
        self.pending_messages.invalidate();
        self.status.invalidate();
    }

    fn has_handle_input(&self) -> bool {
        false
    }

    fn handle_mouse(&self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.transcript.handle_mouse(event)
    }

    fn is_container_handle_mouse(&self) -> bool {
        true
    }

    fn container_children(&self) -> Option<Vec<Arc<dyn Component>>> {
        Some(self.transcript.children())
    }
}
