//! Port of packages/durable/test/harness-ownership.test.ts

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseFactory, FauxResponseStep,
    FauxToolCallOptions, faux_assistant_message, faux_text, faux_tool_call,
};
use pi_ai::types::{Message, StopReason};
use pi_ai::{TSchema, TextContent, Type};
use pi_chord::Context;
use pi_chord::context::with_abort_signal;
use pi_durable::documents::{ConversationDocToken, DocDefinitionInput, define_conversation_doc};
use pi_durable::errors::StorageRejected;
use pi_durable::harness::agent::configure;
use pi_durable::harness::harness::{Conversation, ConversationEntriesQuery, Harness};
use pi_durable::harness::live::{LIVE_DOC, LiveRun};
use pi_durable::harness::types::{
    AgentChange, ConversationHandle, FieldChange, InputSubmissionDraft, ModelRef, SubmissionDraft, ToolExecutionResult,
    ToolRegistration, ToolReplayMode, ToolResultContentBlock, UserInput,
};
use pi_durable::session::session::Session;
use pi_durable::storage::memory::MemoryStorage;
use pi_durable::storage::sqlite::node::{NodeSqliteStorageOptions, open_node_sqlite_storage};
use pi_durable::tasks::{AnyTask, Task, TaskDefinition, TypedPhaseHandler, define_task};
use pi_durable::types::{
    ConversationId, ConversationOwnership, ConversationQuery, ConversationRecord, Cursor, DocumentAddress, DocumentId,
    DocumentPoint, DocumentQuery, DocumentRecord, EntryDraft, EntryId, EntryLookup, EntryQuery, EntryRecord,
    ErasedTaskRecord, InputSubmissionState, JoinPolicy, JsonValue, LatestFork, NextTaskState, Page, Seq, Storage,
    StorageWrite, StoredDocument, SubmissionId, SubmissionQuery, SubmissionRecord, SubmissionState, TaskId,
    TaskOptions, TaskOutcome, TaskOutcomeError, TaskOwnership, TaskQuery, TaskState, TaskStatus, WriteSubmissionState,
};
use pi_durable::{AbortTaskResult, define_tool};
use pi_js::abort::AbortController;
use pretty_assertions::assert_eq;
use serde::{Deserialize, Serialize};
use support::chat_support::{ChatSetup, OpenChatEnv, chat_setup, open_chat};
use support::harness_support::{add_task, add_tool};
use support::session_support::context;
use support::task_support::{Deferred, OpenTasksOptions, aborted, deferred, open_tasks, settled, track};
use tempfile::TempDir;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ending {
    Completed,
    Failed,
}

/// Per-test mutable state replacing module-level maps in TS so parallel tests stay isolated.
#[derive(Clone, Default)]
struct OwnershipState {
    gates: Arc<Mutex<IndexMap<String, Deferred<Ending>>>>,
    runs: Arc<Mutex<IndexMap<String, usize>>>,
}

impl OwnershipState {
    fn new() -> Self {
        Self::default()
    }

    fn gate(&self, name: &str) -> Deferred<Ending> {
        let mut guard = self.gates.lock().unwrap_or_else(|e| e.into_inner());
        guard.entry(name.to_owned()).or_insert_with(deferred).clone()
    }

    fn open(&self, name: &str, ending: Ending) {
        self.gate(name).resolve(ending);
    }

    fn runs_of(&self, name: &str) -> usize {
        *self
            .runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .unwrap_or(&0)
    }

