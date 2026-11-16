//! Port of packages/durable/src/harness/types.ts

use std::any::Any;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use indexmap::{IndexMap, IndexSet};
use pi_ai::models::Models;
use pi_ai::types::{
    AssistantMessage, CacheRetention, Message, ModelThinkingLevel, Tool, ToolCall, ToolResultContentBlock, Transport,
    Usage, UserContent,
};
use pi_chord::delta::Op;
use pi_chord::{AttachedReplicatedState, Context, JsonValue};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::documents::{
    ConversationDoc, ConversationDocFamily, RewindableConversationDocToken, RewindableConversationDocFamilyToken,
    SessionDocFamilyToken, SessionDocToken, TaskDocFamilyToken, TaskDocToken,
};
use crate::env::ExecutionEnv;
use crate::tasks::{AnyTask, Task};
use crate::types::{
    CommitPublication, ConversationId, ConversationOwnership, ConversationRecord, Cursor, DocumentWatch, EntryDraft,
    EntryId, EntryQuery, EntryRecord, ErasedTaskRecord, JoinPolicy, JsonObject, NextTaskState, Page, RunningTask, Seq,
    SubmissionId, SubmissionRecord, TaskId, TaskOptions, TaskOutcome, TaskRecord, WatchHandle,
};

/// Provider and model ID resolved through pi-ai `Models`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRef {
    pub provider: String,
    pub model_id: String,
}

pub type UserInput = UserContent;

/// Behavior when submitting input to a busy conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WhenBusy {
    Steer,
    FollowUp,
    Reject,
}

/// Host submission: user input that may start a run, or a passive entry write.
#[derive(Clone, Debug, PartialEq)]
pub enum SubmissionDraft {
    Input {
        request_id: Option<String>,
        content: UserInput,
        when_busy: Option<WhenBusy>,
    },
    Write {
        request_id: Option<String>,
        entry: EntryDraft,
    },
}

/// Input-only submission draft for internal/task-triggered turns.
#[derive(Clone, Debug, PartialEq)]
pub struct InputSubmissionDraft {
    pub request_id: Option<String>,
    pub content: UserInput,
    pub when_busy: Option<WhenBusy>,
}

/// Settled view of a submission returned from [`Submission::wait`].
#[derive(Clone, Debug, PartialEq)]
pub struct SettledSubmissionRecord {
    pub record: SubmissionRecord,
}

/// Result of aborting a queued/active submission.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbortSubmissionResult {
    Aborted,
    AlreadyPlaced,
    Settled,
    NotFound,
}

/// Terminal task receipt resolved by wait APIs.
#[derive(Clone, Debug, PartialEq)]
pub struct SettledTask<R = JsonValue> {
    pub id: TaskId<R>,
    pub conversation_id: ConversationId,
    pub kind: String,
    pub version: i64,
    pub input: JsonValue,
    pub owner: Option<TaskId>,
    pub background: bool,
    pub abort_requested: bool,
    pub outcome: TaskOutcome<R>,
}

impl<R> SettledTask<R> {
    pub fn from_record(record: ErasedTaskRecord) -> pi_js::Result<Self>
    where
        R: DeserializeOwned,
    {
        let outcome = match &record.state {
            crate::types::TaskState::Terminal { outcome } => crate::tasks::decode_outcome::<R>(outcome)?,
            _ => {
                return Err(pi_js::Error::msg(format!(
                    "Task {} is not terminal (status {:?})",
                    record.id,
                    record.state.status()
                )));
            }
        };
        Ok(Self {
            id: record.id.cast::<R>(),
            conversation_id: record.conversation_id,
            kind: record.kind,
            version: record.version,
            input: record.input,
            owner: record.owner,
            background: record.background,
            abort_requested: record.abort_requested,
            outcome,
        })
    }
}

/// Options of `Conversation.abort()`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConversationAbortOptions {
    pub background: bool,
}

/// Post-tools controls requested by a tool result.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolControl {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff: Option<String>,
}

/// Severity of a tool diagnostic remark.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiagnosticSeverity {
    Info,
    Warn,
    Error,
}

/// Remark about a call for the model and the UI; never part of the tool's data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDiagnostic {
    pub severity: DiagnosticSeverity,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

/// Result returned by executing one tool call.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutionResult<TDetails = JsonValue> {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ToolResultContentBlock>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<TDetails>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Vec<ToolDiagnostic>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<ToolControl>,
}

/// Whether the tools of one round run at once or one after another in call order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolExecutionMode {
    Parallel,
    Sequential,
}

/// How many queued items of one mode a boundary places: the first, or all of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QueueMode {
    All,
    OneAtATime,
}

/// Tool replay safety classification for crash recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolReplayMode {
    Safe,
    Unsafe,
}

/// Stream output retention policy for a tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OutputRetainMode {
    Head,
    Tail,
}

/// Output retention bounds declared on a registered tool.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolOutputLimits {
    pub max_bytes: Option<usize>,
    pub max_lines: Option<usize>,
    pub retain: Option<OutputRetainMode>,
}

/// Executable callback for a tool call.
pub type ToolExecuteFn = Arc<
    dyn Fn(JsonValue, ToolExecutionApi, Context) -> pi_js::BoxFuture<pi_js::Result<ToolExecutionResult>> + Send + Sync,
>;

/// Pure argument repair callback before schema validation.
pub type ToolPrepareArgumentsFn = Arc<dyn Fn(&JsonValue) -> pi_js::Result<JsonValue> + Send + Sync>;

/// Executable tool registered in a registry. Only `Tool` fields enter the transcript.
#[derive(Clone)]
pub struct ToolRegistration {
    pub name: String,
    pub description: String,
    pub parameters: JsonValue,
    pub replay: Option<ToolReplayMode>,
    pub execution_mode: Option<ToolExecutionMode>,
    pub prepare_arguments: Option<ToolPrepareArgumentsFn>,
    pub output_limits: Option<ToolOutputLimits>,
    pub execute: ToolExecuteFn,
}

impl ToolRegistration {
    pub fn as_pi_tool(&self) -> Tool {
        Tool {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
        }
    }
}

/// Object-safe document reader backend for prompt sections, hooks, tools, and task runtimes.
#[async_trait]
pub trait DocumentReaderBackend: Send + Sync {
    async fn snapshot_erased(
        &self,
        def: &Arc<crate::documents::ErasedDocDefinition>,
        conversation_id: Option<ConversationId>,
        task_id: Option<TaskId>,
        key: Option<&str>,
        context: Context,
    ) -> pi_js::Result<Option<JsonObject>>;

