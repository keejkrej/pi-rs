//! Port of packages/durable/src/types.ts

use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_ai::types::Message;
use pi_chord::delta::Op;
use pi_chord::services::state_internals::{ReplicatedStateInternals, get_replicated_state_internals};
use pi_chord::{AttachedReplicatedState, Context, ReplicatedState, ReplicatedStateDelivery};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub use crate::ids::{
    ConversationId, ConversationKind, DocumentId, DocumentKind, EntryId, EntryKind, Id, IdKind, Seq, SubmissionId,
    SubmissionKind, TaskId, TaskKind, id_from_number, seq_from_number,
};

/// Re-exported strict JSON value from `pi-chord`.
pub use pi_chord::JsonValue;

/// JSON object used as the root of every durable document.
pub type JsonObject = serde_json::Map<String, JsonValue>;

/// The root conversation always uses this reserved ID.
pub const ROOT_CONVERSATION_ID: ConversationId = ConversationId::new_unchecked(1);

/// Fork initialization mode for a latest-only conversation document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LatestFork {
    Current,
    Initial,
}

/// Fork initialization mode for a rewindable conversation document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RewindableFork {
    AsOf,
    Current,
    Initial,
}

/// Persisted document history retention mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DocumentHistory {
    Latest,
    Rewindable,
}

/// Persisted document fork initialization policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DocumentFork {
    AsOf,
    Current,
    Initial,
}

impl From<LatestFork> for DocumentFork {
    fn from(fork: LatestFork) -> Self {
        match fork {
            LatestFork::Current => DocumentFork::Current,
            LatestFork::Initial => DocumentFork::Initial,
        }
    }
}

impl From<RewindableFork> for DocumentFork {
    fn from(fork: RewindableFork) -> Self {
        match fork {
            RewindableFork::AsOf => DocumentFork::AsOf,
            RewindableFork::Current => DocumentFork::Current,
            RewindableFork::Initial => DocumentFork::Initial,
        }
    }
}

/// Ownership and lifetime of a document; only conversation documents declare history and fork behavior.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DocumentSemantics {
    Session,
    ConversationLatest { fork: LatestFork },
    ConversationRewindable { fork: RewindableFork },
    Task,
}

impl DocumentSemantics {
    pub fn scope_kind(&self) -> DocumentScopeKind {
        match self {
            Self::Session => DocumentScopeKind::Session,
            Self::ConversationLatest { .. } | Self::ConversationRewindable { .. } => DocumentScopeKind::Conversation,
            Self::Task => DocumentScopeKind::Task,
        }
    }

    pub fn history(&self) -> Option<DocumentHistory> {
        match self {
            Self::ConversationLatest { .. } => Some(DocumentHistory::Latest),
            Self::ConversationRewindable { .. } => Some(DocumentHistory::Rewindable),
            Self::Session | Self::Task => None,
        }
    }

    pub fn fork(&self) -> Option<DocumentFork> {
        match self {
            Self::ConversationLatest { fork } => Some((*fork).into()),
            Self::ConversationRewindable { fork } => Some((*fork).into()),
            Self::Session | Self::Task => None,
        }
    }
}

/// Stored replay state supplied to a document's checkpoint predicate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckpointInfo {
    /// Deltas already stored after the newest base, excluding the change being evaluated.
    pub deltas_since_base: usize,
}

/// Scope kind of a document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DocumentScopeKind {
    Session,
    Conversation,
    Task,
}

/// Concrete owner scope of one document address or incarnation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DocumentScope {
    Session,
    #[serde(rename_all = "camelCase")]
    Conversation {
        conversation_id: ConversationId,
    },
    #[serde(rename_all = "camelCase")]
    Task {
        task_id: TaskId,
    },
}

impl DocumentScope {
    pub fn kind(&self) -> DocumentScopeKind {
        match self {
            Self::Session => DocumentScopeKind::Session,
            Self::Conversation { .. } => DocumentScopeKind::Conversation,
            Self::Task { .. } => DocumentScopeKind::Task,
        }
    }

    pub fn conversation_id(&self) -> Option<ConversationId> {
        match self {
            Self::Conversation { conversation_id } => Some(*conversation_id),
            _ => None,
        }
    }

