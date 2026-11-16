//! Port of packages/durable/src/documents.ts

use std::marker::PhantomData;
use std::sync::Arc;

use pi_chord::delta::Op;
use pi_chord::json::{copy_json, is_json_value};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::types::{
    CheckpointInfo, ConversationId, DocumentAddress, DocumentCreate, DocumentFork, DocumentHistory, DocumentId,
    DocumentRecord, DocumentScope, DocumentSemantics, JsonObject, JsonValue, LatestFork, RewindableFork,
    StoredDocument, TaskId,
};

type InitialFn = Arc<dyn Fn(Option<&JsonValue>) -> pi_js::Result<JsonObject> + Send + Sync>;
type MigrateFn = Arc<dyn Fn(&JsonObject, i64) -> pi_js::Result<JsonObject> + Send + Sync>;
type CheckpointWhenFn = Arc<dyn Fn(&JsonObject, &[Op], CheckpointInfo) -> pi_js::Result<bool> + Send + Sync>;

/// Erased document definition used by [`crate::session::session::Session`], views, and storage helpers.
#[derive(Clone)]
pub struct ErasedDocDefinition {
    pub kind: String,
    pub version: i64,
    pub semantics: DocumentSemantics,
    pub family: bool,
    initial_fn: InitialFn,
    migrate_fn: Option<MigrateFn>,
    checkpoint_when_fn: Option<CheckpointWhenFn>,
}

impl ErasedDocDefinition {
    pub fn initial(&self, seed: Option<&JsonValue>) -> pi_js::Result<JsonObject> {
        (self.initial_fn)(seed)
    }

    pub fn can_migrate(&self) -> bool {
        self.migrate_fn.is_some()
    }

    pub fn migrate(&self, value: &JsonObject, from_version: i64) -> pi_js::Result<JsonObject> {
        let Some(migrate) = &self.migrate_fn else {
            return Err(pi_js::Error::msg(format!(
                "Document {} cannot migrate from version {from_version}",
                self.kind
            )));
        };
        migrate(value, from_version)
    }

    pub fn checkpoint_when(&self, value: &JsonObject, ops: &[Op], info: CheckpointInfo) -> pi_js::Result<bool> {
        let Some(pred) = &self.checkpoint_when_fn else {
            return Ok(false);
        };
        pred(value, ops, info)
    }

    pub fn from_raw(
        kind: impl Into<String>,
        version: i64,
        semantics: DocumentSemantics,
        family: bool,
        initial_fn: InitialFn,
        migrate_fn: Option<MigrateFn>,
        checkpoint_when_fn: Option<CheckpointWhenFn>,
    ) -> pi_js::Result<Arc<Self>> {
        let kind = kind.into();
        validate_version(&kind, version)?;
        Ok(Arc::new(Self {
            kind,
            version,
            semantics,
            family,
            initial_fn,
            migrate_fn,
            checkpoint_when_fn,
        }))
    }
}

/// Erased document token for type-agnostic lists (such as mounted conversation view documents).
#[derive(Clone)]
pub struct AnyDocToken {
    pub definition: Arc<ErasedDocDefinition>,
}

/// Input specification to define a singleton document.
pub struct DocDefinitionInput<T> {
    pub kind: &'static str,
    pub version: i64,
    pub initial: fn() -> T,
    pub migrate: Option<fn(&JsonObject, i64) -> pi_js::Result<T>>,
    pub checkpoint_when: Option<fn(&T, &[Op], CheckpointInfo) -> bool>,
}

/// Input specification to define a keyed document family.
pub struct DocFamilyDefinitionInput<T, I> {
    pub kind: &'static str,
    pub version: i64,
    pub initial: fn(I) -> T,
    pub migrate: Option<fn(&JsonObject, i64) -> pi_js::Result<T>>,
    pub checkpoint_when: Option<fn(&T, &[Op], CheckpointInfo) -> bool>,
}

fn validate_version(kind: &str, version: i64) -> pi_js::Result<()> {
    if version < 1 {
        return Err(pi_js::Error::js(
            "TypeError",
            format!("Document {kind} version must be a positive integer"),
        ));
    }
    Ok(())
}