    async fn snapshot_as_of_erased(
        &self,
        def: &Arc<crate::documents::ErasedDocDefinition>,
        conversation_id: ConversationId,
        key: Option<&str>,
        at: EntryId,
        context: Context,
    ) -> pi_js::Result<Option<JsonObject>>;
}

/// Object-safe document watch acquisition backend.
#[async_trait]
pub trait DocumentObserverBackend: Send + Sync {
    async fn watch_erased(
        &self,
        def: &Arc<crate::documents::ErasedDocDefinition>,
        conversation_id: Option<ConversationId>,
        task_id: Option<TaskId>,
        key: Option<&str>,
        context: Context,
    ) -> pi_js::Result<Option<DocumentWatch<JsonObject>>>;
}

/// Committed document read capability shared across prompts, hooks, tools, and task runtimes.
#[derive(Clone)]
pub struct DocumentReader {
    backend: Arc<dyn DocumentReaderBackend>,
}

impl DocumentReader {
    pub fn from_backend(backend: Arc<dyn DocumentReaderBackend>) -> Self {
        Self { backend }
    }

    pub async fn snapshot_session<T: DeserializeOwned>(
        &self,
        token: &SessionDocToken<T>,
        context: Context,
    ) -> pi_js::Result<Option<T>> {
        self.backend
            .snapshot_erased(&token.definition, None, None, None, context)
            .await?
            .map(|obj| crate::documents::json_object_to_value(&obj))
            .transpose()
    }

    pub async fn snapshot_conversation<T: DeserializeOwned>(
        &self,
        token: &impl ConversationDoc<T>,
        conversation_id: ConversationId,
        context: Context,
    ) -> pi_js::Result<Option<T>> {
        self.backend
            .snapshot_erased(token.definition(), Some(conversation_id), None, None, context)
            .await?
            .map(|obj| crate::documents::json_object_to_value(&obj))
            .transpose()
    }

    pub async fn snapshot_task<T: DeserializeOwned>(
        &self,
        token: &TaskDocToken<T>,
        task_id: TaskId,
        context: Context,
    ) -> pi_js::Result<Option<T>> {
        self.backend
            .snapshot_erased(&token.definition, None, Some(task_id), None, context)
            .await?
            .map(|obj| crate::documents::json_object_to_value(&obj))
            .transpose()
    }

    pub async fn snapshot_session_family<T: DeserializeOwned, I>(
        &self,
        token: &SessionDocFamilyToken<T, I>,
        key: &str,
        context: Context,
    ) -> pi_js::Result<Option<T>> {
        self.backend
            .snapshot_erased(&token.definition, None, None, Some(key), context)
            .await?
            .map(|obj| crate::documents::json_object_to_value(&obj))
            .transpose()
    }

    pub async fn snapshot_conversation_family<T: DeserializeOwned, I>(
        &self,
        token: &impl ConversationDocFamily<T, I>,
        conversation_id: ConversationId,
        key: &str,
        context: Context,
    ) -> pi_js::Result<Option<T>> {
        self.backend
            .snapshot_erased(token.definition(), Some(conversation_id), None, Some(key), context)
            .await?
            .map(|obj| crate::documents::json_object_to_value(&obj))
            .transpose()
    }

    pub async fn snapshot_task_family<T: DeserializeOwned, I>(
        &self,
        token: &TaskDocFamilyToken<T, I>,
        task_id: TaskId,
        key: &str,
        context: Context,
    ) -> pi_js::Result<Option<T>> {
        self.backend
            .snapshot_erased(&token.definition, None, Some(task_id), Some(key), context)
            .await?
            .map(|obj| crate::documents::json_object_to_value(&obj))
            .transpose()
    }

    pub async fn snapshot_as_of<T: DeserializeOwned>(
        &self,
        token: &RewindableConversationDocToken<T>,
        conversation_id: ConversationId,
        at: EntryId,
        context: Context,
    ) -> pi_js::Result<Option<T>> {
        self.backend
            .snapshot_as_of_erased(&token.definition, conversation_id, None, at, context)
            .await?
            .map(|obj| crate::documents::json_object_to_value(&obj))
            .transpose()
    }

    pub async fn snapshot_family_as_of<T: DeserializeOwned, I>(
        &self,
        token: &RewindableConversationDocFamilyToken<T, I>,
        conversation_id: ConversationId,
        key: &str,
        at: EntryId,
        context: Context,
    ) -> pi_js::Result<Option<T>> {
        self.backend
            .snapshot_as_of_erased(&token.definition, conversation_id, Some(key), at, context)
            .await?
            .map(|obj| crate::documents::json_object_to_value(&obj))
            .transpose()
    }
}

/// Input to system prompt section rendering for one request preparation.
#[derive(Clone)]
pub struct PromptInput {
    pub conversation_id: ConversationId,
    pub agent: Agent,
    pub env: Option<Arc<dyn ExecutionEnv>>,
    pub shown: IndexMap<String, String>,
    pub read: DocumentReader,
}

/// Async section renderer callback.
pub type PromptRenderFn =
    Arc<dyn Fn(PromptInput, Context) -> pi_js::BoxFuture<pi_js::Result<Option<String>>> + Send + Sync>;

/// One system prompt section; the agent's sections render in order before each request.
#[derive(Clone)]
pub struct PromptSection {
    pub key: String,
    pub render: PromptRenderFn,
    /// Default true: wrap the text as `<key>\n...\n</key>`.
    pub tag: bool,
}

/// Built by `hook()`; matches tasks by name.
#[derive(Clone)]
pub struct HookRegistration {
    pub task: String,
    pub handlers: Arc<dyn Any + Send + Sync>,
}

/// Built by `wrap_tool()` and `wrap_section()`; targets a tool name or a section key.
#[derive(Clone)]
pub enum Wrap {
    Tool {
        tool: String,
        wrap: Arc<dyn Fn(ToolRegistration) -> ToolRegistration + Send + Sync>,
    },
    Section {
        section: String,
        wrap: Arc<dyn Fn(PromptSection) -> PromptSection + Send + Sync>,
    },
}

