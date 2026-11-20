//! Port of packages/coding-agent/src/core/extensions/runner.ts

use std::any::Any;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use indexmap::{IndexMap, IndexSet};
use pi_agent_core::types::{
    AgentMessage, AgentTool, AgentToolCall, AgentToolCallOutcome, AgentToolResult, ThinkingLevel,
};
use pi_ai::Provider;
use pi_ai::types::{
    ImageContent, JsonObject, Message, Model, ProviderHeaders, SystemMessage, ToolResultMessage, Usage,
};
use pi_ai::utils::transcript::{TranscriptMessage, get_current_system_message};
use pi_js::abort::AbortSignal;
use pi_js::{BoxFuture, Unsubscribe};
use pi_tui::keybindings::{KeyIdOrList, KeybindingsConfig};
use pi_tui::keys::KeyId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::types::{
    AfterProviderResponseEvent, AgentActivityOutcome, AgentBeforeSettleEvent, AgentEndEvent, AgentSettledEvent,
    AgentStartEvent, AutocompleteProviderFactory, BashResult, BeforeAgentStartEvent, BeforeAgentStartEventResult,
    BeforeAgentStartMessageType, BeforeProviderHeadersEvent, BeforeProviderRequestEvent, BoundaryContextPreview,
    BoundaryResult, BuildSystemPromptOptions, CacheWarmingAction, CacheWarmingDecisionEvent,
    CacheWarmingDecisionEventResult, CancelledResult, CompactOptions, ContextEvent, ContextEventResult, ContextUsage,
    ContextWithSystemEvent, CustomMessage, CustomMessageContent, CustomMessageDraft, CustomUiFactory, EditorFactory,
    EntryRenderer, EventBusHandler, ExecuteToolOptions, ExtensionActions, ExtensionCommandContext,
    ExtensionCommandContextActions, ExtensionContext, ExtensionContextActions, ExtensionContextBackend, ExtensionError,
    ExtensionFlag, ExtensionMode, ExtensionRuntime, ExtensionShortcut, ExtensionToolContext, ExtensionUIContext,
    ExtensionUIDialogOptions, ExtensionWidgetOptions, FlagValue, FooterFactory, ForkOptions, HeaderFactory,
    InputDeliveryBehavior, InputEvent, InputEventResult, InputSource, LoadExtensionsResult, LoadedExtension,
    MarkdownTransformer, McpServersChangeEvent, MessageEndEvent, MessageEndEventResult, MessageRenderer, ModelRegistry,
    ModelSelectEvent, NavigateTreeOptions, NewSessionOptions, NormalizedBuildSystemPromptOptions, NotifyType,
    ProjectTrustContext, ProjectTrustEvent, ProjectTrustEventDecision, ProjectTrustEventResult, ProviderConfig,
    ProviderStreamEvent, RegisteredCommand, RegisteredEventHandler, RegisteredTool, ResolvedCommand,
    ResourceDiagnostic, ResourceDiagnosticType, ResourcesDiscoverEvent, ResourcesDiscoverReason,
    ResourcesDiscoverResult, ScopedModel, SendMessageOptions, SendUserMessageOptions, SessionBeforeCompactEvent,
    SessionBeforeCompactResult, SessionBeforeForkEvent, SessionBeforeForkResult, SessionBeforeSwitchEvent,
    SessionBeforeSwitchResult, SessionBeforeTreeEvent, SessionBeforeTreeResult, SessionBoundaryDraft,
    SessionCompactEvent, SessionCompactFailedEvent, SessionInfoChangedEvent, SessionManager, SessionShutdownEvent,
    SessionStartEvent, SessionTreeEvent, SetThemeResult, SwitchSessionOptions, TerminalInputHandler, Theme, ThemeInfo,
    ThinkingLevelSelectEvent, ToolCallEvent, ToolCallEventResult, ToolDefinition, ToolExecutionEndEvent,
    ToolExecutionStartEvent, ToolExecutionUpdateEvent, ToolRendererResolver, ToolRenderers, ToolResultContentBlock,
    ToolResultEvent, ToolResultEventResult, TurnEndEvent, TurnStartEvent, UIPromptEndEvent, UIPromptKind,
    UIPromptStartEvent, UserBashEvent, UserBashEventResult, VirtualModelDefinition, WidgetFactory,
    WorkingIndicatorOptions, build_system_prompt_from_normalized, normalize_build_system_prompt_options,
};

const DEFAULT_STALE_MESSAGE: &str = "This extension ctx is stale after session replacement or reload. Do not use a captured pi or command ctx after ctx.newSession(), ctx.fork(), ctx.switchSession(), or ctx.reload(). For newSession, fork, and switchSession, move post-replacement work into withSession and use the ctx passed to withSession. For reload, do not use the old ctx after await ctx.reload().";

/// Extension shortcuts compete with canonical keybinding ids from keybindings.json.
/// Only editor-global shortcuts are reserved here. Picker-specific bindings are not.
const RESERVED_KEYBINDINGS_FOR_EXTENSION_CONFLICTS: &[&str] = &[
    "app.interrupt",
    "app.clear",
    "app.exit",
    "app.suspend",
    "app.thinking.cycle",
    "app.model.cycleForward",
    "app.model.cycleBackward",
    "app.model.select",
    "app.tools.expand",
    "app.thinking.toggle",
    "app.editor.external",
    "app.message.copy",
    "app.message.followUp",
    "tui.input.submit",
    "tui.select.confirm",
    "tui.select.cancel",
    "tui.input.copy",
    "tui.editor.deleteToLineEnd",
];

#[derive(Clone)]
struct BuiltInKeyBindingEntry {
    keybinding: String,
    restrict_override: bool,
}

fn build_builtin_keybindings(resolved_keybindings: &KeybindingsConfig) -> IndexMap<KeyId, BuiltInKeyBindingEntry> {
    let mut builtin_keybindings: IndexMap<KeyId, BuiltInKeyBindingEntry> = IndexMap::new();
    for (keybinding, keys_opt) in resolved_keybindings {
        let Some(keys) = keys_opt else {
            continue;
        };
        let key_list: Vec<&str> = match keys {
            KeyIdOrList::One(k) => vec![k.as_str()],
            KeyIdOrList::Many(ks) => ks.iter().map(String::as_str).collect(),
        };
        let restrict_override = RESERVED_KEYBINDINGS_FOR_EXTENSION_CONFLICTS.contains(&keybinding.as_str());
        for key in key_list {
            let normalized_key = key.to_ascii_lowercase();
            // If multiple actions bind the same key, the reserved action wins so extensions
            // remain blocked by reserved shortcuts regardless of iteration order.
            if let Some(existing) = builtin_keybindings.get(&normalized_key)
                && existing.restrict_override
                && !restrict_override
            {
                continue;
            }
            builtin_keybindings.insert(
                normalized_key,
                BuiltInKeyBindingEntry {
                    keybinding: keybinding.clone(),
                    restrict_override,
                },
            );
        }
    }
    builtin_keybindings
}

pub fn validate_user_bash_json_result(value: &Value) -> std::result::Result<UserBashEventResult, String> {
    let invalid = "Invalid user_bash handler result: return undefined for local execution or exactly one valid { operations } or { result } object";
    let Some(obj) = value.as_object() else {
        return Err(invalid.to_string());
    };
    let has_operations = obj.get("operations").is_some_and(|v| !v.is_null())
        || obj.contains_key("operations") && !obj.get("operations").is_some_and(|_| false);
    let has_result = obj.contains_key("result");
    // Match JS: `"operations" !== undefined` and `"result" !== undefined`
    let ops_defined = obj.contains_key("operations");
    let res_defined = obj.contains_key("result");
    if ops_defined == res_defined {
        return Err(invalid.to_string());
    }
    let _ = has_operations;
    let _ = has_result;

    if ops_defined {
        // In JSON from QuickJS, operations only survives if wrapped/marker checked cleanly.
        return Err(invalid.to_string());
    }

    let Some(res_obj) = obj.get("result").and_then(Value::as_object) else {
        return Err(invalid.to_string());
    };
    let Some(output) = res_obj.get("output").and_then(Value::as_str) else {
        return Err(invalid.to_string());
    };
    if !res_obj.contains_key("exitCode") {
        return Err(invalid.to_string());
    }
    let exit_code_val = &res_obj["exitCode"];
    let exit_code = if exit_code_val.is_null() {
        None
    } else if let Some(n) = exit_code_val.as_i64() {
        Some(n)
    } else if let Some(f) = exit_code_val.as_f64() {
        Some(f as i64)
    } else {
        return Err(invalid.to_string());
    };
    let Some(cancelled) = res_obj.get("cancelled").and_then(Value::as_bool) else {
        return Err(invalid.to_string());
    };
    let Some(truncated) = res_obj.get("truncated").and_then(Value::as_bool) else {
        return Err(invalid.to_string());
    };
    let full_output_path = match res_obj.get("fullOutputPath") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => return Err(invalid.to_string()),
    };

    Ok(UserBashEventResult::Result {
        result: BashResult {
            output: output.to_string(),
            exit_code: exit_code.and_then(|c| i32::try_from(c).ok()),
            cancelled,
            truncated,
            full_output_path,
        },
    })
}

/// The message that starts a run (`before_agent_start` user or custom trigger).
#[derive(Clone, Debug, PartialEq)]
pub enum BeforeAgentStartTrigger {
    User {
        prompt: String,
        images: Option<Vec<ImageContent>>,
    },
    Custom {
        message: CustomMessage,
    },
}

/// Combined result from all `before_agent_start` handlers.
#[derive(Clone, Debug)]
pub struct BeforeAgentStartCombinedResult {
    pub messages: Vec<CustomMessageDraft>,
    pub system_prompt_options: NormalizedBuildSystemPromptOptions,
}

/// Discovered resource paths collected from `resources_discover` handlers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredResourcePathEntry {
    pub path: String,
    pub extension_path: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredResourcesResult {
    pub skill_paths: Vec<DiscoveredResourcePathEntry>,
    pub prompt_paths: Vec<DiscoveredResourcePathEntry>,
    pub theme_paths: Vec<DiscoveredResourcePathEntry>,
}

/// Events handled by the generic [`ExtensionRunner::emit`] method.
#[derive(Clone, Debug)]
pub enum RunnerEmitEvent {
    ResourcesDiscover(ResourcesDiscoverEvent),
    McpServersChange(McpServersChangeEvent),
    SessionStart(SessionStartEvent),
    SessionInfoChanged(SessionInfoChangedEvent),
    SessionBeforeSwitch(SessionBeforeSwitchEvent),
    SessionBeforeFork(SessionBeforeForkEvent),
    SessionBeforeCompact(SessionBeforeCompactEvent),
    SessionCompact(SessionCompactEvent),
    SessionCompactFailed(SessionCompactFailedEvent),
    SessionShutdown(SessionShutdownEvent),
    SessionBeforeTree(SessionBeforeTreeEvent),
    SessionTree(SessionTreeEvent),
    AfterProviderResponse(AfterProviderResponseEvent),
    ProviderStreamEvent(ProviderStreamEvent),
    AgentStart(AgentStartEvent),
    AgentEnd(AgentEndEvent),
    AgentSettled(AgentSettledEvent),
    UiPromptStart(UIPromptStartEvent),
    UiPromptEnd(UIPromptEndEvent),
    TurnStart(TurnStartEvent),
    MessageStart(super::types::MessageStartEvent),
    MessageUpdate(super::types::MessageUpdateEvent),
    ToolExecutionStart(ToolExecutionStartEvent),
    ToolExecutionUpdate(ToolExecutionUpdateEvent),
    ToolExecutionEnd(ToolExecutionEndEvent),
    ModelSelect(ModelSelectEvent),
    ThinkingLevelSelect(ThinkingLevelSelectEvent),
}

impl RunnerEmitEvent {
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::ResourcesDiscover(_) => "resources_discover",
            Self::McpServersChange(_) => "mcp_servers_change",
            Self::SessionStart(_) => "session_start",
            Self::SessionInfoChanged(_) => "session_info_changed",
            Self::SessionBeforeSwitch(_) => "session_before_switch",
            Self::SessionBeforeFork(_) => "session_before_fork",
            Self::SessionBeforeCompact(_) => "session_before_compact",
            Self::SessionCompact(_) => "session_compact",
            Self::SessionCompactFailed(_) => "session_compact_failed",
            Self::SessionShutdown(_) => "session_shutdown",
            Self::SessionBeforeTree(_) => "session_before_tree",
            Self::SessionTree(_) => "session_tree",
            Self::AfterProviderResponse(_) => "after_provider_response",
            Self::ProviderStreamEvent(_) => "provider_stream_event",
            Self::AgentStart(_) => "agent_start",
            Self::AgentEnd(_) => "agent_end",
            Self::AgentSettled(_) => "agent_settled",
            Self::UiPromptStart(_) => "ui_prompt_start",
            Self::UiPromptEnd(_) => "ui_prompt_end",
            Self::TurnStart(_) => "turn_start",
            Self::MessageStart(_) => "message_start",
            Self::MessageUpdate(_) => "message_update",
            Self::ToolExecutionStart(_) => "tool_execution_start",
            Self::ToolExecutionUpdate(_) => "tool_execution_update",
            Self::ToolExecutionEnd(_) => "tool_execution_end",
            Self::ModelSelect(_) => "model_select",
            Self::ThinkingLevelSelect(_) => "thinking_level_select",
        }
    }

    fn is_session_before(&self) -> bool {
        matches!(
            self,
            Self::SessionBeforeSwitch(_)
                | Self::SessionBeforeFork(_)
                | Self::SessionBeforeCompact(_)
                | Self::SessionBeforeTree(_)
        )
    }

    fn to_json_value(&self) -> Value {
        let mut obj = match self {
            Self::ResourcesDiscover(e) => serde_json::to_value(e).unwrap_or(Value::Null),
            Self::McpServersChange(e) => serde_json::to_value(e).unwrap_or(Value::Null),
            Self::SessionStart(e) => serde_json::to_value(e).unwrap_or(Value::Null),
            Self::SessionInfoChanged(e) => serde_json::to_value(e).unwrap_or(Value::Null),
            Self::SessionBeforeSwitch(e) => serde_json::to_value(e).unwrap_or(Value::Null),
            Self::SessionBeforeFork(e) => serde_json::to_value(e).unwrap_or(Value::Null),
            Self::SessionBeforeCompact(e) => serde_json::to_value(e).unwrap_or(Value::Null),
            Self::SessionCompact(e) => serde_json::to_value(e).unwrap_or(Value::Null),
            Self::SessionCompactFailed(e) => serde_json::to_value(e).unwrap_or(Value::Null),
            Self::SessionShutdown(e) => serde_json::to_value(e).unwrap_or(Value::Null),
            Self::SessionBeforeTree(e) => serde_json::to_value(e).unwrap_or(Value::Null),
            Self::SessionTree(e) => serde_json::to_value(e).unwrap_or(Value::Null),
            Self::AfterProviderResponse(e) => serde_json::to_value(e).unwrap_or(Value::Null),
            Self::ProviderStreamEvent(e) => serde_json::to_value(e).unwrap_or(Value::Null),
            Self::AgentStart(e) => serde_json::to_value(e).unwrap_or(Value::Null),
            Self::AgentEnd(e) => serde_json::to_value(e).unwrap_or(Value::Null),
            Self::AgentSettled(e) => serde_json::to_value(e).unwrap_or(Value::Null),
            Self::UiPromptStart(e) => serde_json::to_value(e).unwrap_or(Value::Null),
            Self::UiPromptEnd(e) => serde_json::to_value(e).unwrap_or(Value::Null),
            Self::TurnStart(e) => serde_json::to_value(e).unwrap_or(Value::Null),
            Self::MessageStart(e) => serde_json::to_value(e).unwrap_or(Value::Null),
            Self::MessageUpdate(e) => serde_json::to_value(e).unwrap_or(Value::Null),
            Self::ToolExecutionStart(e) => serde_json::to_value(e).unwrap_or(Value::Null),
            Self::ToolExecutionUpdate(e) => serde_json::to_value(e).unwrap_or(Value::Null),
            Self::ToolExecutionEnd(e) => serde_json::to_value(e).unwrap_or(Value::Null),
            Self::ModelSelect(e) => serde_json::to_value(e).unwrap_or(Value::Null),
            Self::ThinkingLevelSelect(e) => serde_json::to_value(e).unwrap_or(Value::Null),
        };
        if let Value::Object(ref mut map) = obj {
            map.insert("type".to_string(), Value::String(self.event_type().to_string()));
        }
        obj
    }

    fn into_any(self) -> Arc<dyn Any + Send + Sync> {
        match self {
            Self::ResourcesDiscover(e) => Arc::new(e),
            Self::McpServersChange(e) => Arc::new(e),
            Self::SessionStart(e) => Arc::new(e),
            Self::SessionInfoChanged(e) => Arc::new(e),
            Self::SessionBeforeSwitch(e) => Arc::new(e),
            Self::SessionBeforeFork(e) => Arc::new(e),
            Self::SessionBeforeCompact(e) => Arc::new(e),
            Self::SessionCompact(e) => Arc::new(e),
            Self::SessionCompactFailed(e) => Arc::new(e),
            Self::SessionShutdown(e) => Arc::new(e),
            Self::SessionBeforeTree(e) => Arc::new(e),
            Self::SessionTree(e) => Arc::new(e),
            Self::AfterProviderResponse(e) => Arc::new(e),
            Self::ProviderStreamEvent(e) => Arc::new(e),
            Self::AgentStart(e) => Arc::new(e),
            Self::AgentEnd(e) => Arc::new(e),
            Self::AgentSettled(e) => Arc::new(e),
            Self::UiPromptStart(e) => Arc::new(e),
            Self::UiPromptEnd(e) => Arc::new(e),
            Self::TurnStart(e) => Arc::new(e),
            Self::MessageStart(e) => Arc::new(e),
            Self::MessageUpdate(e) => Arc::new(e),
            Self::ToolExecutionStart(e) => Arc::new(e),
            Self::ToolExecutionUpdate(e) => Arc::new(e),
            Self::ToolExecutionEnd(e) => Arc::new(e),
            Self::ModelSelect(e) => Arc::new(e),
            Self::ThinkingLevelSelect(e) => Arc::new(e),
        }
    }
}

