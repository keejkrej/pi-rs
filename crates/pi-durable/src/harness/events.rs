//! Port of packages/durable/src/harness/events.ts

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use indexmap::{IndexMap, IndexSet};
use pi_ai::types::{AssistantContentBlock, AssistantMessage, Message, Usage};
use pi_chord::delta::{Op, Path, Seg};
use pi_chord::{Context, JsonValue};
use pi_js::abort::AbortReason;
use serde::{Deserialize, Serialize};

use crate::harness::inbox::{InboxItem, InboxState};
use crate::harness::live::{CompactionStatus, DurableRetryState, LiveDeferred, LiveState, ToolSlot, ToolSlotStatus};
use crate::harness::types::{AgentState, CompactionReason, ToolDiagnostic};
use crate::harness::usage::UsageState;
use crate::harness::util::scan_all;
use crate::harness::view::{ConversationView, ConversationViews, ViewObserver};
use crate::session::observation::CommittedWatch;
use crate::types::{
    CommitChange, CommitPublication, ConversationId, EntryRecord, ErasedTaskRecord, JsonObject, SubmissionId,
    SubmissionRecord, TableCommitChange, TaskId, TaskOutcome, TaskQuery, TaskStatus, WatchEnd,
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn abort_error(signal: &pi_js::abort::AbortSignal) -> pi_js::Error {
    match signal.reason() {
        Some(reason) if reason.value.is_none() => pi_js::Error::Abort(reason),
        _ => pi_js::Error::Abort(AbortReason::error("AbortError", "The operation was aborted")),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InboxItemMode {
    Steer,
    FollowUp,
    Write,
}

impl From<&InboxItem> for InboxItemMode {
    fn from(item: &InboxItem) -> Self {
        match item {
            InboxItem::Steer { .. } => Self::Steer,
            InboxItem::FollowUp { .. } => Self::FollowUp,
            InboxItem::Write { .. } => Self::Write,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedItem {
    pub id: SubmissionId,
    pub mode: InboxItemMode,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotRun {
    pub inputs: Vec<SubmissionId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotGeneration {
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<AssistantMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<DurableRetryState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred: Option<LiveDeferred>,
}

/// One change to the in-flight assistant message, relative to that message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageChange {
    #[serde(rename_all = "camelCase")]
    TextStart {
        content_index: usize,
        block: AssistantContentBlock,
    },
    #[serde(rename_all = "camelCase")]
    ThinkingStart {
        content_index: usize,
        block: AssistantContentBlock,
    },
    #[serde(rename_all = "camelCase")]
    ToolcallStart {
        content_index: usize,
        block: AssistantContentBlock,
    },
    #[serde(rename_all = "camelCase")]
    TextDelta {
        content_index: usize,
        delta: String,
    },
    #[serde(rename_all = "camelCase")]
    ThinkingDelta {
        content_index: usize,
        delta: String,
    },
    #[serde(rename_all = "camelCase")]
    ToolcallDelta {
        content_index: usize,
        path: Vec<Seg>,
        delta: String,
    },
    #[serde(rename_all = "camelCase")]
    Block {
        content_index: usize,
        block: AssistantContentBlock,
    },
    Message {
        message: AssistantMessage,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotEvent {
    pub entries: Vec<EntryRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<SnapshotRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<SnapshotGeneration>,
    pub tools: Vec<ToolSlot>,
    pub compactions: Vec<CompactionStatus>,
    pub inbox: Vec<QueuedItem>,
    pub agent: AgentState,
    pub usage: UsageState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolOutputUpdate {
    #[serde(rename_all = "camelCase")]
    Delta {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trim_start: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        append: Option<String>,
    },
    Set {
        set: String,
    },
}

/// Experimental agent event, shaped like the coding agent's session events (spec §9.4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    Snapshot(SnapshotEvent),
    RunStart {
        inputs: Vec<SubmissionId>,
    },
    RunEnd {
        inputs: Vec<SubmissionId>,
    },
    TurnStart,
    TurnEnd,
    MessageStart {
        message: Message,
    },
    MessageUpdate {
        usage: Usage,
        changes: Vec<MessageChange>,
    },
    MessageEnd {
        entry: EntryRecord,
    },
    #[serde(rename_all = "camelCase")]
    ToolExecutionStart {
        tool_call_id: String,
        tool_name: String,
        args: JsonObject,
    },
    #[serde(rename_all = "camelCase")]
    ToolExecutionUpdate {
        tool_call_id: String,
        tool_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<ToolOutputUpdate>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<JsonValue>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diagnostics: Option<Vec<ToolDiagnostic>>,
    },
    #[serde(rename_all = "camelCase")]
    ToolExecutionEnd {
        tool_call_id: String,
        tool_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entry: Option<EntryRecord>,
    },
    InboxUpdate {
        items: Vec<QueuedItem>,
    },
    Submission {
        record: SubmissionRecord,
    },
    #[serde(rename_all = "camelCase")]
    AutoRetryStart {
        attempt: u32,
        at: i64,
        error_message: String,
    },
    AutoRetryEnd {
        attempt: u32,
    },
    #[serde(rename_all = "camelCase")]
    DeferredPoll {
        poll_at: i64,
    },
    EntryAppended {
        entry: EntryRecord,
    },
    AgentChanged {
        agent: AgentState,
    },
    UsageChanged {
        usage: UsageState,
    },
    #[serde(rename_all = "camelCase")]
    TaskFailed {
        task_id: TaskId,
        kind: String,
        message: String,
    },
    #[serde(rename_all = "camelCase")]
    CompactionStart {
        task_id: TaskId<crate::harness::types::CompactionResult>,
        reason: CompactionReason,
        blocking: bool,
    },
    #[serde(rename_all = "camelCase")]
    CompactionEnd {
        task_id: TaskId<crate::harness::types::CompactionResult>,
        reason: CompactionReason,
    },
}

/// Serialized stream of one conversation's event batches, one per commit.
#[derive(Clone)]
pub struct AgentEventStream {
    pub snapshot: SnapshotEvent,
    watch: Arc<CommittedWatch<Vec<AgentEvent>>>,
}

impl AgentEventStream {
    pub fn start<F>(&self, listener: F) -> pi_js::Result<()>
    where
        F: Fn(Vec<AgentEvent>, Context) -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync + 'static,
    {
        self.watch
            .start(Arc::new(move |events, _ops, ctx| listener(events, ctx)))
    }

    pub async fn stop(&self) -> WatchEnd {
        self.watch.stop().await
    }

    pub async fn closed(&self) -> WatchEnd {
        self.watch.closed().await
    }
}

struct ViewParts {
    live: LiveState,
    inbox: Option<InboxState>,
    agent: Option<AgentState>,
    usage: Option<UsageState>,
}

fn parts(view: &ConversationView) -> ViewParts {
    let decode = |kind: &str| -> Option<JsonValue> { view.docs.get(kind).map(|obj| JsonValue::Object(obj.clone())) };
    ViewParts {
        live: decode("pi.live")
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default(),
        inbox: decode("pi.inbox").and_then(|v| serde_json::from_value(v).ok()),
        agent: decode("pi.agent").and_then(|v| serde_json::from_value(v).ok()),
        usage: decode("pi.usage").and_then(|v| serde_json::from_value(v).ok()),
    }
}

fn queued(inbox: Option<&InboxState>) -> Vec<QueuedItem> {
    let Some(state) = inbox else {
        return Vec::new();
    };
    state
        .items
        .iter()
        .map(|item| {
            let id = match item {
                InboxItem::Steer { id, .. } | InboxItem::FollowUp { id, .. } | InboxItem::Write { id, .. } => *id,
            };
            QueuedItem {
                id,
                mode: InboxItemMode::from(item),
            }
        })
        .collect()
}

fn snapshot_of(view: &ConversationView) -> SnapshotEvent {
    let p = parts(view);
    SnapshotEvent {
        entries: view.entries.clone(),
        run: p.live.run.map(|r| SnapshotRun { inputs: r.inputs }),
        generation: p.live.generation.map(|g| SnapshotGeneration {
            attempt: g.attempt,
            message: g.message,
            retry: g.retry,
            deferred: g.deferred,
        }),
        tools: p.live.tools.unwrap_or_default(),
        compactions: p.live.compactions.unwrap_or_default(),
        inbox: queued(p.inbox.as_ref()),
        agent: p.agent.unwrap_or_default(),
        usage: p.usage.unwrap_or_default(),
    }
}

struct EventObserver {
    conversation_id: ConversationId,
    watch: Arc<CommittedWatch<Vec<AgentEvent>>>,
    current: Arc<Mutex<ConversationView>>,
    held: Mutex<IndexSet<TaskId>>,
}

impl ViewObserver for EventObserver {
    fn publication(
        &self,
        before: &ConversationView,
        after: &ConversationView,
        ops: &[Op],
        publication: &CommitPublication,
        context: &Context,
    ) {
        *lock(&self.current) = after.clone();
        let mut held = lock(&self.held);
        let events = translate(self.conversation_id, before, after, ops, publication, &mut held);
        drop(held);
        if !events.is_empty() {
            self.watch.advance(events, Vec::new(), context.clone());
        }
    }

    fn close_session(&self) {
        self.watch.close_session();
    }
}

/// Experimental: attach to one conversation's agent events (spec §9.4). The snapshot and the
/// registration for later commits are captured atomically on the Session line; overflow replaces
/// undelivered batches with one snapshot.
pub async fn watch_events_on_views(
    views: &Arc<ConversationViews>,
    conversation_id: ConversationId,
    context: Context,
) -> pi_js::Result<AgentEventStream> {
    let ctx_for_attach = context.clone();
    let ((watch, snapshot), _detach) = views
        .attach(
            conversation_id,
            move |initial, release, storage| async move {
                let completing = scan_all(|cursor| {
                    let storage = Arc::clone(&storage);
                    let ctx = ctx_for_attach.clone();
                    async move {
                        storage
                            .scan_tasks(
                                TaskQuery {
                                    conversation_id: Some(conversation_id),
                                    kind: Some("pi.generation".to_owned()),
                                    status: Some(TaskStatus::Completing),
                                    ..Default::default()
                                },
                                100,
                                cursor,
                                ctx,
                            )
                            .await
                    }
                })
                .await?;
                let held: IndexSet<TaskId> = completing.into_iter().map(|r| r.id).collect();
                let snapshot = snapshot_of(&initial);
                let current = Arc::new(Mutex::new(initial));
                let current_for_replace = Arc::clone(&current);
                let replace: Box<dyn Fn() -> Vec<AgentEvent> + Send + Sync> = Box::new(move || {
                    let snap = snapshot_of(&lock(&current_for_replace));
                    vec![AgentEvent::Snapshot(snap)]
                });
                let watch = CommittedWatch::new_arc(
                    Vec::new(),
                    |_| false,
                    |v| serde_json::to_value(v).unwrap_or(JsonValue::Null),
                    release,
                    Some(replace),
                );
                let observer: Arc<dyn ViewObserver> = Arc::new(EventObserver {
                    conversation_id,
                    watch: Arc::clone(&watch),
                    current,
                    held: Mutex::new(held),
                });
                Ok((observer, (watch, snapshot)))
            },
            context.clone(),
        )
        .await?;

    if let Some(signal) = context.abort_signal() {
        if signal.aborted() {
            watch.cancel();
            return Err(abort_error(&signal));
        }
        watch.observe_cancellation(&signal)?;
    }

    Ok(AgentEventStream { snapshot, watch })
}

/// The tool result for `call_id` among `entries`.
fn result_of<'a>(entries: &'a [EntryRecord], call_id: &str) -> Option<&'a EntryRecord> {
    entries.iter().find(|entry| {
        matches!(
            entry.model.as_deref().and_then(|m| m.first()),
            Some(Message::ToolResult(tr)) if tr.tool_call_id == call_id
        )
    })
}

fn doc_changed(view_ops: &[Op], kind: &str, before: &ConversationView, after: &ConversationView) -> bool {
    let prefix = [Seg::String("docs".to_owned()), Seg::String(kind.to_owned())];
    view_ops.iter().any(|op| {
        let Some(path) = op_path(op) else {
            return true;
        };
        starts_with(path, &prefix) || starts_with(&prefix, path)
    }) || before.docs.get(kind) != after.docs.get(kind)
}

/// Every event one publication causes, in the order of spec §9.4.
fn translate(
    conversation_id: ConversationId,
    before: &ConversationView,
    after: &ConversationView,
    view_ops: &[Op],
    publication: &CommitPublication,
    held: &mut IndexSet<TaskId>,
) -> Vec<AgentEvent> {
    let mut entries: Vec<&EntryRecord> = Vec::new();
    let mut tasks: IndexMap<TaskId, &ErasedTaskRecord> = IndexMap::new();
    let mut submissions: Vec<&SubmissionRecord> = Vec::new();

    for change in &publication.changes {
        if let CommitChange::Table(table_change) = change {
            match table_change {
                TableCommitChange::Entry(e) if e.conversation_id == conversation_id => entries.push(e),
                TableCommitChange::Task(t) if t.conversation_id == conversation_id => {
                    tasks.insert(t.id, t);
                }
                TableCommitChange::Submission(s) if s.conversation_id == conversation_id => {
                    submissions.push(s);
                }
                _ => {}
            }
        }
    }

    if view_ops.is_empty() && entries.is_empty() && tasks.is_empty() && submissions.is_empty() {
        return Vec::new();
    }

    submissions.sort_by_key(|s| s.id);
    let was = parts(before);
    let now = parts(after);
    let mut events: Vec<AgentEvent> = Vec::new();

    let slots_before: IndexMap<&str, &ToolSlot> = was
        .live
        .tools
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|slot| (slot.call_id.as_str(), slot))
        .collect();
    let slots = now.live.tools.as_deref().unwrap_or(&[]);

    for slot in slots {
        if slot.status != ToolSlotStatus::Running
            || slots_before
                .get(slot.call_id.as_str())
                .is_some_and(|s| s.status == ToolSlotStatus::Running)
        {
            continue;
        }
        let args = slot
            .task_id
            .and_then(|id| tasks.get(&id).copied())
            .and_then(|t| t.state.checkpoint())
            .and_then(|cp| cp.get("arguments"))
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        events.push(AgentEvent::ToolExecutionStart {
            tool_call_id: slot.call_id.clone(),
            tool_name: slot.name.clone(),
            args,
        });
    }

    let partial_before = was.live.generation.as_ref().and_then(|g| g.message.as_ref());
    let partial = now.live.generation.as_ref().and_then(|g| g.message.as_ref());
    if let Some(partial_msg) = partial {
        if partial_before.is_none() {
            events.push(AgentEvent::MessageStart {
                message: Message::Assistant(partial_msg.clone()),
            });
        } else if Some(partial_msg) != partial_before {
            events.push(AgentEvent::MessageUpdate {
                usage: partial_msg.usage,
                changes: message_changes(view_ops, partial_msg),
            });
        }
    }

    for (index, slot) in slots.iter().enumerate() {
        let Some(previous) = slots_before.get(slot.call_id.as_str()).copied() else {
            continue;
        };
        if slot.status != ToolSlotStatus::Running || previous.status != ToolSlotStatus::Running {
            continue;
        }
        if let Some((output, details, diagnostics)) = tool_update(view_ops, index, slot, previous) {
            events.push(AgentEvent::ToolExecutionUpdate {
                tool_call_id: slot.call_id.clone(),
                tool_name: slot.name.clone(),
                output,
                details,
                diagnostics,
            });
        }
    }

    let generation = now.live.generation.as_ref();
    let generation_before = was.live.generation.as_ref();
    if let Some(active_gen) = generation
        && let Some(retry) = &active_gen.retry
        && generation_before.and_then(|g| g.retry.as_ref()).is_none()
    {
        events.push(AgentEvent::AutoRetryStart {
            attempt: active_gen.attempt,
            at: retry.at,
            error_message: retry.error.clone(),
        });
    }
    if let Some(gen_before) = generation_before
        && gen_before.retry.is_some()
        && generation.and_then(|g| g.retry.as_ref()).is_none()
    {
        events.push(AgentEvent::AutoRetryEnd {
            attempt: gen_before.attempt,
        });
    }
    if let Some(deferred) = generation.and_then(|g| g.deferred)
        && Some(deferred.poll_at) != generation_before.and_then(|g| g.deferred.map(|d| d.poll_at))
    {
        events.push(AgentEvent::DeferredPoll {
            poll_at: deferred.poll_at,
        });
    }

    // Tools that end in this commit.
    let mut tool_ends: Vec<(String, String, Option<EntryRecord>)> = Vec::new();
    let mut push_end = |call_id: &str, name: &str, entry_id: Option<crate::types::EntryId>| {
        let entry = entry_id.and_then(|eid| entries.iter().find(|e| e.id == eid).copied().cloned());
        tool_ends.push((call_id.to_owned(), name.to_owned(), entry));
    };

    for previous in slots_before.values() {
        if previous.status == ToolSlotStatus::Done {
            continue;
        }
        match slots.iter().find(|c| c.call_id == previous.call_id) {
            Some(slot) if slot.status == ToolSlotStatus::Done => {
                push_end(&previous.call_id, &previous.name, slot.entry);
            }
            None => {
                let res_id =
                    result_of(&entries.iter().copied().cloned().collect::<Vec<_>>(), &previous.call_id).map(|e| e.id);
                push_end(&previous.call_id, &previous.name, res_id);
            }
            _ => {}
        }
    }
    for slot in slots {
        if slot.status == ToolSlotStatus::Done && !slots_before.contains_key(slot.call_id.as_str()) {
            push_end(&slot.call_id, &slot.name, slot.entry);
        }
    }

    let mut assistant_appended = false;
    for entry in &entries {
        for (call_id, name, end_entry) in &tool_ends {
            if end_entry.as_ref().is_some_and(|e| e.id == entry.id) {
                events.push(AgentEvent::ToolExecutionEnd {
                    tool_call_id: call_id.clone(),
                    tool_name: name.clone(),
                    entry: end_entry.clone(),
                });
            }
        }
        let Some(message) = entry.model.as_deref().and_then(|m| m.first()) else {
            events.push(AgentEvent::EntryAppended {
                entry: (*entry).clone(),
            });
            continue;
        };
        let is_assistant = matches!(message, Message::Assistant(_));
        let streamed = is_assistant && partial_before.is_some() && !assistant_appended;
        if is_assistant {
            assistant_appended = true;
        }
        if !streamed {
            events.push(AgentEvent::MessageStart {
                message: message.clone(),
            });
        }
        events.push(AgentEvent::MessageEnd {
            entry: (*entry).clone(),
        });
    }
    for (call_id, name, end_entry) in tool_ends {
        if end_entry.is_none() {
            events.push(AgentEvent::ToolExecutionEnd {
                tool_call_id: call_id,
                tool_name: name,
                entry: None,
            });
        }
    }

    let compactions_before = was.live.compactions.as_deref().unwrap_or(&[]);
    let compactions = now.live.compactions.as_deref().unwrap_or(&[]);
    for status in compactions_before {
        if !compactions.iter().any(|c| c.task_id == status.task_id) {
            events.push(AgentEvent::CompactionEnd {
                task_id: status.task_id,
                reason: status.reason,
            });
        }
    }

    let mut turn_ended = false;
    for task in tasks.values().copied() {
        match &task.state {
            TaskState::Completing { .. } if task.kind == "pi.generation" && !held.contains(&task.id) => {
                held.insert(task.id);
                turn_ended = true;
            }
            TaskState::Terminal { outcome } => {
                if task.kind == "pi.generation" && !held.shift_remove(&task.id) {
                    turn_ended = true;
                }
                match outcome {
                    TaskOutcome::Faulted { error } => {
                        events.push(AgentEvent::TaskFailed {
                            task_id: task.id,
                            kind: task.kind.clone(),
                            message: error.message.clone(),
                        });
                    }
                    TaskOutcome::Orphaned { reason } => {
                        events.push(AgentEvent::TaskFailed {
                            task_id: task.id,
                            kind: task.kind.clone(),
                            message: reason.clone(),
                        });
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    if turn_ended {
        events.push(AgentEvent::TurnEnd);
    }

    let run = now.live.run.as_ref();
    let run_before = was.live.run.as_ref();
    let run_changed = run.and_then(|r| r.inputs.first()) != run_before.and_then(|r| r.inputs.first());
    if let Some(rb) = run_before
        && run_changed
    {
        events.push(AgentEvent::RunEnd {
            inputs: rb.inputs.clone(),
        });
    }

    for record in submissions {
        events.push(AgentEvent::Submission { record: record.clone() });
    }
    if doc_changed(view_ops, "pi.inbox", before, after) {
        events.push(AgentEvent::InboxUpdate {
            items: queued(now.inbox.as_ref()),
        });
    }
    if doc_changed(view_ops, "pi.agent", before, after) {
        events.push(AgentEvent::AgentChanged {
            agent: now.agent.clone().unwrap_or_default(),
        });
    }
    if doc_changed(view_ops, "pi.usage", before, after) {
        events.push(AgentEvent::UsageChanged {
            usage: now.usage.clone().unwrap_or_default(),
        });
    }
    for status in compactions {
        if !compactions_before.iter().any(|c| c.task_id == status.task_id) {
            events.push(AgentEvent::CompactionStart {
                task_id: status.task_id,
                reason: status.reason,
                blocking: status.blocking,
            });
        }
    }
    if let Some(r) = run
        && run_changed
    {
        events.push(AgentEvent::RunStart {
            inputs: r.inputs.clone(),
        });
    }
    if let Some(r) = run
        && Some(r.task_id) != run_before.map(|rb| rb.task_id)
        && tasks.get(&r.task_id).is_some_and(|t| t.kind == "pi.generation")
    {
        events.push(AgentEvent::TurnStart);
    }

    events
}

fn partial_path() -> [Seg; 4] {
    [
        Seg::String("docs".to_owned()),
        Seg::String("pi.live".to_owned()),
        Seg::String("generation".to_owned()),
        Seg::String("message".to_owned()),
    ]
}

fn op_path(op: &Op) -> Option<&Path> {
    match op {
        Op::Replace(_) => None,
        Op::Set(o) => Some(&o.1),
        Op::Delete(o) => Some(&o.1),
        Op::Append(o) => Some(&o.1),
        Op::Truncate(o) => Some(&o.1),
        Op::Splice(o) => Some(&o.1),
        Op::Move(o) => Some(&o.1),
    }
}

fn seg_as_index(seg: &Seg) -> Option<usize> {
    match seg {
        Seg::Index(i) if *i >= 0 => Some(*i as usize),
        Seg::String(s) => s.parse::<usize>().ok(),
        _ => None,
    }
}

/// Translate the view operations on the in-flight message into message changes (spec §9.4).
fn message_changes(view_ops: &[Op], message: &AssistantMessage) -> Vec<MessageChange> {
    let prefix = partial_path();
    let mut changes: Vec<MessageChange> = Vec::new();
    let mut whole: IndexSet<usize> = IndexSet::new();

    for op in view_ops {
        let Some(path) = op_path(op) else {
            return vec![MessageChange::Message {
                message: message.clone(),
            }];
        };
        if !starts_with(path, &prefix) {
            if starts_with(&prefix, path) {
                return vec![MessageChange::Message {
                    message: message.clone(),
                }];
            }
            continue;
        }
        let rest = &path[prefix.len()..];
        if rest.first() == Some(&Seg::String("usage".to_owned())) {
            continue;
        }
        if rest.first() != Some(&Seg::String("content".to_owned())) {
            return vec![MessageChange::Message {
                message: message.clone(),
            }];
        }
        if rest.len() == 1 {
            let Op::Splice(op_p) = op else {
                return vec![MessageChange::Message {
                    message: message.clone(),
                }];
            };
            if op_p.3 != 0 || op_p.2 < 0 {
                return vec![MessageChange::Message {
                    message: message.clone(),
                }];
            }
            for (offset, item) in op_p.4.iter().enumerate() {
                let Ok(block) = serde_json::from_value::<AssistantContentBlock>(item.clone()) else {
                    return vec![MessageChange::Message {
                        message: message.clone(),
                    }];
                };
                let content_index = op_p.2 as usize + offset;
                let change = match block {
                    AssistantContentBlock::Text(_) => MessageChange::TextStart { content_index, block },
                    AssistantContentBlock::Thinking(_) => MessageChange::ThinkingStart { content_index, block },
                    AssistantContentBlock::ToolCall(_) => MessageChange::ToolcallStart { content_index, block },
                };
                changes.push(change);
            }
            continue;
        }

        let Some(content_index) = rest.get(1).and_then(seg_as_index) else {
            return vec![MessageChange::Message {
                message: message.clone(),
            }];
        };
        if whole.contains(&content_index) {
            continue;
        }
        let field = rest.get(2).and_then(|s| match s {
            Seg::String(k) => Some(k.as_str()),
            Seg::Index(_) => None,
        });

        match op {
            Op::Append(op_a) if rest.len() == 3 && (field == Some("text") || field == Some("thinking")) => {
                if field == Some("text") {
                    changes.push(MessageChange::TextDelta {
                        content_index,
                        delta: op_a.2.clone(),
                    });
                } else {
                    changes.push(MessageChange::ThinkingDelta {
                        content_index,
                        delta: op_a.2.clone(),
                    });
                }
            }
            Op::Append(op_a) if field == Some("arguments") => {
                changes.push(MessageChange::ToolcallDelta {
                    content_index,
                    path: rest[3..].to_vec(),
                    delta: op_a.2.clone(),
                });
            }
            _ => {
                whole.insert(content_index);
                if let Some(block) = message.content.get(content_index).cloned() {
                    changes.push(MessageChange::Block { content_index, block });
                } else {
                    return vec![MessageChange::Message {
                        message: message.clone(),
                    }];
                }
            }
        }
    }

    changes
}

type ToolUpdateFields = (Option<ToolOutputUpdate>, Option<JsonValue>, Option<Vec<ToolDiagnostic>>);

/// Output, details, and diagnostics changes of a running slot, from the view operations on it.
fn tool_update(view_ops: &[Op], index: usize, slot: &ToolSlot, previous: &ToolSlot) -> Option<ToolUpdateFields> {
    let output_path_str = [
        Seg::String("docs".to_owned()),
        Seg::String("pi.live".to_owned()),
        Seg::String("tools".to_owned()),
        Seg::String(index.to_string()),
        Seg::String("output".to_owned()),
    ];
    let output_path_idx = [
        Seg::String("docs".to_owned()),
        Seg::String("pi.live".to_owned()),
        Seg::String("tools".to_owned()),
        Seg::Index(index as i64),
        Seg::String("output".to_owned()),
    ];

    let mut trim_start: usize = 0;
    let mut append = String::new();
    let mut set = false;

    for op in view_ops {
        let Some(path) = op_path(op) else {
            continue;
        };
        if !starts_with(path, &output_path_str) && !starts_with(path, &output_path_idx) {
            continue;
        }
        match op {
            Op::Truncate(op_t) => trim_start += op_t.2.max(0) as usize,
            Op::Append(op_a) => append.push_str(&op_a.2),
            _ => set = true,
        }
    }

    let output = if set || (slot.output != previous.output && trim_start == 0 && append.is_empty()) {
        Some(ToolOutputUpdate::Set {
            set: slot.output.clone().unwrap_or_default(),
        })
    } else if trim_start > 0 || !append.is_empty() {
        Some(ToolOutputUpdate::Delta {
            trim_start: (trim_start > 0).then_some(trim_start),
            append: (!append.is_empty()).then_some(append),
        })
    } else {
        None
    };

    let details = if slot.details != previous.details {
        Some(slot.details.clone().unwrap_or(JsonValue::Null))
    } else {
        None
    };
    let diagnostics = if slot.diagnostics != previous.diagnostics {
        Some(slot.diagnostics.clone().unwrap_or_default())
    } else {
        None
    };

    if output.is_none() && details.is_none() && diagnostics.is_none() {
        None
    } else {
        Some((output, details, diagnostics))
    }
}

fn starts_with(path: &[Seg], prefix: &[Seg]) -> bool {
    if prefix.len() > path.len() {
        return false;
    }
    prefix.iter().zip(path.iter()).all(|(a, b)| match (a, b) {
        (Seg::String(sa), Seg::String(sb)) => sa == sb,
        (Seg::Index(ia), Seg::Index(ib)) => ia == ib,
        (Seg::Index(i), Seg::String(s)) | (Seg::String(s), Seg::Index(i)) => {
            *i >= 0 && s.parse::<i64>().ok() == Some(*i)
        }
    })
}

/// Experimental: attach to one conversation's agent events (spec §9.4).
pub async fn watch_events(
    conversation: &crate::harness::harness::Conversation,
    context: Context,
) -> pi_js::Result<AgentEventStream> {
    watch_events_on_views(conversation.views(), conversation.id(), context).await
}
