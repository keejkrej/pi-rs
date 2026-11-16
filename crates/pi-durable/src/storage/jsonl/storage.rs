//! Port of packages/durable/src/storage/jsonl/storage.ts

use std::error::Error as StdError;
use std::fmt;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::{IndexMap, IndexSet};
use pi_chord::Context;
use pi_chord::delta::Op;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::env::{CreateDirOptions, FileContent, FileError, FileErrorCode, FileKind, FileSystem, RemoveOptions};
use crate::ids::Seq;
use crate::storage::memory::MemoryStorage;
use crate::types::{
    ConversationId, ConversationQuery, ConversationRecord, Cursor, DocumentAddress, DocumentContent, DocumentCreate,
    DocumentHistory, DocumentId, DocumentPoint, DocumentQuery, DocumentRecord, DocumentScopeKind, EntryId, EntryLookup,
    EntryQuery, EntryRecord, ErasedTaskRecord, JsonObject, Page, Storage, StorageWrite, StoredDocument, SubmissionId,
    SubmissionQuery, SubmissionRecord, TaskId, TaskQuery, TaskStatus,
};

const FORMAT_VERSION: i64 = 1;
const MAIN_FILE: &str = "main.jsonl";
const RECLAIM_SUFFIX: &str = ".reclaim";

/// Options configuring a [`JsonlStorage`] instance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JsonlStorageOptions {
    /// Flush every affected sidecar before appending the main marker. Defaults to `false`.
    pub fsync: bool,
}

/// Corruption detected in persisted JSONL records or sidecar files.
#[derive(Clone, Debug)]
pub struct JsonlCorruptionError {
    pub message: String,
    pub cause: Option<Arc<pi_js::Error>>,
}

impl JsonlCorruptionError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            cause: None,
        }
    }

    pub fn with_cause(message: impl Into<String>, cause: pi_js::Error) -> Self {
        Self {
            message: message.into(),
            cause: Some(Arc::new(cause)),
        }
    }

    pub fn into_error(self) -> pi_js::Error {
        pi_js::Error::typed("JsonlCorruptionError", self)
    }
}

impl fmt::Display for JsonlCorruptionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl StdError for JsonlCorruptionError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.cause.as_deref().map(|err| err as &(dyn StdError + 'static))
    }
}

impl From<JsonlCorruptionError> for pi_js::Error {
    fn from(err: JsonlCorruptionError) -> Self {
        err.into_error()
    }
}

/// Fatal I/O failure poisoned a [`JsonlStorage`] instance until reopened.
#[derive(Clone, Debug)]
pub struct JsonlStoragePoisonedError {
    pub message: String,
    pub cause: Option<Arc<pi_js::Error>>,
}

impl JsonlStoragePoisonedError {
    pub fn new(cause: pi_js::Error) -> Self {
        Self {
            message: "JSONL storage is poisoned and must be reopened".to_string(),
            cause: Some(Arc::new(cause)),
        }
    }

    pub fn into_error(self) -> pi_js::Error {
        pi_js::Error::typed("JsonlStoragePoisonedError", self)
    }
}

impl fmt::Display for JsonlStoragePoisonedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl StdError for JsonlStoragePoisonedError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.cause.as_deref().map(|err| err as &(dyn StdError + 'static))
    }
}

