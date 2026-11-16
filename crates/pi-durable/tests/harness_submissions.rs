//! Port of packages/durable/test/harness-submissions.test.ts

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use pi_ai::providers::faux::{
    FauxAssistantMessageContent, FauxContentBlock, FauxResponseStep, faux_assistant_message, faux_text,
};
use pi_ai::types::{Message, UserMessage, UserMessageContent};
use pi_chord::Context;
use pi_chord::context::with_abort_signal;
use pi_durable::entries::{ASSISTANT_ENTRY, USER_ENTRY, define_entry};
use pi_durable::errors::is_conversation_busy;
use pi_durable::harness::generation::GENERATION_TASK;
use pi_durable::harness::live::LIVE_DOC;
use pi_durable::harness::types::{
    ConversationCreateOptions, ConversationOwnership, DocumentReaderExt, SubmissionDraft, UserInput, WhenBusy,
};
use pi_durable::storage::sqlite::node::{NodeSqliteStorage, NodeSqliteStorageOptions, open_node_sqlite_storage};
use pi_durable::tasks::{TaskOptions, TaskOwnership};
use pi_durable::types::{
    ConversationId, ConversationQuery, ConversationRecord, Cursor, DocumentAddress, DocumentId, DocumentPoint,
    DocumentQuery, DocumentRecord, EntryDraft, EntryId, EntryLookup, EntryQuery, EntryRecord, ErasedTaskRecord,
    InputSubmissionState, JsonValue, Page, Seq, Storage, StorageWrite, StoredDocument, SubmissionCreate, SubmissionId,
    SubmissionQuery, SubmissionRecord, SubmissionSettlement, SubmissionState, SubmissionStatus, TaskId, TaskQuery,
    TaskState, WriteSubmissionState,
};
use pi_js::abort::AbortController;
use pretty_assertions::assert_eq;
use serde::{Deserialize, Serialize};
use serde_json::json;
use support::chat_support::{all_entries, chat_setup, open_chat, unanswered};
use support::harness_support::OpenHarnessOptions;
use support::session_support::{ControlledStorage, context};
use support::task_support::{deferred, open_tasks};
use tempfile::TempDir;

async fn sqlite_path() -> (TempDir, String) {
    let dir = TempDir::new().expect("create tempdir");
    let path = dir.path().join("session.sqlite").to_string_lossy().into_owned();
    (dir, path)
}

async fn open_sqlite(path: &str) -> Arc<NodeSqliteStorage> {
    Arc::new(
        open_node_sqlite_storage(path, NodeSqliteStorageOptions::default())
            .await
            .expect("open sqlite"),
    )
}

struct HeldReadsStorage {
    inner: ControlledStorage,
    hold: AtomicBool,
    entered: support::task_support::Deferred<()>,
    release: support::task_support::Deferred<()>,
}