/// Named bundle of code; installed in a registry and selected by conversations by name.
#[derive(Clone, Default)]
pub struct Extension {
    pub name: String,
    pub tools: Vec<ToolRegistration>,
    pub sections: Vec<PromptSection>,
    pub hooks: Vec<HookRegistration>,
    pub wraps: Vec<Wrap>,
    pub tasks: Vec<AnyTask>,
}

/// Tool paired with the extension that owns it.
#[derive(Clone)]
pub struct ExtensionTool {
    pub extension: Extension,
    pub tool: ToolRegistration,
}

/// Section paired with the extension that owns it.
#[derive(Clone)]
pub struct ExtensionSection {
    pub extension: Extension,
    pub section: PromptSection,
}

/// Immutable view of one published registry state.
pub trait RegistrySnapshot: Send + Sync {
    fn installed(&self) -> &[Extension];
    fn extension(&self, name: &str) -> Option<&Extension>;
    fn tools(&self) -> Vec<ExtensionTool>;
    fn sections(&self) -> Vec<ExtensionSection>;
    fn tasks(&self) -> Vec<AnyTask>;
    fn task(&self, name: &str) -> Option<&AnyTask>;
}

/// Read side of a registry consumed by a Harness.
pub trait RegistryReader: Send + Sync {
    fn snapshot(&self) -> Arc<dyn RegistrySnapshot>;
    fn subscribe(&self, listener: Arc<dyn Fn() + Send + Sync>) -> pi_js::Unsubscribe;
}

/// Extension selection stored in [`AgentState`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentExtensionsFilter {
    List(Vec<String>),
    Patch {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        add: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remove: Option<Vec<String>>,
    },
}

/// Tool filter stored in [`AgentState`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentToolsFilter {
    List(Vec<String>),
    Remove { remove: Vec<String> },
}

/// Stored choices of one conversation; names, not objects. Unset fields follow the host.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<ModelThinkingLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<AgentExtensionsFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<AgentToolsFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// Patch state for one field in [`AgentChange`]: unchanged, cleared (`null`), or set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum FieldChange<T> {
    #[default]
    Unchanged,
    Clear,
    Set(T),
}

impl<T> FieldChange<T> {
    pub fn set(value: T) -> Self {
        Self::Set(value)
    }

    pub fn clear() -> Self {
        Self::Clear
    }
}

/// Extension selection change requested via [`AgentChange`].
#[derive(Clone)]
pub enum AgentExtensionsChange {
    Replace(Vec<String>),
    Patch {
        add: Option<Vec<String>>,
        remove: Option<Vec<String>>,
    },
}

impl AgentExtensionsChange {
    pub fn replace(extensions: &[Extension]) -> Self {
        Self::Replace(extensions.iter().map(|ext| ext.name.clone()).collect())
    }

    pub fn patch(add: Option<&[Extension]>, remove: Option<&[Extension]>) -> Self {
        Self::Patch {
            add: add.map(|list| list.iter().map(|ext| ext.name.clone()).collect()),
            remove: remove.map(|list| list.iter().map(|ext| ext.name.clone()).collect()),
        }
    }
}

/// Tool filter change requested via [`AgentChange`].
#[derive(Clone)]
pub enum AgentToolsChange {
    Replace(Vec<String>),
    Remove(Vec<String>),
}

impl AgentToolsChange {
    pub fn replace(tools: &[ToolRegistration]) -> Self {
        Self::Replace(tools.iter().map(|tool| tool.name.clone()).collect())
    }

    pub fn remove(tools: &[ToolRegistration]) -> Self {
        Self::Remove(tools.iter().map(|tool| tool.name.clone()).collect())
    }
}

/// A change to `pi.agent`: `Set` replaces, `Clear` deletes, `Unchanged` keeps the existing value.
#[derive(Clone, Default)]
pub struct AgentChange {
    pub model: FieldChange<ModelRef>,
    pub thinking_level: FieldChange<ModelThinkingLevel>,
    pub extensions: FieldChange<AgentExtensionsChange>,
    pub tools: FieldChange<AgentToolsChange>,
    pub instructions: FieldChange<String>,
    pub cwd: FieldChange<String>,
}

/// A conversation's agent resolved against a registry snapshot and the settings.
#[derive(Clone)]
pub struct Agent {
    pub model: Option<ModelRef>,
    pub thinking_level: ModelThinkingLevel,
    pub extensions: Vec<Extension>,
    pub tools: Vec<ToolRegistration>,
    pub sections: Vec<PromptSection>,
    pub instructions: Option<String>,
    pub cwd: Option<String>,
}

/// Callback run inside the creating commit of a new or forked conversation.
pub type ConversationInitFn = Arc<
    dyn Fn(crate::session::transaction::Tx, ConversationId) -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync,
>;

#[derive(Clone)]
pub struct ConversationCreateOptions {
    pub ownership: ConversationOwnership,
    pub agent: Option<AgentChange>,
    pub init: Option<ConversationInitFn>,
}

/// Deferred mode window option.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DeferredWindow {
    #[serde(rename = "15m")]
    M15,
    #[serde(rename = "1h")]
    H1,
    #[serde(rename = "24h")]
    H24,
}

/// Deferred option for [`ConversationStreamOptions`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StreamDeferredOption {
    Bool(bool),
    Window { window: Option<DeferredWindow> },
}

/// Curated pi-ai request options; absent fields use pi-ai defaults.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationStreamOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<Transport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retry_delay_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<IndexMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<JsonObject>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_retention: Option<CacheRetention>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred: Option<StreamDeferredOption>,
}

/// Durable generation attempt retries; JSON shape of retry policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationRetryPolicy {
    pub enabled: bool,
    pub max_retries: u32,
    pub base_delay_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_agent_delay_ms: Option<u64>,
}

/// Partial override for [`ConversationRetryPolicy`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PartialRetryPolicy {
    pub enabled: Option<bool>,
    pub max_retries: Option<u32>,
    pub base_delay_ms: Option<u64>,
    pub max_agent_delay_ms: Option<Option<u64>>,
}

/// Automatic compaction thresholds (spec §8.7); manual compaction ignores `enabled`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionPolicy {
    pub enabled: bool,
    pub reserve_tokens: u64,
    pub keep_recent_tokens: u64,
    pub background_tokens: u64,
}

