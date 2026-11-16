//! Port of packages/durable/test/harness-lifecycle.test.ts

mod support;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_ai::Type;
use pi_ai::models::Models;
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxAssistantMessageOptions, FauxContentBlock, FauxResponseStep, FauxToolCallOptions,
    faux_assistant_message, faux_text, faux_tool_call,
};
use pi_ai::types::{ModelThinkingLevel, StopReason};
use pi_chord::Context;
use pi_chord::context::with_abort_signal;
use pi_chord::types::ReplicatedStateDeliveryKind;
use pi_durable::define_tool;
use pi_durable::documents::{DocDefinitionInput, SessionDocToken, define_session_doc};
use pi_durable::harness::generation::GENERATION_TASK;
use pi_durable::harness::harness::{ConversationEntriesQuery, Harness};
use pi_durable::harness::registry::create_registry;
use pi_durable::harness::types::{
    AgentChange, ConversationCreateOptions, FieldChange, GenerationHooks, HarnessOptions, SchedulingStatus,
    SubmissionDraft, TaskInspectionState, ToolExecutionResult, ToolRegistration, UserInput,
};
use pi_durable::storage::memory::MemoryStorage;
use pi_durable::storage::sqlite::node::{NodeSqliteStorageOptions, open_node_sqlite_storage};
use pi_durable::tasks::{Task, TaskDefinition, define_task};
use pi_durable::types::{
    ConversationId, ConversationOwnership, ConversationQuery, ConversationRecord, Cursor, DocumentAddress, DocumentId,
    DocumentPoint, DocumentQuery, DocumentRecord, EntryDraft, EntryId, EntryLookup, EntryQuery, EntryRecord,
    ErasedTaskRecord, JsonValue, Page, Seq, Storage, StorageWrite, StoredDocument, SubmissionCreate, SubmissionId,
    SubmissionQuery, SubmissionRecord, SubmissionState, TaskId, TaskOptions, TaskOwnership, TaskQuery, TaskState,
    WatchEnd, WriteSubmissionState,
};
use pi_js::abort::{AbortController, AbortReason};
use pretty_assertions::assert_eq;
use serde::{Deserialize, Serialize};
use support::chat_support::{OpenChatEnv, chat_setup, open_chat};
use support::harness_support::{add_hooks, add_task, add_tool};
use support::session_support::{ControlledStorage, context, flush};
use support::task_support::{
    OpenTasksOptions, completed, counting_reader, deferred, eventually_sync, open_tasks, settled, track,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct NotesDoc {
    text: String,
}

fn notes_doc(kind: &'static str) -> SessionDocToken<NotesDoc> {
    define_session_doc(DocDefinitionInput {
        kind,
        version: 1,
        initial: || NotesDoc { text: String::new() },
        migrate: None,
        checkpoint_when: None,
    })
    .expect("notes doc")
}

fn one_step(
    name: &str,
    run: impl Fn() -> pi_js::BoxFuture<pi_js::Result<()>> + Send + Sync + 'static,
) -> Task<(), JsonValue, (), ()> {
    let run = Arc::new(run);
    let mut phases = indexmap::IndexMap::new();
    phases.insert(
        "run".to_owned(),
        Arc::new(
            move |_task: pi_durable::types::RunningTask<(), JsonValue, ()>,
                  runtime: pi_durable::tasks::TaskRuntimeHandle<(), JsonValue, (), ()>,
                  ctx: Context| {
                let run = Arc::clone(&run);
                Box::pin(async move {
                    run().await?;
                    runtime
                        .commit(|_tx, _current| Box::pin(async { Ok(Some(completed(()))) }), ctx)
                        .await
                }) as pi_js::BoxFuture<pi_js::Result<()>>
            },
        ) as _,
    );
    define_task(TaskDefinition {
        name: name.to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(serde_json::json!({ "phase": "run" }))),
        phases,
        abort: Arc::new(|_, _, _| Box::pin(async { Ok(()) })),
        migrate: None,
        _hooks: std::marker::PhantomData,
    })
}

fn noop_step(name: &str) -> Task<(), JsonValue, (), ()> {
    one_step(name, || Box::pin(async { Ok(()) }))
}

async fn start_task(
    conversation: &pi_durable::harness::harness::Conversation,
    task: &Task<(), JsonValue, (), ()>,
    background: bool,
) -> TaskId<()> {
    let task_c = task.clone();
    conversation
        .commit(
            move |tx| async move {
                tx.create_task(
                    &task_c,
                    &(),
                    TaskOptions {
                        ownership: TaskOwnership::Conversation,
                        conversation_id: None,
                        background,
                    },
                )
                .await
            },
            context(),
        )
        .await
        .expect("start task")
}

async fn storage_open(storage: &dyn Storage) -> bool {
    storage.task(TaskId::new_unchecked(1), context()).await.is_ok()
}

async fn seed_running_task(storage: &dyn Storage) {
    let conversation_id = ConversationId::new_unchecked(storage.mint_id().await.expect("mint conv"));
    let task_id = TaskId::new_unchecked(storage.mint_id().await.expect("mint task"));
    storage
        .commit(
            vec![
                StorageWrite::Conversation {
                    value: ConversationRecord {
                        id: conversation_id,
                        parent: None,
                        owner: None,
                    },
                },
                StorageWrite::Task {
                    value: ErasedTaskRecord {
                        id: task_id,
                        conversation_id,
                        kind: "test.seeded".to_owned(),
                        version: 1,
                        input: JsonValue::Null,
                        owner: None,
                        background: false,
                        abort_requested: false,
                        state: TaskState::Running {
                            checkpoint: serde_json::json!({ "phase": "run" }),
                        },
                        memos: None,
                    },
                },
            ],
            context(),
        )
        .await
        .expect("seed running task");
}