impl From<JsonlStoragePoisonedError> for pi_js::Error {
    fn from(err: JsonlStoragePoisonedError) -> Self {
        err.into_error()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum WireDocumentContent {
    Base { version: i64, value: JsonObject },
    Delta { version: i64, ops: Vec<Op> },
}

impl From<&DocumentContent> for WireDocumentContent {
    fn from(content: &DocumentContent) -> Self {
        match content {
            DocumentContent::Base { version, value } => Self::Base {
                version: *version,
                value: value.clone(),
            },
            DocumentContent::Delta { version, ops } => Self::Delta {
                version: *version,
                ops: ops.clone(),
            },
        }
    }
}

impl From<WireDocumentContent> for DocumentContent {
    fn from(wire: WireDocumentContent) -> Self {
        match wire {
            WireDocumentContent::Base { version, value } => Self::Base { version, value },
            WireDocumentContent::Delta { version, ops } => Self::Delta { version, ops },
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum MainOperation {
    #[serde(rename = "conversation")]
    Conversation { value: ConversationRecord },
    #[serde(rename = "entry")]
    Entry { value: EntryRecord },
    #[serde(rename = "task")]
    Task { value: ErasedTaskRecord },
    #[serde(rename = "task.sidecar")]
    TaskSidecar { id: TaskId, ordinal: usize },
    #[serde(rename = "submission")]
    Submission { value: SubmissionRecord },
    #[serde(rename = "document.create")]
    DocumentCreate { record: DocumentCreate, ordinal: usize },
    #[serde(rename = "document.change")]
    DocumentChange { id: DocumentId, ordinal: usize },
    #[serde(rename = "document.retire")]
    DocumentRetire { id: DocumentId },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct MainMarker {
    format: i64,
    #[serde(rename = "type")]
    record_type: String,
    seq: Seq,
    writes: Vec<MainOperation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum SidecarPayload {
    Task {
        value: ErasedTaskRecord,
    },
    Document {
        id: DocumentId,
        content: WireDocumentContent,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SidecarRecord {
    format: i64,
    #[serde(rename = "type")]
    record_type: String,
    seq: Seq,
    ordinal: usize,
    payload: SidecarPayload,
}

#[derive(Clone, Debug)]
struct ParsedLine<T> {
    value: T,
    start: u64,
}

#[derive(Clone, Debug)]
struct ParsedFile<T> {
    path: String,
    lines: Vec<ParsedLine<T>>,
}

#[derive(Clone, Debug)]
struct EncodedCommit {
    marker: String,
    sidecars: IndexMap<String, String>,
}

#[derive(Debug, Default)]
struct JsonlRuntimeState {
    current_only_documents: IndexSet<DocumentId>,
    live_task_sidecars: IndexSet<TaskId>,
    closed: bool,
    poison_error: Option<JsonlStoragePoisonedError>,
}

/// Portable JSONL implementation of the [`Storage`] contract.
pub struct JsonlStorage {
    fs: Arc<dyn FileSystem>,
    directory: String,
    main_path: String,
    fsync: bool,
    memory: MemoryStorage,
    state: Mutex<JsonlRuntimeState>,
    commit_lock: tokio::sync::Mutex<()>,
}

impl JsonlStorage {
    /// Open or create a JSONL storage directory using the supplied filesystem.
    pub async fn open(
        directory: &str,
        fs: Arc<dyn FileSystem>,
        context: Context,
        options: JsonlStorageOptions,
    ) -> pi_js::Result<Self> {
        let absolute = fs
            .absolute_path(directory, &context)
            .await
            .map_err(|err| error_from_file("path resolution", err))?;
        fs.create_dir(&absolute, Some(CreateDirOptions { recursive: Some(true) }), &context)
            .await
            .map_err(|err| error_from_file("directory creation", err))?;
        let main_path = fs
            .join_path(&[&absolute, MAIN_FILE], &context)
            .await
            .map_err(|err| error_from_file("path join", err))?;

        let storage = Self {
            fs,
            directory: absolute,
            main_path,
            fsync: options.fsync,
            memory: MemoryStorage::new(),
            state: Mutex::new(JsonlRuntimeState::default()),
            commit_lock: tokio::sync::Mutex::new(()),
        };
        storage.recover(&context).await?;
        Ok(storage)
    }

    fn assert_usable(&self) -> pi_js::Result<()> {
        let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if guard.closed {
            return Err(pi_js::Error::msg("JsonlStorage is closed"));
        }
        if let Some(ref poison) = guard.poison_error {
            return Err(poison.clone().into_error());
        }
        Ok(())
    }

    fn poison(&self, cause: pi_js::Error) -> pi_js::Error {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref existing) = guard.poison_error {
            return existing.clone().into_error();
        }
        let poisoned = JsonlStoragePoisonedError::new(cause);
        guard.poison_error = Some(poisoned.clone());
        poisoned.into_error()
    }

    fn encode_commit(&self, seq: Seq, writes: &[StorageWrite]) -> pi_js::Result<EncodedCommit> {
        let mut records: IndexMap<String, Vec<SidecarRecord>> = IndexMap::new();
        let mut main_writes: Vec<MainOperation> = Vec::with_capacity(writes.len());

        let mut add_sidecar = |file: String, payload: SidecarPayload| -> usize {
            let list = records.entry(file).or_default();
            let ordinal = list.len();
            list.push(SidecarRecord {
                format: FORMAT_VERSION,
                record_type: "record".to_string(),
                seq,
                ordinal,
                payload,
            });
            ordinal
        };

        for write in writes {
            match write {
                StorageWrite::Conversation { value } => {
                    main_writes.push(MainOperation::Conversation { value: value.clone() });
                }
                StorageWrite::Entry { value } => {
                    main_writes.push(MainOperation::Entry { value: value.clone() });
                }
                StorageWrite::Submission { value } => {
                    main_writes.push(MainOperation::Submission { value: value.clone() });
                }
                StorageWrite::DocumentRetire { id } => {
                    main_writes.push(MainOperation::DocumentRetire { id: *id });
                }
                StorageWrite::Task { value } => {
                    if value.state.status() == TaskStatus::Terminal {
                        main_writes.push(MainOperation::Task { value: value.clone() });
                    } else {
                        let ordinal = add_sidecar(
                            sidecar_file_name("task", value.id.as_i64()),
                            SidecarPayload::Task { value: value.clone() },
                        );
                        main_writes.push(MainOperation::TaskSidecar { id: value.id, ordinal });
                    }
                }
                StorageWrite::DocumentCreate { record, version, value } => {
                    let ordinal = add_sidecar(
                        sidecar_file_name("doc", record.id.as_i64()),
                        SidecarPayload::Document {
                            id: record.id,
                            content: WireDocumentContent::Base {
                                version: *version,
                                value: value.clone(),
                            },
                        },
                    );
                    main_writes.push(MainOperation::DocumentCreate {
                        record: record.clone(),
                        ordinal,
                    });
                }
                StorageWrite::DocumentChange { id, content } => {
                    let ordinal = add_sidecar(
                        sidecar_file_name("doc", id.as_i64()),
                        SidecarPayload::Document {
                            id: *id,
                            content: WireDocumentContent::from(content),
                        },
                    );
                    main_writes.push(MainOperation::DocumentChange { id: *id, ordinal });
                }
                StorageWrite::DocumentCopy { record, .. } => {
                    return Err(pi_js::Error::msg(format!(
                        "Document {} copy was not resolved",
                        record.id
                    )));
                }
            }
        }

        let mut sidecars = IndexMap::with_capacity(records.len());
        for (file, file_records) in records {
            let mut text = String::new();
            for rec in &file_records {
                text.push_str(&json_line(rec)?);
            }
            sidecars.insert(file, text);
        }

        let marker = MainMarker {
            format: FORMAT_VERSION,
            record_type: "commit".to_string(),
            seq,
            writes: main_writes,
        };
        Ok(EncodedCommit {
            marker: json_line(&marker)?,
            sidecars,
        })
    }

    fn plan_reclamations(&self, writes: &[StorageWrite], encoded: &EncodedCommit) -> IndexMap<String, String> {
        let mut created_current_only_docs: IndexSet<DocumentId> = IndexSet::new();
        let mut retired_docs: IndexSet<DocumentId> = IndexSet::new();
        let mut base_docs: IndexSet<DocumentId> = IndexSet::new();
        let mut final_tasks: IndexMap<TaskId, TaskStatus> = IndexMap::new();

        for write in writes {
            match write {
                StorageWrite::DocumentCreate { record, .. } => {
                    if is_current_only_create(record) {
                        created_current_only_docs.insert(record.id);
                    }
                }
                StorageWrite::DocumentChange { id, content } => {
                    if matches!(content, DocumentContent::Base { .. }) {
                        base_docs.insert(*id);
                    }
                }
                StorageWrite::DocumentRetire { id } => {
                    retired_docs.insert(*id);
                }
                StorageWrite::Task { value } => {
                    final_tasks.insert(value.id, value.state.status());
                }
                _ => {}
            }
        }

        let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let is_curr_only =
            |id: &DocumentId| guard.current_only_documents.contains(id) || created_current_only_docs.contains(id);

        let mut replacements = IndexMap::new();
        for id in &retired_docs {
            if is_curr_only(id) {
                replacements.insert(sidecar_file_name("doc", id.as_i64()), String::new());
            }
        }
        for id in &base_docs {
            if !is_curr_only(id) || retired_docs.contains(id) {
                continue;
            }
            let file = sidecar_file_name("doc", id.as_i64());
            if let Some(content) = encoded.sidecars.get(&file) {
                replacements.insert(file, content.clone());
            }
        }
        for (id, status) in final_tasks {
            let file = sidecar_file_name("task", id.as_i64());
            if status == TaskStatus::Terminal
                && (guard.live_task_sidecars.contains(&id) || encoded.sidecars.contains_key(&file))
            {
                replacements.insert(file, String::new());
            }
        }
        replacements
    }

    fn adopt_sidecar_state(&self, writes: &[StorageWrite]) {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        for write in writes {
            match write {
                StorageWrite::DocumentCreate { record, .. } => {
                    if is_current_only_create(record) {
                        guard.current_only_documents.insert(record.id);
                    }
                }
                StorageWrite::Task { value } => {
                    if value.state.status() == TaskStatus::Terminal {
                        guard.live_task_sidecars.shift_remove(&value.id);
                    } else {
                        guard.live_task_sidecars.insert(value.id);
                    }
                }
                _ => {}
            }
        }
    }

    async fn reclaim_sidecars(&self, replacements: &IndexMap<String, String>, context: &Context) {
        if replacements.is_empty() {
            return;
        }
        if self.fsync && self.fs.flush_file(&self.main_path, context).await.is_err() {
            return;
        }
        for (file, content) in replacements {
            self.replace_sidecar(file, content, context).await;
        }
    }

    async fn replace_sidecar(&self, file: &str, content: &str, context: &Context) {
        let Ok(path) = self.fs.join_path(&[&self.directory, file], context).await else {
            return;
        };
        if content.is_empty() {
            let _ = self
                .fs
                .remove(
                    &path,
                    Some(RemoveOptions {
                        recursive: None,
                        force: Some(true),
                    }),
                    context,
                )
                .await;
            return;
        }
        let temp_name = format!("{file}{RECLAIM_SUFFIX}");
        let Ok(temp_path) = self.fs.join_path(&[&self.directory, &temp_name], context).await else {
            return;
        };
        if self
            .fs
            .write_file(&temp_path, FileContent::Text(content), context)
            .await
            .is_err()
        {
            return;
        }
        if self.fsync && self.fs.flush_file(&temp_path, context).await.is_err() {
            return;
        }
        let _ = self.fs.rename_file(&temp_path, &path, context).await;
    }

    async fn resolve_file(&self, file: &str, context: &Context) -> pi_js::Result<String> {
        self.fs
            .join_path(&[&self.directory, file], context)
            .await
            .map_err(|err| error_from_file("path join", err))
    }

    async fn recover(&self, context: &Context) -> pi_js::Result<()> {
        let main = read_lines(self.fs.as_ref(), &self.main_path, MAIN_FILE, context, parse_main_marker).await?;

        let mut previous_seq = 0i64;
        for marker in &main.lines {
            if marker.value.seq.as_i64() <= previous_seq {
                return Err(JsonlCorruptionError::new(format!(
                    "Commit sequence does not strictly increase in {MAIN_FILE}"
                ))
                .into_error());
            }
            previous_seq = marker.value.seq.as_i64();
        }

        let listed = self
            .fs
            .list_dir(&self.directory, context)
            .await
            .map_err(|err| error_from_file("directory listing", err))?;

        for info in &listed {
            if info.kind == FileKind::File && is_reclaim_file_name(&info.name) {
                let _ = self
                    .fs
                    .remove(
                        &info.path,
                        Some(RemoveOptions {
                            recursive: None,
                            force: Some(true),
                        }),
                        context,
                    )
                    .await;
            }
        }

        let mut sidecar_files: Vec<String> = listed
            .into_iter()
            .filter(|info| info.kind == FileKind::File && is_sidecar_file_name(&info.name))
            .map(|info| info.name)
            .collect();
        sidecar_files.sort();

        let mut parsed_files: IndexMap<String, ParsedFile<SidecarRecord>> = IndexMap::new();
        let mut record_by_key: IndexMap<String, SidecarRecord> = IndexMap::new();
        for file in &sidecar_files {
            let path = self.resolve_file(file, context).await?;
            let parsed = read_lines(self.fs.as_ref(), &path, file, context, |text, line| {
                parse_sidecar_record(text, file, line)
            })
            .await?;

            let mut previous: Option<(i64, usize)> = None;
            for line in &parsed.lines {
                let s = line.value.seq.as_i64();
                let o = line.value.ordinal;
                if let Some((ps, po)) = previous
                    && (s < ps || (s == ps && o <= po))
                {
                    return Err(
                        JsonlCorruptionError::new(format!("Sidecar records are out of order in {file}")).into_error(),
                    );
                }
                previous = Some((s, o));
                record_by_key.insert(sidecar_key(file, line.value.seq, o)?, line.value.clone());
            }
            parsed_files.insert(file.clone(), parsed);
        }

        let mut current_only_documents: IndexSet<DocumentId> = IndexSet::new();
        let mut retired_documents: IndexSet<DocumentId> = IndexSet::new();
        let mut final_task_is_live: IndexMap<TaskId, bool> = IndexMap::new();

        for marker_line in &main.lines {
            for operation in &marker_line.value.writes {
                match operation {
                    MainOperation::DocumentCreate { record, .. } => {
                        if is_current_only_create(record) {
                            current_only_documents.insert(record.id);
                        }
                    }
                    MainOperation::DocumentRetire { id } => {
                        retired_documents.insert(*id);
                    }
                    MainOperation::Task { value } => {
                        final_task_is_live.insert(value.id, false);
                    }
                    MainOperation::TaskSidecar { id, .. } => {
                        final_task_is_live.insert(*id, true);
                    }
                    _ => {}
                }
            }
        }

        let retired_current_only_documents: IndexSet<DocumentId> = retired_documents
            .iter()
            .copied()
            .filter(|id| current_only_documents.contains(id))
            .collect();

        let mut latest_bases: IndexMap<DocumentId, (Seq, usize)> = IndexMap::new();
        for marker_line in &main.lines {
            let marker = &marker_line.value;
            for operation in &marker.writes {
                let (id, ordinal) = match operation {
                    MainOperation::DocumentCreate { record, ordinal } => (record.id, *ordinal),
                    MainOperation::DocumentChange { id, ordinal } => (*id, *ordinal),
                    _ => continue,
                };
                if !current_only_documents.contains(&id) {
                    continue;
                }
                let key = sidecar_key(&sidecar_file_name("doc", id.as_i64()), marker.seq, ordinal)?;
                let Some(rec) = record_by_key.get(&key) else {
                    continue;
                };
                let SidecarPayload::Document {
                    id: payload_id,
                    content: WireDocumentContent::Base { .. },
                } = &rec.payload
                else {
                    continue;
                };
                if *payload_id != id {
                    continue;
                }
                let entry = (rec.seq, rec.ordinal);
                let replace = match latest_bases.get(&id) {
                    None => true,
                    Some(&(prev_seq, prev_ord)) => {
                        rec.seq > prev_seq || (rec.seq == prev_seq && rec.ordinal > prev_ord)
                    }
                };
                if replace {
                    latest_bases.insert(id, entry);
                }
            }
        }

        let is_before_latest_base = |id: DocumentId, seq: Seq, ordinal: usize| -> bool {
            latest_bases
                .get(&id)
                .is_some_and(|&(b_seq, b_ord)| seq < b_seq || (seq == b_seq && ordinal < b_ord))
        };

        let terminal_tasks: IndexSet<TaskId> = final_task_is_live
            .iter()
            .filter_map(|(&id, &live)| (!live).then_some(id))
            .collect();
        let mut confirmed: IndexSet<String> = IndexSet::new();

        for marker_line in &main.lines {
            let marker = &marker_line.value;
            let mut writes: Vec<StorageWrite> = Vec::with_capacity(marker.writes.len());
            for operation in &marker.writes {
                match operation {
                    MainOperation::Conversation { value } => {
                        writes.push(StorageWrite::Conversation { value: value.clone() });
                    }
                    MainOperation::Entry { value } => {
                        writes.push(StorageWrite::Entry { value: value.clone() });
                    }
                    MainOperation::Submission { value } => {
                        writes.push(StorageWrite::Submission { value: value.clone() });
                    }
                    MainOperation::Task { value } => {
                        writes.push(StorageWrite::Task { value: value.clone() });
                    }
                    MainOperation::DocumentRetire { id } => {
                        writes.push(StorageWrite::DocumentRetire { id: *id });
                    }
                    MainOperation::TaskSidecar { id, ordinal } => {
                        let optional = terminal_tasks.contains(id);
                        let file = sidecar_file_name("task", id.as_i64());
                        let record = confirm_record(marker, *ordinal, &file, &record_by_key, &mut confirmed, optional)?;
                        if let Some(rec) = record {
                            let SidecarPayload::Task { value } = &rec.payload else {
                                return Err(JsonlCorruptionError::new(format!(
                                    "Confirmed task sidecar data does not match commit {}",
                                    marker.seq
                                ))
                                .into_error());
                            };
                            if value.id != *id {
                                return Err(JsonlCorruptionError::new(format!(
                                    "Confirmed task sidecar data does not match commit {}",
                                    marker.seq
                                ))
                                .into_error());
                            }
                            if !optional {
                                writes.push(StorageWrite::Task { value: value.clone() });
                            }
                        }
                    }
                    MainOperation::DocumentCreate { record, ordinal } => {
                        let id = record.id;
                        let reclaimed = retired_current_only_documents.contains(&id)
                            || is_before_latest_base(id, marker.seq, *ordinal);
                        let file = sidecar_file_name("doc", id.as_i64());
                        let rec_opt =
                            confirm_record(marker, *ordinal, &file, &record_by_key, &mut confirmed, reclaimed)?;
                        let content = match rec_opt {
                            None => None,
                            Some(rec) => match &rec.payload {
                                SidecarPayload::Document {
                                    id: payload_id,
                                    content,
                                } if *payload_id == id => Some(DocumentContent::from(content.clone())),
                                _ => {
                                    return Err(JsonlCorruptionError::new(format!(
                                        "Confirmed document sidecar data does not match commit {}",
                                        marker.seq
                                    ))
                                    .into_error());
                                }
                            },
                        };
                        let (version, value) = match content {
                            _ if reclaimed => (1, JsonObject::new()),
                            None => (1, JsonObject::new()),
                            Some(DocumentContent::Base { version, value }) => (version, value),
                            Some(DocumentContent::Delta { .. }) => {
                                return Err(JsonlCorruptionError::new(format!(
                                    "Document creation lacks a confirmed base in commit {}",
                                    marker.seq
                                ))
                                .into_error());
                            }
                        };
                        writes.push(StorageWrite::DocumentCreate {
                            record: record.clone(),
                            version,
                            value,
                        });
                    }
                    MainOperation::DocumentChange { id, ordinal } => {
                        let reclaimed = retired_current_only_documents.contains(id)
                            || is_before_latest_base(*id, marker.seq, *ordinal);
                        let file = sidecar_file_name("doc", id.as_i64());
                        let rec_opt =
                            confirm_record(marker, *ordinal, &file, &record_by_key, &mut confirmed, reclaimed)?;
                        let content = match rec_opt {
                            None => None,
                            Some(rec) => match &rec.payload {
                                SidecarPayload::Document {
                                    id: payload_id,
                                    content,
                                } if payload_id == id => Some(DocumentContent::from(content.clone())),
                                _ => {
                                    return Err(JsonlCorruptionError::new(format!(
                                        "Confirmed document sidecar data does not match commit {}",
                                        marker.seq
                                    ))
                                    .into_error());
                                }
                            },
                        };
                        if !reclaimed && let Some(content) = content {
                            writes.push(StorageWrite::DocumentChange { id: *id, content });
                        }
                    }
                }
            }
            let mut prepared = self.memory.prepare_commit(&writes, Some(marker.seq)).map_err(|err| {
                JsonlCorruptionError::with_cause(format!("Invalid committed state at sequence {}", marker.seq), err)
                    .into_error()
            })?;
            prepared.apply().map_err(|err| {
                JsonlCorruptionError::with_cause(format!("Invalid committed state at sequence {}", marker.seq), err)
                    .into_error()
            })?;
        }

        let mut reclamations: IndexMap<String, String> = IndexMap::new();
        for (file, parsed) in &parsed_files {
            let mut unconfirmed_at: Option<u64> = None;
            for line in &parsed.lines {
                let key = sidecar_key(file, line.value.seq, line.value.ordinal)?;
                if confirmed.contains(&key) {
                    if unconfirmed_at.is_some() {
                        return Err(JsonlCorruptionError::new(format!(
                            "Confirmed record follows an unconfirmed tail in {file}"
                        ))
                        .into_error());
                    }
                } else if unconfirmed_at.is_none() {
                    unconfirmed_at = Some(line.start);
                }
            }
            if let Some(pos) = unconfirmed_at {
                self.fs
                    .truncate_file(&parsed.path, pos, context)
                    .await
                    .map_err(|err| error_from_file(format!("tail truncation of {file}"), err))?;
            }

            let mut confirmed_lines: Vec<&ParsedLine<SidecarRecord>> = Vec::new();
            for line in &parsed.lines {
                let key = sidecar_key(file, line.value.seq, line.value.ordinal)?;
                if confirmed.contains(&key) {
                    confirmed_lines.push(line);
                }
            }

            let mut retained_lines: Option<Vec<&ParsedLine<SidecarRecord>>> = None;
            if let Some((prefix, numeric_id)) = parse_sidecar_filename(file) {
                if prefix == "task" && terminal_tasks.contains(&TaskId::new_unchecked(numeric_id)) {
                    retained_lines = Some(Vec::new());
                } else if prefix == "doc" {
                    let doc_id = DocumentId::new_unchecked(numeric_id);
                    if retired_current_only_documents.contains(&doc_id) {
                        retained_lines = Some(Vec::new());
                    } else if latest_bases.contains_key(&doc_id) {
                        retained_lines = Some(
                            confirmed_lines
                                .iter()
                                .copied()
                                .filter(|line| !is_before_latest_base(doc_id, line.value.seq, line.value.ordinal))
                                .collect(),
                        );
                    }
                }
            }

            if let Some(retained) = retained_lines
                && (retained.len() < confirmed_lines.len() || retained.is_empty())
            {
                let mut text = String::new();
                for line in retained {
                    text.push_str(&json_line(&line.value)?);
                }
                reclamations.insert(file.clone(), text);
            }
        }
        self.reclaim_sidecars(&reclamations, context).await;

        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        for id in current_only_documents {
            guard.current_only_documents.insert(id);
        }
        for (id, live) in final_task_is_live {
            if live {
                guard.live_task_sidecars.insert(id);
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Storage for JsonlStorage {
    async fn commit(&self, writes: Vec<StorageWrite>, context: Context) -> pi_js::Result<Seq> {
        let _commit_guard = self.commit_lock.lock().await;
        self.assert_usable()?;
        let mut prepared = self.memory.prepare_commit(&writes, None)?;
        let encoded = self.encode_commit(prepared.seq, &prepared.writes)?;
        let reclamations = self.plan_reclamations(&prepared.writes, &encoded);

        let mut sidecars = Vec::with_capacity(encoded.sidecars.len());
        for (file, content) in encoded.sidecars {
            let path = self.resolve_file(&file, &context).await?;
            sidecars.push((file, content, path));
        }

        for (file, content, path) in &sidecars {
            if let Err(err) = self.fs.append_file(path, FileContent::Text(content), &context).await {
                return Err(self.poison(error_from_file(format!("append to {file}"), err)));
            }
        }
        if self.fsync {
            for (file, _, path) in &sidecars {
                if let Err(err) = self.fs.flush_file(path, &context).await {
                    return Err(self.poison(error_from_file(format!("flush of {file}"), err)));
                }
            }
        }
        if let Err(err) = self
            .fs
            .append_file(&self.main_path, FileContent::Text(&encoded.marker), &context)
            .await
        {
            return Err(self.poison(error_from_file(format!("append to {MAIN_FILE}"), err)));
        }
        let seq = prepared.apply()?;
        self.adopt_sidecar_state(&prepared.writes);
        self.reclaim_sidecars(&reclamations, &context).await;
        Ok(seq)
    }

    async fn mint_id(&self) -> pi_js::Result<i64> {
        self.assert_usable()?;
        self.memory.mint_id().await
    }

    async fn conversation(&self, id: ConversationId, context: Context) -> pi_js::Result<Option<ConversationRecord>> {
        self.assert_usable()?;
        self.memory.conversation(id, context).await
    }

    async fn scan_conversations(
        &self,
        query: ConversationQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<ConversationRecord, Cursor>> {
        self.assert_usable()?;
        self.memory.scan_conversations(query, limit, cursor, context).await
    }

    async fn entry(&self, id: EntryId, context: Context) -> pi_js::Result<Option<EntryLookup>> {
        self.assert_usable()?;
        self.memory.entry(id, context).await
    }

    async fn entry_in_conversation(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
        context: Context,
    ) -> pi_js::Result<Option<EntryLookup>> {
        self.assert_usable()?;
        self.memory.entry_in_conversation(conversation_id, id, context).await
    }

    async fn find_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
        context: Context,
    ) -> pi_js::Result<Option<EntryRecord>> {
        self.assert_usable()?;
        self.memory
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
        self.assert_usable()?;
        self.memory.scan_entries(query, limit, cursor, context).await
    }

    async fn task(&self, id: TaskId, context: Context) -> pi_js::Result<Option<ErasedTaskRecord>> {
        self.assert_usable()?;
        self.memory.task(id, context).await
    }

    async fn scan_tasks(
        &self,
        query: TaskQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<ErasedTaskRecord, Cursor>> {
        self.assert_usable()?;
        self.memory.scan_tasks(query, limit, cursor, context).await
    }

    async fn submission(&self, id: SubmissionId, context: Context) -> pi_js::Result<Option<SubmissionRecord>> {
        self.assert_usable()?;
        self.memory.submission(id, context).await
    }

    async fn scan_submissions(
        &self,
        query: SubmissionQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<SubmissionRecord, Cursor>> {
        self.assert_usable()?;
        self.memory.scan_submissions(query, limit, cursor, context).await
    }

    async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
        context: Context,
    ) -> pi_js::Result<Option<SubmissionRecord>> {
        self.assert_usable()?;
        self.memory
            .submission_by_request(conversation_id, request_id, context)
            .await
    }

    async fn find_document(
        &self,
        address: DocumentAddress,
        at: DocumentPoint,
        context: Context,
    ) -> pi_js::Result<Option<DocumentRecord>> {
        self.assert_usable()?;
        self.memory.find_document(address, at, context).await
    }

    async fn document(
        &self,
        id: DocumentId,
        at: DocumentPoint,
        context: Context,
    ) -> pi_js::Result<Option<StoredDocument>> {
        self.assert_usable()?;
        self.memory.document(id, at, context).await
    }

    async fn scan_documents(
        &self,
        query: DocumentQuery,
        limit: usize,
        cursor: Option<Cursor>,
        context: Context,
    ) -> pi_js::Result<Page<DocumentRecord, Cursor>> {
        self.assert_usable()?;
        self.memory.scan_documents(query, limit, cursor, context).await
    }

    async fn close(&self, context: Context) -> pi_js::Result<()> {
        let _commit_guard = self.commit_lock.lock().await;
        {
            let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.closed = true;
        }
        self.memory.close(context).await
    }
}

fn sidecar_file_name(kind: &str, id: i64) -> String {
    format!("{kind}-{id}.jsonl")
}

fn parse_sidecar_filename(name: &str) -> Option<(&'static str, i64)> {
    let rest = name.strip_suffix(".jsonl")?;
    let (prefix, digits) = if let Some(d) = rest.strip_prefix("doc-") {
        ("doc", d)
    } else if let Some(d) = rest.strip_prefix("task-") {
        ("task", d)
    } else {
        return None;
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if digits.len() > 1 && digits.starts_with('0') {
        return None;
    }
    let id = digits.parse::<i64>().ok()?;
    Some((prefix, id))
}

fn is_sidecar_file_name(name: &str) -> bool {
    parse_sidecar_filename(name).is_some()
}

fn is_reclaim_file_name(name: &str) -> bool {
    name.strip_suffix(RECLAIM_SUFFIX)
        .and_then(parse_sidecar_filename)
        .is_some()
}

fn is_current_only_create(record: &DocumentCreate) -> bool {
    record.scope.kind() != DocumentScopeKind::Conversation || record.history == Some(DocumentHistory::Latest)
}

fn sidecar_key(file: &str, seq: Seq, ordinal: usize) -> pi_js::Result<String> {
    pi_js::json::stringify(&(file, seq.as_i64(), ordinal))
}

fn json_line<T: Serialize>(value: &T) -> pi_js::Result<String> {
    let mut s = pi_js::json::stringify(value)?;
    s.push('\n');
    Ok(s)
}

fn error_from_file(action: impl fmt::Display, error: FileError) -> pi_js::Error {
    let message = format!("JSONL {action} failed: {}", error.message);
    pi_js::Error::js_with_cause("Error", message, error.into())
}

fn parse_json_value(text: &str, description: &str) -> pi_js::Result<JsonValue> {
    serde_json::from_str::<JsonValue>(text).map_err(|err| {
        JsonlCorruptionError::with_cause(
            format!("Malformed complete {description}"),
            pi_js::Error::msg(err.to_string()),
        )
        .into_error()
    })
}

fn as_safe_int(val: &JsonValue) -> Option<i64> {
    if let Some(n) = val.as_i64() {
        return Some(n);
    }
    if let Some(n) = val.as_f64()
        && n.is_finite()
        && n.fract() == 0.0
        && n.abs() <= pi_js::num::MAX_SAFE_INTEGER as f64
    {
        return Some(n as i64);
    }
    None
}

fn validate_main_operation(val: JsonValue, description: &str) -> pi_js::Result<MainOperation> {
    let Some(obj) = val.as_object() else {
        return Err(JsonlCorruptionError::new(format!("Invalid write in {description}")).into_error());
    };
    let Some(op_type) = obj.get("type").and_then(JsonValue::as_str) else {
        return Err(JsonlCorruptionError::new(format!("Invalid write in {description}")).into_error());
    };

    match op_type {
        "conversation" | "entry" | "submission" => {
            let valid_id = obj
                .get("value")
                .and_then(JsonValue::as_object)
                .and_then(|v| v.get("id"))
                .and_then(as_safe_int)
                .is_some();
            if !valid_id {
                return Err(
                    JsonlCorruptionError::new(format!("Invalid {op_type} write in {description}")).into_error(),
                );
            }
        }
        "task" => {
            let valid_task = obj.get("value").and_then(JsonValue::as_object).is_some_and(|task_obj| {
                task_obj.get("id").and_then(as_safe_int).is_some()
                    && task_obj
                        .get("state")
                        .and_then(JsonValue::as_object)
                        .and_then(|s| s.get("status"))
                        .and_then(JsonValue::as_str)
                        == Some("terminal")
            });
            if !valid_task {
                return Err(
                    JsonlCorruptionError::new(format!("Invalid terminal task write in {description}")).into_error(),
                );
            }
        }
        "document.retire" => {
            if obj.get("id").and_then(as_safe_int).is_none() {
                return Err(
                    JsonlCorruptionError::new(format!("Invalid document retirement in {description}")).into_error(),
                );
            }
        }
        "task.sidecar" => {
            let valid = obj.get("id").and_then(as_safe_int).is_some()
                && obj.get("ordinal").and_then(as_safe_int).is_some_and(|o| o >= 0);
            if !valid {
                return Err(
                    JsonlCorruptionError::new(format!("Invalid task sidecar write in {description}")).into_error(),
                );
            }
        }
        "document.create" => {
            let valid = obj
                .get("record")
                .and_then(JsonValue::as_object)
                .and_then(|r| r.get("id"))
                .and_then(as_safe_int)
                .is_some()
                && obj.get("ordinal").and_then(as_safe_int).is_some_and(|o| o >= 0);
            if !valid {
                return Err(
                    JsonlCorruptionError::new(format!("Invalid document creation in {description}")).into_error(),
                );
            }
        }
        "document.change" => {
            let valid = obj.get("id").and_then(as_safe_int).is_some()
                && obj.get("ordinal").and_then(as_safe_int).is_some_and(|o| o >= 0);
            if !valid {
                return Err(
                    JsonlCorruptionError::new(format!("Invalid document change in {description}")).into_error(),
                );
            }
        }
        _ => {
            return Err(JsonlCorruptionError::new(format!("Unknown write type in {description}")).into_error());
        }
    }

    serde_json::from_value::<MainOperation>(val).map_err(|err| {
        JsonlCorruptionError::with_cause(
            format!("Invalid {op_type} write in {description}"),
            pi_js::Error::msg(err.to_string()),
        )
        .into_error()
    })
}

fn parse_main_marker(text: &str, line: usize) -> pi_js::Result<MainMarker> {
    let description = format!("{MAIN_FILE} line {line}");
    let val = parse_json_value(text, &description)?;
    let Some(obj) = val.as_object() else {
        return Err(JsonlCorruptionError::new(format!("Invalid commit marker in {description}")).into_error());
    };
    let seq = obj.get("seq").and_then(as_safe_int);
    let writes_arr = obj.get("writes").and_then(JsonValue::as_array);
    if obj.get("format").and_then(as_safe_int) != Some(FORMAT_VERSION)
        || obj.get("type").and_then(JsonValue::as_str) != Some("commit")
        || seq.is_none_or(|s| s < 1)
        || writes_arr.is_none()
    {
        return Err(JsonlCorruptionError::new(format!("Invalid commit marker in {description}")).into_error());
    }

    let Some(seq) = seq else {
        return Err(JsonlCorruptionError::new(format!("Invalid commit marker in {description}")).into_error());
    };
    let Some(raw_writes) = writes_arr else {
        return Err(JsonlCorruptionError::new(format!("Invalid commit marker in {description}")).into_error());
    };

    let mut writes = Vec::with_capacity(raw_writes.len());
    for raw in raw_writes {
        writes.push(validate_main_operation(raw.clone(), &description)?);
    }

    Ok(MainMarker {
        format: FORMAT_VERSION,
        record_type: "commit".to_string(),
        seq: Seq::new_unchecked(seq),
        writes,
    })
}

fn validate_document_content(val: &JsonValue, description: &str) -> pi_js::Result<()> {
    let Some(obj) = val.as_object() else {
        return Err(JsonlCorruptionError::new(format!("Invalid document content in {description}")).into_error());
    };
    if obj.get("version").and_then(as_safe_int).is_none_or(|v| v < 1) {
        return Err(JsonlCorruptionError::new(format!("Invalid document content in {description}")).into_error());
    }
    match obj.get("kind").and_then(JsonValue::as_str) {
        Some("base") if obj.get("value").is_some_and(JsonValue::is_object) => Ok(()),
        Some("delta") if obj.get("ops").is_some_and(JsonValue::is_array) => Ok(()),
        _ => Err(JsonlCorruptionError::new(format!("Invalid document content in {description}")).into_error()),
    }
}

fn parse_sidecar_record(text: &str, file: &str, line: usize) -> pi_js::Result<SidecarRecord> {
    let description = format!("{file} line {line}");
    let val = parse_json_value(text, &description)?;
    let Some(obj) = val.as_object() else {
        return Err(JsonlCorruptionError::new(format!("Invalid sidecar record in {description}")).into_error());
    };
    let seq = obj.get("seq").and_then(as_safe_int);
    let ordinal = obj.get("ordinal").and_then(as_safe_int);
    let payload_obj = obj.get("payload").and_then(JsonValue::as_object);
    let payload_type = payload_obj.and_then(|p| p.get("type")).and_then(JsonValue::as_str);

    if obj.get("format").and_then(as_safe_int) != Some(FORMAT_VERSION)
        || obj.get("type").and_then(JsonValue::as_str) != Some("record")
        || seq.is_none_or(|s| s < 1)
        || ordinal.is_none_or(|o| o < 0)
        || payload_type.is_none()
    {
        return Err(JsonlCorruptionError::new(format!("Invalid sidecar record in {description}")).into_error());
    }

    let Some(payload) = payload_obj else {
        return Err(JsonlCorruptionError::new(format!("Invalid sidecar record in {description}")).into_error());
    };
    match payload_type {
        Some("task") => {
            let valid = payload.get("value").and_then(JsonValue::as_object).is_some_and(|t| {
                t.get("id").and_then(as_safe_int).is_some()
                    && t.get("state")
                        .and_then(JsonValue::as_object)
                        .and_then(|s| s.get("status"))
                        .and_then(JsonValue::as_str)
                        .is_some_and(|st| st != "terminal")
            });
            if !valid {
                return Err(
                    JsonlCorruptionError::new(format!("Invalid live task record in {description}")).into_error(),
                );
            }
        }
        Some("document") => {
            if payload.get("id").and_then(as_safe_int).is_none() {
                return Err(
                    JsonlCorruptionError::new(format!("Invalid document record in {description}")).into_error(),
                );
            }
            let Some(content) = payload.get("content") else {
                return Err(
                    JsonlCorruptionError::new(format!("Invalid document content in {description}")).into_error(),
                );
            };
            validate_document_content(content, &description)?;
        }
        _ => {
            return Err(
                JsonlCorruptionError::new(format!("Unknown sidecar record type in {description}")).into_error(),
            );
        }
    }

    serde_json::from_value::<SidecarRecord>(val).map_err(|err| {
        JsonlCorruptionError::with_cause(
            format!("Invalid sidecar record in {description}"),
            pi_js::Error::msg(err.to_string()),
        )
        .into_error()
    })
}

fn confirm_record<'a>(
    marker: &MainMarker,
    ordinal: usize,
    file: &str,
    record_by_key: &'a IndexMap<String, SidecarRecord>,
    confirmed: &mut IndexSet<String>,
    optional: bool,
) -> pi_js::Result<Option<&'a SidecarRecord>> {
    let key = sidecar_key(file, marker.seq, ordinal)?;
    if confirmed.contains(&key) {
        return Err(JsonlCorruptionError::new("Sidecar record is confirmed more than once").into_error());
    }
    let Some(line) = record_by_key.get(&key) else {
        if optional {
            return Ok(None);
        }
        return Err(JsonlCorruptionError::new(format!(
            "Missing confirmed sidecar record {file} at sequence {}",
            marker.seq
        ))
        .into_error());
    };
    confirmed.insert(key);
    Ok(Some(line))
}

async fn read_lines<T>(
    fs: &dyn FileSystem,
    path: &str,
    name: &str,
    context: &Context,
    parse: impl Fn(&str, usize) -> pi_js::Result<T>,
) -> pi_js::Result<ParsedFile<T>> {
    let bytes = match fs.read_binary_file(path, context).await {
        Ok(b) => b,
        Err(err) if err.code == FileErrorCode::NotFound => {
            return Ok(ParsedFile {
                path: path.to_string(),
                lines: Vec::new(),
            });
        }
        Err(err) => return Err(error_from_file(format!("read of {name}"), err)),
    };

    let mut complete_size = bytes.len();
    if complete_size > 0 && bytes[complete_size - 1] != b'\n' {
        complete_size = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |pos| pos + 1);
        fs.truncate_file(path, complete_size as u64, context)
            .await
            .map_err(|err| error_from_file(format!("torn-line truncation of {name}"), err))?;
    }

    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut line_number = 1usize;
    for end in 0..complete_size {
        if bytes[end] != b'\n' {
            continue;
        }
        let text = std::str::from_utf8(&bytes[start..end]).map_err(|err| {
            JsonlCorruptionError::with_cause(
                format!("Invalid UTF-8 in complete {name} line {line_number}"),
                pi_js::Error::msg(err.to_string()),
            )
            .into_error()
        })?;
        lines.push(ParsedLine {
            value: parse(text, line_number)?,
            start: start as u64,
        });
        start = end + 1;
        line_number += 1;
    }

    Ok(ParsedFile {
        path: path.to_string(),
        lines,
    })
}