#[async_trait]
impl Storage for HeldReadsStorage {
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
        if self.hold.load(Ordering::Acquire) {
            self.entered.resolve(());
            let _ = self.release.wait().await;
        }
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
async fn appends_an_idle_write_and_settles_it_done_without_a_turn() {
    let storage: Arc<dyn Storage> = Arc::new(ControlledStorage::new());
    let opened = open_chat(storage, &chat_setup(None), support::chat_support::OpenChatEnv::None)
        .await
        .expect("open");
    let submission = opened
        .root
        .submit(
            SubmissionDraft::Write {
                request_id: None,
                entry: EntryDraft {
                    kind: "note".to_owned(),
                    model: None,
                    data: Some(json!({ "text": "x" })),
                    head: None,
                    edits: None,
                },
            },
            context(),
        )
        .await
        .expect("submit write");
    let sub_id = submission.id();
    let settled = submission.wait(context()).await.expect("wait");
    let entry_id = settled.record.state.entry().expect("entry");
    assert_eq!(settled.record.id, sub_id);
    assert_eq!(settled.record.conversation_id, opened.root.id());
    assert_eq!(
        settled.record.state,
        SubmissionState::Write(WriteSubmissionState::Done { entry: entry_id })
    );

    let entries = all_entries(&opened.root, None).await.expect("entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0],
        EntryRecord {
            id: entry_id,
            conversation_id: opened.root.id(),
            kind: "note".to_owned(),
            model: None,
            data: Some(json!({ "text": "x" })),
            head: None,
            edits: None,
            by_task_id: None,
        }
    );
    assert_eq!(
        opened
            .harness
            .snapshot_conversation(&*LIVE_DOC, opened.root.id(), context())
            .await
            .expect("live"),
        Some(pi_durable::harness::live::LiveState::default())
    );
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
                    10,
                    None,
                )
                .await
            },
            context(),
        )
        .await
        .expect("scan tasks");
    assert!(tasks.items.is_empty());
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn places_idle_input_and_rejects_busy_input_with_when_busy_reject_without_writing() {
    let storage = Arc::new(ControlledStorage::new());
    let setup = chat_setup(None);
    setup.now.set_fixed(42);
    let busy = unanswered();
    setup.faux.set_responses(vec![busy.step]);
    let opened = open_chat(
        Arc::clone(&storage) as Arc<dyn Storage>,
        &setup,
        support::chat_support::OpenChatEnv::None,
    )
    .await
    .expect("open");
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
    busy.reached.wait().await.expect("busy reached");

    let record = submission.status(context()).await.expect("status");
    let SubmissionState::Input(InputSubmissionState::Placed { entry: entry_id }) = record.state else {
        panic!("unexpected state: {:?}", record.state);
    };
    let entry = opened
        .root
        .commit(
            move |tx| async move { tx.typed_entry(&*USER_ENTRY, entry_id).await },
            context(),
        )
        .await
        .expect("commit")
        .expect("user entry");
    assert_eq!(
        entry.record.model,
        Some(vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hi".to_owned()),
            timestamp: 42,
        })])
    );

    let live = opened
        .harness
        .snapshot_conversation(&*LIVE_DOC, opened.root.id(), context())
        .await
        .expect("live")
        .expect("live some");
    let run = live.run.expect("run");
    assert_eq!(run.inputs, vec![submission.id()]);
    let task_rec = opened
        .harness
        .get_task(run.task_id.cast::<JsonValue>(), context())
        .await
        .expect("get task")
        .expect("task present");
    assert_eq!(task_rec.kind, "pi.generation");

    let commits = storage.commit_count();
    let err = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: None,
                content: UserInput::Text("again".to_owned()),
                when_busy: Some(WhenBusy::Reject),
            },
            context(),
        )
        .await
        .err()
        .expect("busy reject should fail");
    assert!(is_conversation_busy(&err));
    assert_eq!(storage.commit_count(), commits);
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn deduplicates_request_ids_per_conversation_before_any_write() {
    let storage = Arc::new(ControlledStorage::new());
    let setup = chat_setup(None);
    let busy = unanswered();
    setup.faux.set_responses(vec![busy.step]);
    let opened = open_chat(
        Arc::clone(&storage) as Arc<dyn Storage>,
        &setup,
        support::chat_support::OpenChatEnv::None,
    )
    .await
    .expect("open");
    opened.harness.resume().expect("resume");

    let first = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: Some("r1".to_owned()),
                content: UserInput::Text("hi".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("first");
    busy.reached.wait().await.expect("busy reached");

    let commits = storage.commit_count();
    let again = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: Some("r1".to_owned()),
                content: UserInput::Text("different".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("again");
    assert_eq!(again.id(), first.id());
    assert_eq!(storage.commit_count(), commits);

    let err = opened
        .root
        .submit(
            SubmissionDraft::Write {
                request_id: Some("r1".to_owned()),
                entry: EntryDraft {
                    kind: "note".to_owned(),
                    ..Default::default()
                },
            },
            context(),
        )
        .await
        .err()
        .expect("type mismatch");
    assert!(
        err.message()
            .contains("Request r1 already identifies a submission of type input")
    );
    let first_status = first.status(context()).await.expect("first status");
    assert_eq!(first_status.request_id.as_deref(), Some("r1"));
    assert_eq!(first_status.status(), SubmissionStatus::Placed);

    let other = opened
        .harness
        .create_conversation(
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                ..Default::default()
            },
            context(),
        )
        .await
        .expect("create other");
    let write = other
        .submit(
            SubmissionDraft::Write {
                request_id: Some("r1".to_owned()),
                entry: EntryDraft {
                    kind: "note".to_owned(),
                    ..Default::default()
                },
            },
            context(),
        )
        .await
        .expect("other write");
    assert_ne!(write.id(), first.id());
    let write_again = other
        .submit(
            SubmissionDraft::Write {
                request_id: Some("r1".to_owned()),
                entry: EntryDraft {
                    kind: "note".to_owned(),
                    ..Default::default()
                },
            },
            context(),
        )
        .await
        .expect("other write again");
    assert_eq!(write_again.id(), write.id());
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn reports_abort_results_and_looks_submissions_up_by_conversation() {
    let setup = chat_setup(None);
    let release = deferred();
    let release_cb = release.clone();
    setup
        .faux
        .set_responses(vec![FauxResponseStep::Factory(Arc::new(move |_, _, _, _| {
            let release_cb = release_cb.clone();
            Box::pin(async move {
                release_cb.wait().await.expect("release");
                Ok(faux_assistant_message(
                    FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("answer"))),
                    None,
                ))
            })
        }))]);
    let opened = open_chat(
        Arc::new(ControlledStorage::new()),
        &setup,
        support::chat_support::OpenChatEnv::None,
    )
    .await
    .expect("open");
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
    assert_eq!(
        submission.abort(context()).await.expect("abort"),
        pi_durable::harness::types::AbortSubmissionResult::AlreadyPlaced
    );
    assert_eq!(
        opened
            .harness
            .abort_submission(submission.id(), context(), Some(opened.root.id()))
            .await
            .expect("abort with cid"),
        pi_durable::harness::types::AbortSubmissionResult::AlreadyPlaced
    );
    let other = opened
        .harness
        .create_conversation(
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                ..Default::default()
            },
            context(),
        )
        .await
        .expect("other");
    assert_eq!(
        opened
            .harness
            .abort_submission(submission.id(), context(), Some(other.id()))
            .await
            .expect("abort wrong cid"),
        pi_durable::harness::types::AbortSubmissionResult::NotFound
    );
    let missing_id = SubmissionId::from_i64(999_999);
    assert_eq!(
        opened
            .harness
            .abort_submission(missing_id, context(), None)
            .await
            .expect("abort missing"),
        pi_durable::harness::types::AbortSubmissionResult::NotFound
    );
    assert!(
        opened
            .harness
            .submission(missing_id, context())
            .await
            .expect("get missing")
            .is_none()
    );

    release.resolve(());
    submission.wait(context()).await.expect("wait");
    assert_eq!(
        submission.abort(context()).await.expect("abort settled"),
        pi_durable::harness::types::AbortSubmissionResult::Settled
    );
    assert_eq!(
        opened
            .harness
            .abort_submission(submission.id(), context(), None)
            .await
            .expect("abort settled harness"),
        pi_durable::harness::types::AbortSubmissionResult::Settled
    );
    opened.harness.close(context()).await.expect("close");
}