    pub fn task_id(&self) -> Option<TaskId> {
        match self {
            Self::Task { task_id } => Some(*task_id),
            _ => None,
        }
    }
}

/// Who owns a task: its conversation (a top-level task) or another task of the same conversation (a child task).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TaskOwnership {
    Conversation,
    #[serde(rename_all = "camelCase")]
    Task {
        task_id: TaskId,
    },
}

/// How a waiting task treats the tasks it waits on (spec §5.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum JoinPolicy {
    FailFast,
    AllSettled,
}

/// Creation options for a durable task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskOptions {
    /// Required: a task always names its owner (spec §5.5).
    pub ownership: TaskOwnership,
    /// Default: the owner task's conversation, or the transaction's bound conversation; required for
    /// conversation-owned tasks created by Session commits that are not bound to a conversation.
    pub conversation_id: Option<ConversationId>,
    /// Conversation-owned tasks only: excluded from ordinary idle waits, conversation aborts, and cascades.
    pub background: bool,
}

/// Ownership selected explicitly whenever a conversation is created.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ConversationOwnership {
    Ownerless,
    #[serde(rename_all = "camelCase")]
    Task {
        task_id: TaskId,
    },
}

/// Fork source and inclusive parent entry through which history is inherited.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationParent {
    pub conversation_id: ConversationId,
    pub at: EntryId,
}

/// Creator edge used for attribution, subtree abort, and subtree idle waits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationOwner {
    pub conversation_id: ConversationId,
    pub task_id: TaskId,
}

/// Immutable identity, history ancestry, and task ownership of a transcript scope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationRecord {
    pub id: ConversationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<ConversationParent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<ConversationOwner>,
}

/// An immutable override of one visible entry's contribution to model context.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum ContextEdit {
    Omit { target: EntryId },
    Replace { target: EntryId, messages: Vec<Message> },
}

impl ContextEdit {
    pub fn target(&self) -> EntryId {
        match self {
            Self::Omit { target } | Self::Replace { target, .. } => *target,
        }
    }
}

/// Immutable transcript event with separate model-facing and application-facing payloads.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryRecord {
    pub id: EntryId,
    pub conversation_id: ConversationId,
    /// Application-defined entry discriminator.
    pub kind: String,
    /// Messages contributed to model context; absent for display or bookkeeping entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Vec<Message>>,
    /// JSON payload consumed by views, extensions, or bookkeeping logic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<JsonValue>,
    /// First entry in the active context selected by this entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<EntryId>,
    /// Context-only overrides of earlier visible entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edits: Option<Vec<ContextEdit>>,
    /// Task that appended this entry, when it was produced by durable work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by_task_id: Option<TaskId>,
}

/// `head` selector in an entry draft.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EntryDraftHead {
    SelfId(SelfHead),
    Entry(EntryId),
}

/// Sentinel deserializing/serializing as `"self"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelfHead;

impl Serialize for SelfHead {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str("self")
    }
}

impl<'de> Deserialize<'de> for SelfHead {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        if s == "self" {
            Ok(Self)
        } else {
            Err(serde::de::Error::custom(format!("expected \"self\", got {s:?}")))
        }
    }
}

/// Entry content supplied before the Session assigns identity and task attribution.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryDraft {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Vec<Message>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<EntryDraftHead>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edits: Option<Vec<ContextEdit>>,
}

/// Typed entry record whose `data` has been decoded as `D`.
#[derive(Clone, Debug, PartialEq)]
pub struct TypedEntry<D> {
    pub record: EntryRecord,
    pub data: D,
}

/// Typed entry content supplied before the Session assigns identity; the token supplies `kind`.
#[derive(Clone, Debug, PartialEq)]
pub struct TypedEntryDraft<D> {
    pub model: Option<Vec<Message>>,
    pub data: D,
    pub head: Option<EntryDraftHead>,
    pub edits: Option<Vec<ContextEdit>>,
}

/// Lifecycle status of a durable submission.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SubmissionStatus {
    Queued,
    Placed,
    Done,
    Unanswered,
}

/// Submission kind discriminator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SubmissionType {
    Input,
    Write,
}