struct FailingCloseStorage {
    inner: ControlledStorage,
}

impl FailingCloseStorage {
    fn new() -> Self {
        Self {
            inner: ControlledStorage::new(),
        }
    }
}

#[async_trait]
impl Storage for FailingCloseStorage {
    async fn commit(&self, writes: Vec<StorageWrite>, context: Context) -> pi_js::Result<Seq> {
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
        self.inner.close(context).await?;
        Err(pi_js::Error::msg("close failed"))
    }
}

#[tokio::test]
async fn closes_without_a_cancelled_caller_context_and_rethrows_the_original_error_when_open_fails() {
    let storage = Arc::new(ControlledStorage::new());
    seed_running_task(storage.as_ref()).await;
    let reader = counting_reader(create_registry());
    let held = storage.hold_commits();
    storage.fail_next_commit(pi_js::Error::msg("disk full"));
    let controller = AbortController::new();
    let cancel_ctx = with_abort_signal(controller.signal(), context());
    let storage_open_ref = Arc::clone(&storage);
    let opening = tokio::spawn(async move {
        Harness::open(
            storage_open_ref,
            HarnessOptions {
                models: Models::new(None),
                registry: reader,
                settings: None,
                env: None,
                conversation_created: None,
                now: None,
                on_report: None,
            },
            cancel_ctx,
        )
        .await
    });
    held.entered().await;
    controller.abort(Some(AbortReason::from_error(&pi_js::Error::msg("caller gave up"))));
    held.release();
    let err = match opening.await.expect("join open") {
        Ok(_) => panic!("expected open failure"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("disk full"));
    assert!(!storage_open(storage.as_ref()).await);
}

#[tokio::test]
async fn reports_a_failing_close_and_still_rethrows_the_open_error() {
    let storage = Arc::new(FailingCloseStorage::new());
    seed_running_task(storage.as_ref()).await;
    storage.inner.fail_next_commit(pi_js::Error::msg("disk full"));
    let reports = Arc::new(Mutex::new(Vec::<String>::new()));
    let reports_cb = Arc::clone(&reports);
    let err = match Harness::open(
        storage,
        HarnessOptions {
            models: Models::new(None),
            registry: create_registry(),
            settings: None,
            env: None,
            conversation_created: None,
            now: None,
            on_report: Some(Arc::new(move |err| {
                reports_cb.lock().expect("lock reports").push(err.message().to_owned());
            })),
        },
        context(),
    )
    .await
    {
        Ok(_) => panic!("expected open error"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("disk full"));
    assert_eq!(
        reports.lock().expect("lock reports").clone(),
        vec!["close failed".to_owned()]
    );
}

#[tokio::test]
async fn joins_a_task_handler_that_ignores_its_signal_before_closing_storage() {
    let storage = Arc::new(MemoryStorage::new());
    let reached = deferred::<()>();
    let gate = deferred::<()>();
    let read_after_release = Arc::new(Mutex::new(None::<bool>));

    let reached_cb = reached.clone();
    let gate_cb = gate.clone();
    let read_cb = Arc::clone(&read_after_release);
    let storage_cb = Arc::clone(&storage);
    let stubborn = one_step("test.close-stubborn", move || {
        let reached_cb = reached_cb.clone();
        let gate_cb = gate_cb.clone();
        let read_cb = Arc::clone(&read_cb);
        let storage_cb = Arc::clone(&storage_cb);
        Box::pin(async move {
            reached_cb.resolve(());
            let _ = gate_cb.wait().await;
            *read_cb.lock().expect("lock read") = Some(storage_open(storage_cb.as_ref()).await);
            Ok(())
        })
    });

    let opened = open_tasks(
        Arc::clone(&storage) as Arc<dyn Storage>,
        vec![stubborn.as_any().clone()],
        OpenTasksOptions::default(),
    )
    .await
    .expect("open");
    let root = opened.harness.root(context(), None).await.expect("root");
    start_task(&root, &stubborn, false).await;
    opened.harness.resume().expect("resume");
    reached.wait().await.expect("reached");

    let harness_close = opened.harness.clone();
    let closing = track(async move { harness_close.close(context()).await });
    flush().await;
    assert!(!settled(&closing).await);
    assert!(storage_open(storage.as_ref()).await);
    gate.resolve(());
    closing.wait().await.expect("close");
    assert_eq!(*read_after_release.lock().expect("lock read"), Some(true));
    assert!(!storage_open(storage.as_ref()).await);
}

#[tokio::test]
async fn joins_a_tool_execute_and_a_hook_that_ignore_their_signal_before_closing_storage() {
    for where_ in ["tool", "hook"] {
        let setup = chat_setup(None);
        let storage = Arc::new(MemoryStorage::new());
        let reached = deferred::<()>();
        let gate = deferred::<()>();
        let read_after_release = Arc::new(Mutex::new(None::<bool>));

        let make_stubborn = |reached_cb: support::task_support::Deferred<()>,
                             gate_cb: support::task_support::Deferred<()>,
                             read_cb: Arc<Mutex<Option<bool>>>,
                             storage_cb: Arc<MemoryStorage>| {
            async move {
                reached_cb.resolve(());
                let _ = gate_cb.wait().await;
                *read_cb.lock().expect("lock read") = Some(storage_open(storage_cb.as_ref()).await);
            }
        };

        let reached_t = reached.clone();
        let gate_t = gate.clone();
        let read_t = Arc::clone(&read_after_release);
        let storage_t = Arc::clone(&storage);
        let is_tool = where_ == "tool";
        add_tool(
            &setup.registry,
            define_tool(ToolRegistration {
                name: "wait".to_owned(),
                description: "wait".to_owned(),
                parameters: Type::object([] as [(&str, pi_ai::TSchema); 0]).to_value(),
                replay: None,
                execution_mode: None,
                prepare_arguments: None,
                output_limits: None,
                execute: Arc::new(move |_, _, _| {
                    let reached_t = reached_t.clone();
                    let gate_t = gate_t.clone();
                    let read_t = Arc::clone(&read_t);
                    let storage_t = Arc::clone(&storage_t);
                    Box::pin(async move {
                        if is_tool {
                            make_stubborn(reached_t, gate_t, read_t, storage_t).await;
                        }
                        Ok(ToolExecutionResult::default())
                    })
                }),
            }),
            None,
        )
        .expect("add tool");

        let reached_h = reached.clone();
        let gate_h = gate.clone();
        let read_h = Arc::clone(&read_after_release);
        let storage_h = Arc::clone(&storage);
        let is_hook = where_ == "hook";
        add_hooks(
            &setup.registry,
            &*GENERATION_TASK,
            GenerationHooks {
                before_request: Some(Arc::new(move |_, _, _| {
                    let reached_h = reached_h.clone();
                    let gate_h = gate_h.clone();
                    let read_h = Arc::clone(&read_h);
                    let storage_h = Arc::clone(&storage_h);
                    Box::pin(async move {
                        if is_hook {
                            make_stubborn(reached_h, gate_h, read_h, storage_h).await;
                        }
                        Ok(None)
                    })
                })),
                after_response: None,
                on_yield: None,
                after_tools: None,
            },
            None,
        )
        .expect("add hooks");

        setup.faux.set_responses(vec![
            FauxResponseStep::Message(faux_assistant_message(
                FauxAssistantMessageContent::Block(FauxContentBlock::ToolCall(faux_tool_call(
                    "wait",
                    indexmap::IndexMap::new(),
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
                FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("done"))),
                None,
            )),
        ]);

        let opened = open_chat(Arc::clone(&storage) as Arc<dyn Storage>, &setup, OpenChatEnv::None)
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
        reached.wait().await.expect("reached");

        let harness_close = opened.harness.clone();
        let closing = track(async move { harness_close.close(context()).await });
        flush().await;
        assert!(!settled(&closing).await);
        assert!(storage_open(storage.as_ref()).await);
        gate.resolve(());
        closing.wait().await.expect("close");
        assert_eq!(*read_after_release.lock().expect("lock read"), Some(true));
        assert!(!storage_open(storage.as_ref()).await);
    }
}

#[tokio::test]
async fn lets_a_new_harness_open_the_same_storage_once_close_resolved_no_old_invocation_code_runs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("session.sqlite");
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let gate = deferred::<()>();
    let generation = Arc::new(AtomicUsize::new(1));

    let log_cb = Arc::clone(&log);
    let gate_cb = gate.clone();
    let gen_cb = Arc::clone(&generation);
    let stubborn = one_step("test.generations", move || {
        let log_cb = Arc::clone(&log_cb);
        let gate_cb = gate_cb.clone();
        let gen_cb = Arc::clone(&gen_cb);
        Box::pin(async move {
            let mine = gen_cb.load(Ordering::Acquire);
            log_cb.lock().expect("lock log").push(format!("start {mine}"));
            if mine == 1 {
                let _ = gate_cb.wait().await;
            }
            log_cb.lock().expect("lock log").push(format!("end {mine}"));
            Ok(())
        })
    });

    let sqlite1 = open_node_sqlite_storage(&path.to_string_lossy(), NodeSqliteStorageOptions::default())
        .await
        .expect("sqlite1");
    let first = open_tasks(
        Arc::new(sqlite1),
        vec![stubborn.as_any().clone()],
        OpenTasksOptions::default(),
    )
    .await
    .expect("open1");
    let root = first.harness.root(context(), None).await.expect("root");
    let id = start_task(&root, &stubborn, false).await;
    first.harness.resume().expect("resume1");
    eventually_sync(|| log.lock().expect("lock log").len() == 1).await;

    let harness_close = first.harness.clone();
    let log_close = Arc::clone(&log);
    let closing = track(async move {
        harness_close.close(context()).await?;
        log_close.lock().expect("lock log").push("closed".to_owned());
        Ok::<(), pi_js::Error>(())
    });
    flush().await;
    assert!(!settled(&closing).await);
    gate.resolve(());
    closing.wait().await.expect("close");
    assert_eq!(
        log.lock().expect("lock log").clone(),
        vec!["start 1", "end 1", "closed"]
    );

    generation.store(2, Ordering::Release);
    let sqlite2 = open_node_sqlite_storage(&path.to_string_lossy(), NodeSqliteStorageOptions::default())
        .await
        .expect("sqlite2");
    let second = open_tasks(
        Arc::new(sqlite2),
        vec![stubborn.as_any().clone()],
        OpenTasksOptions::default(),
    )
    .await
    .expect("open2");
    second.harness.resume().expect("resume2");
    second.harness.wait_for_task(id, context()).await.expect("wait2");
    assert_eq!(
        log.lock().expect("lock log").clone(),
        vec!["start 1", "end 1", "closed", "start 2", "end 2"]
    );
    second.harness.close(context()).await.expect("close2");
}

#[tokio::test]
async fn keeps_shutting_down_after_a_cancelled_close_and_a_second_close_awaits_the_same_shutdown() {
    let storage = Arc::new(MemoryStorage::new());
    let reached = deferred::<()>();
    let gate = deferred::<()>();
    let reached_cb = reached.clone();
    let gate_cb = gate.clone();
    let stubborn = one_step("test.close-cancelled", move || {
        let reached_cb = reached_cb.clone();
        let gate_cb = gate_cb.clone();
        Box::pin(async move {
            reached_cb.resolve(());
            let _ = gate_cb.wait().await;
            Ok(())
        })
    });

    let opened = open_tasks(
        Arc::clone(&storage) as Arc<dyn Storage>,
        vec![stubborn.as_any().clone()],
        OpenTasksOptions::default(),
    )
    .await
    .expect("open");
    let root = opened.harness.root(context(), None).await.expect("root");
    start_task(&root, &stubborn, false).await;
    opened.harness.resume().expect("resume");
    reached.wait().await.expect("reached");

    let controller = AbortController::new();
    let cancel_ctx = with_abort_signal(controller.signal(), context());
    let harness_close1 = opened.harness.clone();
    let cancelled = tokio::spawn(async move { harness_close1.close(cancel_ctx).await });
    flush().await;
    controller.abort(Some(AbortReason::from_error(&pi_js::Error::msg("stop waiting"))));
    let err = cancelled.await.expect("join").expect_err("cancelled close");
    assert!(err.to_string().contains("stop waiting"));

    let closed_commit = opened.harness.commit(|_| Box::pin(async { Ok(()) }), context()).await;
    assert!(closed_commit.expect_err("closed").to_string().contains("closed"));
    assert!(storage_open(storage.as_ref()).await);

    let harness_close2 = opened.harness.clone();
    let second = track(async move { harness_close2.close(context()).await });
    flush().await;
    assert!(!settled(&second).await);
    gate.resolve(());
    second.wait().await.expect("second close");
    assert!(!storage_open(storage.as_ref()).await);
}

#[tokio::test]
async fn settles_durably_a_commit_whose_committer_was_cancelled_while_it_was_in_storage() {
    let storage = Arc::new(ControlledStorage::new());
    let opened = open_tasks(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Vec::new(),
        OpenTasksOptions::default(),
    )
    .await
    .expect("open");
    let root = opened.harness.root(context(), None).await.expect("root");
    let held = storage.hold_commits();
    let controller = AbortController::new();
    let cancel_ctx = with_abort_signal(controller.signal(), context());
    let root_cb = root.clone();
    let committing = tokio::spawn(async move {
        let root_id = root_cb.id();
        root_cb
            .commit(
                move |tx| async move {
                    let entry = tx
                        .append_entry(
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
                    Ok(entry.id)
                },
                cancel_ctx,
            )
            .await
    });
    held.entered().await;
    controller.abort(Some(AbortReason::from_error(&pi_js::Error::msg("committer gave up"))));
    held.release();
    let id = committing.await.expect("join").expect("commit settled");
    let page = root
        .entries(ConversationEntriesQuery::default(), 10, None, context())
        .await
        .expect("entries");
    assert_eq!(page.items.iter().map(|e| e.id).collect::<Vec<_>>(), vec![id]);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn publishes_no_frame_to_states_and_watches_from_a_commit_that_settles_during_close() {
    let notes = notes_doc("test.close-frames");
    let storage = Arc::new(ControlledStorage::new());
    let opened = open_tasks(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Vec::new(),
        OpenTasksOptions::default(),
    )
    .await
    .expect("open");
    let root = opened.harness.root(context(), None).await.expect("root");
    let notes_c = notes.clone();
    opened
        .harness
        .commit(
            move |tx| async move {
                let d = tx.doc_session(&notes_c).await?;
                d.update(|v| v.text = "before".to_owned())?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("seed notes");

    let doc_state = opened
        .harness
        .document_state_session(&notes, context())
        .await
        .expect("docState")
        .expect("present");
    let doc_watch = opened
        .harness
        .watch_doc_session(&notes, context())
        .await
        .expect("docWatch")
        .expect("present");
    let view_state = root.view_state(context()).await.expect("viewState");
    let view_watch = root.watch(context()).await.expect("viewWatch");
    let graph_state = opened.harness.task_graph(context()).await.expect("graphState");
    let graph_watch = opened.harness.watch_task_graph(context()).await.expect("graphWatch");

    let frames = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let push = |label: &'static str| {
        let frames = Arc::clone(&frames);
        move || frames.lock().expect("lock frames").push(label)
    };
    let f1 = push("graphState");
    let _g_sub = graph_state.subscribe(Arc::new(move |_, _, d| {
        if d.kind == ReplicatedStateDeliveryKind::Update {
            f1();
        }
        Box::pin(async { Ok(()) })
    }));
    let f2 = push("graphWatch");
    graph_watch
        .start(Arc::new(move |_, _, _| {
            f2();
            Box::pin(async { Ok(()) })
        }))
        .expect("start graphWatch");
    let f3 = push("docState");
    let _d_sub = doc_state.subscribe(move |_, _, d| {
        if d.kind == ReplicatedStateDeliveryKind::Update {
            f3();
        }
        Box::pin(async { Ok(()) })
    });
    let f4 = push("viewState");
    let _v_sub = view_state.subscribe(Arc::new(move |_, _, d| {
        if d.kind == ReplicatedStateDeliveryKind::Update {
            f4();
        }
        Box::pin(async { Ok(()) })
    }));
    let f5 = push("docWatch");
    doc_watch
        .start(Arc::new(move |_, _, _| {
            f5();
            Box::pin(async { Ok(()) })
        }))
        .expect("start docWatch");
    let f6 = push("viewWatch");
    view_watch
        .start(Arc::new(move |_, _, _| {
            f6();
            Box::pin(async { Ok(()) })
        }))
        .expect("start viewWatch");

    let doc_value = doc_state.value();
    let view_value = view_state.value();

    let held = storage.hold_commits();
    let harness_c = opened.harness.clone();
    let root_id = root.id();
    let notes_c2 = notes.clone();
    let committing = tokio::spawn(async move {
        let task_def = noop_step("test.close-graph");
        harness_c
            .commit(
                move |tx| async move {
                    let d = tx.doc_session(&notes_c2).await?;
                    d.update(|v| v.text = "during close".to_owned())?;
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
                    tx.create_task(
                        &task_def,
                        &(),
                        TaskOptions {
                            ownership: TaskOwnership::Conversation,
                            conversation_id: Some(root_id),
                            background: false,
                        },
                    )
                    .await?;
                    Ok(())
                },
                context(),
            )
            .await
    });
    held.entered().await;
    let harness_close = opened.harness.clone();
    let closing = tokio::spawn(async move { harness_close.close(context()).await });
    held.release();
    committing.await.expect("join commit").expect("commit ok");
    closing.await.expect("join close").expect("close ok");
    flush().await;

    assert!(frames.lock().expect("lock frames").is_empty());
    assert_eq!(doc_state.value(), doc_value);
    assert_eq!(view_state.value(), view_value);
    assert_eq!(graph_state.value(), Default::default());
    assert_eq!(graph_watch.closed().await, WatchEnd::SessionClosed);
    assert_eq!(doc_watch.closed().await, WatchEnd::SessionClosed);
    assert_eq!(view_watch.closed().await, WatchEnd::SessionClosed);
    assert!(
        storage
            .last_commit()
            .expect("last commit")
            .iter()
            .any(|write| matches!(write, StorageWrite::Entry { .. }))
    );
}

#[tokio::test]
async fn leaves_no_subscription_behind_a_watch_acquisition_cancelled_on_the_line() {
    let notes = notes_doc("test.cancelled-watch");
    let storage = Arc::new(ControlledStorage::new());
    let opened = open_tasks(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Vec::new(),
        OpenTasksOptions::default(),
    )
    .await
    .expect("open");
    let root = opened.harness.root(context(), None).await.expect("root");
    let notes_c = notes.clone();
    opened
        .harness
        .commit(
            move |tx| async move {
                let d = tx.doc_session(&notes_c).await?;
                d.update(|v| v.text = "x".to_owned())?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("seed");
    let baseline_listeners = opened.harness.commit_listener_count();

    // Cancel a document watch while its acquisition loads the document on the line.
    opened.harness.unload_documents().await.expect("unload1");
    let find = storage.hold_find_document();
    let controller = AbortController::new();
    let cancel_ctx = with_abort_signal(controller.signal(), context());
    let harness_watch = opened.harness.clone();
    let notes_w = notes.clone();
    let doc_watch = tokio::spawn(async move { harness_watch.watch_doc_session(&notes_w, cancel_ctx).await });
    find.entered().await;
    controller.abort(Some(AbortReason::from_error(&pi_js::Error::msg("cancelled"))));
    find.release();
    let err = match doc_watch.await.expect("join docWatch") {
        Ok(_) => panic!("expected cancelled"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("cancelled"));
    assert_eq!(opened.harness.commit_listener_count(), baseline_listeners);

    // Cancel a view watch while it builds its mount on the line.
    opened.harness.unload_documents().await.expect("unload2");
    let find2 = storage.hold_find_document();
    let controller2 = AbortController::new();
    let cancel_ctx2 = with_abort_signal(controller2.signal(), context());
    let root_w = root.clone();
    let view_watch = tokio::spawn(async move { root_w.watch(cancel_ctx2).await });
    find2.entered().await;
    controller2.abort(Some(AbortReason::from_error(&pi_js::Error::msg("cancelled"))));
    find2.release();
    let err2 = match view_watch.await.expect("join viewWatch") {
        Ok(_) => panic!("expected cancelled"),
        Err(err) => err,
    };
    assert!(err2.to_string().contains("cancelled"));

    // No observer kept the mount: reloading after unload still reads storage freshly on the first state.
    let reads_before = storage.document_read_count();
    opened.harness.unload_documents().await.expect("unload3");
    let first = root.view_state(context()).await.expect("first view");
    assert!(storage.document_read_count() > reads_before);
    let first_value = first.value();
    first.dispose().expect("dispose first");
    let second = root.view_state(context()).await.expect("second view");
    assert_eq!(second.value(), first_value);
    second.dispose().expect("dispose second");
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn rejects_conversation_and_harness_operations_once_close_begins_inspect_queued_before_reports_closing() {
    let storage = Arc::new(ControlledStorage::new());
    let opened = open_tasks(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Vec::new(),
        OpenTasksOptions::default(),
    )
    .await
    .expect("open");
    let root = opened.harness.root(context(), None).await.expect("root");
    let root_id = root.id();
    let entry = root
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
                .await
            },
            context(),
        )
        .await
        .expect("entry");
    let submission = root
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
        .expect("submission");

    let held = storage.hold_commits();
    let harness_blk = opened.harness.clone();
    let blocking = tokio::spawn(async move {
        harness_blk
            .commit(
                move |tx| async move {
                    tx.append_entry(
                        root_id,
                        EntryDraft {
                            kind: "blocker".to_owned(),
                            model: None,
                            data: None,
                            head: None,
                            edits: None,
                        },
                    )
                    .await
                },
                context(),
            )
            .await
    });
    held.entered().await;
    let harness_insp = opened.harness.clone();
    let queued_inspect = tokio::spawn(async move { harness_insp.inspect(context()).await });
    flush().await;
    let harness_close = opened.harness.clone();
    let closing = tokio::spawn(async move { harness_close.close(context()).await });
    flush().await;

    let check_closed = |name: &str, err: pi_js::Error| {
        assert!(
            err.to_string().contains("closed"),
            "{name} did not report closed: {err}"
        );
    };
    check_closed(
        "submit",
        match root
            .submit(
                SubmissionDraft::Input {
                    request_id: None,
                    content: UserInput::Text("x".to_owned()),
                    when_busy: None,
                },
                context(),
            )
            .await
        {
            Ok(_) => panic!("submit should fail"),
            Err(err) => err,
        },
    );
    check_closed(
        "agent",
        match root.agent(context()).await {
            Ok(_) => panic!("agent should fail"),
            Err(err) => err,
        },
    );
    check_closed(
        "configure",
        root.configure(
            AgentChange {
                thinking_level: FieldChange::Set(ModelThinkingLevel::Low),
                ..Default::default()
            },
            context(),
        )
        .await
        .expect_err("configure"),
    );
    check_closed(
        "commit",
        root.commit(|_| Box::pin(async { Ok(()) }), context())
            .await
            .expect_err("commit"),
    );
    check_closed("context", root.context(context()).await.expect_err("context"));
    check_closed(
        "entries",
        root.entries(ConversationEntriesQuery::default(), 10, None, context())
            .await
            .expect_err("entries"),
    );
    check_closed(
        "fork",
        match root
            .fork(
                entry.id,
                ConversationCreateOptions {
                    ownership: ConversationOwnership::Ownerless,
                    agent: None,
                    init: None,
                },
                context(),
            )
            .await
        {
            Ok(_) => panic!("fork should fail"),
            Err(err) => err,
        },
    );
    check_closed("compact", root.compact(None, context()).await.expect_err("compact"));
    check_closed("reset", root.reset(None, context()).await.expect_err("reset"));
    check_closed("abort", root.abort(context(), None).await.expect_err("abort"));
    check_closed(
        "conversationIdle",
        root.wait_for_idle(context()).await.expect_err("conversationIdle"),
    );
    check_closed(
        "viewState",
        match root.view_state(context()).await {
            Ok(_) => panic!("viewState should fail"),
            Err(err) => err,
        },
    );
    check_closed(
        "watch",
        match root.watch(context()).await {
            Ok(_) => panic!("watch should fail"),
            Err(err) => err,
        },
    );
    check_closed(
        "taskGraph",
        match opened.harness.task_graph(context()).await {
            Ok(_) => panic!("taskGraph should fail"),
            Err(err) => err,
        },
    );
    check_closed(
        "watchTaskGraph",
        match opened.harness.watch_task_graph(context()).await {
            Ok(_) => panic!("watchTaskGraph should fail"),
            Err(err) => err,
        },
    );
    check_closed("status", submission.status(context()).await.expect_err("status"));
    check_closed("wait", submission.wait(context()).await.expect_err("wait"));
    check_closed(
        "abortSubmission",
        submission.abort(context()).await.expect_err("abortSubmission"),
    );
    check_closed(
        "root",
        match opened.harness.root(context(), None).await {
            Ok(_) => panic!("root should fail"),
            Err(err) => err,
        },
    );
    check_closed(
        "conversation",
        match opened.harness.conversation(root_id, context()).await {
            Ok(_) => panic!("conversation should fail"),
            Err(err) => err,
        },
    );
    check_closed(
        "createConversation",
        match opened
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
        {
            Ok(_) => panic!("createConversation should fail"),
            Err(err) => err,
        },
    );
    check_closed(
        "getTask",
        opened
            .harness
            .get_task::<()>(TaskId::new_unchecked(1), context())
            .await
            .expect_err("getTask"),
    );
    check_closed("inspect", opened.harness.inspect(context()).await.expect_err("inspect"));
    check_closed(
        "submission",
        match opened.harness.submission(submission.id(), context()).await {
            Ok(_) => panic!("submission should fail"),
            Err(err) => err,
        },
    );
    check_closed(
        "abortTask",
        opened
            .harness
            .abort_task(TaskId::new_unchecked(1), context())
            .await
            .expect_err("abortTask"),
    );
    check_closed(
        "waitForTask",
        opened
            .harness
            .wait_for_task::<()>(TaskId::new_unchecked(1), context())
            .await
            .expect_err("waitForTask"),
    );
    check_closed(
        "harnessIdle",
        opened.harness.wait_for_idle(context()).await.expect_err("harnessIdle"),
    );
    check_closed("usage", opened.harness.usage(context()).await.expect_err("usage"));
    check_closed("resume", opened.harness.resume().expect_err("resume"));

    held.release();
    blocking.await.expect("join blocking").expect("blocking ok");
    assert_eq!(
        queued_inspect
            .await
            .expect("join inspect")
            .expect("inspect ok")
            .scheduling,
        SchedulingStatus::Closing
    );
    closing.await.expect("join close").expect("close ok");
}

#[tokio::test]
async fn completes_reads_queued_at_the_seal_and_rejects_queued_waits_and_acquisitions_that_would_follow_commits() {
    let notes = notes_doc("test.queued-at-seal");
    let absent = notes_doc("test.queued-absent");
    let pending = noop_step("test.queued-pending");
    let storage = Arc::new(ControlledStorage::new());
    let opened = open_tasks(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Vec::new(),
        OpenTasksOptions::default(),
    )
    .await
    .expect("open");
    let root = opened.harness.root(context(), None).await.expect("root");
    let notes_c = notes.clone();
    opened
        .harness
        .commit(
            move |tx| async move {
                tx.doc_session(&notes_c).await?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("seed notes");
    let task_id = start_task(&root, &pending, false).await;
    let write = root
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
        .expect("write");
    write.wait(context()).await.expect("write wait");

    let held = storage.hold_commits();
    let harness_blk = opened.harness.clone();
    let root_id = root.id();
    let blocking = tokio::spawn(async move {
        harness_blk
            .commit(
                move |tx| async move {
                    tx.append_entry(
                        root_id,
                        EntryDraft {
                            kind: "blocker".to_owned(),
                            model: None,
                            data: None,
                            head: None,
                            edits: None,
                        },
                    )
                    .await
                },
                context(),
            )
            .await
    });
    held.entered().await;

    fn classify<T>(res: pi_js::Result<Option<T>>) -> &'static str {
        match res {
            Ok(Some(_)) => "resolved",
            Ok(None) => "undefined",
            Err(err) if err.to_string().contains("closed") => "closed",
            Err(_) => "other_error",
        }
    }
    fn classify_res<T>(res: pi_js::Result<T>) -> &'static str {
        match res {
            Ok(_) => "resolved",
            Err(err) if err.to_string().contains("closed") => "closed",
            Err(_) => "other_error",
        }
    }

    let r1 = root.clone();
    let q_context = tokio::spawn(async move { classify_res(r1.context(context()).await) });
    let h1 = opened.harness.clone();
    let n1 = notes.clone();
    let q_snapshot = tokio::spawn(async move { classify(h1.snapshot_session(&n1, context()).await) });
    let w1 = write.clone();
    let q_settled = tokio::spawn(async move { classify_res(w1.wait(context()).await) });
    let h2 = opened.harness.clone();
    let a1 = absent.clone();
    let q_absent_watch = tokio::spawn(async move { classify(h2.watch_doc_session(&a1, context()).await) });
    let h3 = opened.harness.clone();
    let a2 = absent.clone();
    let q_absent_state = tokio::spawn(async move { classify(h3.document_state_session(&a2, context()).await) });
    let h4 = opened.harness.clone();
    let q_wait_task = tokio::spawn(async move { classify_res(h4.wait_for_task(task_id, context()).await) });
    let h5 = opened.harness.clone();
    let n2 = notes.clone();
    let q_doc_state = tokio::spawn(async move { classify(h5.document_state_session(&n2, context()).await) });
    let h6 = opened.harness.clone();
    let n3 = notes.clone();
    let q_watch_doc = tokio::spawn(async move { classify(h6.watch_doc_session(&n3, context()).await) });
    let r2 = root.clone();
    let q_view_state = tokio::spawn(async move { classify_res(r2.view_state(context()).await) });
    let r3 = root.clone();
    let q_watch = tokio::spawn(async move { classify_res(r3.watch(context()).await) });
    let h7 = opened.harness.clone();
    let q_task_graph = tokio::spawn(async move { classify_res(h7.task_graph(context()).await) });

    flush().await;
    let harness_close = opened.harness.clone();
    let closing = tokio::spawn(async move { harness_close.close(context()).await });
    flush().await;
    held.release();
    blocking.await.expect("join blocking").expect("blocking ok");

    assert_eq!(q_context.await.expect("join"), "resolved");
    assert_eq!(q_snapshot.await.expect("join"), "resolved");
    assert_eq!(q_settled.await.expect("join"), "resolved");
    assert_eq!(q_absent_watch.await.expect("join"), "undefined");
    assert_eq!(q_absent_state.await.expect("join"), "undefined");
    assert_eq!(q_wait_task.await.expect("join"), "closed");
    assert_eq!(q_doc_state.await.expect("join"), "closed");
    assert_eq!(q_watch_doc.await.expect("join"), "closed");
    assert_eq!(q_view_state.await.expect("join"), "closed");
    assert_eq!(q_watch.await.expect("join"), "closed");
    assert_eq!(q_task_graph.await.expect("join"), "closed");
    closing.await.expect("join close").expect("close ok");
}

struct PausedFixture {
    harness: Harness,
    root: pi_durable::harness::harness::Conversation,
    id: TaskId<()>,
    submission_id: SubmissionId,
    ran: Arc<AtomicBool>,
}

async fn open_paused() -> PausedFixture {
    let ran = Arc::new(AtomicBool::new(false));
    let ran_cb = Arc::clone(&ran);
    let marker = one_step("test.marker", move || {
        let ran_cb = Arc::clone(&ran_cb);
        Box::pin(async move {
            ran_cb.store(true, Ordering::Release);
            Ok(())
        })
    });
    let opened = open_tasks(
        Arc::new(MemoryStorage::new()),
        vec![marker.as_any().clone()],
        OpenTasksOptions::default(),
    )
    .await
    .expect("open");
    let root = opened.harness.root(context(), None).await.expect("root");
    let id = start_task(&root, &marker, true).await;
    let root_id = root.id();
    let submission_id = opened
        .harness
        .commit(
            move |tx| async move {
                Ok(tx
                    .create_submission(SubmissionCreate {
                        conversation_id: root_id,
                        request_id: None,
                        state: SubmissionState::Write(WriteSubmissionState::Queued),
                    })
                    .await?
                    .id)
            },
            context(),
        )
        .await
        .expect("create submission");
    PausedFixture {
        harness: opened.harness,
        root,
        id,
        submission_id,
        ran,
    }
}

#[tokio::test]
async fn never_schedules_from_a_read_only_viewer() {
    let fx = open_paused().await;
    let notes = notes_doc("test.viewer-notes");
    let notes_c = notes.clone();
    fx.harness
        .commit(
            move |tx| async move {
                tx.doc_session(&notes_c).await?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("seed notes");

    fx.harness.inspect(context()).await.expect("inspect");
    fx.harness.get_task(fx.id, context()).await.expect("getTask");
    fx.harness.usage(context()).await.expect("usage");
    fx.harness.conversation(fx.root.id(), context()).await.expect("conv");
    fx.harness.snapshot_session(&notes, context()).await.expect("snapshot");
    fx.harness
        .document_state_session(&notes, context())
        .await
        .expect("docState")
        .expect("present")
        .dispose()
        .expect("dispose");
    fx.harness
        .watch_doc_session(&notes, context())
        .await
        .expect("watchDoc")
        .expect("present")
        .stop()
        .await;
    let sub = fx
        .harness
        .submission(fx.submission_id, context())
        .await
        .expect("sub")
        .expect("present");
    sub.status(context()).await.expect("status");
    fx.root.agent(context()).await.expect("agent");
    fx.root.context(context()).await.expect("context");
    fx.root
        .entries(ConversationEntriesQuery::default(), 10, None, context())
        .await
        .expect("entries");
    fx.root
        .view_state(context())
        .await
        .expect("viewState")
        .dispose()
        .expect("dispose");
    fx.root.watch(context()).await.expect("watch").stop().await;
    fx.harness
        .task_graph(context())
        .await
        .expect("taskGraph")
        .dispose()
        .expect("dispose");
    fx.harness
        .watch_task_graph(context())
        .await
        .expect("watchTaskGraph")
        .stop()
        .await;
    flush().await;
    flush().await;
    assert!(!fx.ran.load(Ordering::Acquire));
    assert_eq!(
        fx.harness.inspect(context()).await.expect("inspect2").scheduling,
        SchedulingStatus::Paused
    );
    fx.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn schedules_from_every_progress_call() {
    let calls = [
        "submit",
        "compact",
        "abort",
        "conversationIdle",
        "submissionWait",
        "waitForTask",
        "harnessIdle",
    ];
    for name in calls {
        let fx = open_paused().await;
        let root = fx.root.clone();
        let harness = fx.harness.clone();
        let id = fx.id;
        let sub_id = fx.submission_id;
        let pending = tokio::spawn(async move {
            match name {
                "submit" => {
                    let _ = root
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
                        .await;
                }
                "compact" => {
                    let _ = root.compact(None, context()).await;
                }
                "abort" => {
                    let _ = root.abort(context(), None).await;
                }
                "conversationIdle" => {
                    let _ = root.wait_for_idle(context()).await;
                }
                "submissionWait" => {
                    if let Ok(Some(sub)) = harness.submission(sub_id, context()).await {
                        let _ = sub.wait(context()).await;
                    }
                }
                "waitForTask" => {
                    let _ = harness.wait_for_task(id, context()).await;
                }
                "harnessIdle" => {
                    let _ = harness.wait_for_idle(context()).await;
                }
                _ => unreachable!(),
            }
        });
        eventually_sync(|| fx.ran.load(Ordering::Acquire)).await;
        fx.harness.close(context()).await.expect("close");
        let _ = pending.await;
    }
}

#[tokio::test]
async fn runs_what_the_registry_holds_at_resume_a_definition_installed_or_replaced_after_open() {
    let log = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let make = |label: &'static str| {
        let log = Arc::clone(&log);
        one_step("test.late-definition", move || {
            let log = Arc::clone(&log);
            Box::pin(async move {
                log.lock().expect("lock log").push(label);
                Ok(())
            })
        })
    };

    let first = open_tasks(Arc::new(MemoryStorage::new()), Vec::new(), OpenTasksOptions::default())
        .await
        .expect("open");
    let root = first.harness.root(context(), None).await.expect("root");
    let missing = start_task(&root, &make("unused"), false).await;
    let inspected1 = first.harness.inspect(context()).await.expect("inspect1");
    assert_eq!(inspected1.tasks.len(), 1);
    assert!(matches!(
        inspected1.tasks[0].state,
        TaskInspectionState::Blocked {
            reason: pi_durable::harness::types::TaskBlockedReason::MissingTask,
            ..
        }
    ));

    let installed = add_task(&first.registry, make("v1").as_any().clone(), None).expect("add v1");
    let inspected2 = first.harness.inspect(context()).await.expect("inspect2");
    assert!(matches!(inspected2.tasks[0].state, TaskInspectionState::Ready { .. }));

    installed.dispose();
    add_task(&first.registry, make("v2").as_any().clone(), None).expect("add v2");
    first.harness.resume().expect("resume");
    first.harness.wait_for_task(missing, context()).await.expect("wait");
    assert_eq!(log.lock().expect("lock log").clone(), vec!["v2"]);
    first.harness.close(context()).await.expect("close");
}