/// Partial override for [`CompactionPolicy`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PartialCompactionPolicy {
    pub enabled: Option<bool>,
    pub reserve_tokens: Option<u64>,
    pub keep_recent_tokens: Option<u64>,
    pub background_tokens: Option<u64>,
}

/// Why a compaction runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CompactionReason {
    Manual,
    Threshold,
    Overflow,
}

/// Result returned when a compaction completes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<EntryId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submission_id: Option<SubmissionId>,
}

/// Resolved settings over built-in defaults.
#[derive(Clone, Default)]
pub struct Settings {
    pub extensions: Option<Vec<Extension>>,
    pub stream: ConversationStreamOptions,
    pub retry: ConversationRetryPolicy,
    pub compaction: CompactionPolicy,
    pub tool_execution: ToolExecutionMode,
    pub steering_mode: QueueMode,
    pub follow_up_mode: QueueMode,
}

impl Default for ConversationRetryPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            max_retries: 3,
            base_delay_ms: 2000,
            max_agent_delay_ms: Some(60_000),
        }
    }
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            reserve_tokens: 16_384,
            keep_recent_tokens: 20_000,
            background_tokens: 32_768,
        }
    }
}

impl Default for ToolExecutionMode {
    fn default() -> Self {
        Self::Parallel
    }
}

impl Default for QueueMode {
    fn default() -> Self {
        Self::OneAtATime
    }
}

/// Application-provided settings provider (supports dynamic getters / snapshots per access).
#[derive(Clone, Default)]
pub struct HarnessSettingsSnapshot {
    pub extensions: Option<Vec<Extension>>,
    pub stream: Option<ConversationStreamOptions>,
    pub retry: Option<PartialRetryPolicy>,
    pub compaction: Option<PartialCompactionPolicy>,
    pub tool_execution: Option<ToolExecutionMode>,
    pub steering_mode: Option<QueueMode>,
    pub follow_up_mode: Option<QueueMode>,
}

/// Dynamic or static settings interface matching TS getter-capable `HarnessSettings`.
pub trait HarnessSettings: Send + Sync {
    fn get(&self) -> HarnessSettingsSnapshot;
}

impl HarnessSettings for HarnessSettingsSnapshot {
    fn get(&self) -> HarnessSettingsSnapshot {
        self.clone()
    }
}

/// What `HarnessOptions.env` builds an environment for.
#[derive(Clone)]
pub struct EnvTarget {
    pub conversation_id: ConversationId,
    pub cwd: Option<String>,
    pub read: DocumentReader,
}

pub type EnvFactory = Arc<
    dyn Fn(EnvTarget, Context) -> pi_js::BoxFuture<pi_js::Result<Option<Arc<dyn ExecutionEnv>>>> + Send + Sync,
>;

pub type ConversationCreatedHook =
    Arc<dyn Fn(crate::session::transaction::Tx, ConversationRecord) -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync>;

pub type NowFn = Arc<dyn Fn() -> i64 + Send + Sync>;
pub type OnReportFn = Arc<dyn Fn(pi_js::Error) + Send + Sync>;

/// Configuration options passed to `Harness::create`.
#[derive(Clone)]
pub struct HarnessOptions {
    pub models: Models,
    pub registry: Arc<dyn RegistryReader>,
    pub settings: Option<Arc<dyn HarnessSettings>>,
    pub env: Option<EnvFactory>,
    pub conversation_created: Option<ConversationCreatedHook>,
    pub now: Option<NowFn>,
    pub on_report: Option<OnReportFn>,
}

/// Why a task is blocked under the current registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskBlockedReason {
    MissingTask,
    TaskTooOld,
    MigrationFailed,
}

/// Scheduler inspection state for one live task.
#[derive(Clone, Debug)]
pub enum TaskInspectionState {
    Running,
    Ready { migrates: bool },
    Waiting { on: Vec<TaskId> },
    Completing,
    Blocked {
        reason: TaskBlockedReason,
        error: Option<Arc<pi_js::Error>>,
    },
}

/// Live task and what the scheduler would do with it under the current registry.
#[derive(Clone, Debug)]
pub struct TaskInspection {
    pub record: ErasedTaskRecord,
    pub state: TaskInspectionState,
}

/// Scheduling status reported by `Harness::inspect`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SchedulingStatus {
    Paused,
    Running,
    Closing,
}

/// Point-in-time view of live work: unfinished tasks and submissions.
#[derive(Clone, Debug)]
pub struct HarnessInspection {
    pub scheduling: SchedulingStatus,
    pub tasks: Vec<TaskInspection>,
    pub submissions: Vec<SubmissionRecord>,
}

/// Raw active transcript and derived model context.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextView {
    pub head: Option<EntryRecord>,
    pub entries: Vec<EntryRecord>,
    pub contributions: Vec<Vec<Message>>,
    pub messages: Vec<Message>,
}

/// Ledger of one conversation's own spend.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageState {
    pub models: IndexMap<String, Usage>,
    pub tools: IndexMap<String, Usage>,
}

/// Structural mount of one conversation's active transcript and built-in documents (spec §9.3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationView {
    pub conversation: ConversationRecord,
    pub entries: Vec<EntryRecord>,
    pub docs: IndexMap<String, JsonObject>,
}

/// Live task status summary inside [`TaskGraph`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum TaskGraphState {
    Pending { phase: String },
    Running { phase: String },
    Waiting {
        phase: String,
        on: Vec<TaskId>,
        policy: JoinPolicy,
    },
    Completing { outcome: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskGraphNode {
    pub id: TaskId,
    pub kind: String,
    pub conversation_id: ConversationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<TaskId>,
    pub background: bool,
    pub abort_requested: bool,
    pub state: TaskGraphState,
    pub conversations: Vec<ConversationId>,
}

/// Every live task of the Session (spec §9.5).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskGraph {
    pub tasks: IndexMap<String, TaskGraphNode>,
}

pub type ConversationWatch = WatchHandle<ConversationView>;
pub type TaskGraphWatch = WatchHandle<TaskGraph>;

/// What a hook may use: committed reads and the asking task's memos.
#[async_trait]
pub trait HookApiBackend: DocumentReaderBackend + Send + Sync {
    fn task_id(&self) -> TaskId;
    fn conversation_id(&self) -> ConversationId;
    async fn memo_get(&self, name: &str, context: Context) -> pi_js::Result<Option<JsonValue>>;
    async fn memo_set(&self, name: &str, candidate: JsonValue, context: Context) -> pi_js::Result<JsonValue>;
}

