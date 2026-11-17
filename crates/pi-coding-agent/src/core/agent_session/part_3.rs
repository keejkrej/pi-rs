//! Port of packages/coding-agent/src/core/agent-session.ts#L2667-L4348

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use indexmap::{IndexMap, IndexSet};
use pi_agent_core::types::{AgentMessage, AgentTool};
use pi_ai::compat::register_built_in_api_providers;
use pi_ai::types::{AssistantMessage, Model, StopReason, Usage};
use pi_ai::utils::retry::{RetryCallbacks, RetryPolicy};
use pi_ai::{content_text, is_context_overflow, is_recoverable_length, is_retryable_assistant_error, retry_delay_ms};
use pi_js::abort::{AbortController, AbortSignal};
use serde_json::Value;

use crate::core::auth_guidance::format_no_model_selected_message;
use crate::core::bash_executor::{BashOperations, BashResult, execute_bash_with_operations};
use crate::core::bug_report::{GenerateBugReportSummaryOptions, generate_bug_report_summary};
use crate::core::compaction::branch_summarization::{
    BranchSummaryDetails, GenerateBranchSummaryOptions, collect_entries_for_branch_summary, generate_branch_summary,
};
use crate::core::compaction::compaction::{
    CompactOptions, CompactionPreparation, CompactionResult, calculate_context_tokens, compact as run_compact,
    estimate_context_tokens, estimate_projected_context_tokens, prepare_compaction, should_compact,
};
use crate::core::defaults::{from_agent_thinking_level, to_agent_thinking_level};
use crate::core::export_html::{ExportOptions, export_session_to_html};
use crate::core::extensions::runner::{
    DiscoveredResourcePathEntry, ExtensionRunner, ProviderActions, emit_session_shutdown_event,
};
use crate::core::extensions::types::{
    CompactionReason as ExtCompactionReason, ContextUsage, DeliverAs, ExtensionActions, ExtensionContextActions,
    ExtensionError, FlagValue, InputDeliveryBehavior, ReplacedSessionContext, ResourcesDiscoverReason,
    SessionBeforeCompactEvent, SessionBeforeTreeEvent, SessionCompactEvent, SessionInfoChangedEvent,
    SessionShutdownEvent, SessionShutdownReason, SessionStartEvent, SessionStartReason, SessionTreeEvent,
    SlashCommandInfo, SlashCommandSource, ToolDefinition, TreePreparation,
};
use crate::core::extensions::wrapper::wrap_registered_tools;
use crate::core::messages::BashExecutionMessage;
use crate::core::model_registry::ModelRegistry;
use crate::core::package_manager::PathMetadata;
use crate::core::resource_loader::{ExtensionResourcePath, ResourceExtensionPaths};
use crate::core::session_export::export_session_to_jsonl;
use crate::core::session_manager::{BranchSummaryEntry, SessionEntry, get_latest_compaction_entry};
use crate::core::settings_manager::DEFAULT_TOOL_NAMES;
use crate::core::source_info::{
    BUILTIN_PATH_PREFIX, SourceInfo, SourceOrigin, SourceScope, SyntheticSourceOptions, create_synthetic_source_info,
    is_synthetic_path,
};
use crate::core::tools::bash::create_local_bash_operations_with_shell;
use crate::core::tools::tool_definition_wrapper::create_tool_definition_from_agent_tool;
use crate::core::tools::{ToolsOptions, create_all_tool_definitions};
use crate::core::usage_totals::{add_usage_to_totals, create_usage_totals};
use crate::utils::sleep::sleep;

use super::*;

fn to_retry_policy(rs: crate::core::settings_manager::ResolvedRetrySettings) -> RetryPolicy {
    RetryPolicy {
        enabled: rs.enabled,
        max_retries: u32::try_from(rs.max_retries).unwrap_or(0),
        base_delay_ms: u64::try_from(rs.base_delay_ms).unwrap_or(0),
        max_agent_delay_ms: u64::try_from(rs.max_agent_delay_ms).ok(),
    }
}

fn to_ext_compaction_reason(reason: CompactionReason) -> ExtCompactionReason {
    match reason {
        CompactionReason::Manual => ExtCompactionReason::Manual,
        CompactionReason::Threshold => ExtCompactionReason::Threshold,
        CompactionReason::Overflow => ExtCompactionReason::Overflow,
    }
}

fn entry_timestamp_ms(timestamp_iso: &str) -> Option<i64> {
    pi_js::time::parse_date(timestamp_iso)
}

struct SessionSummarizationRetryCallbacks {
    weak: Weak<AgentSessionInner>,
    source: SummarizationRetrySource,
    reason: Option<CompactionReason>,
}

#[async_trait]
impl RetryCallbacks for SessionSummarizationRetryCallbacks {
    async fn on_retry_scheduled(&self, attempt: u32, max_attempts: u32, delay_ms: u64, error_message: &str) {
        if let Some(inner) = self.weak.upgrade() {
            AgentSession { inner }._emit(AgentSessionEvent::SummarizationRetryScheduled {
                attempt,
                max_attempts,
                delay_ms,
                error_message: error_message.to_string(),
            });
        }
    }

    async fn on_retry_attempt_start(&self) {
        if let Some(inner) = self.weak.upgrade() {
            AgentSession { inner }._emit(AgentSessionEvent::SummarizationRetryAttemptStart {
                source: self.source,
                reason: self.reason,
            });
        }
    }

    async fn on_retry_finished(&self, _success: bool, _attempt: u32, _final_error: Option<&str>) {
        if let Some(inner) = self.weak.upgrade() {
            AgentSession { inner }._emit(AgentSessionEvent::SummarizationRetryFinished);
        }
    }
}

struct BranchSummaryControllerGuard {
    inner: Arc<AgentSessionInner>,
    armed: bool,
}

impl Drop for BranchSummaryControllerGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        {
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .branch_summary_abort_controller = None;
        }
        AgentSession {
            inner: Arc::clone(&self.inner),
        }
        ._resolve_idle_wait_if_idle();
    }
}

impl AgentSession {
    // =========================================================================
    // Compaction
    // =========================================================================

    async fn _run_default_compaction(
        &self,
        preparation: &CompactionPreparation,
        model: &Model,
        custom_instructions: Option<&str>,
        signal: &AbortSignal,
        reason: CompactionReason,
    ) -> pi_js::Result<CompactionResult> {
        let request = self
            ._get_summarization_request_auth(model, Some(signal.clone()))
            .await?;
        let retry = to_retry_policy(self.inner.settings_manager.get_retry_settings());
        let callbacks = self._summarization_retry_callbacks(SummarizationRetrySource::Compaction, Some(reason));
        let stream_fn = self.inner.agent.stream_fn();
        let headers = request
            .headers
            .map(|h| h.into_iter().map(|(k, v)| (k, Some(v))).collect());
        let res = run_compact(
            preparation,
            &request.model,
            Some(CompactOptions {
                api_key: request.api_key,
                headers,
                custom_instructions: custom_instructions.map(str::to_string),
                signal: Some(signal.clone()),
                thinking_level: Some(to_agent_thinking_level(request.thinking_level)),
                stream_fn,
                env: request.env,
                retry: Some(retry),
                callbacks: Some(callbacks),
                session_id: Some(self.session_id()),
            }),
        )
        .await?;
        Ok(CompactionResult {
            summary: res.summary,
            first_kept_entry_id: res.first_kept_entry_id,
            tokens_before: res.tokens_before,
            estimated_tokens_after: res.estimated_tokens_after,
            usage: res.usage,
            details: res.details.and_then(|d| serde_json::to_value(d).ok()),
        })
    }

    fn _clear_manual_compaction_state(&self) {
        {
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .compaction_abort_controller = None;
        }
        self._resolve_idle_wait_if_idle();
    }

