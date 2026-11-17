//! Port of packages/coding-agent/src/modes/interactive/interactive-mode.ts#L2988-L4765

use std::sync::Arc;
use std::sync::atomic::Ordering;

use indexmap::{IndexMap, IndexSet};
use pi_agent_core::{AgentMessage, AgentToolResult};
use pi_ai::{
    AssistantContentBlock, AssistantMessage, Message, ModelThinkingLevel, StopReason, UserContentBlock,
    UserMessageContent,
};
use pi_js::Result;
use pi_tui::editor_component::EditorBorderColor;
use pi_tui::tui::OverlayHandle;
use pi_tui::{Component, Spacer};
use serde_json::Value;

use crate::core::agent_session::parse_skill_block;

use crate::core::cache_stats::{CACHE_TTL_MS, ModelPriceSource, collect_cache_misses};
use crate::core::cache_warmer::format_cache_warming_usage;
use crate::core::messages::{create_compaction_summary_message, create_custom_message};
use crate::core::session_manager::{CustomEntry, SessionEntry, UsageEntry, session_entry_to_context_messages};
use crate::core::trust_manager::has_trust_requiring_project_resources;
use crate::modes::interactive::components::assistant_message::AssistantMessageComponent;
use crate::modes::interactive::components::bash_execution::{BashExecutionComponent, TruncationResult};
use crate::modes::interactive::components::branch_summary_message::{
    BranchSummaryMessage, BranchSummaryMessageComponent,
};
use crate::modes::interactive::components::compaction_summary_message::{
    CompactionSummaryMessage, CompactionSummaryMessageComponent,
};
use crate::modes::interactive::components::custom_entry::CustomEntryComponent;
use crate::modes::interactive::components::custom_message::{
    CustomMessage, CustomMessageComponent, CustomMessageContent,
};
use crate::modes::interactive::components::settings_selector::DoubleEscapeAction;
use crate::modes::interactive::components::skill_invocation_message::SkillInvocationMessageComponent;
use crate::modes::interactive::components::status_indicator::{
    CompactionStatusReason, StatusIndicator, StatusIndicatorKind,
};
use crate::modes::interactive::components::themed_text::ThemedText;
use crate::modes::interactive::components::tool_execution::{
    ToolExecutionComponent, ToolExecutionOptions, ToolExecutionResult,
};
use crate::modes::interactive::components::user_message::UserMessageComponent;
use crate::modes::interactive::theme::theme::{ThemeColor, get_editor_theme, theme};
use crate::modes::json_event::{AgentSessionEvent, CompactionReason, SummarizationRetrySource};

use super::*;

fn json_object_to_value(obj: &pi_ai::JsonObject) -> Value {
    let mut map = serde_json::Map::with_capacity(obj.len());
    for (k, v) in obj {
        map.insert(k.clone(), v.clone());
    }
    Value::Object(map)
}

fn thinking_level_name(level: ModelThinkingLevel) -> &'static str {
    match level {
        ModelThinkingLevel::Off => "off",
        ModelThinkingLevel::Minimal => "minimal",
        ModelThinkingLevel::Low => "low",
        ModelThinkingLevel::Medium => "medium",
        ModelThinkingLevel::High => "high",
        ModelThinkingLevel::XHigh => "xhigh",
        ModelThinkingLevel::Max => "max",
    }
}

fn to_compaction_status_reason(reason: CompactionReason) -> CompactionStatusReason {
    match reason {
        CompactionReason::Manual => CompactionStatusReason::Manual,
        CompactionReason::Threshold => CompactionStatusReason::Threshold,
        CompactionReason::Overflow => CompactionStatusReason::Overflow,
    }
}

fn create_custom_agent_message(
    custom_type: &str,
    content: CustomMessageContent,
    display: bool,
    details: Option<Value>,
    timestamp: &str,
) -> AgentMessage {
    let msg = create_custom_message(custom_type.to_string(), content, display, details, timestamp);
    AgentMessage::Custom(serde_json::to_value(msg).unwrap_or(Value::Null))
}

fn create_compaction_summary_agent_message(summary: &str, tokens_before: i64, timestamp: &str) -> AgentMessage {
    let msg = create_compaction_summary_message(summary.to_string(), tokens_before, timestamp);
    AgentMessage::Custom(serde_json::to_value(msg).unwrap_or(Value::Null))
}

struct RuntimeModelPriceSource(Arc<dyn InteractiveModelRuntime>);

impl ModelPriceSource for RuntimeModelPriceSource {
    fn get_cache_read_cost(&self, provider: &str, model_id: &str) -> Option<f64> {
        self.0.get_model(provider, model_id).map(|m| m.cost.cache_read)
    }
}

fn collect_cache_misses_by_id(
    entries: &[SessionEntry],
    runtime: Arc<dyn InteractiveModelRuntime>,
) -> IndexMap<String, CacheMissNotice> {
    let price_source = RuntimeModelPriceSource(runtime);
    let indexed = collect_cache_misses(entries, &price_source);
    let mut by_id = IndexMap::with_capacity(indexed.len());
    for (idx, miss) in indexed {
        if let Some(entry) = entries.get(idx) {
            by_id.insert(entry.id().to_string(), miss);
        }
    }
    by_id
}

enum RenderSessionItemLocal {
    Custom(CustomEntry),
    Usage(UsageEntry),
    CompactionCost(CompactionCostNotice),
    Message { msg: AgentMessage, entry_id: String },
}

struct CustomEntryExpandable(CustomEntryComponent);
impl Expandable for CustomEntryExpandable {
    fn set_expanded(&self, expanded: bool) {
        self.0.set_expanded(expanded);
    }
}

struct CustomMessageExpandable(CustomMessageComponent);
impl Expandable for CustomMessageExpandable {
    fn set_expanded(&self, expanded: bool) {
        self.0.set_expanded(expanded);
    }
}

struct CompactionSummaryExpandable(CompactionSummaryMessageComponent);
impl Expandable for CompactionSummaryExpandable {
    fn set_expanded(&self, expanded: bool) {
        self.0.set_expanded(expanded);
    }
}

struct BranchSummaryExpandable(BranchSummaryMessageComponent);
impl Expandable for BranchSummaryExpandable {
    fn set_expanded(&self, expanded: bool) {
        self.0.set_expanded(expanded);
    }
}

struct SkillInvocationExpandable(SkillInvocationMessageComponent);
impl Expandable for SkillInvocationExpandable {
    fn set_expanded(&self, expanded: bool) {
        self.0.set_expanded(expanded);
    }
}

impl InteractiveMode {
    pub(crate) fn set_default_editor_action(&self, action: &str, handler: Arc<dyn Fn() + Send + Sync>) {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .default_editor_actions
            .insert(action.to_string(), Arc::clone(&handler));
        self.inner.default_editor.on_action(action, handler);
    }

    pub(crate) fn set_default_editor_escape(&self, handler: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .default_editor_on_escape
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = handler.clone();
        self.inner.default_editor.set_on_escape(handler);
    }

    pub(crate) fn set_default_editor_ctrl_d(&self, handler: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .default_editor_on_ctrl_d
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = handler.clone();
        self.inner.default_editor.set_on_ctrl_d(handler);
    }

    pub(crate) fn set_default_editor_paste_image(&self, handler: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .default_editor_on_paste_image
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = handler.clone();
        self.inner.default_editor.set_on_paste_image(handler);
    }

    pub(crate) fn set_default_editor_extension_shortcut(
        &self,
        handler: Option<Arc<dyn Fn(&str) -> bool + Send + Sync>>,
    ) {
        *self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .default_editor_on_extension_shortcut
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = handler.clone();
        self.inner.default_editor.set_on_extension_shortcut(handler);
    }

    pub(crate) async fn get_user_input_or_shutdown(&self) -> Option<String> {
        {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if st.is_shutting_down {
                return None;
            }
            if let Some(queued) = st.pending_user_inputs.pop_front() {
                return Some(queued);
            }
        }
        let rx = {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if st.is_shutting_down {
                return None;
            }
            if let Some(queued) = st.pending_user_inputs.pop_front() {
                return Some(queued);
            }
            let (tx, rx) = tokio::sync::oneshot::channel();
            st.on_input_callback = Some(tx);
            rx
        };
        tokio::select! {
            res = rx => res.ok(),
            _ = self.inner.shutdown_notify.notified() => None,
        }
    }

    pub fn set_custom_editor_component(&self, factory: Option<EditorFactory>) {
        let (current_text, dialog_open, prev_autocomplete, border_color, padding_x, ac_max_vis, active_ind) = {
            let st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let current_text = st.editor.get_text();
            let dialog_open = st.active_selector_dispose.is_some() || !st.extension_dialogs.is_empty();
            (
                current_text,
                dialog_open,
                st.autocomplete_provider.clone(),
                self.inner.default_editor.border_color(),
                self.inner.default_editor.get_padding_x(),
                self.inner.default_editor.get_autocomplete_max_visible(),
                st.active_status_indicator.clone(),
            )
        };

        {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            st.editor_component_factory = factory.clone();
        }

        if !dialog_open {
            self.dispose_active_selector();
            self.inner.editor_container.clear();
        }

        let new_editor = if let Some(make_editor) = factory {
            let custom = make_editor(self.ui(), get_editor_theme(), (*self.inner.keybindings).clone());
            custom.set_on_submit(self.inner.default_editor.on_submit());
            custom.set_on_change(self.inner.default_editor.on_change());
            custom.set_text(&current_text);
            if custom.border_color().is_some() {
                custom.set_border_color(Some(border_color));
            }
            custom.set_padding_x(padding_x as f64);
            custom.set_autocomplete_max_visible(ac_max_vis as f64);
            if let Some(provider) = prev_autocomplete {
                custom.set_autocomplete_provider(provider);
            }
            ActiveEditor::Custom {
                component: custom,
                set_output_pad_unused: (),
            }
        } else {
            self.inner.default_editor.set_text(&current_text);
            ActiveEditor::Default(self.inner.default_editor.clone())
        };

        {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            st.editor = new_editor.clone();
        }

        if !dialog_open {
            self.inner.editor_container.add_child(new_editor.as_component());
        }
        if let Some(ind) = active_ind {
            self.inner.status_container.clear();
            let embedded = self.set_editor_working_status_indicator(Some(ind.clone()));
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .active_working_indicator_embedded = embedded;
            if !embedded {
                self.inner.status_container.add_child(Arc::new(ind));
            }
        }
        if !dialog_open {
            self.ui().set_focus(Some(new_editor.as_component()));
        }
        self.ui().request_render(None);
    }