#[derive(Clone)]
pub struct HookApi {
    backend: Arc<dyn HookApiBackend>,
}

impl HookApi {
    pub fn from_backend(backend: Arc<dyn HookApiBackend>) -> Self {
        Self { backend }
    }

    pub fn task_id(&self) -> TaskId {
        self.backend.task_id()
    }

    pub fn conversation_id(&self) -> ConversationId {
        self.backend.conversation_id()
    }

    pub fn read(&self) -> DocumentReader {
        DocumentReader::from_backend(Arc::clone(&self.backend) as Arc<dyn DocumentReaderBackend>)
    }

    pub async fn memo_get<T: DeserializeOwned>(&self, name: &str, context: Context) -> pi_js::Result<Option<T>> {
        self.backend
            .memo_get(name, context)
            .await?
            .map(|v| serde_json::from_value(v).map_err(|err| pi_js::Error::js("TypeError", err.to_string())))
            .transpose()
    }

    pub async fn memo_set<T: Serialize + DeserializeOwned>(
        &self,
        name: &str,
        candidate: &T,
        context: Context,
    ) -> pi_js::Result<T> {
        let encoded =
            serde_json::to_value(candidate).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
        let winner = self.backend.memo_set(name, encoded, context).await?;
        serde_json::from_value(winner).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))
    }
}

/// Invocation-bound conversation operations for tasks and tools.
#[async_trait]
pub trait ConversationHandleBackend: Send + Sync {
    fn id(&self) -> ConversationId;
    async fn submit(
        &self,
        submission: InputSubmissionDraft,
        context: Context,
    ) -> pi_js::Result<crate::harness::submissions::SubmissionHandle>;
    async fn abort(&self, context: Context, options: Option<ConversationAbortOptions>) -> pi_js::Result<()>;
    async fn wait_for_idle(&self, context: Context) -> pi_js::Result<()>;
}

#[derive(Clone)]
pub struct ConversationHandle {
    backend: Arc<dyn ConversationHandleBackend>,
}

impl ConversationHandle {
    pub fn from_backend(backend: Arc<dyn ConversationHandleBackend>) -> Self {
        Self { backend }
    }

    pub fn id(&self) -> ConversationId {
        self.backend.id()
    }

    pub async fn submit(
        &self,
        submission: InputSubmissionDraft,
        context: Context,
    ) -> pi_js::Result<crate::harness::submissions::SubmissionHandle> {
        self.backend.submit(submission, context).await
    }

    pub async fn abort(&self, context: Context, options: Option<ConversationAbortOptions>) -> pi_js::Result<()> {
        self.backend.abort(context, options).await
    }

    pub async fn wait_for_idle(&self, context: Context) -> pi_js::Result<()> {
        self.backend.wait_for_idle(context).await
    }
}

/// Object-safe backend for [`ToolExecutionApi`].
#[async_trait]
pub trait ToolExecutionApiBackend: DocumentReaderBackend + DocumentObserverBackend + Send + Sync {
    fn task_id(&self) -> TaskId;
    fn conversation_id(&self) -> ConversationId;
    fn call_id(&self) -> &str;
    fn registry(&self) -> Arc<dyn RegistrySnapshot>;
    async fn agent(&self, context: Context) -> pi_js::Result<Agent>;
    fn env(&self) -> Option<Arc<dyn ExecutionEnv>>;
    fn output_text(&self, chunk: &str) -> pi_js::Result<()>;
    fn output_bytes(&self, chunk: &[u8]) -> pi_js::Result<()>;
    fn diagnostic(&self, diagnostic: ToolDiagnostic) -> pi_js::Result<()>;
    async fn details(&self, value: JsonValue, context: Context) -> pi_js::Result<()>;
    async fn commit_erased(
        &self,
        change: Box<
            dyn FnOnce(crate::session::transaction::Tx) -> pi_js::BoxFuture<pi_js::Result<JsonValue>> + Send,
        >,
        context: Context,
    ) -> pi_js::Result<JsonValue>;
    async fn memo_get(&self, name: &str, context: Context) -> pi_js::Result<Option<JsonValue>>;
    async fn memo_set(&self, name: &str, candidate: JsonValue, context: Context) -> pi_js::Result<JsonValue>;
    async fn create_task_erased(
        &self,
        task: &AnyTask,
        input: JsonValue,
        ownership: TaskOwnership,
        background: bool,
        context: Context,
    ) -> pi_js::Result<TaskId>;
    async fn get_task(&self, id: TaskId, context: Context) -> pi_js::Result<Option<ErasedTaskRecord>>;
    async fn wait_for_task(&self, id: TaskId, context: Context) -> pi_js::Result<SettledTask<JsonValue>>;
    async fn conversation(
        &self,
        id: ConversationId,
        context: Context,
    ) -> pi_js::Result<Option<ConversationHandle>>;
}

/// Operations available to one tool invocation.
#[derive(Clone)]
pub struct ToolExecutionApi {
    backend: Arc<dyn ToolExecutionApiBackend>,
}

impl ToolExecutionApi {
    pub fn from_backend(backend: Arc<dyn ToolExecutionApiBackend>) -> Self {
        Self { backend }
    }

    pub fn task_id(&self) -> TaskId {
        self.backend.task_id()
    }

    pub fn conversation_id(&self) -> ConversationId {
        self.backend.conversation_id()
    }

    pub fn call_id(&self) -> &str {
        self.backend.call_id()
    }

    pub fn registry(&self) -> Arc<dyn RegistrySnapshot> {
        self.backend.registry()
    }

    pub async fn agent(&self, context: Context) -> pi_js::Result<Agent> {
        self.backend.agent(context).await
    }

    pub fn env(&self) -> Option<Arc<dyn ExecutionEnv>> {
        self.backend.env()
    }

    pub fn output(&self, chunk: &str) -> pi_js::Result<()> {
        self.backend.output_text(chunk)
    }

    pub fn output_bytes(&self, chunk: &[u8]) -> pi_js::Result<()> {
        self.backend.output_bytes(chunk)
    }

    pub fn diagnostic(&self, diagnostic: ToolDiagnostic) -> pi_js::Result<()> {
        self.backend.diagnostic(diagnostic)
    }