macro_rules! impl_from_runner_event {
    ($($variant:ident($ty:ty)),* $(,)?) => {
        $(
            impl From<$ty> for RunnerEmitEvent {
                fn from(value: $ty) -> Self {
                    Self::$variant(value)
                }
            }
        )*
    };
}

impl_from_runner_event!(
    ResourcesDiscover(ResourcesDiscoverEvent),
    McpServersChange(McpServersChangeEvent),
    SessionStart(SessionStartEvent),
    SessionInfoChanged(SessionInfoChangedEvent),
    SessionBeforeSwitch(SessionBeforeSwitchEvent),
    SessionBeforeFork(SessionBeforeForkEvent),
    SessionBeforeCompact(SessionBeforeCompactEvent),
    SessionCompact(SessionCompactEvent),
    SessionCompactFailed(SessionCompactFailedEvent),
    SessionShutdown(SessionShutdownEvent),
    SessionBeforeTree(SessionBeforeTreeEvent),
    SessionTree(SessionTreeEvent),
    AfterProviderResponse(AfterProviderResponseEvent),
    ProviderStreamEvent(ProviderStreamEvent),
    AgentStart(AgentStartEvent),
    AgentEnd(AgentEndEvent),
    AgentSettled(AgentSettledEvent),
    UiPromptStart(UIPromptStartEvent),
    UiPromptEnd(UIPromptEndEvent),
    TurnStart(TurnStartEvent),
    MessageStart(super::types::MessageStartEvent),
    MessageUpdate(super::types::MessageUpdateEvent),
    ToolExecutionStart(ToolExecutionStartEvent),
    ToolExecutionUpdate(ToolExecutionUpdateEvent),
    ToolExecutionEnd(ToolExecutionEndEvent),
    ModelSelect(ModelSelectEvent),
    ThinkingLevelSelect(ThinkingLevelSelectEvent),
);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SessionBeforeEventResult {
    Switch(SessionBeforeSwitchResult),
    Fork(SessionBeforeForkResult),
    Compact(SessionBeforeCompactResult),
    Tree(SessionBeforeTreeResult),
}

impl SessionBeforeEventResult {
    pub fn cancel(&self) -> bool {
        match self {
            Self::Switch(r) => r.cancel.unwrap_or(false),
            Self::Fork(r) => r.cancel.unwrap_or(false),
            Self::Compact(r) => r.cancel.unwrap_or(false),
            Self::Tree(r) => r.cancel.unwrap_or(false),
        }
    }

    pub fn into_switch(self) -> Option<SessionBeforeSwitchResult> {
        match self {
            Self::Switch(r) => Some(r),
            _ => None,
        }
    }

    pub fn into_fork(self) -> Option<SessionBeforeForkResult> {
        match self {
            Self::Fork(r) => Some(r),
            _ => None,
        }
    }

    pub fn into_compact(self) -> Option<SessionBeforeCompactResult> {
        match self {
            Self::Compact(r) => Some(r),
            _ => None,
        }
    }

    pub fn into_tree(self) -> Option<SessionBeforeTreeResult> {
        match self {
            Self::Tree(r) => Some(r),
            _ => None,
        }
    }
}

pub type ExtensionErrorListener = Arc<dyn Fn(ExtensionError) + Send + Sync>;

#[derive(Clone, Debug)]
pub enum BoundaryBaseEvent {
    TurnEnd {
        outcome: AgentActivityOutcome,
        turn_index: i64,
        message: AgentMessage,
        tool_results: Vec<ToolResultMessage>,
        message_entry_id: String,
        tool_result_entry_ids: Vec<String>,
    },
    AgentBeforeSettle {
        outcome: AgentActivityOutcome,
    },
}

impl BoundaryBaseEvent {
    fn event_type(&self) -> &'static str {
        match self {
            Self::TurnEnd { .. } => "turn_end",
            Self::AgentBeforeSettle { .. } => "agent_before_settle",
        }
    }

    fn into_event(
        &self,
        entries: Vec<SessionBoundaryDraft>,
        should_continue: bool,
        context: BoundaryContextPreview,
    ) -> Arc<dyn Any + Send + Sync> {
        match self {
            Self::TurnEnd {
                outcome,
                turn_index,
                message,
                tool_results,
                message_entry_id,
                tool_result_entry_ids,
            } => Arc::new(TurnEndEvent {
                entries,
                r#continue: should_continue,
                context,
                outcome: *outcome,
                turn_index: *turn_index,
                message: message.clone(),
                tool_results: tool_results.clone(),
                message_entry_id: message_entry_id.clone(),
                tool_result_entry_ids: tool_result_entry_ids.clone(),
            }),
            Self::AgentBeforeSettle { outcome } => Arc::new(AgentBeforeSettleEvent {
                entries,
                r#continue: should_continue,
                context,
                outcome: *outcome,
            }),
        }
    }

    fn to_json_value(
        &self,
        entries: Vec<SessionBoundaryDraft>,
        should_continue: bool,
        context: BoundaryContextPreview,
    ) -> Value {
        let mut val = match self {
            Self::TurnEnd {
                outcome,
                turn_index,
                message,
                tool_results,
                message_entry_id,
                tool_result_entry_ids,
            } => serde_json::to_value(TurnEndEvent {
                entries,
                r#continue: should_continue,
                context,
                outcome: *outcome,
                turn_index: *turn_index,
                message: message.clone(),
                tool_results: tool_results.clone(),
                message_entry_id: message_entry_id.clone(),
                tool_result_entry_ids: tool_result_entry_ids.clone(),
            })
            .unwrap_or(Value::Null),
            Self::AgentBeforeSettle { outcome } => serde_json::to_value(AgentBeforeSettleEvent {
                entries,
                r#continue: should_continue,
                context,
                outcome: *outcome,
            })
            .unwrap_or(Value::Null),
        };
        if let Value::Object(ref mut map) = val {
            map.insert("type".to_string(), Value::String(self.event_type().to_string()));
        }
        val
    }
}

#[derive(Clone, Debug)]
pub struct BoundaryDispatchResult {
    pub entries: Vec<SessionBoundaryDraft>,
    pub r#continue: bool,
    pub context: BoundaryContextPreview,
    pub valid: bool,
}

#[derive(Clone, Default)]
pub struct ProviderActions {
    pub register_provider: Option<Arc<dyn Fn(&str, ProviderConfig) + Send + Sync>>,
    pub register_native_provider: Option<Arc<dyn Fn(Arc<dyn Provider>) + Send + Sync>>,
    pub unregister_provider: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    pub register_virtual_model: Option<Arc<dyn Fn(VirtualModelDefinition) + Send + Sync>>,
    pub unregister_virtual_model: Option<Arc<dyn Fn(&str, &str) + Send + Sync>>,
}

pub type NewSessionHandler =
    Arc<dyn Fn(Option<NewSessionOptions>) -> BoxFuture<pi_js::Result<CancelledResult>> + Send + Sync>;
pub type ForkHandler =
    Arc<dyn Fn(&str, Option<ForkOptions>) -> BoxFuture<pi_js::Result<CancelledResult>> + Send + Sync>;
pub type NavigateTreeHandler =
    Arc<dyn Fn(&str, Option<NavigateTreeOptions>) -> BoxFuture<pi_js::Result<CancelledResult>> + Send + Sync>;
pub type SwitchSessionHandler =
    Arc<dyn Fn(&str, Option<SwitchSessionOptions>) -> BoxFuture<pi_js::Result<CancelledResult>> + Send + Sync>;
pub type ReloadHandler = Arc<dyn Fn() -> BoxFuture<pi_js::Result<()>> + Send + Sync>;
pub type ShutdownHandler = Arc<dyn Fn() + Send + Sync>;

/// Helper function to emit `session_shutdown` event to extensions.
/// Returns true if the event was emitted, false if there were no handlers.
pub async fn emit_session_shutdown_event(extension_runner: &ExtensionRunner, event: SessionShutdownEvent) -> bool {
    if extension_runner.has_handlers("session_shutdown") {
        extension_runner.emit(event).await;
        true
    } else {
        false
    }
}

fn snapshot_event_handlers(
    extensions: &[LoadedExtension],
    event_type: &str,
) -> Vec<(LoadedExtension, Vec<RegisteredEventHandler>)> {
    extensions
        .iter()
        .map(|ext| {
            let handlers = ext
                .handlers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(event_type)
                .map(|list| list.iter().map(|(_, h)| h.clone()).collect())
                .unwrap_or_default();
            (ext.clone(), handlers)
        })
        .collect()
}

struct AgentTranscriptMsg<'a>(&'a AgentMessage);

impl TranscriptMessage for AgentTranscriptMsg<'_> {
    fn as_system_message(&self) -> Option<&SystemMessage> {
        match self.0 {
            AgentMessage::Message(Message::System(sys)) => Some(sys),
            _ => None,
        }
    }
}

/// Re-attach the prompt and tool state after a `context` handler. Handlers only see the
/// conversation; the system messages belong to Pi. An unchanged conversation keeps every
/// system message in place, so models with mid-conversation support keep their cached
/// prefix. A changed one gets the replayed prompt sections and tool declarations as one
/// leading system message, so pruning, windowing, or slicing from a compaction summary
/// cannot drop them.
fn restore_system_messages(
    current: Vec<AgentMessage>,
    visible: &[AgentMessage],
    returned: Vec<AgentMessage>,
) -> Vec<AgentMessage> {
    if returned == visible {
        return current;
    }
    let wrapped: Vec<AgentTranscriptMsg<'_>> = current.iter().map(AgentTranscriptMsg).collect();
    if let Some(head) = get_current_system_message(&wrapped) {
        let mut out = Vec::with_capacity(returned.len() + 1);
        out.push(AgentMessage::Message(Message::System(head)));
        out.extend(returned);
        out
    } else {
        returned
    }
}

#[derive(Clone, Debug, Default)]
pub struct EmitProjectTrustOutput {
    pub result: Option<ProjectTrustEventResult>,
    pub errors: Vec<ExtensionError>,
}

pub async fn emit_project_trust_event(
    extensions_result: &LoadExtensionsResult,
    event: ProjectTrustEvent,
    ctx: ProjectTrustContext,
) -> EmitProjectTrustOutput {
    let mut errors: Vec<ExtensionError> = Vec::new();
    for ext in &extensions_result.extensions {
        let handlers = {
            let guard = ext.project_trust_handlers.lock().unwrap_or_else(|e| e.into_inner());
            guard.iter().map(|(_, h)| Arc::clone(h)).collect::<Vec<_>>()
        };
        for handler in handlers {
            match handler(event.clone(), ctx.clone()).await {
                Ok(res) => {
                    if res.trusted == ProjectTrustEventDecision::Undecided {
                        continue;
                    }
                    return EmitProjectTrustOutput {
                        result: Some(res),
                        errors,
                    };
                }
                Err(err) => {
                    errors.push(ExtensionError {
                        extension_path: ext.path.clone(),
                        event: "project_trust".to_string(),
                        error: err.to_string(),
                        stack: None,
                    });
                }
            }
        }
    }
    EmitProjectTrustOutput { result: None, errors }
}

pub(crate) fn noop_ui_context() -> Arc<dyn ExtensionUIContext> {
    Arc::new(NoOpUIContext)
}

struct NoOpUIContext;

#[async_trait]
impl ExtensionUIContext for NoOpUIContext {
    async fn select(
        &self,
        _title: &str,
        _options: Vec<String>,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        Ok(None)
    }