/// State machine of an input submission.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum InputSubmissionState {
    Queued,
    Placed {
        entry: EntryId,
    },
    Done {
        entry: EntryId,
        answer: EntryId,
    },
    Unanswered {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entry: Option<EntryId>,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<JsonValue>,
    },
}

/// State machine of a passive write submission.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum WriteSubmissionState {
    Queued,
    Done {
        entry: EntryId,
    },
    Unanswered {
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<JsonValue>,
    },
}

/// Combined type + state of one durable submission.
#[derive(Clone, Debug, PartialEq)]
pub enum SubmissionState {
    Input(InputSubmissionState),
    Write(WriteSubmissionState),
}

impl SubmissionState {
    pub fn kind(&self) -> SubmissionType {
        match self {
            Self::Input(_) => SubmissionType::Input,
            Self::Write(_) => SubmissionType::Write,
        }
    }

    pub fn status(&self) -> SubmissionStatus {
        match self {
            Self::Input(InputSubmissionState::Queued) | Self::Write(WriteSubmissionState::Queued) => {
                SubmissionStatus::Queued
            }
            Self::Input(InputSubmissionState::Placed { .. }) => SubmissionStatus::Placed,
            Self::Input(InputSubmissionState::Done { .. }) | Self::Write(WriteSubmissionState::Done { .. }) => {
                SubmissionStatus::Done
            }
            Self::Input(InputSubmissionState::Unanswered { .. })
            | Self::Write(WriteSubmissionState::Unanswered { .. }) => SubmissionStatus::Unanswered,
        }
    }

