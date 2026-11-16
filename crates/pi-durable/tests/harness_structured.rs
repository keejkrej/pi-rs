//! Port of packages/durable/test/harness-structured.test.ts

mod support;

use std::future::Future;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use pi_ai::auth::types::ProviderAuth;
use pi_ai::models::Provider;
use pi_ai::models::create_models;
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseStep, FauxToolCallOptions,
    faux_assistant_message, faux_text, faux_tool_call,
};
use pi_ai::types::{
    ApiStreamOptions, AssistantMessage, AssistantMessageEvent, CancelDeferredFunction, DeferredCancelOptions,
    DeferredHandle, Message, Model, SimpleStreamFunction, SimpleStreamOptions, StopReason, TranscriptContext,
};
use pi_ai::utils::event_stream::{AssistantMessageEventStream, create_assistant_message_event_stream};
use pi_ai::{TSchema as Schema, Type as t};
use pi_chord::Context;
use pi_durable::AbortTaskResult;
use pi_durable::define_tool;
use pi_durable::documents::{DocDefinitionInput, TaskDocToken, define_task_doc};
use pi_durable::entries::TOOL_RESULT_ENTRY;
use pi_durable::errors::StorageRejected;
use pi_durable::harness::events::{AgentEvent, AgentEventStream};
use pi_durable::harness::generation::{GENERATION_TASK, GenerationResult};
use pi_durable::harness::harness::{Conversation, Harness};
use pi_durable::harness::live::{LIVE_DOC, LiveState, ToolSlotStatus};
use pi_durable::harness::registry::RegistryImpl;
use pi_durable::harness::types::{
    ConversationAbortOptions, ConversationCreateOptions, GenerationHooks, HarnessInspection, SubmissionDraft,
    TaskBlockedReason, TaskInspectionState, ToolExecutionMode, ToolExecutionResult, ToolRegistration, UserInput,
};
use pi_durable::session::session::create_session;
use pi_durable::session::transaction::Tx;
use pi_durable::storage::memory::MemoryStorage;
use pi_durable::storage::sqlite::node::{NodeSqliteStorageOptions, open_node_sqlite_storage};
use pi_durable::types::{
    ConversationId, ConversationOwnership, ConversationQuery, ConversationRecord, Cursor, DocumentAddress, DocumentId,
    DocumentPoint, DocumentQuery, DocumentRecord, EntryDraft, EntryId, EntryLookup, EntryQuery, EntryRecord,
    ErasedTaskRecord, InputSubmissionState, JoinPolicy, JsonValue, NextTaskState, Page, Seq, Storage, StorageWrite,
    StoredDocument, SubmissionId, SubmissionQuery, SubmissionRecord, SubmissionState, SubmissionStatus, TaskId,
    TaskOptions, TaskOutcome, TaskOwnership, TaskQuery, TaskState, TaskStatus,
};
use pi_js::BoxFuture;
use pretty_assertions::assert_eq;
use serde::{Deserialize, Serialize};
use support::chat_support::{OpenChatEnv, all_entries, chat_setup, open_chat, text_of, wait_for};
use support::harness_support::{Installed, add_hooks, add_task, add_tool};
use support::session_support::context;
use support::task_support::{Deferred, OpenTasksOptions, aborted, deferred, open_tasks, settled, track};
use tempfile::TempDir;

// ─── A scriptable task ──────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NodeInput {
    name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
enum NodeCheckpoint {
    Run,
    Resume { round: i64 },
}

type NodeRuntime = pi_durable::tasks::TaskRuntimeHandle<NodeInput, NodeCheckpoint, String, ()>;
type Next = NextTaskState<NodeCheckpoint, String>;
type NodeTask = pi_durable::tasks::Task<NodeInput, NodeCheckpoint, String, ()>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ending {
    Completed,
    Failed,
    Throw,
}

type RunBehaviorFn = Arc<dyn Fn(NodeRuntime, Context) -> BoxFuture<pi_js::Result<()>> + Send + Sync>;
type ResumeBehaviorFn = Arc<dyn Fn(NodeRuntime, Context, i64) -> BoxFuture<pi_js::Result<()>> + Send + Sync>;
type AbortBehaviorFn = Arc<dyn Fn(NodeRuntime, Context) -> BoxFuture<pi_js::Result<()>> + Send + Sync>;

#[derive(Clone, Default)]
struct Behavior {
    run: Option<RunBehaviorFn>,
    resume: Option<ResumeBehaviorFn>,
    abort: Option<AbortBehaviorFn>,
}

#[derive(Clone, Default)]
struct StructuredState {
    behaviors: Arc<Mutex<IndexMap<String, Behavior>>>,
    gates: Arc<Mutex<IndexMap<String, Deferred<Ending>>>>,
    log: Arc<Mutex<Vec<String>>>,
}

impl StructuredState {
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

    fn open_ok(&self, name: &str) {
        self.open(name, Ending::Completed);
    }

    fn script(&self, name: &str, behavior: Behavior) {
        self.behaviors
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.to_owned(), behavior);
    }

    fn push_log(&self, line: impl Into<String>) {
        self.log.lock().unwrap_or_else(|e| e.into_inner()).push(line.into());
    }

    fn logs(&self) -> Vec<String> {
        self.log.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn has_log(&self, line: &str) -> bool {
        self.log
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|entry| entry == line)
    }

    fn node(&self) -> NodeTask {
        let state_run = self.clone();
        let state_resume = self.clone();
        let state_abort = self.clone();

        let mut phases: IndexMap<String, pi_durable::tasks::TypedPhaseHandler<NodeInput, NodeCheckpoint, String, ()>> =
            IndexMap::new();
        phases.insert(
            "run".to_owned(),
            Arc::new(move |task, runtime, ctx| {
                let st = state_run.clone();
                Box::pin(async move {
                    let name = task.input.name.clone();
                    st.push_log(format!("run:{name}"));
                    let behavior = st
                        .behaviors
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&name)
                        .and_then(|b| b.run.clone());
                    match behavior {
                        Some(func) => func(runtime, ctx).await,
                        None => default_run(&st, &runtime, ctx, &name).await,
                    }
                })
            }),
        );
        phases.insert(
            "resume".to_owned(),
            Arc::new(move |task, runtime, ctx| {
                let st = state_resume.clone();
                Box::pin(async move {
                    let name = task.input.name.clone();
                    st.push_log(format!("resume:{name}"));
                    let round = match task.checkpoint {
                        NodeCheckpoint::Resume { round } => round,
                        NodeCheckpoint::Run => 0,
                    };
                    let behavior = st
                        .behaviors
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&name)
                        .and_then(|b| b.resume.clone());
                    match behavior {
                        Some(func) => func(runtime, ctx, round).await,
                        None => {
                            runtime
                                .commit(
                                    move |_tx, _current| {
                                        let name_c = name.clone();
                                        Box::pin(async move { Ok(Some(end_completed(&name_c))) })
                                    },
                                    ctx,
                                )
                                .await
                        }
                    }
                })
            }),
        );

        let abort: pi_durable::tasks::TypedAbortHandler<NodeInput, NodeCheckpoint, String, ()> =
            Arc::new(move |task, runtime, ctx| {
                let st = state_abort.clone();
                Box::pin(async move {
                    let name = task.input.name.clone();
                    st.push_log(format!("abort:{name}"));
                    let behavior = st
                        .behaviors
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&name)
                        .and_then(|b| b.abort.clone());
                    match behavior {
                        Some(func) => func(runtime, ctx).await,
                        None => {
                            runtime
                                .commit(
                                    move |_tx, _current| {
                                        let name_c = name.clone();
                                        Box::pin(async move {
                                            Ok(Some(NextTaskState::Terminal {
                                                outcome: TaskOutcome::Aborted {
                                                    reason: None,
                                                    result: Some(name_c),
                                                },
                                            }))
                                        })
                                    },
                                    ctx,
                                )
                                .await
                        }
                    }
                })
            });

        pi_durable::tasks::define_task(pi_durable::tasks::TaskDefinition {
            name: "test.node".to_owned(),
            version: 1,
            initial: Arc::new(|_| Ok(NodeCheckpoint::Run)),
            phases,
            abort,
            migrate: None,
            _hooks: std::marker::PhantomData,
        })
    }
}

async fn default_run(state: &StructuredState, runtime: &NodeRuntime, ctx: Context, name: &str) -> pi_js::Result<()> {
    let gate = state.gate(name);
    let gate_fut = gate.wait();
    let signal = runtime.signal()?;
    let ending = tokio::select! {
        res = gate_fut => res?,
        res = aborted::<Ending>(&signal) => return Err(res.expect_err("abort error")),
    };
    if ending == Ending::Throw {
        return Err(pi_js::Error::msg(format!("{name} threw")));
    }
    let next = end_with(ending, name);
    runtime
        .commit(
            move |_tx, _current| Box::pin(async move { Ok(Some(next.clone())) }),
            ctx,
        )
        .await
}

fn end_completed(name: &str) -> Next {
    NextTaskState::Terminal {
        outcome: TaskOutcome::Completed {
            result: name.to_owned(),
        },
    }
}

fn end_failed(name: &str) -> Next {
    NextTaskState::Terminal {
        outcome: TaskOutcome::Failed {
            error: pi_durable::types::TaskOutcomeError {
                message: format!("{name} failed"),
                detail: None,
            },
            result: None,
        },
    }
}

fn end_with(ending: Ending, name: &str) -> Next {
    match ending {
        Ending::Completed => end_completed(name),
        Ending::Failed => end_failed(name),
        Ending::Throw => unreachable!("Throw returns before end_with"),
    }
}

fn unregistered_task() -> pi_durable::tasks::Task<NodeInput, NodeCheckpoint, String, ()> {
    let mut phases: IndexMap<String, pi_durable::tasks::TypedPhaseHandler<NodeInput, NodeCheckpoint, String, ()>> =
        IndexMap::new();
    phases.insert("run".to_owned(), Arc::new(|_, _, _| Box::pin(async { Ok(()) })));
    pi_durable::tasks::define_task(pi_durable::tasks::TaskDefinition {
        name: "test.unregistered".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(NodeCheckpoint::Run)),
        phases,
        abort: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
        migrate: None,
        _hooks: std::marker::PhantomData,
    })
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct TaskNotes {
    text: String,
}

fn task_notes_doc() -> TaskDocToken<TaskNotes> {
    define_task_doc(DocDefinitionInput {
        kind: "test.task-notes",
        version: 1,
        initial: TaskNotes::default,
        migrate: None,
        checkpoint_when: None,
    })
    .expect("task-notes doc")
}

fn own_conversation() -> TaskOptions {
    TaskOptions {
        ownership: TaskOwnership::Conversation,
        conversation_id: None,
        background: false,
    }
}

fn owned(owner: TaskId) -> TaskOptions {
    TaskOptions {
        ownership: TaskOwnership::Task { task_id: owner },
        conversation_id: None,
        background: false,
    }
}

async fn spawn(tx: &Tx, node: &NodeTask, owner: TaskId, name: &str) -> pi_js::Result<TaskId<String>> {
    tx.create_task(node, &NodeInput { name: name.to_owned() }, owned(owner))
        .await
}

fn wait_on(on: &[TaskId], policy: JoinPolicy, round: i64) -> Next {
    NextTaskState::Waiting {
        checkpoint: NodeCheckpoint::Resume { round },
        on: on.to_vec(),
        policy,
    }
}

async fn start(
    conversation: &Conversation,
    node: &NodeTask,
    name: &str,
    options: TaskOptions,
) -> pi_js::Result<TaskId<String>> {
    let node_c = node.clone();
    let name_c = name.to_owned();
    conversation
        .commit(
            move |tx| {
                let node_c = node_c.clone();
                let name_c = name_c.clone();
                async move { tx.create_task(&node_c, &NodeInput { name: name_c }, options).await }
            },
            context(),
        )
        .await
}

struct OpenedNodes {
    harness: Harness,
    root: Conversation,
    registry: Arc<RegistryImpl>,
    reports: Arc<Mutex<Vec<pi_js::Error>>>,
}

async fn open_nodes(state: &StructuredState, storage: Option<Arc<dyn Storage>>) -> OpenedNodes {
    let storage = storage.unwrap_or_else(|| Arc::new(MemoryStorage::new()));
    let opened = open_tasks(storage, vec![state.node().into_any()], OpenTasksOptions::default())
        .await
        .expect("open tasks");
    let root = opened.harness.root(context(), None).await.expect("root");
    opened.harness.resume().expect("resume");
    OpenedNodes {
        harness: opened.harness,
        root,
        registry: opened.registry,
        reports: opened.reports,
    }
}

async fn task_state(harness: &Harness, id: TaskId) -> TaskState {
    harness
        .get_erased_task(id, context())
        .await
        .expect("get_erased_task")
        .expect("task present")
        .state
}

async fn outcome_of(harness: &Harness, id: TaskId) -> &'static str {
    let rec = harness
        .wait_for_task::<JsonValue>(id.cast(), context())
        .await
        .expect("wait_for_task");
    outcome_status(&rec.outcome)
}

fn outcome_status<R>(outcome: &TaskOutcome<R>) -> &'static str {
    match outcome {
        TaskOutcome::Completed { .. } => "completed",
        TaskOutcome::Failed { .. } => "failed",
        TaskOutcome::Faulted { .. } => "faulted",
        TaskOutcome::Aborted { .. } => "aborted",
        TaskOutcome::Orphaned { .. } => "orphaned",
    }
}

async fn until<F, Fut>(check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    wait_for(check, 5_000).await;
}

async fn sqlite_path() -> (TempDir, String) {
    let dir = tempfile::Builder::new()
        .prefix("pi-durable-structured-")
        .tempdir()
        .expect("tempdir");
    let path = dir.path().join("session.sqlite").to_string_lossy().into_owned();
    (dir, path)
}

fn empty_schema() -> JsonValue {
    t::object([] as [(&str, Schema); 0]).to_value()
}