    async fn confirm(
        &self,
        _title: &str,
        _message: &str,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<bool> {
        Ok(false)
    }

    async fn input(
        &self,
        _title: &str,
        _placeholder: Option<&str>,
        _opts: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        Ok(None)
    }

    fn notify(&self, _message: &str, _notify_type: Option<NotifyType>) {}

    fn on_terminal_input(&self, _handler: TerminalInputHandler) -> Unsubscribe {
        Unsubscribe::noop()
    }

    fn set_status(&self, _key: &str, _text: Option<&str>) {}

    fn set_working_message(&self, _message: Option<&str>) {}

    fn set_working_visible(&self, _visible: bool) {}

    fn set_working_indicator(&self, _options: Option<WorkingIndicatorOptions>) {}

    fn set_hidden_thinking_label(&self, _label: Option<&str>) {}

    fn set_widget_lines(&self, _key: &str, _content: Option<Vec<String>>, _options: Option<ExtensionWidgetOptions>) {}

    fn set_widget_factory(
        &self,
        _key: &str,
        _factory: Option<WidgetFactory>,
        _options: Option<ExtensionWidgetOptions>,
    ) {
    }

    fn set_footer(&self, _factory: Option<FooterFactory>) {}

    fn set_header(&self, _factory: Option<HeaderFactory>) {}

    fn set_title(&self, _title: &str) {}

    async fn custom(
        &self,
        _factory: CustomUiFactory,
        _options: Option<ExtensionCustomOptions>,
    ) -> pi_js::Result<Value> {
        Ok(Value::Null)
    }

    fn paste_to_editor(&self, _text: &str) {}

    fn set_editor_text(&self, _text: &str) {}

    fn get_editor_text(&self) -> String {
        String::new()
    }

    async fn editor(&self, _title: &str, _prefill: Option<&str>) -> pi_js::Result<Option<String>> {
        Ok(None)
    }

    fn add_autocomplete_provider(&self, _factory: AutocompleteProviderFactory) {}

    fn set_editor_component(&self, _factory: Option<EditorFactory>) {}

    fn get_editor_component(&self) -> Option<EditorFactory> {
        None
    }

    fn theme(&self) -> Theme {
        Theme::default()
    }

    fn get_all_themes(&self) -> Vec<ThemeInfo> {
        Vec::new()
    }

    fn get_theme(&self, _name: &str) -> Option<Theme> {
        None
    }

    fn set_theme_by_name(&self, _name: &str) -> SetThemeResult {
        SetThemeResult {
            success: false,
            error: Some("UI not available".to_string()),
        }
    }

    fn set_theme(&self, _theme: Theme) -> SetThemeResult {
        SetThemeResult {
            success: false,
            error: Some("UI not available".to_string()),
        }
    }

    fn get_tools_expanded(&self) -> bool {
        false
    }

    fn set_tools_expanded(&self, _expanded: bool) {}
}

pub fn no_op_ui_context() -> Arc<dyn ExtensionUIContext> {
    Arc::new(NoOpUIContext)
}

use super::types::ExtensionCustomOptions;

struct RunnerCallbacks {
    get_model: Arc<dyn Fn() -> Option<Model> + Send + Sync>,
    get_scoped_models: Arc<dyn Fn() -> Vec<ScopedModel> + Send + Sync>,
    is_idle: Arc<dyn Fn() -> bool + Send + Sync>,
    is_project_trusted: Arc<dyn Fn() -> bool + Send + Sync>,
    get_signal: Arc<dyn Fn() -> Option<AbortSignal> + Send + Sync>,
    abort: Arc<dyn Fn() + Send + Sync>,
    has_pending_messages: Arc<dyn Fn() -> bool + Send + Sync>,
    get_context_usage: Arc<dyn Fn() -> Option<ContextUsage> + Send + Sync>,
    compact: Arc<dyn Fn(Option<CompactOptions>) + Send + Sync>,
    get_system_prompt: Arc<dyn Fn() -> String + Send + Sync>,
    get_system_prompt_options: Arc<dyn Fn() -> BuildSystemPromptOptions + Send + Sync>,
    execute_tool:
        Option<Arc<dyn Fn(&str, &str, Value, ExecuteToolOptions) -> BoxFuture<AgentToolCallOutcome> + Send + Sync>>,
    get_callable_tools: Arc<dyn Fn() -> Vec<AgentTool> + Send + Sync>,
    wait_for_idle: Arc<dyn Fn() -> BoxFuture<pi_js::Result<()>> + Send + Sync>,
    new_session: NewSessionHandler,
    fork: ForkHandler,
    navigate_tree: NavigateTreeHandler,
    switch_session: SwitchSessionHandler,
    reload: ReloadHandler,
    shutdown: ShutdownHandler,
}

#[derive(Default)]
struct UiPromptState {
    depth: usize,
    active: Option<(UIPromptKind, Option<String>)>,
}

struct RunnerContextState {
    runtime: ExtensionRuntime,
    noop_ui: Arc<dyn ExtensionUIContext>,
    ui_context: Mutex<Option<Arc<dyn ExtensionUIContext>>>,
    mode: Mutex<ExtensionMode>,
    cwd: String,
    session_manager: SessionManager,
    model_registry: ModelRegistry,
    callbacks: Mutex<RunnerCallbacks>,
    stale_message: Mutex<Option<String>>,
}

impl RunnerContextState {
    fn assert_active(&self) -> pi_js::Result<()> {
        if let Some(msg) = self.stale_message.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            return Err(pi_js::Error::msg(msg));
        }
        Ok(())
    }

    fn get_ui_context(&self) -> Arc<dyn ExtensionUIContext> {
        self.ui_context
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_else(|| Arc::clone(&self.noop_ui))
    }

    fn has_ui(&self) -> bool {
        self.ui_context.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }
}

struct ExtensionRunnerInner {
    extensions: Vec<LoadedExtension>,
    ctx_state: Arc<RunnerContextState>,
    error_listeners: Mutex<IndexMap<u64, ExtensionErrorListener>>,
    next_listener_id: AtomicU64,
    reported_mcp_servers: Mutex<IndexSet<String>>,
    shortcut_diagnostics: Mutex<Vec<ResourceDiagnostic>>,
    command_diagnostics: Mutex<Vec<ResourceDiagnostic>>,
    ui_prompt_state: Mutex<UiPromptState>,
}

#[derive(Clone)]
pub struct ExtensionRunner {
    inner: Arc<ExtensionRunnerInner>,
}

impl ExtensionRunner {
    pub fn new(
        extensions: Vec<LoadedExtension>,
        runtime: ExtensionRuntime,
        cwd: impl Into<String>,
        session_manager: SessionManager,
        model_registry: ModelRegistry,
    ) -> Self {
        let cwd_str = cwd.into();
        let cwd_for_opts = cwd_str.clone();
        let callbacks = RunnerCallbacks {
            get_model: Arc::new(|| None),
            get_scoped_models: Arc::new(Vec::new),
            is_idle: Arc::new(|| true),
            is_project_trusted: Arc::new(|| true),
            get_signal: Arc::new(|| None),
            abort: Arc::new(|| {}),
            has_pending_messages: Arc::new(|| false),
            get_context_usage: Arc::new(|| None),
            compact: Arc::new(|_| {}),
            get_system_prompt: Arc::new(String::new),
            get_system_prompt_options: Arc::new(move || BuildSystemPromptOptions {
                cwd: cwd_for_opts.clone(),
                ..Default::default()
            }),
            execute_tool: None,
            get_callable_tools: Arc::new(Vec::new),
            wait_for_idle: Arc::new(|| Box::pin(async { Ok(()) })),
            new_session: Arc::new(|_| Box::pin(async { Ok(CancelledResult { cancelled: false }) })),
            fork: Arc::new(|_, _| Box::pin(async { Ok(CancelledResult { cancelled: false }) })),
            navigate_tree: Arc::new(|_, _| Box::pin(async { Ok(CancelledResult { cancelled: false }) })),
            switch_session: Arc::new(|_, _| Box::pin(async { Ok(CancelledResult { cancelled: false }) })),
            reload: Arc::new(|| Box::pin(async { Ok(()) })),
            shutdown: Arc::new(|| {}),
        };
        let ctx_state = Arc::new(RunnerContextState {
            runtime,
            noop_ui: Arc::new(NoOpUIContext),
            ui_context: Mutex::new(None),
            mode: Mutex::new(ExtensionMode::Print),
            cwd: cwd_str,
            session_manager,
            model_registry,
            callbacks: Mutex::new(callbacks),
            stale_message: Mutex::new(None),
        });
        Self {
            inner: Arc::new(ExtensionRunnerInner {
                extensions,
                ctx_state,
                error_listeners: Mutex::new(IndexMap::new()),
                next_listener_id: AtomicU64::new(1),
                reported_mcp_servers: Mutex::new(IndexSet::new()),
                shortcut_diagnostics: Mutex::new(Vec::new()),
                command_diagnostics: Mutex::new(Vec::new()),
                ui_prompt_state: Mutex::new(UiPromptState::default()),
            }),
        }
    }

    pub fn bind_core(
        &self,
        actions: ExtensionActions,
        context_actions: ExtensionContextActions,
        provider_actions: Option<ProviderActions>,
    ) {
        let cs = &self.inner.ctx_state;
        *cs.runtime.inner.actions.lock().unwrap_or_else(|e| e.into_inner()) = actions;
        let state_for_ctx = Arc::clone(cs);
        *cs.runtime
            .inner
            .create_context_fn
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Arc::new(move || {
            Ok(ExtensionContext::from_backend(Arc::new(RunnerContextBackend {
                state: Arc::clone(&state_for_ctx),
            })))
        });

        let cwd_for_opts = cs.cwd.clone();
        {
            let mut cbs = cs.callbacks.lock().unwrap_or_else(|e| e.into_inner());
            cbs.get_model = context_actions.get_model;
            cbs.get_scoped_models = context_actions.get_scoped_models;
            cbs.is_idle = context_actions.is_idle;
            cbs.is_project_trusted = context_actions.is_project_trusted;
            cbs.get_signal = context_actions.get_signal;
            cbs.abort = context_actions.abort;
            cbs.has_pending_messages = context_actions.has_pending_messages;
            cbs.shutdown = context_actions.shutdown;
            cbs.get_context_usage = context_actions.get_context_usage;
            cbs.compact = context_actions.compact;
            cbs.get_system_prompt = context_actions.get_system_prompt;
            cbs.get_system_prompt_options = context_actions.get_system_prompt_options.unwrap_or_else(|| {
                Arc::new(move || BuildSystemPromptOptions {
                    cwd: cwd_for_opts.clone(),
                    ..Default::default()
                })
            });
            cbs.execute_tool = context_actions.execute_tool;
            cbs.get_callable_tools = context_actions.get_callable_tools.unwrap_or_else(|| Arc::new(Vec::new));
        }

        let runner_weak = Arc::downgrade(&self.inner);
        cs.runtime.inner.mcp_servers.set_change_listener(Some(Arc::new(move || {
            let Some(inner) = runner_weak.upgrade() else {
                return;
            };
            let runner = ExtensionRunner { inner };
            let servers = runner.inner.ctx_state.runtime.inner.mcp_servers.list();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let runner_emit = runner.clone();
                handle.spawn(async move {
                    let _ = runner_emit.emit(McpServersChangeEvent { servers }).await;
                });
            }
            runner.report_unhandled_mcp_servers();
        })));

        let prov = provider_actions.unwrap_or_default();

        // Flush queued provider registrations
        let pending_providers = std::mem::take(
            &mut *cs
                .runtime
                .inner
                .pending_provider_registrations
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        );
        for reg in pending_providers {
            if let Some(ref f) = prov.register_provider {
                f(&reg.name, reg.config);
            } else if let Err(err) = cs.model_registry.register_provider(&reg.name, reg.config.into()) {
                self.emit_error(ExtensionError {
                    extension_path: reg.extension_path,
                    event: "bind_core".to_string(),
                    error: err.to_string(),
                    stack: None,
                });
            }
        }

        let pending_native = std::mem::take(
            &mut *cs
                .runtime
                .inner
                .pending_native_provider_registrations
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        );
        for reg in pending_native {
            if let Some(ref f) = prov.register_native_provider {
                f(reg.provider);
            } else if let Err(err) = cs.model_registry.register_native_provider(reg.provider) {
                self.emit_error(ExtensionError {
                    extension_path: reg.extension_path,
                    event: "bind_core".to_string(),
                    error: err.to_string(),
                    stack: None,
                });
            }
        }

        let pending_vm = std::mem::take(
            &mut *cs
                .runtime
                .inner
                .pending_virtual_model_registrations
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        );
        for reg in pending_vm {
            if let Some(ref f) = prov.register_virtual_model {
                f(reg.definition);
            } else if let Err(err) = cs.model_registry.register_virtual_model(reg.definition) {
                self.emit_error(ExtensionError {
                    extension_path: reg.extension_path,
                    event: "bind_core".to_string(),
                    error: err.to_string(),
                    stack: None,
                });
            }
        }

        // Immediate provider registration/unregistration from now on
        let mr_reg = cs.model_registry.clone();
        let cb_reg = prov.register_provider.clone();
        let runner_weak_reg = Arc::downgrade(&self.inner);
        *cs.runtime
            .inner
            .register_provider_fn
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Arc::new(move |name, cfg, ext_path| {
            if let Some(ref f) = cb_reg {
                f(name, cfg);
            } else if let Err(err) = mr_reg.register_provider(name, cfg.into())
                && let Some(inner) = runner_weak_reg.upgrade()
            {
                ExtensionRunner { inner }.emit_error(ExtensionError {
                    extension_path: ext_path.to_string(),
                    event: "register_provider".to_string(),
                    error: err.to_string(),
                    stack: None,
                });
            }
        });

        let mr_nreg = cs.model_registry.clone();
        let cb_nreg = prov.register_native_provider.clone();
        let runner_weak_nreg = Arc::downgrade(&self.inner);
        *cs.runtime
            .inner
            .register_native_provider_fn
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Arc::new(move |provider, ext_path| {
            if let Some(ref f) = cb_nreg {
                f(provider);
            } else if let Err(err) = mr_nreg.register_native_provider(provider)
                && let Some(inner) = runner_weak_nreg.upgrade()
            {
                ExtensionRunner { inner }.emit_error(ExtensionError {
                    extension_path: ext_path.to_string(),
                    event: "register_native_provider".to_string(),
                    error: err.to_string(),
                    stack: None,
                });
            }
        });

        let mr_unreg = cs.model_registry.clone();
        let cb_unreg = prov.unregister_provider.clone();
        *cs.runtime
            .inner
            .unregister_provider_fn
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Arc::new(move |name, _ext_path| {
            if let Some(ref f) = cb_unreg {
                f(name);
            } else {
                mr_unreg.unregister_provider(name);
            }
        });

        let mr_vm = cs.model_registry.clone();
        let cb_vm = prov.register_virtual_model;
        let runner_weak_vm = Arc::downgrade(&self.inner);
        *cs.runtime
            .inner
            .register_virtual_model_fn
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Arc::new(move |def, ext_path| {
            if let Some(ref f) = cb_vm {
                f(def);
            } else if let Err(err) = mr_vm.register_virtual_model(def)
                && let Some(inner) = runner_weak_vm.upgrade()
            {
                ExtensionRunner { inner }.emit_error(ExtensionError {
                    extension_path: ext_path.to_string(),
                    event: "register_virtual_model".to_string(),
                    error: err.to_string(),
                    stack: None,
                });
            }
        });

