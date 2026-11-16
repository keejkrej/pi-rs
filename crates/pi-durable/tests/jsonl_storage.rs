//! Port of packages/durable/test/jsonl-storage.test.ts

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_chord::Context;
use pi_chord::context::BACKGROUND_CONTEXT;

fn background_context() -> Context {
    BACKGROUND_CONTEXT.clone()
}
use pi_chord::delta::{Op, OpSet, Seg, SetVerb};
use pi_durable::env::node::{NodeExecutionEnv, NodeExecutionEnvOptions};
use pi_durable::env::{
    CreateDirOptions, CreateTempFileOptions, EnvCleanup, FileContent, FileError, FileErrorCode, FileInfo, FileSystem,
    ReadTextLinesOptions, RemoveOptions, TextLineReader,
};
use pi_durable::ids::{ConversationId, DocumentId, EntryId, Seq, TaskId, id_from_number, seq_from_number};
use pi_durable::storage::jsonl::node::open_node_jsonl_storage;
use pi_durable::storage::jsonl::{JsonlStorage, JsonlStorageOptions};
use pi_durable::testing::{make_storage_provider, run_storage_conformance};
use pi_durable::types::{
    ConversationQuery, ConversationRecord, Cursor, DocumentAddress, DocumentContent, DocumentCreate, DocumentFork,
    DocumentHistory, DocumentPoint, DocumentQuery, DocumentRecord, DocumentScope, EntryLookup, EntryQuery, EntryRecord,
    ErasedTaskRecord, JsonObject, Page, ROOT_CONVERSATION_ID, Storage, StorageWrite, StoredDocument, SubmissionId,
    SubmissionQuery, SubmissionRecord, TaskOutcome, TaskQuery, TaskState,
};
use serde_json::{Value as JsonValue, json};
use tokio::sync::Mutex as AsyncMutex;

struct ReopeningStorage {
    current: AsyncMutex<Option<JsonlStorage>>,
    directory: String,
    closed: Mutex<bool>,
}

impl ReopeningStorage {
    fn new(current: JsonlStorage, directory: String) -> Self {
        Self {
            current: AsyncMutex::new(Some(current)),
            directory,
            closed: Mutex::new(false),
        }
    }
}

#[async_trait]
impl Storage for ReopeningStorage {
    async fn commit(&self, writes: Vec<StorageWrite>, commit_context: Context) -> pi_js::Result<Seq> {
        let mut guard = self.current.lock().await;
        let storage = guard
            .take()
            .ok_or_else(|| pi_js::Error::msg("ReopeningStorage closed"))?;
        if *self.closed.lock().expect("lock") {
            let res = storage.commit(writes, commit_context).await;
            *guard = Some(storage);
            return res;
        }
        let result = storage.commit(writes, commit_context).await;
        let _ = storage.close(background_context()).await;
        let fs = Arc::new(NodeExecutionEnv::with_options(NodeExecutionEnvOptions {
            cwd: self.directory.clone(),
            ..Default::default()
        }));
        let reopened = JsonlStorage::open(
            &self.directory,
            fs,
            background_context(),
            JsonlStorageOptions::default(),
        )
        .await?;
        *guard = Some(reopened);
        result
    }

    async fn mint_id(&self) -> pi_js::Result<i64> {
        self.current
            .lock()
            .await
            .as_ref()
            .ok_or_else(|| pi_js::Error::msg("closed"))?
            .mint_id()
            .await
    }

    async fn conversation(&self, id: ConversationId, context: Context) -> pi_js::Result<Option<ConversationRecord>> {
        self.current
            .lock()
            .await
            .as_ref()
            .ok_or_else(|| pi_js::Error::msg("closed"))?
            .conversation(id, context)
            .await
    }

    async fn scan_conversations(
        &self,
        query: ConversationQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<ConversationRecord, Cursor>> {
        self.current
            .lock()
            .await
            .as_ref()
            .ok_or_else(|| pi_js::Error::msg("closed"))?
            .scan_conversations(query, limit, cursor, context)
            .await
    }

    async fn entry(&self, id: EntryId, context: Context) -> pi_js::Result<Option<EntryLookup>> {
        self.current
            .lock()
            .await
            .as_ref()
            .ok_or_else(|| pi_js::Error::msg("closed"))?
            .entry(id, context)
            .await
    }

    async fn entry_in_conversation(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
        context: Context,
    ) -> pi_js::Result<Option<EntryLookup>> {
        self.current
            .lock()
            .await
            .as_ref()
            .ok_or_else(|| pi_js::Error::msg("closed"))?
            .entry_in_conversation(conversation_id, id, context)
            .await
    }

    async fn find_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
        context: Context,
    ) -> pi_js::Result<Option<EntryRecord>> {
        self.current
            .lock()
            .await
            .as_ref()
            .ok_or_else(|| pi_js::Error::msg("closed"))?
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
        self.current
            .lock()
            .await
            .as_ref()
            .ok_or_else(|| pi_js::Error::msg("closed"))?
            .scan_entries(query, limit, cursor, context)
            .await
    }

    async fn task(&self, id: TaskId, context: Context) -> pi_js::Result<Option<ErasedTaskRecord>> {
        self.current
            .lock()
            .await
            .as_ref()
            .ok_or_else(|| pi_js::Error::msg("closed"))?
            .task(id, context)
            .await
    }

