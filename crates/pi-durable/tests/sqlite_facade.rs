//! Port of packages/durable/test/sqlite-facade.test.ts

use std::any::Any;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_chord::Context;
use pi_chord::context::BACKGROUND_CONTEXT;

fn background_context() -> Context {
    BACKGROUND_CONTEXT.clone()
}
use pi_durable::errors::StorageRejected;
use pi_durable::ids::{DocumentId, EntryId, id_from_number, seq_from_number};
use pi_durable::storage::sqlite::node::{NodeSqliteDatabase, NodeSqliteStorageOptions, open_node_sqlite_database};
use pi_durable::storage::sqlite::{
    BoxedSqliteTxCallback, SqliteDatabase, SqliteDatabaseExt, SqliteExecutor, SqliteRow, SqliteRowExt, SqliteStorage,
    SqliteValue,
};
use pi_durable::types::{
    ConversationRecord, DocumentContent, DocumentCreate, DocumentPoint, DocumentScope, EntryRecord, JsonObject,
    ROOT_CONVERSATION_ID, Storage, StorageWrite,
};
use serde_json::{Map, json};
use tokio::sync::{Notify, oneshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettlementMode {
    Immediate,
    Delay,
    Reject,
}

struct ControlledSettlementDatabase {
    delegate: Arc<NodeSqliteDatabase>,
    mode: Mutex<SettlementMode>,
    pending: Mutex<Option<oneshot::Sender<()>>>,
}

impl ControlledSettlementDatabase {
    fn new(delegate: Arc<NodeSqliteDatabase>) -> Self {
        Self {
            delegate,
            mode: Mutex::new(SettlementMode::Immediate),
            pending: Mutex::new(None),
        }
    }

    fn control_next_settlement(&self, mode: SettlementMode) {
        assert!(
            self.pending.lock().expect("pending lock").is_none(),
            "A settlement is already pending"
        );
        *self.mode.lock().expect("mode lock") = mode;
    }

    fn settle(&self) {
        let tx = self
            .pending
            .lock()
            .expect("pending lock")
            .take()
            .expect("No settlement is pending");
        let _ = tx.send(());
    }
}

#[async_trait]
impl SqliteExecutor for ControlledSettlementDatabase {
    async fn exec(&self, sql: &str) -> pi_js::Result<()> {
        self.delegate.exec(sql).await
    }

    async fn run(&self, sql: &str, params: &[SqliteValue]) -> pi_js::Result<()> {
        self.delegate.run(sql, params).await
    }

    async fn get(&self, sql: &str, params: &[SqliteValue]) -> pi_js::Result<Option<SqliteRow>> {
        self.delegate.get(sql, params).await
    }

    async fn all(&self, sql: &str, params: &[SqliteValue]) -> pi_js::Result<Vec<SqliteRow>> {
        self.delegate.all(sql, params).await
    }
}

#[async_trait]
impl SqliteDatabase for ControlledSettlementDatabase {
    async fn transaction_boxed<'a>(
        &'a self,
        callback: BoxedSqliteTxCallback<'a>,
    ) -> pi_js::Result<Box<dyn Any + Send>> {
        let mode = {
            let mut guard = self.mode.lock().expect("mode lock");
            let current = *guard;
            *guard = SettlementMode::Immediate;
            current
        };
        if mode == SettlementMode::Immediate {
            return self.delegate.transaction_boxed(callback).await;
        }
        let wrapped: BoxedSqliteTxCallback<'a> = Box::new(move |tx| {
            Box::pin(async move {
                let value = callback(tx).await?;
                if mode == SettlementMode::Reject {
                    return Err(pi_js::Error::msg("controlled settlement rejection"));
                }
                Ok(value)
            })
        });
        let outcome = self.delegate.transaction_boxed(wrapped).await;
        let (tx, rx) = oneshot::channel();
        *self.pending.lock().expect("pending lock") = Some(tx);
        let _ = rx.await;
        outcome
    }

    async fn close(&self) -> pi_js::Result<()> {
        self.delegate.close().await
    }
}

fn memory_db() -> Arc<NodeSqliteDatabase> {
    let conn = rusqlite::Connection::open_in_memory().expect("open in-memory sqlite");
    Arc::new(NodeSqliteDatabase::new(conn))
}

