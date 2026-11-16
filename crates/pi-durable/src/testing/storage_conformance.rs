//! Port of packages/durable/src/testing/storage-conformance.ts

use std::sync::Arc;

use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::delta::{Op, OpReplace, OpSet, OpSplice, ReplaceVerb, Seg, SetVerb, SpliceVerb};
use serde::Serialize;
use serde_json::{Value as JsonValue, json};

use crate::ids::Seq;
use crate::testing::types::{
    StorageConformanceAssertions, StorageConformanceCase, StorageConformanceOptions, StorageConformanceProvider,
};
use crate::types::{
    ConversationId, ConversationOwner, ConversationParent, ConversationQuery, ConversationRecord, DocumentAddress,
    DocumentContent, DocumentCopySource, DocumentCreate, DocumentFork, DocumentHistory, DocumentId, DocumentPoint,
    DocumentQuery, DocumentScope, EntryId, EntryQuery, EntryRecord, ErasedTaskRecord, InputSubmissionState, JoinPolicy,
    JsonObject, ROOT_CONVERSATION_ID, Storage, StorageWrite, SubmissionId, SubmissionQuery, SubmissionRecord,
    SubmissionState, SubmissionStatus, TaskId, TaskOutcome, TaskOutcomeError, TaskQuery, TaskState, TaskStatus,
    WriteSubmissionState,
};

fn to_val<T: Serialize>(value: &T) -> JsonValue {
    match serde_json::to_value(value) {
        Ok(v) => v,
        Err(err) => panic!("failed to convert value to JSON for assertion: {err}"),
    }
}

fn json_obj(val: JsonValue) -> JsonObject {
    match val {
        JsonValue::Object(m) => m,
        other => panic!("expected JSON object, got {other:?}"),
    }
}

async fn create_root(storage: &dyn Storage) -> ConversationId {
    let ctx = BACKGROUND_CONTEXT.clone();
    match storage
        .commit(
            vec![StorageWrite::Conversation {
                value: ConversationRecord {
                    id: ROOT_CONVERSATION_ID,
                    parent: None,
                    owner: None,
                },
            }],
            ctx,
        )
        .await
    {
        Ok(_) => ROOT_CONVERSATION_ID,
        Err(err) => panic!("failed to create root conversation: {err}"),
    }
}

fn pending_task(id: TaskId, conversation_id: ConversationId, phase: &str) -> ErasedTaskRecord {
    ErasedTaskRecord {
        id,
        conversation_id,
        kind: "test.task".to_string(),
        version: 1,
        input: json!({ "value": id }),
        owner: None,
        background: false,
        abort_requested: false,
        state: TaskState::Pending {
            checkpoint: json!({ "phase": phase }),
        },
        memos: None,
    }
}

fn make_entry(id: EntryId, conversation_id: ConversationId, kind: &str) -> EntryRecord {
    EntryRecord {
        id,
        conversation_id,
        kind: kind.to_string(),
        model: None,
        data: None,
        head: None,
        edits: None,
        by_task_id: None,
    }
}

async fn mint_conv(storage: &dyn Storage) -> ConversationId {
    match storage.mint_id().await {
        Ok(raw) => ConversationId::new_unchecked(raw),
        Err(err) => panic!("mint_id failed: {err}"),
    }
}

async fn mint_entry(storage: &dyn Storage) -> EntryId {
    match storage.mint_id().await {
        Ok(raw) => EntryId::new_unchecked(raw),
        Err(err) => panic!("mint_id failed: {err}"),
    }
}

async fn mint_task(storage: &dyn Storage) -> TaskId {
    match storage.mint_id().await {
        Ok(raw) => TaskId::new_unchecked(raw),
        Err(err) => panic!("mint_id failed: {err}"),
    }
}

async fn mint_submission(storage: &dyn Storage) -> SubmissionId {
    match storage.mint_id().await {
        Ok(raw) => SubmissionId::new_unchecked(raw),
        Err(err) => panic!("mint_id failed: {err}"),
    }
}

async fn mint_document(storage: &dyn Storage) -> DocumentId {
    match storage.mint_id().await {
        Ok(raw) => DocumentId::new_unchecked(raw),
        Err(err) => panic!("mint_id failed: {err}"),
    }
}

fn ok_val<T>(res: pi_js::Result<T>) -> T {
    match res {
        Ok(v) => v,
        Err(err) => panic!("unexpected storage error: {err}"),
    }
}

fn create_case<F, Fut>(
    with_storage: &StorageConformanceProvider,
    assertions: &Arc<dyn StorageConformanceAssertions>,
    name: &'static str,
    f: F,
) -> StorageConformanceCase
where
    F: Fn(Arc<dyn Storage>, Arc<dyn StorageConformanceAssertions>) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let test = Arc::new(move |s: Arc<dyn Storage>, a: Arc<dyn StorageConformanceAssertions>| {
        Box::pin(f(s, a)) as crate::testing::types::BoxFuture<'static, ()>
    });
    StorageConformanceCase::new(name, Arc::clone(with_storage), Arc::clone(assertions), test)
}