    /// Manually compact the session context.
    pub async fn compact(&self, custom_instructions: Option<&str>) -> pi_js::Result<CompactionResult> {
        self.abort().await;
        let controller = Arc::new(AbortController::new());
        let signal = controller.signal();
        {
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .compaction_abort_controller = Some(controller);
        }
        self._emit(AgentSessionEvent::CompactionStart {
            reason: CompactionReason::Manual,
        });

        let mut from_extension = false;
        let mut cancelled_by_extension = false;

        let outcome: pi_js::Result<CompactionResult> = async {
            let Some(model) = self.model() else {
                return Err(pi_js::Error::msg(format_no_model_selected_message()));
            };
            let settings = resolved_compaction_settings(&self.inner.settings_manager, Some(&model));
            let path_entries = self.inner.session_manager.get_branch(None);
            let Some(preparation) = prepare_compaction(&path_entries, &settings) else {
                if let Some(SessionEntry::Compaction(_)) = path_entries.last() {
                    return Err(pi_js::Error::msg("Already compacted"));
                }
                return Err(pi_js::Error::msg("Nothing to compact (session too small)"));
            };

            let runner = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extension_runner
                .clone();
            let mut extension_compaction: Option<CompactionResult> = None;

            if let Some(ref r) = runner
                && r.has_handlers("session_before_compact")
            {
                let res = r
                    .emit_session_before_compact(SessionBeforeCompactEvent {
                        preparation: preparation.clone(),
                        branch_entries: path_entries,
                        custom_instructions: custom_instructions.map(str::to_string),
                        reason: to_ext_compaction_reason(CompactionReason::Manual),
                        will_retry: false,
                        signal: Some(signal.clone()),
                    })
                    .await;
                if let Some(res) = res {
                    if res.cancel == Some(true) {
                        cancelled_by_extension = true;
                        return Err(pi_js::Error::msg("Compaction cancelled"));
                    }
                    if let Some(compaction) = res.compaction {
                        extension_compaction = Some(compaction);
                        from_extension = true;
                    }
                }
            }

            let compact_res = match extension_compaction {
                Some(comp) => comp,
                None => {
                    self._run_default_compaction(
                        &preparation,
                        &model,
                        custom_instructions,
                        &signal,
                        CompactionReason::Manual,
                    )
                    .await?
                }
            };

            if signal.aborted() {
                return Err(pi_js::Error::msg("Compaction cancelled"));
            }

            let entry_id = self.inner.session_manager.append_compaction(
                &compact_res.summary,
                Some(&compact_res.first_kept_entry_id),
                compact_res.tokens_before,
                compact_res.details.clone(),
                Some(from_extension),
                compact_res.usage.clone(),
            )?;
            self._refresh_finalized_context();
            let estimated_tokens_after =
                estimate_context_tokens(&self.inner.session_manager.build_session_projection().messages).tokens;

            let saved_entry = self
                .inner
                .session_manager
                .get_entry(&entry_id)
                .and_then(|e| match e {
                    SessionEntry::Compaction(c) => Some(c),
                    _ => None,
                })
                .or_else(|| {
                    self.inner
                        .session_manager
                        .get_entries()
                        .into_iter()
                        .find_map(|e| match e {
                            SessionEntry::Compaction(c) if c.summary == compact_res.summary => Some(c),
                            _ => None,
                        })
                });

            if let (Some(r), Some(compaction_entry)) = (runner, saved_entry) {
                let _ = r
                    .emit(SessionCompactEvent {
                        compaction_entry,
                        from_extension,
                        reason: to_ext_compaction_reason(CompactionReason::Manual),
                        will_retry: false,
                    })
                    .await;
            }

            Ok(CompactionResult {
                summary: compact_res.summary,
                first_kept_entry_id: compact_res.first_kept_entry_id,
                tokens_before: compact_res.tokens_before,
                estimated_tokens_after: Some(estimated_tokens_after),
                usage: compact_res.usage,
                details: compact_res.details,
            })
        }
        .await;

        match outcome {
            Ok(result) => {
                self._clear_manual_compaction_state();
                self._emit(AgentSessionEvent::CompactionEnd {
                    reason: CompactionReason::Manual,
                    result: Some(result.clone()),
                    aborted: false,
                    will_retry: false,
                    error_message: None,
                });
                Ok(result)
            }
            Err(err) => {
                let aborted = signal.aborted() || cancelled_by_extension;
                let error_message = if aborted {
                    None
                } else {
                    Some(format!("Compaction failed: {err}"))
                };
                self._clear_manual_compaction_state();
                self._emit(AgentSessionEvent::CompactionEnd {
                    reason: CompactionReason::Manual,
                    result: None,
                    aborted,
                    will_retry: false,
                    error_message: error_message.clone(),
                });
                self._emit_session_compact_failed(
                    CompactionReason::Manual,
                    error_message,
                    aborted,
                    false,
                    from_extension,
                )
                .await;
                Err(err)
            }
        }
    }

    /// Cancel in-progress compaction (manual or auto).
    pub fn abort_compaction(&self) {
        let (manual, auto) = {
            let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            (
                guard.compaction_abort_controller.clone(),
                guard.auto_compaction_abort_controller.clone(),
            )
        };
        if let Some(c) = manual {
            c.abort(None);
        }
        if let Some(c) = auto {
            c.abort(None);
        }
    }

    /// Cancel in-progress branch summarization.
    pub fn abort_branch_summary(&self) {
        let controller = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .branch_summary_abort_controller
            .clone();
        if let Some(c) = controller {
            c.abort(None);
        }
    }

    pub(crate) async fn _check_compaction(
        &self,
        assistant_message: &AssistantMessage,
        skip_aborted_check: bool,
        tool_results: &[AgentMessage],
    ) -> bool {
        let model = self.model();
        let settings = resolved_compaction_settings(&self.inner.settings_manager, model.as_ref());
        if !settings.enabled {
            return false;
        }
        if skip_aborted_check && assistant_message.stop_reason == StopReason::Aborted {
            return false;
        }

        let message_model = self._model_for_message(assistant_message);
        let same_model = message_model.is_some();
        let context_window = message_model
            .as_ref()
            .or(model.as_ref())
            .map(|m| m.context_window)
            .unwrap_or(0);

        let branch = self.inner.session_manager.get_branch(None);
        let compaction_entry = get_latest_compaction_entry(&branch);
        if let Some(ref comp) = compaction_entry
            && let Some(comp_ts) = entry_timestamp_ms(&comp.timestamp)
            && assistant_message.timestamp <= comp_ts
        {
            return false;
        }

        let current_projection = self.inner.session_manager.build_session_projection();
        let assistant_agent_msg = AgentMessage::from(assistant_message.clone());
        let assistant_entry_id = self._find_persisted_message_entry_id(&assistant_agent_msg);
        let assistant_is_projected = match &assistant_entry_id {
            None => true,
            Some(eid) => current_projection.entries.iter().any(|entry| {
                entry.source_entry.id() == eid && entry.messages.iter().any(|m| m.role() == Some("assistant"))
            }),
        };

        let assistant_index = assistant_entry_id
            .as_ref()
            .and_then(|eid| branch.iter().position(|e| e.id() == eid));
        let entries_after = match assistant_index {
            Some(idx) => &branch[idx + 1..],
            None => &[],
        };
        let has_post_edit = entries_after.iter().any(|e| matches!(e, SessionEntry::ContextEdit(_)));
        let latest_assistant_edit = entries_after.iter().rev().find_map(|e| match e {
            SessionEntry::ContextEdit(ce) if assistant_entry_id.as_deref() == Some(ce.target_id.as_str()) => Some(ce),
            _ => None,
        });
        let assistant_retained_for_explicit_recovery = assistant_entry_id.is_none()
            || (!entries_after.iter().any(|e| matches!(e, SessionEntry::Compaction(_)))
                && latest_assistant_edit.is_none_or(|ce| ce.replacement.is_some()));
        let assistant_usage_matches_projection = assistant_is_projected && !has_post_edit;

        let explicit_overflow =
            assistant_message.stop_reason == StopReason::Error && is_context_overflow(assistant_message, None);
        let context_overflow = same_model
            && ((explicit_overflow && assistant_retained_for_explicit_recovery)
                || (assistant_usage_matches_projection
                    && is_context_overflow(assistant_message, Some(context_window))));
        let recoverable_length = same_model
            && assistant_is_projected
            && message_model
                .as_ref()
                .is_some_and(|m| is_recoverable_length(assistant_message, m.max_tokens));

        if context_overflow || recoverable_length {
            let will_retry = assistant_message.stop_reason != StopReason::Stop;
            if !will_retry {
                return self._run_auto_compaction(CompactionReason::Overflow, false).await;
            }

            let already_attempted = {
                let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                guard.overflow_recovery_attempted
            };
            if already_attempted {
                let error_message = if context_overflow {
                    "Context overflow recovery failed after one compact-and-retry attempt. Try reducing context or switching to a larger-context model."
                } else {
                    "Truncated response recovery failed after one compact-and-retry attempt."
                };
                self._emit(AgentSessionEvent::CompactionEnd {
                    reason: CompactionReason::Overflow,
                    result: None,
                    aborted: false,
                    will_retry: false,
                    error_message: Some(error_message.to_string()),
                });
                self._emit_session_compact_failed(
                    CompactionReason::Overflow,
                    Some(error_message.to_string()),
                    false,
                    false,
                    false,
                )
                .await;
                return false;
            }

            {
                self.inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .overflow_recovery_attempted = true;
            }
            let _ = self._omit_recovery_attempt(assistant_message, tool_results);
            let retry = self._run_auto_compaction(CompactionReason::Overflow, will_retry).await;
            if retry {
                self.inner
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .failed_response = Some(assistant_message.clone());
            }
            return retry;
        }

        let has_context_edits = current_projection
            .entries
            .iter()
            .any(|e| matches!(e.source_entry, SessionEntry::ContextEdit(_)));
        let direct_context_tokens = calculate_context_tokens(&assistant_message.usage);
        let context_tokens = if has_context_edits {
            estimate_projected_context_tokens(&current_projection, &branch).tokens
        } else if assistant_message.stop_reason == StopReason::Error || direct_context_tokens == 0 {
            let messages = self.inner.agent.state().messages;
            let estimate = estimate_context_tokens(&messages);
            if let Some(usage_idx) = estimate.last_usage_index
                && let Some(ref comp) = compaction_entry
                && let Some(comp_ts) = entry_timestamp_ms(&comp.timestamp)
                && let Some(usage_msg) = messages.get(usage_idx)
                && usage_msg.as_assistant().is_some_and(|a| a.timestamp <= comp_ts)
            {
                return false;
            }
            estimate.tokens
        } else {
            direct_context_tokens
        };

        if should_compact(context_tokens, context_window, &settings) {
            return self._run_auto_compaction(CompactionReason::Threshold, false).await;
        }
        false
    }