#[tokio::test]
async fn prepares_each_storage_statement_once_and_reuses_it_across_transactions() {
    let context = background_context();
    let db = memory_db();
    let storage = SqliteStorage::open(db).await.expect("open SqliteStorage");
    storage
        .commit(
            vec![StorageWrite::Conversation {
                value: ConversationRecord {
                    id: ROOT_CONVERSATION_ID,
                    parent: None,
                    owner: None,
                },
            }],
            context.clone(),
        )
        .await
        .expect("commit root");

    let batch: Vec<StorageWrite> = (0..100)
        .map(|index| StorageWrite::Entry {
            value: EntryRecord {
                id: EntryId::new_unchecked(index + 2),
                conversation_id: ROOT_CONVERSATION_ID,
                kind: "cached".to_string(),
                model: None,
                data: None,
                head: None,
                edits: None,
                by_task_id: None,
            },
        })
        .collect();
    storage.commit(batch, context.clone()).await.expect("commit 100");
    storage
        .commit(
            vec![StorageWrite::Entry {
                value: EntryRecord {
                    id: EntryId::new_unchecked(102),
                    conversation_id: ROOT_CONVERSATION_ID,
                    kind: "cached-again".to_string(),
                    model: None,
                    data: None,
                    head: None,
                    edits: None,
                    by_task_id: None,
                },
            }],
            context.clone(),
        )
        .await
        .expect("commit 102");

    assert_eq!(
        storage
            .entry(EntryId::new_unchecked(2), context.clone())
            .await
            .expect("entry 2")
            .expect("present")
            .entry
            .kind,
        "cached"
    );
    assert_eq!(
        storage
            .entry(EntryId::new_unchecked(102), context.clone())
            .await
            .expect("entry 102")
            .expect("present")
            .entry
            .kind,
        "cached-again"
    );

    let duplicate_err = storage
        .commit(
            vec![StorageWrite::Conversation {
                value: ConversationRecord {
                    id: ROOT_CONVERSATION_ID,
                    parent: None,
                    owner: None,
                },
            }],
            context.clone(),
        )
        .await
        .expect_err("expected duplicate ID error");
    assert!(
        duplicate_err
            .to_string()
            .contains("ID 1 already belongs to conversation"),
        "unexpected error: {duplicate_err}"
    );
    assert_eq!(
        storage
            .entry(EntryId::new_unchecked(2), context.clone())
            .await
            .expect("entry 2 after err")
            .expect("present")
            .entry
            .kind,
        "cached"
    );
    storage.close(context).await.expect("close");
}

#[tokio::test]
async fn commits_work_done_through_transaction_handle_and_closes_idempotently() {
    let database = open_node_sqlite_database(":memory:", NodeSqliteStorageOptions::default())
        .await
        .expect("open db");
    database
        .transaction(|tx| async move {
            tx.exec("CREATE TABLE async_probe (value INTEGER)").await?;
            tx.run("INSERT INTO async_probe (value) VALUES (?)", &[SqliteValue::Integer(1)])
                .await?;
            Ok(())
        })
        .await
        .expect("tx");
    let row = database
        .get("SELECT value FROM async_probe", &[])
        .await
        .expect("get")
        .expect("row");
    assert_eq!(row.get_i64("value").expect("value"), 1);
    database.close().await.expect("first close");
    database.close().await.expect("second close");
}