    pub async fn details<T: Serialize>(&self, value: &T, context: Context) -> pi_js::Result<()> {
        let encoded = serde_json::to_value(value).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
        self.backend.details(encoded, context).await
    }

    pub fn read(&self) -> DocumentReader {
        DocumentReader::from_backend(Arc::clone(&self.backend) as Arc<dyn DocumentReaderBackend>)
    }

    pub async fn commit<T, F, Fut>(&self, change: F, context: Context) -> pi_js::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(crate::session::transaction::Tx) -> Fut + Send + 'static,
        Fut: Future<Output = pi_js::Result<T>> + Send + 'static,
    {
        let boxed_res = Arc::new(std::sync::Mutex::new(None::<T>));
        let slot = Arc::clone(&boxed_res);
        self.backend
            .commit_erased(
                Box::new(move |tx| {
                    Box::pin(async move {
                        let out = change(tx).await?;
                        *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(out);
                        Ok(JsonValue::Null)
                    })
                }),
                context,
            )
            .await?;
        let out = boxed_res
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .ok_or_else(|| pi_js::Error::msg("Tool commit did not yield a result"))?;
        Ok(out)
    }

    pub async fn memo_get<T: DeserializeOwned>(&self, name: &str, context: Context) -> pi_js::Result<Option<T>> {
        self.backend
            .memo_get(name, context)
            .await?
            .map(|v| serde_json::from_value(v).map_err(|err| pi_js::Error::js("TypeError", err.to_string())))
            .transpose()
    }

    pub async fn memo_set<T: Serialize + DeserializeOwned>(
        &self,
        name: &str,
        candidate: &T,
        context: Context,
    ) -> pi_js::Result<T> {
        let encoded =
            serde_json::to_value(candidate).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
        let winner = self.backend.memo_set(name, encoded, context).await?;
        serde_json::from_value(winner).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))
    }

    pub async fn create_task<I, S, R, H>(
        &self,
        task: &Task<I, S, R, H>,
        input: &I,
        ownership: TaskOwnership,
        background: bool,
        context: Context,
    ) -> pi_js::Result<TaskId<R>>
    where
        I: Serialize,
    {
        let encoded = serde_json::to_value(input).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
        let raw_id = self
            .backend
            .create_task_erased(task.as_any(), encoded, ownership, background, context)
            .await?;
        Ok(raw_id.cast::<R>())
    }

    pub async fn get_task<R>(&self, id: TaskId<R>, context: Context) -> pi_js::Result<Option<ErasedTaskRecord>> {
        self.backend.get_task(id.erased(), context).await
    }

    pub async fn wait_for_task<R: DeserializeOwned>(
        &self,
        id: TaskId<R>,
        context: Context,
    ) -> pi_js::Result<SettledTask<R>> {
        let raw = self.backend.wait_for_task(id.erased(), context).await?;
        let outcome = crate::tasks::decode_outcome::<R>(&raw.outcome)?;
        Ok(SettledTask {
            id: raw.id.cast::<R>(),
            conversation_id: raw.conversation_id,
            kind: raw.kind,
            version: raw.version,
            input: raw.input,
            owner: raw.owner,
            background: raw.background,
            abort_requested: raw.abort_requested,
            outcome,
        })
    }

    pub async fn conversation(
        &self,
        id: ConversationId,
        context: Context,
    ) -> pi_js::Result<Option<ConversationHandle>> {
        self.backend.conversation(id, context).await
    }

    pub async fn watch_conversation_doc<T: DeserializeOwned + Send + Sync + 'static>(
        &self,
        token: &impl ConversationDoc<T>,
        conversation_id: ConversationId,
        context: Context,
    ) -> pi_js::Result<Option<DocumentWatch<T>>> {
        let Some(watch) = self
            .backend
            .watch_erased(token.definition(), Some(conversation_id), None, None, context)
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(wrap_typed_watch(watch)))
    }
}

pub(crate) fn wrap_typed_watch<T: DeserializeOwned + Send + Sync + 'static>(
    raw: DocumentWatch<JsonObject>,
) -> DocumentWatch<T> {
    struct MappedWatch<T> {
        raw: DocumentWatch<JsonObject>,
        _marker: std::marker::PhantomData<fn() -> T>,
    }

    #[async_trait]
    impl<T: DeserializeOwned + Send + Sync + 'static> crate::types::WatchHandleApi<Option<T>> for MappedWatch<T> {
        fn value(&self) -> Option<T> {
            self.raw
                .value()
                .and_then(|obj| crate::documents::json_object_to_value(&obj).ok())
        }

        fn start(&self, listener: crate::types::WatchListener<Option<T>>) -> pi_js::Result<()> {
            self.raw.start(Arc::new(move |val, ops, ctx| {
                let listener = Arc::clone(&listener);
                Box::pin(async move {
                    let typed = val
                        .map(|obj| crate::documents::json_object_to_value(&obj))
                        .transpose()?;
                    listener(typed, ops, ctx).await
                })
            }))
        }

        async fn stop(&self) -> crate::types::WatchEnd {
            self.raw.stop().await
        }

        async fn closed(&self) -> crate::types::WatchEnd {
            self.raw.closed().await
        }
    }

    WatchHandle::from_inner(Arc::new(MappedWatch {
        raw,
        _marker: std::marker::PhantomData,
    }))
}