        let mr_uvm = cs.model_registry.clone();
        let cb_uvm = prov.unregister_virtual_model;
        *cs.runtime
            .inner
            .unregister_virtual_model_fn
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Arc::new(move |provider, id| {
            if let Some(ref f) = cb_uvm {
                f(provider, id);
            } else {
                mr_uvm.unregister_virtual_model(provider, id);
            }
        });
    }

    pub fn bind_command_context(&self, actions: Option<ExtensionCommandContextActions>) {
        let mut cbs = self.inner.ctx_state.callbacks.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(act) = actions {
            cbs.wait_for_idle = act.wait_for_idle;
            cbs.new_session = act.new_session;
            cbs.fork = act.fork;
            cbs.navigate_tree = act.navigate_tree;
            cbs.switch_session = act.switch_session;
            cbs.reload = act.reload;
        } else {
            cbs.wait_for_idle = Arc::new(|| Box::pin(async { Ok(()) }));
            cbs.new_session = Arc::new(|_| Box::pin(async { Ok(CancelledResult { cancelled: false }) }));
            cbs.fork = Arc::new(|_, _| Box::pin(async { Ok(CancelledResult { cancelled: false }) }));
            cbs.navigate_tree = Arc::new(|_, _| Box::pin(async { Ok(CancelledResult { cancelled: false }) }));
            cbs.switch_session = Arc::new(|_, _| Box::pin(async { Ok(CancelledResult { cancelled: false }) }));
            cbs.reload = Arc::new(|| Box::pin(async { Ok(()) }));
        }
    }

    pub fn set_ui_context(&self, ui_context: Option<Arc<dyn ExtensionUIContext>>, mode: ExtensionMode) {
        let wrapped = ui_context.map(|ui| self.wrap_ui_prompt_context(ui));
        *self
            .inner
            .ctx_state
            .ui_context
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = wrapped;
        *self.inner.ctx_state.mode.lock().unwrap_or_else(|e| e.into_inner()) = mode;
    }

    fn wrap_ui_prompt_context(&self, ui: Arc<dyn ExtensionUIContext>) -> Arc<dyn ExtensionUIContext> {
        Arc::new(UiPromptWrappedContext {
            runner: Arc::downgrade(&self.inner),
            inner: ui,
        })
    }

    async fn with_ui_prompt<T, Fut>(&self, kind: UIPromptKind, title: Option<String>, fut: Fut) -> T
    where
        Fut: Future<Output = T>,
    {
        {
            let mut state = self.inner.ui_prompt_state.lock().unwrap_or_else(|e| e.into_inner());
            let outer = state.depth == 0;
            state.depth += 1;
            if outer {
                state.active = Some((kind, title.clone()));
                drop(state);
                self.emit_ui_prompt_event(RunnerEmitEvent::UiPromptStart(UIPromptStartEvent {
                    reason: "ui_prompt".to_string(),
                    kind,
                    title: title.clone().filter(|t| !t.is_empty()),
                }));
            }
        }
        let _finish = UiPromptFinishGuard {
            runner: Arc::downgrade(&self.inner),
            kind,
            title,
        };
        fut.await
    }

    fn finish_ui_prompt(&self, kind: UIPromptKind, title: Option<String>) {
        let mut state = self.inner.ui_prompt_state.lock().unwrap_or_else(|e| e.into_inner());
        if state.depth > 1 {
            state.depth -= 1;
            return;
        }
        state.depth = 0;
        let active = state.active.take().unwrap_or((kind, title));
        drop(state);
        self.emit_ui_prompt_event(RunnerEmitEvent::UiPromptEnd(UIPromptEndEvent {
            reason: "ui_prompt".to_string(),
            kind: active.0,
            title: active.1.filter(|t| !t.is_empty()),
        }));
    }

    fn emit_ui_prompt_event(&self, event: RunnerEmitEvent) {
        let runner = self.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                tokio::task::yield_now().await;
                let _ = runner.emit(event).await;
            });
        }
    }

    pub fn get_ui_context(&self) -> Arc<dyn ExtensionUIContext> {
        self.inner.ctx_state.get_ui_context()
    }

    pub fn has_ui(&self) -> bool {
        self.inner.ctx_state.has_ui()
    }

    pub fn get_extension_paths(&self) -> Vec<String> {
        self.inner.extensions.iter().map(|e| e.path.clone()).collect()
    }

    /// Get all registered tools from all extensions (first registration per name wins).
    pub fn get_all_registered_tools(&self) -> Vec<RegisteredTool> {
        let mut tools_by_name: IndexMap<String, RegisteredTool> = IndexMap::new();
        for ext in &self.inner.extensions {
            let tools = {
                let guard = ext.tools.lock().unwrap_or_else(|e| e.into_inner());
                guard.values().cloned().collect::<Vec<_>>()
            };
            for tool in tools {
                tools_by_name.entry(tool.definition.name.clone()).or_insert(tool);
            }
        }
        tools_by_name.into_values().collect()
    }

    /// Get a tool definition by name. Returns `None` if not found.
    pub fn get_tool_definition(&self, tool_name: &str) -> Option<ToolDefinition> {
        for ext in &self.inner.extensions {
            let guard = ext.tools.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(tool) = guard.get(tool_name) {
                return Some(tool.definition.clone());
            }
        }
        None
    }

    pub fn get_flags(&self) -> IndexMap<String, ExtensionFlag> {
        let mut all_flags: IndexMap<String, ExtensionFlag> = IndexMap::new();
        for ext in &self.inner.extensions {
            let guard = ext.flags.lock().unwrap_or_else(|e| e.into_inner());
            for (name, flag) in guard.iter() {
                if !all_flags.contains_key(name) {
                    all_flags.insert(name.clone(), flag.clone());
                }
            }
        }
        all_flags
    }

    pub fn set_flag_value(&self, name: &str, value: FlagValue) {
        self.inner
            .ctx_state
            .runtime
            .inner
            .flag_values
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.to_string(), value);
    }

    pub fn get_flag_values(&self) -> IndexMap<String, FlagValue> {
        self.inner
            .ctx_state
            .runtime
            .inner
            .flag_values
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn get_shortcuts(&self, resolved_keybindings: &KeybindingsConfig) -> IndexMap<KeyId, ExtensionShortcut> {
        *self
            .inner
            .shortcut_diagnostics
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Vec::new();
        let builtin_keybindings = build_builtin_keybindings(resolved_keybindings);
        let mut extension_shortcuts: IndexMap<KeyId, ExtensionShortcut> = IndexMap::new();
        let has_ui = self.has_ui();
        let mut new_diags: Vec<ResourceDiagnostic> = Vec::new();

        let mut add_diagnostic = |message: String, extension_path: String| {
            new_diags.push(ResourceDiagnostic {
                r#type: ResourceDiagnosticType::Warning,
                message: message.clone(),
                path: Some(extension_path),
                collision: None,
            });
            if !has_ui {
                pi_js::console::warn(&message);
            }
        };

        for ext in &self.inner.extensions {
            let shortcuts = {
                let guard = ext.shortcuts.lock().unwrap_or_else(|e| e.into_inner());
                guard.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<Vec<_>>()
            };
            for (key, shortcut) in shortcuts {
                let normalized_key = key.to_ascii_lowercase();
                if let Some(built_in) = builtin_keybindings.get(&normalized_key) {
                    if built_in.restrict_override {
                        add_diagnostic(
                            format!(
                                "Extension shortcut '{}' from {} conflicts with built-in shortcut. Skipping.",
                                key, shortcut.extension_path
                            ),
                            shortcut.extension_path.clone(),
                        );
                        continue;
                    }
                    add_diagnostic(
                        format!(
                            "Extension shortcut conflict: '{}' is built-in shortcut for {} and {}. Using {}.",
                            key, built_in.keybinding, shortcut.extension_path, shortcut.extension_path
                        ),
                        shortcut.extension_path.clone(),
                    );
                }
                if let Some(existing) = extension_shortcuts.get(&normalized_key) {
                    add_diagnostic(
                        format!(
                            "Extension shortcut conflict: '{}' registered by both {} and {}. Using {}.",
                            key, existing.extension_path, shortcut.extension_path, shortcut.extension_path
                        ),
                        shortcut.extension_path.clone(),
                    );
                }
                extension_shortcuts.insert(normalized_key, shortcut);
            }
        }

        *self
            .inner
            .shortcut_diagnostics
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = new_diags;
        extension_shortcuts
    }

    pub fn get_shortcut_diagnostics(&self) -> Vec<ResourceDiagnostic> {
        self.inner
            .shortcut_diagnostics
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn invalidate(&self, message: Option<&str>) {
        let msg = message.unwrap_or(DEFAULT_STALE_MESSAGE).to_string();
        let should_invalidate = {
            let mut guard = self
                .inner
                .ctx_state
                .stale_message
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if guard.is_none() {
                *guard = Some(msg.clone());
                true
            } else {
                false
            }
        };
        if should_invalidate {
            self.inner.ctx_state.runtime.invalidate(Some(&msg));
        }
    }

    fn assert_active(&self) -> pi_js::Result<()> {
        self.inner.ctx_state.assert_active()
    }

    pub fn on_error(&self, listener: EventBusHandler) -> Unsubscribe {
        let wrapped: ExtensionErrorListener = Arc::new(move |err| {
            let val = serde_json::to_value(&err).unwrap_or(Value::Null);
            listener(val);
        });
        self.on_error_typed(wrapped)
    }

    pub fn on_error_typed(&self, listener: Arc<dyn Fn(ExtensionError) + Send + Sync>) -> Unsubscribe {
        let id = self.inner.next_listener_id.fetch_add(1, Ordering::Relaxed);
        self.inner
            .error_listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, listener);
        let runner_inner = Arc::clone(&self.inner);
        Unsubscribe::new(move || {
            runner_inner
                .error_listeners
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .shift_remove(&id);
        })
    }

    pub fn emit_error(&self, error: ExtensionError) {
        let listeners: Vec<Arc<dyn Fn(ExtensionError) + Send + Sync>> = {
            let guard = self.inner.error_listeners.lock().unwrap_or_else(|e| e.into_inner());
            guard.values().cloned().collect()
        };
        for listener in listeners {
            listener(error.clone());
        }
    }

    /// Report registered MCP servers when no extension handles `mcp_servers_change`.
    pub fn report_unhandled_mcp_servers(&self) {
        if self.has_handlers("mcp_servers_change") {
            return;
        }
        let servers = self.inner.ctx_state.runtime.inner.mcp_servers.list();
        let mut to_report = Vec::new();
        {
            let mut reported = self
                .inner
                .reported_mcp_servers
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            for server in servers {
                if reported.insert(server.name.clone()) {
                    to_report.push(server);
                }
            }
        }
        for server in to_report {
            self.emit_error(ExtensionError {
                extension_path: server.extension_path,
                event: "register_mcp_server".to_string(),
                error: format!(
                    "MCP server \"{}\" is registered, but no loaded extension connects MCP servers; another extension may have replaced the built-in MCP support",
                    server.name
                ),
                stack: None,
            });
        }
    }

    pub fn has_handlers(&self, event_type: &str) -> bool {
        for ext in &self.inner.extensions {
            let guard = ext.handlers.lock().unwrap_or_else(|e| e.into_inner());
            if guard.get(event_type).is_some_and(|list| !list.is_empty()) {
                return true;
            }
        }
        false
    }

    pub fn get_message_renderer(&self, custom_type: &str) -> Option<MessageRenderer> {
        for ext in &self.inner.extensions {
            let guard = ext.message_renderers.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(r) = guard.get(custom_type) {
                return Some(Arc::clone(r));
            }
        }
        None
    }

    /// Markdown transformers in extension load order; each one leaves Markdown of unregistered surfaces unchanged.
    pub fn get_markdown_transformers(&self) -> Vec<MarkdownTransformer> {
        let mut out: Vec<MarkdownTransformer> = Vec::new();
        for ext in &self.inner.extensions {
            let guard = ext.markdown_transformer.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(reg) = guard.as_ref() {
                let transformer = Arc::clone(&reg.transformer);
                let message_types = reg.message_types.clone();
                out.push(Arc::new(
                    move |markdown: &str, context: crate::core::extensions::types::MarkdownTransformContext| {
                        if message_types.contains(&context.message_type) {
                            transformer(markdown, context)
                        } else {
                            markdown.to_string()
                        }
                    },
                ));
            }
        }
        out
    }

    /// Resolve renderers for calls to `tool_name`: extension resolvers in load order, then `base`.
    pub fn resolve_tool_renderers<F>(&self, tool_name: &str, base: F) -> Option<ToolRenderers>
    where
        F: FnOnce() -> Option<ToolRenderers> + Send + 'static,
    {
        let mut resolvers: Vec<ToolRendererResolver> = Vec::new();
        for ext in &self.inner.extensions {
            let guard = ext.tool_renderers.lock().unwrap_or_else(|e| e.into_inner());
            resolvers.extend(guard.iter().cloned());
        }
        fn resolve_at(
            tool_name: &str,
            resolvers: Vec<ToolRendererResolver>,
            index: usize,
            base: Box<dyn FnOnce() -> Option<ToolRenderers> + Send>,
        ) -> Option<ToolRenderers> {
            if index < resolvers.len() {
                let resolver = Arc::clone(&resolvers[index]);
                let name = tool_name.to_string();
                let next = Box::new(move || resolve_at(&name, resolvers, index + 1, base));
                resolver(tool_name, next)
            } else {
                base()
            }
        }
        resolve_at(tool_name, resolvers, 0, Box::new(base))
    }

    pub fn get_entry_renderer(&self, custom_type: &str) -> Option<EntryRenderer> {
        for ext in &self.inner.extensions {
            let guard = ext.entry_renderers.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(r) = guard.get(custom_type) {
                return Some(Arc::clone(r));
            }
        }
        None
    }

    fn resolve_registered_commands(&self) -> Vec<ResolvedCommand> {
        let mut commands: Vec<RegisteredCommand> = Vec::new();
        let mut counts: IndexMap<String, usize> = IndexMap::new();

        for ext in &self.inner.extensions {
            let ext_cmds = {
                let guard = ext.commands.lock().unwrap_or_else(|e| e.into_inner());
                guard.values().cloned().collect::<Vec<_>>()
            };
            for command in ext_cmds {
                *counts.entry(command.name.clone()).or_insert(0) += 1;
                commands.push(command);
            }
        }

        let mut seen: IndexMap<String, usize> = IndexMap::new();
        let mut taken_invocation_names: IndexSet<String> = IndexSet::new();

        commands
            .into_iter()
            .map(|command| {
                let occurrence = seen.get(&command.name).copied().unwrap_or(0) + 1;
                seen.insert(command.name.clone(), occurrence);

                let total = counts.get(&command.name).copied().unwrap_or(0);
                let mut invocation_name = if total > 1 {
                    format!("{}:{}", command.name, occurrence)
                } else {
                    command.name.clone()
                };

                if taken_invocation_names.contains(&invocation_name) {
                    let mut suffix = occurrence;
                    loop {
                        suffix += 1;
                        let candidate = format!("{}:{}", command.name, suffix);
                        if !taken_invocation_names.contains(&candidate) {
                            invocation_name = candidate;
                            break;
                        }
                    }
                }

                taken_invocation_names.insert(invocation_name.clone());
                ResolvedCommand {
                    command,
                    invocation_name,
                }
            })
            .collect()
    }

    pub fn get_registered_commands(&self) -> Vec<ResolvedCommand> {
        *self.inner.command_diagnostics.lock().unwrap_or_else(|e| e.into_inner()) = Vec::new();
        self.resolve_registered_commands()
    }

    pub fn get_command_diagnostics(&self) -> Vec<ResourceDiagnostic> {
        self.inner
            .command_diagnostics
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn get_command(&self, name: &str) -> Option<ResolvedCommand> {
        self.resolve_registered_commands()
            .into_iter()
            .find(|cmd| cmd.invocation_name == name)
    }

    pub fn get_model_registry(&self) -> &ModelRegistry {
        &self.inner.ctx_state.model_registry
    }

    pub fn shutdown(&self) {
        let f = self
            .inner
            .ctx_state
            .callbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .shutdown
            .clone();
        f();
    }

    pub fn get_active_tools(&self) -> pi_js::Result<Vec<String>> {
        self.assert_active()?;
        let f = self
            .inner
            .ctx_state
            .runtime
            .inner
            .actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_active_tools
            .clone();
        Ok(f())
    }

    pub fn create_context(&self) -> ExtensionContext {
        ExtensionContext::from_backend(Arc::new(RunnerContextBackend {
            state: Arc::clone(&self.inner.ctx_state),
        }))
    }

    pub fn create_tool_context(&self, tool_call_id: &str, signal: Option<AbortSignal>) -> ExtensionToolContext {
        ExtensionToolContext {
            base: self.create_context(),
            tool_call_id: tool_call_id.to_string(),
            default_signal: signal,
        }
    }

    pub fn create_command_context(&self) -> ExtensionCommandContext {
        ExtensionCommandContext {
            base: self.create_context(),
        }
    }

    pub async fn emit_boundary<F, Fut>(
        &self,
        base_event: BoundaryBaseEvent,
        mut build_context: F,
    ) -> BoundaryDispatchResult
    where
        F: FnMut(&[SessionBoundaryDraft]) -> Fut + Send,
        Fut: Future<Output = pi_js::Result<BoundaryContextPreview>> + Send,
    {
        let ctx = self.create_context();
        let event_type = base_event.event_type();
        let mut entries: Vec<SessionBoundaryDraft> = Vec::new();
        let mut should_continue = false;
        let mut context = match build_context(&entries).await {
            Ok(c) => c,
            Err(_) => BoundaryContextPreview::default(),
        };
        let mut valid = true;

        for (ext, handlers) in snapshot_event_handlers(&self.inner.extensions, event_type) {
            for handler in handlers {
                match handler {
                    RegisteredEventHandler::Boxed(f) => {
                        let ev_any = base_event.into_event(entries.clone(), should_continue, context.clone());
                        match f(ev_any, ctx.clone()).await {
                            Ok(Some(res_any)) => {
                                if let Ok(b) = res_any.downcast::<BoundaryResult>() {
                                    if let Some(new_entries) = b.entries {
                                        entries = new_entries;
                                    }
                                    if let Some(cont) = b.r#continue {
                                        should_continue = cont;
                                    }
                                }
                            }
                            Ok(None) => {}
                            Err(err) => {
                                self.emit_error(ExtensionError {
                                    extension_path: ext.path.clone(),
                                    event: event_type.to_string(),
                                    error: err.to_string(),
                                    stack: None,
                                });
                            }
                        }
                    }
                    RegisteredEventHandler::Raw(f) => {
                        let ev_json = base_event.to_json_value(entries.clone(), should_continue, context.clone());
                        match f(ev_json, ctx.clone()).await {
                            Ok(maybe_val) => {
                                let unpacked = unpack_raw_envelope(maybe_val);
                                // Keep mutated `entries` even if the handler threw (mirrors JS array mutation semantics).
                                if let Some(mutated) = unpacked.mutated_event_ref()
                                    && let Some(arr) = mutated.get("entries")
                                    && let Ok(parsed_entries) =
                                        serde_json::from_value::<Vec<SessionBoundaryDraft>>(arr.clone())
                                {
                                    entries = parsed_entries;
                                }
                                match unpacked.into_result() {
                                    Ok(outcome) => {
                                        if let Some(res_val) = outcome.result
                                            && let Ok(b) = serde_json::from_value::<BoundaryResult>(res_val)
                                        {
                                            if let Some(new_entries) = b.entries {
                                                entries = new_entries;
                                            }
                                            if let Some(cont) = b.r#continue {
                                                should_continue = cont;
                                            }
                                        }
                                    }
                                    Err(err) => {
                                        self.emit_error(ExtensionError {
                                            extension_path: ext.path.clone(),
                                            event: event_type.to_string(),
                                            error: err.message,
                                            stack: err.stack,
                                        });
                                    }
                                }
                            }
                            Err(err) => {
                                self.emit_error(ExtensionError {
                                    extension_path: ext.path.clone(),
                                    event: event_type.to_string(),
                                    error: err.to_string(),
                                    stack: None,
                                });
                            }
                        }
                    }
                }

                match build_context(&entries).await {
                    Ok(preview) => {
                        context = preview;
                        valid = true;
                    }
                    Err(err) => {
                        valid = false;
                        self.emit_error(ExtensionError {
                            extension_path: ext.path.clone(),
                            event: event_type.to_string(),
                            error: format!("Invalid boundary entries: {err}"),
                            stack: None,
                        });
                    }
                }
            }
        }

        if valid {
            BoundaryDispatchResult {
                entries,
                r#continue: should_continue,
                context,
                valid: true,
            }
        } else {
            BoundaryDispatchResult {
                entries: Vec::new(),
                r#continue: false,
                context,
                valid: false,
            }
        }
    }

    pub async fn emit(&self, event: impl Into<RunnerEmitEvent>) -> Option<SessionBeforeEventResult> {
        let event = event.into();
        let ctx = self.create_context();
        let event_type = event.event_type();
        let is_session_before = event.is_session_before();
        let ev_json = event.to_json_value();
        let ev_any = event.clone().into_any();
        let mut last_result: Option<SessionBeforeEventResult> = None;

        for (ext, handlers) in snapshot_event_handlers(&self.inner.extensions, event_type) {
            for handler in handlers {
                match handler {
                    RegisteredEventHandler::Boxed(f) => match f(Arc::clone(&ev_any), ctx.clone()).await {
                        Ok(Some(res_any)) if is_session_before => {
                            if let Some(parsed) = parse_boxed_session_before(&event, res_any) {
                                let cancel = parsed.cancel();
                                last_result = Some(parsed);
                                if cancel {
                                    return last_result;
                                }
                            }
                        }
                        Ok(_) => {}
                        Err(err) => {
                            self.emit_error(ExtensionError {
                                extension_path: ext.path.clone(),
                                event: event_type.to_string(),
                                error: err.to_string(),
                                stack: None,
                            });
                        }
                    },
                    RegisteredEventHandler::Raw(f) => match f(ev_json.clone(), ctx.clone()).await {
                        Ok(maybe_val) => match unpack_raw_envelope(maybe_val).into_result() {
                            Ok(outcome) => {
                                if is_session_before
                                    && let Some(res_val) = outcome.result
                                    && let Some(parsed) = parse_json_session_before(&event, res_val)
                                {
                                    let cancel = parsed.cancel();
                                    last_result = Some(parsed);
                                    if cancel {
                                        return last_result;
                                    }
                                }
                            }
                            Err(err) => {
                                self.emit_error(ExtensionError {
                                    extension_path: ext.path.clone(),
                                    event: event_type.to_string(),
                                    error: err.message,
                                    stack: err.stack,
                                });
                            }
                        },
                        Err(err) => {
                            self.emit_error(ExtensionError {
                                extension_path: ext.path.clone(),
                                event: event_type.to_string(),
                                error: err.to_string(),
                                stack: None,
                            });
                        }
                    },
                }
            }
        }

        last_result
    }

    pub async fn emit_session_before_switch(
        &self,
        event: SessionBeforeSwitchEvent,
    ) -> Option<SessionBeforeSwitchResult> {
        self.emit(event).await.and_then(SessionBeforeEventResult::into_switch)
    }

    pub async fn emit_session_before_fork(&self, event: SessionBeforeForkEvent) -> Option<SessionBeforeForkResult> {
        self.emit(event).await.and_then(SessionBeforeEventResult::into_fork)
    }

    pub async fn emit_session_before_compact(
        &self,
        event: SessionBeforeCompactEvent,
    ) -> Option<SessionBeforeCompactResult> {
        self.emit(event).await.and_then(SessionBeforeEventResult::into_compact)
    }

    pub async fn emit_session_before_tree(&self, event: SessionBeforeTreeEvent) -> Option<SessionBeforeTreeResult> {
        self.emit(event).await.and_then(SessionBeforeEventResult::into_tree)
    }

    /// Returns the event's own action unless a handler overrides it; the last override wins.
    pub async fn emit_cache_warming_decision(&self, event: CacheWarmingDecisionEvent) -> CacheWarmingAction {
        let ctx = self.create_context();
        let mut action = event.action;
        let ev_json = with_event_type(&event, "cache_warming_decision");
        let ev_any: Arc<dyn Any + Send + Sync> = Arc::new(event);

        for (ext, handlers) in snapshot_event_handlers(&self.inner.extensions, "cache_warming_decision") {
            for handler in handlers {
                match invoke_single_handler::<CacheWarmingDecisionEventResult>(
                    &handler,
                    Arc::clone(&ev_any),
                    ev_json.clone(),
                    ctx.clone(),
                )
                .await
                {
                    Ok(Some(res)) => {
                        if let Some(a) = res.action {
                            action = a;
                        }
                    }
                    Ok(None) => {}
                    Err(err) => {
                        self.emit_error(ExtensionError {
                            extension_path: ext.path.clone(),
                            event: "cache_warming_decision".to_string(),
                            error: err.message,
                            stack: err.stack,
                        });
                    }
                }
            }
        }
        action
    }

    pub async fn emit_message_end(&self, event: MessageEndEvent) -> Option<AgentMessage> {
        let ctx = self.create_context();
        let mut current_message = event.message.clone();
        let mut modified = false;

        for (ext, handlers) in snapshot_event_handlers(&self.inner.extensions, "message_end") {
            for handler in handlers {
                let current_event = MessageEndEvent {
                    message: current_message.clone(),
                };
                let ev_json = with_event_type(&current_event, "message_end");
                let ev_any: Arc<dyn Any + Send + Sync> = Arc::new(current_event);
                match invoke_single_handler::<MessageEndEventResult>(&handler, ev_any, ev_json, ctx.clone()).await {
                    Ok(Some(res)) => {
                        let Some(new_msg) = res.message else {
                            continue;
                        };
                        if new_msg.role() != current_message.role() {
                            self.emit_error(ExtensionError {
                                extension_path: ext.path.clone(),
                                event: "message_end".to_string(),
                                error: "message_end handlers must return a message with the same role".to_string(),
                                stack: None,
                            });
                            continue;
                        }
                        current_message = new_msg;
                        modified = true;
                    }
                    Ok(None) => {}
                    Err(err) => {
                        self.emit_error(ExtensionError {
                            extension_path: ext.path.clone(),
                            event: "message_end".to_string(),
                            error: err.message,
                            stack: err.stack,
                        });
                    }
                }
            }
        }

        if modified { Some(current_message) } else { None }
    }

    pub async fn emit_tool_result(&self, event: ToolResultEvent) -> Option<ToolResultEventResult> {
        let ctx = self.create_context();
        let mut current_event = event;
        let mut modified = false;

        for (ext, handlers) in snapshot_event_handlers(&self.inner.extensions, "tool_result") {
            for handler in handlers {
                let ev_json = tool_result_event_to_value(&current_event);
                let ev_any: Arc<dyn Any + Send + Sync> = Arc::new(current_event.clone());
                match invoke_single_handler::<ToolResultEventResult>(&handler, ev_any, ev_json, ctx.clone()).await {
                    Ok(Some(res)) => {
                        if let Some(new_content) = res.content {
                            set_tool_result_content(&mut current_event, new_content);
                            if res.structured_content.is_none() {
                                set_tool_result_structured_content(&mut current_event, None);
                            }
                            modified = true;
                        }
                        if let Some(new_details) = res.details {
                            set_tool_result_details(&mut current_event, Some(new_details));
                            modified = true;
                        }
                        if let Some(new_structured) = res.structured_content {
                            set_tool_result_structured_content(&mut current_event, Some(new_structured));
                            modified = true;
                        }
                        if let Some(is_error) = res.is_error {
                            set_tool_result_is_error(&mut current_event, is_error);
                            modified = true;
                        }
                        if let Some(usage) = res.usage {
                            set_tool_result_usage(&mut current_event, Some(usage));
                            modified = true;
                        }
                    }
                    Ok(None) => {}
                    Err(err) => {
                        self.emit_error(ExtensionError {
                            extension_path: ext.path.clone(),
                            event: "tool_result".to_string(),
                            error: err.message,
                            stack: err.stack,
                        });
                    }
                }
            }
        }

        if !modified {
            None
        } else {
            Some(extract_tool_result_patch(&current_event))
        }
    }

    pub async fn emit_tool_call(&self, event: &mut ToolCallEvent) -> pi_js::Result<Option<ToolCallEventResult>> {
        let ctx = self.create_context();
        let mut last_result: Option<ToolCallEventResult> = None;

        for (_ext, handlers) in snapshot_event_handlers(&self.inner.extensions, "tool_call") {
            for handler in handlers {
                match handler {
                    RegisteredEventHandler::Boxed(f) => {
                        let ev_any: Arc<dyn Any + Send + Sync> = Arc::new(event.clone());
                        if let Some(res_any) = f(ev_any, ctx.clone()).await?
                            && let Ok(r) = res_any.downcast::<ToolCallEventResult>()
                        {
                            let blocked = r.block.unwrap_or(false);
                            last_result = Some(*r);
                            if blocked {
                                return Ok(last_result);
                            }
                        }
                    }
                    RegisteredEventHandler::Raw(f) => {
                        let ev_json = tool_call_event_to_value(event);
                        let maybe_val = f(ev_json, ctx.clone()).await?;
                        let unpacked = unpack_raw_envelope(maybe_val);
                        if let Some(mutated) = unpacked.mutated_event_ref() {
                            apply_tool_call_mutated_input(event, mutated);
                        }
                        let outcome = unpacked.into_result().map_err(|e| pi_js::Error::msg(e.message))?;
                        if let Some(res_val) = outcome.result
                            && let Ok(parsed) = serde_json::from_value::<ToolCallEventResult>(res_val)
                        {
                            let blocked = parsed.block.unwrap_or(false);
                            last_result = Some(parsed);
                            if blocked {
                                return Ok(last_result);
                            }
                        }
                    }
                }
            }
        }

        Ok(last_result)
    }

    pub async fn emit_user_bash(&self, event: UserBashEvent) -> pi_js::Result<Option<UserBashEventResult>> {
        let ctx = self.create_context();
        let ev_json = with_event_type(&event, "user_bash");
        let ev_any: Arc<dyn Any + Send + Sync> = Arc::new(event);

        for (ext, handlers) in snapshot_event_handlers(&self.inner.extensions, "user_bash") {
            for handler in handlers {
                let res = match handler {
                    RegisteredEventHandler::Boxed(f) => f(Arc::clone(&ev_any), ctx.clone())
                        .await
                        .map(|opt| opt.and_then(|any| any.downcast::<UserBashEventResult>().ok().map(|b| *b))),
                    RegisteredEventHandler::Raw(f) => match f(ev_json.clone(), ctx.clone()).await {
                        Ok(maybe_val) => match unpack_raw_envelope(maybe_val).into_result() {
                            Ok(outcome) => match outcome.result {
                                None => Ok(None),
                                Some(res_val) => {
                                    // Note: QuickJS extension VM returns `{ __piUserBashExecId }` marker for valid `{ operations: { exec } }`.
                                    validate_user_bash_json_result(&res_val)
                                        .map(Some)
                                        .map_err(pi_js::Error::msg)
                                }
                            },
                            Err(err) => Err(pi_js::Error::msg(err.message)),
                        },
                        Err(err) => Err(err),
                    },
                };

                match res {
                    Ok(None) => continue,
                    Ok(Some(valid)) => return Ok(Some(valid)),
                    Err(err) => {
                        self.emit_error(ExtensionError {
                            extension_path: ext.path.clone(),
                            event: "user_bash".to_string(),
                            error: err.to_string(),
                            stack: None,
                        });
                        return Err(err);
                    }
                }
            }
        }

        Ok(None)
    }

    pub async fn emit_context(&self, messages: Vec<AgentMessage>) -> Vec<AgentMessage> {
        let ctx = self.create_context();
        let mut current_messages = messages;

        for (ext, handlers) in snapshot_event_handlers(&self.inner.extensions, "context") {
            for handler in handlers {
                let visible_snapshot: Vec<AgentMessage> = current_messages
                    .iter()
                    .filter(|m| m.role() != Some("system"))
                    .cloned()
                    .collect();
                let visible_json = visible_snapshot.clone();
                let ev = ContextEvent {
                    messages: visible_snapshot.clone(),
                };
                match handler {
                    RegisteredEventHandler::Boxed(f) => {
                        let ev_any: Arc<dyn Any + Send + Sync> = Arc::new(ev);
                        match f(ev_any, ctx.clone()).await {
                            Ok(Some(res_any)) => {
                                if let Ok(r) = res_any.downcast::<ContextEventResult>()
                                    && let Some(returned) = r.messages
                                {
                                    current_messages =
                                        restore_system_messages(current_messages, &visible_snapshot, returned);
                                }
                            }
                            Ok(None) => {}
                            Err(err) => {
                                self.emit_error(ExtensionError {
                                    extension_path: ext.path.clone(),
                                    event: "context".to_string(),
                                    error: err.to_string(),
                                    stack: None,
                                });
                            }
                        }
                    }
                    RegisteredEventHandler::Raw(f) => {
                        let ev_json = with_event_type(&ev, "context");
                        match f(ev_json, ctx.clone()).await {
                            Ok(maybe_val) => match unpack_raw_envelope(maybe_val).into_result() {
                                Ok(outcome) => {
                                    let from_result = outcome
                                        .result
                                        .and_then(|v| serde_json::from_value::<ContextEventResult>(v).ok())
                                        .and_then(|r| r.messages);
                                    let returned = if let Some(m) = from_result {
                                        Some(m)
                                    } else if let Some(mutated) = outcome
                                        .mutated_event
                                        .and_then(|m| m.get("messages").cloned())
                                        .and_then(|v| serde_json::from_value::<Vec<AgentMessage>>(v).ok())
                                    {
                                        if mutated == visible_json { None } else { Some(mutated) }
                                    } else {
                                        None
                                    };
                                    if let Some(msgs) = returned {
                                        current_messages =
                                            restore_system_messages(current_messages, &visible_snapshot, msgs);
                                    }
                                }
                                Err(err) => {
                                    self.emit_error(ExtensionError {
                                        extension_path: ext.path.clone(),
                                        event: "context".to_string(),
                                        error: err.message,
                                        stack: err.stack,
                                    });
                                }
                            },
                            Err(err) => {
                                self.emit_error(ExtensionError {
                                    extension_path: ext.path.clone(),
                                    event: "context".to_string(),
                                    error: err.to_string(),
                                    stack: None,
                                });
                            }
                        }
                    }
                }
            }
        }

        for (ext, handlers) in snapshot_event_handlers(&self.inner.extensions, "context_with_system") {
            for handler in handlers {
                let had_leading_system = current_messages.first().and_then(AgentMessage::role) == Some("system");
                let ev = ContextWithSystemEvent {
                    messages: current_messages.clone(),
                };
                let res = match handler {
                    RegisteredEventHandler::Boxed(f) => {
                        let ev_any: Arc<dyn Any + Send + Sync> = Arc::new(ev);
                        match f(ev_any, ctx.clone()).await {
                            Ok(Some(res_any)) => {
                                Ok(res_any.downcast::<ContextEventResult>().ok().and_then(|r| r.messages))
                            }
                            Ok(None) => Ok(None),
                            Err(err) => Err(RawInvokeError {
                                message: err.to_string(),
                                stack: None,
                            }),
                        }
                    }
                    RegisteredEventHandler::Raw(f) => {
                        let ev_json = with_event_type(&ev, "context_with_system");
                        match f(ev_json, ctx.clone()).await {
                            Ok(maybe_val) => match unpack_raw_envelope(maybe_val).into_result() {
                                Ok(outcome) => Ok(outcome
                                    .result
                                    .and_then(|v| serde_json::from_value::<ContextEventResult>(v).ok())
                                    .and_then(|r| r.messages)),
                                Err(err) => Err(err),
                            },
                            Err(err) => Err(RawInvokeError {
                                message: err.to_string(),
                                stack: None,
                            }),
                        }
                    }
                };
                match res {
                    Ok(maybe_msgs) => {
                        if let Some(new_msgs) = maybe_msgs {
                            current_messages = new_msgs;
                        }
                        if had_leading_system && current_messages.first().and_then(AgentMessage::role) != Some("system")
                        {
                            self.emit_error(ExtensionError {
                                extension_path: ext.path.clone(),
                                event: "context_with_system".to_string(),
                                error: "Handler removed the leading system message; the request has no prompt or initial tool declarations. Keep it at index 0 or replace a dropped prefix with getCurrentSystemMessage().".to_string(),
                                stack: None,
                            });
                        }
                    }
                    Err(err) => {
                        self.emit_error(ExtensionError {
                            extension_path: ext.path.clone(),
                            event: "context_with_system".to_string(),
                            error: err.message,
                            stack: err.stack,
                        });
                    }
                }
            }
        }

        current_messages
    }

    pub async fn emit_before_provider_request(&self, payload: Value) -> Value {
        let ctx = self.create_context();
        let mut current_payload = payload;

        for (ext, handlers) in snapshot_event_handlers(&self.inner.extensions, "before_provider_request") {
            for handler in handlers {
                let ev = BeforeProviderRequestEvent {
                    payload: current_payload.clone(),
                };
                let ev_json = with_event_type(&ev, "before_provider_request");
                let ev_any: Arc<dyn Any + Send + Sync> = Arc::new(ev);
                match invoke_single_handler::<Value>(&handler, ev_any, ev_json, ctx.clone()).await {
                    Ok(Some(new_payload)) => {
                        current_payload = new_payload;
                    }
                    Ok(None) => {}
                    Err(err) => {
                        self.emit_error(ExtensionError {
                            extension_path: ext.path.clone(),
                            event: "before_provider_request".to_string(),
                            error: err.message,
                            stack: err.stack,
                        });
                    }
                }
            }
        }

        current_payload
    }

    pub async fn emit_before_provider_headers(&self, mut headers: ProviderHeaders) -> ProviderHeaders {
        let ctx = self.create_context();

        for (ext, handlers) in snapshot_event_handlers(&self.inner.extensions, "before_provider_headers") {
            for handler in handlers {
                match handler {
                    RegisteredEventHandler::Boxed(f) => {
                        let shared = Arc::new(Mutex::new(headers.clone()));
                        let ev_any: Arc<dyn Any + Send + Sync> = Arc::new(BeforeProviderHeadersEvent {
                            headers: Arc::clone(&shared),
                        });
                        match f(ev_any, ctx.clone()).await {
                            Ok(_) => {
                                headers = shared.lock().unwrap_or_else(|e| e.into_inner()).clone();
                            }
                            Err(err) => {
                                self.emit_error(ExtensionError {
                                    extension_path: ext.path.clone(),
                                    event: "before_provider_headers".to_string(),
                                    error: err.to_string(),
                                    stack: None,
                                });
                            }
                        }
                    }
                    RegisteredEventHandler::Raw(f) => {
                        let ev_json = serde_json::json!({
                            "type": "before_provider_headers",
                            "headers": headers,
                        });
                        match f(ev_json, ctx.clone()).await {
                            Ok(maybe_val) => {
                                let unpacked = unpack_raw_envelope(maybe_val);
                                match unpacked.into_result() {
                                    Ok(outcome) => {
                                        if let Some(mutated) = outcome.mutated_event
                                            && let Some(h_val) = mutated.get("headers")
                                            && let Ok(parsed) = serde_json::from_value::<ProviderHeaders>(h_val.clone())
                                        {
                                            headers = parsed;
                                        }
                                    }
                                    Err(err) => {
                                        self.emit_error(ExtensionError {
                                            extension_path: ext.path.clone(),
                                            event: "before_provider_headers".to_string(),
                                            error: err.message,
                                            stack: err.stack,
                                        });
                                    }
                                }
                            }
                            Err(err) => {
                                self.emit_error(ExtensionError {
                                    extension_path: ext.path.clone(),
                                    event: "before_provider_headers".to_string(),
                                    error: err.to_string(),
                                    stack: None,
                                });
                            }
                        }
                    }
                }
            }
        }

        headers
    }

    pub async fn emit_before_agent_start(
        &self,
        prompt: &str,
        images: Option<Vec<ImageContent>>,
        system_prompt_options: BuildSystemPromptOptions,
    ) -> BeforeAgentStartCombinedResult {
        self.emit_before_agent_start_trigger(
            BeforeAgentStartTrigger::User {
                prompt: prompt.to_string(),
                images,
            },
            system_prompt_options,
        )
        .await
    }

    pub async fn emit_before_agent_start_trigger(
        &self,
        trigger: BeforeAgentStartTrigger,
        system_prompt_options: BuildSystemPromptOptions,
    ) -> BeforeAgentStartCombinedResult {
        let shared_options = Arc::new(Mutex::new(normalize_build_system_prompt_options(
            &system_prompt_options,
        )));
        let opts_for_override = Arc::clone(&shared_options);
        let ctx = self.create_context().with_system_prompt_override(Arc::new(move || {
            let guard = opts_for_override.lock().unwrap_or_else(|e| e.into_inner());
            build_system_prompt_from_normalized(&guard).unwrap_or_default()
        }));
        let mut messages = Vec::new();

        for (ext, handlers) in snapshot_event_handlers(&self.inner.extensions, "before_agent_start") {
            for handler in handlers {
                let current_prompt_str = {
                    let guard = shared_options.lock().unwrap_or_else(|e| e.into_inner());
                    build_system_prompt_from_normalized(&guard).unwrap_or_default()
                };
                let (message_type, prompt, images, message) = match &trigger {
                    BeforeAgentStartTrigger::User { prompt, images } => {
                        (BeforeAgentStartMessageType::User, prompt.clone(), images.clone(), None)
                    }
                    BeforeAgentStartTrigger::Custom { message } => (
                        BeforeAgentStartMessageType::Custom,
                        String::new(),
                        None,
                        Some(message.clone()),
                    ),
                };
                match handler {
                    RegisteredEventHandler::Boxed(f) => {
                        let ev = BeforeAgentStartEvent {
                            message_type,
                            prompt,
                            images,
                            message,
                            system_prompt: current_prompt_str,
                            system_prompt_options: Arc::clone(&shared_options),
                        };
                        let ev_any: Arc<dyn Any + Send + Sync> = Arc::new(ev);
                        match f(ev_any, ctx.clone()).await {
                            Ok(Some(res_any)) => {
                                if let Ok(r) = res_any.downcast::<BeforeAgentStartEventResult>() {
                                    if let Some(m) = r.message {
                                        messages.push(m);
                                    }
                                    if let Some(sp) = r.system_prompt {
                                        shared_options
                                            .lock()
                                            .unwrap_or_else(|e| e.into_inner())
                                            .force_system_prompt = Some(sp);
                                    }
                                }
                            }
                            Ok(None) => {}
                            Err(err) => {
                                self.emit_error(ExtensionError {
                                    extension_path: ext.path.clone(),
                                    event: "before_agent_start".to_string(),
                                    error: err.to_string(),
                                    stack: None,
                                });
                            }
                        }
                    }
                    RegisteredEventHandler::Raw(f) => {
                        let ev = BeforeAgentStartEvent {
                            message_type,
                            prompt,
                            images,
                            message,
                            system_prompt: current_prompt_str,
                            system_prompt_options: Arc::clone(&shared_options),
                        };
                        let ev_json = with_event_type(&ev, "before_agent_start");
                        match f(ev_json, ctx.clone()).await {
                            Ok(maybe_val) => match unpack_raw_envelope(maybe_val).into_result() {
                                Ok(outcome) => {
                                    if let Some(mutated) = outcome.mutated_event
                                        && let Some(opts_val) = mutated.get("systemPromptOptions")
                                        && let Ok(parsed_opts) =
                                            serde_json::from_value::<NormalizedBuildSystemPromptOptions>(
                                                opts_val.clone(),
                                            )
                                    {
                                        *shared_options.lock().unwrap_or_else(|e| e.into_inner()) = parsed_opts;
                                    }
                                    if let Some(res_val) = outcome.result
                                        && let Ok(r) = serde_json::from_value::<BeforeAgentStartEventResult>(res_val)
                                    {
                                        if let Some(m) = r.message {
                                            messages.push(m);
                                        }
                                        if let Some(sp) = r.system_prompt {
                                            shared_options
                                                .lock()
                                                .unwrap_or_else(|e| e.into_inner())
                                                .force_system_prompt = Some(sp);
                                        }
                                    }
                                }
                                Err(err) => {
                                    self.emit_error(ExtensionError {
                                        extension_path: ext.path.clone(),
                                        event: "before_agent_start".to_string(),
                                        error: err.message,
                                        stack: err.stack,
                                    });
                                }
                            },
                            Err(err) => {
                                self.emit_error(ExtensionError {
                                    extension_path: ext.path.clone(),
                                    event: "before_agent_start".to_string(),
                                    error: err.to_string(),
                                    stack: None,
                                });
                            }
                        }
                    }
                }
            }
        }

        let final_opts = shared_options.lock().unwrap_or_else(|e| e.into_inner()).clone();
        BeforeAgentStartCombinedResult {
            messages,
            system_prompt_options: final_opts,
        }
    }

    pub async fn emit_resources_discover(
        &self,
        cwd: &str,
        reason: ResourcesDiscoverReason,
    ) -> DiscoveredResourcesResult {
        let ctx = self.create_context();
        let mut out = DiscoveredResourcesResult::default();

        for (ext, handlers) in snapshot_event_handlers(&self.inner.extensions, "resources_discover") {
            for handler in handlers {
                let ev = ResourcesDiscoverEvent {
                    cwd: cwd.to_string(),
                    reason,
                };
                let ev_json = with_event_type(&ev, "resources_discover");
                let ev_any: Arc<dyn Any + Send + Sync> = Arc::new(ev);
                match invoke_single_handler::<ResourcesDiscoverResult>(&handler, ev_any, ev_json, ctx.clone()).await {
                    Ok(Some(res)) => {
                        if let Some(skills) = res.skill_paths {
                            for path in skills {
                                out.skill_paths.push(DiscoveredResourcePathEntry {
                                    path,
                                    extension_path: ext.path.clone(),
                                });
                            }
                        }
                        if let Some(prompts) = res.prompt_paths {
                            for path in prompts {
                                out.prompt_paths.push(DiscoveredResourcePathEntry {
                                    path,
                                    extension_path: ext.path.clone(),
                                });
                            }
                        }
                        if let Some(themes) = res.theme_paths {
                            for path in themes {
                                out.theme_paths.push(DiscoveredResourcePathEntry {
                                    path,
                                    extension_path: ext.path.clone(),
                                });
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(err) => {
                        self.emit_error(ExtensionError {
                            extension_path: ext.path.clone(),
                            event: "resources_discover".to_string(),
                            error: err.message,
                            stack: err.stack,
                        });
                    }
                }
            }
        }

        out
    }

    /// Emit `input` event. Transforms chain, `"handled"` short-circuits.
    pub async fn emit_input(
        &self,
        text: &str,
        images: Option<Vec<ImageContent>>,
        source: InputSource,
        streaming_behavior: Option<InputDeliveryBehavior>,
    ) -> InputEventResult {
        let ctx = self.create_context();
        let mut current_text = text.to_string();
        let mut current_images = images.clone();

        for (ext, handlers) in snapshot_event_handlers(&self.inner.extensions, "input") {
            for handler in handlers {
                let ev = InputEvent {
                    text: current_text.clone(),
                    images: current_images.clone(),
                    source,
                    streaming_behavior,
                };
                let ev_json = with_event_type(&ev, "input");
                let ev_any: Arc<dyn Any + Send + Sync> = Arc::new(ev);
                match invoke_single_handler::<InputEventResult>(&handler, ev_any, ev_json, ctx.clone()).await {
                    Ok(Some(res)) => match res {
                        InputEventResult::Handled => return InputEventResult::Handled,
                        InputEventResult::Transform { text: t, images: imgs } => {
                            current_text = t;
                            if imgs.is_some() {
                                current_images = imgs;
                            }
                        }
                        InputEventResult::Continue => {}
                    },
                    Ok(None) => {}
                    Err(err) => {
                        self.emit_error(ExtensionError {
                            extension_path: ext.path.clone(),
                            event: "input".to_string(),
                            error: err.message,
                            stack: err.stack,
                        });
                    }
                }
            }
        }

        if current_text != text || current_images != images {
            InputEventResult::Transform {
                text: current_text,
                images: current_images,
            }
        } else {
            InputEventResult::Continue
        }
    }
}

struct UiPromptFinishGuard {
    runner: Weak<ExtensionRunnerInner>,
    kind: UIPromptKind,
    title: Option<String>,
}

impl Drop for UiPromptFinishGuard {
    fn drop(&mut self) {
        if let Some(inner) = self.runner.upgrade() {
            ExtensionRunner { inner }.finish_ui_prompt(self.kind, self.title.clone());
        }
    }
}

struct UiPromptWrappedContext {
    runner: Weak<ExtensionRunnerInner>,
    inner: Arc<dyn ExtensionUIContext>,
}

#[async_trait]
impl ExtensionUIContext for UiPromptWrappedContext {
    async fn select(
        &self,
        title: &str,
        options: Vec<String>,
        opts: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        if let Some(inner) = self.runner.upgrade() {
            ExtensionRunner { inner }
                .with_ui_prompt(
                    UIPromptKind::Select,
                    Some(title.to_string()),
                    self.inner.select(title, options, opts),
                )
                .await
        } else {
            self.inner.select(title, options, opts).await
        }
    }

    async fn confirm(&self, title: &str, message: &str, opts: Option<ExtensionUIDialogOptions>) -> pi_js::Result<bool> {
        if let Some(inner) = self.runner.upgrade() {
            ExtensionRunner { inner }
                .with_ui_prompt(
                    UIPromptKind::Confirm,
                    Some(title.to_string()),
                    self.inner.confirm(title, message, opts),
                )
                .await
        } else {
            self.inner.confirm(title, message, opts).await
        }
    }

    async fn input(
        &self,
        title: &str,
        placeholder: Option<&str>,
        opts: Option<ExtensionUIDialogOptions>,
    ) -> pi_js::Result<Option<String>> {
        if let Some(inner) = self.runner.upgrade() {
            ExtensionRunner { inner }
                .with_ui_prompt(
                    UIPromptKind::Input,
                    Some(title.to_string()),
                    self.inner.input(title, placeholder, opts),
                )
                .await
        } else {
            self.inner.input(title, placeholder, opts).await
        }
    }

    fn notify(&self, message: &str, notify_type: Option<NotifyType>) {
        self.inner.notify(message, notify_type);
    }

    fn on_terminal_input(&self, handler: TerminalInputHandler) -> Unsubscribe {
        self.inner.on_terminal_input(handler)
    }

    fn set_status(&self, key: &str, text: Option<&str>) {
        self.inner.set_status(key, text);
    }

    fn set_working_message(&self, message: Option<&str>) {
        self.inner.set_working_message(message);
    }

    fn set_working_visible(&self, visible: bool) {
        self.inner.set_working_visible(visible);
    }

    fn set_working_indicator(&self, options: Option<WorkingIndicatorOptions>) {
        self.inner.set_working_indicator(options);
    }

    fn set_hidden_thinking_label(&self, label: Option<&str>) {
        self.inner.set_hidden_thinking_label(label);
    }

    fn set_widget_lines(&self, key: &str, content: Option<Vec<String>>, options: Option<ExtensionWidgetOptions>) {
        self.inner.set_widget_lines(key, content, options);
    }

    fn set_widget_factory(&self, key: &str, factory: Option<WidgetFactory>, options: Option<ExtensionWidgetOptions>) {
        self.inner.set_widget_factory(key, factory, options);
    }

    fn set_footer(&self, factory: Option<FooterFactory>) {
        self.inner.set_footer(factory);
    }

    fn set_header(&self, factory: Option<HeaderFactory>) {
        self.inner.set_header(factory);
    }

    fn set_title(&self, title: &str) {
        self.inner.set_title(title);
    }

    async fn custom(&self, factory: CustomUiFactory, options: Option<ExtensionCustomOptions>) -> pi_js::Result<Value> {
        if let Some(inner) = self.runner.upgrade() {
            ExtensionRunner { inner }
                .with_ui_prompt(UIPromptKind::Custom, None, self.inner.custom(factory, options))
                .await
        } else {
            self.inner.custom(factory, options).await
        }
    }

    fn paste_to_editor(&self, text: &str) {
        self.inner.paste_to_editor(text);
    }

    fn set_editor_text(&self, text: &str) {
        self.inner.set_editor_text(text);
    }

    fn get_editor_text(&self) -> String {
        self.inner.get_editor_text()
    }

    async fn editor(&self, title: &str, prefill: Option<&str>) -> pi_js::Result<Option<String>> {
        if let Some(inner) = self.runner.upgrade() {
            ExtensionRunner { inner }
                .with_ui_prompt(
                    UIPromptKind::Editor,
                    Some(title.to_string()),
                    self.inner.editor(title, prefill),
                )
                .await
        } else {
            self.inner.editor(title, prefill).await
        }
    }

    fn add_autocomplete_provider(&self, factory: AutocompleteProviderFactory) {
        self.inner.add_autocomplete_provider(factory);
    }

    fn set_editor_component(&self, factory: Option<EditorFactory>) {
        self.inner.set_editor_component(factory);
    }

    fn get_editor_component(&self) -> Option<EditorFactory> {
        self.inner.get_editor_component()
    }

    fn theme(&self) -> Theme {
        self.inner.theme()
    }

    fn get_all_themes(&self) -> Vec<ThemeInfo> {
        self.inner.get_all_themes()
    }

    fn get_theme(&self, name: &str) -> Option<Theme> {
        self.inner.get_theme(name)
    }

    fn set_theme_by_name(&self, name: &str) -> SetThemeResult {
        self.inner.set_theme_by_name(name)
    }

    fn set_theme(&self, theme: Theme) -> SetThemeResult {
        self.inner.set_theme(theme)
    }

    fn get_tools_expanded(&self) -> bool {
        self.inner.get_tools_expanded()
    }

    fn set_tools_expanded(&self, expanded: bool) {
        self.inner.set_tools_expanded(expanded);
    }
}

struct RunnerContextBackend {
    state: Arc<RunnerContextState>,
}

impl ExtensionContextBackend for RunnerContextBackend {
    fn assert_active(&self) -> pi_js::Result<()> {
        self.state.assert_active()
    }

    fn ui(&self) -> Arc<dyn ExtensionUIContext> {
        self.state.get_ui_context()
    }

    fn mode(&self) -> ExtensionMode {
        *self.state.mode.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn has_ui(&self) -> bool {
        self.state.has_ui()
    }

    fn cwd(&self) -> String {
        self.state.cwd.clone()
    }

    fn session_manager(&self) -> SessionManager {
        self.state.session_manager.clone()
    }

    fn model_registry(&self) -> ModelRegistry {
        self.state.model_registry.clone()
    }

    fn model(&self) -> Option<Model> {
        let f = self
            .state
            .callbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_model
            .clone();
        f()
    }

    fn scoped_models(&self) -> Vec<ScopedModel> {
        let f = self
            .state
            .callbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_scoped_models
            .clone();
        f()
    }

    fn thinking_level(&self) -> Option<ThinkingLevel> {
        let f = self
            .state
            .runtime
            .inner
            .actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_thinking_level
            .clone();
        Some(f())
    }

    fn is_idle(&self) -> bool {
        let f = self
            .state
            .callbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_idle
            .clone();
        f()
    }

    fn is_project_trusted(&self) -> bool {
        let f = self
            .state
            .callbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_project_trusted
            .clone();
        f()
    }

    fn signal(&self) -> Option<AbortSignal> {
        let f = self
            .state
            .callbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_signal
            .clone();
        f()
    }

    fn abort(&self) {
        let f = self
            .state
            .callbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .abort
            .clone();
        f();
    }

    fn has_pending_messages(&self) -> bool {
        let f = self
            .state
            .callbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .has_pending_messages
            .clone();
        f()
    }

    fn shutdown(&self) {
        let f = self
            .state
            .callbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .shutdown
            .clone();
        f();
    }

    fn get_context_usage(&self) -> Option<ContextUsage> {
        let f = self
            .state
            .callbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_context_usage
            .clone();
        f()
    }

    fn compact(&self, options: Option<CompactOptions>) {
        let f = self
            .state
            .callbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .compact
            .clone();
        f(options);
    }

    fn get_system_prompt(&self) -> String {
        let f = self
            .state
            .callbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_system_prompt
            .clone();
        f()
    }

    fn get_system_prompt_options(&self) -> BuildSystemPromptOptions {
        let f = self
            .state
            .callbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_system_prompt_options
            .clone();
        f()
    }

    fn wait_for_idle(&self) -> BoxFuture<pi_js::Result<()>> {
        let f = self
            .state
            .callbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .wait_for_idle
            .clone();
        f()
    }

    fn new_session(&self, options: Option<NewSessionOptions>) -> BoxFuture<pi_js::Result<CancelledResult>> {
        let f = self
            .state
            .callbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .new_session
            .clone();
        f(options)
    }

    fn fork(&self, entry_id: &str, options: Option<ForkOptions>) -> BoxFuture<pi_js::Result<CancelledResult>> {
        let f = self
            .state
            .callbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .fork
            .clone();
        f(entry_id, options)
    }

    fn navigate_tree(
        &self,
        target_id: &str,
        options: Option<NavigateTreeOptions>,
    ) -> BoxFuture<pi_js::Result<CancelledResult>> {
        let f = self
            .state
            .callbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .navigate_tree
            .clone();
        f(target_id, options)
    }

    fn switch_session(
        &self,
        session_path: &str,
        options: Option<SwitchSessionOptions>,
    ) -> BoxFuture<pi_js::Result<CancelledResult>> {
        let f = self
            .state
            .callbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .switch_session
            .clone();
        f(session_path, options)
    }

    fn reload(&self) -> BoxFuture<pi_js::Result<()>> {
        let f = self
            .state
            .callbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reload
            .clone();
        f()
    }

    fn callable_tools(&self) -> Vec<AgentTool> {
        let f = self
            .state
            .callbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_callable_tools
            .clone();
        f()
    }

    fn execute_tool(
        &self,
        caller_id: &str,
        name: &str,
        args: Value,
        options: ExecuteToolOptions,
    ) -> BoxFuture<AgentToolCallOutcome> {
        let maybe_fn = self
            .state
            .callbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .execute_tool
            .clone();
        if let Some(f) = maybe_fn {
            f(caller_id, name, args, options)
        } else {
            let id = format!("{caller_id}/0");
            let name = name.to_string();
            Box::pin(async move {
                AgentToolCallOutcome {
                    tool_call: AgentToolCall {
                        id,
                        name,
                        arguments: JsonObject::new(),
                    },
                    result: AgentToolResult::text("Nested tool calls are not available in this context"),
                    is_error: true,
                    duration_ms: None,
                }
            })
        }
    }

    fn send_message(&self, message: CustomMessageDraft, options: Option<SendMessageOptions>) {
        let f = self
            .state
            .runtime
            .inner
            .actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send_message
            .clone();
        f(message, options);
    }

    fn send_user_message(&self, content: CustomMessageContent, options: Option<SendUserMessageOptions>) {
        let f = self
            .state
            .runtime
            .inner
            .actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send_user_message
            .clone();
        f(content, options);
    }
}

// ---------------------------------------------------------------------------
// Envelope / event serialization helpers
// ---------------------------------------------------------------------------

struct RawInvokeError {
    message: String,
    stack: Option<String>,
}

struct RawInvokeOutcome {
    result: Option<Value>,
    mutated_event: Option<Value>,
}

enum UnpackedEnvelope {
    Ok(RawInvokeOutcome),
    Err {
        error: RawInvokeError,
        mutated_event: Option<Value>,
    },
}

impl UnpackedEnvelope {
    fn mutated_event_ref(&self) -> Option<&Value> {
        match self {
            Self::Ok(o) => o.mutated_event.as_ref(),
            Self::Err { mutated_event, .. } => mutated_event.as_ref(),
        }
    }

    fn into_result(self) -> std::result::Result<RawInvokeOutcome, RawInvokeError> {
        match self {
            Self::Ok(o) => Ok(o),
            Self::Err { error, .. } => Err(error),
        }
    }
}

fn unpack_raw_envelope(maybe_val: Option<Value>) -> UnpackedEnvelope {
    let Some(val) = maybe_val else {
        return UnpackedEnvelope::Ok(RawInvokeOutcome {
            result: None,
            mutated_event: None,
        });
    };
    if val.get("__piEnvelope").and_then(Value::as_bool) == Some(true) {
        let mutated_event = val.get("mutatedEvent").cloned();
        if let Some(threw) = val.get("threw").and_then(Value::as_str) {
            return UnpackedEnvelope::Err {
                error: RawInvokeError {
                    message: threw.to_string(),
                    stack: val.get("stack").and_then(Value::as_str).map(str::to_string),
                },
                mutated_event,
            };
        }
        let has_result = val.get("hasResult").and_then(Value::as_bool).unwrap_or(false);
        let result = if has_result { val.get("result").cloned() } else { None };
        UnpackedEnvelope::Ok(RawInvokeOutcome { result, mutated_event })
    } else {
        UnpackedEnvelope::Ok(RawInvokeOutcome {
            result: Some(val),
            mutated_event: None,
        })
    }
}

async fn invoke_single_handler<R: serde::de::DeserializeOwned + 'static>(
    handler: &RegisteredEventHandler,
    ev_any: Arc<dyn Any + Send + Sync>,
    ev_json: Value,
    ctx: ExtensionContext,
) -> std::result::Result<Option<R>, RawInvokeError> {
    match handler {
        RegisteredEventHandler::Boxed(f) => match f(ev_any, ctx).await {
            Ok(Some(any_res)) => Ok(any_res.downcast::<R>().ok().map(|b| *b)),
            Ok(None) => Ok(None),
            Err(err) => Err(RawInvokeError {
                message: err.to_string(),
                stack: None,
            }),
        },
        RegisteredEventHandler::Raw(f) => match f(ev_json, ctx).await {
            Ok(maybe_val) => {
                let outcome = unpack_raw_envelope(maybe_val).into_result()?;
                Ok(outcome.result.and_then(|v| serde_json::from_value::<R>(v).ok()))
            }
            Err(err) => Err(RawInvokeError {
                message: err.to_string(),
                stack: None,
            }),
        },
    }
}

fn with_event_type<T: Serialize>(ev: &T, event_type: &str) -> Value {
    let mut val = serde_json::to_value(ev).unwrap_or(Value::Null);
    if let Value::Object(ref mut map) = val {
        map.insert("type".to_string(), Value::String(event_type.to_string()));
    }
    val
}

fn parse_boxed_session_before(
    event: &RunnerEmitEvent,
    res_any: Box<dyn Any + Send>,
) -> Option<SessionBeforeEventResult> {
    match event {
        RunnerEmitEvent::SessionBeforeSwitch(_) => res_any
            .downcast::<SessionBeforeSwitchResult>()
            .ok()
            .map(|r| SessionBeforeEventResult::Switch(*r)),
        RunnerEmitEvent::SessionBeforeFork(_) => res_any
            .downcast::<SessionBeforeForkResult>()
            .ok()
            .map(|r| SessionBeforeEventResult::Fork(*r)),
        RunnerEmitEvent::SessionBeforeCompact(_) => res_any
            .downcast::<SessionBeforeCompactResult>()
            .ok()
            .map(|r| SessionBeforeEventResult::Compact(*r)),
        RunnerEmitEvent::SessionBeforeTree(_) => res_any
            .downcast::<SessionBeforeTreeResult>()
            .ok()
            .map(|r| SessionBeforeEventResult::Tree(*r)),
        _ => None,
    }
}

fn parse_json_session_before(event: &RunnerEmitEvent, res_val: Value) -> Option<SessionBeforeEventResult> {
    match event {
        RunnerEmitEvent::SessionBeforeSwitch(_) => serde_json::from_value::<SessionBeforeSwitchResult>(res_val)
            .ok()
            .map(SessionBeforeEventResult::Switch),
        RunnerEmitEvent::SessionBeforeFork(_) => serde_json::from_value::<SessionBeforeForkResult>(res_val)
            .ok()
            .map(SessionBeforeEventResult::Fork),
        RunnerEmitEvent::SessionBeforeCompact(_) => serde_json::from_value::<SessionBeforeCompactResult>(res_val)
            .ok()
            .map(SessionBeforeEventResult::Compact),
        RunnerEmitEvent::SessionBeforeTree(_) => serde_json::from_value::<SessionBeforeTreeResult>(res_val)
            .ok()
            .map(SessionBeforeEventResult::Tree),
        _ => None,
    }
}

fn tool_call_event_to_value(event: &ToolCallEvent) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("type".to_string(), Value::String("tool_call".to_string()));
    map.insert(
        "toolCallId".to_string(),
        Value::String(event.tool_call_id().to_string()),
    );
    if let Some(pid) = event.parent_tool_call_id() {
        map.insert("parentToolCallId".to_string(), Value::String(pid.to_string()));
    }
    map.insert("toolName".to_string(), Value::String(event.tool_name().to_string()));
    map.insert("input".to_string(), event.input_value());
    Value::Object(map)
}

fn apply_tool_call_mutated_input(event: &mut ToolCallEvent, mutated: &Value) {
    let Some(input_val) = mutated.get("input") else {
        return;
    };
    match event {
        ToolCallEvent::Bash(e) => {
            if let Ok(parsed) = serde_json::from_value(input_val.clone()) {
                e.input = parsed;
            }
        }
        ToolCallEvent::PowerShell(e) => {
            if let Ok(parsed) = serde_json::from_value(input_val.clone()) {
                e.input = parsed;
            }
        }
        ToolCallEvent::Read(e) => {
            if let Ok(parsed) = serde_json::from_value(input_val.clone()) {
                e.input = parsed;
            }
        }
        ToolCallEvent::Edit(e) => {
            if let Ok(parsed) = serde_json::from_value(input_val.clone()) {
                e.input = parsed;
            }
        }
        ToolCallEvent::Write(e) => {
            if let Ok(parsed) = serde_json::from_value(input_val.clone()) {
                e.input = parsed;
            }
        }
        ToolCallEvent::Grep(e) => {
            if let Ok(parsed) = serde_json::from_value(input_val.clone()) {
                e.input = parsed;
            }
        }
        ToolCallEvent::Find(e) => {
            if let Ok(parsed) = serde_json::from_value(input_val.clone()) {
                e.input = parsed;
            }
        }
        ToolCallEvent::Ls(e) => {
            if let Ok(parsed) = serde_json::from_value(input_val.clone()) {
                e.input = parsed;
            }
        }
        ToolCallEvent::Custom(e) => {
            if let Ok(parsed) = serde_json::from_value(input_val.clone()) {
                e.input = parsed;
            }
        }
    }
}

fn tool_result_event_to_value(event: &ToolResultEvent) -> Value {
    let mut val = match event {
        ToolResultEvent::Bash(e) => serde_json::to_value(e).unwrap_or(Value::Null),
        ToolResultEvent::PowerShell(e) => serde_json::to_value(e).unwrap_or(Value::Null),
        ToolResultEvent::Read(e) => serde_json::to_value(e).unwrap_or(Value::Null),
        ToolResultEvent::Edit(e) => serde_json::to_value(e).unwrap_or(Value::Null),
        ToolResultEvent::Write(e) => serde_json::to_value(e).unwrap_or(Value::Null),
        ToolResultEvent::Grep(e) => serde_json::to_value(e).unwrap_or(Value::Null),
        ToolResultEvent::Find(e) => serde_json::to_value(e).unwrap_or(Value::Null),
        ToolResultEvent::Ls(e) => serde_json::to_value(e).unwrap_or(Value::Null),
        ToolResultEvent::Custom(e) => serde_json::to_value(e).unwrap_or(Value::Null),
    };
    if let Value::Object(ref mut map) = val {
        map.insert("type".to_string(), Value::String("tool_result".to_string()));
        map.insert("toolName".to_string(), Value::String(event.tool_name().to_string()));
    }
    val
}

fn set_tool_result_content(event: &mut ToolResultEvent, content: Vec<ToolResultContentBlock>) {
    match event {
        ToolResultEvent::Bash(e) => e.content = content,
        ToolResultEvent::PowerShell(e) => e.content = content,
        ToolResultEvent::Read(e) => e.content = content,
        ToolResultEvent::Edit(e) => e.content = content,
        ToolResultEvent::Write(e) => e.content = content,
        ToolResultEvent::Grep(e) => e.content = content,
        ToolResultEvent::Find(e) => e.content = content,
        ToolResultEvent::Ls(e) => e.content = content,
        ToolResultEvent::Custom(e) => e.content = content,
    }
}

fn set_tool_result_structured_content(event: &mut ToolResultEvent, structured: Option<pi_ai::types::JsonValue>) {
    match event {
        ToolResultEvent::Bash(e) => e.structured_content = structured,
        ToolResultEvent::PowerShell(e) => e.structured_content = structured,
        ToolResultEvent::Read(e) => e.structured_content = structured,
        ToolResultEvent::Edit(e) => e.structured_content = structured,
        ToolResultEvent::Write(e) => e.structured_content = structured,
        ToolResultEvent::Grep(e) => e.structured_content = structured,
        ToolResultEvent::Find(e) => e.structured_content = structured,
        ToolResultEvent::Ls(e) => e.structured_content = structured,
        ToolResultEvent::Custom(e) => e.structured_content = structured,
    }
}

fn set_tool_result_details(event: &mut ToolResultEvent, details: Option<Value>) {
    match event {
        ToolResultEvent::Bash(e) => e.details = details.and_then(|v| serde_json::from_value(v).ok()),
        ToolResultEvent::PowerShell(e) => e.details = details.and_then(|v| serde_json::from_value(v).ok()),
        ToolResultEvent::Read(e) => e.details = details.and_then(|v| serde_json::from_value(v).ok()),
        ToolResultEvent::Edit(e) => e.details = details.and_then(|v| serde_json::from_value(v).ok()),
        ToolResultEvent::Write(_) => {}
        ToolResultEvent::Grep(e) => e.details = details.and_then(|v| serde_json::from_value(v).ok()),
        ToolResultEvent::Find(e) => e.details = details.and_then(|v| serde_json::from_value(v).ok()),
        ToolResultEvent::Ls(e) => e.details = details.and_then(|v| serde_json::from_value(v).ok()),
        ToolResultEvent::Custom(e) => e.details = details,
    }
}

fn set_tool_result_is_error(event: &mut ToolResultEvent, is_error: bool) {
    match event {
        ToolResultEvent::Bash(e) => e.is_error = is_error,
        ToolResultEvent::PowerShell(e) => e.is_error = is_error,
        ToolResultEvent::Read(e) => e.is_error = is_error,
        ToolResultEvent::Edit(e) => e.is_error = is_error,
        ToolResultEvent::Write(e) => e.is_error = is_error,
        ToolResultEvent::Grep(e) => e.is_error = is_error,
        ToolResultEvent::Find(e) => e.is_error = is_error,
        ToolResultEvent::Ls(e) => e.is_error = is_error,
        ToolResultEvent::Custom(e) => e.is_error = is_error,
    }
}

fn set_tool_result_usage(event: &mut ToolResultEvent, usage: Option<Usage>) {
    match event {
        ToolResultEvent::Bash(e) => e.usage = usage,
        ToolResultEvent::PowerShell(e) => e.usage = usage,
        ToolResultEvent::Read(e) => e.usage = usage,
        ToolResultEvent::Edit(e) => e.usage = usage,
        ToolResultEvent::Write(e) => e.usage = usage,
        ToolResultEvent::Grep(e) => e.usage = usage,
        ToolResultEvent::Find(e) => e.usage = usage,
        ToolResultEvent::Ls(e) => e.usage = usage,
        ToolResultEvent::Custom(e) => e.usage = usage,
    }
}

fn extract_tool_result_patch(event: &ToolResultEvent) -> ToolResultEventResult {
    match event {
        ToolResultEvent::Bash(e) => ToolResultEventResult {
            content: Some(e.content.clone()),
            details: e.details.as_ref().and_then(|d| serde_json::to_value(d).ok()),
            structured_content: e.structured_content.clone(),
            is_error: Some(e.is_error),
            usage: e.usage.clone(),
        },
        ToolResultEvent::PowerShell(e) => ToolResultEventResult {
            content: Some(e.content.clone()),
            details: e.details.as_ref().and_then(|d| serde_json::to_value(d).ok()),
            structured_content: e.structured_content.clone(),
            is_error: Some(e.is_error),
            usage: e.usage.clone(),
        },
        ToolResultEvent::Read(e) => ToolResultEventResult {
            content: Some(e.content.clone()),
            details: e.details.as_ref().and_then(|d| serde_json::to_value(d).ok()),
            structured_content: e.structured_content.clone(),
            is_error: Some(e.is_error),
            usage: e.usage.clone(),
        },
        ToolResultEvent::Edit(e) => ToolResultEventResult {
            content: Some(e.content.clone()),
            details: e.details.as_ref().and_then(|d| serde_json::to_value(d).ok()),
            structured_content: e.structured_content.clone(),
            is_error: Some(e.is_error),
            usage: e.usage.clone(),
        },
        ToolResultEvent::Write(e) => ToolResultEventResult {
            content: Some(e.content.clone()),
            details: None,
            structured_content: e.structured_content.clone(),
            is_error: Some(e.is_error),
            usage: e.usage.clone(),
        },
        ToolResultEvent::Grep(e) => ToolResultEventResult {
            content: Some(e.content.clone()),
            details: e.details.as_ref().and_then(|d| serde_json::to_value(d).ok()),
            structured_content: e.structured_content.clone(),
            is_error: Some(e.is_error),
            usage: e.usage.clone(),
        },
        ToolResultEvent::Find(e) => ToolResultEventResult {
            content: Some(e.content.clone()),
            details: e.details.as_ref().and_then(|d| serde_json::to_value(d).ok()),
            structured_content: e.structured_content.clone(),
            is_error: Some(e.is_error),
            usage: e.usage.clone(),
        },
        ToolResultEvent::Ls(e) => ToolResultEventResult {
            content: Some(e.content.clone()),
            details: e.details.as_ref().and_then(|d| serde_json::to_value(d).ok()),
            structured_content: e.structured_content.clone(),
            is_error: Some(e.is_error),
            usage: e.usage.clone(),
        },
        ToolResultEvent::Custom(e) => ToolResultEventResult {
            content: Some(e.content.clone()),
            details: e.details.clone(),
            structured_content: e.structured_content.clone(),
            is_error: Some(e.is_error),
            usage: e.usage.clone(),
        },
    }
}