#[tokio::test]
async fn serializes_concurrent_transactions() {
    let database = open_node_sqlite_database(":memory:", NodeSqliteStorageOptions::default())
        .await
        .expect("open db");
    database
        .exec("CREATE TABLE transaction_queue (value INTEGER)")
        .await
        .expect("create table");

    let first_started = Arc::new(Notify::new());
    let first_started_signal = Arc::clone(&first_started);
    let (gate_tx, gate_rx) = oneshot::channel::<()>();

    let db1 = Arc::clone(&database);
    let first = tokio::spawn(async move {
        db1.transaction(move |tx| async move {
            tx.exec("INSERT INTO transaction_queue (value) VALUES (1)").await?;
            first_started_signal.notify_one();
            let _ = gate_rx.await;
            Ok(())
        })
        .await
    });
    first_started.notified().await;

    let second_started = Arc::new(AtomicBool::new(false));
    let second_started_flag = Arc::clone(&second_started);
    let db2 = Arc::clone(&database);
    let second = tokio::spawn(async move {
        db2.transaction(move |tx| async move {
            second_started_flag.store(true, Ordering::SeqCst);
            tx.exec("INSERT INTO transaction_queue (value) VALUES (2)").await?;
            Ok(())
        })
        .await
    });

    tokio::task::yield_now().await;
    assert!(!second_started.load(Ordering::SeqCst));

    let _ = gate_tx.send(());
    first.await.expect("join first").expect("first ok");
    second.await.expect("join second").expect("second ok");

    let rows = database
        .all("SELECT value FROM transaction_queue ORDER BY value", &[])
        .await
        .expect("select all");
    let values: Vec<i64> = rows.iter().map(|row| row.get_i64("value").expect("val")).collect();
    assert_eq!(values, vec![1, 2]);
    database.close().await.expect("close");
}

#[tokio::test]
async fn runs_operations_in_call_order_whether_they_start_immediately_or_wait() {
    let context = background_context();
    let database = open_node_sqlite_database(":memory:", NodeSqliteStorageOptions::default())
        .await
        .expect("open db");
    database
        .exec("CREATE TABLE call_order (value INTEGER)")
        .await
        .expect("create table");

    let tx_fut = database.transaction(|handle| async move {
        handle
            .run("INSERT INTO call_order (value) VALUES (?)", &[SqliteValue::Integer(1)])
            .await?;
        tokio::task::yield_now().await;
        Err::<(), _>(pi_js::Error::msg("roll back"))
    });
    let before_write = database.all("SELECT value FROM call_order ORDER BY value", &[]);
    let write = database.run("INSERT INTO call_order (value) VALUES (?)", &[SqliteValue::Integer(2)]);
    let after_write = database.all("SELECT value FROM call_order ORDER BY value", &[]);

    let (tx_res, before_res, write_res, after_res) = tokio::join!(tx_fut, before_write, write, after_write);
    assert!(tx_res.expect_err("expected rollback").to_string().contains("roll back"));
    write_res.expect("write ok");
    assert!(before_res.expect("before ok").is_empty());
    let after_values: Vec<i64> = after_res
        .expect("after ok")
        .iter()
        .map(|r| r.get_i64("value").expect("value"))
        .collect();
    assert_eq!(after_values, vec![2]);

    let storage = SqliteStorage::open(database).await.expect("open storage");
    let commit_fut = storage.commit(
        vec![StorageWrite::Conversation {
            value: ConversationRecord {
                id: ROOT_CONVERSATION_ID,
                parent: None,
                owner: None,
            },
        }],
        context.clone(),
    );
    let read_fut = storage.conversation(ROOT_CONVERSATION_ID, context.clone());
    let (commit_res, read_res) = tokio::join!(commit_fut, read_fut);
    commit_res.expect("commit root");
    assert_eq!(
        read_res.expect("read root"),
        Some(ConversationRecord {
            id: ROOT_CONVERSATION_ID,
            parent: None,
            owner: None,
        })
    );
    storage.close(context).await.expect("close");
}

#[tokio::test]
async fn queues_ordinary_operations_behind_an_active_transaction() {
    let database = open_node_sqlite_database(":memory:", NodeSqliteStorageOptions::default())
        .await
        .expect("open db");
    database
        .exec("CREATE TABLE operation_queue (value INTEGER)")
        .await
        .expect("create");

    let tx_started = Arc::new(Notify::new());
    let tx_started_signal = Arc::clone(&tx_started);
    let (gate_tx, gate_rx) = oneshot::channel::<()>();

    let db1 = Arc::clone(&database);
    let pending = tokio::spawn(async move {
        db1.transaction(move |tx| async move {
            tx.exec("INSERT INTO operation_queue (value) VALUES (1)").await?;
            tx_started_signal.notify_one();
            let _ = gate_rx.await;
            Ok(())
        })
        .await
    });
    tx_started.notified().await;

    let write_settled = Arc::new(AtomicBool::new(false));
    let read_settled = Arc::new(AtomicBool::new(false));
    let db_w = Arc::clone(&database);
    let w_flag = Arc::clone(&write_settled);
    let write = tokio::spawn(async move {
        let res = db_w.exec("INSERT INTO operation_queue (value) VALUES (2)").await;
        w_flag.store(true, Ordering::SeqCst);
        res
    });
    let db_r = Arc::clone(&database);
    let r_flag = Arc::clone(&read_settled);
    let read = tokio::spawn(async move {
        let res = db_r.all("SELECT value FROM operation_queue ORDER BY value", &[]).await;
        r_flag.store(true, Ordering::SeqCst);
        res
    });

    tokio::task::yield_now().await;
    assert!(!write_settled.load(Ordering::SeqCst));
    assert!(!read_settled.load(Ordering::SeqCst));

    let _ = gate_tx.send(());
    pending.await.expect("join tx").expect("tx ok");
    write.await.expect("join write").expect("write ok");
    let rows = read.await.expect("join read").expect("read ok");
    let values: Vec<i64> = rows.iter().map(|row| row.get_i64("value").expect("value")).collect();
    assert_eq!(values, vec![1, 2]);
    database.close().await.expect("close");
}