    pub(crate) async fn _run_auto_compaction(&self, reason: CompactionReason, will_retry: bool) -> bool {
        let Some(model) = self.model() else {
            return false;
        };
        let settings = resolved_compaction_settings(&self.inner.settings_manager, Some(&model));
        let path_entries = self.inner.session_manager.get_branch(None);
        let Some(preparation) = prepare_compaction(&path_entries, &settings) else {
            return false;
        };

        let controller = Arc::new(AbortController::new());
        let signal = controller.signal();
        {
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .auto_compaction_abort_controller = Some(controller.clone());
        }
        self._emit(AgentSessionEvent::CompactionStart { reason });

        let mut from_extension = false;
        let mut cancelled_by_extension = false;

        let outcome: pi_js::Result<CompactionResult> = async {
            signal.throw_if_aborted()?;

            let runner = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extension_runner
                .clone();
            let mut extension_compaction: Option<CompactionResult> = None;

            if let Some(ref r) = runner
                && r.has_handlers("session_before_compact")
            {
                let res = r
                    .emit_session_before_compact(SessionBeforeCompactEvent {
                        preparation: preparation.clone(),
                        branch_entries: path_entries,
                        custom_instructions: None,
                        reason: to_ext_compaction_reason(reason),
                        will_retry,
                        signal: Some(signal.clone()),
                    })
                    .await;
                if let Some(res) = res {
                    if res.cancel == Some(true) {
                        cancelled_by_extension = true;
                        return Err(pi_js::Error::msg("Compaction cancelled"));
                    }
                    if let Some(comp) = res.compaction {
                        extension_compaction = Some(comp);
                        from_extension = true;
                    }
                }
            }
            signal.throw_if_aborted()?;

            let compact_res = match extension_compaction {
                Some(comp) => comp,
                None => {
                    self._run_default_compaction(&preparation, &model, None, &signal, reason)
                        .await?
                }
            };
            signal.throw_if_aborted()?;

            let entry_id = self.inner.session_manager.append_compaction(
                &compact_res.summary,
                Some(&compact_res.first_kept_entry_id),
                compact_res.tokens_before,
                compact_res.details.clone(),
                Some(from_extension),
                compact_res.usage.clone(),
            )?;
            self._refresh_finalized_context();
            let estimated_tokens_after =
                estimate_context_tokens(&self.inner.session_manager.build_session_projection().messages).tokens;

            let saved_entry = self
                .inner
                .session_manager
                .get_entry(&entry_id)
                .and_then(|e| match e {
                    SessionEntry::Compaction(c) => Some(c),
                    _ => None,
                })
                .or_else(|| {
                    self.inner
                        .session_manager
                        .get_entries()
                        .into_iter()
                        .find_map(|e| match e {
                            SessionEntry::Compaction(c) if c.summary == compact_res.summary => Some(c),
                            _ => None,
                        })
                });

            if let (Some(r), Some(compaction_entry)) = (runner, saved_entry) {
                let _ = r
                    .emit(SessionCompactEvent {
                        compaction_entry,
                        from_extension,
                        reason: to_ext_compaction_reason(reason),
                        will_retry,
                    })
                    .await;
            }

            Ok(CompactionResult {
                summary: compact_res.summary,
                first_kept_entry_id: compact_res.first_kept_entry_id,
                tokens_before: compact_res.tokens_before,
                estimated_tokens_after: Some(estimated_tokens_after),
                usage: compact_res.usage,
                details: compact_res.details,
            })
        }
        .await;

        {
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .auto_compaction_abort_controller = None;
        }
        self._resolve_idle_wait_if_idle();

        match outcome {
            Ok(result) => {
                self._emit(AgentSessionEvent::CompactionEnd {
                    reason,
                    result: Some(result),
                    aborted: false,
                    will_retry,
                    error_message: None,
                });
                if will_retry {
                    true
                } else {
                    self.inner.agent.has_queued_messages()
                }
            }
            Err(err) => {
                let aborted = signal.aborted() || cancelled_by_extension;
                let error_message = if aborted {
                    None
                } else if reason == CompactionReason::Overflow {
                    Some(format!("Context overflow recovery failed: {err}"))
                } else {
                    Some(format!("Auto-compaction failed: {err}"))
                };
                self._emit(AgentSessionEvent::CompactionEnd {
                    reason,
                    result: None,
                    aborted,
                    will_retry: false,
                    error_message: error_message.clone(),
                });
                self._emit_session_compact_failed(reason, error_message, aborted, false, from_extension)
                    .await;
                false
            }
        }
    }

    pub fn set_auto_compaction_enabled(&self, enabled: bool) {
        self.inner.settings_manager.set_compaction_enabled(enabled);
    }

    pub fn auto_compaction_enabled(&self) -> bool {
        self.inner.settings_manager.get_compaction_enabled()
    }

    // =========================================================================
    // Extensions & Reload
    // =========================================================================