    fn hold_task(&self) -> Task<HoldInput, HoldCheckpoint, (), ()> {
        hold_task_with_state(self.clone())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HoldInput {
    name: String,
    #[serde(default)]
    slow_abort: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
enum HoldCheckpoint {
    Hold,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WaiterInput {
    on: Vec<TaskId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
enum WaiterCheckpoint {
    Wait,
    Done,
}

fn hold_task_with_state(state: OwnershipState) -> Task<HoldInput, HoldCheckpoint, (), ()> {
    let state_hold = state.clone();
    let state_abort = state.clone();
    let mut phases: IndexMap<String, TypedPhaseHandler<HoldInput, HoldCheckpoint, (), ()>> = IndexMap::new();
    phases.insert(
        "hold".to_owned(),
        Arc::new(move |task, runtime, ctx| {
            let state_hold = state_hold.clone();
            Box::pin(async move {
                {
                    let mut runs = state_hold.runs.lock().unwrap_or_else(|e| e.into_inner());
                    *runs.entry(task.input.name.clone()).or_insert(0) += 1;
                }
                let gate = state_hold.gate(&task.input.name);
                let gate_fut = gate.wait();
                let signal = runtime.signal()?;
                let abort_fut = aborted::<Ending>(&signal);
                let ending = tokio::select! {
                    res = gate_fut => res?,
                    res = abort_fut => res?,
                };
                runtime
                    .commit(
                        move |_tx, _current| {
                            Box::pin(async move {
                                Ok(Some(match ending {
                                    Ending::Completed => NextTaskState::Terminal {
                                        outcome: TaskOutcome::Completed { result: () },
                                    },
                                    Ending::Failed => NextTaskState::Terminal {
                                        outcome: TaskOutcome::Failed {
                                            error: TaskOutcomeError {
                                                message: "gate failed".to_owned(),
                                                detail: None,
                                            },
                                            result: None,
                                        },
                                    },
                                }))
                            })
                        },
                        ctx,
                    )
                    .await
            })
        }),
    );

    define_task(TaskDefinition {
        name: "test.hold".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(HoldCheckpoint::Hold)),
        phases,
        abort: Arc::new(move |task, runtime, ctx| {
            let state_abort = state_abort.clone();
            Box::pin(async move {
                if task.input.slow_abort {
                    let key = format!("abort.{}", task.input.name);
                    let _ = state_abort.gate(&key).wait().await;
                }
                runtime
                    .commit(
                        |_tx, _current| {
                            Box::pin(async {
                                Ok(Some(NextTaskState::Terminal {
                                    outcome: TaskOutcome::Aborted {
                                        reason: None,
                                        result: None,
                                    },
                                }))
                            })
                        },
                        ctx,
                    )
                    .await
            })
        }),
        migrate: None,
        _hooks: std::marker::PhantomData,
    })
}

fn waiter_task() -> Task<WaiterInput, WaiterCheckpoint, (), ()> {
    let mut phases: IndexMap<String, TypedPhaseHandler<WaiterInput, WaiterCheckpoint, (), ()>> = IndexMap::new();
    phases.insert(
        "wait".to_owned(),
        Arc::new(|task, runtime, ctx| {
            Box::pin(async move {
                let on = task.input.on.clone();
                runtime
                    .commit(
                        move |_tx, _current| {
                            Box::pin(async move {
                                Ok(Some(NextTaskState::Waiting {
                                    checkpoint: WaiterCheckpoint::Done,
                                    on,
                                    policy: JoinPolicy::AllSettled,
                                }))
                            })
                        },
                        ctx,
                    )
                    .await
            })
        }),
    );
    phases.insert(
        "done".to_owned(),
        Arc::new(|_task, runtime, ctx| {
            Box::pin(async move {
                runtime
                    .commit(
                        |_tx, _current| {
                            Box::pin(async {
                                Ok(Some(NextTaskState::Terminal {
                                    outcome: TaskOutcome::Completed { result: () },
                                }))
                            })
                        },
                        ctx,
                    )
                    .await
            })
        }),
    );

    define_task(TaskDefinition {
        name: "test.waiter".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(WaiterCheckpoint::Wait)),
        phases,
        abort: Arc::new(|_task, runtime, ctx| {
            Box::pin(async move {
                runtime
                    .commit(
                        |_tx, _current| {
                            Box::pin(async {
                                Ok(Some(NextTaskState::Terminal {
                                    outcome: TaskOutcome::Aborted {
                                        reason: None,
                                        result: None,
                                    },
                                }))
                            })
                        },
                        ctx,
                    )
                    .await
            })
        }),
        migrate: None,
        _hooks: std::marker::PhantomData,
    })
}

fn unregistered_task() -> Task<HoldInput, HoldCheckpoint, (), ()> {
    let mut phases: IndexMap<String, TypedPhaseHandler<HoldInput, HoldCheckpoint, (), ()>> = IndexMap::new();
    phases.insert("hold".to_owned(), Arc::new(|_, _, _| Box::pin(async { Ok(()) })));
    define_task(TaskDefinition {
        name: "test.unregistered".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(HoldCheckpoint::Hold)),
        phases,
        abort: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
        migrate: None,
        _hooks: std::marker::PhantomData,
    })
}

struct Tree {
    owner: TaskId<()>,
    child: ConversationId,
    inner: TaskId<()>,
}

async fn owned_child(
    parent: &Conversation,
    hold: &Task<HoldInput, HoldCheckpoint, (), ()>,
    name: &str,
    background: bool,
    slow_inner: bool,
    slow_owner: bool,
) -> Tree {
    let hold_c = hold.clone();
    let name = name.to_owned();
    parent
        .commit(
            move |tx| async move {
                let owner = tx
                    .create_task(
                        &hold_c,
                        &HoldInput {
                            name: name.clone(),
                            slow_abort: slow_owner,
                        },
                        TaskOptions {
                            ownership: TaskOwnership::Conversation,
                            conversation_id: None,
                            background,
                        },
                    )
                    .await?;
                let child = tx
                    .create_conversation(ConversationOwnership::Task {
                        task_id: owner.erased(),
                    })
                    .await?;
                let inner = tx
                    .create_task(
                        &hold_c,
                        &HoldInput {
                            name: format!("{name}.inner"),
                            slow_abort: slow_inner,
                        },
                        TaskOptions {
                            ownership: TaskOwnership::Conversation,
                            conversation_id: Some(child.id),
                            background: false,
                        },
                    )
                    .await?;
                Ok(Tree {
                    owner,
                    child: child.id,
                    inner,
                })
            },
            context(),
        )
        .await
        .expect("ownedChild commit")
}

struct GatedResponse {
    step: FauxResponseStep,
    reached: Deferred<()>,
    #[allow(dead_code)]
    gate: Deferred<()>,
}

fn gated_response(message: pi_ai::types::AssistantMessage) -> GatedResponse {
    let reached = deferred::<()>();
    let gate = deferred::<()>();
    let reached_cb = reached.clone();
    let gate_cb = gate.clone();
    let factory: FauxResponseFactory = Arc::new(move |_ctx, opts, _state, _model| {
        let reached_cb = reached_cb.clone();
        let gate_cb = gate_cb.clone();
        let message = message.clone();
        Box::pin(async move {
            reached_cb.resolve(());
            let signal = opts.and_then(|o| o.signal.clone());
            match signal {
                Some(sig) => {
                    tokio::select! {
                        res = gate_cb.wait() => res?,
                        res = aborted::<()>(&sig) => res?,
                    }
                }
                None => {
                    gate_cb.wait().await?;
                }
            }
            Ok(message)
        })
    });
    GatedResponse {
        step: FauxResponseStep::Factory(factory),
        reached,
        gate,
    }
}

async fn wait_until<F, Fut>(mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..500 {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(check().await, "Condition was not reached");
}

async fn busy(conversation: &Conversation, task: TaskId) {
    conversation
        .commit(
            move |tx| async move {
                let draft = tx.doc_conversation(&*LIVE_DOC, conversation.id()).await?;
                draft.update(|live| {
                    live.run = Some(LiveRun {
                        task_id: task,
                        inputs: Vec::new(),
                    });
                })
            },
            context(),
        )
        .await
        .expect("mark busy");
}

async fn status(harness: &Harness, id: TaskId<()>) -> TaskState {
    harness
        .get_erased_task(id.erased(), context())
        .await
        .expect("get_task")
        .expect("task present")
        .state
}

struct OpenedOwnership {
    harness: Harness,
    root: Conversation,
    reports: Arc<Mutex<Vec<pi_js::Error>>>,
}

async fn open_harness_with(storage: Arc<dyn Storage>, state: &OwnershipState) -> OpenedOwnership {
    let hold_any: AnyTask = state.hold_task().into_any();
    let waiter_any: AnyTask = waiter_task().into_any();
    let opened = open_tasks(storage, vec![hold_any, waiter_any], OpenTasksOptions::default())
        .await
        .expect("open tasks");
    let root = opened.harness.root(context(), None).await.expect("root");
    opened.harness.resume().expect("resume");
    OpenedOwnership {
        harness: opened.harness,
        root,
        reports: opened.reports,
    }
}

async fn sqlite_path() -> (TempDir, String) {
    let dir = TempDir::new().expect("create tempdir");
    let path = dir.path().join("session.sqlite").to_string_lossy().into_owned();
    (dir, path)
}

async fn open_sqlite(path: &str) -> Arc<dyn Storage> {
    Arc::new(
        open_node_sqlite_storage(path, NodeSqliteStorageOptions::default())
            .await
            .expect("open sqlite"),
    )
}

/// Storage wrapper intercepting `commit` to inject a one-shot `StorageRejected` when marking a target task.
struct RejectOnceOnMarkStorage {
    inner: Arc<dyn Storage>,
    reject_mark: Arc<Mutex<Option<TaskId>>>,
}

#[async_trait]
impl Storage for RejectOnceOnMarkStorage {
    async fn commit(&self, writes: Vec<StorageWrite>, context: Context) -> pi_js::Result<Seq> {
        let should_reject = {
            let mut guard = self.reject_mark.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(target) = *guard {
                let hits = writes.iter().any(|w| {
                    matches!(
                        w,
                        StorageWrite::Task { value } if value.id == target && value.abort_requested
                    )
                });
                if hits {
                    *guard = None;
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };
        if should_reject {
            return Err(StorageRejected::new("rejected once").into_error());
        }
        self.inner.commit(writes, context).await
    }

    async fn mint_id(&self) -> pi_js::Result<i64> {
        self.inner.mint_id().await
    }

    async fn conversation(&self, id: ConversationId, context: Context) -> pi_js::Result<Option<ConversationRecord>> {
        self.inner.conversation(id, context).await
    }

    async fn scan_conversations(
        &self,
        query: ConversationQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<ConversationRecord, Cursor>> {
        self.inner.scan_conversations(query, limit, cursor, context).await
    }

    async fn entry(&self, id: EntryId, context: Context) -> pi_js::Result<Option<EntryLookup>> {
        self.inner.entry(id, context).await
    }

    async fn entry_in_conversation(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
        context: Context,
    ) -> pi_js::Result<Option<EntryLookup>> {
        self.inner.entry_in_conversation(conversation_id, id, context).await
    }

    async fn find_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
        context: Context,
    ) -> pi_js::Result<Option<EntryRecord>> {
        self.inner
            .find_latest_head_marker(conversation_id, at_or_before_entry_id, context)
            .await
    }

    async fn scan_entries(
        &self,
        query: EntryQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<EntryRecord, Cursor>> {
        self.inner.scan_entries(query, limit, cursor, context).await
    }

    async fn task(&self, id: TaskId, context: Context) -> pi_js::Result<Option<ErasedTaskRecord>> {
        self.inner.task(id, context).await
    }

    async fn scan_tasks(
        &self,
        query: TaskQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<ErasedTaskRecord, Cursor>> {
        self.inner.scan_tasks(query, limit, cursor, context).await
    }

    async fn submission(&self, id: SubmissionId, context: Context) -> pi_js::Result<Option<SubmissionRecord>> {
        self.inner.submission(id, context).await
    }

    async fn scan_submissions(
        &self,
        query: SubmissionQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<SubmissionRecord, Cursor>> {
        self.inner.scan_submissions(query, limit, cursor, context).await
    }

    async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
        context: Context,
    ) -> pi_js::Result<Option<SubmissionRecord>> {
        self.inner
            .submission_by_request(conversation_id, request_id, context)
            .await
    }

    async fn find_document(
        &self,
        address: DocumentAddress,
        at: DocumentPoint,
        context: Context,
    ) -> pi_js::Result<Option<DocumentRecord>> {
        self.inner.find_document(address, at, context).await
    }

    async fn document(
        &self,
        id: DocumentId,
        at: DocumentPoint,
        context: Context,
    ) -> pi_js::Result<Option<StoredDocument>> {
        self.inner.document(id, at, context).await
    }

    async fn scan_documents(
        &self,
        query: DocumentQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<DocumentRecord, Cursor>> {
        self.inner.scan_documents(query, limit, cursor, context).await
    }

    async fn close(&self, context: Context) -> pi_js::Result<()> {
        self.inner.close(context).await
    }
}

// ---- describe("ownership") ----

#[tokio::test]
async fn keeps_a_conversation_busy_while_its_owned_foreground_subtree_has_live_work_holding_the_completed_owner() {
    let state = OwnershipState::new();
    let hold = state.hold_task();
    let opened = open_harness_with(Arc::new(MemoryStorage::new()), &state).await;
    let tree = owned_child(&opened.root, &hold, "owner", false, false, false).await;
    state.open("owner", Ending::Completed);
    wait_until(|| async { status(&opened.harness, tree.owner).await.status() == TaskStatus::Completing }).await;
    let root_c = opened.root.clone();
    let idle = track(async move { root_c.wait_for_idle(context()).await });
    let harness_c = opened.harness.clone();
    let harness_idle = track(async move { harness_c.wait_for_idle(context()).await });
    assert!(!settled(&idle).await);
    assert!(!settled(&harness_idle).await);
    state.open("owner.inner", Ending::Completed);
    idle.wait().await.expect("root idle");
    harness_idle.wait().await.expect("harness idle");
    assert!(matches!(
        status(&opened.harness, tree.owner).await,
        TaskState::Terminal {
            outcome: TaskOutcome::Completed { .. }
        }
    ));
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn stops_idle_traversal_at_a_background_owner() {
    let state = OwnershipState::new();
    let hold = state.hold_task();
    let opened = open_harness_with(Arc::new(MemoryStorage::new()), &state).await;
    let tree = owned_child(&opened.root, &hold, "background", true, false, false).await;
    opened.root.wait_for_idle(context()).await.expect("root idle");
    opened.harness.wait_for_idle(context()).await.expect("harness idle");
    let child = opened
        .harness
        .conversation(tree.child, context())
        .await
        .expect("conv")
        .expect("present");
    let child_c = child.clone();
    let child_idle = track(async move { child_c.wait_for_idle(context()).await });
    assert!(!settled(&child_idle).await);
    child.abort(context(), None).await.expect("abort child");
    child_idle.wait().await.expect("child idle");
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn cascades_an_abort_mark_to_the_owned_foreground_subtree_and_withdraws_its_queued_inputs() {
    let state = OwnershipState::new();
    let hold = state.hold_task();
    let opened = open_harness_with(Arc::new(MemoryStorage::new()), &state).await;
    let tree = owned_child(&opened.root, &hold, "owner", false, false, false).await;
    let nested = opened
        .harness
        .conversation(tree.child, context())
        .await
        .expect("conv")
        .expect("present");
    let deeper = owned_child(&nested, &hold, "deeper", false, false, false).await;
    let shielded = owned_child(&nested, &hold, "shielded", true, false, false).await;
    busy(&nested, tree.inner.erased()).await;
    let queued = nested
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("later".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit")
        .id();
    assert_eq!(
        opened
            .harness
            .abort_task(tree.owner.erased(), context())
            .await
            .expect("abort"),
        AbortTaskResult::Marked
    );
    for id in [tree.owner, tree.inner, deeper.owner, deeper.inner] {
        let settled = opened.harness.wait_for_task(id, context()).await.expect("wait");
        assert!(matches!(settled.outcome, TaskOutcome::Aborted { .. }));
    }
    assert_ne!(
        status(&opened.harness, shielded.owner).await.status(),
        TaskStatus::Terminal
    );
    assert_ne!(
        status(&opened.harness, shielded.inner).await.status(),
        TaskStatus::Terminal
    );
    let sub_rec = opened
        .harness
        .submission(queued, context())
        .await
        .expect("sub")
        .expect("present")
        .status(context())
        .await
        .expect("status");
    assert!(matches!(
        sub_rec.state,
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, .. }) if reason == "aborted"
    ));
    opened
        .harness
        .abort_task(shielded.owner.erased(), context())
        .await
        .expect("abort shielded");
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn cascades_a_failed_owner_but_not_a_completed_one() {
    let state = OwnershipState::new();
    let hold = state.hold_task();
    let opened = open_harness_with(Arc::new(MemoryStorage::new()), &state).await;
    let failed = owned_child(&opened.root, &hold, "failed", false, false, false).await;
    let completed_tree = owned_child(&opened.root, &hold, "done", false, false, false).await;
    state.open("failed", Ending::Failed);
    state.open("done", Ending::Completed);
    assert!(matches!(
        opened
            .harness
            .wait_for_task(failed.inner, context())
            .await
            .expect("wait")
            .outcome,
        TaskOutcome::Aborted { .. }
    ));
    assert!(matches!(
        opened
            .harness
            .wait_for_task(failed.owner, context())
            .await
            .expect("wait")
            .outcome,
        TaskOutcome::Failed { .. }
    ));
    wait_until(|| async { status(&opened.harness, completed_tree.owner).await.status() == TaskStatus::Completing })
        .await;
    assert_ne!(
        status(&opened.harness, completed_tree.inner).await.status(),
        TaskStatus::Terminal
    );
    state.open("done.inner", Ending::Completed);
    assert!(matches!(
        opened
            .harness
            .wait_for_task(completed_tree.owner, context())
            .await
            .expect("wait")
            .outcome,
        TaskOutcome::Completed { .. }
    ));
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn aborts_a_background_task_directly_with_its_ordinary_subtree() {
    let state = OwnershipState::new();
    let hold = state.hold_task();
    let opened = open_harness_with(Arc::new(MemoryStorage::new()), &state).await;
    let tree = owned_child(&opened.root, &hold, "background", true, false, false).await;
    opened
        .harness
        .abort_task(tree.owner.erased(), context())
        .await
        .expect("abort");
    assert!(matches!(
        opened
            .harness
            .wait_for_task(tree.inner, context())
            .await
            .expect("wait")
            .outcome,
        TaskOutcome::Aborted { .. }
    ));
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn aborts_a_conversation_queued_inputs_withdrawn_writes_kept_foreground_work_aborted_background_kept() {
    let state = OwnershipState::new();
    let hold = state.hold_task();
    let opened = open_harness_with(Arc::new(MemoryStorage::new()), &state).await;
    let foreground = owned_child(&opened.root, &hold, "foreground", false, false, false).await;
    let background = owned_child(&opened.root, &hold, "background", true, false, false).await;
    busy(&opened.root, foreground.owner.erased()).await;
    let input = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("later".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("input")
        .id();
    let write = opened
        .root
        .submit(
            SubmissionDraft::Write {
                request_id: None,
                entry: EntryDraft {
                    kind: "note".to_owned(),
                    model: None,
                    data: None,
                    head: None,
                    edits: None,
                },
            },
            context(),
        )
        .await
        .expect("write")
        .id();
    opened.root.abort(context(), None).await.expect("abort");
    for id in [foreground.owner, foreground.inner] {
        assert_eq!(status(&opened.harness, id).await.status(), TaskStatus::Terminal);
    }
    assert_ne!(
        status(&opened.harness, background.owner).await.status(),
        TaskStatus::Terminal
    );
    assert_ne!(
        status(&opened.harness, background.inner).await.status(),
        TaskStatus::Terminal
    );
    let input_rec = opened
        .harness
        .submission(input, context())
        .await
        .expect("get sub")
        .expect("present")
        .status(context())
        .await
        .expect("status");
    assert!(matches!(
        input_rec.state,
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, .. }) if reason == "aborted"
    ));
    let write_rec = opened
        .harness
        .submission(write, context())
        .await
        .expect("get sub")
        .expect("present")
        .status(context())
        .await
        .expect("status");
    assert!(matches!(
        write_rec.state,
        SubmissionState::Write(WriteSubmissionState::Queued)
    ));
    opened
        .harness
        .abort_task(background.owner.erased(), context())
        .await
        .expect("abort background");
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn aborts_work_created_below_a_held_failed_owner_but_not_below_a_terminal_one() {
    let state = OwnershipState::new();
    let hold = state.hold_task();
    let opened = open_harness_with(Arc::new(MemoryStorage::new()), &state).await;
    let tree = owned_child(&opened.root, &hold, "owner", false, true, false).await;
    state.open("owner", Ending::Failed);
    wait_until(|| async {
        opened
            .harness
            .get_task(tree.inner, context())
            .await
            .expect("get")
            .is_some_and(|t| t.abort_requested)
    })
    .await;
    assert_eq!(
        status(&opened.harness, tree.owner).await.status(),
        TaskStatus::Completing
    );
    let child = opened
        .harness
        .conversation(tree.child, context())
        .await
        .expect("conv")
        .expect("present");
    let hold_c = hold.clone();
    let during = child
        .commit(
            move |tx| async move {
                tx.create_task(
                    &hold_c,
                    &HoldInput {
                        name: "during".to_owned(),
                        slow_abort: false,
                    },
                    TaskOptions {
                        ownership: TaskOwnership::Conversation,
                        conversation_id: None,
                        background: false,
                    },
                )
                .await
            },
            context(),
        )
        .await
        .expect("create during");
    assert!(matches!(
        opened
            .harness
            .wait_for_task(during, context())
            .await
            .expect("wait")
            .outcome,
        TaskOutcome::Aborted { .. }
    ));
    state.open("abort.owner.inner", Ending::Completed);
    assert!(matches!(
        opened
            .harness
            .wait_for_task(tree.owner, context())
            .await
            .expect("wait")
            .outcome,
        TaskOutcome::Failed { .. }
    ));
    let hold_c2 = hold.clone();
    let after = child
        .commit(
            move |tx| async move {
                tx.create_task(
                    &hold_c2,
                    &HoldInput {
                        name: "after".to_owned(),
                        slow_abort: false,
                    },
                    TaskOptions {
                        ownership: TaskOwnership::Conversation,
                        conversation_id: None,
                        background: false,
                    },
                )
                .await
            },
            context(),
        )
        .await
        .expect("create after");
    state.open("after", Ending::Completed);
    assert!(matches!(
        opened
            .harness
            .wait_for_task(after, context())
            .await
            .expect("wait")
            .outcome,
        TaskOutcome::Completed { .. }
    ));
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn withdraws_queued_inputs_below_the_aborted_task_but_keeps_its_own_conversations_queue_and_queued_writes() {
    let state = OwnershipState::new();
    let hold = state.hold_task();
    let opened = open_harness_with(Arc::new(MemoryStorage::new()), &state).await;
    let tree = owned_child(&opened.root, &hold, "owner", false, false, false).await;
    let child = opened
        .harness
        .conversation(tree.child, context())
        .await
        .expect("conv")
        .expect("present");
    busy(&opened.root, tree.owner.erased()).await;
    busy(&child, tree.inner.erased()).await;
    let own = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("own".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("own")
        .id();
    let below = child
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("below".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("below")
        .id();
    let write = child
        .submit(
            SubmissionDraft::Write {
                request_id: None,
                entry: EntryDraft {
                    kind: "note".to_owned(),
                    model: None,
                    data: None,
                    head: None,
                    edits: None,
                },
            },
            context(),
        )
        .await
        .expect("write")
        .id();
    opened
        .harness
        .abort_task(tree.owner.erased(), context())
        .await
        .expect("abort");
    opened.harness.wait_for_task(tree.inner, context()).await.expect("wait");
    let status_of = |id: SubmissionId| {
        let harness = opened.harness.clone();
        async move {
            harness
                .submission(id, context())
                .await
                .expect("sub")
                .expect("present")
                .status(context())
                .await
                .expect("status")
                .status()
        }
    };
    assert_eq!(status_of(own).await, pi_durable::types::SubmissionStatus::Queued);
    assert_eq!(status_of(below).await, pi_durable::types::SubmissionStatus::Unanswered);
    assert_eq!(status_of(write).await, pi_durable::types::SubmissionStatus::Queued);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn keeps_a_nested_background_owners_subtree_when_its_cancelled_background_owner_is_aborted() {
    let state = OwnershipState::new();
    let hold = state.hold_task();
    let opened = open_harness_with(Arc::new(MemoryStorage::new()), &state).await;
    let outer = owned_child(&opened.root, &hold, "outer", true, false, false).await;
    let outer_child = opened
        .harness
        .conversation(outer.child, context())
        .await
        .expect("conv")
        .expect("present");
    let inner = owned_child(&outer_child, &hold, "inner", true, false, false).await;
    opened
        .harness
        .abort_task(outer.owner.erased(), context())
        .await
        .expect("abort outer");
    assert!(matches!(
        opened
            .harness
            .wait_for_task(outer.inner, context())
            .await
            .expect("wait")
            .outcome,
        TaskOutcome::Aborted { .. }
    ));
    assert_ne!(
        status(&opened.harness, inner.owner).await.status(),
        TaskStatus::Terminal
    );
    assert_ne!(
        status(&opened.harness, inner.inner).await.status(),
        TaskStatus::Terminal
    );
    opened
        .harness
        .abort_task(inner.owner.erased(), context())
        .await
        .expect("abort inner");
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn decides_idle_after_reopen_from_owner_edges_it_has_to_load_first() {
    let state = OwnershipState::new();
    let hold = state.hold_task();
    let (_dir, path) = sqlite_path().await;
    let mut opened = open_harness_with(open_sqlite(&path).await, &state).await;
    let foreground = owned_child(&opened.root, &hold, "fg", false, false, false).await;
    let fg_child = opened
        .harness
        .conversation(foreground.child, context())
        .await
        .expect("conv")
        .expect("present");
    let deeper = owned_child(&fg_child, &hold, "fg2", false, false, false).await;
    let background = owned_child(&opened.root, &hold, "bg", true, false, false).await;
    state.open("fg", Ending::Completed);
    state.open("fg2", Ending::Completed);
    wait_until(|| async { status(&opened.harness, foreground.owner).await.status() == TaskStatus::Completing }).await;
    wait_until(|| async { status(&opened.harness, deeper.owner).await.status() == TaskStatus::Completing }).await;
    opened.harness.close(context()).await.expect("close 1");

    opened = open_harness_with(open_sqlite(&path).await, &state).await;
    let root_c = opened.root.clone();
    let root_idle = track(async move { root_c.wait_for_idle(context()).await });
    let harness_c = opened.harness.clone();
    let harness_idle = track(async move { harness_c.wait_for_idle(context()).await });
    assert!(!settled(&root_idle).await);
    assert!(!settled(&harness_idle).await);
    state.open("fg.inner", Ending::Completed);
    state.open("fg2.inner", Ending::Completed);
    root_idle.wait().await.expect("root idle");
    harness_idle.wait().await.expect("harness idle");
    assert_eq!(
        status(&opened.harness, foreground.owner).await.status(),
        TaskStatus::Terminal
    );
    assert_ne!(
        status(&opened.harness, background.inner).await.status(),
        TaskStatus::Terminal
    );
    opened
        .harness
        .abort_task(background.owner.erased(), context())
        .await
        .expect("abort bg");
    opened.harness.close(context()).await.expect("close 2");
}

async fn run_derives_unapplied_marks_at_open(abort_requested: bool, state_override: TaskState) {
    let state = OwnershipState::new();
    let hold = state.hold_task();
    let (_dir, path) = sqlite_path().await;
    let session = Session::new(open_sqlite(&path).await);
    let hold_c = hold.clone();
    let (owner, inner) = session
        .commit(
            move |tx| async move {
                let root = tx.create_conversation(ConversationOwnership::Ownerless).await?;
                let owner = tx
                    .create_task(
                        &hold_c,
                        &HoldInput {
                            name: "gone".to_owned(),
                            slow_abort: false,
                        },
                        TaskOptions {
                            ownership: TaskOwnership::Conversation,
                            conversation_id: Some(root.id),
                            background: false,
                        },
                    )
                    .await?;
                let child = tx
                    .create_conversation(ConversationOwnership::Task {
                        task_id: owner.erased(),
                    })
                    .await?;
                let inner = tx
                    .create_task(
                        &hold_c,
                        &HoldInput {
                            name: "orphan".to_owned(),
                            slow_abort: false,
                        },
                        TaskOptions {
                            ownership: TaskOwnership::Conversation,
                            conversation_id: Some(child.id),
                            background: false,
                        },
                    )
                    .await?;
                Ok((owner, inner))
            },
            context(),
        )
        .await
        .expect("seed tree");
    session
        .commit(
            move |tx| async move {
                let mut record = tx.task(owner.erased()).await?.expect("owner task");
                record.abort_requested = abort_requested;
                record.state = state_override;
                tx.as_transaction().set_task(record)?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("override owner");
    session.close(context()).await.expect("close session");

    let opened = open_harness_with(open_sqlite(&path).await, &state).await;
    assert!(matches!(
        opened
            .harness
            .wait_for_task(inner, context())
            .await
            .expect("wait")
            .outcome,
        TaskOutcome::Aborted { .. }
    ));
    let outcome = opened
        .harness
        .wait_for_task(owner, context())
        .await
        .expect("wait owner")
        .outcome;
    if abort_requested {
        assert!(matches!(outcome, TaskOutcome::Aborted { .. }));
    } else {
        assert!(matches!(outcome, TaskOutcome::Failed { .. }));
    }
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn derives_marks_a_crash_left_unapplied_below_a_held_failed_owner_at_open() {
    run_derives_unapplied_marks_at_open(
        false,
        TaskState::Completing {
            outcome: TaskOutcome::Failed {
                error: TaskOutcomeError {
                    message: "crash".to_owned(),
                    detail: None,
                },
                result: None,
            },
        },
    )
    .await;
}

#[tokio::test]
async fn derives_marks_a_crash_left_unapplied_below_an_abort_marked_owner_at_open() {
    run_derives_unapplied_marks_at_open(
        true,
        TaskState::Pending {
            checkpoint: serde_json::json!({ "phase": "hold" }),
        },
    )
    .await;
}

#[tokio::test]
async fn withdraws_an_input_queued_below_a_held_failed_owner_after_its_cascade() {
    let state = OwnershipState::new();
    let hold = state.hold_task();
    let opened = open_harness_with(Arc::new(MemoryStorage::new()), &state).await;
    let tree = owned_child(&opened.root, &hold, "owner", false, true, false).await;
    let child = opened
        .harness
        .conversation(tree.child, context())
        .await
        .expect("conv")
        .expect("present");
    busy(&child, tree.inner.erased()).await;
    state.open("owner", Ending::Failed);
    wait_until(|| async {
        opened
            .harness
            .get_task(tree.inner, context())
            .await
            .expect("get")
            .is_some_and(|t| t.abort_requested)
    })
    .await;
    let late = child
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("late".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    let settled = late.wait(context()).await.expect("wait");
    assert!(matches!(
        settled.record.state,
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, .. }) if reason == "aborted"
    ));
    state.open("abort.owner.inner", Ending::Completed);
    opened
        .harness
        .wait_for_task(tree.owner, context())
        .await
        .expect("wait owner");
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn marks_work_admitted_after_reopen_below_a_cancelled_owner_whose_edge_was_not_loaded() {
    let state = OwnershipState::new();
    let hold = state.hold_task();
    let (_dir, path) = sqlite_path().await;
    let mut opened = open_harness_with(open_sqlite(&path).await, &state).await;
    let tree = owned_child(&opened.root, &hold, "owner", false, false, true).await;
    state.open("owner.inner", Ending::Completed);
    opened
        .harness
        .wait_for_task(tree.inner, context())
        .await
        .expect("wait inner");
    opened
        .harness
        .abort_task(tree.owner.erased(), context())
        .await
        .expect("abort owner");
    opened.harness.close(context()).await.expect("close 1");

    opened = open_harness_with(open_sqlite(&path).await, &state).await;
    let child = opened
        .harness
        .conversation(tree.child, context())
        .await
        .expect("conv")
        .expect("present");
    let hold_c = hold.clone();
    let late = child
        .commit(
            move |tx| async move {
                tx.create_task(
                    &hold_c,
                    &HoldInput {
                        name: "late".to_owned(),
                        slow_abort: false,
                    },
                    TaskOptions {
                        ownership: TaskOwnership::Conversation,
                        conversation_id: None,
                        background: false,
                    },
                )
                .await
            },
            context(),
        )
        .await
        .expect("create late");
    assert!(matches!(
        opened
            .harness
            .wait_for_task(late, context())
            .await
            .expect("wait")
            .outcome,
        TaskOutcome::Aborted { .. }
    ));
    state.open("abort.owner", Ending::Completed);
    assert!(matches!(
        opened
            .harness
            .wait_for_task(tree.owner, context())
            .await
            .expect("wait")
            .outcome,
        TaskOutcome::Aborted { .. }
    ));
    opened.harness.close(context()).await.expect("close 2");
}

#[tokio::test]
async fn cascades_from_an_owner_the_scheduler_orphans() {
    let state = OwnershipState::new();
    let hold = state.hold_task();
    let unreg = unregistered_task();
    let opened = open_harness_with(Arc::new(MemoryStorage::new()), &state).await;
    let hold_c = hold.clone();
    let (owner, inner) = opened
        .root
        .commit(
            move |tx| async move {
                let owner = tx
                    .create_task(
                        &unreg,
                        &HoldInput {
                            name: "unregistered".to_owned(),
                            slow_abort: false,
                        },
                        TaskOptions {
                            ownership: TaskOwnership::Conversation,
                            conversation_id: None,
                            background: false,
                        },
                    )
                    .await?;
                let child = tx
                    .create_conversation(ConversationOwnership::Task {
                        task_id: owner.erased(),
                    })
                    .await?;
                let inner = tx
                    .create_task(
                        &hold_c,
                        &HoldInput {
                            name: "below".to_owned(),
                            slow_abort: false,
                        },
                        TaskOptions {
                            ownership: TaskOwnership::Conversation,
                            conversation_id: Some(child.id),
                            background: false,
                        },
                    )
                    .await?;
                Ok((owner, inner))
            },
            context(),
        )
        .await
        .expect("create");
    assert_eq!(
        opened
            .harness
            .abort_task(owner.erased(), context())
            .await
            .expect("abort"),
        AbortTaskResult::Marked
    );
    assert_eq!(
        opened
            .harness
            .wait_for_task(owner, context())
            .await
            .expect("wait")
            .outcome,
        TaskOutcome::Orphaned {
            reason: "missing_task".to_owned()
        }
    );
    assert!(matches!(
        opened
            .harness
            .wait_for_task(inner, context())
            .await
            .expect("wait")
            .outcome,
        TaskOutcome::Aborted { .. }
    ));
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn cancels_only_the_callers_wait_never_the_shared_work() {
    let state = OwnershipState::new();
    let hold = state.hold_task();
    let opened = open_harness_with(Arc::new(MemoryStorage::new()), &state).await;
    let tree = owned_child(&opened.root, &hold, "owner", false, false, false).await;
    let waiting = AbortController::new();
    let idle = opened
        .root
        .wait_for_idle(with_abort_signal(waiting.signal(), context()));
    waiting.abort(Some(pi_js::abort::AbortReason::error("Error", "stop waiting")));
    let idle_err = idle.await.expect_err("idle cancelled");
    assert!(idle_err.to_string().contains("stop waiting"));
    assert_ne!(status(&opened.harness, tree.inner).await.status(), TaskStatus::Terminal);

    let slow = owned_child(&opened.root, &hold, "slow", false, false, false).await;
    opened
        .root
        .commit(
            move |tx| async move {
                let mut record = tx.task(slow.owner.erased()).await?.expect("slow owner");
                record.input = serde_json::json!({ "name": "slow", "slowAbort": true });
                tx.as_transaction().set_task(record)?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("update slow");
    let aborting = AbortController::new();
    let root_c = opened.root.clone();
    let abort_sig = aborting.signal();
    let abort_fut = track(async move { root_c.abort(with_abort_signal(abort_sig, context()), None).await });
    wait_until(|| async {
        opened
            .harness
            .get_task(slow.owner, context())
            .await
            .expect("get")
            .is_some_and(|t| t.abort_requested)
    })
    .await;
    aborting.abort(Some(pi_js::abort::AbortReason::error("Error", "stop aborting")));
    let abort_err = abort_fut.wait().await.expect_err("abort cancelled");
    assert!(abort_err.to_string().contains("stop aborting"));
    state.open("abort.slow", Ending::Completed);
    for id in [tree.owner, tree.inner, slow.owner, slow.inner] {
        assert!(matches!(
            opened.harness.wait_for_task(id, context()).await.expect("wait").outcome,
            TaskOutcome::Aborted { .. }
        ));
    }
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn aborts_a_waiting_child_whose_awaited_task_completes_in_the_commit_that_marks_its_owner() {
    let state = OwnershipState::new();
    let hold = state.hold_task();
    let waiter = waiter_task();
    let opened = open_harness_with(Arc::new(MemoryStorage::new()), &state).await;
    let hold_c = hold.clone();
    let waiter_c = waiter.clone();
    let (owner, dependency, blocked) = opened
        .root
        .commit(
            move |tx| async move {
                let owner = tx
                    .create_task(
                        &hold_c,
                        &HoldInput {
                            name: "owner".to_owned(),
                            slow_abort: false,
                        },
                        TaskOptions {
                            ownership: TaskOwnership::Conversation,
                            conversation_id: None,
                            background: false,
                        },
                    )
                    .await?;
                let child = tx
                    .create_conversation(ConversationOwnership::Task {
                        task_id: owner.erased(),
                    })
                    .await?;
                let dependency = tx
                    .create_task(
                        &hold_c,
                        &HoldInput {
                            name: "dependency".to_owned(),
                            slow_abort: false,
                        },
                        TaskOptions {
                            ownership: TaskOwnership::Conversation,
                            conversation_id: None,
                            background: false,
                        },
                    )
                    .await?;
                let blocked = tx
                    .create_task(
                        &waiter_c,
                        &WaiterInput {
                            on: vec![dependency.erased()],
                        },
                        TaskOptions {
                            ownership: TaskOwnership::Conversation,
                            conversation_id: Some(child.id),
                            background: false,
                        },
                    )
                    .await?;
                Ok((owner, dependency, blocked))
            },
            context(),
        )
        .await
        .expect("setup");
    wait_until(|| async { state.runs_of("dependency") == 1 && state.runs_of("owner") == 1 }).await;
    wait_until(|| async { status(&opened.harness, blocked).await.status() == TaskStatus::Waiting }).await;
    opened
        .root
        .commit(
            move |tx| async move {
                let mut completed_dep = tx.task(dependency.erased()).await?.expect("dep");
                let mut marked = tx.task(owner.erased()).await?.expect("owner");
                completed_dep.state = TaskState::Terminal {
                    outcome: TaskOutcome::Completed {
                        result: JsonValue::Null,
                    },
                };
                marked.abort_requested = true;
                tx.as_transaction().set_task(completed_dep)?;
                tx.as_transaction().set_task(marked)?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("simultaneous commit");
    assert!(matches!(
        opened
            .harness
            .wait_for_task(blocked, context())
            .await
            .expect("wait")
            .outcome,
        TaskOutcome::Aborted { .. }
    ));
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn retries_marks_found_through_an_edge_loaded_after_reopen_when_their_commit_is_rejected() {
    let state = OwnershipState::new();
    let hold = state.hold_task();
    let (_dir, path) = sqlite_path().await;
    let mut opened = open_harness_with(open_sqlite(&path).await, &state).await;
    let tree = owned_child(&opened.root, &hold, "owner", false, false, true).await;
    state.open("owner.inner", Ending::Completed);
    opened
        .harness
        .wait_for_task(tree.inner, context())
        .await
        .expect("wait inner");
    opened
        .harness
        .abort_task(tree.owner.erased(), context())
        .await
        .expect("abort owner");
    opened.harness.close(context()).await.expect("close 1");

    let reject_mark = Arc::new(Mutex::new(None::<TaskId>));
    let wrapped: Arc<dyn Storage> = Arc::new(RejectOnceOnMarkStorage {
        inner: open_sqlite(&path).await,
        reject_mark: Arc::clone(&reject_mark),
    });
    opened = open_harness_with(wrapped, &state).await;
    let child = opened
        .harness
        .conversation(tree.child, context())
        .await
        .expect("conv")
        .expect("present");
    let hold_c = hold.clone();
    let reject_mark_c = Arc::clone(&reject_mark);
    let late = child
        .commit(
            move |tx| async move {
                let id = tx
                    .create_task(
                        &hold_c,
                        &HoldInput {
                            name: "late".to_owned(),
                            slow_abort: false,
                        },
                        TaskOptions {
                            ownership: TaskOwnership::Conversation,
                            conversation_id: None,
                            background: false,
                        },
                    )
                    .await?;
                *reject_mark_c.lock().unwrap_or_else(|e| e.into_inner()) = Some(id.erased());
                Ok(id)
            },
            context(),
        )
        .await
        .expect("create late");
    wait_until(|| async { reject_mark.lock().unwrap_or_else(|e| e.into_inner()).is_none() }).await;
    assert!(matches!(
        opened
            .harness
            .wait_for_task(late, context())
            .await
            .expect("wait late")
            .outcome,
        TaskOutcome::Aborted { .. }
    ));
    state.open("abort.owner", Ending::Completed);
    opened.harness.close(context()).await.expect("close 2");
}

#[tokio::test]
async fn retries_a_cascade_whose_commit_the_storage_rejected() {
    let state = OwnershipState::new();
    let hold = state.hold_task();
    let reject_mark = Arc::new(Mutex::new(None::<TaskId>));
    let storage: Arc<dyn Storage> = Arc::new(RejectOnceOnMarkStorage {
        inner: Arc::new(MemoryStorage::new()),
        reject_mark: Arc::clone(&reject_mark),
    });
    let opened = open_harness_with(storage, &state).await;
    let tree = owned_child(&opened.root, &hold, "owner", false, false, false).await;
    *reject_mark.lock().unwrap_or_else(|e| e.into_inner()) = Some(tree.inner.erased());
    state.open("owner", Ending::Failed);
    wait_until(|| async {
        opened
            .reports
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|err| err.downcast_ref::<StorageRejected>().is_some())
    })
    .await;
    assert_eq!(
        status(&opened.harness, tree.owner).await.status(),
        TaskStatus::Completing
    );
    assert_ne!(status(&opened.harness, tree.inner).await.status(), TaskStatus::Terminal);
    let root_id = opened.root.id();
    opened
        .root
        .commit(
            move |tx| async move {
                tx.append_entry(
                    root_id,
                    EntryDraft {
                        kind: "note".to_owned(),
                        model: None,
                        data: None,
                        head: None,
                        edits: None,
                    },
                )
                .await?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("retry commit");
    assert!(matches!(
        opened
            .harness
            .wait_for_task(tree.inner, context())
            .await
            .expect("wait")
            .outcome,
        TaskOutcome::Aborted { .. }
    ));
    assert!(matches!(
        opened
            .harness
            .wait_for_task(tree.owner, context())
            .await
            .expect("wait")
            .outcome,
        TaskOutcome::Failed { .. }
    ));
    opened.harness.close(context()).await.expect("close");
}

// ---- describe("owned conversations from tools and supervisors") ----

#[tokio::test]
async fn gives_a_tool_an_invocation_bound_handle_whose_submissions_stay_durable_after_the_call_ends() {
    let setup: ChatSetup = chat_setup(None);
    let captured_handle = Arc::new(Mutex::new(None::<ConversationHandle>));
    let captured_missing = Arc::new(Mutex::new(true));
    let captured_submission = Arc::new(Mutex::new(None::<pi_durable::harness::submissions::SubmissionHandle>));

    let handle_c = Arc::clone(&captured_handle);
    let missing_c = Arc::clone(&captured_missing);
    let sub_c = Arc::clone(&captured_submission);

    add_tool(
        &setup.registry,
        define_tool(ToolRegistration {
            name: "delegate".to_owned(),
            description: "Delegates".to_owned(),
            parameters: Type::object([] as [(&str, TSchema); 0]).to_value(),
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            output_limits: None,
            execute: Arc::new(move |_args, api, call_context| {
                let handle_c = Arc::clone(&handle_c);
                let missing_c = Arc::clone(&missing_c);
                let sub_c = Arc::clone(&sub_c);
                Box::pin(async move {
                    let task_id = api.task_id();
                    let child = api
                        .commit(
                            move |tx| async move {
                                let created = tx.create_conversation(ConversationOwnership::Task { task_id }).await?;
                                configure(
                                    &tx,
                                    created.id,
                                    &AgentChange {
                                        model: FieldChange::Set(ModelRef {
                                            provider: "faux".to_owned(),
                                            model_id: "faux-1".to_owned(),
                                        }),
                                        ..Default::default()
                                    },
                                )
                                .await?;
                                Ok(created.id)
                            },
                            call_context.clone(),
                        )
                        .await?;
                    let missing = api
                        .conversation(ConversationId::new_unchecked(99_999), call_context.clone())
                        .await?;
                    *missing_c.lock().unwrap_or_else(|e| e.into_inner()) = missing.is_some();
                    let handle = api
                        .conversation(child, call_context.clone())
                        .await?
                        .expect("child conversation handle");
                    let submission = handle
                        .submit(
                            InputSubmissionDraft {
                                request_id: Some("child".to_owned()),
                                content: UserInput::Text("child task".to_owned()),
                                when_busy: None,
                            },
                            call_context.clone(),
                        )
                        .await?;
                    let settled = submission.wait(call_context).await?;
                    *handle_c.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
                    *sub_c.lock().unwrap_or_else(|e| e.into_inner()) = Some(submission);
                    let status_label = match settled.record.state {
                        SubmissionState::Input(InputSubmissionState::Done { .. }) => "done",
                        _ => "other",
                    };
                    Ok(ToolExecutionResult {
                        content: Some(vec![ToolResultContentBlock::Text(TextContent::new(status_label))]),
                        ..Default::default()
                    })
                })
            }),
        }),
        None,
    )
    .expect("add tool");

    setup.faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "delegate",
                IndexMap::new(),
                Some(FauxToolCallOptions {
                    id: Some("c1".to_owned()),
                }),
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("child answer"))),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("done"))),
            None,
        )),
    ]);

    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open chat");
    let sub = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("go".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    sub.wait(context()).await.expect("wait");

    assert!(!*captured_missing.lock().unwrap_or_else(|e| e.into_inner()));
    let handle = captured_handle
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .expect("handle");
    let submission = captured_submission
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .expect("submission");

    assert!(
        match handle
            .submit(
                InputSubmissionDraft {
                    request_id: None,
                    content: UserInput::Text("again".to_owned()),
                    when_busy: None,
                },
                context(),
            )
            .await
        {
            Ok(_) => panic!("submit after end"),
            Err(err) => err,
        }
        .to_string()
        .contains("invocation has ended")
    );
    assert!(
        handle
            .wait_for_idle(context())
            .await
            .expect_err("waitForIdle after end")
            .to_string()
            .contains("invocation has ended")
    );
    assert!(
        handle
            .abort(context(), None)
            .await
            .expect_err("abort after end")
            .to_string()
            .contains("invocation has ended")
    );
    assert!(
        submission
            .wait(context())
            .await
            .expect_err("submission wait after end")
            .to_string()
            .contains("invocation has ended")
    );

    let child_conv = opened
        .harness
        .conversation(handle.id(), context())
        .await
        .expect("conv")
        .expect("present");
    let child_entries = child_conv
        .entries(ConversationEntriesQuery::default(), 100, None, context())
        .await
        .expect("entries")
        .items;
    assert_eq!(child_entries.iter().filter(|entry| entry.kind == "pi.user").count(), 1);
    let inspection = opened.harness.inspect(context()).await.expect("inspect");
    let child_tasks: Vec<_> = inspection
        .tasks
        .into_iter()
        .filter(|task| task.record.conversation_id == handle.id())
        .collect();
    assert!(child_tasks.is_empty());
    let rec = opened
        .harness
        .submission(submission.id(), context())
        .await
        .expect("get sub")
        .expect("present")
        .status(context())
        .await
        .expect("status");
    assert_eq!(rec.status(), pi_durable::types::SubmissionStatus::Done);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn rejects_a_handle_operation_queued_on_the_line_when_the_invocation_ends_before_it_runs() {
    let setup = chat_setup(None);
    let ready = deferred::<(ConversationId, TaskId)>();
    let go = deferred::<()>();
    let queued = deferred::<()>();
    let queued_slot = Arc::new(Mutex::new(
        None::<support::task_support::Tracked<pi_js::Result<pi_durable::harness::submissions::SubmissionHandle>>>,
    ));

    let ready_c = ready.clone();
    let go_c = go.clone();
    let queued_c = queued.clone();
    let queued_slot_for_tool = Arc::clone(&queued_slot);

    add_tool(
        &setup.registry,
        define_tool(ToolRegistration {
            name: "delegate".to_owned(),
            description: "Delegates late".to_owned(),
            parameters: Type::object([] as [(&str, TSchema); 0]).to_value(),
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            output_limits: None,
            execute: Arc::new(move |_args, api, call_context| {
                let ready_c = ready_c.clone();
                let go_c = go_c.clone();
                let queued_c = queued_c.clone();
                let queued_slot_c = Arc::clone(&queued_slot_for_tool);
                Box::pin(async move {
                    let task_id = api.task_id();
                    let child = api
                        .commit(
                            move |tx| async move {
                                let created = tx.create_conversation(ConversationOwnership::Task { task_id }).await?;
                                Ok(created.id)
                            },
                            call_context.clone(),
                        )
                        .await?;
                    let handle = api
                        .conversation(child, call_context.clone())
                        .await?
                        .expect("child handle");
                    ready_c.resolve((child, task_id));
                    go_c.wait().await?;
                    let handle_sub = handle.clone();
                    let submitting = track(async move {
                        handle_sub
                            .submit(
                                InputSubmissionDraft {
                                    request_id: None,
                                    content: UserInput::Text("late".to_owned()),
                                    when_busy: None,
                                },
                                context(),
                            )
                            .await
                    });
                    *queued_slot_c.lock().unwrap_or_else(|e| e.into_inner()) = Some(submitting);
                    queued_c.resolve(());
                    match call_context.abort_signal() {
                        Some(sig) => aborted(&sig).await,
                        None => Ok(ToolExecutionResult::default()),
                    }
                })
            }),
        }),
        None,
    )
    .expect("add tool");

    setup
        .faux
        .set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "delegate",
                IndexMap::new(),
                Some(FauxToolCallOptions {
                    id: Some("c1".to_owned()),
                }),
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        ))]);

    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open chat");
    opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("go".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    let (child, task_id) = ready.wait().await.expect("ready");

    let release = deferred::<()>();
    let release_c = release.clone();
    let root_c = opened.root.clone();
    let holding = track(async move {
        root_c
            .commit(
                move |_tx| async move {
                    let _ = release_c.wait().await;
                    Ok(())
                },
                context(),
            )
            .await
    });
    let harness_c = opened.harness.clone();
    let aborting = track(async move { harness_c.abort_task(task_id, context()).await });
    go.resolve(());
    queued.wait().await.expect("queued");
    let submitting = queued_slot
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .expect("submitting");
    release.resolve(());
    holding.wait().await.expect("holding");
    aborting.wait().await.expect("aborting");
    assert!(submitting.wait().await.is_err());

    let child_conv = opened
        .harness
        .conversation(child, context())
        .await
        .expect("conv")
        .expect("present");
    let child_entries = child_conv
        .entries(ConversationEntriesQuery::default(), 10, None, context())
        .await
        .expect("entries")
        .items;
    assert!(child_entries.is_empty());
    let inspection = opened.harness.inspect(context()).await.expect("inspect");
    let child_subs: Vec<_> = inspection
        .submissions
        .into_iter()
        .filter(|s| s.conversation_id == child)
        .collect();
    assert!(child_subs.is_empty());
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn aborts_a_subagent_run_with_its_parents_conversation_rejecting_the_waiting_tool() {
    let setup = chat_setup(None);
    let child_id_slot = Arc::new(Mutex::new(None::<ConversationId>));
    let tool_wait = deferred::<String>();

    let child_c = Arc::clone(&child_id_slot);
    let wait_c = tool_wait.clone();
    add_tool(
        &setup.registry,
        define_tool(ToolRegistration {
            name: "delegate".to_owned(),
            description: "Delegates".to_owned(),
            parameters: Type::object([] as [(&str, TSchema); 0]).to_value(),
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            output_limits: None,
            execute: Arc::new(move |_args, api, call_context| {
                let child_c = Arc::clone(&child_c);
                let wait_c = wait_c.clone();
                Box::pin(async move {
                    let task_id = api.task_id();
                    let child = api
                        .commit(
                            move |tx| async move {
                                let created = tx.create_conversation(ConversationOwnership::Task { task_id }).await?;
                                configure(
                                    &tx,
                                    created.id,
                                    &AgentChange {
                                        model: FieldChange::Set(ModelRef {
                                            provider: "faux".to_owned(),
                                            model_id: "faux-1".to_owned(),
                                        }),
                                        ..Default::default()
                                    },
                                )
                                .await?;
                                Ok(created.id)
                            },
                            call_context.clone(),
                        )
                        .await?;
                    *child_c.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
                    let handle = api
                        .conversation(child, call_context.clone())
                        .await?
                        .expect("child handle");
                    let submission = handle
                        .submit(
                            InputSubmissionDraft {
                                request_id: None,
                                content: UserInput::Text("child task".to_owned()),
                                when_busy: None,
                            },
                            call_context.clone(),
                        )
                        .await?;
                    match submission.wait(call_context).await {
                        Ok(settled) => {
                            let label = match settled.record.state {
                                SubmissionState::Input(InputSubmissionState::Done { .. }) => "done",
                                _ => "other",
                            };
                            Ok(ToolExecutionResult {
                                content: Some(vec![ToolResultContentBlock::Text(TextContent::new(label))]),
                                ..Default::default()
                            })
                        }
                        Err(err) => {
                            wait_c.resolve(err.to_string());
                            Err(err)
                        }
                    }
                })
            }),
        }),
        None,
    )
    .expect("add tool");

    let child_run = gated_response(faux_assistant_message(
        FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("never"))),
        None,
    ));
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "delegate",
                IndexMap::new(),
                Some(FauxToolCallOptions {
                    id: Some("c1".to_owned()),
                }),
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        child_run.step,
    ]);

    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open chat");
    let input = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("go".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");
    child_run.reached.wait().await.expect("child reached");
    opened.root.abort(context(), None).await.expect("abort root");
    let settled_input = input.wait(context()).await.expect("wait input");
    assert!(matches!(
        settled_input.record.state,
        SubmissionState::Input(InputSubmissionState::Unanswered { reason, .. }) if reason == "aborted"
    ));
    assert!(!tool_wait.wait().await.expect("tool wait").is_empty());

    let child = child_id_slot
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .expect("child id");
    let child_conv = opened
        .harness
        .conversation(child, context())
        .await
        .expect("conv")
        .expect("present");
    child_conv.wait_for_idle(context()).await.expect("child idle");
    let child_live = opened
        .harness
        .snapshot_conversation(&*LIVE_DOC, child, context())
        .await
        .expect("snapshot");
    assert_eq!(child_live, Some(Default::default()));
    let inspection = opened.harness.inspect(context()).await.expect("inspect");
    assert!(inspection.submissions.iter().all(|s| s.conversation_id != child));
    opened.harness.wait_for_idle(context()).await.expect("harness idle");
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn lets_a_running_tool_abort_its_owned_child_and_continue() {
    let setup = chat_setup(None);
    let child_run = gated_response(faux_assistant_message(
        FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("never"))),
        None,
    ));
    let reached_c = child_run.reached.clone();

    add_tool(
        &setup.registry,
        define_tool(ToolRegistration {
            name: "delegate".to_owned(),
            description: "Delegates and cancels".to_owned(),
            parameters: Type::object([] as [(&str, TSchema); 0]).to_value(),
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            output_limits: None,
            execute: Arc::new(move |_args, api, call_context| {
                let reached_c = reached_c.clone();
                Box::pin(async move {
                    let task_id = api.task_id();
                    let child = api
                        .commit(
                            move |tx| async move {
                                let created = tx.create_conversation(ConversationOwnership::Task { task_id }).await?;
                                configure(
                                    &tx,
                                    created.id,
                                    &AgentChange {
                                        model: FieldChange::Set(ModelRef {
                                            provider: "faux".to_owned(),
                                            model_id: "faux-1".to_owned(),
                                        }),
                                        ..Default::default()
                                    },
                                )
                                .await?;
                                Ok(created.id)
                            },
                            call_context.clone(),
                        )
                        .await?;
                    let handle = api
                        .conversation(child, call_context.clone())
                        .await?
                        .expect("child handle");
                    let submission = handle
                        .submit(
                            InputSubmissionDraft {
                                request_id: None,
                                content: UserInput::Text("child task".to_owned()),
                                when_busy: None,
                            },
                            call_context.clone(),
                        )
                        .await?;
                    reached_c.wait().await?;
                    handle.abort(call_context.clone(), None).await?;
                    let settled_child = submission.wait(call_context).await?;
                    let status_label = match settled_child.record.state {
                        SubmissionState::Input(InputSubmissionState::Unanswered { .. }) => "unanswered",
                        _ => "other",
                    };
                    Ok(ToolExecutionResult {
                        content: Some(vec![ToolResultContentBlock::Text(TextContent::new(format!(
                            "child {status_label}"
                        )))]),
                        ..Default::default()
                    })
                })
            }),
        }),
        None,
    )
    .expect("add tool");

    setup.faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "delegate",
                IndexMap::new(),
                Some(FauxToolCallOptions {
                    id: Some("c1".to_owned()),
                }),
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        child_run.step,
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("done"))),
            None,
        )),
    ]);

    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open chat");
    let settled = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("go".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit")
        .wait(context())
        .await
        .expect("wait");
    assert_eq!(settled.record.status(), pi_durable::types::SubmissionStatus::Done);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn aborts_the_children_of_a_tool_call_that_throws_or_is_interrupted_while_the_run_continues() {
    let state = OwnershipState::new();
    let hold = state.hold_task();
    let (_dir, path) = sqlite_path().await;
    let children = Arc::new(Mutex::new(Vec::<TaskId<()>>::new()));
    let tool_running = deferred::<()>();
    let setup = chat_setup(None);
    add_task(&setup.registry, hold.clone().into_any(), None).expect("add hold");

    let children_c = Arc::clone(&children);
    let running_c = tool_running.clone();
    add_tool(
        &setup.registry,
        define_tool(ToolRegistration {
            name: "spawn".to_owned(),
            description: "Starts owned work, then throws or hangs".to_owned(),
            parameters: Type::object([("mode", Type::string())]).to_value(),
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            output_limits: None,
            execute: Arc::new(move |args, api, call_context| {
                let hold_c = hold.clone();
                let children_c = Arc::clone(&children_c);
                let running_c = running_c.clone();
                Box::pin(async move {
                    let task_id = api.task_id();
                    let call_id = api.call_id().to_owned();
                    let child = api
                        .commit(
                            move |tx| async move {
                                let created = tx.create_conversation(ConversationOwnership::Task { task_id }).await?;
                                tx.create_task(
                                    &hold_c,
                                    &HoldInput {
                                        name: format!("child.{call_id}"),
                                        slow_abort: false,
                                    },
                                    TaskOptions {
                                        ownership: TaskOwnership::Conversation,
                                        conversation_id: Some(created.id),
                                        background: false,
                                    },
                                )
                                .await
                            },
                            call_context.clone(),
                        )
                        .await?;
                    children_c.lock().unwrap_or_else(|e| e.into_inner()).push(child);
                    if args.get("mode").and_then(|v| v.as_str()) == Some("throw") {
                        return Err(pi_js::Error::msg("spawn failed"));
                    }
                    running_c.resolve(());
                    match call_context.abort_signal() {
                        Some(sig) => aborted(&sig).await,
                        None => Ok(ToolExecutionResult::default()),
                    }
                })
            }),
        }),
        None,
    )
    .expect("add spawn");

    let call = |mode: &str, id: &str| {
        let mut args = IndexMap::new();
        args.insert("mode".to_owned(), JsonValue::String(mode.to_owned()));
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "spawn",
                args,
                Some(FauxToolCallOptions {
                    id: Some(id.to_owned()),
                }),
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        ))
    };

    setup.faux.set_responses(vec![
        call("throw", "c1"),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("after throw"))),
            None,
        )),
        call("hang", "c2"),
    ]);

    let mut opened = open_chat(open_sqlite(&path).await, &setup, OpenChatEnv::None)
        .await
        .expect("open 1");
    let first = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("one".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("one")
        .wait(context())
        .await
        .expect("wait one");
    assert_eq!(first.record.status(), pi_durable::types::SubmissionStatus::Done);
    let c0 = children.lock().unwrap_or_else(|e| e.into_inner())[0];
    assert!(matches!(
        opened.harness.wait_for_task(c0, context()).await.expect("wait").outcome,
        TaskOutcome::Aborted { .. }
    ));

    let second = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("two".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("two")
        .id();
    tool_running.wait().await.expect("tool running");
    opened.harness.close(context()).await.expect("close 1");

    setup
        .faux
        .set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("after interrupt"))),
            None,
        ))]);
    opened = open_chat(open_sqlite(&path).await, &setup, OpenChatEnv::None)
        .await
        .expect("open 2");
    opened.harness.resume().expect("resume");
    let c1 = children.lock().unwrap_or_else(|e| e.into_inner())[1];
    assert!(matches!(
        opened.harness.wait_for_task(c1, context()).await.expect("wait").outcome,
        TaskOutcome::Aborted { .. }
    ));
    let second_settled = opened
        .harness
        .submission(second, context())
        .await
        .expect("sub")
        .expect("present")
        .wait(context())
        .await
        .expect("wait two");
    assert_eq!(
        second_settled.record.status(),
        pi_durable::types::SubmissionStatus::Done
    );
    let tools = opened
        .harness
        .commit(
            |tx| async move {
                tx.scan_tasks(
                    TaskQuery {
                        kind: Some("pi.tool".to_owned()),
                        ..Default::default()
                    },
                    10,
                    None,
                )
                .await
            },
            context(),
        )
        .await
        .expect("scan tools")
        .items;
    let outcomes: Vec<_> = tools
        .into_iter()
        .map(|task| {
            matches!(
                task.state,
                TaskState::Terminal {
                    outcome: TaskOutcome::Failed { .. }
                }
            )
        })
        .collect();
    assert_eq!(outcomes, vec![true, true]);
    opened.harness.close(context()).await.expect("close 2");
}

#[tokio::test]
async fn reruns_a_replay_safe_subagent_tool_after_a_restart_with_the_same_child_and_submission() {
    let (_dir, path) = sqlite_path().await;
    let children = Arc::new(Mutex::new(Vec::<ConversationId>::new()));
    let setup = chat_setup(None);
    let children_c = Arc::clone(&children);
    add_tool(
        &setup.registry,
        define_tool(ToolRegistration {
            name: "subagent".to_owned(),
            description: "Delegates".to_owned(),
            parameters: Type::object([] as [(&str, TSchema); 0]).to_value(),
            replay: Some(ToolReplayMode::Safe),
            execution_mode: None,
            prepare_arguments: None,
            output_limits: None,
            execute: Arc::new(move |_args, api, call_context| {
                let children_c = Arc::clone(&children_c);
                Box::pin(async move {
                    let task_id = api.task_id();
                    let child = api
                        .commit(
                            move |tx| async move {
                                let existing = tx
                                    .scan_conversations(
                                        ConversationQuery {
                                            owner_conversation_id: None,
                                            owner_task_id: Some(task_id),
                                        },
                                        1,
                                        None,
                                    )
                                    .await?
                                    .items;
                                if let Some(found) = existing.first() {
                                    return Ok(found.id);
                                }
                                let created = tx.create_conversation(ConversationOwnership::Task { task_id }).await?;
                                configure(
                                    &tx,
                                    created.id,
                                    &AgentChange {
                                        model: FieldChange::Set(ModelRef {
                                            provider: "faux".to_owned(),
                                            model_id: "faux-1".to_owned(),
                                        }),
                                        ..Default::default()
                                    },
                                )
                                .await?;
                                Ok(created.id)
                            },
                            call_context.clone(),
                        )
                        .await?;
                    children_c.lock().unwrap_or_else(|e| e.into_inner()).push(child);
                    let handle = api
                        .conversation(child, call_context.clone())
                        .await?
                        .expect("child handle");
                    let submission = handle
                        .submit(
                            InputSubmissionDraft {
                                request_id: Some(format!("subagent:{task_id}")),
                                content: UserInput::Text("child task".to_owned()),
                                when_busy: None,
                            },
                            call_context.clone(),
                        )
                        .await?;
                    let settled = submission.wait(call_context).await?;
                    let status_label = match settled.record.state {
                        SubmissionState::Input(InputSubmissionState::Done { .. }) => "done",
                        _ => "other",
                    };
                    Ok(ToolExecutionResult {
                        content: Some(vec![ToolResultContentBlock::Text(TextContent::new(status_label))]),
                        ..Default::default()
                    })
                })
            }),
        }),
        None,
    )
    .expect("add tool");

    let child_run = gated_response(faux_assistant_message(
        FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("never"))),
        None,
    ));
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                "subagent",
                IndexMap::new(),
                Some(FauxToolCallOptions {
                    id: Some("c1".to_owned()),
                }),
            ))),
            Some(FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            }),
        )),
        child_run.step,
    ]);

    let mut opened = open_chat(open_sqlite(&path).await, &setup, OpenChatEnv::None)
        .await
        .expect("open 1");
    let input = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("go".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit")
        .id();
    child_run.reached.wait().await.expect("reached");
    opened.harness.close(context()).await.expect("close 1");

    setup.faux.set_responses(vec![
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("child answer"))),
            None,
        )),
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("done"))),
            None,
        )),
    ]);
    opened = open_chat(open_sqlite(&path).await, &setup, OpenChatEnv::None)
        .await
        .expect("open 2");
    let settled = opened
        .harness
        .submission(input, context())
        .await
        .expect("sub")
        .expect("present")
        .wait(context())
        .await
        .expect("wait");
    assert_eq!(settled.record.status(), pi_durable::types::SubmissionStatus::Done);
    let child_ids = children.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert_eq!(child_ids.len(), 2);
    assert_eq!(child_ids[1], child_ids[0]);
    let child = opened
        .harness
        .conversation(child_ids[0], context())
        .await
        .expect("conv")
        .expect("present");
    let entries = child
        .entries(ConversationEntriesQuery::default(), 100, None, context())
        .await
        .expect("entries")
        .items;
    assert_eq!(entries.iter().filter(|e| e.kind == "pi.user").count(), 1);
    let ctx_view = opened.root.context(context()).await.expect("context");
    let is_errors: Vec<bool> = ctx_view
        .messages
        .iter()
        .filter_map(|m| match m {
            Message::ToolResult(r) => Some(r.is_error),
            _ => None,
        })
        .collect();
    assert_eq!(is_errors, vec![false]);
    opened.harness.close(context()).await.expect("close 2");
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct ChildrenDoc {
    #[serde(default)]
    child: Option<ConversationId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SupervisorInput {
    parent: ConversationId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
enum SupervisorCheckpoint {
    Run,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SupervisorResult {
    answer: EntryId,
}

#[tokio::test]
async fn lets_a_background_supervisor_resubmit_after_a_restart_without_submitting_twice() {
    let (_dir, path) = sqlite_path().await;
    let children_doc: ConversationDocToken<ChildrenDoc> = define_conversation_doc(
        LatestFork::Initial.into(),
        DocDefinitionInput {
            kind: "test.children",
            version: 1,
            initial: ChildrenDoc::default,
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("children doc");

    let doc_c = children_doc.clone();
    let mut phases: IndexMap<String, TypedPhaseHandler<SupervisorInput, SupervisorCheckpoint, SupervisorResult, ()>> =
        IndexMap::new();
    phases.insert(
        "run".to_owned(),
        Arc::new(move |task, runtime, ctx| {
            let doc_c = doc_c.clone();
            Box::pin(async move {
                let id = runtime
                    .read()?
                    .snapshot_conversation(&doc_c, task.input.parent, ctx.clone())
                    .await?
                    .and_then(|d| d.child)
                    .expect("child present");
                let child = runtime.conversation(id, ctx.clone()).await?.expect("child handle");
                let submission = child
                    .submit(
                        InputSubmissionDraft {
                            request_id: Some("stable".to_owned()),
                            content: UserInput::Text("work".to_owned()),
                            when_busy: None,
                        },
                        ctx.clone(),
                    )
                    .await?;
                let settled = submission.wait(ctx.clone()).await?;
                let answer = match settled.record.state {
                    SubmissionState::Input(InputSubmissionState::Done { answer, .. }) => answer,
                    other => return Err(pi_js::Error::msg(format!("unexpected status: {other:?}"))),
                };
                runtime
                    .commit(
                        move |_tx, _current| {
                            Box::pin(async move {
                                Ok(Some(NextTaskState::Terminal {
                                    outcome: TaskOutcome::Completed {
                                        result: SupervisorResult { answer },
                                    },
                                }))
                            })
                        },
                        ctx,
                    )
                    .await
            })
        }),
    );

    let supervisor_task: Task<SupervisorInput, SupervisorCheckpoint, SupervisorResult, ()> =
        define_task(TaskDefinition {
            name: "test.supervisor".to_owned(),
            version: 1,
            initial: Arc::new(|_| Ok(SupervisorCheckpoint::Run)),
            phases,
            abort: Arc::new(|_task, runtime, ctx| {
                Box::pin(async move {
                    runtime
                        .commit(
                            |_tx, _current| {
                                Box::pin(async {
                                    Ok(Some(NextTaskState::Terminal {
                                        outcome: TaskOutcome::Aborted {
                                            reason: None,
                                            result: None,
                                        },
                                    }))
                                })
                            },
                            ctx,
                        )
                        .await
                })
            }),
            migrate: None,
            _hooks: std::marker::PhantomData,
        });

    let setup = chat_setup(None);
    add_task(&setup.registry, supervisor_task.clone().into_any(), None).expect("add supervisor");
    let first = gated_response(faux_assistant_message(
        FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("never"))),
        None,
    ));
    setup.faux.set_responses(vec![
        first.step,
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("answer"))),
            None,
        )),
    ]);

    let mut opened = open_chat(open_sqlite(&path).await, &setup, OpenChatEnv::None)
        .await
        .expect("open 1");
    let root_id = opened.root.id();
    let doc_c2 = children_doc.clone();
    let sup_c = supervisor_task.clone();
    let (supervisor, child) = opened
        .root
        .commit(
            move |tx| async move {
                let supervisor = tx
                    .create_task(
                        &sup_c,
                        &SupervisorInput { parent: root_id },
                        TaskOptions {
                            ownership: TaskOwnership::Conversation,
                            conversation_id: None,
                            background: true,
                        },
                    )
                    .await?;
                let created = tx
                    .create_conversation(ConversationOwnership::Task {
                        task_id: supervisor.erased(),
                    })
                    .await?;
                configure(
                    &tx,
                    created.id,
                    &AgentChange {
                        model: FieldChange::Set(ModelRef {
                            provider: "faux".to_owned(),
                            model_id: "faux-1".to_owned(),
                        }),
                        ..Default::default()
                    },
                )
                .await?;
                tx.doc_conversation(&doc_c2, root_id).await?.update(|d| {
                    d.child = Some(created.id);
                })?;
                Ok((supervisor, created.id))
            },
            context(),
        )
        .await
        .expect("setup supervisor");
    opened.harness.resume().expect("resume");
    first.reached.wait().await.expect("first reached");
    opened.harness.close(context()).await.expect("close 1");

    opened = open_chat(open_sqlite(&path).await, &setup, OpenChatEnv::None)
        .await
        .expect("open 2");
    opened.harness.resume().expect("resume 2");
    let done = opened
        .harness
        .wait_for_task(supervisor, context())
        .await
        .expect("wait supervisor");
    assert!(matches!(done.outcome, TaskOutcome::Completed { .. }));
    let child_conv = opened
        .harness
        .conversation(child, context())
        .await
        .expect("conv")
        .expect("present");
    let entries = child_conv
        .entries(ConversationEntriesQuery::default(), 100, None, context())
        .await
        .expect("entries")
        .items;
    assert_eq!(entries.iter().filter(|e| e.kind == "pi.user").count(), 1);
    opened.root.wait_for_idle(context()).await.expect("root idle");
    opened.harness.close(context()).await.expect("close 2");
}