#[tokio::test]
async fn queues_database_calls_made_synchronously_by_a_transaction() {
    let database = open_node_sqlite_database(":memory:", NodeSqliteStorageOptions::default())
        .await
        .expect("open db");
    database
        .exec("CREATE TABLE barrier_probe (value INTEGER)")
        .await
        .expect("create");

    let db_for_tx = Arc::clone(&database);
    let outside_holder = Arc::new(Mutex::new(None));
    let outside_clone = Arc::clone(&outside_holder);

    let err = database
        .transaction(move |handle| async move {
            let outside_fut = tokio::spawn(async move {
                db_for_tx
                    .run(
                        "INSERT INTO barrier_probe (value) VALUES (?)",
                        &[SqliteValue::Integer(2)],
                    )
                    .await
            });
            *outside_clone.lock().expect("lock") = Some(outside_fut);
            handle
                .run(
                    "INSERT INTO barrier_probe (value) VALUES (?)",
                    &[SqliteValue::Integer(1)],
                )
                .await?;
            tokio::task::yield_now().await;
            Err::<(), _>(pi_js::Error::msg("roll back"))
        })
        .await
        .expect_err("expected rollback");
    assert!(err.to_string().contains("roll back"));

    let outside_handle = outside_holder.lock().expect("lock").take().expect("outside spawned");
    outside_handle.await.expect("join").expect("outside write ok");

    let rows = database
        .all("SELECT value FROM barrier_probe", &[])
        .await
        .expect("select");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_i64("value").expect("value"), 2);
    database.close().await.expect("close");
}

#[tokio::test]
async fn lets_admitted_multi_query_reads_finish_before_storage_closes() {
    let context = background_context();
    let db = open_node_sqlite_database(":memory:", NodeSqliteStorageOptions::default())
        .await
        .expect("open db");
    let storage = SqliteStorage::open(db).await.expect("open storage");
    let entry_id: EntryId = id_from_number(2);

    storage
        .commit(
            vec![
                StorageWrite::Conversation {
                    value: ConversationRecord {
                        id: ROOT_CONVERSATION_ID,
                        parent: None,
                        owner: None,
                    },
                },
                StorageWrite::Entry {
                    value: EntryRecord {
                        id: entry_id,
                        conversation_id: ROOT_CONVERSATION_ID,
                        kind: "probe".to_string(),
                        model: None,
                        data: None,
                        head: None,
                        edits: None,
                        by_task_id: None,
                    },
                },
            ],
            context.clone(),
        )
        .await
        .expect("commit");

    let scan_fut = storage.scan_entries(
        pi_durable::types::EntryQuery {
            conversation_id: ROOT_CONVERSATION_ID,
            min_entry_id: None,
            max_entry_id: None,
        },
        10,
        None,
        context.clone(),
    );
    let entry_fut = storage.entry_in_conversation(ROOT_CONVERSATION_ID, entry_id, context.clone());
    let head_fut = storage.find_latest_head_marker(ROOT_CONVERSATION_ID, None, context.clone());
    let close1 = storage.close(context.clone());
    let close2 = storage.close(context.clone());

    let (scan_res, entry_res, head_res, close1_res, close2_res) =
        tokio::join!(scan_fut, entry_fut, head_fut, close1, close2);

    let page = scan_res.expect("scan ok");
    let scan_ids: Vec<EntryId> = page.items.into_iter().map(|e| e.id).collect();
    assert_eq!(scan_ids, vec![entry_id]);
    assert_eq!(entry_res.expect("entry ok").expect("present").entry.kind, "probe");
    assert!(head_res.expect("head ok").is_none());
    close1_res.expect("close 1 ok");
    close2_res.expect("close 2 ok");

    let err = storage
        .scan_entries(
            pi_durable::types::EntryQuery {
                conversation_id: ROOT_CONVERSATION_ID,
                min_entry_id: None,
                max_entry_id: None,
            },
            10,
            None,
            context,
        )
        .await
        .expect_err("closed");
    assert!(err.to_string().contains("SqliteStorage is closed"));
}