    pub fn entry(&self) -> Option<EntryId> {
        match self {
            Self::Input(InputSubmissionState::Placed { entry })
            | Self::Input(InputSubmissionState::Done { entry, .. })
            | Self::Write(WriteSubmissionState::Done { entry }) => Some(*entry),
            Self::Input(InputSubmissionState::Unanswered { entry, .. }) => *entry,
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSubmissionFields {
    id: SubmissionId,
    conversation_id: ConversationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request_id: Option<String>,
    #[serde(rename = "type")]
    kind: SubmissionType,
    status: SubmissionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    entry: Option<EntryId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    answer: Option<EntryId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    detail: Option<JsonValue>,
}

/// Durable lifecycle of one admitted user input or passive entry write.
#[derive(Clone, Debug, PartialEq)]
pub struct SubmissionRecord {
    pub id: SubmissionId,
    pub conversation_id: ConversationId,
    /// Host-provided deduplication key, scoped to the conversation.
    pub request_id: Option<String>,
    pub state: SubmissionState,
}

impl SubmissionRecord {
    pub fn kind(&self) -> SubmissionType {
        self.state.kind()
    }

    pub fn status(&self) -> SubmissionStatus {
        self.state.status()
    }

    pub fn is_settled(&self) -> bool {
        matches!(self.status(), SubmissionStatus::Done | SubmissionStatus::Unanswered)
    }
}

impl Serialize for SubmissionRecord {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let raw = RawSubmissionFields {
            id: self.id,
            conversation_id: self.conversation_id,
            request_id: self.request_id.clone(),
            kind: self.kind(),
            status: self.status(),
            entry: self.state.entry(),
            answer: match &self.state {
                SubmissionState::Input(InputSubmissionState::Done { answer, .. }) => Some(*answer),
                _ => None,
            },
            reason: match &self.state {
                SubmissionState::Input(InputSubmissionState::Unanswered { reason, .. })
                | SubmissionState::Write(WriteSubmissionState::Unanswered { reason, .. }) => Some(reason.clone()),
                _ => None,
            },
            detail: match &self.state {
                SubmissionState::Input(InputSubmissionState::Unanswered { detail, .. })
                | SubmissionState::Write(WriteSubmissionState::Unanswered { detail, .. }) => detail.clone(),
                _ => None,
            },
        };
        raw.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SubmissionRecord {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawSubmissionFields::deserialize(deserializer)?;
        let state = match (raw.kind, raw.status) {
            (SubmissionType::Input, SubmissionStatus::Queued) => {
                if raw.entry.is_some() || raw.answer.is_some() || raw.reason.is_some() || raw.detail.is_some() {
                    return Err(serde::de::Error::custom(
                        "queued input submission cannot carry settled fields",
                    ));
                }
                SubmissionState::Input(InputSubmissionState::Queued)
            }
            (SubmissionType::Input, SubmissionStatus::Placed) => {
                let entry = raw
                    .entry
                    .ok_or_else(|| serde::de::Error::custom("placed input requires entry"))?;
                if raw.answer.is_some() || raw.reason.is_some() || raw.detail.is_some() {
                    return Err(serde::de::Error::custom("placed input cannot carry answer/reason"));
                }
                SubmissionState::Input(InputSubmissionState::Placed { entry })
            }
            (SubmissionType::Input, SubmissionStatus::Done) => {
                let entry = raw
                    .entry
                    .ok_or_else(|| serde::de::Error::custom("done input requires entry"))?;
                let answer = raw
                    .answer
                    .ok_or_else(|| serde::de::Error::custom("done input requires answer"))?;
                if raw.reason.is_some() || raw.detail.is_some() {
                    return Err(serde::de::Error::custom("done input cannot carry reason/detail"));
                }
                SubmissionState::Input(InputSubmissionState::Done { entry, answer })
            }
            (SubmissionType::Input, SubmissionStatus::Unanswered) => {
                if raw.answer.is_some() {
                    return Err(serde::de::Error::custom("unanswered input cannot carry answer"));
                }
                let reason = raw
                    .reason
                    .ok_or_else(|| serde::de::Error::custom("unanswered input requires reason"))?;
                SubmissionState::Input(InputSubmissionState::Unanswered {
                    entry: raw.entry,
                    reason,
                    detail: raw.detail,
                })
            }
            (SubmissionType::Write, SubmissionStatus::Queued) => {
                if raw.entry.is_some() || raw.answer.is_some() || raw.reason.is_some() || raw.detail.is_some() {
                    return Err(serde::de::Error::custom("queued write cannot carry settled fields"));
                }
                SubmissionState::Write(WriteSubmissionState::Queued)
            }
            (SubmissionType::Write, SubmissionStatus::Placed) => {
                return Err(serde::de::Error::custom("write submission cannot be placed"));
            }
            (SubmissionType::Write, SubmissionStatus::Done) => {
                let entry = raw
                    .entry
                    .ok_or_else(|| serde::de::Error::custom("done write requires entry"))?;
                if raw.answer.is_some() || raw.reason.is_some() || raw.detail.is_some() {
                    return Err(serde::de::Error::custom("done write cannot carry answer/reason/detail"));
                }
                SubmissionState::Write(WriteSubmissionState::Done { entry })
            }
            (SubmissionType::Write, SubmissionStatus::Unanswered) => {
                if raw.entry.is_some() || raw.answer.is_some() {
                    return Err(serde::de::Error::custom("unanswered write cannot carry entry/answer"));
                }
                let reason = raw
                    .reason
                    .ok_or_else(|| serde::de::Error::custom("unanswered write requires reason"))?;
                SubmissionState::Write(WriteSubmissionState::Unanswered {
                    reason,
                    detail: raw.detail,
                })
            }
        };
        Ok(Self {
            id: raw.id,
            conversation_id: raw.conversation_id,
            request_id: raw.request_id,
            state,
        })
    }
}

/// Terminal status staged for a submission; identity, type, and entry come from its current record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum SubmissionSettlement {
    Done {
        answer: EntryId,
    },
    Unanswered {
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<JsonValue>,
    },
}

/// Submission fields supplied before the Session assigns an ID.
#[derive(Clone, Debug, PartialEq)]
pub struct SubmissionCreate {
    pub conversation_id: ConversationId,
    pub request_id: Option<String>,
    pub state: SubmissionState,
}

/// JSON-safe error snapshot persisted instead of a runtime `Error` object.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskOutcomeError {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<JsonValue>,
}

/// Durable reason and optional result recorded when a task becomes terminal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum TaskOutcome<R = JsonValue> {
    Completed {
        result: R,
    },
    /// Expected task or domain failure explicitly committed by its implementation.
    Failed {
        error: TaskOutcomeError,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<R>,
    },
    /// Explicit cancellation handled by the task's abort protocol.
    Aborted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<R>,
    },
    /// Task that cannot resume because its definition or migration is unavailable.
    Orphaned {
        reason: String,
    },
    /// Runtime-detected contract failure, such as an uncaught throw or no durable progress.
    Faulted {
        error: TaskOutcomeError,
    },
}