pub(crate) fn value_to_json_object<T: Serialize>(value: &T) -> pi_js::Result<JsonObject> {
    let val = serde_json::to_value(value).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
    let copied = copy_json(&val, None)?;
    if !is_json_value(&copied) {
        return Err(pi_js::Error::js("TypeError", "Document value must be strict JSON"));
    }
    match copied {
        JsonValue::Object(map) => Ok(map),
        _ => Err(pi_js::Error::js("TypeError", "Document root must be a JSON object")),
    }
}

pub(crate) fn json_object_to_value<T: DeserializeOwned>(obj: &JsonObject) -> pi_js::Result<T> {
    serde_json::from_value::<T>(JsonValue::Object(obj.clone()))
        .map_err(|err| pi_js::Error::js("TypeError", err.to_string()))
}

fn build_singleton_erased<T>(
    semantics: DocumentSemantics,
    input: DocDefinitionInput<T>,
) -> pi_js::Result<Arc<ErasedDocDefinition>>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    validate_version(input.kind, input.version)?;
    let initial = input.initial;
    let migrate = input.migrate;
    let checkpoint_when = input.checkpoint_when;
    Ok(Arc::new(ErasedDocDefinition {
        kind: input.kind.to_owned(),
        version: input.version,
        semantics,
        family: false,
        initial_fn: Arc::new(move |_seed| value_to_json_object(&initial())),
        migrate_fn: migrate.map(|f| {
            Arc::new(move |val: &JsonObject, from: i64| {
                let migrated = f(val, from)?;
                value_to_json_object(&migrated)
            }) as MigrateFn
        }),
        checkpoint_when_fn: checkpoint_when.map(|pred| {
            Arc::new(move |val: &JsonObject, ops: &[Op], info: CheckpointInfo| {
                let typed: T = json_object_to_value(val)?;
                Ok(pred(&typed, ops, info))
            }) as CheckpointWhenFn
        }),
    }))
}

fn build_family_erased<T, I>(
    semantics: DocumentSemantics,
    input: DocFamilyDefinitionInput<T, I>,
) -> pi_js::Result<Arc<ErasedDocDefinition>>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
    I: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    validate_version(input.kind, input.version)?;
    let initial = input.initial;
    let migrate = input.migrate;
    let checkpoint_when = input.checkpoint_when;
    Ok(Arc::new(ErasedDocDefinition {
        kind: input.kind.to_owned(),
        version: input.version,
        semantics,
        family: true,
        initial_fn: Arc::new(move |seed| {
            let seed_val = seed
                .cloned()
                .ok_or_else(|| pi_js::Error::js("TypeError", "Document family initial() requires a seed"))?;
            let typed_seed: I =
                serde_json::from_value(seed_val).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
            value_to_json_object(&initial(typed_seed))
        }),
        migrate_fn: migrate.map(|f| {
            Arc::new(move |val: &JsonObject, from: i64| {
                let migrated = f(val, from)?;
                value_to_json_object(&migrated)
            }) as MigrateFn
        }),
        checkpoint_when_fn: checkpoint_when.map(|pred| {
            Arc::new(move |val: &JsonObject, ops: &[Op], info: CheckpointInfo| {
                let typed: T = json_object_to_value(val)?;
                Ok(pred(&typed, ops, info))
            }) as CheckpointWhenFn
        }),
    }))
}