    async fn scan_tasks(
        &self,
        query: TaskQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<ErasedTaskRecord, Cursor>> {
        self.current
            .lock()
            .await
            .as_ref()
            .ok_or_else(|| pi_js::Error::msg("closed"))?
            .scan_tasks(query, limit, cursor, context)
            .await
    }

    async fn submission(&self, id: SubmissionId, context: Context) -> pi_js::Result<Option<SubmissionRecord>> {
        self.current
            .lock()
            .await
            .as_ref()
            .ok_or_else(|| pi_js::Error::msg("closed"))?
            .submission(id, context)
            .await
    }

    async fn scan_submissions(
        &self,
        query: SubmissionQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<SubmissionRecord, Cursor>> {
        self.current
            .lock()
            .await
            .as_ref()
            .ok_or_else(|| pi_js::Error::msg("closed"))?
            .scan_submissions(query, limit, cursor, context)
            .await
    }

    async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
        context: Context,
    ) -> pi_js::Result<Option<SubmissionRecord>> {
        self.current
            .lock()
            .await
            .as_ref()
            .ok_or_else(|| pi_js::Error::msg("closed"))?
            .submission_by_request(conversation_id, request_id, context)
            .await
    }

    async fn find_document(
        &self,
        address: DocumentAddress,
        at: DocumentPoint,
        context: Context,
    ) -> pi_js::Result<Option<DocumentRecord>> {
        self.current
            .lock()
            .await
            .as_ref()
            .ok_or_else(|| pi_js::Error::msg("closed"))?
            .find_document(address, at, context)
            .await
    }

    async fn document(
        &self,
        id: DocumentId,
        at: DocumentPoint,
        context: Context,
    ) -> pi_js::Result<Option<StoredDocument>> {
        self.current
            .lock()
            .await
            .as_ref()
            .ok_or_else(|| pi_js::Error::msg("closed"))?
            .document(id, at, context)
            .await
    }

    async fn scan_documents(
        &self,
        query: DocumentQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<DocumentRecord, Cursor>> {
        self.current
            .lock()
            .await
            .as_ref()
            .ok_or_else(|| pi_js::Error::msg("closed"))?
            .scan_documents(query, limit, cursor, context)
            .await
    }

    async fn close(&self, close_context: Context) -> pi_js::Result<()> {
        *self.closed.lock().expect("lock") = true;
        if let Some(storage) = self.current.lock().await.as_ref() {
            storage.close(close_context).await?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailureOp {
    Append,
    Flush,
    Write,
    Rename,
    Remove,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailureMode {
    Before,
    After,
    Short,
}

#[derive(Clone, Copy, Debug)]
struct Failure {
    operation: FailureOp,
    call: usize,
    mode: FailureMode,
}

#[derive(Default)]
struct EnvTracker {
    operations: Vec<String>,
    failure: Option<Failure>,
    append_calls: usize,
    flush_calls: usize,
    write_calls: usize,
    rename_calls: usize,
    remove_calls: usize,
}

impl EnvTracker {
    fn reset_observations(&mut self) {
        self.append_calls = 0;
        self.flush_calls = 0;
        self.write_calls = 0;
        self.rename_calls = 0;
        self.remove_calls = 0;
        self.operations.clear();
    }
}

struct InstrumentedEnv {
    inner: NodeExecutionEnv,
    tracker: Mutex<EnvTracker>,
}

impl InstrumentedEnv {
    fn new(cwd: &str) -> Self {
        Self {
            inner: NodeExecutionEnv::with_options(NodeExecutionEnvOptions {
                cwd: cwd.to_string(),
                ..Default::default()
            }),
            tracker: Mutex::new(EnvTracker::default()),
        }
    }

    fn fail(&self, failure: Failure) {
        let mut guard = self.tracker.lock().expect("lock");
        guard.failure = Some(failure);
        guard.reset_observations();
    }

    fn clear(&self) {
        let mut guard = self.tracker.lock().expect("lock");
        guard.failure = None;
        guard.reset_observations();
    }

    fn operations(&self) -> Vec<String> {
        self.tracker.lock().expect("lock").operations.clone()
    }
}

fn file_basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

#[async_trait]
impl EnvCleanup for InstrumentedEnv {
    async fn cleanup(&self, context: &Context) {
        self.inner.cleanup(context).await;
    }
}

#[async_trait]
impl FileSystem for InstrumentedEnv {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn cwd(&self) -> &str {
        self.inner.cwd()
    }

    async fn absolute_path(&self, path: &str, context: &Context) -> Result<String, FileError> {
        self.inner.absolute_path(path, context).await
    }

    async fn join_path(&self, parts: &[&str], context: &Context) -> Result<String, FileError> {
        self.inner.join_path(parts, context).await
    }

    async fn read_text_file(&self, path: &str, context: &Context) -> Result<String, FileError> {
        self.inner.read_text_file(path, context).await
    }

    async fn open_text_line_reader(&self, path: &str, context: &Context) -> Result<Box<dyn TextLineReader>, FileError> {
        self.inner.open_text_line_reader(path, context).await
    }

    async fn read_text_lines(
        &self,
        path: &str,
        options: Option<ReadTextLinesOptions>,
        context: &Context,
    ) -> Result<Vec<String>, FileError> {
        self.inner.read_text_lines(path, options, context).await
    }

    async fn read_binary_file(&self, path: &str, context: &Context) -> Result<Vec<u8>, FileError> {
        self.inner.read_binary_file(path, context).await
    }

    async fn write_file(&self, path: &str, content: FileContent<'_>, context: &Context) -> Result<(), FileError> {
        let failure = {
            let mut guard = self.tracker.lock().expect("lock");
            guard.write_calls += 1;
            let call = guard.write_calls;
            guard.operations.push(format!("write:{}", file_basename(path)));
            guard
                .failure
                .filter(|f| f.operation == FailureOp::Write && f.call == call)
        };
        let Some(f) = failure else {
            return self.inner.write_file(path, content, context).await;
        };
        match f.mode {
            FailureMode::Before => Err(FileError::new(
                FileErrorCode::Unknown,
                "injected write failure",
                Some(path.to_string()),
            )),
            FailureMode::Short => {
                let bytes = match content {
                    FileContent::Text(t) => t.as_bytes(),
                    FileContent::Bytes(b) => b,
                };
                let split = std::cmp::max(1, bytes.len() / 2);
                self.inner
                    .write_file(path, FileContent::Bytes(&bytes[..split]), context)
                    .await?;
                Err(FileError::new(
                    FileErrorCode::Unknown,
                    "injected short write",
                    Some(path.to_string()),
                ))
            }
            FailureMode::After => {
                self.inner.write_file(path, content, context).await?;
                Err(FileError::new(
                    FileErrorCode::Unknown,
                    "injected post-write failure",
                    Some(path.to_string()),
                ))
            }
        }
    }

    async fn append_file(&self, path: &str, content: FileContent<'_>, context: &Context) -> Result<(), FileError> {
        let failure = {
            let mut guard = self.tracker.lock().expect("lock");
            guard.append_calls += 1;
            let call = guard.append_calls;
            guard.operations.push(format!("append:{}", file_basename(path)));
            guard
                .failure
                .filter(|f| f.operation == FailureOp::Append && f.call == call)
        };
        let Some(f) = failure else {
            return self.inner.append_file(path, content, context).await;
        };
        match f.mode {
            FailureMode::Before => Err(FileError::new(
                FileErrorCode::Unknown,
                "injected append failure",
                Some(path.to_string()),
            )),
            FailureMode::Short => {
                let bytes = match content {
                    FileContent::Text(t) => t.as_bytes(),
                    FileContent::Bytes(b) => b,
                };
                let split = std::cmp::max(1, bytes.len() / 2);
                self.inner
                    .append_file(path, FileContent::Bytes(&bytes[..split]), context)
                    .await?;
                Err(FileError::new(
                    FileErrorCode::Unknown,
                    "injected short append",
                    Some(path.to_string()),
                ))
            }
            FailureMode::After => {
                self.inner.append_file(path, content, context).await?;
                Err(FileError::new(
                    FileErrorCode::Unknown,
                    "injected post-append failure",
                    Some(path.to_string()),
                ))
            }
        }
    }

    async fn truncate_file(&self, path: &str, size: u64, context: &Context) -> Result<(), FileError> {
        self.inner.truncate_file(path, size, context).await
    }

    async fn flush_file(&self, path: &str, context: &Context) -> Result<(), FileError> {
        let failure = {
            let mut guard = self.tracker.lock().expect("lock");
            guard.flush_calls += 1;
            let call = guard.flush_calls;
            guard.operations.push(format!("flush:{}", file_basename(path)));
            guard
                .failure
                .filter(|f| f.operation == FailureOp::Flush && f.call == call)
        };
        let Some(f) = failure else {
            return self.inner.flush_file(path, context).await;
        };
        if f.mode == FailureMode::After {
            self.inner.flush_file(path, context).await?;
        }
        Err(FileError::new(
            FileErrorCode::Unknown,
            "injected flush failure",
            Some(path.to_string()),
        ))
    }

    async fn rename_file(&self, source_path: &str, destination_path: &str, context: &Context) -> Result<(), FileError> {
        let failure = {
            let mut guard = self.tracker.lock().expect("lock");
            guard.rename_calls += 1;
            let call = guard.rename_calls;
            guard.operations.push(format!(
                "rename:{}->{}",
                file_basename(source_path),
                file_basename(destination_path)
            ));
            guard
                .failure
                .filter(|f| f.operation == FailureOp::Rename && f.call == call)
        };
        let Some(f) = failure else {
            return self.inner.rename_file(source_path, destination_path, context).await;
        };
        if f.mode == FailureMode::After {
            self.inner.rename_file(source_path, destination_path, context).await?;
        }
        Err(FileError::new(
            FileErrorCode::Unknown,
            "injected rename failure",
            Some(source_path.to_string()),
        ))
    }

    async fn file_info(&self, path: &str, context: &Context) -> Result<FileInfo, FileError> {
        self.inner.file_info(path, context).await
    }

    async fn list_dir(&self, path: &str, context: &Context) -> Result<Vec<FileInfo>, FileError> {
        self.inner.list_dir(path, context).await
    }

    async fn canonical_path(&self, path: &str, context: &Context) -> Result<String, FileError> {
        self.inner.canonical_path(path, context).await
    }

    async fn exists(&self, path: &str, context: &Context) -> Result<bool, FileError> {
        self.inner.exists(path, context).await
    }

    async fn create_dir(
        &self,
        path: &str,
        options: Option<CreateDirOptions>,
        context: &Context,
    ) -> Result<(), FileError> {
        self.inner.create_dir(path, options, context).await
    }

    async fn remove(&self, path: &str, options: Option<RemoveOptions>, context: &Context) -> Result<(), FileError> {
        let failure = {
            let mut guard = self.tracker.lock().expect("lock");
            guard.remove_calls += 1;
            let call = guard.remove_calls;
            guard.operations.push(format!("remove:{}", file_basename(path)));
            guard
                .failure
                .filter(|f| f.operation == FailureOp::Remove && f.call == call)
        };
        let Some(f) = failure else {
            return self.inner.remove(path, options, context).await;
        };
        if f.mode == FailureMode::After {
            self.inner.remove(path, options, context).await?;
        }
        Err(FileError::new(
            FileErrorCode::Unknown,
            "injected remove failure",
            Some(path.to_string()),
        ))
    }

    async fn create_temp_dir(&self, prefix: Option<&str>, context: &Context) -> Result<String, FileError> {
        self.inner.create_temp_dir(prefix, context).await
    }

    async fn create_temp_file(
        &self,
        options: Option<CreateTempFileOptions>,
        context: &Context,
    ) -> Result<String, FileError> {
        self.inner.create_temp_file(options, context).await
    }
}

fn temp_directory() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().to_string_lossy().into_owned();
    (dir, path)
}

async fn open_storage(dir: &str) -> JsonlStorage {
    let fs: Arc<dyn FileSystem> = Arc::new(NodeExecutionEnv::with_options(NodeExecutionEnvOptions {
        cwd: dir.to_string(),
        ..Default::default()
    }));
    JsonlStorage::open(dir, fs, background_context(), JsonlStorageOptions::default())
        .await
        .expect("open jsonl storage")
}

async fn open_with_env(dir: &str, env: Arc<InstrumentedEnv>, fsync: bool) -> JsonlStorage {
    let fs: Arc<dyn FileSystem> = env;
    JsonlStorage::open(dir, fs, background_context(), JsonlStorageOptions { fsync })
        .await
        .expect("open jsonl storage with instrumented env")
}

fn pending_task(id: TaskId, phase: &str) -> ErasedTaskRecord {
    ErasedTaskRecord {
        id,
        conversation_id: ROOT_CONVERSATION_ID,
        kind: "test.task".to_string(),
        version: 1,
        input: JsonValue::Null,
        owner: None,
        background: false,
        abort_requested: false,
        state: TaskState::Pending {
            checkpoint: json!({ "phase": phase }),
        },
        memos: None,
    }
}

fn terminal_task(id: TaskId) -> ErasedTaskRecord {
    ErasedTaskRecord {
        id,
        conversation_id: ROOT_CONVERSATION_ID,
        kind: "test.task".to_string(),
        version: 1,
        input: JsonValue::Null,
        owner: None,
        background: false,
        abort_requested: false,
        state: TaskState::Terminal {
            outcome: TaskOutcome::Completed {
                result: JsonValue::Null,
            },
        },
        memos: None,
    }
}

async fn create_root(storage: &dyn Storage) {
    storage
        .commit(
            vec![StorageWrite::Conversation {
                value: ConversationRecord {
                    id: ROOT_CONVERSATION_ID,
                    parent: None,
                    owner: None,
                },
            }],
            background_context(),
        )
        .await
        .expect("create root");
}

fn session_document(id: DocumentId, kind: &str) -> DocumentCreate {
    DocumentCreate {
        id,
        kind: kind.to_string(),
        key: None,
        scope: DocumentScope::Session,
        history: None,
        fork: None,
    }
}

fn obj(value: JsonValue) -> JsonObject {
    serde_json::from_value(value).expect("JsonObject")
}

fn read_lines(path: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(path).expect("read lines");
    if text.is_empty() {
        Vec::new()
    } else {
        text.trim_end_matches('\n')
            .split('\n')
            .map(ToString::to_string)
            .collect()
    }
}

#[tokio::test]
async fn jsonl_storage_conformance() {
    run_storage_conformance(
        "JsonlStorage",
        make_storage_provider(|callback| async move {
            let (_dir, dir_path) = temp_directory();
            let storage = Arc::new(open_storage(&dir_path).await);
            let s: Arc<dyn Storage> = storage;
            callback(Arc::clone(&s)).await;
            let _ = s.close(background_context()).await;
        }),
    )
    .await;
}

#[tokio::test]
async fn jsonl_storage_across_reopen_conformance() {
    run_storage_conformance(
        "JsonlStorage across reopen",
        make_storage_provider(|callback| async move {
            let (_dir, dir_path) = temp_directory();
            let current = open_storage(&dir_path).await;
            let storage: Arc<dyn Storage> = Arc::new(ReopeningStorage::new(current, dir_path));
            callback(Arc::clone(&storage)).await;
            let _ = storage.close(background_context()).await;
        }),
    )
    .await;
}

#[tokio::test]
async fn opens_through_the_node_adapter() {
    let context = background_context();
    let (_dir, dir_path) = temp_directory();
    let storage = open_node_jsonl_storage(&dir_path, context.clone(), JsonlStorageOptions::default())
        .await
        .expect("open node jsonl");
    create_root(&storage).await;
    assert_eq!(
        storage
            .conversation(ROOT_CONVERSATION_ID, context.clone())
            .await
            .expect("conv"),
        Some(ConversationRecord {
            id: ROOT_CONVERSATION_ID,
            parent: None,
            owner: None,
        })
    );
    storage.close(context).await.expect("close");
}

#[tokio::test]
async fn publishes_every_commit_before_reclaiming_current_only_document_and_terminal_task_sidecars() {
    let context = background_context();
    let (dir, dir_path) = temp_directory();
    let storage = open_storage(&dir_path).await;
    create_root(&storage).await;
    let task_id: TaskId = id_from_number(storage.mint_id().await.expect("mint task"));
    let doc_id: DocumentId = id_from_number(storage.mint_id().await.expect("mint doc"));

    storage
        .commit(
            vec![StorageWrite::Task {
                value: pending_task(task_id, "ready"),
            }],
            context.clone(),
        )
        .await
        .expect("task");
    storage
        .commit(
            vec![StorageWrite::DocumentCreate {
                record: session_document(doc_id, "test.document"),
                version: 1,
                value: obj(json!({ "count": 0 })),
            }],
            context.clone(),
        )
        .await
        .expect("doc create");
    storage
        .commit(
            vec![StorageWrite::DocumentChange {
                id: doc_id,
                content: DocumentContent::Delta {
                    version: 1,
                    ops: vec![],
                },
            }],
            context.clone(),
        )
        .await
        .expect("doc delta");
    storage
        .commit(
            vec![StorageWrite::DocumentChange {
                id: doc_id,
                content: DocumentContent::Base {
                    version: 1,
                    value: obj(json!({ "count": 1 })),
                },
            }],
            context.clone(),
        )
        .await
        .expect("doc base");

    let task_sidecar = dir.path().join(format!("task-{task_id}.jsonl"));
    let doc_sidecar = dir.path().join(format!("doc-{doc_id}.jsonl"));
    assert_eq!(read_lines(&task_sidecar).len(), 1);
    assert_eq!(read_lines(&doc_sidecar).len(), 1);

    storage
        .commit(vec![StorageWrite::DocumentRetire { id: doc_id }], context.clone())
        .await
        .expect("retire doc");
    storage
        .commit(
            vec![StorageWrite::Task {
                value: terminal_task(task_id),
            }],
            context.clone(),
        )
        .await
        .expect("terminal task");

    let main_lines = read_lines(&dir.path().join("main.jsonl"));
    assert_eq!(main_lines.len(), 7);
    assert!(!task_sidecar.exists());
    assert!(!doc_sidecar.exists());
    let marker_types: Vec<String> = main_lines
        .iter()
        .map(|line| {
            serde_json::from_str::<JsonValue>(line).expect("json")["type"]
                .as_str()
                .expect("type str")
                .to_string()
        })
        .collect();
    assert_eq!(marker_types, vec!["commit"; 7]);
    storage.close(context).await.expect("close");
}

#[tokio::test]
async fn orders_multiple_live_task_replacements_in_one_sidecar_by_commit_ordinal() {
    let context = background_context();
    let (dir, dir_path) = temp_directory();
    let storage = open_storage(&dir_path).await;
    create_root(&storage).await;
    let task_id: TaskId = id_from_number(storage.mint_id().await.expect("mint task"));
    storage
        .commit(
            vec![
                StorageWrite::Task {
                    value: pending_task(task_id, "first"),
                },
                StorageWrite::Task {
                    value: pending_task(task_id, "second"),
                },
            ],
            context.clone(),
        )
        .await
        .expect("commit tasks");
    assert_eq!(read_lines(&dir.path().join(format!("task-{task_id}.jsonl"))).len(), 2);
    storage.close(context.clone()).await.expect("close");

    let reopened = open_storage(&dir_path).await;
    assert_eq!(
        reopened.task(task_id, context.clone()).await.expect("task"),
        Some(pending_task(task_id, "second"))
    );
    reopened.close(context).await.expect("close");
}

#[tokio::test]
async fn removes_a_complete_plus_torn_unconfirmed_multi_record_sidecar_append() {
    let context = background_context();
    let (dir, dir_path) = temp_directory();
    let env = Arc::new(InstrumentedEnv::new(&dir_path));
    let storage = open_with_env(&dir_path, Arc::clone(&env), false).await;
    create_root(&storage).await;
    let task_id: TaskId = id_from_number(storage.mint_id().await.expect("mint task"));

    env.fail(Failure {
        operation: FailureOp::Append,
        call: 1,
        mode: FailureMode::Short,
    });
    let big_phase = format!("second-{}", "x".repeat(512));
    let err = storage
        .commit(
            vec![
                StorageWrite::Task {
                    value: pending_task(task_id, "first"),
                },
                StorageWrite::Task {
                    value: pending_task(task_id, &big_phase),
                },
            ],
            context.clone(),
        )
        .await
        .expect_err("expected poisoned");
    assert!(err.to_string().contains("poisoned"), "{err}");

    let sidecar_path = dir.path().join(format!("task-{task_id}.jsonl"));
    let partial = std::fs::read(&sidecar_path).expect("read partial");
    assert_ne!(partial.last().copied(), Some(b'\n'));
    assert_eq!(partial.iter().filter(|&&b| b == b'\n').count(), 1);
    let _ = storage.close(context.clone()).await;

    let reopened = open_storage(&dir_path).await;
    assert!(reopened.task(task_id, context.clone()).await.expect("task").is_none());
    assert_eq!(std::fs::metadata(&sidecar_path).expect("stat").len(), 0);
    reopened.close(context).await.expect("close");
}

#[tokio::test]
async fn prepares_before_io_and_leaves_preparation_failures_usable() {
    let context = background_context();
    let (_dir, dir_path) = temp_directory();
    let env = Arc::new(InstrumentedEnv::new(&dir_path));
    let storage = open_with_env(&dir_path, Arc::clone(&env), false).await;
    create_root(&storage).await;
    env.clear();

    let id: EntryId = id_from_number(storage.mint_id().await.expect("mint entry"));
    // Commit containing a duplicate ID fails preparation before any filesystem writes.
    assert!(
        storage
            .commit(
                vec![
                    StorageWrite::Entry {
                        value: EntryRecord {
                            id,
                            conversation_id: ROOT_CONVERSATION_ID,
                            kind: "good".to_string(),
                            model: None,
                            data: None,
                            head: None,
                            edits: None,
                            by_task_id: None,
                        },
                    },
                    StorageWrite::Entry {
                        value: EntryRecord {
                            id,
                            conversation_id: ROOT_CONVERSATION_ID,
                            kind: "duplicate".to_string(),
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
            .is_err()
    );
    assert!(env.operations().is_empty());
    assert!(storage.entry(id, context.clone()).await.expect("entry").is_none());

    assert_eq!(
        storage
            .commit(
                vec![StorageWrite::Entry {
                    value: EntryRecord {
                        id,
                        conversation_id: ROOT_CONVERSATION_ID,
                        kind: "good".to_string(),
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
            .expect("usable after rejection"),
        seq_from_number(2)
    );
    storage.close(context).await.expect("close");
}

#[tokio::test]
async fn poisons_after_append_failures_and_recovers_only_confirmed_state() {
    let context = background_context();
    let cases = [
        Failure {
            operation: FailureOp::Append,
            call: 1,
            mode: FailureMode::Before,
        },
        Failure {
            operation: FailureOp::Append,
            call: 1,
            mode: FailureMode::After,
        },
        Failure {
            operation: FailureOp::Append,
            call: 1,
            mode: FailureMode::Short,
        },
        Failure {
            operation: FailureOp::Append,
            call: 2,
            mode: FailureMode::Before,
        },
        Failure {
            operation: FailureOp::Append,
            call: 2,
            mode: FailureMode::After,
        },
        Failure {
            operation: FailureOp::Append,
            call: 3,
            mode: FailureMode::Before,
        },
        Failure {
            operation: FailureOp::Append,
            call: 3,
            mode: FailureMode::Short,
        },
        Failure {
            operation: FailureOp::Append,
            call: 3,
            mode: FailureMode::After,
        },
    ];

    for failure in cases {
        let (_dir, dir_path) = temp_directory();
        let env = Arc::new(InstrumentedEnv::new(&dir_path));
        let storage = open_with_env(&dir_path, Arc::clone(&env), false).await;
        create_root(&storage).await;
        let first_id: DocumentId = id_from_number(storage.mint_id().await.expect("mint 1"));
        let second_id: DocumentId = id_from_number(storage.mint_id().await.expect("mint 2"));
        env.fail(failure);

        let err = storage
            .commit(
                vec![
                    StorageWrite::DocumentCreate {
                        record: session_document(first_id, "first"),
                        version: 1,
                        value: obj(json!({ "text": "α" })),
                    },
                    StorageWrite::DocumentCreate {
                        record: session_document(second_id, "second"),
                        version: 1,
                        value: obj(json!({ "text": "β" })),
                    },
                ],
                context.clone(),
            )
            .await
            .expect_err("expected poison");
        assert!(err.to_string().contains("poisoned"), "{failure:?}: {err}");

        let read_err = storage
            .document(first_id, DocumentPoint::Current, context.clone())
            .await
            .expect_err("poisoned read");
        assert!(read_err.to_string().contains("poisoned"));
        let _ = storage.close(context.clone()).await;

        let reopened = open_storage(&dir_path).await;
        let marker_survived = failure.call == 3 && failure.mode == FailureMode::After;
        let first = reopened
            .document(first_id, DocumentPoint::Current, context.clone())
            .await
            .expect("read 1");
        let second = reopened
            .document(second_id, DocumentPoint::Current, context.clone())
            .await
            .expect("read 2");
        if marker_survived {
            assert_eq!(first.expect("first present").value, obj(json!({ "text": "α" })));
            assert_eq!(second.expect("second present").value, obj(json!({ "text": "β" })));
        } else {
            assert!(first.is_none());
            assert!(second.is_none());
        }
        reopened.close(context.clone()).await.expect("close");
    }
}

#[tokio::test]
async fn poisons_after_flush_failures_and_never_writes_a_marker() {
    let context = background_context();
    let cases = [
        Failure {
            operation: FailureOp::Flush,
            call: 1,
            mode: FailureMode::Before,
        },
        Failure {
            operation: FailureOp::Flush,
            call: 1,
            mode: FailureMode::After,
        },
        Failure {
            operation: FailureOp::Flush,
            call: 2,
            mode: FailureMode::Before,
        },
        Failure {
            operation: FailureOp::Flush,
            call: 2,
            mode: FailureMode::After,
        },
    ];
    for failure in cases {
        let (_dir, dir_path) = temp_directory();
        let env = Arc::new(InstrumentedEnv::new(&dir_path));
        let storage = open_with_env(&dir_path, Arc::clone(&env), true).await;
        create_root(&storage).await;
        let first_id: DocumentId = id_from_number(storage.mint_id().await.expect("mint 1"));
        let second_id: DocumentId = id_from_number(storage.mint_id().await.expect("mint 2"));
        env.fail(failure);

        let err = storage
            .commit(
                vec![
                    StorageWrite::DocumentCreate {
                        record: session_document(first_id, "flush.first"),
                        version: 1,
                        value: JsonObject::new(),
                    },
                    StorageWrite::DocumentCreate {
                        record: session_document(second_id, "flush.second"),
                        version: 1,
                        value: JsonObject::new(),
                    },
                ],
                context.clone(),
            )
            .await
            .expect_err("expected poison");
        assert!(err.to_string().contains("poisoned"), "{failure:?}: {err}");
        let _ = storage.close(context.clone()).await;

        let reopened = open_storage(&dir_path).await;
        assert!(
            reopened
                .document(first_id, DocumentPoint::Current, context.clone())
                .await
                .expect("read 1")
                .is_none()
        );
        assert!(
            reopened
                .document(second_id, DocumentPoint::Current, context.clone())
                .await
                .expect("read 2")
                .is_none()
        );
        reopened.close(context.clone()).await.expect("close");
    }
}

#[tokio::test]
async fn orders_publication_flushes_exactly_and_flushes_main_only_to_authorize_reclamation() {
    let context = background_context();
    for fsync in [false, true] {
        let (_dir, dir_path) = temp_directory();
        let env = Arc::new(InstrumentedEnv::new(&dir_path));
        let storage = open_with_env(&dir_path, Arc::clone(&env), fsync).await;
        create_root(&storage).await;
        let first_id: DocumentId = id_from_number(storage.mint_id().await.expect("mint 1"));
        let second_id: DocumentId = id_from_number(storage.mint_id().await.expect("mint 2"));
        env.clear();

        storage
            .commit(
                vec![
                    StorageWrite::DocumentCreate {
                        record: session_document(second_id, "second"),
                        version: 1,
                        value: JsonObject::new(),
                    },
                    StorageWrite::DocumentCreate {
                        record: session_document(first_id, "first"),
                        version: 1,
                        value: JsonObject::new(),
                    },
                ],
                context.clone(),
            )
            .await
            .expect("create both");

        let mut expected = vec![
            format!("append:doc-{second_id}.jsonl"),
            format!("append:doc-{first_id}.jsonl"),
        ];
        if fsync {
            expected.push(format!("flush:doc-{second_id}.jsonl"));
            expected.push(format!("flush:doc-{first_id}.jsonl"));
        }
        expected.push("append:main.jsonl".to_string());
        assert_eq!(env.operations(), expected);

        env.clear();
        storage
            .commit(
                vec![StorageWrite::DocumentChange {
                    id: first_id,
                    content: DocumentContent::Base {
                        version: 1,
                        value: obj(json!({ "checkpoint": true })),
                    },
                }],
                context.clone(),
            )
            .await
            .expect("base reclaim");
        let mut expected_reclaim = vec![format!("append:doc-{first_id}.jsonl")];
        if fsync {
            expected_reclaim.push(format!("flush:doc-{first_id}.jsonl"));
        }
        expected_reclaim.push("append:main.jsonl".to_string());
        if fsync {
            expected_reclaim.push("flush:main.jsonl".to_string());
        }
        expected_reclaim.push(format!("write:doc-{first_id}.jsonl.reclaim"));
        if fsync {
            expected_reclaim.push(format!("flush:doc-{first_id}.jsonl.reclaim"));
        }
        expected_reclaim.push(format!("rename:doc-{first_id}.jsonl.reclaim->doc-{first_id}.jsonl"));
        assert_eq!(env.operations(), expected_reclaim);

        env.clear();
        let entry_id: EntryId = id_from_number(storage.mint_id().await.expect("mint entry"));
        storage
            .commit(
                vec![StorageWrite::Entry {
                    value: EntryRecord {
                        id: entry_id,
                        conversation_id: ROOT_CONVERSATION_ID,
                        kind: "main-only".to_string(),
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
            .expect("main-only entry");
        assert_eq!(env.operations(), vec!["append:main.jsonl"]);

        let task_id: TaskId = id_from_number(storage.mint_id().await.expect("mint task"));
        storage
            .commit(
                vec![StorageWrite::Task {
                    value: pending_task(task_id, "ready"),
                }],
                context.clone(),
            )
            .await
            .expect("pending task");
        env.clear();
        storage
            .commit(
                vec![StorageWrite::Task {
                    value: terminal_task(task_id),
                }],
                context.clone(),
            )
            .await
            .expect("terminal task");
        let mut expected_task = vec!["append:main.jsonl".to_string()];
        if fsync {
            expected_task.push("flush:main.jsonl".to_string());
        }
        expected_task.push(format!("remove:task-{task_id}.jsonl"));
        assert_eq!(env.operations(), expected_task);

        storage.close(context.clone()).await.expect("close");
    }
}

#[tokio::test]
async fn recovers_committed_base_across_reclaim_write_and_rename_failures() {
    let context = background_context();
    let cases = [
        Failure {
            operation: FailureOp::Write,
            call: 1,
            mode: FailureMode::Before,
        },
        Failure {
            operation: FailureOp::Write,
            call: 1,
            mode: FailureMode::Short,
        },
        Failure {
            operation: FailureOp::Write,
            call: 1,
            mode: FailureMode::After,
        },
        Failure {
            operation: FailureOp::Rename,
            call: 1,
            mode: FailureMode::Before,
        },
        Failure {
            operation: FailureOp::Rename,
            call: 1,
            mode: FailureMode::After,
        },
    ];
    for failure in cases {
        let (dir, dir_path) = temp_directory();
        let env = Arc::new(InstrumentedEnv::new(&dir_path));
        let storage = open_with_env(&dir_path, Arc::clone(&env), false).await;
        create_root(&storage).await;
        let id: DocumentId = id_from_number(storage.mint_id().await.expect("mint doc"));
        storage
            .commit(
                vec![StorageWrite::DocumentCreate {
                    record: session_document(id, "test.document"),
                    version: 1,
                    value: obj(json!({ "count": 0 })),
                }],
                context.clone(),
            )
            .await
            .expect("create");
        storage
            .commit(
                vec![StorageWrite::DocumentChange {
                    id,
                    content: DocumentContent::Delta {
                        version: 1,
                        ops: vec![Op::Set(OpSet(
                            SetVerb::S,
                            vec![Seg::String("count".to_owned())],
                            json!(1),
                        ))],
                    },
                }],
                context.clone(),
            )
            .await
            .expect("delta");

        env.fail(failure);
        assert_eq!(
            storage
                .commit(
                    vec![StorageWrite::DocumentChange {
                        id,
                        content: DocumentContent::Base {
                            version: 1,
                            value: obj(json!({ "count": 2 })),
                        },
                    }],
                    context.clone(),
                )
                .await
                .expect("commit base"),
            seq_from_number(4)
        );
        assert_eq!(
            storage
                .document(id, DocumentPoint::Current, context.clone())
                .await
                .expect("read")
                .expect("present")
                .value,
            obj(json!({ "count": 2 }))
        );
        storage.close(context.clone()).await.expect("close");

        let reopened = open_storage(&dir_path).await;
        assert_eq!(
            reopened
                .document(id, DocumentPoint::Current, context.clone())
                .await
                .expect("reopened read")
                .expect("present")
                .value,
            obj(json!({ "count": 2 }))
        );
        assert_eq!(read_lines(&dir.path().join(format!("doc-{id}.jsonl"))).len(), 1);
        let reclaim_files: Vec<_> = std::fs::read_dir(dir.path())
            .expect("readdir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".reclaim"))
            .collect();
        assert!(reclaim_files.is_empty());
        reopened.close(context.clone()).await.expect("close");
    }
}

#[tokio::test]
async fn recovers_document_retirement_reclamation_across_remove_failures() {
    let context = background_context();
    for mode in [FailureMode::Before, FailureMode::After] {
        let (dir, dir_path) = temp_directory();
        let env = Arc::new(InstrumentedEnv::new(&dir_path));
        let storage = open_with_env(&dir_path, Arc::clone(&env), false).await;
        create_root(&storage).await;
        let task_id: TaskId = id_from_number(storage.mint_id().await.expect("mint task"));
        let id: DocumentId = id_from_number(storage.mint_id().await.expect("mint doc"));
        storage
            .commit(
                vec![
                    StorageWrite::Task {
                        value: pending_task(task_id, "ready"),
                    },
                    StorageWrite::DocumentCreate {
                        record: DocumentCreate {
                            id,
                            kind: "task.document".to_string(),
                            key: None,
                            scope: DocumentScope::Task { task_id },
                            history: None,
                            fork: None,
                        },
                        version: 1,
                        value: obj(json!({ "count": 1 })),
                    },
                ],
                context.clone(),
            )
            .await
            .expect("create");
        env.fail(Failure {
            operation: FailureOp::Remove,
            call: 1,
            mode,
        });
        assert_eq!(
            storage
                .commit(vec![StorageWrite::DocumentRetire { id }], context.clone())
                .await
                .expect("retire"),
            seq_from_number(3)
        );
        assert!(
            storage
                .document(id, DocumentPoint::Current, context.clone())
                .await
                .expect("doc")
                .is_none()
        );
        storage.close(context.clone()).await.expect("close");

        let reopened = open_storage(&dir_path).await;
        assert!(
            reopened
                .document(id, DocumentPoint::Current, context.clone())
                .await
                .expect("doc")
                .is_none()
        );
        assert_eq!(
            reopened.task(task_id, context.clone()).await.expect("task"),
            Some(pending_task(task_id, "ready"))
        );
        assert!(!dir.path().join(format!("doc-{id}.jsonl")).exists());
        let reclaim_files: Vec<_> = std::fs::read_dir(dir.path())
            .expect("readdir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".reclaim"))
            .collect();
        assert!(reclaim_files.is_empty());
        reopened.close(context.clone()).await.expect("close");
    }
}

#[tokio::test]
async fn defers_reclamation_after_authorizing_main_flush_failure() {
    let context = background_context();
    for mode in [FailureMode::Before, FailureMode::After] {
        let (dir, dir_path) = temp_directory();
        let env = Arc::new(InstrumentedEnv::new(&dir_path));
        let storage = open_with_env(&dir_path, Arc::clone(&env), true).await;
        create_root(&storage).await;
        let id: DocumentId = id_from_number(storage.mint_id().await.expect("mint doc"));
        storage
            .commit(
                vec![StorageWrite::DocumentCreate {
                    record: session_document(id, "test.document"),
                    version: 1,
                    value: obj(json!({ "count": 0 })),
                }],
                context.clone(),
            )
            .await
            .expect("create");
        env.fail(Failure {
            operation: FailureOp::Flush,
            call: 2,
            mode,
        });
        assert_eq!(
            storage
                .commit(
                    vec![StorageWrite::DocumentChange {
                        id,
                        content: DocumentContent::Base {
                            version: 1,
                            value: obj(json!({ "count": 2 })),
                        },
                    }],
                    context.clone(),
                )
                .await
                .expect("commit base"),
            seq_from_number(3)
        );
        assert_eq!(
            env.operations(),
            vec![
                format!("append:doc-{id}.jsonl"),
                format!("flush:doc-{id}.jsonl"),
                "append:main.jsonl".to_string(),
                "flush:main.jsonl".to_string(),
            ]
        );
        assert_eq!(
            storage
                .document(id, DocumentPoint::Current, context.clone())
                .await
                .expect("read")
                .expect("present")
                .value,
            obj(json!({ "count": 2 }))
        );
        assert_eq!(read_lines(&dir.path().join(format!("doc-{id}.jsonl"))).len(), 2);
        storage.close(context.clone()).await.expect("close");

        let recovery_env = Arc::new(InstrumentedEnv::new(&dir_path));
        recovery_env.fail(Failure {
            operation: FailureOp::Flush,
            call: 1,
            mode,
        });
        let deferred = open_with_env(&dir_path, Arc::clone(&recovery_env), true).await;
        assert_eq!(
            deferred
                .document(id, DocumentPoint::Current, context.clone())
                .await
                .expect("deferred read")
                .expect("present")
                .value,
            obj(json!({ "count": 2 }))
        );
        assert_eq!(recovery_env.operations(), vec!["flush:main.jsonl"]);
        assert_eq!(read_lines(&dir.path().join(format!("doc-{id}.jsonl"))).len(), 2);
        deferred.close(context.clone()).await.expect("close");

        let reclaimed_env = Arc::new(InstrumentedEnv::new(&dir_path));
        let reclaimed = open_with_env(&dir_path, reclaimed_env, true).await;
        assert_eq!(
            reclaimed
                .document(id, DocumentPoint::Current, context.clone())
                .await
                .expect("reclaimed read")
                .expect("present")
                .value,
            obj(json!({ "count": 2 }))
        );
        assert_eq!(read_lines(&dir.path().join(format!("doc-{id}.jsonl"))).len(), 1);
        reclaimed.close(context.clone()).await.expect("close");
    }
}

#[tokio::test]
async fn keeps_a_committed_base_usable_after_reclaim_temp_flush_failure() {
    let context = background_context();
    for mode in [FailureMode::Before, FailureMode::After] {
        let (dir, dir_path) = temp_directory();
        let env = Arc::new(InstrumentedEnv::new(&dir_path));
        let storage = open_with_env(&dir_path, Arc::clone(&env), true).await;
        create_root(&storage).await;
        let id: DocumentId = id_from_number(storage.mint_id().await.expect("mint doc"));
        storage
            .commit(
                vec![StorageWrite::DocumentCreate {
                    record: session_document(id, "test.document"),
                    version: 1,
                    value: obj(json!({ "count": 0 })),
                }],
                context.clone(),
            )
            .await
            .expect("create");
        env.fail(Failure {
            operation: FailureOp::Flush,
            call: 3,
            mode,
        });
        assert_eq!(
            storage
                .commit(
                    vec![StorageWrite::DocumentChange {
                        id,
                        content: DocumentContent::Base {
                            version: 1,
                            value: obj(json!({ "count": 2 })),
                        },
                    }],
                    context.clone(),
                )
                .await
                .expect("commit base"),
            seq_from_number(3)
        );
        env.clear();
        storage
            .commit(
                vec![StorageWrite::DocumentChange {
                    id,
                    content: DocumentContent::Delta {
                        version: 1,
                        ops: vec![Op::Set(OpSet(
                            SetVerb::S,
                            vec![Seg::String("count".to_owned())],
                            json!(3),
                        ))],
                    },
                }],
                context.clone(),
            )
            .await
            .expect("commit delta");
        storage.close(context.clone()).await.expect("close");

        let reopened = open_with_env(&dir_path, Arc::new(InstrumentedEnv::new(&dir_path)), true).await;
        assert_eq!(
            reopened
                .document(id, DocumentPoint::Current, context.clone())
                .await
                .expect("read")
                .expect("present")
                .value,
            obj(json!({ "count": 3 }))
        );
        assert_eq!(read_lines(&dir.path().join(format!("doc-{id}.jsonl"))).len(), 2);
        reopened.close(context.clone()).await.expect("close");
    }
}

#[tokio::test]
async fn recovers_terminal_task_reclamation_across_remove_failures() {
    let context = background_context();
    for mode in [FailureMode::Before, FailureMode::After] {
        let (dir, dir_path) = temp_directory();
        let env = Arc::new(InstrumentedEnv::new(&dir_path));
        let storage = open_with_env(&dir_path, Arc::clone(&env), false).await;
        create_root(&storage).await;
        let id: TaskId = id_from_number(storage.mint_id().await.expect("mint task"));
        storage
            .commit(
                vec![StorageWrite::Task {
                    value: pending_task(id, "ready"),
                }],
                context.clone(),
            )
            .await
            .expect("pending");
        env.fail(Failure {
            operation: FailureOp::Remove,
            call: 1,
            mode,
        });
        assert_eq!(
            storage
                .commit(
                    vec![StorageWrite::Task {
                        value: terminal_task(id),
                    }],
                    context.clone(),
                )
                .await
                .expect("terminal"),
            seq_from_number(3)
        );
        assert_eq!(
            storage.task(id, context.clone()).await.expect("task"),
            Some(terminal_task(id))
        );
        storage.close(context.clone()).await.expect("close");

        let reopened = open_storage(&dir_path).await;
        assert_eq!(
            reopened.task(id, context.clone()).await.expect("reopened task"),
            Some(terminal_task(id))
        );
        assert!(!dir.path().join(format!("task-{id}.jsonl")).exists());
        let reclaim_files: Vec<_> = std::fs::read_dir(dir.path())
            .expect("readdir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".reclaim"))
            .collect();
        assert!(reclaim_files.is_empty());
        reopened.close(context.clone()).await.expect("close");
    }
}

#[tokio::test]
async fn appends_later_deltas_to_replacement_sidecar_after_current_only_base() {
    let context = background_context();
    let (dir, dir_path) = temp_directory();
    let storage = open_storage(&dir_path).await;
    create_root(&storage).await;
    let id: DocumentId = id_from_number(storage.mint_id().await.expect("mint doc"));
    storage
        .commit(
            vec![StorageWrite::DocumentCreate {
                record: session_document(id, "test.document"),
                version: 1,
                value: obj(json!({ "count": 0 })),
            }],
            context.clone(),
        )
        .await
        .expect("create");
    storage
        .commit(
            vec![StorageWrite::DocumentChange {
                id,
                content: DocumentContent::Base {
                    version: 1,
                    value: obj(json!({ "count": 10 })),
                },
            }],
            context.clone(),
        )
        .await
        .expect("base");
    storage
        .commit(
            vec![StorageWrite::DocumentChange {
                id,
                content: DocumentContent::Delta {
                    version: 1,
                    ops: vec![Op::Set(OpSet(
                        SetVerb::S,
                        vec![Seg::String("count".to_owned())],
                        json!(11),
                    ))],
                },
            }],
            context.clone(),
        )
        .await
        .expect("delta");
    assert_eq!(read_lines(&dir.path().join(format!("doc-{id}.jsonl"))).len(), 2);
    storage.close(context.clone()).await.expect("close");

    let reopened = open_storage(&dir_path).await;
    assert_eq!(
        reopened
            .document(id, DocumentPoint::Current, context.clone())
            .await
            .expect("read")
            .expect("present")
            .value,
        obj(json!({ "count": 11 }))
    );
    reopened.close(context).await.expect("close");
}

#[tokio::test]
async fn never_reclaims_rewindable_document_history_including_after_base_and_retirement() {
    let context = background_context();
    let (dir, dir_path) = temp_directory();
    let storage = open_storage(&dir_path).await;
    create_root(&storage).await;
    let id: DocumentId = id_from_number(storage.mint_id().await.expect("mint doc"));
    let created_at = storage
        .commit(
            vec![StorageWrite::DocumentCreate {
                record: DocumentCreate {
                    id,
                    kind: "rewindable".to_string(),
                    key: None,
                    scope: DocumentScope::Conversation {
                        conversation_id: ROOT_CONVERSATION_ID,
                    },
                    history: Some(DocumentHistory::Rewindable),
                    fork: Some(DocumentFork::AsOf),
                },
                version: 1,
                value: obj(json!({ "count": 0 })),
            }],
            context.clone(),
        )
        .await
        .expect("create");
    let changed_at = storage
        .commit(
            vec![StorageWrite::DocumentChange {
                id,
                content: DocumentContent::Delta {
                    version: 1,
                    ops: vec![Op::Set(OpSet(
                        SetVerb::S,
                        vec![Seg::String("count".to_owned())],
                        json!(1),
                    ))],
                },
            }],
            context.clone(),
        )
        .await
        .expect("delta");
    storage
        .commit(
            vec![StorageWrite::DocumentChange {
                id,
                content: DocumentContent::Base {
                    version: 1,
                    value: obj(json!({ "count": 2 })),
                },
            }],
            context.clone(),
        )
        .await
        .expect("base");
    storage
        .commit(vec![StorageWrite::DocumentRetire { id }], context.clone())
        .await
        .expect("retire");

    let sidecar_path = dir.path().join(format!("doc-{id}.jsonl"));
    assert_eq!(read_lines(&sidecar_path).len(), 3);
    storage.close(context.clone()).await.expect("close");

    let reopened = open_storage(&dir_path).await;
    assert_eq!(
        reopened
            .document(id, DocumentPoint::At(created_at), context.clone())
            .await
            .expect("created")
            .expect("present")
            .value,
        obj(json!({ "count": 0 }))
    );
    assert_eq!(
        reopened
            .document(id, DocumentPoint::At(changed_at), context.clone())
            .await
            .expect("changed")
            .expect("present")
            .value,
        obj(json!({ "count": 1 }))
    );
    assert!(
        reopened
            .document(id, DocumentPoint::Current, context.clone())
            .await
            .expect("current")
            .is_none()
    );
    assert_eq!(read_lines(&sidecar_path).len(), 3);
    reopened.close(context).await.expect("close");
}

#[tokio::test]
async fn reclaims_retired_task_session_and_latest_conversation_document_sidecars() {
    let context = background_context();
    let (dir, dir_path) = temp_directory();
    let storage = open_storage(&dir_path).await;
    create_root(&storage).await;

    let task_id: TaskId = id_from_number(storage.mint_id().await.expect("mint task"));
    let session_id: DocumentId = id_from_number(storage.mint_id().await.expect("mint session doc"));
    let latest_id: DocumentId = id_from_number(storage.mint_id().await.expect("mint latest doc"));
    let task_doc_id: DocumentId = id_from_number(storage.mint_id().await.expect("mint task doc"));

    let created_at = storage
        .commit(
            vec![
                StorageWrite::Task {
                    value: pending_task(task_id, "ready"),
                },
                StorageWrite::DocumentCreate {
                    record: session_document(session_id, "session"),
                    version: 1,
                    value: JsonObject::new(),
                },
                StorageWrite::DocumentCreate {
                    record: DocumentCreate {
                        id: latest_id,
                        kind: "latest".to_string(),
                        key: None,
                        scope: DocumentScope::Conversation {
                            conversation_id: ROOT_CONVERSATION_ID,
                        },
                        history: Some(DocumentHistory::Latest),
                        fork: Some(DocumentFork::Current),
                    },
                    version: 1,
                    value: JsonObject::new(),
                },
                StorageWrite::DocumentCreate {
                    record: DocumentCreate {
                        id: task_doc_id,
                        kind: "task".to_string(),
                        key: None,
                        scope: DocumentScope::Task { task_id },
                        history: None,
                        fork: None,
                    },
                    version: 1,
                    value: JsonObject::new(),
                },
            ],
            context.clone(),
        )
        .await
        .expect("create");
    let retired_at = storage
        .commit(
            vec![
                StorageWrite::DocumentRetire { id: session_id },
                StorageWrite::DocumentRetire { id: latest_id },
                StorageWrite::DocumentRetire { id: task_doc_id },
                StorageWrite::Task {
                    value: terminal_task(task_id),
                },
            ],
            context.clone(),
        )
        .await
        .expect("retire");

    for file in [
        format!("doc-{session_id}.jsonl"),
        format!("doc-{latest_id}.jsonl"),
        format!("doc-{task_doc_id}.jsonl"),
        format!("task-{task_id}.jsonl"),
    ] {
        assert!(!dir.path().join(file).exists());
    }
    storage.close(context.clone()).await.expect("close");

    let reopened = open_storage(&dir_path).await;
    assert_eq!(
        reopened.task(task_id, context.clone()).await.expect("task"),
        Some(terminal_task(task_id))
    );
    assert!(
        reopened
            .document(session_id, DocumentPoint::Current, context.clone())
            .await
            .expect("session doc")
            .is_none()
    );
    assert!(
        reopened
            .document(latest_id, DocumentPoint::Current, context.clone())
            .await
            .expect("latest doc")
            .is_none()
    );
    assert!(
        reopened
            .document(task_doc_id, DocumentPoint::Current, context.clone())
            .await
            .expect("task doc")
            .is_none()
    );
    let found = reopened
        .find_document(
            DocumentAddress {
                kind: "session".to_string(),
                scope: DocumentScope::Session,
                key: None,
            },
            DocumentPoint::At(created_at),
            context.clone(),
        )
        .await
        .expect("find created")
        .expect("present");
    assert_eq!(found.id, session_id);
    assert_eq!(found.created_at, created_at);
    assert_eq!(found.retired_at, Some(retired_at));
    assert!(
        reopened
            .find_document(
                DocumentAddress {
                    kind: "session".to_string(),
                    scope: DocumentScope::Session,
                    key: None,
                },
                DocumentPoint::At(retired_at),
                context.clone(),
            )
            .await
            .expect("find retired")
            .is_none()
    );
    reopened.close(context).await.expect("close");
}

#[tokio::test]
async fn truncates_torn_utf8_tails_at_exact_byte_offsets_and_reuses_unconfirmed_seq() {
    let context = background_context();
    let (dir, dir_path) = temp_directory();
    let storage = open_storage(&dir_path).await;
    create_root(&storage).await;
    let doc_id: DocumentId = id_from_number(storage.mint_id().await.expect("mint doc"));
    storage
        .commit(
            vec![StorageWrite::DocumentCreate {
                record: session_document(doc_id, "test.document"),
                version: 1,
                value: obj(json!({ "text": "kept" })),
            }],
            context.clone(),
        )
        .await
        .expect("create");
    storage.close(context.clone()).await.expect("close");

    let sidecar_path = dir.path().join(format!("doc-{doc_id}.jsonl"));
    let main_path = dir.path().join("main.jsonl");
    let sidecar_size = std::fs::metadata(&sidecar_path).expect("stat").len();
    let main_size = std::fs::metadata(&main_path).expect("stat").len();
    let torn = r#"{"text":"€"#.as_bytes();
    let torn_partial = &torn[..torn.len() - 1];

    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(&sidecar_path)
        .expect("open sidecar")
        .write_all(torn_partial)
        .expect("append torn");
    std::fs::OpenOptions::new()
        .append(true)
        .open(&main_path)
        .expect("open main")
        .write_all(torn_partial)
        .expect("append torn");

    let reopened = open_storage(&dir_path).await;
    assert_eq!(std::fs::metadata(&sidecar_path).expect("stat").len(), sidecar_size);
    assert_eq!(std::fs::metadata(&main_path).expect("stat").len(), main_size);
    assert_eq!(
        reopened
            .commit(
                vec![StorageWrite::DocumentChange {
                    id: doc_id,
                    content: DocumentContent::Delta {
                        version: 1,
                        ops: vec![],
                    },
                }],
                context.clone(),
            )
            .await
            .expect("reuse seq"),
        seq_from_number(3)
    );
    reopened.close(context).await.expect("close");
}

#[tokio::test]
async fn removes_complete_unconfirmed_sidecar_tails_without_resurrecting_terminal_tasks() {
    let context = background_context();
    let (dir, dir_path) = temp_directory();
    let storage = open_storage(&dir_path).await;
    create_root(&storage).await;
    let task_id: TaskId = id_from_number(storage.mint_id().await.expect("mint task"));
    storage
        .commit(
            vec![StorageWrite::Task {
                value: pending_task(task_id, "ready"),
            }],
            context.clone(),
        )
        .await
        .expect("pending");
    storage
        .commit(
            vec![StorageWrite::Task {
                value: terminal_task(task_id),
            }],
            context.clone(),
        )
        .await
        .expect("terminal");
    storage.close(context.clone()).await.expect("close");

    let sidecar_path = dir.path().join(format!("task-{task_id}.jsonl"));
    assert!(!sidecar_path.exists());
    let stale_line = pi_js::json::try_stringify(&json!({
        "format": 1,
        "type": "record",
        "seq": 4,
        "ordinal": 0,
        "payload": { "type": "task", "value": pending_task(task_id, "stale") },
    }))
    .expect("json");
    std::fs::write(&sidecar_path, format!("{stale_line}\n")).expect("write stale sidecar");

    let reopened = open_storage(&dir_path).await;
    assert!(!sidecar_path.exists());
    assert_eq!(
        reopened.task(task_id, context.clone()).await.expect("task"),
        Some(terminal_task(task_id))
    );
    assert_eq!(
        reopened.commit(vec![], context.clone()).await.expect("empty commit"),
        seq_from_number(4)
    );
    reopened.close(context).await.expect("close");
}

#[tokio::test]
async fn fails_open_when_confirmed_sidecar_data_is_missing() {
    let context = background_context();
    let (dir, dir_path) = temp_directory();
    let storage = open_storage(&dir_path).await;
    create_root(&storage).await;
    let doc_id: DocumentId = id_from_number(storage.mint_id().await.expect("mint doc"));
    storage
        .commit(
            vec![StorageWrite::DocumentCreate {
                record: session_document(doc_id, "test.document"),
                version: 1,
                value: JsonObject::new(),
            }],
            context.clone(),
        )
        .await
        .expect("create");
    storage.close(context.clone()).await.expect("close");

    std::fs::write(dir.path().join(format!("doc-{doc_id}.jsonl")), "").expect("truncate sidecar");
    let fs: Arc<dyn FileSystem> = Arc::new(NodeExecutionEnv::with_options(NodeExecutionEnvOptions {
        cwd: dir_path.clone(),
        ..Default::default()
    }));
    let err = match JsonlStorage::open(&dir_path, fs, context, JsonlStorageOptions::default()).await {
        Ok(_) => panic!("expected missing sidecar record error"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("Missing confirmed sidecar record"), "{err}");
}

#[tokio::test]
async fn rejects_confirmed_record_after_unconfirmed_sidecar_record() {
    let context = background_context();
    let (dir, dir_path) = temp_directory();
    let storage = open_storage(&dir_path).await;
    create_root(&storage).await;
    let doc_id: DocumentId = id_from_number(storage.mint_id().await.expect("mint doc"));
    storage
        .commit(
            vec![StorageWrite::DocumentCreate {
                record: session_document(doc_id, "test.document"),
                version: 1,
                value: obj(json!({ "count": 0 })),
            }],
            context.clone(),
        )
        .await
        .expect("create");
    storage
        .commit(
            vec![StorageWrite::DocumentChange {
                id: doc_id,
                content: DocumentContent::Delta {
                    version: 1,
                    ops: vec![Op::Set(OpSet(
                        SetVerb::S,
                        vec![Seg::String("count".to_owned())],
                        json!(1),
                    ))],
                },
            }],
            context.clone(),
        )
        .await
        .expect("delta");
    storage.close(context.clone()).await.expect("close");

    let path = dir.path().join(format!("doc-{doc_id}.jsonl"));
    let lines = read_lines(&path);
    let unconfirmed = pi_js::json::try_stringify(&json!({
        "format": 1,
        "type": "record",
        "seq": 2,
        "ordinal": 999,
        "payload": {
            "type": "document",
            "id": doc_id.as_i64(),
            "content": { "kind": "delta", "version": 1, "ops": [] }
        }
    }))
    .expect("json");
    std::fs::write(&path, format!("{}\n{unconfirmed}\n{}\n", lines[0], lines[1])).expect("rewrite sidecar");

    let fs: Arc<dyn FileSystem> = Arc::new(NodeExecutionEnv::with_options(NodeExecutionEnvOptions {
        cwd: dir_path.clone(),
        ..Default::default()
    }));
    let err = match JsonlStorage::open(&dir_path, fs, context, JsonlStorageOptions::default()).await {
        Ok(_) => panic!("expected confirmed after unconfirmed error"),
        Err(err) => err,
    };
    assert!(
        err.to_string().contains("Confirmed record follows an unconfirmed tail"),
        "{err}"
    );
}

#[tokio::test]
async fn rejects_non_increasing_main_commit_sequences() {
    let context = background_context();
    let (dir, dir_path) = temp_directory();
    let storage = open_storage(&dir_path).await;
    create_root(&storage).await;
    storage.close(context.clone()).await.expect("close");

    let path = dir.path().join("main.jsonl");
    let content = std::fs::read(&path).expect("read main");
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("open main")
        .write_all(&content)
        .expect("duplicate main line");

    let fs: Arc<dyn FileSystem> = Arc::new(NodeExecutionEnv::with_options(NodeExecutionEnvOptions {
        cwd: dir_path.clone(),
        ..Default::default()
    }));
    let err = match JsonlStorage::open(&dir_path, fs, context, JsonlStorageOptions::default()).await {
        Ok(_) => panic!("expected non-increasing seq error"),
        Err(err) => err,
    };
    assert!(
        err.to_string().contains("Commit sequence") && err.to_string().contains("increase"),
        "{err}"
    );
}

#[tokio::test]
async fn rejects_structurally_invalid_confirmed_document_content() {
    let context = background_context();
    let (dir, dir_path) = temp_directory();
    let storage = open_storage(&dir_path).await;
    create_root(&storage).await;
    let doc_id: DocumentId = id_from_number(storage.mint_id().await.expect("mint doc"));
    storage
        .commit(
            vec![StorageWrite::DocumentCreate {
                record: session_document(doc_id, "test.document"),
                version: 1,
                value: JsonObject::new(),
            }],
            context.clone(),
        )
        .await
        .expect("create");
    storage.close(context.clone()).await.expect("close");

    let path = dir.path().join(format!("doc-{doc_id}.jsonl"));
    let mut rec: JsonValue = serde_json::from_str(std::fs::read_to_string(&path).expect("read").trim()).expect("json");
    rec["payload"]["content"]
        .as_object_mut()
        .expect("content obj")
        .remove("value");
    std::fs::write(
        &path,
        format!("{}\n", pi_js::json::try_stringify(&rec).expect("encode")),
    )
    .expect("write");

    let fs: Arc<dyn FileSystem> = Arc::new(NodeExecutionEnv::with_options(NodeExecutionEnvOptions {
        cwd: dir_path.clone(),
        ..Default::default()
    }));
    let err = match JsonlStorage::open(&dir_path, fs, context, JsonlStorageOptions::default()).await {
        Ok(_) => panic!("expected invalid document content"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("Invalid document content"), "{err}");
}

#[tokio::test]
async fn rejects_malformed_complete_main_and_sidecar_lines() {
    let context = background_context();
    let (main_dir, main_dir_path) = temp_directory();
    let main_storage = open_storage(&main_dir_path).await;
    create_root(&main_storage).await;
    main_storage.close(context.clone()).await.expect("close");

    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(main_dir.path().join("main.jsonl"))
        .expect("open main")
        .write_all(b"{bad}\n")
        .expect("write bad line");

    let main_fs: Arc<dyn FileSystem> = Arc::new(NodeExecutionEnv::with_options(NodeExecutionEnvOptions {
        cwd: main_dir_path.clone(),
        ..Default::default()
    }));
    let main_err =
        match JsonlStorage::open(&main_dir_path, main_fs, context.clone(), JsonlStorageOptions::default()).await {
            Ok(_) => panic!("expected malformed main"),
            Err(err) => err,
        };
    assert!(
        main_err.to_string().contains("Malformed complete main.jsonl"),
        "{main_err}"
    );

    let (sidecar_dir, sidecar_dir_path) = temp_directory();
    let sidecar_storage = open_storage(&sidecar_dir_path).await;
    create_root(&sidecar_storage).await;
    sidecar_storage.close(context.clone()).await.expect("close");
    std::fs::write(sidecar_dir.path().join("doc-99.jsonl"), "{bad}\n").expect("write bad sidecar");

    let sidecar_fs: Arc<dyn FileSystem> = Arc::new(NodeExecutionEnv::with_options(NodeExecutionEnvOptions {
        cwd: sidecar_dir_path.clone(),
        ..Default::default()
    }));
    let sidecar_err =
        match JsonlStorage::open(&sidecar_dir_path, sidecar_fs, context, JsonlStorageOptions::default()).await {
            Ok(_) => panic!("expected malformed sidecar"),
            Err(err) => err,
        };
    assert!(
        sidecar_err.to_string().contains("Malformed complete doc-99.jsonl"),
        "{sidecar_err}"
    );
}