impl<R> TaskOutcome<R> {
    pub fn status_str(&self) -> &'static str {
        match self {
            Self::Completed { .. } => "completed",
            Self::Failed { .. } => "failed",
            Self::Aborted { .. } => "aborted",
            Self::Orphaned { .. } => "orphaned",
            Self::Faulted { .. } => "faulted",
        }
    }
}

/// Execution status discriminator for [`TaskState`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskStatus {
    Pending,
    Running,
    Waiting,
    Completing,
    Terminal,
}

/// Complete durable execution state of a task.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum TaskState<S = JsonValue, R = JsonValue> {
    /// Eligible for scheduling.
    Pending { checkpoint: S },
    /// Reserved by one in-memory task invocation.
    Running { checkpoint: S },
    /// Parked without an invocation until every task in `on` is terminal; then resumes at `checkpoint`.
    Waiting {
        checkpoint: S,
        on: Vec<TaskId>,
        policy: JoinPolicy,
    },
    /// Outcome decided; becomes terminal once no ordinary owned work below is live. Runs no more code.
    Completing { outcome: TaskOutcome<R> },
    /// Permanently settled durable result receipt.
    Terminal { outcome: TaskOutcome<R> },
}

impl<S, R> TaskState<S, R> {
    pub fn status(&self) -> TaskStatus {
        match self {
            Self::Pending { .. } => TaskStatus::Pending,
            Self::Running { .. } => TaskStatus::Running,
            Self::Waiting { .. } => TaskStatus::Waiting,
            Self::Completing { .. } => TaskStatus::Completing,
            Self::Terminal { .. } => TaskStatus::Terminal,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Terminal { .. })
    }

    pub fn is_live(&self) -> bool {
        !self.is_terminal()
    }

    pub fn checkpoint(&self) -> Option<&S> {
        match self {
            Self::Pending { checkpoint } | Self::Running { checkpoint } | Self::Waiting { checkpoint, .. } => {
                Some(checkpoint)
            }
            Self::Completing { .. } | Self::Terminal { .. } => None,
        }
    }

    pub fn outcome(&self) -> Option<&TaskOutcome<R>> {
        match self {
            Self::Completing { outcome } | Self::Terminal { outcome } => Some(outcome),
            _ => None,
        }
    }
}

/// Complete replacement record for one durable task state machine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRecord<I = JsonValue, S = JsonValue, R = JsonValue> {
    pub id: TaskId<R>,
    pub conversation_id: ConversationId,
    /// Registered task definition name.
    pub kind: String,
    /// Definition version used to migrate live input and checkpoints.
    pub version: i64,
    /// Original task input retained while the task is live or terminal.
    pub input: I,
    /// Owning task of a child task; absent for a task its conversation owns. Immutable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<TaskId>,
    /// Whether this conversation-owned task is excluded from ordinary idle waits, conversation aborts, and cascades.
    pub background: bool,
    /// Durable abort mark checked before run-mode progress is committed.
    pub abort_requested: bool,
    pub state: TaskState<S, R>,
    /// Small first-writer-wins values retained while the task can run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memos: Option<IndexMap<String, JsonValue>>,
}

/// Erased task record stored in storage and commits.
pub type ErasedTaskRecord = TaskRecord<JsonValue, JsonValue, JsonValue>;

/// Live task record reserved by one invocation.
#[derive(Clone, Debug, PartialEq)]
pub struct RunningTask<I = JsonValue, S = JsonValue, R = JsonValue> {
    pub id: TaskId<R>,
    pub conversation_id: ConversationId,
    pub kind: String,
    pub version: i64,
    pub input: I,
    pub owner: Option<TaskId>,
    pub background: bool,
    pub abort_requested: bool,
    pub checkpoint: S,
    pub memos: Option<IndexMap<String, JsonValue>>,
}

/// Next state a task commits for itself: a replacement checkpoint, a wait, or its outcome.
#[derive(Clone, Debug, PartialEq)]
pub enum NextTaskState<S, R> {
    Running {
        checkpoint: S,
    },
    Waiting {
        checkpoint: S,
        on: Vec<TaskId>,
        policy: JoinPolicy,
    },
    Terminal {
        outcome: TaskOutcome<R>,
    },
}