/// Typed Session-scoped singleton document token.
pub struct SessionDocToken<T> {
    pub definition: Arc<ErasedDocDefinition>,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Clone for SessionDocToken<T> {
    fn clone(&self) -> Self {
        Self {
            definition: Arc::clone(&self.definition),
            _marker: PhantomData,
        }
    }
}

/// Typed latest-only conversation singleton document token.
pub struct ConversationDocToken<T> {
    pub definition: Arc<ErasedDocDefinition>,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Clone for ConversationDocToken<T> {
    fn clone(&self) -> Self {
        Self {
            definition: Arc::clone(&self.definition),
            _marker: PhantomData,
        }
    }
}

/// Typed rewindable conversation singleton document token.
pub struct RewindableConversationDocToken<T> {
    pub definition: Arc<ErasedDocDefinition>,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Clone for RewindableConversationDocToken<T> {
    fn clone(&self) -> Self {
        Self {
            definition: Arc::clone(&self.definition),
            _marker: PhantomData,
        }
    }
}

impl<T> RewindableConversationDocToken<T> {
    /// Widen a rewindable token to a general conversation document token.
    pub fn as_conversation(&self) -> ConversationDocToken<T> {
        ConversationDocToken {
            definition: Arc::clone(&self.definition),
            _marker: PhantomData,
        }
    }
}

/// Typed task-scoped singleton document token.
pub struct TaskDocToken<T> {
    pub definition: Arc<ErasedDocDefinition>,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Clone for TaskDocToken<T> {
    fn clone(&self) -> Self {
        Self {
            definition: Arc::clone(&self.definition),
            _marker: PhantomData,
        }
    }
}

/// Typed Session-scoped document family token.
pub struct SessionDocFamilyToken<T, I> {
    pub definition: Arc<ErasedDocDefinition>,
    _marker: PhantomData<fn(I) -> T>,
}

impl<T, I> Clone for SessionDocFamilyToken<T, I> {
    fn clone(&self) -> Self {
        Self {
            definition: Arc::clone(&self.definition),
            _marker: PhantomData,
        }
    }
}

/// Typed latest-only conversation document family token.
pub struct ConversationDocFamilyToken<T, I> {
    pub definition: Arc<ErasedDocDefinition>,
    _marker: PhantomData<fn(I) -> T>,
}

impl<T, I> Clone for ConversationDocFamilyToken<T, I> {
    fn clone(&self) -> Self {
        Self {
            definition: Arc::clone(&self.definition),
            _marker: PhantomData,
        }
    }
}

/// Typed rewindable conversation document family token.
pub struct RewindableConversationDocFamilyToken<T, I> {
    pub definition: Arc<ErasedDocDefinition>,
    _marker: PhantomData<fn(I) -> T>,
}

impl<T, I> Clone for RewindableConversationDocFamilyToken<T, I> {
    fn clone(&self) -> Self {
        Self {
            definition: Arc::clone(&self.definition),
            _marker: PhantomData,
        }
    }
}

impl<T, I> RewindableConversationDocFamilyToken<T, I> {
    pub fn as_conversation(&self) -> ConversationDocFamilyToken<T, I> {
        ConversationDocFamilyToken {
            definition: Arc::clone(&self.definition),
            _marker: PhantomData,
        }
    }
}

/// Typed task-scoped document family token.
pub struct TaskDocFamilyToken<T, I> {
    pub definition: Arc<ErasedDocDefinition>,
    _marker: PhantomData<fn(I) -> T>,
}

impl<T, I> Clone for TaskDocFamilyToken<T, I> {
    fn clone(&self) -> Self {
        Self {
            definition: Arc::clone(&self.definition),
            _marker: PhantomData,
        }
    }
}

/// Trait implemented by both latest and rewindable singleton conversation document tokens.
pub trait ConversationDoc<T>: Send + Sync {
    fn definition(&self) -> &Arc<ErasedDocDefinition>;
}

impl<T> ConversationDoc<T> for ConversationDocToken<T> {
    fn definition(&self) -> &Arc<ErasedDocDefinition> {
        &self.definition
    }
}

impl<T> ConversationDoc<T> for RewindableConversationDocToken<T> {
    fn definition(&self) -> &Arc<ErasedDocDefinition> {
        &self.definition
    }
}

/// Trait implemented by both latest and rewindable conversation document family tokens.
pub trait ConversationDocFamily<T, I>: Send + Sync {
    fn definition(&self) -> &Arc<ErasedDocDefinition>;
}

impl<T, I> ConversationDocFamily<T, I> for ConversationDocFamilyToken<T, I> {
    fn definition(&self) -> &Arc<ErasedDocDefinition> {
        &self.definition
    }
}

impl<T, I> ConversationDocFamily<T, I> for RewindableConversationDocFamilyToken<T, I> {
    fn definition(&self) -> &Arc<ErasedDocDefinition> {
        &self.definition
    }
}

pub fn define_session_doc<T>(input: DocDefinitionInput<T>) -> pi_js::Result<SessionDocToken<T>>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    Ok(SessionDocToken {
        definition: build_singleton_erased(DocumentSemantics::Session, input)?,
        _marker: PhantomData,
    })
}

pub fn define_conversation_doc<T>(
    fork: LatestFork,
    input: DocDefinitionInput<T>,
) -> pi_js::Result<ConversationDocToken<T>>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    Ok(ConversationDocToken {
        definition: build_singleton_erased(DocumentSemantics::ConversationLatest { fork }, input)?,
        _marker: PhantomData,
    })
}

pub fn define_rewindable_conversation_doc<T>(
    fork: RewindableFork,
    input: DocDefinitionInput<T>,
) -> pi_js::Result<RewindableConversationDocToken<T>>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    Ok(RewindableConversationDocToken {
        definition: build_singleton_erased(DocumentSemantics::ConversationRewindable { fork }, input)?,
        _marker: PhantomData,
    })
}

pub fn define_task_doc<T>(input: DocDefinitionInput<T>) -> pi_js::Result<TaskDocToken<T>>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    Ok(TaskDocToken {
        definition: build_singleton_erased(DocumentSemantics::Task, input)?,
        _marker: PhantomData,
    })
}