    pub(crate) fn show_extension_notify(&self, message: &str, kind: Option<NotifyKind>) {
        match kind {
            Some(NotifyKind::Error) => self.show_error(message),
            Some(NotifyKind::Warning) => self.show_warning(message),
            Some(NotifyKind::Info) | None => self.show_status(message),
        }
    }

    pub(crate) async fn show_extension_custom(
        &self,
        factory: CustomUiFactory,
        options: Option<ExtensionCustomOptions>,
    ) -> Result<Value> {
        let saved_text = self.active_editor().get_expanded_text();
        let saved_text_done = saved_text.clone();
        let opts = options.unwrap_or_default();
        let is_overlay = opts.overlay;
        let overlay_status_source = if is_overlay && opts.blocked.is_some() {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let id = st.next_blocking_overlay_id;
            st.next_blocking_overlay_id += 1;
            Some(format!("extension-overlay:{id}"))
        } else {
            None
        };

        let (tx, rx) = tokio::sync::oneshot::channel::<Value>();
        let tx_slot = Arc::new(std::sync::Mutex::new(Some(tx)));
        let comp_slot: Arc<std::sync::Mutex<Option<Arc<dyn Component>>>> = Arc::new(std::sync::Mutex::new(None));
        let overlay_slot: Arc<std::sync::Mutex<Option<(OverlayHandle, usize)>>> = Arc::new(std::sync::Mutex::new(None));
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let m_remove = self.clone();
        let overlay_slot_rm = Arc::clone(&overlay_slot);
        let source_rm = overlay_status_source.clone();
        let remove_overlay: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let entry = overlay_slot_rm.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some((handle, entry_id)) = entry {
                let mut existed = false;
                {
                    let mut st = m_remove.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(idx) = st.extension_overlays.iter().position(|(id, _)| *id == entry_id) {
                        st.extension_overlays.remove(idx);
                        existed = true;
                    }
                }
                if !existed {
                    return;
                }
                handle.hide();
                if let Some(ref src) = source_rm {
                    m_remove.inner.program_status.set_blocked(src, None);
                }
            }
        });

        let m_close = self.clone();
        let closed_close = Arc::clone(&closed);
        let tx_close = Arc::clone(&tx_slot);
        let comp_close = Arc::clone(&comp_slot);
        let rm_close = Arc::clone(&remove_overlay);
        let done_fn: DoneFn = Arc::new(move |value: Value| {
            if closed_close.swap(true, Ordering::SeqCst) {
                return;
            }
            if is_overlay {
                rm_close();
            } else {
                let comp = comp_close.lock().unwrap_or_else(|e| e.into_inner()).take();
                if let Some(comp) = comp {
                    m_close.remove_extension_dialog(&comp);
                }
                m_close.active_editor().set_text(&saved_text_done);
                m_close.ui().request_render(None);
            }
            let sender = tx_close.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(sender) = sender {
                let _ = sender.send(value);
            }
        });

        let built = factory(self.ui(), theme(), (*self.inner.keybindings).clone(), done_fn).await;
        match built {
            Ok(component) => {
                if closed.load(Ordering::SeqCst) {
                    return rx
                        .await
                        .map_err(|e| pi_js::Error::msg(format!("extension custom closed: {e}")));
                }
                *comp_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&component));
                if is_overlay {
                    let handle = self.ui().show_overlay(component, opts.overlay_options);
                    let entry_id = {
                        let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                        let id = st.next_overlay_entry_id;
                        st.next_overlay_entry_id += 1;
                        st.extension_overlays.push((id, Arc::clone(&remove_overlay)));
                        id
                    };
                    *overlay_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some((handle.clone(), entry_id));
                    if let Some(ref src) = overlay_status_source {
                        self.inner.program_status.set_blocked(src, opts.blocked);
                    }
                    if let Some(on_handle) = opts.on_handle {
                        on_handle(handle);
                    }
                } else {
                    self.push_extension_dialog(component, None, opts.blocked);
                }
                rx.await
                    .map_err(|e| pi_js::Error::msg(format!("extension custom dropped: {e}")))
            }
            Err(err) => {
                if !is_overlay && !closed.load(Ordering::SeqCst) {
                    let comp = comp_slot.lock().unwrap_or_else(|e| e.into_inner()).take();
                    if let Some(comp) = comp {
                        self.remove_extension_dialog(&comp);
                    }
                    self.active_editor().set_text(&saved_text);
                    self.ui().request_render(None);
                }
                Err(err)
            }
        }
    }

    pub(crate) fn show_extension_error(&self, extension_path: &str, error: &str, stack: Option<&str>) {
        let error_msg = format!("Extension \"{extension_path}\" error: {error}");
        let msg_for_render = error_msg.clone();
        self.add_chat_component(
            Arc::new(ThemedText::new(
                Arc::new(move || theme().fg(ThemeColor::Error, &msg_for_render)),
                Some(1),
                Some(0),
            )),
            None,
            None,
            None,
        );
        if let Some(stack_str) = stack {
            let stack_lines: Vec<String> = stack_str.lines().skip(1).map(|l| format!("  {}", l.trim())).collect();
            if !stack_lines.is_empty() {
                self.add_chat_component(
                    Arc::new(ThemedText::new(
                        Arc::new(move || {
                            stack_lines
                                .iter()
                                .map(|line| theme().fg(ThemeColor::Dim, line))
                                .collect::<Vec<_>>()
                                .join("\n")
                        }),
                        Some(1),
                        Some(0),
                    )),
                    None,
                    None,
                    None,
                );
            }
        }
        self.ui().request_render(None);
    }

    // =========================================================================
    // Key Handlers
    // =========================================================================

    pub fn setup_key_handlers(&self) {
        let m_esc = self.clone();
        self.set_default_editor_escape(Some(Arc::new(move || {
            let sess = m_esc.session();
            if sess.is_streaming() {
                m_esc.restore_queued_messages_to_editor(true);
            } else if sess.is_bash_running() {
                sess.abort_bash();
            } else {
                let is_bash_mode = m_esc.inner.state.lock().unwrap_or_else(|e| e.into_inner()).is_bash_mode;
                if is_bash_mode {
                    m_esc.active_editor().set_text("");
                    m_esc.inner.state.lock().unwrap_or_else(|e| e.into_inner()).is_bash_mode = false;
                    m_esc.update_editor_border_color();
                } else if m_esc.active_editor().get_text().trim().is_empty() {
                    let action = m_esc.settings_manager().get_double_escape_action();
                    if action != DoubleEscapeAction::None {
                        let now = chrono::Utc::now().timestamp_millis() as f64;
                        let triggered = {
                            let mut st = m_esc.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                            if now - st.last_escape_time_ms < 500.0 {
                                st.last_escape_time_ms = 0.0;
                                true
                            } else {
                                st.last_escape_time_ms = now;
                                false
                            }
                        };
                        if triggered {
                            match action {
                                DoubleEscapeAction::Tree => m_esc.show_tree_selector(None),
                                DoubleEscapeAction::Fork => m_esc.show_user_message_selector(),
                                DoubleEscapeAction::None => {}
                            }
                        }
                    }
                }
            }
        })));

        let m = self.clone();
        self.set_default_editor_action("app.clear", Arc::new(move || m.handle_ctrl_c()));
        let m = self.clone();
        self.set_default_editor_ctrl_d(Some(Arc::new(move || m.handle_ctrl_d())));
        let m = self.clone();
        self.set_default_editor_action("app.suspend", Arc::new(move || m.handle_ctrl_z()));
        let m = self.clone();
        self.set_default_editor_action("app.thinking.cycle", Arc::new(move || m.cycle_thinking_level()));
        let m = self.clone();
        self.set_default_editor_action(
            "app.model.cycleForward",
            Arc::new(move || {
                let m2 = m.clone();
                tokio::spawn(async move { m2.cycle_model(CycleDirection::Forward).await });
            }),
        );
        let m = self.clone();
        self.set_default_editor_action(
            "app.model.cycleBackward",
            Arc::new(move || {
                let m2 = m.clone();
                tokio::spawn(async move { m2.cycle_model(CycleDirection::Backward).await });
            }),
        );

        let m = self.clone();
        self.ui().set_on_debug(Some(Arc::new(move || m.handle_debug_command())));

        let m = self.clone();
        self.set_default_editor_action("app.model.select", Arc::new(move || m.show_model_selector(None)));
        let m = self.clone();
        self.set_default_editor_action("app.tools.expand", Arc::new(move || m.toggle_tool_output_expansion()));
        let m = self.clone();
        self.set_default_editor_action(
            "app.thinking.toggle",
            Arc::new(move || m.toggle_thinking_block_visibility()),
        );
        let m = self.clone();
        self.set_default_editor_action(
            "app.editor.external",
            Arc::new(move || {
                let m2 = m.clone();
                tokio::spawn(async move { m2.handle_open_external_editor().await });
            }),
        );
        let m = self.clone();
        self.set_default_editor_action(
            "app.message.copy",
            Arc::new(move || {
                let m2 = m.clone();
                tokio::spawn(async move { m2.handle_copy_command(true, true).await });
            }),
        );
        let m = self.clone();
        self.set_default_editor_action(
            "app.message.followUp",
            Arc::new(move || {
                let m2 = m.clone();
                tokio::spawn(async move { m2.handle_follow_up().await });
            }),
        );
        let m = self.clone();
        self.set_default_editor_action("app.message.dequeue", Arc::new(move || m.handle_dequeue()));
        let m = self.clone();
        self.set_default_editor_action(
            "app.session.new",
            Arc::new(move || {
                let m2 = m.clone();
                tokio::spawn(async move { m2.handle_clear_command().await });
            }),
        );
        let m = self.clone();
        self.set_default_editor_action("app.session.tree", Arc::new(move || m.show_tree_selector(None)));
        let m = self.clone();
        self.set_default_editor_action("app.session.fork", Arc::new(move || m.show_user_message_selector()));
        let m = self.clone();
        self.set_default_editor_action("app.session.resume", Arc::new(move || m.show_session_selector()));

        let m = self.clone();
        self.inner
            .default_editor
            .set_on_change(Some(Arc::new(move |text: &str| {
                let next_bash = text.trim_start().starts_with('!');
                let changed = {
                    let mut st = m.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                    if st.is_bash_mode != next_bash {
                        st.is_bash_mode = next_bash;
                        true
                    } else {
                        false
                    }
                };
                if changed {
                    m.update_editor_border_color();
                }
            })));

        let m = self.clone();
        self.set_default_editor_paste_image(Some(Arc::new(move || {
            let m2 = m.clone();
            tokio::spawn(async move { m2.handle_clipboard_paste().await });
        })));
    }

    pub async fn handle_right_click_paste(&self) {
        let target = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .renderer
            .get_focused_component();
        let Some(target) = target else {
            return;
        };
        if let Ok(Some(text)) = (self.inner.hooks.clipboard_read_text)().await
            && !text.is_empty()
        {
            let still_focused = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .renderer
                .get_focused_component()
                .is_some_and(|curr| Arc::ptr_eq(&curr, &target));
            if still_focused {
                target.handle_input(&format!("\x1b[200~{text}\x1b[201~"));
                self.ui().request_render(None);
            }
        }
    }

    pub async fn handle_clipboard_paste(&self) {
        match (self.inner.hooks.clipboard_read_file_paths)().await {
            Ok(Some(file_paths)) if !file_paths.is_empty() => {
                if file_paths
                    .iter()
                    .any(|file_path| file_path.chars().any(char::is_control))
                {
                    self.show_error("Failed to paste from clipboard: Clipboard file path contains control characters");
                    return;
                }
                let is_bash = self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).is_bash_mode;
                let paths = if is_bash {
                    file_paths
                        .iter()
                        .map(|p| quote_if_needed(p))
                        .collect::<Vec<_>>()
                        .join(" ")
                } else {
                    file_paths.join("\n")
                };
                let editor = self.active_editor();
                let (leading_space, trailing_space) = if let Some(cursor) = editor.get_cursor()
                    && let Ok(line_idx) = usize::try_from(cursor.line)
                    && let Ok(col_idx) = usize::try_from(cursor.col)
                {
                    let text = editor.get_text();
                    let current_line = text.split('\n').nth(line_idx).unwrap_or("");
                    let units: Vec<u16> = current_line.encode_utf16().collect();
                    let char_before = col_idx
                        .checked_sub(1)
                        .and_then(|idx| units.get(idx))
                        .and_then(|&u| char::from_u32(u as u32));
                    let char_after = units.get(col_idx).and_then(|&u| char::from_u32(u as u32));
                    let lead = if char_before.is_some_and(|c| !c.is_whitespace()) {
                        " "
                    } else {
                        ""
                    };
                    let trail = if char_after.is_some_and(|c| !c.is_whitespace()) {
                        " "
                    } else {
                        ""
                    };
                    (lead, trail)
                } else {
                    ("", "")
                };
                editor.insert_text_at_cursor(&format!("{leading_space}{paths}{trailing_space}"));
                self.ui().request_render(None);
                return;
            }
            Ok(_) => {}
            Err(err) => {
                self.show_error(&format!("Failed to paste from clipboard: {err}"));
                return;
            }
        }

        match (self.inner.hooks.clipboard_read_image)().await {
            Ok(Some(image)) => {
                let ext =
                    crate::utils::clipboard_image::extension_for_image_mime_type(&image.mime_type).unwrap_or("png");
                let file_name = format!("pi-clipboard-{}.{ext}", pi_js::crypto::random_uuid());
                let file_path = pi_js::path::join(&[&pi_js::env::tmp_dir(), &file_name]);
                if let Err(err) = pi_js::fs::write_file(&file_path, &image.bytes, None) {
                    self.show_error(&format!("Failed to paste from clipboard: {err}"));
                    return;
                }
                self.active_editor().insert_text_at_cursor(&file_path);
                self.ui().request_render(None);
                return;
            }
            Ok(_) => {}
            Err(err) => {
                self.show_error(&format!("Failed to paste from clipboard: {err}"));
                return;
            }
        }

        match (self.inner.hooks.clipboard_read_text)().await {
            Ok(Some(text)) if !text.is_empty() => {
                self.active_editor().insert_text_at_cursor(&text);
                self.ui().request_render(None);
            }
            Ok(_) => {}
            Err(err) => {
                self.show_error(&format!("Failed to paste from clipboard: {err}"));
            }
        }
    }

    pub fn handle_startup_submit(&self, text: &str) {
        self.active_editor().set_text(text);
        self.show_status("Startup is still in progress");
    }

    pub fn setup_editor_submit_handler(&self) {
        let m = self.clone();
        self.inner
            .default_editor
            .set_on_submit(Some(Arc::new(move |raw_text: &str| {
                let raw_text = raw_text.to_string();
                let this = m.clone();
                tokio::spawn(async move {
                    let text = raw_text.trim().to_string();
                    if text.is_empty() {
                        return;
                    }

                    if text == "/settings" {
                        this.show_settings_selector();
                        this.active_editor().set_text("");
                        return;
                    }
                    if text == "/scoped-models" {
                        this.active_editor().set_text("");
                        this.show_models_selector();
                        return;
                    }
                    if text == "/model" || text.starts_with("/model ") {
                        let search_term = text.strip_prefix("/model ").map(|s| s.trim().to_string());
                        this.active_editor().set_text("");
                        this.handle_model_command(search_term.as_deref()).await;
                        return;
                    }
                    if text == "/thinking" || text.starts_with("/thinking ") {
                        let search_term = text.strip_prefix("/thinking ").map(|s| s.trim().to_string());
                        this.active_editor().set_text("");
                        this.handle_thinking_command(search_term.as_deref());
                        return;
                    }
                    if text == "/export" || text.starts_with("/export ") {
                        this.handle_export_command(&text).await;
                        this.active_editor().set_text("");
                        return;
                    }
                    if text == "/import" || text.starts_with("/import ") {
                        this.handle_import_command(&text).await;
                        this.active_editor().set_text("");
                        return;
                    }
                    if text == "/share" {
                        this.handle_share_command().await;
                        this.active_editor().set_text("");
                        return;
                    }
                    if text == "/bug" || text.starts_with("/bug ") {
                        let hint = text
                            .strip_prefix("/bug")
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty());
                        this.active_editor().set_text("");
                        this.handle_bug_command(hint.as_deref()).await;
                        return;
                    }
                    if text == "/copy" {
                        this.handle_copy_command(false, false).await;
                        this.active_editor().set_text("");
                        return;
                    }
                    if text == "/name" || text.starts_with("/name ") {
                        this.handle_name_command(&text);
                        this.active_editor().set_text("");
                        return;
                    }
                    if text == "/session" {
                        this.handle_session_command();
                        this.active_editor().set_text("");
                        return;
                    }
                    if text == "/changelog" {
                        this.handle_changelog_command();
                        this.active_editor().set_text("");
                        return;
                    }
                    if text == "/hotkeys" {
                        this.handle_hotkeys_command();
                        this.active_editor().set_text("");
                        return;
                    }
                    if text == "/fork" {
                        this.show_user_message_selector();
                        this.active_editor().set_text("");
                        return;
                    }
                    if text == "/clone" {
                        this.active_editor().set_text("");
                        this.handle_clone_command().await;
                        return;
                    }
                    if text == "/tree" {
                        this.show_tree_selector(None);
                        this.active_editor().set_text("");
                        return;
                    }
                    if text == "/trust" {
                        this.show_trust_selector();
                        this.active_editor().set_text("");
                        return;
                    }
                    if text == "/login" || text.starts_with("/login ") {
                        let provider_ref = text.strip_prefix("/login ").map(|s| s.trim().to_string());
                        this.active_editor().set_text("");
                        this.handle_login_command(provider_ref.as_deref()).await;
                        return;
                    }
                    if text == "/logout" {
                        this.show_oauth_selector(
                            crate::modes::interactive::components::oauth_selector::OAuthSelectorMode::Logout,
                        )
                        .await;
                        this.active_editor().set_text("");
                        return;
                    }
                    if text == "/new" {
                        this.active_editor().set_text("");
                        this.handle_clear_command().await;
                        return;
                    }
                    if text == "/compact" || text.starts_with("/compact ") {
                        let custom_instructions = text.strip_prefix("/compact ").map(|s| s.trim().to_string());
                        this.active_editor().set_text("");
                        this.handle_compact_command(custom_instructions.as_deref()).await;
                        return;
                    }
                    if text == "/reload" {
                        this.active_editor().set_text("");
                        this.handle_reload_command().await;
                        return;
                    }
                    if text == "/debug" {
                        this.handle_debug_command();
                        this.active_editor().set_text("");
                        return;
                    }
                    if text == "/arminsayshi" {
                        this.handle_armin_says_hi();
                        this.active_editor().set_text("");
                        return;
                    }
                    if text == "/dementedelves" {
                        this.handle_demented_elves();
                        this.active_editor().set_text("");
                        return;
                    }
                    if text == "/resume" {
                        this.show_session_selector();
                        this.active_editor().set_text("");
                        return;
                    }
                    if text == "/quit" {
                        this.active_editor().set_text("");
                        this.shutdown(false).await;
                        return;
                    }

                    if text.starts_with('!') {
                        let is_excluded = text.starts_with("!!");
                        let command = if is_excluded {
                            text[2..].trim()
                        } else {
                            text[1..].trim()
                        };
                        if !command.is_empty() {
                            if this.session().is_bash_running() {
                                this.show_warning("A bash command is already running. Press Esc to cancel it first.");
                                this.active_editor().set_text(&text);
                                return;
                            }
                            this.active_editor().add_to_history(&text);
                            this.handle_bash_command(command, is_excluded).await;
                            this.inner.state.lock().unwrap_or_else(|e| e.into_inner()).is_bash_mode = false;
                            this.update_editor_border_color();
                            return;
                        }
                    }

                    let sess = this.session();
                    if sess.is_compacting() {
                        if this.is_extension_command(&text) {
                            this.active_editor().add_to_history(&text);
                            this.active_editor().set_text("");
                            let _ = sess.prompt(&text, None).await;
                        } else {
                            this.queue_compaction_message(&text, CompactionQueueMode::Steer);
                        }
                        return;
                    }

                    if sess.is_streaming() {
                        this.active_editor().add_to_history(&text);
                        this.active_editor().set_text("");
                        let _ = sess
                            .prompt(
                                &text,
                                Some(InteractivePromptOptions {
                                    images: None,
                                    streaming_behavior: Some(CompactionQueueMode::Steer),
                                }),
                            )
                            .await;
                        this.update_pending_messages_display();
                        this.ui().request_render(None);
                        return;
                    }

                    this.flush_pending_bash_components();

                    let callback = this
                        .inner
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .on_input_callback
                        .take();
                    if let Some(cb) = callback {
                        let _ = cb.send(text.clone());
                    } else {
                        this.inner
                            .state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .pending_user_inputs
                            .push_back(text.clone());
                    }
                    this.active_editor().add_to_history(&text);
                });
            })));
    }

    pub(crate) fn subscribe_to_agent(&self) {
        let m = self.clone();
        let unsub = self.session().subscribe(Arc::new(move |event| {
            let m2 = m.clone();
            Box::pin(async move {
                m2.handle_event(&event).await;
            })
        }));
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).unsubscribe = Some(unsub);
    }

    pub async fn handle_event(&self, event: &AgentSessionEvent) {
        if !self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_initialized
        {
            self.init().await;
        }

        self.inner.footer.invalidate();
        self.inner.program_status.handle_event(event);

        match event {
            AgentSessionEvent::AgentStart => {
                let retry_escape = {
                    let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                    st.pending_tool_components.clear();
                    st.retry_escape_handler.take()
                };
                if let Some(handler) = retry_escape {
                    self.set_default_editor_escape(Some(handler));
                }
            }

            AgentSessionEvent::TurnStart => {
                if self.settings_manager().get_show_terminal_progress() {
                    self.ui().terminal().set_progress(true);
                }
                let (working_visible, is_working_active) = {
                    let st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                    (
                        st.working_visible,
                        st.active_status_indicator
                            .as_ref()
                            .is_some_and(|ind| ind.kind() == StatusIndicatorKind::Working),
                    )
                };
                if working_visible {
                    if !is_working_active {
                        self.show_working_status_indicator();
                    }
                } else {
                    self.clear_status_indicator(None);
                }
                self.ui().request_render(None);
            }

            AgentSessionEvent::TurnEnd { .. } => {}

            AgentSessionEvent::QueueUpdate { .. } => {
                self.update_pending_messages_display();
                self.ui().request_render(None);
            }

            AgentSessionEvent::EntryAppended { entry } => {
                let already_rendered = self
                    .inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .entries_rendered_by_boundary_compaction
                    .shift_remove(entry.id());
                if already_rendered {
                    return;
                }
                match entry {
                    SessionEntry::Custom(custom) => {
                        self.add_custom_entry_to_chat(custom);
                        self.ui().request_render(None);
                    }
                    SessionEntry::Usage(usage_entry) if usage_entry.kind == "cache_warm" => {
                        self.add_cache_warming_usage(usage_entry);
                        self.ui().request_render(None);
                    }
                    SessionEntry::CustomMessage(cm) if cm.display => {
                        self.add_message_to_chat(
                            &create_custom_agent_message(
                                &cm.custom_type,
                                cm.content.clone(),
                                cm.display,
                                cm.details.clone(),
                                &cm.timestamp,
                            ),
                            false,
                        );
                        self.ui().request_render(None);
                    }
                    SessionEntry::Compaction(compaction) => {
                        let entries = self.session_manager().build_context_entries();
                        if entries.first().map(|e| e.id()) != Some(compaction.id.as_str()) {
                            return;
                        }
                        self.clear_chat_container();
                        let branch = self.session_manager().get_branch();
                        let mut entries_after_compaction: IndexSet<String> = IndexSet::new();
                        if let Some(comp_idx) = branch.iter().position(|e| e.id() == compaction.id) {
                            for e in &branch[comp_idx + 1..] {
                                entries_after_compaction.insert(e.id().to_string());
                            }
                        }
                        let retained: Vec<SessionEntry> = entries.into_iter().skip(1).collect();
                        let before_entries: Vec<SessionEntry> = retained
                            .iter()
                            .filter(|e| !entries_after_compaction.contains(e.id()))
                            .cloned()
                            .collect();
                        let after_entries: Vec<SessionEntry> = retained
                            .iter()
                            .filter(|e| entries_after_compaction.contains(e.id()))
                            .cloned()
                            .collect();
                        self.render_session_entries(&before_entries, false, false);
                        self.add_message_to_chat(
                            &create_compaction_summary_agent_message(
                                &compaction.summary,
                                compaction.tokens_before,
                                &compaction.timestamp,
                            ),
                            false,
                        );
                        if let Some(usage) = compaction.usage {
                            self.add_compaction_cost_notice(&CompactionCostNotice {
                                kind: CompactionCostKind::Compaction,
                                usage,
                            });
                        }
                        self.render_session_entries(&after_entries, false, false);
                        {
                            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                            for id in entries_after_compaction {
                                st.entries_rendered_by_boundary_compaction.insert(id);
                            }
                        }
                        self.inner.footer.invalidate();
                        self.ui().request_render(None);
                    }
                    _ => {}
                }
            }

            AgentSessionEvent::SessionInfoChanged { .. } => {
                self.update_terminal_title();
                self.inner.footer.invalidate();
                self.ui().request_render(None);
            }

            AgentSessionEvent::ThinkingLevelChanged { .. } => {
                self.inner.footer.invalidate();
                self.update_editor_border_color();
            }

            AgentSessionEvent::MessageStart { message } => {
                if let AgentMessage::Message(Message::Assistant(assistant)) = message {
                    let (hide_thinking, hidden_label, output_pad) = {
                        let st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                        (st.hide_thinking_block, st.hidden_thinking_label.clone(), st.output_pad)
                    };
                    let comp = AssistantMessageComponent::new(
                        None,
                        Some(hide_thinking),
                        Some(self.get_markdown_theme_with_settings()),
                        Some(&hidden_label),
                        Some(output_pad),
                        Some(self.get_markdown_transformers()),
                    );
                    let comp_arc: Arc<dyn Component> = Arc::new(comp.clone());
                    let comp_pad = comp.clone();
                    let pad_fn: Arc<dyn Fn(i64) + Send + Sync> = Arc::new(move |p| comp_pad.set_output_pad(p));
                    self.add_chat_component(Arc::clone(&comp_arc), None, Some(comp.clone()), Some(pad_fn));
                    {
                        let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                        st.streaming_component = Some(comp.clone());
                        st.streaming_component_arc = Some(comp_arc);
                        st.streaming_message = Some(assistant.clone());
                    }
                    comp.update_content(assistant.clone(), Some(true));
                    self.ui().request_render(None);
                } else if matches!(message, AgentMessage::Message(Message::User(_))) {
                    self.add_message_to_chat(message, false);
                    self.update_pending_messages_display();
                    self.ui().request_render(None);
                } else if message.role() == Some("custom") {
                    self.add_message_to_chat(message, false);
                    self.ui().request_render(None);
                }
            }

            AgentSessionEvent::MessageUpdate { message, .. } => {
                let AgentMessage::Message(Message::Assistant(assistant)) = message else {
                    return;
                };
                let (streaming_comp, tool_output_expanded, output_pad) = {
                    let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                    let comp = st.streaming_component.clone();
                    if comp.is_some() {
                        st.streaming_message = Some(assistant.clone());
                    }
                    (comp, st.tool_output_expanded, st.output_pad)
                };
                if let Some(comp) = streaming_comp {
                    comp.update_content(assistant.clone(), Some(true));
                    for block in &assistant.content {
                        if let AssistantContentBlock::ToolCall(tc) = block {
                            let existing = self
                                .inner
                                .state
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .pending_tool_components
                                .get(&tc.id)
                                .cloned();
                            if let Some(tool_comp) = existing {
                                tool_comp.update_args(json_object_to_value(&tc.arguments));
                            } else {
                                let settings = self.settings_manager();
                                let tool_comp = ToolExecutionComponent::new(
                                    &tc.name,
                                    &tc.id,
                                    json_object_to_value(&tc.arguments),
                                    Some(ToolExecutionOptions {
                                        show_images: Some(settings.get_show_images()),
                                        image_width_cells: settings.get_image_width_cells(),
                                        output_pad: Some(output_pad),
                                    }),
                                    self.session().get_tool_renderers(&tc.name),
                                    self.ui(),
                                    &self.session_manager().get_cwd(),
                                );
                                tool_comp.set_expanded(tool_output_expanded);
                                self.add_tool_chat_component(tool_comp.clone());
                                self.inner
                                    .state
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .pending_tool_components
                                    .insert(tc.id.clone(), tool_comp);
                            }
                        }
                    }
                    self.ui().request_render(None);
                }
            }

            AgentSessionEvent::MessageEnd { message } => {
                if matches!(message, AgentMessage::Message(Message::User(_))) {
                    return;
                }
                let AgentMessage::Message(Message::Assistant(assistant)) = message else {
                    self.ui().request_render(None);
                    return;
                };
                let streaming_comp = self
                    .inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .streaming_component
                    .clone();
                if let Some(comp) = streaming_comp {
                    let mut final_msg = assistant.clone();
                    let mut error_message: Option<String> = None;
                    if final_msg.stop_reason == StopReason::Aborted {
                        let retry_attempt = self.session().retry_attempt();
                        let msg = if retry_attempt > 0 {
                            format!(
                                "Aborted after {retry_attempt} retry attempt{}",
                                if retry_attempt > 1 { "s" } else { "" }
                            )
                        } else {
                            "Operation aborted".to_string()
                        };
                        final_msg.error_message = Some(msg.clone());
                        error_message = Some(msg);
                    }
                    comp.update_content(final_msg.clone(), Some(false));

                    if final_msg.stop_reason == StopReason::Aborted || final_msg.stop_reason == StopReason::Error {
                        let err_str = error_message.unwrap_or_else(|| {
                            final_msg
                                .error_message
                                .clone()
                                .filter(|s| !s.is_empty())
                                .unwrap_or_else(|| "Error".to_string())
                        });
                        let pending: Vec<ToolExecutionComponent> = {
                            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                            st.pending_tool_components.drain(..).map(|(_, c)| c).collect()
                        };
                        for tool_comp in pending {
                            tool_comp.update_result(
                                ToolExecutionResult {
                                    content: vec![UserContentBlock::Text(pi_ai::TextContent::new(err_str.clone()))],
                                    details: None,
                                    is_error: true,
                                    duration_ms: None,
                                },
                                None,
                            );
                        }
                        self.maybe_suggest_bug_report(&final_msg);
                    } else {
                        let pending: Vec<ToolExecutionComponent> = {
                            let st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                            st.pending_tool_components.values().cloned().collect()
                        };
                        for tool_comp in pending {
                            tool_comp.set_args_complete();
                        }
                        self.maybe_show_thinking_drop_notice(&final_msg);
                        self.maybe_show_cache_miss_notice(&final_msg);
                    }

                    {
                        let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                        st.streaming_component = None;
                        st.streaming_component_arc = None;
                        st.streaming_message = None;
                    }
                    self.inner.footer.invalidate();
                }
                self.ui().request_render(None);
            }

            AgentSessionEvent::BashExecutionUpdate { .. } => {}

            AgentSessionEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
                parent_tool_call_id,
            } => {
                if parent_tool_call_id.is_some() {
                    return;
                }
                let existing = self
                    .inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .pending_tool_components
                    .get(tool_call_id)
                    .cloned();
                let component = if let Some(c) = existing {
                    c
                } else {
                    let (tool_output_expanded, output_pad) = {
                        let st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                        (st.tool_output_expanded, st.output_pad)
                    };
                    let settings = self.settings_manager();
                    let created = ToolExecutionComponent::new(
                        tool_name,
                        tool_call_id,
                        args.clone(),
                        Some(ToolExecutionOptions {
                            show_images: Some(settings.get_show_images()),
                            image_width_cells: settings.get_image_width_cells(),
                            output_pad: Some(output_pad),
                        }),
                        self.session().get_tool_renderers(tool_name),
                        self.ui(),
                        &self.session_manager().get_cwd(),
                    );
                    created.set_expanded(tool_output_expanded);
                    self.add_tool_chat_component(created.clone());
                    self.inner
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .pending_tool_components
                        .insert(tool_call_id.clone(), created.clone());
                    created
                };
                component.mark_execution_started();
                self.ui().request_render(None);
            }

            AgentSessionEvent::ToolExecutionUpdate {
                tool_call_id,
                partial_result,
                ..
            } => {
                let comp = self
                    .inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .pending_tool_components
                    .get(tool_call_id)
                    .cloned();
                if let Some(component) = comp {
                    let parsed = agent_tool_result_to_execution_result(partial_result, false, None);
                    component.update_result(parsed, Some(true));
                    self.ui().request_render(None);
                }
            }

            AgentSessionEvent::ToolExecutionEnd {
                tool_call_id,
                result,
                is_error,
                ..
            } => {
                if *is_error {
                    self.maybe_show_install_change_warning();
                }
                let comp = self
                    .inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .pending_tool_components
                    .shift_remove(tool_call_id);
                if let Some(component) = comp {
                    let duration_ms = result
                        .details
                        .as_ref()
                        .and_then(|d| d.get("durationMs"))
                        .and_then(Value::as_f64);
                    let parsed = agent_tool_result_to_execution_result(result, *is_error, duration_ms);
                    component.update_result(parsed, None);
                    self.ui().request_render(None);
                }
            }

            AgentSessionEvent::AgentEnd { .. } => {
                if self.settings_manager().get_show_terminal_progress() {
                    self.ui().terminal().set_progress(false);
                }
                self.clear_status_indicator(Some(StatusIndicatorKind::Working));
                let streaming_arc = {
                    let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                    st.streaming_component = None;
                    st.streaming_message = None;
                    st.pending_tool_components.clear();
                    st.streaming_component_arc.take()
                };
                if let Some(arc) = streaming_arc {
                    self.remove_chat_component(&arc);
                }
                self.ui().request_render(None);
            }

            AgentSessionEvent::AgentSettled => {
                self.flush_pending_bash_components();
                self.ui().request_render(None);
                self.check_shutdown_requested().await;
            }

            AgentSessionEvent::CompactionStart { reason } => {
                if self.settings_manager().get_show_terminal_progress() {
                    self.ui().terminal().set_progress(true);
                }
                let prev_escape = self
                    .inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .default_editor_on_escape
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                self.inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .auto_compaction_escape_handler = prev_escape;
                let sess = self.session();
                self.set_default_editor_escape(Some(Arc::new(move || sess.abort_compaction())));

                let color_fn = if self.active_editor().embed_working_status() {
                    let fallback = theme()
                        .get_thinking_border_color_by_name(thinking_level_name(self.session().model_thinking_level()));
                    Some(self.active_editor().border_color().unwrap_or(fallback))
                } else {
                    None
                };
                self.show_status_indicator(StatusIndicator::compaction(
                    self.ui(),
                    to_compaction_status_reason(*reason),
                    color_fn,
                ));
                self.ui().request_render(None);
            }

            AgentSessionEvent::CompactionEnd {
                reason,
                result,
                aborted,
                will_retry,
                error_message,
            } => {
                if self.settings_manager().get_show_terminal_progress() {
                    self.ui().terminal().set_progress(false);
                }
                let prev_escape = self
                    .inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .auto_compaction_escape_handler
                    .take();
                if let Some(handler) = prev_escape {
                    self.set_default_editor_escape(Some(handler));
                }
                self.clear_status_indicator(Some(StatusIndicatorKind::Compaction));

                if *aborted {
                    if *reason == CompactionReason::Manual {
                        self.show_error("Compaction cancelled");
                    } else {
                        self.show_status("Auto-compaction cancelled");
                    }
                } else if let Some(res) = result {
                    let entries = self.session_manager().build_context_entries();
                    if matches!(entries.first(), Some(SessionEntry::Compaction(_))) {
                        self.clear_chat_container();
                        self.render_session_entries(&entries[1..], false, false);
                        let now_iso = chrono::Utc::now().to_rfc3339();
                        self.add_message_to_chat(
                            &create_compaction_summary_agent_message(&res.summary, res.tokens_before, &now_iso),
                            false,
                        );
                        if let Some(usage) = res.usage {
                            self.add_compaction_cost_notice(&CompactionCostNotice {
                                kind: CompactionCostKind::Compaction,
                                usage,
                            });
                        }
                        self.inner.footer.invalidate();
                    } else {
                        self.show_error("Completed compaction is missing from the session context");
                    }
                } else if let Some(err_msg) = error_message {
                    if *reason == CompactionReason::Manual {
                        self.show_error(err_msg);
                    } else {
                        self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
                        let msg = err_msg.clone();
                        self.add_chat_component(
                            Arc::new(ThemedText::new(
                                Arc::new(move || theme().fg(ThemeColor::Error, &msg)),
                                Some(1),
                                Some(0),
                            )),
                            None,
                            None,
                            None,
                        );
                    }
                }
                let m = self.clone();
                let retry_flag = *will_retry;
                tokio::spawn(async move { m.flush_compaction_queue(retry_flag).await });
                self.ui().request_render(None);
            }

            AgentSessionEvent::AutoRetryStart {
                attempt,
                max_attempts,
                delay_ms,
                ..
            } => {
                let prev_escape = self
                    .inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .default_editor_on_escape
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                self.inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .retry_escape_handler = prev_escape;
                let sess = self.session();
                self.set_default_editor_escape(Some(Arc::new(move || sess.abort_retry())));
                self.show_status_indicator(StatusIndicator::retry(
                    self.ui(),
                    *attempt as i64,
                    *max_attempts as i64,
                    *delay_ms as i64,
                ));
                self.ui().request_render(None);
            }

            AgentSessionEvent::AutoRetryEnd {
                success,
                attempt,
                final_error,
            } => {
                let prev_escape = self
                    .inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .retry_escape_handler
                    .take();
                if let Some(handler) = prev_escape {
                    self.set_default_editor_escape(Some(handler));
                }
                self.clear_status_indicator(Some(StatusIndicatorKind::Retry));
                if !success {
                    let err_text = final_error
                        .as_deref()
                        .filter(|s| !s.is_empty())
                        .unwrap_or("Unknown error");
                    self.show_error(&format!("Retry failed after {attempt} attempts: {err_text}"));
                }
                self.ui().request_render(None);
            }

            AgentSessionEvent::SummarizationRetryScheduled {
                attempt,
                max_attempts,
                delay_ms,
                error_message,
            } => {
                self.show_error(error_message);
                self.show_status_indicator(StatusIndicator::retry(
                    self.ui(),
                    *attempt as i64,
                    *max_attempts as i64,
                    *delay_ms as i64,
                ));
                self.ui().request_render(None);
            }

            AgentSessionEvent::SummarizationRetryAttemptStart { source, reason } => {
                self.clear_status_indicator(Some(StatusIndicatorKind::Retry));
                match source {
                    SummarizationRetrySource::BranchSummary => {
                        self.show_status_indicator(StatusIndicator::branch_summary(self.ui()));
                    }
                    SummarizationRetrySource::Compaction => {
                        let color_fn = if self.active_editor().embed_working_status() {
                            let fallback = theme().get_thinking_border_color_by_name(thinking_level_name(
                                self.session().model_thinking_level(),
                            ));
                            Some(self.active_editor().border_color().unwrap_or(fallback))
                        } else {
                            None
                        };
                        let r = to_compaction_status_reason(reason.unwrap_or(CompactionReason::Manual));
                        self.show_status_indicator(StatusIndicator::compaction(self.ui(), r, color_fn));
                    }
                }
                self.ui().request_render(None);
            }

            AgentSessionEvent::SummarizationRetryFinished => {
                self.clear_status_indicator(Some(StatusIndicatorKind::Retry));
                self.ui().request_render(None);
            }
        }
    }

    fn get_user_message_text(&self, message: &AgentMessage) -> String {
        let AgentMessage::Message(Message::User(user)) = message else {
            return String::new();
        };
        match &user.content {
            UserMessageContent::Text(s) => s.clone(),
            UserMessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    UserContentBlock::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(""),
        }
    }

    pub fn show_managed_tool_status(&self, status: &ToolStatus) {
        {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if !st.managed_tool_status_started {
                st.managed_tool_status_started = true;
                drop(st);
                self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
            }
        }
        let message = if status.is_warning {
            format!("Warning: {}", status.message)
        } else {
            status.message.clone()
        };
        let color = if status.is_warning {
            ThemeColor::Warning
        } else {
            ThemeColor::Dim
        };
        self.add_chat_component(
            Arc::new(ThemedText::new(
                Arc::new(move || theme().fg(color, &message)),
                Some(1),
                Some(0),
            )),
            None,
            None,
            None,
        );
        {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            st.last_status_spacer = None;
            st.last_status_text = None;
            st.last_status_text_component = None;
        }
        self.ui().request_render(None);
    }

    pub fn show_status(&self, message: &str) {
        let can_reuse = {
            let st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let children = self.inner.chat_container.children();
            if children.len() >= 2
                && let (Some(last_comp), Some(last_spacer), Some(last_text)) = (
                    &st.last_status_text_component,
                    &st.last_status_spacer,
                    &st.last_status_text,
                )
            {
                let last = &children[children.len() - 1];
                let second_last = &children[children.len() - 2];
                if Arc::ptr_eq(last, last_comp) && Arc::ptr_eq(second_last, last_spacer) {
                    Some(last_text.clone())
                } else {
                    None
                }
            } else {
                None
            }
        };

        if let Some(prev_text) = can_reuse {
            {
                let st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                *st.last_status_message.lock().unwrap_or_else(|e| e.into_inner()) = message.to_string();
            }
            prev_text.invalidate();
            self.ui().request_render(None);
            return;
        }

        let spacer: Arc<dyn Component> = Arc::new(Spacer::new(Some(1)));
        let msg_slot = {
            let st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            *st.last_status_message.lock().unwrap_or_else(|e| e.into_inner()) = message.to_string();
            Arc::clone(&st.last_status_message)
        };
        let text = ThemedText::new(
            Arc::new(move || {
                let s = msg_slot.lock().unwrap_or_else(|e| e.into_inner()).clone();
                theme().fg(ThemeColor::Dim, &s)
            }),
            Some(1),
            Some(0),
        );
        let text_arc: Arc<dyn Component> = Arc::new(text.clone());
        self.add_chat_component(Arc::clone(&spacer), None, None, None);
        self.add_chat_component(Arc::clone(&text_arc), None, None, None);
        {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            st.last_status_spacer = Some(spacer);
            st.last_status_text = Some(text);
            st.last_status_text_component = Some(text_arc);
        }
        self.ui().request_render(None);
    }

    pub(crate) fn add_custom_entry_to_chat(&self, entry: &CustomEntry) {
        let Some(renderer) = self.session().extension_runner().get_entry_renderer(&entry.custom_type) else {
            return;
        };
        let entry_comp = CustomEntryComponent::new(entry.clone(), renderer);
        let tool_expanded = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .tool_output_expanded;
        entry_comp.set_expanded(tool_expanded);
        if !entry_comp.has_content() {
            return;
        }
        let comp_arc: Arc<dyn Component> = Arc::new(entry_comp.clone());
        let exp: Arc<dyn Expandable> = Arc::new(CustomEntryExpandable(entry_comp));
        let streaming_arc = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .streaming_component_arc
            .clone();
        if let Some(before) = streaming_arc {
            self.insert_chat_component_before(comp_arc, &before, Some(exp), None);
        } else {
            self.add_chat_component(comp_arc, Some(exp), None, None);
        }
    }

    pub fn add_message_to_chat(&self, message: &AgentMessage, populate_history: bool) {
        let (tool_output_expanded, hide_thinking_block, hidden_thinking_label, output_pad) = {
            let st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            (
                st.tool_output_expanded,
                st.hide_thinking_block,
                st.hidden_thinking_label.clone(),
                st.output_pad,
            )
        };

        match message {
            AgentMessage::Message(Message::User(_)) => {
                let text_content = self.get_user_message_text(message);
                if text_content.is_empty() {
                    return;
                }
                if !self.inner.chat_container.children().is_empty() {
                    self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
                }
                if let Some(skill_block) = parse_skill_block(&text_content) {
                    let user_after = skill_block.user_message.clone();
                    let skill_comp = SkillInvocationMessageComponent::new(
                        skill_block,
                        Some(self.get_markdown_theme_with_settings()),
                    );
                    skill_comp.set_output_pad(output_pad);
                    skill_comp.set_expanded(tool_output_expanded);
                    let skill_arc: Arc<dyn Component> = Arc::new(skill_comp.clone());
                    let exp: Arc<dyn Expandable> = Arc::new(SkillInvocationExpandable(skill_comp.clone()));
                    let pad_target = skill_comp;
                    let pad_fn: Arc<dyn Fn(i64) + Send + Sync> = Arc::new(move |p| pad_target.set_output_pad(p));
                    self.add_chat_component(skill_arc, Some(exp), None, Some(pad_fn));

                    if let Some(user_text) = user_after {
                        self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
                        let user_comp = UserMessageComponent::new(
                            &user_text,
                            Some(self.get_markdown_theme_with_settings()),
                            Some(output_pad),
                            Some(self.get_markdown_transformers()),
                        );
                        let user_arc: Arc<dyn Component> = Arc::new(user_comp.clone());
                        let pad_fn: Arc<dyn Fn(i64) + Send + Sync> = Arc::new(move |p| user_comp.set_output_pad(p));
                        self.add_chat_component(user_arc, None, None, Some(pad_fn));
                    }
                } else {
                    let user_comp = UserMessageComponent::new(
                        &text_content,
                        Some(self.get_markdown_theme_with_settings()),
                        Some(output_pad),
                        Some(self.get_markdown_transformers()),
                    );
                    let user_arc: Arc<dyn Component> = Arc::new(user_comp.clone());
                    let pad_fn: Arc<dyn Fn(i64) + Send + Sync> = Arc::new(move |p| user_comp.set_output_pad(p));
                    self.add_chat_component(user_arc, None, None, Some(pad_fn));
                }
                if populate_history {
                    self.active_editor().add_to_history(&text_content);
                }
            }
            AgentMessage::Message(Message::Assistant(assistant)) => {
                let comp = AssistantMessageComponent::new(
                    Some(assistant.clone()),
                    Some(hide_thinking_block),
                    Some(self.get_markdown_theme_with_settings()),
                    Some(&hidden_thinking_label),
                    Some(output_pad),
                    Some(self.get_markdown_transformers()),
                );
                let comp_arc: Arc<dyn Component> = Arc::new(comp.clone());
                let pad_comp = comp.clone();
                let pad_fn: Arc<dyn Fn(i64) + Send + Sync> = Arc::new(move |p| pad_comp.set_output_pad(p));
                self.add_chat_component(comp_arc, None, Some(comp), Some(pad_fn));
            }
            AgentMessage::Message(Message::System(_)) | AgentMessage::Message(Message::ToolResult(_)) => {}
            AgentMessage::Custom(val) => match val.get("role").and_then(Value::as_str) {
                Some("bashExecution") => {
                    let command = val.get("command").and_then(Value::as_str).unwrap_or("");
                    let exclude = val.get("excludeFromContext").and_then(Value::as_bool).unwrap_or(false);
                    let comp = BashExecutionComponent::new(command, self.ui(), Some(exclude), Some(output_pad));
                    if let Some(out) = val.get("output").and_then(Value::as_str)
                        && !out.is_empty()
                    {
                        comp.append_output(out);
                    }
                    let exit_code = val.get("exitCode").and_then(Value::as_i64);
                    let cancelled = val.get("cancelled").and_then(Value::as_bool).unwrap_or(false);
                    let truncated = val.get("truncated").and_then(Value::as_bool).unwrap_or(false);
                    let trunc_arg = if truncated {
                        Some(TruncationResult {
                            content: String::new(),
                            truncated: true,
                            truncated_by: None,
                            total_lines: 0,
                            total_bytes: 0,
                            output_lines: 0,
                            output_bytes: 0,
                            last_line_partial: false,
                            first_line_exceeds_limit: false,
                            max_lines: 0,
                            max_bytes: 0,
                        })
                    } else {
                        None
                    };
                    let full_path = val.get("fullOutputPath").and_then(Value::as_str);
                    comp.set_complete(exit_code, cancelled, trunc_arg, full_path);
                    let comp_arc: Arc<dyn Component> = Arc::new(comp.clone());
                    let exp_comp = comp.clone();
                    let exp_fn: Arc<dyn Expandable> = Arc::new(BashExecutionExpandable(exp_comp));
                    let pad_fn: Arc<dyn Fn(i64) + Send + Sync> = Arc::new(move |p| comp.set_output_pad(p));
                    self.add_chat_component(comp_arc, Some(exp_fn), None, Some(pad_fn));
                }
                Some("custom") => {
                    if let Ok(custom_msg) = serde_json::from_value::<CustomMessage>(val.clone())
                        && custom_msg.display
                    {
                        let renderer = self
                            .session()
                            .extension_runner()
                            .get_message_renderer(&custom_msg.custom_type);
                        let comp = CustomMessageComponent::new(
                            custom_msg,
                            renderer,
                            Some(self.get_markdown_theme_with_settings()),
                            Some(output_pad),
                        );
                        comp.set_expanded(tool_output_expanded);
                        let comp_arc: Arc<dyn Component> = Arc::new(comp.clone());
                        let exp: Arc<dyn Expandable> = Arc::new(CustomMessageExpandable(comp.clone()));
                        let pad_fn: Arc<dyn Fn(i64) + Send + Sync> = Arc::new(move |p| comp.set_output_pad(p));
                        self.add_chat_component(comp_arc, Some(exp), None, Some(pad_fn));
                    }
                }
                Some("compactionSummary") => {
                    if let Ok(summary_msg) = serde_json::from_value::<CompactionSummaryMessage>(val.clone()) {
                        self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
                        let comp = CompactionSummaryMessageComponent::new(
                            summary_msg,
                            Some(self.get_markdown_theme_with_settings()),
                        );
                        comp.set_output_pad(output_pad);
                        comp.set_expanded(tool_output_expanded);
                        let comp_arc: Arc<dyn Component> = Arc::new(comp.clone());
                        let exp: Arc<dyn Expandable> = Arc::new(CompactionSummaryExpandable(comp.clone()));
                        let pad_fn: Arc<dyn Fn(i64) + Send + Sync> = Arc::new(move |p| comp.set_output_pad(p));
                        self.add_chat_component(comp_arc, Some(exp), None, Some(pad_fn));
                    }
                }
                Some("branchSummary") => {
                    if let Ok(summary_msg) = serde_json::from_value::<BranchSummaryMessage>(val.clone()) {
                        self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
                        let comp = BranchSummaryMessageComponent::new(
                            summary_msg,
                            Some(self.get_markdown_theme_with_settings()),
                        );
                        comp.set_output_pad(output_pad);
                        comp.set_expanded(tool_output_expanded);
                        let comp_arc: Arc<dyn Component> = Arc::new(comp.clone());
                        let exp: Arc<dyn Expandable> = Arc::new(BranchSummaryExpandable(comp.clone()));
                        let pad_fn: Arc<dyn Fn(i64) + Send + Sync> = Arc::new(move |p| comp.set_output_pad(p));
                        self.add_chat_component(comp_arc, Some(exp), None, Some(pad_fn));
                    }
                }
                _ => {}
            },
        }
    }

    fn render_session_items(&self, items: Vec<RenderSessionItemLocal>, update_footer: bool, populate_history: bool) {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending_tool_components
            .clear();
        let mut rendered_pending_tools: IndexMap<String, ToolExecutionComponent> = IndexMap::new();
        let cache_misses = if self.settings_manager().get_show_cache_miss_notices() {
            let entries = self.session_manager().get_entries();
            collect_cache_misses_by_id(&entries, self.session().model_runtime())
        } else {
            IndexMap::new()
        };

        if update_footer {
            self.inner.footer.invalidate();
            self.update_editor_border_color();
        }

        let (tool_output_expanded, output_pad) = {
            let st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            (st.tool_output_expanded, st.output_pad)
        };

        for item in items {
            match item {
                RenderSessionItemLocal::Custom(c) => {
                    self.add_custom_entry_to_chat(&c);
                }
                RenderSessionItemLocal::Usage(u) => {
                    self.add_cache_warming_usage(&u);
                }
                RenderSessionItemLocal::CompactionCost(notice) => {
                    self.add_compaction_cost_notice(&notice);
                }
                RenderSessionItemLocal::Message { msg, entry_id } => match &msg {
                    AgentMessage::Message(Message::Assistant(assistant)) => {
                        self.add_message_to_chat(&msg, false);
                        let settings = self.settings_manager();
                        for block in &assistant.content {
                            if let AssistantContentBlock::ToolCall(tc) = block {
                                let tool_comp = ToolExecutionComponent::new(
                                    &tc.name,
                                    &tc.id,
                                    json_object_to_value(&tc.arguments),
                                    Some(ToolExecutionOptions {
                                        show_images: Some(settings.get_show_images()),
                                        image_width_cells: settings.get_image_width_cells(),
                                        output_pad: Some(output_pad),
                                    }),
                                    self.session().get_tool_renderers(&tc.name),
                                    self.ui(),
                                    &self.session_manager().get_cwd(),
                                );
                                tool_comp.set_expanded(tool_output_expanded);
                                self.add_tool_chat_component(tool_comp.clone());

                                if assistant.stop_reason == StopReason::Aborted
                                    || assistant.stop_reason == StopReason::Error
                                {
                                    let err_msg = if assistant.stop_reason == StopReason::Aborted {
                                        let retry_attempt = self.session().retry_attempt();
                                        if retry_attempt > 0 {
                                            format!(
                                                "Aborted after {retry_attempt} retry attempt{}",
                                                if retry_attempt > 1 { "s" } else { "" }
                                            )
                                        } else {
                                            "Operation aborted".to_string()
                                        }
                                    } else {
                                        assistant
                                            .error_message
                                            .clone()
                                            .filter(|s| !s.is_empty())
                                            .unwrap_or_else(|| "Error".to_string())
                                    };
                                    tool_comp.update_result(
                                        ToolExecutionResult {
                                            content: vec![UserContentBlock::Text(pi_ai::TextContent::new(err_msg))],
                                            details: None,
                                            is_error: true,
                                            duration_ms: None,
                                        },
                                        None,
                                    );
                                } else {
                                    rendered_pending_tools.insert(tc.id.clone(), tool_comp);
                                }
                            }
                        }
                        if assistant.stop_reason != StopReason::Aborted
                            && assistant.stop_reason != StopReason::Error
                            && let Some(miss) = cache_misses.get(&entry_id)
                        {
                            self.add_cache_miss_notice(miss);
                        }
                    }
                    AgentMessage::Message(Message::ToolResult(tr)) => {
                        if let Some(comp) = rendered_pending_tools.shift_remove(&tr.tool_call_id) {
                            comp.update_result(
                                ToolExecutionResult {
                                    content: tr.content.clone(),
                                    details: tr.details.clone(),
                                    is_error: tr.is_error,
                                    duration_ms: None,
                                },
                                None,
                            );
                        }
                    }
                    _ => {
                        self.add_message_to_chat(&msg, populate_history);
                    }
                },
            }
        }

        {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            for (id, comp) in rendered_pending_tools {
                st.pending_tool_components.insert(id, comp);
            }
        }
        self.ui().request_render(None);
    }

    pub fn render_session_entries(&self, entries: &[SessionEntry], update_footer: bool, populate_history: bool) {
        self.ui().reset_text_selection();
        let mut items: Vec<RenderSessionItemLocal> = Vec::new();
        for entry in entries {
            match entry {
                SessionEntry::Custom(c) => items.push(RenderSessionItemLocal::Custom(c.clone())),
                SessionEntry::Usage(u) if u.kind == "cache_warm" => {
                    items.push(RenderSessionItemLocal::Usage(u.clone()));
                }
                _ => {
                    let messages = session_entry_to_context_messages(entry);
                    let has_msgs = !messages.is_empty();
                    let entry_id = entry.id().to_string();
                    for msg in messages {
                        items.push(RenderSessionItemLocal::Message {
                            msg,
                            entry_id: entry_id.clone(),
                        });
                    }
                    if has_msgs {
                        match entry {
                            SessionEntry::Compaction(c) => {
                                if let Some(usage) = c.usage {
                                    items.push(RenderSessionItemLocal::CompactionCost(CompactionCostNotice {
                                        kind: CompactionCostKind::Compaction,
                                        usage,
                                    }));
                                }
                            }
                            SessionEntry::BranchSummary(b) => {
                                if let Some(usage) = b.usage {
                                    items.push(RenderSessionItemLocal::CompactionCost(CompactionCostNotice {
                                        kind: CompactionCostKind::BranchSummary,
                                        usage,
                                    }));
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        self.render_session_items(items, update_footer, populate_history);
    }

    fn add_cache_warming_usage(&self, entry: &UsageEntry) {
        if !self.settings_manager().get_show_cache_miss_notices() {
            return;
        }
        let previous = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cache_warming_notice
            .clone();
        if let Some(prev) = previous {
            let children = self.inner.chat_container.children();
            if let Some(last) = children.last()
                && prev.note == entry.note
            {
                let last_text = last.render(80);
                let prev_text = prev.component.render(80);
                if last_text == prev_text {
                    *prev.total_cost.lock().unwrap_or_else(|e| e.into_inner()) += entry.usage.cost.total;
                    prev.component.invalidate();
                    return;
                }
            }
        }
        let total_cost = Arc::new(std::sync::Mutex::new(entry.usage.cost.total));
        let cost_ref = Arc::clone(&total_cost);
        let template_entry = entry.clone();
        let comp = ThemedText::new(
            Arc::new(move || {
                let c = *cost_ref.lock().unwrap_or_else(|e| e.into_inner());
                let mut e = template_entry.clone();
                e.usage.cost.total = c;
                theme().fg(ThemeColor::Dim, &format_cache_warming_usage(&e))
            }),
            Some(1),
            Some(0),
        );
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cache_warming_notice = Some(CacheWarmingNoticeEntry {
            component: comp.clone(),
            total_cost,
            note: entry.note.clone(),
        });
        self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
        self.add_chat_component(Arc::new(comp), None, None, None);
    }

    fn add_compaction_cost_notice(&self, notice: &CompactionCostNotice) {
        if !self.settings_manager().get_show_cache_miss_notices() {
            return;
        }
        let usage = notice.usage;
        let tokens = usage.input + usage.output + usage.cache_read + usage.cache_write;
        let cost = if usage.cost.total >= 0.01 {
            format!(" (~${})", pi_js::num::to_fixed(usage.cost.total, 2))
        } else {
            String::new()
        };
        let label = match notice.kind {
            CompactionCostKind::Compaction => "Compaction",
            CompactionCostKind::BranchSummary => "Branch summary",
        };
        let text = format!(
            "{label}: {} tokens billed{cost}",
            crate::modes::interactive::components::footer::format_tokens(tokens)
        );
        self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
        self.add_chat_component(
            Arc::new(ThemedText::new(
                Arc::new(move || theme().fg(ThemeColor::Warning, &text)),
                Some(1),
                Some(0),
            )),
            None,
            None,
            None,
        );
    }

    pub fn count_dropped_thinking_blocks(message: &AssistantMessage) -> usize {
        let mut count = 0usize;
        for diagnostic in message.diagnostics.as_deref().unwrap_or(&[]) {
            if diagnostic.r#type != "anthropic_input_transformations" {
                continue;
            }
            if let Some(details) = &diagnostic.details
                && let Some(Value::Array(transformations)) = details.get("transformations")
            {
                count += transformations
                    .iter()
                    .filter(|t| {
                        t.as_object().and_then(|o| o.get("type")).and_then(Value::as_str) == Some("thinking_dropped")
                    })
                    .count();
            }
        }
        count
    }

    pub fn maybe_show_thinking_drop_notice(&self, message: &AssistantMessage) {
        if !self.settings_manager().get_show_cache_miss_notices() {
            return;
        }
        let dropped_count = Self::count_dropped_thinking_blocks(message);
        if dropped_count == 0 {
            return;
        }
        let mut previous_dropped_count = 0usize;
        let branch = self.session_manager().get_branch();
        for entry in branch.iter().rev() {
            if let SessionEntry::Message(me) = entry
                && let AgentMessage::Message(Message::Assistant(prev_msg)) = &me.message
            {
                previous_dropped_count = Self::count_dropped_thinking_blocks(prev_msg);
                break;
            }
        }
        if dropped_count <= previous_dropped_count {
            return;
        }
        let noun = if dropped_count == 1 {
            "thinking block"
        } else {
            "thinking blocks"
        };
        let msg = format!("Anthropic dropped {dropped_count} {noun} (details in session)");
        self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
        self.add_chat_component(
            Arc::new(ThemedText::new(
                Arc::new(move || theme().fg(ThemeColor::Warning, &msg)),
                Some(1),
                Some(0),
            )),
            None,
            None,
            None,
        );
    }

    fn maybe_show_cache_miss_notice(&self, message: &AssistantMessage) {
        if !self.settings_manager().get_show_cache_miss_notices() {
            return;
        }
        if let Some(miss) = self.session().detect_cache_miss(message) {
            self.add_cache_miss_notice(&miss);
        }
    }

    fn add_cache_miss_notice(&self, miss: &CacheMissNotice) {
        if miss.missed_tokens < 20_000 && miss.missed_cost < 0.1 {
            return;
        }
        let cost = if miss.missed_cost >= 0.01 {
            format!(" (~${:.2})", miss.missed_cost)
        } else {
            String::new()
        };
        let re_billed = format!(
            "{} tokens re-billed{cost}",
            crate::modes::interactive::components::footer::format_tokens(miss.missed_tokens)
        );
        let label = if miss.model_changed {
            "Cache miss after model switch".to_string()
        } else if miss.idle_ms >= CACHE_TTL_MS {
            format!(
                "Cache miss after {}m idle",
                ((miss.idle_ms as f64) / 60_000.0).round() as i64
            )
        } else {
            "Cache miss".to_string()
        };
        let full = format!("{label}: {re_billed}");
        self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
        self.add_chat_component(
            Arc::new(ThemedText::new(
                Arc::new(move || theme().fg(ThemeColor::Warning, &full)),
                Some(1),
                Some(0),
            )),
            None,
            None,
            None,
        );
    }

    pub fn render_initial_messages(&self) {
        let entries = self.session_manager().build_context_entries();
        self.render_session_entries(&entries, true, true);
        self.render_project_trust_warning_if_needed();

        let all_entries = self.session_manager().get_entries();
        let compaction_count = all_entries
            .iter()
            .filter(|e| matches!(e, SessionEntry::Compaction(_)))
            .count();
        if compaction_count > 0 {
            let times = if compaction_count == 1 {
                "1 time".to_string()
            } else {
                format!("{compaction_count} times")
            };
            self.show_status(&format!("Session compacted {times}"));
        }
    }

    pub(crate) fn render_project_trust_warning_if_needed(&self) {
        if self.settings_manager().is_project_trusted()
            || !has_trust_requiring_project_resources(&self.session_manager().get_cwd())
        {
            return;
        }
        if !self.inner.chat_container.children().is_empty() {
            self.add_chat_component(Arc::new(Spacer::new(Some(1))), None, None, None);
        }
        let msg = format!(
            "This project is not trusted. Project {} resources and packages are ignored. Use /trust to save a trust decision, then restart pi.",
            *CONFIG_DIR_NAME
        );
        self.add_chat_component(
            Arc::new(ThemedText::new(
                Arc::new(move || theme().fg(ThemeColor::Warning, &msg)),
                Some(1),
                Some(0),
            )),
            None,
            None,
            None,
        );
    }

    pub async fn get_user_input(&self) -> String {
        self.get_user_input_or_shutdown().await.unwrap_or_default()
    }

    pub(crate) fn rebuild_chat_from_messages(&self) {
        self.clear_chat_container();
        let entries = self.session_manager().build_context_entries();
        self.render_session_entries(&entries, false, false);
    }

    // =========================================================================
    // Lifecycle & Key Handlers
    // =========================================================================

    pub(crate) fn handle_ctrl_c(&self) {
        let now = chrono::Utc::now().timestamp_millis() as f64;
        let should_shutdown = {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if now - st.last_sigint_time_ms < 500.0 {
                true
            } else {
                st.last_sigint_time_ms = now;
                false
            }
        };
        if should_shutdown {
            let m = self.clone();
            tokio::spawn(async move { m.shutdown(false).await });
        } else {
            self.clear_editor();
        }
    }

    pub(crate) fn handle_ctrl_d(&self) {
        let m = self.clone();
        tokio::spawn(async move { m.shutdown(false).await });
    }

    pub(crate) async fn shutdown(&self, from_signal: bool) {
        {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if st.is_shutting_down {
                return;
            }
            st.is_shutting_down = true;
            st.on_input_callback.take();
        }
        self.inner.shutdown_notify.notify_waiters();

        if from_signal {
            self.inner.runtime_host.dispose().await;
            self.inner.theme_controller.disable_auto_sync();
            let _ = self.ui().terminal().drain_input(Some(1000), None).await;
            let output = self.settings_manager().get_fullscreen_exit_output();
            self.stop(Some(output));
            (self.inner.hooks.exit_process)(0);
            return;
        }

        self.inner.theme_controller.disable_auto_sync();
        let _ = self.ui().terminal().drain_input(Some(1000), None).await;
        let output = self.settings_manager().get_fullscreen_exit_output();
        self.stop(Some(output));
        self.inner.runtime_host.dispose().await;

        if let Some(resume_cmd) = format_resume_command(self.session_manager().as_ref()) {
            pi_js::console::log(&format!("\x1b[2mTo resume this session:\x1b[22m {resume_cmd}"));
        }

        (self.inner.hooks.exit_process)(0);
    }

    pub(crate) async fn check_shutdown_requested(&self) {
        let requested = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .shutdown_requested;
        if requested {
            self.shutdown(false).await;
        }
    }

    pub(crate) fn register_signal_handlers(&self) {
        self.unregister_signal_handlers();
        #[cfg(unix)]
        {
            // WIRE(@tui-w3): Hook SIGTERM / SIGHUP into tokio::signal::unix -> shutdown(true) once wired into CLI binary.
        }
    }

    pub(crate) fn unregister_signal_handlers(&self) {}

    pub fn handle_ctrl_z(&self) {
        let is_windows = self.inner.hooks.platform_is_windows.unwrap_or(cfg!(not(unix)));
        if is_windows {
            self.show_status("Suspend to background is not supported on Windows");
            return;
        }
        if let Some(ref suspend_fn) = self.inner.hooks.suspend_process {
            let m = self.clone();
            let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let cancelled_cb = Arc::clone(&cancelled);
            let on_sigcont: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                if !cancelled_cb.load(std::sync::atomic::Ordering::SeqCst) {
                    m.ui().start();
                    m.ui().request_render(Some(true));
                }
            });
            self.ui().stop(None);
            if let Err(err) = suspend_fn(on_sigcont) {
                cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
                panic!("{err}");
            }
            return;
        }
        #[cfg(unix)]
        {
            let m = self.clone();
            if let Ok(mut sigcont) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::from_raw(nix::libc::SIGCONT))
            {
                tokio::spawn(async move {
                    sigcont.recv().await;
                    m.ui().start();
                    m.ui().request_render(Some(true));
                });
            }
            self.ui().stop(None);
            let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(0), nix::sys::signal::Signal::SIGTSTP);
        }
    }

    pub async fn handle_follow_up(&self) {
        let text = self.active_editor().get_expanded_text().trim().to_string();
        if text.is_empty() {
            return;
        }
        let sess = self.session();
        if sess.is_compacting() {
            if self.is_extension_command(&text) {
                self.active_editor().add_to_history(&text);
                self.active_editor().set_text("");
                let _ = sess.prompt(&text, None).await;
            } else {
                self.queue_compaction_message(&text, CompactionQueueMode::FollowUp);
            }
            return;
        }

        if sess.is_streaming() {
            self.active_editor().add_to_history(&text);
            self.active_editor().set_text("");
            let _ = sess
                .prompt(
                    &text,
                    Some(InteractivePromptOptions {
                        images: None,
                        streaming_behavior: Some(CompactionQueueMode::FollowUp),
                    }),
                )
                .await;
            self.update_pending_messages_display();
            self.ui().request_render(None);
        } else if let Some(on_submit) = self.active_editor().on_submit() {
            self.active_editor().set_text("");
            on_submit(&text);
        }
    }

    pub fn handle_dequeue(&self) {
        let restored = self.restore_queued_messages_to_editor(false);
        if restored == 0 {
            self.show_status("No queued messages to restore");
        } else {
            let plural = if restored > 1 { "s" } else { "" };
            self.show_status(&format!("Restored {restored} queued message{plural} to editor"));
        }
    }

    pub(crate) fn update_editor_border_color(&self) {
        let is_bash = self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).is_bash_mode;
        let border_color: EditorBorderColor = if is_bash {
            theme().get_bash_mode_border_color()
        } else {
            let level = self.session().model_thinking_level();
            theme().get_thinking_border_color_by_name(thinking_level_name(level))
        };
        self.active_editor().set_border_color(border_color);
        if let Some(ind) = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active_status_indicator
            .clone()
        {
            ind.invalidate();
        }
        self.ui().request_render(None);
    }

    pub fn cycle_thinking_level(&self) {
        match self.session().cycle_thinking_level() {
            None => self.show_status("Current model does not support thinking"),
            Some(new_level) => {
                self.inner.footer.invalidate();
                self.update_editor_border_color();
                self.show_status(&format!("Thinking level: {}", thinking_level_name(new_level)));
            }
        }
    }

    pub async fn cycle_model(&self, direction: CycleDirection) {
        match self.session().cycle_model(direction).await {
            Ok(None) => {
                let msg = if !self.session().scoped_models().is_empty() {
                    "Only one model in scope"
                } else {
                    "Only one model available"
                };
                self.show_status(msg);
            }
            Ok(Some(result)) => {
                self.inner.footer.invalidate();
                self.update_editor_border_color();
                let thinking_str = if result.model.reasoning && result.thinking_level != ModelThinkingLevel::Off {
                    format!(" (thinking: {})", thinking_level_name(result.thinking_level))
                } else {
                    String::new()
                };
                let label = if result.model.name.is_empty() {
                    result.model.id.as_str()
                } else {
                    result.model.name.as_str()
                };
                self.show_status(&format!("Switched to {label}{thinking_str}"));
                let m = self.clone();
                let model_for_warn = result.model.clone();
                tokio::spawn(async move {
                    m.maybe_warn_about_anthropic_subscription_auth(Some(&model_for_warn))
                        .await;
                });
            }
            Err(err) => {
                self.show_error(&err.to_string());
            }
        }
    }

    pub fn toggle_tool_output_expansion(&self) {
        let current = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .tool_output_expanded;
        self.set_tools_expanded(!current);
    }

    pub fn set_tools_expanded(&self, expanded: bool) {
        let expandables = {
            let mut st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if expanded == st.tool_output_expanded {
                return;
            }
            st.tool_output_expanded = expanded;
            let mut items: Vec<Arc<dyn Expandable>> = Vec::new();
            if st.custom_header.is_none()
                && let Some((_, Some(exp))) = &st.built_in_header
            {
                items.push(Arc::clone(exp));
            }
            for tracker in &st.resource_trackers {
                if let Some(exp) = &tracker.expandable {
                    items.push(Arc::clone(exp));
                }
            }
            for tracker in &st.chat_trackers {
                if let Some(exp) = &tracker.expandable {
                    items.push(Arc::clone(exp));
                }
            }
            for tracker in &st.pending_trackers {
                if let Some(exp) = &tracker.expandable {
                    items.push(Arc::clone(exp));
                }
            }
            items
        };

        for exp in expandables {
            exp.set_expanded(expanded);
        }
        let state_str = if expanded { "expanded" } else { "collapsed" };
        self.show_status(&format!("Tool output: {state_str}"));
    }
}

fn agent_tool_result_to_execution_result(
    result: &AgentToolResult,
    is_error: bool,
    duration_ms: Option<f64>,
) -> ToolExecutionResult {
    ToolExecutionResult {
        content: result.content.clone(),
        details: result.details.clone(),
        is_error,
        duration_ms,
    }
}
