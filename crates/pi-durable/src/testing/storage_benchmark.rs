//! Port of packages/durable/src/testing/storage-benchmark.ts

use std::future::Future;
use std::pin::Pin;
use std::sync::LazyLock;

use indexmap::IndexMap;
use pi_chord::context::BACKGROUND_CONTEXT;
use pi_chord::delta::{Op, OpSet, Seg, SetVerb};
use serde_json::{Value as JsonValue, json};

use crate::ids::Seq;
use crate::types::{
    ConversationId, ConversationParent, ConversationRecord, DocumentContent, DocumentCreate, DocumentFork,
    DocumentHistory, DocumentId, DocumentPoint, DocumentScope, EntryId, EntryQuery, EntryRecord, ErasedTaskRecord,
    JsonObject, ROOT_CONVERSATION_ID, Storage, StorageWrite, SubmissionId, SubmissionRecord, SubmissionState, TaskId,
    TaskOutcome, TaskQuery, TaskState, TaskStatus, WriteSubmissionState,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StorageBenchmarkScale {
    pub name: &'static str,
    pub entry_count: usize,
    pub task_count: usize,
    pub document_count: usize,
}

pub const STORAGE_MEMORY_SCALES: &[StorageBenchmarkScale] = &[
    StorageBenchmarkScale {
        name: "1k",
        entry_count: 1_000,
        task_count: 200,
        document_count: 200,
    },
    StorageBenchmarkScale {
        name: "10k",
        entry_count: 10_000,
        task_count: 2_000,
        document_count: 2_000,
    },
];

pub const TIMING_SCALE: StorageBenchmarkScale = StorageBenchmarkScale {
    name: "timing",
    entry_count: 1_000,
    task_count: 300,
    document_count: 300,
};

const REPLAY_TAILS: [usize; 4] = [0, 16, 128, 1_024];
const HISTORY_SEGMENT_LENGTH: usize = 128;
const FORK_DEPTH: usize = 8;
const ENTRIES_PER_FORK: usize = 32;
const BATCH_SIZE: usize = 100;

pub fn storage_benchmark_primary_record_count(scale: StorageBenchmarkScale) -> usize {
    1 + scale.entry_count
        + scale.task_count
        + scale.document_count
        + REPLAY_TAILS.len()
        + 1
        + FORK_DEPTH * (1 + ENTRIES_PER_FORK)
}

#[derive(Clone, Debug)]
pub struct StorageBenchmarkDataset {
    pub first_entry_id: EntryId,
    pub filtered_task_count: usize,
    pub exact_document_id: DocumentId,
    pub exact_document_key: String,
    pub replay_document_ids: IndexMap<usize, DocumentId>,
    pub historical_document_id: DocumentId,
    pub ancient_at: Seq,
    pub recent_at: Seq,
    pub deepest_conversation_id: ConversationId,
    pub ancestor_head_entry_id: EntryId,
}

fn build_benchmark_task(id: TaskId, index: usize) -> ErasedTaskRecord {
    let kind = if index % 4 == 0 {
        "benchmark.filtered"
    } else {
        "benchmark.other"
    }
    .to_string();
    let background = index % 5 == 0;
    let abort_requested = index % 7 == 0;
    let state = match index % 3 {
        0 => TaskState::Pending {
            checkpoint: json!({ "index": index, "payload": "x".repeat(64) }),
        },
        1 => TaskState::Running {
            checkpoint: json!({ "index": index, "payload": "x".repeat(64) }),
        },
        _ => TaskState::Terminal {
            outcome: TaskOutcome::Completed {
                result: json!({ "index": index }),
            },
        },
    };
    ErasedTaskRecord {
        id,
        conversation_id: ROOT_CONVERSATION_ID,
        kind,
        version: 1,
        input: json!({ "index": index }),
        owner: None,
        background,
        abort_requested,
        state,
        memos: None,
    }
}

fn json_obj(val: JsonValue) -> pi_js::Result<JsonObject> {
    match val {
        JsonValue::Object(m) => Ok(m),
        _ => Err(pi_js::Error::msg("Expected JSON object")),
    }
}

/// Seed deterministic representative data through only the public [`Storage`] contract.
pub async fn seed_storage_benchmark(
    storage: &dyn Storage,
    scale: Option<StorageBenchmarkScale>,
) -> pi_js::Result<StorageBenchmarkDataset> {
    let scale = scale.unwrap_or(TIMING_SCALE);
    let ctx = BACKGROUND_CONTEXT.clone();

    storage
        .commit(
            vec![StorageWrite::Conversation {
                value: ConversationRecord {
                    id: ROOT_CONVERSATION_ID,
                    parent: None,
                    owner: None,
                },
            }],
            ctx.clone(),
        )
        .await?;

    let mut first_entry_id: Option<EntryId> = None;
    for start in (0..scale.entry_count).step_by(BATCH_SIZE) {
        let end = (start + BATCH_SIZE).min(scale.entry_count);
        let mut writes = Vec::with_capacity(end - start);
        for index in start..end {
            let id = EntryId::new_unchecked(storage.mint_id().await?);
            if index == 0 {
                first_entry_id = Some(id);
            }
            writes.push(StorageWrite::Entry {
                value: EntryRecord {
                    id,
                    conversation_id: ROOT_CONVERSATION_ID,
                    kind: "benchmark.entry".to_string(),
                    model: None,
                    data: Some(json!({
                        "index": index,
                        "text": format!("entry-{index}-{}", "x".repeat(96)),
                    })),
                    head: if index == 0 { Some(id) } else { None },
                    edits: None,
                    by_task_id: None,
                },
            });
        }
        storage.commit(writes, ctx.clone()).await?;
    }

    for start in (0..scale.task_count).step_by(BATCH_SIZE) {
        let end = (start + BATCH_SIZE).min(scale.task_count);
        let mut writes = Vec::with_capacity(end - start);
        for index in start..end {
            let id = TaskId::new_unchecked(storage.mint_id().await?);
            writes.push(StorageWrite::Task {
                value: build_benchmark_task(id, index),
            });
        }
        storage.commit(writes, ctx.clone()).await?;
    }

    let mut exact_document_id: Option<DocumentId> = None;
    for start in (0..scale.document_count).step_by(BATCH_SIZE) {
        let end = (start + BATCH_SIZE).min(scale.document_count);
        let mut writes = Vec::with_capacity(end - start);
        for index in start..end {
            let id = DocumentId::new_unchecked(storage.mint_id().await?);
            exact_document_id = Some(id);
            writes.push(StorageWrite::DocumentCreate {
                record: DocumentCreate {
                    id,
                    kind: "benchmark.family".to_string(),
                    key: Some(format!("key-{index}")),
                    scope: DocumentScope::Session,
                    history: None,
                    fork: None,
                },
                version: 1,
                value: json_obj(json!({ "index": index, "text": "x".repeat(128) }))?,
            });
        }
        storage.commit(writes, ctx.clone()).await?;
    }

    let mut replay_entries = Vec::with_capacity(REPLAY_TAILS.len());
    for tail in REPLAY_TAILS {
        let id = DocumentId::new_unchecked(storage.mint_id().await?);
        replay_entries.push((tail, id));
    }
    let replay_creates: Vec<StorageWrite> = replay_entries
        .iter()
        .map(|&(tail, id)| -> pi_js::Result<StorageWrite> {
            Ok(StorageWrite::DocumentCreate {
                record: DocumentCreate {
                    id,
                    kind: "benchmark.replay".to_string(),
                    key: Some(tail.to_string()),
                    scope: DocumentScope::Conversation {
                        conversation_id: ROOT_CONVERSATION_ID,
                    },
                    history: Some(DocumentHistory::Rewindable),
                    fork: Some(DocumentFork::AsOf),
                },
                version: 1,
                value: json_obj(json!({ "count": 0, "text": "x".repeat(64) }))?,
            })
        })
        .collect::<pi_js::Result<Vec<_>>>()?;
    storage.commit(replay_creates, ctx.clone()).await?;

    let max_tail = *REPLAY_TAILS.last().unwrap_or(&0);
    for count in 1..=max_tail {
        let writes: Vec<StorageWrite> = replay_entries
            .iter()
            .filter(|&&(tail, _)| count <= tail)
            .map(|&(_, id)| StorageWrite::DocumentChange {
                id,
                content: DocumentContent::Delta {
                    version: 1,
                    ops: vec![Op::Set(OpSet(
                        SetVerb::S,
                        vec![Seg::String("count".to_string())],
                        json!(count),
                    ))],
                },
            })
            .collect();
        storage.commit(writes, ctx.clone()).await?;
    }

    let historical_document_id = DocumentId::new_unchecked(storage.mint_id().await?);
    let historical_record = DocumentCreate {
        id: historical_document_id,
        kind: "benchmark.history".to_string(),
        key: None,
        scope: DocumentScope::Conversation {
            conversation_id: ROOT_CONVERSATION_ID,
        },
        history: Some(DocumentHistory::Rewindable),
        fork: Some(DocumentFork::AsOf),
    };
    storage
        .commit(
            vec![StorageWrite::DocumentCreate {
                record: historical_record,
                version: 1,
                value: json_obj(json!({ "count": 0 }))?,
            }],
            ctx.clone(),
        )
        .await?;

    let mut ancient_at: Option<Seq> = None;
    for count in 1..=HISTORY_SEGMENT_LENGTH {
        let seq = storage
            .commit(
                vec![StorageWrite::DocumentChange {
                    id: historical_document_id,
                    content: DocumentContent::Delta {
                        version: 1,
                        ops: vec![Op::Set(OpSet(
                            SetVerb::S,
                            vec![Seg::String("count".to_string())],
                            json!(count),
                        ))],
                    },
                }],
                ctx.clone(),
            )
            .await?;
        ancient_at = Some(seq);
    }
    storage
        .commit(
            vec![StorageWrite::DocumentChange {
                id: historical_document_id,
                content: DocumentContent::Base {
                    version: 1,
                    value: json_obj(json!({ "count": HISTORY_SEGMENT_LENGTH }))?,
                },
            }],
            ctx.clone(),
        )
        .await?;
    let ancient_at = ancient_at.ok_or_else(|| pi_js::Error::msg("Benchmark history seed produced no commits"))?;

    let mut recent_at = ancient_at;
    for count in (HISTORY_SEGMENT_LENGTH + 1)..=(HISTORY_SEGMENT_LENGTH * 2) {
        recent_at = storage
            .commit(
                vec![StorageWrite::DocumentChange {
                    id: historical_document_id,
                    content: DocumentContent::Delta {
                        version: 1,
                        ops: vec![Op::Set(OpSet(
                            SetVerb::S,
                            vec![Seg::String("count".to_string())],
                            json!(count),
                        ))],
                    },
                }],
                ctx.clone(),
            )
            .await?;
    }

    let first_entry_id = first_entry_id.ok_or_else(|| pi_js::Error::msg("Benchmark scale must create entries"))?;
    let mut parent_conversation_id = ROOT_CONVERSATION_ID;
    let mut parent_at = first_entry_id;
    let mut deepest_conversation_id = ROOT_CONVERSATION_ID;
    for depth in 0..FORK_DEPTH {
        let conversation_id = ConversationId::new_unchecked(storage.mint_id().await?);
        storage
            .commit(
                vec![StorageWrite::Conversation {
                    value: ConversationRecord {
                        id: conversation_id,
                        parent: Some(ConversationParent {
                            conversation_id: parent_conversation_id,
                            at: parent_at,
                        }),
                        owner: None,
                    },
                }],
                ctx.clone(),
            )
            .await?;
        let mut entry_writes = Vec::with_capacity(ENTRIES_PER_FORK);
        let mut last_id = None;
        for index in 0..ENTRIES_PER_FORK {
            let id = EntryId::new_unchecked(storage.mint_id().await?);
            last_id = Some(id);
            entry_writes.push(StorageWrite::Entry {
                value: EntryRecord {
                    id,
                    conversation_id,
                    kind: "benchmark.fork".to_string(),
                    model: None,
                    data: Some(json!({ "depth": depth, "index": index })),
                    head: None,
                    edits: None,
                    by_task_id: None,
                },
            });
        }
        storage.commit(entry_writes, ctx.clone()).await?;
        parent_conversation_id = conversation_id;
        parent_at = last_id.ok_or_else(|| pi_js::Error::msg("Missing fork entries"))?;
        deepest_conversation_id = conversation_id;
    }

    let exact_document_id =
        exact_document_id.ok_or_else(|| pi_js::Error::msg("Benchmark scale must create documents"))?;
    let mut replay_document_ids = IndexMap::new();
    for (tail, id) in replay_entries {
        replay_document_ids.insert(tail, id);
    }

    Ok(StorageBenchmarkDataset {
        first_entry_id,
        filtered_task_count: 50.min(scale.task_count.div_ceil(60)),
        exact_document_id,
        exact_document_key: format!("key-{}", scale.document_count - 1),
        replay_document_ids,
        historical_document_id,
        ancient_at,
        recent_at,
        deepest_conversation_id,
        ancestor_head_entry_id: first_entry_id,
    })
}

pub type BenchmarkReadRun = for<'a> fn(
    &'a dyn Storage,
    &'a StorageBenchmarkDataset,
) -> Pin<Box<dyn Future<Output = pi_js::Result<i64>> + Send + 'a>>;
pub type BenchmarkReadExpected = fn(&StorageBenchmarkDataset) -> i64;

