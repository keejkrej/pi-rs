//! Port of packages/durable/src/harness/live.ts

use std::sync::LazyLock;

use pi_ai::types::AssistantMessage;
use pi_chord::JsonValue;
use serde::{Deserialize, Serialize};

use crate::documents::{ConversationDocToken, DocDefinitionInput, define_conversation_doc};
use crate::harness::generation::convert_partial;
use crate::harness::scheduler::SchedulerOutcome;
use crate::harness::types::{CompactionReason, CompactionResult, ToolDiagnostic};
use crate::session::transaction::Transaction;
use crate::types::{EntryId, ErasedTaskRecord, LatestFork, SubmissionId, SubmissionSettlement, TaskId};

/// Status of one tool call slot in [`LiveState`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolSlotStatus {
    Pending,
    Running,
    Done,
}

/// Presentation of one tool call of the current round.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSlot {
    pub call_id: String,
    pub name: String,
    /// Absent for a call not started yet (sequential round) and for a call its request did not
    /// offer, which starts `Done` with the `entry` generation wrote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    pub status: ToolSlotStatus,
    /// Retained running output and what the bounds dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped_bytes: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped_lines: Option<usize>,
    /// Last `details()` value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
    /// Diagnostics recorded through `api.diagnostic()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Vec<ToolDiagnostic>>,
    /// Result entry once done; absent when the tool task faulted or was orphaned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<EntryId>,
}

/// Durable backoff state for generation/compaction retries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DurableRetryState {
    pub at: i64,
    pub error: String,
}

/// Presentation of one live compaction task (spec §8.7).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionStatus {
    pub task_id: TaskId<CompactionResult>,
    pub reason: CompactionReason,
    /// Whether a generation waits for it: a compaction the generation owns.
    pub blocking: bool,
    pub attempt: u32,
    /// Durable backoff before the next summarization attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<DurableRetryState>,
}

/// Active run ownership and its attached user submissions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveRun {
    pub task_id: TaskId,
    pub inputs: Vec<SubmissionId>,
}

/// Deferred response polling metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveDeferred {
    pub poll_at: i64,
}

/// Presentation of the current generation attempt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveGeneration {
    pub attempt: u32,
    /// Committed throttled partial of the in-flight response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<AssistantMessage>,
    /// Durable backoff before the next attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<DurableRetryState>,
    /// Provider-side deferred response being polled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred: Option<LiveDeferred>,
}

/// Built-in live conversation state: run control and presentation of the current generation and tool round.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveState {
    /// Run control: the task that settles the run's inputs, and those inputs; present exactly while busy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<LiveRun>,
    /// Presentation of the current generation attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<LiveGeneration>,
    /// The current tool round in call order, from the tool-calling answer until the generation's `tools` phase ends it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolSlot>>,
    /// Live compaction tasks in task ID order; absent when none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compactions: Option<Vec<CompactionStatus>>,
}

pub static LIVE_DOC: LazyLock<ConversationDocToken<LiveState>> = LazyLock::new(|| {
    define_conversation_doc(
        LatestFork::Initial,
        DocDefinitionInput {
            kind: "pi.live",
            version: 1,
            initial: LiveState::default,
            migrate: None,
            // REMINDER: a complete base whenever nothing runs (spec §8.2): no generation and no running tool slot. That holds
            // while idle, in the commit handing a generation over to its tool round, and between tools, so the delta chain
            // spans at most one generation or the overlapping execution of one round's tools. A slot holds output only while
            // running, so every base is small. Do not add a delta-count bound; the tool output benchmark checks this rule.
            checkpoint_when: Some(|value, _, _| {
                value.generation.is_none()
                    && !value
                        .tools
                        .as_deref()
                        .unwrap_or(&[])
                        .iter()
                        .any(|slot| slot.status == ToolSlotStatus::Running)
            }),
        },
    )
    .expect("valid built-in pi.live doc definition")
});

/// Built-in task kinds that can own `pi.live.run`.
const GENERATION_TASK_KIND: &str = "pi.generation";
const TOOL_TASK_KIND: &str = "pi.tool";
const COMPACTION_TASK_KIND: &str = "pi.compaction";

/// End the run owned by `task_id`: settle each of its inputs and remove `run`.
/// Always removes `generation` and `tools`, whose presentation belongs to the ending run.
pub fn end_run(
    tx: &Transaction,
    live: &mut LiveState,
    task_id: TaskId,
    settlement: &SubmissionSettlement,
) -> pi_js::Result<()> {
    if let Some(run) = &live.run
        && run.task_id == task_id
    {
        for &id in &run.inputs {
            tx.settle_submission(id, settlement.clone())?;
        }
        live.run = None;
    }
    live.generation = None;
    live.tools = None;
    Ok(())
}