/// Persisted lifecycle record for one create-to-retire document incarnation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentRecord {
    pub id: DocumentId,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub created_at: Seq,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retired_at: Option<Seq>,
    pub scope: DocumentScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<DocumentHistory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork: Option<DocumentFork>,
}

/// Fields supplied when storage creates and stamps a new `DocumentRecord`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentCreate {
    pub id: DocumentId,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub scope: DocumentScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<DocumentHistory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork: Option<DocumentFork>,
}

impl DocumentCreate {
    pub fn as_address(&self) -> DocumentAddress {
        DocumentAddress {
            kind: self.kind.clone(),
            scope: self.scope,
            key: self.key.clone(),
        }
    }

    pub fn into_record(self, created_at: Seq) -> DocumentRecord {
        DocumentRecord {
            id: self.id,
            kind: self.kind,
            key: self.key,
            created_at,
            retired_at: None,
            scope: self.scope,
            history: self.history,
            fork: self.fork,
        }
    }
}

/// One ordered scan result and its optional continuation state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page<T, C = Cursor> {
    pub items: Vec<T>,
    pub next: Option<C>,
}

/// Backend-owned JSON continuation state that callers only round-trip to the same scan.
pub type Cursor = JsonObject;

/// Optional filters for an ordered conversation scan.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConversationQuery {
    pub owner_conversation_id: Option<ConversationId>,
    pub owner_task_id: Option<TaskId>,
}

/// Inclusive ID bounds for a newest-first scan of one conversation's fork-aware history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryQuery {
    pub conversation_id: ConversationId,
    pub min_entry_id: Option<EntryId>,
    pub max_entry_id: Option<EntryId>,
}

/// Optional filters for an ordered scan of durable task records.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskQuery {
    pub conversation_id: Option<ConversationId>,
    pub kind: Option<String>,
    pub status: Option<TaskStatus>,
    pub abort_requested: Option<bool>,
    pub background: Option<bool>,
}

/// Optional filters for an ordered scan of submission records.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SubmissionQuery {
    pub conversation_id: Option<ConversationId>,
    pub status: Option<SubmissionStatus>,
}

/// Current state or one historical commit sequence used for document membership and content reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DocumentPoint {
    Current,
    At(Seq),
}

/// Exact logical identity of a singleton or one keyed family member.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DocumentAddress {
    pub kind: String,
    pub scope: DocumentScope,
    pub key: Option<String>,
}

/// Ordered scan of document incarnations alive in one exact scope at one point.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentQuery {
    pub scope: DocumentScope,
    pub at: DocumentPoint,
    pub kind: Option<String>,
}

/// Complete checkpoint or Chord operation batch selected by the owning Session.
#[derive(Clone, Debug, PartialEq)]
pub enum DocumentContent {
    Base { version: i64, value: JsonObject },
    Delta { version: i64, ops: Vec<Op> },
}

/// Exact persisted source selected for a definition-free document copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DocumentCopySource {
    pub id: DocumentId,
    pub at: DocumentPoint,
}

/// Detached materialized value and stored definition version at a selected point.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredDocument {
    pub record: DocumentRecord,
    pub version: i64,
    pub value: JsonObject,
    pub deltas_since_base: usize,
}

/// One record or document mutation in an atomic storage commit.
#[derive(Clone, Debug, PartialEq)]
pub enum StorageWrite {
    Conversation {
        value: ConversationRecord,
    },
    Entry {
        value: EntryRecord,
    },
    Task {
        value: ErasedTaskRecord,
    },
    Submission {
        value: SubmissionRecord,
    },
    DocumentCreate {
        record: DocumentCreate,
        version: i64,
        value: JsonObject,
    },
    DocumentCopy {
        record: DocumentCreate,
        source: DocumentCopySource,
    },
    DocumentChange {
        id: DocumentId,
        content: DocumentContent,
    },
    DocumentRetire {
        id: DocumentId,
    },
}