#[tokio::test]
async fn rejects_transaction_handle_used_after_transaction_settles() {
    let database = open_node_sqlite_database(":memory:", NodeSqliteStorageOptions::default())
        .await
        .expect("open db");
    database
        .exec("CREATE TABLE stale_probe (value INTEGER)")
        .await
        .expect("create");

    let stale_handle = Arc::new(Mutex::new(None::<Arc<dyn SqliteExecutor>>));
    let holder = Arc::clone(&stale_handle);
    database
        .transaction(move |tx| async move {
            tx.run("INSERT INTO stale_probe (value) VALUES (?)", &[SqliteValue::Integer(1)])
                .await?;
            *holder.lock().expect("lock") = Some(tx);
            Ok(())
        })
        .await
        .expect("tx ok");

    let handle = stale_handle.lock().expect("lock").take().expect("handle");
    let stale_msg = "SQLite transaction handle is no longer active";
    let exec_err = handle
        .exec("INSERT INTO stale_probe (value) VALUES (2)")
        .await
        .expect_err("stale exec");
    assert!(exec_err.to_string().contains(stale_msg));
    let run_err = handle
        .run("INSERT INTO stale_probe (value) VALUES (?)", &[SqliteValue::Integer(3)])
        .await
        .expect_err("stale run");
    assert!(run_err.to_string().contains(stale_msg));

    let rows = database
        .all("SELECT value FROM stale_probe", &[])
        .await
        .expect("select");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_i64("value").expect("value"), 1);
    database.close().await.expect("close");
}

#[tokio::test]
async fn does_not_preserve_guaranteed_rejection_when_rollback_itself_fails() {
    let database = open_node_sqlite_database(":memory:", NodeSqliteStorageOptions::default())
        .await
        .expect("open db");
    database
        .exec("CREATE TABLE rollback_probe (value INTEGER)")
        .await
        .expect("create");

    let err = database
        .transaction(|tx| async move {
            tx.exec("INSERT INTO rollback_probe (value) VALUES (1)").await?;
            tx.exec("COMMIT").await?;
            Err::<(), _>(StorageRejected::new("rejected after an escaped commit").into_error())
        })
        .await
        .expect_err("expected AggregateError");
    assert_eq!(err.name(), "AggregateError");

    let row = database
        .get("SELECT value FROM rollback_probe", &[])
        .await
        .expect("get")
        .expect("row");
    assert_eq!(row.get_i64("value").expect("value"), 1);
    database.close().await.expect("close");
}