pub fn define_session_doc_family<T, I>(
    input: DocFamilyDefinitionInput<T, I>,
) -> pi_js::Result<SessionDocFamilyToken<T, I>>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
    I: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    Ok(SessionDocFamilyToken {
        definition: build_family_erased(DocumentSemantics::Session, input)?,
        _marker: PhantomData,
    })
}

pub fn define_conversation_doc_family<T, I>(
    fork: LatestFork,
    input: DocFamilyDefinitionInput<T, I>,
) -> pi_js::Result<ConversationDocFamilyToken<T, I>>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
    I: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    Ok(ConversationDocFamilyToken {
        definition: build_family_erased(DocumentSemantics::ConversationLatest { fork }, input)?,
        _marker: PhantomData,
    })
}

pub fn define_rewindable_conversation_doc_family<T, I>(
    fork: RewindableFork,
    input: DocFamilyDefinitionInput<T, I>,
) -> pi_js::Result<RewindableConversationDocFamilyToken<T, I>>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
    I: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    Ok(RewindableConversationDocFamilyToken {
        definition: build_family_erased(DocumentSemantics::ConversationRewindable { fork }, input)?,
        _marker: PhantomData,
    })
}

pub fn define_task_doc_family<T, I>(input: DocFamilyDefinitionInput<T, I>) -> pi_js::Result<TaskDocFamilyToken<T, I>>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
    I: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    Ok(TaskDocFamilyToken {
        definition: build_family_erased(DocumentSemantics::Task, input)?,
        _marker: PhantomData,
    })
}

/// Closure-capable document callbacks (supports captured state and fallible callbacks).
pub struct DocDefinitionCallbacks<T> {
    pub kind: &'static str,
    pub version: i64,
    pub initial: Arc<dyn Fn() -> pi_js::Result<T> + Send + Sync>,
    pub migrate: Option<Arc<dyn Fn(&JsonObject, i64) -> pi_js::Result<T> + Send + Sync>>,
    pub checkpoint_when: Option<Arc<dyn Fn(&T, &[Op], CheckpointInfo) -> pi_js::Result<bool> + Send + Sync>>,
}

/// Closure-capable document family callbacks (supports captured state and fallible callbacks).
pub struct DocFamilyDefinitionCallbacks<T, I> {
    pub kind: &'static str,
    pub version: i64,
    pub initial: Arc<dyn Fn(I) -> pi_js::Result<T> + Send + Sync>,
    pub migrate: Option<Arc<dyn Fn(&JsonObject, i64) -> pi_js::Result<T> + Send + Sync>>,
    pub checkpoint_when: Option<Arc<dyn Fn(&T, &[Op], CheckpointInfo) -> pi_js::Result<bool> + Send + Sync>>,
}

fn build_singleton_callbacks<T>(
    semantics: DocumentSemantics,
    input: DocDefinitionCallbacks<T>,
) -> pi_js::Result<Arc<ErasedDocDefinition>>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    let initial = input.initial;
    let migrate = input.migrate;
    let checkpoint_when = input.checkpoint_when;
    ErasedDocDefinition::from_raw(
        input.kind,
        input.version,
        semantics,
        false,
        Arc::new(move |_seed| value_to_json_object(&initial()?)),
        migrate.map(|f| Arc::new(move |val: &JsonObject, from: i64| value_to_json_object(&f(val, from)?)) as MigrateFn),
        checkpoint_when.map(|pred| {
            Arc::new(move |val: &JsonObject, ops: &[Op], info: CheckpointInfo| {
                let typed: T = json_object_to_value(val)?;
                pred(&typed, ops, info)
            }) as CheckpointWhenFn
        }),
    )
}