// ─── Ownership ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn creates_a_child_in_its_owners_conversation_with_its_owner_recorded() {
    let st = StructuredState::new();
    let node = st.node();
    let child_slot = Arc::new(Mutex::new(None::<TaskId<String>>));
    let child_cb = Arc::clone(&child_slot);
    let node_cb = node.clone();
    st.script(
        "parent",
        Behavior {
            run: Some(Arc::new(move |runtime, ctx| {
                let child_cb = Arc::clone(&child_cb);
                let node_cb = node_cb.clone();
                Box::pin(async move {
                    let owner = runtime.task_id()?.erased();
                    runtime
                        .commit(
                            move |tx, _current| {
                                let child_cb = Arc::clone(&child_cb);
                                let node_cb = node_cb.clone();
                                Box::pin(async move {
                                    let child = spawn(&tx, &node_cb, owner, "child").await?;
                                    *child_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
                                    Ok(Some(wait_on(&[child.erased()], JoinPolicy::AllSettled, 1)))
                                })
                            },
                            ctx,
                        )
                        .await
                })
            })),
            ..Default::default()
        },
    );

    let opened = open_nodes(&st, None).await;
    let parent = start(&opened.root, &node, "parent", own_conversation())
        .await
        .expect("start parent");
    until(|| {
        let child_slot = Arc::clone(&child_slot);
        async move { child_slot.lock().unwrap_or_else(|e| e.into_inner()).is_some() }
    })
    .await;
    let child = child_slot.lock().unwrap_or_else(|e| e.into_inner()).expect("child");
    let record = opened
        .harness
        .get_erased_task(child.erased(), context())
        .await
        .expect("get_erased_task")
        .expect("record");
    assert_eq!(record.owner, Some(parent.erased()));
    assert_eq!(record.conversation_id, opened.root.id());
    assert!(!record.background);
    let parent_rec = opened
        .harness
        .get_erased_task(parent.erased(), context())
        .await
        .expect("get parent")
        .expect("parent record");
    assert_eq!(parent_rec.owner, None);

    st.open_ok("child");
    assert_eq!(outcome_of(&opened.harness, parent.erased()).await, "completed");
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn rejects_missing_owner_wrong_conversation_and_background_child() {
    let st = StructuredState::new();
    let node = st.node();
    let opened = open_nodes(&st, None).await;
    let parent = start(&opened.root, &node, "parent", own_conversation())
        .await
        .expect("start parent");
    let other = opened
        .harness
        .create_conversation(
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            context(),
        )
        .await
        .expect("other");

    let missing_err = opened
        .root
        .commit(
            {
                let node_c = node.clone();
                move |tx| async move {
                    tx.create_task(
                        &node_c,
                        &NodeInput { name: "x".to_owned() },
                        owned(TaskId::new_unchecked(999_999)),
                    )
                    .await
                }
            },
            context(),
        )
        .await
        .expect_err("missing owner");
    assert!(missing_err.message().contains("does not exist"));

    let conv_err = opened
        .root
        .commit(
            {
                let node_c = node.clone();
                let other_id = other.id();
                move |tx| async move {
                    let mut opts = owned(parent.erased());
                    opts.conversation_id = Some(other_id);
                    tx.create_task(&node_c, &NodeInput { name: "x".to_owned() }, opts).await
                }
            },
            context(),
        )
        .await
        .expect_err("other conversation");
    assert!(conv_err.message().contains("owner's conversation"));

    let bg_err = opened
        .root
        .commit(
            {
                let node_c = node.clone();
                move |tx| async move {
                    let mut opts = owned(parent.erased());
                    opts.background = true;
                    tx.create_task(&node_c, &NodeInput { name: "x".to_owned() }, opts).await
                }
            },
            context(),
        )
        .await
        .expect_err("background child");
    assert!(bg_err.message().contains("cannot be background"));

    // The owner's conversation is the default, even from a commit bound to another conversation.
    let child = other
        .commit(
            {
                let node_c = node.clone();
                move |tx| async move { spawn(&tx, &node_c, parent.erased(), "child").await }
            },
            context(),
        )
        .await
        .expect("create child from other");
    let child_rec = opened
        .harness
        .get_erased_task(child.erased(), context())
        .await
        .expect("get child")
        .expect("record");
    assert_eq!(child_rec.conversation_id, opened.root.id());

    st.open_ok("parent");
    st.open_ok("child");
    opened.harness.wait_for_task(parent, context()).await.expect("wait");
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn rejects_new_owned_work_below_completing_terminal_or_abort_marked_owner() {
    let st = StructuredState::new();
    let node = st.node();
    let node_cb = node.clone();
    st.script(
        "parent",
        Behavior {
            run: Some(Arc::new(move |runtime, ctx| {
                let node_cb = node_cb.clone();
                Box::pin(async move {
                    let owner = runtime.task_id()?.erased();
                    runtime
                        .commit(
                            move |tx, _current| {
                                let node_cb = node_cb.clone();
                                Box::pin(async move {
                                    let _ = spawn(&tx, &node_cb, owner, "child").await?;
                                    Ok(Some(NextTaskState::Running {
                                        checkpoint: NodeCheckpoint::Resume { round: 1 },
                                    }))
                                })
                            },
                            ctx,
                        )
                        .await
                })
            })),
            ..Default::default()
        },
    );

    let opened = open_nodes(&st, None).await;
    let parent = start(&opened.root, &node, "parent", own_conversation())
        .await
        .expect("parent");
    until(|| {
        let harness = opened.harness.clone();
        async move { task_state(&harness, parent.erased()).await.status() == TaskStatus::Completing }
    })
    .await;

    let err1 = start(&opened.root, &node, "late", owned(parent.erased()))
        .await
        .expect_err("completing rejects child");
    assert!(err1.message().contains("is completing"));

    let conv_err1 = opened
        .root
        .commit(
            move |tx| async move {
                tx.create_conversation(ConversationOwnership::Task {
                    task_id: parent.erased(),
                })
                .await
            },
            context(),
        )
        .await
        .expect_err("completing rejects conversation");
    assert!(conv_err1.message().contains("is completing"));

    st.open_ok("child");
    opened.harness.wait_for_task(parent, context()).await.expect("wait");

    let err2 = start(&opened.root, &node, "late", owned(parent.erased()))
        .await
        .expect_err("terminal rejects child");
    assert!(err2.message().contains("is terminal"));

    let conv_err2 = opened
        .root
        .commit(
            move |tx| async move {
                tx.create_conversation(ConversationOwnership::Task {
                    task_id: parent.erased(),
                })
                .await
            },
            context(),
        )
        .await
        .expect_err("terminal rejects conversation");
    assert!(conv_err2.message().contains("is terminal"));

    let st_slow = st.clone();
    st.script(
        "slow",
        Behavior {
            abort: Some(Arc::new(move |runtime, ctx| {
                let st_slow = st_slow.clone();
                Box::pin(async move {
                    let _ = st_slow.gate("abort.slow").wait().await?;
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
            })),
            ..Default::default()
        },
    );
    let slow = start(&opened.root, &node, "slow", own_conversation())
        .await
        .expect("slow");
    until(|| {
        let st = st.clone();
        async move { st.has_log("run:slow") }
    })
    .await;
    opened
        .harness
        .abort_task(slow.erased(), context())
        .await
        .expect("abort slow");

    let abort_err = start(&opened.root, &node, "late", owned(slow.erased()))
        .await
        .expect_err("abort-marked rejects child");
    assert!(abort_err.message().contains("is abort-marked"));

    st.open_ok("abort.slow");
    assert_eq!(outcome_of(&opened.harness, slow.erased()).await, "aborted");
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn cannot_create_child_in_finishing_commit_but_work_in_owned_conversation_holds_it() {
    let st = StructuredState::new();
    let node = st.node();
    let node_eager = node.clone();
    st.script(
        "eager",
        Behavior {
            run: Some(Arc::new(move |runtime, ctx| {
                let node_eager = node_eager.clone();
                Box::pin(async move {
                    let owner = runtime.task_id()?.erased();
                    runtime
                        .commit(
                            move |tx, _current| {
                                let node_eager = node_eager.clone();
                                Box::pin(async move {
                                    let _ = spawn(&tx, &node_eager, owner, "never").await?;
                                    Ok(Some(end_completed("eager")))
                                })
                            },
                            ctx,
                        )
                        .await
                })
            })),
            ..Default::default()
        },
    );

    let opened = open_nodes(&st, None).await;
    let eager = start(&opened.root, &node, "eager", own_conversation())
        .await
        .expect("eager");
    let eager_rec = opened
        .harness
        .wait_for_task(eager, context())
        .await
        .expect("wait eager");
    match eager_rec.outcome {
        TaskOutcome::Faulted { error } => assert!(error.message.contains("is completing")),
        other => panic!("expected Faulted, got {other:?}"),
    }
    let nodes = opened
        .harness
        .commit(
            |tx| async move {
                tx.scan_tasks(
                    TaskQuery {
                        kind: Some("test.node".to_owned()),
                        ..Default::default()
                    },
                    20,
                    None,
                )
                .await
            },
            context(),
        )
        .await
        .expect("scan");
    let names: Vec<String> = nodes
        .items
        .into_iter()
        .map(|task| serde_json::from_value::<NodeInput>(task.input).expect("input").name)
        .collect();
    assert_eq!(names, vec!["eager"]);

    let child_conv = Arc::new(Mutex::new(None::<ConversationId>));
    let child_run = Arc::clone(&child_conv);
    let child_resume = Arc::clone(&child_conv);
    let node_host = node.clone();
    st.script(
        "host",
        Behavior {
            run: Some(Arc::new(move |runtime, ctx| {
                let child_run = Arc::clone(&child_run);
                Box::pin(async move {
                    let owner = runtime.task_id()?.erased();
                    runtime
                        .commit(
                            move |tx, _current| {
                                let child_run = Arc::clone(&child_run);
                                Box::pin(async move {
                                    let conv = tx
                                        .create_conversation(ConversationOwnership::Task { task_id: owner })
                                        .await?;
                                    *child_run.lock().unwrap_or_else(|e| e.into_inner()) = Some(conv.id);
                                    Ok(Some(NextTaskState::Running {
                                        checkpoint: NodeCheckpoint::Resume { round: 1 },
                                    }))
                                })
                            },
                            ctx,
                        )
                        .await
                })
            })),
            resume: Some(Arc::new(move |runtime, ctx, _| {
                let child_resume = Arc::clone(&child_resume);
                let node_host = node_host.clone();
                Box::pin(async move {
                    let cid = child_resume
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .expect("child conv");
                    runtime
                        .commit(
                            move |tx, _current| {
                                let node_host = node_host.clone();
                                Box::pin(async move {
                                    let mut opts = own_conversation();
                                    opts.conversation_id = Some(cid);
                                    let _ = tx
                                        .create_task(
                                            &node_host,
                                            &NodeInput {
                                                name: "inner".to_owned(),
                                            },
                                            opts,
                                        )
                                        .await?;
                                    Ok(Some(end_completed("host")))
                                })
                            },
                            ctx,
                        )
                        .await
                })
            })),
            ..Default::default()
        },
    );

    let host = start(&opened.root, &node, "host", own_conversation())
        .await
        .expect("host");
    until(|| {
        let harness = opened.harness.clone();
        async move { task_state(&harness, host.erased()).await.status() == TaskStatus::Completing }
    })
    .await;
    let h_c = opened.harness.clone();
    let waiter = track(async move { h_c.wait_for_task(host, context()).await });
    assert!(!settled(&waiter).await);
    st.open_ok("inner");
    let host_settled = waiter.wait().await.expect("wait host");
    assert_eq!(outcome_status(&host_settled.outcome), "completed");
    opened.harness.close(context()).await.expect("close");
}

// ─── Waiting ────────────────────────────────────────────────────────────────

fn parent_of(
    st: &StructuredState,
    node: &NodeTask,
    parent: &str,
    children: &[&str],
    policy: JoinPolicy,
) -> (Arc<Mutex<Vec<TaskId>>>, Arc<Mutex<Vec<&'static str>>>) {
    let ids = Arc::new(Mutex::new(Vec::<TaskId>::new()));
    let outcomes = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let child_names: Vec<String> = children.iter().map(|s| (*s).to_owned()).collect();

    let ids_run = Arc::clone(&ids);
    let node_run = node.clone();
    let names_run = child_names.clone();
    let ids_res = Arc::clone(&ids);
    let out_res = Arc::clone(&outcomes);
    let parent_name = parent.to_owned();

    st.script(
        parent,
        Behavior {
            run: Some(Arc::new(move |runtime, ctx| {
                let ids_run = Arc::clone(&ids_run);
                let node_run = node_run.clone();
                let names_run = names_run.clone();
                Box::pin(async move {
                    let owner = runtime.task_id()?.erased();
                    runtime
                        .commit(
                            move |tx, _current| {
                                let ids_run = Arc::clone(&ids_run);
                                let node_run = node_run.clone();
                                let names_run = names_run.clone();
                                Box::pin(async move {
                                    let mut created = Vec::with_capacity(names_run.len());
                                    for name in &names_run {
                                        created.push(spawn(&tx, &node_run, owner, name).await?.erased());
                                    }
                                    *ids_run.lock().unwrap_or_else(|e| e.into_inner()) = created.clone();
                                    Ok(Some(wait_on(&created, policy, 1)))
                                })
                            },
                            ctx,
                        )
                        .await
                })
            })),
            resume: Some(Arc::new(move |runtime, ctx, _| {
                let ids_res = Arc::clone(&ids_res);
                let out_res = Arc::clone(&out_res);
                let parent_name = parent_name.clone();
                Box::pin(async move {
                    let awaited = ids_res.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    let fetched = runtime.outcomes::<JsonValue>(&awaited, ctx.clone()).await?;
                    let statuses: Vec<&'static str> = fetched.iter().map(outcome_status).collect();
                    *out_res.lock().unwrap_or_else(|e| e.into_inner()) = statuses;
                    runtime
                        .commit(
                            move |_tx, _current| {
                                let parent_name = parent_name.clone();
                                Box::pin(async move { Ok(Some(end_completed(&parent_name))) })
                            },
                            ctx,
                        )
                        .await
                })
            })),
            abort: None,
        },
    );
    (ids, outcomes)
}

#[tokio::test]
async fn resumes_once_every_awaited_task_is_terminal_and_reads_outcomes_in_order_all_settled() {
    let st = StructuredState::new();
    let node = st.node();
    let unreg = unregistered_task();
    let ids = Arc::new(Mutex::new(Vec::<TaskId>::new()));
    let outcomes = Arc::new(Mutex::new(Vec::<&'static str>::new()));

    let ids_run = Arc::clone(&ids);
    let node_run = node.clone();
    let unreg_run = unreg.clone();
    let ids_res = Arc::clone(&ids);
    let out_res = Arc::clone(&outcomes);
    st.script(
        "parent",
        Behavior {
            run: Some(Arc::new(move |runtime, ctx| {
                let ids_run = Arc::clone(&ids_run);
                let node_run = node_run.clone();
                let unreg_run = unreg_run.clone();
                Box::pin(async move {
                    let owner = runtime.task_id()?.erased();
                    runtime
                        .commit(
                            move |tx, _current| {
                                let ids_run = Arc::clone(&ids_run);
                                let node_run = node_run.clone();
                                let unreg_run = unreg_run.clone();
                                Box::pin(async move {
                                    let mut created = Vec::new();
                                    for name in ["ok", "fails", "throws", "aborted"] {
                                        created.push(spawn(&tx, &node_run, owner, name).await?.erased());
                                    }
                                    let orphan = tx
                                        .create_task(
                                            &unreg_run,
                                            &NodeInput {
                                                name: "orphan".to_owned(),
                                            },
                                            owned(owner),
                                        )
                                        .await?
                                        .erased();
                                    created.push(orphan);
                                    *ids_run.lock().unwrap_or_else(|e| e.into_inner()) = created.clone();
                                    Ok(Some(wait_on(&created, JoinPolicy::AllSettled, 1)))
                                })
                            },
                            ctx,
                        )
                        .await
                })
            })),
            resume: Some(Arc::new(move |runtime, ctx, _| {
                let ids_res = Arc::clone(&ids_res);
                let out_res = Arc::clone(&out_res);
                Box::pin(async move {
                    let awaited = ids_res.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    let fetched = runtime.outcomes::<JsonValue>(&awaited, ctx.clone()).await?;
                    *out_res.lock().unwrap_or_else(|e| e.into_inner()) = fetched.iter().map(outcome_status).collect();
                    runtime
                        .commit(
                            |_tx, _current| Box::pin(async { Ok(Some(end_completed("parent"))) }),
                            ctx,
                        )
                        .await
                })
            })),
            abort: None,
        },
    );

    let opened = open_nodes(&st, None).await;
    let parent = start(&opened.root, &node, "parent", own_conversation())
        .await
        .expect("parent");
    until(|| {
        let ids = Arc::clone(&ids);
        async move { ids.lock().unwrap_or_else(|e| e.into_inner()).len() == 5 }
    })
    .await;
    until(|| {
        let harness = opened.harness.clone();
        async move { task_state(&harness, parent.erased()).await.status() == TaskStatus::Waiting }
    })
    .await;

    let id_list = ids.lock().unwrap_or_else(|e| e.into_inner()).clone();
    st.open_ok("ok");
    st.open("fails", Ending::Failed);
    st.open("throws", Ending::Throw);
    opened.harness.abort_task(id_list[3], context()).await.expect("abort 3");
    let res4 = opened.harness.abort_task(id_list[4], context()).await.expect("abort 4");
    assert_eq!(res4, AbortTaskResult::Marked);
    assert_eq!(outcome_of(&opened.harness, parent.erased()).await, "completed");
    assert_eq!(
        *outcomes.lock().unwrap_or_else(|e| e.into_inner()),
        vec!["completed", "failed", "faulted", "aborted", "orphaned"]
    );
    let aborts: Vec<String> = st
        .logs()
        .into_iter()
        .filter(|line| line.starts_with("abort:"))
        .collect();
    assert_eq!(aborts, vec!["abort:aborted"]);
    opened.harness.close(context()).await.expect("close");
}

async fn run_fail_fast_case(ending: Ending, expected_status: &str) {
    let st = StructuredState::new();
    let node = st.node();
    let (_ids, outcomes) = parent_of(&st, &node, "checkout", &["p1", "p2", "p3", "p4"], JoinPolicy::FailFast);
    let opened = open_nodes(&st, None).await;
    let parent = start(&opened.root, &node, "checkout", own_conversation())
        .await
        .expect("parent");
    until(|| {
        let harness = opened.harness.clone();
        async move { task_state(&harness, parent.erased()).await.status() == TaskStatus::Waiting }
    })
    .await;
    st.open("p2", ending);
    assert_eq!(outcome_of(&opened.harness, parent.erased()).await, "completed");
    assert_eq!(
        *outcomes.lock().unwrap_or_else(|e| e.into_inner()),
        vec!["aborted", expected_status, "aborted", "aborted"]
    );
    assert!(
        !opened
            .harness
            .get_erased_task(parent.erased(), context())
            .await
            .expect("get parent")
            .expect("record")
            .abort_requested
    );
    let mut aborts: Vec<String> = st
        .logs()
        .into_iter()
        .filter(|line| line.starts_with("abort:"))
        .collect();
    aborts.sort();
    assert_eq!(aborts, vec!["abort:p1", "abort:p3", "abort:p4"]);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn fails_fast_when_a_child_ends_failed_aborts_siblings_not_parent() {
    run_fail_fast_case(Ending::Failed, "failed").await;
}

#[tokio::test]
async fn fails_fast_when_a_child_ends_faulted_aborts_siblings_not_parent() {
    run_fail_fast_case(Ending::Throw, "faulted").await;
}

#[tokio::test]
async fn fails_fast_on_a_held_failure_before_the_failing_child_drains() {
    let st = StructuredState::new();
    let node = st.node();
    let ids = Arc::new(Mutex::new(Vec::<TaskId>::new()));
    let outside = Arc::new(Mutex::new(None::<TaskId>));

    let node_p1 = node.clone();
    st.script(
        "p1",
        Behavior {
            run: Some(Arc::new(move |runtime, ctx| {
                let node_p1 = node_p1.clone();
                Box::pin(async move {
                    let owner = runtime.task_id()?.erased();
                    runtime
                        .commit(
                            move |tx, _current| {
                                let node_p1 = node_p1.clone();
                                Box::pin(async move {
                                    let _ = spawn(&tx, &node_p1, owner, "grandchild").await?;
                                    Ok(Some(NextTaskState::Running {
                                        checkpoint: NodeCheckpoint::Resume { round: 1 },
                                    }))
                                })
                            },
                            ctx,
                        )
                        .await
                })
            })),
            resume: Some(Arc::new(|runtime, ctx, _| {
                Box::pin(async move {
                    runtime
                        .commit(|_tx, _current| Box::pin(async { Ok(Some(end_failed("p1"))) }), ctx)
                        .await
                })
            })),
            abort: None,
        },
    );

    let st_gc = st.clone();
    st.script(
        "grandchild",
        Behavior {
            abort: Some(Arc::new(move |runtime, ctx| {
                let st_gc = st_gc.clone();
                Box::pin(async move {
                    let _ = st_gc.gate("abort.grandchild").wait().await?;
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
            })),
            ..Default::default()
        },
    );

    let ids_parent = Arc::clone(&ids);
    let out_parent = Arc::clone(&outside);
    let node_parent = node.clone();
    st.script(
        "parent",
        Behavior {
            run: Some(Arc::new(move |runtime, ctx| {
                let ids_parent = Arc::clone(&ids_parent);
                let out_parent = Arc::clone(&out_parent);
                let node_parent = node_parent.clone();
                Box::pin(async move {
                    let owner = runtime.task_id()?.erased();
                    runtime
                        .commit(
                            move |tx, _current| {
                                let ids_parent = Arc::clone(&ids_parent);
                                let out_parent = Arc::clone(&out_parent);
                                let node_parent = node_parent.clone();
                                Box::pin(async move {
                                    let mut created = Vec::new();
                                    for name in ["p1", "p2"] {
                                        created.push(spawn(&tx, &node_parent, owner, name).await?.erased());
                                    }
                                    let out = spawn(&tx, &node_parent, owner, "outside").await?.erased();
                                    *ids_parent.lock().unwrap_or_else(|e| e.into_inner()) = created.clone();
                                    *out_parent.lock().unwrap_or_else(|e| e.into_inner()) = Some(out);
                                    Ok(Some(wait_on(&created, JoinPolicy::FailFast, 1)))
                                })
                            },
                            ctx,
                        )
                        .await
                })
            })),
            ..Default::default()
        },
    );

    let opened = open_nodes(&st, None).await;
    let parent = start(&opened.root, &node, "parent", own_conversation())
        .await
        .expect("parent");
    until(|| {
        let ids = Arc::clone(&ids);
        async move { ids.lock().unwrap_or_else(|e| e.into_inner()).len() == 2 }
    })
    .await;
    let id_list = ids.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let outside_id = outside.lock().unwrap_or_else(|e| e.into_inner()).expect("outside");

    assert_eq!(outcome_of(&opened.harness, id_list[1]).await, "aborted");
    assert!(matches!(
        task_state(&opened.harness, id_list[0]).await,
        TaskState::Completing {
            outcome: TaskOutcome::Failed { .. }
        }
    ));
    assert_eq!(
        task_state(&opened.harness, parent.erased()).await.status(),
        TaskStatus::Waiting
    );

    st.open_ok("abort.grandchild");
    until(|| {
        let st = st.clone();
        async move { st.has_log("resume:parent") }
    })
    .await;

    assert_eq!(outcome_of(&opened.harness, id_list[0]).await, "failed");
    let m0 = opened
        .harness
        .get_erased_task(id_list[0], context())
        .await
        .unwrap()
        .unwrap()
        .abort_requested;
    let m1 = opened
        .harness
        .get_erased_task(id_list[1], context())
        .await
        .unwrap()
        .unwrap()
        .abort_requested;
    let mout = opened
        .harness
        .get_erased_task(outside_id, context())
        .await
        .unwrap()
        .unwrap()
        .abort_requested;
    assert_eq!([m0, m1, mout], [false, true, false]);
    assert!(
        !opened
            .harness
            .get_erased_task(parent.erased(), context())
            .await
            .unwrap()
            .unwrap()
            .abort_requested
    );
    assert_eq!(
        task_state(&opened.harness, outside_id).await.status(),
        TaskStatus::Running
    );

    until(|| {
        let harness = opened.harness.clone();
        async move { task_state(&harness, parent.erased()).await.status() == TaskStatus::Completing }
    })
    .await;
    st.open_ok("outside");
    assert_eq!(outcome_of(&opened.harness, parent.erased()).await, "completed");
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn waits_with_all_settled_on_tasks_it_does_not_own_including_already_terminal() {
    let st = StructuredState::new();
    let node = st.node();
    let opened = open_nodes(&st, None).await;
    let done = start(&opened.root, &node, "done", own_conversation())
        .await
        .expect("done");
    st.open_ok("done");
    opened.harness.wait_for_task(done, context()).await.expect("wait done");
    let live = start(&opened.root, &node, "live", own_conversation())
        .await
        .expect("live");

    let outcomes = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let done_id = done.erased();
    let live_id = live.erased();
    let out_res = Arc::clone(&outcomes);
    st.script(
        "parent",
        Behavior {
            run: Some(Arc::new(move |runtime, ctx| {
                Box::pin(async move {
                    runtime
                        .commit(
                            move |_tx, _current| {
                                Box::pin(
                                    async move { Ok(Some(wait_on(&[done_id, live_id], JoinPolicy::AllSettled, 1))) },
                                )
                            },
                            ctx,
                        )
                        .await
                })
            })),
            resume: Some(Arc::new(move |runtime, ctx, _| {
                let out_res = Arc::clone(&out_res);
                Box::pin(async move {
                    let fetched = runtime.outcomes::<JsonValue>(&[done_id, live_id], ctx.clone()).await?;
                    *out_res.lock().unwrap_or_else(|e| e.into_inner()) = fetched.iter().map(outcome_status).collect();
                    runtime
                        .commit(
                            |_tx, _current| Box::pin(async { Ok(Some(end_completed("parent"))) }),
                            ctx,
                        )
                        .await
                })
            })),
            abort: None,
        },
    );

    let parent = start(&opened.root, &node, "parent", own_conversation())
        .await
        .expect("parent");
    until(|| {
        let harness = opened.harness.clone();
        async move { task_state(&harness, parent.erased()).await.status() == TaskStatus::Waiting }
    })
    .await;
    st.open("live", Ending::Failed);
    assert_eq!(outcome_of(&opened.harness, parent.erased()).await, "completed");
    assert_eq!(
        *outcomes.lock().unwrap_or_else(|e| e.into_inner()),
        vec!["completed", "failed"]
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn rejects_waits_on_itself_its_owner_missing_task_and_fail_fast_on_foreign_task() {
    let st = StructuredState::new();
    let node = st.node();
    let opened = open_nodes(&st, None).await;
    let other = start(&opened.root, &node, "other", own_conversation())
        .await
        .expect("other");

    let other_id = other.erased();
    let cases: [(&str, Arc<dyn Fn(TaskId) -> Vec<TaskId> + Send + Sync>, JoinPolicy, &str); 3] = [
        (
            "self",
            Arc::new(|tid| vec![tid]),
            JoinPolicy::AllSettled,
            "cannot wait on itself or its owner",
        ),
        (
            "missing",
            Arc::new(|_| vec![TaskId::new_unchecked(999_999)]),
            JoinPolicy::AllSettled,
            "does not exist",
        ),
        (
            "foreign",
            Arc::new(move |_| vec![other_id]),
            JoinPolicy::FailFast,
            "only on tasks it owns",
        ),
    ];

    for (name, on_fn, policy, msg) in cases {
        st.script(
            name,
            Behavior {
                run: Some(Arc::new(move |runtime, ctx| {
                    let on_fn = Arc::clone(&on_fn);
                    Box::pin(async move {
                        let on = on_fn(runtime.task_id()?.erased());
                        runtime
                            .commit(
                                move |_tx, _current| {
                                    let on = on.clone();
                                    Box::pin(async move { Ok(Some(wait_on(&on, policy, 1))) })
                                },
                                ctx,
                            )
                            .await
                    })
                })),
                ..Default::default()
            },
        );
        let id = start(&opened.root, &node, name, own_conversation())
            .await
            .expect("start case");
        let rec = opened.harness.wait_for_task(id, context()).await.expect("wait case");
        match rec.outcome {
            TaskOutcome::Faulted { error } => assert!(error.message.contains(msg), "got {}", error.message),
            other => panic!("expected Faulted for {name}, got {other:?}"),
        }
    }

    let child_slot = Arc::new(Mutex::new(None::<TaskId<String>>));
    let parent_slot = Arc::new(Mutex::new(None::<TaskId<String>>));
    let parent_for_child = Arc::clone(&parent_slot);
    st.script(
        "child",
        Behavior {
            run: Some(Arc::new(move |runtime, ctx| {
                let parent_for_child = Arc::clone(&parent_for_child);
                Box::pin(async move {
                    let p = parent_for_child
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .expect("parent id")
                        .erased();
                    runtime
                        .commit(
                            move |_tx, _current| {
                                Box::pin(async move { Ok(Some(wait_on(&[p], JoinPolicy::AllSettled, 1))) })
                            },
                            ctx,
                        )
                        .await
                })
            })),
            ..Default::default()
        },
    );
    let child_cb = Arc::clone(&child_slot);
    let node_cb = node.clone();
    st.script(
        "parent",
        Behavior {
            run: Some(Arc::new(move |runtime, ctx| {
                let child_cb = Arc::clone(&child_cb);
                let node_cb = node_cb.clone();
                Box::pin(async move {
                    let owner = runtime.task_id()?.erased();
                    runtime
                        .commit(
                            move |tx, _current| {
                                let child_cb = Arc::clone(&child_cb);
                                let node_cb = node_cb.clone();
                                Box::pin(async move {
                                    let child = spawn(&tx, &node_cb, owner, "child").await?;
                                    *child_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
                                    Ok(Some(wait_on(&[child.erased()], JoinPolicy::AllSettled, 1)))
                                })
                            },
                            ctx,
                        )
                        .await
                })
            })),
            ..Default::default()
        },
    );

    let parent = start(&opened.root, &node, "parent", own_conversation())
        .await
        .expect("start parent");
    *parent_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(parent);
    let parent_settled = opened
        .harness
        .wait_for_task(parent, context())
        .await
        .expect("wait parent");
    assert_eq!(outcome_status(&parent_settled.outcome), "completed");
    let child_id = child_slot.lock().unwrap_or_else(|e| e.into_inner()).expect("child");
    let child_settled = opened
        .harness
        .wait_for_task(child_id, context())
        .await
        .expect("wait child");
    match child_settled.outcome {
        TaskOutcome::Faulted { error } => assert!(error.message.contains("cannot wait on itself or its owner")),
        other => panic!("expected Faulted, got {other:?}"),
    }
    st.open_ok("other");
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn resumes_at_the_next_pass_when_it_waits_on_nothing() {
    let st = StructuredState::new();
    let node = st.node();
    st.script(
        "parent",
        Behavior {
            run: Some(Arc::new(|runtime, ctx| {
                Box::pin(async move {
                    runtime
                        .commit(
                            |_tx, _current| Box::pin(async { Ok(Some(wait_on(&[], JoinPolicy::FailFast, 1))) }),
                            ctx,
                        )
                        .await
                })
            })),
            ..Default::default()
        },
    );
    let opened = open_nodes(&st, None).await;
    let parent = start(&opened.root, &node, "parent", own_conversation())
        .await
        .expect("parent");
    assert_eq!(outcome_of(&opened.harness, parent.erased()).await, "completed");
    assert_eq!(st.logs(), vec!["run:parent", "resume:parent"]);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn keeps_running_phases_after_spawning_and_waits_on_subsets_in_sequence() {
    let st = StructuredState::new();
    let node = st.node();
    let ids = Arc::new(Mutex::new(Vec::<TaskId>::new()));

    let ids_run = Arc::clone(&ids);
    let node_run = node.clone();
    let ids_res = Arc::clone(&ids);
    let st_res = st.clone();
    st.script(
        "parent",
        Behavior {
            run: Some(Arc::new(move |runtime, ctx| {
                let ids_run = Arc::clone(&ids_run);
                let node_run = node_run.clone();
                Box::pin(async move {
                    let owner = runtime.task_id()?.erased();
                    runtime
                        .commit(
                            move |tx, _current| {
                                let ids_run = Arc::clone(&ids_run);
                                let node_run = node_run.clone();
                                Box::pin(async move {
                                    let mut created = Vec::new();
                                    for name in ["a", "b", "c"] {
                                        created.push(spawn(&tx, &node_run, owner, name).await?.erased());
                                    }
                                    *ids_run.lock().unwrap_or_else(|e| e.into_inner()) = created;
                                    Ok(Some(NextTaskState::Running {
                                        checkpoint: NodeCheckpoint::Resume { round: 0 },
                                    }))
                                })
                            },
                            ctx,
                        )
                        .await
                })
            })),
            resume: Some(Arc::new(move |runtime, ctx, round| {
                let ids_res = Arc::clone(&ids_res);
                let st_res = st_res.clone();
                Box::pin(async move {
                    st_res.push_log(format!("round:{round}"));
                    let list = ids_res.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    if round == 0 {
                        return runtime
                            .commit(
                                move |_tx, _current| {
                                    Box::pin(async move { Ok(Some(wait_on(&[list[0]], JoinPolicy::AllSettled, 1))) })
                                },
                                ctx,
                            )
                            .await;
                    }
                    if round == 1 {
                        return runtime
                            .commit(
                                move |_tx, _current| {
                                    Box::pin(
                                        async move { Ok(Some(wait_on(&[list[1], list[2]], JoinPolicy::FailFast, 2))) },
                                    )
                                },
                                ctx,
                            )
                            .await;
                    }
                    runtime
                        .commit(
                            |_tx, _current| Box::pin(async { Ok(Some(end_completed("parent"))) }),
                            ctx,
                        )
                        .await
                })
            })),
            abort: None,
        },
    );

    let opened = open_nodes(&st, None).await;
    let parent = start(&opened.root, &node, "parent", own_conversation())
        .await
        .expect("parent");
    until(|| {
        let st = st.clone();
        async move { st.has_log("round:0") }
    })
    .await;
    until(|| {
        let harness = opened.harness.clone();
        async move { task_state(&harness, parent.erased()).await.status() == TaskStatus::Waiting }
    })
    .await;
    let list = ids.lock().unwrap_or_else(|e| e.into_inner()).clone();
    st.open_ok("b");
    let _ = opened
        .harness
        .wait_for_task::<JsonValue>(list[1].cast(), context())
        .await
        .expect("b settled");
    assert_eq!(
        task_state(&opened.harness, parent.erased()).await.status(),
        TaskStatus::Waiting
    );
    st.open_ok("a");
    until(|| {
        let st = st.clone();
        async move { st.has_log("round:1") }
    })
    .await;
    st.open_ok("c");
    assert_eq!(outcome_of(&opened.harness, parent.erased()).await, "completed");
    let rounds: Vec<String> = st
        .logs()
        .into_iter()
        .filter(|line| line.starts_with("round:"))
        .collect();
    assert_eq!(rounds, vec!["round:0", "round:1", "round:2"]);
    opened.harness.close(context()).await.expect("close");
}

// ─── Completing ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn holds_a_finished_task_until_its_owned_work_drains_waiters_and_task_docs_wait() {
    let st = StructuredState::new();
    let node = st.node();
    let notes = task_notes_doc();
    let child_slot = Arc::new(Mutex::new(None::<TaskId<String>>));
    let child_cb = Arc::clone(&child_slot);
    let node_cb = node.clone();
    let notes_cb = notes.clone();

    st.script(
        "parent",
        Behavior {
            run: Some(Arc::new(move |runtime, ctx| {
                let child_cb = Arc::clone(&child_cb);
                let node_cb = node_cb.clone();
                let notes_cb = notes_cb.clone();
                Box::pin(async move {
                    let tid = runtime.task_id()?.erased();
                    runtime
                        .commit(
                            move |tx, _current| {
                                let child_cb = Arc::clone(&child_cb);
                                let node_cb = node_cb.clone();
                                let notes_cb = notes_cb.clone();
                                Box::pin(async move {
                                    tx.doc_task(&notes_cb, tid).await?.update(|doc| {
                                        doc.text = "notes".to_owned();
                                    })?;
                                    let child = spawn(&tx, &node_cb, tid, "child").await?;
                                    *child_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
                                    Ok(Some(NextTaskState::Running {
                                        checkpoint: NodeCheckpoint::Resume { round: 1 },
                                    }))
                                })
                            },
                            ctx,
                        )
                        .await
                })
            })),
            ..Default::default()
        },
    );

    let opened = open_nodes(&st, None).await;
    let parent = start(&opened.root, &node, "parent", own_conversation())
        .await
        .expect("parent");
    until(|| {
        let harness = opened.harness.clone();
        async move { task_state(&harness, parent.erased()).await.status() == TaskStatus::Completing }
    })
    .await;
    assert_eq!(
        task_state(&opened.harness, parent.erased()).await,
        TaskState::Completing {
            outcome: TaskOutcome::Completed {
                result: JsonValue::String("parent".to_owned()),
            }
        }
    );

    let h_w = opened.harness.clone();
    let waiter = track(async move { h_w.wait_for_task(parent, context()).await });
    assert!(!settled(&waiter).await);
    assert_eq!(
        opened
            .harness
            .snapshot_task(&notes, parent.erased(), context())
            .await
            .expect("snap"),
        Some(TaskNotes {
            text: "notes".to_owned()
        })
    );
    let inspected = opened.harness.inspect(context()).await.expect("inspect");
    let found = inspected
        .tasks
        .iter()
        .find(|t| t.record.id == parent.erased())
        .expect("inspected");
    assert!(matches!(found.state, TaskInspectionState::Completing));

    let r_w = opened.root.clone();
    let root_idle = track(async move { r_w.wait_for_idle(context()).await });
    assert!(!settled(&root_idle).await);

    st.open_ok("child");
    let settled_parent = waiter.wait().await.expect("wait");
    assert_eq!(outcome_status(&settled_parent.outcome), "completed");
    assert_eq!(
        settled_parent.outcome,
        TaskOutcome::Completed {
            result: "parent".to_owned()
        }
    );
    assert_eq!(
        opened
            .harness
            .snapshot_task(&notes, parent.erased(), context())
            .await
            .expect("snap after"),
        None
    );
    root_idle.wait().await.expect("root idle");
    assert!(child_slot.lock().unwrap_or_else(|e| e.into_inner()).is_some());
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn keeps_holding_for_ordinary_work_created_during_the_hold() {
    let st = StructuredState::new();
    let node = st.node();
    let conv_slot = Arc::new(Mutex::new(None::<ConversationId>));
    let conv_cb = Arc::clone(&conv_slot);
    let node_cb = node.clone();

    st.script(
        "parent",
        Behavior {
            run: Some(Arc::new(move |runtime, ctx| {
                let conv_cb = Arc::clone(&conv_cb);
                let node_cb = node_cb.clone();
                Box::pin(async move {
                    let owner = runtime.task_id()?.erased();
                    runtime
                        .commit(
                            move |tx, _current| {
                                let conv_cb = Arc::clone(&conv_cb);
                                let node_cb = node_cb.clone();
                                Box::pin(async move {
                                    let conv = tx
                                        .create_conversation(ConversationOwnership::Task { task_id: owner })
                                        .await?;
                                    let mut opts = own_conversation();
                                    opts.conversation_id = Some(conv.id);
                                    let _ = tx
                                        .create_task(
                                            &node_cb,
                                            &NodeInput {
                                                name: "first".to_owned(),
                                            },
                                            opts,
                                        )
                                        .await?;
                                    *conv_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(conv.id);
                                    Ok(Some(NextTaskState::Running {
                                        checkpoint: NodeCheckpoint::Resume { round: 1 },
                                    }))
                                })
                            },
                            ctx,
                        )
                        .await
                })
            })),
            ..Default::default()
        },
    );

    let opened = open_nodes(&st, None).await;
    let parent = start(&opened.root, &node, "parent", own_conversation())
        .await
        .expect("parent");
    until(|| {
        let harness = opened.harness.clone();
        async move { task_state(&harness, parent.erased()).await.status() == TaskStatus::Completing }
    })
    .await;
    let cid = conv_slot.lock().unwrap_or_else(|e| e.into_inner()).expect("cid");
    let child = opened
        .harness
        .conversation(cid, context())
        .await
        .expect("conv")
        .expect("present");
    let _second = start(&child, &node, "second", own_conversation())
        .await
        .expect("second");
    st.open_ok("first");
    until(|| {
        let st = st.clone();
        async move { st.has_log("run:second") }
    })
    .await;
    assert_eq!(
        task_state(&opened.harness, parent.erased()).await.status(),
        TaskStatus::Completing
    );
    st.open_ok("second");
    assert_eq!(outcome_of(&opened.harness, parent.erased()).await, "completed");
    opened.harness.close(context()).await.expect("close");
}

async fn run_held_failure_case(ending: Ending, expected_parent_outcome: &str) {
    let st = StructuredState::new();
    let node = st.node();
    let child_slot = Arc::new(Mutex::new(None::<TaskId<String>>));
    let child_cb = Arc::clone(&child_slot);
    let node_cb = node.clone();

    st.script(
        "parent",
        Behavior {
            run: Some(Arc::new(move |runtime, ctx| {
                let child_cb = Arc::clone(&child_cb);
                let node_cb = node_cb.clone();
                Box::pin(async move {
                    let owner = runtime.task_id()?.erased();
                    runtime
                        .commit(
                            move |tx, _current| {
                                let child_cb = Arc::clone(&child_cb);
                                let node_cb = node_cb.clone();
                                Box::pin(async move {
                                    let child = spawn(&tx, &node_cb, owner, "child").await?;
                                    *child_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
                                    Ok(Some(NextTaskState::Running {
                                        checkpoint: NodeCheckpoint::Resume { round: 1 },
                                    }))
                                })
                            },
                            ctx,
                        )
                        .await
                })
            })),
            resume: Some(Arc::new(move |runtime, ctx, _| {
                Box::pin(async move {
                    if ending == Ending::Throw {
                        return Err(pi_js::Error::msg("parent threw"));
                    }
                    runtime
                        .commit(|_tx, _current| Box::pin(async { Ok(Some(end_failed("parent"))) }), ctx)
                        .await
                })
            })),
            abort: None,
        },
    );

    let opened = open_nodes(&st, None).await;
    let parent = start(&opened.root, &node, "parent", own_conversation())
        .await
        .expect("parent");
    until(|| {
        let child_slot = Arc::clone(&child_slot);
        async move { child_slot.lock().unwrap_or_else(|e| e.into_inner()).is_some() }
    })
    .await;
    let child = child_slot.lock().unwrap_or_else(|e| e.into_inner()).expect("child");
    assert_eq!(outcome_of(&opened.harness, child.erased()).await, "aborted");
    assert_eq!(
        outcome_of(&opened.harness, parent.erased()).await,
        expected_parent_outcome
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn aborts_work_below_a_held_failure_then_finishes_with_held_outcome() {
    run_held_failure_case(Ending::Failed, "failed").await;
}

#[tokio::test]
async fn aborts_work_below_a_held_scheduler_fault_then_finishes_with_held_outcome() {
    run_held_failure_case(Ending::Throw, "faulted").await;
}

#[tokio::test]
async fn only_marks_a_completing_task_when_aborted_work_below_aborted_held_outcome_stays() {
    let st = StructuredState::new();
    let node = st.node();
    let child_slot = Arc::new(Mutex::new(None::<TaskId<String>>));
    let child_cb = Arc::clone(&child_slot);
    let node_cb = node.clone();

    st.script(
        "parent",
        Behavior {
            run: Some(Arc::new(move |runtime, ctx| {
                let child_cb = Arc::clone(&child_cb);
                let node_cb = node_cb.clone();
                Box::pin(async move {
                    let owner = runtime.task_id()?.erased();
                    runtime
                        .commit(
                            move |tx, _current| {
                                let child_cb = Arc::clone(&child_cb);
                                let node_cb = node_cb.clone();
                                Box::pin(async move {
                                    let child = spawn(&tx, &node_cb, owner, "child").await?;
                                    *child_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
                                    Ok(Some(NextTaskState::Running {
                                        checkpoint: NodeCheckpoint::Resume { round: 1 },
                                    }))
                                })
                            },
                            ctx,
                        )
                        .await
                })
            })),
            ..Default::default()
        },
    );

    let opened = open_nodes(&st, None).await;
    let parent = start(&opened.root, &node, "parent", own_conversation())
        .await
        .expect("parent");
    until(|| {
        let harness = opened.harness.clone();
        async move { task_state(&harness, parent.erased()).await.status() == TaskStatus::Completing }
    })
    .await;
    let child = child_slot.lock().unwrap_or_else(|e| e.into_inner()).expect("child");
    assert_eq!(
        opened
            .harness
            .abort_task(parent.erased(), context())
            .await
            .expect("abort parent"),
        AbortTaskResult::Marked
    );
    assert_eq!(outcome_of(&opened.harness, child.erased()).await, "aborted");
    let settled_parent = opened
        .harness
        .wait_for_task(parent, context())
        .await
        .expect("wait parent");
    assert_eq!(
        settled_parent.outcome,
        TaskOutcome::Completed {
            result: "parent".to_owned()
        }
    );
    assert!(settled_parent.abort_requested);
    assert!(!st.has_log("abort:parent"));
    assert_eq!(
        opened
            .harness
            .abort_task(parent.erased(), context())
            .await
            .expect("abort terminal"),
        AbortTaskResult::Terminal
    );
    opened.harness.close(context()).await.expect("close");
}

// ─── Abort order ────────────────────────────────────────────────────────────

fn chain(
    st: &StructuredState,
    node: &NodeTask,
    harness_slot: &Arc<Mutex<Option<Harness>>>,
    name: &str,
    child: &str,
) -> Arc<Mutex<Option<TaskId<String>>>> {
    let found = Arc::new(Mutex::new(None::<TaskId<String>>));
    let found_run = Arc::clone(&found);
    let node_run = node.clone();
    let child_name = child.to_owned();

    let found_abort = Arc::clone(&found);
    let harness_abort = Arc::clone(harness_slot);
    let st_abort = st.clone();
    let name_abort = name.to_owned();

    st.script(
        name,
        Behavior {
            run: Some(Arc::new(move |runtime, ctx| {
                let found_run = Arc::clone(&found_run);
                let node_run = node_run.clone();
                let child_name = child_name.clone();
                Box::pin(async move {
                    let owner = runtime.task_id()?.erased();
                    runtime
                        .commit(
                            move |tx, _current| {
                                let found_run = Arc::clone(&found_run);
                                let node_run = node_run.clone();
                                let child_name = child_name.clone();
                                Box::pin(async move {
                                    let cid = spawn(&tx, &node_run, owner, &child_name).await?;
                                    *found_run.lock().unwrap_or_else(|e| e.into_inner()) = Some(cid);
                                    Ok(Some(wait_on(&[cid.erased()], JoinPolicy::AllSettled, 1)))
                                })
                            },
                            ctx,
                        )
                        .await
                })
            })),
            resume: None,
            abort: Some(Arc::new(move |runtime, ctx| {
                let found_abort = Arc::clone(&found_abort);
                let harness_abort = Arc::clone(&harness_abort);
                let st_abort = st_abort.clone();
                let name_abort = name_abort.clone();
                Box::pin(async move {
                    let h = harness_abort
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone()
                        .expect("harness");
                    let cid = found_abort
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .expect("child id")
                        .erased();
                    let status_str = match task_state(&h, cid).await.status() {
                        TaskStatus::Pending => "pending",
                        TaskStatus::Running => "running",
                        TaskStatus::Waiting => "waiting",
                        TaskStatus::Completing => "completing",
                        TaskStatus::Terminal => "terminal",
                    };
                    st_abort.push_log(format!("{name_abort} saw {status_str}"));
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
            })),
        },
    );
    found
}

#[tokio::test]
async fn runs_abort_handlers_bottom_up_across_three_levels_each_after_level_below_is_terminal() {
    let st = StructuredState::new();
    let node = st.node();
    let harness_slot = Arc::new(Mutex::new(None::<Harness>));
    let b = chain(&st, &node, &harness_slot, "a", "b");
    let c = chain(&st, &node, &harness_slot, "b", "c");
    let opened = open_nodes(&st, None).await;
    *harness_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(opened.harness.clone());

    let a = start(&opened.root, &node, "a", own_conversation()).await.expect("a");
    until(|| {
        let st = st.clone();
        async move { st.has_log("run:c") }
    })
    .await;
    let c_id = c.lock().unwrap_or_else(|e| e.into_inner()).expect("c id");
    until(|| {
        let harness = opened.harness.clone();
        async move { task_state(&harness, c_id.erased()).await.status() == TaskStatus::Running }
    })
    .await;
    assert_eq!(
        opened.harness.abort_task(a.erased(), context()).await.expect("abort a"),
        AbortTaskResult::Marked
    );
    assert_eq!(outcome_of(&opened.harness, a.erased()).await, "aborted");
    let filtered: Vec<String> = st
        .logs()
        .into_iter()
        .filter(|line| line.starts_with("abort:") || line.contains(" saw "))
        .collect();
    assert_eq!(
        filtered,
        vec!["abort:c", "abort:b", "b saw terminal", "abort:a", "a saw terminal"]
    );
    assert!(b.lock().unwrap_or_else(|e| e.into_inner()).is_some());
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn does_not_wait_for_a_task_it_waits_on_but_does_not_own() {
    let st = StructuredState::new();
    let node = st.node();
    let opened = open_nodes(&st, None).await;
    let other = start(&opened.root, &node, "other", own_conversation())
        .await
        .expect("other");
    let other_id = other.erased();
    st.script(
        "parent",
        Behavior {
            run: Some(Arc::new(move |runtime, ctx| {
                Box::pin(async move {
                    runtime
                        .commit(
                            move |_tx, _current| {
                                Box::pin(async move { Ok(Some(wait_on(&[other_id], JoinPolicy::AllSettled, 1))) })
                            },
                            ctx,
                        )
                        .await
                })
            })),
            ..Default::default()
        },
    );
    let parent = start(&opened.root, &node, "parent", own_conversation())
        .await
        .expect("parent");
    until(|| {
        let harness = opened.harness.clone();
        async move { task_state(&harness, parent.erased()).await.status() == TaskStatus::Waiting }
    })
    .await;
    opened
        .harness
        .abort_task(parent.erased(), context())
        .await
        .expect("abort");
    assert_eq!(outcome_of(&opened.harness, parent.erased()).await, "aborted");
    assert_eq!(
        task_state(&opened.harness, other.erased()).await.status(),
        TaskStatus::Running
    );
    st.open_ok("other");
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn reports_an_abort_marked_task_as_waiting_for_its_live_owned_work() {
    let st = StructuredState::new();
    let node = st.node();
    let child_slot = Arc::new(Mutex::new(None::<TaskId<String>>));
    let st_child = st.clone();
    st.script(
        "child",
        Behavior {
            abort: Some(Arc::new(move |runtime, ctx| {
                let st_child = st_child.clone();
                Box::pin(async move {
                    let _ = st_child.gate("abort.child").wait().await?;
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
            })),
            ..Default::default()
        },
    );
    let child_cb = Arc::clone(&child_slot);
    let node_cb = node.clone();
    st.script(
        "parent",
        Behavior {
            run: Some(Arc::new(move |runtime, ctx| {
                let child_cb = Arc::clone(&child_cb);
                let node_cb = node_cb.clone();
                Box::pin(async move {
                    let owner = runtime.task_id()?.erased();
                    runtime
                        .commit(
                            move |tx, _current| {
                                let child_cb = Arc::clone(&child_cb);
                                let node_cb = node_cb.clone();
                                Box::pin(async move {
                                    let child = spawn(&tx, &node_cb, owner, "child").await?;
                                    *child_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
                                    Ok(Some(wait_on(&[child.erased()], JoinPolicy::AllSettled, 1)))
                                })
                            },
                            ctx,
                        )
                        .await
                })
            })),
            ..Default::default()
        },
    );

    let opened = open_nodes(&st, None).await;
    let parent = start(&opened.root, &node, "parent", own_conversation())
        .await
        .expect("parent");
    until(|| {
        let harness = opened.harness.clone();
        async move { task_state(&harness, parent.erased()).await.status() == TaskStatus::Waiting }
    })
    .await;
    opened
        .harness
        .abort_task(parent.erased(), context())
        .await
        .expect("abort");
    until(|| {
        let st = st.clone();
        async move { st.has_log("abort:child") }
    })
    .await;
    let child_id = child_slot.lock().unwrap_or_else(|e| e.into_inner()).expect("child");
    let inspected: HarnessInspection = opened.harness.inspect(context()).await.expect("inspect");
    let found = inspected
        .tasks
        .iter()
        .find(|task| task.record.id == parent.erased())
        .expect("found");
    assert!(matches!(
        &found.state,
        TaskInspectionState::Waiting { on } if on == &vec![child_id.erased()]
    ));
    st.open_ok("abort.child");
    assert_eq!(outcome_of(&opened.harness, parent.erased()).await, "aborted");
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn faults_an_abort_handler_that_tries_to_wait() {
    let st = StructuredState::new();
    let node = st.node();
    st.script(
        "parent",
        Behavior {
            abort: Some(Arc::new(|runtime, ctx| {
                Box::pin(async move {
                    runtime
                        .commit(
                            |_tx, _current| Box::pin(async { Ok(Some(wait_on(&[], JoinPolicy::AllSettled, 1))) }),
                            ctx,
                        )
                        .await
                })
            })),
            ..Default::default()
        },
    );
    let opened = open_nodes(&st, None).await;
    let parent = start(&opened.root, &node, "parent", own_conversation())
        .await
        .expect("parent");
    until(|| {
        let st = st.clone();
        async move { st.has_log("run:parent") }
    })
    .await;
    opened
        .harness
        .abort_task(parent.erased(), context())
        .await
        .expect("abort");
    let rec = opened.harness.wait_for_task(parent, context()).await.expect("wait");
    match rec.outcome {
        TaskOutcome::Faulted { error } => assert!(error.message.contains("cannot wait")),
        other => panic!("expected Faulted, got {other:?}"),
    }
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn orphans_a_blocked_task_only_after_its_owned_work_drained() {
    let st = StructuredState::new();
    let node = st.node();
    let unreg = unregistered_task();
    let opened = open_nodes(&st, None).await;
    let (owner, child) = opened
        .root
        .commit(
            {
                let unreg = unreg.clone();
                let node = node.clone();
                move |tx| async move {
                    let owner = tx
                        .create_task(
                            &unreg,
                            &NodeInput {
                                name: "owner".to_owned(),
                            },
                            own_conversation(),
                        )
                        .await?;
                    let child = spawn(&tx, &node, owner.erased(), "child").await?;
                    Ok((owner, child))
                }
            },
            context(),
        )
        .await
        .expect("commit");
    until(|| {
        let st = st.clone();
        async move { st.has_log("run:child") }
    })
    .await;
    assert_eq!(
        opened
            .harness
            .abort_task(owner.erased(), context())
            .await
            .expect("abort"),
        AbortTaskResult::Marked
    );
    assert_eq!(outcome_of(&opened.harness, child.erased()).await, "aborted");
    let rec = opened
        .harness
        .wait_for_task(owner, context())
        .await
        .expect("wait owner");
    assert_eq!(
        rec.outcome,
        TaskOutcome::Orphaned {
            reason: "missing_task".to_owned(),
        }
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn cascades_through_task_and_conversation_edges_bottom_up() {
    let st = StructuredState::new();
    let node = st.node();
    let x_slot = Arc::new(Mutex::new(None::<TaskId<String>>));

    let node_a = node.clone();
    st.script(
        "a",
        Behavior {
            run: Some(Arc::new(move |runtime, ctx| {
                let node_a = node_a.clone();
                Box::pin(async move {
                    let owner = runtime.task_id()?.erased();
                    runtime
                        .commit(
                            move |tx, _current| {
                                let node_a = node_a.clone();
                                Box::pin(async move {
                                    let b = spawn(&tx, &node_a, owner, "b").await?;
                                    Ok(Some(wait_on(&[b.erased()], JoinPolicy::AllSettled, 1)))
                                })
                            },
                            ctx,
                        )
                        .await
                })
            })),
            ..Default::default()
        },
    );

    let x_cb = Arc::clone(&x_slot);
    let node_b = node.clone();
    st.script(
        "b",
        Behavior {
            run: Some(Arc::new(move |runtime, ctx| {
                let x_cb = Arc::clone(&x_cb);
                let node_b = node_b.clone();
                Box::pin(async move {
                    let owner = runtime.task_id()?.erased();
                    runtime
                        .commit(
                            move |tx, _current| {
                                let x_cb = Arc::clone(&x_cb);
                                let node_b = node_b.clone();
                                Box::pin(async move {
                                    let conv = tx
                                        .create_conversation(ConversationOwnership::Task { task_id: owner })
                                        .await?;
                                    let mut opts = own_conversation();
                                    opts.conversation_id = Some(conv.id);
                                    let x = tx
                                        .create_task(&node_b, &NodeInput { name: "x".to_owned() }, opts)
                                        .await?;
                                    *x_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(x);
                                    Ok(Some(NextTaskState::Running {
                                        checkpoint: NodeCheckpoint::Resume { round: 1 },
                                    }))
                                })
                            },
                            ctx,
                        )
                        .await
                })
            })),
            resume: Some(Arc::new(|runtime, _ctx, _| {
                Box::pin(async move { aborted::<()>(&runtime.signal()?).await })
            })),
            abort: None,
        },
    );

    let opened = open_nodes(&st, None).await;
    let a = start(&opened.root, &node, "a", own_conversation()).await.expect("a");
    until(|| {
        let st = st.clone();
        async move { st.has_log("run:x") }
    })
    .await;
    opened.harness.abort_task(a.erased(), context()).await.expect("abort a");
    let x_id = x_slot.lock().unwrap_or_else(|e| e.into_inner()).expect("x id");
    assert_eq!(outcome_of(&opened.harness, a.erased()).await, "aborted");
    assert_eq!(outcome_of(&opened.harness, x_id.erased()).await, "aborted");
    let aborts: Vec<String> = st
        .logs()
        .into_iter()
        .filter(|line| line.starts_with("abort:"))
        .collect();
    assert_eq!(aborts, vec!["abort:x", "abort:b", "abort:a"]);
    opened.harness.close(context()).await.expect("close");
}

// ─── Boundaries ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn keeps_background_work_through_conversation_abort_background_true_aborts_it_and_waits() {
    let st = StructuredState::new();
    let node = st.node();
    let child_slot = Arc::new(Mutex::new(None::<TaskId<String>>));
    let below_slot = Arc::new(Mutex::new(None::<TaskId<String>>));

    let child_cb = Arc::clone(&child_slot);
    let below_cb = Arc::clone(&below_slot);
    let node_bg = node.clone();
    st.script(
        "background",
        Behavior {
            run: Some(Arc::new(move |runtime, ctx| {
                let child_cb = Arc::clone(&child_cb);
                let below_cb = Arc::clone(&below_cb);
                let node_bg = node_bg.clone();
                Box::pin(async move {
                    let owner = runtime.task_id()?.erased();
                    runtime
                        .commit(
                            move |tx, _current| {
                                let child_cb = Arc::clone(&child_cb);
                                let below_cb = Arc::clone(&below_cb);
                                let node_bg = node_bg.clone();
                                Box::pin(async move {
                                    let child = spawn(&tx, &node_bg, owner, "child").await?;
                                    let conv = tx
                                        .create_conversation(ConversationOwnership::Task { task_id: owner })
                                        .await?;
                                    let mut opts = own_conversation();
                                    opts.conversation_id = Some(conv.id);
                                    let below = tx
                                        .create_task(
                                            &node_bg,
                                            &NodeInput {
                                                name: "below".to_owned(),
                                            },
                                            opts,
                                        )
                                        .await?;
                                    *child_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
                                    *below_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(below);
                                    Ok(Some(wait_on(&[child.erased()], JoinPolicy::AllSettled, 1)))
                                })
                            },
                            ctx,
                        )
                        .await
                })
            })),
            ..Default::default()
        },
    );

    let opened = open_nodes(&st, None).await;
    let foreground = start(&opened.root, &node, "foreground", own_conversation())
        .await
        .expect("fg");
    let mut bg_opts = own_conversation();
    bg_opts.background = true;
    let background = start(&opened.root, &node, "background", bg_opts).await.expect("bg");
    until(|| {
        let st = st.clone();
        async move { st.has_log("run:below") && st.has_log("run:child") }
    })
    .await;

    let child_id = child_slot.lock().unwrap_or_else(|e| e.into_inner()).expect("child");
    let below_id = below_slot.lock().unwrap_or_else(|e| e.into_inner()).expect("below");
    opened.root.abort(context(), None).await.expect("abort root");
    assert_eq!(outcome_of(&opened.harness, foreground.erased()).await, "aborted");
    for id in [background.erased(), child_id.erased(), below_id.erased()] {
        assert_ne!(task_state(&opened.harness, id).await.status(), TaskStatus::Terminal);
    }

    opened
        .root
        .abort(context(), Some(ConversationAbortOptions { background: true }))
        .await
        .expect("abort root bg");
    for id in [background.erased(), child_id.erased(), below_id.erased()] {
        assert_eq!(task_state(&opened.harness, id).await.status(), TaskStatus::Terminal);
    }
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn never_cascades_from_a_terminal_owner_aborted_subagent_conversation_runs_new_work() {
    let st = StructuredState::new();
    let node = st.node();
    let conv_slot = Arc::new(Mutex::new(None::<ConversationId>));
    let conv_cb = Arc::clone(&conv_slot);
    st.script(
        "agent",
        Behavior {
            run: Some(Arc::new(move |runtime, ctx| {
                let conv_cb = Arc::clone(&conv_cb);
                Box::pin(async move {
                    let owner = runtime.task_id()?.erased();
                    runtime
                        .commit(
                            move |tx, _current| {
                                let conv_cb = Arc::clone(&conv_cb);
                                Box::pin(async move {
                                    let conv = tx
                                        .create_conversation(ConversationOwnership::Task { task_id: owner })
                                        .await?;
                                    *conv_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(conv.id);
                                    Ok(Some(NextTaskState::Running {
                                        checkpoint: NodeCheckpoint::Resume { round: 1 },
                                    }))
                                })
                            },
                            ctx,
                        )
                        .await
                })
            })),
            resume: Some(Arc::new(|runtime, _ctx, _| {
                Box::pin(async move { aborted::<()>(&runtime.signal()?).await })
            })),
            abort: None,
        },
    );

    let opened = open_nodes(&st, None).await;
    let agent = start(&opened.root, &node, "agent", own_conversation())
        .await
        .expect("agent");
    until(|| {
        let st = st.clone();
        async move { st.has_log("resume:agent") }
    })
    .await;
    opened
        .harness
        .abort_task(agent.erased(), context())
        .await
        .expect("abort agent");
    assert_eq!(outcome_of(&opened.harness, agent.erased()).await, "aborted");
    let cid = conv_slot.lock().unwrap_or_else(|e| e.into_inner()).expect("cid");
    let child = opened
        .harness
        .conversation(cid, context())
        .await
        .expect("conv")
        .expect("present");
    let question = start(&child, &node, "question", own_conversation())
        .await
        .expect("question");
    st.open_ok("question");
    assert_eq!(outcome_of(&opened.harness, question.erased()).await, "completed");
    opened.harness.close(context()).await.expect("close");
}

// ─── Recovery ───────────────────────────────────────────────────────────────

struct Seeded {
    parent: TaskId,
    children: Vec<TaskId>,
}

async fn seed<F, Fut>(path: &str, node: &NodeTask, children: &[&str], edit: F) -> Seeded
where
    F: FnOnce(pi_durable::session::session::Session, Seeded) -> Fut,
    Fut: Future<Output = ()>,
{
    let storage = Arc::new(
        open_node_sqlite_storage(path, NodeSqliteStorageOptions::default())
            .await
            .expect("open sqlite"),
    );
    let session = create_session(storage);
    let node_c = node.clone();
    let child_names: Vec<String> = children.iter().map(|s| (*s).to_owned()).collect();
    let seeded = session
        .commit(
            move |tx| async move {
                let root = tx.create_conversation(ConversationOwnership::Ownerless).await?;
                let mut opts = own_conversation();
                opts.conversation_id = Some(root.id);
                let parent = tx
                    .create_task(
                        &node_c,
                        &NodeInput {
                            name: "parent".to_owned(),
                        },
                        opts,
                    )
                    .await?
                    .erased();
                let mut ids = Vec::with_capacity(child_names.len());
                for name in &child_names {
                    ids.push(spawn(&tx, &node_c, parent, name).await?.erased());
                }
                Ok(Seeded { parent, children: ids })
            },
            context(),
        )
        .await
        .expect("seed commit");
    edit(
        session.clone(),
        Seeded {
            parent: seeded.parent,
            children: seeded.children.clone(),
        },
    )
    .await;
    session.close(context()).await.expect("close seed");
    seeded
}

async fn patch_task<F>(session: &pi_durable::session::session::Session, id: TaskId, f: F)
where
    F: FnOnce(ErasedTaskRecord) -> ErasedTaskRecord + Send + 'static,
{
    session
        .commit(
            move |tx| async move {
                let rec = tx.task(id).await?.expect("task");
                tx.as_transaction().set_task(f(rec))
            },
            context(),
        )
        .await
        .expect("patch task");
}

fn completed_state() -> TaskState {
    TaskState::Terminal {
        outcome: TaskOutcome::Completed {
            result: JsonValue::String("done".to_owned()),
        },
    }
}

fn failed_state() -> TaskState {
    TaskState::Terminal {
        outcome: TaskOutcome::Failed {
            error: pi_durable::types::TaskOutcomeError {
                message: "declined".to_owned(),
                detail: None,
            },
            result: None,
        },
    }
}

#[tokio::test]
async fn resumes_a_parent_whose_awaited_children_finished_before_the_crash() {
    let st = StructuredState::new();
    let node = st.node();
    let (_dir, path) = sqlite_path().await;
    let seeded = seed(&path, &node, &["c1"], |session, s| async move {
        patch_task(&session, s.children[0], |mut rec| {
            rec.state = completed_state();
            rec
        })
        .await;
        let children = s.children.clone();
        patch_task(&session, s.parent, move |mut rec| {
            rec.state = TaskState::Waiting {
                checkpoint: serde_json::to_value(NodeCheckpoint::Resume { round: 1 }).unwrap(),
                on: children,
                policy: JoinPolicy::AllSettled,
            };
            rec
        })
        .await;
    })
    .await;

    let sqlite = Arc::new(
        open_node_sqlite_storage(&path, NodeSqliteStorageOptions::default())
            .await
            .expect("open"),
    );
    let opened = open_nodes(&st, Some(sqlite)).await;
    assert_eq!(outcome_of(&opened.harness, seeded.parent).await, "completed");
    assert_eq!(st.logs(), vec!["resume:parent"]);
    assert_eq!(seeded.children.len(), 1);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn marks_fail_fast_siblings_a_crash_left_unmarked() {
    let st = StructuredState::new();
    let node = st.node();
    let (_dir, path) = sqlite_path().await;
    let seeded = seed(&path, &node, &["c1", "c2"], |session, s| async move {
        patch_task(&session, s.children[0], |mut rec| {
            rec.state = failed_state();
            rec
        })
        .await;
        let children = s.children.clone();
        patch_task(&session, s.parent, move |mut rec| {
            rec.state = TaskState::Waiting {
                checkpoint: serde_json::to_value(NodeCheckpoint::Resume { round: 1 }).unwrap(),
                on: children,
                policy: JoinPolicy::FailFast,
            };
            rec
        })
        .await;
    })
    .await;

    let sqlite = Arc::new(
        open_node_sqlite_storage(&path, NodeSqliteStorageOptions::default())
            .await
            .expect("open"),
    );
    let opened = open_nodes(&st, Some(sqlite)).await;
    assert_eq!(outcome_of(&opened.harness, seeded.children[1]).await, "aborted");
    assert_eq!(outcome_of(&opened.harness, seeded.parent).await, "completed");
    assert!(!st.has_log("run:c2"));
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn finalizes_a_held_outcome_whose_work_drained_before_crash_and_keeps_one_whose_work_lives() {
    let st = StructuredState::new();
    let node = st.node();
    let (_dir, path) = sqlite_path().await;
    let held = TaskState::Completing {
        outcome: TaskOutcome::Completed {
            result: JsonValue::String("parent".to_owned()),
        },
    };
    let held_c = held.clone();
    let seeded = seed(&path, &node, &["c1"], |session, s| async move {
        patch_task(&session, s.parent, move |mut rec| {
            rec.state = held_c;
            rec
        })
        .await;
    })
    .await;

    let sqlite1 = Arc::new(
        open_node_sqlite_storage(&path, NodeSqliteStorageOptions::default())
            .await
            .expect("open 1"),
    );
    let opened1 = open_nodes(&st, Some(sqlite1)).await;
    until(|| {
        let st = st.clone();
        async move { st.has_log("run:c1") }
    })
    .await;
    assert_eq!(task_state(&opened1.harness, seeded.parent).await, held);
    opened1.harness.close(context()).await.expect("close 1");

    let sqlite2 = Arc::new(
        open_node_sqlite_storage(&path, NodeSqliteStorageOptions::default())
            .await
            .expect("open 2"),
    );
    let opened2 = open_nodes(&st, Some(sqlite2)).await;
    st.open_ok("c1");
    assert_eq!(outcome_of(&opened2.harness, seeded.children[0]).await, "completed");
    assert_eq!(outcome_of(&opened2.harness, seeded.parent).await, "completed");
    opened2.harness.close(context()).await.expect("close 2");

    let (_dir2, drained_path) = sqlite_path().await;
    let second = seed(&drained_path, &node, &["c1"], |session, s| async move {
        patch_task(&session, s.children[0], |mut rec| {
            rec.state = completed_state();
            rec
        })
        .await;
        patch_task(&session, s.parent, move |mut rec| {
            rec.state = held;
            rec
        })
        .await;
    })
    .await;
    let sqlite3 = Arc::new(
        open_node_sqlite_storage(&drained_path, NodeSqliteStorageOptions::default())
            .await
            .expect("open 3"),
    );
    let opened3 = open_nodes(&st, Some(sqlite3)).await;
    assert_eq!(outcome_of(&opened3.harness, second.parent).await, "completed");
    opened3.harness.close(context()).await.expect("close 3");
}

#[tokio::test]
async fn resumes_a_bottom_up_abort_a_crash_interrupted_before_the_cascade() {
    let st = StructuredState::new();
    let node = st.node();
    let (_dir, path) = sqlite_path().await;
    let seeded = seed(&path, &node, &["c1"], |session, s| async move {
        let children = s.children.clone();
        patch_task(&session, s.parent, move |mut rec| {
            rec.abort_requested = true;
            rec.state = TaskState::Waiting {
                checkpoint: serde_json::to_value(NodeCheckpoint::Resume { round: 1 }).unwrap(),
                on: children,
                policy: JoinPolicy::AllSettled,
            };
            rec
        })
        .await;
    })
    .await;

    let sqlite = Arc::new(
        open_node_sqlite_storage(&path, NodeSqliteStorageOptions::default())
            .await
            .expect("open"),
    );
    let opened = open_nodes(&st, Some(sqlite)).await;
    assert_eq!(outcome_of(&opened.harness, seeded.parent).await, "aborted");
    assert_eq!(outcome_of(&opened.harness, seeded.children[0]).await, "aborted");
    let aborts: Vec<String> = st
        .logs()
        .into_iter()
        .filter(|line| line.starts_with("abort:"))
        .collect();
    assert_eq!(aborts, vec!["abort:c1", "abort:parent"]);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn reopens_a_checkout_waiting_on_live_payments_and_finishes_it() {
    let st = StructuredState::new();
    let node = st.node();
    let (_dir, path) = sqlite_path().await;
    let (ids, outcomes) = parent_of(&st, &node, "checkout", &["p1", "p2"], JoinPolicy::FailFast);

    let sqlite1 = Arc::new(
        open_node_sqlite_storage(&path, NodeSqliteStorageOptions::default())
            .await
            .expect("open 1"),
    );
    let opened1 = open_nodes(&st, Some(sqlite1)).await;
    let parent = start(&opened1.root, &node, "checkout", own_conversation())
        .await
        .expect("start checkout");
    until(|| {
        let harness = opened1.harness.clone();
        async move { task_state(&harness, parent.erased()).await.status() == TaskStatus::Waiting }
    })
    .await;
    until(|| {
        let st = st.clone();
        async move { st.has_log("run:p1") && st.has_log("run:p2") }
    })
    .await;
    opened1.harness.close(context()).await.expect("close 1");

    let sqlite2 = Arc::new(
        open_node_sqlite_storage(&path, NodeSqliteStorageOptions::default())
            .await
            .expect("open 2"),
    );
    let opened2 = open_nodes(&st, Some(sqlite2)).await;
    let waiting_on = match task_state(&opened2.harness, parent.erased()).await {
        TaskState::Waiting { on, .. } => on,
        other => panic!("expected Waiting, got {other:?}"),
    };
    *ids.lock().unwrap_or_else(|e| e.into_inner()) = waiting_on;
    st.open_ok("p1");
    st.open_ok("p2");
    assert_eq!(outcome_of(&opened2.harness, parent.erased()).await, "completed");
    assert_eq!(
        *outcomes.lock().unwrap_or_else(|e| e.into_inner()),
        vec!["completed", "completed"]
    );
    opened2.harness.close(context()).await.expect("close 2");
}

// ─── Built-in tool rounds ───────────────────────────────────────────────────

fn blocking_tool(name: &str, mode: ToolExecutionMode) -> (Deferred<()>, ToolRegistration) {
    let started = deferred();
    let started_cb = started.clone();
    let reg = define_tool(ToolRegistration {
        name: name.to_owned(),
        description: format!("The {name} tool"),
        parameters: empty_schema(),
        replay: None,
        execution_mode: Some(mode),
        prepare_arguments: None,
        output_limits: None,
        execute: Arc::new(move |_args, api, call_context| {
            let started_cb = started_cb.clone();
            Box::pin(async move {
                api.output("partial")?;
                started_cb.resolve(());
                let sig = call_context.abort_signal().expect("abort signal");
                aborted::<ToolExecutionResult>(&sig).await
            })
        }),
    });
    (started, reg)
}

fn tool_calls(calls: &[(&str, &str)]) -> AssistantMessage {
    let blocks: Vec<FauxContentBlock> = calls
        .iter()
        .map(|(name, id)| {
            FauxContentBlock::ToolCall(faux_tool_call(
                name,
                IndexMap::new(),
                Some(FauxToolCallOptions {
                    id: Some((*id).to_owned()),
                }),
            ))
        })
        .collect();
    faux_assistant_message(
        FauxAssistantMessageContent::Blocks(blocks),
        Some(FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        }),
    )
}

fn done_assistant() -> AssistantMessage {
    faux_assistant_message(
        FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("done"))),
        None,
    )
}

fn noop_tool() -> ToolRegistration {
    define_tool(ToolRegistration {
        name: "noop".to_owned(),
        description: "Does nothing".to_owned(),
        parameters: empty_schema(),
        replay: None,
        execution_mode: None,
        prepare_arguments: None,
        output_limits: None,
        execute: Arc::new(|_, _, _| Box::pin(async { Ok(ToolExecutionResult::default()) })),
    })
}

#[tokio::test]
async fn owns_its_tool_tasks_waits_for_them_and_hands_run_to_conversation_owned_generation() {
    let setup = chat_setup(None);
    add_tool(&setup.registry, noop_tool(), None).expect("noop");
    setup.faux.set_responses(vec![
        FauxResponseStep::Message(tool_calls(&[("noop", "c1"), ("noop", "c2")])),
        FauxResponseStep::Message(done_assistant()),
    ]);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open chat");
    opened.harness.resume().expect("resume");
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
    assert_eq!(settled.record.status(), SubmissionStatus::Done);

    let root_id = opened.root.id();
    let tasks = opened
        .harness
        .commit(
            move |tx| async move {
                tx.scan_tasks(
                    TaskQuery {
                        conversation_id: Some(root_id),
                        ..Default::default()
                    },
                    20,
                    None,
                )
                .await
            },
            context(),
        )
        .await
        .expect("scan")
        .items;
    let gens: Vec<&ErasedTaskRecord> = tasks.iter().filter(|task| task.kind == "pi.generation").collect();
    assert_eq!(gens.len(), 2);
    let first = gens[0];
    let second = gens[1];
    let tool_owners: Vec<Option<TaskId>> = tasks
        .iter()
        .filter(|task| task.kind == "pi.tool")
        .map(|task| task.owner)
        .collect();
    assert_eq!(tool_owners, vec![Some(first.id), Some(first.id)]);
    assert_eq!(first.owner, None);
    assert_eq!(second.owner, None);
    let entries = all_entries(&opened.root, None).await.expect("entries");
    let assistant = entries
        .iter()
        .find(|entry| entry.kind == "pi.assistant")
        .expect("assistant");
    assert_eq!(
        first.state,
        TaskState::Terminal {
            outcome: TaskOutcome::Completed {
                result: serde_json::to_value(GenerationResult { entry_id: assistant.id }).expect("json"),
            }
        }
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn aborts_a_parallel_round_with_its_generation_tools_first_then_generation_every_result_written() {
    let setup = chat_setup(None);
    let (one_started, one_reg) = blocking_tool("one", ToolExecutionMode::Parallel);
    let (two_started, two_reg) = blocking_tool("two", ToolExecutionMode::Parallel);
    add_tool(&setup.registry, one_reg, None).expect("one");
    add_tool(&setup.registry, two_reg, None).expect("two");
    setup.faux.set_responses(vec![FauxResponseStep::Message(tool_calls(&[
        ("one", "c1"),
        ("two", "c2"),
    ]))]);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open");
    opened.harness.resume().expect("resume");
    let submission = opened
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
    tokio::try_join!(one_started.wait(), two_started.wait()).expect("started");
    let live = opened
        .harness
        .snapshot_conversation(&*LIVE_DOC, opened.root.id(), context())
        .await
        .expect("snap")
        .expect("live");
    let generation = live.run.expect("run").task_id;
    opened.harness.abort_task(generation, context()).await.expect("abort");
    let settled_sub = submission.wait(context()).await.expect("wait");
    assert!(matches!(
        settled_sub.record.state,
        SubmissionState::Input(InputSubmissionState::Unanswered { ref reason, .. }) if reason == "aborted"
    ));

    let entries = all_entries(&opened.root, None).await.expect("entries");
    let call_ids: Vec<String> = entries
        .iter()
        .filter(|entry| TOOL_RESULT_ENTRY.matches(Some(*entry)))
        .map(|entry| match &entry.model.as_ref().expect("model")[0] {
            Message::ToolResult(tr) => tr.tool_call_id.clone(),
            other => panic!("expected toolResult, got {other:?}"),
        })
        .collect();
    assert_eq!(call_ids, vec!["c1", "c2"]);
    assert_eq!(
        opened
            .harness
            .snapshot_conversation(&*LIVE_DOC, opened.root.id(), context())
            .await
            .expect("snap after"),
        Some(LiveState::default())
    );
    assert_eq!(outcome_of(&opened.harness, generation).await, "aborted");
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn answers_the_unstarted_calls_of_an_aborted_sequential_round_with_aborted_results_in_call_order() {
    let setup = chat_setup(None);
    let (one_started, one_reg) = blocking_tool("one", ToolExecutionMode::Sequential);
    let (_two_started, two_reg) = blocking_tool("two", ToolExecutionMode::Sequential);
    add_tool(&setup.registry, one_reg, None).expect("one");
    add_tool(&setup.registry, two_reg, None).expect("two");
    setup.faux.set_responses(vec![FauxResponseStep::Message(tool_calls(&[
        ("one", "c1"),
        ("two", "c2"),
        ("two", "c3"),
    ]))]);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open");
    opened.harness.resume().expect("resume");
    let submission = opened
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
    one_started.wait().await.expect("one started");

    let live = opened
        .harness
        .snapshot_conversation(&*LIVE_DOC, opened.root.id(), context())
        .await
        .expect("snap")
        .expect("live");
    let started_flags: Vec<bool> = live
        .tools
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|slot| slot.task_id.is_some())
        .collect();
    assert_eq!(started_flags, vec![true, false, false]);
    let generation = live.run.expect("run").task_id;
    opened.harness.abort_task(generation, context()).await.expect("abort");
    let settled_sub = submission.wait(context()).await.expect("wait");
    assert!(matches!(
        settled_sub.record.state,
        SubmissionState::Input(InputSubmissionState::Unanswered { ref reason, .. }) if reason == "aborted"
    ));

    let entries = all_entries(&opened.root, None).await.expect("entries");
    let results: Vec<_> = entries
        .iter()
        .filter(|entry| TOOL_RESULT_ENTRY.matches(Some(*entry)))
        .map(|entry| match &entry.model.as_ref().expect("model")[0] {
            Message::ToolResult(tr) => tr.clone(),
            other => panic!("expected toolResult, got {other:?}"),
        })
        .collect();
    let call_ids: Vec<&str> = results.iter().map(|r| r.tool_call_id.as_str()).collect();
    let is_errors: Vec<bool> = results.iter().map(|r| r.is_error).collect();
    assert_eq!(call_ids, vec!["c1", "c2", "c3"]);
    assert_eq!(is_errors, vec![true, true, true]);
    assert_eq!(
        text_of(&Message::ToolResult(results[1].clone())).as_deref(),
        Some("<harness>\n[error] Tool two was aborted\n</harness>")
    );

    let root_id = opened.root.id();
    let tools = opened
        .harness
        .commit(
            move |tx| async move {
                tx.scan_tasks(
                    TaskQuery {
                        conversation_id: Some(root_id),
                        kind: Some("pi.tool".to_owned()),
                        ..Default::default()
                    },
                    20,
                    None,
                )
                .await
            },
            context(),
        )
        .await
        .expect("scan tools")
        .items;
    assert_eq!(tools.len(), 1);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn ends_a_turn_at_the_generations_hold_before_its_successors_turn_starts() {
    let st = StructuredState::new();
    let node = st.node();
    let setup = chat_setup(None);
    add_task(&setup.registry, node.clone().into_any(), None).expect("add node");
    add_tool(&setup.registry, noop_tool(), None).expect("noop");

    let harness_slot = Arc::new(Mutex::new(None::<Harness>));
    let harness_cb = Arc::clone(&harness_slot);
    let node_cb = node.clone();
    add_hooks(
        &setup.registry,
        &*GENERATION_TASK,
        GenerationHooks {
            after_tools: Some(Arc::new(move |_, _, api, ctx| {
                let harness_cb = Arc::clone(&harness_cb);
                let node_cb = node_cb.clone();
                let owner = api.task_id();
                Box::pin(async move {
                    let h = harness_cb
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone()
                        .expect("harness");
                    h.commit(
                        move |tx| async move { spawn(&tx, &node_cb, owner, "hooked").await },
                        ctx,
                    )
                    .await?;
                    Ok(())
                })
            })),
            ..Default::default()
        },
        None,
    )
    .expect("add_hooks");

    setup.faux.set_responses(vec![
        FauxResponseStep::Message(tool_calls(&[("noop", "c1")])),
        FauxResponseStep::Message(done_assistant()),
    ]);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open");
    *harness_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(opened.harness.clone());
    opened.harness.resume().expect("resume");

    let stream = opened.root.watch_events(context()).await.expect("watch");
    let events = Arc::new(Mutex::new(Vec::<AgentEvent>::new()));
    let events_cb = Arc::clone(&events);
    stream
        .start(move |batch, _| {
            let events_cb = Arc::clone(&events_cb);
            Box::pin(async move {
                events_cb.lock().unwrap_or_else(|e| e.into_inner()).extend(batch);
                Ok(())
            })
        })
        .expect("start");

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
        .expect("submit")
        .wait(context())
        .await
        .expect("wait");

    let first = opened
        .harness
        .commit(
            |tx| async move {
                tx.scan_tasks(
                    TaskQuery {
                        kind: Some("pi.generation".to_owned()),
                        ..Default::default()
                    },
                    1,
                    None,
                )
                .await
            },
            context(),
        )
        .await
        .expect("scan")
        .items[0]
        .clone();
    assert_eq!(
        task_state(&opened.harness, first.id).await.status(),
        TaskStatus::Completing
    );

    let turns = |list: &[AgentEvent]| -> Vec<&'static str> {
        list.iter()
            .filter_map(|e| match e {
                AgentEvent::TurnStart => Some("turn_start"),
                AgentEvent::TurnEnd => Some("turn_end"),
                _ => None,
            })
            .collect()
    };
    until(|| {
        let events = Arc::clone(&events);
        async move { turns(&events.lock().unwrap_or_else(|e| e.into_inner())).len() == 4 }
    })
    .await;
    assert_eq!(
        turns(&events.lock().unwrap_or_else(|e| e.into_inner())),
        vec!["turn_start", "turn_end", "turn_start", "turn_end"]
    );
    st.open_ok("hooked");
    assert_eq!(outcome_of(&opened.harness, first.id).await, "completed");
    until(|| {
        let events = Arc::clone(&events);
        async move { turns(&events.lock().unwrap_or_else(|e| e.into_inner())).len() == 4 }
    })
    .await;
    stream.stop().await;
    opened.harness.close(context()).await.expect("close");
}

struct ProxyProvider {
    inner: Arc<dyn Provider + Send + Sync>,
    stream_simple_override: Option<SimpleStreamFunction>,
    cancel_deferred_override: Option<CancelDeferredFunction>,
}

impl Provider for ProxyProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn base_url(&self) -> Option<&str> {
        self.inner.base_url()
    }

    fn headers(&self) -> Option<&pi_ai::types::ProviderHeaders> {
        self.inner.headers()
    }

    fn auth(&self) -> &ProviderAuth {
        self.inner.auth()
    }

    fn get_models(&self) -> Vec<Model> {
        self.inner.get_models()
    }

    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<ApiStreamOptions>,
    ) -> AssistantMessageEventStream {
        self.inner.stream(model, context, options)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        if let Some(override_fn) = &self.stream_simple_override {
            return override_fn(model, context, options);
        }
        self.inner.stream_simple(model, context, options)
    }

    fn fetch_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<pi_ai::types::DeferredFetchOptions>,
    ) -> Option<AssistantMessageEventStream> {
        self.inner.fetch_deferred(model, handle, options)
    }

    fn cancel_deferred(
        &self,
        model: &Model,
        handle: &DeferredHandle,
        options: Option<DeferredCancelOptions>,
    ) -> Option<BoxFuture<pi_js::Result<()>>> {
        if let Some(override_fn) = &self.cancel_deferred_override {
            return Some(override_fn(model, handle, options));
        }
        self.inner.cancel_deferred(model, handle, options)
    }
}

struct RejectingStorage {
    inner: MemoryStorage,
    reject_assistant_once: Arc<Mutex<bool>>,
    reject_final: Arc<Mutex<Option<TaskId>>>,
    reject_tool_result: bool,
}

impl RejectingStorage {
    fn new(reject_assistant_once: Arc<Mutex<bool>>, reject_final: Arc<Mutex<Option<TaskId>>>) -> Self {
        Self {
            inner: MemoryStorage::new(),
            reject_assistant_once,
            reject_final,
            reject_tool_result: false,
        }
    }

    fn reject_tool_result() -> Self {
        Self {
            inner: MemoryStorage::new(),
            reject_assistant_once: Arc::new(Mutex::new(false)),
            reject_final: Arc::new(Mutex::new(None)),
            reject_tool_result: true,
        }
    }
}

#[async_trait]
impl Storage for RejectingStorage {
    async fn commit(&self, writes: Vec<StorageWrite>, context: Context) -> pi_js::Result<Seq> {
        if self.reject_tool_result
            && writes
                .iter()
                .any(|w| matches!(w, StorageWrite::Entry { value } if value.kind == "pi.tool-result"))
        {
            return Err(StorageRejected::new("Tool result is not strict JSON").into_error());
        }
        let should_reject_assistant = {
            let mut guard = self.reject_assistant_once.lock().unwrap_or_else(|e| e.into_inner());
            if *guard
                && writes
                    .iter()
                    .any(|w| matches!(w, StorageWrite::Entry { value } if value.kind == "pi.assistant"))
            {
                *guard = false;
                true
            } else {
                false
            }
        };
        if should_reject_assistant {
            return Err(StorageRejected::new("final assistant message is not strict JSON").into_error());
        }

        let should_reject_final = {
            let mut guard = self.reject_final.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(target) = *guard {
                let finalizes = writes.iter().any(|w| {
                    matches!(
                        w,
                        StorageWrite::Task { value } if value.id == target && value.state.status() == TaskStatus::Terminal
                    )
                });
                if finalizes {
                    *guard = None;
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };
        if should_reject_final {
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

#[tokio::test]
async fn keeps_run_control_with_a_faulted_generation_until_owned_work_drains_and_retries_rejected_final_commit() {
    let st = StructuredState::new();
    let node = st.node();
    let mut setup = chat_setup(None);
    let reject_assistant = Arc::new(Mutex::new(true));
    let reject_final = Arc::new(Mutex::new(None::<TaskId>));
    let storage = Arc::new(RejectingStorage::new(
        Arc::clone(&reject_assistant),
        Arc::clone(&reject_final),
    ));

    let proxy = Arc::new(ProxyProvider {
        inner: setup.faux.provider(),
        stream_simple_override: Some(Arc::new(|_, _, _| {
            let stream = create_assistant_message_event_stream();
            let stream_bg = stream.clone();
            tokio::spawn(async move {
                let partial = faux_assistant_message(
                    FauxAssistantMessageContent::Text("partial".to_owned()),
                    Some(FauxAssistantMessageOptions {
                        stop_reason: Some(StopReason::Pending),
                        ..Default::default()
                    }),
                );
                stream_bg.push(AssistantMessageEvent::Start { partial });
                let _ = pi_js::time::sleep(300, None).await;
                let final_msg = faux_assistant_message(FauxAssistantMessageContent::Text("final".to_owned()), None);
                stream_bg.push(AssistantMessageEvent::Done {
                    reason: StopReason::Stop,
                    message: final_msg,
                });
            });
            stream
        })),
        cancel_deferred_override: None,
    });
    setup.models = create_models(None);
    setup.models.set_provider(proxy);

    add_task(&setup.registry, node.clone().into_any(), None).expect("add node");
    let harness_slot = Arc::new(Mutex::new(None::<Harness>));
    let harness_cb = Arc::clone(&harness_slot);
    let node_cb = node.clone();
    add_hooks(
        &setup.registry,
        &*GENERATION_TASK,
        GenerationHooks {
            before_request: Some(Arc::new(move |messages, api, ctx| {
                let harness_cb = Arc::clone(&harness_cb);
                let node_cb = node_cb.clone();
                let owner = api.task_id();
                Box::pin(async move {
                    let h = harness_cb
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone()
                        .expect("harness");
                    h.commit(
                        move |tx| async move { spawn(&tx, &node_cb, owner, "hooked").await },
                        ctx,
                    )
                    .await?;
                    Ok(Some(messages))
                })
            })),
            ..Default::default()
        },
        None,
    )
    .expect("add hooks");

    let st_hooked = st.clone();
    st.script(
        "hooked",
        Behavior {
            abort: Some(Arc::new(move |runtime, ctx| {
                let st_hooked = st_hooked.clone();
                Box::pin(async move {
                    let _ = st_hooked.gate("abort.hooked").wait().await?;
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
            })),
            ..Default::default()
        },
    );

    let opened = open_chat(storage, &setup, OpenChatEnv::None).await.expect("open");
    *harness_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(opened.harness.clone());
    opened.harness.resume().expect("resume");

    let submission = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("hi".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit");

    until(|| {
        let harness = opened.harness.clone();
        let root_id = opened.root.id();
        async move {
            let Some(live) = harness
                .snapshot_conversation(&*LIVE_DOC, root_id, context())
                .await
                .expect("snap")
            else {
                return false;
            };
            let Some(run) = live.run else {
                return false;
            };
            task_state(&harness, run.task_id).await.status() == TaskStatus::Completing
        }
    })
    .await;

    let generation = opened
        .harness
        .snapshot_conversation(&*LIVE_DOC, opened.root.id(), context())
        .await
        .expect("snap")
        .expect("live")
        .run
        .expect("run")
        .task_id;
    assert!(matches!(
        task_state(&opened.harness, generation).await,
        TaskState::Completing {
            outcome: TaskOutcome::Faulted { .. }
        }
    ));

    let root_c = opened.root.clone();
    let sub_id = submission.id();
    let sub_waiter = track(async move { submission.wait(context()).await });
    assert!(!settled(&sub_waiter).await);
    assert_eq!(
        opened
            .harness
            .snapshot_conversation(&*LIVE_DOC, opened.root.id(), context())
            .await
            .expect("snap")
            .and_then(|s| s.run)
            .map(|r| r.task_id),
        Some(generation)
    );

    until(|| {
        let st = st.clone();
        async move { st.has_log("abort:hooked") }
    })
    .await;
    *reject_final.lock().unwrap_or_else(|e| e.into_inner()) = Some(generation);
    st.open_ok("abort.hooked");

    until(|| {
        let reports = Arc::clone(&setup.reports);
        async move {
            reports
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .any(|err| err.downcast_ref::<StorageRejected>().is_some())
        }
    })
    .await;
    assert_eq!(
        opened
            .harness
            .snapshot_conversation(&*LIVE_DOC, root_c.id(), context())
            .await
            .expect("snap")
            .and_then(|s| s.run)
            .map(|r| r.task_id),
        Some(generation)
    );
    let entries_before = all_entries(&root_c, None).await.expect("entries");
    assert_eq!(
        entries_before.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
        vec!["pi.user"]
    );
    assert!(!settled(&sub_waiter).await);

    let root_c_id = root_c.id();
    root_c
        .commit(
            move |tx| async move {
                tx.append_entry(
                    root_c_id,
                    EntryDraft {
                        kind: "note".to_owned(),
                        ..Default::default()
                    },
                )
                .await?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("note");

    let settled_sub = sub_waiter.wait().await.expect("wait");
    assert_eq!(settled_sub.record.id, sub_id);
    assert!(matches!(
        settled_sub.record.state,
        SubmissionState::Input(InputSubmissionState::Unanswered { ref reason, .. }) if reason == "faulted"
    ));
    assert_eq!(
        opened
            .harness
            .snapshot_conversation(&*LIVE_DOC, opened.root.id(), context())
            .await
            .expect("snap after"),
        Some(LiveState::default())
    );
    let entries_after = all_entries(&opened.root, None).await.expect("entries after");
    assert_eq!(
        entries_after.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
        vec!["pi.user", "note", "pi.assistant"]
    );
    opened.harness.close(context()).await.expect("close");
}

// ─── Review follow-ups ──────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VersionedInput {
    on: Vec<TaskId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
enum VersionedCheckpoint {
    Wait,
    Resume,
    Migrated,
}

fn versioned_task(version: i64) -> pi_durable::tasks::Task<VersionedInput, VersionedCheckpoint, String, ()> {
    let mut phases: IndexMap<
        String,
        pi_durable::tasks::TypedPhaseHandler<VersionedInput, VersionedCheckpoint, String, ()>,
    > = IndexMap::new();
    phases.insert(
        "wait".to_owned(),
        Arc::new(|task, runtime, ctx| {
            Box::pin(async move {
                let on = task.input.on.clone();
                runtime
                    .commit(
                        move |_tx, _current| {
                            let on = on.clone();
                            Box::pin(async move {
                                Ok(Some(NextTaskState::Waiting {
                                    checkpoint: VersionedCheckpoint::Resume,
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
        "resume".to_owned(),
        Arc::new(|_, runtime, ctx| {
            Box::pin(async move {
                runtime
                    .commit(
                        |_tx, _current| {
                            Box::pin(async {
                                Ok(Some(NextTaskState::Terminal {
                                    outcome: TaskOutcome::Completed {
                                        result: "v1".to_owned(),
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
    phases.insert(
        "migrated".to_owned(),
        Arc::new(|_, runtime, ctx| {
            Box::pin(async move {
                runtime
                    .commit(
                        |_tx, _current| {
                            Box::pin(async {
                                Ok(Some(NextTaskState::Terminal {
                                    outcome: TaskOutcome::Completed {
                                        result: "v2".to_owned(),
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

    let migrate = if version == 2 {
        Some(Arc::new(|input: JsonValue, _checkpoint: JsonValue, _from: i64| {
            let parsed: VersionedInput =
                serde_json::from_value(input).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
            Ok((parsed, VersionedCheckpoint::Migrated))
        })
            as pi_durable::tasks::TypedMigrateFn<VersionedInput, VersionedCheckpoint>)
    } else {
        None
    };

    pi_durable::tasks::define_task(pi_durable::tasks::TaskDefinition {
        name: "test.versioned".to_owned(),
        version,
        initial: Arc::new(|_| Ok(VersionedCheckpoint::Wait)),
        phases,
        abort: Arc::new(|_, runtime, ctx| {
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
        migrate,
        _hooks: std::marker::PhantomData,
    })
}

fn spawn_and_finish(
    st: &StructuredState,
    node: &NodeTask,
    name: &str,
    child: &str,
) -> Arc<Mutex<Option<TaskId<String>>>> {
    let found = Arc::new(Mutex::new(None::<TaskId<String>>));
    let found_run = Arc::clone(&found);
    let node_run = node.clone();
    let child_name = child.to_owned();
    st.script(
        name,
        Behavior {
            run: Some(Arc::new(move |runtime, ctx| {
                let found_run = Arc::clone(&found_run);
                let node_run = node_run.clone();
                let child_name = child_name.clone();
                Box::pin(async move {
                    let owner = runtime.task_id()?.erased();
                    runtime
                        .commit(
                            move |tx, _current| {
                                let found_run = Arc::clone(&found_run);
                                let node_run = node_run.clone();
                                let child_name = child_name.clone();
                                Box::pin(async move {
                                    let cid = spawn(&tx, &node_run, owner, &child_name).await?;
                                    *found_run.lock().unwrap_or_else(|e| e.into_inner()) = Some(cid);
                                    Ok(Some(NextTaskState::Running {
                                        checkpoint: NodeCheckpoint::Resume { round: 1 },
                                    }))
                                })
                            },
                            ctx,
                        )
                        .await
                })
            })),
            ..Default::default()
        },
    );
    found
}

#[tokio::test]
async fn keeps_a_waiting_task_blocked_without_its_definition_and_resumes_it_migrated_under_newer_one() {
    let st = StructuredState::new();
    let node = st.node();
    let opened = open_nodes(&st, None).await;
    let v1 = versioned_task(1);
    let registration: Installed = add_task(&opened.registry, v1.clone().into_any(), None).expect("v1");
    let other = start(&opened.root, &node, "other", own_conversation())
        .await
        .expect("other");
    let waiter = opened
        .root
        .commit(
            {
                let v1 = v1.clone();
                let other_id = other.erased();
                move |tx| async move {
                    tx.create_task(&v1, &VersionedInput { on: vec![other_id] }, own_conversation())
                        .await
                }
            },
            context(),
        )
        .await
        .expect("waiter");
    until(|| {
        let harness = opened.harness.clone();
        async move { task_state(&harness, waiter.erased()).await.status() == TaskStatus::Waiting }
    })
    .await;
    registration.dispose();
    st.open_ok("other");
    opened
        .harness
        .wait_for_task(other, context())
        .await
        .expect("wait other");
    let inspected = opened.harness.inspect(context()).await.expect("inspect");
    let entry = inspected
        .tasks
        .iter()
        .find(|t| t.record.id == waiter.erased())
        .expect("found");
    assert!(matches!(
        entry.state,
        TaskInspectionState::Blocked {
            reason: TaskBlockedReason::MissingTask,
            ..
        }
    ));
    assert_eq!(
        task_state(&opened.harness, waiter.erased()).await.status(),
        TaskStatus::Waiting
    );
    add_task(&opened.registry, versioned_task(2).into_any(), None).expect("v2");
    let rec = opened
        .harness
        .wait_for_task(waiter, context())
        .await
        .expect("wait waiter");
    assert_eq!(
        rec.outcome,
        TaskOutcome::Completed {
            result: "v2".to_owned()
        }
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn orphans_an_aborted_waiting_task_without_definition_at_once_leaving_waited_task_running() {
    let st = StructuredState::new();
    let node = st.node();
    let opened = open_nodes(&st, None).await;
    let v1 = versioned_task(1);
    let registration = add_task(&opened.registry, v1.clone().into_any(), None).expect("v1");
    let other = start(&opened.root, &node, "other", own_conversation())
        .await
        .expect("other");
    let waiter = opened
        .root
        .commit(
            {
                let v1 = v1.clone();
                let other_id = other.erased();
                move |tx| async move {
                    tx.create_task(&v1, &VersionedInput { on: vec![other_id] }, own_conversation())
                        .await
                }
            },
            context(),
        )
        .await
        .expect("waiter");
    until(|| {
        let harness = opened.harness.clone();
        async move { task_state(&harness, waiter.erased()).await.status() == TaskStatus::Waiting }
    })
    .await;
    registration.dispose();
    assert_eq!(
        opened
            .harness
            .abort_task(waiter.erased(), context())
            .await
            .expect("abort"),
        AbortTaskResult::Marked
    );
    let waiter_state = task_state(&opened.harness, waiter.erased()).await;
    match waiter_state {
        TaskState::Terminal { outcome } => assert_eq!(
            outcome,
            TaskOutcome::Orphaned {
                reason: "missing_task".to_owned()
            }
        ),
        other => panic!("expected Terminal, got {other:?}"),
    }
    assert_eq!(
        task_state(&opened.harness, other.erased()).await.status(),
        TaskStatus::Running
    );
    st.open_ok("other");
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn never_migrates_a_held_outcome_a_newer_definition_leaves_it_and_version_alone() {
    let st = StructuredState::new();
    let node = st.node();
    let opened = open_nodes(&st, None).await;

    let make_holder = |version: i64| {
        let node_c = node.clone();
        let mut phases: IndexMap<String, pi_durable::tasks::TypedPhaseHandler<(), NodeCheckpoint, String, ()>> =
            IndexMap::new();
        phases.insert(
            "run".to_owned(),
            Arc::new(move |task, runtime, ctx| {
                let node_c = node_c.clone();
                Box::pin(async move {
                    runtime
                        .commit(
                            move |tx, _current| {
                                let node_c = node_c.clone();
                                Box::pin(async move {
                                    let _ = spawn(&tx, &node_c, task.id.erased(), "child").await?;
                                    Ok(None)
                                })
                            },
                            ctx.clone(),
                        )
                        .await?;
                    runtime
                        .commit(
                            |_tx, _current| {
                                Box::pin(async {
                                    Ok(Some(NextTaskState::Terminal {
                                        outcome: TaskOutcome::Completed {
                                            result: "held".to_owned(),
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
        pi_durable::tasks::define_task(pi_durable::tasks::TaskDefinition {
            name: "test.holder".to_owned(),
            version,
            initial: Arc::new(|_| Ok(NodeCheckpoint::Run)),
            phases,
            abort: Arc::new(|_, runtime, ctx| {
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
            migrate: Some(Arc::new(|_, _, _| Err(pi_js::Error::msg("never migrates")))),
            _hooks: std::marker::PhantomData,
        })
    };

    let h1 = make_holder(1);
    add_task(&opened.registry, h1.clone().into_any(), Some("ext")).expect("h1");
    let parent = opened
        .root
        .commit(
            move |tx| async move { tx.create_task(&h1, &(), own_conversation()).await },
            context(),
        )
        .await
        .expect("parent");
    until(|| {
        let harness = opened.harness.clone();
        async move { task_state(&harness, parent.erased()).await.status() == TaskStatus::Completing }
    })
    .await;
    add_task(&opened.registry, make_holder(2).into_any(), Some("ext")).expect("h2");
    st.open_ok("child");
    let settled_parent = opened.harness.wait_for_task(parent, context()).await.expect("wait");
    assert_eq!(
        settled_parent.outcome,
        TaskOutcome::Completed {
            result: "held".to_owned()
        }
    );
    assert_eq!(settled_parent.version, 1);
    assert!(opened.reports.lock().unwrap_or_else(|e| e.into_inner()).is_empty());
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn treats_a_held_task_as_live_outcomes_rejects_and_waiter_resumes_at_final_commit() {
    let st = StructuredState::new();
    let node = st.node();
    let _child = spawn_and_finish(&st, &node, "held", "child");
    let opened = open_nodes(&st, None).await;
    let held = start(&opened.root, &node, "held", own_conversation())
        .await
        .expect("held");
    until(|| {
        let harness = opened.harness.clone();
        async move { task_state(&harness, held.erased()).await.status() == TaskStatus::Completing }
    })
    .await;

    let rejection = Arc::new(Mutex::new(String::new()));
    let rej_cb = Arc::clone(&rejection);
    let held_id = held.erased();
    st.script(
        "reader",
        Behavior {
            run: Some(Arc::new(move |runtime, ctx| {
                let rej_cb = Arc::clone(&rej_cb);
                Box::pin(async move {
                    let msg = match runtime.outcomes::<JsonValue>(&[held_id], ctx.clone()).await {
                        Ok(_) => "resolved".to_owned(),
                        Err(err) => err.message(),
                    };
                    *rej_cb.lock().unwrap_or_else(|e| e.into_inner()) = msg;
                    runtime
                        .commit(
                            move |_tx, _current| {
                                Box::pin(async move { Ok(Some(wait_on(&[held_id], JoinPolicy::AllSettled, 1))) })
                            },
                            ctx,
                        )
                        .await
                })
            })),
            ..Default::default()
        },
    );

    let reader = start(&opened.root, &node, "reader", own_conversation())
        .await
        .expect("reader");
    until(|| {
        let harness = opened.harness.clone();
        async move { task_state(&harness, reader.erased()).await.status() == TaskStatus::Waiting }
    })
    .await;
    assert_eq!(
        *rejection.lock().unwrap_or_else(|e| e.into_inner()),
        format!("Task {held_id} is not terminal")
    );
    st.open_ok("child");
    assert_eq!(outcome_of(&opened.harness, reader.erased()).await, "completed");
    assert!(st.has_log("resume:reader"));
    opened.harness.close(context()).await.expect("close");
}

// ─── Conversation abort and boundaries ──────────────────────────────────────

#[tokio::test]
async fn marks_a_held_completed_task_with_conversation_abort_work_below_aborted_outcome_stays() {
    let st = StructuredState::new();
    let node = st.node();
    let child = spawn_and_finish(&st, &node, "held", "child");
    let opened = open_nodes(&st, None).await;
    let held = start(&opened.root, &node, "held", own_conversation())
        .await
        .expect("held");
    until(|| {
        let harness = opened.harness.clone();
        async move { task_state(&harness, held.erased()).await.status() == TaskStatus::Completing }
    })
    .await;
    opened.root.abort(context(), None).await.expect("abort root");
    let cid = child.lock().unwrap_or_else(|e| e.into_inner()).expect("child id");
    assert_eq!(outcome_of(&opened.harness, cid.erased()).await, "aborted");
    let record = opened.harness.wait_for_task(held, context()).await.expect("wait");
    assert_eq!(outcome_status(&record.outcome), "completed");
    assert!(record.abort_requested);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn marks_and_awaits_only_the_background_work_reached_when_background_true_is_admitted() {
    let st = StructuredState::new();
    let node = st.node();
    let st_first = st.clone();
    st.script(
        "first",
        Behavior {
            abort: Some(Arc::new(move |runtime, ctx| {
                let st_first = st_first.clone();
                Box::pin(async move {
                    let _ = st_first.gate("abort.first").wait().await?;
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
            })),
            ..Default::default()
        },
    );

    let opened = open_nodes(&st, None).await;
    let mut bg = own_conversation();
    bg.background = true;
    let first = start(&opened.root, &node, "first", bg).await.expect("first");
    until(|| {
        let st = st.clone();
        async move { st.has_log("run:first") }
    })
    .await;
    let root_c = opened.root.clone();
    let aborting = track(async move {
        root_c
            .abort(context(), Some(ConversationAbortOptions { background: true }))
            .await
    });
    until(|| {
        let harness = opened.harness.clone();
        async move {
            harness
                .get_erased_task(first.erased(), context())
                .await
                .unwrap()
                .unwrap()
                .abort_requested
        }
    })
    .await;
    let later = start(&opened.root, &node, "later", bg).await.expect("later");
    st.open_ok("abort.first");
    aborting.wait().await.expect("aborting");
    assert_ne!(
        task_state(&opened.harness, later.erased()).await.status(),
        TaskStatus::Terminal
    );
    assert!(
        !opened
            .harness
            .get_erased_task(later.erased(), context())
            .await
            .unwrap()
            .unwrap()
            .abort_requested
    );
    opened
        .harness
        .abort_task(later.erased(), context())
        .await
        .expect("abort later");
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn stops_a_cascade_at_an_unmarked_background_task_but_not_at_a_marked_one() {
    let st = StructuredState::new();
    let node = st.node();
    let st_owner = st.clone();
    st.script(
        "owner",
        Behavior {
            abort: Some(Arc::new(move |runtime, ctx| {
                let st_owner = st_owner.clone();
                Box::pin(async move {
                    let _ = st_owner.gate("abort.owner").wait().await?;
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
            })),
            ..Default::default()
        },
    );

    let opened = open_nodes(&st, None).await;
    let (owner, outer_id, background, inner_id) = opened
        .root
        .commit(
            {
                let node = node.clone();
                move |tx| async move {
                    let owner = tx
                        .create_task(
                            &node,
                            &NodeInput {
                                name: "owner".to_owned(),
                            },
                            own_conversation(),
                        )
                        .await?
                        .erased();
                    let outer = tx
                        .create_conversation(ConversationOwnership::Task { task_id: owner })
                        .await?;
                    let mut bg_opts = own_conversation();
                    bg_opts.conversation_id = Some(outer.id);
                    bg_opts.background = true;
                    let background = tx
                        .create_task(
                            &node,
                            &NodeInput {
                                name: "background".to_owned(),
                            },
                            bg_opts,
                        )
                        .await?
                        .erased();
                    let inner = tx
                        .create_conversation(ConversationOwnership::Task { task_id: background })
                        .await?;
                    Ok((owner, outer.id, background, inner.id))
                }
            },
            context(),
        )
        .await
        .expect("tree");

    until(|| {
        let st = st.clone();
        async move { st.has_log("run:owner") && st.has_log("run:background") }
    })
    .await;
    opened.harness.abort_task(owner, context()).await.expect("abort owner");
    until(|| {
        let st = st.clone();
        async move { st.has_log("abort:owner") }
    })
    .await;
    let inner = opened
        .harness
        .conversation(inner_id, context())
        .await
        .expect("inner")
        .expect("present");
    let outer = opened
        .harness
        .conversation(outer_id, context())
        .await
        .expect("outer")
        .expect("present");
    let shielded = start(&inner, &node, "shielded", own_conversation())
        .await
        .expect("shielded");
    let exposed = start(&outer, &node, "exposed", own_conversation())
        .await
        .expect("exposed");
    assert_eq!(outcome_of(&opened.harness, exposed.erased()).await, "aborted");
    assert!(
        !opened
            .harness
            .get_erased_task(shielded.erased(), context())
            .await
            .unwrap()
            .unwrap()
            .abort_requested
    );
    opened
        .harness
        .abort_task(background, context())
        .await
        .expect("abort bg");
    assert_eq!(outcome_of(&opened.harness, shielded.erased()).await, "aborted");
    assert_eq!(outcome_of(&opened.harness, background).await, "aborted");
    st.open_ok("abort.owner");
    assert_eq!(outcome_of(&opened.harness, owner).await, "aborted");
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn retries_a_finalization_the_storage_rejected_with_the_next_commit() {
    let st = StructuredState::new();
    let node = st.node();
    let reject_final = Arc::new(Mutex::new(None::<TaskId>));
    let storage = Arc::new(RejectingStorage::new(
        Arc::new(Mutex::new(false)),
        Arc::clone(&reject_final),
    ));
    let child = spawn_and_finish(&st, &node, "parent", "child");
    let opened = open_nodes(&st, Some(storage)).await;
    let parent = start(&opened.root, &node, "parent", own_conversation())
        .await
        .expect("parent");
    until(|| {
        let harness = opened.harness.clone();
        async move { task_state(&harness, parent.erased()).await.status() == TaskStatus::Completing }
    })
    .await;
    *reject_final.lock().unwrap_or_else(|e| e.into_inner()) = Some(parent.erased());
    st.open_ok("child");
    let cid = child.lock().unwrap_or_else(|e| e.into_inner()).expect("child id");
    opened.harness.wait_for_task(cid, context()).await.expect("wait child");
    until(|| {
        let reports = Arc::clone(&opened.reports);
        async move {
            reports
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .any(|err| err.downcast_ref::<StorageRejected>().is_some())
        }
    })
    .await;
    assert_eq!(
        task_state(&opened.harness, parent.erased()).await.status(),
        TaskStatus::Completing
    );
    let root_id = opened.root.id();
    opened
        .root
        .commit(
            move |tx| async move {
                tx.append_entry(
                    root_id,
                    EntryDraft {
                        kind: "note".to_owned(),
                        ..Default::default()
                    },
                )
                .await?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("note");
    assert_eq!(outcome_of(&opened.harness, parent.erased()).await, "completed");
    opened.harness.close(context()).await.expect("close");
}

// ─── Recovery through ownership edges ───────────────────────────────────────

#[tokio::test]
async fn shows_an_abort_marked_owner_waiting_for_work_in_its_owned_conversation_before_resume_after_reopen() {
    let st = StructuredState::new();
    let node = st.node();
    let (_dir, path) = sqlite_path().await;

    let sqlite_seed = Arc::new(
        open_node_sqlite_storage(&path, NodeSqliteStorageOptions::default())
            .await
            .expect("open"),
    );
    let session = create_session(sqlite_seed);
    let (owner, inner) = session
        .commit(
            {
                let node = node.clone();
                move |tx| async move {
                    let conv = tx.create_conversation(ConversationOwnership::Ownerless).await?;
                    let mut opts = own_conversation();
                    opts.conversation_id = Some(conv.id);
                    let owner = tx
                        .create_task(
                            &node,
                            &NodeInput {
                                name: "owner".to_owned(),
                            },
                            opts,
                        )
                        .await?
                        .erased();
                    let child = tx
                        .create_conversation(ConversationOwnership::Task { task_id: owner })
                        .await?;
                    let mut inner_opts = own_conversation();
                    inner_opts.conversation_id = Some(child.id);
                    let inner = tx
                        .create_task(
                            &node,
                            &NodeInput {
                                name: "inner".to_owned(),
                            },
                            inner_opts,
                        )
                        .await?
                        .erased();
                    Ok((owner, inner))
                }
            },
            context(),
        )
        .await
        .expect("tree");
    patch_task(&session, owner, |mut rec| {
        rec.abort_requested = true;
        rec
    })
    .await;
    session.close(context()).await.expect("close seed");

    let sqlite = Arc::new(
        open_node_sqlite_storage(&path, NodeSqliteStorageOptions::default())
            .await
            .expect("open"),
    );
    let opened = open_tasks(sqlite, vec![node.into_any()], OpenTasksOptions::default())
        .await
        .expect("open tasks");
    let inspected = opened.harness.inspect(context()).await.expect("inspect");
    let found = inspected.tasks.iter().find(|t| t.record.id == owner).expect("found");
    assert!(matches!(
        &found.state,
        TaskInspectionState::Waiting { on } if on == &vec![inner]
    ));
    opened.harness.resume().expect("resume");
    assert_eq!(outcome_of(&opened.harness, owner).await, "aborted");
    let aborts: Vec<String> = st
        .logs()
        .into_iter()
        .filter(|line| line.starts_with("abort:"))
        .collect();
    assert_eq!(aborts, vec!["abort:inner", "abort:owner"]);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn keeps_the_root_busy_after_reopen_for_work_below_a_child_tasks_owned_conversation() {
    let st = StructuredState::new();
    let node = st.node();
    let (_dir, path) = sqlite_path().await;
    let inner_slot = Arc::new(Mutex::new(None::<TaskId<String>>));
    let inner_cb = Arc::clone(&inner_slot);
    let node_cb = node.clone();

    st.script(
        "child",
        Behavior {
            run: Some(Arc::new(move |runtime, ctx| {
                let inner_cb = Arc::clone(&inner_cb);
                let node_cb = node_cb.clone();
                Box::pin(async move {
                    let owner = runtime.task_id()?.erased();
                    runtime
                        .commit(
                            move |tx, _current| {
                                let inner_cb = Arc::clone(&inner_cb);
                                let node_cb = node_cb.clone();
                                Box::pin(async move {
                                    let conv = tx
                                        .create_conversation(ConversationOwnership::Task { task_id: owner })
                                        .await?;
                                    let mut opts = own_conversation();
                                    opts.conversation_id = Some(conv.id);
                                    let inner = tx
                                        .create_task(
                                            &node_cb,
                                            &NodeInput {
                                                name: "inner".to_owned(),
                                            },
                                            opts,
                                        )
                                        .await?;
                                    *inner_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(inner);
                                    Ok(Some(NextTaskState::Running {
                                        checkpoint: NodeCheckpoint::Resume { round: 1 },
                                    }))
                                })
                            },
                            ctx,
                        )
                        .await
                })
            })),
            ..Default::default()
        },
    );
    parent_of(&st, &node, "parent", &["child"], JoinPolicy::AllSettled);

    let sqlite1 = Arc::new(
        open_node_sqlite_storage(&path, NodeSqliteStorageOptions::default())
            .await
            .expect("open 1"),
    );
    let opened1 = open_nodes(&st, Some(sqlite1)).await;
    let parent = start(&opened1.root, &node, "parent", own_conversation())
        .await
        .expect("parent");
    until(|| {
        let st = st.clone();
        async move { st.has_log("run:inner") }
    })
    .await;
    opened1.harness.close(context()).await.expect("close 1");

    let sqlite2 = Arc::new(
        open_node_sqlite_storage(&path, NodeSqliteStorageOptions::default())
            .await
            .expect("open 2"),
    );
    let opened2 = open_nodes(&st, Some(sqlite2)).await;
    let r2 = opened2.root.clone();
    let idle = track(async move { r2.wait_for_idle(context()).await });
    assert!(!settled(&idle).await);
    st.open_ok("inner");
    idle.wait().await.expect("idle");
    assert_eq!(outcome_of(&opened2.harness, parent.erased()).await, "completed");
    assert!(inner_slot.lock().unwrap_or_else(|e| e.into_inner()).is_some());
    opened2.harness.close(context()).await.expect("close 2");
}

#[tokio::test]
async fn keeps_a_finished_background_ancestor_a_boundary_after_reopen_which_background_true_crosses() {
    let st = StructuredState::new();
    let node = st.node();
    let (_dir, path) = sqlite_path().await;
    let conv_slot = Arc::new(Mutex::new(None::<ConversationId>));
    let conv_cb = Arc::clone(&conv_slot);

    st.script(
        "child",
        Behavior {
            run: Some(Arc::new(move |runtime, ctx| {
                let conv_cb = Arc::clone(&conv_cb);
                Box::pin(async move {
                    let owner = runtime.task_id()?.erased();
                    runtime
                        .commit(
                            move |tx, _current| {
                                let conv_cb = Arc::clone(&conv_cb);
                                Box::pin(async move {
                                    let conv = tx
                                        .create_conversation(ConversationOwnership::Task { task_id: owner })
                                        .await?;
                                    *conv_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(conv.id);
                                    Ok(Some(NextTaskState::Running {
                                        checkpoint: NodeCheckpoint::Resume { round: 1 },
                                    }))
                                })
                            },
                            ctx,
                        )
                        .await
                })
            })),
            ..Default::default()
        },
    );
    parent_of(&st, &node, "background", &["child"], JoinPolicy::AllSettled);

    let sqlite1 = Arc::new(
        open_node_sqlite_storage(&path, NodeSqliteStorageOptions::default())
            .await
            .expect("open 1"),
    );
    let opened1 = open_nodes(&st, Some(sqlite1)).await;
    let mut bg = own_conversation();
    bg.background = true;
    let background = start(&opened1.root, &node, "background", bg).await.expect("bg");
    opened1
        .harness
        .wait_for_task(TaskId::<String>::new_unchecked(background.as_i64()), context())
        .await
        .expect("wait bg");
    opened1.harness.close(context()).await.expect("close 1");

    let sqlite2 = Arc::new(
        open_node_sqlite_storage(&path, NodeSqliteStorageOptions::default())
            .await
            .expect("open 2"),
    );
    let opened2 = open_nodes(&st, Some(sqlite2)).await;
    let cid = conv_slot.lock().unwrap_or_else(|e| e.into_inner()).expect("cid");
    let child_conv = opened2
        .harness
        .conversation(cid, context())
        .await
        .expect("conv")
        .expect("present");
    let below = start(&child_conv, &node, "below", own_conversation())
        .await
        .expect("below");
    until(|| {
        let st = st.clone();
        async move { st.has_log("run:below") }
    })
    .await;
    opened2.root.wait_for_idle(context()).await.expect("root idle");
    opened2.root.abort(context(), None).await.expect("abort root");
    assert_eq!(
        task_state(&opened2.harness, below.erased()).await.status(),
        TaskStatus::Running
    );
    opened2
        .root
        .abort(context(), Some(ConversationAbortOptions { background: true }))
        .await
        .expect("abort bg");
    assert_eq!(outcome_of(&opened2.harness, below.erased()).await, "aborted");
    opened2.harness.close(context()).await.expect("close 2");
}

// ─── Tool rounds and events ─────────────────────────────────────────────────

async fn listen(conversation: &Conversation) -> (AgentEventStream, Arc<Mutex<Vec<AgentEvent>>>) {
    let stream = conversation.watch_events(context()).await.expect("watch_events");
    let events = Arc::new(Mutex::new(Vec::<AgentEvent>::new()));
    let events_cb = Arc::clone(&events);
    stream
        .start(move |batch, _| {
            let events_cb = Arc::clone(&events_cb);
            Box::pin(async move {
                events_cb.lock().unwrap_or_else(|e| e.into_inner()).extend(batch);
                Ok(())
            })
        })
        .expect("start");
    (stream, events)
}

fn event_labels(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolExecutionEnd {
                tool_call_id, entry, ..
            } => Some(format!("end:{tool_call_id}:{}", entry.is_some())),
            AgentEvent::MessageEnd { entry } => {
                let first = entry.model.as_ref().and_then(|m| m.first());
                match first {
                    Some(Message::ToolResult(tr)) => Some(format!("result:{}", tr.tool_call_id)),
                    Some(Message::User(_)) => Some("message:user".to_owned()),
                    Some(Message::Assistant(_)) => Some("message:assistant".to_owned()),
                    Some(Message::System(_)) => Some("message:system".to_owned()),
                    None => Some("message:undefined".to_owned()),
                }
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn ends_an_aborted_sequential_rounds_unstarted_calls_with_result_entries_right_before_them() {
    let setup = chat_setup(None);
    let (one_started, one_reg) = blocking_tool("one", ToolExecutionMode::Sequential);
    let (_two_started, two_reg) = blocking_tool("two", ToolExecutionMode::Sequential);
    add_tool(&setup.registry, one_reg, None).expect("one");
    add_tool(&setup.registry, two_reg, None).expect("two");
    setup.faux.set_responses(vec![FauxResponseStep::Message(tool_calls(&[
        ("one", "c1"),
        ("two", "c2"),
        ("two", "c3"),
    ]))]);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open");
    opened.harness.resume().expect("resume");
    let (stream, events) = listen(&opened.root).await;
    let submission = opened
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
    one_started.wait().await.expect("one started");
    let gen_id = opened
        .harness
        .snapshot_conversation(&*LIVE_DOC, opened.root.id(), context())
        .await
        .expect("snap")
        .expect("live")
        .run
        .expect("run")
        .task_id;
    opened.harness.abort_task(gen_id, context()).await.expect("abort");
    submission.wait(context()).await.expect("wait");
    until(|| {
        let events = Arc::clone(&events);
        async move {
            event_labels(&events.lock().unwrap_or_else(|e| e.into_inner()))
                .iter()
                .any(|l| l == "result:c3")
        }
    })
    .await;
    let filtered: Vec<String> = event_labels(&events.lock().unwrap_or_else(|e| e.into_inner()))
        .into_iter()
        .filter(|l| !l.starts_with("message:"))
        .collect();
    assert_eq!(
        filtered,
        vec![
            "end:c1:true",
            "result:c1",
            "end:c2:true",
            "result:c2",
            "end:c3:true",
            "result:c3",
        ]
    );
    stream.stop().await;
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn emits_one_turn_end_per_generation_also_for_a_stream_attached_while_it_holds() {
    let st = StructuredState::new();
    let node = st.node();
    let setup = chat_setup(None);
    add_task(&setup.registry, node.clone().into_any(), None).expect("add node");
    add_tool(&setup.registry, noop_tool(), None).expect("noop");

    let harness_slot = Arc::new(Mutex::new(None::<Harness>));
    let harness_cb = Arc::clone(&harness_slot);
    let node_cb = node.clone();
    add_hooks(
        &setup.registry,
        &*GENERATION_TASK,
        GenerationHooks {
            after_tools: Some(Arc::new(move |_, _, api, ctx| {
                let harness_cb = Arc::clone(&harness_cb);
                let node_cb = node_cb.clone();
                let owner = api.task_id();
                Box::pin(async move {
                    let h = harness_cb
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone()
                        .expect("harness");
                    h.commit(
                        move |tx| async move { spawn(&tx, &node_cb, owner, "hooked").await },
                        ctx,
                    )
                    .await?;
                    Ok(())
                })
            })),
            ..Default::default()
        },
        None,
    )
    .expect("hooks");

    setup.faux.set_responses(vec![
        FauxResponseStep::Message(tool_calls(&[("noop", "c1")])),
        FauxResponseStep::Message(done_assistant()),
    ]);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open");
    *harness_slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(opened.harness.clone());
    opened.harness.resume().expect("resume");

    let (early_stream, early_events) = listen(&opened.root).await;
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
        .expect("submit")
        .wait(context())
        .await
        .expect("wait");
    let (late_stream, late_events) = listen(&opened.root).await;
    st.open_ok("hooked");

    let first = opened
        .harness
        .commit(
            |tx| async move {
                tx.scan_tasks(
                    TaskQuery {
                        kind: Some("pi.generation".to_owned()),
                        ..Default::default()
                    },
                    1,
                    None,
                )
                .await
            },
            context(),
        )
        .await
        .expect("scan")
        .items[0]
        .clone();
    opened
        .harness
        .wait_for_task::<JsonValue>(first.id.cast(), context())
        .await
        .expect("wait first");
    let root_id = opened.root.id();
    opened
        .root
        .commit(
            move |tx| async move {
                tx.append_entry(
                    root_id,
                    EntryDraft {
                        kind: "note".to_owned(),
                        ..Default::default()
                    },
                )
                .await?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("note");

    until(|| {
        let late_events = Arc::clone(&late_events);
        async move {
            late_events
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .any(|e| matches!(e, AgentEvent::EntryAppended { .. }))
        }
    })
    .await;

    let turn_ends = |list: &[AgentEvent]| list.iter().filter(|e| matches!(e, AgentEvent::TurnEnd)).count();
    assert_eq!(turn_ends(&early_events.lock().unwrap_or_else(|e| e.into_inner())), 2);
    assert_eq!(turn_ends(&late_events.lock().unwrap_or_else(|e| e.into_inner())), 0);
    early_stream.stop().await;
    late_stream.stop().await;
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn lets_a_tool_that_owns_live_work_finish_its_call_at_hold_while_generation_waits() {
    let st = StructuredState::new();
    let node = st.node();
    let setup = chat_setup(None);
    add_task(&setup.registry, node.clone().into_any(), None).expect("add node");
    let node_cb = node.clone();
    add_tool(
        &setup.registry,
        define_tool(ToolRegistration {
            name: "delegate".to_owned(),
            description: "Starts work in a conversation it owns".to_owned(),
            parameters: empty_schema(),
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            output_limits: None,
            execute: Arc::new(move |_args, api, call_context| {
                let node_cb = node_cb.clone();
                Box::pin(async move {
                    let owner = api.task_id();
                    api.commit(
                        move |tx| {
                            let node_cb = node_cb.clone();
                            async move {
                                let child = tx
                                    .create_conversation(ConversationOwnership::Task { task_id: owner })
                                    .await?;
                                let mut opts = own_conversation();
                                opts.conversation_id = Some(child.id);
                                tx.create_task(&node_cb, &NodeInput { name: "sub".to_owned() }, opts)
                                    .await?;
                                Ok(())
                            }
                        },
                        call_context,
                    )
                    .await?;
                    Ok(ToolExecutionResult {
                        content: Some(vec![pi_durable::harness::types::ToolResultContentBlock::Text(
                            pi_ai::TextContent {
                                text: "started".to_owned(),
                                text_signature: None,
                            },
                        )]),
                        ..Default::default()
                    })
                })
            }),
        }),
        None,
    )
    .expect("delegate");

    setup.faux.set_responses(vec![
        FauxResponseStep::Message(tool_calls(&[("delegate", "c1")])),
        FauxResponseStep::Message(done_assistant()),
    ]);
    let opened = open_chat(Arc::new(MemoryStorage::new()), &setup, OpenChatEnv::None)
        .await
        .expect("open");
    opened.harness.resume().expect("resume");
    let (stream, events) = listen(&opened.root).await;

    let submission = opened
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
    until(|| {
        let harness = opened.harness.clone();
        let root_id = opened.root.id();
        async move {
            harness
                .snapshot_conversation(&*LIVE_DOC, root_id, context())
                .await
                .expect("snap")
                .and_then(|s| {
                    s.tools
                        .as_deref()
                        .and_then(|t| t.first())
                        .map(|slot| slot.status == ToolSlotStatus::Done)
                })
                == Some(true)
        }
    })
    .await;

    let live = opened
        .harness
        .snapshot_conversation(&*LIVE_DOC, opened.root.id(), context())
        .await
        .expect("snap")
        .expect("live");
    let live_tools = live.tools.as_deref().unwrap_or_default();
    let tool_id = live_tools[0].task_id.expect("tool task id");
    assert!(live_tools[0].entry.is_some());
    assert_eq!(
        task_state(&opened.harness, tool_id).await.status(),
        TaskStatus::Completing
    );
    assert_eq!(
        task_state(&opened.harness, live.run.expect("run").task_id)
            .await
            .status(),
        TaskStatus::Waiting
    );
    until(|| {
        let events = Arc::clone(&events);
        async move {
            event_labels(&events.lock().unwrap_or_else(|e| e.into_inner()))
                .iter()
                .any(|l| l == "end:c1:true")
        }
    })
    .await;

    let sub_waiter = track(async move { submission.wait(context()).await });
    assert!(!settled(&sub_waiter).await);
    st.open_ok("sub");
    let settled_sub = sub_waiter.wait().await.expect("wait");
    assert_eq!(settled_sub.record.status(), SubmissionStatus::Done);
    assert_eq!(outcome_of(&opened.harness, tool_id).await, "completed");
    stream.stop().await;
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn holds_a_faulted_tools_slot_and_task_failed_until_the_work_it_owns_drained() {
    let st = StructuredState::new();
    let node = st.node();
    let setup = chat_setup(None);
    add_task(&setup.registry, node.clone().into_any(), None).expect("add node");
    let st_held = st.clone();
    st.script(
        "held",
        Behavior {
            abort: Some(Arc::new(move |runtime, ctx| {
                let st_held = st_held.clone();
                Box::pin(async move {
                    let _ = st_held.gate("abort.held").wait().await?;
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
            })),
            ..Default::default()
        },
    );

    let node_cb = node.clone();
    add_tool(
        &setup.registry,
        define_tool(ToolRegistration {
            name: "broken".to_owned(),
            description: "Starts owned work, then returns a result that faults at settlement".to_owned(),
            parameters: empty_schema(),
            replay: None,
            execution_mode: None,
            prepare_arguments: None,
            output_limits: None,
            execute: Arc::new(move |_args, api, call_context| {
                let node_cb = node_cb.clone();
                Box::pin(async move {
                    let owner = api.task_id();
                    api.commit(
                        move |tx| {
                            let node_cb = node_cb.clone();
                            async move {
                                let _ = spawn(&tx, &node_cb, owner, "held").await?;
                                Ok(())
                            }
                        },
                        call_context,
                    )
                    .await?;
                    Ok(ToolExecutionResult::default())
                })
            }),
        }),
        None,
    )
    .expect("broken");

    setup.faux.set_responses(vec![
        FauxResponseStep::Message(tool_calls(&[("broken", "c1")])),
        FauxResponseStep::Message(done_assistant()),
    ]);
    let opened = open_chat(
        Arc::new(RejectingStorage::reject_tool_result()),
        &setup,
        OpenChatEnv::None,
    )
    .await
    .expect("open");
    opened.harness.resume().expect("resume");
    let (stream, events) = listen(&opened.root).await;

    let submission = opened
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
    until(|| {
        let st = st.clone();
        async move { st.has_log("abort:held") }
    })
    .await;

    let live = opened
        .harness
        .snapshot_conversation(&*LIVE_DOC, opened.root.id(), context())
        .await
        .expect("snap")
        .expect("live");
    let live_tools = live.tools.as_deref().unwrap_or_default();
    let tool_id = live_tools[0].task_id.expect("tool id");
    assert!(matches!(
        task_state(&opened.harness, tool_id).await,
        TaskState::Completing {
            outcome: TaskOutcome::Faulted { .. }
        }
    ));
    assert_ne!(live_tools[0].status, ToolSlotStatus::Done);
    assert!(
        !events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|e| matches!(e, AgentEvent::TaskFailed { .. }))
    );

    st.open_ok("abort.held");
    let settled_sub = submission.wait(context()).await.expect("wait");
    assert_eq!(settled_sub.record.status(), SubmissionStatus::Done);
    assert_eq!(outcome_of(&opened.harness, tool_id).await, "faulted");
    until(|| {
        let events = Arc::clone(&events);
        async move {
            events
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .any(|e| matches!(e, AgentEvent::TaskFailed { .. }))
        }
    })
    .await;
    assert!(
        event_labels(&events.lock().unwrap_or_else(|e| e.into_inner()))
            .iter()
            .any(|l| l == "end:c1:false")
    );
    stream.stop().await;
    opened.harness.close(context()).await.expect("close");
}