#[tokio::test]
async fn cancels_only_a_wait_and_rejects_pending_waits_on_close() {
    let setup = chat_setup(None);
    setup.faux.set_responses(vec![unanswered().step]);
    let opened = open_chat(
        Arc::new(ControlledStorage::new()),
        &setup,
        support::chat_support::OpenChatEnv::None,
    )
    .await
    .expect("open");
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
    let controller = AbortController::new();
    let cancelled_ctx = with_abort_signal(&context(), controller.signal());
    let sub_c = submission.clone();
    let cancelled_fut = tokio::spawn(async move { sub_c.wait(cancelled_ctx).await });
    let sub_p = submission.clone();
    let pending_fut = tokio::spawn(async move { sub_p.wait(context()).await });
    tokio::task::yield_now().await;

    controller.abort(Some(pi_js::Error::msg("stop waiting")));
    let err = cancelled_fut
        .await
        .expect("join cancelled")
        .err()
        .expect("cancelled should reject");
    assert!(err.message().contains("stop waiting"));
    assert_eq!(
        submission.status(context()).await.expect("status").status(),
        SubmissionStatus::Placed
    );
    opened.harness.close(context()).await.expect("close");
    let err_close = pending_fut
        .await
        .expect("join pending")
        .err()
        .expect("pending should reject");
    assert!(err_close.message().contains("Harness is closed"));
}