#[derive(Clone, Copy)]
pub struct StorageReadBenchmark {
    pub name: &'static str,
    pub run: BenchmarkReadRun,
    pub expected: BenchmarkReadExpected,
}

fn expect_tail_0(_: &StorageBenchmarkDataset) -> i64 {
    0
}
fn expect_tail_16(_: &StorageBenchmarkDataset) -> i64 {
    16
}
fn expect_tail_128(_: &StorageBenchmarkDataset) -> i64 {
    128
}
fn expect_tail_1024(_: &StorageBenchmarkDataset) -> i64 {
    1_024
}

fn read_replay_tail<'a>(
    storage: &'a dyn Storage,
    dataset: &'a StorageBenchmarkDataset,
    tail: usize,
) -> Pin<Box<dyn Future<Output = pi_js::Result<i64>> + Send + 'a>> {
    Box::pin(async move {
        let Some(&doc_id) = dataset.replay_document_ids.get(&tail) else {
            return Ok(-1);
        };
        let doc = storage
            .document(doc_id, DocumentPoint::Current, BACKGROUND_CONTEXT.clone())
            .await?;
        Ok(doc
            .and_then(|d| d.value.get("count").and_then(JsonValue::as_i64))
            .unwrap_or(-1))
    })
}

pub static STORAGE_READ_BENCHMARKS: LazyLock<Vec<StorageReadBenchmark>> = LazyLock::new(|| {
    vec![
        StorageReadBenchmark {
            name: "exact entry lookup",
            run: |storage, dataset| {
                Box::pin(async move {
                    Ok(storage
                        .entry(dataset.first_entry_id, BACKGROUND_CONTEXT.clone())
                        .await?
                        .map_or(-1, |found| found.entry.id.as_i64()))
                })
            },
            expected: |dataset| dataset.first_entry_id.as_i64(),
        },
        StorageReadBenchmark {
            name: "entry page scan (100)",
            run: |storage, _| {
                Box::pin(async move {
                    let page = storage
                        .scan_entries(
                            EntryQuery {
                                conversation_id: ROOT_CONVERSATION_ID,
                                min_entry_id: None,
                                max_entry_id: None,
                            },
                            100,
                            None,
                            BACKGROUND_CONTEXT.clone(),
                        )
                        .await?;
                    Ok(page.items.len() as i64)
                })
            },
            expected: |_| 100,
        },
        StorageReadBenchmark {
            name: "filtered task scan (50)",
            run: |storage, _| {
                Box::pin(async move {
                    let page = storage
                        .scan_tasks(
                            TaskQuery {
                                conversation_id: None,
                                kind: Some("benchmark.filtered".to_string()),
                                status: Some(TaskStatus::Pending),
                                abort_requested: None,
                                background: Some(true),
                            },
                            50,
                            None,
                            BACKGROUND_CONTEXT.clone(),
                        )
                        .await?;
                    Ok(page.items.len() as i64)
                })
            },
            expected: |dataset| dataset.filtered_task_count as i64,
        },
        StorageReadBenchmark {
            name: "exact document address among many",
            run: |storage, dataset| {
                Box::pin(async move {
                    let found = storage
                        .find_document(
                            crate::types::DocumentAddress {
                                kind: "benchmark.family".to_string(),
                                scope: DocumentScope::Session,
                                key: Some(dataset.exact_document_key.clone()),
                            },
                            DocumentPoint::Current,
                            BACKGROUND_CONTEXT.clone(),
                        )
                        .await?;
                    Ok(found.map_or(-1, |rec| rec.id.as_i64()))
                })
            },
            expected: |dataset| dataset.exact_document_id.as_i64(),
        },
        StorageReadBenchmark {
            name: "document replay tail (0)",
            run: |storage, dataset| read_replay_tail(storage, dataset, 0),
            expected: expect_tail_0,
        },
        StorageReadBenchmark {
            name: "document replay tail (16)",
            run: |storage, dataset| read_replay_tail(storage, dataset, 16),
            expected: expect_tail_16,
        },
        StorageReadBenchmark {
            name: "document replay tail (128)",
            run: |storage, dataset| read_replay_tail(storage, dataset, 128),
            expected: expect_tail_128,
        },
        StorageReadBenchmark {
            name: "document replay tail (1024)",
            run: |storage, dataset| read_replay_tail(storage, dataset, 1_024),
            expected: expect_tail_1024,
        },
        StorageReadBenchmark {
            name: "ancient historical read before newer base",
            run: |storage, dataset| {
                Box::pin(async move {
                    let doc = storage
                        .document(
                            dataset.historical_document_id,
                            DocumentPoint::At(dataset.ancient_at),
                            BACKGROUND_CONTEXT.clone(),
                        )
                        .await?;
                    Ok(doc
                        .and_then(|d| d.value.get("count").and_then(JsonValue::as_i64))
                        .unwrap_or(-1))
                })
            },
            expected: |_| HISTORY_SEGMENT_LENGTH as i64,
        },
        StorageReadBenchmark {
            name: "recent historical read after newer base",
            run: |storage, dataset| {
                Box::pin(async move {
                    let doc = storage
                        .document(
                            dataset.historical_document_id,
                            DocumentPoint::At(dataset.recent_at),
                            BACKGROUND_CONTEXT.clone(),
                        )
                        .await?;
                    Ok(doc
                        .and_then(|d| d.value.get("count").and_then(JsonValue::as_i64))
                        .unwrap_or(-1))
                })
            },
            expected: |_| (HISTORY_SEGMENT_LENGTH * 2) as i64,
        },
        StorageReadBenchmark {
            name: "fork-depth history scan (100)",
            run: |storage, dataset| {
                Box::pin(async move {
                    let page = storage
                        .scan_entries(
                            EntryQuery {
                                conversation_id: dataset.deepest_conversation_id,
                                min_entry_id: None,
                                max_entry_id: None,
                            },
                            100,
                            None,
                            BACKGROUND_CONTEXT.clone(),
                        )
                        .await?;
                    Ok(page.items.len() as i64)
                })
            },
            expected: |_| 100,
        },
        StorageReadBenchmark {
            name: "fork-depth head lookup",
            run: |storage, dataset| {
                Box::pin(async move {
                    let marker = storage
                        .find_latest_head_marker(dataset.deepest_conversation_id, None, BACKGROUND_CONTEXT.clone())
                        .await?;
                    Ok(marker.map_or(-1, |rec| rec.id.as_i64()))
                })
            },
            expected: |dataset| dataset.ancestor_head_entry_id.as_i64(),
        },
    ]
});