/// Build every runner-independent storage conformance case.
pub fn create_storage_conformance(options: StorageConformanceOptions) -> Vec<StorageConformanceCase> {
    let w = &options.with_storage;
    let a = &options.assertions;

    vec![
        create_case(
            w,
            a,
            "reserves ID 1 for the immutable root conversation",
            |storage, expect| async move {
                let ctx = BACKGROUND_CONTEXT.clone();
                let minted = ok_val(storage.mint_id().await);
                expect.strict_equal(&json!(minted), &json!(2));
                let root_id = create_root(storage.as_ref()).await;
                expect.strict_equal(&to_val(&root_id), &to_val(&ROOT_CONVERSATION_ID));
                let root_conv = ok_val(storage.conversation(ROOT_CONVERSATION_ID, ctx.clone()).await);
                expect.deep_equal(
                    &to_val(&root_conv),
                    &to_val(&Some(ConversationRecord {
                        id: ROOT_CONVERSATION_ID,
                        parent: None,
                        owner: None,
                    })),
                );
                let dup = storage
                    .commit(
                        vec![StorageWrite::Conversation {
                            value: ConversationRecord {
                                id: ROOT_CONVERSATION_ID,
                                parent: None,
                                owner: None,
                            },
                        }],
                        ctx,
                    )
                    .await
                    .map(|_| ());
                expect.rejects_containing(
                    dup,
                    &format!("ID {} already belongs to conversation", ROOT_CONVERSATION_ID),
                );
            },
        ),
        create_case(
            w,
            a,
            "commits mixed table writes atomically and rolls all of them back on failure",
            |storage, expect| async move {
                let ctx = BACKGROUND_CONTEXT.clone();
                let root_id = create_root(storage.as_ref()).await;
                let entry_id = mint_entry(storage.as_ref()).await;
                let task_id = mint_task(storage.as_ref()).await;
                let submission_id = mint_submission(storage.as_ref()).await;
                let task = pending_task(task_id, root_id, "ready");
                let input = SubmissionRecord {
                    id: submission_id,
                    conversation_id: root_id,
                    request_id: Some("request-1".to_string()),
                    state: SubmissionState::Input(InputSubmissionState::Placed { entry: entry_id }),
                };
                let mut user_entry = make_entry(entry_id, root_id, "user");
                user_entry.data = Some(json!({ "text": "hello" }));

                let initial_seq = ok_val(
                    storage
                        .commit(
                            vec![
                                StorageWrite::Entry {
                                    value: user_entry.clone(),
                                },
                                StorageWrite::Task { value: task.clone() },
                                StorageWrite::Submission { value: input.clone() },
                            ],
                            ctx.clone(),
                        )
                        .await,
                );

                let looked_up = ok_val(storage.entry(entry_id, ctx.clone()).await);
                expect.deep_equal(
                    &json!({
                        "entry": to_val(&looked_up.as_ref().map(|l| &l.entry)),
                        "commitSeq": to_val(&looked_up.as_ref().map(|l| l.commit_seq)),
                    }),
                    &json!({
                        "entry": to_val(&Some(&user_entry)),
                        "commitSeq": to_val(&Some(initial_seq)),
                    }),
                );
                expect.deep_equal(
                    &to_val(&ok_val(storage.task(task_id, ctx.clone()).await)),
                    &to_val(&Some(&task)),
                );
                expect.deep_equal(
                    &to_val(&ok_val(storage.submission(submission_id, ctx.clone()).await)),
                    &to_val(&Some(&input)),
                );

                let transient_entry_id = mint_entry(storage.as_ref()).await;
                let mut running_task = task.clone();
                running_task.state = TaskState::Running {
                    checkpoint: json!({ "phase": "effect" }),
                };
                let done_input = SubmissionRecord {
                    id: submission_id,
                    conversation_id: root_id,
                    request_id: Some("request-1".to_string()),
                    state: SubmissionState::Input(InputSubmissionState::Done {
                        entry: entry_id,
                        answer: transient_entry_id,
                    }),
                };
                let failed = storage
                    .commit(
                        vec![
                            StorageWrite::Task { value: running_task },
                            StorageWrite::Submission { value: done_input },
                            StorageWrite::Entry {
                                value: make_entry(transient_entry_id, root_id, "assistant"),
                            },
                            StorageWrite::Conversation {
                                value: ConversationRecord {
                                    id: root_id,
                                    parent: None,
                                    owner: None,
                                },
                            },
                        ],
                        ctx.clone(),
                    )
                    .await
                    .map(|_| ());
                expect.rejects_containing(failed, &format!("ID {root_id} already belongs to conversation"));

                expect.deep_equal(
                    &to_val(&ok_val(storage.task(task_id, ctx.clone()).await)),
                    &to_val(&Some(&task)),
                );
                expect.deep_equal(
                    &to_val(&ok_val(storage.submission(submission_id, ctx.clone()).await)),
                    &to_val(&Some(&input)),
                );
                expect.ok(
                    ok_val(storage.entry(transient_entry_id, ctx.clone()).await).is_none(),
                    "transient entry must not exist after rollback",
                );
                let after_id = mint_entry(storage.as_ref()).await;
                let after_rollback_seq = ok_val(
                    storage
                        .commit(
                            vec![StorageWrite::Entry {
                                value: make_entry(after_id, root_id, "after-rollback"),
                            }],
                            ctx,
                        )
                        .await,
                );
                expect.greater_than(after_rollback_seq.as_i64(), initial_seq.as_i64());
            },
        ),
        create_case(
            w,
            a,
            "detaches retained writes and every returned record",
            |storage, expect| async move {
                let ctx = BACKGROUND_CONTEXT.clone();
                let root_id = create_root(storage.as_ref()).await;
                let entry_id = mint_entry(storage.as_ref()).await;
                let task_id = mint_task(storage.as_ref()).await;
                let submission_id = mint_submission(storage.as_ref()).await;

                let mut stored_entry = make_entry(entry_id, root_id, "note");
                stored_entry.data = Some(json!({ "nested": [1, 2] }));
                let mut stored_task = pending_task(task_id, root_id, "ready");
                stored_task.state = TaskState::Pending {
                    checkpoint: json!({ "phase": "ready", "nested": { "count": 1 } }),
                };
                let stored_input = SubmissionRecord {
                    id: submission_id,
                    conversation_id: root_id,
                    request_id: None,
                    state: SubmissionState::Input(InputSubmissionState::Unanswered {
                        entry: None,
                        reason: "failed".to_string(),
                        detail: Some(json!({ "codes": ["initial"] })),
                    }),
                };

                ok_val(
                    storage
                        .commit(
                            vec![
                                StorageWrite::Entry {
                                    value: stored_entry.clone(),
                                },
                                StorageWrite::Task {
                                    value: stored_task.clone(),
                                },
                                StorageWrite::Submission {
                                    value: stored_input.clone(),
                                },
                            ],
                            ctx.clone(),
                        )
                        .await,
                );

                if let Some(JsonValue::Object(ref mut m)) = stored_entry.data
                    && let Some(JsonValue::Array(arr)) = m.get_mut("nested")
                {
                    arr.push(json!(3));
                }
                let mut read_entry = ok_val(storage.entry(entry_id, ctx.clone()).await);
                expect.deep_equal(
                    &to_val(&read_entry.as_ref().and_then(|l| l.entry.data.as_ref())),
                    &json!({ "nested": [1, 2] }),
                );
                if let Some(ref mut looked) = read_entry
                    && let Some(JsonValue::Object(ref mut m)) = looked.entry.data
                    && let Some(JsonValue::Array(arr)) = m.get_mut("nested")
                {
                    arr.push(json!(9));
                }

                expect.deep_equal(
                    &to_val(&ok_val(storage.entry(entry_id, ctx.clone()).await).and_then(|l| l.entry.data)),
                    &json!({ "nested": [1, 2] }),
                );
                expect.deep_equal(
                    &to_val(&ok_val(storage.task(task_id, ctx.clone()).await).map(|t| t.state)),
                    &json!({ "status": "pending", "checkpoint": { "phase": "ready", "nested": { "count": 1 } } }),
                );
                expect.deep_equal(
                    &to_val(&ok_val(storage.submission(submission_id, ctx).await).map(|s| s.state)),
                    &json!({ "status": "unanswered", "reason": "failed", "detail": { "codes": ["initial"] } }),
                );
            },
        ),
        create_case(
            w,
            a,
            "detaches prototype-like JSON keys without changing object prototypes",
            |storage, expect| async move {
                let ctx = BACKGROUND_CONTEXT.clone();
                let root_id = create_root(storage.as_ref()).await;
                let entry_id = mint_entry(storage.as_ref()).await;
                let data: JsonValue = match serde_json::from_str(
                    r#"{"__proto__":{"polluted":false},"constructor":{"label":"stored"},"toString":"value"}"#,
                ) {
                    Ok(v) => v,
                    Err(e) => panic!("bad json literal: {e}"),
                };
                let mut rec = make_entry(entry_id, root_id, "note");
                rec.data = Some(data.clone());
                ok_val(
                    storage
                        .commit(vec![StorageWrite::Entry { value: rec }], ctx.clone())
                        .await,
                );
                let first_read = ok_val(storage.entry(entry_id, ctx).await);
                expect.deep_equal(&to_val(&first_read.and_then(|l| l.entry.data)), &data);
            },
        ),
        create_case(
            w,
            a,
            "indexes entries committed out of ID order",
            |storage, expect| async move {
                let ctx = BACKGROUND_CONTEXT.clone();
                let root_id = create_root(storage.as_ref()).await;
                let e30 = make_entry(EntryId::new_unchecked(30), root_id, "message");
                let e10 = make_entry(EntryId::new_unchecked(10), root_id, "message");
                let mut e20 = make_entry(EntryId::new_unchecked(20), root_id, "marker");
                e20.head = Some(EntryId::new_unchecked(10));
                ok_val(
                    storage
                        .commit(
                            vec![
                                StorageWrite::Entry { value: e30 },
                                StorageWrite::Entry { value: e10 },
                                StorageWrite::Entry { value: e20 },
                            ],
                            ctx.clone(),
                        )
                        .await,
                );

                let page = ok_val(
                    storage
                        .scan_entries(
                            EntryQuery {
                                conversation_id: root_id,
                                min_entry_id: None,
                                max_entry_id: None,
                            },
                            10,
                            None,
                            ctx.clone(),
                        )
                        .await,
                );
                let ids: Vec<i64> = page.items.iter().map(|e| e.id.as_i64()).collect();
                expect.deep_equal(&json!(ids), &json!([30, 20, 10]));
                let head = ok_val(storage.find_latest_head_marker(root_id, None, ctx).await);
                expect.strict_equal(&to_val(&head.map(|h| h.id)), &json!(20));
            },
        ),
        create_case(
            w,
            a,
            "continues an entry cursor below its last item after a newer commit",
            |storage, expect| async move {
                let ctx = BACKGROUND_CONTEXT.clone();
                let root_id = create_root(storage.as_ref()).await;
                let oldest_id = mint_entry(storage.as_ref()).await;
                let middle_id = mint_entry(storage.as_ref()).await;
                let newest_id = mint_entry(storage.as_ref()).await;
                ok_val(
                    storage
                        .commit(
                            vec![
                                StorageWrite::Entry {
                                    value: make_entry(oldest_id, root_id, "message"),
                                },
                                StorageWrite::Entry {
                                    value: make_entry(middle_id, root_id, "message"),
                                },
                                StorageWrite::Entry {
                                    value: make_entry(newest_id, root_id, "message"),
                                },
                            ],
                            ctx.clone(),
                        )
                        .await,
                );

                let first = ok_val(
                    storage
                        .scan_entries(
                            EntryQuery {
                                conversation_id: root_id,
                                min_entry_id: None,
                                max_entry_id: None,
                            },
                            2,
                            None,
                            ctx.clone(),
                        )
                        .await,
                );
                let first_ids: Vec<EntryId> = first.items.iter().map(|e| e.id).collect();
                expect.deep_equal(&to_val(&first_ids), &to_val(&vec![newest_id, middle_id]));

                let appended_id = mint_entry(storage.as_ref()).await;
                ok_val(
                    storage
                        .commit(
                            vec![StorageWrite::Entry {
                                value: make_entry(appended_id, root_id, "message"),
                            }],
                            ctx.clone(),
                        )
                        .await,
                );
                let second = ok_val(
                    storage
                        .scan_entries(
                            EntryQuery {
                                conversation_id: root_id,
                                min_entry_id: None,
                                max_entry_id: None,
                            },
                            2,
                            first.next,
                            ctx,
                        )
                        .await,
                );
                let second_ids: Vec<EntryId> = second.items.iter().map(|e| e.id).collect();
                expect.deep_equal(&to_val(&second_ids), &to_val(&vec![oldest_id]));
                expect.ok(second.next.is_none(), "second page should have no next");
            },
        ),
        create_case(
            w,
            a,
            "paginates conversations by opaque cursor in ascending ID order",
            |storage, expect| async move {
                let ctx = BACKGROUND_CONTEXT.clone();
                let root_id = create_root(storage.as_ref()).await;
                let second_id = mint_conv(storage.as_ref()).await;
                let third_id = mint_conv(storage.as_ref()).await;
                ok_val(
                    storage
                        .commit(
                            vec![
                                StorageWrite::Conversation {
                                    value: ConversationRecord {
                                        id: third_id,
                                        parent: None,
                                        owner: None,
                                    },
                                },
                                StorageWrite::Conversation {
                                    value: ConversationRecord {
                                        id: second_id,
                                        parent: None,
                                        owner: None,
                                    },
                                },
                            ],
                            ctx.clone(),
                        )
                        .await,
                );

                let first = ok_val(
                    storage
                        .scan_conversations(ConversationQuery::default(), 2, None, ctx.clone())
                        .await,
                );
                let first_ids: Vec<ConversationId> = first.items.iter().map(|c| c.id).collect();
                expect.deep_equal(&to_val(&first_ids), &to_val(&vec![root_id, second_id]));
                expect.ok(first.next.is_some(), "first page should have next cursor");

                let round_tripped: Option<crate::types::Cursor> = match first.next {
                    Some(c) => {
                        let s = match pi_js::json::stringify(&c) {
                            Ok(s) => s,
                            Err(e) => panic!("stringify failed: {e}"),
                        };
                        match serde_json::from_str(&s) {
                            Ok(v) => Some(v),
                            Err(e) => panic!("parse failed: {e}"),
                        }
                    }
                    None => None,
                };
                let second = ok_val(
                    storage
                        .scan_conversations(ConversationQuery::default(), 2, round_tripped, ctx)
                        .await,
                );
                let second_ids: Vec<ConversationId> = second.items.iter().map(|c| c.id).collect();
                expect.deep_equal(&to_val(&second_ids), &to_val(&vec![third_id]));
                expect.ok(second.next.is_none(), "second page should have no next");
            },
        ),
        create_case(
            w,
            a,
            "filters and pages conversations by durable owner edges",
            |storage, expect| async move {
                let ctx = BACKGROUND_CONTEXT.clone();
                let root_id = create_root(storage.as_ref()).await;
                let other_owner_id = mint_conv(storage.as_ref()).await;
                let first_task_id = mint_task(storage.as_ref()).await;
                let second_task_id = mint_task(storage.as_ref()).await;
                let first_id = mint_conv(storage.as_ref()).await;
                let second_id = mint_conv(storage.as_ref()).await;
                let third_id = mint_conv(storage.as_ref()).await;
                ok_val(
                    storage
                        .commit(
                            vec![
                                StorageWrite::Conversation {
                                    value: ConversationRecord {
                                        id: other_owner_id,
                                        parent: None,
                                        owner: None,
                                    },
                                },
                                StorageWrite::Conversation {
                                    value: ConversationRecord {
                                        id: first_id,
                                        parent: None,
                                        owner: Some(ConversationOwner {
                                            conversation_id: root_id,
                                            task_id: first_task_id,
                                        }),
                                    },
                                },
                                StorageWrite::Conversation {
                                    value: ConversationRecord {
                                        id: second_id,
                                        parent: None,
                                        owner: Some(ConversationOwner {
                                            conversation_id: root_id,
                                            task_id: second_task_id,
                                        }),
                                    },
                                },
                                StorageWrite::Conversation {
                                    value: ConversationRecord {
                                        id: third_id,
                                        parent: None,
                                        owner: Some(ConversationOwner {
                                            conversation_id: other_owner_id,
                                            task_id: first_task_id,
                                        }),
                                    },
                                },
                            ],
                            ctx.clone(),
                        )
                        .await,
                );

                let first = ok_val(
                    storage
                        .scan_conversations(
                            ConversationQuery {
                                owner_conversation_id: Some(root_id),
                                owner_task_id: None,
                            },
                            1,
                            None,
                            ctx.clone(),
                        )
                        .await,
                );
                expect.deep_equal(
                    &to_val(&first.items.iter().map(|c| c.id).collect::<Vec<_>>()),
                    &to_val(&vec![first_id]),
                );
                expect.ok(first.next.is_some(), "expected next cursor");

                let second = ok_val(
                    storage
                        .scan_conversations(
                            ConversationQuery {
                                owner_conversation_id: Some(root_id),
                                owner_task_id: None,
                            },
                            1,
                            first.next,
                            ctx.clone(),
                        )
                        .await,
                );
                expect.deep_equal(
                    &to_val(&second.items.iter().map(|c| c.id).collect::<Vec<_>>()),
                    &to_val(&vec![second_id]),
                );
                expect.ok(second.next.is_none(), "expected no next cursor");

                let by_task = ok_val(
                    storage
                        .scan_conversations(
                            ConversationQuery {
                                owner_conversation_id: None,
                                owner_task_id: Some(first_task_id),
                            },
                            10,
                            None,
                            ctx.clone(),
                        )
                        .await,
                );
                expect.deep_equal(
                    &to_val(&by_task.items.iter().map(|c| c.id).collect::<Vec<_>>()),
                    &to_val(&vec![first_id, third_id]),
                );

                let by_both = ok_val(
                    storage
                        .scan_conversations(
                            ConversationQuery {
                                owner_conversation_id: Some(root_id),
                                owner_task_id: Some(first_task_id),
                            },
                            10,
                            None,
                            ctx,
                        )
                        .await,
                );
                expect.deep_equal(
                    &to_val(&by_both.items.iter().map(|c| c.id).collect::<Vec<_>>()),
                    &to_val(&vec![first_id]),
                );
            },
        ),
        create_case(
            w,
            a,
            "scans deep fork history newest-first through every ancestor cap",
            |storage, expect| async move {
                let ctx = BACKGROUND_CONTEXT.clone();
                let root_id = create_root(storage.as_ref()).await;
                let root_first = mint_entry(storage.as_ref()).await;
                let root_fork_point = mint_entry(storage.as_ref()).await;
                let root_excluded_same_commit = mint_entry(storage.as_ref()).await;
                let mut fork_marker = make_entry(root_fork_point, root_id, "marker");
                fork_marker.head = Some(root_first);
                let root_entries_seq = ok_val(
                    storage
                        .commit(
                            vec![
                                StorageWrite::Entry {
                                    value: make_entry(root_first, root_id, "message"),
                                },
                                StorageWrite::Entry { value: fork_marker },
                                StorageWrite::Entry {
                                    value: make_entry(root_excluded_same_commit, root_id, "message"),
                                },
                            ],
                            ctx.clone(),
                        )
                        .await,
                );

                let child_id = mint_conv(storage.as_ref()).await;
                ok_val(
                    storage
                        .commit(
                            vec![StorageWrite::Conversation {
                                value: ConversationRecord {
                                    id: child_id,
                                    parent: Some(ConversationParent {
                                        conversation_id: root_id,
                                        at: root_fork_point,
                                    }),
                                    owner: None,
                                },
                            }],
                            ctx.clone(),
                        )
                        .await,
                );

                let child_fork_point = mint_entry(storage.as_ref()).await;
                let child_excluded = mint_entry(storage.as_ref()).await;
                ok_val(
                    storage
                        .commit(
                            vec![
                                StorageWrite::Entry {
                                    value: make_entry(child_fork_point, child_id, "note"),
                                },
                                StorageWrite::Entry {
                                    value: make_entry(child_excluded, child_id, "message"),
                                },
                            ],
                            ctx.clone(),
                        )
                        .await,
                );

                let root_excluded_later = mint_entry(storage.as_ref()).await;
                ok_val(
                    storage
                        .commit(
                            vec![StorageWrite::Entry {
                                value: make_entry(root_excluded_later, root_id, "message"),
                            }],
                            ctx.clone(),
                        )
                        .await,
                );

                let grandchild_id = mint_conv(storage.as_ref()).await;
                ok_val(
                    storage
                        .commit(
                            vec![StorageWrite::Conversation {
                                value: ConversationRecord {
                                    id: grandchild_id,
                                    parent: Some(ConversationParent {
                                        conversation_id: child_id,
                                        at: child_fork_point,
                                    }),
                                    owner: None,
                                },
                            }],
                            ctx.clone(),
                        )
                        .await,
                );

                let grandchild_head = mint_entry(storage.as_ref()).await;
                let grandchild_tail = mint_entry(storage.as_ref()).await;
                let mut gc_head_rec = make_entry(grandchild_head, grandchild_id, "marker");
                gc_head_rec.head = Some(grandchild_head);
                let grandchild_entries_seq = ok_val(
                    storage
                        .commit(
                            vec![
                                StorageWrite::Entry { value: gc_head_rec },
                                StorageWrite::Entry {
                                    value: make_entry(grandchild_tail, grandchild_id, "message"),
                                },
                            ],
                            ctx.clone(),
                        )
                        .await,
                );

                let child_excluded_later = mint_entry(storage.as_ref()).await;
                ok_val(
                    storage
                        .commit(
                            vec![StorageWrite::Entry {
                                value: make_entry(child_excluded_later, child_id, "message"),
                            }],
                            ctx.clone(),
                        )
                        .await,
                );

                let first = ok_val(
                    storage
                        .scan_entries(
                            EntryQuery {
                                conversation_id: grandchild_id,
                                min_entry_id: None,
                                max_entry_id: None,
                            },
                            2,
                            None,
                            ctx.clone(),
                        )
                        .await,
                );
                expect.deep_equal(
                    &to_val(&first.items.iter().map(|e| e.id).collect::<Vec<_>>()),
                    &to_val(&vec![grandchild_tail, grandchild_head]),
                );

                let second = ok_val(
                    storage
                        .scan_entries(
                            EntryQuery {
                                conversation_id: grandchild_id,
                                min_entry_id: None,
                                max_entry_id: None,
                            },
                            2,
                            first.next,
                            ctx.clone(),
                        )
                        .await,
                );
                expect.deep_equal(
                    &to_val(&second.items.iter().map(|e| e.id).collect::<Vec<_>>()),
                    &to_val(&vec![child_fork_point, root_fork_point]),
                );

                let third = ok_val(
                    storage
                        .scan_entries(
                            EntryQuery {
                                conversation_id: grandchild_id,
                                min_entry_id: None,
                                max_entry_id: None,
                            },
                            2,
                            second.next,
                            ctx.clone(),
                        )
                        .await,
                );
                expect.deep_equal(
                    &to_val(&third.items.iter().map(|e| e.id).collect::<Vec<_>>()),
                    &to_val(&vec![root_first]),
                );
                expect.ok(third.next.is_none(), "expected third page next to be None");

                let current_marker = ok_val(storage.find_latest_head_marker(grandchild_id, None, ctx.clone()).await);
                expect.strict_equal(
                    &to_val(&current_marker.as_ref().map(|m| m.id)),
                    &to_val(&Some(grandchild_head)),
                );
                expect.strict_equal(
                    &to_val(&current_marker.as_ref().and_then(|m| m.head)),
                    &to_val(&Some(grandchild_head)),
                );

                let historical_marker = ok_val(
                    storage
                        .find_latest_head_marker(grandchild_id, Some(child_fork_point), ctx.clone())
                        .await,
                );
                expect.strict_equal(
                    &to_val(&historical_marker.as_ref().map(|m| m.id)),
                    &to_val(&Some(root_fork_point)),
                );
                expect.strict_equal(
                    &to_val(&historical_marker.as_ref().and_then(|m| m.head)),
                    &to_val(&Some(root_first)),
                );
                expect.ok(
                    ok_val(
                        storage
                            .find_latest_head_marker(grandchild_id, Some(root_first), ctx.clone())
                            .await,
                    )
                    .is_none(),
                    "no marker at or before root_first",
                );

                let active_first = ok_val(
                    storage
                        .scan_entries(
                            EntryQuery {
                                conversation_id: grandchild_id,
                                min_entry_id: current_marker.as_ref().and_then(|m| m.head),
                                max_entry_id: None,
                            },
                            1,
                            None,
                            ctx.clone(),
                        )
                        .await,
                );
                expect.deep_equal(
                    &to_val(&active_first.items.iter().map(|e| e.id).collect::<Vec<_>>()),
                    &to_val(&vec![grandchild_tail]),
                );
                expect.ok(active_first.next.is_some(), "expected active_first next");
                let active_second = ok_val(
                    storage
                        .scan_entries(
                            EntryQuery {
                                conversation_id: grandchild_id,
                                min_entry_id: current_marker.as_ref().and_then(|m| m.head),
                                max_entry_id: None,
                            },
                            1,
                            active_first.next,
                            ctx.clone(),
                        )
                        .await,
                );
                expect.deep_equal(
                    &to_val(&active_second.items.iter().map(|e| e.id).collect::<Vec<_>>()),
                    &to_val(&vec![grandchild_head]),
                );
                expect.ok(active_second.next.is_none(), "expected active_second next None");

                let bounded = ok_val(
                    storage
                        .scan_entries(
                            EntryQuery {
                                conversation_id: grandchild_id,
                                min_entry_id: historical_marker.as_ref().and_then(|m| m.head),
                                max_entry_id: Some(child_fork_point),
                            },
                            10,
                            None,
                            ctx.clone(),
                        )
                        .await,
                );
                expect.deep_equal(
                    &to_val(&bounded.items.iter().map(|e| e.id).collect::<Vec<_>>()),
                    &to_val(&vec![child_fork_point, root_fork_point, root_first]),
                );

                let r_first = ok_val(storage.entry(root_first, ctx.clone()).await);
                expect.deep_equal(
                    &json!({
                        "entry": to_val(&r_first.as_ref().map(|l| &l.entry)),
                        "commitSeq": to_val(&r_first.as_ref().map(|l| l.commit_seq)),
                    }),
                    &json!({
                        "entry": to_val(&Some(make_entry(root_first, root_id, "message"))),
                        "commitSeq": to_val(&Some(root_entries_seq)),
                    }),
                );
                expect.strict_equal(
                    &to_val(&ok_val(storage.entry(root_fork_point, ctx.clone()).await).map(|l| l.commit_seq)),
                    &to_val(&Some(root_entries_seq)),
                );
                expect.strict_equal(
                    &to_val(&ok_val(storage.entry(grandchild_head, ctx.clone()).await).map(|l| l.commit_seq)),
                    &to_val(&Some(grandchild_entries_seq)),
                );
                expect.strict_equal(
                    &to_val(&ok_val(storage.entry(grandchild_tail, ctx.clone()).await).map(|l| l.commit_seq)),
                    &to_val(&Some(grandchild_entries_seq)),
                );
                expect.ok(
                    ok_val(storage.entry(EntryId::new_unchecked(999_999), ctx.clone()).await).is_none(),
                    "unknown entry id should return None",
                );

                let in_gc = ok_val(
                    storage
                        .entry_in_conversation(grandchild_id, root_first, ctx.clone())
                        .await,
                );
                expect.deep_equal(
                    &json!({
                        "entry": to_val(&in_gc.as_ref().map(|l| &l.entry)),
                        "commitSeq": to_val(&in_gc.as_ref().map(|l| l.commit_seq)),
                    }),
                    &json!({
                        "entry": to_val(&Some(make_entry(root_first, root_id, "message"))),
                        "commitSeq": to_val(&Some(root_entries_seq)),
                    }),
                );
                expect.strict_equal(
                    &to_val(
                        &ok_val(
                            storage
                                .entry_in_conversation(grandchild_id, child_fork_point, ctx.clone())
                                .await,
                        )
                        .map(|l| l.entry.conversation_id),
                    ),
                    &to_val(&Some(child_id)),
                );
                expect.strict_equal(
                    &to_val(
                        &ok_val(
                            storage
                                .entry_in_conversation(grandchild_id, grandchild_tail, ctx.clone())
                                .await,
                        )
                        .map(|l| l.commit_seq),
                    ),
                    &to_val(&Some(grandchild_entries_seq)),
                );

                for hidden in [
                    root_excluded_same_commit,
                    root_excluded_later,
                    child_excluded,
                    child_excluded_later,
                ] {
                    expect.ok(
                        ok_val(storage.entry_in_conversation(grandchild_id, hidden, ctx.clone()).await).is_none(),
                        "fork-excluded entry must not be visible",
                    );
                }
                expect.ok(
                    ok_val(
                        storage
                            .entry_in_conversation(root_id, grandchild_head, ctx.clone())
                            .await,
                    )
                    .is_none(),
                    "grandchild entry must not be visible in root",
                );
                expect.ok(
                    ok_val(
                        storage
                            .entry_in_conversation(grandchild_id, EntryId::new_unchecked(999_999), ctx.clone())
                            .await,
                    )
                    .is_none(),
                    "nonexistent entry returns None",
                );

                expect.rejects_containing(
                    storage
                        .entry_in_conversation(ConversationId::new_unchecked(999_999), root_first, ctx.clone())
                        .await
                        .map(|_| ()),
                    "Unknown conversation",
                );
                expect.rejects_containing(
                    storage
                        .scan_entries(
                            EntryQuery {
                                conversation_id: ConversationId::new_unchecked(999_999),
                                min_entry_id: None,
                                max_entry_id: None,
                            },
                            10,
                            None,
                            ctx,
                        )
                        .await
                        .map(|_| ()),
                    "Unknown conversation",
                );
            },
        ),
        create_case(
            w,
            a,
            "replaces complete task records and pages filtered task scans",
            |storage, expect| async move {
                let ctx = BACKGROUND_CONTEXT.clone();
                let root_id = create_root(storage.as_ref()).await;
                let first_id = mint_task(storage.as_ref()).await;
                let second_id = mint_task(storage.as_ref()).await;
                let third_id = mint_task(storage.as_ref()).await;

                let mut first = pending_task(first_id, root_id, "ready");
                let mut memos = IndexMap::new();
                memos.insert("winner".to_string(), json!("first"));
                first.memos = Some(memos);

                let mut second = pending_task(second_id, root_id, "ready");
                second.background = true;

                let mut third = pending_task(third_id, root_id, "ready");
                third.abort_requested = true;

                ok_val(
                    storage
                        .commit(
                            vec![
                                StorageWrite::Task { value: first.clone() },
                                StorageWrite::Task { value: second.clone() },
                                StorageWrite::Task { value: third.clone() },
                            ],
                            ctx.clone(),
                        )
                        .await,
                );

                let mut running = first.clone();
                running.state = TaskState::Running {
                    checkpoint: json!({ "phase": "effect", "attempt": 1 }),
                };
                running.abort_requested = true;
                ok_val(
                    storage
                        .commit(vec![StorageWrite::Task { value: running.clone() }], ctx.clone())
                        .await,
                );
                expect.deep_equal(
                    &to_val(&ok_val(storage.task(first_id, ctx.clone()).await)),
                    &to_val(&Some(&running)),
                );

                let terminal = ErasedTaskRecord {
                    id: first_id,
                    conversation_id: root_id,
                    kind: first.kind,
                    version: first.version,
                    input: first.input,
                    owner: None,
                    background: false,
                    abort_requested: true,
                    state: TaskState::Terminal {
                        outcome: TaskOutcome::Completed {
                            result: json!({ "entryId": 99 }),
                        },
                    },
                    memos: None,
                };
                ok_val(
                    storage
                        .commit(
                            vec![StorageWrite::Task {
                                value: terminal.clone(),
                            }],
                            ctx.clone(),
                        )
                        .await,
                );
                expect.deep_equal(
                    &to_val(&ok_val(storage.task(first_id, ctx.clone()).await)),
                    &to_val(&Some(&terminal)),
                );

                let pending_page = ok_val(
                    storage
                        .scan_tasks(
                            TaskQuery {
                                status: Some(TaskStatus::Pending),
                                ..Default::default()
                            },
                            1,
                            None,
                            ctx.clone(),
                        )
                        .await,
                );
                expect.deep_equal(
                    &to_val(&pending_page.items.iter().map(|t| t.id).collect::<Vec<_>>()),
                    &to_val(&vec![second_id]),
                );
                expect.ok(pending_page.next.is_some(), "expected pending_page next");
                let pending_page2 = ok_val(
                    storage
                        .scan_tasks(
                            TaskQuery {
                                status: Some(TaskStatus::Pending),
                                ..Default::default()
                            },
                            1,
                            pending_page.next,
                            ctx.clone(),
                        )
                        .await,
                );
                expect.deep_equal(
                    &to_val(&pending_page2.items.iter().map(|t| t.id).collect::<Vec<_>>()),
                    &to_val(&vec![third_id]),
                );
                let term_scan = ok_val(
                    storage
                        .scan_tasks(
                            TaskQuery {
                                status: Some(TaskStatus::Terminal),
                                abort_requested: Some(true),
                                ..Default::default()
                            },
                            10,
                            None,
                            ctx.clone(),
                        )
                        .await,
                );
                expect.deep_equal(&to_val(&term_scan.items), &to_val(&vec![terminal]));
                let bg_scan = ok_val(
                    storage
                        .scan_tasks(
                            TaskQuery {
                                background: Some(true),
                                ..Default::default()
                            },
                            10,
                            None,
                            ctx,
                        )
                        .await,
                );
                expect.deep_equal(
                    &to_val(&bg_scan.items.iter().map(|t| t.id).collect::<Vec<_>>()),
                    &to_val(&vec![second_id]),
                );
            },
        ),
        create_case(
            w,
            a,
            "stores owners and scans waiting and completing tasks by status",
            |storage, expect| async move {
                let ctx = BACKGROUND_CONTEXT.clone();
                let root_id = create_root(storage.as_ref()).await;
                let owner_id = mint_task(storage.as_ref()).await;
                let waiting_id = mint_task(storage.as_ref()).await;
                let completing_id = mint_task(storage.as_ref()).await;

                let owner = pending_task(owner_id, root_id, "ready");
                let mut waiting = pending_task(waiting_id, root_id, "ready");
                waiting.owner = Some(owner_id);
                waiting.state = TaskState::Waiting {
                    checkpoint: json!({ "phase": "next" }),
                    on: vec![owner_id],
                    policy: JoinPolicy::AllSettled,
                };
                let mut memos = IndexMap::new();
                memos.insert("kept".to_string(), json!(true));
                waiting.memos = Some(memos);

                let mut completing = pending_task(completing_id, root_id, "ready");
                completing.owner = Some(owner_id);
                completing.state = TaskState::Completing {
                    outcome: TaskOutcome::Failed {
                        error: TaskOutcomeError {
                            message: "held".to_string(),
                            detail: None,
                        },
                        result: None,
                    },
                };

                ok_val(
                    storage
                        .commit(
                            vec![
                                StorageWrite::Task { value: owner },
                                StorageWrite::Task { value: waiting.clone() },
                                StorageWrite::Task {
                                    value: completing.clone(),
                                },
                            ],
                            ctx.clone(),
                        )
                        .await,
                );
                expect.deep_equal(
                    &to_val(&ok_val(storage.task(waiting_id, ctx.clone()).await)),
                    &to_val(&Some(&waiting)),
                );
                expect.deep_equal(
                    &to_val(&ok_val(storage.task(completing_id, ctx.clone()).await)),
                    &to_val(&Some(&completing)),
                );

                expect.deep_equal(
                    &to_val(
                        &ok_val(
                            storage
                                .scan_tasks(
                                    TaskQuery {
                                        status: Some(TaskStatus::Waiting),
                                        ..Default::default()
                                    },
                                    10,
                                    None,
                                    ctx.clone(),
                                )
                                .await,
                        )
                        .items,
                    ),
                    &to_val(&vec![waiting]),
                );
                expect.deep_equal(
                    &to_val(
                        &ok_val(
                            storage
                                .scan_tasks(
                                    TaskQuery {
                                        status: Some(TaskStatus::Completing),
                                        ..Default::default()
                                    },
                                    10,
                                    None,
                                    ctx.clone(),
                                )
                                .await,
                        )
                        .items,
                    ),
                    &to_val(&vec![&completing]),
                );
                expect.deep_equal(
                    &to_val(
                        &ok_val(
                            storage
                                .scan_tasks(
                                    TaskQuery {
                                        status: Some(TaskStatus::Pending),
                                        ..Default::default()
                                    },
                                    10,
                                    None,
                                    ctx.clone(),
                                )
                                .await,
                        )
                        .items
                        .iter()
                        .map(|t| t.id)
                        .collect::<Vec<_>>(),
                    ),
                    &to_val(&vec![owner_id]),
                );

                let mut terminal = completing.clone();
                terminal.state = TaskState::Terminal {
                    outcome: TaskOutcome::Failed {
                        error: TaskOutcomeError {
                            message: "held".to_string(),
                            detail: None,
                        },
                        result: None,
                    },
                };
                ok_val(
                    storage
                        .commit(
                            vec![StorageWrite::Task {
                                value: terminal.clone(),
                            }],
                            ctx.clone(),
                        )
                        .await,
                );
                expect.deep_equal(
                    &to_val(
                        &ok_val(
                            storage
                                .scan_tasks(
                                    TaskQuery {
                                        status: Some(TaskStatus::Completing),
                                        ..Default::default()
                                    },
                                    10,
                                    None,
                                    ctx.clone(),
                                )
                                .await,
                        )
                        .items,
                    ),
                    &json!([]),
                );
                expect.deep_equal(
                    &to_val(
                        &ok_val(
                            storage
                                .scan_tasks(
                                    TaskQuery {
                                        status: Some(TaskStatus::Terminal),
                                        ..Default::default()
                                    },
                                    10,
                                    None,
                                    ctx,
                                )
                                .await,
                        )
                        .items,
                    ),
                    &to_val(&vec![terminal]),
                );
            },
        ),
        create_case(
            w,
            a,
            "indexes request IDs per conversation and replaces complete submission records",
            |storage, expect| async move {
                let ctx = BACKGROUND_CONTEXT.clone();
                let root_id = create_root(storage.as_ref()).await;
                let second_conv_id = mint_conv(storage.as_ref()).await;
                ok_val(
                    storage
                        .commit(
                            vec![StorageWrite::Conversation {
                                value: ConversationRecord {
                                    id: second_conv_id,
                                    parent: None,
                                    owner: None,
                                },
                            }],
                            ctx.clone(),
                        )
                        .await,
                );

                let first_id = mint_submission(storage.as_ref()).await;
                let second_id = mint_submission(storage.as_ref()).await;
                let other_conv_id = mint_submission(storage.as_ref()).await;

                let first = SubmissionRecord {
                    id: first_id,
                    conversation_id: root_id,
                    request_id: Some("same".to_string()),
                    state: SubmissionState::Input(InputSubmissionState::Queued),
                };
                let second = SubmissionRecord {
                    id: second_id,
                    conversation_id: root_id,
                    request_id: Some("other".to_string()),
                    state: SubmissionState::Input(InputSubmissionState::Queued),
                };
                let other_conversation = SubmissionRecord {
                    id: other_conv_id,
                    conversation_id: second_conv_id,
                    request_id: Some("same".to_string()),
                    state: SubmissionState::Input(InputSubmissionState::Queued),
                };
                ok_val(
                    storage
                        .commit(
                            vec![
                                StorageWrite::Submission { value: first.clone() },
                                StorageWrite::Submission { value: second.clone() },
                                StorageWrite::Submission {
                                    value: other_conversation.clone(),
                                },
                            ],
                            ctx.clone(),
                        )
                        .await,
                );
                expect.deep_equal(
                    &to_val(&ok_val(
                        storage.submission_by_request(root_id, "same", ctx.clone()).await,
                    )),
                    &to_val(&Some(&first)),
                );
                expect.deep_equal(
                    &to_val(&ok_val(
                        storage.submission_by_request(second_conv_id, "same", ctx.clone()).await,
                    )),
                    &to_val(&Some(&other_conversation)),
                );

                let placed_second = SubmissionRecord {
                    id: second_id,
                    conversation_id: root_id,
                    request_id: Some("other".to_string()),
                    state: SubmissionState::Input(InputSubmissionState::Placed {
                        entry: mint_entry(storage.as_ref()).await,
                    }),
                };
                ok_val(
                    storage
                        .commit(
                            vec![StorageWrite::Submission {
                                value: placed_second.clone(),
                            }],
                            ctx.clone(),
                        )
                        .await,
                );
                expect.deep_equal(
                    &to_val(&ok_val(storage.submission(second_id, ctx.clone()).await)),
                    &to_val(&Some(&placed_second)),
                );
                expect.deep_equal(
                    &to_val(&ok_val(
                        storage.submission_by_request(root_id, "other", ctx.clone()).await,
                    )),
                    &to_val(&Some(&placed_second)),
                );

                let all_ids = scan_submission_ids(storage.as_ref(), SubmissionQuery::default()).await;
                expect.deep_equal(&to_val(&all_ids), &to_val(&vec![first_id, second_id, other_conv_id]));
                let root_ids = scan_submission_ids(
                    storage.as_ref(),
                    SubmissionQuery {
                        conversation_id: Some(root_id),
                        status: None,
                    },
                )
                .await;
                expect.deep_equal(&to_val(&root_ids), &to_val(&vec![first_id, second_id]));
                let queued_ids = scan_submission_ids(
                    storage.as_ref(),
                    SubmissionQuery {
                        conversation_id: None,
                        status: Some(SubmissionStatus::Queued),
                    },
                )
                .await;
                expect.deep_equal(&to_val(&queued_ids), &to_val(&vec![first_id, other_conv_id]));
                let placed_ids = scan_submission_ids(
                    storage.as_ref(),
                    SubmissionQuery {
                        conversation_id: None,
                        status: Some(SubmissionStatus::Placed),
                    },
                )
                .await;
                expect.deep_equal(&to_val(&placed_ids), &to_val(&vec![second_id]));
                let sec_queued = scan_submission_ids(
                    storage.as_ref(),
                    SubmissionQuery {
                        conversation_id: Some(second_conv_id),
                        status: Some(SubmissionStatus::Queued),
                    },
                )
                .await;
                expect.deep_equal(&to_val(&sec_queued), &to_val(&vec![other_conv_id]));
                let sec_placed = scan_submission_ids(
                    storage.as_ref(),
                    SubmissionQuery {
                        conversation_id: Some(second_conv_id),
                        status: Some(SubmissionStatus::Placed),
                    },
                )
                .await;
                expect.deep_equal(&to_val(&sec_placed), &json!([]));
                expect.deep_equal(
                    &to_val(
                        &ok_val(
                            storage
                                .scan_submissions(
                                    SubmissionQuery {
                                        conversation_id: None,
                                        status: Some(SubmissionStatus::Placed),
                                    },
                                    10,
                                    None,
                                    ctx,
                                )
                                .await,
                        )
                        .items,
                    ),
                    &to_val(&vec![placed_second]),
                );
            },
        ),
        create_case(
            w,
            a,
            "stores passive write submissions without input-only lifecycle states",
            |storage, expect| async move {
                let ctx = BACKGROUND_CONTEXT.clone();
                let root_id = create_root(storage.as_ref()).await;
                let done_id = mint_submission(storage.as_ref()).await;
                let failed_id = mint_submission(storage.as_ref()).await;

                let queued_done = SubmissionRecord {
                    id: done_id,
                    conversation_id: root_id,
                    request_id: Some("passive-done".to_string()),
                    state: SubmissionState::Write(WriteSubmissionState::Queued),
                };
                let queued_failed = SubmissionRecord {
                    id: failed_id,
                    conversation_id: root_id,
                    request_id: Some("passive-failed".to_string()),
                    state: SubmissionState::Write(WriteSubmissionState::Queued),
                };
                ok_val(
                    storage
                        .commit(
                            vec![
                                StorageWrite::Submission { value: queued_done },
                                StorageWrite::Submission { value: queued_failed },
                            ],
                            ctx.clone(),
                        )
                        .await,
                );

                let done = SubmissionRecord {
                    id: done_id,
                    conversation_id: root_id,
                    request_id: Some("passive-done".to_string()),
                    state: SubmissionState::Write(WriteSubmissionState::Done {
                        entry: mint_entry(storage.as_ref()).await,
                    }),
                };
                let unanswered = SubmissionRecord {
                    id: failed_id,
                    conversation_id: root_id,
                    request_id: Some("passive-failed".to_string()),
                    state: SubmissionState::Write(WriteSubmissionState::Unanswered {
                        reason: "closed".to_string(),
                        detail: Some(json!({ "retryable": false })),
                    }),
                };
                ok_val(
                    storage
                        .commit(
                            vec![
                                StorageWrite::Submission { value: done.clone() },
                                StorageWrite::Submission {
                                    value: unanswered.clone(),
                                },
                            ],
                            ctx.clone(),
                        )
                        .await,
                );
                expect.deep_equal(
                    &to_val(&ok_val(storage.submission(done_id, ctx.clone()).await)),
                    &to_val(&Some(&done)),
                );
                expect.deep_equal(
                    &to_val(&ok_val(
                        storage
                            .submission_by_request(root_id, "passive-done", ctx.clone())
                            .await,
                    )),
                    &to_val(&Some(&done)),
                );
                expect.deep_equal(
                    &to_val(&ok_val(storage.submission(failed_id, ctx.clone()).await)),
                    &to_val(&Some(&unanswered)),
                );
                expect.deep_equal(
                    &to_val(&ok_val(
                        storage.submission_by_request(root_id, "passive-failed", ctx).await,
                    )),
                    &to_val(&Some(&unanswered)),
                );
            },
        ),
        create_case(
            w,
            a,
            "reconstructs rewindable documents and preserves half-open incarnations",
            |storage, expect| async move {
                let ctx = BACKGROUND_CONTEXT.clone();
                let root_id = create_root(storage.as_ref()).await;
                let first_id = mint_document(storage.as_ref()).await;
                let first_record = DocumentCreate {
                    id: first_id,
                    kind: "conversation.notes".to_string(),
                    key: None,
                    scope: DocumentScope::Conversation {
                        conversation_id: root_id,
                    },
                    history: Some(DocumentHistory::Rewindable),
                    fork: Some(DocumentFork::AsOf),
                };
                let created_at = ok_val(
                    storage
                        .commit(
                            vec![StorageWrite::DocumentCreate {
                                record: first_record.clone(),
                                version: 1,
                                value: json_obj(json!({ "items": ["a"], "nested": { "count": 1 } })),
                            }],
                            ctx.clone(),
                        )
                        .await,
                );
                let ops = vec![
                    Op::Splice(OpSplice(
                        SpliceVerb::P,
                        vec![Seg::String("items".to_string())],
                        1,
                        0,
                        vec![json!("b")],
                    )),
                    Op::Set(OpSet(
                        SetVerb::S,
                        vec![Seg::String("nested".to_string()), Seg::String("count".to_string())],
                        json!(2),
                    )),
                ];
                let changed_at = ok_val(
                    storage
                        .commit(
                            vec![StorageWrite::DocumentChange {
                                id: first_id,
                                content: DocumentContent::Delta { version: 1, ops },
                            }],
                            ctx.clone(),
                        )
                        .await,
                );

                let at_create = ok_val(
                    storage
                        .document(first_id, DocumentPoint::At(created_at), ctx.clone())
                        .await,
                );
                expect.partial_deep_equal(
                    &doc_summary(&at_create),
                    &json!({ "version": 1, "value": { "items": ["a"], "nested": { "count": 1 } }, "deltasSinceBase": 0 }),
                );
                let changed = ok_val(
                    storage
                        .document(first_id, DocumentPoint::At(changed_at), ctx.clone())
                        .await,
                );
                expect.partial_deep_equal(
                    &doc_summary(&changed),
                    &json!({ "value": { "items": ["a", "b"], "nested": { "count": 2 } }, "deltasSinceBase": 1 }),
                );

                let checkpoint_at = ok_val(
                    storage
                        .commit(
                            vec![StorageWrite::DocumentChange {
                                id: first_id,
                                content: DocumentContent::Base {
                                    version: 2,
                                    value: json_obj(json!({ "items": ["checkpoint"], "nested": { "count": 3 } })),
                                },
                            }],
                            ctx.clone(),
                        )
                        .await,
                );
                let replaced_at = ok_val(
                    storage
                        .commit(
                            vec![StorageWrite::DocumentChange {
                                id: first_id,
                                content: DocumentContent::Delta {
                                    version: 2,
                                    ops: vec![Op::Replace(OpReplace(
                                        ReplaceVerb::R,
                                        json!({ "items": ["replacement"], "nested": { "count": 4 } }),
                                    ))],
                                },
                            }],
                            ctx.clone(),
                        )
                        .await,
                );
                expect.partial_deep_equal(
                    &doc_summary(&ok_val(
                        storage
                            .document(first_id, DocumentPoint::At(changed_at), ctx.clone())
                            .await,
                    )),
                    &json!({ "version": 1, "value": { "items": ["a", "b"], "nested": { "count": 2 } } }),
                );
                expect.partial_deep_equal(
                    &doc_summary(&ok_val(storage.document(first_id, DocumentPoint::At(checkpoint_at), ctx.clone()).await)),
                    &json!({ "version": 2, "value": { "items": ["checkpoint"], "nested": { "count": 3 } }, "deltasSinceBase": 0 }),
                );
                expect.partial_deep_equal(
                    &doc_summary(&ok_val(
                        storage
                            .document(first_id, DocumentPoint::At(replaced_at), ctx.clone())
                            .await,
                    )),
                    &json!({ "value": { "items": ["replacement"], "nested": { "count": 4 } }, "deltasSinceBase": 1 }),
                );
                expect.strict_equal(
                    &to_val(
                        &ok_val(storage.document(first_id, DocumentPoint::Current, ctx.clone()).await)
                            .map(|d| d.deltas_since_base),
                    ),
                    &to_val(&Some(1usize)),
                );

                let second_id = mint_document(storage.as_ref()).await;
                let mut second_rec = first_record.clone();
                second_rec.id = second_id;
                let retired_at = ok_val(
                    storage
                        .commit(
                            vec![
                                StorageWrite::DocumentCreate {
                                    record: second_rec,
                                    version: 1,
                                    value: json_obj(json!({ "items": ["new"] })),
                                },
                                StorageWrite::DocumentRetire { id: first_id },
                                StorageWrite::DocumentChange {
                                    id: first_id,
                                    content: DocumentContent::Delta {
                                        version: 2,
                                        ops: vec![Op::Set(OpSet(
                                            SetVerb::S,
                                            vec![Seg::String("retiring".to_string())],
                                            json!(true),
                                        ))],
                                    },
                                },
                            ],
                            ctx.clone(),
                        )
                        .await,
                );

                let address = DocumentAddress {
                    kind: first_record.kind.clone(),
                    scope: first_record.scope,
                    key: None,
                };
                expect.strict_equal(
                    &to_val(
                        &ok_val(
                            storage
                                .find_document(address.clone(), DocumentPoint::At(changed_at), ctx.clone())
                                .await,
                        )
                        .map(|d| d.id),
                    ),
                    &to_val(&Some(first_id)),
                );
                expect.partial_deep_equal(
                    &to_val(&ok_val(
                        storage
                            .find_document(address, DocumentPoint::At(retired_at), ctx.clone())
                            .await,
                    )),
                    &json!({ "id": second_id, "createdAt": retired_at }),
                );
                expect.deep_equal(
                    &to_val(
                        &ok_val(
                            storage
                                .scan_documents(
                                    DocumentQuery {
                                        scope: first_record.scope,
                                        at: DocumentPoint::At(changed_at),
                                        kind: None,
                                    },
                                    10,
                                    None,
                                    ctx.clone(),
                                )
                                .await,
                        )
                        .items
                        .iter()
                        .map(|d| d.id)
                        .collect::<Vec<_>>(),
                    ),
                    &to_val(&vec![first_id]),
                );
                expect.deep_equal(
                    &to_val(
                        &ok_val(
                            storage
                                .scan_documents(
                                    DocumentQuery {
                                        scope: first_record.scope,
                                        at: DocumentPoint::At(retired_at),
                                        kind: None,
                                    },
                                    10,
                                    None,
                                    ctx.clone(),
                                )
                                .await,
                        )
                        .items
                        .iter()
                        .map(|d| d.id)
                        .collect::<Vec<_>>(),
                    ),
                    &to_val(&vec![second_id]),
                );
                expect.ok(
                    ok_val(
                        storage
                            .document(first_id, DocumentPoint::At(retired_at), ctx.clone())
                            .await,
                    )
                    .is_none(),
                    "retired doc must not be visible at retired_at",
                );
                expect.deep_equal(
                    &to_val(&ok_val(storage.document(second_id, DocumentPoint::Current, ctx).await).map(|d| d.value)),
                    &json!({ "items": ["new"] }),
                );
            },
        ),
        create_case(
            w,
            a,
            "streams long document tails across root replacement deltas",
            |storage, expect| async move {
                let ctx = BACKGROUND_CONTEXT.clone();
                let root_id = create_root(storage.as_ref()).await;
                let id = mint_document(storage.as_ref()).await;
                let record = DocumentCreate {
                    id,
                    kind: "conversation.long-tail".to_string(),
                    key: None,
                    scope: DocumentScope::Conversation {
                        conversation_id: root_id,
                    },
                    history: Some(DocumentHistory::Rewindable),
                    fork: Some(DocumentFork::AsOf),
                };
                let initial_rows: Vec<JsonValue> = (0..512)
                    .map(|value| json!({ "value": value, "stable": format!("row-{value}") }))
                    .collect();
                let initial = json_obj(json!({ "revision": 0, "rows": initial_rows }));
                let created_at = ok_val(
                    storage
                        .commit(
                            vec![StorageWrite::DocumentCreate {
                                record,
                                version: 1,
                                value: initial.clone(),
                            }],
                            ctx.clone(),
                        )
                        .await,
                );

                let mut before_replacement = initial.clone();
                let mut before_replacement_at = created_at;
                for revision in 1i64..=24 {
                    let index = (revision as usize * 17) % 512;
                    if let Some(JsonValue::Array(rows)) = before_replacement.get_mut("rows")
                        && let Some(JsonValue::Object(row)) = rows.get_mut(index)
                    {
                        row.insert("value".to_string(), json!(-revision));
                    }
                    before_replacement.insert("revision".to_string(), json!(revision));
                    before_replacement_at = ok_val(
                        storage
                            .commit(
                                vec![StorageWrite::DocumentChange {
                                    id,
                                    content: DocumentContent::Delta {
                                        version: 1,
                                        ops: vec![
                                            Op::Set(OpSet(
                                                SetVerb::S,
                                                vec![
                                                    Seg::String("rows".to_string()),
                                                    Seg::Index(index as i64),
                                                    Seg::String("value".to_string()),
                                                ],
                                                json!(-revision),
                                            )),
                                            Op::Set(OpSet(
                                                SetVerb::S,
                                                vec![Seg::String("revision".to_string())],
                                                json!(revision),
                                            )),
                                        ],
                                    },
                                }],
                                ctx.clone(),
                            )
                            .await,
                    );
                }

                let replacement_rows: Vec<JsonValue> = (0..512)
                    .map(|value| json!({ "value": 10_000 + value, "stable": format!("new-{value}") }))
                    .collect();
                let replacement = json_obj(json!({ "revision": 100, "rows": replacement_rows }));
                let replacement_snapshot = replacement.clone();
                let replacement_at = ok_val(
                    storage
                        .commit(
                            vec![StorageWrite::DocumentChange {
                                id,
                                content: DocumentContent::Delta {
                                    version: 1,
                                    ops: vec![Op::Replace(OpReplace(ReplaceVerb::R, JsonValue::Object(replacement)))],
                                },
                            }],
                            ctx.clone(),
                        )
                        .await,
                );

                let mut current = replacement_snapshot.clone();
                for revision in 101i64..=124 {
                    let index = (revision as usize * 19) % 512;
                    if let Some(JsonValue::Array(rows)) = current.get_mut("rows")
                        && let Some(JsonValue::Object(row)) = rows.get_mut(index)
                    {
                        row.insert("value".to_string(), json!(-revision));
                    }
                    current.insert("revision".to_string(), json!(revision));
                    ok_val(
                        storage
                            .commit(
                                vec![StorageWrite::DocumentChange {
                                    id,
                                    content: DocumentContent::Delta {
                                        version: 1,
                                        ops: vec![
                                            Op::Set(OpSet(
                                                SetVerb::S,
                                                vec![
                                                    Seg::String("rows".to_string()),
                                                    Seg::Index(index as i64),
                                                    Seg::String("value".to_string()),
                                                ],
                                                json!(-revision),
                                            )),
                                            Op::Set(OpSet(
                                                SetVerb::S,
                                                vec![Seg::String("revision".to_string())],
                                                json!(revision),
                                            )),
                                        ],
                                    },
                                }],
                                ctx.clone(),
                            )
                            .await,
                    );
                }

                expect.deep_equal(
                    &to_val(
                        &ok_val(storage.document(id, DocumentPoint::At(created_at), ctx.clone()).await)
                            .map(|d| d.value),
                    ),
                    &to_val(&Some(initial)),
                );
                expect.deep_equal(
                    &to_val(
                        &ok_val(
                            storage
                                .document(id, DocumentPoint::At(before_replacement_at), ctx.clone())
                                .await,
                        )
                        .map(|d| d.value),
                    ),
                    &to_val(&Some(before_replacement)),
                );
                expect.deep_equal(
                    &to_val(
                        &ok_val(
                            storage
                                .document(id, DocumentPoint::At(replacement_at), ctx.clone())
                                .await,
                        )
                        .map(|d| d.value),
                    ),
                    &to_val(&Some(replacement_snapshot)),
                );
                expect.deep_equal(
                    &to_val(&ok_val(storage.document(id, DocumentPoint::Current, ctx).await).map(|d| d.value)),
                    &to_val(&Some(current)),
                );
            },
        ),
        create_case(
            w,
            a,
            "copies stored document bases independently and rejects ambiguous sources",
            |storage, expect| async move {
                let ctx = BACKGROUND_CONTEXT.clone();
                let root_id = create_root(storage.as_ref()).await;
                let child_id = mint_conv(storage.as_ref()).await;
                let second_child_id = mint_conv(storage.as_ref()).await;
                ok_val(
                    storage
                        .commit(
                            vec![
                                StorageWrite::Conversation {
                                    value: ConversationRecord {
                                        id: child_id,
                                        parent: None,
                                        owner: None,
                                    },
                                },
                                StorageWrite::Conversation {
                                    value: ConversationRecord {
                                        id: second_child_id,
                                        parent: None,
                                        owner: None,
                                    },
                                },
                            ],
                            ctx.clone(),
                        )
                        .await,
                );

                let source_id = mint_document(storage.as_ref()).await;
                let source_record = DocumentCreate {
                    id: source_id,
                    kind: "copy.source".to_string(),
                    key: None,
                    scope: DocumentScope::Conversation {
                        conversation_id: root_id,
                    },
                    history: Some(DocumentHistory::Rewindable),
                    fork: Some(DocumentFork::AsOf),
                };
                let created_at = ok_val(
                    storage
                        .commit(
                            vec![StorageWrite::DocumentCreate {
                                record: source_record.clone(),
                                version: 2,
                                value: json_obj(json!({ "count": 1, "rows": [{ "value": "base" }] })),
                            }],
                            ctx.clone(),
                        )
                        .await,
                );
                ok_val(
                    storage
                        .commit(
                            vec![StorageWrite::DocumentChange {
                                id: source_id,
                                content: DocumentContent::Delta {
                                    version: 2,
                                    ops: vec![
                                        Op::Set(OpSet(SetVerb::S, vec![Seg::String("count".to_string())], json!(2))),
                                        Op::Splice(OpSplice(
                                            SpliceVerb::P,
                                            vec![Seg::String("rows".to_string())],
                                            1,
                                            0,
                                            vec![json!({ "value": "current" })],
                                        )),
                                    ],
                                },
                            }],
                            ctx.clone(),
                        )
                        .await,
                );

                let historical_copy_id = mint_document(storage.as_ref()).await;
                let current_copy_id = mint_document(storage.as_ref()).await;
                let retired_copy_id = mint_document(storage.as_ref()).await;
                let child_record = |id: DocumentId, conv: ConversationId| DocumentCreate {
                    id,
                    kind: "copy.source".to_string(),
                    key: None,
                    scope: DocumentScope::Conversation { conversation_id: conv },
                    history: Some(DocumentHistory::Rewindable),
                    fork: Some(DocumentFork::AsOf),
                };
                ok_val(
                    storage
                        .commit(
                            vec![
                                StorageWrite::DocumentCopy {
                                    record: child_record(historical_copy_id, child_id),
                                    source: DocumentCopySource {
                                        id: source_id,
                                        at: DocumentPoint::At(created_at),
                                    },
                                },
                                StorageWrite::DocumentCopy {
                                    record: child_record(current_copy_id, second_child_id),
                                    source: DocumentCopySource {
                                        id: source_id,
                                        at: DocumentPoint::Current,
                                    },
                                },
                                StorageWrite::DocumentCopy {
                                    record: child_record(retired_copy_id, root_id),
                                    source: DocumentCopySource {
                                        id: source_id,
                                        at: DocumentPoint::Current,
                                    },
                                },
                                StorageWrite::DocumentRetire { id: retired_copy_id },
                            ],
                            ctx.clone(),
                        )
                        .await,
                );
                expect.partial_deep_equal(
                    &doc_summary(&ok_val(
                        storage
                            .document(historical_copy_id, DocumentPoint::Current, ctx.clone())
                            .await,
                    )),
                    &json!({ "version": 2, "value": { "count": 1, "rows": [{ "value": "base" }] } }),
                );
                expect.partial_deep_equal(
                    &doc_summary(&ok_val(storage.document(current_copy_id, DocumentPoint::Current, ctx.clone()).await)),
                    &json!({ "version": 2, "value": { "count": 2, "rows": [{ "value": "base" }, { "value": "current" }] } }),
                );
                expect.ok(
                    ok_val(
                        storage
                            .document(retired_copy_id, DocumentPoint::Current, ctx.clone())
                            .await,
                    )
                    .is_none(),
                    "retired copy should be None",
                );

                ok_val(
                    storage
                        .commit(
                            vec![
                                StorageWrite::DocumentChange {
                                    id: source_id,
                                    content: DocumentContent::Base {
                                        version: 2,
                                        value: json_obj(json!({ "count": 99, "rows": [] })),
                                    },
                                },
                                StorageWrite::DocumentRetire { id: source_id },
                            ],
                            ctx.clone(),
                        )
                        .await,
                );
                expect.deep_equal(
                    &to_val(
                        &ok_val(
                            storage
                                .document(current_copy_id, DocumentPoint::Current, ctx.clone())
                                .await,
                        )
                        .map(|d| d.value),
                    ),
                    &json!({ "count": 2, "rows": [{ "value": "base" }, { "value": "current" }] }),
                );

                let latest_source_id = mint_document(storage.as_ref()).await;
                let latest_copy_id = mint_document(storage.as_ref()).await;
                let latest_source = DocumentCreate {
                    id: latest_source_id,
                    kind: "copy.latest".to_string(),
                    key: None,
                    scope: DocumentScope::Conversation {
                        conversation_id: root_id,
                    },
                    history: Some(DocumentHistory::Latest),
                    fork: Some(DocumentFork::Current),
                };
                ok_val(
                    storage
                        .commit(
                            vec![StorageWrite::DocumentCreate {
                                record: latest_source.clone(),
                                version: 4,
                                value: json_obj(json!({ "retained": "copy" })),
                            }],
                            ctx.clone(),
                        )
                        .await,
                );
                let mut latest_copy_rec = latest_source.clone();
                latest_copy_rec.id = latest_copy_id;
                latest_copy_rec.scope = DocumentScope::Conversation {
                    conversation_id: child_id,
                };
                ok_val(
                    storage
                        .commit(
                            vec![StorageWrite::DocumentCopy {
                                record: latest_copy_rec,
                                source: DocumentCopySource {
                                    id: latest_source_id,
                                    at: DocumentPoint::Current,
                                },
                            }],
                            ctx.clone(),
                        )
                        .await,
                );
                ok_val(
                    storage
                        .commit(
                            vec![
                                StorageWrite::DocumentChange {
                                    id: latest_source_id,
                                    content: DocumentContent::Base {
                                        version: 4,
                                        value: json_obj(json!({ "retained": "source-only" })),
                                    },
                                },
                                StorageWrite::DocumentRetire { id: latest_source_id },
                            ],
                            ctx.clone(),
                        )
                        .await,
                );
                expect.partial_deep_equal(
                    &doc_summary(&ok_val(
                        storage
                            .document(latest_copy_id, DocumentPoint::Current, ctx.clone())
                            .await,
                    )),
                    &json!({ "version": 4, "value": { "retained": "copy" } }),
                );

                let conflict_id = mint_document(storage.as_ref()).await;
                let conflict_res = storage
                    .commit(
                        vec![
                            StorageWrite::DocumentCopy {
                                record: child_record(conflict_id, child_id),
                                source: DocumentCopySource {
                                    id: current_copy_id,
                                    at: DocumentPoint::Current,
                                },
                            },
                            StorageWrite::DocumentRetire { id: current_copy_id },
                        ],
                        ctx.clone(),
                    )
                    .await
                    .map(|_| ());
                expect.rejects_error_name(conflict_res, "StorageRejected");
                expect.ok(
                    ok_val(storage.document(conflict_id, DocumentPoint::Current, ctx.clone()).await).is_none(),
                    "conflict copy must not persist",
                );
                expect.deep_equal(
                    &to_val(
                        &ok_val(
                            storage
                                .document(current_copy_id, DocumentPoint::Current, ctx.clone())
                                .await,
                        )
                        .map(|d| d.value),
                    ),
                    &json!({ "count": 2, "rows": [{ "value": "base" }, { "value": "current" }] }),
                );

                let mismatch_id = mint_document(storage.as_ref()).await;
                let mut mismatch_rec = child_record(mismatch_id, child_id);
                mismatch_rec.kind = "copy.mismatch".to_string();
                let mismatch_res = storage
                    .commit(
                        vec![StorageWrite::DocumentCopy {
                            record: mismatch_rec,
                            source: DocumentCopySource {
                                id: current_copy_id,
                                at: DocumentPoint::Current,
                            },
                        }],
                        ctx.clone(),
                    )
                    .await
                    .map(|_| ());
                expect.rejects_error_name(mismatch_res, "StorageRejected");
                expect.ok(
                    ok_val(storage.document(mismatch_id, DocumentPoint::Current, ctx).await).is_none(),
                    "mismatch copy must not persist",
                );
            },
        ),
        create_case(
            w,
            a,
            "uses bases for version transitions and rejects historical reads of current-only documents",
            |storage, expect| async move {
                let ctx = BACKGROUND_CONTEXT.clone();
                create_root(storage.as_ref()).await;
                let id = mint_document(storage.as_ref()).await;
                ok_val(
                    storage
                        .commit(
                            vec![StorageWrite::DocumentCreate {
                                record: DocumentCreate {
                                    id,
                                    kind: "session.settings".to_string(),
                                    key: None,
                                    scope: DocumentScope::Session,
                                    history: None,
                                    fork: None,
                                },
                                version: 1,
                                value: json_obj(json!({ "count": 1 })),
                            }],
                            ctx.clone(),
                        )
                        .await,
                );
                ok_val(
                    storage
                        .commit(
                            vec![StorageWrite::DocumentChange {
                                id,
                                content: DocumentContent::Delta {
                                    version: 1,
                                    ops: vec![Op::Set(OpSet(
                                        SetVerb::S,
                                        vec![Seg::String("count".to_string())],
                                        json!(2),
                                    ))],
                                },
                            }],
                            ctx.clone(),
                        )
                        .await,
                );
                let migrated_at = ok_val(
                    storage
                        .commit(
                            vec![StorageWrite::DocumentChange {
                                id,
                                content: DocumentContent::Base {
                                    version: 2,
                                    value: json_obj(json!({ "count": 3 })),
                                },
                            }],
                            ctx.clone(),
                        )
                        .await,
                );
                expect.partial_deep_equal(
                    &doc_summary(&ok_val(storage.document(id, DocumentPoint::Current, ctx.clone()).await)),
                    &json!({ "version": 2, "value": { "count": 3 } }),
                );
                expect.rejects_containing(
                    storage
                        .document(id, DocumentPoint::At(migrated_at), ctx.clone())
                        .await
                        .map(|_| ()),
                    "does not retain historical content",
                );
                expect.rejects_containing(
                    storage
                        .commit(
                            vec![StorageWrite::DocumentChange {
                                id,
                                content: DocumentContent::Delta {
                                    version: 1,
                                    ops: vec![Op::Set(OpSet(
                                        SetVerb::S,
                                        vec![Seg::String("count".to_string())],
                                        json!(4),
                                    ))],
                                },
                            }],
                            ctx.clone(),
                        )
                        .await
                        .map(|_| ()),
                    "version transition requires a base",
                );
                expect.deep_equal(
                    &to_val(&ok_val(storage.document(id, DocumentPoint::Current, ctx.clone()).await).map(|d| d.value)),
                    &json!({ "count": 3 }),
                );
                ok_val(
                    storage
                        .commit(vec![StorageWrite::DocumentRetire { id }], ctx.clone())
                        .await,
                );
                expect.ok(
                    ok_val(storage.document(id, DocumentPoint::Current, ctx).await).is_none(),
                    "retired doc should return None",
                );
            },
        ),
        create_case(
            w,
            a,
            "indexes logical addresses and exact-scope scans independently",
            |storage, expect| async move {
                let ctx = BACKGROUND_CONTEXT.clone();
                let root_id = create_root(storage.as_ref()).await;
                let first_id = mint_document(storage.as_ref()).await;
                let second_id = mint_document(storage.as_ref()).await;
                let conversation_id = mint_document(storage.as_ref()).await;
                let task_id = mint_task(storage.as_ref()).await;
                let task_singleton_id = mint_document(storage.as_ref()).await;
                let task_family_id = mint_document(storage.as_ref()).await;
                let task_other_kind_id = mint_document(storage.as_ref()).await;

                let created_at = ok_val(
                    storage
                        .commit(
                            vec![
                                StorageWrite::Task {
                                    value: pending_task(task_id, root_id, "ready"),
                                },
                                StorageWrite::DocumentCreate {
                                    record: DocumentCreate {
                                        id: first_id,
                                        kind: "cache".to_string(),
                                        key: Some("__proto__".to_string()),
                                        scope: DocumentScope::Session,
                                        history: None,
                                        fork: None,
                                    },
                                    version: 1,
                                    value: json_obj(json!({ "owner": "first" })),
                                },
                                StorageWrite::DocumentCreate {
                                    record: DocumentCreate {
                                        id: second_id,
                                        kind: "cache".to_string(),
                                        key: Some("constructor".to_string()),
                                        scope: DocumentScope::Session,
                                        history: None,
                                        fork: None,
                                    },
                                    version: 1,
                                    value: json_obj(json!({ "owner": "second" })),
                                },
                                StorageWrite::DocumentCreate {
                                    record: DocumentCreate {
                                        id: conversation_id,
                                        kind: "cache".to_string(),
                                        key: Some("__proto__".to_string()),
                                        scope: DocumentScope::Conversation {
                                            conversation_id: root_id,
                                        },
                                        history: Some(DocumentHistory::Latest),
                                        fork: Some(DocumentFork::Current),
                                    },
                                    version: 1,
                                    value: json_obj(json!({ "owner": "conversation" })),
                                },
                                StorageWrite::DocumentCreate {
                                    record: DocumentCreate {
                                        id: task_singleton_id,
                                        kind: "task.cache".to_string(),
                                        key: None,
                                        scope: DocumentScope::Task { task_id },
                                        history: None,
                                        fork: None,
                                    },
                                    version: 1,
                                    value: json_obj(json!({ "owner": "singleton" })),
                                },
                                StorageWrite::DocumentCreate {
                                    record: DocumentCreate {
                                        id: task_family_id,
                                        kind: "task.cache".to_string(),
                                        key: Some("member".to_string()),
                                        scope: DocumentScope::Task { task_id },
                                        history: None,
                                        fork: None,
                                    },
                                    version: 1,
                                    value: json_obj(json!({ "owner": "family" })),
                                },
                                StorageWrite::DocumentCreate {
                                    record: DocumentCreate {
                                        id: task_other_kind_id,
                                        kind: "task.other".to_string(),
                                        key: None,
                                        scope: DocumentScope::Task { task_id },
                                        history: None,
                                        fork: None,
                                    },
                                    version: 1,
                                    value: json_obj(json!({ "owner": "other" })),
                                },
                            ],
                            ctx.clone(),
                        )
                        .await,
                );

                expect.strict_equal(
                    &to_val(
                        &ok_val(
                            storage
                                .find_document(
                                    DocumentAddress {
                                        kind: "cache".to_string(),
                                        scope: DocumentScope::Session,
                                        key: Some("__proto__".to_string()),
                                    },
                                    DocumentPoint::Current,
                                    ctx.clone(),
                                )
                                .await,
                        )
                        .map(|d| d.id),
                    ),
                    &to_val(&Some(first_id)),
                );

                let first = ok_val(
                    storage
                        .scan_documents(
                            DocumentQuery {
                                scope: DocumentScope::Session,
                                at: DocumentPoint::Current,
                                kind: None,
                            },
                            1,
                            None,
                            ctx.clone(),
                        )
                        .await,
                );
                expect.strict_equal(&json!(first.items.len()), &json!(1));
                let second = ok_val(
                    storage
                        .scan_documents(
                            DocumentQuery {
                                scope: DocumentScope::Session,
                                at: DocumentPoint::Current,
                                kind: None,
                            },
                            1,
                            first.next,
                            ctx.clone(),
                        )
                        .await,
                );
                let combined: Vec<DocumentId> = first.items.into_iter().chain(second.items).map(|d| d.id).collect();
                expect.deep_equal(&to_val(&combined), &to_val(&vec![first_id, second_id]));

                let conv_docs = ok_val(
                    storage
                        .scan_documents(
                            DocumentQuery {
                                scope: DocumentScope::Conversation {
                                    conversation_id: root_id,
                                },
                                at: DocumentPoint::Current,
                                kind: None,
                            },
                            10,
                            None,
                            ctx.clone(),
                        )
                        .await,
                );
                expect.deep_equal(
                    &to_val(&conv_docs.items.iter().map(|d| d.id).collect::<Vec<_>>()),
                    &to_val(&vec![conversation_id]),
                );

                expect.strict_equal(
                    &to_val(
                        &ok_val(
                            storage
                                .find_document(
                                    DocumentAddress {
                                        kind: "task.cache".to_string(),
                                        scope: DocumentScope::Task { task_id },
                                        key: None,
                                    },
                                    DocumentPoint::Current,
                                    ctx.clone(),
                                )
                                .await,
                        )
                        .map(|d| d.id),
                    ),
                    &to_val(&Some(task_singleton_id)),
                );
                expect.strict_equal(
                    &to_val(
                        &ok_val(
                            storage
                                .find_document(
                                    DocumentAddress {
                                        kind: "task.cache".to_string(),
                                        scope: DocumentScope::Task { task_id },
                                        key: Some("member".to_string()),
                                    },
                                    DocumentPoint::Current,
                                    ctx.clone(),
                                )
                                .await,
                        )
                        .map(|d| d.id),
                    ),
                    &to_val(&Some(task_family_id)),
                );
                expect.deep_equal(
                    &to_val(
                        &ok_val(
                            storage
                                .scan_documents(
                                    DocumentQuery {
                                        scope: DocumentScope::Task { task_id },
                                        at: DocumentPoint::Current,
                                        kind: Some("task.cache".to_string()),
                                    },
                                    10,
                                    None,
                                    ctx.clone(),
                                )
                                .await,
                        )
                        .items
                        .iter()
                        .map(|d| d.id)
                        .collect::<Vec<_>>(),
                    ),
                    &to_val(&vec![task_singleton_id, task_family_id]),
                );
                expect.rejects_containing(
                    storage
                        .document(task_singleton_id, DocumentPoint::At(created_at), ctx)
                        .await
                        .map(|_| ()),
                    "does not retain historical content",
                );
            },
        ),
        create_case(
            w,
            a,
            "keeps document lifecycle failures atomic and gives create-plus-retire an empty lifetime",
            |storage, expect| async move {
                let ctx = BACKGROUND_CONTEXT.clone();
                let root_id = create_root(storage.as_ref()).await;
                let first_id = mint_document(storage.as_ref()).await;
                let second_id = mint_document(storage.as_ref()).await;
                let record = DocumentCreate {
                    id: first_id,
                    kind: "singleton".to_string(),
                    key: None,
                    scope: DocumentScope::Session,
                    history: None,
                    fork: None,
                };
                ok_val(
                    storage
                        .commit(
                            vec![StorageWrite::DocumentCreate {
                                record: record.clone(),
                                version: 1,
                                value: json_obj(json!({ "value": 1 })),
                            }],
                            ctx.clone(),
                        )
                        .await,
                );

                let mut second_rec = record.clone();
                second_rec.id = second_id;
                expect.rejects_containing(
                    storage
                        .commit(
                            vec![
                                StorageWrite::DocumentCreate {
                                    record: second_rec,
                                    version: 1,
                                    value: json_obj(json!({ "value": 2 })),
                                },
                                StorageWrite::DocumentChange {
                                    id: first_id,
                                    content: DocumentContent::Delta {
                                        version: 1,
                                        ops: vec![],
                                    },
                                },
                            ],
                            ctx.clone(),
                        )
                        .await
                        .map(|_| ()),
                    "already has a current incarnation",
                );
                expect.deep_equal(
                    &to_val(
                        &ok_val(storage.document(first_id, DocumentPoint::Current, ctx.clone()).await).map(|d| d.value),
                    ),
                    &json!({ "value": 1 }),
                );
                expect.ok(
                    ok_val(storage.document(second_id, DocumentPoint::Current, ctx.clone()).await).is_none(),
                    "conflicting doc must not be created",
                );

                let empty_id = mint_document(storage.as_ref()).await;
                let empty_at = ok_val(
                    storage
                        .commit(
                            vec![
                                StorageWrite::DocumentCreate {
                                    record: DocumentCreate {
                                        id: empty_id,
                                        kind: record.kind.clone(),
                                        key: Some("empty".to_string()),
                                        scope: DocumentScope::Conversation {
                                            conversation_id: root_id,
                                        },
                                        history: Some(DocumentHistory::Rewindable),
                                        fork: Some(DocumentFork::Initial),
                                    },
                                    version: 1,
                                    value: JsonObject::new(),
                                },
                                StorageWrite::DocumentRetire { id: empty_id },
                            ],
                            ctx.clone(),
                        )
                        .await,
                );
                expect.ok(
                    ok_val(storage.document(empty_id, DocumentPoint::Current, ctx.clone()).await).is_none(),
                    "retired on creation doc should not be visible",
                );
                expect.ok(
                    ok_val(
                        storage
                            .document(empty_id, DocumentPoint::At(empty_at), ctx.clone())
                            .await,
                    )
                    .is_none(),
                    "empty lifetime doc should not be visible at empty_at",
                );
                expect.ok(
                    ok_val(
                        storage
                            .find_document(
                                DocumentAddress {
                                    kind: record.kind,
                                    scope: DocumentScope::Conversation {
                                        conversation_id: root_id,
                                    },
                                    key: Some("empty".to_string()),
                                },
                                DocumentPoint::At(empty_at),
                                ctx,
                            )
                            .await,
                    )
                    .is_none(),
                    "find_document should return None for empty lifetime",
                );
            },
        ),
        create_case(
            w,
            a,
            "rolls back record tables and secondary indexes when a document command fails",
            |storage, expect| async move {
                let ctx = BACKGROUND_CONTEXT.clone();
                let root_id = create_root(storage.as_ref()).await;
                let task_id = mint_task(storage.as_ref()).await;
                let submission_id = mint_submission(storage.as_ref()).await;
                let document_id = mint_document(storage.as_ref()).await;
                let task = pending_task(task_id, root_id, "ready");
                let submission = SubmissionRecord {
                    id: submission_id,
                    conversation_id: root_id,
                    request_id: Some("atomic".to_string()),
                    state: SubmissionState::Input(InputSubmissionState::Queued),
                };
                let record = DocumentCreate {
                    id: document_id,
                    kind: "atomic".to_string(),
                    key: None,
                    scope: DocumentScope::Session,
                    history: None,
                    fork: None,
                };
                let baseline_seq = ok_val(
                    storage
                        .commit(
                            vec![
                                StorageWrite::Task { value: task.clone() },
                                StorageWrite::Submission {
                                    value: submission.clone(),
                                },
                                StorageWrite::DocumentCreate {
                                    record: record.clone(),
                                    version: 1,
                                    value: json_obj(json!({ "count": 1 })),
                                },
                            ],
                            ctx.clone(),
                        )
                        .await,
                );

                let entry_id = mint_entry(storage.as_ref()).await;
                let conflicting_id = mint_document(storage.as_ref()).await;
                let mut running_task = task.clone();
                running_task.state = TaskState::Running {
                    checkpoint: json!({ "phase": "effect" }),
                };
                let unanswered_sub = SubmissionRecord {
                    id: submission_id,
                    conversation_id: root_id,
                    request_id: Some("atomic".to_string()),
                    state: SubmissionState::Input(InputSubmissionState::Unanswered {
                        entry: None,
                        reason: "failed".to_string(),
                        detail: None,
                    }),
                };
                let mut conflict_rec = record.clone();
                conflict_rec.id = conflicting_id;
                expect.rejects_containing(
                    storage
                        .commit(
                            vec![
                                StorageWrite::Task { value: running_task },
                                StorageWrite::Submission { value: unanswered_sub },
                                StorageWrite::Entry {
                                    value: make_entry(entry_id, root_id, "transient"),
                                },
                                StorageWrite::DocumentCreate {
                                    record: conflict_rec,
                                    version: 1,
                                    value: json_obj(json!({ "count": 2 })),
                                },
                            ],
                            ctx.clone(),
                        )
                        .await
                        .map(|_| ()),
                    "already has a current incarnation",
                );

                expect.deep_equal(
                    &to_val(&ok_val(storage.task(task_id, ctx.clone()).await)),
                    &to_val(&Some(&task)),
                );
                expect.deep_equal(
                    &to_val(
                        &ok_val(
                            storage
                                .scan_tasks(
                                    TaskQuery {
                                        status: Some(TaskStatus::Pending),
                                        ..Default::default()
                                    },
                                    10,
                                    None,
                                    ctx.clone(),
                                )
                                .await,
                        )
                        .items,
                    ),
                    &to_val(&vec![task]),
                );
                expect.deep_equal(
                    &to_val(&ok_val(
                        storage.submission_by_request(root_id, "atomic", ctx.clone()).await,
                    )),
                    &to_val(&Some(submission)),
                );
                expect.ok(
                    ok_val(storage.entry(entry_id, ctx.clone()).await).is_none(),
                    "transient entry rolled back",
                );
                expect.ok(
                    ok_val(
                        storage
                            .document(conflicting_id, DocumentPoint::Current, ctx.clone())
                            .await,
                    )
                    .is_none(),
                    "conflicting doc rolled back",
                );
                expect.strict_equal(
                    &to_val(
                        &ok_val(
                            storage
                                .find_document(
                                    DocumentAddress {
                                        kind: record.kind,
                                        scope: record.scope,
                                        key: None,
                                    },
                                    DocumentPoint::Current,
                                    ctx.clone(),
                                )
                                .await,
                        )
                        .map(|d| d.id),
                    ),
                    &to_val(&Some(document_id)),
                );
                let after_rollback_seq = ok_val(
                    storage
                        .commit(
                            vec![StorageWrite::DocumentChange {
                                id: document_id,
                                content: DocumentContent::Delta {
                                    version: 1,
                                    ops: vec![Op::Set(OpSet(
                                        SetVerb::S,
                                        vec![Seg::String("count".to_string())],
                                        json!(3),
                                    ))],
                                },
                            }],
                            ctx,
                        )
                        .await,
                );
                expect.greater_than(after_rollback_seq.as_i64(), baseline_seq.as_i64());
            },
        ),
        create_case(
            w,
            a,
            "keeps indexed string identities lossless",
            |storage, expect| async move {
                let ctx = BACKGROUND_CONTEXT.clone();
                let root_id = create_root(storage.as_ref()).await;
                let first = "identity\0\"\\a";
                let second = "identity\0\"\\b";
                let first_task_id = mint_task(storage.as_ref()).await;
                let second_task_id = mint_task(storage.as_ref()).await;
                let first_sub_id = mint_submission(storage.as_ref()).await;
                let second_sub_id = mint_submission(storage.as_ref()).await;
                let first_kind_doc_id = mint_document(storage.as_ref()).await;
                let second_kind_doc_id = mint_document(storage.as_ref()).await;
                let first_key_doc_id = mint_document(storage.as_ref()).await;
                let second_key_doc_id = mint_document(storage.as_ref()).await;

                let mut t1 = pending_task(first_task_id, root_id, "ready");
                t1.kind = first.to_string();
                let mut t2 = pending_task(second_task_id, root_id, "ready");
                t2.kind = second.to_string();

                ok_val(
                    storage
                        .commit(
                            vec![
                                StorageWrite::Task { value: t1 },
                                StorageWrite::Task { value: t2 },
                                StorageWrite::Submission {
                                    value: SubmissionRecord {
                                        id: first_sub_id,
                                        conversation_id: root_id,
                                        request_id: Some(first.to_string()),
                                        state: SubmissionState::Input(InputSubmissionState::Queued),
                                    },
                                },
                                StorageWrite::Submission {
                                    value: SubmissionRecord {
                                        id: second_sub_id,
                                        conversation_id: root_id,
                                        request_id: Some(second.to_string()),
                                        state: SubmissionState::Input(InputSubmissionState::Queued),
                                    },
                                },
                                StorageWrite::DocumentCreate {
                                    record: DocumentCreate {
                                        id: first_kind_doc_id,
                                        kind: first.to_string(),
                                        key: None,
                                        scope: DocumentScope::Session,
                                        history: None,
                                        fork: None,
                                    },
                                    version: 1,
                                    value: json_obj(json!({ "identity": "first kind" })),
                                },
                                StorageWrite::DocumentCreate {
                                    record: DocumentCreate {
                                        id: second_kind_doc_id,
                                        kind: second.to_string(),
                                        key: None,
                                        scope: DocumentScope::Session,
                                        history: None,
                                        fork: None,
                                    },
                                    version: 1,
                                    value: json_obj(json!({ "identity": "second kind" })),
                                },
                                StorageWrite::DocumentCreate {
                                    record: DocumentCreate {
                                        id: first_key_doc_id,
                                        kind: "family".to_string(),
                                        key: Some(first.to_string()),
                                        scope: DocumentScope::Session,
                                        history: None,
                                        fork: None,
                                    },
                                    version: 1,
                                    value: json_obj(json!({ "identity": "first key" })),
                                },
                                StorageWrite::DocumentCreate {
                                    record: DocumentCreate {
                                        id: second_key_doc_id,
                                        kind: "family".to_string(),
                                        key: Some(second.to_string()),
                                        scope: DocumentScope::Session,
                                        history: None,
                                        fork: None,
                                    },
                                    version: 1,
                                    value: json_obj(json!({ "identity": "second key" })),
                                },
                            ],
                            ctx.clone(),
                        )
                        .await,
                );

                expect.deep_equal(
                    &to_val(
                        &ok_val(
                            storage
                                .scan_tasks(
                                    TaskQuery {
                                        kind: Some(first.to_string()),
                                        ..Default::default()
                                    },
                                    10,
                                    None,
                                    ctx.clone(),
                                )
                                .await,
                        )
                        .items
                        .iter()
                        .map(|t| t.id)
                        .collect::<Vec<_>>(),
                    ),
                    &to_val(&vec![first_task_id]),
                );
                expect.strict_equal(
                    &to_val(&ok_val(storage.submission_by_request(root_id, first, ctx.clone()).await).map(|s| s.id)),
                    &to_val(&Some(first_sub_id)),
                );
                expect.strict_equal(
                    &to_val(&ok_val(storage.submission_by_request(root_id, second, ctx.clone()).await).map(|s| s.id)),
                    &to_val(&Some(second_sub_id)),
                );
                expect.strict_equal(
                    &to_val(
                        &ok_val(
                            storage
                                .find_document(
                                    DocumentAddress {
                                        kind: first.to_string(),
                                        scope: DocumentScope::Session,
                                        key: None,
                                    },
                                    DocumentPoint::Current,
                                    ctx.clone(),
                                )
                                .await,
                        )
                        .map(|d| d.id),
                    ),
                    &to_val(&Some(first_kind_doc_id)),
                );
                expect.strict_equal(
                    &to_val(
                        &ok_val(
                            storage
                                .find_document(
                                    DocumentAddress {
                                        kind: second.to_string(),
                                        scope: DocumentScope::Session,
                                        key: None,
                                    },
                                    DocumentPoint::Current,
                                    ctx.clone(),
                                )
                                .await,
                        )
                        .map(|d| d.id),
                    ),
                    &to_val(&Some(second_kind_doc_id)),
                );
                expect.strict_equal(
                    &to_val(
                        &ok_val(
                            storage
                                .find_document(
                                    DocumentAddress {
                                        kind: "family".to_string(),
                                        scope: DocumentScope::Session,
                                        key: Some(first.to_string()),
                                    },
                                    DocumentPoint::Current,
                                    ctx.clone(),
                                )
                                .await,
                        )
                        .map(|d| d.id),
                    ),
                    &to_val(&Some(first_key_doc_id)),
                );
                expect.strict_equal(
                    &to_val(
                        &ok_val(
                            storage
                                .find_document(
                                    DocumentAddress {
                                        kind: "family".to_string(),
                                        scope: DocumentScope::Session,
                                        key: Some(second.to_string()),
                                    },
                                    DocumentPoint::Current,
                                    ctx.clone(),
                                )
                                .await,
                        )
                        .map(|d| d.id),
                    ),
                    &to_val(&Some(second_key_doc_id)),
                );
                expect.deep_equal(
                    &to_val(
                        &ok_val(
                            storage
                                .scan_documents(
                                    DocumentQuery {
                                        scope: DocumentScope::Session,
                                        at: DocumentPoint::Current,
                                        kind: Some(first.to_string()),
                                    },
                                    10,
                                    None,
                                    ctx,
                                )
                                .await,
                        )
                        .items
                        .iter()
                        .map(|d| d.id)
                        .collect::<Vec<_>>(),
                    ),
                    &to_val(&vec![first_kind_doc_id]),
                );
            },
        ),
        create_case(
            w,
            a,
            "keeps one global record ID namespace and rejects exhausted ID minting",
            |storage, expect| async move {
                let ctx = BACKGROUND_CONTEXT.clone();
                let root_id = create_root(storage.as_ref()).await;
                let explicit_entry_id = EntryId::new_unchecked(100);
                ok_val(
                    storage
                        .commit(
                            vec![StorageWrite::Entry {
                                value: make_entry(explicit_entry_id, root_id, "message"),
                            }],
                            ctx.clone(),
                        )
                        .await,
                );
                expect.strict_equal(&json!(ok_val(storage.mint_id().await)), &json!(101));
                expect.rejects_containing(
                    storage
                        .commit(
                            vec![StorageWrite::Task {
                                value: pending_task(
                                    TaskId::new_unchecked(explicit_entry_id.as_i64()),
                                    root_id,
                                    "ready",
                                ),
                            }],
                            ctx.clone(),
                        )
                        .await
                        .map(|_| ()),
                    &format!("ID {explicit_entry_id} already belongs to entry"),
                );

                ok_val(
                    storage
                        .commit(
                            vec![StorageWrite::Entry {
                                value: make_entry(
                                    EntryId::new_unchecked(pi_js::num::MAX_SAFE_INTEGER),
                                    root_id,
                                    "last-id",
                                ),
                            }],
                            ctx,
                        )
                        .await,
                );
                expect.rejects_containing(storage.mint_id().await.map(|_| ()), "ID space is exhausted");
                expect.rejects_containing(storage.mint_id().await.map(|_| ()), "ID space is exhausted");
            },
        ),
        create_case(
            w,
            a,
            "rejects every operation after close",
            |storage, expect| async move {
                let ctx = BACKGROUND_CONTEXT.clone();
                create_root(storage.as_ref()).await;
                ok_val(storage.close(ctx.clone()).await);
                expect.rejects_containing(
                    storage
                        .conversation(ROOT_CONVERSATION_ID, ctx.clone())
                        .await
                        .map(|_| ()),
                    "closed",
                );
                expect.rejects_containing(storage.commit(vec![], ctx).await.map(|_| ()), "closed");
                expect.rejects_containing(storage.mint_id().await.map(|_| ()), "closed");
            },
        ),
    ]
}

async fn scan_submission_ids(storage: &dyn Storage, query: SubmissionQuery) -> Vec<SubmissionId> {
    let ctx = BACKGROUND_CONTEXT.clone();
    let mut found = Vec::new();
    let mut cursor = None;
    loop {
        let page = ok_val(storage.scan_submissions(query, 1, cursor, ctx.clone()).await);
        for item in page.items {
            found.push(item.id);
        }
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    found
}

fn doc_summary(doc: &Option<crate::types::StoredDocument>) -> JsonValue {
    match doc {
        Some(d) => json!({
            "version": d.version,
            "value": d.value,
            "deltasSinceBase": d.deltas_since_base,
        }),
        None => JsonValue::Null,
    }
}

// Keep unused imports silenced cleanly if needed.
const _: fn(Seq) -> i64 = Seq::as_i64;