    pub async fn bind_extensions(&self, bindings: ExtensionBindings) -> pi_js::Result<()> {
        let (runner, start_event) = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ui) = bindings.ui_context {
                guard.extension_ui_context = Some(ui);
            }
            if let Some(mode) = bindings.mode {
                guard.extension_mode = mode;
            }
            if let Some(actions) = bindings.command_context_actions {
                guard.extension_command_context_actions = Some(actions);
            }
            if let Some(abort_handler) = bindings.abort_handler {
                guard.extension_abort_handler = Some(abort_handler);
            }
            if let Some(shutdown_handler) = bindings.shutdown_handler {
                guard.extension_shutdown_handler = Some(shutdown_handler);
            }
            if let Some(on_error) = bindings.on_error {
                guard.extension_error_listener = Some(on_error);
            }
            (guard.extension_runner.clone(), guard.session_start_event.clone())
        };

        if let Some(runner) = runner {
            self._apply_extension_bindings(&runner);
            let _ = runner.emit(start_event.clone()).await;
            runner.report_unhandled_mcp_servers();
        }
        let reason = if start_event.reason == SessionStartReason::Reload {
            ResourcesDiscoverReason::Reload
        } else {
            ResourcesDiscoverReason::Startup
        };
        self.extend_resources_from_extensions(reason).await;
        Ok(())
    }

    async fn extend_resources_from_extensions(&self, reason: ResourcesDiscoverReason) {
        let (runner, cwd) = {
            let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            (guard.extension_runner.clone(), guard.cwd.clone())
        };
        let Some(runner) = runner else {
            return;
        };
        if !runner.has_handlers("resources_discover") {
            return;
        }
        let discovered = runner.emit_resources_discover(&cwd, reason).await;
        if discovered.skill_paths.is_empty() && discovered.prompt_paths.is_empty() && discovered.theme_paths.is_empty()
        {
            return;
        }

        let extension_paths = ResourceExtensionPaths {
            skill_paths: (!discovered.skill_paths.is_empty())
                .then(|| self.build_extension_resource_paths(&discovered.skill_paths)),
            prompt_paths: (!discovered.prompt_paths.is_empty())
                .then(|| self.build_extension_resource_paths(&discovered.prompt_paths)),
            theme_paths: (!discovered.theme_paths.is_empty())
                .then(|| self.build_extension_resource_paths(&discovered.theme_paths)),
        };
        self.inner.resource_loader.extend_resources(extension_paths);
        let active = self.get_active_tool_names();
        self._rebuild_system_prompt(&active);
    }

    fn build_extension_resource_paths(&self, entries: &[DiscoveredResourcePathEntry]) -> Vec<ExtensionResourcePath> {
        entries
            .iter()
            .map(|entry| {
                let source = self.get_extension_source_label(&entry.extension_path);
                let base_dir = if is_synthetic_path(&entry.extension_path) {
                    None
                } else {
                    Some(pi_js::path::dirname(&entry.extension_path))
                };
                ExtensionResourcePath {
                    path: entry.path.clone(),
                    metadata: PathMetadata {
                        source,
                        scope: SourceScope::Temporary,
                        origin: SourceOrigin::TopLevel,
                        base_dir,
                        package_root: None,
                        changelog_path: None,
                    },
                }
            })
            .collect()
    }

    fn get_extension_source_label(&self, extension_path: &str) -> String {
        if is_synthetic_path(extension_path) {
            let stripped = extension_path.replace(['<', '>'], "");
            return format!("extension:{stripped}");
        }
        let base = pi_js::path::basename(extension_path, None);
        let name = base
            .strip_suffix(".ts")
            .or_else(|| base.strip_suffix(".js"))
            .unwrap_or(&base);
        format!("extension:{name}")
    }

    fn _apply_extension_bindings(&self, runner: &ExtensionRunner) {
        let (ui_context, mode, cmd_actions, error_listener, prev_unsub) = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            (
                guard.extension_ui_context.clone(),
                guard.extension_mode,
                guard.extension_command_context_actions.clone(),
                guard.extension_error_listener.clone(),
                guard.extension_error_unsubscriber.take(),
            )
        };
        runner.set_ui_context(ui_context, mode);
        runner.bind_command_context(cmd_actions);
        if let Some(unsub) = prev_unsub {
            unsub.call();
        }
        let next_unsub = error_listener.map(|listener| runner.on_error_typed(listener));
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extension_error_unsubscriber = next_unsub;
    }

    fn _refresh_current_model_from_registry(&self) {
        let Some(current_model) = self.model() else {
            return;
        };
        let Some(refreshed) = self
            .inner
            .model_runtime
            .get_model(&current_model.provider, &current_model.id)
        else {
            return;
        };
        if refreshed == current_model {
            return;
        }
        self.inner.agent.set_model(refreshed);
    }

    fn _bind_extension_core(&self, runner: &ExtensionRunner) {
        let runner_for_cmds = runner.clone();
        let resource_loader = Arc::clone(&self.inner.resource_loader);
        let get_commands = Arc::new(move || -> Vec<SlashCommandInfo> {
            let ext_cmds: Vec<SlashCommandInfo> = runner_for_cmds
                .get_registered_commands()
                .into_iter()
                .map(|c| SlashCommandInfo {
                    name: c.invocation_name,
                    description: c.command.description,
                    source: SlashCommandSource::Extension,
                    source_info: c.command.source_info,
                })
                .collect();
            let templates: Vec<SlashCommandInfo> = resource_loader
                .get_prompts()
                .0
                .into_iter()
                .map(|t| SlashCommandInfo {
                    name: t.name,
                    description: Some(t.description),
                    source: SlashCommandSource::Prompt,
                    source_info: t.source_info,
                })
                .collect();
            let skills: Vec<SlashCommandInfo> = resource_loader
                .get_skills()
                .0
                .into_iter()
                .map(|s| SlashCommandInfo {
                    name: format!("skill:{}", s.name),
                    description: Some(s.description),
                    source: SlashCommandSource::Skill,
                    source_info: s.source_info,
                })
                .collect();
            let mut all = Vec::with_capacity(ext_cmds.len() + templates.len() + skills.len());
            all.extend(ext_cmds);
            all.extend(templates);
            all.extend(skills);
            all
        });

        let weak = Arc::downgrade(&self.inner);
        let runner_for_err = runner.clone();

        let actions = ExtensionActions {
            send_message: {
                let weak = weak.clone();
                let runner = runner_for_err.clone();
                Arc::new(move |msg, options| {
                    let Some(inner) = weak.upgrade() else {
                        return;
                    };
                    let session = AgentSession { inner };
                    let draft = SendCustomMessageDraft {
                        custom_type: msg.custom_type,
                        content: msg.content,
                        display: msg.display,
                        details: msg.details,
                    };
                    let send_opts = options.map(|o| SendCustomMessageOptions {
                        trigger_turn: o.trigger_turn,
                        deliver_as: o.deliver_as.map(|d| match d {
                            DeliverAs::Steer => CustomMessageDelivery::Steer,
                            DeliverAs::FollowUp => CustomMessageDelivery::FollowUp,
                            DeliverAs::NextTurn => CustomMessageDelivery::NextTurn,
                        }),
                    });
                    if let Some(agent_msg) = session._try_send_custom_message_sync(draft, send_opts) {
                        let runner = runner.clone();
                        tokio::spawn(async move {
                            if let Err(err) = session
                                ._run_agent_prompt(pi_agent_core::agent::PromptInput::Message(agent_msg))
                                .await
                            {
                                runner.emit_error(ExtensionError {
                                    extension_path: "<runtime>".to_string(),
                                    event: "send_message".to_string(),
                                    error: err.to_string(),
                                    stack: None,
                                });
                            }
                        });
                    }
                })
            },
            send_user_message: {
                let weak = weak.clone();
                let runner = runner_for_err.clone();
                Arc::new(move |content, options| {
                    let Some(inner) = weak.upgrade() else {
                        return;
                    };
                    let session = AgentSession { inner };
                    let input = match content {
                        crate::core::extensions::types::CustomMessageContent::Text(s) => UserMessageInput::Text(s),
                        crate::core::extensions::types::CustomMessageContent::Blocks(b) => UserMessageInput::Blocks(b),
                    };
                    let send_opts = options.map(|o| SendUserMessageOptions {
                        deliver_as: o.deliver_as.map(|d| match d {
                            InputDeliveryBehavior::Steer => StreamingBehavior::Steer,
                            InputDeliveryBehavior::FollowUp => StreamingBehavior::FollowUp,
                        }),
                        expand_prompt_templates: o.expand_prompt_templates,
                    });
                    match session._try_send_user_message_sync(input, send_opts) {
                        Ok(None) => {}
                        Ok(Some((track, fut))) => {
                            let runner = runner.clone();
                            let session_for_track = session.clone();
                            let handle = tokio::spawn(async move {
                                if let Err(err) = fut.await {
                                    runner.emit_error(ExtensionError {
                                        extension_path: "<runtime>".to_string(),
                                        event: "send_user_message".to_string(),
                                        error: err.to_string(),
                                        stack: None,
                                    });
                                }
                            });
                            if track {
                                session_for_track
                                    .inner
                                    .state
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .pending_extension_send_tasks
                                    .push(handle);
                            }
                        }
                        Err(err) => {
                            runner.emit_error(ExtensionError {
                                extension_path: "<runtime>".to_string(),
                                event: "send_user_message".to_string(),
                                error: err.to_string(),
                                stack: None,
                            });
                        }
                    }
                })
            },
            append_entry: {
                let weak = weak.clone();
                Arc::new(move |custom_type, data| {
                    if let Some(inner) = weak.upgrade()
                        && let Ok(entry_id) = inner.session_manager.append_custom_entry(custom_type, data)
                        && let Some(entry) = inner.session_manager.get_entry(&entry_id)
                    {
                        AgentSession { inner }._emit(AgentSessionEvent::EntryAppended { entry });
                    }
                })
            },
            set_session_name: {
                let weak = weak.clone();
                Arc::new(move |name| {
                    if let Some(inner) = weak.upgrade() {
                        AgentSession { inner }.set_session_name(name);
                    }
                })
            },
            get_session_name: {
                let weak = weak.clone();
                Arc::new(move || {
                    weak.upgrade()
                        .and_then(|inner| inner.session_manager.get_session_name())
                })
            },
            set_label: {
                let weak = weak.clone();
                Arc::new(move |entry_id, label| {
                    if let Some(inner) = weak.upgrade() {
                        let _ = inner.session_manager.append_label_change(entry_id, label);
                    }
                })
            },
            get_active_tools: {
                let weak = weak.clone();
                Arc::new(move || {
                    weak.upgrade()
                        .map(|inner| AgentSession { inner }.get_active_tool_names())
                        .unwrap_or_default()
                })
            },
            get_all_tools: {
                let weak = weak.clone();
                Arc::new(move || {
                    weak.upgrade()
                        .map(|inner| AgentSession { inner }.get_all_tools())
                        .unwrap_or_default()
                })
            },
            get_settings: {
                let weak = weak.clone();
                Arc::new(move || {
                    weak.upgrade()
                        .map(|inner| inner.settings_manager.get_settings())
                        .unwrap_or_default()
                })
            },
            set_active_tools: {
                let weak = weak.clone();
                Arc::new(move |tool_names| {
                    if let Some(inner) = weak.upgrade() {
                        AgentSession { inner }.set_active_tools_by_name(&tool_names);
                    }
                })
            },
            refresh_tools: {
                let weak = weak.clone();
                Arc::new(move || {
                    if let Some(inner) = weak.upgrade() {
                        AgentSession { inner }._refresh_tool_registry(None, false);
                    }
                })
            },
            get_commands,
            set_model: {
                let weak = weak.clone();
                Arc::new(move |model| {
                    let weak = weak.clone();
                    Box::pin(async move {
                        let Some(inner) = weak.upgrade() else {
                            return Ok(false);
                        };
                        if !inner.model_runtime.has_configured_auth(&model.provider) {
                            return Ok(false);
                        }
                        AgentSession { inner }.set_model(model, None).await?;
                        Ok(true)
                    })
                })
            },
            get_thinking_level: {
                let weak = weak.clone();
                Arc::new(move || {
                    weak.upgrade()
                        .map(|inner| to_agent_thinking_level(AgentSession { inner }.thinking_level()))
                        .unwrap_or(pi_agent_core::ThinkingLevel::Off)
                })
            },
            set_thinking_level: {
                let weak = weak.clone();
                Arc::new(move |level| {
                    if let Some(inner) = weak.upgrade() {
                        AgentSession { inner }.set_thinking_level(from_agent_thinking_level(level), None);
                    }
                })
            },
        };

        let context_actions = ExtensionContextActions {
            get_model: {
                let weak = weak.clone();
                Arc::new(move || weak.upgrade().and_then(|inner| AgentSession { inner }.model()))
            },
            get_scoped_models: {
                let weak = weak.clone();
                Arc::new(move || {
                    weak.upgrade()
                        .map(|inner| {
                            AgentSession { inner }
                                .scoped_models()
                                .into_iter()
                                .map(|entry| crate::core::model_resolver::ScopedModel {
                                    model: entry.model,
                                    thinking_level: entry.thinking_level,
                                })
                                .collect()
                        })
                        .unwrap_or_default()
                })
            },
            is_idle: {
                let weak = weak.clone();
                Arc::new(move || {
                    weak.upgrade()
                        .map(|inner| AgentSession { inner }.is_idle())
                        .unwrap_or(true)
                })
            },
            is_project_trusted: {
                let weak = weak.clone();
                Arc::new(move || {
                    weak.upgrade()
                        .map(|inner| inner.settings_manager.is_project_trusted())
                        .unwrap_or(false)
                })
            },
            get_signal: {
                let weak = weak.clone();
                Arc::new(move || weak.upgrade().and_then(|inner| inner.agent.signal()))
            },
            abort: {
                let weak = weak.clone();
                Arc::new(move || {
                    if let Some(inner) = weak.upgrade() {
                        let handler = inner
                            .state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .extension_abort_handler
                            .clone();
                        if let Some(handler) = handler {
                            handler();
                        } else {
                            let session = AgentSession { inner };
                            session.request_abort();
                            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                                handle.spawn(async move {
                                    session.wait_for_idle().await;
                                });
                            }
                        }
                    }
                })
            },
            has_pending_messages: {
                let weak = weak.clone();
                Arc::new(move || {
                    weak.upgrade()
                        .map(|inner| AgentSession { inner }.pending_message_count() > 0)
                        .unwrap_or(false)
                })
            },
            shutdown: {
                let weak = weak.clone();
                Arc::new(move || {
                    if let Some(inner) = weak.upgrade() {
                        let handler = inner
                            .state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .extension_shutdown_handler
                            .clone();
                        if let Some(handler) = handler {
                            handler();
                        }
                    }
                })
            },
            get_context_usage: {
                let weak = weak.clone();
                Arc::new(move || {
                    weak.upgrade()
                        .and_then(|inner| AgentSession { inner }.get_context_usage())
                })
            },
            compact: {
                let weak = weak.clone();
                Arc::new(move |options| {
                    if let Some(inner) = weak.upgrade() {
                        tokio::spawn(async move {
                            let session = AgentSession { inner };
                            let instr = options.as_ref().and_then(|o| o.custom_instructions.as_deref());
                            match session.compact(instr).await {
                                Ok(res) => {
                                    if let Some(cb) = options.as_ref().and_then(|o| o.on_complete.as_ref()) {
                                        cb(res);
                                    }
                                }
                                Err(err) => {
                                    if let Some(cb) = options.as_ref().and_then(|o| o.on_error.as_ref()) {
                                        cb(err);
                                    }
                                }
                            }
                        });
                    }
                })
            },
            get_system_prompt: {
                let weak = weak.clone();
                Arc::new(move || {
                    weak.upgrade()
                        .map(|inner| AgentSession { inner }.system_prompt())
                        .unwrap_or_default()
                })
            },
            get_system_prompt_options: Some({
                let weak = weak.clone();
                Arc::new(move || {
                    weak.upgrade()
                        .map(|inner| {
                            let guard = inner.state.lock().unwrap_or_else(|e| e.into_inner());
                            normalized_to_build_options(&guard.base_system_prompt_options)
                        })
                        .unwrap_or_default()
                })
            }),
            execute_tool: Some({
                let weak = weak.clone();
                Arc::new(move |caller_id, name, args, options| {
                    let weak = weak.clone();
                    let caller_id = caller_id.to_string();
                    let name = name.to_string();
                    Box::pin(async move {
                        let Some(inner) = weak.upgrade() else {
                            return pi_agent_core::types::AgentToolCallOutcome {
                                tool_call: pi_agent_core::types::AgentToolCall {
                                    id: caller_id,
                                    name,
                                    arguments: Default::default(),
                                },
                                result: pi_agent_core::types::AgentToolResult::text("Session disposed"),
                                is_error: true,
                            };
                        };
                        let name_fallback = name.clone();
                        let caller_fallback = caller_id.clone();
                        AgentSession { inner }
                            ._execute_nested_tool_call(&caller_id, &name, args, options)
                            .await
                            .unwrap_or_else(|err| pi_agent_core::types::AgentToolCallOutcome {
                                tool_call: pi_agent_core::types::AgentToolCall {
                                    id: caller_fallback,
                                    name: name_fallback,
                                    arguments: Default::default(),
                                },
                                result: pi_agent_core::types::AgentToolResult::text(err.to_string()),
                                is_error: true,
                            })
                    })
                })
            }),
            get_callable_tools: Some({
                let weak = weak.clone();
                Arc::new(move || {
                    weak.upgrade()
                        .map(|inner| AgentSession { inner }._get_callable_tools(None))
                        .unwrap_or_default()
                })
            }),
        };

        let provider_actions = ProviderActions {
            register_provider: Some({
                let weak = weak.clone();
                Arc::new(move |name, config| {
                    if let Some(inner) = weak.upgrade() {
                        let _ = inner.model_runtime.register_provider(name, config.into());
                        AgentSession { inner }._refresh_current_model_from_registry();
                    }
                })
            }),
            register_native_provider: Some({
                let weak = weak.clone();
                Arc::new(move |provider| {
                    if let Some(inner) = weak.upgrade() {
                        inner.model_runtime.register_native_provider(provider);
                        AgentSession { inner }._refresh_current_model_from_registry();
                    }
                })
            }),
            unregister_provider: Some({
                let weak = weak.clone();
                Arc::new(move |name| {
                    if let Some(inner) = weak.upgrade() {
                        inner.model_runtime.unregister_provider(name);
                        AgentSession { inner }._refresh_current_model_from_registry();
                    }
                })
            }),
            register_virtual_model: Some({
                let weak = weak.clone();
                Arc::new(move |definition| {
                    if let Some(inner) = weak.upgrade() {
                        inner.model_runtime.register_virtual_model(definition);
                        AgentSession { inner }._refresh_current_model_from_registry();
                    }
                })
            }),
            unregister_virtual_model: Some({
                let weak = weak.clone();
                Arc::new(move |provider, id| {
                    if let Some(inner) = weak.upgrade() {
                        inner.model_runtime.unregister_virtual_model(provider, id);
                        AgentSession { inner }._refresh_current_model_from_registry();
                    }
                })
            }),
        };

        runner.bind_core(actions, context_actions, Some(provider_actions));
    }

    pub(crate) fn _refresh_tool_registry(
        &self,
        active_tool_names: Option<&[String]>,
        include_all_extension_tools: bool,
    ) {
        let previous_active = self.get_active_tool_names();
        let (runner, base_tool_defs, custom_tools, allowed_tool_names, previous_registry_names) = {
            let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            (
                guard.extension_runner.clone(),
                guard.base_tool_definitions.clone(),
                guard.custom_tools.clone(),
                guard.allowed_tool_names.clone(),
                guard.tool_registry.keys().cloned().collect::<Vec<_>>(),
            )
        };

        let previous_activated_on_registration: IndexSet<String> = previous_registry_names
            .into_iter()
            .filter(|name| self._is_activated_on_registration(name))
            .collect();

        let registered_tools = runner
            .as_ref()
            .map(ExtensionRunner::get_all_registered_tools)
            .unwrap_or_default();

        let mut all_custom_tools = registered_tools;
        for def in custom_tools {
            let path = format!("<sdk:{}>", def.name);
            all_custom_tools.push(crate::core::extensions::types::RegisteredTool {
                source_info: create_synthetic_source_info(
                    &path,
                    SyntheticSourceOptions {
                        source: "sdk".to_string(),
                        ..Default::default()
                    },
                ),
                definition: def,
            });
        }
        all_custom_tools.retain(|t| self._is_allowed_tool(&t.definition.name));

        let mut definition_registry: IndexMap<String, ToolDefinitionEntry> = IndexMap::new();
        let mut allowed_base_entries = Vec::new();
        for (name, def) in &base_tool_defs {
            if self._is_allowed_tool(name) {
                let source_info: SourceInfo = create_synthetic_source_info(
                    &format!("{BUILTIN_PATH_PREFIX}{name}"),
                    SyntheticSourceOptions {
                        source: "builtin".to_string(),
                        ..Default::default()
                    },
                );
                definition_registry.insert(
                    name.clone(),
                    ToolDefinitionEntry {
                        definition: def.clone(),
                        source_info: source_info.clone(),
                    },
                );
                allowed_base_entries.push(crate::core::extensions::types::RegisteredTool {
                    definition: def.clone(),
                    source_info,
                });
            }
        }
        for tool in &all_custom_tools {
            definition_registry.insert(
                tool.definition.name.clone(),
                ToolDefinitionEntry {
                    definition: tool.definition.clone(),
                    source_info: tool.source_info.clone(),
                },
            );
        }

        let mut tool_prompt_snippets: IndexMap<String, String> = IndexMap::new();
        let mut tool_prompt_guidelines: IndexMap<String, Vec<String>> = IndexMap::new();
        for entry in definition_registry.values() {
            if let Some(snippet) = self._normalize_prompt_snippet(entry.definition.prompt_snippet.as_deref()) {
                tool_prompt_snippets.insert(entry.definition.name.clone(), snippet);
            }
            let guidelines = self._normalize_prompt_guidelines(entry.definition.prompt_guidelines.as_deref());
            if !guidelines.is_empty() {
                tool_prompt_guidelines.insert(entry.definition.name.clone(), guidelines);
            }
        }

        let wrapped_ext = match runner.as_ref() {
            Some(r) => wrap_registered_tools(&all_custom_tools, r),
            None => Vec::new(),
        };
        let wrapped_builtin = match runner.as_ref() {
            Some(r) => wrap_registered_tools(&allowed_base_entries, r),
            None => Vec::new(),
        };

        let mut tool_registry: IndexMap<String, AgentTool> = IndexMap::new();
        for tool in wrapped_builtin {
            tool_registry.insert(tool.name.clone(), tool);
        }
        for tool in wrapped_ext.iter().cloned() {
            tool_registry.insert(tool.name.clone(), tool);
        }

        let pending_tool_names: Vec<String> = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.tool_definitions = definition_registry;
            guard.tool_prompt_snippets = tool_prompt_snippets;
            guard.tool_prompt_guidelines = tool_prompt_guidelines;
            guard.tool_registry = tool_registry;
            guard.pending_tool_names.iter().cloned().collect()
        };

        let mut next_active: Vec<String> = active_tool_names
            .map(<[String]>::to_vec)
            .unwrap_or(previous_active)
            .into_iter()
            .filter(|name| self._is_allowed_tool(name))
            .collect();

        if let Some(ref allowed) = allowed_tool_names {
            let keys: Vec<String> = {
                let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                guard.tool_registry.keys().cloned().collect()
            };
            for tool_name in keys {
                if super::tool_name_matches_entries(allowed, &tool_name) && self._is_declarable(&tool_name) {
                    next_active.push(tool_name);
                }
            }
        } else if include_all_extension_tools {
            for tool in &wrapped_ext {
                if self._is_activated_on_registration(&tool.name) {
                    next_active.push(tool.name.clone());
                }
            }
        } else if active_tool_names.is_none() {
            let keys: Vec<String> = {
                let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
                guard.tool_registry.keys().cloned().collect()
            };
            for tool_name in keys {
                if !previous_activated_on_registration.contains(&tool_name)
                    && self._is_activated_on_registration(&tool_name)
                {
                    next_active.push(tool_name);
                }
            }
        }

        next_active.extend(pending_tool_names);
        let deduped: IndexSet<String> = next_active.into_iter().collect();
        let final_active: Vec<String> = deduped.into_iter().collect();
        self._set_active_tools(&final_active);
    }

    fn _is_declarable(&self, name: &str) -> bool {
        let exposure = self._get_tool_exposure(name);
        matches!(
            exposure,
            crate::core::extensions::types::ToolExposure::Direct
                | crate::core::extensions::types::ToolExposure::ModelOnly
        )
    }

    fn _is_activated_on_registration(&self, name: &str) -> bool {
        if !self._is_declarable(name) {
            return false;
        }
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .tool_definitions
            .get(name)
            .and_then(|e| e.definition.default_active)
            != Some(false)
    }

    pub(crate) fn _build_runtime(
        &self,
        active_tool_names: Option<&[String]>,
        flag_values: Option<&IndexMap<String, FlagValue>>,
        include_all_extension_tools: bool,
    ) {
        let auto_resize_images = self.inner.settings_manager.get_image_auto_resize();
        let shell_command_prefix = self.inner.settings_manager.get_shell_command_prefix();
        let shell_path = self.inner.settings_manager.get_shell_path();

        let (cwd, base_tools_override, runner_ref) = {
            let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            (
                guard.cwd.clone(),
                guard.base_tools_override.clone(),
                guard.extension_runner_ref.clone(),
            )
        };

        let base_tool_defs: IndexMap<String, ToolDefinition> = match base_tools_override {
            Some(ref overrides) => overrides
                .iter()
                .map(|(name, tool)| (name.clone(), create_tool_definition_from_agent_tool(tool.clone())))
                .collect(),
            None => create_all_tool_definitions(
                &cwd,
                Some(&ToolsOptions {
                    read: Some(crate::core::tools::read::ReadToolOptions {
                        auto_resize_images: Some(auto_resize_images),
                        ..Default::default()
                    }),
                    bash: Some(crate::core::tools::bash::BashToolOptions {
                        operations: shell_path.map(|sp| create_local_bash_operations_with_shell(Some(sp))),
                        command_prefix: shell_command_prefix,
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
            )
            .into_iter()
            .map(|(name, def)| (name.as_str().to_string(), def))
            .collect(),
        };

        let default_active_tool_names: Vec<String> = match base_tools_override {
            Some(ref overrides) => overrides.keys().cloned().collect(),
            None => DEFAULT_TOOL_NAMES.iter().map(|s| (*s).to_string()).collect(),
        };

        let extensions_result = self.inner.resource_loader.get_core_extensions();
        if let Some(flags) = flag_values {
            let mut runtime_flags = extensions_result
                .runtime
                .inner
                .flag_values
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            for (k, v) in flags {
                runtime_flags.insert(k.clone(), v.clone());
            }
        }

        let runner = ExtensionRunner::new(
            extensions_result.extensions,
            extensions_result.runtime,
            cwd,
            self.inner.session_manager.clone(),
            ModelRegistry::new(self.inner.model_runtime.clone()),
        );
        if let Some(ref runner_ref) = runner_ref {
            *runner_ref.lock().unwrap_or_else(|e| e.into_inner()) = Some(runner.clone());
        }

        {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.base_tool_definitions = base_tool_defs;
            guard.extension_runner = Some(runner.clone());
        }

        self._bind_extension_core(&runner);
        self._apply_extension_bindings(&runner);

        let base_active = active_tool_names.unwrap_or(&default_active_tool_names);
        self._refresh_tool_registry(Some(base_active), include_all_extension_tools);
    }

    pub async fn reload(&self, options: Option<ReloadOptions>) -> pi_js::Result<()> {
        let (old_runner, uses_default_tools) = {
            let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            (guard.extension_runner.clone(), guard.uses_default_tools)
        };
        let previous_flag_values = if let Some(ref old) = old_runner {
            let flags = old.get_flag_values();
            emit_session_shutdown_event(
                old,
                SessionShutdownEvent {
                    reason: SessionShutdownReason::Reload,
                    target_session_file: None,
                },
            )
            .await;
            old.invalidate(None);
            Some(flags)
        } else {
            None
        };

        let previous_default_tools: IndexSet<String> = if uses_default_tools {
            self.inner
                .settings_manager
                .get_default_tools()
                .unwrap_or_else(|| DEFAULT_TOOL_NAMES.iter().map(|s| (*s).to_string()).collect())
                .into_iter()
                .collect()
        } else {
            IndexSet::new()
        };

        self.inner.settings_manager.reload().await;
        self.sync_queue_modes_from_settings();
        register_built_in_api_providers();
        self.inner.resource_loader.reload(None).await?;

        let added_default_tools: Vec<String> = if uses_default_tools {
            self.inner
                .settings_manager
                .get_default_tools()
                .unwrap_or_else(|| DEFAULT_TOOL_NAMES.iter().map(|s| (*s).to_string()).collect())
                .into_iter()
                .filter(|name| !previous_default_tools.contains(name))
                .collect()
        } else {
            Vec::new()
        };

        let current_active = self.get_active_tool_names();
        {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            for name in &current_active {
                guard.pending_tool_names.insert(name.clone());
            }
        }
        let mut next_active = current_active;
        next_active.extend(added_default_tools);

        self._build_runtime(Some(&next_active), previous_flag_values.as_ref(), true);

        let (has_bindings, runner) = {
            let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let has = guard.extension_ui_context.is_some()
                || guard.extension_command_context_actions.is_some()
                || guard.extension_shutdown_handler.is_some()
                || guard.extension_error_listener.is_some();
            (has, guard.extension_runner.clone())
        };
        if has_bindings {
            if let Some(opts) = options
                && let Some(hook) = opts.before_session_start
            {
                hook().await?;
            }
            if let Some(runner) = runner {
                let _ = runner
                    .emit(SessionStartEvent {
                        reason: SessionStartReason::Reload,
                        previous_session_file: None,
                    })
                    .await;
                runner.report_unhandled_mcp_servers();
            }
            self.extend_resources_from_extensions(ResourcesDiscoverReason::Reload)
                .await;
        }
        Ok(())
    }

    // =========================================================================
    // Auto-Retry
    // =========================================================================

    pub(crate) fn _is_retryable_error(&self, message: &AssistantMessage) -> bool {
        let context_window = self
            ._model_for_message(message)
            .or_else(|| self.model())
            .map(|m| m.context_window)
            .unwrap_or(0);
        if is_context_overflow(message, Some(context_window)) {
            return false;
        }
        is_retryable_assistant_error(message)
    }

    fn _summarization_retry_callbacks(
        &self,
        source: SummarizationRetrySource,
        reason: Option<CompactionReason>,
    ) -> Arc<dyn RetryCallbacks> {
        Arc::new(SessionSummarizationRetryCallbacks {
            weak: Arc::downgrade(&self.inner),
            source,
            reason,
        })
    }

    pub(crate) fn _finish_cancelled_retry(&self) {
        let attempt = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if guard.retry_attempt == 0 {
                return;
            }
            let a = guard.retry_attempt;
            guard.retry_attempt = 0;
            a
        };
        self._emit(AgentSessionEvent::AutoRetryEnd {
            success: false,
            attempt,
            final_error: Some("Retry cancelled".to_string()),
        });
    }

    pub(crate) async fn _prepare_retry(&self, message: &AssistantMessage) -> bool {
        let settings = to_retry_policy(self.inner.settings_manager.get_retry_settings());
        if !settings.enabled {
            return false;
        }

        let (attempt, exceeded) = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.retry_attempt += 1;
            if guard.retry_attempt > settings.max_retries {
                guard.retry_attempt -= 1;
                (guard.retry_attempt, true)
            } else {
                (guard.retry_attempt, false)
            }
        };
        if exceeded {
            return false;
        }

        let delay_ms = retry_delay_ms(&settings, attempt);
        let err_msg = message
            .error_message
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("Unknown error")
            .to_string();

        self._emit(AgentSessionEvent::AutoRetryStart {
            attempt,
            max_attempts: settings.max_retries,
            delay_ms,
            error_message: err_msg,
        });

        let _ = self._omit_recovery_attempt(message, &[]);

        let controller = Arc::new(AbortController::new());
        let signal = controller.signal();
        {
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retry_abort_controller = Some(controller);
        }

        let sleep_res = sleep(delay_ms, Some(&signal)).await;
        {
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retry_abort_controller = None;
        }

        if sleep_res.is_err() {
            self._finish_cancelled_retry();
            return false;
        }
        true
    }

    /// Cancel in-progress retry.
    pub fn abort_retry(&self) {
        let controller = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retry_abort_controller
            .clone();
        if let Some(c) = controller {
            c.abort(None);
        }
    }

    pub fn is_retrying(&self) -> bool {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retry_abort_controller
            .is_some()
    }

    pub fn auto_retry_enabled(&self) -> bool {
        self.inner.settings_manager.get_retry_enabled()
    }

    pub fn set_auto_retry_enabled(&self, enabled: bool) {
        self.inner.settings_manager.set_retry_enabled(enabled);
    }

    // =========================================================================
    // Bash Execution
    // =========================================================================

    pub async fn execute_bash(
        &self,
        command: &str,
        on_chunk: Option<Arc<dyn Fn(&str) + Send + Sync>>,
        options: Option<ExecuteBashOptions>,
    ) -> pi_js::Result<BashResult> {
        let opts = options.unwrap_or_default();
        let controller = Arc::new(AbortController::new());
        let signal = controller.signal();
        let bash_id = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            let id = guard.next_bash_id;
            guard.next_bash_id += 1;
            guard.bash_abort_controllers.push((id, controller));
            id
        };

        let prefix = self.inner.settings_manager.get_shell_command_prefix();
        let shell_path = self.inner.settings_manager.get_shell_path();
        let resolved_command = match prefix {
            Some(ref p) => format!("{p}\n{command}"),
            None => command.to_string(),
        };

        let operations: Arc<dyn BashOperations> = match opts.operations {
            Some(ops) => ops,
            None => create_local_bash_operations_with_shell(shell_path),
        };
        let weak = Arc::downgrade(&self.inner);
        let event_id = opts.id.clone();
        let chunk_cb = Arc::new(move |delta: &str| {
            if let Some(ref cb) = on_chunk {
                cb(delta);
            }
            if let Some(inner) = weak.upgrade() {
                AgentSession { inner }._emit(AgentSessionEvent::BashExecutionUpdate {
                    id: event_id.clone(),
                    delta: delta.to_string(),
                });
            }
        });

        let result = execute_bash_with_operations(
            &resolved_command,
            &self.inner.session_manager.get_cwd(),
            operations.as_ref(),
            Some(crate::core::bash_executor::BashExecutorOptions {
                on_chunk: Some(chunk_cb),
                signal: Some(signal),
            }),
        )
        .await;

        {
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .bash_abort_controllers
                .retain(|(id, _)| *id != bash_id);
        }

        let bash_result = result?;
        self.record_bash_result(
            command,
            &bash_result,
            Some(RecordBashOptions {
                exclude_from_context: opts.exclude_from_context,
            }),
        );
        Ok(bash_result)
    }

    pub fn record_bash_result(&self, command: &str, result: &BashResult, options: Option<RecordBashOptions>) {
        let bash_message = BashExecutionMessage {
            role: "bashExecution".to_string(),
            command: command.to_string(),
            output: result.output.clone(),
            exit_code: result.exit_code.map(i64::from),
            cancelled: result.cancelled,
            truncated: result.truncated,
            full_output_path: result.full_output_path.clone(),
            timestamp: pi_js::time::now_ms(),
            exclude_from_context: options.and_then(|o| o.exclude_from_context),
        };
        if self.is_streaming() {
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pending_bash_messages
                .push(bash_message);
        } else {
            let _ = self
                .inner
                .session_manager
                .append_message(AgentMessage::from(bash_message));
            self._refresh_finalized_context();
        }
    }

    pub fn abort_bash(&self) {
        let controllers: Vec<Arc<AbortController>> = {
            let guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            guard
                .bash_abort_controllers
                .iter()
                .map(|(_, c)| Arc::clone(c))
                .collect()
        };
        for c in controllers {
            c.abort(None);
        }
    }

    pub fn is_bash_running(&self) -> bool {
        !self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .bash_abort_controllers
            .is_empty()
    }

    pub fn has_pending_bash_messages(&self) -> bool {
        !self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending_bash_messages
            .is_empty()
    }

    pub(crate) fn _flush_pending_bash_messages(&self) {
        let pending = {
            let mut guard = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
            if guard.pending_bash_messages.is_empty() {
                return;
            }
            std::mem::take(&mut guard.pending_bash_messages)
        };
        for bash_message in pending {
            let _ = self
                .inner
                .session_manager
                .append_message(AgentMessage::from(bash_message));
        }
        self._refresh_finalized_context();
    }

    // =========================================================================
    // Session Management & Tree Navigation
    // =========================================================================

    pub fn set_session_name(&self, name: &str) {
        let _ = self.inner.session_manager.append_session_info(name);
        let session_name = self.inner.session_manager.get_session_name();
        self._emit(AgentSessionEvent::SessionInfoChanged {
            name: session_name.clone(),
        });
        let runner = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extension_runner
            .clone();
        if let Some(runner) = runner {
            let mut fut = Box::pin(async move {
                let _ = runner.emit(SessionInfoChangedEvent { name: session_name }).await;
            });
            let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
            if std::future::Future::poll(fut.as_mut(), &mut cx).is_pending()
                && let Ok(handle) = tokio::runtime::Handle::try_current()
            {
                handle.spawn(fut);
            }
        }
    }

    pub async fn navigate_tree(
        &self,
        target_id: &str,
        options: Option<NavigateTreeOptions>,
    ) -> pi_js::Result<NavigateTreeResult> {
        if self.is_streaming() {
            return Err(pi_js::Error::msg(
                "Wait for the current response to finish before navigating the session tree.",
            ));
        }
        if self.is_compacting() {
            return Err(pi_js::Error::msg(
                "Wait for the current compaction or tree navigation to finish before navigating the session tree.",
            ));
        }

        let opts = options.unwrap_or_default();
        let old_leaf_id = self.inner.session_manager.get_leaf_id();
        if old_leaf_id.as_deref() == Some(target_id) {
            return Ok(NavigateTreeResult {
                editor_text: None,
                cancelled: false,
                aborted: None,
                summary_entry: None,
            });
        }

        if opts.summarize == Some(true) && self.model().is_none() {
            return Err(pi_js::Error::msg("No model available for summarization"));
        }

        let Some(target_entry) = self.inner.session_manager.get_entry(target_id) else {
            return Err(pi_js::Error::msg(format!("Entry {target_id} not found")));
        };

        let collected =
            collect_entries_for_branch_summary(&self.inner.session_manager, old_leaf_id.as_deref(), target_id);
        let mut custom_instructions = opts.custom_instructions;
        let mut replace_instructions = opts.replace_instructions;
        let mut label = opts.label;

        let preparation = TreePreparation {
            target_id: target_id.to_string(),
            old_leaf_id: old_leaf_id.clone(),
            common_ancestor_id: collected.common_ancestor_id,
            entries_to_summarize: collected.entries.clone(),
            user_wants_summary: opts.summarize == Some(true),
            custom_instructions: custom_instructions.clone(),
            replace_instructions,
            label: label.clone(),
        };

        let controller = Arc::new(AbortController::new());
        let signal = controller.signal();
        {
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .branch_summary_abort_controller = Some(controller);
        }
        let mut controller_guard = BranchSummaryControllerGuard {
            inner: Arc::clone(&self.inner),
            armed: true,
        };

        let runner = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extension_runner
            .clone();

        let mut ext_summary: Option<crate::core::extensions::types::TreeSummaryOverride> = None;
        let mut from_extension = false;

        if let Some(ref r) = runner
            && r.has_handlers("session_before_tree")
        {
            let res = r
                .emit_session_before_tree(SessionBeforeTreeEvent {
                    preparation,
                    signal: Some(signal.clone()),
                })
                .await;
            if let Some(res) = res {
                if res.cancel == Some(true) {
                    return Ok(NavigateTreeResult {
                        editor_text: None,
                        cancelled: true,
                        aborted: None,
                        summary_entry: None,
                    });
                }
                if opts.summarize == Some(true)
                    && let Some(summary) = res.summary
                {
                    ext_summary = Some(summary);
                    from_extension = true;
                }
                if res.custom_instructions.is_some() {
                    custom_instructions = res.custom_instructions;
                }
                if res.replace_instructions.is_some() {
                    replace_instructions = res.replace_instructions;
                }
                if res.label.is_some() {
                    label = res.label;
                }
            }
        }

        let mut summary_text: Option<String> = None;
        let mut summary_details: Option<Value> = None;
        let mut summary_usage: Option<Usage> = None;

        if opts.summarize == Some(true) && !collected.entries.is_empty() && ext_summary.is_none() {
            let model = self
                .model()
                .ok_or_else(|| pi_js::Error::msg("No model available for summarization"))?;
            let auth = self
                ._get_summarization_request_auth(&model, Some(signal.clone()))
                .await?;
            let bs_settings = self.inner.settings_manager.get_branch_summary_settings();
            let retry = to_retry_policy(self.inner.settings_manager.get_retry_settings());
            let callbacks = self._summarization_retry_callbacks(SummarizationRetrySource::BranchSummary, None);
            let headers = auth.headers.map(|h| h.into_iter().map(|(k, v)| (k, Some(v))).collect());
            let res = generate_branch_summary(
                &collected.entries,
                GenerateBranchSummaryOptions {
                    model: auth.model,
                    api_key: auth.api_key,
                    headers,
                    env: auth.env,
                    signal: Some(signal),
                    thinking_level: Some(to_agent_thinking_level(auth.thinking_level)),
                    custom_instructions,
                    replace_instructions,
                    reserve_tokens: Some(bs_settings.reserve_tokens),
                    stream_fn: self.inner.agent.stream_fn(),
                    retry: Some(retry),
                    callbacks: Some(callbacks),
                },
            )
            .await;
            if res.aborted == Some(true) {
                return Ok(NavigateTreeResult {
                    editor_text: None,
                    cancelled: true,
                    aborted: Some(true),
                    summary_entry: None,
                });
            }
            if let Some(err) = res.error {
                return Err(pi_js::Error::msg(err));
            }
            summary_text = res.summary;
            summary_usage = res.usage;
            let details_struct = BranchSummaryDetails {
                read_files: res.read_files.unwrap_or_default(),
                modified_files: res.modified_files.unwrap_or_default(),
            };
            summary_details = Some(pi_js::json::parse(&pi_js::json::stringify(&details_struct))?);
        } else if let Some(ext) = ext_summary {
            summary_text = Some(ext.summary);
            summary_details = ext.details;
            summary_usage = ext.usage;
        }

        let (new_leaf_id, editor_text): (Option<String>, Option<String>) = match &target_entry {
            SessionEntry::Message(m) if m.message.role() == Some("user") => {
                let text = m
                    .message
                    .as_user()
                    .map(|u| content_text(&u.content, Some("")))
                    .unwrap_or_default();
                (m.parent_id.clone(), Some(text))
            }
            SessionEntry::CustomMessage(cm) => {
                let text = match &cm.content {
                    crate::core::extensions::types::CustomMessageContent::Text(s) => s.clone(),
                    crate::core::extensions::types::CustomMessageContent::Blocks(b) => content_text(b, Some("")),
                };
                (cm.parent_id.clone(), Some(text))
            }
            _ => (Some(target_id.to_string()), None),
        };

        let mut summary_entry: Option<BranchSummaryEntry> = None;
        if let Some(ref text) = summary_text {
            let summary_id = self.inner.session_manager.branch_with_summary(
                new_leaf_id.as_deref(),
                text,
                summary_details,
                Some(from_extension),
                summary_usage,
            )?;
            summary_entry = self.inner.session_manager.get_entry(&summary_id).and_then(|e| match e {
                SessionEntry::BranchSummary(bs) => Some(bs),
                _ => None,
            });
            if let Some(ref lbl) = label {
                let _ = self.inner.session_manager.append_label_change(&summary_id, Some(lbl));
            }
        } else if let Some(ref leaf) = new_leaf_id {
            self.inner.session_manager.branch(leaf)?;
        } else {
            self.inner.session_manager.reset_leaf();
        }

        if let Some(ref lbl) = label
            && summary_text.is_none()
        {
            let _ = self.inner.session_manager.append_label_change(target_id, Some(lbl));
        }

        self._refresh_finalized_context();
        self._restore_tools_from_transcript();

        controller_guard.armed = false;
        {
            self.inner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .branch_summary_abort_controller = None;
        }
        self._resolve_idle_wait_if_idle();

        if let Some(ref r) = runner {
            let _ = r
                .emit(SessionTreeEvent {
                    new_leaf_id: self.inner.session_manager.get_leaf_id(),
                    old_leaf_id,
                    summary_entry: summary_entry.clone(),
                    from_extension: summary_text.as_ref().map(|_| from_extension),
                })
                .await;
        }

        Ok(NavigateTreeResult {
            editor_text,
            cancelled: false,
            aborted: None,
            summary_entry,
        })
    }

    pub fn get_user_messages_for_forking(&self) -> Vec<ForkableUserMessage> {
        let mut result = Vec::new();
        for entry in self.inner.session_manager.get_entries() {
            if let SessionEntry::Message(m) = entry
                && let Some(user) = m.message.as_user()
            {
                let text = content_text(&user.content, Some(""));
                if !text.is_empty() {
                    result.push(ForkableUserMessage { entry_id: m.id, text });
                }
            }
        }
        result
    }

    pub fn get_session_stats(&self) -> SessionStats {
        let mut user_messages = 0i64;
        let mut assistant_messages = 0i64;
        let mut tool_results = 0i64;
        let mut total_messages = 0i64;
        let mut tool_calls = 0i64;
        let mut usage_totals = create_usage_totals();

        for entry in self.inner.session_manager.get_entries() {
            match entry {
                SessionEntry::Usage(u) => add_usage_to_totals(&mut usage_totals, &u.usage),
                SessionEntry::BranchSummary(bs) => {
                    if let Some(ref u) = bs.usage {
                        add_usage_to_totals(&mut usage_totals, u);
                    }
                }
                SessionEntry::Compaction(c) => {
                    if let Some(ref u) = c.usage {
                        add_usage_to_totals(&mut usage_totals, u);
                    }
                }
                SessionEntry::Message(m) => {
                    total_messages += 1;
                    if m.message.role() == Some("user") {
                        user_messages += 1;
                    } else if let Some(tr) = m.message.as_tool_result() {
                        tool_results += 1;
                        if let Some(ref u) = tr.usage {
                            add_usage_to_totals(&mut usage_totals, u);
                        }
                    } else if let Some(a) = m.message.as_assistant() {
                        assistant_messages += 1;
                        for block in &a.content {
                            if matches!(block, pi_ai::types::AssistantContentBlock::ToolCall(_)) {
                                tool_calls += 1;
                            }
                        }
                        add_usage_to_totals(&mut usage_totals, &a.usage);
                    }
                }
                _ => {}
            }
        }

        SessionStats {
            session_file: self.session_file(),
            session_id: self.session_id(),
            user_messages,
            assistant_messages,
            tool_calls,
            tool_results,
            total_messages,
            tokens: SessionTokenStats {
                input: usage_totals.input,
                output: usage_totals.output,
                cache_read: usage_totals.cache_read,
                cache_write: usage_totals.cache_write,
                total: usage_totals.input + usage_totals.output + usage_totals.cache_read + usage_totals.cache_write,
            },
            cost: usage_totals.cost,
            context_usage: self.get_context_usage(),
        }
    }

    pub fn get_context_usage(&self) -> Option<ContextUsage> {
        let model = self._limits_model()?;
        let context_window = model.context_window;
        if context_window <= 0 {
            return None;
        }

        let projection = self.inner.session_manager.build_session_projection();
        let branch = self.inner.session_manager.get_branch(None);
        if let Some(latest_compaction) = get_latest_compaction_entry(&branch) {
            let projected_assistants: IndexSet<String> = projection
                .entries
                .iter()
                .filter(|entry| {
                    entry.messages.iter().any(|m| {
                        m.as_assistant().is_some_and(|a| {
                            a.stop_reason != StopReason::Aborted
                                && a.stop_reason != StopReason::Error
                                && calculate_context_tokens(&a.usage) > 0
                        })
                    })
                })
                .map(|entry| entry.source_entry.id().to_string())
                .collect();
            let compaction_idx = branch.iter().position(|e| e.id() == latest_compaction.id).unwrap_or(0);
            let has_post_compaction_usage = branch[compaction_idx + 1..]
                .iter()
                .any(|e| projected_assistants.contains(e.id()));
            if !has_post_compaction_usage {
                return Some(ContextUsage {
                    tokens: None,
                    context_window,
                    percent: None,
                });
            }
        }

        let estimate = estimate_projected_context_tokens(&projection, &branch);
        let percent = (estimate.tokens as f64 / context_window as f64) * 100.0;
        Some(ContextUsage {
            tokens: Some(estimate.tokens),
            context_window,
            percent: Some(percent),
        })
    }

    pub async fn export_to_html(
        &self,
        output_path: Option<&str>,
        options: Option<ExportHtmlOptions>,
    ) -> pi_js::Result<String> {
        let requested_theme = options.and_then(|o| o.theme_name);
        let settings_theme = self.inner.settings_manager.get_theme();
        let candidate_theme = requested_theme.or(settings_theme);
        let resolved_theme = candidate_theme.as_deref().and_then(|name| {
            crate::modes::interactive::theme::theme::resolve_theme_setting(
                Some(name),
                crate::modes::interactive::theme::theme::get_terminal_theme(),
            )
            .or_else(|| Some(name.to_string()))
        });
        let theme_name = resolved_theme
            .as_deref()
            .and_then(crate::modes::interactive::theme::theme::get_theme_by_name)
            .and_then(|t| t.name().map(str::to_string))
            .or(resolved_theme);
        let session = self.clone();
        let get_tool_definition: Arc<
            dyn Fn(&str) -> Option<Arc<dyn crate::core::export_html::tool_renderer::CustomToolRenderer>> + Send + Sync,
        > = Arc::new(move |name| {
            session
                .get_tool_definition(name)
                .map(|def| Arc::new(def) as Arc<dyn crate::core::export_html::tool_renderer::CustomToolRenderer>)
        });
        let tool_renderer = crate::core::export_html::tool_renderer::create_tool_html_renderer(
            crate::core::export_html::tool_renderer::ToolHtmlRendererDeps {
                get_tool_definition,
                theme: crate::core::export_html::tool_renderer::HtmlTheme {
                    name: theme_name.clone(),
                },
                cwd: self.inner.session_manager.get_cwd(),
                width: None,
            },
        );
        export_session_to_html(
            &self.inner.session_manager,
            Some(&self.inner.agent.state()),
            Some(ExportOptions {
                output_path: output_path.map(str::to_string),
                theme_name,
                tool_renderer: Some(tool_renderer),
                theme_override: None,
            }),
        )
        .await
    }

    pub fn export_to_jsonl(&self, output_path: Option<&str>) -> pi_js::Result<String> {
        export_session_to_jsonl(&self.inner.session_manager, output_path, None)
    }

    pub async fn summarize_for_bug_report(&self, options: SummarizeBugReportOptions) -> pi_js::Result<String> {
        let Some(model) = self.model() else {
            return Err(pi_js::Error::msg("No model selected"));
        };
        let auth = self
            ._get_summarization_request_auth(&model, Some(options.signal.clone()))
            .await?;
        let retry = to_retry_policy(self.inner.settings_manager.get_retry_settings());
        let headers = auth.headers.map(|h| h.into_iter().map(|(k, v)| (k, Some(v))).collect());
        generate_bug_report_summary(GenerateBugReportSummaryOptions {
            model: auth.model,
            api_key: auth.api_key,
            headers,
            env: auth.env,
            messages: self.messages(),
            hint: options.hint,
            signal: Some(options.signal),
            thinking_level: Some(to_agent_thinking_level(auth.thinking_level)),
            stream_fn: self.inner.agent.stream_fn(),
            retry: Some(retry),
            session_id: Some(self.session_id()),
        })
        .await
    }

    // =========================================================================
    // Utilities & Extension System
    // =========================================================================

    pub fn get_last_assistant_text(&self) -> Option<String> {
        let messages = self.messages();
        let last_assistant = messages.iter().rev().find_map(|m| {
            let a = m.as_assistant()?;
            if a.stop_reason == StopReason::Aborted && a.content.is_empty() {
                None
            } else {
                Some(a)
            }
        })?;

        let mut text = String::new();
        for block in &last_assistant.content {
            if let pi_ai::types::AssistantContentBlock::Text(t) = block {
                text.push_str(&t.text);
            }
        }
        let trimmed = pi_js::str16::trim(&text);
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    pub fn create_replaced_session_context(&self) -> pi_js::Result<ReplacedSessionContext> {
        let runner = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extension_runner
            .clone()
            .ok_or_else(|| pi_js::Error::msg("ExtensionRunner is not initialized"))?;
        Ok(ReplacedSessionContext {
            base: runner.create_command_context(),
        })
    }

    pub fn has_extension_handlers(&self, event_type: &str) -> bool {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extension_runner
            .as_ref()
            .is_some_and(|r| r.has_handlers(event_type))
    }

    pub fn extension_runner(&self) -> Option<ExtensionRunner> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extension_runner
            .clone()
    }

    #[doc(hidden)]
    pub async fn check_compaction_for_test(
        &self,
        assistant_message: &AssistantMessage,
        skip_aborted_check: bool,
    ) -> bool {
        self._check_compaction(assistant_message, skip_aborted_check, &[]).await
    }

    #[doc(hidden)]
    pub async fn run_auto_compaction_for_test(&self, reason: CompactionReason, will_retry: bool) -> bool {
        self._run_auto_compaction(reason, will_retry).await
    }

    #[doc(hidden)]
    pub fn clear_model_for_test(&self) {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner()).has_model = false;
    }
}