#[tokio::test]
async fn rejects_a_wait_whose_submission_read_spans_the_start_of_close() {
    let entered = deferred();
    let release = deferred();
    let storage = Arc::new(HeldReadsStorage {
        inner: ControlledStorage::new(),
        hold: AtomicBool::new(false),
        entered: entered.clone(),
        release: release.clone(),
    });
    let setup = chat_setup(None);
    setup.faux.set_responses(vec![unanswered().step]);
    let opened = open_chat(
        Arc::clone(&storage) as Arc<dyn Storage>,
        &setup,
        support::chat_support::OpenChatEnv::None,
    )
    .await
    .expect("open");
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
    storage.hold.store(true, Ordering::Release);
    let wait_handle = tokio::spawn({
        let sub = submission.clone();
        async move { sub.wait(context()).await }
    });
    entered.wait().await.expect("entered");
    let harness_close = opened.harness.clone();
    let closing = tokio::spawn(async move { harness_close.close(context()).await });
    tokio::task::yield_now().await;
    release.resolve(());
    let wait_err = wait_handle.await.expect("join wait").err().expect("wait should reject");
    assert!(wait_err.message().contains("Harness is closed"));
    closing.await.expect("join close").expect("close ok");
}

#[tokio::test]
async fn reacquires_a_submission_after_reopen_and_settles_it_durably() {
    let (_dir, path) = sqlite_path().await;
    let setup = chat_setup(None);
    let busy = unanswered();
    setup.faux.set_responses(vec![
        busy.step,
        FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("after reopen"))),
            None,
        )),
    ]);

    let opened = open_chat(
        open_sqlite(&path).await,
        &setup,
        support::chat_support::OpenChatEnv::None,
    )
    .await
    .expect("open 1");
    let id = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: Some("print".to_owned()),
                content: UserInput::Text("hi".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("submit")
        .id();
    busy.reached.wait().await.expect("busy reached");
    opened.harness.close(context()).await.expect("close 1");

    let opened = open_chat(
        open_sqlite(&path).await,
        &setup,
        support::chat_support::OpenChatEnv::None,
    )
    .await
    .expect("open 2");
    let submission = opened
        .harness
        .submission(id, context())
        .await
        .expect("sub")
        .expect("sub some");
    assert_eq!(
        submission.status(context()).await.expect("status").status(),
        SubmissionStatus::Placed
    );
    opened.harness.resume().expect("resume 2");
    let settled = submission.wait(context()).await.expect("wait");
    assert!(matches!(
        settled.record.state,
        SubmissionState::Input(InputSubmissionState::Done { .. })
    ));
    opened.harness.close(context()).await.expect("close 2");

    let opened = open_chat(
        open_sqlite(&path).await,
        &setup,
        support::chat_support::OpenChatEnv::None,
    )
    .await
    .expect("open 3");
    let reacquired = opened
        .harness
        .submission(id, context())
        .await
        .expect("sub")
        .expect("some")
        .wait(context())
        .await
        .expect("wait 3");
    assert_eq!(reacquired, settled);
    let again = opened
        .root
        .submit(
            SubmissionDraft::Input {
                request_id: Some("print".to_owned()),
                content: UserInput::Text("hi".to_owned()),
                when_busy: None,
            },
            context(),
        )
        .await
        .expect("again");
    assert_eq!(again.id(), id);
    opened.harness.close(context()).await.expect("close 3");
}