/// Object-safe backend powering [`TaskRuntime`] for all tasks.
#[async_trait]
pub trait TaskRuntimeBackend: DocumentReaderBackend + DocumentObserverBackend + Send + Sync {
    fn task_id(&self) -> TaskId;
    fn conversation_id(&self) -> ConversationId;
    fn signal(&self) -> pi_js::abort::AbortSignal;
    fn registry(&self) -> Arc<dyn RegistrySnapshot>;
    async fn agent(&self, context: Context) -> pi_js::Result<Agent>;
    fn settings(&self) -> Settings;
    fn models(&self) -> &Models;
    async fn env(&self, context: Context) -> pi_js::Result<Option<Arc<dyn ExecutionEnv>>>;
    fn hooks_for_task(&self, task_name: &str) -> Vec<Arc<dyn Any + Send + Sync>>;
    async fn commit_erased(
        &self,
        change: Box<
            dyn FnOnce(
                    crate::session::transaction::Tx,
                    RunningTask<JsonValue, JsonValue, JsonValue>,
                ) -> pi_js::BoxFuture<pi_js::Result<Option<NextTaskState<JsonValue, JsonValue>>>>
                + Send,
        >,
        context: Context,
    ) -> pi_js::Result<()>;
    async fn memo_get(&self, name: &str, context: Context) -> pi_js::Result<Option<JsonValue>>;
    async fn memo_set(&self, name: &str, candidate: JsonValue, context: Context) -> pi_js::Result<JsonValue>;
    async fn get_task(&self, id: TaskId, context: Context) -> pi_js::Result<Option<ErasedTaskRecord>>;
    async fn wait_for_task(&self, id: TaskId, context: Context) -> pi_js::Result<SettledTask<JsonValue>>;
    async fn outcomes(&self, ids: &[TaskId], context: Context) -> pi_js::Result<Vec<TaskOutcome<JsonValue>>>;
    async fn conversation(
        &self,
        id: ConversationId,
        context: Context,
    ) -> pi_js::Result<Option<ConversationHandle>>;
    async fn entry(&self, id: EntryId, context: Context) -> pi_js::Result<Option<EntryRecord>>;
    async fn context_view(
        &self,
        conversation_id: ConversationId,
        context: Context,
        at: Option<EntryId>,
    ) -> pi_js::Result<ContextView>;
    fn now(&self) -> i64;
    fn report(&self, error: pi_js::Error);
    async fn sleep(&self, until: i64, context: Context) -> pi_js::Result<()>;
    fn hook_api(&self) -> HookApi;
}

/// Extension wrapper around [`crate::tasks::TaskRuntimeHandle`] providing ergonomic typed operations.
#[async_trait]
pub trait TaskRuntimeExt<I, S, R, H> {
    fn backend(&self) -> &Arc<dyn TaskRuntimeBackend>;
}

/// Dispatches matching hooks for a task in extension order, isolating throws until signalled.
#[derive(Clone)]
pub struct HookRunner<H: Send + Sync + 'static> {
    backend: Arc<dyn TaskRuntimeBackend>,
    task_name: String,
    _marker: std::marker::PhantomData<fn() -> H>,
}

impl<H: Send + Sync + 'static> HookRunner<H> {
    pub fn new(backend: Arc<dyn TaskRuntimeBackend>, task_name: impl Into<String>) -> Self {
        Self {
            backend,
            task_name: task_name.into(),
            _marker: std::marker::PhantomData,
        }
    }

    pub async fn each<F, Fut>(&self, mut invoke: F) -> pi_js::Result<()>
    where
        F: FnMut(&H, HookApi) -> Fut,
        Fut: Future<Output = pi_js::Result<()>>,
    {
        let api = self.backend.hook_api();
        let signal = self.backend.signal();
        let handlers = self.backend.hooks_for_task(&self.task_name);
        for any_handler in handlers {
            let Some(handler) = any_handler.downcast_ref::<H>() else {
                continue;
            };
            if let Err(err) = invoke(handler, api.clone()).await {
                if signal.aborted() {
                    return Err(err);
                }
                self.backend.report(err);
            }
        }
        Ok(())
    }
}

pub type HookResult<T> = pi_js::Result<Option<T>>;

/// Hooks of the built-in generation task.
#[derive(Clone, Default)]
pub struct GenerationHooks {
    pub before_request: Option<
        Arc<
            dyn Fn(Vec<Message>, HookApi, Context) -> pi_js::BoxFuture<HookResult<Vec<Message>>> + Send + Sync,
        >,
    >,
    pub after_response:
        Option<Arc<dyn Fn(AssistantMessage, HookApi, Context) -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync>>,
    pub on_yield: Option<
        Arc<
            dyn Fn(AssistantMessage, HookApi, Context) -> pi_js::BoxFuture<HookResult<UserInput>> + Send + Sync,
        >,
    >,
    pub after_tools: Option<
        Arc<
            dyn Fn(EntryId, Vec<EntryId>, HookApi, Context) -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync,
        >,
    >,
}

/// Decision returned by a [`ToolHooks::before_tool`] handler.
#[derive(Clone, Debug, PartialEq)]
pub enum BeforeToolDecision {
    Arguments(JsonObject),
    Block(String),
}

/// Hooks of the built-in tool task.
#[derive(Clone, Default)]
pub struct ToolHooks {
    pub before_tool: Option<
        Arc<
            dyn Fn(ToolCall, HookApi, Context) -> pi_js::BoxFuture<HookResult<BeforeToolDecision>> + Send + Sync,
        >,
    >,
    pub after_tool: Option<
        Arc<
            dyn Fn(ToolCall, ToolExecutionResult, HookApi, Context) -> pi_js::BoxFuture<HookResult<ToolExecutionResult>>
                + Send
                + Sync,
        >,
    >,
}

/// Input context passed to [`CompactionHooks::before_compact`].
#[derive(Clone, Debug)]
pub struct BeforeCompactInput {
    pub reason: CompactionReason,
    pub entries: Vec<EntryRecord>,
    pub messages: Vec<Message>,
    pub first_kept: EntryId,
    pub instructions: Option<String>,
}

/// Decision returned by [`CompactionHooks::before_compact`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BeforeCompactDecision {
    Decline,
    Summary(String),
}

/// Hooks of the built-in compaction task.
#[derive(Clone, Default)]
pub struct CompactionHooks {
    pub before_compact: Option<
        Arc<
            dyn Fn(BeforeCompactInput, HookApi, Context) -> pi_js::BoxFuture<HookResult<BeforeCompactDecision>>
                + Send
                + Sync,
        >,
    >,
}