/// Add the status of a compaction task created in this commit; statuses stay in task ID order.
pub fn add_compaction_status(live: &mut LiveState, status: CompactionStatus) {
    live.compactions.get_or_insert_with(Vec::new).push(status);
}

/// The status of compaction task `task_id`, if listed.
pub fn compaction_status<'a>(live: &'a LiveState, task_id: TaskId) -> Option<&'a CompactionStatus> {
    live.compactions
        .as_deref()
        .and_then(|list| list.iter().find(|status| status.task_id.erased() == task_id))
}

/// Mutable status of compaction task `task_id`, if listed.
pub fn compaction_status_mut<'a>(live: &'a mut LiveState, task_id: TaskId) -> Option<&'a mut CompactionStatus> {
    live.compactions
        .as_deref_mut()
        .and_then(|list| list.iter_mut().find(|status| status.task_id.erased() == task_id))
}

/// Remove the status of compaction task `task_id`, and the list once empty.
pub fn remove_compaction_status(live: &mut LiveState, task_id: TaskId) {
    if let Some(statuses) = &mut live.compactions {
        if let Some(index) = statuses.iter().position(|status| status.task_id.erased() == task_id) {
            statuses.remove(index);
        }
        if statuses.is_empty() {
            live.compactions = None;
        }
    }
}

/// The slot of tool task `task_id` in the current round, if the round still lists it.
pub fn tool_slot<'a>(live: &'a LiveState, task_id: TaskId) -> Option<&'a ToolSlot> {
    live.tools
        .as_deref()
        .and_then(|list| list.iter().find(|slot| slot.task_id == Some(task_id)))
}

/// Mutable slot of tool task `task_id` in the current round, if the round still lists it.
pub fn tool_slot_mut<'a>(live: &'a mut LiveState, task_id: TaskId) -> Option<&'a mut ToolSlot> {
    live.tools
        .as_deref_mut()
        .and_then(|list| list.iter_mut().find(|slot| slot.task_id == Some(task_id)))
}

/// Mark a slot done: the result entry, if any, now carries its running output, details, and diagnostics.
pub fn finish_slot(slot: &mut ToolSlot, entry: Option<EntryId>) {
    slot.status = ToolSlotStatus::Done;
    if let Some(entry_id) = entry {
        slot.entry = Some(entry_id);
    }
    clear_progress(slot);
}

/// Remove what a tool published while running; its result entry or a rerun replaces it.
pub fn clear_progress(slot: &mut ToolSlot) {
    slot.output = None;
    slot.dropped_bytes = None;
    slot.dropped_lines = None;
    slot.details = None;
    slot.diagnostics = None;
}

/// Harness cleanup for a terminal outcome the scheduler writes itself (`faulted` or `orphaned`).
/// A run task ends its run; a tool task's slot is marked done without an entry, and context
/// derivation synthesizes the missing result; a compaction task's status is removed.
/// Ignores other kinds so it never creates `pi.live` elsewhere.
pub async fn settle_scheduler_outcome(
    tx: &Transaction,
    record: &ErasedTaskRecord,
    outcome: &SchedulerOutcome,
) -> pi_js::Result<()> {
    if record.kind == TOOL_TASK_KIND {
        let draft = tx.doc_conversation(&*LIVE_DOC, record.conversation_id).await?;
        draft.update(|live| {
            if let Some(slot) = tool_slot_mut(live, record.id) {
                finish_slot(slot, None);
            }
        })?;
        return Ok(());
    }
    if record.kind == COMPACTION_TASK_KIND {
        let draft = tx.doc_conversation(&*LIVE_DOC, record.conversation_id).await?;
        draft.update(|live| {
            remove_compaction_status(live, record.id);
        })?;
        return Ok(());
    }
    if record.kind != GENERATION_TASK_KIND {
        return Ok(());
    }
    let draft = tx.doc_conversation(&*LIVE_DOC, record.conversation_id).await?;
    let mut live = draft.get()?;
    if live.run.as_ref().is_none_or(|run| run.task_id != record.id) {
        return Ok(());
    }
    convert_partial(tx, &mut live, record.conversation_id).await?;
    let settlement = match outcome {
        SchedulerOutcome::Faulted { error } => SubmissionSettlement::Unanswered {
            reason: "faulted".to_owned(),
            detail: Some(JsonValue::String(error.message.clone())),
        },
        SchedulerOutcome::Orphaned { reason } => SubmissionSettlement::Unanswered {
            reason: reason.clone(),
            detail: None,
        },
    };
    end_run(tx, &mut live, record.id, &settlement)?;
    draft.replace(live)
}