/// Committed change of one document incarnation.
#[derive(Clone, Debug, PartialEq)]
pub enum DocumentCommitChange {
    Document {
        record: DocumentRecord,
        conversation_id: Option<ConversationId>,
        version: Option<i64>,
        value: Option<JsonObject>,
        ops: Vec<Op>,
    },
    DocumentCopy {
        record: DocumentRecord,
        conversation_id: ConversationId,
        source: DocumentCopySource,
    },
}

/// Complete table record committed without another publication copy.
#[derive(Clone, Debug, PartialEq)]
pub enum TableCommitChange {
    Conversation(ConversationRecord),
    Entry(EntryRecord),
    Task(ErasedTaskRecord),
    Submission(SubmissionRecord),
}

/// One immutable change from a Session commit.
#[derive(Clone, Debug, PartialEq)]
pub enum CommitChange {
    Table(TableCommitChange),
    Document(DocumentCommitChange),
}

/// Every immutable change from one successful Session commit. Change order is unspecified.
#[derive(Clone, Debug, PartialEq)]
pub struct CommitPublication {
    pub seq: Seq,
    pub changes: Vec<CommitChange>,
}

/// Entry returned alongside the commit sequence that persisted it.
#[derive(Clone, Debug, PartialEq)]
pub struct EntryLookup {
    pub entry: EntryRecord,
    pub commit_seq: Seq,
}

/// Terminal result of one document or view watch.
#[derive(Clone, Debug)]
pub enum WatchEnd {
    Stopped,
    Cancelled,
    SessionClosed,
    Retired,
    ListenerError { error: Arc<pi_js::Error> },
}

impl PartialEq for WatchEnd {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Stopped, Self::Stopped)
            | (Self::Cancelled, Self::Cancelled)
            | (Self::SessionClosed, Self::SessionClosed)
            | (Self::Retired, Self::Retired) => true,
            (Self::ListenerError { error: a }, Self::ListenerError { error: b }) => a.to_string() == b.to_string(),
            _ => false,
        }
    }
}

/// Async listener callback installed on a [`WatchHandle`].
pub type WatchListener<T> =
    Arc<dyn Fn(T, Vec<Op>, Context) -> Pin<Box<dyn Future<Output = pi_js::Result<()>> + Send>> + Send + Sync>;

/// Abstract watch handle interface used by typed watch wrappers.
#[async_trait]
pub trait WatchHandleApi<T: Send + Sync + 'static>: Send + Sync {
    fn value(&self) -> T;
    fn start(&self, listener: WatchListener<T>) -> pi_js::Result<()>;
    async fn stop(&self) -> WatchEnd;
    async fn closed(&self) -> WatchEnd;
}

/// Serialized exact-frame observation of an immutable value with bounded pending delivery.
#[derive(Clone)]
pub struct WatchHandle<T> {
    inner: Arc<dyn WatchHandleApi<T>>,
}

impl<T: Send + Sync + 'static> WatchHandle<T> {
    pub fn from_inner(inner: Arc<dyn WatchHandleApi<T>>) -> Self {
        Self { inner }
    }

    pub fn value(&self) -> T {
        self.inner.value()
    }

    pub fn start(&self, listener: WatchListener<T>) -> pi_js::Result<()> {
        self.inner.start(listener)
    }

    pub async fn stop(&self) -> WatchEnd {
        self.inner.stop().await
    }

    pub async fn closed(&self) -> WatchEnd {
        self.inner.closed().await
    }
}

pub type DocumentWatch<T> = WatchHandle<Option<T>>;

fn decode_document_state_value<T: DeserializeOwned>(value: JsonValue) -> pi_js::Result<Option<T>> {
    match value {
        JsonValue::Null => Ok(None),
        other => serde_json::from_value::<T>(other)
            .map(Some)
            .map_err(|err| pi_js::Error::js("TypeError", err.to_string())),
    }
}

/// Disposable read-only Chord state bound to one committed document incarnation (or `None` when retired).
pub struct DocumentState<T> {
    raw: AttachedReplicatedState<JsonValue>,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Clone for DocumentState<T> {
    fn clone(&self) -> Self {
        Self {
            raw: self.raw.clone(),
            _marker: PhantomData,
        }
    }
}

impl<T> DocumentState<T> {
    pub fn from_raw(raw: AttachedReplicatedState<JsonValue>) -> Self {
        Self {
            raw,
            _marker: PhantomData,
        }
    }