fn build_family_callbacks<T, I>(
    semantics: DocumentSemantics,
    input: DocFamilyDefinitionCallbacks<T, I>,
) -> pi_js::Result<Arc<ErasedDocDefinition>>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
    I: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    let initial = input.initial;
    let migrate = input.migrate;
    let checkpoint_when = input.checkpoint_when;
    ErasedDocDefinition::from_raw(
        input.kind,
        input.version,
        semantics,
        true,
        Arc::new(move |seed| {
            let seed_val = seed
                .cloned()
                .ok_or_else(|| pi_js::Error::js("TypeError", "Document family initial() requires a seed"))?;
            let typed_seed: I =
                serde_json::from_value(seed_val).map_err(|err| pi_js::Error::js("TypeError", err.to_string()))?;
            value_to_json_object(&initial(typed_seed)?)
        }),
        migrate.map(|f| Arc::new(move |val: &JsonObject, from: i64| value_to_json_object(&f(val, from)?)) as MigrateFn),
        checkpoint_when.map(|pred| {
            Arc::new(move |val: &JsonObject, ops: &[Op], info: CheckpointInfo| {
                let typed: T = json_object_to_value(val)?;
                pred(&typed, ops, info)
            }) as CheckpointWhenFn
        }),
    )
}

pub fn define_session_doc_with<T>(input: DocDefinitionCallbacks<T>) -> pi_js::Result<SessionDocToken<T>>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    Ok(SessionDocToken {
        definition: build_singleton_callbacks(DocumentSemantics::Session, input)?,
        _marker: PhantomData,
    })
}

pub fn define_conversation_doc_with<T>(
    fork: LatestFork,
    input: DocDefinitionCallbacks<T>,
) -> pi_js::Result<ConversationDocToken<T>>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    Ok(ConversationDocToken {
        definition: build_singleton_callbacks(DocumentSemantics::ConversationLatest { fork }, input)?,
        _marker: PhantomData,
    })
}

pub fn define_rewindable_conversation_doc_with<T>(
    fork: RewindableFork,
    input: DocDefinitionCallbacks<T>,
) -> pi_js::Result<RewindableConversationDocToken<T>>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    Ok(RewindableConversationDocToken {
        definition: build_singleton_callbacks(DocumentSemantics::ConversationRewindable { fork }, input)?,
        _marker: PhantomData,
    })
}

pub fn define_session_doc_family_with<T, I>(
    input: DocFamilyDefinitionCallbacks<T, I>,
) -> pi_js::Result<SessionDocFamilyToken<T, I>>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
    I: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    Ok(SessionDocFamilyToken {
        definition: build_family_callbacks(DocumentSemantics::Session, input)?,
        _marker: PhantomData,
    })
}

/// Logical address plus its stable string identity for maps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedAddress {
    pub address: DocumentAddress,
    pub id: String,
}

/// Stable string identity of one logical address.
pub fn address_id(address: &DocumentAddress) -> String {
    let owner: Option<i64> = match address.scope {
        DocumentScope::Session => None,
        DocumentScope::Conversation { conversation_id } => Some(conversation_id.as_i64()),
        DocumentScope::Task { task_id } => Some(task_id.as_i64()),
    };
    let scope_kind = match address.scope {
        DocumentScope::Session => "session",
        DocumentScope::Conversation { .. } => "conversation",
        DocumentScope::Task { .. } => "task",
    };
    pi_js::json::stringify(&(address.kind.as_str(), scope_kind, owner, address.key.as_deref()))
}