/// Helper to invoke a [`TaskRuntimeHandle`] cleanly with concrete task generics.
impl<I, S, R, H> crate::tasks::TaskRuntimeHandle<I, S, R, H>
where
    I: DeserializeOwned + Send + Sync + 'static,
    S: Serialize + DeserializeOwned + Send + Sync + 'static,
    R: Serialize + DeserializeOwned + Send + Sync + 'static,
    H: Send + Sync + 'static,
{
    fn rt(&self) -> pi_js::Result<&dyn TaskRuntimeBackend> {
        self.erased
            .as_any()
            .downcast_ref::<Arc<dyn TaskRuntimeBackend>>()
            .map(|arc| arc.as_ref())
            .ok_or_else(|| pi_js::Error::msg("Invalid erased task runtime backend"))
    }

    fn rt_arc(&self) -> pi_js::Result<Arc<dyn TaskRuntimeBackend>> {
        self.erased
            .as_any()
            .downcast_ref::<Arc<dyn TaskRuntimeBackend>>()
            .cloned()
            .ok_or_else(|| pi_js::Error::msg("Invalid erased task runtime backend"))
    }

    pub fn task_id(&self) -> pi_js::Result<TaskId<R>> {
        Ok(self.rt()?.task_id().cast::<R>())
    }

    pub fn conversation_id(&self) -> pi_js::Result<ConversationId> {
        Ok(self.rt()?.conversation_id())
    }

    pub fn signal(&self) -> pi_js::Result<pi_js::abort::AbortSignal> {
        Ok(self.rt()?.signal())
    }

    pub fn registry(&self) -> pi_js::Result<Arc<dyn RegistrySnapshot>> {
        Ok(self.rt()?.registry())
    }

    pub async fn agent(&self, context: Context) -> pi_js::Result<Agent> {
        self.rt()?.agent(context).await
    }

    pub fn settings(&self) -> pi_js::Result<Settings> {
        Ok(self.rt()?.settings())
    }

    pub fn models(&self) -> pi_js::Result<Models> {
        Ok(self.rt()?.models().clone())
    }

    pub async fn env(&self, context: Context) -> pi_js::Result<Option<Arc<dyn ExecutionEnv>>> {
        self.rt()?.env(context).await
    }

    pub fn hooks(&self, task_name: &str) -> pi_js::Result<HookRunner<H>> {
        Ok(HookRunner::new(self.rt_arc()?, task_name))
    }

    pub fn read(&self) -> pi_js::Result<DocumentReader> {
        let rt = self.rt_arc()?;
        Ok(DocumentReader::from_backend(rt as Arc<dyn DocumentReaderBackend>))
    }

    pub async fn commit<F, Fut>(&self, change: F, context: Context) -> pi_js::Result<()>
    where
        F: FnOnce(crate::session::transaction::Tx, RunningTask<I, S, R>) -> Fut + Send + 'static,
        Fut: Future<Output = pi_js::Result<Option<NextTaskState<S, R>>>> + Send + 'static,
    {
        let rt = self.rt()?;
        rt.commit_erased(
            Box::new(move |tx, raw_current| {
                Box::pin(async move {
                    let input: I = serde_json::from_value(raw_current.input)
                        .map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
                    let checkpoint: S = serde_json::from_value(raw_current.checkpoint)
                        .map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
                    let current = RunningTask {
                        id: raw_current.id.cast::<R>(),
                        conversation_id: raw_current.conversation_id,
                        kind: raw_current.kind,
                        version: raw_current.version,
                        input,
                        owner: raw_current.owner,
                        background: raw_current.background,
                        abort_requested: raw_current.abort_requested,
                        checkpoint,
                        memos: raw_current.memos,
                    };
                    match change(tx, current).await? {
                        None => Ok(None),
                        Some(next) => Ok(Some(crate::tasks::encode_next_task_state(next)?)),
                    }
                })
            }),
            context,
        )
        .await
    }

    pub async fn memo_get<T: DeserializeOwned>(&self, name: &str, context: Context) -> pi_js::Result<Option<T>> {
        self.rt()?
            .memo_get(name, context)
            .await?
            .map(|v| serde_json::from_value(v).map_err(|err| pi_js::Error::js("TypeError", err.to_string())))
            .transpose()
    }

    pub async fn memo_set<T: Serialize + DeserializeOwned>(
        &self,
        name: &str,
        candidate: &T,
        context: Context,
    ) -> pi_js::Result<T> {
        let encoded =
            serde_json::to_value(candidate).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
        let winner = self.rt()?.memo_set(name, encoded, context).await?;
        serde_json::from_value(winner).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))
    }

    pub async fn get_task<T>(&self, id: TaskId<T>, context: Context) -> pi_js::Result<Option<ErasedTaskRecord>> {
        self.rt()?.get_task(id.erased(), context).await
    }

    pub async fn wait_for_task<T: DeserializeOwned>(
        &self,
        id: TaskId<T>,
        context: Context,
    ) -> pi_js::Result<SettledTask<T>> {
        let raw = self.rt()?.wait_for_task(id.erased(), context).await?;
        let outcome = crate::tasks::decode_outcome::<T>(&raw.outcome)?;
        Ok(SettledTask {
            id: raw.id.cast::<T>(),
            conversation_id: raw.conversation_id,
            kind: raw.kind,
            version: raw.version,
            input: raw.input,
            owner: raw.owner,
            background: raw.background,
            abort_requested: raw.abort_requested,
            outcome,
        })
    }

    pub async fn outcomes<T: DeserializeOwned>(
        &self,
        ids: &[TaskId<T>],
        context: Context,
    ) -> pi_js::Result<Vec<TaskOutcome<T>>> {
        let erased: Vec<TaskId> = ids.iter().map(|id| id.erased()).collect();
        let raw = self.rt()?.outcomes(&erased, context).await?;
        raw.iter().map(crate::tasks::decode_outcome::<T>).collect()
    }

    pub async fn conversation(
        &self,
        id: ConversationId,
        context: Context,
    ) -> pi_js::Result<Option<ConversationHandle>> {
        self.rt()?.conversation(id, context).await
    }

    pub async fn entry(&self, id: EntryId, context: Context) -> pi_js::Result<Option<EntryRecord>> {
        self.rt()?.entry(id, context).await
    }

    pub async fn typed_entry<D: DeserializeOwned>(
        &self,
        token: &crate::entries::Entry<D>,
        id: EntryId,
        context: Context,
    ) -> pi_js::Result<Option<crate::types::TypedEntry<D>>> {
        let record = self.rt()?.entry(id, context).await?;
        token.extract(record.as_ref())
    }

    pub async fn context(
        &self,
        conversation_id: ConversationId,
        context: Context,
        at: Option<EntryId>,
    ) -> pi_js::Result<ContextView> {
        self.rt()?.context_view(conversation_id, context, at).await
    }

    pub fn now(&self) -> pi_js::Result<i64> {
        Ok(self.rt()?.now())
    }

    pub fn report(&self, error: pi_js::Error) {
        if let Ok(rt) = self.rt() {
            rt.report(error);
        }
    }

    pub async fn sleep(&self, until: i64, context: Context) -> pi_js::Result<()> {
        self.rt()?.sleep(until, context).await
    }
}