#[tokio::test]
async fn awaits_async_transaction_settlement_and_adopts_ids_only_after_success() {
    let context = background_context();
    let inner = open_node_sqlite_database(":memory:", NodeSqliteStorageOptions::default())
        .await
        .expect("open db");
    let database = Arc::new(ControlledSettlementDatabase::new(inner));

    database.control_next_settlement(SettlementMode::Delay);
    let opened = Arc::new(AtomicBool::new(false));
    let opened_flag = Arc::clone(&opened);
    let db_open = Arc::clone(&database) as Arc<dyn SqliteDatabase>;
    let opening = tokio::spawn(async move {
        let storage = SqliteStorage::open(db_open).await;
        opened_flag.store(true, Ordering::SeqCst);
        storage
    });
    tokio::task::yield_now().await;
    assert!(!opened.load(Ordering::SeqCst));
    database.settle();
    let storage = Arc::new(opening.await.expect("join open").expect("open ok"));

    database.control_next_settlement(SettlementMode::Delay);
    let committed = Arc::new(AtomicBool::new(false));
    let committed_flag = Arc::clone(&committed);
    let storage_commit = Arc::clone(&storage);
    let ctx1 = context.clone();
    let committing = tokio::spawn(async move {
        let res = storage_commit
            .commit(
                vec![StorageWrite::Entry {
                    value: EntryRecord {
                        id: EntryId::new_unchecked(100),
                        conversation_id: ROOT_CONVERSATION_ID,
                        kind: "settled".to_string(),
                        model: None,
                        data: None,
                        head: None,
                        edits: None,
                        by_task_id: None,
                    },
                }],
                ctx1,
            )
            .await;
        committed_flag.store(true, Ordering::SeqCst);
        res
    });
    tokio::task::yield_now().await;
    assert!(!committed.load(Ordering::SeqCst));
    assert_eq!(storage.mint_id().await.expect("mint while pending"), 2);
    database.settle();
    assert_eq!(
        committing.await.expect("join commit").expect("commit ok"),
        seq_from_number(1)
    );
    assert_eq!(storage.mint_id().await.expect("mint after adopt"), 101);

    database.control_next_settlement(SettlementMode::Reject);
    let storage_reject = Arc::clone(&storage);
    let ctx2 = context.clone();
    let rejected = tokio::spawn(async move {
        storage_reject
            .commit(
                vec![StorageWrite::Entry {
                    value: EntryRecord {
                        id: EntryId::new_unchecked(200),
                        conversation_id: ROOT_CONVERSATION_ID,
                        kind: "rejected".to_string(),
                        model: None,
                        data: None,
                        head: None,
                        edits: None,
                        by_task_id: None,
                    },
                }],
                ctx2,
            )
            .await
    });
    tokio::task::yield_now().await;
    assert_eq!(storage.mint_id().await.expect("mint during reject"), 102);
    database.settle();
    let reject_err = rejected.await.expect("join reject").expect_err("expected rejection");
    assert!(
        reject_err.to_string().contains("controlled settlement rejection"),
        "unexpected error: {reject_err}"
    );
    assert_eq!(storage.mint_id().await.expect("mint after reject"), 103);
    assert!(
        storage
            .entry(EntryId::new_unchecked(200), context.clone())
            .await
            .expect("lookup rejected")
            .is_none()
    );
    storage.close(context).await.expect("close");
}

#[tokio::test]
async fn reads_document_from_one_committed_state_while_commit_replaces_its_base() {
    let context = background_context();
    let id: DocumentId = id_from_number(5);

    for yields in 0..16 {
        let db = open_node_sqlite_database(":memory:", NodeSqliteStorageOptions::default())
            .await
            .expect("open db");
        let storage = Arc::new(SqliteStorage::open(db).await.expect("open storage"));

        let mut v1: JsonObject = Map::new();
        v1.insert("value".to_string(), json!(1));
        storage
            .commit(
                vec![StorageWrite::DocumentCreate {
                    record: DocumentCreate {
                        id,
                        kind: "replaced".to_string(),
                        key: None,
                        scope: DocumentScope::Session,
                        history: None,
                        fork: None,
                    },
                    version: 1,
                    value: v1.clone(),
                }],
                context.clone(),
            )
            .await
            .expect("create doc");

        let s_read = Arc::clone(&storage);
        let ctx_r = context.clone();
        let read_task = tokio::spawn(async move { s_read.document(id, DocumentPoint::Current, ctx_r).await });

        for _ in 0..yields {
            tokio::task::yield_now().await;
        }

        let mut v2: JsonObject = Map::new();
        v2.insert("value".to_string(), json!(2));
        let replace_res = storage
            .commit(
                vec![StorageWrite::DocumentChange {
                    id,
                    content: DocumentContent::Base {
                        version: 1,
                        value: v2.clone(),
                    },
                }],
                context.clone(),
            )
            .await;

        let stored = read_task
            .await
            .expect("join read")
            .expect("read ok")
            .expect("doc present");
        replace_res.expect("replace ok");
        assert!(stored.value == v1 || stored.value == v2);
        storage.close(context.clone()).await.expect("close");
    }
}