#[tokio::test]
async fn enables_scheduling_when_a_caller_submits_or_waits() {
    let setup = chat_setup(None);
    setup
        .faux
        .set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            FauxAssistantMessageContent::Block(FauxContentBlock::Text(faux_text("answer"))),
            None,
        ))]);
    let opened = open_chat(
        Arc::new(ControlledStorage::new()),
        &setup,
        support::chat_support::OpenChatEnv::None,
    )
    .await
    .expect("open");
    let settled = opened
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
        .expect("submit")
        .wait(context())
        .await
        .expect("wait");
    assert_eq!(settled.record.status(), SubmissionStatus::Done);
    opened.harness.close(context()).await.expect("close");

    let passive = open_chat(
        Arc::new(ControlledStorage::new()),
        &chat_setup(None),
        support::chat_support::OpenChatEnv::None,
    )
    .await
    .expect("passive open");
    let task_id = passive
        .root
        .commit(
            |tx| async move {
                tx.create_task(
                    &*GENERATION_TASK,
                    &Default::default(),
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
        .expect("create task");
    let rec = passive
        .harness
        .get_task(task_id, context())
        .await
        .expect("get task")
        .expect("some");
    assert!(matches!(rec.state, TaskState::Pending { .. }));
    let done = passive
        .harness
        .wait_for_task(task_id, context())
        .await
        .expect("wait task");
    let _ = done.outcome;
    let rec_after = passive
        .harness
        .get_task(task_id, context())
        .await
        .expect("get after")
        .expect("some");
    assert!(matches!(rec_after.state, TaskState::Terminal { .. }));
    passive.harness.close(context()).await.expect("close passive");
    let _ = (open_tasks, OpenHarnessOptions::default);
}

#[tokio::test]
async fn settles_submissions_by_their_current_record_in_the_transaction() {
    let opened = open_chat(
        Arc::new(ControlledStorage::new()),
        &chat_setup(None),
        support::chat_support::OpenChatEnv::None,
    )
    .await
    .expect("open");
    let root_id = opened.root.id();
    let entry_id = opened
        .root
        .commit(
            move |tx| async move {
                Ok(tx
                    .append_entry(
                        root_id,
                        EntryDraft {
                            kind: "note".to_owned(),
                            ..Default::default()
                        },
                    )
                    .await?
                    .id)
            },
            context(),
        )
        .await
        .expect("append entry");

    let queued = opened
        .harness
        .commit(
            move |tx| async move {
                Ok(tx
                    .create_submission(SubmissionCreate {
                        conversation_id: root_id,
                        request_id: None,
                        state: SubmissionState::Input(InputSubmissionState::Queued),
                    })
                    .await?
                    .id)
            },
            context(),
        )
        .await
        .expect("queued");
    let write = opened
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
        .expect("write");

    let answer = SubmissionSettlement::Done { answer: entry_id };
    let a1 = answer.clone();
    let err1 = opened
        .root
        .commit(move |tx| async move { tx.settle_submission(queued, a1) }, context())
        .await
        .err()
        .expect("queued not placed");
    assert!(err1.message().contains("is not a placed input"));

    let a2 = answer.clone();
    let err2 = opened
        .root
        .commit(move |tx| async move { tx.settle_submission(write, a2) }, context())
        .await
        .err()
        .expect("write not placed");
    assert!(err2.message().contains("is not a placed input"));

    let err3 = opened
        .root
        .commit(
            |tx| async move {
                tx.settle_submission(
                    SubmissionId::from_i64(999_999),
                    SubmissionSettlement::Unanswered {
                        reason: "x".to_owned(),
                        detail: None,
                    },
                )
            },
            context(),
        )
        .await
        .err()
        .expect("missing submission");
    assert!(err3.message().contains("does not exist"));

    let placed = opened
        .harness
        .commit(
            move |tx| async move {
                let rec = tx
                    .create_submission(SubmissionCreate {
                        conversation_id: root_id,
                        request_id: None,
                        state: SubmissionState::Input(InputSubmissionState::Placed { entry: entry_id }),
                    })
                    .await?;
                tx.settle_submission(rec.id, answer)?;
                tx.settle_submission(
                    rec.id,
                    SubmissionSettlement::Unanswered {
                        reason: "late".to_owned(),
                        detail: None,
                    },
                )?;
                Ok(rec.id)
            },
            context(),
        )
        .await
        .expect("placed");

    let st = opened
        .harness
        .submission(placed, context())
        .await
        .expect("sub")
        .expect("some")
        .status(context())
        .await
        .expect("status");
    assert_eq!(
        st.state,
        SubmissionState::Input(InputSubmissionState::Done {
            entry: entry_id,
            answer: entry_id
        })
    );
    opened.harness.close(context()).await.expect("close");
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct CounterData {
    n: i64,
}

#[tokio::test]
async fn appends_and_reads_typed_entries_through_tokens() {
    let counter_tok = define_entry::<CounterData>("app.counter").expect("counter token");
    let marker_tok = define_entry::<()>("app.marker").expect("marker token");
    let opened = open_chat(
        Arc::new(ControlledStorage::new()),
        &chat_setup(None),
        support::chat_support::OpenChatEnv::None,
    )
    .await
    .expect("open");
    let root_id = opened.root.id();

    let counter = opened
        .root
        .commit(
            {
                let tok = counter_tok.clone();
                move |tx| async move {
                    tx.append_typed_entry(
                        &tok,
                        root_id,
                        pi_durable::types::TypedEntryDraft {
                            model: None,
                            data: CounterData { n: 1 },
                            head: None,
                            edits: None,
                        },
                    )
                    .await
                }
            },
            context(),
        )
        .await
        .expect("append counter");
    let n: i64 = counter.data.n;
    assert_eq!(n, 1);
    assert_eq!(
        counter.record,
        EntryRecord {
            id: counter.record.id,
            conversation_id: root_id,
            kind: "app.counter".to_owned(),
            model: None,
            data: Some(json!({ "n": 1 })),
            head: None,
            edits: None,
            by_task_id: None,
        }
    );

    let marker = opened
        .root
        .commit(
            {
                let tok = marker_tok.clone();
                move |tx| async move {
                    tx.append_typed_entry(
                        &tok,
                        root_id,
                        pi_durable::types::TypedEntryDraft {
                            model: None,
                            data: (),
                            head: None,
                            edits: None,
                        },
                    )
                    .await
                }
            },
            context(),
        )
        .await
        .expect("append marker");
    assert_eq!(marker.record.kind, "app.marker");

    let read_counter = opened
        .root
        .commit(
            {
                let tok = counter_tok.clone();
                let id = counter.record.id;
                move |tx| async move { tx.typed_entry(&tok, id).await }
            },
            context(),
        )
        .await
        .expect("read counter");
    assert_eq!(read_counter, Some(counter.clone()));

    let mismatch = opened
        .root
        .commit(
            {
                let tok = marker_tok.clone();
                let id = counter.record.id;
                move |tx| async move { tx.typed_entry(&tok, id).await }
            },
            context(),
        )
        .await
        .expect("read mismatch");
    assert_eq!(mismatch, None);

    let missing = opened
        .root
        .commit(
            {
                let tok = counter_tok.clone();
                move |tx| async move { tx.typed_entry(&tok, EntryId::from_i64(999_999)).await }
            },
            context(),
        )
        .await
        .expect("read missing");
    assert_eq!(missing, None);

    assert!(counter_tok.matches(&counter.record));
    assert!(!ASSISTANT_ENTRY.matches(&counter.record));
    assert_eq!([USER_ENTRY.kind(), ASSISTANT_ENTRY.kind()], ["pi.user", "pi.assistant"]);
    opened.harness.close(context()).await.expect("close");
}