    pub fn as_raw(&self) -> &AttachedReplicatedState<JsonValue> {
        &self.raw
    }

    pub fn into_raw(self) -> AttachedReplicatedState<JsonValue> {
        self.raw
    }

    pub fn as_replicated(&self) -> ReplicatedState<JsonValue> {
        self.raw.as_replicated()
    }

    pub fn internals(&self) -> Option<Arc<dyn ReplicatedStateInternals>> {
        get_replicated_state_internals(&self.raw)
    }

    pub fn subscribe_json<F>(&self, listener: F) -> pi_js::Unsubscribe
    where
        F: Fn(JsonValue, Context, ReplicatedStateDelivery) -> pi_js::BoxFuture<pi_js::Result<()>>
            + Send
            + Sync
            + 'static,
    {
        self.raw.subscribe(Arc::new(listener))
    }

    pub fn dispose(&self) -> pi_js::Result<()> {
        self.raw.dispose()
    }
}

impl<T: DeserializeOwned> DocumentState<T> {
    pub fn value(&self) -> Option<T> {
        decode_document_state_value::<T>(self.raw.value()).ok().flatten()
    }

    pub fn try_value(&self) -> pi_js::Result<Option<T>> {
        decode_document_state_value::<T>(self.raw.value())
    }

    pub fn subscribe<F>(&self, listener: F) -> pi_js::Unsubscribe
    where
        F: Fn(Option<T>, Context, ReplicatedStateDelivery) -> pi_js::BoxFuture<pi_js::Result<()>>
            + Send
            + Sync
            + 'static,
    {
        let listener = Arc::new(listener);
        self.raw.subscribe(Arc::new(move |json, ctx, delivery| {
            let listener = Arc::clone(&listener);
            Box::pin(async move {
                let typed = decode_document_state_value::<T>(json)?;
                listener(typed, ctx, delivery).await
            })
        }))
    }
}

/// Atomic persistence boundary for Session records.
#[async_trait]
pub trait Storage: Send + Sync {
    async fn commit(&self, writes: Vec<StorageWrite>, context: Context) -> pi_js::Result<Seq>;

    async fn mint_id(&self) -> pi_js::Result<i64>;

    async fn conversation(&self, id: ConversationId, context: Context) -> pi_js::Result<Option<ConversationRecord>>;

    async fn scan_conversations(
        &self,
        query: ConversationQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<ConversationRecord, Cursor>>;

    async fn entry(&self, id: EntryId, context: Context) -> pi_js::Result<Option<EntryLookup>>;

    async fn entry_in_conversation(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
        context: Context,
    ) -> pi_js::Result<Option<EntryLookup>>;

    async fn find_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
        context: Context,
    ) -> pi_js::Result<Option<EntryRecord>>;

    async fn scan_entries(
        &self,
        query: EntryQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<EntryRecord, Cursor>>;

    async fn task(&self, id: TaskId, context: Context) -> pi_js::Result<Option<ErasedTaskRecord>>;

    async fn scan_tasks(
        &self,
        query: TaskQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<ErasedTaskRecord, Cursor>>;

    async fn submission(&self, id: SubmissionId, context: Context) -> pi_js::Result<Option<SubmissionRecord>>;

    async fn scan_submissions(
        &self,
        query: SubmissionQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<SubmissionRecord, Cursor>>;

    async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
        context: Context,
    ) -> pi_js::Result<Option<SubmissionRecord>>;

    async fn find_document(
        &self,
        address: DocumentAddress,
        at: DocumentPoint,
        context: Context,
    ) -> pi_js::Result<Option<DocumentRecord>>;

    async fn document(
        &self,
        id: DocumentId,
        at: DocumentPoint,
        context: Context,
    ) -> pi_js::Result<Option<StoredDocument>>;

    async fn scan_documents(
        &self,
        query: DocumentQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<DocumentRecord, Cursor>>;

    async fn close(&self, context: Context) -> pi_js::Result<()>;
}

/// Helper to mint a strongly typed ID from any [`Storage`] implementation.
pub async fn mint_typed_id<K: IdKind, R>(storage: &dyn Storage) -> pi_js::Result<Id<K, R>> {
    let raw = storage.mint_id().await?;
    Ok(Id::new_unchecked(raw))
}