pub type BenchmarkWriteRun =
    for<'a> fn(&'a dyn Storage) -> Pin<Box<dyn Future<Output = pi_js::Result<usize>> + Send + 'a>>;

#[derive(Clone, Copy)]
pub struct StorageWriteBenchmark {
    pub name: &'static str,
    pub expected: usize,
    pub run: BenchmarkWriteRun,
}

/// Seed the common state expected by every write benchmark sample.
pub async fn seed_storage_write_benchmark(storage: &dyn Storage) -> pi_js::Result<()> {
    let ctx = BACKGROUND_CONTEXT.clone();
    storage
        .commit(
            vec![StorageWrite::Conversation {
                value: ConversationRecord {
                    id: ROOT_CONVERSATION_ID,
                    parent: None,
                    owner: None,
                },
            }],
            ctx.clone(),
        )
        .await?;
    let mut writes = Vec::with_capacity(100);
    for index in 0..100 {
        let id = EntryId::new_unchecked(storage.mint_id().await?);
        writes.push(StorageWrite::Entry {
            value: EntryRecord {
                id,
                conversation_id: ROOT_CONVERSATION_ID,
                kind: "benchmark.baseline".to_string(),
                model: None,
                data: Some(json!({ "index": index })),
                head: None,
                edits: None,
                by_task_id: None,
            },
        });
    }
    storage.commit(writes, ctx).await?;
    Ok(())
}

