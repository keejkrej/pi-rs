//! Port of packages/durable/test/session-tables.test.ts

mod support;

use std::collections::BTreeSet;
use std::sync::Arc;

use indexmap::IndexMap;
use pi_durable::documents::{
    DocDefinitionInput, DocFamilyDefinitionInput, RewindableConversationDocToken, TaskDocFamilyToken, TaskDocToken,
    define_rewindable_conversation_doc, define_task_doc, define_task_doc_family,
};
use pi_durable::entries::define_entry;
use pi_durable::errors::is_read_after_write;
use pi_durable::ids::{ConversationId, TaskId, id_from_number};
use pi_durable::session::{SessionImpl, TransactionScope};
use pi_durable::tasks::{Task, TaskDefinition, define_task};
use pi_durable::types::{
    CommitChange, ConversationOwnership, ConversationQuery, ConversationRecord, DocumentPoint, DocumentQuery,
    DocumentScope, EntryDraft, EntryDraftHead, EntryQuery, EntryRecord, ErasedTaskRecord, RewindableFork, SelfHead,
    Storage, StorageWrite, TableCommitChange, TaskOptions, TaskOutcome, TaskOwnership, TaskQuery, TaskState,
    TypedEntryDraft,
};
use pretty_assertions::assert_eq;
use serde::{Deserialize, Serialize};
use serde_json::json;
use support::session_support::{context, create_conversation, document_changes, flush, open_test_session};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct WorkInput {
    path: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
enum Checkpoint {
    Start,
    Next { step: i64 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct WorkResult {
    ok: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Progress {
    lines: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Notes {
    text: String,
}

struct NonStrictValue;

impl Serialize for NonStrictValue {
    fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("Value contains NaN and is not strict JSON"))
    }
}

fn work_task() -> Task<WorkInput, Checkpoint, WorkResult> {
    let mut phases: IndexMap<String, pi_durable::tasks::TypedPhaseHandler<WorkInput, Checkpoint, WorkResult, ()>> =
        IndexMap::new();
    let noop: pi_durable::tasks::TypedPhaseHandler<WorkInput, Checkpoint, WorkResult, ()> =
        Arc::new(|_task, _runtime, _ctx| Box::pin(futures::future::ready(Ok(()))));
    phases.insert("start".to_owned(), Arc::clone(&noop));
    phases.insert("next".to_owned(), noop);
    define_task(TaskDefinition {
        name: "test.work".to_owned(),
        version: 1,
        initial: Arc::new(|_| Ok(Checkpoint::Start)),
        phases,
        abort: Arc::new(|_task, _runtime, _ctx| Box::pin(futures::future::ready(Ok(())))),
        migrate: None,
        _hooks: std::marker::PhantomData,
    })
}

fn progress_doc() -> TaskDocToken<Progress> {
    define_task_doc(DocDefinitionInput {
        kind: "test.progress",
        version: 1,
        initial: || Progress { lines: Vec::new() },
        migrate: None,
        checkpoint_when: None,
    })
    .expect("progress doc")
}

fn step_doc() -> TaskDocFamilyToken<Progress, ()> {
    define_task_doc_family(DocFamilyDefinitionInput {
        kind: "test.step",
        version: 1,
        initial: |_| Progress { lines: Vec::new() },
        migrate: None,
        checkpoint_when: None,
    })
    .expect("step doc")
}

fn notes_doc() -> RewindableConversationDocToken<Notes> {
    define_rewindable_conversation_doc(
        RewindableFork::AsOf,
        DocDefinitionInput {
            kind: "test.notes",
            version: 1,
            initial: || Notes { text: String::new() },
            migrate: None,
            checkpoint_when: None,
        },
    )
    .expect("notes doc")
}

fn terminal_task(mut task: ErasedTaskRecord) -> ErasedTaskRecord {
    task.state = TaskState::Terminal {
        outcome: TaskOutcome::Completed {
            result: json!({ "ok": true }),
        },
    };
    task.memos = None;
    task
}

async fn create_work_task(
    session: &Arc<SessionImpl>,
    conversation_id: ConversationId,
    with_document: bool,
) -> TaskId<WorkResult> {
    let task = work_task();
    let progress = progress_doc();
    session
        .commit(
            move |tx| async move {
                let task_id = tx
                    .create_task(
                        &task,
                        &WorkInput { path: "a".to_owned() },
                        TaskOptions {
                            ownership: TaskOwnership::Conversation,
                            conversation_id: Some(conversation_id),
                            background: false,
                        },
                    )
                    .await?;
                if with_document {
                    tx.doc_task(&progress, task_id.erased())
                        .await?
                        .update(|p| p.lines.push("started".to_owned()))?;
                }
                Ok(task_id)
            },
            context(),
        )
        .await
        .expect("create task")
}

#[tokio::test]
async fn allows_table_reads_only_before_the_first_table_write() {
    let harness = open_test_session();
    let conversation_id = create_conversation(&harness.session).await;
    let notes = notes_doc();
    let notes_c = notes.clone();

    harness
        .session
        .commit(
            move |tx| async move {
                assert_eq!(
                    tx.conversation(conversation_id).await?,
                    Some(ConversationRecord {
                        id: conversation_id,
                        parent: None,
                        owner: None,
                    })
                );
                let conv_page = tx.scan_conversations(ConversationQuery::default(), 1, None).await?;
                assert_eq!(
                    conv_page.items,
                    vec![ConversationRecord {
                        id: conversation_id,
                        parent: None,
                        owner: None,
                    }]
                );
                assert!(conv_page.next.is_none());
                assert!(
                    tx.scan_tasks(
                        TaskQuery {
                            conversation_id: Some(conversation_id),
                            ..Default::default()
                        },
                        10,
                        None
                    )
                    .await?
                    .items
                    .is_empty()
                );
                assert!(
                    tx.scan_entries(
                        EntryQuery {
                            conversation_id,
                            min_entry_id: None,
                            max_entry_id: None,
                        },
                        10,
                        None
                    )
                    .await?
                    .items
                    .is_empty()
                );

                tx.append_entry(
                    conversation_id,
                    EntryDraft {
                        kind: "note".to_owned(),
                        ..Default::default()
                    },
                )
                .await?;

                let err = tx.conversation(conversation_id).await.expect_err("read after write");
                assert!(is_read_after_write(&err));
                let err = tx.task(id_from_number(1)).await.expect_err("task read after write");
                assert_eq!(
                    err.to_string(),
                    "Tx.task() cannot read tables after the first table write"
                );
                let err = tx.entry(id_from_number(1)).await.expect_err("entry read after write");
                assert!(is_read_after_write(&err));
                let err = tx
                    .scan_conversations(ConversationQuery::default(), 10, None)
                    .await
                    .expect_err("scan conversations after write");
                assert!(is_read_after_write(&err));
                let err = tx
                    .scan_entries(
                        EntryQuery {
                            conversation_id,
                            min_entry_id: None,
                            max_entry_id: None,
                        },
                        10,
                        None,
                    )
                    .await
                    .expect_err("scan entries after write");
                assert!(is_read_after_write(&err));

                // Document access remains available after table writes.
                tx.doc_conversation(&notes_c, conversation_id)
                    .await?
                    .update(|n| n.text = "after write".to_owned())?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("commit");

    assert_eq!(
        harness
            .facade
            .snapshot_conversation(&notes, conversation_id, context())
            .await
            .expect("snap"),
        Some(Notes {
            text: "after write".to_owned()
        })
    );
}

#[tokio::test]
async fn passes_caller_selected_limits_and_cursors_through_table_scans() {
    let harness = open_test_session();
    let ids = vec![
        create_conversation(&harness.session).await,
        create_conversation(&harness.session).await,
        create_conversation(&harness.session).await,
    ];
    let ids_c = ids.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                let first = tx.scan_conversations(ConversationQuery::default(), 2, None).await?;
                assert_eq!(
                    first.items.iter().map(|c| c.id).collect::<Vec<_>>(),
                    ids_c[..2].to_vec()
                );
                assert!(first.next.is_some());
                let second = tx
                    .scan_conversations(ConversationQuery::default(), 2, first.next)
                    .await?;
                assert_eq!(
                    second.items.iter().map(|c| c.id).collect::<Vec<_>>(),
                    ids_c[2..].to_vec()
                );
                assert!(second.next.is_none());
                Ok(())
            },
            context(),
        )
        .await
        .expect("scan pages");
}

#[tokio::test]
async fn treats_synchronous_set_task_as_the_first_table_write() {
    let harness = open_test_session();
    let conversation_id = create_conversation(&harness.session).await;
    let task_id = create_work_task(&harness.session, conversation_id, false).await;
    harness
        .session
        .commit_with(
            move |tx| async move {
                let task = tx.task(task_id.erased()).await?.expect("task");
                tx.set_task(task)?;
                let err = tx.task(task_id.erased()).await.expect_err("read after setTask");
                assert!(is_read_after_write(&err));
                Ok(())
            },
            context(),
            TransactionScope::default(),
        )
        .await
        .expect("commit_with");
}

#[tokio::test]
async fn creates_conversations_entries_and_tasks_with_minted_ids() {
    let harness = open_test_session();
    let task = work_task();
    let task_c = task.clone();
    let (conversation, first, headed, task_id) = harness
        .session
        .commit(
            move |tx| async move {
                let conversation = tx.create_conversation(ConversationOwnership::Ownerless).await?;
                let first = tx
                    .append_entry(
                        conversation.id,
                        EntryDraft {
                            kind: "note".to_owned(),
                            data: Some(json!("one")),
                            ..Default::default()
                        },
                    )
                    .await?;
                let headed = tx
                    .append_entry(
                        conversation.id,
                        EntryDraft {
                            kind: "summary".to_owned(),
                            head: Some(EntryDraftHead::SelfId(SelfHead)),
                            ..Default::default()
                        },
                    )
                    .await?;
                let task_id = tx
                    .create_task(
                        &task_c,
                        &WorkInput { path: "x".to_owned() },
                        TaskOptions {
                            ownership: TaskOwnership::Conversation,
                            conversation_id: Some(conversation.id),
                            background: true,
                        },
                    )
                    .await?;
                Ok((conversation, first, headed, task_id))
            },
            context(),
        )
        .await
        .expect("create records");

    let raw_ids: BTreeSet<i64> = [
        conversation.id.as_i64(),
        first.id.as_i64(),
        headed.id.as_i64(),
        task_id.as_i64(),
    ]
    .into_iter()
    .collect();
    assert_eq!(raw_ids.len(), 4);
    assert_eq!(headed.head, Some(headed.id));
    assert_eq!(
        first,
        EntryRecord {
            id: first.id,
            conversation_id: conversation.id,
            kind: "note".to_owned(),
            model: None,
            data: Some(json!("one")),
            head: None,
            edits: None,
            by_task_id: None,
        }
    );
    assert_eq!(
        harness
            .storage
            .entry(headed.id, context())
            .await
            .expect("entry lookup")
            .expect("present")
            .entry,
        headed
    );
    assert_eq!(
        harness.storage.task(task_id.erased(), context()).await.expect("task"),
        Some(ErasedTaskRecord {
            id: task_id.erased(),
            conversation_id: conversation.id,
            kind: "test.work".to_owned(),
            version: 1,
            input: json!({ "path": "x" }),
            owner: None,
            background: true,
            abort_requested: false,
            state: TaskState::Pending {
                checkpoint: json!({ "phase": "start" })
            },
            memos: None,
        })
    );

    flush().await;
    let publication = harness.last_publication().expect("publication");
    assert_eq!(publication.changes.len(), 4);
    let admitted = harness.storage.last_admitted_commit().expect("admitted");
    for change in &publication.changes {
        match change {
            CommitChange::Table(TableCommitChange::Conversation(c)) => {
                assert_eq!(c, &conversation);
                assert!(admitted.contains(&StorageWrite::Conversation { value: c.clone() }));
            }
            CommitChange::Table(TableCommitChange::Entry(e)) => {
                assert!(e == &first || e == &headed);
                assert!(admitted.contains(&StorageWrite::Entry { value: e.clone() }));
            }
            CommitChange::Table(TableCommitChange::Task(t)) => {
                assert!(admitted.contains(&StorageWrite::Task { value: t.clone() }));
            }
            other => panic!("Unexpected change: {other:?}"),
        }
    }

    let task_c = task.clone();
    let err = harness
        .session
        .commit(
            move |tx| async move {
                tx.create_task(
                    &task_c,
                    &WorkInput { path: "x".to_owned() },
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
        .expect_err("missing conversationId");
    assert!(err.to_string().contains("requires options.conversationId"));

    let err = harness
        .session
        .commit(
            |tx| async move {
                tx.append_entry(
                    id_from_number(12345),
                    EntryDraft {
                        kind: "note".to_owned(),
                        ..Default::default()
                    },
                )
                .await
            },
            context(),
        )
        .await
        .expect_err("missing conversation");
    assert!(err.to_string().contains("Conversation 12345 does not exist"));
}

#[tokio::test]
async fn creates_a_conversation_with_an_explicitly_staged_task_owner() {
    let harness = open_test_session();
    let parent_id = create_conversation(&harness.session).await;
    let task = work_task();
    let (supervisor_id, child) = harness
        .session
        .commit(
            move |tx| async move {
                let supervisor_id = tx
                    .create_task(
                        &task,
                        &WorkInput {
                            path: "background".to_owned(),
                        },
                        TaskOptions {
                            ownership: TaskOwnership::Conversation,
                            conversation_id: Some(parent_id),
                            background: true,
                        },
                    )
                    .await?;
                let child = tx
                    .create_conversation(ConversationOwnership::Task {
                        task_id: supervisor_id.erased(),
                    })
                    .await?;
                Ok((supervisor_id, child))
            },
            context(),
        )
        .await
        .expect("create owned child");

    assert_eq!(
        child.owner,
        Some(pi_durable::types::ConversationOwner {
            conversation_id: parent_id,
            task_id: supervisor_id.erased(),
        })
    );
    assert_eq!(
        harness.storage.conversation(child.id, context()).await.expect("conv"),
        Some(child.clone())
    );

    let child_id = child.id;
    let err = harness
        .session
        .commit_with(
            move |tx| async move {
                let mut supervisor = tx.task(supervisor_id.erased()).await?.expect("supervisor");
                supervisor.conversation_id = child_id;
                tx.set_task(supervisor)
            },
            context(),
            TransactionScope::default(),
        )
        .await
        .expect_err("cannot move task conversation");
    assert!(
        err.to_string()
            .contains(&format!("Task {supervisor_id} cannot change conversations"))
    );
    assert_eq!(
        harness
            .storage
            .task(supervisor_id.erased(), context())
            .await
            .expect("task")
            .expect("present")
            .conversation_id,
        parent_id
    );
}

#[tokio::test]
async fn rejects_missing_terminal_and_abort_marked_conversation_owners_atomically() {
    let harness = open_test_session();
    let parent_id = create_conversation(&harness.session).await;
    let moved_staged_task_id: Arc<std::sync::Mutex<Option<TaskId>>> = Arc::new(std::sync::Mutex::new(None));
    let moved_cb = Arc::clone(&moved_staged_task_id);
    let task = work_task();
    let task_c = task.clone();

    let err = harness
        .session
        .commit_with(
            move |tx| async move {
                let tid = tx
                    .create_task(
                        &task_c,
                        &WorkInput {
                            path: "move".to_owned(),
                        },
                        TaskOptions {
                            ownership: TaskOwnership::Conversation,
                            conversation_id: Some(parent_id),
                            background: false,
                        },
                    )
                    .await?;
                *moved_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(tid.erased());
                tx.set_task(ErasedTaskRecord {
                    id: tid.erased(),
                    conversation_id: id_from_number(998),
                    kind: task_c.name().to_owned(),
                    version: task_c.version(),
                    input: json!({ "path": "move" }),
                    owner: None,
                    background: false,
                    abort_requested: false,
                    state: TaskState::Pending {
                        checkpoint: json!({ "phase": "start" }),
                    },
                    memos: None,
                })
            },
            context(),
            TransactionScope::default(),
        )
        .await
        .expect_err("move staged");
    assert!(err.to_string().contains("cannot change conversations"));
    let tid = moved_staged_task_id
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .expect("staged task id");
    assert_eq!(harness.storage.task(tid, context()).await.expect("task"), None);

    let missing_task_id: TaskId = id_from_number(999);
    let err = harness
        .session
        .commit(
            move |tx| async move {
                tx.create_conversation(ConversationOwnership::Task {
                    task_id: missing_task_id,
                })
                .await
            },
            context(),
        )
        .await
        .expect_err("missing owner");
    assert!(err.to_string().contains("Conversation owner task 999 does not exist"));

    let rejected_child_id: Arc<std::sync::Mutex<Option<ConversationId>>> = Arc::new(std::sync::Mutex::new(None));
    let rejected_cb = Arc::clone(&rejected_child_id);
    let task_c = task.clone();
    let err = harness
        .session
        .commit_with(
            move |tx| async move {
                let supervisor_id = tx
                    .create_task(
                        &task_c,
                        &WorkInput {
                            path: "aborting".to_owned(),
                        },
                        TaskOptions {
                            ownership: TaskOwnership::Conversation,
                            conversation_id: Some(parent_id),
                            background: false,
                        },
                    )
                    .await?;
                let child = tx
                    .create_conversation(ConversationOwnership::Task {
                        task_id: supervisor_id.erased(),
                    })
                    .await?;
                *rejected_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(child.id);
                tx.set_task(ErasedTaskRecord {
                    id: supervisor_id.erased(),
                    conversation_id: parent_id,
                    kind: task_c.name().to_owned(),
                    version: task_c.version(),
                    input: json!({ "path": "aborting" }),
                    owner: None,
                    background: false,
                    abort_requested: true,
                    state: TaskState::Pending {
                        checkpoint: json!({ "phase": "start" }),
                    },
                    memos: None,
                })
            },
            context(),
            TransactionScope::default(),
        )
        .await
        .expect_err("abort marked owner");
    assert!(err.to_string().contains("is abort-marked"));
    let child_id = rejected_child_id
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .expect("rejected child id");
    assert_eq!(
        harness.storage.conversation(child_id, context()).await.expect("conv"),
        None
    );

    let task_c = task.clone();
    let err = harness
        .session
        .commit_with(
            move |tx| async move {
                let supervisor_id = tx
                    .create_task(
                        &task_c,
                        &WorkInput {
                            path: "terminal".to_owned(),
                        },
                        TaskOptions {
                            ownership: TaskOwnership::Conversation,
                            conversation_id: Some(parent_id),
                            background: false,
                        },
                    )
                    .await?;
                tx.create_conversation(ConversationOwnership::Task {
                    task_id: supervisor_id.erased(),
                })
                .await?;
                tx.set_task(ErasedTaskRecord {
                    id: supervisor_id.erased(),
                    conversation_id: parent_id,
                    kind: task_c.name().to_owned(),
                    version: task_c.version(),
                    input: json!({ "path": "terminal" }),
                    owner: None,
                    background: false,
                    abort_requested: false,
                    state: TaskState::Terminal {
                        outcome: TaskOutcome::Completed {
                            result: json!({ "ok": true }),
                        },
                    },
                    memos: None,
                })
            },
            context(),
            TransactionScope::default(),
        )
        .await
        .expect_err("staged terminal owner");
    assert!(err.to_string().contains("is terminal"));

    let terminal_owner_id = create_work_task(&harness.session, parent_id, false).await;
    harness
        .session
        .commit_with(
            move |tx| async move {
                let rec = tx.task(terminal_owner_id.erased()).await?.expect("task");
                tx.set_task(terminal_task(rec))
            },
            context(),
            TransactionScope::default(),
        )
        .await
        .expect("mark terminal");
    let err = harness
        .session
        .commit(
            move |tx| async move {
                tx.create_conversation(ConversationOwnership::Task {
                    task_id: terminal_owner_id.erased(),
                })
                .await
            },
            context(),
        )
        .await
        .expect_err("committed terminal owner");
    assert!(err.to_string().contains("is terminal"));
}

#[tokio::test]
async fn takes_ownership_of_table_json_and_rejects_non_strict_values() {
    let harness = open_test_session();
    let conversation_id = create_conversation(&harness.session).await;

    let mut payload = json!({ "nested": { "value": 1 } });
    let entry = harness
        .session
        .commit(
            {
                let payload_clone = payload.clone();
                move |tx| async move {
                    tx.append_entry(
                        conversation_id,
                        EntryDraft {
                            kind: "data".to_owned(),
                            data: Some(payload_clone),
                            ..Default::default()
                        },
                    )
                    .await
                }
            },
            context(),
        )
        .await
        .expect("append data");
    payload["nested"]["value"] = json!(2);
    assert_eq!(
        harness
            .storage
            .entry(entry.id, context())
            .await
            .expect("entry")
            .expect("present")
            .entry
            .data,
        Some(json!({ "nested": { "value": 1 } }))
    );

    let omitted = harness
        .session
        .commit(
            move |tx| async move {
                tx.append_entry(
                    conversation_id,
                    EntryDraft {
                        kind: "omitted".to_owned(),
                        data: None,
                        ..Default::default()
                    },
                )
                .await
            },
            context(),
        )
        .await
        .expect("append omitted");
    assert_eq!(omitted.data, None);
    let omitted_json = serde_json::to_value(&omitted).expect("omitted json");
    assert!(!omitted_json.as_object().expect("obj").contains_key("data"));
    let stored_omitted = harness
        .storage
        .entry(omitted.id, context())
        .await
        .expect("stored omitted")
        .expect("present")
        .entry;
    let stored_json = serde_json::to_value(&stored_omitted).expect("stored json");
    assert!(!stored_json.as_object().expect("obj").contains_key("data"));

    let commits = harness.storage.commit_count();
    let invalid_entry = define_entry::<NonStrictValue>("invalid").expect("entry def");
    let err = match harness
        .session
        .commit(
            move |tx| async move {
                tx.append_typed_entry(
                    &invalid_entry,
                    conversation_id,
                    TypedEntryDraft {
                        model: None,
                        data: NonStrictValue,
                        head: None,
                        edits: None,
                    },
                )
                .await
            },
            context(),
        )
        .await
    {
        Ok(_) => panic!("non-strict"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("strict JSON"));
    assert_eq!(harness.storage.commit_count(), commits);
}

#[tokio::test]
async fn replaces_task_records_completely() {
    let harness = open_test_session();
    let conversation_id = create_conversation(&harness.session).await;
    let task_id = create_work_task(&harness.session, conversation_id, false).await;

    harness
        .session
        .commit_with(
            move |tx| async move {
                let task = tx.task(task_id.erased()).await?.expect("task");
                let mut memos = IndexMap::new();
                memos.insert("choice".to_owned(), json!("b"));
                tx.set_task(ErasedTaskRecord {
                    id: task.id,
                    conversation_id: task.conversation_id,
                    kind: task.kind,
                    version: task.version,
                    input: task.input,
                    owner: task.owner,
                    background: task.background,
                    abort_requested: task.abort_requested,
                    state: TaskState::Running {
                        checkpoint: json!({ "phase": "next", "step": 2 }),
                    },
                    memos: Some(memos),
                })
            },
            context(),
            TransactionScope::default(),
        )
        .await
        .expect("replace task");

    let stored = harness
        .storage
        .task(task_id.erased(), context())
        .await
        .expect("task")
        .expect("present");
    assert_eq!(
        stored.state,
        TaskState::Running {
            checkpoint: json!({ "phase": "next", "step": 2 })
        }
    );
    assert_eq!(stored.memos.as_ref().and_then(|m| m.get("choice")), Some(&json!("b")));
}

#[tokio::test]
async fn creates_a_task_and_then_its_document_in_one_transaction_without_read_after_write() {
    let harness = open_test_session();
    let conversation_id = create_conversation(&harness.session).await;
    let task = work_task();
    let progress = progress_doc();
    let step = step_doc();

    let progress_c = progress.clone();
    let step_c = step.clone();
    let task_id = harness
        .session
        .commit(
            move |tx| async move {
                let created_task_id = tx
                    .create_task(
                        &task,
                        &WorkInput { path: "a".to_owned() },
                        TaskOptions {
                            ownership: TaskOwnership::Conversation,
                            conversation_id: Some(conversation_id),
                            background: false,
                        },
                    )
                    .await?;
                // Validation uses the candidate task record, not a caller table read.
                tx.doc_task(&progress_c, created_task_id.erased())
                    .await?
                    .update(|p| p.lines.push("created".to_owned()))?;
                tx.doc_task_family(&step_c, created_task_id.erased(), "one", ())
                    .await?
                    .update(|p| p.lines.push("step".to_owned()))?;
                Ok(created_task_id)
            },
            context(),
        )
        .await
        .expect("create task + docs");

    assert_eq!(
        harness
            .facade
            .snapshot_task(&progress, task_id.erased(), context())
            .await
            .expect("snap"),
        Some(Progress {
            lines: vec!["created".to_owned()]
        })
    );
    flush().await;
    let publication = harness.last_publication().expect("publication");
    let documents = document_changes(&publication);
    assert!(publication.changes.iter().any(|change| match change {
        CommitChange::Table(TableCommitChange::Task(t)) => t.id == task_id.erased(),
        _ => false,
    }));
    assert_eq!(documents.len(), 2);
    // Task documents derive their conversation from the task record.
    for doc_change in &documents {
        assert_eq!(doc_change.conversation_id, Some(conversation_id));
    }

    let progress_c = progress.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.create_conversation(ConversationOwnership::Ownerless).await?;
                tx.doc_task(&progress_c, task_id.erased())
                    .await?
                    .update(|p| p.lines.push("committed task".to_owned()))?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("update task doc");
    flush().await;
    assert_eq!(
        document_changes(&harness.last_publication().expect("pub"))[0].conversation_id,
        Some(conversation_id)
    );
}

#[tokio::test]
async fn rejects_task_documents_after_a_terminal_candidate() {
    let harness = open_test_session();
    let conversation_id = create_conversation(&harness.session).await;
    let task_id = create_work_task(&harness.session, conversation_id, true).await;
    let progress = progress_doc();
    let step = step_doc();

    let progress_c = progress.clone();
    let step_c = step.clone();
    harness
        .session
        .commit_with(
            move |tx| async move {
                let task = tx.task(task_id.erased()).await?.expect("task");
                let draft = tx.doc_task(&progress_c, task_id.erased()).await?;
                tx.set_task(terminal_task(task.clone()))?;
                let err = match tx.doc_task(&progress_c, task_id.erased()).await {
                    Ok(_) => panic!("terminal progress"),
                    Err(err) => err,
                };
                assert!(err.to_string().contains(&format!("Task {task_id} is terminal")));
                let err = match tx.doc_task_family(&step_c, task_id.erased(), "late", ()).await {
                    Ok(_) => panic!("terminal step"),
                    Err(err) => err,
                };
                assert!(err.to_string().contains(&format!("Task {task_id} is terminal")));
                let err = tx.set_task(task).expect_err("overwrite terminal candidate");
                assert!(err.to_string().contains("terminal candidate"));
                draft.update(|p| p.lines.push("final".to_owned()))?;
                Ok(())
            },
            context(),
            TransactionScope::default(),
        )
        .await
        .expect("commit terminal");

    let err = harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_task(&progress, task_id.erased()).await?;
                Ok(())
            },
            context(),
        )
        .await
        .expect_err("post-terminal doc access");
    assert!(err.to_string().contains(&format!("Task {task_id} is terminal")));
}

#[tokio::test]
async fn retires_task_documents_at_terminal_settlement_including_documents_created_in_the_same_transaction() {
    let harness = open_test_session();
    let conversation_id = create_conversation(&harness.session).await;
    let task_id = create_work_task(&harness.session, conversation_id, true).await;
    let progress = progress_doc();
    let step = step_doc();

    let step_c = step.clone();
    harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_task_family(&step_c, task_id.erased(), "committed", ())
                    .await?
                    .update(|p| p.lines.push("x".to_owned()))?;
                Ok(())
            },
            context(),
        )
        .await
        .expect("committed step");
    flush().await;
    let published = harness.publications_snapshot().len();

    let step_c = step.clone();
    harness
        .session
        .commit_with(
            move |tx| async move {
                let task = tx.task(task_id.erased()).await?.expect("task");
                tx.doc_task_family(&step_c, task_id.erased(), "new", ())
                    .await?
                    .update(|p| p.lines.push("created then retired".to_owned()))?;
                tx.set_task(terminal_task(task))
            },
            context(),
            TransactionScope::default(),
        )
        .await
        .expect("terminal settlement");

    let writes = harness.storage.last_commit().expect("writes");
    assert_eq!(writes.len(), 5);
    assert_eq!(
        writes.iter().filter(|w| matches!(w, StorageWrite::Task { .. })).count(),
        1
    );
    let creation_ids: Vec<_> = writes
        .iter()
        .filter_map(|w| match w {
            StorageWrite::DocumentCreate { record, .. } => Some(record.id),
            _ => None,
        })
        .collect();
    let retirement_ids: Vec<_> = writes
        .iter()
        .filter_map(|w| match w {
            StorageWrite::DocumentRetire { id } => Some(*id),
            _ => None,
        })
        .collect();
    assert_eq!(creation_ids.len(), 1);
    assert_eq!(retirement_ids.len(), 3);
    assert!(retirement_ids.contains(&creation_ids[0]));

    flush().await;
    let pubs = harness.publications_snapshot();
    assert_eq!(pubs.len(), published + 1);
    let publication = pubs.last().expect("pub");
    let documents = document_changes(publication);
    assert_eq!(
        publication
            .changes
            .iter()
            .filter(|c| matches!(c, CommitChange::Table(TableCommitChange::Task(_))))
            .count(),
        1
    );
    assert_eq!(
        documents.iter().map(|d| d.value.clone()).collect::<Vec<_>>(),
        vec![None, None, None]
    );
    for doc_change in &documents {
        assert!(doc_change.ops.is_empty());
        assert_eq!(doc_change.conversation_id, Some(conversation_id));
    }
    assert_eq!(
        harness
            .facade
            .snapshot_task(&progress, task_id.erased(), context())
            .await
            .expect("snap progress"),
        None
    );
    assert_eq!(
        harness
            .facade
            .snapshot_task_family(&step, task_id.erased(), "committed", context())
            .await
            .expect("snap step"),
        None
    );

    let alive = harness
        .storage
        .scan_documents(
            DocumentQuery {
                scope: DocumentScope::Task {
                    task_id: task_id.erased(),
                },
                at: DocumentPoint::Current,
                kind: None,
            },
            10,
            None,
            context(),
        )
        .await
        .expect("scan docs");
    assert!(alive.items.is_empty());

    let err = harness
        .session
        .commit_with(
            move |tx| async move {
                let rec = tx.task(task_id.erased()).await?.expect("task");
                tx.set_task(terminal_task(rec))
            },
            context(),
            TransactionScope::default(),
        )
        .await
        .expect_err("already terminal");
    assert!(err.to_string().contains(&format!("Task {task_id} is already terminal")));
}

#[tokio::test]
async fn validates_document_owners() {
    let harness = open_test_session();
    let progress = progress_doc();
    let notes = notes_doc();

    let err = harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_task(&progress, id_from_number(4242)).await?;
                Ok(())
            },
            context(),
        )
        .await
        .expect_err("missing task");
    assert!(err.to_string().contains("Task 4242 does not exist"));

    let err = harness
        .session
        .commit(
            move |tx| async move {
                tx.doc_conversation(&notes, id_from_number(4242)).await?;
                Ok(())
            },
            context(),
        )
        .await
        .expect_err("missing conv");
    assert!(err.to_string().contains("Conversation 4242 does not exist"));
}