/// Resolve a concrete logical address + map key ID for a definition and owner/key.
pub fn resolve_address(
    definition: &ErasedDocDefinition,
    conversation_id: Option<ConversationId>,
    task_id: Option<TaskId>,
    key: Option<String>,
) -> pi_js::Result<ResolvedAddress> {
    let scope = match definition.semantics {
        DocumentSemantics::Session => DocumentScope::Session,
        DocumentSemantics::ConversationLatest { .. } | DocumentSemantics::ConversationRewindable { .. } => {
            let cid = conversation_id.ok_or_else(|| {
                pi_js::Error::js(
                    "TypeError",
                    format!("Document {} requires a conversation ID", definition.kind),
                )
            })?;
            DocumentScope::Conversation { conversation_id: cid }
        }
        DocumentSemantics::Task => {
            let tid = task_id.ok_or_else(|| {
                pi_js::Error::js("TypeError", format!("Document {} requires a task ID", definition.kind))
            })?;
            DocumentScope::Task { task_id: tid }
        }
    };
    if definition.family && key.is_none() {
        return Err(pi_js::Error::js(
            "TypeError",
            format!("Document family {} requires a key", definition.kind),
        ));
    }
    let address = DocumentAddress {
        kind: definition.kind.clone(),
        scope,
        key: if definition.family { key } else { None },
    };
    let id = address_id(&address);
    Ok(ResolvedAddress { address, id })
}

/// Build the storage create record for a new incarnation at an address.
pub fn document_create(definition: &ErasedDocDefinition, address: &DocumentAddress, id: DocumentId) -> DocumentCreate {
    DocumentCreate {
        id,
        kind: address.kind.clone(),
        key: address.key.clone(),
        scope: address.scope,
        history: definition.semantics.history(),
        fork: definition.semantics.fork(),
    }
}

/// Reject typed access whose token disagrees with the persisted scope, history, or fork semantics.
pub fn check_record_scope(definition: &ErasedDocDefinition, record: &DocumentRecord) -> pi_js::Result<()> {
    let expected_scope = definition.semantics.scope_kind();
    let mismatch = record.scope.kind() != expected_scope
        || (expected_scope == crate::types::DocumentScopeKind::Conversation
            && (record.history != definition.semantics.history() || record.fork != definition.semantics.fork()));
    if mismatch {
        return Err(pi_js::Error::js(
            "TypeError",
            format!(
                "Document {} ({}) does not match the supplied definition semantics",
                record.id, record.kind
            ),
        ));
    }
    Ok(())
}

/// Reject typed access whose token disagrees with a [`DocumentCreate`] scope, history, or fork semantics.
pub fn check_create_scope(definition: &ErasedDocDefinition, create: &DocumentCreate) -> pi_js::Result<()> {
    let expected_scope = definition.semantics.scope_kind();
    let mismatch = create.scope.kind() != expected_scope
        || (expected_scope == crate::types::DocumentScopeKind::Conversation
            && (create.history != definition.semantics.history() || create.fork != definition.semantics.fork()));
    if mismatch {
        return Err(pi_js::Error::js(
            "TypeError",
            format!(
                "Document {} ({}) does not match the supplied definition semantics",
                create.id, create.kind
            ),
        ));
    }
    Ok(())
}

/// Reject typed access to a stored version the supplied definition cannot use.
pub fn check_record_version(
    definition: &ErasedDocDefinition,
    id: DocumentId,
    kind: &str,
    version: i64,
) -> pi_js::Result<()> {
    if version > definition.version {
        return Err(pi_js::Error::msg(format!(
            "Document {id} ({kind}) has newer version {version} than {}",
            definition.version
        )));
    }
    if version < definition.version && !definition.can_migrate() {
        return Err(pi_js::Error::msg(format!(
            "Document {id} ({kind}) requires migration from version {version}"
        )));
    }
    Ok(())
}

/// Validate and materialize a detached stored value for typed access.
pub fn materialize_document(definition: &ErasedDocDefinition, stored: &StoredDocument) -> pi_js::Result<JsonObject> {
    check_record_scope(definition, &stored.record)?;
    check_record_version(definition, stored.record.id, &stored.record.kind, stored.version)?;
    if stored.version == definition.version {
        return Ok(stored.value.clone());
    }
    definition.migrate(&stored.value, stored.version)
}

/// Validate and materialize one detached value before its first persisted incarnation (against a [`DocumentCreate`]).
pub fn materialize_create_value(
    definition: &ErasedDocDefinition,
    create: &DocumentCreate,
    version: i64,
    value: &JsonObject,
) -> pi_js::Result<JsonObject> {
    check_create_scope(definition, create)?;
    check_record_version(definition, create.id, &create.kind, version)?;
    if version == definition.version {
        return Ok(value.clone());
    }
    definition.migrate(value, version)
}