pub static STORAGE_WRITE_BENCHMARKS: LazyLock<Vec<StorageWriteBenchmark>> = LazyLock::new(|| {
    vec![
        StorageWriteBenchmark {
            name: "commit one entry",
            expected: 1,
            run: |storage| {
                Box::pin(async move {
                    let id = EntryId::new_unchecked(storage.mint_id().await?);
                    storage
                        .commit(
                            vec![StorageWrite::Entry {
                                value: EntryRecord {
                                    id,
                                    conversation_id: ROOT_CONVERSATION_ID,
                                    kind: "benchmark.write".to_string(),
                                    model: None,
                                    data: Some(json!({ "text": "x".repeat(128) })),
                                    head: None,
                                    edits: None,
                                    by_task_id: None,
                                },
                            }],
                            BACKGROUND_CONTEXT.clone(),
                        )
                        .await?;
                    Ok(1)
                })
            },
        },
        StorageWriteBenchmark {
            name: "commit 100 entries",
            expected: 100,
            run: |storage| {
                Box::pin(async move {
                    let mut writes = Vec::with_capacity(100);
                    for index in 0..100 {
                        let id = EntryId::new_unchecked(storage.mint_id().await?);
                        writes.push(StorageWrite::Entry {
                            value: EntryRecord {
                                id,
                                conversation_id: ROOT_CONVERSATION_ID,
                                kind: "benchmark.write".to_string(),
                                model: None,
                                data: Some(json!({ "index": index, "text": "x".repeat(128) })),
                                head: None,
                                edits: None,
                                by_task_id: None,
                            },
                        });
                    }
                    let count = writes.len();
                    storage.commit(writes, BACKGROUND_CONTEXT.clone()).await?;
                    Ok(count)
                })
            },
        },
        StorageWriteBenchmark {
            name: "commit mixed entry/task/submission/document",
            expected: 4,
            run: |storage| {
                Box::pin(async move {
                    let entry_id = EntryId::new_unchecked(storage.mint_id().await?);
                    let task_id = TaskId::new_unchecked(storage.mint_id().await?);
                    let submission_id = SubmissionId::new_unchecked(storage.mint_id().await?);
                    let document_id = DocumentId::new_unchecked(storage.mint_id().await?);
                    let writes = vec![
                        StorageWrite::Entry {
                            value: EntryRecord {
                                id: entry_id,
                                conversation_id: ROOT_CONVERSATION_ID,
                                kind: "benchmark.mixed".to_string(),
                                model: None,
                                data: None,
                                head: None,
                                edits: None,
                                by_task_id: None,
                            },
                        },
                        StorageWrite::Task {
                            value: build_benchmark_task(task_id, task_id.as_i64() as usize),
                        },
                        StorageWrite::Submission {
                            value: SubmissionRecord {
                                id: submission_id,
                                conversation_id: ROOT_CONVERSATION_ID,
                                request_id: Some(format!("benchmark-{submission_id}")),
                                state: SubmissionState::Write(WriteSubmissionState::Done { entry: entry_id }),
                            },
                        },
                        StorageWrite::DocumentCreate {
                            record: DocumentCreate {
                                id: document_id,
                                kind: "benchmark.mixed".to_string(),
                                key: Some(document_id.to_string()),
                                scope: DocumentScope::Session,
                                history: None,
                                fork: None,
                            },
                            version: 1,
                            value: json_obj(json!({ "entryId": entry_id, "taskId": task_id }))?,
                        },
                    ];
                    let count = writes.len();
                    storage.commit(writes, BACKGROUND_CONTEXT.clone()).await?;
                    Ok(count)
                })
            },
        },
    ]
});
